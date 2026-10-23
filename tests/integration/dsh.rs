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
            lineage: None,
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
            lineage: None,
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

/// A v4 log shaped like one dsh 0.2 records: the system prompt is a surface
/// node, dsh injects its runtime context as extra `user/message`s, and tool
/// results are tool-role messages rather than `tool-result` wrappers.
fn v4_body() -> dsh::DshSession {
    let t = 1_792_066_443_800_i64;
    dsh::DshSession {
        header: json!({
            "type": "session", "version": 4, "id": "session-v4", "createdAt": t,
            "cwd": "/repo", "isSeeded": false, "delegationDepth": 0
        }),
        events: vec![
            json!({"type": "turn/start", "seq": 0, "time": t, "data": {"turn": 1}}),
            json!({"type": "step/start", "seq": 1, "time": t, "data": {"turn": 1, "step": 1}}),
            json!({"type": "system/message", "seq": 2, "time": t, "data": {"turn": 1, "step": 1,
                "message": {"role": "system", "id": "sys", "source": {"kind": "system-prompt"},
                    "content": [{"type": "text", "text": "You are an AI agent."}]}},
                "surfaceOp": "append"}),
            json!({"type": "user/message", "seq": 3, "time": t + 1, "data": {
                "role": "user", "id": "u1", "source": {"kind": "user"},
                "content": [{"type": "text", "text": "count the lines"}]}, "surfaceOp": "append"}),
            json!({"type": "user/message", "seq": 4, "time": t + 1, "data": {
                "role": "user", "id": "u2", "source": {"kind": "runtime-context", "form": "snapshot"},
                "content": [{"type": "text", "text": "Current runtime context."}]}, "surfaceOp": "append"}),
            json!({"type": "assistant/message", "seq": 5, "time": t + 2, "data": {"turn": 1, "step": 1,
                "message": {"role": "assistant", "id": "a1",
                    "source": {"kind": "model", "provider": "deepseek-official", "model": "deepseek-flash"},
                    "content": [
                        {"type": "reasoning", "text": "read it"},
                        {"type": "tool-call", "id": "c1", "name": "read",
                         "arguments": "{\"file_path\": \"/repo/notes.txt\"}"}
                    ]}}, "surfaceOp": "append"}),
            json!({"type": "tool/call", "seq": 6, "time": t + 2, "data": {"turn": 1, "step": 1,
                "callId": "c1", "name": "read", "arguments": "{\"file_path\": \"/repo/notes.txt\"}"}}),
            json!({"type": "tool/result", "seq": 7, "time": t + 3, "data": {"turn": 1, "step": 1,
                "message": {"role": "tool", "id": "r1", "source": {"kind": "tool", "callId": "c1"},
                    "toolCallId": "c1", "isError": false,
                    "content": [{"type": "text", "text": "1: alpha\n2: beta"}]}},
                "sourceEventSeqs": [6], "surfaceOp": "append"}),
            json!({"type": "step/end", "seq": 8, "time": t + 3, "data": {"turn": 1, "step": 1}}),
            json!({"type": "step/start", "seq": 9, "time": t + 3, "data": {"turn": 1, "step": 2}}),
            json!({"type": "assistant/message", "seq": 10, "time": t + 4, "data": {"turn": 1, "step": 2,
                "message": {"role": "assistant", "id": "a2",
                    "source": {"kind": "model", "provider": "deepseek-official", "model": "deepseek-flash"},
                    "content": [{"type": "text", "text": "It has 2 lines."}]}}, "surfaceOp": "append"}),
            json!({"type": "step/end", "seq": 11, "time": t + 4, "data": {"turn": 1, "step": 2}}),
            json!({"type": "turn/end", "seq": 12, "time": t + 4,
                "data": {"turn": 1, "reason": {"kind": "completed"}}}),
        ],
    }
}

fn write_log(dir: &std::path::Path, name: &str, body: &dsh::DshSession) {
    std::fs::create_dir_all(dir).unwrap();
    let mut text = format!("{}\n", serde_json::to_string(&body.header).unwrap());
    for event in &body.events {
        text.push_str(&serde_json::to_string(event).unwrap());
        text.push('\n');
    }
    std::fs::write(dir.join(name), text).unwrap();
}

fn texts(message: &common::Message) -> Vec<String> {
    message
        .content
        .iter()
        .map(|block| match block {
            Block::Text { text } => format!("text:{text}"),
            Block::Thinking { text, .. } => format!("thinking:{text}"),
            Block::ToolUse { id, .. } => format!("tool_use:{id}"),
            Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => format!("tool_result:{tool_use_id}:{is_error}:{content:?}"),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn discovery_reads_the_newest_versioned_generation() {
    // dsh 0.2 names each generation `session.vN.jsonl(.zstd)` and, when it
    // migrates an old session, writes the new generation beside the original
    // instead of replacing it. The highest generation is the live one.
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("--repo--").join("session-v4");
    let mut stale = sample_body();
    stale.header["id"] = json!("session-v4");
    write_log(&dir, "session.jsonl", &stale);
    write_log(&dir, "session.v4.jsonl", &v4_body());

    let store = dsh::DshStore::new(root.path());
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    let loaded = store.load(&found[0].reference).unwrap();
    assert_eq!(loaded.body.header["version"], json!(4));
}

#[test]
fn v4_surface_projects_tool_role_results_and_skips_injected_context() {
    let native = txcript::Transcript::new(
        common::Meta {
            id: "session-v4".into(),
            timestamp: ts("2026-10-15T00:00:00Z"),
            cwd: Some("/repo".into()),
            git_branch: None,
            title: None,
            cli_version: None,
            model: None,
            lineage: None,
        },
        v4_body(),
    );
    let common = dsh::Dsh::to_common(&native).unwrap();
    let got: Vec<(common::Role, Vec<String>)> = common
        .body
        .iter()
        .map(|message| (message.role, texts(message)))
        .collect();
    assert_eq!(
        got,
        vec![
            (common::Role::User, vec!["text:count the lines".to_string()]),
            (
                common::Role::Assistant,
                vec!["thinking:read it".to_string(), "tool_use:c1".to_string()]
            ),
            (
                common::Role::User,
                vec!["tool_result:c1:false:Text(\"1: alpha\\n2: beta\")".to_string()]
            ),
            (
                common::Role::Assistant,
                vec!["text:It has 2 lines.".to_string()]
            ),
        ]
    );
}

#[test]
fn v4_replacement_endpoints_are_start_seq_and_end_seq() {
    // Since v3 a replace op names its span `startSeq`/`endSeq`, and the system
    // prompt is a surface node the span is counted around. A compaction
    // checkpoint replaces the conversation it summarizes.
    let mut body = v4_body();
    body.events.push(json!({"type": "user/message", "seq": 13, "time": 1_792_066_443_900_i64,
        "data": {"role": "user", "id": "k1", "source": {"kind": "compact-checkpoint", "compactionId": "x"},
            "content": [{"type": "text", "text": "Summary: counted 2 lines."}]},
        "sourceEventSeqs": [3, 4, 5, 7, 10],
        "surfaceOp": {"op": "replace", "startSeq": 3, "endSeq": 10}}));
    let native = txcript::Transcript::new(
        common::Meta {
            id: "session-v4".into(),
            timestamp: ts("2026-10-15T00:00:00Z"),
            cwd: None,
            git_branch: None,
            title: None,
            cli_version: None,
            model: None,
            lineage: None,
        },
        body,
    );
    let common = dsh::Dsh::to_common(&native).unwrap();
    assert_eq!(common.body.len(), 1, "{:?}", common.body);
    assert_eq!(
        texts(&common.body[0]),
        vec!["text:Summary: counted 2 lines.".to_string()]
    );
}

#[test]
fn parent_session_becomes_lineage() {
    let root = tempfile::tempdir().unwrap();
    // A subagent run records its parent and `origin: subagent`.
    let mut child = v4_body();
    child.header["id"] = json!("child");
    child.header["parentSession"] = json!("session-parent");
    child.header["origin"] = json!("subagent");
    child.header["delegationDepth"] = json!(1);
    write_log(
        &root.path().join("--repo--").join("child"),
        "session.v4.jsonl",
        &child,
    );
    // A user fork records only the parent, and is seeded with its history.
    let mut fork = v4_body();
    fork.header["id"] = json!("session-fork");
    fork.header["parentSession"] = json!("session-parent");
    fork.header["isSeeded"] = json!(true);
    write_log(
        &root.path().join("--repo--").join("session-fork"),
        "session.v4.jsonl",
        &fork,
    );

    let mut found = dsh::DshStore::new(root.path()).discover().unwrap();
    found.sort_by(|a, b| a.meta.id.cmp(&b.meta.id));
    let lineage = |i: usize| found[i].meta.lineage.clone().expect("lineage");
    assert_eq!(found[0].meta.id, "child");
    assert_eq!(lineage(0).parent, "session-parent");
    assert_eq!(lineage(0).relation, common::Relation::Spawn);
    assert_eq!(found[1].meta.id, "session-fork");
    assert_eq!(lineage(1).relation, common::Relation::Fork);
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
            lineage: None,
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
