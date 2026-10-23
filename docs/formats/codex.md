# Codex

Codex is OpenAI's coding-agent CLI (`codex`), an open-source Rust program at
[github.com/openai/codex](https://github.com/openai/codex). It persists each session as a
"rollout": a JSONL file the CLI both appends to live and replays on resume. This document is
derived from the upstream serialization code (open-source code as spec), cross-checked against
real local sessions and against txcript's parser in `src/harness/codex.rs`, which is the
authoritative mapping into txcript's Common model.

```
~/.codex/sessions/
└── 2026/08/10/                                  dated YYYY/MM/DD tree
    └── rollout-2026-08-10T13-02-51-<uuid>.jsonl one file per session
        │
        ├── {"timestamp","type":"session_meta","payload":{…}}   header
        ├── {"timestamp","type":"turn_context","payload":{…}}   turn boundary
        ├── {"timestamp","type":"response_item","payload":{…}}  protocol log (what the model sees)
        ├── {"timestamp","type":"event_msg","payload":{…}}      display log (what the TUI replays)
        └── …                                                   two logs, interleaved
```

## On disk

Rollouts live under `~/.codex/sessions`, or `$CODEX_HOME/sessions` when `CODEX_HOME` is set
(Codex honors the override before its home lookup, and txcript mirrors that in
`CodexStore::default_root`). Files are sharded into dated subdirectories `YYYY/MM/DD` by session
start time, one file per session, named `rollout-<YYYY-MM-DDThh-mm-ss>-<session-uuid>.jsonl`
(colons in the timestamp become hyphens for filesystem safety).

txcript's discovery walks the tree recursively for `rollout-*.jsonl` files (symlinked directories
are not followed, guarding against cycles; symlinked files still list). A file only counts as a
session if it contains a `session_meta` line carrying an `id`; discovery parses just those lines
and skips message payloads entirely. On load, a missing id falls back to the filename's uuid.

Codex compresses a rollout it hasn't written to for 7 days. It replaces the file in place with
`rollout-<ts>-<id>.jsonl.zst`, one zstd frame at level 3, and expands it back to `.jsonl` the
next time the session is resumed. Upstream gates this behind its
`local_thread_store_compression` feature. With txcript's `codex_zstd` feature, on by default,
discovery lists a compressed rollout and `load` decodes it. Discovery reads only as far as the
header line, so listing a compressed session costs about what a plain one does. Both forms can be
on disk for a moment while Codex compresses or expands a session. Codex reads the plain one then,
and discovery lists only that one. Without the feature a compressed rollout isn't listed. zstd is
a C library, so the wasm build leaves it out.

Codex's own `/archive` (TUI) and `codex archive`/`codex unarchive` (CLI) move a rollout out of
this dated tree into a flat sibling directory, `archived_sessions` (no `YYYY/MM/DD` sharding).
`CodexStore::default_root` sets `archived_sessions_dir` to that sibling, and discovery walks it
alongside `sessions_dir`, so an archived rollout is still listed — Codex's own session picker just
won't show it until it's unarchived back into `sessions/`. A `CodexStore` built directly from a
custom `sessions_dir` has no archived directory unless one is set with
`with_archived_sessions_dir`.

Deletion accepts rollout files, plain or compressed, in either configured directory, including
when the active directory is missing. It resolves symlinks before checking containment and refuses paths
outside both directories. Saving always writes into `sessions_dir`.

For Rust callers, the new public field changes struct-literal construction. Replace
`CodexStore { sessions_dir }` with `CodexStore::new(sessions_dir)`, or include
`archived_sessions_dir: None` in the literal. Use `with_archived_sessions_dir` to enable
archives for a custom store.

## Dissection of a transcript

Every line shares one envelope — upstream's `RolloutLine`: a `timestamp` (RFC 3339, millisecond
precision, UTC) plus a `RolloutItem` flattened as `type` + `payload` (some versions add an
`ordinal`). Two logs interleave in one file: `response_item` is the protocol log (the exact items
the model exchanges) and `event_msg` is the display log (what the TUI renders). Most content
exists in both; txcript reads primarily from `response_item` and uses `event_msg` for what only
it carries.

| Their name | What it is | Maps to |
|---|---|---|
| `session_meta` | Header: `id`, `timestamp`, `cwd`, `cli_version`, `git.branch`, `model_provider`, instructions, `source`/`parent_thread_id`/`forked_from_id` | `Meta` (id, timestamp, cwd, git_branch, cli_version, model, lineage) |
| `turn_context` | Turn boundary: `turn_id`, `model`, cwd, sandbox/approval policy | Model attribution for the turn's assistant messages |
| `response_item` / `message` | User or assistant message; `content` array of `input_text` / `output_text` / `input_image` (data URL) | `Message` with `Text` / `Image` blocks |
| `response_item` / `reasoning` | Reasoning item: `summary` array of `summary_text`, opaque `encrypted_content` | Assistant `Thinking` block (summary text only) |
| `response_item` / `function_call` | Tool call; `arguments` is JSON-in-a-string, paired by `call_id` | `ToolUse`; `exec_command` / `shell` normalize to `Bash` |
| `response_item` / `function_call_output` | Tool result mirror | `ToolResult` — kept only if no canonical result exists (see below) |
| `response_item` / `custom_tool_call` (+ `_output`) | Freeform-input tool, notably `apply_patch` | `Edit` (single-hunk update), `Write` (single add), else `ApplyPatch` raw |
| `response_item` / `web_search_call` | Server-side search; `action` object, `call_id` often absent | `WebSearch` `ToolUse` |
| `event_msg` / `exec_command_end` | Canonical shell result: `aggregated_output`, `exit_code` | `ToolResult` (`is_error` from nonzero exit) |
| `event_msg` / `web_search_end` | Search result, carries the `call_id` the call may lack | `ToolResult`, plus call-id pairing |
| `event_msg` / `token_count` | Usage snapshot: `info.last_token_usage` | `Usage` on the turn's last assistant text |
| `event_msg` / `task_started` / `task_complete` | Turn lifecycle | Triggers model/usage backfill onto that turn |
| everything else (`compacted`, `world_state`, `agent_message`, `user_message`, …) | Mirrors or non-conversational state | Skipped in Common; preserved verbatim at the native level |

Translation is a single stateful pass, not a per-line map. `turn_context` / `task_started` set the
current turn; assistant messages inherit that turn's model, and `token_count` + `task_complete`
backfill usage onto the turn's last assistant text. Tool calls pair with results by `call_id` —
except web search, where the call often has no `call_id`: txcript matches the `web_search_end`
event by the serialized `action` object and adopts its id (falling back to a synthetic
`web_search:N`). Because shell and custom-tool results appear twice — a rich `event_msg` and a
plain `function_call_output` mirror — the mirror is dropped whenever a canonical result with the
same `call_id` exists. User messages that are pure scaffolding (`<environment_context>`,
`<permissions instructions>`, etc.) are dropped; `system` / `developer` roles are not turns.

A synthetic `response_item` line, shaped like the real thing:

```json
{"timestamp":"2026-08-10T20:05:52.101Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":[\"bash\",\"-lc\",\"cargo test\"]}","call_id":"call_abc123"}}
```

That becomes an assistant `ToolUse` with tool `Bash { command: "cargo test" }` — argv arrays of
the form `["bash"|"sh"|"zsh", "-lc"|"-c", cmd]` collapse to the inner command.

## Lineage

`session_meta.payload` names a parent two ways, and `Meta::lineage` is filled from whichever
applies, at discovery time as well as on load — reading the header line is all it costs.
`source.subagent` present means this thread was spawned: the parent is
`source.subagent.thread_spawn.parent_thread_id` for a spawned subagent, or the top-level
`parent_thread_id` for the guardian-review shape (`source.subagent.other == "guardian"`, usually
paired with `thread_source: "guardian_review"`) and any other subagent shape. No `source.subagent`
but a top-level `parent_thread_id` is also a spawn. Otherwise, `forked_from_id` is a fork. A
subagent thread forked from its parent's history at spawn time carries both fields; spawn wins.
`from_common` writes a Fork lineage back as `forked_from_id`, so a fork survives a round trip
through Common. It writes nothing for a spawn, since `source.subagent` would turn the copy into
a subagent thread, and nothing for a continue, which Codex has no field for. It also skips a
parent that isn't a UUID, because Codex rejects a `session_meta` whose `forked_from_id` isn't
one. `source` is always `"cli"` and `parent_thread_id` is never written. When `continue` or
`crop` writes a copy into Codex from a Codex session, the copy is a fork of that session and
names it in `forked_from_id`. A copy from another harness gets no lineage, because its source
id doesn't name a Codex thread.

## Caveats

- **Lossiness in Common.** `encrypted_content` on reasoning items, sandbox/approval context,
  rate limits, and all display-only events don't survive `to_common`. The native `Vec<Line>`
  representation keeps every payload as raw JSON, so native ↔ disk round-trips are lossless.
- **`apply_patch` is best-effort.** Only a lone single-hunk update maps to `Edit` and a lone file
  add to `Write`; multi-file, multi-hunk, delete, and move patches stay as a raw `ApplyPatch`
  with the touched paths listed. On export, `Edit` and `Write` become `apply_patch`
  custom-tool calls, and raw `ApplyPatch` envelopes are unwrapped. Their results use
  `custom_tool_call_output`, preserving the error flag. Patches are line-based:
  `replace_all` becomes one hunk, Codex can normalize trailing blank lines, and the Common
  reader does not retain the final line terminator. Exported `Write` calls use an add-file
  patch, which does not encode whether the original call created or overwrote a file.
- **Shell export.** `Bash` becomes `exec_command` with `cmd` and optional `workdir`.
  The canonical timeout, description, and background fields have no matching fields in
  this mapping and are omitted. Other tools keep their canonical function-call form.
- **Resume is picky.** `from_common` must emit `model_provider: "openai"` in `session_meta` —
  current Codex resolves a null provider to the empty name and fails resume with
  ``Model provider `` not found``. `base_instructions` may be null (defaults substitute). Foreign
  tool names are normalized to OpenAI's `[A-Za-z0-9_-]+` requirement when written: unsupported
  characters become `_`, and an empty name becomes `tool`. This destination-only normalization is
  not reversible, and distinct foreign names can converge (for example, `a.b` and `a/b`).
- **Version drift.** Newer rollouts add fields (`ordinal`, `session_id`, `parent_thread_id`,
  structured `source`) and kinds (`world_state`, `compacted`, `inter_agent_communication`).
  Unknown envelope fields land in a flattened `extra` map and unknown kinds are carried
  verbatim, so drift degrades to skipped-in-Common rather than parse failure.
- **Hostile input.** Session ids are validated as path components before save-path construction;
  malformed lines are skipped by the JSONL parser rather than aborting the file; a `session_meta`
  without an id disqualifies a file from discovery instead of producing a broken session.
- **Duplicate results are by design.** Seeing both `exec_command_end` and a matching
  `function_call_output` in a file is normal; only one becomes a `ToolResult`. Matching
  and duplicate suppression follow each function/custom call occurrence, so a completed
  ID can be reused without changing the result type or dropping an earlier result.

## References

- Upstream envelope (`RolloutLine`, `RolloutItem` with `tag = "type", content = "payload"`):
  <https://github.com/openai/codex/blob/260261ed8f5c91ad6b7f695571a4111ed1a46272/codex-rs/history/src/lib.rs> (accessed 2026-08-10)
- Upstream writer (dated-tree layout, `rollout-<ts>-<id>.jsonl`, millisecond UTC timestamps):
  <https://github.com/openai/codex/blob/260261ed8f5c91ad6b7f695571a4111ed1a46272/codex-rs/rollout/src/recorder.rs> (accessed 2026-08-10)
- Payload item types (`ResponseItem`, `EventMsg`, `SessionMeta`, `TurnContext`):
  <https://github.com/openai/codex/blob/260261ed8f5c91ad6b7f695571a4111ed1a46272/codex-rs/protocol/src/protocol.rs> (accessed 2026-08-10)
- Upstream compression (the `.jsonl.zst` name, the 7-day age, plain read before compressed):
  <https://github.com/openai/codex/blob/eb1d7065f8b1b2fb3d197bc66448deb432ab0a12/codex-rs/rollout/src/compression.rs> (accessed 2026-10-13)
- Authoritative mapping in this repo: `src/harness/codex.rs` (codec, store, tool normalization),
  with shape fixtures and aggregation assertions in `tests/integration/codex.rs`.

Last verified: 2026-10-13, against src/harness/codex.rs and real local sessions.
