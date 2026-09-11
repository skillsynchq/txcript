#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Integration tests for the `OpenCode` codec — the part→block transform
//! (synthetic/bookkeeping parts dropped, tool parts split into use+result,
//! turn usage/finish attached) and the codec fixpoint through Common. The
//! `SQLite` store has its own feature-gated unit test in the module.

use chrono::{DateTime, Utc};
use serde_json::json;
use txcript::common;
use txcript::harness::opencode;
use txcript::{Codec, Common, Transcript};

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn meta() -> common::Meta {
    common::Meta {
        id: "ses_1".into(),
        timestamp: ts("2026-01-02T03:04:05.000Z"),
        cwd: Some("/repo".into()),
        git_branch: None,
        title: Some("Demo".into()),
        cli_version: Some("1.15.0".into()),
        model: Some("claude-opus-4-7".into()),
    }
}

#[test]
fn to_common_splits_parts_and_attaches_turn_usage() {
    // One user message and one assistant message whose parts mix reasoning,
    // text, a completed edit tool, and trailing text — plus bookkeeping parts
    // (step-start/finish) that must be ignored.
    let export: opencode::Export = serde_json::from_value(json!({
        "info": { "id": "ses_1", "directory": "/repo" },
        "messages": [
            {
                "info": { "role": "user", "time": { "created": 1_778_834_704_520_i64 } },
                "parts": [{ "type": "text", "text": "please edit the file" }],
            },
            {
                "info": {
                    "role": "assistant",
                    "modelID": "claude-opus-4-7",
                    "finish": "stop",
                    "tokens": { "input": 6, "output": 88, "cache": { "write": 21428, "read": 10 } },
                    "time": { "created": 1_778_834_704_540_i64 },
                },
                "parts": [
                    { "type": "step-start", "snapshot": "abc" },
                    { "type": "reasoning", "text": "thinking about it" },
                    { "type": "text", "text": "On it." },
                    { "type": "tool", "tool": "edit", "callID": "call-1", "state": {
                        "status": "completed",
                        "input": { "filePath": "/repo/a.rs", "oldString": "old", "newString": "new" },
                        "output": "done" } },
                    { "type": "text", "text": "Finished the edit." },
                    { "type": "step-finish", "reason": "stop" },
                ],
            },
        ],
    }))
    .unwrap();

    let common = opencode::OpenCode::to_common(&Transcript::new(meta(), export)).unwrap();
    let msgs = &common.body;

    // user | assistant(reasoning+text) | assistant(ToolUse) | user(ToolResult) | assistant(text)
    assert_eq!(msgs.len(), 5);

    assert!(
        matches!(&msgs[0].content[0], common::Block::Text { text } if text == "please edit the file")
    );

    assert!(
        matches!(&msgs[1].content[0], common::Block::Thinking { text, .. } if text == "thinking about it")
    );
    assert!(matches!(&msgs[1].content[1], common::Block::Text { text } if text == "On it."));

    // `edit` normalizes to a typed Edit with renamed keys.
    assert!(matches!(
        &msgs[2].content[0],
        common::Block::ToolUse { id, tool: common::Tool::Edit { file_path, old_string, new_string, .. } }
            if id == "call-1" && file_path == "/repo/a.rs" && old_string == "old" && new_string == "new"
    ));
    assert!(matches!(
        &msgs[3].content[0],
        common::Block::ToolResult { tool_use_id, content: common::ToolOutput::Text(t), is_error: false }
            if tool_use_id == "call-1" && t == "done"
    ));

    // Usage + finish land on the turn's last assistant message.
    let last = &msgs[4];
    assert!(
        matches!(&last.content[0], common::Block::Text { text } if text == "Finished the edit.")
    );
    assert_eq!(last.stop_reason, Some(common::StopReason::EndTurn));
    let usage = last.usage.unwrap();
    assert_eq!(usage.input_tokens, 6);
    assert_eq!(usage.output_tokens, 88);
    assert_eq!(usage.cache_creation_input_tokens, Some(21428));
    assert_eq!(usage.cache_read_input_tokens, Some(10));
}

#[test]
fn errored_tool_call_becomes_error_result() {
    let export: opencode::Export = serde_json::from_value(json!({
        "info": { "id": "ses_1" },
        "messages": [{
            "info": { "role": "assistant", "modelID": "m", "time": { "created": 1i64 } },
            "parts": [{ "type": "tool", "tool": "bash", "callID": "c9", "state": {
                "status": "error", "input": { "command": "false" }, "error": "exit code 1" } }],
        }],
    }))
    .unwrap();

    let msgs = opencode::OpenCode::to_common(&Transcript::new(meta(), export))
        .unwrap()
        .body;
    assert_eq!(msgs.len(), 2);
    assert!(matches!(
        &msgs[0].content[0],
        common::Block::ToolUse {
            tool: common::Tool::Bash { .. },
            ..
        }
    ));
    assert!(matches!(
        &msgs[1].content[0],
        common::Block::ToolResult { content: common::ToolOutput::Text(t), is_error: true, .. } if t == "exit code 1"
    ));
}

#[test]
fn pending_tool_call_has_no_result() {
    let export: opencode::Export = serde_json::from_value(json!({
        "info": { "id": "ses_1" },
        "messages": [{
            "info": { "role": "assistant", "modelID": "m", "time": { "created": 1i64 } },
            "parts": [{ "type": "tool", "tool": "read", "callID": "c5", "state": {
                "status": "running", "input": { "filePath": "/repo/a.rs" } } }],
        }],
    }))
    .unwrap();

    let msgs = opencode::OpenCode::to_common(&Transcript::new(meta(), export))
        .unwrap()
        .body;
    assert_eq!(msgs.len(), 1);
    assert!(matches!(
        &msgs[0].content[0],
        common::Block::ToolUse {
            tool: common::Tool::Read { .. },
            ..
        }
    ));
}

/// Shaped to round-trip through `OpenCode`'s grouping: each assistant turn is its
/// own message, every assistant carries a model and a `stop_reason` (`OpenCode`
/// requires a finish), and the tool turn's result folds into the tool part.
fn sample_common() -> Transcript<Common> {
    let model = || Some("claude-opus-4-7".to_string());
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
            ],
            timestamp: ts("2026-01-02T03:04:07.000Z"),
            model: model(),
            stop_reason: Some(common::StopReason::EndTurn),
            usage: Some(common::Usage {
                input_tokens: 6,
                output_tokens: 88,
                cache_read_input_tokens: Some(10),
                cache_creation_input_tokens: Some(21428),
            }),
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::ToolUse {
                id: "call-1".into(),
                tool: common::Tool::Edit {
                    file_path: "/repo/a.rs".into(),
                    old_string: "old".into(),
                    new_string: "new".into(),
                    replace_all: false,
                },
            }],
            timestamp: ts("2026-01-02T03:04:08.000Z"),
            model: model(),
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
            // OpenCode folds the result into the tool part, so it shares the
            // tool turn's timestamp — reflect that for the fixpoint.
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
            timestamp: ts("2026-01-02T03:04:10.000Z"),
            model: model(),
            stop_reason: Some(common::StopReason::EndTurn),
            usage: Some(common::Usage {
                input_tokens: 1,
                output_tokens: 2,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }),
        },
    ];
    Transcript::new(meta(), body)
}

#[test]
fn codec_fixpoint_through_common_loses_nothing() {
    let common = sample_common();
    let native = opencode::OpenCode::from_common(&common).unwrap();
    let back = opencode::OpenCode::to_common(&native).unwrap();
    assert_eq!(common, back);
}

fn sample_parallel_common() -> Transcript<Common> {
    let meta = common::Meta {
        id: "ses_parallel".into(),
        timestamp: ts("2026-01-02T03:04:05.000Z"),
        cwd: Some("/repo".into()),
        git_branch: None,
        title: Some("Parallel".into()),
        cli_version: Some("1.15.0".into()),
        model: Some("claude-opus-4-7".into()),
    };
    let model = || Some("claude-opus-4-7".to_string());
    let body = vec![
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::Text {
                text: "read and edit files".into(),
            }],
            timestamp: ts("2026-01-02T03:04:06.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![
                common::Block::ToolUse {
                    id: "call-1".into(),
                    tool: common::Tool::Read {
                        file_path: "/repo/a.rs".into(),
                        offset: None,
                        limit: None,
                    },
                },
                common::Block::ToolUse {
                    id: "call-2".into(),
                    tool: common::Tool::Edit {
                        file_path: "/repo/b.rs".into(),
                        old_string: "foo".into(),
                        new_string: "bar".into(),
                        replace_all: false,
                    },
                },
            ],
            timestamp: ts("2026-01-02T03:04:07.000Z"),
            model: model(),
            stop_reason: Some(common::StopReason::ToolUse),
            usage: None,
        },
        common::Message {
            role: common::Role::User,
            content: vec![
                common::Block::ToolResult {
                    tool_use_id: "call-1".into(),
                    content: common::ToolOutput::Text("file content a".into()),
                    is_error: false,
                },
                common::Block::ToolResult {
                    tool_use_id: "call-2".into(),
                    content: common::ToolOutput::Text("edited b".into()),
                    is_error: false,
                },
            ],
            timestamp: ts("2026-01-02T03:04:08.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::Text {
                text: "all changes finished".into(),
            }],
            timestamp: ts("2026-01-02T03:04:09.000Z"),
            model: model(),
            stop_reason: Some(common::StopReason::EndTurn),
            usage: Some(common::Usage {
                input_tokens: 10,
                output_tokens: 20,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }),
        },
    ];
    Transcript::new(meta, body)
}

#[test]
fn parallel_tool_calls_and_results_preserve_valid_parent_ids() {
    let transcript = sample_parallel_common();
    let export = opencode::OpenCode::from_common(&transcript).unwrap();

    let emitted_user_ids: std::collections::HashSet<String> = export
        .body
        .messages
        .iter()
        .filter(|m| m.info.get("role").and_then(|r| r.as_str()) == Some("user"))
        .filter_map(|m| {
            m.info
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_string)
        })
        .collect();

    assert_eq!(
        emitted_user_ids.len(),
        1,
        "only the prompt user message is emitted"
    );

    for m in &export.body.messages {
        if m.info.get("role").and_then(|r| r.as_str()) == Some("assistant") {
            let parent_id = m
                .info
                .get("parentID")
                .and_then(|p| p.as_str())
                .expect("assistant message must have parentID");
            assert!(
                emitted_user_ids.contains(parent_id),
                "assistant parentID '{parent_id}' must resolve to an emitted user message",
            );
        }
    }
}
