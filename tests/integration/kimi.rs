#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Integration tests for the Kimi Code harness.

use chrono::{DateTime, Utc};
use serde_json::json;
use txcript::common;
use txcript::harness::kimi;
use txcript::{Codec, Common, Store, TextCodec};

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn event(inner: &serde_json::Value, time: i64) -> serde_json::Value {
    json!({"type": "context.append_loop_event", "time": time, "event": inner.clone()})
}

fn sample_body() -> kimi::KimiSession {
    kimi::KimiSession {
        state: json!({
            "createdAt": "2026-01-02T03:04:05.000Z",
            "title": "Parser work",
            "workDir": "/repo"
        }),
        wire: vec![
            json!({"type": "metadata", "protocol_version": "1.4"}),
            json!({"type": "llm.request", "model": "kimi-k2", "time": 1_767_323_045_000_i64}),
            json!({"type": "context.append_message", "time": 1_767_323_046_000_i64,
                "message": {"role": "user", "content": [{"type": "text", "text": "edit the file"}]}}),
            event(
                &json!({"type": "content.part", "part": {"type": "think", "think": "inspect it"}}),
                1_767_323_047_000_i64,
            ),
            event(
                &json!({"type": "content.part", "part": {"type": "text", "text": "On it."}}),
                1_767_323_047_000_i64,
            ),
            event(
                &json!({"type": "tool.call", "toolCallId": "call-1", "name": "Edit",
                "args": {"file_path": "/repo/a.rs", "old_string": "old", "new_string": "new"}}),
                1_767_323_047_000_i64,
            ),
            event(
                &json!({"type": "tool.result", "toolCallId": "call-1",
                "result": {"output": "done", "isError": false}}),
                1_767_323_048_000_i64,
            ),
            event(
                &json!({"type": "content.part", "part": {"type": "text", "text": "finished"}}),
                1_767_323_049_000_i64,
            ),
            // An unknown event must remain in the native body.
            json!({"type": "future.kimi.event", "payload": {"v": 1}}),
        ],
    }
}

#[test]
fn metadata_and_messages_are_converted() {
    let body = sample_body();
    let meta = common::Meta {
        id: "abc".into(),
        timestamp: ts("2026-01-02T03:04:05.000Z"),
        cwd: Some("/repo".into()),
        git_branch: None,
        title: Some("Parser work".into()),
        cli_version: None,
        model: Some("kimi-k2".into()),
    };
    let native = txcript::Transcript::new(meta, body);
    // Build through TextCodec so the test exercises the public native text API.
    let text = kimi::Kimi::to_text(&native).unwrap();
    let parsed = kimi::Kimi::from_text(&text).unwrap();
    assert_eq!(parsed.meta.cwd.as_deref(), Some("/repo"));
    assert_eq!(parsed.meta.title.as_deref(), Some("Parser work"));
    assert_eq!(parsed.meta.model.as_deref(), Some("kimi-k2"));

    let common = kimi::Kimi::to_common(&parsed).unwrap();
    assert_eq!(common.body.len(), 4);
    assert!(matches!(
        common.body[1].content[0],
        common::Block::Thinking { .. }
    ));
    assert!(matches!(
        common.body[1].content[2],
        common::Block::ToolUse { .. }
    ));
    assert!(matches!(
        common.body[2].content[0],
        common::Block::ToolResult { .. }
    ));
    assert_eq!(common.body[3].timestamp, ts("2026-01-02T03:04:09.000Z"));
}

#[test]
fn text_codec_recovers_the_id_without_a_directory() {
    // The store can fall back to the session directory name, but `from_text`
    // (and the wasm parser on top of it) only sees the JSON. Schema version 2
    // carries `id`; version 1 only carries the agent home directory.
    let mut v2 = sample_body();
    v2.state = json!({"version": 2, "id": "session_abc", "cwd": "/repo",
        "createdAt": "2026-01-02T03:04:05.000Z"});
    let rendered = kimi::Kimi::to_text(&txcript::Transcript::new(
        common::Meta {
            id: "session_abc".into(),
            timestamp: ts("2026-01-02T03:04:05.000Z"),
            cwd: Some("/repo".into()),
            git_branch: None,
            title: None,
            cli_version: None,
            model: None,
        },
        v2,
    ))
    .unwrap();
    assert_eq!(
        kimi::Kimi::from_text(&rendered).unwrap().meta.id,
        "session_abc"
    );

    let mut v1 = sample_body();
    v1.state = json!({"workDir": "/repo", "createdAt": "2026-01-02T03:04:05.000Z",
        "agents": {"main": {"homedir": "/home/u/.kimi-code/sessions/wd_repo_1/session_abc/agents/main"}}});
    let rendered = kimi::Kimi::to_text(&txcript::Transcript::new(
        common::Meta {
            id: "session_abc".into(),
            timestamp: ts("2026-01-02T03:04:05.000Z"),
            cwd: Some("/repo".into()),
            git_branch: None,
            title: None,
            cli_version: None,
            model: None,
        },
        v1,
    ))
    .unwrap();
    assert_eq!(
        kimi::Kimi::from_text(&rendered).unwrap().meta.id,
        "session_abc"
    );
}

#[test]
fn native_text_round_trip_retains_unknown_wire_events() {
    let body = sample_body();
    let meta = common::Meta {
        id: "session-id".into(),
        timestamp: ts("2026-01-02T03:04:05.000Z"),
        cwd: Some("/repo".into()),
        git_branch: None,
        title: Some("Parser work".into()),
        cli_version: None,
        model: Some("kimi-k2".into()),
    };
    let transcript = txcript::Transcript::new(meta, body.clone());
    let text = kimi::Kimi::to_text(&transcript).unwrap();
    let parsed = kimi::Kimi::from_text(&text).unwrap();
    assert_eq!(parsed.body, body);
    assert!(text.contains("future.kimi.event"));
}

#[test]
fn store_discovers_and_loads_a_session() {
    let root = tempfile::tempdir().unwrap();
    let session = root.path().join("wd_repo_hash").join("session_abc");
    write_session(&session, &sample_body());
    let body = sample_body();

    let store = kimi::KimiStore::new(root.path());
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].meta.id, "session_abc");
    assert_eq!(found[0].meta.timestamp, ts("2026-01-02T03:04:05.000Z"));

    let loaded = store.load(&found[0].reference).unwrap();
    assert_eq!(loaded.body, body);
}

#[test]
fn discovery_does_not_depend_on_the_directory_name() {
    // Discovery is gated on structure — a state.json and a main wire log —
    // exactly like every other file-backed store. A Kimi release that renames
    // its session directories must not make sessions disappear from `list`.
    let root = tempfile::tempdir().unwrap();
    write_session(
        &root.path().join("workspace-1").join("conversation-7"),
        &sample_body(),
    );
    let found = kimi::KimiStore::new(root.path()).discover().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].meta.id, "conversation-7");
}

/// Kimi resolves its data root first and `sessions/` beneath it, so a store
/// rooted at `<home>/sessions` puts the session index at `<home>`.
fn kimi_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("sessions")).unwrap();
    home
}

fn saveable(id: &str, cwd: &str) -> txcript::Transcript<kimi::Kimi> {
    let mut body = sample_body();
    // Shaped like a real state.json, which always carries a creation time.
    body.state = json!({
        "sessionId": id,
        "workDir": cwd,
        "title": "Parser work",
        "createdAt": 1_767_323_045_000_i64
    });
    txcript::Transcript::new(
        common::Meta {
            id: id.into(),
            timestamp: ts("2026-01-02T03:04:05.000Z"),
            cwd: Some(cwd.into()),
            git_branch: None,
            title: Some("Parser work".into()),
            cli_version: None,
            model: None,
        },
        body,
    )
}

#[test]
fn save_writes_the_layout_kimi_discovers() {
    // Kimi keys a workspace directory by `wd_<slug>_<sha256(workDir)[:12]>`
    // and finds sessions only through `session_index.jsonl` — there is no
    // directory-scan fallback, so a save that skips the index is invisible.
    let home = kimi_home();
    let store = kimi::KimiStore::new(home.path().join("sessions"));
    let saved = store.save(&saveable("session_abc", "/repo")).unwrap();

    assert_eq!(saved.id, "session_abc");
    let expected = home
        .path()
        .join("sessions")
        .join("wd_repo_816fc349d3fa")
        .join("session_abc");
    assert_eq!(saved.reference, expected);
    assert!(expected.join("state.json").is_file());
    assert!(expected.join("agents/main/wire.jsonl").is_file());

    let index = std::fs::read_to_string(home.path().join("session_index.jsonl")).unwrap();
    let entry: serde_json::Value = serde_json::from_str(index.trim()).unwrap();
    assert_eq!(entry["sessionId"], json!("session_abc"));
    assert_eq!(entry["workDir"], json!("/repo"));
    assert_eq!(entry["sessionDir"], json!(expected.to_str().unwrap()));
}

#[test]
fn saved_session_is_discovered_and_loads_back() {
    let home = kimi_home();
    let store = kimi::KimiStore::new(home.path().join("sessions"));
    let original = saveable("session_abc", "/repo");
    store.save(&original).unwrap();

    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].meta.id, "session_abc");

    let loaded = store.load(&found[0].reference).unwrap();
    // The wire log is carried through verbatim.
    assert_eq!(loaded.body.wire, original.body.wire);
    // `state` is normalized on the way out — `agents.main.homedir` names where
    // the session actually landed — so the round-trip contract is that the
    // normalization is a fixed point, not that it is a no-op.
    let again = store.save(&loaded).unwrap();
    assert_eq!(store.load(&again.reference).unwrap().body, loaded.body);
}

#[test]
fn save_points_the_agent_homedir_at_where_the_session_landed() {
    // Kimi resolves an agent's wire log through `agents.<name>.homedir`, and
    // txcript's directory-free readers recover the session id from it. Saving
    // into a different root has to rewrite it, or the copy points at the
    // original.
    let home = kimi_home();
    let store = kimi::KimiStore::new(home.path().join("sessions"));
    let saved = store.save(&saveable("session_abc", "/repo")).unwrap();

    let elsewhere = kimi_home();
    let other = kimi::KimiStore::new(elsewhere.path().join("sessions"));
    let moved = other.save(&store.load(&saved.reference).unwrap()).unwrap();

    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(moved.reference.join("state.json")).unwrap())
            .unwrap();
    let agent_dir = moved.reference.join("agents").join("main");
    assert_eq!(
        state["agents"]["main"]["homedir"],
        json!(agent_dir.to_str().unwrap())
    );
}

#[test]
fn delete_removes_the_session_and_tombstones_the_index() {
    let home = kimi_home();
    let store = kimi::KimiStore::new(home.path().join("sessions"));
    let saved = store.save(&saveable("session_abc", "/repo")).unwrap();

    store.delete(&saved.reference).unwrap();
    assert!(!saved.reference.exists());
    // Kimi's index is append-only; a removal is a `deleted` record, not an
    // edit, so a live Kimi reading the index still converges on "gone".
    let index = std::fs::read_to_string(home.path().join("session_index.jsonl")).unwrap();
    let last: serde_json::Value = serde_json::from_str(index.lines().last().unwrap()).unwrap();
    assert_eq!(last["sessionId"], json!("session_abc"));
    assert_eq!(last["deleted"], json!(true));
    assert_eq!(store.discover().unwrap().len(), 0);
}

#[test]
fn save_fills_identity_a_converted_session_lacks() {
    // A session converted from another harness has no Kimi state.json, so the
    // fields Kimi and `from_text` read the session back from must be supplied.
    let home = kimi_home();
    let store = kimi::KimiStore::new(home.path().join("sessions"));
    let mut transcript = saveable("session_abc", "/repo");
    transcript.body.state = json!({});

    let saved = store.save(&transcript).unwrap();
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(saved.reference.join("state.json")).unwrap())
            .unwrap();
    assert_eq!(state["sessionId"], json!("session_abc"));
    assert_eq!(state["workDir"], json!("/repo"));
    assert_eq!(state["createdAt"], json!(1_767_323_045_000_i64));
    // And the id survives without the directory, which is what the wasm
    // parser and `from_text` depend on.
    let text = kimi::Kimi::to_text(&store.load(&saved.reference).unwrap()).unwrap();
    assert_eq!(kimi::Kimi::from_text(&text).unwrap().meta.id, "session_abc");
}

/// Kimi accepts sessions written from outside — it reads whatever the index
/// points at — so `continue --with kimi` writes a real session rather than
/// refusing.
#[test]
fn continuing_into_kimi_writes_a_session() {
    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();

    let common = kimi::Kimi::to_common(&saveable("session_abc", "/repo")).unwrap();
    let written = txcript::local::write(txcript::HarnessId::Kimi, &common, Some(&sessions))
        .expect("kimi accepts written sessions");
    assert_eq!(written.id, "session_abc");

    // Reach the session through discovery rather than the printed location,
    // which is a Debug rendering shared by every harness.
    let store = kimi::KimiStore::new(&sessions);
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    let dir = &found[0].reference;
    assert!(dir.join("state.json").is_file());
    assert!(dir.join("agents/main/wire.jsonl").is_file());
    assert!(home.path().join("session_index.jsonl").is_file());
}

#[test]
fn saved_state_carries_what_kimi_reads_a_session_by() {
    // Kimi renders a session's time from `updatedAt` — without it the CLI
    // lists the session at the epoch — and locates each agent's log through
    // `agents.<name>.homedir`, which only the store knows the absolute path
    // for. Both must be written even when the source had neither.
    let home = kimi_home();
    let store = kimi::KimiStore::new(home.path().join("sessions"));
    let mut transcript = saveable("session_abc", "/repo");
    transcript.body.state = json!({});

    let saved = store.save(&transcript).unwrap();
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(saved.reference.join("state.json")).unwrap())
            .unwrap();

    assert_eq!(state["updatedAt"], json!(1_767_323_045_000_i64));
    let agent_dir = saved.reference.join("agents").join("main");
    assert_eq!(
        state["agents"]["main"]["homedir"],
        json!(agent_dir.to_str().unwrap())
    );
}

#[test]
fn save_preserves_state_a_native_session_already_had() {
    // A real Kimi state.json owns these fields; a save must not overwrite the
    // session's own bookkeeping with txcript's idea of it.
    let home = kimi_home();
    let store = kimi::KimiStore::new(home.path().join("sessions"));
    let mut transcript = saveable("session_abc", "/repo");
    transcript.body.state["updatedAt"] = json!(1_799_999_999_000_i64);
    transcript.body.state["isCustomTitle"] = json!(true);

    let saved = store.save(&transcript).unwrap();
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(saved.reference.join("state.json")).unwrap())
            .unwrap();
    assert_eq!(state["updatedAt"], json!(1_799_999_999_000_i64));
    assert_eq!(state["isCustomTitle"], json!(true));
}

#[test]
fn save_refuses_an_id_that_would_escape_the_store() {
    let home = kimi_home();
    let store = kimi::KimiStore::new(home.path().join("sessions"));
    let error = store
        .save(&saveable("../../escape", "/repo"))
        .expect_err("a traversing id must be rejected");
    assert!(
        error.to_string().contains("not usable as a file name"),
        "expected an id-shape rejection, got: {error}"
    );
    assert!(!home.path().parent().unwrap().join("escape").exists());
}

fn write_session(dir: &std::path::Path, body: &kimi::KimiSession) {
    std::fs::create_dir_all(dir.join("agents/main")).unwrap();
    std::fs::write(
        dir.join("state.json"),
        serde_json::to_string(&body.state).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("agents/main/wire.jsonl"),
        body.wire
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
}

#[test]
fn epoch_millisecond_created_at_is_supported() {
    let mut body = sample_body();
    body.state["createdAt"] = json!(1_767_323_045_000_i64);
    let native = txcript::Transcript::new(
        common::Meta {
            id: "abc".into(),
            timestamp: ts("2026-01-02T03:04:05.000Z"),
            cwd: Some("/repo".into()),
            git_branch: None,
            title: None,
            cli_version: None,
            model: None,
        },
        body,
    );
    let text = kimi::Kimi::to_text(&native).unwrap();
    let parsed = kimi::Kimi::from_text(&text).unwrap();
    assert_eq!(parsed.meta.timestamp, ts("2026-01-02T03:04:05.000Z"));
}

fn common_sample() -> txcript::Transcript<Common> {
    let meta = common::Meta {
        id: "abc".into(),
        timestamp: ts("2026-01-02T03:04:05.000Z"),
        cwd: Some("/repo".into()),
        git_branch: None,
        title: Some("Parser work".into()),
        cli_version: None,
        model: Some("kimi-k2".into()),
    };
    let body = vec![
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::Text {
                text: "edit the file".into(),
            }],
            timestamp: ts("2026-01-02T03:04:06.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::Text {
                text: "On it.".into(),
            }],
            timestamp: ts("2026-01-02T03:04:07.000Z"),
            model: Some("kimi-k2".into()),
            stop_reason: Some(common::StopReason::EndTurn),
            usage: None,
        },
    ];
    txcript::Transcript::new(meta, body)
}

#[test]
fn assistant_timestamps_survive_a_common_round_trip() {
    let native = kimi::Kimi::from_common(&common_sample()).unwrap();
    let back = kimi::Kimi::to_common(&native).unwrap();
    assert_eq!(back.body[0].timestamp, ts("2026-01-02T03:04:06.000Z"));
    // Assistant turns are rendered as loop events; they must carry their own
    // `time` or every assistant message collapses onto the session timestamp.
    assert_eq!(back.body[1].timestamp, ts("2026-01-02T03:04:07.000Z"));
}

#[test]
fn rendering_is_deterministic() {
    let common = common_sample();
    let first = kimi::Kimi::to_text(&kimi::Kimi::from_common(&common).unwrap()).unwrap();
    let second = kimi::Kimi::to_text(&kimi::Kimi::from_common(&common).unwrap()).unwrap();
    assert_eq!(first, second);
}

#[test]
fn context_undo_removes_the_retried_prompt() {
    let mut body = sample_body();
    body.wire
        .push(json!({"type": "turn.prompt", "time": 1_767_323_050_000_i64}));
    body.wire.push(
        json!({"type": "context.append_message", "time": 1_767_323_050_000_i64,
        "message": {"role": "user", "content": [{"type": "text", "text": "continue"}]}}),
    );
    body.wire
        .push(json!({"type": "turn.ended", "turnId": 1, "reason": "failed"}));
    body.wire
        .push(json!({"type": "context.undo", "count": 1, "time": 1_767_323_051_000_i64}));
    body.wire
        .push(json!({"type": "turn.prompt", "time": 1_767_323_052_000_i64}));
    body.wire.push(
        json!({"type": "context.append_message", "time": 1_767_323_052_000_i64,
        "message": {"role": "user", "content": [{"type": "text", "text": "continue"}]}}),
    );

    let native = txcript::Transcript::new(
        common::Meta {
            id: "abc".into(),
            timestamp: ts("2026-01-02T03:04:05.000Z"),
            cwd: None,
            git_branch: None,
            title: None,
            cli_version: None,
            model: None,
        },
        body,
    );
    let converted = kimi::Kimi::to_common(&native).unwrap();
    let prompts = converted
        .body
        .iter()
        .filter(|message| {
            matches!(
                message.content.first(),
                Some(common::Block::Text { text }) if text == "continue"
            )
        })
        .count();
    assert_eq!(prompts, 1, "the rolled-back prompt must not be replayed");
}
