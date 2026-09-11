#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! `Store::delete` round-trips: save into a temp root, discover finds it,
//! delete, discover no longer does — per store, against real backends.

use chrono::{TimeZone, Utc};
use txcript::common::{Block, Message, Meta, Role};
use txcript::harness::{campfire, claude_code, codex, grok, grok_bot, pi};
use txcript::{Codec, Common, Store, Transcript};

#[cfg(feature = "opencode")]
use txcript::harness::cursor;

fn small_common(id: &str) -> Transcript<Common> {
    let meta = Meta {
        id: id.to_string(),
        timestamp: Utc
            .timestamp_opt(1_780_000_000, 0)
            .single()
            .unwrap_or_default(),
        cwd: Some("/tmp/delete-me".to_string()),
        git_branch: None,
        title: Some("deletable".to_string()),
        cli_version: None,
        model: None,
    };
    let message = Message {
        role: Role::User,
        content: vec![Block::Text {
            text: "hello".to_string(),
        }],
        timestamp: meta.timestamp,
        model: None,
        stop_reason: None,
        usage: None,
    };
    Transcript::new(meta, vec![message])
}

/// Save, then discover finds one; delete, then discover finds none — for any
/// file-backed store.
fn roundtrip<C, S>(store: &S)
where
    C: Codec,
    S: Store<H = C>,
{
    let native = C::from_common(&small_common("11111111-2222-4333-8444-555555555555"))
        .unwrap_or_else(|e| panic!("from_common: {e}"));
    let saved = store.save(&native).unwrap_or_else(|e| panic!("save: {e}"));

    let found = store.discover().unwrap_or_else(|e| panic!("discover: {e}"));
    assert_eq!(found.len(), 1, "one session after save");

    store
        .delete(&saved.reference)
        .unwrap_or_else(|e| panic!("delete: {e}"));

    let found = store
        .discover()
        .unwrap_or_else(|e| panic!("rediscover: {e}"));
    assert!(
        found.is_empty(),
        "no sessions after delete, got {}",
        found.len()
    );

    assert!(
        store.delete(&saved.reference).is_err(),
        "deleting twice errors"
    );
}

#[test]
fn claude_code_delete_roundtrip() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    roundtrip(&claude_code::ClaudeStore::new(dir.path().to_path_buf()));
}

#[test]
fn codex_delete_roundtrip() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    roundtrip(&codex::CodexStore::new(dir.path().to_path_buf()));
}

#[test]
fn pi_delete_roundtrip() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    roundtrip(&pi::PiStore::new(dir.path().to_path_buf()));
}

#[test]
fn campfire_delete_roundtrip() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    roundtrip(&campfire::CampfireStore::new(dir.path().to_path_buf()));
}

#[test]
fn grok_delete_removes_the_session_directory() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let store = grok::GrokStore::new(dir.path().to_path_buf());
    roundtrip(&store);
    // The project directory may remain, but no session directory does.
    let leftover_sessions: Vec<_> = walk_files(dir.path());
    assert!(
        leftover_sessions.is_empty(),
        "no files left behind: {leftover_sessions:?}"
    );
}

#[test]
fn grok_bot_delete_removes_the_session_directory() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let store = grok_bot::GrokBotStore::new(dir.path().to_path_buf());
    roundtrip(&store);
    let leftovers: Vec<_> = walk_files(dir.path());
    assert!(leftovers.is_empty(), "no files left behind: {leftovers:?}");
}

#[cfg(feature = "opencode")]
#[test]
fn cursor_delete_removes_the_whole_session_dir() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let store = cursor::CursorStore::new(dir.path().to_path_buf());
    roundtrip(&store);
    let leftovers: Vec<_> = walk_files(dir.path());
    assert!(
        leftovers.is_empty(),
        "meta.json/prompt_history.json must go with store.db: {leftovers:?}"
    );
}

#[cfg(feature = "opencode")]
#[test]
fn cursor_delete_refuses_a_non_session_path() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let store = cursor::CursorStore::new(dir.path().to_path_buf());
    assert!(store.delete(&dir.path().join("somewhere/else.db")).is_err());
}

fn walk_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        // A directory that vanished mid-walk has nothing to list.
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.push(path);
                }
            }
        }
    }
    files
}

/// The opencode "delete" archives in place: the row survives with
/// `time_archived` set, and discover stops listing it.
#[cfg(feature = "opencode")]
mod opencode_archive {
    use txcript::Store;
    use txcript::harness::opencode::OpenCodeStore;

    fn seed_db(path: &std::path::Path) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(path).unwrap_or_else(|e| panic!("open: {e}"));
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, title TEXT,
                 version TEXT, time_created INTEGER, time_archived INTEGER, model TEXT);
             CREATE TABLE message (id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
             CREATE TABLE part (id TEXT, message_id TEXT, session_id TEXT, data TEXT);
             INSERT INTO session (id, directory, title, version, time_created, model)
                 VALUES ('ses_1', '/tmp', 'a session', '1.0', 1780000000000, NULL);",
        )
        .unwrap_or_else(|e| panic!("seed: {e}"));
        conn
    }

    #[test]
    fn delete_archives_and_discover_skips() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let db = dir.path().join("opencode.sqlite3");
        let conn = seed_db(&db);
        let store = OpenCodeStore::new(db);

        assert_eq!(store.discover().map_or(0, |v| v.len()), 1);
        store
            .delete(&"ses_1".to_string())
            .unwrap_or_else(|e| panic!("delete: {e}"));
        assert_eq!(store.discover().map_or(99, |v| v.len()), 0);

        // The row survives, archived.
        let archived: Option<i64> = conn
            .query_row(
                "SELECT time_archived FROM session WHERE id = 'ses_1'",
                [],
                |r| r.get(0),
            )
            .unwrap_or_else(|e| panic!("row gone: {e}"));
        assert!(archived.is_some());

        // Archiving an already-archived (or unknown) session errors.
        assert!(store.delete(&"ses_1".to_string()).is_err());
        assert!(store.delete(&"nope".to_string()).is_err());
    }
}
