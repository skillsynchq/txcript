#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Integration tests for token usage aggregation, arithmetic, and cross-harness preservation.

use chrono::{DateTime, Utc};
use txcript::common::{Block, Message, Meta, Role, StopReason, Usage};
use txcript::harness::fx::Fx;
use txcript::harness::hermes::Hermes;
use txcript::{Codec, Transcript};

fn sample_meta() -> Meta {
    Meta {
        id: "usage-session-test".into(),
        timestamp: DateTime::<Utc>::UNIX_EPOCH,
        cwd: Some("/work/test".into()),
        git_branch: Some("feat/usage".into()),
        title: Some("Token Usage Test".into()),
        cli_version: Some("0.14.4".into()),
        model: Some("claude-sonnet".into()),
    }
}

fn user_msg(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![Block::Text { text: text.into() }],
        timestamp: DateTime::<Utc>::UNIX_EPOCH,
        model: None,
        stop_reason: None,
        usage: None,
    }
}

fn asst_msg(text: &str, usage: Option<Usage>) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![Block::Text { text: text.into() }],
        timestamp: DateTime::<Utc>::UNIX_EPOCH,
        model: Some("claude-sonnet".into()),
        stop_reason: Some(StopReason::EndTurn),
        usage,
    }
}

#[test]
fn usage_arithmetic_methods_and_sums() {
    let empty = Usage::default();
    assert!(empty.is_zero());
    assert_eq!(empty.total_tokens(), 0);

    let u1 = Usage {
        input_tokens: 150,
        output_tokens: 45,
        cache_read_input_tokens: Some(30),
        cache_creation_input_tokens: None,
    };
    assert!(!u1.is_zero());
    assert_eq!(u1.total_tokens(), 225);

    let u2 = Usage {
        input_tokens: 250,
        output_tokens: 80,
        cache_read_input_tokens: Some(50),
        cache_creation_input_tokens: Some(20),
    };
    assert_eq!(u2.total_tokens(), 400);

    let combined = u1 + u2;
    assert_eq!(combined.input_tokens, 400);
    assert_eq!(combined.output_tokens, 125);
    assert_eq!(combined.cache_read_input_tokens, Some(80));
    assert_eq!(combined.cache_creation_input_tokens, Some(20));
    assert_eq!(combined.total_tokens(), 625);

    let mut accum = u1;
    accum += u2;
    assert_eq!(accum, combined);

    let list = vec![u1, u2, Usage::default()];
    let summed: Usage = list.into_iter().sum();
    assert_eq!(summed, combined);
}

#[test]
fn transcript_total_usage_aggregation() {
    // 1. Transcript with no usage on any turn
    let empty_t = Transcript::new(sample_meta(), vec![user_msg("hi"), asst_msg("hello", None)]);
    assert_eq!(empty_t.total_usage(), None);

    // 2. Transcript with multiple assistant turns reporting usage
    let u1 = Usage {
        input_tokens: 100,
        output_tokens: 20,
        cache_read_input_tokens: Some(10),
        cache_creation_input_tokens: None,
    };
    let u2 = Usage {
        input_tokens: 200,
        output_tokens: 50,
        cache_read_input_tokens: None,
        cache_creation_input_tokens: Some(15),
    };
    let populated_t = Transcript::new(
        sample_meta(),
        vec![
            user_msg("step 1"),
            asst_msg("res 1", Some(u1)),
            user_msg("step 2"),
            asst_msg("res 2", Some(u2)),
        ],
    );

    let total = populated_t.total_usage().expect("should aggregate usage");
    assert_eq!(total.input_tokens, 300);
    assert_eq!(total.output_tokens, 70);
    assert_eq!(total.cache_read_input_tokens, Some(10));
    assert_eq!(total.cache_creation_input_tokens, Some(15));
    assert_eq!(total.total_tokens(), 395);
}

#[test]
fn fx_token_usage_preservation_roundtrip() {
    let u1 = Usage {
        input_tokens: 120,
        output_tokens: 30,
        cache_read_input_tokens: Some(10),
        cache_creation_input_tokens: None,
    };
    let u2 = Usage {
        input_tokens: 180,
        output_tokens: 70,
        cache_read_input_tokens: Some(20),
        cache_creation_input_tokens: Some(5),
    };

    let original = Transcript::new(
        sample_meta(),
        vec![
            user_msg("first prompt"),
            asst_msg("first response", Some(u1)),
            user_msg("second prompt"),
            asst_msg("second response", Some(u2)),
        ],
    );

    // Convert to Fx native representation
    let fx_transcript = Fx::from_common(&original).expect("from_common should succeed");

    // Verify session.json token totals
    let session = fx_transcript
        .body
        .session
        .as_ref()
        .expect("session.json exists");
    assert_eq!(session["total_input_tokens"], 300);
    assert_eq!(session["total_output_tokens"], 100);

    // Verify usage-v2.json sidecar was created and has aggregated counts
    let usage_v2 = fx_transcript
        .body
        .usage
        .as_ref()
        .expect("usage-v2.json exists");
    assert_eq!(usage_v2["input_tokens"], 300);
    assert_eq!(usage_v2["output_tokens"], 100);
    assert_eq!(usage_v2["cache_read_tokens"], 30);
    assert_eq!(usage_v2["cache_write_tokens"], 5);

    // Convert back to Common
    let roundtrip = Fx::to_common(&fx_transcript).expect("to_common should succeed");

    // Check that total_usage matches
    let rt_total = roundtrip
        .total_usage()
        .expect("roundtrip should have usage");
    assert_eq!(rt_total.input_tokens, 300);
    assert_eq!(rt_total.output_tokens, 100);
}

#[test]
fn hermes_token_usage_preservation_roundtrip() {
    let u = Usage {
        input_tokens: 450,
        output_tokens: 120,
        cache_read_input_tokens: Some(80),
        cache_creation_input_tokens: Some(25),
    };

    let original = Transcript::new(
        sample_meta(),
        vec![
            user_msg("solve this problem"),
            asst_msg("solved it", Some(u)),
        ],
    );

    // Convert to Hermes native
    let hermes_transcript = Hermes::from_common(&original).expect("Hermes from_common");

    // Verify the assistant row contains the serialized usage object
    let rows = hermes_transcript.body["messages"]
        .as_array()
        .expect("messages array");
    let asst_row = rows
        .iter()
        .find(|r| r["role"] == "assistant")
        .expect("assistant row");
    let row_usage = &asst_row["usage"];
    assert_eq!(row_usage["prompt_tokens"], 450);
    assert_eq!(row_usage["completion_tokens"], 120);
    assert_eq!(row_usage["cached_tokens"], 80);
    assert_eq!(row_usage["cache_creation_tokens"], 25);
    assert_eq!(row_usage["total_tokens"], 675);

    // Convert back to Common
    let roundtrip = Hermes::to_common(&hermes_transcript).expect("Hermes to_common");
    let rt_total = roundtrip.total_usage().expect("roundtrip has total usage");
    assert_eq!(rt_total.input_tokens, 450);
    assert_eq!(rt_total.output_tokens, 120);
    assert_eq!(rt_total.cache_read_input_tokens, Some(80));
    assert_eq!(rt_total.cache_creation_input_tokens, Some(25));
    assert_eq!(rt_total.total_tokens(), 675);
}
