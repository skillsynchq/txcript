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
| `assistant/message` `usage` / `interrupted` | `Usage` / `StopReason::Aborted` |
| `tool/result` | user tool-result (`isError` kept) |

The ordered surface is rebuilt before projection. Surface nodes are tracked by
event `seq`, because log-only events sit between them and a node's seq is not
its surface position. A `replace` `surfaceOp` names the inclusive **seq** range
it shadows — both endpoints must be on the current surface — and substitutes
the replacing node for that whole run; a range txcript cannot resolve shadows
nothing. Packed chunk rows and `assistant/chunk` stream events are ignored for
Common because the assembled `assistant/message` already carries the step.

`usage` maps `inputTokens`/`outputTokens` and the optional
`cacheReadTokens`/`cacheWriteTokens` onto Common's `Usage`. `reasoningTokens`
has no Common counterpart and survives in the native body only. Shadowed
surface nodes likewise stay in the native body, so a text round trip keeps the
full log even though Common shows the model-visible surface.

## Store capabilities

The store reads and writes. dsh ships no session-import command, but it does
not need one: it discovers sessions by walking its root, so writing one is a
matter of reproducing the layout it scans for.

What makes that exact rather than approximate is dsh's validation. Three of its
checks fail the **entire** listing rather than skipping the one bad session, so
each is a hard requirement on the writer:

| Check | Requirement |
| --- | --- |
| `assertZstdHeaderFrame` | the first Zstandard frame decodes to exactly one line — the header |
| `assertStoredIdentity` | the header's own `id` and `cwd` name the path the log was found at |
| `checkRootEncoding` | one root never mixes `.jsonl` with `.jsonl.zstd` |

A duplicate session id across two project directories is rejected the same way.

### Layout derivation

- **Project directory** — `--<key>--`, where `key` collapses each run of `/`,
  `\`, or `:` to a single `-`, keeps `[A-Za-z0-9._-]`, escapes every other
  UTF-16 code unit as `~XXXX`, strips leading dashes, falls back to `root` if
  nothing remains, and truncates to 251 characters. A session with no cwd goes
  under `_no-cwd`.
- **Session directory** — the id under the same `~XXXX` escape, with `.` and
  `..` special-cased whole. This is what contains a traversing id: escaping the
  separators turns `../../evil` into the literal directory
  `..~002F..~002Fevil`.

Escaping operates on UTF-16 code units, not Unicode scalars, which is what
makes it injective over lone surrogates.

### What `save` writes

`<root>/<project>/<encoded id>/session.jsonl.zstd` — a checksummed Zstandard
frame holding the header line, then one holding the event lines. The header is
stamped with the id and cwd that built the path, because a copy given a new
identity would otherwise keep pointing at the original's.

The physical encoding follows whatever the root already uses; only an empty
root falls back to dsh's own default of `zstd`. Re-saving a session whose cwd
changed removes the copy under the old project directory, since leaving it
would be the duplicate-id corruption above.

`delete` removes the session directory. dsh's persistence seam has no delete
API, but it also keeps no index — an absent directory is simply not scanned.

The native resume command documented for a TUI profile is
`dsh --profile tui --resume <id>`.

## Provenance

Open source. Layout and event vocabulary follow the DeepSeek Harness
packages `@deepseek-ai/dsh-session` and
`@deepseek-ai/dsh-session-persistence-jsonl` (session format version 0,
developer preview; the project warns of compatibility-breaking changes).
The path derivation, frame layout, and validation rules above are that
package's own `encodeSegment`, `projectKey`, `encodeMaterialization`, and
`listArtifacts`.

The reader was checked against a local `session.jsonl.zstd` written by dsh
around 2026-08-14. The writer was checked by running the official backend's
`listArtifacts` and `loadStored` against a txcript-written root: it lists the
session and decodes all 1148 of its event records. Compressing the log as one
frame instead of two — which txcript itself still reads — makes that same check
fail with dsh's `first frame is not exactly one header line`.

Last verified: 2026-09-07, against `@deepseek-ai/dsh-session-persistence-jsonl`
0.0.1-rc.1.
