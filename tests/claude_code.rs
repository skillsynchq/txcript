#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Covers Store round-trip fidelity, Common codec fixpoints, and conversation
//! extraction.

use chrono::{DateTime, Utc};
use serde_json::json;
use txcript::common;
use txcript::harness::claude_code;
use txcript::{Codec, Common, Store, Transcript};

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// A realistic single session: a user ask, an assistant turn that thinks and
/// calls Edit, the tool result, and a final assistant turn with usage. Plus
/// non-message lines (summary, custom-title, a snapshot) that must survive on
/// disk but never become messages.
fn sample_jsonl() -> String {
    let lines = [
        json!({"type": "summary", "summary": "Fix the parser", "leafUuid": "abc"}),
        json!({"type": "custom-title", "customTitle": "Parser work"}),
        json!({
            "type": "user", "uuid": "u1", "parentUuid": null,
            "sessionId": "sess-1", "cwd": "/work/repo", "gitBranch": "main",
            "version": "1.2.3", "timestamp": "2026-01-02T03:04:05.000Z",
            "message": {"role": "user", "content": "fix the off-by-one"},
        }),
        json!({
            "type": "assistant", "uuid": "a1", "parentUuid": "u1",
            "sessionId": "sess-1", "timestamp": "2026-01-02T03:04:06.000Z",
            "message": {
                "role": "assistant",
                "model": "claude-opus-4-8",
                "content": [
                    {"type": "thinking", "thinking": "off-by-one in the loop", "signature": "sig-xyz"},
                    {"type": "text", "text": "Patching the bound."},
                    {"type": "tool_use", "id": "t1", "name": "Edit", "input": {
                        "file_path": "/work/repo/src/p.rs",
                        "old_string": "i <= n", "new_string": "i < n"
                    }},
                ],
            },
        }),
        json!({
            "type": "user", "uuid": "u2", "parentUuid": "a1",
            "sessionId": "sess-1", "timestamp": "2026-01-02T03:04:07.000Z",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "applied"},
            ]},
        }),
        json!({
            "type": "assistant", "uuid": "a2", "parentUuid": "u2",
            "sessionId": "sess-1", "timestamp": "2026-01-02T03:04:08.000Z",
            "message": {
                "role": "assistant",
                "model": "claude-opus-4-8",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 100, "output_tokens": 20, "cache_read_input_tokens": 50},
                "content": [{"type": "text", "text": "Done."}],
            },
        }),
        // A line type the codec doesn't model — must round-trip untouched.
        json!({"type": "file-history-snapshot", "snapshot": {"files": ["a", "b"]}}),
    ];
    lines
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[test]
fn store_round_trip_is_lossless_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let store = claude_code::ClaudeStore::new(dir.path());

    let src = dir.path().join("orig.jsonl");
    std::fs::write(&src, sample_jsonl()).unwrap();

    let loaded = store.load(&src).unwrap();
    let saved = store.save(&loaded).unwrap();
    let reloaded = store.load(&saved.reference).unwrap();

    // Every native record — including the summary, title, and snapshot lines
    // the codec ignores — survives a load→save→load cycle unchanged.
    assert_eq!(loaded.body, reloaded.body);
    // And the on-disk landing spot is derived from the session metadata.
    assert!(saved.reference.ends_with("sess-1.jsonl"));
}

#[test]
fn windows_cwd_encodes_the_project_dir() {
    let dir = tempfile::tempdir().unwrap();
    let store = claude_code::ClaudeStore::new(dir.path());

    let src = dir.path().join("orig.jsonl");
    let jsonl = sample_jsonl().replace("/work/repo", r"C:\\Users\\dev\\repo");
    std::fs::write(&src, jsonl).unwrap();

    // `C:\Users\dev\repo` lands in `C--Users-dev-repo`, Claude's own
    // Windows encoding (`\` and `:` map to `-` like `/` and `.`).
    let saved = store.save(&store.load(&src).unwrap()).unwrap();
    let project = saved.reference.parent().unwrap().file_name().unwrap();
    assert_eq!(project.to_str(), Some("C--Users-dev-repo"));
}

#[test]
fn discover_extracts_session_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("-work-repo");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("sess-1.jsonl"), sample_jsonl()).unwrap();

    let store = claude_code::ClaudeStore::new(dir.path());
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    let meta = &found[0].meta;
    assert_eq!(meta.id, "sess-1");
    assert_eq!(meta.cwd.as_deref(), Some("/work/repo"));
    assert_eq!(meta.git_branch.as_deref(), Some("main"));
    assert_eq!(meta.model.as_deref(), Some("claude-opus-4-8"));
    assert_eq!(meta.cli_version.as_deref(), Some("1.2.3"));
    // custom-title wins over the summary line.
    assert_eq!(meta.title.as_deref(), Some("Parser work"));
    assert_eq!(meta.timestamp, ts("2026-01-02T03:04:05.000Z"));
}

#[test]
fn to_common_extracts_the_conversation_faithfully() {
    let dir = tempfile::tempdir().unwrap();
    let store = claude_code::ClaudeStore::new(dir.path());
    let src = dir.path().join("s.jsonl");
    std::fs::write(&src, sample_jsonl()).unwrap();

    let common = claude_code::ClaudeCode::to_common(&store.load(&src).unwrap()).unwrap();
    let msgs = &common.body;

    // Four conversational turns; the summary/title/snapshot lines are dropped.
    assert_eq!(msgs.len(), 4);

    assert_eq!(msgs[0].role, common::Role::User);
    assert!(
        matches!(&msgs[0].content[0], common::Block::Text { text } if text == "fix the off-by-one")
    );

    // Assistant turn: thinking (with signature), text, and a typed Edit.
    assert_eq!(msgs[1].role, common::Role::Assistant);
    assert_eq!(msgs[1].model.as_deref(), Some("claude-opus-4-8"));
    assert!(matches!(
        &msgs[1].content[0],
        common::Block::Thinking { text, signature: Some(s), .. } if text == "off-by-one in the loop" && s == "sig-xyz"
    ));
    match &msgs[1].content[2] {
        common::Block::ToolUse {
            id,
            tool:
                common::Tool::Edit {
                    file_path,
                    old_string,
                    new_string,
                    ..
                },
        } => {
            assert_eq!(id, "t1");
            assert_eq!(file_path, "/work/repo/src/p.rs");
            assert_eq!(old_string, "i <= n");
            assert_eq!(new_string, "i < n");
        }
        other => panic!("expected Edit tool_use, got {other:?}"),
    }

    // Tool result rides on a User message (Anthropic convention).
    assert_eq!(msgs[2].role, common::Role::User);
    assert!(matches!(
        &msgs[2].content[0],
        common::Block::ToolResult { tool_use_id, content: common::ToolOutput::Text(t), is_error: false }
            if tool_use_id == "t1" && t == "applied"
    ));

    // Final turn carries usage and stop reason.
    assert_eq!(msgs[3].stop_reason, Some(common::StopReason::EndTurn));
    let usage = msgs[3].usage.unwrap();
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.cache_read_input_tokens, Some(50));
}

/// A Common transcript covering every block kind, used to prove the codec
/// fixpoint `to_common(from_common(c)) == c`.
fn sample_common() -> Transcript<Common> {
    let meta = common::Meta {
        id: "sess-1".into(),
        timestamp: ts("2026-01-02T03:04:05.000Z"),
        cwd: Some("/work/repo".into()),
        git_branch: Some("main".into()),
        title: Some("Parser work".into()),
        cli_version: Some("1.2.3".into()),
        model: Some("claude-opus-4-8".into()),
    };
    let body = vec![
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::Text {
                text: "fix it".into(),
            }],
            timestamp: ts("2026-01-02T03:04:05.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![
                common::Block::Thinking {
                    text: "thinking".into(),
                    signature: Some("sig".into()),
                    encrypted: None,
                },
                common::Block::Text {
                    text: "patching".into(),
                },
                common::Block::ToolUse {
                    id: "t1".into(),
                    tool: common::Tool::Edit {
                        file_path: "/a.rs".into(),
                        old_string: "x".into(),
                        new_string: "y".into(),
                        replace_all: false,
                    },
                },
            ],
            timestamp: ts("2026-01-02T03:04:06.000Z"),
            model: Some("claude-opus-4-8".into()),
            stop_reason: Some(common::StopReason::ToolUse),
            usage: None,
        },
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::ToolResult {
                tool_use_id: "t1".into(),
                content: common::ToolOutput::Text("ok".into()),
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
            model: Some("claude-opus-4-8".into()),
            stop_reason: Some(common::StopReason::EndTurn),
            usage: Some(common::Usage {
                input_tokens: 100,
                output_tokens: 20,
                cache_read_input_tokens: Some(50),
                cache_creation_input_tokens: None,
            }),
        },
    ];
    Transcript::new(meta, body)
}

#[test]
fn codec_fixpoint_through_common_loses_nothing() {
    let common = sample_common();
    let native = claude_code::ClaudeCode::from_common(&common).unwrap();
    let back = claude_code::ClaudeCode::to_common(&native).unwrap();
    assert_eq!(common, back);
}

/// `from_common` is a pure function: same input, identical output (deterministic
/// uuids), so conversions are reproducible.
#[test]
fn from_common_is_deterministic() {
    let common = sample_common();
    let a =
        serde_json::to_value(claude_code::ClaudeCode::from_common(&common).unwrap().body).unwrap();
    let b =
        serde_json::to_value(claude_code::ClaudeCode::from_common(&common).unwrap().body).unwrap();
    assert_eq!(a, b);
}
