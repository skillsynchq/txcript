#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! Adversarial path handling: transcript-supplied ids must never escape a
//! store's root, deletes must refuse references outside it, and untrusted
//! numeric fields must not size allocations.

use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use txcript::common::{Block, Message, Meta, Role};
use txcript::harness::{
    amp, antigravity, campfire, claude_code, codex, cursor, grok, grok_bot, pi,
};
use txcript::{Codec, Common, Store, Transcript};

fn small_common(id: &str) -> Transcript<Common> {
    let meta = Meta {
        id: id.to_string(),
        timestamp: Utc
            .timestamp_opt(1_780_000_000, 0)
            .single()
            .unwrap_or_default(),
        cwd: Some("/tmp/adversarial".to_string()),
        git_branch: None,
        title: Some("hostile".to_string()),
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

/// Ids that, joined into a path, would land outside the store root.
const EVIL_IDS: &[&str] = &["../../pwned", "/tmp/abs-pwned", "a/b", "..", ""];

/// Saving under a hostile id must either error or stay inside `root`.
fn assert_save_confined<S>(store: &S, root: &Path)
where
    S: Store<Ref = PathBuf>,
    S::H: Codec,
{
    for evil in EVIL_IDS {
        // A codec that refuses the id outright is just as safe as a store
        // that does.
        let Ok(native) = <S::H as Codec>::from_common(&small_common(evil)) else {
            continue;
        };
        match store.save(&native) {
            Err(_) => {}
            Ok(saved) => {
                let canon = saved
                    .reference
                    .canonicalize()
                    .unwrap_or_else(|_| saved.reference.clone());
                let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
                assert!(
                    canon.starts_with(&root),
                    "id `{evil}` escaped the store root: {} is not under {}",
                    canon.display(),
                    root.display()
                );
            }
        }
    }
}

#[test]
fn hostile_ids_cannot_escape_any_file_backed_store() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    assert_save_confined(&claude_code::ClaudeStore::new(root.to_path_buf()), root);
    assert_save_confined(&codex::CodexStore::new(root.to_path_buf()), root);
    assert_save_confined(&pi::PiStore::new(root.to_path_buf()), root);
    assert_save_confined(&campfire::CampfireStore::new(root.to_path_buf()), root);
    assert_save_confined(&cursor::CursorStore::new(root.to_path_buf()), root);
    assert_save_confined(&grok::GrokStore::new(root.to_path_buf()), root);
    assert_save_confined(&grok_bot::GrokBotStore::new(root.to_path_buf()), root);
    assert_save_confined(&amp::AmpStore::new(root.to_path_buf()), root);
    assert_save_confined(
        &antigravity::AntigravityStore::new(root.to_path_buf()),
        root,
    );
}

#[test]
fn traversal_id_is_rejected_before_anything_is_written() {
    // The representative direct case: the id flows verbatim into the file
    // name, so save must refuse it, not resolve it.
    let dir = tempfile::tempdir().unwrap();
    let store = claude_code::ClaudeStore::new(dir.path().to_path_buf());
    let native = claude_code::ClaudeCode::from_common(&small_common("../../pwned")).unwrap();
    assert!(store.save(&native).is_err(), "traversal id must not save");
}

#[test]
fn cursor_delete_refuses_paths_outside_the_chats_root() {
    let chats = tempfile::tempdir().unwrap();
    let victim = tempfile::tempdir().unwrap();
    let store_db = victim.path().join("store.db");
    std::fs::write(&store_db, b"not a real store").unwrap();

    let store = cursor::CursorStore::new(chats.path().to_path_buf());
    assert!(
        store.delete(&store_db).is_err(),
        "a store.db outside the chats root must be refused"
    );
    assert!(victim.path().is_dir(), "the foreign directory must survive");
    assert!(store_db.is_file(), "the foreign file must survive");

    // A path that merely *ends* in store.db but doesn't exist proves nothing
    // about its parent being a session; refuse it too.
    let phantom = chats.path().join("w").join("id").join("store.db");
    assert!(store.delete(&phantom).is_err());
}

#[test]
fn grok_delete_refuses_paths_outside_the_sessions_root() {
    let sessions = tempfile::tempdir().unwrap();
    let victim = tempfile::tempdir().unwrap();
    // Give the victim a session's shape: the guard must hold even when the
    // directory looks like a real session.
    std::fs::write(victim.path().join("updates.jsonl"), b"").unwrap();

    let store = grok::GrokStore::new(sessions.path().to_path_buf());
    assert!(
        store.delete(&victim.path().to_path_buf()).is_err(),
        "a session-shaped directory outside the root must be refused"
    );
    assert!(victim.path().is_dir(), "the foreign directory must survive");

    // A directory with no conversation log isn't a session at all.
    let project = sessions.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    assert!(store.delete(&project).is_err());
    assert!(project.is_dir());
}

#[cfg(unix)]
#[test]
fn grok_delete_refuses_a_symlink_into_the_sessions_root() {
    let sessions = tempfile::tempdir().unwrap();
    let victim = tempfile::tempdir().unwrap();
    std::fs::write(victim.path().join("updates.jsonl"), b"").unwrap();

    // Plant a link at the exact depth delete expects a session at.
    let project = sessions.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let link = project.join("linked-session");
    std::os::unix::fs::symlink(victim.path(), &link).unwrap();

    let store = grok::GrokStore::new(sessions.path().to_path_buf());
    assert!(
        store.delete(&link).is_err(),
        "a symlink resolving outside the root must be refused"
    );
    assert!(
        victim.path().join("updates.jsonl").is_file(),
        "the symlink target must survive"
    );
}

#[test]
fn antigravity_delete_refuses_paths_outside_the_conversations_root() {
    let root = tempfile::tempdir().unwrap();
    let victim = tempfile::tempdir().unwrap();
    let foreign_db = victim.path().join("some-session.db");
    std::fs::write(&foreign_db, b"not a real db").unwrap();

    let store = antigravity::AntigravityStore::new(root.path().to_path_buf());
    assert!(
        store.delete(&foreign_db).is_err(),
        "a .db outside the conversations root must be refused"
    );
    assert!(foreign_db.is_file(), "the foreign file must survive");
}

#[cfg(unix)]
#[test]
fn discovery_survives_a_symlink_loop() {
    use txcript::harness::{claude_code, codex, pi};

    // Claude Code: a project dir containing a link back to the projects
    // root, next to one real session. Discovery must terminate and still
    // find the real session.
    let claude_root = tempfile::tempdir().unwrap();
    let project = claude_root.path().join("-work-repo");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("11111111-2222-4333-8444-555555555555.jsonl"),
        r#"{"type":"user","uuid":"u1","sessionId":"11111111-2222-4333-8444-555555555555","timestamp":"2026-01-02T03:04:05.000Z","cwd":"/work/repo","message":{"role":"user","content":"hello"}}"#,
    )
    .unwrap();
    std::os::unix::fs::symlink(claude_root.path(), project.join("loop")).unwrap();
    let store = claude_code::ClaudeStore::new(claude_root.path().to_path_buf());
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1, "the real session is still discovered");

    // Codex and pi: the same ancestor link must not hang their scanners.
    let codex_root = tempfile::tempdir().unwrap();
    let inner = codex_root.path().join("2026");
    std::fs::create_dir_all(&inner).unwrap();
    std::os::unix::fs::symlink(codex_root.path(), inner.join("loop")).unwrap();
    assert!(
        codex::CodexStore::new(codex_root.path().to_path_buf())
            .discover()
            .unwrap()
            .is_empty()
    );

    let pi_root = tempfile::tempdir().unwrap();
    let inner = pi_root.path().join("--proj--");
    std::fs::create_dir_all(&inner).unwrap();
    std::os::unix::fs::symlink(pi_root.path(), inner.join("loop")).unwrap();
    assert!(
        pi::PiStore::new(pi_root.path().to_path_buf())
            .discover()
            .unwrap()
            .is_empty()
    );
}
