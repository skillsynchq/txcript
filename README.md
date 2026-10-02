<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/wordmark-dark.svg">
    <img src="docs/assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">Continue your conversation in another coding agent.</p>

<p align="center">
  English | <a href="docs/translations/README.ja.md">日本語</a> | <a href="docs/translations/README.zh-CN.md">简体中文</a> | <a href="docs/translations/README.zh-TW.md">繁體中文</a> | <a href="docs/translations/README.ko.md">한국어</a> | <a href="docs/translations/README.de.md">Deutsch</a> | <a href="docs/translations/README.es.md">Español</a> | <a href="docs/translations/README.fr.md">Français</a> | <a href="docs/translations/README.it.md">Italiano</a> | <a href="docs/translations/README.pt-BR.md">Português (Brasil)</a> | <a href="docs/translations/README.ru.md">Русский</a> | <a href="docs/translations/README.mr.md">मराठी</a> | <a href="docs/translations/README.ta.md">தமிழ்</a>
</p>

<p align="center">
  <a href="https://crates.io/crates/txcript"><img src="https://img.shields.io/crates/v/txcript?logo=rust&color=4c71f2" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/txcript"><img src="https://img.shields.io/npm/v/txcript?logo=npm&color=4c71f2" alt="npm"></a>
  <a href="https://docs.rs/txcript"><img src="https://img.shields.io/docsrs/txcript?logo=docsdotrs" alt="docs.rs"></a>
  <a href="https://github.com/skillsynchq/txcript/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/skillsynchq/txcript/ci.yml?branch=main&logo=github&label=ci" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-555" alt="License"></a>
</p>

txcript is a library for converting agent sessions. Start a conversation in Claude Code and continue it in Codex, carrying over messages, reasoning, and tool history where the target supports them.

Build session search, viewers, and editors against one transcript model. txcript handles the agent-specific formats, with a Rust API, a JavaScript package, and a CLI.

[Try the CLI](#try-the-cli) · [Use the library](#use-the-library) · [Supported agents](#supported-agents) · [Documentation](#documentation)

<p align="center">
  <img src="docs/assets/demo.gif" alt="An OpenCode session continued in Claude Code using txcript" width="680">
</p>

## Try the CLI

Download a binary for macOS, Linux, or Windows from [Releases](https://github.com/skillsynchq/txcript/releases), or install from source with Rust 1.96 or newer:

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli --locked
```

Find a Claude Code session and continue it in Codex:

```sh
txcript list --from claude_code
txcript continue <session-id> --with codex
```

Use an ID from the list; an unambiguous prefix works too. txcript writes a new native session and launches Codex in the recorded working directory. The source session is kept. Have the target agent installed and signed in before continuing.

Other ways to work with your sessions:

```sh
txcript query "relay bug"                # search local session history
txcript view <session-id>                # read a conversation in the terminal
txcript crop <session-id>                # edit or trim history into a new copy
txcript export <session-id> --out run.json
```

Move `run.json` to another machine and continue it with `txcript continue ./run.json --with claude_code`. Bring the project files separately.

Run `txcript mcp` to let an MCP client list, search, and read past sessions. Its tools are read-only. See the [CLI reference](docs/usage.md#cli) for filters, message ranges, and shell integration.

## Use the library

### Rust

```sh
cargo add txcript
```

Convert a Claude Code transcript into Codex's native format:

```rust
use txcript::harness::{claude_code::ClaudeCode, codex::Codex};
use txcript::{TextCodec, convert};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = std::fs::read_to_string("session.jsonl")?;
    let source = ClaudeCode::from_text(&input)?;
    let target = convert::<ClaudeCode, Codex>(&source)?;
    std::fs::write("rollout.jsonl", Codex::to_text(&target)?)?;
    Ok(())
}
```

Use `Store` implementations to discover, load, and save sessions in an agent's own storage. Convert to `Transcript<Common>` to search, crop, or render conversations through the same API across agents. See the [Rust API](https://docs.rs/txcript) and [examples](docs/usage.md#rust-crate).

### JavaScript

```sh
npm install txcript
```

```js
import { convert } from "txcript";
import { readFileSync, writeFileSync } from "node:fs";

const input = readFileSync("session.jsonl", "utf8");
const output = convert(input, "claude_code", "codex");
writeFileSync("rollout.jsonl", output);
```

The package includes prebuilt WebAssembly for Node and Bun. It converts and searches session text in memory; your application handles files and storage. See the [JavaScript reference](docs/usage.md#npm-package).

## Supported agents

Each name links to its format documentation. Use the ID with `--from` and `--with`.

| Agent | ID | Read from | Continue into |
|---|---|:---:|:---:|
| [Claude Code](docs/formats/claude-code.md) | `claude_code` | Yes | Yes |
| [Codex](docs/formats/codex.md) | `codex` | Yes | Yes |
| [OpenCode](docs/formats/opencode.md) | `opencode` | Yes | Yes |
| [Cursor CLI](docs/formats/cursor.md) | `cursor` | Yes | Yes |
| [Cursor desktop](docs/formats/cursor-desktop.md) | `cursor_desktop` | Yes | Yes |
| [pi](docs/formats/pi.md) | `pi` | Yes | Yes |
| [Campfire](docs/formats/campfire.md) | `campfire` | Yes | Yes |
| [Cowork](docs/formats/cowork.md) | `cowork` | Yes | Yes |
| [Grok CLI](docs/formats/grok.md) | `grok` | Yes | Yes |
| [Grok Bot](docs/formats/grok-bot.md) | `grok_bot` | Yes | Via local gateway |
| [fx](docs/formats/fx.md) | `fx` | Yes | Yes |
| [Antigravity](docs/formats/antigravity.md) | `antigravity` | Yes | Yes |
| [uji](docs/formats/uji.md) | `uji` | Yes | Yes |
| [Hermes Agent](docs/formats/hermes.md) | `hermes` | Yes | No |
| [Amp](docs/formats/amp.md) | `amp` | Yes | No |
| [Cloud Cowork](docs/formats/cowork-remote.md) | `cowork_remote` | Live account | No |
| [Claude Chat](docs/formats/claude-chat.md) | `claude_chat` | Live account | No |
| [ChatGPT](docs/formats/chatgpt.md) | `chatgpt` | Live account | No |

Local discovery skips the live accounts. Select `--from claude_chat`, `--from cowork_remote`, or `--from chatgpt` explicitly to read them. These sources use private web APIs and reuse an existing app login; requirements and limitations are in their linked docs.

### Bring another agent

An agent without a native adapter can emit [Simple](docs/formats/simple.md), txcript's interchange JSON. Save a document like this as `run.json`:

```json
{
  "messages": [
    { "role": "user", "content": "Find why the tests fail." },
    { "role": "assistant", "content": "The test clock is using local time." }
  ]
}
```

```sh
txcript continue ./run.json --with claude_code
```

Simple also represents reasoning, tool calls, results, images, and metadata. It is the format `txcript export` writes.

## What carries over

The common model represents messages, reasoning, tool calls and results, images, metadata, and token usage. What survives conversion depends on what the source records and the destination can represent. Agent-specific records and unsupported fields can be lost.

Conversion carries conversation history. The destination supplies its own system instructions and tools, and project files must be available separately. Native load/save and conversion have different preservation guarantees; see each [format's caveats](docs/formats/README.md).

## Documentation

- [CLI reference](docs/usage.md#cli): commands, search, cropping, MCP, and shell integration.
- [Rust API](https://docs.rs/txcript) and [JavaScript reference](docs/usage.md#npm-package).
- [Transcript formats](docs/formats/README.md): storage layouts, mappings, and limitations, with sources and reverse-engineering notes.
- [Development](docs/usage.md#development) and [test guide](tests/README.md).
- [Contributing](CONTRIBUTING.md) · [Report a security vulnerability](SECURITY.md).
- [Changelog](CHANGELOG.md) · [Report an issue](https://github.com/skillsynchq/txcript/issues).

## License

[Apache-2.0](LICENSE)
