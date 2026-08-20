//! Fuzzy and substring search over transcripts (feature `search`).
//!
//! Two entry points, sharing [`Query`] and [`Hit`]:
//!
//! - [`search`] — one-shot, stateless: the matching lines of a single
//!   [`Transcript<Common>`].
//! - [`Index`] — [`Index::insert`] transcripts once, then [`Index::query`]
//!   per keystroke. Text is extracted and pre-converted to UTF-32 at insert;
//!   queries scan without parsing or transcoding.
//!
//! Matching is [nucleo](https://github.com/helix-editor/nucleo), helix's
//! matcher, with smart case and Latin diacritic folding. [`Mode::Fuzzy`]
//! accepts the full fzf-style pattern language (`foo bar` all-of, `'exact`,
//! `^prefix`, `suffix$`, `!not`); [`Mode::Substring`] treats the pattern as
//! one literal needle. Ranking adds a literal-occurrence tier above gapped
//! fuzzy matches; nucleo scoring orders lines within each tier.

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str, Utf32String};
use serde::{Deserialize, Serialize};

use crate::common::{Block, Message, Meta, Role, Tool, ToolOutput};
use crate::{Common, HarnessId, Span, Transcript};

// ── query ──────────────────────────────────────────────────────────────

/// How the pattern matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// fzf-style fuzzy: space-separated atoms must all match; an atom may use
    /// `'exact`, `^prefix`, `suffix$`, `!negate` syntax.
    Fuzzy,
    /// The whole pattern is one literal substring, spaces included.
    Substring,
}

/// Case sensitivity. [`Case::Smart`] is case-insensitive until the pattern
/// contains an uppercase character (vim smartcase).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Case {
    Smart,
    Insensitive,
    Sensitive,
}

/// What kind of content a line came from — the "what am I looking at" label
/// on every [`Hit`], and the filter axis of [`Query::origins`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Text the user typed.
    User,
    /// Text the assistant wrote.
    Assistant,
    /// Model reasoning.
    Thinking,
    /// Tool inputs: the Bash command, the file path, the written content.
    ToolUse,
    /// Tool outputs. Excluded from [`Origin::DEFAULT`].
    ToolResult,
    /// Session metadata: title, cwd, git branch. [`Hit::span`] is empty and
    /// [`Hit::block`] is `0` for these.
    Meta,
}

impl Origin {
    /// Every origin, [`Origin::ToolResult`] included.
    pub const ALL: [Origin; 6] = [
        Origin::User,
        Origin::Assistant,
        Origin::Thinking,
        Origin::ToolUse,
        Origin::ToolResult,
        Origin::Meta,
    ];
    /// The default query scope: everything except [`Origin::ToolResult`].
    pub const DEFAULT: [Origin; 5] = [
        Origin::User,
        Origin::Assistant,
        Origin::Thinking,
        Origin::ToolUse,
        Origin::Meta,
    ];

    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// A search request. Build with [`Query::fuzzy`] or [`Query::substring`],
/// then adjust fields as needed; the same value drives both [`search`] and
/// [`Index::query`]. Deserializes from as little as `{"pattern": "…"}` —
/// every other field has the constructor's default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Query {
    pub pattern: String,
    pub mode: Mode,
    pub case: Case,
    /// Which content kinds to search. Defaults to [`Origin::DEFAULT`]
    /// (everything but tool output).
    pub origins: Vec<Origin>,
    /// Restrict to sessions from these harnesses; `None` searches all.
    /// Only meaningful for [`Index::query`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harnesses: Option<Vec<HarnessId>>,
    /// For [`Index::query`], the maximum number of documents returned; for
    /// [`search`], the maximum number of hits. `None` is unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Maximum [`Hit`]s materialized per document by [`Index::query`] — the
    /// document's best-scoring lines, in transcript order. `None`
    /// materializes every match. Defaults to `8`.
    #[serde(
        default = "default_hits_per_doc",
        skip_serializing_if = "Option::is_none"
    )]
    pub hits_per_doc: Option<usize>,
}

#[allow(clippy::unnecessary_wraps)] // serde default for an Option field
fn default_hits_per_doc() -> Option<usize> {
    Some(8)
}

impl Default for Query {
    /// An empty fuzzy query — matches every document with no hits.
    fn default() -> Query {
        Query::fuzzy("")
    }
}

impl Query {
    /// A fuzzy query with smart case over the default origins.
    #[must_use]
    pub fn fuzzy(pattern: impl Into<String>) -> Query {
        Query::new(pattern, Mode::Fuzzy)
    }

    /// A literal substring query with smart case over the default origins.
    #[must_use]
    pub fn substring(pattern: impl Into<String>) -> Query {
        Query::new(pattern, Mode::Substring)
    }

    fn new(pattern: impl Into<String>, mode: Mode) -> Query {
        Query {
            pattern: pattern.into(),
            mode,
            case: Case::Smart,
            origins: Origin::DEFAULT.to_vec(),
            harnesses: None,
            limit: None,
            hits_per_doc: default_hits_per_doc(),
        }
    }

    fn compile(&self) -> Compiled {
        match self.mode {
            Mode::Fuzzy => {
                let case = match self.case {
                    Case::Smart => CaseMatching::Smart,
                    Case::Insensitive => CaseMatching::Ignore,
                    Case::Sensitive => CaseMatching::Respect,
                };
                Compiled::Fuzzy {
                    pattern: Pattern::parse(&self.pattern, case, Normalization::Smart),
                    // Whole-pattern literal needle for the exact-occurrence
                    // tier. Atom syntax (`'`, `^`, `!`) will not match
                    // literally, so the bonus is not applied.
                    exact: Needle::new(self.pattern.trim(), self.case),
                }
            }
            Mode::Substring => Compiled::Substring(Needle::new(&self.pattern, self.case)),
        }
    }

    fn origin_mask(&self) -> u8 {
        self.origins.iter().fold(0, |m, o| m | o.bit())
    }
}

// ── results ────────────────────────────────────────────────────────────

/// One matching line. `highlights` are character-index ranges into `line`,
/// ready for highlighting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    /// The message the line came from, as a one-message [`Span`] ready for
    /// [`Transcript::fragment`]. Empty for [`Origin::Meta`] lines, which come
    /// from the session header rather than a message.
    pub span: Span,
    /// Index into the message's content blocks (`0` for [`Origin::Meta`]).
    pub block: usize,
    pub origin: Origin,
    /// The matched line's text.
    pub line: String,
    /// Match positions within `line`, in characters, merged and sorted.
    pub highlights: Vec<Range<u32>>,
    pub score: u32,
}

/// Identity of an indexed document: which harness's session it is.
///
/// `source` disambiguates distinct copies that share a `(harness, id)` —
/// Claude Code, for one, writes the same session id into more than one
/// project directory when a session is resumed from a different cwd — so
/// both copies can be indexed side by side instead of one silently
/// replacing the other. Callers whose ids are already unique may leave it
/// `None`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocKey {
    pub harness: HarnessId,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl fmt::Display for DocKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.harness, self.id)
    }
}

/// One document's result from [`Index::query`]: the session, its best-hit
/// score, and its matching lines in transcript order.
#[derive(Debug)]
pub struct DocMatch<'i> {
    pub key: &'i DocKey,
    pub meta: &'i Meta,
    /// The best line score in the document.
    pub score: u32,
    /// Matching lines, in transcript order.
    pub hits: Vec<Hit>,
}

// ── one-shot ───────────────────────────────────────────────────────────

/// Search a single transcript. Hits come back in transcript order,
/// [`Query::limit`] capping their count.
#[must_use]
pub fn search(transcript: &Transcript<Common>, query: &Query) -> Vec<Hit> {
    let lines = extract(&transcript.meta, &transcript.body);
    let pattern = query.compile();
    if pattern_is_empty(&pattern, query) {
        // An empty pattern scores everything 0; match nothing instead.
        Vec::new()
    } else {
        let mask = query.origin_mask();
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut indices = Vec::new();
        lines
            .iter()
            // Lines from origins the query did not select are not searched.
            .filter(|line| line.origin.bit() & mask != 0)
            .filter_map(|line| line.hit(&pattern, &mut matcher, &mut indices))
            .take(query.limit.unwrap_or(usize::MAX))
            .collect()
    }
}

// ── index ──────────────────────────────────────────────────────────────

/// An in-memory search index over many transcripts.
///
/// [`Index::query`] takes `&self` and does no I/O; the index is
/// `Send + Sync`. It never touches a [`Store`](crate::Store) or the
/// filesystem — the caller decides what goes in and when it is refreshed.
/// Re-inserting an existing [`DocKey`] replaces that document.
#[derive(Default)]
pub struct Index {
    docs: Vec<Doc>,
    by_key: HashMap<DocKey, usize>,
}

#[derive(Serialize, Deserialize)]
struct Doc {
    key: DocKey,
    meta: Meta,
    lines: Vec<Line>,
    chars: usize,
}

/// One transcript's searchable content, extracted outside the index — the
/// caller-parallelizable half of [`Index::insert`]. Extraction (line
/// splitting, UTF-32 transcoding) is the expensive part of an insert; build
/// `Extracted`s on worker threads and fold each into the index with
/// [`Index::insert_extracted`].
///
/// Serializable, so callers that keep a persistent cache can store the
/// extracted form and skip re-parsing unchanged sessions: deserializing an
/// `Extracted` costs a UTF-32 transcode of its lines, not a session parse.
/// The serialized shape is internal — it has no stability guarantee across
/// crate versions, and a cache should key on the version that wrote it.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct Extracted(Doc);

impl Extracted {
    /// Extract `transcript`'s searchable lines for `key`. Pure CPU, no
    /// index involved — safe to run on any thread.
    #[must_use]
    pub fn new(key: DocKey, transcript: &Transcript<Common>) -> Extracted {
        let lines = extract(&transcript.meta, &transcript.body);
        Extracted(Doc {
            key,
            meta: transcript.meta.clone(),
            chars: lines.iter().map(|l| l.text.len()).sum(),
            lines,
        })
    }

    /// The document this extraction belongs to.
    #[must_use]
    pub fn key(&self) -> &DocKey {
        &self.0.key
    }

    /// The session metadata captured at extraction.
    #[must_use]
    pub fn meta(&self) -> &Meta {
        &self.0.meta
    }
}

impl Index {
    #[must_use]
    pub fn new() -> Index {
        Index::default()
    }

    /// Number of indexed documents.
    #[must_use]
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Total indexed lines across all documents.
    #[must_use]
    pub fn lines(&self) -> usize {
        self.docs.iter().map(|d| d.lines.len()).sum()
    }

    /// Total indexed characters across all documents.
    #[must_use]
    pub fn chars(&self) -> usize {
        self.docs.iter().map(|d| d.chars).sum()
    }

    /// Add a transcript under `key`, replacing any document already indexed
    /// under the same key.
    pub fn insert(&mut self, key: DocKey, transcript: &Transcript<Common>) {
        self.insert_extracted(Extracted::new(key, transcript));
    }

    /// Fold a pre-extracted document into the index, replacing any document
    /// already indexed under its key. Cheap — the expensive half already
    /// happened in [`Extracted::new`], possibly on another thread.
    pub fn insert_extracted(&mut self, extracted: Extracted) {
        let Extracted(doc) = extracted;
        if let Some(&i) = self.by_key.get(&doc.key) {
            self.docs[i] = doc;
        } else {
            self.by_key.insert(doc.key.clone(), self.docs.len());
            self.docs.push(doc);
        }
    }

    /// Remove the document indexed under `key`. Returns whether it existed.
    pub fn remove(&mut self, key: &DocKey) -> bool {
        match self.by_key.remove(key) {
            // Unknown key: nothing to remove.
            None => false,
            Some(i) => {
                self.docs.swap_remove(i);
                // The doc swapped into slot `i` (if any) gets its index remapped.
                if let Some(moved) = self.docs.get(i) {
                    self.by_key.insert(moved.key.clone(), i);
                }
                true
            }
        }
    }

    /// Run a query, returning matching documents ranked by best-hit score
    /// (ties broken newest-first), each with its hits in transcript order.
    ///
    /// An empty pattern matches every document with no hits, ranked
    /// newest-first.
    #[must_use]
    pub fn query(&self, query: &Query) -> Vec<DocMatch<'_>> {
        let pattern = query.compile();
        if pattern_is_empty(&pattern, query) {
            // Empty pattern: every selected document, newest first, no hits.
            self.all_docs(query)
        } else {
            let mask = query.origin_mask();

            // Pass 1: score every line, keep per-doc line indices. Highlight
            // indices are deferred to pass 2 so only surviving docs pay for
            // them.
            let mut scored = self.score_all(&pattern, mask, query);

            scored.sort_by(|a, b| {
                b.1.cmp(&a.1).then_with(|| {
                    self.docs[b.0]
                        .meta
                        .timestamp
                        .cmp(&self.docs[a.0].meta.timestamp)
                })
            });
            if let Some(limit) = query.limit {
                scored.truncate(limit);
            }

            // Pass 2: highlight spans for the surviving docs' best lines only.
            self.materialize(scored, &pattern, query)
        }
    }

    /// Turn pass-1 survivors into [`DocMatch`]es, computing highlight spans.
    /// Sharded like scoring — span extraction re-runs the matcher per line.
    /// Chunks of the ranked list concatenate in order, so ranking is kept.
    #[cfg(not(target_arch = "wasm32"))]
    fn materialize(
        &self,
        mut scored: Vec<Scored>,
        pattern: &Compiled,
        query: &Query,
    ) -> Vec<DocMatch<'_>> {
        let workers = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        if workers > 1 && scored.len() >= 64 {
            let chunk = scored.len().div_ceil(workers);
            std::thread::scope(|scope| {
                let handles: Vec<_> = scored
                    .chunks_mut(chunk)
                    .map(|part| scope.spawn(move || self.materialize_chunk(part, pattern, query)))
                    .collect();
                handles
                    .into_iter()
                    .flat_map(|h| h.join().unwrap_or_default())
                    .collect()
            })
        } else {
            // One core or a small result set: threads would only add latency.
            self.materialize_chunk(&mut scored, pattern, query)
        }
    }

    /// Turn pass-1 survivors into [`DocMatch`]es, computing highlight spans.
    /// wasm32 has no threads; it always materializes sequentially.
    #[cfg(target_arch = "wasm32")]
    fn materialize(
        &self,
        mut scored: Vec<Scored>,
        pattern: &Compiled,
        query: &Query,
    ) -> Vec<DocMatch<'_>> {
        self.materialize_chunk(&mut scored, pattern, query)
    }

    fn materialize_chunk(
        &self,
        scored: &mut [Scored],
        pattern: &Compiled,
        query: &Query,
    ) -> Vec<DocMatch<'_>> {
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut indices = Vec::new();
        scored
            .iter_mut()
            .map(|(d, best, hit_lines)| {
                let doc = &self.docs[*d];
                let hits = select_hits(std::mem::take(hit_lines), query.hits_per_doc)
                    .into_iter()
                    .filter_map(|(l, _)| doc.lines[l].hit(pattern, &mut matcher, &mut indices))
                    .collect();
                DocMatch {
                    key: &doc.key,
                    meta: &doc.meta,
                    score: *best,
                    hits,
                }
            })
            .collect()
    }

    /// Score every selected document, sharding across cores when the corpus
    /// is large enough to pay for the threads.
    #[cfg(not(target_arch = "wasm32"))]
    fn score_all(&self, pattern: &Compiled, mask: u8, query: &Query) -> Vec<Scored> {
        let workers = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        // Below ~64k lines a single core finishes in well under a
        // millisecond; spawning threads would only add latency.
        if workers > 1 && self.lines() >= 64 * 1024 {
            // Shard by character volume, not document count — session
            // sizes are heavily skewed, and one oversized shard would
            // set the whole query's latency.
            let shards = self.shards(workers);
            std::thread::scope(|scope| {
                let handles: Vec<_> = shards
                    .into_iter()
                    .map(|shard| {
                        let docs = &self.docs[shard.clone()];
                        scope.spawn(move || score_docs(docs, shard.start, pattern, mask, query))
                    })
                    .collect();
                handles
                    .into_iter()
                    .flat_map(|h| h.join().unwrap_or_default())
                    .collect()
            })
        } else {
            score_docs(&self.docs, 0, pattern, mask, query)
        }
    }

    /// Score every selected document. wasm32 has no threads; it always
    /// takes the sequential path.
    #[cfg(target_arch = "wasm32")]
    fn score_all(&self, pattern: &Compiled, mask: u8, query: &Query) -> Vec<Scored> {
        score_docs(&self.docs, 0, pattern, mask, query)
    }

    /// Split the document list into up to `workers` contiguous ranges of
    /// roughly equal character volume.
    #[cfg(not(target_arch = "wasm32"))]
    fn shards(&self, workers: usize) -> Vec<std::ops::Range<usize>> {
        let target = (self.chars() / workers).max(1);
        let mut shards = Vec::with_capacity(workers);
        let mut start = 0;
        let mut acc = 0;
        for (i, doc) in self.docs.iter().enumerate() {
            acc += doc.chars;
            if acc >= target && shards.len() + 1 < workers {
                shards.push(start..i + 1);
                start = i + 1;
                acc = 0;
            }
        }
        if start < self.docs.len() {
            shards.push(start..self.docs.len());
        }
        shards
    }

    /// Every selected document, newest first, with no hits.
    fn all_docs(&self, query: &Query) -> Vec<DocMatch<'_>> {
        let mut all: Vec<&Doc> = self.docs.iter().filter(|d| d.selected(query)).collect();
        all.sort_by_key(|doc| std::cmp::Reverse(doc.meta.timestamp));
        if let Some(limit) = query.limit {
            all.truncate(limit);
        }
        all.into_iter()
            .map(|doc| DocMatch {
                key: &doc.key,
                meta: &doc.meta,
                score: 0,
                hits: Vec::new(),
            })
            .collect()
    }
}

impl Doc {
    fn selected(&self, query: &Query) -> bool {
        query
            .harnesses
            .as_ref()
            .is_none_or(|hs| hs.contains(&self.key.harness))
    }
}

/// Pass-1 result for one document: its index, best line score, and each
/// matched line with its score.
type Scored = (usize, u32, Vec<(usize, u32)>);

fn score_docs(
    docs: &[Doc],
    base: usize,
    pattern: &Compiled,
    mask: u8,
    query: &Query,
) -> Vec<Scored> {
    let mut matcher = Matcher::new(Config::DEFAULT);
    docs.iter()
        .enumerate()
        // Documents outside the query's harness filter are not scored.
        .filter(|(_, doc)| doc.selected(query))
        .filter_map(|(d, doc)| {
            let hit_lines: Vec<(usize, u32)> = doc
                .lines
                .iter()
                .enumerate()
                // Lines from origins the query did not select are not scored.
                .filter(|(_, line)| line.origin.bit() & mask != 0)
                .filter_map(|(l, line)| {
                    pattern
                        .score(line.text.slice(..), &mut matcher)
                        .map(|score| (l, score))
                })
                .collect();
            let best = hit_lines.iter().map(|&(_, score)| score).max();
            // A document with no matching line does not survive pass 1.
            best.map(|best| (base + d, best, hit_lines))
        })
        .collect()
}

/// Reduce a doc's matched lines to the ones worth materializing: the
/// `cap` best-scoring, put back into transcript order.
fn select_hits(mut hit_lines: Vec<(usize, u32)>, cap: Option<usize>) -> Vec<(usize, u32)> {
    if let Some(cap) = cap
        && hit_lines.len() > cap
    {
        hit_lines.sort_unstable_by_key(|&(_, score)| std::cmp::Reverse(score));
        hit_lines.truncate(cap);
        hit_lines.sort_unstable_by_key(|&(line, _)| line);
    }
    hit_lines
}

/// An empty/whitespace pattern parses to zero atoms, which scores everything
/// 0. Detect it from the input.
fn pattern_is_empty(_: &Compiled, query: &Query) -> bool {
    query.pattern.trim().is_empty()
}

// ── matchers ───────────────────────────────────────────────────────────

/// A compiled [`Query`] pattern.
///
/// Fuzzy scoring adds an exact-occurrence bonus so literal matches outrank
/// gapped matches.
///
/// Substring is our own scan: nucleo 0.3.1's case-insensitive substring
/// matcher misses matches near the end of a line when the needle's first
/// lowercase letter sits at position ≥ 2 (flag-shaped needles like
/// `--nocapture`), and its `Pattern::new` splits on whitespace, which is not
/// "one literal needle". Occurrences are still scored through nucleo's
/// `exact_match`, so fuzzy and substring scores stay on one scale.
enum Compiled {
    Fuzzy { pattern: Pattern, exact: Needle },
    Substring(Needle),
}

/// Added to a fuzzy score when the whole pattern occurs literally. High enough
/// to keep exact and gapped tiers separate while preserving fuzzy filtering.
const EXACT_BONUS: u32 = 1 << 16;

impl Compiled {
    fn score(&self, hay: Utf32Str<'_>, matcher: &mut Matcher) -> Option<u32> {
        match self {
            Compiled::Fuzzy { pattern, exact } => pattern
                .score(hay, matcher)
                .map(|score| score + exact.find(hay).map_or(0, |_| EXACT_BONUS)),
            Compiled::Substring(needle) => needle
                .find(hay)
                .map(|start| needle.score_at(hay, start, matcher)),
        }
    }

    fn indices(&self, hay: Utf32Str<'_>, matcher: &mut Matcher, out: &mut Vec<u32>) -> Option<u32> {
        match self {
            Compiled::Fuzzy { pattern, exact } => match exact.find(hay) {
                // The literal occurrence is what ranked the line; highlight
                // it, not whichever gapped alignment the matcher preferred.
                Some(start) => pattern.score(hay, matcher).map(|score| {
                    out.extend(start..start + exact.len_u32());
                    score + EXACT_BONUS
                }),
                None => pattern.indices(hay, matcher, out),
            },
            Compiled::Substring(needle) => needle.find(hay).map(|start| {
                out.extend(start..start + needle.len_u32());
                needle.score_at(hay, start, matcher)
            }),
        }
    }
}

/// One literal needle, pre-folded per the query's case rule. ASCII needles
/// carry a byte copy so ASCII haystacks (the overwhelming majority) run on
/// memchr/memmem instead of a per-char loop.
struct Needle {
    chars: Vec<char>,
    bytes: Option<Vec<u8>>,
    utf32: Utf32String,
    sensitive: bool,
}

impl Needle {
    fn new(pattern: &str, case: Case) -> Needle {
        let sensitive = match case {
            Case::Sensitive => true,
            Case::Insensitive => false,
            Case::Smart => pattern.chars().any(char::is_uppercase),
        };
        let chars: Vec<char> = pattern
            .chars()
            .map(|c| if sensitive { c } else { fold(c) })
            .collect();
        let bytes = chars
            .iter()
            .all(char::is_ascii)
            .then(|| chars.iter().map(|&c| c as u8).collect());
        Needle {
            chars,
            bytes,
            utf32: Utf32String::from(pattern),
            sensitive,
        }
    }

    fn len_u32(&self) -> u32 {
        u32::try_from(self.chars.len()).unwrap_or(u32::MAX)
    }

    /// Character index of the first occurrence in `hay`, under this needle's
    /// case rule.
    #[allow(clippy::cast_possible_truncation)] // haystacks are single lines
    fn find(&self, hay: Utf32Str<'_>) -> Option<u32> {
        match self.chars.len() {
            // An empty needle matches nothing.
            0 => None,
            // A needle longer than the haystack cannot fit.
            n if n > hay.len() => None,
            n => match hay {
                // A needle with non-ASCII characters (`bytes` is `None`)
                // cannot occur in an ASCII haystack.
                Utf32Str::Ascii(bytes) => self.bytes.as_deref().and_then(|needle| {
                    if self.sensitive {
                        memchr::memmem::find(bytes, needle).map(|i| i as u32)
                    } else {
                        // Needle is folded lowercase; walk candidate positions
                        // of its first byte (either case) and verify the
                        // window.
                        let (first, upper) = (needle[0], needle[0].to_ascii_uppercase());
                        memchr::memchr2_iter(first, upper, &bytes[..=bytes.len() - n])
                            .find(|&at| bytes[at..at + n].eq_ignore_ascii_case(needle))
                            .map(|at| at as u32)
                    }
                }),
                Utf32Str::Unicode(chars) => chars
                    .windows(n)
                    .position(|w| {
                        w.iter().zip(&self.chars).all(|(&c, &want)| {
                            if self.sensitive {
                                c == want
                            } else {
                                fold(c) == want
                            }
                        })
                    })
                    .map(|i| i as u32),
            },
        }
    }

    /// Score the verified occurrence at `start` with nucleo's exact matcher,
    /// keeping substring scores on nucleo's scale.
    fn score_at(&self, hay: Utf32Str<'_>, start: u32, matcher: &mut Matcher) -> u32 {
        let m = matcher.exact_match(
            hay.slice_u32(start..start + self.len_u32()),
            self.utf32.slice(..),
        );
        // The occurrence is already verified; a scoring miss (case rules
        // stricter than ours) still counts as a plain match.
        m.map_or(u32::from(SCORE_FALLBACK), u32::from)
    }
}

const SCORE_FALLBACK: u16 = 16;

/// Single-char case fold, matching nucleo's one-to-one folding model.
fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        // Multi-char lowercase expansions stay unfolded; `to_lowercase`
        // never yields an empty expansion.
        (Some(_), Some(_)) | (None, _) => c,
    }
}

// ── extraction ─────────────────────────────────────────────────────────

/// One searchable line, pre-converted to UTF-32 so queries never transcode.
#[derive(Serialize, Deserialize)]
struct Line {
    message: u32,
    block: u32,
    origin: Origin,
    #[serde(with = "utf32_text")]
    text: Utf32String,
}

/// `Utf32String` as plain text on the wire: the UTF-32 form is a matcher
/// detail, and the transcode back is what deserializing pays instead of a
/// session parse.
mod utf32_text {
    use nucleo_matcher::Utf32String;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(text: &Utf32String, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&text.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Utf32String, D::Error> {
        String::deserialize(d).map(Utf32String::from)
    }
}

impl Line {
    fn hit(
        &self,
        pattern: &Compiled,
        matcher: &mut Matcher,
        indices: &mut Vec<u32>,
    ) -> Option<Hit> {
        indices.clear();
        let score = pattern.indices(self.text.slice(..), matcher, indices)?;
        indices.sort_unstable();
        indices.dedup();
        Some(Hit {
            span: match self.origin {
                // Meta lines come from the session header, not a message.
                Origin::Meta => Span(0..0),
                _ => Span(self.message as usize..self.message as usize + 1),
            },
            block: self.block as usize,
            origin: self.origin,
            line: self.text.to_string(),
            highlights: merge_spans(indices),
            score,
        })
    }
}

/// Collapse sorted, deduplicated character indices into contiguous ranges.
fn merge_spans(indices: &[u32]) -> Vec<Range<u32>> {
    let mut spans: Vec<Range<u32>> = Vec::new();
    for &i in indices {
        match spans.last_mut() {
            // Adjacent to the open span: extend it.
            Some(last) if last.end == i => last.end = i + 1,
            // First index, or a gap after the open span: start a new span.
            Some(_) | None => spans.push(i..i + 1),
        }
    }
    spans
}

fn extract(meta: &Meta, messages: &[Message]) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut push = |message: u32, block: u32, origin: Origin, text: &str| {
        for line in text.lines() {
            let line = line.trim_end();
            if !line.trim_start().is_empty() {
                lines.push(Line {
                    message,
                    block,
                    origin,
                    text: Utf32String::from(line),
                });
            }
        }
    };

    for text in [&meta.title, &meta.cwd, &meta.git_branch]
        .into_iter()
        .flatten()
    {
        push(0, 0, Origin::Meta, text);
    }

    for (m, message) in messages.iter().enumerate() {
        let m = u32::try_from(m).unwrap_or(u32::MAX);
        for (b, block) in message.content.iter().enumerate() {
            let b = u32::try_from(b).unwrap_or(u32::MAX);
            match block {
                Block::Text { text } => {
                    let origin = match message.role {
                        Role::User => Origin::User,
                        Role::Assistant => Origin::Assistant,
                    };
                    push(m, b, origin, text);
                }
                Block::Thinking { text, .. } => push(m, b, Origin::Thinking, text),
                Block::ToolUse { tool, .. } => {
                    extract_tool(tool, |text| push(m, b, Origin::ToolUse, text));
                }
                Block::ToolResult { content, .. } => match content {
                    ToolOutput::Text(text) => push(m, b, Origin::ToolResult, text),
                    ToolOutput::Json(value) => {
                        push(m, b, Origin::ToolResult, &value.to_string());
                    }
                },
                // Images carry no searchable text.
                Block::Image { .. } => {}
            }
        }
    }
    lines
}

/// Searchable strings from a tool invocation.
fn extract_tool(tool: &Tool, mut push: impl FnMut(&str)) {
    match tool {
        Tool::Command { command, args } => {
            push(command);
            if let Some(args) = args {
                push(args);
            }
        }
        Tool::Read { file_path, .. } => push(file_path),
        Tool::Write { file_path, content } => {
            push(file_path);
            push(content);
        }
        Tool::Edit {
            file_path,
            old_string,
            new_string,
            ..
        } => {
            push(file_path);
            push(old_string);
            push(new_string);
        }
        Tool::MultiEdit { file_path, edits } => {
            push(file_path);
            for edit in edits {
                push(&edit.old_string);
                push(&edit.new_string);
            }
        }
        Tool::Bash {
            command,
            description,
            ..
        } => {
            push(command);
            if let Some(description) = description {
                push(description);
            }
        }
        Tool::Raw { tool_name, input } => {
            push(tool_name);
            if !input.is_null() {
                push(&input.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Index` must stay shareable across threads for caller-side sharding.
    #[test]
    fn index_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Index>();
    }

    #[test]
    fn merge_spans_groups_consecutive_indices() {
        assert_eq!(merge_spans(&[0, 1, 2, 5, 6, 9]), vec![0..3, 5..7, 9..10]);
        assert_eq!(merge_spans(&[]), Vec::<Range<u32>>::new());
    }
}
