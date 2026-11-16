# OpenCode

OpenCode is an open-source terminal coding agent built by the SST team
([github.com/sst/opencode](https://github.com/sst/opencode)). There is no
published transcript spec; the open-source storage and export code serves as
the spec. This document is grounded in that source (permalinks below), in
txcript's parser (`src/harness/opencode.rs`), and in inspection of real local
sessions.

```
~/.local/share/opencode/opencode.db          (one SQLite DB for everything)
│
├─ 1.x: session / message / part
│   ├─ session   1 row  ─ id "ses_…", directory, title, version,
│   │                     time_created, time_archived, model (JSON)
│   ├─ message   n rows ─ session_id, time_created, data (JSON: role, modelID,
│   │                     finish, tokens, …)
│   └─ part      m rows ─ message_id, session_id, data (JSON: one typed part)
│
└─ 2.x: session_v2 / session_message
    ├─ session_v2      1 row  ─ id "ses_…", project_id, directory, title,
    │                           version, agent, model (JSON), time_created,
    │                           time_archived, parent_id
    └─ session_message n rows ─ session_id, type, seq, time_created, data
                                (JSON: one whole message)

1.x reassembles as `{ info, messages: [ { info, parts: […] } ] }`;
2.x as `{ info, messages: [ { type: "user", text, files } |
                            { type: "assistant", content: […] } ] }`.
```

Both generations are supported. A database holding `session_v2` +
`session_message` is 2.x — OpenCode keeps the 1.x tables around for rows it
migrated, but writes every new session to the 2.x pair and reads only those, so
a 1.x-shaped import is invisible in its UI.

## On disk

Everything lives in one SQLite database, managed by Drizzle ORM inside
OpenCode: `$XDG_DATA_HOME/opencode/opencode.db`, defaulting to
`~/.local/share/opencode/opencode.db`. txcript resolves the path as
`OPENCODE_DB` env var, then `$XDG_DATA_HOME/opencode/opencode.db`, then the
`~/.local/share` fallback (`OpenCodeStore::default_db`). A sibling `storage/`
directory of per-record JSON files is the pre-SQLite layout; OpenCode migrates
it into the DB on upgrade and txcript reads only the DB.

The DB holds more than transcripts (`project`, `todo`, cost/token rollups on
`session`); txcript reads three tables. `message.data` and `part.data` are
JSON text columns — the schema is relational only down to the message/part
grain, opaque JSON below that.

Discovery is one query: `session` (1.x) or `session_v2` (2.x) rows where
`time_archived IS NULL`, newest first. Since all sessions share the one file,
a store `Ref` is just the session id. Loading a 1.x session selects its
`message` rows (ordered by `time_created`, then id) and `part` rows (grouped by
message, ordered by id), and assembles the same `{info, messages: [{info,
parts}]}` document that `opencode export` emits — that export shape, not the
raw rows, is txcript's native `Body` for this harness. Loading a 2.x session
selects `session_message` rows by `seq` and expands each flat message back
into the same `{info, parts}` record, so both generations converge on one code
path. A 2.x record with no 1.x shape — `compaction`, `idle`, `shell`,
`system`, … — rides through as an opaque record the codec skips and an export
writes back unchanged. The text form (`from_text`/`to_text`, used by the WASM
converters) speaks the 2.x `opencode session export` document and reads the
1.x export shape as well.

## Writing into OpenCode 2.x

2.x changed more than the tables: `opencode import` became
`opencode session import`, and the document is no longer the 1.x export shape.
It flattens each message (`{type: "user"|"assistant", …}` with a `content`
array) and it *requires* two fields the 1.x export never had:

- `info.projectID` and `info.location.directory` — schema-required, but the
  server ignores both: the import request carries its own top-level
  `location`, which the CLI fills from `--directory`, and that is what the
  importer resolves the project from, rewriting the stored row. txcript still
  emits the real project row id when the read-only `project` table matches
  the directory — OpenCode stores the canonical git/hg root there, so a
  subdirectory of the repository matches too — and falls back to the schema's
  own `"global"` static when nothing does. `info.location.directory` is kept
  in step with the `--directory` argument txcript passes.

`OpenCodeStore::save` asks `opencode --version` which generation it is
talking to: a CLI before 2.1 gets the legacy document through
`opencode import`, 2.1 and later get the flat document through
`opencode session import --directory …`. The binary, not the database, is the
signal because an upgraded CLI migrates an old database on its next start —
the import may legitimately run first.

The conversion is mechanical: a user turn's text parts join into `text`, `file`
parts become `files[]` entries with an inline `[Image N]` marker, assistant
`text`/`reasoning` parts map one-to-one, a `tool` part moves `state.output`
into `state.content`, and `tool_use` becomes `finish: "tool-calls"`. Injected
(`synthetic`) user text becomes its own `synthetic` record. Token counts roll
up into `info.tokens`, every per-message token object is written in the full
shape the schema validates, and tool input is forced to an object (a freeform
Codex code-mode input rides as `{"input": …}`).

2.x also renamed the built-in tools and several input keys, so the document
uses its names (`packages/core/src/tool/plugin/*.ts`): `bash` is `shell`
(`timeout_ms` → `timeout`, `run_in_background` → `background`), `read`,
`edit` and `write` take `path` for `filePath`, `grep` takes `include` for
`glob` and `limit` for `head_limit` (its `-i` flag is the inverse of
`caseSensitive`), `task` is `subagent` (`subagent_type` → `agent`), `Skill`
is `skill` (`skill` → `id`), and `WebSearch` is `websearch`. Reading maps
them back, so every other key survives a round trip unchanged.

Attachment payloads are normalized to the canonical base64 the import schema
demands — 1.x also stored percent-encoded data URLs and wrapped base64, and
OpenCode's own migration re-encodes both — and mention offsets are written as
JS string indices (UTF-16 code units), matching how the composer places
extmarks. An `[Image N]` marker already present in the text is pointed at
rather than appended a second time, so a round-trip doesn't fork the marker.

Re-importing a session whose id already exists does not update it: 2.x
rejects the duplicate (the CLI prints `Session already exists` to stderr and
exits 0), and txcript surfaces that as an error rather than reporting a save
that never landed. 1.x behaved differently — it kept the session row and
appended messages not yet present.

## Dissection of a transcript

| Their name | What it is | Maps to |
|---|---|---|
| session | Header row: id (`ses_…`), directory, title, version, created time, model, `parent_id` | `Meta` — id, timestamp, cwd, title, cli_version, model, lineage (`git_branch` is never available) |
| message | One turn: `data` JSON with `role`, plus `modelID`, `finish`, `tokens` on assistant turns | One or more Common `Message`s, `Role::User` / `Role::Assistant` |
| part `text` | Prose; `"synthetic": true` marks harness-injected text | `Block::Text` (synthetic dropped) |
| part `reasoning` | Model thinking | `Block::Thinking` (no signature) |
| part `file` | Attachment as a `data:` URL with a `mime` | `Block::Image` when the mime is `image/*` |
| part `tool` | Whole call lifecycle in one record: `tool`, `callID`, `state {status, input, output/error}` | `Block::ToolUse` + `Block::ToolResult`, paired by `callID` |
| part `step-start` / `step-finish` / `snapshot` / `patch` … | Agent-loop bookkeeping | Dropped from the conversation (kept in the native body) |

Two structural translations do most of the work. First, OpenCode keeps a tool
call and its result in a single `tool` part; txcript splits it Anthropic-style
into a `ToolUse` on an assistant message and a `ToolResult` on a following
user message (an error status becomes `is_error: true`; a still
pending/running call emits the use with no result). Second, one OpenCode
assistant message becomes several Common messages, cut at each tool part;
the turn-level `tokens` and `finish` attach to the turn's last assistant
message as `Usage` and `StopReason` (`stop` → `EndTurn`, `length` →
`MaxTokens`, etc.). Lowercase tool names normalize to canonical ones —
`edit` → `Edit` with `filePath`/`oldString`/`newString` renamed to snake_case
— and unknown tools pass through as `Tool::Raw`. Placeholder titles
(`New session - …`) are treated as absent.

A synthetic message record, shaped like the real thing:

```json
{
  "info": {
    "id": "msg_0123abcd", "sessionID": "ses_0123abcd", "role": "assistant",
    "modelID": "claude-opus-4-7", "providerID": "anthropic",
    "finish": "stop", "cost": 0,
    "tokens": { "input": 6, "output": 88, "cache": { "read": 10, "write": 21428 } },
    "time": { "created": 1778834704540, "completed": 1778834704999 }
  },
  "parts": [
    { "type": "step-start" },
    { "type": "reasoning", "text": "The rename is mechanical." },
    { "type": "tool", "tool": "edit", "callID": "call_1",
      "state": { "status": "completed",
                 "input": { "filePath": "/repo/a.rs", "oldString": "old", "newString": "new" },
                 "output": "done" } },
    { "type": "text", "text": "Renamed it." }
  ]
}
```

## Lineage

`session.parent_id` names the session that spawned this one — OpenCode's subagent/task tool
starts a child session with its own row. A non-empty value always becomes a Spawn
`Meta.lineage`; it's a bare column read, so `discover` (one extra `SELECT` column) and `load`
both fill it with no added query. OpenCode has no fork or continue relation that reuses this
column. `from_common`/`opencode import` never writes it.

## Caveats

- **Writes go through the CLI, not the DB.** txcript opens the database
  read-only; `save` serializes the document to a private temp file and shells
  out to `opencode import` (1.x) or `opencode session import` (2.x), with the
  binary on PATH, letting OpenCode own its schema. `import` rejects session
  ids that don't start with `ses`, so foreign UUIDs are deterministically
  re-shaped.
- **Delete is archive.** `delete` sets `time_archived`; rows remain and the
  session is recoverable from OpenCode's UI. Discovery filters archived rows.
- **Schema drift.** The schema is Drizzle-migrated and moves quickly (the
  `session` and `session_v2` tables keep growing columns). txcript touches only
  the stable core: `session(id, directory, title, version, time_created,
  time_archived, model, parent_id)`, `message(id, session_id, time_created,
  data)`, `part(id, message_id, session_id, data)`, and on 2.x
  `session_v2(id, project_id, directory, title, version, model, time_created,
  time_updated, time_archived, parent_id)` + `session_message(id, session_id,
  type, seq, time_created, data)`. Channel builds may write
  `opencode-<channel>.db`; only `opencode.db` is discovered.
- **Lossiness in Common.** Bookkeeping and synthetic parts are dropped;
  per-part timestamps collapse to the message's; a tool result shares its
  call's timestamp after a round-trip; `EndTurn`, `StopSequence`, `Aborted`,
  and `Other` stop reasons all re-export as `"stop"` (OpenCode requires a
  finish and has no spelling for the rest). Same-harness round-trips through
  Common are fixpoint-tested, not byte-identical.
- **Hostile input.** A `part` row whose JSON doesn't parse is dropped and the
  message survives; unknown or missing roles are skipped; an absent DB means
  "no sessions", not an error. The import staging file is created
  `create_new` + mode 0600 under a randomized name.

## References

- Drizzle tables `session` / `message` / `part`:
  <https://github.com/sst/opencode/blob/3a90639cb57619a21e59f544b3e8d23ffed56f48/packages/core/src/session/sql.ts>
- Export shape `{info, messages: [{info, parts}]}`:
  <https://github.com/sst/opencode/blob/3a90639cb57619a21e59f544b3e8d23ffed56f48/packages/opencode/src/cli/cmd/export.ts>
- DB filename and channel-suffix logic:
  <https://github.com/sst/opencode/blob/3a90639cb57619a21e59f544b3e8d23ffed56f48/packages/core/src/database/database.ts>
- XDG data-dir resolution:
  <https://github.com/sst/opencode/blob/3a90639cb57619a21e59f544b3e8d23ffed56f48/packages/core/src/global.ts>
- Legacy JSON `storage/` tree and its migration:
  <https://github.com/sst/opencode/blob/3a90639cb57619a21e59f544b3e8d23ffed56f48/packages/opencode/src/storage/storage.ts>

Last verified: 2026-09-26, against src/harness/opencode.rs and real local
sessions. The authoritative mapping is `src/harness/opencode.rs` (codec,
tool-name normalization, SQLite store) with `tests/integration/opencode.rs`
as executable shape examples.
