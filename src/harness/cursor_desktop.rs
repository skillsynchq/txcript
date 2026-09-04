//! Cursor desktop (IDE app) sessions: rows in the app's global
//! `state.vscdb` (`<User dir>/globalStorage/state.vscdb`).
//!
//! The desktop app keeps one `composerHeaders` table row per session (the
//! discovery index) and, in the `cursorDiskKV` key-value table, a
//! `composerData:<composerId>` state document plus one
//! `bubbleId:<composerId>:<bubbleId>` JSON document per message ("bubble").
//! Bubble `type` 1 is the user, 2 the assistant; assistant bubbles carry
//! `text`, `thinking`, or a `toolFormerData` tool call+result. The display
//! log is authoritative: the app lists, renders, and offers to continue a
//! session from these rows alone (verified against Cursor 3.16 by loading a
//! profile with every `agentKv` blob deleted).
//!
//! Deliberately not carried: the `agentKv:blob:*` content-addressed cache
//! (the model-side request log). Its blobs are commingled across sessions
//! with no session-scoped root pointer on disk, and the app regenerates what
//! it needs from the bubbles.
//!
//! Known representational losses through [`Common`]: image blocks, stop
//! reasons, `Block::Thinking::encrypted`, `Tool::Bash`
//! `description`/`run_in_background`, and text tool results that themselves
//! parse as JSON (they come back as [`ToolOutput::Json`]).

use std::collections::HashMap;
#[cfg(feature = "opencode")]
use std::path::Path;
use std::path::PathBuf;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::common::{Block, Message, Meta, Role, Tool, ToolOutput, Usage};
use crate::error::{Error, Result};
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

#[cfg(feature = "opencode")]
use rusqlite::{Connection, OpenFlags, params};

/// The Cursor desktop harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorDesktop;

impl Harness for CursorDesktop {
    const NAME: &'static str = "cursor_desktop";
    type Body = DesktopSession;
}

/// Faithful representation of one desktop session: its `composerHeaders`
/// row and every `cursorDiskKV` row keyed by its composer id. Cell values
/// are kept as the raw TEXT the app wrote, byte-lossless.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesktopSession {
    /// `composerHeaders.value` — the `{"type":"head",...}` JSON document.
    pub header: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub last_updated_at: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_archived: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_subagent: bool,
    #[serde(default)]
    pub recency: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_at: Option<i64>,
    /// The `composerData:<id>` row, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composer_data: Option<String>,
    /// `bubbleId:<id>:<bubbleId>` rows in database order.
    pub bubbles: Vec<DesktopRow>,
    /// Any other `cursorDiskKV` row mentioning the composer id
    /// (`checkpointId:…`, `composerVirtualRowHeights:…`, future kinds) —
    /// preserved so unmodeled record kinds survive a round trip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aux: Vec<DesktopRow>,
}

/// One raw `cursorDiskKV` row: full key and verbatim TEXT value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopRow {
    pub key: String,
    pub value: String,
}

impl Codec for CursorDesktop {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            session_messages(&transcript.body, &transcript.meta),
        ))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            session_from_messages(&transcript.meta, &transcript.body),
        ))
    }
}

impl TextCodec for CursorDesktop {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let body: DesktopSession = serde_json::from_str(text)?;
        let meta = meta_from_session(&body, None);
        Ok(Transcript::new(meta, body))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Ok(serde_json::to_string_pretty(&transcript.body)?)
    }
}

// -- meta ----------------------------------------------------------------

/// The composer id recorded in the header document, if any.
#[cfg(feature = "opencode")]
fn header_composer_id(body: &DesktopSession) -> Option<String> {
    let header: Value = serde_json::from_str(&body.header).ok()?;
    header
        .get("composerId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn meta_from_session(body: &DesktopSession, id: Option<&str>) -> Meta {
    meta_from_parts(
        &body.header,
        body.composer_data.as_deref(),
        body.created_at,
        id,
    )
}

/// Session metadata from the head document and `composerData:` cell alone —
/// everything discovery needs without touching the bubble rows.
fn meta_from_parts(
    header: &str,
    composer_data: Option<&str>,
    created_at: i64,
    id: Option<&str>,
) -> Meta {
    let header: Value = serde_json::from_str(header).unwrap_or(Value::Null);
    let composer_data = composer_data.and_then(|s| serde_json::from_str::<Value>(s).ok());
    let title = [header.get("name"), header.get("subtitle")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(str::to_string);
    let cwd = header
        .pointer("/workspaceIdentifier/uri/fsPath")
        .or_else(|| header.pointer("/agentLocation/environment/uri/fsPath"))
        .or_else(|| header.pointer("/trackedGitRepos/0/repoPath"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    // "default" is the model-picker placeholder, not a model identity.
    let model = composer_data
        .as_ref()
        .and_then(|d| d.pointer("/modelConfig/modelName"))
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty() && *m != "default")
        .map(str::to_string);
    let timestamp = DateTime::from_timestamp_millis(created_at)
        .filter(|_| created_at > 0)
        .unwrap_or_else(Utc::now);
    Meta {
        id: id
            .map(str::to_string)
            .or_else(|| {
                header
                    .get("composerId")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_default(),
        timestamp,
        cwd,
        git_branch: None,
        title,
        cli_version: None,
        model,
    }
}

// -- to_common -----------------------------------------------------------

fn parse_bubble_timestamp(bubble: &Value, fallback: DateTime<Utc>) -> DateTime<Utc> {
    bubble
        .get("createdAt")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map_or(fallback, |t| t.with_timezone(&Utc))
}

/// Bubble documents in conversation order: the order recorded in
/// `composerData.fullConversationHeadersOnly` when present, database order
/// otherwise.
fn ordered_bubbles(body: &DesktopSession) -> Vec<Value> {
    let mut by_id: HashMap<String, Value> = HashMap::new();
    let mut natural = Vec::new();
    for row in &body.bubbles {
        // A corrupt bubble drops from Common; it still survives natively.
        let Ok(value) = serde_json::from_str::<Value>(&row.value) else {
            continue;
        };
        let id = value
            .get("bubbleId")
            .and_then(Value::as_str)
            .map_or_else(|| row.key.clone(), str::to_string);
        natural.push(id.clone());
        by_id.insert(id, value);
    }
    let ordered: Vec<String> = body
        .composer_data
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|d| {
            d.get("fullConversationHeadersOnly")
                .and_then(Value::as_array)
                .map(|headers| {
                    headers
                        .iter()
                        .filter_map(|h| h.get("bubbleId").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
        })
        .filter(|ids: &Vec<String>| !ids.is_empty())
        .unwrap_or(natural);
    ordered
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect()
}

fn session_messages(body: &DesktopSession, meta: &Meta) -> Vec<Message> {
    let fallback_ts = meta.timestamp;
    let mut messages = Vec::new();
    let mut assistant: Option<Message> = None;

    for bubble in ordered_bubbles(body) {
        let ts = parse_bubble_timestamp(&bubble, fallback_ts);
        match bubble.get("type").and_then(Value::as_u64) {
            Some(1) => {
                flush(&mut messages, &mut assistant);
                let text = bubble.get("text").and_then(Value::as_str).unwrap_or("");
                if !text.trim().is_empty() {
                    messages.push(Message {
                        role: Role::User,
                        content: vec![Block::Text {
                            text: text.to_string(),
                        }],
                        timestamp: ts,
                        model: None,
                        stop_reason: None,
                        usage: None,
                    });
                }
            }
            Some(2) => {
                let current = assistant.get_or_insert_with(|| Message {
                    role: Role::Assistant,
                    content: Vec::new(),
                    timestamp: ts,
                    model: None,
                    stop_reason: None,
                    usage: None,
                });
                if let Some(model) = bubble
                    .pointer("/modelInfo/modelName")
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty() && *m != "default")
                {
                    current.model = Some(model.to_string());
                }
                if let Some(usage) = bubble_usage(&bubble) {
                    current.usage = Some(usage);
                }
                if let Some(text) = bubble
                    .pointer("/thinking/text")
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                {
                    let signature = bubble
                        .pointer("/thinking/signature")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    current.content.push(Block::Thinking {
                        text: text.to_string(),
                        signature,
                        encrypted: None,
                    });
                }
                if let Some(text) = bubble
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                {
                    current.content.push(Block::Text {
                        text: text.to_string(),
                    });
                }
                if let Some(tool_call) = bubble.get("toolFormerData").filter(|t| t.is_object()) {
                    push_tool_call(&mut messages, &mut assistant, tool_call, ts, meta);
                }
            }
            _ => {}
        }
    }
    flush(&mut messages, &mut assistant);
    messages
}

fn flush(messages: &mut Vec<Message>, assistant: &mut Option<Message>) {
    if let Some(msg) = assistant.take()
        && !msg.content.is_empty()
    {
        messages.push(msg);
    }
}

fn bubble_usage(bubble: &Value) -> Option<Usage> {
    let input = bubble
        .pointer("/tokenCount/inputTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = bubble
        .pointer("/tokenCount/outputTokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    // All-zero counts are the serializer default, not an observation.
    (input > 0 || output > 0).then_some(Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
    })
}

/// Emit the `ToolUse` on the assistant message and its paired result on the
/// following user message, per the Anthropic convention.
fn push_tool_call(
    messages: &mut Vec<Message>,
    assistant: &mut Option<Message>,
    tool_call: &Value,
    ts: DateTime<Utc>,
    meta: &Meta,
) {
    let name = tool_call
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params: Value = tool_call
        .get("params")
        .and_then(Value::as_str)
        .and_then(|p| serde_json::from_str(p).ok())
        .unwrap_or_else(|| json!({}));
    // Cursor call ids can carry characters (a literal newline between the
    // call and function-call halves) that other harnesses' id grammars
    // refuse; fold them into the canonical-safe alphabet.
    let id = tool_call
        .get("toolCallId")
        .and_then(Value::as_str)
        .map(sanitize_call_id)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| deterministic_id(&meta.id, messages.len(), "call"));
    assistant
        .get_or_insert_with(|| Message {
            role: Role::Assistant,
            content: Vec::new(),
            timestamp: ts,
            model: None,
            stop_reason: None,
            usage: None,
        })
        .content
        .push(Block::ToolUse {
            id: id.clone(),
            tool: normalize_tool(name, params),
        });
    flush(messages, assistant);

    let status = tool_call
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // An in-flight call has no result to pair.
    if status != "completed" && status != "error" {
        return;
    }
    let raw = tool_call
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let content = serde_json::from_str::<Value>(raw).map_or_else(
        |_| ToolOutput::Text(raw.to_string()),
        |v| {
            if v.is_null() {
                ToolOutput::Text(raw.to_string())
            } else {
                ToolOutput::Json(v)
            }
        },
    );
    messages.push(Message {
        role: Role::User,
        content: vec![Block::ToolResult {
            tool_use_id: id,
            content,
            is_error: status == "error",
        }],
        timestamp: ts,
        model: None,
        stop_reason: None,
        usage: None,
    });
}

/// Map a native call id onto `[A-Za-z0-9_-]`, the strictest id grammar any
/// target harness enforces (Anthropic's `tool_use` pattern).
fn sanitize_call_id(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// -- tool mapping --------------------------------------------------------

/// Map a desktop-native tool call onto the Claude-canonical convention.
/// Only the shapes verified against real sessions are renamed; everything
/// else goes through [`Tool::from_canonical`], which types canonical names
/// and keeps unknown ones (`ripgrep_raw_search`, `ask_question`, MCP tools)
/// losslessly in [`Tool::Raw`].
fn normalize_tool(name: &str, params: Value) -> Tool {
    match name {
        "run_terminal_command_v2" => {
            let mut input = Map::new();
            if let Some(cmd) = params.get("command") {
                input.insert("command".into(), cmd.clone());
            }
            if let Some(cwd) = params.get("cwd").and_then(Value::as_str)
                && !cwd.is_empty()
            {
                input.insert("workdir".into(), Value::from(cwd));
            }
            if let Some(timeout) = params.pointer("/options/timeout").and_then(Value::as_u64) {
                input.insert("timeout_ms".into(), Value::from(timeout));
            }
            Tool::from_canonical("Bash", Value::Object(input))
        }
        "read_file_v2" => {
            let mut input = Map::new();
            if let Some(path) = params.get("targetFile") {
                input.insert("file_path".into(), path.clone());
            }
            for key in ["offset", "limit"] {
                if let Some(v) = params.get(key).and_then(Value::as_u64) {
                    input.insert(key.into(), Value::from(v));
                }
            }
            Tool::from_canonical("Read", Value::Object(input))
        }
        "write_file_v2" => {
            let mut input = Map::new();
            if let Some(path) = params.get("targetFile") {
                input.insert("file_path".into(), path.clone());
            }
            if let Some(contents) = params.get("contents") {
                input.insert("content".into(), contents.clone());
            }
            Tool::from_canonical("Write", Value::Object(input))
        }
        "edit_file_v2" => {
            let mut input = Map::new();
            if let Some(path) = params.get("targetFile") {
                input.insert("file_path".into(), path.clone());
            }
            if let Some(old) = params.get("oldString") {
                input.insert("old_string".into(), old.clone());
            }
            if let Some(new) = params.get("newString") {
                input.insert("new_string".into(), new.clone());
            }
            Tool::from_canonical("Edit", Value::Object(input))
        }
        _ => Tool::from_canonical(name, params),
    }
}

/// Inverse of [`normalize_tool`]: the desktop name, params, and (when known)
/// Cursor's numeric tool id.
fn denormalize_tool(tool: &Tool) -> (String, Value, Option<u64>) {
    match tool {
        Tool::Bash {
            command,
            workdir,
            timeout_ms,
            ..
        } => {
            let mut params = Map::new();
            params.insert("command".into(), Value::from(command.clone()));
            params.insert(
                "cwd".into(),
                Value::from(workdir.clone().unwrap_or_default()),
            );
            if let Some(ms) = timeout_ms {
                params.insert("options".into(), json!({ "timeout": ms }));
            }
            (
                "run_terminal_command_v2".into(),
                Value::Object(params),
                Some(15),
            )
        }
        Tool::Read {
            file_path,
            offset,
            limit,
        } => {
            let mut params = Map::new();
            params.insert("targetFile".into(), Value::from(file_path.clone()));
            if let Some(o) = offset {
                params.insert("offset".into(), Value::from(*o));
            }
            if let Some(l) = limit {
                params.insert("limit".into(), Value::from(*l));
            }
            ("read_file_v2".into(), Value::Object(params), Some(40))
        }
        Tool::Write { file_path, content } => {
            let mut params = Map::new();
            params.insert("targetFile".into(), Value::from(file_path.clone()));
            params.insert("contents".into(), Value::from(content.clone()));
            ("write_file_v2".into(), Value::Object(params), Some(43))
        }
        Tool::Edit {
            file_path,
            old_string,
            new_string,
            ..
        } => {
            let mut params = Map::new();
            params.insert("targetFile".into(), Value::from(file_path.clone()));
            params.insert("oldString".into(), Value::from(old_string.clone()));
            params.insert("newString".into(), Value::from(new_string.clone()));
            ("edit_file_v2".into(), Value::Object(params), Some(44))
        }
        other => {
            let (name, input) = other.to_canonical();
            let number = match name.as_str() {
                "ripgrep_raw_search" => Some(41),
                "glob_file_search" => Some(42),
                "ask_question" => Some(51),
                _ => None,
            };
            (name, input, number)
        }
    }
}

// -- from_common ---------------------------------------------------------

/// The complete default-state `composerData` document a fresh Cursor 3.16
/// composer carries (captured from a real draft, instance fields stripped).
/// The app's loader requires the full shape.
const COMPOSER_DATA_TEMPLATE: &str = r#"{"_v":17,"activeCustomMode":null,"activeTabsShouldBeReactive":true,"addedFiles":0,"agentBackend":"cursor-agent","allAttachedFileCodeChunksUris":[],"applied":false,"applyAgentBackendTypeRestrictions":false,"browserChipManuallyDisabled":false,"browserChipManuallyEnabled":false,"canvasPillCollapsed":false,"capabilities":[{"data":{"bubbleDataMap":"{}"},"type":15},{"data":{},"type":19},{"data":{},"type":33},{"data":{},"type":32},{"data":{},"type":23},{"data":{},"type":16},{"data":{},"type":24}],"capabilityContexts":[],"codeBlockData":{},"context":{"browserSelections":[],"composers":[],"cursorCommands":[],"cursorRules":[],"externalLinks":[],"extraContext":[],"fileSelections":[],"folderSelections":[],"gitPRDiffSelections":[],"mentions":{"browserSelections":{},"composers":{},"consoleLogs":[],"cursorCommands":{},"cursorRules":{},"diffHistory":[],"externalLinks":{},"fileSelections":{},"folderSelections":{},"gitDiff":[],"gitDiffFromBranchToMain":[],"gitPRDiffSelections":{},"ideEditorsState":[],"selectedCommits":{},"selectedDocs":{},"selectedDocuments":{},"selectedImages":{},"selectedPullRequests":{},"selectedVideos":{},"selections":{},"subagentSelections":{},"terminalFiles":{},"terminalSelections":{},"uiElementSelections":[]},"selectedCommits":[],"selectedDocs":[],"selectedDocuments":[],"selectedImages":[],"selectedPullRequests":[],"selectedVideos":[],"selections":[],"subagentSelections":[],"terminalSelections":[]},"conversationMap":{},"conversationState":"~","debugModeSuggestionUsed":false,"forceMode":"edit","generatingBubbleIds":[],"gitHubPromptDismissed":false,"hasChangedContext":true,"hasLoaded":true,"hasUnreadMessages":false,"isAgentic":true,"isApplyingWorktree":false,"isBestOfNParent":false,"isBestOfNSubcomposer":false,"isContinuationInProgress":false,"isCreatingWorktree":false,"isDraft":false,"isFileListExpanded":false,"isNAL":true,"isProject":false,"isQueueExpanded":true,"isReadingLongFile":false,"isSpec":false,"isSpecSubagentDone":false,"isUndoingWorktree":false,"modelConfig":{"maxMode":true,"modelName":"default","selectedModels":[{"modelId":"default","parameters":[]}]},"newlyCreatedFiles":[],"newlyCreatedFolders":[],"originalFileStates":{},"pendingCreateWorktree":false,"pendingExitedCustomMode":null,"planModeSuggestionUsed":false,"queueItems":[],"removedFiles":0,"richText":"","status":"none","stopHookLoopCount":0,"subComposerIds":[],"subagentComposerIds":[],"text":"","todos":[],"totalLinesAdded":0,"totalLinesRemoved":0,"trackedGitRepos":[],"unifiedMode":"agent","usageData":{},"workspaceIdentifier":{"id":"empty-window"},"worktreeStartedReadOnly":false}"#;

const NS: Uuid = Uuid::from_bytes([
    0x6b, 0x1d, 0x0c, 0x5d, 0x2e, 0x9a, 0x4f, 0x1b, 0x8c, 0x3e, 0x7a, 0x21, 0x30, 0x7b, 0xc7, 0x42,
]);

fn deterministic_id(session_id: &str, i: usize, j: impl std::fmt::Display) -> String {
    Uuid::new_v5(&NS, format!("{session_id}:{i}:{j}").as_bytes()).to_string()
}

fn iso_millis(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// The empty-collection skeleton Cursor's serializer writes on every bubble.
/// Matching it keeps synthesized bubbles shaped like native ones.
fn bubble_skeleton() -> Map<String, Value> {
    let mut m = Map::new();
    for key in [
        "approximateLintErrors",
        "lints",
        "codebaseContextChunks",
        "commits",
        "pullRequests",
        "attachedCodeChunks",
        "assistantSuggestedDiffs",
        "gitDiffs",
        "interpreterResults",
        "images",
        "attachedFolders",
        "attachedFoldersNew",
        "userResponsesToSuggestedCodeBlocks",
        "suggestedCodeBlocks",
        "diffsForCompressingFiles",
        "relevantFiles",
        "toolResults",
        "notepads",
        "capabilities",
        "multiFileLinterErrors",
        "diffHistories",
        "recentLocationsHistory",
        "recentlyViewedFiles",
        "fileDiffTrajectories",
        "docsReferences",
        "webReferences",
        "aiWebSearchResults",
        "attachedFoldersListDirResults",
        "humanChanges",
        "summarizedComposers",
        "cursorRules",
        "contextPieces",
    ] {
        m.insert(key.into(), json!([]));
    }
    m.insert("isAgentic".into(), json!(false));
    m.insert("existedSubsequentTerminalCommand".into(), json!(false));
    m.insert("existedPreviousTerminalCommand".into(), json!(false));
    m.insert("attachedHumanChanges".into(), json!(false));
    m.insert("requestId".into(), json!(""));
    m.insert("conversationState".into(), json!("~"));
    m.insert("unifiedMode".into(), json!(2));
    // The bubble reviver calls `text.replace` unconditionally on assistant
    // bubbles; `text` must exist (empty) even on thinking/tool bubbles.
    m.insert("text".into(), json!(""));
    m.insert("richText".into(), json!(""));
    m
}

fn new_bubble(
    cid: &str,
    i: usize,
    j: usize,
    bubble_type: u64,
    ts: DateTime<Utc>,
) -> Map<String, Value> {
    let mut b = bubble_skeleton();
    b.insert("_v".into(), json!(3));
    b.insert("type".into(), json!(bubble_type));
    b.insert("bubbleId".into(), Value::from(deterministic_id(cid, i, j)));
    b.insert("createdAt".into(), Value::from(iso_millis(ts)));
    b.insert(
        "tokenCount".into(),
        json!({ "inputTokens": 0, "outputTokens": 0 }),
    );
    b
}

fn session_from_messages(meta: &Meta, messages: &[Message]) -> DesktopSession {
    // Deterministic session identity: the sole non-deterministic exception
    // (a fresh v4) is avoided by deriving from the meta the caller fixed.
    let cid = if meta.id.is_empty() {
        Uuid::new_v5(
            &NS,
            format!(
                "session:{}:{}",
                meta.timestamp.timestamp_millis(),
                meta.title.as_deref().unwrap_or_default()
            )
            .as_bytes(),
        )
        .to_string()
    } else {
        meta.id.clone()
    };
    let bubbles = bubbles_from_messages(&cid, messages);
    let created = meta.timestamp.timestamp_millis();
    let headers: Vec<Value> = bubbles.iter().map(header_entry).collect();
    // The app hides untitled sessions from the Agents sidebar; fall back to
    // the first user turn the way Cursor itself titles sessions.
    let name = meta
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .or_else(|| {
            messages.iter().find_map(|m| {
                (m.role == Role::User).then(|| {
                    m.content.iter().find_map(|block| match block {
                        Block::Text { text } if !text.trim().is_empty() => {
                            Some(preview(text.trim()))
                        }
                        _ => None,
                    })
                })?
            })
        })
        .unwrap_or_else(|| "Imported session".into());
    let model_name = meta.model.clone().unwrap_or_else(|| "default".into());
    // Start from the full default-state document the app writes for a fresh
    // composer — the loader rejects sparser documents ("Failed to load
    // composer data") — then fill the per-session fields.
    let mut composer_data: Map<String, Value> =
        serde_json::from_str(COMPOSER_DATA_TEMPLATE).unwrap_or_default();
    composer_data.insert("composerId".into(), Value::from(cid.clone()));
    composer_data.insert("name".into(), Value::from(name.clone()));
    composer_data.insert("createdAt".into(), json!(created));
    composer_data.insert("lastUpdatedAt".into(), json!(created));
    composer_data.insert("fullConversationHeadersOnly".into(), Value::Array(headers));
    composer_data.insert(
        "modelConfig".into(),
        json!({
            "maxMode": false,
            "modelName": model_name,
            "selectedModels": [{ "modelId": model_name, "parameters": [] }],
        }),
    );
    let composer_data = Value::Object(composer_data);

    DesktopSession {
        header: head_json(meta, &cid, created, &name),
        workspace_id: None,
        created_at: created,
        last_updated_at: created,
        is_archived: false,
        is_subagent: false,
        recency: created,
        checkpoint_at: None,
        composer_data: Some(compact(&composer_data)),
        bubbles: bubbles
            .into_iter()
            .map(|b| {
                let bubble_id = b
                    .get("bubbleId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                DesktopRow {
                    key: format!("bubbleId:{cid}:{bubble_id}"),
                    value: compact(&Value::Object(b)),
                }
            })
            .collect(),
        aux: Vec::new(),
    }
}

fn bubbles_from_messages(cid: &str, messages: &[Message]) -> Vec<Map<String, Value>> {
    let mut bubbles: Vec<Map<String, Value>> = Vec::new();
    // tool_use_id -> bubble index, for pairing results back onto the call.
    let mut pending: HashMap<String, usize> = HashMap::new();

    for (i, msg) in messages.iter().enumerate() {
        match msg.role {
            Role::User => {
                let mut texts = Vec::new();
                for block in &msg.content {
                    match block {
                        Block::Text { text } => texts.push(text.as_str()),
                        Block::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            if let Some(&idx) = pending.get(tool_use_id) {
                                attach_result(&mut bubbles[idx], content, *is_error, msg.timestamp);
                            }
                        }
                        _ => {}
                    }
                }
                let joined = texts.join("\n\n");
                if !joined.is_empty() {
                    let mut b = new_bubble(cid, i, 0, 1, msg.timestamp);
                    // The renderer expects the user turn's ProseMirror doc
                    // and (empty) context alongside the plain text.
                    b.insert(
                        "richText".into(),
                        Value::from(compact(&rich_text_doc(&joined))),
                    );
                    b.insert("context".into(), empty_user_context());
                    b.insert("modelInfo".into(), json!({ "modelName": "default" }));
                    b.insert("text".into(), Value::from(joined));
                    bubbles.push(b);
                }
            }
            Role::Assistant => {
                for (j, block) in msg.content.iter().enumerate() {
                    let mut b = new_bubble(cid, i, j, 2, msg.timestamp);
                    if let Some(model) = &msg.model {
                        b.insert("modelInfo".into(), json!({ "modelName": model }));
                    }
                    match block {
                        Block::Text { text } => {
                            b.insert("text".into(), Value::from(text.clone()));
                        }
                        Block::Thinking {
                            text, signature, ..
                        } => {
                            b.insert("capabilityType".into(), json!(30));
                            b.insert("thinkingDurationMs".into(), json!(0));
                            b.insert(
                                "thinking".into(),
                                json!({
                                    "text": text,
                                    "signature": signature.clone().unwrap_or_default(),
                                }),
                            );
                        }
                        Block::ToolUse { id, tool } => {
                            let (name, input, number) = denormalize_tool(tool);
                            let mut call = Map::new();
                            if let Some(n) = number {
                                call.insert("tool".into(), Value::from(n));
                            }
                            call.insert("name".into(), Value::from(name));
                            call.insert("toolCallId".into(), Value::from(id.clone()));
                            call.insert("params".into(), Value::from(compact(&input)));
                            call.insert("rawArgs".into(), json!(""));
                            call.insert("status".into(), json!("started"));
                            call.insert("result".into(), json!(""));
                            b.insert("capabilityType".into(), json!(15));
                            b.insert("toolFormerData".into(), Value::Object(call));
                            pending.insert(id.clone(), bubbles.len());
                        }
                        // No native slot: images and unknown blocks drop.
                        _ => continue,
                    }
                    if let Some(usage) = msg.usage {
                        b.insert(
                            "tokenCount".into(),
                            json!({
                                "inputTokens": usage.input_tokens,
                                "outputTokens": usage.output_tokens,
                            }),
                        );
                    }
                    bubbles.push(b);
                }
            }
        }
    }

    bubbles
}

/// The `ProseMirror` document Cursor stores alongside a user turn's text.
fn rich_text_doc(text: &str) -> Value {
    json!({
        "type": "doc",
        "content": [{
            "type": "paragraph",
            "content": [{ "type": "text", "text": text }],
        }],
    })
}

/// The empty attachment-context object every native user bubble carries.
fn empty_user_context() -> Value {
    let mut ctx = Map::new();
    for key in [
        "composers",
        "selectedCommits",
        "selectedPullRequests",
        "selectedImages",
        "selectedDocuments",
        "selectedVideos",
        "folderSelections",
        "fileSelections",
        "terminalFiles",
        "selections",
        "terminalSelections",
        "selectedDocs",
        "externalLinks",
        "cursorRules",
        "cursorCommands",
        "gitPRDiffSelections",
        "subagentSelections",
        "browserSelections",
        "extraContext",
    ] {
        ctx.insert(key.into(), json!([]));
    }
    let mut mentions = Map::new();
    for key in [
        "composers",
        "selectedCommits",
        "selectedPullRequests",
        "selectedImages",
        "selectedDocuments",
        "selectedVideos",
        "folderSelections",
        "fileSelections",
        "terminalFiles",
        "selections",
        "terminalSelections",
        "selectedDocs",
        "externalLinks",
        "cursorRules",
        "cursorCommands",
        "gitPRDiffSelections",
        "subagentSelections",
        "browserSelections",
    ] {
        mentions.insert(key.into(), json!({}));
    }
    for key in [
        "gitDiff",
        "gitDiffFromBranchToMain",
        "diffHistory",
        "uiElementSelections",
        "consoleLogs",
        "ideEditorsState",
    ] {
        mentions.insert(key.into(), json!([]));
    }
    ctx.insert("mentions".into(), Value::Object(mentions));
    Value::Object(ctx)
}

fn preview(text: &str) -> String {
    text.chars().take(120).collect()
}

/// The sidebar/renderer index entry for one bubble. Without `grouping`
/// (notably `isRenderable`) the app refuses to open the conversation.
fn header_entry(b: &Map<String, Value>) -> Value {
    let mut grouping = Map::new();
    grouping.insert("isRenderable".into(), json!(true));
    grouping.insert("toolDisplayComputed".into(), json!(true));
    let is_user = b.get("type").and_then(Value::as_u64) == Some(1);
    if let Some(call) = b.get("toolFormerData") {
        grouping.insert("capabilityType".into(), json!(15));
        grouping.insert("isToolGroupable".into(), json!(true));
        if let Some(n) = call.get("tool").filter(|n| n.is_u64()) {
            grouping.insert("toolFormerTool".into(), n.clone());
        }
        if let Some(status) = call.get("status") {
            grouping.insert("toolFormerStatus".into(), status.clone());
        }
        if let Some(id) = call.get("toolCallId") {
            grouping.insert("toolCallId".into(), id.clone());
        }
    } else if let Some(thinking) = b.get("thinking") {
        grouping.insert("capabilityType".into(), json!(30));
        grouping.insert("hasThinking".into(), json!(true));
        grouping.insert(
            "thinkingDurationMs".into(),
            b.get("thinkingDurationMs").cloned().unwrap_or(json!(0)),
        );
        let _ = thinking;
    } else if let Some(text) = b.get("text").and_then(Value::as_str) {
        grouping.insert("hasText".into(), json!(true));
        grouping.insert("textPreview".into(), Value::from(preview(text)));
        if is_user {
            grouping.insert("isShortPlainText".into(), json!(text.len() < 100));
        } else {
            grouping.insert(
                "isKeptFinalAiVisibleOutsideWorkedForGroup".into(),
                json!(true),
            );
        }
    }
    json!({
        "bubbleId": b.get("bubbleId").cloned().unwrap_or_default(),
        "type": b.get("type").cloned().unwrap_or_default(),
        "grouping": grouping,
        "createdAt": b.get("createdAt").cloned().unwrap_or_default(),
    })
}

fn head_json(meta: &Meta, cid: &str, created: i64, name: &str) -> String {
    let mut header = Map::new();
    header.insert("type".into(), json!("head"));
    header.insert("composerId".into(), Value::from(cid));
    header.insert("name".into(), Value::from(name));
    header.insert("createdAt".into(), json!(created));
    header.insert("lastUpdatedAt".into(), json!(created));
    header.insert("unifiedMode".into(), json!("agent"));
    header.insert("forceMode".into(), json!("edit"));
    header.insert("isDraft".into(), json!(false));
    header.insert("isArchived".into(), json!(false));
    if let Some(cwd) = &meta.cwd {
        let uri = json!({
            "$mid": 1,
            "fsPath": cwd,
            "external": format!("file://{cwd}"),
            "path": cwd,
            "scheme": "file",
        });
        // The workspace hash is the app's own; the store fills the real one
        // from `workspaceStorage/*/workspace.json` when it can.
        header.insert(
            "workspaceIdentifier".into(),
            json!({ "id": "", "uri": uri }),
        );
        header.insert(
            "trackedGitRepos".into(),
            json!([{ "repoPath": cwd, "branches": [] }]),
        );
    }

    compact(&Value::Object(header))
}

fn attach_result(
    bubble: &mut Map<String, Value>,
    content: &ToolOutput,
    is_error: bool,
    ts: DateTime<Utc>,
) {
    let raw = match content {
        ToolOutput::Text(s) => s.clone(),
        ToolOutput::Json(v) => compact(v),
    };
    if let Some(Value::Object(call)) = bubble.get_mut("toolFormerData") {
        call.insert("result".into(), Value::from(raw));
        call.insert(
            "status".into(),
            Value::from(if is_error { "error" } else { "completed" }),
        );
    }
    // Natively the result rides the call's bubble, whose `createdAt` is the
    // result time — restamp so the result's timestamp survives the fixpoint.
    bubble.insert("createdAt".into(), Value::from(iso_millis(ts)));
}

fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

// -- store ---------------------------------------------------------------

/// Reads and writes desktop sessions in a Cursor `User` directory
/// (default `~/Library/Application Support/Cursor/User` on macOS).
#[derive(Debug, Clone)]
pub struct CursorDesktopStore {
    pub user_dir: PathBuf,
}

impl CursorDesktopStore {
    pub fn new(user_dir: impl Into<PathBuf>) -> Self {
        Self {
            user_dir: user_dir.into(),
        }
    }

    /// The platform-default Cursor `User` directory, honoring
    /// `CURSOR_DESKTOP_USER_DIR`.
    #[must_use]
    pub fn default_root() -> Option<Self> {
        if let Ok(dir) = std::env::var("CURSOR_DESKTOP_USER_DIR")
            && !dir.is_empty()
        {
            return Some(Self::new(dir));
        }
        let home = super::home_dir()?;
        let user = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/Cursor/User")
        } else if cfg!(windows) {
            home.join("AppData")
                .join("Roaming")
                .join("Cursor")
                .join("User")
        } else {
            home.join(".config/Cursor/User")
        };
        Some(Self::new(user))
    }

    fn db_path(&self) -> PathBuf {
        self.user_dir.join("globalStorage").join("state.vscdb")
    }

    /// The backing database path, for human-facing messages.
    #[must_use]
    pub fn db_display(&self) -> String {
        self.db_path().display().to_string()
    }

    /// Resolve the app's workspace hash for `cwd` by scanning
    /// `workspaceStorage/*/workspace.json`.
    #[cfg(feature = "opencode")]
    fn workspace_id_for(&self, cwd: &str) -> Option<String> {
        let root = self.user_dir.join("workspaceStorage");
        for entry in std::fs::read_dir(root).ok()?.flatten() {
            let meta_path = entry.path().join("workspace.json");
            let Ok(text) = std::fs::read_to_string(&meta_path) else {
                continue;
            };
            let Ok(ws) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let folder = ws.get("folder").and_then(Value::as_str).unwrap_or("");
            if matches_workspace_folder(folder, cwd) {
                return entry.file_name().to_str().map(str::to_string);
            }
        }
        None
    }
}

#[cfg(feature = "opencode")]
fn matches_workspace_folder(folder: &str, cwd: &str) -> bool {
    let Some(raw_path) = folder.strip_prefix("file://") else {
        return false;
    };
    let decoded = raw_path.replace("%3A", ":").replace("%3a", ":");
    normalize_path(&decoded) == normalize_path(cwd)
}

#[cfg(feature = "opencode")]
fn normalize_path(s: &str) -> String {
    let clean = s.trim_start_matches('/').replace('\\', "/");
    #[cfg(windows)]
    {
        clean.to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        clean
    }
}

#[cfg(feature = "opencode")]
impl Store for CursorDesktopStore {
    type H = CursorDesktop;
    type Ref = String;

    fn discover(&self) -> Result<Vec<Discovered<String>>> {
        let db_path = self.db_path();
        if !db_path.is_file() {
            return Ok(Vec::new());
        }
        let conn = open_ro(&db_path)?;
        if has_table(&conn, "composerHeaders")? {
            return discover_from_headers(&conn);
        }
        // Pre-table databases: walk composerData keys and read each session.
        Ok(list_headers(&conn)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|cid| {
                let body = read_session(&conn, &cid).ok()?;
                // Drafts and empty composers are not sessions.
                if body.bubbles.is_empty() {
                    return None;
                }
                Some(Discovered {
                    meta: meta_from_session(&body, Some(&cid)),
                    reference: cid,
                })
            })
            .collect())
    }

    fn load(&self, reference: &String) -> Result<Transcript<CursorDesktop>> {
        let conn = open_ro(&self.db_path())?;
        let body = read_session(&conn, reference)?;
        Ok(Transcript::new(
            meta_from_session(&body, Some(reference)),
            body,
        ))
    }

    fn save(&self, transcript: &Transcript<CursorDesktop>) -> Result<Saved<String>> {
        let id = header_composer_id(&transcript.body)
            .or_else(|| Some(transcript.meta.id.clone()).filter(|s| !s.is_empty()))
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        super::checked_id_component(CursorDesktop::NAME, &id)?;
        let mut body = transcript.body.clone();
        if body.workspace_id.is_none() {
            body.workspace_id = transcript
                .meta
                .cwd
                .as_deref()
                .and_then(|cwd| self.workspace_id_for(cwd));
        }
        let db_path = self.db_path();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(&db_path).map_err(sqlite_err)?;
        write_session(&mut conn, &id, &body)?;
        register_in_sidebar(&conn, &id, &transcript.meta, body.workspace_id.as_deref())?;
        Ok(Saved {
            id: id.clone(),
            reference: id,
        })
    }

    fn delete(&self, reference: &String) -> Result<()> {
        super::checked_id_component(CursorDesktop::NAME, reference)?;
        let conn = Connection::open(self.db_path()).map_err(sqlite_err)?;
        let deleted: usize = conn
            .execute(
                "DELETE FROM composerHeaders WHERE composerId = ?1",
                params![reference],
            )
            .map_err(sqlite_err)?;
        conn.execute(
            "DELETE FROM cursorDiskKV WHERE key = ?1 OR key LIKE ?2 OR key LIKE ?3",
            params![
                format!("composerData:{reference}"),
                format!("bubbleId:{reference}:%"),
                format!("%:{reference}%"),
            ],
        )
        .map_err(sqlite_err)?;
        if deleted == 0 {
            return Err(Error::Malformed {
                harness: CursorDesktop::NAME,
                detail: format!("no such session: {reference}"),
            });
        }
        Ok(())
    }

    fn fingerprints(&self, refs: &[String]) -> Result<HashMap<String, String>> {
        let mut out = HashMap::with_capacity(refs.len());
        let Ok(conn) = open_ro(&self.db_path()) else {
            for r in refs {
                out.insert(r.clone(), String::new());
            }
            return Ok(out);
        };
        for r in refs {
            let fp = conn
                .query_row(
                    "SELECT lastUpdatedAt FROM composerHeaders WHERE composerId = ?1",
                    params![r],
                    |row| row.get::<_, i64>(0),
                )
                .map(|t| t.to_string())
                .unwrap_or_default();
            out.insert(r.clone(), fp);
        }
        Ok(out)
    }
}

#[cfg(not(feature = "opencode"))]
impl Store for CursorDesktopStore {
    type H = CursorDesktop;
    type Ref = String;

    fn discover(&self) -> Result<Vec<Discovered<String>>> {
        Ok(Vec::new())
    }

    fn load(&self, _reference: &String) -> Result<Transcript<CursorDesktop>> {
        Err(sqlite_unavailable())
    }

    fn save(&self, _transcript: &Transcript<CursorDesktop>) -> Result<Saved<String>> {
        Err(sqlite_unavailable())
    }

    fn delete(&self, _reference: &String) -> Result<()> {
        Err(sqlite_unavailable())
    }
}

#[cfg(feature = "opencode")]
fn open_ro(db_path: &Path) -> Result<Connection> {
    Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlite_err)
}

/// Session ids from the `composerHeaders` table, falling back to scanning
/// `composerData:` keys on older databases without the table.
#[cfg(feature = "opencode")]
fn list_headers(conn: &Connection) -> Result<Vec<String>> {
    let from_table = conn
        .prepare("SELECT composerId FROM composerHeaders ORDER BY recency DESC")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()
        });
    if let Ok(ids) = from_table {
        return Ok(ids);
    }
    let mut stmt = conn
        .prepare("SELECT key FROM cursorDiskKV WHERE key LIKE 'composerData:%'")
        .map_err(sqlite_err)?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sqlite_err)?
        .filter_map(std::result::Result::ok)
        .filter_map(|k| k.strip_prefix("composerData:").map(str::to_string))
        .collect();
    Ok(ids)
}

#[cfg(feature = "opencode")]
fn has_table(conn: &Connection, name: &str) -> Result<bool> {
    conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![name],
        |row| row.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .map_err(sqlite_err)
}

/// Discovery over `composerHeaders` alone: one pass over the header table,
/// an indexed point lookup for each `composerData:` cell, and an indexed
/// range probe for "has at least one bubble". Never reads bubble bodies and
/// never scans the key-value table, so it stays flat as the store grows.
#[cfg(feature = "opencode")]
fn discover_from_headers(conn: &Connection) -> Result<Vec<Discovered<String>>> {
    let mut stmt = conn
        .prepare(
            "SELECT h.composerId, h.value, h.createdAt,
                    (SELECT CAST(value AS TEXT) FROM cursorDiskKV
                      WHERE key = 'composerData:' || h.composerId)
             FROM composerHeaders h
             WHERE EXISTS (SELECT 1 FROM cursorDiskKV
                            WHERE key >= 'bubbleId:' || h.composerId || ':'
                              AND key <  'bubbleId:' || h.composerId || ';')
             ORDER BY h.recency DESC",
        )
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(sqlite_err)?;
    Ok(rows
        .filter_map(std::result::Result::ok)
        .map(|(cid, header, created, data)| Discovered {
            meta: meta_from_parts(
                header.as_deref().unwrap_or_default(),
                data.as_deref(),
                created.unwrap_or_default(),
                Some(&cid),
            ),
            reference: cid,
        })
        .collect())
}

/// Half-open key range covering every key that starts with `prefix`: the
/// upper bound is the prefix with its last character stepped up by one
/// code point, which sorts after every extension of the prefix under
/// the default byte-wise `BINARY` collation. A range probe walks the key index;
/// the equivalent `LIKE 'prefix%'` cannot, because `LIKE` is
/// case-insensitive by default, and scans the whole table instead.
#[cfg(feature = "opencode")]
fn prefix_range(prefix: &str) -> (String, String) {
    let mut hi = prefix.to_string();
    if let Some(next) = hi.pop().and_then(|c| char::from_u32(c as u32 + 1)) {
        hi.push(next);
    } else {
        hi = prefix.to_string();
        hi.push('\u{10FFFF}');
    }
    (prefix.to_string(), hi)
}

/// Every bubble row of one session.
#[cfg(feature = "opencode")]
fn bubble_range(cid: &str) -> (String, String) {
    prefix_range(&format!("bubbleId:{cid}:"))
}

/// The distinct `<kind>:` key prefixes in the key-value table, found by
/// skipping through the key index one prefix at a time: each step seeks to
/// the smallest key past the previous prefix's range, so the cost is a
/// handful of index seeks regardless of table size. Keys without a colon
/// name no session and are stepped over individually.
#[cfg(feature = "opencode")]
fn key_kinds(conn: &Connection) -> Result<Vec<String>> {
    let mut from = conn
        .prepare("SELECT min(key) FROM cursorDiskKV WHERE key >= ?1")
        .map_err(sqlite_err)?;
    let mut after = conn
        .prepare("SELECT min(key) FROM cursorDiskKV WHERE key > ?1")
        .map_err(sqlite_err)?;
    let mut kinds = Vec::new();
    let mut next = from
        .query_row(params![""], |r| r.get::<_, Option<String>>(0))
        .map_err(sqlite_err)?;
    while let Some(key) = next {
        next = match key.find(':') {
            Some(i) => {
                let kind = key[..=i].to_string();
                let (_, hi) = prefix_range(&kind);
                kinds.push(kind);
                from.query_row(params![hi], |r| r.get(0))
            }
            None => after.query_row(params![key], |r| r.get(0)),
        }
        .map_err(sqlite_err)?;
    }
    Ok(kinds)
}

/// Rows of unmodeled kinds keyed by the session — `checkpointId:<cid>…`,
/// `composerVirtualRowHeights:<cid>`, and whatever the app adds next — in
/// database order. One indexed range probe per kind, so a session's cost
/// does not grow with the size of the store.
#[cfg(feature = "opencode")]
fn read_aux(conn: &Connection, cid: &str) -> Result<Vec<DesktopRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT rowid, key, CAST(value AS TEXT) FROM cursorDiskKV
             WHERE key >= ?1 AND key < ?2",
        )
        .map_err(sqlite_err)?;
    let mut rows: Vec<(i64, DesktopRow)> = Vec::new();
    for kind in key_kinds(conn)? {
        if matches!(kind.as_str(), "bubbleId:" | "composerData:" | "agentKv:") {
            continue;
        }
        let (lo, hi) = prefix_range(&format!("{kind}{cid}"));
        let found = stmt
            .query_map(params![lo, hi], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    DesktopRow {
                        key: row.get(1)?,
                        value: row.get(2)?,
                    },
                ))
            })
            .map_err(sqlite_err)?;
        rows.extend(found.filter_map(std::result::Result::ok));
    }
    rows.sort_by_key(|(rowid, _)| *rowid);
    Ok(rows.into_iter().map(|(_, row)| row).collect())
}

#[cfg(feature = "opencode")]
fn read_session(conn: &Connection, cid: &str) -> Result<DesktopSession> {
    let header_row = conn
        .query_row(
            "SELECT value, workspaceId, createdAt, lastUpdatedAt, isArchived, isSubagent,
                    recency, checkpointAt
             FROM composerHeaders WHERE composerId = ?1",
            params![cid],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                ))
            },
        )
        .ok();

    let cell = |key: String| -> Option<String> {
        conn.query_row(
            "SELECT CAST(value AS TEXT) FROM cursorDiskKV WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .ok()
    };
    let composer_data = cell(format!("composerData:{cid}"));
    let (header, workspace_id, created, updated, archived, subagent, recency, checkpoint) =
        if let Some(row) = header_row {
            row
        } else {
            // Pre-table databases: synthesize a head from composerData.
            let value = composer_data.clone().ok_or_else(|| Error::Malformed {
                harness: CursorDesktop::NAME,
                detail: format!("no such session: {cid}"),
            })?;
            let parsed: Value = serde_json::from_str(&value).unwrap_or(Value::Null);
            let created = parsed.get("createdAt").and_then(Value::as_i64);
            (
                format!("{{\"type\":\"head\",\"composerId\":\"{cid}\"}}"),
                None,
                created,
                parsed.get("lastUpdatedAt").and_then(Value::as_i64),
                None,
                None,
                None,
                None,
            )
        };

    let (lo, hi) = bubble_range(cid);
    let bubbles = keyed_rows(
        conn,
        "SELECT key, CAST(value AS TEXT) FROM cursorDiskKV
         WHERE key >= ?1 AND key < ?2 ORDER BY rowid",
        &[&lo, &hi],
    )?;
    let aux = read_aux(conn, cid)?;

    Ok(DesktopSession {
        header,
        workspace_id,
        created_at: created.unwrap_or_default(),
        last_updated_at: updated.unwrap_or_default(),
        is_archived: archived.unwrap_or_default() != 0,
        is_subagent: subagent.unwrap_or_default() != 0,
        recency: recency.unwrap_or_default(),
        checkpoint_at: checkpoint,
        composer_data,
        bubbles,
        aux,
    })
}

/// Register the session in the Agents-home sidebar index: the app lists
/// local agent sessions through `glass.localAgentProjects.v1` (projects by
/// workspace) and `glass.localAgentProjectMembership.v1` (composer →
/// project), both in `ItemTable`. A session absent from the membership map
/// never appears in the sidebar (found by deletion bisection against
/// Cursor 3.16).
#[cfg(feature = "opencode")]
fn register_in_sidebar(
    conn: &Connection,
    cid: &str,
    meta: &Meta,
    workspace_id: Option<&str>,
) -> Result<()> {
    let Some(cwd) = meta.cwd.as_deref().filter(|c| !c.is_empty()) else {
        return Ok(());
    };
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)",
    )
    .map_err(sqlite_err)?;
    let read = |key: &str| -> Option<String> {
        conn.query_row(
            "SELECT CAST(value AS TEXT) FROM ItemTable WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .ok()
    };
    let mut projects: Vec<Value> = read("glass.localAgentProjects.v1")
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let project_id = projects
        .iter()
        .find(|p| p.pointer("/workspace/uri/fsPath").and_then(Value::as_str) == Some(cwd))
        .and_then(|p| p.get("id").and_then(Value::as_str))
        .map(str::to_string);
    let project_id = if let Some(existing) = project_id {
        existing
    } else {
        let fresh = Uuid::new_v5(&NS, format!("project:{cwd}").as_bytes()).to_string();
        let name = Path::new(cwd)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Imported");
        let ms = meta.timestamp.timestamp_millis();
        projects.push(json!({
            "id": fresh,
            "name": name,
            "workspace": {
                "id": workspace_id.unwrap_or(""),
                "uri": {
                    "$mid": 1,
                    "fsPath": cwd,
                    "external": format!("file://{cwd}"),
                    "path": cwd,
                    "scheme": "file",
                },
            },
            "createdAt": ms,
            "lastUpdatedAt": ms,
            "isArchived": false,
        }));
        let rendered = compact(&Value::Array(projects));
        conn.execute(
            "INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?1, ?2)",
            params!["glass.localAgentProjects.v1", rendered],
        )
        .map_err(sqlite_err)?;
        fresh
    };
    let mut membership: Map<String, Value> = read("glass.localAgentProjectMembership.v1")
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    membership.insert(cid.to_string(), Value::from(project_id));
    conn.execute(
        "INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?1, ?2)",
        params![
            "glass.localAgentProjectMembership.v1",
            compact(&Value::Object(membership))
        ],
    )
    .map_err(sqlite_err)?;
    Ok(())
}

#[cfg(feature = "opencode")]
fn keyed_rows(conn: &Connection, sql: &str, like: &[&String]) -> Result<Vec<DesktopRow>> {
    let mut stmt = conn.prepare(sql).map_err(sqlite_err)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(like), |row| {
            Ok(DesktopRow {
                key: row.get(0)?,
                value: row.get(1)?,
            })
        })
        .map_err(sqlite_err)?;
    Ok(rows.filter_map(std::result::Result::ok).collect())
}

#[cfg(feature = "opencode")]
fn write_session(conn: &mut Connection, cid: &str, body: &DesktopSession) -> Result<()> {
    let tx = conn.transaction().map_err(sqlite_err)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS cursorDiskKV (key TEXT PRIMARY KEY, value BLOB);
         CREATE TABLE IF NOT EXISTS composerHeaders (
             composerId TEXT PRIMARY KEY, workspaceId TEXT, createdAt INTEGER,
             lastUpdatedAt INTEGER, isArchived INTEGER, isSubagent INTEGER,
             recency INTEGER, checkpointAt INTEGER, value TEXT)",
    )
    .map_err(sqlite_err)?;
    // Replace the session wholesale so removed bubbles don't linger.
    tx.execute(
        "DELETE FROM cursorDiskKV WHERE key LIKE ?1",
        params![format!("bubbleId:{cid}:%")],
    )
    .map_err(sqlite_err)?;
    tx.execute(
        "INSERT OR REPLACE INTO composerHeaders
         (composerId, workspaceId, createdAt, lastUpdatedAt, isArchived,
          isSubagent, recency, checkpointAt, value)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            cid,
            body.workspace_id,
            body.created_at,
            body.last_updated_at,
            i64::from(body.is_archived),
            i64::from(body.is_subagent),
            body.recency,
            body.checkpoint_at,
            body.header,
        ],
    )
    .map_err(sqlite_err)?;
    if let Some(data) = &body.composer_data {
        tx.execute(
            "INSERT OR REPLACE INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            params![format!("composerData:{cid}"), data],
        )
        .map_err(sqlite_err)?;
    }
    for row in body.bubbles.iter().chain(&body.aux) {
        tx.execute(
            "INSERT OR REPLACE INTO cursorDiskKV (key, value) VALUES (?1, ?2)",
            params![row.key, row.value],
        )
        .map_err(sqlite_err)?;
    }
    tx.commit().map_err(sqlite_err)
}

// Passed point-free to `map_err`, which hands over the error by value.
#[allow(clippy::needless_pass_by_value)]
#[cfg(feature = "opencode")]
fn sqlite_err(e: rusqlite::Error) -> Error {
    Error::Malformed {
        harness: CursorDesktop::NAME,
        detail: e.to_string(),
    }
}

#[cfg(not(feature = "opencode"))]
fn sqlite_unavailable() -> Error {
    Error::Unconvertible {
        harness: CursorDesktop::NAME,
        detail: "Cursor desktop store support requires the `opencode` feature for SQLite"
            .to_string(),
    }
}
