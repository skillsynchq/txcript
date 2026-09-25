#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! Behavior of `txcript::search`: cold/hot parity, origin labeling and
//! filtering, ranking, replacement, and span math.

use chrono::{TimeZone, Utc};
use txcript::common::{Block, Message, Meta, Role, Tool, ToolOutput};
use txcript::search::{DocKey, Hit, Index, Origin, Query, search};
use txcript::{Common, HarnessId, Span, Transcript};

fn meta(id: &str, secs: i64) -> Meta {
    Meta {
        id: id.to_string(),
        timestamp: Utc
            .timestamp_opt(1_780_000_000 + secs, 0)
            .single()
            .unwrap_or_default(),
        cwd: Some("/work/replay".to_string()),
        git_branch: Some("relay-protocol-v6".to_string()),
        title: Some(format!("session {id}")),
        cli_version: None,
        model: None,
    }
}

fn message(role: Role, blocks: Vec<Block>) -> Message {
    Message {
        role,
        content: blocks,
        timestamp: Utc
            .timestamp_opt(1_780_000_000, 0)
            .single()
            .unwrap_or_default(),
        model: None,
        stop_reason: None,
        usage: None,
    }
}

fn text(t: &str) -> Block {
    Block::Text {
        text: t.to_string(),
    }
}

/// A transcript exercising every origin.
fn rich_transcript(id: &str, secs: i64) -> Transcript<Common> {
    Transcript::new(
        meta(id, secs),
        vec![
            message(
                Role::User,
                vec![text("please fix the flaky websocket test")],
            ),
            message(
                Role::Assistant,
                vec![
                    Block::Thinking {
                        text: "the reconnect timer races the handshake".to_string(),
                        signature: None,
                        encrypted: None,
                    },
                    text("looking at the reconnect logic now"),
                    Block::ToolUse {
                        id: "t1".to_string(),
                        tool: Tool::Bash {
                            command: "cargo test websocket -- --nocapture".to_string(),
                            workdir: None,
                            timeout_ms: None,
                            description: None,
                            run_in_background: false,
                        },
                    },
                ],
            ),
            message(
                Role::User,
                vec![Block::ToolResult {
                    tool_use_id: "t1".to_string(),
                    content: ToolOutput::Text("test websocket_reconnect ... FAILED".to_string()),
                    is_error: true,
                }],
            ),
        ],
    )
}

fn key(harness: HarnessId, id: &str) -> DocKey {
    DocKey {
        harness,
        id: id.to_string(),
        source: None,
    }
}

/// Two sessions sharing a `(harness, id)` — Claude Code writes the same
/// sessionId into more than one project directory — must both stay indexed
/// when their `source` differs, while a same-source re-insert still replaces.
#[test]
fn sessions_sharing_an_id_are_distinct_documents_by_source() {
    let sourced = |source: &str| DocKey {
        harness: HarnessId::ClaudeCode,
        id: "dup-1".to_string(),
        source: Some(source.to_string()),
    };
    let transcript = |body: &str| {
        Transcript::new(
            meta("dup-1", 0),
            vec![message(Role::User, vec![text(body)])],
        )
    };

    let mut index = Index::new();
    index.insert(
        sourced("/projects/a/dup-1.jsonl"),
        &transcript("alpha copy"),
    );
    index.insert(sourced("/projects/b/dup-1.jsonl"), &transcript("beta copy"));
    assert_eq!(index.len(), 2, "different sources are different documents");

    let hits = index.query(&Query::fuzzy("copy"));
    assert_eq!(hits.len(), 2, "both copies are searchable");

    index.insert(
        sourced("/projects/b/dup-1.jsonl"),
        &transcript("beta copy revised"),
    );
    assert_eq!(index.len(), 2, "same source replaces, not duplicates");
}

#[test]
fn cold_and_hot_agree() {
    let t = rich_transcript("a", 0);
    let q = Query::fuzzy("reconnect");

    let cold = search(&t, &q);
    let mut index = Index::new();
    index.insert(key(HarnessId::ClaudeCode, "a"), &t);
    let hot = index.query(&q);

    assert_eq!(hot.len(), 1);
    assert_eq!(hot[0].hits, cold);
    assert!(!cold.is_empty());
}

#[test]
fn hits_are_labeled_by_origin() {
    let t = rich_transcript("a", 0);
    let origin_of = |q: &Query| -> Vec<Origin> { search(&t, q).iter().map(|h| h.origin).collect() };

    assert_eq!(
        origin_of(&Query::substring("flaky websocket")),
        vec![Origin::User]
    );
    assert_eq!(
        origin_of(&Query::substring("races the handshake")),
        vec![Origin::Thinking]
    );
    assert_eq!(
        origin_of(&Query::substring("looking at the reconnect")),
        vec![Origin::Assistant]
    );
    assert_eq!(
        origin_of(&Query::substring("--nocapture")),
        vec![Origin::ToolUse]
    );
}

#[test]
fn tool_result_needs_opt_in() {
    let t = rich_transcript("a", 0);

    // Default origins exclude tool output.
    assert!(search(&t, &Query::substring("websocket_reconnect ... FAILED")).is_empty());

    let mut q = Query::substring("websocket_reconnect ... FAILED");
    q.origins = Origin::ALL.to_vec();
    let hits = search(&t, &q);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].origin, Origin::ToolResult);
}

#[test]
fn meta_fields_are_searchable() {
    let t = rich_transcript("a", 0);
    let hits = search(&t, &Query::substring("relay-protocol-v6"));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].origin, Origin::Meta);
    // Meta lines come from the session header, not a message: an empty span.
    assert_eq!((hits[0].span.clone(), hits[0].block), (Span(0..0), 0));
}

#[test]
fn spans_cover_the_substring_in_characters() {
    let t = Transcript::new(
        meta("a", 0),
        vec![message(Role::User, vec![text("naïve needle here")])],
    );
    let hits = search(&t, &Query::substring("needle"));
    assert_eq!(hits.len(), 1);
    // "naïve " is 6 characters; the span is char-indexed, not byte-indexed.
    assert_eq!(hits[0].highlights, vec![6..12]);
    let chars: Vec<char> = hits[0].line.chars().collect();
    let matched: String = chars[6..12].iter().collect();
    assert_eq!(matched, "needle");
}

#[test]
fn smart_case_is_insensitive_until_uppercase() {
    let t = Transcript::new(
        meta("a", 0),
        vec![message(Role::User, vec![text("read the README first")])],
    );
    assert!(!search(&t, &Query::substring("readme")).is_empty());
    // Uppercase in the pattern demands a case match: no "REadme" text exists.
    assert!(search(&t, &Query::substring("REadme")).is_empty());
}

#[test]
fn fuzzy_uses_fzf_pattern_syntax() {
    let t = rich_transcript("a", 0);
    // Two atoms, both must match somewhere on the line.
    assert!(!search(&t, &Query::fuzzy("cargo websocket")).is_empty());
    // Negation atom rejects lines containing the term.
    let hits = search(&t, &Query::fuzzy("reconnect !handshake"));
    assert!(hits.iter().all(|h| !h.line.contains("handshake")));
    assert!(!hits.is_empty());
}

#[test]
fn empty_pattern_lists_documents_newest_first() {
    let mut index = Index::new();
    index.insert(
        key(HarnessId::ClaudeCode, "old"),
        &rich_transcript("old", 0),
    );
    index.insert(key(HarnessId::Codex, "new"), &rich_transcript("new", 1000));

    let matches = index.query(&Query::fuzzy("  "));
    let ids: Vec<&str> = matches.iter().map(|m| m.key.id.as_str()).collect();
    assert_eq!(ids, vec!["new", "old"]);
    assert!(matches.iter().all(|m| m.hits.is_empty() && m.score == 0));
}

#[test]
fn harness_filter_scopes_the_query() {
    let mut index = Index::new();
    index.insert(key(HarnessId::ClaudeCode, "a"), &rich_transcript("a", 0));
    index.insert(key(HarnessId::Codex, "b"), &rich_transcript("b", 0));

    let mut q = Query::fuzzy("reconnect");
    q.harnesses = Some(vec![HarnessId::Codex]);
    let matches = index.query(&q);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].key.harness, HarnessId::Codex);
}

#[test]
fn insert_replaces_and_remove_keeps_the_map_consistent() {
    let mut index = Index::new();
    index.insert(key(HarnessId::ClaudeCode, "a"), &rich_transcript("a", 0));
    index.insert(key(HarnessId::Codex, "b"), &rich_transcript("b", 0));
    index.insert(key(HarnessId::Pi, "c"), &rich_transcript("c", 0));

    // Replace: same key, new content; the old content must be gone.
    let replacement = Transcript::new(
        meta("a", 0),
        vec![message(Role::User, vec![text("entirely different topic")])],
    );
    index.insert(key(HarnessId::ClaudeCode, "a"), &replacement);
    assert_eq!(index.len(), 3);
    let mut q = Query::substring("reconnect");
    assert_eq!(index.query(&q).len(), 2);

    // Remove the first doc; the swap-moved doc must still be reachable.
    assert!(index.remove(&key(HarnessId::ClaudeCode, "a")));
    assert!(!index.remove(&key(HarnessId::ClaudeCode, "a")));
    assert_eq!(index.len(), 2);
    q.pattern = "different topic".to_string();
    assert!(index.query(&q).is_empty());
    assert_eq!(index.query(&Query::substring("reconnect")).len(), 2);
}

#[test]
fn limit_caps_documents_hot_and_hits_cold() {
    let mut index = Index::new();
    for (i, id) in ["a", "b", "c"].iter().enumerate() {
        index.insert(
            key(HarnessId::ClaudeCode, id),
            &rich_transcript(id, i64::try_from(i).unwrap_or(0)),
        );
    }
    let mut q = Query::fuzzy("reconnect");
    q.limit = Some(2);
    assert_eq!(index.query(&q).len(), 2);

    let mut q = Query::substring("e");
    q.limit = Some(1);
    assert_eq!(search(&rich_transcript("a", 0), &q).len(), 1);
}

#[test]
fn ranking_prefers_the_better_match() {
    let tight = Transcript::new(
        meta("tight", 0),
        vec![message(Role::User, vec![text("relay protocol")])],
    );
    let loose = Transcript::new(
        meta("loose", 1000),
        vec![message(
            Role::User,
            vec![text("the r-e-l-a-y thing uses a p.r.o.t.o.c.o.l of sorts")],
        )],
    );
    let mut index = Index::new();
    index.insert(key(HarnessId::ClaudeCode, "tight"), &tight);
    index.insert(key(HarnessId::ClaudeCode, "loose"), &loose);

    let matches = index.query(&Query::fuzzy("relay protocol"));
    assert_eq!(matches.len(), 2, "both fuzzy-match");
    // The contiguous match must outrank the scattered one despite being older.
    assert_eq!(matches[0].key.id, "tight");
    assert!(matches[0].score > matches[1].score);
}

/// Literal substring occurrences outrank gapped fuzzy alignments and highlight
/// as one contiguous span.
#[test]
fn exact_occurrence_outranks_gappy_fuzzy() {
    let gappy = Transcript::new(
        meta("gappy", 1000),
        vec![message(
            Role::Assistant,
            vec![text("construct the proper sequence")],
        )],
    );
    let exact = Transcript::new(
        meta("exact", 0),
        vec![message(
            Role::Assistant,
            vec![text("one consequence of the change")],
        )],
    );
    let mut index = Index::new();
    index.insert(key(HarnessId::ClaudeCode, "gappy"), &gappy);
    index.insert(key(HarnessId::ClaudeCode, "exact"), &exact);

    let matches = index.query(&Query::fuzzy("consequence"));
    assert_eq!(matches.len(), 2, "both fuzzy-match");
    assert_eq!(matches[0].key.id, "exact");
    assert!(matches[0].score > matches[1].score);
    assert_eq!(
        matches[0].hits[0].highlights,
        vec![4..15],
        "highlight covers the literal occurrence"
    );
}

/// Substring mode is one literal needle: spaces are part of it, and matches
/// in uppercase text are found case-insensitively.
#[test]
fn substring_is_literal_and_folds_case() {
    let t = Transcript::new(
        meta("a", 0),
        vec![message(Role::User, vec![text("RUN --NOCAPTURE NOW")])],
    );
    assert_eq!(search(&t, &Query::substring("--nocapture now")).len(), 1);
    // A literal with a space only matches the contiguous text.
    assert!(search(&t, &Query::substring("run now")).is_empty());
}

#[test]
fn query_deserializes_from_pattern_only() {
    let q: Query = serde_json::from_str(r#"{"pattern":"relay"}"#).unwrap_or_default();
    assert_eq!(q, Query::fuzzy("relay"));
}

#[test]
fn hit_serializes_for_the_wire() {
    let t = rich_transcript("a", 0);
    let hits = search(&t, &Query::substring("flaky"));
    let json = serde_json::to_string(&hits).unwrap_or_default();
    let back: Vec<Hit> = serde_json::from_str(&json).unwrap_or_default();
    assert_eq!(back, hits);
}

#[test]
fn hit_span_resolves_to_the_matched_message() {
    let t = rich_transcript("a", 0);
    let hits = search(&t, &Query::substring("flaky"));
    assert_eq!(hits.len(), 1);
    let fragment = t.fragment(&hits[0].span).expect("span is in bounds");
    assert_eq!(fragment.len(), 1);
    // The fragment borrows the message whose block produced the matched line.
    match &fragment[0].content[hits[0].block] {
        Block::Text { text } => assert!(text.contains(&hits[0].line)),
        other => panic!("expected the matched text block, got {other:?}"),
    }
}

#[test]
fn meta_hit_resolves_to_an_empty_fragment() {
    let t = rich_transcript("a", 0);
    let hits = search(&t, &Query::substring("relay-protocol-v6"));
    let fragment = t.fragment(&hits[0].span).expect("empty span is in bounds");
    assert!(fragment.is_empty());
}

#[test]
fn fragment_covers_ranges_and_rejects_out_of_bounds() {
    let t = rich_transcript("a", 0);
    let all = t.fragment(&Span(0..t.body.len())).expect("whole session");
    assert_eq!(all, t.body.as_slice());
    assert_eq!(t.fragment(&Span(0..t.body.len() + 1)), None);
}

// ── multi-criteria filter tests ─────────────────────────────────────────────

/// Build a two-document index: "alpha" on branch `main`, "beta" on `dev`.
/// The common search pattern "needle" appears in both.
fn two_doc_index() -> Index {
    let make = |id: &str, branch: &str, model: Option<&str>, secs: i64| {
        let mut m = meta(id, secs);
        m.git_branch = Some(branch.to_string());
        m.model = model.map(str::to_string);
        Transcript::new(m, vec![message(Role::User, vec![text("needle content")])])
    };
    let mut index = Index::new();
    index.insert(
        key(HarnessId::ClaudeCode, "alpha"),
        &make("alpha", "main", Some("claude-3-5-sonnet"), 0),
    );
    index.insert(
        key(HarnessId::ClaudeCode, "beta"),
        &make("beta", "dev", Some("gpt-4o"), 3600),
    );
    index
}

#[test]
fn query_filter_git_branch_exact_match() {
    let index = two_doc_index();

    let mut q = Query::substring("needle");
    q.git_branch = Some("main".to_string());
    let hits = index.query(&q);
    assert_eq!(hits.len(), 1, "only the 'main' branch session");
    assert_eq!(hits[0].key.id, "alpha");

    // Unknown branch → no hits.
    q.git_branch = Some("nonexistent".to_string());
    assert!(index.query(&q).is_empty());

    // No filter → both sessions.
    q.git_branch = None;
    assert_eq!(index.query(&q).len(), 2);
}

#[test]
fn query_filter_model_case_insensitive_substring() {
    let index = two_doc_index();

    // "sonnet" is a substring of "claude-3-5-sonnet".
    let mut q = Query::substring("needle");
    q.model = Some("SONNET".to_string());
    let hits = index.query(&q);
    assert_eq!(hits.len(), 1, "only the claude sonnet session");
    assert_eq!(hits[0].key.id, "alpha");

    // "gpt" matches the beta session.
    q.model = Some("gpt".to_string());
    let hits = index.query(&q);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].key.id, "beta");

    // No filter → both.
    q.model = None;
    assert_eq!(index.query(&q).len(), 2);
}

#[test]
fn query_filter_since_and_until() {
    let index = two_doc_index();

    // Base timestamp is 1_780_000_000; beta is +3600 s later.
    let base = Utc.timestamp_opt(1_780_000_000, 0).single().unwrap();
    let after_alpha = Utc.timestamp_opt(1_780_001_000, 0).single().unwrap();
    let after_beta = Utc.timestamp_opt(1_780_010_000, 0).single().unwrap();

    let mut q = Query::substring("needle");

    // since=after_alpha only includes beta.
    q.since = Some(after_alpha);
    let hits = index.query(&q);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].key.id, "beta");

    // until=base only includes alpha (beta is strictly after).
    q.since = None;
    q.until = Some(base);
    let hits = index.query(&q);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].key.id, "alpha");

    // since and until together can produce zero results.
    q.since = Some(after_beta);
    q.until = Some(after_beta);
    assert!(index.query(&q).is_empty());

    // No filter → both.
    q.since = None;
    q.until = None;
    assert_eq!(index.query(&q).len(), 2);
}

#[test]
fn query_filter_cwd_path_prefix() {
    // The meta helper sets cwd = "/work/replay"; make a second with a sibling dir.
    let t_a = Transcript::new(
        meta("a", 0),
        vec![message(Role::User, vec![text("needle content")])],
    );
    let mut m_b = meta("b", 0);
    m_b.cwd = Some("/work/other".to_string());
    let t_b = Transcript::new(m_b, vec![message(Role::User, vec![text("needle content")])]);

    let mut index = Index::new();
    index.insert(key(HarnessId::ClaudeCode, "a"), &t_a);
    index.insert(key(HarnessId::ClaudeCode, "b"), &t_b);

    // "/work/replay" prefix: only session a.
    let mut q = Query::substring("needle");
    q.cwd = Some("/work/replay".to_string());
    let hits = index.query(&q);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].key.id, "a");

    // "/work" is a parent: both sessions are under it.
    q.cwd = Some("/work".to_string());
    assert_eq!(index.query(&q).len(), 2);

    // No filter → both.
    q.cwd = None;
    assert_eq!(index.query(&q).len(), 2);
}
