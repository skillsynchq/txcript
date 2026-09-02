# DeepSeek Harness (`dsh`)

DeepSeek Harness stores sessions under:

```text
$DSH_HOME/sessions/          # default ~/.dsh/sessions
  --<normalized-cwd>--/      # or _no-cwd/
    <encoded-id>/
      session.jsonl.zstd     # default: concatenated checksummed Zstandard frames
      session.jsonl          # only when compression: 'none'
```

Home resolution is configured path, then `$DSH_HOME`, then `~/.dsh`. An empty
`$DSH_HOME` is treated as unset.

## Native representation

`txcript::harness::dsh::DshSession` retains the first JSONL line as `header`
and every subsequent line as raw JSON values. Packed storage rows
(`text-chunks`, `reasoning-chunks`, `tool-call-chunks`) and unknown event
types stay in the native body so a text load/render round trip does not drop
bookkeeping the Common projection does not understand.

The official on-disk format version is `0`. There is no migration; txcript
still loads the native body when the version field differs.

### Header

The first line is tagged `type: "session"` and carries `version`, `id`,
`createdAt` (epoch milliseconds), optional `cwd`, `parentSession`,
`seedLength`, `origin`, `delegationDepth`, and `agentPreset`.

### Events

Each event is `{ type, seq, time, data, ... }`. Surface events
(`user/message`, `assistant/message`, `tool/result`) may also carry
`surfaceOp` (`"append"` or `{ op: "replace", start, end }`) and
`sourceEventSeqs`. Log-only events (turn/step markers, `assistant/chunk`,
`request/header`, packed chunk rows, …) never enter the Common conversation.

## Common projection

| dsh event | Common representation |
| --- | --- |
| `user/message` with text parts | user text message |
| `assistant/message` `reasoning` / `text` / `tool-call` | thinking / text / tool-use |
| `tool/result` | user tool-result (`isError` kept) |

The ordered surface is rebuilt before projection: a `replace` `surfaceOp`
truncates earlier surface nodes. Packed chunk rows and `assistant/chunk`
stream events are ignored for Common because the assembled `assistant/message`
already carries the step.

## Store capabilities

DeepSeek Harness has no documented session-import CLI. The persistence seam
also has no delete API. The txcript dsh store is therefore **read-only**:

- `list`, `query`, `view`, and `export` work;
- a dsh session can be converted into any writable target harness;
- `save` and `delete` return an explicit read-only error;
- txcript never writes directly into `~/.dsh/sessions`.

The native resume command documented for a TUI profile is
`dsh --profile tui --resume <id>`. Cross-harness continuation is supported;
native dsh continuation is refused.

## Provenance

Open source. Layout and event vocabulary follow the DeepSeek Harness
packages `@deepseek-ai/dsh-session` and
`@deepseek-ai/dsh-session-persistence-jsonl` (session format version 0,
developer preview; the project warns of compatibility-breaking changes).
The reader was also checked against a local `session.jsonl.zstd` written by
dsh around 2026-08-14.

Last verified: 2026-08-29
