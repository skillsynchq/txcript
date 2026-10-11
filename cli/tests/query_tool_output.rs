#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! `txcript query --tool-output` and the MCP `search_sessions` tool's
//! `tool_output` argument, end to end through the built binary.
//!
//! Text that only ever appeared in what a tool printed (a build error, a
//! file's contents) used to be unreachable from both: they searched
//! `Origin::DEFAULT`, which leaves tool output out, with no way to ask for
//! it, while `txcript view` showed the text plainly.
//!
//! Every run points `HOME` and the Claude Code root at a temp dir and passes
//! `--from claude-code`, so no real session store is ever read.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const SESSION: &str = "11111111-2222-3333-4444-555555555555";
const NEEDLE: &str = "ZEBRAQUUX_LINKER_FAULT";

/// One Claude Code session whose only mention of [`NEEDLE`] is in a Bash
/// tool's output.
fn write_session(root: &Path) {
    let dir = root.join("claude").join("projects").join("-work-demo");
    std::fs::create_dir_all(&dir).unwrap();
    let line = |v: Value| v.to_string();
    let lines = [
        line(json!({
            "type": "user", "uuid": "u1", "parentUuid": null, "sessionId": SESSION,
            "cwd": "/work/demo", "timestamp": "2026-01-02T03:04:05.000Z",
            "message": {"role": "user", "content": "run the build please"},
        })),
        line(json!({
            "type": "assistant", "uuid": "a1", "parentUuid": "u1", "sessionId": SESSION,
            "timestamp": "2026-01-02T03:04:06.000Z",
            "message": {"role": "assistant", "model": "claude-opus-4-8", "content": [
                {"type": "text", "text": "Running it."},
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "make all"}},
            ]},
        })),
        line(json!({
            "type": "user", "uuid": "u2", "parentUuid": "a1", "sessionId": SESSION,
            "timestamp": "2026-01-02T03:04:07.000Z",
            "message": {"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "t1",
                "content": format!("compiling...\nerror: {NEEDLE} in libfoo.a\nmake: *** Error 1"),
            }]},
        })),
        line(json!({
            "type": "assistant", "uuid": "a2", "parentUuid": "u2", "sessionId": SESSION,
            "timestamp": "2026-01-02T03:04:08.000Z",
            "message": {"role": "assistant", "model": "claude-opus-4-8", "content": [
                {"type": "text", "text": "The build failed at link time."},
            ]},
        })),
    ];
    std::fs::write(
        dir.join(format!("{SESSION}.jsonl")),
        lines.join("\n") + "\n",
    )
    .unwrap();
}

/// The binary, sandboxed to `root`.
fn txcript(root: &Path) -> Command {
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_txcript"));
    cmd.env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("APPDATA", &home)
        .env("XDG_DATA_HOME", &home)
        .env("CODEX_HOME", home.join("codex"))
        .env("CLAUDE_CONFIG_DIR", root.join("claude"))
        .env_remove("TXCRIPT_CACHE");
    cmd
}

fn query(root: &Path, args: &[&str]) -> String {
    let out = txcript(root)
        .args(["query", NEEDLE, "--from", "claude-code"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn query_searches_tool_output_only_when_asked() {
    let root = tempfile::tempdir().unwrap();
    write_session(root.path());

    // The default scope is unchanged: tool output is not searched.
    assert_eq!(
        query(root.path(), &[]),
        format!("no matches for `{NEEDLE}`\n")
    );

    let found = query(root.path(), &["--tool-output"]);
    assert!(found.contains(SESSION), "{found}");
    assert!(
        found.contains(&format!("[tool_result] error: {NEEDLE} in libfoo.a")),
        "{found}"
    );
}

#[test]
fn query_tool_output_reads_through_the_search_cache() {
    let root = tempfile::tempdir().unwrap();
    write_session(root.path());
    let cache = root.path().join("search.db");
    let cache = cache.to_str().unwrap();

    // The first run fills the cache; the second thaws the session from it.
    // Cached documents keep their tool output, so both find it.
    for _ in 0..2 {
        let found = query(root.path(), &["--tool-output", "--cache", cache]);
        assert!(found.contains("[tool_result]"), "{found}");
    }
    assert!(query(root.path(), &["--cache", cache]).starts_with("no matches"));
}

/// Call `search_sessions` over stdio and return its structured result.
fn mcp_search(root: &Path, arguments: &Value) -> Value {
    let mut child = txcript(root)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for message in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"},
        }}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "search_sessions", "arguments": arguments,
        }}),
    ] {
        writeln!(stdin, "{message}").unwrap();
    }
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let response = stdout
        .lines()
        .map(Result::unwrap)
        .map(|line| serde_json::from_str::<Value>(&line).unwrap())
        .find(|message| message["id"] == 2)
        .expect("no search_sessions response");
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(response["result"]["isError"], false, "{response}");
    response["result"]["structuredContent"].clone()
}

#[test]
fn mcp_search_matches_the_cli_flag() {
    let root = tempfile::tempdir().unwrap();
    write_session(root.path());

    for arguments in [
        json!({"pattern": NEEDLE, "from": "claude-code"}),
        json!({"pattern": NEEDLE, "from": "claude-code", "tool_output": false}),
    ] {
        let result = mcp_search(root.path(), &arguments);
        assert_eq!(result["matches"], json!([]), "{arguments}");
    }

    let result = mcp_search(
        root.path(),
        &json!({"pattern": NEEDLE, "from": "claude-code", "tool_output": true}),
    );
    let matches = result["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1, "{result}");
    assert_eq!(matches[0]["session"]["id"], SESSION);
    let hits = matches[0]["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{result}");
    assert_eq!(hits[0]["origin"], "tool_result");
    assert_eq!(hits[0]["line"], format!("error: {NEEDLE} in libfoo.a"));
}
