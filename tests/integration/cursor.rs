#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Integration tests for the Cursor `SQLite` chat-store harness.

use chrono::{DateTime, Utc};
use serde_json::json;
#[cfg(feature = "opencode")]
use txcript::Store;
use txcript::common;
use txcript::harness::cursor;
use txcript::{Codec, Common, TextCodec, Transcript};

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn sample_common() -> Transcript<Common> {
    let meta = common::Meta {
        id: "sess-1".into(),
        timestamp: ts("2026-01-02T03:04:05.000Z"),
        cwd: Some("/repo".into()),
        git_branch: None,
        title: Some("Cursor demo".into()),
        cli_version: None,
        model: Some("composer-2.5-fast".into()),
    };
    Transcript::new(
        meta,
        vec![
            common::Message {
                role: common::Role::User,
                content: vec![common::Block::Text {
                    text: "read the file".into(),
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
                        text: String::new(),
                        signature: None,
                        encrypted: Some("opaque-reasoning".into()),
                    },
                    common::Block::Text {
                        text: "I'll inspect it.".into(),
                    },
                    common::Block::ToolUse {
                        id: "tool-1".into(),
                        tool: common::Tool::Read {
                            file_path: "/repo/README.md".into(),
                            offset: None,
                            limit: None,
                        },
                    },
                ],
                timestamp: ts("2026-01-02T03:04:07.000Z"),
                model: Some("composer-2.5-fast".into()),
                stop_reason: None,
                usage: None,
            },
            common::Message {
                role: common::Role::User,
                content: vec![common::Block::ToolResult {
                    tool_use_id: "tool-1".into(),
                    content: common::ToolOutput::Text("contents".into()),
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
                    text: "README loaded.".into(),
                }],
                timestamp: ts("2026-01-02T03:04:09.000Z"),
                model: Some("composer-2.5-fast".into()),
                stop_reason: None,
                usage: None,
            },
        ],
    )
}

#[test]
#[cfg(feature = "opencode")]
fn save_uses_cursor_chat_store_path() {
    let dir = tempfile::tempdir().unwrap();
    let common = sample_common();
    let native = cursor::Cursor::from_common(&common).unwrap();
    let saved = cursor::CursorStore::new(dir.path()).save(&native).unwrap();

    assert_eq!(saved.id, "sess-1");
    assert_eq!(
        saved.reference,
        dir.path()
            .join("6530f9eb448d96e7552a3c3a29b6cd2b")
            .join("sess-1")
            .join("store.db")
    );
    assert!(saved.reference.is_file());
    assert!(
        saved
            .reference
            .parent()
            .unwrap()
            .join("meta.json")
            .is_file()
    );
    assert!(
        saved
            .reference
            .parent()
            .unwrap()
            .join("prompt_history.json")
            .is_file()
    );
}

#[test]
#[cfg(feature = "opencode")]
fn discover_and_load_extract_cursor_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let common = sample_common();
    let native = cursor::Cursor::from_common(&common).unwrap();
    cursor::CursorStore::new(dir.path()).save(&native).unwrap();

    let found = cursor::CursorStore::new(dir.path()).discover().unwrap();
    assert_eq!(found.len(), 1);
    let meta = &found[0].meta;
    assert_eq!(meta.id, "sess-1");
    assert_eq!(meta.cwd.as_deref(), Some("/repo"));
    assert_eq!(meta.title.as_deref(), Some("Cursor demo"));
    assert_eq!(meta.model.as_deref(), Some("composer-2.5-fast"));

    let loaded = cursor::CursorStore::new(dir.path())
        .load(&found[0].reference)
        .unwrap();
    let round = cursor::Cursor::to_common(&loaded).unwrap();
    assert_eq!(round.body.len(), 4);
}

#[test]
fn to_common_preserves_cursor_tool_calls_results_and_reasoning() {
    let common = sample_common();
    let native = cursor::Cursor::from_common(&common).unwrap();
    let round = cursor::Cursor::to_common(&native).unwrap();

    assert_eq!(round.body[0].content, common.body[0].content);
    assert!(matches!(
        &round.body[1].content[0],
        common::Block::Thinking {
            encrypted: Some(data),
            ..
        } if data == "opaque-reasoning"
    ));
    assert!(matches!(
        &round.body[1].content[2],
        common::Block::ToolUse {
            id,
            tool: common::Tool::Read { file_path, .. },
        } if id == "tool-1" && file_path == "/repo/README.md"
    ));
    assert!(matches!(
        &round.body[2].content[0],
        common::Block::ToolResult {
            tool_use_id,
            content: common::ToolOutput::Text(text),
            is_error: false,
        } if tool_use_id == "tool-1" && text == "contents"
    ));
}

#[test]
fn text_codec_round_trips_native_export() {
    let native = cursor::Cursor::from_common(&sample_common()).unwrap();
    let text = cursor::Cursor::to_text(&native).unwrap();
    let round = cursor::Cursor::from_text(&text).unwrap();
    assert_eq!(round.body, native.body);
    assert_eq!(
        cursor::Cursor::to_common(&round).unwrap().body,
        sample_common().body
    );
}

#[test]
fn from_common_writes_cursor_resume_state_turns() {
    let native = cursor::Cursor::from_common(&sample_common()).unwrap();
    let root = latest_root_blob(&native.body);
    let turn_refs = len_fields(&root.data, 8);

    assert_eq!(turn_refs.len(), 1);
    let prompt_refs = len_fields(&root.data, 1);
    assert_eq!(prompt_refs.len(), 4);
    for prompt_ref in &prompt_refs {
        let prompt_id = hex_encode_test(prompt_ref);
        let blob = native
            .body
            .blobs
            .iter()
            .find(|blob| blob.id == prompt_id)
            .expect("prompt json blob exists");
        let obj: serde_json::Value = serde_json::from_slice(&blob.data).expect("prompt json");
        assert!(
            obj.get("role")
                .and_then(serde_json::Value::as_str)
                .is_some()
        );
    }

    let turn_id = hex_encode_test(&turn_refs[0]);
    let turn_blob = native
        .body
        .blobs
        .iter()
        .find(|blob| blob.id == turn_id)
        .expect("turn structure blob exists");
    let agent_turns = len_fields(&turn_blob.data, 1);
    assert_eq!(agent_turns.len(), 1);

    let user_refs = len_fields(&agent_turns[0], 1);
    let step_refs = len_fields(&agent_turns[0], 2);
    assert_eq!(user_refs.len(), 1);
    assert!(step_refs.len() >= 3);

    let user_id = hex_encode_test(&user_refs[0]);
    let user_blob = native
        .body
        .blobs
        .iter()
        .find(|blob| blob.id == user_id)
        .expect("user message blob exists");
    assert_eq!(string_fields(&user_blob.data, 17).len(), 1);
    assert!(!varint_fields(&user_blob.data, 25).is_empty());
    let mut read_tool_call = None;
    for step_ref in step_refs {
        let step_id = hex_encode_test(&step_ref);
        let step_blob = native
            .body
            .blobs
            .iter()
            .find(|blob| blob.id == step_id)
            .unwrap_or_else(|| panic!("step blob {step_id} exists"));
        if let Some(tool_call) = len_fields(&step_blob.data, 2).into_iter().next() {
            assert_eq!(string_fields(&tool_call, 57), vec!["tool-1".to_string()]);
            read_tool_call = len_fields(&tool_call, 8).into_iter().next();
        }
    }

    let read_tool_call = read_tool_call.expect("read tool call step exists");
    let read_args = len_fields(&read_tool_call, 1);
    let read_result = len_fields(&read_tool_call, 2);
    assert_eq!(read_args.len(), 1);
    assert_eq!(read_result.len(), 1);
    assert_eq!(
        string_fields(&read_args[0], 1),
        vec!["/repo/README.md".to_string()]
    );

    let read_success = len_fields(&read_result[0], 1);
    assert_eq!(read_success.len(), 1);
    assert_eq!(
        string_fields(&read_success[0], 1),
        vec!["contents".to_string()]
    );
    assert_eq!(
        string_fields(&read_success[0], 7),
        vec!["/repo/README.md".to_string()]
    );
}

#[test]
fn from_common_writes_cursor_shell_tool_calls() {
    let mut common = sample_common();
    common.body = vec![
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::Text {
                text: "check git".into(),
            }],
            timestamp: ts("2026-01-02T03:04:06.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::ToolUse {
                id: "shell-1".into(),
                tool: common::Tool::Bash {
                    command: "git status --short".into(),
                    workdir: Some("/repo".into()),
                    timeout_ms: None,
                    description: Some("Show status".into()),
                    run_in_background: false,
                },
            }],
            timestamp: ts("2026-01-02T03:04:07.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::ToolResult {
                tool_use_id: "shell-1".into(),
                content: common::ToolOutput::Text(" M src/main.rs".into()),
                is_error: false,
            }],
            timestamp: ts("2026-01-02T03:04:08.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
    ];

    let native = cursor::Cursor::from_common(&common).unwrap();
    let tool_call = first_tool_call(&native.body).expect("shell tool call");
    let shell = len_fields(&tool_call, 1);
    assert_eq!(shell.len(), 1);
    assert_eq!(string_fields(&tool_call, 57), vec!["shell-1".to_string()]);

    let args = len_fields(&shell[0], 1);
    let result = len_fields(&shell[0], 2);
    assert_eq!(args.len(), 1);
    assert_eq!(result.len(), 1);
    assert_eq!(
        string_fields(&args[0], 1),
        vec!["git status --short".to_string()]
    );
    assert_eq!(string_fields(&args[0], 2), vec!["/repo".to_string()]);
    assert_eq!(string_fields(&args[0], 4), vec!["shell-1".to_string()]);

    let success = len_fields(&result[0], 1);
    assert_eq!(success.len(), 1);
    assert_eq!(
        string_fields(&success[0], 5),
        vec![" M src/main.rs".to_string()]
    );
}

#[test]
fn from_common_writes_cursor_edit_tool_calls_with_diff_payload() {
    let mut common = sample_common();
    common.body = vec![
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::Text {
                text: "update readme".into(),
            }],
            timestamp: ts("2026-01-02T03:04:06.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::ToolUse {
                id: "edit-1".into(),
                tool: common::Tool::Edit {
                    file_path: "/repo/README.md".into(),
                    old_string: "old title\n".into(),
                    new_string: "new title\n".into(),
                    replace_all: false,
                },
            }],
            timestamp: ts("2026-01-02T03:04:07.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::ToolResult {
                tool_use_id: "edit-1".into(),
                content: common::ToolOutput::Text(
                    "The file /repo/README.md has been updated successfully.".into(),
                ),
                is_error: false,
            }],
            timestamp: ts("2026-01-02T03:04:08.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
    ];

    let native = cursor::Cursor::from_common(&common).unwrap();
    let tool_call = first_tool_call(&native.body).expect("edit tool call");
    let edit = len_fields(&tool_call, 12);
    assert_eq!(edit.len(), 1);
    assert_eq!(string_fields(&tool_call, 57), vec!["edit-1".to_string()]);

    let args = len_fields(&edit[0], 1);
    let result = len_fields(&edit[0], 2);
    assert_eq!(args.len(), 1);
    assert_eq!(result.len(), 1);
    assert_eq!(
        string_fields(&args[0], 1),
        vec!["/repo/README.md".to_string()]
    );
    assert_eq!(string_fields(&args[0], 6), vec!["new title\n".to_string()]);

    let success = len_fields(&result[0], 1);
    assert_eq!(success.len(), 1);
    assert_eq!(
        string_fields(&success[0], 1),
        vec!["/repo/README.md".to_string()]
    );
    assert_eq!(varint_fields(&success[0], 3), vec![1]);
    assert_eq!(varint_fields(&success[0], 4), vec![1]);

    let diff = string_fields(&success[0], 5)
        .into_iter()
        .next()
        .expect("diff string");
    assert!(diff.contains("--- a//repo/README.md"));
    assert!(diff.contains("+++ b//repo/README.md"));
    assert!(diff.contains("-old title"));
    assert!(diff.contains("+new title"));
    assert!(!diff.contains("updated successfully"));
    assert_eq!(
        string_fields(&success[0], 6),
        vec!["old title\n".to_string()]
    );
    assert_eq!(
        string_fields(&success[0], 7),
        vec!["new title\n".to_string()]
    );
}

#[test]
#[cfg(feature = "opencode")]
fn store_round_trip_preserves_non_json_blobs() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let mut native = cursor::Cursor::from_common(&sample_common()).unwrap();
    native.body.blobs.insert(
        0,
        cursor::CursorBlob {
            id: "binary-internal-state".into(),
            data: vec![0, 159, 146, 150, 1, 2, 3],
        },
    );

    let saved = cursor::CursorStore::new(src.path()).save(&native).unwrap();
    let loaded = cursor::CursorStore::new(src.path())
        .load(&saved.reference)
        .unwrap();
    assert_eq!(loaded.body.blobs[0].data, vec![0, 159, 146, 150, 1, 2, 3]);

    let copied = cursor::CursorStore::new(dst.path()).save(&loaded).unwrap();
    let reloaded = cursor::CursorStore::new(dst.path())
        .load(&copied.reference)
        .unwrap();
    assert_eq!(reloaded.body, loaded.body);
}

#[test]
fn parses_existing_cursor_message_shapes() {
    let body = cursor::CursorDb {
        blobs: vec![
            cursor::CursorBlob {
                id: "system".into(),
                data: serde_json::to_vec(&json!({
                    "role": "system",
                    "content": "You are Cursor."
                }))
                .unwrap(),
            },
            cursor::CursorBlob {
                id: "context".into(),
                data: serde_json::to_vec(&json!({
                    "role": "user",
                    "content": "<user_info>\nWorkspace Path: /Users/me/repo\n</user_info>"
                }))
                .unwrap(),
            },
            cursor::CursorBlob {
                id: "user".into(),
                data: serde_json::to_vec(&json!({
                    "role": "user",
                    "content": [{"type": "text", "text": "<user_query>\nhello!\n</user_query>"}]
                }))
                .unwrap(),
            },
            cursor::CursorBlob {
                id: "assistant".into(),
                data: serde_json::to_vec(&json!({
                    "role": "assistant",
                    "content": [{
                        "type": "tool-call",
                        "toolCallId": "tool-1",
                        "toolName": "StrReplace",
                        "args": {
                            "path": "/repo/a.rs",
                            "old_string": "old",
                            "new_string": "new"
                        }
                    }],
                    "providerOptions": {"cursor": {"modelName": "composer-2.5"}}
                }))
                .unwrap(),
            },
        ],
        meta: Vec::new(),
        session_meta: Some(json!({
            "schemaVersion": 1,
            "createdAtMs": 1_767_337_445_000_i64,
            "hasConversation": true,
            "title": "Hello Agent",
            "updatedAtMs": 1_767_337_445_000_i64
        })),
    };
    let transcript = Transcript::<cursor::Cursor>::new(
        common::Meta {
            id: "sess".into(),
            timestamp: ts("2026-01-02T03:04:05.000Z"),
            cwd: Some("/Users/me/repo".into()),
            git_branch: None,
            title: None,
            cli_version: None,
            model: None,
        },
        body,
    );
    let common = cursor::Cursor::to_common(&transcript).unwrap();
    assert_eq!(common.body.len(), 2);
    assert!(matches!(
        &common.body[1].content[0],
        common::Block::ToolUse {
            tool: common::Tool::Edit { file_path, old_string, new_string, .. },
            ..
        } if file_path == "/repo/a.rs" && old_string == "old" && new_string == "new"
    ));
}

#[test]
fn native_call_ids_are_sanitized_into_common() {
    let body = cursor::CursorDb {
        blobs: vec![cursor::CursorBlob {
            id: "assistant".into(),
            data: serde_json::to_vec(&json!({
                "role": "assistant",
                "content": [{
                    "type": "tool-call",
                    "toolCallId": "call-abc-12\nfc_def_4",
                    "toolName": "ReadFile",
                    "args": { "target_file": "/repo/x.rs" }
                }]
            }))
            .unwrap(),
        }],
        meta: Vec::new(),
        session_meta: Some(json!({"schemaVersion": 1, "hasConversation": true})),
    };
    let transcript = Transcript::<cursor::Cursor>::new(
        common::Meta {
            id: "sess".into(),
            timestamp: ts("2026-01-02T03:04:05.000Z"),
            cwd: None,
            git_branch: None,
            title: None,
            cli_version: None,
            model: None,
        },
        body,
    );
    let common = cursor::Cursor::to_common(&transcript).unwrap();
    let ids: Vec<&str> = common
        .body
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            common::Block::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["call-abc-12_fc_def_4"]);
}

#[test]
fn native_call_ids_are_clamped_to_codex_max() {
    let tool_call_id = format!("call-{}", "x".repeat(80));
    assert_eq!(tool_call_id.len(), 85);
    let body = cursor::CursorDb {
        blobs: vec![cursor::CursorBlob {
            id: "assistant".into(),
            data: serde_json::to_vec(&json!({
                "role": "assistant",
                "content": [{
                    "type": "tool-call",
                    "toolCallId": tool_call_id,
                    "toolName": "ReadFile",
                    "args": { "target_file": "/repo/x.rs" }
                }]
            }))
            .unwrap(),
        }],
        meta: Vec::new(),
        session_meta: Some(json!({"schemaVersion": 1, "hasConversation": true})),
    };
    let transcript = Transcript::<cursor::Cursor>::new(
        common::Meta {
            id: "sess".into(),
            timestamp: ts("2026-01-02T03:04:05.000Z"),
            cwd: None,
            git_branch: None,
            title: None,
            cli_version: None,
            model: None,
        },
        body,
    );
    let common = cursor::Cursor::to_common(&transcript).unwrap();
    let ids: Vec<&str> = common
        .body
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            common::Block::ToolUse { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 1);
    assert!(
        ids[0].len() <= common::TOOL_ID_MAX_LEN,
        "id len {} > {}: {}",
        ids[0].len(),
        common::TOOL_ID_MAX_LEN,
        ids[0]
    );
}

fn latest_root_blob(body: &cursor::CursorDb) -> &cursor::CursorBlob {
    let meta = cursor_meta_json(body);
    let id = meta
        .get("latestRootBlobId")
        .and_then(serde_json::Value::as_str)
        .expect("latest root id");
    body.blobs
        .iter()
        .find(|blob| blob.id == id)
        .expect("latest root blob")
}

fn cursor_meta_json(body: &cursor::CursorDb) -> serde_json::Value {
    let raw = body
        .meta
        .iter()
        .find(|entry| entry.key == "0")
        .expect("cursor meta row")
        .value
        .as_str();
    let decoded = hex_decode_test(raw);
    serde_json::from_slice(&decoded).expect("cursor meta json")
}

fn first_tool_call(body: &cursor::CursorDb) -> Option<Vec<u8>> {
    let root = latest_root_blob(body);
    len_fields(&root.data, 8)
        .into_iter()
        .find_map(|turn_ref| {
            let turn_id = hex_encode_test(&turn_ref);
            match body.blobs.iter().find(|blob| blob.id == turn_id) {
                // A dangling turn ref ends the walk with no result.
                None => Some(None),
                // Otherwise the turn either yields the first call (stop)
                // or has none (keep walking).
                Some(turn_blob) => turn_tool_call(body, turn_blob).map(Some),
            }
        })
        .flatten()
}

fn turn_tool_call(body: &cursor::CursorDb, turn_blob: &cursor::CursorBlob) -> Option<Vec<u8>> {
    len_fields(&turn_blob.data, 1)
        .into_iter()
        .flat_map(|agent_turn| len_fields(&agent_turn, 2))
        .find_map(|step_ref| {
            let step_id = hex_encode_test(&step_ref);
            // A step blob may be absent: it simply offers no tool call.
            body.blobs
                .iter()
                .find(|blob| blob.id == step_id)
                .and_then(|step_blob| len_fields(&step_blob.data, 2).into_iter().next())
        })
}

fn string_fields(data: &[u8], wanted_field: u64) -> Vec<String> {
    len_fields(data, wanted_field)
        .into_iter()
        .map(|bytes| String::from_utf8(bytes).expect("utf-8 field"))
        .collect()
}

fn varint_fields(data: &[u8], wanted_field: u64) -> Vec<u64> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let Some(key) = read_varint(data, &mut i) else {
            break;
        };
        let field = key >> 3;
        match key & 0x07 {
            0 => {
                let Some(value) = read_varint(data, &mut i) else {
                    break;
                };
                if field == wanted_field {
                    out.push(value);
                }
            }
            1 => i = i.saturating_add(8),
            2 => {
                let Some(len) = read_varint(data, &mut i).and_then(|v| usize::try_from(v).ok())
                else {
                    break;
                };
                let Some(end) = i.checked_add(len) else {
                    break;
                };
                if end > data.len() {
                    break;
                }
                i = end;
            }
            5 => i = i.saturating_add(4),
            // Wire types 3/4 (groups) and 6/7 don't occur in this format:
            // the buffer is malformed, stop parsing.
            _ => break,
        }
    }
    out
}

fn len_fields(data: &[u8], wanted_field: u64) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let Some(key) = read_varint(data, &mut i) else {
            break;
        };
        let field = key >> 3;
        match key & 0x07 {
            0 => {
                let _ = read_varint(data, &mut i);
            }
            1 => i = i.saturating_add(8),
            2 => {
                let Some(len) = read_varint(data, &mut i).and_then(|v| usize::try_from(v).ok())
                else {
                    break;
                };
                let Some(end) = i.checked_add(len) else {
                    break;
                };
                if end > data.len() {
                    break;
                }
                let value = data[i..end].to_vec();
                if field == wanted_field {
                    out.push(value);
                }
                i = end;
            }
            5 => i = i.saturating_add(4),
            // Wire types 3/4 (groups) and 6/7 don't occur in this format:
            // the buffer is malformed, stop parsing.
            _ => break,
        }
    }
    out
}

fn read_varint(data: &[u8], i: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        // Ran off the buffer or past 64 bits: not a valid varint.
        if *i >= data.len() || shift >= 64 {
            break None;
        }
        let byte = data[*i];
        *i += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break Some(value);
        }
        shift += 7;
    }
}

fn hex_decode_test(s: &str) -> Vec<u8> {
    assert_eq!(s.len() % 2, 0);
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn hex_encode_test(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}
