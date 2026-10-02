# uji

uji keeps every session in one `SQLite` database, `uji.db`. Each session is
a row in `sessions`, and each entry in its transcript is a row in
`messages`, whose `data` column holds the entry as JSON. txcript's portable
text form for uji is the session row with its message rows nested in a
`messages` array, each row's `data` decoded. Provenance is **open source**:
the format is read from uji's storage and agent code, pinned below.

```
~/.local/share/uji/uji.db                one database for every session
├─ sessions                              one row per session
│    id, title, directory,               metadata; parent is set for
│    time_created, time_updated, parent  sessions another session started
├─ messages                              ordered rows per session
│    id, session_id, seq, type,          data is the entry as JSON
│    time_created, data
└─ settings                              uji's own settings, untouched
```

## On disk

The database resolves the way uji resolves it: `$UJI_DB`, else `uji.db` in
`$UJI_DATA_DIR`, else in `$XDG_DATA_HOME/uji` when that path is absolute,
else in `~/.local/share/uji`. Discovery lists every `sessions` row, including
sessions another session started, which carry their `parent` as
`Relation::Spawn` lineage. Loading reads the session's `messages` rows in
`seq` order. Times are milliseconds since the Unix epoch.

Saving writes a new session and its rows in one transaction, replacing any
session with the same id. `uji resume --id` accepts only UUID-shaped ids, so
a transcript whose id is not a UUID is saved under a fresh one, and the
returned id is the one to resume. Only uji creates and migrates the
database, so saving into one that does not exist yet fails and asks you to
start uji once. Delete removes the session and its rows.

## Dissection of a transcript

| Their name | What it is | Maps to |
|---|---|---|
| `sessions` row | `id`, `title` (`untitled` until uji names it), `directory`, `time_created`, `parent` | `Meta`; `untitled` becomes no title |
| `user`, `context` | `text`, and `images` with `media_type` and base64 `data` | `Role::User` with `Block::Text` and `Block::Image` |
| `assistant` | `text`, `reasoning`, `tool_calls` with `id`, `name` and JSON-string `arguments`, and `replay` | `Role::Assistant` with `Block::Thinking`, `Block::Text` and `Block::ToolUse` |
| `tool` | `tool_call_id`, `name`, `content`, `images` | `Role::User` with `Block::ToolResult`, followed by any images |
| `compaction` | `summary` of the messages before it | `Role::User` with the summary as `Block::Text` |
| `shell`, `error`, `system` | `!` commands, turn errors, and notes | kept in the native body, no conversational turn |

Tool names map to the Claude convention and back: `read_file` ⇄ `Read`,
`write_file` ⇄ `Write` and `edit_file` ⇄ `Edit`, with `path` ⇄ `file_path`,
and `run_command` ⇄ `Bash`, with `timeout` in seconds ⇄ `timeout_ms`. Other
names, such as plugin and MCP tools, pass through as `Tool::Raw`. A tool
result is an error when its content starts with `error:` or `denied:`, which
is how uji words failed and refused calls.

`replay` holds what a provider needs back on a later turn, tagged with the
API and the model it came from:

| `replay.api` | Contents | Maps to |
|---|---|---|
| `anthropic` | the reply's content blocks, in order: `thinking` with its `signature`, `redacted_thinking` with its `data`, `text`, and `tool_use` | one `Block::Thinking` per thinking block, with `signature`, or with `encrypted` for redacted thinking |
| `responses` | the reply's output items: `reasoning` items with their encrypted content, and `message` and `function_call` items | one `Block::Thinking` per reasoning item, with the item as JSON in `encrypted` |

Writing rebuilds `replay` from those blocks, using the message's model, so
uji sends signed thinking and encrypted reasoning back when it continues with
the same model. Without them, the thinking text is written as `reasoning`.

## Caveats

- An assistant row has one `text`, so several text blocks in one message
  join with blank lines, and text written after a tool call moves before it.
- uji stores no stop reason or per-message token usage, so none survive a
  hop through uji.
- A user-side slash command (`Tool::Command`) and its output have no slot in
  uji and are left out.
- Gemini's per-call thought signatures have no slot in `Common`, so they do
  not survive a hop out of uji.
- Images keep their type and data. uji's own `width`, `height` and `name`
  are not carried into `Common`.

## References

- Schema and migrations:
  [`lua/uji/core/store/init.lua`](https://github.com/uji-labs/uji/blob/3e8bcc1bad3c852e5409ed3246cda2c17b9083e3/lua/uji/core/store/init.lua)
- Message rows and their decoding:
  [`lua/uji/core/store/session.lua`](https://github.com/uji-labs/uji/blob/3e8bcc1bad3c852e5409ed3246cda2c17b9083e3/lua/uji/core/store/session.lua)
- Assistant entries and `replay`:
  [`lua/uji/core/loop.lua`](https://github.com/uji-labs/uji/blob/3e8bcc1bad3c852e5409ed3246cda2c17b9083e3/lua/uji/core/loop.lua)
- Database location:
  [`lua/uji/core/paths.lua`](https://github.com/uji-labs/uji/blob/3e8bcc1bad3c852e5409ed3246cda2c17b9083e3/lua/uji/core/paths.lua)

The parser (`src/harness/uji.rs`) and the integration tests
(`tests/integration/uji.rs`, including a real temp `SQLite` database with
uji's schema) are the normative mapping.

Last verified: 2026-10-02.
