#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Conversion fidelity (`txcript::fidelity`) measured against every harness
//! codec: a rich canonical session assessed per target with a golden result,
//! determinism, and no-panic sweeps over generated transcripts.
//!
//! The golden table below is the oracle for codec fidelity. A codec change
//! that loses less (or more) shows up as a line to add, change, or delete
//! here, which is the point: update it deliberately, in the same commit as the
//! codec change, and update the table in `docs/formats/README.md` that
//! `docs_table_matches_the_measurement` checks. Only losses and changes are
//! listed (synthesized items and per-message timestamps are not stable enough
//! to pin) and each line is one harness, kind, category and count on the
//! `rich` session.

use chrono::{DateTime, Utc};
use proptest::prelude::*;
use serde_json::json;
use txcript::common::{
    Block, ImageSource, Message, Meta, Role, StopReason, Tool, ToolOutput, Usage,
};
use txcript::fidelity::{self, Category, LossKind, Severity};
use txcript::{Common, HarnessId, Transcript};

fn ts(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
}

// One fixture with every kind of block; length grows with the kinds covered.
#[allow(clippy::too_many_lines)]
fn rich() -> Transcript<Common> {
    let meta = Meta {
        id: "fid-1".into(),
        timestamp: ts(0),
        cwd: Some("/work/repo".into()),
        git_branch: Some("feat/clock".into()),
        title: Some("Fix the flaky clock test".into()),
        cli_version: Some("1.2.3".into()),
        model: Some("claude-opus-4-8".into()),
        lineage: None,
    };
    let model = || Some("claude-opus-4-8".to_string());
    let usage = |n: u64| {
        Some(Usage {
            input_tokens: 1000 + n,
            output_tokens: 100 + n,
            cache_read_input_tokens: Some(900),
            cache_creation_input_tokens: None,
        })
    };
    let user = |blocks: Vec<Block>, t: i64| Message {
        role: Role::User,
        content: blocks,
        timestamp: ts(t),
        model: None,
        stop_reason: None,
        usage: None,
    };
    let assistant = |blocks: Vec<Block>, t: i64, stop: StopReason| Message {
        role: Role::Assistant,
        content: blocks,
        timestamp: ts(t),
        model: model(),
        stop_reason: Some(stop),
        usage: usage(u64::try_from(t).unwrap()),
    };
    let text = |s: &str| Block::Text { text: s.into() };
    let call = |id: &str, tool: Tool| Block::ToolUse {
        id: id.into(),
        tool,
    };
    let result = |id: &str, out: ToolOutput, is_error: bool| Block::ToolResult {
        tool_use_id: id.into(),
        content: out,
        is_error,
    };
    let body = vec![
        user(
            vec![
                text("Fix the flaky test in clock.rs. 日本語のコメントも直して 🚀"),
                Block::Image {
                    source: ImageSource {
                        source_type: "base64".into(),
                        media_type: "image/png".into(),
                        data: "iVBORw0KGgo=".into(),
                    },
                },
            ],
            1,
        ),
        assistant(
            vec![
                Block::Thinking {
                    text: "The test reads the wall clock.".into(),
                    signature: Some("sig-aaa".into()),
                    encrypted: None,
                },
                text("Let me run it first."),
                call(
                    "c1",
                    Tool::Bash {
                        command: "cargo test clock".into(),
                        workdir: None,
                        timeout_ms: None,
                        description: Some("Run the clock tests".into()),
                        run_in_background: false,
                    },
                ),
            ],
            2,
            StopReason::ToolUse,
        ),
        user(
            vec![result(
                "c1",
                ToolOutput::Text("running 3 tests\n3 passed".into()),
                false,
            )],
            3,
        ),
        assistant(
            vec![
                text("Pinning the clock."),
                call(
                    "c2",
                    Tool::Edit {
                        file_path: "/work/repo/src/clock.rs".into(),
                        old_string: "Utc::now()".into(),
                        new_string: "self.now".into(),
                        replace_all: false,
                    },
                ),
            ],
            4,
            StopReason::ToolUse,
        ),
        user(
            vec![result("c2", ToolOutput::Text("patched".into()), false)],
            5,
        ),
        assistant(
            vec![call(
                "c3",
                Tool::Raw {
                    tool_name: "mcp__docs__lookup".into(),
                    input: json!({"q": "clock", "filters": {"b": 1, "a": 2}}),
                },
            )],
            6,
            StopReason::ToolUse,
        ),
        user(
            vec![result(
                "c3",
                ToolOutput::Json(json!([{"title": "Clock"}, {"title": "Time"}])),
                false,
            )],
            7,
        ),
        assistant(
            vec![call(
                "c4",
                Tool::Bash {
                    command: "false".into(),
                    workdir: None,
                    timeout_ms: None,
                    description: None,
                    run_in_background: false,
                },
            )],
            8,
            StopReason::ToolUse,
        ),
        user(vec![result("c4", ToolOutput::Text("boom".into()), true)], 9),
        user(
            vec![call(
                "c5",
                Tool::Command {
                    command: "/release".into(),
                    args: Some("patch".into()),
                },
            )],
            10,
        ),
        user(
            vec![result(
                "c5",
                ToolOutput::Text("released v1.2.3".into()),
                false,
            )],
            11,
        ),
        assistant(
            vec![
                Block::Thinking {
                    text: "Everything passes now.".into(),
                    signature: None,
                    encrypted: None,
                },
                text("All green."),
            ],
            12,
            StopReason::EndTurn,
        ),
        user(vec![text("thanks")], 13),
        assistant(vec![text("Anytime.")], 14, StopReason::Aborted),
    ];
    Transcript::new(meta, body)
}

/// The targets `continue --with` writes, minus the pull-only and mint-only
/// ones: the default targets of a fidelity report.
const WRITABLE: [HarnessId; 11] = [
    HarnessId::ClaudeCode,
    HarnessId::Codex,
    HarnessId::OpenCode,
    HarnessId::Pi,
    HarnessId::Campfire,
    HarnessId::Cursor,
    HarnessId::CursorDesktop,
    HarnessId::Grok,
    HarnessId::Fx,
    HarnessId::Antigravity,
    HarnessId::Cowork,
];

/// One line per harness and severity, then one per loss, sorted.
const GOLDEN: &[&str] = &[
    "amp lost tool_call x1",
    "amp severity content",
    "antigravity changed title x1",
    "antigravity lost message_model x6",
    "antigravity lost model x1",
    "antigravity lost tool_args Bash.description x1",
    "antigravity lost tool_call x1",
    "antigravity lost tool_result x1",
    "antigravity severity content",
    "campfire lost git_branch x1",
    "campfire lost model x1",
    "campfire lost thinking_signature x1",
    "campfire lost title x1",
    "campfire lost tool_call x1",
    "campfire severity content",
    "claude_code changed stop_reason x1",
    "claude_code severity accounting",
    "codex changed tool_name /release -> _release x1",
    "codex lost is_error x1",
    "codex lost model x1",
    "codex lost stop_reason x6",
    "codex lost thinking_signature x1",
    "codex lost title x1",
    "codex lost tool_args Bash.description x1",
    "codex lost usage x2",
    "codex severity content",
    "cowork changed stop_reason x1",
    "cowork severity accounting",
    "cursor lost git_branch x1",
    "cursor lost stop_reason x6",
    "cursor lost tool_call x1",
    "cursor lost usage x6",
    "cursor severity content",
    "cursor_desktop lost git_branch x1",
    "cursor_desktop lost image x1",
    "cursor_desktop lost stop_reason x6",
    "cursor_desktop lost tool_args Bash.description x1",
    "cursor_desktop lost tool_call x1",
    "cursor_desktop lost tool_result x1",
    "cursor_desktop lost usage cache_read_input_tokens x6",
    "cursor_desktop severity content",
    "fx lost git_branch x1",
    "fx lost stop_reason x5",
    "fx lost tool_args Bash.description x1",
    "fx lost tool_call x1",
    "fx lost usage x6",
    "fx severity content",
    "grok lost stop_reason x4",
    "grok lost thinking_signature x1",
    "grok lost tool_call x1",
    "grok lost usage x6",
    "grok severity content",
    "grok_bot changed title x1",
    "grok_bot lost cwd x1",
    "grok_bot lost git_branch x1",
    "grok_bot lost image x1",
    "grok_bot lost message_model x6",
    "grok_bot lost model x1",
    "grok_bot lost stop_reason x6",
    "grok_bot lost usage x6",
    "grok_bot severity content",
    "hermes lost thinking_signature x1",
    "hermes lost tool_call x1",
    "hermes lost usage x6",
    "hermes severity content",
    "opencode changed stop_reason x1",
    "opencode lost git_branch x1",
    "opencode lost thinking_signature x1",
    "opencode lost tool_call x1",
    "opencode lost tool_result x1",
    "opencode severity content",
    "pi lost git_branch x1",
    "pi lost model x1",
    "pi lost thinking_signature x1",
    "pi lost title x1",
    "pi lost tool_call x1",
    "pi severity content",
    "simple severity none",
];

/// The `rich` session assessed for `target`.
fn report(target: HarnessId) -> fidelity::Report {
    fidelity::assess(target, &rich()).unwrap_or_else(|e| panic!("{target}: {e}"))
}

fn golden_lines(target: HarnessId, report: &fidelity::Report) -> Vec<String> {
    let mut lines = vec![format!("{target} severity {}", report.severity.as_str())];
    for item in &report.items {
        if item.kind == LossKind::Synthesized || item.category == Category::Timestamp {
            continue;
        }
        let kind = match item.kind {
            LossKind::Lost => "lost",
            _ => "changed",
        };
        let category = serde_json::to_value(item.category).unwrap();
        let path = item
            .path
            .as_deref()
            .map(|p| format!(" {p}"))
            .unwrap_or_default();
        lines.push(format!(
            "{target} {kind} {}{path} x{}",
            category.as_str().unwrap(),
            item.count
        ));
    }
    lines
}

#[test]
fn every_target_matches_the_golden_measurement() {
    let mut actual: Vec<String> = fidelity::TARGETS
        .into_iter()
        .flat_map(|target| golden_lines(target, &report(target)))
        .collect();
    actual.sort();
    let expected: Vec<String> = GOLDEN.iter().map(ToString::to_string).collect();
    if actual != expected {
        let table = actual
            .iter()
            .map(|l| format!("    \"{l}\","))
            .collect::<Vec<_>>()
            .join("\n");
        panic!(
            "the measured fidelity differs from GOLDEN. If a codec change caused this on \
             purpose, replace the table with:\n{table}"
        );
    }
}

#[test]
fn claude_code_and_simple_lose_no_content() {
    for target in [HarnessId::ClaudeCode, HarnessId::Simple, HarnessId::Cowork] {
        let r = report(target);
        assert!(
            r.severity < Severity::Content,
            "{target} lost content: {:?}",
            r.items
        );
    }
    let simple = report(HarnessId::Simple);
    assert!(simple.items.is_empty(), "{:?}", simple.items);
    assert_eq!(simple.messages_before, simple.messages_after);
}

#[test]
fn measurement_is_deterministic() {
    let src = rich();
    for target in fidelity::TARGETS {
        let first = fidelity::assess(target, &src).unwrap();
        for _ in 0..3 {
            assert_eq!(fidelity::assess(target, &src).unwrap(), first, "{target}");
        }
    }
}

#[test]
fn check_all_reports_every_target_and_never_aborts() {
    let results = fidelity::check_all(&rich());
    let targets: Vec<HarnessId> = results.iter().map(|(t, _)| *t).collect();
    assert_eq!(targets, fidelity::TARGETS);
    for (target, result) in results {
        assert_eq!(result.unwrap().target, target);
    }
}

#[test]
fn read_only_sources_cannot_be_assessed_as_targets() {
    // Not in `TARGETS`, and a direct call fails cleanly instead of panicking.
    for target in [
        HarnessId::ClaudeChat,
        HarnessId::CoworkRemote,
        HarnessId::ChatGpt,
    ] {
        assert!(!fidelity::TARGETS.contains(&target));
        let _ = fidelity::assess(target, &rich());
    }
}

fn status(report: &fidelity::Report, class: Severity) -> &'static str {
    let any = |kind: LossKind| {
        report.items.iter().any(|i| {
            i.kind == kind && i.category.severity() == class && i.category != Category::Timestamp
        })
    };
    if any(LossKind::Lost) {
        "lost"
    } else if any(LossKind::Changed) {
        "changed"
    } else {
        "ok"
    }
}

/// One cell of the docs table: `lost: a, b; changed: c`, or a dash.
fn cell(report: &fidelity::Report, class: Severity) -> String {
    let names = |kind: LossKind| -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for item in report.items.iter().filter(|i| {
            i.kind == kind && i.category.severity() == class && i.category != Category::Timestamp
        }) {
            let name = serde_json::to_value(item.category).unwrap();
            let name = name.as_str().unwrap().to_string();
            if !names.contains(&name) {
                names.push(name);
            }
        }
        names
    };
    let parts: Vec<String> = [(LossKind::Lost, "lost"), (LossKind::Changed, "changed")]
        .into_iter()
        .filter_map(|(kind, label)| {
            let names = names(kind);
            (!names.is_empty()).then(|| format!("{label}: {}", names.join(", ")))
        })
        .collect();
    if parts.is_empty() {
        "ok".to_string()
    } else {
        parts.join("; ")
    }
}

/// The per-harness support table in `docs/formats/README.md` is this
/// measurement, rendered. When it drifts, the failure prints the rows to paste.
#[test]
fn docs_table_matches_the_measurement() {
    let docs = include_str!("../../docs/formats/README.md");
    let rows: Vec<String> = WRITABLE
        .into_iter()
        .map(|target| {
            let r = report(target);
            format!(
                "| {target} | {} | {} | {} |",
                cell(&r, Severity::Content),
                cell(&r, Severity::Accounting),
                cell(&r, Severity::Metadata)
            )
        })
        .collect();
    let missing: Vec<&String> = rows
        .iter()
        .filter(|row| !docs.contains(row.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/formats/README.md is out of date; its table rows should be:\n{}",
        rows.join("\n")
    );
    // The status words the CLI matrix would print agree with the cells.
    for target in WRITABLE {
        let r = report(target);
        for class in [Severity::Content, Severity::Accounting, Severity::Metadata] {
            let word = status(&r, class);
            let c = cell(&r, class);
            assert_eq!(word == "ok", c == "ok", "{target} {class:?}: {word} vs {c}");
        }
    }
}

// ── generated transcripts ──────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Step {
    UserText(String),
    UserImage,
    Reply {
        thinking: Option<(String, Option<String>)>,
        text: String,
        usage: Option<(u64, u64)>,
        stop: Option<StopReason>,
        model: Option<String>,
    },
    Call {
        tool: Tool,
        result: ToolOutput,
        is_error: bool,
    },
    Command {
        name: String,
        args: Option<String>,
        output: String,
    },
}

fn arb_text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => "[a-zA-Z0-9][a-zA-Z0-9 ,.!?<>&'/_-]{0,38}",
        1 => "[a-zA-Z0-9 日本語é🚀\n\t]{1,30}",
        1 => Just("  padded\n".to_string()),
        1 => Just(String::new()),
    ]
}

fn arb_stop() -> impl Strategy<Value = StopReason> {
    prop_oneof![
        Just(StopReason::EndTurn),
        Just(StopReason::ToolUse),
        Just(StopReason::MaxTokens),
        Just(StopReason::StopSequence),
        Just(StopReason::Aborted),
        Just(StopReason::Error),
        "[a-z_]{1,8}".prop_map(StopReason::Other),
    ]
}

fn arb_output() -> impl Strategy<Value = ToolOutput> {
    prop_oneof![
        arb_text().prop_map(ToolOutput::Text),
        arb_text().prop_map(|t| ToolOutput::Json(json!([{"type": "text", "text": t}]))),
        arb_text().prop_map(|t| ToolOutput::Json(json!({"k": t, "n": [1, 2, 3]}))),
    ]
}

fn arb_tool() -> impl Strategy<Value = Tool> {
    prop_oneof![
        (arb_text(), proptest::option::of(arb_text())).prop_map(|(command, description)| {
            Tool::Bash {
                command,
                workdir: None,
                timeout_ms: None,
                description,
                run_in_background: false,
            }
        }),
        ("/repo/[a-z]{1,8}\\.rs", arb_text(), arb_text()).prop_map(
            |(file_path, old_string, new_string)| Tool::Edit {
                file_path,
                old_string,
                new_string,
                replace_all: false,
            }
        ),
        "/repo/[a-z]{1,8}\\.rs".prop_map(|file_path| Tool::Read {
            file_path,
            offset: None,
            limit: None,
        }),
        ("mcp__[a-z]{1,5}__[a-z]{1,6}", arb_text()).prop_map(|(tool_name, q)| Tool::Raw {
            tool_name,
            input: json!({"q": q, "opts": {"b": 1, "a": [true, null]}}),
        }),
    ]
}

fn arb_step() -> impl Strategy<Value = Step> {
    prop_oneof![
        arb_text().prop_map(Step::UserText),
        Just(Step::UserImage),
        (
            proptest::option::of((arb_text(), proptest::option::of("[a-z0-9]{4,10}"))),
            arb_text(),
            proptest::option::of((0u64..5000, 0u64..500)),
            proptest::option::of(arb_stop()),
            proptest::option::of("[a-z0-9-]{3,12}"),
        )
            .prop_map(|(thinking, text, usage, stop, model)| Step::Reply {
                thinking,
                text,
                usage,
                stop,
                model,
            }),
        (arb_tool(), arb_output(), any::<bool>()).prop_map(|(tool, result, is_error)| {
            Step::Call {
                tool,
                result,
                is_error,
            }
        }),
        ("/[a-z]{2,8}", proptest::option::of(arb_text()), arb_text())
            .prop_map(|(name, args, output)| Step::Command { name, args, output }),
    ]
}

fn message(role: Role, content: Vec<Block>, n: usize) -> Message {
    Message {
        role,
        content,
        timestamp: ts(i64::try_from(n).unwrap() + 1),
        model: None,
        stop_reason: None,
        usage: None,
    }
}

fn build(steps: &[Step]) -> Transcript<Common> {
    let mut t = rich();
    t.body.clear();
    for (i, step) in steps.iter().enumerate() {
        let id = format!("c{i}");
        let n = t.body.len();
        match step {
            Step::UserText(text) => t.body.push(message(
                Role::User,
                vec![Block::Text { text: text.clone() }],
                n,
            )),
            Step::UserImage => t.body.push(message(
                Role::User,
                vec![Block::Image {
                    source: ImageSource {
                        source_type: "base64".into(),
                        media_type: "image/png".into(),
                        data: "iVBORw0KGgo=".into(),
                    },
                }],
                n,
            )),
            Step::Reply {
                thinking,
                text,
                usage,
                stop,
                model,
            } => {
                let mut content = Vec::new();
                if let Some((text, signature)) = thinking {
                    content.push(Block::Thinking {
                        text: text.clone(),
                        signature: signature.clone(),
                        encrypted: None,
                    });
                }
                content.push(Block::Text { text: text.clone() });
                let mut reply = message(Role::Assistant, content, n);
                reply.model.clone_from(model);
                reply.stop_reason.clone_from(stop);
                reply.usage = usage.map(|(input_tokens, output_tokens)| Usage {
                    input_tokens,
                    output_tokens,
                    cache_read_input_tokens: None,
                    cache_creation_input_tokens: None,
                });
                t.body.push(reply);
            }
            Step::Call {
                tool,
                result,
                is_error,
            } => {
                t.body.push(message(
                    Role::Assistant,
                    vec![Block::ToolUse {
                        id: id.clone(),
                        tool: tool.clone(),
                    }],
                    n,
                ));
                t.body.push(message(
                    Role::User,
                    vec![Block::ToolResult {
                        tool_use_id: id,
                        content: result.clone(),
                        is_error: *is_error,
                    }],
                    n + 1,
                ));
            }
            Step::Command { name, args, output } => {
                t.body.push(message(
                    Role::User,
                    vec![Block::ToolUse {
                        id: id.clone(),
                        tool: Tool::Command {
                            command: name.clone(),
                            args: args.clone(),
                        },
                    }],
                    n,
                ));
                t.body.push(message(
                    Role::User,
                    vec![Block::ToolResult {
                        tool_use_id: id,
                        content: ToolOutput::Text(output.clone()),
                        is_error: false,
                    }],
                    n + 1,
                ));
            }
        }
    }
    t
}

proptest! {
    /// Whatever the transcript, assessing it never panics for any target,
    /// repeats exactly, and measures the lossless Simple codec as lossless:
    /// a false positive from the comparator shows up here.
    #[test]
    fn assessing_generated_transcripts_is_total_and_stable(
        steps in proptest::collection::vec(arb_step(), 0..8)
    ) {
        let src = build(&steps);
        for target in fidelity::TARGETS {
            let first = fidelity::assess(target, &src);
            let second = fidelity::assess(target, &src);
            match (first, second) {
                (Ok(a), Ok(b)) => {
                    prop_assert_eq!(&a, &b);
                    prop_assert_eq!(a.messages_before, src.body.len());
                    if target == HarnessId::Simple {
                        prop_assert!(a.items.is_empty(), "simple is lossless: {:?}", a.items);
                    }
                }
                (Err(a), Err(b)) => prop_assert_eq!(a.to_string(), b.to_string()),
                _ => prop_assert!(false, "{target}: flaky result"),
            }
        }
    }
}
