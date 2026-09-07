# Kimi Code

Kimi Code CLI stores sessions under:

```text
~/.kimi-code/sessions/wd_<workspace>_<hash>/session_<uuid>/
    state.json
    agents/<agent-name>/wire.jsonl
```

`state.json` contains session metadata. Each agent has an append-only wire
log; `agents/main/wire.jsonl` is the main conversation. Other agent logs are
separate conversations and are not merged into the main transcript.

## Native representation

`txcript::harness::kimi::KimiSession` retains `state.json` as a JSON value and
the main wire log as a JSONL value list. Unknown Kimi event types remain in the
native body, so a native text load/render round trip does not discard events
that txcript does not understand.

The `createdAt` field is accepted as either an RFC3339 string or epoch
milliseconds. Kimi versions have emitted both forms.

### Session id and schema versions

`state.json` has two observed shapes. Schema version 2 (`"version": 2`) records
the session id in `id` and the working directory in `cwd`. Version 1 carries no
`version` marker, uses `workDir`, and has no id field at all. Kimi's own
`session_index.jsonl` calls the same value `sessionId`.

The id is therefore resolved in order: `sessionId`, `id`, then the
`session_<uuid>` segment of `agents.<name>.homedir` — an absolute path every
observed schema records. The store falls back to the session directory name.
The homedir fallback matters for `from_text` and the wasm parser, which see the
JSON without its path.

Discovery is gated on structure — a readable `state.json` plus
`agents/main/wire.jsonl` — never on the directory name, so a Kimi release that
renames its session directories still lists.

## Common projection

The following wire events become conversational blocks:

| Kimi event | Common representation |
| --- | --- |
| `context.append_message` with `role=user` | user text message |
| `content.part` with `part.type=text` | assistant text block |
| `content.part` with `part.type=think` | assistant thinking block |
| `tool.call` | assistant tool-use block |
| `tool.result` | user tool-result block |
| `context.undo` | rewinds the last `count` turns |

Tool IDs and arguments are retained. `isError` maps to the Common tool-result
error flag. A Kimi result note is appended to textual output so truncation or
permission annotations are not silently lost. Usage, timing, permission,
MCP-tool-snapshot, and step-bookkeeping events are not fabricated as messages.

### Rewound context

Kimi rewinds its context with `context.undo` after a failed or cancelled turn,
then re-sends the prompt as a fresh `turn.prompt`. Because `wire.jsonl` is
append-only, the rolled-back entries stay on disk. Replaying them would
resurrect prompts the user already retried — a session that hit ten provider
errors in a row reads back with the same prompt ten times — so the reader
applies the rewind.

`count` is measured in turns, not entries: one turn can append several
messages (a prompt plus injected reminders), and a single `count: 1` undo drops
all of them. A wire log with no `turn.prompt` markers falls back to entry
granularity.

## Store capabilities

The store reads and writes. Kimi ships no import command, but it does not need
one: sessions are loaded from whatever `session_index.jsonl` points at, so
writing one is a matter of laying out the files Kimi expects.

### The session index

`<data root>/session_index.jsonl` is an append-only log, one JSON record per
line, sitting one level above `sessions/`:

```json
{"sessionId": "session_<uuid>", "sessionDir": "/abs/path", "workDir": "/abs/cwd"}
{"sessionId": "session_<uuid>", "deleted": true}
```

**It is the only discovery path.** Kimi does not scan the sessions directory,
so a session written without an index record is invisible to `kimi session
list` and `kimi --session`. Removal is a `deleted` tombstone rather than a
rewrite, which is how txcript's `delete` retires a session too.

Kimi validates each record on read: `sessionDir` must be absolute, must sit
inside the sessions directory, and its last path segment must equal
`sessionId`.

### Workspace directory names

A session directory lives under `sessions/wd_<slug>_<hash>/`, where `slug` is
the working directory's last path segment — lowercased, every run of characters
outside `[a-z0-9._-]` collapsed to `-`, trimmed of leading and trailing
dashes, capped at 40 characters — and `hash` is the first 12 hex characters of
`sha256(workDir)`, with the path normalized to forward slashes and no trailing
slash.

Getting this name wrong does not hide a session, because the index points at it
directly, but it does break Kimi's own `--cwd` filtering and `kimi -c`, both of
which resolve a working directory to this exact name.

### What `save` writes

- `sessions/wd_<slug>_<hash>/<session id>/state.json`
- `sessions/wd_<slug>_<hash>/<session id>/agents/main/wire.jsonl`
- one appended record in `<data root>/session_index.jsonl`

An id that is not usable as a single path component is rejected before
anything is written. A session converted from another harness has no Kimi
`state.json`, so `save` fills in the identity fields Kimi and txcript's
directory-free `from_text` read back: `sessionId`, `workDir`, `title`,
`createdAt`, and `updatedAt`. The last one is not decorative — `kimi session
list` renders its timestamp column from `updatedAt` alone, so a session written
without it lists at the Unix epoch.

`agents.main.homedir` is rewritten rather than preserved, because it names
where the session actually is: Kimi resolves the wire log through it, and
txcript's directory-free readers recover the session id from it. A session
saved under a second root would otherwise keep pointing at the first. Saving is
therefore idempotent rather than byte-preserving on `state.json` — a
`load → save → load` round trip is stable from the first save onward.

The native Kimi resume command is `kimi --session <id>`.

## Provenance

**Reverse-engineered.** The wire protocol comes from sessions observed locally
and the CLI surface documented by `kimi --help`. The index contract and the
workspace-name derivation come from the shipped `kimi` binary's own
`readSessionIndex` and `encodeWorkDirKey`, and were confirmed end to end
against an isolated `KIMI_CODE_HOME`: a session written by txcript is listed by
`kimi session list`, and `kimi export` reports a `sessionFirstActivity` derived
from the `time` field in the written wire log — so Kimi parses the events
back, not just the directory. The derived workspace names reproduce the
existing directory names of real local sessions exactly.

The wire protocol is an implementation detail and may change between Kimi
releases; unknown events are retained to make the reader fail conservatively
rather than silently discarding native data.

Last verified: 2026-09-07, against Kimi Code 0.41.0 (wire protocol 1.5) and
real local sessions.
