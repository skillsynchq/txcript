#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Regressions for Codex call/result pairing when completed IDs are reused.

use serde_json::json;
use txcript::common::{Block, Role, Tool, ToolOutput};
use txcript::harness::codex;
use txcript::{Codec, Common, TextCodec, Transcript};

use super::{meta, msg};

/// Fixed in 7b854de ("pair reused tool IDs by call occurrence"), issue #59.
/// A global patch-ID set made a shell result use the custom-tool envelope
/// if any later edit reused that ID. Each completed call must retain its
/// native result type and survive reading the exported transcript.
#[test]
fn reused_call_ids_preserve_result_types_and_round_trip() {
    let mut common = Transcript::<Common>::new(meta("reused-calls"), Vec::new());
    let call_template = msg(Role::Assistant, Vec::new(), 0);
    let result_template = msg(Role::User, Vec::new(), 1);
    let bash = Tool::Bash {
        command: "ls".into(),
        workdir: None,
        timeout_ms: None,
        description: None,
        run_in_background: false,
    };
    let tools = [
        bash.clone(),
        Tool::Edit {
            file_path: "main.rs".into(),
            old_string: "old".into(),
            new_string: "new".into(),
            replace_all: false,
        },
        bash.clone(),
        Tool::Write {
            file_path: "new.rs".into(),
            content: "new".into(),
        },
        bash.clone(),
        Tool::Raw {
            tool_name: "ApplyPatch".into(),
            input: serde_json::json!({
                "patch": "*** Begin Patch\n*** Delete File: old.rs\n*** End Patch",
                "files": ["old.rs"],
            }),
        },
        bash,
    ];
    for (i, tool) in tools.into_iter().enumerate() {
        let mut call = call_template.clone();
        call.content = vec![Block::ToolUse {
            id: "reused".into(),
            tool,
        }];
        let mut result = result_template.clone();
        result.content = vec![Block::ToolResult {
            tool_use_id: "reused".into(),
            content: ToolOutput::Text(format!("result {i}")),
            is_error: i % 2 == 1,
        }];
        common.body.extend([call, result]);
    }

    let native = codex::Codex::from_common(&common).unwrap();
    let kinds: Vec<_> = native
        .body
        .iter()
        .filter(|line| line.kind == "response_item" && line.payload["call_id"] == "reused")
        .map(|line| line.payload["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "function_call",
            "function_call_output",
            "custom_tool_call",
            "custom_tool_call_output",
            "function_call",
            "function_call_output",
            "custom_tool_call",
            "custom_tool_call_output",
            "function_call",
            "function_call_output",
            "custom_tool_call",
            "custom_tool_call_output",
            "function_call",
            "function_call_output",
        ]
    );
    assert_eq!(common, codex::Codex::to_common(&native).unwrap());
}

/// Fixed in 7b854de ("pair reused tool IDs by call occurrence"), issue #59.
/// Deduplicating canonical results by ID across the entire transcript
/// erased other calls' only results when they reused that ID. Suppress only
/// the matching mirror, regardless of which copy arrives first.
#[test]
fn reused_call_ids_deduplicate_only_their_own_mirrors() {
    let shell = json!({
        "type": "function_call", "name": "exec_command",
        "arguments": "{\"cmd\":\"ls\"}", "call_id": "reused",
    });
    let patch = json!({
        "type": "custom_tool_call", "name": "apply_patch", "call_id": "reused",
        "input": "*** Begin Patch\n*** Delete File: old.rs\n*** End Patch",
    });
    let fallback = |output| {
        json!({
            "type": "function_call_output", "call_id": "reused", "output": output,
        })
    };
    let cases = [
        (
            shell.clone(),
            "event_msg",
            json!({
                "type": "exec_command_end", "call_id": "reused",
                "aggregated_output": "command failed", "exit_code": 1,
            }),
            ToolOutput::Text("command failed".into()),
        ),
        (
            patch,
            "response_item",
            json!({
                "type": "custom_tool_call_output", "call_id": "reused",
                "output": json!({
                    "output": {"error": "missing file", "files": ["old.rs"]},
                    "metadata": {"exit_code": 1},
                }).to_string(),
            }),
            ToolOutput::Json(json!({"error": "missing file", "files": ["old.rs"]})),
        ),
    ];
    for (call, canonical_kind, canonical, content) in cases {
        for canonical_first in [false, true] {
            let mut pair = [
                (canonical_kind, canonical.clone()),
                ("response_item", fallback("mirror")),
            ];
            if !canonical_first {
                pair.reverse();
            }
            let records = [
                ("response_item", shell.clone()),
                ("response_item", fallback("first")),
                ("response_item", call.clone()),
                pair[0].clone(),
                pair[1].clone(),
                ("response_item", shell.clone()),
                ("response_item", fallback("last")),
            ];
            let text: String = records
                .into_iter()
                .map(|(kind, payload)| json!({"type": kind, "payload": payload}).to_string() + "\n")
                .collect();
            let native = codex::Codex::from_text(&text).unwrap();
            let common = codex::Codex::to_common(&native).unwrap();
            let results: Vec<_> = common
                .body
                .iter()
                .flat_map(|msg| &msg.content)
                .filter(|block| matches!(block, Block::ToolResult { .. }))
                .cloned()
                .collect();
            assert_eq!(
                results,
                [
                    Block::ToolResult {
                        tool_use_id: "reused".into(),
                        content: ToolOutput::Text("first".into()),
                        is_error: false,
                    },
                    Block::ToolResult {
                        tool_use_id: "reused".into(),
                        content: content.clone(),
                        is_error: true,
                    },
                    Block::ToolResult {
                        tool_use_id: "reused".into(),
                        content: ToolOutput::Text("last".into()),
                        is_error: false,
                    },
                ],
                "{}; canonical_first={canonical_first}",
                call["type"]
            );
        }
    }
}
