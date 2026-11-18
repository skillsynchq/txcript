//! Parallel tool calls recorded one call per message (Codex `function_call`
//! items; Claude Code's one-line-per-block JSONL) reach Common as
//! consecutive assistant messages with every result after them. The pi and
//! Claude Code writers replayed each call as its own assistant message, so
//! the first call was no longer followed by its result:
//! - pi paired it with a synthetic "No result provided" error and orphaned
//!   the real result;
//! - Claude Code replaced its result with "[Tool result missing due to
//!   internal error]".
//!
//! Each target's own replay rule is modelled below and checked on the
//! written session.

use std::collections::HashSet;

use serde_json::{Value, json};
use txcript::common::{Block, Role};
use txcript::harness::{claude_code, codex, pi};
use txcript::{Codec, TextCodec, Transcript};

fn codex_session(parallel: bool) -> String {
    let call = |id: &str, file: &str| json!({"type": "response_item", "payload": {"type": "function_call", "call_id": id, "name": "exec_command", "arguments": json!({"cmd": format!("cat {file}")}).to_string()}});
    let output = |id: &str, text: &str| json!({"type": "response_item", "payload": {"type": "function_call_output", "call_id": id, "output": text}});
    let mut lines = vec![
        json!({"type": "session_meta", "payload": {"id": "parallel", "cwd": "/work/repo"}}),
        json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "read a and b"}]}}),
        json!({"type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Reading both."}]}}),
    ];
    if parallel {
        lines.extend([
            call("call_a", "a.txt"),
            call("call_b", "b.txt"),
            output("call_a", "alpha"),
            output("call_b", "bravo"),
        ]);
    } else {
        lines.extend([
            call("call_a", "a.txt"),
            output("call_a", "alpha"),
            call("call_b", "b.txt"),
            output("call_b", "bravo"),
        ]);
    }
    lines.push(json!({"type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "a is alpha, b is bravo"}]}}));
    lines
        .into_iter()
        .map(|mut line| {
            line["timestamp"] = json!("2026-11-16T10:00:00.000Z");
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Claude Code's own JSONL for the same parallel batch: one line per block,
/// each result on its own user line.
fn claude_session() -> String {
    let line = |uuid: &str, parent: Option<&str>, kind: &str, content: Value| {
        json!({"type": kind, "uuid": uuid, "parentUuid": parent, "sessionId": "parallel-cc",
               "timestamp": "2026-11-16T10:00:00.000Z", "cwd": "/work/repo",
               "message": {"role": kind, "content": content}})
        .to_string()
    };
    [
        line("u1", None, "user", json!("read a and b")),
        line("a1", Some("u1"), "assistant", json!([{"type": "text", "text": "Reading both."}])),
        line("a2", Some("a1"), "assistant", json!([{"type": "tool_use", "id": "call_a", "name": "Read", "input": {"file_path": "/work/repo/a.txt"}}])),
        line("a3", Some("a2"), "assistant", json!([{"type": "tool_use", "id": "call_b", "name": "Read", "input": {"file_path": "/work/repo/b.txt"}}])),
        line("u2", Some("a3"), "user", json!([{"type": "tool_result", "tool_use_id": "call_a", "content": "alpha"}])),
        line("u3", Some("u2"), "user", json!([{"type": "tool_result", "tool_use_id": "call_b", "content": "bravo"}])),
        line("a4", Some("u3"), "assistant", json!([{"type": "text", "text": "a is alpha, b is bravo"}])),
    ]
    .join("\n")
}

fn from_codex(parallel: bool) -> Transcript<txcript::Common> {
    codex::Codex::to_common(&codex::Codex::from_text(&codex_session(parallel)).unwrap()).unwrap()
}

fn from_claude() -> Transcript<txcript::Common> {
    claude_code::ClaudeCode::to_common(
        &claude_code::ClaudeCode::from_text(&claude_session()).unwrap(),
    )
    .unwrap()
}

/// pi's replay rule: an assistant message's tool calls must be answered by
/// the `toolResult` messages that directly follow it; anything else first
/// fills the open calls with "No result provided". Returns the synthetic
/// errors pi would add and the results it would find unmatched.
fn pi_replay_violations(text: &str) -> (Vec<String>, Vec<String>) {
    let mut missing = Vec::new();
    let mut orphaned = Vec::new();
    let mut open: Vec<String> = Vec::new();
    for line in text.lines() {
        let line: Value = serde_json::from_str(line).unwrap();
        if line["type"] != "message" {
            continue;
        }
        let msg = &line["message"];
        if msg["role"] == "toolResult" {
            let id = msg["toolCallId"].as_str().unwrap().to_string();
            match open.iter().position(|o| *o == id) {
                Some(p) => {
                    open.remove(p);
                }
                None => orphaned.push(id),
            }
            continue;
        }
        missing.append(&mut open);
        if msg["role"] == "assistant" {
            for block in msg["content"].as_array().unwrap() {
                if block["type"] == "toolCall" {
                    open.push(block["id"].as_str().unwrap().to_string());
                }
            }
        }
    }
    missing.append(&mut open);
    (missing, orphaned)
}

/// Claude Code's replay rule: consecutive assistant lines with the same
/// `message.id` are one API message, consecutive user lines are one API
/// message, and every `tool_use` must be answered in the very next user
/// message; otherwise its result is replaced with "[Tool result missing due
/// to internal error]". Returns the ids that would be replaced.
fn claude_replay_violations(text: &str) -> Vec<String> {
    /// One API message: role, message id, `tool_use` ids, `tool_result` ids.
    type ApiMessage = (String, Option<String>, Vec<String>, HashSet<String>);
    let mut api: Vec<ApiMessage> = Vec::new();
    for line in text.lines() {
        let line: Value = serde_json::from_str(line).unwrap();
        let role = match line["type"].as_str() {
            Some(r @ ("user" | "assistant")) => r.to_string(),
            _ => continue,
        };
        let id = line["message"]["id"].as_str().map(String::from);
        let blocks = line["message"]["content"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let uses: Vec<String> = blocks
            .iter()
            .filter(|b| b["type"] == "tool_use")
            .map(|b| b["id"].as_str().unwrap().to_string())
            .collect();
        let results: HashSet<String> = blocks
            .iter()
            .filter(|b| b["type"] == "tool_result")
            .map(|b| b["tool_use_id"].as_str().unwrap().to_string())
            .collect();
        let merge = api.last().is_some_and(|(r, last_id, _, _)| {
            *r == role && (role == "user" || (id.is_some() && *last_id == id))
        });
        if merge {
            let last = api.last_mut().unwrap();
            last.2.extend(uses);
            last.3.extend(results);
        } else {
            api.push((role, id, uses, results));
        }
    }
    let mut missing = Vec::new();
    for (i, (role, _, uses, _)) in api.iter().enumerate() {
        if role != "assistant" {
            continue;
        }
        let next = api.get(i + 1).filter(|(r, ..)| r == "user");
        for id in uses {
            if !next.is_some_and(|(.., results)| results.contains(id)) {
                missing.push(id.clone());
            }
        }
    }
    missing
}

#[test]
fn parallel_calls_from_codex_and_claude_code_replay_with_their_results_in_pi() {
    for (source, common) in [("codex", from_codex(true)), ("claude_code", from_claude())] {
        // The source shape that triggers it: the calls in separate assistant
        // messages, the results after both.
        let roles: Vec<Role> = common.body.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            [
                Role::User,
                Role::Assistant,
                Role::Assistant,
                Role::Assistant,
                Role::User,
                Role::User,
                Role::Assistant
            ],
            "{source}"
        );

        let text = pi::Pi::to_text(&pi::Pi::from_common(&common).unwrap()).unwrap();
        assert_eq!(
            pi_replay_violations(&text),
            (vec![], vec![]),
            "{source}: {text}"
        );

        // Nothing is lost: both calls and both results, in order.
        let back = pi::Pi::to_common(&pi::Pi::from_text(&text).unwrap()).unwrap();
        let blocks: Vec<String> = back
            .body
            .iter()
            .flat_map(|m| &m.content)
            .map(|b| match b {
                Block::Text { text } => format!("text:{text}"),
                Block::ToolUse { id, .. } => format!("call:{id}"),
                Block::ToolResult { tool_use_id, .. } => format!("result:{tool_use_id}"),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            blocks,
            [
                "text:read a and b",
                "text:Reading both.",
                "call:call_a",
                "call:call_b",
                "result:call_a",
                "result:call_b",
                "text:a is alpha, b is bravo"
            ],
            "{source}"
        );
    }
}

#[test]
fn parallel_calls_from_codex_and_claude_code_replay_with_their_results_in_claude_code() {
    for (source, common) in [("codex", from_codex(true)), ("claude_code", from_claude())] {
        let text = claude_code::ClaudeCode::to_text(
            &claude_code::ClaudeCode::from_common(&common).unwrap(),
        )
        .unwrap();
        assert_eq!(
            claude_replay_violations(&text),
            Vec::<String>::new(),
            "{source}: {text}"
        );

        // The response's lines share one id, as Claude Code writes them; the
        // later, separate response does not.
        let assistant_ids: Vec<Option<String>> = text
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|l| l["type"] == "assistant")
            .map(|l| l["message"]["id"].as_str().map(String::from))
            .collect();
        assert_eq!(assistant_ids.len(), 4, "{source}");
        assert!(
            assistant_ids[0]
                .as_deref()
                .is_some_and(|id| id.starts_with("msg_"))
        );
        assert_eq!(assistant_ids[0], assistant_ids[1], "{source}");
        assert_eq!(assistant_ids[1], assistant_ids[2], "{source}");
        assert_eq!(assistant_ids[3], None, "{source}");

        // Reading the file back gives the same conversation.
        let back =
            claude_code::ClaudeCode::to_common(&claude_code::ClaudeCode::from_text(&text).unwrap())
                .unwrap();
        assert_eq!(back.body, common.body, "{source}");
    }
}

/// Sequential calls (call, result, call, result) were already fine and must
/// be written exactly as before: separate pi messages, no Claude Code ids.
#[test]
fn sequential_calls_are_written_unchanged() {
    let common = from_codex(false);
    let text = pi::Pi::to_text(&pi::Pi::from_common(&common).unwrap()).unwrap();
    assert_eq!(pi_replay_violations(&text), (vec![], vec![]));
    let assistants = text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|l| l["message"]["role"] == "assistant")
        .count();
    assert_eq!(assistants, 4);

    let text =
        claude_code::ClaudeCode::to_text(&claude_code::ClaudeCode::from_common(&common).unwrap())
            .unwrap();
    assert_eq!(claude_replay_violations(&text), Vec::<String>::new());
    assert!(
        text.lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .all(|l| l["message"].get("id").is_none()),
        "{text}"
    );
}
