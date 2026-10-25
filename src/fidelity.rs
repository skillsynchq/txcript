//! Conversion fidelity: what a conversion into a harness would lose, before
//! doing it.
//!
//! [`assess`] renders a canonical transcript as the target harness's native
//! text, parses that text back, and compares the two. Everything the target's
//! codec fails to persist shows up as a [`Loss`]. The result is a
//! *codec-level* measurement: it covers exactly what the codec writes into the
//! harness's session text, and nothing a [`Store`](crate::Store) adds or
//! changes around it (artifacts materialized as files, a fresh id and
//! timestamp, a re-homed working directory, the `OpenCode` import binary, the
//! Cursor `SQLite` database). There is no I/O, so it runs everywhere the
//! codecs do, WASM included.
//!
//! # How the comparison works
//!
//! Targets split and merge messages, so messages are never compared one to
//! one. Each transcript is flattened to a stream of typed items (text,
//! thinking, tool calls, tool results, images, artifacts) and the streams are
//! matched by content. What matches is preserved; leftovers that resemble
//! each other are [`LossKind::Changed`] (a renamed tool, an edited text); the
//! rest are [`LossKind::Lost`] when only the source has them and
//! [`LossKind::Synthesized`] when only the round trip does. Per-message
//! accounting (usage, stop reason, model, timestamp) and session metadata are
//! compared the same way, field by field.
//!
//! Normalization keeps formatting out of the verdict: text is trimmed, JSON
//! (tool inputs, structured or JSON-looking tool results) compares by value so
//! key order and whitespace never count, a list of text parts equals its
//! joined text, and regenerated ids are ignored.
//!
//! # Severity
//!
//! [`Severity`] ranks the worst thing a conversion does. Only
//! [`LossKind::Lost`] and [`LossKind::Changed`] items raise it; synthesized
//! items are listed but never fail a check.
//!
//! - [`Severity::Content`]: text, thinking, tool calls and arguments, results,
//!   images, artifacts, error flags, block order.
//! - [`Severity::Accounting`]: usage, stop reasons, per-message models,
//!   thinking signatures.
//! - [`Severity::Metadata`]: title, model, git branch, working directory,
//!   timestamps, lineage.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};

use serde::Serialize;
use serde_json::Value;

use crate::common::{Artifact, ArtifactSource, Block, Role, StopReason, ToolOutput, Usage};
use crate::dispatch::{parse_to_common, render_from_common};
use crate::{Common, HarnessId, Result, Transcript};

/// The harnesses a canonical transcript can be rendered into: every codec
/// except the live read-only sources (Claude Chat, cloud Cowork, `ChatGPT`).
///
/// Whether a harness can also be *written* to disk (`continue --with`) is a
/// store question this module does not answer: Simple, Hermes, and Amp have
/// codecs but no store to write into.
pub const TARGETS: [HarnessId; 15] = [
    HarnessId::ClaudeCode,
    HarnessId::Codex,
    HarnessId::OpenCode,
    HarnessId::Pi,
    HarnessId::Campfire,
    HarnessId::Cursor,
    HarnessId::CursorDesktop,
    HarnessId::Grok,
    HarnessId::GrokBot,
    HarnessId::Fx,
    HarnessId::Hermes,
    HarnessId::Amp,
    HarnessId::Antigravity,
    HarnessId::Simple,
    HarnessId::Cowork,
];

/// Longest example kept in a [`Loss`], in characters.
const EXAMPLE_CAP: usize = 80;
/// Longest example kept under [`Options::detail`].
const DETAIL_CAP: usize = 240;
/// How many look-alike candidates one leftover is compared against.
const PAIR_CANDIDATES: usize = 64;

/// How bad a conversion is, by the worst class of thing it loses or changes.
/// Ordered: `None < Metadata < Accounting < Content`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Nothing is lost or changed.
    None,
    /// Only session metadata differs.
    Metadata,
    /// Token accounting, stop reasons, or per-message models differ.
    Accounting,
    /// The conversation itself differs.
    Content,
}

impl Severity {
    /// The stable lowercase name, as used in JSON and `--fail-on`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Severity::None => "none",
            Severity::Metadata => "metadata",
            Severity::Accounting => "accounting",
            Severity::Content => "content",
        }
    }
}

/// What happened to an item on the way through the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LossKind {
    /// Present in the source, absent after the round trip.
    Lost,
    /// Present in both, but different.
    Changed,
    /// Absent in the source, present after the round trip.
    Synthesized,
}

/// What part of a transcript an item is about. Declared content first, then
/// accounting, then metadata; items sort in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Message text.
    Text,
    /// Thinking text.
    Thinking,
    /// A tool call that does not survive at all.
    ToolCall,
    /// A tool call that survives under another name.
    ToolName,
    /// A tool call that survives with different arguments.
    ToolArgs,
    /// A tool result.
    ToolResult,
    /// An inline image.
    Image,
    /// A named file.
    Artifact,
    /// A tool result's error flag.
    IsError,
    /// Blocks that survive but in a different order.
    Order,
    /// The opaque token that lets a harness replay thinking.
    ThinkingSignature,
    /// Per-message token accounting.
    Usage,
    /// Why a turn ended.
    StopReason,
    /// The model that produced a message.
    MessageModel,
    /// The session title.
    Title,
    /// The session's primary model.
    Model,
    /// The git branch.
    GitBranch,
    /// The working directory.
    Cwd,
    /// Session or message timestamps.
    Timestamp,
    /// How the session relates to another.
    Lineage,
}

impl Category {
    /// The severity class this category belongs to.
    #[must_use]
    pub const fn severity(self) -> Severity {
        match self {
            Category::Text
            | Category::Thinking
            | Category::ToolCall
            | Category::ToolName
            | Category::ToolArgs
            | Category::ToolResult
            | Category::Image
            | Category::Artifact
            | Category::IsError
            | Category::Order => Severity::Content,
            Category::ThinkingSignature
            | Category::Usage
            | Category::StopReason
            | Category::MessageModel => Severity::Accounting,
            Category::Title
            | Category::Model
            | Category::GitBranch
            | Category::Cwd
            | Category::Timestamp
            | Category::Lineage => Severity::Metadata,
        }
    }
}

/// One kind of difference, with how many times it happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Loss {
    /// Lost, changed, or synthesized.
    pub kind: LossKind,
    /// What it is about.
    pub category: Category,
    /// A finer label within the category: `Bash.description` for a dropped
    /// tool argument, `/release -> _release` for a renamed tool, the usage
    /// field that was dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// How many items.
    pub count: usize,
    /// Index in the source of the first affected message. `None` for
    /// synthesized items, which are not in the source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_at: Option<usize>,
    /// A short sample: at most 80 characters, control characters replaced by
    /// spaces, and never a tool result body or image data (unless
    /// [`Options::detail`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
}

/// What converting a transcript into one harness costs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// The harness converted into.
    pub target: HarnessId,
    /// The worst class of loss; see the module docs.
    pub severity: Severity,
    /// Messages in the source.
    pub messages_before: usize,
    /// Messages after the round trip. Targets split and merge turns, so this
    /// differing is not itself a loss.
    pub messages_after: usize,
    /// The differences, sorted by category, kind, then path.
    pub items: Vec<Loss>,
    /// Things worth knowing that are not differences, such as an empty
    /// session.
    pub warnings: Vec<String>,
}

impl Report {
    /// Total number of items of `kind` across every category.
    #[must_use]
    pub fn total(&self, kind: LossKind) -> usize {
        self.items
            .iter()
            .filter(|item| item.kind == kind)
            .map(|item| item.count)
            .sum()
    }
}

/// Knobs for [`assess_with`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// Keep longer examples, and a preview of tool results. Off by default so
    /// a report stays safe to paste: counts and short samples only.
    pub detail: bool,
}

/// Assess converting `source` into `target`.
///
/// # Errors
/// When the target's codec cannot render the transcript or cannot read its
/// own output back.
pub fn assess(target: HarnessId, source: &Transcript<Common>) -> Result<Report> {
    assess_with(target, source, Options::default())
}

/// [`assess`] with options.
///
/// # Errors
/// As [`assess`].
pub fn assess_with(
    target: HarnessId,
    source: &Transcript<Common>,
    options: Options,
) -> Result<Report> {
    // Stores write artifacts out as files before the codec sees them; do the
    // same on paper so the codec gets the shape it gets in real use.
    let prepared = with_artifact_paths(source);
    let text = render_from_common(target, &prepared)?;
    let round_trip = parse_to_common(target, &text)?;
    Ok(compare(target, source, &round_trip, options))
}

/// Whether an artifact's content travels in the transcript rather than in a
/// file.
fn held_inline(artifact: &Artifact) -> bool {
    !matches!(artifact.source, ArtifactSource::Path { .. })
}

/// `source` with every inline artifact replaced by a file path, as the store
/// layer does before writing. Without such artifacts, `source` itself.
fn with_artifact_paths(source: &Transcript<Common>) -> Cow<'_, Transcript<Common>> {
    let any = source
        .body
        .iter()
        .flat_map(|m| &m.content)
        .any(|b| matches!(b, Block::Artifact { artifact } if held_inline(artifact)));
    if !any {
        return Cow::Borrowed(source);
    }
    let mut prepared = source.clone();
    for (at, message) in prepared.body.iter_mut().enumerate() {
        for (n, block) in message.content.iter_mut().enumerate() {
            if let Block::Artifact { artifact } = block
                && held_inline(artifact)
            {
                let media_type = match &artifact.source {
                    ArtifactSource::Text { media_type, .. }
                    | ArtifactSource::Base64 { media_type, .. }
                    | ArtifactSource::Path { media_type, .. } => media_type.clone(),
                };
                let name = artifact.name.replace(['/', '\\'], "_");
                artifact.source = ArtifactSource::Path {
                    path: format!("/txcript-artifacts/{at}-{n}/{name}"),
                    media_type,
                };
            }
        }
    }
    Cow::Owned(prepared)
}

/// Assess `source` against every harness in [`TARGETS`]. A target whose codec
/// fails is reported as an error under its own entry and never stops the
/// others.
#[must_use]
pub fn check_all(source: &Transcript<Common>) -> Vec<(HarnessId, Result<Report>)> {
    TARGETS
        .into_iter()
        .map(|target| (target, assess(target, source)))
        .collect()
}

// ── comparison ─────────────────────────────────────────────────────────

fn compare(
    target: HarnessId,
    source: &Transcript<Common>,
    round_trip: &Transcript<Common>,
    options: Options,
) -> Report {
    let mut acc = Acc::new(options);
    let src = flatten(source);
    let rt = flatten(round_trip);
    compare_content(&src, &rt, &mut acc);
    compare_accounting(source, round_trip, &mut acc);
    compare_meta(source, round_trip, &mut acc);

    let items: Vec<Loss> = acc.items.into_values().collect();
    let severity = items
        .iter()
        .filter(|item| item.kind != LossKind::Synthesized)
        .map(|item| item.category.severity())
        .max()
        .unwrap_or(Severity::None);
    let mut warnings = Vec::new();
    if source.body.is_empty() {
        warnings.push("the session has 0 messages; there is nothing to lose".to_string());
    }
    Report {
        target,
        severity,
        messages_before: source.body.len(),
        messages_after: round_trip.body.len(),
        items,
        warnings,
    }
}

/// Collects items, merging repeats of the same kind of difference.
struct Acc {
    items: BTreeMap<(Category, LossKind, Option<String>), Loss>,
    cap: usize,
    detail: bool,
}

impl Acc {
    fn new(options: Options) -> Acc {
        Acc {
            items: BTreeMap::new(),
            cap: if options.detail {
                DETAIL_CAP
            } else {
                EXAMPLE_CAP
            },
            detail: options.detail,
        }
    }

    fn add(
        &mut self,
        kind: LossKind,
        category: Category,
        path: Option<String>,
        at: Option<usize>,
        example: impl FnOnce() -> Option<String>,
    ) {
        self.add_n(kind, category, path, at, 1, example);
    }

    fn add_n(
        &mut self,
        kind: LossKind,
        category: Category,
        path: Option<String>,
        at: Option<usize>,
        count: usize,
        example: impl FnOnce() -> Option<String>,
    ) {
        if count == 0 {
            return;
        }
        let cap = self.cap;
        let entry = self
            .items
            .entry((category, kind, path.clone()))
            .or_insert_with(|| Loss {
                kind,
                category,
                path,
                count: 0,
                first_at: at,
                example: example().map(|e| clip(&e, cap)),
            });
        entry.count += count;
        entry.first_at = match (entry.first_at, at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
}

/// `s` on one line, at most `cap` characters, with an ellipsis when cut.
fn clip(s: &str, cap: usize) -> String {
    let mut out = String::new();
    let mut chars = s
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c });
    for _ in 0..cap.saturating_sub(1) {
        match chars.next() {
            Some(c) => out.push(c),
            None => return out,
        }
    }
    match (chars.next(), chars.next()) {
        (None, _) => {}
        (Some(c), None) => out.push(c),
        (Some(_), Some(_)) => out.push('…'),
    }
    out
}

/// `before -> after`, each side cut to the stretch around where the two start
/// to differ, with line breaks and tabs shown rather than collapsed, so a
/// change in trailing whitespace is visible.
fn arrow(before: &str, after: &str, cap: usize) -> String {
    let half = cap.saturating_sub(4) / 2;
    let fits = |s: &str| s.chars().take(half + 1).count() <= half;
    let skip = if fits(before) && fits(after) {
        0
    } else {
        let shared = before
            .chars()
            .zip(after.chars())
            .take_while(|(x, y)| x == y)
            .count();
        shared.saturating_sub(8)
    };
    let side = |s: &str| {
        let mut out: Vec<char> = Vec::new();
        if skip > 0 {
            out.push('…');
        }
        for c in s.chars().skip(skip) {
            match c {
                '\n' => out.extend(['\\', 'n']),
                '\t' => out.extend(['\\', 't']),
                '\r' => out.extend(['\\', 'r']),
                c if c.is_control() => out.push(' '),
                c => out.push(c),
            }
            if out.len() > half {
                out.truncate(half.saturating_sub(1));
                out.push('…');
                break;
            }
        }
        out.into_iter().collect::<String>()
    };
    format!("{} -> {}", side(before), side(after))
}

// ── content stream ─────────────────────────────────────────────────────

/// A tool result reduced to what it says.
enum Proj<'a> {
    Text(Cow<'a, str>),
    Json(Cow<'a, Value>),
}

impl<'a> Proj<'a> {
    fn into_text(self) -> Cow<'a, str> {
        match self {
            Proj::Text(s) => s,
            Proj::Json(v) => Cow::Owned(v.to_string()),
        }
    }
}

enum Body<'a> {
    Text {
        role: Role,
        text: &'a str,
    },
    Thinking {
        text: &'a str,
        signature: Option<&'a str>,
        encrypted: Option<&'a str>,
    },
    Call {
        name: String,
        input: Value,
    },
    Result {
        output: &'a ToolOutput,
        is_error: bool,
    },
    Image {
        media_type: &'a str,
    },
    Artifact {
        name: &'a str,
    },
}

/// One block of a flattened transcript.
struct Item<'a> {
    /// Identity of the block's content, ids and formatting excluded.
    key: u64,
    /// The message the block came from.
    at: usize,
    body: Body<'a>,
}

fn hasher(tag: u8) -> DefaultHasher {
    let mut h = DefaultHasher::new();
    tag.hash(&mut h);
    h
}

/// Hash a JSON value by content: object keys in sorted order, so neither key
/// order nor whitespace in the source text affects it.
fn hash_value(v: &Value, h: &mut DefaultHasher) {
    match v {
        Value::Null => 0u8.hash(h),
        Value::Bool(b) => (1u8, b).hash(h),
        Value::Number(n) => {
            2u8.hash(h);
            if let Some(u) = n.as_u64() {
                (0u8, u).hash(h);
            } else if let Some(i) = n.as_i64() {
                (1u8, i).hash(h);
            } else {
                (2u8, n.as_f64().map(f64::to_bits)).hash(h);
            }
        }
        Value::String(s) => (3u8, s).hash(h),
        Value::Array(items) => {
            (4u8, items.len()).hash(h);
            for item in items {
                hash_value(item, h);
            }
        }
        Value::Object(map) => {
            (5u8, map.len()).hash(h);
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for key in keys {
                key.hash(h);
                hash_value(&map[key], h);
            }
        }
    }
}

/// The text of a result that is a list of plain text parts.
fn text_parts(parts: &[Value]) -> Option<String> {
    let mut texts = Vec::with_capacity(parts.len());
    for part in parts {
        let map = part.as_object()?;
        if map.get("type")?.as_str()? != "text" || map.keys().any(|k| k != "type" && k != "text") {
            return None;
        }
        texts.push(map.get("text")?.as_str()?);
    }
    (!texts.is_empty()).then(|| texts.join("\n"))
}

fn project(output: &ToolOutput) -> Proj<'_> {
    /// Text that is JSON compares as that JSON, so re-spacing it is not a
    /// change. `depth` bounds how many layers of JSON-in-a-string are peeled.
    fn of_text(t: Cow<'_, str>, depth: u8) -> Proj<'_> {
        if depth > 0
            && t.starts_with(['{', '['])
            && let Ok(v) = serde_json::from_str::<Value>(&t)
        {
            return of_value(Cow::Owned(v), depth - 1);
        }
        Proj::Text(t)
    }
    fn of_value(v: Cow<'_, Value>, depth: u8) -> Proj<'_> {
        match &*v {
            Value::String(s) => of_text(Cow::Owned(s.trim().to_string()), depth),
            Value::Array(parts) => match text_parts(parts) {
                Some(joined) => of_text(Cow::Owned(joined.trim().to_string()), depth),
                None => Proj::Json(v),
            },
            _ => Proj::Json(v),
        }
    }
    match output {
        ToolOutput::Text(s) => of_text(Cow::Borrowed(s.trim()), 3),
        ToolOutput::Json(v) => of_value(Cow::Borrowed(v), 3),
    }
}

fn flatten(t: &Transcript<Common>) -> Vec<Item<'_>> {
    let mut out = Vec::new();
    for (at, message) in t.body.iter().enumerate() {
        for block in &message.content {
            let item = match block {
                Block::Text { text } => {
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    let mut h = hasher(0);
                    (message.role == Role::User, text).hash(&mut h);
                    Item {
                        key: h.finish(),
                        at,
                        body: Body::Text {
                            role: message.role,
                            text,
                        },
                    }
                }
                Block::Thinking {
                    text,
                    signature,
                    encrypted,
                } => {
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    let mut h = hasher(1);
                    text.hash(&mut h);
                    Item {
                        key: h.finish(),
                        at,
                        body: Body::Thinking {
                            text,
                            signature: signature.as_deref(),
                            encrypted: encrypted.as_deref(),
                        },
                    }
                }
                Block::ToolUse { tool, .. } => {
                    let (name, input) = tool.to_canonical();
                    let mut h = hasher(2);
                    name.hash(&mut h);
                    hash_value(&input, &mut h);
                    Item {
                        key: h.finish(),
                        at,
                        body: Body::Call { name, input },
                    }
                }
                Block::ToolResult {
                    content, is_error, ..
                } => {
                    // Only the hash is kept: a result can be megabytes, and the
                    // projection is rebuilt for the few that need comparing.
                    let mut h = hasher(3);
                    match &project(content) {
                        Proj::Text(s) => (0u8, s.as_ref()).hash(&mut h),
                        Proj::Json(v) => {
                            1u8.hash(&mut h);
                            hash_value(v, &mut h);
                        }
                    }
                    Item {
                        key: h.finish(),
                        at,
                        body: Body::Result {
                            output: content,
                            is_error: *is_error,
                        },
                    }
                }
                Block::Image { source } => {
                    let mut h = hasher(4);
                    (&source.media_type, &source.data).hash(&mut h);
                    Item {
                        key: h.finish(),
                        at,
                        body: Body::Image {
                            media_type: &source.media_type,
                        },
                    }
                }
                Block::Artifact { artifact } => Item {
                    key: artifact_key(artifact),
                    at,
                    body: Body::Artifact {
                        name: &artifact.name,
                    },
                },
            };
            out.push(item);
        }
    }
    out
}

/// An artifact's identity is its name: the store writes its content to a file
/// of its own, which the codec never carries, so content is not compared.
fn artifact_key(artifact: &Artifact) -> u64 {
    let mut h = hasher(5);
    artifact.name.hash(&mut h);
    h.finish()
}

/// Match equal keys one to one, in order. Returns the matched index pairs,
/// then the unmatched indices of each side.
fn match_keys(src: &[u64], rt: &[u64]) -> (Vec<(usize, usize)>, Vec<usize>, Vec<usize>) {
    let mut index: HashMap<u64, VecDeque<usize>> = HashMap::new();
    for (j, key) in rt.iter().enumerate() {
        index.entry(*key).or_default().push_back(j);
    }
    let mut used = vec![false; rt.len()];
    let mut pairs = Vec::new();
    let mut src_left = Vec::new();
    for (i, key) in src.iter().enumerate() {
        match index.get_mut(key).and_then(VecDeque::pop_front) {
            Some(j) => {
                used[j] = true;
                pairs.push((i, j));
            }
            None => src_left.push(i),
        }
    }
    let rt_left = (0..rt.len()).filter(|j| !used[*j]).collect();
    (pairs, src_left, rt_left)
}

/// The source positions of matched items that left the longest in-order run
/// behind: the blocks that moved. `pairs` is sorted by source position.
fn moved(pairs: &[(usize, usize)]) -> Vec<usize> {
    let mut tails: Vec<usize> = Vec::new();
    let mut prev = vec![None; pairs.len()];
    for (k, &(_, j)) in pairs.iter().enumerate() {
        let pos = tails.partition_point(|&t| pairs[t].1 < j);
        prev[k] = pos.checked_sub(1).map(|p| tails[p]);
        if pos == tails.len() {
            tails.push(k);
        } else {
            tails[pos] = k;
        }
    }
    let mut keep = vec![false; pairs.len()];
    let mut cursor = tails.last().copied();
    while let Some(k) = cursor {
        keep[k] = true;
        cursor = prev[k];
    }
    pairs
        .iter()
        .zip(keep)
        .filter(|(_, kept)| !kept)
        .map(|(pair, _)| pair.0)
        .collect()
}

/// Whitespace-insensitive equality, without allocating.
fn same_words(a: &str, b: &str) -> bool {
    a.split_whitespace().eq(b.split_whitespace())
}

/// The first few characters of `s` with whitespace collapsed: two texts can
/// only resemble each other when these agree.
fn lead(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    let mut taken = 0;
    for word in s.split_whitespace() {
        for c in word.chars() {
            if taken >= 6 {
                return h.finish();
            }
            c.hash(&mut h);
            taken += 1;
        }
        ' '.hash(&mut h);
        taken += 1;
    }
    h.finish()
}

/// Whether two leftover texts are probably the same text, edited: equal but
/// for whitespace, or opening with the same eight characters.
fn related(a: &str, b: &str) -> bool {
    same_words(a, b) || a.chars().zip(b.chars()).take_while(|(p, q)| p == q).count() >= 8
}

/// A leftover tool call's identity for pairing: its name and each argument.
fn call_keys(name: &str, input: &Value) -> Vec<(u8, u64)> {
    let mut by_name = hasher(10);
    name.hash(&mut by_name);
    let mut by_input = hasher(11);
    hash_value(input, &mut by_input);
    vec![(0, by_name.finish()), (1, by_input.finish())]
}

/// Whether two leftover calls are the same call, altered.
fn related_calls(a: (&str, &Value), b: (&str, &Value)) -> bool {
    if a.1 == b.1 {
        return true;
    }
    if a.0 != b.0 {
        return false;
    }
    match (a.1.as_object(), b.1.as_object()) {
        (Some(x), Some(y)) => x.iter().any(|(k, v)| y.get(k) == Some(v)),
        _ => false,
    }
}

/// Pair leftover source items with leftover round-trip items that resemble
/// them. Candidates come from `keys` buckets; `related` has the last word.
/// Returns the pairs and what stayed unpaired on each side.
fn pair_up(
    src_left: &[usize],
    rt_left: &[usize],
    src_keys: &dyn Fn(usize) -> Vec<(u8, u64)>,
    rt_keys: &dyn Fn(usize) -> Vec<(u8, u64)>,
    related: &dyn Fn(usize, usize) -> bool,
) -> (Vec<(usize, usize)>, Vec<usize>, Vec<usize>) {
    let mut buckets: HashMap<(u8, u64), Vec<usize>> = HashMap::new();
    for &j in rt_left {
        for key in rt_keys(j) {
            buckets.entry(key).or_default().push(j);
        }
    }
    let mut used: HashSet<usize> = HashSet::new();
    let mut cursors: HashMap<(u8, u64), usize> = HashMap::new();
    let mut pairs = Vec::new();
    let mut src_unpaired = Vec::new();
    for &i in src_left {
        let mut found = None;
        for key in src_keys(i) {
            let Some(bucket) = buckets.get(&key) else {
                continue;
            };
            let cursor = cursors.entry(key).or_insert(0);
            while *cursor < bucket.len() && used.contains(&bucket[*cursor]) {
                *cursor += 1;
            }
            found = bucket[*cursor..]
                .iter()
                .filter(|j| !used.contains(j))
                .take(PAIR_CANDIDATES)
                .find(|&&j| related(i, j))
                .copied();
            if found.is_some() {
                break;
            }
        }
        match found {
            Some(j) => {
                used.insert(j);
                pairs.push((i, j));
            }
            None => src_unpaired.push(i),
        }
    }
    let rt_unpaired = rt_left
        .iter()
        .copied()
        .filter(|j| !used.contains(j))
        .collect();
    (pairs, src_unpaired, rt_unpaired)
}

fn category_of(body: &Body<'_>) -> Category {
    match body {
        Body::Text { .. } => Category::Text,
        Body::Thinking { .. } => Category::Thinking,
        Body::Call { .. } => Category::ToolCall,
        Body::Result { .. } => Category::ToolResult,
        Body::Image { .. } => Category::Image,
        Body::Artifact { .. } => Category::Artifact,
    }
}

/// A short description of an item for an example.
fn describe(body: &Body<'_>, detail: bool) -> Option<String> {
    match body {
        Body::Text { text, .. } | Body::Thinking { text, .. } => Some((*text).to_string()),
        Body::Call { name, input } => Some(call_label(name, input)),
        Body::Result { output, .. } => detail.then(|| project(output).into_text().into_owned()),
        Body::Image { media_type } => Some((*media_type).to_string()),
        Body::Artifact { name } => Some((*name).to_string()),
    }
}

/// `Bash: cargo test`: the tool and its most identifying argument.
fn call_label(name: &str, input: &Value) -> String {
    const PRIMARY: [&str; 6] = ["command", "file_path", "path", "url", "pattern", "query"];
    let arg = input.as_object().and_then(|map| {
        PRIMARY
            .iter()
            .find_map(|key| map.get(*key).and_then(Value::as_str))
    });
    match arg {
        Some(arg) => format!("{name}: {arg}"),
        None => name.to_string(),
    }
}

/// One round of leftover pairing for a kind of block: the pairs, and what
/// stayed unpaired on each side.
type Pairing = (Vec<(usize, usize)>, Vec<usize>, Vec<usize>);

fn leftover_texts(src: &[Item<'_>], rt: &[Item<'_>], s: &[usize], r: &[usize]) -> Pairing {
    fn text<'a>(items: &[Item<'a>], n: usize) -> &'a str {
        match &items[n].body {
            Body::Text { text, .. } | Body::Thinking { text, .. } => text,
            _ => "",
        }
    }
    let keys = |items: &[Item<'_>], n: usize| {
        let tag = u8::from(matches!(items[n].body, Body::Thinking { .. }));
        vec![(tag, lead(text(items, n)))]
    };
    pair_up(s, r, &|i| keys(src, i), &|j| keys(rt, j), &|i, j| {
        // A text never pairs with a thinking block.
        keys(src, i) == keys(rt, j) && related(text(src, i), text(rt, j))
    })
}

fn leftover_calls(src: &[Item<'_>], rt: &[Item<'_>], s: &[usize], r: &[usize]) -> Pairing {
    fn call<'i>(items: &'i [Item<'_>], n: usize) -> Option<(&'i str, &'i Value)> {
        match &items[n].body {
            Body::Call { name, input } => Some((name.as_str(), input)),
            _ => None,
        }
    }
    let keys = |items: &[Item<'_>], n: usize| {
        call(items, n).map_or_else(Vec::new, |(name, input)| call_keys(name, input))
    };
    pair_up(
        s,
        r,
        &|i| keys(src, i),
        &|j| keys(rt, j),
        &|i, j| match (call(src, i), call(rt, j)) {
            (Some(a), Some(b)) => related_calls(a, b),
            _ => false,
        },
    )
}

fn leftover_results(src: &[Item<'_>], rt: &[Item<'_>], s: &[usize], r: &[usize]) -> Pairing {
    /// What each leftover result says, worked out once.
    fn texts<'a>(items: &[Item<'a>], idx: &[usize]) -> HashMap<usize, Cow<'a, str>> {
        idx.iter()
            .filter_map(|&n| match &items[n].body {
                Body::Result { output, .. } => Some((n, project(output).into_text())),
                _ => None,
            })
            .collect()
    }
    let (said_src, said_rt) = (texts(src, s), texts(rt, r));
    let key = |said: &HashMap<usize, Cow<'_, str>>, n: usize| {
        vec![(2, lead(said.get(&n).map_or("", AsRef::as_ref)))]
    };
    pair_up(
        s,
        r,
        &|i| key(&said_src, i),
        &|j| key(&said_rt, j),
        &|i, j| match (said_src.get(&i), said_rt.get(&j)) {
            (Some(a), Some(b)) => related(a, b),
            _ => false,
        },
    )
}

/// Pair leftovers by a coarse label alone (an image's media type, an
/// artifact's name): same label, same item.
fn leftover_labeled(
    src: &[Item<'_>],
    rt: &[Item<'_>],
    s: &[usize],
    r: &[usize],
    label: for<'a> fn(&'a Body<'_>) -> Option<&'a str>,
) -> Pairing {
    let keys = |items: &[Item<'_>], n: usize| {
        label(&items[n].body).map_or_else(Vec::new, |l| {
            let mut h = hasher(14);
            l.hash(&mut h);
            vec![(5, h.finish())]
        })
    };
    pair_up(s, r, &|i| keys(src, i), &|j| keys(rt, j), &|_, _| true)
}

/// Blocks that matched by content may still differ in what rides along:
/// a thinking block's replay token, a result's error flag.
fn compare_matched(src: &[Item<'_>], rt: &[Item<'_>], matched: &[(usize, usize)], acc: &mut Acc) {
    for &(i, j) in matched {
        match (&src[i].body, &rt[j].body) {
            (Body::Thinking { .. }, Body::Thinking { .. }) => {
                compare_token(&src[i], &rt[j], acc);
            }
            (Body::Result { is_error: was, .. }, Body::Result { is_error: now, .. })
                if was != now =>
            {
                if *was {
                    acc.add(
                        LossKind::Lost,
                        Category::IsError,
                        None,
                        Some(src[i].at),
                        || None,
                    );
                } else {
                    acc.add(LossKind::Synthesized, Category::IsError, None, None, || {
                        None
                    });
                }
            }
            _ => {}
        }
    }
}

fn compare_content(src: &[Item<'_>], rt: &[Item<'_>], acc: &mut Acc) {
    let src_keys: Vec<u64> = src.iter().map(|i| i.key).collect();
    let rt_keys: Vec<u64> = rt.iter().map(|i| i.key).collect();
    let (matched, src_left, rt_left) = match_keys(&src_keys, &rt_keys);
    let (detail, cap) = (acc.detail, acc.cap);

    compare_matched(src, rt, &matched, acc);

    let out_of_order = moved(&matched);
    if let Some(&first) = out_of_order.first() {
        acc.add_n(
            LossKind::Changed,
            Category::Order,
            None,
            Some(src[first].at),
            out_of_order.len(),
            || None,
        );
    }

    // Leftovers that resemble each other are one item changed; the rest are
    // lost (source only) or synthesized (round trip only).
    let split = |items: &[Item<'_>], idx: &[usize], category: Category| -> Vec<usize> {
        idx.iter()
            .copied()
            .filter(|&n| category_of(&items[n].body) == category)
            .collect()
    };
    let by = |category: Category| {
        (
            split(src, &src_left, category),
            split(rt, &rt_left, category),
        )
    };
    let mut unpaired_src: Vec<usize> = Vec::new();
    let mut unpaired_rt: Vec<usize> = Vec::new();
    let mut settle = |(pairs, su, ru): Pairing, on_pair: &mut dyn FnMut(usize, usize)| {
        for (i, j) in pairs {
            on_pair(i, j);
        }
        unpaired_src.extend(su);
        unpaired_rt.extend(ru);
    };

    // Text and thinking pair separately from each other.
    for category in [Category::Text, Category::Thinking] {
        let (s, r) = by(category);
        settle(leftover_texts(src, rt, &s, &r), &mut |i, j| {
            changed_text(&src[i], &rt[j], acc);
        });
    }
    let (s, r) = by(Category::ToolCall);
    settle(leftover_calls(src, rt, &s, &r), &mut |i, j| {
        changed_call(&src[i], &rt[j], acc);
    });
    let (s, r) = by(Category::ToolResult);
    settle(leftover_results(src, rt, &s, &r), &mut |i, j| {
        acc.add(
            LossKind::Changed,
            Category::ToolResult,
            None,
            Some(src[i].at),
            || {
                detail.then(|| {
                    arrow(
                        &describe_full(&src[i].body),
                        &describe_full(&rt[j].body),
                        cap,
                    )
                })
            },
        );
    });
    let (s, r) = by(Category::Image);
    settle(
        leftover_labeled(src, rt, &s, &r, |b| match b {
            Body::Image { media_type } => Some(media_type),
            _ => None,
        }),
        &mut |i, _| {
            acc.add(
                LossKind::Changed,
                Category::Image,
                None,
                Some(src[i].at),
                || describe(&src[i].body, false),
            );
        },
    );
    let (s, r) = by(Category::Artifact);
    settle(
        leftover_labeled(src, rt, &s, &r, |b| match b {
            Body::Artifact { name } => Some(name),
            _ => None,
        }),
        &mut |i, _| {
            acc.add(
                LossKind::Changed,
                Category::Artifact,
                None,
                Some(src[i].at),
                || describe(&src[i].body, false),
            );
        },
    );

    emit_unpaired(src, rt, unpaired_src, unpaired_rt, acc);
}

/// What no counterpart was found for: lost from the source, synthesized in
/// the round trip.
fn emit_unpaired(
    src: &[Item<'_>],
    rt: &[Item<'_>],
    mut unpaired_src: Vec<usize>,
    mut unpaired_rt: Vec<usize>,
    acc: &mut Acc,
) {
    let detail = acc.detail;
    unpaired_src.sort_unstable();
    unpaired_rt.sort_unstable();
    for i in unpaired_src {
        let item = &src[i];
        acc.add(
            LossKind::Lost,
            category_of(&item.body),
            None,
            Some(item.at),
            || describe(&item.body, detail),
        );
    }
    for j in unpaired_rt {
        let item = &rt[j];
        acc.add(
            LossKind::Synthesized,
            category_of(&item.body),
            None,
            None,
            || describe(&item.body, detail),
        );
    }
}

fn describe_full(body: &Body<'_>) -> String {
    describe(body, true).unwrap_or_default()
}

/// A matched thinking block whose replay token did not survive intact.
fn compare_token(src: &Item<'_>, rt: &Item<'_>, acc: &mut Acc) {
    let (
        Body::Thinking {
            signature: s_sig,
            encrypted: s_enc,
            ..
        },
        Body::Thinking {
            signature: r_sig,
            encrypted: r_enc,
            ..
        },
    ) = (&src.body, &rt.body)
    else {
        return;
    };
    if (s_sig, s_enc) == (r_sig, r_enc) {
        return;
    }
    let had = s_sig.is_some() || s_enc.is_some();
    let has = r_sig.is_some() || r_enc.is_some();
    let (kind, at) = match (had, has) {
        (true, false) => (LossKind::Lost, Some(src.at)),
        (false, true) => (LossKind::Synthesized, None),
        _ => (LossKind::Changed, Some(src.at)),
    };
    acc.add(kind, Category::ThinkingSignature, None, at, || None);
}

/// Two leftover texts that resemble each other.
fn changed_text(src: &Item<'_>, rt: &Item<'_>, acc: &mut Acc) {
    let cap = acc.cap;
    match (&src.body, &rt.body) {
        (
            Body::Text {
                role: was,
                text: before,
            },
            Body::Text {
                role: now,
                text: after,
            },
        ) => {
            if was != now && same_words(before, after) {
                let name = |r: &Role| {
                    if *r == Role::User {
                        "user"
                    } else {
                        "assistant"
                    }
                };
                acc.add(
                    LossKind::Changed,
                    Category::Text,
                    Some("role".to_string()),
                    Some(src.at),
                    || Some(format!("{} -> {}", name(was), name(now))),
                );
            } else {
                acc.add(
                    LossKind::Changed,
                    Category::Text,
                    None,
                    Some(src.at),
                    || Some(arrow(before, after, cap)),
                );
            }
        }
        (Body::Thinking { text: before, .. }, Body::Thinking { text: after, .. }) => {
            acc.add(
                LossKind::Changed,
                Category::Thinking,
                None,
                Some(src.at),
                || Some(arrow(before, after, cap)),
            );
            compare_token(src, rt, acc);
        }
        _ => {}
    }
}

fn show(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), str::to_string)
}

/// Two leftover calls that are the same call, altered: a new name, or
/// arguments that were dropped or rewritten.
fn changed_call(src: &Item<'_>, rt: &Item<'_>, acc: &mut Acc) {
    let (
        Body::Call {
            name: s_name,
            input: s_in,
        },
        Body::Call {
            name: r_name,
            input: r_in,
        },
    ) = (&src.body, &rt.body)
    else {
        return;
    };
    let at = Some(src.at);
    let cap = acc.cap;
    if s_name != r_name {
        acc.add(
            LossKind::Changed,
            Category::ToolName,
            Some(format!("{s_name} -> {r_name}")),
            at,
            || None,
        );
    }
    match (s_in.as_object(), r_in.as_object()) {
        (Some(before), Some(after)) => {
            fn sorted(
                map: &serde_json::Map<String, Value>,
                keep: impl Fn(&String) -> bool,
            ) -> Vec<&String> {
                let mut keys: Vec<&String> = map.keys().filter(|k| keep(k)).collect();
                keys.sort();
                keys
            }
            for key in sorted(before, |k| after.contains_key(k)) {
                if after[key] != before[key] {
                    acc.add(
                        LossKind::Changed,
                        Category::ToolArgs,
                        Some(format!("{s_name}.{key}")),
                        at,
                        || Some(arrow(&show(&before[key]), &show(&after[key]), cap)),
                    );
                }
            }
            let mut added = sorted(after, |k| !before.contains_key(k));
            for key in sorted(before, |k| !after.contains_key(k)) {
                // The same value under another key is a rename, not a loss.
                if let Some(pos) = added.iter().position(|k| after[*k] == before[key]) {
                    let new = added.remove(pos);
                    acc.add(
                        LossKind::Changed,
                        Category::ToolArgs,
                        Some(format!("{s_name}.{key} -> {new}")),
                        at,
                        || Some(show(&before[key])),
                    );
                } else {
                    acc.add(
                        LossKind::Lost,
                        Category::ToolArgs,
                        Some(format!("{s_name}.{key}")),
                        at,
                        || Some(show(&before[key])),
                    );
                }
            }
            for key in added {
                acc.add(
                    LossKind::Synthesized,
                    Category::ToolArgs,
                    Some(format!("{s_name}.{key}")),
                    None,
                    || Some(show(&after[key])),
                );
            }
        }
        _ if s_in != r_in => acc.add(
            LossKind::Changed,
            Category::ToolArgs,
            Some(s_name.clone()),
            at,
            || Some(arrow(&show(s_in), &show(r_in), cap)),
        ),
        _ => {}
    }
}

// ── accounting ─────────────────────────────────────────────────────────

fn str_key(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn stop_name(reason: &StopReason) -> String {
    match reason {
        StopReason::EndTurn => "end_turn".to_string(),
        StopReason::ToolUse => "tool_use".to_string(),
        StopReason::MaxTokens => "max_tokens".to_string(),
        StopReason::StopSequence => "stop_sequence".to_string(),
        StopReason::Aborted => "aborted".to_string(),
        StopReason::Error => "error".to_string(),
        StopReason::Other(other) => format!("other({other})"),
    }
}

/// Compare per-message values that targets may move between messages: equal
/// values cancel out; when the same number are left on each side they are
/// the same slots changed, otherwise the surplus is lost or synthesized.
fn compare_values(category: Category, src: &[(usize, String)], rt: &[String], acc: &mut Acc) {
    let s_keys: Vec<u64> = src.iter().map(|(_, v)| str_key(v)).collect();
    let r_keys: Vec<u64> = rt.iter().map(|v| str_key(v)).collect();
    let (_, src_left, rt_left) = match_keys(&s_keys, &r_keys);
    let cap = acc.cap;
    if !src_left.is_empty() && src_left.len() == rt_left.len() {
        for (&i, &j) in src_left.iter().zip(&rt_left) {
            acc.add(LossKind::Changed, category, None, Some(src[i].0), || {
                Some(arrow(&src[i].1, &rt[j], cap))
            });
        }
        return;
    }
    for &i in &src_left {
        acc.add(LossKind::Lost, category, None, Some(src[i].0), || {
            Some(src[i].1.clone())
        });
    }
    for &j in &rt_left {
        acc.add(LossKind::Synthesized, category, None, None, || {
            Some(rt[j].clone())
        });
    }
}

/// A usage record's fields. An absent cache count and a zero one both mean
/// "nothing cached", so they compare equal.
fn usage_fields(u: &Usage) -> [(&'static str, u64); 4] {
    [
        ("input_tokens", u.input_tokens),
        ("output_tokens", u.output_tokens),
        (
            "cache_read_input_tokens",
            u.cache_read_input_tokens.unwrap_or(0),
        ),
        (
            "cache_creation_input_tokens",
            u.cache_creation_input_tokens.unwrap_or(0),
        ),
    ]
}

fn compare_usage(src: &[(usize, Usage)], rt: &[Usage], acc: &mut Acc) {
    let key = |u: &Usage| {
        let mut h = DefaultHasher::new();
        usage_fields(u).hash(&mut h);
        h.finish()
    };
    // The same turn with its cache counts dropped still has the same input
    // and output counts.
    let core = |u: &Usage| {
        let mut h = DefaultHasher::new();
        (u.input_tokens, u.output_tokens).hash(&mut h);
        h.finish()
    };
    let s_keys: Vec<u64> = src.iter().map(|(_, u)| key(u)).collect();
    let r_keys: Vec<u64> = rt.iter().map(key).collect();
    let (_, src_left, rt_left) = match_keys(&s_keys, &r_keys);
    let s_cores: Vec<u64> = src_left.iter().map(|&i| core(&src[i].1)).collect();
    let r_cores: Vec<u64> = rt_left.iter().map(|&j| core(&rt[j])).collect();
    let (pairs, s_gone, r_new) = match_keys(&s_cores, &r_cores);
    let mut fieldwise = |i: usize, j: usize| {
        let at = Some(src[i].0);
        for ((name, was), (_, now)) in usage_fields(&src[i].1)
            .into_iter()
            .zip(usage_fields(&rt[j]))
        {
            let path = Some(name.to_string());
            match (was, now) {
                (a, b) if a == b => {}
                (_, 0) => acc.add(LossKind::Lost, Category::Usage, path, at, || None),
                (0, _) => acc.add(LossKind::Synthesized, Category::Usage, path, None, || None),
                (a, b) => acc.add(LossKind::Changed, Category::Usage, path, at, || {
                    Some(format!("{a} -> {b}"))
                }),
            }
        }
    };
    for (a, b) in pairs {
        fieldwise(src_left[a], rt_left[b]);
    }
    // Nothing alike but as many records on each side: the same turns,
    // counted differently.
    if !s_gone.is_empty() && s_gone.len() == r_new.len() {
        for (&a, &b) in s_gone.iter().zip(&r_new) {
            fieldwise(src_left[a], rt_left[b]);
        }
        return;
    }
    for a in s_gone {
        acc.add(
            LossKind::Lost,
            Category::Usage,
            None,
            Some(src[src_left[a]].0),
            || None,
        );
    }
    for _ in r_new {
        acc.add(LossKind::Synthesized, Category::Usage, None, None, || None);
    }
}

fn compare_accounting(src: &Transcript<Common>, rt: &Transcript<Common>, acc: &mut Acc) {
    let usage: Vec<(usize, Usage)> = src
        .body
        .iter()
        .enumerate()
        .filter_map(|(at, m)| m.usage.map(|u| (at, u)))
        .collect();
    let usage_rt: Vec<Usage> = rt.body.iter().filter_map(|m| m.usage).collect();
    compare_usage(&usage, &usage_rt, acc);

    let stops = |t: &Transcript<Common>| -> Vec<(usize, String)> {
        t.body
            .iter()
            .enumerate()
            .filter_map(|(at, m)| m.stop_reason.as_ref().map(|r| (at, stop_name(r))))
            .collect()
    };
    let rt_stops: Vec<String> = stops(rt).into_iter().map(|(_, v)| v).collect();
    compare_values(Category::StopReason, &stops(src), &rt_stops, acc);

    let models = |t: &Transcript<Common>| -> Vec<(usize, String)> {
        t.body
            .iter()
            .enumerate()
            .filter_map(|(at, m)| {
                let model = m.model.as_deref().map(str::trim).filter(|m| !m.is_empty());
                model.map(|model| (at, model.to_string()))
            })
            .collect()
    };
    let rt_models: Vec<String> = models(rt).into_iter().map(|(_, v)| v).collect();
    compare_values(Category::MessageModel, &models(src), &rt_models, acc);

    // Targets that do not keep message times leave the source's seconds
    // missing from the round trip.
    let kept: HashSet<i64> = rt.body.iter().map(|m| m.timestamp.timestamp()).collect();
    for (at, m) in src.body.iter().enumerate() {
        if !kept.contains(&m.timestamp.timestamp()) {
            acc.add(
                LossKind::Lost,
                Category::Timestamp,
                Some("message".to_string()),
                Some(at),
                || Some(m.timestamp.to_rfc3339()),
            );
        }
    }
}

// ── metadata ───────────────────────────────────────────────────────────

fn compare_field(category: Category, was: Option<&str>, now: Option<&str>, acc: &mut Acc) {
    // An empty value is no value.
    let (was, now) = (was.filter(|s| !s.is_empty()), now.filter(|s| !s.is_empty()));
    let cap = acc.cap;
    match (was, now) {
        (Some(a), None) => acc.add(LossKind::Lost, category, None, None, || Some(a.to_string())),
        (Some(a), Some(b)) if a != b => {
            acc.add(LossKind::Changed, category, None, None, || {
                Some(arrow(a, b, cap))
            });
        }
        (None, Some(b)) => acc.add(LossKind::Synthesized, category, None, None, || {
            Some(b.to_string())
        }),
        _ => {}
    }
}

fn compare_meta(src: &Transcript<Common>, rt: &Transcript<Common>, acc: &mut Acc) {
    let (s, r) = (&src.meta, &rt.meta);
    compare_field(
        Category::Title,
        s.title.as_deref().map(str::trim),
        r.title.as_deref().map(str::trim),
        acc,
    );
    compare_field(Category::Model, s.model.as_deref(), r.model.as_deref(), acc);
    compare_field(
        Category::GitBranch,
        s.git_branch.as_deref(),
        r.git_branch.as_deref(),
        acc,
    );
    compare_field(Category::Cwd, s.cwd.as_deref(), r.cwd.as_deref(), acc);
    let lineage =
        |l: &crate::common::Lineage| format!("{:?} of {}", l.relation, l.parent).to_lowercase();
    compare_field(
        Category::Lineage,
        s.lineage.as_ref().map(lineage).as_deref(),
        r.lineage.as_ref().map(lineage).as_deref(),
        acc,
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::*;
    use crate::common::{ImageSource, Lineage, Message, Meta, Relation, Tool};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
    }

    fn meta() -> Meta {
        Meta {
            id: "t".into(),
            timestamp: at(0),
            cwd: None,
            git_branch: None,
            title: None,
            cli_version: None,
            model: None,
            lineage: None,
        }
    }

    fn msg(role: Role, content: Vec<Block>, secs: i64) -> Message {
        Message {
            role,
            content,
            timestamp: at(secs),
            model: None,
            stop_reason: None,
            usage: None,
        }
    }

    fn text(s: &str) -> Block {
        Block::Text { text: s.into() }
    }

    fn call(id: &str, name: &str, input: Value) -> Block {
        Block::ToolUse {
            id: id.into(),
            tool: Tool::from_canonical(name, input),
        }
    }

    fn result(id: &str, out: ToolOutput) -> Block {
        Block::ToolResult {
            tool_use_id: id.into(),
            content: out,
            is_error: false,
        }
    }

    fn thinking(s: &str, signature: Option<&str>) -> Block {
        Block::Thinking {
            text: s.into(),
            signature: signature.map(Into::into),
            encrypted: None,
        }
    }

    fn convo(messages: Vec<Message>) -> Transcript<Common> {
        Transcript::new(meta(), messages)
    }

    fn user(blocks: Vec<Block>) -> Message {
        msg(Role::User, blocks, 1)
    }

    fn assistant(blocks: Vec<Block>) -> Message {
        msg(Role::Assistant, blocks, 2)
    }

    fn run(src: &Transcript<Common>, rt: &Transcript<Common>) -> Report {
        compare(HarnessId::Simple, src, rt, Options::default())
    }

    /// The `(kind, category, path, count)` of every item.
    fn shape(report: &Report) -> Vec<(LossKind, Category, Option<&str>, usize)> {
        report
            .items
            .iter()
            .map(|i| (i.kind, i.category, i.path.as_deref(), i.count))
            .collect()
    }

    /// Assert that a comparison found nothing.
    fn assert_clean(report: &Report) {
        assert_eq!(report.items.len(), 0, "{:?}", report.items);
    }

    fn base() -> Transcript<Common> {
        convo(vec![
            user(vec![text("fix it")]),
            assistant(vec![
                thinking("hmm", Some("sig")),
                text("on it"),
                call(
                    "c1",
                    "Bash",
                    json!({"command": "ls", "description": "list"}),
                ),
            ]),
            user(vec![result("c1", ToolOutput::Text("a\nb".into()))]),
        ])
    }

    #[test]
    fn identical_transcripts_have_no_items() {
        let src = base();
        let report = run(&src, &src.clone());
        assert!(report.items.is_empty(), "{report:?}");
        assert_eq!(report.severity, Severity::None);
        assert_eq!(report.warnings.len(), 0);
        assert_eq!(report.messages_before, report.messages_after);
    }

    #[test]
    fn lost_text_is_content() {
        let src = convo(vec![user(vec![text("one"), text("two")])]);
        let rt = convo(vec![user(vec![text("one")])]);
        let report = run(&src, &rt);
        assert_eq!(shape(&report), [(LossKind::Lost, Category::Text, None, 1)]);
        assert_eq!(report.items[0].example.as_deref(), Some("two"));
        assert_eq!(report.items[0].first_at, Some(0));
        assert_eq!(report.severity, Severity::Content);
    }

    #[test]
    fn synthesized_text_is_listed_but_does_not_raise_severity() {
        let src = convo(vec![user(vec![text("one")])]);
        let rt = convo(vec![user(vec![text("one"), text("extra")])]);
        let report = run(&src, &rt);
        assert_eq!(
            shape(&report),
            [(LossKind::Synthesized, Category::Text, None, 1)]
        );
        assert_eq!(report.items[0].first_at, None);
        assert_eq!(report.severity, Severity::None);
    }

    #[test]
    fn edited_text_is_changed_once_not_lost_and_synthesized() {
        let src = convo(vec![user(vec![text("please fix the parser bug today")])]);
        let rt = convo(vec![user(vec![text("please fix the parser bug")])]);
        let report = run(&src, &rt);
        assert_eq!(
            shape(&report),
            [(LossKind::Changed, Category::Text, None, 1)]
        );
        assert_eq!(
            report.items[0].example.as_deref(),
            Some("please fix the parser bug today -> please fix the parser bug")
        );
    }

    #[test]
    fn unrelated_replacement_text_is_lost_plus_synthesized() {
        let src = convo(vec![user(vec![text("alpha beta gamma")])]);
        let rt = convo(vec![user(vec![text("omega psi chi")])]);
        let report = run(&src, &rt);
        assert_eq!(
            shape(&report),
            [
                (LossKind::Lost, Category::Text, None, 1),
                (LossKind::Synthesized, Category::Text, None, 1)
            ]
        );
    }

    #[test]
    fn surrounding_whitespace_is_not_a_change() {
        let src = convo(vec![user(vec![text("  hello\n")])]);
        let rt = convo(vec![user(vec![text("hello")])]);
        assert_clean(&run(&src, &rt));
    }

    #[test]
    fn empty_text_blocks_are_ignored() {
        let src = convo(vec![user(vec![text("hello")])]);
        let rt = convo(vec![user(vec![text("hello"), text("  ")])]);
        assert_clean(&run(&src, &rt));
    }

    #[test]
    fn a_text_changing_role_is_a_changed_text() {
        let src = convo(vec![user(vec![text("same words")])]);
        let rt = convo(vec![msg(Role::Assistant, vec![text("same words")], 1)]);
        let report = run(&src, &rt);
        assert_eq!(
            shape(&report),
            [(LossKind::Changed, Category::Text, Some("role"), 1)]
        );
        assert_eq!(
            report.items[0].example.as_deref(),
            Some("user -> assistant")
        );
    }

    #[test]
    fn thinking_lost_and_changed() {
        let src = convo(vec![assistant(vec![thinking(
            "deep thoughts about it",
            None,
        )])]);
        let lost = run(&src, &convo(vec![assistant(vec![])]));
        assert_eq!(
            shape(&lost),
            [(LossKind::Lost, Category::Thinking, None, 1)]
        );
        let changed = run(
            &src,
            &convo(vec![assistant(vec![thinking("deep thoughts about", None)])]),
        );
        assert_eq!(
            shape(&changed),
            [(LossKind::Changed, Category::Thinking, None, 1)]
        );
    }

    #[test]
    fn thinking_signature_lost_changed_synthesized() {
        let signed = convo(vec![assistant(vec![thinking("hmm", Some("a"))])]);
        let bare = convo(vec![assistant(vec![thinking("hmm", None)])]);
        let other = convo(vec![assistant(vec![thinking("hmm", Some("b"))])]);
        assert_eq!(
            shape(&run(&signed, &bare)),
            [(LossKind::Lost, Category::ThinkingSignature, None, 1)]
        );
        assert_eq!(
            shape(&run(&signed, &other)),
            [(LossKind::Changed, Category::ThinkingSignature, None, 1)]
        );
        assert_eq!(
            shape(&run(&bare, &signed)),
            [(LossKind::Synthesized, Category::ThinkingSignature, None, 1)]
        );
        assert_eq!(run(&signed, &bare).severity, Severity::Accounting);
    }

    #[test]
    fn a_dropped_tool_call_is_lost_with_its_label() {
        let src = base();
        let mut rt = base();
        rt.body[1].content.pop();
        let report = run(&src, &rt);
        assert!(
            report.items.iter().any(|i| i.kind == LossKind::Lost
                && i.category == Category::ToolCall
                && i.example.as_deref() == Some("Bash: ls")),
            "{report:?}"
        );
    }

    #[test]
    fn a_renamed_tool_is_changed_by_name_only() {
        let src = convo(vec![user(vec![call(
            "c1",
            "/release",
            json!({"args": "patch"}),
        )])]);
        let rt = convo(vec![user(vec![call(
            "c1",
            "_release",
            json!({"args": "patch"}),
        )])]);
        let report = run(&src, &rt);
        assert_eq!(
            shape(&report),
            [(
                LossKind::Changed,
                Category::ToolName,
                Some("/release -> _release"),
                1
            )]
        );
    }

    #[test]
    fn dropped_rewritten_and_added_tool_arguments() {
        let src = convo(vec![assistant(vec![call(
            "c1",
            "Bash",
            json!({"command": "ls", "description": "list", "timeout_ms": 5}),
        )])]);
        let rt = convo(vec![assistant(vec![call(
            "c1",
            "Bash",
            json!({"command": "ls", "timeout_ms": 9, "workdir": "/x"}),
        )])]);
        let report = run(&src, &rt);
        assert_eq!(
            shape(&report),
            [
                (
                    LossKind::Lost,
                    Category::ToolArgs,
                    Some("Bash.description"),
                    1
                ),
                (
                    LossKind::Changed,
                    Category::ToolArgs,
                    Some("Bash.timeout_ms"),
                    1
                ),
                (
                    LossKind::Synthesized,
                    Category::ToolArgs,
                    Some("Bash.workdir"),
                    1
                ),
            ]
        );
        assert_eq!(report.items[0].example.as_deref(), Some("list"));
        assert_eq!(report.items[1].example.as_deref(), Some("5 -> 9"));
    }

    #[test]
    fn a_call_with_nothing_in_common_is_lost_not_changed() {
        let src = convo(vec![assistant(vec![call(
            "c1",
            "Bash",
            json!({"command": "ls"}),
        )])]);
        let rt = convo(vec![assistant(vec![call(
            "c1",
            "Bash",
            json!({"command": "pwd"}),
        )])]);
        let report = run(&src, &rt);
        assert_eq!(
            shape(&report),
            [
                (LossKind::Lost, Category::ToolCall, None, 1),
                (LossKind::Synthesized, Category::ToolCall, None, 1)
            ]
        );
    }

    #[test]
    fn json_formatting_is_not_loss() {
        // Key order and whitespace in a result held as text, or in a
        // structured result, never count.
        let src = convo(vec![
            assistant(vec![call(
                "c1",
                "mcp__x__y",
                json!({"b": 1, "a": {"y": [1, 2], "x": null}}),
            )]),
            user(vec![result(
                "c1",
                ToolOutput::Text("{\"a\": 1,\n  \"b\": [1,\n 2]}".into()),
            )]),
        ]);
        let rt = convo(vec![
            assistant(vec![call(
                "c9",
                "mcp__x__y",
                json!({"a": {"x": null, "y": [1, 2]}, "b": 1}),
            )]),
            user(vec![result(
                "c9",
                ToolOutput::Json(json!({"b": [1, 2], "a": 1})),
            )]),
        ]);
        let report = run(&src, &rt);
        assert!(report.items.is_empty(), "{report:?}");
    }

    #[test]
    fn a_list_of_text_parts_equals_its_text() {
        let src = convo(vec![user(vec![result(
            "c1",
            ToolOutput::Json(json!([{"type": "text", "text": "hello"}])),
        )])]);
        let rt = convo(vec![user(vec![result(
            "c1",
            ToolOutput::Text("hello".into()),
        )])]);
        assert_clean(&run(&src, &rt));
    }

    #[test]
    fn a_changed_result_hides_its_body_unless_detail() {
        let src = convo(vec![user(vec![result(
            "c1",
            ToolOutput::Text("secret output line one and more".into()),
        )])]);
        let rt = convo(vec![user(vec![result(
            "c1",
            ToolOutput::Text("secret output line one".into()),
        )])]);
        let plain = run(&src, &rt);
        assert_eq!(
            shape(&plain),
            [(LossKind::Changed, Category::ToolResult, None, 1)]
        );
        assert_eq!(plain.items[0].example, None);
        let detailed = compare(HarnessId::Simple, &src, &rt, Options { detail: true });
        assert!(
            detailed.items[0]
                .example
                .as_deref()
                .is_some_and(|e| e.contains("secret"))
        );
    }

    #[test]
    fn a_lost_result_never_shows_its_body() {
        let src = convo(vec![user(vec![result(
            "c1",
            ToolOutput::Text("password=hunter2".into()),
        )])]);
        let report = run(&src, &convo(vec![user(vec![])]));
        assert_eq!(
            shape(&report),
            [(LossKind::Lost, Category::ToolResult, None, 1)]
        );
        assert_eq!(report.items[0].example, None);
    }

    #[test]
    fn error_flag_lost_and_synthesized() {
        let flagged = |is_error| {
            convo(vec![user(vec![Block::ToolResult {
                tool_use_id: "c1".into(),
                content: ToolOutput::Text("boom".into()),
                is_error,
            }])])
        };
        assert_eq!(
            shape(&run(&flagged(true), &flagged(false))),
            [(LossKind::Lost, Category::IsError, None, 1)]
        );
        assert_eq!(
            shape(&run(&flagged(false), &flagged(true))),
            [(LossKind::Synthesized, Category::IsError, None, 1)]
        );
    }

    #[test]
    fn images_and_artifacts() {
        let image = |data: &str| Block::Image {
            source: ImageSource {
                source_type: "base64".into(),
                media_type: "image/png".into(),
                data: data.into(),
            },
        };
        let art = |text: &str| Block::Artifact {
            artifact: Artifact {
                id: "a".into(),
                name: "report.md".into(),
                source: ArtifactSource::Text {
                    text: text.into(),
                    media_type: None,
                },
            },
        };
        let src = convo(vec![user(vec![image("AAAA"), art("# one")])]);
        let gone = run(&src, &convo(vec![user(vec![])]));
        assert_eq!(
            shape(&gone),
            [
                (LossKind::Lost, Category::Image, None, 1),
                (LossKind::Lost, Category::Artifact, None, 1)
            ]
        );
        assert_eq!(gone.items[0].example.as_deref(), Some("image/png"));
        assert_eq!(gone.items[1].example.as_deref(), Some("report.md"));
        // A re-encoded image is a change; an artifact is its name, since its
        // content lives in a file of the store's making.
        let edited = run(&src, &convo(vec![user(vec![image("BBBB"), art("# two")])]));
        assert_eq!(
            shape(&edited),
            [(LossKind::Changed, Category::Image, None, 1)]
        );
    }

    #[test]
    fn an_artifact_that_comes_back_as_text_is_lost() {
        // Harnesses without a file carrier keep only a mention in the text;
        // claude_code keeps the artifact itself.
        let src = convo(vec![assistant(vec![Block::Artifact {
            artifact: Artifact {
                id: "a".into(),
                name: "report.md".into(),
                source: ArtifactSource::Text {
                    text: "# one".into(),
                    media_type: None,
                },
            },
        }])]);
        let kept = assess(HarnessId::ClaudeCode, &src).unwrap();
        assert!(
            kept.items.iter().all(|i| i.category != Category::Artifact),
            "{:?}",
            kept.items
        );
        let mention = assess(HarnessId::Cursor, &src).unwrap();
        assert!(
            mention
                .items
                .iter()
                .any(|i| i.kind == LossKind::Lost && i.category == Category::Artifact),
            "{:?}",
            mention.items
        );
    }

    #[test]
    fn split_and_merged_messages_are_not_loss() {
        let blocks = vec![
            text("first"),
            call("c1", "Bash", json!({"command": "ls"})),
            text("last"),
        ];
        let merged = convo(vec![assistant(blocks.clone())]);
        let split = convo(blocks.into_iter().map(|b| assistant(vec![b])).collect());
        assert_clean(&run(&merged, &split));
        let back = run(&split, &merged);
        assert_clean(&back);
        assert_eq!(back.messages_before, 3);
        assert_eq!(back.messages_after, 1);
    }

    #[test]
    fn reordered_blocks_are_a_content_change() {
        let src = convo(vec![assistant(vec![
            text("one"),
            text("two"),
            text("three"),
        ])]);
        let rt = convo(vec![assistant(vec![
            text("three"),
            text("one"),
            text("two"),
        ])]);
        let report = run(&src, &rt);
        assert_eq!(
            shape(&report),
            [(LossKind::Changed, Category::Order, None, 1)]
        );
        assert_eq!(report.severity, Severity::Content);
    }

    fn with_usage(n: u64, cache: Option<u64>) -> Transcript<Common> {
        let mut t = convo(vec![assistant(vec![text("hi")])]);
        t.body[0].usage = Some(Usage {
            input_tokens: n,
            output_tokens: 5,
            cache_read_input_tokens: cache,
            cache_creation_input_tokens: None,
        });
        t
    }

    #[test]
    fn usage_lost_changed_field_by_field_and_synthesized() {
        let plain = convo(vec![assistant(vec![text("hi")])]);
        assert_eq!(
            shape(&run(&with_usage(10, None), &plain)),
            [(LossKind::Lost, Category::Usage, None, 1)]
        );
        assert_eq!(
            shape(&run(&plain, &with_usage(10, None))),
            [(LossKind::Synthesized, Category::Usage, None, 1)]
        );
        assert_eq!(
            shape(&run(&with_usage(10, Some(7)), &with_usage(10, None))),
            [(
                LossKind::Lost,
                Category::Usage,
                Some("cache_read_input_tokens"),
                1
            )]
        );
        assert_eq!(
            shape(&run(&with_usage(10, None), &with_usage(12, None))),
            [(LossKind::Changed, Category::Usage, Some("input_tokens"), 1)]
        );
    }

    #[test]
    fn an_absent_cache_count_equals_zero() {
        assert_clean(&run(&with_usage(10, None), &with_usage(10, Some(0))));
        assert_clean(&run(&with_usage(10, Some(0)), &with_usage(10, None)));
    }

    fn with_stop(reason: Option<StopReason>) -> Transcript<Common> {
        let mut t = convo(vec![assistant(vec![text("hi")])]);
        t.body[0].stop_reason = reason;
        t
    }

    #[test]
    fn stop_reason_lost_changed_synthesized() {
        let ended = with_stop(Some(StopReason::EndTurn));
        let none = with_stop(None);
        assert_eq!(
            shape(&run(&ended, &none)),
            [(LossKind::Lost, Category::StopReason, None, 1)]
        );
        assert_eq!(
            shape(&run(&none, &ended)),
            [(LossKind::Synthesized, Category::StopReason, None, 1)]
        );
        // Same slot, other representation: one change, not a loss plus a
        // synthesis.
        let aborted = with_stop(Some(StopReason::Aborted));
        let other = with_stop(Some(StopReason::Other("aborted".into())));
        let report = run(&aborted, &other);
        assert_eq!(
            shape(&report),
            [(LossKind::Changed, Category::StopReason, None, 1)]
        );
        assert_eq!(
            report.items[0].example.as_deref(),
            Some("aborted -> other(aborted)")
        );
        assert_eq!(report.severity, Severity::Accounting);
    }

    #[test]
    fn unequal_stop_reason_leftovers_are_not_paired() {
        let mut src = convo(vec![assistant(vec![text("a")]), assistant(vec![text("b")])]);
        src.body[0].stop_reason = Some(StopReason::EndTurn);
        src.body[1].stop_reason = Some(StopReason::Aborted);
        let mut rt = src.clone();
        rt.body[0].stop_reason = Some(StopReason::ToolUse);
        rt.body[1].stop_reason = None;
        let report = run(&src, &rt);
        assert_eq!(
            shape(&report),
            [
                (LossKind::Lost, Category::StopReason, None, 2),
                (LossKind::Synthesized, Category::StopReason, None, 1)
            ]
        );
    }

    #[test]
    fn message_model_lost_changed_synthesized() {
        let model = |m: Option<&str>| {
            let mut t = convo(vec![assistant(vec![text("hi")])]);
            t.body[0].model = m.map(Into::into);
            t
        };
        assert_eq!(
            shape(&run(&model(Some("m1")), &model(None))),
            [(LossKind::Lost, Category::MessageModel, None, 1)]
        );
        assert_eq!(
            shape(&run(&model(Some("m1")), &model(Some("m2")))),
            [(LossKind::Changed, Category::MessageModel, None, 1)]
        );
        assert_eq!(
            shape(&run(&model(None), &model(Some("m1")))),
            [(LossKind::Synthesized, Category::MessageModel, None, 1)]
        );
    }

    #[test]
    fn message_times_are_compared_by_second() {
        let src = convo(vec![
            msg(Role::User, vec![text("a")], 1),
            msg(Role::User, vec![text("b")], 2),
        ]);
        let mut rt = src.clone();
        rt.body[1].timestamp = at(50);
        let report = run(&src, &rt);
        assert_eq!(
            shape(&report),
            [(LossKind::Lost, Category::Timestamp, Some("message"), 1)]
        );
        assert_eq!(report.items[0].first_at, Some(1));
        assert_eq!(report.severity, Severity::Metadata);
        // Sub-second precision is not a loss.
        let mut fine = src.clone();
        fine.body[0].timestamp += chrono::Duration::milliseconds(400);
        assert_clean(&run(&src, &fine));
    }

    #[test]
    fn session_metadata_lost_changed_synthesized() {
        let mut src = base();
        src.meta.title = Some("A title".into());
        src.meta.model = Some("m1".into());
        src.meta.git_branch = Some("main".into());
        src.meta.cwd = Some("/work".into());
        src.meta.lineage = Some(Lineage {
            parent: "p1".into(),
            relation: Relation::Fork,
        });
        let mut rt = base();
        rt.meta.title = Some("Another".into());
        rt.meta.model = None;
        rt.meta.cwd = Some("/work".into());
        rt.meta.git_branch = None;
        let report = run(&src, &rt);
        assert_eq!(
            shape(&report),
            [
                (LossKind::Changed, Category::Title, None, 1),
                (LossKind::Lost, Category::Model, None, 1),
                (LossKind::Lost, Category::GitBranch, None, 1),
                (LossKind::Lost, Category::Lineage, None, 1),
            ]
        );
        assert_eq!(report.items[3].example.as_deref(), Some("fork of p1"));
        assert_eq!(report.severity, Severity::Metadata);

        let mut gained = base();
        gained.meta.cwd = Some("/new".into());
        assert_eq!(
            shape(&run(&base(), &gained)),
            [(LossKind::Synthesized, Category::Cwd, None, 1)]
        );
        // Titles compare trimmed.
        let mut padded = base();
        padded.meta.title = Some("T ".into());
        let mut bare = base();
        bare.meta.title = Some("T".into());
        assert_clean(&run(&padded, &bare));
    }

    #[test]
    fn the_session_start_is_not_reported() {
        let src = base();
        let mut rt = base();
        rt.meta.timestamp = at(3600);
        assert_clean(&run(&src, &rt));
    }

    #[test]
    fn an_empty_session_warns() {
        let src = convo(vec![]);
        let report = run(&src, &src.clone());
        assert_clean(&report);
        assert_eq!(report.severity, Severity::None);
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].contains("0 messages"));
    }

    #[test]
    fn severity_is_ordered_and_takes_the_worst_item() {
        assert!(Severity::None < Severity::Metadata);
        assert!(Severity::Metadata < Severity::Accounting);
        assert!(Severity::Accounting < Severity::Content);

        let mut src = base();
        src.meta.title = Some("t".into());
        src.body[1].stop_reason = Some(StopReason::EndTurn);
        let mut rt = src.clone();
        rt.meta.title = None;
        assert_eq!(run(&src, &rt).severity, Severity::Metadata);
        rt.body[1].stop_reason = None;
        assert_eq!(run(&src, &rt).severity, Severity::Accounting);
        rt.body[0].content.clear();
        assert_eq!(run(&src, &rt).severity, Severity::Content);
    }

    #[test]
    fn categories_map_to_their_severity_class() {
        assert_eq!(Category::Text.severity(), Severity::Content);
        assert_eq!(Category::IsError.severity(), Severity::Content);
        assert_eq!(Category::Order.severity(), Severity::Content);
        assert_eq!(Category::Usage.severity(), Severity::Accounting);
        assert_eq!(Category::ThinkingSignature.severity(), Severity::Accounting);
        assert_eq!(Category::Lineage.severity(), Severity::Metadata);
        assert_eq!(Category::Timestamp.severity(), Severity::Metadata);
    }

    #[test]
    fn examples_are_capped_and_single_line() {
        let long = format!("start\n\u{1b}[31m{}", "x".repeat(500));
        let src = convo(vec![user(vec![text(&long)])]);
        let report = run(&src, &convo(vec![user(vec![])]));
        let example = report.items[0].example.as_deref().unwrap();
        assert_eq!(example.chars().count(), EXAMPLE_CAP);
        assert!(example.ends_with('…'));
        assert!(!example.chars().any(char::is_control), "{example:?}");
        let detailed = compare(
            HarnessId::Simple,
            &src,
            &convo(vec![user(vec![])]),
            Options { detail: true },
        );
        let longer = detailed.items[0].example.as_deref().unwrap();
        assert_eq!(longer.chars().count(), DETAIL_CAP);
    }

    #[test]
    fn clip_handles_edges() {
        assert_eq!(clip("abc", 80), "abc");
        assert_eq!(clip("  abc  ", 80), "abc");
        assert_eq!(clip("abcd", 4), "abcd");
        assert_eq!(clip("abcde", 4), "abc…");
        assert_eq!(clip("", 4), "");
        assert_eq!(clip("日本語のコメントも直して", 5), "日本語の…");
    }

    #[test]
    fn comparison_is_deterministic() {
        let src = base();
        let mut rt = base();
        rt.body[1].content.truncate(1);
        rt.meta.title = Some("x".into());
        let first = run(&src, &rt);
        for _ in 0..20 {
            assert_eq!(run(&src, &rt), first);
        }
    }

    #[test]
    fn duplicate_blocks_match_one_to_one() {
        let src = convo(vec![user(vec![text("ok"), text("ok"), text("ok")])]);
        let rt = convo(vec![user(vec![text("ok")])]);
        let report = run(&src, &rt);
        assert_eq!(shape(&report), [(LossKind::Lost, Category::Text, None, 2)]);
    }

    #[test]
    fn assess_round_trips_through_the_real_codec() {
        let src = base();
        let report = assess(HarnessId::Simple, &src).unwrap();
        assert!(report.items.is_empty(), "{report:?}");
        assert_eq!(report.target, HarnessId::Simple);
        let serialized = serde_json::to_value(&report).unwrap();
        assert_eq!(serialized["target"], "simple");
        assert_eq!(serialized["severity"], "none");
        assert_eq!(serialized["items"], json!([]));
    }

    #[test]
    fn check_all_covers_every_target_and_survives_an_empty_session() {
        let results = check_all(&convo(vec![]));
        assert_eq!(results.len(), TARGETS.len());
        for (target, report) in results {
            let report = report.unwrap_or_else(|e| panic!("{target}: {e}"));
            assert_eq!(report.messages_before, 0);
            assert_eq!(report.warnings.len(), 1);
        }
    }

    #[test]
    fn read_only_sources_are_not_targets() {
        for harness in [
            HarnessId::ClaudeChat,
            HarnessId::CoworkRemote,
            HarnessId::ChatGpt,
        ] {
            assert!(!TARGETS.contains(&harness));
        }
        for harness in HarnessId::ALL {
            let readonly = matches!(
                harness,
                HarnessId::ClaudeChat | HarnessId::CoworkRemote | HarnessId::ChatGpt
            );
            assert_eq!(TARGETS.contains(&harness), !readonly, "{harness}");
        }
    }

    #[test]
    fn re_spaced_json_inside_text_parts_is_not_a_change() {
        // Found by hand: a target re-serialized the JSON a result carried as
        // the text of a text part, compactly; that is formatting, not loss.
        let parts = |text: &str| ToolOutput::Json(json!([{"type": "text", "text": text}]));
        let src = convo(vec![user(vec![result(
            "c1",
            parts("{\"a\": 1,\n  \"b\": [1, 2]}"),
        )])]);
        let rt = convo(vec![user(vec![result(
            "c1",
            parts("{\"a\":1,\"b\":[1,2]}"),
        )])]);
        assert_clean(&run(&src, &rt));
        // A real difference inside it still counts.
        let other = convo(vec![user(vec![result(
            "c1",
            parts("{\"a\":2,\"b\":[1,2]}"),
        )])]);
        assert_eq!(
            shape(&run(&src, &other)),
            [
                (LossKind::Lost, Category::ToolResult, None, 1),
                (LossKind::Synthesized, Category::ToolResult, None, 1)
            ]
        );
    }

    #[test]
    fn nested_stringified_json_is_bounded() {
        let mut text = "{\"k\": 1}".to_string();
        // Each layer doubles the escaping, so a dozen is already deep.
        for _ in 0..12 {
            text = serde_json::to_string(&json!([text])).unwrap();
        }
        let src = convo(vec![user(vec![result("c1", ToolOutput::Text(text))])]);
        assert_clean(&run(&src, &src.clone()));
    }

    #[test]
    fn empty_models_and_titles_are_no_value() {
        // Found by hand: pi writes an empty model on every turn it has none
        // for, which is not a per-message model.
        let mut src = convo(vec![assistant(vec![text("hi")])]);
        let mut rt = src.clone();
        rt.body[0].model = Some(String::new());
        rt.meta.title = Some(String::new());
        assert_clean(&run(&src, &rt));
        src.body[0].model = Some(" ".into());
        assert_clean(&run(&src, &rt));
    }

    #[test]
    fn a_cache_count_dropped_from_fewer_records_is_both_a_field_and_a_record_loss() {
        // Found by hand: cursor_desktop drops cache counts and merges turns.
        let record = |input: u64, cache: Option<u64>| Usage {
            input_tokens: input,
            output_tokens: 5,
            cache_read_input_tokens: cache,
            cache_creation_input_tokens: None,
        };
        let mut src = convo(vec![assistant(vec![text("a")]); 3]);
        for (n, m) in src.body.iter_mut().enumerate() {
            m.usage = Some(record(10 + n as u64, Some(7)));
        }
        let mut rt = convo(vec![assistant(vec![text("a")]); 2]);
        rt.body[0].usage = Some(record(10, None));
        rt.body[1].usage = Some(record(11, None));
        let report = run(&src, &rt);
        assert_eq!(
            shape(&report),
            [
                (LossKind::Lost, Category::Text, None, 1),
                (LossKind::Lost, Category::Usage, None, 1),
                (
                    LossKind::Lost,
                    Category::Usage,
                    Some("cache_read_input_tokens"),
                    2
                ),
            ]
        );
    }

    #[test]
    fn arrow_shows_whitespace_and_cuts_around_the_difference() {
        assert_eq!(arrow("# notes\n", "# notes", 80), "# notes\\n -> # notes");
        let head = "x".repeat(300);
        let a = format!("{head} alpha {}", "y".repeat(300));
        let b = format!("{head} beta {}", "y".repeat(300));
        let shown = arrow(&a, &b, 80);
        assert!(shown.chars().count() <= 80, "{shown}");
        assert!(shown.contains("alpha") && shown.contains("beta"), "{shown}");
        assert!(shown.starts_with('…'), "{shown}");
    }

    #[test]
    fn many_look_alike_leftovers_are_bounded_and_counted() {
        // Results that open alike but are different never pair; each is
        // compared against a bounded number of candidates.
        let results = |tail: &str| -> Vec<Message> {
            (0..2000)
                .map(|n| {
                    user(vec![result(
                        "c",
                        ToolOutput::Text(format!("output{tail}{n}")),
                    )])
                })
                .collect()
        };
        let report = run(&convo(results("-a-")), &convo(results("-b-")));
        assert_eq!(
            shape(&report),
            [
                (LossKind::Lost, Category::ToolResult, None, 2000),
                (LossKind::Synthesized, Category::ToolResult, None, 2000),
            ]
        );
    }

    #[test]
    fn loss_serializes_without_empty_fields() {
        let src = convo(vec![user(vec![result("c1", ToolOutput::Text("x".into()))])]);
        let report = run(&src, &convo(vec![user(vec![])]));
        let value = serde_json::to_value(&report.items[0]).unwrap();
        assert_eq!(
            value,
            json!({"kind": "lost", "category": "tool_result", "count": 1, "first_at": 0})
        );
    }
}
