# pi

pi is Mario Zechner's open-source coding agent, developed in the pi monorepo
(github.com/earendil-works/pi, formerly `badlogic/pi-mono`; npm
`@earendil-works/pi-coding-agent`, formerly `@mariozechner/pi-coding-agent`).
The upstream repo ships both the code that writes sessions and an explicit
format document, so this doc's provenance is **open-source code as spec**:
everything below is drawn from the upstream source and its
`session-format.md`, cross-checked against real local sessions and txcript's
parser in `src/harness/pi.rs`.

```
~/.pi/agent/sessions/
└── --Users-alice-src-myproj--/                 one dir per project cwd
    └── 2026-01-02T03-04-05-000Z_<uuid>.jsonl   one file per session
        │
        ├─ {"type":"session", ...}              header, always line 1
        ├─ {"type":"model_change", ...}         bookkeeping ┐ tree nodes,
        ├─ {"type":"message", "message":{...}}  turns       │ chained via
        ├─ {"type":"message", ...}              turns       │ id/parentId
        └─ {"type":"custom_message", ...}       extensions  ┘
```

## On disk

The sessions root resolves in this order (see `pi::resolve_sessions_dir`,
which matches upstream `config.ts`):

1. `PI_CODING_AGENT_SESSION_DIR` — used as the sessions dir verbatim;
2. `PI_CODING_AGENT_DIR` — agent dir override, sessions in `<dir>/sessions`;
3. default `~/.pi/agent/sessions`.

A leading `~` in either env var expands to the home directory. Under the
root, each project cwd gets one directory: strip the leading path separator,
replace `/`, `\`, and `:` with `-`, and wrap the result in `--…--` (so
`/Users/alice/src/myproj` becomes `--Users-alice-src-myproj--`). Each session
is a single JSONL file named `<timestamp>_<session-uuid>.jsonl`, where the
timestamp is the session's ISO-8601 start time with `:` and `.` replaced by
`-`. pi appends a line per event; there is no sidecar state.

txcript's discovery (`PiStore::discover`) walks the root recursively for
`*.jsonl` (skipping symlinked directories to avoid cycles) and accepts a file
only if its first parsable line is a `session` header. Discovery parses just
the meta-bearing line types — `session`, `model_change`, `session_info` —
and never builds message payloads.

## Dissection of a transcript

Every line is a JSON object tagged by `type`. Lines after the header carry
`id`, `parentId`, and an ISO `timestamp`, forming a tree (branching happens
in place, in the same file). Conversational content lives in `message` lines
under a `message` payload whose `role` discriminates further.

| Their name | What it is | Maps to |
|---|---|---|
| `session` (line 1) | Header: `id`, `version` (3), `timestamp`, `cwd`, optional `parentSession` | `Meta.id`, `Meta.timestamp`, `Meta.cwd` |
| `message` / role `user` | User turn; `content` is a string or array of `text`/`image` blocks | `Role::User` with `Block::Text` / `Block::Image` |
| `message` / role `assistant` | Model turn; `content` blocks plus `model`, `provider`, `api`, `stopReason`, `usage` | `Role::Assistant`; `Message.model`, `.stop_reason`, `.usage` |
| content block `toolCall` | Tool invocation: `id`, `name`, `arguments` | `Block::ToolUse`, tool normalized to canonical names |
| `message` / role `toolResult` | Tool outcome: `toolCallId`, `toolName`, `content`, `isError` | `Role::User` turn with `Block::ToolResult` |
| `message` / role `bashExecution` | A `!cmd` the user ran in pi's shell: `command`, `output`, `exitCode`, `excludeFromContext` | Synthetic `ToolUse(Bash)` + `ToolResult` pair, id `bash_exec_N` |
| `custom_message` | Extension-injected context that replays as a turn | `Role::User` turn |
| `model_change` | Model switch: `modelId`, `provider` | `Meta.model` (latest wins) |
| `session_info` | User-assigned session name: `name` | `Meta.title` |
| `thinking_level_change`, `compaction`, `branch_summary`, `label`, `custom` | Bookkeeping: reasoning level, context compaction, abandoned-branch summaries, bookmarks, extension state | `Record::Other` — preserved verbatim, no conversational turn |

Threading: entries chain leaf-ward via `id`/`parentId`; pi uses the tree for
in-place branching and context rebuilding. txcript reads records in file
order, which is correct for the common single-branch session. Tool calls
pair with results by `toolCallId`. Tool names normalize both ways: `bash` ↔
`Bash`, `read`/`write` ↔ `Read`/`Write` (`path` ↔ `file_path`), `edit` ↔
`Edit`/`MultiEdit` (`oldText`/`newText` ↔ `old_string`/`new_string`, one
edit vs many), `find` ↔ `Glob`, `ls` ↔ `LS`, `grep` ↔ `Grep`; unknown and MCP
tool names pass through untouched. `stopReason` maps `stop`/`length`/`toolUse`/`error`/
`aborted` onto the Common `StopReason` set; `usage` fields are `input`,
`output`, `cacheRead`, `cacheWrite`.

A synthetic assistant line, shaped like the real thing:

```json
{"type": "message", "id": "a1b2c3d4", "parentId": "e5f6a7b8",
 "timestamp": "2026-01-02T03:04:07.000Z",
 "message": {"role": "assistant",
   "content": [{"type": "thinking", "thinking": "check the file first"},
               {"type": "toolCall", "id": "call-1", "name": "read",
                "arguments": {"path": "/repo/src/main.rs"}}],
   "api": "anthropic-messages", "provider": "anthropic",
   "model": "claude-opus-4-8",
   "usage": {"input": 10, "output": 20, "cacheRead": 5, "cacheWrite": 2},
   "stopReason": "toolUse", "timestamp": 1767323047000}}
```

## Caveats

- **Branches flatten.** txcript does not walk the `id`/`parentId` tree; a
  session with in-place branches replays all branches in file order.
  Likewise `compaction` entries are preserved but not applied, so txcript
  shows the full pre-compaction history rather than pi's rebuilt context.
- **Version drift.** The format is at `version: 3` (v1 was linear, v2 added
  the tree, v3 unified message roles). pi migrates old files on load;
  txcript targets v3 and keeps unknown shapes as `Record::Other`.
- **Native round-trip is lossless; Common is not.** Every line — including
  bookkeeping — survives native load/save byte-for-byte in meaning. Going
  through Common drops what pi can't express (assistant images, user
  thinking), re-derives entry ids deterministically (8 chars, matching pi's
  truncated-uuid convention), guesses `provider`/`api` from the model id,
  zeroes cost, and collapses `StopSequence`/`Other` stop reasons to `"stop"`.
- **bashExecution honors pi's context rule**: runs flagged
  `excludeFromContext` (pi's `!!` prefix) produce no turn.
- **Two timestamps per line.** The envelope carries ISO-8601, the message
  payload an epoch-millis `timestamp`; txcript reads the envelope and falls
  back to the session's start time.
- **Hostile input**: non-JSON lines are skipped; a known-tag line that fails
  its schema is kept whole as `Record::Other`; the session id is validated
  as a path component before it names a file on save.

## References

- Format doc: <https://github.com/earendil-works/pi/blob/cd6852a123f2c0cc646a41a2a52f3711a603b822/packages/coding-agent/docs/session-format.md>
- Path/dir scheme (cwd encoding, file naming): <https://github.com/earendil-works/pi/blob/cd6852a123f2c0cc646a41a2a52f3711a603b822/packages/coding-agent/src/core/session-manager.ts>
- Env/dir resolution and branding indirection: <https://github.com/earendil-works/pi/blob/cd6852a123f2c0cc646a41a2a52f3711a603b822/packages/coding-agent/src/config.ts>
- Append-only JSONL writer: <https://github.com/earendil-works/pi/blob/cd6852a123f2c0cc646a41a2a52f3711a603b822/packages/agent/src/harness/session/jsonl/storage.ts>

The authoritative txcript mapping is `src/harness/pi.rs`, exercised by
`tests/integration/pi.rs` (store fidelity, discovery, tool normalization,
codec fixpoint).

Last verified: 2026-08-10, against src/harness/pi.rs and real local sessions.
