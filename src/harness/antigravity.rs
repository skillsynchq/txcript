//! Antigravity CLI (`agy`) conversations:
//! `~/.gemini/antigravity-cli/conversations/<id>.db`.
//!
//! Antigravity persists a conversation as a small `SQLite` database of
//! protobuf-encoded trajectory steps (`gemini_coder.Step` — a step-type tag,
//! a `CortexStepMetadata` envelope carrying timestamps / the tool call /
//! model usage, and one payload message per step kind), plus a `brain/<id>/`
//! working directory whose `.system_generated/logs/transcript*.jsonl` files
//! mirror the conversation for display. The database is the resume carrier:
//! `agy --conversation=<id>` rebuilds model context from the steps alone and
//! silently starts a fresh session when the database is missing, while a
//! missing `brain/<id>/` directory wedges the CLI — `save` therefore always
//! writes the database *and* creates the brain directory.
//!
//! Losslessness is at the blob level: every table row round-trips byte-for-
//! byte through [`AntigravityDb`]; only the step payloads the codec
//! understands are interpreted, with hand-rolled protobuf read/write for the
//! handful of field numbers involved (recovered from the descriptors embedded
//! in the `agy` binary).
//!
//! Known representational losses through `Common`:
//! - `toolAction`/`toolSummary` display strings inside typed tool arguments
//!   (and their `CortexStepMetadata` mirrors) are dropped; `Tool::Raw`
//!   passthrough keeps them.
//! - Per-tool-call binary `thinking_signature` blobs are not representable
//!   and are regenerated as absent.
//! - Edit/Write tool results are derived data in Antigravity (diff chunks,
//!   uris); free-form result text from other harnesses is replaced by the
//!   canonical derived JSON (`{"file": …, "created"|"edited": true}`).
//! - Assistant-side images have no native slot and flatten to placeholder
//!   text; user images ride `CortexStepUserInput.images`.
//! - Numeric model ids surface as `model-<n>` strings; foreign model names
//!   cannot be mapped back to Antigravity's enum and are omitted on write.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::common::{Block, ImageSource, Message, Meta, Role, StopReason, Tool, ToolOutput, Usage};
use crate::error::{Error, Result};
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

#[cfg(feature = "opencode")]
use rusqlite::{Connection, OpenFlags, params};
#[cfg(feature = "opencode")]
use std::fs;
#[cfg(feature = "opencode")]
use std::path::Path;

/// The Antigravity harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Antigravity;

impl Harness for Antigravity {
    const NAME: &'static str = "antigravity";
    type Body = AntigravityDb;
}

/// Faithful in-memory representation of one `conversations/<id>.db`, plus the
/// brain transcript sidecar logs when present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AntigravityDb {
    pub trajectory_meta: Vec<TrajectoryMetaRow>,
    pub steps: Vec<StepRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gen_metadata: Vec<SizedBlobRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub executor_metadata: Vec<IndexedBlobRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_references: Vec<IndexedBlobRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub battle_mode_infos: Vec<IndexedBlobRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trajectory_metadata_blob: Vec<KeyedBlobRow>,
    /// `brain/<id>/.system_generated/logs/transcript.jsonl`, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    /// `brain/<id>/.system_generated/logs/transcript_full.jsonl`, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_full: Option<String>,
}

/// One row of `trajectory_meta`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryMetaRow {
    pub trajectory_id: String,
    pub cascade_id: String,
    pub trajectory_type: i64,
    pub source: i64,
}

/// One row of `steps`. Blob columns hold protobuf bytes, hex-encoded in the
/// text form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRow {
    pub idx: i64,
    pub step_type: i64,
    pub status: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_subtrajectory: bool,
    #[serde(with = "serde_hex")]
    pub metadata: Vec<u8>,
    #[serde(with = "serde_hex", default, skip_serializing_if = "Vec::is_empty")]
    pub error_details: Vec<u8>,
    #[serde(with = "serde_hex", default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<u8>,
    #[serde(with = "serde_hex", default, skip_serializing_if = "Vec::is_empty")]
    pub task_details: Vec<u8>,
    #[serde(with = "serde_hex", default, skip_serializing_if = "Vec::is_empty")]
    pub render_info: Vec<u8>,
    #[serde(with = "serde_hex")]
    pub step_payload: Vec<u8>,
    #[serde(default)]
    pub step_format: i64,
}

/// One row of `gen_metadata` (has an extra `size` column).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizedBlobRow {
    pub idx: i64,
    #[serde(with = "serde_hex")]
    pub data: Vec<u8>,
    pub size: i64,
}

/// One row of `executor_metadata` / `parent_references` / `battle_mode_infos`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedBlobRow {
    pub idx: i64,
    #[serde(with = "serde_hex")]
    pub data: Vec<u8>,
}

/// One row of `trajectory_metadata_blob` (`id` is `"main"` in practice).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyedBlobRow {
    pub id: String,
    #[serde(with = "serde_hex")]
    pub data: Vec<u8>,
}

// -- step-type and enum constants (gemini_coder.Step / exa.cortex_pb) -----

const STEP_VIEW_FILE: i64 = 8;
const STEP_LIST_DIRECTORY: i64 = 9;
const STEP_USER_INPUT: i64 = 14;
const STEP_PLANNER_RESPONSE: i64 = 15;
const STEP_RUN_COMMAND: i64 = 21;
const STEP_CHECKPOINT: i64 = 23;
const STEP_MCP_TOOL: i64 = 38;
const STEP_CODE_ACTION: i64 = 5;
const STEP_GREP_SEARCH: i64 = 7;
const STEP_CONVERSATION_HISTORY: i64 = 98;
const STEP_SYSTEM_MESSAGE: i64 = 101;
const STEP_EPHEMERAL_MESSAGE: i64 = 90;
const STEP_GENERIC: i64 = 132;

const STATUS_PENDING: i64 = 1;
const STATUS_DONE: i64 = 3;
const STATUS_CANCELED: i64 = 6;
const STATUS_ERROR: i64 = 7;

// Payload field number inside `gemini_coder.Step` per step type.
fn payload_field(step_type: i64) -> Option<u32> {
    match step_type {
        STEP_VIEW_FILE => Some(14),
        STEP_LIST_DIRECTORY => Some(15),
        STEP_USER_INPUT => Some(19),
        STEP_PLANNER_RESPONSE => Some(20),
        STEP_RUN_COMMAND => Some(28),
        STEP_CHECKPOINT => Some(30),
        STEP_MCP_TOOL => Some(47),
        STEP_CODE_ACTION => Some(10),
        STEP_GREP_SEARCH => Some(13),
        STEP_CONVERSATION_HISTORY => Some(111),
        STEP_SYSTEM_MESSAGE => Some(114),
        STEP_EPHEMERAL_MESSAGE => Some(103),
        STEP_GENERIC => Some(140),
        _ => None,
    }
}

impl Codec for Antigravity {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            db_to_messages(&transcript.body, &transcript.meta),
        ))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            db_from_messages(&transcript.meta, &transcript.body),
        ))
    }
}

impl TextCodec for Antigravity {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let body: AntigravityDb = serde_json::from_str(text)?;
        let meta = meta_from_db(&body);
        Ok(Transcript::new(meta, body))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Ok(serde_json::to_string_pretty(&transcript.body)?)
    }
}

// -- protobuf wire helpers -------------------------------------------------

/// One decoded protobuf field value.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PbValue<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed32([u8; 4]),
    Fixed64([u8; 8]),
}

/// Decode the fields of one message. Tolerant: on any malformed byte the
/// fields parsed so far are returned — a corrupt blob yields a shorter view,
/// never an error.
fn pb_fields(buf: &[u8]) -> Vec<(u32, PbValue<'_>)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < buf.len() {
        let Some((tag, next)) = pb_read_varint(buf, i) else {
            break;
        };
        let field = u32::try_from(tag >> 3).unwrap_or(0);
        let parsed = match tag & 7 {
            0 => pb_read_varint(buf, next).map(|(v, j)| (PbValue::Varint(v), j)),
            2 => pb_read_varint(buf, next).and_then(|(len, j)| {
                let end = j.checked_add(usize::try_from(len).ok()?)?;
                (end <= buf.len()).then(|| (PbValue::Bytes(&buf[j..end]), end))
            }),
            5 => buf
                .get(next..next + 4)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .map(|b| (PbValue::Fixed32(b), next + 4)),
            1 => buf
                .get(next..next + 8)
                .and_then(|b| <[u8; 8]>::try_from(b).ok())
                .map(|b| (PbValue::Fixed64(b), next + 8)),
            _ => None,
        };
        match parsed {
            Some((value, j)) if field > 0 => {
                out.push((field, value));
                i = j;
            }
            // A zero field number or malformed body ends the readable prefix.
            Some(_) | None => i = buf.len(),
        }
    }
    out
}

fn pb_read_varint(buf: &[u8], mut i: usize) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    let mut shift = 0u32;
    while shift < 64 {
        let byte = *buf.get(i)?;
        value |= u64::from(byte & 0x7f) << shift;
        i += 1;
        if byte & 0x80 == 0 {
            return Some((value, i));
        }
        shift += 7;
    }
    None
}

/// First occurrence of a length-delimited subfield.
fn pb_sub<'a>(fields: &[(u32, PbValue<'a>)], field: u32) -> Option<&'a [u8]> {
    fields.iter().find_map(|(f, v)| match v {
        PbValue::Bytes(b) if *f == field => Some(*b),
        _ => None,
    })
}

/// Every occurrence of a length-delimited subfield, in order.
fn pb_subs<'a>(fields: &[(u32, PbValue<'a>)], field: u32) -> Vec<&'a [u8]> {
    fields
        .iter()
        .filter_map(|(f, v)| match v {
            PbValue::Bytes(b) if *f == field => Some(*b),
            _ => None,
        })
        .collect()
}

fn pb_str(fields: &[(u32, PbValue<'_>)], field: u32) -> Option<String> {
    pb_sub(fields, field).and_then(|b| std::str::from_utf8(b).ok().map(String::from))
}

fn pb_uint(fields: &[(u32, PbValue<'_>)], field: u32) -> Option<u64> {
    fields.iter().find_map(|(f, v)| match v {
        PbValue::Varint(n) if *f == field => Some(*n),
        _ => None,
    })
}

/// `google.protobuf.Timestamp {1: seconds, 2: nanos}`.
fn pb_timestamp(buf: &[u8]) -> Option<DateTime<Utc>> {
    let fields = pb_fields(buf);
    let secs = i64::try_from(pb_uint(&fields, 1)?).ok()?;
    let nanos = u32::try_from(pb_uint(&fields, 2).unwrap_or(0)).unwrap_or(0);
    DateTime::from_timestamp(secs, nanos)
}

fn pb_key(out: &mut Vec<u8>, field: u32, wire_type: u8) {
    pb_varint(out, (u64::from(field) << 3) | u64::from(wire_type));
}

// The casts take the low 7 bits by construction — varint encoding.
#[allow(clippy::cast_possible_truncation)]
fn pb_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn pb_varint_field(out: &mut Vec<u8>, field: u32, value: u64) {
    pb_key(out, field, 0);
    pb_varint(out, value);
}

fn pb_len(out: &mut Vec<u8>, field: u32, bytes: &[u8]) {
    pb_key(out, field, 2);
    pb_varint(out, bytes.len() as u64);
    out.extend(bytes);
}

fn pb_string(out: &mut Vec<u8>, field: u32, value: &str) {
    if !value.is_empty() {
        pb_len(out, field, value.as_bytes());
    }
}

fn pb_timestamp_field(out: &mut Vec<u8>, field: u32, ts: DateTime<Utc>) {
    let mut inner = Vec::new();
    let secs = ts.timestamp();
    if let Ok(secs) = u64::try_from(secs)
        && secs > 0
    {
        pb_varint_field(&mut inner, 1, secs);
    }
    let nanos = ts.timestamp_subsec_nanos();
    if nanos > 0 {
        pb_varint_field(&mut inner, 2, u64::from(nanos));
    }
    pb_len(out, field, &inner);
}

// -- native -> common ------------------------------------------------------

fn db_to_messages(db: &AntigravityDb, meta: &Meta) -> Vec<Message> {
    let fallback_ts = meta.timestamp;
    let mut steps: Vec<&StepRow> = db.steps.iter().collect();
    steps.sort_by_key(|s| s.idx);

    let mut messages = Vec::new();
    for step in steps {
        let payload = pb_fields(&step.step_payload);
        let meta_fields = pb_sub(&payload, 5).map(pb_fields).unwrap_or_default();
        let ts = pb_sub(&meta_fields, 1)
            .and_then(pb_timestamp)
            .unwrap_or(fallback_ts);
        match step.step_type {
            STEP_USER_INPUT => {
                messages.extend(user_input_message(&payload, ts));
            }
            STEP_PLANNER_RESPONSE => {
                messages.extend(planner_message(&payload, &meta_fields, ts));
            }
            // Bookkeeping: summaries, context markers, injected notices.
            STEP_CHECKPOINT
            | STEP_CONVERSATION_HISTORY
            | STEP_SYSTEM_MESSAGE
            | STEP_EPHEMERAL_MESSAGE => {}
            _ => {
                messages.extend(tool_result_message(step, &payload, &meta_fields, ts));
            }
        }
    }
    messages
}

/// `CortexStepUserInput`: clean text in `user_response` (2) / `items[].text`
/// (3.1) / `query` (1); inline images in `images` (5).
fn user_input_message(payload: &[(u32, PbValue<'_>)], ts: DateTime<Utc>) -> Option<Message> {
    let input = pb_sub(payload, 19).map(pb_fields)?;
    let text = pb_str(&input, 2)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            pb_subs(&input, 3)
                .into_iter()
                .find_map(|item| pb_str(&pb_fields(item), 1).filter(|s| !s.trim().is_empty()))
        })
        .or_else(|| pb_str(&input, 1).filter(|s| !s.trim().is_empty()));

    let mut content = Vec::new();
    if let Some(text) = text {
        content.push(Block::Text { text });
    }
    for image in pb_subs(&input, 5) {
        let fields = pb_fields(image);
        if let Some(data) = pb_str(&fields, 1) {
            content.push(Block::Image {
                source: ImageSource {
                    source_type: "base64".to_string(),
                    media_type: pb_str(&fields, 2).unwrap_or_else(|| "image/png".to_string()),
                    data,
                },
            });
        }
    }
    (!content.is_empty()).then_some(Message {
        role: Role::User,
        content,
        timestamp: ts,
        model: None,
        stop_reason: None,
        usage: None,
    })
}

/// `CortexStepPlannerResponse`: thinking (3, signature 4), response text (1),
/// tool calls (7); model/usage from the step metadata envelope.
fn planner_message(
    payload: &[(u32, PbValue<'_>)],
    meta_fields: &[(u32, PbValue<'_>)],
    ts: DateTime<Utc>,
) -> Option<Message> {
    let planner = pb_sub(payload, 20).map(pb_fields).unwrap_or_default();

    let mut content = Vec::new();
    let thinking = pb_str(&planner, 3).unwrap_or_default();
    if !thinking.trim().is_empty() {
        content.push(Block::Thinking {
            text: thinking,
            signature: pb_str(&planner, 4).filter(|s| !s.is_empty()),
            encrypted: None,
        });
    }
    if let Some(text) = pb_str(&planner, 1).filter(|s| !s.trim().is_empty()) {
        content.push(Block::Text { text });
    }
    for call in pb_subs(&planner, 7) {
        content.extend(tool_use_block(call));
    }

    let usage = pb_sub(meta_fields, 9).map(pb_fields).and_then(|u| {
        let input_tokens = pb_uint(&u, 2).unwrap_or(0);
        let output_tokens = pb_uint(&u, 3).unwrap_or(0);
        let cache_read = pb_uint(&u, 5);
        let cache_write = pb_uint(&u, 4);
        (input_tokens > 0 || output_tokens > 0).then_some(Usage {
            input_tokens,
            output_tokens,
            cache_read_input_tokens: cache_read.filter(|n| *n > 0),
            cache_creation_input_tokens: cache_write.filter(|n| *n > 0),
        })
    });

    (!content.is_empty()).then(|| Message {
        role: Role::Assistant,
        content,
        timestamp: ts,
        model: pb_uint(meta_fields, 11).map(model_name),
        stop_reason: pb_uint(&planner, 12).map(stop_reason_from_native),
        usage,
    })
}

/// `exa.codeium_common_pb.ChatToolCall {1: id, 2: name, 3: arguments_json}`.
fn tool_use_block(call: &[u8]) -> Option<Block> {
    let fields = pb_fields(call);
    let id = pb_str(&fields, 1)?;
    let name = pb_str(&fields, 2)?;
    let args = pb_str(&fields, 3)
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| Value::Object(Map::new()));
    Some(Block::ToolUse {
        id,
        tool: normalize_tool(&name, args),
    })
}

/// Any step whose metadata carries a tool call is that call's result record.
fn tool_result_message(
    step: &StepRow,
    payload: &[(u32, PbValue<'_>)],
    meta_fields: &[(u32, PbValue<'_>)],
    ts: DateTime<Utc>,
) -> Option<Message> {
    let call = pb_sub(meta_fields, 4).map(pb_fields)?;
    let tool_use_id = pb_str(&call, 1)?;
    // A step still pending or running carries no result yet.
    let done = matches!(step.status, STATUS_DONE | STATUS_ERROR | STATUS_CANCELED);
    if !done {
        return None;
    }

    let error_text = error_details_text(&step.error_details);
    let (content, tool_error) = result_output(step.step_type, payload);
    let is_error = step.status == STATUS_ERROR
        || step.status == STATUS_CANCELED
        || error_text.is_some()
        || tool_error;
    let content = match error_text {
        Some(text) => ToolOutput::Text(text),
        None => content,
    };

    Some(Message {
        role: Role::User,
        content: vec![Block::ToolResult {
            tool_use_id,
            content,
            is_error,
        }],
        timestamp: ts,
        model: None,
        stop_reason: None,
        usage: None,
    })
}

/// `exa.cortex_pb.CortexErrorDetails`: the model-facing message wins.
fn error_details_text(error_details: &[u8]) -> Option<String> {
    let fields = pb_fields(error_details);
    pb_str(&fields, 9)
        .or_else(|| pb_str(&fields, 1))
        .or_else(|| pb_str(&fields, 2))
        .filter(|s| !s.trim().is_empty())
}

/// Extract the result content of one tool step. Returns the output plus a
/// tool-level error flag (a nonzero command exit code).
fn result_output(step_type: i64, payload: &[(u32, PbValue<'_>)]) -> (ToolOutput, bool) {
    let field = payload_field(step_type);
    let body = field
        .and_then(|f| pb_sub(payload, f))
        .map(pb_fields)
        .unwrap_or_default();
    match step_type {
        // CortexStepViewFile: content (4), else raw_content (9).
        STEP_VIEW_FILE => {
            let text = pb_str(&body, 4)
                .or_else(|| pb_str(&body, 9))
                .unwrap_or_default();
            (ToolOutput::Text(text), false)
        }
        // CortexStepRunCommand: combined_output.full, exit_code (6).
        STEP_RUN_COMMAND => {
            let text = pb_sub(&body, 21)
                .and_then(|out| pb_str(&pb_fields(out), 1))
                .or_else(|| pb_str(&body, 4))
                .unwrap_or_default();
            let exit = pb_uint(&body, 6).unwrap_or(0);
            (ToolOutput::Text(text), exit != 0)
        }
        // CortexStepListDirectory: results (3) as a JSON listing.
        STEP_LIST_DIRECTORY => {
            let entries: Vec<Value> = pb_subs(&body, 3)
                .into_iter()
                .filter_map(|entry| {
                    let fields = pb_fields(entry);
                    let name = pb_str(&fields, 1)?;
                    let mut obj = json!({ "name": name });
                    if pb_uint(&fields, 2) == Some(1) {
                        obj["is_dir"] = Value::Bool(true);
                    }
                    if let Some(size) = pb_uint(&fields, 4) {
                        obj["size_bytes"] = Value::from(size);
                    }
                    Some(obj)
                })
                .collect();
            let path = pb_str(&body, 1)
                .as_deref()
                .map(strip_file_uri)
                .unwrap_or_default();
            (
                ToolOutput::Json(json!({ "path": path, "entries": entries })),
                false,
            )
        }
        // CortexStepGrepSearch: raw_output (3); grep_error (5) marks failure.
        STEP_GREP_SEARCH => {
            let error = pb_str(&body, 5).filter(|s| !s.trim().is_empty());
            match error {
                Some(text) => (ToolOutput::Text(text), true),
                None => (
                    ToolOutput::Text(pb_str(&body, 3).unwrap_or_default()),
                    false,
                ),
            }
        }
        // CortexStepCodeAction: results are derived; surface the canonical
        // derived form.
        STEP_CODE_ACTION => {
            let spec = pb_sub(&body, 1).map(pb_fields).unwrap_or_default();
            let created = pb_sub(&spec, 2).is_some();
            let uri = pb_sub(&body, 2)
                .map(pb_fields)
                .and_then(|result| {
                    pb_sub(&result, 1)
                        .map(pb_fields)
                        .and_then(|edit| pb_str(&edit, 8))
                })
                .or_else(|| spec_target_uri(&spec))
                .unwrap_or_default();
            let key = if created { "created" } else { "edited" };
            (
                ToolOutput::Json(json!({ "file": strip_file_uri(&uri), key: true })),
                false,
            )
        }
        // CortexStepMcpTool: result_string (3).
        STEP_MCP_TOOL => (
            ToolOutput::Text(pb_str(&body, 3).unwrap_or_default()),
            false,
        ),
        // CortexStepGeneric: result.result (2.1).
        STEP_GENERIC => {
            let text = pb_sub(&body, 2)
                .and_then(|result| pb_str(&pb_fields(result), 1))
                .unwrap_or_default();
            (parse_result_text(text), false)
        }
        // Unmodeled tool steps: the call is preserved, the payload is not
        // interpretable — an empty result rather than a dropped record.
        _ => (ToolOutput::Text(String::new()), false),
    }
}

/// `ActionSpec.command.file.absolute_uri` / `ActionSpec.create_file.path.absolute_uri`.
fn spec_target_uri(spec: &[(u32, PbValue<'_>)]) -> Option<String> {
    let from_path_scope = |buf: &[u8]| pb_str(&pb_fields(buf), 5);
    pb_sub(spec, 2)
        .map(pb_fields)
        .and_then(|create| pb_sub(&create, 2).and_then(from_path_scope))
        .or_else(|| {
            pb_sub(spec, 1)
                .map(pb_fields)
                .and_then(|command| pb_sub(&command, 4).and_then(from_path_scope))
        })
}

/// Result strings written by `from_common` may carry structured JSON; objects
/// and arrays parse back to [`ToolOutput::Json`], anything else stays text.
fn parse_result_text(text: String) -> ToolOutput {
    serde_json::from_str::<Value>(&text)
        .ok()
        .filter(|v| v.is_object() || v.is_array())
        .map_or(ToolOutput::Text(text), ToolOutput::Json)
}

fn strip_file_uri(uri: &str) -> String {
    uri.strip_prefix("file://").unwrap_or(uri).to_string()
}

fn file_uri(path: &str) -> String {
    if path.starts_with("file://") {
        path.to_string()
    } else {
        format!("file://{path}")
    }
}

fn model_name(id: u64) -> String {
    format!("model-{id}")
}

fn model_id(name: &str) -> Option<u64> {
    name.strip_prefix("model-").and_then(|n| n.parse().ok())
}

/// `exa.codeium_common_pb.StopReason` -> canonical.
fn stop_reason_from_native(value: u64) -> StopReason {
    match value {
        2 => StopReason::EndTurn,
        3 => StopReason::MaxTokens,
        10 => StopReason::ToolUse,
        13 => StopReason::Error,
        16 => StopReason::Aborted,
        other => StopReason::Other(format!("stop-{other}")),
    }
}

fn stop_reason_to_native(reason: &StopReason) -> Option<u64> {
    match reason {
        StopReason::EndTurn | StopReason::StopSequence => Some(2),
        StopReason::MaxTokens => Some(3),
        StopReason::ToolUse => Some(10),
        StopReason::Error => Some(13),
        StopReason::Aborted => Some(16),
        StopReason::Other(s) => s.strip_prefix("stop-").and_then(|n| n.parse().ok()),
    }
}

// -- tool normalization ----------------------------------------------------

/// Display-only argument keys mirrored from the step metadata; stripped when
/// (and only when) the remainder maps onto a typed tool.
const DISPLAY_KEYS: [&str; 2] = ["toolAction", "toolSummary"];

// One arm per native tool name; each documents its own key mapping.
#[allow(clippy::too_many_lines)]
fn normalize_tool(name: &str, args: Value) -> Tool {
    let raw = || Tool::Raw {
        tool_name: name.to_string(),
        input: args.clone(),
    };
    match name {
        "run_command" => typed_args(&args, |obj| {
            let command = string_key(obj, "CommandLine")?;
            let workdir = string_key(obj, "Cwd");
            let mut extra = non_display_keys(obj);
            extra.retain(|k| !matches!(k.as_str(), "CommandLine" | "Cwd"));
            // The CLI stamps a fixed 2s async threshold on every command; a
            // non-default value is not representable and stays Raw.
            if obj
                .get("WaitMsBeforeAsync")
                .is_some_and(|v| v == &json!(2000))
            {
                extra.retain(|k| k != "WaitMsBeforeAsync");
            }
            let mut input = json!({ "command": command });
            if let Some(workdir) = workdir {
                input["workdir"] = Value::String(workdir);
            }
            extra
                .is_empty()
                .then(|| Tool::from_canonical("Bash", input))
        })
        .unwrap_or_else(raw),
        "view_file" => typed_args(&args, |obj| {
            let file_path = string_key(obj, "AbsolutePath")?;
            let start = uint_key(obj, "StartLine");
            let end = uint_key(obj, "EndLine");
            let mut extra = non_display_keys(obj);
            extra.retain(|k| !matches!(k.as_str(), "AbsolutePath" | "StartLine" | "EndLine"));
            let limit = match (start, end) {
                (Some(s), Some(e)) if e >= s => Some(e - s + 1),
                (None, Some(e)) => Some(e),
                (_, None) => None,
                // An inverted range is not representable; keep it Raw.
                (Some(_), Some(_)) => {
                    extra.push(String::from("EndLine"));
                    None
                }
            };
            let mut input = json!({ "file_path": file_path });
            if let Some(offset) = start {
                input["offset"] = Value::from(offset);
            }
            if let Some(limit) = limit {
                input["limit"] = Value::from(limit);
            }
            extra
                .is_empty()
                .then(|| Tool::from_canonical("Read", input))
        })
        .unwrap_or_else(raw),
        "write_to_file" => typed_args(&args, |obj| {
            let file_path = string_key(obj, "TargetFile")?;
            let content = string_key(obj, "CodeContent")?;
            let mut extra = non_display_keys(obj);
            // `Description`/`Instruction` are model-authored display strings
            // with no canonical slot; `Overwrite` is the CLI's constant.
            extra.retain(|k| {
                !matches!(
                    k.as_str(),
                    "TargetFile" | "CodeContent" | "Overwrite" | "Description" | "Instruction"
                )
            });
            extra.is_empty().then(|| {
                Tool::from_canonical(
                    "Write",
                    json!({ "file_path": file_path, "content": content }),
                )
            })
        })
        .unwrap_or_else(raw),
        "replace_file_content" => typed_args(&args, |obj| {
            let file_path = string_key(obj, "TargetFile")?;
            let old_string = string_key(obj, "TargetContent")?;
            let new_string = string_key(obj, "ReplacementContent")?;
            let replace_all = obj
                .get("AllowMultiple")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut extra = non_display_keys(obj);
            extra.retain(|k| {
                !matches!(
                    k.as_str(),
                    "TargetFile"
                        | "TargetContent"
                        | "ReplacementContent"
                        | "AllowMultiple"
                        | "StartLine"
                        | "EndLine"
                        | "Description"
                        | "Instruction"
                )
            });
            let mut input = json!({
                "file_path": file_path,
                "old_string": old_string,
                "new_string": new_string,
            });
            if replace_all {
                input["replace_all"] = Value::Bool(true);
            }
            extra
                .is_empty()
                .then(|| Tool::from_canonical("Edit", input))
        })
        .unwrap_or_else(raw),
        // Canonical names arriving back from a generic carrier, and anything
        // else (list_dir, grep_search, mcp tools, …) passes through.
        other => Tool::from_canonical(other, args),
    }
}

/// Run `f` over the args object; `None` (non-object args or a failed match)
/// means "keep Raw".
fn typed_args(args: &Value, f: impl FnOnce(&Map<String, Value>) -> Option<Tool>) -> Option<Tool> {
    args.as_object().and_then(f)
}

fn string_key(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(String::from)
}

fn uint_key(obj: &Map<String, Value>, key: &str) -> Option<u64> {
    obj.get(key).and_then(Value::as_u64)
}

fn non_display_keys(obj: &Map<String, Value>) -> Vec<String> {
    obj.keys()
        .filter(|k| !DISPLAY_KEYS.contains(&k.as_str()))
        .cloned()
        .collect()
}

/// The native tool name and argument JSON for a canonical tool. The inverse
/// of [`normalize_tool`] over its typed range.
fn denormalize_tool(tool: &Tool) -> (String, Value) {
    match tool {
        Tool::Bash {
            command, workdir, ..
        } => {
            let mut args = json!({
                "CommandLine": command,
                "WaitMsBeforeAsync": 2000,
            });
            if let Some(workdir) = workdir {
                args["Cwd"] = Value::String(workdir.clone());
            }
            ("run_command".to_string(), args)
        }
        Tool::Read {
            file_path,
            offset,
            limit,
        } => {
            let mut args = json!({ "AbsolutePath": file_path });
            match (offset, limit) {
                (Some(start), Some(len)) => {
                    args["StartLine"] = Value::from(*start);
                    args["EndLine"] = Value::from(start + len.saturating_sub(1));
                }
                (Some(start), None) => {
                    args["StartLine"] = Value::from(*start);
                }
                (None, Some(len)) => {
                    args["EndLine"] = Value::from(*len);
                }
                (None, None) => {}
            }
            ("view_file".to_string(), args)
        }
        Tool::Write { file_path, content } => (
            "write_to_file".to_string(),
            json!({
                "TargetFile": file_path,
                "CodeContent": content,
                "Overwrite": true,
            }),
        ),
        Tool::Edit {
            file_path,
            old_string,
            new_string,
            replace_all,
        } => {
            let mut args = json!({
                "TargetFile": file_path,
                "TargetContent": old_string,
                "ReplacementContent": new_string,
            });
            if *replace_all {
                args["AllowMultiple"] = Value::Bool(true);
            }
            ("replace_file_content".to_string(), args)
        }
        // MultiEdit and Raw ride the generic carrier under canonical names.
        Tool::MultiEdit { .. } | Tool::Raw { .. } => tool.to_canonical(),
    }
}

// -- common -> native ------------------------------------------------------

const NS: Uuid = Uuid::from_bytes([
    0x5e, 0x1f, 0x9a, 0x27, 0xb3, 0x44, 0x4c, 0x8e, 0x92, 0x6d, 0x0a, 0x71, 0xc5, 0x3f, 0x88, 0x14,
]);

#[derive(Debug, Clone)]
struct PendingResult {
    content: ToolOutput,
    is_error: bool,
    timestamp: DateTime<Utc>,
}

// A linear walk over every message role/block combination; splitting it
// would scatter the step-index bookkeeping.
#[allow(clippy::too_many_lines)]
fn db_from_messages(meta: &Meta, messages: &[Message]) -> AntigravityDb {
    // Generating a fresh id for an id-less session is the sole permitted
    // non-determinism.
    let session_id = if meta.id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        meta.id.clone()
    };
    let trajectory_id =
        Uuid::new_v5(&NS, format!("{session_id}:trajectory").as_bytes()).to_string();

    let results: HashMap<String, PendingResult> = messages
        .iter()
        .flat_map(|m| m.content.iter().map(move |b| (m.timestamp, b)))
        .filter_map(|(timestamp, block)| match block {
            Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some((
                tool_use_id.clone(),
                PendingResult {
                    content: content.clone(),
                    is_error: *is_error,
                    timestamp,
                },
            )),
            // Only results feed the pairing map.
            Block::Text { .. }
            | Block::Thinking { .. }
            | Block::ToolUse { .. }
            | Block::Image { .. } => None,
        })
        .collect();
    let known_uses: std::collections::HashSet<String> = messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|block| match block {
            Block::ToolUse { id, .. } => Some(id.clone()),
            // Only uses mint ids.
            Block::Text { .. }
            | Block::Thinking { .. }
            | Block::ToolResult { .. }
            | Block::Image { .. } => None,
        })
        .collect();

    let mut steps = Vec::new();
    let mut turn: u64 = 0;
    for message in messages {
        match message.role {
            Role::User => {
                let orphan_text = orphan_result_text(&message.content, &known_uses);
                let user_text = user_message_text(&message.content, &orphan_text);
                let images = user_images(&message.content);
                if !user_text.is_empty() || !images.is_empty() {
                    turn += 1;
                    steps.push(user_input_step(
                        &session_id,
                        &trajectory_id,
                        steps.len() as u64,
                        turn,
                        &user_text,
                        &images,
                        message.timestamp,
                    ));
                }
            }
            Role::Assistant => {
                let idx = steps.len() as u64;
                steps.push(planner_step(
                    &session_id,
                    &trajectory_id,
                    idx,
                    turn,
                    message,
                ));
                for block in &message.content {
                    if let Block::ToolUse { id, tool } = block {
                        steps.push(tool_step(
                            &session_id,
                            &trajectory_id,
                            steps.len() as u64,
                            turn,
                            id,
                            tool,
                            results.get(id),
                            message.timestamp,
                        ));
                    }
                }
            }
        }
    }

    let transcript = render_transcript(&steps, messages);
    AntigravityDb {
        trajectory_meta: vec![TrajectoryMetaRow {
            trajectory_id: trajectory_id.clone(),
            cascade_id: session_id.clone(),
            trajectory_type: 4,
            source: 17,
        }],
        steps,
        gen_metadata: Vec::new(),
        executor_metadata: Vec::new(),
        parent_references: Vec::new(),
        battle_mode_infos: Vec::new(),
        trajectory_metadata_blob: vec![KeyedBlobRow {
            id: "main".to_string(),
            data: trajectory_metadata_proto(meta, &session_id),
        }],
        transcript: Some(transcript.clone()),
        transcript_full: Some(transcript),
    }
}

fn user_message_text(blocks: &[Block], orphan_text: &str) -> String {
    let mut parts: Vec<String> = blocks
        .iter()
        .filter_map(|block| match block {
            Block::Text { text } if !text.trim().is_empty() => Some(text.trim().to_string()),
            Block::Thinking { text, .. } if !text.trim().is_empty() => {
                Some(text.trim().to_string())
            }
            // Images ride their own payload field; results ride tool steps.
            Block::Text { .. }
            | Block::Thinking { .. }
            | Block::ToolUse { .. }
            | Block::ToolResult { .. }
            | Block::Image { .. } => None,
        })
        .collect();
    if !orphan_text.is_empty() {
        parts.push(orphan_text.to_string());
    }
    parts.join("\n\n")
}

fn user_images(blocks: &[Block]) -> Vec<&ImageSource> {
    blocks
        .iter()
        .filter_map(|block| match block {
            Block::Image { source } => Some(source),
            // Only images carry image data.
            Block::Text { .. }
            | Block::Thinking { .. }
            | Block::ToolUse { .. }
            | Block::ToolResult { .. } => None,
        })
        .collect()
}

/// Results whose calls are absent from the transcript render as user text —
/// the same downgrade Cursor applies.
fn orphan_result_text(blocks: &[Block], known_uses: &std::collections::HashSet<String>) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            Block::ToolResult { tool_use_id, .. } if known_uses.contains(tool_use_id) => None,
            Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let status = if *is_error { "error" } else { "result" };
                Some(format!(
                    "Tool {status} for {tool_use_id}:\n{}",
                    tool_output_text(content)
                ))
            }
            // Other blocks are not tool results.
            Block::Text { .. }
            | Block::Thinking { .. }
            | Block::ToolUse { .. }
            | Block::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn tool_output_text(out: &ToolOutput) -> String {
    match out {
        ToolOutput::Text(s) => s.clone(),
        ToolOutput::Json(v) => v.to_string(),
    }
}

// Step assembly. Each helper returns a full `StepRow`: envelope fields, the
// metadata mirror column, and the type-specific payload.

struct StepParts {
    step_type: i64,
    status: i64,
    metadata: Vec<u8>,
    payload_field: u32,
    payload_body: Vec<u8>,
}

fn assemble_step(idx: u64, parts: StepParts) -> StepRow {
    let mut step_payload = Vec::new();
    pb_varint_field(
        &mut step_payload,
        1,
        u64::try_from(parts.step_type).unwrap_or(0),
    );
    pb_varint_field(
        &mut step_payload,
        4,
        u64::try_from(parts.status).unwrap_or(3),
    );
    pb_len(&mut step_payload, 5, &parts.metadata);
    pb_len(&mut step_payload, parts.payload_field, &parts.payload_body);
    StepRow {
        idx: i64::try_from(idx).unwrap_or(0),
        step_type: parts.step_type,
        status: parts.status,
        has_subtrajectory: false,
        metadata: parts.metadata,
        error_details: Vec::new(),
        permissions: Vec::new(),
        task_details: Vec::new(),
        render_info: Vec::new(),
        step_payload,
        step_format: 0,
    }
}

/// `CortexStepMetadata` for one emitted step.
#[allow(clippy::too_many_arguments)]
fn step_metadata_proto(
    session_id: &str,
    trajectory_id: &str,
    idx: u64,
    turn: u64,
    source: u64,
    ts: DateTime<Utc>,
    tool_call: Option<&[u8]>,
    message: Option<&Message>,
) -> Vec<u8> {
    let mut out = Vec::new();
    pb_timestamp_field(&mut out, 1, ts);
    pb_varint_field(&mut out, 3, source);
    if let Some(call) = tool_call {
        pb_len(&mut out, 4, call);
    }
    if let Some(usage) = message.and_then(|m| m.usage.as_ref()) {
        let mut stats = Vec::new();
        if let Some(model) = message.and_then(|m| m.model.as_deref()).and_then(model_id) {
            pb_varint_field(&mut stats, 1, model);
        }
        pb_varint_field(&mut stats, 2, usage.input_tokens);
        pb_varint_field(&mut stats, 3, usage.output_tokens);
        if let Some(cache_write) = usage.cache_creation_input_tokens {
            pb_varint_field(&mut stats, 4, cache_write);
        }
        if let Some(cache_read) = usage.cache_read_input_tokens {
            pb_varint_field(&mut stats, 5, cache_read);
        }
        pb_len(&mut out, 9, &stats);
    }
    if let Some(model) = message.and_then(|m| m.model.as_deref()).and_then(model_id) {
        pb_varint_field(&mut out, 11, model);
    }
    let execution_id =
        Uuid::new_v5(&NS, format!("{session_id}:{turn}:exec").as_bytes()).to_string();
    pb_string(&mut out, 12, &execution_id);
    let mut step_info = Vec::new();
    pb_string(&mut step_info, 1, trajectory_id);
    if idx > 0 {
        pb_varint_field(&mut step_info, 2, idx);
    }
    if turn > 0 {
        pb_varint_field(&mut step_info, 3, turn.saturating_sub(1));
    }
    pb_string(&mut step_info, 4, session_id);
    pb_len(&mut out, 20, &step_info);
    pb_varint_field(&mut out, 21, 1);
    out
}

fn user_input_step(
    session_id: &str,
    trajectory_id: &str,
    idx: u64,
    turn: u64,
    text: &str,
    images: &[&ImageSource],
    ts: DateTime<Utc>,
) -> StepRow {
    let mut body = Vec::new();
    pb_string(&mut body, 2, text);
    if !text.is_empty() {
        let mut item = Vec::new();
        pb_string(&mut item, 1, text);
        pb_len(&mut body, 3, &item);
    }
    for image in images {
        let mut data = Vec::new();
        pb_string(&mut data, 1, &image.data);
        pb_string(&mut data, 2, &image.media_type);
        pb_len(&mut body, 5, &data);
    }
    let metadata = step_metadata_proto(session_id, trajectory_id, idx, turn, 4, ts, None, None);
    assemble_step(
        idx,
        StepParts {
            step_type: STEP_USER_INPUT,
            status: STATUS_DONE,
            metadata,
            payload_field: 19,
            payload_body: body,
        },
    )
}

fn planner_step(
    session_id: &str,
    trajectory_id: &str,
    idx: u64,
    turn: u64,
    message: &Message,
) -> StepRow {
    let mut body = Vec::new();
    let text = message
        .content
        .iter()
        .filter_map(|block| match block {
            Block::Text { text } if !text.trim().is_empty() => Some(text.trim().to_string()),
            Block::Image { source } => Some(format!("[image: {}]", source.media_type)),
            // Thinking and tool calls have their own slots; results ride
            // tool steps.
            Block::Text { .. }
            | Block::Thinking { .. }
            | Block::ToolUse { .. }
            | Block::ToolResult { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    pb_string(&mut body, 1, &text);
    let thinking = message.content.iter().find_map(|block| match block {
        Block::Thinking {
            text, signature, ..
        } if !text.trim().is_empty() => Some((text.clone(), signature.clone())),
        // Only substantive thinking has a native slot.
        Block::Text { .. }
        | Block::Thinking { .. }
        | Block::ToolUse { .. }
        | Block::ToolResult { .. }
        | Block::Image { .. } => None,
    });
    if let Some((thinking, signature)) = thinking {
        pb_string(&mut body, 3, &thinking);
        if let Some(signature) = signature {
            pb_string(&mut body, 4, &signature);
        }
    }
    let message_id = Uuid::new_v5(&NS, format!("{session_id}:{idx}:msg").as_bytes());
    pb_string(&mut body, 6, &format!("bot-{message_id}"));
    for block in &message.content {
        if let Block::ToolUse { id, tool } = block {
            pb_len(&mut body, 7, &chat_tool_call_proto(id, tool));
        }
    }
    pb_string(&mut body, 8, &text);
    if let Some(stop) = message.stop_reason.as_ref().and_then(stop_reason_to_native) {
        pb_varint_field(&mut body, 12, stop);
    }
    let metadata = step_metadata_proto(
        session_id,
        trajectory_id,
        idx,
        turn,
        2,
        message.timestamp,
        None,
        Some(message),
    );
    assemble_step(
        idx,
        StepParts {
            step_type: STEP_PLANNER_RESPONSE,
            status: STATUS_DONE,
            metadata,
            payload_field: 20,
            payload_body: body,
        },
    )
}

/// `exa.codeium_common_pb.ChatToolCall` with the tool denormalized to its
/// native name and argument keys.
fn chat_tool_call_proto(id: &str, tool: &Tool) -> Vec<u8> {
    let (name, args) = denormalize_tool(tool);
    let mut out = Vec::new();
    pb_string(&mut out, 1, id);
    pb_string(&mut out, 2, &name);
    pb_string(&mut out, 3, &args.to_string());
    pb_string(&mut out, 9, &name);
    out
}

#[allow(clippy::too_many_arguments)]
fn tool_step(
    session_id: &str,
    trajectory_id: &str,
    idx: u64,
    turn: u64,
    call_id: &str,
    tool: &Tool,
    result: Option<&PendingResult>,
    call_ts: DateTime<Utc>,
) -> StepRow {
    let ts = result.map_or(call_ts, |r| r.timestamp);
    let call = chat_tool_call_proto(call_id, tool);
    let (step_type, payload_field_no, body, status) = tool_step_payload(tool, result);
    let metadata = step_metadata_proto(
        session_id,
        trajectory_id,
        idx,
        turn,
        2,
        ts,
        Some(&call),
        None,
    );
    assemble_step(
        idx,
        StepParts {
            step_type,
            status,
            metadata,
            payload_field: payload_field_no,
            payload_body: body,
        },
    )
}

/// The native payload for one call+result pair. Typed tools regenerate the
/// step kind Antigravity itself writes; everything else rides
/// `CortexStepGeneric` with the flattened result text.
// One arm per typed tool; each writes a different observed payload shape.
#[allow(clippy::too_many_lines)]
fn tool_step_payload(tool: &Tool, result: Option<&PendingResult>) -> (i64, u32, Vec<u8>, i64) {
    match tool {
        // A completed read/edit/write whose result content the native step
        // cannot carry (an error message, or free-form text where the payload
        // has no result slot) rides the generic carrier so the text survives.
        Tool::Read { .. } if result.is_some_and(|r| r.is_error) => generic_step_payload(result),
        Tool::Write { file_path, .. }
            if result.is_some_and(|r| {
                r.is_error || !is_derived_edit_result(r, file_path, "created")
            }) =>
        {
            generic_step_payload(result)
        }
        Tool::Edit { file_path, .. }
            if result
                .is_some_and(|r| r.is_error || !is_derived_edit_result(r, file_path, "edited")) =>
        {
            generic_step_payload(result)
        }
        Tool::Bash {
            command, workdir, ..
        } => {
            let mut body = Vec::new();
            pb_string(&mut body, 2, workdir.as_deref().unwrap_or_default());
            if let Some(result) = result {
                pb_varint_field(&mut body, 6, u64::from(result.is_error));
            }
            pb_varint_field(&mut body, 11, 1);
            pb_varint_field(&mut body, 12, 2000);
            if let Some(result) = result {
                let mut output = Vec::new();
                pb_string(&mut output, 1, &tool_output_text(&result.content));
                pb_len(&mut body, 21, &output);
            }
            pb_string(&mut body, 23, command);
            pb_string(&mut body, 25, command);
            // A failed command is a completed step; the exit code carries
            // the error.
            let status = if result.is_some() {
                STATUS_DONE
            } else {
                STATUS_PENDING
            };
            (STEP_RUN_COMMAND, 28, body, status)
        }
        Tool::Read {
            file_path,
            offset,
            limit,
        } => {
            let mut body = Vec::new();
            pb_string(&mut body, 1, &file_uri(file_path));
            if let Some(start) = offset {
                pb_varint_field(&mut body, 2, *start);
            }
            match (offset, limit) {
                (Some(start), Some(len)) => {
                    pb_varint_field(&mut body, 3, start + len.saturating_sub(1));
                }
                (None, Some(len)) => pb_varint_field(&mut body, 3, *len),
                (_, None) => {}
            }
            let status = match result {
                // Errored reads took the generic arm above.
                Some(r) => {
                    let text = tool_output_text(&r.content);
                    pb_string(&mut body, 4, &text);
                    pb_varint_field(&mut body, 11, text.lines().count() as u64);
                    pb_varint_field(&mut body, 12, text.len() as u64);
                    STATUS_DONE
                }
                None => STATUS_PENDING,
            };
            (STEP_VIEW_FILE, 14, body, status)
        }
        Tool::Write { file_path, content } => {
            let uri = file_uri(file_path);
            let mut create = Vec::new();
            pb_string(&mut create, 1, content);
            let mut path = Vec::new();
            pb_string(&mut path, 5, &uri);
            pb_len(&mut create, 2, &path);
            pb_varint_field(&mut create, 4, 1);
            let mut spec = Vec::new();
            pb_len(&mut spec, 2, &create);

            let mut body = Vec::new();
            pb_len(&mut body, 1, &spec);
            if result.is_some() {
                let mut edit = Vec::new();
                pb_string(&mut edit, 8, &uri);
                pb_varint_field(&mut edit, 11, 1);
                let mut action_result = Vec::new();
                pb_len(&mut action_result, 1, &edit);
                pb_len(&mut body, 2, &action_result);
            }
            pb_varint_field(&mut body, 4, 1);
            // Errored and free-form results took the generic arm above.
            let status = if result.is_some() {
                STATUS_DONE
            } else {
                STATUS_PENDING
            };
            (STEP_CODE_ACTION, 10, body, status)
        }
        Tool::Edit {
            file_path,
            old_string,
            new_string,
            replace_all,
        } => {
            let uri = file_uri(file_path);
            let mut chunk = Vec::new();
            pb_string(&mut chunk, 1, old_string);
            pb_string(&mut chunk, 2, new_string);
            if *replace_all {
                pb_varint_field(&mut chunk, 3, 1);
            }
            let mut command = Vec::new();
            pb_varint_field(&mut command, 2, 1);
            let mut path = Vec::new();
            pb_string(&mut path, 5, &uri);
            pb_len(&mut command, 4, &path);
            pb_varint_field(&mut command, 8, 1);
            pb_len(&mut command, 9, &chunk);
            let mut spec = Vec::new();
            pb_len(&mut spec, 1, &command);

            let mut body = Vec::new();
            pb_len(&mut body, 1, &spec);
            if result.is_some() {
                let mut edit = Vec::new();
                pb_string(&mut edit, 8, &uri);
                pb_string(&mut edit, 10, old_string);
                let mut action_result = Vec::new();
                pb_len(&mut action_result, 1, &edit);
                pb_len(&mut body, 2, &action_result);
            }
            pb_varint_field(&mut body, 4, 1);
            // Errored and free-form results took the generic arm above.
            let status = if result.is_some() {
                STATUS_DONE
            } else {
                STATUS_PENDING
            };
            (STEP_CODE_ACTION, 10, body, status)
        }
        Tool::MultiEdit { .. } | Tool::Raw { .. } => generic_step_payload(result),
    }
}

/// `CortexStepGeneric` carrying the flattened result text; the call itself
/// rides the step metadata's `tool_call`.
fn generic_step_payload(result: Option<&PendingResult>) -> (i64, u32, Vec<u8>, i64) {
    let mut body = Vec::new();
    let status = match result {
        Some(r) => {
            let mut generic_result = Vec::new();
            pb_string(&mut generic_result, 1, &tool_output_text(&r.content));
            pb_len(&mut body, 2, &generic_result);
            if r.is_error {
                STATUS_ERROR
            } else {
                STATUS_DONE
            }
        }
        None => STATUS_PENDING,
    };
    (STEP_GENERIC, 140, body, status)
}

/// Whether a result is exactly the canonical derived form `to_common`
/// produces for a native edit step — the only content `CortexStepCodeAction`
/// can represent.
fn is_derived_edit_result(result: &PendingResult, file_path: &str, key: &str) -> bool {
    matches!(&result.content, ToolOutput::Json(v) if *v == json!({ "file": file_path, key: true }))
}

/// `exa.cortex_pb.CortexTrajectoryMetadata` for the `main` row.
fn trajectory_metadata_proto(meta: &Meta, session_id: &str) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(cwd) = meta.cwd.as_deref() {
        let uri = file_uri(cwd);
        let mut workspace = Vec::new();
        pb_string(&mut workspace, 1, &uri);
        pb_string(&mut workspace, 2, &uri);
        if let Some(branch) = meta.git_branch.as_deref() {
            pb_string(&mut workspace, 4, branch);
        }
        pb_len(&mut out, 1, &workspace);
        pb_string(&mut out, 7, &uri);
    }
    pb_timestamp_field(&mut out, 2, meta.timestamp);
    pb_string(&mut out, 6, session_id);
    pb_string(&mut out, 18, "default-cli-project");
    out
}

/// The display-log mirror of the emitted steps, one JSON object per line —
/// best-effort historical reconstruction of what the CLI itself logs.
fn render_transcript(steps: &[StepRow], messages: &[Message]) -> String {
    let _ = messages;
    let mut out = String::new();
    for step in steps {
        let payload = pb_fields(&step.step_payload);
        let meta_fields = pb_sub(&payload, 5).map(pb_fields).unwrap_or_default();
        let ts = pb_sub(&meta_fields, 1).and_then(pb_timestamp);
        let (source, type_name) = match step.step_type {
            STEP_USER_INPUT => ("USER_EXPLICIT", "USER_INPUT"),
            STEP_PLANNER_RESPONSE => ("MODEL", "PLANNER_RESPONSE"),
            STEP_RUN_COMMAND => ("MODEL", "RUN_COMMAND"),
            STEP_VIEW_FILE => ("MODEL", "VIEW_FILE"),
            STEP_LIST_DIRECTORY => ("MODEL", "LIST_DIRECTORY"),
            STEP_CODE_ACTION => ("MODEL", "CODE_ACTION"),
            STEP_GREP_SEARCH => ("MODEL", "GREP_SEARCH"),
            STEP_MCP_TOOL => ("MODEL", "MCP_TOOL"),
            STEP_GENERIC => ("MODEL", "GENERIC"),
            _ => ("SYSTEM", "UNKNOWN"),
        };
        let mut line = json!({
            "step_index": step.idx,
            "source": source,
            "type": type_name,
            "status": if step.status == STATUS_ERROR { "ERROR" } else { "DONE" },
        });
        if let Some(ts) = ts {
            line["created_at"] = Value::String(ts.format("%Y-%m-%dT%H:%M:%SZ").to_string());
        }
        let content = match step.step_type {
            STEP_USER_INPUT => pb_sub(&payload, 19)
                .map(pb_fields)
                .and_then(|input| pb_str(&input, 2)),
            STEP_PLANNER_RESPONSE => pb_sub(&payload, 20)
                .map(pb_fields)
                .and_then(|planner| pb_str(&planner, 1)),
            _ => {
                let (output, _) = result_output(step.step_type, &payload);
                Some(tool_output_text(&output))
            }
        };
        if let Some(content) = content.filter(|c| !c.is_empty()) {
            line["content"] = Value::String(content);
        }
        out.push_str(&line.to_string());
        out.push('\n');
    }
    out
}

// -- metadata --------------------------------------------------------------

fn meta_from_db(db: &AntigravityDb) -> Meta {
    let main_blob = db
        .trajectory_metadata_blob
        .iter()
        .find(|row| row.id == "main")
        .map(|row| pb_fields(&row.data))
        .unwrap_or_default();

    let id = db
        .trajectory_meta
        .first()
        .map(|row| row.cascade_id.clone())
        .filter(|id| !id.is_empty())
        .or_else(|| pb_str(&main_blob, 6))
        .unwrap_or_default();

    let workspace = pb_sub(&main_blob, 1).map(pb_fields).unwrap_or_default();
    let cwd = pb_str(&workspace, 1)
        .or_else(|| pb_str(&main_blob, 7))
        .as_deref()
        .map(strip_file_uri)
        .filter(|s| !s.is_empty());
    let git_branch = pb_str(&workspace, 4).filter(|s| !s.is_empty());

    let mut steps: Vec<&StepRow> = db.steps.iter().collect();
    steps.sort_by_key(|s| s.idx);

    let timestamp = pb_sub(&main_blob, 2)
        .and_then(pb_timestamp)
        .or_else(|| {
            steps.iter().find_map(|step| {
                let payload = pb_fields(&step.step_payload);
                let meta_fields = pb_sub(&payload, 5).map(pb_fields)?;
                pb_sub(&meta_fields, 1).and_then(pb_timestamp)
            })
        })
        .unwrap_or_else(|| DateTime::from_timestamp_millis(0).unwrap_or_else(Utc::now));

    // The latest checkpoint's title wins; the first user message seeds a
    // fallback.
    let title = steps
        .iter()
        .rev()
        .filter(|step| step.step_type == STEP_CHECKPOINT)
        .find_map(|step| {
            let payload = pb_fields(&step.step_payload);
            let checkpoint = pb_sub(&payload, 30).map(pb_fields)?;
            pb_str(&checkpoint, 10)
                .filter(|s| !s.trim().is_empty())
                .or_else(|| pb_str(&checkpoint, 4).filter(|s| !s.trim().is_empty()))
        })
        .or_else(|| {
            steps
                .iter()
                .filter(|step| step.step_type == STEP_USER_INPUT)
                .find_map(|step| {
                    let payload = pb_fields(&step.step_payload);
                    let input = pb_sub(&payload, 19).map(pb_fields)?;
                    pb_str(&input, 2).filter(|s| !s.trim().is_empty())
                })
                .map(|text| truncate_title(&text))
        });

    let model = steps
        .iter()
        .rev()
        .filter(|step| step.step_type == STEP_PLANNER_RESPONSE)
        .find_map(|step| {
            let payload = pb_fields(&step.step_payload);
            let meta_fields = pb_sub(&payload, 5).map(pb_fields)?;
            pb_uint(&meta_fields, 11).map(model_name)
        });

    Meta {
        id,
        timestamp,
        cwd,
        git_branch,
        title,
        cli_version: None,
        model,
    }
}

fn truncate_title(text: &str) -> String {
    const MAX: usize = 80;
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= MAX {
        one_line
    } else {
        let mut t: String = one_line.chars().take(MAX.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

// -- store -----------------------------------------------------------------

/// Reads and writes Antigravity conversations under a CLI data root
/// (default `~/.gemini/antigravity-cli`).
#[derive(Debug, Clone)]
pub struct AntigravityStore {
    pub root: PathBuf,
}

impl AntigravityStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The default data root, `~/.gemini/antigravity-cli`.
    #[must_use]
    pub fn default_root() -> Option<Self> {
        super::home_dir().map(|home| Self::new(home.join(".gemini").join("antigravity-cli")))
    }

    #[cfg(feature = "opencode")]
    fn conversations_dir(&self) -> PathBuf {
        self.root.join("conversations")
    }

    #[cfg(feature = "opencode")]
    fn brain_dir(&self, id: &str) -> PathBuf {
        self.root.join("brain").join(id)
    }
}

#[cfg(feature = "opencode")]
impl Store for AntigravityStore {
    type H = Antigravity;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        // A missing root has no sessions; unreadable entries are skipped.
        Ok(fs::read_dir(self.conversations_dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "db"))
            .filter_map(|path| {
                // Pre-`.db` `.pb` files and foreign SQLite files don't parse.
                let body = read_db(&path, &self.root).ok()?;
                let mut meta = meta_from_db(&body);
                backfill_meta(&mut meta, &path);
                Some(Discovered {
                    meta,
                    reference: path,
                })
            })
            .collect())
    }

    fn load(&self, reference: &PathBuf) -> Result<Transcript<Antigravity>> {
        let body = read_db(reference, &self.root)?;
        let mut meta = meta_from_db(&body);
        backfill_meta(&mut meta, reference);
        Ok(Transcript::new(meta, body))
    }

    fn save(&self, transcript: &Transcript<Antigravity>) -> Result<Saved<PathBuf>> {
        let id = [
            transcript.meta.id.clone(),
            transcript
                .body
                .trajectory_meta
                .first()
                .map(|row| row.cascade_id.clone())
                .unwrap_or_default(),
        ]
        .into_iter()
        .find(|candidate| !candidate.is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

        let conversations = self.conversations_dir();
        fs::create_dir_all(&conversations)?;
        let db_path = conversations.join(format!("{id}.db"));
        write_db(&db_path, &transcript.body)?;

        // The CLI wedges on resume when `brain/<id>/` is absent; contents are
        // optional.
        let logs_dir = self.brain_dir(&id).join(".system_generated").join("logs");
        fs::create_dir_all(&logs_dir)?;
        if let Some(text) = transcript.body.transcript.as_deref() {
            fs::write(logs_dir.join("transcript.jsonl"), text)?;
        }
        if let Some(text) = transcript.body.transcript_full.as_deref() {
            fs::write(logs_dir.join("transcript_full.jsonl"), text)?;
        }

        Ok(Saved {
            id,
            reference: db_path,
        })
    }

    /// The `Ref` is the conversation database; deleting removes it, its WAL
    /// sidecars, and the session's brain directory.
    fn delete(&self, reference: &PathBuf) -> Result<()> {
        let id = reference
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|_| reference.extension().is_some_and(|ext| ext == "db"))
            .ok_or_else(|| Error::Malformed {
                harness: Antigravity::NAME,
                detail: format!("not a conversation db path: {}", reference.display()),
            })?;
        fs::remove_file(reference)?;
        for suffix in ["db-wal", "db-shm"] {
            let sidecar = reference.with_extension(suffix);
            if sidecar.exists() {
                fs::remove_file(sidecar)?;
            }
        }
        let brain = self.brain_dir(&id);
        if brain.is_dir() {
            fs::remove_dir_all(brain)?;
        }
        Ok(())
    }

    fn fingerprints(&self, refs: &[PathBuf]) -> Result<HashMap<String, String>> {
        let mut out = HashMap::with_capacity(refs.len());
        for path in refs {
            out.insert(path.to_string_lossy().into_owned(), file_fingerprint(path));
        }
        Ok(out)
    }
}

#[cfg(not(feature = "opencode"))]
impl Store for AntigravityStore {
    type H = Antigravity;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        Ok(Vec::new())
    }

    fn load(&self, _reference: &PathBuf) -> Result<Transcript<Antigravity>> {
        Err(sqlite_unavailable())
    }

    fn save(&self, _transcript: &Transcript<Antigravity>) -> Result<Saved<PathBuf>> {
        Err(sqlite_unavailable())
    }

    fn delete(&self, _reference: &PathBuf) -> Result<()> {
        Err(sqlite_unavailable())
    }
}

#[cfg(not(feature = "opencode"))]
fn sqlite_unavailable() -> Error {
    Error::Unconvertible {
        harness: Antigravity::NAME,
        detail: "Antigravity store support requires the `opencode` feature for SQLite".to_string(),
    }
}

#[cfg(feature = "opencode")]
fn backfill_meta(meta: &mut Meta, db_path: &Path) {
    if meta.id.is_empty() {
        meta.id = db_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    if meta.timestamp.timestamp_millis() == 0 {
        meta.timestamp = fs::metadata(db_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .map_or_else(Utc::now, DateTime::from);
    }
}

// One straight-line SELECT per table; splitting hides the schema.
#[allow(clippy::too_many_lines)]
#[cfg(feature = "opencode")]
fn read_db(db_path: &Path, root: &Path) -> Result<AntigravityDb> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlite_err)?;

    let trajectory_meta = {
        let mut stmt = conn
            .prepare(
                "SELECT trajectory_id, cascade_id, trajectory_type, source \
                 FROM trajectory_meta ORDER BY trajectory_id",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(TrajectoryMetaRow {
                    trajectory_id: row.get(0)?,
                    cascade_id: row.get(1)?,
                    trajectory_type: row.get(2)?,
                    source: row.get(3)?,
                })
            })
            .map_err(sqlite_err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sqlite_err)?
    };

    let steps = {
        let mut stmt = conn
            .prepare(
                "SELECT idx, step_type, status, has_subtrajectory, metadata, error_details, \
                 permissions, task_details, render_info, step_payload, step_format \
                 FROM steps ORDER BY idx",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(StepRow {
                    idx: row.get(0)?,
                    step_type: row.get(1)?,
                    status: row.get(2)?,
                    has_subtrajectory: row.get::<_, Option<bool>>(3)?.unwrap_or(false),
                    metadata: row.get::<_, Option<Vec<u8>>>(4)?.unwrap_or_default(),
                    error_details: row.get::<_, Option<Vec<u8>>>(5)?.unwrap_or_default(),
                    permissions: row.get::<_, Option<Vec<u8>>>(6)?.unwrap_or_default(),
                    task_details: row.get::<_, Option<Vec<u8>>>(7)?.unwrap_or_default(),
                    render_info: row.get::<_, Option<Vec<u8>>>(8)?.unwrap_or_default(),
                    step_payload: row.get::<_, Option<Vec<u8>>>(9)?.unwrap_or_default(),
                    step_format: row.get::<_, Option<i64>>(10)?.unwrap_or(0),
                })
            })
            .map_err(sqlite_err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sqlite_err)?
    };

    let sized_rows = |table: &str| -> Result<Vec<SizedBlobRow>> {
        let mut stmt = conn
            .prepare(&format!("SELECT idx, data, size FROM {table} ORDER BY idx"))
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SizedBlobRow {
                    idx: row.get(0)?,
                    data: row.get::<_, Option<Vec<u8>>>(1)?.unwrap_or_default(),
                    size: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                })
            })
            .map_err(sqlite_err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sqlite_err)
    };
    let indexed_rows = |table: &str| -> Result<Vec<IndexedBlobRow>> {
        let mut stmt = conn
            .prepare(&format!("SELECT idx, data FROM {table} ORDER BY idx"))
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(IndexedBlobRow {
                    idx: row.get(0)?,
                    data: row.get::<_, Option<Vec<u8>>>(1)?.unwrap_or_default(),
                })
            })
            .map_err(sqlite_err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sqlite_err)
    };

    let trajectory_metadata_blob = {
        let mut stmt = conn
            .prepare("SELECT id, data FROM trajectory_metadata_blob ORDER BY id")
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(KeyedBlobRow {
                    id: row.get(0)?,
                    data: row.get::<_, Option<Vec<u8>>>(1)?.unwrap_or_default(),
                })
            })
            .map_err(sqlite_err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sqlite_err)?
    };

    let id = db_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let logs = root
        .join("brain")
        .join(&id)
        .join(".system_generated")
        .join("logs");
    let transcript = fs::read_to_string(logs.join("transcript.jsonl")).ok();
    let transcript_full = fs::read_to_string(logs.join("transcript_full.jsonl")).ok();

    Ok(AntigravityDb {
        trajectory_meta,
        steps,
        gen_metadata: sized_rows("gen_metadata")?,
        executor_metadata: indexed_rows("executor_metadata")?,
        parent_references: indexed_rows("parent_references")?,
        battle_mode_infos: indexed_rows("battle_mode_infos")?,
        trajectory_metadata_blob,
        transcript,
        transcript_full,
    })
}

#[cfg(feature = "opencode")]
fn write_db(db_path: &Path, body: &AntigravityDb) -> Result<()> {
    let conn = Connection::open(db_path).map_err(sqlite_err)?;
    conn.execute_batch(
        // user_version 1 matches the CLI's own migration stamp; a zero
        // version makes the CLI treat the database as uninitialized.
        "PRAGMA user_version = 1;\n\
         CREATE TABLE IF NOT EXISTS `trajectory_meta` (`trajectory_id` text,`cascade_id` text,\
         `trajectory_type` integer,`source` integer,PRIMARY KEY (`trajectory_id`));\n\
         CREATE TABLE IF NOT EXISTS `steps` (`idx` integer,`step_type` integer NOT NULL DEFAULT 0,\
         `status` integer NOT NULL DEFAULT 0,`has_subtrajectory` numeric NOT NULL DEFAULT false,\
         `metadata` blob,`error_details` blob,`permissions` blob,`task_details` blob,\
         `render_info` blob,`step_payload` blob,`step_format` integer NOT NULL DEFAULT 0,\
         PRIMARY KEY (`idx`));\n\
         CREATE INDEX IF NOT EXISTS `idx_steps_status` ON `steps`(`status`);\n\
         CREATE INDEX IF NOT EXISTS `idx_steps_step_type` ON `steps`(`step_type`);\n\
         CREATE TABLE IF NOT EXISTS `gen_metadata` (`idx` integer,`data` blob,\
         `size` integer NOT NULL DEFAULT 0,PRIMARY KEY (`idx`));\n\
         CREATE TABLE IF NOT EXISTS `executor_metadata` (`idx` integer,`data` blob,\
         PRIMARY KEY (`idx`));\n\
         CREATE TABLE IF NOT EXISTS `parent_references` (`idx` integer,`data` blob,\
         PRIMARY KEY (`idx`));\n\
         CREATE TABLE IF NOT EXISTS `trajectory_metadata_blob` (`id` text DEFAULT \"main\",\
         `data` blob,PRIMARY KEY (`id`));\n\
         CREATE TABLE IF NOT EXISTS `battle_mode_infos` (`idx` integer,`data` blob,\
         PRIMARY KEY (`idx`));\n\
         DELETE FROM trajectory_meta; DELETE FROM steps; DELETE FROM gen_metadata;\n\
         DELETE FROM executor_metadata; DELETE FROM parent_references;\n\
         DELETE FROM trajectory_metadata_blob; DELETE FROM battle_mode_infos;",
    )
    .map_err(sqlite_err)?;

    for row in &body.trajectory_meta {
        conn.execute(
            "INSERT OR REPLACE INTO trajectory_meta \
             (trajectory_id, cascade_id, trajectory_type, source) VALUES (?1, ?2, ?3, ?4)",
            params![
                row.trajectory_id,
                row.cascade_id,
                row.trajectory_type,
                row.source
            ],
        )
        .map_err(sqlite_err)?;
    }
    for step in &body.steps {
        conn.execute(
            "INSERT OR REPLACE INTO steps (idx, step_type, status, has_subtrajectory, metadata, \
             error_details, permissions, task_details, render_info, step_payload, step_format) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                step.idx,
                step.step_type,
                step.status,
                step.has_subtrajectory,
                step.metadata,
                step.error_details,
                step.permissions,
                step.task_details,
                step.render_info,
                step.step_payload,
                step.step_format,
            ],
        )
        .map_err(sqlite_err)?;
    }
    for row in &body.gen_metadata {
        conn.execute(
            "INSERT OR REPLACE INTO gen_metadata (idx, data, size) VALUES (?1, ?2, ?3)",
            params![row.idx, row.data, row.size],
        )
        .map_err(sqlite_err)?;
    }
    let indexed = [
        ("executor_metadata", &body.executor_metadata),
        ("parent_references", &body.parent_references),
        ("battle_mode_infos", &body.battle_mode_infos),
    ];
    for (table, rows) in indexed {
        for row in rows {
            conn.execute(
                &format!("INSERT OR REPLACE INTO {table} (idx, data) VALUES (?1, ?2)"),
                params![row.idx, row.data],
            )
            .map_err(sqlite_err)?;
        }
    }
    for row in &body.trajectory_metadata_blob {
        conn.execute(
            "INSERT OR REPLACE INTO trajectory_metadata_blob (id, data) VALUES (?1, ?2)",
            params![row.id, row.data],
        )
        .map_err(sqlite_err)?;
    }
    Ok(())
}

// Passed point-free to `map_err`, which hands over the error by value.
#[allow(clippy::needless_pass_by_value)]
#[cfg(feature = "opencode")]
fn sqlite_err(e: rusqlite::Error) -> Error {
    Error::Malformed {
        harness: Antigravity::NAME,
        detail: e.to_string(),
    }
}

#[cfg(feature = "opencode")]
fn file_fingerprint(path: &Path) -> String {
    // An unreadable file fingerprints as empty.
    fs::metadata(path).map_or_else(
        |_| String::new(),
        |meta| {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{mtime}:{}", meta.len())
        },
    )
}

// -- hex -------------------------------------------------------------------

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(HEX[(byte >> 4) as usize]));
        out.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    out
}

fn hex_decode(s: &str) -> std::result::Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    if bytes.len().is_multiple_of(2) {
        bytes
            .chunks_exact(2)
            .map(|pair| Ok((hex_value(pair[0])? << 4) | hex_value(pair[1])?))
            .collect()
    } else {
        Err("hex string has odd length".to_string())
    }
}

fn hex_value(b: u8) -> std::result::Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(format!("invalid hex byte `{}`", char::from(b))),
    }
}

mod serde_hex {
    use serde::{Deserialize, Deserializer, Serializer, de};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&super::hex_encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> std::result::Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        super::hex_decode(&s).map_err(de::Error::custom)
    }
}
