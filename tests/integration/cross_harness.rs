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
#[allow(clippy::too_many_lines, clippy::similar_names)]
fn multimodal_artifacts_and_images_survive_conversions() {
    let t0 = ts("2026-08-18T10:00:00Z");
    let artifact = common::Artifact {
        id: "art-1".into(),
        name: "plan.md".into(),
        source: common::ArtifactSource::Text {
            text: "# My Plan\nDo stuff.".into(),
            media_type: Some("text/markdown".into()),
        },
    };
    let image = common::ImageSource {
        source_type: "base64".into(),
        media_type: "image/png".into(),
        data: "iVBORw0KGgo=".into(),
    };
    let common = Transcript::new(
        common::Meta {
            id: "multi-hop-1".into(),
            timestamp: t0,
            cwd: Some("/work".into()),
            git_branch: None,
            title: Some("multimodal".into()),
            cli_version: None,
            model: None,
        },
        vec![
            common::Message {
                role: common::Role::User,
                content: vec![
                    common::Block::Text {
                        text: "hello".into(),
                    },
                    common::Block::Artifact {
                        artifact: artifact.clone(),
                    },
                    common::Block::Image {
                        source: image.clone(),
                    },
                ],
                timestamp: t0,
                model: None,
                stop_reason: None,
                usage: None,
            },
            common::Message {
                role: common::Role::Assistant,
                content: vec![
                    common::Block::Text {
                        text: "output".into(),
                    },
                    common::Block::Artifact { artifact },
                    common::Block::Image { source: image },
                ],
                timestamp: t0,
                model: None,
                stop_reason: None,
                usage: None,
            },
        ],
    );

    // Verify Cursor Desktop from_common preserves both user and assistant artifact and image text
    let cd = cursor_desktop::CursorDesktop::from_common(&common).unwrap();
    let cd_common = cursor_desktop::CursorDesktop::to_common(&cd).unwrap();
    assert!(cd_common.body.iter().any(|m| m.content.iter().any(|b| {
        match b {
            common::Block::Text { text } => {
                text.contains("[artifact: plan.md]") || text.contains("My Plan")
            }
            _ => false,
        }
    })));
    assert!(cd_common.body.iter().any(|m| m.content.iter().any(|b| {
        match b {
            common::Block::Text { text } => text.contains("[image: image/png]"),
            _ => false,
        }
    })));

    // Verify GrokBot from_common preserves artifact and image content
    let gb = grok_bot::GrokBot::from_common(&common).unwrap();
    let gb_common = grok_bot::GrokBot::to_common(&gb).unwrap();
    assert!(gb_common.body.iter().any(|m| m.content.iter().any(|b| {
        match b {
            common::Block::Text { text } => {
                text.contains("[artifact: plan.md]") || text.contains("My Plan")
            }
            _ => false,
        }
    })));

    // Verify Hermes from_common preserves artifact
    let h = hermes::Hermes::from_common(&common).unwrap();
    let h_common = hermes::Hermes::to_common(&h).unwrap();
    assert!(h_common.body.iter().any(|m| m.content.iter().any(|b| {
        match b {
            common::Block::Text { text } => {
                text.contains("[artifact: plan.md]") || text.contains("My Plan")
            }
            _ => false,
        }
    })));

    // Verify Pi from_common preserves assistant image
    let p = pi::Pi::from_common(&common).unwrap();
    let p_common = pi::Pi::to_common(&p).unwrap();
    assert!(p_common.body.iter().any(|m| m.content.iter().any(|b| {
        match b {
            common::Block::Text { text } => text.contains("[image: image/png]"),
            _ => false,
        }
    })));

    // Verify Grok from_common preserves assistant image
    let g = grok::Grok::from_common(&common).unwrap();
    let g_common = grok::Grok::to_common(&g).unwrap();
    assert!(g_common.body.iter().any(|m| m.content.iter().any(|b| {
        match b {
            common::Block::Text { text } => text.contains("[image: image/png]"),
            _ => false,
        }
    })));
}

#[test]
fn codex_data_url_parsing_robustness() {
    use txcript::TextCodec;
    let jsonl = concat!(
        "{\"timestamp\":\"2026-08-18T10:00:00.000Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-1\",\"cwd\":\"/work\"}}\n",
        "{\"timestamp\":\"2026-08-18T10:00:01.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_image\",\"image_url\":\"data:IMAGE/PNG;name=photo.png;base64, iVBORw0KGgo= \\n\"}]}}\n"
    );
    let loaded = codex::Codex::from_text(jsonl).unwrap();
    let common = codex::Codex::to_common(&loaded).unwrap();
    let img = common.body[0].content.iter().find_map(|b| match b {
        common::Block::Image { source } => Some(source),
        _ => None,
    });
    assert!(img.is_some());
    let source = img.unwrap();
    assert_eq!(source.media_type, "image/png");
    assert_eq!(source.data, "iVBORw0KGgo=");
}
