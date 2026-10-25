//! `txcript check` — what converting a session into a harness would lose.
//!
//! The source is resolved exactly like `continue` (a session id or title, a
//! Simple document file or `-`, an optional `#range`), measured with
//! [`txcript::fidelity`], and reported without writing anything or launching
//! an agent. The same measurement backs the one-line note and the `--strict`
//! gate on `continue` and `crop`, and `continue --dry-run`.

use std::fmt::Write as _;
use std::process::ExitCode;

use serde::Serialize;
use txcript::fidelity::{self, Category, Loss, LossKind, Options, Report, Severity};
use txcript::{Common, HarnessId, Transcript};

use crate::{fragment, style};

/// The JSON document's `schema` number. Bump it only for a change that
/// breaks existing readers; adding fields does not.
const SCHEMA: u32 = 1;

/// When `--fail-on` should make `check` exit non-zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum FailOn {
    /// Never (the default)
    None,
    /// When content is lost or changed: text, thinking, tool calls, results,
    /// images, artifacts
    Content,
    /// When accounting or content is lost or changed: usage, stop reasons,
    /// per-message models, thinking signatures
    Accounting,
    /// On any loss or change, session metadata included
    Metadata,
}

impl FailOn {
    /// The lowest severity that fails, if any.
    pub(crate) fn threshold(self) -> Option<Severity> {
        match self {
            FailOn::None => Option::None,
            FailOn::Content => Some(Severity::Content),
            FailOn::Accounting => Some(Severity::Accounting),
            FailOn::Metadata => Some(Severity::Metadata),
        }
    }
}

/// Why a harness cannot be written to, if it cannot. These are the harnesses
/// `local::write_with` refuses outright; a test keeps the two in step.
pub(crate) fn unwritable(target: HarnessId) -> Option<&'static str> {
    match target {
        HarnessId::ClaudeChat | HarnessId::ChatGpt => {
            Some("it is a live read-only source and is never continued into")
        }
        HarnessId::CoworkRemote => Some("cloud Cowork is pull-only"),
        HarnessId::Hermes => Some("its state.db is read-only and Hermes has no session import"),
        HarnessId::Amp => Some("amp threads live on the server and cannot be imported"),
        HarnessId::Simple => {
            Some("simple is an interchange input; sessions are not written into it")
        }
        _ => None,
    }
}

fn not_writable(target: HarnessId, reason: &str) -> String {
    format!("{target} is not writable: {reason}")
}

/// The targets a bare `check` compares: every harness `continue --with`
/// writes. `grok_bot` mints a live agent and only appears when named.
fn matrix_targets() -> Vec<HarnessId> {
    fidelity::TARGETS
        .into_iter()
        .filter(|t| unwritable(*t).is_none() && *t != HarnessId::GrokBot)
        .collect()
}

/// A session resolved for checking.
pub(crate) struct Resolved {
    pub harness: HarnessId,
    /// The transcript as it would be written: already cut to any `#range`.
    pub common: Transcript<Common>,
    /// Whether a `#range` cut it down.
    pub sliced: bool,
}

fn slice(
    harness: HarnessId,
    common: Transcript<Common>,
    request: Option<&fragment::SpanReq>,
) -> Result<Resolved, String> {
    match request {
        Some(req) => Ok(Resolved {
            harness,
            common: fragment::sliced(&common, req)?,
            sliced: true,
        }),
        None => Ok(Resolved {
            harness,
            common,
            sliced: false,
        }),
    }
}

/// Resolve `id` the way `continue` does: a Simple document first, then a live
/// source, then a local session by id or title, then an Amp server thread.
pub(crate) fn resolve(id: &str, from: Option<HarnessId>) -> Result<Resolved, String> {
    if let Some((input, request)) = crate::document_source(id) {
        if from.is_some() {
            return Err(crate::FROM_WITH_DOCUMENT.to_string());
        }
        let common = crate::load_document(&input)?;
        return slice(HarnessId::Simple, common, request.as_ref());
    }
    if let Some((harness, loaded)) = crate::load_direct_remote(id, from) {
        let (common, request) = loaded?;
        return slice(harness, common, request.as_ref());
    }
    let sessions = crate::discover_with_spinner(from)?;
    let (src, request) = match fragment::parse_ref(id) {
        (_, Some(_)) if crate::find_exact(&sessions, from, id).is_some() => (id, None),
        parsed => parsed,
    };
    match crate::find_session(&sessions, from, src)? {
        Some(found) => {
            let common = found.read().map_err(|e| e.to_string())?;
            slice(found.harness, common, request.as_ref())
        }
        None if matches!(from, None | Some(HarnessId::Amp)) && crate::is_amp_thread_id(src) => {
            let common = crate::load_amp_server_thread(src)?;
            slice(HarnessId::Amp, common, request.as_ref())
        }
        None => Err(crate::no_session_error(from, src)),
    }
}

/// One target's outcome.
struct Outcome {
    target: HarnessId,
    result: Result<Report, String>,
}

fn assess_all(common: &Transcript<Common>, targets: &[HarnessId], detail: bool) -> Vec<Outcome> {
    let options = Options { detail };
    targets
        .iter()
        .map(|&target| {
            let result = match unwritable(target) {
                Some(reason) => Err(not_writable(target, reason)),
                None => fidelity::assess_with(target, common, options)
                    .map_err(|e| format!("{target}: {e}")),
            };
            Outcome { target, result }
        })
        .collect()
}

/// How `check` presents its result.
#[derive(Clone, Copy)]
pub(crate) struct Present {
    pub json: bool,
    pub detail: bool,
    pub fail_on: Option<Severity>,
}

/// `txcript check`. With no `with`, one row per writable target; with some,
/// a full report for each.
pub(crate) fn cmd_check(
    id: &str,
    with: &[HarnessId],
    from: Option<HarnessId>,
    present: Present,
) -> Result<ExitCode, String> {
    let source = resolve(id, from)?;
    let targets = if with.is_empty() {
        matrix_targets()
    } else {
        with.to_vec()
    };
    let outcomes = assess_all(&source.common, &targets, present.detail);
    let text = if present.json {
        render_json(&source, &outcomes)?
    } else if with.is_empty() {
        render_matrix(&source, &outcomes, id)
    } else {
        outcomes
            .iter()
            .map(|o| render_target(&source, o, present.detail))
            .collect::<Vec<_>>()
            .join("\n")
    };
    // A failed write means the reader is gone (`txcript check … | head`):
    // finish quietly instead of panicking the way `print!` would.
    let _ = std::io::Write::write_all(&mut std::io::stdout(), text.as_bytes());
    Ok(exit_code(&outcomes, present.fail_on))
}

/// `continue --dry-run`: what `continue` would write, and what it would cost.
pub(crate) fn cmd_dry_run(
    id: &str,
    with: Option<HarnessId>,
    from: Option<HarnessId>,
    strict: bool,
) -> Result<ExitCode, String> {
    let source = resolve(id, from)?;
    let target = match (with, source.harness) {
        (Some(target), _) => target,
        (None, HarnessId::Simple) => return Err(crate::DOCUMENT_NEEDS_WITH.to_string()),
        (None, harness) => harness,
    };
    crate::ensure_resumable_source(source.harness, target)?;
    // What `continue` does without a range: resume the original where it is.
    let in_place = target == source.harness && !source.sliced && target != HarnessId::Simple;
    let (text, code) = if in_place {
        let text = format!(
            "continue {} in {target}: resumes the session in place; nothing is converted\n",
            style::scrub(&short(&source.common)),
        );
        (text, ExitCode::SUCCESS)
    } else {
        let outcome = assess_all(&source.common, &[target], false).remove(0);
        let text = render_target(&source, &outcome, false);
        let code = exit_code(&[outcome], strict.then_some(Severity::Content));
        (text, code)
    };
    let _ = std::io::Write::write_all(&mut std::io::stdout(), text.as_bytes());
    Ok(code)
}

fn exit_code(outcomes: &[Outcome], fail_on: Option<Severity>) -> ExitCode {
    let failed = outcomes.iter().any(|o| match &o.result {
        Err(_) => true,
        Ok(report) => fail_on.is_some_and(|min| report.severity >= min),
    });
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

// ── the `continue` / `crop` gate ───────────────────────────────────────

/// What `continue` and `crop` do about loss: say so on stderr, and with
/// `strict` refuse to write.
#[derive(Clone, Copy)]
pub(crate) struct Gate<'a> {
    pub quiet: bool,
    pub strict: bool,
    /// The session argument as the user typed it, for the suggested command.
    pub reference: &'a str,
    pub from: Option<HarnessId>,
}

impl<'a> Gate<'a> {
    /// The default: note any loss, never refuse.
    pub(crate) fn note_only(reference: &'a str) -> Gate<'a> {
        Gate {
            quiet: false,
            strict: false,
            reference,
            from: None,
        }
    }

    /// Measure `common` as it is about to be written into `target`. Returns
    /// the note to print once the write succeeds, or an error that stops the
    /// write when `strict` and content would be lost or changed.
    pub(crate) fn before_write(
        &self,
        target: HarnessId,
        common: &Transcript<Common>,
    ) -> Result<Option<String>, String> {
        if (self.quiet && !self.strict) || unwritable(target).is_some() {
            return Ok(None);
        }
        // A codec failure surfaces from the write itself, with its own error.
        let Ok(report) = fidelity::assess(target, common) else {
            return Ok(None);
        };
        let command = self.command(target);
        if self.strict && report.severity >= Severity::Content {
            return Err(format!(
                "refusing to convert to {target} with --strict: content would be lost or \
                 changed ({}); nothing was written; run '{command}' for details",
                summarize(&report, Severity::Content).unwrap_or_default()
            ));
        }
        // "Lossy" only when something is actually gone; a representation
        // that merely changed is not a loss.
        let lossy = report.items.iter().any(|i| i.kind == LossKind::Lost);
        let what = if lossy {
            "lossy conversion"
        } else {
            "conversion changes"
        };
        Ok(summarize(&report, Severity::Metadata)
            .filter(|_| !self.quiet)
            .map(|s| format!("note: {what}: {s}; run '{command}' for details")))
    }

    fn command(&self, target: HarnessId) -> String {
        let from = self.from.map_or(String::new(), |h| format!(" --from {h}"));
        format!(
            "{} check {}{from} --with {target}",
            crate::program(),
            shell_quote(self.reference)
        )
    }
}

/// `s` as one shell word.
fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./#:@%+=,-".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// What was lost or changed in categories at or above `floor`, in a few
/// words: `200 usage records, title`. `None` when nothing was.
fn summarize(report: &Report, floor: Severity) -> Option<String> {
    const SHOWN: usize = 6;
    let mut parts: Vec<(Category, usize)> = Vec::new();
    for item in report
        .items
        .iter()
        .filter(|i| i.kind != LossKind::Synthesized && i.category.severity() >= floor)
    {
        match parts.iter_mut().find(|(c, _)| *c == item.category) {
            Some((_, n)) => *n += item.count,
            None => parts.push((item.category, item.count)),
        }
    }
    if parts.is_empty() {
        return None;
    }
    let hidden = parts.len().saturating_sub(SHOWN);
    let mut text = parts
        .iter()
        .take(SHOWN)
        .map(|&(category, n)| phrase(category, n, None))
        .collect::<Vec<_>>()
        .join(", ");
    if hidden > 0 {
        let _ = write!(text, ", and {hidden} more");
    }
    Some(text)
}

// ── wording ────────────────────────────────────────────────────────────

/// `1 image` / `3 images`; a singular session-level field has no count.
fn phrase(category: Category, n: usize, path: Option<&str>) -> String {
    let (one, many) = match category {
        Category::Text => ("text block", "text blocks"),
        Category::Thinking => ("thinking block", "thinking blocks"),
        Category::ToolCall => ("tool call", "tool calls"),
        Category::ToolName => ("tool name renamed", "tool names renamed"),
        Category::ToolArgs => ("tool argument", "tool arguments"),
        Category::ToolResult => ("tool result", "tool results"),
        Category::Image => ("image", "images"),
        Category::Artifact => ("artifact", "artifacts"),
        Category::IsError => ("error flag", "error flags"),
        Category::Order => ("block moved out of order", "blocks moved out of order"),
        Category::ThinkingSignature => ("thinking signature", "thinking signatures"),
        Category::Usage => ("usage record", "usage records"),
        Category::StopReason => ("stop reason", "stop reasons"),
        Category::MessageModel => ("per-message model", "per-message models"),
        Category::Title => return "title".to_string(),
        Category::Model => return "model".to_string(),
        Category::GitBranch => return "git branch".to_string(),
        Category::Cwd => return "working directory".to_string(),
        Category::Lineage => return "lineage".to_string(),
        Category::Timestamp => ("message timestamp", "message timestamps"),
    };
    let (one, many) = match (category, path) {
        (Category::Usage, Some(_)) => ("usage field", "usage fields"),
        _ => (one, many),
    };
    let noun = if n == 1 { one } else { many };
    let mut text = format!("{n} {noun}");
    if let Some(path) = path {
        let _ = write!(text, " ({path})");
    }
    text
}

fn class_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Content => "content",
        Severity::Accounting => "accounting",
        Severity::Metadata => "metadata",
        Severity::None => "none",
    }
}

const CLASSES: [Severity; 3] = [Severity::Content, Severity::Accounting, Severity::Metadata];

fn preserved(class: Severity) -> &'static str {
    match class {
        Severity::Content => "text, thinking, tool calls, results, images preserved",
        Severity::Accounting => "usage, stop reasons, per-message models preserved",
        _ => "title, model, branch, working directory preserved",
    }
}

fn short(common: &Transcript<Common>) -> String {
    let id: String = common.meta.id.chars().take(8).collect();
    if id.is_empty() { "-".to_string() } else { id }
}

fn header(source: &Resolved) -> String {
    format!(
        "check {} ({}, {})",
        style::scrub(&short(&source.common)),
        source.harness,
        messages(source.common.body.len())
    )
}

fn messages(n: usize) -> String {
    format!("{n} message{}", if n == 1 { "" } else { "s" })
}

// ── human output ───────────────────────────────────────────────────────

/// What a class did overall: the worst thing among its lost and changed items.
fn status(report: &Report, class: Severity) -> &'static str {
    let in_class = |kind| {
        report
            .items
            .iter()
            .any(|i| i.kind == kind && i.category.severity() == class)
    };
    if in_class(LossKind::Lost) {
        "lost"
    } else if in_class(LossKind::Changed) {
        "changed"
    } else {
        "ok"
    }
}

fn item_line(item: &Loss, detail: bool) -> String {
    let tag = match item.kind {
        LossKind::Lost => "LOST",
        LossKind::Changed => "CHANGED",
        LossKind::Synthesized => "ADDED",
    };
    // A rename shows as `/release -> _release`; every other path qualifies
    // the noun, as in `tool arguments (Bash.description)`.
    let (path, example) = match item.category {
        Category::ToolName => (None, item.path.as_deref()),
        Category::Timestamp => (None, item.example.as_deref()),
        _ => (item.path.as_deref(), item.example.as_deref()),
    };
    let mut line = format!("{tag:<8} {}", phrase(item.category, item.count, path));
    if let Some(example) = example {
        let _ = write!(line, "  {}", style::scrub(example));
    }
    if detail && let Some(at) = item.first_at {
        let _ = write!(line, "  (first at message {})", at + 1);
    }
    line
}

fn render_target(source: &Resolved, outcome: &Outcome, detail: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{} -> {}", header(source), outcome.target);
    let report = match &outcome.result {
        Ok(report) => report,
        Err(error) => {
            let _ = writeln!(out, "  error      {}", style::scrub(error));
            return out;
        }
    };
    for class in CLASSES {
        let items: Vec<&Loss> = report
            .items
            .iter()
            .filter(|i| i.category.severity() == class)
            .collect();
        let name = class_name(class);
        if items.iter().all(|i| i.kind == LossKind::Synthesized) {
            let _ = writeln!(out, "  {name:<10} ok: {}", preserved(class));
        }
        for (n, item) in items.iter().enumerate() {
            let label = if n == 0 && status(report, class) != "ok" {
                name
            } else {
                ""
            };
            let _ = writeln!(out, "  {label:<10} {}", item_line(item, detail));
        }
    }
    if report.messages_before != report.messages_after {
        let how = if report.messages_after > report.messages_before {
            "the target splits turns"
        } else {
            "the target merges turns"
        };
        let _ = writeln!(
            out,
            "  {:<10} {} become {} ({how})",
            "structure",
            messages(report.messages_before),
            report.messages_after
        );
    }
    for warning in &report.warnings {
        let _ = writeln!(out, "  {:<10} {warning}", "warning");
    }
    let _ = writeln!(
        out,
        "  codec-level: measured on the target's session text; a store's own additions are not counted"
    );
    out
}

fn render_matrix(source: &Resolved, outcomes: &[Outcome], id: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}, codec-level, {} targets",
        header(source),
        outcomes.len()
    );
    let _ = writeln!(
        out,
        "{:<15} {:<8} {:<11} {:<9} notes",
        "target", "content", "accounting", "metadata"
    );
    for outcome in outcomes {
        match &outcome.result {
            Err(error) => {
                let _ = writeln!(
                    out,
                    "{:<15} error: {}",
                    outcome.target.as_str(),
                    style::scrub(error)
                );
            }
            Ok(report) => {
                let mut notes = Vec::new();
                if report.messages_before != report.messages_after {
                    notes.push(format!(
                        "messages {}->{}",
                        report.messages_before, report.messages_after
                    ));
                }
                let added = report.total(LossKind::Synthesized);
                if added > 0 {
                    notes.push(format!("{added} added"));
                }
                let row = format!(
                    "{:<15} {:<8} {:<11} {:<9} {}",
                    outcome.target.as_str(),
                    status(report, Severity::Content),
                    status(report, Severity::Accounting),
                    status(report, Severity::Metadata),
                    notes.join(", ")
                );
                let _ = writeln!(out, "{}", row.trim_end());
            }
        }
    }
    let _ = writeln!(
        out,
        "run '{} check {} --with <harness>' for details",
        crate::program(),
        shell_quote(id)
    );
    out
}

// ── JSON ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Document<'a> {
    schema: u32,
    source: SourceJson<'a>,
    targets: Vec<TargetJson<'a>>,
}

#[derive(Serialize)]
struct SourceJson<'a> {
    harness: HarnessId,
    id: &'a str,
    messages: usize,
}

#[derive(Serialize)]
struct TargetJson<'a> {
    harness: HarnessId,
    error: Option<&'a str>,
    messages_after: Option<usize>,
    severity: Option<Severity>,
    items: Vec<ItemJson<'a>>,
    warnings: &'a [String],
    counts: Option<Counts>,
}

#[derive(Serialize)]
struct ItemJson<'a> {
    kind: LossKind,
    category: Category,
    path: Option<&'a str>,
    count: usize,
    /// 1-based, as `view` numbers messages and `#range` selects them.
    first_message: Option<usize>,
    example: Option<&'a str>,
}

#[derive(Serialize)]
struct Counts {
    lost: usize,
    changed: usize,
    synthesized: usize,
}

fn render_json(source: &Resolved, outcomes: &[Outcome]) -> Result<String, String> {
    let targets = outcomes
        .iter()
        .map(|outcome| match &outcome.result {
            Err(error) => TargetJson {
                harness: outcome.target,
                error: Some(error),
                messages_after: None,
                severity: None,
                items: Vec::new(),
                warnings: &[],
                counts: None,
            },
            Ok(report) => TargetJson {
                harness: outcome.target,
                error: None,
                messages_after: Some(report.messages_after),
                severity: Some(report.severity),
                items: report
                    .items
                    .iter()
                    .map(|item| ItemJson {
                        kind: item.kind,
                        category: item.category,
                        path: item.path.as_deref(),
                        count: item.count,
                        first_message: item.first_at.map(|at| at + 1),
                        example: item.example.as_deref(),
                    })
                    .collect(),
                warnings: &report.warnings,
                counts: Some(Counts {
                    lost: report.total(LossKind::Lost),
                    changed: report.total(LossKind::Changed),
                    synthesized: report.total(LossKind::Synthesized),
                }),
            },
        })
        .collect();
    let document = Document {
        schema: SCHEMA,
        source: SourceJson {
            harness: source.harness,
            id: &source.common.meta.id,
            messages: source.common.body.len(),
        },
        targets,
    };
    let mut text = serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use txcript::common::{Block, Message, Meta, Role, StopReason, Tool};

    use super::*;

    fn session(title: Option<&str>, command: bool) -> Transcript<Common> {
        let at = DateTime::from_timestamp(1_790_000_000, 0).unwrap();
        let message = |role, content| Message {
            role,
            content,
            timestamp: at,
            model: None,
            stop_reason: None,
            usage: None,
        };
        let mut body = vec![
            message(
                Role::User,
                vec![Block::Text {
                    text: "hello".into(),
                }],
            ),
            Message {
                stop_reason: Some(StopReason::EndTurn),
                ..message(Role::Assistant, vec![Block::Text { text: "hi".into() }])
            },
        ];
        if command {
            body.push(message(
                Role::User,
                vec![Block::ToolUse {
                    id: "c1".into(),
                    tool: Tool::Command {
                        command: "/release".into(),
                        args: None,
                    },
                }],
            ));
        }
        Transcript::new(
            Meta {
                id: "abcdef012345".into(),
                timestamp: at,
                cwd: None,
                git_branch: None,
                title: title.map(Into::into),
                cli_version: None,
                model: None,
                lineage: None,
            },
            body,
        )
    }

    fn gate(quiet: bool, strict: bool) -> Gate<'static> {
        Gate {
            quiet,
            strict,
            reference: "my session",
            from: Some(HarnessId::ClaudeCode),
        }
    }

    #[test]
    fn the_note_summarizes_what_was_lost_and_names_the_command() {
        let note = gate(false, false)
            .before_write(HarnessId::Pi, &session(Some("T"), false))
            .unwrap()
            .unwrap();
        assert_eq!(
            note,
            "note: lossy conversion: title; run 'txcript check 'my session' \
             --from claude_code --with pi' for details"
        );
    }

    #[test]
    fn a_change_without_a_loss_is_not_called_lossy() {
        let mut t = session(None, false);
        t.body[1].stop_reason = Some(StopReason::Aborted);
        let note = gate(false, false)
            .before_write(HarnessId::ClaudeCode, &t)
            .unwrap()
            .unwrap();
        assert!(
            note.starts_with("note: conversion changes: 1 stop reason; run "),
            "{note}"
        );
    }

    #[test]
    fn nothing_lost_means_no_note() {
        let t = session(None, false);
        assert_eq!(
            gate(false, false).before_write(HarnessId::Pi, &t).unwrap(),
            None
        );
        assert_eq!(
            gate(false, true)
                .before_write(HarnessId::ClaudeCode, &t)
                .unwrap(),
            None
        );
    }

    #[test]
    fn quiet_silences_the_note_but_not_strict() {
        let titled = session(Some("T"), false);
        assert_eq!(
            gate(true, false)
                .before_write(HarnessId::Pi, &titled)
                .unwrap(),
            None
        );
        let cmd = session(None, true);
        let err = gate(true, true)
            .before_write(HarnessId::Pi, &cmd)
            .unwrap_err();
        assert!(
            err.starts_with("refusing to convert to pi with --strict"),
            "{err}"
        );
        assert!(err.contains("nothing was written"), "{err}");
    }

    #[test]
    fn strict_only_refuses_content_loss() {
        // Metadata loss alone is a note, not a refusal.
        let titled = session(Some("T"), false);
        let note = gate(false, true)
            .before_write(HarnessId::Pi, &titled)
            .unwrap();
        assert!(note.is_some());
        // A lost call is content.
        let cmd = session(None, true);
        assert!(gate(false, true).before_write(HarnessId::Pi, &cmd).is_err());
        assert!(
            gate(false, false)
                .before_write(HarnessId::Pi, &cmd)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn an_unwritable_target_is_left_to_the_write() {
        let cmd = session(Some("T"), true);
        for target in [
            HarnessId::Hermes,
            HarnessId::Amp,
            HarnessId::Simple,
            HarnessId::ChatGpt,
        ] {
            assert_eq!(gate(false, true).before_write(target, &cmd).unwrap(), None);
        }
    }

    /// `unwritable` mirrors what `local::write_with` refuses outright; if the
    /// library grows or loses a refusal this fails and the list is updated.
    #[test]
    fn unwritable_agrees_with_the_store_layer() {
        let dir = tempfile::tempdir().unwrap();
        let t = session(Some("T"), false);
        for harness in HarnessId::ALL {
            let written = txcript::local::write_with(
                harness,
                &t,
                txcript::local::WriteOpts {
                    root: Some(dir.path()),
                    metadata: None,
                },
            );
            if unwritable(harness).is_some() {
                assert!(written.is_err(), "{harness} is listed unwritable but wrote");
            }
        }
        for harness in [
            HarnessId::ClaudeCode,
            HarnessId::Codex,
            HarnessId::Pi,
            HarnessId::Campfire,
            HarnessId::Cursor,
            HarnessId::Grok,
            HarnessId::Fx,
            HarnessId::Antigravity,
        ] {
            assert!(unwritable(harness).is_none(), "{harness}");
        }
    }

    #[test]
    fn the_matrix_covers_writable_targets_and_leaves_out_grok_bot() {
        let targets = matrix_targets();
        assert_eq!(targets.len(), 11);
        assert!(!targets.contains(&HarnessId::GrokBot));
        assert!(!targets.contains(&HarnessId::Simple));
        assert!(targets.iter().all(|t| unwritable(*t).is_none()));
    }

    #[test]
    fn shell_words_are_quoted_only_when_needed() {
        assert_eq!(shell_quote("abc123"), "abc123");
        assert_eq!(shell_quote("abc#5-12"), "abc#5-12");
        assert_eq!(shell_quote("/tmp/run.json"), "/tmp/run.json");
        assert_eq!(shell_quote("Fix the bug"), "'Fix the bug'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a;b"), "'a;b'");
    }

    #[test]
    fn fail_on_maps_to_a_threshold() {
        assert_eq!(FailOn::None.threshold(), None);
        assert_eq!(FailOn::Content.threshold(), Some(Severity::Content));
        assert_eq!(FailOn::Accounting.threshold(), Some(Severity::Accounting));
        assert_eq!(FailOn::Metadata.threshold(), Some(Severity::Metadata));
    }

    #[test]
    fn exit_codes_follow_severity_and_errors() {
        let report = |severity| Outcome {
            target: HarnessId::Pi,
            result: Ok(Report {
                target: HarnessId::Pi,
                severity,
                messages_before: 1,
                messages_after: 1,
                items: Vec::new(),
                warnings: Vec::new(),
            }),
        };
        let failed = |outcomes: &[Outcome], at| exit_code(outcomes, at) == ExitCode::FAILURE;
        assert!(!failed(&[report(Severity::Content)], None));
        assert!(failed(
            &[report(Severity::Content)],
            Some(Severity::Content)
        ));
        assert!(!failed(
            &[report(Severity::Accounting)],
            Some(Severity::Content)
        ));
        assert!(failed(
            &[report(Severity::Accounting)],
            Some(Severity::Metadata)
        ));
        assert!(!failed(&[report(Severity::None)], Some(Severity::Metadata)));
        let broken = Outcome {
            target: HarnessId::Hermes,
            result: Err("nope".into()),
        };
        assert!(failed(&[report(Severity::None), broken], None));
    }

    #[test]
    fn phrases_count_and_qualify() {
        assert_eq!(phrase(Category::Usage, 1, None), "1 usage record");
        assert_eq!(phrase(Category::Usage, 40, None), "40 usage records");
        assert_eq!(
            phrase(Category::Usage, 3, Some("cache_read_input_tokens")),
            "3 usage fields (cache_read_input_tokens)"
        );
        assert_eq!(phrase(Category::Title, 1, None), "title");
        assert_eq!(
            phrase(Category::ToolArgs, 2, Some("Bash.description")),
            "2 tool arguments (Bash.description)"
        );
    }

    #[test]
    fn the_summary_is_content_first_and_capped() {
        let item = |category, count| Loss {
            kind: LossKind::Lost,
            category,
            path: None,
            count,
            first_at: None,
            example: None,
        };
        let report = Report {
            target: HarnessId::Pi,
            severity: Severity::Content,
            messages_before: 1,
            messages_after: 1,
            items: vec![
                item(Category::Text, 2),
                item(Category::ToolCall, 1),
                item(Category::Image, 1),
                item(Category::Usage, 5),
                item(Category::StopReason, 1),
                item(Category::MessageModel, 1),
                item(Category::Title, 1),
                item(Category::Model, 1),
            ],
            warnings: Vec::new(),
        };
        assert_eq!(
            summarize(&report, Severity::Metadata).unwrap(),
            "2 text blocks, 1 tool call, 1 image, 5 usage records, 1 stop reason, \
             1 per-message model, and 2 more"
        );
        assert_eq!(
            summarize(&report, Severity::Content).unwrap(),
            "2 text blocks, 1 tool call, 1 image"
        );
        let mut synthesized = report;
        synthesized.items = vec![Loss {
            kind: LossKind::Synthesized,
            ..item(Category::Text, 3)
        }];
        assert_eq!(summarize(&synthesized, Severity::Metadata), None);
    }

    #[test]
    fn the_matrix_has_no_trailing_spaces_and_marks_errors() {
        let source = Resolved {
            harness: HarnessId::Simple,
            common: session(Some("T"), false),
            sliced: false,
        };
        let outcomes = assess_all(&source.common, &[HarnessId::Pi, HarnessId::Hermes], false);
        let text = render_matrix(&source, &outcomes, "abcdef012345");
        assert!(text.starts_with("check abcdef01 (simple, 2 messages), codec-level, 2 targets\n"));
        assert!(text.lines().all(|l| l == l.trim_end()), "{text:?}");
        assert!(
            text.contains("pi              ok       ok          lost"),
            "{text}"
        );
        assert!(
            text.contains("hermes          error: hermes is not writable"),
            "{text}"
        );
    }

    #[test]
    fn json_nulls_the_fields_of_a_target_that_failed() {
        let source = Resolved {
            harness: HarnessId::Simple,
            common: session(None, false),
            sliced: false,
        };
        let outcomes = assess_all(&source.common, &[HarnessId::Amp], false);
        let value: serde_json::Value =
            serde_json::from_str(&render_json(&source, &outcomes).unwrap()).unwrap();
        let target = &value["targets"][0];
        assert_eq!(target["harness"], "amp");
        assert!(target["error"].as_str().unwrap().contains("not writable"));
        for key in ["messages_after", "severity", "counts"] {
            assert!(target[key].is_null(), "{key}");
        }
        assert_eq!(value["schema"], 1);
    }
}
