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
fn store_discovers_and_loads_a_session() {
    let root = tempfile::tempdir().unwrap();
    let session = root.path().join("--repo--").join("session-abc");
    write_session(&session, &sample_body());
    let store = dsh::DshStore::new(root.path());
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].meta.id, "session-abc");
    let loaded = store.load(&found[0].reference).unwrap();
    assert_eq!(loaded.body, sample_body());
}

fn saveable(id: &str, cwd: &str) -> txcript::Transcript<dsh::Dsh> {
    let mut body = sample_body();
    body.header["id"] = json!(id);
    body.header["cwd"] = json!(cwd);
    let meta = common::Meta {
        id: id.into(),
        timestamp: ts("2024-01-01T00:04:05.000Z"),
        cwd: Some(cwd.into()),
        git_branch: None,
        title: Some("Parser work".into()),
        cli_version: None,
        model: None,
        lineage: None,
    };
    txcript::Transcript::new(meta, body)
}

#[cfg(feature = "dsh_zstd")]
#[test]
fn save_writes_the_layout_dsh_scans_for() {
    // dsh finds sessions by walking `<root>/<project key>/<encoded id>/`, and
    // then asserts the header's own id and cwd name that exact path. A layout
    // that disagrees is not merely skipped — it fails dsh's whole listing.
    let root = tempfile::tempdir().unwrap();
    let store = dsh::DshStore::new(root.path());
    let saved = store
        .save(&saveable("session-abc", "/repo/Mobile Documents/想法"))
        .unwrap();

    let expected = root
        .path()
        .join("--repo-Mobile~0020Documents-~60F3~6CD5--")
        .join("session-abc");
    assert_eq!(saved.reference, expected);
    assert!(expected.join("session.jsonl.zstd").is_file());
}

#[cfg(feature = "dsh_zstd")]
#[test]
fn saved_session_is_discovered_and_loads_back() {
    let root = tempfile::tempdir().unwrap();
    let store = dsh::DshStore::new(root.path());
    let original = saveable("session-abc", "/repo");
    store.save(&original).unwrap();

    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].meta.id, "session-abc");

    let loaded = store.load(&found[0].reference).unwrap();
    assert_eq!(loaded.body, original.body);
}

/// Decode only the first Zstandard frame of a written log, the way dsh's
/// `assertZstdHeaderFrame` does.
#[cfg(feature = "dsh_zstd")]
fn first_frame(log: &std::path::Path) -> String {
    use std::io::Read as _;

    let bytes = std::fs::read(log).unwrap();
    let mut decoder = zstd::stream::read::Decoder::new(std::io::Cursor::new(bytes))
        .unwrap()
        .single_frame();
    let mut first = String::new();
    decoder.read_to_string(&mut first).unwrap();
    first
}

#[cfg(feature = "dsh_zstd")]
#[test]
fn the_first_frame_holds_exactly_the_header_line() {
    // dsh reads a session's metadata by decompressing one frame and requiring
    // it to be a single line. Compressing the whole log as one frame still
    // decodes, and still round-trips through txcript, but dsh rejects it —
    // the frame split is part of the format.
    let root = tempfile::tempdir().unwrap();
    let store = dsh::DshStore::new(root.path());
    let saved = store.save(&saveable("session-abc", "/repo")).unwrap();

    let first = first_frame(&saved.reference.join("session.jsonl.zstd"));
    assert_eq!(first.matches('\n').count(), 1);
    assert!(first.ends_with('\n'));
    let header: serde_json::Value = serde_json::from_str(first.trim_end()).unwrap();
    assert_eq!(header["type"], json!("session"));
    assert_eq!(header["id"], json!("session-abc"));
}

#[cfg(feature = "dsh_zstd")]
#[test]
fn save_stamps_the_header_with_the_identity_that_built_the_path() {
    // dsh cross-checks every log it finds against the path its own header
    // would name, and a mismatch fails its entire listing rather than skipping
    // the one session. A copy given a new id must not keep the old one.
    let root = tempfile::tempdir().unwrap();
    let store = dsh::DshStore::new(root.path());
    let mut transcript = saveable("session-abc", "/repo");
    transcript.meta.id = "session-copy".into();
    transcript.meta.cwd = Some("/elsewhere".into());

    let saved = store.save(&transcript).unwrap();
    assert_eq!(saved.id, "session-copy");
    assert_eq!(
        saved.reference,
        root.path().join("--elsewhere--").join("session-copy")
    );

    let header: serde_json::Value =
        serde_json::from_str(first_frame(&saved.reference.join("session.jsonl.zstd")).trim_end())
            .unwrap();
    assert_eq!(header["id"], json!("session-copy"));
    assert_eq!(header["cwd"], json!("/elsewhere"));
}

#[test]
fn save_follows_the_encoding_the_root_already_uses() {
    // A root that mixes `.jsonl` and `.jsonl.zstd` does not merely hide the
    // odd session: dsh refuses to list *any* session in it, under either
    // configuration. Writing the default encoding into a plaintext root would
    // take the user's whole session list down, so the root decides.
    let root = tempfile::tempdir().unwrap();
    write_session(
        &root.path().join("--repo--").join("session-old"),
        &sample_body(),
    );

    let store = dsh::DshStore::new(root.path());
    let saved = store.save(&saveable("session-abc", "/repo")).unwrap();
    assert!(saved.reference.join("session.jsonl").is_file());
    assert!(!saved.reference.join("session.jsonl.zstd").exists());

    // And it still reads back.
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 2);
}

#[cfg(feature = "dsh_zstd")]
#[test]
fn saving_under_a_new_cwd_leaves_no_duplicate_id_behind() {
    // A project directory is keyed by cwd, so re-saving a session whose cwd
    // changed lands it somewhere new. dsh treats one id appearing under two
    // project directories as corruption and fails its whole listing, so the
    // old copy has to go.
    let root = tempfile::tempdir().unwrap();
    let store = dsh::DshStore::new(root.path());
    let first = store.save(&saveable("session-abc", "/a")).unwrap();
    let second = store.save(&saveable("session-abc", "/b")).unwrap();

    assert_ne!(first.reference, second.reference);
    assert!(!first.reference.exists());
    assert_eq!(store.discover().unwrap().len(), 1);
}

#[cfg(feature = "dsh_zstd")]
#[test]
fn a_failed_save_leaves_the_previous_copy_in_place() {
    // Re-saving under a new cwd retires the copy under the old project
    // directory — but only once the replacement is on disk. If writing the
    // replacement fails, the old session must still be there.
    let root = tempfile::tempdir().unwrap();
    let store = dsh::DshStore::new(root.path());
    let first = store.save(&saveable("session-abc", "/a")).unwrap();

    // Learn where `/b` sessions go, then block that project directory with a
    // plain file so the replacement cannot be created.
    let scratch = tempfile::tempdir().unwrap();
    let probe = dsh::DshStore::new(scratch.path())
        .save(&saveable("probe", "/b"))
        .unwrap();
    let project = probe.reference.parent().unwrap().file_name().unwrap();
    std::fs::write(root.path().join(project), b"").unwrap();

    assert!(store.save(&saveable("session-abc", "/b")).is_err());
    assert!(
        first.reference.join("session.jsonl.zstd").is_file(),
        "the previous copy must survive a failed save"
    );
    assert_eq!(store.discover().unwrap().len(), 1);
}

#[cfg(feature = "dsh_zstd")]
#[test]
fn save_refuses_an_id_that_would_escape_the_store() {
    let root = tempfile::tempdir().unwrap();
    let store = dsh::DshStore::new(root.path());
    // dsh encodes rather than rejects a traversing id, so the escape attempt
    // becomes a literal directory name and stays inside the store.
    let saved = store.save(&saveable("../../evil", "/repo")).unwrap();
    assert!(saved.reference.starts_with(root.path()));
    // A dot is a safe character; only a segment that is entirely `.` or `..`
    // is special-cased. Escaping the separators is what contains the traversal.
    assert_eq!(saved.reference.file_name().unwrap(), "..~002F..~002Fevil");
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

fn message(role: common::Role, at: &str, content: Vec<Block>) -> common::Message {
    common::Message {
        role,
        content,
        timestamp: ts(at),
        model: None,
        stop_reason: None,
        usage: None,
    }
}

fn tool_use(id: &str) -> Block {
    Block::ToolUse {
        id: id.into(),
        tool: common::Tool::from_canonical("bash", json!({ "command": "ls" })),
    }
}

/// Two turns: the first answers one tool call and moves to a second step;
/// the second leaves its call unanswered.
fn tool_session() -> txcript::Transcript<Common> {
    use common::Role::{Assistant, User};
    let mut transcript = common_sample();
    transcript.body.extend([
        message(
            Assistant,
            "2024-01-01T00:00:47.000Z",
            vec![tool_use("call-1")],
        ),
        message(
            User,
            "2024-01-01T00:00:48.000Z",
            vec![Block::ToolResult {
                tool_use_id: "call-1".into(),
                content: common::ToolOutput::Text("a\nb".into()),
                is_error: false,
            }],
        ),
        message(
            Assistant,
            "2024-01-01T00:00:49.000Z",
            vec![Block::Text {
                text: "two files".into(),
            }],
        ),
        message(
            User,
            "2024-01-01T00:00:50.000Z",
            vec![Block::Text {
                text: "again".into(),
            }],
        ),
        message(
            Assistant,
            "2024-01-01T00:00:51.000Z",
            vec![tool_use("call-2")],
        ),
    ]);
    transcript
}

fn event_types(body: &dsh::DshSession) -> Vec<&str> {
    body.events
        .iter()
        .map(|event| event["type"].as_str().unwrap())
        .collect()
}

#[test]
fn from_common_writes_the_v4_turn_and_step_lifecycle() {
    let native = dsh::Dsh::from_common(&tool_session()).unwrap();
    assert_eq!(native.body.header["version"], json!(4));
    assert_eq!(native.body.header["isSeeded"], json!(false));
    assert_eq!(
        event_types(&native.body),
        [
            "session/title",
            "turn/start",
            "step/start",
            "system/message",
            "user/message",
            "assistant/message",
            "tool/call",
            "tool/result",
            "step/end",
            "step/start",
            "assistant/message",
            "step/end",
            "turn/end",
            "turn/start",
            "step/start",
            "user/message",
            "assistant/message",
            "tool/result",
            "step/end",
            "turn/end",
        ]
    );
    for (seq, event) in native.body.events.iter().enumerate() {
        assert_eq!(event["seq"], json!(seq));
    }
    let events = &native.body.events;
    // A result cites the `tool/call` that started it, and is a tool-role
    // message rather than a wrapped block.
    assert_eq!(events[7]["sourceEventSeqs"], json!([6]));
    assert_eq!(events[7]["data"]["message"]["role"], json!("tool"));
    assert_eq!(events[7]["data"]["message"]["toolCallId"], json!("call-1"));
    // An unanswered call is closed with dsh's own not-started repair, and
    // the turn it ends is recorded as interrupted.
    assert_eq!(
        events[17]["data"]["message"]["id"],
        json!("interrupted-tool-result-call-2-17")
    );
    assert_eq!(
        events[17]["data"]["error"]["code"],
        json!("TOOL_NOT_STARTED")
    );
    assert!(events[17].get("sourceEventSeqs").is_none());
    assert_eq!(events[19]["data"]["reason"]["kind"], json!("interrupted"));
    assert_eq!(events[12]["data"]["reason"]["kind"], json!("completed"));
    // Every model source names a provider and model, and the stream is
    // present but empty.
    assert_eq!(events[5]["data"]["stream"], json!([]));
    assert_eq!(
        events[5]["data"]["message"]["source"]["model"],
        json!("deepseek-v4-flash")
    );
}

#[test]
fn from_common_round_trips_tool_calls_and_results() {
    let native = dsh::Dsh::from_common(&tool_session()).unwrap();
    let common = dsh::Dsh::to_common(&native).unwrap();
    let blocks: Vec<&Block> = common.body.iter().flat_map(|m| &m.content).collect();
    assert!(matches!(blocks[1], Block::ToolUse { id, .. } if id == "call-1"));
    assert!(matches!(
        blocks[2],
        Block::ToolResult { tool_use_id, is_error: false, .. } if tool_use_id == "call-1"
    ));
    assert!(matches!(
        blocks.last().unwrap(),
        Block::ToolResult { tool_use_id, is_error: true, .. } if tool_use_id == "call-2"
    ));
}

#[test]
fn save_names_the_log_for_its_header_version() {
    let root = tempfile::tempdir().unwrap();
    let store = dsh::DshStore::new(root.path());
    // Fix the root to plaintext so the test does not depend on `dsh_zstd`.
    write_session(
        &root.path().join("--elsewhere--").join("session-other"),
        &sample_body(),
    );

    let native = dsh::Dsh::from_common(&tool_session()).unwrap();
    let saved = store.save(&native).unwrap();
    assert!(saved.reference.join("session.v4.jsonl").is_file());

    // A re-save replaces the session, so another generation beside it goes.
    std::fs::write(saved.reference.join("session.v1.jsonl"), "stale").unwrap();
    store.save(&native).unwrap();
    assert!(!saved.reference.join("session.v1.jsonl").exists());

    // A native body keeps its own version, and so its own file name.
    let mut older = native.clone();
    older.body.header["version"] = json!(1);
    store.save(&older).unwrap();
    assert!(saved.reference.join("session.v1.jsonl").is_file());
    assert!(!saved.reference.join("session.v4.jsonl").exists());
}
