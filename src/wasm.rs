//! WebAssembly bindings (feature `wasm`).
//!
//! The boundary is text-in / text-out: the JS host (Bun, a browser, Node) owns
//! all I/O — reading session files, writing the result — and calls these pure
//! functions for the transformation. Only the codec crosses into WASM; the
//! `Store` layer (filesystem, `SQLite`, subprocess) stays native.
//!
//! Build: `wasm-pack build --target nodejs --no-default-features --features wasm`
//! (Bun imports the generated ES module directly), or
//! `cargo build --lib --target wasm32-unknown-unknown --no-default-features --features wasm`.

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use crate::common;
use crate::harness::{
    amp, antigravity, campfire, claude_code, codex, cursor, cursor_desktop, grok, hermes, opencode,
    pi, simple,
};
use crate::transcript::{Codec, Common, HarnessId, TextCodec, Transcript};

/// Continue/convert a session from one harness's native text into another's.
///
/// `input` is the source session text (JSONL for `claude_code`/codex/pi/campfire,
/// the Cursor JSON DB export for cursor, the JSON dump of the session's
/// database rows for `cursor_desktop`, the `opencode export` JSON for
/// opencode, the JSON bundle of the session directory for grok, the
/// `hermes sessions export` JSON object for hermes, the thread JSON document
/// for amp, the JSON dump of the conversation database for antigravity, the
/// interchange JSON document for simple); `from`/`to` are harness names
/// (`"claude_code"`, `"codex"`, `"opencode"`, `"pi"`, `"campfire"`,
/// `"cursor"`, `"cursor_desktop"`, `"grok"`, `"hermes"`, `"amp"`,
/// `"antigravity"`, `"simple"`). Returns the target harness's native text.
#[wasm_bindgen]
pub fn convert(input: &str, from: &str, to: &str) -> Result<String, JsError> {
    parse_harness(from)
        .and_then(|from| parse_harness(to).map(|to| (from, to)))
        .and_then(|(from, to)| {
            parse_to_common(from, input)
                .and_then(|common| render_from_common(to, &common))
                .map_err(js)
        })
}

/// Parse a session into the canonical model as JSON (`{ meta, messages }`).
#[wasm_bindgen(js_name = toCommon)]
pub fn to_common(input: &str, from: &str) -> Result<String, JsError> {
    parse_harness(from)
        .and_then(|from| parse_to_common(from, input).map_err(js))
        .and_then(|common| {
            serde_json::to_string(&CommonJson {
                meta: common.meta,
                messages: common.body,
            })
            .map_err(js)
        })
}

/// Render a canonical model (`{ meta, messages }` JSON) into a harness's native
/// text — the inverse of [`to_common`].
#[wasm_bindgen(js_name = fromCommon)]
pub fn from_common(common_json: &str, to: &str) -> Result<String, JsError> {
    let to = parse_harness(to)?;
    serde_json::from_str(common_json)
        .map_err(js)
        .and_then(|parsed: CommonJson| {
            render_from_common(to, &Transcript::new(parsed.meta, parsed.messages)).map_err(js)
        })
}

/// The harness names this build understands.
#[wasm_bindgen]
pub fn harnesses() -> Vec<String> {
    HarnessId::ALL
        .iter()
        .map(|h| h.as_str().to_string())
        .collect()
}

// ── search (features `wasm` + `search`) ────────────────────────────────

/// One-shot search of a single session. `input`/`from` as in [`convert`];
/// `query_json` deserializes into [`crate::search::Query`] (only `pattern`
/// is required). Returns a JSON array of hits.
#[cfg(feature = "search")]
#[wasm_bindgen(js_name = searchTranscript)]
pub fn search_transcript(input: &str, from: &str, query_json: &str) -> Result<String, JsError> {
    let from = parse_harness(from)?;
    let common = parse_to_common(from, input).map_err(js)?;
    let query: crate::search::Query = serde_json::from_str(query_json).map_err(js)?;
    serde_json::to_string(&crate::search::search(&common, &query)).map_err(js)
}

/// In-memory index: insert sessions once, query per keystroke.
///
/// ```js
/// const s = new Searcher();
/// s.insert("claude_code", id, sessionText);
/// const matches = JSON.parse(s.query(JSON.stringify({ pattern: "relay bug" })));
/// ```
#[cfg(feature = "search")]
#[wasm_bindgen]
pub struct Searcher {
    index: crate::search::Index,
}

#[cfg(feature = "search")]
#[wasm_bindgen]
impl Searcher {
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Searcher {
        Searcher {
            index: crate::search::Index::new(),
        }
    }

    /// Parse a session's native text and index it under `harness` + `id`,
    /// replacing any document already indexed under the same key.
    pub fn insert(&mut self, harness: &str, id: &str, input: &str) -> Result<(), JsError> {
        let harness = parse_harness(harness)?;
        let common = parse_to_common(harness, input).map_err(js)?;
        let key = crate::search::DocKey {
            harness,
            id: id.to_string(),
            source: None,
        };
        self.index.insert(key, &common);
        Ok(())
    }

    /// Drop the session indexed under `harness` + `id`; returns whether it
    /// existed.
    pub fn remove(&mut self, harness: &str, id: &str) -> Result<bool, JsError> {
        let key = crate::search::DocKey {
            harness: parse_harness(harness)?,
            id: id.to_string(),
            source: None,
        };
        Ok(self.index.remove(&key))
    }

    /// Number of indexed sessions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Run a query (JSON, only `pattern` required). Returns a JSON array of
    /// `{ key, meta, score, hits }`, ranked.
    pub fn query(&self, query_json: &str) -> Result<String, JsError> {
        #[derive(Serialize)]
        struct MatchJson<'a> {
            key: &'a crate::search::DocKey,
            meta: &'a common::Meta,
            score: u32,
            hits: Vec<crate::search::Hit>,
        }
        let query: crate::search::Query = serde_json::from_str(query_json).map_err(js)?;
        let matches: Vec<MatchJson> = self
            .index
            .query(&query)
            .into_iter()
            .map(|m| MatchJson {
                key: m.key,
                meta: m.meta,
                score: m.score,
                hits: m.hits,
            })
            .collect();
        serde_json::to_string(&matches).map_err(js)
    }
}

#[cfg(feature = "search")]
impl Default for Searcher {
    fn default() -> Searcher {
        Searcher::new()
    }
}

// ── dispatch ───────────────────────────────────────────────────────────

/// Canonical model on the wire: a flat `{ meta, messages }` object.
#[derive(Serialize, Deserialize)]
struct CommonJson {
    meta: common::Meta,
    messages: Vec<common::Message>,
}

fn parse_to_common(harness: HarnessId, text: &str) -> crate::Result<Transcript<Common>> {
    fn go<H: TextCodec + Codec>(text: &str) -> crate::Result<Transcript<Common>> {
        H::to_common(&H::from_text(text)?)
    }
    match harness {
        HarnessId::ClaudeCode => go::<claude_code::ClaudeCode>(text),
        HarnessId::Codex => go::<codex::Codex>(text),
        HarnessId::OpenCode => go::<opencode::OpenCode>(text),
        HarnessId::Pi => go::<pi::Pi>(text),
        HarnessId::Campfire => go::<campfire::Campfire>(text),
        HarnessId::Cursor => go::<cursor::Cursor>(text),
        HarnessId::CursorDesktop => go::<cursor_desktop::CursorDesktop>(text),
        HarnessId::Grok => go::<grok::Grok>(text),
        HarnessId::Hermes => go::<hermes::Hermes>(text),
        HarnessId::Amp => go::<amp::Amp>(text),
        HarnessId::Antigravity => go::<antigravity::Antigravity>(text),
        HarnessId::Simple => go::<simple::Simple>(text),
    }
}

fn render_from_common(harness: HarnessId, common: &Transcript<Common>) -> crate::Result<String> {
    fn go<H: TextCodec + Codec>(common: &Transcript<Common>) -> crate::Result<String> {
        H::to_text(&H::from_common(common)?)
    }
    match harness {
        HarnessId::ClaudeCode => go::<claude_code::ClaudeCode>(common),
        HarnessId::Codex => go::<codex::Codex>(common),
        HarnessId::OpenCode => go::<opencode::OpenCode>(common),
        HarnessId::Pi => go::<pi::Pi>(common),
        HarnessId::Campfire => go::<campfire::Campfire>(common),
        HarnessId::Cursor => go::<cursor::Cursor>(common),
        HarnessId::CursorDesktop => go::<cursor_desktop::CursorDesktop>(common),
        HarnessId::Grok => go::<grok::Grok>(common),
        HarnessId::Hermes => go::<hermes::Hermes>(common),
        HarnessId::Amp => go::<amp::Amp>(common),
        HarnessId::Antigravity => go::<antigravity::Antigravity>(common),
        HarnessId::Simple => go::<simple::Simple>(common),
    }
}

fn parse_harness(name: &str) -> Result<HarnessId, JsError> {
    name.parse().map_err(js)
}

fn js<E: std::fmt::Display>(e: E) -> JsError {
    JsError::new(&e.to_string())
}
