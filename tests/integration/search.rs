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
        lineage: None,
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

// ── tool output ─────────────────────────────────────────────────────────────
//
// Tool output is indexed like every other origin but searched only when a
// query selects `Origin::ToolResult` — what `txcript query --tool-output` and
// the MCP `tool_output` argument do. These pin that the wider scope finds
// tool output with the same literal, case-insensitive semantics as message
// text, and changes nothing else.

/// The query `txcript query` and the MCP search tool build (the CLI crate's
/// `user_query`): one literal needle, case-insensitive, tool output searched
/// only when asked.
fn cli_query(pattern: &str, tool_output: bool) -> Query {
    let mut q = Query::substring(pattern);
    q.case = txcript::search::Case::Insensitive;
    if tool_output {
        q.origins = Origin::ALL.to_vec();
    }
    q
}

fn bash(id: &str, command: &str) -> Block {
    Block::ToolUse {
        id: id.to_string(),
        tool: Tool::Bash {
            command: command.to_string(),
            workdir: None,
            timeout_ms: None,
            description: None,
            run_in_background: false,
        },
    }
}

fn output(id: &str, out: &str) -> Block {
    Block::ToolResult {
        tool_use_id: id.to_string(),
        content: ToolOutput::Text(out.to_string()),
        is_error: false,
    }
}

/// A session with `needle` in exactly one place: the user's words, the
/// assistant's, or a tool's output.
fn session_with(id: &str, secs: i64, place: Origin, needle: &str) -> Transcript<Common> {
    let in_place = |origin: Origin, filler: &str| {
        if origin == place {
            format!("{filler} {needle} {filler}")
        } else {
            filler.to_string()
        }
    };
    Transcript::new(
        meta(id, secs),
        vec![
            message(
                Role::User,
                vec![text(&in_place(Origin::User, "please look"))],
            ),
            message(
                Role::Assistant,
                vec![
                    text(&in_place(Origin::Assistant, "checking")),
                    bash("t1", "make check"),
                ],
            ),
            message(
                Role::User,
                vec![output("t1", &in_place(Origin::ToolResult, "ok"))],
            ),
        ],
    )
}

#[test]
fn message_text_matches_are_the_same_with_tool_output() {
    for place in [Origin::User, Origin::Assistant] {
        let t = session_with("a", 0, place, "quokka-7");
        let narrow = search(&t, &cli_query("quokka-7", false));
        let wide = search(&t, &cli_query("quokka-7", true));
        assert_eq!(narrow.len(), 1, "{place:?}");
        assert_eq!(narrow[0].origin, place);
        assert_eq!(wide, narrow, "{place:?}");
    }
    // The other non-tool-output origins, from the shared fixture.
    let t = rich_transcript("a", 0);
    for pattern in [
        "flaky websocket",
        "races the handshake",
        "--nocapture",
        "relay-protocol-v6",
    ] {
        assert_eq!(
            search(&t, &cli_query(pattern, true)),
            search(&t, &cli_query(pattern, false)),
            "{pattern}"
        );
    }
}

#[test]
fn text_only_in_tool_output_needs_tool_output() {
    let t = session_with("a", 0, Origin::ToolResult, "quokka-7");
    assert_eq!(search(&t, &cli_query("quokka-7", false)), Vec::new());

    let hits = search(&t, &cli_query("quokka-7", true));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].origin, Origin::ToolResult);
    assert_eq!(hits[0].line, "ok quokka-7 ok");
    // Hot and cold agree under the wider scope too.
    let mut index = Index::new();
    index.insert(key(HarnessId::ClaudeCode, "a"), &t);
    let hot = index.query(&cli_query("quokka-7", true));
    assert_eq!(hot.len(), 1);
    assert_eq!(hot[0].hits, hits);
}

#[test]
fn only_the_tool_output_that_matches_is_hit() {
    let t = Transcript::new(
        meta("a", 0),
        vec![
            message(
                Role::Assistant,
                vec![
                    bash("t1", "ls"),
                    bash("t2", "cat Cargo.toml"),
                    bash("t3", "git status"),
                ],
            ),
            message(
                Role::User,
                vec![
                    output("t1", "Cargo.toml\nsrc"),
                    output("t2", "[package]\nname = \"wombat-engine\""),
                    output("t3", "nothing to commit"),
                ],
            ),
        ],
    );
    let hits = search(&t, &cli_query("wombat-engine", true));
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].span.clone(), hits[0].block), (Span(1..2), 1));
    let fragment = t.fragment(&hits[0].span).unwrap();
    match &fragment[0].content[hits[0].block] {
        Block::ToolResult {
            tool_use_id,
            content: ToolOutput::Text(out),
            ..
        } => {
            assert_eq!(tool_use_id, "t2");
            assert!(out.contains(&hits[0].line));
        }
        other => panic!("expected t2's output, got {other:?}"),
    }
}

#[test]
fn tool_output_matches_inside_surrounding_text() {
    let t = Transcript::new(
        meta("a", 0),
        vec![message(
            Role::User,
            vec![output(
                "t1",
                "running 3 tests\n  at parser::step (src/lexer.rs:42)   \ntest result: FAILED",
            )],
        )],
    );
    let hits = search(&t, &cli_query("lexer.rs:42", true));
    assert_eq!(hits.len(), 1);
    // The matched line, trailing whitespace trimmed, as for message text.
    assert_eq!(hits[0].line, "  at parser::step (src/lexer.rs:42)");
    assert_eq!(hits[0].highlights, vec![23..34]);
}

#[test]
fn sessions_matching_by_tool_output_and_by_text_are_all_found() {
    let mut index = Index::new();
    let both = Transcript::new(
        meta("both", 300),
        vec![
            message(Role::User, vec![text("why does kestrel-9 fail")]),
            message(Role::User, vec![output("t1", "error: kestrel-9 timed out")]),
        ],
    );
    index.insert(
        key(HarnessId::ClaudeCode, "text"),
        &session_with("text", 0, Origin::User, "kestrel-9"),
    );
    index.insert(
        key(HarnessId::Codex, "tool"),
        &session_with("tool", 200, Origin::ToolResult, "kestrel-9"),
    );
    index.insert(
        key(HarnessId::ClaudeCode, "asst"),
        &session_with("asst", 100, Origin::Assistant, "kestrel-9"),
    );
    index.insert(key(HarnessId::Pi, "both"), &both);
    index.insert(
        key(HarnessId::ClaudeCode, "none"),
        &session_with("none", 400, Origin::User, "falcon"),
    );
    let ids =
        |q: &Query| -> Vec<String> { index.query(q).iter().map(|m| m.key.id.clone()).collect() };

    // A literal needle scores the same wherever it sits, so ties rank
    // newest-first under either scope.
    assert_eq!(
        ids(&cli_query("kestrel-9", false)),
        ["both", "asst", "text"]
    );
    assert_eq!(
        ids(&cli_query("kestrel-9", true)),
        ["both", "tool", "asst", "text"]
    );
    // A session matching both ways shows both lines, in transcript order.
    let wide = index.query(&cli_query("kestrel-9", true));
    let origins: Vec<Origin> = wide[0].hits.iter().map(|h| h.origin).collect();
    assert_eq!(origins, [Origin::User, Origin::ToolResult]);
}

#[test]
fn absent_text_and_empty_patterns_are_unchanged_by_tool_output() {
    let mut index = Index::new();
    index.insert(
        key(HarnessId::ClaudeCode, "old"),
        &session_with("old", 0, Origin::ToolResult, "quokka-7"),
    );
    index.insert(
        key(HarnessId::Codex, "new"),
        &session_with("new", 1000, Origin::User, "quokka-7"),
    );

    for tool_output in [false, true] {
        let q = cli_query("not-in-any-session", tool_output);
        assert_eq!(index.query(&q).len(), 0, "tool_output={tool_output}");
        assert_eq!(
            search(&session_with("a", 0, Origin::ToolResult, "quokka-7"), &q),
            Vec::new()
        );

        // An empty pattern lists every document, newest first, with no hits;
        // the one-shot search matches nothing.
        let q = cli_query("   ", tool_output);
        let listed = index.query(&q);
        let ids: Vec<&str> = listed.iter().map(|m| m.key.id.as_str()).collect();
        assert_eq!(ids, ["new", "old"]);
        assert!(listed.iter().all(|m| m.hits.is_empty() && m.score == 0));
        assert_eq!(
            search(&session_with("a", 0, Origin::ToolResult, "quokka-7"), &q),
            Vec::new()
        );
    }
}

/// Case folding and Unicode in tool output follow the rules for message
/// text, whatever those are: one per-character case fold, no diacritic
/// folding in substring mode, character-indexed highlights.
#[test]
fn tool_output_case_and_unicode_follow_message_text() {
    let line = "Naïve ÉCHEC: 日本語 Überprüfung failed";
    let as_text = Transcript::new(meta("a", 0), vec![message(Role::User, vec![text(line)])]);
    let as_output = Transcript::new(
        meta("b", 0),
        vec![message(Role::User, vec![output("t1", line)])],
    );
    for (pattern, found) in [
        ("naïve échec", true),
        ("NAÏVE", true),
        ("überprüfung", true),
        ("日本語", true),
        // Substring mode does not fold diacritics, in any origin.
        ("naive", false),
        ("echec", false),
        ("ÜBERPRUFUNG", false),
    ] {
        let from_text = search(&as_text, &cli_query(pattern, true));
        let from_output = search(&as_output, &cli_query(pattern, true));
        assert_eq!(from_text.len(), usize::from(found), "{pattern}");
        assert_eq!(from_output.len(), from_text.len(), "{pattern}");
        for (t, o) in from_text.iter().zip(&from_output) {
            assert_eq!((&t.line, &t.highlights), (&o.line, &o.highlights));
            assert_eq!(o.origin, Origin::ToolResult);
        }
    }
    let hits = search(&as_output, &cli_query("日本語 überprüfung", true));
    assert_eq!(hits.len(), 1);
    // "Naïve ÉCHEC: " is 13 characters; highlights count characters.
    assert_eq!(hits[0].highlights, vec![13..28]);
}

/// Tool outputs that are empty, blank, JSON rather than text, or answer no
/// known tool call neither panic nor produce stray hits.
#[test]
fn odd_tool_outputs_are_searched_safely() {
    let result = |id: &str, content: ToolOutput, is_error: bool| Block::ToolResult {
        tool_use_id: id.to_string(),
        content,
        is_error,
    };
    let t = Transcript::new(
        meta("a", 0),
        vec![
            message(Role::Assistant, vec![bash("t1", "true")]),
            message(
                Role::User,
                vec![
                    result("t1", ToolOutput::Text(String::new()), false),
                    result("t2", ToolOutput::Text(" \n\t\n".to_string()), true),
                    result("t3", ToolOutput::Json(serde_json::Value::Null), false),
                    result(
                        "orphan",
                        ToolOutput::Json(serde_json::json!({"rows": [{"name": "ibis-3"}]})),
                        false,
                    ),
                ],
            ),
            // A tool result where a harness put it on the assistant turn.
            message(
                Role::Assistant,
                vec![result(
                    "t4",
                    ToolOutput::Text("ibis-3 done".to_string()),
                    false,
                )],
            ),
        ],
    );
    let hits = search(&t, &cli_query("ibis-3", true));
    let found: Vec<(usize, usize, Origin)> = hits
        .iter()
        .map(|h| (h.span.0.start, h.block, h.origin))
        .collect();
    assert_eq!(
        found,
        [(1, 3, Origin::ToolResult), (2, 0, Origin::ToolResult)]
    );
    assert_eq!(search(&t, &cli_query("ibis-3", false)), Vec::new());

    let mut index = Index::new();
    index.insert(key(HarnessId::ClaudeCode, "a"), &t);
    assert_eq!(index.query(&cli_query("ibis-3", true))[0].hits, hits);
}

/// Several dozen generated sessions with large, uneven tool outputs —
/// enough lines that `Index::query` shards scoring across threads and
/// enough matches that it shards highlighting too. A third of the sessions
/// mention the needle only in tool output, a third only in user text.
#[test]
fn many_sessions_with_large_tool_outputs() {
    const SESSIONS: u64 = 96;
    // Fixed-seed xorshift: output sizes and needle positions vary, the same
    // way on every run.
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move |bound: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % bound
    };
    let mut index = Index::new();
    for i in 0..SESSIONS {
        let id = format!("s{i:03}");
        let lines = 600 + next(200);
        let at = next(lines);
        let out: String = (0..lines)
            .map(|l| {
                if i.is_multiple_of(3) && l == at {
                    format!("warning: pelican-42 deprecated (line {l})\n")
                } else {
                    format!("   Compiling crate-{l} v0.{i}.0\n")
                }
            })
            .collect();
        let ask = if i % 3 == 1 {
            "is pelican-42 still used?"
        } else {
            "build it"
        };
        let t = Transcript::new(
            meta(&id, i64::try_from(i).unwrap()),
            vec![
                message(Role::User, vec![text(ask)]),
                message(Role::Assistant, vec![bash("t1", "cargo build")]),
                message(Role::User, vec![output("t1", &out)]),
            ],
        );
        index.insert(key(HarnessId::ClaudeCode, &id), &t);
    }
    assert!(index.lines() >= 64 * 1024, "{} lines", index.lines());

    let ids =
        |q: &Query| -> Vec<String> { index.query(q).iter().map(|m| m.key.id.clone()).collect() };
    // Equal literal scores: newest (highest-numbered) first.
    let expect = |keep: &dyn Fn(u64) -> bool| -> Vec<String> {
        (0..SESSIONS)
            .rev()
            .filter(|&i| keep(i))
            .map(|i| format!("s{i:03}"))
            .collect()
    };

    let narrow = cli_query("pelican-42", false);
    assert_eq!(ids(&narrow), expect(&|i| i % 3 == 1));

    let wide = cli_query("PELICAN-42", true);
    let all = ids(&wide);
    assert_eq!(all, expect(&|i| i % 3 != 2));
    assert_eq!(ids(&wide), all, "repeat queries agree");
    for m in index.query(&wide) {
        let n: u64 = m.key.id[1..].parse().unwrap();
        let origin = if n.is_multiple_of(3) {
            Origin::ToolResult
        } else {
            Origin::User
        };
        assert_eq!(m.hits.len(), 1, "{}", m.key.id);
        assert_eq!(m.hits[0].origin, origin, "{}", m.key.id);
    }

    // The CLI's 20-document cap keeps the top of the same ranking.
    let mut capped = wide.clone();
    capped.limit = Some(20);
    assert_eq!(ids(&capped), all[..20]);
}
