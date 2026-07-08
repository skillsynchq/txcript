#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Integration tests for the Antigravity (`agy`) `SQLite` trajectory harness.

use chrono::{DateTime, Utc};
use serde_json::json;
#[cfg(feature = "opencode")]
use txcript::Store;
use txcript::common;
use txcript::harness::antigravity;
use txcript::{Codec, Common, TextCodec, Transcript};

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

// -- a tiny protobuf writer mirroring the real on-disk step encoding --------

fn pb_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(u8::try_from(value & 0x7f).unwrap() | 0x80);
        value >>= 7;
    }
    out.push(u8::try_from(value).unwrap());
}

fn pb_field(out: &mut Vec<u8>, field: u64, value: u64) {
    pb_varint(out, field << 3);
    pb_varint(out, value);
}

fn pb_bytes(out: &mut Vec<u8>, field: u64, bytes: &[u8]) {
    pb_varint(out, (field << 3) | 2);
    pb_varint(out, bytes.len() as u64);
    out.extend(bytes);
}

fn pb_str(out: &mut Vec<u8>, field: u64, s: &str) {
    pb_bytes(out, field, s.as_bytes());
}

fn pb_ts(out: &mut Vec<u8>, field: u64, when: DateTime<Utc>) {
    let mut inner = Vec::new();
    pb_field(&mut inner, 1, u64::try_from(when.timestamp()).unwrap());
    pb_bytes(out, field, &inner);
}

/// `CortexStepMetadata` as the CLI writes it: `created_at`, source, an optional
/// tool call, model usage and generator model, and the trajectory step info.
fn step_metadata(
    when: DateTime<Utc>,
    source: u64,
    tool_call: Option<&[u8]>,
    model_usage: bool,
) -> Vec<u8> {
    let mut meta = Vec::new();
    pb_ts(&mut meta, 1, when);
    pb_field(&mut meta, 3, source);
    if let Some(call) = tool_call {
        pb_bytes(&mut meta, 4, call);
    }
    if model_usage {
        let mut usage = Vec::new();
        pb_field(&mut usage, 1, 1020);
        pb_field(&mut usage, 2, 16870);
        pb_field(&mut usage, 3, 275);
        pb_field(&mut usage, 5, 12000);
        pb_bytes(&mut meta, 9, &usage);
        pb_field(&mut meta, 11, 1020);
    }
    meta
}

fn chat_tool_call(id: &str, name: &str, args: &str) -> Vec<u8> {
    let mut call = Vec::new();
    pb_str(&mut call, 1, id);
    pb_str(&mut call, 2, name);
    pb_str(&mut call, 3, args);
    pb_str(&mut call, 9, name);
    call
}

fn step_row(
    idx: i64,
    step_type: i64,
    status: i64,
    metadata: Vec<u8>,
    payload_field: u64,
    payload_body: &[u8],
) -> antigravity::StepRow {
    let mut step_payload = Vec::new();
    pb_field(&mut step_payload, 1, u64::try_from(step_type).unwrap());
    pb_field(&mut step_payload, 4, u64::try_from(status).unwrap());
    pb_bytes(&mut step_payload, 5, &metadata);
    pb_bytes(&mut step_payload, payload_field, payload_body);
    antigravity::StepRow {
        idx,
        step_type,
        status,
        has_subtrajectory: false,
        metadata,
        error_details: Vec::new(),
        permissions: Vec::new(),
        task_details: Vec::new(),
        render_info: Vec::new(),
        step_payload,
        step_format: 0,
    }
}

const SESSION: &str = "3dc412af-b91a-4d9d-82f0-6cd3f991ce2c";
const TRAJECTORY: &str = "fe61a3f1-c177-49aa-ab0b-e266e7ec5be6";

/// A native fixture built from the real sample session's shapes: user input,
/// a planner turn calling `list_dir`, the `LIST_DIRECTORY` result step, a
/// checkpoint, a conversation-history marker, a final planner text turn — and
/// one unmodeled step kind (`TASK_BOUNDARY`, 81) that must survive untouched.
#[allow(clippy::too_many_lines)]
fn native_fixture() -> antigravity::AntigravityDb {
    let t0 = ts("2026-07-07T23:25:41Z");
    let t1 = ts("2026-07-07T23:25:42Z");
    let t2 = ts("2026-07-07T23:25:53Z");

    // steps[0]: USER_INPUT — clean text in user_response (2) and items (3.1).
    let mut user = Vec::new();
    pb_str(&mut user, 2, "hey ag, what's up?");
    let mut item = Vec::new();
    pb_str(&mut item, 1, "hey ag, what's up?");
    pb_bytes(&mut user, 3, &item);
    let user_step = step_row(0, 14, 3, step_metadata(t0, 4, None, false), 19, &user);

    // steps[1]: CONVERSATION_HISTORY — bookkeeping, payload 111 empty.
    let history_step = step_row(1, 98, 3, step_metadata(t0, 5, None, false), 111, &[]);

    // steps[2]: PLANNER_RESPONSE with a list_dir call.
    let call = chat_tool_call(
        "uu95xuqb",
        "list_dir",
        r#"{"DirectoryPath":"/repo","toolAction":"Listing workspace directory","toolSummary":"List directory"}"#,
    );
    let mut planner = Vec::new();
    pb_str(&mut planner, 1, "Let me look around.");
    pb_str(&mut planner, 6, "bot-351f9bd9-7637-4938-8347-803724ea0d9f");
    pb_bytes(&mut planner, 7, &call);
    pb_str(&mut planner, 8, "Let me look around.");
    pb_field(&mut planner, 12, 2);
    let planner_step = step_row(2, 15, 3, step_metadata(t0, 2, None, true), 20, &planner);

    // steps[3]: LIST_DIRECTORY result carrying the same call in its metadata.
    let mut listing = Vec::new();
    pb_str(&mut listing, 1, "file:///repo");
    for (name, is_dir, size) in [("src", true, 0u64), ("Cargo.toml", false, 2632)] {
        let mut entry = Vec::new();
        pb_str(&mut entry, 1, name);
        if is_dir {
            pb_field(&mut entry, 2, 1);
        } else {
            pb_field(&mut entry, 4, size);
        }
        pb_bytes(&mut listing, 3, &entry);
    }
    let list_step = step_row(
        3,
        9,
        3,
        step_metadata(t1, 2, Some(&call), false),
        15,
        &listing,
    );

    // steps[4]: CHECKPOINT — bookkeeping; its user_intent seeds the title.
    let mut checkpoint = Vec::new();
    pb_str(&mut checkpoint, 4, "Casual Friendly Greeting");
    pb_field(&mut checkpoint, 9, 1);
    let checkpoint_step = step_row(4, 23, 3, step_metadata(t1, 5, None, false), 30, &checkpoint);

    // steps[5]: final PLANNER_RESPONSE, text only.
    let mut closing = Vec::new();
    pb_str(&mut closing, 1, "This repo converts transcripts.");
    pb_str(&mut closing, 8, "This repo converts transcripts.");
    pb_field(&mut closing, 12, 2);
    let closing_step = step_row(5, 15, 3, step_metadata(t2, 2, None, true), 20, &closing);

    // steps[6]: unmodeled TASK_BOUNDARY (81) with opaque payload bytes.
    let unmodeled = step_row(
        6,
        81,
        3,
        step_metadata(t2, 5, None, false),
        93,
        &[0x08, 0x01],
    );

    // The trajectory metadata blob: workspace, created_at, conversation id.
    let mut workspace = Vec::new();
    pb_str(&mut workspace, 1, "file:///repo");
    pb_str(&mut workspace, 2, "file:///repo");
    pb_str(&mut workspace, 4, "main");
    let mut main_blob = Vec::new();
    pb_bytes(&mut main_blob, 1, &workspace);
    pb_ts(&mut main_blob, 2, t0);
    pb_str(&mut main_blob, 6, SESSION);
    pb_str(&mut main_blob, 7, "file:///repo");
    pb_str(&mut main_blob, 18, "default-cli-project");

    antigravity::AntigravityDb {
        trajectory_meta: vec![antigravity::TrajectoryMetaRow {
            trajectory_id: TRAJECTORY.into(),
            cascade_id: SESSION.into(),
            trajectory_type: 4,
            source: 17,
        }],
        steps: vec![
            user_step,
            history_step,
            planner_step,
            list_step,
            checkpoint_step,
            closing_step,
            unmodeled,
        ],
        gen_metadata: vec![antigravity::SizedBlobRow {
            idx: 0,
            data: vec![0x0a, 0x02, 0x08, 0x01],
            size: 4,
        }],
        executor_metadata: vec![antigravity::IndexedBlobRow {
            idx: 0,
            data: vec![0x08, 0x04],
        }],
        parent_references: Vec::new(),
        battle_mode_infos: Vec::new(),
        trajectory_metadata_blob: vec![antigravity::KeyedBlobRow {
            id: "main".into(),
            data: main_blob,
        }],
        transcript: Some("{\"step_index\":0}\n".into()),
        transcript_full: Some("{\"step_index\":0,\"full\":true}\n".into()),
    }
}

fn native_transcript() -> Transcript<antigravity::Antigravity> {
    let text = serde_json::to_string(&native_fixture()).unwrap();
    antigravity::Antigravity::from_text(&text).unwrap()
}

// -- store ------------------------------------------------------------------

#[cfg(feature = "opencode")]
#[test]
fn store_round_trip_is_lossless_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let store = antigravity::AntigravityStore::new(dir.path());

    let saved = store.save(&native_transcript()).unwrap();
    assert_eq!(saved.id, SESSION);
    assert_eq!(
        saved.reference,
        dir.path()
            .join("conversations")
            .join(format!("{SESSION}.db")),
        "save path shape"
    );
    assert!(
        dir.path().join("brain").join(SESSION).is_dir(),
        "brain directory must exist or the CLI wedges on resume"
    );

    let loaded = store.load(&saved.reference).unwrap();
    assert_eq!(loaded.body, native_fixture());
    assert_eq!(loaded.meta.id, SESSION);

    // Save the loaded copy again: identical records both times.
    let resaved = store.save(&loaded).unwrap();
    let reloaded = store.load(&resaved.reference).unwrap();
    assert_eq!(reloaded.body, loaded.body);
}

#[cfg(feature = "opencode")]
#[test]
fn discover_extracts_metadata_and_sniffs_format() {
    let dir = tempfile::tempdir().unwrap();
    let store = antigravity::AntigravityStore::new(dir.path());
    store.save(&native_transcript()).unwrap();

    // Neighbors that must be skipped: an old-format .pb file and a foreign
    // SQLite database under .db.
    let conversations = dir.path().join("conversations");
    std::fs::write(conversations.join("old-format.pb"), b"\x0a\x02\x08\x01").unwrap();
    let foreign = rusqlite::Connection::open(conversations.join("foreign.db")).unwrap();
    foreign
        .execute_batch("CREATE TABLE unrelated (x); INSERT INTO unrelated VALUES (1);")
        .unwrap();
    drop(foreign);

    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    let meta = &found[0].meta;
    assert_eq!(meta.id, SESSION);
    assert_eq!(meta.timestamp, ts("2026-07-07T23:25:41Z"));
    assert_eq!(meta.cwd.as_deref(), Some("/repo"));
    assert_eq!(meta.git_branch.as_deref(), Some("main"));
    assert_eq!(meta.title.as_deref(), Some("Casual Friendly Greeting"));
    assert_eq!(meta.model.as_deref(), Some("model-1020"));
}

#[cfg(feature = "opencode")]
#[test]
fn delete_removes_db_and_brain_dir() {
    let dir = tempfile::tempdir().unwrap();
    let store = antigravity::AntigravityStore::new(dir.path());
    let saved = store.save(&native_transcript()).unwrap();
    assert!(saved.reference.exists());

    store.delete(&saved.reference).unwrap();
    assert!(!saved.reference.exists());
    assert!(!dir.path().join("brain").join(SESSION).exists());
    assert!(store.discover().unwrap().is_empty());
}

// -- to_common ---------------------------------------------------------------

#[test]
fn to_common_extracts_conversation_and_skips_bookkeeping() {
    let common = antigravity::Antigravity::to_common(&native_transcript()).unwrap();
    // user, planner, list_dir result, final planner — history/checkpoint/
    // unmodeled steps are bookkeeping.
    assert_eq!(common.body.len(), 4);

    let user = &common.body[0];
    assert_eq!(user.role, common::Role::User);
    assert_eq!(
        user.content,
        vec![common::Block::Text {
            text: "hey ag, what's up?".into()
        }]
    );
    assert_eq!(user.timestamp, ts("2026-07-07T23:25:41Z"));

    let planner = &common.body[1];
    assert_eq!(planner.role, common::Role::Assistant);
    assert_eq!(planner.model.as_deref(), Some("model-1020"));
    assert_eq!(planner.stop_reason, Some(common::StopReason::EndTurn));
    assert_eq!(
        planner.usage,
        Some(common::Usage {
            input_tokens: 16870,
            output_tokens: 275,
            cache_read_input_tokens: Some(12000),
            cache_creation_input_tokens: None,
            cost_usd: None,
        })
    );
    assert_eq!(planner.content.len(), 2);
    assert_eq!(
        planner.content[0],
        common::Block::Text {
            text: "Let me look around.".into()
        }
    );
    // list_dir has no canonical equivalent: Raw with untouched native args.
    match &planner.content[1] {
        common::Block::ToolUse { id, tool } => {
            assert_eq!(id, "uu95xuqb");
            match tool {
                common::Tool::Raw { tool_name, input } => {
                    assert_eq!(tool_name, "list_dir");
                    assert_eq!(input["DirectoryPath"], "/repo");
                    assert_eq!(input["toolAction"], "Listing workspace directory");
                }
                other => panic!("expected Raw list_dir, got {other:?}"),
            }
        }
        other => panic!("expected ToolUse, got {other:?}"),
    }

    let result = &common.body[2];
    assert_eq!(result.role, common::Role::User);
    assert_eq!(
        result.content,
        vec![common::Block::ToolResult {
            tool_use_id: "uu95xuqb".into(),
            content: common::ToolOutput::Json(json!({
                "path": "/repo",
                "entries": [
                    { "name": "src", "is_dir": true },
                    { "name": "Cargo.toml", "size_bytes": 2632 },
                ],
            })),
            is_error: false,
        }]
    );

    let closing = &common.body[3];
    assert_eq!(
        closing.content,
        vec![common::Block::Text {
            text: "This repo converts transcripts.".into()
        }]
    );
}

#[test]
fn to_common_types_native_tools_and_strips_display_keys() {
    // A run_command call as the CLI writes it maps to a typed Bash with the
    // fixed WaitMsBeforeAsync default and display keys stripped.
    let t0 = ts("2026-07-07T23:25:41Z");
    let call = chat_tool_call(
        "qnt5r63x",
        "run_command",
        r#"{"CommandLine":"pwd","Cwd":"/scratch","WaitMsBeforeAsync":2000,"toolAction":"Checking cwd","toolSummary":"Get cwd"}"#,
    );
    let mut planner = Vec::new();
    pb_bytes(&mut planner, 7, &call);
    let planner_step = step_row(0, 15, 3, step_metadata(t0, 2, None, false), 20, &planner);

    // The failed command: nonzero exit code, output in combined_output.full.
    let mut run = Vec::new();
    pb_str(&mut run, 2, "/scratch");
    pb_field(&mut run, 6, 1);
    let mut output = Vec::new();
    pb_str(&mut output, 1, "cat: nope: No such file or directory\n");
    pb_bytes(&mut run, 21, &output);
    pb_str(&mut run, 23, "pwd");
    let run_step = step_row(1, 21, 3, step_metadata(t0, 2, Some(&call), false), 28, &run);

    let body = antigravity::AntigravityDb {
        steps: vec![planner_step, run_step],
        ..empty_db()
    };
    let text = serde_json::to_string(&body).unwrap();
    let common =
        antigravity::Antigravity::to_common(&antigravity::Antigravity::from_text(&text).unwrap())
            .unwrap();

    assert_eq!(common.body.len(), 2);
    match &common.body[0].content[0] {
        common::Block::ToolUse { tool, .. } => assert_eq!(
            *tool,
            common::Tool::Bash {
                command: "pwd".into(),
                workdir: Some("/scratch".into()),
                timeout_ms: None,
                description: None,
                run_in_background: false,
            }
        ),
        other => panic!("expected ToolUse, got {other:?}"),
    }
    assert_eq!(
        common.body[1].content,
        vec![common::Block::ToolResult {
            tool_use_id: "qnt5r63x".into(),
            content: common::ToolOutput::Text("cat: nope: No such file or directory\n".into()),
            is_error: true,
        }]
    );
}

fn empty_db() -> antigravity::AntigravityDb {
    antigravity::AntigravityDb {
        trajectory_meta: vec![antigravity::TrajectoryMetaRow {
            trajectory_id: TRAJECTORY.into(),
            cascade_id: SESSION.into(),
            trajectory_type: 4,
            source: 17,
        }],
        steps: Vec::new(),
        gen_metadata: Vec::new(),
        executor_metadata: Vec::new(),
        parent_references: Vec::new(),
        battle_mode_infos: Vec::new(),
        trajectory_metadata_blob: Vec::new(),
        transcript: None,
        transcript_full: None,
    }
}

// -- fixpoint ----------------------------------------------------------------

/// A Common transcript shaped at Antigravity's native granularity: per-step
/// messages, `model-<id>` model strings, derived edit results, text command
/// output.
#[allow(clippy::too_many_lines)]
fn fixpoint_common() -> Transcript<Common> {
    let meta = common::Meta {
        id: "9b6e7a44-6f10-4c2e-8f7e-2b1a0d3c5e91".into(),
        timestamp: ts("2026-07-07T20:00:00Z"),
        cwd: Some("/repo".into()),
        git_branch: Some("main".into()),
        title: None,
        cli_version: None,
        model: Some("model-1020".into()),
    };
    let model = || Some("model-1020".to_string());
    let usage = common::Usage {
        input_tokens: 100,
        output_tokens: 20,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: None,
        cost_usd: None,
    };
    let body = vec![
        common::Message {
            role: common::Role::User,
            content: vec![
                common::Block::Text {
                    text: "please fix the loop".into(),
                },
                common::Block::Image {
                    source: common::ImageSource {
                        source_type: "base64".into(),
                        media_type: "image/png".into(),
                        data: "aGVsbG8=".into(),
                    },
                },
            ],
            timestamp: ts("2026-07-07T20:00:01Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![
                common::Block::Thinking {
                    text: "the loop is off by one".into(),
                    signature: Some("sig-1".into()),
                    encrypted: None,
                },
                common::Block::Text {
                    text: "Looking at the file.".into(),
                },
                common::Block::ToolUse {
                    id: "call-read".into(),
                    tool: common::Tool::Read {
                        file_path: "/repo/a.rs".into(),
                        offset: Some(1),
                        limit: Some(3),
                    },
                },
            ],
            timestamp: ts("2026-07-07T20:00:02Z"),
            model: model(),
            stop_reason: Some(common::StopReason::EndTurn),
            usage: Some(usage),
        },
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::ToolResult {
                tool_use_id: "call-read".into(),
                content: common::ToolOutput::Text("for i in 0..=n {\n".into()),
                is_error: false,
            }],
            timestamp: ts("2026-07-07T20:00:03Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![
                common::Block::Text {
                    text: "Fixing it.".into(),
                },
                common::Block::ToolUse {
                    id: "call-edit".into(),
                    tool: common::Tool::Edit {
                        file_path: "/repo/a.rs".into(),
                        old_string: "0..=n".into(),
                        new_string: "0..n".into(),
                        replace_all: false,
                    },
                },
            ],
            timestamp: ts("2026-07-07T20:00:04Z"),
            model: model(),
            stop_reason: Some(common::StopReason::EndTurn),
            usage: Some(usage),
        },
        common::Message {
            role: common::Role::User,
            // The canonical derived form — the only edit result the native
            // CODE_ACTION step can represent.
            content: vec![common::Block::ToolResult {
                tool_use_id: "call-edit".into(),
                content: common::ToolOutput::Json(json!({"file": "/repo/a.rs", "edited": true})),
                is_error: false,
            }],
            timestamp: ts("2026-07-07T20:00:05Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![
                common::Block::Text {
                    text: "Verifying.".into(),
                },
                common::Block::ToolUse {
                    id: "call-bash".into(),
                    tool: common::Tool::Bash {
                        command: "cargo test".into(),
                        workdir: Some("/repo".into()),
                        timeout_ms: None,
                        description: None,
                        run_in_background: false,
                    },
                },
                common::Block::ToolUse {
                    id: "call-todo".into(),
                    tool: common::Tool::Raw {
                        tool_name: "TodoWrite".into(),
                        input: json!({"todos": [{"content": "ship it"}]}),
                    },
                },
            ],
            timestamp: ts("2026-07-07T20:00:06Z"),
            model: model(),
            stop_reason: Some(common::StopReason::EndTurn),
            usage: Some(usage),
        },
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::ToolResult {
                tool_use_id: "call-bash".into(),
                content: common::ToolOutput::Text("error: test failed\n".into()),
                is_error: true,
            }],
            timestamp: ts("2026-07-07T20:00:07Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::ToolResult {
                tool_use_id: "call-todo".into(),
                content: common::ToolOutput::Text("recorded".into()),
                is_error: false,
            }],
            timestamp: ts("2026-07-07T20:00:07Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        // A final turn with an in-flight call: no result step content.
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::ToolUse {
                id: "call-pending".into(),
                tool: common::Tool::Bash {
                    command: "sleep 60".into(),
                    workdir: None,
                    timeout_ms: None,
                    description: None,
                    run_in_background: false,
                },
            }],
            timestamp: ts("2026-07-07T20:00:08Z"),
            model: model(),
            stop_reason: Some(common::StopReason::EndTurn),
            usage: Some(usage),
        },
    ];
    Transcript::new(meta, body)
}

#[test]
fn codec_fixpoint_through_common_loses_nothing() {
    let original = fixpoint_common();
    let native = antigravity::Antigravity::from_common(&original).unwrap();
    let round = antigravity::Antigravity::to_common(&native).unwrap();
    assert_eq!(round.meta, original.meta);
    assert_eq!(round.body, original.body);
}

#[test]
fn free_form_edit_results_survive_via_the_generic_carrier() {
    // Foreign harnesses report edits with free-form text; CODE_ACTION cannot
    // carry it, so the pair rides CortexStepGeneric and must round trip.
    let mut transcript = fixpoint_common();
    transcript.body[4].content = vec![common::Block::ToolResult {
        tool_use_id: "call-edit".into(),
        content: common::ToolOutput::Text("The file /repo/a.rs has been updated.".into()),
        is_error: false,
    }];
    let native = antigravity::Antigravity::from_common(&transcript).unwrap();
    let round = antigravity::Antigravity::to_common(&native).unwrap();
    assert_eq!(round.body, transcript.body);
}

#[test]
fn from_common_is_deterministic() {
    let common = fixpoint_common();
    let a = antigravity::Antigravity::from_common(&common).unwrap();
    let b = antigravity::Antigravity::from_common(&common).unwrap();
    assert_eq!(
        antigravity::Antigravity::to_text(&a).unwrap(),
        antigravity::Antigravity::to_text(&b).unwrap()
    );
}

#[test]
fn text_codec_round_trips_the_body() {
    let transcript = native_transcript();
    let text = antigravity::Antigravity::to_text(&transcript).unwrap();
    let back = antigravity::Antigravity::from_text(&text).unwrap();
    assert_eq!(back.body, transcript.body);
    assert_eq!(back.meta.id, SESSION);
}
