# Grok Bot

Grok Bot is xAI's desktop assistant (distinct from the [Grok CLI](grok.md) /
Grok Build coding agent). It is closed source and publishes no transcript
schema; this document is **reverse-engineered** from observed
`agent-transcripts` JSONL on a Grok Bot box (2026-09-10). The authoritative
mapping is `src/harness/grok_bot.rs`.

```
agent-transcripts/
└── <agent-uuid>/
    ├── <agent-uuid>.jsonl .......... append-only conversation
    └── <agent-uuid>.journal-mode ... tiny sidecar (ignored)
sand-subagent-<uuid>/ ............... same shape (subagent transcripts)
```

## On disk

Session id = directory name = JSONL basename. The default root is
`$TXCRIPT_GROK_BOT_ROOT` or `$GROK_BOT_TRANSCRIPTS`, falling back to
`$HOME/agent-data/agent-transcripts`. Discovery lists immediate child
directories that contain `<dirname>.jsonl` whose first record is a
`role`/`message` envelope (`user` / `assistant` / `tool`); the journal-mode
sidecar is ignored. Unreadable directories are skipped.

## Dissection of a transcript

There is no session header. Each line is one JSON object:

| Their name | What it is | Maps to |
|---|---|---|
| record `role: "user"` | user prompt; text often prefixed `[t0u]\n` | `Message { role: User }` (prefix stripped in Common) |
| record `role: "assistant"` | model text and/or `tool_use` blocks | `Message { role: Assistant }` |
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

## Caveats

- No public import / resume CLI: convert *from* `grok_bot`, not into it
  (store load/save still works for conversion and tests).
- No native per-record timestamps, model, usage, or stop reasons.
- Address prefixes are stripped in Common and not regenerated.
- Rich shell bookkeeping keys survive native round trips; through Common they
  only persist while the call remains `Tool::Raw`.
- Result-envelope fields beside `success` / `failure` (such as
  `isBackground`) do not survive projection through Common.
- Thinking / image / artifact blocks have no observed native slots.

## References

- Observed sessions under `/home/box/agent-data/agent-transcripts/` (Grok Bot
  box), 2026-09-10.
- Parser: `src/harness/grok_bot.rs`.

Last verified: 2026-09-10.
