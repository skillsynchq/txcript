//! Amp (ampcode.com): one JSON document per thread at
//! `~/.local/share/amp/threads/<T-…>.json`.
//!
//! The native [`Body`](Thread) is Amp's thread document — the same shape the
//! local-first CLI wrote to disk and `amp threads export` emits today:
//! `{v, id, created, title, env, messages, …}`. Only `messages` is typed (as
//! raw JSON values); every other top-level key rides in `extra`, so the
//! document round-trips without loss across schema eras (the legacy
//! local-first files and the modern server-side export differ in bookkeeping
//! keys like `meta`, `protocolMessageID`, and `readAt`, and in tool names —
//! legacy `Bash{cmd,cwd}` vs modern `shell_command{command,workdir}` — all of
//! which this codec accepts).
//!
//! Amp already follows the Anthropic convention: assistant messages carry
//! `tool_use` blocks and the *user* messages that follow carry `tool_result`
//! blocks, so messages map 1:1 onto [`Common`]. Turn attribution lives on the
//! assistant message itself (`usage.model`, `usage.timestamp`, `state`).
//!
//! Current Amp CLI versions are server-authoritative: they neither read nor
//! write the local `threads/` directory (verified by bisection — a thread
//! file placed there is invisible to `amp threads list`/`export`). The store
//! therefore targets the legacy local-first location, which era-matched CLI
//! versions (`@sourcegraph/amp` ≤ early 2026) read and resume natively.
//! Bisecting against such a version: `created` is the one top-level field
//! its parser requires (`from_common` always writes it); `v`, `agentMode`,
//! `nextMessageId`, and `env` are optional but observed-shaped.
//!
//! Known representational losses through [`Common`] (the native body keeps
//! them; they do not survive `from_common`):
//! - `run.progress`, `run.trackFiles`, and per-run `~debug` on tool results;
//!   `cancelled` / `rejected-by-user` result statuses collapse to
//!   `is_error: true` text results.
//! - Message bookkeeping: `userState`, `fileMentions`, `interrupted`,
//!   `originalToolUseInput`, `turnElapsedMs`, `readAt`, `protocolMessageID`,
//!   and `usage.credits`.
//! - An assistant with no stop reason renders as `end_turn` (Amp requires a
//!   `state`); `StopReason::Aborted` maps to `state: {type: "cancelled"}`.
//! - `sourcePath` on image blocks and `signature`-less thinking providers.
//! - Shared-thread-era kinds never observed locally (`role: "parent"`
//!   messages, `manual_bash_invocation` blocks) pass through the native body
//!   unmodeled and carry nothing into [`Common`].

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::common::{Block, ImageSource, Message, Meta, Role, StopReason, Tool, ToolOutput, Usage};
use crate::error::{Error, Result};
use crate::harness::jsonl;
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

/// The Amp harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Amp;

impl Harness for Amp {
    const NAME: &'static str = "amp";
    type Body = Thread;
}

/// Amp's thread document. `messages` holds each message as raw JSON — the
/// codec interprets them, serde stays lossless — and every other top-level
/// key (`v`, `id`, `created`, `title`, `env`, `agentMode`, `~debug`, …)
/// passes through `extra` untouched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    #[serde(default)]
    pub messages: Vec<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Thread {
    /// The thread id (`T-…`) as recorded in the document.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.extra.get("id").and_then(Value::as_str)
    }
}

// ── codec: to_common ───────────────────────────────────────────────────

impl Codec for Amp {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        let mut last_ts = transcript.meta.timestamp;
        let messages = transcript
            .body
            .messages
            .iter()
            .filter_map(|m| match m.get("role").and_then(Value::as_str) {
                Some("user") => user_message(m, &mut last_ts),
                Some("assistant") => assistant_message(m, &mut last_ts),
                // Any other role (or a missing one) carries no conversational
                // turn; the native body keeps the record.
                _ => None,
            })
            .collect();
        Ok(Transcript::new(transcript.meta.clone(), messages))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            build_thread(&transcript.meta, &transcript.body),
        ))
    }
}

impl TextCodec for Amp {
    /// The text form is the thread JSON document — the bytes of a local
    /// thread file, byte-compatible with `amp threads export` output.
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let thread: Thread = serde_json::from_str(text)?;
        let meta = meta_from_thread(&thread);
        Ok(Transcript::new(meta, thread))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        // Amp writes its thread files pretty-printed; match that.
        Ok(serde_json::to_string_pretty(&transcript.body)?)
    }
}

fn user_message(m: &Value, last_ts: &mut DateTime<Utc>) -> Option<Message> {
    let ts = m
        .get("meta")
        .and_then(|meta| meta.get("sentAt"))
        .and_then(Value::as_i64)
        .and_then(DateTime::from_timestamp_millis);
    if let Some(ts) = ts {
        *last_ts = ts;
    }
    let content: Vec<Block> = content_values(m)
        .iter()
        .filter_map(|b| match b.get("type").and_then(Value::as_str) {
            Some("text") => text_block(b),
            Some("image") => image_block(b),
            Some("tool_result") => tool_result_block(b),
            // Unknown block kinds carry nothing into the canonical model;
            // the native message value still round-trips them.
            _ => None,
        })
        .collect();
    // A message whose blocks parse to nothing carries no turn.
    (!content.is_empty()).then_some(Message {
        role: Role::User,
        content,
        timestamp: *last_ts,
        model: None,
        stop_reason: None,
        usage: None,
    })
}

fn assistant_message(m: &Value, last_ts: &mut DateTime<Utc>) -> Option<Message> {
    let usage = m.get("usage");
    let ts = usage
        .and_then(|u| u.get("timestamp"))
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<DateTime<Utc>>().ok());
    if let Some(ts) = ts {
        *last_ts = ts;
    }
    let content: Vec<Block> = content_values(m)
        .iter()
        .filter_map(|b| match b.get("type").and_then(Value::as_str) {
            Some("text") => text_block(b),
            Some("thinking") => thinking_block(b),
            Some("tool_use") => tool_use_block(b),
            Some("image") => image_block(b),
            // Unknown block kinds carry nothing into the canonical model;
            // the native message value still round-trips them.
            _ => None,
        })
        .collect();
    // A message whose blocks parse to nothing carries no turn.
    let message = Message {
        role: Role::Assistant,
        content,
        timestamp: *last_ts,
        model: usage
            .and_then(|u| u.get("model"))
            .and_then(Value::as_str)
            .map(String::from),
        stop_reason: parse_state(m.get("state")),
        usage: usage.and_then(parse_usage),
    };
    (!message.content.is_empty()).then_some(message)
}

/// A message's `content` as a block list — borrowed straight from the
/// message in the common array case; a bare string becomes one synthetic
/// text block value.
fn content_values(m: &Value) -> std::borrow::Cow<'_, [Value]> {
    match m.get("content") {
        Some(Value::Array(arr)) => std::borrow::Cow::Borrowed(arr.as_slice()),
        Some(Value::String(s)) => std::borrow::Cow::Owned(vec![json!({"type": "text", "text": s})]),
        // Content is an array (or, defensively, a string) on the wire; any
        // other shape carries no blocks.
        _ => std::borrow::Cow::Borrowed(&[]),
    }
}

fn text_block(b: &Value) -> Option<Block> {
    b.get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(|text| Block::Text {
            text: text.to_string(),
        })
}

fn thinking_block(b: &Value) -> Option<Block> {
    b.get("thinking")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(|text| Block::Thinking {
            text: text.to_string(),
            signature: b.get("signature").and_then(Value::as_str).map(String::from),
            encrypted: None,
        })
}

fn image_block(b: &Value) -> Option<Block> {
    let source = b.get("source")?;
    Some(Block::Image {
        source: ImageSource {
            source_type: source
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("base64")
                .to_string(),
            media_type: source.get("mediaType")?.as_str()?.to_string(),
            data: source.get("data")?.as_str()?.to_string(),
        },
    })
}

fn tool_use_block(b: &Value) -> Option<Block> {
    let id = b.get("id")?.as_str()?.to_string();
    let name = b.get("name")?.as_str()?;
    let input = b.get("input").cloned().unwrap_or(Value::Object(Map::new()));
    let (canonical, input) = normalize_tool(name, input);
    Some(Block::ToolUse {
        id,
        tool: Tool::from_canonical(&canonical, input),
    })
}

fn tool_result_block(b: &Value) -> Option<Block> {
    let tool_use_id = b.get("toolUseID")?.as_str()?.to_string();
    let run = b.get("run")?;
    let reason_or = |fallback: &str| {
        run.get("reason")
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_string()
    };
    let (content, is_error) = match run.get("status").and_then(Value::as_str) {
        Some("done") => (
            value_to_output(run.get("result").cloned().unwrap_or_default()),
            false,
        ),
        Some("error") => (ToolOutput::Text(error_message(run.get("error"))), true),
        // Cancelled and rejected runs have no canonical status of their own;
        // both collapse to an error-flagged result (documented loss).
        Some("cancelled") => (ToolOutput::Text(reason_or("cancelled")), true),
        Some("rejected-by-user") => (ToolOutput::Text(reason_or("rejected by user")), true),
        // An unknown or missing status keeps the whole run payload.
        _ => (ToolOutput::Json(run.clone()), false),
    };
    Some(Block::ToolResult {
        tool_use_id,
        content,
        is_error,
    })
}

fn error_message(error: Option<&Value>) -> String {
    match error {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| v.to_string(), String::from),
        None => "Tool call failed".to_string(),
    }
}

fn parse_state(state: Option<&Value>) -> Option<StopReason> {
    let state = state?;
    match state.get("type").and_then(Value::as_str) {
        Some("complete") => state
            .get("stopReason")
            .and_then(Value::as_str)
            .map(parse_stop_reason),
        Some("cancelled") => Some(StopReason::Aborted),
        // "streaming" means the turn was still in flight when the thread was
        // captured — no stop reason yet. Unknown states carry none either.
        _ => None,
    }
}

fn parse_stop_reason(s: &str) -> StopReason {
    match s {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "error" => StopReason::Error,
        other => StopReason::Other(other.to_string()),
    }
}

fn parse_usage(u: &Value) -> Option<Usage> {
    let count = |key: &str| u.get(key).and_then(Value::as_u64);
    let input = count("inputTokens").unwrap_or(0);
    let output = count("outputTokens").unwrap_or(0);
    let cache_read = count("cacheReadInputTokens");
    let cache_creation = count("cacheCreationInputTokens");
    // An all-zero usage is what `from_common` writes when no usage was
    // recorded; parse it back to None so the absence round-trips.
    (input != 0
        || output != 0
        || cache_read.is_some_and(|n| n != 0)
        || cache_creation.is_some_and(|n| n != 0))
    .then_some(Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_input_tokens: cache_read,
        cache_creation_input_tokens: cache_creation,
    })
}

fn value_to_output(v: Value) -> ToolOutput {
    match v {
        Value::String(s) => ToolOutput::Text(s),
        other => ToolOutput::Json(other),
    }
}

fn output_to_value(out: &ToolOutput) -> Value {
    match out {
        ToolOutput::Text(s) => Value::String(s.clone()),
        ToolOutput::Json(v) => v.clone(),
    }
}

fn output_to_string(out: &ToolOutput) -> String {
    match out {
        ToolOutput::Text(s) => s.clone(),
        ToolOutput::Json(v) => v.to_string(),
    }
}

// ── codec: from_common ─────────────────────────────────────────────────

fn build_thread(meta: &Meta, messages: &[Message]) -> Thread {
    let thread_id = amp_thread_id(&meta.id);

    let values: Vec<Value> = messages
        .iter()
        .enumerate()
        .map(|(i, msg)| match msg.role {
            Role::User => user_value(msg, i),
            Role::Assistant => assistant_value(msg, i, meta),
        })
        .collect();

    let mut extra = Map::new();
    // `v` is Amp's revision counter; 1 is a plausible fresh-write value.
    extra.insert("v".into(), json!(1));
    extra.insert("id".into(), json!(thread_id));
    extra.insert("created".into(), json!(meta.timestamp.timestamp_millis()));
    if let Some(title) = meta.title.as_deref().filter(|t| !t.is_empty()) {
        extra.insert("title".into(), json!(title));
    }
    extra.insert("agentMode".into(), json!("smart"));
    extra.insert("nextMessageId".into(), json!(values.len()));
    if let Some(env) = build_env(meta) {
        extra.insert("env".into(), env);
    }

    Thread {
        messages: values,
        extra,
    }
}

/// The thread id Amp will accept. Amp validates ids against `T-` followed
/// by at least eight `[A-Za-z0-9-]` characters, so a foreign session id
/// (a Claude uuid, a codex rollout id) becomes a *deterministic* `T-` id —
/// `UUIDv5` over the original — and only an absent id mints a fresh random
/// one.
fn amp_thread_id(id: &str) -> String {
    const NS: Uuid = Uuid::from_bytes([
        0x61, 0x6d, 0x70, 0x2d, 0x74, 0x68, 0x72, 0x65, 0x61, 0x64, 0x2d, 0x69, 0x64, 0x2d, 0x6e,
        0x73,
    ]);
    let rest = id.strip_prefix("T-");
    let valid = rest.is_some_and(|rest| {
        rest.len() >= 8 && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    });
    if valid {
        id.to_string()
    } else if id.is_empty() {
        // The sole non-deterministic fallback: a brand-new id when the
        // transcript has none.
        format!("T-{}", Uuid::new_v4())
    } else {
        format!("T-{}", Uuid::new_v5(&NS, id.as_bytes()))
    }
}

/// Best-effort historical reconstruction of the `env.initial` header Amp
/// records at thread start — enough for `meta_from_thread` to recover the
/// cwd, branch, and CLI version.
fn build_env(meta: &Meta) -> Option<Value> {
    let mut initial = Map::new();
    if let Some(cwd) = meta.cwd.as_deref().filter(|c| !c.is_empty()) {
        let mut tree = Map::new();
        tree.insert("uri".into(), json!(path_to_file_uri(cwd)));
        let name = cwd.rsplit(['/', '\\']).next().unwrap_or(cwd);
        tree.insert("displayName".into(), json!(name));
        if let Some(branch) = meta.git_branch.as_deref().filter(|b| !b.is_empty()) {
            tree.insert(
                "repository".into(),
                json!({ "ref": format!("refs/heads/{branch}"), "type": "git" }),
            );
        }
        initial.insert("trees".into(), json!([tree]));
    }
    if let Some(version) = meta.cli_version.as_deref().filter(|v| !v.is_empty()) {
        initial.insert(
            "platform".into(),
            json!({ "client": "CLI", "clientType": "cli", "clientVersion": version }),
        );
    }
    (!initial.is_empty()).then(|| json!({ "initial": initial }))
}

fn user_value(msg: &Message, index: usize) -> Value {
    let content: Vec<Value> = msg
        .content
        .iter()
        .filter_map(|block| match block {
            Block::Text { text } => Some(json!({ "type": "text", "text": text })),
            Block::Image { source } => Some(json!({
                "type": "image",
                "source": {
                    "type": source.source_type,
                    "data": source.data,
                    "mediaType": source.media_type,
                },
            })),
            Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some(json!({
                "type": "tool_result",
                "toolUseID": tool_use_id,
                "run": run_value(content, *is_error),
            })),
            // Thinking and ToolUse never occur on user turns in the canonical
            // model; there is nothing of them to render.
            Block::Thinking { .. } | Block::ToolUse { .. } => None,
        })
        .collect();
    let mut m = Map::new();
    m.insert("role".into(), json!("user"));
    m.insert("messageId".into(), json!(index));
    m.insert("content".into(), Value::Array(content));
    // Amp stamps `sentAt` on messages the user actually sent; tool-result
    // carriers don't have one (their time is recovered from the preceding
    // assistant turn on read).
    let is_result_carrier = msg
        .content
        .iter()
        .any(|b| matches!(b, Block::ToolResult { .. }));
    if !is_result_carrier {
        m.insert(
            "meta".into(),
            json!({ "sentAt": msg.timestamp.timestamp_millis() }),
        );
    }
    Value::Object(m)
}

fn run_value(content: &ToolOutput, is_error: bool) -> Value {
    if is_error {
        json!({
            "status": "error",
            "error": { "message": output_to_string(content) },
        })
    } else {
        json!({ "status": "done", "result": output_to_value(content) })
    }
}

fn assistant_value(msg: &Message, index: usize, meta: &Meta) -> Value {
    let content: Vec<Value> = msg
        .content
        .iter()
        .filter_map(|block| match block {
            Block::Text { text } => Some(json!({ "type": "text", "text": text })),
            Block::Thinking {
                text, signature, ..
            } => {
                let mut b = Map::new();
                b.insert("type".into(), json!("thinking"));
                b.insert("thinking".into(), json!(text));
                if let Some(sig) = signature {
                    b.insert("signature".into(), json!(sig));
                    b.insert("provider".into(), json!("anthropic"));
                }
                Some(Value::Object(b))
            }
            Block::ToolUse { id, tool } => {
                let (canonical, input) = tool.to_canonical();
                let (name, input) = denormalize_tool(&canonical, input);
                Some(json!({
                    "type": "tool_use",
                    "complete": true,
                    "id": id,
                    "name": name,
                    "input": input,
                }))
            }
            Block::Image { source } => Some(json!({
                "type": "image",
                "source": {
                    "type": source.source_type,
                    "data": source.data,
                    "mediaType": source.media_type,
                },
            })),
            // A ToolResult never occurs on assistant turns in the canonical
            // model; there is nothing of it to render.
            Block::ToolResult { .. } => None,
        })
        .collect();

    let mut m = Map::new();
    m.insert("role".into(), json!("assistant"));
    m.insert("messageId".into(), json!(index));
    m.insert("content".into(), Value::Array(content));
    m.insert("state".into(), state_value(msg.stop_reason.as_ref()));
    m.insert("usage".into(), usage_value(msg, meta));
    Value::Object(m)
}

fn state_value(stop: Option<&StopReason>) -> Value {
    match stop {
        Some(StopReason::Aborted) => json!({ "type": "cancelled" }),
        Some(reason) => json!({ "type": "complete", "stopReason": stop_reason_str(reason) }),
        // Amp requires a state; a turn with no recorded stop reason renders
        // as a normal completion (documented loss).
        None => json!({ "type": "complete", "stopReason": "end_turn" }),
    }
}

fn stop_reason_str(r: &StopReason) -> String {
    match r {
        // Aborted is rendered as a cancelled state by `state_value`; if it
        // reaches here it degrades to a plain completion.
        StopReason::EndTurn | StopReason::Aborted => "end_turn".into(),
        StopReason::ToolUse => "tool_use".into(),
        StopReason::MaxTokens => "max_tokens".into(),
        StopReason::StopSequence => "stop_sequence".into(),
        StopReason::Error => "error".into(),
        StopReason::Other(s) => s.clone(),
    }
}

fn usage_value(msg: &Message, meta: &Meta) -> Value {
    let mut u = Map::new();
    if let Some(model) = msg.model.as_deref().or(meta.model.as_deref()) {
        u.insert("model".into(), json!(model));
    }
    u.insert(
        "timestamp".into(),
        json!(msg.timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)),
    );
    let usage = msg.usage.as_ref();
    u.insert(
        "inputTokens".into(),
        json!(usage.map_or(0, |u| u.input_tokens)),
    );
    u.insert(
        "outputTokens".into(),
        json!(usage.map_or(0, |u| u.output_tokens)),
    );
    // Cache counts only when recorded — a written 0 would read back as
    // `Some(0)` and a `None` cache wouldn't round-trip.
    let mut total = usage.map_or(0, |u| u.input_tokens);
    if let Some(read) = usage.and_then(|u| u.cache_read_input_tokens) {
        u.insert("cacheReadInputTokens".into(), json!(read));
        total += read;
    }
    if let Some(creation) = usage.and_then(|u| u.cache_creation_input_tokens) {
        u.insert("cacheCreationInputTokens".into(), json!(creation));
        total += creation;
    }
    // Required-but-derivable bookkeeping, reconstructed best-effort: the
    // context window Amp recorded for this model era, and the running total.
    u.insert("maxInputTokens".into(), json!(168_000));
    u.insert("totalInputTokens".into(), json!(total));
    Value::Object(u)
}

// ── tool normalization ─────────────────────────────────────────────────

/// Map an Amp tool name and input onto the Claude-canonical convention.
/// Handles both eras: legacy (`Bash{cmd,cwd}`, `Read{path,read_range}`) and
/// modern (`shell_command{command,workdir}`). Unknown tools pass through.
fn normalize_tool(name: &str, input: Value) -> (String, Value) {
    match name {
        "Bash" => (
            "Bash".to_string(),
            rename_keys(input, &[("cmd", "command"), ("cwd", "workdir")]),
        ),
        "shell_command" => ("Bash".to_string(), input),
        "Read" => (
            "Read".to_string(),
            read_range_to_offset(rename_keys(input, &[("path", "file_path")])),
        ),
        "edit_file" => (
            "Edit".to_string(),
            rename_keys(
                input,
                &[
                    ("path", "file_path"),
                    ("old_str", "old_string"),
                    ("new_str", "new_string"),
                ],
            ),
        ),
        "create_file" => (
            "Write".to_string(),
            rename_keys(input, &[("path", "file_path")]),
        ),
        "glob" => (
            "Glob".to_string(),
            rename_keys(input, &[("filePattern", "pattern")]),
        ),
        "todo_write" => ("TodoWrite".to_string(), input),
        "todo_read" => ("TodoRead".to_string(), input),
        "read_web_page" => (
            "WebFetch".to_string(),
            rename_keys(input, &[("objective", "prompt")]),
        ),
        // Grep already matches the canonical name and keys; web_search,
        // painter, finder, look_at, skill, … have no canonical counterpart
        // and pass through as themselves (landing in `Tool::Raw`).
        other => (other.to_string(), input),
    }
}

/// Exact inverse of [`normalize_tool`], emitting the legacy-era native names
/// the local-first CLI renders natively.
fn denormalize_tool(name: &str, input: Value) -> (String, Value) {
    match name {
        "Bash" => (
            "Bash".to_string(),
            rename_keys(input, &[("command", "cmd"), ("workdir", "cwd")]),
        ),
        "Read" => (
            "Read".to_string(),
            offset_to_read_range(rename_keys(input, &[("file_path", "path")])),
        ),
        "Edit" => (
            "edit_file".to_string(),
            rename_keys(
                input,
                &[
                    ("file_path", "path"),
                    ("old_string", "old_str"),
                    ("new_string", "new_str"),
                ],
            ),
        ),
        "Write" => (
            "create_file".to_string(),
            rename_keys(input, &[("file_path", "path")]),
        ),
        "Glob" => (
            "glob".to_string(),
            rename_keys(input, &[("pattern", "filePattern")]),
        ),
        "TodoWrite" => ("todo_write".to_string(), input),
        "TodoRead" => ("todo_read".to_string(), input),
        "WebFetch" => (
            "read_web_page".to_string(),
            rename_keys(input, &[("prompt", "objective")]),
        ),
        // Grep is shared verbatim; canonical names Amp has no spelling for
        // (MultiEdit, WebSearch, …) pass through and normalize back the same.
        other => (other.to_string(), input),
    }
}

/// `read_range: [first, last]` (1-based, inclusive) becomes canonical
/// `offset`/`limit`. Any other shape — or a range next to explicit
/// `offset`/`limit` keys — is left in place so the input falls through to
/// `Tool::Raw` losslessly.
fn read_range_to_offset(input: Value) -> Value {
    match input {
        Value::Object(mut obj) => {
            let range = obj.get("read_range").and_then(range_bounds);
            match range {
                Some((first, last))
                    if !obj.contains_key("offset") && !obj.contains_key("limit") =>
                {
                    obj.remove("read_range");
                    obj.insert("offset".into(), json!(first));
                    obj.insert("limit".into(), json!(last - first + 1));
                    Value::Object(obj)
                }
                // No well-formed range to convert; keep the input untouched.
                _ => Value::Object(obj),
            }
        }
        // A non-object input has no keys to convert.
        other => other,
    }
}

/// Inverse of [`read_range_to_offset`]: both `offset` and `limit` fold back
/// into `read_range`. A lone `offset` or `limit` has no exact range spelling
/// and stays as its canonical key (which `normalize_tool` passes back
/// unchanged, so the pair still round-trips).
fn offset_to_read_range(input: Value) -> Value {
    match input {
        Value::Object(mut obj) => {
            let offset = obj.get("offset").and_then(Value::as_u64);
            let limit = obj.get("limit").and_then(Value::as_u64);
            match (offset, limit) {
                (Some(first), Some(limit)) if first >= 1 && limit >= 1 => {
                    obj.remove("offset");
                    obj.remove("limit");
                    obj.insert("read_range".into(), json!([first, first + limit - 1]));
                    Value::Object(obj)
                }
                // Without both halves there is no exact range; keep the
                // canonical keys.
                _ => Value::Object(obj),
            }
        }
        // A non-object input has no keys to convert.
        other => other,
    }
}

fn range_bounds(range: &Value) -> Option<(u64, u64)> {
    match range.as_array().map(Vec::as_slice) {
        Some([first, last]) => match (first.as_u64(), last.as_u64()) {
            (Some(first), Some(last)) if first >= 1 && last >= first => Some((first, last)),
            // Bounds that aren't ordered 1-based integers aren't a range.
            _ => None,
        },
        // Anything but a two-element array isn't a range.
        _ => None,
    }
}

fn rename_keys(input: Value, renames: &[(&str, &str)]) -> Value {
    match input {
        Value::Object(mut obj) => {
            for (from, to) in renames {
                if let Some(value) = obj.remove(*from) {
                    obj.insert((*to).to_string(), value);
                }
            }
            Value::Object(obj)
        }
        // A non-object input has no keys to rename; pass it through untouched.
        other => other,
    }
}

// ── meta ───────────────────────────────────────────────────────────────

/// Session metadata from a thread document. `id` is left empty when the
/// document carries none; the [`Store`] fills it from the filename.
fn meta_from_thread(thread: &Thread) -> Meta {
    let earliest_sent = thread
        .messages
        .iter()
        .filter_map(|m| {
            m.get("meta")
                .and_then(|meta| meta.get("sentAt"))
                .and_then(Value::as_i64)
        })
        .min();
    let model = thread.messages.iter().find_map(|m| {
        m.get("usage")
            .and_then(|u| u.get("model"))
            .and_then(Value::as_str)
            .map(String::from)
    });
    meta_from_parts(MetaParts {
        id: thread.id().map(String::from),
        created: thread.extra.get("created").and_then(Value::as_i64),
        title: thread
            .extra
            .get("title")
            .and_then(Value::as_str)
            .map(String::from),
        env: thread.extra.get("env").cloned(),
        earliest_sent,
        model,
    })
}

/// The raw ingredients of a [`Meta`], shared by the full parse and the
/// store's shallow discovery scan.
struct MetaParts {
    id: Option<String>,
    created: Option<i64>,
    title: Option<String>,
    env: Option<Value>,
    earliest_sent: Option<i64>,
    model: Option<String>,
}

fn meta_from_parts(parts: MetaParts) -> Meta {
    let tree = parts
        .env
        .as_ref()
        .and_then(|env| env.get("initial"))
        .and_then(|initial| initial.get("trees"))
        .and_then(|trees| trees.get(0));
    let cwd = tree
        .and_then(|t| t.get("uri"))
        .and_then(Value::as_str)
        .and_then(file_uri_to_path);
    let git_branch = tree
        .and_then(|t| t.get("repository"))
        .and_then(|r| r.get("ref"))
        .and_then(Value::as_str)
        .map(|r| r.strip_prefix("refs/heads/").unwrap_or(r).to_string());
    let cli_version = parts
        .env
        .as_ref()
        .and_then(|env| env.get("initial"))
        .and_then(|initial| initial.get("platform"))
        .and_then(|p| p.get("clientVersion"))
        .and_then(Value::as_str)
        .map(String::from);
    // Threads with no assistant turn still record the model as an
    // `env.initial.tags` entry of the form `model:<id>`.
    let tag_model = parts
        .env
        .as_ref()
        .and_then(|env| env.get("initial"))
        .and_then(|initial| initial.get("tags"))
        .and_then(Value::as_array)
        .and_then(|tags| {
            tags.iter()
                .filter_map(Value::as_str)
                .find_map(|t| t.strip_prefix("model:"))
        })
        .map(String::from);
    Meta {
        id: parts.id.unwrap_or_default(),
        timestamp: parts
            .created
            .or(parts.earliest_sent)
            .and_then(DateTime::from_timestamp_millis)
            .unwrap_or_else(Utc::now),
        cwd,
        git_branch,
        title: parts.title.filter(|t| !t.trim().is_empty()),
        cli_version,
        model: parts.model.or(tag_model),
    }
}

// ── file: URI helpers ──────────────────────────────────────────────────

/// `file:///a%20b` → `/a b`. Anything that isn't a `file://` URI is not a
/// local path.
fn file_uri_to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Some(byte) = hex_pair(bytes[i + 1], bytes[i + 2]) {
                    out.push(byte);
                    i += 3;
                } else {
                    // A stray `%` that isn't an escape passes through verbatim.
                    out.push(b'%');
                    i += 1;
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    let decoded = String::from_utf8_lossy(&out).into_owned();
    // A drive-letter path came from Windows (`file:///C%3A/Users/x` →
    // `/C:/Users/x`): drop the URI's leading slash and restore backslashes.
    let bytes = decoded.as_bytes();
    if bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && bytes.get(3).is_none_or(|b| *b == b'/')
    {
        return Some(decoded[1..].replace('/', "\\"));
    }
    Some(decoded)
}

/// `/a b` → `file:///a%20b`, percent-encoding everything outside the RFC 3986
/// unreserved set (plus `/`). A Windows path is normalized into URI shape
/// first (`C:\Users\x` → `file:///C%3A/Users/x`).
fn path_to_file_uri(path: &str) -> String {
    let bytes = path.as_bytes();
    let windows = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let normalized;
    let path = if windows {
        normalized = format!("/{}", path.replace('\\', "/"));
        normalized.as_str()
    } else {
        path
    };
    let mut out = String::with_capacity(path.len() + 8);
    out.push_str("file://");
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(char::from(byte));
            }
            other => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(char::from(HEX[usize::from(other >> 4)]));
                out.push(char::from(HEX[usize::from(other & 0x0f)]));
            }
        }
    }
    out
}

fn hex_pair(hi: u8, lo: u8) -> Option<u8> {
    let digit = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    };
    Some(digit(hi)? * 16 + digit(lo)?)
}

// ── store ──────────────────────────────────────────────────────────────

/// Reads and writes Amp threads as JSON documents in a flat directory
/// (default `~/.local/share/amp/threads`).
#[derive(Debug, Clone)]
pub struct AmpStore {
    pub threads_dir: PathBuf,
}

impl AmpStore {
    pub fn new(threads_dir: impl Into<PathBuf>) -> Self {
        Self {
            threads_dir: threads_dir.into(),
        }
    }

    /// The default threads directory, honoring `AMP_THREADS_DIR` /
    /// `XDG_DATA_HOME`, falling back to `~/.local/share/amp/threads`.
    #[must_use]
    pub fn default_root() -> Option<Self> {
        std::env::var_os("AMP_THREADS_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                // amp's own platform switch consults XDG_DATA_HOME only off
                // Windows; there the data dir is always `<home>\.local\share\amp`.
                if cfg!(windows) {
                    None
                } else {
                    std::env::var_os("XDG_DATA_HOME")
                        .filter(|v| !v.is_empty())
                        .map(|xdg| PathBuf::from(xdg).join("amp").join("threads"))
                }
            })
            .or_else(|| {
                super::home_dir().map(|home| {
                    home.join(".local")
                        .join("share")
                        .join("amp")
                        .join("threads")
                })
            })
            .map(Self::new)
    }
}

impl Store for AmpStore {
    type H = Amp;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        if self.threads_dir.is_dir() {
            // An unreadable directory lists nothing.
            let entries = fs::read_dir(&self.threads_dir).into_iter().flatten();
            Ok(entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|e| e == "json"))
                .filter_map(|path| {
                    // A file that fails to read or isn't a thread document is
                    // skipped, not fatal.
                    let text = fs::read_to_string(&path).ok()?;
                    let mut meta = scan_meta(&text)?;
                    if meta.id.is_empty() {
                        meta.id = jsonl::file_id(&path);
                    }
                    Some(Discovered {
                        meta,
                        reference: path,
                    })
                })
                .collect())
        } else {
            // A missing root means no threads, not an error.
            Ok(Vec::new())
        }
    }

    fn load(&self, reference: &PathBuf) -> Result<Transcript<Amp>> {
        let mut transcript = Amp::from_text(&fs::read_to_string(reference)?)?;
        if transcript.meta.id.is_empty() {
            transcript.meta.id = jsonl::file_id(reference);
        }
        Ok(transcript)
    }

    fn save(&self, transcript: &Transcript<Amp>) -> Result<Saved<PathBuf>> {
        // The document's own id wins: it is the one `from_common` shaped to
        // Amp's `T-…` rules, and the one the CLI resumes by. A foreign
        // `meta.id` (a Claude uuid) is not a valid Amp thread id.
        let id = transcript
            .body
            .id()
            .filter(|id| !id.is_empty())
            .or_else(|| Some(transcript.meta.id.as_str()).filter(|id| !id.is_empty()))
            .map(String::from)
            .ok_or_else(|| Error::Unconvertible {
                harness: Amp::NAME,
                detail: "thread has no id to name its file".to_string(),
            })?;
        fs::create_dir_all(&self.threads_dir)?;
        let path = self.threads_dir.join(format!("{id}.json"));
        fs::write(&path, Amp::to_text(transcript)?)?;
        Ok(Saved {
            id,
            reference: path,
        })
    }

    fn delete(&self, reference: &PathBuf) -> Result<()> {
        Ok(fs::remove_file(reference)?)
    }

    fn fingerprints(&self, refs: &[PathBuf]) -> Result<HashMap<String, String>> {
        Ok(refs
            .iter()
            .map(|path| (path.to_string_lossy().into_owned(), file_fingerprint(path)))
            .collect())
    }
}

// ── discover: shallow meta scan ────────────────────────────────────────

/// Shallow mirror of [`Thread`] for discovery: top-level metadata is typed,
/// message payloads are skipped rather than built. `v` doubles as the format
/// sniff — every Amp thread document carries it.
#[derive(Deserialize)]
struct ScanThread {
    v: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    created: Option<i64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    env: Option<Value>,
    #[serde(default)]
    messages: Vec<ScanMessage>,
}

/// Only the attribution keys; block content is never built.
#[derive(Deserialize)]
struct ScanMessage {
    #[serde(default)]
    meta: Option<ScanSent>,
    #[serde(default)]
    usage: Option<ScanUsage>,
}

#[derive(Deserialize)]
struct ScanSent {
    #[serde(rename = "sentAt", default)]
    sent_at: Option<i64>,
}

#[derive(Deserialize)]
struct ScanUsage {
    #[serde(default)]
    model: Option<String>,
}

/// [`meta_from_thread`] over a shallow scan of the raw text — equivalent to
/// `from_text(text)?.meta`, but discovery never builds message content.
/// `None` when the text isn't an Amp thread document.
fn scan_meta(text: &str) -> Option<Meta> {
    let scan: ScanThread = serde_json::from_str(text).ok()?;
    // The `v` revision counter marks the format; a JSON file without it is
    // some other artifact that happens to live in the directory.
    scan.v.as_ref()?;
    let earliest_sent = scan
        .messages
        .iter()
        .filter_map(|m| m.meta.as_ref().and_then(|meta| meta.sent_at))
        .min();
    let model = scan
        .messages
        .iter()
        .find_map(|m| m.usage.as_ref().and_then(|u| u.model.clone()));
    Some(meta_from_parts(MetaParts {
        id: scan.id,
        created: scan.created,
        title: scan.title,
        env: scan.env,
        earliest_sent,
        model,
    }))
}

fn file_fingerprint(path: &Path) -> String {
    match fs::metadata(path) {
        // An unreadable file has no fingerprint.
        Err(_) => String::new(),
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{mtime}:{}", meta.len())
        }
    }
}
