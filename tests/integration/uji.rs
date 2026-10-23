#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Integration tests for uji's session database and its JSON text form.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
#[cfg(feature = "uji")]
use txcript::Store;
use txcript::common::{Block, Relation, Role, Tool, ToolOutput};
use txcript::harness::uji;
use txcript::{Codec, TextCodec, Transcript};

const ID: &str = "019a0000-0000-7000-8000-000000000001";

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn row(seq: u64, time: i64, data: &Value) -> Value {
    json!({ "id": format!("m{seq}"), "seq": seq, "time_created": time, "data": data })
}

fn native() -> Value {
    json!({
        "id": ID,
        "title": "fix the bug",
        "directory": "/repo",
        "time_created": 1_768_000_000_000_i64,
        "time_updated": 1_768_000_009_000_i64,
        "parent": null,
        "messages": [
            row(1, 1_768_000_001_000, &json!({
                "type": "user",
                "text": "fix it",
                "images": [{ "media_type": "image/png", "data": "iVBO", "name": "shot.png", "width": 2, "height": 2 }]
            })),
            row(2, 1_768_000_002_000, &json!({
                "type": "assistant",
                "text": "Looking.",
                "reasoning": "plan",
                "tool_calls": [
                    { "id": "c1", "name": "read_file", "arguments": "{\"path\":\"/repo/a.rs\",\"offset\":3}" },
                    { "id": "c2", "name": "run_command", "arguments": "{\"command\":\"cargo test\",\"timeout\":30}" },
                    { "id": "c3", "name": "web_search", "arguments": "{\"query\":\"relay\"}" }
                ]
            })),
            row(3, 1_768_000_003_000, &json!({ "type": "tool", "tool_call_id": "c1", "name": "read_file", "content": "    3| fn a()" })),
            row(4, 1_768_000_004_000, &json!({ "type": "tool", "tool_call_id": "c2", "name": "run_command", "content": "error: command timed out after 30s" })),
            row(5, 1_768_000_005_000, &json!({ "type": "tool", "tool_call_id": "c3", "name": "web_search", "content": "results" })),
            row(6, 1_768_000_006_000, &json!({ "type": "shell", "command": "ls", "output": "a.rs", "code": 0 })),
            row(7, 1_768_000_007_000, &json!({
                "type": "assistant",
                "text": "done",
                "reasoning": "check",
                "tool_calls": [],
                "replay": {
                    "api": "anthropic",
                    "model": "claude-opus-4-8",
                    "content": [
                        { "type": "thinking", "thinking": "check", "signature": "sig" },
                        { "type": "text", "text": "done" }
                    ]
                }
            })),
            row(8, 1_768_000_008_000, &json!({ "type": "compaction", "summary": "sum", "through": 7, "files": [] })),
            row(9, 1_768_000_009_000, &json!({ "type": "error", "text": "interrupted" }))
        ]
    })
}

fn parsed() -> Transcript<uji::Uji> {
    uji::Uji::from_text(&native().to_string()).unwrap()
}

#[test]
fn native_session_maps_messages_tools_and_reasoning() {
    let common = uji::Uji::to_common(&parsed()).unwrap();
    assert_eq!(common.meta.id, ID);
    assert_eq!(common.meta.cwd.as_deref(), Some("/repo"));
    assert_eq!(common.meta.title.as_deref(), Some("fix the bug"));
    assert_eq!(common.meta.timestamp, ts("2026-01-09T23:06:40Z"));
    assert!(common.meta.lineage.is_none());

    let body = &common.body;
    assert_eq!(body.len(), 7, "shell and error entries carry no turn");

    assert_eq!(body[0].role, Role::User);
    assert!(matches!(&body[0].content[0], Block::Text { text } if text == "fix it"));
    assert!(
        matches!(&body[0].content[1], Block::Image { source } if source.media_type == "image/png")
    );

    assert_eq!(body[1].role, Role::Assistant);
    assert!(matches!(&body[1].content[0], Block::Thinking { text, .. } if text == "plan"));
    assert!(matches!(&body[1].content[1], Block::Text { text } if text == "Looking."));
    assert!(matches!(
        &body[1].content[2],
        Block::ToolUse { id, tool: Tool::Read { file_path, offset: Some(3), limit: None } }
            if id == "c1" && file_path == "/repo/a.rs"
    ));
    assert!(matches!(
        &body[1].content[3],
        Block::ToolUse { tool: Tool::Bash { command, timeout_ms: Some(30_000), .. }, .. }
            if command == "cargo test"
    ));
    assert!(matches!(
        &body[1].content[4],
        Block::ToolUse { tool: Tool::Raw { tool_name, .. }, .. } if tool_name == "web_search"
    ));

    assert!(matches!(
        &body[2].content[0],
        Block::ToolResult { tool_use_id, is_error: false, content: ToolOutput::Text(text) }
            if tool_use_id == "c1" && text == "    3| fn a()"
    ));
    assert!(matches!(
        &body[3].content[0],
        Block::ToolResult { is_error: true, .. }
    ));

    assert_eq!(body[5].model.as_deref(), Some("claude-opus-4-8"));
    assert!(matches!(
        &body[5].content[0],
        Block::Thinking { text, signature: Some(signature), .. } if text == "check" && signature == "sig"
    ));
    assert!(matches!(&body[6].content[0], Block::Text { text } if text == "sum"));
}

#[test]
fn codec_fixpoint_through_common_loses_nothing() {
    let common = uji::Uji::to_common(&parsed()).unwrap();
    let back = uji::Uji::to_common(&uji::Uji::from_common(&common).unwrap()).unwrap();
    assert_eq!(back, common);
}

#[test]
fn from_common_is_deterministic() {
    let common = uji::Uji::to_common(&parsed()).unwrap();
    let first = uji::Uji::from_common(&common).unwrap();
    let second = uji::Uji::from_common(&common).unwrap();
    assert_eq!(first.body, second.body);
}

#[test]
fn from_common_writes_uji_tool_names_and_rebuilds_the_replay() {
    let common = uji::Uji::to_common(&parsed()).unwrap();
    let body = uji::Uji::from_common(&common).unwrap().body;
    let rows = body["messages"].as_array().unwrap();
    let calls = rows[1]["data"]["tool_calls"].as_array().unwrap();
    assert_eq!(calls[0]["name"], "read_file");
    assert_eq!(
        serde_json::from_str::<Value>(calls[0]["arguments"].as_str().unwrap()).unwrap(),
        json!({ "path": "/repo/a.rs", "offset": 3 })
    );
    assert_eq!(calls[1]["name"], "run_command");
    assert_eq!(
        serde_json::from_str::<Value>(calls[1]["arguments"].as_str().unwrap()).unwrap(),
        json!({ "command": "cargo test", "timeout": 30 })
    );
    assert_eq!(rows[2]["data"]["name"], "read_file");
    let replay = &rows[5]["data"]["replay"];
    assert_eq!(replay["api"], "anthropic");
    assert_eq!(replay["model"], "claude-opus-4-8");
    assert_eq!(replay["content"][0]["signature"], "sig");
}

#[test]
fn responses_reasoning_items_survive_as_encrypted_thinking() {
    let item = json!({ "id": "rs_1", "type": "reasoning", "summary": [{ "type": "summary_text", "text": "think" }], "encrypted_content": "enc" });
    let body = json!({
        "id": ID,
        "title": "untitled",
        "directory": "/repo",
        "time_created": 1_768_000_000_000_i64,
        "time_updated": 1_768_000_000_000_i64,
        "messages": [
            row(1, 1_768_000_001_000, &json!({ "type": "user", "text": "hi" })),
            row(2, 1_768_000_002_000, &json!({
                "type": "assistant",
                "text": "hello",
                "reasoning": "think",
                "tool_calls": [],
                "replay": {
                    "api": "responses",
                    "model": "gpt-5.5",
                    "items": [{ "kind": "reasoning", "item": item }, { "kind": "message", "id": "msg_1" }]
                }
            }))
        ]
    });
    let common = uji::Uji::to_common(&uji::Uji::from_text(&body.to_string()).unwrap()).unwrap();
    assert!(
        common.meta.title.is_none(),
        "uji's placeholder title is not a title"
    );
    let Block::Thinking {
        text, encrypted, ..
    } = &common.body[1].content[0]
    else {
        panic!("expected thinking");
    };
    assert_eq!(text, "think");
    assert_eq!(
        serde_json::from_str::<Value>(encrypted.as_deref().unwrap()).unwrap(),
        item
    );
    let rebuilt = uji::Uji::from_common(&common).unwrap().body;
    let replay = &rebuilt["messages"][1]["data"]["replay"];
    assert_eq!(replay["api"], "responses");
    assert_eq!(replay["items"][0]["item"], item);
}

#[test]
fn a_session_started_by_another_records_its_parent() {
    let mut body = native();
    body["parent"] = json!("019a0000-0000-7000-8000-0000000000aa");
    let meta = uji::Uji::from_text(&body.to_string()).unwrap().meta;
    let lineage = meta.lineage.unwrap();
    assert_eq!(lineage.parent, "019a0000-0000-7000-8000-0000000000aa");
    assert_eq!(lineage.relation, Relation::Spawn);
}

#[cfg(feature = "uji")]
fn uji_schema(path: &std::path::Path) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT NOT NULL, directory TEXT NOT NULL,
             time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
             parent TEXT REFERENCES sessions(id) ON DELETE CASCADE);
         CREATE TABLE messages (id TEXT PRIMARY KEY,
             session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
             seq INTEGER NOT NULL, type TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);
         CREATE UNIQUE INDEX messages_session_seq ON messages(session_id, seq);
         CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         PRAGMA user_version = 2;",
    )
    .unwrap();
    conn
}

#[cfg(feature = "uji")]
fn uji_database(path: &std::path::Path) {
    let conn = uji_schema(path);
    let body = native();
    conn.execute(
        "INSERT INTO sessions (id, title, directory, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![ID, "fix the bug", "/repo", 1_768_000_000_000_i64, 1_768_000_009_000_i64],
    )
    .unwrap();
    for message in body["messages"].as_array().unwrap() {
        conn.execute(
            "INSERT INTO messages (id, session_id, seq, type, time_created, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                message["id"].as_str().unwrap(),
                ID,
                message["seq"].as_i64().unwrap(),
                message["data"]["type"].as_str().unwrap(),
                message["time_created"].as_i64().unwrap(),
                message["data"].to_string()
            ],
        )
        .unwrap();
    }
}

#[cfg(feature = "uji")]
#[test]
fn store_discovers_loads_and_round_trips_through_a_fresh_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("uji.db");
    uji_database(&db);
    let store = uji::UjiStore::new(&db);

    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].reference, ID);
    assert_eq!(found[0].meta.title.as_deref(), Some("fix the bug"));
    assert_eq!(found[0].meta.cwd.as_deref(), Some("/repo"));

    let loaded = store.load(&ID.to_string()).unwrap();
    let empty = dir.path().join("copy.db");
    uji_schema(&empty);
    let copy = uji::UjiStore::new(&empty);
    let saved = copy.save(&loaded).unwrap();
    assert_eq!(saved.id, ID);
    let reloaded = copy.load(&saved.reference).unwrap();
    assert_eq!(reloaded.body["messages"], loaded.body["messages"]);
    assert_eq!(
        uji::Uji::to_common(&reloaded).unwrap(),
        uji::Uji::to_common(&loaded).unwrap()
    );
}

#[cfg(feature = "uji")]
#[test]
fn saving_needs_the_database_uji_creates() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("uji.db");
    let store = uji::UjiStore::new(&db);
    let common = uji::Uji::to_common(&parsed()).unwrap();
    let error = store
        .save(&uji::Uji::from_common(&common).unwrap())
        .unwrap_err();
    assert!(error.to_string().contains("start uji once"));
    assert!(!db.exists(), "txcript never creates uji's database");
}

#[cfg(feature = "uji")]
#[test]
fn saving_replaces_ids_uji_cannot_resume_and_delete_removes_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("uji.db");
    uji_database(&db);
    let store = uji::UjiStore::new(&db);
    let mut common = uji::Uji::to_common(&parsed()).unwrap();
    common.meta.id = "ses_not_a_uuid".to_string();
    let saved = store
        .save(&uji::Uji::from_common(&common).unwrap())
        .unwrap();
    assert!(uuid::Uuid::try_parse(&saved.id).is_ok());
    assert_eq!(store.discover().unwrap().len(), 2);

    let before = store.fingerprints(std::slice::from_ref(&saved.id)).unwrap();
    assert_ne!(before[&saved.id], "");

    store.delete(&saved.id).unwrap();
    assert_eq!(store.discover().unwrap().len(), 1);
    assert!(store.delete(&saved.id).is_err());
}
