#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Integration tests for the Grok Bot codec and store — JSONL envelopes with
//! `role: tool` results, shell/read normalization, address-prefix stripping,
//! and the codec fixpoint through Common.

use chrono::{TimeZone, Utc};
use serde_json::json;
use txcript::common::{Block, Message, Meta, Role, Tool, ToolOutput};
use txcript::harness::grok_bot;
use txcript::{Codec, Common, HarnessId, Store, TextCodec, Transcript};

fn ts() -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_780_000_000, 0)
        .single()
        .unwrap_or_default()
}

/// Anonymized native fixture modeled on observed Grok Bot JSONL shapes.
fn native_fixture_lines() -> String {
    let records = vec![
        json!({"role":"user","message":{"content":[{"type":"text","text":"[t0u]\nList the project root"}]}}),
        json!({"role":"assistant","message":{"content":[{"type":"text","text":"Checking."}]}}),
        json!({"role":"assistant","message":{"content":[{"type":"tool_use","name":"send_message","input":{"text":{"content":"Checking."}}}]}}),
        json!({"role":"tool","message":{"content":[{"type":"tool_result","name":"send_message","result":{"success":{"messageId":"t0s0"}}}]}}),
        json!({"role":"assistant","message":{"content":[{
            "type":"tool_use","name":"shell",
            "input":{"command":"ls -la","timeout":30000,"description":"list root",
                     "toolCallId":"call-shell-1"}
        }]}}),
        json!({"role":"tool","message":{"content":[{"type":"tool_result","name":"shell","result":{
            "success":{"stdout":"README.md\n","exitCode":0},"isBackground":false
        }}]}}),
        json!({"role":"assistant","message":{"content":[{
            "type":"tool_use","name":"read","input":{"path":"/tmp/README.md","limit":20}
        }]}}),
        json!({"role":"tool","message":{"content":[{"type":"tool_result","name":"read","result":{
            "failure":{"message":"not found"}
        }}]}}),
        json!({"role":"assistant","message":{"content":[{"type":"text","text":"Done."}]}}),
        // Unmodeled role: must survive disk round trips.
        json!({"role":"supervisor","note":"bookkeeping the codec does not model"}),
    ];
    records
        .into_iter()
        .map(|v| serde_json::to_string(&v).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn write_session(root: &std::path::Path, id: &str, body: &str) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{id}.jsonl")), body).unwrap();
    std::fs::write(dir.join(format!("{id}.journal-mode")), "2").unwrap();
}

#[test]
fn store_round_trip_is_lossless_on_disk() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let id = "11111111-2222-4333-8444-555555555555";
    write_session(src.path(), id, &native_fixture_lines());

    let loaded = grok_bot::GrokBotStore::new(src.path())
        .load(&src.path().join(id))
        .unwrap();
    let saved = grok_bot::GrokBotStore::new(dst.path())
        .save(&loaded)
        .unwrap();
    assert_eq!(saved.reference, dst.path().join(id));
    assert_eq!(saved.id, id);

    let reloaded = grok_bot::GrokBotStore::new(dst.path())
        .load(&saved.reference)
        .unwrap();
    assert_eq!(loaded.body.records, reloaded.body.records);
    assert_eq!(
        reloaded.body.records.last().unwrap().get("role").unwrap(),
        "supervisor"
    );
}

#[test]
fn discover_extracts_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    write_session(dir.path(), id, &native_fixture_lines());
    // Stray non-session directory must be skipped.
    std::fs::create_dir_all(dir.path().join("not-a-session")).unwrap();
    std::fs::write(dir.path().join("not-a-session/notes.txt"), "hi").unwrap();

    let found = grok_bot::GrokBotStore::new(dir.path()).discover().unwrap();
    assert_eq!(found.len(), 1);
    let meta = &found[0].meta;
    assert_eq!(meta.id, id);
    assert_eq!(meta.title.as_deref(), Some("List the project root"));
}

#[test]
fn to_common_strips_address_prefixes_and_types_shell_read() {
    let text = native_fixture_lines();
    let common =
        grok_bot::GrokBot::to_common(&grok_bot::GrokBot::from_text(&text).unwrap()).unwrap();
    let msgs = &common.body;

    // user, assistant(text), assistant(send_message), user(result),
    // assistant(shell), user(shell result), assistant(read), user(read err),
    // assistant(Done) — supervisor skipped.
    assert_eq!(msgs.len(), 9);

    assert_eq!(msgs[0].role, Role::User);
    assert!(matches!(&msgs[0].content[0], Block::Text { text } if text == "List the project root"));

    assert!(matches!(
        &msgs[2].content[0],
        Block::ToolUse { tool: Tool::Raw { tool_name, .. }, .. } if tool_name == "send_message"
    ));

    assert!(matches!(
        &msgs[4].content[0],
        Block::ToolUse {
            id,
            tool: Tool::Bash {
                command,
                timeout_ms,
                description,
                ..
            }
        } if id == "call-shell-1"
            && command == "ls -la"
            && *timeout_ms == Some(30_000)
            && description.as_deref() == Some("list root")
    ));

    assert!(matches!(
        &msgs[5].content[0],
        Block::ToolResult { tool_use_id, is_error, .. }
            if tool_use_id == "call-shell-1" && !*is_error
    ));

    assert!(matches!(
        &msgs[6].content[0],
        Block::ToolUse {
            tool: Tool::Read { file_path, limit, .. },
            ..
        } if file_path == "/tmp/README.md" && *limit == Some(20)
    ));

    assert!(matches!(
        &msgs[7].content[0],
        Block::ToolResult { is_error, .. } if *is_error
    ));
}

fn msg(role: Role, content: Vec<Block>) -> Message {
    Message {
        role,
        content,
        timestamp: ts(),
        model: None,
        stop_reason: None,
        usage: None,
    }
}

fn fixpoint_common() -> Transcript<Common> {
    Transcript::new(
        Meta {
            id: "fixpoint-session".into(),
            timestamp: ts(),
            cwd: None,
            git_branch: None,
            title: Some("fixpoint".into()),
            cli_version: None,
            model: None,
        },
        vec![
            msg(
                Role::User,
                vec![Block::Text {
                    text: "run ls".into(),
                }],
            ),
            msg(
                Role::Assistant,
                vec![
                    Block::Text {
                        text: "Running.".into(),
                    },
                    Block::ToolUse {
                        id: "call-1".into(),
                        tool: Tool::Bash {
                            command: "ls".into(),
                            workdir: None,
                            timeout_ms: Some(5_000),
                            description: Some("list".into()),
                            run_in_background: false,
                        },
                    },
                    Block::ToolUse {
                        id: "call-2".into(),
                        tool: Tool::Raw {
                            tool_name: "send_message".into(),
                            input: json!({"text":{"content":"Running."}}),
                        },
                    },
                ],
            ),
            msg(
                Role::User,
                vec![Block::ToolResult {
                    tool_use_id: "call-1".into(),
                    content: ToolOutput::Json(json!({"stdout":"a\n","exitCode":0})),
                    is_error: false,
                }],
            ),
            msg(
                Role::User,
                vec![Block::ToolResult {
                    tool_use_id: "call-2".into(),
                    content: ToolOutput::Json(json!({"messageId":"t0s0"})),
                    is_error: false,
                }],
            ),
            msg(
                Role::Assistant,
                vec![Block::ToolUse {
                    id: "call-3".into(),
                    tool: Tool::Read {
                        file_path: "/tmp/a".into(),
                        offset: None,
                        limit: Some(10),
                    },
                }],
            ),
            msg(
                Role::User,
                vec![Block::ToolResult {
                    tool_use_id: "call-3".into(),
                    content: ToolOutput::Text("missing".into()),
                    is_error: true,
                }],
            ),
        ],
    )
}

#[test]
fn codec_fixpoint_through_common_loses_nothing() {
    let common = fixpoint_common();
    let native = grok_bot::GrokBot::from_common(&common).unwrap();
    let back = grok_bot::GrokBot::to_common(&native).unwrap();
    assert_eq!(back.body, common.body);
}

#[test]
fn from_common_is_deterministic() {
    let common = Transcript::new(
        Meta {
            id: "det-id".into(),
            timestamp: ts(),
            cwd: None,
            git_branch: None,
            title: None,
            cli_version: None,
            model: None,
        },
        vec![Message {
            role: Role::User,
            content: vec![Block::Text { text: "hi".into() }],
            timestamp: ts(),
            model: None,
            stop_reason: None,
            usage: None,
        }],
    );
    let a = grok_bot::GrokBot::to_text(&grok_bot::GrokBot::from_common(&common).unwrap()).unwrap();
    let b = grok_bot::GrokBot::to_text(&grok_bot::GrokBot::from_common(&common).unwrap()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn sniff_skips_non_envelope_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    let id = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff";
    let bad_dir = dir.path().join(id);
    std::fs::create_dir_all(&bad_dir).unwrap();
    std::fs::write(
        bad_dir.join(format!("{id}.jsonl")),
        "{\"type\":\"session_meta\",\"id\":\"x\"}\n",
    )
    .unwrap();
    let found = grok_bot::GrokBotStore::new(dir.path()).discover().unwrap();
    assert!(found.is_empty());
}

#[test]
fn sand_subagent_dirs_are_discovered() {
    let dir = tempfile::tempdir().unwrap();
    let id = "sand-subagent-aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    write_session(
        dir.path(),
        id,
        &format!(
            "{}\n",
            json!({"role":"user","message":{"content":[{"type":"text","text":"[t0u]\nsubtask"}]}})
        ),
    );
    let found = grok_bot::GrokBotStore::new(dir.path()).discover().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].meta.id, id);
    assert_eq!(found[0].meta.title.as_deref(), Some("subtask"));
}

#[test]
fn write_with_root_override_saves_jsonl_only() {
    let common = fixpoint_common();
    let dir = tempfile::tempdir().unwrap();
    let written = txcript::local::write(HarnessId::GrokBot, &common, Some(dir.path())).unwrap();
    assert_eq!(written.id, "fixpoint-session");
    let path = dir
        .path()
        .join(&written.id)
        .join(format!("{}.jsonl", written.id));
    assert!(path.is_file(), "expected {}", path.display());
}

#[test]
fn common_to_transcript_entries_maps_user_and_assistant_text() {
    let entries = grok_bot::common_to_transcript_entries(&fixpoint_common());
    assert!(
        entries
            .iter()
            .any(|e| e.get("kind") == Some(&json!("message"))
                && e.get("role") == Some(&json!("user"))),
        "{entries:?}"
    );
    assert!(
        entries
            .iter()
            .any(|e| e.get("kind") == Some(&json!("send-message"))),
        "{entries:?}"
    );
}

#[test]
fn friendly_aliases_resolve_to_grok_bot() {
    for alias in [
        "grok_bot",
        "grok-bot",
        "grokbot",
        "grok_desktop",
        "grok-desktop",
    ] {
        assert_eq!(alias.parse::<HarnessId>().unwrap(), HarnessId::GrokBot);
    }
}

#[test]
fn agent_profile_from_prefers_metadata_name() {
    let common = fixpoint_common();
    let meta = json!({"name": "Relay bot", "description": "from Simple", "extra": 1});
    let (name, desc) = grok_bot::agent_profile_from(&common, Some(&meta));
    assert_eq!(name, "Relay bot");
    assert_eq!(desc, "from Simple");
}

#[test]
fn agent_profile_from_falls_back_to_title() {
    let common = fixpoint_common();
    let (name, desc) = grok_bot::agent_profile_from(&common, None);
    assert_eq!(name, "fixpoint");
    assert!(desc.contains("txcript"));
}

#[test]
fn preflight_rejects_missing_agents_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let missing_agents = tmp.path().join("no-agents");
    let transcripts = tmp.path().join("transcripts");
    std::fs::create_dir_all(&transcripts).unwrap();
    let err = grok_bot::preflight_live_roots(&missing_agents, &transcripts).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("agents directory missing"), "{msg}");
}

#[test]
fn preflight_accepts_existing_writable_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    let transcripts = tmp.path().join("transcripts");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::create_dir_all(&transcripts).unwrap();
    grok_bot::preflight_live_roots(&agents, &transcripts).unwrap();
}

#[test]
fn discover_unions_agents_profile_and_transcripts() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    let transcripts = tmp.path().join("transcripts");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::create_dir_all(&transcripts).unwrap();

    // Live agent with profile, no JSONL.
    let marcus = "5a1951b5-c513-43dd-92ea-85679adc4a6d";
    let marcus_dir = agents.join(marcus);
    std::fs::create_dir_all(&marcus_dir).unwrap();
    std::fs::write(
        marcus_dir.join("profile.json"),
        r#"{"name":"Marcus","description":"","harness":"temporal"}"#,
    )
    .unwrap();

    // Minted agent with profile + JSONL — title must be profile name, not snippet.
    let minted = "11111111-2222-4333-8444-555555555555";
    let minted_agent = agents.join(minted);
    std::fs::create_dir_all(&minted_agent).unwrap();
    std::fs::write(
        minted_agent.join("profile.json"),
        r#"{"name":"Relay bot","description":"from Simple"}"#,
    )
    .unwrap();
    write_session(
        &transcripts,
        minted,
        &format!(
            "{}\n",
            json!({"role":"user","message":{"content":[{"type":"text","text":"[t0u]\nfirst message snippet"}]}})
        ),
    );

    // Subagent JSONL only (no profile).
    let sub = "sand-subagent-aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    write_session(
        &transcripts,
        sub,
        &format!(
            "{}\n",
            json!({"role":"user","message":{"content":[{"type":"text","text":"[t0u]\nsubtask work"}]}})
        ),
    );

    let found = grok_bot::GrokBotStore::new(&transcripts)
        .with_agents(&agents)
        .discover()
        .unwrap();
    let by_id: std::collections::HashMap<_, _> =
        found.iter().map(|d| (d.meta.id.as_str(), d)).collect();
    assert_eq!(by_id.len(), 3, "{by_id:?}");
    assert_eq!(by_id[marcus].meta.title.as_deref(), Some("Marcus"));
    assert_eq!(by_id[minted].meta.title.as_deref(), Some("Relay bot"));
    assert_eq!(by_id[sub].meta.title.as_deref(), Some("subtask work"));
}

#[test]
fn ui_entries_to_common_maps_message_and_send_message() {
    let entries = vec![
        json!({
            "kind":"send-message",
            "id":"tbs0",
            "message":{"type":"text","content":"Hello from bot"},
            "timestampMs": 1_780_000_000_000i64
        }),
        json!({
            "kind":"send-message",
            "id":"tbs1",
            "message":{"type":"widget","widget":{"prompt":"skip me"}},
            "timestampMs": 1_780_000_000_100i64
        }),
        json!({
            "kind":"message",
            "id":"t0u",
            "role":"user",
            "content":"Hi Marcus",
            "timestampMs": 1_780_000_001_000i64
        }),
        json!({
            "kind":"message",
            "id":"agent-out",
            "role":"assistant",
            "content":"Noted.",
            "timestampMs": 1_780_000_002_000i64
        }),
        json!({
            "kind":"user-attachment",
            "id":"t0ua0",
            "file_name":"x.json",
            "timestampMs": 1_780_000_003_000i64
        }),
    ];
    let common = grok_bot::ui_entries_to_common("agent-1", &entries, Some("Marcus".into()));
    assert_eq!(common.meta.title.as_deref(), Some("Marcus"));
    assert_eq!(common.body.len(), 3);
    assert!(matches!(&common.body[0].content[0], Block::Text { text } if text == "Hello from bot"));
    assert_eq!(common.body[0].role, Role::Assistant);
    assert_eq!(common.body[1].role, Role::User);
    assert!(matches!(&common.body[1].content[0], Block::Text { text } if text == "Hi Marcus"));
    assert_eq!(common.body[2].role, Role::Assistant);
}

#[cfg(any(feature = "opencode", feature = "hermes"))]
#[test]
fn load_reconstructs_from_store_db_when_jsonl_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    let transcripts = tmp.path().join("transcripts");
    std::fs::create_dir_all(&transcripts).unwrap();
    let id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let agent_dir = agents.join(id);
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("profile.json"),
        r#"{"name":"Ledger Bot","description":""}"#,
    )
    .unwrap();

    let db = agent_dir.join("store.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE transcript_entries (
                seq INTEGER PRIMARY KEY,
                id TEXT NOT NULL,
                entry TEXT NOT NULL
            );",
        )
        .unwrap();
        let entries = [
            json!({
                "kind":"message","id":"t0u","role":"user","content":"ping",
                "timestampMs": 1_780_000_100_000i64
            }),
            json!({
                "kind":"send-message","id":"t0s0",
                "message":{"type":"text","content":"pong"},
                "timestampMs": 1_780_000_100_500i64
            }),
        ];
        for (i, e) in entries.iter().enumerate() {
            conn.execute(
                "INSERT INTO transcript_entries(seq, id, entry) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    i64::try_from(i + 1).unwrap_or(i64::MAX),
                    e.get("id").and_then(|v| v.as_str()).unwrap(),
                    e.to_string()
                ],
            )
            .unwrap();
        }
    }

    let store = grok_bot::GrokBotStore::new(&transcripts).with_agents(&agents);
    let loaded = store.load(&agent_dir).unwrap();
    assert_eq!(loaded.meta.id, id);
    assert_eq!(loaded.meta.title.as_deref(), Some("Ledger Bot"));
    let common = grok_bot::GrokBot::to_common(&loaded).unwrap();
    assert_eq!(common.body.len(), 2);
    assert!(matches!(&common.body[0].content[0], Block::Text { text } if text == "ping"));
    assert!(matches!(&common.body[1].content[0], Block::Text { text } if text == "pong"));
}

#[cfg(any(feature = "opencode", feature = "hermes"))]
#[test]
fn load_prefers_jsonl_over_store_db() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    let transcripts = tmp.path().join("transcripts");
    let id = "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff";
    let agent_dir = agents.join(id);
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("profile.json"),
        r#"{"name":"Jsonl Wins","description":""}"#,
    )
    .unwrap();
    let db = agent_dir.join("store.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE transcript_entries (
                seq INTEGER PRIMARY KEY,
                id TEXT NOT NULL,
                entry TEXT NOT NULL
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO transcript_entries(seq, id, entry) VALUES (1, 't0u', ?1)",
            [
                json!({"kind":"message","id":"t0u","role":"user","content":"from-store"})
                    .to_string(),
            ],
        )
        .unwrap();
    }
    write_session(
        &transcripts,
        id,
        &format!(
            "{}\n{}\n",
            json!({"role":"user","message":{"content":[{"type":"text","text":"[t0u]\nfrom-jsonl"}]}}),
            json!({"role":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}),
        ),
    );

    let store = grok_bot::GrokBotStore::new(&transcripts).with_agents(&agents);
    let loaded = store.load(&agent_dir).unwrap();
    assert_eq!(loaded.meta.title.as_deref(), Some("Jsonl Wins"));
    let common = grok_bot::GrokBot::to_common(&loaded).unwrap();
    assert!(
        matches!(&common.body[0].content[0], Block::Text { text } if text == "from-jsonl"),
        "{:?}",
        common.body
    );
}

#[test]
fn load_errors_clearly_when_no_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    let transcripts = tmp.path().join("transcripts");
    std::fs::create_dir_all(&transcripts).unwrap();
    let id = "cccccccc-dddd-4eee-8fff-000000000000";
    let agent_dir = agents.join(id);
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("profile.json"),
        r#"{"name":"Empty Bot","description":""}"#,
    )
    .unwrap();

    let err = grok_bot::GrokBotStore::new(&transcripts)
        .with_agents(&agents)
        .load(&agent_dir)
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("no conversation"), "{msg}");
    assert!(msg.contains(id), "{msg}");
}
