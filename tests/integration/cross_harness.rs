#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Cross-harness conversion preserves block-level conversation content while
//! allowing harness-specific grouping and metadata differences.

use chrono::{DateTime, Utc};
use txcript::common;
use txcript::harness::{
    amp, antigravity, campfire, claude_code, codex, cursor, cursor_desktop, grok, hermes, opencode,
    pi, simple,
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

#[test]
fn conversation_survives_every_hop() {
    let common = sample();
    let expected = signature(&common);

    // Land it in Claude, then walk it across every harness via the hub.
    let claude = claude_code::ClaudeCode::from_common(&common).unwrap();
    assert_eq!(
        signature(&claude_code::ClaudeCode::to_common(&claude).unwrap()),
        expected,
        "claude"
    );

    let codex = convert::<claude_code::ClaudeCode, codex::Codex>(&claude).unwrap();
    assert_eq!(
        signature(&codex::Codex::to_common(&codex).unwrap()),
        expected,
        "codex"
    );

    let opencode = convert::<codex::Codex, opencode::OpenCode>(&codex).unwrap();
    assert_eq!(
        signature(&opencode::OpenCode::to_common(&opencode).unwrap()),
        expected,
        "opencode"
    );

    let pi = convert::<opencode::OpenCode, pi::Pi>(&opencode).unwrap();
    assert_eq!(signature(&pi::Pi::to_common(&pi).unwrap()), expected, "pi");

    let campfire = convert::<pi::Pi, campfire::Campfire>(&pi).unwrap();
    assert_eq!(
        signature(&campfire::Campfire::to_common(&campfire).unwrap()),
        expected,
        "campfire"
    );

    let cursor = convert::<campfire::Campfire, cursor::Cursor>(&campfire).unwrap();
    assert_eq!(
        signature(&cursor::Cursor::to_common(&cursor).unwrap()),
        expected,
        "cursor"
    );

    let cursor_desktop = convert::<cursor::Cursor, cursor_desktop::CursorDesktop>(&cursor).unwrap();
    assert_eq!(
        signature(&cursor_desktop::CursorDesktop::to_common(&cursor_desktop).unwrap()),
        expected,
        "cursor_desktop"
    );

    let grok = convert::<cursor_desktop::CursorDesktop, grok::Grok>(&cursor_desktop).unwrap();
    assert_eq!(
        signature(&grok::Grok::to_common(&grok).unwrap()),
        expected,
        "grok"
    );

    let hermes = convert::<grok::Grok, hermes::Hermes>(&grok).unwrap();
    assert_eq!(
        signature(&hermes::Hermes::to_common(&hermes).unwrap()),
        expected,
        "hermes"
    );

    let amp = convert::<hermes::Hermes, amp::Amp>(&hermes).unwrap();
    assert_eq!(
        signature(&amp::Amp::to_common(&amp).unwrap()),
        expected,
        "amp"
    );

    let antigravity = convert::<amp::Amp, antigravity::Antigravity>(&amp).unwrap();
    assert_eq!(
        signature(&antigravity::Antigravity::to_common(&antigravity).unwrap()),
        expected,
        "antigravity"
    );

    let simple = convert::<antigravity::Antigravity, simple::Simple>(&antigravity).unwrap();
    assert_eq!(
        signature(&simple::Simple::to_common(&simple).unwrap()),
        expected,
        "simple"
    );

    // And all the way back to Claude.
    let round = convert::<simple::Simple, claude_code::ClaudeCode>(&simple).unwrap();
    assert_eq!(
        signature(&claude_code::ClaudeCode::to_common(&round).unwrap()),
        expected,
        "claude (round)"
    );
}
