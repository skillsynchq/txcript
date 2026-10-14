//! Qoder CLI: `~/.qoder/projects/<encoded-cwd>/<session>.jsonl`.
//!
//! Qoder's transcript is Claude Code's wire format: one JSON object per line,
//! `user`/`assistant` lines wrapping an Anthropic message, threaded by
//! `parentUuid`. Tool calls use Claude's own tool names and argument keys
//! (`Read`, `Edit`, `Write`, `Bash`, `Grep`, `Glob`, ...) and tool results ride
//! on `user` lines. So `Qoder` shares Claude Code's native [`Record`] body and
//! conversation codec. What differs is everything around the conversation:
//!
//! - **Bookkeeping lines** (`active-leaf`, `runtime-config`,
//!   `workspace-directories`, `worktree-state`, `last-prompt`, `relocated`,
//!   `file-history-snapshot`, `attachment`, informational `system`) are kept
//!   verbatim in [`Record::Other`] and carry no turn. Hook output, skill and
//!   agent listings are `attachment` lines, so they never reach the
//!   conversation.
//! - **Titles** come from a `custom-title` line (`qodercli -n <name>`), else
//!   from the generated `ai-title` line. Qoder has no `summary` line, so
//!   `from_common` writes `ai-title`.
//! - **Lineage**: a session made with `--fork-session` stamps every copied
//!   line with `forkedFrom: {sessionId, messageUuid}`; the first such
//!   `sessionId` is a [`Relation::Fork`] parent, read at discovery as well as
//!   on load.
//! - **Directory encoding**: the project directory is the cwd with every
//!   UTF-16 code unit outside `[A-Za-z0-9]` replaced by `-`. Past 200
//!   characters it is cut to 200, then `-`, then the base-36 absolute value
//!   of a djb2-xor hash of the original path (observed on a 315-character
//!   cwd).
//! - **Sidecars**: `<session>/state.json` and `<session>/compression-v2/` are
//!   Qoder-internal. They are neither read nor written; a session without
//!   them resumes with its full history, and `delete` mirrors
//!   `qodercli --delete-session` by removing only the `.jsonl`.
//! - **Discovery** lists `<projects>/<project>/<session>.jsonl` files that
//!   hold at least one line with a `type` and a `sessionId`; `delete` only
//!   removes a file at that same depth.
//!
//! What `from_common` writes: the conversation lines (exactly Claude Code's),
//! an `ai-title` line when the session has a title, and a trailing
//! `active-leaf` line naming the last turn. Resume needs only the
//! conversation chain; the extra lines mirror what Qoder itself writes.
//! Model names, message ids and usage from other harnesses are accepted
//! as-is; a thinking block's foreign signature is written through and Qoder
//! resumes with it. Known losses are Claude Code's, plus `Meta::lineage`,
//! which is read but not written back.
//!
//! The CLI has two regional builds with this one format. The global build
//! (`qodercli`, `~/.qoder`) honors `QODER_CONFIG_DIR`, which this store
//! follows. The China build (`qoderclicn`, `~/.qoder-cn`) ignores that
//! variable; point txcript at it with `QODER_CONFIG_DIR="$HOME/.qoder-cn"`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::DateTime;
use serde::Deserialize;
use serde_json::json;

use crate::common::{Lineage, Meta, Relation};
use crate::error::{Error, Result};
use crate::harness::claude_code::{self, Record};
use crate::harness::jsonl;
use crate::transcript::{Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript};

/// The Qoder harness marker. Shares Claude Code's native [`Record`] body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Qoder;

impl Harness for Qoder {
    const NAME: &'static str = "qoder";
    type Body = Vec<Record>;
}

// ── codec ──────────────────────────────────────────────────────────────

impl Codec for Qoder {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        Ok(Transcript::new(
            transcript.meta.clone(),
            claude_code::records_to_messages(&transcript.body, transcript.meta.timestamp),
        ))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        // Qoder has no `summary` line to anchor a title to, so the donor
        // writes none and the title goes on an `ai-title` line below.
        let untitled = Meta {
            title: None,
            ..transcript.meta.clone()
        };
        let mut records = claude_code::messages_to_records(&untitled, &transcript.body);

        // The donor stamps one session id on every line (a fresh one when
        // `meta.id` is empty); the lines added here must carry the same.
        let session_id = records
            .iter()
            .find_map(|record| match record {
                Record::User(entry) | Record::Assistant(entry) => entry.session_id.clone(),
                _ => None,
            })
            .unwrap_or_else(|| transcript.meta.id.clone());

        if let Some(title) = transcript.meta.title.as_deref().filter(|t| !t.is_empty()) {
            records.insert(
                0,
                Record::Other(json!({
                    "type": "ai-title",
                    "sessionId": session_id,
                    "aiTitle": title,
                })),
            );
        }

        // The last conversation line is the leaf, and its own timestamp is
        // the line's (a trailing local-command line is not a leaf).
        let leaf = records.iter().rev().find_map(|record| match record {
            Record::User(entry) | Record::Assistant(entry) => Some(entry),
            _ => None,
        });
        if let Some(entry) = leaf {
            let millis = entry
                .timestamp
                .as_deref()
                .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                .map(|ts| ts.timestamp_millis());
            records.push(Record::Other(json!({
                "type": "active-leaf",
                "sessionId": session_id,
                "leafUuid": entry.uuid,
                "explicit": false,
                "timestamp": millis,
            })));
        }

        Ok(Transcript::new(transcript.meta.clone(), records))
    }
}

impl TextCodec for Qoder {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let records: Vec<Record> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(claude_code::record_from_line)
            .collect();
        let mut meta = claude_code::meta_from_records(&records);
        overlay_meta(&mut meta, text);
        Ok(Transcript::new(meta, records))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        jsonl::render(&transcript.body)
    }
}

/// The few `Meta` fields Qoder keeps on lines the donor ignores, found in the
/// same order from the raw text on every path — `from_text` and discovery
/// both call this, so `discover` and `load` agree by construction.
#[derive(Deserialize)]
struct SideLine {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(rename = "aiTitle", default)]
    ai_title: Option<String>,
    #[serde(rename = "forkedFrom", default)]
    forked_from: Option<ForkedFrom>,
}

#[derive(Deserialize)]
struct ForkedFrom {
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
}

/// Fill `meta.title` from the generated title (a `custom-title` the donor
/// already read wins) and `meta.lineage` from `forkedFrom`.
fn overlay_meta(meta: &mut Meta, text: &str) {
    let mut ai_title: Option<String> = None;
    let mut parent: Option<String> = None;
    for line in text.lines() {
        // Cheap gate: only these two keys matter, and most lines are large.
        if !(line.contains("\"ai-title\"") || line.contains("\"forkedFrom\"")) {
            continue;
        }
        let Ok(side) = serde_json::from_str::<SideLine>(line) else {
            continue;
        };
        if side.kind.as_deref() == Some("ai-title")
            && let Some(title) = side.ai_title.filter(|t| !t.is_empty())
        {
            ai_title = Some(title);
        }
        if parent.is_none()
            && let Some(id) = side
                .forked_from
                .and_then(|f| f.session_id)
                .filter(|id| !id.is_empty())
        {
            parent = Some(id);
        }
    }
    if meta.title.is_none() {
        meta.title = ai_title;
    }
    if meta.lineage.is_none()
        && let Some(parent) = parent
    {
        meta.lineage = Some(Lineage {
            parent,
            relation: Relation::Fork,
        });
    }
}

/// Session metadata from a shallow scan of the raw text: the donor's
/// user/assistant/title scan plus [`overlay_meta`]. Equivalent to
/// `from_text(text)?.meta`, without building message payloads.
fn meta_from_text(text: &str) -> Meta {
    let mut meta = claude_code::meta_from_text(text);
    overlay_meta(&mut meta, text);
    meta
}

/// Settle the id and lineage once the file is known: an id no record carries
/// is the file stem, and a session is never its own fork parent. Shared by
/// `discover` and `load` so they agree.
fn finish_meta(meta: &mut Meta, path: &Path) {
    if meta.id.is_empty() {
        meta.id = jsonl::file_id(path);
    }
    if meta.lineage.as_ref().is_some_and(|l| l.parent == meta.id) {
        meta.lineage = None;
    }
}

/// The two keys every Qoder line carries, however it is typed.
#[derive(Deserialize)]
struct Sniff {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
}

/// Whether `text` holds at least one record of a Qoder session: a JSON line
/// with a string `type` and a `sessionId`. Bookkeeping-only sessions (opened
/// and closed without a prompt) qualify; an empty or foreign file does not.
fn looks_like_session(text: &str) -> bool {
    text.lines().any(|line| {
        serde_json::from_str::<Sniff>(line)
            .is_ok_and(|sniff| sniff.kind.is_some() && sniff.session_id.is_some())
    })
}

// ── store ──────────────────────────────────────────────────────────────

/// The longest project-directory name Qoder writes before it truncates.
const MAX_DIR_LEN: usize = 200;

/// Reads and writes Qoder sessions under a projects root (default
/// `~/.qoder/projects`).
#[derive(Debug, Clone)]
pub struct QoderStore {
    pub root: PathBuf,
}

impl QoderStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The default projects root: `$QODER_CONFIG_DIR/projects` when set, else
    /// `~/.qoder/projects`.
    #[must_use]
    pub fn default_root() -> Option<Self> {
        std::env::var_os("QODER_CONFIG_DIR")
            .filter(|v| !v.is_empty())
            .map(|dir| Self::new(PathBuf::from(dir).join("projects")))
            .or_else(|| super::home_dir().map(|h| Self::new(h.join(".qoder").join("projects"))))
    }

    /// Every `<root>/<project>/<session>.jsonl`, in path order. Sessions sit
    /// exactly two levels down; the `<session>/` sidecar directories beside
    /// them are not descended into.
    fn session_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        // An unreadable directory lists nothing.
        for project in fs::read_dir(&self.root).into_iter().flatten().flatten() {
            // `file_type` doesn't follow symlinks, so a link can't send the
            // walk out of the root; symlinked session files still list.
            if !project.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            for entry in fs::read_dir(project.path()).into_iter().flatten().flatten() {
                let path = entry.path();
                if !entry.file_type().is_ok_and(|t| t.is_dir())
                    && path.extension().is_some_and(|e| e == "jsonl")
                {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    }
}

impl Store for QoderStore {
    type H = Qoder;
    type Ref = PathBuf;

    fn discover(&self) -> Result<Vec<Discovered<PathBuf>>> {
        if !self.root.is_dir() {
            // A missing root means no sessions, not an error.
            return Ok(Vec::new());
        }
        let files = self.session_files();
        Ok(super::filter_map_parallel(&files, |path| {
            // A file that fails to read, or holds no session record, is
            // skipped: a `.jsonl` extension alone doesn't make a session.
            fs::read_to_string(path)
                .ok()
                .filter(|text| looks_like_session(text))
                .map(|text| {
                    let mut meta = meta_from_text(&text);
                    finish_meta(&mut meta, path);
                    Discovered {
                        meta,
                        reference: path.clone(),
                    }
                })
        }))
    }

    fn load(&self, reference: &PathBuf) -> Result<Transcript<Qoder>> {
        let mut transcript = Qoder::from_text(&fs::read_to_string(reference)?)?;
        finish_meta(&mut transcript.meta, reference);
        Ok(transcript)
    }

    fn save(&self, transcript: &Transcript<Qoder>) -> Result<Saved<PathBuf>> {
        let id = transcript.meta.id.clone();
        super::checked_id_component(Qoder::NAME, &id)?;
        let cwd = transcript.meta.cwd.as_deref().unwrap_or_default();
        // An unknown cwd files under the filesystem root's project (`/` →
        // `-`) rather than loose in the projects root, where neither Qoder
        // nor `discover` would look.
        let project = if cwd.is_empty() {
            "-".to_string()
        } else {
            encode_project_dir(cwd)
        };
        let dir = self.root.join(project);
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{id}.jsonl"));
        fs::write(&path, Qoder::to_text(transcript)?)?;
        Ok(Saved {
            id,
            reference: path,
        })
    }

    /// Removes a Qoder session log, as `qodercli --delete-session` does: the
    /// `.jsonl` only, leaving the `<session>/` sidecar. Guarded on shape and
    /// containment: the reference must be a `.jsonl` file resolving within
    /// `root`, so a foreign or stale reference never removes files outside
    /// the configured projects.
    fn delete(&self, reference: &PathBuf) -> Result<()> {
        if reference.extension().is_none_or(|ext| ext != "jsonl") {
            return Err(Error::Malformed {
                harness: Qoder::NAME,
                detail: format!("not a qoder session file: {}", reference.display()),
            });
        }
        let canon = reference.canonicalize()?;
        let root = self.root.canonicalize()?;
        if canon.strip_prefix(&root).is_err() || canon == root {
            return Err(Error::Malformed {
                harness: Qoder::NAME,
                detail: format!(
                    "refusing to delete outside the projects root: {}",
                    reference.display()
                ),
            });
        }
        // A session sits exactly at `<root>/<project>/<session>.jsonl`, the
        // layout `discover` lists. The reference's own location must match
        // it, and that location is what gets removed: a symlink is unlinked
        // itself, never resolved to a sidecar or another project's session.
        let parent = reference
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .canonicalize()?;
        let name = reference.file_name().ok_or_else(|| Error::Malformed {
            harness: Qoder::NAME,
            detail: format!("not a qoder session file: {}", reference.display()),
        })?;
        if parent.parent() != Some(root.as_path()) {
            return Err(Error::Malformed {
                harness: Qoder::NAME,
                detail: format!(
                    "not a session at <projects root>/<project>/<session>.jsonl: {}",
                    reference.display()
                ),
            });
        }
        Ok(fs::remove_file(parent.join(name))?)
    }

    fn fingerprints(&self, refs: &[PathBuf]) -> Result<HashMap<String, String>> {
        Ok(refs
            .iter()
            .map(|path| {
                (
                    path.to_string_lossy().into_owned(),
                    claude_code::file_fingerprint(path),
                )
            })
            .collect())
    }
}

/// Qoder's project-directory name for `cwd`: every UTF-16 code unit outside
/// `[A-Za-z0-9]` becomes `-` (so a character beyond the BMP becomes two),
/// and past [`MAX_DIR_LEN`] the name is cut and suffixed with a hash of the
/// original path so distinct long paths stay distinct.
fn encode_project_dir(cwd: &str) -> String {
    let mut name = String::with_capacity(cwd.len());
    for c in cwd.chars() {
        if c.is_ascii_alphanumeric() {
            name.push(c);
        } else {
            name.extend(std::iter::repeat_n('-', c.len_utf16()));
        }
    }
    if name.len() <= MAX_DIR_LEN {
        return name;
    }
    // The name is ASCII by now, so truncating by bytes is by characters.
    name.truncate(MAX_DIR_LEN);
    format!("{name}-{}", base36(path_hash(cwd)))
}

/// The hash Qoder suffixes onto a truncated project directory: djb2 with xor
/// (`h = (h * 33) ^ unit`, seeded 5381) over the path's UTF-16 code units in
/// 32-bit signed arithmetic, then the absolute value.
fn path_hash(path: &str) -> u32 {
    let mut hash: i32 = 5381;
    for unit in path.encode_utf16() {
        hash = hash.wrapping_mul(33) ^ i32::from(unit);
    }
    hash.unsigned_abs()
}

fn base36(mut n: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8_lossy(&out).into_owned()
}
