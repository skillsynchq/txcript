//! CLI over the `txcript` crate: list, search, and continue local AI coding
//! sessions across supported harnesses.
//!
//! ```text
//! txcript list                          # all local sessions, every harness
//!     [--from <harness>]                    #   only this harness's sessions
//!     [--cwd <dir>]                         #   only sessions recorded under <dir>
//!     [-n <N>]                              #   at most N sessions
//!     [--since <when>] [--until <when>]     #   bound the session start time
//! txcript continue <id>[#range]         # continue <id>, then launch the harness
//!     [--with <harness>]                    #   ...continuing in <harness> instead
//!     [--from <harness>]                    #   scope the id lookup to one harness
//!     [--out <dir>]                         #   write under <dir>; implies --no-resume
//!     [--no-resume]                         #   write the session but don't launch
//! txcript view <id>[#range]             # print a session as compact text
//!     [--from <harness>]                    #   scope the id lookup to one harness
//! txcript query '<pattern>'             # one-shot search, print ranked hits
//! txcript query                         # fzf-style picker; Enter continues
//!     [--from <harness>]                    #   search only <harness> (default: all)
//!     [--with <harness>]                    #   continue the pick in <harness>
//!     [--cwd <dir>]                         #   only sessions recorded in <dir>
//! txcript mcp                           # serve MCP over stdio
//! txcript completion <shell>            # print a completion script
//! ```
//!
//! By default `continue` launches the harness from the recorded working
//! directory when it still exists. Resume commands are overridable per harness
//! via `TRANSCRIPT_<HARNESS>_RESUME_CMD` (a `{id}` template).
//!
//! `#range` is a 1-based, inclusive message range (`#7`, `#5-12`, `#5-`,
//! `#-10`); `view` prints the matching ordinals, so what you see is what you
//! reference. See `fragment.rs`.
//!
//! Anywhere a session id is accepted, any unambiguous prefix of it works too;
//! an ambiguous prefix errors with the candidates. Exact ids and titles win
//! over prefix interpretation.
//!
//! Session discovery/conversion lives in [`txcript::local`]; ranking lives in
//! [`txcript::search`].

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use txcript::harness::amp;
use txcript::{Codec, Common, HarnessId, TextCodec, Transcript, local};

mod fragment;
mod mcp;
mod view;

const HARNESSES: &str = "harnesses: claude_code, codex, opencode, pi, campfire, cursor, cursor_desktop, grok, amp, \
     antigravity, simple";

#[derive(Parser)]
#[command(
    name = "txcript",
    version,
    about = "List, search, and continue local AI coding sessions in any harness",
    after_help = HARNESSES
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List local sessions across every harness, newest first
    List {
        /// List only this harness's sessions
        #[arg(long, value_name = "HARNESS", value_parser = HarnessParser)]
        from: Option<HarnessId>,
        /// Only sessions recorded in or under this working directory
        #[arg(long, value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
        cwd: Option<PathBuf>,
        /// Show at most this many sessions
        #[arg(long, short = 'n', value_name = "N")]
        limit: Option<usize>,
        /// Only sessions started at or after this time (RFC3339 or
        /// YYYY-MM-DD, a bare date meaning that local midnight)
        #[arg(long, value_name = "WHEN", value_parser = parse_since)]
        since: Option<chrono::DateTime<chrono::Utc>>,
        /// Only sessions started at or before this time (RFC3339 or
        /// YYYY-MM-DD, a bare date meaning the end of that local day)
        #[arg(long, value_name = "WHEN", value_parser = parse_until)]
        until: Option<chrono::DateTime<chrono::Utc>>,
    },
    /// Continue a session, then launch its harness
    ///
    /// Same-harness continues resume the original in place; --with
    /// re-synthesizes into another harness's native, resumable format first.
    /// A `#range` suffix continues just those messages (as a new session);
    /// ranges that cut a tool call away from its result are refused.
    ///
    /// Anything that writes a copy writes a *new* session, with its own id and
    /// today's timestamp — the source is never modified. The printed resume
    /// command carries the new id.
    Continue {
        /// Session id (any unambiguous prefix) or its exact title, with an
        /// optional `#range` of 1-based inclusive message numbers
        /// (`abc#5-12`, `#7`, `#5-`, `#-10`)
        // Other: without a hint, generated completions fall back to filenames.
        #[arg(value_hint = clap::ValueHint::Other)]
        id: String,
        /// Continue in this harness instead of the session's own
        #[arg(long, value_name = "HARNESS", value_parser = HarnessParser)]
        with: Option<HarnessId>,
        /// Only look for the session in this harness
        #[arg(long, value_name = "HARNESS", value_parser = HarnessParser)]
        from: Option<HarnessId>,
        /// Write under this directory instead of the harness's live root
        /// (implies --no-resume: the harness wouldn't see the copy). Exports
        /// keep the source's id and timestamp; copies into a live store get
        /// their own.
        #[arg(long, value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
        out: Option<PathBuf>,
        /// Write the session but don't launch the harness
        #[arg(long)]
        no_resume: bool,
    },
    /// Print a session as compact text
    ///
    /// Prints the same token-conscious projection the MCP server serves,
    /// numbered `── #N ──` per message, so a printed ordinal can be fed
    /// straight back as a `#range`. Output is colorless and pager-free —
    /// it pipes cleanly into pbcopy or an LLM prompt.
    View {
        /// Session id (any unambiguous prefix) or its exact title, with an
        /// optional `#range` of 1-based inclusive message numbers
        /// (`abc#5-12`, `#7`, `#5-`, `#-10`)
        // Other: without a hint, generated completions fall back to filenames.
        #[arg(value_hint = clap::ValueHint::Other)]
        source: String,
        /// Only look for the session in this harness
        #[arg(long, value_name = "HARNESS", value_parser = HarnessParser)]
        from: Option<HarnessId>,
    },
    /// Search session content; without a pattern, open an fzf-style picker
    ///
    /// A pattern prints ranked hits, labeled by what matched (user text,
    /// assistant text, thinking, tool use, session metadata). The picker
    /// filters per keystroke; Enter continues the selection, Esc cancels.
    Query {
        /// fzf-style pattern ('exact, ^prefix, suffix$, !not); omit to pick
        /// interactively
        // Other: without a hint, generated completions fall back to filenames.
        #[arg(value_hint = clap::ValueHint::Other)]
        pattern: Option<String>,
        /// Continue the picked session in this harness
        #[arg(long, value_name = "HARNESS", value_parser = HarnessParser)]
        with: Option<HarnessId>,
        /// Search only this harness
        #[arg(long, value_name = "HARNESS", value_parser = HarnessParser)]
        from: Option<HarnessId>,
        /// Only sessions recorded in or under this working directory
        #[arg(long, value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
        cwd: Option<PathBuf>,
    },
    /// Serve the Model Context Protocol over stdin/stdout
    Mcp,
    /// Print a completion script for a shell (add it to your shell config)
    Completion {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

fn harness(s: &str) -> Result<HarnessId, txcript::Error> {
    s.parse()
}

/// [`harness`] as a clap parser that also advertises the canonical names, so
/// help and shell completion offer them. Parsing stays [`harness`]'s (its
/// friendly aliases included): `possible_values` informs, it doesn't restrict.
#[derive(Clone)]
struct HarnessParser;

impl clap::builder::TypedValueParser for HarnessParser {
    type Value = HarnessId;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<HarnessId, clap::Error> {
        (harness as fn(&str) -> Result<HarnessId, txcript::Error>).parse_ref(cmd, arg, value)
    }

    fn possible_values(
        &self,
    ) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        Some(Box::new(
            HarnessId::ALL
                .into_iter()
                .map(|h| clap::builder::PossibleValue::new(h.as_str())),
        ))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::List {
            from,
            cwd,
            limit,
            since,
            until,
        } => {
            cmd_list(from, cwd.as_deref(), limit, since, until);
            Ok(ExitCode::SUCCESS)
        }
        Command::Continue {
            id,
            with,
            from,
            out,
            no_resume,
        } => cmd_continue(&id, with, from, out.as_ref(), no_resume),
        Command::View { source, from } => view::cmd_view(&source, from),
        Command::Query {
            pattern,
            with,
            from,
            cwd,
        } => query::cmd_query(pattern, with, from, cwd.as_deref()),
        Command::Mcp => mcp::serve().await,
        Command::Completion { shell } => {
            // Render to a buffer first: a failed stdout write means the
            // reader is gone (`… | head`), which should end quietly, not
            // panic inside clap_complete.
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "txcript", &mut script);
            let _ = std::io::Write::write_all(&mut std::io::stdout(), &script);
            Ok(ExitCode::SUCCESS)
        }
    };
    result.unwrap_or_else(|e| {
        eprintln!("error: {e}");
        ExitCode::FAILURE
    })
}

/// True when a session's recorded `cwd` is `dir` or anywhere under it, so a
/// monorepo session started in `repo/packages/foo` shows up when listing
/// `repo`. The check is component-wise (`/foo/barbaz` is not under
/// `/foo/bar`). Both sides are canonicalized so different spellings of one
/// directory still match (`/tmp` vs `/private/tmp`, `$PWD` through a
/// symlink); a path that no longer exists keeps its raw spelling, so
/// vanished directories compare as plain components.
fn under_dir(session_cwd: &str, dir: &std::path::Path) -> bool {
    let canon = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canon(std::path::Path::new(session_cwd)).starts_with(canon(dir))
}

/// The `--from`/`--cwd` session filters shared by `list` and `query`.
/// A `--cwd` filter excludes sessions with no recorded cwd — they don't
/// pertain to any folder.
fn selected(
    session: &local::Session,
    from: Option<HarnessId>,
    cwd: Option<&std::path::Path>,
) -> bool {
    matches_filters(session.harness, session.meta.cwd.as_deref(), from, cwd)
}

fn matches_filters(
    session_harness: HarnessId,
    session_cwd: Option<&str>,
    from: Option<HarnessId>,
    cwd: Option<&std::path::Path>,
) -> bool {
    from.is_none_or(|harness| session_harness == harness)
        && cwd.is_none_or(|dir| session_cwd.is_some_and(|recorded| under_dir(recorded, dir)))
}

/// The first session matching `needle` exactly — by id or title — scoped to
/// `from` when given. Discovery order is newest-first, so copies sharing an
/// id resolve to the newest.
fn find_exact<'a>(
    sessions: &'a [local::Session],
    from: Option<HarnessId>,
    needle: &str,
) -> Option<&'a local::Session> {
    sessions.iter().find(|s| {
        from.is_none_or(|h| s.harness == h)
            && (s.meta.id == needle || s.meta.title.as_deref() == Some(needle))
    })
}

/// Resolve `needle` to a session: exact id or exact title first, then an
/// unambiguous id prefix. `Ok(None)` when nothing matches; `Err` with the
/// candidates when several distinct ids share the prefix.
fn find_session<'a>(
    sessions: &'a [local::Session],
    from: Option<HarnessId>,
    needle: &str,
) -> Result<Option<&'a local::Session>, String> {
    if let Some(found) = find_exact(sessions, from, needle) {
        return Ok(Some(found));
    }
    if needle.is_empty() {
        return Ok(None);
    }
    let scoped: Vec<&local::Session> = sessions
        .iter()
        .filter(|s| from.is_none_or(|h| s.harness == h))
        .collect();
    let hits = distinct_prefix_matches(scoped.iter().map(|s| s.meta.id.as_str()), needle);
    match hits.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(scoped[*one])),
        many => {
            let candidates: Vec<&local::Session> = many.iter().map(|&i| scoped[i]).collect();
            Err(ambiguous_message(needle, &candidates))
        }
    }
}

/// Positions of the first occurrence of each distinct id starting with
/// `prefix`. Claude Code writes a session resumed from another cwd under the
/// same id in a second store; those copies collapse to the first (newest —
/// discovery order) rather than reading as an ambiguity.
fn distinct_prefix_matches<'a>(ids: impl Iterator<Item = &'a str>, prefix: &str) -> Vec<usize> {
    let mut seen: Vec<&str> = Vec::new();
    let mut hits = Vec::new();
    for (i, id) in ids.enumerate() {
        if id.starts_with(prefix) && !seen.contains(&id) {
            seen.push(id);
            hits.push(i);
        }
    }
    hits
}

fn ambiguous_message(needle: &str, candidates: &[&local::Session]) -> String {
    use std::fmt::Write as _;
    let mut msg = format!(
        "`{needle}` prefixes {} session ids — add characters:",
        candidates.len()
    );
    for s in candidates.iter().take(10) {
        let title = s.meta.title.as_deref().unwrap_or("");
        let _ = write!(
            msg,
            "\n  {:<12}  {}  {}",
            s.harness,
            style::scrub(&s.meta.id),
            style::scrub(title)
        );
    }
    if candidates.len() > 10 {
        let _ = write!(msg, "\n  …and {} more", candidates.len() - 10);
    }
    msg
}

/// `--since`: a bare `YYYY-MM-DD` means that local midnight.
fn parse_since(s: &str) -> Result<chrono::DateTime<chrono::Utc>, String> {
    parse_when(s, false)
}

/// `--until`: a bare `YYYY-MM-DD` means the end of that local day.
fn parse_until(s: &str) -> Result<chrono::DateTime<chrono::Utc>, String> {
    parse_when(s, true)
}

fn parse_when(s: &str, end_of_day: bool) -> Result<chrono::DateTime<chrono::Utc>, String> {
    use chrono::TimeZone as _;
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&chrono::Utc));
    }
    let date = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| {
        format!("`{s}` is neither RFC3339 (2026-08-18T10:00:00Z) nor a date (2026-08-18)")
    })?;
    let time = if end_of_day {
        chrono::NaiveTime::from_hms_opt(23, 59, 59).unwrap_or(chrono::NaiveTime::MIN)
    } else {
        chrono::NaiveTime::MIN
    };
    // Bare dates read as the user's local calendar. A time made ambiguous or
    // skipped by a DST edge takes the earlier mapping; UTC is the fallback.
    Ok(
        match chrono::Local.from_local_datetime(&date.and_time(time)) {
            chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => {
                t.with_timezone(&chrono::Utc)
            }
            chrono::LocalResult::None => chrono::Utc.from_utc_datetime(&date.and_time(time)),
        },
    )
}

/// Compact age for the listing's WHEN column: relative inside a week, the
/// local date past it. Widths stay within 10 characters.
fn format_when(ts: chrono::DateTime<chrono::Utc>) -> String {
    format_when_at(ts, chrono::Utc::now())
}

fn format_when_at(ts: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>) -> String {
    let delta = now.signed_duration_since(ts);
    // Small clock skew (a session stamped just ahead of us) reads as now.
    match delta {
        d if d.num_seconds() < 60 => "just now".to_string(),
        d if d.num_minutes() < 60 => format!("{}m ago", d.num_minutes()),
        d if d.num_hours() < 24 => format!("{}h ago", d.num_hours()),
        d if d.num_days() < 7 => format!("{}d ago", d.num_days()),
        _ => ts
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string(),
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    #[test]
    fn omitted_filters_include_every_harness_and_missing_cwd() {
        assert!(matches_filters(HarnessId::Codex, None, None, None));
        assert!(matches_filters(
            HarnessId::ClaudeCode,
            Some("/some/project"),
            None,
            None
        ));
    }

    #[test]
    fn supplied_filters_require_matching_harness_and_recorded_cwd() {
        let cwd = std::path::Path::new("/some/project");
        assert!(matches_filters(
            HarnessId::Codex,
            Some("/some/project"),
            Some(HarnessId::Codex),
            Some(cwd)
        ));
        assert!(!matches_filters(
            HarnessId::ClaudeCode,
            Some("/some/project"),
            Some(HarnessId::Codex),
            Some(cwd)
        ));
        assert!(!matches_filters(
            HarnessId::Codex,
            None,
            Some(HarnessId::Codex),
            Some(cwd)
        ));
    }

    #[test]
    fn cwd_filter_admits_subdirectories_on_component_boundaries() {
        let repo = std::path::Path::new("/some/repo");
        assert!(matches_filters(
            HarnessId::Codex,
            Some("/some/repo/packages/foo"),
            None,
            Some(repo)
        ));
        // A sibling sharing the prefix as a string is not under the filter.
        assert!(!matches_filters(
            HarnessId::Codex,
            Some("/some/repo2"),
            None,
            Some(repo)
        ));
        // The subtree runs downward only: a parent isn't "under" its child.
        assert!(!matches_filters(
            HarnessId::Codex,
            Some("/some"),
            None,
            Some(repo)
        ));
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::distinct_prefix_matches;

    #[test]
    fn prefixes_match_first_copy_of_each_distinct_id() {
        let ids = ["abc123", "abcdef", "abc123", "zzz"];
        // Two distinct ids share `abc`; the duplicate collapses to its first
        // (newest) copy.
        assert_eq!(distinct_prefix_matches(ids.into_iter(), "abc"), [0, 1]);
        assert_eq!(distinct_prefix_matches(ids.into_iter(), "abc1"), [0]);
        assert_eq!(
            distinct_prefix_matches(ids.into_iter(), "nope"),
            [] as [usize; 0]
        );
    }
}

#[cfg(test)]
mod when_tests {
    use super::{format_when_at, parse_since, parse_until};

    #[test]
    fn ages_stay_within_the_ten_char_column() {
        let now: chrono::DateTime<chrono::Utc> = "2026-08-18T12:00:00Z".parse().unwrap();
        let at = |s: &str| format_when_at(s.parse().unwrap(), now);
        assert_eq!(at("2026-08-18T11:59:30Z"), "just now");
        assert_eq!(at("2026-08-18T11:15:00Z"), "45m ago");
        assert_eq!(at("2026-08-18T02:00:00Z"), "10h ago");
        assert_eq!(at("2026-08-15T12:00:00Z"), "3d ago");
        // Past a week: an absolute local date, exactly 10 chars.
        assert_eq!(at("2026-01-01T00:00:00Z").len(), 10);
        // A session stamped slightly ahead of our clock reads as now.
        assert_eq!(at("2026-08-18T12:00:20Z"), "just now");
    }

    #[test]
    fn bounds_accept_rfc3339_and_bare_dates() {
        let expected: chrono::DateTime<chrono::Utc> = "2026-08-18T10:00:00Z".parse().unwrap();
        assert_eq!(parse_since("2026-08-18T10:00:00Z").unwrap(), expected);
        // A bare date spans its whole local day: until lands after since.
        let since = parse_since("2026-08-18").unwrap();
        let until = parse_until("2026-08-18").unwrap();
        assert!(until > since);
        assert_eq!((until - since).num_seconds(), 24 * 3600 - 1);
        assert!(parse_since("yesterday").is_err());
    }
}

#[cfg(test)]
mod stamp_tests {
    use super::stamp_live_cwd;

    #[test]
    fn dead_cwds_rehome_to_the_current_directory_except_for_exports() {
        let mut copy = super::identity_tests::transcript();
        copy.meta.cwd = Some("/no/such/dir/txcript-test".into());
        stamp_live_cwd(&mut copy, None);
        let current = std::env::current_dir().unwrap();
        assert_eq!(copy.meta.cwd.as_deref(), current.to_str());

        // A cwd that still exists is kept.
        let mut copy = super::identity_tests::transcript();
        copy.meta.cwd = current.to_str().map(String::from);
        stamp_live_cwd(&mut copy, None);
        assert_eq!(copy.meta.cwd.as_deref(), current.to_str());

        // `--out` exports stay faithful to the source, dead cwd or not.
        let mut copy = super::identity_tests::transcript();
        copy.meta.cwd = Some("/no/such/dir/txcript-test".into());
        stamp_live_cwd(&mut copy, Some(std::path::Path::new("/tmp/x")));
        assert_eq!(copy.meta.cwd.as_deref(), Some("/no/such/dir/txcript-test"));
    }
}

#[cfg(test)]
mod scrub_tests {
    use super::style::scrub;

    #[test]
    fn control_characters_become_spaces_one_for_one() {
        // ANSI SGR, an OSC-52 clipboard write, a forged row via newline, and
        // a bell: all neutralized, and the char count is unchanged so column
        // math and highlight spans stay aligned.
        let hostile = "a\x1b[31mred\x1b]52;c;evil\x07b\nrow\ttab";
        let cleaned = scrub(hostile);
        assert!(!cleaned.chars().any(char::is_control));
        assert_eq!(cleaned.chars().count(), hostile.chars().count());
        assert_eq!(scrub("plain text stays"), "plain text stays");
    }
}

#[cfg(test)]
mod identity_tests {
    use super::{Common, HarnessId, Transcript, fresh_identity};
    use txcript::common::Meta;

    pub(crate) fn transcript() -> Transcript<Common> {
        Transcript::new(
            Meta {
                id: "bb3c5476-0d25-46d0-803a-0ed9de155e6b".into(),
                timestamp: "2026-07-30T20:33:48Z".parse().unwrap_or_default(),
                cwd: Some("/work/aristotle".into()),
                git_branch: None,
                title: None,
                cli_version: None,
                model: None,
            },
            Vec::new(),
        )
    }

    #[test]
    fn a_copy_into_a_live_store_becomes_its_own_session() {
        let mut copy = transcript();
        let (id, ts) = (copy.meta.id.clone(), copy.meta.timestamp);
        fresh_identity(&mut copy, HarnessId::ClaudeCode, None);
        // Writing under the source id would land on the source's own file.
        assert_ne!(copy.meta.id, id);
        assert!(copy.meta.timestamp > ts, "the copy is filed under today");
        // v4 for everything but codex: version nibble, then the variant bits.
        assert_eq!(&copy.meta.id[14..15], "4");
        assert!(matches!(&copy.meta.id[19..20], "8" | "9" | "a" | "b"));
    }

    #[test]
    fn codex_copies_get_the_v7_shape_codex_mints_itself() {
        let mut copy = transcript();
        fresh_identity(&mut copy, HarnessId::Codex, None);
        assert_eq!(&copy.meta.id[14..15], "7");
    }

    #[test]
    fn an_out_export_keeps_the_source_identity() {
        let mut copy = transcript();
        let (id, ts) = (copy.meta.id.clone(), copy.meta.timestamp);
        fresh_identity(
            &mut copy,
            HarnessId::Codex,
            Some(std::path::Path::new("/tmp/x")),
        );
        assert_eq!(copy.meta.id, id);
        assert_eq!(copy.meta.timestamp, ts);
    }
}

fn cmd_list(
    from: Option<HarnessId>,
    cwd: Option<&std::path::Path>,
    limit: Option<usize>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    until: Option<chrono::DateTime<chrono::Utc>>,
) {
    let sessions = discover_with_spinner();
    let listed: Vec<_> = sessions
        .iter()
        .filter(|s| {
            selected(s, from, cwd)
                && since.is_none_or(|t| s.meta.timestamp >= t)
                && until.is_none_or(|t| s.meta.timestamp <= t)
        })
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    if listed.is_empty() {
        let scope = cwd.map_or(String::new(), |d| format!(" for {}", d.display()));
        let when = match (since, until) {
            (None, None) => String::new(),
            _ => " in that time range".to_string(),
        };
        match from {
            Some(h) => println!("no local {h} sessions found{scope}{when}"),
            None => println!("no local sessions found{scope}{when}"),
        }
    } else {
        use std::io::Write;
        let color = style::enabled();
        // A failed write means the reader is gone (`txcript list | head`):
        // stop quietly instead of panicking the way `println!` would.
        let mut out = std::io::stdout().lock();
        let header = format!(
            "{:<12}  {:<10}  {:<38}  TITLE / FIRST MESSAGE",
            "HARNESS", "WHEN", "ID"
        );
        if writeln!(out, "{}", style::dim(&header, color)).is_err() {
            return;
        }
        for s in listed {
            let label = s
                .meta
                .title
                .clone()
                .unwrap_or_else(|| s.meta.cwd.clone().unwrap_or_default());
            let row = format!(
                "{}  {}  {}  {}",
                style::harness(s.harness, 12, color),
                style::dim(&format!("{:<10}", format_when(s.meta.timestamp)), color),
                style::dim(
                    &format!("{:<38}", truncate(&style::scrub(&s.meta.id), 38)),
                    color
                ),
                truncate(&style::scrub(&label), 60)
            );
            if writeln!(out, "{row}").is_err() {
                return;
            }
        }
    }
}

/// ANSI styling for the printing commands: colors reach a terminal, plain
/// text reaches a pipe or redirect (and everywhere when `NO_COLOR` is set).
/// Padding happens before coloring — escape bytes would otherwise count
/// against the column width.
mod style {
    use std::io::IsTerminal;

    use txcript::HarnessId;

    pub fn enabled() -> bool {
        std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
    }

    /// [`enabled`], but for output on stderr (the status lines).
    pub fn enabled_err() -> bool {
        std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
    }

    pub fn dim(s: &str, on: bool) -> String {
        if on {
            format!("\x1b[2m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Neutralize control characters in transcript-derived text (ids, titles,
    /// matched lines, recorded paths) before it reaches the terminal: a
    /// session file could otherwise drive the terminal itself — ANSI/OSC
    /// state, clipboard writes, forged rows. One space per control character
    /// keeps char counts, and with them column padding and highlight spans,
    /// unchanged.
    pub fn scrub(s: &str) -> String {
        s.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect()
    }

    /// The harness name padded to `pad`, in its color when `on`. Each harness
    /// keeps a stable color so a mixed listing reads at a glance.
    pub fn harness(h: HarnessId, pad: usize, on: bool) -> String {
        let name = format!("{:<pad$}", h.as_str());
        if on {
            format!("{}{name}\x1b[0m", color(h))
        } else {
            name
        }
    }

    const fn color(h: HarnessId) -> &'static str {
        match h {
            HarnessId::ClaudeCode => "\x1b[33m",    // yellow
            HarnessId::Codex => "\x1b[36m",         // cyan
            HarnessId::OpenCode => "\x1b[32m",      // green
            HarnessId::Pi => "\x1b[35m",            // magenta
            HarnessId::Campfire => "\x1b[91m",      // bright red
            HarnessId::Cursor => "\x1b[34m",        // blue
            HarnessId::CursorDesktop => "\x1b[96m", // bright cyan
            HarnessId::Grok => "\x1b[37m",          // white
            HarnessId::Amp => "\x1b[95m",           // bright magenta
            HarnessId::Antigravity => "\x1b[94m",   // bright blue
            HarnessId::Simple => "\x1b[92m",        // bright green
        }
    }
}

fn cmd_continue(
    id: &str,
    with: Option<HarnessId>,
    from: Option<HarnessId>,
    out: Option<&PathBuf>,
    no_resume: bool,
) -> Result<ExitCode, String> {
    // Locate the session by id (exact or unambiguous prefix) or exact title,
    // optionally scoped to one harness.
    let sessions = discover_with_spinner();
    // A whole-input match (a title that itself contains `#12`) beats the
    // fragment interpretation.
    let (src, request) = match fragment::parse_ref(id) {
        (_, Some(_)) if find_exact(&sessions, from, id).is_some() => (id, None),
        parsed => parsed,
    };
    let found = find_session(&sessions, from, src)?;

    // Resuming an `--out` copy can't work — the harness reads its live root, not
    // our redirect — so a redirect implies "write only".
    let resume = out.is_none() && !no_resume;
    match found {
        Some(found) => continue_session(
            found,
            with,
            request.as_ref(),
            out.map(PathBuf::as_path),
            resume,
        ),
        // Modern Amp CLIs are server-authoritative and write no local thread
        // files; an Amp-shaped id that isn't on disk may still exist on
        // ampcode.com, reachable through Amp's own exporter.
        None if matches!(from, None | Some(HarnessId::Amp)) && is_amp_thread_id(src) => {
            continue_amp_server_thread(
                src,
                with,
                request.as_ref(),
                out.map(PathBuf::as_path),
                resume,
            )
        }
        None => Err(match from {
            Some(h) => format!("no {h} session matches `{src}` (try `txcript list`)"),
            None => format!("no local session matches `{src}` (try `txcript list`)"),
        }),
    }
}

/// The id shape Amp mints and validates: `T-` then 8+ `[A-Za-z0-9-]`.
fn is_amp_thread_id(id: &str) -> bool {
    id.strip_prefix("T-").is_some_and(|rest| {
        rest.len() >= 8 && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

/// Fetch a server-side Amp thread via `amp threads export` and continue it:
/// same-harness resumes by id (the thread already lives where Amp reads it);
/// any other target gets the usual convert-and-write.
fn continue_amp_server_thread(
    id: &str,
    with: Option<HarnessId>,
    span_req: Option<&fragment::SpanReq>,
    out: Option<&std::path::Path>,
    resume: bool,
) -> Result<ExitCode, String> {
    eprintln!(
        "not on disk; fetching: {}",
        style::dim(&format!("amp threads export {id}"), style::enabled_err())
    );
    let output = std::process::Command::new("amp")
        .args(["threads", "export", id])
        .output()
        .map_err(|e| format!("running `amp threads export {id}`: {e} (is amp on PATH?)"))?;
    if !output.status.success() {
        return Err(format!(
            "`amp threads export {id}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let native = amp::Amp::from_text(&text).map_err(|e| e.to_string())?;
    let common: Transcript<Common> = amp::Amp::to_common(&native).map_err(|e| e.to_string())?;

    let target = with.unwrap_or(HarnessId::Amp);
    let resume_id = match (span_req, target == HarnessId::Amp && out.is_none()) {
        // The thread already lives server-side, exactly where Amp resumes from.
        (None, true) => id.to_string(),
        (None, false) => {
            let mut copy = common.clone();
            fresh_identity(&mut copy, target, out);
            stamp_live_cwd(&mut copy, out);
            write_and_report(HarnessId::Amp, target, &copy, out)?
        }
        // A sliced continue always rewrites — the server thread can't resume
        // a subset of itself in place.
        (Some(req), _) => {
            let mut copy = fragment::sliced(&common, req)?;
            fresh_identity(&mut copy, target, out);
            stamp_live_cwd(&mut copy, out);
            write_and_report(HarnessId::Amp, target, &copy, out)?
        }
    };
    launch(target, &resume_id, common.meta.cwd.as_deref(), resume)
}

/// Continue `found` in `with` (default: its own harness): same-harness resumes
/// in place, cross-harness re-synthesizes; then exec the harness if `resume`.
/// A `span_req` restricts the continue to that message range (always as a
/// rewritten copy — the original can't resume a subset of itself in place).
fn continue_session(
    found: &local::Session,
    with: Option<HarnessId>,
    span_req: Option<&fragment::SpanReq>,
    out: Option<&std::path::Path>,
    resume: bool,
) -> Result<ExitCode, String> {
    let target = with.unwrap_or(found.harness);
    let in_place = span_req.is_none() && target == found.harness && out.is_none();

    let resume_id = match (span_req, in_place) {
        // Same-harness live sessions can resume by id without rewriting.
        (None, true) => found.meta.id.clone(),
        (None, false) => {
            let mut common = found.read().map_err(|e| e.to_string())?;
            fresh_identity(&mut common, target, out);
            stamp_live_cwd(&mut common, out);
            write_and_report(found.harness, target, &common, out)?
        }
        (Some(req), _) => {
            let common = found.read().map_err(|e| e.to_string())?;
            let mut copy = fragment::sliced(&common, req)?;
            fresh_identity(&mut copy, target, out);
            stamp_live_cwd(&mut copy, out);
            write_and_report(found.harness, target, &copy, out)?
        }
    };

    launch(target, &resume_id, found.meta.cwd.as_deref(), resume)
}

/// Give a to-be-written copy its own identity.
///
/// The stores key their files by `meta.id` and date-shard by `meta.timestamp`,
/// so a copy written under the source's identity lands exactly where the
/// source lives: a `#range` continue would overwrite the very session it
/// sliced, silently discarding every message outside the range, and a
/// cross-harness copy would be filed under the original's date instead of
/// today's.
///
/// `--out` is exempt: it redirects to a scratch root rather than a live store,
/// where preserving the source identity makes the write a faithful export.
fn fresh_identity(
    common: &mut Transcript<Common>,
    target: HarnessId,
    out: Option<&std::path::Path>,
) {
    if out.is_some() {
        return;
    }
    // Codex stamps its rollouts with v7 UUIDs; matching the shape keeps the
    // copy out of any version-aware code path. v4 everywhere else. Harnesses
    // that need a different spelling (opencode's `ses_` prefix) re-shape this
    // themselves in `from_common`.
    common.meta.id = match target {
        HarnessId::Codex => uuid::Uuid::now_v7().to_string(),
        _ => uuid::Uuid::new_v4().to_string(),
    };
    common.meta.timestamp = chrono::Utc::now();
}

/// Re-home a live-store copy whose recorded cwd no longer exists. The stores
/// shard by `meta.cwd`, so a copy filed under a dead directory would be
/// invisible to the harness, which `launch` starts from the fallback
/// (current) directory in that case. `--out` exports keep the recorded cwd —
/// they're faithful exports, and no harness reads them in place.
fn stamp_live_cwd(common: &mut Transcript<Common>, out: Option<&std::path::Path>) {
    if out.is_some() {
        return;
    }
    let dead = common
        .meta
        .cwd
        .as_deref()
        .is_some_and(|c| !c.is_empty() && !std::path::Path::new(c).is_dir());
    if dead && let Ok(current) = std::env::current_dir() {
        common.meta.cwd = Some(current.to_string_lossy().into_owned());
    }
}

/// Write `common` as `target`'s native format, print the conversion line,
/// and return the id to resume with.
fn write_and_report(
    source: HarnessId,
    target: HarnessId,
    common: &Transcript<Common>,
    out: Option<&std::path::Path>,
) -> Result<String, String> {
    let written = local::write(target, common, out).map_err(|e| e.to_string())?;
    let on = style::enabled();
    println!(
        "{} → {}  {}",
        style::harness(source, 0, on),
        style::harness(target, 0, on),
        // `location` is Debug-rendered by the lib (its Ref is generic);
        // shed the quotes it puts around paths.
        style::dim(written.location.trim_matches('"'), on)
    );
    Ok(written.id)
}

/// Exec (or print) the harness resume command for `resume_id`.
fn launch(
    target: HarnessId,
    resume_id: &str,
    cwd: Option<&str>,
    resume: bool,
) -> Result<ExitCode, String> {
    let (bin, args) = local::resume_command(target, resume_id);
    if resume {
        // Hand the terminal to the harness — replaces this process on Unix.
        let workdir = resume_workdir(cwd);
        // The id inside the command came from a session file; scrub it for
        // display (the exec below still gets the exact argv).
        let shown = style::scrub(
            &std::iter::once(&bin)
                .chain(&args)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        );
        match &workdir {
            Some(dir) => eprintln!(
                "resuming: {shown} {}",
                style::dim(&format!("(in {})", dir.display()), style::enabled_err())
            ),
            None => eprintln!("resuming: {shown}"),
        }
        // Brief pause so users can read or cancel before exec.
        if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            std::thread::sleep(std::time::Duration::from_millis(600));
        }
        handoff(&bin, &args, workdir.as_deref())
    } else {
        println!(
            "  resume with: {}",
            style::scrub(&format!("{} {}", bin, args.join(" ")))
        );
        Ok(ExitCode::SUCCESS)
    }
}

/// Return the recorded cwd if it exists; otherwise warn and use the current
/// directory.
fn resume_workdir(cwd: Option<&str>) -> Option<PathBuf> {
    cwd.filter(|c| !c.is_empty()).and_then(|c| {
        let dir = PathBuf::from(c);
        if dir.is_dir() {
            Some(dir)
        } else {
            eprintln!(
                "warning: session cwd `{}` no longer exists; resuming from the current directory",
                style::scrub(c)
            );
            None
        }
    })
}

fn discover_with_spinner() -> Vec<local::Session> {
    let spinner = spin::Spinner::start("searching local sessions…");
    let sessions = local::discover_with(|harness, count| {
        spinner.set(format!("scanning {harness}… ({count} found)"));
    });
    spinner.finish();
    sessions
}

/// Replace this process with the harness from `workdir` when given. On
/// non-Unix, spawn and wait, then report the child's code.
#[cfg(unix)]
fn handoff(
    bin: &str,
    args: &[String],
    workdir: Option<&std::path::Path>,
) -> Result<ExitCode, String> {
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args);
    if let Some(dir) = workdir {
        cmd.current_dir(dir);
    }
    // `exec` only returns if it failed to launch.
    let e = cmd.exec();
    Err(format!("failed to launch `{bin}`: {e} (is it on PATH?)"))
}

#[cfg(not(unix))]
fn handoff(
    bin: &str,
    args: &[String],
    workdir: Option<&std::path::Path>,
) -> Result<ExitCode, String> {
    let spawn = |program: &str| {
        let mut cmd = std::process::Command::new(program);
        cmd.args(args);
        if let Some(dir) = workdir {
            cmd.current_dir(dir);
        }
        cmd.status()
    };
    let status = match spawn(bin) {
        Ok(status) => status,
        // npm-installed harnesses are `.cmd` shims on Windows, which
        // CreateProcess won't resolve from the bare name; the explicit
        // extension makes std route the launch through cmd.exe.
        Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::NotFound => {
            spawn(&format!("{bin}.cmd"))
                .map_err(|_| format!("failed to launch `{bin}`: {e} (is it on PATH?)"))?
        }
        Err(e) => return Err(format!("failed to launch `{bin}`: {e} (is it on PATH?)")),
    };
    Ok(match status.code() {
        // `ExitCode` is u8-wide; a child code outside 0..=255 still reports
        // failure, just not the exact value.
        Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        // No code (killed by a signal-equivalent): treated as success, as the
        // previous `exit(code.unwrap_or(0))` did.
        None => ExitCode::SUCCESS,
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// A tiny background spinner on stderr, so a slow scan shows it's alive.
/// No-op when stderr isn't a terminal (piped or redirected output stays clean).
mod spin {
    use std::io::{IsTerminal, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    pub struct Spinner {
        running: Arc<AtomicBool>,
        label: Arc<Mutex<String>>,
        handle: Option<JoinHandle<()>>,
        active: bool,
    }

    impl Spinner {
        pub fn start(initial: &str) -> Self {
            let active = std::io::stderr().is_terminal();
            let running = Arc::new(AtomicBool::new(true));
            let label = Arc::new(Mutex::new(initial.to_string()));
            let handle = active.then(|| {
                let (running, label) = (running.clone(), label.clone());
                thread::spawn(move || {
                    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                    let mut err = std::io::stderr();
                    let mut i = 0;
                    while running.load(Ordering::Relaxed) {
                        let text = label.lock().map(|g| g.clone()).unwrap_or_default();
                        let _ = write!(err, "\r\x1b[2K{} {text}", FRAMES[i % FRAMES.len()]);
                        let _ = err.flush();
                        i += 1;
                        thread::sleep(Duration::from_millis(80));
                    }
                })
            });
            Self {
                running,
                label,
                handle,
                active,
            }
        }

        pub fn set(&self, text: String) {
            if self.active
                && let Ok(mut g) = self.label.lock()
            {
                *g = text;
            }
        }

        /// Stop the spinner and clear its line.
        pub fn finish(self) {
            self.running.store(false, Ordering::Relaxed);
            if let Some(h) = self.handle {
                let _ = h.join();
            }
            if self.active {
                let mut err = std::io::stderr();
                let _ = write!(err, "\r\x1b[2K");
                let _ = err.flush();
            }
        }
    }
}

// ── query: one-shot search and the fzf-style picker ─────────────────────

mod query {
    use std::collections::HashMap;

    use txcript::search::{DocKey, DocMatch, Extracted, Index, Origin, Query};
    use txcript::{HarnessId, local};

    // Keyed by the full DocKey — source included — so two sessions sharing a
    // (harness, id), as Claude Code writes when one session is resumed from
    // another cwd, both stay reachable instead of one overwriting the other.
    type Sessions = HashMap<DocKey, local::Session>;

    /// Build the same filtered index used by the CLI for the MCP search tool.
    pub(super) fn index_for(from: Option<HarnessId>, cwd: Option<&std::path::Path>) -> Index {
        build_index(from, cwd).0
    }

    pub(super) fn cmd_query(
        pattern: Option<String>,
        with: Option<HarnessId>,
        from: Option<HarnessId>,
        cwd: Option<&std::path::Path>,
    ) -> Result<std::process::ExitCode, String> {
        let (index, sessions) = build_index(from, cwd);
        match pattern {
            Some(pattern) => {
                if with.is_some() {
                    eprintln!("warning: --with ignored with a pattern");
                }
                one_shot(&index, &pattern);
                Ok(std::process::ExitCode::SUCCESS)
            }
            None => match tui::pick(&index)? {
                // Cancelled; terminal already restored, nothing to continue.
                None => Ok(std::process::ExitCode::SUCCESS),
                Some(key) => {
                    let session = sessions
                        .get(&key)
                        .ok_or("internal error: picked session not found")?;
                    drop(index);
                    super::continue_session(session, with, None, None, true)
                }
            },
        }
    }

    /// Build the search index and session lookup. Sessions parse and extract
    /// on every core: workers pull the next undrained session, parse it,
    /// extract its searchable lines, and send the result back over a bounded
    /// channel; this thread folds arrivals into the index as they land, so
    /// at most a few extracted documents are ever in flight.
    fn build_index(from: Option<HarnessId>, cwd: Option<&std::path::Path>) -> (Index, Sessions) {
        let found = super::discover_with_spinner();
        let spinner = super::spin::Spinner::start("indexing…");
        let scoped: Vec<local::Session> = found
            .into_iter()
            .filter(|session| super::selected(session, from, cwd))
            .collect();
        let total = scoped.len();
        let workers = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let mut index = Index::new();
        // Which sessions parsed cleanly, by position in `scoped`; the lookup
        // map is built from these after the workers release their borrow.
        let mut indexed = vec![false; total];
        let next = std::sync::atomic::AtomicUsize::new(0);
        let (tx, rx) = std::sync::mpsc::sync_channel(workers * 2);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let tx = tx.clone();
                let (next, scoped) = (&next, &scoped);
                scope.spawn(move || {
                    std::iter::from_fn(|| {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        (i < scoped.len()).then_some(i)
                    })
                    // Unreadable sessions are skipped, matching discover.
                    .filter_map(|i| {
                        scoped[i].read().ok().map(|common| {
                            let key = DocKey {
                                harness: scoped[i].harness,
                                id: scoped[i].meta.id.clone(),
                                source: Some(scoped[i].location()),
                            };
                            (i, Extracted::new(key, &common))
                        })
                    })
                    .for_each(|extracted| {
                        // A send only fails when the receiver is gone, and
                        // this thread's scope outlives it.
                        let _ = tx.send(extracted);
                    });
                });
            }
            // Workers hold the remaining clones; the receive loop below ends
            // when the last of them finishes.
            drop(tx);
            let mut arrived = Vec::with_capacity(total);
            for (i, extracted) in rx {
                if arrived.len() % 32 == 0 {
                    spinner.set(format!("indexing… ({}/{total})", arrived.len()));
                }
                arrived.push((i, extracted));
                indexed[i] = true;
            }
            // Insert in discovery order, not arrival order: document order
            // breaks full score-and-timestamp ties in query results, and it
            // should not vary run to run.
            arrived.sort_unstable_by_key(|&(i, _)| i);
            for (_, extracted) in arrived {
                index.insert_extracted(extracted);
            }
        });
        let sessions: Sessions = scoped
            .into_iter()
            .zip(&indexed)
            .filter_map(|(session, &ok)| {
                ok.then(|| {
                    let key = DocKey {
                        harness: session.harness,
                        id: session.meta.id.clone(),
                        source: Some(session.location()),
                    };
                    (key, session)
                })
            })
            .collect();
        spinner.finish();
        (index, sessions)
    }

    /// Print ranked hits for a pattern, colorized when stdout is a terminal.
    fn one_shot(index: &Index, pattern: &str) {
        use std::io::{IsTerminal, Write};
        let mut q = Query::fuzzy(pattern);
        q.limit = Some(20);
        q.hits_per_doc = Some(3);
        let matches = index.query(&q);
        if matches.is_empty() {
            println!("no matches for `{pattern}`");
        } else {
            let color = std::io::stdout().is_terminal();
            // A failed write means the reader is gone (`… | head`): stop
            // quietly instead of panicking the way `println!` would.
            let mut out = std::io::stdout().lock();
            for m in &matches {
                if writeln!(out, "{}", doc_line(m, color)).is_err() {
                    return;
                }
                for hit in &m.hits {
                    let line = format!(
                        "  [{:>11}] {}",
                        origin_label(hit.origin),
                        highlight(&hit.line, &hit.highlights, 120, color)
                    );
                    if writeln!(out, "{line}").is_err() {
                        return;
                    }
                }
            }
        }
    }

    /// Label used in query result columns.
    pub(super) fn origin_label(origin: Origin) -> &'static str {
        match origin {
            Origin::User => "user",
            Origin::Assistant => "assistant",
            Origin::Thinking => "thinking",
            Origin::ToolUse => "tool_use",
            Origin::ToolResult => "tool_result",
            Origin::Meta => "meta",
        }
    }

    fn doc_line(m: &DocMatch<'_>, color: bool) -> String {
        let label = m
            .meta
            .title
            .clone()
            .or_else(|| m.meta.cwd.as_deref().map(basename))
            .unwrap_or_default();
        let date = m.meta.timestamp.format("%Y-%m-%d %H:%M");
        format!(
            "{}  {}  {}  {}",
            crate::style::harness(m.key.harness, 0, color),
            crate::style::dim(&crate::style::scrub(&m.key.id), color),
            crate::style::dim(&date.to_string(), color),
            crate::style::scrub(&label)
        )
    }

    pub(super) fn basename(path: &str) -> String {
        std::path::Path::new(path)
            .file_name()
            .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
    }

    /// Render `line` truncated to `width` chars, match spans emphasized.
    pub(super) fn highlight(
        line: &str,
        spans: &[std::ops::Range<u32>],
        width: usize,
        color: bool,
    ) -> String {
        let mut out = String::new();
        let mut in_span = false;
        for (i, ch) in line.chars().take(width).enumerate() {
            let i = u32::try_from(i).unwrap_or(u32::MAX);
            let now = spans.iter().any(|s| s.contains(&i));
            if color && now != in_span {
                out.push_str(if now { "\x1b[1;31m" } else { "\x1b[0m" });
                in_span = now;
            }
            // Matched lines are transcript content: a control character here
            // could drive the terminal. Same one-for-one swap as
            // `style::scrub`, inline to keep the span indexes aligned.
            out.push(if ch.is_control() { ' ' } else { ch });
        }
        if color && in_span {
            out.push_str("\x1b[0m");
        }
        if line.chars().count() > width {
            out.push('…');
        }
        out
    }

    // ── the picker ───────────────────────────────────────────────────────

    #[cfg(unix)]
    mod tui {
        use std::collections::VecDeque;
        use std::io::{IsTerminal, Read, Write};
        use std::process::{Command, Stdio};

        use terminal_size::{Height, Width};
        use txcript::search::{DocKey, DocMatch, Hit, Index, Query};

        /// RAII guard for raw mode and the alternate screen.
        struct Term {
            saved: String,
        }

        impl Term {
            fn enter() -> Result<Term, String> {
                let saved = stty(&["-g"])?.trim().to_string();
                // min 0 time 1: reads poll at 100ms so a lone ESC is
                // distinguishable from an escape sequence.
                stty(&["raw", "-echo", "min", "0", "time", "1"])?;
                print!("\x1b[?1049h\x1b[?25l");
                let _ = std::io::stdout().flush();
                Ok(Term { saved })
            }
        }

        fn term_size() -> (usize, usize) {
            terminal_size::terminal_size().map_or((24, 80), |(Width(cols), Height(rows))| {
                (usize::from(rows), usize::from(cols))
            })
        }

        impl Drop for Term {
            fn drop(&mut self) {
                print!("\x1b[?25h\x1b[?1049l");
                let _ = std::io::stdout().flush();
                let _ = stty(&[&self.saved]);
            }
        }

        fn stty(args: &[&str]) -> Result<String, String> {
            let out = Command::new("stty")
                .args(args)
                .stdin(Stdio::inherit())
                .output()
                .map_err(|e| format!("stty: {e}"))?;
            if out.status.success() {
                Ok(String::from_utf8_lossy(&out.stdout).into_owned())
            } else {
                Err(format!(
                    "stty {}: {}",
                    args.join(" "),
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        }

        #[derive(Clone, Copy)]
        enum Key {
            Char(char),
            Backspace,
            Clear,
            Up,
            Down,
            Enter,
            Cancel,
            None,
        }

        /// Interactive fuzzy picker over the index. Returns the chosen doc,
        /// or `None` on cancel. The terminal is fully restored either way.
        pub(super) fn pick(index: &Index) -> Result<Option<DocKey>, String> {
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                // Raw mode and the alternate screen need real terminal stdio.
                Err("interactive query needs a terminal (pass a pattern instead)".into())
            } else {
                let term = Term::enter()?;
                let mut input = String::new();
                let mut selected = 0usize;
                let mut stdin = Input::new(std::io::stdin().lock());
                let (mut rows, mut cols) = term_size();
                let mut results = query(index, &input, rows);
                let mut pending = None;
                render(&input, &results, selected, index.len(), rows, cols);

                let picked = 'ui: loop {
                    let key = pending.take().map_or_else(|| read_key(&mut stdin), Ok)?;
                    match key {
                        // A poll timeout: nothing pressed, keep waiting.
                        Key::None => {}
                        Key::Char(c) => {
                            input.push(c);
                            selected = 0;
                            (rows, cols) = term_size();
                            results = query(index, &input, rows);
                            render(&input, &results, selected, index.len(), rows, cols);
                        }
                        Key::Backspace => {
                            input.pop();
                            selected = 0;
                            (rows, cols) = term_size();
                            results = query(index, &input, rows);
                            render(&input, &results, selected, index.len(), rows, cols);
                        }
                        Key::Clear => {
                            input.clear();
                            selected = 0;
                            (rows, cols) = term_size();
                            results = query(index, &input, rows);
                            render(&input, &results, selected, index.len(), rows, cols);
                        }
                        Key::Up | Key::Down => {
                            move_selection(&mut selected, key, results.len());
                            // A held arrow can put several complete key
                            // sequences in one terminal read. Apply all of
                            // them, then render the final row once.
                            pending = drain_navigation(&mut stdin, &mut selected, results.len())?;

                            let (new_rows, new_cols) = term_size();
                            if new_rows != rows {
                                results = query(index, &input, new_rows);
                                selected = selected.min(results.len().saturating_sub(1));
                            }
                            (rows, cols) = (new_rows, new_cols);
                            render(&input, &results, selected, index.len(), rows, cols);
                        }
                        // Enter with no match under the cursor: keep waiting.
                        Key::Enter => {
                            if let Some(key) = results.key(selected) {
                                break 'ui Some(key.clone());
                            }
                        }
                        Key::Cancel => break 'ui None,
                    }
                };
                drop(term);
                Ok(picked)
            }
        }

        struct Results<'a> {
            docs: Vec<DocMatch<'a>>,
            rows: Vec<ResultRow>,
            searching: bool,
        }

        #[derive(Clone, Copy)]
        struct ResultRow {
            doc: usize,
            hit: Option<usize>,
        }

        impl<'a> Results<'a> {
            fn len(&self) -> usize {
                self.rows.len()
            }

            fn key(&self, row: usize) -> Option<&DocKey> {
                self.rows
                    .get(row)
                    .and_then(|row| self.docs.get(row.doc))
                    .map(|doc| doc.key)
            }

            fn get(&self, row: usize) -> Option<(&DocMatch<'a>, Option<&Hit>)> {
                let row = self.rows.get(row)?;
                let doc = self.docs.get(row.doc)?;
                Some((doc, row.hit.and_then(|hit| doc.hits.get(hit))))
            }
        }

        fn query<'a>(index: &'a Index, input: &str, rows: usize) -> Results<'a> {
            let visible = rows.saturating_sub(2).max(1);
            let searching = !input.trim().is_empty();
            let mut q = Query::fuzzy(input);
            q.limit = Some(visible);
            q.hits_per_doc = searching.then_some(visible);
            let docs = index.query(&q);

            let mut result_rows = if searching {
                docs.iter()
                    .enumerate()
                    .flat_map(|(doc, result)| {
                        (0..result.hits.len()).map(move |hit| ResultRow {
                            doc,
                            hit: Some(hit),
                        })
                    })
                    .collect::<Vec<_>>()
            } else {
                (0..docs.len())
                    .map(|doc| ResultRow { doc, hit: None })
                    .collect()
            };

            if searching {
                // Search rows are occurrences, ranked independently. A
                // session can therefore occupy several rows when it contains
                // several strong matches.
                result_rows.sort_by(|a, b| {
                    let a_score = a.hit.map_or(0, |hit| docs[a.doc].hits[hit].score);
                    let b_score = b.hit.map_or(0, |hit| docs[b.doc].hits[hit].score);
                    b_score
                        .cmp(&a_score)
                        .then_with(|| a.doc.cmp(&b.doc))
                        .then_with(|| a.hit.cmp(&b.hit))
                });
                result_rows.truncate(visible);
            }

            Results {
                docs,
                rows: result_rows,
                searching,
            }
        }

        fn move_selection(selected: &mut usize, key: Key, len: usize) {
            match key {
                Key::Up => *selected = selected.saturating_sub(1),
                Key::Down => {
                    *selected = selected.saturating_add(1).min(len.saturating_sub(1));
                }
                _ => {}
            }
        }

        /// Apply navigation keys already captured by the current terminal
        /// read. The first non-navigation key is preserved for the next loop.
        fn drain_navigation(
            stdin: &mut Input<impl Read>,
            selected: &mut usize,
            len: usize,
        ) -> Result<Option<Key>, String> {
            while stdin.has_buffered() {
                let key = read_key(stdin)?;
                match key {
                    Key::Up | Key::Down => move_selection(selected, key, len),
                    Key::None => {}
                    other => return Ok(Some(other)),
                }
            }
            Ok(None)
        }

        fn render(
            input: &str,
            results: &Results<'_>,
            selected: usize,
            total: usize,
            rows: usize,
            cols: usize,
        ) {
            use std::fmt::Write as _;
            // The match count is post-limit: a full page means "at least".
            let count = if results.len() >= rows.saturating_sub(2) {
                format!("{}+", results.len())
            } else {
                results.len().to_string()
            };
            let summary = if results.searching {
                format!("{count} matches")
            } else {
                format!("{count}/{total}")
            };
            let mut frame = String::from("\x1b[H\x1b[2J");
            let _ = write!(
                frame,
                "\x1b[1m>\x1b[0m {input}\x1b[7m \x1b[0m\r\n\x1b[2m  {summary}\x1b[0m"
            );
            // Lines are *prefixed* with \r\n: a trailing newline on the last
            // row would scroll the prompt off the top of the screen.
            for i in 0..results.len().min(rows.saturating_sub(2)) {
                let Some((doc, hit)) = results.get(i) else {
                    continue;
                };
                let line = row(doc, hit, cols.saturating_sub(2));
                if i == selected {
                    // The row's internal styling ends in resets that would
                    // kill the selection underline mid-line: re-assert it
                    // after each, and pad to the row edge so the underline
                    // runs the full width.
                    let pad =
                        " ".repeat(cols.saturating_sub(2).saturating_sub(visible_width(&line)));
                    let line = line.replace("\x1b[0m", "\x1b[0m\x1b[4m");
                    let _ = write!(frame, "\r\n\x1b[4m▌{line}{pad}\x1b[0m");
                } else {
                    let _ = write!(frame, "\r\n {line}");
                }
            }
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(frame.as_bytes());
            let _ = out.flush();
        }

        /// One list row: harness, date, label, then this row's hit line
        /// prefixed by what kind of content it matched in. Empty-query rows
        /// have no hit and represent the session itself.
        fn row(m: &DocMatch<'_>, hit: Option<&Hit>, cols: usize) -> String {
            let label = m
                .meta
                .title
                .clone()
                .or_else(|| m.meta.cwd.as_deref().map(super::basename))
                .unwrap_or_default();
            let head = format!(
                "{} \x1b[2m{} {:<8}\x1b[0m {:<24} ",
                crate::style::harness(m.key.harness, 11, true),
                m.meta.timestamp.format("%m-%d %H:%M"),
                truncate_chars(&crate::style::scrub(&m.key.id), 8),
                truncate_chars(&crate::style::scrub(&label), 24),
            );
            // 11 + 1 + 11 + 1 + 8 + 1 + 24 + 1 visible chars so far.
            let room = cols.saturating_sub(58);
            let preview = hit.map_or_else(String::new, |hit| {
                format!(
                    "\x1b[2m{:>11}\x1b[0m {}",
                    super::origin_label(hit.origin),
                    // 11 + 1 for the origin column.
                    super::highlight(&hit.line, &hit.highlights, room.saturating_sub(12), true)
                )
            });
            format!("{head}{preview}")
        }

        /// Character width of `s` with its ANSI escape sequences stripped —
        /// what the terminal will actually render.
        fn visible_width(s: &str) -> usize {
            let mut in_escape = false;
            s.chars()
                .filter(|&c| match (in_escape, c) {
                    (false, '\x1b') => {
                        in_escape = true;
                        false
                    }
                    (false, _) => true,
                    // `m` closes every sequence this UI emits (SGR only).
                    (true, 'm') => {
                        in_escape = false;
                        false
                    }
                    (true, _) => false,
                })
                .count()
        }

        fn truncate_chars(s: &str, max: usize) -> String {
            if s.chars().count() <= max {
                s.to_string()
            } else {
                let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
                t.push('…');
                t
            }
        }

        /// Read one key, decoding UTF-8 and the arrow escape sequences. With
        /// `min 0 time 1`, a read can legitimately return nothing.
        // Separate arms distinguish timeout from ignored input.
        #[allow(clippy::match_same_arms)]
        fn read_key(stdin: &mut Input<impl Read>) -> Result<Key, String> {
            let key = match stdin.read_byte()? {
                // A poll timeout: nothing was pressed.
                None => Key::None,
                Some(0x03) => Key::Cancel, // ctrl-c
                Some(0x0a | 0x0d) => Key::Enter,
                Some(0x7f | 0x08) => Key::Backspace,
                Some(0x15) => Key::Clear, // ctrl-u
                Some(0x0e) => Key::Down,  // ctrl-n
                Some(0x10) => Key::Up,    // ctrl-p
                Some(0x1b) => match stdin.read_byte()? {
                    Some(b'[') => match stdin.read_byte()? {
                        Some(b'A') => Key::Up,
                        Some(b'B') => Key::Down,
                        // Any other (or truncated) CSI sequence: not a
                        // picker key.
                        Some(_) | None => Key::None,
                    },
                    None => Key::Cancel, // a lone ESC
                    // Other escape sequences (alt-chords): not picker keys.
                    Some(_) => Key::None,
                },
                Some(b) if (0x20..0x7f).contains(&b) => Key::Char(b as char),
                Some(b) if b >= 0xc2 => utf8_tail(stdin, b)?,
                // Unmapped control bytes and stray UTF-8 continuation bytes.
                Some(_) => Key::None,
            };
            Ok(key)
        }

        /// Finish a UTF-8 multibyte sequence whose lead byte was `lead`.
        fn utf8_tail(stdin: &mut Input<impl Read>, lead: u8) -> Result<Key, String> {
            let len = match lead {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                _ => 4, // 0xf0 and above (the caller guarantees lead >= 0xc2)
            };
            // `None` folds the whole tail to `None`: a poll timeout
            // mid-sequence means a truncated character, not a key.
            let tail: Option<Vec<u8>> = (1..len)
                .map(|_| stdin.read_byte())
                .collect::<Result<_, _>>()?;
            Ok(tail
                .map(|rest| std::iter::once(lead).chain(rest).collect())
                .and_then(|buf| String::from_utf8(buf).ok())
                .and_then(|s| s.chars().next())
                .map_or(Key::None, Key::Char))
        }

        /// Buffered terminal input. Reading a chunk instead of one byte at a
        /// time exposes already-queued repeat events so navigation can batch
        /// them without a nonblocking syscall or another thread.
        struct Input<R> {
            inner: R,
            buffered: VecDeque<u8>,
        }

        impl<R: Read> Input<R> {
            fn new(inner: R) -> Self {
                Self {
                    inner,
                    buffered: VecDeque::new(),
                }
            }

            fn has_buffered(&self) -> bool {
                !self.buffered.is_empty()
            }

            fn read_byte(&mut self) -> Result<Option<u8>, String> {
                if let Some(byte) = self.buffered.pop_front() {
                    return Ok(Some(byte));
                }

                let mut chunk = [0u8; 4096];
                match self.inner.read(&mut chunk) {
                    Ok(0) => Ok(None),
                    Ok(read) => {
                        self.buffered.extend(&chunk[1..read]);
                        Ok(Some(chunk[0]))
                    }
                    Err(e) => Err(format!("reading stdin: {e}")),
                }
            }
        }

        #[cfg(test)]
        mod tests {
            use super::{Input, Key, drain_navigation, move_selection, read_key};

            #[test]
            fn queued_navigation_is_applied_before_one_render() {
                let bytes = b"\x1b[B\x1b[B\x1b[B\x1b[B\x1b[Bx";
                let mut input = Input::new(&bytes[..]);
                let first = read_key(&mut input).unwrap();
                let mut selected = 0;
                move_selection(&mut selected, first, 20);

                let pending = drain_navigation(&mut input, &mut selected, 20).unwrap();

                assert_eq!(selected, 5);
                assert!(matches!(pending, Some(Key::Char('x'))));
            }

            #[test]
            fn batched_navigation_preserves_boundary_order() {
                let bytes = b"\x1b[A\x1b[B";
                let mut input = Input::new(&bytes[..]);
                let first = read_key(&mut input).unwrap();
                let mut selected = 0;
                move_selection(&mut selected, first, 20);

                let pending = drain_navigation(&mut input, &mut selected, 20).unwrap();

                assert_eq!(selected, 1);
                assert!(pending.is_none());
            }
        }
    }

    #[cfg(not(unix))]
    mod tui {
        use txcript::search::{DocKey, Index};

        pub(super) fn pick(_: &Index) -> Result<Option<DocKey>, String> {
            Err("the interactive picker is unix-only; pass a pattern instead".into())
        }
    }
}
