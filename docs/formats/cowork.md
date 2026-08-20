# Cowork

Cowork is the Claude desktop app's local agent mode — the surface through
which people who never open a terminal run Claude on their own files. Under
the hood it is Claude Code: the app launches the CLI headlessly through the
Agent SDK, points it at a private config directory per task, and keeps its
own session record beside it. Nothing about the format is documented;
txcript's mapping was reverse-engineered from real sessions on this machine
(app versions writing CLI 2.1.5 through 2.1.234, January–August 2026) and
corroborated against the app's own session loader and record validator in
its bundled JavaScript.

```
~/Library/Application Support/Claude/local-agent-mode-sessions/   (macOS)
%APPDATA%\Claude\local-agent-mode-sessions\                       (Windows)
~/.config/Claude/local-agent-mode-sessions/                        (Linux)
└── <org-uuid>/<account-uuid>/                 one tree per signed-in account
    ├── local_<uuid>.json         ── session record (title, cwd, model, times)
    ├── local_<uuid>/             ── session storage directory
    │   ├── .claude/              ── the task's private CLAUDE_CONFIG_DIR
    │   │   └── projects/<encoded-cwd>/
    │   │       ├── <cliSessionId>.jsonl   ── the conversation (Claude Code JSONL)
    │   │       └── <cliSessionId>/subagents/*.jsonl   (not carried)
    │   ├── audit.jsonl           ── Agent SDK stream, HMAC-chained (carried, never written)
    │   ├── .audit-key            ── encrypted chain key (not carried)
    │   ├── uploads/, outputs/    ── the user's files (not carried)
    │   └── .claude/.claude.json, backups/, debug/   (not carried)
    ├── agent/local_ditto_*.json  ── agent-type sessions, same shape, own subdir
    └── cowork_settings.json, rpm/, debug/, …        (app state, skipped)
```

## On disk

The root is the app's data directory plus `local-agent-mode-sessions`,
overridable wholesale with `$COWORK_SESSIONS_DIR`. The first two levels are
the organisation and account UUIDs; txcript recognises an account tree by
both names parsing as UUIDs, which is what separates it from the app's other
state at the same level (`skills-plugin/`). Inside an account tree a session
is a `local_*.json` record plus a same-named directory, in the tree itself
or in its `agent/` subdirectory. Discovery lists the records (the app does
exactly the same: `readdir`, filter `local_*.json`, validate) and reads each
one plus a shallow scan of its transcript for metadata.

The record names the Claude Code session under it (`cliSessionId`); the
transcript is `<storage dir>/.claude/projects/<slug>/<cliSessionId>.jsonl`,
found under whichever project slug holds it — the slug is Claude Code's
encoding of the cwd, which for the app's long storage paths is truncated
with a hash suffix, so it is never reconstructed, only searched. Sessions
from early 2026 ran inside the app's Linux VM with `cwd: /sessions/<name>`;
later ones run on the host (`hostLoopMode: true`) with the cwd inside the
storage directory's `outputs/`. Both shapes load the same way.

`save` writes into the most recently active account tree (the one whose
newest record is youngest), creating the record, the storage directory with
its `.claude/projects/<encoded-cwd>/<cliSessionId>.jsonl`, `outputs/` and
`uploads/`, and `audit.jsonl` only when the body carries one. A root with no
account tree is an error: the app has never run there, so there is nowhere
it would look.

## Dissection of a transcript

| Their name | What it is | Maps to |
|---|---|---|
| `local_<id>.json` | The app's session record. Required by its validator: `sessionId`, `processName`, `cwd`, `createdAt`, `lastActivityAt`; typed here alongside `cliSessionId`, `model`, `title`, `isArchived`; everything else (`systemPrompt`, `enabledMcpTools`, `egressAllowedDomains`, permission grants, …) passes through untouched | `Meta.id`, `Meta.timestamp` (`createdAt`), `Meta.cwd`, `Meta.title`, `Meta.model` |
| `<cliSessionId>.jsonl` | Claude Code's session JSONL, exactly as documented in [claude-code.md](claude-code.md): `user`/`assistant` entry lines wrapping Anthropic messages, plus bookkeeping | The conversation, through the `claude_code` codec; `Meta.cli_version` and `Meta.git_branch` come from its envelopes |
| `queue-operation`, `last-prompt`, `attachment`, `ai-title` lines | Cowork/SDK bookkeeping in the transcript (prompt queueing, deferred-tool and skill listings, the generated title) | Nothing; kept verbatim in the native body |
| `user` line with `isMeta: true` | Context the app injects for the model — typically the pages of an uploaded PDF rendered as images | A `Role::User` message of `Image` blocks (it is what the model saw) |
| `<uploaded_files>…</uploaded_files>` prefix | The app's attachment manifest at the head of a prompt | Kept as prompt text: it names the files, it is not boilerplate |
| `audit.jsonl` | The Agent SDK message stream (`system/init`, `user`, `assistant`, `rate_limit_event`, `result`), every line signed into an HMAC chain | Nothing; carried verbatim, never written |

Tool names are Claude Code's (`Read`, `Edit`, `Bash`, …) plus the app's
MCP servers (`mcp__workspace__bash`, `mcp__cowork__present_files`,
`mcp__Claude_in_Chrome__*`), which pass through as `Raw` calls like any
`mcp__*` tool.

Regeneration writes a record with every required field: `processName` is
synthesised (`txcript-<8 hex>`), `hostLoopMode` is `true` so the app runs the
CLI on the host against `cwd`, `initialMessage` is the first prompt's text,
and `lastActivityAt` the last message's time. The Claude Code transcript is
stamped with a `cliSessionId` derived as UUIDv5 of the session id, so the
same conversation always yields the same files. A session id without the
`local_` prefix gets one — the app lists nothing else.

## Caveats

- The app's record validator silently drops any `local_*.json` it cannot
  parse, so a missing required field makes a session invisible rather than
  broken. The required set above comes from the validator itself.
- The app reads the records once at startup and afterwards tracks only the
  sessions it creates itself; a session written while it is running appears
  after a quit and relaunch. There is no per-session launcher or deep link —
  `claude://resume?session=` imports a Claude Code session into the app's
  Claude Code surface, not Cowork — so `txcript continue` ends with
  `open -a Claude`.
- `audit.jsonl` is a tamper-evident chain keyed through Electron's
  `safeStorage`; txcript cannot sign entries and does not try. The app
  treats unsigned and absent logs as valid.
- `to_common` inherits every Claude Code caveat (local-command envelopes,
  non-Anthropic stop reasons, tool-result shapes) and adds none. Subagent
  transcripts are not carried.
- Thinking blocks in the on-disk transcript can be empty strings with only
  a signature (the SDK stream redacts them); they load as empty `Thinking`
  blocks, as they would from Claude Code.
- Session start is the record's `createdAt`, which precedes the first
  transcript line by the CLI's start-up time.
- Regenerating from Common loses what `claude_code` loses; the record's
  non-conversational state (system prompt, MCP settings, permission grants)
  is not reconstructed — the app regenerates it on launch.

## References

- Reverse-engineered from sessions under
  `~/Library/Application Support/Claude/local-agent-mode-sessions`, written
  by Claude desktop app builds embedding Claude Code 2.1.5–2.1.234.
- The record schema and loader (`LocalAgentModeSessionManager`) were read
  from the app bundle (`app.asar`, `.vite/build/index2.chunk-*.js`) of the
  installed desktop app on 2026-08-20.
- Authoritative mapping: `src/harness/cowork.rs` (module docs and code) and
  `tests/integration/cowork.rs` (fixtures shaped like real sessions).

Last verified: 2026-08-20, against src/harness/cowork.rs and real local sessions.
