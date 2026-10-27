//! `OpenCode` 2.1's transcript layout.
//!
//! 2.1 replaced the `{info, parts}` message pair with one flat record per
//! message — `user`, `assistant`, `shell`, `compaction`, … — stored in a
//! `session_message` table, and `opencode session export` / `session import`
//! move that `{info, messages}` document. The codec keeps the 2.0 export as
//! the native [`Export`]; this module projects between the two layouts, so a
//! 2.1 database, export file or import is a pure function of the same body.
//!
//! Records the 2.0 layout has no slot for (`compaction`, `idle`, `shell`,
//! `system`, …) ride along as a [`MessageRecord`] whose `info` is the record
//! itself — no `role`, no parts — so the codec skips them and they go back out
//! unchanged.

use serde_json::{Map, Value, json};

use super::{Export, MessageRecord, det_hex};
use crate::error::{Error, Result};

/// Whether a parsed export document is the 2.1 layout: flat message records,
/// or (with no messages) a 2.1 header, which names the working directory in
/// `location` rather than `directory`.
pub(super) fn is_document(doc: &Value) -> bool {
    match doc
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.first())
    {
        Some(first) => first.get("type").is_some() && first.get("parts").is_none(),
        None => doc
            .get("info")
            .is_some_and(|info| info.get("projectID").is_some() || info.get("location").is_some()),
    }
}

/// The 2.1 document for a native body.
pub(super) fn from_legacy(export: &Export) -> Value {
    let session_id = str_of(export.info.get("id")).unwrap_or_default();
    let mut messages = Vec::with_capacity(export.messages.len());
    for (i, record) in export.messages.iter().enumerate() {
        messages.extend(record_from_legacy(record, session_id, i));
    }
    json!({ "info": header_from_legacy(&export.info), "messages": messages })
}

/// A native body from a 2.1 document.
pub(super) fn to_legacy(doc: &Value) -> Result<Export> {
    let Some(Value::Object(info)) = doc.get("info") else {
        return Err(Error::Malformed {
            harness: "opencode",
            detail: "export document has no `info` object".to_string(),
        });
    };
    let mut header = info.clone();
    // The 2.0 header spells the working directory `directory`; keep both so
    // every reader of the native body finds the field it knows.
    if header.get("directory").is_none()
        && let Some(dir) = directory(&Value::Object(info.clone()))
    {
        header.insert("directory".into(), json!(dir));
    }
    let session_id = str_of(header.get("id")).unwrap_or_default().to_string();
    let mut last_user_id: Option<String> = None;
    let messages = doc
        .get("messages")
        .and_then(Value::as_array)
        .map(|records| {
            records
                .iter()
                .enumerate()
                .map(|(i, record)| record_to_legacy(record, &session_id, i, &mut last_user_id))
                .collect()
        })
        .unwrap_or_default();
    Ok(Export {
        info: Value::Object(header),
        messages,
    })
}

// ── 2.0 → 2.1 ──────────────────────────────────────────────────────────

fn header_from_legacy(info: &Value) -> Value {
    let created = millis(info.get("time").and_then(|t| t.get("created"))).unwrap_or(0);
    let updated = millis(info.get("time").and_then(|t| t.get("updated"))).unwrap_or(created);
    let mut header = Map::new();
    header.insert(
        "id".into(),
        json!(str_of(info.get("id")).unwrap_or_default()),
    );
    // The importer files the session under the project it resolves from the
    // import directory; the header only has to name one.
    header.insert(
        "projectID".into(),
        json!(str_of(info.get("projectID")).unwrap_or("global")),
    );
    header.insert(
        "cost".into(),
        json!(info.get("cost").and_then(Value::as_f64).unwrap_or(0.0)),
    );
    header.insert("tokens".into(), tokens_full(info.get("tokens")));
    header.insert(
        "time".into(),
        json!({ "created": created, "updated": updated }),
    );
    if let Some(title) = str_of(info.get("title")) {
        header.insert("title".into(), json!(title));
    }
    if let Some(dir) = directory(info) {
        header.insert("location".into(), json!({ "directory": dir }));
    }
    if let Some(model) = model_ref(info.get("model"), None) {
        header.insert("model".into(), model);
    }
    if let Some(parent) = str_of(info.get("parentID")) {
        header.insert("parentID".into(), json!(parent));
    }
    Value::Object(header)
}

/// The 2.1 records for one native message: a user record (plus one
/// `synthetic` record per synthetic text part), one assistant record, or a
/// 2.1 record kept verbatim by [`record_to_legacy`].
fn record_from_legacy(record: &MessageRecord, session_id: &str, i: usize) -> Vec<Value> {
    let info = &record.info;
    match info.get("role").and_then(Value::as_str) {
        Some("user") => user_records_from_legacy(record, session_id, i),
        Some("assistant") => vec![assistant_record_from_legacy(record, session_id, i)],
        // Already a 2.1 record: it has a `type` and no `role`.
        _ => info
            .get("type")
            .is_some()
            .then(|| info.clone())
            .into_iter()
            .collect(),
    }
}

fn user_records_from_legacy(record: &MessageRecord, session_id: &str, i: usize) -> Vec<Value> {
    let id = message_id(&record.info, session_id, i);
    let created = created_ms(&record.info);
    let mut text = Vec::new();
    let mut synthetic = Vec::new();
    let mut files = Vec::new();
    for part in &record.parts {
        match part.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(body) = part.get("text").and_then(Value::as_str) {
                    if super::is_synthetic(part) {
                        synthetic.push(body);
                    } else {
                        text.push(body);
                    }
                }
            }
            Some("file") => {
                if let Some(attachment) = attachment_from_part(part) {
                    files.push(attachment);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    if !text.is_empty() || !files.is_empty() {
        let mut user = base(&id, "user", created);
        user.insert("text".into(), json!(text.join("\n\n")));
        if !files.is_empty() {
            user.insert("files".into(), Value::Array(files));
        }
        out.push(Value::Object(user));
    }
    for (j, body) in synthetic.into_iter().enumerate() {
        let mut record = base(&format!("{id}_s{j}"), "synthetic", created);
        record.insert("text".into(), json!(body));
        out.push(Value::Object(record));
    }
    out
}

fn assistant_record_from_legacy(record: &MessageRecord, session_id: &str, i: usize) -> Value {
    let info = &record.info;
    let id = message_id(info, session_id, i);
    let created = created_ms(info);
    let mut out = base(&id, "assistant", created);
    if let Some(completed) = millis(info.get("time").and_then(|t| t.get("completed"))) {
        out.insert(
            "time".into(),
            json!({ "created": created, "completed": completed }),
        );
    }
    out.insert(
        "agent".into(),
        json!(str_of(info.get("agent")).unwrap_or("build")),
    );
    // 2.0 keeps the model flat on the assistant (`modelID`/`providerID`).
    let model = model_ref(None, str_of(info.get("modelID"))).map_or_else(
        || json!({ "id": "unknown", "providerID": "anthropic" }),
        |mut model| {
            if let Value::Object(obj) = &mut model
                && let Some(provider) = str_of(info.get("providerID"))
            {
                obj.insert("providerID".into(), json!(provider));
            }
            model
        },
    );
    out.insert("model".into(), model);
    if let Some(finish) = info.get("finish").and_then(Value::as_str) {
        out.insert("finish".into(), json!(finish_v2(finish)));
    }
    if info.get("tokens").is_some_and(Value::is_object) {
        out.insert("tokens".into(), tokens_full(info.get("tokens")));
    }
    if let Some(cost) = info.get("cost").and_then(Value::as_f64) {
        out.insert("cost".into(), json!(cost));
    }
    let content: Vec<Value> = record
        .parts
        .iter()
        .filter_map(|part| content_from_part(part, created))
        .collect();
    out.insert("content".into(), Value::Array(content));
    Value::Object(out)
}

fn content_from_part(part: &Value, created: i64) -> Option<Value> {
    match part.get("type").and_then(Value::as_str)? {
        // Synthetic text is harness bookkeeping; 2.1 has no assistant slot for it.
        "text" if super::is_synthetic(part) => None,
        "text" => Some(
            json!({ "type": "text", "text": part.get("text").and_then(Value::as_str).unwrap_or_default() }),
        ),
        "reasoning" => Some(
            json!({ "type": "reasoning", "text": part.get("text").and_then(Value::as_str).unwrap_or_default() }),
        ),
        "tool" => tool_from_part(part, created),
        // step-start/finish, snapshot, patch, file: bookkeeping, or content
        // the 2.1 assistant record cannot carry.
        _ => None,
    }
}

fn tool_from_part(part: &Value, created: i64) -> Option<Value> {
    let name = part.get("tool").and_then(Value::as_str)?;
    let call_id = part
        .get("callID")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let state = part.get("state");
    let status = state
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    // 2.1 requires an object input; anything else is kept, wrapped as
    // `{ "input": <value> }`.
    let input = match state.and_then(|s| s.get("input")) {
        Some(Value::Object(obj)) => Value::Object(obj.clone()),
        Some(Value::Null) | None => Value::Object(Map::new()),
        Some(other) => json!({ "input": other }),
    };
    let (name, input) = tool_to_v2(name, input);
    let started = millis(
        state
            .and_then(|s| s.get("time"))
            .and_then(|t| t.get("start")),
    )
    .unwrap_or(created);
    let ended = millis(state.and_then(|s| s.get("time")).and_then(|t| t.get("end")));
    let mut time = Map::new();
    time.insert("created".into(), json!(started));
    let state_v2 = match status {
        "completed" => {
            let output = state
                .and_then(|s| s.get("output"))
                .map(value_to_text)
                .unwrap_or_default();
            time.insert("completed".into(), json!(ended.unwrap_or(started)));
            let mut completed = Map::new();
            completed.insert("status".into(), json!("completed"));
            completed.insert("input".into(), input);
            completed.insert(
                "content".into(),
                json!([{ "type": "text", "text": output }]),
            );
            if let Some(metadata) = state
                .and_then(|s| s.get("metadata"))
                .filter(|m| m.is_object())
            {
                completed.insert("metadata".into(), metadata.clone());
            }
            Value::Object(completed)
        }
        "error" => {
            let message = state
                .and_then(|s| s.get("error"))
                .map(value_to_text)
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "Tool call failed".to_string());
            time.insert("completed".into(), json!(ended.unwrap_or(started)));
            json!({ "status": "error", "input": input, "error": { "type": "unknown", "message": message } })
        }
        // pending / running: still in flight.
        _ => json!({ "status": "running", "input": input, "metadata": {} }),
    };
    Some(json!({
        "type": "tool",
        "id": call_id,
        "name": name,
        "state": state_v2,
        "time": Value::Object(time),
    }))
}

/// A built-in tool 2.1 renamed, or whose input keys it renamed: the name the
/// codec writes, the 2.1 name, and the input keys as `(codec key, 2.1 key)`.
type ToolRename = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

/// Every [`ToolRename`] (`packages/core/src/tool/plugin/*.ts`). Keys not
/// listed are kept, so a call survives a round trip unchanged.
const TOOLS_2_1: &[ToolRename] = &[
    (
        "bash",
        "shell",
        &[
            ("timeout_ms", "timeout"),
            ("run_in_background", "background"),
        ],
    ),
    ("read", "read", &[("filePath", "path")]),
    ("edit", "edit", &[("filePath", "path")]),
    ("write", "write", &[("filePath", "path")]),
    (
        "grep",
        "grep",
        &[("glob", "include"), ("head_limit", "limit")],
    ),
    ("task", "subagent", &[("subagent_type", "agent")]),
    ("Skill", "skill", &[("skill", "id")]),
    ("WebSearch", "websearch", &[]),
];

/// The 2.1 name and input of a tool call the codec names the 2.0 way.
fn tool_to_v2(name: &str, input: Value) -> (String, Value) {
    let Some((_, v2_name, keys)) = TOOLS_2_1.iter().find(|(legacy, ..)| *legacy == name) else {
        return (name.to_string(), input);
    };
    let mut input = super::rename_keys(input, keys);
    // grep's case switch is inverted: `-i` means insensitive, 2.1's
    // `caseSensitive` the opposite.
    if *v2_name == "grep" {
        flip_flag(&mut input, "-i", "caseSensitive");
    }
    ((*v2_name).to_string(), input)
}

/// The codec's name and input of a 2.1 tool call; the inverse of
/// [`tool_to_v2`].
fn tool_from_v2(name: &str, input: Value) -> (String, Value) {
    let Some((legacy, _, keys)) = TOOLS_2_1.iter().find(|(_, v2_name, _)| *v2_name == name) else {
        return (name.to_string(), input);
    };
    let reversed: Vec<(&str, &str)> = keys.iter().map(|(from, to)| (*to, *from)).collect();
    let mut input = super::rename_keys(input, &reversed);
    if name == "grep" {
        flip_flag(&mut input, "caseSensitive", "-i");
    }
    ((*legacy).to_string(), input)
}

/// Moves a boolean from `from` to `to`, negated; any other value is left in
/// place.
fn flip_flag(input: &mut Value, from: &str, to: &str) {
    if let Value::Object(obj) = input
        && let Some(Value::Bool(flag)) = obj.get(from).cloned()
    {
        obj.remove(from);
        obj.insert(to.to_string(), Value::Bool(!flag));
    }
}

fn attachment_from_part(part: &Value) -> Option<Value> {
    let mime = part.get("mime").and_then(Value::as_str)?;
    let url = part.get("url").and_then(Value::as_str)?;
    if let Some((header, data)) = url.split_once(',')
        && header.starts_with("data:")
        && header.ends_with(";base64")
    {
        return Some(json!({ "data": data, "mime": mime, "source": { "type": "inline" } }));
    }
    None
}

// ── 2.1 → 2.0 ──────────────────────────────────────────────────────────

/// The native record for one 2.1 message. `last_user_id` threads the parent
/// link 2.0 assistant records carry.
pub(super) fn record_to_legacy(
    record: &Value,
    session_id: &str,
    i: usize,
    last_user_id: &mut Option<String>,
) -> MessageRecord {
    let id = message_id(record, session_id, i);
    match record.get("type").and_then(Value::as_str) {
        Some("user") => {
            *last_user_id = Some(id.clone());
            user_to_legacy(record, session_id, i, &id)
        }
        Some("assistant") => {
            assistant_to_legacy(record, session_id, i, &id, last_user_id.as_deref())
        }
        // shell, compaction, idle, synthetic, system, skill, *-switched: no 2.0
        // shape; kept verbatim, skipped by the codec.
        _ => MessageRecord {
            info: record.clone(),
            parts: Vec::new(),
        },
    }
}

/// A 2.0 part's identity: deterministic id, session and message.
fn stamp(
    part: Map<String, Value>,
    session_id: &str,
    message_id: &str,
    i: usize,
    j: usize,
) -> Value {
    let mut part = part;
    part.insert(
        "id".into(),
        json!(format!("prt_{}", det_hex(session_id, i, j + 1))),
    );
    part.insert("sessionID".into(), json!(session_id));
    part.insert("messageID".into(), json!(message_id));
    Value::Object(part)
}

fn user_to_legacy(record: &Value, session_id: &str, i: usize, id: &str) -> MessageRecord {
    let created = created_ms(record);
    let mut info = Map::new();
    info.insert("id".into(), json!(id));
    info.insert("sessionID".into(), json!(session_id));
    info.insert("role".into(), json!("user"));
    info.insert(
        "agent".into(),
        json!(str_of(record.get("agent")).unwrap_or("build")),
    );
    info.insert(
        "time".into(),
        json!({ "created": created, "completed": created }),
    );
    let mut parts = Vec::new();
    if let Some(text) = str_of(record.get("text")) {
        let mut part = Map::new();
        part.insert("type".into(), json!("text"));
        part.insert("text".into(), json!(text));
        part.insert("time".into(), json!({ "start": created, "end": created }));
        parts.push(stamp(part, session_id, id, i, parts.len()));
    }
    for file in record
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(part) = file_part(file) {
            parts.push(stamp(part, session_id, id, i, parts.len()));
        }
    }
    MessageRecord {
        info: Value::Object(info),
        parts,
    }
}

fn assistant_to_legacy(
    record: &Value,
    session_id: &str,
    i: usize,
    id: &str,
    parent_id: Option<&str>,
) -> MessageRecord {
    let created = created_ms(record);
    let mut info = Map::new();
    info.insert("id".into(), json!(id));
    info.insert("sessionID".into(), json!(session_id));
    info.insert("role".into(), json!("assistant"));
    info.insert(
        "agent".into(),
        json!(str_of(record.get("agent")).unwrap_or("build")),
    );
    info.insert("mode".into(), json!("build"));
    if let Some(parent) = parent_id {
        info.insert("parentID".into(), json!(parent));
    }
    let completed = millis(record.get("time").and_then(|t| t.get("completed"))).unwrap_or(created);
    info.insert(
        "time".into(),
        json!({ "created": created, "completed": completed }),
    );
    let model = record.get("model");
    info.insert(
        "modelID".into(),
        json!(str_of(model.and_then(|m| m.get("id"))).unwrap_or("unknown")),
    );
    info.insert(
        "providerID".into(),
        json!(str_of(model.and_then(|m| m.get("providerID"))).unwrap_or("anthropic")),
    );
    if let Some(finish) = record.get("finish").and_then(Value::as_str) {
        info.insert("finish".into(), json!(finish));
    }
    if let Some(tokens) = record.get("tokens").filter(|t| t.is_object()) {
        info.insert("tokens".into(), tokens.clone());
    }
    if let Some(cost) = record.get("cost").and_then(Value::as_f64) {
        info.insert("cost".into(), json!(cost));
    }
    let parts = record
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(part_from_content)
        .enumerate()
        .map(|(j, part)| stamp(part, session_id, id, i, j))
        .collect();
    MessageRecord {
        info: Value::Object(info),
        parts,
    }
}

fn part_from_content(item: &Value) -> Option<Map<String, Value>> {
    let mut part = Map::new();
    match item.get("type").and_then(Value::as_str)? {
        "text" => {
            part.insert("type".into(), json!("text"));
            part.insert(
                "text".into(),
                json!(item.get("text").and_then(Value::as_str).unwrap_or_default()),
            );
        }
        "reasoning" => {
            part.insert("type".into(), json!("reasoning"));
            part.insert(
                "text".into(),
                json!(item.get("text").and_then(Value::as_str).unwrap_or_default()),
            );
        }
        "tool" => {
            let state = item.get("state");
            let status = state
                .and_then(|s| s.get("status"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let input = state
                .and_then(|s| s.get("input"))
                .filter(|input| input.is_object())
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new()));
            let (name, input) = tool_from_v2(
                item.get("name").and_then(Value::as_str).unwrap_or_default(),
                input,
            );
            let mut legacy = Map::new();
            legacy.insert("input".into(), input);
            match status {
                "completed" => {
                    legacy.insert("status".into(), json!("completed"));
                    legacy.insert(
                        "output".into(),
                        json!(content_text(state.and_then(|s| s.get("content")))),
                    );
                    if let Some(metadata) = state
                        .and_then(|s| s.get("metadata"))
                        .filter(|m| m.is_object())
                    {
                        legacy.insert("metadata".into(), metadata.clone());
                    }
                }
                "error" => {
                    legacy.insert("status".into(), json!("error"));
                    let message = state
                        .and_then(|s| s.get("error"))
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("Tool call failed");
                    legacy.insert("error".into(), json!(message));
                }
                // streaming (input still a string) / running: in flight.
                _ => {
                    legacy.insert("status".into(), json!("running"));
                }
            }
            if let Some(time) = item.get("time") {
                let start = millis(time.get("created"));
                let end = millis(time.get("completed")).or(start);
                if let Some(start) = start {
                    legacy.insert(
                        "time".into(),
                        json!({ "start": start, "end": end.unwrap_or(start) }),
                    );
                }
            }
            part.insert("type".into(), json!("tool"));
            part.insert("tool".into(), json!(name));
            part.insert(
                "callID".into(),
                json!(item.get("id").and_then(Value::as_str).unwrap_or_default()),
            );
            part.insert("state".into(), Value::Object(legacy));
        }
        _ => return None,
    }
    Some(part)
}

fn file_part(file: &Value) -> Option<Map<String, Value>> {
    let mime = file.get("mime").and_then(Value::as_str)?;
    let url = match file
        .get("source")
        .and_then(|s| s.get("type"))
        .and_then(Value::as_str)
    {
        Some("uri") => file
            .get("source")
            .and_then(|s| s.get("uri"))
            .and_then(Value::as_str)?
            .to_string(),
        _ => format!(
            "data:{mime};base64,{}",
            file.get("data").and_then(Value::as_str)?
        ),
    };
    let mut part = Map::new();
    part.insert("type".into(), json!("file"));
    part.insert("mime".into(), json!(mime));
    part.insert("url".into(), json!(url));
    Some(part)
}

/// The text of a 2.1 tool result: its text items, joined by newlines.
fn content_text(content: Option<&Value>) -> String {
    content
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

// ── small helpers ──────────────────────────────────────────────────────

fn base(id: &str, kind: &str, created: i64) -> Map<String, Value> {
    let mut record = Map::new();
    record.insert("id".into(), json!(id));
    record.insert("type".into(), json!(kind));
    record.insert("time".into(), json!({ "created": created }));
    record
}

fn str_of(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn millis(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64)
}

fn created_ms(record: &Value) -> i64 {
    millis(record.get("time").and_then(|t| t.get("created"))).unwrap_or(0)
}

/// 2.1 requires `msg_` ids; a record without one gets a deterministic id.
fn message_id(record: &Value, session_id: &str, i: usize) -> String {
    match str_of(record.get("id")) {
        Some(id) if id.starts_with("msg_") => id.to_string(),
        Some(id) => format!("msg_{id}"),
        None => format!("msg_{}", det_hex(session_id, i, 0)),
    }
}

fn directory(info: &Value) -> Option<&str> {
    str_of(info.get("location").and_then(|l| l.get("directory")))
        .or_else(|| str_of(info.get("directory")))
}

/// A `Model.Ref` from a 2.0 `model` object or a bare id; `None` without an id.
fn model_ref(model: Option<&Value>, id: Option<&str>) -> Option<Value> {
    let id = id.or_else(|| str_of(model.and_then(|m| m.get("id"))))?;
    let provider = str_of(model.and_then(|m| m.get("providerID"))).unwrap_or("anthropic");
    Some(json!({ "id": id, "providerID": provider }))
}

/// The full `TokenUsage.Info` object 2.1 validates; absent counts are 0.
fn tokens_full(tokens: Option<&Value>) -> Value {
    let count = |path: &[&str]| -> u64 {
        let mut cursor = tokens;
        for key in path {
            cursor = cursor.and_then(|v| v.get(key));
        }
        cursor.and_then(Value::as_u64).unwrap_or(0)
    };
    json!({
        "input": count(&["input"]),
        "output": count(&["output"]),
        "reasoning": count(&["reasoning"]),
        "cache": { "read": count(&["cache", "read"]), "write": count(&["cache", "write"]) },
    })
}

/// 2.1's `FinishReason`: `stop`, `length`, `tool-calls`, `content-filter`,
/// `error`, `unknown`. 2.0 spelled tool calls `tool_use`.
fn finish_v2(finish: &str) -> &'static str {
    match finish {
        "stop" => "stop",
        "length" => "length",
        "tool_use" | "tool_calls" | "tool-calls" => "tool-calls",
        "content-filter" => "content-filter",
        "error" => "error",
        _ => "unknown",
    }
}

fn value_to_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
