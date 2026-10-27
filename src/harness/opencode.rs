//! `OpenCode`: a single `SQLite` database (`~/.local/share/opencode/opencode.db`).
//! Up to 2.0 it held `session`, `message`, and `part` rows; since 2.1 it keeps
//! `session_v2` rows and one flat `session_message` record per message.
//!
//! The native [`Body`](OpenCode) is the 2.0 export shape —
//! `{info, messages: [{info, parts}]}` — which the codec understands; the `v2`
//! module projects it to and from 2.1's `{info, messages: [record]}` document,
//! the form `opencode session export` emits and `opencode session import`
//! consumes. [`OpenCodeStore`] reads either database layout read-only and
//! assembles the native shape; `save` hands the document the installed CLI
//! understands to `opencode session import` (2.1) or `opencode import`
//! (earlier). The text form is the 2.1 document; `from_text` reads both. The
//! codec and native types are pure JSON and compile without the `opencode`
//! feature; only the store pulls in `rusqlite`.
//!
//! A `part` of type `tool` splits into a [`Block::ToolUse`] on an assistant
//! message and a [`Block::ToolResult`] on the following user message, matching
//! the Anthropic shape. Synthetic text parts and internal bookkeeping
//! (step-start/finish, snapshot, patch, …) are dropped from the conversation
//! but preserved in the native body.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::common::{Block, ImageSource, Message, Meta, Role, StopReason, Tool, ToolOutput, Usage};
use crate::error::Result;
use crate::transcript::{Codec, Common, Harness, TextCodec, Transcript};

mod v2;

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
    /// The text form is the `opencode session export` JSON document (2.1);
    /// the 2.0 `opencode export` shape is read as well.
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let doc: Value = serde_json::from_str(text)?;
        let export = if v2::is_document(&doc) {
            v2::to_legacy(&doc)?
        } else {
            serde_json::from_value(doc)?
        };
        let meta = meta_from_info(&export.info);
        Ok(Transcript::new(meta, export))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Ok(serde_json::to_string(&v2::from_legacy(&transcript.body))?)
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
        // 2.0 spelled the working directory `directory`; 2.1 nests it in
        // `location`.
        cwd: string("directory").or_else(|| {
            info.get("location")
                .and_then(|location| location.get("directory"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(String::from)
        }),
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
            // `opencode import` requires a slug; derive it from the session
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
            // Fold the matching ToolResult into state.output.
            if let Some(result) = following_result(messages, *idx, id)
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

/// The result of call `id` made in `messages[idx]`: the first matching
/// `ToolResult` in the messages after it, up to the next user message that is
/// more than tool results (the next prompt). Parallel calls can be recorded one
/// per assistant message, with each result in its own later user message, so
/// the result is not always in the very next message.
fn following_result<'m>(
    messages: &'m [Message],
    idx: usize,
    id: &str,
) -> Option<(&'m ToolOutput, bool)> {
    for message in messages.iter().skip(idx + 1) {
        if message.role != Role::User {
            continue;
        }
        let mut prompt = false;
        for block in &message.content {
            match block {
                Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } if tool_use_id == id => return Some((content, *is_error)),
                Block::ToolResult { .. } => {}
                _ => prompt = true,
            }
        }
        if prompt {
            return None;
        }
    }
    None
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
    let (_, data) = part
        .get("url")
        .and_then(Value::as_str)?
        .split_once(',')
        .filter(|(header, _)| header.starts_with("data:"))?;
    Some(ImageSource {
        source_type: "base64".to_string(),
        media_type: mime.to_string(),
        data: data.to_string(),
    })
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
    // `opencode import` requires the full tokens object, cache included;
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
    use serde_json::Value;

    use super::{Export, MessageRecord, OpenCode, v2};
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
    }

    /// Which tables a database keeps its sessions in: 2.1 moved them to
    /// `session_v2` and `session_message`; earlier releases used `session`,
    /// `message` and `part`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Layout {
        Legacy,
        V2,
    }

    impl Layout {
        fn detect(conn: &Connection) -> Result<Self> {
            let v2: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master \
                     WHERE type = 'table' AND name = 'session_v2')",
                    [],
                    |row| row.get(0),
                )
                .map_err(sqlite_err)?;
            Ok(if v2 { Self::V2 } else { Self::Legacy })
        }

        fn session_table(self) -> &'static str {
            match self {
                Self::Legacy => "session",
                Self::V2 => "session_v2",
            }
        }

        fn message_table(self) -> &'static str {
            match self {
                Self::Legacy => "message",
                Self::V2 => "session_message",
            }
        }
    }

    /// Which generation the `opencode` on `PATH` is: 2.1 imports with
    /// `opencode session import` and the flat document; earlier releases
    /// with `opencode import` and the `{info, parts}` export.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Generation {
        Legacy,
        Current,
    }

    impl Generation {
        fn of_cli() -> Result<Self> {
            use std::process::{Command, Stdio};

            let output = Command::new("opencode")
                .arg("--version")
                .stdin(Stdio::null())
                .output()
                .map_err(|e| Error::Unconvertible {
                    harness: "opencode",
                    detail: format!("running `opencode --version`: {e} (is OpenCode on PATH?)"),
                })?;
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(Self::parse(&text))
        }

        /// `opencode v2.1.4`, `2.0.3`, `v1.17.20`: anything before 2.1 is the
        /// legacy generation; a version that cannot be read is taken as
        /// current.
        fn parse(text: &str) -> Self {
            let version = text.split_whitespace().find_map(|word| {
                let mut numbers = word.trim_start_matches('v').split('.');
                let major: u32 = numbers.next()?.parse().ok()?;
                let minor: u32 = numbers.next()?.parse().ok()?;
                Some((major, minor))
            });
            match version {
                Some(version) if version < (2, 1) => Self::Legacy,
                _ => Self::Current,
            }
        }
    }

    impl Store for OpenCodeStore {
        type H = OpenCode;
        type Ref = String;

        fn discover(&self) -> Result<Vec<Discovered<String>>> {
            if self.db_path.is_file() {
                let conn = self.open()?;
                let layout = Layout::detect(&conn)?;
                let mut stmt = conn
                    .prepare(&format!(
                        "SELECT id, directory, title, version, time_created, model, parent_id \
                         FROM {} WHERE time_archived IS NULL ORDER BY time_created DESC",
                        layout.session_table()
                    ))
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
            let layout = Layout::detect(&conn)?;
            let (meta, info) = session_row(&conn, layout, reference)?;
            let messages = match layout {
                Layout::Legacy => legacy_messages(&conn, reference)?,
                Layout::V2 => v2_messages(&conn, reference)?,
            };
            Ok(Transcript::new(meta, Export { info, messages }))
        }

        fn save(&self, transcript: &Transcript<OpenCode>) -> Result<Saved<String>> {
            // OpenCode owns its schema; hand it the export and let its import
            // command normalize and persist. Requires the `opencode` binary,
            // whose generation picks the command and the document layout.
            use std::io::Write;
            use std::process::{Command, Stdio};

            let generation = Generation::of_cli()?;
            let (command, export) = match generation {
                Generation::Current => (
                    "opencode session import",
                    serde_json::to_string(&v2::from_legacy(&transcript.body))?,
                ),
                Generation::Legacy => ("opencode import", serde_json::to_string(&transcript.body)?),
            };
            // A randomized name plus `create_new` keeps the staging file off
            // any predictable, pre-plantable path (and never follows a
            // symlink); 0600 keeps the exported transcript private in a
            // world-shared temp dir.
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
            file.write_all(export.as_bytes())?;
            drop(file);

            let mut import = Command::new("opencode");
            match generation {
                Generation::Current => {
                    import.arg("session").arg("import").arg(&tmp);
                    // 2.1 files the session under the directory it is imported
                    // from; name the transcript's own when it exists here.
                    if let Some(cwd) = transcript
                        .meta
                        .cwd
                        .as_deref()
                        .filter(|dir| Path::new(dir).is_dir())
                    {
                        import.arg("--directory").arg(cwd);
                    }
                }
                Generation::Legacy => {
                    import.arg("import").arg(&tmp);
                }
            }
            let output = import
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .map_err(|e| Error::Unconvertible {
                    harness: "opencode",
                    detail: format!("running `{command}`: {e} (is OpenCode on PATH?)"),
                })?;
            if output.status.success() {
                let _ = std::fs::remove_file(&tmp);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let id = stdout
                    .lines()
                    .find_map(|l| l.strip_prefix("Imported session: ").map(str::trim))
                    .map_or_else(|| transcript.meta.id.clone(), String::from);
                Ok(Saved {
                    reference: id.clone(),
                    id,
                })
            } else {
                // The export file is left behind on purpose, for debugging.
                Err(Error::Unconvertible {
                    harness: "opencode",
                    detail: format!(
                        "`{command}` failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    ),
                })
            }
        }

        /// Archives the session in place — `OpenCode`'s own notion of
        /// deletion. `discover` (here and in `OpenCode`'s UI) filters
        /// archived sessions out; the rows stay, reversible from `OpenCode`.
        fn delete(&self, reference: &String) -> Result<()> {
            let conn = Connection::open(&self.db_path).map_err(sqlite_err)?;
            let layout = Layout::detect(&conn)?;
            let now = chrono::Utc::now().timestamp_millis();
            let updated = conn
                .execute(
                    &format!(
                        "UPDATE {} SET time_archived = ?1 \
                         WHERE id = ?2 AND time_archived IS NULL",
                        layout.session_table()
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
                let layout = Layout::detect(&conn)?;
                let mut stmt = conn
                    .prepare(&format!(
                        "SELECT session_id, MAX(time_created) FROM {} GROUP BY session_id",
                        layout.message_table()
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

    /// The 2.0 layout: `message` rows carry the message `info`, `part` rows
    /// its parts.
    fn legacy_messages(conn: &Connection, reference: &str) -> Result<Vec<MessageRecord>> {
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
                obj.entry("time").or_insert_with(
                    || serde_json::json!({ "created": time_created, "completed": time_created }),
                );
            }
            messages.push(MessageRecord {
                info: msg_info,
                parts: parts_by_message.remove(&message_id).unwrap_or_default(),
            });
        }
        Ok(messages)
    }

    /// The 2.1 layout: one `session_message` row per message, its `data`
    /// column the record minus `id` and `type`, in `seq` order.
    fn v2_messages(conn: &Connection, reference: &str) -> Result<Vec<MessageRecord>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, type, data, time_created FROM session_message \
                 WHERE session_id = ?1 ORDER BY seq, time_created, id",
            )
            .map_err(sqlite_err)?;
        let rows: Vec<(String, String, String, i64)> = stmt
            .query_map([reference], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(sqlite_err)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sqlite_err)?;
        drop(stmt);

        let mut last_user_id = None;
        let mut messages = Vec::with_capacity(rows.len());
        for (i, (id, kind, data, time_created)) in rows.into_iter().enumerate() {
            // A row whose JSON doesn't parse is dropped; the session survives
            // without it.
            let Ok(Value::Object(mut record)) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            record.insert("id".into(), Value::String(id));
            record.insert("type".into(), Value::String(kind));
            record
                .entry("time")
                .or_insert_with(|| serde_json::json!({ "created": time_created }));
            messages.push(v2::record_to_legacy(
                &Value::Object(record),
                reference,
                i,
                &mut last_user_id,
            ));
        }
        Ok(messages)
    }

    fn session_row(conn: &Connection, layout: Layout, id: &str) -> Result<(Meta, Value)> {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT directory, title, version, time_created, model, slug, time_updated, \
                        parent_id \
                 FROM {} WHERE id = ?1",
                layout.session_table()
            ))
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
        use super::{Generation, OpenCodeStore};
        use crate::common::{Block, Relation, Role, Tool, ToolOutput};
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
                "INSERT INTO message VALUES ('m_a','ses_1',1778834704540,?1)",
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

        /// A 2.1 database: `session_v2` plus flat `session_message` records —
        /// the conversation of `make_db` plus the `idle` marker 2.1 writes
        /// when a turn ends.
        fn make_db_v2() -> std::path::PathBuf {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "transcript-oc-v2-test-{n}-{:?}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("opencode.db");
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE session_v2 (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT, \
                   slug TEXT, directory TEXT, title TEXT, version TEXT, model TEXT, \
                   time_created INTEGER, time_updated INTEGER, time_archived INTEGER);\
                 CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, \
                   seq INTEGER, time_created INTEGER, time_updated INTEGER, data TEXT);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO session_v2 VALUES ('ses_1','global',NULL,'clever-engine','/repo','Demo','2.1.4',\
                 '{\"id\":\"claude-opus-4-7\",\"providerID\":\"anthropic\"}',1778834704515,1778834704516,NULL)",
                [],
            )
            .unwrap();
            for (id, kind, seq, created, data) in [
                (
                    "msg_u",
                    "user",
                    1,
                    1_778_834_704_520_i64,
                    r#"{"text":"edit it","time":{"created":1778834704520}}"#,
                ),
                (
                    "msg_a",
                    "assistant",
                    2,
                    1_778_834_704_540,
                    r#"{"agent":"build","model":{"id":"claude-opus-4-7","providerID":"anthropic"},"finish":"stop","cost":0,"tokens":{"input":6,"output":88,"reasoning":0,"cache":{"read":10,"write":21428}},"time":{"created":1778834704540,"completed":1778834704560},"content":[{"type":"reasoning","text":"thinking"},{"type":"text","text":"On it."},{"type":"tool","id":"c1","name":"edit","time":{"created":1778834704545,"completed":1778834704550},"state":{"status":"completed","input":{"path":"/repo/a.rs","oldString":"old","newString":"new"},"content":[{"type":"text","text":"done"}]}},{"type":"text","text":"Finished."}]}"#,
                ),
                (
                    "msg_i",
                    "idle",
                    3,
                    1_778_834_704_570,
                    r#"{"outcome":"succeeded","time":{"created":1778834704570}}"#,
                ),
            ] {
                conn.execute(
                    "INSERT INTO session_message VALUES (?1,'ses_1',?2,?3,?4,?4,?5)",
                    params![id, kind, seq, created, data],
                )
                .unwrap();
            }
            path
        }

        #[test]
        fn discover_and_load_from_a_2_1_database() {
            let path = make_db_v2();
            let store = OpenCodeStore::new(&path);

            let found = store.discover().unwrap();
            assert_eq!(found.len(), 1);
            let d = &found[0];
            assert_eq!(d.reference, "ses_1");
            assert_eq!(d.meta.title.as_deref(), Some("Demo"));
            assert_eq!(d.meta.model.as_deref(), Some("claude-opus-4-7"));
            assert_eq!(d.meta.cwd.as_deref(), Some("/repo"));
            assert_eq!(d.meta.cli_version.as_deref(), Some("2.1.4"));

            let native = store.load(&d.reference).unwrap();
            // The idle marker rides along verbatim; the codec skips it.
            assert_eq!(native.body.messages.len(), 3);
            assert_eq!(native.body.messages[2].info["type"], "idle");
            let msgs = OpenCode::to_common(&native).unwrap().body;
            // user | assistant(reasoning+text) | assistant(tool) | user(result) | assistant(text)
            assert_eq!(msgs.len(), 5);
            assert_eq!(msgs[0].role, Role::User);
            assert!(matches!(
                &msgs[2].content[0],
                Block::ToolUse { id, tool: Tool::Edit { file_path, .. } }
                    if id == "c1" && file_path == "/repo/a.rs"
            ));
            assert!(matches!(
                &msgs[3].content[0],
                Block::ToolResult { tool_use_id, content: ToolOutput::Text(text), is_error: false }
                    if tool_use_id == "c1" && text == "done"
            ));
            let usage = msgs[4].usage.unwrap();
            assert_eq!(usage.input_tokens, 6);
            assert_eq!(usage.cache_creation_input_tokens, Some(21428));

            let prints = store.fingerprints(&["ses_1".to_string()]).unwrap();
            assert_eq!(prints["ses_1"], "1778834704570");
        }

        #[test]
        fn cli_generation_is_read_from_the_version_line() {
            assert_eq!(Generation::parse("opencode v2.1.4\n"), Generation::Current);
            assert_eq!(Generation::parse("2.0.3"), Generation::Legacy);
            assert_eq!(Generation::parse("v1.17.20"), Generation::Legacy);
            assert_eq!(
                Generation::parse("opencode 2.1.0-beta.1"),
                Generation::Current
            );
            assert_eq!(Generation::parse("3.0.0"), Generation::Current);
            assert_eq!(Generation::parse(""), Generation::Current);
        }
    }
}
