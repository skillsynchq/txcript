# Cursor desktop

Cursor is Anysphere's AI coding product. txcript reads the agent sessions of
its **desktop app** (the IDE's Agent panel and Agents home), which the app
keeps as rows in one global SQLite database, `state.vscdb`. This is *not* the
CLI agent's store — `cursor-agent` sessions live under `~/.cursor/chats` and
are covered by [cursor.md](cursor.md). The format is closed source and
undocumented: Cursor's docs describe chat history features but never the
storage, and community forum threads confirm only the file path. Everything
below is **reverse-engineered** from real local sessions and encoded in
txcript's parser. Observations are from Cursor 3.16 and 3.17.8 on macOS.

Each session (Cursor calls it a *composer*) is one row in a header table plus
a handful of keys in a key-value table: a state document and one JSON
"bubble" per message.

```
<Cursor User dir>/                       ~/Library/Application Support/Cursor/User
├── globalStorage/
│   └── state.vscdb                      SQLite, rollback journal
│       ├── composerHeaders              one row per session: the discovery index
│       │     composerId | workspaceId | createdAt | lastUpdatedAt | isArchived
│       │     | isSubagent | recency | checkpointAt | value ({"type":"head",…})
│       ├── cursorDiskKV (key, value)    the session bodies
│       │     composerData:<cid>              state document (model, bubble order, …)
│       │     bubbleId:<cid>:<bubbleId>       one message; type 1 user, 2 assistant
│       │     checkpointId:<cid>:<id>         file checkpoints (carried, not read)
│       │     composerVirtualRowHeights:<cid> renderer cache (carried, not read)
│       │     agentKv:blob:<sha>              model-side request cache (not carried)
│       └── ItemTable (key, value)       app state; the Agents sidebar index lives here
│             glass.localAgentProjects.v1
│             glass.localAgentProjectMembership.v1
└── workspaceStorage/<hash>/workspace.json   {"folder": "file:///…"} → workspaceId
```

## On disk

The root is the app's `User` directory: `~/Library/Application Support/Cursor/User`
on macOS, `%USERPROFILE%\AppData\Roaming\Cursor\User` on Windows,
`~/.config/Cursor/User` elsewhere (`CursorDesktopStore::default_root()`). The
`CURSOR_DESKTOP_USER_DIR` environment variable overrides it. Every session on
the machine lives in the single database at `globalStorage/state.vscdb`;
`-wal`/`-shm` sidecars sit beside it while the app is running, and txcript
opens it read-only. Workspace ids in `composerHeaders.workspaceId` are the
app's own 32-hex-digit hashes (or `empty-window` for a session with no
folder); `workspaceStorage/<hash>/workspace.json` maps a hash back to its
folder URI.

Discovery is one query over `composerHeaders`, newest `recency` first, joined
to each session's `composerData:` cell and filtered to sessions that own at
least one `bubbleId:` row — drafts and empty composers (the app keeps an
`empty-state-draft` row, for instance) are not sessions. Bubble bodies are
never read during discovery. Databases from before Cursor introduced the
header table (the app records the migration under the
`composer.composerHeaders.migratedToTable` key) fall back to scanning
`composerData:` keys and synthesizing a head from each document. The session
id txcript reports is the `composerId`, a UUID. Change detection uses
`composerHeaders.lastUpdatedAt`.

SQLite support is gated behind the `opencode` cargo feature (the shared
rusqlite dependency); without it the store is inert, though the codec still
converts txcript's JSON text form.

## Dissection of a transcript

A session's native body is its header row and every `cursorDiskKV` row keyed
by its composer id, each cell kept as the raw text the app wrote. Messages are
the `bubbleId:` rows, read in the order `composerData.fullConversationHeadersOnly`
lists them (by `rowid` when that list is absent). Nothing threads bubbles to
each other; the state document's list is the only ordering.

| Their name | What it is | Maps to |
| --- | --- | --- |
| `composerHeaders.composerId` | The session id (UUID) | `Meta.id` |
| `composerHeaders.createdAt` | ms epoch | `Meta.timestamp` |
| `composerHeaders.value` (`{"type":"head",…}`) | `name`, `workspaceIdentifier.uri.fsPath`, `trackedGitRepos`, `unifiedMode`, `isDraft`, `isArchived` | `Meta.title` (`name`, else `subtitle`); `Meta.cwd` (`workspaceIdentifier.uri.fsPath`, else `agentLocation.environment.uri.fsPath`, else `trackedGitRepos[0].repoPath`) |
| `composerData:<cid>` (`_v` 17) | The composer's full state document: `modelConfig`, `fullConversationHeadersOnly`, context, todos, flags | `Meta.model` from `modelConfig.modelName` (`"default"` is the picker placeholder, not a model); bubble order from `fullConversationHeadersOnly[].bubbleId` |
| `bubbleId:<cid>:<bubbleId>` (`_v` 3) | One message document; `type` 1 user, 2 assistant; `createdAt` RFC 3339 | one `Message`, or part of one |
| user bubble `text` (with `richText`, `context`) | The prompt as plain text; `richText` is the same text as a ProseMirror document; `context` lists attachments | `Role::User` with one `Block::Text` |
| assistant bubble `text` | A visible response segment | `Block::Text` |
| `thinking` `{text, signature}` (`capabilityType` 30) | A reasoning segment and its provider signature | `Block::Thinking` (skipped when `text` is blank) |
| `toolFormerData` (`capabilityType` 15) | A tool call *and* its result: `tool` (numeric id), `name`, `toolCallId`, `params` (JSON as a string), `status`, `result` (string) | `Block::ToolUse` on the assistant message, then a `Role::User` message holding the `Block::ToolResult` |
| `toolFormerData.status` | `started`, `completed`, `error` | `completed`/`error` pair a result (`is_error` on `error`); anything else is in flight and yields no result |
| `toolFormerData.result` | The tool's output as a string | `ToolOutput::Json` when it parses as non-null JSON, else `ToolOutput::Text` |
| `modelInfo.modelName` | Model per bubble | `Message.model` |
| `tokenCount` `{inputTokens, outputTokens}` | Per-bubble counts; all-zero is the serializer default | `Message.usage` when either is non-zero |
| `run_terminal_command_v2` (tool 15), `read_file_v2` (40) | Cursor's shell and read tools | `Tool::Bash` (`command`, `cwd`→`workdir`, `options.timeout`→`timeout_ms`), `Tool::Read` (`targetFile`→`file_path`, `offset`, `limit`) |
| `ripgrep_raw_search` (41), `glob_file_search` (42), `ask_question` (51), MCP tools, … | The rest of Cursor's tool vocabulary | `Tool::Raw`, name and params untouched |
| `checkpointId:<cid>…`, `composerVirtualRowHeights:<cid>`, any other `<kind>:<cid>…` key | File checkpoints, renderer caches, future record kinds | carried verbatim in the native body; no `Message` |
| `agentKv:blob:*` | Content-addressed model-side request cache, commingled across sessions | not carried; the app regenerates it |

Consecutive assistant bubbles fold into one assistant `Message`: a thinking
bubble, a text bubble, and a tool bubble in a row become one message with
three blocks. A tool call closes the assistant message, because the paired
result must follow as its own user message. User bubbles with blank text are
dropped, as are bubbles that fail to parse as JSON (they still survive
natively). Cursor call ids can contain characters other harnesses reject (a
literal newline has been observed), so ids are folded onto `[A-Za-z0-9_-]`;
a missing id gets a deterministic UUIDv5 from the session id and position.
`Meta.git_branch`, `Meta.cli_version`, and `stop_reason` are never present.

A real assistant tool bubble, with the empty-collection skeleton Cursor's
serializer writes on every bubble stripped out:

```json
{
  "_v": 3,
  "type": 2,
  "bubbleId": "9ec20cdb-efd4-5159-8af2-c3c3ad9cf80c",
  "createdAt": "2026-08-17T18:55:41.627Z",
  "modelInfo": {"modelName": "claude-fable-5"},
  "tokenCount": {"inputTokens": 2, "outputTokens": 497},
  "capabilityType": 15,
  "toolFormerData": {
    "tool": 15,
    "name": "run_terminal_command_v2",
    "toolCallId": "toolu_012XHqBKN3SbRtVsid4vorRj",
    "params": "{\"command\":\"grep -rn download --include=*.tsx -il app\"}",
    "rawArgs": "",
    "status": "completed",
    "result": "app/components/download-button.tsx\n"
  }
}
```

txcript's text form of a session is a JSON object with the header row's
columns, the `composer_data` cell, and the `bubbles` and `aux` rows as
`{key, value}` pairs, every cell still a string.

### Writing

Coming *from* Common, txcript synthesizes what the app's loader insists on:
a complete `composerData` document (started from a default-state template
captured from a fresh Cursor 3.16 composer — sparser documents fail with
"Failed to load composer data"), the full bubble skeleton, a ProseMirror
`richText` for each user turn, and a `fullConversationHeadersOnly` entry per
bubble whose `grouping` flags (`isRenderable` above all) the renderer needs
before it will open the conversation. Tool results are written back onto the
call's bubble, whose `createdAt` becomes the result time. Known tools get
their numeric ids back. An untitled session is named from its first user
turn, because the Agents sidebar hides unnamed sessions.

Writing a session is not enough for the app to list it. The Agents home
sidebar reads `glass.localAgentProjects.v1` (projects keyed by workspace
folder) and `glass.localAgentProjectMembership.v1` (composer id → project)
from `ItemTable`; a session absent from the membership map never appears. On
save txcript adds the session to the project for `Meta.cwd`, creating the
project when needed, and resolves the header's `workspaceId` from
`workspaceStorage/*/workspace.json`. Without a `cwd` the session is written
but not registered. Saves replace the session's bubbles wholesale inside one
transaction.

## Caveats

- **Reverse-engineered; expect drift.** Shapes are from Cursor 3.16 and
  3.17.8 (`composerData._v` 17, bubble `_v` 3). The loader's requirements —
  which fields of `composerData` and `grouping` it refuses to open without —
  were found by deletion bisection, not from a spec. A release that changes
  them breaks resume of imported sessions before it breaks reading.
- **Thinking is often signature-only.** On recent Cursor builds the
  `thinking.text` of an assistant bubble is empty and only the provider
  `signature` is stored. Those bubbles produce no `Thinking` block, so a
  session can look thought-free in Common while the app shows a thinking
  duration.
- **Only two tools are typed.** Shell and read calls become `Tool::Bash` and
  `Tool::Read`; every other native tool — search, glob, ask, MCP — stays
  `Tool::Raw`. Cursor's own edit tools have not been observed with a numeric
  id in real sessions and are not renamed. Sessions txcript imported carry
  canonical names (`Edit`, `Write`, `Bash`) with no `tool` number, and read
  back as typed tools.
- **Lossy through Common by design.** Image blocks, stop reasons, encrypted
  thinking, `Tool::Bash` `description`/`run_in_background`, and the
  distinction between a text result and a JSON-shaped text result (it comes
  back as `ToolOutput::Json`) are lost. Native load keeps every cell
  byte-for-byte, so a same-harness round trip is lossless.
- **The `agentKv` cache is deliberately not carried.** Its blobs are shared
  across sessions with no per-session root on disk. Cursor 3.16 lists,
  renders, and continues a session with every `agentKv` blob deleted, so the
  bubbles are the carrier.
- **One database for everything.** Discovery, load, and save all touch the
  same file the running app has open. Reads are read-only and safe;
  `save` and `delete` write into a live app's database, and the app may not
  notice until it reloads.
- **Older per-workspace stores are not read.** Earlier Cursor versions kept
  chat history in `workspaceStorage/<hash>/state.vscdb`; txcript reads only
  the global database.
- Hostile input: session ids are validated as path components before writes,
  and `delete()` removes the header row plus every `cursorDiskKV` key naming
  the id, failing if no header row existed.

## References

No public specification of `state.vscdb` exists. Cursor's chat history docs
(<https://cursor.com/docs/agent/chat/history>, accessed 2026-09-01) describe
the feature and not the storage. The community thread "Where are cursor chats
stored?" (<https://forum.cursor.com/t/where-are-cursor-chats-stored/77295>,
accessed 2026-09-01) confirms the global database path and nothing of its
schema. This document is reverse-engineered.

The authoritative mapping is `src/harness/cursor_desktop.rs`, exercised by
`tests/integration/cursor_desktop.rs`.

Last verified: 2026-09-01, against src/harness/cursor_desktop.rs and real local
sessions from Cursor 3.17.8.
