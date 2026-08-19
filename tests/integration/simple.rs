#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Integration tests for the Simple interchange format — the leniency
//! ladder (string content, missing ids, missing timestamps), unknown-key
//! preservation at every level, and the codec fixpoint through Common.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use txcript::common;
use txcript::harness::simple;
use txcript::{Codec, Common, TextCodec, Transcript};

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// A native document exercising every rung of the ladder (anonymized): L0
/// string content, blocks with and without explicit ids, full metadata, an
/// unknown top-level key, an unknown message key, an unknown block type, an
/// unknown role, and one malformed message that must survive as a raw
/// record.
fn native_fixture() -> Value {
    json!({
        "id": "simple-fixture-1",
        "timestamp": "2026-08-18T10:00:00.000Z",
        "cwd": "/Users/dev/proj",
        "git_branch": "main",
        "title": "Pagination fix",
        "cli_version": "0.1.0",
        "model": "claude-opus-5",
        "generator": "my-agent/0.1",
        "messages": [
            { "role": "user", "content": "fix the off-by-one", "mood": "hopeful" },
            { "role": "assistant", "content": [
                { "type": "thinking", "text": "the bound is inclusive" },
                { "type": "tool_use", "name": "Bash", "input": { "command": "cargo test" } },
            ], "model": "claude-opus-5", "stop_reason": "tool_use" },
            { "role": "user", "content": [
                { "type": "tool_result", "content": "42 passed" },
            ] },
            { "role": "assistant", "content": [
                { "type": "tool_use", "id": "call-x", "name": "my_probe", "input": { "q": 1 } },
                { "type": "vibes", "intensity": 11 },
            ] },
            { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "call-x", "content": { "hits": [1, 2] }, "is_error": true },
            ] },
            { "role": "assistant", "content": "Done.",
              "timestamp": "2026-08-18T10:00:12.000Z",
              "usage": { "input_tokens": 900, "output_tokens": 80 } },
            { "role": "system", "content": "you are a helpful agent" },
            { "role": "user", "content": "thanks", "timestamp": "not-a-timestamp" },
        ],
    })
}

#[test]
fn text_round_trip_is_lossless() {
    let first = simple::Simple::from_text(&native_fixture().to_string()).unwrap();
    let second = simple::Simple::from_text(&simple::Simple::to_text(&first).unwrap()).unwrap();
    assert_eq!(first, second);

    // The malformed message survives as a raw record, verbatim.
    assert!(first.body.records.iter().any(|r| matches!(
        r,
        simple::Record::Other(v) if v.get("timestamp").and_then(Value::as_str) == Some("not-a-timestamp")
    )));
    // Unknown keys survive at the top level and on messages.
    assert_eq!(
        first.body.extra.get("generator"),
        Some(&json!("my-agent/0.1"))
    );
    assert!(first.body.records.iter().any(|r| matches!(
        r,
        simple::Record::Message(m) if m.extra.get("mood") == Some(&json!("hopeful"))
    )));
}

#[test]
fn from_text_extracts_metadata() {
    let meta = simple::Simple::from_text(&native_fixture().to_string())
        .unwrap()
        .meta;
    assert_eq!(meta.id, "simple-fixture-1");
    assert_eq!(meta.timestamp, ts("2026-08-18T10:00:00Z"));
    assert_eq!(meta.cwd.as_deref(), Some("/Users/dev/proj"));
    assert_eq!(meta.git_branch.as_deref(), Some("main"));
    assert_eq!(meta.title.as_deref(), Some("Pagination fix"));
    assert_eq!(meta.cli_version.as_deref(), Some("0.1.0"));
    assert_eq!(meta.model.as_deref(), Some("claude-opus-5"));
}

#[test]
fn to_common_extracts_the_full_ladder() {
    let native = simple::Simple::from_text(&native_fixture().to_string()).unwrap();

    let common = simple::Simple::to_common(&native).unwrap();
    // The system-role and malformed messages are not conversation.
    assert_eq!(common.body.len(), 6);

    // L0 string content becomes a text block.
    assert_eq!(
        common.body[0].content,
        vec![common::Block::Text {
            text: "fix the off-by-one".into()
        }]
    );

    // A canonical name types the tool; the id-less call gets a synthetic id
    // that the id-less result pairs with, FIFO.
    let call_id = match &common.body[1].content[1] {
        common::Block::ToolUse {
            id,
            tool: common::Tool::Bash { command, .. },
        } => {
            assert_eq!(command, "cargo test");
            id.clone()
        }
        other => panic!("expected a typed Bash call, got {other:?}"),
    };
    assert_eq!(
        common.body[2].content,
        vec![common::Block::ToolResult {
            tool_use_id: call_id,
            content: common::ToolOutput::Text("42 passed".into()),
            is_error: false,
        }]
    );
    assert_eq!(common.body[1].model.as_deref(), Some("claude-opus-5"));
    assert_eq!(
        common.body[1].stop_reason,
        Some(common::StopReason::ToolUse)
    );

    // A free-form name passes through as Raw; the unknown block type drops.
    assert_eq!(common.body[3].content.len(), 1);
    assert_eq!(
        common.body[3].content[0],
        common::Block::ToolUse {
            id: "call-x".into(),
            tool: common::Tool::Raw {
                tool_name: "my_probe".into(),
                input: json!({"q": 1})
            },
        }
    );
    // An explicit pairing with JSON output and the error flag.
    assert_eq!(
        common.body[4].content,
        vec![common::Block::ToolResult {
            tool_use_id: "call-x".into(),
            content: common::ToolOutput::Json(json!({"hits": [1, 2]})),
            is_error: true,
        }]
    );

    // Timestamps: absent inherits the nearest preceding (here the session's),
    // explicit is kept, and usage lands where written.
    assert_eq!(common.body[0].timestamp, ts("2026-08-18T10:00:00Z"));
    assert_eq!(common.body[5].timestamp, ts("2026-08-18T10:00:12Z"));
    assert_eq!(
        common.body[5].usage,
        Some(common::Usage {
            input_tokens: 900,
            output_tokens: 80,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        })
    );
}

/// A canonical transcript at full fidelity: every block type, typed and raw
/// tools, JSON and error results, provider reasoning tokens, usage, stop
/// reasons, an image, and a single-text message (the L0 shorthand path).
#[allow(clippy::too_many_lines)]
fn rich_common() -> Transcript<Common> {
    let meta = common::Meta {
        id: "fix-42".into(),
        timestamp: ts("2026-08-18T10:00:00Z"),
        cwd: Some("/Users/dev/proj".into()),
        git_branch: Some("main".into()),
        title: Some("Pagination fix".into()),
        cli_version: Some("0.1.0".into()),
        model: Some("claude-opus-5".into()),
    };
    let body = vec![
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::Text {
                text: "fix it".into(),
            }],
            timestamp: ts("2026-08-18T10:00:01Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![
                common::Block::Thinking {
                    text: "inclusive bound".into(),
                    signature: Some("sig-abc".into()),
                    encrypted: None,
                },
                common::Block::ToolUse {
                    id: "call-1".into(),
                    tool: common::Tool::Edit {
                        file_path: "/Users/dev/proj/a.rs".into(),
                        old_string: "i <= n".into(),
                        new_string: "i < n".into(),
                        replace_all: false,
                    },
                },
            ],
            timestamp: ts("2026-08-18T10:00:02Z"),
            model: Some("claude-opus-5".into()),
            stop_reason: Some(common::StopReason::ToolUse),
            usage: Some(common::Usage {
                input_tokens: 900,
                output_tokens: 80,
                cache_read_input_tokens: Some(800),
                cache_creation_input_tokens: None,
            }),
        },
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::ToolResult {
                tool_use_id: "call-1".into(),
                content: common::ToolOutput::Json(json!({"applied": true})),
                is_error: false,
            }],
            timestamp: ts("2026-08-18T10:00:03Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::ToolUse {
                id: "call-2".into(),
                tool: common::Tool::Raw {
                    tool_name: "my_probe".into(),
                    input: json!({"q": 1}),
                },
            }],
            timestamp: ts("2026-08-18T10:00:04Z"),
            model: Some("claude-opus-5".into()),
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::User,
            content: vec![
                common::Block::ToolResult {
                    tool_use_id: "call-2".into(),
                    content: common::ToolOutput::Text("probe failed".into()),
                    is_error: true,
                },
                common::Block::Image {
                    source: common::ImageSource {
                        source_type: "base64".into(),
                        media_type: "image/png".into(),
                        data: "aGk=".into(),
                    },
                },
            ],
            timestamp: ts("2026-08-18T10:00:05Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::Text {
                text: "Done.".into(),
            }],
            timestamp: ts("2026-08-18T10:00:06Z"),
            model: Some("claude-opus-5".into()),
            stop_reason: Some(common::StopReason::EndTurn),
            usage: None,
        },
    ];
    Transcript::new(meta, body)
}

#[test]
fn codec_fixpoint_through_common_loses_nothing() {
    let common = rich_common();
    let native = simple::Simple::from_common(&common).unwrap();
    let back = simple::Simple::to_common(&native).unwrap();
    assert_eq!(common, back);
}

#[test]
fn text_round_trip_preserves_the_native_body() {
    let common = rich_common();
    let native = simple::Simple::from_common(&common).unwrap();
    let text = simple::Simple::to_text(&native).unwrap();
    let reparsed = simple::Simple::from_text(&text).unwrap();
    assert_eq!(native, reparsed);
}

#[test]
fn from_common_is_deterministic() {
    let common = rich_common();
    let a = simple::Simple::to_text(&simple::Simple::from_common(&common).unwrap()).unwrap();
    let b = simple::Simple::to_text(&simple::Simple::from_common(&common).unwrap()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn barebones_document_is_enough() {
    let text = r#"{"messages": [
        {"role": "user", "content": "hi"},
        {"role": "assistant", "content": "hello"}
    ]}"#;
    let native = simple::Simple::from_text(text).unwrap();
    let common = simple::Simple::to_common(&native).unwrap();
    assert_eq!(common.body.len(), 2);
    assert_eq!(common.body[0].role, common::Role::User);
    assert_eq!(
        common.body[1].content,
        vec![common::Block::Text {
            text: "hello".into()
        }]
    );
    // Both messages inherit the synthesized session timestamp.
    assert_eq!(common.body[0].timestamp, common.meta.timestamp);
}

#[test]
fn documents_without_a_messages_array_are_rejected() {
    assert!(simple::Simple::from_text("[]").is_err());
    assert!(simple::Simple::from_text(r#"{"not": "simple"}"#).is_err());
    assert!(simple::Simple::from_text(r#"{"messages": 5}"#).is_err());
    assert!(simple::Simple::from_text("not json").is_err());
}

#[test]
fn id_less_results_pair_fifo_across_interleaved_calls() {
    let text = json!({
        "id": "s",
        "timestamp": "2026-08-18T10:00:00Z",
        "messages": [
            { "role": "assistant", "content": [
                { "type": "tool_use", "name": "Bash", "input": { "command": "a" } },
                { "type": "tool_use", "name": "Bash", "input": { "command": "b" } },
            ] },
            { "role": "user", "content": [
                { "type": "tool_result", "content": "out-a" },
                { "type": "tool_result", "content": "out-b" },
            ] },
        ],
    })
    .to_string();
    let common = simple::Simple::to_common(&simple::Simple::from_text(&text).unwrap()).unwrap();

    let call = |block: &common::Block| match block {
        common::Block::ToolUse { id, .. } => id.clone(),
        other => panic!("expected a call, got {other:?}"),
    };
    let result = |block: &common::Block| match block {
        common::Block::ToolResult { tool_use_id, .. } => tool_use_id.clone(),
        other => panic!("expected a result, got {other:?}"),
    };
    assert_eq!(
        call(&common.body[0].content[0]),
        result(&common.body[1].content[0])
    );
    assert_eq!(
        call(&common.body[0].content[1]),
        result(&common.body[1].content[1])
    );
    assert_ne!(
        result(&common.body[1].content[0]),
        result(&common.body[1].content[1])
    );
}
