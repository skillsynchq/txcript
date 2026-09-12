#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Cross-harness conversion preserves block-level conversation content while
//! allowing harness-specific grouping and metadata differences.

use chrono::{DateTime, Utc};
use txcript::common;
use txcript::harness::{
    amp, antigravity, campfire, claude_code, codex, cowork, cursor, cursor_desktop, fx, grok,
    grok_bot, hermes, opencode, pi, simple,
};
use txcript::{Codec, Common, Transcript, convert};

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// A flat, grouping-independent fingerprint of the conversation: one line per
/// block, in order, role-tagged. Stable across harnesses that split or merge
/// messages differently.
fn signature(t: &Transcript<Common>) -> Vec<String> {
    let mut out = Vec::new();
    for msg in &t.body {
        let role = match msg.role {
            common::Role::User => "user",
            common::Role::Assistant => "assistant",
        };
        for block in &msg.content {
            let desc = match block {
                common::Block::Text { text } => format!("text:{text}"),
                common::Block::Thinking { text, .. } => format!("thinking:{text}"),
                common::Block::ToolUse { id, tool } => match tool {
                    common::Tool::Edit {
                        file_path,
                        old_string,
                        new_string,
                        ..
                    } => {
                        format!("use:{id}:Edit:{file_path}:{old_string}->{new_string}")
                    }
                    common::Tool::Bash { command, .. } => format!("use:{id}:Bash:{command}"),
                    common::Tool::Raw { tool_name, .. } => format!("use:{id}:Raw:{tool_name}"),
                    other => format!("use:{id}:{other:?}"),
                },
                common::Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let text = match content {
                        common::ToolOutput::Text(s) => s.clone(),
                        common::ToolOutput::Json(v) => v.to_string(),
                    };
                    format!("result:{tool_use_id}:{is_error}:{text}")
                }
                common::Block::Image { source } => format!("image:{}", source.media_type),
                common::Block::Artifact { artifact } => {
                    format!("artifact:{}:{}", artifact.id, artifact.display_text())
                }
            };
            out.push(format!("{role}/{desc}"));
        }
    }
    out
}

/// Single-block-per-turn so every harness's grouping agrees: a user ask, an
/// assistant Edit, the tool result, and a closing line.
fn sample() -> Transcript<Common> {
    let meta = common::Meta {
        id: "x1".into(),
        timestamp: ts("2026-01-02T03:04:05.000Z"),
        cwd: Some("/repo".into()),
        git_branch: None,
        title: Some("Cross".into()),
        cli_version: None,
        model: Some("claude-opus-4-8".into()),
    };
    let model = || Some("claude-opus-4-8".to_string());
    let body = vec![
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::Text {
                text: "fix the bug".into(),
            }],
            timestamp: ts("2026-01-02T03:04:06.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::ToolUse {
                id: "call-1".into(),
                tool: common::Tool::Edit {
                    file_path: "/repo/a.rs".into(),
                    old_string: "i <= n".into(),
                    new_string: "i < n".into(),
                    replace_all: false,
                },
            }],
            timestamp: ts("2026-01-02T03:04:07.000Z"),
            model: model(),
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::ToolResult {
                tool_use_id: "call-1".into(),
                content: common::ToolOutput::Text("patched".into()),
                is_error: false,
            }],
            timestamp: ts("2026-01-02T03:04:07.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::Text {
                text: "done".into(),
            }],
            timestamp: ts("2026-01-02T03:04:08.000Z"),
            model: model(),
            stop_reason: None,
            usage: None,
        },
    ];
    Transcript::new(meta, body)
}

fn sample_with_raw_tool(tool_name: &str) -> Transcript<Common> {
    let meta = common::Meta {
        id: "x2".into(),
        timestamp: ts("2026-01-02T03:04:05.000Z"),
        cwd: Some("/repo".into()),
        git_branch: None,
        title: Some("Cross Raw".into()),
        cli_version: None,
        model: Some("claude-opus-4-8".into()),
    };
    let model = || Some("claude-opus-4-8".to_string());
    let body = vec![
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::Text {
                text: "execute action".into(),
            }],
            timestamp: ts("2026-01-02T03:04:06.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::ToolUse {
                id: "call-1".into(),
                tool: common::Tool::Raw {
                    tool_name: tool_name.into(),
                    input: serde_json::json!({ "query": "txcript" }),
                },
            }],
            timestamp: ts("2026-01-02T03:04:07.000Z"),
            model: model(),
            stop_reason: Some(common::StopReason::ToolUse),
            usage: None,
        },
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::ToolResult {
                tool_use_id: "call-1".into(),
                content: common::ToolOutput::Text("executed".into()),
                is_error: false,
            }],
            timestamp: ts("2026-01-02T03:04:07.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::Text {
                text: "done".into(),
            }],
            timestamp: ts("2026-01-02T03:04:08.000Z"),
            model: model(),
            stop_reason: Some(common::StopReason::EndTurn),
            usage: None,
        },
    ];
    Transcript::new(meta, body)
}

fn assert_cycle(common: &Transcript<Common>, context: &str) {
    let expected = signature(common);

    // Land it in Claude, then walk it across every harness via the hub.
    let claude = claude_code::ClaudeCode::from_common(common).unwrap();
    assert_eq!(
        signature(&claude_code::ClaudeCode::to_common(&claude).unwrap()),
        expected,
        "{context}: claude"
    );

    let codex = convert::<claude_code::ClaudeCode, codex::Codex>(&claude).unwrap();
    assert_eq!(
        signature(&codex::Codex::to_common(&codex).unwrap()),
        expected,
        "{context}: codex"
    );

    let opencode = convert::<codex::Codex, opencode::OpenCode>(&codex).unwrap();
    assert_eq!(
        signature(&opencode::OpenCode::to_common(&opencode).unwrap()),
        expected,
        "{context}: opencode"
    );

    let pi = convert::<opencode::OpenCode, pi::Pi>(&opencode).unwrap();
    assert_eq!(
        signature(&pi::Pi::to_common(&pi).unwrap()),
        expected,
        "{context}: pi"
    );

    let campfire = convert::<pi::Pi, campfire::Campfire>(&pi).unwrap();
    assert_eq!(
        signature(&campfire::Campfire::to_common(&campfire).unwrap()),
        expected,
        "{context}: campfire"
    );

    let cursor = convert::<campfire::Campfire, cursor::Cursor>(&campfire).unwrap();
    assert_eq!(
        signature(&cursor::Cursor::to_common(&cursor).unwrap()),
        expected,
        "{context}: cursor"
    );

    let cursor_desktop = convert::<cursor::Cursor, cursor_desktop::CursorDesktop>(&cursor).unwrap();
    assert_eq!(
        signature(&cursor_desktop::CursorDesktop::to_common(&cursor_desktop).unwrap()),
        expected,
        "{context}: cursor_desktop"
    );

    let grok = convert::<cursor_desktop::CursorDesktop, grok::Grok>(&cursor_desktop).unwrap();
    assert_eq!(
        signature(&grok::Grok::to_common(&grok).unwrap()),
        expected,
        "{context}: grok"
    );

    let grok_bot = convert::<grok::Grok, grok_bot::GrokBot>(&grok).unwrap();
    assert_eq!(
        signature(&grok_bot::GrokBot::to_common(&grok_bot).unwrap()),
        expected,
        "{context}: grok_bot"
    );

    let fx = convert::<grok_bot::GrokBot, fx::Fx>(&grok_bot).unwrap();
    assert_eq!(
        signature(&fx::Fx::to_common(&fx).unwrap()),
        expected,
        "{context}: fx"
    );

    let hermes = convert::<fx::Fx, hermes::Hermes>(&fx).unwrap();
    assert_eq!(
        signature(&hermes::Hermes::to_common(&hermes).unwrap()),
        expected,
        "{context}: hermes"
    );

    let amp = convert::<hermes::Hermes, amp::Amp>(&hermes).unwrap();
    assert_eq!(
        signature(&amp::Amp::to_common(&amp).unwrap()),
        expected,
        "{context}: amp"
    );

    let antigravity = convert::<amp::Amp, antigravity::Antigravity>(&amp).unwrap();
    assert_eq!(
        signature(&antigravity::Antigravity::to_common(&antigravity).unwrap()),
        expected,
        "{context}: antigravity"
    );

    let simple = convert::<antigravity::Antigravity, simple::Simple>(&antigravity).unwrap();
    assert_eq!(
        signature(&simple::Simple::to_common(&simple).unwrap()),
        expected,
        "{context}: simple"
    );

    let cowork = convert::<simple::Simple, cowork::Cowork>(&simple).unwrap();
    assert_eq!(
        signature(&cowork::Cowork::to_common(&cowork).unwrap()),
        expected,
        "{context}: cowork"
    );

    // And all the way back to Claude.
    let round = convert::<cowork::Cowork, claude_code::ClaudeCode>(&cowork).unwrap();
    assert_eq!(
        signature(&claude_code::ClaudeCode::to_common(&round).unwrap()),
        expected,
        "{context}: claude (round)"
    );
}

#[test]
fn conversation_survives_every_hop() {
    assert_cycle(&sample(), "edit");
}

#[test]
fn custom_tool_casing_survives_every_hop() {
    for tool_name in [
        "mcp__github__create_issue",
        "custom_analyzer",
        "WebSearch",
        "myCustomTool",
    ] {
        assert_cycle(&sample_with_raw_tool(tool_name), tool_name);
    }
}

#[test]
fn stop_reasons_survive_conversions() {
    for expected_reason in [
        common::StopReason::EndTurn,
        common::StopReason::ToolUse,
        common::StopReason::MaxTokens,
        common::StopReason::Aborted,
        common::StopReason::Error,
    ] {
        let mut t = sample();
        let last_idx = t.body.len() - 1;
        t.body[last_idx].stop_reason = Some(expected_reason.clone());

        // Roundtrip through Claude Code
        let claude = claude_code::ClaudeCode::from_common(&t).unwrap();
        let back_claude = claude_code::ClaudeCode::to_common(&claude).unwrap();
        assert_eq!(
            back_claude.body[last_idx].stop_reason,
            Some(expected_reason.clone()),
            "ClaudeCode failed for {expected_reason:?}"
        );

        // Convert Claude Code -> Hermes -> Amp -> Grok -> OpenCode -> Pi -> Antigravity
        let hermes = convert::<claude_code::ClaudeCode, hermes::Hermes>(&claude).unwrap();
        let back_hermes = hermes::Hermes::to_common(&hermes).unwrap();
        assert_eq!(
            back_hermes.body[last_idx].stop_reason,
            Some(expected_reason.clone()),
            "Hermes failed for {expected_reason:?}"
        );

        let amp = convert::<hermes::Hermes, amp::Amp>(&hermes).unwrap();
        let back_amp = amp::Amp::to_common(&amp).unwrap();
        assert_eq!(
            back_amp.body[last_idx].stop_reason,
            Some(expected_reason.clone()),
            "Amp failed for {expected_reason:?}"
        );

        let grok = convert::<amp::Amp, grok::Grok>(&amp).unwrap();
        let back_grok = grok::Grok::to_common(&grok).unwrap();
        assert_eq!(
            back_grok.body[last_idx].stop_reason,
            Some(expected_reason.clone()),
            "Grok failed for {expected_reason:?}"
        );

        let opencode = convert::<grok::Grok, opencode::OpenCode>(&grok).unwrap();
        let back_opencode = opencode::OpenCode::to_common(&opencode).unwrap();
        assert_eq!(
            back_opencode.body[last_idx].stop_reason,
            Some(expected_reason.clone()),
            "OpenCode failed for {expected_reason:?}"
        );

        let pi = convert::<opencode::OpenCode, pi::Pi>(&opencode).unwrap();
        let back_pi = pi::Pi::to_common(&pi).unwrap();
        assert_eq!(
            back_pi.body[last_idx].stop_reason,
            Some(expected_reason.clone()),
            "Pi failed for {expected_reason:?}"
        );

        let antigravity = convert::<pi::Pi, antigravity::Antigravity>(&pi).unwrap();
        let back_antigravity = antigravity::Antigravity::to_common(&antigravity).unwrap();
        assert_eq!(
            back_antigravity.body[last_idx].stop_reason,
            Some(expected_reason.clone()),
            "Antigravity failed for {expected_reason:?}"
        );
    }
}
