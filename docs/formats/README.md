# Transcript formats

One document per harness, describing the on-disk transcript format txcript
reads and writes. These are high-level, human-readable companions to the
parsers — the code in `src/harness/` and the integration tests are the
authoritative mapping; when a document and the code disagree, the code wins
and the document gets fixed.

Every document follows the same structure:

1. **About** — what the harness is and where the knowledge in the document
   comes from (its provenance, see below), followed by a diagram of the
   format's anatomy.
2. **On disk** — where sessions live on the computer, how files are named,
   and how discovery finds them.
3. **Dissection of a transcript** — each part of the format, the name the
   format itself uses for it, and what txcript translates it to.
4. **Caveats** — quirks, drift, and lossiness.
5. **References** — citations, and a `Last verified:` date.

## Provenance

Not all of these formats are documented by their vendors, so each document
states which of three tiers its knowledge comes from:

- **Official documentation** — the vendor publishes a format spec; the
  document cites it with an access date.
- **Open source** — no spec, but the harness's own serialization code is
  public; the document cites permalinks pinned to a commit SHA.
- **Reverse-engineered** — closed source and undocumented; the format is
  described from observed sessions, and the document records the harness
  version and date of observation.

A dated `Last verified:` line closes every document. These formats drift
silently — an undated description of a reverse-engineered format is worse
than none.

## Harnesses

| Document | Harness | Parser |
| --- | --- | --- |
| [claude-code.md](claude-code.md) | Claude Code (Anthropic) | `src/harness/claude_code.rs` |
| [codex.md](codex.md) | Codex CLI (OpenAI) | `src/harness/codex.rs` |
| [opencode.md](opencode.md) | OpenCode (SST) | `src/harness/opencode.rs` |
| [cursor.md](cursor.md) | Cursor | `src/harness/cursor.rs` |
| [amp.md](amp.md) | Amp (Sourcegraph) | `src/harness/amp.rs` |
| [grok.md](grok.md) | Grok CLI (xAI) | `src/harness/grok.rs` |
| [antigravity.md](antigravity.md) | Antigravity (Google) | `src/harness/antigravity.rs` |
| [pi.md](pi.md) | pi | `src/harness/pi.rs` |
| [campfire.md](campfire.md) | Campfire (embeds pi) | `src/harness/campfire.rs` |
| [simple.md](simple.md) | Simple (txcript's own interchange format) | `src/harness/simple.rs` |
