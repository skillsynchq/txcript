#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! `txcript check` and the conversion-loss gate on `continue`, through the
//! real binary. Every store environment variable points into a temp dir, so no
//! real history is read or written, and sources are named with `--from` or
//! passed as Simple documents. The prediction-vs-reality tests are the ones
//! that matter most: they convert for real, read the copy back, and hold the
//! report to what actually changed.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use txcript::HarnessId;
use txcript::common::{
    Block, ImageSource, Message, Meta, Role, StopReason, Tool, ToolOutput, Usage,
};

struct Env {
    home: tempfile::TempDir,
}

/// Every variable the stores read, so nothing outside the temp dir is touched.
const STORE_VARS: [&str; 15] = [
    "CODEX_HOME",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "PI_CODING_AGENT_DIR",
    "OPENCODE_DB",
    "HERMES_HOME",
    "FX_HOME",
    "GROK_HOME",
    "COWORK_SESSIONS_DIR",
    "AMP_THREADS_DIR",
    "CURSOR_DESKTOP_USER_DIR",
    "TXCRIPT_GROK_BOT_ROOT",
    "GROK_BOT_TRANSCRIPTS",
    "TXCRIPT_GROK_BOT_AGENTS",
];

impl Env {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self) -> &Path {
        self.home.path()
    }

    /// A project directory that exists, so the copy keeps its recorded cwd.
    fn project(&self) -> String {
        let dir = self.path().join("project");
        fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn command(&self) -> Command {
        let empty = self.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_txcript"));
        cmd.env_clear()
            .env("HOME", self.path())
            .env("USERPROFILE", self.path())
            .env("CLAUDE_CONFIG_DIR", self.path().join(".claude"))
            .env("NO_COLOR", "1")
            .current_dir(self.path());
        for var in STORE_VARS {
            cmd.env(var, &empty);
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn run_stdin(&self, args: &[&str], stdin: &str) -> Output {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    /// Write a Simple document and return its path.
    fn document(&self, name: &str, text: &str) -> String {
        let path = self.path().join(name);
        fs::write(&path, text).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// Every file under the temp dir, with its size, excluding the inputs.
    fn snapshot(&self) -> BTreeMap<PathBuf, u64> {
        fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, u64>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.insert(path.clone(), entry.metadata().map_or(0, |m| m.len()));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(self.path(), &mut out);
        out
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", stdout(out)))
}

fn ts(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
}

fn message(role: Role, content: Vec<Block>, secs: i64) -> Message {
    Message {
        role,
        content,
        timestamp: ts(secs),
        model: None,
        stop_reason: None,
        usage: None,
    }
}

fn text(s: &str) -> Block {
    Block::Text { text: s.into() }
}

fn assistant(content: Vec<Block>, secs: i64, stop: StopReason) -> Message {
    Message {
        model: Some("claude-opus-4-8".into()),
        stop_reason: Some(stop),
        usage: Some(Usage {
            input_tokens: 1000 + u64::try_from(secs).unwrap(),
            output_tokens: 100,
            cache_read_input_tokens: Some(900),
            cache_creation_input_tokens: None,
        }),
        ..message(Role::Assistant, content, secs)
    }
}

fn result(id: &str, out: ToolOutput, is_error: bool) -> Block {
    Block::ToolResult {
        tool_use_id: id.into(),
        content: out,
        is_error,
    }
}

/// A session with every kind of thing a conversion can lose: signed thinking,
/// a Bash description, an MCP call with a structured result, an error result,
/// a slash command, an image, usage, stop reasons, a title, a branch.
// One session with every kind of thing a conversion can lose; length grows
// with the kinds covered.
#[allow(clippy::too_many_lines)]
fn rich(cwd: &str) -> txcript::Transcript<txcript::Common> {
    let meta = Meta {
        id: "11111111-2222-4333-8444-555555555555".into(),
        timestamp: ts(0),
        cwd: Some(cwd.into()),
        git_branch: Some("feat/clock".into()),
        title: Some("Fix the flaky clock test".into()),
        cli_version: None,
        model: Some("claude-opus-4-8".into()),
        lineage: None,
    };
    let body = vec![
        message(
            Role::User,
            vec![
                text("Fix the flaky test in clock.rs 🚀"),
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
                    text: "It reads the wall clock.".into(),
                    signature: Some("sig-aaa".into()),
                    encrypted: None,
                },
                text("Running it first."),
                Block::ToolUse {
                    id: "c1".into(),
                    tool: Tool::Bash {
                        command: "cargo test clock".into(),
                        workdir: None,
                        timeout_ms: None,
                        description: Some("Run the clock tests".into()),
                        run_in_background: false,
                    },
                },
            ],
            2,
            StopReason::ToolUse,
        ),
        message(
            Role::User,
            vec![result("c1", ToolOutput::Text("3 passed".into()), false)],
            3,
        ),
        assistant(
            vec![Block::ToolUse {
                id: "c2".into(),
                tool: Tool::Raw {
                    tool_name: "mcp__docs__lookup".into(),
                    input: json!({"q": "clock", "filters": {"b": 1, "a": 2}}),
                },
            }],
            4,
            StopReason::ToolUse,
        ),
        message(
            Role::User,
            vec![result(
                "c2",
                ToolOutput::Json(json!([{"title": "Clock"}, {"title": "Time"}])),
                false,
            )],
            5,
        ),
        assistant(
            vec![Block::ToolUse {
                id: "c3".into(),
                tool: Tool::Bash {
                    command: "false".into(),
                    workdir: None,
                    timeout_ms: None,
                    description: None,
                    run_in_background: false,
                },
            }],
            6,
            StopReason::ToolUse,
        ),
        message(
            Role::User,
            vec![result("c3", ToolOutput::Text("boom".into()), true)],
            7,
        ),
        message(
            Role::User,
            vec![Block::ToolUse {
                id: "c4".into(),
                tool: Tool::Command {
                    command: "/release".into(),
                    args: Some("patch".into()),
                },
            }],
            8,
        ),
        message(
            Role::User,
            vec![result(
                "c4",
                ToolOutput::Text("released v1.2.3".into()),
                false,
            )],
            9,
        ),
        assistant(vec![text("All green.")], 10, StopReason::EndTurn),
    ];
    txcript::Transcript::new(meta, body)
}

/// Text only: nothing for a conversion to lose in the formats that keep text.
fn plain(cwd: &str) -> txcript::Transcript<txcript::Common> {
    let meta = Meta {
        id: "99999999-2222-4333-8444-555555555555".into(),
        timestamp: ts(0),
        cwd: Some(cwd.into()),
        git_branch: None,
        title: None,
        cli_version: None,
        model: None,
        lineage: None,
    };
    let body = vec![
        message(Role::User, vec![text("hello there")], 1),
        message(Role::Assistant, vec![text("hi")], 2),
    ];
    txcript::Transcript::new(meta, body)
}

fn simple_text(t: &txcript::Transcript<txcript::Common>) -> String {
    txcript::dispatch::render_from_common(HarnessId::Simple, t).unwrap()
}

fn rich_doc(env: &Env) -> String {
    env.document("rich.json", &simple_text(&rich(&env.project())))
}

fn plain_doc(env: &Env) -> String {
    env.document("plain.json", &simple_text(&plain(&env.project())))
}

fn target<'a>(doc: &'a Value, harness: &str) -> &'a Value {
    doc["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["harness"] == harness)
        .unwrap_or_else(|| panic!("no {harness} target in {doc}"))
}

fn has_item(target: &Value, kind: &str, category: &str, path: Option<&str>) -> bool {
    target["items"].as_array().unwrap().iter().any(|i| {
        i["kind"] == kind && i["category"] == category && path.is_none_or(|p| i["path"] == p)
    })
}

// ── check ──────────────────────────────────────────────────────────────

#[test]
fn check_with_a_target_prints_a_report() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let out = env.run(&["check", &doc, "--with", "codex"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with("check 11111111 (simple, 10 messages) -> codex\n"),
        "{text}"
    );
    assert!(text.contains("CHANGED  1 tool name renamed  /release -> _release"));
    assert!(
        text.contains("LOST     1 tool argument (Bash.description)"),
        "{text}"
    );
    assert!(text.contains("LOST     1 error flag"), "{text}");
    assert!(text.contains("LOST     1 thinking signature"), "{text}");
    assert!(
        text.contains("LOST     title  Fix the flaky clock test"),
        "{text}"
    );
    assert!(text.contains("LOST     model  claude-opus-4-8"), "{text}");
    assert!(text.contains("structure"), "{text}");
    assert!(text.contains("codec-level"), "{text}");
    // Every class is labelled once, in order.
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle}: {text}"))
    };
    assert!(at("  content ") < at("  accounting ") && at("  accounting ") < at("  metadata "));
    // Privacy: no tool output body, and no image data.
    assert!(
        !text.contains("3 passed") && !text.contains("iVBOR"),
        "{text}"
    );
}

#[test]
fn check_json_is_versioned_and_complete() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let out = env.run(&["check", &doc, "--with", "codex", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let value = json(&out);
    assert_eq!(value["schema"], 1);
    assert_eq!(value["source"]["harness"], "simple");
    assert_eq!(
        value["source"]["id"],
        "11111111-2222-4333-8444-555555555555"
    );
    assert_eq!(value["source"]["messages"], 10);
    let targets = value["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 1);
    let codex = &targets[0];
    assert_eq!(codex["harness"], "codex");
    assert_eq!(codex["error"], Value::Null);
    assert_eq!(codex["severity"], "content");
    assert!(codex["messages_after"].as_u64().unwrap() >= 10);
    assert!(has_item(
        codex,
        "changed",
        "tool_name",
        Some("/release -> _release")
    ));
    assert!(has_item(
        codex,
        "lost",
        "tool_args",
        Some("Bash.description")
    ));
    assert!(has_item(codex, "lost", "title", None));
    let counts = &codex["counts"];
    assert!(counts["lost"].as_u64().unwrap() >= 5, "{counts}");
    assert_eq!(counts["changed"], 1);
    for item in codex["items"].as_array().unwrap() {
        for key in [
            "kind",
            "category",
            "path",
            "count",
            "first_message",
            "example",
        ] {
            assert!(item.get(key).is_some(), "{key} missing from {item}");
        }
        assert!(item["count"].as_u64().unwrap() >= 1);
        // Examples stay short.
        assert!(
            item["example"]
                .as_str()
                .is_none_or(|e| e.chars().count() <= 80)
        );
    }
    // 1-based, like `view` and `#range`.
    let rename = codex["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["category"] == "tool_name")
        .unwrap();
    assert_eq!(rename["first_message"], 8);
}

#[test]
fn bare_check_compares_every_writable_target() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let out = env.run(&["check", &doc]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("codec-level, 11 targets"), "{text}");
    assert!(text.contains("target          content  accounting  metadata  notes"));
    let rows: Vec<&str> = text
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    for harness in [
        "claude_code",
        "codex",
        "opencode",
        "pi",
        "campfire",
        "cursor",
        "cursor_desktop",
        "grok",
        "fx",
        "antigravity",
        "cowork",
    ] {
        assert!(rows.contains(&harness), "{harness} missing from {text}");
    }
    for harness in [
        "grok_bot",
        "hermes",
        "amp",
        "claude_chat",
        "chatgpt",
        "simple",
    ] {
        assert!(!rows.contains(&harness), "{harness} listed in {text}");
    }
    let codex = text.lines().find(|l| l.starts_with("codex ")).unwrap();
    let cells: Vec<&str> = codex.split_whitespace().collect();
    assert_eq!(&cells[1..4], ["lost", "lost", "lost"], "{codex}");
    assert!(codex.contains("messages 10->"), "{codex}");
    assert!(text.contains("for details"), "{text}");
    assert!(
        text.lines().all(|l| l == l.trim_end()),
        "trailing whitespace in {text:?}"
    );
}

#[test]
fn bare_check_json_lists_the_same_targets() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let out = env.run(&["check", &doc, "--json"]);
    let value = json(&out);
    let names: Vec<&str> = value["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["harness"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 11, "{names:?}");
    assert!(!names.contains(&"grok_bot"));
    assert_eq!(target(&value, "claude_code")["severity"], "none");
    assert_eq!(target(&value, "pi")["severity"], "content");
}

#[test]
fn grok_bot_is_assessed_when_named() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let out = env.run(&["check", &doc, "--with", "grok_bot", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(target(&json(&out), "grok_bot")["error"], Value::Null);
}

#[test]
fn unwritable_targets_report_per_target_without_aborting_the_rest() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let out = env.run(&[
        "check", &doc, "--with", "hermes", "--with", "codex", "--with", "amp", "--json",
    ]);
    // A target that cannot be assessed fails the command, but not the report.
    assert_eq!(out.status.code(), Some(1));
    let value = json(&out);
    let hermes = target(&value, "hermes");
    assert!(
        hermes["error"].as_str().unwrap().contains("not writable"),
        "{hermes}"
    );
    assert_eq!(hermes["items"].as_array().unwrap().len(), 0);
    assert_eq!(hermes["severity"], Value::Null);
    assert!(target(&value, "amp")["error"].is_string());
    let codex = target(&value, "codex");
    assert_eq!(codex["error"], Value::Null);
    assert_eq!(codex["severity"], "content");

    let human = env.run(&["check", &doc, "--with", "hermes", "--with", "codex"]);
    assert_eq!(human.status.code(), Some(1));
    let text = stdout(&human);
    assert!(text.contains("hermes is not writable"), "{text}");
    assert!(text.contains("-> codex"), "{text}");
}

#[test]
fn fail_on_sets_the_exit_code_from_the_severity() {
    let env = Env::new();
    let rich_doc = rich_doc(&env);
    let plain_doc = plain_doc(&env);
    // (document, target, severity) and what each threshold does with it.
    // codex on the rich session is content; claude_code is accounting only (a
    // stop reason's representation); pi on the plain session with a title is
    // metadata; claude_code on the plain one is lossless.
    let titled = env.document(
        "titled.json",
        &simple_text(&{
            let mut t = plain(&env.project());
            t.meta.title = Some("A title".into());
            t
        }),
    );
    let aborted = env.document(
        "aborted.json",
        &simple_text(&{
            let mut t = plain(&env.project());
            t.body[1].stop_reason = Some(StopReason::Aborted);
            t
        }),
    );
    let cases: [(&str, &str, [i32; 4]); 5] = [
        (&rich_doc, "codex", [0, 1, 1, 1]),
        (&aborted, "claude_code", [0, 0, 1, 1]),
        (&titled, "pi", [0, 0, 0, 1]),
        (&plain_doc, "claude_code", [0, 0, 0, 0]),
        (&rich_doc, "claude_code", [0, 0, 0, 0]),
    ];
    for (doc, harness, expected) in cases {
        for (threshold, want) in ["none", "content", "accounting", "metadata"]
            .into_iter()
            .zip(expected)
        {
            let out = env.run(&["check", doc, "--with", harness, "--fail-on", threshold]);
            assert_eq!(
                out.status.code(),
                Some(want),
                "{harness} --fail-on {threshold}: {}{}",
                stdout(&out),
                stderr(&out)
            );
            // The report prints either way.
            assert!(stdout(&out).contains(&format!("-> {harness}")));
        }
    }
    // Matrix mode: the worst of all targets decides.
    let out = env.run(&["check", &rich_doc, "--fail-on", "content"]);
    assert_eq!(out.status.code(), Some(1));
    let out = env.run(&["check", &rich_doc]);
    assert_eq!(out.status.code(), Some(0));
    let out = env.run(&["check", &rich_doc, "--fail-on", "bogus"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn a_lossless_conversion_says_so() {
    let env = Env::new();
    let doc = plain_doc(&env);
    let out = env.run(&["check", &doc, "--with", "claude_code"]);
    let text = stdout(&out);
    assert!(text.contains("content    ok: text, thinking"), "{text}");
    assert!(text.contains("accounting ok:"), "{text}");
    assert!(text.contains("metadata   ok:"), "{text}");
    assert!(
        !text.contains("LOST") && !text.contains("CHANGED"),
        "{text}"
    );
}

#[test]
fn detail_adds_message_numbers_and_longer_examples() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let plain_out = stdout(&env.run(&["check", &doc, "--with", "codex"]));
    assert!(!plain_out.contains("first at message"));
    let detailed = stdout(&env.run(&["check", &doc, "--with", "codex", "--detail"]));
    assert!(detailed.contains("(first at message 8)"), "{detailed}");
}

#[test]
fn help_explains_the_privacy_and_the_codec_level_caveat() {
    let env = Env::new();
    let out = env.run(&["check", "--help"]);
    // Help wraps to the terminal width; compare the words.
    let text = stdout(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.contains("codec-level"), "{text}");
    assert!(text.contains("never a tool result or image"), "{text}");
    assert!(text.contains("--detail"));
    assert!(text.contains("--fail-on"));
    let long = stdout(&env.run(&["continue", "--help"]));
    for flag in ["--quiet", "--strict", "--dry-run"] {
        assert!(long.contains(flag), "{flag} missing from continue --help");
    }
    let crop = stdout(&env.run(&["crop", "--help"]));
    assert!(crop.contains("--quiet") && crop.contains("--strict"));
}

// ── sources ────────────────────────────────────────────────────────────

#[test]
fn check_reads_a_simple_document_from_stdin() {
    let env = Env::new();
    let text = simple_text(&rich(&env.project()));
    let out = env.run_stdin(&["check", "-", "--with", "codex", "--json"], &text);
    assert!(out.status.success(), "{}", stderr(&out));
    let value = json(&out);
    assert_eq!(value["source"]["messages"], 10);
    assert!(has_item(
        target(&value, "codex"),
        "changed",
        "tool_name",
        None
    ));
}

#[test]
fn a_range_reports_exactly_what_would_be_written() {
    let env = Env::new();
    let doc = rich_doc(&env);
    // Messages 1-3: no slash command, so no tool rename.
    let out = env.run(&["check", &format!("{doc}#1-3"), "--with", "codex", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let value = json(&out);
    assert_eq!(value["source"]["messages"], 3);
    assert!(!has_item(
        target(&value, "codex"),
        "changed",
        "tool_name",
        None
    ));
    // The slash command alone is message 8.
    let out = env.run(&["check", &format!("{doc}#8-9"), "--with", "codex", "--json"]);
    let value = json(&out);
    assert_eq!(value["source"]["messages"], 2);
    assert!(has_item(
        target(&value, "codex"),
        "changed",
        "tool_name",
        None
    ));
    // A range that cuts a call from its result fails as `continue` does.
    let out = env.run(&["check", &format!("{doc}#2"), "--with", "codex"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("cuts a tool call away from its result"),
        "{}",
        stderr(&out)
    );
    let out = env.run(&["check", &format!("{doc}#40"), "--with", "codex"]);
    assert!(stderr(&out).contains("out of bounds"), "{}", stderr(&out));
}

#[test]
fn a_missing_source_is_an_error_like_continue() {
    let env = Env::new();
    let out = env.run(&["check", "nothing-here", "--from", "codex"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "error: no codex session matches `nothing-here` (try `txcript list`)\n"
    );
    let doc = rich_doc(&env);
    let out = env.run(&["check", &doc, "--from", "codex"]);
    assert!(stderr(&out).contains("--from scopes the search for a local session"));
}

#[test]
fn an_empty_session_warns() {
    let env = Env::new();
    let mut t = plain(&env.project());
    t.body.clear();
    let doc = env.document("empty.json", &simple_text(&t));
    let out = env.run(&["check", &doc, "--with", "claude_code"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("0 messages"), "{}", stdout(&out));
    let out = env.run(&["check", &doc, "--json"]);
    assert!(out.status.success());
    assert_eq!(json(&out)["source"]["messages"], 0);
}

#[test]
fn a_stored_session_is_checked_by_id() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let written = env.run(&[
        "continue",
        &doc,
        "--with",
        "claude_code",
        "--no-resume",
        "--quiet",
    ]);
    assert!(written.status.success(), "{}", stderr(&written));
    let out = env.run(&[
        "check",
        "11111111",
        "--from",
        "claude_code",
        "--with",
        "pi",
        "--json",
    ]);
    // The store re-ids the copy, so look it up by prefix of what was written.
    let listed = stdout(&env.run(&["list", "--from", "claude_code"]));
    let id = listed
        .split_whitespace()
        .find(|w| w.len() == 36 && w.matches('-').count() == 4)
        .unwrap()
        .to_string();
    let out2 = env.run(&[
        "check",
        &id[..8],
        "--from",
        "claude_code",
        "--with",
        "pi",
        "--json",
    ]);
    assert!(out2.status.success(), "{}{}", stderr(&out), stderr(&out2));
    let value = json(&out2);
    assert_eq!(value["source"]["harness"], "claude_code");
    assert!(has_item(target(&value, "pi"), "lost", "title", None));
}

// ── nothing is written ─────────────────────────────────────────────────

#[test]
fn check_and_dry_run_write_nothing() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let before = env.snapshot();
    for args in [
        vec!["check", &doc],
        vec!["check", &doc, "--with", "codex", "--detail"],
        vec!["check", &doc, "--json"],
        vec!["continue", &doc, "--with", "codex", "--dry-run"],
        vec![
            "continue",
            &doc,
            "--with",
            "cursor",
            "--dry-run",
            "--strict",
        ],
        vec![
            "continue",
            &doc,
            "--with",
            "pi",
            "--dry-run",
            "--out",
            "out-dir",
        ],
    ] {
        let out = env.run(&args);
        assert!(
            out.status.success() || args.contains(&"--strict"),
            "{args:?}: {}",
            stderr(&out)
        );
        assert_eq!(env.snapshot(), before, "{args:?} wrote something");
    }
    assert!(!env.path().join("out-dir").exists());
}

#[test]
fn dry_run_is_check_for_the_same_target() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let check = env.run(&["check", &doc, "--with", "codex"]);
    let dry = env.run(&["continue", &doc, "--with", "codex", "--dry-run"]);
    assert_eq!(stdout(&check), stdout(&dry));
    assert!(dry.status.success());
    assert_eq!(stderr(&dry), "");
    // `--strict` is `--fail-on content`.
    let strict = env.run(&["continue", &doc, "--with", "codex", "--dry-run", "--strict"]);
    assert_eq!(strict.status.code(), Some(1));
    assert_eq!(stdout(&strict), stdout(&check));
    let lossless = env.run(&[
        "continue",
        &plain_doc(&env),
        "--with",
        "claude_code",
        "--dry-run",
        "--strict",
    ]);
    assert!(lossless.status.success());
    // A document has no harness of its own, so the target is required.
    let out = env.run(&["continue", &doc, "--dry-run"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("pass --with <harness>"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn dry_run_of_an_in_place_resume_says_nothing_is_converted() {
    let env = Env::new();
    let doc = plain_doc(&env);
    let written = env.run(&[
        "continue",
        &doc,
        "--with",
        "claude_code",
        "--no-resume",
        "--quiet",
    ]);
    assert!(written.status.success(), "{}", stderr(&written));
    let listed = stdout(&env.run(&["list", "--from", "claude_code"]));
    let id = listed
        .split_whitespace()
        .find(|w| w.len() == 36 && w.matches('-').count() == 4)
        .unwrap();
    let before = env.snapshot();
    let out = env.run(&["continue", id, "--from", "claude_code", "--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("resumes the session in place; nothing is converted"));
    // With a range the copy is rewritten, so there is something to report.
    let out = env.run(&[
        "continue",
        &format!("{id}#1-2"),
        "--from",
        "claude_code",
        "--dry-run",
    ]);
    assert!(stdout(&out).contains("-> claude_code"), "{}", stdout(&out));
    assert_eq!(env.snapshot(), before);
}

// ── the gate on continue ───────────────────────────────────────────────

/// `continue --with codex --no-resume` on `doc`: output with the per-run ids
/// and paths smoothed out.
fn normalized(out: &Output) -> String {
    stdout(out)
        .lines()
        .map(|l| {
            if let Some(rest) = l.strip_prefix("simple → codex  ") {
                assert!(
                    Path::new(rest).extension().is_some_and(|e| e == "jsonl")
                        && rest.contains("rollout-"),
                    "unexpected location in {l}"
                );
                "simple → codex  <path>".to_string()
            } else if l.starts_with("  resume with: codex resume ") {
                "  resume with: codex resume <id>".to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn continue_notes_the_loss_once_on_stderr_and_leaves_stdout_alone() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let noisy = env.run(&["continue", &doc, "--with", "codex", "--no-resume"]);
    assert!(noisy.status.success(), "{}", stderr(&noisy));
    let err = stderr(&noisy);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.starts_with("note: lossy conversion: "), "{err}");
    // Content first; what does not fit is counted.
    assert!(
        err.contains("1 tool name renamed, 1 tool argument"),
        "{err}"
    );
    assert!(err.contains(", and 2 more; run "), "{err}");
    assert!(
        err.trim_end().ends_with(&format!(
            "run 'txcript check {doc} --with codex' for details"
        )),
        "{err}"
    );

    // `--quiet` removes the note and nothing else: stdout is byte-for-byte the
    // pre-existing output format either way.
    let quiet = env.run(&[
        "continue",
        &doc,
        "--with",
        "codex",
        "--no-resume",
        "--quiet",
    ]);
    assert!(quiet.status.success());
    assert_eq!(stderr(&quiet), "");
    assert_eq!(normalized(&noisy), normalized(&quiet));
    assert_eq!(
        normalized(&noisy),
        "simple → codex  <path>\n  resume with: codex resume <id>"
    );
    // The note is for stderr only, and appears whether or not stderr is a
    // terminal (it never is here).
    assert!(!stdout(&noisy).contains("lossy"));
}

#[test]
fn the_note_names_only_the_chosen_target() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let out = env.run(&["continue", &doc, "--with", "fx", "--no-resume"]);
    let err = stderr(&out);
    assert!(err.contains("--with fx"), "{err}");
    assert!(!err.contains("codex"), "{err}");
}

#[test]
fn a_lossless_conversion_prints_no_note() {
    let env = Env::new();
    let doc = plain_doc(&env);
    let out = env.run(&["continue", &doc, "--with", "claude_code", "--no-resume"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
}

#[test]
fn strict_refuses_before_writing_anything() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let before = env.snapshot();
    let out = env.run(&[
        "continue",
        &doc,
        "--with",
        "codex",
        "--no-resume",
        "--strict",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "", "nothing may be reported as written");
    let err = stderr(&out);
    assert!(
        err.starts_with("error: refusing to convert to codex with --strict"),
        "{err}"
    );
    assert!(err.contains("nothing was written"), "{err}");
    assert!(err.contains("tool name"), "{err}");
    assert!(
        err.contains(&format!("run 'txcript check {doc} --with codex'")),
        "{err}"
    );
    assert_eq!(env.snapshot(), before, "strict wrote something");
    // `--out` is a write too.
    let out = env.run(&[
        "continue", &doc, "--with", "codex", "--out", "exported", "--strict",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(env.snapshot(), before);
    assert!(!env.path().join("exported").exists());
}

#[test]
fn strict_lets_through_what_only_loses_accounting_or_metadata() {
    let env = Env::new();
    let doc = plain_doc(&env);
    let mut t = plain(&env.project());
    t.meta.title = Some("A title".into());
    let titled = env.document("titled.json", &simple_text(&t));
    let out = env.run(&[
        "continue",
        &titled,
        "--with",
        "pi",
        "--no-resume",
        "--strict",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("lossy conversion: title"),
        "{}",
        stderr(&out)
    );
    let out = env.run(&[
        "continue",
        &doc,
        "--with",
        "claude_code",
        "--no-resume",
        "--strict",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn strict_wins_over_quiet() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let out = env.run(&[
        "continue",
        &doc,
        "--with",
        "codex",
        "--no-resume",
        "--strict",
        "--quiet",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("refusing to convert"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn an_unwritable_target_keeps_its_existing_error() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let out = env.run(&["continue", &doc, "--with", "hermes", "--no-resume"]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.contains("Hermes state.db is read-only"), "{err}");
    assert!(!err.contains("lossy"), "{err}");
}

#[test]
fn a_ranged_continue_is_measured_on_the_range() {
    let env = Env::new();
    let doc = rich_doc(&env);
    // The slash command is outside messages 1-3, so strict passes there.
    let out = env.run(&[
        "continue",
        &format!("{doc}#1-3"),
        "--with",
        "pi",
        "--no-resume",
        "--quiet",
        "--strict",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn a_broken_pipe_is_not_a_panic() {
    let env = Env::new();
    let doc = rich_doc(&env);
    let mut child = env
        .command()
        .args(["check", &doc, "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(!stderr(&out).contains("panicked"), "{}", stderr(&out));
}

// ── prediction against reality ─────────────────────────────────────────

/// What a session has, counted the way a reader of the exported document
/// would count it. Independent of the comparator under test.
fn facets(doc: &Value) -> BTreeMap<String, i64> {
    let mut f: BTreeMap<String, i64> = BTreeMap::new();
    let mut add = |key: &str| *f.entry(key.to_string()).or_insert(0) += 1;
    for message in doc["messages"].as_array().unwrap() {
        for key in ["usage", "stop_reason", "model"] {
            if message.get(key).is_some_and(|v| !v.is_null()) {
                add(if key == "model" { "message_model" } else { key });
            }
        }
        let Some(blocks) = message["content"].as_array() else {
            continue;
        };
        for block in blocks {
            match block["type"].as_str().unwrap() {
                "thinking" if block.get("signature").is_some_and(|s| !s.is_null()) => {
                    add("thinking_signature");
                }
                "tool_use" => {
                    if block["input"].get("description").is_some() {
                        add("tool_description");
                    }
                    add(&format!("tool:{}", block["name"].as_str().unwrap()));
                }
                "tool_result" if block["is_error"] == true => add("is_error"),
                "image" => add("image"),
                _ => {}
            }
        }
    }
    for key in ["title", "model", "git_branch"] {
        if doc.get(key).is_some_and(|v| !v.is_null()) {
            add(&format!("meta:{key}"));
        }
    }
    f
}

/// Session ids the store lists for a harness.
fn ids(env: &Env, harness: &str) -> Vec<String> {
    stdout(&env.run(&["list", "--from", harness]))
        .split_whitespace()
        .filter(|w| w.len() == 36 && w.matches('-').count() == 4)
        .map(String::from)
        .collect()
}

/// Accounting, thinking, and metadata counts that fell between two sessions.
fn really_lost(was: &BTreeMap<String, i64>, now: &BTreeMap<String, i64>) -> BTreeMap<String, i64> {
    let mut real = BTreeMap::new();
    for (key, count) in was {
        let kept = now.get(key).copied().unwrap_or(0);
        if !key.starts_with("tool:") && *count > kept {
            real.insert(key.clone(), count - kept);
        }
    }
    real
}

/// The same counts, as the report claims them lost.
fn claimed_lost(predicted: &Value) -> BTreeMap<String, i64> {
    let mut claimed = BTreeMap::new();
    for item in predicted["items"].as_array().unwrap() {
        if item["kind"] != "lost" {
            continue;
        }
        let count = i64::try_from(item["count"].as_u64().unwrap()).unwrap();
        let category = item["category"].as_str().unwrap();
        let key = match (category, item["path"].as_str()) {
            ("usage", None) => "usage",
            ("stop_reason", _) => "stop_reason",
            ("message_model", _) => "message_model",
            ("thinking_signature", _) => "thinking_signature",
            ("is_error", _) => "is_error",
            ("image", _) => "image",
            ("tool_args", Some("Bash.description")) => "tool_description",
            ("title", _) => "meta:title",
            ("model", _) => "meta:model",
            ("git_branch", _) => "meta:git_branch",
            _ => continue,
        };
        *claimed.entry(key.to_string()).or_insert(0) += count;
    }
    claimed
}

/// Convert for real into `harness`, read the copy back as a Simple document,
/// and hold what `check` predicted against what changed.
fn assert_prediction_matches_reality(harness: &str) {
    let env = Env::new();
    let doc = rich_doc(&env);
    let source: Value = serde_json::from_str(&fs::read_to_string(&doc).unwrap()).unwrap();

    let report = json(&env.run(&["check", &doc, "--with", harness, "--json"]));
    let predicted = target(&report, harness);
    assert_eq!(predicted["error"], Value::Null, "{harness}: {predicted}");

    let before = ids(&env, harness);
    let written = env.run(&[
        "continue",
        &doc,
        "--with",
        harness,
        "--no-resume",
        "--quiet",
    ]);
    assert!(written.status.success(), "{harness}: {}", stderr(&written));
    let new: Vec<String> = ids(&env, harness)
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect();
    assert_eq!(new.len(), 1, "{harness}: {new:?}");
    let exported = env.run(&["export", &new[0], "--from", harness]);
    assert!(
        exported.status.success(),
        "{harness}: {}",
        stderr(&exported)
    );
    let copy: Value = serde_json::from_slice(&exported.stdout).unwrap();

    // What really disappeared, against what the report says was lost.
    let (was, now) = (facets(&source), facets(&copy));
    let real = really_lost(&was, &now);
    let claimed = claimed_lost(predicted);
    assert_eq!(
        claimed, real,
        "{harness}: the report claims {claimed:?} lost but {real:?} really was\n{predicted}"
    );

    // Every claimed rename really happened.
    let renames: Vec<&str> = predicted["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["category"] == "tool_name")
        .map(|i| i["path"].as_str().unwrap())
        .collect();
    for path in &renames {
        let (old, new) = path.split_once(" -> ").unwrap();
        assert_eq!(
            now.get(&format!("tool:{new}")).copied(),
            Some(1),
            "{harness}: {path}"
        );
        assert_eq!(now.get(&format!("tool:{old}")), None, "{harness}: {path}");
    }
    // With none claimed, every tool call that is gone is claimed as lost.
    if renames.is_empty() {
        let gone: i64 = was
            .iter()
            .filter(|(k, _)| k.starts_with("tool:"))
            .map(|(k, v)| (v - now.get(k).copied().unwrap_or(0)).max(0))
            .sum();
        let lost_calls: i64 = predicted["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["kind"] == "lost" && i["category"] == "tool_call")
            .map(|i| i64::try_from(i["count"].as_u64().unwrap()).unwrap())
            .sum();
        assert_eq!(
            gone, lost_calls,
            "{harness}: tool calls gone against claimed"
        );
    }

    // The message count is exact.
    assert_eq!(
        copy["messages"].as_array().unwrap().len(),
        usize::try_from(predicted["messages_after"].as_u64().unwrap()).unwrap(),
        "{harness}: messages after"
    );
}

#[test]
fn prediction_matches_reality_for_claude_code() {
    assert_prediction_matches_reality("claude_code");
}

#[test]
fn prediction_matches_reality_for_codex() {
    assert_prediction_matches_reality("codex");
}

#[test]
fn prediction_matches_reality_for_pi() {
    assert_prediction_matches_reality("pi");
}

#[test]
fn prediction_matches_reality_for_campfire() {
    assert_prediction_matches_reality("campfire");
}

#[test]
fn prediction_matches_reality_for_cursor() {
    assert_prediction_matches_reality("cursor");
}

#[test]
fn prediction_matches_reality_for_cursor_desktop() {
    assert_prediction_matches_reality("cursor_desktop");
}

#[test]
fn prediction_matches_reality_for_grok() {
    assert_prediction_matches_reality("grok");
}

#[test]
fn prediction_matches_reality_for_fx() {
    assert_prediction_matches_reality("fx");
}

#[test]
fn prediction_matches_reality_for_antigravity() {
    assert_prediction_matches_reality("antigravity");
}
