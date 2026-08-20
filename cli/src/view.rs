//! `txcript view` — print a session as compact text.
//!
//! The source is a session id or exact title, looked up like `continue`,
//! with an optional `#range` fragment (see `fragment.rs`). Output goes to
//! stdout, colorless and pager-free, so it pipes cleanly into `pbcopy` or an
//! LLM prompt. Message numbers are printed in the output (`── #N ──` rules),
//! so what you see is what you reference.

use std::process::ExitCode;

use txcript::{HarnessId, Span, text};

use crate::fragment;

pub fn cmd_view(source: &str, from: Option<HarnessId>) -> Result<ExitCode, String> {
    let sessions = super::discover_with_spinner();
    // A whole-input match (a title that itself contains `#12`) beats the
    // fragment interpretation.
    let (src, request) = match fragment::parse_ref(source) {
        (_, Some(_)) if super::find_exact(&sessions, from, source).is_some() => (source, None),
        parsed => parsed,
    };

    let session = super::find_session(&sessions, from, src)?.ok_or_else(|| {
        let scope = from.map_or(String::new(), |h| format!(" {h}"));
        format!(
            "no local{scope} session matches `{src}` (try `{} list`)",
            crate::program()
        )
    })?;
    let common = session
        .read()
        .map_err(|e| format!("reading session `{src}`: {e}"))?;

    let total = common.body.len();
    let span = match &request {
        Some(req) => req.resolve(total)?,
        None => Span(0..total),
    };
    // `resolve` bounds-checked against `total`, so the render always lands.
    let rendered = text::to_text_fragment(&common, &span)
        .ok_or_else(|| format!("range is out of bounds — the session has {total} messages"))?;
    // A failed write means the reader is gone (`txcript view … | head`):
    // finish quietly instead of panicking the way `print!` would.
    let _ = std::io::Write::write_all(&mut std::io::stdout(), rendered.as_bytes());
    Ok(ExitCode::SUCCESS)
}
