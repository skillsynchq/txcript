//! Fragment refs — `<source>#<range>` — pointing at part of a session.
//!
//! The `#` suffix names a **1-based, inclusive** message range in the
//! session's canonical (`Transcript<Common>`) body: `#7` is message 7,
//! `#1-10` the first ten, `#5-` from 5 to the end, `#-10` start through 10.
//! It composes with the id-or-title sources `continue` and `view` accept.
//!
//! Internally ranges become [`Span`] (0-based, half-open), the same indices
//! `view`'s printed `── #N ──` ordinals are minted against, so a ref seen in
//! one command's output resolves to the same messages everywhere.

use txcript::{Common, Span, Transcript};

/// A parsed `#range` suffix: 1-based inclusive bounds, either end open.
/// Resolution against a concrete session length happens in [`SpanReq::resolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanReq {
    start: Option<usize>,
    end: Option<usize>,
}

/// Split a session ref into its source and optional fragment range. The
/// suffix after the *last* `#` is treated as a range only when it matches
/// the range grammar — so titles carrying a real `#anchor`, or any
/// non-numeric suffix, fall through as plain sources.
///
/// Ambiguity guard: callers that can cheaply check "does the *whole* input
/// name a session?" (a title literally containing `#12`) should do so first
/// and skip the fragment interpretation on a hit.
#[must_use]
pub fn parse_ref(input: &str) -> (&str, Option<SpanReq>) {
    match input.rfind('#') {
        // No `#` at all: the input is a plain source.
        None => (input, None),
        Some(pos) => {
            let (source, suffix) = (&input[..pos], &input[pos + 1..]);
            match (source.is_empty(), parse_range(suffix)) {
                // A leading `#` (`#7`) leaves no source to attach the range
                // to; a suffix that isn't a range (`#anchor`) leaves the `#`
                // to the source itself.
                (true, _) | (false, None) => (input, None),
                (false, Some(req)) => (source, Some(req)),
            }
        }
    }
}

/// `N`, `N-M`, `N-`, or `-M` — digits only, at least one bound.
pub(crate) fn parse_range(s: &str) -> Option<SpanReq> {
    #[allow(clippy::option_option)] // Some(None) is an open bound; None, not a range
    fn bound(t: &str) -> Option<Option<usize>> {
        match t {
            "" => Some(None),
            digits if digits.bytes().all(|b| b.is_ascii_digit()) => digits.parse().ok().map(Some),
            // Anything non-numeric is not a bound.
            _ => None,
        }
    }

    match s.split_once('-') {
        // No dash: a single number names a one-message range (and an empty
        // suffix falls out as `None` through the empty bound).
        None => bound(s)?.map(|n| SpanReq {
            start: Some(n),
            end: Some(n),
        }),
        // A bare `-` carries no bound at all.
        Some(("", "")) => None,
        Some((a, b)) => Some(SpanReq {
            start: bound(a)?,
            end: bound(b)?,
        }),
    }
}

impl SpanReq {
    /// Convert to a concrete [`Span`] against a session of `len` messages.
    ///
    /// # Errors
    /// When the range is empty, inverted, or past `len`; the message carries
    /// the user-facing 1-based numbers.
    pub fn resolve(&self, len: usize) -> Result<Span, String> {
        let start = self.start.unwrap_or(1);
        let end = self.end.unwrap_or(len);
        match (start, end) {
            // Message numbers are 1-based; a 0 bound can't name anything.
            (0, _) | (_, 0) => Err(format!("message numbers are 1-based — `#{self}` has a 0")),
            (s, e) if s > e => Err(format!("range `#{self}` is inverted")),
            (_, e) if e > len => Err(format!(
                "range `#{self}` is out of bounds — the session has {len} message{}",
                if len == 1 { "" } else { "s" }
            )),
            (s, e) => Ok(Span(s - 1..e)),
        }
    }
}

impl std::fmt::Display for SpanReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.start, self.end) {
            (Some(a), Some(b)) if a == b => write!(f, "{a}"),
            (a, b) => {
                if let Some(a) = a {
                    write!(f, "{a}")?;
                }
                write!(f, "-")?;
                if let Some(b) = b {
                    write!(f, "{b}")?;
                }
                Ok(())
            }
        }
    }
}

/// Human-facing `#a-b` (`#a` for a single message) for a resolved [`Span`].
#[must_use]
pub fn format_span(span: &Span) -> String {
    match span.0.len() {
        1 => format!("#{}", span.0.start + 1),
        _ => format!("#{}-{}", span.0.start + 1, span.0.end),
    }
}

/// The kept runs of a crop, joined: `#1-2, #11-40`.
#[must_use]
pub fn format_spans(spans: &[Span]) -> String {
    spans.iter().map(format_span).collect::<Vec<_>>().join(", ")
}

/// The transcript restricted to `req`: same meta, only the spanned messages.
///
/// # Errors
/// When `req` doesn't resolve against the transcript, or the range cuts a
/// tool call away from its result — the sliced session would confuse the
/// harness it's continued in; the message suggests the nearest valid range.
pub fn sliced(common: &Transcript<Common>, req: &SpanReq) -> Result<Transcript<Common>, String> {
    let span = req.resolve(common.body.len())?;
    common.crop(&span).map_err(|error| match error {
        txcript::CropError::SplitToolPair { nearest, .. } => format!(
            "range {} cuts a tool call away from its result — nearest valid range is {}",
            format_span(&span),
            format_span(&nearest)
        ),
        txcript::CropError::InvalidRange { .. } | txcript::CropError::AmbiguousToolId { .. } => {
            error.to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use txcript::common::{Block, Message, Meta, Role, Tool, ToolOutput};

    use super::*;

    fn req(start: Option<usize>, end: Option<usize>) -> SpanReq {
        SpanReq { start, end }
    }

    #[test]
    fn parse_ref_splits_range_suffixes() {
        assert_eq!(parse_ref("abc#1-10"), ("abc", Some(req(Some(1), Some(10)))));
        assert_eq!(parse_ref("abc#7"), ("abc", Some(req(Some(7), Some(7)))));
        assert_eq!(parse_ref("abc#5-"), ("abc", Some(req(Some(5), None))));
        assert_eq!(parse_ref("abc#-10"), ("abc", Some(req(None, Some(10)))));
        assert_eq!(
            parse_ref("Fix the parser #2#3-9"),
            ("Fix the parser #2", Some(req(Some(3), Some(9))))
        );
    }

    #[test]
    fn parse_ref_leaves_non_ranges_alone() {
        assert_eq!(parse_ref("abc"), ("abc", None));
        assert_eq!(parse_ref("abc#"), ("abc#", None));
        assert_eq!(parse_ref("abc#-"), ("abc#-", None));
        assert_eq!(parse_ref("title#anchor"), ("title#anchor", None));
        assert_eq!(parse_ref("abc#1x"), ("abc#1x", None));
        assert_eq!(parse_ref("#7"), ("#7", None));
    }

    #[test]
    fn resolve_converts_one_based_inclusive_to_span() {
        assert_eq!(req(Some(1), Some(10)).resolve(20).unwrap(), Span(0..10));
        assert_eq!(req(Some(7), Some(7)).resolve(20).unwrap(), Span(6..7));
        assert_eq!(req(Some(5), None).resolve(20).unwrap(), Span(4..20));
        assert_eq!(req(None, Some(10)).resolve(20).unwrap(), Span(0..10));
        assert!(req(Some(0), Some(3)).resolve(20).is_err());
        assert!(req(Some(9), Some(3)).resolve(20).is_err());
        assert!(req(Some(1), Some(21)).resolve(20).is_err());
    }

    #[test]
    fn errors_carry_the_request_as_written() {
        let err = req(Some(5), None).resolve(3).unwrap_err();
        assert!(err.contains("`#5-`"), "got: {err}");
        let err = req(None, Some(9)).resolve(3).unwrap_err();
        assert!(err.contains("`#-9`"), "got: {err}");
        let err = req(Some(4), Some(4)).resolve(3).unwrap_err();
        assert!(err.contains("`#4`"), "got: {err}");
    }

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

    fn tool_use(id: &str) -> Block {
        Block::ToolUse {
            id: id.into(),
            tool: Tool::Raw {
                tool_name: "x".into(),
                input: serde_json::json!({}),
            },
        }
    }

    fn tool_result(id: &str) -> Block {
        Block::ToolResult {
            tool_use_id: id.into(),
            content: ToolOutput::Text("ok".into()),
            is_error: false,
        }
    }

    fn text(t: &str) -> Block {
        Block::Text { text: t.into() }
    }

    /// 0:user 1:assistant(tool a) 2:user(result a) 3:assistant 4:user
    fn body() -> Vec<Message> {
        vec![
            message(Role::User, vec![text("q")]),
            message(Role::Assistant, vec![tool_use("a")]),
            message(Role::User, vec![tool_result("a")]),
            message(Role::Assistant, vec![text("done")]),
            message(Role::User, vec![text("thanks")]),
        ]
    }

    fn crop(body: &[Message], span: &Span) -> Result<Transcript<Common>, String> {
        Transcript::new(
            Meta {
                id: "test".into(),
                timestamp: DateTime::<Utc>::UNIX_EPOCH,
                cwd: None,
                git_branch: None,
                title: None,
                cli_version: None,
                model: None,
            },
            body.to_vec(),
        )
        .crop(span)
        .map_err(|error| {
            let suggestion = error
                .nearest_valid_span()
                .map(format_span)
                .unwrap_or_default();
            format!("{error} {suggestion}")
        })
    }

    #[test]
    fn pairing_accepts_clean_and_rejects_cut_ranges() {
        let body = body();
        assert!(crop(&body, &Span(0..5)).is_ok());
        assert!(crop(&body, &Span(3..5)).is_ok());
        assert!(crop(&body, &Span(1..3)).is_ok());

        // Cuts the result off the call (messages 1..2) → suggest #1-3.
        let err = crop(&body, &Span(0..2)).unwrap_err();
        assert!(err.contains("#1-3"), "got: {err}");
        // Starts on an orphaned result → expand backwards.
        let err = crop(&body, &Span(2..5)).unwrap_err();
        assert!(err.contains("#2-5"), "got: {err}");
    }

    #[test]
    fn pairing_ignores_dangling_calls_in_the_source_session() {
        // Session ends on an unanswered tool_use — not the crop's fault.
        let mut body = body();
        body.push(message(Role::Assistant, vec![tool_use("b")]));
        assert!(crop(&body, &Span(3..6)).is_ok());
        assert!(crop(&body, &Span(0..6)).is_ok());
    }
}
