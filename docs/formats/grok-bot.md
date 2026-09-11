# Grok Bot

Grok Bot is xAI's desktop assistant (distinct from the [Grok CLI](grok.md) /
Grok Build coding agent). It is closed source and publishes no transcript
schema; this document is **reverse-engineered** from observed
`agent-transcripts` JSONL and agent store DBs on a Grok Bot box
(2026-09-10…11). The authoritative mapping is `src/harness/grok_bot.rs`.

```
agent-transcripts/
└── <agent-uuid>/
    ├── <agent-uuid>.jsonl .......... append-only conversation (txcript reads this)
    └── <agent-uuid>.journal-mode ... tiny sidecar (ignored)
sand-subagent-<uuid>/ ............... same shape (subagent transcripts)

agents/<agent-uuid>/
├── store.db ........................ kv + transcript_entries (UI ledger)
└── conversation-blobs.db ........... encrypted conversation blobs
```

## On disk

Session id = agent / directory uuid. Roots:

| Path | Env override | Role |
|---|---|---|
| `$HOME/agent-data/agent-transcripts` | `TXCRIPT_GROK_BOT_ROOT` / `GROK_BOT_TRANSCRIPTS` | JSONL codec (preferred read/write) |
| `$HOME/agent-data/agents` | `TXCRIPT_GROK_BOT_AGENTS` | `profile.json` + `store.db` UI ledger |

**Discovery** unions:

1. Every `agents/<uuid>/` that has `profile.json` (title = profile `name`).
2. Every `agent-transcripts/<id>/` with `<id>.jsonl` whose first record is a
   `role`/`message` envelope (`user` / `assistant` / `tool`) — including
   `sand-subagent-*` dirs that have no profile. Journal-mode sidecars are
   ignored. Unreadable dirs are skipped.

When both sources exist for the same id, the profile `name` wins as the list
title; the transcripts path is preferred as the locator (safe JSONL delete).

**Load** cascade (first hit wins):

1. `agent-transcripts/<id>/<id>.jsonl` — full codec fidelity.
2. Non-empty `agents/<id>/store.db` `transcript_entries` — reconstructed into
   Common via `ui_entries_to_common` (text turns only), then `from_common`.
3. Local gateway `POST /api/openAgent` with `{id}` (Bearer from
   `gateway.json` / `TXCRIPT_GROK_BOT_GATEWAY`+`TOKEN`) — same UI ledger shape
   as `transcript_entries`. Used when the on-disk ledger is empty (common for
   long-lived temporal bots).
4. Otherwise a clear error.

Minted bots usually have JSONL + seeded `transcript_entries`. Live bots such
as Marcus may have profile + empty ledger + no JSONL; conversation content is
only available through `openAgent`.

## Dissection of a transcript

There is no session header. Each line is one JSON object:

| Their name | What it is | Maps to |
|---|---|---|
| record `role: "user"` | user prompt; text often prefixed `[t0u]\n` | `Message { role: User }` (prefix stripped in Common) |
| record `role: "assistant"` | model text and/or `tool_use` / `thinking` blocks | `Message { role: Assistant }` |
| record `role: "tool"` | `tool_result` blocks | `Message { role: User }` with `ToolResult` |
| block `text` | plain text | `Block::Text` |
| block `tool_use` | `{name, input}`; `toolCallId` may live inside `input` | `Block::ToolUse` (id from `toolCallId` or synthetic UUIDv5) |
| block `tool_result` | `{name, result: {success\|failure}}` | `Block::ToolResult`; paired FIFO by tool name |

Tool name mapping: `shell` → `Bash` (`timeout` → `timeout_ms`), `read` →
`Read` (`path` → `file_path`); denormalize is the inverse. Other tools
(`send_message`, `web_search`, `web_fetch`, `get_mcp_tools`, `update_todos`,
`communicate_update`, …) pass through as `Tool::Raw`. `send_message` often
duplicates the preceding assistant text — both are kept (no dedupe).

A minimal synthetic session:

```json
{"role":"user","message":{"content":[{"type":"text","text":"[t0u]\nHello"}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"Hi."}]}}
{"role":"assistant","message":{"content":[{"type":"tool_use","name":"send_message","input":{"text":{"content":"Hi."}}}]}}
{"role":"tool","message":{"content":[{"type":"tool_result","name":"send_message","result":{"success":{"messageId":"t0s0"}}}]}}
```

## Continue-into / write

`txcript continue --with grok_bot` mints a **new** box-harness agent on the
live local gateway and seeds UI history. It never launches a CLI resume
(`--no-resume` is implied for `grok_bot`).

1. Preflight: `~/agent-data/agents` and `~/agent-data/agent-transcripts` must
   exist and be writable (or `TXCRIPT_GROK_BOT_AGENTS` /
   `TXCRIPT_GROK_BOT_ROOT` / `GROK_BOT_TRANSCRIPTS`). Missing layout fails
   cleanly — use `--out <dir>` for JSONL-only export without a live box.
2. `POST /api/createAgent` with `{name, description, harness: "box"}`
   (Bearer token from `$HOME/agent-data/gateway.json`, or
   `TXCRIPT_GROK_BOT_GATEWAY` + `TXCRIPT_GROK_BOT_TOKEN`).
3. Unlock the new agent's `store.db` by opening another agent.
4. Write Common text turns into `agents/<id>/store.db` `transcript_entries`.
5. Write agent-transcripts JSONL for the read path.
6. `POST /api/openAgent` with the new id — history appears in the product.

### `--metadata`

Repeatable `key=value` or a JSON object, merged left-to-right. `grok_bot`
recognizes:

| Key | Effect |
|---|---|
| `name` | Agent display name for `createAgent` (else transcript title / `"txcript session"`) |
| `description` | Agent description (else a fixed txcript default) |

Unknown keys are ignored (forward-compatible for other harnesses).

```sh
txcript continue ./run.json --with grok_bot \
  --metadata name='Relay bot' \
  --metadata description='Continued from Simple'

txcript continue ./run.json --with grok_bot \
  --metadata '{"name":"Relay bot","description":"from Simple","future":true}'
```


## Caveats

- List titles come from `profile.json` `name` when the agent exists under
  `agents/`; JSONL-only subagents still fall back to the first user text.
- Store/gateway reconstruction keeps user/assistant text turns only (widgets,
  attachments, spend events, and native tool JSONL detail are not recovered
  from the UI ledger). Prefer JSONL when present.
- Gateway `openAgent` is a read fallback and may focus that agent in the UI.
- Continue-into needs a live local gateway and SQLite (`opencode`/`hermes`
  feature). Without them, `local::write` errors clearly.
- No native per-record timestamps, model, usage, or stop reasons.
- Address prefixes are stripped in Common and not regenerated.
- Rich shell bookkeeping keys survive native round trips; through Common they
  only persist while the call remains `Tool::Raw`.
- Thinking blocks are accepted if present; image / artifact blocks have no
  observed native slots.
- The optional `.journal-mode` sidecar is ignored and not part of the body.

## References

- Observed sessions under `/home/box/agent-data/agent-transcripts/` and
  `/home/box/agent-data/agents/*/store.db` (Grok Bot box), 2026-09-10…11.
- Gateway mint sequence verified against `duplicateAgent` / `openAgent` on
  `127.0.0.1:1340`, 2026-09-11.
- Parser: `src/harness/grok_bot.rs`.

Last verified: 2026-09-11 (discover all agents + JSONL/store/gateway load).
