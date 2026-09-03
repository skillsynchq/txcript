//! Cursor agent sessions: `~/.cursor/chats/<md5(workspace)>/<id>/store.db`.
//!
//! Cursor's resumable store is a small `SQLite` database with content-addressed
//! blobs. JSON message blobs carry the conversation; non-JSON/protobuf blobs
//! carry Cursor's internal graph state. This harness preserves every blob byte
//! for native load/save, while converting JSON message blobs through `Common`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::common::{Block, EditOp, ImageSource, Message, Meta, Role, Tool, ToolOutput};
use crate::error::{Error, Result};
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

#[cfg(feature = "opencode")]
use rusqlite::{Connection, OpenFlags, params};
#[cfg(feature = "opencode")]
use std::fs;

/// The Cursor harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor;

impl Harness for Cursor {
    const NAME: &'static str = "cursor";
    type Body = CursorDb;
}

/// Faithful in-memory representation of `store.db`, plus sibling `meta.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CursorDb {
    pub blobs: Vec<CursorBlob>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub meta: Vec<CursorMetaEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_meta: Option<Value>,
}

/// One row in Cursor's `blobs` table. `id` is SHA-256 of `data`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CursorBlob {
    pub id: String,
    #[serde(with = "serde_hex")]
    pub data: Vec<u8>,
}

/// One row in Cursor's `meta` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorMetaEntry {
    pub key: String,
    pub value: String,
}

impl Codec for Cursor {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            db_to_messages(&transcript.body, &transcript.meta),
        ))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            db_from_messages(&transcript.meta, &transcript.body)?,
        ))
    }
}

impl TextCodec for Cursor {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let body: CursorDb = serde_json::from_str(text)?;
        let meta = meta_from_db(&body, None);
        Ok(Transcript::new(meta, body))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Ok(serde_json::to_string_pretty(&transcript.body)?)
    }
}

/// Reads and writes Cursor chat stores under a chats root
/// (default `~/.cursor/chats`).
#[derive(Debug, Clone)]
pub struct CursorStore {
    pub chats_dir: PathBuf,
}

impl CursorStore {
    pub fn new(chats_dir: impl Into<PathBuf>) -> Self {
        Self {
            chats_dir: chats_dir.into(),
        }
    }

    /// The default Cursor chats root, `~/.cursor/chats`.
    #[must_use]
    pub fn default_root() -> Option<Self> {
        dirs_home().map(|h| Self::new(h.join(".cursor").join("chats")))
    }

    #[cfg(feature = "opencode")]
    fn collect_sessions(&self) -> Vec<PathBuf> {
        // Unreadable directories and entries yield no sessions.
        fs::read_dir(&self.chats_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|workspace| workspace.path())
            // Stray files at the workspace level are not session directories.
            .filter(|workspace_dir| workspace_dir.is_dir())
            .flat_map(|workspace_dir| fs::read_dir(workspace_dir).into_iter().flatten().flatten())
            .map(|session| session.path().join("store.db"))
            // A session directory without a store.db has no transcript.
            .filter(|db| db.is_file())
            .collect()
    }

    #[cfg(feature = "opencode")]
    fn file_timestamp(path: &Path) -> DateTime<Utc> {
        fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| {
                let secs = i64::try_from(d.as_secs()).ok()?;
                DateTime::from_timestamp(secs, d.subsec_nanos())
            })
            .unwrap_or_else(Utc::now)
    }
}

#[cfg(feature = "opencode")]
impl Store for CursorStore {
    type H = Cursor;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        Ok(self
            .collect_sessions()
            .into_iter()
            // A store.db that fails to open or parse is not a session.
            .filter_map(|db_path| {
                let body = read_db(&db_path).ok()?;
                let mut meta = meta_from_db(&body, Some(&db_path));
                if meta.timestamp.timestamp_millis() == 0 {
                    meta.timestamp = Self::file_timestamp(&db_path);
                }
                Some(Discovered {
                    meta,
                    reference: db_path,
                })
            })
            .collect())
    }

    fn load(&self, reference: &PathBuf) -> Result<Transcript<Cursor>> {
        let body = read_db(reference)?;
        let mut meta = meta_from_db(&body, Some(reference));
        if meta.timestamp.timestamp_millis() == 0 {
            meta.timestamp = Self::file_timestamp(reference);
        }
        Ok(Transcript::new(meta, body))
    }

    fn save(&self, transcript: &Transcript<Cursor>) -> Result<Saved<PathBuf>> {
        let id = if transcript.meta.id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            transcript.meta.id.clone()
        };
        super::checked_id_component(Cursor::NAME, &id)?;
        let cwd = transcript
            .meta
            .cwd
            .clone()
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        let workspace = md5_hex(cwd.as_bytes());
        let session_dir = self.chats_dir.join(workspace).join(&id);
        fs::create_dir_all(&session_dir)?;

        let mut body = transcript.body.clone();
        ensure_cursor_meta(&mut body, &transcript.meta, &id);
        let db_path = session_dir.join("store.db");
        write_db(&db_path, &body)?;
        write_session_files(&session_dir, &body, &transcript.meta, &id)?;

        Ok(Saved {
            id,
            reference: db_path,
        })
    }

    /// The `Ref` is the session's `store.db`; deleting removes its parent
    /// directory. Guarded on shape and containment: the store file must
    /// exist and its session directory must resolve (symlinks and all) to
    /// `<chats_dir>/<workspace>/<id>`, so a stale or foreign reference never
    /// removes an unrelated tree.
    fn delete(&self, reference: &PathBuf) -> Result<()> {
        let session_dir = reference
            .parent()
            .filter(|_| reference.file_name().is_some_and(|n| n == "store.db"))
            .filter(|_| reference.is_file())
            .ok_or_else(|| Error::Malformed {
                harness: "cursor",
                detail: format!("not a session store.db path: {}", reference.display()),
            })?;
        let canon = session_dir.canonicalize()?;
        let root = self.chats_dir.canonicalize()?;
        let contained = canon
            .strip_prefix(&root)
            .is_ok_and(|rest| rest.components().count() == 2);
        if !contained {
            return Err(Error::Malformed {
                harness: "cursor",
                detail: format!(
                    "refusing to delete outside the chats root: {}",
                    reference.display()
                ),
            });
        }
        Ok(std::fs::remove_dir_all(canon)?)
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
impl Store for CursorStore {
    type H = Cursor;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        Ok(Vec::new())
    }

    fn load(&self, _reference: &PathBuf) -> Result<Transcript<Cursor>> {
        Err(sqlite_unavailable())
    }

    fn save(&self, _transcript: &Transcript<Cursor>) -> Result<Saved<PathBuf>> {
        Err(sqlite_unavailable())
    }

    fn delete(&self, _reference: &PathBuf) -> Result<()> {
        Err(sqlite_unavailable())
    }
}

// -- sqlite store --------------------------------------------------------

#[cfg(feature = "opencode")]
fn read_db(db_path: &Path) -> Result<CursorDb> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlite_err)?;
    let mut blobs_stmt = conn
        .prepare("SELECT id, data FROM blobs ORDER BY rowid")
        .map_err(sqlite_err)?;
    let blobs = blobs_stmt
        .query_map([], |row| {
            Ok(CursorBlob {
                id: row.get(0)?,
                data: row.get(1)?,
            })
        })
        .map_err(sqlite_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_err)?;

    let mut meta = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT key, value FROM meta ORDER BY key") {
        let rows = stmt
            .query_map([], |row| {
                Ok(CursorMetaEntry {
                    key: row.get(0)?,
                    value: row.get(1)?,
                })
            })
            .map_err(sqlite_err)?;
        meta = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(sqlite_err)?;
    }

    let session_meta = db_path
        .parent()
        .and_then(|p| fs::read_to_string(p.join("meta.json")).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());

    Ok(CursorDb {
        blobs,
        meta,
        session_meta,
    })
}

#[cfg(feature = "opencode")]
fn write_db(db_path: &Path, body: &CursorDb) -> Result<()> {
    let mut conn = Connection::open(db_path).map_err(sqlite_err)?;
    // One transaction around the wipe and every insert: a mid-write failure
    // (disk full, the CLI holding the database busy) rolls back to the
    // original store instead of leaving it truncated.
    let tx = conn.transaction().map_err(sqlite_err)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS blobs (id TEXT PRIMARY KEY, data BLOB);
         CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);
         DELETE FROM blobs;
         DELETE FROM meta;",
    )
    .map_err(sqlite_err)?;
    for blob in &body.blobs {
        tx.execute(
            "INSERT OR REPLACE INTO blobs (id, data) VALUES (?1, ?2)",
            params![blob.id, blob.data],
        )
        .map_err(sqlite_err)?;
    }
    for entry in &body.meta {
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![entry.key, entry.value],
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
        harness: Cursor::NAME,
        detail: e.to_string(),
    }
}

#[cfg(not(feature = "opencode"))]
fn sqlite_unavailable() -> Error {
    Error::Unconvertible {
        harness: Cursor::NAME,
        detail: "Cursor store support requires the `opencode` feature for SQLite".to_string(),
    }
}

// -- db <-> common -------------------------------------------------------

fn db_to_messages(db: &CursorDb, meta: &Meta) -> Vec<Message> {
    let fallback_ts = meta.timestamp;
    db.blobs.iter().fold(Vec::new(), |mut messages, blob| {
        // `messages.len()` numbers the message, minting deterministic ids.
        messages.extend(blob_message(blob, meta, fallback_ts, messages.len()));
        messages
    })
}

/// The conversational turn one blob carries, if any.
fn blob_message(
    blob: &CursorBlob,
    meta: &Meta,
    fallback_ts: DateTime<Utc>,
    message_idx: usize,
) -> Option<Message> {
    // Non-JSON blobs carry Cursor's internal graph state, not messages.
    let obj = serde_json::from_slice::<Value>(&blob.data).ok()?;
    let (role, content, model) = match obj.get("role").and_then(Value::as_str) {
        // Editor-injected context is not the user's turn.
        Some("user") if is_context_injection(&obj) => None,
        Some("user") => Some((Role::User, parse_user_content(&obj), None)),
        Some("assistant") => Some((
            Role::Assistant,
            parse_assistant_content(&obj, &meta.id, message_idx),
            cursor_model(&obj).or_else(|| meta.model.clone()),
        )),
        // Tool results ride along as user-role messages in `Common`.
        Some("tool") => Some((Role::User, parse_tool_content(&obj), None)),
        // The system prompt and unknown roles carry no conversational turn;
        // a blob without a string `role` is not a message.
        Some(_) | None => None,
    }?;
    // A message whose content parses to nothing carries no turn either.
    (!content.is_empty()).then(|| Message {
        role,
        content,
        timestamp: timestamp_from_obj(&obj).unwrap_or(fallback_ts),
        model,
        stop_reason: None,
        usage: None,
    })
}

fn db_from_messages(meta: &Meta, messages: &[Message]) -> Result<CursorDb> {
    let mut values = Vec::new();
    let mut tool_names = HashMap::new();

    for message in messages {
        match message.role {
            Role::User => {
                let mut user_blocks = Vec::new();
                for block in &message.content {
                    match block {
                        Block::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            let tool_name = tool_names
                                .get(tool_use_id)
                                .cloned()
                                .unwrap_or_else(|| "tool".to_string());
                            values.push(json!({
                                "role": "tool",
                                "content": [{
                                    "type": "tool-result",
                                    "toolCallId": tool_use_id,
                                    "toolName": tool_name,
                                    "result": tool_output_text(content),
                                }],
                                "createdAt": message.timestamp.timestamp_millis(),
                                "isError": is_error,
                            }));
                        }
                        other => {
                            user_blocks.push(serialize_user_block(other));
                        }
                    }
                }
                if !user_blocks.is_empty() {
                    values.push(json!({
                        "role": "user",
                        "content": user_blocks,
                        "createdAt": message.timestamp.timestamp_millis(),
                    }));
                }
            }
            Role::Assistant => {
                let content: Vec<Value> = message
                    .content
                    .iter()
                    .filter_map(|block| serialize_assistant_block(block, &mut tool_names))
                    .collect();
                if !content.is_empty() {
                    let mut obj = json!({
                        "role": "assistant",
                        "content": content,
                        "createdAt": message.timestamp.timestamp_millis(),
                    });
                    if let Some(model) = message.model.as_ref().or(meta.model.as_ref()) {
                        obj["providerOptions"] = json!({"cursor": {"modelName": model}});
                    }
                    values.push(obj);
                }
            }
        }
    }

    let mut blobs = Vec::new();
    let mut message_ids = Vec::new();
    for value in values {
        let data = serde_json::to_vec(&value)?;
        let id = sha256_hex(&data);
        message_ids.push(id.clone());
        blobs.push(CursorBlob { id, data });
    }
    blobs.extend(cursor_state_blobs(meta, messages, &message_ids));

    let mut body = CursorDb {
        blobs,
        meta: Vec::new(),
        session_meta: None,
    };
    ensure_cursor_meta(&mut body, meta, &meta.id);
    Ok(body)
}

#[derive(Debug, Default)]
struct CursorStateTurn {
    user_text: String,
    steps: Vec<CursorStateStep>,
}

#[derive(Debug)]
enum CursorStateStep {
    Assistant(String),
    Thinking(String),
    Tool(CursorStateToolCall),
}

#[derive(Debug)]
struct CursorStateToolCall {
    id: String,
    tool: Tool,
    result: Option<CursorStateToolResult>,
}

#[derive(Debug, Clone)]
struct CursorStateToolResult {
    content: ToolOutput,
    is_error: bool,
}

fn cursor_state_blobs(
    meta: &Meta,
    messages: &[Message],
    _message_ids: &[String],
) -> Vec<CursorBlob> {
    let turns = cursor_state_turns(meta, messages);
    let mut blobs = Vec::new();
    let mut turn_ids = Vec::new();

    for (turn_idx, turn) in turns.iter().enumerate() {
        let user_blob = cursor_blob(cursor_user_message_proto(meta, turn_idx, &turn.user_text));
        let user_id = sha256(&user_blob.data);
        blobs.push(user_blob);

        let step_blobs: Vec<CursorBlob> = turn
            .steps
            .iter()
            .map(|step| match step {
                CursorStateStep::Assistant(text) => cursor_assistant_step_proto(text),
                CursorStateStep::Thinking(text) => cursor_thinking_step_proto(text),
                CursorStateStep::Tool(call) => cursor_tool_step_proto(meta, call),
            })
            // A step that encodes to nothing has no blob in the turn graph.
            .filter(|data| !data.is_empty())
            .map(cursor_blob)
            .collect();
        let step_ids: Vec<_> = step_blobs.iter().map(|blob| sha256(&blob.data)).collect();
        blobs.extend(step_blobs);

        let turn_blob = cursor_blob(cursor_turn_structure_proto(&user_id, &step_ids));
        turn_ids.push(sha256(&turn_blob.data));
        blobs.push(turn_blob);
    }

    blobs.push(cursor_blob(cursor_root_state_proto(meta, &turn_ids)));
    blobs
}

fn cursor_state_turns(meta: &Meta, messages: &[Message]) -> Vec<CursorStateTurn> {
    let mut turns = Vec::new();
    let mut current: Option<CursorStateTurn> = None;
    let tool_results = cursor_tool_results(messages);
    let known_tool_uses = cursor_tool_use_ids(messages);

    for message in messages {
        match message.role {
            Role::User => {
                let user_text = user_state_text(&message.content);
                if !user_text.is_empty() {
                    if let Some(turn) = current.take() {
                        turns.push(turn);
                    }
                    current = Some(CursorStateTurn {
                        user_text,
                        steps: Vec::new(),
                    });
                }

                let tool_result_text =
                    orphan_tool_result_state_text(&message.content, &known_tool_uses);
                if !tool_result_text.is_empty() {
                    let turn = current.get_or_insert_with(|| CursorStateTurn {
                        user_text: "Tool result".to_string(),
                        steps: Vec::new(),
                    });
                    turn.steps
                        .push(CursorStateStep::Assistant(tool_result_text));
                }
            }
            Role::Assistant => {
                let turn = current.get_or_insert_with(|| CursorStateTurn {
                    user_text: meta
                        .title
                        .clone()
                        .unwrap_or_else(|| "Continue.".to_string()),
                    steps: Vec::new(),
                });
                turn.steps
                    .extend(assistant_state_steps(&message.content, &tool_results));
            }
        }
    }

    if let Some(turn) = current {
        turns.push(turn);
    }
    turns
        .into_iter()
        .filter(|turn| !turn.user_text.trim().is_empty() || !turn.steps.is_empty())
        .collect()
}

fn cursor_tool_results(messages: &[Message]) -> HashMap<String, CursorStateToolResult> {
    messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some((
                tool_use_id.clone(),
                CursorStateToolResult {
                    content: content.clone(),
                    is_error: *is_error,
                },
            )),
            // Only results carry tool output.
            Block::Text { .. }
            | Block::Thinking { .. }
            | Block::ToolUse { .. }
            | Block::Image { .. }
            | Block::Artifact { .. } => None,
        })
        .collect()
}

fn cursor_tool_use_ids(messages: &[Message]) -> HashSet<String> {
    messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            Block::ToolUse { id, .. } => Some(id.clone()),
            // Only tool uses mint ids.
            Block::Text { .. }
            | Block::Thinking { .. }
            | Block::ToolResult { .. }
            | Block::Image { .. }
            | Block::Artifact { .. } => None,
        })
        .collect()
}

fn user_state_text(blocks: &[Block]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            Block::Text { text } => Some(text.trim().to_string()),
            Block::Image { source } => Some(format!("[image: {}]", source.media_type)),
            Block::Artifact { artifact } => Some(artifact.display_text()),
            Block::Thinking { text, .. } if !text.trim().is_empty() => {
                Some(text.trim().to_string())
            }
            // Empty thinking, tool uses, and results have no user-side text.
            Block::Thinking { .. } | Block::ToolUse { .. } | Block::ToolResult { .. } => None,
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn assistant_state_steps(
    blocks: &[Block],
    tool_results: &HashMap<String, CursorStateToolResult>,
) -> Vec<CursorStateStep> {
    blocks
        .iter()
        .filter_map(|block| match block {
            Block::Text { text } if !text.trim().is_empty() => {
                Some(CursorStateStep::Assistant(text.trim().to_string()))
            }
            Block::Thinking { text, .. } if !text.trim().is_empty() => {
                Some(CursorStateStep::Thinking(text.trim().to_string()))
            }
            Block::ToolUse { id, tool } => Some(CursorStateStep::Tool(CursorStateToolCall {
                id: id.clone(),
                tool: tool.clone(),
                result: tool_results.get(id).cloned(),
            })),
            Block::Image { source } => Some(CursorStateStep::Assistant(format!(
                "[image: {}]",
                source.media_type
            ))),
            Block::Artifact { artifact } => {
                Some(CursorStateStep::Assistant(artifact.display_text()))
            }
            // Empty text/thinking is no step; results render with their calls.
            Block::Text { .. } | Block::Thinking { .. } | Block::ToolResult { .. } => None,
        })
        .collect()
}

fn tool_use_state_text(id: &str, tool: &Tool) -> String {
    let (name, input) = denormalize_tool(tool);
    let args = serde_json::to_string_pretty(&input).unwrap_or_else(|_| input.to_string());
    format!("Tool call {name} ({id}):\n{args}")
}

fn orphan_tool_result_state_text(blocks: &[Block], known_tool_uses: &HashSet<String>) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            // A result whose call is in the transcript renders with the call.
            Block::ToolResult { tool_use_id, .. } if known_tool_uses.contains(tool_use_id) => None,
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
            | Block::Image { .. }
            | Block::Artifact { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn cursor_blob(data: Vec<u8>) -> CursorBlob {
    let id = sha256_hex(&data);
    CursorBlob { id, data }
}

fn cursor_user_message_proto(meta: &Meta, turn_idx: usize, text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    pb_string(&mut out, 1, text);
    pb_string(&mut out, 2, &cursor_message_id(&meta.id, turn_idx, text));
    pb_varint_field(&mut out, 4, 1);
    out
}

fn cursor_assistant_step_proto(text: &str) -> Vec<u8> {
    let mut assistant = Vec::new();
    pb_string(&mut assistant, 1, text);
    let mut step = Vec::new();
    pb_len(&mut step, 1, &assistant);
    step
}

fn cursor_thinking_step_proto(text: &str) -> Vec<u8> {
    let mut thinking = Vec::new();
    pb_string(&mut thinking, 1, text);
    let mut step = Vec::new();
    pb_len(&mut step, 3, &thinking);
    step
}

fn cursor_tool_step_proto(meta: &Meta, call: &CursorStateToolCall) -> Vec<u8> {
    match cursor_tool_call_proto(meta, call) {
        Some(tool_call) => {
            let mut step = Vec::new();
            pb_len(&mut step, 2, &tool_call);
            step
        }
        // A tool with no Cursor-native encoding falls back to plain text.
        None => cursor_assistant_step_proto(&tool_use_state_text(&call.id, &call.tool)),
    }
}

fn cursor_tool_call_proto(meta: &Meta, call: &CursorStateToolCall) -> Option<Vec<u8>> {
    let (field, tool_data) = match &call.tool {
        Tool::Bash {
            command,
            workdir,
            timeout_ms,
            description,
            run_in_background,
        } => Some((
            1,
            cursor_shell_tool_call_proto(
                command,
                workdir
                    .as_deref()
                    .or(meta.cwd.as_deref())
                    .unwrap_or_default(),
                *timeout_ms,
                description.as_deref(),
                *run_in_background,
                &call.id,
                call.result.as_ref(),
            ),
        )),
        Tool::Read {
            file_path,
            offset,
            limit,
        } => Some((
            8,
            cursor_read_tool_call_proto(file_path, *offset, *limit, call.result.as_ref()),
        )),
        Tool::Edit {
            file_path,
            old_string,
            new_string,
            ..
        } => {
            let payload =
                CursorEditPayload::replace(file_path, old_string, new_string, call.result.as_ref());
            Some((
                12,
                cursor_edit_tool_call_proto(file_path, &payload, call.result.as_ref()),
            ))
        }
        Tool::Write { file_path, content } => {
            let payload = CursorEditPayload::write(file_path, content, call.result.as_ref());
            Some((
                12,
                cursor_edit_tool_call_proto(file_path, &payload, call.result.as_ref()),
            ))
        }
        Tool::MultiEdit { file_path, edits } => {
            let payload = CursorEditPayload::multi_edit(file_path, edits, call.result.as_ref());
            Some((
                12,
                cursor_edit_tool_call_proto(file_path, &payload, call.result.as_ref()),
            ))
        }
        // Raw tools and user commands have no Cursor-native proto shape.
        Tool::Raw { .. } | Tool::Command { .. } => None,
    }?;

    let mut out = Vec::new();
    pb_len(&mut out, field, &tool_data);
    pb_string(&mut out, 57, &call.id);
    Some(out)
}

fn cursor_shell_tool_call_proto(
    command: &str,
    workdir: &str,
    timeout_ms: Option<u64>,
    description: Option<&str>,
    run_in_background: bool,
    tool_call_id: &str,
    result: Option<&CursorStateToolResult>,
) -> Vec<u8> {
    let mut args = Vec::new();
    pb_string(&mut args, 1, command);
    pb_string(&mut args, 2, workdir);
    if let Some(timeout_ms) = timeout_ms.and_then(|v| u32::try_from(v).ok()) {
        pb_varint_field(&mut args, 3, u64::from(timeout_ms));
    }
    pb_string(&mut args, 4, tool_call_id);
    if run_in_background {
        pb_bool_field(&mut args, 11, true);
        pb_varint_field(&mut args, 13, 2);
    }
    if let Some(description) = description {
        pb_string(&mut args, 15, description);
    }

    let mut shell = Vec::new();
    pb_len(&mut shell, 1, &args);
    if let Some(result) = result {
        pb_len(
            &mut shell,
            2,
            &cursor_shell_result_proto(command, workdir, result),
        );
    }
    if let Some(description) = description {
        pb_string(&mut shell, 3, description);
    }
    shell
}

fn cursor_shell_result_proto(
    command: &str,
    workdir: &str,
    result: &CursorStateToolResult,
) -> Vec<u8> {
    let output = tool_output_text(&result.content);
    let mut shell_result = Vec::new();
    if result.is_error {
        let mut failure = Vec::new();
        pb_string(&mut failure, 1, command);
        pb_string(&mut failure, 2, workdir);
        pb_varint_field(&mut failure, 3, 1);
        pb_string(&mut failure, 6, &output);
        pb_len(&mut shell_result, 2, &failure);
    } else {
        let mut success = Vec::new();
        pb_string(&mut success, 1, command);
        pb_string(&mut success, 2, workdir);
        pb_varint_field(&mut success, 3, 0);
        pb_string(&mut success, 5, &output);
        pb_len(&mut shell_result, 1, &success);
    }
    shell_result
}

fn cursor_read_tool_call_proto(
    path: &str,
    offset: Option<u64>,
    limit: Option<u64>,
    result: Option<&CursorStateToolResult>,
) -> Vec<u8> {
    let offset_u32 = offset.and_then(|v| u32::try_from(v).ok());
    let limit_u32 = limit.and_then(|v| u32::try_from(v).ok());
    let mut args = Vec::new();
    pb_string(&mut args, 1, path);
    if let Some(offset) = offset_u32 {
        pb_varint_field(&mut args, 2, u64::from(offset));
    }
    if let Some(limit) = limit_u32 {
        pb_varint_field(&mut args, 3, u64::from(limit));
    }

    let mut read = Vec::new();
    pb_len(&mut read, 1, &args);
    if let Some(result) = result {
        pb_len(
            &mut read,
            2,
            &cursor_read_result_proto(path, offset_u32, result),
        );
    }
    read
}

fn cursor_read_result_proto(
    path: &str,
    offset: Option<u32>,
    result: &CursorStateToolResult,
) -> Vec<u8> {
    let mut read_result = Vec::new();
    let text = tool_output_text(&result.content);
    if result.is_error {
        let mut error = Vec::new();
        pb_string(&mut error, 1, &text);
        pb_len(&mut read_result, 2, &error);
    } else {
        let mut success = Vec::new();
        pb_string(&mut success, 1, &text);
        if text.is_empty() {
            pb_bool_field(&mut success, 2, true);
        }
        pb_varint_field(&mut success, 4, line_count(&text));
        pb_varint_field(&mut success, 5, text.len() as u64);
        pb_string(&mut success, 7, path);
        if let Some(offset) = offset {
            let mut range = Vec::new();
            let start = offset.saturating_add(1);
            let lines_below =
                u32::try_from(line_count(&text).saturating_sub(1)).unwrap_or(u32::MAX);
            let end = start.saturating_add(lines_below);
            pb_varint_field(&mut range, 1, u64::from(start));
            pb_varint_field(&mut range, 2, u64::from(end));
            pb_len(&mut success, 8, &range);
        }
        pb_len(&mut read_result, 1, &success);
    }
    read_result
}

fn cursor_edit_tool_call_proto(
    path: &str,
    payload: &CursorEditPayload,
    result: Option<&CursorStateToolResult>,
) -> Vec<u8> {
    let mut args = Vec::new();
    pb_string(&mut args, 1, path);
    pb_string(&mut args, 6, &payload.stream_content);

    let mut edit = Vec::new();
    pb_len(&mut edit, 1, &args);
    if let Some(result) = result {
        pb_len(
            &mut edit,
            2,
            &cursor_edit_result_proto(path, payload, result),
        );
    }
    edit
}

fn cursor_edit_result_proto(
    path: &str,
    payload: &CursorEditPayload,
    result: &CursorStateToolResult,
) -> Vec<u8> {
    let output = tool_output_text(&result.content);
    let mut edit_result = Vec::new();
    if result.is_error {
        let mut error = Vec::new();
        pb_string(&mut error, 1, path);
        pb_string(&mut error, 2, &output);
        pb_string(&mut error, 5, &output);
        pb_len(&mut edit_result, 7, &error);
    } else {
        let mut success = Vec::new();
        pb_string(&mut success, 1, path);
        if let Some(lines_added) = payload.lines_added {
            pb_varint_field(&mut success, 3, lines_added);
        }
        if let Some(lines_removed) = payload.lines_removed {
            pb_varint_field(&mut success, 4, lines_removed);
        }
        if let Some(diff_string) = &payload.diff_string {
            pb_string(&mut success, 5, diff_string);
        }
        if let Some(before_full_file_content) = &payload.before_full_file_content {
            pb_string(&mut success, 6, before_full_file_content);
        }
        pb_string(&mut success, 7, &payload.after_full_file_content);
        pb_string(&mut success, 8, &payload.message);
        pb_len(&mut edit_result, 1, &success);
    }
    edit_result
}

#[derive(Debug)]
struct CursorEditPayload {
    stream_content: String,
    diff_string: Option<String>,
    before_full_file_content: Option<String>,
    after_full_file_content: String,
    lines_added: Option<u64>,
    lines_removed: Option<u64>,
    message: String,
}

impl CursorEditPayload {
    fn replace(
        path: &str,
        old_string: &str,
        new_string: &str,
        result: Option<&CursorStateToolResult>,
    ) -> Self {
        Self {
            stream_content: new_string.to_string(),
            diff_string: Some(unified_replace_diff(path, old_string, new_string)),
            before_full_file_content: Some(old_string.to_string()),
            after_full_file_content: new_string.to_string(),
            lines_added: Some(line_count(new_string)),
            lines_removed: Some(line_count(old_string)),
            message: edit_success_message(path, result, "updated"),
        }
    }

    fn write(path: &str, content: &str, result: Option<&CursorStateToolResult>) -> Self {
        Self {
            stream_content: content.to_string(),
            diff_string: Some(unified_write_diff(path, content)),
            before_full_file_content: None,
            after_full_file_content: content.to_string(),
            lines_added: Some(line_count(content)),
            lines_removed: Some(0),
            message: edit_success_message(path, result, "written"),
        }
    }

    fn multi_edit(path: &str, edits: &[EditOp], result: Option<&CursorStateToolResult>) -> Self {
        let stream_content = edits
            .iter()
            .map(|edit| edit.new_string.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let diff_string = edits
            .iter()
            .map(|edit| unified_replace_diff(path, &edit.old_string, &edit.new_string))
            .collect::<Vec<_>>()
            .join("\n");
        let before_full_file_content = edits
            .iter()
            .map(|edit| edit.old_string.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let after_full_file_content = if stream_content.is_empty() {
            serde_json::to_string_pretty(edits).unwrap_or_else(|_| "[]".to_string())
        } else {
            stream_content.clone()
        };
        Self {
            stream_content,
            diff_string: (!diff_string.is_empty()).then_some(diff_string),
            before_full_file_content: (!before_full_file_content.is_empty())
                .then_some(before_full_file_content),
            after_full_file_content,
            lines_added: Some(
                edits
                    .iter()
                    .map(|edit| line_count(&edit.new_string))
                    .sum::<u64>(),
            ),
            lines_removed: Some(
                edits
                    .iter()
                    .map(|edit| line_count(&edit.old_string))
                    .sum::<u64>(),
            ),
            message: edit_success_message(path, result, "updated"),
        }
    }
}

fn edit_success_message(
    path: &str,
    result: Option<&CursorStateToolResult>,
    fallback_action: &str,
) -> String {
    result
        .filter(|result| !result.is_error)
        .map(|result| tool_output_text(&result.content))
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| match fallback_action {
            "written" => format!("Wrote contents to {path}"),
            _ => format!("The file {path} has been updated."),
        })
}

fn unified_replace_diff(path: &str, old_string: &str, new_string: &str) -> String {
    let old_len = line_count(old_string);
    let new_len = line_count(new_string);
    let old_range = diff_range(old_len);
    let new_range = diff_range(new_len);
    let mut out = format!("--- a/{path}\n+++ b/{path}\n@@ -{old_range} +{new_range} @@\n");
    append_diff_lines(&mut out, '-', old_string);
    append_diff_lines(&mut out, '+', new_string);
    out
}

fn unified_write_diff(path: &str, content: &str) -> String {
    let new_len = line_count(content);
    let new_range = diff_range(new_len);
    let mut out = format!("--- /dev/null\n+++ b/{path}\n@@ -0,0 +{new_range} @@\n");
    append_diff_lines(&mut out, '+', content);
    out
}

fn diff_range(lines: u64) -> String {
    match lines {
        0 => "0,0".to_string(),
        1 => "1".to_string(),
        _ => format!("1,{lines}"),
    }
}

fn append_diff_lines(out: &mut String, prefix: char, text: &str) {
    // `lines()` on empty text yields nothing, so nothing is appended.
    for line in text.lines() {
        out.push(prefix);
        out.push_str(line);
        out.push('\n');
    }
}

fn line_count(text: &str) -> u64 {
    if text.is_empty() {
        0
    } else {
        text.lines().count() as u64
    }
}

fn cursor_turn_structure_proto(user_id: &[u8; 32], step_ids: &[[u8; 32]]) -> Vec<u8> {
    let mut agent_turn = Vec::new();
    pb_len(&mut agent_turn, 1, user_id);
    for step_id in step_ids {
        pb_len(&mut agent_turn, 2, step_id);
    }

    let mut turn = Vec::new();
    pb_len(&mut turn, 1, &agent_turn);
    turn
}

fn cursor_root_state_proto(meta: &Meta, turn_ids: &[[u8; 32]]) -> Vec<u8> {
    let mut root = Vec::new();
    for turn_id in turn_ids {
        pb_len(&mut root, 8, turn_id);
    }
    pb_varint_field(&mut root, 10, 1);
    let started_ms = meta.timestamp.timestamp_millis();
    if let Ok(ms) = u64::try_from(started_ms)
        && ms > 0
    {
        pb_varint_field(&mut root, 26, ms);
    }
    root
}

fn cursor_message_id(session_id: &str, turn_idx: usize, text: &str) -> String {
    const NS: Uuid = Uuid::from_bytes([
        0x7a, 0x63, 0x24, 0x1e, 0x88, 0x0d, 0x4d, 0xc9, 0xa7, 0x99, 0x3b, 0x25, 0x9f, 0x64, 0x12,
        0x02,
    ]);
    Uuid::new_v5(&NS, format!("{session_id}:{turn_idx}:{text}").as_bytes()).to_string()
}

fn pb_string(out: &mut Vec<u8>, field: u32, value: &str) {
    if !value.is_empty() {
        pb_len(out, field, value.as_bytes());
    }
}

fn pb_len(out: &mut Vec<u8>, field: u32, bytes: &[u8]) {
    pb_key(out, field, 2);
    pb_varint(out, bytes.len() as u64);
    out.extend(bytes);
}

fn pb_varint_field(out: &mut Vec<u8>, field: u32, value: u64) {
    pb_key(out, field, 0);
    pb_varint(out, value);
}

fn pb_bool_field(out: &mut Vec<u8>, field: u32, value: bool) {
    pb_varint_field(out, field, u64::from(value));
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

fn parse_user_content(obj: &Value) -> Vec<Block> {
    match obj.get("content") {
        Some(Value::String(s)) => text_blocks_from_str(s, true),
        Some(Value::Array(arr)) => arr.iter().filter_map(parse_user_block).collect(),
        // Missing content or any other JSON shape carries no blocks.
        None | Some(_) => Vec::new(),
    }
}

fn parse_user_block(v: &Value) -> Option<Block> {
    match v.get("type").and_then(Value::as_str)? {
        "text" => {
            let text = strip_user_query(v.get("text")?.as_str()?);
            let text = clean_redacted_text(&text);
            (!text.is_empty()).then_some(Block::Text { text })
        }
        "image" => parse_image(v),
        // Unknown block types carry nothing representable.
        _ => None,
    }
}

fn parse_assistant_content(obj: &Value, session_id: &str, message_idx: usize) -> Vec<Block> {
    match obj.get("content") {
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .filter_map(|(i, v)| parse_assistant_block(v, session_id, message_idx, i))
            .collect(),
        // Missing content or any non-array shape carries no blocks.
        None | Some(_) => Vec::new(),
    }
}

fn parse_assistant_block(
    v: &Value,
    session_id: &str,
    message_idx: usize,
    block_idx: usize,
) -> Option<Block> {
    match v.get("type").and_then(Value::as_str)? {
        "text" => {
            let text = clean_redacted_text(v.get("text")?.as_str()?);
            (!text.is_empty()).then_some(Block::Text { text })
        }
        "reasoning" | "thinking" => Some(Block::Thinking {
            text: v
                .get("text")
                .or_else(|| v.get("thinking"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            signature: v.get("signature").and_then(Value::as_str).map(String::from),
            encrypted: v.get("data").and_then(Value::as_str).map(String::from),
        }),
        "redacted-reasoning" => Some(Block::Thinking {
            text: String::new(),
            signature: None,
            encrypted: v.get("data").and_then(Value::as_str).map(String::from),
        }),
        "tool-call" => {
            let name = v.get("toolName")?.as_str()?;
            let input = v.get("args").cloned().unwrap_or(Value::Object(Map::new()));
            let id = v.get("toolCallId").and_then(Value::as_str).map_or_else(
                || tool_use_id(session_id, message_idx, block_idx, name),
                String::from,
            );
            Some(Block::ToolUse {
                id,
                tool: cursor_tool(name, input),
            })
        }
        "image" => parse_image(v),
        // Unknown block types carry nothing representable.
        _ => None,
    }
}

fn parse_tool_content(obj: &Value) -> Vec<Block> {
    match obj.get("content") {
        Some(Value::Array(arr)) => arr
            .iter()
            // Only tool-result entries carry output.
            .filter(|v| v.get("type").and_then(Value::as_str) == Some("tool-result"))
            .filter_map(|v| {
                let tool_use_id = v.get("toolCallId")?.as_str()?.to_string();
                let content = match v.get("result") {
                    Some(Value::String(s)) => ToolOutput::Text(s.clone()),
                    Some(other) => ToolOutput::Json(other.clone()),
                    None => ToolOutput::Text(String::new()),
                };
                let is_error = v
                    .get("isError")
                    .or_else(|| obj.get("isError"))
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| matches!(&content, ToolOutput::Text(s) if looks_error(s)));
                Some(Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                })
            })
            .collect(),
        // Missing content or any non-array shape carries no blocks.
        None | Some(_) => Vec::new(),
    }
}

fn serialize_user_block(block: &Block) -> Value {
    match block {
        Block::Text { text } => json!({"type": "text", "text": wrap_user_query(text)}),
        Block::Image { source } => serialize_image(source),
        other => {
            let text = match other {
                Block::Thinking { text, .. } => text.clone(),
                Block::ToolUse { tool, .. } => format!("{tool:?}"),
                Block::ToolResult { content, .. } => tool_output_text(content),
                Block::Artifact { artifact } => artifact.display_text(),
                Block::Text { .. } | Block::Image { .. } => String::new(),
            };
            json!({"type": "text", "text": text})
        }
    }
}

fn serialize_assistant_block(
    block: &Block,
    tool_names: &mut HashMap<String, String>,
) -> Option<Value> {
    match block {
        Block::Text { text } => Some(json!({"type": "text", "text": text})),
        Block::Thinking {
            text,
            signature,
            encrypted,
        } => {
            if let Some(data) = encrypted {
                Some(json!({"type": "redacted-reasoning", "data": data}))
            } else {
                let mut obj = json!({"type": "reasoning", "text": text});
                if let Some(sig) = signature {
                    obj["signature"] = Value::String(sig.clone());
                }
                Some(obj)
            }
        }
        Block::ToolUse { id, tool } => {
            let (name, args) = denormalize_tool(tool);
            tool_names.insert(id.clone(), name.clone());
            Some(json!({
                "type": "tool-call",
                "toolCallId": id,
                "toolName": name,
                "args": args,
            }))
        }
        Block::Image { source } => Some(serialize_image(source)),
        Block::Artifact { artifact } => {
            Some(json!({"type": "text", "text": artifact.display_text()}))
        }
        // Cursor carries results in tool-role messages, not assistant blocks.
        Block::ToolResult { .. } => None,
    }
}

fn parse_image(v: &Value) -> Option<Block> {
    let source = v.get("source")?;
    Some(Block::Image {
        source: ImageSource {
            source_type: source
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("base64")
                .to_string(),
            media_type: source.get("media_type")?.as_str()?.to_string(),
            data: source.get("data")?.as_str()?.to_string(),
        },
    })
}

fn serialize_image(source: &ImageSource) -> Value {
    json!({
        "type": "image",
        "source": {
            "type": source.source_type,
            "media_type": source.media_type,
            "data": source.data,
        },
    })
}

// -- metadata ------------------------------------------------------------

fn meta_from_db(db: &CursorDb, db_path: Option<&Path>) -> Meta {
    let db_meta = cursor_meta_value(db);
    let session_meta = db.session_meta.as_ref();
    let created_ms = session_meta
        .and_then(|v| v.get("createdAtMs"))
        .and_then(Value::as_i64)
        .or_else(|| {
            db_meta
                .as_ref()
                .and_then(|v| v.get("createdAt"))
                .and_then(Value::as_i64)
        })
        .unwrap_or(0);
    let timestamp = DateTime::from_timestamp_millis(created_ms)
        .unwrap_or_else(|| DateTime::from_timestamp_millis(0).unwrap_or_else(Utc::now));

    let mut title = session_meta
        .and_then(|v| v.get("title"))
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| {
            db_meta
                .as_ref()
                .and_then(|v| v.get("name"))
                .and_then(Value::as_str)
                .map(String::from)
        });
    let mut cwd = db_meta
        .as_ref()
        .and_then(|v| v.get("workspacePath"))
        .and_then(Value::as_str)
        .map(String::from);
    let mut model = db_meta
        .as_ref()
        .and_then(|v| v.get("lastUsedModel"))
        .and_then(Value::as_str)
        .map(String::from);
    let mut first_user_text = None;

    // Non-JSON blobs carry Cursor's internal graph state, not messages.
    let json_blobs = db
        .blobs
        .iter()
        .filter_map(|blob| serde_json::from_slice::<Value>(&blob.data).ok());
    for obj in json_blobs {
        if cwd.is_none() {
            cwd = workspace_from_message(&obj);
        }
        if model.is_none() {
            model = cursor_model(&obj);
        }
        if first_user_text.is_none()
            && obj.get("role").and_then(Value::as_str) == Some("user")
            && !is_context_injection(&obj)
        {
            first_user_text = parse_user_content(&obj)
                .into_iter()
                .find_map(|block| match block {
                    Block::Text { text } if !text.is_empty() => Some(text),
                    // Only non-empty text can seed a title.
                    Block::Text { .. }
                    | Block::Thinking { .. }
                    | Block::ToolUse { .. }
                    | Block::ToolResult { .. }
                    | Block::Image { .. }
                    | Block::Artifact { .. } => None,
                });
        }
    }
    if title.as_deref().is_none_or(str::is_empty) {
        title = first_user_text.map(|s| truncate_title(&s));
    }

    Meta {
        id: db_path
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .or_else(|| {
                db_meta
                    .as_ref()
                    .and_then(|v| v.get("agentId"))
                    .and_then(Value::as_str)
                    .map(String::from)
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

fn ensure_cursor_meta(body: &mut CursorDb, meta: &Meta, id: &str) {
    let now_ms = Utc::now().timestamp_millis();
    let created_at = body
        .session_meta
        .as_ref()
        .and_then(|v| v.get("createdAtMs"))
        .and_then(Value::as_i64)
        .unwrap_or_else(|| {
            if meta.timestamp.timestamp_millis() > 0 {
                meta.timestamp.timestamp_millis()
            } else {
                now_ms
            }
        });
    let updated_at = body
        .session_meta
        .as_ref()
        .and_then(|v| v.get("updatedAtMs"))
        .and_then(Value::as_i64)
        .unwrap_or(now_ms);
    let title = meta
        .title
        .clone()
        .unwrap_or_else(|| "Imported Session".to_string());
    let latest = body.blobs.last().map(|b| b.id.clone()).unwrap_or_default();
    let model = meta
        .model
        .clone()
        .unwrap_or_else(|| "composer-2.5".to_string());
    let mut value = json!({
        "agentId": id,
        "latestRootBlobId": latest,
        "name": title,
        "createdAt": created_at,
        "mode": "default",
        "isRunEverything": true,
        "approvalMode": "unrestricted",
        "lastUsedModel": model,
    });
    if let Some(cwd) = meta.cwd.as_ref() {
        value["workspacePath"] = Value::String(cwd.clone());
    }
    let encoded = hex_encode(value.to_string().as_bytes());
    if let Some(entry) = body.meta.iter_mut().find(|entry| entry.key == "0") {
        entry.value = encoded;
    } else {
        body.meta.push(CursorMetaEntry {
            key: "0".to_string(),
            value: encoded,
        });
    }
    body.session_meta = Some(json!({
        "schemaVersion": 1,
        "createdAtMs": created_at,
        "hasConversation": !body.blobs.is_empty(),
        "title": title,
        "updatedAtMs": updated_at,
    }));
}

#[cfg(feature = "opencode")]
fn write_session_files(session_dir: &Path, body: &CursorDb, meta: &Meta, _id: &str) -> Result<()> {
    let session_meta = body.session_meta.clone().unwrap_or_else(|| {
        json!({
            "schemaVersion": 1,
            "createdAtMs": meta.timestamp.timestamp_millis(),
            "hasConversation": !body.blobs.is_empty(),
            "title": meta.title.clone().unwrap_or_default(),
            "updatedAtMs": Utc::now().timestamp_millis(),
        })
    });
    fs::write(
        session_dir.join("meta.json"),
        serde_json::to_string(&session_meta)?,
    )?;

    let prompts = db_to_messages(body, meta)
        .into_iter()
        .filter(|message| message.role == Role::User)
        .flat_map(|message| message.content)
        .filter_map(|block| match block {
            Block::Text { text } if !text.is_empty() => Some(text),
            // Only non-empty text blocks are prompts.
            Block::Text { .. }
            | Block::Thinking { .. }
            | Block::ToolUse { .. }
            | Block::ToolResult { .. }
            | Block::Image { .. }
            | Block::Artifact { .. } => None,
        })
        .collect::<Vec<_>>();
    fs::write(
        session_dir.join("prompt_history.json"),
        serde_json::to_string_pretty(&prompts)?,
    )?;
    Ok(())
}

fn cursor_meta_value(db: &CursorDb) -> Option<Value> {
    let raw = &db.meta.iter().find(|entry| entry.key == "0")?.value;
    // The value is stored either as plain JSON or as hex-encoded JSON.
    serde_json::from_str(raw)
        .ok()
        .or_else(|| serde_json::from_slice(&hex_decode(raw).ok()?).ok())
}

fn timestamp_from_obj(obj: &Value) -> Option<DateTime<Utc>> {
    obj.get("createdAt")
        .or_else(|| obj.get("timestamp"))
        .and_then(Value::as_i64)
        .and_then(DateTime::from_timestamp_millis)
        .or_else(|| {
            obj.get("timestamp")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<DateTime<Utc>>().ok())
        })
}

fn cursor_model(obj: &Value) -> Option<String> {
    obj.get("providerOptions")
        .and_then(|p| p.pointer("/cursor/modelName"))
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| {
            obj.get("content")
                .and_then(Value::as_array)
                .and_then(|arr| arr.iter().find_map(cursor_model))
        })
}

fn workspace_from_message(obj: &Value) -> Option<String> {
    content_strings(obj.get("content")).find_map(|s| {
        s.lines()
            .find_map(|line| line.strip_prefix("Workspace Path: ").map(String::from))
    })
}

fn is_context_injection(obj: &Value) -> bool {
    content_strings(obj.get("content")).any(|s| s.contains("<user_info>"))
}

fn content_strings(content: Option<&Value>) -> impl Iterator<Item = &str> {
    let strings: Vec<&str> = match content {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|block| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| block.get("result").and_then(Value::as_str))
            })
            .collect(),
        // Missing content or any other JSON shape has no text to scan.
        None | Some(_) => Vec::new(),
    };
    strings.into_iter()
}

// -- tool normalization --------------------------------------------------

fn cursor_tool(name: &str, args: Value) -> Tool {
    let canonical = match name {
        "Shell" => "Bash",
        "StrReplace" => "Edit",
        other => other,
    };
    Tool::from_canonical(canonical, normalize_cursor_args(canonical, args))
}

fn normalize_cursor_args(tool: &str, args: Value) -> Value {
    match args {
        Value::Object(obj) => Value::Object(
            obj.into_iter()
                .map(|(k, v)| {
                    let key = match (tool, k.as_str()) {
                        (_, "path" | "filePath") => "file_path",
                        ("Bash", "cwd") => "workdir",
                        ("Edit", "oldString") => "old_string",
                        ("Edit", "newString") => "new_string",
                        // Keys the formats agree on pass through unchanged.
                        _ => k.as_str(),
                    }
                    .to_string();
                    (key, v)
                })
                .collect(),
        ),
        // Non-object args have no keys to rename.
        other => other,
    }
}

fn denormalize_tool(tool: &Tool) -> (String, Value) {
    let (name, input) = tool.to_canonical();
    let cursor_name = match name.as_str() {
        "Bash" => "Shell",
        "Edit" => "StrReplace",
        other => other,
    }
    .to_string();
    (cursor_name, denormalize_cursor_args(&name, input))
}

fn denormalize_cursor_args(tool: &str, input: Value) -> Value {
    match input {
        Value::Object(obj) => Value::Object(
            obj.into_iter()
                .map(|(k, v)| {
                    let key = match (tool, k.as_str()) {
                        (_, "file_path") => "path",
                        ("Bash", "workdir") => "cwd",
                        ("Edit", "old_string") => "oldString",
                        ("Edit", "new_string") => "newString",
                        // Keys the formats agree on pass through unchanged.
                        _ => k.as_str(),
                    }
                    .to_string();
                    (key, v)
                })
                .collect(),
        ),
        // Non-object args have no keys to rename.
        other => other,
    }
}

// -- text helpers --------------------------------------------------------

fn text_blocks_from_str(s: &str, strip_query: bool) -> Vec<Block> {
    let text = if strip_query {
        strip_user_query(s)
    } else {
        s.to_string()
    };
    let text = clean_redacted_text(&text);
    if text.is_empty() {
        Vec::new()
    } else {
        vec![Block::Text { text }]
    }
}

fn strip_user_query(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(inner) = trimmed
        .strip_prefix("<user_query>")
        .and_then(|s| s.strip_suffix("</user_query>"))
    {
        inner.trim().to_string()
    } else {
        text.to_string()
    }
}

fn wrap_user_query(text: &str) -> String {
    if text.trim_start().starts_with("<user_query>") {
        text.to_string()
    } else {
        format!("<user_query>\n{text}\n</user_query>")
    }
}

fn clean_redacted_text(text: &str) -> String {
    text.replace("\n\n[REDACTED]", "")
        .replace("[REDACTED]\n\n", "")
        .replace("[REDACTED]", "")
        .trim()
        .to_string()
}

fn looks_error(text: &str) -> bool {
    text.starts_with("Error:") || text.contains(" exited with code ")
}

fn tool_output_text(out: &ToolOutput) -> String {
    match out {
        ToolOutput::Text(s) => s.clone(),
        ToolOutput::Json(v) => v.to_string(),
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

fn tool_use_id(session_id: &str, message_idx: usize, block_idx: usize, name: &str) -> String {
    const NS: Uuid = Uuid::from_bytes([
        0x3c, 0x7a, 0x1b, 0x44, 0x9e, 0x2f, 0x4d, 0x68, 0xa1, 0x0c, 0x55, 0x6e, 0x7f, 0x88, 0x99,
        0xaa,
    ]);
    Uuid::new_v5(
        &NS,
        format!("{session_id}:{message_idx}:{block_idx}:{name}").as_bytes(),
    )
    .to_string()
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

fn dirs_home() -> Option<PathBuf> {
    super::home_dir()
}

// -- hashes and hex ------------------------------------------------------

// MD5 implementation follows RFC 1321 naming and table generation.
#[allow(
    clippy::many_single_char_names,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
#[cfg(any(feature = "opencode", test))]
fn md5_hex(data: &[u8]) -> String {
    let mut msg = data.to_vec();
    let bit_len = (msg.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend(bit_len.to_le_bytes());

    let mut a0 = 0x6745_2301u32;
    let mut b0 = 0xefcd_ab89u32;
    let mut c0 = 0x98ba_dcfeu32;
    let mut d0 = 0x1032_5476u32;
    let shifts: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let mut k = [0u32; 64];
    for (i, slot) in k.iter_mut().enumerate() {
        *slot = ((f64::sin((i + 1) as f64).abs() * 4_294_967_296.0).floor()) as u32;
    }

    for chunk in msg.as_chunks::<64>().0 {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            let start = i * 4;
            *word = u32::from_le_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(k[i])
                    .wrapping_add(m[g])
                    .rotate_left(shifts[i]),
            );
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = Vec::with_capacity(16);
    out.extend(a0.to_le_bytes());
    out.extend(b0.to_le_bytes());
    out.extend(c0.to_le_bytes());
    out.extend(d0.to_le_bytes());
    hex_encode(&out)
}

fn sha256_hex(data: &[u8]) -> String {
    hex_encode(&sha256(data))
}

// FIPS 180-4's own variable naming; the constant tables dominate the length.
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut h = [
        0x6a09_e667u32,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (msg.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend(bit_len.to_be_bytes());

    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            let start = i * 4;
            *word = u32::from_be_bytes([
                chunk[start],
                chunk[start + 1],
                chunk[start + 2],
                chunk[start + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

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
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[hi, lo]| Ok((hex_value(*hi)? << 4) | hex_value(*lo)?))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_match_cursor_paths_and_blob_ids() {
        assert_eq!(
            md5_hex(b"/Users/nishantjoshi/Workspace/10-Work-Projects/10.03-Replay/transcript"),
            "bee2c84d4cc659714c766b9d0e4af911"
        );
        assert_eq!(
            sha256_hex(br#"{"role":"user","content":"hello"}"#),
            "b5344ebd07750817371cf4670e556b15a4babb975bde955ff4ce24bca09731ef"
        );
    }
}
