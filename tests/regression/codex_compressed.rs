#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

//! Regression for the rollouts Codex compresses.

use txcript::Store;
use txcript::harness::codex;

/// Issue #84. Codex began compressing a rollout it hadn't written to for a
/// week, in place, to `rollout-….jsonl.zst` (seen with Codex 0.162.1).
/// Discovery matched `.jsonl` alone, so on one machine 781 of 814 sessions
/// stopped listing in one pass, with every file still on disk. A compressed
/// rollout must list and load exactly as its plain form does.
#[test]
fn compressed_rollouts_list_and_load_like_plain_ones() {
    let rollout = [
        r#"{"timestamp":"2026-10-01T00:00:00.000Z","type":"session_meta","payload":{"id":"sess-cold","timestamp":"2026-10-01T00:00:00.000Z","cwd":"/repo","cli_version":"0.162.1"}}"#,
        r#"{"timestamp":"2026-10-01T00:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#,
        r#"{"timestamp":"2026-10-01T00:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}}"#,
    ]
    .join("\n")
        + "\n";
    // Upstream writes one zstd frame at level 3.
    let compressed = zstd::stream::encode_all(rollout.as_bytes(), 3).unwrap();

    let store_holding = |name: &str, bytes: &[u8]| {
        let dir = tempfile::tempdir().unwrap();
        let day = dir.path().join("2026").join("10").join("01");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join(name), bytes).unwrap();
        dir
    };
    let name = "rollout-2026-10-01T00-00-00-sess-cold.jsonl";
    let plain_dir = store_holding(name, rollout.as_bytes());
    let compressed_dir = store_holding(&format!("{name}.zst"), &compressed);
    let plain = codex::CodexStore::new(plain_dir.path());
    let store = codex::CodexStore::new(compressed_dir.path());

    let expected = plain.discover().unwrap();
    let found = store.discover().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].meta, expected[0].meta);
    assert_eq!(found[0].meta.id, "sess-cold");

    let loaded = store.load(&found[0].reference).unwrap();
    assert_eq!(loaded.body.len(), 3);
    assert_eq!(loaded, plain.load(&expected[0].reference).unwrap());
}
