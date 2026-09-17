//! fx (Vercel's terminal coding agent): `~/.fx/sessions/<session-id>/`.
//!
//! An fx session is a *directory* of files built around an append-only event
//! log. The files that matter here:
//!
//! - `events.jsonl` — the event log, one JSON object per line with a typed
//!   envelope (`schema_version`, `log_generation`, `seq`, `event_id`,
//!   `timestamp_ms`, `kind`, `payload`). Only `history_turn_committed` events
//!   carry the conversation; `session_started` is the required header, and
//!   `recovery_checkpoint_set` / `usage_checkpointed` are bookkeeping. fx's
//!   loader is strict: it rejects unknown event kinds and unknown keys at
//!   every level, and a tool result's `output` must be a string.
//! - `session.json` — the derived header (id, workspace, preferences, and the
//!   byte offsets into `events.jsonl` that pin the current generation).
//! - `authority.json` — session identity (`authority_id`, `storage_format`).
//! - `commit.<log_generation>.json` + an empty `commit.lock` — the commit
//!   boundary. Both are required or fx reports the session unavailable.
//! - `display.json` — title/preview for `fx sessions`.
//! - `usage-v2.json`, `checkpoint.json`, `images/*.bin` — carried when present.
//!
//! A `history_turn_committed` turn holds `user` (prompt text + images),
//! `execution.tool_steps` (each an intermediate assistant message with its
//! tool calls and results), and `assistant` (the concluding text). An
//! `interrupted` turn instead carries a single in-flight `tool_call` and a
//! `terminal_reason`.
//!
//! `to_common` reads the conversation from the `history_turn_committed`
//! events; `from_common` regenerates `events.jsonl` (header + turns) and every
//! sidecar fx validates on resume, recomputing the byte offsets.
//!
//! fx's committed-turn schema has no slot for model reasoning and rejects
//! unknown keys, so `Thinking` blocks are preserved out-of-band in a private
//! `txcript-meta.json` sidecar that fx ignores: same-harness round trips keep
//! reasoning, while fx resume simply renders the conversation without it.
//!
//! Known representational losses through `Common`: per-turn token usage and
//! per-message model (fx stores one session model), `replace_all` on edits
//! (fx's `edit_file` has no such flag), `terminal` action/profile and other
//! non-Bash tool argument shapes when a session leaves fx, and the recovery
//! checkpoint state. `execution.files` (fx's changed-file panel) is emitted
//! empty; the conversation itself is intact.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::common::{Block, ImageSource, Message, Meta, Role, StopReason, Tool, ToolOutput};
use crate::error::{Error, Result};
use crate::harness::jsonl;
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

/// The fx harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fx;

impl Harness for Fx {
    const NAME: &'static str = "fx";
    type Body = FxSession;
}

// ── native records ─────────────────────────────────────────────────────

/// Faithful in-memory representation of one fx session directory.
///
/// The event log stays raw JSON (`Vec<Value>`) so native ↔ disk is lossless
/// without modeling fx's internals; only `history_turn_committed` events are
/// interpreted by the codec, and everything else passes through untouched.
/// The `reasoning` sidecar is txcript-private (fx never writes or reads it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FxSession {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<FxImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
}

/// One snapshot image under `images/`. `path` is its directory-relative name
/// (e.g. `images/image-1-<hex>.bin`); the bytes are hex in the text form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FxImage {
    pub path: String,
    #[serde(with = "serde_hex")]
    pub data: Vec<u8>,
}

impl Codec for Fx {
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

impl TextCodec for Fx {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let body: FxSession = serde_json::from_str(text)?;
        let meta = meta_from_body(&body);
        Ok(Transcript::new(meta, body))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Ok(serde_json::to_string_pretty(&transcript.body)?)
    }
}

// ── codec: to_common ───────────────────────────────────────────────────

fn body_to_messages(body: &FxSession, fallback_ts: DateTime<Utc>) -> Vec<Message> {
    let reasoning = index_reasoning(body.reasoning.as_ref());
    let images: HashMap<&str, &FxImage> =
        body.images.iter().map(|i| (i.path.as_str(), i)).collect();
    let model = session_model(body);

    let mut messages: Vec<Message> = Vec::new();
    let mut turn_idx: u64 = 0;
    for event in &body.events {
        if event.get("kind").and_then(Value::as_str) != Some("history_turn_committed") {
            continue;
        }
        let Some(turn) = event.pointer("/payload/turn") else {
            continue;
        };
        let ts = event
            .get("timestamp_ms")
            .and_then(Value::as_i64)
            .and_then(DateTime::from_timestamp_millis)
            .unwrap_or(fallback_ts);
        emit_turn(
            turn,
            ts,
            turn_idx,
            model.as_deref(),
            &reasoning,
            &images,
            &mut messages,
        );
        turn_idx += 1;
    }
    messages
}

/// Emit one committed (or interrupted) turn as `Common` messages. Reasoning
/// blocks for the turn's `n`-th assistant message are keyed `(turn_idx, n)` in
/// the private sidecar and spliced back in the same order `from_common` wrote
/// them.
fn emit_turn(
    turn: &Value,
    ts: DateTime<Utc>,
    turn_idx: u64,
    model: Option<&str>,
    reasoning: &HashMap<(u64, u64), Vec<Block>>,
    images: &HashMap<&str, &FxImage>,
    out: &mut Vec<Message>,
) {
    // The user prompt opens the turn.
    let user_content = user_blocks(turn.get("user"), images);
    if !user_content.is_empty() {
        out.push(plain(Role::User, user_content, ts));
    }

    let mut aidx: u64 = 0;
    let mut push_assistant =
        |content: Vec<Block>, stop: Option<StopReason>, out: &mut Vec<Message>| {
            let mut blocks = reasoning
                .get(&(turn_idx, aidx))
                .cloned()
                .unwrap_or_default();
            blocks.extend(content);
            aidx += 1;
            if blocks.is_empty() {
                return;
            }
            out.push(Message {
                role: Role::Assistant,
                content: blocks,
                timestamp: ts,
                model: model.map(String::from),
                stop_reason: stop,
                usage: None,
            });
        };

    if turn.get("kind").and_then(Value::as_str) == Some("interrupted") {
        let mut content = Vec::new();
        if let Some(text) = nonempty_str(turn.get("assistant")) {
            content.push(Block::Text { text });
        }
        if let Some(call) = turn.get("tool_call")
            && let Some(block) = tool_use_block(call)
        {
            content.push(block);
        }
        push_assistant(content, Some(StopReason::Aborted), out);
        return;
    }

    // Committed turn: intermediate steps, then the concluding text.
    if let Some(steps) = turn
        .pointer("/execution/tool_steps")
        .and_then(Value::as_array)
    {
        for step in steps {
            let mut content = Vec::new();
            if let Some(text) = nonempty_str(step.get("assistant")) {
                content.push(Block::Text { text });
            }
            if let Some(calls) = step.get("tool_calls").and_then(Value::as_array) {
                content.extend(calls.iter().filter_map(tool_use_block));
            }
            push_assistant(content, None, out);

            if let Some(results) = step.get("tool_results").and_then(Value::as_array) {
                let blocks: Vec<Block> = results.iter().filter_map(tool_result_block).collect();
                if !blocks.is_empty() {
                    out.push(plain(Role::User, blocks, ts));
                }
            }
        }
    }

    let final_text = nonempty_str(turn.get("assistant"));
    let final_content: Vec<Block> = final_text
        .map(|text| Block::Text { text })
        .into_iter()
        .collect();
    push_assistant(final_content, Some(StopReason::EndTurn), out);
}

fn user_blocks(user: Option<&Value>, images: &HashMap<&str, &FxImage>) -> Vec<Block> {
    let mut blocks = Vec::new();
    let Some(user) = user else {
        return blocks;
    };
    if let Some(text) = nonempty_str(user.get("text")) {
        blocks.push(Block::Text { text });
    }
    if let Some(entries) = user.get("images").and_then(Value::as_array) {
        for entry in entries {
            if let Some(source) = image_source(entry, images) {
                blocks.push(Block::Image { source });
            }
        }
    }
    blocks
}

/// Resolve a committed-turn image entry to inline base64, pulling the bytes
/// from the carried `images/` snapshot it names.
fn image_source(entry: &Value, images: &HashMap<&str, &FxImage>) -> Option<ImageSource> {
    let snapshot = entry.get("snapshot_path").and_then(Value::as_str)?;
    let bytes = &images.get(snapshot)?.data;
    Some(ImageSource {
        source_type: "base64".to_string(),
        media_type: entry
            .get("media_type")
            .and_then(Value::as_str)
            .unwrap_or("application/octet-stream")
            .to_string(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

fn tool_use_block(call: &Value) -> Option<Block> {
    let id = call.get("id").and_then(Value::as_str)?;
    let name = call.get("name").and_then(Value::as_str).unwrap_or("tool");
    let input = parse_arguments(call.get("arguments_json"));
    Some(Block::ToolUse {
        id: id.to_string(),
        tool: normalize_tool(name, input),
    })
}

fn tool_result_block(result: &Value) -> Option<Block> {
    let tool_use_id = result.get("tool_call_id").and_then(Value::as_str)?;
    let content = match result.get("output") {
        Some(Value::String(s)) => ToolOutput::Text(s.clone()),
        Some(Value::Null) | None => ToolOutput::Text(String::new()),
        Some(other) => ToolOutput::Json(other.clone()),
    };
    Some(Block::ToolResult {
        tool_use_id: tool_use_id.to_string(),
        content,
        is_error: result.get("status").and_then(Value::as_str) == Some("failure"),
    })
}

fn plain(role: Role, content: Vec<Block>, timestamp: DateTime<Utc>) -> Message {
    Message {
        role,
        content,
        timestamp,
        model: None,
        stop_reason: None,
        usage: None,
    }
}

fn nonempty_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(String::from)
}

/// Tool-call `arguments_json` is JSON-in-a-string; keep the raw string when it
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

fn session_model(body: &FxSession) -> Option<String> {
    body.session
        .as_ref()
        .and_then(|s| s.pointer("/preferences/model"))
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .map(String::from)
        .or_else(|| {
            body.events.iter().find_map(|e| {
                e.pointer("/payload/preferences/model")
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty())
                    .map(String::from)
            })
        })
}

fn index_reasoning(v: Option<&Value>) -> HashMap<(u64, u64), Vec<Block>> {
    let mut out = HashMap::new();
    let Some(entries) = v.and_then(|v| v.get("entries")).and_then(Value::as_array) else {
        return out;
    };
    for entry in entries {
        let (Some(t), Some(a)) = (
            entry.get("t").and_then(Value::as_u64),
            entry.get("a").and_then(Value::as_u64),
        ) else {
            continue;
        };
        let blocks: Vec<Block> = entry
            .get("blocks")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(reasoning_block).collect())
            .unwrap_or_default();
        out.insert((t, a), blocks);
    }
    out
}

fn reasoning_block(v: &Value) -> Option<Block> {
    Some(Block::Thinking {
        text: v.get("text").and_then(Value::as_str)?.to_string(),
        signature: v.get("signature").and_then(Value::as_str).map(String::from),
        encrypted: v.get("encrypted").and_then(Value::as_str).map(String::from),
    })
}

// ── tool normalization (fx native ↔ canonical) ─────────────────────────

/// fx's file/shell tools onto the Claude convention. Names and argument keys
/// are mapped, then `Tool::from_canonical`'s `deny_unknown_fields` fallback
/// keeps anything unexpected lossless. Non-file/shell tools (`grep_files`,
/// `vision`, `mcp_*`, …) pass through as `Tool::Raw` under their native name.
fn normalize_tool(name: &str, input: Value) -> Tool {
    match name {
        "read_file" => Tool::from_canonical("Read", rename_keys(input, &[("path", "file_path")])),
        "write_file" => Tool::from_canonical("Write", rename_keys(input, &[("path", "file_path")])),
        "edit_file" => Tool::from_canonical("Edit", rename_keys(input, &[("path", "file_path")])),
        "terminal" => Tool::from_canonical("Bash", terminal_to_bash(input)),
        other => Tool::from_canonical(other, input),
    }
}

/// Inverse of [`normalize_tool`]. Returns the native tool name and the input
/// value to serialize into `arguments_json`.
fn denormalize_tool(tool: &Tool) -> (String, Value) {
    let (name, input) = tool.to_canonical();
    match name.as_str() {
        "Read" => (
            "read_file".into(),
            rename_keys(input, &[("file_path", "path")]),
        ),
        "Write" => (
            "write_file".into(),
            rename_keys(input, &[("file_path", "path")]),
        ),
        "Edit" => (
            "edit_file".into(),
            drop_keys(
                rename_keys(input, &[("file_path", "path")]),
                &["replace_all"],
            ),
        ),
        "Bash" => ("terminal".into(), bash_to_terminal(&input)),
        _ => (name, input),
    }
}

/// fx `terminal` args → canonical `Bash`: keep the command, map `cwd` to
/// `workdir`, and drop fx-only keys (`action`, `profile`) so the typed `Bash`
/// variant is used rather than falling to `Raw`.
fn terminal_to_bash(input: Value) -> Value {
    let Value::Object(obj) = input else {
        return input;
    };
    let mut out = Map::new();
    for (k, v) in obj {
        match k.as_str() {
            "command" => {
                out.insert("command".into(), v);
            }
            "cwd" => {
                if v.as_str().is_some_and(|s| !s.is_empty()) {
                    out.insert("workdir".into(), v);
                }
            }
            // fx-specific shell keys have no canonical Bash slot.
            "action" | "profile" => {}
            _ => {
                out.insert(k, v);
            }
        }
    }
    Value::Object(out)
}

/// Inverse of [`terminal_to_bash`]: canonical `Bash` → fx `terminal`, synthesizing
/// the `action`/`profile`/`cwd` fx expects. Bash-only extras with no fx slot
/// (`timeout_ms`, `description`, `run_in_background`) are dropped.
fn bash_to_terminal(input: &Value) -> Value {
    let command = input
        .get("command")
        .cloned()
        .unwrap_or_else(|| Value::String(String::new()));
    let cwd = input
        .get("workdir")
        .and_then(Value::as_str)
        .unwrap_or_default();
    json!({
        "action": "exec",
        "command": command,
        "cwd": cwd,
        "profile": "user",
    })
}

fn rename_keys(input: Value, renames: &[(&str, &str)]) -> Value {
    match input {
        Value::Object(obj) => Value::Object(
            obj.into_iter()
                .map(|(k, v)| {
                    let key = renames
                        .iter()
                        .find(|(from, _)| *from == k)
                        .map_or(k, |(_, to)| (*to).to_string());
                    (key, v)
                })
                .collect(),
        ),
        other => other,
    }
}

fn drop_keys(input: Value, keys: &[&str]) -> Value {
    match input {
        Value::Object(obj) => Value::Object(
            obj.into_iter()
                .filter(|(k, _)| !keys.contains(&k.as_str()))
                .collect(),
        ),
        other => other,
    }
}

// ── codec: from_common ─────────────────────────────────────────────────

fn body_from_messages(meta: &Meta, messages: &[Message]) -> FxSession {
    let session_id = if meta.id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        meta.id.clone()
    };
    let created_ms = meta.timestamp.timestamp_millis();
    let generation = derived_hex(&session_id, "gen");
    let authority_id = derived_hex(&session_id, "authority");

    let mut builder = TurnBuilder {
        images: Vec::new(),
        reasoning: Vec::new(),
        tool_names: HashMap::new(),
    };
    let mut turn_payloads: Vec<(i64, Value)> = Vec::new();
    let mut i = 0;
    let mut turn_idx: u64 = 0;
    while i < messages.len() {
        // A turn opens at a prompt user message (text/image); any leading
        // non-prompt messages form a turn with no explicit prompt.
        let prompt = if is_prompt(&messages[i]) {
            let p = Some(&messages[i]);
            i += 1;
            p
        } else {
            None
        };
        let body_start = i;
        while i < messages.len() && !is_prompt(&messages[i]) {
            i += 1;
        }
        let body = &messages[body_start..i];
        let ts = prompt
            .or_else(|| body.first())
            .map_or(meta.timestamp, |m| m.timestamp);
        let payload = builder.build_turn(turn_idx, prompt, body);
        turn_payloads.push((ts.timestamp_millis(), payload));
        turn_idx += 1;
    }

    // Assemble the event log: the session header, then one committed event
    // per turn. Byte offsets are computed from the rendered lines.
    let mut events: Vec<Value> = Vec::new();
    events.push(session_started(&session_id, &generation, created_ms, meta));
    let mut last_ts = created_ms;
    for (idx, (ts_ms, turn)) in turn_payloads.into_iter().enumerate() {
        last_ts = ts_ms;
        let seq = u64::try_from(idx).unwrap_or(0) + 2;
        events.push(json!({
            "schema_version": 1,
            "log_generation": generation,
            "seq": seq,
            "event_id": derived_hex(&session_id, &format!("event:{seq}")),
            "timestamp_ms": ts_ms,
            "kind": "history_turn_committed",
            "payload": {
                "conversation_language": "und",
                "total_input_tokens": 0,
                "total_output_tokens": 0,
                "turn": turn,
            },
        }));
    }

    assemble_body(
        meta,
        &session_id,
        &generation,
        &authority_id,
        created_ms,
        last_ts,
        events,
        builder,
    )
}

/// Wrap a finished event log with the derived header and sidecars fx validates
/// on resume, recomputing the byte offsets `session.json`/`commit.json` pin.
#[allow(clippy::too_many_arguments)]
fn assemble_body(
    meta: &Meta,
    session_id: &str,
    generation: &str,
    authority_id: &str,
    created_ms: i64,
    last_ts: i64,
    events: Vec<Value>,
    builder: TurnBuilder,
) -> FxSession {
    let last_seq = u64::try_from(events.len()).unwrap_or(1);
    let event_log_bytes = total_bytes(&events);
    let base_bytes = events.first().map_or(0, line_bytes);
    let last_event_id = events
        .last()
        .and_then(|e| e.get("event_id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let session = json!({
        "schema_version": 3,
        "storage_format": "event_log_v1",
        "id": session_id,
        "authority_id": authority_id,
        "log_generation": generation,
        "created_at_ms": created_ms,
        "updated_at_ms": last_ts,
        "origin_workspace_root": meta.cwd.clone().unwrap_or_default(),
        "workspace_root": meta.cwd.clone().unwrap_or_default(),
        "conversation_language": "und",
        "history_len": events.len().saturating_sub(1),
        "total_input_tokens": 0,
        "total_output_tokens": 0,
        "last_event_seq": last_seq,
        "event_log_bytes": event_log_bytes,
        "generation_base_seq": 1,
        "generation_base_bytes": base_bytes,
        "preferences": preferences(meta),
    });
    let authority = json!({
        "schema_version": 1,
        "session_id": session_id,
        "authority_id": authority_id,
        "storage_format": "event_log_v1",
        "source": "native_create",
    });
    let commit = json!({
        "schema_version": 1,
        "session_id": session_id,
        "log_generation": generation,
        "through_seq": last_seq,
        "through_event_id": last_event_id,
        "through_event_log_bytes": event_log_bytes,
    });
    let title = meta.title.clone().unwrap_or_default();
    let display = json!({
        "schema_version": 1,
        "title": title,
        "preview": title,
        "origin_workspace_root": meta.cwd.clone().unwrap_or_default(),
    });
    let reasoning =
        (!builder.reasoning.is_empty()).then(|| json!({ "entries": builder.reasoning }));

    FxSession {
        events,
        session: Some(session),
        authority: Some(authority),
        commit: Some(commit),
        display: Some(display),
        usage: None,
        checkpoint: None,
        images: builder.images,
        reasoning,
    }
}

/// Groups `Common` messages back into fx turns, collecting the reasoning
/// sidecar entries and image snapshots as it goes.
struct TurnBuilder {
    images: Vec<FxImage>,
    reasoning: Vec<Value>,
    /// `tool_use_id` → native tool name, so a result carries the same
    /// `tool_name` its call did (fx pairs them when rebuilding the model
    /// history — a mismatch is rejected as invalid gateway history).
    tool_names: HashMap<String, String>,
}

impl TurnBuilder {
    fn build_turn(&mut self, turn_idx: u64, prompt: Option<&Message>, body: &[Message]) -> Value {
        let user = self.user_value(prompt);

        // An aborted turn whose only assistant content is the in-flight call
        // maps to fx's `interrupted` turn shape.
        if let [only] = body
            && only.role == Role::Assistant
            && only.stop_reason == Some(StopReason::Aborted)
            && only
                .content
                .iter()
                .any(|b| matches!(b, Block::ToolUse { .. }))
        {
            return self.interrupted_turn(turn_idx, &user, only);
        }

        let mut steps: Vec<Value> = Vec::new();
        let mut assistant_final = String::new();
        let mut aidx: u64 = 0;
        let mut j = 0;
        while j < body.len() {
            let msg = &body[j];
            match msg.role {
                Role::Assistant => {
                    self.record_reasoning(turn_idx, aidx, msg);
                    let text = joined_text(msg);
                    let calls = self.tool_calls(msg);
                    let is_last = j + 1 >= body.len();
                    if calls.is_empty() {
                        if is_last {
                            assistant_final = text;
                        } else {
                            steps.push(json!({
                                "assistant": text_or_null(&text),
                                "tool_calls": [],
                                "tool_results": [],
                            }));
                        }
                    } else {
                        // Results ride on the following tool-result message.
                        let mut results = Vec::new();
                        if let Some(next) = body.get(j + 1)
                            && next.role == Role::User
                            && has_tool_result(next)
                        {
                            results = self.tool_results(next);
                            j += 1;
                        }
                        steps.push(json!({
                            "assistant": text_or_null(&text),
                            "tool_calls": calls,
                            "tool_results": results,
                        }));
                    }
                    aidx += 1;
                }
                Role::User => {
                    // Orphan tool results (no preceding call this turn) attach
                    // to the last step rather than being dropped.
                    if has_tool_result(msg)
                        && let Some(Value::Object(step)) = steps.last_mut()
                        && let Some(Value::Array(existing)) = step.get_mut("tool_results")
                    {
                        existing.extend(self.tool_results(msg));
                    }
                }
            }
            j += 1;
        }

        json!({
            "kind": "assistant",
            "user": user,
            "assistant": assistant_final,
            "execution": {
                "schema_version": 3,
                "tool_steps": steps,
                "files": [],
            },
        })
    }

    fn interrupted_turn(&mut self, turn_idx: u64, user: &Value, msg: &Message) -> Value {
        self.record_reasoning(turn_idx, 0, msg);
        let text = joined_text(msg);
        let call = msg.content.iter().find_map(|b| match b {
            Block::ToolUse { id, tool } => Some(self.tool_call_value(id, tool)),
            _ => None,
        });
        json!({
            "kind": "interrupted",
            "user": user,
            "assistant": text_or_null(&text),
            "tool_call": call.unwrap_or(Value::Null),
            "completed_tool_names": [],
            "terminal_reason": "cancelled",
        })
    }

    fn user_value(&mut self, prompt: Option<&Message>) -> Value {
        let mut text_parts: Vec<String> = Vec::new();
        let mut images: Vec<Value> = Vec::new();
        if let Some(msg) = prompt {
            for block in &msg.content {
                match block {
                    Block::Text { text } => text_parts.push(text.clone()),
                    Block::Artifact { artifact } => text_parts.push(artifact.display_text()),
                    Block::ToolUse { tool, .. } => {
                        if let Some(cmd) = tool.command_display() {
                            text_parts.push(cmd);
                        }
                    }
                    Block::Image { source } => {
                        if let Some(entry) = self.push_image(images.len(), source) {
                            images.push(entry);
                        }
                    }
                    _ => {}
                }
            }
        }
        json!({ "text": text_parts.join("\n\n"), "images": images })
    }

    /// Materialize a `Common` image into an `images/` snapshot and return the
    /// committed-turn entry that references it.
    fn push_image(&mut self, index: usize, source: &ImageSource) -> Option<Value> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(source.data.as_bytes())
            .ok()?;
        let digest = sha256_hex(&bytes);
        let id = index + 1;
        let path = format!("images/image-{id}-{}.bin", &digest[..16]);
        self.images.push(FxImage {
            path: path.clone(),
            data: bytes,
        });
        Some(json!({
            "id": id,
            "path": path,
            "media_type": source.media_type,
            "snapshot_path": path,
            "snapshot_sha256": digest,
        }))
    }

    fn record_reasoning(&mut self, turn_idx: u64, aidx: u64, msg: &Message) {
        let blocks: Vec<Value> = msg
            .content
            .iter()
            .filter_map(|b| match b {
                Block::Thinking {
                    text,
                    signature,
                    encrypted,
                } => {
                    let mut entry = json!({ "text": text });
                    if let (Value::Object(map), Some(sig)) = (&mut entry, signature) {
                        map.insert("signature".into(), Value::String(sig.clone()));
                    }
                    if let (Value::Object(map), Some(enc)) = (&mut entry, encrypted) {
                        map.insert("encrypted".into(), Value::String(enc.clone()));
                    }
                    Some(entry)
                }
                _ => None,
            })
            .collect();
        if !blocks.is_empty() {
            self.reasoning
                .push(json!({ "t": turn_idx, "a": aidx, "blocks": blocks }));
        }
    }
}

impl TurnBuilder {
    fn tool_calls(&mut self, msg: &Message) -> Vec<Value> {
        msg.content
            .iter()
            .filter_map(|b| match b {
                Block::ToolUse { id, tool } => Some(self.tool_call_value(id, tool)),
                _ => None,
            })
            .collect()
    }

    fn tool_call_value(&mut self, id: &str, tool: &Tool) -> Value {
        let (name, input) = denormalize_tool(tool);
        self.tool_names.insert(id.to_string(), name.clone());
        json!({
            "id": id,
            "name": name,
            "arguments_json": serde_json::to_string(&input).unwrap_or_else(|_| "{}".into()),
            "provider_result": Value::Null,
        })
    }

    fn tool_results(&self, msg: &Message) -> Vec<Value> {
        msg.content
            .iter()
            .filter_map(|b| match b {
                Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let output = tool_output_string(content);
                    let bytes = output.len();
                    let tool_name = self
                        .tool_names
                        .get(tool_use_id)
                        .cloned()
                        .unwrap_or_else(|| "tool".to_string());
                    Some(json!({
                            "tool_call_id": tool_use_id,
                            "tool_name": tool_name,
                            "status": if *is_error { "failure" } else { "success" },
                        "output": output,
                        "output_handle": Value::Null,
                        "preview": Value::Null,
                        "output_bytes": bytes,
                        "stored_output_bytes": bytes,
                        "truncated": false,
                        "provider_native": false,
                        "created_at_ms": 0,
                        "permission_feedback": [],
                        "committed_file_presentation": Value::Null,
                        "command_output_replay": Value::Null,
                        "command_process_presentation": Value::Null,
                    }))
                }
                _ => None,
            })
            .collect()
    }
}

/// fx stores tool output as a plain string. Anthropic-style block arrays
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
                Some(_) | None => v.to_string(),
            }
        }
    }
}

fn is_prompt(msg: &Message) -> bool {
    msg.role == Role::User
        && msg.content.iter().any(|b| match b {
            Block::Text { .. } | Block::Image { .. } | Block::Artifact { .. } => true,
            Block::ToolUse { tool, .. } => tool.command_display().is_some(),
            _ => false,
        })
}

fn has_tool_result(msg: &Message) -> bool {
    msg.content
        .iter()
        .any(|b| matches!(b, Block::ToolResult { .. }))
}

fn joined_text(msg: &Message) -> String {
    let parts: Vec<String> = msg
        .content
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } => Some(text.clone()),
            Block::Artifact { artifact } => Some(artifact.display_text()),
            _ => None,
        })
        .collect();
    parts.join("\n\n")
}

fn text_or_null(text: &str) -> Value {
    if text.is_empty() {
        Value::Null
    } else {
        Value::String(text.to_string())
    }
}

fn session_started(session_id: &str, generation: &str, created_ms: i64, meta: &Meta) -> Value {
    json!({
        "schema_version": 1,
        "log_generation": generation,
        "seq": 1,
        "event_id": derived_hex(session_id, "event:1"),
        "timestamp_ms": created_ms,
        "kind": "session_started",
        "payload": {
            "id": session_id,
            "created_at_ms": created_ms,
            "origin_workspace_root": meta.cwd.clone().unwrap_or_default(),
            "workspace_root": meta.cwd.clone().unwrap_or_default(),
            "conversation_language": "und",
            "preferences": preferences(meta),
            "usage": zero_usage(),
        },
    })
}

fn preferences(meta: &Meta) -> Value {
    json!({
        "model": meta.model.clone().unwrap_or_default(),
        "effort": "auto",
        "fast_mode": false,
        "provider": "gateway",
    })
}

fn zero_usage() -> Value {
    json!({
        "billing": "complete",
        "api_duration_complete": true,
        "wall_duration_complete": true,
        "code_complete": true,
        "next_sequence": 1,
        "settled_through_sequence": 0,
        "api_duration_ms": 0,
        "wall_duration_ms": 0,
        "total_cost": 0,
        "input_tokens": 0,
        "output_tokens": 0,
        "cache_read_tokens": 0,
        "cache_write_tokens": 0,
        "billable_web_search_calls": 0,
        "lines_added": 0,
        "lines_removed": 0,
        "models": [],
        "pending": [],
    })
}

fn line_bytes(v: &Value) -> usize {
    serde_json::to_string(v).map_or(0, |s| s.len() + 1)
}

fn total_bytes(events: &[Value]) -> usize {
    events.iter().map(line_bytes).sum()
}

/// Deterministic 32-hex identifiers (fx's own shape) derived from the session
/// id, so `from_common` is a pure function of the transcript.
const NS: Uuid = Uuid::from_bytes([
    0x0f, 0x27, 0x3b, 0x8a, 0x14, 0x62, 0x4d, 0x91, 0xb2, 0x0e, 0x71, 0x6a, 0x3d, 0x9d, 0x18, 0x2f,
]);

fn derived_hex(session_id: &str, key: &str) -> String {
    Uuid::new_v5(&NS, format!("{session_id}:{key}").as_bytes())
        .simple()
        .to_string()
}

// ── metadata ───────────────────────────────────────────────────────────

fn meta_from_body(body: &FxSession) -> Meta {
    let header = body.session.as_ref();
    let started = body
        .events
        .iter()
        .find(|e| e.get("kind").and_then(Value::as_str) == Some("session_started"))
        .map(|e| e.get("payload").unwrap_or(e));

    let get_str = |ptr: &str| {
        header
            .and_then(|h| h.pointer(ptr))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(String::from)
    };

    let id = get_str("/id").unwrap_or_default();
    let created_ms = header
        .and_then(|h| h.get("created_at_ms"))
        .and_then(Value::as_i64)
        .or_else(|| {
            started
                .and_then(|p| p.get("created_at_ms"))
                .and_then(Value::as_i64)
        });
    let timestamp = created_ms
        .and_then(DateTime::from_timestamp_millis)
        .unwrap_or_else(Utc::now);

    let cwd = get_str("/workspace_root")
        .or_else(|| get_str("/origin_workspace_root"))
        .or_else(|| {
            started
                .and_then(|p| p.get("workspace_root"))
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(String::from)
        });

    let title = body
        .display
        .as_ref()
        .and_then(|d| d.get("title"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .map(String::from)
        .or_else(|| first_prompt_title(&body.events));

    let model = session_model(body);

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

/// The first user prompt, truncated, as a title fallback when `display.json`
/// carries none.
fn first_prompt_title(events: &[Value]) -> Option<String> {
    let text = events
        .iter()
        .filter(|e| e.get("kind").and_then(Value::as_str) == Some("history_turn_committed"))
        .find_map(|e| {
            e.pointer("/payload/turn/user/text")
                .and_then(Value::as_str)
                .filter(|t| !t.trim().is_empty())
        })?;
    let trimmed = text.trim();
    Some(
        trimmed
            .chars()
            .take(60)
            .collect::<String>()
            .trim()
            .to_string(),
    )
}

// ── store ──────────────────────────────────────────────────────────────

/// Reads and writes fx session directories under a sessions root (default
/// `~/.fx/sessions`, or `$FX_HOME/sessions`).
#[derive(Debug, Clone)]
pub struct FxStore {
    pub sessions_dir: PathBuf,
}

impl FxStore {
    pub fn new(sessions_dir: impl Into<PathBuf>) -> Self {
        Self {
            sessions_dir: sessions_dir.into(),
        }
    }

    /// The default sessions root: `$FX_HOME/sessions` when set, else
    /// `~/.fx/sessions`.
    #[must_use]
    pub fn default_root() -> Option<Self> {
        std::env::var_os("FX_HOME")
            .filter(|v| !v.is_empty())
            .map(|home| Self::new(PathBuf::from(home).join("sessions")))
            .or_else(|| super::home_dir().map(|h| Self::new(h.join(".fx").join("sessions"))))
    }
}

/// Parse one fx session directory into its native body and metadata. All file
/// reads are tolerant: a missing sidecar yields `None`, never an error.
fn read_session(dir: &Path) -> Transcript<Fx> {
    let text = |name: &str| fs::read_to_string(dir.join(name)).ok();
    let value =
        |name: &str| -> Option<Value> { text(name).and_then(|t| serde_json::from_str(&t).ok()) };
    let commit = fs::read_dir(dir).ok().and_then(|entries| {
        entries.flatten().find_map(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            (name.starts_with("commit.") && name.ends_with(".json"))
                .then(|| fs::read_to_string(e.path()).ok())
                .flatten()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        })
    });

    let body = FxSession {
        events: text("events.jsonl")
            .map(|t| jsonl::parse(&t))
            .unwrap_or_default(),
        session: value("session.json"),
        authority: value("authority.json"),
        commit,
        display: value("display.json"),
        usage: value("usage-v2.json"),
        checkpoint: value("checkpoint.json"),
        images: load_images(dir),
        reasoning: value("txcript-meta.json"),
    };
    let mut meta = meta_from_body(&body);
    if meta.id.is_empty() {
        meta.id = jsonl::file_id(dir);
    }
    Transcript::new(meta, body)
}

/// Read every `images/*.bin` snapshot, sorted by name for a deterministic body.
fn load_images(dir: &Path) -> Vec<FxImage> {
    let images_dir = dir.join("images");
    let mut out: Vec<FxImage> = match fs::read_dir(&images_dir) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| {
                let data = fs::read(e.path()).ok()?;
                let name = e.file_name().to_string_lossy().into_owned();
                Some(FxImage {
                    path: format!("images/{name}"),
                    data,
                })
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Whether an event log begins with fx's `session_started` header — the sniff
/// that identifies a session directory.
fn is_fx_session(dir: &Path) -> bool {
    let Ok(text) = fs::read_to_string(dir.join("events.jsonl")) else {
        return false;
    };
    text.lines()
        .find(|l| !l.trim().is_empty())
        .and_then(|l| serde_json::from_str::<Value>(l).ok())
        .and_then(|v| v.get("kind").and_then(Value::as_str).map(String::from))
        .as_deref()
        == Some("session_started")
}

impl Store for FxStore {
    type H = Fx;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        let Ok(entries) = fs::read_dir(&self.sessions_dir) else {
            // No sessions root (fx never ran here) means no sessions.
            return Ok(Vec::new());
        };
        Ok(entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .filter(|dir| is_fx_session(dir))
            .map(|dir| {
                let transcript = read_session(&dir);
                Discovered {
                    meta: transcript.meta,
                    reference: dir,
                }
            })
            .collect())
    }

    fn load(&self, reference: &PathBuf) -> Result<Transcript<Fx>> {
        if reference.is_dir() {
            Ok(read_session(reference))
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no session directory at {}", reference.display()),
            )
            .into())
        }
    }

    fn save(&self, transcript: &Transcript<Fx>) -> Result<Saved<PathBuf>> {
        let meta = &transcript.meta;
        let id = if meta.id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            meta.id.clone()
        };
        super::checked_id_component(Fx::NAME, &id)?;
        let dir = self.sessions_dir.join(&id);
        fs::create_dir_all(&dir)?;
        // fx refuses to load a session whose durable storage is world- or
        // group-accessible ("private permissions unsupported"), so every file
        // and directory must be owner-only.
        restrict_dir(&dir);

        let body = &transcript.body;
        let write_file = |name: &str, contents: &[u8]| -> Result<()> {
            let path = dir.join(name);
            fs::write(&path, contents)?;
            restrict_file(&path);
            Ok(())
        };
        write_file("events.jsonl", jsonl::render(&body.events)?.as_bytes())?;

        let write_value = |name: &str, v: &Option<Value>| -> Result<()> {
            if let Some(v) = v {
                write_file(name, serde_json::to_string(v)?.as_bytes())?;
            }
            Ok(())
        };
        write_value("session.json", &body.session)?;
        write_value("authority.json", &body.authority)?;
        write_value("display.json", &body.display)?;
        write_value("usage-v2.json", &body.usage)?;
        write_value("checkpoint.json", &body.checkpoint)?;
        write_value("txcript-meta.json", &body.reasoning)?;

        if let Some(commit) = &body.commit {
            let generation = commit
                .get("log_generation")
                .and_then(Value::as_str)
                .unwrap_or("0");
            write_file(
                &format!("commit.{generation}.json"),
                serde_json::to_string(commit)?.as_bytes(),
            )?;
        }
        // The commit boundary is settled: an empty `commit.lock` must exist or
        // fx reports the session's authority unavailable.
        write_file("commit.lock", b"")?;

        if !body.images.is_empty() {
            let images_dir = dir.join("images");
            fs::create_dir_all(&images_dir)?;
            restrict_dir(&images_dir);
            for image in &body.images {
                if let Some(name) = Path::new(&image.path).file_name() {
                    let path = images_dir.join(name);
                    fs::write(&path, &image.data)?;
                    restrict_file(&path);
                }
            }
        }

        Ok(Saved { id, reference: dir })
    }

    /// An fx session is a self-contained directory; delete removes it whole.
    /// Guarded on shape and containment: the directory must carry an fx event
    /// log and resolve (symlinks and all) to `<sessions_dir>/<id>`, so a stale
    /// or foreign reference never removes an unrelated tree.
    fn delete(&self, reference: &PathBuf) -> Result<()> {
        if !reference.join("events.jsonl").is_file() {
            return Err(Error::Malformed {
                harness: Fx::NAME,
                detail: format!("not an fx session directory: {}", reference.display()),
            });
        }
        let canon = reference.canonicalize()?;
        let root = self.sessions_dir.canonicalize()?;
        let contained = canon
            .strip_prefix(&root)
            .is_ok_and(|rest| rest.components().count() == 1);
        if !contained {
            return Err(Error::Malformed {
                harness: Fx::NAME,
                detail: format!(
                    "refusing to delete outside the sessions root: {}",
                    reference.display()
                ),
            });
        }
        Ok(fs::remove_dir_all(canon)?)
    }

    fn fingerprints(&self, refs: &[PathBuf]) -> Result<HashMap<String, String>> {
        let mut out = HashMap::with_capacity(refs.len());
        for dir in refs {
            out.insert(
                dir.to_string_lossy().into_owned(),
                file_fingerprint(&dir.join("events.jsonl")),
            );
        }
        Ok(out)
    }
}

/// Make a session directory owner-only (`0700`). fx treats group- or
/// world-accessible session storage as unsafe and refuses to load it.
#[cfg(unix)]
fn restrict_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
}

/// Make a session file owner-only (`0600`), for the same reason as
/// [`restrict_dir`].
#[cfg(unix)]
fn restrict_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_dir(_path: &Path) {}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) {}

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

// ── hashes and hex ──────────────────────────────────────────────────────

fn sha256_hex(data: &[u8]) -> String {
    let digest = sha256(data);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(char::from(HEX[(byte >> 4) as usize]));
        out.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    out
}

const HEX: &[u8; 16] = b"0123456789abcdef";

// FIPS 180-4 SHA-256, hand-rolled to keep the codec dependency-free (matching
// the repo's other pure-Rust hashes). Only used to name/verify image snapshots.
#[allow(clippy::many_single_char_names, clippy::unreadable_literal)]
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for (hi, vi) in h.iter_mut().zip(v) {
            *hi = hi.wrapping_add(vi);
        }
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

mod serde_hex {
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut out = String::with_capacity(bytes.len() * 2);
        for &byte in bytes {
            out.push(char::from(super::HEX[(byte >> 4) as usize]));
            out.push(char::from(super::HEX[(byte & 0x0f) as usize]));
        }
        serializer.serialize_str(&out)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> std::result::Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s.len() % 2 != 0 {
            return Err(de::Error::custom("hex string has odd length"));
        }
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(de::Error::custom))
            .collect()
    }
}
