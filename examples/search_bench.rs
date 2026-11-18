//! Benchmark `txcript::search` against the real sessions on this machine.
//!
//! ```text
//! cargo run --release --example search_bench            # index everything, time queries
//! cargo run --release --example search_bench -- relay   # also print top hits for a pattern
//! ```
//!
//! Loads local sessions, builds an [`Index`], and times fuzzy/substring
//! queries.

use std::time::Instant;

use txcript::harness::{campfire, claude_code, codex, cursor, dsh, grok, pi};
use txcript::search::{DocKey, Index, Origin, Query};
use txcript::{Codec, Common, HarnessId, Store, Transcript};

fn main() {
    let started = Instant::now();
    let mut index = Index::new();
    let mut loaded = 0usize;
    let mut failed = 0usize;

    load_files::<claude_code::ClaudeCode, _>(
        HarnessId::ClaudeCode,
        claude_code::ClaudeStore::default_root(),
        &mut index,
        &mut loaded,
        &mut failed,
    );
    load_files::<codex::Codex, _>(
        HarnessId::Codex,
        codex::CodexStore::default_root(),
        &mut index,
        &mut loaded,
        &mut failed,
    );
    load_files::<pi::Pi, _>(
        HarnessId::Pi,
        pi::PiStore::default_root(),
        &mut index,
        &mut loaded,
        &mut failed,
    );
    load_files::<campfire::Campfire, _>(
        HarnessId::Campfire,
        campfire::CampfireStore::default_root(),
        &mut index,
        &mut loaded,
        &mut failed,
    );
    load_files::<cursor::Cursor, _>(
        HarnessId::Cursor,
        cursor::CursorStore::default_root(),
        &mut index,
        &mut loaded,
        &mut failed,
    );
    load_files::<grok::Grok, _>(
        HarnessId::Grok,
        grok::GrokStore::default_root(),
        &mut index,
        &mut loaded,
        &mut failed,
    );
    load_files::<dsh::Dsh, _>(
        HarnessId::Dsh,
        dsh::DshStore::default_root(),
        &mut index,
        &mut loaded,
        &mut failed,
    );

    let build = started.elapsed();
    println!(
        "indexed {loaded} sessions ({failed} unreadable): {} lines, {} MB in {build:.2?}",
        index.lines(),
        index.chars() / 1_000_000,
    );

    for query in [
        Query::fuzzy("relay protocol refactor"),
        Query::fuzzy("srch"),
        Query::fuzzy("e"), // pathological: matches nearly every line
        Query::substring("panic"),
        {
            let mut q = Query::substring("error");
            q.origins = Origin::ALL.to_vec();
            q
        },
        {
            // Picker-style per-keystroke query.
            let mut q = Query::fuzzy("relay protocol refactor");
            q.limit = Some(64);
            q
        },
    ] {
        bench(&index, &query);
    }

    if let Some(pattern) = std::env::args().nth(1) {
        show(&index, &Query::fuzzy(&pattern));
    }
}

const RUNS: u32 = 20;

fn bench(index: &Index, query: &Query) {
    // Warm once, then average a burst — the per-keystroke number.
    let _ = index.query(query);
    let start = Instant::now();
    let mut docs = 0;
    for _ in 0..RUNS {
        docs = index.query(query).len();
    }
    let per = start.elapsed() / RUNS;
    println!(
        "{:>9?}/query  {:<9} {} {} {:>28}  -> {docs} docs",
        per,
        format!("{:?}", query.mode).to_lowercase(),
        if query.origins.len() == Origin::ALL.len() {
            "all-origins"
        } else {
            "default    "
        },
        query
            .limit
            .map_or("no-limit".to_string(), |l| format!("limit={l} ")),
        format!("`{}`", query.pattern),
    );
}

fn show(index: &Index, query: &Query) {
    let mut q = query.clone();
    q.limit = Some(5);
    for m in index.query(&q) {
        let title = m.meta.title.as_deref().unwrap_or("");
        println!(
            "\n{} {}  (score {})  {title}",
            m.key.harness, m.key.id, m.score
        );
        for hit in m.hits.iter().take(3) {
            let line: String = hit.line.chars().take(100).collect();
            println!("  [{:?}] {line}", hit.origin);
        }
    }
}

fn load_files<C, S>(
    harness: HarnessId,
    store: Option<S>,
    index: &mut Index,
    loaded: &mut usize,
    failed: &mut usize,
) where
    C: Codec,
    S: Store<H = C>,
    S::Ref: std::fmt::Debug,
{
    // A harness that isn't installed on this machine has no root to scan.
    if let Some(store) = store {
        for found in store.discover().unwrap_or_default() {
            // Keyed by source too: sessions sharing an id (Claude Code
            // duplicates a sessionId across project dirs) must all count.
            let source = format!("{:?}", found.reference);
            match store
                .load(&found.reference)
                .and_then(|native| C::to_common(&native))
            {
                Ok(common) => {
                    insert(index, harness, found.meta.id, source, &common);
                    *loaded += 1;
                }
                Err(_) => *failed += 1,
            }
        }
    }
}

fn insert(
    index: &mut Index,
    harness: HarnessId,
    id: String,
    source: String,
    common: &Transcript<Common>,
) {
    index.insert(
        DocKey {
            harness,
            id,
            source: Some(source),
        },
        common,
    );
}
