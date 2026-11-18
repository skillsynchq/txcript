# OpenCode

OpenCode is an open-source terminal coding agent built by the SST team
([github.com/anomalyco/opencode](https://github.com/anomalyco/opencode),
formerly `sst/opencode`). There is no published transcript spec; the
open-source storage and export code serves as the spec. This document is
grounded in that source (permalinks below), in txcript's parser
(`src/harness/opencode.rs` and `src/harness/opencode/v2.rs`), and in
inspection of real local sessions.

```
~/.local/share/opencode/opencode.db          (one SQLite DB for everything)
│
├─ session_v2       1 row  ─ id "ses_…", project_id, directory, title, version,
│                            model (JSON), cost, tokens_*, time_created,
│                            time_updated, time_archived, parent_id
└─ session_message  n rows ─ session_id, type, seq, time_created,
                             data (JSON: the record minus id and type)

session_v2 ──< session_message    reassembled as the `opencode session export`
                                  shape: { info, messages: [ record, … ] }
```

Up to 2.0 the same file held `session`, `message` and `part` tables, and
`opencode export` emitted `{ info, messages: [ { info, parts: […] } ] }`.
txcript reads both layouts; which one a database has is decided by the
presence of `session_v2`.

## On disk

Everything lives in one SQLite database, managed by Drizzle ORM inside
OpenCode: `$XDG_DATA_HOME/opencode/opencode.db`, defaulting to
`~/.local/share/opencode/opencode.db`. txcript resolves the path as
`OPENCODE_DB` env var, then `$XDG_DATA_HOME/opencode/opencode.db`, then the
`~/.local/share` fallback (`OpenCodeStore::default_db`). A sibling `storage/`
directory of per-record JSON files is the pre-SQLite layout; OpenCode migrates
it into the DB on upgrade and txcript reads only the DB.

The DB holds more than transcripts (`project`, `kv`, inbox and pending
queues, instruction state); txcript reads two tables. `session_message.data`
is a JSON text column — the schema is relational only down to the message
grain, opaque JSON below that.

Discovery is one query: `session_v2` rows where `time_archived IS NULL`,
newest first. Since all sessions share the one file, a store `Ref` is just
the session id. Loading a session selects its `session_message` rows in `seq`
order and puts `id` and `type` back on each record, which is the
`{info, messages}` document `opencode session export` emits. That document is
the harness's text form; internally the codec works on the 2.0 layout, and
the two are projected into each other by `src/harness/opencode/v2.rs`.

## Dissection of a transcript

| Their name | What it is | Maps to |
|---|---|---|
| session_v2 | Header row: id (`ses_…`), directory, title, version, created time, model, `parent_id` | `Meta` — id, timestamp, cwd, title, cli_version, model, lineage (`git_branch` is never available) |
| `user` record | One prompt: `text`, optional `files` (base64 attachments with a `mime`) | `Message` with `Role::User`: `Block::Text`, `Block::Image` for `image/*` files |
| `assistant` record | One step: `model`, `content[]`, `finish`, `tokens`, `cost` | One or more Common `Message`s with `Role::Assistant` |
| `content[]` `text` | Prose | `Block::Text` |
| `content[]` `reasoning` | Model thinking | `Block::Thinking` (no signature) |
| `content[]` `tool` | Whole call lifecycle in one item: `id`, `name`, `state {status, input, content/error}` | `Block::ToolUse` + `Block::ToolResult`, paired by `id` |
| `synthetic`, `system`, `skill`, `shell`, `compaction`, `idle`, `*-switched` records | Harness bookkeeping and the user's shell commands | Dropped from the conversation (kept in the native body, written back unchanged) |

Two structural translations do most of the work. First, OpenCode keeps a tool
call and its result in a single `tool` item; txcript splits it Anthropic-style
into a `ToolUse` on an assistant message and a `ToolResult` on a following
user message (an error status becomes `is_error: true`; a still
running/streaming call emits the use with no result; a completed result's
`content` text items are joined by newlines). Second, one OpenCode assistant
record becomes several Common messages, cut at each tool item; the step-level
`tokens` and `finish` attach to the step's last assistant message as `Usage`
and `StopReason` (`stop` → `EndTurn`, `length` → `MaxTokens`, `tool-calls` →
`ToolUse`, etc.). Lowercase tool names normalize to canonical ones — `edit` →
`Edit` with `path`/`oldString`/`newString` renamed to snake_case — and
unknown tools pass through as `Tool::Raw`. Placeholder titles
(`New session - …`) are treated as absent.

2.1 renamed some built-in tools and input keys; the 2.1 document uses its
names and the 2.0 shape keeps the old ones (`TOOLS_2_1` in `v2.rs`): `bash` is
`shell` (`timeout_ms` → `timeout`, `run_in_background` → `background`), `task`
is `subagent` (`subagent_type` → `agent`), `Skill` is `skill` (`skill` → `id`),
`WebSearch` is `websearch`; `read`, `edit` and `write` take `path` for
`filePath`, and `grep` takes `include` for `glob`, `limit` for `head_limit` and
`caseSensitive` for the inverse of `-i`. Every other key is kept, so a call
reads back unchanged.

A synthetic assistant record, shaped like the real thing:

```json
{
  "id": "msg_0123abcd", "type": "assistant", "agent": "build",
  "model": { "id": "claude-opus-4-7", "providerID": "anthropic" },
  "finish": "stop", "cost": 0,
  "tokens": { "input": 6, "output": 88, "reasoning": 0, "cache": { "read": 10, "write": 21428 } },
  "time": { "created": 1778834704540, "completed": 1778834704999 },
  "content": [
    { "type": "reasoning", "text": "The rename is mechanical." },
    { "type": "tool", "id": "call_1", "name": "edit",
      "time": { "created": 1778834704545, "completed": 1778834704550 },
      "state": { "status": "completed",
                 "input": { "path": "/repo/a.rs", "oldString": "old", "newString": "new" },
                 "content": [{ "type": "text", "text": "done" }] } },
    { "type": "text", "text": "Renamed it." }
  ]
}
```

The 2.0 shape, which `from_text` still reads, kept the same information as a
`message` (`{role, modelID, finish, tokens, …}`) plus `parts` of type `text`,
`reasoning`, `file` and `tool` (`{tool, callID, state {status, input,
output/error}}`), with bookkeeping parts (`step-start`/`step-finish`,
`snapshot`, `patch`) in between.

## Lineage

`session_v2.parent_id` names the session that spawned this one — OpenCode's
subagent/task tool starts a child session with its own row. A non-empty value
always becomes a Spawn `Meta.lineage`; it's a bare column read, so `discover`
(one extra `SELECT` column) and `load` both fill it with no added query.
OpenCode has no fork or continue relation that reuses this column.
`from_common`/`opencode session import` never writes it.

## Caveats

- **Writes go through the CLI, not the DB.** txcript opens the database
  read-only; `save` serializes the document to a private temp file and shells
  out to `opencode session import <file> --directory <cwd>` (the binary must
  be on PATH; `--directory` is the transcript's cwd when it exists on this
  machine, otherwise OpenCode files the session under the current directory),
  letting OpenCode own its schema. `import` rejects session ids that don't
  start with `ses`, so foreign UUIDs are deterministically re-shaped. txcript
  reads `opencode --version` first: a 2.0 or earlier CLI gets the old
  `{info, parts}` export through `opencode import` instead.
- **2.1 validates the document.** The header must carry `projectID`, `cost`,
  `tokens` and `location`; every message must be one of 2.1's record types.
  `from_common` writes exactly that; a 2.0 export file is not accepted by
  2.1's importer (its legacy decoder only renames native-data keys), so
  `continue` a 2.0 file through txcript rather than importing it directly.
- **Delete is archive.** `delete` sets `time_archived`; rows remain and the
  session is recoverable from OpenCode's UI. Discovery filters archived rows.
- **Schema drift.** The schema is Drizzle-migrated and moves quickly (the
  session table keeps growing columns). txcript touches only the stable
  core: `session_v2(id, directory, title, version, time_created,
  time_updated, time_archived, model, slug, parent_id)` and
  `session_message(id, session_id, type, seq, time_created, data)` — or, on
  a 2.0 database, `session`, `message(id, session_id, time_created, data)` and
  `part(id, message_id, session_id, data)`. Channel builds may write
  `opencode-<channel>.db`; only `opencode.db` is discovered.
- **Lossiness in Common.** Bookkeeping records and synthetic text are
  dropped; per-item timestamps collapse to the message's; a tool result
  shares its call's timestamp after a round-trip; `EndTurn`, `StopSequence`,
  `Aborted`, and `Other` stop reasons all re-export as `"stop"` (OpenCode
  requires a finish and has no spelling for the rest). An assistant image has
  no slot in a 2.1 assistant record and is dropped on write; the 2.1 header
  has none for the CLI version (`Meta.cli_version` is read from the database
  column, and the importer stamps its own). Same-harness round-trips through
  Common are fixpoint-tested, not byte-identical.
- **Hostile input.** A `session_message` row whose JSON doesn't parse is
  dropped and the session survives; unknown record types are kept verbatim
  and skipped by the codec; an absent DB means "no sessions", not an error.
  The import staging file is created `create_new` + mode 0600 under a
  randomized name.

## References

- Drizzle tables `session_v2` / `session_message`:
  <https://github.com/anomalyco/opencode/blob/v2.1.4/packages/core/src/session/sql.ts>
- Message records (`Session.Message.Info`):
  <https://github.com/anomalyco/opencode/blob/v2.1.4/packages/schema/src/session-message.ts>
- Import endpoint and the 2.0-export compatibility it offers:
  <https://github.com/anomalyco/opencode/blob/v2.1.4/packages/protocol/src/groups/session.ts>,
  <https://github.com/anomalyco/opencode/blob/v2.1.4/packages/schema/src/session-legacy.ts>
- `opencode session import` / `session export` commands:
  <https://github.com/anomalyco/opencode/blob/v2.1.4/packages/cli/src/commands/commands.ts>,
  <https://github.com/anomalyco/opencode/blob/v2.1.4/packages/cli/src/commands/handlers/session/import.ts>
- Importer (`SessionTransfer.import`):
  <https://github.com/anomalyco/opencode/blob/v2.1.4/packages/core/src/session/transfer.ts>
- 2.0 tables `session` / `message` / `part` and export shape:
  <https://github.com/sst/opencode/blob/3a90639cb57619a21e59f544b3e8d23ffed56f48/packages/core/src/session/sql.ts>,
  <https://github.com/sst/opencode/blob/3a90639cb57619a21e59f544b3e8d23ffed56f48/packages/opencode/src/cli/cmd/export.ts>

Last verified: 2026-10-27, against `src/harness/opencode.rs`,
`src/harness/opencode/v2.rs`, OpenCode 2.1.4 and real local sessions. The
authoritative mapping is `src/harness/opencode.rs` (codec, tool-name
normalization, SQLite store) and `src/harness/opencode/v2.rs` (the 2.1
projection) with `tests/integration/opencode.rs` as executable shape examples.
