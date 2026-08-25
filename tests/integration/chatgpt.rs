#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! `ChatGPT`'s live detail-response codec. HTTP behavior is covered by the
//! harness module's mock server; these tests cover public conversion bounds.

use serde_json::{Value, json};
use txcript::common::{Block, Role, Tool};
use txcript::harness::chatgpt;
use txcript::{Codec, HarnessId, TextCodec, Transcript};

fn fixture() -> Value {
    json!({
        "conversation_id":"11111111-1111-4111-8111-111111111111",
        "title":"A live ChatGPT conversation",
        "create_time":1_770_000_000,
        "update_time":1_770_000_060,
        "current_node":"result",
        "default_model_slug":"gpt-5-6",
        "future_server_field":{"preserved":true},
        "mapping":{
            "user":{"id":"user","parent":null,"children":["tool","branch"],"message":{"id":"m-user","author":{"role":"user"},"create_time":1_770_000_000,"content":{"content_type":"multimodal_text","parts":["inspect this"]}}},
            "branch":{"id":"branch","parent":"user","children":[],"message":{"id":"m-branch","author":{"role":"assistant"},"create_time":1_770_000_001,"content":{"content_type":"text","parts":["inactive"]},"end_turn":true}},
            "tool":{"id":"tool","parent":"user","children":["result"],"message":{"id":"m-tool","author":{"role":"assistant"},"create_time":1_770_000_002,"content":{"content_type":"code","text":"{\"path\":\"README.md\"}"},"recipient":"file_search","metadata":{"model_slug":"gpt-5-6"}}},
            "result":{"id":"result","parent":"tool","children":[],"message":{"id":"m-result","author":{"role":"tool"},"create_time":1_770_000_003,"content":{"content_type":"multimodal_text","parts":[{"found":true}]}}}
        }
    })
}

fn native() -> Transcript<chatgpt::ChatGpt> {
    chatgpt::ChatGpt::from_text(&fixture().to_string()).unwrap()
}

#[test]
fn text_round_trip_preserves_the_complete_mapping() {
    let first = native();
    let rendered = chatgpt::ChatGpt::to_text(&first).unwrap();
    let second = chatgpt::ChatGpt::from_text(&rendered).unwrap();
    assert_eq!(first, second);
    assert_eq!(second.body.extra["future_server_field"]["preserved"], true);
}

#[test]
fn active_path_maps_chatgpt_tools_and_results() {
    let common = chatgpt::ChatGpt::to_common(&native()).unwrap();
    assert_eq!(common.body.len(), 3);
    assert_eq!(common.body[0].role, Role::User);
    assert!(common.body.iter().all(|message| {
        message
            .content
            .iter()
            .all(|block| !matches!(block, Block::Text { text } if text == "inactive"))
    }));
    let tool_id = match &common.body[1].content[0] {
        Block::ToolUse {
            id,
            tool: Tool::Raw { tool_name, input },
        } if tool_name == "file_search" && input["path"] == "README.md" => id,
        other => panic!("expected raw ChatGPT tool, got {other:?}"),
    };
    assert!(matches!(
        &common.body[2].content[0],
        Block::ToolResult {
            tool_use_id,
            content: txcript::common::ToolOutput::Json(value),
            ..
        } if tool_use_id == tool_id && value["found"] == true
    ));
}

#[test]
fn chatgpt_is_source_only_for_every_write_root() {
    let common = chatgpt::ChatGpt::to_common(&native()).unwrap();
    assert!(chatgpt::ChatGpt::from_common(&common).is_err());
    let dir = tempfile::tempdir().unwrap();
    for root in [None, Some(dir.path())] {
        let error = txcript::local::write(HarnessId::ChatGpt, &common, root)
            .err()
            .unwrap_or_else(|| panic!("ChatGPT write should be refused"));
        assert!(error.to_string().contains("never continued into ChatGPT"));
    }
}

#[test]
fn friendly_aliases_resolve_to_chatgpt() {
    for alias in ["chatgpt", "chat-gpt", "chat_gpt", "openai-chat"] {
        assert_eq!(alias.parse::<HarnessId>().unwrap(), HarnessId::ChatGpt);
    }
}
