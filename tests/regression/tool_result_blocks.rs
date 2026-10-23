#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Regressions for Claude tool results that arrive as content-block arrays.

use serde_json::{Value, json};
use txcript::harness::{claude_code::ClaudeCode, codex::Codex, grok::Grok, pi::Pi};
use txcript::{Codec, Common, TextCodec, Transcript};

const TEXT: &str = "row1: alpha\nrow2: \"beta\"";
const PLAIN: &str = "plain line one\nplain \"two\"";
const OBJECT: &str = r#"{"count":3,"rows":["a","b"]}"#;

fn image_data() -> String {
    "iVBORw0KGgo".repeat(15_000)
}

/// A Claude Code session whose tool results cover every shape: a text-block
/// array (every MCP tool), a text + image array (screenshots), a plain
/// string, and a bare JSON object.
fn session() -> Transcript<Common> {
    let base = json!({"sessionId": "blocks-1", "cwd": "/work/repo", "version": "2.0.0"});
    let line = |mut value: Value, second: u32| {
        value["timestamp"] = json!(format!("2026-01-02T03:04:{second:02}.000Z"));
        for (key, field) in base.as_object().unwrap() {
            value[key] = field.clone();
        }
        value.to_string()
    };
    let call =
        |id: &str, name: &str| json!({"type": "tool_use", "id": id, "name": name, "input": {}});
    let text = [
        line(
            json!({"type": "user", "uuid": "u1", "message": {"role": "user", "content": "run them"}}),
            1,
        ),
        line(
            json!({"type": "assistant", "uuid": "a1", "parentUuid": "u1", "message": {
                "id": "m1", "role": "assistant", "model": "claude-opus-4-8", "stop_reason": "tool_use",
                "content": [
                    call("t1", "mcp__db__query"),
                    call("t2", "mcp__shot__capture"),
                    call("t3", "Bash"),
                    call("t4", "mcp__db__stats"),
                ],
            }}),
            2,
        ),
        line(
            json!({"type": "user", "uuid": "u2", "parentUuid": "a1", "message": {
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": [
                        {"type": "text", "text": TEXT},
                    ]},
                    {"type": "tool_result", "tool_use_id": "t2", "content": [
                        {"type": "text", "text": "screenshot taken"},
                        {"type": "image", "source": {
                            "type": "base64", "media_type": "image/png", "data": image_data(),
                        }},
                    ]},
                    {"type": "tool_result", "tool_use_id": "t3", "content": PLAIN},
                    {"type": "tool_result", "tool_use_id": "t4", "content": {"count": 3, "rows": ["a", "b"]}},
                ],
            }}),
            3,
        ),
    ]
    .join("\n");
    ClaudeCode::to_common(&ClaudeCode::from_text(&text).unwrap()).unwrap()
}

/// The text each of the four tool results was written as, in call order.
fn codex_outputs(common: &Transcript<Common>) -> Vec<String> {
    let text = Codex::to_text(&Codex::from_common(common).unwrap()).unwrap();
    text.lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|line| line["payload"]["type"] == "function_call_output")
        .map(|line| line["payload"]["output"].as_str().unwrap().to_owned())
        .collect()
}

fn pi_outputs(common: &Transcript<Common>) -> Vec<String> {
    let text = Pi::to_text(&Pi::from_common(common).unwrap()).unwrap();
    text.lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|line| line["message"]["role"] == "toolResult")
        .map(|line| {
            line["message"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn grok_outputs(common: &Transcript<Common>) -> Vec<String> {
    let native = Grok::from_common(common).unwrap();
    let log = serde_json::to_value(&native.body.chat_history).unwrap();
    log.as_array()
        .unwrap()
        .iter()
        .filter(|record| record["type"] == "tool_result")
        .map(|record| record["content"].as_str().unwrap().to_owned())
        .collect()
}

/// Every Claude tool result that is a content-block array (all of MCP's, and
/// every screenshot or image read) was stringified as compact JSON. Readers
/// saw escaped `\n` and `\"` and reordered keys instead of the text, and an
/// image in the result was dumped as its entire base64 payload: a 150 KB
/// screenshot made a 200 KB `view` line, a 200 KB line in each exported
/// rollout, and the MCP server refused to read the session as over its
/// 100000-byte budget. Block arrays now render as their text, with images as
/// an `[image: <media type>]` placeholder; strings and other JSON are
/// unchanged.
#[test]
fn tool_result_block_arrays_render_as_text_not_json() {
    let common = session();
    let image = image_data();
    let screenshot = "screenshot taken\n\n[image: image/png]";

    // `view` and the MCP `read_session` text.
    let view = txcript::text::to_text(&common);
    assert!(view.contains(&format!("[result 1]\n{TEXT}\n")), "{view}");
    assert!(
        view.contains(&format!("[result 2]\n{screenshot}\n")),
        "{view}"
    );
    assert!(view.contains(&format!("[result 3]\n{PLAIN}\n")));
    assert!(view.contains(&format!("[result 4]\n{OBJECT}\n")));
    assert!(!view.contains(&image[..64]));
    assert!(view.len() < 2_000, "view is {} bytes", view.len());

    // Every exporter that writes plain-string results.
    for outputs in [
        codex_outputs(&common),
        pi_outputs(&common),
        grok_outputs(&common),
    ] {
        assert_eq!(outputs, [TEXT, screenshot, PLAIN, OBJECT]);
    }
}

#[cfg(feature = "search")]
#[test]
fn tool_result_block_arrays_search_as_text_not_json() {
    use txcript::search::{Origin, Query, search};

    let common = session();
    let image = image_data();
    let mut query = Query::substring("alpha");
    query.origins = Origin::ALL.to_vec();
    let hits = search(&common, &query);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].origin, Origin::ToolResult);
    assert_eq!(hits[0].line, "row1: alpha");

    let mut query = Query::substring("screenshot");
    query.origins = Origin::ALL.to_vec();
    let lines: Vec<_> = search(&common, &query)
        .into_iter()
        .map(|hit| hit.line)
        .collect();
    assert_eq!(lines, ["screenshot taken"]);

    for pattern in ["image/png", "iVBOR"] {
        let mut query = Query::substring(pattern);
        query.origins = Origin::ALL.to_vec();
        let lines: Vec<_> = search(&common, &query)
            .into_iter()
            .map(|hit| hit.line)
            .collect();
        assert!(
            lines
                .iter()
                .all(|line| line.len() < 100 && !line.contains(&image[..64]))
        );
    }
}
