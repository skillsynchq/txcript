//! Codex: `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl`.
//!
//! A rollout is two interleaved logs sharing the `{timestamp, type, payload}`
//! envelope: `response_item` (the protocol log the model sees) and `event_msg`
//! (the display log the TUI replays). `to_common` runs the same stateful
//! aggregation the CLI provider does — turn/model/usage backfill, web-search
//! call/result pairing, and dedup of the duplicate `function_call_output` that
//! shadows a canonical `exec_command_end`/`custom_tool_call_output`. Tool names
//! are normalized to canonical (`exec_command`/`shell` → Bash, `apply_patch` →
//! Edit/Write/ApplyPatch).
//!
//! `from_common` emits both `response_item` and `event_msg` logs so Codex can
//! resume and replay the session. It also emits `turn_context`, `token_count`,
//! and `task_complete` records for model and usage metadata.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::common::{Block, ImageSource, Message, Meta, Role, Tool, ToolOutput, Usage};
use crate::error::Result;
use crate::harness::jsonl;
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

/// The Codex harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Codex;

impl Harness for Codex {
    const NAME: &'static str = "codex";
    type Body = Vec<Line>;
}

/// One rollout line. Every codex line shares this envelope; the `payload`
/// is kept as faithful JSON so native ↔ disk round-trips losslessly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Line {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// ── codec: to_common ───────────────────────────────────────────────────

#[derive(Clone)]
struct Queued {
    role: Role,
    content: Vec<Block>,
    timestamp: DateTime<Utc>,
    model: Option<String>,
    usage: Option<Usage>,
    result_call_id: Option<String>,
    is_fallback_result: bool,
}

impl Codec for Codex {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            lines_to_messages(&transcript.body, transcript.meta.timestamp),
        ))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            messages_to_lines(&transcript.meta, &transcript.body),
        ))
    }
}

impl TextCodec for Codex {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let lines: Vec<Line> = jsonl::parse(text);
        let meta = meta_from_lines(&lines);
        Ok(Transcript::new(meta, lines))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        jsonl::render(&transcript.body)
    }
}

// One stateful pass mirroring the CLI provider's aggregation: turn context,
// usage backfill, web-search pairing, and result dedup all share the same
// running state, so splitting the match arms would scatter it.
#[allow(clippy::too_many_lines)]
fn lines_to_messages(lines: &[Line], fallback_ts: DateTime<Utc>) -> Vec<Message> {
    let mut queued: Vec<Queued> = Vec::new();
    let mut current_turn_id: Option<String> = None;
    let mut turn_models: HashMap<String, String> = HashMap::new();
    let mut turn_usage: HashMap<String, Usage> = HashMap::new();
    let mut last_assistant_text_by_turn: HashMap<String, usize> = HashMap::new();
    let mut canonical_results: HashSet<String> = HashSet::new();
    let mut pending_web_search_ids: HashMap<String, Vec<String>> = HashMap::new();
    let mut unresolved_web_search_indices: HashMap<String, Vec<usize>> = HashMap::new();

    let model_for = |turn: &Option<String>, models: &HashMap<String, String>| {
        turn.as_ref().and_then(|t| models.get(t)).cloned()
    };

    for line in lines {
        let ts = line
            .timestamp
            .as_deref()
            .and_then(parse_ts)
            .unwrap_or(fallback_ts);
        let payload = &line.payload;
        match line.kind.as_str() {
            "turn_context" => {
                current_turn_id = payload
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .map(String::from);
                if let (Some(turn), Some(model)) = (
                    current_turn_id.as_ref(),
                    payload.get("model").and_then(Value::as_str),
                ) {
                    turn_models.insert(turn.clone(), model.to_string());
                }
            }
            "event_msg" => {
                let ptype = payload.get("type").and_then(Value::as_str).unwrap_or("");
                match ptype {
                    "task_started" => {
                        current_turn_id = payload
                            .get("turn_id")
                            .and_then(Value::as_str)
                            .map(String::from);
                    }
                    "task_complete" => {
                        if let Some(turn) = payload.get("turn_id").and_then(Value::as_str)
                            && let Some(&idx) = last_assistant_text_by_turn.get(turn)
                        {
                            if let Some(model) = turn_models.get(turn) {
                                queued[idx].model = Some(model.clone());
                            }
                            if let Some(usage) = turn_usage.get(turn) {
                                queued[idx].usage = Some(*usage);
                            }
                        }
                    }
                    "token_count" => {
                        if let (Some(turn), Some(usage)) =
                            (current_turn_id.as_ref(), parse_last_token_usage(payload))
                        {
                            turn_usage.insert(turn.clone(), usage);
                        }
                    }
                    "exec_command_end" => {
                        // A result without a call_id cannot pair with its
                        // call.
                        if let Some(call_id) = payload
                            .get("call_id")
                            .and_then(Value::as_str)
                            .map(String::from)
                        {
                            canonical_results.insert(call_id.clone());
                            queued.push(tool_result(
                                ts,
                                call_id,
                                ToolOutput::Text(format_exec_output(payload)),
                                payload
                                    .get("exit_code")
                                    .and_then(Value::as_i64)
                                    .is_some_and(|c| c != 0),
                                false,
                            ));
                        }
                    }
                    "web_search_end" => {
                        // A result without a call_id cannot pair with its
                        // call.
                        if let Some(call_id) = payload
                            .get("call_id")
                            .and_then(Value::as_str)
                            .map(String::from)
                        {
                            let action_key = payload
                                .get("action")
                                .and_then(|a| serde_json::to_string(a).ok())
                                .unwrap_or_default();
                            pending_web_search_ids
                                .entry(action_key.clone())
                                .or_default()
                                .push(call_id.clone());
                            if let Some(indices) =
                                unresolved_web_search_indices.get_mut(&action_key)
                                && let Some(index) = indices.pop()
                                && let Some(Block::ToolUse { id, .. }) =
                                    queued[index].content.get_mut(0)
                            {
                                id.clone_from(&call_id);
                            }
                            canonical_results.insert(call_id.clone());
                            queued.push(tool_result(
                                ts,
                                call_id,
                                ToolOutput::Text(format_web_search_result(payload)),
                                false,
                                false,
                            ));
                        }
                    }
                    // Remaining display events mirror response_item data or
                    // carry no conversational content.
                    _ => {}
                }
            }
            "response_item" => {
                let ptype = payload.get("type").and_then(Value::as_str).unwrap_or("");
                match ptype {
                    "message" => {
                        let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
                        // A message without content carries no turn.
                        if let Some(content) = payload.get("content") {
                            match role {
                                // Setup preludes (<environment_context> etc.)
                                // are harness scaffolding, not conversation.
                                "user" if is_setup_message(content) => {}
                                "user" => {
                                    let blocks = parse_content_blocks(content);
                                    // An empty parse carries no turn.
                                    if !blocks.is_empty() {
                                        queued.push(plain(Role::User, blocks, ts, None));
                                    }
                                }
                                "assistant" => {
                                    let blocks = parse_content_blocks(content);
                                    // An empty parse carries no turn.
                                    if !blocks.is_empty() {
                                        queued.push(plain(
                                            Role::Assistant,
                                            blocks,
                                            ts,
                                            model_for(&current_turn_id, &turn_models),
                                        ));
                                        if let Some(turn) = current_turn_id.as_ref() {
                                            last_assistant_text_by_turn
                                                .insert(turn.clone(), queued.len() - 1);
                                        }
                                    }
                                }
                                // Other roles (system/developer) are not
                                // conversational turns.
                                _ => {}
                            }
                        }
                    }
                    "reasoning" => {
                        // A reasoning item without summary text renders
                        // nothing.
                        if let Some(thinking) = parse_reasoning_summary(payload) {
                            queued.push(plain(
                                Role::Assistant,
                                vec![Block::Thinking {
                                    text: thinking,
                                    signature: None,
                                    encrypted: None,
                                }],
                                ts,
                                model_for(&current_turn_id, &turn_models),
                            ));
                        }
                    }
                    "function_call" => {
                        // A call without a call_id cannot pair with its
                        // result.
                        if let Some(call_id) = payload
                            .get("call_id")
                            .and_then(Value::as_str)
                            .map(String::from)
                        {
                            let raw_name = payload
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_string();
                            let raw_input = parse_optional_json_string(
                                payload.get("arguments").and_then(Value::as_str),
                            );
                            let (name, input) = normalize_function_tool(&raw_name, raw_input);
                            queued.push(tool_use(
                                ts,
                                model_for(&current_turn_id, &turn_models),
                                call_id,
                                &name,
                                input,
                            ));
                        }
                    }
                    "function_call_output" => {
                        // A result without a call_id cannot pair with its
                        // call.
                        if let Some(call_id) = payload
                            .get("call_id")
                            .and_then(Value::as_str)
                            .map(String::from)
                        {
                            let content = payload
                                .get("output")
                                .and_then(Value::as_str)
                                .map_or(ToolOutput::Text(String::new()), |s| {
                                    ToolOutput::Text(s.to_string())
                                });
                            queued.push(tool_result(ts, call_id, content, false, true));
                        }
                    }
                    "custom_tool_call" => {
                        // A call without a call_id cannot pair with its
                        // result.
                        if let Some(call_id) = payload
                            .get("call_id")
                            .and_then(Value::as_str)
                            .map(String::from)
                        {
                            let raw_name = payload
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("custom_tool")
                                .to_string();
                            let raw_input = match payload.get("input") {
                                Some(Value::String(raw)) => parse_json_string_or_raw(raw),
                                Some(other) => other.clone(),
                                None => Value::Object(Map::new()),
                            };
                            let (name, input) = normalize_custom_tool(&raw_name, raw_input);
                            queued.push(tool_use(
                                ts,
                                model_for(&current_turn_id, &turn_models),
                                call_id,
                                &name,
                                input,
                            ));
                        }
                    }
                    "custom_tool_call_output" => {
                        // A result without a call_id cannot pair with its
                        // call.
                        if let Some(call_id) = payload
                            .get("call_id")
                            .and_then(Value::as_str)
                            .map(String::from)
                        {
                            let raw = payload
                                .get("output")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            let (content, is_error) = parse_custom_tool_output(raw);
                            canonical_results.insert(call_id.clone());
                            queued.push(tool_result(ts, call_id, content, is_error, false));
                        }
                    }
                    "web_search_call" => {
                        let action = payload.get("action").cloned().unwrap_or(Value::Null);
                        let action_key = serde_json::to_string(&action).unwrap_or_default();
                        let seq = queued.len();
                        let call_id = payload
                            .get("call_id")
                            .and_then(Value::as_str)
                            .map(String::from)
                            .or_else(|| {
                                pending_web_search_ids
                                    .get_mut(&action_key)
                                    .and_then(Vec::pop)
                            })
                            .unwrap_or_else(|| format!("web_search:{seq}"));
                        queued.push(tool_use(
                            ts,
                            model_for(&current_turn_id, &turn_models),
                            call_id.clone(),
                            "WebSearch",
                            action,
                        ));
                        if call_id.starts_with("web_search:") {
                            unresolved_web_search_indices
                                .entry(action_key)
                                .or_default()
                                .push(queued.len() - 1);
                        }
                    }
                    // Other protocol items have no Common representation.
                    _ => {}
                }
            }
            // Other envelope kinds carry no conversational content.
            _ => {}
        }
    }

    // Drop the fallback function_call_output when a canonical result exists.
    queued
        .into_iter()
        .filter(|q| {
            !q.is_fallback_result
                || q.result_call_id
                    .as_ref()
                    .is_none_or(|c| !canonical_results.contains(c))
        })
        .map(|q| Message {
            role: q.role,
            content: q.content,
            timestamp: q.timestamp,
            model: q.model,
            stop_reason: None,
            usage: q.usage,
        })
        .collect()
}

fn plain(role: Role, content: Vec<Block>, ts: DateTime<Utc>, model: Option<String>) -> Queued {
    Queued {
        role,
        content,
        timestamp: ts,
        model,
        usage: None,
        result_call_id: None,
        is_fallback_result: false,
    }
}

fn tool_use(
    ts: DateTime<Utc>,
    model: Option<String>,
    id: String,
    name: &str,
    input: Value,
) -> Queued {
    Queued {
        role: Role::Assistant,
        content: vec![Block::ToolUse {
            id,
            tool: Tool::from_canonical(name, input),
        }],
        timestamp: ts,
        model,
        usage: None,
        result_call_id: None,
        is_fallback_result: false,
    }
}

fn tool_result(
    ts: DateTime<Utc>,
    call_id: String,
    content: ToolOutput,
    is_error: bool,
    is_fallback: bool,
) -> Queued {
    Queued {
        role: Role::User,
        content: vec![Block::ToolResult {
            tool_use_id: call_id.clone(),
            content,
            is_error,
        }],
        timestamp: ts,
        model: None,
        usage: None,
        result_call_id: Some(call_id),
        is_fallback_result: is_fallback,
    }
}

// ── codec: from_common ─────────────────────────────────────────────────

fn messages_to_lines(meta: &Meta, messages: &[Message]) -> Vec<Line> {
    let mut lines = Vec::new();

    // session_meta — codex requires model_provider/base_instructions present
    // or resume silently falls through to a fresh session. base_instructions
    // may be null (codex substitutes its defaults), but model_provider must
    // name a real provider: current codex resolves null to the empty provider
    // name and fails resume with "Model provider `` not found". Native
    // rollouts always carry "openai".
    let mut payload = json!({
        "id": meta.id,
        "timestamp": meta.timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
        "cwd": meta.cwd.clone().unwrap_or_default(),
        "originator": "codex_cli_rs",
        "cli_version": meta.cli_version.clone().unwrap_or_default(),
        "source": "cli",
        "model_provider": "openai",
        "base_instructions": Value::Null,
    });
    if let Some(branch) = meta.git_branch.as_deref()
        && let Value::Object(obj) = &mut payload
    {
        obj.insert("git".into(), json!({ "branch": branch }));
    }
    lines.push(meta_line(&meta.timestamp, "session_meta", payload));

    for (i, msg) in messages.iter().enumerate() {
        let ts = msg.timestamp.to_rfc3339_opts(SecondsFormat::Millis, true);

        // A turn boundary carries model attribution and frames usage backfill.
        let turn_id = format!("turn-{i}");
        if matches!(msg.role, Role::Assistant) {
            let mut tc = json!({ "turn_id": turn_id });
            if let Some(model) = msg.model.as_deref()
                && let Value::Object(obj) = &mut tc
            {
                obj.insert("model".into(), Value::String(model.into()));
            }
            lines.push(meta_line(&msg.timestamp, "turn_context", tc));
        }

        push_message_lines(&mut lines, msg, &ts);

        if matches!(msg.role, Role::Assistant)
            && let Some(usage) = msg.usage.as_ref()
        {
            lines.push(meta_line(
                &msg.timestamp,
                "event_msg",
                json!({
                    "type": "token_count",
                    "info": { "last_token_usage": {
                        "input_tokens": usage.input_tokens,
                        "output_tokens": usage.output_tokens,
                        "cached_input_tokens": usage.cache_read_input_tokens.unwrap_or(0),
                    }},
                }),
            ));
            lines.push(meta_line(
                &msg.timestamp,
                "event_msg",
                json!({ "type": "task_complete", "turn_id": turn_id }),
            ));
        }
    }
    lines
}

/// Emit the `response_item` (and paired display `event_msg`) lines for one message.
fn push_message_lines(lines: &mut Vec<Line>, msg: &Message, ts: &str) {
    let role_str = match msg.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let mut message_content: Vec<Value> = Vec::new();
    let mut text_chunks: Vec<String> = Vec::new();

    for block in &msg.content {
        match block {
            Block::Text { text } => {
                let kind = match msg.role {
                    Role::Assistant => "output_text",
                    Role::User => "input_text",
                };
                message_content.push(json!({ "type": kind, "text": text }));
                text_chunks.push(text.clone());
            }
            Block::Image { source } => {
                message_content.push(json!({
                    "type": "input_image",
                    "image_url": format!("data:{};{},{}", source.media_type, source.source_type, source.data),
                }));
            }
            Block::Thinking { text, .. } => {
                lines.push(meta_line_str(
                    ts,
                    "response_item",
                    json!({
                        "type": "reasoning",
                        "summary": [{ "type": "summary_text", "text": text }],
                        "encrypted_content": Value::Null,
                    }),
                ));
                lines.push(meta_line_str(
                    ts,
                    "event_msg",
                    json!({ "type": "agent_reasoning", "text": text }),
                ));
            }
            Block::ToolUse { id, tool } => {
                let (name, input) = tool.to_canonical();
                lines.push(meta_line_str(
                    ts,
                    "response_item",
                    json!({
                        "type": "function_call",
                        "name": name,
                        "arguments": input.to_string(),
                        "call_id": id,
                    }),
                ));
            }
            Block::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                lines.push(meta_line_str(
                    ts,
                    "response_item",
                    json!({
                        "type": "function_call_output",
                        "call_id": tool_use_id,
                        "output": tool_output_text(content),
                    }),
                ));
            }
        }
    }

    if !message_content.is_empty() {
        lines.push(meta_line_str(
            ts,
            "response_item",
            json!({ "type": "message", "role": role_str, "content": message_content }),
        ));
        if !text_chunks.is_empty() {
            let combined = text_chunks.join("\n\n");
            let event = match msg.role {
                Role::User => {
                    json!({ "type": "user_message", "message": combined, "kind": "plain" })
                }
                Role::Assistant => json!({ "type": "agent_message", "message": combined }),
            };
            lines.push(meta_line_str(ts, "event_msg", event));
        }
    }
}

fn meta_line(ts: &DateTime<Utc>, kind: &str, payload: Value) -> Line {
    meta_line_str(
        &ts.to_rfc3339_opts(SecondsFormat::Millis, true),
        kind,
        payload,
    )
}

fn meta_line_str(ts: &str, kind: &str, payload: Value) -> Line {
    Line {
        timestamp: Some(ts.to_string()),
        kind: kind.to_string(),
        payload,
        extra: Map::new(),
    }
}

// ── store ──────────────────────────────────────────────────────────────

/// Reads and writes Codex rollouts under a sessions root (default
/// `~/.codex/sessions`).
#[derive(Debug, Clone)]
pub struct CodexStore {
    pub sessions_dir: PathBuf,
}

impl CodexStore {
    pub fn new(sessions_dir: impl Into<PathBuf>) -> Self {
        Self {
            sessions_dir: sessions_dir.into(),
        }
    }

    /// The default sessions root: `$CODEX_HOME/sessions` when set (Codex
    /// honors that override before its home lookup), else `~/.codex/sessions`.
    #[must_use]
    pub fn default_root() -> Option<Self> {
        std::env::var_os("CODEX_HOME")
            .filter(|v| !v.is_empty())
            .map(|codex_home| Self::new(PathBuf::from(codex_home).join("sessions")))
            .or_else(|| home().map(|h| Self::new(h.join(".codex").join("sessions"))))
    }
}

impl Store for CodexStore {
    type H = Codex;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        if self.sessions_dir.is_dir() {
            let mut files = Vec::new();
            collect_rollouts(&self.sessions_dir, &mut files);
            Ok(files
                .into_iter()
                .filter_map(|path| {
                    // A rollout that fails to read, or lacks a session_meta
                    // with an id, is not a resumable session. Only
                    // session_meta lines are parsed — message payloads are
                    // skipped whole — and the scan stops at the first one
                    // when it already carries the id (the usual case).
                    let text = fs::read_to_string(&path).ok()?;
                    let mut session_lines = text
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .filter(|line| is_session_meta(line))
                        .filter_map(|line| serde_json::from_str::<Line>(line).ok());
                    let has_id = |l: &Line| l.payload.get("id").and_then(Value::as_str).is_some();
                    let first = session_lines.next()?;
                    let has_meta = has_id(&first) || session_lines.any(|l| has_id(&l));
                    has_meta.then(|| {
                        let mut meta = meta_from_lines(std::slice::from_ref(&first));
                        if meta.id.is_empty() {
                            meta.id = jsonl::file_id(&path);
                        }
                        Discovered {
                            meta,
                            reference: path,
                        }
                    })
                })
                .collect())
        } else {
            // A missing sessions root means no sessions, not an error.
            Ok(Vec::new())
        }
    }

    fn load(&self, reference: &PathBuf) -> Result<Transcript<Codex>> {
        let mut transcript = Codex::from_text(&fs::read_to_string(reference)?)?;
        if transcript.meta.id.is_empty() {
            transcript.meta.id = jsonl::file_id(reference);
        }
        Ok(transcript)
    }

    fn save(&self, transcript: &Transcript<Codex>) -> Result<Saved<PathBuf>> {
        let t = &transcript.meta.timestamp;
        let dir = self
            .sessions_dir
            .join(format!("{:04}", t.year()))
            .join(format!("{:02}", t.month()))
            .join(format!("{:02}", t.day()));
        fs::create_dir_all(&dir)?;
        let id = transcript.meta.id.clone();
        let compact = t.format("%Y-%m-%dT%H-%M-%S").to_string();
        let path = dir.join(format!("rollout-{compact}-{id}.jsonl"));
        fs::write(&path, Codex::to_text(transcript)?)?;
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
            .map(|p| (p.to_string_lossy().into_owned(), file_fingerprint(p)))
            .collect())
    }
}

/// The `type` tag alone — the cheapest classification of a line. `kind` is
/// required, as on [`Line`], so a line the full parse would skip fails the
/// probe too.
#[derive(Deserialize)]
struct KindProbe {
    #[serde(rename = "type")]
    kind: String,
}

fn is_session_meta(line: &str) -> bool {
    serde_json::from_str::<KindProbe>(line).is_ok_and(|p| p.kind == "session_meta")
}

fn meta_from_lines(lines: &[Line]) -> Meta {
    let mut meta = Meta {
        id: String::new(),
        timestamp: Utc::now(),
        cwd: None,
        git_branch: None,
        title: None,
        cli_version: None,
        model: None,
    };
    // Only the first session_meta names the session.
    if let Some(p) = lines
        .iter()
        .find(|l| l.kind == "session_meta")
        .map(|l| &l.payload)
    {
        meta.id = p
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        meta.cwd = p.get("cwd").and_then(Value::as_str).map(String::from);
        meta.git_branch = p
            .get("git")
            .and_then(|g| g.get("branch"))
            .and_then(Value::as_str)
            .map(String::from);
        meta.cli_version = p
            .get("cli_version")
            .and_then(Value::as_str)
            .map(String::from);
        meta.model = p
            .get("model")
            .or_else(|| p.get("model_name"))
            .and_then(Value::as_str)
            .map(String::from);
        if let Some(ts) = p
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_ts)
        {
            meta.timestamp = ts;
        }
    }
    meta
}

fn collect_rollouts(dir: &Path, out: &mut Vec<PathBuf>) {
    // An unreadable directory contributes no rollouts.
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rollouts(&path, out);
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && name.starts_with("rollout-")
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
            {
                out.push(path);
            }
        }
    }
}

// ── tool normalization (codex native → canonical) ──────────────────────

fn normalize_function_tool(name: &str, input: Value) -> (String, Value) {
    match name {
        "exec_command" | "shell" => normalize_shell_tool_input(input),
        _ => (name.to_string(), input),
    }
}

fn normalize_custom_tool(name: &str, input: Value) -> (String, Value) {
    match name {
        "apply_patch" => normalize_apply_patch_input(input),
        _ => (name.to_string(), input),
    }
}

fn normalize_shell_tool_input(input: Value) -> (String, Value) {
    match input {
        Value::Object(obj) => {
            let mut normalized = Map::new();
            if let Some(command) = obj
                .get("cmd")
                .and_then(command_value_to_display)
                .or_else(|| obj.get("command").and_then(command_value_to_display))
            {
                normalized.insert("command".to_string(), Value::String(command));
            }
            if let Some(workdir) = obj
                .get("workdir")
                .or_else(|| obj.get("cwd"))
                .and_then(Value::as_str)
            {
                normalized.insert("workdir".to_string(), Value::String(workdir.to_string()));
            }
            if normalized.is_empty() {
                ("Bash".to_string(), Value::Object(obj))
            } else {
                ("Bash".to_string(), Value::Object(normalized))
            }
        }
        // A non-object input has no fields to normalize; pass it through.
        other => ("Bash".to_string(), other),
    }
}

fn command_value_to_display(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let strings: Vec<String> = parts
                .iter()
                .filter_map(|part| match part {
                    Value::String(t) => Some(t.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    // Nulls and nested containers have no argv rendering.
                    Value::Null | Value::Array(_) | Value::Object(_) => None,
                })
                .collect();
            if strings.is_empty() {
                None
            } else if strings.len() >= 3
                && matches!(strings[0].as_str(), "bash" | "sh" | "zsh")
                && matches!(strings[1].as_str(), "-lc" | "-c")
            {
                Some(strings[2].clone())
            } else {
                Some(strings.join(" "))
            }
        }
        // Nulls, scalars, and objects are not command encodings.
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => None,
    }
}

// Parser for the apply_patch envelope grammar.
#[allow(clippy::too_many_lines)]
fn normalize_apply_patch_input(input: Value) -> (String, Value) {
    enum Op {
        Add {
            file_path: String,
            content: String,
        },
        Update {
            file_path: String,
            old: String,
            new: String,
        },
    }
    struct Hunk {
        old: Vec<String>,
        new: Vec<String>,
    }
    enum Pending {
        Add {
            file_path: String,
            lines: Vec<String>,
        },
        Update {
            file_path: String,
            hunks: Vec<Hunk>,
            current: Option<Hunk>,
        },
    }
    impl Pending {
        fn finish(self) -> Option<Op> {
            match self {
                Pending::Add { file_path, lines } => Some(Op::Add {
                    file_path,
                    content: lines.join("\n"),
                }),
                Pending::Update {
                    file_path,
                    mut hunks,
                    current,
                } => {
                    if let Some(h) = current {
                        hunks.push(h);
                    }
                    // Only a single-hunk update maps onto one Edit.
                    <[Hunk; 1]>::try_from(hunks).ok().map(|[h]| Op::Update {
                        file_path,
                        old: h.old.join("\n"),
                        new: h.new.join("\n"),
                    })
                }
            }
        }
    }
    struct Parser {
        ops: Vec<Op>,
        current: Option<Pending>,
    }
    impl Parser {
        /// Fold one patch line into the state; `None` means the patch does
        /// not normalize and the caller keeps the raw envelope.
        fn feed(&mut self, line: &str) -> Option<()> {
            if line == "*** Begin Patch" || line == "*** End Patch" || line == "*** End of File" {
                // Envelope framing carries no file content.
                Some(())
            } else if let Some(fp) = line.strip_prefix("*** Add File: ") {
                self.flush()?;
                self.current = Some(Pending::Add {
                    file_path: fp.to_string(),
                    lines: Vec::new(),
                });
                Some(())
            } else if let Some(fp) = line.strip_prefix("*** Update File: ") {
                self.flush()?;
                self.current = Some(Pending::Update {
                    file_path: fp.to_string(),
                    hunks: Vec::new(),
                    current: None,
                });
                Some(())
            } else if line.starts_with("*** Delete File: ") || line.starts_with("*** Move to: ") {
                // Deletes and moves have no Write/Edit equivalent.
                None
            } else {
                match self.current.as_mut() {
                    Some(Pending::Add { lines, .. }) => {
                        if let Some(c) = line.strip_prefix('+') {
                            lines.push(c.to_string());
                        }
                    }
                    Some(Pending::Update { hunks, current, .. }) => {
                        if line.starts_with("@@") {
                            if let Some(h) = current.take() {
                                hunks.push(h);
                            }
                            *current = Some(Hunk {
                                old: Vec::new(),
                                new: Vec::new(),
                            });
                        } else if let Some(h) = current.as_mut() {
                            if let Some(c) = line.strip_prefix(' ') {
                                h.old.push(c.to_string());
                                h.new.push(c.to_string());
                            } else if let Some(c) = line.strip_prefix('-') {
                                h.old.push(c.to_string());
                            } else if let Some(c) = line.strip_prefix('+') {
                                h.new.push(c.to_string());
                            }
                        }
                        // Update lines before the first @@ have no hunk to
                        // join; they are envelope noise.
                    }
                    // Content before any file header has no home.
                    None => {}
                }
                Some(())
            }
        }

        fn flush(&mut self) -> Option<()> {
            match self.current.take() {
                // Nothing pending is trivially flushed.
                None => Some(()),
                Some(p) => {
                    self.ops.push(p.finish()?);
                    Some(())
                }
            }
        }

        fn finish(mut self) -> Option<Vec<Op>> {
            self.flush()?;
            Some(self.ops)
        }
    }

    match input {
        Value::String(patch) => {
            let mut parser = Parser {
                ops: Vec::new(),
                current: None,
            };
            let single = patch
                .lines()
                .try_for_each(|line| parser.feed(line))
                .and_then(|()| parser.finish())
                // Only a lone Add/Update maps onto Write/Edit.
                .and_then(|ops| <[Op; 1]>::try_from(ops).ok());
            match single {
                Some([Op::Add { file_path, content }]) => (
                    "Write".to_string(),
                    json!({ "file_path": file_path, "content": content }),
                ),
                Some(
                    [
                        Op::Update {
                            file_path,
                            old,
                            new,
                        },
                    ],
                ) => (
                    "Edit".to_string(),
                    json!({ "file_path": file_path, "old_string": old, "new_string": new }),
                ),
                // Multi-op, delete/move, or malformed patches keep the raw
                // envelope with the touched paths listed.
                None => {
                    let files = apply_patch_file_paths(&patch);
                    (
                        "ApplyPatch".to_string(),
                        json!({ "patch": patch, "files": files }),
                    )
                }
            }
        }
        // A non-string input is not a patch envelope; pass it through.
        other => ("ApplyPatch".to_string(), other),
    }
}

fn apply_patch_file_paths(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|line| {
            line.strip_prefix("*** Update File: ")
                .or_else(|| line.strip_prefix("*** Add File: "))
                .or_else(|| line.strip_prefix("*** Delete File: "))
                .map(String::from)
        })
        .collect()
}

// ── payload parsing ────────────────────────────────────────────────────

fn parse_content_blocks(content: &Value) -> Vec<Block> {
    // Non-array content has no blocks.
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| match entry.get("type").and_then(Value::as_str)? {
            "input_text" | "output_text" => {
                let text = entry.get("text")?.as_str()?.to_string();
                (!text.is_empty()).then_some(Block::Text { text })
            }
            "input_image" => {
                let url = entry.get("image_url")?.as_str()?;
                parse_data_url_image(url).map(|source| Block::Image { source })
            }
            // Other content kinds have no Common representation.
            _ => None,
        })
        .collect()
}

fn parse_data_url_image(image_url: &str) -> Option<ImageSource> {
    let (header, data) = image_url.split_once(',')?;
    let media_type = header
        .strip_prefix("data:")?
        .strip_suffix(";base64")?
        .to_string();
    Some(ImageSource {
        source_type: "base64".to_string(),
        media_type,
        data: data.to_string(),
    })
}

fn is_setup_message(content: &Value) -> bool {
    // A setup message is a non-empty block array in which every block is a
    // text block whose text is setup scaffolding.
    content.as_array().is_some_and(|arr| {
        !arr.is_empty()
            && arr.iter().all(|entry| {
                matches!(
                    entry.get("type").and_then(Value::as_str),
                    Some("input_text" | "output_text")
                ) && entry
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(is_setup_text)
            })
    })
}

fn is_setup_text(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<environment_context>")
        || t.starts_with("<permissions instructions>")
        || t.starts_with("<collaboration_mode>")
        || t.starts_with("<sandbox_mode>")
        || t.starts_with("<approval_policy>")
}

fn parse_reasoning_summary(payload: &Value) -> Option<String> {
    let summary = payload.get("summary")?.as_array()?;
    let parts: Vec<&str> = summary
        .iter()
        .filter_map(|e| {
            (e.get("type").and_then(Value::as_str) == Some("summary_text"))
                .then(|| e.get("text").and_then(Value::as_str))
                .flatten()
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

fn parse_last_token_usage(payload: &Value) -> Option<Usage> {
    let usage = payload
        .get("info")
        .and_then(|i| i.get("last_token_usage"))?;
    Some(Usage {
        input_tokens: usage.get("input_tokens")?.as_u64()?,
        output_tokens: usage.get("output_tokens")?.as_u64()?,
        cache_read_input_tokens: usage.get("cached_input_tokens").and_then(Value::as_u64),
        cache_creation_input_tokens: None,
    })
}

fn format_exec_output(payload: &Value) -> String {
    // aggregated_output already interleaves both streams.
    if let Some(output) = payload.get("aggregated_output").and_then(Value::as_str) {
        output.to_string()
    } else {
        let stdout = payload
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let stderr = payload
            .get("stderr")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match (stdout.is_empty(), stderr.is_empty()) {
            (false, false) => format!("{stdout}\n{stderr}"),
            (false, true) => stdout.to_string(),
            (true, false) => stderr.to_string(),
            (true, true) => String::new(),
        }
    }
}

fn parse_custom_tool_output(raw: &str) -> (ToolOutput, bool) {
    match parse_json_string_or_raw(raw) {
        Value::Object(ref obj) => {
            let content = match obj.get("output") {
                Some(Value::String(s)) => ToolOutput::Text(s.clone()),
                Some(other) => ToolOutput::Json(other.clone()),
                None => ToolOutput::Text(raw.to_string()),
            };
            let is_error = obj
                .get("metadata")
                .and_then(|m| m.get("exit_code"))
                .and_then(Value::as_i64)
                .is_some_and(|c| c != 0);
            (content, is_error)
        }
        Value::String(s) => (ToolOutput::Text(s), false),
        other => (ToolOutput::Json(other), false),
    }
}

fn format_web_search_result(payload: &Value) -> String {
    let action = payload.get("action").cloned().unwrap_or(Value::Null);
    let query = payload.get("query").and_then(Value::as_str).unwrap_or("");
    match action.get("type").and_then(Value::as_str) {
        Some("search") => action
            .get("queries")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|q| !q.is_empty())
            .unwrap_or_else(|| query.to_string()),
        Some("open_page") => action
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or(query)
            .to_string(),
        Some("find_in_page") => {
            let pattern = action.get("pattern").and_then(Value::as_str).unwrap_or("");
            let url = action.get("url").and_then(Value::as_str).unwrap_or(query);
            if pattern.is_empty() {
                url.to_string()
            } else if url.is_empty() {
                pattern.to_string()
            } else {
                format!("Find `{pattern}` in {url}")
            }
        }
        _ => serde_json::to_string(&action).unwrap_or_default(),
    }
}

fn parse_json_string_or_raw(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

fn parse_optional_json_string(raw: Option<&str>) -> Value {
    raw.map_or(Value::Object(Map::new()), parse_json_string_or_raw)
}

fn tool_output_text(out: &ToolOutput) -> String {
    match out {
        ToolOutput::Text(s) => s.clone(),
        ToolOutput::Json(v) => v.to_string(),
    }
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    s.parse::<DateTime<Utc>>().ok()
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

fn home() -> Option<PathBuf> {
    super::home_dir()
}
