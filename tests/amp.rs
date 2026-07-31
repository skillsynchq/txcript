#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Integration tests for the Amp codec and store — 1:1 message mapping
//! (Amp is already in the Anthropic convention), era-spanning tool
//! normalization, run-status handling, and the codec fixpoint through
//! Common.

use chrono::{DateTime, Utc};
use serde_json::json;
use txcript::common;
use txcript::harness::amp;
use txcript::{Codec, Common, Store, TextCodec, Transcript};

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// A native thread document shaped like a real legacy local-first file
/// (anonymized): env header, thinking + `tool_use` turns, dict and error
/// runs, an unmodeled bookkeeping record, and thread-level `~debug`.
fn native_fixture() -> serde_json::Value {
    json!({
        "v": 42,
        "id": "T-0199aaaa-bbbb-7ccc-8ddd-eeeeffff0001",
        "created": 1_768_178_184_664_i64,
        "title": "Fix the off-by-one",
        "agentMode": "smart",
        "nextMessageId": 6,
        "~debug": { "queue": [] },
        "env": {
            "initial": {
                "trees": [{
                    "uri": "file:///Users/dev/proj%20x",
                    "displayName": "proj x",
                    "repository": { "ref": "refs/heads/main", "sha": "abc123", "url": "https://example.com/r.git", "type": "git" },
                }],
                "platform": { "os": "darwin", "client": "CLI", "clientVersion": "0.0.1768178000-gaaaaaa" },
            },
        },
        "messages": [
            {
                "role": "user",
                "messageId": 0,
                "content": [{ "type": "text", "text": "fix the loop bound" }],
                "userState": { "currentlyVisibleFiles": [] },
                "agentMode": "smart",
                "meta": { "sentAt": 1_768_178_271_390_i64 },
            },
            {
                "role": "assistant",
                "messageId": 1,
                "content": [
                    { "type": "thinking", "thinking": "the bound is off", "signature": "sig-abc", "provider": "anthropic" },
                    { "type": "tool_use", "complete": true, "id": "toolu_01", "name": "Bash",
                      "input": { "cmd": "grep -n 'i <= n' src/a.rs", "cwd": "/Users/dev/proj x" } },
                ],
                "state": { "type": "complete", "stopReason": "tool_use" },
                "usage": {
                    "model": "claude-opus-4-5-20251101", "maxInputTokens": 168_000,
                    "inputTokens": 12, "outputTokens": 80,
                    "cacheCreationInputTokens": 900, "cacheReadInputTokens": 3400,
                    "totalInputTokens": 4312, "timestamp": "2026-01-12T00:37:55.000Z",
                },
            },
            {
                "role": "user",
                "messageId": 2,
                "content": [{ "type": "tool_result", "toolUseID": "toolu_01",
                    "run": { "status": "done", "result": { "output": "7:    i <= n\n", "exitCode": 0 },
                             "progress": [], "trackFiles": ["src/a.rs"] } }],
            },
            {
                "role": "assistant",
                "messageId": 3,
                "content": [
                    { "type": "tool_use", "complete": true, "id": "toolu_02", "name": "edit_file",
                      "input": { "path": "/Users/dev/proj x/src/a.rs", "old_str": "i <= n", "new_str": "i < n" } },
                    { "type": "tool_use", "complete": true, "id": "toolu_03", "name": "painter",
                      "input": { "prompt": "a celebratory sticker", "savePath": "/tmp/s.png" } },
                ],
                "state": { "type": "complete", "stopReason": "tool_use" },
                "usage": {
                    "model": "claude-opus-4-5-20251101", "maxInputTokens": 168_000,
                    "inputTokens": 4, "outputTokens": 60, "cacheCreationInputTokens": 10,
                    "cacheReadInputTokens": 4300, "totalInputTokens": 4314,
                    "timestamp": "2026-01-12T00:38:05.000Z",
                },
            },
            {
                "role": "user",
                "messageId": 4,
                "content": [
                    { "type": "tool_result", "toolUseID": "toolu_02",
                      "run": { "status": "done", "result": { "diff": "-i <= n\n+i < n", "lineRange": [7, 7] } } },
                    { "type": "tool_result", "toolUseID": "toolu_03",
                      "run": { "status": "error", "error": { "message": "Save path already exists" } } },
                ],
            },
            {
                "role": "assistant",
                "messageId": 5,
                "content": [{ "type": "text", "text": "Fixed the bound." }],
                "state": { "type": "complete", "stopReason": "end_turn" },
                "usage": {
                    "model": "claude-opus-4-5-20251101", "maxInputTokens": 168_000,
                    "inputTokens": 6, "outputTokens": 20, "cacheCreationInputTokens": 0,
                    "cacheReadInputTokens": 4310, "totalInputTokens": 4316,
                    "timestamp": "2026-01-12T00:38:12.000Z",
                },
                "turnElapsedMs": 7000,
            },
            // An unmodeled record kind: it must survive the disk round trip
            // even though the codec carries nothing of it into Common.
            { "role": "supervisor", "note": "bookkeeping the codec does not model" },
        ],
    })
}

// ── store ──────────────────────────────────────────────────────────────

#[test]
fn store_round_trip_is_lossless_on_disk() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let id = "T-0199aaaa-bbbb-7ccc-8ddd-eeeeffff0001";
    std::fs::write(
        src.path().join(format!("{id}.json")),
        serde_json::to_string_pretty(&native_fixture()).unwrap(),
    )
    .unwrap();

    let loaded = amp::AmpStore::new(src.path())
        .load(&src.path().join(format!("{id}.json")))
        .unwrap();
    let saved = amp::AmpStore::new(dst.path()).save(&loaded).unwrap();
    // The save path is `<root>/<thread-id>.json`, exactly the native scheme.
    assert_eq!(saved.reference, dst.path().join(format!("{id}.json")));
    assert_eq!(saved.id, id);

    let reloaded = amp::AmpStore::new(dst.path())
        .load(&saved.reference)
        .unwrap();
    assert_eq!(loaded, reloaded);
    // The unmodeled bookkeeping record survived both hops verbatim.
    assert_eq!(
        reloaded.body.messages.last().unwrap().get("role").unwrap(),
        "supervisor"
    );
}

#[test]
fn discover_extracts_metadata() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path()
            .join("T-0199aaaa-bbbb-7ccc-8ddd-eeeeffff0001.json"),
        serde_json::to_string(&native_fixture()).unwrap(),
    )
    .unwrap();
    // A stray JSON file that is not a thread document must be skipped.
    std::fs::write(dir.path().join("settings.json"), r#"{"theme":"dark"}"#).unwrap();

    let found = amp::AmpStore::new(dir.path()).discover().unwrap();
    assert_eq!(found.len(), 1);
    let meta = &found[0].meta;
    assert_eq!(meta.id, "T-0199aaaa-bbbb-7ccc-8ddd-eeeeffff0001");
    assert_eq!(meta.timestamp, ts("2026-01-12T00:36:24.664Z"));
    // The file:// URI is percent-decoded back into a plain path.
    assert_eq!(meta.cwd.as_deref(), Some("/Users/dev/proj x"));
    assert_eq!(meta.git_branch.as_deref(), Some("main"));
    assert_eq!(meta.title.as_deref(), Some("Fix the off-by-one"));
    assert_eq!(meta.cli_version.as_deref(), Some("0.0.1768178000-gaaaaaa"));
    assert_eq!(meta.model.as_deref(), Some("claude-opus-4-5-20251101"));
}

// ── to_common ──────────────────────────────────────────────────────────

fn load_fixture() -> Transcript<Common> {
    let text = serde_json::to_string(&native_fixture()).unwrap();
    amp::Amp::to_common(&amp::Amp::from_text(&text).unwrap()).unwrap()
}

#[test]
fn to_common_maps_messages_one_to_one_with_typed_tools() {
    let msgs = load_fixture().body;
    // Six conversational messages; the supervisor record carries no turn.
    assert_eq!(msgs.len(), 6);

    assert_eq!(msgs[0].role, common::Role::User);
    assert_eq!(msgs[0].timestamp, ts("2026-01-12T00:37:51.390Z"));
    assert!(
        matches!(&msgs[0].content[0], common::Block::Text { text } if text == "fix the loop bound")
    );

    // Thinking keeps its signature; the legacy Bash spelling normalizes to
    // the canonical typed tool with renamed keys.
    assert!(matches!(
        &msgs[1].content[0],
        common::Block::Thinking { text, signature, .. }
            if text == "the bound is off" && signature.as_deref() == Some("sig-abc")
    ));
    assert!(matches!(
        &msgs[1].content[1],
        common::Block::ToolUse { id, tool: common::Tool::Bash { command, workdir, .. } }
            if id == "toolu_01"
                && command == "grep -n 'i <= n' src/a.rs"
                && workdir.as_deref() == Some("/Users/dev/proj x")
    ));
    assert_eq!(msgs[1].model.as_deref(), Some("claude-opus-4-5-20251101"));
    assert_eq!(msgs[1].stop_reason, Some(common::StopReason::ToolUse));
    assert_eq!(msgs[1].timestamp, ts("2026-01-12T00:37:55.000Z"));
    let usage = msgs[1].usage.unwrap();
    assert_eq!(usage.input_tokens, 12);
    assert_eq!(usage.output_tokens, 80);
    assert_eq!(usage.cache_read_input_tokens, Some(3400));
    assert_eq!(usage.cache_creation_input_tokens, Some(900));

    // A structured run result stays JSON; the result carrier takes the
    // preceding assistant's timestamp (Amp stamps none of its own).
    assert!(matches!(
        &msgs[2].content[0],
        common::Block::ToolResult { tool_use_id, content: common::ToolOutput::Json(v), is_error: false }
            if tool_use_id == "toolu_01" && v.get("exitCode").is_some()
    ));
    assert_eq!(msgs[2].timestamp, ts("2026-01-12T00:37:55.000Z"));

    // edit_file becomes a typed Edit; painter has no canonical counterpart
    // and lands in Raw with its input untouched.
    assert!(matches!(
        &msgs[3].content[0],
        common::Block::ToolUse { tool: common::Tool::Edit { file_path, old_string, new_string, .. }, .. }
            if file_path == "/Users/dev/proj x/src/a.rs" && old_string == "i <= n" && new_string == "i < n"
    ));
    assert!(matches!(
        &msgs[3].content[1],
        common::Block::ToolUse { tool: common::Tool::Raw { tool_name, input }, .. }
            if tool_name == "painter" && input.get("savePath").is_some()
    ));

    // The errored run becomes an error-flagged text result.
    assert!(matches!(
        &msgs[4].content[1],
        common::Block::ToolResult { content: common::ToolOutput::Text(t), is_error: true, .. }
            if t == "Save path already exists"
    ));

    assert_eq!(msgs[5].stop_reason, Some(common::StopReason::EndTurn));
}

#[test]
fn modern_shell_command_normalizes_to_bash() {
    let text = serde_json::to_string(&json!({
        "v": 13,
        "id": "T-modern",
        "created": 1_783_466_656_505_i64,
        "messages": [{
            "role": "assistant",
            "messageId": 1,
            "content": [{ "type": "tool_use", "complete": true, "id": "toolu_09",
                "name": "shell_command",
                "input": { "command": "echo hi", "workdir": "/w" },
                "blockState": {}, "providerToolUseId": "x" }],
            "state": { "type": "complete", "stopReason": "tool_use" },
            "usage": { "model": "claude-opus-4-8", "inputTokens": 2, "outputTokens": 9,
                       "timestamp": "2026-07-07T23:36:15.576Z" },
        }],
    }))
    .unwrap();
    let msgs = amp::Amp::to_common(&amp::Amp::from_text(&text).unwrap())
        .unwrap()
        .body;
    assert!(matches!(
        &msgs[0].content[0],
        common::Block::ToolUse { tool: common::Tool::Bash { command, workdir, .. }, .. }
            if command == "echo hi" && workdir.as_deref() == Some("/w")
    ));
}

#[test]
fn read_range_becomes_offset_and_limit() {
    let text = serde_json::to_string(&json!({
        "v": 1, "id": "T-r", "created": 1i64,
        "messages": [{
            "role": "assistant", "messageId": 0,
            "content": [
                { "type": "tool_use", "complete": true, "id": "t1", "name": "Read",
                  "input": { "path": "/a.rs", "read_range": [280, 300] } },
                { "type": "tool_use", "complete": true, "id": "t2", "name": "Read",
                  "input": { "path": "/b.rs", "read_range": "whole" } },
            ],
            "state": { "type": "complete", "stopReason": "tool_use" },
            "usage": { "inputTokens": 1, "outputTokens": 1 },
        }],
    }))
    .unwrap();
    let msgs = amp::Amp::to_common(&amp::Amp::from_text(&text).unwrap())
        .unwrap()
        .body;
    assert!(matches!(
        &msgs[0].content[0],
        common::Block::ToolUse { tool: common::Tool::Read { file_path, offset, limit }, .. }
            if file_path == "/a.rs" && *offset == Some(280) && *limit == Some(21)
    ));
    // A malformed range cannot be converted; the whole input survives in Raw.
    assert!(matches!(
        &msgs[0].content[1],
        common::Block::ToolUse { tool: common::Tool::Raw { tool_name, input }, .. }
            if tool_name == "Read" && input.get("read_range").is_some()
    ));
}

#[test]
fn cancelled_turns_and_runs_map_to_aborted_and_error() {
    let text = serde_json::to_string(&json!({
        "v": 1, "id": "T-c", "created": 1i64,
        "messages": [
            {
                "role": "assistant", "messageId": 0,
                "content": [{ "type": "text", "text": "working on it" }],
                "state": { "type": "cancelled" },
                "usage": { "inputTokens": 3, "outputTokens": 1 },
            },
            {
                "role": "user", "messageId": 1,
                "content": [{ "type": "tool_result", "toolUseID": "t9",
                    "run": { "status": "cancelled", "reason": "user pressed esc" } }],
            },
            {
                "role": "user", "messageId": 2,
                "content": [{ "type": "tool_result", "toolUseID": "t10",
                    "run": { "status": "rejected-by-user", "reason": "not allowed", "toAllow": [] } }],
            },
        ],
    }))
    .unwrap();
    let msgs = amp::Amp::to_common(&amp::Amp::from_text(&text).unwrap())
        .unwrap()
        .body;
    assert_eq!(msgs[0].stop_reason, Some(common::StopReason::Aborted));
    assert!(matches!(
        &msgs[1].content[0],
        common::Block::ToolResult { content: common::ToolOutput::Text(t), is_error: true, .. }
            if t == "user pressed esc"
    ));
    assert!(matches!(
        &msgs[2].content[0],
        common::Block::ToolResult { content: common::ToolOutput::Text(t), is_error: true, .. }
            if t == "not allowed"
    ));
}

#[test]
fn streaming_turn_has_no_stop_reason() {
    let text = serde_json::to_string(&json!({
        "v": 1, "id": "T-s", "created": 1i64,
        "messages": [{
            "role": "assistant", "messageId": 0,
            "content": [{ "type": "text", "text": "partial" }],
            "state": { "type": "streaming" },
            "usage": { "inputTokens": 1, "outputTokens": 1 },
        }],
    }))
    .unwrap();
    let msgs = amp::Amp::to_common(&amp::Amp::from_text(&text).unwrap())
        .unwrap()
        .body;
    assert_eq!(msgs[0].stop_reason, None);
}

// ── fixpoint ───────────────────────────────────────────────────────────

fn meta() -> common::Meta {
    common::Meta {
        id: "T-0199aaaa-bbbb-7ccc-8ddd-eeeeffff0002".into(),
        timestamp: ts("2026-01-02T03:04:05.000Z"),
        cwd: Some("/Users/dev/proj x".into()),
        git_branch: Some("main".into()),
        title: Some("Round trip".into()),
        cli_version: Some("0.0.1768178000-gaaaaaa".into()),
        model: Some("claude-opus-4-5-20251101".into()),
    }
}

/// Shaped at Amp's native granularity: assistants carry a model and a stop
/// reason (Amp requires a `state`), result carriers share the preceding
/// assistant's timestamp (Amp stamps no time on them), and error results are
/// text (Amp stores an error message string).
#[allow(clippy::too_many_lines)]
fn sample_common() -> Transcript<Common> {
    let model = || Some("claude-opus-4-5-20251101".to_string());
    let body = vec![
        common::Message {
            role: common::Role::User,
            content: vec![
                common::Block::Text {
                    text: "add a sticker to the readme".into(),
                },
                common::Block::Image {
                    source: common::ImageSource {
                        source_type: "base64".into(),
                        media_type: "image/png".into(),
                        data: "aGVsbG8=".into(),
                    },
                },
            ],
            timestamp: ts("2026-01-02T03:04:06.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![
                common::Block::Thinking {
                    text: "plan the edit".into(),
                    signature: Some("sig-1".into()),
                    encrypted: None,
                },
                common::Block::Text {
                    text: "On it.".into(),
                },
                common::Block::ToolUse {
                    id: "toolu_a".into(),
                    tool: common::Tool::Bash {
                        command: "ls -la".into(),
                        workdir: Some("/Users/dev/proj x".into()),
                        timeout_ms: None,
                        description: None,
                        run_in_background: false,
                    },
                },
                common::Block::ToolUse {
                    id: "toolu_b".into(),
                    tool: common::Tool::Read {
                        file_path: "/Users/dev/proj x/README.md".into(),
                        offset: Some(1),
                        limit: Some(50),
                    },
                },
            ],
            timestamp: ts("2026-01-02T03:04:07.000Z"),
            model: model(),
            stop_reason: Some(common::StopReason::ToolUse),
            usage: Some(common::Usage {
                input_tokens: 10,
                output_tokens: 40,
                cache_read_input_tokens: Some(0),
                cache_creation_input_tokens: Some(1200),
            }),
        },
        common::Message {
            role: common::Role::User,
            content: vec![
                common::Block::ToolResult {
                    tool_use_id: "toolu_a".into(),
                    content: common::ToolOutput::Json(
                        json!({ "output": "README.md\n", "exitCode": 0 }),
                    ),
                    is_error: false,
                },
                common::Block::ToolResult {
                    tool_use_id: "toolu_b".into(),
                    content: common::ToolOutput::Text("# readme".into()),
                    is_error: false,
                },
            ],
            // Result carriers share the tool turn's timestamp (see above).
            timestamp: ts("2026-01-02T03:04:07.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::ToolUse {
                id: "toolu_c".into(),
                tool: common::Tool::Raw {
                    tool_name: "mcp__design__render".into(),
                    input: json!({ "spec": { "kind": "sticker" } }),
                },
            }],
            timestamp: ts("2026-01-02T03:04:09.000Z"),
            model: model(),
            stop_reason: Some(common::StopReason::ToolUse),
            usage: None,
        },
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::ToolResult {
                tool_use_id: "toolu_c".into(),
                content: common::ToolOutput::Text("render failed: no canvas".into()),
                is_error: true,
            }],
            timestamp: ts("2026-01-02T03:04:09.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::Text {
                text: "Stopping here.".into(),
            }],
            timestamp: ts("2026-01-02T03:04:11.000Z"),
            model: model(),
            stop_reason: Some(common::StopReason::Aborted),
            usage: Some(common::Usage {
                input_tokens: 5,
                output_tokens: 2,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }),
        },
    ];
    Transcript::new(meta(), body)
}

#[test]
fn codec_fixpoint_through_common_loses_nothing() {
    let common = sample_common();
    let native = amp::Amp::from_common(&common).unwrap();
    let back = amp::Amp::to_common(&native).unwrap();
    assert_eq!(common, back);
}

/// A Windows cwd survives the trip through Amp's reconstructed `file://`
/// env URI: `C:\…` normalizes to `file:///C%3A/…` and decodes back.
#[test]
fn windows_cwd_round_trips_through_the_env_uri() {
    let mut common = sample_common();
    common.meta.cwd = Some(r"C:\Users\dev\proj x".into());

    let native = amp::Amp::from_common(&common).unwrap();
    let back = amp::Amp::to_common(&native).unwrap();
    assert_eq!(back.meta.cwd.as_deref(), Some(r"C:\Users\dev\proj x"));
}

/// A foreign session id (Claude uuid) becomes a deterministic, valid
/// `T-…` thread id, and the store names the file by it — `amp threads
/// continue` rejects anything else before even looking it up.
#[test]
fn foreign_ids_become_valid_amp_thread_ids() {
    let mut common = sample_common();
    common.meta.id = "f06fec53-2d8c-49e1-8594-2639ec1177d0".into();

    let a = amp::Amp::from_common(&common).unwrap();
    let b = amp::Amp::from_common(&common).unwrap();
    let id = a.body.id().unwrap().to_string();
    assert_eq!(Some(id.as_str()), b.body.id(), "id must be deterministic");
    let rest = id.strip_prefix("T-").expect("amp ids start with T-");
    assert!(rest.len() >= 8);
    assert!(rest.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'));

    let dir = tempfile::tempdir().unwrap();
    let saved = amp::AmpStore::new(dir.path()).save(&a).unwrap();
    assert_eq!(saved.id, id, "save must hand back the resumable id");
    assert_eq!(saved.reference, dir.path().join(format!("{id}.json")));

    // A native amp id passes through untouched.
    let native = amp::Amp::from_common(&sample_common()).unwrap();
    assert_eq!(
        native.body.id(),
        Some("T-0199aaaa-bbbb-7ccc-8ddd-eeeeffff0002")
    );
}

#[test]
fn from_common_is_deterministic() {
    let common = sample_common();
    let a = amp::Amp::to_text(&amp::Amp::from_common(&common).unwrap()).unwrap();
    let b = amp::Amp::to_text(&amp::Amp::from_common(&common).unwrap()).unwrap();
    assert_eq!(a, b);
}

/// The rendered thread text parses back to identical native records — the
/// text form is the disk form.
#[test]
fn text_codec_round_trips_the_thread_document() {
    let native = amp::Amp::from_common(&sample_common()).unwrap();
    let text = amp::Amp::to_text(&native).unwrap();
    let reparsed = amp::Amp::from_text(&text).unwrap();
    assert_eq!(native.body, reparsed.body);
    // Meta re-derived from the bytes matches what from_common embedded.
    assert_eq!(reparsed.meta, native.meta);
}

/// Amp is server-authoritative with no thread import: continuing a session
/// into amp is refused unconditionally, output-directory override included.
#[test]
fn continuing_into_amp_is_refused() {
    let common = sample_common();
    let dir = tempfile::tempdir().unwrap();
    for root in [None, Some(dir.path())] {
        let err = match txcript::local::write(txcript::HarnessId::Amp, &common, root) {
            Err(e) => e.to_string(),
            Ok(written) => panic!("expected refusal, wrote {}", written.location),
        };
        assert!(
            err.contains("cannot be continued into amp"),
            "unexpected error: {err}"
        );
    }
}
