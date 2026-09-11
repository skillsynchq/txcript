//! Grok Bot (desktop assistant) transcripts: append-only JSONL under
//! `agent-transcripts/<agent-uuid>/<agent-uuid>.jsonl`.
//!
//! Distinct from the existing [`grok`](crate::harness::grok) harness (Grok
//! CLI / Grok Build at `~/.grok/sessions`). Discovery unions
//! `agents/<uuid>/profile.json` (title = profile `name`) with
//! `agent-transcripts` JSONL dirs (including `sand-subagent-*`). Load prefers
//! JSONL, then non-empty `store.db` `transcript_entries`, then the local
//! gateway `openAgent` when reachable. Grok Bot writes one JSON object
//! per line with no session header:
//!
//! ```json
//! {"role":"user"|"assistant"|"tool","message":{"content":[…blocks…]}}
//! ```
//!
//! Content blocks are Anthropic-shaped (`text`, `tool_use`, `tool_result`).
//! Tool results ride on native `role: "tool"` records and project onto
//! [`Role::User`](crate::common::Role::User) in [`Common`](crate::Common).
//! User prompts are often tagged with a message-address prefix such as
//! `[t0u]\n`; that prefix is stripped when projecting text into Common and
//! is not regenerated on `from_common`.
//!
//! `send_message` frequently duplicates the preceding assistant text. Both
//! the readable text block and the `send_message` tool call are kept in
//! Common — no deliberate dedupe — so a same-harness round trip can rebuild
//! the native shape.
//!
//! Tool names `shell` / `read` normalize to Claude-canonical `Bash` / `Read`
//! (with `timeout`→`timeout_ms`, `path`→`file_path`); denormalize is the
//! inverse. Other tools (`send_message`, `web_search`, `web_fetch`, …) pass
//! through as [`Tool::Raw`](crate::common::Tool::Raw). A `toolCallId` inside
//! tool input becomes the Common tool-use id and is re-inserted on write.
//!
//! Continue-into mints a new box-harness agent through the live local gateway
//! (`POST /api/createAgent` with `harness: "box"`), seeds `store.db`
//! `transcript_entries` from Common, writes agent-transcripts JSONL, and
//! `openAgent`s so the UI shows history. A root override to `local::write`
//! still writes JSONL only (tests / offline). Cloning an existing agent with
//! full blob history is the separate `duplicateAgent` + DB-restore path
//! documented in `docs/formats/grok-bot.md`.
//!
//! Known representational losses through Common:
//! - address prefixes (`[t0u]`) on user text;
//! - per-record timestamps, model, usage, and stop reasons (absent natively);
//! - shell bookkeeping keys that survive only while the call stays `Tool::Raw`
//!   (`parsingResult`, `simpleCommands`, sandbox flags, …);
//! - thinking / image / artifact blocks (no observed native slots);
//! - the optional `.journal-mode` sidecar (ignored; not part of the body).

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(any(feature = "opencode", feature = "hermes"))]
use std::io::{Read, Write};
#[cfg(any(feature = "opencode", feature = "hermes"))]
use std::net::TcpStream;

use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::common::{Block, Message, Meta, Role, Tool, ToolOutput};
use crate::error::{Error, Result};
use crate::harness::jsonl;
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

/// Fixed namespace for deterministic synthetic tool-use ids.
const TOOL_ID_NAMESPACE: Uuid = Uuid::from_u128(0xb07_a9e7_4c2d_5f18_9a3b_6e1c_8d4f_2a70);

/// The Grok Bot harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrokBot;

impl Harness for GrokBot {
    const NAME: &'static str = "grok_bot";
    type Body = Body;
}

/// Native body: the JSONL records as raw JSON values so unknown keys and
/// unmodeled roles survive same-format round trips.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Body {
    pub records: Vec<Value>,
}

// ── TextCodec ──────────────────────────────────────────────────────────

impl TextCodec for GrokBot {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let records: Vec<Value> = jsonl::parse(text);
        let meta = meta_from_records(&records);
        Ok(Transcript::new(meta, Body { records }))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        jsonl::render(&transcript.body.records)
    }
}

// ── Codec ──────────────────────────────────────────────────────────────

impl Codec for GrokBot {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            records_to_messages(&transcript.body.records, &transcript.meta),
        ))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        let mut meta = transcript.meta.clone();
        if meta.id.is_empty() {
            meta.id = Uuid::new_v4().to_string();
        }
        Ok(Transcript::new(
            meta.clone(),
            Body {
                records: messages_to_records(&meta, &transcript.body),
            },
        ))
    }
}

fn records_to_messages(records: &[Value], meta: &Meta) -> Vec<Message> {
    let mut messages = Vec::new();
    let mut pending: VecDeque<(String, String)> = VecDeque::new();
    let last_ts = meta.timestamp;
    let session = meta.id.as_str();

    for (i, record) in records.iter().enumerate() {
        let role = record.get("role").and_then(Value::as_str).unwrap_or("");
        let content = record
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);

        match role {
            "user" => {
                let blocks = project_user_blocks(content, session, i, &mut pending);
                if !blocks.is_empty() {
                    messages.push(Message {
                        role: Role::User,
                        content: blocks,
                        timestamp: last_ts,
                        model: None,
                        stop_reason: None,
                        usage: None,
                    });
                }
            }
            "assistant" => {
                let blocks = project_assistant_blocks(content, session, i, &mut pending);
                if !blocks.is_empty() {
                    messages.push(Message {
                        role: Role::Assistant,
                        content: blocks,
                        timestamp: last_ts,
                        model: None,
                        stop_reason: None,
                        usage: None,
                    });
                }
            }
            "tool" => {
                let blocks = project_tool_blocks(content, &mut pending);
                if !blocks.is_empty() {
                    messages.push(Message {
                        role: Role::User,
                        content: blocks,
                        timestamp: last_ts,
                        model: None,
                        stop_reason: None,
                        usage: None,
                    });
                }
            }
            // Unmodeled roles stay in the native body only.
            _ => {}
        }
    }
    messages
}

fn project_user_blocks(
    content: &[Value],
    session: &str,
    msg_idx: usize,
    pending: &mut VecDeque<(String, String)>,
) -> Vec<Block> {
    content
        .iter()
        .enumerate()
        .filter_map(|(j, b)| match b.get("type").and_then(Value::as_str) {
            Some("text") => text_block(b, true),
            Some("tool_result") => Some(tool_result_block(b, pending)),
            Some("tool_use") => tool_use_block(b, session, msg_idx, j, pending),
            _ => None,
        })
        .collect()
}

fn project_assistant_blocks(
    content: &[Value],
    session: &str,
    msg_idx: usize,
    pending: &mut VecDeque<(String, String)>,
) -> Vec<Block> {
    content
        .iter()
        .enumerate()
        .filter_map(|(j, b)| match b.get("type").and_then(Value::as_str) {
            Some("text") => text_block(b, false),
            Some("tool_use") => tool_use_block(b, session, msg_idx, j, pending),
            Some("thinking") => thinking_block(b),
            _ => None,
        })
        .collect()
}

fn project_tool_blocks(content: &[Value], pending: &mut VecDeque<(String, String)>) -> Vec<Block> {
    content
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        .map(|b| tool_result_block(b, pending))
        .collect()
}

fn text_block(b: &Value, strip_address: bool) -> Option<Block> {
    let mut text = b.get("text").and_then(Value::as_str)?.to_string();
    if strip_address {
        text = strip_address_prefix(&text).to_string();
    }
    if text.trim().is_empty() {
        return None;
    }
    Some(Block::Text { text })
}

fn thinking_block(b: &Value) -> Option<Block> {
    let text = b
        .get("thinking")
        .or_else(|| b.get("text"))
        .and_then(Value::as_str)?
        .to_string();
    if text.trim().is_empty() {
        return None;
    }
    Some(Block::Thinking {
        text,
        signature: b.get("signature").and_then(Value::as_str).map(String::from),
        encrypted: None,
    })
}

fn tool_use_block(
    b: &Value,
    session: &str,
    msg_idx: usize,
    block_idx: usize,
    pending: &mut VecDeque<(String, String)>,
) -> Option<Block> {
    let name = b.get("name").and_then(Value::as_str)?.to_string();
    let mut input = b.get("input").cloned().unwrap_or_else(|| json!({}));
    let id = take_tool_call_id(&mut input)
        .unwrap_or_else(|| synthetic_tool_id(session, msg_idx, block_idx, &name));
    let (canonical, input) = normalize_tool(&name, input);
    let tool = Tool::from_canonical(&canonical, input);
    pending.push_back((id.clone(), name));
    Some(Block::ToolUse { id, tool })
}

fn tool_result_block(b: &Value, pending: &mut VecDeque<(String, String)>) -> Block {
    let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
    let tool_use_id = take_pending(pending, name).unwrap_or_else(|| {
        // Orphan result: deterministic-looking placeholder.
        format!("orphan:{name}")
    });
    let result = b.get("result").cloned().unwrap_or(Value::Null);
    let is_error = result.get("failure").is_some();
    let content = result_to_output(&result, is_error);
    Block::ToolResult {
        tool_use_id,
        content,
        is_error,
    }
}

fn take_pending(pending: &mut VecDeque<(String, String)>, name: &str) -> Option<String> {
    if let Some(pos) = pending.iter().position(|(_, n)| n == name) {
        return pending.remove(pos).map(|(id, _)| id);
    }
    pending.pop_front().map(|(id, _)| id)
}

fn result_to_output(result: &Value, is_error: bool) -> ToolOutput {
    let payload = if is_error {
        result
            .get("failure")
            .cloned()
            .unwrap_or_else(|| result.clone())
    } else if let Some(success) = result.get("success") {
        success.clone()
    } else {
        result.clone()
    };
    match payload {
        Value::String(s) => ToolOutput::Text(s),
        other => ToolOutput::Json(other),
    }
}

fn messages_to_records(_meta: &Meta, messages: &[Message]) -> Vec<Value> {
    let mut records = Vec::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();

    for message in messages {
        match message.role {
            Role::User => {
                let mut text_blocks = Vec::new();
                let mut result_blocks = Vec::new();
                for block in &message.content {
                    match block {
                        Block::Text { text } => {
                            text_blocks.push(json!({"type": "text", "text": text}));
                        }
                        Block::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            let name = tool_names
                                .get(tool_use_id)
                                .cloned()
                                .unwrap_or_else(|| "tool".to_string());
                            result_blocks.push(json!({
                                "type": "tool_result",
                                "name": name,
                                "result": output_to_result(content, *is_error),
                            }));
                        }
                        Block::Image { .. } | Block::Artifact { .. } | Block::Thinking { .. } => {}
                        Block::ToolUse { id, tool } => {
                            // Rare on user turns (e.g. slash commands); emit as assistant-shaped.
                            let (native_name, input) = denormalize_tool_use(id, tool);
                            tool_names.insert(id.clone(), native_name.clone());
                            records.push(json!({
                                "role": "assistant",
                                "message": {"content": [{
                                    "type": "tool_use",
                                    "name": native_name,
                                    "input": input,
                                }]},
                            }));
                        }
                    }
                }
                if !text_blocks.is_empty() {
                    records.push(json!({
                        "role": "user",
                        "message": {"content": text_blocks},
                    }));
                }
                for result in result_blocks {
                    records.push(json!({
                        "role": "tool",
                        "message": {"content": [result]},
                    }));
                }
            }
            Role::Assistant => {
                let mut content = Vec::new();
                for block in &message.content {
                    match block {
                        Block::Text { text } => {
                            if !text.trim().is_empty() {
                                content.push(json!({"type": "text", "text": text}));
                            }
                        }
                        Block::Thinking {
                            text, signature, ..
                        } => {
                            let mut obj = Map::new();
                            obj.insert("type".into(), json!("thinking"));
                            obj.insert("thinking".into(), json!(text));
                            if let Some(sig) = signature {
                                obj.insert("signature".into(), json!(sig));
                            }
                            content.push(Value::Object(obj));
                        }
                        Block::ToolUse { id, tool } => {
                            let (native_name, input) = denormalize_tool_use(id, tool);
                            tool_names.insert(id.clone(), native_name.clone());
                            content.push(json!({
                                "type": "tool_use",
                                "name": native_name,
                                "input": input,
                            }));
                        }
                        Block::ToolResult { .. } | Block::Image { .. } | Block::Artifact { .. } => {
                        }
                    }
                }
                if !content.is_empty() {
                    records.push(json!({
                        "role": "assistant",
                        "message": {"content": content},
                    }));
                }
            }
        }
    }
    records
}

fn denormalize_tool_use(id: &str, tool: &Tool) -> (String, Value) {
    let (canonical, input) = tool.to_canonical();
    let (native_name, mut input) = denormalize_tool(&canonical, input);
    if let Value::Object(map) = &mut input {
        map.insert("toolCallId".into(), json!(id));
    }
    (native_name, input)
}

fn output_to_result(content: &ToolOutput, is_error: bool) -> Value {
    let payload = match content {
        ToolOutput::Text(s) => Value::String(s.clone()),
        ToolOutput::Json(v) => v.clone(),
    };
    if is_error {
        json!({"failure": payload})
    } else {
        json!({"success": payload})
    }
}

// ── tool normalization ─────────────────────────────────────────────────

fn normalize_tool(name: &str, input: Value) -> (String, Value) {
    match name {
        "shell" => (
            "Bash".to_string(),
            rename_keys(input, &[("timeout", "timeout_ms")]),
        ),
        "read" => (
            "Read".to_string(),
            rename_keys(input, &[("path", "file_path")]),
        ),
        other => (other.to_string(), input),
    }
}

fn denormalize_tool(name: &str, input: Value) -> (String, Value) {
    match name {
        "Bash" => (
            "shell".to_string(),
            rename_keys(input, &[("timeout_ms", "timeout")]),
        ),
        "Read" => (
            "read".to_string(),
            rename_keys(input, &[("file_path", "path")]),
        ),
        other => (other.to_string(), input),
    }
}

fn rename_keys(mut input: Value, pairs: &[(&str, &str)]) -> Value {
    let Some(obj) = input.as_object_mut() else {
        return input;
    };
    for &(from, to) in pairs {
        if let Some(v) = obj.remove(from) {
            obj.insert(to.to_string(), v);
        }
    }
    input
}

fn take_tool_call_id(input: &mut Value) -> Option<String> {
    input
        .as_object_mut()?
        .remove("toolCallId")
        .and_then(|v| v.as_str().map(String::from))
}

fn synthetic_tool_id(session: &str, msg_idx: usize, block_idx: usize, name: &str) -> String {
    let key = format!("{session}:{msg_idx}:{block_idx}:{name}");
    Uuid::new_v5(&TOOL_ID_NAMESPACE, key.as_bytes()).to_string()
}

/// Strip a leading `[t0u]`-style address tag from user text.
fn strip_address_prefix(text: &str) -> &str {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'[') {
        return text;
    }
    let Some(end) = bytes.iter().position(|&c| c == b']') else {
        return text;
    };
    let tag = &text[1..end];
    // Observed shape: t<digits><optional letter>, e.g. t0u / t1u / t12a.
    let mut chars = tag.chars();
    if chars.next() != Some('t') {
        return text;
    }
    let mut saw_digit = false;
    for c in chars.by_ref() {
        if c.is_ascii_digit() {
            saw_digit = true;
        } else if c.is_ascii_alphabetic() && saw_digit {
            // trailing letter ok; must be end
            if chars.next().is_some() {
                return text;
            }
            break;
        } else {
            return text;
        }
    }
    if !saw_digit {
        return text;
    }
    let rest = &text[end + 1..];
    rest.strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
        .unwrap_or(rest)
}

fn meta_from_records(records: &[Value]) -> Meta {
    let mut title = None;
    for record in records {
        if record.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(content) = record
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(Value::as_str) != Some("text") {
                continue;
            }
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                let stripped = strip_address_prefix(text).trim();
                if !stripped.is_empty() {
                    title = Some(stripped.chars().take(80).collect());
                    break;
                }
            }
        }
        if title.is_some() {
            break;
        }
    }
    Meta {
        id: String::new(), // Store fills from directory name
        timestamp: Utc::now(),
        cwd: None,
        git_branch: None,
        title,
        cli_version: None,
        model: None,
    }
}

// ── Store ──────────────────────────────────────────────────────────────

/// File-backed access to Grok Bot transcripts and agent profiles.
///
/// `root` is the `agent-transcripts` directory. Optional `agents` points at
/// `agents/` (profile.json + store.db). Discovery unions both; load prefers
/// JSONL, then `store.db` `transcript_entries`, then gateway `openAgent`.
#[derive(Debug, Clone)]
pub struct GrokBotStore {
    pub root: PathBuf,
    /// Live agents directory (`TXCRIPT_GROK_BOT_AGENTS` / `~/agent-data/agents`).
    pub agents: Option<PathBuf>,
}

impl GrokBotStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        // Transcripts-only; attach agents via [`Self::with_agents`] or
        // [`Self::default_root`] so temp-dir tests do not scan the live box.
        Self {
            root: root.into(),
            agents: None,
        }
    }

    /// Override the agents directory (tests / fixtures).
    #[must_use]
    pub fn with_agents(mut self, agents: impl Into<PathBuf>) -> Self {
        self.agents = Some(agents.into());
        self
    }

    /// Resolve `TXCRIPT_GROK_BOT_ROOT` / `GROK_BOT_TRANSCRIPTS`, then
    /// `$HOME/agent-data/agent-transcripts`. Agents root is resolved from
    /// `TXCRIPT_GROK_BOT_AGENTS` / `$HOME/agent-data/agents` when present.
    #[must_use]
    pub fn default_root() -> Option<Self> {
        std::env::var_os("TXCRIPT_GROK_BOT_ROOT")
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var_os("GROK_BOT_TRANSCRIPTS").filter(|v| !v.is_empty()))
            .map(PathBuf::from)
            .or_else(|| {
                super::home_dir().map(|home| home.join("agent-data").join("agent-transcripts"))
            })
            .map(|root| Self {
                root,
                agents: agents_root(),
            })
    }

    fn session_jsonl(dir: &Path) -> Option<PathBuf> {
        let name = dir.file_name()?.to_str()?;
        let path = dir.join(format!("{name}.jsonl"));
        path.is_file().then_some(path)
    }

    fn session_id(reference: &Path) -> Option<String> {
        reference
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
    }

    fn jsonl_path_for(&self, id: &str) -> Option<PathBuf> {
        let dir = self.root.join(id);
        Self::session_jsonl(&dir)
    }

    fn agent_dir_for(&self, id: &str) -> Option<PathBuf> {
        let agents = self.agents.as_ref()?;
        let dir = agents.join(id);
        dir.is_dir().then_some(dir)
    }

    fn profile_path(agent_dir: &Path) -> PathBuf {
        agent_dir.join("profile.json")
    }

    fn store_db_path(agent_dir: &Path) -> PathBuf {
        agent_dir.join("store.db")
    }

    /// Read display name from `agents/<id>/profile.json` when present.
    fn profile_name(agent_dir: &Path) -> Option<String> {
        let raw = fs::read_to_string(Self::profile_path(agent_dir)).ok()?;
        let v: Value = serde_json::from_str(&raw).ok()?;
        v.get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    fn discover_from_agents(&self, out: &mut HashMap<String, Discovered<PathBuf>>) {
        let Some(agents) = self.agents.as_ref() else {
            return;
        };
        if !agents.is_dir() {
            return;
        }
        let Ok(entries) = fs::read_dir(agents) else {
            return;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Some(id) = dir.file_name().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            if !Self::profile_path(&dir).is_file() {
                continue;
            }
            let title = Self::profile_name(&dir);
            let mut timestamp = Utc::now();
            for candidate in [
                Self::store_db_path(&dir),
                Self::profile_path(&dir),
                dir.clone(),
            ] {
                if let Ok(modified) = fs::metadata(&candidate).and_then(|s| s.modified()) {
                    timestamp = DateTime::<Utc>::from(modified);
                    break;
                }
            }
            out.insert(
                id.clone(),
                Discovered {
                    meta: Meta {
                        id: id.clone(),
                        timestamp,
                        cwd: None,
                        git_branch: None,
                        title,
                        cli_version: None,
                        model: None,
                    },
                    // Prefer the agent directory as the list locator so mtime
                    // reflects the live bot; load still resolves JSONL by id.
                    reference: dir,
                },
            );
        }
    }

    fn discover_from_transcripts(&self, out: &mut HashMap<String, Discovered<PathBuf>>) {
        if !self.root.is_dir() {
            return;
        }
        let Ok(entries) = fs::read_dir(&self.root) else {
            return;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Some(jsonl_path) = Self::session_jsonl(&dir) else {
                continue;
            };
            let Ok(text) = fs::read_to_string(&jsonl_path) else {
                continue;
            };
            if !sniff_grok_bot(&text) {
                continue;
            }
            let Some(id) = dir.file_name().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            let mut meta = meta_from_records(&jsonl::parse::<Value>(&text));
            meta.id.clone_from(&id);
            if let Ok(modified) = fs::metadata(&jsonl_path).and_then(|s| s.modified()) {
                meta.timestamp = DateTime::<Utc>::from(modified);
            }
            match out.get_mut(&id) {
                Some(existing) => {
                    // Keep profile title when present; refresh mtime from JSONL.
                    if existing.meta.title.is_none() {
                        existing.meta.title = meta.title.take();
                    }
                    existing.meta.timestamp = meta.timestamp;
                    // Prefer transcripts path when JSONL exists (safe delete).
                    existing.reference = dir;
                }
                None => {
                    out.insert(
                        id,
                        Discovered {
                            meta,
                            reference: dir,
                        },
                    );
                }
            }
        }
    }

    fn load_from_jsonl(&self, id: &str) -> Result<Option<Transcript<GrokBot>>> {
        let Some(jsonl_path) = self.jsonl_path_for(id) else {
            return Ok(None);
        };
        let mut transcript = GrokBot::from_text(&fs::read_to_string(&jsonl_path)?)?;
        if transcript.meta.id.is_empty() {
            transcript.meta.id = id.to_string();
        }
        if let Some(agent_dir) = self.agent_dir_for(id)
            && let Some(name) = Self::profile_name(&agent_dir)
        {
            transcript.meta.title = Some(name);
        }
        if let Ok(modified) = fs::metadata(&jsonl_path).and_then(|s| s.modified()) {
            transcript.meta.timestamp = DateTime::<Utc>::from(modified);
        }
        Ok(Some(transcript))
    }

    // Under `--no-default-features` only the stub body is compiled, which
    // trips unused_self / unnecessary_wraps; the featured path uses both.
    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn load_from_store_db(&self, id: &str) -> Result<Option<Transcript<GrokBot>>> {
        #[cfg(any(feature = "opencode", feature = "hermes"))]
        {
            let Some(agent_dir) = self.agent_dir_for(id) else {
                return Ok(None);
            };
            let store_db = Self::store_db_path(&agent_dir);
            if !store_db.is_file() {
                return Ok(None);
            }
            let entries = read_transcript_entries(&store_db)?;
            if entries.is_empty() {
                return Ok(None);
            }
            let title = Self::profile_name(&agent_dir);
            let common = ui_entries_to_common(id, &entries, title);
            let mut native = GrokBot::from_common(&common)?;
            native.meta.title = common.meta.title;
            native.meta.timestamp = common.meta.timestamp;
            Ok(Some(native))
        }
        #[cfg(not(any(feature = "opencode", feature = "hermes")))]
        {
            let _ = (self, id);
            Ok(None)
        }
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn load_from_gateway(&self, id: &str) -> Result<Option<Transcript<GrokBot>>> {
        #[cfg(any(feature = "opencode", feature = "hermes"))]
        {
            let Some(gateway) = Gateway::discover() else {
                return Ok(None);
            };
            let resp = match gateway.open_agent(id) {
                Ok(v) => v,
                Err(Error::Unconvertible { .. }) => return Ok(None),
                Err(e) => return Err(e),
            };
            let entries = match resp {
                Value::Array(items) => items,
                other => other
                    .get("entries")
                    .or_else(|| other.get("messages"))
                    .or_else(|| other.get("transcript"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            };
            if entries.is_empty() {
                return Ok(None);
            }
            let title = self
                .agent_dir_for(id)
                .as_ref()
                .and_then(|d| Self::profile_name(d));
            let common = ui_entries_to_common(id, &entries, title);
            let mut native = GrokBot::from_common(&common)?;
            native.meta.title = common.meta.title;
            native.meta.timestamp = common.meta.timestamp;
            Ok(Some(native))
        }
        #[cfg(not(any(feature = "opencode", feature = "hermes")))]
        {
            let _ = (self, id);
            Ok(None)
        }
    }
}

impl Store for GrokBotStore {
    type H = GrokBot;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        let mut by_id: HashMap<String, Discovered<PathBuf>> = HashMap::new();
        self.discover_from_agents(&mut by_id);
        self.discover_from_transcripts(&mut by_id);
        let mut out: Vec<_> = by_id.into_values().collect();
        out.sort_by(|a, b| a.meta.id.cmp(&b.meta.id));
        Ok(out)
    }

    fn load(&self, reference: &PathBuf) -> Result<Transcript<GrokBot>> {
        let id = Self::session_id(reference).ok_or_else(|| Error::Malformed {
            harness: GrokBot::NAME,
            detail: format!("cannot derive agent id from {}", reference.display()),
        })?;

        if let Some(transcript) = self.load_from_jsonl(&id)? {
            return Ok(transcript);
        }
        if let Some(transcript) = self.load_from_store_db(&id)? {
            return Ok(transcript);
        }
        if let Some(transcript) = self.load_from_gateway(&id)? {
            return Ok(transcript);
        }

        Err(Error::Malformed {
            harness: GrokBot::NAME,
            detail: format!(
                "no conversation for agent `{id}`: missing \
                 agent-transcripts/{id}/{id}.jsonl, empty store.db \
                 transcript_entries, and gateway openAgent unavailable or empty"
            ),
        })
    }

    fn save(&self, transcript: &Transcript<GrokBot>) -> Result<Saved<PathBuf>> {
        let id = if transcript.meta.id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            transcript.meta.id.clone()
        };
        super::checked_id_component(GrokBot::NAME, &id)?;
        let dir = self.root.join(&id);
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{id}.jsonl"));
        fs::write(&path, GrokBot::to_text(transcript)?)?;
        Ok(Saved { id, reference: dir })
    }

    fn delete(&self, reference: &PathBuf) -> Result<()> {
        // Never wipe live `agents/<id>` trees — only remove JSONL transcripts.
        if let Some(id) = Self::session_id(reference) {
            let transcript_dir = self.root.join(&id);
            if transcript_dir.is_dir() {
                return Ok(fs::remove_dir_all(transcript_dir)?);
            }
            if let Some(agents) = &self.agents
                && reference.starts_with(agents)
            {
                // Profile-only agent with no JSONL: nothing safe to delete.
                return Ok(());
            }
            // Fall through so a missing transcript dir errors like before.
            if reference == &transcript_dir || reference.starts_with(&self.root) {
                return Ok(fs::remove_dir_all(transcript_dir)?);
            }
        }
        if reference.is_dir() {
            Ok(fs::remove_dir_all(reference)?)
        } else {
            Ok(fs::remove_file(reference)?)
        }
    }

    fn fingerprints(&self, refs: &[PathBuf]) -> Result<HashMap<String, String>> {
        Ok(refs
            .iter()
            .map(|dir| {
                let key = dir.to_string_lossy().into_owned();
                let id = Self::session_id(dir).unwrap_or_default();
                let mut parts = Vec::new();
                if let Some(jsonl) = self.jsonl_path_for(&id) {
                    parts.push(file_fingerprint(&jsonl));
                }
                if let Some(agent_dir) = self.agent_dir_for(&id) {
                    parts.push(file_fingerprint(&Self::store_db_path(&agent_dir)));
                    parts.push(file_fingerprint(&Self::profile_path(&agent_dir)));
                }
                if parts.is_empty() {
                    parts.push(file_fingerprint(dir));
                }
                (key, parts.join("|"))
            })
            .collect())
    }
}

/// First non-empty line must be a Grok Bot envelope (`role` + `message`).
fn sniff_grok_bot(text: &str) -> bool {
    let line = text.lines().find(|l| !l.trim().is_empty());
    let Some(line) = line else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    let role = v.get("role").and_then(Value::as_str);
    matches!(role, Some("user" | "assistant" | "tool")) && v.get("message").is_some()
}

fn file_fingerprint(path: &Path) -> String {
    match fs::metadata(path) {
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{mtime}:{}", meta.len())
        }
        Err(_) => String::new(),
    }
}

/// Project UI ledger entries (`store.db` / gateway `openAgent`) into Common.
///
/// Recognizes `kind: "message"` (user/assistant text) and
/// `kind: "send-message"` with `message.type == "text"`. Widgets, attachments,
/// spend events, and other kinds are skipped.
#[must_use]
pub fn ui_entries_to_common(
    id: &str,
    entries: &[Value],
    title: Option<String>,
) -> Transcript<Common> {
    let mut messages = Vec::new();
    let mut last_ts = Utc::now();
    for entry in entries {
        let ts = entry
            .get("timestampMs")
            .and_then(Value::as_i64)
            .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
            .unwrap_or(last_ts);
        last_ts = ts;
        match entry.get("kind").and_then(Value::as_str) {
            Some("message") => {
                let role = match entry.get("role").and_then(Value::as_str) {
                    Some("assistant") => Role::Assistant,
                    _ => Role::User,
                };
                let Some(text) = ui_entry_text(entry.get("content")) else {
                    continue;
                };
                messages.push(Message {
                    role,
                    content: vec![Block::Text { text }],
                    timestamp: ts,
                    model: None,
                    stop_reason: None,
                    usage: None,
                });
            }
            Some("send-message") => {
                let Some(message) = entry.get("message") else {
                    continue;
                };
                if message.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                let Some(text) = ui_entry_text(message.get("content")) else {
                    continue;
                };
                messages.push(Message {
                    role: Role::Assistant,
                    content: vec![Block::Text { text }],
                    timestamp: ts,
                    model: None,
                    stop_reason: None,
                    usage: None,
                });
            }
            _ => {}
        }
    }
    Transcript::new(
        Meta {
            id: id.to_string(),
            timestamp: last_ts,
            cwd: None,
            git_branch: None,
            title,
            cli_version: None,
            model: None,
        },
        messages,
    )
}

fn ui_entry_text(content: Option<&Value>) -> Option<String> {
    let content = content?;
    match content {
        Value::String(s) => {
            let t = s.trim();
            (!t.is_empty()).then(|| s.clone())
        }
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    let trimmed = t.trim();
                    if !trimmed.is_empty() {
                        parts.push(t);
                    }
                } else if let Some(t) = block.get("content").and_then(Value::as_str) {
                    let trimmed = t.trim();
                    if !trimmed.is_empty() {
                        parts.push(t);
                    }
                }
            }
            let joined = parts.join("\n");
            (!joined.trim().is_empty()).then_some(joined)
        }
        other => {
            let s = other.to_string();
            let t = s.trim();
            (!t.is_empty() && t != "null").then_some(s)
        }
    }
}

// ── Continue-into / gateway mint ───────────────────────────────────────

/// Mint a new Grok Bot agent whose UI transcript matches `common`.
///
/// Requires a reachable local gateway (`$HOME/agent-data/gateway.json` or
/// `TXCRIPT_GROK_BOT_GATEWAY` / `TXCRIPT_GROK_BOT_TOKEN`) and `SQLite` support
/// (the `opencode` or `hermes` feature). Seeds `agents/<id>/store.db`
/// `transcript_entries`, writes agent-transcripts JSONL, then `openAgent`.
///
/// `metadata` is an optional JSON object from `txcript continue --metadata`.
/// Recognized keys today: `name`, `description` (agent profile). Unknown keys
/// are ignored.
///
/// # Errors
/// When the live `agents` / `agent-transcripts` dirs are missing, the gateway
/// is unreachable, `SQLite` support is missing, or agent creation / seeding
/// fails.
pub fn mint_with_history(
    common: &Transcript<Common>,
    metadata: Option<&Value>,
) -> Result<Saved<PathBuf>> {
    #[cfg(any(feature = "opencode", feature = "hermes"))]
    {
        mint_with_history_inner(common, metadata)
    }
    #[cfg(not(any(feature = "opencode", feature = "hermes")))]
    {
        let _ = (common, metadata);
        Err(Error::Unconvertible {
            harness: GrokBot::NAME,
            detail: "continuing into grok_bot requires the opencode or hermes \
                     feature (SQLite) plus a live local gateway"
                .to_string(),
        })
    }
}

/// Open an existing agent in the Grok Bot UI (no CLI resume binary).
///
/// # Errors
/// When the local gateway cannot be reached or rejects the request.
pub fn open_in_ui(id: &str) -> Result<()> {
    #[cfg(any(feature = "opencode", feature = "hermes"))]
    {
        let gateway = Gateway::discover().ok_or_else(|| Error::Unconvertible {
            harness: GrokBot::NAME,
            detail: "no local Grok Bot gateway (expected $HOME/agent-data/gateway.json \
                     or TXCRIPT_GROK_BOT_GATEWAY + TXCRIPT_GROK_BOT_TOKEN)"
                .to_string(),
        })?;
        let _ = gateway.open_agent(id)?;
        Ok(())
    }
    #[cfg(not(any(feature = "opencode", feature = "hermes")))]
    {
        let _ = id;
        Err(Error::Unconvertible {
            harness: GrokBot::NAME,
            detail: "opening a grok_bot agent requires the opencode or hermes feature".to_string(),
        })
    }
}

#[cfg(any(feature = "opencode", feature = "hermes"))]
fn mint_with_history_inner(
    common: &Transcript<Common>,
    metadata: Option<&Value>,
) -> Result<Saved<PathBuf>> {
    let gateway = Gateway::discover().ok_or_else(|| Error::Unconvertible {
        harness: GrokBot::NAME,
        detail: "no local Grok Bot gateway (expected $HOME/agent-data/gateway.json \
                 or TXCRIPT_GROK_BOT_GATEWAY + TXCRIPT_GROK_BOT_TOKEN)"
            .to_string(),
    })?;
    let agents_root = agents_root().ok_or_else(|| Error::Unconvertible {
        harness: GrokBot::NAME,
        detail: "cannot resolve $HOME/agent-data/agents for grok_bot mint".to_string(),
    })?;
    let transcripts = GrokBotStore::default_root().ok_or_else(|| Error::Unconvertible {
        harness: GrokBot::NAME,
        detail: "cannot resolve agent-transcripts root for grok_bot mint".to_string(),
    })?;
    preflight_live_roots(&agents_root, &transcripts.root)?;

    let (name, description) = agent_profile_from(common, metadata);

    let id = gateway.create_agent(&name, &description)?;
    super::checked_id_component(GrokBot::NAME, &id)?;

    let agent_dir = agents_root.join(&id);
    let store_db = agent_dir.join("store.db");
    if !store_db.is_file() {
        return Err(Error::Malformed {
            harness: GrokBot::NAME,
            detail: format!("createAgent did not create {}", store_db.display()),
        });
    }

    // createAgent leaves the new agent active and locks store.db. Open a
    // different agent (if any) so we can seed transcript_entries.
    gateway.unlock_agent_db(&id)?;

    let entries = common_to_transcript_entries(common);
    write_transcript_entries(&store_db, &entries)?;

    let mut native = GrokBot::from_common(common)?;
    native.meta.id.clone_from(&id);
    let saved = transcripts.save(&native)?;

    // Load UI history for the minted agent.
    let _ = gateway.open_agent(&id)?;

    Ok(Saved {
        id,
        reference: saved.reference,
    })
}

/// Resolve createAgent name/description from `--metadata` and transcript meta.
#[must_use]
pub fn agent_profile_from(
    common: &Transcript<Common>,
    metadata: Option<&Value>,
) -> (String, String) {
    let meta_name = metadata_string(metadata, "name");
    let meta_desc = metadata_string(metadata, "description");
    let name = meta_name
        .or_else(|| {
            common
                .meta
                .title
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .map(|t| t.chars().take(80).collect::<String>())
        })
        .unwrap_or_else(|| "txcript session".to_string());
    let description = meta_desc.unwrap_or_else(|| "Continued into Grok Bot by txcript".to_string());
    (name.chars().take(80).collect(), description)
}

fn metadata_string(metadata: Option<&Value>, key: &str) -> Option<String> {
    let v = metadata?.get(key)?;
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Ensure the live Grok Bot layout exists and is writable before minting.
///
/// # Errors
/// When either directory is missing or not writable.
pub fn preflight_live_roots(agents: &Path, transcripts: &Path) -> Result<()> {
    for (label, path) in [("agents", agents), ("agent-transcripts", transcripts)] {
        if !path.is_dir() {
            return Err(Error::Unconvertible {
                harness: GrokBot::NAME,
                detail: format!(
                    "live grok_bot {label} directory missing at {} — continue into \
                     grok_bot needs a Grok Bot box layout (`~/agent-data/agents` and \
                     `~/agent-data/agent-transcripts`, or TXCRIPT_GROK_BOT_AGENTS / \
                     TXCRIPT_GROK_BOT_ROOT). Use --out <dir> for JSONL-only export",
                    path.display()
                ),
            });
        }
        let probe = path.join(".txcript-write-probe");
        match fs::write(&probe, b"") {
            Ok(()) => {
                let _ = fs::remove_file(&probe);
            }
            Err(e) => {
                return Err(Error::Unconvertible {
                    harness: GrokBot::NAME,
                    detail: format!(
                        "live grok_bot {label} directory not writable at {}: {e}",
                        path.display()
                    ),
                });
            }
        }
    }
    Ok(())
}

/// Project Common messages into the UI ledger shape used by `store.db`.
#[must_use]
pub fn common_to_transcript_entries(common: &Transcript<Common>) -> Vec<Value> {
    let mut out = Vec::new();
    let mut user_i = 0u32;
    let mut asst_turn = 0u32;
    for message in &common.body {
        let ts = u64::try_from(message.timestamp.timestamp_millis().max(0)).unwrap_or(0);
        match message.role {
            Role::User => {
                let mut texts = Vec::new();
                for block in &message.content {
                    if let Block::Text { text } = block
                        && !text.trim().is_empty()
                    {
                        texts.push(text.as_str());
                    }
                }
                if texts.is_empty() {
                    continue;
                }
                let id = format!("t{user_i}u");
                user_i += 1;
                asst_turn = user_i.saturating_sub(1);
                out.push(json!({
                    "kind": "message",
                    "id": id,
                    "role": "user",
                    "content": texts.join("\n"),
                    "timestampMs": ts,
                    "isStreaming": false,
                }));
            }
            Role::Assistant => {
                let mut part = 0u32;
                for block in &message.content {
                    let Block::Text { text } = block else {
                        continue;
                    };
                    if text.trim().is_empty() {
                        continue;
                    }
                    let id = format!("t{asst_turn}a{part}");
                    part += 1;
                    out.push(json!({
                        "kind": "send-message",
                        "id": id,
                        "message": {"type": "text", "content": text},
                        "timestampMs": ts,
                    }));
                }
            }
        }
    }
    out
}

#[cfg(any(feature = "opencode", feature = "hermes"))]
fn write_transcript_entries(store_db: &Path, entries: &[Value]) -> Result<()> {
    use rusqlite::Connection;
    let conn = Connection::open(store_db).map_err(|e| Error::Malformed {
        harness: GrokBot::NAME,
        detail: format!("open store.db: {e}"),
    })?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS transcript_entries (
            seq INTEGER PRIMARY KEY,
            id TEXT NOT NULL,
            entry TEXT NOT NULL
        );",
    )
    .map_err(|e| Error::Malformed {
        harness: GrokBot::NAME,
        detail: format!("ensure transcript_entries: {e}"),
    })?;
    conn.execute("DELETE FROM transcript_entries", [])
        .map_err(|e| Error::Malformed {
            harness: GrokBot::NAME,
            detail: format!("clear transcript_entries: {e}"),
        })?;
    let mut insert = conn
        .prepare("INSERT INTO transcript_entries(seq, id, entry) VALUES (?1, ?2, ?3)")
        .map_err(|e| Error::Malformed {
            harness: GrokBot::NAME,
            detail: format!("prepare insert: {e}"),
        })?;
    for (i, entry) in entries.iter().enumerate() {
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("entry")
            .to_string();
        let body = serde_json::to_string(entry)?;
        insert
            .execute(rusqlite::params![
                i64::try_from(i + 1).unwrap_or(i64::MAX),
                id,
                body
            ])
            .map_err(|e| Error::Malformed {
                harness: GrokBot::NAME,
                detail: format!("insert transcript entry: {e}"),
            })?;
    }
    // Bump unread so the sidebar notices.
    if let Some(last) = entries.last() {
        let ts = last.get("timestampMs").and_then(Value::as_u64).unwrap_or(0);
        let unread = json!({
            "lastActivityAt": ts,
            "lastViewedAt": 0,
            "isManuallyUnread": false,
            "unreadCount": entries.len(),
        });
        let _ = conn.execute(
            "INSERT OR REPLACE INTO kv(key, value) VALUES ('unreadState', ?1)",
            [unread.to_string()],
        );
    }
    Ok(())
}

#[cfg(any(feature = "opencode", feature = "hermes"))]
fn read_transcript_entries(store_db: &Path) -> Result<Vec<Value>> {
    use rusqlite::{Connection, OpenFlags};
    let conn =
        Connection::open_with_flags(store_db, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| {
            Error::Malformed {
                harness: GrokBot::NAME,
                detail: format!("open store.db: {e}"),
            }
        })?;
    let mut stmt = match conn.prepare("SELECT entry FROM transcript_entries ORDER BY seq ASC") {
        Ok(s) => s,
        Err(e) => {
            // Missing table => treat as empty ledger.
            let msg = e.to_string();
            if msg.contains("no such table") {
                return Ok(Vec::new());
            }
            return Err(Error::Malformed {
                harness: GrokBot::NAME,
                detail: format!("query transcript_entries: {e}"),
            });
        }
    };
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| Error::Malformed {
            harness: GrokBot::NAME,
            detail: format!("read transcript_entries: {e}"),
        })?;
    let mut out = Vec::new();
    for row in rows {
        let raw = row.map_err(|e| Error::Malformed {
            harness: GrokBot::NAME,
            detail: format!("transcript_entries row: {e}"),
        })?;
        if let Ok(v) = serde_json::from_str(&raw) {
            out.push(v);
        }
    }
    Ok(out)
}

/// Resolve `TXCRIPT_GROK_BOT_AGENTS`, then `$HOME/agent-data/agents`.
#[must_use]
pub fn agents_root() -> Option<PathBuf> {
    std::env::var_os("TXCRIPT_GROK_BOT_AGENTS")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| super::home_dir().map(|h| h.join("agent-data").join("agents")))
}

#[cfg(any(feature = "opencode", feature = "hermes"))]
#[derive(Debug, Clone)]
struct Gateway {
    host: String,
    port: u16,
    token: String,
}

#[cfg(any(feature = "opencode", feature = "hermes"))]
impl Gateway {
    fn discover() -> Option<Self> {
        if let (Ok(hostport), Ok(token)) = (
            std::env::var("TXCRIPT_GROK_BOT_GATEWAY"),
            std::env::var("TXCRIPT_GROK_BOT_TOKEN"),
        ) && !hostport.is_empty()
            && !token.is_empty()
        {
            let (host, port) = split_host_port(&hostport)?;
            return Some(Self { host, port, token });
        }
        let path = super::home_dir()?.join("agent-data").join("gateway.json");
        let raw = fs::read_to_string(path).ok()?;
        let v: Value = serde_json::from_str(&raw).ok()?;
        let host = v.get("host").and_then(Value::as_str).unwrap_or("127.0.0.1");
        let port = u16::try_from(v.get("port").and_then(Value::as_u64)?).ok()?;
        let token = v.get("token").and_then(Value::as_str)?.to_string();
        if token.is_empty() {
            return None;
        }
        Some(Self {
            host: host.to_string(),
            port,
            token,
        })
    }

    fn create_agent(&self, name: &str, description: &str) -> Result<String> {
        let body = json!({
            "name": name,
            "description": description,
            "harness": "box",
        });
        let resp = self.post("/api/createAgent", &body)?;
        resp.pointer("/agent/id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::Malformed {
                harness: GrokBot::NAME,
                detail: format!("createAgent missing agent.id: {resp}"),
            })
    }

    fn open_agent(&self, id: &str) -> Result<Value> {
        self.post("/api/openAgent", &json!({"id": id}))
    }

    fn unlock_agent_db(&self, keep_locked: &str) -> Result<()> {
        let agents = self.list_agents().unwrap_or_default();
        if let Some(other) = agents.into_iter().find(|a| a != keep_locked) {
            let _ = self.open_agent(&other)?;
        }
        Ok(())
    }

    fn list_agents(&self) -> Result<Vec<String>> {
        let resp = self.post("/api/listAgents", &json!({}))?;
        let Some(arr) = resp.as_array() else {
            return Ok(Vec::new());
        };
        Ok(arr
            .iter()
            .filter_map(|a| a.get("id").and_then(Value::as_str).map(str::to_string))
            .collect())
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let payload = serde_json::to_vec(body)?;
        let mut stream = TcpStream::connect((self.host.as_str(), self.port)).map_err(|e| {
            Error::Unconvertible {
                harness: GrokBot::NAME,
                detail: format!(
                    "cannot reach Grok Bot gateway {}:{}: {e}",
                    self.host, self.port
                ),
            }
        })?;
        let request = format!(
            "POST {path} HTTP/1.1\r\n\
             Host: {host}:{port}\r\n\
             Authorization: Bearer {token}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {len}\r\n\
             Connection: close\r\n\
             \r\n",
            host = self.host,
            port = self.port,
            token = self.token,
            len = payload.len(),
        );
        stream
            .write_all(request.as_bytes())
            .and_then(|()| stream.write_all(&payload))
            .map_err(|e| Error::Malformed {
                harness: GrokBot::NAME,
                detail: format!("gateway write failed: {e}"),
            })?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).map_err(|e| Error::Malformed {
            harness: GrokBot::NAME,
            detail: format!("gateway read failed: {e}"),
        })?;
        let text = String::from_utf8_lossy(&buf);
        let Some(idx) = text.find("\r\n\r\n") else {
            return Err(Error::Malformed {
                harness: GrokBot::NAME,
                detail: "gateway response missing header terminator".to_string(),
            });
        };
        let (header, body) = text.split_at(idx + 4);
        let status = header
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("0");
        let value: Value =
            serde_json::from_str(body.trim()).unwrap_or_else(|_| json!({"raw": body}));
        if !status.starts_with('2') {
            return Err(Error::Unconvertible {
                harness: GrokBot::NAME,
                detail: format!("gateway {path} HTTP {status}: {value}"),
            });
        }
        if value.get("error").is_some() {
            return Err(Error::Unconvertible {
                harness: GrokBot::NAME,
                detail: format!("gateway {path}: {value}"),
            });
        }
        Ok(value)
    }
}

#[cfg(any(feature = "opencode", feature = "hermes"))]
fn split_host_port(hostport: &str) -> Option<(String, u16)> {
    let hostport = hostport
        .strip_prefix("http://")
        .or_else(|| hostport.strip_prefix("https://"))
        .unwrap_or(hostport);
    let (host, port) = hostport.rsplit_once(':')?;
    Some((host.to_string(), port.parse().ok()?))
}
