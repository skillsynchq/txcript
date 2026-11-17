#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use serde_json::json;
use txcript::common::{Block, Role, ToolOutput};
use txcript::harness::{claude_code::ClaudeCode, fx::Fx};
use txcript::{Codec, TextCodec};

/// A user message can hold a tool result and text together. The fx writer
/// took it for a new prompt and read only its text, so the call reached fx
/// with no result.
#[test]
fn a_tool_result_sharing_a_user_message_with_text_reaches_fx() {
    let line = |kind: &str, uuid: &str, secs: u32, content: serde_json::Value| {
        json!({
            "type": kind, "uuid": uuid, "sessionId": "sess-mixed", "cwd": "/work/repo",
            "timestamp": format!("2026-01-02T03:04:{secs:02}.000Z"),
            "message": {"role": kind, "content": content},
        })
        .to_string()
    };
    let source = [
        line("user", "u1", 5, json!("run the tests")),
        line(
            "assistant",
            "a1",
            6,
            json!([
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "cargo test"}},
                {"type": "tool_use", "id": "t2", "name": "Bash", "input": {"command": "cargo clippy"}},
            ]),
        ),
        line(
            "user",
            "u2",
            7,
            json!([
                {"type": "tool_result", "tool_use_id": "t1", "content": "2 failed"},
                {"type": "tool_result", "tool_use_id": "t2", "content": "rejected", "is_error": true},
                {"type": "text", "text": "skip clippy, fix the tests first"},
            ]),
        ),
        line("assistant", "a2", 8, json!([{"type": "text", "text": "Fixing them."}])),
    ]
    .join("\n");
    let common = ClaudeCode::to_common(&ClaudeCode::from_text(&source).unwrap()).unwrap();
    assert_eq!(common.body.len(), 4);
    assert_eq!(common.body[2].content.len(), 3, "one message, three blocks");

    let fx = Fx::from_common(&common).unwrap();
    let back = Fx::to_common(&fx).unwrap();

    let blocks: Vec<(Role, &Block)> = back
        .body
        .iter()
        .flat_map(|m| m.content.iter().map(|b| (m.role, b)))
        .collect();
    let result = |id: &str, text: &str, is_error| Block::ToolResult {
        tool_use_id: id.into(),
        content: ToolOutput::Text(text.into()),
        is_error,
    };
    let text = |text: &str| Block::Text { text: text.into() };
    assert_eq!(
        blocks,
        [
            (Role::User, &text("run the tests")),
            (Role::Assistant, &common.body[1].content[0]),
            (Role::Assistant, &common.body[1].content[1]),
            (Role::User, &result("t1", "2 failed", false)),
            (Role::User, &result("t2", "rejected", true)),
            (Role::User, &text("skip clippy, fix the tests first")),
            (Role::Assistant, &text("Fixing them.")),
        ]
    );
    assert_eq!(fx.body.events.len(), 3, "session header and two turns");
    assert_eq!(
        Fx::to_common(&Fx::from_common(&back).unwrap()).unwrap(),
        back
    );
}
