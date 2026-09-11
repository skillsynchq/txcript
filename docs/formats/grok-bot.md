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

Session id = directory name = JSONL basename. The default root is
`$TXCRIPT_GROK_BOT_ROOT` or `$GROK_BOT_TRANSCRIPTS`, falling back to
`$HOME/agent-data/agent-transcripts`. Discovery lists immediate child
directories that contain `<dirname>.jsonl` whose first record is a
`role`/`message` envelope (`user` / `assistant` / `tool`); the journal-mode
sidecar is ignored. Unreadable directories are skipped. `sand-subagent-*`
directories use the same layout and are included.

txcript's harness loads and saves the **JSONL** under `agent-transcripts/`.
Per-agent SQLite under `agents/<uuid>/` (`store.db` with
`transcript_entries`, plus `conversation-blobs.db`) is what the product UI
actually renders; those DBs are documented under Continue-into below and are
not parsed by the current codec.

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

**Refused by `local::write`.** There is no public session-import or resume
CLI. Writing agent-transcripts JSONL alone does **not** make history appear
in the Grok Bot UI.

Verified mint-with-history sequence on a live local gateway
(`http://127.0.0.1:1340`, Bearer token from `$HOME/agent-data/gateway.json`):

1. `POST /api/duplicateAgent` with body `{"id":"<source>"}` — clones the agent.
2. `POST /api/openAgent` with the **source** id (unlocks / materializes copy DB paths).
3. Restore the source `store.db` + `conversation-blobs.db` onto the new agent
   directory; remap `agentId` in store metadata; keep `blobEncryptionKey` and
   `latestRootBlobId`; do **not** `clearConversation`.
4. `POST /api/openAgent` with the **new** id — UI history appears.

`CreateAgent` / host-official clone paths clear chat
(`includesChatHistory=false`). The restore above is the
`includesChatHistory=true` equivalent. Because that path needs a live gateway,
encrypted blob keys, and a source agent DB — and cannot be synthesized cleanly
from `Transcript<Common>` — txcript keeps continue-into source-only (like
Hermes / Amp). `GrokBotStore::save` still writes JSONL for conversion and
tests.

## Caveats

- No public import / resume CLI: convert *from* `grok_bot`, not into it
  (store load/save still works for conversion and tests).
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

Last verified: 2026-09-11.
