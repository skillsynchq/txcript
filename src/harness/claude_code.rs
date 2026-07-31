//! Claude Code: `~/.claude/projects/<encoded-cwd>/<session>.jsonl`.
//!
//! Claude's on-disk payload is already the Anthropic Messages shape, so the
//! codec is close to the identity — it is the reference every other harness
//! normalizes toward. Each line is one [`Record`]; user/assistant lines carry
//! an [`ApiMessage`] whose `content` is a string or an array of Anthropic
//! blocks. Non-message lines (summaries, titles, snapshots) are preserved
//! verbatim in [`Record::Other`] so native ↔ disk stays lossless even though
//! the codec ignores them.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::common::{Block, ImageSource, Message, Meta, Role, StopReason, Tool, ToolOutput, Usage};
use crate::error::{Error, Result};
use crate::harness::jsonl;
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

/// The Claude Code harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaudeCode;

impl Harness for ClaudeCode {
    const NAME: &'static str = "claude_code";
    type Body = Vec<Record>;
}

// ── native records ─────────────────────────────────────────────────────

/// One JSONL line. Known line types are typed; anything else is kept whole in
/// [`Record::Other`] so the file round-trips without loss.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Summary(SummaryLine),
    User(EntryLine),
    Assistant(EntryLine),
    Other(Value),
}

/// A `user` or `assistant` line: the per-line envelope plus the wrapped
/// Anthropic message. Unknown envelope keys collect in `extra`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryLine {
    #[serde(
        rename = "parentUuid",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_uuid: Option<String>,
    pub uuid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(rename = "sessionId", default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(rename = "gitBranch", default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub message: ApiMessage,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A `summary` line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryLine {
    pub summary: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The wrapped Anthropic message. `content` is preserved as raw JSON (string or
/// block array); the codec is what turns it into typed [`Block`]s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub content: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// Classify by type; malformed known records fall back to Other.
impl From<Value> for Record {
    fn from(v: Value) -> Self {
        match v.get("type").and_then(Value::as_str) {
            Some("summary") => SummaryLine::deserialize(&v)
                .map(Record::Summary)
                .unwrap_or(Record::Other(v)),
            Some("user") => EntryLine::deserialize(&v)
                .map(Record::User)
                .unwrap_or(Record::Other(v)),
            Some("assistant") => EntryLine::deserialize(&v)
                .map(Record::Assistant)
                .unwrap_or(Record::Other(v)),
            _ => Record::Other(v),
        }
    }
}

impl From<Record> for Value {
    fn from(r: Record) -> Self {
        fn tagged(line: impl Serialize, ty: &str) -> Value {
            let mut v = serde_json::to_value(line).unwrap_or(Value::Null);
            if let Value::Object(obj) = &mut v {
                obj.insert("type".into(), Value::String(ty.into()));
            }
            v
        }
        match r {
            Record::Summary(s) => tagged(s, "summary"),
            Record::User(e) => tagged(e, "user"),
            Record::Assistant(e) => tagged(e, "assistant"),
            Record::Other(v) => v,
        }
    }
}

impl Serialize for Record {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        Value::from(self.clone()).serialize(s)
    }
}

impl<'de> Deserialize<'de> for Record {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        Ok(Record::from(Value::deserialize(d)?))
    }
}

// ── codec ──────────────────────────────────────────────────────────────

impl Codec for ClaudeCode {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        let fallback_ts = transcript.meta.timestamp;
        let messages = transcript
            .body
            .iter()
            .filter_map(|record| match record {
                Record::User(e) => Some((Role::User, e)),
                Record::Assistant(e) => Some((Role::Assistant, e)),
                // Summaries, titles, snapshots carry no conversational turn.
                Record::Summary(_) | Record::Other(_) => None,
            })
            .filter_map(|(role, entry)| {
                let content = parse_blocks(&entry.message.content);
                // An entry whose blocks parse to nothing carries no turn
                // either.
                (!content.is_empty()).then(|| Message {
                    role,
                    content,
                    timestamp: entry
                        .timestamp
                        .as_deref()
                        .and_then(parse_ts)
                        .unwrap_or(fallback_ts),
                    model: entry.message.model.clone(),
                    stop_reason: entry.message.stop_reason.as_deref().map(parse_stop_reason),
                    usage: entry.message.usage.as_ref().and_then(parse_usage),
                })
            })
            .collect();
        Ok(Transcript::new(transcript.meta.clone(), messages))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        let meta = &transcript.meta;
        let session_id = if meta.id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            meta.id.clone()
        };
        let mut records = Vec::with_capacity(transcript.body.len() + 1);

        // A leading summary gives `claude --resume` a friendly title.
        if let Some(title) = meta.title.as_deref().filter(|t| !t.is_empty()) {
            records.push(Record::Summary(SummaryLine {
                summary: title.to_string(),
                extra: Map::from_iter([(
                    "leafUuid".into(),
                    Value::String(entry_uuid(&session_id, usize::MAX)),
                )]),
            }));
        }

        let mut parent_uuid: Option<String> = None;
        for (i, msg) in transcript.body.iter().enumerate() {
            let uuid = entry_uuid(&session_id, i);
            let api = ApiMessage {
                role: Some(role_str(msg.role).to_string()),
                content: serialize_blocks(&msg.content),
                model: msg.model.clone(),
                stop_reason: msg.stop_reason.as_ref().map(stop_reason_str),
                usage: msg.usage.as_ref().map(serialize_usage),
                extra: Map::new(),
            };
            let entry = EntryLine {
                parent_uuid: parent_uuid.clone(),
                uuid: uuid.clone(),
                timestamp: Some(msg.timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)),
                session_id: Some(session_id.clone()),
                cwd: meta.cwd.clone(),
                git_branch: meta.git_branch.clone(),
                version: meta.cli_version.clone(),
                message: api,
                extra: Map::new(),
            };
            records.push(match msg.role {
                Role::User => Record::User(entry),
                Role::Assistant => Record::Assistant(entry),
            });
            parent_uuid = Some(uuid);
        }

        Ok(Transcript::new(meta.clone(), records))
    }
}

impl TextCodec for ClaudeCode {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        // Not jsonl::parse: that would route every line through Record's
        // Deserialize and its intermediate `Value` tree. Typed lines here
        // parse straight into their structs.
        let records: Vec<Record> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(record_from_line)
            .collect();
        let meta = meta_from_records(&records);
        Ok(Transcript::new(meta, records))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        jsonl::render(&transcript.body)
    }
}

// ── store ──────────────────────────────────────────────────────────────

/// Reads and writes Claude Code sessions under a projects root (default
/// `~/.claude/projects`).
#[derive(Debug, Clone)]
pub struct ClaudeStore {
    pub root: PathBuf,
}

impl ClaudeStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The default projects root: `$CLAUDE_CONFIG_DIR/projects` when set
    /// (every `~/.claude` path moves under that directory), else
    /// `~/.claude/projects`.
    #[must_use]
    pub fn default_root() -> Option<Self> {
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|v| !v.is_empty())
            .map(|dir| Self::new(PathBuf::from(dir).join("projects")))
            .or_else(|| dirs_home().map(|h| Self::new(h.join(".claude").join("projects"))))
    }

    fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
        // An unreadable directory lists nothing.
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                // Subagent and tool-result side-files aren't top-level sessions.
                if name != "subagents" && name != "tool-results" {
                    Self::collect_jsonl(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                out.push(path);
            }
        }
    }
}

impl Store for ClaudeStore {
    type H = ClaudeCode;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        if self.root.is_dir() {
            let mut files = Vec::new();
            Self::collect_jsonl(&self.root, &mut files);
            Ok(files
                .into_iter()
                .filter_map(|path| {
                    // A session that fails to read is skipped, not fatal. Meta
                    // comes from the shallow scan, not a full parse.
                    fs::read_to_string(&path).ok().map(|text| {
                        let mut meta = meta_from_text(&text);
                        if meta.id.is_empty() {
                            meta.id = jsonl::file_id(&path);
                        }
                        Discovered {
                            meta,
                            reference: path,
                        }
                    })
                })
                .collect())
        } else {
            // A missing root means no sessions, not an error.
            Ok(Vec::new())
        }
    }

    fn load(&self, reference: &PathBuf) -> Result<Transcript<ClaudeCode>> {
        let mut transcript = ClaudeCode::from_text(&fs::read_to_string(reference)?)?;
        if transcript.meta.id.is_empty() {
            transcript.meta.id = jsonl::file_id(reference);
        }
        Ok(transcript)
    }

    fn save(&self, transcript: &Transcript<ClaudeCode>) -> Result<Saved<PathBuf>> {
        let cwd = transcript.meta.cwd.as_deref().unwrap_or_default();
        let dir = self.root.join(encode_project_dir(cwd));
        fs::create_dir_all(&dir)?;
        let id = transcript.meta.id.clone();
        let path = dir.join(format!("{id}.jsonl"));
        fs::write(&path, ClaudeCode::to_text(transcript)?)?;
        Ok(Saved {
            id,
            reference: path,
        })
    }

    fn delete(&self, reference: &PathBuf) -> Result<()> {
        Ok(fs::remove_file(reference)?)
    }

    fn fingerprints(&self, refs: &[PathBuf]) -> Result<HashMap<String, String>> {
        let mut out = HashMap::with_capacity(refs.len());
        for path in refs {
            out.insert(path.to_string_lossy().into_owned(), file_fingerprint(path));
        }
        Ok(out)
    }
}

// ── block <-> value ────────────────────────────────────────────────────

fn parse_blocks(content: &Value) -> Vec<Block> {
    match content {
        Value::String(s) => {
            if s.is_empty() {
                Vec::new()
            } else {
                vec![Block::Text { text: s.clone() }]
            }
        }
        Value::Array(arr) => arr.iter().filter_map(parse_block).collect(),
        // Content is a string or a block array on the wire; any other JSON
        // shape carries no blocks.
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => Vec::new(),
    }
}

fn parse_block(v: &Value) -> Option<Block> {
    match v.get("type").and_then(Value::as_str)? {
        "text" => Some(Block::Text {
            text: v.get("text")?.as_str()?.to_string(),
        }),
        "thinking" => Some(Block::Thinking {
            text: v.get("thinking")?.as_str()?.to_string(),
            signature: v.get("signature").and_then(Value::as_str).map(String::from),
            encrypted: None,
        }),
        "tool_use" => {
            let id = v.get("id")?.as_str()?.to_string();
            let name = v.get("name")?.as_str()?;
            let input = v.get("input").cloned().unwrap_or(Value::Object(Map::new()));
            Some(Block::ToolUse {
                id,
                tool: Tool::from_canonical(name, input),
            })
        }
        "tool_result" => Some(Block::ToolResult {
            tool_use_id: v.get("tool_use_id")?.as_str()?.to_string(),
            content: parse_tool_output(v.get("content")),
            is_error: v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
        }),
        "image" => {
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
        // Block types we don't model carry nothing into the canonical model
        // (the native `Record` still round-trips them).
        _ => None,
    }
}

fn serialize_blocks(blocks: &[Block]) -> Value {
    Value::Array(blocks.iter().map(serialize_block).collect())
}

fn serialize_block(block: &Block) -> Value {
    match block {
        Block::Text { text } => serde_json::json!({"type": "text", "text": text}),
        Block::Thinking {
            text, signature, ..
        } => {
            let mut obj = serde_json::json!({"type": "thinking", "thinking": text});
            if let Some(sig) = signature {
                obj["signature"] = Value::String(sig.clone());
            }
            obj
        }
        Block::ToolUse { id, tool } => {
            let (name, input) = tool.to_canonical();
            serde_json::json!({"type": "tool_use", "id": id, "name": name, "input": input})
        }
        Block::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let mut obj = serde_json::json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": serialize_tool_output(content),
            });
            if *is_error {
                obj["is_error"] = Value::Bool(true);
            }
            obj
        }
        Block::Image { source } => serde_json::json!({
            "type": "image",
            "source": {
                "type": source.source_type,
                "media_type": source.media_type,
                "data": source.data,
            },
        }),
    }
}

fn parse_tool_output(content: Option<&Value>) -> ToolOutput {
    match content {
        Some(Value::String(s)) => ToolOutput::Text(s.clone()),
        Some(other) => ToolOutput::Json(other.clone()),
        None => ToolOutput::Text(String::new()),
    }
}

fn serialize_tool_output(out: &ToolOutput) -> Value {
    match out {
        ToolOutput::Text(s) => Value::String(s.clone()),
        // The Anthropic wire accepts only a string or a content-block array
        // in `tool_result.content`; a bare object fails the whole session
        // load on resume. Keep block arrays (Claude's own native shape),
        // flatten any other JSON to its compact text.
        ToolOutput::Json(v) if is_block_array(v) => v.clone(),
        ToolOutput::Json(v) => Value::String(v.to_string()),
    }
}

/// Whether a value is an Anthropic content-block array — the only non-string
/// shape Claude Code itself writes into `tool_result.content`.
fn is_block_array(v: &Value) -> bool {
    v.as_array().is_some_and(|arr| {
        arr.iter()
            .all(|b| b.get("type").and_then(Value::as_str).is_some())
    })
}

fn parse_usage(v: &Value) -> Option<Usage> {
    Some(Usage {
        input_tokens: v.get("input_tokens")?.as_u64()?,
        output_tokens: v.get("output_tokens")?.as_u64()?,
        cache_read_input_tokens: v.get("cache_read_input_tokens").and_then(Value::as_u64),
        cache_creation_input_tokens: v.get("cache_creation_input_tokens").and_then(Value::as_u64),
    })
}

fn serialize_usage(u: &Usage) -> Value {
    let mut obj = serde_json::json!({
        "input_tokens": u.input_tokens,
        "output_tokens": u.output_tokens,
    });
    if let Some(read) = u.cache_read_input_tokens {
        obj["cache_read_input_tokens"] = read.into();
    }
    if let Some(write) = u.cache_creation_input_tokens {
        obj["cache_creation_input_tokens"] = write.into();
    }
    obj
}

// Claude's stop_reason strings are the canonical Anthropic set.
fn parse_stop_reason(s: &str) -> StopReason {
    match s {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        other => StopReason::Other(other.to_string()),
    }
}

fn stop_reason_str(r: &StopReason) -> String {
    match r {
        StopReason::EndTurn => "end_turn".into(),
        StopReason::ToolUse => "tool_use".into(),
        StopReason::MaxTokens => "max_tokens".into(),
        StopReason::StopSequence => "stop_sequence".into(),
        StopReason::Aborted => "aborted".into(),
        StopReason::Error => "error".into(),
        StopReason::Other(s) => s.clone(),
    }
}

// ── helpers ────────────────────────────────────────────────────────────

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    s.parse::<DateTime<Utc>>().ok()
}

/// Deterministic per-entry uuid, so `from_common` is a pure function of the
/// transcript (no randomness, reproducible conversions and tests).
fn entry_uuid(session_id: &str, index: usize) -> String {
    const NS: Uuid = Uuid::from_bytes([
        0x9f, 0x0d, 0x98, 0x36, 0x9e, 0xe7, 0x4c, 0x62, 0x83, 0xb4, 0xfb, 0x8e, 0x01, 0x36, 0x5c,
        0x9f,
    ]);
    Uuid::new_v5(&NS, format!("{session_id}:{index}").as_bytes()).to_string()
}

/// Extract session metadata from the records. `id` is left empty when no
/// `sessionId` is present; a [`Store`] fills it from the filename.
fn meta_from_records(records: &[Record]) -> Meta {
    let mut meta = Meta {
        id: String::new(),
        timestamp: Utc::now(),
        cwd: None,
        git_branch: None,
        title: None,
        cli_version: None,
        model: None,
    };
    let mut summary: Option<String> = None;
    let mut custom_title: Option<String> = None;
    let mut earliest: Option<DateTime<Utc>> = None;

    for record in records {
        match record {
            Record::User(e) => {
                if let Some(id) = &e.session_id
                    && meta.id.is_empty()
                {
                    meta.id.clone_from(id);
                }
                meta.cwd = meta.cwd.take().or_else(|| e.cwd.clone());
                meta.git_branch = meta.git_branch.take().or_else(|| e.git_branch.clone());
                meta.cli_version = meta.cli_version.take().or_else(|| e.version.clone());
                note_ts(&mut earliest, e.timestamp.as_deref());
            }
            Record::Assistant(e) => {
                if meta.model.is_none() {
                    meta.model.clone_from(&e.message.model);
                }
                note_ts(&mut earliest, e.timestamp.as_deref());
            }
            Record::Summary(s) => {
                if summary.is_none() {
                    summary = Some(s.summary.clone());
                }
            }
            Record::Other(v) => match v.get("type").and_then(Value::as_str) {
                Some("custom-title") => {
                    custom_title = v
                        .get("customTitle")
                        .and_then(Value::as_str)
                        .map(String::from);
                }
                Some("agent-name") if custom_title.is_none() => {
                    custom_title = v.get("agentName").and_then(Value::as_str).map(String::from);
                }
                // Other line types carry no session metadata.
                _ => {}
            },
        }
    }

    if let Some(ts) = earliest {
        meta.timestamp = ts;
    }
    meta.title = custom_title.or(summary);
    meta
}

fn note_ts(earliest: &mut Option<DateTime<Utc>>, ts: Option<&str>) {
    if let Some(parsed) = ts.and_then(parse_ts)
        && earliest.is_none_or(|e| parsed < e)
    {
        *earliest = Some(parsed);
    }
}

// ── discover: shallow meta scan ────────────────────────────────────────

/// Shallow mirror of [`EntryLine`] for discovery: the same typed fields with
/// the same types — so a line that fails the full parse fails here too — but
/// the payload-bearing `message.content` is only presence-checked, never
/// built. Fields the full parse tolerates unconditionally (`extra`, `usage`)
/// are covered by serde's ignore-unknown default.
#[derive(Deserialize)]
struct MetaEntryLine {
    #[serde(rename = "parentUuid", default)]
    parent_uuid: Option<String>,
    uuid: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(rename = "gitBranch", default)]
    git_branch: Option<String>,
    #[serde(default)]
    version: Option<String>,
    message: MetaApiMessage,
}

/// Shallow mirror of [`ApiMessage`]: `content` must be present (as in the
/// full parse) but its value is skipped, not built.
#[derive(Deserialize)]
struct MetaApiMessage {
    #[serde(default)]
    role: Option<String>,
    #[allow(dead_code)] // presence check only; the value is never built
    content: serde::de::IgnoredAny,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
}

// A skeleton [`EntryLine`] carrying only what [`meta_from_records`] reads.
impl From<MetaEntryLine> for EntryLine {
    fn from(m: MetaEntryLine) -> EntryLine {
        EntryLine {
            parent_uuid: m.parent_uuid,
            uuid: m.uuid,
            timestamp: m.timestamp,
            session_id: m.session_id,
            cwd: m.cwd,
            git_branch: m.git_branch,
            version: m.version,
            message: ApiMessage {
                role: m.message.role,
                content: Value::Null,
                model: m.message.model,
                stop_reason: m.message.stop_reason,
                usage: None,
                extra: Map::new(),
            },
            extra: Map::new(),
        }
    }
}

/// Parse one line straight into its typed record — no intermediate [`Value`]
/// tree. Only lines outside the typed set pay a second parse into the
/// preserved [`Record::Other`] `Value`: an unknown or non-string tag, or the
/// rare known-tag line whose body fails its typed schema. Invalid JSON
/// parses to `None` and is skipped, exactly as under [`jsonl::parse`].
fn record_from_line(line: &str) -> Option<Record> {
    let other = || serde_json::from_str::<Value>(line).ok().map(Record::Other);
    match serde_json::from_str::<jsonl::TypeProbe>(line) {
        // A failed probe isn't necessarily junk: a non-string `type` also
        // fails it. The fallback keeps such lines whole and skips the rest.
        Err(_) => other(),
        Ok(probe) => match probe.kind.as_deref() {
            Some("summary") => serde_json::from_str(line)
                .ok()
                .map(Record::Summary)
                .or_else(other),
            Some("user") => serde_json::from_str(line)
                .ok()
                .map(Record::User)
                .or_else(other),
            Some("assistant") => serde_json::from_str(line)
                .ok()
                .map(Record::Assistant)
                .or_else(other),
            Some(_) | None => other(),
        },
    }
}

/// Parse one line only as far as discovery needs: user/assistant lines
/// through the shallow mirrors, summary/custom-title/agent-name lines whole
/// (each is one short line). Everything else — malformed lines included —
/// lands in [`Record::Other`] under the full parse and contributes no
/// metadata there, so here it parses to `None`.
fn scan_line(line: &str) -> Option<Record> {
    let probe: jsonl::TypeProbe = serde_json::from_str(line).ok()?;
    match probe.kind.as_deref() {
        Some("user") => serde_json::from_str::<MetaEntryLine>(line)
            .ok()
            .map(|m| Record::User(m.into())),
        Some("assistant") => serde_json::from_str::<MetaEntryLine>(line)
            .ok()
            .map(|m| Record::Assistant(m.into())),
        Some("summary") => serde_json::from_str::<SummaryLine>(line)
            .ok()
            .map(Record::Summary),
        Some("custom-title" | "agent-name") => serde_json::from_str(line).ok().map(Record::Other),
        // No other line type carries session metadata.
        Some(_) | None => None,
    }
}

/// [`meta_from_records`] over a shallow scan of the raw text — equivalent to
/// `from_text(text)?.meta`, but discovery never builds message payloads.
fn meta_from_text(text: &str) -> Meta {
    let records: Vec<Record> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(scan_line)
        .collect();
    meta_from_records(&records)
}

/// Claude's project-dir encoding: every `/` and `.` becomes `-`. Windows
/// cwds encode the same way with `\` and the drive `:` also mapped
/// (`C:\Users\x` → `C--Users-x`).
fn encode_project_dir(path: &str) -> String {
    path.chars()
        .map(|c| {
            if matches!(c, '/' | '.' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect()
}

fn file_fingerprint(path: &Path) -> String {
    match fs::metadata(path) {
        // An unreadable file has no fingerprint.
        Err(_) => String::new(),
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{mtime}:{}", meta.len())
        }
    }
}

fn dirs_home() -> Option<PathBuf> {
    super::home_dir()
}

// Surface the canonical-name guard as a typed error for callers that care.
#[allow(dead_code)]
fn unconvertible(detail: impl Into<String>) -> Error {
    Error::Unconvertible {
        harness: ClaudeCode::NAME,
        detail: detail.into(),
    }
}
