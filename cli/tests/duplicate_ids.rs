#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Copies of one session id (Claude Code writes a second copy under another
//! project folder when a session is resumed from another cwd) must resolve to
//! the most recently modified copy, whichever order the file system walks the
//! project folders in. Runs the real `txcript` binary against a synthetic
//! Claude Code store in a temp dir; every other store is pointed at an empty
//! temp dir so no real history is read.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

const ID: &str = "11111111-aaaa-4aaa-8aaa-000000000001";
const OTHER_ID: &str = "11111111-bbbb-4bbb-8bbb-000000000002";

fn line(id: &str, n: usize, text: &str) -> String {
    format!(
        r#"{{"type":"user","uuid":"u{n}","parentUuid":null,"sessionId":"{id}","cwd":"/work/proj","timestamp":"2026-01-02T03:04:{n:02}.000Z","message":{{"role":"user","content":"{text}"}}}}"#
    )
}

/// Write a copy of session `id` into project folder `dir` with `messages`
/// lines and a file mtime `mtime` seconds after a fixed epoch. The last message of
/// a copy names the copy, so output shows which one was loaded.
fn write_copy(projects: &Path, dir: &str, id: &str, messages: usize, tag: &str, mtime: u64) {
    let folder = projects.join(dir);
    fs::create_dir_all(&folder).unwrap();
    let mut body = String::new();
    for n in 1..messages {
        body.push_str(&line(id, n, "shared history"));
        body.push('\n');
    }
    body.push_str(&line(id, messages, tag));
    body.push('\n');
    let path = folder.join(format!("{id}.jsonl"));
    fs::write(&path, body).unwrap();
    let file = fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + mtime))
        .unwrap();
}

struct Env {
    home: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn projects(&self) -> std::path::PathBuf {
        self.home.path().join(".claude").join("projects")
    }

    fn run(&self, args: &[&str]) -> (bool, String) {
        let empty = self.home.path().join("empty");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_txcript"));
        cmd.env_clear()
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("CLAUDE_CONFIG_DIR", self.home.path().join(".claude"));
        for var in [
            "CODEX_HOME",
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "PI_CODING_AGENT_DIR",
            "OPENCODE_DB",
            "HERMES_HOME",
            "FX_HOME",
            "GROK_HOME",
            "COWORK_SESSIONS_DIR",
            "AMP_THREADS_DIR",
            "CURSOR_DESKTOP_USER_DIR",
            "TXCRIPT_GROK_BOT_ROOT",
        ] {
            cmd.env(var, &empty);
        }
        let out = cmd
            .args(args)
            .args(["--from", "claude-code"])
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }
}

/// Four copies whose mtime order is unrelated to their folder names or
/// creation order, so neither can pick the newest by accident.
fn four_copies(env: &Env) {
    let p = env.projects();
    write_copy(&p, "m-dir", ID, 4, "COPY-OLDEST", 10);
    write_copy(&p, "a-dir", ID, 5, "COPY-NEWEST", 40);
    write_copy(&p, "z-dir", ID, 4, "COPY-MIDDLE", 30);
    write_copy(&p, "b-dir", ID, 4, "COPY-OLD", 20);
}

#[test]
fn view_loads_the_newest_copy_of_a_duplicated_id() {
    let env = Env::new();
    four_copies(&env);
    let (ok, out) = env.run(&["view", ID]);
    assert!(ok, "{out}");
    assert!(out.contains("COPY-NEWEST"), "{out}");
    assert!(
        !out.contains("COPY-OLD") && !out.contains("COPY-MIDDLE"),
        "{out}"
    );
}

#[test]
fn export_loads_the_newest_copy_of_a_duplicated_id() {
    let env = Env::new();
    four_copies(&env);
    let (ok, out) = env.run(&["export", ID]);
    assert!(ok, "{out}");
    assert!(out.contains("COPY-NEWEST"), "{out}");
}

#[test]
fn an_id_prefix_resolves_to_the_newest_copy() {
    let env = Env::new();
    four_copies(&env);
    let (ok, out) = env.run(&["view", "11111111-aaaa"]);
    assert!(ok, "{out}");
    assert!(out.contains("COPY-NEWEST"), "{out}");
}

#[test]
fn different_ids_sharing_a_prefix_stay_ambiguous() {
    let env = Env::new();
    four_copies(&env);
    write_copy(&env.projects(), "a-dir", OTHER_ID, 4, "OTHER", 50);
    let (ok, out) = env.run(&["view", "11111111"]);
    assert!(!ok, "{out}");
    assert!(out.contains("prefixes 2 session ids"), "{out}");
}

#[test]
fn copies_with_the_same_mtime_resolve_to_one_of_them_without_error() {
    let env = Env::new();
    let p = env.projects();
    write_copy(&p, "a-dir", ID, 4, "COPY-A", 10);
    write_copy(&p, "b-dir", ID, 4, "COPY-B", 10);
    let (ok, out) = env.run(&["view", ID]);
    assert!(ok, "{out}");
    assert!(out.contains("COPY-A") ^ out.contains("COPY-B"), "{out}");
}
