# DeepSeek Harness (`dsh`)

DeepSeek Harness stores sessions under:

```text
$DSH_HOME/sessions/              # default ~/.dsh/sessions
  --<normalized-cwd>--/          # or _no-cwd/
    <encoded-id>/
      session.v4.jsonl.zstd      # current: concatenated checksummed Zstandard frames
      session.v4.jsonl           # only when compression: 'none'
      session.jsonl.zstd         # older generations (v0 has no `.vN`) may remain
      session.lock
```

Home resolution is configured path, then `$DSH_HOME`, then `~/.dsh`. An empty
`$DSH_HOME` is treated as unset.

Each format generation has its own file name, `session[.vN].jsonl[.zstd]`,
where a missing `.vN` is version 0. When dsh opens an older session for
writing, it migrates it and publishes the new generation **beside** the old
one; the old file is kept byte-identical. The numerically highest generation is
the live log, and that is the one txcript reads. Lock files and in-flight
`.tmp` files are not logs.

Reading `.jsonl.zstd` needs txcript's `dsh_zstd` feature, on by default.
Without it (and in the wasm build), compressed sessions are left out of
discovery and loading one fails; uncompressed `.jsonl` logs still work.

## Native representation

`txcript::harness::dsh::DshSession` retains the first JSONL line as `header`
and every subsequent line as raw JSON values. Packed storage rows
(`text-chunks`, `reasoning-chunks`, `tool-call-chunks`, v0–v1 only) and
unknown event types stay in the native body so a text load/render round trip
does not drop bookkeeping the Common projection does not understand.

txcript reads generations 0 through 4 directly rather than migrating them.
The differences that matter to the Common projection are small:

| Version | Change that affects the reader |
| --- | --- |
| v1 | file renamed `session.v1.jsonl`; layout otherwise unchanged |
| v2 | top-level `assistant/chunk` and packed chunk rows gone (the stream is embedded in `assistant/message`); header `seedLength` replaced by `isSeeded` |
| v3 | system prompt becomes a `system/message` surface node; replace endpoints renamed `start`/`end` → `startSeq`/`endSeq` |
| v4 | `tool/result` becomes a tool-role message (no `tool-result` wrapper); message sources become direct `kind`s; `developer/message` surface node |

### Header

The first line is tagged `type: "session"` and carries `version`, `id`,
`createdAt` (epoch milliseconds), `delegationDepth`, `isSeeded` (v2+;
`seedLength` before), and optional `cwd`, `parentSession`, `origin`, and
`agentPreset`.

### Events

Each event is `{ type, seq, time, data, ... }`. Surface events
(`system/message`, `developer/message`, `user/message`, `assistant/message`,
`tool/result`) carry `surfaceOp` — `"append"`, or a replace op naming an
inclusive span — and, except on `assistant/message`, may carry
`sourceEventSeqs`. Log-only events (turn/step markers, `tool/call`,
`request/header`, inbox splices, delivery and title bookkeeping, …) never enter
the Common conversation.

## Common projection

| dsh event | Common representation |
| --- | --- |
| `user/message` with text parts | user text message, unless it is environment context (below) |
| `assistant/message` `reasoning` / `text` / `tool-call` | thinking / text / tool-use |
| `assistant/message` `usage` / `interrupted` | `Usage` / `StopReason::Aborted` |
| `tool/result` | user tool-result (`isError` kept), from the v4 tool-role message or the earlier wrapper block |
| `system/message`, `developer/message` | nothing, but they hold surface positions |

The ordered surface is rebuilt before projection. Surface nodes are tracked by
event `seq`, because log-only events sit between them and a node's seq is not
its surface position. A replace op names the inclusive span it shadows — both
endpoints must be on the current surface — and substitutes the replacing node
for that whole run; a range txcript cannot resolve shadows nothing. System and
developer messages are surface nodes too, so a span is counted across them even
though they produce no Common message.

dsh injects its own environment into the conversation as extra `user/message`s:
runtime and sandbox snapshots, the skill catalog, instruction files, clocks.
Those sources (`runtime-context`, `skill-catalog`, `agent-instructions`,
`time-context`, `tmux-context`, `repeat-tool-reminder`, `model-selection`,
`plan-mode`; before v4, the same names as `{ kind: "plugin", plugin }`, with the
system-prompt plugin's messages standing for `runtime-context`) are not
projected — they describe dsh, not the conversation, and would mislead another
harness the session continues in. Every other source is kept, unknown ones
included: a `compact-checkpoint` message is the only remaining copy of the
history it replaced.

`usage` maps `inputTokens`/`outputTokens` and the optional
`cacheReadTokens`/`cacheWriteTokens` onto Common's `Usage`. `reasoningTokens`
has no Common counterpart and survives in the native body only. Shadowed
surface nodes likewise stay in the native body, so a text round trip keeps the
full log even though Common shows the model-visible surface.

## Lineage

`parentSession` is the id of the session this one was seeded from. A subagent
run also records `origin: "subagent"` and becomes a Spawn `Meta.lineage`; a
user fork records no origin and becomes a Fork. dsh resumes a session under its
own id, so there is no continuation link. Lineage is filled at discovery as
well as on load.

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

Open source. Layout, event vocabulary, and the per-version changes follow the
DeepSeek Harness packages `@deepseek-ai/dsh-session`,
`@deepseek-ai/dsh-session-persistence-jsonl`, and the adjacent migration
packages `@deepseek-ai/dsh-session-format-v0-to-v1` through `-v3-to-v4`
(format version 4 at 0.2.0-rc.2; the project still marks the format a
developer preview and warns of compatibility-breaking changes).

The reader was checked against sessions recorded by dsh 0.2.0-rc.2 — a
tool-using session resumed for a second turn, and a subagent run with its
parent — and against a session recorded by dsh around 2026-08-14 in format v0,
read both as v0 and after the official catalog migrated it to v4: the two
generations project to identical Common messages.

Last verified: 2026-10-15, against `@deepseek-ai/dsh` 0.2.0-rc.2.
