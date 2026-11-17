# Grok

Grok is xAI's official terminal coding agent — the `grok` binary, also branded
**Grok Build**. This is the CLI txcript parses, not the unaffiliated community
`superagent-ai/grok-cli`: the disambiguators are the `~/.grok/sessions` store,
the ACP-style display log, and `"agent_name": "cursor"` in its own session
metadata (its toolset and harness lineage are Cursor's). xAI open-sourced Grok
Build in July 2026 under Apache 2.0 (a large Rust workspace at
`xai-org/grok-build`; issues and external contributions are closed). txcript's
mapping was reverse-engineered from real sessions written by grok 0.2.114 on
this machine; the upstream source now exists as a corroborating spec, and the
persistence module cited below matches the observed layout.

```
~/.grok/sessions/                          (or $GROK_HOME/sessions)
└── <percent-encoded-cwd>/                 one dir per workspace
    ├── prompt_history.jsonl               (project-level; ignored)
    └── <session-uuid>/                    one dir per session
        ├── chat_history.jsonl    ── model-protocol log   ┐ the conversation
        ├── updates.jsonl         ── ACP display log      ┘ (two views of it)
        ├── summary.json          ── session metadata
        └── sidecars: events.jsonl, rewind_points.jsonl,
            prompt_context.json, resources_state.json, signals.json,
            system_prompt.txt   (carried verbatim, not interpreted)
            terminal/, assets/, *.lock   (not carried)
```

## On disk

A session is a **directory of plain files**, not a single transcript file. The
root is `~/.grok/sessions`, overridden wholesale by `$GROK_HOME` (sessions then
live at `$GROK_HOME/sessions`). The first level is the workspace path,
URL-percent-encoded per byte (RFC 3986 unreserved characters kept); the second
is a session UUID. txcript's discovery walks exactly those two levels and
sniffs a session by the presence of `updates.jsonl` or `chat_history.jsonl` —
stray files like the project-level `prompt_history.jsonl` and the
`session_search.sqlite` index at the root are skipped. When `summary.json`
alone answers every metadata question (id, cwd, `created_at`, model),
discovery never opens the logs.

## Dissection of a transcript

The conversation exists twice: `chat_history.jsonl` is what the model sees on
continuation, `updates.jsonl` is what `grok --resume` replays on screen.
txcript reads the conversation from the model log and backfills from the
display log what the model log lacks — timestamps, user images, tool failure
status, and stop reasons.

| Their name | What it is | Maps to |
|---|---|---|
| `chat_history.jsonl` line, `type: "user"` | Prompt text blocks, wrapped in `<user_query>` tags; optional `prior_turn_interrupt` | `Role::User` message of `Text` blocks (wrapper stripped), plus `Image` blocks pulled from the display log |
| `type: "assistant"` | Response text plus `tool_calls` (each with `id`, `name`, `arguments` as JSON-in-a-string), `model_id` | `Role::Assistant` message: `Text` + `ToolUse` blocks; `model` |
| `type: "reasoning"` | `summary` (array of `summary_text` parts) plus opaque `encrypted_content` | Assistant `Thinking` block; the encrypted token is preserved for round-trips |
| `type: "tool_result"` | `tool_call_id` plus (usually string) `content` | `ToolResult` block on a `Role::User` message, paired by id |
| `type: "system"` | The injected system prompt | Dropped from the conversation (kept in the native body) |
| `updates.jsonl` `user_message_chunk` | ACP prompt chunk — text, or `{type: "image", data, mimeType}`; grouped by `_meta.promptIndex` | Prompt timestamp; images (their **only** home — the model log holds a textual description instead) |
| `agent_message_chunk`, `agent_thought_chunk` | Streamed display text | Timestamps for text/thinking messages |
| `tool_call`, `tool_call_update` | Display-side call lifecycle (`status`: pending → completed/failed) | Tool timestamps; `status: "failed"` sets `is_error` on the result |
| `turn_completed` (method `_x.ai/session/update`) | End-of-turn marker with `stop_reason` | `StopReason` on the turn's last assistant message (`cancelled` → `Aborted`; `prior_turn_interrupt` is the fallback) |
| `summary.json` | Session metadata: `info.id`, `info.cwd`, `created_at`, `generated_title`, `current_model_id`, `head_branch`, git info | `Meta` |

Threading is strictly linear — no parent pointers, no branching; a turn opens
at each real user prompt and closes at the next one. Tool calls pair with
results by `tool_call_id` alone. Grok's injected `<user_info>` preamble (a
`user` record) is context, not a prompt: it is skipped, though its
`Workspace Path:` line serves as a cwd fallback when `summary.json` is absent.
Tool names are Cursor's and get normalized to the Claude convention
(`Shell`→`Bash`, `StrReplace`→`Edit`, `path`→`file_path`, and so on).

A synthetic `chat_history.jsonl` assistant line, shaped like the real thing:

```json
{"type": "assistant", "content": "Reading the config first.",
 "tool_calls": [{"id": "call_a1b2", "name": "Read",
                 "arguments": "{\"path\": \"/tmp/demo/config.toml\"}"}],
 "model_id": "grok-composer-2.5-fast", "model_fingerprint": "fp_0000"}
```

## Caveats

- Observations are from grok 0.2.114 (`chat_format_version: 1`).
- The model log carries **no timestamps**. Every date comes from the display
  log's `_meta.agentTimestampMs` (or the line-level epoch `timestamp`); a
  session missing `updates.jsonl` — they exist in the wild — parses fine but
  every message falls back to the session timestamp.
- User images live only in the display log; natively the model log holds a
  Grok-generated description. Regeneration writes an `[image]` placeholder
  there, since the description can't be synthesized.
- Tool-call `arguments` are JSON-in-a-string; unparseable strings are kept
  raw rather than dropped. `block_until_ms` arrives as a float (`120000.0`)
  and is converted to integral `timeout_ms` only when lossless.
- Token usage is never extracted (the display log's `_meta.totalTokens` is
  ignored), so `usage` is always absent.
- Regenerating from Common is lossy: per-turn usage, non-turn stop reasons,
  reasoning record ids, structured tool-result JSON (flattened to strings: block arrays
  read as their text and `[image: <media type>]` placeholders, other JSON as compact text),
  and the system prompt record are not reproduced; sidecars are left empty
  for Grok to regenerate. Native load → save round-trips everything except
  `terminal/` output scratch and `*.lock` files, which are deliberately
  not carried.
- Hostile input is bounded: `_meta.promptIndex` is untrusted and sizes an
  allocation, so implausible values are treated as absent; delete refuses
  any path that doesn't resolve to `<root>/<project>/<id>`.

## References

- Upstream (open source, Apache 2.0), pinned:
  <https://github.com/xai-org/grok-build/blob/b13fa526f5112c0b20dad5f1f2300d3d3b127895/crates/codegen/xai-chat-state/src/persistence.rs>
  — session persistence in `xai-chat-state`; path verified at that SHA via the
  GitHub API on 2026-08-10.
- This document was reverse-engineered from sessions observed under
  `~/.grok/sessions` (grok 0.2.114) before the upstream release; the code
  above corroborates it but was not the primary source.
- Authoritative mapping: `src/harness/grok.rs` (module docs and code) and
  `tests/integration/grok.rs` (fixtures shaped like real sessions).

Last verified: 2026-08-10, against src/harness/grok.rs and real local sessions.
