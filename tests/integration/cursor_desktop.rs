#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used
)]

//! Integration tests for the Cursor desktop (`state.vscdb`) harness.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use txcript::common;
use txcript::harness::cursor_desktop::{CursorDesktop, DesktopRow, DesktopSession};
use txcript::{Codec, Common, TextCodec, Transcript};

#[cfg(feature = "opencode")]
use txcript::Store;
#[cfg(feature = "opencode")]
use txcript::harness::cursor_desktop::CursorDesktopStore;

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

const CID: &str = "7a21307b-c742-491d-ac90-21fdab1f1746";

fn bubble_key(bubble_id: &str) -> String {
    format!("bubbleId:{CID}:{bubble_id}")
}

/// A native session shaped like a real Cursor 3.16 database: one user
/// bubble, thinking, text, a completed read, an errored search, an unknown
/// record, and a corrupt row. Bubble rows are stored out of order; the
/// `composerData` header list carries the true order.
fn sample_session() -> DesktopSession {
    let user = json!({
        "_v": 3, "type": 1, "bubbleId": "b-user",
        "createdAt": "2026-08-17T06:30:36.299Z",
        "tokenCount": {"inputTokens": 0, "outputTokens": 0},
        "unifiedMode": 2, "conversationState": "~",
        "text": "Can you evaluate the support for cursor sessions in this library?",
        "richText": "{\"type\":\"doc\"}",
        "unmodeledField": {"survives": true},
    });
    let thinking = json!({
        "_v": 3, "type": 2, "bubbleId": "b-think",
        "createdAt": "2026-08-17T06:30:41.992Z",
        "tokenCount": {"inputTokens": 0, "outputTokens": 0},
        "thinking": {"text": "Evaluating Cursor session support.", "signature": "sig-1"},
        "thinkingDurationMs": 2201,
    });
    let text = json!({
        "_v": 3, "type": 2, "bubbleId": "b-text",
        "createdAt": "2026-08-17T06:30:44.209Z",
        "tokenCount": {"inputTokens": 900, "outputTokens": 42},
        "modelInfo": {"modelName": "composer-2"},
        "text": "I'll evaluate Cursor session support.",
    });
    let read_call = json!({
        "_v": 3, "type": 2, "bubbleId": "b-read",
        "createdAt": "2026-08-17T06:30:50.000Z",
        "tokenCount": {"inputTokens": 0, "outputTokens": 0},
        "toolFormerData": {
            "tool": 40, "name": "read_file_v2", "toolCallId": "call-1",
            "status": "completed", "rawArgs": "",
            "params": "{\"targetFile\":\"/repo/src/lib.rs\",\"limit\":80,\"charsLimit\":1000000}",
            "result": "{\"totalLinesInFile\":290}",
        },
    });
    let search_call = json!({
        "_v": 3, "type": 2, "bubbleId": "b-search",
        "createdAt": "2026-08-17T06:30:55.000Z",
        "tokenCount": {"inputTokens": 0, "outputTokens": 0},
        "toolFormerData": {
            "tool": 42, "name": "glob_file_search", "toolCallId": "call-2",
            "status": "error", "rawArgs": "",
            "params": "{\"targetDirectory\":\"/nope\",\"globPattern\":\"**/plan.md\"}",
            "result": "no such directory",
        },
    });
    // A record kind this harness doesn't model must survive the round trip.
    let unmodeled = json!({
        "_v": 9, "type": 7, "bubbleId": "b-mystery",
        "novel": {"future": "record"},
    });

    let headers: Vec<Value> = [
        ("b-user", 1),
        ("b-think", 2),
        ("b-text", 2),
        ("b-read", 2),
        ("b-search", 2),
        ("b-mystery", 7),
    ]
    .iter()
    .map(|(id, t)| json!({"bubbleId": id, "type": t}))
    .collect();
    let composer_data = json!({
        "_v": 17,
        "composerId": CID,
        "hasLoaded": true,
        "status": "completed",
        "fullConversationHeadersOnly": headers,
        "modelConfig": {"modelName": "composer-2", "maxMode": true},
    });
    let header = json!({
        "type": "head",
        "composerId": CID,
        "name": "Can you evaluate the support",
        "subtitle": "Read lib.rs",
        "createdAt": 1_786_948_236_174_i64,
        "lastUpdatedAt": 1_786_948_236_297_i64,
        "unifiedMode": "agent",
        "workspaceIdentifier": {
            "id": "84c9a67ebe7054485d323a481a9cffeb",
            "uri": {"fsPath": "/repo", "scheme": "file"},
        },
    });

    DesktopSession {
        header: header.to_string(),
        workspace_id: Some("84c9a67ebe7054485d323a481a9cffeb".into()),
        created_at: 1_786_948_236_174,
        last_updated_at: 1_786_948_236_297,
        is_archived: false,
        is_subagent: false,
        recency: 1_786_948_236_297,
        checkpoint_at: None,
        composer_data: Some(composer_data.to_string()),
        // Deliberately not in conversation order; one row is corrupt JSON.
        bubbles: vec![
            DesktopRow {
                key: bubble_key("b-text"),
                value: text.to_string(),
            },
            DesktopRow {
                key: bubble_key("b-user"),
                value: user.to_string(),
            },
            DesktopRow {
                key: bubble_key("b-corrupt"),
                value: "{not json".into(),
            },
            DesktopRow {
                key: bubble_key("b-think"),
                value: thinking.to_string(),
            },
            DesktopRow {
                key: bubble_key("b-read"),
                value: read_call.to_string(),
            },
            DesktopRow {
                key: bubble_key("b-search"),
                value: search_call.to_string(),
            },
            DesktopRow {
                key: bubble_key("b-mystery"),
                value: unmodeled.to_string(),
            },
        ],
        aux: vec![DesktopRow {
            key: format!("checkpointId:{CID}:ck-1"),
            value: json!({"files": []}).to_string(),
        }],
    }
}

fn sample_transcript() -> Transcript<CursorDesktop> {
    let meta = common::Meta {
        id: CID.into(),
        timestamp: ts("2026-08-17T06:30:36.174Z"),
        cwd: Some("/repo".into()),
        git_branch: None,
        title: Some("Can you evaluate the support".into()),
        cli_version: None,
        model: Some("composer-2".into()),
    };
    Transcript::new(meta, sample_session())
}

#[cfg(feature = "opencode")]
#[test]
fn store_round_trip_is_lossless_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let store = CursorDesktopStore::new(dir.path());
    let saved = store.save(&sample_transcript()).unwrap();
    assert_eq!(saved.id, CID);
    assert!(
        dir.path()
            .join("globalStorage")
            .join("state.vscdb")
            .is_file(),
        "save writes the app's database path shape"
    );

    let loaded = store.load(&saved.reference).unwrap();
    assert_eq!(loaded.body, sample_session());
    assert_eq!(loaded.meta.id, CID);

    // Save the loaded copy again: still identical (idempotent writer).
    store.save(&loaded).unwrap();
    assert_eq!(store.load(&saved.reference).unwrap().body, loaded.body);
}

#[cfg(feature = "opencode")]
#[test]
fn discover_extracts_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let store = CursorDesktopStore::new(dir.path());
    store.save(&sample_transcript()).unwrap();

    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    let meta = &found[0].meta;
    assert_eq!(meta.id, CID);
    assert_eq!(meta.title.as_deref(), Some("Can you evaluate the support"));
    assert_eq!(meta.cwd.as_deref(), Some("/repo"));
    assert_eq!(meta.model.as_deref(), Some("composer-2"));
    assert_eq!(
        meta.timestamp,
        DateTime::from_timestamp_millis(1_786_948_236_174).unwrap()
    );
}

#[cfg(feature = "opencode")]
#[test]
fn discover_skips_drafts_and_empty_composers() {
    let dir = tempfile::tempdir().unwrap();
    let store = CursorDesktopStore::new(dir.path());
    let mut empty = sample_transcript();
    empty.body.bubbles.clear();
    empty.meta.id = "11111111-1111-4111-8111-111111111111".into();
    empty.body.header = empty
        .body
        .header
        .replace(CID, "11111111-1111-4111-8111-111111111111");
    store.save(&empty).unwrap();
    assert!(store.discover().unwrap().is_empty());
}

#[test]
fn to_common_extracts_typed_conversation() {
    let common = CursorDesktop::to_common(&sample_transcript()).unwrap();
    let msgs = &common.body;

    // user, assistant(thinking+text+read call), read result,
    // assistant(search call), search result — corrupt and unmodeled rows
    // drop from Common (they still live in the native body).
    assert_eq!(msgs.len(), 5);

    assert_eq!(msgs[0].role, common::Role::User);
    assert_eq!(
        msgs[0].content,
        vec![common::Block::Text {
            text: "Can you evaluate the support for cursor sessions in this library?".into(),
        }]
    );

    assert_eq!(msgs[1].role, common::Role::Assistant);
    assert_eq!(msgs[1].model.as_deref(), Some("composer-2"));
    assert_eq!(
        msgs[1].usage,
        Some(common::Usage {
            input_tokens: 900,
            output_tokens: 42,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        })
    );
    assert_eq!(
        msgs[1].content[0],
        common::Block::Thinking {
            text: "Evaluating Cursor session support.".into(),
            signature: Some("sig-1".into()),
            encrypted: None,
        }
    );

    // The desktop read maps onto the canonical typed Read, riding the same
    // assistant message as the thinking and text that preceded it.
    assert_eq!(
        msgs[1].content[2],
        common::Block::ToolUse {
            id: "call-1".into(),
            tool: common::Tool::Read {
                file_path: "/repo/src/lib.rs".into(),
                offset: None,
                limit: Some(80),
            },
        }
    );
    assert_eq!(
        msgs[2].content,
        vec![common::Block::ToolResult {
            tool_use_id: "call-1".into(),
            content: common::ToolOutput::Json(json!({"totalLinesInFile": 290})),
            is_error: false,
        }]
    );

    // Unmapped native tools pass through as Raw with native name and params.
    assert_eq!(
        msgs[3].content,
        vec![common::Block::ToolUse {
            id: "call-2".into(),
            tool: common::Tool::Raw {
                tool_name: "glob_file_search".into(),
                input: json!({"targetDirectory": "/nope", "globPattern": "**/plan.md"}),
            },
        }]
    );
    assert_eq!(
        msgs[4].content,
        vec![common::Block::ToolResult {
            tool_use_id: "call-2".into(),
            content: common::ToolOutput::Text("no such directory".into()),
            is_error: true,
        }]
    );
}

fn fixpoint_common() -> Transcript<Common> {
    let meta = common::Meta {
        id: CID.into(),
        timestamp: ts("2026-08-17T06:30:36.174Z"),
        cwd: Some("/repo".into()),
        git_branch: None,
        title: Some("Fixpoint".into()),
        cli_version: None,
        model: Some("composer-2".into()),
    };
    let t0 = ts("2026-08-17T06:30:36.299Z");
    let t1 = ts("2026-08-17T06:30:41.992Z");
    Transcript::new(
        meta,
        vec![
            common::Message {
                role: common::Role::User,
                content: vec![common::Block::Text {
                    text: "run the tests".into(),
                }],
                timestamp: t0,
                model: None,
                stop_reason: None,
                usage: None,
            },
            common::Message {
                role: common::Role::Assistant,
                content: vec![
                    common::Block::Thinking {
                        text: "planning".into(),
                        signature: Some("sig-9".into()),
                        encrypted: None,
                    },
                    common::Block::Text {
                        text: "Running them now.".into(),
                    },
                    common::Block::ToolUse {
                        id: "call-a".into(),
                        tool: common::Tool::Bash {
                            command: "cargo test".into(),
                            workdir: Some("/repo".into()),
                            timeout_ms: Some(30_000),
                            description: None,
                            run_in_background: false,
                        },
                    },
                ],
                timestamp: t1,
                model: Some("composer-2".into()),
                usage: Some(common::Usage {
                    input_tokens: 12,
                    output_tokens: 7,
                    cache_read_input_tokens: None,
                    cache_creation_input_tokens: None,
                }),
                stop_reason: None,
            },
            // The result's own timestamp is restamped onto the call bubble,
            // so a result time later than the assistant turn survives.
            common::Message {
                role: common::Role::User,
                content: vec![common::Block::ToolResult {
                    tool_use_id: "call-a".into(),
                    content: common::ToolOutput::Json(json!({"output": "ok", "exit": 0})),
                    is_error: false,
                }],
                timestamp: ts("2026-08-17T06:30:47.500Z"),
                model: None,
                stop_reason: None,
                usage: None,
            },
            common::Message {
                role: common::Role::Assistant,
                content: vec![common::Block::ToolUse {
                    id: "call-b".into(),
                    tool: common::Tool::Raw {
                        tool_name: "ask_question".into(),
                        input: json!({"title": "Pick one", "questions": []}),
                    },
                }],
                timestamp: t1,
                model: None,
                stop_reason: None,
                usage: None,
            },
            common::Message {
                role: common::Role::User,
                content: vec![common::Block::ToolResult {
                    tool_use_id: "call-b".into(),
                    content: common::ToolOutput::Text("interrupted, not valid JSON".into()),
                    is_error: true,
                }],
                timestamp: t1,
                model: None,
                stop_reason: None,
                usage: None,
            },
            common::Message {
                role: common::Role::Assistant,
                content: vec![common::Block::Text {
                    text: "All green.".into(),
                }],
                timestamp: t1,
                model: None,
                stop_reason: None,
                usage: None,
            },
        ],
    )
}

#[test]
fn codec_fixpoint_through_common_loses_nothing() {
    let common = fixpoint_common();
    let native = CursorDesktop::from_common(&common).unwrap();
    let back = CursorDesktop::to_common(&native).unwrap();
    assert_eq!(back.meta, common.meta);
    assert_eq!(back.body, common.body);
}

#[test]
fn from_common_is_deterministic() {
    let a = CursorDesktop::from_common(&fixpoint_common()).unwrap();
    let b = CursorDesktop::from_common(&fixpoint_common()).unwrap();
    assert_eq!(
        CursorDesktop::to_text(&a).unwrap(),
        CursorDesktop::to_text(&b).unwrap()
    );
}

#[test]
fn text_codec_round_trips_the_body() {
    let transcript = sample_transcript();
    let text = CursorDesktop::to_text(&transcript).unwrap();
    let parsed = CursorDesktop::from_text(&text).unwrap();
    assert_eq!(parsed.body, transcript.body);
    assert_eq!(parsed.meta.id, CID);
}

#[test]
fn native_call_ids_are_sanitized_for_other_harnesses() {
    // Real Cursor toolCallIds embed a literal newline between the call and
    // function-call halves; Anthropic's tool_use id grammar refuses it.
    let mut session = sample_session();
    let call = json!({
        "_v": 3, "type": 2, "bubbleId": "b-nl",
        "toolFormerData": {
            "tool": 40, "name": "read_file_v2",
            "toolCallId": "call-abc-12\nfc_def_4",
            "status": "completed",
            "params": "{\"targetFile\":\"/repo/x.rs\"}",
            "result": "{}",
        },
    });
    session.bubbles.push(DesktopRow {
        key: bubble_key("b-nl"),
        value: call.to_string(),
    });
    if let Some(data) = &session.composer_data {
        let mut v: Value = serde_json::from_str(data).unwrap();
        v["fullConversationHeadersOnly"]
            .as_array_mut()
            .unwrap()
            .push(json!({"bubbleId": "b-nl", "type": 2}));
        session.composer_data = Some(v.to_string());
    }
    let transcript = Transcript::new(sample_transcript().meta, session);
    let msgs = CursorDesktop::to_common(&transcript).unwrap().body;
    let ids: Vec<String> = msgs
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            common::Block::ToolUse { id, .. } => Some(id.clone()),
            common::Block::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
            _ => None,
        })
        .collect();
    assert!(ids.iter().any(|id| id == "call-abc-12_fc_def_4"));
    for id in ids {
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "unsanitized id: {id:?}"
        );
    }
}

#[test]
fn pending_tool_call_yields_no_result_message() {
    let mut session = sample_session();
    let pending = json!({
        "_v": 3, "type": 2, "bubbleId": "b-pending",
        "toolFormerData": {
            "tool": 15, "name": "run_terminal_command_v2", "toolCallId": "call-p",
            "status": "started",
            "params": "{\"command\":\"sleep 100\",\"cwd\":\"\"}",
            "result": "",
        },
    });
    session.bubbles.push(DesktopRow {
        key: bubble_key("b-pending"),
        value: pending.to_string(),
    });
    if let Some(data) = &session.composer_data {
        let mut v: Value = serde_json::from_str(data).unwrap();
        v["fullConversationHeadersOnly"]
            .as_array_mut()
            .unwrap()
            .push(json!({"bubbleId": "b-pending", "type": 2}));
        session.composer_data = Some(v.to_string());
    }
    let transcript = Transcript::new(sample_transcript().meta, session);
    let msgs = CursorDesktop::to_common(&transcript).unwrap().body;
    let last = msgs.last().unwrap();
    assert_eq!(last.role, common::Role::Assistant);
    assert_eq!(
        last.content,
        vec![common::Block::ToolUse {
            id: "call-p".into(),
            tool: common::Tool::Bash {
                command: "sleep 100".into(),
                workdir: None,
                timeout_ms: None,
                description: None,
                run_in_background: false,
            },
        }]
    );
}

#[test]
fn edit_tool_with_replace_all_survives_fixpoint() {
    let mut common = fixpoint_common();
    let t_user = ts("2026-08-17T06:30:47.800Z");
    let t_tool = ts("2026-08-17T06:30:49.000Z");
    common.body.push(common::Message {
        role: common::Role::User,
        content: vec![common::Block::Text {
            text: "replace in file".into(),
        }],
        timestamp: t_user,
        model: None,
        stop_reason: None,
        usage: None,
    });
    common.body.push(common::Message {
        role: common::Role::Assistant,
        content: vec![common::Block::ToolUse {
            id: "call-edit-replace-all".into(),
            tool: common::Tool::Edit {
                file_path: "/repo/src/lib.rs".into(),
                old_string: "foo".into(),
                new_string: "bar".into(),
                replace_all: true,
            },
        }],
        timestamp: t_tool,
        model: None,
        stop_reason: None,
        usage: None,
    });
    common.body.push(common::Message {
        role: common::Role::User,
        content: vec![common::Block::ToolResult {
            tool_use_id: "call-edit-replace-all".into(),
            content: common::ToolOutput::Text("ok".into()),
            is_error: false,
        }],
        timestamp: t_tool,
        model: None,
        stop_reason: None,
        usage: None,
    });

    let native = CursorDesktop::from_common(&common).unwrap();
    let back = CursorDesktop::to_common(&native).unwrap();
    assert_eq!(back.body, common.body);
}

#[test]
fn edit_and_write_tools_normalize_and_denormalize_losslessly() {
    let mut common = fixpoint_common();
    let t_user = ts("2026-08-17T06:30:47.800Z");
    let t_tool1 = ts("2026-08-17T06:30:49.000Z");
    let t_tool2 = ts("2026-08-17T06:30:50.000Z");
    common.body.push(common::Message {
        role: common::Role::User,
        content: vec![common::Block::Text {
            text: "apply edits and writes".into(),
        }],
        timestamp: t_user,
        model: None,
        stop_reason: None,
        usage: None,
    });
    common.body.push(common::Message {
        role: common::Role::Assistant,
        content: vec![common::Block::ToolUse {
            id: "call-edit".into(),
            tool: common::Tool::Edit {
                file_path: "/repo/src/lib.rs".into(),
                old_string: "foo".into(),
                new_string: "bar".into(),
                replace_all: false,
            },
        }],
        timestamp: t_tool1,
        model: None,
        stop_reason: None,
        usage: None,
    });
    common.body.push(common::Message {
        role: common::Role::User,
        content: vec![common::Block::ToolResult {
            tool_use_id: "call-edit".into(),
            content: common::ToolOutput::Text("applied".into()),
            is_error: false,
        }],
        timestamp: t_tool1,
        model: None,
        stop_reason: None,
        usage: None,
    });
    common.body.push(common::Message {
        role: common::Role::Assistant,
        content: vec![common::Block::ToolUse {
            id: "call-write".into(),
            tool: common::Tool::Write {
                file_path: "/repo/src/main.rs".into(),
                content: "fn main() {}".into(),
            },
        }],
        timestamp: t_tool2,
        model: None,
        stop_reason: None,
        usage: None,
    });
    common.body.push(common::Message {
        role: common::Role::User,
        content: vec![common::Block::ToolResult {
            tool_use_id: "call-write".into(),
            content: common::ToolOutput::Text("written".into()),
            is_error: false,
        }],
        timestamp: t_tool2,
        model: None,
        stop_reason: None,
        usage: None,
    });

    let native = CursorDesktop::from_common(&common).unwrap();
    let back = CursorDesktop::to_common(&native).unwrap();
    assert_eq!(back.body, common.body);
}

#[cfg(feature = "opencode")]
#[test]
fn store_resolves_workspace_id_with_percent_encoding_and_spaces() {
    let dir = tempfile::tempdir().unwrap();
    let store = CursorDesktopStore::new(dir.path());
    let ws_root = dir.path().join("workspaceStorage");

    #[cfg(not(windows))]
    let test_cwd = "/Users/me/My Project";
    #[cfg(windows)]
    let test_cwd = r"C:\Users\me\My Project";

    // Workspace with spaces (%20)
    let ws_space = ws_root.join("ws-space");
    std::fs::create_dir_all(&ws_space).unwrap();
    #[cfg(not(windows))]
    let ws_folder = "file:///Users/me/My%20Project";
    #[cfg(windows)]
    let ws_folder = "file:///c%3A/Users/me/My%20Project";
    std::fs::write(
        ws_space.join("workspace.json"),
        json!({"folder": ws_folder}).to_string(),
    )
    .unwrap();

    let mut t = sample_transcript();
    t.body.workspace_id = None;
    t.meta.id = "22222222-2222-4222-8222-222222222222".into();
    t.meta.cwd = Some(test_cwd.into());
    let saved = store.save(&t).unwrap();
    let loaded = store.load(&saved.reference).unwrap();
    assert_eq!(loaded.body.workspace_id.as_deref(), Some("ws-space"));

    // Negative test: POSIX relative 'repo' does NOT match absolute '/repo'
    let ws_posix = ws_root.join("ws-posix");
    std::fs::create_dir_all(&ws_posix).unwrap();
    std::fs::write(
        ws_posix.join("workspace.json"),
        json!({"folder": "file:///repo"}).to_string(),
    )
    .unwrap();

    let mut t_rel = sample_transcript();
    t_rel.body.workspace_id = None;
    t_rel.meta.id = "33333333-3333-4333-8333-333333333333".into();
    t_rel.meta.cwd = Some("repo".into());
    let saved_rel = store.save(&t_rel).unwrap();
    let loaded_rel = store.load(&saved_rel.reference).unwrap();
    assert_ne!(loaded_rel.body.workspace_id.as_deref(), Some("ws-posix"));
}
