# Test taxonomy

Four kinds of verification, each in its own place. When adding a test, pick
the category by what the test *proves*, not by what module it touches.

| Category | Where | Run with |
|---|---|---|
| Unit | `#[cfg(test)] mod tests` inline in `src/` and `cli/src/` | `cargo test --lib -p txcript -p txcript-cli` |
| Integration | `tests/integration/` | `cargo test --test integration --all-features` |
| Regression | `tests/regression/` | `cargo test --test regression --all-features` |
| Benchmarks | `benches/` (criterion) | `cargo bench` |

`cargo test --workspace --all-features` runs everything except benchmarks —
the same invocation CI uses.

## Unit

Pure logic testable without touching disk or another module's internals:
tool-argument typing (`src/common.rs`), text projection (`src/text.rs`),
fragment range parsing (`cli/src/fragment.rs`). Lives next to the code it
tests, Rust-style. If a test needs a temp dir or more than one public type,
it probably belongs in integration.

## Integration

`tests/integration/` — one binary, public API only, real backing stores
(temp dirs, real SQLite), no mocks. One module per harness covering the
three contracts every harness must honor:

- **Store round-trip**: `load → save → load` is lossless on disk, including
  record types the codec doesn't model.
- **Codec fixpoint**: `to_common(from_common(c)) == c`, and `from_common` is
  deterministic.
- **Discovery**: session metadata (id, cwd, title, model, timestamp) is
  extracted correctly.

Plus cross-cutting modules: `cross_harness` (a conversation survives a hop
through every harness), `path_safety` (adversarial ids, symlinks, and
references cannot escape a store root), `store_delete`, `search`, and
`properties` — proptest sweeps asserting the codec fixpoint over generated
conversations for every harness that writes (all but the pull-only Claude
Chat and ChatGPT) at once. The generator in `properties`
is deliberately constrained to what every harness models; widen it
deliberately (see its module docs), and commit any
`tests/proptest-regressions/` file proptest writes — that is its record of
past failing seeds.

## Regression

`tests/regression/` — one test per bug that actually shipped, cited by the
commit that fixed it, self-contained enough to understand the incident from
this file alone. The rule: if a test exists because something *broke* (here
or upstream), it goes here with the story in its doc comment. If it exists
because something must always hold, it's an integration test.

## Benchmarks

`benches/` — criterion, deterministic synthetic sessions, reproducible on
any machine; run `cargo bench` and compare against a baseline with
`--save-baseline`/`--baseline`. `examples/search_bench.rs` is different on
purpose: it profiles against the real sessions on *your* machine (as the
`corpus-check` workflow does) and is a profiling tool, not a benchmark you
can compare across machines.

## Conventions

- Fixtures are handcrafted JSON modeled on real session files, inline in the
  test module — no fixture directories, no mocks, and never a copied real
  session (they contain private data).
- Every test file opens with `#![allow(clippy::expect_used, clippy::panic,
  clippy::unwrap_used)]`; the production lint set (workspace `Cargo.toml`)
  denies all three.
- Feature-gated surfaces gate at the module declaration in `main.rs`
  (`search`) or on the test (`opencode` store).
