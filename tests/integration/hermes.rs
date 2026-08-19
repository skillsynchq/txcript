#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Integration tests for Hermes Agent's exported session and `SQLite` store.

use chrono::{DateTime, Utc};
use serde_json::json;
#[cfg(feature = "hermes")]
use txcript::Store;
use txcript::harness::hermes;
use txcript::{Codec, Common, TextCodec, Transcript};

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

#[test]
#[allow(clippy::too_many_lines)]
fn exported_session_preserves_conversation_tools_and_reasoning() {
    let export = json!({
        "id": "20260717_120337_deadbeef",
        "source": "cli",
        "model": "gpt-5.6-sol",
        "started_at": 1_768_000_000.0,
        "cwd": "/repo",
        "git_branch": "main",
        "title": "Hermes demo",
        "messages": [
            {
                "id": 1,
                "session_id": "20260717_120337_deadbeef",
                "role": "user",
                "content": "read the file",
                "timestamp": 1_768_000_001.0,
                "active": 1,
                "compacted": 0
            },
            {
                "id": 2,
                "session_id": "20260717_120337_deadbeef",
                "role": "assistant",
                "content": "I'll inspect it.",
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "arguments": "{\"path\":\"/repo/README.md\",\"offset\":1}"
                    }
                }],
                "reasoning_content": "Need to inspect the requested file.",
                "finish_reason": "tool_calls",
                "timestamp": 1_768_000_002.0,
                "active": 1,
                "compacted": 0
            },
            {
                "id": 3,
                "session_id": "20260717_120337_deadbeef",
                "role": "tool",
                "content": "{\"content\":\"1|# Demo\",\"total_lines\":1}",
                "tool_call_id": "call-1",
                "tool_name": "read_file",
                "timestamp": 1_768_000_003.0,
                "active": 1,
                "compacted": 0
            },
            {
                "id": 4,
                "session_id": "20260717_120337_deadbeef",
                "role": "assistant",
                "content": "README loaded.",
                "finish_reason": "stop",
                "timestamp": 1_768_000_004.0,
                "active": 1,
                "compacted": 0
            },
            {
                "id": 5,
                "session_id": "20260717_120337_deadbeef",
                "role": "session_meta",
                "content": null,
                "timestamp": 1_768_000_005.0,
                "active": 1,
                "compacted": 0,
                "future_field": {"kept": true}
            }
        ],
        "future_session_field": {"kept": true}
    });

    let text = serde_json::to_string(&export).unwrap();
    let native = hermes::Hermes::from_text(&text).unwrap();
    assert_eq!(native.body, export);
    assert_eq!(native.meta.id, "20260717_120337_deadbeef");
    assert_eq!(native.meta.timestamp, ts("2026-01-09T23:06:40Z"));
    assert_eq!(native.meta.cwd.as_deref(), Some("/repo"));
    assert_eq!(native.meta.git_branch.as_deref(), Some("main"));
    assert_eq!(native.meta.title.as_deref(), Some("Hermes demo"));
    assert_eq!(native.meta.model.as_deref(), Some("gpt-5.6-sol"));

    let common = hermes::Hermes::to_common(&native).unwrap();
    assert_eq!(common.body.len(), 4);
    assert!(matches!(
        &common.body[1].content[0],
        txcript::common::Block::Thinking { text, .. }
            if text == "Need to inspect the requested file."
    ));
    assert!(matches!(
        &common.body[1].content[2],
        txcript::common::Block::ToolUse {
            id,
            tool: txcript::common::Tool::Read { file_path, offset: Some(1), .. },
        } if id == "call-1" && file_path == "/repo/README.md"
    ));
    assert!(matches!(
        &common.body[2].content[0],
        txcript::common::Block::ToolResult {
            tool_use_id,
            content: txcript::common::ToolOutput::Json(value),
            is_error: false,
        } if tool_use_id == "call-1" && value["content"] == "1|# Demo"
    ));
    assert_eq!(
        common.body[3].stop_reason,
        Some(txcript::common::StopReason::EndTurn)
    );

    let rendered = hermes::Hermes::to_text(&native).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&rendered).unwrap(),
        export
    );
}

fn sample_common() -> Transcript<Common> {
    let meta = txcript::common::Meta {
        id: "hermes-fixpoint".into(),
        timestamp: ts("2026-01-02T03:04:05Z"),
        cwd: Some("/repo".into()),
        git_branch: Some("feat/demo".into()),
        title: Some("Hermes fixpoint".into()),
        cli_version: None,
        model: Some("gpt-5.6-sol".into()),
    };
    Transcript::new(
        meta,
        vec![
            txcript::common::Message {
                role: txcript::common::Role::User,
                content: vec![txcript::common::Block::Text {
                    text: "read it".into(),
                }],
                timestamp: ts("2026-01-02T03:04:06Z"),
                model: None,
                stop_reason: None,
                usage: None,
            },
            txcript::common::Message {
                role: txcript::common::Role::Assistant,
                content: vec![
                    txcript::common::Block::Thinking {
                        text: "I should inspect it.".into(),
                        signature: None,
                        encrypted: Some("opaque-reasoning".into()),
                    },
                    txcript::common::Block::Text {
                        text: "Inspecting.".into(),
                    },
                    txcript::common::Block::ToolUse {
                        id: "call-1".into(),
                        tool: txcript::common::Tool::Read {
                            file_path: "/repo/README.md".into(),
                            offset: Some(1),
                            limit: Some(20),
                        },
                    },
                ],
                timestamp: ts("2026-01-02T03:04:07Z"),
                model: Some("gpt-5.6-sol".into()),
                stop_reason: Some(txcript::common::StopReason::ToolUse),
                usage: None,
            },
            txcript::common::Message {
                role: txcript::common::Role::User,
                content: vec![txcript::common::Block::ToolResult {
                    tool_use_id: "call-1".into(),
                    content: txcript::common::ToolOutput::Json(json!({"content": "demo"})),
                    is_error: false,
                }],
                timestamp: ts("2026-01-02T03:04:08Z"),
                model: None,
                stop_reason: None,
                usage: None,
            },
            txcript::common::Message {
                role: txcript::common::Role::Assistant,
                content: vec![txcript::common::Block::Text {
                    text: "Done.".into(),
                }],
                timestamp: ts("2026-01-02T03:04:09Z"),
                model: Some("gpt-5.6-sol".into()),
                stop_reason: Some(txcript::common::StopReason::EndTurn),
                usage: None,
            },
        ],
    )
}

#[test]
fn codec_fixpoint_through_common_loses_nothing() {
    let common = sample_common();
    let native = hermes::Hermes::from_common(&common).unwrap();
    let round = hermes::Hermes::to_common(&native).unwrap();
    assert_eq!(round, common);
}

#[test]
fn from_common_is_deterministic() {
    let common = sample_common();
    let first = hermes::Hermes::from_common(&common).unwrap();
    let second = hermes::Hermes::from_common(&common).unwrap();
    assert_eq!(
        hermes::Hermes::to_text(&first).unwrap(),
        hermes::Hermes::to_text(&second).unwrap()
    );
}

#[test]
fn from_common_with_empty_id_is_deterministic() {
    let mut common = sample_common();
    common.meta.id.clear();
    let first = hermes::Hermes::from_common(&common).unwrap();
    let second = hermes::Hermes::from_common(&common).unwrap();
    assert_eq!(first.body, second.body);
    assert_eq!(first.meta.id, second.meta.id);
    assert!(!first.meta.id.is_empty());
}

#[test]
fn missing_or_invalid_started_at_uses_stable_epoch() {
    for text in [
        r#"{"id":"missing","messages":[]}"#,
        r#"{"id":"invalid","started_at":"nope","messages":[]}"#,
    ] {
        let first = hermes::Hermes::from_text(text).unwrap();
        let second = hermes::Hermes::from_text(text).unwrap();
        assert_eq!(first.meta.timestamp, second.meta.timestamp);
        assert_eq!(first.meta.timestamp, DateTime::<Utc>::UNIX_EPOCH);
    }
}

#[test]
fn encrypted_reasoning_survives_without_plaintext() {
    let text = json!({
        "id": "encrypted-only",
        "started_at": 1_768_000_000.0,
        "messages": [{
            "role": "assistant",
            "content": "",
            "codex_reasoning_items": "[{\"type\":\"reasoning\",\"encrypted_content\":\"opaque\"}]",
            "timestamp": 1_768_000_001.0,
            "active": 1
        }]
    })
    .to_string();
    let native = hermes::Hermes::from_text(&text).unwrap();
    let common = hermes::Hermes::to_common(&native).unwrap();
    assert!(matches!(
        &common.body[0].content[0],
        txcript::common::Block::Thinking {
            text,
            encrypted: Some(value),
            ..
        } if text.is_empty() && value.contains("encrypted_content")
    ));
}

#[test]
fn malformed_tool_arguments_remain_raw() {
    let text = json!({
        "id": "malformed-args",
        "started_at": 1_768_000_000.0,
        "messages": [{
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call-raw",
                "type": "function",
                "function": {"name": "read_file", "arguments": "not-json"}
            }],
            "timestamp": 1_768_000_001.0,
            "active": 1
        }]
    })
    .to_string();
    let native = hermes::Hermes::from_text(&text).unwrap();
    let common = hermes::Hermes::to_common(&native).unwrap();
    assert!(matches!(
        &common.body[0].content[0],
        txcript::common::Block::ToolUse {
            tool: txcript::common::Tool::Raw { tool_name, input },
            ..
        } if tool_name == "read_file" && input == "not-json"
    ));
}

#[test]
fn multimodal_content_and_parallel_tool_count_survive() {
    let text = json!({
        "id": "multimodal",
        "started_at": 1_768_000_000.0,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "inspect this"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,aGVsbG8="}}
            ],
            "timestamp": 1_768_000_001.0,
            "active": 1
        }]
    })
    .to_string();
    let native = hermes::Hermes::from_text(&text).unwrap();
    let common = hermes::Hermes::to_common(&native).unwrap();
    assert!(
        matches!(&common.body[0].content[0], txcript::common::Block::Text { text } if text == "inspect this")
    );
    assert!(matches!(
        &common.body[0].content[1],
        txcript::common::Block::Image { source }
            if source.media_type == "image/png" && source.data == "aGVsbG8="
    ));
    let round = hermes::Hermes::to_common(&hermes::Hermes::from_common(&common).unwrap()).unwrap();
    assert_eq!(round, common);

    let mut with_calls = common;
    with_calls.body.push(txcript::common::Message {
        role: txcript::common::Role::Assistant,
        content: vec![
            txcript::common::Block::ToolUse {
                id: "call-a".into(),
                tool: txcript::common::Tool::Raw {
                    tool_name: "one".into(),
                    input: json!({}),
                },
            },
            txcript::common::Block::ToolUse {
                id: "call-b".into(),
                tool: txcript::common::Tool::Raw {
                    tool_name: "two".into(),
                    input: json!({}),
                },
            },
        ],
        timestamp: ts("2026-01-09T23:06:42Z"),
        model: None,
        stop_reason: Some(txcript::common::StopReason::ToolUse),
        usage: None,
    });
    let native = hermes::Hermes::from_common(&with_calls).unwrap();
    assert_eq!(native.body["tool_call_count"], 2);
}

#[test]
#[cfg(feature = "hermes")]
#[allow(clippy::too_many_lines)]
fn sqlite_store_discovers_and_loads_export_shape_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
             id TEXT PRIMARY KEY,
             source TEXT NOT NULL,
             model TEXT,
             started_at REAL NOT NULL,
             cwd TEXT,
             git_branch TEXT,
             title TEXT,
             archived INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE messages (
             id INTEGER PRIMARY KEY,
             session_id TEXT NOT NULL,
             role TEXT NOT NULL,
             content TEXT,
             tool_call_id TEXT,
             tool_calls TEXT,
             tool_name TEXT,
             timestamp REAL NOT NULL,
             active INTEGER NOT NULL DEFAULT 1,
             compacted INTEGER NOT NULL DEFAULT 0
         );
         INSERT INTO sessions VALUES (
             'hermes-db-1', 'cli', 'gpt-5.6-sol', 1768000000.0,
             '/repo', 'main', 'Database demo', 0
         );
         INSERT INTO messages VALUES (
             1, 'hermes-db-1', 'user', 'hello', NULL, NULL, NULL,
             1768000003.0, 1, 0
         );
         INSERT INTO messages VALUES (
             2, 'hermes-db-1', 'assistant', '', NULL,
             '[{\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"/repo/README.md\\\"}\"}}]',
             NULL, 1768000001.0, 1, 0
         );
         INSERT INTO messages VALUES (
             3, 'hermes-db-1', 'tool', '{\"content\":\"demo\"}', 'call-1',
             NULL, 'read_file', 1768000002.0, 1, 0
         );
         INSERT INTO messages VALUES (
             4, 'hermes-db-1', 'assistant', '', NULL, 'not-json', NULL,
             1768000004.0, 1, 0
         );
         INSERT INTO messages VALUES (
             5, 'hermes-db-1', 'user', 'rewound secret', NULL, NULL, NULL,
             1768000005.0, 0, 0
         );",
    )
    .unwrap();
    conn.execute(
        "UPDATE messages SET content = ?1 WHERE id = 1",
        ["\0json:[{\"type\":\"text\",\"text\":\"hello\"}]"],
    )
    .unwrap();
    drop(conn);

    let before = std::fs::read(&db).unwrap();
    let store = hermes::HermesStore::new(&db);
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].meta.id, "hermes-db-1");
    assert_eq!(found[0].meta.cwd.as_deref(), Some("/repo"));
    assert_eq!(found[0].meta.title.as_deref(), Some("Database demo"));

    let loaded = store.load(&found[0].reference).unwrap();
    assert_eq!(
        loaded.body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert!(loaded.body["messages"][0]["content"].is_array());
    assert!(loaded.body["messages"][1]["tool_calls"].is_array());
    assert_eq!(
        loaded.body["messages"][2]["content"],
        "{\"content\":\"demo\"}"
    );
    assert_eq!(loaded.body["messages"][3]["tool_calls"], json!([]));
    let common = hermes::Hermes::to_common(&loaded).unwrap();
    assert_eq!(common.body.len(), 3);
    assert!(matches!(
        &common.body[1].content[0],
        txcript::common::Block::ToolUse { id, .. } if id == "call-1"
    ));

    let error = store.save(&loaded).unwrap_err().to_string();
    assert!(error.contains("read-only"));
    assert!(store.delete(&found[0].reference).is_err());
    let first_fingerprint =
        store.fingerprints(&[found[0].reference.clone()]).unwrap()[&found[0].reference].clone();
    assert_eq!(std::fs::read(&db).unwrap(), before);

    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE messages SET content = 'edited' WHERE id = 2", [])
        .unwrap();
    drop(conn);
    let second_fingerprint =
        store.fingerprints(&[found[0].reference.clone()]).unwrap()[&found[0].reference].clone();
    assert_ne!(first_fingerprint, second_fingerprint);

    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("UPDATE messages SET active = 0 WHERE id = 2", [])
        .unwrap();
    drop(conn);
    let third_fingerprint =
        store.fingerprints(&[found[0].reference.clone()]).unwrap()[&found[0].reference].clone();
    assert_ne!(second_fingerprint, third_fingerprint);

    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE sessions SET title = 'Renamed' WHERE id = 'hermes-db-1'",
        [],
    )
    .unwrap();
    drop(conn);
    let fourth_fingerprint =
        store.fingerprints(&[found[0].reference.clone()]).unwrap()[&found[0].reference].clone();
    assert_ne!(third_fingerprint, fourth_fingerprint);
}
