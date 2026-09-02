# Cursor

Cursor is Anysphere's AI coding product. txcript reads the sessions of its
**CLI agent** (`cursor-agent`, resumed with `agent --resume=<id>`), which keeps
a resumable chat store per session under `~/.cursor/chats`. This is *not* the
editor's chat storage — IDE chats live in the app's `state.vscdb` and are
covered by [cursor-desktop.md](cursor-desktop.md). The format is closed source
and undocumented; everything in this page is reverse-engineered from real
local sessions and encoded in txcript's parser. Cursor's SDK docs describe a
matching *abstract* store (content-addressed "checkpoint" blobs, a
`rootBlobId` pointer) but publish no schema, table names, or file paths.

```
~/.cursor/chats/
└── <md5(workspace path)>/            workspace bucket, hex
    └── <session uuid>/               one session
        ├── store.db                  SQLite
        │   ├── blobs(id, data)      id = sha256(data), content-addressed
        │   │    ├─ JSON blobs       messages: role user/assistant/tool
        │   │    └─ protobuf blobs   turn graph: user msg, steps, turns, root
        │   └── meta(key, value)     key "0" → (hex-encoded) JSON agent record
        ├── meta.json                 title, createdAtMs, updatedAtMs
        └── prompt_history.json       array of user prompt strings
```

## On disk

The root is `~/.cursor/chats` (`CursorStore::default_root()`); a different root
can be passed programmatically, but there is no environment override. Layout is
two directory levels: a workspace bucket named by the lowercase hex MD5 of the
absolute workspace path, then one directory per session named by its UUID —
the session id txcript reports. Each session directory holds a `store.db`
SQLite database (two tables, `blobs` and `meta`), a small `meta.json`, and a
`prompt_history.json`. WAL sidecars (`store.db-wal`/`-shm`) appear while the
CLI is live; txcript opens the database read-only.

Discovery walks the two levels and keeps every `store.db` that opens and
parses; broken databases are silently skipped. Timestamps come from
`meta.json.createdAtMs`, falling back to the meta-table record's `createdAt`,
then the file's mtime. SQLite support is gated behind the `opencode` cargo
feature (the shared rusqlite dependency); without it the store is inert.

## Dissection of a transcript

The conversation is the sequence of *JSON* blobs, read in `rowid` order (no
explicit linking or threading). Non-JSON blobs are protobuf-encoded internal
graph state and carry no messages.

| Their name | What it is | Maps to |
|---|---|---|
| `blobs` row | content-addressed blob, `id` = SHA-256 of `data` | JSON → a `Message`; protobuf → kept as opaque bytes |
| `role: "user"` blob | user turn; `content` is a string or block array, text wrapped in `<user_query>…</user_query>` | `Role::User` with `Block::Text` / `Block::Image` |
| `role: "assistant"` blob | blocks of type `text`, `reasoning`/`thinking`, `redacted-reasoning`, `tool-call`, `image` | `Role::Assistant` with `Text` / `Thinking` / `ToolUse` |
| `role: "tool"` blob | `tool-result` entries: `toolCallId`, `toolName`, `result`, `isError` | `Role::User` with `Block::ToolResult` |
| `toolCallId` | pairs a `tool-call` with its result | `ToolUse.id` / `ToolResult.tool_use_id` |
| `createdAt` | per-message ms epoch | `Message.timestamp` |
| `providerOptions.cursor.modelName` | model per assistant message | `Message.model`, seeds `Meta.model` |
| `Shell`, `StrReplace`, `Read`, … | Cursor's tool vocabulary | `Tool::Bash`, `Tool::Edit`, `Tool::Read`, … (`path`→`file_path`, `cwd`→`workdir`) |
| protobuf blobs | turn graph: user message, assistant/thinking/tool steps, turn structure, root state | not converted; regenerated from scratch by `from_common` |
| `meta` key `"0"` | agent record (`agentId`, `name`, `createdAt`, `workspacePath`, `lastUsedModel`, `latestRootBlobId`, `mode`, `approvalMode`), hex-encoded or plain JSON | fallbacks for `Meta` id/title/timestamp/cwd/model |
| `meta.json` | `schemaVersion`, `title`, `createdAtMs`, `updatedAtMs`, `hasConversation` | primary source of `Meta.title` and `Meta.timestamp` |
| `prompt_history.json` | user prompt strings | not read; regenerated on save |

User blobs whose text contains `<user_info>` are editor-injected context, not
the user's turn, and are dropped. `<user_query>` wrappers are stripped on read
and re-added on write; `[REDACTED]` markers are removed. A `tool-call` missing
its `toolCallId` gets a deterministic UUIDv5 minted from session id, message
index, block index, and tool name. When `isError` is absent it is inferred
heuristically (an `Error:` prefix or an "exited with code" phrase). Title falls
back to the first real user text when the metadata carries none; `Meta.cwd`
comes from `workspacePath`, or is scraped from a `Workspace Path:` line in
injected context. `stop_reason` and `usage` are never present.

A synthetic assistant blob, shaped like the real thing:

```json
{
  "role": "assistant",
  "content": [
    {"type": "text", "text": "I'll read the file first."},
    {"type": "tool-call", "toolCallId": "call_01",
     "toolName": "Read", "args": {"path": "src/main.rs"}}
  ],
  "createdAt": 1754000000000,
  "providerOptions": {"cursor": {"modelName": "composer-2.5"}}
}
```

## Caveats

- **The protobuf turn graph is the hard part.** Roughly a quarter of blobs in
  an observed session are non-JSON graph state. On native load txcript keeps
  every blob byte-for-byte, so a same-harness round-trip is lossless. Coming
  *from* Common, the graph must be synthesized: txcript hand-encodes protobuf
  for shell, read, and edit/write tool steps; other tools degrade to
  plain-text steps. The result resumes in `cursor-agent`, but the graph is an
  approximation, not what Cursor itself would have written.
- Conversion to Common is lossy by design: system-prompt blobs, unknown roles,
  editor-injected context, and all graph blobs carry no `Message`.
- The `meta` table value has been observed hex-encoded; the parser also
  accepts plain JSON, suggesting version drift in the CLI.
- Message ordering rests on `rowid` insertion order — nothing in the records
  themselves orders them.
- Hostile input: session ids are validated as path components before writes,
  and `delete()` canonicalizes and checks containment under the chats root
  (exactly `<root>/<workspace>/<id>`) before removing anything.
- Observations are from `cursor-agent` 2026.06.26 on macOS, sessions with
  `meta.json` `schemaVersion: 1`. The blob shapes (Vercel-AI-SDK-style
  `tool-call` / `tool-result` types) may drift with CLI releases.

## References

No public specification of the `~/.cursor/chats` store exists. Cursor's
TypeScript SDK documentation (<https://cursor.com/docs/sdk/typescript>,
accessed 2026-08-10) describes an abstract `LocalAgentStore` with
content-addressed checkpoint blobs and a `latestCheckpoint.rootBlobId`
pointer — consistent with what is on disk — but documents no schema, table,
or path. This document is reverse-engineered.

Last verified: 2026-08-10, against src/harness/cursor.rs and real local
sessions. The authoritative mapping is `src/harness/cursor.rs`; shape examples
live in `tests/integration/cursor.rs`.
