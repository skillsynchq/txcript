//! `ChatGPT` (`chatgpt.com`): live, pull-only conversations fetched from
//! `ChatGPT`'s private web API.
//!
//! [`ChatGptStore`] discovers and loads conversations with GET requests.
//! Conversation writes, deletes, and same-harness continues are refused at
//! every boundary. The store reuses Codex's existing `ChatGPT` login from
//! `CODEX_HOME/auth.json` (or `~/.codex/auth.json`) without modifying it.
//!
//! The native [`Conversation`] preserves the complete parent-linked `mapping`
//! and every unmodeled server field. Conversion follows `current_node` to the
//! root. Side branches, citations, UI state, attachments, and unknown content
//! remain in the native body but cannot all be represented in Common.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::common::{Block, Message, Meta, Role, StopReason, Tool, ToolOutput};
use crate::error::{Error, Result};
use crate::transcript::{Codec, Common, Harness, TextCodec, Transcript};

const SYNTHETIC_ID_NAMESPACE: uuid::Uuid =
    uuid::Uuid::from_u128(0x15a7_ba3d_1802_5e72_ba52_4e5d_93e1_1d01);

/// The `ChatGPT` harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatGpt;

impl Harness for ChatGpt {
    const NAME: &'static str = "chatgpt";
    type Body = Conversation;
}

/// One live `/backend-api/conversation/{id}` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    #[serde(default)]
    pub mapping: Map<String, Value>,
    #[serde(default)]
    pub current_node: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Conversation {
    /// The server-side conversation id.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.extra
            .get("conversation_id")
            .or_else(|| self.extra.get("id"))
            .and_then(Value::as_str)
    }
}

impl Codec for ChatGpt {
    #[allow(clippy::too_many_lines)]
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        let nodes = active_path(&transcript.body);
        let mut messages = Vec::new();
        let mut last_timestamp = transcript.meta.timestamp;
        let mut last_tool_id = None;

        for node in nodes {
            let Some(native) = node.get("message").filter(|value| value.is_object()) else {
                continue;
            };
            let role = native
                .pointer("/author/role")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !matches!(role, "user" | "assistant" | "tool") {
                continue;
            }
            if hidden(native) && role != "tool" {
                continue;
            }
            if let Some(timestamp) = value_timestamp(native.get("create_time")) {
                last_timestamp = timestamp;
            }

            if role == "tool" {
                let id = parent_tool_id(&transcript.body, node)
                    .or_else(|| last_tool_id.clone())
                    .unwrap_or_else(|| message_id(native));
                if let Some(content) = content_output(native.get("content")) {
                    messages.push(Message {
                        role: Role::User,
                        content: vec![Block::ToolResult {
                            tool_use_id: id,
                            content,
                            is_error: native
                                .pointer("/metadata/is_error")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        }],
                        timestamp: last_timestamp,
                        model: None,
                        stop_reason: None,
                        usage: None,
                    });
                }
                continue;
            }

            let model = if role == "assistant" {
                message_model(native)
            } else {
                None
            };
            let content_type = native
                .pointer("/content/content_type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let channel = native.pointer("/metadata/channel").and_then(Value::as_str);
            let recipient = native.get("recipient").and_then(Value::as_str);

            let mut blocks = Vec::new();
            if role == "assistant"
                && recipient.is_some_and(|value| !matches!(value, "all" | "assistant"))
            {
                let id = message_id(native);
                let raw = content_text(native.get("content"));
                let input = serde_json::from_str(&raw).unwrap_or_else(|_| json!({ "text": raw }));
                blocks.push(Block::ToolUse {
                    id: id.clone(),
                    tool: Tool::Raw {
                        tool_name: recipient.unwrap_or("tool").to_string(),
                        input,
                    },
                });
                last_tool_id = Some(id);
            } else if matches!(content_type, "thoughts" | "reasoning_recap")
                || channel == Some("commentary")
                || native
                    .pointer("/metadata/is_reasoning")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                let text = reasoning_text(native.get("content"));
                if !text.is_empty() {
                    blocks.push(Block::Thinking {
                        text,
                        signature: None,
                        encrypted: None,
                    });
                }
            } else {
                let text = content_text(native.get("content"));
                if !text.is_empty() {
                    blocks.push(Block::Text { text });
                }
            }

            if blocks.is_empty() {
                continue;
            }
            let has_tool = blocks
                .iter()
                .any(|block| matches!(block, Block::ToolUse { .. }));
            messages.push(Message {
                role: if role == "user" {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: blocks,
                timestamp: last_timestamp,
                model,
                stop_reason: (role == "assistant").then_some(if has_tool {
                    StopReason::ToolUse
                } else if native.get("end_turn").and_then(Value::as_bool) == Some(true) {
                    StopReason::EndTurn
                } else {
                    StopReason::Other("unknown".to_string())
                }),
                usage: None,
            });
        }
        Ok(Transcript::new(transcript.meta.clone(), messages))
    }

    fn from_common(_: &Transcript<Common>) -> Result<Transcript<Self>> {
        Err(read_only_error())
    }
}

impl TextCodec for ChatGpt {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let value: Value = serde_json::from_str(text)?;
        let object = value.as_object().ok_or_else(|| Error::Malformed {
            harness: ChatGpt::NAME,
            detail:
                "expected one live conversation object; account-export arrays are not supported"
                    .to_string(),
        })?;
        let id = object.get("conversation_id").or_else(|| object.get("id"));
        if !object.get("mapping").is_some_and(Value::is_object) || !id.is_some_and(Value::is_string)
        {
            return Err(Error::Malformed {
                harness: ChatGpt::NAME,
                detail:
                    "live response is missing string `conversation_id`/`id` or object `mapping`"
                        .to_string(),
            });
        }
        let body: Conversation = serde_json::from_value(value)?;
        Ok(Transcript::new(meta_from_conversation(&body), body))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Ok(serde_json::to_string_pretty(&transcript.body)?)
    }
}

fn meta_from_conversation(conversation: &Conversation) -> Meta {
    let string = |key: &str| {
        conversation
            .extra
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(String::from)
    };
    let timestamp = value_timestamp(conversation.extra.get("create_time"))
        .or_else(|| value_timestamp(conversation.extra.get("update_time")))
        .unwrap_or_else(Utc::now);
    Meta {
        id: conversation.id().unwrap_or_default().to_string(),
        timestamp,
        cwd: None,
        git_branch: None,
        title: string("title"),
        cli_version: None,
        model: conversation_model(conversation),
    }
}

fn conversation_model(conversation: &Conversation) -> Option<String> {
    active_path(conversation)
        .into_iter()
        .rev()
        .filter_map(|node| node.get("message"))
        .find_map(message_model)
        .or_else(|| {
            conversation
                .extra
                .get("default_model_slug")
                .and_then(Value::as_str)
                .map(String::from)
        })
}

fn message_model(message: &Value) -> Option<String> {
    ["model_slug", "resolved_model_slug"]
        .into_iter()
        .find_map(|key| {
            message
                .pointer(&format!("/metadata/{key}"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(String::from)
        })
}

fn active_path(conversation: &Conversation) -> Vec<&Value> {
    let Some(mut current) = conversation
        .current_node
        .as_deref()
        .filter(|id| !id.is_empty())
    else {
        return fallback_nodes(conversation);
    };
    let mut seen = HashSet::new();
    let mut nodes = Vec::new();
    loop {
        if !seen.insert(current) {
            return fallback_nodes(conversation);
        }
        let Some(node) = conversation.mapping.get(current) else {
            return fallback_nodes(conversation);
        };
        nodes.push(node);
        let Some(parent) = node
            .get("parent")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            break;
        };
        if !conversation.mapping.contains_key(parent) {
            return fallback_nodes(conversation);
        }
        current = parent;
    }
    nodes.reverse();
    nodes
}

fn fallback_nodes(conversation: &Conversation) -> Vec<&Value> {
    let mut nodes: Vec<_> = conversation.mapping.values().collect();
    nodes.sort_by(|left, right| {
        let key = |node: &Value| {
            node.pointer("/message/create_time")
                .and_then(Value::as_f64)
                .unwrap_or(f64::NEG_INFINITY)
        };
        key(left)
            .partial_cmp(&key(right))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                left.get("id")
                    .and_then(Value::as_str)
                    .cmp(&right.get("id").and_then(Value::as_str))
            })
    });
    nodes
}

fn hidden(message: &Value) -> bool {
    message
        .pointer("/metadata/is_visually_hidden_from_conversation")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn message_id(message: &Value) -> String {
    message
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(
            || {
                let bytes = serde_json::to_vec(message).unwrap_or_default();
                uuid::Uuid::new_v5(&SYNTHETIC_ID_NAMESPACE, &bytes).to_string()
            },
            String::from,
        )
}

fn parent_tool_id(conversation: &Conversation, node: &Value) -> Option<String> {
    let parent = node.get("parent").and_then(Value::as_str)?;
    let message = conversation.mapping.get(parent)?.get("message")?;
    let recipient = message.get("recipient").and_then(Value::as_str)?;
    (!matches!(recipient, "all" | "assistant")).then(|| message_id(message))
}

fn content_text(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(text) = content.get("text").and_then(Value::as_str) {
        return text.to_string();
    }
    content
        .get("parts")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn reasoning_text(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(thoughts) = content.get("thoughts").and_then(Value::as_array) {
        return thoughts
            .iter()
            .flat_map(|thought| {
                let mut parts = Vec::new();
                for key in ["summary", "content"] {
                    if let Some(value) = thought.get(key).and_then(Value::as_str) {
                        parts.push(value);
                    }
                }
                if let Some(chunks) = thought.get("chunks").and_then(Value::as_array) {
                    parts.extend(chunks.iter().filter_map(Value::as_str));
                }
                parts
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    if let Some(text) = content.get("content").and_then(Value::as_str) {
        return text.to_string();
    }
    content_text(Some(content))
}

fn content_output(content: Option<&Value>) -> Option<ToolOutput> {
    let content = content?;
    if let Some(text) = content.get("text").and_then(Value::as_str) {
        return Some(parse_output(text));
    }
    let parts = content.get("parts").and_then(Value::as_array)?;
    if parts.iter().any(|part| !part.is_string()) {
        return Some(if parts.len() == 1 {
            ToolOutput::Json(parts[0].clone())
        } else {
            ToolOutput::Json(Value::Array(parts.clone()))
        });
    }
    let text = parts
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then(|| parse_output(&text))
}

fn parse_output(text: &str) -> ToolOutput {
    serde_json::from_str(text).map_or_else(|_| ToolOutput::Text(text.to_string()), ToolOutput::Json)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn value_timestamp(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let value = value?;
    if let Some(seconds) = value.as_i64() {
        return DateTime::from_timestamp(seconds, 0);
    }
    if let Some(seconds) = value.as_f64() {
        let whole = seconds.floor() as i64;
        let nanos = ((seconds - seconds.floor()) * 1_000_000_000.0) as u32;
        return DateTime::from_timestamp(whole, nanos);
    }
    value.as_str().and_then(|text| text.parse().ok())
}

fn read_only_error() -> Error {
    Error::Unconvertible {
        harness: ChatGpt::NAME,
        detail: "ChatGPT is a live read-only source; conversations can be pulled out and converted into another harness, but never written, deleted, or continued in ChatGPT"
            .to_string(),
    }
}

// ── live remote store ──────────────────────────────────────────────────

#[cfg(feature = "chatgpt")]
mod remote {
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use base64::Engine as _;
    use chrono::{DateTime, Utc};
    use futures_util::StreamExt as _;
    use serde::Deserialize;
    use serde_json::Value;
    use uuid::Uuid;

    use super::{ChatGpt, Conversation, meta_from_conversation, read_only_error, value_timestamp};
    use crate::error::{Error, Result};
    use crate::harness::home_dir;
    use crate::transcript::{Discovered, Harness, Saved, Store, Transcript};

    const CHATGPT_BASE_URL: &str = "https://chatgpt.com";
    const PAGE_SIZE: usize = 100;
    const MAX_RESPONSE_BYTES: u64 = 128 * 1024 * 1024;
    const MAX_CODEX_AUTH_BYTES: usize = 1024 * 1024;

    /// A stable reference to one `ChatGPT` conversation.
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub struct ChatGptRef {
        pub conversation_id: String,
        pub updated_at: Option<DateTime<Utc>>,
    }

    impl ChatGptRef {
        #[must_use]
        pub fn key(&self) -> String {
            self.conversation_id.clone()
        }
    }

    #[derive(Debug, Clone)]
    struct Credentials {
        access_token: String,
        account_id: String,
    }

    #[derive(Debug, Deserialize)]
    struct CodexAuth {
        #[serde(default)]
        auth_mode: Option<String>,
        #[serde(default)]
        tokens: Option<CodexTokens>,
    }

    #[derive(Debug, Deserialize)]
    struct CodexTokens {
        access_token: String,
        #[serde(default)]
        account_id: Option<String>,
        #[serde(default)]
        id_token: Option<String>,
    }

    /// Read-only client for `ChatGPT`'s live web conversation store.
    pub struct ChatGptStore {
        credentials: Credentials,
        agent: BrowserTransport,
        base_url: String,
    }

    impl ChatGptStore {
        /// Enumerate `ChatGPT` conversations through an undocumented private
        /// endpoint. Aggregate discovery never calls this method.
        ///
        /// ```compile_fail
        /// #![deny(deprecated)]
        /// use txcript::harness::chatgpt::ChatGptStore;
        /// fn discover(store: &ChatGptStore) {
        ///     let _ = store.discover();
        /// }
        /// ```
        ///
        /// # Errors
        /// When authentication fails or the private response format changes.
        #[deprecated(
            note = "ChatGPT discovery enumerates the selected account through an undocumented private chatgpt.com endpoint that OpenAI can observe or restrict"
        )]
        pub fn discover(&self) -> Result<Vec<Discovered<ChatGptRef>>> {
            <Self as Store>::discover(self)
        }

        /// Reuse the `ChatGPT` login already managed by Codex.
        ///
        /// `txcript` only reads `CODEX_HOME/auth.json` (or
        /// `~/.codex/auth.json`). It never refreshes or rewrites Codex's
        /// credentials.
        ///
        /// # Errors
        /// When Codex is not signed in with `ChatGPT`, its auth file is
        /// malformed or expired, or the HTTP client cannot start.
        pub fn from_codex() -> Result<Self> {
            let credentials = load_codex_credentials(&codex_home()?)?;
            Self::build(credentials, CHATGPT_BASE_URL.to_string())
        }

        /// Build a direct reference without enumerating the account.
        ///
        /// # Errors
        /// When `conversation_id` is not a UUID.
        pub fn conversation_ref(&self, conversation_id: String) -> Result<ChatGptRef> {
            validate_conversation_id(&conversation_id)?;
            Ok(ChatGptRef {
                conversation_id,
                updated_at: None,
            })
        }

        fn build(credentials: Credentials, base_url: String) -> Result<Self> {
            validate_header("access token", &credentials.access_token)?;
            validate_header("account id", &credentials.account_id)?;
            Ok(Self {
                credentials,
                agent: BrowserTransport::start()?,
                base_url,
            })
        }

        fn headers(&self) -> Vec<(&'static str, String)> {
            let mut headers = vec![(
                "authorization",
                format!("Bearer {}", self.credentials.access_token),
            )];
            headers.push(("chatgpt-account-id", self.credentials.account_id.clone()));
            headers.push(("originator", "txcript".to_string()));
            headers
        }

        fn get_json(&self, path: &str) -> Result<Value> {
            let response = self.agent.request(BrowserRequestSpec {
                url: format!("{}{path}", self.base_url),
                headers: self.headers(),
                max_bytes: MAX_RESPONSE_BYTES,
            })?;
            response_json(&response)
        }

        #[cfg(test)]
        fn for_test(access_token: &str, base_url: String) -> Result<Self> {
            Self::build(
                Credentials {
                    access_token: access_token.to_string(),
                    account_id: "account-test".to_string(),
                },
                base_url,
            )
        }
    }

    impl Store for ChatGptStore {
        type H = ChatGpt;
        type Ref = ChatGptRef;

        fn discover(&self) -> Result<Vec<Discovered<Self::Ref>>> {
            eprintln!(
                "warning: ChatGPT discovery enumerates the selected account through an undocumented private chatgpt.com endpoint that OpenAI can observe or restrict"
            );
            let mut out = Vec::new();
            let mut offset = 0;
            loop {
                let path = format!(
                    "/backend-api/conversations?offset={offset}&limit={PAGE_SIZE}&order=updated"
                );
                let value = self.get_json(&path)?;
                let rows = value
                    .get("items")
                    .or_else(|| value.get("conversations"))
                    .or_else(|| value.get("data"))
                    .and_then(Value::as_array)
                    .or_else(|| value.as_array())
                    .ok_or_else(|| protocol_error("conversation list has no array of items"))?;
                for row in rows {
                    let id = row
                        .get("id")
                        .or_else(|| row.get("conversation_id"))
                        .and_then(Value::as_str)
                        .ok_or_else(|| protocol_error("conversation summary has no string id"))?;
                    validate_conversation_id(id)?;
                    let timestamp = value_timestamp(row.get("create_time"))
                        .or_else(|| value_timestamp(row.get("update_time")))
                        .unwrap_or_else(Utc::now);
                    let updated_at = value_timestamp(row.get("update_time"));
                    out.push(Discovered {
                        meta: crate::common::Meta {
                            id: id.to_string(),
                            timestamp,
                            cwd: None,
                            git_branch: None,
                            title: row.get("title").and_then(Value::as_str).map(String::from),
                            cli_version: None,
                            model: row
                                .get("default_model_slug")
                                .and_then(Value::as_str)
                                .map(String::from),
                        },
                        reference: ChatGptRef {
                            conversation_id: id.to_string(),
                            updated_at,
                        },
                    });
                }
                if rows.len() < PAGE_SIZE {
                    break;
                }
                if rows.is_empty() {
                    return Err(protocol_error(
                        "conversation list returned an empty full page",
                    ));
                }
                offset += rows.len();
            }
            Ok(out)
        }

        fn load(&self, reference: &Self::Ref) -> Result<Transcript<Self::H>> {
            validate_conversation_id(&reference.conversation_id)?;
            let value = self.get_json(&format!(
                "/backend-api/conversation/{}",
                reference.conversation_id
            ))?;
            let id = value
                .get("conversation_id")
                .or_else(|| value.get("id"))
                .and_then(Value::as_str);
            if id != Some(reference.conversation_id.as_str())
                || !value.get("mapping").is_some_and(Value::is_object)
            {
                return Err(protocol_error(
                    "conversation detail has the wrong id or no `mapping` object",
                ));
            }
            let body: Conversation =
                serde_json::from_value(value).map_err(|error| Error::Remote {
                    harness: ChatGpt::NAME,
                    detail: format!("ChatGPT conversation shape changed: {error}"),
                })?;
            Ok(Transcript::new(meta_from_conversation(&body), body))
        }

        fn save(&self, _: &Transcript<Self::H>) -> Result<Saved<Self::Ref>> {
            Err(read_only_error())
        }

        fn delete(&self, _: &Self::Ref) -> Result<()> {
            Err(read_only_error())
        }

        fn fingerprints(&self, refs: &[Self::Ref]) -> Result<HashMap<String, String>> {
            Ok(refs
                .iter()
                .map(|reference| {
                    (
                        reference.key(),
                        reference
                            .updated_at
                            .map(|value| value.to_rfc3339())
                            .unwrap_or_default(),
                    )
                })
                .collect())
        }
    }

    struct BrowserTransport {
        sender: mpsc::Sender<BrowserRequest>,
    }

    struct BrowserRequest {
        spec: BrowserRequestSpec,
        reply: mpsc::SyncSender<std::result::Result<BrowserResponse, String>>,
    }

    struct BrowserRequestSpec {
        url: String,
        headers: Vec<(&'static str, String)>,
        max_bytes: u64,
    }

    struct BrowserResponse {
        status: u16,
        body: Vec<u8>,
    }

    impl BrowserTransport {
        fn start() -> Result<Self> {
            let (sender, receiver) = mpsc::channel::<BrowserRequest>();
            let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
            thread::Builder::new()
                .name("txcript-chatgpt-http".to_string())
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| format!("could not start HTTP runtime: {error}"));
                    let client = runtime.as_ref().map_err(Clone::clone).and_then(|_| {
                        wreq::Client::builder()
                            .emulation(wreq_util::Profile::Chrome148)
                            .redirect(wreq::redirect::Policy::none())
                            .timeout(Duration::from_secs(30))
                            .build()
                            .map_err(|error| {
                                format!("could not build browser HTTP client: {error}")
                            })
                    });
                    let startup = match (&runtime, &client) {
                        (Ok(_), Ok(_)) => Ok(()),
                        (Err(error), _) | (_, Err(error)) => Err(error.clone()),
                    };
                    if ready_sender.send(startup).is_err() {
                        return;
                    }
                    let (Ok(runtime), Ok(client)) = (runtime, client) else {
                        return;
                    };
                    while let Ok(request) = receiver.recv() {
                        let result = runtime.block_on(execute(&client, &request.spec));
                        let _ = request.reply.send(result);
                    }
                })
                .map_err(|error| {
                    protocol_error(&format!("could not start HTTP worker: {error}"))
                })?;
            ready_receiver
                .recv()
                .map_err(|_| protocol_error("HTTP worker stopped during startup"))?
                .map_err(|detail| protocol_error(&detail))?;
            Ok(Self { sender })
        }

        fn request(&self, spec: BrowserRequestSpec) -> Result<BrowserResponse> {
            let (reply, response) = mpsc::sync_channel(1);
            self.sender
                .send(BrowserRequest { spec, reply })
                .map_err(|_| protocol_error("HTTP worker stopped before the request"))?;
            response
                .recv()
                .map_err(|_| protocol_error("HTTP worker stopped during the request"))?
                .map_err(|detail| protocol_error(&detail))
        }
    }

    async fn execute(
        client: &wreq::Client,
        spec: &BrowserRequestSpec,
    ) -> std::result::Result<BrowserResponse, String> {
        let mut builder = client
            .get(&spec.url)
            .header(wreq::header::ACCEPT, "application/json")
            .header("originator", "txcript")
            .header("sec-fetch-mode", "cors")
            .header("referer", "https://chatgpt.com/");
        for (name, value) in &spec.headers {
            let mut header = wreq::header::HeaderValue::from_str(value)
                .map_err(|_| format!("could not construct safe `{name}` header"))?;
            header.set_sensitive(true);
            builder = builder.header(*name, header);
        }
        let response = builder
            .send()
            .await
            .map_err(|error| format!("request failed: {error}"))?;
        let status = response.status().as_u16();
        if response
            .content_length()
            .is_some_and(|length| length > spec.max_bytes)
        {
            return Err(format!(
                "response exceeded the {} byte limit",
                spec.max_bytes
            ));
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| format!("failed reading response: {error}"))?;
            if body.len().saturating_add(chunk.len())
                > usize::try_from(spec.max_bytes).unwrap_or(usize::MAX)
            {
                return Err(format!(
                    "response exceeded the {} byte limit",
                    spec.max_bytes
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(BrowserResponse { status, body })
    }

    fn response_json(response: &BrowserResponse) -> Result<Value> {
        if !(200..300).contains(&response.status) {
            let message = serde_json::from_slice::<Value>(&response.body)
                .ok()
                .and_then(|value| {
                    value
                        .pointer("/error/message")
                        .or_else(|| value.get("detail"))
                        .or_else(|| value.get("message"))
                        .and_then(Value::as_str)
                        .map(safe_server_message)
                })
                .filter(|value| !value.is_empty());
            let guidance = match response.status {
                401 => {
                    "Codex's ChatGPT login was rejected or expired; open Codex to refresh it or run `codex login`"
                }
                403 => {
                    "ChatGPT refused the authenticated read; the account may not allow this private endpoint"
                }
                429 => "ChatGPT rate-limited the read; wait and try again",
                _ => "ChatGPT rejected the request",
            };
            return Err(Error::Remote {
                harness: ChatGpt::NAME,
                detail: message.map_or_else(
                    || format!("HTTP {}: {guidance}", response.status),
                    |message| format!("HTTP {}: {guidance}: {message}", response.status),
                ),
            });
        }
        serde_json::from_slice(&response.body).map_err(|error| Error::Remote {
            harness: ChatGpt::NAME,
            detail: format!("ChatGPT returned unexpected JSON: {error}"),
        })
    }

    fn codex_home() -> Result<PathBuf> {
        if let Some(path) = std::env::var_os("CODEX_HOME").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(path));
        }
        home_dir()
            .map(|home| home.join(".codex"))
            .ok_or_else(|| protocol_error("could not resolve Codex's home directory"))
    }

    fn load_codex_credentials(codex_home: &Path) -> Result<Credentials> {
        let path = codex_home.join("auth.json");
        let bytes = fs::read(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::Remote {
                    harness: ChatGpt::NAME,
                    detail: format!(
                        "Codex ChatGPT login not found at {}; sign in to Codex with ChatGPT first",
                        path.display()
                    ),
                }
            } else {
                error.into()
            }
        })?;
        if bytes.len() > MAX_CODEX_AUTH_BYTES {
            return Err(protocol_error(
                "Codex auth file exceeded the 1 MiB safety limit",
            ));
        }
        parse_codex_auth(&bytes)
    }

    fn parse_codex_auth(bytes: &[u8]) -> Result<Credentials> {
        let auth: CodexAuth = serde_json::from_slice(bytes).map_err(|_| Error::Remote {
            harness: ChatGpt::NAME,
            detail: "Codex auth file is malformed; sign in to Codex with ChatGPT again".to_string(),
        })?;
        if auth.auth_mode.as_deref() != Some("chatgpt") {
            return Err(protocol_error(
                "Codex is not signed in with ChatGPT; sign in to Codex with ChatGPT first",
            ));
        }
        let tokens = auth.tokens.ok_or_else(|| {
            protocol_error("Codex auth has no ChatGPT tokens; sign in to Codex again")
        })?;
        validate_header("access token", &tokens.access_token)?;
        if token_expiry(&tokens.access_token)
            .is_some_and(|expiry| expiry <= Utc::now() + chrono::Duration::seconds(60))
        {
            return Err(protocol_error(
                "Codex's ChatGPT access token is expired; open Codex to refresh it or run `codex login`",
            ));
        }
        let account_id = tokens
            .account_id
            .or_else(|| tokens.id_token.as_deref().and_then(account_id_from_token))
            .or_else(|| account_id_from_token(&tokens.access_token))
            .ok_or_else(|| protocol_error("Codex auth has no ChatGPT account id"))?;
        validate_header("account id", &account_id)?;
        Ok(Credentials {
            access_token: tokens.access_token,
            account_id,
        })
    }

    fn token_payload(token: &str) -> Option<Value> {
        let payload = token.split('.').nth(1)?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn token_expiry(token: &str) -> Option<DateTime<Utc>> {
        let seconds = token_payload(token)?.get("exp")?.as_i64()?;
        DateTime::from_timestamp(seconds, 0)
    }

    fn account_id_from_token(token: &str) -> Option<String> {
        let value = token_payload(token)?;
        [
            "/chatgpt_account_id",
            "/https:~1~1api.openai.com~1auth/chatgpt_account_id",
        ]
        .into_iter()
        .find_map(|pointer| {
            value
                .pointer(pointer)
                .and_then(Value::as_str)
                .map(String::from)
        })
    }

    fn validate_conversation_id(id: &str) -> Result<()> {
        Uuid::parse_str(id)
            .map(|_| ())
            .map_err(|_| Error::Malformed {
                harness: ChatGpt::NAME,
                detail: format!("conversation id `{}` is not a UUID", id.escape_debug()),
            })
    }

    fn validate_header(name: &str, value: &str) -> Result<()> {
        if value.is_empty() || value.chars().any(char::is_control) {
            Err(Error::Remote {
                harness: ChatGpt::NAME,
                detail: format!(
                    "Codex's stored {name} is empty or contains unsafe characters; sign in to Codex again"
                ),
            })
        } else {
            Ok(())
        }
    }
    fn safe_server_message(message: &str) -> String {
        message
            .chars()
            .filter(|character| !character.is_control() || *character == ' ')
            .take(300)
            .collect()
    }

    fn protocol_error(detail: &str) -> Error {
        Error::Remote {
            harness: ChatGpt::NAME,
            detail: detail.to_string(),
        }
    }

    #[cfg(test)]
    mod tests {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};

        use serde_json::json;

        use super::*;
        use crate::Codec as _;

        #[test]
        fn reads_codex_chatgpt_auth_without_owning_it() {
            let auth = br#"{
                "auth_mode":"chatgpt",
                "tokens":{
                    "access_token":"test-token",
                    "account_id":"account-test",
                    "id_token":"unused",
                    "refresh_token":"ignored"
                }
            }"#;
            let credentials = parse_codex_auth(auth).unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(credentials.access_token, "test-token");
            assert_eq!(credentials.account_id, "account-test");
        }

        #[test]
        fn rejects_non_chatgpt_codex_auth() {
            let error = parse_codex_auth(br#"{"auth_mode":"apikey"}"#)
                .unwrap_err()
                .to_string();
            assert!(error.contains("not signed in with ChatGPT"));
        }

        #[test]
        fn live_store_uses_get_only_and_preserves_auth_headers() {
            let listener =
                TcpListener::bind(("127.0.0.1", 0)).unwrap_or_else(|error| panic!("{error}"));
            let address = listener
                .local_addr()
                .unwrap_or_else(|error| panic!("{error}"));
            let seen = Arc::new(Mutex::new(Vec::new()));
            let server_seen = Arc::clone(&seen);
            let server = thread::spawn(move || {
                for response in [
                    json!({"items":[{"id":"11111111-1111-4111-8111-111111111111","title":"one","create_time":1,"update_time":2}]}),
                    json!({
                        "conversation_id":"11111111-1111-4111-8111-111111111111",
                        "title":"one",
                        "create_time":1,
                        "current_node":"n1",
                        "mapping":{"n1":{"id":"n1","parent":null,"children":[],"message":{"id":"m1","author":{"role":"user"},"create_time":1,"content":{"content_type":"text","parts":["hello"]}}}}
                    }),
                ] {
                    let (mut stream, _) =
                        listener.accept().unwrap_or_else(|error| panic!("{error}"));
                    let mut bytes = [0_u8; 8192];
                    let count = stream
                        .read(&mut bytes)
                        .unwrap_or_else(|error| panic!("{error}"));
                    server_seen
                        .lock()
                        .unwrap_or_else(|error| panic!("{error}"))
                        .push(String::from_utf8_lossy(&bytes[..count]).into_owned());
                    let body = response.to_string();
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                        .unwrap_or_else(|error| panic!("{error}"));
                }
            });
            let store = ChatGptStore::for_test("test-token", format!("http://{address}"))
                .unwrap_or_else(|error| panic!("{error}"));
            let found = Store::discover(&store).unwrap_or_else(|error| panic!("{error}"));
            let loaded = store
                .load(&found[0].reference)
                .unwrap_or_else(|error| panic!("{error}"));
            let common = ChatGpt::to_common(&loaded).unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(common.body.len(), 1);
            server.join().unwrap_or_else(|_| panic!("server panicked"));
            let requests = seen.lock().unwrap_or_else(|error| panic!("{error}"));
            assert!(requests.iter().all(|request| request.starts_with("GET ")));
            assert!(requests.iter().all(|request| {
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer test-token")
            }));
            assert!(requests.iter().all(|request| {
                request
                    .to_ascii_lowercase()
                    .contains("chatgpt-account-id: account-test")
            }));
        }
    }
}

#[cfg(feature = "chatgpt")]
pub use remote::{ChatGptRef, ChatGptStore};

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn transcript(value: &Value) -> Transcript<ChatGpt> {
        ChatGpt::from_text(&value.to_string()).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn active_path_skips_branches_and_maps_thinking_and_text() {
        let native = transcript(&json!({
            "conversation_id":"11111111-1111-4111-8111-111111111111",
            "title":"contract",
            "create_time":1,
            "current_node":"a2",
            "mapping":{
                "u":{"id":"u","parent":null,"children":["branch","a1"],"message":{"id":"mu","author":{"role":"user"},"create_time":1,"content":{"content_type":"text","parts":["hello"]}}},
                "branch":{"id":"branch","parent":"u","children":[],"message":{"id":"mb","author":{"role":"assistant"},"create_time":2,"content":{"content_type":"text","parts":["wrong branch"]},"end_turn":true}},
                "a1":{"id":"a1","parent":"u","children":["a2"],"message":{"id":"ma1","author":{"role":"assistant"},"create_time":2,"content":{"content_type":"reasoning_recap","parts":["thought"]},"metadata":{"model_slug":"gpt-test"}}},
                "a2":{"id":"a2","parent":"a1","children":[],"message":{"id":"ma2","author":{"role":"assistant"},"create_time":3,"content":{"content_type":"text","parts":["answer"]},"end_turn":true,"metadata":{"model_slug":"gpt-test"}}}
            }
        }));
        let common = ChatGpt::to_common(&native).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(common.body.len(), 3);
        assert!(matches!(common.body[1].content[0], Block::Thinking { .. }));
        assert!(matches!(&common.body[2].content[0], Block::Text { text } if text == "answer"));
        assert_eq!(common.meta.model.as_deref(), Some("gpt-test"));
    }

    #[test]
    fn native_round_trip_preserves_unknown_fields_and_branches() {
        let value = json!({
            "conversation_id":"11111111-1111-4111-8111-111111111111",
            "mapping":{},
            "current_node":null,
            "future":{"nested":true}
        });
        let native = transcript(&value);
        let rendered = ChatGpt::to_text(&native).unwrap_or_else(|error| panic!("{error}"));
        let reparsed: Value =
            serde_json::from_str(&rendered).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(reparsed, value);
    }

    #[test]
    fn every_write_boundary_refuses() {
        let native = transcript(&json!({
            "conversation_id":"11111111-1111-4111-8111-111111111111",
            "mapping":{}
        }));
        let common = ChatGpt::to_common(&native).unwrap_or_else(|error| panic!("{error}"));
        assert!(ChatGpt::from_common(&common).is_err());
    }
}
