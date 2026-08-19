#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Property-based codec invariants: for generated well-formed conversations,
//! `to_common(from_common(c))` preserves every block's content, order, and
//! call→result pairing, for every harness codec. The handcrafted fixtures in
//! the per-harness modules pin exact shapes; these sweep the input space.
//!
//! The generator is deliberately constrained to what every harness models —
//! text, thinking, Edit/Bash/Raw calls with text results — so a failure here
//! is a lost or corrupted block, not a known representational gap. Widen it
//! deliberately, not incidentally: `is_error` results, JSON tool output, and
//! images are asymmetrically supported today and belong behind per-harness
//! expectations if added.

use chrono::{DateTime, Utc};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use serde_json::json;
use txcript::common::{Block, Message, Meta, Role, Tool, ToolOutput};
use txcript::harness::{
    amp, antigravity, campfire, claude_code, codex, cursor, cursor_desktop, grok, hermes, opencode,
    pi, simple,
};
use txcript::{Codec, Common, Transcript};

fn ts(secs: u32) -> DateTime<Utc> {
    format!("2026-01-02T03:{:02}:{:02}.000Z", secs / 60, secs % 60)
        .parse()
        .unwrap()
}

/// One logical exchange step; `ToolCall` expands to an assistant call plus
/// the user message carrying its result.
#[derive(Debug, Clone)]
enum Unit {
    UserText(String),
    AssistantText {
        thinking: Option<String>,
        text: String,
    },
    ToolCall {
        tool: Tool,
        result: String,
    },
}

/// Message text: non-empty, may span two lines, and includes the envelope's
/// own metacharacters (`<`, `>`, `&`) so escaping bugs surface. Edges stay
/// on word characters: cursor's native format stores trimmed text, so
/// leading/trailing whitespace is a known, deliberate normalization.
fn arb_text() -> impl Strategy<Value = String> {
    let line = "[a-zA-Z0-9]([a-zA-Z0-9 ,.!?<>&'/_-]{0,37}[a-zA-Z0-9,.!?<>&'/_-])?";
    prop_oneof![
        3 => line.prop_map(String::from),
        1 => (line, line).prop_map(|(a, b)| format!("{a}\n{b}")),
    ]
}

fn arb_tool() -> impl Strategy<Value = Tool> {
    prop_oneof![
        (
            "/repo/[a-z]{1,8}\\.rs",
            arb_text(),
            arb_text(),
            any::<bool>()
        )
            .prop_map(
                |(file_path, old_string, new_string, replace_all)| Tool::Edit {
                    file_path,
                    old_string,
                    new_string,
                    replace_all,
                }
            ),
        arb_text().prop_map(|command| Tool::Bash {
            command,
            workdir: None,
            timeout_ms: None,
            description: None,
            run_in_background: false,
        }),
        // A name no harness types, so it can't normalize into a typed tool.
        // Title-case-shaped only: opencode lowercases unknown names on write
        // and title-cases on read, so any other shape (`mcp__x`, `WebSearch`)
        // does not round-trip through it today — a known codec bug this
        // generator excludes rather than hides behind a looser assertion.
        ("Custom_[a-z]{1,8}", arb_text()).prop_map(|(tool_name, note)| Tool::Raw {
            tool_name,
            input: json!({ "note": note }),
        }),
    ]
}

fn arb_unit() -> impl Strategy<Value = Unit> {
    prop_oneof![
        arb_text().prop_map(Unit::UserText),
        (proptest::option::of(arb_text()), arb_text())
            .prop_map(|(thinking, text)| Unit::AssistantText { thinking, text }),
        (arb_tool(), arb_text()).prop_map(|(tool, result)| Unit::ToolCall { tool, result }),
    ]
}

/// A conversation that always opens with a user message, as every harness's
/// real sessions do.
fn arb_conversation() -> impl Strategy<Value = Vec<Unit>> {
    (arb_text(), proptest::collection::vec(arb_unit(), 0..6)).prop_map(|(opening, rest)| {
        let mut units = vec![Unit::UserText(opening)];
        units.extend(rest);
        units
    })
}

fn build(units: &[Unit]) -> Transcript<Common> {
    let meta = Meta {
        id: "prop-1".to_string(),
        timestamp: ts(0),
        cwd: Some("/repo".to_string()),
        git_branch: None,
        title: Some("property".to_string()),
        cli_version: None,
        model: Some("claude-opus-4-8".to_string()),
    };
    let msg = |role, content, secs| Message {
        role,
        content,
        timestamp: ts(secs),
        model: match role {
            Role::Assistant => Some("claude-opus-4-8".to_string()),
            Role::User => None,
        },
        stop_reason: None,
        usage: None,
    };
    let mut body = Vec::new();
    let mut calls = 0;
    for (i, unit) in units.iter().enumerate() {
        let secs = u32::try_from(i).unwrap() * 2 + 1;
        match unit {
            Unit::UserText(text) => {
                body.push(msg(
                    Role::User,
                    vec![Block::Text { text: text.clone() }],
                    secs,
                ));
            }
            Unit::AssistantText { thinking, text } => {
                let mut content = Vec::new();
                if let Some(thinking) = thinking {
                    content.push(Block::Thinking {
                        text: thinking.clone(),
                        signature: None,
                        encrypted: None,
                    });
                }
                content.push(Block::Text { text: text.clone() });
                body.push(msg(Role::Assistant, content, secs));
            }
            Unit::ToolCall { tool, result } => {
                calls += 1;
                let id = format!("call-{calls}");
                body.push(msg(
                    Role::Assistant,
                    vec![Block::ToolUse {
                        id: id.clone(),
                        tool: tool.clone(),
                    }],
                    secs,
                ));
                body.push(msg(
                    Role::User,
                    vec![Block::ToolResult {
                        tool_use_id: id,
                        content: ToolOutput::Text(result.clone()),
                        is_error: false,
                    }],
                    secs + 1,
                ));
            }
        }
    }
    Transcript::new(meta, body)
}

/// A flat, grouping-independent fingerprint: one line per block, role-tagged,
/// with call ids replaced by their 1-based order of first use. Two
/// transcripts compare equal exactly when the same blocks appear in the same
/// order with the same call→result pairing, regardless of how a harness
/// groups blocks into messages or mints ids.
fn signature(t: &Transcript<Common>) -> Vec<String> {
    let mut ids = std::collections::HashMap::new();
    let mut renumber = |id: &str| {
        let next = ids.len() + 1;
        ids.entry(id.to_string()).or_insert(next).to_string()
    };
    let mut out = Vec::new();
    for m in &t.body {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        for block in &m.content {
            let desc = match block {
                Block::Text { text } => format!("text:{text}"),
                Block::Thinking { text, .. } => format!("thinking:{text}"),
                Block::ToolUse { id, tool } => {
                    let desc = match tool {
                        // `replace_all` stays out of the fingerprint: pi's
                        // (and campfire's) edit format has no replace-all
                        // concept, so it cannot survive that hop.
                        Tool::Edit {
                            file_path,
                            old_string,
                            new_string,
                            ..
                        } => format!("Edit:{file_path}:{old_string}->{new_string}"),
                        other => format!("{other:?}"),
                    };
                    format!("use:{}:{desc}", renumber(id))
                }
                Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let text = match content {
                        ToolOutput::Text(s) => s.clone(),
                        ToolOutput::Json(v) => v.to_string(),
                    };
                    format!("result:{}:{is_error}:{text}", renumber(tool_use_id))
                }
                Block::Image { source } => format!("image:{}", source.media_type),
            };
            out.push(format!("{role}/{desc}"));
        }
    }
    out
}

fn assert_fixpoint<C: Codec>(name: &str, common: &Transcript<Common>) -> Result<(), TestCaseError> {
    let native = C::from_common(common)
        .map_err(|e| TestCaseError::fail(format!("{name}: from_common failed: {e}")))?;
    let back = C::to_common(&native)
        .map_err(|e| TestCaseError::fail(format!("{name}: to_common failed: {e}")))?;
    prop_assert_eq!(
        signature(common),
        signature(&back),
        "{} lost conversation",
        name
    );
    Ok(())
}

proptest! {
    #[test]
    fn every_codec_preserves_generated_conversations(units in arb_conversation()) {
        let common = build(&units);
        assert_fixpoint::<claude_code::ClaudeCode>("claude_code", &common)?;
        assert_fixpoint::<codex::Codex>("codex", &common)?;
        assert_fixpoint::<opencode::OpenCode>("opencode", &common)?;
        assert_fixpoint::<pi::Pi>("pi", &common)?;
        assert_fixpoint::<campfire::Campfire>("campfire", &common)?;
        assert_fixpoint::<cursor::Cursor>("cursor", &common)?;
        assert_fixpoint::<cursor_desktop::CursorDesktop>("cursor_desktop", &common)?;
        assert_fixpoint::<grok::Grok>("grok", &common)?;
        assert_fixpoint::<hermes::Hermes>("hermes", &common)?;
        assert_fixpoint::<amp::Amp>("amp", &common)?;
        assert_fixpoint::<antigravity::Antigravity>("antigravity", &common)?;
        assert_fixpoint::<simple::Simple>("simple", &common)?;
    }
}
