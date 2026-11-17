//! Issue #104: Codex code mode (`custom_tool_call` named `exec`) stores the
//! call input as a raw JS string and the result as an array of `input_text`
//! content items. The Codex reader only accepted string outputs, so the
//! result arrived empty in every target (Claude Code resumed with "the file
//! read never returned anything"), and the `OpenCode` writer passed the
//! string input through, which `opencode import` rejects with "Expected
//! object ... at `["state"]["input"]`".

use serde_json::{Value, json};
use txcript::common::{Block, ToolOutput};
use txcript::harness::{claude_code, codex, opencode};
use txcript::{Codec, TextCodec};

const SCRIPT: &str =
    "text(await tools.exec_command({cmd:\"cat marker.txt\",max_output_tokens:1000}));\n";
const MARKER: &str = "The harbour gate code is 4471.";

fn code_mode_session() -> String {
    let call_id = "call_bb06566fce2b4dc9bc3d78aeccea79c7";
    [
        json!({"type": "session_meta", "payload": {"id": "code-mode", "cwd": "/work/repo"}}),
        json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Read marker.txt and tell me what it says"}]}}),
        json!({"type": "response_item", "payload": {"type": "custom_tool_call", "status": "completed", "call_id": call_id, "name": "exec", "input": SCRIPT}}),
        json!({"type": "response_item", "payload": {"type": "custom_tool_call_output", "call_id": call_id, "output": [
            {"type": "input_text", "text": "Script completed\nWall time 0.2 seconds\nOutput:\n"},
            {"type": "input_text", "text": format!("{{\"chunk_id\":\"61df54\",\"exit_code\":0,\"output\":\"{MARKER}\\n\"}}")},
        ]}}),
    ]
    .map(|mut line| {
        line["timestamp"] = json!("2026-11-15T03:52:12.000Z");
        line.to_string()
    })
    .join("\n")
}

fn find_all<'a>(v: &'a Value, pred: &dyn Fn(&Value) -> bool, out: &mut Vec<&'a Value>) {
    if pred(v) {
        out.push(v);
    }
    match v {
        Value::Array(items) => items.iter().for_each(|i| find_all(i, pred, out)),
        Value::Object(map) => map.values().for_each(|i| find_all(i, pred, out)),
        _ => {}
    }
}

#[test]
fn codex_code_mode_result_survives_and_opencode_input_is_an_object() {
    let native = codex::Codex::from_text(&code_mode_session()).unwrap();
    let common = codex::Codex::to_common(&native).unwrap();

    // The array-of-content-items output must be read, not dropped.
    let result = common
        .body
        .iter()
        .flat_map(|m| &m.content)
        .find_map(|b| match b {
            Block::ToolResult { content, .. } => Some(content),
            _ => None,
        })
        .expect("tool result");
    match result {
        ToolOutput::Text(text) => {
            assert!(text.starts_with("Script completed\n"), "{text}");
            assert!(text.contains(MARKER), "{text}");
        }
        other @ ToolOutput::Json(_) => panic!("expected text output, got {other:?}"),
    }

    // Claude Code: the tool_result carries the output.
    let claude =
        claude_code::ClaudeCode::to_text(&claude_code::ClaudeCode::from_common(&common).unwrap())
            .unwrap();
    let lines: Vec<Value> = claude
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let mut results = Vec::new();
    for line in &lines {
        find_all(line, &|v| v["type"] == "tool_result", &mut results);
    }
    assert_eq!(results.len(), 1, "{claude}");
    assert!(results[0].to_string().contains("4471"), "{}", results[0]);

    // OpenCode: `state.input` must be an object that keeps the script, and
    // the folded output must carry the result.
    let oc = opencode::OpenCode::from_common(&common).unwrap();
    let oc_value: Value = serde_json::from_str(&opencode::OpenCode::to_text(&oc).unwrap()).unwrap();
    let mut tools = Vec::new();
    find_all(&oc_value, &|v| v["type"] == "tool", &mut tools);
    assert_eq!(tools.len(), 1, "{oc_value}");
    let state = &tools[0]["state"];
    assert!(state["input"].is_object(), "{state}");
    assert_eq!(state["input"], json!({ "input": SCRIPT }));
    assert!(
        state["output"]
            .as_str()
            .unwrap_or_default()
            .contains(MARKER),
        "{state}"
    );
}

fn header(status: &str) -> Value {
    json!({"type": "input_text", "text": format!("{status}\nWall time 0.1 seconds\nOutput:\n")})
}

/// Follow-up to #104: Codex never writes a call's `success` flag; for code
/// mode the only on-disk failure signal is the `Script failed` status header
/// (the first content item). Array results used to always import as
/// non-errors, so a failed script looked successful in every target.
#[test]
fn codex_code_mode_failed_script_imports_as_an_error() {
    let cases = [
        // (result kind, output items, expected is_error)
        (
            "custom_tool_call_output",
            json!([header("Script failed"), {"type": "input_text", "text": "Script error:\nReferenceError: nope is not defined"}]),
            true,
        ),
        (
            "function_call_output",
            json!([header("Script failed"), {"type": "input_text", "text": "Script error:\nboom"}]),
            true,
        ),
        (
            "custom_tool_call_output",
            json!([header("Script completed"), {"type": "input_text", "text": "ok"}]),
            false,
        ),
        (
            "custom_tool_call_output",
            json!([header("Script terminated")]),
            false,
        ),
        // No code-mode header: no error signal, like a string output.
        (
            "function_call_output",
            json!([{"type": "input_text", "text": "Script failed\nWall time is a phrase in this file"}]),
            false,
        ),
        (
            "function_call_output",
            json!([{"type": "input_text", "text": "caption"}, {"type": "input_image", "image_url": "data:image/png;base64,AA=="}]),
            false,
        ),
    ];
    for (kind, output, expected) in cases {
        let call = if kind == "function_call_output" {
            json!({"type": "function_call", "call_id": "c1", "name": "wait", "arguments": "{\"cell_id\":\"1\"}"})
        } else {
            json!({"type": "custom_tool_call", "call_id": "c1", "name": "exec", "input": SCRIPT})
        };
        let source = [
            json!({"type": "session_meta", "payload": {"id": "code-mode-err", "cwd": "/work/repo"}}),
            json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "go"}]}}),
            json!({"type": "response_item", "payload": call}),
            json!({"type": "response_item", "payload": {"type": kind, "call_id": "c1", "output": output}}),
        ]
        .map(|mut line| {
            line["timestamp"] = json!("2026-11-15T03:52:12.000Z");
            line.to_string()
        })
        .join("\n");
        let common = codex::Codex::to_common(&codex::Codex::from_text(&source).unwrap()).unwrap();
        let is_error = common
            .body
            .iter()
            .flat_map(|m| &m.content)
            .find_map(|b| match b {
                Block::ToolResult { is_error, .. } => Some(*is_error),
                _ => None,
            })
            .expect("tool result");
        assert_eq!(is_error, expected, "{kind}: {output}");

        // The flag reaches a target: Claude Code's tool_result.is_error.
        let claude = claude_code::ClaudeCode::to_text(
            &claude_code::ClaudeCode::from_common(&common).unwrap(),
        )
        .unwrap();
        let mut results = Vec::new();
        for line in claude.lines() {
            let line: Value = serde_json::from_str(line).unwrap();
            let mut found = Vec::new();
            find_all(&line, &|v| v["type"] == "tool_result", &mut found);
            results.extend(found.into_iter().cloned());
        }
        assert_eq!(results.len(), 1, "{claude}");
        assert_eq!(
            results[0]["is_error"].as_bool().unwrap_or(false),
            expected,
            "{}",
            results[0]
        );
    }
}

/// Follow-up to #104: the `OpenCode` writer wraps a non-object input as
/// `{"input": <value>}` and flags it in `state.metadata`; the reader must undo
/// exactly that wrap so Codex -> `OpenCode` -> common returns the original
/// freeform input, while a genuine `{"input": ...}` tool input (no flag) is
/// left alone.
#[test]
fn opencode_round_trip_restores_codex_freeform_input() {
    use txcript::common::Tool;

    let common =
        codex::Codex::to_common(&codex::Codex::from_text(&code_mode_session()).unwrap()).unwrap();
    let original = common
        .body
        .iter()
        .flat_map(|m| &m.content)
        .find_map(|b| match b {
            Block::ToolUse { tool, .. } => Some(tool.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(original.to_canonical(), ("exec".to_string(), json!(SCRIPT)));

    let text =
        opencode::OpenCode::to_text(&opencode::OpenCode::from_common(&common).unwrap()).unwrap();
    let back =
        opencode::OpenCode::to_common(&opencode::OpenCode::from_text(&text).unwrap()).unwrap();
    let back_tool = back
        .body
        .iter()
        .flat_map(|m| &m.content)
        .find_map(|b| match b {
            Block::ToolUse { tool, .. } => Some(tool.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(back_tool, original);
    // And a second hop writes the same OpenCode session again.
    assert_eq!(
        opencode::OpenCode::to_text(&opencode::OpenCode::from_common(&back).unwrap()).unwrap(),
        text
    );

    // A genuine object input `{"input": "..."}` is not unwrapped.
    let genuine = Tool::Raw {
        tool_name: "custom_tool".into(),
        input: json!({"input": "original"}),
    };
    let mut with_genuine = common.clone();
    for block in with_genuine
        .body
        .iter_mut()
        .flat_map(|m| m.content.iter_mut())
    {
        if let Block::ToolUse { tool, .. } = block {
            *tool = genuine.clone();
        }
    }
    let text =
        opencode::OpenCode::to_text(&opencode::OpenCode::from_common(&with_genuine).unwrap())
            .unwrap();
    let back =
        opencode::OpenCode::to_common(&opencode::OpenCode::from_text(&text).unwrap()).unwrap();
    let back_tool = back
        .body
        .iter()
        .flat_map(|m| &m.content)
        .find_map(|b| match b {
            Block::ToolUse { tool, .. } => Some(tool.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        back_tool.to_canonical(),
        ("custom_tool".to_string(), json!({"input": "original"}))
    );
}
