//! `OpenCode`: a single `SQLite` database (`~/.local/share/opencode/opencode.db`) holding either
//! `session` / `message` / `part` rows (1.x) or `session_v2` / `session_message` rows (2.x).
//!
//! The native [`Body`](OpenCode) is `OpenCode`'s 1.x export shape —
//! `{info, messages: [{info, parts}]}` — which doubles as the interchange form between the codec
//! and the store. [`OpenCodeStore`] reads the DB read-only and assembles that shape; `save` hands
//! it to `OpenCode`'s own import command, translating to the flat document 2.x requires. The codec
//! and native types are pure JSON and compile without the `opencode` feature; only the store pulls
//! in `rusqlite`.
//!
//! A `part` of type `tool` splits into a [`Block::ToolUse`] on an assistant
//! message and a [`Block::ToolResult`] on the following user message, matching
//! the Anthropic shape. Synthetic text parts and internal bookkeeping
//! (step-start/finish, snapshot, patch, …) are dropped from the conversation
//! but preserved in the native body.

use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::common::{Block, ImageSource, Message, Meta, Role, StopReason, Tool, ToolOutput, Usage};
use crate::error::Result;
use crate::transcript::{Codec, Common, Harness, TextCodec, Transcript};

/// The `OpenCode` harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenCode;

impl Harness for OpenCode {
    const NAME: &'static str = "opencode";
    type Body = Export;
}

/// `OpenCode`'s export document: a session header plus its messages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Export {
    pub info: Value,
    pub messages: Vec<MessageRecord>,
}

/// One message and its parts, as `opencode export` emits them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageRecord {
    pub info: Value,
    pub parts: Vec<Value>,
}

// ── codec: to_common ───────────────────────────────────────────────────

impl Codec for OpenCode {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        let fallback = transcript.meta.timestamp;
        let mut messages = Vec::new();
        for record in &transcript.body.messages {
            let ts = msg_timestamp(&record.info, fallback);
            match record.info.get("role").and_then(Value::as_str) {
                Some("user") => build_user_message(&record.parts, ts, &mut messages),
                Some("assistant") => {
                    build_assistant_messages(&record.info, &record.parts, ts, &mut messages);
                }
                // Any other role (or a missing one) carries no conversational
                // turn.
                _ => {}
            }
        }
        Ok(Transcript::new(transcript.meta.clone(), messages))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            build_export(&transcript.meta, &transcript.body),
        ))
    }
}

impl TextCodec for OpenCode {
    /// The text form is the current `opencode session export` document (2.x,
    /// flat records); the 1.x `opencode export` shape is read as well.
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let doc: Value = serde_json::from_str(text)?;
        let export = if is_v2_document(&doc) {
            v2_document_to_export(&doc)
        } else {
            serde_json::from_value(doc)?
        };
        let meta = meta_from_info(&export.info);
        Ok(Transcript::new(meta, export))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Ok(serde_json::to_string(&build_v2_document(
            &transcript.body,
            "global",
        ))?)
    }
}

/// True when the document is `opencode session export`'s 2.x layout: a flat
/// message record (a `type`, no `parts`), or — with no messages — a 2.x header,
/// which names the working directory in `location` rather than `directory`.
fn is_v2_document(doc: &Value) -> bool {
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

/// The native (1.x-shaped) body for a 2.x document, so `from_text` accepts what
/// `opencode session export` produces and the codec keeps one shape.
fn v2_document_to_export(doc: &Value) -> Export {
    let mut header = doc.get("info").cloned().unwrap_or_else(|| json!({}));
    // The 1.x header spells the working directory `directory`; 2.x nests it in
    // `location`, and everything that reads the native body expects the former.
    let directory = header
        .pointer("/location/directory")
        .and_then(Value::as_str)
        .filter(|dir| !dir.is_empty())
        .map(String::from);
    if header.get("directory").is_none()
        && let Some(directory) = directory
    {
        header["directory"] = json!(directory);
    }
    let messages = doc
        .get("messages")
        .and_then(Value::as_array)
        .map(|records| {
            records
                .iter()
                .filter_map(|record| {
                    let kind = record.get("type").and_then(Value::as_str)?;
                    let id = record.get("id").and_then(Value::as_str).unwrap_or_default();
                    let ts = record
                        .pointer("/time/created")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    Some(match kind {
                        "user" => v2_user_record(id, record, ts),
                        "assistant" => v2_assistant_record(id, record, ts),
                        // A record with no 1.x shape rides verbatim.
                        _ => MessageRecord {
                            info: record.clone(),
                            parts: Vec::new(),
                        },
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Export {
        info: header,
        messages,
    }
}

/// Session metadata from an export's `info` object (the WASM/text path; the
/// `SQLite` store derives the same fields from `session` columns).
fn meta_from_info(info: &Value) -> Meta {
    let string = |key: &str| {
        info.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    let title = string("title").filter(|t| !t.starts_with("New session - "));
    let timestamp = info
        .get("time")
        .and_then(|t| t.get("created"))
        .and_then(Value::as_i64)
        .and_then(DateTime::from_timestamp_millis)
        .unwrap_or_else(Utc::now);
    let model = info
        .get("model")
        .and_then(|m| m.get("id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from);
    Meta {
        id: string("id").unwrap_or_default(),
        timestamp,
        cwd: string("directory"),
        git_branch: None,
        title,
        cli_version: string("version"),
        model,
        // OpenCode's parent-session signal is the SQLite `session.parent_id`
        // column, not a field in the export/import JSON this path reads.
        lineage: None,
    }
}

fn build_user_message(parts: &[Value], ts: DateTime<Utc>, out: &mut Vec<Message>) {
    let blocks: Vec<Block> = parts
        .iter()
        .filter_map(|part| match part.get("type").and_then(Value::as_str) {
            // Synthetic text is harness bookkeeping, not something the user
            // wrote.
            Some("text") if is_synthetic(part) => None,
            Some("text") => part
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .map(|text| Block::Text {
                    text: text.to_string(),
                }),
            Some("file") => parse_file_image(part).map(|source| Block::Image { source }),
            // Tool and bookkeeping parts don't occur on user messages.
            _ => None,
        })
        .collect();
    if !blocks.is_empty() {
        out.push(Message {
            role: Role::User,
            content: blocks,
            timestamp: ts,
            model: None,
            stop_reason: None,
            usage: None,
        });
    }
}

fn build_assistant_messages(
    info: &Value,
    parts: &[Value],
    ts: DateTime<Utc>,
    out: &mut Vec<Message>,
) {
    let model = info
        .get("modelID")
        .and_then(Value::as_str)
        .map(String::from);
    let usage = parse_tokens(info.get("tokens"));
    let stop_reason = info.get("finish").and_then(Value::as_str).map(parse_finish);

    let mut pending: Vec<Block> = Vec::new();
    let mut assistant_indices: Vec<usize> = Vec::new();

    for part in parts {
        match part.get("type").and_then(Value::as_str) {
            Some("text") => {
                // Synthetic text is harness bookkeeping, not model output.
                if !is_synthetic(part)
                    && let Some(text) = part.get("text").and_then(Value::as_str)
                    && !text.trim().is_empty()
                {
                    pending.push(Block::Text {
                        text: text.to_string(),
                    });
                }
            }
            Some("reasoning") => {
                if let Some(text) = part.get("text").and_then(Value::as_str)
                    && !text.trim().is_empty()
                {
                    pending.push(Block::Thinking {
                        text: text.to_string(),
                        signature: None,
                        encrypted: None,
                    });
                }
            }
            Some("file") => {
                if let Some(source) = parse_file_image(part) {
                    pending.push(Block::Image { source });
                }
            }
            Some("tool") => {
                flush_assistant(
                    &mut pending,
                    out,
                    &mut assistant_indices,
                    ts,
                    model.as_ref(),
                );
                // A part without a `tool` name has nothing to convert.
                if let Some((use_block, result_block)) = parse_tool(part) {
                    out.push(Message {
                        role: Role::Assistant,
                        content: vec![use_block],
                        timestamp: ts,
                        model: model.clone(),
                        stop_reason: None,
                        usage: None,
                    });
                    assistant_indices.push(out.len() - 1);
                    if let Some(result_block) = result_block {
                        out.push(Message {
                            role: Role::User,
                            content: vec![result_block],
                            timestamp: ts,
                            model: None,
                            stop_reason: None,
                            usage: None,
                        });
                    }
                }
            }
            // step-start/finish, snapshot, patch, … are bookkeeping, not
            // conversation content.
            _ => {}
        }
    }
    flush_assistant(
        &mut pending,
        out,
        &mut assistant_indices,
        ts,
        model.as_ref(),
    );

    // Usage and finish describe the whole turn; attach to its last assistant.
    if let Some(&last) = assistant_indices.last() {
        out[last].usage = usage;
        out[last].stop_reason = stop_reason;
    }
}

fn flush_assistant(
    pending: &mut Vec<Block>,
    out: &mut Vec<Message>,
    indices: &mut Vec<usize>,
    ts: DateTime<Utc>,
    model: Option<&String>,
) {
    if !pending.is_empty() {
        out.push(Message {
            role: Role::Assistant,
            content: std::mem::take(pending),
            timestamp: ts,
            model: model.cloned(),
            stop_reason: None,
            usage: None,
        });
        indices.push(out.len() - 1);
    }
}

fn parse_tool(part: &Value) -> Option<(Block, Option<Block>)> {
    let tool = part.get("tool").and_then(Value::as_str)?;
    let call_id = part
        .get("callID")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let state = part.get("state");
    let status = state
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let raw_input = state
        .and_then(|s| s.get("input"))
        .cloned()
        .unwrap_or(Value::Object(Map::new()));
    let (name, input) = normalize_tool(tool, raw_input);
    let use_block = Block::ToolUse {
        id: call_id.clone(),
        tool: Tool::from_canonical(&name, input),
    };
    let result_block = match status {
        "completed" => {
            let output = state
                .and_then(|s| s.get("output"))
                .cloned()
                .unwrap_or(Value::String(String::new()));
            Some(Block::ToolResult {
                tool_use_id: call_id,
                content: value_to_output(output),
                is_error: false,
            })
        }
        "error" => {
            let message = state
                .and_then(|s| s.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("Tool call failed")
                .to_string();
            Some(Block::ToolResult {
                tool_use_id: call_id,
                content: ToolOutput::Text(message),
                is_error: true,
            })
        }
        // pending / running — still in flight; emit the call without a result.
        _ => None,
    };
    Some((use_block, result_block))
}

// ── codec: from_common ─────────────────────────────────────────────────

fn format_session_id(id: &str) -> String {
    if id.is_empty() {
        format!("ses_{}", Uuid::new_v4().simple())
    } else if id.starts_with("ses") {
        id.to_string()
    } else {
        format!("ses_{}", id.replace('-', ""))
    }
}

struct ExportContext<'a> {
    session_id: &'a str,
    cwd: &'a str,
    default_model: Option<&'a str>,
    messages: &'a [Message],
}

fn build_user_record(
    ctx: &ExportContext<'_>,
    msg: &Message,
    msg_id: &str,
    idx: usize,
) -> (MessageRecord, bool) {
    let msg_ms = msg.timestamp.timestamp_millis();
    let mut info = Map::new();
    info.insert("id".into(), json!(msg_id));
    info.insert("sessionID".into(), json!(ctx.session_id));
    info.insert(
        "time".into(),
        json!({ "created": msg_ms, "completed": msg_ms }),
    );
    info.insert("role".into(), json!("user"));
    info.insert("agent".into(), json!("build"));
    let model = msg
        .model
        .as_deref()
        .or(ctx.default_model)
        .unwrap_or("unknown");
    info.insert(
        "model".into(),
        json!({ "providerID": "anthropic", "modelID": model }),
    );
    let mut parts = Vec::new();
    for (j, block) in msg.content.iter().enumerate() {
        if let Some(part) = user_part(block, ctx.session_id, msg_id, idx, j, msg_ms) {
            parts.push(part);
        }
    }
    let emitted = !parts.is_empty();
    (
        MessageRecord {
            info: Value::Object(info),
            parts,
        },
        emitted,
    )
}

fn build_assistant_record(
    ctx: &ExportContext<'_>,
    msg: &Message,
    msg_id: &str,
    parent_id: &str,
    idx: &mut usize,
) -> MessageRecord {
    let msg_ms = msg.timestamp.timestamp_millis();
    let mut info = Map::new();
    info.insert("id".into(), json!(msg_id));
    info.insert("sessionID".into(), json!(ctx.session_id));
    info.insert(
        "time".into(),
        json!({ "created": msg_ms, "completed": msg_ms }),
    );
    info.insert("role".into(), json!("assistant"));
    let model = msg
        .model
        .as_deref()
        .or(ctx.default_model)
        .unwrap_or("unknown");
    info.insert("modelID".into(), json!(model));
    info.insert("providerID".into(), json!("anthropic"));
    info.insert("mode".into(), json!("build"));
    info.insert("agent".into(), json!("build"));
    info.insert("parentID".into(), json!(parent_id));
    info.insert("path".into(), json!({ "cwd": ctx.cwd, "root": ctx.cwd }));
    info.insert("finish".into(), json!(finish_str(msg.stop_reason.as_ref())));
    info.insert("cost".into(), json!(0.0));
    info.insert("tokens".into(), tokens_value(msg.usage.as_ref()));

    let mut parts = vec![json!({
        "id": format!("prt_{}", det_hex(ctx.session_id, *idx, 0)),
        "sessionID": ctx.session_id,
        "messageID": msg_id,
        "type": "step-start",
    })];
    for (j, block) in msg.content.iter().enumerate() {
        let part = assistant_part(
            block,
            ctx.session_id,
            msg_id,
            *idx,
            j + 1,
            msg_ms,
            ctx.messages,
            idx,
        );
        parts.push(part);
    }
    MessageRecord {
        info: Value::Object(info),
        parts,
    }
}

fn build_export(meta: &Meta, messages: &[Message]) -> Export {
    let session_id = format_session_id(&meta.id);
    let now = meta.timestamp.timestamp_millis();
    let ctx = ExportContext {
        session_id: &session_id,
        cwd: meta.cwd.as_deref().unwrap_or_default(),
        default_model: meta.model.as_deref(),
        messages,
    };

    let mut out: Vec<MessageRecord> = Vec::new();
    let mut last_user_msg_id: Option<String> = None;
    let mut idx = 0usize;
    while idx < messages.len() {
        let msg = &messages[idx];
        let msg_id = format!("msg_{}", det_hex(&session_id, idx, 0));

        match msg.role {
            Role::User => {
                let (record, emitted) = build_user_record(&ctx, msg, &msg_id, idx);
                if emitted {
                    last_user_msg_id = Some(msg_id.clone());
                    out.push(record);
                }
            }
            Role::Assistant => {
                let parent_id = last_user_msg_id.as_deref().unwrap_or(&msg_id);
                let record = build_assistant_record(&ctx, msg, &msg_id, parent_id, &mut idx);
                out.push(record);
            }
        }
        idx += 1;
    }

    Export {
        info: json!({
            "id": session_id,
            // OpenCode's 1.x import requires a slug; derive it from the session
            // id so re-imports stay stable.
            "slug": format!("txcript-{}", &det_hex(&session_id, 0, 0)[..8]),
            "directory": meta.cwd.clone().unwrap_or_default(),
            "title": meta.title.clone().unwrap_or_default(),
            "version": meta.cli_version.clone().unwrap_or_default(),
            "time": { "created": now, "updated": now },
            "model": { "id": meta.model.clone().unwrap_or_default(), "providerID": "anthropic" },
        }),
        messages: out,
    }
}

// ── codec: from_common for OpenCode 2.x ─────────────────────────────────

/// `OpenCode` 2.x's import document.
///
/// 2.x replaced `{info, messages: [{info, parts}]}` with one flat object per message
/// (`{type: "user", text, files}` / `{type: "assistant", content}`), moved `opencode import` to
/// `opencode session import`, and made `info.projectID` + `info.location` mandatory. A legacy
/// export is flattened here at the store boundary so the native body — and everything that reads
/// it, including the codec — stays on the shape `to_common` already understands.
fn build_v2_document(export: &Export, project_id: &str) -> Value {
    let info = &export.info;
    let read = |key: &str| {
        info.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let created = info
        .pointer("/time/created")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let updated = info
        .pointer("/time/updated")
        .and_then(Value::as_i64)
        .unwrap_or(created);

    let mut messages = Vec::with_capacity(export.messages.len());
    for record in &export.messages {
        let ts = record
            .info
            .pointer("/time/created")
            .and_then(Value::as_i64)
            .unwrap_or(created);
        match record.info.get("role").and_then(Value::as_str) {
            Some("user") => messages.extend(v2_user_records(record, ts)),
            Some("assistant") => messages.push(v2_assistant_message(record, ts)),
            // A 2.x record with no 1.x shape (`compaction`, `idle`, `shell`,
            // `synthetic`, …) rides through verbatim.
            _ => {
                if record.info.get("type").is_some() {
                    messages.push(record.info.clone());
                }
            }
        }
    }

    let tokens = aggregate_tokens(&messages);
    let mut header = json!({
        "id": read("id"),
        "projectID": project_id,
        "agent": info.get("agent").and_then(Value::as_str).unwrap_or("build"),
        "model": info.get("model").cloned().filter(Value::is_object)
            .unwrap_or_else(|| json!({ "providerID": "anthropic" })),
        "cost": info.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
        "tokens": tokens,
        "outcome": info.get("outcome").and_then(Value::as_str).unwrap_or("succeeded"),
        "time": { "created": created, "updated": updated },
        "title": read("title"),
        "location": { "directory": read("directory") },
    });
    if let Some(parent) = info.get("parentID").and_then(Value::as_str) {
        header["parentID"] = json!(parent);
    }
    json!({ "info": header, "messages": messages })
}

fn v2_user_records(record: &MessageRecord, ts: i64) -> Vec<Value> {
    let mut text = String::new();
    let mut synthetic = Vec::new();
    let mut files = Vec::new();
    for part in &record.parts {
        match part.get("type").and_then(Value::as_str) {
            Some("text") => {
                let chunk = part.get("text").and_then(Value::as_str).unwrap_or_default();
                if chunk.trim().is_empty() {
                    continue;
                }
                // Injected text has its own 2.x record; it is not the prompt.
                if is_synthetic(part) {
                    synthetic.push(chunk);
                    continue;
                }
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(chunk);
            }
            Some("file") => {
                // 2.x carries attachments beside the text as `files[]` entries with an
                // inline `[Image N]` mention marker; the legacy `file` part holds a data
                // URL. The TUI never appends a marker the message already carries
                // (promptAttachmentLabel reuses marker text), so when rebuilding a
                // document — a txcript round-trip, or a session OpenCode itself wrote —
                // point the mention at the existing occurrence instead of growing a
                // duplicate.
                let mime = part
                    .get("mime")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                let Some((header, payload)) = part
                    .get("url")
                    .and_then(Value::as_str)
                    .and_then(|url| url.split_once(','))
                    .filter(|(header, _)| header.starts_with("data:"))
                else {
                    continue;
                };
                // The import schema only accepts canonical base64; normalize the
                // forms OpenCode itself tolerates and re-encodes.
                let payload =
                    canonical_base64(header, payload).unwrap_or_else(|| payload.to_string());
                let marker = format!("[Image {}]", files.len() + 1);
                // Mention offsets are JS string indices (UTF-16 code units): the
                // composer places extmarks with `String.length`, not Rust byte
                // offsets, so they diverge as soon as non-ASCII precedes the marker.
                let start = if let Some(at) = text.find(&marker) {
                    text[..at].encode_utf16().count()
                } else {
                    if !text.is_empty() {
                        text.push_str("\n\n");
                    }
                    let start = text.encode_utf16().count();
                    text.push_str(&marker);
                    start
                };
                let end = start + marker.encode_utf16().count();
                files.push(json!({
                    "data": payload,
                    "mime": mime,
                    "source": { "type": "inline" },
                    "name": "txcript",
                    "mention": { "text": marker, "start": start, "end": end },
                }));
            }
            _ => {}
        }
    }
    let id = record.info.get("id").cloned().unwrap_or(Value::Null);
    let mut out = Vec::new();
    if !text.is_empty() || !files.is_empty() {
        out.push(json!({
            "id": id,
            "time": { "created": ts },
            "text": text,
            "files": files,
            "agents": [],
            "type": "user",
        }));
    }
    for (i, body) in synthetic.into_iter().enumerate() {
        let id = id
            .as_str()
            .map_or_else(|| format!("msg_s{i}"), |id| format!("{id}_s{i}"));
        out.push(json!({
            "id": id,
            "type": "synthetic",
            "time": { "created": ts },
            "text": body,
        }));
    }
    out
}

fn v2_assistant_message(record: &MessageRecord, ts: i64) -> Value {
    let mut content = Vec::new();
    for part in &record.parts {
        match part.get("type").and_then(Value::as_str) {
            // Injected text is not model output; 2.x has no assistant slot for it.
            Some("text") if is_synthetic(part) => {}
            Some("text") => {
                if let Some(text) = part.get("text").and_then(Value::as_str)
                    && !text.trim().is_empty()
                {
                    content.push(json!({ "type": "text", "text": text }));
                }
            }
            Some("reasoning") => {
                if let Some(text) = part.get("text").and_then(Value::as_str)
                    && !text.trim().is_empty()
                {
                    content.push(json!({ "type": "reasoning", "text": text }));
                }
            }
            // 2.x assistant content is text/reasoning/tool only; an image keeps its place as a
            // marker rather than silently vanishing from the transcript.
            Some("file") => {
                let mime = part.get("mime").and_then(Value::as_str).unwrap_or("image");
                content.push(json!({ "type": "text", "text": format!("[Image omitted: {mime}]") }));
            }
            Some("tool") => content.push(v2_tool_part(part, ts)),
            _ => {}
        }
    }

    let model_id = record
        .info
        .get("modelID")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .unwrap_or("unknown");
    let provider = record
        .info
        .get("providerID")
        .and_then(Value::as_str)
        .unwrap_or("anthropic");
    let (finish, raw_finish) = v2_finish(
        record
            .info
            .get("finish")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let completed = record
        .info
        .pointer("/time/completed")
        .and_then(Value::as_i64)
        .unwrap_or(ts);
    json!({
        "id": record.info.get("id").cloned().unwrap_or(Value::Null),
        "time": { "created": ts, "completed": completed },
        "type": "assistant",
        "agent": record.info.get("agent").and_then(Value::as_str).unwrap_or("build"),
        "model": { "id": model_id, "providerID": provider },
        "content": content,
        "finish": finish,
        "rawFinish": raw_finish,
        "cost": record.info.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
        "tokens": full_tokens(record.info.get("tokens")),
    })
}

/// A legacy `tool` part (call and result folded into `state.output`/`state.error`) rewritten as
/// the 2.x part, which keeps the result in `state.content`.
fn v2_tool_part(part: &Value, ts: i64) -> Value {
    let name = part
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("tool")
        .to_string();
    let call_id = part
        .get("callID")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let state = part.get("state");
    let input = object_input(
        state
            .and_then(|s| s.get("input"))
            .cloned()
            .unwrap_or_else(|| json!({})),
    );
    // 2.x renamed the built-in tools and several input keys; write its names.
    let (name, input) = tool_to_v2(&name, input);
    let status = state
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("completed");
    // 1.x keeps the call's timing inside `state.time`; 2.x wants it on the part.
    let started = state
        .and_then(|s| s.pointer("/time/start"))
        .and_then(Value::as_i64)
        .unwrap_or(ts);
    let completed = state
        .and_then(|s| s.pointer("/time/end"))
        .and_then(Value::as_i64)
        .unwrap_or(started);

    let state_json = if status == "error" {
        let message = state
            .and_then(|s| s.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("Tool call failed")
            .to_string();
        json!({
            "status": "error",
            "input": input,
            "error": { "type": "tool.execution", "message": message },
            "metadata": { "truncated": false },
        })
    } else if status == "completed" {
        let output = state
            .and_then(|s| s.get("output"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        json!({
            "status": "completed",
            "input": input,
            "content": [{ "type": "text", "text": output }],
            "metadata": { "truncated": false },
        })
    } else {
        // pending / running: still in flight.
        json!({ "status": "running", "input": input, "metadata": { "truncated": false } })
    };

    json!({
        "type": "tool",
        "id": call_id,
        "name": name,
        // `executed` is OpenCode's "providerExecuted" flag: true means the
        // provider ran the tool server-side, and replay then insists on a
        // server-tool result type it only knows for web_search / code_execution
        // / web_fetch. Imported history was run locally by the source harness,
        // so this must stay false.
        "executed": false,
        "state": state_json,
        "time": { "created": started, "completed": completed },
    })
}

/// 2.x finish vocabulary differs from the 1.x one it replaced: `tool_use` became `tool-calls`,
/// and the set of things 1.x spelled as `stop` has no 2.x equivalent other than `unknown`.
fn v2_finish(finish: &str) -> (String, String) {
    match finish {
        "tool_use" | "tool-calls" | "tool_calls" => ("tool-calls".into(), "tool_calls".into()),
        "length" => ("length".into(), "length".into()),
        "error" => ("error".into(), "error".into()),
        "" | "stop" => ("stop".into(), "stop".into()),
        other => ("unknown".into(), other.to_string()),
    }
}

fn aggregate_tokens(messages: &[Value]) -> Value {
    let mut input = 0i64;
    let mut output = 0i64;
    let mut reasoning = 0i64;
    let mut read = 0i64;
    let mut write = 0i64;
    for message in messages {
        if message.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let tokens = &message["tokens"];
        input += tokens.get("input").and_then(Value::as_i64).unwrap_or(0);
        output += tokens.get("output").and_then(Value::as_i64).unwrap_or(0);
        reasoning += tokens.get("reasoning").and_then(Value::as_i64).unwrap_or(0);
        read += tokens
            .pointer("/cache/read")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        write += tokens
            .pointer("/cache/write")
            .and_then(Value::as_i64)
            .unwrap_or(0);
    }
    json!({
        "input": input,
        "output": output,
        "reasoning": reasoning,
        "cache": { "read": read, "write": write },
    })
}

/// The full `TokenUsage.Info` object 2.x validates; a partial 1.x object fills
/// its missing counts with 0 rather than failing the import.
fn full_tokens(tokens: Option<&Value>) -> Value {
    let count = |path: &[&str]| -> i64 {
        let mut cursor = tokens;
        for key in path {
            cursor = cursor.and_then(|value| value.get(key));
        }
        cursor.and_then(Value::as_i64).unwrap_or(0)
    };
    json!({
        "input": count(&["input"]),
        "output": count(&["output"]),
        "reasoning": count(&["reasoning"]),
        "cache": { "read": count(&["cache", "read"]), "write": count(&["cache", "write"]) },
    })
}

// ── 2.x record → legacy body ───────────────────────────────────────────

/// The 1.x-shaped record for a 2.x user record: text and attachments become
/// parts, so `to_common` stays the single code path for both generations.
fn v2_user_record(id: &str, data: &Value, ts: i64) -> MessageRecord {
    let mut parts = Vec::new();
    if let Some(text) = data
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        parts.push(json!({ "type": "text", "text": text }));
    }
    for file in data
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let mime = file
            .get("mime")
            .and_then(Value::as_str)
            .unwrap_or("application/octet-stream");
        let payload = file.get("data").and_then(Value::as_str).unwrap_or_default();
        parts.push(json!({
            "type": "file",
            "mime": mime,
            "url": format!("data:{mime};base64,{payload}"),
        }));
    }
    MessageRecord {
        info: json!({ "id": id, "role": "user", "time": { "created": ts } }),
        parts,
    }
}

/// The 1.x-shaped record for a 2.x assistant record.
fn v2_assistant_record(id: &str, data: &Value, ts: i64) -> MessageRecord {
    let mut parts = Vec::new();
    for item in data
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    parts.push(json!({ "type": "text", "text": text }));
                }
            }
            Some("reasoning") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    parts.push(json!({ "type": "reasoning", "text": text }));
                }
            }
            Some("file") => {
                let mime = item
                    .get("mime")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                if let Some(url) = item.get("url").and_then(Value::as_str) {
                    parts.push(json!({ "type": "file", "mime": mime, "url": url }));
                }
            }
            Some("tool") => parts.push(v2_tool_record_part(item)),
            _ => {}
        }
    }
    let completed = data
        .pointer("/time/completed")
        .and_then(Value::as_i64)
        .unwrap_or(ts);
    MessageRecord {
        info: json!({
            "id": id,
            "role": "assistant",
            "agent": data.get("agent").and_then(Value::as_str).unwrap_or("build"),
            "modelID": data.pointer("/model/id").and_then(Value::as_str).unwrap_or_default(),
            "providerID": data.pointer("/model/providerID").and_then(Value::as_str)
                .unwrap_or("anthropic"),
            "finish": data.get("finish").and_then(Value::as_str).unwrap_or("stop"),
            "cost": data.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
            "tokens": data.get("tokens").cloned().unwrap_or_else(|| json!({})),
            "time": { "created": ts, "completed": completed },
        }),
        parts,
    }
}

/// The 1.x-shaped part for a 2.x tool part: the names and keys map back to the
/// legacy boundary the codec knows (`shell` → `bash`, `path` → `filePath`, …),
/// the result moves from `state.content` back into `state.output`, and the
/// call timing moves to `state.time`.
fn v2_tool_record_part(item: &Value) -> Value {
    let state = item.get("state");
    let status = state
        .and_then(|s| s.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("completed");
    let (name, input) = tool_from_v2(
        item.get("name").and_then(Value::as_str).unwrap_or("tool"),
        state
            .and_then(|s| s.get("input"))
            .cloned()
            .unwrap_or_else(|| json!({})),
    );
    let mut legacy_state = json!({ "status": status, "input": input });
    if status == "error" {
        let message = state
            .and_then(|s| s.get("error"))
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("Tool call failed");
        legacy_state["error"] = json!(message);
    } else {
        let output = state
            .and_then(|s| s.get("content"))
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        legacy_state["output"] = json!(output);
    }
    if let Some(time) = item.get("time") {
        let start = time.get("created").and_then(Value::as_i64);
        let end = time.get("completed").and_then(Value::as_i64).or(start);
        if let Some(start) = start {
            legacy_state["time"] = json!({ "start": start, "end": end.unwrap_or(start) });
        }
    }
    json!({
        "type": "tool",
        "tool": name,
        "callID": item.get("id").and_then(Value::as_str).unwrap_or_default(),
        "state": legacy_state,
    })
}

fn user_part(
    block: &Block,
    session: &str,
    msg_id: &str,
    i: usize,
    j: usize,
    ms: i64,
) -> Option<Value> {
    let base = |mut p: Map<String, Value>| {
        p.insert(
            "id".into(),
            json!(format!("prt_{}", det_hex(session, i, j + 1))),
        );
        p.insert("sessionID".into(), json!(session));
        p.insert("messageID".into(), json!(msg_id));
        Value::Object(p)
    };
    match block {
        Block::Text { text } => {
            let mut p = Map::new();
            p.insert("type".into(), json!("text"));
            p.insert("text".into(), json!(text));
            p.insert("time".into(), json!({ "start": ms, "end": ms }));
            Some(base(p))
        }
        Block::Image { source } => {
            let mut p = Map::new();
            p.insert("type".into(), json!("file"));
            p.insert("mime".into(), json!(source.media_type));
            p.insert(
                "url".into(),
                json!(format!(
                    "data:{};{},{}",
                    source.media_type, source.source_type, source.data
                )),
            );
            Some(base(p))
        }
        Block::Artifact { artifact } => {
            let mut p = Map::new();
            p.insert("type".into(), json!("text"));
            p.insert("text".into(), json!(artifact.display_text()));
            p.insert("time".into(), json!({ "start": ms, "end": ms }));
            Some(base(p))
        }
        // Thinking and ToolUse never occur on user turns; a ToolResult is
        // folded into the preceding tool part by `assistant_part` and has no
        // user-part shape of its own.
        Block::Thinking { .. } | Block::ToolUse { .. } | Block::ToolResult { .. } => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn assistant_part(
    block: &Block,
    session: &str,
    msg_id: &str,
    i: usize,
    j: usize,
    ms: i64,
    messages: &[Message],
    idx: &mut usize,
) -> Value {
    let time = json!({ "start": ms, "end": ms });
    let mut p = match block {
        Block::Text { text } => json!({ "type": "text", "text": text, "time": time }),
        Block::Thinking { text, .. } => json!({ "type": "reasoning", "text": text, "time": time }),
        Block::Image { source } => json!({
            "type": "file",
            "mime": source.media_type,
            "url": format!("data:{};{},{}", source.media_type, source.source_type, source.data),
            "time": time,
        }),
        Block::Artifact { artifact } => {
            json!({ "type": "text", "text": artifact.display_text(), "time": time })
        }
        Block::ToolUse { id, tool } => {
            let (name, input) = tool.to_canonical();
            let (oc_name, oc_input) = denormalize_tool(&name, input);
            let mut state = json!({
                "status": "completed",
                "input": oc_input,
                "output": "",
                "title": oc_name,
                "metadata": {},
                "time": time,
            });
            // Fold matching ToolResult from the following user turn into state.output.
            if let Some(next) = messages.get(*idx + 1)
                && matches!(next.role, Role::User)
                && let Some(result) = next.content.iter().find_map(|b| match b {
                    Block::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } if tool_use_id == id => Some((content, *is_error)),
                    _ => None,
                })
                && let Value::Object(obj) = &mut state
            {
                if result.1 {
                    obj.insert("status".into(), json!("error"));
                    obj.insert("error".into(), json!(output_to_string(result.0)));
                    obj.remove("output");
                } else {
                    obj.insert("output".into(), json!(output_to_string(result.0)));
                }
            }
            json!({ "type": "tool", "tool": oc_name, "callID": id, "state": state })
        }
        Block::ToolResult { content, .. } => {
            json!({ "type": "text", "text": output_to_string(content), "time": time })
        }
    };
    if let Value::Object(obj) = &mut p {
        obj.insert(
            "id".into(),
            json!(format!("prt_{}", det_hex(session, i, j + 1))),
        );
        obj.insert("sessionID".into(), json!(session));
        obj.insert("messageID".into(), json!(msg_id));
    }
    p
}

// ── tool normalization ─────────────────────────────────────────────────

fn normalize_tool(tool: &str, input: Value) -> (String, Value) {
    match tool {
        "bash" => ("Bash".to_string(), input),
        "edit" => (
            "Edit".to_string(),
            rename_keys(
                input,
                &[
                    ("filePath", "file_path"),
                    ("oldString", "old_string"),
                    ("newString", "new_string"),
                    ("replaceAll", "replace_all"),
                ],
            ),
        ),
        "write" => (
            "Write".to_string(),
            rename_keys(input, &[("filePath", "file_path")]),
        ),
        "read" => (
            "Read".to_string(),
            rename_keys(input, &[("filePath", "file_path")]),
        ),
        "glob" => ("Glob".to_string(), input),
        "grep" => ("Grep".to_string(), input),
        "list" => ("LS".to_string(), input),
        "todowrite" => ("TodoWrite".to_string(), input),
        "todoread" => ("TodoRead".to_string(), input),
        "webfetch" => ("WebFetch".to_string(), input),
        "task" => ("Task".to_string(), input),
        "question" => ("Question".to_string(), input),
        other => (other.to_string(), input),
    }
}

fn denormalize_tool(name: &str, input: Value) -> (String, Value) {
    match name {
        "Bash" => ("bash".to_string(), input),
        "Edit" => (
            "edit".to_string(),
            rename_keys(
                input,
                &[
                    ("file_path", "filePath"),
                    ("old_string", "oldString"),
                    ("new_string", "newString"),
                    ("replace_all", "replaceAll"),
                ],
            ),
        ),
        "Write" => (
            "write".to_string(),
            rename_keys(input, &[("file_path", "filePath")]),
        ),
        "Read" => (
            "read".to_string(),
            rename_keys(input, &[("file_path", "filePath")]),
        ),
        "Glob" => ("glob".to_string(), input),
        "Grep" => ("grep".to_string(), input),
        "LS" => ("list".to_string(), input),
        "TodoWrite" => ("todowrite".to_string(), input),
        "TodoRead" => ("todoread".to_string(), input),
        "WebFetch" => ("webfetch".to_string(), input),
        "Task" => ("task".to_string(), input),
        "Question" => ("question".to_string(), input),
        other => (other.to_string(), input),
    }
}

/// A built-in tool 2.x renamed: the name at the legacy boundary — what
/// [`denormalize_tool`] writes and [`normalize_tool`] reads — its 2.x name, and
/// the input keys as `(legacy key, 2.x key)`. Sources:
/// `packages/core/src/tool/plugin/*.ts`.
type ToolRename = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

/// 2.x renamed the built-in tools and several input keys: `bash` is `shell`
/// with `timeout`/`background`, the file tools take `path`, `task` is
/// `subagent`, and so on. Keys not listed pass through, so unknown tools and
/// extra arguments survive either direction.
const TOOLS_V2: &[ToolRename] = &[
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

/// The 2.x name and input of a tool call the legacy boundary names.
fn tool_to_v2(name: &str, input: Value) -> (String, Value) {
    let Some((_, v2_name, keys)) = TOOLS_V2.iter().find(|(legacy, ..)| *legacy == name) else {
        return (name.to_string(), input);
    };
    let mut input = rename_keys(input, keys);
    if *v2_name == "grep" {
        // `-i` means case-insensitive; 2.x's `caseSensitive` is the inverse.
        flip_flag(&mut input, "-i", "caseSensitive");
    }
    ((*v2_name).to_string(), input)
}

/// The legacy name and input of a 2.x tool call; the inverse of [`tool_to_v2`].
fn tool_from_v2(name: &str, input: Value) -> (String, Value) {
    let Some((legacy, _, keys)) = TOOLS_V2.iter().find(|(_, v2_name, _)| *v2_name == name) else {
        return (name.to_string(), input);
    };
    let reversed: Vec<(&str, &str)> = keys.iter().map(|(from, to)| (*to, *from)).collect();
    let mut input = rename_keys(input, &reversed);
    if *legacy == "grep" {
        flip_flag(&mut input, "caseSensitive", "-i");
    }
    ((*legacy).to_string(), input)
}

/// Moves a boolean from `from` to `to`, negated; any other value stays put.
fn flip_flag(input: &mut Value, from: &str, to: &str) {
    if let Value::Object(obj) = input
        && let Some(Value::Bool(flag)) = obj.get(from).cloned()
    {
        obj.remove(from);
        obj.insert(to.to_string(), Value::Bool(!flag));
    }
}

/// 2.x validates tool input as an object (a `Record`); a freeform input — a
/// Codex code-mode call, an unmodeled harness tool — rides as
/// `{"input": <value>}`, which keeps the document importable.
fn object_input(input: Value) -> Value {
    match input {
        Value::Object(_) => input,
        Value::Null => Value::Object(Map::new()),
        other => json!({ "input": other }),
    }
}

// ── small helpers ──────────────────────────────────────────────────────

fn is_synthetic(part: &Value) -> bool {
    part.get("synthetic")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn parse_file_image(part: &Value) -> Option<ImageSource> {
    let mime = part
        .get("mime")
        .and_then(Value::as_str)
        .filter(|mime| mime.starts_with("image/"))?;
    let (header, data) = part
        .get("url")
        .and_then(Value::as_str)?
        .split_once(',')
        .filter(|(header, _)| header.starts_with("data:"))?;
    // 1.x stored payloads the import schema would reject wholesale —
    // percent-encoded data URLs and base64 with wrapped lines. OpenCode's own
    // migration re-encodes both, so mirror it; a payload that survives neither
    // path is left as-is and fails wherever the consuming harness fails.
    let data = canonical_base64(header, data).unwrap_or_else(|| data.to_string());
    Some(ImageSource {
        source_type: "base64".to_string(),
        media_type: mime.to_string(),
        data,
    })
}

/// Normalizes a data-URL payload into the canonical base64 `OpenCode`'s import
/// schema demands (`length % 4 == 0 && /^[A-Za-z0-9+/]*={0,2}$/`), the way
/// `OpenCode`'s own 1.x→2.x migration does (`migrateFile`).
fn canonical_base64(header: &str, payload: &str) -> Option<String> {
    if header.ends_with(";base64") {
        if !non_canonical_base64(payload) {
            return Some(payload.to_string());
        }
        // Re-encode from the bytes: `Buffer.from(payload, "base64")` accepts
        // whitespace and missing padding, so OpenCode tolerates those on the
        // way in and emits canonical base64 on the way out.
        let cleaned: String = payload
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        let engine = base64::engine::GeneralPurpose::new(
            &base64::alphabet::STANDARD,
            base64::engine::GeneralPurposeConfig::new()
                .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
        );
        engine
            .decode(cleaned.as_bytes())
            .ok()
            .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
    } else {
        percent_decode(payload).map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
    }
}

/// True when a base64 payload isn't in the canonical form the import schema's
/// linear check requires — `length % 4 == 0` with the standard alphabet and at
/// most two trailing `=` — written as a scan so multi-megabyte payloads don't
/// hit the schema's backtracking pattern.
fn non_canonical_base64(payload: &str) -> bool {
    let bytes = payload.as_bytes();
    let mut padding = 0;
    let mut seen_padding = false;
    for &byte in bytes {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' if !seen_padding => {}
            b'=' => {
                seen_padding = true;
                padding += 1;
            }
            _ => return true,
        }
    }
    !bytes.len().is_multiple_of(4) || padding > 2
}

/// Percent-decoding, for the 1.x file parts that stored a data URL without a
/// `;base64` header (`decodeURIComponent` in `OpenCode`'s migration).
fn percent_decode(input: &str) -> Option<Vec<u8>> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                // The two bytes after a `%` in a well-formed URL are ASCII hex
                // digits; anything else (including a stray multi-byte char) is
                // a malformed encoding.
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'%' => return None,
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    Some(out)
}

fn parse_tokens(tokens: Option<&Value>) -> Option<Usage> {
    let tokens = tokens?;
    let input = tokens.get("input").and_then(Value::as_u64).unwrap_or(0);
    let output = tokens.get("output").and_then(Value::as_u64).unwrap_or(0);
    let cache = tokens.get("cache");
    // A zero cache count means "no cache activity", which is the same fact
    // `None` records; collapsing them keeps the codec a fixpoint now that
    // `tokens_value` must always write the cache object out.
    let cache_read = cache
        .and_then(|c| c.get("read"))
        .and_then(Value::as_u64)
        .filter(|&n| n != 0);
    let cache_write = cache
        .and_then(|c| c.get("write"))
        .and_then(Value::as_u64)
        .filter(|&n| n != 0);
    // An all-zero token object means no usage was recorded.
    (input != 0 || output != 0 || cache_read.is_some() || cache_write.is_some()).then_some(Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_input_tokens: cache_read,
        cache_creation_input_tokens: cache_write,
    })
}

fn tokens_value(usage: Option<&Usage>) -> Value {
    // Both import generations require the full tokens object, cache included;
    // absent counts are written as 0 (and read back as `Some(0)`).
    json!({
        "input": usage.map_or(0, |u| u.input_tokens),
        "output": usage.map_or(0, |u| u.output_tokens),
        "reasoning": 0,
        "cache": {
            "read": usage.and_then(|u| u.cache_read_input_tokens).unwrap_or(0),
            "write": usage.and_then(|u| u.cache_creation_input_tokens).unwrap_or(0),
        },
    })
}

fn parse_finish(s: &str) -> StopReason {
    match s {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_use" | "tool-calls" | "tool_calls" => StopReason::ToolUse,
        "error" => StopReason::Error,
        other => StopReason::Other(other.to_string()),
    }
}

fn finish_str(r: Option<&StopReason>) -> &'static str {
    match r {
        Some(StopReason::MaxTokens) => "length",
        Some(StopReason::ToolUse) => "tool_use",
        Some(StopReason::Error) => "error",
        // OpenCode requires a finish and has no spelling for these; they all
        // collapse to "stop".
        Some(
            StopReason::EndTurn
            | StopReason::StopSequence
            | StopReason::Aborted
            | StopReason::Other(_),
        )
        | None => "stop",
    }
}

fn value_to_output(v: Value) -> ToolOutput {
    match v {
        Value::String(s) => ToolOutput::Text(s),
        other => ToolOutput::Json(other),
    }
}

fn output_to_string(out: &ToolOutput) -> String {
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
        // A non-object input has no keys to rename; pass it through untouched.
        other => other,
    }
}

fn msg_timestamp(info: &Value, fallback: DateTime<Utc>) -> DateTime<Utc> {
    info.get("time")
        .and_then(|t| t.get("created"))
        .and_then(Value::as_i64)
        .and_then(DateTime::from_timestamp_millis)
        .unwrap_or(fallback)
}

/// Deterministic hex id fragment, so `from_common` is a pure function.
fn det_hex(session: &str, i: usize, j: usize) -> String {
    const NS: Uuid = Uuid::from_bytes([
        0x6f, 0x70, 0x65, 0x6e, 0x63, 0x6f, 0x64, 0x65, 0x2d, 0x69, 0x64, 0x2d, 0x6e, 0x73, 0x21,
        0x21,
    ]);
    Uuid::new_v5(&NS, format!("{session}:{i}:{j}").as_bytes())
        .simple()
        .to_string()
}

// ── store (SQLite) ─────────────────────────────────────────────────────

#[cfg(feature = "opencode")]
pub use store::OpenCodeStore;

#[cfg(feature = "opencode")]
mod store {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use rusqlite::{Connection, OpenFlags};
    use serde_json::{Value, json};

    use super::{
        Export, MessageRecord, OpenCode, build_v2_document, v2_assistant_record, v2_user_record,
    };
    use crate::common::{Lineage, Meta, Relation};
    use crate::error::{Error, Result};
    use crate::transcript::{Discovered, Saved, Store, Transcript};
    use chrono::{DateTime, Utc};

    /// Reads and writes `OpenCode` sessions in a `SQLite` database. A [`Ref`] is a
    /// session id, since every session shares the one database file.
    ///
    /// [`Ref`]: Store::Ref
    #[derive(Debug, Clone)]
    pub struct OpenCodeStore {
        pub db_path: PathBuf,
    }

    impl OpenCodeStore {
        pub fn new(db_path: impl Into<PathBuf>) -> Self {
            Self {
                db_path: db_path.into(),
            }
        }

        /// The default database, honoring `OPENCODE_DB` / `XDG_DATA_HOME`,
        /// falling back to `~/.local/share/opencode/opencode.db`.
        pub fn default_db() -> Option<Self> {
            std::env::var_os("OPENCODE_DB")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("XDG_DATA_HOME")
                        .filter(|v| !v.is_empty())
                        .map(|xdg| PathBuf::from(xdg).join("opencode").join("opencode.db"))
                })
                .or_else(|| {
                    crate::harness::home_dir().map(|home| {
                        home.join(".local")
                            .join("share")
                            .join("opencode")
                            .join("opencode.db")
                    })
                })
                .map(Self::new)
        }

        fn open(&self) -> Result<Connection> {
            Connection::open_with_flags(&self.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(sqlite_err)
        }

        /// Whether the installed `opencode` binary speaks 2.x: it moved the
        /// import command and flattened the document, so the generation picks
        /// both. Reads the binary rather than the database because an upgraded
        /// CLI migrates an old database on its next start — the importer may
        /// have to run before that has happened.
        fn cli_generation() -> Result<Schema> {
            use std::process::{Command, Stdio};

            let output = Command::new("opencode")
                .arg("--version")
                .stdin(Stdio::null())
                .output()
                .map_err(|e| Error::Unconvertible {
                    harness: "opencode",
                    detail: format!("running `opencode --version`: {e} (is OpenCode on PATH?)"),
                })?;
            Ok(parse_generation(&format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )))
        }

        /// `session import` in 2.x requires `info.projectID`, but the server
        /// ignores it — it resolves the project from the `--directory` and
        /// rewrites the stored row id (`transfer.ts`), so the field only has to
        /// satisfy the schema. txcript still emits the real row id when the
        /// read-only `project` table matches, so the document looks like one
        /// `OpenCode` itself would have written, and falls back to the schema's
        /// own `"global"` static otherwise.
        fn project_id(&self, directory: Option<&Path>) -> String {
            let dir = directory
                .map(Path::to_path_buf)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."));
            self.find_project(&dir)
                .ok()
                .flatten()
                .unwrap_or_else(|| "global".to_string())
        }

        fn find_project(&self, dir: &Path) -> Result<Option<String>> {
            if !self.db_path.is_file() {
                return Ok(None);
            }
            let conn = self.open()?;
            if schema_of(&conn)? != Schema::V2 {
                return Ok(None);
            }
            // `project.worktree` stores the canonical git/hg root: OpenCode
            // resolves a project by walking up from the launch directory to the
            // nearest marker (`Project.root`), so a directory inside the
            // repository — txcript run from a subdirectory, or pointed there —
            // belongs to the same project. Match ancestors too and take the
            // nearest (deepest) worktree, mirroring that walk.
            let mut stmt = conn
                .prepare("SELECT id, worktree FROM project")
                .map_err(sqlite_err)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(sqlite_err)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sqlite_err)?;
            let Some(wanted) = dir.canonicalize().ok() else {
                return Ok(None);
            };
            let mut best: Option<(String, usize)> = None;
            for (id, worktree) in rows {
                let Some(candidate) = Path::new(&worktree).canonicalize().ok() else {
                    continue;
                };
                if wanted != candidate && !wanted.starts_with(&candidate) {
                    continue;
                }
                let depth = candidate.components().count();
                if best.as_ref().is_none_or(|(_, d)| depth > *d) {
                    best = Some((id, depth));
                }
            }
            Ok(best.map(|(id, _)| id))
        }
    }

    /// Which generation of `OpenCode`'s storage the database speaks. 2.x keeps the legacy
    /// `session`/`message`/`part` tables for rows it migrated but writes every new session to
    /// `session_v2` + `session_message`, and its import command and document shape differ.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Schema {
        Legacy,
        V2,
    }

    fn schema_of(conn: &Connection) -> Result<Schema> {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND name IN ('session_v2', 'session_message')",
            )
            .map_err(sqlite_err)?;
        let found = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sqlite_err)?;
        Ok(if found.len() == 2 {
            Schema::V2
        } else {
            Schema::Legacy
        })
    }

    /// `opencode v2.1.4`, `2.0.3`, `v1.17.20`: the flat document and
    /// `session import` arrived in 2.1. A version that cannot be read is taken
    /// as current — the error it produces names the command.
    fn parse_generation(text: &str) -> Schema {
        let version = text.split_whitespace().find_map(|word| {
            let mut numbers = word.trim_start_matches('v').split('.');
            let major: u32 = numbers.next()?.parse().ok()?;
            let minor: u32 = numbers.next()?.parse().ok()?;
            Some((major, minor))
        });
        match version {
            Some(version) if version < (2, 1) => Schema::Legacy,
            _ => Schema::V2,
        }
    }

    fn discover_v2(conn: &Connection) -> Result<Vec<Discovered<String>>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, directory, title, version, time_created, model, parent_id \
                 FROM session_v2 WHERE time_archived IS NULL ORDER BY time_created DESC",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(sqlite_err)?;
        let raw: Vec<_> = rows.collect::<rusqlite::Result<_>>().map_err(sqlite_err)?;
        Ok(raw
            .into_iter()
            .map(
                |(id, directory, title, version, time_created, model_json, parent_id)| Discovered {
                    meta: Meta {
                        id: id.clone(),
                        timestamp: time_created
                            .and_then(DateTime::from_timestamp_millis)
                            .unwrap_or_else(Utc::now),
                        cwd: directory,
                        git_branch: None,
                        title: title.filter(|t| !is_placeholder_title(t)),
                        cli_version: version,
                        model: model_json.as_deref().and_then(model_id),
                        lineage: lineage_from_parent_id(parent_id),
                    },
                    reference: id,
                },
            )
            .collect())
    }

    fn load_v2(conn: &Connection, reference: &str) -> Result<Transcript<OpenCode>> {
        let (meta, info) = session_row_v2(conn, reference)?;
        let messages = v2_records(conn, reference)?;
        Ok(Transcript::new(meta, Export { info, messages }))
    }

    /// 2.x messages are one flat JSON object each; they are expanded back into the legacy
    /// `{info, parts}` record so `to_common` stays the single code path for both generations.
    fn v2_records(conn: &Connection, session: &str) -> Result<Vec<MessageRecord>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, type, data, time_created FROM session_message \
                 WHERE session_id = ?1 ORDER BY seq",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map([session], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(sqlite_err)?;

        let mut out = Vec::new();
        for row in rows {
            let (id, kind, data, ts) = row.map_err(sqlite_err)?;
            // A row whose JSON doesn't parse is dropped; the rest of the session survives.
            let Ok(data) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            match kind.as_str() {
                "user" => out.push(v2_user_record(&id, &data, ts)),
                "assistant" => out.push(v2_assistant_record(&id, &data, ts)),
                // Records with no 1.x shape (`compaction`, `idle`, `shell`,
                // `synthetic`, `system`, …) ride through verbatim: the codec
                // skips a record without a role, and a document rebuilt from
                // this body puts them back unchanged.
                _ => out.push(MessageRecord {
                    info: verbatim_record(&id, &kind, &data, ts),
                    parts: Vec::new(),
                }),
            }
        }
        Ok(out)
    }

    /// A 2.x record as it stands, with the columnar `id`/`type` and a fallback
    /// `time.created` injected so it is a complete document record.
    fn verbatim_record(id: &str, kind: &str, data: &Value, ts: i64) -> Value {
        let mut record = data.clone();
        if let Value::Object(obj) = &mut record {
            obj.insert("id".into(), json!(id));
            obj.insert("type".into(), json!(kind));
            obj.entry("time")
                .or_insert_with(|| json!({ "created": ts }));
        }
        record
    }

    fn session_row_v2(conn: &Connection, id: &str) -> Result<(Meta, Value)> {
        let mut stmt = conn
            .prepare(
                "SELECT directory, title, version, time_created, model, time_updated, parent_id \
                 FROM session_v2 WHERE id = ?1",
            )
            .map_err(sqlite_err)?;
        let row = stmt
            .query_row([id], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(sqlite_err)?;
        let (directory, title, version, time_created, model_json, time_updated, parent_id) = row;
        let timestamp = time_created
            .and_then(DateTime::from_timestamp_millis)
            .unwrap_or_else(Utc::now);
        let meta = Meta {
            id: id.to_string(),
            timestamp,
            cwd: directory.clone(),
            git_branch: None,
            title: title.clone().filter(|t| !is_placeholder_title(t)),
            cli_version: version.clone(),
            model: model_json.as_deref().and_then(model_id),
            lineage: lineage_from_parent_id(parent_id),
        };
        let info = json!({
            "id": id,
            "directory": directory.unwrap_or_default(),
            "title": title.unwrap_or_default(),
            "version": version.unwrap_or_default(),
            "model": model_json.as_deref().map_or(Value::Null, |m| {
                serde_json::from_str::<Value>(m).unwrap_or(Value::Null)
            }),
            "time": {
                "created": time_created.unwrap_or(0),
                "updated": time_updated.or(time_created).unwrap_or(0),
            },
        });
        Ok((meta, info))
    }

    /// Hands the document to `OpenCode`'s own importer — the schema stays `OpenCode`'s problem.
    /// 2.x moved the command under `session`, hence the subcommand flip.
    fn import_document(payload: &str, v2: bool, directory: Option<&Path>) -> Result<String> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        // A randomized name plus `create_new` keeps the staging file off any predictable,
        // pre-plantable path (and never follows a symlink); 0600 keeps the transcript private in
        // a world-shared temp dir.
        let tmp =
            std::env::temp_dir().join(format!("opencode-import-{}.json", uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(payload.as_bytes())?;
        drop(file);

        let mut command = Command::new("opencode");
        if v2 {
            command.arg("session");
        }
        command.arg("import").arg(&tmp);
        if v2 && let Some(dir) = directory {
            command.arg("--directory").arg(dir);
        }
        let output = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| Error::Unconvertible {
                harness: "opencode",
                detail: format!("running `opencode import`: {e} (is OpenCode on PATH?)"),
            })?;
        if output.status.success() {
            let _ = std::fs::remove_file(&tmp);
            parse_import_output(
                payload,
                &String::from_utf8_lossy(&output.stdout),
                &String::from_utf8_lossy(&output.stderr),
                v2,
            )
        } else {
            // The staged document is left behind on purpose, for debugging.
            Err(Error::Unconvertible {
                harness: "opencode",
                detail: format!(
                    "`opencode import` failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            })
        }
    }

    /// Interprets `opencode import` output. 2.x rejects a duplicate id with a
    /// 409, and its CLI reports that by printing `Session already exists` to
    /// stderr while still exiting 0 with empty stdout — treating it as success
    /// would leave the stored session silently stale, so surface the conflict.
    fn parse_import_output(payload: &str, stdout: &str, stderr: &str, v2: bool) -> Result<String> {
        if v2 && stderr.contains("Session already exists") {
            let id = serde_json::from_str::<Value>(payload)
                .ok()
                .and_then(|doc| {
                    doc.pointer("/info/id")
                        .and_then(Value::as_str)
                        .map(String::from)
                })
                .unwrap_or_default();
            return Err(Error::Unconvertible {
                harness: "opencode",
                detail: if id.is_empty() {
                    "`opencode session import` rejected the document: a session with this \
                     id already exists (2.x refuses duplicates instead of updating them)."
                        .to_string()
                } else {
                    format!(
                        "`opencode session import` rejected the document: session `{id}` \
                         already exists (2.x refuses a duplicate id instead of updating \
                         the stored session)."
                    )
                },
            });
        }
        Ok(stdout
            .lines()
            .find_map(|l| l.strip_prefix("Imported session: ").map(str::trim))
            .unwrap_or_default()
            .to_string())
    }

    impl Store for OpenCodeStore {
        type H = OpenCode;
        type Ref = String;

        fn discover(&self) -> Result<Vec<Discovered<String>>> {
            if self.db_path.is_file() {
                let conn = self.open()?;
                if schema_of(&conn)? == Schema::V2 {
                    return discover_v2(&conn);
                }
                let mut stmt = conn
                    .prepare(
                        "SELECT id, directory, title, version, time_created, model, parent_id \
                         FROM session WHERE time_archived IS NULL ORDER BY time_created DESC",
                    )
                    .map_err(sqlite_err)?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<i64>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, Option<String>>(6)?,
                        ))
                    })
                    .map_err(sqlite_err)?;
                let raw: Vec<_> = rows.collect::<rusqlite::Result<_>>().map_err(sqlite_err)?;
                Ok(raw
                    .into_iter()
                    .map(
                        |(id, directory, title, version, time_created, model_json, parent_id)| {
                            Discovered {
                                meta: Meta {
                                    id: id.clone(),
                                    timestamp: time_created
                                        .and_then(DateTime::from_timestamp_millis)
                                        .unwrap_or_else(Utc::now),
                                    cwd: directory,
                                    git_branch: None,
                                    title: title.filter(|t| !is_placeholder_title(t)),
                                    cli_version: version,
                                    model: model_json.as_deref().and_then(model_id),
                                    lineage: lineage_from_parent_id(parent_id),
                                },
                                reference: id,
                            }
                        },
                    )
                    .collect())
            } else {
                // No database means OpenCode has no sessions yet, not an error.
                Ok(Vec::new())
            }
        }

        fn load(&self, reference: &String) -> Result<Transcript<OpenCode>> {
            let conn = self.open()?;
            if schema_of(&conn)? == Schema::V2 {
                return load_v2(&conn, reference);
            }

            let (meta, info) = session_row(&conn, reference)?;

            let mut msg_stmt = conn
                .prepare(
                    "SELECT id, data, time_created FROM message \
                     WHERE session_id = ?1 ORDER BY time_created, id",
                )
                .map_err(sqlite_err)?;
            let message_rows: Vec<(String, String, i64)> = msg_stmt
                .query_map([reference], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map_err(sqlite_err)?
                .collect::<rusqlite::Result<_>>()
                .map_err(sqlite_err)?;
            drop(msg_stmt);

            let mut part_stmt = conn
                .prepare(
                    "SELECT message_id, data FROM part \
                     WHERE session_id = ?1 ORDER BY message_id, id",
                )
                .map_err(sqlite_err)?;
            let part_rows = part_stmt
                .query_map([reference], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(sqlite_err)?;
            let mut parts_by_message: HashMap<String, Vec<Value>> = HashMap::new();
            for row in part_rows {
                let (message_id, data) = row.map_err(sqlite_err)?;
                // A part row whose JSON doesn't parse is dropped; the message
                // survives without it.
                if let Ok(value) = serde_json::from_str::<Value>(&data) {
                    parts_by_message.entry(message_id).or_default().push(value);
                }
            }
            drop(part_stmt);

            let mut messages = Vec::with_capacity(message_rows.len());
            for (message_id, data, time_created) in message_rows {
                let mut msg_info: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
                // The export shape carries time on the message; the DB keeps it
                // in a column. Inject it so the codec reads a uniform shape.
                if let Value::Object(obj) = &mut msg_info {
                    obj.entry("time")
                        .or_insert_with(|| serde_json::json!({ "created": time_created, "completed": time_created }));
                }
                messages.push(MessageRecord {
                    info: msg_info,
                    parts: parts_by_message.remove(&message_id).unwrap_or_default(),
                });
            }

            Ok(Transcript::new(meta, Export { info, messages }))
        }

        fn save(&self, transcript: &Transcript<OpenCode>) -> Result<Saved<String>> {
            // OpenCode owns its schema; hand it the document and let its own importer normalize
            // and persist. Which document (and which command) depends on the generation of the
            // installed CLI. Requires the `opencode` binary.
            let export = &transcript.body;
            // The recorded cwd may have been a temp checkout that no longer exists; imports land
            // in a live project, so fall back to the process directory in that case.
            let recorded = export
                .info
                .get("directory")
                .and_then(Value::as_str)
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from);
            let directory = recorded
                .filter(|dir| dir.is_dir())
                .or_else(|| std::env::current_dir().ok());

            let imported = match Self::cli_generation()? {
                Schema::Legacy => import_document(&serde_json::to_string(export)?, false, None)?,
                Schema::V2 => {
                    let project_id = self.project_id(directory.as_deref());
                    let mut document = build_v2_document(export, &project_id);
                    // Keep the document's location in step with the directory actually imported.
                    if let Some(dir) = &directory {
                        document["info"]["location"]["directory"] =
                            json!(dir.display().to_string());
                    }
                    import_document(
                        &serde_json::to_string(&document)?,
                        true,
                        directory.as_deref(),
                    )?
                }
            };
            // Fall back to the id txcript minted when the CLI printed none.
            let id = if imported.is_empty() {
                export
                    .info
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            } else {
                imported
            };
            Ok(Saved {
                reference: id.clone(),
                id,
            })
        }

        /// Archives the session in place — `OpenCode`'s own notion of
        /// deletion. `discover` (here and in `OpenCode`'s UI) filters
        /// archived sessions out; the rows stay, reversible from `OpenCode`.
        fn delete(&self, reference: &String) -> Result<()> {
            let conn = Connection::open(&self.db_path).map_err(sqlite_err)?;
            let table = if schema_of(&conn)? == Schema::V2 {
                "session_v2"
            } else {
                "session"
            };
            let now = chrono::Utc::now().timestamp_millis();
            let updated = conn
                .execute(
                    &format!(
                        "UPDATE {table} SET time_archived = ?1 \
                         WHERE id = ?2 AND time_archived IS NULL"
                    ),
                    rusqlite::params![now, reference],
                )
                .map_err(sqlite_err)?;
            if updated == 0 {
                Err(Error::Malformed {
                    harness: "opencode",
                    detail: format!("no active session `{reference}` to archive"),
                })
            } else {
                Ok(())
            }
        }

        fn fingerprints(&self, refs: &[String]) -> Result<HashMap<String, String>> {
            if self.db_path.is_file() {
                let conn = self.open()?;
                let table = if schema_of(&conn)? == Schema::V2 {
                    "session_message"
                } else {
                    "message"
                };
                let mut stmt = conn
                    .prepare(&format!(
                        "SELECT session_id, MAX(time_created) FROM {table} GROUP BY session_id"
                    ))
                    .map_err(sqlite_err)?;
                let rows = stmt
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                        ))
                    })
                    .map_err(sqlite_err)?;
                let max_by: HashMap<String, i64> = rows
                    .map(|row| row.map_err(sqlite_err))
                    .collect::<Result<_>>()?;
                Ok(refs
                    .iter()
                    .map(|s| (s.clone(), max_by.get(s).copied().unwrap_or(0).to_string()))
                    .collect())
            } else {
                // No database yet: sessions have no fingerprints to report.
                Ok(HashMap::new())
            }
        }
    }

    fn session_row(conn: &Connection, id: &str) -> Result<(Meta, Value)> {
        let mut stmt = conn
            .prepare(
                "SELECT directory, title, version, time_created, model, slug, time_updated, \
                        parent_id \
                 FROM session WHERE id = ?1",
            )
            .map_err(sqlite_err)?;
        let row = stmt
            .query_row([id], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })
            .map_err(sqlite_err)?;
        let (directory, title, version, time_created, model_json, slug, time_updated, parent_id) =
            row;
        let timestamp = time_created
            .and_then(DateTime::from_timestamp_millis)
            .unwrap_or_else(Utc::now);
        let meta = Meta {
            id: id.to_string(),
            timestamp,
            cwd: directory.clone(),
            git_branch: None,
            title: title.clone().filter(|t| !is_placeholder_title(t)),
            cli_version: version.clone(),
            model: model_json.as_deref().and_then(model_id),
            lineage: lineage_from_parent_id(parent_id),
        };
        let info = serde_json::json!({
            "id": id,
            "slug": slug.unwrap_or_default(),
            "directory": directory.unwrap_or_default(),
            "title": title.unwrap_or_default(),
            "version": version.unwrap_or_default(),
            "time": {
                "created": time_created.unwrap_or(0),
                "updated": time_updated.or(time_created).unwrap_or(0),
            },
        });
        Ok((meta, info))
    }

    fn is_placeholder_title(title: &str) -> bool {
        title.trim().is_empty() || title.starts_with("New session - ")
    }

    /// `session.parent_id` names the session that spawned this one (`OpenCode`'s
    /// subagent/task tool starts a child session with its own row). A non-empty
    /// value is always a spawn — `OpenCode` has no fork/continue relation that
    /// reuses this column.
    fn lineage_from_parent_id(parent_id: Option<String>) -> Option<Lineage> {
        parent_id.filter(|id| !id.is_empty()).map(|parent| Lineage {
            parent,
            relation: Relation::Spawn,
        })
    }

    fn model_id(model_json: &str) -> Option<String> {
        serde_json::from_str::<Value>(model_json)
            .ok()?
            .get("id")
            .and_then(Value::as_str)
            .map(String::from)
    }

    // Passed point-free to `map_err`, which hands over the error by value.
    #[allow(clippy::needless_pass_by_value)]
    fn sqlite_err(e: rusqlite::Error) -> Error {
        Error::Malformed {
            harness: "opencode",
            detail: e.to_string(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::OpenCode;
        use super::OpenCodeStore;
        use crate::common::{Block, Relation, Role, Tool};
        use crate::transcript::{Codec, Store};
        use rusqlite::{Connection, params};

        /// Build a minimal `OpenCode` database: a session with one user message
        /// and one assistant message whose parts cover reasoning, text, a
        /// completed edit tool, and trailing text.
        fn make_db() -> std::path::PathBuf {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("transcript-oc-test-{n}-{:?}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("opencode.db");
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, title TEXT, \
                   version TEXT, time_created INTEGER, time_archived INTEGER, model TEXT, \
                   slug TEXT, time_updated INTEGER, parent_id TEXT);\
                 CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);\
                 CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, data TEXT);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO session VALUES ('ses_1','/repo','Demo','1.15.0',1778834704515,NULL,'{\"id\":\"claude-opus-4-7\"}','clever-engine',1778834704516,NULL)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message VALUES ('m_u','ses_1',1778834704520,'{\"role\":\"user\"}')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO part VALUES ('p_u','m_u','ses_1','{\"type\":\"text\",\"text\":\"edit it\"}')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message VALUES ('m_a','ses_1',1_778_834_704_540,?1)",
                params![r#"{"role":"assistant","modelID":"claude-opus-4-7","finish":"stop","tokens":{"input":6,"output":88,"cache":{"write":21428,"read":10}}}"#],
            )
            .unwrap();
            for (id, data) in [
                ("p_a1", r#"{"type":"reasoning","text":"thinking"}"#),
                ("p_a2", r#"{"type":"text","text":"On it."}"#),
                (
                    "p_a3",
                    r#"{"type":"tool","tool":"edit","callID":"c1","state":{"status":"completed","input":{"filePath":"/repo/a.rs","oldString":"old","newString":"new"},"output":"done"}}"#,
                ),
                ("p_a4", r#"{"type":"text","text":"Finished."}"#),
            ] {
                conn.execute(
                    "INSERT INTO part VALUES (?1,'m_a','ses_1',?2)",
                    params![id, data],
                )
                .unwrap();
            }
            path
        }

        #[test]
        fn discover_and_load_from_sqlite() {
            let path = make_db();
            let store = OpenCodeStore::new(&path);

            let found = store.discover().unwrap();
            assert_eq!(found.len(), 1);
            let d = &found[0];
            assert_eq!(d.reference, "ses_1");
            assert_eq!(d.meta.title.as_deref(), Some("Demo"));
            assert_eq!(d.meta.model.as_deref(), Some("claude-opus-4-7"));
            assert_eq!(d.meta.cwd.as_deref(), Some("/repo"));
            assert!(d.meta.lineage.is_none(), "no parent_id, no lineage");

            let native = store.load(&d.reference).unwrap();
            let msgs = OpenCode::to_common(&native).unwrap().body;
            // user | assistant(reasoning+text) | assistant(tool) | user(result) | assistant(text)
            assert_eq!(msgs.len(), 5);
            assert_eq!(msgs[0].role, Role::User);
            assert!(matches!(
                &msgs[2].content[0],
                Block::ToolUse { id, tool: Tool::Edit { file_path, .. } }
                    if id == "c1" && file_path == "/repo/a.rs"
            ));
            assert!(
                matches!(&msgs[3].content[0], Block::ToolResult { tool_use_id, .. } if tool_use_id == "c1")
            );
            let usage = msgs[4].usage.unwrap();
            assert_eq!(usage.input_tokens, 6);
            assert_eq!(usage.cache_creation_input_tokens, Some(21428));
        }

        #[test]
        fn discover_skips_archived_sessions() {
            let path = make_db();
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "INSERT INTO session VALUES ('ses_2','/repo','Archived','1.15.0',1778834704600,1778834704999,NULL,'quiet-harbor',1778834704601,NULL)",
                [],
            )
            .unwrap();
            drop(conn);
            let found = OpenCodeStore::new(&path).discover().unwrap();
            assert!(found.iter().all(|d| d.reference != "ses_2"));
        }

        /// `OpenCode` 2.x's tables: `session_v2` plus one flat
        /// `session_message` per turn.
        fn make_v2_db() -> std::path::PathBuf {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("transcript-oc2-test-{n}-{:?}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("opencode.db");
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE project (id TEXT PRIMARY KEY, worktree TEXT NOT NULL);\
                 CREATE TABLE session_v2 (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, \
                   slug TEXT NOT NULL, directory TEXT NOT NULL, title TEXT, version TEXT NOT NULL, \
                   model TEXT, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, \
                   time_archived INTEGER, parent_id TEXT);\
                 CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, \
                   type TEXT NOT NULL, seq INTEGER NOT NULL, time_created INTEGER NOT NULL, \
                   time_updated INTEGER NOT NULL, data TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute("INSERT INTO project VALUES ('prj_1','/repo')", [])
                .unwrap();
            conn.execute(
                "INSERT INTO session_v2 VALUES ('ses_v2','prj_1','demo','/repo','Demo','2.3.0',\
                   '{\"id\":\"claude-opus-4-7\",\"providerID\":\"anthropic\"}',1778834704515,1778834704516,NULL,NULL)",
                [],
            )
            .unwrap();
            for (id, kind, seq, ts, data) in [
                (
                    "msg_u",
                    "user",
                    1,
                    1_778_834_704_520_i64,
                    r#"{"text":"edit it","files":[],"agents":[]}"#,
                ),
                (
                    "msg_a1",
                    "assistant",
                    2,
                    1_778_834_704_540,
                    r#"{"time":{"created":1778834704540,"completed":1778834704550},"agent":"build","model":{"id":"claude-opus-4-7","providerID":"anthropic"},"content":[{"type":"reasoning","text":"thinking"},{"type":"text","text":"On it."},{"type":"tool","id":"c1","name":"edit","state":{"status":"completed","input":{"filePath":"/repo/a.rs","oldString":"old","newString":"new"},"content":[{"type":"text","text":"done"}],"metadata":{}}},{"type":"text","text":"Finished."}],"finish":"tool-calls","cost":0,"tokens":{"input":6,"output":88,"reasoning":0,"cache":{"read":10,"write":21428}}}"#,
                ),
                (
                    "msg_idle",
                    "idle",
                    3,
                    1_778_834_704_560,
                    r#"{"outcome":"succeeded"}"#,
                ),
            ] {
                conn.execute(
                    "INSERT INTO session_message VALUES (?1,'ses_v2',?2,?3,?4,?4,?5)",
                    params![id, kind, seq, ts, data],
                )
                .unwrap();
            }
            path
        }

        #[test]
        fn discover_and_load_from_v2_tables() {
            let path = make_v2_db();
            let store = OpenCodeStore::new(&path);

            let found = store.discover().unwrap();
            assert_eq!(found.len(), 1, "the idle marker is not a session");
            let d = &found[0];
            assert_eq!(d.reference, "ses_v2");
            assert_eq!(d.meta.title.as_deref(), Some("Demo"));
            assert_eq!(d.meta.model.as_deref(), Some("claude-opus-4-7"));
            assert_eq!(d.meta.cwd.as_deref(), Some("/repo"));

            let native = store.load(&d.reference).unwrap();
            let msgs = OpenCode::to_common(&native).unwrap().body;
            // user | assistant(reasoning+text) | assistant(tool) | user(result) | assistant(text)
            assert_eq!(msgs.len(), 5);
            assert_eq!(msgs[0].role, Role::User);
            assert!(matches!(
                &msgs[2].content[0],
                Block::ToolUse { id, tool: Tool::Edit { file_path, .. } }
                    if id == "c1" && file_path == "/repo/a.rs"
            ));
            assert!(
                matches!(&msgs[3].content[0], Block::ToolResult { tool_use_id, .. } if tool_use_id == "c1")
            );
            let usage = msgs[4].usage.unwrap();
            assert_eq!(usage.input_tokens, 6);
            assert_eq!(usage.cache_creation_input_tokens, Some(21428));
        }

        #[test]
        fn v2_delete_archives_the_row() {
            let path = make_v2_db();
            let store = OpenCodeStore::new(&path);
            store.delete(&"ses_v2".to_string()).unwrap();
            assert!(store.discover().unwrap().is_empty());
            // Archived, not removed: 2.x keeps the row for its own UI.
            let conn = Connection::open(&path).unwrap();
            let archived: i64 = conn
                .query_row(
                    "SELECT time_archived FROM session_v2 WHERE id = 'ses_v2'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(archived > 0);
        }

        #[test]
        fn v2_fingerprints_track_session_message_times() {
            let path = make_v2_db();
            let store = OpenCodeStore::new(&path);
            let prints = store.fingerprints(&["ses_v2".to_string()]).unwrap();
            assert_eq!(
                prints.get("ses_v2"),
                Some(&1_778_834_704_560_i64.to_string())
            );
        }

        /// `session.parent_id` names a spawning session; the signal is a
        /// bare column read, so both `discover` and `load` surface it
        /// without any extra query.
        #[test]
        fn parent_id_column_becomes_spawn_lineage() {
            let path = make_db();
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "INSERT INTO session VALUES ('ses_child','/repo','Task','1.15.0',1778834704700,NULL,NULL,'quiet-owl',1778834704701,'ses_1')",
                [],
            )
            .unwrap();
            drop(conn);

            let store = OpenCodeStore::new(&path);
            let found = store.discover().unwrap();
            let child = found
                .iter()
                .find(|d| d.reference == "ses_child")
                .expect("child session discovered");
            let lineage = child.meta.lineage.as_ref().expect("spawn lineage");
            assert_eq!(lineage.parent, "ses_1");
            assert_eq!(lineage.relation, Relation::Spawn);

            let loaded = store.load(&child.reference).unwrap();
            let lineage = loaded.meta.lineage.as_ref().expect("spawn lineage on load");
            assert_eq!(lineage.parent, "ses_1");
            assert_eq!(lineage.relation, Relation::Spawn);
        }

        #[test]
        fn project_lookup_matches_subdirectories_of_the_worktree() {
            // `project.worktree` holds the canonical repo root: OpenCode
            // resolves a project by walking up to the nearest marker, so
            // txcript run from (or pointed at) a subdirectory belongs to the
            // same project.
            let path = make_v2_db();
            let root = path.parent().unwrap().join("repo");
            std::fs::create_dir_all(root.join("src")).unwrap();
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "UPDATE project SET worktree = ?1",
                params![root.display().to_string()],
            )
            .unwrap();
            drop(conn);

            let store = OpenCodeStore::new(&path);
            assert_eq!(store.project_id(Some(root.as_path())), "prj_1");
            assert_eq!(store.project_id(Some(root.join("src").as_path())), "prj_1");

            // A directory no project row encloses falls back to the schema's
            // own static instead of failing the save.
            let elsewhere = path.parent().unwrap().join("elsewhere");
            std::fs::create_dir_all(&elsewhere).unwrap();
            assert_eq!(store.project_id(Some(elsewhere.as_path())), "global");
        }

        #[test]
        fn cli_generation_reads_the_import_surface_from_the_version() {
            assert_eq!(
                super::parse_generation("opencode v2.3.0"),
                super::Schema::V2
            );
            assert_eq!(super::parse_generation("2.1.4"), super::Schema::V2);
            assert_eq!(
                super::parse_generation("opencode v1.19.5"),
                super::Schema::Legacy
            );
            assert_eq!(super::parse_generation("2.0.3"), super::Schema::Legacy);
            // Unreadable output is taken as current; the error it produces
            // names the command, so a wrong guess is diagnosable.
            assert_eq!(super::parse_generation("unknown"), super::Schema::V2);
        }

        #[test]
        fn duplicate_import_is_an_error_not_a_silent_success() {
            // 2.x reports a conflicting id on stderr while exiting 0; treating
            // that as success would leave the stored session silently stale.
            let err = super::parse_import_output(
                r#"{"info":{"id":"ses_1"}}"#,
                "",
                "Session already exists\n",
                true,
            )
            .unwrap_err();
            assert!(err.to_string().contains("ses_1"));

            // The happy path still parses the printed id, and the conflict
            // marker is only treated as one in the 2.x shape.
            let id = super::parse_import_output(
                r#"{"info":{"id":"ses_1"}}"#,
                "Imported session: ses_9\n",
                "",
                false,
            )
            .unwrap();
            assert_eq!(id, "ses_9");
        }
    }
}

#[cfg(test)]
mod v2_document_tests {
    use base64::Engine as _;
    use chrono::{DateTime, Utc};
    use serde_json::{Value, json};

    use super::{OpenCode, build_v2_document, parse_file_image};
    use crate::common::{Block, ImageSource, Message, Meta, Role, Tool, ToolOutput};
    use crate::transcript::{Codec, Common, TextCodec, Transcript};

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn sample() -> Transcript<Common> {
        let meta = Meta {
            id: "ses_1".into(),
            timestamp: ts("2026-01-02T03:04:05.000Z"),
            cwd: Some("/repo".into()),
            git_branch: None,
            title: Some("Demo".into()),
            cli_version: Some("2.3.0".into()),
            model: Some("claude-opus-4-7".into()),
            lineage: None,
        };
        let model = || Some("claude-opus-4-7".to_string());
        let body = vec![
            Message {
                role: Role::User,
                content: vec![
                    Block::Image {
                        source: ImageSource {
                            source_type: "base64".into(),
                            media_type: "image/png".into(),
                            data: "AAAA".into(),
                        },
                    },
                    Block::Text {
                        text: "please edit".into(),
                    },
                ],
                timestamp: ts("2026-01-02T03:04:06.000Z"),
                model: None,
                stop_reason: None,
                usage: None,
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    Block::Thinking {
                        text: "thinking".into(),
                        signature: None,
                        encrypted: None,
                    },
                    Block::Text {
                        text: "On it.".into(),
                    },
                    Block::ToolUse {
                        id: "c1".into(),
                        tool: Tool::from_canonical("Bash", json!({ "command": "ls" })),
                    },
                ],
                timestamp: ts("2026-01-02T03:04:07.000Z"),
                model: model(),
                stop_reason: None,
                usage: None,
            },
            Message {
                role: Role::User,
                content: vec![Block::ToolResult {
                    tool_use_id: "c1".into(),
                    content: ToolOutput::Text("ok".into()),
                    is_error: false,
                }],
                timestamp: ts("2026-01-02T03:04:08.000Z"),
                model: None,
                stop_reason: None,
                usage: None,
            },
        ];
        Transcript::new(meta, body)
    }

    #[test]
    fn flattens_the_legacy_export_into_the_2x_document() {
        let native = OpenCode::from_common(&sample()).unwrap();
        let doc = build_v2_document(&native.body, "prj_1");

        // 2.x refuses an import without the project row and the location object.
        assert_eq!(doc["info"]["projectID"], "prj_1");
        assert_eq!(doc["info"]["location"]["directory"], "/repo");
        assert_eq!(doc["info"]["title"], "Demo");
        assert_eq!(doc["info"]["outcome"], "succeeded");

        let user = &doc["messages"][0];
        assert_eq!(user["type"], "user");
        assert_eq!(user["text"], "[Image 1]\n\nplease edit");
        assert_eq!(user["files"][0]["mime"], "image/png");
        assert_eq!(user["files"][0]["data"], "AAAA");
        assert_eq!(user["files"][0]["mention"]["text"], "[Image 1]");

        let assistant = &doc["messages"][1];
        assert_eq!(assistant["type"], "assistant");
        assert_eq!(assistant["content"][0]["type"], "reasoning");
        assert_eq!(assistant["content"][1]["type"], "text");
        let tool = &assistant["content"][2];
        assert_eq!(tool["type"], "tool");
        // 2.x renamed `bash` to `shell`.
        assert_eq!(tool["name"], "shell");
        assert_eq!(tool["id"], "c1");
        assert_eq!(tool["state"]["status"], "completed");
        // The result moved from `state.output` into 2.x's `state.content`.
        assert_eq!(tool["state"]["content"][0]["text"], "ok");
        // `executed` is OpenCode's `providerExecuted`. Marking an imported
        // call as provider-executed makes replay treat its result as a server
        // tool result, which Anthropic Messages only knows for web_search,
        // code_execution and web_fetch — it fails with "does not know how to
        // round-trip server tool result for <name>" on anything else.
        assert_eq!(tool["executed"], false);
    }

    #[test]
    fn tool_names_and_inputs_follow_the_2x_renames() {
        let cases = [
            (
                "bash",
                json!({ "command": "ls", "timeout_ms": 5000, "run_in_background": true, "description": "list" }),
                "shell",
                json!({ "command": "ls", "timeout": 5000, "background": true, "description": "list" }),
            ),
            (
                "read",
                json!({ "filePath": "/a.rs", "offset": 3 }),
                "read",
                json!({ "path": "/a.rs", "offset": 3 }),
            ),
            (
                "edit",
                json!({ "filePath": "/a.rs", "oldString": "a", "newString": "b" }),
                "edit",
                json!({ "path": "/a.rs", "oldString": "a", "newString": "b" }),
            ),
            (
                "grep",
                json!({ "pattern": "fn", "glob": "*.rs", "-i": true, "head_limit": 5 }),
                "grep",
                json!({ "pattern": "fn", "include": "*.rs", "caseSensitive": false, "limit": 5 }),
            ),
            (
                "task",
                json!({ "subagent_type": "explore", "description": "d", "prompt": "p" }),
                "subagent",
                json!({ "agent": "explore", "description": "d", "prompt": "p" }),
            ),
            (
                "Skill",
                json!({ "skill": "pdf" }),
                "skill",
                json!({ "id": "pdf" }),
            ),
            (
                "WebSearch",
                json!({ "query": "rust" }),
                "websearch",
                json!({ "query": "rust" }),
            ),
            (
                "todowrite",
                json!({ "todos": [] }),
                "todowrite",
                json!({ "todos": [] }),
            ),
        ];
        for (legacy, legacy_input, v2_name, v2_input) in cases {
            assert_eq!(
                super::tool_to_v2(legacy, legacy_input.clone()),
                (v2_name.to_string(), v2_input.clone()),
                "{legacy} → 2.x"
            );
            assert_eq!(
                super::tool_from_v2(v2_name, v2_input),
                (legacy.to_string(), legacy_input),
                "{v2_name} → legacy"
            );
        }
    }

    #[test]
    fn tool_parts_read_back_with_the_legacy_names_and_keys() {
        // A session OpenCode 2.x wrote names `shell` and uses `path`; the codec
        // reads it back as the `Bash`/`Edit` tools the rest of txcript knows.
        let record = super::v2_assistant_record(
            "msg_a",
            &json!({
                "type": "assistant",
                "agent": "build",
                "model": { "id": "m", "providerID": "anthropic" },
                "finish": "tool-calls",
                "time": { "created": 5, "completed": 6 },
                "content": [{
                    "type": "tool",
                    "id": "c1",
                    "name": "shell",
                    "time": { "created": 5, "completed": 6 },
                    "state": {
                        "status": "completed",
                        "input": { "command": "ls", "timeout": 5000, "background": true },
                        "content": [{ "type": "text", "text": "ok" }],
                    },
                }],
            }),
            5,
        );
        let native = Transcript::new(
            sample().meta,
            super::Export {
                info: json!({}),
                messages: vec![record],
            },
        );
        let msgs = OpenCode::to_common(&native).unwrap().body;
        assert_eq!(msgs.len(), 2);
        assert!(matches!(
            &msgs[0].content[0],
            Block::ToolUse { id, tool: Tool::Bash { timeout_ms, run_in_background, .. } }
                if id == "c1" && *timeout_ms == Some(5000) && *run_in_background
        ));
    }

    #[test]
    fn freeform_tool_input_is_wrapped_for_the_2x_schema() {
        // 2.x validates tool input as an object; a Codex code-mode call carries
        // a string input, which must not make the whole document invalid.
        let mut transcript = sample();
        transcript.body[1].content = vec![Block::ToolUse {
            id: "c1".into(),
            tool: Tool::Raw {
                tool_name: "exec".into(),
                input: json!("print('hi')"),
            },
        }];
        let native = OpenCode::from_common(&transcript).unwrap();
        let doc = build_v2_document(&native.body, "prj_1");
        assert_eq!(
            doc["messages"][1]["content"][0]["state"]["input"],
            json!({ "input": "print('hi')" })
        );
    }

    #[test]
    fn text_form_is_the_2x_document_and_reads_both_layouts() {
        let native = OpenCode::from_common(&sample()).unwrap();
        let text = OpenCode::to_text(&native).unwrap();
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(doc["messages"][0]["type"], "user");
        assert_eq!(doc["messages"][1]["type"], "assistant");
        assert_eq!(doc["messages"][1]["content"][2]["name"], "shell");

        // The flat document reads back to the same conversation (message
        // boundaries converge the way the 1.x path already splits turns).
        let reread = OpenCode::from_text(&text).unwrap();
        assert_eq!(reread.meta.id, "ses_1");
        assert_eq!(reread.meta.cwd.as_deref(), Some("/repo"));
        let common = OpenCode::to_common(&reread).unwrap();
        assert_eq!(common.body.len(), 4);
        assert!(matches!(
            &common.body[2].content[0],
            Block::ToolUse { tool: Tool::Bash { command, .. }, .. } if command == "ls"
        ));

        // A 1.x export is still read.
        let legacy = serde_json::to_string(&native.body).unwrap();
        let reread = OpenCode::from_text(&legacy).unwrap();
        assert_eq!(reread.body.messages.len(), native.body.messages.len());
    }

    #[test]
    fn unmodeled_records_ride_through_a_text_round_trip() {
        // `idle`, `compaction`, `shell`, … have no 1.x shape; they must survive
        // an export/import round trip unchanged.
        let doc = json!({
            "info": {
                "id": "ses_1", "projectID": "global",
                "location": { "directory": "/repo" },
                "cost": 0.0,
                "tokens": { "input": 0, "output": 0, "reasoning": 0,
                            "cache": { "read": 0, "write": 0 } },
                "time": { "created": 1, "updated": 2 },
            },
            "messages": [
                { "id": "msg_u", "type": "user", "time": { "created": 1 },
                  "text": "hi", "files": [], "agents": [] },
                { "id": "msg_i", "type": "idle", "time": { "created": 2 },
                  "outcome": "succeeded" },
                { "id": "msg_c", "type": "compaction", "status": "completed",
                  "reason": "auto", "summary": "sum", "time": { "created": 3 } },
            ],
        });
        let native = OpenCode::from_text(&doc.to_string()).unwrap();
        assert_eq!(native.body.messages.len(), 3);
        assert_eq!(native.body.messages[1].info["type"], "idle");

        let out = OpenCode::to_text(&native).unwrap();
        let back: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(back["messages"][1], doc["messages"][1]);
        assert_eq!(back["messages"][2], doc["messages"][2]);
    }

    #[test]
    fn round_trip_reuses_the_marker_already_in_the_text() {
        // A v2 session OpenCode itself wrote — or that txcript exported
        // earlier — keeps the `[Image N]` marker in the text. Rebuilding the
        // document must point the mention at that occurrence, not grow a
        // second copy of the marker.
        let mut transcript = sample();
        transcript.body[0].content = vec![
            Block::Text {
                text: "[Image 1]\n\nplease edit".into(),
            },
            Block::Image {
                source: ImageSource {
                    source_type: "base64".into(),
                    media_type: "image/png".into(),
                    data: "AAAA".into(),
                },
            },
        ];
        let native = OpenCode::from_common(&transcript).unwrap();
        let doc = build_v2_document(&native.body, "prj_1");

        let user = &doc["messages"][0];
        assert_eq!(user["text"], "[Image 1]\n\nplease edit");
        assert_eq!(user["files"][0]["mention"]["start"], 0);
        assert_eq!(user["files"][0]["mention"]["end"], 9);
    }

    #[test]
    fn mention_offsets_are_utf16_string_indices() {
        // The composer places attachment extmarks with `String.length`, so the
        // offsets are UTF-16 code units: 🦀 is one char, four UTF-8 bytes and
        // two UTF-16 units. A byte offset would say 10, a char count 7.
        let mut transcript = sample();
        transcript.body[0].content = vec![
            Block::Text {
                text: "\u{1F980}ello".into(),
            },
            Block::Image {
                source: ImageSource {
                    source_type: "base64".into(),
                    media_type: "image/png".into(),
                    data: "AAAA".into(),
                },
            },
        ];
        let native = OpenCode::from_common(&transcript).unwrap();
        let doc = build_v2_document(&native.body, "prj_1");

        let user = &doc["messages"][0];
        assert_eq!(user["text"], "\u{1F980}ello\n\n[Image 1]");
        assert_eq!(user["files"][0]["mention"]["start"], 8);
        assert_eq!(user["files"][0]["mention"]["end"], 17);
    }

    #[test]
    fn normalizes_attachment_payloads_to_canonical_base64() {
        // The import schema accepts nothing but canonical base64; a payload
        // with missing padding is re-encoded the way OpenCode's own migration
        // does instead of failing the whole import.
        let mut transcript = sample();
        transcript.body[0].content = vec![Block::Image {
            source: ImageSource {
                source_type: "base64".into(),
                media_type: "image/png".into(),
                data: "AAA".into(),
            },
        }];
        let native = OpenCode::from_common(&transcript).unwrap();
        let doc = build_v2_document(&native.body, "prj_1");

        assert_eq!(doc["messages"][0]["files"][0]["data"], "AAA=");
    }

    #[test]
    fn parse_file_image_re_encodes_percent_encoded_payloads() {
        // 1.x also stored attachments as data URLs without a `;base64`
        // header — a percent-encoded payload the 2.x import schema would
        // reject and OpenCode's migration re-encodes.
        let part = json!({
            "type": "file",
            "mime": "image/png",
            "url": "data:image/png,%FF%FE%00%01",
        });
        let source = parse_file_image(&part).unwrap();
        assert_eq!(
            source.data,
            base64::engine::general_purpose::STANDARD.encode([0xFFu8, 0xFE, 0x00, 0x01])
        );
    }

    #[test]
    fn base64_canonicality_mirrors_the_import_schema() {
        // The schema's linear check: `length % 4 == 0` and the standard
        // alphabet with at most two trailing `=`.
        for canonical in ["", "AAAA", "AA==", "AAA=", "+/=="] {
            assert!(!super::non_canonical_base64(canonical), "{canonical:?}");
        }
        for non_canonical in ["A", "AA", "AAA", "AA AA", "A===", "AAAA=", "=AAA", "AA-_"] {
            assert!(
                super::non_canonical_base64(non_canonical),
                "{non_canonical:?}"
            );
        }
    }
}
