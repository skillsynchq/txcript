# ChatGPT

ChatGPT conversations live on `chatgpt.com`, not in a txcript-managed local
directory. ChatGPT has no supported conversation API, and its web API is
private and undocumented. This integration is therefore reverse-engineered
from OpenAI's current OAuth client, a real signed-in account, live list/detail
responses, and independent readers. It is deliberately **pull-only**:
txcript lists and loads conversations but never creates, updates, deletes, or
resumes one in ChatGPT.

This is separate from ChatGPT's account data export. txcript accepts one live
conversation detail object, not an export archive or `conversations.json`.

## Access

Create a login owned by txcript:

```sh
txcript chatgpt login
txcript chatgpt status
txcript list --from chatgpt
```

The login uses OAuth authorization-code flow with PKCE and a localhost
callback. The browser receives the verifier challenge; only the returned code
is exchanged for tokens. Credentials live at
`~/.txcript/chatgpt-auth.json`; on Unix the directory and file are restricted
to modes `0700` and `0600`. txcript never reads browser cookies,
`~/.codex/auth.json`, or another application's credentials. `logout` removes
only txcript's file.

OpenAI currently presents this public installed-app OAuth client as “Codex” on
the consent screen. That is an auth implementation detail, not credential
sharing with an installed Codex client: txcript runs its own PKCE transaction,
can use a different ChatGPT account, and owns its token lifecycle. OpenAI does
not publish third-party client registration for the ChatGPT conversation API,
so this flow can change or be withdrawn.

OAuth code exchange and refresh use `POST https://auth.openai.com/oauth/token`.
Those are the only POST requests in this harness. The conversation API itself
is GET-only.

## Remote store

The current read path is:

1. Paginate `GET /backend-api/conversations` with `offset`, `limit=100`, and
   `order=updated` after the caller explicitly selects `--from chatgpt`.
2. Load one conversation with `GET /backend-api/conversation/{id}`.

An exact UUID can skip the list entirely:

```sh
txcript view 6a8d3401-2098-83ea-8ddd-3d034e6f0a28 --from chatgpt
```

The production conversation origin is fixed to `https://chatgpt.com`; tokens
cannot be redirected to a configured host. Redirects are disabled. Requests
use a browser-compatible transport profile, a bearer token, the selected
`ChatGPT-Account-ID` when present, and an `originator` header. Credential
headers are marked sensitive and server errors are length-limited and scrubbed
before display.

Aggregate operations such as bare `txcript list` never contact ChatGPT.
Discovery requires explicit `--from chatgpt`, prints a warning, and direct Rust
calls to `ChatGptStore::discover()` produce a compile-time deprecation warning.
MCP `list_sessions` refuses live account enumeration. Direct exact-ID reads are
available through the CLI and MCP without a list request.

## Conversation shape

One detail response is an object containing:

```text
conversation_id (or id), title, create_time, update_time
current_node
mapping{
  node_id: {
    id, parent, children[]
    message: {
      id, author.role, create_time, end_turn, recipient
      content: { content_type, parts[], text, thoughts[] }
      metadata: { model_slug, resolved_model_slug, channel, ... }
    }
  }
}
```

The native body keeps every mapping node as raw JSON and flattens all other
server fields into an open map. Unknown fields and inactive branches survive
the native text boundary even when Common cannot represent them.

Messages form a parent-linked tree. The codec follows `current_node` back to
the root and converts that active branch. A missing, cyclic, or broken graph
falls back to creation-time order rather than looping or silently truncating.

| ChatGPT shape | Common mapping |
|---|---|
| user `text` / `multimodal_text` string parts | user `Block::Text` |
| assistant final text | assistant `Block::Text` |
| `thoughts`, `reasoning_recap`, reasoning metadata, assistant `commentary` | `Block::Thinking` |
| assistant message with a non-default `recipient` | `Tool::Raw` using the message id |
| `tool` author message | user-carried `Block::ToolResult`, paired to its parent call |
| `metadata.model_slug` / `resolved_model_slug` | per-message and conversation model |

Tool arguments and results are parsed as JSON when valid and otherwise kept as
text. Visually hidden UI messages are excluded from Common unless they carry a
tool result. System/developer messages remain native and are not emitted as
user or assistant turns.

## Refusals and losses

- `Codec::from_common`, `Store::save`, and `Store::delete` always return a
  source-only error.
- `txcript continue <chatgpt-id>` without another target is refused, as is any
  `--with chatgpt`. Pulling into a writable harness is supported.
- Side branches, system/developer records, citations, feedback/UI state,
  object-valued multimodal parts, attachments, unknown content kinds, and
  fields without a Common slot remain only in the native response.
- OAuth and the private conversation endpoints can drift or be restricted.
  Unsupported status codes and response shapes fail with guidance rather than
  being reported as an empty account.

## References

- [OpenAI Codex OAuth server source](https://github.com/openai/codex/blob/main/codex-rs/login/src/server.rs), the current public installed-app PKCE parameters and token exchange.
- ChatGPT live list and detail responses, verified with a separate signed-in
  personal account on 2026-08-25.
- [Soluna-Angelito ChatGPT Conversation Exporter](https://github.com/Soluna-Angelito/chatgpt-conversation-exporter), an independent reader of the same live mapping and active-node contract.
- Authoritative txcript mapping: `src/harness/chatgpt.rs` and
  `tests/integration/chatgpt.rs`.

Last verified: 2026-08-25.
