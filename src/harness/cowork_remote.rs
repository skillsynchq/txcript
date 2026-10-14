//! Cloud Cowork sessions, pulled explicitly from the signed-in Claude Desktop
//! account. Native exports retain the session response and event envelopes;
//! Common uses the Claude Code codec for complete top-level SDK messages.
//! Streaming deltas, control events, and subagent messages remain native-only.
//! Explicit attachments and presented files are downloaded on an explicit load.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::common::{ArtifactSource, Block, Meta, Tool};
use crate::harness::claude_code::{self, Record};
use crate::{Codec, Common, Error, Harness, Result, TextCodec, Transcript};

/// A read-only cloud Cowork source, distinct from the writable local `cowork`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoworkRemote;

impl Harness for CoworkRemote {
    const NAME: &'static str = "cowork_remote";
    type Body = RemoteSession;
}

/// The complete detail response and ordered event envelopes. Unknown fields
/// are retained, including tool metadata and attachment references.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteSession {
    pub session: Value,
    pub events: Vec<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl TextCodec for CoworkRemote {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let body: RemoteSession = serde_json::from_str(text)?;
        let meta = metadata(session_detail(&body.session))?;
        Ok(Transcript::new(meta, body))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Ok(serde_json::to_string_pretty(&transcript.body)?)
    }
}

impl Codec for CoworkRemote {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        let files: std::collections::BTreeMap<String, crate::common::Artifact> = transcript
            .body
            .extra
            .get("$txcript_files")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()?
            .unwrap_or_default();
        let mut records = Vec::new();
        let mut seen = std::collections::HashMap::new();
        let mut file_tools = std::collections::HashMap::new();
        for event in &transcript.body.events {
            if let Some(id) = event.get("event_id").and_then(Value::as_str)
                && let Some(previous) = seen.insert(id, event)
            {
                if previous != event {
                    return Err(malformed("conflicting duplicate event id"));
                }
                continue;
            }
            let mut payload = event
                .get("payload")
                .cloned()
                .ok_or_else(|| malformed("event is missing `payload`"))?;
            if !matches!(
                payload.get("type").and_then(Value::as_str),
                Some("user" | "assistant")
            ) || payload
                .get("parent_tool_use_id")
                .is_some_and(|id| !id.is_null())
            {
                continue;
            }
            if let Some(blocks) = payload
                .pointer("/message/content")
                .and_then(Value::as_array)
            {
                for block in blocks {
                    if block.get("type").and_then(Value::as_str) == Some("tool_use")
                        && block.get("name").and_then(Value::as_str) == Some("Artifact")
                        && let Some(id) = block.get("id").and_then(Value::as_str)
                    {
                        file_tools.insert(
                            id.to_string(),
                            block.get("input").cloned().unwrap_or(Value::Null),
                        );
                    }
                }
            }
            let object = payload
                .as_object_mut()
                .ok_or_else(|| malformed("message payload is not an object"))?;
            if !object.contains_key("uuid") {
                let id = event
                    .get("event_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| malformed("message has neither uuid nor event_id"))?;
                object.insert("uuid".into(), Value::String(id.into()));
            }
            if !object.contains_key("timestamp")
                && let Some(timestamp) = event.get("created_at")
            {
                object.insert("timestamp".into(), timestamp.clone());
            }
            super::cowork_files::attach(&mut payload, &files)?;
            let record = Record::from(payload);
            if matches!(record, Record::Other(_)) {
                return Err(malformed("invalid SDK message in Cowork events"));
            }
            records.push(record);
        }
        let mut messages = claude_code::records_to_messages(&records, transcript.meta.timestamp);
        for message in &mut messages {
            for block in &mut message.content {
                if let Block::Artifact { artifact } = block
                    && matches!(artifact.source, ArtifactSource::Path { .. })
                {
                    // A VM path must never be mistaken for a readable host
                    // file. Keep its original tool reference until hydrated.
                    let input = file_tools
                        .get(&artifact.id)
                        .cloned()
                        .ok_or_else(|| malformed("remote artifact is missing its source tool"))?;
                    *block = Block::ToolUse {
                        id: artifact.id.clone(),
                        tool: Tool::Raw {
                            tool_name: "Artifact".into(),
                            input,
                        },
                    };
                }
            }
        }
        let mut meta = transcript.meta.clone();
        if meta.model.is_none() {
            meta.model = messages.iter().find_map(|message| message.model.clone());
        }
        Ok(Transcript::new(meta, messages))
    }

    fn from_common(_: &Transcript<Common>) -> Result<Transcript<Self>> {
        Err(read_only_error())
    }
}

/// Validate a full Cowork id or a Claude Cowork/chat URL. Chat aliases use
/// Claude's version-8 UUIDs; resolving them never enumerates the account.
///
/// # Errors
/// When the input is not a Cowork id, chat alias, or supported claude.ai URL.
pub fn normalize_id(id: &str) -> Result<String> {
    if let Some(path) = id.strip_prefix("https://claude.ai/") {
        let path = path
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .trim_end_matches('/');
        if let Some(id) = path.strip_prefix("cowork/") {
            return normalize_session_id(id);
        }
        if let Some(id) = path.strip_prefix("chat/") {
            return chat_id(id)
                .ok_or_else(|| malformed("expected a Cowork chat URL with a version-8 UUID"));
        }
        return Err(malformed(
            "expected a claude.ai/cowork/ or claude.ai/chat/ URL",
        ));
    }
    if let Some(id) = chat_id(id) {
        return Ok(id);
    }
    normalize_session_id(id)
}

fn chat_id(id: &str) -> Option<String> {
    // Require the normal URL spelling, not UUID parser conveniences such as
    // URNs or braces. Version alone selects a lookup, never proves its source.
    let uuid = uuid::Uuid::parse_str(id).ok()?;
    (id.len() == 36 && uuid.get_version_num() == 8).then(|| uuid.to_string())
}

fn normalize_session_id(id: &str) -> Result<String> {
    let suffix = id
        .strip_prefix("cse_")
        .or_else(|| id.strip_prefix("session_"))
        .filter(|suffix| {
            !suffix.is_empty()
                && suffix.len() <= 64
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
        .ok_or_else(|| malformed("expected a full cse_… or session_… Cowork session id"))?;
    Ok(format!("cse_{suffix}"))
}

#[cfg(feature = "cowork_remote")]
fn session_from_chat(snapshot: &Value, requested_id: &str) -> Result<String> {
    let conversation = snapshot
        .pointer("/event/update/conversation")
        .ok_or_else(|| malformed("chat snapshot is missing its conversation"))?;
    if conversation.get("id").and_then(Value::as_str) != Some(requested_id) {
        return Err(malformed("Claude returned a different chat id"));
    }
    let extras = conversation
        .get("extras")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed("chat has no Cowork source metadata"))?;
    let has_cowork_meta = extras.iter().any(|extra| {
        extra.get("@type").and_then(Value::as_str)
            == Some("type.googleapis.com/anthropic.bard.api.v1alpha.CoworkSessionMeta")
    });
    if !has_cowork_meta {
        return Err(malformed(
            "chat is not a presented Cowork session; use claude_chat for ordinary chats",
        ));
    }
    let mut session_id = None;
    for extra in extras {
        if extra.get("@type").and_then(Value::as_str)
            != Some("type.googleapis.com/anthropic.bard.api.v1alpha.WorkspaceUpgradeState")
        {
            continue;
        }
        let id = extra
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| malformed("Cowork chat mapping is missing its session id"))?;
        let id = normalize_session_id(id)?;
        if session_id.as_ref().is_some_and(|previous| previous != &id) {
            return Err(malformed("Cowork chat has conflicting session mappings"));
        }
        session_id = Some(id);
    }
    session_id.ok_or_else(|| malformed("Cowork chat has no backing session mapping"))
}

fn session_detail(response: &Value) -> &Value {
    response
        .get("session")
        .or_else(|| {
            response
                .get("response_shape")
                .filter(|value| value.get("id").is_some())
        })
        .unwrap_or(response)
}

fn metadata(session: &Value) -> Result<Meta> {
    let id = session
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed("session is missing string `id`"))?;
    Ok(Meta {
        id: normalize_session_id(id)?,
        timestamp: timestamp(session, "created_at").unwrap_or(DateTime::<Utc>::UNIX_EPOCH),
        // This is a remote VM path, not a directory on the receiving machine.
        cwd: None,
        git_branch: None,
        title: session
            .get("title")
            .and_then(Value::as_str)
            .map(String::from),
        cli_version: None,
        model: session
            .pointer("/config/model")
            .and_then(Value::as_str)
            .map(String::from),
        lineage: None,
    })
}

fn timestamp(value: &Value, key: &str) -> Option<DateTime<Utc>> {
    value.get(key)?.as_str()?.parse().ok()
}

fn malformed(detail: &str) -> Error {
    Error::Malformed {
        harness: CoworkRemote::NAME,
        detail: detail.into(),
    }
}

pub(crate) fn read_only_error() -> Error {
    Error::Unconvertible {
        harness: CoworkRemote::NAME,
        detail: "cloud Cowork is pull-only; choose a writable destination such as cowork, claude_code, or codex".into(),
    }
}

#[cfg(feature = "cowork_remote")]
mod remote {
    use base64::Engine;
    use std::collections::HashSet;

    use super::{
        CoworkRemote, RemoteSession, chat_id, malformed, metadata, normalize_id, read_only_error,
        session_detail, session_from_chat, timestamp,
    };
    use crate::harness::claude_chat::ClaudeChatStore;
    use crate::{Discovered, Error, Result, Saved, Store, Transcript};
    use chrono::{DateTime, Utc};
    use serde_json::Value;

    const MAX_PAGES: usize = 1000;
    const MAX_ROWS: usize = 500_000;

    /// The account and Cowork id or chat alias needed for an explicit read.
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub struct CoworkRemoteRef {
        pub organization_uuid: String,
        pub session_id: String,
        pub updated_at: Option<DateTime<Utc>>,
    }

    /// Read-only store using the same host-owned login as Claude Chat.
    pub struct CoworkRemoteStore {
        claude: ClaudeChatStore,
    }

    impl CoworkRemoteStore {
        /// Reuse the signed-in Claude Desktop account without enumerating it.
        ///
        /// # Errors
        /// When Desktop credentials are unavailable or invalid.
        pub fn from_desktop() -> Result<Self> {
            Ok(Self {
                claude: ClaudeChatStore::from_desktop().map_err(remote_error)?,
            })
        }

        /// Resolve an id in the explicit or Desktop-active organization,
        /// without first listing cloud sessions.
        ///
        /// # Errors
        /// When either identifier is invalid or no organization is selected.
        pub fn session_ref(
            &self,
            id: &str,
            organization: Option<String>,
        ) -> Result<CoworkRemoteRef> {
            let session_id = normalize_id(id)?;
            Ok(CoworkRemoteRef {
                organization_uuid: self
                    .claude
                    .resolve_organization(organization)
                    .map_err(remote_error)?,
                session_id,
                updated_at: None,
            })
        }

        fn get(&self, path: &str, organization: &str) -> Result<Value> {
            self.claude
                .get_code_json(path, organization)
                .map_err(remote_error)
        }

        fn pages(&self, path: &str, organization: &str) -> Result<Vec<Value>> {
            let mut rows = Vec::new();
            let mut total_bytes = 0_usize;
            let mut cursors = HashSet::new();
            let mut cursor = None;
            for _ in 0..MAX_PAGES {
                let url = cursor.as_ref().map_or_else(
                    || path.to_string(),
                    |cursor: &String| format!("{path}&cursor={}", encode(cursor)),
                );
                let page = self.get(&url, organization)?;
                total_bytes = total_bytes.saturating_add(serde_json::to_vec(&page)?.len());
                if total_bytes > 256 * 1024 * 1024 {
                    return Err(malformed(
                        "Cowork pagination exceeded the byte limit; refusing a partial transcript",
                    ));
                }
                let data = page
                    .get("data")
                    .and_then(Value::as_array)
                    .ok_or_else(|| malformed("page is missing array `data`"))?;
                if rows.len().saturating_add(data.len()) > MAX_ROWS {
                    return Err(malformed(
                        "Cowork pagination exceeded the row limit; refusing a partial transcript",
                    ));
                }
                rows.extend(data.iter().cloned());
                match page.get("next_cursor") {
                    None | Some(Value::Null) => return Ok(rows),
                    Some(Value::String(next)) if next.is_empty() => return Ok(rows),
                    Some(Value::String(next))
                        if !data.is_empty() && cursors.insert(next.clone()) =>
                    {
                        cursor = Some(next.clone());
                    }
                    _ => {
                        return Err(malformed(
                            "invalid or repeated pagination cursor; refusing a partial transcript",
                        ));
                    }
                }
            }
            Err(malformed(
                "Cowork pagination exceeded the page limit; refusing a partial transcript",
            ))
        }
    }

    impl Store for CoworkRemoteStore {
        type H = CoworkRemote;
        type Ref = CoworkRemoteRef;

        fn discover(&self) -> Result<Vec<Discovered<Self::Ref>>> {
            let mut found = Vec::new();
            let mut seen = HashSet::new();
            for organization in self.claude.organizations().map_err(remote_error)? {
                for row in self.pages(
                    "/v1/code/sessions?tags=cowork-remote&limit=200",
                    &organization,
                )? {
                    // The code-session endpoint also serves Claude Code; keep
                    // the provider's explicit Cowork tag as a second guard.
                    if !row
                        .get("tags")
                        .and_then(Value::as_array)
                        .is_some_and(|tags| tags.iter().any(|tag| tag == "cowork-remote"))
                    {
                        continue;
                    }
                    let meta = metadata(&row)?;
                    if seen.insert((organization.clone(), meta.id.clone())) {
                        found.push(Discovered {
                            reference: CoworkRemoteRef {
                                organization_uuid: organization.clone(),
                                session_id: meta.id.clone(),
                                updated_at: timestamp(&row, "last_event_at")
                                    .or_else(|| timestamp(&row, "updated_at")),
                            },
                            meta,
                        });
                    }
                }
            }
            found.sort_by_key(|item| std::cmp::Reverse(item.meta.timestamp));
            Ok(found)
        }

        fn load(&self, reference: &Self::Ref) -> Result<Transcript<Self::H>> {
            let id = normalize_id(&reference.session_id)?;
            let (id, snapshot) = if chat_id(&id).is_some() {
                let snapshot = self
                    .claude
                    .cowork_chat_snapshot(&id, &reference.organization_uuid)
                    .map_err(remote_error)?;
                (session_from_chat(&snapshot, &id)?, Some(snapshot))
            } else {
                (id, None)
            };
            let path = format!("/v1/code/sessions/{id}");
            let session = self.get(&path, &reference.organization_uuid)?;
            let detail = session_detail(&session);
            let meta = metadata(detail)?;
            if meta.id != id {
                return Err(malformed("Cowork returned a different session id"));
            }
            if !detail
                .get("tags")
                .and_then(Value::as_array)
                .is_some_and(|tags| tags.iter().any(|tag| tag == "cowork-remote"))
            {
                return Err(malformed("session is not tagged cowork-remote"));
            }
            let events = self.pages(
                &format!("{path}/events?limit=500&sort_order=asc"),
                &reference.organization_uuid,
            )?;
            // Reject malformed pages before claiming to have loaded a session.
            if events
                .iter()
                .any(|event| !event.get("payload").is_some_and(Value::is_object))
            {
                return Err(malformed("event is missing object `payload`"));
            }
            let mut extra = serde_json::Map::default();
            let mut files = std::collections::BTreeMap::new();
            let mut total = 0_usize;
            for event in &events {
                let payload = &event["payload"];
                if !matches!(payload["type"].as_str(), Some("user" | "assistant"))
                    || payload
                        .get("parent_tool_use_id")
                        .is_some_and(|value| !value.is_null())
                {
                    continue;
                }
                for file in super::super::cowork_files::references(payload) {
                    if files.contains_key(&file.key) {
                        continue;
                    }
                    let (bytes, media_type) = self
                        .claude
                        .cowork_file(
                            &reference.organization_uuid,
                            &id,
                            file.uuid.as_deref(),
                            file.path.as_deref(),
                        )
                        .map_err(|error| Error::Remote {
                            harness: "cowork_remote",
                            detail: format!("Could not include {}: {error}", file.name),
                        })?;
                    total = total.saturating_add(bytes.len());
                    if total > 128 * 1024 * 1024 {
                        return Err(malformed("Cowork files exceed the 128 MB limit"));
                    }
                    files.insert(
                        file.key.clone(),
                        crate::common::Artifact {
                            id: file.key,
                            name: file.name,
                            source: crate::common::ArtifactSource::Base64 {
                                data: base64::engine::general_purpose::STANDARD.encode(bytes),
                                media_type,
                            },
                        },
                    );
                }
            }
            if !files.is_empty() {
                extra.insert("$txcript_files".into(), serde_json::to_value(files)?);
            }
            if let Some(snapshot) = snapshot {
                extra.insert("$txcript_chat_snapshot".into(), snapshot);
            }
            Ok(Transcript::new(
                meta,
                RemoteSession {
                    session,
                    events,
                    extra,
                },
            ))
        }

        fn save(&self, _: &Transcript<Self::H>) -> Result<Saved<Self::Ref>> {
            Err(read_only_error())
        }

        fn delete(&self, _: &Self::Ref) -> Result<()> {
            Err(read_only_error())
        }
    }

    fn remote_error(error: Error) -> Error {
        match error {
            Error::Remote { detail, .. } => Error::Remote {
                harness: "cowork_remote",
                detail,
            },
            other => other,
        }
    }

    fn encode(value: &str) -> String {
        use std::fmt::Write;
        let mut encoded = String::new();
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                encoded.push(char::from(byte));
            } else {
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
        encoded
    }
    #[cfg(test)]
    #[allow(clippy::unwrap_used, clippy::panic, clippy::needless_pass_by_value)]
    mod tests {
        use super::*;
        use crate::{Codec, Harness, TextCodec};
        use serde_json::json;
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::thread;

        const ORG: &str = "00000000-0000-4000-8000-000000000001";

        fn server(
            responses: Vec<(u16, Value)>,
        ) -> (CoworkRemoteStore, thread::JoinHandle<Vec<String>>) {
            server_bytes(
                responses
                    .into_iter()
                    .map(|(status, value)| {
                        (status, "application/json", value.to_string().into_bytes())
                    })
                    .collect(),
            )
        }

        fn server_bytes(
            responses: Vec<(u16, &'static str, Vec<u8>)>,
        ) -> (CoworkRemoteStore, thread::JoinHandle<Vec<String>>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let handle = thread::spawn(move || {
                let mut requests = Vec::new();
                for (status, content_type, body) in responses {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                        .unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut request = String::new();
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" || line.is_empty() {
                            break;
                        }
                        request.push_str(&line);
                    }
                    let length = request
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or_default();
                    let mut sent = vec![0; length];
                    reader.read_exact(&mut sent).unwrap();
                    request.push_str(&String::from_utf8_lossy(&sent));
                    requests.push(request);
                    write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                    stream.write_all(&body).unwrap();
                }
                requests
            });
            let claude =
                ClaudeChatStore::for_test("test-session-secret", Some(ORG.into()), base).unwrap();
            (CoworkRemoteStore { claude }, handle)
        }

        const CHAT: &str = "00000000-0000-8000-8000-000000000002";

        fn snapshot() -> Value {
            json!({"event":{"update":{"replaceAllState":true,"conversation":{
            "id": CHAT, "title":"Cloud task", "extras":[
                {"@type":"type.googleapis.com/anthropic.bard.api.v1alpha.WorkspaceUpgradeState", "sessionId":"cse_test01"},
                {"@type":"type.googleapis.com/anthropic.bard.api.v1alpha.CoworkSessionMeta", "future":"preserve"}
            ]}}}})
        }

        fn frame(value: &Value) -> Vec<u8> {
            let body = value.to_string().into_bytes();
            let mut frame = vec![0];
            frame.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
            frame.extend(body);
            frame
        }

        #[test]
        fn load_downloads_attached_and_presented_files_once_and_keeps_associations() {
            use base64::Engine;
            let mut fixture = super::super::tests::fixture();
            fixture["events"][0]["payload"]["file_attachments"] = json!([{
                "file_name":"input.pdf","file_uuid":ORG,"is_image":false
            }]);
            let path = "/mnt/user-data/outputs/budget #1.xlsx";
            fixture["events"][1]["payload"]["message"]["content"] = json!([
                {"type":"tool_use","id":"file1","name":"mcp__cowork__present_files","input":{"filepaths":[path,path]}}
            ]);
            let pdf = b"%PDF-1.7\0\xff".to_vec();
            let sheet = b"PK\x03\x04\0\xff".to_vec();
            let (store, handle) = server_bytes(vec![
                (
                    200,
                    "application/json",
                    fixture["session"].to_string().into_bytes(),
                ),
                (
                    200,
                    "application/json",
                    json!({"data":fixture["events"]}).to_string().into_bytes(),
                ),
                (200, "application/pdf", pdf.clone()),
                (
                    200,
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                    sheet.clone(),
                ),
            ]);
            let native = store
                .load(&store.session_ref("cse_test01", None).unwrap())
                .unwrap();
            let encoded = CoworkRemote::to_text(&native).unwrap();
            let common =
                CoworkRemote::to_common(&CoworkRemote::from_text(&encoded).unwrap()).unwrap();
            for (message, name, expected) in [(0, "input.pdf", pdf), (1, "budget #1.xlsx", sheet)] {
                let file = common.body[message]
                    .content
                    .iter()
                    .find_map(|block| match block {
                        crate::common::Block::Artifact { artifact } => Some(artifact),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(file.name, name);
                let crate::common::ArtifactSource::Base64 { data, .. } = &file.source else {
                    panic!("missing bytes")
                };
                assert_eq!(
                    base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .unwrap(),
                    expected
                );
            }
            let requests = handle.join().unwrap();
            assert_eq!(requests.len(), 4);
            assert!(requests.iter().all(|request| request.starts_with("GET ")));
            assert!(requests[2].contains(&format!("/files/{ORG}/contents")));
            assert!(requests[3].contains("path=%2Fmnt%2Fuser-data%2Foutputs%2Fbudget%20%231.xlsx"));
        }

        #[test]
        fn unavailable_file_fails_with_its_name_instead_of_a_partial_success() {
            let mut fixture = super::super::tests::fixture();
            fixture["events"][0]["payload"]["file_attachments"] =
                json!([{"file_name":"missing.pdf","file_uuid":ORG}]);
            let (store, handle) = server(vec![
                (200, fixture["session"].clone()),
                (200, json!({"data":fixture["events"]})),
                (404, json!({"error":"not found"})),
            ]);
            let error = store
                .load(&store.session_ref("cse_test01", None).unwrap())
                .unwrap_err();
            assert!(error.to_string().contains("missing.pdf"));
            assert_eq!(handle.join().unwrap().len(), 3);
        }

        #[test]
        fn chat_url_resolves_then_reads_all_native_events_without_discovery() {
            let fixture = super::super::tests::fixture();
            let snapshot = snapshot();
            let mut stream = frame(&json!({"event":{"heartbeat":{}}}));
            stream.extend(frame(&snapshot));
            let (store, handle) = server_bytes(vec![
                (200, "application/connect+json", stream),
                (
                    200,
                    "application/json",
                    fixture["session"].to_string().into_bytes(),
                ),
                (
                    200,
                    "application/json",
                    json!({"data":fixture["events"],"next_cursor":null})
                        .to_string()
                        .into_bytes(),
                ),
            ]);
            let reference = store
                .session_ref(&format!("https://claude.ai/chat/{CHAT}"), None)
                .unwrap();
            let native = store.load(&reference).unwrap();
            assert_eq!(native.meta.id, "cse_test01");
            assert_eq!(native.body.extra["$txcript_chat_snapshot"], snapshot);
            assert_eq!(CoworkRemote::to_common(&native).unwrap().body.len(), 4);
            let text = CoworkRemote::to_text(&native).unwrap();
            assert_eq!(CoworkRemote::from_text(&text).unwrap(), native);
            let requests = handle.join().unwrap();
            assert_eq!(requests.len(), 3);
            assert!(requests[0].starts_with("POST /claudeai-rpc/anthropic.bard.api.v1alpha.ConversationService/StreamTimeline HTTP/1.1"));
            assert!(requests[0].contains("\"existingOnly\":true"));
            assert!(
                requests[0]
                    .to_ascii_lowercase()
                    .contains(&format!("x-organization-uuid: {ORG}"))
            );
            assert!(requests[1].starts_with("GET /v1/code/sessions/cse_test01 HTTP/1.1"));
            assert!(requests[2].contains("/events?limit=500&sort_order=asc"));
        }

        #[test]
        fn untrusted_or_ordinary_chat_mappings_never_trigger_a_session_read() {
            let mut wrong_chat = snapshot();
            wrong_chat["event"]["update"]["conversation"]["id"] = json!(ORG);
            let mut ordinary = snapshot();
            ordinary["event"]["update"]["conversation"]["extras"]
                .as_array_mut()
                .unwrap()
                .pop();
            let mut unsafe_id = snapshot();
            unsafe_id["event"]["update"]["conversation"]["extras"][0]["sessionId"] =
                json!("cse_../bad");
            let mut conflicting = snapshot();
            conflicting["event"]["update"]["conversation"]["extras"].as_array_mut().unwrap().push(
                json!({"@type":"type.googleapis.com/anthropic.bard.api.v1alpha.WorkspaceUpgradeState","sessionId":"cse_other"}));
            for bad in [wrong_chat, ordinary, unsafe_id, conflicting] {
                let (store, handle) =
                    server_bytes(vec![(200, "application/connect+json", frame(&bad))]);
                assert!(store.load(&store.session_ref(CHAT, None).unwrap()).is_err());
                assert_eq!(handle.join().unwrap().len(), 1);
            }
        }

        #[test]
        fn failed_and_incomplete_timelines_do_not_fall_back_to_account_discovery() {
            for (status, mime, body) in [
                (403, "application/json", b"{}".to_vec()),
                (302, "text/html", Vec::new()),
                (200, "application/json", b"{}".to_vec()),
                (
                    200,
                    "application/connect+json",
                    frame(&json!({"event":{"heartbeat":{}}})),
                ),
                (200, "application/connect+json", vec![0, 0, 0, 0, 50, b'{']),
            ] {
                let (store, handle) = server_bytes(vec![(status, mime, body)]);
                assert!(store.load(&store.session_ref(CHAT, None).unwrap()).is_err());
                assert_eq!(handle.join().unwrap().len(), 1);
            }
        }

        #[test]
        fn direct_read_walks_all_events_without_listing() {
            let fixture = super::super::tests::fixture();
            let (store, handle) = server(vec![
                (200, fixture["session"].clone()),
                (
                    200,
                    json!({"data": fixture["events"].as_array().unwrap()[..2], "next_cursor":"a/b+?"}),
                ),
                (
                    200,
                    json!({"data": fixture["events"].as_array().unwrap()[2..], "next_cursor":null}),
                ),
            ]);
            let reference = store.session_ref("session_test01", None).unwrap();
            let native = store.load(&reference).unwrap();
            assert_eq!(
                native.body.events,
                fixture["events"].as_array().unwrap().clone()
            );
            assert_eq!(CoworkRemote::to_common(&native).unwrap().body.len(), 4);
            assert!(store.save(&native).is_err());
            assert!(store.delete(&reference).is_err());
            let requests = handle.join().unwrap();
            assert_eq!(requests.len(), 3);
            assert!(requests[0].starts_with("GET /v1/code/sessions/cse_test01 HTTP/1.1"));
            assert!(requests[1].starts_with(
                "GET /v1/code/sessions/cse_test01/events?limit=500&sort_order=asc HTTP/1.1"
            ));
            assert!(requests[2].contains("&cursor=a%2Fb%2B%3F HTTP/1.1"));
            for request in requests {
                let request = request.to_ascii_lowercase();
                assert!(request.contains(&format!("x-organization-uuid: {ORG}")));
                assert!(request.contains("anthropic-beta: ccr-byoc-2025-07-29"));
                assert!(request.contains("anthropic-client-feature: ccr"));
            }
        }

        #[test]
        fn discovery_pages_and_filters_code_sessions() {
            let row = super::super::tests::fixture()["session"]["session"].clone();
            let (store, handle) = server(vec![
                (200, json!({"data":[row], "next_cursor":"next"})),
                (
                    200,
                    json!({"data":[row, {"id":"cse_code", "tags":["code"]}], "next_cursor":null}),
                ),
            ]);
            let found = store.discover().unwrap();
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].reference.organization_uuid, ORG);
            assert_eq!(found[0].meta.id, "cse_test01");
            let requests = handle.join().unwrap();
            assert!(
                requests[0]
                    .starts_with("GET /v1/code/sessions?tags=cowork-remote&limit=200 HTTP/1.1")
            );
            assert!(requests[1].contains("&cursor=next HTTP/1.1"));
        }

        #[test]
        fn bad_pagination_never_returns_a_partial_transcript() {
            for second in [
                (200, json!({"data":[{}], "next_cursor":"repeat"})),
                (200, json!({"data":[], "next_cursor":"new"})),
                (200, json!({"unexpected":true})),
                (403, json!({"error":"test-session-secret"})),
            ] {
                let (store, handle) = server(vec![
                    (200, json!({"data":[{}], "next_cursor":"repeat"})),
                    second,
                ]);
                let error = store
                    .pages("/v1/code/sessions?tags=cowork-remote&limit=200", ORG)
                    .unwrap_err();
                assert!(!error.to_string().contains("test-session-secret"));
                assert_eq!(handle.join().unwrap().len(), 2);
            }
        }

        #[test]
        fn wrong_session_and_wrong_harness_are_refused_before_events() {
            for session in [
                json!({"id":"cse_other", "tags":["cowork-remote"]}),
                json!({"id":"cse_test01", "tags":["code"]}),
            ] {
                let (store, handle) = server(vec![(200, json!({"session":session}))]);
                let reference = store.session_ref("cse_test01", None).unwrap();
                assert!(store.load(&reference).is_err());
                assert_eq!(handle.join().unwrap().len(), 1);
            }
        }

        #[test]
        fn invalid_references_and_mutations_make_no_requests() {
            let (store, handle) = server(vec![]);
            assert!(store.session_ref("cse_../bad", None).is_err());
            assert!(store.session_ref("cse_valid", Some("bad".into())).is_err());
            let reference = CoworkRemoteRef {
                organization_uuid: ORG.into(),
                session_id: "cse_../bad".into(),
                updated_at: None,
            };
            assert!(store.load(&reference).is_err());
            assert!(store.delete(&reference).is_err());
            assert_eq!(handle.join().unwrap(), [] as [String; 0]);
            assert_eq!(CoworkRemote::NAME, "cowork_remote");
        }
    }
}

#[cfg(feature = "cowork_remote")]
pub use remote::{CoworkRemoteRef, CoworkRemoteStore};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::needless_pass_by_value)]
mod tests {
    use super::*;
    use crate::common::{Block, Role};
    use serde_json::json;

    #[test]
    fn ids_and_claude_urls_are_normalized_without_accepting_foreign_urls() {
        let chat = "00000000-0000-8000-8000-000000000002";
        for input in [
            "cse_test01",
            "session_test01",
            "https://claude.ai/cowork/cse_test01?from=desktop",
        ] {
            assert_eq!(normalize_id(input).unwrap(), "cse_test01");
        }
        assert_eq!(normalize_id(chat).unwrap(), chat);
        assert_eq!(
            normalize_id(&format!("https://claude.ai/chat/{chat}/?x=1#anchor")).unwrap(),
            chat
        );
        for input in [
            "https://claude.ai.evil.test/cowork/cse_test01",
            "https://claude.ai@evil.test/cowork/cse_test01",
            "http://claude.ai/cowork/cse_test01",
            "https://claude.ai/cowork/cse_test01/extra",
            "https://claude.ai/chat/00000000-0000-4000-8000-000000000002",
            "https://claude.ai/chat/cse_test01",
            "cse_../bad",
        ] {
            assert!(normalize_id(input).is_err(), "accepted {input}");
        }
    }

    pub(super) fn fixture() -> Value {
        json!({
            "session": {"session": {"id":"cse_test01", "title":"Cloud task",
                "created_at":"2026-09-24T10:00:00Z", "tags":["cowork-remote"],
                "config":{"cwd":"/remote/vm/task", "future":true}}, "future_envelope":42},
            "events": [
                {"event_id":"e1", "sequence_num":1, "created_at":"2026-09-24T10:01:00Z",
                 "payload":{"type":"user", "uuid":"u1", "session_id":"session_test01", "parent_tool_use_id":null,
                   "message":{"role":"user", "content":"Make a report"}}},
                {"event_id":"e2", "payload":{"type":"assistant", "uuid":"a1", "message":{
                   "role":"assistant", "model":"claude-example", "content":[
                     {"type":"thinking", "thinking":"Plan"},
                     {"type":"tool_use", "id":"tool1", "name":"Write", "input":{"file_path":"/tmp/report.md", "content":"Report"}}
                   ]}}},
                {"event_id":"e3", "payload":{"type":"user", "message":{"content":[
                    {"type":"tool_result", "tool_use_id":"tool1", "content":"Created report"}
                ]}}},
                {"event_id":"e4", "payload":{"type":"assistant", "uuid":"a2", "message":{
                    "content":[{"type":"text", "text":"Done"}, {"type":"image", "source":{"type":"base64", "media_type":"image/png", "data":"aGVsbG8="}}],
                    "stop_reason":"end_turn", "usage":{"input_tokens":10,"output_tokens":4}}}},
                {"event_id":"e5", "payload":{"type":"stream_event", "event":{"delta":{"text":"Done"}}}},
                {"event_id":"e6", "payload":{"type":"control_request", "future":{"file":"/outputs/report.md"}}},
                {"event_id":"e7", "payload":{"type":"assistant", "uuid":"sub1", "parent_tool_use_id":"agent1", "message":{"content":"Subagent"}}}
            ],
            "future_native": {"unchanged":true}
        })
    }

    #[test]
    fn native_round_trip_and_common_messages() {
        let value = fixture();
        let native = CoworkRemote::from_text(&value.to_string()).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&CoworkRemote::to_text(&native).unwrap()).unwrap(),
            value
        );
        let common = CoworkRemote::to_common(&native).unwrap();
        assert_eq!(common.meta.id, "cse_test01");
        assert_eq!(common.meta.title.as_deref(), Some("Cloud task"));
        assert_eq!(common.meta.model.as_deref(), Some("claude-example"));
        assert!(common.meta.cwd.is_none());
        assert_eq!(common.body.len(), 4);
        assert_eq!(common.body[0].role, Role::User);
        assert_eq!(
            common.body[0].timestamp.to_rfc3339(),
            "2026-09-24T10:01:00+00:00"
        );
        assert!(
            matches!(&common.body[1].content[0], Block::Thinking { text, .. } if text == "Plan")
        );
        assert!(matches!(&common.body[1].content[1], Block::ToolUse { id, .. } if id == "tool1"));
        assert!(
            matches!(&common.body[2].content[0], Block::ToolResult { tool_use_id, .. } if tool_use_id == "tool1")
        );
        assert!(matches!(&common.body[3].content[1], Block::Image { .. }));
        let local = crate::harness::cowork::Cowork::from_common(&common).unwrap();
        let round = crate::harness::cowork::Cowork::to_common(&local).unwrap();
        assert_eq!(round.body, common.body);
        assert!(CoworkRemote::from_common(&common).is_err());
        assert!(crate::local::write(crate::HarnessId::CoworkRemote, &common, None).is_err());
    }

    #[test]
    fn remote_artifact_paths_remain_tool_references() {
        let mut value = fixture();
        let input =
            json!({"file_path":"/tmp/host-file", "description":"Cloud report", "future":42});
        value["events"][1]["payload"]["message"]["content"] = json!([
            {"type":"tool_use", "id":"tool1", "name":"Artifact", "input":input}
        ]);
        let native = CoworkRemote::from_text(&value.to_string()).unwrap();
        let common = CoworkRemote::to_common(&native).unwrap();
        assert!(
            matches!(&common.body[1].content[0], Block::ToolUse { id, tool:Tool::Raw { tool_name, input:original } } if id == "tool1" && tool_name == "Artifact" && original == &input)
        );
        assert!(common.body.iter().flat_map(|message| &message.content).all(|block| !matches!(block, Block::Artifact { artifact } if matches!(artifact.source, ArtifactSource::Path { .. }))));
    }

    #[test]
    fn overlapping_event_pages_convert_once_but_conflicts_fail() {
        let mut value = fixture();
        let duplicate = value["events"][0].clone();
        value["events"].as_array_mut().unwrap().push(duplicate);
        let native = CoworkRemote::from_text(&value.to_string()).unwrap();
        assert_eq!(native.body.events.len(), 8);
        assert_eq!(CoworkRemote::to_common(&native).unwrap().body.len(), 4);
        value["events"][7]["payload"]["message"]["content"] = json!("Changed");
        let native = CoworkRemote::from_text(&value.to_string()).unwrap();
        assert!(CoworkRemote::to_common(&native).is_err());
    }

    #[test]
    fn malformed_messages_are_not_silently_lost() {
        let mut value = fixture();
        value["events"][0]["payload"]["message"] = json!(false);
        let native = CoworkRemote::from_text(&value.to_string()).unwrap();
        assert!(CoworkRemote::to_common(&native).is_err());
    }

    #[test]
    fn identifiers_cannot_escape_the_provider_route() {
        assert_eq!(normalize_id("session_abc123").unwrap(), "cse_abc123");
        for id in [
            "cse_",
            "cse_../other",
            "cse_x?foo=1",
            "https://evil.test/cse_x",
            "local_x",
            "cse_x\n",
        ] {
            assert!(normalize_id(id).is_err(), "{id}");
        }
        assert!(normalize_id(&format!("cse_{}", "a".repeat(65))).is_err());
        assert_eq!(
            "cowork-remote".parse::<crate::HarnessId>().unwrap(),
            crate::HarnessId::CoworkRemote
        );
    }
}
