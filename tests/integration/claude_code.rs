#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Covers Store round-trip fidelity, Common codec fixpoints, and conversation
//! extraction.

use chrono::{DateTime, Utc};
use serde_json::json;
use txcript::common;
use txcript::harness::claude_code;
use txcript::{Codec, Common, Store, TextCodec, Transcript};

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// A realistic single session: a user ask, an assistant turn that thinks and
/// calls Edit, the tool result, and a final assistant turn with usage. Plus
/// non-message lines (summary, custom-title, a snapshot) that must survive on
/// disk but never become messages.
fn sample_jsonl() -> String {
    let lines = [
        json!({"type": "summary", "summary": "Fix the parser", "leafUuid": "abc"}),
        json!({"type": "custom-title", "customTitle": "Parser work"}),
        json!({
            "type": "user", "uuid": "u1", "parentUuid": null,
            "sessionId": "sess-1", "cwd": "/work/repo", "gitBranch": "main",
            "version": "1.2.3", "timestamp": "2026-01-02T03:04:05.000Z",
            "message": {"role": "user", "content": "fix the off-by-one"},
        }),
        json!({
            "type": "assistant", "uuid": "a1", "parentUuid": "u1",
            "sessionId": "sess-1", "timestamp": "2026-01-02T03:04:06.000Z",
            "message": {
                "role": "assistant",
                "model": "claude-opus-4-8",
                "content": [
                    {"type": "thinking", "thinking": "off-by-one in the loop", "signature": "sig-xyz"},
                    {"type": "text", "text": "Patching the bound."},
                    {"type": "tool_use", "id": "t1", "name": "Edit", "input": {
                        "file_path": "/work/repo/src/p.rs",
                        "old_string": "i <= n", "new_string": "i < n"
                    }},
                ],
            },
        }),
        json!({
            "type": "user", "uuid": "u2", "parentUuid": "a1",
            "sessionId": "sess-1", "timestamp": "2026-01-02T03:04:07.000Z",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "applied"},
            ]},
        }),
        json!({
            "type": "assistant", "uuid": "a2", "parentUuid": "u2",
            "sessionId": "sess-1", "timestamp": "2026-01-02T03:04:08.000Z",
            "message": {
                "role": "assistant",
                "model": "claude-opus-4-8",
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 100, "output_tokens": 20, "cache_read_input_tokens": 50},
                "content": [{"type": "text", "text": "Done."}],
            },
        }),
        // A line type the codec doesn't model — must round-trip untouched.
        json!({"type": "file-history-snapshot", "snapshot": {"files": ["a", "b"]}}),
    ];
    lines
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[test]
fn store_round_trip_is_lossless_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let store = claude_code::ClaudeStore::new(dir.path());

    let src = dir.path().join("orig.jsonl");
    std::fs::write(&src, sample_jsonl()).unwrap();

    let loaded = store.load(&src).unwrap();
    let saved = store.save(&loaded).unwrap();
    let reloaded = store.load(&saved.reference).unwrap();

    // Every native record — including the summary, title, and snapshot lines
    // the codec ignores — survives a load→save→load cycle unchanged.
    assert_eq!(loaded.body, reloaded.body);
    // And the on-disk landing spot is derived from the session metadata.
    assert!(saved.reference.ends_with("sess-1.jsonl"));
}

#[test]
fn windows_cwd_encodes_the_project_dir() {
    let dir = tempfile::tempdir().unwrap();
    let store = claude_code::ClaudeStore::new(dir.path());

    let src = dir.path().join("orig.jsonl");
    let jsonl = sample_jsonl().replace("/work/repo", r"C:\\Users\\dev\\repo");
    std::fs::write(&src, jsonl).unwrap();

    // `C:\Users\dev\repo` lands in `C--Users-dev-repo`, Claude's own
    // Windows encoding (`\` and `:` map to `-` like `/` and `.`).
    let saved = store.save(&store.load(&src).unwrap()).unwrap();
    let project = saved.reference.parent().unwrap().file_name().unwrap();
    assert_eq!(project.to_str(), Some("C--Users-dev-repo"));
}

#[test]
fn discover_extracts_session_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("-work-repo");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("sess-1.jsonl"), sample_jsonl()).unwrap();

    let store = claude_code::ClaudeStore::new(dir.path());
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    let meta = &found[0].meta;
    assert_eq!(meta.id, "sess-1");
    assert_eq!(meta.cwd.as_deref(), Some("/work/repo"));
    assert_eq!(meta.git_branch.as_deref(), Some("main"));
    assert_eq!(meta.model.as_deref(), Some("claude-opus-4-8"));
    assert_eq!(meta.cli_version.as_deref(), Some("1.2.3"));
    // custom-title wins over the summary line.
    assert_eq!(meta.title.as_deref(), Some("Parser work"));
    assert_eq!(meta.timestamp, ts("2026-01-02T03:04:05.000Z"));
}

#[test]
fn to_common_extracts_the_conversation_faithfully() {
    let dir = tempfile::tempdir().unwrap();
    let store = claude_code::ClaudeStore::new(dir.path());
    let src = dir.path().join("s.jsonl");
    std::fs::write(&src, sample_jsonl()).unwrap();

    let common = claude_code::ClaudeCode::to_common(&store.load(&src).unwrap()).unwrap();
    let msgs = &common.body;

    // Four conversational turns; the summary/title/snapshot lines are dropped.
    assert_eq!(msgs.len(), 4);

    assert_eq!(msgs[0].role, common::Role::User);
    assert!(
        matches!(&msgs[0].content[0], common::Block::Text { text } if text == "fix the off-by-one")
    );

    // Assistant turn: thinking (with signature), text, and a typed Edit.
    assert_eq!(msgs[1].role, common::Role::Assistant);
    assert_eq!(msgs[1].model.as_deref(), Some("claude-opus-4-8"));
    assert!(matches!(
        &msgs[1].content[0],
        common::Block::Thinking { text, signature: Some(s), .. } if text == "off-by-one in the loop" && s == "sig-xyz"
    ));
    match &msgs[1].content[2] {
        common::Block::ToolUse {
            id,
            tool:
                common::Tool::Edit {
                    file_path,
                    old_string,
                    new_string,
                    ..
                },
        } => {
            assert_eq!(id, "t1");
            assert_eq!(file_path, "/work/repo/src/p.rs");
            assert_eq!(old_string, "i <= n");
            assert_eq!(new_string, "i < n");
        }
        other => panic!("expected Edit tool_use, got {other:?}"),
    }

    // Tool result rides on a User message (Anthropic convention).
    assert_eq!(msgs[2].role, common::Role::User);
    assert!(matches!(
        &msgs[2].content[0],
        common::Block::ToolResult { tool_use_id, content: common::ToolOutput::Text(t), is_error: false }
            if tool_use_id == "t1" && t == "applied"
    ));

    // Final turn carries usage and stop reason.
    assert_eq!(msgs[3].stop_reason, Some(common::StopReason::EndTurn));
    let usage = msgs[3].usage.unwrap();
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.cache_read_input_tokens, Some(50));
}

/// A Common transcript covering every block kind, used to prove the codec
/// fixpoint `to_common(from_common(c)) == c`.
fn sample_common() -> Transcript<Common> {
    let meta = common::Meta {
        id: "sess-1".into(),
        timestamp: ts("2026-01-02T03:04:05.000Z"),
        cwd: Some("/work/repo".into()),
        git_branch: Some("main".into()),
        title: Some("Parser work".into()),
        cli_version: Some("1.2.3".into()),
        model: Some("claude-opus-4-8".into()),
    };
    let body = vec![
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::Text {
                text: "fix it".into(),
            }],
            timestamp: ts("2026-01-02T03:04:05.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![
                common::Block::Thinking {
                    text: "thinking".into(),
                    signature: Some("sig".into()),
                    encrypted: None,
                },
                common::Block::Text {
                    text: "patching".into(),
                },
                common::Block::ToolUse {
                    id: "t1".into(),
                    tool: common::Tool::Edit {
                        file_path: "/a.rs".into(),
                        old_string: "x".into(),
                        new_string: "y".into(),
                        replace_all: false,
                    },
                },
            ],
            timestamp: ts("2026-01-02T03:04:06.000Z"),
            model: Some("claude-opus-4-8".into()),
            stop_reason: Some(common::StopReason::ToolUse),
            usage: None,
        },
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::ToolResult {
                tool_use_id: "t1".into(),
                content: common::ToolOutput::Text("ok".into()),
                is_error: false,
            }],
            timestamp: ts("2026-01-02T03:04:07.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::Text {
                text: "done".into(),
            }],
            timestamp: ts("2026-01-02T03:04:08.000Z"),
            model: Some("claude-opus-4-8".into()),
            stop_reason: Some(common::StopReason::EndTurn),
            usage: Some(common::Usage {
                input_tokens: 100,
                output_tokens: 20,
                cache_read_input_tokens: Some(50),
                cache_creation_input_tokens: None,
            }),
        },
    ];
    Transcript::new(meta, body)
}

#[test]
fn codec_fixpoint_through_common_loses_nothing() {
    let common = sample_common();
    let native = claude_code::ClaudeCode::from_common(&common).unwrap();
    let back = claude_code::ClaudeCode::to_common(&native).unwrap();
    assert_eq!(common, back);
}

/// Foreign harnesses put block tags in `tool_result.content` that the
/// Anthropic wire rejects (e.g. `knowledge`); passing one through verbatim
/// makes the converted session fail to load with a 400 on resume. Only the
/// wire's own tags may survive as an array — anything else flattens to its
/// compact JSON text. (Regression: simple → `claude_code` with a `knowledge`
/// block.)
#[test]
fn foreign_tool_result_blocks_flatten_to_text() {
    let mut common = sample_common();
    common.body[1].content.push(common::Block::ToolUse {
        id: "t2".into(),
        tool: common::Tool::Raw {
            tool_name: "Recall".into(),
            input: json!({ "q": "fact" }),
        },
    });
    common.body[2].content = vec![
        common::Block::ToolResult {
            tool_use_id: "t1".into(),
            content: common::ToolOutput::Json(json!([
                { "type": "knowledge", "id": "k1", "content": "remembered fact" },
                { "type": "text", "text": "ok" },
            ])),
            is_error: false,
        },
        common::Block::ToolResult {
            tool_use_id: "t2".into(),
            content: common::ToolOutput::Json(json!([
                { "type": "text", "text": "native shape" },
            ])),
            is_error: false,
        },
    ];

    let native = claude_code::ClaudeCode::from_common(&common).unwrap();
    let text = claude_code::ClaudeCode::to_text(&native).unwrap();
    let contents: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|entry| entry.get("message")?.get("content").cloned())
        .flat_map(|content| content.as_array().cloned().unwrap_or_default())
        .filter_map(|block| {
            (block.get("type")? == "tool_result").then(|| block.get("content").cloned())?
        })
        .collect();
    assert_eq!(contents.len(), 2, "both tool results should be emitted");
    // The foreign-tagged array flattens to a string…
    assert!(contents[0].is_string(), "got {:?}", contents[0]);
    // …while the wire's native block-array shape passes through untouched.
    assert_eq!(
        contents[1],
        json!([{ "type": "text", "text": "native shape" }])
    );
}

/// `from_common` is a pure function: same input, identical output (deterministic
/// uuids), so conversions are reproducible.
#[test]
fn from_common_is_deterministic() {
    let common = sample_common();
    let a =
        serde_json::to_value(claude_code::ClaudeCode::from_common(&common).unwrap().body).unwrap();
    let b =
        serde_json::to_value(claude_code::ClaudeCode::from_common(&common).unwrap().body).unwrap();
    assert_eq!(a, b);
}

// ── local command envelopes ────────────────────────────────────────────

/// Every shape Claude Code has written its local-command markup in, drawn from
/// real sessions: the command on a `user` line with the tags in one order, on
/// a `system` line in the other, inside a lone text block, and with the
/// `<command-args>` tag absent. Plus its output, the caveat boilerplate, and
/// a genuine message that merely *quotes* the markup.
fn envelope_jsonl() -> String {
    let lines = [
        json!({
            "type": "user", "uuid": "u1", "parentUuid": null, "isMeta": true,
            "sessionId": "sess-1", "cwd": "/work/repo", "timestamp": "2026-01-02T03:04:05.000Z",
            "message": {"role": "user", "content":
                "<local-command-caveat>Caveat: The messages below were generated by the user while running local commands. DO NOT respond to these messages or otherwise consider them in your response unless the user explicitly asks you to.</local-command-caveat>"},
        }),
        // Older shape: envelope as the whole string body, name tag first.
        json!({
            "type": "user", "uuid": "u2", "parentUuid": "u1",
            "sessionId": "sess-1", "timestamp": "2026-01-02T03:04:06.000Z",
            "message": {"role": "user", "content":
                "<command-name>/release</command-name>\n            <command-message>release</command-message>\n            <command-args>patch</command-args>"},
        }),
        // Its output, on a `system` line linked back by parentUuid — and
        // drawn in colour, as `/context` and friends are.
        json!({
            "type": "system", "subtype": "local_command", "uuid": "s1", "parentUuid": "u2",
            "sessionId": "sess-1", "timestamp": "2026-01-02T03:04:07.000Z",
            "content": "<local-command-stdout>\u{1b}[1mcut \u{1b}[38;2;136;136;136mv0.4.3\u{1b}[39m</local-command-stdout>",
        }),
        // Newer shape: envelope on a `system` line, message tag first, no args.
        json!({
            "type": "system", "subtype": "local_command", "uuid": "s2", "parentUuid": "s1",
            "sessionId": "sess-1", "timestamp": "2026-01-02T03:04:08.000Z",
            "content": "<command-message>context</command-message>\n<command-name>/context</command-name>",
        }),
        // Envelope wrapped in a lone text block rather than a bare string.
        json!({
            "type": "user", "uuid": "u3", "parentUuid": "s2",
            "sessionId": "sess-1", "timestamp": "2026-01-02T03:04:09.000Z",
            "message": {"role": "user", "content": [
                {"type": "text", "text": "<command-name>/clear</command-name>\n            <command-message>clear</command-message>\n            <command-args></command-args>"},
            ]},
        }),
        // A real message that quotes the markup: it must stay plain text.
        json!({
            "type": "user", "uuid": "u4", "parentUuid": "u3",
            "sessionId": "sess-1", "timestamp": "2026-01-02T03:04:10.000Z",
            "message": {"role": "user", "content":
                "why does claude add <command-name>/clear</command-name> to my messages?"},
        }),
        // Output with no command of its own to attach to.
        json!({
            "type": "system", "subtype": "local_command", "uuid": "s3", "parentUuid": "u4",
            "sessionId": "sess-1", "timestamp": "2026-01-02T03:04:11.000Z",
            "content": "<local-command-stdout>Export cancelled</local-command-stdout>",
        }),
        // A `system` line that is not a local command carries nothing.
        json!({
            "type": "system", "subtype": "turn_duration", "uuid": "s4", "parentUuid": "s3",
            "sessionId": "sess-1", "timestamp": "2026-01-02T03:04:12.000Z",
            "content": "turn took 4.2s",
        }),
    ];
    lines
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn envelope_common() -> Transcript<Common> {
    claude_code::ClaudeCode::to_common(
        &claude_code::ClaudeCode::from_text(&envelope_jsonl()).unwrap(),
    )
    .unwrap()
}

#[test]
fn slash_commands_become_command_calls_whatever_shape_they_arrive_in() {
    let common = envelope_common();
    let msgs = &common.body;

    // The caveat and the non-command `system` line carry nothing; everything
    // else is a turn. Three commands, two outputs, one quoting message.
    assert_eq!(msgs.len(), 6);

    let command = |m: &common::Message| match &m.content[..] {
        [
            common::Block::ToolUse {
                id,
                tool: common::Tool::Command { command, args },
            },
        ] => (id.clone(), command.clone(), args.clone()),
        other => panic!("expected a command call, got {other:?}"),
    };

    // Both tag orders parse, and `<command-args>` is optional. A command is
    // the *user* driving the harness, so it rides a User turn.
    assert_eq!(msgs[0].role, common::Role::User);
    assert_eq!(
        command(&msgs[0]),
        ("u2".into(), "/release".into(), Some("patch".into()))
    );
    assert_eq!(command(&msgs[2]), ("s2".into(), "/context".into(), None));
    // An empty `<command-args>` is no args at all, not an empty string.
    assert_eq!(command(&msgs[3]), ("u3".into(), "/clear".into(), None));

    // Output pairs to the command above it by parentUuid, with the colour
    // stripped back out.
    assert!(matches!(
        &msgs[1].content[0],
        common::Block::ToolResult { tool_use_id, content: common::ToolOutput::Text(t), is_error: false }
            if tool_use_id == "u2" && t == "cut v0.4.3"
    ));

    // A message that merely quotes the markup is untouched conversation.
    assert!(matches!(
        &msgs[4].content[0],
        common::Block::Text { text }
            if text == "why does claude add <command-name>/clear</command-name> to my messages?"
    ));

    // Output with no command of its own stands alone under its own id rather
    // than attaching to an unrelated call.
    assert!(matches!(
        &msgs[5].content[0],
        common::Block::ToolResult { tool_use_id, content: common::ToolOutput::Text(t), .. }
            if tool_use_id == "s3" && t == "Export cancelled"
    ));
}

/// Commands survive a trip back through the native format unchanged — and
/// come back as Claude's own markup, never as a `tool_use` block on a user
/// turn, which the Anthropic API rejects when the session is resumed.
#[test]
fn commands_round_trip_as_native_markup() {
    let common = envelope_common();
    let native = claude_code::ClaudeCode::from_common(&common).unwrap();
    let text = claude_code::ClaudeCode::to_text(&native).unwrap();

    assert!(text.contains("<command-name>/release</command-name>"));
    assert!(text.contains("<command-args>patch</command-args>"));
    assert!(text.contains("<local-command-stdout>cut v0.4.3</local-command-stdout>"));
    assert!(text.contains(r#""subtype":"local_command""#));

    // No user line carries a tool_use block.
    for line in text.lines() {
        let record: serde_json::Value = serde_json::from_str(line).unwrap();
        if record.get("type").and_then(serde_json::Value::as_str) == Some("user") {
            assert!(
                !line.contains(r#""type":"tool_use""#),
                "user line carries a tool_use block: {line}"
            );
        }
    }

    // And the conversation itself is stable from the native side: the caveat
    // is gone for good, everything else survives. Call ids are the one
    // exception — `from_common` mints its own deterministic uuids, as it does
    // for every tool call — so compare the pairing rather than the ids.
    let back = claude_code::ClaudeCode::to_common(&native).unwrap();
    assert_eq!(erase_call_ids(&common), erase_call_ids(&back));
}

/// The body with every call id replaced by its 1-based order of first use, so
/// two transcripts compare equal exactly when they pair calls to results the
/// same way.
fn erase_call_ids(transcript: &Transcript<Common>) -> Vec<common::Message> {
    let mut ids = std::collections::HashMap::new();
    let mut renumber = |id: &String| {
        let next = ids.len() + 1;
        ids.entry(id.clone()).or_insert(next).to_string()
    };
    transcript
        .body
        .iter()
        .map(|msg| common::Message {
            content: msg
                .content
                .iter()
                .map(|block| match block {
                    common::Block::ToolUse { id, tool } => common::Block::ToolUse {
                        id: renumber(id),
                        tool: tool.clone(),
                    },
                    common::Block::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => common::Block::ToolResult {
                        tool_use_id: renumber(tool_use_id),
                        content: content.clone(),
                        is_error: *is_error,
                    },
                    other => other.clone(),
                })
                .collect(),
            ..msg.clone()
        })
        .collect()
}

/// The command name and its arguments are searchable, and the text projection
/// shows the command as itself rather than as a wrapped-up tool name.
#[test]
fn commands_render_and_index_as_themselves() {
    let common = envelope_common();

    let rendered = txcript::text::to_text(&common);
    assert!(rendered.contains("[tool 1 /release]\n{\"args\":\"patch\"}"));
    // A command with no arguments renders as its label alone.
    assert!(rendered.contains("[tool 3 /clear]\n"));
    assert!(!rendered.contains("Caveat: The messages below"));
    // The only markup left is the user's own quoting of it.
    assert_eq!(rendered.matches("<command-name>").count(), 1);
    assert!(rendered.contains("[user]\nwhy does claude add <command-name>"));
}

#[cfg(feature = "search")]
#[test]
fn commands_are_searchable_by_name() {
    let common = envelope_common();
    let hits = txcript::search::search(&common, &txcript::search::Query::substring("/release"));
    assert!(
        hits.iter()
            .any(|hit| hit.origin == txcript::search::Origin::ToolUse),
        "the command name should be searchable"
    );
}

#[test]
fn claude_code_preserves_aborted_and_error_stop_reasons() {
    let mut common = sample_common();
    common.body[3].stop_reason = Some(common::StopReason::Aborted);
    let native = claude_code::ClaudeCode::from_common(&common).unwrap();
    let back = claude_code::ClaudeCode::to_common(&native).unwrap();
    assert_eq!(back.body[3].stop_reason, Some(common::StopReason::Aborted));

    common.body[3].stop_reason = Some(common::StopReason::Error);
    let native = claude_code::ClaudeCode::from_common(&common).unwrap();
    let back = claude_code::ClaudeCode::to_common(&native).unwrap();
    assert_eq!(back.body[3].stop_reason, Some(common::StopReason::Error));
}
