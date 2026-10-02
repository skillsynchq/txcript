//! uji sessions: one `SQLite` database, `~/.local/share/uji/uji.db`.
//!
//! uji keeps a `sessions` row per session and a `messages` row per transcript
//! entry, whose `data` column holds the entry as JSON. The native body is the
//! session row with its message rows nested in a `messages` array, each row's
//! `data` decoded; that JSON object is the harness's portable text form.
//!
//! The codec reads `user`, `context`, `assistant`, `tool`, and `compaction`
//! entries. Assistant reasoning survives through [`Common`] as
//! [`Block::Thinking`], and uji's provider replay data rides along: Anthropic
//! thinking signatures as `signature`, redacted thinking and Responses
//! reasoning items as `encrypted`. `shell`, `error`, and `system` entries stay
//! in the native body without a conversational turn.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

#[cfg(feature = "uji")]
use rusqlite::{Connection, OpenFlags, OptionalExtension};
#[cfg(feature = "uji")]
use std::collections::HashMap;
use std::collections::HashSet;
#[cfg(feature = "uji")]
use std::path::Path;
use std::path::PathBuf;

use crate::common::{Block, ImageSource, Lineage, Message, Meta, Relation, Role, Tool, ToolOutput};
#[cfg(feature = "uji")]
use crate::error::Error;
use crate::error::Result;
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

/// The title uji gives a session before it names one.
const UNTITLED: &str = "untitled";

/// The uji harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Uji;

impl Harness for Uji {
    const NAME: &'static str = "uji";
    type Body = Value;
}

impl TextCodec for Uji {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let body: Value = serde_json::from_str(text)?;
        Ok(Transcript::new(meta_from_body(&body), body))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Ok(serde_json::to_string_pretty(&transcript.body)?)
    }
}

impl Codec for Uji {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            messages_from_body(&transcript.body, &transcript.meta),
        ))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            body_from_messages(&transcript.meta, &transcript.body),
        ))
    }
}

/// uji's session database.
#[derive(Debug, Clone)]
pub struct UjiStore {
    pub db_path: PathBuf,
}

impl UjiStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: path.into(),
        }
    }

    /// Resolve the database the way uji does: `$UJI_DB`, else `uji.db` in
    /// `$UJI_DATA_DIR`, else in `$XDG_DATA_HOME/uji` when that is absolute,
    /// else in `~/.local/share/uji`.
    #[must_use]
    pub fn default_db() -> Option<Self> {
        let set = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        if let Some(db) = set("UJI_DB") {
            return Some(Self::new(db));
        }
        set("UJI_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                set("XDG_DATA_HOME")
                    .map(PathBuf::from)
                    .filter(|base| base.is_absolute())
                    .map(|base| base.join("uji"))
            })
            .or_else(|| super::home_dir().map(|home| home.join(".local/share/uji")))
            .map(|dir| Self::new(dir.join("uji.db")))
    }
}

#[cfg(feature = "uji")]
impl Store for UjiStore {
    type H = Uji;
    type Ref = String;

    fn discover(&self) -> Result<Vec<Discovered<String>>> {
        if !self.db_path.is_file() {
            return Ok(Vec::new());
        }
        let conn = open_read_only(&self.db_path)?;
        let rows = query_rows(&conn, "SELECT * FROM sessions", &[])?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let meta = meta_from_body(&Value::Object(row));
                (!meta.id.is_empty()).then(|| Discovered {
                    reference: meta.id.clone(),
                    meta,
                })
            })
            .collect())
    }

    fn load(&self, reference: &String) -> Result<Transcript<Uji>> {
        let conn = open_read_only(&self.db_path)?;
        let mut session = query_rows(
            &conn,
            "SELECT * FROM sessions WHERE id = ?1",
            &[reference.as_str()],
        )?
        .pop()
        .ok_or_else(|| not_found(reference, &self.db_path))?;
        let rows = query_rows(
            &conn,
            "SELECT id, seq, time_created, data FROM messages WHERE session_id = ?1 ORDER BY seq",
            &[reference.as_str()],
        )?;
        let messages = rows
            .into_iter()
            .map(|mut row| {
                if let Some(Value::String(data)) = row.get("data")
                    && let Ok(decoded) = serde_json::from_str::<Value>(data)
                {
                    row.insert("data".to_string(), decoded);
                }
                Value::Object(row)
            })
            .collect();
        session.insert("messages".to_string(), Value::Array(messages));
        let body = Value::Object(session);
        Ok(Transcript::new(meta_from_body(&body), body))
    }

    fn save(&self, transcript: &Transcript<Uji>) -> Result<Saved<String>> {
        let mut conn = open_writable(&self.db_path)?;
        let id = session_id(transcript);
        let body = &transcript.body;
        let created = body
            .get("time_created")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| transcript.meta.timestamp.timestamp_millis());
        let updated = body
            .get("time_updated")
            .and_then(Value::as_i64)
            .unwrap_or(created);
        let title = body
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.is_empty())
            .unwrap_or(UNTITLED);
        let directory = body.get("directory").and_then(Value::as_str).unwrap_or("");
        let parent = body.get("parent").and_then(Value::as_str);
        let tx = conn.transaction().map_err(sqlite_error)?;
        let parent = match parent {
            Some(parent) if session_exists(&tx, parent)? => Some(parent),
            _ => None,
        };
        tx.execute("DELETE FROM messages WHERE session_id = ?1", [&id])
            .map_err(sqlite_error)?;
        tx.execute("DELETE FROM sessions WHERE id = ?1", [&id])
            .map_err(sqlite_error)?;
        if let Some(parent) = parent {
            tx.execute(
                "INSERT INTO sessions (id, title, directory, parent, time_created, time_updated) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![id, title, directory, parent, created, updated],
            )
        } else {
            tx.execute(
                "INSERT INTO sessions (id, title, directory, time_created, time_updated) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, title, directory, created, updated],
            )
        }
        .map_err(sqlite_error)?;
        let rows = body
            .get("messages")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        let mut next_seq = 0i64;
        for row in rows {
            let seq = row
                .get("seq")
                .and_then(Value::as_i64)
                .filter(|seq| *seq > next_seq)
                .unwrap_or(next_seq + 1);
            next_seq = seq;
            let data = row.get("data").cloned().unwrap_or(Value::Null);
            let kind = data
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("system")
                .to_string();
            let message_id = row
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map_or_else(|| uuid::Uuid::new_v4().to_string(), String::from);
            let time = row
                .get("time_created")
                .and_then(Value::as_i64)
                .unwrap_or(created);
            let data = match data {
                Value::String(raw) => raw,
                other => serde_json::to_string(&other)?,
            };
            tx.execute(
                "INSERT INTO messages (id, session_id, seq, type, time_created, data) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![message_id, id, seq, kind, time, data],
            )
            .map_err(sqlite_error)?;
        }
        tx.commit().map_err(sqlite_error)?;
        Ok(Saved {
            reference: id.clone(),
            id,
        })
    }

    fn delete(&self, reference: &String) -> Result<()> {
        let mut conn = open_writable(&self.db_path)?;
        let tx = conn.transaction().map_err(sqlite_error)?;
        tx.execute("DELETE FROM messages WHERE session_id = ?1", [reference])
            .map_err(sqlite_error)?;
        let removed = tx
            .execute("DELETE FROM sessions WHERE id = ?1", [reference])
            .map_err(sqlite_error)?;
        if removed == 0 {
            return Err(not_found(reference, &self.db_path));
        }
        tx.commit().map_err(sqlite_error)
    }

    fn fingerprints(&self, refs: &[String]) -> Result<HashMap<String, String>> {
        let mut output = HashMap::with_capacity(refs.len());
        if !self.db_path.is_file() {
            return Ok(output);
        }
        let conn = open_read_only(&self.db_path)?;
        let mut statement = conn
            .prepare(
                "SELECT s.time_updated, \
                 (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id), \
                 (SELECT COALESCE(MAX(seq), 0) FROM messages m WHERE m.session_id = s.id) \
                 FROM sessions s WHERE s.id = ?1",
            )
            .map_err(sqlite_error)?;
        for reference in refs {
            let cursor = statement
                .query_row([reference], |row| {
                    Ok(format!(
                        "{}:{}:{}",
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?
                    ))
                })
                .optional()
                .map_err(sqlite_error)?
                .unwrap_or_default();
            output.insert(reference.clone(), cursor);
        }
        Ok(output)
    }
}

#[cfg(not(feature = "uji"))]
impl Store for UjiStore {
    type H = Uji;
    type Ref = String;

    fn discover(&self) -> Result<Vec<Discovered<String>>> {
        Ok(Vec::new())
    }

    fn load(&self, _reference: &String) -> Result<Transcript<Uji>> {
        Err(sqlite_unavailable())
    }

    fn save(&self, _transcript: &Transcript<Uji>) -> Result<Saved<String>> {
        Err(sqlite_unavailable())
    }

    fn delete(&self, _reference: &String) -> Result<()> {
        Err(sqlite_unavailable())
    }
}

#[cfg(not(feature = "uji"))]
fn sqlite_unavailable() -> crate::error::Error {
    crate::error::Error::Unconvertible {
        harness: Uji::NAME,
        detail: "uji store support requires the `uji` feature for SQLite".to_string(),
    }
}

#[cfg(feature = "uji")]
fn open_read_only(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlite_error)
}

/// Open uji's database for writing. Only uji creates and migrates it, so a
/// database that does not exist yet is an error rather than one to create.
#[cfg(feature = "uji")]
fn open_writable(path: &Path) -> Result<Connection> {
    if !path.is_file() {
        return Err(Error::Unconvertible {
            harness: Uji::NAME,
            detail: format!(
                "no uji database at {}; start uji once to create it",
                path.display()
            ),
        });
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlite_error)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(sqlite_error)?;
    conn.execute_batch("PRAGMA foreign_keys = ON")
        .map_err(sqlite_error)?;
    Ok(conn)
}

#[cfg(feature = "uji")]
fn session_exists(conn: &Connection, id: &str) -> Result<bool> {
    let mut statement = conn
        .prepare("SELECT 1 FROM sessions WHERE id = ?1")
        .map_err(sqlite_error)?;
    statement.exists([id]).map_err(sqlite_error)
}

#[cfg(feature = "uji")]
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
            let value = match row.get_ref(index).map_err(sqlite_error)? {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(number) => json!(number),
                rusqlite::types::ValueRef::Real(number) => json!(number),
                rusqlite::types::ValueRef::Text(bytes) | rusqlite::types::ValueRef::Blob(bytes) => {
                    Value::String(String::from_utf8_lossy(bytes).into_owned())
                }
            };
            object.insert(name.clone(), value);
        }
        output.push(object);
    }
    Ok(output)
}

/// The id to save under. `uji resume --id` takes only UUID-shaped ids, so any
/// other id is replaced with a fresh one.
#[cfg(feature = "uji")]
fn session_id(transcript: &Transcript<Uji>) -> String {
    let id = transcript
        .body
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .unwrap_or(&transcript.meta.id);
    if uuid::Uuid::try_parse(id).is_ok() {
        id.to_string()
    } else {
        uuid::Uuid::new_v4().to_string()
    }
}

#[cfg(feature = "uji")]
fn not_found(reference: &str, db: &Path) -> Error {
    Error::Malformed {
        harness: Uji::NAME,
        detail: format!("session `{reference}` not found in {}", db.display()),
    }
}

#[cfg(feature = "uji")]
#[allow(clippy::needless_pass_by_value)]
fn sqlite_error(error: rusqlite::Error) -> Error {
    Error::Malformed {
        harness: Uji::NAME,
        detail: error.to_string(),
    }
}

fn millis(value: Option<&Value>) -> Option<DateTime<Utc>> {
    value
        .and_then(Value::as_i64)
        .and_then(DateTime::from_timestamp_millis)
}

fn meta_from_body(body: &Value) -> Meta {
    let string = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    Meta {
        id: string("id").unwrap_or_default(),
        timestamp: millis(body.get("time_created")).unwrap_or(DateTime::<Utc>::UNIX_EPOCH),
        cwd: string("directory"),
        git_branch: None,
        title: string("title").filter(|title| title != UNTITLED),
        cli_version: None,
        model: None,
        lineage: string("parent").map(|parent| Lineage {
            parent,
            relation: Relation::Spawn,
        }),
    }
}

fn messages_from_body(body: &Value, meta: &Meta) -> Vec<Message> {
    body.get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let timestamp = millis(row.get("time_created")).unwrap_or(meta.timestamp);
            message_from_entry(row.get("data")?, timestamp)
        })
        .collect()
}

fn text_of(entry: &Value, key: &str) -> String {
    entry
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn user(content: Vec<Block>, timestamp: DateTime<Utc>) -> Option<Message> {
    (!content.is_empty()).then_some(Message {
        role: Role::User,
        content,
        timestamp,
        model: None,
        stop_reason: None,
        usage: None,
    })
}

fn message_from_entry(entry: &Value, timestamp: DateTime<Utc>) -> Option<Message> {
    match entry.get("type").and_then(Value::as_str)? {
        "user" | "context" => {
            let mut content = Vec::new();
            let text = text_of(entry, "text");
            if !text.is_empty() {
                content.push(Block::Text { text });
            }
            content.extend(images(entry));
            user(content, timestamp)
        }
        "compaction" => {
            let summary = text_of(entry, "summary");
            user(
                if summary.is_empty() {
                    Vec::new()
                } else {
                    vec![Block::Text { text: summary }]
                },
                timestamp,
            )
        }
        "tool" => {
            let content = text_of(entry, "content");
            let mut blocks = vec![Block::ToolResult {
                tool_use_id: text_of(entry, "tool_call_id"),
                is_error: content.starts_with("error:") || content.starts_with("denied:"),
                content: ToolOutput::Text(content),
            }];
            blocks.extend(images(entry));
            user(blocks, timestamp)
        }
        "assistant" => assistant_message(entry, timestamp),
        _ => None,
    }
}

fn images(entry: &Value) -> impl Iterator<Item = Block> + '_ {
    entry
        .get("images")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|image| {
            Some(Block::Image {
                source: ImageSource {
                    source_type: "base64".to_string(),
                    media_type: image.get("media_type")?.as_str()?.to_string(),
                    data: image.get("data")?.as_str()?.to_string(),
                },
            })
        })
}

fn assistant_message(entry: &Value, timestamp: DateTime<Utc>) -> Option<Message> {
    let replay = entry.get("replay");
    let api = replay.and_then(|r| r.get("api")).and_then(Value::as_str);
    let mut content = match api {
        Some("anthropic") => anthropic_thinking(replay),
        Some("responses") => responses_thinking(replay),
        _ => Vec::new(),
    };
    if content.is_empty() {
        let reasoning = text_of(entry, "reasoning");
        if !reasoning.is_empty() {
            content.push(Block::Thinking {
                text: reasoning,
                signature: None,
                encrypted: None,
            });
        }
    }
    let text = text_of(entry, "text");
    if !text.is_empty() {
        content.push(Block::Text { text });
    }
    content.extend(
        entry
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(tool_use),
    );
    if content.is_empty() {
        return None;
    }
    Some(Message {
        role: Role::Assistant,
        content,
        timestamp,
        model: replay
            .and_then(|r| r.get("model"))
            .and_then(Value::as_str)
            .filter(|model| !model.is_empty())
            .map(String::from),
        stop_reason: None,
        usage: None,
    })
}

fn anthropic_thinking(replay: Option<&Value>) -> Vec<Block> {
    replay
        .and_then(|r| r.get("content"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| match block.get("type").and_then(Value::as_str)? {
            "thinking" => Some(Block::Thinking {
                text: text_of(block, "thinking"),
                signature: block
                    .get("signature")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(String::from),
                encrypted: None,
            }),
            "redacted_thinking" => Some(Block::Thinking {
                text: String::new(),
                signature: None,
                encrypted: block.get("data").and_then(Value::as_str).map(String::from),
            }),
            _ => None,
        })
        .collect()
}

fn responses_thinking(replay: Option<&Value>) -> Vec<Block> {
    replay
        .and_then(|r| r.get("items"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("kind").and_then(Value::as_str) == Some("reasoning"))
        .filter_map(|entry| {
            let item = entry.get("item")?;
            let text = item
                .get("summary")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n");
            Some(Block::Thinking {
                text,
                signature: None,
                encrypted: Some(item.to_string()),
            })
        })
        .collect()
}

fn tool_use(call: &Value) -> Option<Block> {
    let name = call.get("name").and_then(Value::as_str)?;
    let raw = call
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    let input = if raw.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
    };
    let (name, input) = canonical_tool(name, input);
    Some(Block::ToolUse {
        id: text_of(call, "id"),
        tool: Tool::from_canonical(&name, input),
    })
}

/// uji's built-in tools and the canonical tools they correspond to.
const TOOLS: [(&str, &str); 4] = [
    ("read_file", "Read"),
    ("write_file", "Write"),
    ("edit_file", "Edit"),
    ("run_command", "Bash"),
];

fn canonical_tool(name: &str, input: Value) -> (String, Value) {
    let Some(canonical) = TOOLS
        .iter()
        .find(|(native, _)| *native == name)
        .map(|(_, canonical)| *canonical)
    else {
        return (name.to_string(), input);
    };
    let Value::Object(mut object) = input else {
        return (name.to_string(), input);
    };
    if canonical == "Bash" {
        if let Some(seconds) = object.get("timeout").and_then(Value::as_u64) {
            object.remove("timeout");
            object.insert(
                "timeout_ms".to_string(),
                json!(seconds.saturating_mul(1000)),
            );
        }
    } else {
        rename_key(&mut object, "path", "file_path");
    }
    (canonical.to_string(), Value::Object(object))
}

fn native_tool(tool: &Tool) -> (String, Value) {
    let (name, input) = tool.to_canonical();
    let Some(native) = TOOLS
        .iter()
        .find(|(_, canonical)| *canonical == name)
        .map(|(native, _)| *native)
    else {
        return (name, input);
    };
    let Value::Object(mut object) = input else {
        return (name, input);
    };
    if native == "run_command" {
        if let Some(ms) = object.get("timeout_ms").and_then(Value::as_u64)
            && ms % 1000 == 0
        {
            object.remove("timeout_ms");
            object.insert("timeout".to_string(), json!(ms / 1000));
        }
    } else {
        rename_key(&mut object, "file_path", "path");
    }
    (native.to_string(), Value::Object(object))
}

fn rename_key(object: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = object.remove(from) {
        object.insert(to.to_string(), value);
    }
}

fn body_from_messages(meta: &Meta, messages: &[Message]) -> Value {
    let mut names = std::collections::HashMap::new();
    let mut commands = HashSet::new();
    let mut rows = Vec::new();
    for message in messages {
        let time = message.timestamp.timestamp_millis();
        let entries = match message.role {
            Role::Assistant => assistant_entry(meta, message, &mut names)
                .into_iter()
                .collect(),
            Role::User => user_entries(message, &names, &mut commands),
        };
        for data in entries {
            rows.push(json!({ "seq": rows.len() + 1, "time_created": time, "data": data }));
        }
    }
    let created = meta.timestamp.timestamp_millis();
    let updated = messages
        .iter()
        .map(|message| message.timestamp.timestamp_millis())
        .max()
        .unwrap_or(created)
        .max(created);
    let mut body = json!({
        "id": meta.id,
        "title": meta.title.clone().unwrap_or_else(|| UNTITLED.to_string()),
        "directory": meta.cwd.clone().unwrap_or_default(),
        "time_created": created,
        "time_updated": updated,
        "messages": rows,
    });
    if let Some(lineage) = meta
        .lineage
        .as_ref()
        .filter(|lineage| lineage.relation == Relation::Spawn)
    {
        body["parent"] = Value::String(lineage.parent.clone());
    }
    body
}

fn assistant_entry(
    meta: &Meta,
    message: &Message,
    names: &mut std::collections::HashMap<String, String>,
) -> Option<Value> {
    let mut text = Vec::new();
    let mut reasoning = Vec::new();
    let mut calls = Vec::new();
    for block in &message.content {
        match block {
            Block::Text { text: part } => text.push(part.clone()),
            Block::Artifact { artifact } => text.push(artifact.display_text()),
            Block::Thinking { text: part, .. } if !part.is_empty() => reasoning.push(part.clone()),
            Block::ToolUse { id, tool } => {
                let (name, input) = native_tool(tool);
                names.insert(id.clone(), name.clone());
                let arguments = match input {
                    Value::String(raw) => raw,
                    other => other.to_string(),
                };
                calls.push(json!({ "id": id, "name": name, "arguments": arguments }));
            }
            _ => {}
        }
    }
    if text.is_empty() && reasoning.is_empty() && calls.is_empty() {
        return None;
    }
    let mut entry = json!({ "type": "assistant", "text": text.join("\n\n"), "tool_calls": calls });
    if !reasoning.is_empty() {
        entry["reasoning"] = Value::String(reasoning.join("\n\n"));
    }
    let model = message.model.as_ref().or(meta.model.as_ref());
    if let Some(replay) = model.and_then(|model| replay(message, model)) {
        entry["replay"] = replay;
    }
    Some(entry)
}

/// Rebuild uji's provider replay from thinking blocks that carry one: signed
/// Anthropic thinking, or Responses reasoning items kept in `encrypted`.
fn replay(message: &Message, model: &str) -> Option<Value> {
    let signed = message.content.iter().any(|block| {
        matches!(
            block,
            Block::Thinking {
                signature: Some(_),
                ..
            }
        )
    });
    if signed {
        let content: Vec<Value> = message
            .content
            .iter()
            .filter_map(|block| match block {
                Block::Thinking {
                    text,
                    signature: Some(signature),
                    ..
                } => Some(json!({ "type": "thinking", "thinking": text, "signature": signature })),
                Block::Thinking {
                    encrypted: Some(data),
                    ..
                } => Some(json!({ "type": "redacted_thinking", "data": data })),
                Block::Text { text } => Some(json!({ "type": "text", "text": text })),
                Block::ToolUse { .. } => Some(json!({ "type": "tool_use" })),
                _ => None,
            })
            .collect();
        return Some(json!({ "api": "anthropic", "model": model, "content": content }));
    }
    let mut items = Vec::new();
    for block in &message.content {
        match block {
            Block::Thinking {
                encrypted: Some(raw),
                ..
            } => {
                let item = serde_json::from_str::<Value>(raw)
                    .ok()
                    .filter(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))?;
                items.push(json!({ "kind": "reasoning", "item": item }));
            }
            Block::Text { .. } => items.push(json!({ "kind": "message" })),
            Block::ToolUse { .. } => items.push(json!({ "kind": "function_call" })),
            _ => {}
        }
    }
    items
        .iter()
        .any(|entry| entry["kind"] == "reasoning")
        .then(|| json!({ "api": "responses", "model": model, "items": items }))
}

fn user_entries(
    message: &Message,
    names: &std::collections::HashMap<String, String>,
    commands: &mut HashSet<String>,
) -> Vec<Value> {
    let mut entries: Vec<Value> = Vec::new();
    let mut pending: Option<Value> = None;
    let flush = |pending: &mut Option<Value>, entries: &mut Vec<Value>| {
        if let Some(entry) = pending.take() {
            entries.push(entry);
        }
    };
    for block in &message.content {
        match block {
            Block::Text { text } => {
                let entry = match pending.take() {
                    Some(entry) if entry["type"] == "user" => entry,
                    other => {
                        if let Some(entry) = other {
                            entries.push(entry);
                        }
                        json!({ "type": "user", "text": "" })
                    }
                };
                let mut entry = entry;
                let joined = match entry["text"].as_str().unwrap_or_default() {
                    "" => text.clone(),
                    before => format!("{before}\n\n{text}"),
                };
                entry["text"] = Value::String(joined);
                pending = Some(entry);
            }
            Block::Artifact { artifact } => {
                flush(&mut pending, &mut entries);
                entries.push(json!({ "type": "user", "text": artifact.display_text() }));
            }
            Block::Image { source } => {
                let image = json!({
                    "media_type": source.media_type,
                    "data": source.data,
                    "name": format!("image.{}", extension(&source.media_type)),
                });
                let target = pending.get_or_insert_with(|| json!({ "type": "user", "text": "" }));
                match target["images"].as_array_mut() {
                    Some(list) => list.push(image),
                    None => target["images"] = json!([image]),
                }
            }
            Block::ToolUse {
                id,
                tool: Tool::Command { .. },
            } => {
                commands.insert(id.clone());
            }
            Block::ToolResult {
                tool_use_id,
                content,
                ..
            } if !commands.contains(tool_use_id) => {
                flush(&mut pending, &mut entries);
                let text = match content {
                    ToolOutput::Text(text) => text.clone(),
                    ToolOutput::Json(value) => value.to_string(),
                };
                pending = Some(json!({
                    "type": "tool",
                    "tool_call_id": tool_use_id,
                    "name": names.get(tool_use_id).cloned().unwrap_or_default(),
                    "content": text,
                }));
            }
            _ => {}
        }
    }
    flush(&mut pending, &mut entries);
    entries
}

fn extension(media_type: &str) -> &str {
    match media_type {
        "image/jpeg" => "jpg",
        other => other.strip_prefix("image/").unwrap_or("png"),
    }
}
