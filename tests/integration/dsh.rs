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
    };
    txcript::Transcript::new(meta, body)
}

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
