#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! Qoder's native disk fidelity, shared conversation codec, and store contracts.
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use txcript::common;
use txcript::harness::qoder::{Qoder, QoderStore};
use txcript::{Codec, Common, HarnessId, Store, TextCodec, Transcript};

const ID: &str = "11111111-2222-4333-8444-555555555555";
const TIME: &str = "2026-01-02T03:04:05.000Z";
fn ts() -> DateTime<Utc> {
    TIME.parse().unwrap()
}
fn render(lines: &[Value]) -> String {
    lines
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}
fn lines() -> Vec<Value> {
    let mut lines = vec![
        json!({"type":"ai-title","sessionId":ID,"aiTitle":"Generated title"}),
        json!({"type":"user","uuid":"u1","parentUuid":null,"sessionId":ID,"cwd":"/work/proj","gitBranch":"main","version":"1.1.65","timestamp":TIME,"message":{"role":"user","content":"inspect the project"}}),
        json!({"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":ID,"timestamp":TIME,"message":{"role":"assistant","model":"qfmodel","content":[
            {"type":"thinking","thinking":"Check the files","signature":""},
            {"type":"tool_use","id":"read","name":"Read","input":{"file_path":"/work/proj/a.rs","offset":1,"limit":20}},
            {"type":"tool_use","id":"edit","name":"Edit","input":{"file_path":"/work/proj/a.rs","old_string":"old","new_string":"new","replace_all":true}},
            {"type":"tool_use","id":"write","name":"Write","input":{"file_path":"/work/proj/b.rs","content":"hello"}},
            {"type":"tool_use","id":"bash","name":"Bash","input":{"command":"exit 1","description":"Fail deliberately"}},
            {"type":"tool_use","id":"grep","name":"Grep","input":{"pattern":"hello","path":"/work/proj"}},
            {"type":"tool_use","id":"glob","name":"Glob","input":{"pattern":"*.rs","path":"/work/proj"}}
        ]}}),
        json!({"type":"user","uuid":"u2","parentUuid":"a1","sessionId":ID,"timestamp":TIME,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"bash","content":"exit status 1","is_error":true}]}}),
    ];
    for kind in [
        "runtime-config",
        "workspace-directories",
        "worktree-state",
        "last-prompt",
        "file-history-snapshot",
        "active-leaf",
        "relocated",
    ] {
        lines.push(json!({"type":kind,"sessionId":ID,"opaque":{"kept":true},"timestamp":TIME}));
    }
    lines.push(json!({"type":"system","subtype":"informational","sessionId":ID,"content":"internal notice","timestamp":TIME}));
    for kind in [
        "hook_output",
        "skill_listing",
        "agent_listing_delta",
        "hook_permission_decision",
        "critical_system_reminder",
    ] {
        lines.push(json!({"type":"attachment","sessionId":ID,"attachment":{"type":kind,"content":"internal data"},"timestamp":TIME}));
    }
    lines
}
fn native() -> Transcript<Qoder> {
    Qoder::from_text(&render(&lines())).unwrap()
}
/// Independent Common fixture: exercise fields not inferred from native parsing.
fn common_fixture() -> Transcript<Common> {
    let meta = common::Meta {
        id: ID.into(),
        timestamp: ts(),
        cwd: Some("/work/proj".into()),
        git_branch: Some("main".into()),
        title: Some("Generated title".into()),
        cli_version: Some("1.1.65".into()),
        model: Some("qfmodel".into()),
        lineage: None,
    };
    let message = |role, content, model| common::Message {
        role,
        content,
        timestamp: ts(),
        model,
        stop_reason: None,
        usage: None,
    };
    let mut assistant = message(
        common::Role::Assistant,
        vec![
            common::Block::Thinking {
                text: "Check the files".into(),
                signature: Some(String::new()),
                encrypted: None,
            },
            common::Block::Text {
                text: "Running the command".into(),
            },
            common::Block::ToolUse {
                id: "bash".into(),
                tool: common::Tool::Bash {
                    command: "exit 1".into(),
                    workdir: None,
                    timeout_ms: None,
                    description: Some("Fail deliberately".into()),
                    run_in_background: false,
                },
            },
        ],
        Some("qfmodel".into()),
    );
    assistant.stop_reason = Some(common::StopReason::ToolUse);
    assistant.usage = Some(common::Usage {
        input_tokens: 10,
        output_tokens: 20,
        cache_read_input_tokens: Some(3),
        cache_creation_input_tokens: Some(4),
    });
    Transcript::new(
        meta,
        vec![
            message(
                common::Role::User,
                vec![common::Block::Text {
                    text: "inspect the project".into(),
                }],
                None,
            ),
            assistant,
            message(
                common::Role::User,
                vec![common::Block::ToolResult {
                    tool_use_id: "bash".into(),
                    content: common::ToolOutput::Text("exit status 1".into()),
                    is_error: true,
                }],
                None,
            ),
        ],
    )
}

#[test]
fn store_round_trip_is_lossless_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let store = QoderStore::new(dir.path());
    let src = dir.path().join("original.jsonl");
    std::fs::write(&src, render(&lines())).unwrap();
    let loaded = store.load(&src).unwrap();
    let saved = store.save(&loaded).unwrap();
    assert_eq!(
        saved.reference,
        dir.path().join("-work-proj").join(format!("{ID}.jsonl"))
    );
    assert_eq!(loaded, store.load(&saved.reference).unwrap());
    assert_eq!(loaded.body.len(), lines().len());
}

#[test]
fn discover_extracts_metadata() {
    for (custom, ai, fork) in [
        (true, true, true),
        (false, true, false),
        (false, false, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = QoderStore::new(dir.path());
        let mut rows = lines();
        if !ai {
            rows.remove(0);
        }
        if custom {
            rows.insert(
                0,
                json!({"type":"custom-title","sessionId":ID,"customTitle":"Named session"}),
            );
        }
        if fork {
            rows[2]["forkedFrom"] =
                json!({"sessionId":"parent-session","messageUuid":"parent-message"});
        }
        let native = Qoder::from_text(&render(&rows)).unwrap();
        let saved = store.save(&native).unwrap();
        let found = store.discover().unwrap();
        assert_eq!(found.len(), 1);
        let meta = &found[0].meta;
        assert_eq!(*meta, store.load(&saved.reference).unwrap().meta);
        assert_eq!(meta.id, ID);
        assert_eq!(meta.cwd.as_deref(), Some("/work/proj"));
        assert_eq!(meta.git_branch.as_deref(), Some("main"));
        assert_eq!(meta.cli_version.as_deref(), Some("1.1.65"));
        assert_eq!(meta.model.as_deref(), Some("qfmodel"));
        assert_eq!(meta.timestamp, ts());
        assert_eq!(
            meta.title.as_deref(),
            if custom {
                Some("Named session")
            } else if ai {
                Some("Generated title")
            } else {
                None
            }
        );
        assert_eq!(
            meta.lineage,
            fork.then(|| common::Lineage {
                parent: "parent-session".into(),
                relation: common::Relation::Fork
            })
        );
    }
}

#[test]
fn to_common_extracts_typed_tools_and_skips_bookkeeping() {
    let c = Qoder::to_common(&native()).unwrap();
    assert_eq!(c.body.len(), 3);
    assert!(
        matches!(&c.body[1].content[0],common::Block::Thinking {signature:Some(s),text,..} if s.is_empty() && text == "Check the files")
    );
    for (block, expected) in c.body[1].content[1..].iter().zip([
        json!({"file_path":"/work/proj/a.rs","offset":1,"limit":20}),
        json!({"file_path":"/work/proj/a.rs","old_string":"old","new_string":"new","replace_all":true}),
        json!({"file_path":"/work/proj/b.rs","content":"hello"}),
        json!({"command":"exit 1","description":"Fail deliberately"}),
        json!({"pattern":"hello","path":"/work/proj"}),
        json!({"pattern":"*.rs","path":"/work/proj"}),
    ]) {
        let common::Block::ToolUse {tool,..} = block else {panic!("expected tool")};
        // Common currently models Grep/Glob as Raw; preserve their names and keys.
        if matches!(tool, common::Tool::Raw { tool_name, .. } if tool_name == "Grep" || tool_name == "Glob") {
            assert!(matches!(block, common::Block::ToolUse { id, .. } if id == "grep" || id == "glob"));
        } else {
            assert!(!matches!(tool, common::Tool::Raw {..}), "unexpected raw: {tool:?}");
        }
        assert_eq!(*tool,common::Tool::from_canonical(match block {common::Block::ToolUse {id,..} => match id.as_str() {"read"=>"Read","edit"=>"Edit","write"=>"Write","bash"=>"Bash","grep"=>"Grep",_=>"Glob"},_=>unreachable!()},expected));
    }
    assert!(
        matches!(&c.body[2].content[0],common::Block::ToolResult {tool_use_id,content:common::ToolOutput::Text(t),is_error:true} if tool_use_id=="bash" && t=="exit status 1")
    );
}

#[test]
fn to_common_preserves_local_model_command_and_output() {
    let mut rows = lines();
    for (uuid, parent, content) in [
        (
            "caveat",
            "u2",
            "<local-command-caveat>Ignore local command scaffolding.</local-command-caveat>",
        ),
        (
            "cmd",
            "caveat",
            "<command-name>/model</command-name><command-message>model</command-message>",
        ),
        (
            "output",
            "cmd",
            "<local-command-stdout>Model set to qfmodel</local-command-stdout>",
        ),
    ] {
        rows.push(json!({"type":"user","uuid":uuid,"parentUuid":parent,"sessionId":ID,"timestamp":TIME,"isMeta":true,"message":{"role":"user","content":content}}));
    }
    let c = Qoder::to_common(&Qoder::from_text(&render(&rows)).unwrap()).unwrap();
    assert_eq!(c.body.len(), 5);
    assert!(
        matches!(&c.body[3].content[0],common::Block::ToolUse {id,tool:common::Tool::Command {command,args:None}} if id=="cmd" && command=="/model")
    );
    assert!(
        matches!(&c.body[4].content[0],common::Block::ToolResult {tool_use_id,content:common::ToolOutput::Text(t),is_error:false} if tool_use_id=="cmd" && t=="Model set to qfmodel")
    );
}

#[test]
fn codec_fixpoint_through_common_loses_nothing() {
    let c = common_fixture();
    assert_eq!(
        Qoder::to_common(&Qoder::from_common(&c).unwrap()).unwrap(),
        c
    );
}
#[test]
fn from_common_is_deterministic() {
    let c = common_fixture();
    assert_eq!(
        Qoder::to_text(&Qoder::from_common(&c).unwrap()).unwrap(),
        Qoder::to_text(&Qoder::from_common(&c).unwrap()).unwrap()
    );
}
#[test]
fn from_common_writes_ai_title_and_trailing_active_leaf() {
    for empty_id in [false, true] {
        let mut c = common_fixture();
        if empty_id {
            c.meta.id.clear();
        }
        c.body.last_mut().unwrap().timestamp = "2026-01-02T03:04:08.123Z".parse().unwrap();
        let n = Qoder::from_common(&c).unwrap();
        let rows: Vec<Value> = Qoder::to_text(&n)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let session = &rows[1]["sessionId"];
        assert!(session.as_str().is_some_and(|s| !s.is_empty()));
        assert_eq!(
            rows[0],
            json!({"type":"ai-title","sessionId":session,"aiTitle":c.meta.title})
        );
        assert!(
            rows.iter()
                .all(|r| r["sessionId"] == *session && r["type"] != "summary")
        );
        assert_eq!(
            rows.last().unwrap(),
            &json!({"type":"active-leaf","sessionId":session,"leafUuid":rows[rows.len()-2]["uuid"],"explicit":false,"timestamp":c.body.last().unwrap().timestamp.timestamp_millis()})
        );
    }
    let mut c = common_fixture();
    c.meta.title = None;
    let n = Qoder::from_common(&c).unwrap();
    assert!(!Qoder::to_text(&n).unwrap().contains("ai-title"));
    c.body.clear();
    c.meta.title = Some("Empty session".into());
    let rows = Qoder::to_text(&Qoder::from_common(&c).unwrap()).unwrap();
    assert!(!rows.contains("active-leaf"));
}

#[test]
fn save_encodes_project_directories_using_observed_qoder_goldens() {
    let long = format!(
        "/tmp/txq-long/{}",
        (1..=14)
            .map(|i| format!("segment_number_{i}"))
            .collect::<Vec<_>>()
            .join("/")
    );
    // Non-ASCII path (an astral emoji is two UTF-16 units, so two dashes and
    // two hash units); 239 units, observed from qodercli 1.1.65.
    let second = format!(
        "/tmp/txq-long2/{}",
        (1..=18)
            .map(|i| format!("ünï_😀_seg{i}"))
            .collect::<Vec<_>>()
            .join("/")
    );
    let second_expected = "-tmp-txq-long2--n-----seg1--n-----seg2--n-----seg3--n-----seg4--n-----seg5--n-----seg6--n-----seg7--n-----seg8--n-----seg9--n-----seg10--n-----seg11--n-----seg12--n-----seg13--n-----seg14--n-----seg15-2lws58".to_string();
    assert_eq!(long.len(), 256);
    assert_eq!(second.encode_utf16().count(), 239);
    assert_eq!(second_expected.len(), 207);
    for (cwd,expected) in [
        ("/tmp/a.b_c d/sub.dir".into(),"-tmp-a-b-c-d-sub-dir".into()),
        ("/é项目-x".into(),"-----x".into()),
        ("/😀x".into(),"---x".into()),
        (r"C:\Users\me".into(),"C--Users-me".into()),
        ("a".repeat(199),"a".repeat(199)),
        ("a".repeat(200),"a".repeat(200)),
        (long,"-tmp-txq-long-segment-number-1-segment-number-2-segment-number-3-segment-number-4-segment-number-5-segment-number-6-segment-number-7-segment-number-8-segment-number-9-segment-number-10-segment-number--75ig72".into()),
        (second,second_expected),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut n = native(); n.meta.cwd = Some(cwd);
        let saved = QoderStore::new(dir.path()).save(&n).unwrap();
        assert_eq!(saved.reference,dir.path().join(expected).join(format!("{ID}.jsonl")));
    }
}
#[test]
fn missing_cwd_saves_under_root_project_and_is_discovered() {
    let dir = tempfile::tempdir().unwrap();
    let store = QoderStore::new(dir.path());
    let mut c = common_fixture();
    c.meta.cwd = None;
    let saved = store.save(&Qoder::from_common(&c).unwrap()).unwrap();
    assert_eq!(
        saved.reference,
        dir.path().join("-").join(format!("{ID}.jsonl"))
    );
    assert_eq!(store.discover().unwrap().len(), 1);
}
#[test]
fn delete_removes_only_session_log_and_discovery_ignores_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    let store = QoderStore::new(dir.path());
    let saved = store.save(&native()).unwrap();
    let sidecar = saved.reference.with_extension("");
    std::fs::create_dir_all(&sidecar).unwrap();
    std::fs::write(sidecar.join("state.json"), "{\"kept\":true}").unwrap();
    std::fs::write(sidecar.join("something.jsonl"), render(&lines())).unwrap();
    assert_eq!(store.discover().unwrap().len(), 1);
    let other = saved.reference.with_extension("txt");
    std::fs::write(&other, "keep").unwrap();
    assert!(store.delete(&other).is_err());
    store.delete(&saved.reference).unwrap();
    assert!(!saved.reference.exists());
    assert_eq!(
        std::fs::read_to_string(sidecar.join("state.json")).unwrap(),
        "{\"kept\":true}"
    );
    assert!(sidecar.join("something.jsonl").is_file());
    assert!(store.discover().unwrap().is_empty());
}
#[test]
fn discovery_tolerates_missing_root_invalid_lines_and_unreadable_files() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        QoderStore::new(dir.path().join("missing"))
            .discover()
            .unwrap()
            .is_empty()
    );
    let project = dir.path().join("-work-proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("valid.jsonl"),
        format!("garbage\n{}\n{{broken\n", render(&lines())),
    )
    .unwrap();
    std::fs::write(project.join("unreadable.jsonl"), [0xff, 0xfe]).unwrap();
    let mut rows = lines();
    // Conversation lines without an id: the session id falls back to the
    // file stem. Bookkeeping lines keep theirs, so it is still a session.
    for row in &mut rows {
        if matches!(row["type"].as_str(), Some("user" | "assistant")) {
            row.as_object_mut().unwrap().remove("sessionId");
        }
    }
    std::fs::write(project.join("fallback.jsonl"), render(&rows)).unwrap();
    let store = QoderStore::new(dir.path());
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 2);
    assert!(found.iter().any(|d| d.meta.id == "fallback"));
    for d in found {
        assert_eq!(d.meta, store.load(&d.reference).unwrap().meta);
    }
}
#[test]
fn text_round_trip_preserves_ai_title_and_forked_from() {
    let mut rows = lines();
    rows[1]["forkedFrom"] = json!({"sessionId":"parent-session","messageUuid":"parent-message"});
    let n = Qoder::from_text(&render(&rows)).unwrap();
    assert_eq!(n, Qoder::from_text(&Qoder::to_text(&n).unwrap()).unwrap());
    assert_eq!(n.meta.title.as_deref(), Some("Generated title"));
    assert_eq!(n.meta.lineage.unwrap().parent, "parent-session");
}
#[test]
fn friendly_aliases_resolve_to_qoder_and_all_ids_round_trip() {
    assert!(HarnessId::ALL.contains(&HarnessId::Qoder));
    for id in HarnessId::ALL {
        assert_eq!(id.as_str().parse::<HarnessId>().unwrap(), id);
    }
    for alias in ["qoder", "qodercli", "qoder_cli", "qoder-cli"] {
        assert_eq!(alias.parse::<HarnessId>().unwrap(), HarnessId::Qoder);
    }
}

/// The shared Claude codec preserves native `timeout` verbatim because Common
/// types Bash's canonical timeout as `timeout_ms`. This is a donor limitation,
/// not a Qoder-specific mapping: the fallback must retain every argument.
#[test]
fn to_common_preserves_native_bash_timeout_in_raw_fallback() {
    let mut rows = lines();
    rows[2]["message"]["content"][4]["input"]["timeout"] = json!(1000);
    let c = Qoder::to_common(&Qoder::from_text(&render(&rows)).unwrap()).unwrap();
    assert_eq!(c.body.len(), 3);
    assert!(matches!(&c.body[1].content[4], common::Block::ToolUse {
        id, tool: common::Tool::Raw { tool_name, input }
    } if id == "bash" && tool_name == "Bash" && input == &json!({"command":"exit 1","description":"Fail deliberately","timeout":1000})));
}

#[test]
fn discover_lists_only_files_that_hold_a_session_record() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("-work-proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("empty.jsonl"), "").unwrap();
    std::fs::write(project.join("garbage.jsonl"), "garbage\n").unwrap();
    std::fs::write(project.join("foreign.jsonl"), "{\"a\":1}\n[1,2]\n").unwrap();
    std::fs::write(project.join("typeless.jsonl"), "{\"sessionId\":\"x\"}\n").unwrap();
    // A session opened and closed without a prompt has bookkeeping lines
    // only, and is still a session.
    let idle = json!({"type":"workspace-directories","sessionId":ID,"directories":["/work/proj"]});
    std::fs::write(project.join(format!("{ID}.jsonl")), format!("{idle}\n")).unwrap();
    let found = QoderStore::new(dir.path()).discover().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].meta.id, ID);
}

#[cfg(unix)]
#[test]
fn delete_unlinks_a_link_itself_and_refuses_files_outside_the_session_layout() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let store = QoderStore::new(dir.path());
    let saved = store.save(&native()).unwrap();
    let project = saved.reference.parent().unwrap().to_path_buf();
    let sidecar = project.join(ID);
    std::fs::create_dir_all(sidecar.join("compression-v2")).unwrap();
    let state = sidecar.join("state.json");
    std::fs::write(&state, "{}").unwrap();

    // A link into a sidecar is unlinked itself; the sidecar file survives.
    let link = project.join("linked.jsonl");
    symlink(&state, &link).unwrap();
    store.delete(&link).unwrap();
    assert!(link.symlink_metadata().is_err());
    assert!(state.exists());

    // A link to another project's session removes the link, not the session.
    let mut other = native();
    other.meta.id = "22222222-3333-4444-8555-666666666666".into();
    other.meta.cwd = Some("/work/other".into());
    let other_saved = store.save(&other).unwrap();
    let cross = project.join("cross.jsonl");
    symlink(&other_saved.reference, &cross).unwrap();
    store.delete(&cross).unwrap();
    assert!(cross.symlink_metadata().is_err());
    assert!(other_saved.reference.exists());

    // Files that are not at <root>/<project>/<session>.jsonl are refused.
    let nested = sidecar.join("compression-v2").join("data.jsonl");
    std::fs::write(&nested, "{}\n").unwrap();
    let loose = dir.path().join("loose.jsonl");
    std::fs::write(&loose, "{}\n").unwrap();
    let notes = project.join("notes.txt");
    std::fs::write(&notes, "keep").unwrap();
    for reference in [&nested, &loose, &notes] {
        assert!(store.delete(reference).is_err(), "{}", reference.display());
        assert!(reference.exists(), "{}", reference.display());
    }

    // A real session still deletes, and its sidecar stays.
    store.delete(&saved.reference).unwrap();
    assert!(!saved.reference.exists());
    assert!(state.exists());
}

#[test]
fn a_session_is_never_its_own_fork_parent() {
    // Conversation lines carry no sessionId, so the id comes from the file
    // stem, which here equals the `forkedFrom` id.
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("-work-proj");
    std::fs::create_dir_all(&project).unwrap();
    let mut rows = lines();
    for row in &mut rows {
        if matches!(row["type"].as_str(), Some("user" | "assistant")) {
            row.as_object_mut().unwrap().remove("sessionId");
        }
    }
    rows[1]["forkedFrom"] = json!({"sessionId":ID,"messageUuid":"m"});
    let path = project.join(format!("{ID}.jsonl"));
    std::fs::write(&path, render(&rows)).unwrap();
    let store = QoderStore::new(dir.path());
    let found = store.discover().unwrap();
    assert_eq!(found[0].meta.id, ID);
    assert_eq!(found[0].meta.lineage, None);
    assert_eq!(store.load(&path).unwrap().meta, found[0].meta);
}

#[test]
fn active_leaf_names_the_last_conversation_line_when_a_command_ends_the_session() {
    let mut rows = lines();
    for (uuid, parent, content) in [
        (
            "cmd",
            "u2",
            "<command-name>/model</command-name><command-message>model</command-message>",
        ),
        (
            "output",
            "cmd",
            "<local-command-stdout>Model set to qfmodel</local-command-stdout>",
        ),
    ] {
        rows.push(json!({"type":"user","uuid":uuid,"parentUuid":parent,"sessionId":ID,"timestamp":"2026-01-02T03:04:09.000Z","isMeta":true,"message":{"role":"user","content":content}}));
    }
    let c = Qoder::to_common(&Qoder::from_text(&render(&rows)).unwrap()).unwrap();
    let written = Qoder::from_common(&c).unwrap();
    let values: Vec<Value> = Qoder::to_text(&written)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let leaf = values.last().unwrap();
    assert_eq!(leaf["type"], "active-leaf");
    // The scenario holds only if a command line really trails the leaf.
    let leaf_at = values
        .iter()
        .position(|v| v["uuid"] == leaf["leafUuid"])
        .unwrap();
    assert!(
        values[leaf_at + 1..values.len() - 1]
            .iter()
            .any(|v| v["type"] == "system")
    );
    let leaf_ts: DateTime<Utc> = values[leaf_at]["timestamp"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(leaf["timestamp"].as_i64(), Some(leaf_ts.timestamp_millis()));
}
