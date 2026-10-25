# Transcript formats

One document per harness, describing the on-disk transcript format txcript
reads and writes. These are high-level, human-readable companions to the
parsers — the code in `src/harness/` and the integration tests are the
authoritative mapping; when a document and the code disagree, the code wins
and the document gets fixed.

Every document follows the same structure:

1. **About** — what the harness is and where the knowledge in the document
   comes from (its provenance, see below), followed by a diagram of the
   format's anatomy.
2. **On disk** — where sessions live on the computer, how files are named,
   and how discovery finds them.
3. **Dissection of a transcript** — each part of the format, the name the
   format itself uses for it, and what txcript translates it to.
4. **Caveats** — quirks, drift, and lossiness.
5. **References** — citations, and a `Last verified:` date.

## Provenance

Not all of these formats are documented by their vendors, so each document
states which of three tiers its knowledge comes from:

- **Official documentation** — the vendor publishes a format spec; the
  document cites it with an access date.
- **Open source** — no spec, but the harness's own serialization code is
  public; the document cites permalinks pinned to a commit SHA.
- **Reverse-engineered** — closed source and undocumented; the format is
  described from observed sessions, and the document records the harness
  version and date of observation.

A dated `Last verified:` line closes every document. These formats drift
silently — an undated description of a reverse-engineered format is worse
than none.

## Harnesses

| Document | Harness | Parser |
| --- | --- | --- |
| [claude-code.md](claude-code.md) | Claude Code (Anthropic) | `src/harness/claude_code.rs` |
| [cowork-remote.md](cowork-remote.md) | Cloud Cowork (Anthropic) | `src/harness/cowork_remote.rs` |
| [claude-chat.md](claude-chat.md) | Claude Chat (Anthropic) | `src/harness/claude_chat.rs` |
| [chatgpt.md](chatgpt.md) | ChatGPT (OpenAI) | `src/harness/chatgpt.rs` |
| [cowork.md](cowork.md) | Cowork (Claude desktop app) | `src/harness/cowork.rs` |
| [codex.md](codex.md) | Codex CLI (OpenAI) | `src/harness/codex.rs` |
| [opencode.md](opencode.md) | OpenCode (SST) | `src/harness/opencode.rs` |
| [cursor.md](cursor.md) | Cursor | `src/harness/cursor.rs` |
| [cursor-desktop.md](cursor-desktop.md) | Cursor desktop (IDE app) | `src/harness/cursor_desktop.rs` |
| [amp.md](amp.md) | Amp (Sourcegraph) | `src/harness/amp.rs` |
| [grok.md](grok.md) | Grok CLI (xAI) | `src/harness/grok.rs` |
| [grok-bot.md](grok-bot.md) | Grok Bot (desktop assistant) | `src/harness/grok_bot.rs` |
| [fx.md](fx.md) | fx (Vercel) | `src/harness/fx.rs` |
| [hermes.md](hermes.md) | Hermes Agent | `src/harness/hermes.rs` |
| [antigravity.md](antigravity.md) | Antigravity (Google) | `src/harness/antigravity.rs` |
| [pi.md](pi.md) | pi | `src/harness/pi.rs` |
| [campfire.md](campfire.md) | Campfire (embeds pi) | `src/harness/campfire.rs` |
| [simple.md](simple.md) | Simple (txcript's own interchange format) | `src/harness/simple.rs` |

## Conversion fidelity

What converting *into* each harness loses, measured by `txcript check` (see [Fidelity](../usage.md#fidelity)) on one synthetic session that has every kind of block: signed and unsigned thinking, a Bash call with a description, an edit, an MCP call with a structured result, an error result, a slash command, an image, token usage, stop reasons, per-message models, a title, a git branch. Each cell names the categories that were lost or changed (`ok`: nothing). Items the target only made up, and message timestamps, are not listed.

| Target | Content | Accounting | Metadata |
| --- | --- | --- | --- |
| claude_code | ok | changed: stop_reason | ok |
| codex | lost: tool_args, is_error; changed: tool_name | lost: thinking_signature, usage, stop_reason | lost: title, model |
| opencode | lost: tool_call, tool_result | lost: thinking_signature; changed: stop_reason | lost: git_branch |
| pi | lost: tool_call | lost: thinking_signature | lost: title, model, git_branch |
| campfire | lost: tool_call | lost: thinking_signature | lost: title, model, git_branch |
| cursor | lost: tool_call | lost: usage, stop_reason | lost: git_branch |
| cursor_desktop | lost: tool_call, tool_args, tool_result, image | lost: usage, stop_reason | lost: git_branch |
| grok | lost: tool_call | lost: thinking_signature, usage, stop_reason | ok |
| fx | lost: tool_call, tool_args | lost: usage, stop_reason | lost: git_branch |
| antigravity | lost: tool_call, tool_args, tool_result | lost: message_model | lost: model; changed: title |
| cowork | ok | changed: stop_reason | ok |

The `tool_call` and `tool_result` losses are the slash command, which several formats have no way to record; `tool_name` is it written as `_release`. Hermes, Amp, and Simple can be rendered but not written by `continue`, and `grok_bot` mints a live agent, so `check` leaves them out of its default table. The measurement is codec-level: it does not include what a store adds, such as artifact files. It is a regression oracle as well as documentation: `tests/integration/fidelity.rs` fails when the table drifts from the codecs, and prints the rows to paste.
