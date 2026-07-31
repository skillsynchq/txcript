//! pi (`@mariozechner/pi-coding-agent`): one JSONL file per session under
//! `~/.pi/agent/sessions/<encoded-cwd>/<timestamp>_<uuid>.jsonl`.
//!
//! The first line is a `session` header; the rest are tree nodes chained via
//! `id`/`parentId`, read in file order (linear is correct for the common
//! single-branch session). Conversational entries are `message` lines whose
//! `message.role` is `user`/`assistant`/`toolResult`/`bashExecution`, plus
//! `custom_message`. `model_change`/`session_info` carry metadata; the rest is
//! bookkeeping, preserved in [`Record::Other`] so native ↔ disk is lossless.
//!
//! The codec helpers here are `pub(crate)` and shared verbatim by Campfire,
//! which is the identical format under a different home.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::common::{Block, ImageSource, Message, Meta, Role, StopReason, Tool, ToolOutput, Usage};
use crate::error::Result;
use crate::harness::jsonl;
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

/// The pi harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pi;

impl Harness for Pi {
    const NAME: &'static str = "pi";
    type Body = Vec<Record>;
}

// ── native records ─────────────────────────────────────────────────────

/// One JSONL line. The message union is preserved as raw JSON and typed by the
/// codec.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Session(SessionHeader),
    Message(MessageEntry),
    Custom(CustomEntry),
    Other(Value),
}

/// The leading `session` header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHeader {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A `message` line: the tree envelope plus the raw message payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageEntry {
    pub id: String,
    #[serde(rename = "parentId", default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub message: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A `custom_message` line — extension context that replays as a user turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "parentId", default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub content: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// Deserializing from `&v` copies each field once, straight into the typed
// line — no intermediate clone of the whole Value — and leaves `v` intact
// for the fallback.
impl From<Value> for Record {
    fn from(v: Value) -> Self {
        match v.get("type").and_then(Value::as_str) {
            Some("session") => SessionHeader::deserialize(&v)
                .map(Record::Session)
                .unwrap_or(Record::Other(v)),
            Some("message") => MessageEntry::deserialize(&v)
                .map(Record::Message)
                .unwrap_or(Record::Other(v)),
            Some("custom_message") => CustomEntry::deserialize(&v)
                .map(Record::Custom)
                .unwrap_or(Record::Other(v)),
            _ => Record::Other(v),
        }
    }
}

impl From<Record> for Value {
    fn from(r: Record) -> Self {
        fn tagged(line: impl Serialize, ty: &str) -> Value {
            let mut v = serde_json::to_value(line).unwrap_or(Value::Null);
            if let Value::Object(obj) = &mut v {
                obj.insert("type".into(), Value::String(ty.into()));
            }
            v
        }
        match r {
            Record::Session(s) => tagged(s, "session"),
            Record::Message(m) => tagged(m, "message"),
            Record::Custom(c) => tagged(c, "custom_message"),
            Record::Other(v) => v,
        }
    }
}

impl Serialize for Record {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        Value::from(self.clone()).serialize(s)
    }
}

impl<'de> Deserialize<'de> for Record {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(Record::from(Value::deserialize(d)?))
    }
}

// ── codec ──────────────────────────────────────────────────────────────

impl Codec for Pi {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            records_to_messages(&transcript.body, transcript.meta.timestamp),
        ))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            messages_to_records(&transcript.meta, &transcript.body),
        ))
    }
}

impl TextCodec for Pi {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let records = records_from_text(text);
        Ok(Transcript::new(meta_from_records(&records), records))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        jsonl::render(&transcript.body)
    }
}

/// pi native records → canonical messages. Shared with Campfire.
pub(crate) fn records_to_messages(records: &[Record], fallback_ts: DateTime<Utc>) -> Vec<Message> {
    let mut messages = Vec::new();
    let mut bash_seq = 0usize;
    for record in records {
        match record {
            Record::Message(entry) => {
                let ts = entry
                    .timestamp
                    .as_deref()
                    .and_then(parse_ts)
                    .unwrap_or(fallback_ts);
                let role = entry
                    .message
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let content = entry.message.get("content").unwrap_or(&Value::Null);
                match role {
                    "user" => {
                        push_if_nonempty(
                            &mut messages,
                            Role::User,
                            parse_user_content(content),
                            ts,
                        );
                    }
                    "assistant" => {
                        let blocks = parse_assistant_content(content);
                        if !blocks.is_empty() {
                            messages.push(Message {
                                role: Role::Assistant,
                                content: blocks,
                                timestamp: ts,
                                model: entry
                                    .message
                                    .get("model")
                                    .and_then(Value::as_str)
                                    .map(String::from),
                                stop_reason: entry
                                    .message
                                    .get("stopReason")
                                    .and_then(Value::as_str)
                                    .map(parse_stop_reason),
                                usage: parse_usage(entry.message.get("usage")),
                            });
                        }
                    }
                    "toolResult" => {
                        let tool_use_id = entry
                            .message
                            .get("toolCallId")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        messages.push(Message {
                            role: Role::User,
                            content: vec![Block::ToolResult {
                                tool_use_id,
                                content: parse_tool_result_content(content),
                                is_error: entry
                                    .message
                                    .get("isError")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                            }],
                            timestamp: ts,
                            model: None,
                            stop_reason: None,
                            usage: None,
                        });
                    }
                    "bashExecution" => {
                        bash_seq += 1;
                        push_bash_execution(&entry.message, ts, bash_seq, &mut messages);
                    }
                    // Unknown or missing roles carry no conversational turn.
                    _ => {}
                }
            }
            Record::Custom(c) => {
                let ts = c
                    .timestamp
                    .as_deref()
                    .and_then(parse_ts)
                    .unwrap_or(fallback_ts);
                push_if_nonempty(
                    &mut messages,
                    Role::User,
                    parse_user_content(&c.content),
                    ts,
                );
            }
            // The header and bookkeeping lines carry no conversational turn.
            Record::Session(_) | Record::Other(_) => {}
        }
    }
    messages
}

/// Canonical messages → pi native records. Shared with Campfire.
pub(crate) fn messages_to_records(meta: &Meta, messages: &[Message]) -> Vec<Record> {
    let session_id = if meta.id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        meta.id.clone()
    };
    let mut records = Vec::with_capacity(messages.len() + 1);
    records.push(Record::Session(SessionHeader {
        id: session_id.clone(),
        timestamp: Some(meta.timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)),
        cwd: meta.cwd.clone(),
        extra: Map::from_iter([("version".into(), json!(3))]),
    }));

    // toolCall id → pi tool name, so the matching toolResult carries it.
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut parent_id: Option<String> = None;

    for (i, msg) in messages.iter().enumerate() {
        let ts_iso = msg.timestamp.to_rfc3339_opts(SecondsFormat::Millis, true);
        let ts_ms = msg.timestamp.timestamp_millis();
        for (j, payload) in pi_payloads_for(msg, ts_ms, &mut tool_names)
            .into_iter()
            .enumerate()
        {
            let entry_id = short_id(&session_id, i, j);
            records.push(Record::Message(MessageEntry {
                id: entry_id.clone(),
                parent_id: parent_id.clone(),
                timestamp: Some(ts_iso.clone()),
                message: payload,
                extra: Map::new(),
            }));
            parent_id = Some(entry_id);
        }
    }
    records
}

fn pi_payloads_for(
    msg: &Message,
    ts_ms: i64,
    tool_names: &mut HashMap<String, String>,
) -> Vec<Value> {
    let mut out = Vec::new();
    match msg.role {
        Role::User => {
            let mut content: Vec<Value> = Vec::new();
            for block in &msg.content {
                match block {
                    Block::Text { text } => content.push(json!({"type": "text", "text": text})),
                    Block::Image { source } => content.push(json!({
                        "type": "image", "data": source.data, "mimeType": source.media_type,
                    })),
                    Block::ToolResult {
                        tool_use_id,
                        content: result,
                        is_error,
                    } => {
                        let tool_name = tool_names
                            .get(tool_use_id)
                            .cloned()
                            .unwrap_or_else(|| "tool".to_string());
                        out.push(json!({
                            "role": "toolResult",
                            "toolCallId": tool_use_id,
                            "toolName": tool_name,
                            "content": [{"type": "text", "text": tool_output_text(result)}],
                            "isError": is_error,
                            "timestamp": ts_ms,
                        }));
                    }
                    // Not expressible in a pi user message.
                    Block::Thinking { .. } | Block::ToolUse { .. } => {}
                }
            }
            if !content.is_empty() {
                out.push(json!({"role": "user", "content": content, "timestamp": ts_ms}));
            }
        }
        Role::Assistant => {
            let mut content: Vec<Value> = Vec::new();
            for block in &msg.content {
                match block {
                    Block::Text { text } => content.push(json!({"type": "text", "text": text})),
                    Block::Thinking { text, .. } => {
                        content.push(json!({"type": "thinking", "thinking": text}));
                    }
                    Block::ToolUse { id, tool } => {
                        let (pi_name, pi_input) = denormalize_tool(tool);
                        tool_names.insert(id.clone(), pi_name.clone());
                        content.push(
                            json!({"type": "toolCall", "id": id, "name": pi_name, "arguments": pi_input}),
                        );
                    }
                    // Not expressible in a pi assistant message.
                    Block::Image { .. } | Block::ToolResult { .. } => {}
                }
            }
            // An assistant turn with no expressible blocks emits no record.
            if !content.is_empty() {
                let model = msg.model.clone().unwrap_or_default();
                let (provider, api) = provider_api(&model);
                out.push(json!({
                    "role": "assistant",
                    "content": content,
                    "api": api,
                    "provider": provider,
                    "model": model,
                    "usage": serialize_usage(msg.usage.as_ref()),
                    "stopReason": stop_reason_str(msg.stop_reason.as_ref()),
                    "timestamp": ts_ms,
                }));
            }
        }
    }
    out
}

// ── store ──────────────────────────────────────────────────────────────

/// Reads and writes pi-format sessions under a sessions root.
#[derive(Debug, Clone)]
pub struct PiStore {
    pub sessions_dir: PathBuf,
}

impl PiStore {
    pub fn new(sessions_dir: impl Into<PathBuf>) -> Self {
        Self {
            sessions_dir: sessions_dir.into(),
        }
    }

    /// pi's default sessions root, honoring `PI_CODING_AGENT_*` overrides.
    pub fn default_root() -> Option<Self> {
        resolve_sessions_dir(".pi", "PI").map(Self::new)
    }
}

impl Store for PiStore {
    type H = Pi;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        Ok(discover_format(&self.sessions_dir))
    }

    fn load(&self, reference: &PathBuf) -> Result<Transcript<Pi>> {
        load_session(reference, Pi::from_text)
    }

    fn save(&self, transcript: &Transcript<Pi>) -> Result<Saved<PathBuf>> {
        write_session(&self.sessions_dir, &transcript.meta, &transcript.body)
    }

    fn delete(&self, reference: &PathBuf) -> Result<()> {
        Ok(std::fs::remove_file(reference)?)
    }

    fn fingerprints(&self, refs: &[PathBuf]) -> Result<HashMap<String, String>> {
        Ok(file_fingerprints(refs))
    }
}

// ── shared store helpers (also used by Campfire) ───────────────────────

/// Read a pi/Campfire file and parse it with the given harness's `from_text`,
/// filling an empty id from the filename. Shared by both stores' `load`.
pub(crate) fn load_session<H, F>(path: &Path, from_text: F) -> Result<Transcript<H>>
where
    H: Harness,
    F: Fn(&str) -> Result<Transcript<H>>,
{
    let mut transcript = from_text(&fs::read_to_string(path)?)?;
    if transcript.meta.id.is_empty() {
        transcript.meta.id = jsonl::file_id(path);
    }
    Ok(transcript)
}

pub(crate) fn discover_format(sessions_dir: &Path) -> Vec<Discovered<PathBuf>> {
    let mut files = Vec::new();
    if sessions_dir.is_dir() {
        collect_jsonl(sessions_dir, &mut files);
    }
    files
        .into_iter()
        .filter_map(|path| {
            // An unreadable file discovers nothing.
            let text = fs::read_to_string(&path).ok()?;
            // A pi file's first record must be a session header, else skip it.
            let records = meta_scan(&text)?;
            let mut meta = meta_from_records(&records);
            if meta.id.is_empty() {
                meta.id = jsonl::file_id(&path);
            }
            Some(Discovered {
                meta,
                reference: path,
            })
        })
        .collect()
}

/// Parse one line straight into its typed record — no intermediate [`Value`]
/// tree. Only lines outside the typed set pay a second parse into the
/// preserved [`Record::Other`] `Value`: an unknown or non-string tag, or the
/// rare known-tag line whose body fails its typed schema. Invalid JSON
/// parses to `None` and is skipped, exactly as under [`jsonl::parse`].
fn record_from_line(line: &str) -> Option<Record> {
    let other = || serde_json::from_str::<Value>(line).ok().map(Record::Other);
    match serde_json::from_str::<jsonl::TypeProbe>(line) {
        // A failed probe isn't necessarily junk: a non-string `type` also
        // fails it. The fallback keeps such lines whole and skips the rest.
        Err(_) => other(),
        Ok(probe) => match probe.kind.as_deref() {
            Some("session") => serde_json::from_str(line)
                .ok()
                .map(Record::Session)
                .or_else(other),
            Some("message") => serde_json::from_str(line)
                .ok()
                .map(Record::Message)
                .or_else(other),
            Some("custom_message") => serde_json::from_str(line)
                .ok()
                .map(Record::Custom)
                .or_else(other),
            Some(_) | None => other(),
        },
    }
}

/// Every record in a session's text, typed one-pass per line. Shared with
/// Campfire, whose files are pi-shaped.
pub(crate) fn records_from_text(text: &str) -> Vec<Record> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(record_from_line)
        .collect()
}

/// Shallow scan for discovery: the first record decides whether this is a pi
/// file at all (`None` when it is not a session header), and after it only
/// the tiny meta-bearing line types are parsed — message payloads are never
/// built. The result folds through [`meta_from_records`] exactly as the full
/// parse would: the line types it skips contribute nothing to the fold.
fn meta_scan(text: &str) -> Option<Vec<Record>> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    // Lines that aren't valid JSON are not records at all (`jsonl::parse`
    // skips them), so the first record is the first line that parses.
    let first = lines
        .by_ref()
        .find_map(|line| serde_json::from_str::<Record>(line).ok())?;
    match first {
        Record::Session(header) => Some(
            std::iter::once(Record::Session(header))
                .chain(lines.filter_map(meta_line))
                .collect(),
        ),
        // A file whose first record is not a session header is not a pi
        // session.
        Record::Message(_) | Record::Custom(_) | Record::Other(_) => None,
    }
}

/// Parse one line only if its type can carry session metadata; conversational
/// lines (the bulk of a session) are never parsed past the type tag.
fn meta_line(line: &str) -> Option<Record> {
    let probe: jsonl::TypeProbe = serde_json::from_str(line).ok()?;
    match probe.kind.as_deref() {
        Some("session" | "model_change" | "session_info") => {
            serde_json::from_str::<Record>(line).ok()
        }
        // No other line type carries session metadata.
        Some(_) | None => None,
    }
}

pub(crate) fn write_session(
    sessions_dir: &Path,
    meta: &Meta,
    records: &[Record],
) -> Result<Saved<PathBuf>> {
    let cwd = meta.cwd.as_deref().unwrap_or_default();
    let dir = sessions_dir.join(encode_cwd(cwd));
    fs::create_dir_all(&dir)?;
    let id = meta.id.clone();
    let file_ts = meta
        .timestamp
        .to_rfc3339_opts(SecondsFormat::Millis, true)
        .replace([':', '.'], "-");
    let path = dir.join(format!("{file_ts}_{id}.jsonl"));
    fs::write(&path, jsonl::render(records)?)?;
    Ok(Saved {
        id,
        reference: path,
    })
}

pub(crate) fn meta_from_records(records: &[Record]) -> Meta {
    let mut meta = Meta {
        id: String::new(),
        timestamp: Utc::now(),
        cwd: None,
        git_branch: None,
        title: None,
        cli_version: None,
        model: None,
    };
    for record in records {
        match record {
            Record::Session(s) => {
                meta.id.clone_from(&s.id);
                meta.cwd.clone_from(&s.cwd);
                if let Some(ts) = s.timestamp.as_deref().and_then(parse_ts) {
                    meta.timestamp = ts;
                }
            }
            Record::Other(v) => match v.get("type").and_then(Value::as_str) {
                // Latest model_change wins; model can switch mid-session.
                Some("model_change") => {
                    if let Some(m) = v.get("modelId").and_then(Value::as_str) {
                        meta.model = Some(m.to_string());
                    }
                }
                Some("session_info") => {
                    meta.title = v
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from);
                }
                // Other bookkeeping types carry no session metadata.
                _ => {}
            },
            // Conversational lines carry no session metadata.
            Record::Message(_) | Record::Custom(_) => {}
        }
    }
    meta
}

/// pi/Campfire sessions-dir resolution: `<PREFIX>_CODING_AGENT_SESSION_DIR`
/// wins, then `<PREFIX>_CODING_AGENT_DIR` + `/sessions`, then
/// `~/<config_dir>/agent/sessions`.
pub(crate) fn resolve_sessions_dir(config_dir: &str, env_prefix: &str) -> Option<PathBuf> {
    let expand = |raw: String| -> Option<PathBuf> {
        if raw.is_empty() {
            None
        } else if let Some(rest) = raw.strip_prefix('~') {
            home().map(|h| h.join(rest.trim_start_matches(['/', '\\'])))
        } else {
            Some(PathBuf::from(raw))
        }
    };
    let env_dir = |suffix: &str| {
        std::env::var(format!("{env_prefix}_CODING_AGENT_{suffix}"))
            .ok()
            .and_then(&expand)
    };
    env_dir("SESSION_DIR").or_else(|| {
        env_dir("DIR")
            .or_else(|| home().map(|h| h.join(config_dir).join("agent")))
            .map(|agent_dir| agent_dir.join("sessions"))
    })
}

// ── content parsing ────────────────────────────────────────────────────

fn parse_user_content(content: &Value) -> Vec<Block> {
    match content {
        Value::String(s) => {
            if s.trim().is_empty() {
                Vec::new()
            } else {
                vec![Block::Text { text: s.clone() }]
            }
        }
        Value::Array(arr) => arr
            .iter()
            .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let text = b.get("text")?.as_str()?;
                    (!text.trim().is_empty()).then(|| Block::Text {
                        text: text.to_string(),
                    })
                }
                Some("image") => parse_image(b).map(|source| Block::Image { source }),
                // Unknown or untagged blocks carry no user content.
                _ => None,
            })
            .collect(),
        // Other JSON shapes carry no user content.
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => Vec::new(),
    }
}

fn parse_assistant_content(content: &Value) -> Vec<Block> {
    // Assistant content is always a block array; anything else carries none.
    content.as_array().map_or_else(Vec::new, |arr| {
        arr.iter()
            .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let text = b.get("text")?.as_str()?;
                    (!text.trim().is_empty()).then(|| Block::Text {
                        text: text.to_string(),
                    })
                }
                Some("thinking") => {
                    let thinking = b.get("thinking")?.as_str()?;
                    (!thinking.trim().is_empty()).then(|| Block::Thinking {
                        text: thinking.to_string(),
                        signature: None,
                        encrypted: None,
                    })
                }
                Some("toolCall") => {
                    let id = b
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let raw_name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let raw_input = b
                        .get("arguments")
                        .cloned()
                        .unwrap_or(Value::Object(Map::new()));
                    let (name, input) = normalize_tool(raw_name, raw_input);
                    Some(Block::ToolUse {
                        id,
                        tool: Tool::from_canonical(&name, input),
                    })
                }
                // Unknown or untagged blocks carry no assistant content.
                _ => None,
            })
            .collect()
    })
}

fn parse_tool_result_content(content: &Value) -> ToolOutput {
    match content {
        Value::String(s) => ToolOutput::Text(s.clone()),
        // An all-text block array flattens to plain text; one with images
        // keeps the block structure as JSON.
        Value::Array(arr)
            if !arr
                .iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("image")) =>
        {
            let text = arr
                .iter()
                .filter_map(|b| {
                    (b.get("type").and_then(Value::as_str) == Some("text"))
                        .then(|| b.get("text").and_then(Value::as_str))
                        .flatten()
                })
                .collect::<Vec<_>>()
                .join("\n");
            ToolOutput::Text(text)
        }
        Value::Array(arr) => {
            let blocks: Vec<Value> = arr
                .iter()
                .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                    Some("text") => Some(json!({"type": "text", "text": b.get("text")?.as_str()?})),
                    Some("image") => {
                        let s = parse_image(b)?;
                        Some(json!({"type": "image", "source": {
                            "type": s.source_type, "media_type": s.media_type, "data": s.data,
                        }}))
                    }
                    // Unknown or untagged blocks carry no tool output.
                    _ => None,
                })
                .collect();
            ToolOutput::Json(Value::Array(blocks))
        }
        other => ToolOutput::Json(other.clone()),
    }
}

fn parse_image(block: &Value) -> Option<ImageSource> {
    Some(ImageSource {
        source_type: "base64".to_string(),
        media_type: block
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("image/png")
            .to_string(),
        data: block.get("data").and_then(Value::as_str)?.to_string(),
    })
}

fn parse_usage(usage: Option<&Value>) -> Option<Usage> {
    let usage = usage?;
    let input = usage.get("input").and_then(Value::as_u64).unwrap_or(0);
    let output = usage.get("output").and_then(Value::as_u64).unwrap_or(0);
    let cache_read = usage.get("cacheRead").and_then(Value::as_u64);
    let cache_write = usage.get("cacheWrite").and_then(Value::as_u64);
    // An all-zero usage carries no information.
    let has_tokens =
        input != 0 || output != 0 || cache_read.unwrap_or(0) != 0 || cache_write.unwrap_or(0) != 0;
    has_tokens.then_some(Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_input_tokens: cache_read,
        cache_creation_input_tokens: cache_write,
    })
}

fn serialize_usage(usage: Option<&Usage>) -> Value {
    let input = usage.map_or(0, |u| u.input_tokens);
    let output = usage.map_or(0, |u| u.output_tokens);
    json!({
        "input": input,
        "output": output,
        "cacheRead": usage.and_then(|u| u.cache_read_input_tokens).unwrap_or(0),
        "cacheWrite": usage.and_then(|u| u.cache_creation_input_tokens).unwrap_or(0),
        "totalTokens": input + output,
        "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0},
    })
}

fn push_bash_execution(msg: &Value, ts: DateTime<Utc>, seq: usize, out: &mut Vec<Message>) {
    let excluded = msg
        .get("excludeFromContext")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match msg.get("command").and_then(Value::as_str) {
        // pi excludes `!!`-prefixed runs from LLM context; mirror that.
        _ if excluded => {}
        // A run without a command carries no turn.
        None | Some("") => {}
        Some(command) => {
            let output = msg.get("output").and_then(Value::as_str).unwrap_or("");
            let is_error = msg
                .get("exitCode")
                .and_then(Value::as_i64)
                .is_some_and(|c| c != 0);
            let call_id = format!("bash_exec_{seq}");
            out.push(Message {
                role: Role::Assistant,
                content: vec![Block::ToolUse {
                    id: call_id.clone(),
                    tool: Tool::Bash {
                        command: command.to_string(),
                        workdir: None,
                        timeout_ms: None,
                        description: None,
                        run_in_background: false,
                    },
                }],
                timestamp: ts,
                model: None,
                stop_reason: None,
                usage: None,
            });
            out.push(Message {
                role: Role::User,
                content: vec![Block::ToolResult {
                    tool_use_id: call_id,
                    content: ToolOutput::Text(output.to_string()),
                    is_error,
                }],
                timestamp: ts,
                model: None,
                stop_reason: None,
                usage: None,
            });
        }
    }
}

// ── tool normalization ─────────────────────────────────────────────────

/// pi tool name + input → canonical (Claude) name + input.
fn normalize_tool(tool: &str, input: Value) -> (String, Value) {
    match tool {
        // MCP tool names are identical on both sides.
        t if t.starts_with("mcp__") => (t.to_string(), input),
        "bash" => ("Bash".to_string(), input),
        "read" => (
            "Read".to_string(),
            rename_keys(input, &[("path", "file_path")]),
        ),
        "write" => (
            "Write".to_string(),
            rename_keys(input, &[("path", "file_path")]),
        ),
        "edit" => normalize_edit(input),
        "grep" => ("Grep".to_string(), input),
        "find" => ("Glob".to_string(), input),
        "ls" => ("LS".to_string(), input),
        other => (title_case(other), input),
    }
}

fn normalize_edit(input: Value) -> (String, Value) {
    match input {
        Value::Object(mut obj) => {
            let file_path = obj.remove("path");
            if let Some(Value::Array(edits)) = obj.remove("edits") {
                let mapped: Vec<Value> = edits
                    .into_iter()
                    .map(|e| {
                        rename_keys(e, &[("oldText", "old_string"), ("newText", "new_string")])
                    })
                    .collect();
                if mapped.len() == 1 {
                    let mut out = Map::new();
                    if let Some(fp) = file_path {
                        out.insert("file_path".to_string(), fp);
                    }
                    if let Some(old) = mapped[0].get("old_string") {
                        out.insert("old_string".to_string(), old.clone());
                    }
                    if let Some(new) = mapped[0].get("new_string") {
                        out.insert("new_string".to_string(), new.clone());
                    }
                    ("Edit".to_string(), Value::Object(out))
                } else {
                    let mut out = Map::new();
                    if let Some(fp) = file_path {
                        out.insert("file_path".to_string(), fp);
                    }
                    out.insert("edits".to_string(), Value::Array(mapped));
                    ("MultiEdit".to_string(), Value::Object(out))
                }
            } else {
                // Missing or non-array `edits`: a plain Edit, path renamed.
                if let Some(fp) = file_path {
                    obj.insert("file_path".to_string(), fp);
                }
                ("Edit".to_string(), Value::Object(obj))
            }
        }
        // Non-object arguments pass through unchanged.
        other => ("Edit".to_string(), other),
    }
}

/// Canonical [`Tool`] → pi tool name + arguments (inverse of `normalize_tool`).
fn denormalize_tool(tool: &Tool) -> (String, Value) {
    let (name, input) = tool.to_canonical();
    match name.as_str() {
        // MCP tool names are identical on both sides.
        n if n.starts_with("mcp__") => (n.to_string(), input),
        "Bash" => ("bash".to_string(), input),
        "Read" => (
            "read".to_string(),
            rename_keys(input, &[("file_path", "path")]),
        ),
        "Write" => (
            "write".to_string(),
            rename_keys(input, &[("file_path", "path")]),
        ),
        "Edit" => match input {
            Value::Object(mut obj) => {
                let path = obj.remove("file_path");
                let old = obj.remove("old_string").unwrap_or_default();
                let new = obj.remove("new_string").unwrap_or_default();
                let mut out = Map::new();
                if let Some(p) = path {
                    out.insert("path".to_string(), p);
                }
                out.insert(
                    "edits".to_string(),
                    json!([{"oldText": old, "newText": new}]),
                );
                ("edit".to_string(), Value::Object(out))
            }
            // Non-object arguments pass through unchanged.
            other => ("edit".to_string(), other),
        },
        "MultiEdit" => match input {
            Value::Object(mut obj) => {
                let path = obj.remove("file_path");
                let edits = match obj.remove("edits") {
                    Some(Value::Array(a)) => a,
                    // Missing or non-array `edits` maps to an empty list.
                    None | Some(_) => Vec::new(),
                };
                let mapped: Vec<Value> = edits
                    .into_iter()
                    .map(|e| {
                        rename_keys(e, &[("old_string", "oldText"), ("new_string", "newText")])
                    })
                    .collect();
                let mut out = Map::new();
                if let Some(p) = path {
                    out.insert("path".to_string(), p);
                }
                out.insert("edits".to_string(), Value::Array(mapped));
                ("edit".to_string(), Value::Object(out))
            }
            // Non-object arguments pass through unchanged.
            other => ("edit".to_string(), other),
        },
        "Grep" => ("grep".to_string(), input),
        "Glob" => ("find".to_string(), input),
        "LS" => ("ls".to_string(), input),
        other => (other.to_ascii_lowercase(), input),
    }
}

// ── small helpers ──────────────────────────────────────────────────────

fn parse_stop_reason(s: &str) -> StopReason {
    match s {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "toolUse" => StopReason::ToolUse,
        "error" => StopReason::Error,
        "aborted" => StopReason::Aborted,
        other => StopReason::Other(other.to_string()),
    }
}

fn stop_reason_str(r: Option<&StopReason>) -> &'static str {
    match r {
        Some(StopReason::MaxTokens) => "length",
        Some(StopReason::ToolUse) => "toolUse",
        Some(StopReason::Error) => "error",
        Some(StopReason::Aborted) => "aborted",
        // pi's vocabulary ends here: a normal end of turn, a stop sequence,
        // an unknown reason, or no reason at all all render as "stop".
        Some(StopReason::EndTurn | StopReason::StopSequence | StopReason::Other(_)) | None => {
            "stop"
        }
    }
}

/// Best-effort `(provider, api)` from a model id — internally plausible for the
/// historical record; pi uses the live provider on resume.
fn provider_api(model: &str) -> (&'static str, &'static str) {
    let m = model.to_ascii_lowercase();
    if m.starts_with("gpt") || m.starts_with("o1") || m.starts_with("o3") || m.contains("codex") {
        ("openai", "openai-responses")
    } else if m.starts_with("gemini") {
        ("google", "google-generative-ai")
    } else {
        ("anthropic", "anthropic-messages")
    }
}

fn tool_output_text(out: &ToolOutput) -> String {
    match out {
        ToolOutput::Text(s) => s.clone(),
        ToolOutput::Json(v) => v.to_string(),
    }
}

fn rename_keys(input: Value, renames: &[(&str, &str)]) -> Value {
    match input {
        Value::Object(mut obj) => {
            for (from, to) in renames {
                if from != to
                    && let Some(value) = obj.remove(*from)
                {
                    obj.insert((*to).to_string(), value);
                }
            }
            Value::Object(obj)
        }
        // Non-object inputs have no keys to rename.
        other => other,
    }
}

fn title_case(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn push_if_nonempty(out: &mut Vec<Message>, role: Role, content: Vec<Block>, ts: DateTime<Utc>) {
    if !content.is_empty() {
        out.push(Message {
            role,
            content,
            timestamp: ts,
            model: None,
            stop_reason: None,
            usage: None,
        });
    }
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    s.parse::<DateTime<Utc>>().ok()
}

/// Deterministic 8-char pi entry id (pi truncates uuids); pure function of the
/// session id and the message/payload index.
fn short_id(session_id: &str, i: usize, j: usize) -> String {
    const NS: Uuid = Uuid::from_bytes([
        0x70, 0x69, 0x2d, 0x65, 0x6e, 0x74, 0x72, 0x79, 0x2d, 0x69, 0x64, 0x2d, 0x6e, 0x73, 0x21,
        0x21,
    ]);
    let full = Uuid::new_v5(&NS, format!("{session_id}:{i}:{j}").as_bytes()).to_string();
    full[..8].to_string()
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    // An unreadable directory contributes no files.
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_jsonl(&path, out);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                out.push(path);
            }
        }
    }
}

/// pi's cwd → dir-name encoding: strip the leading separator, replace
/// `/ \ :` with `-`, wrap in `--…--`.
fn encode_cwd(cwd: &str) -> String {
    let body: String = cwd
        .trim_start_matches(['/', '\\'])
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    format!("--{body}--")
}

fn file_fingerprints(refs: &[PathBuf]) -> HashMap<String, String> {
    refs.iter()
        .map(|path| {
            let fp = fs::metadata(path)
                .ok()
                .and_then(|m| {
                    let len = m.len();
                    m.modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| format!("{}:{len}", d.as_nanos()))
                })
                .unwrap_or_default();
            (path.to_string_lossy().into_owned(), fp)
        })
        .collect()
}

fn home() -> Option<PathBuf> {
    super::home_dir()
}
