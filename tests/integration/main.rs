//! Integration tests: every module exercises public API against real backing
//! stores (temp dirs, real `SQLite`) — no mocks. Per-harness modules cover
//! store round-trip fidelity, codec fixpoints through Common, and discovery;
//! the rest cover cross-harness conversion, adversarial path handling,
//! search, and deletion. Property-based codec invariants live in
//! `properties`. Tests that pin a specific shipped bug belong in
//! `tests/regression/`, not here — see `tests/README.md`.

mod amp;
mod antigravity;
mod chatgpt;
mod claude_chat;
mod claude_code;
mod codex;
mod cowork;
mod cross_harness;
mod cursor;
mod cursor_desktop;
mod fx;
mod grok;
mod hermes;
mod opencode;
mod path_safety;
mod pi;
mod properties;
mod simple;
mod store_delete;

#[cfg(feature = "search")]
mod search;
