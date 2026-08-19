//! Simple — txcript's own interchange format, a pseudo-harness with no app
//! behind it.
//!
//! The native format is a single JSON document: session metadata at the top
//! level (all optional), and a `messages` array of `{role, content, …}`
//! objects whose block shapes follow the Anthropic Messages API convention.
//! `content` is a plain string or an array of `text` / `thinking` /
//! `tool_use` / `tool_result` / `image` blocks. Almost everything is
//! optional: missing tool ids are synthesized deterministically and id-less
//! results pair FIFO with preceding unpaired calls; missing timestamps
//! inherit the nearest preceding message's, then the session's. The format
//! is specified level by level in `docs/formats/simple.md` — that document
//! and this module are written to agree; when they drift, fix the document.
//!
//! Tolerance: a message that fails the typed parse, a block with an unknown
//! `type`, and a message with an unknown role are preserved verbatim in the
//! body ([`Record::Other`] or their raw fields) and excluded from the
//! canonical conversation. Unknown keys at every level ride in flattened
//! `extra` maps. Only a document that is not a JSON object with a `messages`
//! array fails to parse.
//!
//! There is no [`Store`](crate::Store): a Simple session is a document
//! handed to txcript directly (a file or stdin at the CLI, text at the WASM
//! boundary), not something discovered from or written into a managed
//! location — [`crate::local::write`] refuses the `simple` target
//! accordingly. The codec itself stays symmetric so library and WASM users
//! can render Simple text.
//!
//! Known losses: unknown keys and unmodeled records survive same-format
//! round trips but do not cross [`Common`]; there is deliberately no
//! system-prompt slot ([`Common`] has none, and every harness rebuilds its
//! own environment on resume). Within the modeled fields the codec is a
//! projection of [`Common`] itself, so `to_common(from_common(c)) == c`
//! holds for any canonical transcript `c`.

use std::collections::VecDeque;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::common::{Block, ImageSource, Message, Meta, Role, StopReason, Tool, ToolOutput, Usage};
use crate::error::{Error, Result};
use crate::transcript::{Codec, Common, Harness, TextCodec, Transcript};

/// The Simple harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Simple;

impl Harness for Simple {
    const NAME: &'static str = "simple";
    type Body = Doc;
}

/// The native body: the document's message entries plus any unknown
/// top-level keys. The modeled top-level metadata (`id`, `timestamp`, `cwd`,
/// `git_branch`, `title`, `cli_version`, `model`) lives in [`Meta`] and is
/// re-rendered from it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Doc {
    pub records: Vec<Record>,
    /// Unknown top-level keys, preserved through same-format round trips.
    pub extra: Map<String, Value>,
}

/// One element of the `messages` array: a parsed message, or — when the
/// typed parse fails (malformed timestamp, non-string-non-array content) —
/// the element preserved verbatim.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Record {
    Message(Entry),
    Other(Value),
}

/// A message as written on the wire. Every field is optional but `role` and
/// `content` are what make it conversation: an entry whose role is not
/// `user`/`assistant` is preserved but contributes nothing to [`Common`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    #[serde(default, skip_serializing_if = "Content::is_empty_blocks")]
    pub content: Content,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Unknown message-level keys, preserved.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Message content: the L0 plain string, or an array of blocks. Blocks stay
/// raw [`Value`]s in the body — the codec interprets them, serde stays
/// lossless.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Blocks(Vec<Value>),
}

impl Default for Content {
    fn default() -> Self {
        Content::Blocks(Vec::new())
    }
}

impl Content {
    /// True for the default empty block array, so an absent `content` key
    /// stays absent on render.
    fn is_empty_blocks(&self) -> bool {
        matches!(self, Content::Blocks(blocks) if blocks.is_empty())
    }
}

fn malformed(detail: impl Into<String>) -> Error {
    Error::Malformed {
        harness: Simple::NAME,
        detail: detail.into(),
    }
}

// ── codec ──────────────────────────────────────────────────────────────

impl Codec for Simple {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        let mut messages = Vec::new();
        // A message without a timestamp inherits the nearest preceding one.
        let mut last_ts = transcript.meta.timestamp;
        // Unpaired tool_use ids in emission order, for id-less results.
        let mut pending: VecDeque<String> = VecDeque::new();
        for (i, record) in transcript.body.records.iter().enumerate() {
            let Record::Message(entry) = record else {
                continue;
            };
            let role = match entry.role.trim().to_ascii_lowercase().as_str() {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                // Unknown roles are not conversation; they stay in the body.
                _ => continue,
            };
            let timestamp = entry.timestamp.unwrap_or(last_ts);
            last_ts = timestamp;
            let content = match &entry.content {
                Content::Text(text) => vec![Block::Text { text: text.clone() }],
                Content::Blocks(blocks) => blocks
                    .iter()
                    .enumerate()
                    .filter_map(|(j, block)| {
                        block_to_common(block, &transcript.meta.id, i, j, &mut pending)
                    })
                    .collect(),
            };
            messages.push(Message {
                role,
                content,
                timestamp,
                model: entry.model.clone(),
                stop_reason: entry.stop_reason.clone(),
                usage: entry.usage,
            });
        }
        Ok(Transcript::new(transcript.meta.clone(), messages))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        let records = transcript.body.iter().map(entry_from_common).collect();
        Ok(Transcript::new(
            transcript.meta.clone(),
            Doc {
                records,
                extra: Map::new(),
            },
        ))
    }
}

/// Interpret one raw block [`Value`]. `None` — an unknown `type` or a known
/// type missing its defining field — drops the block from [`Common`]; it
/// still lives in the body.
fn block_to_common(
    block: &Value,
    session_id: &str,
    i: usize,
    j: usize,
    pending: &mut VecDeque<String>,
) -> Option<Block> {
    match block.get("type").and_then(Value::as_str)? {
        "text" => Some(Block::Text {
            text: block.get("text")?.as_str()?.to_string(),
        }),
        "thinking" => Some(Block::Thinking {
            text: block.get("text")?.as_str()?.to_string(),
            signature: str_field(block, "signature"),
            encrypted: str_field(block, "encrypted"),
        }),
        "tool_use" => {
            let name = block.get("name")?.as_str()?;
            let input = block.get("input").cloned().unwrap_or(Value::Null);
            let id = str_field(block, "id").unwrap_or_else(|| synth_id(session_id, i, j));
            pending.push_back(id.clone());
            Some(Block::ToolUse {
                id,
                tool: Tool::from_canonical(name, input),
            })
        }
        "tool_result" => {
            let tool_use_id = match str_field(block, "tool_use_id") {
                Some(id) => {
                    // An explicitly paired call is no longer pending.
                    if let Some(pos) = pending.iter().position(|p| *p == id) {
                        pending.remove(pos);
                    }
                    id
                }
                // FIFO: the oldest unpaired call, per the Anthropic ordering
                // convention; a result with no call at all still gets a
                // stable synthetic id.
                None => pending
                    .pop_front()
                    .unwrap_or_else(|| synth_id(session_id, i, j)),
            };
            let content = match block.get("content") {
                None => ToolOutput::Text(String::new()),
                Some(Value::String(text)) => ToolOutput::Text(text.clone()),
                Some(other) => ToolOutput::Json(other.clone()),
            };
            Some(Block::ToolResult {
                tool_use_id,
                content,
                is_error: block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        }
        "image" => ImageSource::deserialize(block.get("source")?)
            .ok()
            .map(|source| Block::Image { source }),
        _ => None,
    }
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(String::from)
}

/// Deterministic id for a `tool_use` without one (and for a `tool_result`
/// with neither an id nor a pending call): pure function of the session id
/// and the message/block index.
fn synth_id(session_id: &str, i: usize, j: usize) -> String {
    const NS: Uuid = Uuid::from_bytes(*b"txcript-simple!!");
    Uuid::new_v5(&NS, format!("{session_id}:{i}:{j}").as_bytes()).to_string()
}

fn entry_from_common(message: &Message) -> Record {
    let content = match message.content.as_slice() {
        // The L0 shorthand: a single text block renders as a plain string.
        [Block::Text { text }] => Content::Text(text.clone()),
        blocks => Content::Blocks(blocks.iter().map(block_to_value).collect()),
    };
    Record::Message(Entry {
        role: match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
        .to_string(),
        content,
        timestamp: Some(message.timestamp),
        model: message.model.clone(),
        stop_reason: message.stop_reason.clone(),
        usage: message.usage,
        extra: Map::new(),
    })
}

fn block_to_value(block: &Block) -> Value {
    match block {
        Block::Text { text } => json!({"type": "text", "text": text}),
        Block::Thinking {
            text,
            signature,
            encrypted,
        } => {
            let mut map = Map::new();
            map.insert("type".into(), json!("thinking"));
            map.insert("text".into(), json!(text));
            if let Some(signature) = signature {
                map.insert("signature".into(), json!(signature));
            }
            if let Some(encrypted) = encrypted {
                map.insert("encrypted".into(), json!(encrypted));
            }
            Value::Object(map)
        }
        Block::ToolUse { id, tool } => {
            let (name, input) = tool.to_canonical();
            json!({"type": "tool_use", "id": id, "name": name, "input": input})
        }
        Block::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let mut map = Map::new();
            map.insert("type".into(), json!("tool_result"));
            map.insert("tool_use_id".into(), json!(tool_use_id));
            map.insert(
                "content".into(),
                match content {
                    ToolOutput::Text(text) => Value::String(text.clone()),
                    ToolOutput::Json(value) => value.clone(),
                },
            );
            if *is_error {
                map.insert("is_error".into(), Value::Bool(true));
            }
            Value::Object(map)
        }
        Block::Image { source } => json!({"type": "image", "source": {
            "type": source.source_type,
            "media_type": source.media_type,
            "data": source.data,
        }}),
    }
}

// ── text codec ─────────────────────────────────────────────────────────

impl TextCodec for Simple {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let document: Value = serde_json::from_str(text)?;
        let Value::Object(mut map) = document else {
            return Err(malformed("top level is not a JSON object"));
        };
        let messages = match map.remove("messages") {
            Some(Value::Array(messages)) => messages,
            Some(_) => return Err(malformed("`messages` is not an array")),
            None => return Err(malformed("no `messages` array")),
        };
        let records = messages.into_iter().map(record).collect();
        // The timestamp is extracted only when it parses; any other value
        // stays in `extra` (and wins on render), so an odd document round
        // trips rather than losing the field.
        let timestamp = map
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<DateTime<Utc>>().ok());
        if timestamp.is_some() {
            map.remove("timestamp");
        }
        let meta = Meta {
            id: take_str(&mut map, "id").unwrap_or_default(),
            timestamp: timestamp.unwrap_or_else(Utc::now),
            cwd: take_str(&mut map, "cwd"),
            git_branch: take_str(&mut map, "git_branch"),
            title: take_str(&mut map, "title"),
            cli_version: take_str(&mut map, "cli_version"),
            model: take_str(&mut map, "model"),
        };
        Ok(Transcript::new(
            meta,
            Doc {
                records,
                extra: map,
            },
        ))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        let meta = &transcript.meta;
        let mut map = Map::new();
        if !meta.id.is_empty() {
            map.insert("id".into(), json!(meta.id));
        }
        map.insert(
            "timestamp".into(),
            json!(meta.timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)),
        );
        for (key, value) in [
            ("cwd", &meta.cwd),
            ("git_branch", &meta.git_branch),
            ("title", &meta.title),
            ("cli_version", &meta.cli_version),
            ("model", &meta.model),
        ] {
            if let Some(value) = value {
                map.insert(key.into(), json!(value));
            }
        }
        map.insert(
            "messages".into(),
            serde_json::to_value(&transcript.body.records)?,
        );
        // Unknown top-level keys last: on the rare collision (a `timestamp`
        // that never parsed), the preserved original wins.
        for (key, value) in &transcript.body.extra {
            map.insert(key.clone(), value.clone());
        }
        let mut out = serde_json::to_string_pretty(&Value::Object(map))?;
        out.push('\n');
        Ok(out)
    }
}

/// Parse one `messages` element; a failed typed parse preserves the element
/// verbatim instead of erroring or dropping it.
fn record(value: Value) -> Record {
    match Entry::deserialize(&value) {
        Ok(entry) => Record::Message(entry),
        Err(_) => Record::Other(value),
    }
}

/// Remove and return `key` only when it holds a string; other shapes stay in
/// the map as preserved unknowns.
fn take_str(map: &mut Map<String, Value>, key: &str) -> Option<String> {
    if !map.get(key).is_some_and(Value::is_string) {
        return None;
    }
    match map.remove(key) {
        Some(Value::String(s)) => Some(s),
        _ => None,
    }
}
