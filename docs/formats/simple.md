# Simple

Simple is txcript's own interchange format: a single JSON document any agent
can emit to make its transcripts portable into every harness txcript
supports. It is the one format in this collection with no app behind it —
there is nothing to resume natively, no storage schema to reverse-engineer.
This document is not a description of the format; it *is* the format. The
parser in `src/harness/simple.rs` and the integration tests are the
authoritative mapping, per the convention of this collection.

Simple is a forgiving projection of txcript's canonical model
(`Transcript<Common>`): every field the canonical model carries has a slot
here, and almost every slot is optional. The block shapes follow the
Anthropic Messages API convention, which is also the canonical model's
convention — an agent built on that API can emit its message array nearly
verbatim.

```
{
  "id": "...", "timestamp": "...",              session metadata,
  "cwd": "...", "title": "...", "model": "...", all optional
  "messages": [
    {"role": "user",      "content": "..."},        string content, or
    {"role": "assistant", "content": [ ... ]}       Anthropic-style blocks
  ]
}
```

The format is specified as fidelity levels. Every level is the same schema
read further — nothing is enforced per level, any mix parses. An emitter
reads until it has what it needs and stops.

## On disk

Nowhere, deliberately. A Simple session is a document you hand to txcript
directly — there is no managed directory, no discovery, and nothing appears
in `txcript list`. The document itself is the session: keep it wherever you
like, or never materialize it at all and pipe it in.

Handing it over is the entire import story:

```sh
txcript continue ./run.json --with claude_code    # a file
my-agent --dump | txcript continue - --with claude_code    # stdin
```

txcript parses the document, rewrites it as a real session in the target
harness's own store, and launches that harness on it. From that moment the
conversation lives in the target harness; the source document is never
modified.

Simple is an interchange *input*: sessions are continued from Simple
documents, not written into them, so `--with simple` is refused. (The codec
itself is symmetric — the library and WASM APIs can render Simple text —
but txcript manages no location to write such a document to.)

## The format, level by level

### L0 — barebones

The minimum valid document: `messages`, each with `role` and `content` as a
plain string.

```json
{
  "messages": [
    { "role": "user", "content": "fix the off-by-one in pagination" },
    { "role": "assistant", "content": "Fixed - the loop bound was inclusive." }
  ]
}
```

`role` is `"user"` or `"assistant"` (matched case-insensitively). This is
already enough to continue into any harness: txcript synthesizes the
session id, timestamps, and per-harness bookkeeping.

### L1 — tool use

`content` becomes an array of blocks. Five block types are modeled:

| Block | Fields | Notes |
|---|---|---|
| `text` | `text` | |
| `thinking` | `text` | model reasoning; see L6 for provider tokens |
| `tool_use` | `name`, `input`, `id`? | `input` is any JSON, default `null` |
| `tool_result` | `content`, `tool_use_id`?, `is_error`? | `content` is a string or any JSON |
| `image` | `source` | see L6 |

```json
{
  "messages": [
    { "role": "user", "content": "run the tests" },
    { "role": "assistant", "content": [
        { "type": "thinking", "text": "cargo test covers it." },
        { "type": "tool_use", "name": "Bash", "input": { "command": "cargo test" } }
    ] },
    { "role": "user", "content": [
        { "type": "tool_result", "content": "42 passed" }
    ] },
    { "role": "assistant", "content": "All green." }
  ]
}
```

Pairing: a `tool_use` without an `id` gets a deterministic synthetic one; a
`tool_result` without a `tool_use_id` pairs with the oldest preceding
unpaired `tool_use` (first-in-first-out, the Anthropic ordering convention).
Supply explicit ids to pair out of order or to interleave concurrent calls.
`tool_result` blocks ride on `user` messages, per the same convention.

Tool names are free-form. Any name passes through losslessly; names in the
Claude canonical convention (`Bash`, `Read`, `Write`, `Edit`, `MultiEdit`,
with `file_path`/`old_string`/… argument keys) are recognized and render as
typed, native tool calls in the target harness. A name starting with `/` is
a user-invoked command (`{"name": "/release", "input": {"args": "patch"}}`),
paired with whatever the command printed as its `tool_result`.

Skills need no special representation: a model-invoked skill is a `tool_use`
(e.g. name `Skill`) whose loaded instructions arrive as the paired
`tool_result`; a user-invoked skill is a `/command`. Context the
conversation depends on but that no tool call produced (injected memories,
preloaded instructions) belongs inline in `user` content, exactly where the
model saw it. Environment scaffolding the target harness regenerates on
resume (directory listings, git status) is best omitted.

### L2 — model name

`model` at the top level names the session's primary model; `model` on an
assistant message overrides it per turn when it varies.

```json
{
  "model": "claude-opus-5",
  "messages": [
    { "role": "user", "content": "hi" },
    { "role": "assistant", "content": "Hello.", "model": "claude-opus-5" }
  ]
}
```

### L3 — session metadata

Top-level `timestamp` (RFC 3339, when the session started), `cwd`, `title`,
and `git_branch`. `cwd` matters more than it looks: target stores encode it
into the session's on-disk path, and `txcript continue` launches the target
harness from it. `title` is what the target harness's listings and resume
pickers show.

```json
{
  "timestamp": "2026-08-18T10:00:00Z",
  "cwd": "/Users/alice/src/myproj",
  "git_branch": "main",
  "title": "Pagination fix",
  "messages": [ ... ]
}
```

A document without a `timestamp` is stamped with the time it is first
parsed. A message may carry its own `timestamp`; one without inherits the
nearest preceding message's, or the session's.

### L4 — identity

`id` at the top level: the session's identifier, as the emitting agent
knows it. Continuing into a live harness always mints a fresh id for the
copy (so nothing can collide with the target's real sessions), but the
original id is what exports and provenance refer back to. Absent, txcript
derives one from the file name or generates a UUID.

### L5 — accounting

`usage` on assistant messages, and per-message `timestamp`s:

```json
{ "role": "assistant", "content": "Done.",
  "timestamp": "2026-08-18T10:00:12Z",
  "usage": { "input_tokens": 900, "output_tokens": 80,
             "cache_read_input_tokens": 800 } }
```

`input_tokens` and `output_tokens` are required inside `usage` (integers);
the two cache fields are optional. Omit `usage` entirely when unknown.

### L6 — full

The appendix tier: fields nobody hand-writes, present so a conversion *into*
Simple from a real harness drops nothing.

- `stop_reason` on assistant messages: why the turn ended. One of
  `"end_turn"`, `"tool_use"`, `"max_tokens"`, `"stop_sequence"`,
  `"aborted"`, `"error"`, or `{"other": "<verbatim reason>"}`.
- `signature` and `encrypted` on `thinking` blocks: opaque provider
  reasoning tokens (Anthropic signature, encrypted reasoning content),
  carried so a round trip can replay them.
- `image` blocks, Anthropic shape:
  `{"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "<base64>"}}`.
- `cli_version` at the top level: the version of whatever produced the
  session.

## Tolerance

The parser never rejects a document over one bad element:

- A message that fails to parse (a malformed `timestamp`, `content` that is
  neither string nor array) is preserved verbatim in the file and excluded
  from the conversation.
- A block with an unknown `type`, or a message with an unknown `role`, is
  likewise preserved but not conversation.
- Unknown keys — top-level, message-level, block-level — are preserved
  through a same-format round trip. They do not cross into other harnesses.

The only hard errors: the document is not valid JSON, or its top level is
not an object with a `messages` array.

## The format as types

The whole schema, as structural TypeScript types. This block is
self-contained — the reference to hand to a code generator (or an agent)
writing an emitter. The types describe what an emitter writes; the parser
is more tolerant, per the Tolerance section.

```ts
type SimpleDocument = {
  id?: string;              // your agent's session id
  timestamp?: string;       // RFC 3339 session start, e.g. "2026-08-18T10:00:00Z"
  cwd?: string;             // working directory the session ran in
  git_branch?: string;
  title?: string;
  cli_version?: string;     // version of the emitting agent
  model?: string;           // primary model for the session
  messages: Message[];
};

type Message = {
  role: "user" | "assistant";
  content: string | Block[];      // plain text, or Anthropic-style blocks
  timestamp?: string;             // RFC 3339
  model?: string;                 // overrides the session model for this turn
  stop_reason?: StopReason;
  usage?: Usage;
};

type Block =
  | { type: "text"; text: string }
  | { type: "thinking"; text: string; signature?: string; encrypted?: string }
  // Omitted ids pair each id-less tool_result with the oldest unpaired
  // tool_use, first-in-first-out. Canonical Claude tool names (Bash, Read,
  // Write, Edit, MultiEdit) render as typed, native calls in the target
  // harness; any other name passes through losslessly. A name starting
  // with "/" is a user-invoked command.
  | { type: "tool_use"; name: string; input?: unknown; id?: string }
  // content: a string, or any JSON value. Rides on a "user" message.
  | { type: "tool_result"; content?: unknown; tool_use_id?: string; is_error?: boolean }
  | { type: "image"; source: { type: "base64"; media_type: string; data: string } };

type StopReason =
  | "end_turn" | "tool_use" | "max_tokens" | "stop_sequence" | "aborted" | "error"
  | { other: string };

type Usage = {
  input_tokens: number;           // integers
  output_tokens: number;
  cache_read_input_tokens?: number;
  cache_creation_input_tokens?: number;
};
```

Unknown extra keys are allowed at the document, message, and block level;
they survive a Simple round trip but do not cross into other harnesses.

## Caveats

- Simple is the richest interchange surface in this collection: the
  conversion to the canonical model is total (every modeled field has a
  slot), so it also serves as a lossless *export* target. What other
  harnesses cannot represent is lost when converting onward, not here.
- There is no system-prompt slot, deliberately. No harness transports a
  system prompt through conversion — each rebuilds its own environment on
  resume — so a field here would silently die at the hub. Content the
  conversation depends on belongs inline in `user` messages.
- Unknown keys survive a Simple→Simple round trip only; conversion to
  another harness carries the modeled fields.
- Key order in written files is alphabetical (canonicalized by the JSON
  serializer); round-trip fidelity is value-level, not byte-level.

<details>
<summary><strong>Prompt for AI agents</strong> — paste this when asking an agent to convert a transcript to Simple, or to write a transformer that emits it</summary>

````
Target format: "Simple", txcript's interchange JSON — one JSON object.
Any transcript in this format can be continued in Claude Code, Codex, and
other coding agents via `txcript continue <file> --with <harness>`.

type Doc = {
  messages: Msg[];                  // required; everything else optional
  id?: string;                      // the source agent's session id
  timestamp?: string;               // RFC 3339, e.g. "2026-08-18T10:00:00Z"
  cwd?: string; git_branch?: string; title?: string;
  cli_version?: string; model?: string;
};
type Msg = {
  role: "user" | "assistant";
  content: string | Block[];        // plain text, or Anthropic-style blocks
  timestamp?: string; model?: string;
  usage?: { input_tokens: number; output_tokens: number };  // integers
};
type Block =
  | { type: "text"; text: string }
  | { type: "thinking"; text: string }
  | { type: "tool_use"; name: string; input?: unknown; id?: string }
  | { type: "tool_result"; content?: unknown; tool_use_id?: string; is_error?: boolean }
  | { type: "image"; source: { type: "base64"; media_type: string; data: string } };

Rules:
- Emit only fields you have; omit the rest entirely (never null).
- A tool_result rides on the "user" message after its call. Ids are
  optional: an id-less result pairs with the oldest unpaired tool_use.
- Where a tool matches Claude's, use its name and argument keys —
  Bash {command}, Read {file_path}, Write {file_path, content},
  Edit {file_path, old_string, new_string} — it renders as a native tool
  call in the target. Any other name passes through unchanged; both are
  valid. A name starting with "/" is a user-invoked command.
- Skill or command invocations are tool calls; their loaded content is
  the paired tool_result.
- Context the model saw but no tool produced (system reminders, injected
  memory) goes inline in user content. Environment noise the next
  harness regenerates (directory listings, git status) is best dropped.

Full spec, including stop_reason and thinking signatures:
https://github.com/skillsynchq/txcript/blob/main/docs/formats/simple.md
````

</details>

## References

Simple is defined by txcript; there is no upstream. The parser
(`src/harness/simple.rs`) and the integration tests
(`tests/integration/simple.rs`) are the normative mapping.

Last verified: 2026-08-18 (format introduced).
