#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use chrono::{DateTime, Utc};
use serde_json::json;
use txcript::common::{self, Block};
use txcript::harness::dsh;
use txcript::{Codec, Common, Store, TextCodec};

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn sample_body() -> dsh::DshSession {
    dsh::DshSession {
        header: json!({
            "type": "session",
            "version": 0,
            "id": "session-abc",
            "createdAt": 1_704_067_445_000_i64,
            "cwd": "/repo",
            "delegationDepth": 0,
            "agentPreset": "standard"
        }),
        events: vec![
            json!({"type": "session/title", "seq": 0, "time": 1_704_067_445_000_i64,
                "data": {"title": "Parser work", "source": {"kind": "fallback"}}}),
            json!({"type": "request/context", "seq": 1, "time": 1_704_067_445_001_i64,
                "data": {"provider": "deepseek-official", "model": "deepseek-v4-flash"}}),
            json!({"type": "user/message", "seq": 2, "time": 1_704_067_445_002_i64, "data": {
                "role": "user",
                "id": "u1",
                "content": [{"type": "text", "text": "list files"}],
                "source": {"kind": "user"}
            }, "surfaceOp": "append"}),
            json!({"type": "reasoning-chunks", "seq0": 3, "time0": 1_704_067_445_003_i64,
                "data": {"texts": ["should", " not", " appear"]}}),
            json!({"type": "assistant/message", "seq": 4, "time": 1_704_067_445_004_i64, "data": {
                "turn": 1, "step": 1,
                "message": {
                    "role": "assistant",
                    "id": "a1",
                    "content": [
                        {"type": "reasoning", "text": "plan"},
                        {"type": "tool-call", "id": "c1", "name": "bash",
                         "arguments": "{\"command\":\"ls\"}"}
                    ],
                    "source": {"kind": "model", "provider": "deepseek-official", "model": "deepseek-v4-flash"}
                }
            }, "surfaceOp": "append"}),
            json!({"type": "tool/result", "seq": 5, "time": 1_704_067_445_005_i64, "data": {
                "turn": 1, "step": 1,
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool-result",
                        "toolCallId": "c1",
                        "isError": false,
                        "content": [{"type": "text", "text": "a.rs"}]
                    }]
                }
            }, "surfaceOp": "append"}),
            json!({"type": "future.dsh.event", "seq": 6, "time": 1_704_067_445_006_i64, "data": {}}),
        ],
    }
}

#[test]
fn metadata_and_messages_are_converted() {
    let native = txcript::Transcript::new(
        common::Meta {
            id: "session-abc".into(),
            timestamp: ts("2024-01-01T00:00:45.000Z"),
            cwd: Some("/repo".into()),
            git_branch: None,
            title: Some("Parser work".into()),
            cli_version: None,
            model: Some("deepseek-v4-flash".into()),
        },
        sample_body(),
    );
    let text = dsh::Dsh::to_text(&native).unwrap();
    let parsed = dsh::Dsh::from_text(&text).unwrap();
    assert_eq!(parsed.meta.cwd.as_deref(), Some("/repo"));
    assert_eq!(parsed.meta.title.as_deref(), Some("Parser work"));
    assert_eq!(parsed.meta.model.as_deref(), Some("deepseek-v4-flash"));

    let common = dsh::Dsh::to_common(&parsed).unwrap();
    assert_eq!(common.body.len(), 3);
    assert!(matches!(
        common.body[1].content[0],
        common::Block::Thinking { .. }
    ));
    assert!(matches!(
        common.body[1].content[1],
        common::Block::ToolUse { .. }
    ));
    assert!(matches!(
        common.body[2].content[0],
        common::Block::ToolResult { .. }
    ));
}

#[test]
fn native_text_round_trip_retains_unknown_events() {
    let transcript = txcript::Transcript::new(
        common::Meta {
            id: "session-abc".into(),
            timestamp: ts("2024-01-01T00:00:45.000Z"),
            cwd: Some("/repo".into()),
            git_branch: None,
            title: Some("Parser work".into()),
            cli_version: None,
            model: Some("deepseek-v4-flash".into()),
        },
        sample_body(),
    );
    let text = dsh::Dsh::to_text(&transcript).unwrap();
    let parsed = dsh::Dsh::from_text(&text).unwrap();
    assert_eq!(parsed.body, sample_body());
    assert!(text.contains("future.dsh.event"));
}

#[test]
fn read_only_store_discovers_and_refuses_writes() {
    let root = tempfile::tempdir().unwrap();
    let session = root.path().join("--repo--").join("session-abc");
    write_session(&session, &sample_body());
    let store = dsh::DshStore::new(root.path());
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].meta.id, "session-abc");
    let loaded = store.load(&found[0].reference).unwrap();
    assert_eq!(loaded.body, sample_body());
    assert!(store.save(&loaded).is_err());
    assert!(store.delete(&found[0].reference).is_err());
}

#[test]
fn discovery_does_not_depend_on_the_directory_name() {
    let root = tempfile::tempdir().unwrap();
    write_session(
        &root.path().join("workspace-1").join("conversation-7"),
        &sample_body(),
    );
    let found = dsh::DshStore::new(root.path()).discover().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].meta.id, "session-abc");
}

fn write_session(dir: &std::path::Path, body: &dsh::DshSession) {
    std::fs::create_dir_all(dir).unwrap();
    let mut text = format!("{}\n", serde_json::to_string(&body.header).unwrap());
    for event in &body.events {
        text.push_str(&serde_json::to_string(event).unwrap());
        text.push('\n');
    }
    std::fs::write(dir.join("session.jsonl"), text).unwrap();
}

fn common_sample() -> txcript::Transcript<Common> {
    txcript::Transcript::new(
        common::Meta {
            id: "session-xyz".into(),
            timestamp: ts("2024-01-01T00:00:45.000Z"),
            cwd: Some("/repo".into()),
            git_branch: None,
            title: Some("hi".into()),
            cli_version: None,
            model: Some("deepseek-v4-flash".into()),
        },
        vec![common::Message {
            role: common::Role::User,
            content: vec![Block::Text {
                text: "hello".into(),
            }],
            timestamp: ts("2024-01-01T00:00:46.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        }],
    )
}

#[test]
fn rendering_is_deterministic() {
    let a = dsh::Dsh::from_common(&common_sample()).unwrap();
    let b = dsh::Dsh::from_common(&common_sample()).unwrap();
    assert_eq!(a.body, b.body);
}

#[test]
fn assistant_timestamps_survive_a_common_round_trip() {
    let native = dsh::Dsh::from_common(&common_sample()).unwrap();
    let common = dsh::Dsh::to_common(&native).unwrap();
    assert_eq!(common.body[0].timestamp, ts("2024-01-01T00:00:46.000Z"));
}
