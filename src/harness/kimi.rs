//! Kimi Code CLI sessions: `~/.kimi-code/sessions/wd_*/session_*/`.
//!
//! Kimi keeps session metadata in `state.json` and the append-only event stream
//! for each agent in `agents/<name>/wire.jsonl`. The main conversation is the
//! `agents/main` stream. Kimi has no documented session import or deletion
//! interface, so [`KimiStore`] is deliberately read-only: sessions can be
//! discovered, loaded, searched, exported, and converted into another
//! harness, but txcript never writes an undocumented Kimi session.
//!
//! The native body retains both JSON files as raw JSON. This makes loading and
//! rendering a session lossless even when Kimi adds bookkeeping events that
//! txcript does not understand. The Common projection interprets user messages,
//! assistant text/reasoning, tool calls, and tool results.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::common::{Block, Message, Meta, Role, StopReason, Tool, ToolOutput};
use crate::error::{Error, Result};
use crate::harness::jsonl;
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

/// The Kimi Code CLI harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kimi;

impl Harness for Kimi {
    const NAME: &'static str = "kimi";
    type Body = KimiSession;
}

/// The two JSON documents that make up the native main-agent session.
///
/// `state` is the contents of `state.json`; `wire` is the parsed JSONL from
/// `agents/main/wire.jsonl`. Keeping them raw is intentional: Kimi's wire
/// protocol has many bookkeeping event types and they must not be discarded by
/// a native load/render round trip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KimiSession {
    pub state: Value,
    #[serde(default)]
    pub wire: Vec<Value>,
}

impl TextCodec for Kimi {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let body: KimiSession = serde_json::from_str(text)?;
        let meta = meta_from_body(&body);
        Ok(Transcript::new(meta, body))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Ok(serde_json::to_string_pretty(&transcript.body)?)
    }
}

impl Codec for Kimi {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            wire_to_messages(&transcript.body.wire, transcript.meta.timestamp),
        ))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            body_from_common(transcript),
        ))
    }
}

/// Read-only access to Kimi Code's session directories.
#[derive(Debug, Clone)]
pub struct KimiStore {
    pub sessions_dir: PathBuf,
}

impl KimiStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            sessions_dir: path.into(),
        }
    }

    /// Resolve `$KIMI_HOME/sessions`, falling back to
    /// `~/.kimi-code/sessions`.
    #[must_use]
    pub fn default_root() -> Option<Self> {
        std::env::var_os("KIMI_HOME")
            .filter(|v| !v.is_empty())
            .map(|home| Self::new(PathBuf::from(home).join("sessions")))
            .or_else(|| {
                super::home_dir().map(|home| Self::new(home.join(".kimi-code").join("sessions")))
            })
    }
}

impl Store for KimiStore {
    type H = Kimi;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        let mut found = Vec::new();
        let Ok(entries) = fs::read_dir(&self.sessions_dir) else {
            return Ok(found);
        };
        for workspace in entries.flatten().map(|entry| entry.path()) {
            if !workspace.is_dir() {
                continue;
            }
            let Ok(sessions) = fs::read_dir(&workspace) else {
                continue;
            };
            for session_dir in sessions.flatten().map(|entry| entry.path()) {
                if !session_dir.is_dir() {
                    continue;
                }
                let state_path = session_dir.join("state.json");
                let wire_path = session_dir.join("agents").join("main").join("wire.jsonl");
                if !state_path.is_file() || !wire_path.is_file() {
                    continue;
                }
                let Ok(body) = load_body(&session_dir) else {
                    continue;
                };
                let mut meta = meta_from_body(&body);
                if meta.id.is_empty() {
                    meta.id = session_id_from_path(&session_dir);
                }
                found.push(Discovered {
                    meta,
                    reference: session_dir,
                });
            }
        }
        Ok(found)
    }

    fn load(&self, reference: &PathBuf) -> Result<Transcript<Kimi>> {
        let body = load_body(reference)?;
        let mut meta = meta_from_body(&body);
        if meta.id.is_empty() {
            meta.id = session_id_from_path(reference);
        }
        Ok(Transcript::new(meta, body))
    }

    fn save(&self, _transcript: &Transcript<Kimi>) -> Result<Saved<PathBuf>> {
        Err(read_only_error())
    }

    fn delete(&self, _reference: &PathBuf) -> Result<()> {
        Err(read_only_error())
    }

    fn fingerprints(&self, refs: &[PathBuf]) -> Result<HashMap<String, String>> {
        let mut output = HashMap::with_capacity(refs.len());
        for reference in refs {
            let state = file_fingerprint(&reference.join("state.json"));
            let wire = file_fingerprint(&reference.join("agents").join("main").join("wire.jsonl"));
            output.insert(
                reference.to_string_lossy().into_owned(),
                format!("{state}:{wire}"),
            );
        }
        Ok(output)
    }
}

fn read_only_error() -> Error {
    Error::Unconvertible {
        harness: Kimi::NAME,
        detail: "Kimi Code session storage is read-only in txcript; Kimi has no documented session import or delete command".to_string(),
    }
}

fn load_body(reference: &Path) -> Result<KimiSession> {
    let state: Value = serde_json::from_str(&fs::read_to_string(reference.join("state.json"))?)?;
    let wire = jsonl::parse(&fs::read_to_string(
        reference.join("agents").join("main").join("wire.jsonl"),
    )?);
    Ok(KimiSession { state, wire })
}

/// Last-resort id: the session directory name, as every other file-backed
/// store does. Discovery already rejects anything without a `state.json` and a
/// main wire log, so the name is a label, not a filter — a Kimi release that
/// renames its directories must not make sessions vanish from `list`.
fn session_id_from_path(path: &Path) -> String {
    jsonl::file_id(path)
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

fn meta_from_body(body: &KimiSession) -> Meta {
    let state = body.state.as_object();
    let string = |key: &str| {
        state
            .and_then(|object| object.get(key))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(String::from)
    };
    let id = string("sessionId")
        .or_else(|| string("id"))
        .or_else(|| id_from_agent_homedir(&body.state))
        .unwrap_or_default();
    let timestamp = state
        .and_then(|object| object.get("createdAt"))
        .and_then(parse_timestamp)
        .or_else(|| first_event_timestamp(&body.wire))
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let title = string("title").filter(|title| title != "New Session");
    let model = body.wire.iter().find_map(|event| {
        (event.get("type") == Some(&Value::String("llm.request".to_string())))
            .then(|| event.get("model").and_then(Value::as_str).map(String::from))
            .flatten()
    });
    Meta {
        id,
        timestamp,
        cwd: string("workDir").or_else(|| string("cwd")),
        git_branch: string("gitBranch").or_else(|| string("git_branch")),
        title,
        cli_version: string("cliVersion").or_else(|| string("cli_version")),
        model,
    }
}

/// Recover the session id from `agents.<name>.homedir`.
///
/// Kimi's `state.json` gained a top-level `id` in schema version 2; version 1
/// carries no id field at all. Every version does record each agent's absolute
/// home directory, which lives inside the `session_<uuid>` directory, so the
/// path is the only id a bare version-1 `state.json` carries. Without this the
/// text codec — and the wasm parser built on it — would silently drop the id
/// for older sessions. The store still falls back to the directory name.
fn id_from_agent_homedir(state: &Value) -> Option<String> {
    let agents = state.get("agents")?.as_object()?;
    let homedir = agents
        .get("main")
        .into_iter()
        .chain(agents.values())
        .find_map(|agent| agent.get("homedir").and_then(Value::as_str))?;
    homedir
        .split(['/', '\\'])
        .find(|segment| segment.starts_with("session_"))
        .map(String::from)
}

fn parse_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    if let Some(text) = value.as_str() {
        return text.parse().ok();
    }
    value.as_i64().and_then(DateTime::from_timestamp_millis)
}

fn first_event_timestamp(events: &[Value]) -> Option<DateTime<Utc>> {
    events.iter().find_map(|event| {
        event.get("time").and_then(parse_timestamp).or_else(|| {
            event
                .get("event")
                .and_then(|inner| inner.get("time"))
                .and_then(parse_timestamp)
        })
    })
}

fn event_time(event: &Value, fallback: DateTime<Utc>) -> DateTime<Utc> {
    event
        .get("time")
        .and_then(parse_timestamp)
        .or_else(|| {
            event
                .get("event")
                .and_then(|inner| inner.get("time"))
                .and_then(parse_timestamp)
        })
        .unwrap_or(fallback)
}

#[derive(Default)]
struct MessageBuilder {
    messages: Vec<Message>,
    assistant: Vec<Block>,
    assistant_time: Option<DateTime<Utc>>,
    assistant_model: Option<String>,
    results: Vec<Block>,
    result_time: Option<DateTime<Utc>>,
    tool_sequence: usize,
    /// Message count at each `turn.prompt`, so `context.undo` can rewind whole
    /// turns rather than individual messages.
    turn_starts: Vec<usize>,
}

impl MessageBuilder {
    fn flush_assistant(&mut self, fallback: DateTime<Utc>) {
        if self.assistant.is_empty() {
            return;
        }
        let has_tool = self
            .assistant
            .iter()
            .any(|block| matches!(block, Block::ToolUse { .. }));
        self.messages.push(Message {
            role: Role::Assistant,
            content: std::mem::take(&mut self.assistant),
            timestamp: self.assistant_time.take().unwrap_or(fallback),
            model: self.assistant_model.take(),
            stop_reason: Some(if has_tool {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            }),
            usage: None,
        });
    }

    fn flush_results(&mut self, fallback: DateTime<Utc>) {
        if self.results.is_empty() {
            return;
        }
        self.messages.push(Message {
            role: Role::User,
            content: std::mem::take(&mut self.results),
            timestamp: self.result_time.take().unwrap_or(fallback),
            model: None,
            stop_reason: None,
            usage: None,
        });
    }

    fn add_assistant(&mut self, block: Block, timestamp: DateTime<Utc>, model: Option<String>) {
        self.flush_results(timestamp);
        self.assistant_time.get_or_insert(timestamp);
        if model.is_some() {
            self.assistant_model = model;
        }
        self.assistant.push(block);
    }

    fn add_result(&mut self, block: Block, timestamp: DateTime<Utc>) {
        self.flush_assistant(timestamp);
        self.result_time.get_or_insert(timestamp);
        self.results.push(block);
    }

    fn add_user(&mut self, blocks: Vec<Block>, timestamp: DateTime<Utc>) {
        self.flush_assistant(timestamp);
        self.flush_results(timestamp);
        if !blocks.is_empty() {
            self.messages.push(Message {
                role: Role::User,
                content: blocks,
                timestamp,
                model: None,
                stop_reason: None,
                usage: None,
            });
        }
    }

    fn finish(mut self, fallback: DateTime<Utc>) -> Vec<Message> {
        self.flush_assistant(fallback);
        self.flush_results(fallback);
        self.messages
    }

    /// Record a turn boundary from `turn.prompt`.
    fn begin_turn(&mut self, fallback: DateTime<Utc>) {
        self.flush_assistant(fallback);
        self.flush_results(fallback);
        self.turn_starts.push(self.messages.len());
    }

    /// Apply a Kimi `context.undo`: rewind the last `count` turns.
    ///
    /// Kimi rewinds its context after a failed or cancelled turn, then re-sends
    /// the prompt as a fresh turn. `wire.jsonl` is append-only, so the
    /// rolled-back entries stay on disk; replaying them would resurrect prompts
    /// the user already retried. `count` is measured in turns, not entries: a
    /// single turn can append several messages, and one `count: 1` undo drops
    /// all of them. Logs without `turn.prompt` markers fall back to entry
    /// granularity. Pending buffers are flushed first because a half-assembled
    /// turn belongs to the range being rewound.
    fn undo(&mut self, count: usize, fallback: DateTime<Utc>) {
        self.flush_assistant(fallback);
        self.flush_results(fallback);
        if self.turn_starts.is_empty() {
            let keep = self.messages.len().saturating_sub(count);
            self.messages.truncate(keep);
            return;
        }
        let rewind_to = self.turn_starts.len().saturating_sub(count);
        let keep = self.turn_starts.get(rewind_to).copied().unwrap_or(0);
        self.turn_starts.truncate(rewind_to);
        self.messages.truncate(keep);
    }
}

#[allow(clippy::too_many_lines)]
fn wire_to_messages(wire: &[Value], fallback: DateTime<Utc>) -> Vec<Message> {
    let mut builder = MessageBuilder::default();
    let mut model: Option<String> = None;
    for event in wire {
        match event.get("type").and_then(Value::as_str) {
            Some("llm.request") => {
                model = event.get("model").and_then(Value::as_str).map(String::from);
            }
            Some("turn.prompt") => builder.begin_turn(event_time(event, fallback)),
            Some("context.undo") => {
                let count =
                    usize::try_from(event.get("count").and_then(Value::as_u64).unwrap_or(1))
                        .unwrap_or(usize::MAX);
                builder.undo(count, event_time(event, fallback));
            }
            Some("context.append_message") => {
                let Some(message) = event.get("message") else {
                    continue;
                };
                if message.get("role").and_then(Value::as_str) != Some("user") {
                    continue;
                }
                let blocks = message
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|content| {
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
                            .collect()
                    })
                    .unwrap_or_default();
                builder.add_user(blocks, event_time(event, fallback));
            }
            Some("context.append_loop_event") => {
                let Some(inner) = event.get("event") else {
                    continue;
                };
                let timestamp = event_time(event, fallback);
                match inner.get("type").and_then(Value::as_str) {
                    Some("content.part") => {
                        let Some(part) = inner.get("part") else {
                            continue;
                        };
                        match part.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                if let Some(text) = part.get("text").and_then(Value::as_str) {
                                    builder.add_assistant(
                                        Block::Text {
                                            text: text.to_string(),
                                        },
                                        timestamp,
                                        model.clone(),
                                    );
                                }
                            }
                            Some("think") => {
                                if let Some(text) = part.get("think").and_then(Value::as_str) {
                                    builder.add_assistant(
                                        Block::Thinking {
                                            text: text.to_string(),
                                            signature: None,
                                            encrypted: None,
                                        },
                                        timestamp,
                                        model.clone(),
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                    Some("tool.call") => {
                        builder.tool_sequence += 1;
                        let id = inner.get("toolCallId").and_then(Value::as_str).map_or_else(
                            || format!("kimi-tool-{}", builder.tool_sequence),
                            String::from,
                        );
                        let name = inner
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        let input = inner.get("args").cloned().unwrap_or(Value::Null);
                        builder.add_assistant(
                            Block::ToolUse {
                                id,
                                tool: Tool::from_canonical(name, input),
                            },
                            timestamp,
                            model.clone(),
                        );
                    }
                    Some("tool.result") => {
                        let id = inner
                            .get("toolCallId")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        let result = inner.get("result").cloned().unwrap_or(Value::Null);
                        let (content, is_error) = tool_result(result);
                        builder.add_result(
                            Block::ToolResult {
                                tool_use_id: id,
                                content,
                                is_error,
                            },
                            timestamp,
                        );
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    builder.finish(fallback)
}

fn tool_result(result: Value) -> (ToolOutput, bool) {
    let Some(object) = result.as_object() else {
        return (tool_output(result), false);
    };
    let is_error = object
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut output = object.get("output").cloned().unwrap_or(Value::Null);
    if let Some(note) = object.get("note").and_then(Value::as_str)
        && let Some(text) = output.as_str()
    {
        output = Value::String(format!("{text}\n\n[kimi note: {note}]"));
    }
    (tool_output(output), is_error)
}

fn tool_output(value: Value) -> ToolOutput {
    match value {
        Value::String(text) => ToolOutput::Text(text),
        other => ToolOutput::Json(other),
    }
}

/// Namespace for the bookkeeping ids txcript has to synthesize when rendering a
/// Kimi wire log. Kimi itself uses random v4 uuids; deriving v5 ids keeps
/// rendering deterministic so the same transcript always produces the same
/// bytes.
const NS: Uuid = Uuid::from_bytes([
    0x7b, 0x2c, 0x41, 0xd6, 0x8f, 0x53, 0x4a, 0x19, 0xb0, 0x64, 0x3e, 0xa7, 0x15, 0xc8, 0x92, 0x0d,
]);

/// The wire protocol version txcript renders. Observed in Kimi Code sessions;
/// the reader does not depend on it.
const PROTOCOL_VERSION: &str = "1.5";

fn body_from_common(transcript: &Transcript<Common>) -> KimiSession {
    let timestamp = transcript.meta.timestamp.timestamp_millis();
    let state = json!({
        "sessionId": transcript.meta.id,
        "createdAt": timestamp,
        "title": transcript.meta.title,
        "workDir": transcript.meta.cwd,
    });
    let mut wire = vec![
        json!({"type": "metadata", "protocol_version": PROTOCOL_VERSION, "created_at": timestamp}),
    ];
    for (index, message) in transcript.body.iter().enumerate() {
        let time = message.timestamp.timestamp_millis();
        if message.role == Role::User {
            let mut content = Vec::new();
            for block in &message.content {
                if let Block::Text { text } = block {
                    content.push(json!({"type": "text", "text": text}));
                }
            }
            if !content.is_empty() {
                wire.push(json!({"type": "context.append_message", "time": time, "message": {"role": "user", "content": content, "toolCalls": []}}));
            }
            for block in &message.content {
                if let Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } = block
                {
                    let output = match content {
                        ToolOutput::Text(text) => Value::String(text.clone()),
                        ToolOutput::Json(value) => value.clone(),
                    };
                    wire.push(json!({"type": "context.append_loop_event", "time": time, "event": {"type": "tool.result", "toolCallId": tool_use_id, "result": {"output": output, "isError": is_error}}}));
                }
            }
        } else {
            let step_uuid = Uuid::new_v5(
                &NS,
                format!("{}:{index}:step", transcript.meta.id).as_bytes(),
            )
            .to_string();
            wire.push(json!({"type": "llm.request", "time": time, "model": message.model}));
            wire.push(json!({"type": "context.append_loop_event", "time": time, "event": {"type": "step.begin", "uuid": step_uuid, "turnId": "0", "step": 1}}));
            for block in &message.content {
                match block {
                    Block::Text { text } => wire.push(json!({"type": "context.append_loop_event", "time": time, "event": {"type": "content.part", "part": {"type": "text", "text": text}}})),
                    Block::Thinking { text, .. } => wire.push(json!({"type": "context.append_loop_event", "time": time, "event": {"type": "content.part", "part": {"type": "think", "think": text}}})),
                    Block::ToolUse { id, tool } => {
                        let (name, input) = tool.to_canonical();
                        wire.push(json!({"type": "context.append_loop_event", "time": time, "event": {"type": "tool.call", "toolCallId": id, "name": name, "args": input}}));
                    }
                    Block::ToolResult { .. } | Block::Image { .. } | Block::Artifact { .. } => {}
                }
            }
            wire.push(json!({"type": "context.append_loop_event", "time": time, "event": {"type": "step.end", "finishReason": "stop"}}));
        }
    }
    KimiSession { state, wire }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_kimi_timestamp_shapes() {
        assert_eq!(
            parse_timestamp(&json!(1_785_766_971_574_i64))
                .unwrap()
                .timestamp_millis(),
            1_785_766_971_574_i64
        );
        assert!(parse_timestamp(&json!("2026-08-03T14:22:52.557Z")).is_some());
    }

    #[test]
    fn tool_result_keeps_note_and_error() {
        let (content, error) =
            tool_result(json!({"output": "boom", "note": "truncated", "isError": true}));
        assert!(error);
        assert!(matches!(content, ToolOutput::Text(text) if text.contains("truncated")));
    }

    #[test]
    fn session_id_is_recovered_from_every_state_schema() {
        let meta = |state: Value| {
            meta_from_body(&KimiSession {
                state,
                wire: Vec::new(),
            })
            .id
        };
        // Schema version 2 records the id directly.
        assert_eq!(
            meta(json!({"version": 2, "id": "session_abc", "cwd": "/repo"})),
            "session_abc"
        );
        // Version 1 has no id field; the agent home directory is the only
        // copy a bare state.json carries.
        assert_eq!(
            meta(json!({
                "workDir": "/repo",
                "agents": {"main": {"homedir": "/home/u/.kimi-code/sessions/wd_repo_1/session_abc/agents/main"}}
            })),
            "session_abc"
        );
        // Kimi's own index calls the field sessionId.
        assert_eq!(meta(json!({"sessionId": "session_abc"})), "session_abc");
        assert_eq!(meta(json!({"workDir": "/repo"})), "");
    }

    #[test]
    fn context_undo_rewinds_whole_turns() {
        let user = |text: &str, time: i64| {
            json!({"type": "context.append_message", "time": time,
                "message": {"role": "user", "content": [{"type": "text", "text": text}]}})
        };
        // Kimi's retry shape: one turn appends the prompt *and* a system
        // reminder, fails, and is rewound with `count: 1`. Counting entries
        // instead of turns would leave the prompt behind and duplicate it.
        let wire = vec![
            json!({"type": "turn.prompt", "time": 1_i64}),
            user("continue", 2_i64),
            user("<system-reminder>", 3_i64),
            json!({"type": "turn.ended", "turnId": 6, "reason": "failed"}),
            json!({"type": "context.undo", "count": 1, "time": 4_i64}),
            json!({"type": "turn.prompt", "time": 5_i64}),
            user("continue", 6_i64),
            user("<system-reminder>", 7_i64),
        ];
        let messages = wire_to_messages(&wire, DateTime::<Utc>::UNIX_EPOCH);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].timestamp.timestamp_millis(), 6_i64);
    }

    #[test]
    fn context_undo_rewinds_several_turns_at_once() {
        let user = |text: &str, time: i64| {
            json!({"type": "context.append_message", "time": time,
                "message": {"role": "user", "content": [{"type": "text", "text": text}]}})
        };
        let wire = vec![
            json!({"type": "turn.prompt", "time": 1_i64}),
            user("keep me", 1_i64),
            json!({"type": "turn.prompt", "time": 2_i64}),
            user("first retry", 2_i64),
            json!({"type": "turn.prompt", "time": 3_i64}),
            user("second retry", 3_i64),
            json!({"type": "context.undo", "count": 2, "time": 4_i64}),
        ];
        let messages = wire_to_messages(&wire, DateTime::<Utc>::UNIX_EPOCH);
        assert_eq!(messages.len(), 1);
        assert!(matches!(
            &messages[0].content[0],
            Block::Text { text } if text == "keep me"
        ));
    }

    #[test]
    fn context_undo_without_turn_markers_falls_back_to_entries() {
        let user = |text: &str, time: i64| {
            json!({"type": "context.append_message", "time": time,
                "message": {"role": "user", "content": [{"type": "text", "text": text}]}})
        };
        let wire = vec![
            user("continue", 1_786_970_050_554_i64),
            json!({"type": "context.undo", "count": 1, "time": 1_786_970_609_991_i64}),
            user("continue", 1_786_970_613_673_i64),
        ];
        let messages = wire_to_messages(&wire, DateTime::<Utc>::UNIX_EPOCH);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].timestamp.timestamp_millis(),
            1_786_970_613_673_i64
        );
    }

    #[test]
    fn context_undo_rewinds_a_pending_assistant_turn() {
        let wire = vec![
            json!({"type": "context.append_message", "time": 1_i64,
                "message": {"role": "user", "content": [{"type": "text", "text": "hi"}]}}),
            json!({"type": "context.append_loop_event", "time": 2_i64,
                "event": {"type": "content.part", "part": {"type": "text", "text": "half"}}}),
            json!({"type": "context.undo", "count": 1, "time": 3_i64}),
        ];
        let messages = wire_to_messages(&wire, DateTime::<Utc>::UNIX_EPOCH);
        // No turn markers, so entry granularity: the half-assembled assistant
        // turn is the most recent entry and the user prompt survives.
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::User);
    }
}
