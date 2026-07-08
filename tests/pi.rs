#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Integration tests for the pi harness — on-disk Store fidelity, metadata
//! discovery, the tool-normalization and bashExecution-expansion transforms,
//! and the codec fixpoint through Common.

use chrono::{DateTime, Utc};
use serde_json::json;
use txcript::common;
use txcript::harness::pi;
use txcript::{Codec, Common, Store, Transcript};

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// A realistic pi session: header, model/title bookkeeping, a user ask, an
/// assistant turn that thinks and calls `edit`, a toolResult, a `!`-shell
/// bashExecution, and a `custom_message`.
fn sample_jsonl() -> String {
    let lines = [
        json!({"type": "session", "version": 3, "id": "abc", "timestamp": "2026-01-02T03:04:05.000Z", "cwd": "/repo"}),
        json!({"type": "model_change", "id": "m1", "parentId": null, "timestamp": "2026-01-02T03:04:05.100Z", "modelId": "claude-opus-4-8"}),
        json!({"type": "session_info", "id": "s1", "parentId": "m1", "timestamp": "2026-01-02T03:04:05.200Z", "name": "Parser work"}),
        json!({"type": "message", "id": "u1", "parentId": "s1", "timestamp": "2026-01-02T03:04:06.000Z",
            "message": {"role": "user", "content": [{"type": "text", "text": "edit the file"}], "timestamp": 1}}),
        json!({"type": "message", "id": "a1", "parentId": "u1", "timestamp": "2026-01-02T03:04:07.000Z",
            "message": {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "let me edit"},
                {"type": "text", "text": "On it."},
                {"type": "toolCall", "id": "call-1", "name": "edit", "arguments": {
                    "path": "/repo/a.rs", "edits": [{"oldText": "old", "newText": "new"}]}}],
                "model": "claude-opus-4-8", "provider": "anthropic", "api": "anthropic-messages",
                "usage": {"input": 10, "output": 20, "cacheRead": 5, "cacheWrite": 2, "totalTokens": 37,
                    "cost": {"input": 0.004_537, "output": 0.001_428, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.005_965}},
                "stopReason": "toolUse", "timestamp": 2}}),
        json!({"type": "message", "id": "t1", "parentId": "a1", "timestamp": "2026-01-02T03:04:08.000Z",
            "message": {"role": "toolResult", "toolCallId": "call-1", "toolName": "edit",
                "content": [{"type": "text", "text": "done"}], "isError": false, "timestamp": 3}}),
        json!({"type": "message", "id": "b1", "parentId": "t1", "timestamp": "2026-01-02T03:04:09.000Z",
            "message": {"role": "bashExecution", "command": "ls", "output": "f1\nf2\n", "exitCode": 0, "excludeFromContext": false, "timestamp": 4}}),
        json!({"type": "custom_message", "id": "c1", "parentId": "b1", "timestamp": "2026-01-02T03:04:10.000Z",
            "content": [{"type": "text", "text": "remember this"}]}),
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
    let store = pi::PiStore::new(dir.path());

    let src = dir.path().join("orig.jsonl");
    std::fs::write(&src, sample_jsonl()).unwrap();

    let loaded = store.load(&src).unwrap();
    let saved = store.save(&loaded).unwrap();
    let reloaded = store.load(&saved.reference).unwrap();

    // Every native record survives — header, model_change/session_info
    // bookkeeping, messages, and the custom_message.
    assert_eq!(loaded.body, reloaded.body);
    assert!(saved.reference.to_string_lossy().contains("--repo--"));
}

#[test]
fn discover_extracts_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("--repo--");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("s.jsonl"), sample_jsonl()).unwrap();

    let store = pi::PiStore::new(dir.path());
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    let meta = &found[0].meta;
    assert_eq!(meta.id, "abc");
    assert_eq!(meta.cwd.as_deref(), Some("/repo"));
    assert_eq!(meta.model.as_deref(), Some("claude-opus-4-8"));
    assert_eq!(meta.title.as_deref(), Some("Parser work"));
    assert_eq!(meta.timestamp, ts("2026-01-02T03:04:05.000Z"));
}

#[test]
fn to_common_normalizes_tools_and_expands_bash() {
    let dir = tempfile::tempdir().unwrap();
    let store = pi::PiStore::new(dir.path());
    let src = dir.path().join("s.jsonl");
    std::fs::write(&src, sample_jsonl()).unwrap();

    let common = pi::Pi::to_common(&store.load(&src).unwrap()).unwrap();
    let msgs = &common.body;

    // user, assistant(think/text/edit), toolResult, bash-use, bash-result,
    // custom_message -> 6 messages.
    assert_eq!(msgs.len(), 6);

    // pi `edit` with one hunk normalizes to a typed Edit with renamed keys.
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
            assert_eq!(id, "call-1");
            assert_eq!(file_path, "/repo/a.rs");
            assert_eq!(old_string, "old");
            assert_eq!(new_string, "new");
        }
        other => panic!("expected Edit, got {other:?}"),
    }
    assert_eq!(msgs[1].stop_reason, Some(common::StopReason::ToolUse));
    let usage = msgs[1].usage.unwrap();
    assert_eq!(usage.input_tokens, 10);
    assert_eq!(usage.cache_creation_input_tokens, Some(2));
    // The recorded spend surfaces as the cost total; the split isn't modeled.
    assert_eq!(usage.cost_usd, Some(0.005_965));

    // toolResult flattens to text on a User message.
    assert!(matches!(
        &msgs[2].content[0],
        common::Block::ToolResult { tool_use_id, content: common::ToolOutput::Text(t), is_error: false }
            if tool_use_id == "call-1" && t == "done"
    ));

    // bashExecution expands into a Bash tool_use + its result.
    assert!(matches!(
        &msgs[3].content[0],
        common::Block::ToolUse { tool: common::Tool::Bash { command, .. }, .. } if command == "ls"
    ));
    assert!(matches!(
        &msgs[4].content[0],
        common::Block::ToolResult { content: common::ToolOutput::Text(t), .. } if t == "f1\nf2\n"
    ));

    // custom_message replays as a user turn.
    assert_eq!(msgs[5].role, common::Role::User);
    assert!(matches!(&msgs[5].content[0], common::Block::Text { text } if text == "remember this"));
}

#[test]
fn multi_edit_maps_to_multiedit() {
    let dir = tempfile::tempdir().unwrap();
    let store = pi::PiStore::new(dir.path());
    let src = dir.path().join("s.jsonl");
    std::fs::write(
        &src,
        [
            json!({"type": "session", "version": 3, "id": "abc", "timestamp": "2026-01-02T03:04:05.000Z", "cwd": "/repo"}).to_string(),
            json!({"type": "message", "id": "a1", "parentId": null, "timestamp": "2026-01-02T03:04:07.000Z",
                "message": {"role": "assistant", "content": [
                    {"type": "toolCall", "id": "c1", "name": "edit", "arguments": {
                        "path": "/repo/a.rs", "edits": [
                            {"oldText": "a", "newText": "b"}, {"oldText": "c", "newText": "d"}]}}],
                    "model": "m", "stopReason": "toolUse", "timestamp": 1}}).to_string(),
        ]
        .join("\n"),
    )
    .unwrap();

    let common = pi::Pi::to_common(&store.load(&src).unwrap()).unwrap();
    match &common.body[0].content[0] {
        common::Block::ToolUse {
            tool: common::Tool::MultiEdit { file_path, edits },
            ..
        } => {
            assert_eq!(file_path, "/repo/a.rs");
            assert_eq!(edits.len(), 2);
            assert_eq!(edits[0].old_string, "a");
            assert_eq!(edits[1].new_string, "d");
        }
        other => panic!("expected MultiEdit, got {other:?}"),
    }
}

/// A Common transcript shaped the way a pi round-trip can reproduce exactly:
/// homogeneous user messages, assistant turns with `model/usage/stop_reason`,
/// pi-representable thinking (no signature).
fn sample_common() -> Transcript<Common> {
    let meta = common::Meta {
        id: "abc".into(),
        timestamp: ts("2026-01-02T03:04:05.000Z"),
        cwd: Some("/repo".into()),
        git_branch: None,
        title: None,
        cli_version: None,
        model: Some("claude-opus-4-8".into()),
    };
    let body = vec![
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::Text {
                text: "edit it".into(),
            }],
            timestamp: ts("2026-01-02T03:04:06.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![
                common::Block::Thinking {
                    text: "thinking".into(),
                    signature: None,
                    encrypted: None,
                },
                common::Block::Text {
                    text: "on it".into(),
                },
                common::Block::ToolUse {
                    id: "call-1".into(),
                    tool: common::Tool::Edit {
                        file_path: "/repo/a.rs".into(),
                        old_string: "old".into(),
                        new_string: "new".into(),
                        replace_all: false,
                    },
                },
            ],
            timestamp: ts("2026-01-02T03:04:07.000Z"),
            model: Some("claude-opus-4-8".into()),
            stop_reason: Some(common::StopReason::ToolUse),
            usage: None,
        },
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::ToolResult {
                tool_use_id: "call-1".into(),
                content: common::ToolOutput::Text("done".into()),
                is_error: false,
            }],
            timestamp: ts("2026-01-02T03:04:08.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::Text {
                text: "finished".into(),
            }],
            timestamp: ts("2026-01-02T03:04:09.000Z"),
            model: Some("claude-opus-4-8".into()),
            stop_reason: Some(common::StopReason::EndTurn),
            usage: Some(common::Usage {
                input_tokens: 10,
                output_tokens: 20,
                cache_read_input_tokens: Some(5),
                cache_creation_input_tokens: Some(2),
                cost_usd: Some(0.005_965),
            }),
        },
    ];
    Transcript::new(meta, body)
}

#[test]
fn codec_fixpoint_through_common_loses_nothing() {
    let common = sample_common();
    let native = pi::Pi::from_common(&common).unwrap();
    let back = pi::Pi::to_common(&native).unwrap();
    assert_eq!(common, back);
}
