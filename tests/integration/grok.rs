#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Grok CLI harness tests: Store round trip, metadata discovery, Common
//! extraction, codec fixpoints, and format-specific behavior.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tempfile::TempDir;
use txcript::common::{Block, Message, Meta, Role, StopReason, Tool, ToolOutput};
use txcript::harness::grok::{ChatRecord, Grok, GrokStore};
use txcript::{Codec, Store, TextCodec, Transcript};

const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";

fn png_image_block() -> Block {
    Block::Image {
        source: txcript::common::ImageSource {
            source_type: "base64".into(),
            media_type: "image/png".into(),
            data: "UE5HYnl0ZXM=".into(),
        },
    }
}

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// One display-log line, the shape Grok writes: an ACP `session/update`
/// notification with `_meta.eventId` / `_meta.agentTimestampMs`.
fn update_line(seq: u64, ts_ms: i64, method: &str, update: &Value) -> String {
    json!({
        "timestamp": ts_ms / 1000,
        "method": method,
        "params": {
            "sessionId": SESSION_ID,
            "update": update,
            "_meta": {
                "eventId": format!("{SESSION_ID}-{seq}"),
                "agentTimestampMs": ts_ms,
            },
        },
    })
    .to_string()
}

/// The model-protocol log, mirroring the real sample session's shapes:
/// a system prompt, the `<user_info>` injection, `<user_query>`-wrapped
/// prompts, reasoning with `encrypted_content`, tool calls with string
/// `arguments`, and one unmodeled record that must survive untouched.
fn chat_history_jsonl() -> String {
    [
        json!({"type": "system", "content": "You are an AI coding assistant."}),
        json!({"type": "user", "content": [{"type": "text",
            "text": "<user_info>\nOS Version: darwin 25.5.0\n\nWorkspace Path: /repo\n</user_info>"}]}),
        json!({"type": "user", "content": [{"type": "text",
            "text": "<user_query>\nwhat is this repo?\n</user_query>"}]}),
        json!({"type": "reasoning", "id": "rs_native", "status": "completed",
            "summary": [{"type": "summary_text", "text": "User asks about the repo"}],
            "encrypted_content": "ENCTOK"}),
        json!({"type": "assistant", "content": "Looking around.",
            "model_id": "grok-composer-2.5-fast", "model_fingerprint": "fp_1",
            "tool_calls": [
                {"id": "call-1", "name": "Read",
                 "arguments": "{\"path\":\"/repo/README.md\",\"limit\":80}"},
                {"id": "call-2", "name": "Shell",
                 "arguments": "{\"command\":\"ls\",\"description\":\"List files\",\"block_until_ms\":120000.0}"},
                {"id": "call-3", "name": "Glob",
                 "arguments": "{\"glob_pattern\":\"**/*.rs\",\"target_directory\":\"/repo\"}"},
            ]}),
        json!({"type": "tool_result", "tool_call_id": "call-1", "content": "# readme"}),
        json!({"type": "tool_result", "tool_call_id": "call-2", "content": "ls: not permitted"}),
        json!({"type": "tool_result", "tool_call_id": "call-3", "content": "src/lib.rs"}),
        json!({"type": "user", "prior_turn_interrupt": "mid_turn_abort",
            "content": [{"type": "text", "text": "<user_query>\nwhat's up?\n</user_query>"}]}),
        json!({"type": "assistant", "content": "All done.",
            "model_id": "grok-composer-2.5-fast"}),
        // Unmodeled bookkeeping: must pass through as ChatRecord::Other.
        json!({"type": "compaction", "snapshot": {"tokens": 12345}}),
    ]
    .iter()
    .map(|v| v.to_string() + "\n")
    .collect()
}

fn updates_jsonl() -> String {
    let mut seq = 0u64;
    let mut line = |ts_ms: i64, method: &str, update: Value| {
        let out = update_line(seq, ts_ms, method, &update);
        seq += 1;
        out + "\n"
    };
    [
        line(1_000_000_001_000, "session/update", json!({
            "sessionUpdate": "user_message_chunk",
            "content": {"type": "text", "text": "what is this repo?"},
            "_meta": {"modelId": "grok-composer-2.5-fast", "promptIndex": 0}})),
        line(1_000_000_002_000, "session/update", json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": {"type": "text", "text": "User asks about the repo"}})),
        line(1_000_000_003_000, "session/update", json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "Looking around."}})),
        line(1_000_000_004_000, "session/update", json!({
            "sessionUpdate": "tool_call", "toolCallId": "call-1",
            "title": "Read", "rawInput": {"path": "/repo/README.md", "limit": 80}})),
        line(1_000_000_004_100, "session/update", json!({
            "sessionUpdate": "tool_call", "toolCallId": "call-2",
            "title": "Shell", "rawInput": {"command": "ls"}})),
        line(1_000_000_004_200, "session/update", json!({
            "sessionUpdate": "tool_call", "toolCallId": "call-3",
            "title": "Glob", "rawInput": {"glob_pattern": "**/*.rs"}})),
        line(1_000_000_005_000, "session/update", json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "call-1",
            "status": "completed",
            "content": [{"type": "content", "content": {"type": "text", "text": "# readme"}}]})),
        line(1_000_000_006_000, "session/update", json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "call-2",
            "status": "failed",
            "content": [{"type": "content", "content": {"type": "text", "text": "ls: not permitted"}}]})),
        line(1_000_000_006_500, "session/update", json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "call-3",
            "status": "completed",
            "content": [{"type": "content", "content": {"type": "text", "text": "src/lib.rs"}}]})),
        line(1_000_000_007_000, "_x.ai/session/update", json!({
            "sessionUpdate": "turn_completed",
            "prompt_id": "aaaaaaaa-0000-0000-0000-000000000000",
            "stop_reason": "cancelled"})),
        line(1_000_000_008_000, "session/update", json!({
            "sessionUpdate": "user_message_chunk",
            "content": {"type": "text", "text": "what's up?"},
            "_meta": {"modelId": "grok-composer-2.5-fast", "promptIndex": 1}})),
        line(1_000_000_008_100, "session/update", json!({
            "sessionUpdate": "user_message_chunk",
            "content": {"type": "image", "data": "UE5HYnl0ZXM=", "mimeType": "image/png"},
            "_meta": {"modelId": "grok-composer-2.5-fast", "promptIndex": 1}})),
        line(1_000_000_009_000, "session/update", json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "All done."}})),
        line(1_000_000_010_000, "_x.ai/session/update", json!({
            "sessionUpdate": "turn_completed",
            "prompt_id": "bbbbbbbb-0000-0000-0000-000000000000",
            "stop_reason": "end_turn"})),
    ]
    .concat()
}

fn summary_json() -> String {
    json!({
        "info": {"id": SESSION_ID, "cwd": "/repo"},
        "session_summary": "Repo Q&A",
        "generated_title": "Repo Q&A",
        "created_at": "2026-07-02T01:24:11.341000Z",
        "updated_at": "2026-07-02T01:27:58.880000Z",
        "num_messages": 13,
        "num_chat_messages": 11,
        "current_model_id": "grok-composer-2.5-fast",
        "chat_format_version": 1,
        "head_branch": "main",
    })
    .to_string()
}

/// Write the native fixture under `<root>/<encoded-cwd>/<id>/` the way Grok
/// itself lays sessions out, and return the session directory.
fn write_fixture(root: &std::path::Path) -> std::path::PathBuf {
    let dir = root.join("%2Frepo").join(SESSION_ID);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("chat_history.jsonl"), chat_history_jsonl()).unwrap();
    std::fs::write(dir.join("updates.jsonl"), updates_jsonl()).unwrap();
    std::fs::write(dir.join("summary.json"), summary_json()).unwrap();
    std::fs::write(
        dir.join("system_prompt.txt"),
        "You are an AI coding assistant.",
    )
    .unwrap();
    std::fs::write(
        dir.join("signals.json"),
        json!({"turnCount": 2}).to_string(),
    )
    .unwrap();
    dir
}

#[test]
fn store_round_trip_is_lossless_on_disk() {
    let src = TempDir::new().unwrap();
    let session_dir = write_fixture(src.path());
    let store = GrokStore::new(src.path());
    let first = store.load(&session_dir).unwrap();

    // The unmodeled record is carried, untouched.
    assert!(first.body.chat_history.iter().any(|r| matches!(
        r,
        ChatRecord::Other(v) if v.get("type").and_then(Value::as_str) == Some("compaction")
    )));

    let dst = TempDir::new().unwrap();
    let dst_store = GrokStore::new(dst.path());
    let saved = dst_store.save(&first).unwrap();

    // Grok's directory encoding: percent-encoded cwd, then the session id.
    assert_eq!(saved.id, SESSION_ID);
    assert_eq!(saved.reference, dst.path().join("%2Frepo").join(SESSION_ID));

    let second = dst_store.load(&saved.reference).unwrap();
    assert_eq!(first, second);
}

#[test]
fn discover_extracts_metadata() {
    let root = TempDir::new().unwrap();
    write_fixture(root.path());
    let store = GrokStore::new(root.path());
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    let meta = &found[0].meta;
    assert_eq!(meta.id, SESSION_ID);
    assert_eq!(meta.cwd.as_deref(), Some("/repo"));
    assert_eq!(meta.title.as_deref(), Some("Repo Q&A"));
    assert_eq!(meta.model.as_deref(), Some("grok-composer-2.5-fast"));
    assert_eq!(meta.git_branch.as_deref(), Some("main"));
    assert_eq!(meta.timestamp, ts("2026-07-02T01:24:11.341000Z"));
}

#[test]
fn to_common_extracts_conversation_with_display_log_backfill() {
    let root = TempDir::new().unwrap();
    let dir = write_fixture(root.path());
    let store = GrokStore::new(root.path());
    let common = Grok::to_common(&store.load(&dir).unwrap()).unwrap();
    let msgs = &common.body;

    // system + user_info + compaction are skipped; everything else lands.
    assert_eq!(msgs.len(), 8);

    // Scaffolding stripped, timestamp from the display log.
    assert_eq!(msgs[0].role, Role::User);
    assert_eq!(
        msgs[0].content,
        vec![Block::Text {
            text: "what is this repo?".into()
        }]
    );
    assert_eq!(msgs[0].timestamp.timestamp_millis(), 1_000_000_001_000);

    // Reasoning becomes Thinking, keeping the provider token.
    assert_eq!(
        msgs[1].content,
        vec![Block::Thinking {
            text: "User asks about the repo".into(),
            signature: None,
            encrypted: Some("ENCTOK".into()),
        }]
    );

    // The tool-bearing assistant message: typed tools with renamed keys.
    assert_eq!(msgs[2].model.as_deref(), Some("grok-composer-2.5-fast"));
    assert_eq!(msgs[2].content.len(), 4);
    assert_eq!(
        msgs[2].content[1],
        Block::ToolUse {
            id: "call-1".into(),
            tool: Tool::Read {
                file_path: "/repo/README.md".into(),
                offset: None,
                limit: Some(80),
            },
        }
    );
    assert_eq!(
        msgs[2].content[2],
        Block::ToolUse {
            id: "call-2".into(),
            tool: Tool::Bash {
                command: "ls".into(),
                workdir: None,
                timeout_ms: Some(120_000),
                description: Some("List files".into()),
                run_in_background: false,
            },
        }
    );
    // Glob has no typed variant: Raw, with keys canonicalized.
    assert_eq!(
        msgs[2].content[3],
        Block::ToolUse {
            id: "call-3".into(),
            tool: Tool::Raw {
                tool_name: "Glob".into(),
                input: json!({"pattern": "**/*.rs", "path": "/repo"}),
            },
        }
    );
    // The first turn was cancelled: its last assistant message says so.
    assert_eq!(msgs[2].stop_reason, Some(StopReason::Aborted));

    // Results pair by call id; the failed status marks is_error.
    assert_eq!(
        msgs[3].content,
        vec![Block::ToolResult {
            tool_use_id: "call-1".into(),
            content: ToolOutput::Text("# readme".into()),
            is_error: false,
        }]
    );
    assert_eq!(
        msgs[4].content,
        vec![Block::ToolResult {
            tool_use_id: "call-2".into(),
            content: ToolOutput::Text("ls: not permitted".into()),
            is_error: true,
        }]
    );
    assert_eq!(msgs[4].timestamp.timestamp_millis(), 1_000_000_006_000);

    // The second prompt's image lives only in the display log and is
    // re-attached to the user message.
    assert_eq!(
        msgs[6].content,
        vec![
            Block::Text {
                text: "what's up?".into()
            },
            png_image_block(),
        ]
    );

    // Second turn completed normally.
    assert_eq!(msgs[7].role, Role::Assistant);
    assert_eq!(msgs[7].stop_reason, Some(StopReason::EndTurn));
}

/// A Common transcript shaped at Grok's native granularity: text blocks and
/// tool calls grouped per assistant record, thinking as its own message,
/// results one per message, stop reasons only on each turn's last assistant
/// message, millisecond timestamps.
fn representable_common() -> Transcript<txcript::Common> {
    let meta = Meta {
        id: SESSION_ID.into(),
        timestamp: ts("2026-07-02T01:24:11.341Z"),
        cwd: Some("/repo".into()),
        git_branch: Some("main".into()),
        title: Some("Repo Q&A".into()),
        cli_version: None,
        model: Some("grok-composer-2.5-fast".into()),
    };
    let model = || Some("grok-composer-2.5-fast".to_string());
    let body = vec![
        Message {
            role: Role::User,
            // Images ride the display log; text-then-images is Grok's
            // native prompt granularity.
            content: vec![
                Block::Text {
                    text: "rename the helper".into(),
                },
                png_image_block(),
            ],
            timestamp: ts("2026-07-02T01:24:12.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![Block::Thinking {
                text: "Find the helper first".into(),
                signature: None,
                encrypted: Some("ENCTOK".into()),
            }],
            timestamp: ts("2026-07-02T01:24:13.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![
                Block::Text {
                    text: "Renaming it now.".into(),
                },
                Block::ToolUse {
                    id: "call-1".into(),
                    tool: Tool::Edit {
                        file_path: "/repo/src/lib.rs".into(),
                        old_string: "fn helper".into(),
                        new_string: "fn assist".into(),
                        replace_all: true,
                    },
                },
                Block::ToolUse {
                    id: "call-2".into(),
                    tool: Tool::Raw {
                        tool_name: "mcp__search__query".into(),
                        input: json!({"q": "helper"}),
                    },
                },
            ],
            timestamp: ts("2026-07-02T01:24:14.000Z"),
            model: model(),
            stop_reason: None,
            usage: None,
        },
        Message {
            role: Role::User,
            content: vec![Block::ToolResult {
                tool_use_id: "call-1".into(),
                content: ToolOutput::Text("edited".into()),
                is_error: false,
            }],
            timestamp: ts("2026-07-02T01:24:15.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        Message {
            role: Role::User,
            content: vec![Block::ToolResult {
                tool_use_id: "call-2".into(),
                content: ToolOutput::Text("no matches".into()),
                is_error: true,
            }],
            timestamp: ts("2026-07-02T01:24:16.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![Block::Text {
                text: "Done.".into(),
            }],
            timestamp: ts("2026-07-02T01:24:17.000Z"),
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
    let native = Grok::from_common(&common).unwrap();
    let back = Grok::to_common(&native).unwrap();
    assert_eq!(back.meta, common.meta);
    assert_eq!(back.body, common.body);
}

#[test]
fn from_common_is_deterministic() {
    let common = representable_common();
    let a = Grok::to_text(&Grok::from_common(&common).unwrap()).unwrap();
    let b = Grok::to_text(&Grok::from_common(&common).unwrap()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn from_common_regenerates_both_logs_and_summary() {
    let native = Grok::from_common(&representable_common()).unwrap();

    // The model log: user + assistant records wrap/collect natively.
    let first_user = native
        .body
        .chat_history
        .iter()
        .find_map(|r| match r {
            ChatRecord::User(l) => Some(l),
            // Only user records carry the re-wrapped prompt.
            ChatRecord::System(_)
            | ChatRecord::Assistant(_)
            | ChatRecord::Reasoning(_)
            | ChatRecord::ToolResult(_)
            | ChatRecord::Other(_) => None,
        })
        .unwrap();
    let text = first_user.content[0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("<user_query>"),
        "user prompts are re-wrapped for Grok: {text}"
    );

    // Display log required for resume UI.
    let kinds: Vec<&str> = native
        .body
        .updates
        .iter()
        .filter_map(|l| l.pointer("/params/update/sessionUpdate")?.as_str())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "user_message_chunk",
            "user_message_chunk", // the prompt's image, as its own chunk
            "agent_thought_chunk",
            "agent_message_chunk",
            "tool_call",
            "tool_call",
            "tool_call_update",
            "tool_call_update",
            "agent_message_chunk",
            "turn_completed",
        ]
    );

    // Native tool naming in the display log.
    let call = native
        .body
        .updates
        .iter()
        .find(|l| {
            l.pointer("/params/update/sessionUpdate")
                .and_then(Value::as_str)
                == Some("tool_call")
        })
        .unwrap();
    assert_eq!(
        call.pointer("/params/update/rawInput/path")
            .and_then(Value::as_str),
        Some("/repo/src/lib.rs"),
        "canonical file_path denormalizes to Grok's `path`"
    );
    assert_eq!(
        call.pointer("/params/update/kind").and_then(Value::as_str),
        Some("edit")
    );

    // summary.json: what `grok sessions` and discovery read.
    let summary = native.body.summary.as_ref().unwrap();
    assert_eq!(
        summary.pointer("/info/id").and_then(Value::as_str),
        Some(SESSION_ID)
    );
    assert_eq!(
        summary.pointer("/info/cwd").and_then(Value::as_str),
        Some("/repo")
    );
    assert_eq!(
        summary.get("generated_title").and_then(Value::as_str),
        Some("Repo Q&A")
    );
}

#[test]
fn aborted_turn_round_trips_via_turn_completed_and_interrupt_flag() {
    let mut common = representable_common();
    // Abort the (only) turn, then open a second one.
    common.body[5].stop_reason = Some(StopReason::Aborted);
    common.body.push(Message {
        role: Role::User,
        content: vec![Block::Text {
            text: "continue".into(),
        }],
        timestamp: ts("2026-07-02T01:24:18.000Z"),
        model: None,
        stop_reason: None,
        usage: None,
    });
    common.body.push(Message {
        role: Role::Assistant,
        content: vec![Block::Text {
            text: "Picking back up.".into(),
        }],
        timestamp: ts("2026-07-02T01:24:19.000Z"),
        model: Some("grok-composer-2.5-fast".into()),
        stop_reason: Some(StopReason::EndTurn),
        usage: None,
    });

    let native = Grok::from_common(&common).unwrap();
    // The second prompt carries Grok's own interrupt marker.
    let interrupted = native.body.chat_history.iter().any(|r| matches!(
        r,
        ChatRecord::User(l) if l.prior_turn_interrupt == Some(Value::String("mid_turn_abort".into()))
    ));
    assert!(interrupted);

    let back = Grok::to_common(&native).unwrap();
    assert_eq!(back.body, common.body);
}

#[test]
fn unparseable_tool_arguments_fall_back_to_raw_string() {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("%2Frepo").join(SESSION_ID);
    std::fs::create_dir_all(&dir).unwrap();
    let record = json!({"type": "assistant", "content": "",
        "tool_calls": [{"id": "call-x", "name": "Weird", "arguments": "not json {"}]});
    std::fs::write(dir.join("chat_history.jsonl"), record.to_string() + "\n").unwrap();
    std::fs::write(dir.join("updates.jsonl"), "").unwrap();

    let store = GrokStore::new(root.path());
    let common = Grok::to_common(&store.load(&dir).unwrap()).unwrap();
    assert_eq!(
        common.body[0].content[0],
        Block::ToolUse {
            id: "call-x".into(),
            tool: Tool::Raw {
                tool_name: "Weird".into(),
                input: Value::String("not json {".into()),
            },
        }
    );

    // And it renders back out as the same raw string.
    let native = Grok::from_common(&common).unwrap();
    let call = native
        .body
        .chat_history
        .iter()
        .find_map(|r| match r {
            ChatRecord::Assistant(l) => l.tool_calls.as_ref(),
            // Tool calls hang off assistant records only.
            ChatRecord::System(_)
            | ChatRecord::User(_)
            | ChatRecord::Reasoning(_)
            | ChatRecord::ToolResult(_)
            | ChatRecord::Other(_) => None,
        })
        .unwrap();
    assert_eq!(call[0]["arguments"].as_str(), Some("not json {"),);
}

/// Grok's `tool_result.content` is a plain string; a structured result (e.g.
/// Claude's block-array outputs) must be flattened, or Grok refuses to load
/// the whole session ("invalid type: sequence, expected a string").
#[test]
fn structured_tool_outputs_flatten_to_strings_in_the_model_log() {
    let mut common = representable_common();
    common.body[3].content = vec![Block::ToolResult {
        tool_use_id: "call-1".into(),
        content: ToolOutput::Json(json!([
            {"type": "text", "text": "first"},
            {"type": "text", "text": "second"},
        ])),
        is_error: false,
    }];
    common.body[4].content = vec![Block::ToolResult {
        tool_use_id: "call-2".into(),
        content: ToolOutput::Json(json!([{"tool_name": "WebFetch", "type": "tool_reference"}])),
        is_error: false,
    }];

    let native = Grok::from_common(&common).unwrap();
    let contents: Vec<&Value> = native
        .body
        .chat_history
        .iter()
        .filter_map(|r| match r {
            ChatRecord::ToolResult(l) => Some(&l.content),
            // Every other record kind carries no tool output.
            ChatRecord::System(_)
            | ChatRecord::User(_)
            | ChatRecord::Assistant(_)
            | ChatRecord::Reasoning(_)
            | ChatRecord::Other(_) => None,
        })
        .collect();
    assert_eq!(
        contents[0],
        &Value::String("first\n\nsecond".into()),
        "block arrays flatten to their text"
    );
    assert_eq!(
        contents[1],
        &Value::String(r#"[{"tool_name":"WebFetch","type":"tool_reference"}]"#.into()),
        "other JSON is stringified"
    );
}

#[test]
fn discovery_ignores_directories_without_session_logs() {
    let root = TempDir::new().unwrap();
    let not_a_session = root.path().join("%2Frepo").join("something-else");
    std::fs::create_dir_all(&not_a_session).unwrap();
    std::fs::write(not_a_session.join("notes.txt"), "hi").unwrap();
    let store = GrokStore::new(root.path());
    assert!(store.discover().unwrap().is_empty());
}

#[test]
fn grok_preserves_stop_reasons_roundtrip() {
    for stop_reason in [
        StopReason::EndTurn,
        StopReason::ToolUse,
        StopReason::MaxTokens,
        StopReason::StopSequence,
        StopReason::Aborted,
        StopReason::Error,
    ] {
        let mut common = representable_common();
        let last_idx = common.body.len() - 1;
        common.body[last_idx].stop_reason = Some(stop_reason.clone());
        let native = Grok::from_common(&common).unwrap();
        let back = Grok::to_common(&native).unwrap();
        assert_eq!(
            back.body[last_idx].stop_reason,
            Some(stop_reason),
            "Grok roundtrip should preserve stop_reason"
        );
    }
}
