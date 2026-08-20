# Per-harness pattern catalog

Distilled from the existing integrations. Read the section for whichever
codec you chose as reference in Phase 1 — then re-read the actual source file
before implementing; this catalog tells you what to look for, not what to copy
blindly.

## claude_code — JSONL, few typed kinds, near-identity codec

- `Record` enum `Summary | User | Assistant | Other(Value)`; manual serde via
  `From<Value>` dispatch on `type`. Parse failure of a *known* type demotes to
  `Other(v)` — malformed lines round-trip verbatim. `tagged()` re-inserts the
  tag on render.
- `EntryLine` types only the envelope (`parentUuid`, `uuid`, `timestamp`,
  `sessionId`, `cwd`, `gitBranch`, `version`) + flatten `extra`. The API
  `message.content` and `usage` stay raw `Value` — the codec interprets them,
  serde stays lossless.
- Claude Code IS the canonical tool convention: no normalize step, input goes
  straight to `Tool::from_canonical`.
- Session metadata is stamped on **every line**; `from_common` must re-stamp
  `sessionId/cwd/gitBranch/version` per line, and meta extraction is keep-first
  (+ earliest timestamp, `custom-title` > `agent-name` > `summary` for title).
- `from_common` synthesizes a leading `SummaryLine` from `meta.title` purely so
  `claude --resume` shows a title; its `leafUuid` is `entry_uuid(session,
  usize::MAX)` — valid-looking, intentionally dangling.
- Deterministic entry uuids: UUIDv5 over `"{session_id}:{index}"`; parent chain
  is linear (previous entry's uuid).
- Store: `~/.claude/projects/<encoded-cwd>/<id>.jsonl`; encoding maps **both**
  `/` and `.` to `-`. Discovery skips `subagents/` and `tool-results/` dirs.
- Known asymmetry: `Aborted`/`Error` stop reasons render as `"aborted"`/
  `"error"` but parse back as `Other(…)` — non-Anthropic stop reasons don't
  fixpoint; keep them out of fixtures.

## codex — JSONL, one envelope kind, dual logs, stateful aggregation

- One record type: `Line { timestamp?, type, payload: Value, flatten extra }`.
  No enum needed — unknown kinds pass through via `_ => {}` in the codec.
- **Dual logs**: `response_item` (protocol) and `event_msg` (display) both
  describe the same events. Reading: canonical result per kind
  (`exec_command_end`, `custom_tool_call_output`) marks its call_id; mirror
  `function_call_output`s are flagged `is_fallback_result` and deduped in a
  post-pass (order-independent). Writing: **emit both logs** — response_item
  only ⇒ the TUI resumes to an empty conversation.
- Attribution is remote: `turn_context` sets model per turn, `token_count`
  stashes usage, `task_complete` backfills both onto the turn's *last assistant
  text message* (tracked by queued index). `from_common` re-emits
  `turn_context`/`token_count`/`task_complete` or the data dies after one round
  trip.
- `session_meta` must carry `model_provider` and `base_instructions` keys even
  as `null` — omission makes codex silently start a fresh session.
- JSON-in-strings everywhere (`arguments`, `input`, `output`): every parse has
  a raw-string fallback; never unwrap.
- Tool normalization: `shell`/`exec_command` → `Bash` (unwraps legacy argv
  arrays `["bash","-lc",script]`); `apply_patch` → `Write`/`Edit` only for the
  unambiguous single-file cases, else `Raw ApplyPatch {patch, files}` keeping
  the patch text. `from_common` emits canonical names in `function_call` (no
  denormalization back to `shell`) — fixpoint holds because unknown names pass
  through normalize.
- Web search calls historically lack call_id: pair by serialized `action`
  JSON, tolerate result-before-call (pending-id map) and call-before-result
  (retro-patch queued index), synthetic `web_search:{seq}` as last resort.
- Setup scaffolding (`<environment_context>`, `<permissions instructions>`, …)
  filtered by text-prefix check on user messages.
- Store: `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<id>.jsonl`; discovery
  matches `rollout-*.jsonl` anywhere but validates by loading and finding a
  `session_meta` with a string id.

## pi (and campfire) — JSONL, typed kinds, shared-format delegate

- `Record` enum `Session | Message | Custom | Other(Value)`, same manual-serde
  pattern as claude_code. `MessageEntry.message` stays raw `Value` (pi's
  role union is messy; "a faithful Value is the lossless choice").
- Tree punt: entries carry `id`/`parentId`; bytes preserved, file order taken
  as linear, single-branch assumption documented in the module doc.
- `bashExecution` records (pi's `!` shell) expand to a synthetic
  ToolUse/ToolResult pair (`bash_exec_{seq}`); `excludeFromContext: true`
  (`!!`) is dropped, mirroring pi's own context semantics. Not regenerated on
  from_common — comes back as an ordinary tool call.
- Tool shape change: pi `edit {path, edits:[{oldText,newText}]}` → `Edit` (1
  hunk) or `MultiEdit` (≥2). Denormalize is the exact inverse.
- toolResult requires a redundant `toolName`: rebuilt from the
  `tool_use_id → name` map captured while emitting toolCalls; orphans get
  `"tool"`.
- Fabricated-but-required: `provider`/`api` guessed from model-id prefix,
  `totalTokens`, zeroed `cost` object. All-zero usage parses to `None`.
- Deterministic short ids: first 8 hex of UUIDv5 over `"{session}:{i}:{j}"`.
- Meta from bookkeeping records: latest `model_change` wins for model;
  `session_info.name` (trimmed) for title.
- Store: env-override chain (`$PI_CODING_AGENT_SESSION_DIR` >
  `$PI_CODING_AGENT_DIR/sessions` > `~/.pi/agent/sessions`); cwd encoded
  `--{body}--`; discovery sniffs "first record is a session header".
- **Campfire** is the delegate pattern: pi's helpers (`records_to_messages`,
  `messages_to_records`, `meta_from_records`, store helpers,
  `resolve_sessions_dir(config_dir, env_prefix)`) are `pub(crate)` and
  parameterized; campfire.rs is 84 lines of identity + root. If your new
  harness embeds/forks an existing one, do this, not a copy.

## opencode — JSON export document, CLI-delegated writes, feature-gated store

- Body = the `opencode export` document `{info, messages: [{info, parts}]}` —
  the harness's own interchange shape, NOT raw DB rows. Everything inside the
  two-level envelope is `Value`, navigated with `.get()`; losslessness is
  total because nothing is struct-parsed.
- One native assistant message fans out into several Common messages (pending
  text/reasoning flushed before each tool part; ToolUse and ToolResult each
  their own message). Turn-level `tokens`/`finish` attach to the **last**
  assistant message of the fan-out.
- `from_common` pairs by lookahead: a following user message with exactly one
  matching `ToolResult` block is folded into the tool part's `state` and
  consumed. The fixpoint fixture must give the result the same timestamp as
  the tool turn.
- Required-field defaults chosen consciously: `finish` defaults `"stop"`
  (documented loss for `StopReason::Other`); `providerID` hardcoded; `cache`
  object omitted entirely when zero (Option-vs-zero trap).
- `projectID` etc. are NOT synthesized — `save()` writes a temp JSON and
  shells out to `opencode import`, which owns schema defaults. DB is opened
  `READ_ONLY` for discover/load only.
- Feature gating: cargo feature = `dep:rusqlite` only; the gate wraps just
  `mod store` + its re-export. Codec/TextCodec/Body compile featureless (the
  WASM path). Store unit tests live inside the gated module against a real
  temp SQLite DB with a minimal copy of the schema.
- Storage-shape vs export-shape differences normalized at load (DB time column
  injected into `info.time`) so the codec sees one shape from both paths.
- Placeholder titles (`"New session - …"`) filtered in both meta paths.

## cursor — SQLite blobs, reverse-engineered internals, blob-level losslessness

- Body = `CursorDb { blobs: Vec<{id, data: Vec<u8> hex-serde}>, meta rows,
  session_meta: Option<Value> }`. No field-level unknown capture — losslessness
  is at the **blob byte level**; only parse the JSON blobs you understand,
  binary/protobuf blobs pass through untouched. Blob id = sha256(data).
- TextCodec for a DB harness = pretty JSON dump of the Body (binary as hex) —
  your portable serialization, nothing Cursor emits.
- `from_common` regenerates Cursor's *internal* structures or resume breaks:
  the hand-rolled protobuf turn graph (root state → turns → steps, tool-call
  payloads with exact field numbers), `latestRootBlobId` pointing at the last
  blob, the hex-encoded meta row `"0"`, and sidecar `meta.json` +
  `prompt_history.json`. MD5/SHA-256/protobuf are hand-rolled in-file rather
  than adding deps.
- Native invariants forced: every turn needs a user message (assistant-first
  transcripts get a synthetic "Continue." turn); orphan tool results become
  text steps; `Tool::Raw` has no proto encoding → degraded to a plaintext
  step while the JSON blob still carries it.
- Meta is a fallback ladder across four layers: sidecar `meta.json` → db meta
  row (plain *or* hex JSON) → scraping blob content (`Workspace Path: `,
  `providerOptions.cursor.modelName`) → file mtime for epoch-0 timestamps.
- Read tolerant / write authoritative: read is `READ_ONLY`, missing tables
  tolerated, bad rows skipped; write recreates schema and wipes tables (full
  rewrite, never merge).
- Scaffolding round-trip: `<user_query>` wrapper stripped on read, re-added
  idempotently on write; `<user_info>` injection blobs skipped but mined for
  cwd; `[REDACTED]` markers scrubbed.
- `is_error` heuristic (`Error:` prefix, `" exited with code "`) when Cursor
  omits the flag.
- Tests include a hand-rolled protobuf *reader* asserting exact field numbers
  — test the wire format with your own decoder, not string-contains.

## grok — session directory, dual logs with exclusive carriers

- Body = one struct field per file: typed `chat_history` records, raw
  `Vec<Value>` for `updates`/`events`/`rewind_points`, `Option<Value>`
  sidecars, `Option<String>` system prompt. Ephemeral scratch (`terminal/`
  output logs, `*.lock`) deliberately not carried.
- **Exclusive carriers**: the model log has the conversation but NO
  timestamps, images, error flags, or stop reasons — those live only in the
  display log (`updates.jsonl`, ACP `session/update` notifications). And the
  display log is what `--resume`/`export` replay; the model log is what the
  model sees. `to_common` = stateful join (index the display log first:
  per-prompt chunks keyed by `_meta.promptIndex`, call/result timestamps by
  id, `turn_completed` stop reasons in turn order); `from_common` = fan-out
  (every message emits into both logs, or half the session is missing).
- Discovered by bisection against `grok export`: only `updates.jsonl` is
  load-bearing for replay; `summary.json` feeds listing/metadata;
  `chat_history.jsonl` feeds continuation context. All three regenerated.
- Strict parser, loud failure: `tool_result.content` is `String` — a block
  array in that slot fails the *entire* session load. Flatten (text blocks →
  joined text, other JSON → compact string). Images never enter the model
  log (natively Grok substitutes a generated description); they ride the
  display log as `{"type":"image", data, mimeType}` chunks of the prompt —
  shape confirmed by probing `--prompt-json`, not guessed.
- Tool convention is Cursor's (`Shell`/`StrReplace`, `path` keys) plus
  float `block_until_ms` → integral `timeout_ms` only when exact; `Glob`
  keys canonicalized inside `Raw` for cross-harness portability.
- Missing sample coverage (edits, images) was *minted*: headless
  `grok -p`/`--prompt-json` probe sessions in a scratch cwd.
- Store: `$GROK_HOME/sessions` > `~/.grok/sessions`; cwd percent-encoded
  (RFC 3986 unreserved kept); a session dir is sniffed by the presence of
  either log; discovery meta comes cheap from `summary.json`.

## cowork — app record + embedded Claude Code JSONL, consumer read from the app bundle

- Body = `CoworkSession { header: Header (typed required/meta fields +
  flatten extra), transcript: Vec<claude_code::Record>, audit: Vec<Value> }`.
  The conversation IS Claude Code's JSONL under a per-task
  `CLAUDE_CONFIG_DIR`, so the codec delegates to `claude_code`'s
  `pub(crate)` `records_to_messages`/`messages_to_records` (extracted for
  this; the campfire move applied to a non-sibling) with `meta.id` swapped
  for the `cliSessionId`. Nothing Cowork-specific is re-parsed.
- **The consumer was read, not probed**: the desktop app has no CLI, so the
  regeneration contract came from its bundled JS (`npx @electron/asar
  extract`, then grep the `.vite/build/*.chunk-*.js` for the session
  manager). Its zod schema for `local_*.json` is `.passthrough()` with five
  required keys (`sessionId`, `processName`, `cwd`, `createdAt`,
  `lastActivityAt`); a record failing it is silently not listed. The
  transcript is located by `cliSessionId` under *any* project slug, so the
  slug need not match Claude Code's (hash-truncated) encoding of long cwds.
- Shell gotcha that burned an hour: `grep` was aliased to ugrep, which
  rejects long-context patterns on minified lines, and BSD grep caps `\{n\}`
  at 255 — use a Python `re` script for bundle archaeology.
- `audit.jsonl` is an HMAC chain keyed via Electron `safeStorage`: carried
  verbatim on native round trips, never written (the app accepts unsigned
  and absent logs). Synthesized fields: `processName` (`txcript-<8 hex>`),
  `hostLoopMode: true` (host CLI, not the VM), `initialMessage`,
  `lastActivityAt` = last message. `cliSessionId` = UUIDv5 of the session id.
- Ids are `local_<uuid>`; foreign ids get the prefix (the one documented
  `Meta` change through Common). Store root is the Electron userData dir;
  account trees are `<org-uuid>/<account-uuid>/` (both levels UUID-shaped —
  that's the sniff against sibling app state); `save` picks the tree with the
  youngest record. The app loads records once at startup: a written session
  shows up after quit + relaunch, and there is no per-session deep link
  (`claude://resume?session=` targets the app's *Claude Code* surface).
- Resume verification split: Cowork → Claude Code headless (`claude -p
  --resume` from an unrelated cwd) and TUI; Claude Code → Cowork only via the
  app, because the per-task config dir has no credentials (`claude -p` under
  it says "Not logged in") — the user drives that check.

## Cross-cutting invariants (all)

- Fallback-to-raw at every level: record → `Other(Value)`, tool →
  `Tool::Raw`, JSON-string parse → keep the raw string. Nothing errors,
  nothing drops.
- `#[serde(default, skip_serializing_if)]` on every optional; type tag outside
  the struct; messy payloads as `Value`.
- Deterministic UUIDv5 synthesis (each harness has its own fixed namespace
  constant); v4 only for empty `meta.id`.
- Meta extraction: per-field fallback chain; empty id backfilled from filename
  by the Store, not the TextCodec.
- Fingerprints: `"{mtime_nanos}:{len}"` or one cheap SQL aggregate; failures
  are empty strings, missing roots are `Ok(vec![])`.
- The fixpoint direction is `to_common ∘ from_common = id` on
  harness-representable Common; `from_common ∘ to_common` produces a
  *different but equivalent* native encoding and is only tested at the Store
  layer (record equality of load→save→load).
- Test names are behavior sentences; fixtures inline `json!` + `tempfile`;
  `#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]` at the
  top of test files only.
