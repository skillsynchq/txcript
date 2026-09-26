#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! fx harness tests: Store round trip, metadata discovery, Common extraction,
//! codec fixpoints, and format-specific behavior (errored results, images,
//! interrupted turns, reasoning preserved in the private sidecar).

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tempfile::TempDir;
use txcript::common::{
    Block, ImageSource, Message, Meta, Role, StopReason, Tool, ToolOutput, Usage,
};
use txcript::harness::fx::{Fx, FxStore};
use txcript::{Codec, Store, TextCodec, Transcript};

const SESSION_ID: &str = "1787599019497-1787599019497274000-8ce6639806926fb7";

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn png_image_block() -> Block {
    Block::Image {
        source: ImageSource {
            source_type: "base64".into(),
            media_type: "image/png".into(),
            data: "UE5HYnl0ZXM=".into(),
        },
    }
}

/// One event-log line in fx's envelope shape.
fn event(seq: u64, ts_ms: i64, kind: &str, payload: &Value) -> Value {
    json!({
        "schema_version": 1,
        "log_generation": "388f98f17a167b27a3cdadf3850b237e",
        "seq": seq,
        "event_id": format!("{seq:032x}"),
        "timestamp_ms": ts_ms,
        "kind": kind,
        "payload": payload,
    })
}

/// A tool result in fx's committed shape.
fn tool_result(id: &str, status: &str, output: &str) -> Value {
    json!({
        "tool_call_id": id,
        "tool_name": "tool",
        "status": status,
        "output": output,
        "output_handle": Value::Null,
        "preview": Value::Null,
        "output_bytes": output.len(),
        "stored_output_bytes": output.len(),
        "truncated": false,
        "provider_native": false,
        "created_at_ms": 0,
        "permission_feedback": [],
        "committed_file_presentation": Value::Null,
        "command_output_replay": Value::Null,
        "command_process_presentation": Value::Null,
    })
}

/// The event log, modeling the real sample's shapes: the header, a tool-rich
/// committed turn (read/terminal/edit with a failed terminal), a bookkeeping
/// event, an image turn, and one unmodeled event that must survive.
fn events_jsonl() -> String {
    let started = json!({
        "id": SESSION_ID,
        "created_at_ms": 1_787_599_019_497_i64,
        "origin_workspace_root": "/repo",
        "workspace_root": "/repo",
        "conversation_language": "und",
        "preferences": {"model": "zai/glm-5.2", "effort": "auto", "fast_mode": false, "provider": "gateway"},
        "usage": {"input_tokens": 0, "output_tokens": 0, "models": [], "pending": []},
    });
    let tool_turn = json!({
        "conversation_language": "und-Latn",
        "total_input_tokens": 100,
        "total_output_tokens": 3,
        "turn": {
            "kind": "assistant",
            "user": {"text": "read notes and edit it", "images": []},
            "assistant": "DONE",
            "execution": {
                "schema_version": 3,
                "tool_steps": [{
                    "assistant": "Reading and editing.",
                    "tool_calls": [
                        {"id": "call-1", "name": "read_file",
                         "arguments_json": "{\"path\":\"notes.txt\",\"limit\":80}", "provider_result": Value::Null},
                        {"id": "call-2", "name": "terminal",
                         "arguments_json": "{\"action\":\"exec\",\"command\":\"ls -la\",\"cwd\":\"/repo\",\"profile\":\"user\"}", "provider_result": Value::Null},
                        {"id": "call-3", "name": "edit_file",
                         "arguments_json": "{\"path\":\"notes.txt\",\"old_string\":\"beta\",\"new_string\":\"delta\"}", "provider_result": Value::Null},
                        {"id": "call-4", "name": "terminal",
                         "arguments_json": "{\"action\":\"exec\",\"command\":\"cat /nope\",\"cwd\":\"/repo\",\"profile\":\"user\"}", "provider_result": Value::Null},
                    ],
                    "tool_results": [
                        tool_result("call-1", "success", "alpha\nbeta\ngamma"),
                        tool_result("call-2", "success", "total 0"),
                        tool_result("call-3", "success", "edited notes.txt"),
                        tool_result("call-4", "failure", "cat: /nope: No such file or directory"),
                    ],
                }],
                "files": [],
            },
        },
    });
    let image_turn = json!({
        "conversation_language": "und-Latn",
        "total_input_tokens": 200,
        "total_output_tokens": 6,
        "turn": {
            "kind": "assistant",
            "user": {"text": "what color?", "images": [{
                "id": 1, "path": "/repo/red.png", "media_type": "image/png",
                "snapshot_path": "images/image-1-3fd6e6be528c182d.bin",
                "snapshot_sha256": "3fd6e6be528c182d768563a63b65ac5a70d022149a01eeeaaa30396d75f426e0",
            }]},
            "assistant": "Solid red.",
            "execution": {"schema_version": 3, "tool_steps": [], "files": []},
        },
    });
    [
        event(1, 1_787_599_019_497, "session_started", &started),
        event(
            2,
            1_787_599_037_000,
            "usage_checkpointed",
            &json!({"usage": {"input_tokens": 100}}),
        ),
        event(3, 1_787_599_037_222, "history_turn_committed", &tool_turn),
        event(4, 1_787_599_045_084, "history_turn_committed", &image_turn),
        // Unmodeled bookkeeping: must pass through untouched.
        event(
            5,
            1_787_599_045_100,
            "custom_marker",
            &json!({"note": "not a known kind"}),
        ),
    ]
    .iter()
    .map(|v| v.to_string() + "\n")
    .collect()
}

fn session_json() -> String {
    json!({
        "schema_version": 3,
        "storage_format": "event_log_v1",
        "id": SESSION_ID,
        "authority_id": "18d99893e4d755b55ed8fe1ef7ae4ea9",
        "log_generation": "388f98f17a167b27a3cdadf3850b237e",
        "created_at_ms": 1_787_599_019_497_i64,
        "updated_at_ms": 1_787_599_045_084_i64,
        "origin_workspace_root": "/repo",
        "workspace_root": "/repo",
        "conversation_language": "und-Latn",
        "history_len": 2,
        "last_event_seq": 5,
        "preferences": {"model": "zai/glm-5.2", "effort": "auto", "fast_mode": false, "provider": "gateway"},
    })
    .to_string()
}

/// Write the native fixture under `<root>/<id>/` the way fx lays a session out.
fn write_fixture(root: &std::path::Path) -> std::path::PathBuf {
    let dir = root.join(SESSION_ID);
    std::fs::create_dir_all(dir.join("images")).unwrap();
    std::fs::write(dir.join("events.jsonl"), events_jsonl()).unwrap();
    std::fs::write(dir.join("session.json"), session_json()).unwrap();
    std::fs::write(
        dir.join("authority.json"),
        json!({"schema_version": 1, "session_id": SESSION_ID,
               "authority_id": "18d99893e4d755b55ed8fe1ef7ae4ea9",
               "storage_format": "event_log_v1", "source": "native_create"})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("commit.388f98f17a167b27a3cdadf3850b237e.json"),
        json!({"schema_version": 1, "session_id": SESSION_ID,
               "log_generation": "388f98f17a167b27a3cdadf3850b237e",
               "through_seq": 5, "through_event_id": format!("{:032x}", 5),
               "through_event_log_bytes": 999})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("display.json"),
        json!({"schema_version": 1, "title": "read notes and edit it",
               "preview": "read notes and edit it", "origin_workspace_root": "/repo"})
        .to_string(),
    )
    .unwrap();
    // The image bytes the second turn references.
    std::fs::write(dir.join("images/image-1-3fd6e6be528c182d.bin"), b"PNGbytes").unwrap();
    dir
}

#[test]
fn store_round_trip_is_lossless_on_disk() {
    let src = TempDir::new().unwrap();
    let session_dir = write_fixture(src.path());
    let store = FxStore::new(src.path());
    let first = store.load(&session_dir).unwrap();

    // The unmodeled event is carried, untouched.
    assert!(
        first
            .body
            .events
            .iter()
            .any(|e| { e.get("kind").and_then(Value::as_str) == Some("custom_marker") })
    );
    // The image bytes came along.
    assert_eq!(first.body.images.len(), 1);
    assert_eq!(first.body.images[0].data, b"PNGbytes");

    let dst = TempDir::new().unwrap();
    let dst_store = FxStore::new(dst.path());
    let saved = dst_store.save(&first).unwrap();

    // fx's directory encoding: the session id is the directory name.
    assert_eq!(saved.id, SESSION_ID);
    assert_eq!(saved.reference, dst.path().join(SESSION_ID));

    let second = dst_store.load(&saved.reference).unwrap();
    assert_eq!(first, second);
}

#[test]
fn discover_extracts_metadata() {
    let root = TempDir::new().unwrap();
    write_fixture(root.path());
    let store = FxStore::new(root.path());
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    let meta = &found[0].meta;
    assert_eq!(meta.id, SESSION_ID);
    assert_eq!(meta.cwd.as_deref(), Some("/repo"));
    assert_eq!(meta.title.as_deref(), Some("read notes and edit it"));
    assert_eq!(meta.model.as_deref(), Some("zai/glm-5.2"));
    assert_eq!(meta.timestamp.timestamp_millis(), 1_787_599_019_497);
}

#[test]
fn discovery_ignores_directories_without_a_session_log() {
    let root = TempDir::new().unwrap();
    let not_a_session = root.path().join("latest");
    std::fs::create_dir_all(&not_a_session).unwrap();
    std::fs::write(not_a_session.join("something.json"), "{}").unwrap();
    let store = FxStore::new(root.path());
    assert!(store.discover().unwrap().is_empty());
}

#[test]
fn to_common_extracts_conversation_with_typed_tools() {
    let root = TempDir::new().unwrap();
    let dir = write_fixture(root.path());
    let store = FxStore::new(root.path());
    let common = Fx::to_common(&store.load(&dir).unwrap()).unwrap();
    let msgs = &common.body;

    // session_started, usage_checkpointed and custom_marker are skipped; two
    // committed turns produce their messages.
    // turn 1: user, assistant(step text + 4 tool uses), user(4 results),
    //         assistant("DONE"). turn 2: user(text+image), assistant("Solid red.")
    assert_eq!(msgs.len(), 6);

    assert_eq!(msgs[0].role, Role::User);
    assert_eq!(
        msgs[0].content,
        vec![Block::Text {
            text: "read notes and edit it".into()
        }]
    );

    // The tool-bearing assistant message: typed tools with renamed keys.
    let assistant = &msgs[1];
    assert_eq!(assistant.role, Role::Assistant);
    assert_eq!(assistant.model.as_deref(), Some("zai/glm-5.2"));
    assert_eq!(assistant.content.len(), 5); // step text + 4 tool uses
    assert_eq!(
        assistant.content[1],
        Block::ToolUse {
            id: "call-1".into(),
            tool: Tool::Read {
                file_path: "notes.txt".into(),
                offset: None,
                limit: Some(80)
            },
        }
    );
    assert_eq!(
        assistant.content[2],
        Block::ToolUse {
            id: "call-2".into(),
            tool: Tool::Bash {
                command: "ls -la".into(),
                workdir: Some("/repo".into()),
                timeout_ms: None,
                description: None,
                run_in_background: false,
            },
        }
    );
    assert_eq!(
        assistant.content[3],
        Block::ToolUse {
            id: "call-3".into(),
            tool: Tool::Edit {
                file_path: "notes.txt".into(),
                old_string: "beta".into(),
                new_string: "delta".into(),
                replace_all: false,
            },
        }
    );

    // Results pair by call id; the failed terminal marks is_error.
    let results = &msgs[2];
    assert_eq!(results.role, Role::User);
    assert_eq!(results.content.len(), 4);
    assert_eq!(
        results.content[3],
        Block::ToolResult {
            tool_use_id: "call-4".into(),
            content: ToolOutput::Text("cat: /nope: No such file or directory".into()),
            is_error: true,
        }
    );

    // Concluding text, with the turn's stop reason.
    assert_eq!(
        msgs[3].content,
        vec![Block::Text {
            text: "DONE".into()
        }]
    );
    assert_eq!(msgs[3].stop_reason, Some(StopReason::EndTurn));

    // The image turn: text + the snapshot image re-inlined from images/.
    assert_eq!(
        msgs[4].content,
        vec![
            Block::Text {
                text: "what color?".into()
            },
            png_image_block()
        ]
    );
    assert_eq!(
        msgs[5].content,
        vec![Block::Text {
            text: "Solid red.".into()
        }]
    );
}

/// A Common transcript at fx's native granularity: one timestamp per turn,
/// text and tool calls grouped per assistant message, thinking on its own,
/// results one message per tool round, stop reason on each turn's last
/// assistant message.
#[allow(clippy::too_many_lines)]
fn representable_common() -> Transcript<txcript::Common> {
    let meta = Meta {
        id: SESSION_ID.into(),
        timestamp: ts("2026-07-02T01:24:11.341Z"),
        cwd: Some("/repo".into()),
        git_branch: None,
        title: Some("Repo work".into()),
        cli_version: None,
        model: Some("zai/glm-5.2".into()),
    };
    let model = || Some("zai/glm-5.2".to_string());
    let t1 = ts("2026-07-02T01:24:12.000Z");
    let t2 = ts("2026-07-02T01:24:20.000Z");
    let body = vec![
        Message {
            role: Role::User,
            content: vec![
                Block::Text {
                    text: "rename the helper".into(),
                },
                png_image_block(),
            ],
            timestamp: t1,
            model: None,
            stop_reason: None,
            usage: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![
                Block::Thinking {
                    text: "Find the helper first".into(),
                    signature: None,
                    encrypted: Some("ENCTOK".into()),
                },
                Block::Text {
                    text: "Renaming it now.".into(),
                },
                Block::ToolUse {
                    id: "call-1".into(),
                    tool: Tool::Edit {
                        file_path: "/repo/src/lib.rs".into(),
                        old_string: "fn helper".into(),
                        new_string: "fn assist".into(),
                        replace_all: false,
                    },
                },
                Block::ToolUse {
                    id: "call-2".into(),
                    tool: Tool::Raw {
                        tool_name: "grep_files".into(),
                        input: json!({"pattern": "helper", "path": "."}),
                    },
                },
            ],
            timestamp: t1,
            model: model(),
            stop_reason: None,
            usage: None,
        },
        Message {
            role: Role::User,
            content: vec![
                Block::ToolResult {
                    tool_use_id: "call-1".into(),
                    content: ToolOutput::Text("edited".into()),
                    is_error: false,
                },
                Block::ToolResult {
                    tool_use_id: "call-2".into(),
                    content: ToolOutput::Text("no matches".into()),
                    is_error: true,
                },
            ],
            timestamp: t1,
            model: None,
            stop_reason: None,
            usage: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![Block::Text {
                text: "Done.".into(),
            }],
            timestamp: t1,
            model: model(),
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        },
        // A second turn with a plain prompt and reply.
        Message {
            role: Role::User,
            content: vec![Block::Text {
                text: "thanks".into(),
            }],
            timestamp: t2,
            model: None,
            stop_reason: None,
            usage: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![Block::Text {
                text: "You're welcome.".into(),
            }],
            timestamp: t2,
            model: model(),
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        },
    ];
    Transcript::new(meta, body)
}

#[test]
fn codec_fixpoint_through_common_loses_nothing() {
    let common = representable_common();
    let native = Fx::from_common(&common).unwrap();
    let back = Fx::to_common(&native).unwrap();
    assert_eq!(back.meta, common.meta);
    assert_eq!(back.body, common.body);
}

#[test]
fn from_common_is_deterministic() {
    let common = representable_common();
    let a = Fx::to_text(&Fx::from_common(&common).unwrap()).unwrap();
    let b = Fx::to_text(&Fx::from_common(&common).unwrap()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn from_common_regenerates_the_header_and_commit_boundary() {
    let native = Fx::from_common(&representable_common()).unwrap();

    // The event log opens with the required session_started header.
    assert_eq!(
        native.body.events[0].get("kind").and_then(Value::as_str),
        Some("session_started")
    );
    // The header's byte offsets pin the actual rendered log.
    let session = native.body.session.as_ref().unwrap();
    let rendered: usize = native
        .body
        .events
        .iter()
        .map(|e| serde_json::to_string(e).unwrap().len() + 1)
        .sum();
    assert_eq!(
        session.get("event_log_bytes").and_then(Value::as_u64),
        Some(rendered as u64)
    );
    // The commit boundary agrees on the through byte count.
    let commit = native.body.commit.as_ref().unwrap();
    assert_eq!(
        commit
            .get("through_event_log_bytes")
            .and_then(Value::as_u64),
        Some(rendered as u64)
    );

    // Reasoning lives in the private sidecar, keyed by turn/assistant index.
    let reasoning = native.body.reasoning.as_ref().unwrap();
    let entries = reasoning.get("entries").and_then(Value::as_array).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].pointer("/blocks/0/text").and_then(Value::as_str),
        Some("Find the helper first")
    );

    // The edit denormalizes to fx's native `edit_file` with a `path` key.
    let call = native
        .body
        .events
        .iter()
        .filter(|e| e.get("kind").and_then(Value::as_str) == Some("history_turn_committed"))
        .find_map(|e| {
            e.pointer("/payload/turn/execution/tool_steps/0/tool_calls/0")
                .cloned()
        })
        .unwrap();
    assert_eq!(call.get("name").and_then(Value::as_str), Some("edit_file"));
    let args: Value =
        serde_json::from_str(call.get("arguments_json").and_then(Value::as_str).unwrap()).unwrap();
    assert_eq!(
        args.get("path").and_then(Value::as_str),
        Some("/repo/src/lib.rs")
    );
    assert!(
        args.get("replace_all").is_none(),
        "fx edit_file has no replace_all"
    );
}

#[test]
fn errored_and_structured_results_round_trip_and_flatten() {
    let mut common = representable_common();
    // Replace the second turn's reply with a structured JSON tool result.
    common.body[2].content = vec![
        Block::ToolResult {
            tool_use_id: "call-1".into(),
            content: ToolOutput::Json(json!([
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second"},
            ])),
            is_error: false,
        },
        Block::ToolResult {
            tool_use_id: "call-2".into(),
            content: ToolOutput::Json(json!({"tool_name": "WebFetch"})),
            is_error: true,
        },
    ];
    let native = Fx::from_common(&common).unwrap();
    // fx stores results as plain strings: block arrays flatten to their text,
    // other JSON becomes compact string.
    let outputs: Vec<String> = native
        .body
        .events
        .iter()
        .filter_map(|e| e.pointer("/payload/turn/execution/tool_steps/0/tool_results"))
        .flat_map(|r| r.as_array().cloned().unwrap_or_default())
        .filter_map(|r| r.get("output").and_then(Value::as_str).map(String::from))
        .collect();
    assert_eq!(outputs[0], "first\n\nsecond");
    assert_eq!(outputs[1], r#"{"tool_name":"WebFetch"}"#);
    // The failure status survives back to Common.
    let back = Fx::to_common(&native).unwrap();
    let is_errors: Vec<bool> = back
        .body
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            Block::ToolResult { is_error, .. } => Some(*is_error),
            _ => None,
        })
        .collect();
    assert_eq!(is_errors, vec![false, true]);
}

#[test]
fn interrupted_turn_round_trips() {
    let meta = Meta {
        id: SESSION_ID.into(),
        timestamp: ts("2026-07-02T01:24:11.341Z"),
        cwd: Some("/repo".into()),
        git_branch: None,
        title: None,
        cli_version: None,
        model: Some("zai/glm-5.2".into()),
    };
    let t = ts("2026-07-02T01:24:12.000Z");
    let common = Transcript::new(
        meta,
        vec![
            Message {
                role: Role::User,
                content: vec![Block::Text {
                    text: "run a long job".into(),
                }],
                timestamp: t,
                model: None,
                stop_reason: None,
                usage: None,
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    Block::Text {
                        text: "Starting it.".into(),
                    },
                    Block::ToolUse {
                        id: "call-x".into(),
                        tool: Tool::Bash {
                            command: "sleep 45".into(),
                            workdir: None,
                            timeout_ms: None,
                            description: None,
                            run_in_background: false,
                        },
                    },
                ],
                timestamp: t,
                model: Some("zai/glm-5.2".into()),
                stop_reason: Some(StopReason::Aborted),
                usage: None,
            },
        ],
    );

    let native = Fx::from_common(&common).unwrap();
    // The turn is written in fx's interrupted shape.
    let kind = native
        .body
        .events
        .iter()
        .find_map(|e| e.pointer("/payload/turn/kind").and_then(Value::as_str));
    assert_eq!(kind, Some("interrupted"));

    let back = Fx::to_common(&native).unwrap();
    assert_eq!(back.body, common.body);
}

#[test]
fn unparseable_tool_arguments_fall_back_to_raw_string() {
    let root = TempDir::new().unwrap();
    let dir = root.path().join(SESSION_ID);
    std::fs::create_dir_all(&dir).unwrap();
    let turn = json!({
        "conversation_language": "und",
        "total_input_tokens": 0,
        "total_output_tokens": 0,
        "turn": {
            "kind": "assistant",
            "user": {"text": "hi", "images": []},
            "assistant": "",
            "execution": {"schema_version": 3, "files": [], "tool_steps": [{
                "assistant": Value::Null,
                "tool_calls": [{"id": "call-x", "name": "weird",
                    "arguments_json": "not json {", "provider_result": Value::Null}],
                "tool_results": [],
            }]},
        },
    });
    let log = [
        event(
            1,
            1,
            "session_started",
            &json!({"id": SESSION_ID, "created_at_ms": 1, "workspace_root": "/repo",
                     "preferences": {"model": "m"}}),
        ),
        event(2, 2, "history_turn_committed", &turn),
    ]
    .iter()
    .map(|v| v.to_string() + "\n")
    .collect::<String>();
    std::fs::write(dir.join("events.jsonl"), log).unwrap();

    let store = FxStore::new(root.path());
    let common = Fx::to_common(&store.load(&dir).unwrap()).unwrap();
    // msgs[0] is the "hi" prompt; the raw tool call rides the assistant step.
    assert_eq!(
        common.body[1].content[0],
        Block::ToolUse {
            id: "call-x".into(),
            tool: Tool::Raw {
                tool_name: "weird".into(),
                input: Value::String("not json {".into()),
            },
        }
    );
}

#[test]
fn usage_cache_tokens_round_trip() {
    let meta = Meta {
        id: SESSION_ID.into(),
        timestamp: ts("2026-07-02T01:24:11.341Z"),
        cwd: Some("/repo".into()),
        git_branch: None,
        title: None,
        cli_version: None,
        model: Some("zai/glm-5.2".into()),
    };
    let t = ts("2026-07-02T01:24:12.000Z");
    let common = Transcript::new(
        meta,
        vec![
            Message {
                role: Role::User,
                content: vec![Block::Text {
                    text: "hello".into(),
                }],
                timestamp: t,
                model: None,
                stop_reason: None,
                usage: None,
            },
            Message {
                role: Role::Assistant,
                content: vec![Block::Text {
                    text: "hi there".into(),
                }],
                timestamp: t,
                model: Some("zai/glm-5.2".into()),
                stop_reason: Some(StopReason::EndTurn),
                usage: Some(Usage {
                    input_tokens: 150,
                    output_tokens: 45,
                    cache_read_input_tokens: Some(30),
                    cache_creation_input_tokens: Some(15),
                }),
            },
        ],
    );

    let native = Fx::from_common(&common).unwrap();
    let back = Fx::to_common(&native).unwrap();
    let last_asst_usage = back.body.iter().rev().find_map(|m| m.usage);
    assert_eq!(
        last_asst_usage,
        Some(Usage {
            input_tokens: 150,
            output_tokens: 45,
            cache_read_input_tokens: Some(30),
            cache_creation_input_tokens: Some(15),
        })
    );
}
