//! Hermes Agent sessions: `~/.hermes/state.db` and `hermes sessions export`.
//!
//! Hermes stores session metadata and ordered message rows in `SQLite`. Its JSONL
//! exporter emits one JSON object per session, with the message rows nested in
//! a `messages` array; that object is this harness's portable text form. The
//! codec reads active user, assistant, and tool rows, preserving reasoning,
//! function calls, results, stop reasons, and metadata through [`Common`].
//!
//! Native JSON is retained as an opaque [`Value`], so unknown session columns,
//! message columns, and bookkeeping roles survive text round trips. Hermes has
//! no public import command, so the store is intentionally read-only; generated
//! Hermes text can be exported or converted, but is not inserted into a live
//! `state.db`. Multiple same-kind text/reasoning blocks are joined with newlines,
//! and images have no native Hermes message-row slot.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use uuid::Uuid;

#[cfg(feature = "hermes")]
use rusqlite::{Connection, OpenFlags};
#[cfg(feature = "hermes")]
use std::collections::HashMap;
#[cfg(feature = "hermes")]
use std::path::Path;
use std::path::PathBuf;

use crate::common::{Block, ImageSource, Message, Meta, Role, StopReason, Tool, ToolOutput};
use crate::error::{Error, Result};
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

/// The Hermes Agent harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hermes;

impl Harness for Hermes {
    const NAME: &'static str = "hermes";
    type Body = Value;
}

impl TextCodec for Hermes {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let body: Value = serde_json::from_str(text)?;
        let meta = meta_from_export(&body);
        Ok(Transcript::new(meta, body))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Ok(serde_json::to_string_pretty(&transcript.body)?)
    }
}

impl Codec for Hermes {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            messages_from_export(&transcript.body, &transcript.meta),
        ))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        let body = export_from_messages(&transcript.meta, &transcript.body);
        let mut meta = transcript.meta.clone();
        if meta.id.is_empty() {
            meta.id = body
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
        }
        Ok(Transcript::new(meta, body))
    }
}

/// Read-only access to Hermes Agent's canonical `state.db` session store.
#[derive(Debug, Clone)]
pub struct HermesStore {
    pub db_path: PathBuf,
}

impl HermesStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: path.into(),
        }
    }

    /// Resolve `$HERMES_HOME/state.db`, falling back to `~/.hermes/state.db`.
    #[must_use]
    pub fn default_root() -> Option<Self> {
        std::env::var_os("HERMES_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| super::home_dir().map(|home| home.join(".hermes")))
            .map(|root| Self::new(root.join("state.db")))
    }
}

#[cfg(feature = "hermes")]
impl Store for HermesStore {
    type H = Hermes;
    type Ref = String;

    fn discover(&self) -> Result<Vec<Discovered<String>>> {
        if !self.db_path.is_file() {
            return Ok(Vec::new());
        }
        let conn = open_read_only(&self.db_path)?;
        let rows = query_rows(&conn, "SELECT * FROM sessions", &[])?;
        Ok(rows
            .into_iter()
            .filter(|row| row.get("archived").and_then(Value::as_i64) != Some(1))
            .filter_map(|row| {
                let body = Value::Object(row);
                let meta = meta_from_export(&body);
                (!meta.id.is_empty()).then(|| Discovered {
                    reference: meta.id.clone(),
                    meta,
                })
            })
            .collect())
    }

    fn load(&self, reference: &String) -> Result<Transcript<Hermes>> {
        let conn = open_read_only(&self.db_path)?;
        let mut sessions = query_rows(
            &conn,
            "SELECT * FROM sessions WHERE id = ?1",
            &[reference.as_str()],
        )?;
        let mut session = sessions.pop().ok_or_else(|| Error::Malformed {
            harness: Hermes::NAME,
            detail: format!(
                "session `{reference}` not found in {}",
                self.db_path.display()
            ),
        })?;
        let mut messages = query_rows(
            &conn,
            "SELECT * FROM messages WHERE session_id = ?1 AND active = 1 ORDER BY id",
            &[reference.as_str()],
        )?;
        for row in &mut messages {
            normalize_export_message(row);
        }
        session.insert(
            "messages".to_string(),
            Value::Array(messages.into_iter().map(Value::Object).collect()),
        );
        let body = Value::Object(session);
        Ok(Transcript::new(meta_from_export(&body), body))
    }

    fn save(&self, _transcript: &Transcript<Hermes>) -> Result<Saved<String>> {
        Err(read_only_error())
    }

    fn delete(&self, _reference: &String) -> Result<()> {
        Err(read_only_error())
    }

    fn fingerprints(&self, refs: &[String]) -> Result<HashMap<String, String>> {
        let mut output = HashMap::with_capacity(refs.len());
        for reference in refs {
            let transcript = self.load(reference)?;
            output.insert(reference.clone(), stable_hash(&transcript.body));
        }
        Ok(output)
    }
}

#[cfg(not(feature = "hermes"))]
impl Store for HermesStore {
    type H = Hermes;
    type Ref = String;

    fn discover(&self) -> Result<Vec<Discovered<String>>> {
        Ok(Vec::new())
    }

    fn load(&self, _reference: &String) -> Result<Transcript<Hermes>> {
        Err(sqlite_unavailable())
    }

    fn save(&self, _transcript: &Transcript<Hermes>) -> Result<Saved<String>> {
        Err(sqlite_unavailable())
    }

    fn delete(&self, _reference: &String) -> Result<()> {
        Err(sqlite_unavailable())
    }
}

#[cfg(feature = "hermes")]
fn open_read_only(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlite_error)
}

#[cfg(feature = "hermes")]
fn query_rows(conn: &Connection, sql: &str, args: &[&str]) -> Result<Vec<Map<String, Value>>> {
    let mut statement = conn.prepare(sql).map_err(sqlite_error)?;
    let names: Vec<String> = statement
        .column_names()
        .into_iter()
        .map(String::from)
        .collect();
    let mut rows = statement
        .query(rusqlite::params_from_iter(args.iter()))
        .map_err(sqlite_error)?;
    let mut output = Vec::new();
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        let mut object = Map::with_capacity(names.len());
        for (index, name) in names.iter().enumerate() {
            let value = row.get_ref(index).map_err(sqlite_error)?;
            object.insert(name.clone(), sqlite_value(value));
        }
        output.push(object);
    }
    Ok(output)
}

#[cfg(feature = "hermes")]
fn sqlite_value(value: rusqlite::types::ValueRef<'_>) -> Value {
    match value {
        rusqlite::types::ValueRef::Null => Value::Null,
        rusqlite::types::ValueRef::Integer(number) => json!(number),
        rusqlite::types::ValueRef::Real(number) => json!(number),
        rusqlite::types::ValueRef::Text(bytes) => {
            Value::String(String::from_utf8_lossy(bytes).into_owned())
        }
        rusqlite::types::ValueRef::Blob(bytes) => json!({"$blob_hex": hex(bytes)}),
    }
}

#[cfg(feature = "hermes")]
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(feature = "hermes")]
fn normalize_export_message(row: &mut Map<String, Value>) {
    // `HermesState.get_messages` decodes sentinel-prefixed structured content
    // and tool_calls, while leaving ordinary JSON-looking tool-result strings
    // as text. Match that public export contract rather than exposing SQLite's
    // storage representation.
    if let Some(value) = row.get_mut("content")
        && let Value::String(content) = value
        && let Some(encoded) = content.strip_prefix("\0json:")
        && let Ok(decoded) = serde_json::from_str(encoded)
    {
        *value = decoded;
    }
    if let Some(value) = row.get_mut("tool_calls")
        && let Value::String(text) = value
        && !text.is_empty()
    {
        *value = serde_json::from_str(text).unwrap_or_else(|_| json!([]));
    }
}

#[cfg(feature = "hermes")]
#[allow(clippy::needless_pass_by_value)]
fn sqlite_error(error: rusqlite::Error) -> Error {
    Error::Malformed {
        harness: Hermes::NAME,
        detail: error.to_string(),
    }
}

#[cfg(feature = "hermes")]
fn read_only_error() -> Error {
    Error::Unconvertible {
        harness: Hermes::NAME,
        detail:
            "Hermes state.db support is read-only; use `hermes sessions export` for portable output"
                .to_string(),
    }
}

#[cfg(not(feature = "hermes"))]
fn sqlite_unavailable() -> Error {
    Error::Unconvertible {
        harness: Hermes::NAME,
        detail: "Hermes store support requires the `hermes` feature for SQLite".to_string(),
    }
}

fn meta_from_export(body: &Value) -> Meta {
    let string = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    Meta {
        id: string("id").unwrap_or_default(),
        timestamp: body
            .get("started_at")
            .and_then(Value::as_f64)
            .and_then(timestamp_from_seconds)
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH),
        cwd: string("cwd"),
        git_branch: string("git_branch"),
        title: string("title"),
        cli_version: string("cli_version"),
        model: string("model"),
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn timestamp_from_seconds(seconds: f64) -> Option<DateTime<Utc>> {
    if !seconds.is_finite() {
        return None;
    }
    let whole = seconds.floor();
    let nanos = ((seconds - whole) * 1_000_000_000.0).round();
    let secs = whole as i64;
    let nanos = nanos.clamp(0.0, 999_999_999.0) as u32;
    DateTime::from_timestamp(secs, nanos)
}

fn message_timestamp(row: &Value, fallback: DateTime<Utc>) -> DateTime<Utc> {
    row.get("timestamp")
        .and_then(Value::as_f64)
        .and_then(timestamp_from_seconds)
        .unwrap_or(fallback)
}

fn messages_from_export(body: &Value, meta: &Meta) -> Vec<Message> {
    body.get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|row| row.get("active").and_then(Value::as_i64) != Some(0))
        .filter_map(|row| message_from_row(row, meta))
        .collect()
}

fn message_from_row(row: &Value, meta: &Meta) -> Option<Message> {
    let timestamp = message_timestamp(row, meta.timestamp);
    match row.get("role").and_then(Value::as_str) {
        Some("user") => {
            let content = content_blocks(row.get("content"));
            (!content.is_empty()).then_some(Message {
                role: Role::User,
                content,
                timestamp,
                model: None,
                stop_reason: None,
                usage: None,
            })
        }
        Some("assistant") => assistant_message(row, meta, timestamp),
        Some("tool") => tool_result_message(row, timestamp),
        // `session_meta`, system, and future bookkeeping rows remain in the
        // native body but carry no conversational turn.
        _ => None,
    }
}

fn assistant_message(row: &Value, meta: &Meta, timestamp: DateTime<Utc>) -> Option<Message> {
    let mut content = Vec::new();
    let reasoning = reasoning_text(row).filter(|s| !s.trim().is_empty());
    let encrypted = reasoning_encrypted(row);
    if reasoning.is_some() || encrypted.is_some() {
        content.push(Block::Thinking {
            text: reasoning.unwrap_or_default(),
            signature: None,
            encrypted,
        });
    }
    content.extend(content_blocks(row.get("content")));
    if let Some(calls) = tool_calls(row) {
        content.extend(calls);
    }
    if content.is_empty() {
        return None;
    }
    Some(Message {
        role: Role::Assistant,
        content,
        timestamp,
        model: meta.model.clone(),
        stop_reason: row
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(parse_finish_reason),
        usage: None,
    })
}

fn content_blocks(value: Option<&Value>) -> Vec<Block> {
    match value {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(text)) if text.trim().is_empty() => Vec::new(),
        Some(Value::String(text)) => vec![Block::Text { text: text.clone() }],
        Some(Value::Array(parts)) => parts.iter().filter_map(content_part).collect(),
        Some(value) => content_part(value).into_iter().collect(),
    }
}

fn content_part(part: &Value) -> Option<Block> {
    match part.get("type").and_then(Value::as_str) {
        Some("text") => part
            .get("text")
            .and_then(Value::as_str)
            .map(|text| Block::Text {
                text: text.to_string(),
            }),
        Some("image_url") => part
            .pointer("/image_url/url")
            .and_then(Value::as_str)
            .and_then(data_url_image)
            .or(Some(Block::Text {
                text: value_text(part),
            })),
        _ => Some(Block::Text {
            text: value_text(part),
        }),
    }
}

fn data_url_image(url: &str) -> Option<Block> {
    let value = url.strip_prefix("data:")?;
    let (media_type, data) = value.split_once(";base64,")?;
    Some(Block::Image {
        source: ImageSource {
            source_type: "base64".to_string(),
            media_type: media_type.to_string(),
            data: data.to_string(),
        },
    })
}

fn reasoning_text(row: &Value) -> Option<String> {
    ["reasoning_content", "reasoning"]
        .into_iter()
        .find_map(|key| text_content(row.get(key)).filter(|s| !s.is_empty()))
}

fn reasoning_encrypted(row: &Value) -> Option<String> {
    ["codex_reasoning_items", "reasoning_details"]
        .into_iter()
        .find_map(|key| row.get(key).filter(|v| !v.is_null()))
        .map(value_text)
}

fn tool_calls(row: &Value) -> Option<Vec<Block>> {
    let value = row.get("tool_calls")?;
    let parsed;
    let calls = if let Some(array) = value.as_array() {
        array
    } else {
        let text = value.as_str()?;
        parsed = serde_json::from_str::<Value>(text).ok()?;
        parsed.as_array()?
    };
    Some(calls.iter().filter_map(tool_call_block).collect())
}

fn tool_call_block(call: &Value) -> Option<Block> {
    let function = call.get("function")?;
    let name = function.get("name").and_then(Value::as_str)?;
    let input = match function.get("arguments") {
        Some(Value::String(text)) => {
            serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone()))
        }
        Some(value) => value.clone(),
        None => Value::Object(Map::new()),
    };
    let id = call
        .get("id")
        .or_else(|| call.get("call_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let (canonical_name, canonical_input) = normalize_tool(name, input);
    Some(Block::ToolUse {
        id,
        tool: Tool::from_canonical(&canonical_name, canonical_input),
    })
}

fn normalize_tool(name: &str, input: Value) -> (String, Value) {
    let Some(mut object) = input.as_object().cloned() else {
        return (name.to_string(), input);
    };
    match name {
        "read_file" => {
            rename_key(&mut object, "path", "file_path");
            ("Read".to_string(), Value::Object(object))
        }
        "write_file" => {
            rename_key(&mut object, "path", "file_path");
            ("Write".to_string(), Value::Object(object))
        }
        "patch" if object.get("mode").and_then(Value::as_str) == Some("replace") => {
            object.remove("mode");
            rename_key(&mut object, "path", "file_path");
            ("Edit".to_string(), Value::Object(object))
        }
        "terminal" => {
            rename_key(&mut object, "background", "run_in_background");
            if let Some(timeout) = object.get_mut("timeout")
                && let Some(seconds) = timeout.as_u64()
            {
                *timeout = json!(seconds.saturating_mul(1000));
                rename_key(&mut object, "timeout", "timeout_ms");
            }
            ("Bash".to_string(), Value::Object(object))
        }
        _ => (name.to_string(), Value::Object(object)),
    }
}

fn rename_key(object: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = object.remove(from) {
        object.insert(to.to_string(), value);
    }
}

fn tool_result_message(row: &Value, timestamp: DateTime<Utc>) -> Option<Message> {
    let tool_use_id = row.get("tool_call_id").and_then(Value::as_str)?.to_string();
    let content = row
        .get("content")
        .cloned()
        .map_or_else(|| ToolOutput::Text(String::new()), tool_output);
    Some(Message {
        role: Role::User,
        content: vec![Block::ToolResult {
            tool_use_id,
            is_error: tool_result_is_error(row, &content),
            content,
        }],
        timestamp,
        model: None,
        stop_reason: None,
        usage: None,
    })
}

fn tool_output(value: Value) -> ToolOutput {
    match value {
        Value::String(text) => serde_json::from_str::<Value>(&text)
            .ok()
            .filter(|v| v.is_object() || v.is_array())
            .map_or(ToolOutput::Text(text), ToolOutput::Json),
        other => ToolOutput::Json(other),
    }
}

fn tool_result_is_error(row: &Value, output: &ToolOutput) -> bool {
    if row.get("effect_disposition").and_then(Value::as_str) == Some("denied") {
        return true;
    }
    match output {
        ToolOutput::Json(value) => {
            value.get("success").and_then(Value::as_bool) == Some(false)
                || value
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .is_some_and(|code| code != 0)
                || value.get("error").is_some_and(|error| !error.is_null())
        }
        ToolOutput::Text(_) => false,
    }
}

fn text_content(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(value_text(other)),
    }
}

fn value_text(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), String::from)
}

fn parse_finish_reason(reason: &str) -> StopReason {
    match reason {
        "stop" | "end_turn" => StopReason::EndTurn,
        "tool_calls" | "tool_use" => StopReason::ToolUse,
        "length" | "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "aborted" | "cancelled" => StopReason::Aborted,
        "error" => StopReason::Error,
        other => StopReason::Other(other.to_string()),
    }
}

fn finish_reason(reason: Option<&StopReason>, has_tools: bool) -> &'static str {
    match reason {
        Some(StopReason::MaxTokens) => "length",
        Some(StopReason::StopSequence) => "stop_sequence",
        Some(StopReason::Aborted) => "aborted",
        Some(StopReason::Error) => "error",
        Some(StopReason::EndTurn | StopReason::Other(_)) | None if !has_tools => "stop",
        Some(StopReason::ToolUse | StopReason::EndTurn | StopReason::Other(_)) | None => {
            "tool_calls"
        }
    }
}

fn stable_hash<T: serde::Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    Uuid::new_v5(&Uuid::NAMESPACE_URL, &bytes)
        .simple()
        .to_string()
}

fn export_from_messages(meta: &Meta, messages: &[Message]) -> Value {
    let session_id = if meta.id.is_empty() {
        format!(
            "{}_{}",
            meta.timestamp.format("%Y%m%d_%H%M%S"),
            stable_hash(&(meta, messages))
        )
    } else {
        meta.id.clone()
    };
    let rows = rows_from_messages(&session_id, messages);

    json!({
        "id": session_id,
        "source": "txcript",
        "model": meta.model,
        "started_at": timestamp_seconds(meta.timestamp),
        "cwd": meta.cwd,
        "git_branch": meta.git_branch,
        "title": meta.title,
        "message_count": rows.len(),
        "tool_call_count": rows.iter()
            .filter(|row| row.get("role").and_then(Value::as_str) == Some("assistant"))
            .filter_map(|row| row.get("tool_calls").and_then(Value::as_array))
            .map(Vec::len)
            .sum::<usize>(),
        "messages": rows
    })
}

fn rows_from_messages(session_id: &str, messages: &[Message]) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut next_id = 1_u64;
    for message in messages {
        match message.role {
            Role::User => {
                for block in &message.content {
                    if let Block::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } = block
                    {
                        rows.push(json!({
                            "id": next_id,
                            "session_id": session_id,
                            "role": "tool",
                            "content": output_value(content),
                            "tool_call_id": tool_use_id,
                            "effect_disposition": if *is_error { "denied" } else { "allowed" },
                            "timestamp": timestamp_seconds(message.timestamp),
                            "observed": 0,
                            "active": 1,
                            "compacted": 0
                        }));
                        next_id += 1;
                    }
                }
                let native = native_content(&message.content);
                if !native.is_null() {
                    rows.push(base_row(
                        next_id,
                        session_id,
                        "user",
                        &native,
                        message.timestamp,
                    ));
                    next_id += 1;
                }
            }
            Role::Assistant => {
                let mut reasoning = Vec::new();
                let mut encrypted = None;
                let mut calls = Vec::new();
                for block in &message.content {
                    match block {
                        Block::Thinking {
                            text: value,
                            encrypted: value_encrypted,
                            ..
                        } => {
                            reasoning.push(value.clone());
                            encrypted = encrypted.or_else(|| value_encrypted.clone());
                        }
                        Block::ToolUse { id, tool } => calls.push(tool_call_value(id, tool)),
                        Block::Text { .. } | Block::Image { .. } | Block::ToolResult { .. } => {}
                    }
                }
                let has_tools = !calls.is_empty();
                let native = native_content(&message.content);
                let mut row =
                    base_row(next_id, session_id, "assistant", &native, message.timestamp);
                if let Some(object) = row.as_object_mut() {
                    if !reasoning.is_empty() {
                        object.insert("reasoning_content".into(), json!(reasoning.join("\n")));
                    }
                    if let Some(value) = encrypted {
                        object.insert("codex_reasoning_items".into(), json!(value));
                    }
                    if has_tools {
                        object.insert("tool_calls".into(), Value::Array(calls));
                    }
                    object.insert(
                        "finish_reason".into(),
                        json!(finish_reason(message.stop_reason.as_ref(), has_tools)),
                    );
                }
                rows.push(row);
                next_id += 1;
            }
        }
    }
    rows
}

fn native_content(blocks: &[Block]) -> Value {
    let has_image = blocks
        .iter()
        .any(|block| matches!(block, Block::Image { .. }));
    if !has_image {
        let text = blocks
            .iter()
            .filter_map(|block| match block {
                Block::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        return if text.is_empty() {
            Value::Null
        } else {
            Value::String(text)
        };
    }
    Value::Array(
        blocks
            .iter()
            .filter_map(|block| match block {
                Block::Text { text } => Some(json!({"type": "text", "text": text})),
                Block::Image { source } => Some(json!({
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:{};base64,{}", source.media_type, source.data)
                    }
                })),
                _ => None,
            })
            .collect(),
    )
}

fn base_row(
    id: u64,
    session_id: &str,
    role: &str,
    content: &Value,
    timestamp: DateTime<Utc>,
) -> Value {
    json!({
        "id": id,
        "session_id": session_id,
        "role": role,
        "content": content,
        "timestamp": timestamp_seconds(timestamp),
        "observed": 0,
        "active": 1,
        "compacted": 0
    })
}

#[allow(clippy::cast_precision_loss)]
fn timestamp_seconds(timestamp: DateTime<Utc>) -> f64 {
    timestamp.timestamp() as f64 + f64::from(timestamp.timestamp_subsec_nanos()) / 1_000_000_000.0
}

fn tool_call_value(id: &str, tool: &Tool) -> Value {
    let (name, input) = denormalize_tool(tool);
    json!({
        "id": id,
        "call_id": id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": input.to_string()
        }
    })
}

fn denormalize_tool(tool: &Tool) -> (String, Value) {
    match tool {
        Tool::Read { .. } => {
            let (_, mut input) = tool.to_canonical();
            if let Some(object) = input.as_object_mut() {
                rename_key(object, "file_path", "path");
            }
            ("read_file".to_string(), input)
        }
        Tool::Write { .. } => {
            let (_, mut input) = tool.to_canonical();
            if let Some(object) = input.as_object_mut() {
                rename_key(object, "file_path", "path");
            }
            ("write_file".to_string(), input)
        }
        Tool::Edit { .. } => {
            let (_, mut input) = tool.to_canonical();
            if let Some(object) = input.as_object_mut() {
                rename_key(object, "file_path", "path");
                object.insert("mode".into(), json!("replace"));
            }
            ("patch".to_string(), input)
        }
        Tool::Bash { .. } => {
            let (_, mut input) = tool.to_canonical();
            if let Some(object) = input.as_object_mut() {
                rename_key(object, "run_in_background", "background");
                if let Some(milliseconds) = object.remove("timeout_ms") {
                    let seconds = milliseconds
                        .as_u64()
                        .map(|ms| ms / 1000)
                        .unwrap_or_default();
                    object.insert("timeout".into(), json!(seconds));
                }
            }
            ("terminal".to_string(), input)
        }
        // Commands keep their canonical slash-prefixed name; no Hermes tool
        // name may collide with it, so the round trip is the identity.
        Tool::MultiEdit { .. } | Tool::Raw { .. } | Tool::Command { .. } => tool.to_canonical(),
    }
}

fn output_value(output: &ToolOutput) -> Value {
    match output {
        ToolOutput::Text(text) => Value::String(text.clone()),
        ToolOutput::Json(value) => value.clone(),
    }
}
