#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Cowork harness tests: Store round trip, metadata discovery, Common
//! extraction, codec fixpoints, and format-specific behavior.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tempfile::TempDir;
use txcript::common::{
    Block, ImageSource, Message, Meta, Role, StopReason, Tool, ToolOutput, Usage,
};
use txcript::harness::claude_code::Record;
use txcript::harness::cowork::{Cowork, CoworkStore};
use txcript::{Codec, Store, TextCodec, Transcript};

const ORG: &str = "f00cbda9-1bc5-46fe-adb4-ea0be3ce03cb";
const ACCOUNT: &str = "3f389f6c-c5fe-4c2d-92bf-8f61bfa2bf6c";
const SESSION_ID: &str = "local_5a7d4a86-4628-4182-a822-292dc8a1df9d";
const CLI_ID: &str = "c9258e92-ad74-42f8-b67c-c9186dc46931";
const CWD: &str = "/repo";

fn ts(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn png_image_block() -> Block {
    Block::Image {
        source: ImageSource {
            source_type: "base64".into(),
            media_type: "image/png".into(),
            data: "UE5HYnl0ZXM=".into(),
        },
    }
}

/// The app's session record, shaped like a real `local_*.json` (the
/// conversation never lives here — only settings and bookkeeping).
fn header_json() -> String {
    json!({
        "sessionId": SESSION_ID,
        "processName": "pensive-youthful-mendel",
        "cliSessionId": CLI_ID,
        "cwd": CWD,
        "createdAt": 1_783_372_201_477_i64,
        "lastActivityAt": 1_783_372_232_997_i64,
        "model": "claude-opus-4-8",
        "isArchived": false,
        "title": "Security deposit refund terms",
        "vmProcessName": "pensive-youthful-mendel",
        "hostLoopMode": true,
        "initialMessage": "what are the terms for: refund of security deposit.",
        "chromePermissionMode": "skip_all_permission_checks",
        "userSelectedFolders": [],
        "enabledMcpTools": {"mcp__workspace__bash": true},
        "slashCommands": ["compact", "context"],
        "systemPrompt": "Claude is powering Cowork mode…",
        "egressAllowedDomains": ["github.com"],
        "accountName": "Me",
        "emailAddress": "me@example.com",
    })
    .to_string()
}

/// The Claude Code transcript Cowork keeps under its per-task config dir,
/// mirroring the real sample's record kinds: queue/prompt bookkeeping, the
/// `<uploaded_files>` prompt with an image, thinking + a Read call, the
/// tool result, the `isMeta` page images, deferred-tool attachments, and
/// an `ai-title` line — every non-message kind must survive untouched.
fn transcript_jsonl() -> String {
    let env = |uuid: &str, parent: Option<&str>, at: &str| {
        json!({
            "uuid": uuid,
            "parentUuid": parent,
            "timestamp": at,
            "sessionId": CLI_ID,
            "cwd": CWD,
            "gitBranch": "HEAD",
            "version": "2.1.177",
            "isSidechain": false,
            "userType": "external",
            "entrypoint": "sdk-ts",
        })
    };
    let line = |kind: &str, mut envelope: Value, rest: Value| {
        let obj = envelope.as_object_mut().unwrap();
        obj.insert("type".into(), Value::String(kind.into()));
        for (k, v) in rest.as_object().unwrap() {
            obj.insert(k.clone(), v.clone());
        }
        envelope.to_string() + "\n"
    };
    [
        json!({"type": "queue-operation", "operation": "enqueue",
            "timestamp": "2026-07-06T21:10:05.420Z", "sessionId": CLI_ID}).to_string() + "\n",
        line("user", env("u1", None, "2026-07-06T21:10:05.652Z"), json!({
            "promptId": "p1", "permissionMode": "bypassPermissions", "promptSource": "sdk",
            "message": {"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "UE5HYnl0ZXM="}},
                {"type": "text", "text": "<uploaded_files>\n<file><file_path>terms.pdf</file_path></file>\n</uploaded_files>\n\nwhat are the terms for: refund of security deposit."},
            ]}})),
        json!({"type": "attachment", "uuid": "a1", "parentUuid": "u1", "sessionId": CLI_ID,
            "timestamp": "2026-07-06T21:10:05.700Z", "cwd": CWD, "gitBranch": "HEAD",
            "version": "2.1.177", "isSidechain": false, "userType": "external", "entrypoint": "sdk-ts",
            "attachment": {"type": "deferred_tools_delta", "addedNames": ["WebSearch"], "addedLines": []}}).to_string() + "\n",
        line("assistant", env("a2", Some("u1"), "2026-07-06T21:10:11.900Z"), json!({
            "requestId": "req_1",
            "message": {"role": "assistant", "model": "claude-opus-4-8", "id": "msg_1",
                "content": [{"type": "thinking", "thinking": "Read the PDF first.", "signature": "sig1"}],
                "stop_reason": null, "usage": {"input_tokens": 10, "output_tokens": 2}}})),
        line("assistant", env("a3", Some("a2"), "2026-07-06T21:10:16.300Z"), json!({
            "requestId": "req_1",
            "message": {"role": "assistant", "model": "claude-opus-4-8", "id": "msg_1",
                "content": [{"type": "tool_use", "id": "toolu_1", "name": "Read",
                             "input": {"file_path": "/repo/uploads/terms.pdf"}}],
                "stop_reason": null, "usage": {"input_tokens": 10, "output_tokens": 20}}})),
        line("user", env("u4", Some("a3"), "2026-07-06T21:10:19.186Z"), json!({
            "promptId": "p1", "sourceToolAssistantUUID": "a3",
            "toolUseResult": {"type": "pdf", "file": {"filePath": "/repo/uploads/terms.pdf", "numPages": 6}},
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1",
                 "content": [{"type": "text", "text": "PDF with 6 pages (rendered as images)"}]}]}})),
        line("user", env("u5", Some("u4"), "2026-07-06T21:10:19.185Z"), json!({
            "promptId": "p1", "isMeta": true,
            "message": {"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "UE5HYnl0ZXM="}},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "UE5HYnl0ZXM="}},
            ]}})),
        line("assistant", env("a6", Some("u5"), "2026-07-06T21:10:32.970Z"), json!({
            "requestId": "req_2",
            "message": {"role": "assistant", "model": "claude-opus-4-8", "id": "msg_2",
                "content": [{"type": "text", "text": "The deposit is refunded within 30 days."}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 300, "output_tokens": 40, "cache_read_input_tokens": 100}}})),
        json!({"type": "last-prompt", "leafUuid": "a6", "sessionId": CLI_ID}).to_string() + "\n",
        json!({"type": "ai-title", "aiTitle": "Security deposit refund terms", "sessionId": CLI_ID}).to_string() + "\n",
    ]
    .concat()
}

/// A slice of the Agent SDK stream the app appends to `audit.jsonl`, with
/// its HMAC chain fields; opaque to txcript, carried verbatim.
fn audit_jsonl() -> String {
    [
        json!({"type": "system", "subtype": "init", "cwd": CWD, "session_id": CLI_ID,
            "uuid": "x1", "_audit_timestamp": "2026-07-06T21:10:05.667Z", "_audit_hmac": "00ab"}),
        json!({"type": "result", "subtype": "success", "session_id": CLI_ID, "num_turns": 3,
            "uuid": "x2", "_audit_timestamp": "2026-07-06T21:10:33.003Z", "_audit_hmac": "00cd"}),
    ]
    .iter()
    .map(|v| v.to_string() + "\n")
    .collect()
}

/// Lay the fixture out exactly as the app does under `<root>/<org>/<account>/`
/// and return the session record path (the Store's reference).
fn write_fixture(root: &std::path::Path) -> std::path::PathBuf {
    let account = root.join(ORG).join(ACCOUNT);
    let dir = account.join(SESSION_ID);
    let project = dir.join(".claude").join("projects").join("-repo");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(dir.join("outputs")).unwrap();
    std::fs::create_dir_all(dir.join("uploads")).unwrap();
    std::fs::write(project.join(format!("{CLI_ID}.jsonl")), transcript_jsonl()).unwrap();
    // A subagent transcript sits beside the main one and is not a session.
    let sub = project.join(CLI_ID).join("subagents");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("agent-1.jsonl"), "{\"type\":\"user\"}\n").unwrap();
    std::fs::write(dir.join("audit.jsonl"), audit_jsonl()).unwrap();
    std::fs::write(dir.join(".audit-key"), "v1:opaque").unwrap();
    let record = account.join(format!("{SESSION_ID}.json"));
    std::fs::write(&record, header_json()).unwrap();
    record
}

#[test]
fn store_round_trip_is_lossless_on_disk() {
    let src = TempDir::new().unwrap();
    let record = write_fixture(src.path());
    let store = CoworkStore::new(src.path());
    let first = store.load(&record).unwrap();

    // Cowork-only and bookkeeping records are carried, untouched.
    let other = |kind: &str| {
        first.body.transcript.iter().any(|r| {
            matches!(
                r,
                Record::Other(v) if v.get("type").and_then(Value::as_str) == Some(kind)
            )
        })
    };
    assert!(other("queue-operation"));
    assert!(other("attachment"));
    assert!(other("last-prompt"));
    assert!(other("ai-title"));
    assert_eq!(first.body.audit.len(), 2);
    assert_eq!(
        first.body.header.extra.get("chromePermissionMode"),
        Some(&json!("skip_all_permission_checks"))
    );

    // A fresh root with an (empty) account tree, as on a machine where the
    // app has run but holds no sessions yet.
    let dst = TempDir::new().unwrap();
    let other_account = dst.path().join(ORG).join(ACCOUNT);
    std::fs::create_dir_all(&other_account).unwrap();
    let dst_store = CoworkStore::new(dst.path());
    let saved = dst_store.save(&first).unwrap();

    // The app's layout: record beside a same-named directory holding the
    // per-task Claude Code config dir with the transcript under the
    // Claude-encoded cwd.
    assert_eq!(saved.id, SESSION_ID);
    assert_eq!(
        saved.reference,
        other_account.join(format!("{SESSION_ID}.json"))
    );
    let transcript = other_account
        .join(SESSION_ID)
        .join(".claude/projects/-repo")
        .join(format!("{CLI_ID}.jsonl"));
    assert!(
        transcript.is_file(),
        "transcript at {}",
        transcript.display()
    );
    assert!(other_account.join(SESSION_ID).join("audit.jsonl").is_file());
    assert!(other_account.join(SESSION_ID).join("outputs").is_dir());

    let second = dst_store.load(&saved.reference).unwrap();
    assert_eq!(first, second);
}

#[test]
fn discover_extracts_metadata() {
    let root = TempDir::new().unwrap();
    let record = write_fixture(root.path());
    let store = CoworkStore::new(root.path());
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].reference, record);
    let meta = &found[0].meta;
    assert_eq!(meta.id, SESSION_ID);
    assert_eq!(meta.cwd.as_deref(), Some(CWD));
    assert_eq!(meta.title.as_deref(), Some("Security deposit refund terms"));
    assert_eq!(meta.model.as_deref(), Some("claude-opus-4-8"));
    // Start time is the app's createdAt, not the first transcript line.
    assert_eq!(meta.timestamp, ts("2026-07-06T21:10:01.477Z"));
    // CLI version and branch only exist in the transcript.
    assert_eq!(meta.cli_version.as_deref(), Some("2.1.177"));
    assert_eq!(meta.git_branch.as_deref(), Some("HEAD"));

    // Discovery and load agree on every field.
    assert_eq!(store.load(&record).unwrap().meta, *meta);
}

#[test]
fn to_common_extracts_the_claude_code_conversation() {
    let root = TempDir::new().unwrap();
    let record = write_fixture(root.path());
    let store = CoworkStore::new(root.path());
    let common = Cowork::to_common(&store.load(&record).unwrap()).unwrap();
    let msgs = &common.body;

    // queue-operation, attachment, last-prompt and ai-title carry no turn.
    assert_eq!(msgs.len(), 6);

    // The prompt keeps Cowork's <uploaded_files> manifest: it is what the
    // model saw and names the attachment, not boilerplate.
    assert_eq!(msgs[0].role, Role::User);
    assert_eq!(msgs[0].content.len(), 2);
    assert_eq!(msgs[0].content[0], png_image_block());
    assert!(matches!(
        &msgs[0].content[1],
        Block::Text { text } if text.starts_with("<uploaded_files>")
    ));
    assert_eq!(msgs[0].timestamp, ts("2026-07-06T21:10:05.652Z"));

    assert_eq!(
        msgs[1].content,
        vec![Block::Thinking {
            text: "Read the PDF first.".into(),
            signature: Some("sig1".into()),
            encrypted: None,
        }]
    );
    assert_eq!(
        msgs[2].content,
        vec![Block::ToolUse {
            id: "toolu_1".into(),
            tool: Tool::Read {
                file_path: "/repo/uploads/terms.pdf".into(),
                offset: None,
                limit: None,
            },
        }]
    );
    assert_eq!(msgs[2].model.as_deref(), Some("claude-opus-4-8"));

    // Claude's block-array result stays structured.
    assert_eq!(
        msgs[3].content,
        vec![Block::ToolResult {
            tool_use_id: "toolu_1".into(),
            content: ToolOutput::Json(json!([
                {"type": "text", "text": "PDF with 6 pages (rendered as images)"}
            ])),
            is_error: false,
        }]
    );

    // The rendered PDF pages (an isMeta user line) are model context.
    assert_eq!(msgs[4].role, Role::User);
    assert_eq!(msgs[4].content, vec![png_image_block(), png_image_block()]);

    assert_eq!(msgs[5].stop_reason, Some(StopReason::EndTurn));
    assert_eq!(
        msgs[5].usage,
        Some(Usage {
            input_tokens: 300,
            output_tokens: 40,
            cache_read_input_tokens: Some(100),
            cache_creation_input_tokens: None,
        })
    );
}

/// A Common transcript at Cowork's native granularity — Claude Code's, with
/// a `local_` session id and a millisecond start time.
#[allow(clippy::too_many_lines)]
fn representable_common() -> Transcript<txcript::Common> {
    let meta = Meta {
        id: SESSION_ID.into(),
        timestamp: ts("2026-07-06T21:10:01.477Z"),
        cwd: Some(CWD.into()),
        git_branch: Some("main".into()),
        title: Some("Security deposit refund terms".into()),
        cli_version: Some("2.1.177".into()),
        model: Some("claude-opus-4-8".into()),
    };
    let model = || Some("claude-opus-4-8".to_string());
    let body = vec![
        Message {
            role: Role::User,
            content: vec![
                Block::Text {
                    text: "what are the terms for: refund of security deposit.".into(),
                },
                png_image_block(),
            ],
            timestamp: ts("2026-07-06T21:10:05.652Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![
                Block::Thinking {
                    text: "Read the PDF first.".into(),
                    signature: Some("sig1".into()),
                    encrypted: None,
                },
                Block::ToolUse {
                    id: "toolu_1".into(),
                    tool: Tool::Read {
                        file_path: "/repo/uploads/terms.pdf".into(),
                        offset: None,
                        limit: None,
                    },
                },
            ],
            timestamp: ts("2026-07-06T21:10:16.300Z"),
            model: model(),
            stop_reason: Some(StopReason::ToolUse),
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 20,
                cache_read_input_tokens: None,
                cache_creation_input_tokens: None,
            }),
        },
        Message {
            role: Role::User,
            content: vec![Block::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: ToolOutput::Json(json!([{"type": "text", "text": "6 pages"}])),
                is_error: false,
            }],
            timestamp: ts("2026-07-06T21:10:19.186Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![Block::ToolUse {
                id: "toolu_2".into(),
                tool: Tool::Raw {
                    tool_name: "mcp__workspace__bash".into(),
                    input: json!({"command": "ls uploads"}),
                },
            }],
            timestamp: ts("2026-07-06T21:10:20.000Z"),
            model: model(),
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        },
        Message {
            role: Role::User,
            content: vec![Block::ToolResult {
                tool_use_id: "toolu_2".into(),
                content: ToolOutput::Text("ls: sandbox denied".into()),
                is_error: true,
            }],
            timestamp: ts("2026-07-06T21:10:21.000Z"),
            model: None,
            stop_reason: None,
            usage: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![Block::Text {
                text: "The deposit is refunded within 30 days.".into(),
            }],
            timestamp: ts("2026-07-06T21:10:32.970Z"),
            model: model(),
            stop_reason: Some(StopReason::EndTurn),
            usage: Some(Usage {
                input_tokens: 300,
                output_tokens: 40,
                cache_read_input_tokens: Some(100),
                cache_creation_input_tokens: Some(5),
            }),
        },
    ];
    Transcript::new(meta, body)
}

#[test]
fn codec_fixpoint_through_common_loses_nothing() {
    let common = representable_common();
    let native = Cowork::from_common(&common).unwrap();
    let back = Cowork::to_common(&native).unwrap();
    assert_eq!(back.meta, common.meta);
    assert_eq!(back.body, common.body);
}

#[test]
fn from_common_is_deterministic() {
    let common = representable_common();
    let a = Cowork::to_text(&Cowork::from_common(&common).unwrap()).unwrap();
    let b = Cowork::to_text(&Cowork::from_common(&common).unwrap()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn from_common_regenerates_the_app_record_and_the_cli_transcript() {
    let native = Cowork::from_common(&representable_common()).unwrap();
    let header = &native.body.header;

    // Every field the app's record validator requires, plus the ones it
    // shows in its session list.
    assert_eq!(header.session_id.as_deref(), Some(SESSION_ID));
    assert!(
        header
            .process_name
            .as_deref()
            .is_some_and(|p| !p.is_empty())
    );
    assert_eq!(header.cwd.as_deref(), Some(CWD));
    assert_eq!(
        header
            .created_at
            .as_ref()
            .and_then(serde_json::Number::as_i64),
        Some(1_783_372_201_477)
    );
    assert_eq!(
        header
            .last_activity_at
            .as_ref()
            .and_then(serde_json::Number::as_i64),
        Some(1_783_372_232_970),
        "last activity is the final message's time"
    );
    assert_eq!(
        header.title.as_deref(),
        Some("Security deposit refund terms")
    );
    assert_eq!(header.model.as_deref(), Some("claude-opus-4-8"));
    assert_eq!(header.is_archived, Some(false));
    assert_eq!(header.extra.get("hostLoopMode"), Some(&json!(true)));
    assert_eq!(
        header.extra.get("initialMessage"),
        Some(&json!(
            "what are the terms for: refund of security deposit."
        ))
    );

    // The transcript is Claude Code's, stamped with the CLI session id the
    // app resumes by — a UUID derived from the Cowork id.
    let cli = header.cli_session_id.clone().unwrap();
    assert!(uuid::Uuid::parse_str(&cli).is_ok());
    assert_ne!(cli, SESSION_ID);
    let stamped: Vec<Option<&str>> = native
        .body
        .transcript
        .iter()
        .filter_map(|r| match r {
            Record::User(e) | Record::Assistant(e) => Some(e.session_id.as_deref()),
            // Summary and other lines carry no sessionId.
            Record::Summary(_) | Record::Other(_) => None,
        })
        .collect();
    assert_eq!(stamped.len(), 6);
    assert!(stamped.iter().all(|id| *id == Some(cli.as_str())));

    // The audit log is the app's own tamper-evident record; never forged.
    assert!(native.body.audit.is_empty());
}

#[test]
fn from_common_prefixes_foreign_ids_with_local() {
    let mut common = representable_common();
    common.meta.id = "11111111-1111-4111-8111-111111111111".into();
    let native = Cowork::from_common(&common).unwrap();
    assert_eq!(native.meta.id, "local_11111111-1111-4111-8111-111111111111");
    assert_eq!(
        native.body.header.session_id.as_deref(),
        Some("local_11111111-1111-4111-8111-111111111111")
    );
    // Everything but the id survives the prefixing.
    let back = Cowork::to_common(&native).unwrap();
    assert_eq!(back.body, common.body);
}

#[test]
fn text_codec_round_trips_the_session_bundle() {
    let root = TempDir::new().unwrap();
    let record = write_fixture(root.path());
    let store = CoworkStore::new(root.path());
    let loaded = store.load(&record).unwrap();
    let text = Cowork::to_text(&loaded).unwrap();
    let parsed = Cowork::from_text(&text).unwrap();
    assert_eq!(parsed, loaded);
}

#[test]
fn discovery_skips_non_account_trees_and_invalid_records() {
    let root = TempDir::new().unwrap();
    write_fixture(root.path());
    let account = root.path().join(ORG).join(ACCOUNT);

    // The app's other state at the same levels.
    std::fs::create_dir_all(root.path().join("skills-plugin").join(ACCOUNT).join(ORG)).unwrap();
    std::fs::write(
        root.path()
            .join("skills-plugin")
            .join(ACCOUNT)
            .join(ORG)
            .join("local_x.json"),
        header_json(),
    )
    .unwrap();
    std::fs::write(account.join("cowork_settings.json"), "{}").unwrap();
    std::fs::write(account.join("remote-session-spaces.json"), "{}").unwrap();
    // A record the app would reject (no sessionId) and one that isn't JSON.
    std::fs::write(account.join("local_broken.json"), "{\"cwd\": \"/x\"}").unwrap();
    std::fs::write(account.join("local_junk.json"), "not json").unwrap();

    let store = CoworkStore::new(root.path());
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].meta.id, SESSION_ID);
}

#[test]
fn discovery_includes_agent_sessions_and_missing_root_is_empty() {
    let root = TempDir::new().unwrap();
    write_fixture(root.path());
    let agent = root.path().join(ORG).join(ACCOUNT).join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    let mut header: Value = serde_json::from_str(&header_json()).unwrap();
    header["sessionId"] = json!("local_ditto_3f389f6c");
    header["title"] = json!("Ditto");
    std::fs::write(agent.join("local_ditto_3f389f6c.json"), header.to_string()).unwrap();

    let store = CoworkStore::new(root.path());
    let mut titles: Vec<String> = store
        .discover()
        .unwrap()
        .iter()
        .filter_map(|d| d.meta.title.clone())
        .collect();
    titles.sort();
    assert_eq!(titles, vec!["Ditto", "Security deposit refund terms"]);

    assert!(
        CoworkStore::new(root.path().join("nowhere"))
            .discover()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn save_picks_the_most_recently_active_account() {
    let root = TempDir::new().unwrap();
    let stale = root.path().join(ORG).join(ACCOUNT);
    let active = root
        .path()
        .join("0d9f1b4a-7c1e-4e3b-9a6d-2f5c8b1e7d20")
        .join("5e2c7a9b-3d4f-4b8a-8c1d-9e0f1a2b3c4d");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::create_dir_all(&active).unwrap();
    std::fs::write(stale.join("local_old.json"), header_json()).unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    // Write access: Windows refuses to touch the mtime of a read-only handle.
    std::fs::OpenOptions::new()
        .write(true)
        .open(stale.join("local_old.json"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    std::fs::write(active.join("local_new.json"), header_json()).unwrap();

    let store = CoworkStore::new(root.path());
    let saved = store
        .save(&Cowork::from_common(&representable_common()).unwrap())
        .unwrap();
    assert_eq!(saved.reference, active.join(format!("{SESSION_ID}.json")));

    // Without any account tree there is nowhere the app would look.
    let empty = TempDir::new().unwrap();
    assert!(
        CoworkStore::new(empty.path())
            .save(&Cowork::from_common(&representable_common()).unwrap())
            .is_err()
    );
}

#[test]
fn delete_removes_record_and_storage_dir_inside_the_root_only() {
    let root = TempDir::new().unwrap();
    let record = write_fixture(root.path());
    let store = CoworkStore::new(root.path());
    let dir = record.with_extension("");
    assert!(dir.is_dir());
    store.delete(&record).unwrap();
    assert!(!record.exists());
    assert!(!dir.exists());

    // A look-alike outside the root is refused, and left alone.
    let elsewhere = TempDir::new().unwrap();
    let foreign = write_fixture(elsewhere.path());
    assert!(store.delete(&foreign).is_err());
    assert!(foreign.is_file());
}

#[test]
fn fingerprints_follow_the_transcript() {
    let root = TempDir::new().unwrap();
    let record = write_fixture(root.path());
    let store = CoworkStore::new(root.path());
    let before = store.fingerprints(std::slice::from_ref(&record)).unwrap();
    let transcript = record
        .with_extension("")
        .join(".claude/projects/-repo")
        .join(format!("{CLI_ID}.jsonl"));
    let mut text = std::fs::read_to_string(&transcript).unwrap();
    text.push_str("{\"type\":\"queue-operation\"}\n");
    std::fs::write(&transcript, text).unwrap();
    let after = store.fingerprints(std::slice::from_ref(&record)).unwrap();
    let key = record.to_string_lossy().into_owned();
    assert_ne!(before[&key], after[&key]);
    assert!(!after[&key].is_empty());
}
