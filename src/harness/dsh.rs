//! `dsh` sessions: `$DSH_HOME/sessions` (default `~/.dsh/sessions`).
//!
//! Official persistence writes one append-only JSONL log per session, Zstandard
//! framed by default (`session.jsonl.zstd`). The first line is a `type: session`
//! header; the rest are `SessionEvent` records (and packed `*-chunks` rows).
//! There is no documented session-import CLI, the on-disk format is version 0
//! with no migration, and the persistence seam has no delete API, so
//! [`DshStore`] is read-only: sessions can be discovered, loaded, searched,
//! exported, and converted into another harness, but txcript never writes
//! `~/.dsh/sessions`.
//!
//! Native body retains the header and every log line as raw JSON, including
//! packed chunk rows and unknown event types. The Common projection uses the
//! ordered surface (`user/message`, `assistant/message`, `tool/result`).

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::common::{Block, Message, Meta, Role, StopReason, Tool, ToolOutput};
use crate::error::{Error, Result};
use crate::harness::jsonl;
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

/// On-disk format version stamped into every newly-written `SessionHeader`.
/// Official readers refuse any other value; txcript still loads the native
/// body so unknown future logs are not silently dropped.
const SESSION_FORMAT_VERSION: u64 = 0;

/// The `dsh` harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dsh;

impl Harness for Dsh {
    const NAME: &'static str = "dsh";
    type Body = DshSession;
}

/// Header line plus the remaining JSONL records, kept raw.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DshSession {
    pub header: Value,
    #[serde(default)]
    pub events: Vec<Value>,
}

impl TextCodec for Dsh {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let body: DshSession = serde_json::from_str(text)?;
        let meta = meta_from_body(&body);
        Ok(Transcript::new(meta, body))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Ok(serde_json::to_string_pretty(&transcript.body)?)
    }
}

impl Codec for Dsh {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            events_to_messages(&transcript.body.events, transcript.meta.timestamp),
        ))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            body_from_common(transcript),
        ))
    }
}

/// Read-only access to `dsh` session directories.
#[derive(Debug, Clone)]
pub struct DshStore {
    pub sessions_dir: PathBuf,
}

impl DshStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            sessions_dir: path.into(),
        }
    }

    /// `$DSH_HOME/sessions`, then `~/.dsh/sessions`. Empty `$DSH_HOME` is unset.
    #[must_use]
    pub fn default_root() -> Option<Self> {
        std::env::var_os("DSH_HOME")
            .filter(|value| !value.is_empty() && !value.to_string_lossy().trim().is_empty())
            .map(|home| Self::new(PathBuf::from(home).join("sessions")))
            .or_else(|| super::home_dir().map(|home| Self::new(home.join(".dsh").join("sessions"))))
    }
}

impl Store for DshStore {
    type H = Dsh;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        let mut found = Vec::new();
        let Ok(projects) = fs::read_dir(&self.sessions_dir) else {
            return Ok(found);
        };
        for project in projects.flatten().map(|entry| entry.path()) {
            if !project.is_dir() {
                continue;
            }
            let Ok(sessions) = fs::read_dir(&project) else {
                continue;
            };
            for session_dir in sessions.flatten().map(|entry| entry.path()) {
                if !session_dir.is_dir() {
                    continue;
                }
                let Some(log) = log_path(&session_dir) else {
                    continue;
                };
                let Ok(body) = load_body(&log) else {
                    continue;
                };
                let mut meta = meta_from_body(&body);
                if meta.id.is_empty() {
                    meta.id = jsonl::file_id(&session_dir);
                }
                found.push(Discovered {
                    meta,
                    reference: session_dir,
                });
            }
        }
        Ok(found)
    }

    fn load(&self, reference: &PathBuf) -> Result<Transcript<Dsh>> {
        let log = log_path(reference).ok_or_else(|| Error::Malformed {
            harness: Dsh::NAME,
            detail: format!("no session.jsonl in {}", reference.display()),
        })?;
        let body = load_body(&log)?;
        let mut meta = meta_from_body(&body);
        if meta.id.is_empty() {
            meta.id = jsonl::file_id(reference);
        }
        Ok(Transcript::new(meta, body))
    }

    fn save(&self, _transcript: &Transcript<Dsh>) -> Result<Saved<PathBuf>> {
        Err(read_only_error())
    }

    fn delete(&self, _reference: &PathBuf) -> Result<()> {
        Err(read_only_error())
    }

    fn fingerprints(&self, refs: &[PathBuf]) -> Result<HashMap<String, String>> {
        let mut output = HashMap::with_capacity(refs.len());
        for reference in refs {
            let cursor =
                log_path(reference).map_or_else(String::new, |path| file_fingerprint(&path));
            output.insert(reference.to_string_lossy().into_owned(), cursor);
        }
        Ok(output)
    }
}

fn read_only_error() -> Error {
    Error::Unconvertible {
        harness: Dsh::NAME,
        detail: "dsh session storage is read-only in txcript; dsh has no documented session import command and its persistence seam does not delete logs".to_string(),
    }
}

fn log_path(session_dir: &Path) -> Option<PathBuf> {
    let zstd = session_dir.join("session.jsonl.zstd");
    if zstd.is_file() {
        return Some(zstd);
    }
    let plain = session_dir.join("session.jsonl");
    plain.is_file().then_some(plain)
}

fn load_body(path: &Path) -> Result<DshSession> {
    let text = read_log_text(path)?;
    parse_log(&text)
}

fn parse_log(text: &str) -> Result<DshSession> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let Some(first) = lines.next() else {
        return Err(Error::Malformed {
            harness: Dsh::NAME,
            detail: "empty session log".to_string(),
        });
    };
    let header: Value = serde_json::from_str(first)?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return Err(Error::Malformed {
            harness: Dsh::NAME,
            detail: "first line is not a session header".to_string(),
        });
    }
    let events = lines
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();
    Ok(DshSession { header, events })
}

fn read_log_text(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    if path.extension().is_some_and(|ext| ext == "zstd") {
        return decode_zstd(&bytes);
    }
    String::from_utf8(bytes).map_err(|error| Error::Malformed {
        harness: Dsh::NAME,
        detail: format!("session log is not utf-8: {error}"),
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn decode_zstd(bytes: &[u8]) -> Result<String> {
    // Official JSONL backend concatenates independent Zstandard frames
    // (header frame, then one frame per append batch). Decode each.
    let starts = zstd_frame_starts(bytes);
    let starts = if starts.is_empty() { vec![0] } else { starts };
    let mut out = Vec::new();
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(bytes.len());
        let mut decoder =
            zstd::stream::read::Decoder::new(std::io::Cursor::new(&bytes[*start..end]))?;
        decoder.read_to_end(&mut out)?;
    }
    String::from_utf8(out).map_err(|error| Error::Malformed {
        harness: Dsh::NAME,
        detail: format!("zstd session log is not utf-8: {error}"),
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn zstd_frame_starts(bytes: &[u8]) -> Vec<usize> {
    const MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 4 <= bytes.len() {
        if bytes[index..index + 4] == MAGIC {
            starts.push(index);
            index += 4;
        } else {
            index += 1;
        }
    }
    starts
}

#[cfg(target_arch = "wasm32")]
fn decode_zstd(_bytes: &[u8]) -> Result<String> {
    Err(Error::Malformed {
        harness: Dsh::NAME,
        detail: "zstd session logs cannot be decoded in wasm".to_string(),
    })
}

fn file_fingerprint(path: &Path) -> String {
    let Ok(metadata) = fs::metadata(path) else {
        return String::new();
    };
    let modified = metadata
        .modified()
        .ok()
        .map(|time| {
            DateTime::<Utc>::from(time)
                .timestamp_nanos_opt()
                .unwrap_or_default()
        })
        .unwrap_or_default();
    format!("{}:{modified}", metadata.len())
}

fn meta_from_body(body: &DshSession) -> Meta {
    let header = &body.header;
    let id = header
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let timestamp = header
        .get("createdAt")
        .and_then(Value::as_i64)
        .and_then(DateTime::from_timestamp_millis)
        .or_else(|| first_event_time(&body.events))
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let cwd = header
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(String::from);
    let title = body.events.iter().rev().find_map(|event| {
        (event.get("type").and_then(Value::as_str) == Some("session/title"))
            .then(|| {
                event
                    .get("data")
                    .and_then(|data| data.get("title"))
                    .and_then(Value::as_str)
                    .filter(|title| !title.is_empty())
                    .map(String::from)
            })
            .flatten()
    });
    let model = body.events.iter().rev().find_map(|event| {
        match event.get("type").and_then(Value::as_str) {
            Some("request/context") => event
                .get("data")
                .and_then(|data| data.get("model"))
                .and_then(Value::as_str)
                .map(String::from),
            Some("request/header") => event
                .get("data")
                .and_then(|data| data.get("header"))
                .and_then(|header| header.get("config"))
                .and_then(|config| config.get("model"))
                .and_then(Value::as_str)
                .map(String::from),
            _ => None,
        }
    });
    Meta {
        id,
        timestamp,
        cwd,
        git_branch: None,
        title,
        cli_version: None,
        model,
    }
}

fn first_event_time(events: &[Value]) -> Option<DateTime<Utc>> {
    events
        .iter()
        .find_map(|event| event.get("time").and_then(Value::as_i64))
        .and_then(DateTime::from_timestamp_millis)
}

fn json_usize(value: Option<&Value>) -> Option<usize> {
    value
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

fn event_time(event: &Value, fallback: DateTime<Utc>) -> DateTime<Utc> {
    event
        .get("time")
        .and_then(Value::as_i64)
        .and_then(DateTime::from_timestamp_millis)
        .unwrap_or(fallback)
}

/// Rebuild the ordered surface, then project those nodes into Common messages.
/// Packed `*-chunks` rows and log-only events stay in the native body.
fn events_to_messages(events: &[Value], fallback: DateTime<Utc>) -> Vec<Message> {
    let mut surface: Vec<&Value> = Vec::new();
    for event in events {
        let Some(kind) = event.get("type").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(kind, "user/message" | "assistant/message" | "tool/result") {
            continue;
        }
        match event.get("surfaceOp") {
            Some(Value::Object(op)) if op.get("op").and_then(Value::as_str) == Some("replace") => {
                let start = json_usize(op.get("start")).unwrap_or(0);
                let end = json_usize(op.get("end")).unwrap_or(start);
                if start < surface.len() {
                    let end = end.min(surface.len().saturating_sub(1)).max(start);
                    surface.drain(start..=end);
                    surface.insert(start, event);
                } else {
                    surface.push(event);
                }
            }
            _ => surface.push(event),
        }
    }

    let mut messages = Vec::new();
    for event in surface {
        let timestamp = event_time(event, fallback);
        match event.get("type").and_then(Value::as_str) {
            Some("user/message") => {
                let blocks = user_text_blocks(event.get("data"));
                if !blocks.is_empty() {
                    messages.push(user_message(blocks, timestamp));
                }
            }
            Some("assistant/message") => {
                let data = event.get("data");
                let content = data
                    .and_then(|value| value.get("message"))
                    .and_then(|message| message.get("content"));
                let blocks = assistant_blocks(content);
                if blocks.is_empty() {
                    continue;
                }
                let model = data
                    .and_then(|value| value.get("message"))
                    .and_then(|message| message.get("source"))
                    .and_then(|source| source.get("model"))
                    .and_then(Value::as_str)
                    .map(String::from);
                let has_tool = blocks
                    .iter()
                    .any(|block| matches!(block, Block::ToolUse { .. }));
                messages.push(Message {
                    role: Role::Assistant,
                    content: blocks,
                    timestamp,
                    model,
                    stop_reason: Some(if has_tool {
                        StopReason::ToolUse
                    } else {
                        StopReason::EndTurn
                    }),
                    usage: None,
                });
            }
            Some("tool/result") => {
                if let Some(block) = tool_result_block(event.get("data")) {
                    messages.push(user_message(vec![block], timestamp));
                }
            }
            _ => {}
        }
    }
    messages
}

fn user_message(content: Vec<Block>, timestamp: DateTime<Utc>) -> Message {
    Message {
        role: Role::User,
        content,
        timestamp,
        model: None,
        stop_reason: None,
        usage: None,
    }
}

fn user_text_blocks(data: Option<&Value>) -> Vec<Block> {
    let Some(content) = data
        .and_then(|value| value.get("content"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    content
        .iter()
        .filter_map(|part| {
            if part.get("type").and_then(Value::as_str) != Some("text") {
                return None;
            }
            Some(Block::Text {
                text: part
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .filter(|block| match block {
            Block::Text { text } => !text.is_empty(),
            _ => true,
        })
        .collect()
}

fn assistant_blocks(content: Option<&Value>) -> Vec<Block> {
    let Some(parts) = content.and_then(Value::as_array) else {
        return Vec::new();
    };
    parts
        .iter()
        .filter_map(|part| match part.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                (!text.is_empty()).then(|| Block::Text {
                    text: text.to_string(),
                })
            }
            Some("reasoning") => {
                let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                (!text.is_empty()).then(|| Block::Thinking {
                    text: text.to_string(),
                    signature: None,
                    encrypted: None,
                })
            }
            Some("tool-call") => {
                let id = part
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let name = part
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let input = part
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|raw| serde_json::from_str(raw).ok())
                    .or_else(|| part.get("arguments").cloned())
                    .unwrap_or(Value::Null);
                Some(Block::ToolUse {
                    id,
                    tool: Tool::from_canonical(name, input),
                })
            }
            _ => None,
        })
        .collect()
}

fn tool_result_block(data: Option<&Value>) -> Option<Block> {
    let message = data.and_then(|value| value.get("message"))?;
    let parts = message.get("content")?.as_array()?;
    let result = parts
        .iter()
        .find(|part| part.get("type").and_then(Value::as_str) == Some("tool-result"))?;
    let tool_use_id = result
        .get("toolCallId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .map(|inner| {
            inner
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .map(|part| part.get("text").and_then(Value::as_str).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    Some(Block::ToolResult {
        tool_use_id,
        content: ToolOutput::Text(text),
        is_error,
    })
}

const NS: Uuid = Uuid::from_bytes([
    0x4d, 0x73, 0x68, 0x2d, 0x74, 0x78, 0x63, 0x72, 0x69, 0x70, 0x74, 0x2d, 0x64, 0x73, 0x68, 0x31,
]);

#[allow(clippy::too_many_lines)]
fn body_from_common(transcript: &Transcript<Common>) -> DshSession {
    let created = transcript.meta.timestamp.timestamp_millis();
    let id = if transcript.meta.id.is_empty() {
        format!("session-{}", Uuid::new_v5(&NS, b"empty-id"))
    } else {
        transcript.meta.id.clone()
    };
    let mut header = json!({
        "type": "session",
        "version": SESSION_FORMAT_VERSION,
        "id": id,
        "createdAt": created,
        "delegationDepth": 0,
    });
    if let Some(cwd) = &transcript.meta.cwd {
        header["cwd"] = json!(cwd);
    }
    let mut events = Vec::new();
    let mut seq = 0_u64;
    if let Some(title) = &transcript.meta.title {
        events.push(json!({
            "type": "session/title",
            "seq": seq,
            "time": created,
            "data": { "title": title, "source": { "kind": "fallback" } }
        }));
        seq += 1;
    }
    for (index, message) in transcript.body.iter().enumerate() {
        let time = message.timestamp.timestamp_millis();
        if message.role == Role::User {
            let texts: Vec<Value> = message
                .content
                .iter()
                .filter_map(|block| match block {
                    Block::Text { text } => Some(json!({"type": "text", "text": text})),
                    _ => None,
                })
                .collect();
            if !texts.is_empty() {
                let msg_id = Uuid::new_v5(&NS, format!("{id}:{index}:user").as_bytes()).to_string();
                events.push(json!({
                    "type": "user/message",
                    "seq": seq,
                    "time": time,
                    "data": {
                        "role": "user",
                        "id": msg_id,
                        "content": texts,
                        "source": { "kind": "user" }
                    },
                    "surfaceOp": "append"
                }));
                seq += 1;
            }
            for block in &message.content {
                if let Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } = block
                {
                    let text = match content {
                        ToolOutput::Text(text) => text.clone(),
                        ToolOutput::Json(value) => value.to_string(),
                    };
                    events.push(json!({
                        "type": "tool/result",
                        "seq": seq,
                        "time": time,
                        "data": {
                            "turn": 1,
                            "step": 1,
                            "message": {
                                "role": "user",
                                "source": { "kind": "tool", "callId": tool_use_id },
                                "content": [{
                                    "type": "tool-result",
                                    "toolCallId": tool_use_id,
                                    "isError": is_error,
                                    "content": [{ "type": "text", "text": text }]
                                }]
                            }
                        },
                        "sourceEventSeqs": [],
                        "surfaceOp": "append"
                    }));
                    seq += 1;
                }
            }
        } else {
            let mut content = Vec::new();
            for block in &message.content {
                match block {
                    Block::Text { text } => content.push(json!({"type": "text", "text": text})),
                    Block::Thinking { text, .. } => {
                        content.push(json!({"type": "reasoning", "text": text}));
                    }
                    Block::ToolUse { id: call_id, tool } => {
                        let (name, input) = tool.to_canonical();
                        let arguments =
                            serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                        content.push(json!({
                            "type": "tool-call",
                            "id": call_id,
                            "name": name,
                            "arguments": arguments
                        }));
                    }
                    Block::ToolResult { .. } | Block::Image { .. } | Block::Artifact { .. } => {}
                }
            }
            if content.is_empty() {
                continue;
            }
            let msg_id =
                Uuid::new_v5(&NS, format!("{id}:{index}:assistant").as_bytes()).to_string();
            let mut source = json!({ "kind": "model" });
            if let Some(model) = &message.model {
                source["model"] = json!(model);
            }
            events.push(json!({
                "type": "assistant/message",
                "seq": seq,
                "time": time,
                "data": {
                    "turn": 1,
                    "step": index + 1,
                    "message": {
                        "role": "assistant",
                        "id": msg_id,
                        "content": content,
                        "source": source
                    }
                },
                "surfaceOp": "append"
            }));
            seq += 1;
        }
    }
    let _ = seq;
    DshSession { header, events }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(ms: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(ms).unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
    }

    #[test]
    fn header_line_feeds_meta() {
        let body = DshSession {
            header: json!({
                "type": "session",
                "version": 0,
                "id": "session-abc",
                "createdAt": 1_786_637_231_769_i64,
                "cwd": "/repo",
                "delegationDepth": 0,
                "agentPreset": "standard"
            }),
            events: vec![
                json!({"type": "session/title", "seq": 0, "time": 1, "data": {"title": "hello"}}),
                json!({"type": "request/context", "seq": 1, "time": 2, "data": {"provider": "deepseek-official", "model": "deepseek-v4-flash"}}),
            ],
        };
        let meta = meta_from_body(&body);
        assert_eq!(meta.id, "session-abc");
        assert_eq!(meta.cwd.as_deref(), Some("/repo"));
        assert_eq!(meta.title.as_deref(), Some("hello"));
        assert_eq!(meta.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(meta.timestamp, ts(1_786_637_231_769));
    }

    #[test]
    fn surface_projection_keeps_text_thinking_and_tools() {
        let events = vec![
            json!({"type": "user/message", "seq": 0, "time": 10, "data": {
                "role": "user",
                "content": [{"type": "text", "text": "hi"}],
                "source": {"kind": "user"}
            }, "surfaceOp": "append"}),
            json!({"type": "assistant/chunk", "seq": 1, "time": 11, "data": {"turn": 1, "step": 1, "chunk": {"type": "block-start"}}}),
            json!({"type": "reasoning-chunks", "seq0": 2, "time0": 12, "data": {"texts": ["skip"]}}),
            json!({"type": "assistant/message", "seq": 3, "time": 13, "data": {
                "turn": 1, "step": 1,
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "reasoning", "text": "think"},
                        {"type": "text", "text": "ok"},
                        {"type": "tool-call", "id": "c1", "name": "bash", "arguments": "{\"command\":\"ls\"}"}
                    ],
                    "source": {"kind": "model", "model": "deepseek-v4-flash"}
                }
            }, "surfaceOp": "append"}),
            json!({"type": "tool/result", "seq": 4, "time": 14, "data": {
                "message": {"content": [{
                    "type": "tool-result",
                    "toolCallId": "c1",
                    "isError": false,
                    "content": [{"type": "text", "text": "a.rs"}]
                }]}
            }, "surfaceOp": "append"}),
        ];
        let messages = events_to_messages(&events, DateTime::<Utc>::UNIX_EPOCH);
        assert_eq!(messages.len(), 3);
        assert!(matches!(&messages[0].content[0], Block::Text { text } if text == "hi"));
        assert!(matches!(&messages[1].content[0], Block::Thinking { text, .. } if text == "think"));
        assert!(matches!(&messages[1].content[2], Block::ToolUse { id, .. } if id == "c1"));
        assert_eq!(messages[1].model.as_deref(), Some("deepseek-v4-flash"));
        assert!(matches!(
            &messages[2].content[0],
            Block::ToolResult { tool_use_id, is_error, .. } if tool_use_id == "c1" && !is_error
        ));
    }

    #[test]
    fn surface_replace_drops_shadowed_nodes() {
        let events = vec![
            json!({"type": "user/message", "seq": 0, "time": 1, "data": {
                "content": [{"type": "text", "text": "old"}]
            }, "surfaceOp": "append"}),
            json!({"type": "user/message", "seq": 1, "time": 2, "data": {
                "content": [{"type": "text", "text": "kept"}]
            }, "surfaceOp": "append"}),
            json!({"type": "user/message", "seq": 2, "time": 3, "data": {
                "content": [{"type": "text", "text": "summary"}]
            }, "surfaceOp": {"op": "replace", "start": 0, "end": 0}}),
        ];
        let messages = events_to_messages(&events, DateTime::<Utc>::UNIX_EPOCH);
        assert_eq!(messages.len(), 2);
        assert!(matches!(&messages[0].content[0], Block::Text { text } if text == "summary"));
        assert!(matches!(&messages[1].content[0], Block::Text { text } if text == "kept"));
    }

    #[test]
    fn parse_log_requires_session_header() {
        let err = parse_log("{\"type\":\"user/message\"}\n").unwrap_err();
        assert!(err.to_string().contains("session header"));
    }
}
