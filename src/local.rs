//! Local sessions across every harness.
//!
//! The seven operations, in terms of this module:
//!
//! | op        | call |
//! |-----------|------|
//! | list      | [`discover`] |
//! | read      | [`Session::read`] |
//! | write     | [`write()`] |
//! | open      | [`resume_command`] (caller execs it) |
//! | translate | [`Session::read`] + [`write()`] |
//! | continue  | [`Session::read`] + [`write()`] + [`resume_command`] |
//! | delete    | [`Session::delete`] |
//!
//! Everything here uses each harness's default on-disk location. For custom
//! roots, use the per-harness [`Store`]s directly.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use chrono::{DateTime, Utc};

use crate::common::{ArtifactSource, Block, Meta};
use crate::harness::{
    amp, antigravity, campfire, claude_code, codex, cowork, cursor, fx, grok, pi,
};

#[cfg(feature = "chatgpt")]
use crate::harness::chatgpt;
#[cfg(feature = "claude_chat")]
use crate::harness::claude_chat;

#[cfg(feature = "hermes")]
use crate::harness::hermes;

#[cfg(feature = "opencode")]
use crate::harness::cursor_desktop;
use crate::{Codec, Common, Error, Harness, HarnessId, Result, Store, Transcript};

#[cfg(feature = "opencode")]
use crate::harness::opencode;

/// One session found on this machine.
pub struct Session {
    pub harness: HarnessId,
    pub meta: Meta,
    /// When the session's backing store last changed — the file mtime for
    /// file-backed sessions. Unlike [`Meta::timestamp`] (when the session
    /// *started*), this moves as the session grows. `None` when the backend
    /// doesn't expose it.
    pub updated_at: Option<DateTime<Utc>>,
    locator: Locator,
}

enum Locator {
    Path(PathBuf),
    #[cfg(feature = "claude_chat")]
    ClaudeChatRemote(claude_chat::ClaudeChatRef),
    #[cfg(feature = "chatgpt")]
    ChatGptRemote(chatgpt::ChatGptRef),
    #[cfg(any(feature = "opencode", feature = "hermes"))]
    Id(String),
}

/// Every local session, newest first. Unreadable stores and sessions are
/// skipped.
#[must_use]
pub fn discover() -> Vec<Session> {
    discover_with(|_, _| {})
}

/// [`discover`], reporting progress: `on_store(harness, sessions_so_far)` is
/// called before each store is scanned.
#[must_use]
pub fn discover_with(mut on_store: impl FnMut(HarnessId, usize)) -> Vec<Session> {
    fn scan<S>(harness: HarnessId, store: Option<S>, out: &mut Vec<Session>)
    where
        S: Store<Ref = PathBuf>,
    {
        // No store (no home directory) or an unreadable one lists nothing.
        let discovered = store.map_or_else(Vec::new, |s| s.discover().unwrap_or_default());
        out.extend(discovered.into_iter().map(|d| Session {
            harness,
            meta: d.meta,
            updated_at: file_mtime(&d.reference),
            locator: Locator::Path(d.reference),
        }));
    }

    let mut out: Vec<Session> = Vec::new();
    on_store(HarnessId::ClaudeCode, out.len());
    scan(
        HarnessId::ClaudeCode,
        claude_code::ClaudeStore::default_root(),
        &mut out,
    );
    // Live web harnesses are deliberately excluded from aggregate discovery.
    on_store(HarnessId::Codex, out.len());
    scan(
        HarnessId::Codex,
        codex::CodexStore::default_root(),
        &mut out,
    );
    on_store(HarnessId::Pi, out.len());
    scan(HarnessId::Pi, pi::PiStore::default_root(), &mut out);
    on_store(HarnessId::Campfire, out.len());
    scan(
        HarnessId::Campfire,
        campfire::CampfireStore::default_root(),
        &mut out,
    );
    on_store(HarnessId::Cursor, out.len());
    scan(
        HarnessId::Cursor,
        cursor::CursorStore::default_root(),
        &mut out,
    );
    on_store(HarnessId::Grok, out.len());
    scan(HarnessId::Grok, grok::GrokStore::default_root(), &mut out);
    on_store(HarnessId::Fx, out.len());
    scan(HarnessId::Fx, fx::FxStore::default_root(), &mut out);
    on_store(HarnessId::Amp, out.len());
    scan(HarnessId::Amp, amp::AmpStore::default_root(), &mut out);
    on_store(HarnessId::Antigravity, out.len());
    scan(
        HarnessId::Antigravity,
        antigravity::AntigravityStore::default_root(),
        &mut out,
    );
    on_store(HarnessId::Cowork, out.len());
    scan(
        HarnessId::Cowork,
        cowork::CoworkStore::default_root(),
        &mut out,
    );

    #[cfg(feature = "hermes")]
    {
        on_store(HarnessId::Hermes, out.len());
        if let Some(store) = hermes::HermesStore::default_root() {
            for d in store.discover().unwrap_or_default() {
                out.push(Session {
                    harness: HarnessId::Hermes,
                    meta: d.meta,
                    // The database doesn't surface a per-session mtime.
                    updated_at: None,
                    locator: Locator::Id(d.reference),
                });
            }
        }
    }

    #[cfg(feature = "opencode")]
    {
        on_store(HarnessId::CursorDesktop, out.len());
        if let Some(store) = cursor_desktop::CursorDesktopStore::default_root() {
            for d in store.discover().unwrap_or_default() {
                out.push(Session {
                    harness: HarnessId::CursorDesktop,
                    meta: d.meta,
                    // The database doesn't surface a per-session mtime.
                    updated_at: None,
                    locator: Locator::Id(d.reference),
                });
            }
        }
        on_store(HarnessId::OpenCode, out.len());
        if let Some(store) = opencode::OpenCodeStore::default_db() {
            for d in store.discover().unwrap_or_default() {
                out.push(Session {
                    harness: HarnessId::OpenCode,
                    meta: d.meta,
                    // The database doesn't surface a per-session mtime.
                    updated_at: None,
                    locator: Locator::Id(d.reference),
                });
            }
        }
    }

    out.sort_by_key(|s| std::cmp::Reverse(s.meta.timestamp));
    out
}

/// Discover one harness, surfacing remote errors when a live source is
/// explicitly requested. Other harnesses retain the tolerant aggregate scan.
///
/// # Errors
/// When the explicitly selected live backend rejects access or changes shape.
pub fn discover_harness(harness: HarnessId) -> Result<Vec<Session>> {
    #[cfg(feature = "claude_chat")]
    if harness == HarnessId::ClaudeChat {
        let mut out = Vec::new();
        discover_claude_chat_into(&mut out)?;
        out.sort_by_key(|session| std::cmp::Reverse(session.meta.timestamp));
        return Ok(out);
    }
    #[cfg(not(feature = "claude_chat"))]
    if harness == HarnessId::ClaudeChat {
        return Err(Error::Remote {
            harness: "claude_chat",
            detail:
                "live Claude Chat support was not compiled in (enable the `claude_chat` feature)"
                    .to_string(),
        });
    }
    #[cfg(feature = "chatgpt")]
    if harness == HarnessId::ChatGpt {
        let mut out = Vec::new();
        discover_chatgpt_into(&mut out)?;
        out.sort_by_key(|session| std::cmp::Reverse(session.meta.timestamp));
        return Ok(out);
    }
    #[cfg(not(feature = "chatgpt"))]
    if harness == HarnessId::ChatGpt {
        return Err(Error::Remote {
            harness: "chatgpt",
            detail: "live ChatGPT support was not compiled in (enable the `chatgpt` feature)"
                .to_string(),
        });
    }
    Ok(discover()
        .into_iter()
        .filter(|session| session.harness == harness)
        .collect())
}

/// The sessions a `--from` selection names: every local harness when
/// `from` is `None`, else that one harness. This is the only path that
/// reaches a live web source, and only when it is named explicitly — an omitted
/// `from` never contacts either one.
///
/// # Errors
/// When the explicitly selected live backend rejects access or changes shape.
pub fn discover_scoped(from: Option<HarnessId>) -> Result<Vec<Session>> {
    match from {
        None => Ok(discover()),
        Some(harness) => discover_harness(harness),
    }
}

#[cfg(feature = "claude_chat")]
fn discover_claude_chat_into(out: &mut Vec<Session>) -> Result<()> {
    let store = claude_chat::ClaudeChatStore::from_desktop()?;
    for discovered in Store::discover(&store)? {
        out.push(Session {
            harness: HarnessId::ClaudeChat,
            meta: discovered.meta,
            updated_at: discovered.reference.updated_at,
            locator: Locator::ClaudeChatRemote(discovered.reference),
        });
    }
    Ok(())
}

#[cfg(feature = "chatgpt")]
fn discover_chatgpt_into(out: &mut Vec<Session>) -> Result<()> {
    let store = chatgpt::ChatGptStore::from_codex()?;
    for discovered in Store::discover(&store)? {
        out.push(Session {
            harness: HarnessId::ChatGpt,
            meta: discovered.meta,
            updated_at: discovered.reference.updated_at,
            locator: Locator::ChatGptRemote(discovered.reference),
        });
    }
    Ok(())
}

fn file_mtime(path: &Path) -> Option<DateTime<Utc>> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(DateTime::<Utc>::from(modified))
}

impl Session {
    /// Where the session lives, for display: a path, or a database id.
    #[must_use]
    pub fn location(&self) -> String {
        match &self.locator {
            Locator::Path(p) => p.display().to_string(),
            #[cfg(feature = "claude_chat")]
            Locator::ClaudeChatRemote(reference) => {
                format!("https://claude.ai/chat/{}", reference.conversation_uuid)
            }
            #[cfg(feature = "chatgpt")]
            Locator::ChatGptRemote(reference) => {
                format!("https://chatgpt.com/c/{}", reference.conversation_id)
            }
            #[cfg(feature = "opencode")]
            Locator::Id(id) => format!("{} db session {id}", self.harness),
        }
    }

    /// Load and convert to the canonical model.
    ///
    /// # Errors
    /// When the session no longer exists, doesn't parse, or its store's
    /// backend is unavailable.
    pub fn read(&self) -> Result<Transcript<Common>> {
        fn go<S>(store: Option<S>, path: &PathBuf) -> Result<Transcript<Common>>
        where
            S: Store<Ref = PathBuf>,
            S::H: Codec,
        {
            <S::H as Codec>::to_common(&required(store)?.load(path)?)
        }
        match (&self.harness, &self.locator) {
            (HarnessId::ClaudeCode, Locator::Path(p)) => {
                go(claude_code::ClaudeStore::default_root(), p)
            }
            #[cfg(feature = "claude_chat")]
            (HarnessId::ClaudeChat, Locator::ClaudeChatRemote(reference)) => {
                let store = claude_chat::ClaudeChatStore::from_desktop()?;
                claude_chat::ClaudeChat::to_common(&store.load(reference)?)
            }
            #[cfg(feature = "chatgpt")]
            (HarnessId::ChatGpt, Locator::ChatGptRemote(reference)) => {
                let store = chatgpt::ChatGptStore::from_codex()?;
                chatgpt::ChatGpt::to_common(&store.load(reference)?)
            }
            (HarnessId::Codex, Locator::Path(p)) => go(codex::CodexStore::default_root(), p),
            (HarnessId::Pi, Locator::Path(p)) => go(pi::PiStore::default_root(), p),
            (HarnessId::Campfire, Locator::Path(p)) => {
                go(campfire::CampfireStore::default_root(), p)
            }
            (HarnessId::Cursor, Locator::Path(p)) => go(cursor::CursorStore::default_root(), p),
            (HarnessId::Grok, Locator::Path(p)) => go(grok::GrokStore::default_root(), p),
            (HarnessId::Fx, Locator::Path(p)) => go(fx::FxStore::default_root(), p),
            (HarnessId::Amp, Locator::Path(p)) => go(amp::AmpStore::default_root(), p),
            (HarnessId::Antigravity, Locator::Path(p)) => {
                go(antigravity::AntigravityStore::default_root(), p)
            }
            (HarnessId::Cowork, Locator::Path(p)) => go(cowork::CoworkStore::default_root(), p),
            #[cfg(feature = "hermes")]
            (HarnessId::Hermes, Locator::Id(id)) => {
                hermes::Hermes::to_common(&required(hermes::HermesStore::default_root())?.load(id)?)
            }
            #[cfg(feature = "opencode")]
            (HarnessId::CursorDesktop, Locator::Id(id)) => {
                let store = required(cursor_desktop::CursorDesktopStore::default_root())?;
                cursor_desktop::CursorDesktop::to_common(&store.load(id)?)
            }
            #[cfg(feature = "opencode")]
            (HarnessId::OpenCode, Locator::Id(id)) => opencode::OpenCode::to_common(
                &required(opencode::OpenCodeStore::default_db())?.load(id)?,
            ),
            _ => Err(mismatch(self.harness)),
        }
    }

    /// Remove the session from its harness (see [`Store::delete`] for what
    /// removal means per backend).
    ///
    /// # Errors
    /// When the session no longer exists or the backend rejects the removal.
    pub fn delete(&self) -> Result<()> {
        fn go<S>(store: Option<S>, path: &PathBuf) -> Result<()>
        where
            S: Store<Ref = PathBuf>,
        {
            required(store)?.delete(path)
        }
        match (&self.harness, &self.locator) {
            (HarnessId::ClaudeCode, Locator::Path(p)) => {
                go(claude_code::ClaudeStore::default_root(), p)
            }
            #[cfg(feature = "claude_chat")]
            (HarnessId::ClaudeChat, Locator::ClaudeChatRemote(reference)) => {
                claude_chat::ClaudeChatStore::from_desktop()?.delete(reference)
            }
            #[cfg(feature = "chatgpt")]
            (HarnessId::ChatGpt, Locator::ChatGptRemote(reference)) => {
                chatgpt::ChatGptStore::from_codex()?.delete(reference)
            }
            (HarnessId::Codex, Locator::Path(p)) => go(codex::CodexStore::default_root(), p),
            (HarnessId::Pi, Locator::Path(p)) => go(pi::PiStore::default_root(), p),
            (HarnessId::Campfire, Locator::Path(p)) => {
                go(campfire::CampfireStore::default_root(), p)
            }
            (HarnessId::Cursor, Locator::Path(p)) => go(cursor::CursorStore::default_root(), p),
            (HarnessId::Grok, Locator::Path(p)) => go(grok::GrokStore::default_root(), p),
            (HarnessId::Fx, Locator::Path(p)) => go(fx::FxStore::default_root(), p),
            (HarnessId::Amp, Locator::Path(p)) => go(amp::AmpStore::default_root(), p),
            (HarnessId::Antigravity, Locator::Path(p)) => {
                go(antigravity::AntigravityStore::default_root(), p)
            }
            (HarnessId::Cowork, Locator::Path(p)) => go(cowork::CoworkStore::default_root(), p),
            // Errors: the Hermes store is read-only.
            #[cfg(feature = "hermes")]
            (HarnessId::Hermes, Locator::Id(id)) => {
                required(hermes::HermesStore::default_root())?.delete(id)
            }
            #[cfg(feature = "opencode")]
            (HarnessId::CursorDesktop, Locator::Id(id)) => {
                required(cursor_desktop::CursorDesktopStore::default_root())?.delete(id)
            }
            #[cfg(feature = "opencode")]
            (HarnessId::OpenCode, Locator::Id(id)) => {
                required(opencode::OpenCodeStore::default_db())?.delete(id)
            }
            _ => Err(mismatch(self.harness)),
        }
    }

    /// The command that resumes this session in its own harness.
    #[must_use]
    pub fn resume_command(&self) -> (String, Vec<String>) {
        resume_command(self.harness, &self.meta.id)
    }
}

/// Per-session change cursors, positionally aligned with `sessions` — the
/// batched form of [`Store::fingerprints`] across every harness, for callers
/// that cache parsed transcripts and want to know which ones to re-read.
/// A cursor is an opaque string that changes whenever the session's backing
/// data does; an empty string means the backend could not establish one, and
/// a cache should treat the session as changed.
///
/// File-backed sessions fall back to the file's mtime and size when their
/// store reports nothing. Stores are queried once per harness.
#[must_use]
pub fn fingerprints(sessions: &[Session]) -> Vec<String> {
    let mut by_harness: HashMap<HarnessId, Vec<usize>> = HashMap::new();
    for (i, s) in sessions.iter().enumerate() {
        by_harness.entry(s.harness).or_default().push(i);
    }

    let mut out = vec![String::new(); sessions.len()];
    for (harness, at) in by_harness {
        let group = Group {
            at: &at,
            sessions,
            out: &mut out,
        };
        match harness {
            HarnessId::ClaudeCode => group.files(claude_code::ClaudeStore::default_root()),
            #[cfg(feature = "claude_chat")]
            HarnessId::ClaudeChat => {
                group.claude_chat_remote(claude_chat::ClaudeChatStore::from_desktop());
            }
            #[cfg(feature = "chatgpt")]
            HarnessId::ChatGpt => {
                group.chatgpt_remote(chatgpt::ChatGptStore::from_codex());
            }
            HarnessId::Codex => group.files(codex::CodexStore::default_root()),
            HarnessId::Pi => group.files(pi::PiStore::default_root()),
            HarnessId::Campfire => group.files(campfire::CampfireStore::default_root()),
            HarnessId::Cursor => group.files(cursor::CursorStore::default_root()),
            HarnessId::Grok => group.files(grok::GrokStore::default_root()),
            HarnessId::Fx => group.files(fx::FxStore::default_root()),
            HarnessId::Amp => group.files(amp::AmpStore::default_root()),
            HarnessId::Antigravity => group.files(antigravity::AntigravityStore::default_root()),
            HarnessId::Cowork => group.files(cowork::CoworkStore::default_root()),
            #[cfg(feature = "hermes")]
            HarnessId::Hermes => group.ids(hermes::HermesStore::default_root()),
            #[cfg(feature = "opencode")]
            HarnessId::CursorDesktop => {
                group.ids(cursor_desktop::CursorDesktopStore::default_root());
            }
            #[cfg(feature = "opencode")]
            HarnessId::OpenCode => group.ids(opencode::OpenCodeStore::default_db()),
            // Not discovered (no store), or compiled out: no cursor.
            _ => {}
        }
    }
    out
}

/// One harness's slice of a [`fingerprints`] call: which sessions, and where
/// their cursors go.
struct Group<'a> {
    at: &'a [usize],
    sessions: &'a [Session],
    out: &'a mut [String],
}

impl Group<'_> {
    fn files<S>(self, store: Option<S>)
    where
        S: Store<Ref = PathBuf>,
    {
        let paths: Vec<PathBuf> = self
            .at
            .iter()
            .filter_map(|&i| self.sessions[i].path().cloned())
            .collect();
        let by_path = store
            .and_then(|s| s.fingerprints(&paths).ok())
            .unwrap_or_default();
        for &i in self.at {
            if let Some(p) = self.sessions[i].path() {
                let known = by_path
                    .get(&p.to_string_lossy().into_owned())
                    .cloned()
                    .unwrap_or_default();
                self.out[i] = if known.is_empty() {
                    claude_code::file_fingerprint(p)
                } else {
                    known
                };
            }
        }
    }

    #[cfg(feature = "claude_chat")]
    fn claude_chat_remote(self, store: Result<claude_chat::ClaudeChatStore>) {
        let refs: Vec<claude_chat::ClaudeChatRef> = self
            .at
            .iter()
            .filter_map(|&index| self.sessions[index].claude_chat_remote().cloned())
            .collect();
        let by_ref = store
            .ok()
            .and_then(|store| store.fingerprints(&refs).ok())
            .unwrap_or_default();
        for &index in self.at {
            if let Some(reference) = self.sessions[index].claude_chat_remote() {
                self.out[index] = by_ref.get(&reference.key()).cloned().unwrap_or_default();
            }
        }
    }

    #[cfg(feature = "chatgpt")]
    fn chatgpt_remote(self, store: Result<chatgpt::ChatGptStore>) {
        let refs: Vec<chatgpt::ChatGptRef> = self
            .at
            .iter()
            .filter_map(|&index| self.sessions[index].chatgpt_remote().cloned())
            .collect();
        let by_ref = store
            .ok()
            .and_then(|store| store.fingerprints(&refs).ok())
            .unwrap_or_default();
        for &index in self.at {
            if let Some(reference) = self.sessions[index].chatgpt_remote() {
                self.out[index] = by_ref.get(&reference.key()).cloned().unwrap_or_default();
            }
        }
    }

    #[cfg(any(feature = "opencode", feature = "hermes"))]
    fn ids<S>(self, store: Option<S>)
    where
        S: Store<Ref = String>,
    {
        let ids: Vec<String> = self
            .at
            .iter()
            .filter_map(|&i| self.sessions[i].id().cloned())
            .collect();
        let by_id = store
            .and_then(|s| s.fingerprints(&ids).ok())
            .unwrap_or_default();
        for &i in self.at {
            if let Some(id) = self.sessions[i].id() {
                self.out[i] = by_id.get(id).cloned().unwrap_or_default();
            }
        }
    }
}

impl Session {
    /// The file behind a file-backed session.
    // Option for symmetry with `id`: without the SQLite features every
    // session is file-backed and the None arm compiles out.
    #[allow(clippy::unnecessary_wraps)]
    fn path(&self) -> Option<&PathBuf> {
        match &self.locator {
            Locator::Path(p) => Some(p),
            #[cfg(feature = "claude_chat")]
            Locator::ClaudeChatRemote(_) => None,
            #[cfg(feature = "chatgpt")]
            Locator::ChatGptRemote(_) => None,
            #[cfg(any(feature = "opencode", feature = "hermes"))]
            Locator::Id(_) => None,
        }
    }

    #[cfg(feature = "claude_chat")]
    fn claude_chat_remote(&self) -> Option<&claude_chat::ClaudeChatRef> {
        match &self.locator {
            Locator::ClaudeChatRemote(reference) => Some(reference),
            Locator::Path(_) => None,
            #[cfg(feature = "chatgpt")]
            Locator::ChatGptRemote(_) => None,
            #[cfg(any(feature = "opencode", feature = "hermes"))]
            Locator::Id(_) => None,
        }
    }

    #[cfg(feature = "chatgpt")]
    fn chatgpt_remote(&self) -> Option<&chatgpt::ChatGptRef> {
        match &self.locator {
            Locator::ChatGptRemote(reference) => Some(reference),
            Locator::Path(_) => None,
            #[cfg(feature = "claude_chat")]
            Locator::ClaudeChatRemote(_) => None,
            #[cfg(any(feature = "opencode", feature = "hermes"))]
            Locator::Id(_) => None,
        }
    }

    /// The database id behind an id-backed session.
    #[cfg(any(feature = "opencode", feature = "hermes"))]
    fn id(&self) -> Option<&String> {
        match &self.locator {
            Locator::Id(id) => Some(id),
            Locator::Path(_) => None,
            #[cfg(feature = "claude_chat")]
            Locator::ClaudeChatRemote(_) => None,
            #[cfg(feature = "chatgpt")]
            Locator::ChatGptRemote(_) => None,
        }
    }
}

/// The outcome of [`write()`]: the id the target harness resumes by, and a
/// human-readable location.
pub struct Written {
    pub id: String,
    pub location: String,
}

/// Persist a canonical transcript in `target`'s native, resumable format.
/// `root` overrides the harness's default on-disk root (file-backed stores
/// only; `OpenCode` always goes through `opencode import` into the live
/// database, so a root override for it is refused rather than silently
/// redirected).
///
/// # Errors
/// When conversion to the target fails or its store rejects the write.
// One dispatch arm per harness; length grows with the harness count, not
// with complexity.
#[allow(clippy::too_many_lines)]
pub fn write(
    target: HarnessId,
    common: &Transcript<Common>,
    root: Option<&Path>,
) -> Result<Written> {
    fn go<S>(
        store: Option<S>,
        make: impl FnOnce(PathBuf) -> S,
        root: Option<&Path>,
        common: &Transcript<Common>,
        default_dir: impl FnOnce(S) -> PathBuf,
    ) -> Result<Written>
    where
        S: Store,
        S::H: Codec,
        S::Ref: std::fmt::Debug,
    {
        let store = match root {
            Some(dir) => make(dir.to_path_buf()),
            None => make(default_dir(required(store)?)),
        };
        let native = <S::H as Codec>::from_common(common)?;
        let saved = store.save(&native)?;
        Ok(Written {
            id: saved.id,
            location: format!("{:?}", saved.reference),
        })
    }

    match target {
        HarnessId::ClaudeCode => write_claude_code(common, root),
        // Live web sources are server-authoritative and have no import. Their
        // additional in-place-resume refusals live in the CLI because that
        // path deliberately bypasses `write` for existing sessions.
        HarnessId::ClaudeChat => Err(Error::Unconvertible {
            harness: "claude_chat",
            detail: "Claude Chat is a live read-only source; sessions can be pulled out and converted into another harness, but never continued into Claude"
                .to_string(),
        }),
        HarnessId::ChatGpt => Err(Error::Unconvertible {
            harness: "chatgpt",
            detail: "ChatGPT is a live read-only source; sessions can be pulled out and converted into another harness, but never continued into ChatGPT"
                .to_string(),
        }),
        HarnessId::Codex => go(
            codex::CodexStore::default_root(),
            codex::CodexStore::new,
            root,
            common,
            |s| s.sessions_dir,
        ),
        HarnessId::Pi => go(
            pi::PiStore::default_root(),
            pi::PiStore::new,
            root,
            common,
            |s| s.sessions_dir,
        ),
        HarnessId::Campfire => go(
            campfire::CampfireStore::default_root(),
            campfire::CampfireStore::new,
            root,
            common,
            |s| s.sessions_dir,
        ),
        HarnessId::Cursor => go(
            cursor::CursorStore::default_root(),
            cursor::CursorStore::new,
            root,
            common,
            |s| s.chats_dir,
        ),
        HarnessId::Grok => go(
            grok::GrokStore::default_root(),
            grok::GrokStore::new,
            root,
            common,
            |s| s.sessions_dir,
        ),
        HarnessId::Fx => go(
            fx::FxStore::default_root(),
            fx::FxStore::new,
            root,
            common,
            |s| s.sessions_dir,
        ),
        // Hermes's state.db is read-only in txcript and Hermes has no
        // session-import command. Sessions convert *from* Hermes, never
        // into it.
        HarnessId::Hermes => Err(Error::Unconvertible {
            harness: "hermes",
            detail: "Hermes state.db is read-only and Hermes has no session \
                     import command; sessions can be converted from Hermes, \
                     but not continued into it"
                .to_string(),
        }),
        // Amp is server-authoritative: threads live on ampcode.com and the
        // CLI has no import, so a locally written thread can never be
        // resumed. Sessions convert *from* amp, never into it.
        HarnessId::Amp => Err(Error::Unconvertible {
            harness: "amp",
            detail: "amp has no thread import (threads are server-side); \
                     sessions cannot be continued into amp — convert from amp \
                     instead"
                .to_string(),
        }),
        HarnessId::Antigravity => go(
            antigravity::AntigravityStore::default_root(),
            antigravity::AntigravityStore::new,
            root,
            common,
            |s| s.root,
        ),
        HarnessId::Cowork => go(
            cowork::CoworkStore::default_root(),
            cowork::CoworkStore::new,
            root,
            common,
            |s| s.root,
        ),
        // Simple is an interchange *input*: documents are handed to txcript
        // directly (a file, stdin, the WASM text API) rather than managed in
        // a directory of its own, so there is nowhere to write one back to.
        HarnessId::Simple => Err(Error::Unconvertible {
            harness: "simple",
            detail: "simple is an interchange input; sessions are continued \
                     from Simple documents, not written into them"
                .to_string(),
        }),
        // `opencode import` writes into the live database wherever it lives;
        // honoring a root override is impossible, and ignoring it would send
        // an "export" into the user's real store.
        HarnessId::OpenCode if root.is_some() => Err(Error::Unconvertible {
            harness: "opencode",
            detail: "opencode sessions can only be imported into the live \
                     opencode database (via `opencode import`); writing to \
                     an alternate directory is not supported"
                .to_string(),
        }),
        HarnessId::OpenCode => write_opencode(common),
        // The desktop app reads one fixed database; a root override names an
        // alternate `User` directory rather than a session directory.
        HarnessId::CursorDesktop => write_cursor_desktop(common, root),
    }
}

fn write_claude_code(common: &Transcript<Common>, root: Option<&Path>) -> Result<Written> {
    let store = match root {
        Some(dir) => claude_code::ClaudeStore::new(dir.to_path_buf()),
        None => required(claude_code::ClaudeStore::default_root())?,
    };
    let prepared = materialize_artifacts_for_claude_code(common, &store.root)?;
    let native = claude_code::ClaudeCode::from_common(&prepared)?;
    let saved = store.save(&native)?;
    Ok(Written {
        id: saved.id,
        location: saved.reference.display().to_string(),
    })
}

fn materialize_artifacts_for_claude_code(
    common: &Transcript<Common>,
    projects_root: &Path,
) -> Result<Transcript<Common>> {
    const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;

    crate::harness::checked_id_component(claude_code::ClaudeCode::NAME, &common.meta.id)?;
    let artifact_root = projects_root
        .join(claude_code::encode_project_dir(
            common.meta.cwd.as_deref().unwrap_or_default(),
        ))
        .join(&common.meta.id)
        .join("artifacts");
    let mut prepared = common.clone();
    for (message_index, message) in prepared.body.iter_mut().enumerate() {
        for (block_index, block) in message.content.iter_mut().enumerate() {
            let Block::Artifact { artifact } = block else {
                continue;
            };
            let (bytes, media_type) = match &artifact.source {
                ArtifactSource::Path { .. } => continue,
                ArtifactSource::Text { text, media_type } => {
                    if text.len() > MAX_ARTIFACT_BYTES {
                        return Err(artifact_error("artifact exceeds the size limit"));
                    }
                    (text.as_bytes().to_vec(), media_type.clone())
                }
                ArtifactSource::Base64 { data, media_type } => {
                    let max_encoded = MAX_ARTIFACT_BYTES
                        .saturating_mul(4)
                        .saturating_div(3)
                        .saturating_add(4);
                    if data.len() > max_encoded {
                        return Err(artifact_error("artifact exceeds the size limit"));
                    }
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .map_err(|_| artifact_error("artifact contains invalid base64"))?;
                    if bytes.len() > MAX_ARTIFACT_BYTES {
                        return Err(artifact_error("artifact exceeds the size limit"));
                    }
                    (bytes, media_type.clone())
                }
            };
            let directory = artifact_root.join(format!("{message_index}-{block_index}"));
            std::fs::create_dir_all(&directory)?;
            let path = directory.join(safe_artifact_name(&artifact.name));
            std::fs::write(&path, bytes)?;
            let path = std::path::absolute(&path).unwrap_or(path);
            artifact.source = ArtifactSource::Path {
                path: path.to_string_lossy().into_owned(),
                media_type,
            };
        }
    }
    Ok(prepared)
}

fn safe_artifact_name(name: &str) -> String {
    let mut safe = String::new();
    for character in name.trim().chars() {
        let character =
            if character.is_control() || matches!(character, '/' | '\\' | '`' | ':' | '\0') {
                '_'
            } else {
                character
            };
        if safe.len().saturating_add(character.len_utf8()) > 180 {
            break;
        }
        safe.push(character);
    }
    if safe.is_empty() || matches!(safe.as_str(), "." | "..") {
        "artifact".to_string()
    } else {
        safe
    }
}

fn artifact_error(detail: &str) -> Error {
    Error::Malformed {
        harness: claude_code::ClaudeCode::NAME,
        detail: detail.to_string(),
    }
}

#[cfg(feature = "opencode")]
fn write_cursor_desktop(common: &Transcript<Common>, root: Option<&Path>) -> Result<Written> {
    let store = match root {
        Some(dir) => cursor_desktop::CursorDesktopStore::new(dir.to_path_buf()),
        None => required(cursor_desktop::CursorDesktopStore::default_root())?,
    };
    let native = cursor_desktop::CursorDesktop::from_common(common)?;
    let saved = store.save(&native)?;
    Ok(Written {
        id: saved.id,
        location: store.db_display(),
    })
}

#[cfg(not(feature = "opencode"))]
fn write_cursor_desktop(_: &Transcript<Common>, _: Option<&Path>) -> Result<Written> {
    Err(Error::Unconvertible {
        harness: "cursor_desktop",
        detail: "Cursor desktop support not compiled in (enable the `opencode` feature)"
            .to_string(),
    })
}

#[cfg(feature = "opencode")]
fn write_opencode(common: &Transcript<Common>) -> Result<Written> {
    let store = required(opencode::OpenCodeStore::default_db())?;
    let native = opencode::OpenCode::from_common(common)?;
    let saved = store.save(&native)?;
    Ok(Written {
        id: saved.id,
        location: "imported via `opencode import`".to_string(),
    })
}

#[cfg(not(feature = "opencode"))]
fn write_opencode(_: &Transcript<Common>) -> Result<Written> {
    Err(Error::Unconvertible {
        harness: "opencode",
        detail: "opencode support not compiled in (enable the `opencode` feature)".to_string(),
    })
}

/// The command that resumes session `id` in `harness` — `(binary, args)`,
/// for the caller to exec or spawn. Overridable per harness with
/// `TRANSCRIPT_<HARNESS>_RESUME_CMD`, a template where `{id}` is substituted
/// (e.g. `TRANSCRIPT_CODEX_RESUME_CMD="codex resume {id}"`).
#[must_use]
pub fn resume_command(harness: HarnessId, id: &str) -> (String, Vec<String>) {
    let key = format!(
        "TRANSCRIPT_{}_RESUME_CMD",
        harness.as_str().to_ascii_uppercase()
    );
    let overridden = std::env::var(&key)
        .ok()
        .and_then(|template| apply_resume_template(&template, id));
    overridden.unwrap_or_else(|| {
        let id = id.to_string();
        match harness {
            HarnessId::ClaudeCode => ("claude".into(), vec!["--resume".into(), id]),
            // Source-only harnesses: the CLI refuses before this fallback.
            HarnessId::ClaudeChat | HarnessId::ChatGpt | HarnessId::Simple => {
                ("txcript".into(), Vec::new())
            }
            HarnessId::Codex => ("codex".into(), vec!["resume".into(), id]),
            HarnessId::OpenCode => ("opencode".into(), vec!["--session".into(), id]),
            HarnessId::Pi => ("pi".into(), vec!["--session".into(), id]),
            HarnessId::Campfire => ("campfire".into(), vec!["--session".into(), id]),
            HarnessId::Cursor => ("agent".into(), vec![format!("--resume={id}")]),
            // No per-session CLI entry point: `cursor` opens the IDE, where
            // the session is in the Agents sidebar.
            HarnessId::CursorDesktop => ("cursor".into(), Vec::new()),
            HarnessId::Grok => ("grok".into(), vec!["--resume".into(), id]),
            HarnessId::Fx => ("fx".into(), vec!["--resume".into(), id]),
            HarnessId::Hermes => ("hermes".into(), vec!["--resume".into(), id]),
            HarnessId::Amp => ("amp".into(), vec!["threads".into(), "continue".into(), id]),
            HarnessId::Antigravity => ("agy".into(), vec![format!("--conversation={id}")]),
            // No per-session entry point: the Claude desktop app lists the
            // session under Cowork once it restarts or rescans.
            HarnessId::Cowork if cfg!(target_os = "macos") => {
                ("open".into(), vec!["-a".into(), "Claude".into()])
            }
            HarnessId::Cowork => ("Claude".into(), Vec::new()),
        }
    })
}

/// Expand a `TRANSCRIPT_<HARNESS>_RESUME_CMD` template into `(binary, args)`.
///
/// The template is split into argv entries *before* `{id}` is substituted:
/// the id comes from the session file, so an id containing whitespace must
/// stay a single argument rather than becoming extra flags. Empty templates
/// expand to nothing (the override is ignored).
fn apply_resume_template(template: &str, id: &str) -> Option<(String, Vec<String>)> {
    let mut parts = template
        .split_whitespace()
        .map(|part| part.replace("{id}", id))
        .collect::<Vec<_>>()
        .into_iter();
    parts.next().map(|bin| (bin, parts.collect()))
}

fn required<S>(store: Option<S>) -> Result<S> {
    store.ok_or_else(|| {
        Error::Io(std::io::Error::other(
            "no home directory; use the harness Store directly with an explicit root",
        ))
    })
}

fn mismatch(harness: HarnessId) -> Error {
    Error::Malformed {
        harness: "local",
        detail: format!("locator does not belong to {harness}"),
    }
}

#[cfg(test)]
mod live_remote_gate_tests {
    use super::{HarnessId, discover_scoped, discover_with};

    #[test]
    fn aggregate_discovery_never_reaches_claude_chat() {
        let mut scanned = Vec::new();
        let sessions = discover_with(|harness, _| scanned.push(harness));
        assert!(!scanned.contains(&HarnessId::ClaudeChat));
        assert!(!scanned.contains(&HarnessId::ChatGpt));
        assert!(sessions.iter().all(|s| s.harness != HarnessId::ClaudeChat));
        assert!(sessions.iter().all(|s| s.harness != HarnessId::ChatGpt));
    }

    #[test]
    fn an_omitted_from_never_yields_live_web_sessions() {
        let sessions = discover_scoped(None).unwrap_or_else(|e| panic!("{e}"));
        assert!(sessions.iter().all(|s| s.harness != HarnessId::ClaudeChat));
        assert!(sessions.iter().all(|s| s.harness != HarnessId::ChatGpt));
    }

    #[test]
    fn another_harness_as_from_never_yields_live_web_sessions() {
        let sessions = discover_scoped(Some(HarnessId::Codex)).unwrap_or_else(|e| panic!("{e}"));
        assert!(sessions.iter().all(|s| s.harness == HarnessId::Codex));
    }
}

#[cfg(test)]
mod resume_template_tests {
    use super::apply_resume_template;

    #[test]
    fn id_is_substituted_after_argv_splitting() {
        let (bin, args) =
            apply_resume_template("claude --resume {id}", "abc --dangerously-skip-permissions")
                .unwrap_or_else(|| panic!("template expands"));
        assert_eq!(bin, "claude");
        // The whole id — spaces, injected flag and all — is one argument.
        assert_eq!(
            args,
            vec![
                "--resume".to_string(),
                "abc --dangerously-skip-permissions".to_string()
            ]
        );
    }

    #[test]
    fn id_embedded_in_a_flag_stays_one_argument() {
        let (bin, args) = apply_resume_template("agent --resume={id}", "a b c")
            .unwrap_or_else(|| panic!("template expands"));
        assert_eq!(bin, "agent");
        assert_eq!(args, vec!["--resume=a b c".to_string()]);
    }

    #[test]
    fn empty_template_expands_to_nothing() {
        assert!(apply_resume_template("   ", "id").is_none());
    }
}
