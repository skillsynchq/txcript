#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::HashSet;

use serde_json::{Value, json};
use txcript::harness::claude_code::ClaudeCode;
use txcript::harness::pi::Pi;
use txcript::{Codec, TextCodec};

/// Issue #105. A Claude Code local slash command (`/rename`) is a
/// `Tool::Command` call on a *user* turn plus its printed output as the
/// paired `ToolResult`. The pi writer dropped every user-side `ToolUse` but
/// kept the result, so the copy carried a `toolResult` whose `toolCallId`
/// no assistant `toolCall` ever opened — and pi's Anthropic provider
/// refused the whole session on resume (`unexpected tool_use_id found in
/// tool_result blocks`). Output with no command of its own (a `!`-run, or a
/// command whose line was never written) hit the same path.
///
/// pi has no user-side tool call, so the command and its output must land
/// as plain user text, and every `toolResult` that remains must answer a
/// `toolCall` from the assistant message right before it.
#[test]
fn claude_slash_command_converts_to_pi_without_orphan_tool_results() {
    let jsonl = slash_command_session();

    let common = ClaudeCode::to_common(&ClaudeCode::from_text(&jsonl).unwrap()).unwrap();
    let pi_text = Pi::to_text(&Pi::from_common(&common).unwrap()).unwrap();
    let payloads: Vec<Value> = pi_text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v["type"] == "message")
        .map(|v| v["message"].clone())
        .collect();

    // pi's resume rule (the Anthropic API's, which pi forwards verbatim):
    // each run of `toolResult`s answers `toolCall`s from the assistant
    // message immediately before the run.
    let mut open: HashSet<String> = HashSet::new();
    for payload in &payloads {
        match payload["role"].as_str().unwrap() {
            "assistant" => {
                open = payload["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|b| b["type"] == "toolCall")
                    .map(|b| b["id"].as_str().unwrap().to_string())
                    .collect();
            }
            "toolResult" => {
                let id = payload["toolCallId"].as_str().unwrap();
                assert!(
                    open.contains(id),
                    "orphan toolResult {id}: no toolCall in the previous assistant message\n{pi_text}"
                );
            }
            _ => open.clear(),
        }
    }

    // The real tool call still pairs as a call/result.
    assert!(payloads.iter().any(|p| p["role"] == "toolResult"
        && p["toolCallId"] == "toolu_ls"
        && p["toolName"] == "bash"));

    // The command and both outputs survive as user-visible text.
    let user_text: Vec<&str> = payloads
        .iter()
        .filter(|p| p["role"] == "user")
        .flat_map(|p| p["content"].as_array().unwrap())
        .filter_map(|b| b["text"].as_str())
        .collect();
    for expected in [
        "/rename something",
        "Session renamed to: something",
        "Export cancelled",
    ] {
        assert!(
            user_text.iter().any(|t| t.contains(expected)),
            "{expected:?} missing from pi user text: {user_text:?}"
        );
    }
}

/// A Claude Code session with an ordinary tool call, a `/rename` slash
/// command (newer CLIs' `system` line shape, as in the issue) with its
/// output, and a stray output line with no command of its own.
fn slash_command_session() -> String {
    let lines = [
        json!({
            "type": "user", "uuid": "u1", "parentUuid": null,
            "sessionId": "sess-105", "cwd": "/work/repo", "timestamp": "2026-01-02T03:04:05.000Z",
            "message": {"role": "user", "content": "list the files"},
        }),
        json!({
            "type": "assistant", "uuid": "a1", "parentUuid": "u1",
            "sessionId": "sess-105", "timestamp": "2026-01-02T03:04:06.000Z",
            "message": {"role": "assistant", "model": "claude-opus-4-8", "content": [
                {"type": "tool_use", "id": "toolu_ls", "name": "Bash", "input": {"command": "ls"}},
            ]},
        }),
        json!({
            "type": "user", "uuid": "u2", "parentUuid": "a1",
            "sessionId": "sess-105", "timestamp": "2026-01-02T03:04:07.000Z",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_ls", "content": "Cargo.toml\nsrc"},
            ]},
        }),
        json!({
            "type": "assistant", "uuid": "a2", "parentUuid": "u2",
            "sessionId": "sess-105", "timestamp": "2026-01-02T03:04:08.000Z",
            "message": {"role": "assistant", "model": "claude-opus-4-8", "content": [
                {"type": "text", "text": "Two entries."},
            ]},
        }),
        json!({
            "type": "user", "uuid": "u3", "parentUuid": "a2", "isMeta": true,
            "sessionId": "sess-105", "timestamp": "2026-01-02T03:04:09.000Z",
            "message": {"role": "user", "content":
                "<local-command-caveat>Caveat: The messages below were generated by the user while running local commands.</local-command-caveat>"},
        }),
        json!({
            "type": "system", "subtype": "local_command", "uuid": "8fa153f4-572f-48d9-bea6-74ac00dd32f3", "parentUuid": "u3",
            "sessionId": "sess-105", "timestamp": "2026-01-02T03:04:10.000Z",
            "content": "<command-name>/rename</command-name>\n<command-message>rename</command-message>\n<command-args>something</command-args>",
        }),
        json!({
            "type": "system", "subtype": "local_command", "uuid": "s2", "parentUuid": "8fa153f4-572f-48d9-bea6-74ac00dd32f3",
            "sessionId": "sess-105", "timestamp": "2026-01-02T03:04:11.000Z",
            "content": "<local-command-stdout>Session renamed to: something</local-command-stdout>",
        }),
        // Output with no command line to pair with.
        json!({
            "type": "system", "subtype": "local_command", "uuid": "s3", "parentUuid": "s2",
            "sessionId": "sess-105", "timestamp": "2026-01-02T03:04:12.000Z",
            "content": "<local-command-stdout>Export cancelled</local-command-stdout>",
        }),
        json!({
            "type": "user", "uuid": "u4", "parentUuid": "s3",
            "sessionId": "sess-105", "timestamp": "2026-01-02T03:04:13.000Z",
            "message": {"role": "user", "content": "one more thing"},
        }),
    ];
    lines
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}
