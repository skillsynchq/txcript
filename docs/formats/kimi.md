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

Kimi Code has no documented session import or deletion command. The txcript
Kimi store is therefore **read-only**:

- `list`, `query`, `view`, and `export` work;
- a Kimi session can be converted into any writable target harness;
- `save` and `delete` return an explicit read-only error;
- txcript never writes directly into `~/.kimi-code/sessions`.

The native Kimi resume command is `kimi --session <id>`. Cross-harness
continuation is supported; native Kimi continuation is refused because the
store cannot safely create a Kimi session without an official import contract.

## Provenance

The format is based on the Kimi Code CLI wire protocol observed in local
sessions and the public CLI interface documented by `kimi --help`. The wire
protocol is an implementation detail and may change between Kimi releases;
unknown events are retained to make the reader fail conservatively rather than
silently discarding native data.
