# Hermes Agent

Hermes Agent keeps its sessions in a single `SQLite` database,
`~/.hermes/state.db`, and ships a `hermes sessions export` command that
emits one JSON object per session. That export object — the session row
with its message rows nested in a `messages` array — is the harness's
portable text form; txcript reads the database directly for discovery and
loading, matching the exporter's semantics. Provenance is
**reverse-engineered**: the format was described from observed sessions and
Hermes's storage behavior (Hermes CLI, July 2026), cross-checked against
txcript's parser in `src/harness/hermes.rs`.

```
~/.hermes/state.db                       ($HERMES_HOME/state.db overrides)
├─ sessions                              one row per session
│    id, source, model, started_at,      metadata; archived=1 rows are
│    cwd, git_branch, title, archived    hidden from discovery
└─ messages                              ordered rows per session
     role: user | assistant | tool       conversation
     role: session_meta, …               bookkeeping, carried not converted
```

## On disk

The database resolves as `$HERMES_HOME/state.db` (when set and non-empty),
else `~/.hermes/state.db`. Discovery lists non-archived `sessions` rows;
loading selects the session's `messages` rows `WHERE active = 1 ORDER BY
id`, mirroring the exporter: rewound rows (`active = 0`) are history Hermes
itself no longer replays, and stay out.

The store is deliberately **read-only** (`SQLITE_OPEN_READ_ONLY`): Hermes
has no supported session-import API, so txcript converts sessions *from*
Hermes but never writes into `state.db`. `save` and `delete` refuse;
continuing *into* Hermes is refused at the conversion layer with the same
explanation.

## Dissection of a transcript

| Their name | What it is | Maps to |
|---|---|---|
| `sessions` row | `id`, `model`, `started_at` (epoch seconds, float), `cwd`, `git_branch`, `title` | `Meta` |
| `messages` / role `user` | `content`: string, or structured parts (decoded from the `\0json:` sentinel prefix) | `Role::User` with `Block::Text` / `Block::Image` (data-URL `image_url` parts) |
| `messages` / role `assistant` | `content`, `tool_calls` (OpenAI function-call shape), `reasoning_content`, `codex_reasoning_items`, `finish_reason` | `Role::Assistant`; `Block::Thinking` (opaque reasoning in `encrypted`), `Block::ToolUse`, `Message.stop_reason` |
| `messages` / role `tool` | `content`, `tool_call_id`, `effect_disposition` | `Role::User` with `Block::ToolResult`; `denied` → `is_error` |
| `messages` / other roles | `session_meta` and future bookkeeping | carried in the native body, no conversational turn |

Tool names normalize to the Claude convention and back: `read_file` /
`write_file` ⇄ `Read`/`Write` (`path` ⇄ `file_path`), `patch` with
`mode: "replace"` ⇄ `Edit`, `terminal` ⇄ `Bash` (`background` ⇄
`run_in_background`, `timeout` seconds ⇄ `timeout_ms`). Unknown names and
malformed argument JSON pass through as `Tool::Raw`. Tool-result strings
that parse as JSON objects/arrays become structured output; error status is
inferred from `effect_disposition: "denied"`, `success: false`, a non-zero
`exit_code`, or a non-null `error`.

The native export JSON is retained as an opaque value, so unknown session
columns, message columns, and bookkeeping roles survive text round trips.

## Caveats

- Read-only by design (no import API); `--with hermes` is refused.
- Multiple same-kind reasoning blocks join with newlines on write; images
  have no native message-row slot outside structured content parts.
- `started_at` missing or malformed parses to the Unix epoch,
  deterministically, rather than the current time — discovery order stays
  stable for a database txcript cannot fully interpret.
- Blob columns (none in the known schema) would surface as
  `{"$blob_hex": …}` values.

## References

No vendor format documentation is published. The parser
(`src/harness/hermes.rs`) and the integration tests
(`tests/integration/hermes.rs`, including a real temp `SQLite` database
matching the observed schema) are the normative mapping. Observed against
Hermes CLI sessions in July 2026; not re-verified against a live install
since (none was available at revival time).

Last verified: 2026-07-17 (original observation; revived 2026-08-18).
