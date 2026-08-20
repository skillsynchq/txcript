//! Cowork — the Claude desktop app's local agent mode:
//! `<app data>/Claude/local-agent-mode-sessions/<org>/<account>/local_<uuid>.json`.
//!
//! Cowork runs Claude Code headlessly (through the Agent SDK) with a private
//! `CLAUDE_CONFIG_DIR` per task, and keeps its own session record next to it.
//! One session is therefore three carriers:
//!
//! - `local_<id>.json` — the app's session record (**header** here): the
//!   `local_…` session id, `cliSessionId` (the Claude Code session under it),
//!   `cwd`, `createdAt`/`lastActivityAt` (epoch ms), `model`, `title`,
//!   `isArchived`, the rendered system prompt, MCP/plugin settings. The app
//!   lists sessions by reading these; a record its validator rejects is
//!   silently skipped. Required: `sessionId`, `processName`, `cwd`,
//!   `createdAt`, `lastActivityAt`.
//! - `local_<id>/.claude/projects/<encoded-cwd>/<cliSessionId>.jsonl` — the
//!   conversation, in Claude Code's own JSONL (**transcript**). The app
//!   locates it by `cliSessionId` under any project slug and resumes it with
//!   the CLI; txcript reuses the `claude_code` codec on it verbatim, so every
//!   Claude Code record kind, tool, and quirk applies unchanged. Cowork-only
//!   records (`queue-operation`, `last-prompt`, `attachment`, `ai-title`)
//!   ride through as [`Record::Other`].
//! - `local_<id>/audit.jsonl` — the Agent SDK stream as the app saw it
//!   (**audit**): an append-only, HMAC-chained log whose key lives in
//!   Electron's `safeStorage`. It is carried verbatim for native round trips
//!   and never regenerated — the app tolerates its absence (unsigned and
//!   missing logs are both valid states for it).
//!
//! Not carried: the per-task `.claude/.claude.json` config cache and its
//! backups, `uploads/` and `outputs/` (the user's files), subagent
//! transcripts under `<cliSessionId>/subagents/`, and `debug/`.
//!
//! `to_common` is the Claude Code mapping over the transcript; the header
//! supplies `Meta` (id, title, cwd, model, start time). `from_common`
//! regenerates the transcript through the Claude Code codec under a
//! deterministic `cliSessionId` (`UUIDv5` of the session id) and a header
//! carrying every field the app requires; the session id is given a `local_`
//! prefix when it lacks one, which is the one way `Meta` can change through
//! Common. The audit log is left empty. Everything `claude_code` cannot
//! represent (its module docs) is lost here too.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use uuid::Uuid;

use crate::common::{Block, Message, Meta, Role};
use crate::error::{Error, Result};
use crate::harness::claude_code::{self, Record};
use crate::harness::jsonl;
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

/// The Cowork harness marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cowork;

impl Harness for Cowork {
    const NAME: &'static str = "cowork";
    type Body = CoworkSession;
}

// ── native records ─────────────────────────────────────────────────────

/// Faithful in-memory representation of one Cowork session: its app record,
/// its Claude Code transcript, and its audit log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoworkSession {
    pub header: Header,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transcript: Vec<Record>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audit: Vec<Value>,
}

/// The app's session record (`local_<id>.json`). Only the fields the codec
/// reads or writes are typed; the rest (system prompt, MCP configuration,
/// permission grants, …) flatten into `extra` untouched.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Header {
    #[serde(rename = "sessionId", default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(
        rename = "cliSessionId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cli_session_id: Option<String>,
    #[serde(
        rename = "processName",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub process_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Epoch milliseconds.
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<Number>,
    /// Epoch milliseconds.
    #[serde(
        rename = "lastActivityAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub last_activity_at: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(
        rename = "isArchived",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub is_archived: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// ── codec ──────────────────────────────────────────────────────────────

impl Codec for Cowork {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            claude_code::records_to_messages(
                &transcript.body.transcript,
                transcript.meta.timestamp,
            ),
        ))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        let (meta, body) = body_from_messages(&transcript.meta, &transcript.body);
        Ok(Transcript::new(meta, body))
    }
}

impl TextCodec for Cowork {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let body: CoworkSession = serde_json::from_str(text)?;
        let meta = meta_from_body(&body);
        Ok(Transcript::new(meta, body))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Ok(serde_json::to_string_pretty(&transcript.body)?)
    }
}

/// The native session for a canonical conversation, plus the `Meta` it
/// answers to (the session id gains Cowork's `local_` prefix if missing).
fn body_from_messages(meta: &Meta, messages: &[Message]) -> (Meta, CoworkSession) {
    let session_id = session_id_for(&meta.id);
    let cli_session_id = cli_session_id_for(&session_id);

    // The transcript is Claude Code's, stamped with the CLI session id —
    // that is the id the app resumes by, not the `local_…` one.
    let mut cli_meta = meta.clone();
    cli_meta.id.clone_from(&cli_session_id);
    let transcript = claude_code::messages_to_records(&cli_meta, messages);

    let created_at = meta.timestamp.timestamp_millis();
    let last_activity_at = messages
        .iter()
        .map(|m| m.timestamp.timestamp_millis())
        .max()
        .map_or(created_at, |last| last.max(created_at));
    let initial_message = messages
        .iter()
        .find(|m| m.role == Role::User)
        .and_then(|m| {
            m.content.iter().find_map(|block| match block {
                Block::Text { text } => Some(text.clone()),
                // Only text opens a prompt in the app's record.
                Block::Thinking { .. }
                | Block::ToolUse { .. }
                | Block::ToolResult { .. }
                | Block::Image { .. } => None,
            })
        });

    let mut extra = Map::new();
    // The app runs the CLI on the host against `cwd` (rather than inside its
    // Linux VM, whose cwd would be `/sessions/<processName>`).
    extra.insert("hostLoopMode".into(), Value::Bool(true));
    if let Some(text) = initial_message {
        extra.insert("initialMessage".into(), Value::String(text));
    }
    let header = Header {
        session_id: Some(session_id.clone()),
        cli_session_id: Some(cli_session_id.clone()),
        // Required by the app's record validator; natively the VM/process
        // name. Derived, so conversion stays a pure function of the input.
        process_name: Some(format!("txcript-{}", &cli_session_id[..8])),
        cwd: Some(meta.cwd.clone().unwrap_or_default()),
        created_at: Some(created_at.into()),
        last_activity_at: Some(last_activity_at.into()),
        model: meta.model.clone(),
        title: meta.title.clone(),
        is_archived: Some(false),
        extra,
    };

    let mut out_meta = meta.clone();
    out_meta.id = session_id;
    (
        out_meta,
        CoworkSession {
            header,
            transcript,
            audit: Vec::new(),
        },
    )
}

/// Cowork session ids are `local_<uuid>`; the app lists only files with that
/// prefix. An empty id mints a fresh one.
fn session_id_for(id: &str) -> String {
    if id.is_empty() {
        format!("local_{}", Uuid::new_v4())
    } else if id.starts_with("local_") {
        id.to_string()
    } else {
        format!("local_{id}")
    }
}

/// Deterministic Claude Code session id for a Cowork session, so
/// `from_common` is a pure function of the transcript.
fn cli_session_id_for(session_id: &str) -> String {
    const NS: Uuid = Uuid::from_bytes([
        0x3c, 0x7a, 0xe1, 0x52, 0x8b, 0x4d, 0x4e, 0x0f, 0x9a, 0x61, 0x2d, 0xc8, 0x7f, 0x15, 0xb9,
        0x04,
    ]);
    Uuid::new_v5(&NS, session_id.as_bytes()).to_string()
}

// ── metadata ───────────────────────────────────────────────────────────

/// `Meta` from the session: the header is authoritative for what it carries
/// (id, start time, cwd, title, model); the transcript supplies the rest
/// (CLI version, git branch) and backfills any header gap.
fn meta_from_body(body: &CoworkSession) -> Meta {
    meta_from_parts(
        &body.header,
        claude_code::meta_from_records(&body.transcript),
    )
}

fn meta_from_parts(header: &Header, transcript: Meta) -> Meta {
    let non_empty = |s: &Option<String>| s.clone().filter(|v| !v.trim().is_empty());
    Meta {
        id: header.session_id.clone().unwrap_or_default(),
        timestamp: header
            .created_at
            .as_ref()
            .and_then(epoch_millis)
            .unwrap_or(transcript.timestamp),
        cwd: non_empty(&header.cwd).or(transcript.cwd),
        git_branch: transcript.git_branch,
        title: non_empty(&header.title).or(transcript.title),
        cli_version: transcript.cli_version,
        model: non_empty(&header.model).or(transcript.model),
    }
}

/// Epoch milliseconds as written by `Date.now()`; a fractional value is
/// truncated to the millisecond.
#[allow(clippy::cast_possible_truncation)] // guarded: finite, in i64 range
fn epoch_millis(n: &Number) -> Option<DateTime<Utc>> {
    let ms = n.as_i64().or_else(|| {
        n.as_f64()
            .filter(|f| f.is_finite() && f.abs() < 9.0e15)
            .map(|f| f as i64)
    })?;
    DateTime::from_timestamp_millis(ms)
}

// ── store ──────────────────────────────────────────────────────────────

/// Reads and writes Cowork sessions under the app's
/// `local-agent-mode-sessions` directory.
///
/// The directory holds one `<org-uuid>/<account-uuid>/` tree per signed-in
/// account; discovery walks them all, and `save` writes into the most
/// recently active one.
#[derive(Debug, Clone)]
pub struct CoworkStore {
    pub root: PathBuf,
}

impl CoworkStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The app's sessions root: `$COWORK_SESSIONS_DIR` when set, else
    /// `local-agent-mode-sessions` under the Claude desktop app's data
    /// directory (`~/Library/Application Support/Claude` on macOS,
    /// `%APPDATA%\Claude` on Windows, `~/.config/Claude` elsewhere).
    #[must_use]
    pub fn default_root() -> Option<Self> {
        if let Some(dir) = std::env::var_os("COWORK_SESSIONS_DIR").filter(|v| !v.is_empty()) {
            return Some(Self::new(PathBuf::from(dir)));
        }
        let home = super::home_dir()?;
        let app_data = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/Claude")
        } else if cfg!(windows) {
            std::env::var_os("APPDATA")
                .filter(|v| !v.is_empty())
                .map_or_else(|| home.join("AppData").join("Roaming"), PathBuf::from)
                .join("Claude")
        } else {
            home.join(".config/Claude")
        };
        Some(Self::new(app_data.join("local-agent-mode-sessions")))
    }

    /// Every `<org>/<account>/` directory under the root. Both levels are
    /// UUID-named; that is what separates account trees from the app's
    /// other state (`skills-plugin/`, …) at the same level.
    fn account_dirs(&self) -> Vec<PathBuf> {
        let uuid_dirs = |dir: &Path| -> Vec<PathBuf> {
            fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.is_dir()
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| Uuid::parse_str(n).is_ok())
                })
                .collect()
        };
        let mut out: Vec<PathBuf> = uuid_dirs(&self.root)
            .iter()
            .flat_map(|org| uuid_dirs(org))
            .collect();
        out.sort();
        out
    }

    /// The account tree `save` writes into: the one whose newest session
    /// record is most recent (the account the app is using), else the only
    /// one there is.
    fn active_account_dir(&self) -> Result<PathBuf> {
        let newest_record = |dir: &Path| {
            session_files(dir)
                .iter()
                .filter_map(|p| fs::metadata(p).and_then(|m| m.modified()).ok())
                .max()
        };
        self.account_dirs()
            .into_iter()
            .map(|dir| (newest_record(&dir), dir))
            .max()
            .map(|(_, dir)| dir)
            .ok_or_else(|| Error::Unconvertible {
                harness: Cowork::NAME,
                detail: format!(
                    "no Cowork account directory under {}; open Cowork once so the app \
                     creates its <org>/<account> tree",
                    self.root.display()
                ),
            })
    }

    /// The transcript file for a session record, if the header names a CLI
    /// session and the file exists under any project slug.
    fn transcript_path(session_dir: &Path, header: &Header) -> Option<PathBuf> {
        let cli = header.cli_session_id.as_deref()?;
        super::checked_id_component(Cowork::NAME, cli).ok()?;
        let projects = session_dir.join(".claude").join("projects");
        fs::read_dir(projects)
            .ok()?
            .flatten()
            .map(|slug| slug.path().join(format!("{cli}.jsonl")))
            .find(|p| p.is_file())
    }
}

/// The session records in one directory, plus those of its `agent/`
/// subdirectory (where the app keeps its agent-type sessions).
fn session_files(dir: &Path) -> Vec<PathBuf> {
    let records = |dir: &Path| -> Vec<PathBuf> {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && is_session_record(p))
            .collect()
    };
    let mut out = records(dir);
    out.extend(records(&dir.join("agent")));
    out.sort();
    out
}

/// Whether a path is named like an app session record: `local_*.json`.
fn is_session_record(path: &Path) -> bool {
    path.file_stem()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("local_"))
        && path.extension().is_some_and(|e| e == "json")
}

/// The session's storage directory: the record's path without `.json`.
fn session_dir(record: &Path) -> PathBuf {
    record.with_extension("")
}

fn read_header(path: &Path) -> Result<Header> {
    let text = fs::read_to_string(path)?;
    let header: Header = serde_json::from_str(&text)?;
    if header.session_id.is_none() {
        return Err(Error::Malformed {
            harness: Cowork::NAME,
            detail: format!("{} carries no sessionId", path.display()),
        });
    }
    Ok(header)
}

impl Store for CoworkStore {
    type H = Cowork;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        // No root (Cowork never ran here) means no sessions.
        if !self.root.is_dir() {
            return Ok(Vec::new());
        }
        Ok(self
            .account_dirs()
            .iter()
            .flat_map(|account| session_files(account))
            .filter_map(|path| {
                // A record the app would reject, or that fails to read, is
                // skipped, not fatal. Discovery meta comes from the header
                // plus Claude Code's shallow scan of the transcript — the
                // same fields a full load would yield.
                let header = read_header(&path).ok()?;
                let transcript_meta = Self::transcript_path(&session_dir(&path), &header)
                    .and_then(|p| fs::read_to_string(p).ok())
                    .map_or_else(
                        || claude_code::meta_from_records(&[]),
                        |text| claude_code::meta_from_text(&text),
                    );
                let mut meta = meta_from_parts(&header, transcript_meta);
                if meta.id.is_empty() {
                    meta.id = jsonl::file_id(&path);
                }
                Some(Discovered {
                    meta,
                    reference: path,
                })
            })
            .collect())
    }

    fn load(&self, reference: &PathBuf) -> Result<Transcript<Cowork>> {
        let header = read_header(reference)?;
        let dir = session_dir(reference);
        let transcript = Self::transcript_path(&dir, &header)
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|text| {
                text.lines()
                    .filter(|line| !line.trim().is_empty())
                    .filter_map(claude_code::record_from_line)
                    .collect()
            })
            .unwrap_or_default();
        let audit = fs::read_to_string(dir.join("audit.jsonl"))
            .map(|text| jsonl::parse(&text))
            .unwrap_or_default();
        let body = CoworkSession {
            header,
            transcript,
            audit,
        };
        let mut meta = meta_from_body(&body);
        if meta.id.is_empty() {
            meta.id = jsonl::file_id(reference);
        }
        Ok(Transcript::new(meta, body))
    }

    fn save(&self, transcript: &Transcript<Cowork>) -> Result<Saved<PathBuf>> {
        let body = &transcript.body;
        let id = body
            .header
            .session_id
            .clone()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| transcript.meta.id.clone());
        super::checked_id_component(Cowork::NAME, &id)?;
        let account = self.active_account_dir()?;
        let record = account.join(format!("{id}.json"));
        let dir = account.join(&id);

        // The CLI session the app will resume: the header's, or the one the
        // codec would have derived.
        let cli = body
            .header
            .cli_session_id
            .clone()
            .unwrap_or_else(|| cli_session_id_for(&id));
        super::checked_id_component(Cowork::NAME, &cli)?;
        let cwd = body
            .header
            .cwd
            .clone()
            .or_else(|| transcript.meta.cwd.clone())
            .unwrap_or_default();
        let project_dir = dir
            .join(".claude")
            .join("projects")
            .join(claude_code::encode_project_dir(&cwd));
        fs::create_dir_all(&project_dir)?;
        // The app's cwd for host sessions; also where it expects uploads.
        fs::create_dir_all(dir.join("outputs"))?;
        fs::create_dir_all(dir.join("uploads"))?;

        fs::write(
            project_dir.join(format!("{cli}.jsonl")),
            jsonl::render(&body.transcript)?,
        )?;
        if !body.audit.is_empty() {
            fs::write(dir.join("audit.jsonl"), jsonl::render(&body.audit)?)?;
        }
        fs::write(&record, serde_json::to_string(&body.header)?)?;
        Ok(Saved {
            id,
            reference: record,
        })
    }

    /// Removes the session record and its storage directory. Guarded on
    /// shape and containment: the reference must be a `local_*.json` record
    /// resolving to `<root>/<org>/<account>/[agent/]<id>.json`, so a stale or
    /// foreign reference never removes an unrelated tree.
    fn delete(&self, reference: &PathBuf) -> Result<()> {
        if !(is_session_record(reference) && reference.is_file()) {
            return Err(Error::Malformed {
                harness: Cowork::NAME,
                detail: format!("not a Cowork session record: {}", reference.display()),
            });
        }
        let canon = reference.canonicalize()?;
        let root = self.root.canonicalize()?;
        let contained = canon.strip_prefix(&root).is_ok_and(|rest| {
            let parts: Vec<_> = rest.components().collect();
            parts.len() == 3
                || (parts.len() == 4 && parts[2].as_os_str() == std::ffi::OsStr::new("agent"))
        });
        if !contained {
            return Err(Error::Malformed {
                harness: Cowork::NAME,
                detail: format!(
                    "refusing to delete outside the sessions root: {}",
                    reference.display()
                ),
            });
        }
        let dir = session_dir(&canon);
        if dir.is_dir() {
            fs::remove_dir_all(&dir)?;
        }
        Ok(fs::remove_file(canon)?)
    }

    fn fingerprints(&self, refs: &[PathBuf]) -> Result<HashMap<String, String>> {
        let mut out = HashMap::with_capacity(refs.len());
        for record in refs {
            // The transcript is what grows; the record changes with it, so
            // it stands in when the transcript can't be found.
            let file = read_header(record)
                .ok()
                .and_then(|h| Self::transcript_path(&session_dir(record), &h))
                .unwrap_or_else(|| record.clone());
            out.insert(
                record.to_string_lossy().into_owned(),
                claude_code::file_fingerprint(&file),
            );
        }
        Ok(out)
    }
}
