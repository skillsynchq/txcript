//! The generic [`Transcript<H>`] and the three traits that act on it:
//! [`Harness`] (what representation a transcript is in), [`Codec`] (mapping a
//! native representation to and from [`Common`]), and [`Store`] (procuring and
//! persisting native transcripts against a real backend).

use std::collections::{HashMap, VecDeque};
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

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.0.start, self.0.end)
    }
}

/// Why a [`Transcript<Common>`] could not be cropped to a requested [`Span`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CropError {
    /// The span is empty, inverted, or reaches outside the transcript body.
    #[error("invalid crop range {span} for a transcript with {message_count} messages")]
    InvalidRange { span: Span, message_count: usize },
    /// The span contains only one side of a complete tool call/result pair.
    #[error(
        "crop range {span} cuts a tool call away from its result; nearest valid range is {nearest}"
    )]
    SplitToolPair { span: Span, nearest: Span },
    /// A tool id is reused before its previous call resolves, or resolves more
    /// than once, so there is no safe way to infer call/result ownership.
    #[error("transcript contains an ambiguous tool id")]
    AmbiguousToolId { tool_id: String },
}

impl CropError {
    /// The smallest outward expansion that keeps complete tool pairs together.
    /// Only present for [`CropError::SplitToolPair`].
    #[must_use]
    pub fn nearest_valid_span(&self) -> Option<&Span> {
        match self {
            Self::SplitToolPair { nearest, .. } => Some(nearest),
            Self::InvalidRange { .. } | Self::AmbiguousToolId { .. } => None,
        }
    }
}

impl Transcript<Common> {
    /// Resolve a [`Span`] to its messages, borrowing from this transcript.
    /// `None` when the span reaches past the end of the session.
    #[must_use]
    pub fn fragment(&self, span: &Span) -> Option<&[Message]> {
        self.body.get(span.0.clone())
    }

    /// Create a non-destructive copy containing only `span`.
    ///
    /// Metadata is preserved. A crop must contain at least one message and
    /// cannot separate a tool call from its result when both exist in the
    /// source transcript.
    ///
    /// # Errors
    /// When the span is invalid or splits a complete tool call/result pair.
    pub fn crop(&self, span: &Span) -> std::result::Result<Self, CropError> {
        self.crop_to(std::slice::from_ref(span))
    }

    /// Create a non-destructive copy containing only the messages in
    /// `spans`, in their original order: the cut between one span and the
    /// next is closed, so `[0..3, 8..10]` keeps five messages and drops
    /// the five between them.
    ///
    /// Spans may be given in any order and may overlap; together they must
    /// name at least one message. Metadata is preserved. As with
    /// [`crop`](Self::crop), a tool call and its result are kept or
    /// dropped together: the error names the span that holds one half and
    /// the smallest expansion of it that holds both.
    ///
    /// # Errors
    /// When a span is invalid, the spans keep nothing, or a tool call is
    /// separated from its result.
    pub fn crop_to(&self, spans: &[Span]) -> std::result::Result<Self, CropError> {
        let len = self.body.len();
        let mut kept = vec![false; len];
        for span in spans {
            if span.0.start >= span.0.end || span.0.end > len {
                return Err(CropError::InvalidRange {
                    span: span.clone(),
                    message_count: len,
                });
            }
            for flag in &mut kept[span.0.clone()] {
                *flag = true;
            }
        }
        if !kept.iter().any(|flag| *flag) {
            return Err(CropError::InvalidRange {
                span: Span(0..0),
                message_count: len,
            });
        }

        let pairs = tool_pairs(&self.body)?;
        if let Some(&(tool_use, tool_result)) = pairs
            .iter()
            .find(|(tool_use, tool_result)| kept[*tool_use] != kept[*tool_result])
        {
            let kept_half = if kept[tool_use] {
                tool_use
            } else {
                tool_result
            };
            let span = spans
                .iter()
                .find(|span| span.0.contains(&kept_half))
                .cloned()
                .unwrap_or(Span(kept_half..kept_half + 1));
            let nearest = nearest_valid_span(&pairs, &span, len);
            return Err(CropError::SplitToolPair { span, nearest });
        }

        let body = self
            .body
            .iter()
            .zip(&kept)
            .filter(|(_, kept)| **kept)
            .map(|(message, _)| message.clone())
            .collect();
        Ok(Self::new(self.meta.clone(), body))
    }

    /// The messages that must be kept or dropped together: each complete
    /// tool call/result pair as `(call index, result index)`, in the order
    /// the results arrive. A call the transcript never answers is not a
    /// pair.
    ///
    /// # Errors
    /// When a tool id is reused before its previous call resolves, or
    /// resolves more than once.
    pub fn tool_pairs(&self) -> std::result::Result<Vec<(usize, usize)>, CropError> {
        tool_pairs(&self.body)
    }
}

fn tool_pairs(body: &[Message]) -> std::result::Result<Vec<(usize, usize)>, CropError> {
    enum ToolState {
        Outstanding(usize),
        Resolved,
    }

    let mut uses: HashMap<&str, ToolState> = HashMap::new();
    let mut pairs = Vec::new();
    for (index, message) in body.iter().enumerate() {
        for block in &message.content {
            match block {
                crate::common::Block::ToolUse { id, .. } => match uses.entry(id) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(ToolState::Outstanding(index));
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        if matches!(entry.get(), ToolState::Outstanding(_)) {
                            return Err(CropError::AmbiguousToolId {
                                tool_id: id.clone(),
                            });
                        }
                        entry.insert(ToolState::Outstanding(index));
                    }
                },
                crate::common::Block::ToolResult { tool_use_id, .. } => {
                    if let Some(state) = uses.get_mut(tool_use_id.as_str()) {
                        match state {
                            ToolState::Outstanding(use_index) => {
                                pairs.push((*use_index, index));
                                *state = ToolState::Resolved;
                            }
                            ToolState::Resolved => {
                                return Err(CropError::AmbiguousToolId {
                                    tool_id: tool_use_id.clone(),
                                });
                            }
                        }
                    }
                }
                crate::common::Block::Text { .. }
                | crate::common::Block::Thinking { .. }
                | crate::common::Block::Image { .. }
                | crate::common::Block::Artifact { .. } => {}
            }
        }
    }
    Ok(pairs)
}

fn nearest_valid_span(pairs: &[(usize, usize)], span: &Span, len: usize) -> Span {
    let mut linked = vec![Vec::new(); len];
    for &(tool_use, tool_result) in pairs {
        if tool_use < len && tool_result < len {
            linked[tool_use].push(tool_result);
            linked[tool_result].push(tool_use);
        }
    }

    let (mut start, mut end) = (span.0.start, span.0.end);
    let mut queued = vec![false; len];
    let mut queue = VecDeque::new();
    let mut enqueue = |index: usize, queue: &mut VecDeque<usize>| {
        if !queued[index] {
            queued[index] = true;
            queue.push_back(index);
        }
    };
    for index in start..end {
        enqueue(index, &mut queue);
    }

    while let Some(index) = queue.pop_front() {
        for &other in &linked[index] {
            if other < start {
                for added in other..start {
                    enqueue(added, &mut queue);
                }
                start = other;
            } else if other >= end {
                for added in end..=other {
                    enqueue(added, &mut queue);
                }
                end = other + 1;
            }
        }
    }
    Span(start..end)
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
    ClaudeChat,
    ChatGpt,
    Codex,
    OpenCode,
    Pi,
    Campfire,
    Cursor,
    CursorDesktop,
    Grok,
    Fx,
    Hermes,
    Amp,
    Antigravity,
    Simple,
    Cowork,
}

impl HarnessId {
    pub const ALL: [HarnessId; 16] = [
        HarnessId::ClaudeCode,
        HarnessId::ClaudeChat,
        HarnessId::ChatGpt,
        HarnessId::Codex,
        HarnessId::OpenCode,
        HarnessId::Pi,
        HarnessId::Campfire,
        HarnessId::Cursor,
        HarnessId::CursorDesktop,
        HarnessId::Grok,
        HarnessId::Fx,
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
            HarnessId::ClaudeChat => "claude_chat",
            HarnessId::ChatGpt => "chatgpt",
            HarnessId::Codex => "codex",
            HarnessId::OpenCode => "opencode",
            HarnessId::Pi => "pi",
            HarnessId::Campfire => "campfire",
            HarnessId::Cursor => "cursor",
            HarnessId::CursorDesktop => "cursor_desktop",
            HarnessId::Grok => "grok",
            HarnessId::Fx => "fx",
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
            "claude_chat" | "claude-chat" | "claude_web" | "claude-web" => {
                Ok(HarnessId::ClaudeChat)
            }
            "chatgpt" | "chat_gpt" | "chat-gpt" | "openai_chat" | "openai-chat" => {
                Ok(HarnessId::ChatGpt)
            }
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
            "fx" | "fx_cli" | "fx-cli" | "fxcli" | "vercel_fx" | "vercel-fx" => Ok(HarnessId::Fx),
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

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::*;
    use crate::common::{Block, Role, Tool, ToolOutput};

    fn message(role: Role, content: Vec<Block>) -> Message {
        Message {
            role,
            content,
            timestamp: DateTime::<Utc>::UNIX_EPOCH,
            model: None,
            stop_reason: None,
            usage: None,
        }
    }

    fn transcript(body: Vec<Message>) -> Transcript<Common> {
        Transcript::new(
            Meta {
                id: "source-session".into(),
                timestamp: DateTime::<Utc>::UNIX_EPOCH,
                cwd: Some("/work/project".into()),
                git_branch: Some("main".into()),
                title: Some("Crop me".into()),
                cli_version: None,
                model: None,
            },
            body,
        )
    }

    fn text(value: &str) -> Message {
        message(Role::User, vec![Block::Text { text: value.into() }])
    }

    #[test]
    fn crop_copies_only_the_requested_messages_and_preserves_metadata() {
        let source = transcript(vec![text("one"), text("two"), text("three")]);

        let cropped = source.crop(&Span(1..3)).unwrap();

        assert_eq!(cropped.meta, source.meta);
        assert_eq!(cropped.body, source.body[1..3]);
        assert_eq!(source.body.len(), 3, "cropping must not mutate the source");
    }

    #[test]
    fn crop_rejects_empty_inverted_and_out_of_bounds_spans() {
        let source = transcript(vec![text("one"), text("two")]);
        let inverted_start = source.body.len();

        for span in [
            Span(1..1),
            Span(inverted_start..1),
            Span(0..source.body.len() + 1),
        ] {
            let error = source.crop(&span).unwrap_err();
            assert!(error.to_string().contains("invalid crop range"));
        }
    }

    #[test]
    fn crop_keeps_tool_calls_and_results_together() {
        let source = transcript(vec![
            message(
                Role::Assistant,
                vec![Block::ToolUse {
                    id: "call-1".into(),
                    tool: Tool::Raw {
                        tool_name: "Read".into(),
                        input: serde_json::json!({"path": "src/lib.rs"}),
                    },
                }],
            ),
            message(
                Role::User,
                vec![Block::ToolResult {
                    tool_use_id: "call-1".into(),
                    content: ToolOutput::Text("contents".into()),
                    is_error: false,
                }],
            ),
            text("done"),
        ]);

        let error = source.crop(&Span(1..3)).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cuts a tool call away from its result")
        );
        assert_eq!(error.nearest_valid_span(), Some(&Span(0..3)));
        assert!(source.crop(&Span(0..2)).is_ok());
    }

    #[test]
    fn crop_to_keeps_the_union_of_spans_in_order_and_closes_the_cuts() {
        let source = transcript(vec![text("one"), text("two"), text("three")]);
        let cropped = source.crop_to(&[Span(2..3), Span(0..1)]).unwrap();
        assert_eq!(cropped.meta, source.meta);
        assert_eq!(cropped.body.len(), 2);
        assert_eq!(cropped.body[0], source.body[0]);
        assert_eq!(cropped.body[1], source.body[2]);
        // Overlap is fine; the message is kept once.
        let overlapping = source.crop_to(&[Span(0..2), Span(1..3)]).unwrap();
        assert_eq!(overlapping.body, source.body);
        assert_eq!(source.body.len(), 3, "cropping must not mutate the source");
    }

    #[test]
    fn crop_to_rejects_nothing_kept_and_bad_spans() {
        let source = transcript(vec![text("one"), text("two"), text("three")]);
        assert!(matches!(
            source.crop_to(&[]).unwrap_err(),
            CropError::InvalidRange { .. }
        ));
        assert!(matches!(
            source.crop_to(&[Span(0..1), Span(2..2)]).unwrap_err(),
            CropError::InvalidRange {
                span: Span(std::ops::Range { start: 2, end: 2 }),
                ..
            }
        ));
        assert!(source.crop_to(&[Span(0..1), Span(1..9)]).is_err());
    }

    #[test]
    fn crop_to_names_the_span_that_splits_a_tool_pair() {
        let mut source = transcript(vec![text("one"), text("two"), text("three")]);
        source.body[1].content.push(Block::ToolUse {
            id: "call-1".into(),
            tool: Tool::Raw {
                tool_name: "x".into(),
                input: serde_json::json!({}),
            },
        });
        source.body[2].content.push(Block::ToolResult {
            tool_use_id: "call-1".into(),
            content: ToolOutput::Text("ok".into()),
            is_error: false,
        });
        assert_eq!(source.tool_pairs().unwrap(), vec![(1, 2)]);
        // Keeps the call (#2) in one span and drops the result (#3).
        let error = source.crop_to(&[Span(0..1), Span(1..2)]).unwrap_err();
        assert_eq!(
            error,
            CropError::SplitToolPair {
                span: Span(1..2),
                nearest: Span(1..3),
            }
        );
        assert!(source.crop_to(&[Span(0..1), Span(1..3)]).is_ok());
        assert!(source.crop_to(&[Span(0..1)]).is_ok());
    }

    #[test]
    fn nearest_valid_crop_expands_distant_tool_pairs_without_scanning_all_ranges() {
        let started = std::time::Instant::now();
        let nearest = nearest_valid_span(&[(5_000, 9_999)], &Span(5_000..5_001), 10_000);

        assert_eq!(nearest, Span(5_000..10_000));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "nearest span search took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn crop_fails_closed_for_ambiguous_tool_ids_but_allows_sequential_reuse() {
        let tool_use = || {
            message(
                Role::Assistant,
                vec![Block::ToolUse {
                    id: "same-id".into(),
                    tool: Tool::Raw {
                        tool_name: "Read".into(),
                        input: serde_json::Value::Null,
                    },
                }],
            )
        };
        let result = || {
            message(
                Role::User,
                vec![Block::ToolResult {
                    tool_use_id: "same-id".into(),
                    content: ToolOutput::Text("ok".into()),
                    is_error: false,
                }],
            )
        };

        let duplicate_use = transcript(vec![tool_use(), tool_use(), result()]);
        assert!(
            duplicate_use
                .crop(&Span(0..3))
                .unwrap_err()
                .to_string()
                .contains("ambiguous tool id")
        );
        let hostile = CropError::AmbiguousToolId {
            tool_id: "\x1b]8;;https://example.invalid\x07\nspoofed".into(),
        }
        .to_string();
        assert_eq!(hostile, "transcript contains an ambiguous tool id");

        let duplicate_result = transcript(vec![tool_use(), result(), result()]);
        assert!(
            duplicate_result
                .crop(&Span(0..3))
                .unwrap_err()
                .to_string()
                .contains("ambiguous tool id")
        );

        let sequential = transcript(vec![tool_use(), result(), tool_use(), result()]);
        assert!(sequential.crop(&Span(0..2)).is_ok());
        assert!(sequential.crop(&Span(2..4)).is_ok());
    }
}
