//! Grok CLI (Grok Build): `~/.grok/sessions/<urlencoded-cwd>/<session-uuid>/`.
//!
//! A Grok session is a *directory* of files, three of which matter here:
//!
//! - `chat_history.jsonl` — the model-protocol log, one record per line typed
//!   by `type`: `system`, `user`, `assistant` (text + `tool_calls`),
//!   `reasoning` (summary + `encrypted_content`), `tool_result`. This is what
//!   the model sees on continuation.
//! - `updates.jsonl` — the ACP display log (`session/update` notifications:
//!   `user_message_chunk`, `agent_message_chunk`, `agent_thought_chunk`,
//!   `tool_call`, `tool_call_update`, `turn_completed`, …). This is what
//!   `grok --resume` and `grok export` actually replay; a session without it
//!   is invisible to Grok.
//! - `summary.json` — session metadata (id, cwd, title, model, git info).
//!
//! The remaining sidecars (`events.jsonl` telemetry, `signals.json`,
//! `prompt_context.json`, `resources_state.json`, `rewind_points.jsonl`,
//! `system_prompt.txt`) are carried verbatim in the [`GrokSession`] body so a
//! native load → save round-trips them; `from_common` leaves them empty and
//! Grok regenerates what it needs.
//!
//! `to_common` reads the conversation from `chat_history.jsonl` and backfills
//! what that log lacks from `updates.jsonl`: per-message timestamps, tool-result
//! error status, and per-turn stop reasons. `from_common` regenerates
//! `chat_history.jsonl`, `updates.jsonl`, and `summary.json`.
//!
//! User images follow Grok's own split: the display log carries the bytes
//! (an ACP `{"type": "image", data, mimeType}` chunk of the prompt), while
//! the model log holds only text — natively a Grok-generated description;
//! `from_common` writes an `[image]` placeholder there.
//!
//! `from_common` cannot preserve per-turn usage, non-turn stop reasons,
//! reasoning signatures, structured tool-result JSON, reasoning record ids, or
//! system prompt records.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::common::{Block, ImageSource, Message, Meta, Role, StopReason, Tool, ToolOutput};
use crate::error::Result;
use crate::harness::jsonl;
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

/// The Grok CLI harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grok;

impl Harness for Grok {
    const NAME: &'static str = "grok";
    type Body = GrokSession;
}

// ── native records ─────────────────────────────────────────────────────

/// Faithful in-memory representation of one Grok session directory.
///
/// `chat_history` is typed per record kind; everything else is raw JSON so
/// native ↔ disk stays lossless without modeling Grok's internals. The
/// `terminal/` output logs and `*.lock` files are intentionally not carried
/// (ephemeral tool-output scratch, not session state).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrokSession {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chat_history: Vec<ChatRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub updates: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rewind_points: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_context: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources_state: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signals: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

/// One `chat_history.jsonl` line. Known kinds are typed; anything else (or a
/// malformed known kind) is kept whole in [`ChatRecord::Other`].
#[derive(Debug, Clone, PartialEq)]
pub enum ChatRecord {
    System(SystemLine),
    User(UserLine),
    Assistant(AssistantLine),
    Reasoning(ReasoningLine),
    ToolResult(ToolResultLine),
    Other(Value),
}

/// A `system` line: the injected system prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemLine {
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub content: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A `user` line: content blocks, plus `prior_turn_interrupt` when the
/// previous turn was aborted mid-flight.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserLine {
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub content: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_turn_interrupt: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// An `assistant` line: response text plus any `tool_calls` (whose
/// `arguments` are JSON-in-a-string).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantLine {
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub content: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_fingerprint: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A `reasoning` line: summary text plus the opaque provider reasoning token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReasoningLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub summary: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A `tool_result` line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResultLine {
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub content: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl From<Value> for ChatRecord {
    fn from(v: Value) -> Self {
        // Deserializing from `&v` copies each retained field once — no
        // whole-tree clone — and leaves `v` intact for the fallback. The
        // `type` tag lives outside the typed structs, so the flatten sweeps
        // it into `extra`; it is dropped there to keep it from duplicating.
        fn typed<T: for<'de> Deserialize<'de>>(
            v: &Value,
            extra: impl Fn(&mut T) -> &mut Map<String, Value>,
            f: impl Fn(T) -> ChatRecord,
        ) -> Option<ChatRecord> {
            T::deserialize(v).ok().map(|mut line| {
                extra(&mut line).remove("type");
                f(line)
            })
        }
        let record = match v.get("type").and_then(Value::as_str) {
            Some("system") => typed(&v, |l: &mut SystemLine| &mut l.extra, ChatRecord::System),
            Some("user") => typed(&v, |l: &mut UserLine| &mut l.extra, ChatRecord::User),
            Some("assistant") => typed(
                &v,
                |l: &mut AssistantLine| &mut l.extra,
                ChatRecord::Assistant,
            ),
            Some("reasoning") => typed(
                &v,
                |l: &mut ReasoningLine| &mut l.extra,
                ChatRecord::Reasoning,
            ),
            Some("tool_result") => typed(
                &v,
                |l: &mut ToolResultLine| &mut l.extra,
                ChatRecord::ToolResult,
            ),
            // Unknown or untagged kinds stay whole in `Other` below.
            _ => None,
        };
        record.unwrap_or(ChatRecord::Other(v))
    }
}

impl From<ChatRecord> for Value {
    fn from(r: ChatRecord) -> Self {
        fn tagged(line: impl Serialize, ty: &str) -> Value {
            let mut v = serde_json::to_value(line).unwrap_or(Value::Null);
            if let Value::Object(obj) = &mut v {
                obj.insert("type".into(), Value::String(ty.into()));
            }
            v
        }
        match r {
            ChatRecord::System(l) => tagged(l, "system"),
            ChatRecord::User(l) => tagged(l, "user"),
            ChatRecord::Assistant(l) => tagged(l, "assistant"),
            ChatRecord::Reasoning(l) => tagged(l, "reasoning"),
            ChatRecord::ToolResult(l) => tagged(l, "tool_result"),
            ChatRecord::Other(v) => v,
        }
    }
}

impl Serialize for ChatRecord {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        Value::from(self.clone()).serialize(s)
    }
}

impl<'de> Deserialize<'de> for ChatRecord {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(ChatRecord::from(Value::deserialize(d)?))
    }
}

// ── codec: to_common ───────────────────────────────────────────────────

impl Codec for Grok {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            body_to_messages(&transcript.body, transcript.meta.timestamp),
        ))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            body_from_messages(&transcript.meta, &transcript.body),
        ))
    }
}

impl TextCodec for Grok {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let body: GrokSession = serde_json::from_str(text)?;
        let meta = meta_from_body(&body);
        Ok(Transcript::new(meta, body))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Ok(serde_json::to_string_pretty(&transcript.body)?)
    }
}

/// What the display log contributes to the conversation: timestamps (the chat
/// log carries none), user images (which live ONLY here — the model log gets
/// a textual description Grok generates), tool failure status, and per-turn
/// stop reasons.
#[derive(Default)]
struct DisplayIndex {
    prompts: Vec<PromptChunks>,
    thought_ts: Vec<DateTime<Utc>>,
    agent_ts: Vec<DateTime<Utc>>,
    call_ts: HashMap<String, DateTime<Utc>>,
    result_ts: HashMap<String, DateTime<Utc>>,
    failed: HashSet<String>,
    stop_reasons: Vec<StopReason>,
}

/// One prompt's `user_message_chunk`s, grouped by `_meta.promptIndex`: its
/// first timestamp and any attached images.
#[derive(Default)]
struct PromptChunks {
    ts: Option<DateTime<Utc>>,
    images: Vec<ImageSource>,
}

fn index_updates(updates: &[Value]) -> DisplayIndex {
    let mut idx = DisplayIndex::default();
    // Lines without an update payload or a timestamp index nothing.
    let dated = updates
        .iter()
        .filter_map(|line| line.pointer("/params/update").zip(update_ts(line)));
    for (update, ts) in dated {
        let call_id = || {
            update
                .get("toolCallId")
                .and_then(Value::as_str)
                .map(String::from)
        };
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("user_message_chunk") => {
                let content = update.get("content").unwrap_or(&Value::Null);
                let is_image = content.get("type").and_then(Value::as_str) == Some("image");
                // Chunks of one prompt share a promptIndex; without one, a
                // text chunk opens a new prompt and an image joins the last.
                let slot = update
                    .pointer("/_meta/promptIndex")
                    .and_then(Value::as_u64)
                    .and_then(|i| usize::try_from(i).ok())
                    .unwrap_or_else(|| {
                        let started = idx.prompts.len();
                        if is_image {
                            started.saturating_sub(1)
                        } else {
                            started
                        }
                    });
                if idx.prompts.len() <= slot {
                    idx.prompts.resize_with(slot + 1, PromptChunks::default);
                }
                let prompt = &mut idx.prompts[slot];
                prompt.ts.get_or_insert(ts);
                if is_image && let Some(source) = image_from_chunk(content) {
                    prompt.images.push(source);
                }
            }
            Some("agent_thought_chunk") => idx.thought_ts.push(ts),
            Some("agent_message_chunk") => idx.agent_ts.push(ts),
            Some("tool_call") => {
                if let Some(id) = call_id() {
                    idx.call_ts.entry(id).or_insert(ts);
                }
            }
            Some("tool_call_update") => {
                let status = update.get("status").and_then(Value::as_str);
                if let Some(id) = call_id() {
                    match status {
                        Some("completed") => {
                            idx.result_ts.insert(id, ts);
                        }
                        Some("failed") => {
                            idx.result_ts.insert(id.clone(), ts);
                            idx.failed.insert(id);
                        }
                        // Interim statuses (pending, in_progress) carry no
                        // result timestamp or failure verdict yet.
                        _ => {}
                    }
                }
            }
            Some("turn_completed") => {
                let reason = update
                    .get("stop_reason")
                    .and_then(Value::as_str)
                    .unwrap_or("end_turn");
                idx.stop_reasons.push(parse_stop_reason(reason));
            }
            // Other update kinds (plans, command lists, …) don't touch the
            // conversation.
            _ => {}
        }
    }
    idx
}

/// Streaming chunks are consecutive within one message; only the first chunk's
/// timestamp per logical message matters, but Grok writes whole messages as
/// single chunks, so consuming one per message is faithful.
fn update_ts(line: &Value) -> Option<DateTime<Utc>> {
    line.pointer("/params/_meta/agentTimestampMs")
        .and_then(Value::as_i64)
        .and_then(DateTime::from_timestamp_millis)
        .or_else(|| {
            line.get("timestamp")
                .and_then(Value::as_i64)
                .and_then(|s| DateTime::from_timestamp(s, 0))
        })
}

fn body_to_messages(body: &GrokSession, fallback_ts: DateTime<Utc>) -> Vec<Message> {
    let idx = index_updates(&body.updates);
    let mut prompts = idx.prompts.iter();
    let mut thought_ts = idx.thought_ts.iter();
    let mut agent_ts = idx.agent_ts.iter();

    let mut messages: Vec<Message> = Vec::new();
    // Per prompt turn: the index of its last assistant message, for stop-reason
    // backfill from `turn_completed`, and whether the *next* prompt flagged the
    // turn as interrupted.
    let mut turn_last_assistant: Vec<Option<usize>> = Vec::new();
    let mut interrupted_turns: HashSet<usize> = HashSet::new();
    let mut current_last_assistant: Option<usize> = None;
    let mut in_turn = false;

    for record in &body.chat_history {
        match record {
            // Grok's injected `<user_info>` preamble is context, not a prompt.
            ChatRecord::User(line) if is_user_info(&line.content) => {}
            ChatRecord::User(line) => {
                let mut content = parse_user_blocks(&line.content);
                // A prompt whose blocks parse to nothing carries no turn.
                if !content.is_empty() {
                    if in_turn {
                        turn_last_assistant.push(current_last_assistant.take());
                    }
                    if line.prior_turn_interrupt.is_some() && !turn_last_assistant.is_empty() {
                        interrupted_turns.insert(turn_last_assistant.len() - 1);
                    }
                    in_turn = true;
                    // The prompt's images live only in the display log.
                    let prompt = prompts.next();
                    let images = prompt.map(|p| p.images.as_slice()).unwrap_or_default();
                    content.extend(images.iter().cloned().map(|source| Block::Image { source }));
                    let ts = prompt.and_then(|p| p.ts).unwrap_or(fallback_ts);
                    messages.push(plain_message(Role::User, content, ts));
                }
            }
            ChatRecord::Assistant(line) => {
                let (content, first_call_id) = assistant_blocks(line);
                // A line with no text and no tool calls renders nothing.
                if !content.is_empty() {
                    // A text-bearing message streamed as an agent_message_chunk;
                    // a tool-only message's time is its first tool_call.
                    let timestamp =
                        if line.content.as_str().is_some_and(|t| !t.trim().is_empty()) {
                            agent_ts.next().copied()
                        } else {
                            first_call_id.and_then(|id| idx.call_ts.get(&id).copied())
                        }
                        .unwrap_or(fallback_ts);
                    messages.push(Message {
                        role: Role::Assistant,
                        content,
                        timestamp,
                        model: line.model_id.clone(),
                        stop_reason: None,
                        usage: None,
                    });
                    current_last_assistant = Some(messages.len() - 1);
                }
            }
            ChatRecord::Reasoning(line) => {
                // A reasoning line whose summary holds no text renders nothing.
                if let Some(text) = reasoning_summary_text(&line.summary) {
                    messages.push(plain_message(
                        Role::Assistant,
                        vec![Block::Thinking {
                            text,
                            signature: None,
                            encrypted: line.encrypted_content.clone(),
                        }],
                        thought_ts.next().copied().unwrap_or(fallback_ts),
                    ));
                }
            }
            ChatRecord::ToolResult(line) => {
                let content = match &line.content {
                    Value::String(s) => ToolOutput::Text(s.clone()),
                    Value::Null => ToolOutput::Text(String::new()),
                    other => ToolOutput::Json(other.clone()),
                };
                messages.push(plain_message(
                    Role::User,
                    vec![Block::ToolResult {
                        tool_use_id: line.tool_call_id.clone(),
                        content,
                        is_error: idx.failed.contains(&line.tool_call_id),
                    }],
                    idx.result_ts
                        .get(&line.tool_call_id)
                        .copied()
                        .unwrap_or(fallback_ts),
                ));
            }
            // The system prompt and unmodeled records carry no conversation.
            ChatRecord::System(_) | ChatRecord::Other(_) => {}
        }
    }
    if in_turn {
        turn_last_assistant.push(current_last_assistant.take());
    }
    backfill_stop_reasons(
        &mut messages,
        &turn_last_assistant,
        &interrupted_turns,
        &idx,
    );
    messages
}

/// `turn_completed` events arrive one per prompt turn, in order; apply each
/// turn's stop reason to its last assistant message. `prior_turn_interrupt`
/// flags are the fallback for turns whose `turn_completed` never landed.
fn backfill_stop_reasons(
    messages: &mut [Message],
    turn_last_assistant: &[Option<usize>],
    interrupted_turns: &HashSet<usize>,
    idx: &DisplayIndex,
) {
    // Turns that produced no assistant message have nothing to stamp.
    let stamped = turn_last_assistant
        .iter()
        .enumerate()
        .filter_map(|(turn, last)| last.map(|msg_idx| (turn, msg_idx)));
    for (turn, msg_idx) in stamped {
        if let Some(reason) = idx.stop_reasons.get(turn) {
            messages[msg_idx].stop_reason = Some(reason.clone());
        } else if interrupted_turns.contains(&turn) {
            messages[msg_idx].stop_reason = Some(StopReason::Aborted);
        }
    }
}

fn plain_message(role: Role, content: Vec<Block>, timestamp: DateTime<Utc>) -> Message {
    Message {
        role,
        content,
        timestamp,
        model: None,
        stop_reason: None,
        usage: None,
    }
}

/// The typed blocks of one assistant record, plus the first tool-call id
/// (used to date tool-only messages from the display log).
fn assistant_blocks(line: &AssistantLine) -> (Vec<Block>, Option<String>) {
    let mut content = Vec::new();
    if let Some(text) = line.content.as_str().filter(|t| !t.trim().is_empty()) {
        content.push(Block::Text {
            text: text.to_string(),
        });
    }
    let mut first_call_id = None;
    // A call missing its id cannot be paired with a result.
    let calls = line
        .tool_calls
        .as_ref()
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|call| call.get("id").and_then(Value::as_str).map(|id| (id, call)));
    for (id, call) in calls {
        first_call_id.get_or_insert_with(|| id.to_string());
        let name = call.get("name").and_then(Value::as_str).unwrap_or("tool");
        let input = parse_arguments(call.get("arguments"));
        content.push(Block::ToolUse {
            id: id.to_string(),
            tool: normalize_tool(name, input),
        });
    }
    (content, first_call_id)
}

fn is_user_info(content: &Value) -> bool {
    user_texts(content).any(|t| t.contains("<user_info>"))
}

fn user_texts(content: &Value) -> impl Iterator<Item = &str> {
    let out: Vec<&str> = match content {
        Value::String(s) => vec![s.as_str()],
        Value::Array(arr) => arr
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect(),
        // Other JSON shapes carry no user text.
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => Vec::new(),
    };
    out.into_iter()
}

fn parse_user_blocks(content: &Value) -> Vec<Block> {
    match content {
        Value::String(s) => text_block(s).into_iter().collect(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| match v.get("type").and_then(Value::as_str) {
                Some("text") => text_block(v.get("text").and_then(Value::as_str).unwrap_or("")),
                // Non-text chunk kinds (images live in the display log)
                // parse to nothing.
                _ => None,
            })
            .collect(),
        // Other JSON shapes hold no prompt blocks.
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => Vec::new(),
    }
}

fn text_block(raw: &str) -> Option<Block> {
    let text = strip_user_query(raw);
    (!text.trim().is_empty()).then_some(Block::Text { text })
}

/// An ACP image content block from a `user_message_chunk`:
/// `{"type": "image", "data": <base64>, "mimeType": …}`.
fn image_from_chunk(content: &Value) -> Option<ImageSource> {
    Some(ImageSource {
        source_type: "base64".to_string(),
        media_type: content.get("mimeType")?.as_str()?.to_string(),
        data: content.get("data")?.as_str()?.to_string(),
    })
}

fn strip_user_query(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(inner) = trimmed
        .strip_prefix("<user_query>")
        .and_then(|s| s.strip_suffix("</user_query>"))
    {
        inner.trim().to_string()
    } else {
        text.to_string()
    }
}

fn wrap_user_query(text: &str) -> String {
    if text.trim_start().starts_with("<user_query>") {
        text.to_string()
    } else {
        format!("<user_query>\n{text}\n</user_query>")
    }
}

fn reasoning_summary_text(summary: &Value) -> Option<String> {
    let parts: Vec<&str> = summary
        .as_array()?
        .iter()
        .filter_map(|e| {
            (e.get("type").and_then(Value::as_str) == Some("summary_text"))
                .then(|| e.get("text").and_then(Value::as_str))
                .flatten()
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// Tool-call `arguments` are JSON-in-a-string; keep the raw string when it
/// isn't valid JSON so nothing is dropped.
fn parse_arguments(arguments: Option<&Value>) -> Value {
    match arguments {
        Some(Value::String(raw)) => {
            serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.clone()))
        }
        Some(other) => other.clone(),
        None => Value::Object(Map::new()),
    }
}

fn parse_stop_reason(s: &str) -> StopReason {
    match s {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "cancelled" => StopReason::Aborted,
        "error" => StopReason::Error,
        other => StopReason::Other(other.to_string()),
    }
}

fn stop_reason_str(r: &StopReason) -> String {
    match r {
        StopReason::EndTurn => "end_turn".into(),
        StopReason::ToolUse => "tool_use".into(),
        StopReason::MaxTokens => "max_tokens".into(),
        StopReason::StopSequence => "stop_sequence".into(),
        StopReason::Aborted => "cancelled".into(),
        StopReason::Error => "error".into(),
        StopReason::Other(s) => s.clone(),
    }
}

// ── tool normalization (grok native ↔ canonical) ───────────────────────

/// Grok's toolset is Cursor's (`agent_name: "cursor"` in its own summary):
/// `Shell`/`StrReplace`/`Write`/`Read` with `path`-style keys. Map names and
/// keys onto the Claude convention, then let `Tool::from_canonical`'s
/// `deny_unknown_fields` fallback keep anything unexpected lossless.
fn normalize_tool(name: &str, input: Value) -> Tool {
    let canonical = match name {
        "Shell" => "Bash",
        "StrReplace" => "Edit",
        other => other,
    };
    Tool::from_canonical(canonical, normalize_args(canonical, input))
}

fn normalize_args(tool: &str, args: Value) -> Value {
    match args {
        Value::Object(obj) => Value::Object(
            obj.into_iter()
                .map(|(k, v)| match (tool, k.as_str()) {
                    ("Read" | "Write" | "Edit" | "MultiEdit", "path") => ("file_path".into(), v),
                    ("Write", "contents") => ("content".into(), v),
                    ("Bash", "block_until_ms") => match integral(&v) {
                        Some(ms) => ("timeout_ms".into(), Value::from(ms)),
                        None => (k, v),
                    },
                    ("Glob", "glob_pattern") => ("pattern".into(), v),
                    ("Glob", "target_directory") => ("path".into(), v),
                    _ => (k, v),
                })
                .collect(),
        ),
        // Non-object arguments have no keys to rename; pass through.
        other => other,
    }
}

/// The inverse of [`normalize_tool`], for `from_common`.
fn denormalize_tool(tool: &Tool) -> (String, Value) {
    let (name, input) = tool.to_canonical();
    let grok_name = match name.as_str() {
        "Bash" => "Shell",
        "Edit" => "StrReplace",
        other => other,
    }
    .to_string();
    (grok_name, denormalize_args(&name, input))
}

fn denormalize_args(tool: &str, input: Value) -> Value {
    match input {
        Value::Object(obj) => Value::Object(
            obj.into_iter()
                .map(|(k, v)| {
                    let key = match (tool, k.as_str()) {
                        ("Read" | "Write" | "Edit" | "MultiEdit", "file_path") => "path".into(),
                        ("Write", "content") => "contents".into(),
                        ("Bash", "timeout_ms") => "block_until_ms".into(),
                        ("Glob", "pattern") => "glob_pattern".into(),
                        ("Glob", "path") => "target_directory".into(),
                        _ => k,
                    };
                    (key, v)
                })
                .collect(),
        ),
        // Non-object input has no keys to rename; pass through.
        other => other,
    }
}

/// Grok writes `block_until_ms` as a float (`120000.0`); canonical
/// `timeout_ms` is integral. Only convert when nothing would be lost.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // guarded: integral, in range
fn integral(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| {
        v.as_f64()
            .filter(|f| f.fract() == 0.0 && (0.0..=9_007_199_254_740_992.0).contains(f))
            .map(|f| f as u64)
    })
}

// ── codec: from_common ─────────────────────────────────────────────────

fn body_from_messages(meta: &Meta, messages: &[Message]) -> GrokSession {
    let session_id = if meta.id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        meta.id.clone()
    };

    let mut chat: Vec<ChatRecord> = Vec::new();
    let mut updates = UpdateLog::new(&session_id, meta.model.as_deref());
    // Turn bookkeeping: a prompt turn opens at each text-bearing user message
    // and closes (with a turn_completed) just before the next one, or at EOF.
    let mut turn_open = false;
    let mut last_stop: Option<StopReason> = None;

    for (i, msg) in messages.iter().enumerate() {
        match msg.role {
            Role::User => {
                let mut prompt_texts: Vec<String> = Vec::new();
                let mut prompt_images: Vec<&ImageSource> = Vec::new();
                for block in &msg.content {
                    match block {
                        Block::Text { text } => prompt_texts.push(text.clone()),
                        // Images live in the display log only; the model log
                        // natively carries a Grok-generated description we
                        // cannot synthesize.
                        Block::Image { source } => prompt_images.push(source),
                        Block::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            chat.push(ChatRecord::ToolResult(ToolResultLine {
                                tool_call_id: tool_use_id.clone(),
                                // Grok's tool_result content is a plain
                                // string; structured outputs are flattened.
                                content: Value::String(tool_output_string(content)),
                                extra: Map::new(),
                            }));
                            updates.tool_result(msg.timestamp, tool_use_id, content, *is_error);
                        }
                        // Assistant-side blocks have no slot in a user record.
                        Block::Thinking { .. } | Block::ToolUse { .. } => {}
                    }
                }
                if !prompt_texts.is_empty() || !prompt_images.is_empty() {
                    if turn_open {
                        updates.turn_completed(msg.timestamp, last_stop.as_ref());
                    }
                    let interrupted = turn_open && matches!(last_stop, Some(StopReason::Aborted));
                    turn_open = true;
                    last_stop = None;
                    // An image-only prompt still needs a model-log record.
                    if prompt_texts.is_empty() {
                        prompt_texts.push("[image]".to_string());
                    }
                    let text = prompt_texts.join("\n\n");
                    chat.push(ChatRecord::User(UserLine {
                        content: json!([{"type": "text", "text": wrap_user_query(&text)}]),
                        prior_turn_interrupt: interrupted
                            .then(|| Value::String("mid_turn_abort".into())),
                        extra: Map::new(),
                    }));
                    updates.user_message(msg.timestamp, &text, &prompt_images);
                }
            }
            Role::Assistant => {
                if push_assistant(&mut chat, &mut updates, &session_id, i, msg) {
                    last_stop.clone_from(&msg.stop_reason);
                }
            }
        }
    }
    if turn_open {
        let final_ts = messages.last().map_or(meta.timestamp, |m| m.timestamp);
        updates.turn_completed(final_ts, last_stop.as_ref());
    }

    let updates = updates.finish();
    let summary = summary_value(meta, &session_id, chat.len(), updates.len());
    GrokSession {
        chat_history: chat,
        updates,
        events: Vec::new(),
        rewind_points: Vec::new(),
        summary: Some(summary),
        prompt_context: None,
        resources_state: None,
        signals: None,
        system_prompt: None,
    }
}

/// Emit one Common assistant message as native records: thinking blocks as
/// `reasoning` lines, text and tool calls as one `assistant` line, mirrored
/// into the display log. Returns whether an `assistant` line was written
/// (thinking-only messages don't end a turn).
fn push_assistant(
    chat: &mut Vec<ChatRecord>,
    updates: &mut UpdateLog,
    session_id: &str,
    msg_index: usize,
    msg: &Message,
) -> bool {
    let mut texts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut pending_calls: Vec<(String, String, Value)> = Vec::new();
    for (j, block) in msg.content.iter().enumerate() {
        match block {
            Block::Thinking {
                text, encrypted, ..
            } => {
                chat.push(ChatRecord::Reasoning(ReasoningLine {
                    id: Some(format!("rs_{}", grok_uuid(session_id, msg_index, j))),
                    summary: json!([{ "type": "summary_text", "text": text }]),
                    encrypted_content: encrypted.clone(),
                    status: Some("completed".into()),
                    extra: Map::new(),
                }));
                updates.thought(msg.timestamp, text);
            }
            Block::Text { text } => texts.push(text.clone()),
            Block::ToolUse { id, tool } => {
                let (name, input) = denormalize_tool(tool);
                let arguments = match &input {
                    Value::String(raw) => raw.clone(),
                    other => other.to_string(),
                };
                tool_calls.push(json!({
                    "id": id,
                    "name": name,
                    "arguments": arguments,
                }));
                pending_calls.push((id.clone(), name, input));
            }
            // User-side blocks have no slot in an assistant record.
            Block::Image { .. } | Block::ToolResult { .. } => {}
        }
    }
    if texts.is_empty() && tool_calls.is_empty() {
        // Thinking-only: the reasoning lines above are all there is to write.
        false
    } else {
        let text = texts.join("\n\n");
        chat.push(ChatRecord::Assistant(AssistantLine {
            content: Value::String(text.clone()),
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(Value::Array(tool_calls))
            },
            model_id: msg.model.clone(),
            model_fingerprint: None,
            extra: Map::new(),
        }));
        if !text.is_empty() {
            updates.agent_message(msg.timestamp, &text);
        }
        for (id, name, input) in pending_calls {
            updates.tool_call(msg.timestamp, &id, &name, &input);
        }
        true
    }
}

/// Builder for `updates.jsonl` — the display log `grok --resume` replays.
struct UpdateLog {
    session_id: String,
    model: Option<String>,
    lines: Vec<Value>,
    prompt_index: u64,
}

impl UpdateLog {
    fn new(session_id: &str, model: Option<&str>) -> Self {
        Self {
            session_id: session_id.to_string(),
            model: model.map(String::from),
            lines: Vec::new(),
            prompt_index: 0,
        }
    }

    fn push(&mut self, ts: DateTime<Utc>, method: &str, update: &Value) {
        let seq = self.lines.len();
        self.lines.push(json!({
            "timestamp": ts.timestamp(),
            "method": method,
            "params": {
                "sessionId": self.session_id,
                "update": update,
                "_meta": {
                    "eventId": format!("{}-{seq}", self.session_id),
                    "agentTimestampMs": ts.timestamp_millis(),
                },
            },
        }));
    }

    fn user_message(&mut self, ts: DateTime<Utc>, text: &str, images: &[&ImageSource]) {
        let meta = |log: &Self| {
            let mut meta = Map::new();
            if let Some(model) = &log.model {
                meta.insert("modelId".into(), Value::String(model.clone()));
            }
            meta.insert("promptIndex".into(), Value::from(log.prompt_index));
            meta
        };
        self.push(
            ts,
            "session/update",
            &json!({
                "sessionUpdate": "user_message_chunk",
                "content": { "type": "text", "text": text },
                "_meta": meta(self),
            }),
        );
        // Images are separate chunks of the same prompt (Grok's own layout);
        // this is their only home — the model log never holds image bytes.
        for image in images {
            self.push(
                ts,
                "session/update",
                &json!({
                    "sessionUpdate": "user_message_chunk",
                    "content": {
                        "type": "image",
                        "data": image.data,
                        "mimeType": image.media_type,
                    },
                    "_meta": meta(self),
                }),
            );
        }
    }

    fn agent_message(&mut self, ts: DateTime<Utc>, text: &str) {
        self.push(
            ts,
            "session/update",
            &json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": text },
            }),
        );
    }

    fn thought(&mut self, ts: DateTime<Utc>, text: &str) {
        self.push(
            ts,
            "session/update",
            &json!({
                "sessionUpdate": "agent_thought_chunk",
                "content": { "type": "text", "text": text },
            }),
        );
    }

    fn tool_call(&mut self, ts: DateTime<Utc>, id: &str, name: &str, input: &Value) {
        self.push(
            ts,
            "session/update",
            &json!({
                "sessionUpdate": "tool_call",
                "toolCallId": id,
                "title": tool_title(name, input),
                "kind": tool_kind(name),
                "rawInput": input,
            }),
        );
    }

    fn tool_result(&mut self, ts: DateTime<Utc>, id: &str, content: &ToolOutput, is_error: bool) {
        let text = tool_output_string(content);
        self.push(
            ts,
            "session/update",
            &json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": id,
                "status": if is_error { "failed" } else { "completed" },
                "content": [{ "type": "content", "content": { "type": "text", "text": text } }],
            }),
        );
    }

    fn turn_completed(&mut self, ts: DateTime<Utc>, stop: Option<&StopReason>) {
        let prompt_id = grok_prompt_uuid(&self.session_id, self.prompt_index);
        let reason = stop.map_or_else(|| "end_turn".to_string(), stop_reason_str);
        self.push(
            ts,
            "_x.ai/session/update",
            &json!({
                "sessionUpdate": "turn_completed",
                "prompt_id": prompt_id,
                "stop_reason": reason,
            }),
        );
        self.prompt_index += 1;
    }

    fn finish(self) -> Vec<Value> {
        self.lines
    }
}

/// Grok stores tool results as plain strings. Anthropic-style block arrays
/// flatten to their text; anything else structured becomes compact JSON.
fn tool_output_string(content: &ToolOutput) -> String {
    match content {
        ToolOutput::Text(s) => s.clone(),
        ToolOutput::Json(v) => {
            let block_texts: Option<Vec<&str>> = v.as_array().and_then(|arr| {
                arr.iter()
                    .map(|b| {
                        (b.get("type").and_then(Value::as_str) == Some("text"))
                            .then(|| b.get("text").and_then(Value::as_str))
                            .flatten()
                    })
                    .collect()
            });
            match block_texts {
                Some(texts) if !texts.is_empty() => texts.join("\n\n"),
                // Not a text-block array (or an empty one): compact JSON.
                Some(_) | None => v.to_string(),
            }
        }
    }
}

/// Display kind for a native tool name, mirroring Grok's own labels.
fn tool_kind(name: &str) -> &'static str {
    match name {
        "Read" => "read",
        "Write" | "StrReplace" | "MultiEdit" => "edit",
        "Shell" => "execute",
        "Glob" | "Grep" => "search",
        _ => "other",
    }
}

fn tool_title(name: &str, input: &Value) -> String {
    let get = |key: &str| input.get(key).and_then(Value::as_str);
    match name {
        "Read" => get("path").map(|p| format!("Read `{p}`")),
        "Write" | "StrReplace" => get("path").map(|p| format!("Edit `{p}`")),
        "Shell" => get("command").map(|c| format!("Execute `{c}`")),
        "Glob" => get("glob_pattern").map(|p| format!("Glob `{p}`")),
        "Grep" => get("pattern").map(String::from),
        _ => None,
    }
    .unwrap_or_else(|| name.to_string())
}

fn summary_value(meta: &Meta, session_id: &str, num_chat: usize, num_updates: usize) -> Value {
    let ts = meta.timestamp.to_rfc3339_opts(SecondsFormat::Micros, true);
    let title = meta.title.clone().unwrap_or_default();
    let mut summary = json!({
        "info": {
            "id": session_id,
            "cwd": meta.cwd.clone().unwrap_or_default(),
        },
        "session_summary": title,
        "generated_title": title,
        "created_at": ts,
        "updated_at": ts,
        "num_messages": num_updates,
        "num_chat_messages": num_chat,
        "current_model_id": meta.model.clone().unwrap_or_default(),
        "chat_format_version": 1,
    });
    if let Some(branch) = meta.git_branch.as_deref()
        && let Value::Object(obj) = &mut summary
    {
        obj.insert("head_branch".into(), Value::String(branch.into()));
    }
    summary
}

/// Deterministic ids, so `from_common` is a pure function of the transcript.
const NS: Uuid = Uuid::from_bytes([
    0x6b, 0x2e, 0x41, 0x7d, 0x35, 0x0a, 0x4f, 0x91, 0x8c, 0x27, 0xd4, 0x5b, 0x9e, 0x63, 0x18, 0x2f,
]);

fn grok_uuid(session_id: &str, i: usize, j: usize) -> String {
    Uuid::new_v5(&NS, format!("{session_id}:{i}:{j}").as_bytes()).to_string()
}

fn grok_prompt_uuid(session_id: &str, turn: u64) -> String {
    Uuid::new_v5(&NS, format!("{session_id}:prompt:{turn}").as_bytes()).to_string()
}

// ── metadata ───────────────────────────────────────────────────────────

fn meta_from_body(body: &GrokSession) -> Meta {
    let summary = body.summary.as_ref();
    let get = |key: &str| {
        summary
            .and_then(|s| s.get(key))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(String::from)
    };

    let id = summary
        .and_then(|s| s.pointer("/info/id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let cwd = summary
        .and_then(|s| s.pointer("/info/cwd"))
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(String::from)
        .or_else(|| workspace_from_chat(&body.chat_history));
    let timestamp = get("created_at")
        .and_then(|s| s.parse::<DateTime<Utc>>().ok())
        .or_else(|| body.updates.iter().find_map(update_ts))
        .unwrap_or_else(Utc::now);
    let model = get("current_model_id").or_else(|| {
        body.chat_history.iter().find_map(|r| match r {
            ChatRecord::Assistant(line) => line.model_id.clone(),
            // Only assistant lines carry a model id.
            ChatRecord::System(_)
            | ChatRecord::User(_)
            | ChatRecord::Reasoning(_)
            | ChatRecord::ToolResult(_)
            | ChatRecord::Other(_) => None,
        })
    });

    Meta {
        id,
        timestamp,
        cwd,
        git_branch: get("head_branch"),
        title: get("generated_title").or_else(|| get("session_summary")),
        cli_version: None,
        model,
    }
}

/// Grok injects a `<user_info>` preamble carrying the workspace path; mine it
/// when `summary.json` is absent.
fn workspace_from_chat(records: &[ChatRecord]) -> Option<String> {
    records.iter().find_map(|record| match record {
        ChatRecord::User(line) => user_texts(&line.content).find_map(|text| {
            text.lines()
                .find_map(|l| l.strip_prefix("Workspace Path: ").map(String::from))
        }),
        // Only user lines carry the injected preamble.
        ChatRecord::System(_)
        | ChatRecord::Assistant(_)
        | ChatRecord::Reasoning(_)
        | ChatRecord::ToolResult(_)
        | ChatRecord::Other(_) => None,
    })
}

// ── store ──────────────────────────────────────────────────────────────

/// Reads and writes Grok session directories under a sessions root (default
/// `~/.grok/sessions`, or `$GROK_HOME/sessions`).
#[derive(Debug, Clone)]
pub struct GrokStore {
    pub sessions_dir: PathBuf,
}

impl GrokStore {
    pub fn new(sessions_dir: impl Into<PathBuf>) -> Self {
        Self {
            sessions_dir: sessions_dir.into(),
        }
    }

    /// The default sessions root: `$GROK_HOME/sessions` when set, else
    /// `~/.grok/sessions`.
    #[must_use]
    pub fn default_root() -> Option<Self> {
        std::env::var_os("GROK_HOME")
            .filter(|v| !v.is_empty())
            .map(|home| Self::new(PathBuf::from(home).join("sessions")))
            .or_else(|| super::home_dir().map(|h| Self::new(h.join(".grok").join("sessions"))))
    }
}

impl GrokStore {
    /// Discovery metadata for one session directory. When `summary.json`
    /// alone answers everything [`meta_from_body`] would ask of it, the
    /// conversation logs are never read; otherwise the fallbacks need them,
    /// so it comes from a full load. A directory that neither path can read
    /// as a session discovers nothing.
    fn discover_meta(&self, dir: &Path) -> Option<Meta> {
        let summary: Option<Value> = fs::read_to_string(dir.join("summary.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok());
        if summary.as_ref().is_some_and(summary_answers_meta) {
            let mut meta = meta_from_body(&GrokSession {
                chat_history: Vec::new(),
                updates: Vec::new(),
                events: Vec::new(),
                rewind_points: Vec::new(),
                summary,
                prompt_context: None,
                resources_state: None,
                signals: None,
                system_prompt: None,
            });
            if meta.id.is_empty() {
                meta.id = jsonl::file_id(dir);
            }
            Some(meta)
        } else {
            self.load(&dir.to_path_buf()).ok().map(|t| t.meta)
        }
    }
}

/// Whether `summary.json` alone pins every [`meta_from_body`] field that has
/// a conversation-log fallback: timestamp (`created_at`), cwd (`info/cwd`),
/// and model (`current_model_id`). The remaining fields never look past the
/// summary, so a summary passing this check yields the same [`Meta`] as a
/// full load.
fn summary_answers_meta(summary: &Value) -> bool {
    summary
        .get("created_at")
        .and_then(Value::as_str)
        .is_some_and(|s| s.parse::<DateTime<Utc>>().is_ok())
        && summary
            .pointer("/info/cwd")
            .and_then(Value::as_str)
            .is_some_and(|cwd| !cwd.is_empty())
        && summary
            .get("current_model_id")
            .and_then(Value::as_str)
            .is_some_and(|model| !model.is_empty())
}

impl Store for GrokStore {
    type H = Grok;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        match fs::read_dir(&self.sessions_dir) {
            // No sessions root (Grok never ran here) means no sessions.
            Err(_) => Ok(Vec::new()),
            Ok(projects) => Ok(projects
                .flatten()
                .map(|project| project.path())
                // Stray files at the project level aren't project dirs.
                .filter(|project_dir| project_dir.is_dir())
                // An unreadable project dir contributes nothing.
                .filter_map(|project_dir| fs::read_dir(project_dir).ok())
                .flat_map(Iterator::flatten)
                .map(|session| session.path())
                // Sniff: a Grok session directory carries at least one of its
                // two conversation logs.
                .filter(|dir| {
                    dir.join("updates.jsonl").is_file() || dir.join("chat_history.jsonl").is_file()
                })
                .filter_map(|dir| {
                    let meta = self.discover_meta(&dir)?;
                    Some(Discovered {
                        meta,
                        reference: dir,
                    })
                })
                .collect()),
        }
    }

    fn load(&self, reference: &PathBuf) -> Result<Transcript<Grok>> {
        if reference.is_dir() {
            let text = |name: &str| fs::read_to_string(reference.join(name)).ok();
            let lines = |name: &str| -> Vec<Value> {
                text(name).map(|t| jsonl::parse(&t)).unwrap_or_default()
            };
            let value = |name: &str| -> Option<Value> {
                text(name).and_then(|t| serde_json::from_str(&t).ok())
            };

            let body = GrokSession {
                chat_history: text("chat_history.jsonl")
                    .map(|t| jsonl::parse(&t))
                    .unwrap_or_default(),
                updates: lines("updates.jsonl"),
                events: lines("events.jsonl"),
                rewind_points: lines("rewind_points.jsonl"),
                summary: value("summary.json"),
                prompt_context: value("prompt_context.json"),
                resources_state: value("resources_state.json"),
                signals: value("signals.json"),
                system_prompt: text("system_prompt.txt"),
            };
            let mut meta = meta_from_body(&body);
            if meta.id.is_empty() {
                meta.id = jsonl::file_id(reference);
            }
            Ok(Transcript::new(meta, body))
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no session directory at {}", reference.display()),
            )
            .into())
        }
    }

    fn save(&self, transcript: &Transcript<Grok>) -> Result<Saved<PathBuf>> {
        let meta = &transcript.meta;
        let id = if meta.id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            meta.id.clone()
        };
        let cwd = meta.cwd.clone().unwrap_or_default();
        let dir = self.sessions_dir.join(encode_cwd(&cwd)).join(&id);
        fs::create_dir_all(&dir)?;

        let body = &transcript.body;
        fs::write(
            dir.join("chat_history.jsonl"),
            jsonl::render(&body.chat_history)?,
        )?;
        fs::write(dir.join("updates.jsonl"), jsonl::render(&body.updates)?)?;
        if !body.events.is_empty() {
            fs::write(dir.join("events.jsonl"), jsonl::render(&body.events)?)?;
        }
        if !body.rewind_points.is_empty() {
            fs::write(
                dir.join("rewind_points.jsonl"),
                jsonl::render(&body.rewind_points)?,
            )?;
        }
        if let Some(summary) = &body.summary {
            fs::write(
                dir.join("summary.json"),
                serde_json::to_string_pretty(summary)?,
            )?;
        }
        if let Some(v) = &body.prompt_context {
            fs::write(dir.join("prompt_context.json"), serde_json::to_string(v)?)?;
        }
        if let Some(v) = &body.resources_state {
            fs::write(dir.join("resources_state.json"), serde_json::to_string(v)?)?;
        }
        if let Some(v) = &body.signals {
            fs::write(dir.join("signals.json"), serde_json::to_string(v)?)?;
        }
        if let Some(prompt) = &body.system_prompt {
            fs::write(dir.join("system_prompt.txt"), prompt)?;
        }

        Ok(Saved { id, reference: dir })
    }

    /// A grok session is a self-contained directory; delete removes it whole.
    fn delete(&self, reference: &PathBuf) -> Result<()> {
        Ok(fs::remove_dir_all(reference)?)
    }

    fn fingerprints(&self, refs: &[PathBuf]) -> Result<HashMap<String, String>> {
        let mut out = HashMap::with_capacity(refs.len());
        for dir in refs {
            let file = ["updates.jsonl", "chat_history.jsonl"]
                .iter()
                .map(|n| dir.join(n))
                .find(|p| p.is_file());
            let fingerprint = file.map(|p| file_fingerprint(&p)).unwrap_or_default();
            out.insert(dir.to_string_lossy().into_owned(), fingerprint);
        }
        Ok(out)
    }
}

/// Grok's project-dir encoding: URL percent-encoding of the workspace path
/// (RFC 3986 unreserved characters kept, everything else `%XX` per byte).
fn encode_cwd(cwd: &str) -> String {
    let mut out = String::with_capacity(cwd.len() * 3);
    for byte in cwd.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(char::from(HEX[usize::from(byte >> 4)]));
                out.push(char::from(HEX[usize::from(byte & 0x0F)]));
            }
        }
    }
    out
}

fn file_fingerprint(path: &Path) -> String {
    fs::metadata(path)
        .ok()
        .and_then(|m| {
            let len = m.len();
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| format!("{}:{len}", d.as_nanos()))
        })
        .unwrap_or_default()
}
