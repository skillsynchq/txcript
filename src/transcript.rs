//! The generic [`Transcript<H>`] and the three traits that act on it:
//! [`Harness`] (what representation a transcript is in), [`Codec`] (mapping a
//! native representation to and from [`Common`]), and [`Store`] (procuring and
//! persisting native transcripts against a real backend).

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::common::{Message, Meta};
use crate::error::Result;

/// A transcript in some representation `H`.
///
/// `H` selects the body type: [`Common`] holds `Vec<Message>`, the canonical
/// model; a harness marker holds that harness's faithful native records. `meta`
/// is always the cross-harness [`Meta`]; harness-specific header detail lives
/// inside `body`.
pub struct Transcript<H: Harness = Common> {
    pub meta: Meta,
    pub body: H::Body,
}

impl<H: Harness> Transcript<H> {
    pub fn new(meta: Meta, body: H::Body) -> Self {
        Self { meta, body }
    }
}

// Hand-written because deriving would wrongly demand `H: Clone`/`Debug`/`Eq`;
// the bounds belong on the associated `Body`, not the marker `H`.
impl<H: Harness> Clone for Transcript<H>
where
    H::Body: Clone,
{
    fn clone(&self) -> Self {
        Self {
            meta: self.meta.clone(),
            body: self.body.clone(),
        }
    }
}

impl<H: Harness> fmt::Debug for Transcript<H>
where
    H::Body: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transcript")
            .field("harness", &H::NAME)
            .field("meta", &self.meta)
            .field("body", &self.body)
            .finish()
    }
}

impl<H: Harness> PartialEq for Transcript<H>
where
    H::Body: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.meta == other.meta && self.body == other.body
    }
}

/// A transcript representation. Implemented by [`Common`] and by each harness
/// marker. The marker is a zero-size type; the representation is its `Body`.
pub trait Harness {
    /// Stable lowercase identifier, e.g. `"common"`, `"claude_code"`, `"codex"`.
    const NAME: &'static str;

    /// The body representation for this harness. `Common::Body = Vec<Message>`;
    /// a harness's `Body` is its faithful native record set.
    type Body;
}

/// The canonical hub representation. Every cross-harness conversion routes
/// through `Transcript<Common>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Common;

impl Harness for Common {
    const NAME: &'static str = "common";
    type Body = Vec<Message>;
}

/// A half-open range of message indices: the primitive for pointing at part
/// of a session. Owned and serializable, so it crosses process and wire
/// boundaries (search results, CLI arguments, MCP responses); resolution
/// against a loaded transcript is [`Transcript::fragment`].
///
/// Indices are positions in the parsed snapshot the span was minted against.
/// They stay valid as a live session appends; they are not stable across
/// cross-harness conversion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span(pub Range<usize>);

impl Transcript<Common> {
    /// Resolve a [`Span`] to its messages, borrowing from this transcript.
    /// `None` when the span reaches past the end of the session.
    #[must_use]
    pub fn fragment(&self, span: &Span) -> Option<&[Message]> {
        self.body.get(span.0.clone())
    }
}

/// Maps a harness's native representation to and from [`Common`].
///
/// `to_common` may *canonicalize* representation but must not *discard*
/// detail — anything a same-harness round trip needs is preserved in
/// [`Common`]'s typed fields. The `to_common`→`from_common` guarantee is
/// semantic equality, not byte equality; byte-exactness lives at the
/// native ↔ disk boundary in [`Store`].
pub trait Codec: Harness + Sized {
    /// # Errors
    /// When the native records are malformed beyond the raw-fallback layer.
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>>;
    /// # Errors
    /// When this harness cannot represent the transcript.
    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>>;
}

impl Codec for Common {
    fn to_common(transcript: &Transcript<Common>) -> Result<Transcript<Common>> {
        Ok(transcript.clone())
    }
    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Common>> {
        Ok(transcript.clone())
    }
}

/// Convert a transcript from one harness to another through the [`Common`] hub.
///
/// ```ignore
/// let codex_session = convert::<ClaudeCode, Codex>(&claude_session)?;
/// ```
///
/// # Errors
/// When `A` cannot parse its records or `B` cannot represent the transcript.
pub fn convert<A, B>(transcript: &Transcript<A>) -> Result<Transcript<B>>
where
    A: Codec,
    B: Codec,
{
    B::from_common(&A::to_common(transcript)?)
}

/// Parsing and rendering a harness's native session *text*, free of any
/// filesystem or database. [`Store`] layers location on top of it; the WASM
/// bindings use it directly.
pub trait TextCodec: Harness + Sized {
    /// Parse native session text into a transcript. `meta.id` may be empty when
    /// the text carries no internal id; a [`Store`] fills it from the filename.
    ///
    /// # Errors
    /// When the text is not this harness's session format.
    fn from_text(text: &str) -> Result<Transcript<Self>>;

    /// Render a transcript back to native session text.
    ///
    /// # Errors
    /// When the records cannot be serialized.
    fn to_text(transcript: &Transcript<Self>) -> Result<String>;
}

/// Reading and writing native transcripts against a real backend (a session
/// directory, a `SQLite` database, an `import` subprocess).
pub trait Store {
    /// The harness this store reads and writes.
    type H: Harness;
    /// A locator for one transcript at rest: a file path, a database id, a slug.
    type Ref;

    /// Cheap metadata scan — no full message parsing.
    ///
    /// # Errors
    /// When the backend itself fails; a missing root is `Ok(vec![])`.
    fn discover(&self) -> Result<Vec<Discovered<Self::Ref>>>;

    /// Load and parse one transcript into its faithful native representation.
    ///
    /// # Errors
    /// When the reference doesn't exist or its content doesn't parse.
    fn load(&self, reference: &Self::Ref) -> Result<Transcript<Self::H>>;

    /// Persist a native transcript so the harness can resume it.
    ///
    /// # Errors
    /// When the backend rejects the write.
    fn save(&self, transcript: &Transcript<Self::H>) -> Result<Saved<Self::Ref>>;

    /// Remove one transcript from the backend so the harness no longer lists
    /// or resumes it. File-backed stores remove the session file or directory;
    /// `OpenCode` archives the session in place.
    ///
    /// # Errors
    /// When the reference doesn't exist or the backend rejects the removal.
    fn delete(&self, reference: &Self::Ref) -> Result<()>;

    /// Per-reference change cursors, for callers that cache parsed transcripts.
    /// Default: no fingerprints, forcing a re-parse. Backends with a cheap
    /// change signal (file mtime, a `MAX(updated)` query) should override.
    ///
    /// # Errors
    /// When the backend itself fails; per-reference failures are empty strings.
    fn fingerprints(&self, _refs: &[Self::Ref]) -> Result<HashMap<String, String>> {
        Ok(HashMap::new())
    }
}

/// A transcript found by [`Store::discover`]: its metadata and how to load it.
#[derive(Debug, Clone)]
pub struct Discovered<R> {
    pub meta: Meta,
    pub reference: R,
}

/// The outcome of [`Store::save`]: the id the harness will resume by, and where
/// it landed.
#[derive(Debug, Clone)]
pub struct Saved<R> {
    pub id: String,
    pub reference: R,
}

/// Runtime tag for the harnesses this crate implements — string-keyed
/// dispatch, where the type-level [`Harness`] markers select a
/// [`Body`](Harness::Body).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessId {
    ClaudeCode,
    Codex,
    OpenCode,
    Pi,
    Campfire,
    Cursor,
    CursorDesktop,
    Grok,
    Hermes,
    Amp,
    Antigravity,
    Simple,
    Cowork,
}

impl HarnessId {
    pub const ALL: [HarnessId; 13] = [
        HarnessId::ClaudeCode,
        HarnessId::Codex,
        HarnessId::OpenCode,
        HarnessId::Pi,
        HarnessId::Campfire,
        HarnessId::Cursor,
        HarnessId::CursorDesktop,
        HarnessId::Grok,
        HarnessId::Hermes,
        HarnessId::Amp,
        HarnessId::Antigravity,
        HarnessId::Simple,
        HarnessId::Cowork,
    ];

    /// The stable lowercase name, matching the corresponding [`Harness::NAME`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            HarnessId::ClaudeCode => "claude_code",
            HarnessId::Codex => "codex",
            HarnessId::OpenCode => "opencode",
            HarnessId::Pi => "pi",
            HarnessId::Campfire => "campfire",
            HarnessId::Cursor => "cursor",
            HarnessId::CursorDesktop => "cursor_desktop",
            HarnessId::Grok => "grok",
            HarnessId::Hermes => "hermes",
            HarnessId::Amp => "amp",
            HarnessId::Antigravity => "antigravity",
            HarnessId::Simple => "simple",
            HarnessId::Cowork => "cowork",
        }
    }
}

impl fmt::Display for HarnessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HarnessId {
    type Err = crate::error::Error;

    fn from_str(s: &str) -> Result<Self> {
        // Accept a few friendly aliases alongside the canonical names.
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude_code" | "claude-code" | "claudecode" => Ok(HarnessId::ClaudeCode),
            "codex" => Ok(HarnessId::Codex),
            "opencode" | "open_code" | "open-code" => Ok(HarnessId::OpenCode),
            "pi" => Ok(HarnessId::Pi),
            "campfire" => Ok(HarnessId::Campfire),
            "cursor" | "cursor_cli" | "cursor-cli" | "cursorcli" => Ok(HarnessId::Cursor),
            "cursor_desktop" | "cursor-desktop" | "cursordesktop" | "cursor_ide" | "cursor-ide" => {
                Ok(HarnessId::CursorDesktop)
            }
            "grok" | "grok_cli" | "grok-cli" | "grokcli" | "grok_build" | "grok-build" => {
                Ok(HarnessId::Grok)
            }
            "hermes" | "hermes_agent" | "hermes-agent" | "hermesagent" => Ok(HarnessId::Hermes),
            "amp" | "ampcode" | "amp_code" | "amp-code" => Ok(HarnessId::Amp),
            "antigravity" | "agy" | "antigravity_cli" | "antigravity-cli" | "anti-gravity" => {
                Ok(HarnessId::Antigravity)
            }
            "simple" | "simple_json" | "simple-json" => Ok(HarnessId::Simple),
            "cowork" | "claude_cowork" | "claude-cowork" | "claude_desktop" | "claude-desktop" => {
                Ok(HarnessId::Cowork)
            }
            other => Err(crate::error::Error::UnknownHarness(other.to_string())),
        }
    }
}
