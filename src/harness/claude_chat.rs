//! Claude Chat (`claude.ai`): live, pull-only conversations fetched from
//! Claude's private web API.
//!
//! [`ClaudeChatStore`] is deliberately source-only. It discovers and loads
//! conversations with GET requests, but [`Store::save`] and [`Store::delete`]
//! always refuse. [`Codec::from_common`] refuses for the same reason: txcript
//! must never manufacture or push a Claude web conversation.
//!
//! The native [`Body`](Conversation) is one detail response. `chat_messages`
//! stays as raw JSON values and all other server fields pass through `extra`,
//! so schema additions survive the text boundary. The live store may add a
//! `$txcript_images` and `$txcript_files` maps containing base64 copies of
//! same-origin images and presented artifacts; the original server fields
//! remain untouched.
//!
//! Claude Chat messages form a parent-linked tree. Conversion follows the
//! active `current_leaf_message_uuid` path; a missing, cyclic, or broken graph
//! falls back to server order. Side branches remain in the native body but are
//! not representable in Common. Unknown content blocks likewise remain native.
//!
//! Known losses through Common: branches outside the active path, citations,
//! feedback/UI state, attachments that Claude no longer serves, and unknown
//! block kinds. Unknown tools retain unmodeled inputs through [`Tool::Raw`];
//! hydrated generated files use first-class [`Block::Artifact`] values.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::common::{
    Artifact, ArtifactSource, Block, ImageSource, Message, Meta, Role, StopReason, Tool,
    ToolOutput, Usage,
};
use crate::error::{Error, Result};
use crate::transcript::{Codec, Common, Harness, TextCodec, Transcript};

const SYNTHETIC_ID_NAMESPACE: Uuid = Uuid::from_u128(0x42d0_364d_f40c_50af_a0fd_1bd2_d713_20bd);

/// The Claude Chat harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaudeChat;

impl Harness for ClaudeChat {
    const NAME: &'static str = "claude_chat";
    type Body = Conversation;
}

/// One live conversation detail response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    #[serde(default)]
    pub chat_messages: Vec<Value>,
    /// Images hydrated by the live store, keyed by the file UUID or URL found
    /// in a message. This field is txcript-owned; server fields remain in
    /// `extra` unchanged.
    #[serde(
        default,
        skip_serializing_if = "Map::is_empty",
        rename = "$txcript_images"
    )]
    pub hydrated_images: Map<String, Value>,
    /// Presented artifacts hydrated by the live store, keyed by Claude's file
    /// UUID. Values contain `name`, `media_type`, and base64 `data` fields.
    #[serde(
        default,
        skip_serializing_if = "Map::is_empty",
        rename = "$txcript_files"
    )]
    pub hydrated_files: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Conversation {
    /// The conversation UUID recorded by Claude.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.extra.get("uuid").and_then(Value::as_str)
    }
}

impl Codec for ClaudeChat {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        let mut messages = Vec::new();
        let mut last_timestamp = transcript.meta.timestamp;
        let model = conversation_model(&transcript.body);

        for native in active_path(&transcript.body) {
            push_native_message(
                &transcript.body,
                native,
                model.as_deref(),
                &mut last_timestamp,
                &mut messages,
            );
        }

        Ok(Transcript::new(transcript.meta.clone(), messages))
    }

    fn from_common(_: &Transcript<Common>) -> Result<Transcript<Self>> {
        Err(read_only_error())
    }
}

impl TextCodec for ClaudeChat {
    /// Parse one live conversation detail response. Claude's account export
    /// array (`conversations.json`) is intentionally not accepted.
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let value: Value = serde_json::from_str(text)?;
        let object = value.as_object().ok_or_else(|| Error::Malformed {
            harness: ClaudeChat::NAME,
            detail:
                "expected one live conversation object; Claude data-export arrays are not supported"
                    .to_string(),
        })?;
        if !object.get("chat_messages").is_some_and(Value::is_array)
            || !object.get("uuid").is_some_and(Value::is_string)
        {
            return Err(Error::Malformed {
                harness: ClaudeChat::NAME,
                detail: "live response is missing string `uuid` or array `chat_messages`"
                    .to_string(),
            });
        }
        let body: Conversation = serde_json::from_value(value)?;
        let meta = meta_from_conversation(&body);
        Ok(Transcript::new(meta, body))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Ok(serde_json::to_string_pretty(&transcript.body)?)
    }
}

fn meta_from_conversation(conversation: &Conversation) -> Meta {
    let string = |key: &str| {
        conversation
            .extra
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(String::from)
    };
    let timestamp = string("created_at")
        .and_then(|s| parse_timestamp(&s))
        .or_else(|| string("updated_at").and_then(|s| parse_timestamp(&s)))
        .unwrap_or_else(Utc::now);
    Meta {
        id: string("uuid").unwrap_or_default(),
        timestamp,
        cwd: None,
        git_branch: None,
        title: string("name").or_else(|| string("summary")),
        cli_version: None,
        model: conversation_model(conversation),
    }
}

fn conversation_model(conversation: &Conversation) -> Option<String> {
    conversation.extra.get("model").and_then(|model| {
        model
            .as_str()
            .map(String::from)
            .or_else(|| model.get("id").and_then(Value::as_str).map(String::from))
    })
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    value.parse().ok()
}

/// Follow the active leaf when the graph is sound; otherwise keep server
/// order rather than dropping an arbitrary prefix or looping forever.
fn active_path(conversation: &Conversation) -> Vec<&Value> {
    let Some(leaf) = conversation
        .extra
        .get("current_leaf_message_uuid")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    else {
        return conversation.chat_messages.iter().collect();
    };

    let by_id: HashMap<&str, usize> = conversation
        .chat_messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            message
                .get("uuid")
                .and_then(Value::as_str)
                .map(|id| (id, index))
        })
        .collect();
    let mut seen = HashSet::new();
    let mut indices = Vec::new();
    let mut current = leaf;

    loop {
        if !seen.insert(current) {
            return conversation.chat_messages.iter().collect();
        }
        let Some(&index) = by_id.get(current) else {
            return conversation.chat_messages.iter().collect();
        };
        indices.push(index);
        let parent = conversation.chat_messages[index]
            .get("parent_message_uuid")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let Some(parent) = parent else {
            break;
        };
        if !by_id.contains_key(parent) {
            return conversation.chat_messages.iter().collect();
        }
        current = parent;
    }
    indices.reverse();
    indices
        .into_iter()
        .map(|index| &conversation.chat_messages[index])
        .collect()
}

fn push_native_message(
    conversation: &Conversation,
    native: &Value,
    conversation_model: Option<&str>,
    last_timestamp: &mut DateTime<Utc>,
    out: &mut Vec<Message>,
) {
    let Some(default_role) = native
        .get("sender")
        .and_then(Value::as_str)
        .and_then(|sender| match sender {
            "human" | "user" => Some(Role::User),
            "assistant" => Some(Role::Assistant),
            _ => None,
        })
    else {
        return;
    };
    if let Some(timestamp) = native
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
    {
        *last_timestamp = timestamp;
    }
    let timestamp = *last_timestamp;
    let start = out.len();
    let mut pending_role = default_role;
    let mut pending = Vec::new();
    let mut last_tool_id: Option<String> = None;
    let mut structured_text = Vec::new();

    if let Some(content) = native.get("content").and_then(Value::as_array) {
        for (block_index, value) in content.iter().enumerate() {
            if value.get("type").and_then(Value::as_str) == Some("text")
                && let Some(text) = value.get("text").and_then(Value::as_str)
            {
                structured_text.push(text.trim());
            }
            if let Some((role, block)) = parse_block(
                conversation,
                native,
                value,
                block_index,
                default_role,
                last_tool_id.as_deref(),
            ) {
                if let Block::ToolUse { id, .. } = &block {
                    last_tool_id = Some(id.clone());
                }
                append_role_block(role, block, timestamp, out, &mut pending_role, &mut pending);
            }
            if value.get("type").and_then(Value::as_str) == Some("tool_result") {
                append_tool_result_artifacts(
                    conversation,
                    value,
                    timestamp,
                    out,
                    &mut pending_role,
                    &mut pending,
                );
            }
        }
    }

    if let Some(text) = native
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        && !structured_text.iter().any(|known| *known == text.trim())
    {
        append_role_block(
            default_role,
            Block::Text {
                text: text.to_string(),
            },
            timestamp,
            out,
            &mut pending_role,
            &mut pending,
        );
    }

    for source in message_images(conversation, native) {
        append_role_block(
            default_role,
            Block::Image { source },
            timestamp,
            out,
            &mut pending_role,
            &mut pending,
        );
    }
    flush_pending(pending_role, &mut pending, timestamp, out);
    attribute_assistant(out, start, native, conversation_model);
}

fn attribute_assistant(
    out: &mut [Message],
    start: usize,
    native: &Value,
    conversation_model: Option<&str>,
) {
    let model = native
        .get("model")
        .and_then(|value| {
            value
                .as_str()
                .or_else(|| value.get("id").and_then(Value::as_str))
        })
        .or(conversation_model)
        .map(String::from);
    let usage = native.get("usage").and_then(parse_usage);
    let stop_reason = native
        .get("stop_reason")
        .or_else(|| native.get("stopReason"))
        .and_then(Value::as_str)
        .map(parse_stop_reason);
    if let Some(message) = out[start..]
        .iter_mut()
        .rev()
        .find(|message| message.role == Role::Assistant)
    {
        message.model = model;
        message.usage = usage;
        message.stop_reason = stop_reason;
    }
}

fn append_tool_result_artifacts(
    conversation: &Conversation,
    block: &Value,
    timestamp: DateTime<Utc>,
    out: &mut Vec<Message>,
    pending_role: &mut Role,
    pending: &mut Vec<Block>,
) {
    for artifact in artifacts_from_tool_result(conversation, block) {
        append_role_block(
            Role::Assistant,
            Block::Artifact { artifact },
            timestamp,
            out,
            pending_role,
            pending,
        );
    }
}

fn append_role_block(
    role: Role,
    block: Block,
    timestamp: DateTime<Utc>,
    out: &mut Vec<Message>,
    pending_role: &mut Role,
    pending: &mut Vec<Block>,
) {
    if *pending_role != role {
        flush_pending(*pending_role, pending, timestamp, out);
        *pending_role = role;
    }
    pending.push(block);
}

fn flush_pending(
    role: Role,
    pending: &mut Vec<Block>,
    timestamp: DateTime<Utc>,
    out: &mut Vec<Message>,
) {
    if pending.is_empty() {
        return;
    }
    out.push(Message {
        role,
        content: std::mem::take(pending),
        timestamp,
        model: None,
        stop_reason: None,
        usage: None,
    });
}

fn parse_block(
    conversation: &Conversation,
    message: &Value,
    block: &Value,
    block_index: usize,
    default_role: Role,
    preceding_tool_id: Option<&str>,
) -> Option<(Role, Block)> {
    match block.get("type").and_then(Value::as_str)? {
        "text" => nonempty(block.get("text")?).map(|text| {
            (
                default_role,
                Block::Text {
                    text: text.to_string(),
                },
            )
        }),
        "thinking" | "reasoning" => block
            .get("thinking")
            .or_else(|| block.get("text"))
            .and_then(nonempty)
            .map(|text| {
                (
                    Role::Assistant,
                    Block::Thinking {
                        text: text.to_string(),
                        signature: block
                            .get("signature")
                            .and_then(Value::as_str)
                            .map(String::from),
                        encrypted: block
                            .get("encrypted_content")
                            .and_then(Value::as_str)
                            .map(String::from),
                    },
                )
            }),
        "tool_use" => tool_use_block(conversation, message, block, block_index),
        "tool_result" => Some((
            Role::User,
            tool_result_block(conversation, message, block, block_index, preceding_tool_id),
        )),
        "image" => inline_image(block).map(|source| (default_role, Block::Image { source })),
        "artifact" | "document" => inline_artifact(conversation, message, block, block_index)
            .map(|artifact| (Role::Assistant, Block::Artifact { artifact })),
        _ => None,
    }
}

fn nonempty(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.trim().is_empty())
}

fn tool_use_block(
    conversation: &Conversation,
    message: &Value,
    block: &Value,
    block_index: usize,
) -> Option<(Role, Block)> {
    let name = block.get("name").and_then(Value::as_str)?;
    let id = block
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(
            || synthetic_block_id(conversation, message, block_index, "tool-use"),
            String::from,
        );
    let input = block
        .get("input")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let (name, input) = normalize_tool(name, input);
    Some((
        Role::Assistant,
        Block::ToolUse {
            id,
            tool: Tool::from_canonical(&name, input),
        },
    ))
}

fn tool_result_block(
    conversation: &Conversation,
    message: &Value,
    block: &Value,
    block_index: usize,
    preceding_tool_id: Option<&str>,
) -> Block {
    let tool_use_id = block
        .get("tool_use_id")
        .or_else(|| block.get("toolUseID"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .or(preceding_tool_id)
        .map_or_else(
            || synthetic_block_id(conversation, message, block_index, "tool-result"),
            String::from,
        );
    let value = block
        .get("content")
        .or_else(|| block.get("output"))
        .or_else(|| block.get("result"))
        .cloned()
        .unwrap_or(Value::Null);
    let content = value
        .as_str()
        .map(|text| ToolOutput::Text(text.to_string()))
        .unwrap_or(ToolOutput::Json(value));
    let is_error = block
        .get("is_error")
        .or_else(|| block.get("isError"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || block.get("error").is_some_and(|error| !error.is_null())
        || matches!(
            block.get("status").and_then(Value::as_str),
            Some("error" | "failed")
        );
    Block::ToolResult {
        tool_use_id,
        content,
        is_error,
    }
}

fn artifacts_from_tool_result(conversation: &Conversation, block: &Value) -> Vec<Artifact> {
    let value = block
        .get("content")
        .or_else(|| block.get("output"))
        .or_else(|| block.get("result"));
    let parsed;
    let value = match value {
        Some(Value::String(text)) => {
            parsed = serde_json::from_str::<Value>(text).ok();
            parsed.as_ref()
        }
        other => other,
    };
    let mut artifacts = Vec::new();
    let mut seen = HashSet::new();
    if let Some(value) = value {
        collect_hydrated_artifacts(conversation, value, &mut seen, &mut artifacts, 0);
    }
    artifacts
}

fn collect_hydrated_artifacts(
    conversation: &Conversation,
    value: &Value,
    seen: &mut HashSet<String>,
    artifacts: &mut Vec<Artifact>,
    depth: usize,
) {
    if depth > 24 {
        return;
    }
    match value {
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("local_resource")
                && let Some(id) = object.get("uuid").and_then(Value::as_str)
                && seen.insert(id.to_string())
                && let Some(file) = conversation.hydrated_files.get(id)
                && let Some(data) = file.get("data").and_then(Value::as_str)
            {
                let name = file
                    .get("file_name")
                    .or_else(|| object.get("name"))
                    .or_else(|| file.get("name"))
                    .and_then(Value::as_str)
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or("artifact")
                    .to_string();
                let media_type = file
                    .get("media_type")
                    .or_else(|| object.get("mime_type"))
                    .or_else(|| object.get("media_type"))
                    .and_then(Value::as_str)
                    .map(String::from);
                artifacts.push(Artifact {
                    id: id.to_string(),
                    name,
                    source: ArtifactSource::Base64 {
                        data: data.to_string(),
                        media_type,
                    },
                });
            }
            for nested in object.values() {
                collect_hydrated_artifacts(conversation, nested, seen, artifacts, depth + 1);
            }
        }
        Value::Array(values) => {
            for nested in values {
                collect_hydrated_artifacts(conversation, nested, seen, artifacts, depth + 1);
            }
        }
        _ => {}
    }
}

fn inline_artifact(
    conversation: &Conversation,
    message: &Value,
    block: &Value,
    block_index: usize,
) -> Option<Artifact> {
    let id = block
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(
            || synthetic_block_id(conversation, message, block_index, "artifact"),
            String::from,
        );
    let name = block
        .get("title")
        .or_else(|| block.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("artifact")
        .to_string();
    let source = block.get("source");
    let media_type = source
        .and_then(|source| source.get("media_type").or_else(|| source.get("mediaType")))
        .or_else(|| block.get("media_type"))
        .or_else(|| block.get("mime_type"))
        .and_then(Value::as_str)
        .map(String::from);
    let artifact_source = if let Some(data) = source
        .and_then(|source| source.get("data"))
        .and_then(Value::as_str)
    {
        match source
            .and_then(|source| source.get("type"))
            .and_then(Value::as_str)
        {
            Some("text") => ArtifactSource::Text {
                text: data.to_string(),
                media_type,
            },
            _ => ArtifactSource::Base64 {
                data: data.to_string(),
                media_type,
            },
        }
    } else {
        let text = block
            .get("content")
            .or_else(|| block.get("text"))
            .and_then(Value::as_str)?;
        ArtifactSource::Text {
            text: text.to_string(),
            media_type: media_type.or_else(|| Some("text/plain".to_string())),
        }
    };
    Some(Artifact {
        id,
        name,
        source: artifact_source,
    })
}

fn inline_image(block: &Value) -> Option<ImageSource> {
    let source = block.get("source").unwrap_or(block);
    let data = source.get("data")?.as_str()?.to_string();
    let media_type = source
        .get("media_type")
        .or_else(|| source.get("mediaType"))?
        .as_str()?
        .to_string();
    Some(ImageSource {
        source_type: source
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("base64")
            .to_string(),
        media_type,
        data,
    })
}

fn message_images(conversation: &Conversation, message: &Value) -> Vec<ImageSource> {
    let mut seen = HashSet::new();
    let mut images = Vec::new();
    for field in ["files", "files_v2", "attachments"] {
        let Some(files) = message.get(field).and_then(Value::as_array) else {
            continue;
        };
        for file in files {
            let keys = [
                file.get("uuid").and_then(Value::as_str),
                file.get("id").and_then(Value::as_str),
                file.get("preview_url").and_then(Value::as_str),
                file.get("thumbnail_url").and_then(Value::as_str),
            ];
            if let Some((key, source)) = keys.into_iter().flatten().find_map(|key| {
                conversation
                    .hydrated_images
                    .get(key)
                    .and_then(|value| serde_json::from_value(value.clone()).ok())
                    .map(|source| (key, source))
            }) && seen.insert(key.to_string())
            {
                images.push(source);
            }
        }
    }
    images
}

fn synthetic_block_id(
    conversation: &Conversation,
    message: &Value,
    block_index: usize,
    kind: &str,
) -> String {
    let session = conversation.id().unwrap_or("unknown-conversation");
    let message = message
        .get("uuid")
        .and_then(Value::as_str)
        .unwrap_or("unknown-message");
    Uuid::new_v5(
        &SYNTHETIC_ID_NAMESPACE,
        format!("{session}:{message}:{block_index}:{kind}").as_bytes(),
    )
    .to_string()
}

fn normalize_tool(name: &str, mut input: Value) -> (String, Value) {
    let canonical = match name {
        "bash" | "bash_tool" | "shell" | "shell_command" | "run_terminal_cmd" => "Bash",
        "read_file" | "view" => "Read",
        "create_file" | "write_file" => "Write",
        "edit_file" | "str_replace" => "Edit",
        other => other,
    };
    if let Some(object) = input.as_object_mut() {
        match canonical {
            "Bash" => {
                rename_key(object, "cmd", "command");
                rename_key(object, "cwd", "workdir");
            }
            "Read" | "Write" | "Edit" => rename_key(object, "path", "file_path"),
            _ => {}
        }
        // Claude Chat's live tools include UI-only descriptions that are not
        // accepted by Claude Code's typed Read/Write tools. The complete
        // source block remains in the native body.
        if matches!(canonical, "Read" | "Write") {
            object.remove("description");
        }
        if canonical == "Read"
            && let Some(range) = object.remove("view_range").and_then(|value| {
                let values = value.as_array()?;
                let start = values.first()?.as_u64()?;
                let end = values.get(1)?.as_u64()?;
                Some((start, end))
            })
        {
            object.entry("offset").or_insert(Value::from(range.0));
            object.entry("limit").or_insert(Value::from(
                range.1.saturating_sub(range.0).saturating_add(1),
            ));
        }
        if canonical == "Write" {
            rename_key(object, "file_text", "content");
        }
        if canonical == "Edit" {
            rename_key(object, "old_str", "old_string");
            rename_key(object, "oldText", "old_string");
            rename_key(object, "new_str", "new_string");
            rename_key(object, "newText", "new_string");
        }
    }
    (canonical.to_string(), input)
}

fn rename_key(object: &mut Map<String, Value>, from: &str, to: &str) {
    if !object.contains_key(to)
        && let Some(value) = object.remove(from)
    {
        object.insert(to.to_string(), value);
    }
}

fn parse_usage(value: &Value) -> Option<Usage> {
    let input_tokens = u64_field(value, &["input_tokens", "inputTokens"])?;
    let output_tokens = u64_field(value, &["output_tokens", "outputTokens"])?;
    Some(Usage {
        input_tokens,
        output_tokens,
        cache_read_input_tokens: u64_field(
            value,
            &["cache_read_input_tokens", "cacheReadInputTokens"],
        ),
        cache_creation_input_tokens: u64_field(
            value,
            &["cache_creation_input_tokens", "cacheCreationInputTokens"],
        ),
    })
}

fn u64_field(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(key).and_then(Value::as_u64))
}

fn parse_stop_reason(reason: &str) -> StopReason {
    StopReason::parse(reason)
}

fn read_only_error() -> Error {
    Error::Unconvertible {
        harness: ClaudeChat::NAME,
        detail: "Claude Chat is a live read-only source; conversations can be pulled out and converted into another harness, but never written, deleted, or continued in Claude"
            .to_string(),
    }
}

// ── live remote store ──────────────────────────────────────────────────

#[cfg(feature = "claude_chat")]
mod remote {
    use std::collections::HashMap;
    use std::sync::{OnceLock, mpsc};
    use std::thread;
    use std::time::Duration;

    use base64::Engine;
    use chrono::{DateTime, Utc};
    use futures_util::StreamExt;
    use serde_json::Value;
    use uuid::Uuid;

    use super::{ClaudeChat, Conversation, meta_from_conversation, read_only_error};
    use crate::error::{Error, Result};
    use crate::transcript::{Discovered, Harness, Saved, Store, Transcript};

    const CLAUDE_BASE_URL: &str = "https://claude.ai";
    const PAGE_SIZE: usize = 100;
    const MAX_APP_SHELL_BYTES: u64 = 8 * 1024 * 1024;
    const MAX_RESPONSE_BYTES: u64 = 128 * 1024 * 1024;
    const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
    const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
    const DISABLED_CREDENTIAL_ENV_VARS: [&str; 3] = [
        "TXCRIPT_CLAUDE_CHAT_SESSION_KEY",
        "TXCRIPT_CLAUDE_CHAT_CF_BM",
        "TXCRIPT_CLAUDE_CHAT_CF_CLEARANCE",
    ];

    /// A stable reference to one remote conversation.
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub struct ClaudeChatRef {
        pub organization_uuid: String,
        pub conversation_uuid: String,
        pub updated_at: Option<DateTime<Utc>>,
    }

    impl ClaudeChatRef {
        #[must_use]
        pub fn key(&self) -> String {
            format!("{}:{}", self.organization_uuid, self.conversation_uuid)
        }
    }

    #[derive(Clone)]
    struct Credentials {
        session_key: String,
        session_key_v3: Option<String>,
        session_key_lc: Option<String>,
        session_key_v3_lc: Option<String>,
        cf_bm: Option<String>,
        cf_clearance: Option<String>,
        routing_hint: Option<String>,
        last_active_org: Option<String>,
        anonymous_id: Option<String>,
        device_id: Option<String>,
        activity_session_id: Option<String>,
        client_platform: &'static str,
    }

    /// Read-only client for Claude's live web conversation store.
    pub struct ClaudeChatStore {
        credentials: Credentials,
        active_organization_uuid: Option<String>,
        organization_uuid: Option<String>,
        agent: BrowserTransport,
        client_metadata: OnceLock<Option<ClientMetadata>>,
        base_url: String,
    }

    #[derive(Clone)]
    struct ClientMetadata {
        version: String,
        git_hash: String,
        build_timestamp: String,
    }

    struct BrowserTransport {
        sender: mpsc::Sender<BrowserRequest>,
    }

    struct BrowserRequest {
        url: String,
        cookie: String,
        accept: &'static str,
        headers: Vec<(&'static str, String)>,
        max_bytes: u64,
        reply: mpsc::SyncSender<std::result::Result<BrowserResponse, String>>,
    }

    struct BrowserResponse {
        status: u16,
        content_type: Option<String>,
        cf_mitigated: bool,
        body: Vec<u8>,
    }

    impl BrowserTransport {
        fn start() -> Result<Self> {
            let (sender, receiver) = mpsc::channel::<BrowserRequest>();
            let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
            thread::Builder::new()
                .name("txcript-claude-chat-http".to_string())
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| format!("could not start HTTP runtime: {error}"));
                    let client = match &runtime {
                        Ok(_) => wreq::Client::builder()
                            // Claude Desktop currently embeds Chromium 148. Matching the
                            // browser's TLS, HTTP/2, and header profile is required by the
                            // edge in front of Claude's private read API.
                            .emulation(wreq_util::Profile::Chrome148)
                            // Never let Claude Desktop's credential-bearing
                            // cookie follow a response to another origin.
                            .redirect(wreq::redirect::Policy::none())
                            .timeout(Duration::from_secs(30))
                            .build()
                            .map_err(|error| {
                                format!("could not build browser HTTP client: {error}")
                            }),
                        Err(error) => Err(error.clone()),
                    };
                    let startup = match (&runtime, &client) {
                        (Ok(_), Ok(_)) => Ok(()),
                        (Err(error), _) | (_, Err(error)) => Err(error.clone()),
                    };
                    if ready_sender.send(startup).is_err() {
                        return;
                    }
                    let (Ok(runtime), Ok(client)) = (runtime, client) else {
                        return;
                    };
                    while let Ok(request) = receiver.recv() {
                        let result = runtime.block_on(execute_get(&client, &request));
                        let _ = request.reply.send(result);
                    }
                })
                .map_err(|error| Error::Remote {
                    harness: ClaudeChat::NAME,
                    detail: format!("could not start browser HTTP worker: {error}"),
                })?;
            ready_receiver
                .recv()
                .map_err(|_| Error::Remote {
                    harness: ClaudeChat::NAME,
                    detail: "browser HTTP worker stopped during startup".to_string(),
                })?
                .map_err(|detail| Error::Remote {
                    harness: ClaudeChat::NAME,
                    detail,
                })?;
            Ok(Self { sender })
        }

        fn get(
            &self,
            url: String,
            cookie: String,
            accept: &'static str,
            headers: Vec<(&'static str, String)>,
            max_bytes: u64,
        ) -> Result<BrowserResponse> {
            let (reply, response) = mpsc::sync_channel(1);
            self.sender
                .send(BrowserRequest {
                    url,
                    cookie,
                    accept,
                    headers,
                    max_bytes,
                    reply,
                })
                .map_err(|_| Error::Remote {
                    harness: ClaudeChat::NAME,
                    detail: "browser HTTP worker stopped before the request".to_string(),
                })?;
            response
                .recv()
                .map_err(|_| Error::Remote {
                    harness: ClaudeChat::NAME,
                    detail: "browser HTTP worker stopped during the request".to_string(),
                })?
                .map_err(|detail| Error::Remote {
                    harness: ClaudeChat::NAME,
                    detail,
                })
        }
    }

    async fn execute_get(
        client: &wreq::Client,
        request: &BrowserRequest,
    ) -> std::result::Result<BrowserResponse, String> {
        let mut cookie = wreq::header::HeaderValue::from_str(&request.cookie)
            .map_err(|_| "could not construct a safe Claude cookie header".to_string())?;
        cookie.set_sensitive(true);
        let mut builder = client
            .get(&request.url)
            .header(wreq::header::COOKIE, cookie)
            .header(wreq::header::ACCEPT, request.accept)
            .header("referer", "https://claude.ai/new")
            .header("sec-fetch-dest", "")
            .header("sec-fetch-mode", "cors")
            .header("sec-fetch-site", "same-origin");
        for (name, value) in &request.headers {
            let mut value = wreq::header::HeaderValue::from_str(value)
                .map_err(|_| format!("could not construct safe `{name}` header"))?;
            value.set_sensitive(true);
            builder = builder.header(*name, value);
        }
        let response = builder
            .send()
            .await
            .map_err(|error| format!("request failed: {error}"))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(wreq::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(String::from);
        let cf_mitigated = response
            .headers()
            .get("cf-mitigated")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("challenge"));
        if response
            .content_length()
            .is_some_and(|length| length > request.max_bytes)
        {
            return Err(format!(
                "Claude response exceeded the {} byte limit",
                request.max_bytes
            ));
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|error| format!("failed reading Claude response: {error}"))?;
            let length = u64::try_from(body.len())
                .unwrap_or(u64::MAX)
                .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            if length > request.max_bytes {
                return Err(format!(
                    "Claude response exceeded the {} byte limit",
                    request.max_bytes
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(BrowserResponse {
            status,
            content_type,
            cf_mitigated,
            body,
        })
    }

    impl ClaudeChatStore {
        /// Enumerate the selected account's Claude Chat conversations.
        ///
        /// Direct calls intentionally produce a compile-time warning because
        /// this uses an undocumented private endpoint that Anthropic can
        /// observe or restrict. Generic store consumers may call
        /// [`Store::discover`] explicitly after making their own consent
        /// boundary visible to users.
        ///
        /// ```compile_fail
        /// #![deny(deprecated)]
        /// use txcript::harness::claude_chat::ClaudeChatStore;
        /// fn discover(store: &ClaudeChatStore) {
        ///     let _ = store.discover();
        /// }
        /// ```
        ///
        /// # Errors
        /// When authentication fails, Claude rejects the request, or its
        /// private response format changes.
        #[deprecated(
            note = "Claude Chat discovery enumerates the selected account's conversation list through an undocumented private claude.ai endpoint that Anthropic can observe or restrict"
        )]
        pub fn discover(&self) -> Result<Vec<Discovered<ClaudeChatRef>>> {
            <Self as Store>::discover(self)
        }

        /// Resolve the signed-in Claude Desktop store. Supplying credential
        /// material through environment variables is refused.
        ///
        /// # Errors
        /// Returns an error for disabled credential variables or when Desktop
        /// credentials cannot be read safely.
        pub fn from_desktop() -> Result<Self> {
            let disabled = disabled_credential_variables(|name| std::env::var_os(name).is_some());
            if !disabled.is_empty() {
                return Err(Error::Remote {
                    harness: ClaudeChat::NAME,
                    detail: format!(
                        "environment-supplied Claude credentials are disabled in V1; unset {}; Claude Chat uses the signed-in Claude Desktop session automatically",
                        disabled.join(", ")
                    ),
                });
            }
            let organization_uuid = nonempty_env("TXCRIPT_CLAUDE_CHAT_ORGANIZATION_UUID");
            let credentials = desktop_credentials()?;
            Self::build(credentials, organization_uuid, CLAUDE_BASE_URL.to_string())
        }

        /// Build a complete reference for one conversation without enumerating
        /// the account's conversation list. An explicit organization wins;
        /// otherwise the store's configured or Claude Desktop-active
        /// organization is used.
        ///
        /// # Errors
        /// Returns an error when either UUID is invalid or no organization is
        /// available from the caller, configuration, or Claude Desktop.
        pub fn conversation_ref(
            &self,
            conversation_uuid: String,
            organization_uuid: Option<String>,
        ) -> Result<ClaudeChatRef> {
            validate_uuid("conversation", &conversation_uuid)?;
            let organization_uuid = organization_uuid
                .or_else(|| self.organization_uuid.clone())
                .or_else(|| self.active_organization_uuid.clone())
                .ok_or_else(|| Error::Remote {
                    harness: ClaudeChat::NAME,
                    detail: "Claude Desktop has no current organization; open Claude Desktop and select the account or organization containing this chat".to_string(),
                })?;
            validate_uuid("organization", &organization_uuid)?;
            Ok(ClaudeChatRef {
                organization_uuid,
                conversation_uuid,
                updated_at: None,
            })
        }

        fn build(
            credentials: Credentials,
            organization_uuid: Option<String>,
            base_url: String,
        ) -> Result<Self> {
            validate_cookie("sessionKey", &credentials.session_key)?;
            for (name, value) in [
                ("sessionKeyV3", credentials.session_key_v3.as_deref()),
                ("sessionKeyLC", credentials.session_key_lc.as_deref()),
                ("sessionKeyV3LC", credentials.session_key_v3_lc.as_deref()),
                ("__cf_bm", credentials.cf_bm.as_deref()),
                ("cf_clearance", credentials.cf_clearance.as_deref()),
                ("routingHint", credentials.routing_hint.as_deref()),
                ("lastActiveOrg", credentials.last_active_org.as_deref()),
                ("ajs_anonymous_id", credentials.anonymous_id.as_deref()),
                ("anthropic-device-id", credentials.device_id.as_deref()),
                (
                    "activitySessionId",
                    credentials.activity_session_id.as_deref(),
                ),
            ] {
                if let Some(value) = value {
                    validate_cookie(name, value)?;
                }
            }
            if let Some(id) = organization_uuid.as_deref() {
                validate_uuid("organization", id)?;
            }
            // Desktop keeps its current organization UUID in this cookie. It
            // is a safe fallback when account-wide bootstrap discovery is not
            // available, but remains distinct from an explicit restriction.
            let active_organization_uuid = credentials
                .last_active_org
                .as_ref()
                .filter(|id| Uuid::parse_str(id).is_ok())
                .cloned();
            Ok(Self {
                credentials,
                active_organization_uuid,
                organization_uuid,
                agent: BrowserTransport::start()?,
                client_metadata: OnceLock::new(),
                base_url,
            })
        }

        fn discover_organizations(&self) -> Result<Vec<String>> {
            let value =
                self.get_json_with_cookie("/api/organizations", self.session_cookie_header())?;
            let rows = value
                .as_array()
                .or_else(|| value.get("data").and_then(Value::as_array))
                .or_else(|| value.get("organizations").and_then(Value::as_array))
                .ok_or_else(|| protocol_error("organizations response is not an array"))?;
            let mut organizations = Vec::new();
            for row in rows {
                let id = row
                    .get("uuid")
                    .and_then(Value::as_str)
                    .ok_or_else(|| protocol_error("organization is missing string `uuid`"))?;
                validate_uuid("organization", id)?;
                organizations.push(id.to_string());
            }
            Ok(organizations)
        }

        fn organizations(&self) -> Result<Vec<String>> {
            if let Some(id) = &self.organization_uuid {
                return Ok(vec![id.clone()]);
            }
            if let Some(id) = &self.active_organization_uuid {
                return Ok(vec![id.clone()]);
            }
            self.discover_organizations()
        }

        #[cfg(test)]
        fn for_test(
            session_key: &str,
            organization_uuid: Option<String>,
            base_url: String,
        ) -> Result<Self> {
            let store = Self::build(
                Credentials {
                    session_key: session_key.to_string(),
                    session_key_v3: None,
                    session_key_lc: None,
                    session_key_v3_lc: None,
                    cf_bm: None,
                    cf_clearance: None,
                    routing_hint: None,
                    last_active_org: None,
                    anonymous_id: None,
                    device_id: None,
                    activity_session_id: None,
                    client_platform: "web_claude_ai",
                },
                organization_uuid,
                base_url,
            )?;
            // Mock tests model the API contract directly and do not need the
            // production app-shell metadata preflight.
            let _ = store.client_metadata.set(None);
            Ok(store)
        }

        fn discover_organization(
            &self,
            organization: &str,
        ) -> Result<Vec<Discovered<ClaudeChatRef>>> {
            let mut found = Vec::new();
            let mut offset = 0;
            loop {
                let path = format!(
                    "/api/organizations/{organization}/chat_conversations_v2?limit={PAGE_SIZE}&offset={offset}&consistency=strong"
                );
                let value = self.get_json(&path)?;
                let rows = value
                    .get("data")
                    .and_then(Value::as_array)
                    .or_else(|| value.as_array())
                    .ok_or_else(|| {
                        protocol_error("conversation list response is missing array `data`")
                    })?;
                let has_more = value.get("has_more").and_then(Value::as_bool);
                for row in rows {
                    let id = row.get("uuid").and_then(Value::as_str).ok_or_else(|| {
                        protocol_error("conversation summary is missing string `uuid`")
                    })?;
                    validate_uuid("conversation", id)?;
                    let body = summary_as_conversation(row)?;
                    let meta = meta_from_conversation(&body);
                    let updated_at = row
                        .get("updated_at")
                        .and_then(Value::as_str)
                        .and_then(|timestamp| timestamp.parse().ok());
                    found.push(Discovered {
                        meta,
                        reference: ClaudeChatRef {
                            organization_uuid: organization.to_string(),
                            conversation_uuid: id.to_string(),
                            updated_at,
                        },
                    });
                }
                if has_more == Some(false) || (has_more.is_none() && rows.len() < PAGE_SIZE) {
                    break;
                }
                if rows.is_empty() {
                    return Err(protocol_error(
                        "conversation list reported `has_more` with an empty page",
                    ));
                }
                offset += rows.len();
            }
            Ok(found)
        }

        fn cookie_header(&self) -> String {
            let mut parts = self.session_cookie_parts();
            for (name, value) in [
                ("routingHint", self.credentials.routing_hint.as_deref()),
                ("lastActiveOrg", self.credentials.last_active_org.as_deref()),
                ("ajs_anonymous_id", self.credentials.anonymous_id.as_deref()),
                ("anthropic-device-id", self.credentials.device_id.as_deref()),
                (
                    "activitySessionId",
                    self.credentials.activity_session_id.as_deref(),
                ),
                ("__cf_bm", self.credentials.cf_bm.as_deref()),
                ("cf_clearance", self.credentials.cf_clearance.as_deref()),
            ] {
                if let Some(value) = value {
                    parts.push(format!("{name}={value}"));
                }
            }
            parts.join("; ")
        }

        fn session_cookie_parts(&self) -> Vec<String> {
            let mut parts = vec![format!("sessionKey={}", self.credentials.session_key)];
            for (name, value) in [
                ("sessionKeyV3", self.credentials.session_key_v3.as_deref()),
                ("sessionKeyLC", self.credentials.session_key_lc.as_deref()),
                (
                    "sessionKeyV3LC",
                    self.credentials.session_key_v3_lc.as_deref(),
                ),
            ] {
                if let Some(value) = value {
                    parts.push(format!("{name}={value}"));
                }
            }
            parts
        }

        fn session_cookie_header(&self) -> String {
            self.session_cookie_parts().join("; ")
        }

        fn base_request_headers(&self) -> Vec<(&'static str, String)> {
            let mut headers = vec![(
                "anthropic-client-platform",
                self.credentials.client_platform.to_string(),
            )];
            if let Some(value) = &self.credentials.anonymous_id {
                headers.push(("anthropic-anonymous-id", value.clone()));
            }
            if let Some(value) = &self.credentials.device_id {
                headers.push(("anthropic-device-id", value.clone()));
            }
            if let Some(value) = &self.credentials.activity_session_id {
                headers.push(("x-activity-session-id", value.clone()));
            }
            headers
        }

        fn request_headers(&self) -> Vec<(&'static str, String)> {
            let mut headers = self.base_request_headers();
            if let Some(metadata) = self
                .client_metadata
                .get_or_init(|| self.fetch_client_metadata().ok())
            {
                headers.extend([
                    ("anthropic-client-version", metadata.version.clone()),
                    ("anthropic-client-sha", metadata.git_hash.clone()),
                    ("anthropic-client-build", metadata.build_timestamp.clone()),
                ]);
            }
            headers
        }

        fn fetch_client_metadata(&self) -> Result<ClientMetadata> {
            let response = self.agent.get(
                format!("{}/new", self.base_url),
                self.cookie_header(),
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                self.base_request_headers(),
                MAX_APP_SHELL_BYTES,
            )?;
            if !(200..300).contains(&response.status) {
                return Err(remote_error(&response));
            }
            let html = std::str::from_utf8(&response.body).map_err(|error| Error::Remote {
                harness: ClaudeChat::NAME,
                detail: format!("Claude's app shell was not UTF-8: {error}"),
            })?;
            Ok(ClientMetadata {
                version: html_data_attribute(html, "version")
                    .ok_or_else(|| protocol_error("app shell is missing `data-version`"))?,
                git_hash: html_data_attribute(html, "git-hash")
                    .ok_or_else(|| protocol_error("app shell is missing `data-git-hash`"))?,
                build_timestamp: html_data_attribute(html, "build-timestamp")
                    .ok_or_else(|| protocol_error("app shell is missing `data-build-timestamp`"))?,
            })
        }

        fn get_json(&self, path: &str) -> Result<Value> {
            self.get_json_with_cookie(path, self.cookie_header())
        }

        fn get_json_with_cookie(&self, path: &str, cookie: String) -> Result<Value> {
            let url = format!("{}{path}", self.base_url);
            let response = self.agent.get(
                url,
                cookie,
                "application/json",
                self.request_headers(),
                MAX_RESPONSE_BYTES,
            )?;
            if !(200..300).contains(&response.status) {
                return Err(remote_error(&response));
            }
            serde_json::from_slice(&response.body).map_err(|error| Error::Remote {
                harness: ClaudeChat::NAME,
                detail: format!("Claude returned unexpected JSON: {error}"),
            })
        }

        fn hydrate_images(&self, conversation: &mut Conversation) {
            let candidates: Vec<(String, String, Option<String>)> = conversation
                .chat_messages
                .iter()
                .flat_map(|message| {
                    ["files", "files_v2", "attachments"]
                        .into_iter()
                        .filter_map(move |field| message.get(field).and_then(Value::as_array))
                })
                .flatten()
                .filter_map(image_candidate)
                .collect();
            for (key, url, hinted_mime) in candidates {
                if conversation.hydrated_images.contains_key(&key) {
                    continue;
                }
                if let Some(source) = self.download_image(&url, hinted_mime.as_deref()) {
                    conversation
                        .hydrated_images
                        .insert(key, serde_json::to_value(source).unwrap_or(Value::Null));
                }
            }
        }

        fn hydrate_files(
            &self,
            organization: &str,
            conversation_uuid: &str,
            conversation: &mut Conversation,
        ) {
            let mut candidates = HashMap::new();
            for message in super::active_path(conversation) {
                collect_file_candidates(message, 0, &mut candidates);
            }
            if candidates.is_empty() {
                return;
            }
            let Some(files) = self.list_sandbox_files(organization, conversation_uuid) else {
                return;
            };
            for candidate in candidates.into_values() {
                if conversation.hydrated_files.contains_key(&candidate.uuid) {
                    continue;
                }
                let Some(metadata) = sandbox_file_for(&candidate, &files) else {
                    continue;
                };
                if let Some(file) =
                    self.download_file(organization, conversation_uuid, &candidate, metadata)
                {
                    conversation.hydrated_files.insert(candidate.uuid, file);
                }
            }
        }

        fn list_sandbox_files(
            &self,
            organization: &str,
            conversation_uuid: &str,
        ) -> Option<Vec<SandboxFile>> {
            validate_uuid("organization", organization).ok()?;
            validate_uuid("conversation", conversation_uuid).ok()?;
            let path = format!(
                "/api/organizations/{organization}/conversations/{conversation_uuid}/wiggle/list-files?prefix="
            );
            let value = self.get_json(&path).ok()?;
            Some(
                value
                    .get("files_metadata")
                    .and_then(Value::as_array)?
                    .iter()
                    .filter_map(sandbox_file)
                    .collect::<Vec<_>>(),
            )
        }

        fn download_file(
            &self,
            organization: &str,
            conversation_uuid: &str,
            candidate: &FileCandidate,
            metadata: &SandboxFile,
        ) -> Option<Value> {
            // Both identifiers are validated before they become path
            // components. The origin itself is fixed in production.
            validate_uuid("organization", organization).ok()?;
            validate_uuid("conversation", conversation_uuid).ok()?;
            let url = format!(
                "{}/api/organizations/{organization}/conversations/{conversation_uuid}/wiggle/download-file?path={}",
                self.base_url,
                encode_query_component(&metadata.path)
            );
            let response = self
                .agent
                .get(
                    url,
                    self.cookie_header(),
                    "application/octet-stream,*/*;q=0.8",
                    self.request_headers(),
                    MAX_FILE_BYTES,
                )
                .ok()?;
            if !(200..300).contains(&response.status) {
                return None;
            }
            let media_type = candidate
                .media_type
                .clone()
                .or_else(|| Some(metadata.content_type.clone()));
            let size = response.body.len();
            let file_name = metadata
                .path
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or("artifact");
            Some(serde_json::json!({
                "name": candidate.name,
                "file_name": file_name,
                "media_type": media_type,
                "size": size,
                "data": base64::engine::general_purpose::STANDARD.encode(response.body),
            }))
        }

        fn download_image(
            &self,
            source_url: &str,
            hinted_mime: Option<&str>,
        ) -> Option<crate::common::ImageSource> {
            let url = if source_url.starts_with('/') {
                format!("{}{source_url}", self.base_url)
            } else if source_url
                .strip_prefix(&self.base_url)
                .is_some_and(|suffix| suffix.starts_with('/'))
            {
                source_url.to_string()
            } else {
                // Never forward a Claude credential to another host.
                return None;
            };
            let response = self
                .agent
                .get(
                    url,
                    self.cookie_header(),
                    "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
                    self.request_headers(),
                    MAX_IMAGE_BYTES,
                )
                .ok()?;
            if !(200..300).contains(&response.status) {
                return None;
            }
            let media_type = response
                .content_type
                .as_deref()
                .and_then(|value| value.split(';').next())
                .filter(|value| value.starts_with("image/"))
                .or(hinted_mime.filter(|value| value.starts_with("image/")))?
                .to_string();
            Some(crate::common::ImageSource {
                source_type: "base64".to_string(),
                media_type,
                data: base64::engine::general_purpose::STANDARD.encode(response.body),
            })
        }
    }

    impl Store for ClaudeChatStore {
        type H = ClaudeChat;
        type Ref = ClaudeChatRef;

        fn discover(&self) -> Result<Vec<Discovered<Self::Ref>>> {
            eprintln!(
                "warning: Claude Chat discovery enumerates the selected account's conversation list through an undocumented private claude.ai endpoint that Anthropic can observe or restrict"
            );
            let mut found = Vec::new();
            for organization in self.organizations()? {
                found.extend(self.discover_organization(&organization)?);
            }
            Ok(found)
        }

        fn load(&self, reference: &Self::Ref) -> Result<Transcript<Self::H>> {
            validate_uuid("conversation", &reference.conversation_uuid)?;
            let organization_uuid = &reference.organization_uuid;
            validate_uuid("organization", organization_uuid)?;
            let path = format!(
                "/api/organizations/{}/chat_conversations/{}?tree=True&rendering_mode=messages&render_all_tools=true",
                organization_uuid, reference.conversation_uuid
            );
            let value = self.get_json(&path)?;
            if value.get("uuid").and_then(Value::as_str)
                != Some(reference.conversation_uuid.as_str())
                || !value.get("chat_messages").is_some_and(Value::is_array)
            {
                return Err(protocol_error(
                    "conversation detail has the wrong `uuid` or no `chat_messages` array",
                ));
            }
            let mut conversation: Conversation =
                serde_json::from_value(value).map_err(|error| Error::Remote {
                    harness: ClaudeChat::NAME,
                    detail: format!("Claude conversation shape changed: {error}"),
                })?;
            self.hydrate_images(&mut conversation);
            self.hydrate_files(
                organization_uuid,
                &reference.conversation_uuid,
                &mut conversation,
            );
            let meta = meta_from_conversation(&conversation);
            Ok(Transcript::new(meta, conversation))
        }

        fn save(&self, _: &Transcript<Self::H>) -> Result<Saved<Self::Ref>> {
            Err(read_only_error())
        }

        fn delete(&self, _: &Self::Ref) -> Result<()> {
            Err(read_only_error())
        }

        fn fingerprints(&self, refs: &[Self::Ref]) -> Result<HashMap<String, String>> {
            Ok(refs
                .iter()
                .map(|reference| {
                    let fingerprint = reference
                        .updated_at
                        .map(|timestamp| timestamp.to_rfc3339())
                        .unwrap_or_default();
                    (reference.key(), fingerprint)
                })
                .collect())
        }
    }

    fn summary_as_conversation(value: &Value) -> Result<Conversation> {
        let mut object = value
            .as_object()
            .cloned()
            .ok_or_else(|| protocol_error("conversation summary is not an object"))?;
        object.insert("chat_messages".to_string(), Value::Array(Vec::new()));
        serde_json::from_value(Value::Object(object)).map_err(|error| {
            protocol_error(&format!("conversation summary shape changed: {error}"))
        })
    }

    fn image_candidate(file: &Value) -> Option<(String, String, Option<String>)> {
        let url = file
            .get("preview_url")
            .or_else(|| file.get("thumbnail_url"))
            .and_then(Value::as_str)?;
        let key = file
            .get("uuid")
            .or_else(|| file.get("id"))
            .and_then(Value::as_str)
            .unwrap_or(url)
            .to_string();
        let mime = file
            .get("file_type")
            .or_else(|| file.get("mime_type"))
            .and_then(Value::as_str)
            .map(String::from);
        Some((key, url.to_string(), mime))
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct FileCandidate {
        uuid: String,
        name: String,
        media_type: Option<String>,
        path: Option<String>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct SandboxFile {
        path: String,
        content_type: String,
    }

    fn collect_file_candidates(
        value: &Value,
        depth: usize,
        out: &mut HashMap<String, FileCandidate>,
    ) {
        if depth > 24 {
            return;
        }
        match value {
            Value::Object(object) => {
                if object.get("type").and_then(Value::as_str) == Some("local_resource")
                    && let Some(uuid) = object.get("uuid").and_then(Value::as_str)
                    && Uuid::parse_str(uuid).is_ok()
                {
                    let name = object
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|name| !name.trim().is_empty())
                        .unwrap_or("artifact")
                        .to_string();
                    let media_type = object
                        .get("mime_type")
                        .or_else(|| object.get("media_type"))
                        .and_then(Value::as_str)
                        .map(String::from);
                    let candidate = FileCandidate {
                        uuid: uuid.to_string(),
                        name,
                        media_type,
                        path: object
                            .get("file_path")
                            .and_then(Value::as_str)
                            .map(String::from),
                    };
                    // Wiggle addresses the current sandbox file by path, not
                    // a historical resource UUID. If Claude presented the
                    // same output path several times, only the final card can
                    // be hydrated honestly; mapping today's bytes onto older
                    // revisions would fabricate their contents.
                    let key = candidate
                        .path
                        .clone()
                        .unwrap_or_else(|| candidate.uuid.clone());
                    out.insert(key, candidate);
                }
                for nested in object.values() {
                    collect_file_candidates(nested, depth + 1, out);
                }
            }
            Value::Array(values) => {
                for nested in values {
                    collect_file_candidates(nested, depth + 1, out);
                }
            }
            Value::String(text) if text.starts_with(['{', '[']) => {
                if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                    collect_file_candidates(&parsed, depth + 1, out);
                }
            }
            _ => {}
        }
    }

    fn sandbox_file(value: &Value) -> Option<SandboxFile> {
        Some(SandboxFile {
            path: value.get("path")?.as_str()?.to_string(),
            content_type: value
                .get("content_type")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream")
                .to_string(),
        })
    }

    fn sandbox_file_for<'a>(
        candidate: &FileCandidate,
        files: &'a [SandboxFile],
    ) -> Option<&'a SandboxFile> {
        let path = candidate.path.as_deref()?;
        files.iter().find(|file| file.path == path).or_else(|| {
            let basename = path.rsplit('/').next()?;
            let mut matches = files
                .iter()
                .filter(|file| file.path.rsplit('/').next() == Some(basename));
            let first = matches.next()?;
            matches.next().is_none().then_some(first)
        })
    }

    fn encode_query_component(value: &str) -> String {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut encoded = String::with_capacity(value.len());
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                encoded.push(char::from(byte));
            } else {
                encoded.push('%');
                encoded.push(char::from(HEX[usize::from(byte >> 4)]));
                encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
            }
        }
        encoded
    }

    fn nonempty_env(name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    fn disabled_credential_variables(
        mut is_set: impl FnMut(&'static str) -> bool,
    ) -> Vec<&'static str> {
        DISABLED_CREDENTIAL_ENV_VARS
            .into_iter()
            .filter(|name| is_set(name))
            .collect()
    }

    fn html_data_attribute(html: &str, name: &str) -> Option<String> {
        ['"', '\''].into_iter().find_map(|quote| {
            let marker = format!("data-{name}={quote}");
            let value = html.split_once(&marker)?.1.split_once(quote)?.0;
            (!value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
            .then(|| value.to_string())
        })
    }

    fn validate_cookie(name: &str, value: &str) -> Result<()> {
        if value.is_empty() || value.contains(';') || value.chars().any(char::is_control) {
            return Err(Error::Remote {
                harness: ClaudeChat::NAME,
                detail: format!("{name} has an invalid cookie value"),
            });
        }
        Ok(())
    }

    fn validate_uuid(kind: &str, id: &str) -> Result<()> {
        Uuid::parse_str(id)
            .map(|_| ())
            .map_err(|_| protocol_error(&format!("{kind} id is not a UUID")))
    }

    fn protocol_error(detail: &str) -> Error {
        Error::Remote {
            harness: ClaudeChat::NAME,
            detail: format!("Claude's private web API changed: {detail}"),
        }
    }

    fn remote_error(response: &BrowserResponse) -> Error {
        let mut detail = match response.status {
            401 => {
                "Claude rejected the Desktop session; sign in again in Claude Desktop, then retry"
                    .to_string()
            }
            403 if response.cf_mitigated => {
                "Cloudflare challenged the read even with Claude Desktop's browser profile; open Claude Desktop once, then retry"
                    .to_string()
            }
            403 => {
                "Claude refused the authenticated read; the account or organization may not allow this private endpoint"
                    .to_string()
            }
            429 => {
                "Claude rate-limited the read; wait and try again".to_string()
            }
            status => format!("Claude returned HTTP {status}"),
        };
        if let Some(message) = safe_server_message(response) {
            detail.push_str(": ");
            detail.push_str(&message);
        }
        Error::Remote {
            harness: ClaudeChat::NAME,
            detail,
        }
    }

    fn safe_server_message(response: &BrowserResponse) -> Option<String> {
        let is_json = response
            .content_type
            .as_deref()
            .is_some_and(|value| value.starts_with("application/json"));
        if !is_json {
            return None;
        }
        let value: Value = serde_json::from_slice(&response.body).ok()?;
        ["/error/message", "/message", "/error/type", "/type"]
            .into_iter()
            .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
            .filter(|message| {
                !message.is_empty()
                    && message.len() <= 200
                    && !message.chars().any(char::is_control)
            })
            .map(String::from)
    }

    #[cfg(target_os = "macos")]
    fn desktop_credentials() -> Result<Credentials> {
        macos::read_desktop_credentials()
    }

    #[cfg(not(target_os = "macos"))]
    fn desktop_credentials() -> Result<Credentials> {
        Err(Error::Remote {
            harness: ClaudeChat::NAME,
            detail: "Claude Desktop credential reuse is supported only on macOS in V1".to_string(),
        })
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use std::collections::HashMap;
        use std::fs;
        use std::path::{Path, PathBuf};
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};

        use aes::Aes128;
        use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
        use cbc::Decryptor;
        use pbkdf2::pbkdf2_hmac;
        use rusqlite::{Connection, OpenFlags};
        use sha1::Sha1;
        use sha2::{Digest, Sha256};

        use super::{Credentials, validate_cookie};
        use crate::error::{Error, Result};
        use crate::harness::{claude_chat::ClaudeChat, home_dir};
        use crate::transcript::Harness;

        type Aes128CbcDec = Decryptor<Aes128>;

        pub(super) fn read_desktop_credentials() -> Result<Credentials> {
            let home = home_dir().ok_or_else(|| desktop_error("home directory is unavailable"))?;
            let support = home.join("Library/Application Support/Claude");
            let cookie_path = [support.join("Cookies"), support.join("Network/Cookies")]
                .into_iter()
                .find(|path| path.is_file())
                .ok_or_else(|| desktop_error("Claude Desktop cookie database was not found"))?;
            let copy = CookieCopy::new(&cookie_path)?;
            let connection =
                Connection::open_with_flags(&copy.database, OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .map_err(|error| {
                        desktop_error(&format!("could not open Claude Desktop cookies: {error}"))
                    })?;
            let mut statement = connection
                .prepare(
                    "SELECT host_key, name, value, encrypted_value, expires_utc FROM cookies \
                     WHERE host_key LIKE '%claude.ai%' AND name IN (\
                         'sessionKey', 'sessionKeyV3', 'sessionKeyLC', 'sessionKeyV3LC', \
                         '__cf_bm', 'cf_clearance', \
                         'routingHint', 'lastActiveOrg', 'ajs_anonymous_id', \
                         'anthropic-device-id', 'activitySessionId'\
                     ) \
                     ORDER BY last_access_utc DESC",
                )
                .map_err(|error| {
                    desktop_error(&format!("Claude Desktop cookie schema changed: {error}"))
                })?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                })
                .map_err(|error| {
                    desktop_error(&format!("could not read Claude Desktop cookies: {error}"))
                })?;
            let mut raw = Vec::new();
            for row in rows {
                raw.push(row.map_err(|error| {
                    desktop_error(&format!("could not read a Claude Desktop cookie: {error}"))
                })?);
            }
            let now = chromium_time_now()?;
            raw.retain(|(_, _, _, _, expires)| cookie_is_current(*expires, now));
            let needs_key = raw
                .iter()
                .any(|(_, _, value, encrypted, _)| value.is_empty() && !encrypted.is_empty());
            let password = if needs_key {
                Some(keychain_password()?)
            } else {
                None
            };
            let mut cookies = HashMap::new();
            for (host, name, value, encrypted, _) in raw {
                if cookies.contains_key(&name) {
                    continue;
                }
                let plaintext = if !value.is_empty() {
                    value
                } else if let Some(password) = password.as_deref() {
                    decrypt_cookie(&host, &encrypted, password)?
                } else {
                    continue;
                };
                validate_cookie(&name, &plaintext)?;
                cookies.insert(name, plaintext);
            }
            let session_key_v3 = cookies.remove("sessionKeyV3");
            let session_key = cookies
                .remove("sessionKey")
                .or_else(|| session_key_v3.clone())
                .ok_or_else(|| {
                    desktop_error("Claude Desktop has no current readable claude.ai session cookie")
                })?;
            Ok(Credentials {
                session_key,
                session_key_v3,
                session_key_lc: cookies.remove("sessionKeyLC"),
                session_key_v3_lc: cookies.remove("sessionKeyV3LC"),
                cf_bm: cookies.remove("__cf_bm"),
                cf_clearance: cookies.remove("cf_clearance"),
                routing_hint: cookies.remove("routingHint"),
                last_active_org: cookies.remove("lastActiveOrg"),
                anonymous_id: cookies.remove("ajs_anonymous_id"),
                device_id: cookies.remove("anthropic-device-id"),
                activity_session_id: cookies.remove("activitySessionId"),
                // Txcript reuses Desktop's cookie but is still a web client;
                // claiming Electron's privileged platform would be false and
                // is rejected by Claude's bootstrap authorization.
                client_platform: "web_claude_ai",
            })
        }

        fn chromium_time_now() -> Result<i64> {
            const WINDOWS_TO_UNIX_EPOCH_MICROS: u128 = 11_644_473_600_000_000;
            let unix_micros = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| desktop_error(&format!("system clock is invalid: {error}")))?
                .as_micros();
            i64::try_from(unix_micros.saturating_add(WINDOWS_TO_UNIX_EPOCH_MICROS))
                .map_err(|_| desktop_error("system clock is outside Chromium's supported range"))
        }

        pub(super) const fn cookie_is_current(expires: i64, now: i64) -> bool {
            expires == 0 || expires > now
        }

        fn keychain_password() -> Result<String> {
            for args in [
                vec![
                    "find-generic-password",
                    "-w",
                    "-s",
                    "Claude Safe Storage",
                    "-a",
                    "Claude",
                ],
                vec!["find-generic-password", "-w", "-s", "Claude Safe Storage"],
            ] {
                let output = Command::new("/usr/bin/security")
                    .args(args)
                    .output()
                    .map_err(|error| {
                        desktop_error(&format!("could not query macOS Keychain: {error}"))
                    })?;
                if output.status.success()
                    && let Ok(value) = String::from_utf8(output.stdout)
                    && !value.trim().is_empty()
                {
                    return Ok(value.trim_end().to_string());
                }
            }
            Err(desktop_error(
                "macOS Keychain did not expose `Claude Safe Storage`; open and sign in to Claude Desktop, then retry",
            ))
        }

        fn decrypt_cookie(host: &str, encrypted: &[u8], password: &str) -> Result<String> {
            let ciphertext = encrypted.strip_prefix(b"v10").ok_or_else(|| {
                desktop_error("Claude Desktop uses an unsupported cookie encryption version")
            })?;
            let mut key = [0_u8; 16];
            pbkdf2_hmac::<Sha1>(password.as_bytes(), b"saltysalt", 1003, &mut key);
            let plaintext = Aes128CbcDec::new(&key.into(), &[b' '; 16].into())
                .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
                .map_err(|_| desktop_error("Claude Desktop cookie decryption failed"))?;
            let host_digest = Sha256::digest(host.as_bytes());
            let plaintext = if plaintext.starts_with(host_digest.as_slice()) {
                // Chromium database version 24+ prefixes the plaintext with
                // SHA-256(host_key); older databases do not.
                &plaintext[host_digest.len()..]
            } else {
                plaintext.as_slice()
            };
            let value = std::str::from_utf8(plaintext)
                .map_err(|_| desktop_error(&format!("decrypted cookie for {host} is not UTF-8")))?;
            Ok(value.to_string())
        }

        struct CookieCopy {
            directory: PathBuf,
            database: PathBuf,
        }

        impl CookieCopy {
            fn new(source: &Path) -> Result<Self> {
                let directory = std::env::temp_dir().join(format!(
                    "txcript-claude-cookies-{}-{}",
                    std::process::id(),
                    uuid::Uuid::new_v4()
                ));
                fs::create_dir(&directory).map_err(Error::from)?;
                let database = directory.join("Cookies");
                if let Err(error) = fs::copy(source, &database) {
                    let _ = fs::remove_dir_all(&directory);
                    return Err(Error::from(error));
                }
                for suffix in ["-wal", "-shm"] {
                    let source_sidecar = PathBuf::from(format!("{}{suffix}", source.display()));
                    if source_sidecar.is_file() {
                        let _ =
                            fs::copy(source_sidecar, directory.join(format!("Cookies{suffix}")));
                    }
                }
                Ok(Self {
                    directory,
                    database,
                })
            }
        }

        impl Drop for CookieCopy {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.directory);
            }
        }

        fn desktop_error(detail: &str) -> Error {
            Error::Remote {
                harness: ClaudeChat::NAME,
                detail: format!("Claude Desktop authentication failed: {detail}"),
            }
        }
    }

    #[cfg(test)]
    #[allow(
        clippy::expect_used,
        clippy::needless_pass_by_value,
        clippy::panic,
        clippy::unwrap_used
    )]
    mod tests {
        use std::io::{BufRead, BufReader, Write};
        use std::net::{TcpListener, TcpStream};
        use std::sync::{Arc, Mutex};
        use std::thread;

        use chrono::{TimeZone, Utc};
        use serde_json::{Map, json};

        use super::*;
        use crate::common::{ArtifactSource, Block, Role};
        use crate::{Codec, Store};

        #[test]
        fn credential_environment_variables_are_explicitly_disabled() {
            let found = disabled_credential_variables(|name| {
                matches!(
                    name,
                    "TXCRIPT_CLAUDE_CHAT_SESSION_KEY" | "TXCRIPT_CLAUDE_CHAT_CF_CLEARANCE"
                )
            });
            assert_eq!(
                found,
                [
                    "TXCRIPT_CLAUDE_CHAT_SESSION_KEY",
                    "TXCRIPT_CLAUDE_CHAT_CF_CLEARANCE"
                ]
            );
        }

        struct MockResponse {
            status: u16,
            content_type: &'static str,
            extra_headers: &'static str,
            body: Vec<u8>,
        }

        impl MockResponse {
            fn json(value: Value) -> Self {
                Self {
                    status: 200,
                    content_type: "application/json",
                    extra_headers: "",
                    body: serde_json::to_vec(&value).unwrap(),
                }
            }

            fn status(status: u16) -> Self {
                Self {
                    status,
                    content_type: "text/plain",
                    extra_headers: "",
                    body: Vec::new(),
                }
            }

            fn json_status(status: u16, value: Value, extra_headers: &'static str) -> Self {
                Self {
                    status,
                    content_type: "application/json",
                    extra_headers,
                    body: serde_json::to_vec(&value).unwrap(),
                }
            }
        }

        fn mock_server(
            responses: Vec<MockResponse>,
        ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&requests);
            let handle = thread::spawn(move || {
                for response in responses {
                    let (stream, _) = listener.accept().unwrap();
                    serve_one(stream, response, &captured);
                }
            });
            (format!("http://{address}"), requests, handle)
        }

        fn serve_one(
            mut stream: TcpStream,
            response: MockResponse,
            requests: &Arc<Mutex<Vec<String>>>,
        ) {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut lines = Vec::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                lines.push(line.trim_end().to_string());
            }
            requests.lock().unwrap().push(lines.join("\n"));
            let reason = if response.status == 200 {
                "OK"
            } else {
                "Error"
            };
            write!(
                stream,
                "HTTP/1.1 {} {}\r\nContent-Type: {}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n",
                response.status,
                reason,
                response.content_type,
                response.extra_headers,
                response.body.len()
            )
            .unwrap();
            stream.write_all(&response.body).unwrap();
        }

        fn summary(id: &str, name: &str, created_at: &str, updated_at: &str) -> Value {
            json!({
                "uuid": id,
                "name": name,
                "created_at": created_at,
                "updated_at": updated_at,
                "model": "claude-sonnet-4-6"
            })
        }

        #[test]
        fn discover_paginates_with_get_only_and_keeps_remote_fingerprints() {
            let organization = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
            let first = "11111111-1111-4111-8111-111111111111";
            let second = "22222222-2222-4222-8222-222222222222";
            let (base, requests, handle) = mock_server(vec![
                MockResponse::json(json!({
                    "data": [summary(first, "First", "2026-01-01T00:00:00Z", "2026-01-02T00:00:00Z")],
                    "has_more": true
                })),
                MockResponse::json(json!({
                    "data": [summary(second, "Second", "2026-01-03T00:00:00Z", "2026-01-04T00:00:00Z")],
                    "has_more": false
                })),
            ]);
            let store =
                ClaudeChatStore::for_test("test-session-key", Some(organization.into()), base)
                    .unwrap();
            let found = Store::discover(&store).unwrap();
            handle.join().unwrap();

            assert_eq!(found.len(), 2);
            assert_eq!(found[0].meta.id, first);
            assert_eq!(found[1].meta.title.as_deref(), Some("Second"));
            assert_eq!(found[0].reference.organization_uuid, organization);
            let fingerprints = store
                .fingerprints(
                    &found
                        .iter()
                        .map(|item| item.reference.clone())
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            assert_eq!(
                fingerprints
                    .get(&found[0].reference.key())
                    .map(String::as_str),
                Some("2026-01-02T00:00:00+00:00")
            );

            let requests = requests.lock().unwrap();
            assert!(requests[0].starts_with(&format!(
                "GET /api/organizations/{organization}/chat_conversations_v2?limit=100&offset=0&consistency=strong HTTP/1.1"
            )));
            assert!(requests[1].contains("offset=1"));
            assert!(requests.iter().all(|request| request.starts_with("GET ")));
            assert!(requests.iter().all(|request| {
                request.contains("cookie: sessionKey=test-session-key")
                    || request.contains("Cookie: sessionKey=test-session-key")
            }));
        }

        #[test]
        fn discover_resolves_organizations_before_listing_conversations() {
            let organization = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
            let conversation = "11111111-1111-4111-8111-111111111111";
            let (base, requests, handle) = mock_server(vec![
                MockResponse::json(json!([{"uuid": organization}])),
                MockResponse::json(json!({
                    "data": [summary(
                        conversation,
                        "Discovered organization",
                        "2026-01-01T00:00:00Z",
                        "2026-01-02T00:00:00Z"
                    )],
                    "has_more": false
                })),
            ]);
            let store = ClaudeChatStore::for_test("test-session-key", None, base).unwrap();
            let found = Store::discover(&store).unwrap();
            handle.join().unwrap();

            assert_eq!(found.len(), 1);
            assert_eq!(found[0].reference.organization_uuid, organization);
            let requests = requests.lock().unwrap();
            assert!(requests[0].starts_with("GET /api/organizations HTTP/1.1"));
            assert!(requests[1].starts_with(&format!(
                "GET /api/organizations/{organization}/chat_conversations_v2?"
            )));
            assert!(requests.iter().all(|request| request.starts_with("GET ")));
        }

        #[test]
        fn desktop_active_organization_skips_rejected_account_wide_discovery() {
            let organization = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
            let conversation = "11111111-1111-4111-8111-111111111111";
            let (base, requests, handle) = mock_server(vec![MockResponse::json(json!({
                "data": [summary(
                    conversation,
                    "Current organization",
                    "2026-01-01T00:00:00Z",
                    "2026-01-02T00:00:00Z"
                )],
                "has_more": false
            }))]);
            let mut store = ClaudeChatStore::for_test("test-session-key", None, base).unwrap();
            store.active_organization_uuid = Some(organization.to_string());
            let found = Store::discover(&store).unwrap();
            handle.join().unwrap();

            assert_eq!(found.len(), 1);
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert!(requests[0].starts_with(&format!(
                "GET /api/organizations/{organization}/chat_conversations_v2?"
            )));
        }

        #[test]
        fn direct_load_uses_the_active_organization_without_discovery() {
            let organization = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
            let conversation = "11111111-1111-4111-8111-111111111111";
            let message = "33333333-3333-4333-8333-333333333333";
            let (base, requests, handle) = mock_server(vec![MockResponse::json(json!({
                "uuid": conversation,
                "name": "Direct chat",
                "created_at": "2026-01-01T00:00:00Z",
                "current_leaf_message_uuid": message,
                "chat_messages": [{
                    "uuid": message,
                    "sender": "human",
                    "created_at": "2026-01-01T00:00:00Z",
                    "content": [{"type":"text","text":"Hello"}]
                }]
            }))]);
            let mut store = ClaudeChatStore::for_test("test-session-key", None, base).unwrap();
            store.active_organization_uuid = Some(organization.to_string());

            let reference = store
                .conversation_ref(conversation.to_string(), None)
                .unwrap();
            assert_eq!(reference.organization_uuid, organization);
            let native = store.load(&reference).unwrap();
            handle.join().unwrap();

            assert_eq!(native.meta.id, conversation);
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert!(requests[0].starts_with(&format!(
                "GET /api/organizations/{organization}/chat_conversations/{conversation}?"
            )));
            assert!(!requests[0].contains("chat_conversations_v2"));
        }

        #[test]
        fn conversation_ref_requires_an_available_organization() {
            let conversation = "11111111-1111-4111-8111-111111111111";
            let store = ClaudeChatStore::for_test(
                "test-session-key",
                None,
                "http://127.0.0.1:1".to_string(),
            )
            .unwrap();

            let error = store
                .conversation_ref(conversation.to_string(), None)
                .unwrap_err();

            assert!(error.to_string().contains("no current organization"));
        }

        #[test]
        fn conversation_ref_prefers_an_explicit_organization() {
            let configured = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
            let active = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
            let explicit = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
            let conversation = "11111111-1111-4111-8111-111111111111";
            let mut store = ClaudeChatStore::for_test(
                "test-session-key",
                Some(configured.to_string()),
                "http://127.0.0.1:1".to_string(),
            )
            .unwrap();
            store.active_organization_uuid = Some(active.to_string());

            let reference = store
                .conversation_ref(conversation.to_string(), Some(explicit.to_string()))
                .unwrap();

            assert_eq!(reference.organization_uuid, explicit);
            assert_eq!(reference.conversation_uuid, conversation);
        }

        #[test]
        fn load_requests_full_tree_and_hydrates_same_origin_images() {
            let organization = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
            let conversation = "11111111-1111-4111-8111-111111111111";
            let (base, requests, handle) = mock_server(vec![
                MockResponse::json(json!({
                    "uuid": conversation,
                    "name": "Image chat",
                    "created_at": "2026-01-01T00:00:00Z",
                    "current_leaf_message_uuid": "33333333-3333-4333-8333-333333333333",
                    "chat_messages": [{
                        "uuid": "33333333-3333-4333-8333-333333333333",
                        "sender": "human",
                        "created_at": "2026-01-01T00:00:00Z",
                        "content": [{"type":"text","text":"What is this?"}],
                        "files": [{
                            "uuid": "44444444-4444-4444-8444-444444444444",
                            "file_type": "image/png",
                            "preview_url": "/image.png"
                        }]
                    }]
                })),
                MockResponse {
                    status: 200,
                    content_type: "image/png",
                    extra_headers: "",
                    body: b"png-bytes".to_vec(),
                },
            ]);
            let store =
                ClaudeChatStore::for_test("test-session-key", Some(organization.into()), base)
                    .unwrap();
            let reference = ClaudeChatRef {
                organization_uuid: organization.into(),
                conversation_uuid: conversation.into(),
                updated_at: None,
            };
            let native = store.load(&reference).unwrap();
            let common = ClaudeChat::to_common(&native).unwrap();
            handle.join().unwrap();

            assert_eq!(native.body.hydrated_images.len(), 1);
            assert_eq!(common.body.len(), 1);
            assert_eq!(common.body[0].role, Role::User);
            assert!(
                matches!(common.body[0].content.last(), Some(Block::Image { source }) if source.media_type == "image/png")
            );
            let requests = requests.lock().unwrap();
            assert!(
                requests[0].contains("tree=True&rendering_mode=messages&render_all_tools=true")
            );
            assert!(requests[1].starts_with("GET /image.png HTTP/1.1"));
            assert!(requests.iter().all(|request| request.starts_with("GET ")));
        }

        #[test]
        fn load_lists_and_hydrates_presented_artifacts_with_get_only() {
            let organization = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
            let conversation = "11111111-1111-4111-8111-111111111111";
            let message = "22222222-2222-4222-8222-222222222222";
            let file = "33333333-3333-4333-8333-333333333333";
            let tool = "toolu_present";
            let sandbox_path = "/mnt/user-data/outputs/Resume final.docx";
            let mime = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
            let resource = json!({
                "type": "local_resource",
                "uuid": file,
                "name": "Resume final",
                "mime_type": mime,
                "file_path": sandbox_path,
            });
            let (base, requests, handle) = mock_server(vec![
                MockResponse::json(json!({
                    "uuid": conversation,
                    "name": "Artifact chat",
                    "created_at": "2026-01-01T00:00:00Z",
                    "current_leaf_message_uuid": message,
                    "chat_messages": [{
                        "uuid": message,
                        "sender": "assistant",
                        "created_at": "2026-01-01T00:00:00Z",
                        "content": [
                            {"type":"tool_use","id":tool,"name":"present_files","input":{"filepaths":[sandbox_path]}},
                            {"type":"tool_result","tool_use_id":tool,"content":[resource]}
                        ]
                    }]
                })),
                MockResponse::json(json!({
                    "success": true,
                    "files_metadata": [{
                        "path": sandbox_path,
                        "size": 10,
                        "content_type": mime,
                        "created_at": "2026-01-01T00:00:00Z"
                    }]
                })),
                MockResponse {
                    status: 200,
                    content_type: "application/octet-stream",
                    extra_headers: "",
                    body: b"docx-bytes".to_vec(),
                },
            ]);
            let store =
                ClaudeChatStore::for_test("test-session-key", Some(organization.into()), base)
                    .unwrap();
            let reference = ClaudeChatRef {
                organization_uuid: organization.into(),
                conversation_uuid: conversation.into(),
                updated_at: None,
            };
            let native = store.load(&reference).unwrap();
            let common = ClaudeChat::to_common(&native).unwrap();
            handle.join().unwrap();

            assert_eq!(native.body.hydrated_files.len(), 1);
            assert_eq!(
                native.body.hydrated_files[file]
                    .get("data")
                    .and_then(Value::as_str),
                Some("ZG9jeC1ieXRlcw==")
            );
            assert!(common.body.iter().any(|message| {
                message.content.iter().any(|block| {
                    matches!(block, Block::Artifact { artifact }
                        if artifact.id == file
                            && artifact.name == "Resume final.docx"
                            && matches!(&artifact.source, ArtifactSource::Base64 { data, media_type }
                                if data == "ZG9jeC1ieXRlcw==" && media_type.as_deref() == Some(mime)))
                })
            }));
            let requests = requests.lock().unwrap();
            assert!(requests[1].starts_with(&format!(
                "GET /api/organizations/{organization}/conversations/{conversation}/wiggle/list-files?prefix= HTTP/1.1"
            )));
            assert!(requests[2].starts_with(&format!(
                "GET /api/organizations/{organization}/conversations/{conversation}/wiggle/download-file?path=%2Fmnt%2Fuser-data%2Foutputs%2FResume%20final.docx HTTP/1.1"
            )));
            assert!(requests.iter().all(|request| request.starts_with("GET ")));
        }

        #[test]
        fn authentication_failures_are_actionable_and_redacted() {
            let organization = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
            let (base, _, handle) = mock_server(vec![MockResponse::status(401)]);
            let store =
                ClaudeChatStore::for_test("secret-never-print", Some(organization.into()), base)
                    .unwrap();
            let error = Store::discover(&store).unwrap_err().to_string();
            handle.join().unwrap();
            assert!(error.contains("rejected the Desktop session"));
            assert!(error.contains("sign in again in Claude Desktop"));
            assert!(!error.contains("secret-never-print"));
        }

        #[test]
        fn authorization_errors_preserve_safe_server_guidance() {
            let organization = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
            let (base, _, handle) = mock_server(vec![MockResponse::json_status(
                403,
                json!({"error": {"message": "Invalid authorization for organization"}}),
                "",
            )]);
            let store =
                ClaudeChatStore::for_test("secret-never-print", Some(organization.into()), base)
                    .unwrap();
            let error = Store::discover(&store).unwrap_err().to_string();
            handle.join().unwrap();

            assert!(error.contains("Invalid authorization for organization"));
            assert!(!error.contains("secret-never-print"));
        }

        #[test]
        fn cloudflare_challenges_are_distinct_from_invalid_sessions() {
            let organization = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
            let (base, _, handle) = mock_server(vec![MockResponse::json_status(
                403,
                json!({"error": {"message": "challenge"}}),
                "cf-mitigated: challenge\r\n",
            )]);
            let store =
                ClaudeChatStore::for_test("secret-never-print", Some(organization.into()), base)
                    .unwrap();
            let error = Store::discover(&store).unwrap_err().to_string();
            handle.join().unwrap();

            assert!(error.contains("Cloudflare challenged"));
            assert!(!error.contains("rejected the session"));
        }

        #[test]
        fn app_shell_metadata_parser_accepts_only_header_safe_values() {
            let html = r#"<html data-version="1.0.0" data-git-hash="abc-123" data-build-timestamp="1787260420">"#;
            assert_eq!(
                html_data_attribute(html, "version").as_deref(),
                Some("1.0.0")
            );
            assert_eq!(
                html_data_attribute(html, "git-hash").as_deref(),
                Some("abc-123")
            );
            assert_eq!(
                html_data_attribute("<html data-version=\"bad value\">", "version"),
                None
            );
        }

        #[test]
        fn protocol_drift_is_not_reported_as_an_empty_store() {
            let organization = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
            let (base, _, handle) = mock_server(vec![MockResponse::json(json!({}))]);
            let store =
                ClaudeChatStore::for_test("test-session-key", Some(organization.into()), base)
                    .unwrap();
            let error = Store::discover(&store).unwrap_err().to_string();
            handle.join().unwrap();
            assert!(error.contains("private web API changed"));
            assert!(error.contains("array `data`"));
        }

        #[test]
        fn save_and_delete_refuse_without_network_access() {
            let organization = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
            let conversation = "11111111-1111-4111-8111-111111111111";
            let store = ClaudeChatStore::for_test(
                "test-session-key",
                Some(organization.into()),
                "http://127.0.0.1:1".to_string(),
            )
            .unwrap();
            let transcript = Transcript::new(
                crate::common::Meta {
                    id: conversation.into(),
                    timestamp: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
                    cwd: None,
                    git_branch: None,
                    title: None,
                    cli_version: None,
                    model: None,
                },
                Conversation {
                    chat_messages: Vec::new(),
                    hydrated_images: Map::new(),
                    hydrated_files: Map::new(),
                    extra: Map::new(),
                },
            );
            let reference = ClaudeChatRef {
                organization_uuid: organization.into(),
                conversation_uuid: conversation.into(),
                updated_at: None,
            };
            assert!(
                store
                    .save(&transcript)
                    .unwrap_err()
                    .to_string()
                    .contains("read-only source")
            );
            assert!(
                store
                    .delete(&reference)
                    .unwrap_err()
                    .to_string()
                    .contains("read-only source")
            );
        }

        #[test]
        fn image_hydration_rejects_lookalike_origins() {
            let store = ClaudeChatStore::for_test(
                "test-session-key",
                None,
                "https://claude.ai".to_string(),
            )
            .unwrap();
            assert!(
                store
                    .download_image(
                        "https://claude.ai.attacker.example/image.png",
                        Some("image/png")
                    )
                    .is_none()
            );
        }

        #[cfg(target_os = "macos")]
        #[test]
        fn desktop_cookie_expiry_filter_drops_stale_cloudflare_state() {
            assert!(super::macos::cookie_is_current(0, 100));
            assert!(super::macos::cookie_is_current(101, 100));
            assert!(!super::macos::cookie_is_current(100, 100));
            assert!(!super::macos::cookie_is_current(99, 100));
        }
    }
}

#[cfg(feature = "claude_chat")]
pub use remote::{ClaudeChatRef, ClaudeChatStore};

#[cfg(test)]
mod codec_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn data_export_arrays_are_refused() {
        let error = ClaudeChat::from_text("[]").expect_err("arrays are not live conversations");
        assert!(error.to_string().contains("data-export arrays"));
    }

    #[test]
    fn broken_parent_graph_falls_back_to_server_order() {
        let text = serde_json::to_string(&json!({
            "uuid": "11111111-1111-4111-8111-111111111111",
            "created_at": "2026-01-01T00:00:00Z",
            "current_leaf_message_uuid": "22222222-2222-4222-8222-222222222222",
            "chat_messages": [
                {"uuid":"11111111-1111-4111-8111-111111111112","sender":"human","created_at":"2026-01-01T00:00:00Z","content":[{"type":"text","text":"first"}]},
                {"uuid":"22222222-2222-4222-8222-222222222222","parent_message_uuid":"missing","sender":"assistant","created_at":"2026-01-01T00:00:01Z","content":[{"type":"text","text":"second"}]}
            ]
        })).expect("fixture serializes");
        let common = ClaudeChat::to_common(&ClaudeChat::from_text(&text).expect("fixture parses"))
            .expect("conversion succeeds");
        assert_eq!(common.body.len(), 2);
    }

    #[test]
    fn live_command_read_and_create_tools_use_claude_code_names() {
        let text = serde_json::to_string(&json!({
            "uuid": "11111111-1111-4111-8111-111111111111",
            "created_at": "2026-01-01T00:00:00Z",
            "current_leaf_message_uuid": "22222222-2222-4222-8222-222222222222",
            "chat_messages": [{
                "uuid": "22222222-2222-4222-8222-222222222222",
                "sender": "assistant",
                "created_at": "2026-01-01T00:00:00Z",
                "content": [
                    {"type":"tool_use","id":"bash","name":"bash_tool","input":{"command":"pwd","description":"Check cwd"}},
                    {"type":"tool_result","tool_use_id":"bash","content":[{"type":"text","text":"/tmp"}]},
                    {"type":"tool_use","id":"read","name":"view","input":{"path":"/tmp/a.txt","description":"Read it","view_range":[4,8]}},
                    {"type":"tool_result","tool_use_id":"read","content":[{"type":"text","text":"lines"}]},
                    {"type":"tool_use","id":"write","name":"create_file","input":{"path":"/tmp/b.txt","file_text":"body","description":"Create it"}}
                ]
            }]
        }))
        .expect("fixture serializes");
        let common = ClaudeChat::to_common(&ClaudeChat::from_text(&text).expect("fixture parses"))
            .expect("conversion succeeds");
        let tools = common
            .body
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                Block::ToolUse { tool, .. } => Some(tool),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(matches!(
            tools[0],
            Tool::Bash {
                command,
                description,
                ..
            } if command == "pwd" && description.as_deref() == Some("Check cwd")
        ));
        assert!(matches!(
            tools[1],
            Tool::Read {
                file_path,
                offset: Some(4),
                limit: Some(5)
            } if file_path == "/tmp/a.txt"
        ));
        assert!(matches!(
            tools[2],
            Tool::Write {
                file_path,
                content
            } if file_path == "/tmp/b.txt" && content == "body"
        ));
    }
}
