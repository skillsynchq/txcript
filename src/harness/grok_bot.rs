//! Grok Bot (desktop assistant) transcripts: append-only JSONL under
//! `agent-transcripts/<agent-uuid>/<agent-uuid>.jsonl`.
//!
//! Distinct from the existing [`grok`](crate::harness::grok) harness (Grok
//! CLI / Grok Build at `~/.grok/sessions`). Grok Bot writes one JSON object
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
//! Grok Bot has no public session-import / resume CLI, so continue-into is
//! source-only (like Hermes / Amp): the store still load/saves JSONL for
//! conversion and tests, but `local::write` refuses `--with grok_bot`.
//!
//! Known representational losses through Common:
//! - address prefixes (`[t0u]`) on user text;
//! - per-record timestamps, model, usage, and stop reasons (absent natively);
//! - shell bookkeeping keys that survive only while the call stays `Tool::Raw`
//!   (`parsingResult`, `simpleCommands`, sandbox flags, …);
//! - fields beside the native result's `success` / `failure` payload (for
//!   example `isBackground`);
//! - thinking / image / artifact blocks (no observed native slots);
//! - the optional `.journal-mode` sidecar (ignored; not part of the body).

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
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
        let records = text
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .map(|(index, line)| {
                serde_json::from_str(line).map_err(|error| Error::Malformed {
                    harness: GrokBot::NAME,
                    detail: format!("invalid JSON on line {}: {error}", index + 1),
                })
            })
            .collect::<Result<Vec<Value>>>()?;
        if !records.first().is_some_and(is_grok_bot_envelope) {
            return Err(Error::Malformed {
                harness: GrokBot::NAME,
                detail: "first non-empty line is not a role/message envelope".to_string(),
            });
        }
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
    let pos = pending
        .iter()
        .position(|(_, pending_name)| pending_name == name)?;
    pending.remove(pos).map(|(id, _)| id)
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

/// File-backed access to Grok Bot `agent-transcripts` directories.
#[derive(Debug, Clone)]
pub struct GrokBotStore {
    pub root: PathBuf,
}

impl GrokBotStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve `TXCRIPT_GROK_BOT_ROOT` / `GROK_BOT_TRANSCRIPTS`, then
    /// `$HOME/agent-data/agent-transcripts`.
    #[must_use]
    pub fn default_root() -> Option<Self> {
        std::env::var_os("TXCRIPT_GROK_BOT_ROOT")
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var_os("GROK_BOT_TRANSCRIPTS").filter(|v| !v.is_empty()))
            .map(PathBuf::from)
            .or_else(|| {
                super::home_dir().map(|home| home.join("agent-data").join("agent-transcripts"))
            })
            .map(Self::new)
    }

    fn checked_session_jsonl(&self, dir: &Path) -> Result<PathBuf> {
        let metadata = fs::symlink_metadata(dir)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Error::Malformed {
                harness: GrokBot::NAME,
                detail: format!("refusing non-directory session path: {}", dir.display()),
            });
        }

        let root = self.root.canonicalize()?;
        let canonical = dir.canonicalize()?;
        let contained = canonical
            .strip_prefix(&root)
            .is_ok_and(|rest| rest.components().count() == 1);
        if !contained {
            return Err(Error::Malformed {
                harness: GrokBot::NAME,
                detail: format!("refusing session path outside root: {}", dir.display()),
            });
        }

        let name = canonical
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::Malformed {
                harness: GrokBot::NAME,
                detail: format!("invalid session directory name: {}", dir.display()),
            })?;
        let path = canonical.join(format!("{name}.jsonl"));
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Error::Malformed {
                harness: GrokBot::NAME,
                detail: format!("refusing non-file session transcript: {}", path.display()),
            });
        }
        Ok(path)
    }

    fn session_dir_for_save(&self, id: &str) -> Result<PathBuf> {
        fs::create_dir_all(&self.root)?;
        let canonical_root = self.root.canonicalize()?;
        let dir = self.root.join(id);
        match fs::symlink_metadata(&dir) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(Error::Malformed {
                    harness: GrokBot::NAME,
                    detail: format!("refusing non-directory session path: {}", dir.display()),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&dir)?,
            Err(error) => return Err(error.into()),
        }

        let canonical = dir.canonicalize()?;
        let contained = canonical.strip_prefix(&canonical_root).is_ok_and(|rest| {
            rest.components().count() == 1 && canonical.file_name() == Some(id.as_ref())
        });
        if !contained {
            return Err(Error::Malformed {
                harness: GrokBot::NAME,
                detail: format!("refusing session path outside root: {}", dir.display()),
            });
        }
        let path = canonical.join(format!("{id}.jsonl"));
        if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(Error::Malformed {
                harness: GrokBot::NAME,
                detail: format!("refusing symlinked session file: {}", path.display()),
            });
        }
        Ok(dir)
    }
}

impl Store for GrokBotStore {
    type H = GrokBot;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        if !self.root.is_dir() {
            return Ok(Vec::new());
        }
        let entries = fs::read_dir(&self.root).into_iter().flatten();
        Ok(entries
            .flatten()
            .map(|e| e.path())
            .filter_map(|dir| {
                let jsonl_path = self.checked_session_jsonl(&dir).ok()?;
                let text = fs::read_to_string(&jsonl_path).ok()?;
                if !sniff_grok_bot(&text) {
                    return None;
                }
                let mut meta = GrokBot::from_text(&text).ok()?.meta;
                if meta.id.is_empty() {
                    meta.id = dir
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                }
                if let Ok(modified) = fs::metadata(&jsonl_path).and_then(|s| s.modified()) {
                    meta.timestamp = DateTime::<Utc>::from(modified);
                }
                Some(Discovered {
                    meta,
                    reference: dir,
                })
            })
            .collect())
    }

    fn load(&self, reference: &PathBuf) -> Result<Transcript<GrokBot>> {
        let jsonl_path = self.checked_session_jsonl(reference)?;
        let mut transcript = GrokBot::from_text(&fs::read_to_string(&jsonl_path)?)?;
        if transcript.meta.id.is_empty() {
            transcript.meta.id = reference.file_name().map_or_else(
                || jsonl::file_id(&jsonl_path),
                |s| s.to_string_lossy().into_owned(),
            );
        }
        if let Ok(modified) = fs::metadata(&jsonl_path).and_then(|s| s.modified()) {
            transcript.meta.timestamp = DateTime::<Utc>::from(modified);
        }
        Ok(transcript)
    }

    fn save(&self, transcript: &Transcript<GrokBot>) -> Result<Saved<PathBuf>> {
        let id = if transcript.meta.id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            transcript.meta.id.clone()
        };
        super::checked_id_component(GrokBot::NAME, &id)?;
        let dir = self.session_dir_for_save(&id)?;
        let path = dir.join(format!("{id}.jsonl"));
        fs::write(&path, GrokBot::to_text(transcript)?)?;
        Ok(Saved { id, reference: dir })
    }

    /// A Grok Bot session is one immediate child of the transcript root.
    /// Refuse foreign and symlinked paths even when they have the right shape.
    fn delete(&self, reference: &PathBuf) -> Result<()> {
        let jsonl = self.checked_session_jsonl(reference)?;
        let dir = jsonl.parent().ok_or_else(|| Error::Malformed {
            harness: GrokBot::NAME,
            detail: format!("invalid session path: {}", reference.display()),
        })?;
        Ok(fs::remove_dir_all(dir)?)
    }

    fn fingerprints(&self, refs: &[PathBuf]) -> Result<HashMap<String, String>> {
        Ok(refs
            .iter()
            .map(|dir| {
                let fp = self
                    .checked_session_jsonl(dir)
                    .ok()
                    .map(|p| file_fingerprint(&p))
                    .unwrap_or_default();
                (dir.to_string_lossy().into_owned(), fp)
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
    is_grok_bot_envelope(&v)
}

fn is_grok_bot_envelope(value: &Value) -> bool {
    let role = value.get("role").and_then(Value::as_str);
    matches!(role, Some("user" | "assistant" | "tool")) && value.get("message").is_some()
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
