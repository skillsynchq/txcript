#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Integration tests for Codex Store fidelity, `to_common` aggregation, and
//! Common codec fixpoints.

use chrono::{DateTime, Utc};
use txcript::common;
use txcript::harness::codex;
use txcript::{Codec, Common, Store, Transcript};

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// Fixture covering image input, reasoning, shell, `apply_patch`, web search,
/// assistant text, usage, and model backfill.
fn exercise_rollout() -> String {
    [
        r#"{"timestamp":"2026-04-01T00:00:00Z","type":"turn_context","payload":{"turn_id":"turn-1","model":"gpt-5.2-codex"}}"#,
        r#"{"timestamp":"2026-04-01T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Please inspect this image"},{"type":"input_image","image_url":"data:image/png;base64,Zm9v"}]}}"#,
        r#"{"timestamp":"2026-04-01T00:00:02Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"Inspecting project files"}],"content":null,"encrypted_content":"secret"}}"#,
        r#"{"timestamp":"2026-04-01T00:00:03Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"ls\"}","call_id":"call-shell"}}"#,
        r#"{"timestamp":"2026-04-01T00:00:04Z","type":"event_msg","payload":{"type":"exec_command_end","call_id":"call-shell","aggregated_output":"file1\nfile2\n","stdout":"","stderr":"","exit_code":0}}"#,
        r#"{"timestamp":"2026-04-01T00:00:05Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call-shell","output":"mirror output"}}"#,
        r#"{"timestamp":"2026-04-01T00:00:06Z","type":"response_item","payload":{"type":"custom_tool_call","status":"completed","call_id":"call-patch","name":"apply_patch","input":"*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n*** End Patch"}}"#,
        r#"{"timestamp":"2026-04-01T00:00:07Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call-patch","output":"{\"output\":\"Success. Updated the following files:\\nM src/main.rs\\n\",\"metadata\":{\"exit_code\":0,\"duration_seconds\":0.0}}"}}"#,
        r#"{"timestamp":"2026-04-01T00:00:08Z","type":"event_msg","payload":{"type":"web_search_end","call_id":"ws-1","query":"Next.js docs","action":{"type":"search","query":"Next.js docs","queries":["Next.js docs","Next.js cache docs"]}}}"#,
        r#"{"timestamp":"2026-04-01T00:00:09Z","type":"response_item","payload":{"type":"web_search_call","status":"completed","action":{"type":"search","query":"Next.js docs","queries":["Next.js docs","Next.js cache docs"]}}}"#,
        r#"{"timestamp":"2026-04-01T00:00:10Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}"#,
        r#"{"timestamp":"2026-04-01T00:00:11Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":25,"output_tokens":40,"reasoning_output_tokens":5,"total_tokens":145}}}}"#,
        r#"{"timestamp":"2026-04-01T00:00:12Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1"}}"#,
    ]
    .join("\n")
        + "\n"
}

#[test]
fn to_common_runs_the_full_aggregation() {
    let dir = tempfile::tempdir().unwrap();
    let store = codex::CodexStore::new(dir.path());
    let src = dir.path().join("rollout-x.jsonl");
    std::fs::write(&src, exercise_rollout()).unwrap();

    let msgs = codex::Codex::to_common(&store.load(&src).unwrap())
        .unwrap()
        .body;

    // The duplicate function_call_output for call-shell is deduped against the
    // canonical exec_command_end, leaving 9 messages.
    assert_eq!(msgs.len(), 9);

    // user: text + image
    assert!(
        matches!(&msgs[0].content[0], common::Block::Text { text } if text == "Please inspect this image")
    );
    assert!(matches!(&msgs[0].content[1], common::Block::Image { .. }));

    // reasoning summary -> thinking (encrypted_content dropped)
    assert!(
        matches!(&msgs[1].content[0], common::Block::Thinking { text, .. } if text == "Inspecting project files")
    );

    // exec_command -> typed Bash
    assert!(matches!(
        &msgs[2].content[0],
        common::Block::ToolUse { id, tool: common::Tool::Bash { command, .. } } if id == "call-shell" && command == "ls"
    ));
    // canonical exec result (from the event log), not the mirror fallback
    assert!(matches!(
        &msgs[3].content[0],
        common::Block::ToolResult { tool_use_id, content: common::ToolOutput::Text(t), is_error: false }
            if tool_use_id == "call-shell" && t == "file1\nfile2\n"
    ));

    // apply_patch single hunk -> typed Edit
    assert!(matches!(
        &msgs[4].content[0],
        common::Block::ToolUse { tool: common::Tool::Edit { file_path, old_string, new_string, .. }, .. }
            if file_path == "src/main.rs" && old_string == "old" && new_string == "new"
    ));
    assert!(matches!(
        &msgs[5].content[0],
        common::Block::ToolResult { tool_use_id, .. } if tool_use_id == "call-patch"
    ));

    // web-search result pairs back to the call via the action key.
    assert!(matches!(
        &msgs[6].content[0],
        common::Block::ToolResult { tool_use_id, content: common::ToolOutput::Text(t), .. }
            if tool_use_id == "ws-1" && t == "Next.js docs\nNext.js cache docs"
    ));
    assert!(matches!(
        &msgs[7].content[0],
        common::Block::ToolUse { id, tool: common::Tool::Raw { tool_name, .. } } if id == "ws-1" && tool_name == "WebSearch"
    ));

    // final assistant text, with model + usage backfilled from the turn.
    assert!(matches!(&msgs[8].content[0], common::Block::Text { text } if text == "Done."));
    assert_eq!(msgs[8].model.as_deref(), Some("gpt-5.2-codex"));
    let usage = msgs[8].usage.unwrap();
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 40);
    assert_eq!(usage.cache_read_input_tokens, Some(25));
}

#[test]
fn legacy_shell_array_command_normalizes_to_bash() {
    let dir = tempfile::tempdir().unwrap();
    let store = codex::CodexStore::new(dir.path());
    let src = dir.path().join("rollout-y.jsonl");
    std::fs::write(
        &src,
        [
            r#"{"timestamp":"2026-04-01T00:00:00Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"ls\"],\"workdir\":\"/repo\"}","call_id":"c1"}}"#,
            r#"{"timestamp":"2026-04-01T00:00:01Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"out"}}"#,
        ]
        .join("\n"),
    )
    .unwrap();

    let msgs = codex::Codex::to_common(&store.load(&src).unwrap())
        .unwrap()
        .body;
    assert!(matches!(
        &msgs[0].content[0],
        common::Block::ToolUse { tool: common::Tool::Bash { command, workdir: Some(w), .. }, .. }
            if command == "ls" && w == "/repo"
    ));
}

#[test]
fn store_round_trip_is_lossless_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let store = codex::CodexStore::new(dir.path());
    let src = dir.path().join("rollout-z.jsonl");
    let body = format!(
        "{}\n{}\n",
        r#"{"timestamp":"2026-01-02T03:04:05.000Z","type":"session_meta","payload":{"id":"sess-1","timestamp":"2026-01-02T03:04:05.000Z","cwd":"/repo","cli_version":"0.104.0","source":"cli","originator":"codex_cli_rs","model_provider":null,"base_instructions":null,"git":{"branch":"main"}}}"#,
        r#"{"timestamp":"2026-01-02T03:04:06.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#,
    );
    std::fs::write(&src, &body).unwrap();

    let loaded = store.load(&src).unwrap();
    let saved = store.save(&loaded).unwrap();
    let reloaded = store.load(&saved.reference).unwrap();

    assert_eq!(loaded.body, reloaded.body);
    // Landed under YYYY/MM/DD with a rollout-<ts>-<id> name. Compare path
    // components, not the string — separators differ on Windows.
    let components: Vec<_> = saved
        .reference
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    assert!(components.windows(3).any(|w| w == ["2026", "01", "02"]));
    assert!(saved.reference.to_string_lossy().contains("rollout-"));
    assert!(saved.reference.to_string_lossy().ends_with("sess-1.jsonl"));
}

#[test]
fn discover_extracts_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("2026").join("01").join("02");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(
        nested.join("rollout-2026-01-02T03-04-05-sess-1.jsonl"),
        format!(
            "{}\n{}\n",
            r#"{"timestamp":"2026-01-02T03:04:05.000Z","type":"session_meta","payload":{"id":"sess-1","timestamp":"2026-01-02T03:04:05.000Z","cwd":"/repo","cli_version":"0.104.0","model":"gpt-5.2-codex","git":{"branch":"main"}}}"#,
            r#"{"timestamp":"2026-01-02T03:04:06.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#,
        ),
    )
    .unwrap();

    let found = codex::CodexStore::new(dir.path()).discover().unwrap();
    assert_eq!(found.len(), 1);
    let meta = &found[0].meta;
    assert_eq!(meta.id, "sess-1");
    assert_eq!(meta.cwd.as_deref(), Some("/repo"));
    assert_eq!(meta.git_branch.as_deref(), Some("main"));
    assert_eq!(meta.model.as_deref(), Some("gpt-5.2-codex"));
    assert_eq!(meta.cli_version.as_deref(), Some("0.104.0"));
}

/// Shaped at codex's granularity: each assistant block is its own message
/// (codex stores one `response_item` per line), every assistant turn carries a
/// model, only the final text turn carries usage, and `stop_reason` is None
/// (codex has no stop reason). This is exactly what `to_common` produces, so
/// `from_common`/`to_common` is a clean fixpoint.
fn sample_common() -> Transcript<Common> {
    let meta = common::Meta {
        id: "sess-1".into(),
        timestamp: ts("2026-01-02T03:04:05.000Z"),
        cwd: Some("/repo".into()),
        git_branch: Some("main".into()),
        title: None,
        cli_version: Some("0.104.0".into()),
        model: Some("gpt-5.2-codex".into()),
    };
    let model = || Some("gpt-5.2-codex".to_string());
    let body = vec![
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::Text {
                text: "inspect".into(),
            }],
            timestamp: ts("2026-01-02T03:04:06.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::Thinking {
                text: "reasoning".into(),
                signature: None,
                encrypted: None,
            }],
            timestamp: ts("2026-01-02T03:04:07.000Z"),
            model: model(),
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::ToolUse {
                id: "call-x".into(),
                tool: common::Tool::Bash {
                    command: "ls".into(),
                    workdir: None,
                    timeout_ms: None,
                    description: None,
                    run_in_background: false,
                },
            }],
            timestamp: ts("2026-01-02T03:04:08.000Z"),
            model: model(),
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::User,
            content: vec![common::Block::ToolResult {
                tool_use_id: "call-x".into(),
                content: common::ToolOutput::Text("file1\nfile2\n".into()),
                is_error: false,
            }],
            timestamp: ts("2026-01-02T03:04:09.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        common::Message {
            role: common::Role::Assistant,
            content: vec![common::Block::Text {
                text: "Done.".into(),
            }],
            timestamp: ts("2026-01-02T03:04:10.000Z"),
            model: model(),
            stop_reason: None,
            usage: Some(common::Usage {
                input_tokens: 100,
                output_tokens: 40,
                cache_read_input_tokens: Some(25),
                cache_creation_input_tokens: None,
            }),
        },
    ];
    Transcript::new(meta, body)
}

#[test]
fn codec_fixpoint_through_common_loses_nothing() {
    let common = sample_common();
    let native = codex::Codex::from_common(&common).unwrap();
    let back = codex::Codex::to_common(&native).unwrap();
    assert_eq!(common, back);
}
