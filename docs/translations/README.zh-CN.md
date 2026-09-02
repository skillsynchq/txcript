<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">一个在 harness 之间迁移会话的库</p>

<p align="center">
  <a href="../../README.md">English</a> | <a href="README.ja.md">日本語</a> | 简体中文 | <a href="README.zh-TW.md">繁體中文</a> | <a href="README.ko.md">한국어</a> | <a href="README.de.md">Deutsch</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (Brasil)</a> | <a href="README.ru.md">Русский</a> | <a href="README.mr.md">मराठी</a> | <a href="README.ta.md">தமிழ்</a>
</p>

<p align="center">
  <a href="https://crates.io/crates/txcript"><img src="https://img.shields.io/crates/v/txcript?logo=rust&color=4c71f2" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/txcript"><img src="https://img.shields.io/npm/v/txcript?logo=npm&color=4c71f2" alt="npm"></a>
  <a href="https://docs.rs/txcript"><img src="https://img.shields.io/docsrs/txcript?logo=docsdotrs" alt="docs.rs"></a>
  <a href="https://github.com/skillsynchq/txcript/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/skillsynchq/txcript/ci.yml?branch=main&logo=github&label=ci" alt="CI"></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-555" alt="License"></a>
</p>

<p align="center">
  <a href="https://claude.com/claude-code"><img src="../assets/claude-icon.svg" alt="Claude Code" height="44" width="44"></a>
  &nbsp;&nbsp;&nbsp;&nbsp;
  <a href="https://github.com/openai/codex"><img src="https://github.com/openai.png?size=160" alt="Codex" height="44" width="44"></a>
  &nbsp;&nbsp;&nbsp;&nbsp;
  <a href="https://opencode.ai"><img src="https://opencode.ai/apple-touch-icon-v3.png" alt="OpenCode" height="44" width="44"></a>
  &nbsp;&nbsp;&nbsp;&nbsp;
  <a href="https://pi.dev"><img src="https://pi.dev/logo-auto.svg" alt="pi" height="44" width="44"></a>
  &nbsp;&nbsp;&nbsp;&nbsp;
  <a href="https://cursor.com"><img src="https://github.com/cursor.png?size=160" alt="Cursor" height="44" width="44"></a>
  &nbsp;&nbsp;&nbsp;&nbsp;
  <a href="https://github.com/xai-org/grok-build"><img src="https://github.com/xai-org.png?size=160" alt="Grok CLI" height="44" width="44"></a>
  &nbsp;&nbsp;&nbsp;&nbsp;
  <a href="https://ampcode.com"><img src="https://ampcode.com/app-icon.png?v=3" alt="Amp" height="44" width="44"></a>
  &nbsp;&nbsp;&nbsp;&nbsp;
  <a href="https://antigravity.google"><img src="https://github.com/google-antigravity.png?size=160" alt="Antigravity" height="44" width="44"></a>
</p>

在 Claude Code 中开始一个会话，遇到用量限制或卡壳时，换到 Codex 里接着做，完整的对话、推理和工具历史原样保留：

<p align="center">
  <img src="../assets/demo.gif" alt="txcript continue: an OpenCode session resumed in Claude Code">
</p>

txcript 通过一个带类型的公共模型来映射各个 harness 的原生会话记录格式。原生加载/保存是字节级无损的；跨 harness 转换会保留消息、推理、工具调用、工具结果、图像、元数据，以及在可用时的用量信息。它以 [**CLI**](#cli)、[**Rust crate**](#rust-crate) 和 [**npm 包**](#npm-包) 的形式发布。

## 亮点

- **10 个 harness，一个模型**：所有格式都经由 `Transcript<Common>` 转换，因此新增一个 harness 就等于把它接入其他所有 harness。
- **字节级无损往返**：以会话自身的格式加载并保存，可以原样复现。
- **随处继续**：`txcript continue <id> --with <harness>` 会把会话重写为另一个 harness 的原生格式并启动它。原始会话绝不会被修改。
- **搜索一切**：对本机上的所有会话做模糊/子串搜索（fzf 风格语法，由 [nucleo](https://github.com/helix-editor/nucleo) 驱动），可作为库 API、一次性 CLI 查询或交互式选择器使用。
- **MCP 服务器**：`txcript mcp` 暴露只读的 `list_sessions`、`search_sessions` 和 `read_session` 工具，让智能体可以把过往会话作为上下文来挖掘。
- **格式文档齐全**：每个 harness 的磁盘格式都在 [`docs/formats/`](../formats) 中有完整记述，且每条论断都注明出处（官方文档、源码 permalink 或逆向工程笔记）。

## 支持的 harness

```mermaid
flowchart LR
    claude["Claude Code"] <--> common(("Transcript&lt;Common&gt;"))
    codex["Codex"] <--> common
    opencode["OpenCode"] <--> common
    pi["pi"] <--> common
    campfire["Campfire"] <--> common
    common <--> cursor["Cursor CLI"]
    common <--> cursordesktop["Cursor desktop"]
    common <--> grok["Grok CLI"]
    common <--> antigravity["Antigravity"]
    amp["Amp"] --> common
```

发现、列表、搜索、`view` 以及原生往返对每个 harness 均可用。CLI 和 WASM API 接受的正是这些 `id` 字符串。

| Harness | id | 磁盘上的会话 | 原生格式 | 转换 | 可继续到 | 文档 |
|---|---|---|---|:---:|:---:|---|
| [Claude Code](https://claude.com/claude-code) | `claude_code` | `~/.claude/projects/` | JSONL | ⇄ | ✓ | [规格](../formats/claude-code.md) |
| [Codex](https://github.com/openai/codex) | `codex` | `~/.codex/sessions/` | rollout JSONL | ⇄ | ✓ | [规格](../formats/codex.md) |
| [OpenCode](https://opencode.ai) | `opencode` | `~/.local/share/opencode/opencode.db` | SQLite | ⇄ | ✓ | [规格](../formats/opencode.md) |
| [pi](https://pi.dev) | `pi` | `~/.pi/agent/sessions/` | JSONL | ⇄ | ✓ | [规格](../formats/pi.md) |
| [Campfire](../formats/campfire.md) | `campfire` | `~/.campfire/agent/sessions/` | JSONL | ⇄ | ✓ | [规格](../formats/campfire.md) |
| [Cursor CLI](https://cursor.com/cli) | `cursor` | `~/.cursor/chats/` | SQLite | ⇄ | ✓ | [规格](../formats/cursor.md) |
| [Cursor desktop](https://cursor.com) | `cursor_desktop` | `<Cursor User dir>/globalStorage/` | SQLite | ⇄ | ✓ | [规格](../formats/cursor-desktop.md) |
| [Grok CLI](https://github.com/xai-org/grok-build) | `grok` | `~/.grok/sessions/` | 会话目录（JSON） | ⇄ | ✓ | [规格](../formats/grok.md) |
| [Amp](https://ampcode.com) | `amp` | `~/.local/share/amp/threads/` | 线程 JSON | → | — <sup>1</sup> | [规格](../formats/amp.md) |
| [Antigravity](https://antigravity.google) | `antigravity` | `~/.gemini/antigravity-cli/` | SQLite | ⇄ | ✓ | [规格](../formats/antigravity.md) |

<sup>1</sup> Amp 的线程保存在服务端，且 CLI 没有导入功能：会话可以*从* Amp 转换，但无法继续到 Amp 中。

## 安装

**CLI**（安装 `txcript` 二进制）：

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli
# or from a checkout: cargo install --path cli
```

**Rust crate**：

```sh
cargo add txcript
```

**npm 包**（预编译 WASM，无需 Rust 工具链）：

```sh
bun add txcript     # or: npm install txcript
```

## CLI

发现本地会话，并在任意 harness 中继续其中一个：

```sh
txcript list                             # local sessions across every harness
txcript continue <id>[#range]            # continue <id>, then launch its harness
    [--with <harness>]                    #   ...continuing in <harness> instead
    [--from <harness>]                    #   scope the id lookup to one harness
    [--out <dir>]                         #   write under <dir>; implies --no-resume
    [--no-resume]                         #   write the session but don't launch
txcript view <id>[#range]                # print a session as compact text
    [--from <harness>]                    #   scope the id lookup to one harness
txcript export <id>[#range]              # write a session as a Simple document
    [--from <harness>]                    #   scope the id lookup to one harness
    [--out <file>]                        #   write to <file> instead of stdout
```

`continue` 会把会话写到目标 harness 保存会话的位置，然后启动该 harness 打开它，并把终端交给它：

- 同 harness：就地恢复原会话。
- 跨 harness（`--with`）：把会话重新合成为目标的原生格式。写出的始终是一份副本；源会话绝不会被修改或删除。
- 启动命令按 harness 各自设定，且可覆盖：将 `TRANSCRIPT_<HARNESS>_RESUME_CMD` 设为一个 `{id}` 模板，例如 `TRANSCRIPT_CODEX_RESUME_CMD="codex resume {id}"`。

`view` 会把会话打印为紧凑文本，每条消息由一条 `── #N ──` 分隔线编号。`#range` 按这些打印出的序号选择消息，从 1 开始且两端闭合：

- `abc#7`：仅第 7 条消息
- `abc#5-12`：第 5 到第 12 条消息
- `abc#5-`：从第 5 条消息到末尾
- `abc#-10`：从开头到第 10 条消息

`continue` 接受同样的后缀，只把这些消息作为新会话继续。会把工具调用与其结果拆开的范围会被拒绝，错误信息会给出最接近的有效范围建议。

`export` 把会话写为 [Simple](../formats/simple.md) 文档，输出到 stdout 或 `--out <file>`。该文档是规范模型的完整呈现——`continue` 在 harness 之间携带的一切——脱离了任何 harness 保存会话的位置，因此可以作为一个文件从一台机器移动到另一台机器：

```sh
txcript export 0dc114bf --out session.json       # on this machine
txcript continue ./session.json --with claude_code   # on the other one
```

记录的工作目录，如果在导入机器上存在就会保留，否则会被 `continue` 运行所在的目录替换。`export` 接受与 `view` 相同的 `#range` 后缀和 `--from` 范围。

### 搜索

```sh
txcript query 'relay bug'                # one-shot: ranked hits, highlighted
txcript query                            # fzf-style picker; Enter continues
    [--from <harness>]                   #   search only <harness> (default: all)
    [--with <harness>]                   #   continue the pick in <harness>
```

选择器不依赖任何第三方库（raw 模式 ANSI）：输入即可用 fzf 风格的模糊语法过滤，方向键 / ctrl-p/n 移动，Enter 在其自身的 harness（或 `--with` 指定的 harness）中继续所选会话，Esc 取消。每一行都会显示匹配到的内容类型：用户文本、助手文本、思考、工具调用、工具输出或会话元数据。

### MCP 服务器

```sh
txcript mcp                              # stdio transport
```

暴露三个只读工具；它们的可选过滤参数与 CLI 一致：

- `list_sessions(from?, cwd?)`
- `search_sessions(pattern, from?, cwd?)`
- `read_session(id, from?)`

<sub>\* 省略 `from` 时包含所有 harness；省略 `cwd` 时不做目录过滤。没有记录工作目录的会话，只有在省略 `cwd` 时才会被匹配。</sub>

### Shell 补全

```sh
txcript completion zsh > ~/.zfunc/_txcript      # or wherever your fpath looks
source <(txcript completion bash)               # bash, ad hoc
txcript completion fish > ~/.config/fish/completions/txcript.fish
```

## Rust crate

```toml
[dependencies]
txcript = "0.6"
# Drops the OpenCode SQLite store (rusqlite); the OpenCode codec stays available.
# txcript = { version = "0.6", default-features = false }
```

三个层次，由小到大：

- `Codec`：每个 harness 的 `to_common` / `from_common`；`convert::<A, B>` 通过规范模型把它们串联起来。
- `TextCodec`：`from_text` / `to_text`，用于解析和渲染 harness 的原生会话文本，不做任何 I/O。
- `Store`：针对真实后端做发现/加载/保存（会话目录，或 OpenCode 与两种 Cursor 的 SQLite 数据库）。

在内存中转换（不经过文件系统）：

```rust
use txcript::harness::{claude_code, codex};
use txcript::{Codec, TextCodec, convert};

let claude = claude_code::ClaudeCode::from_text(jsonl_text)?;          // Transcript<ClaudeCode>
let codex = convert::<claude_code::ClaudeCode, codex::Codex>(&claude)?; // Transcript<Codex>
let codex_text = codex::Codex::to_text(&codex)?;                       // native rollout JSONL
```

或者通过 `Store` 走磁盘：

```rust
use txcript::harness::{claude_code, codex};
use txcript::{Store, convert};

let store = claude_code::ClaudeStore::default_root().expect("home dir");
let found = store.discover()?;                       // cheap metadata scan
let claude = store.load(&found[0].reference)?;       // Transcript<ClaudeCode>

let codex = convert::<_, codex::Codex>(&claude)?;
codex::CodexStore::default_root().expect("home dir").save(&codex)?;  // resumable on disk
```

规范模型是 `Transcript<Common>`：`Meta` + `Vec<Message>`，其中 `Message` 持有带类型的 `Block`（`Text`、`Thinking`、`ToolUse`、`ToolResult`、`Image`）和一个带类型的 `Tool` 枚举。

用户在 harness 中运行的斜杠命令（`/release patch`）同样是规范的：它是用户轮次上的一次 `Tool::Command` 调用，并与命令打印回来的内容作为其 `ToolResult` 配对。

### 搜索（`search` feature，默认启用）

`txcript::search` 通过 [nucleo](https://github.com/helix-editor/nucleo) 支持对会话记录的模糊与子串搜索。一次性搜索：

```rust
use txcript::search::{Query, search};

let hits = search(&common, &Query::fuzzy("relay bug"));   // fzf syntax: 'exact ^prefix !not
for hit in hits {
    // hit.origin: User | Assistant | Thinking | ToolUse | ToolResult | Meta
    // hit.span addresses the message; hit.highlights are char ranges into hit.line
    let messages = common.fragment(&hit.span);            // zero-copy: Option<&[Message]>
}
```

若要做选择器式搜索，先构建一次 `Index`，然后随每次按键查询：

```rust
use txcript::search::{DocKey, Index, Query};

let mut index = Index::new();
index.insert(DocKey { harness, id }, &common);   // re-insert replaces; caller owns refresh
let matches = index.query(&Query::fuzzy("srch")); // ranked docs, best lines as hits
```

空模式会按最新在前返回文档。工具输出默认被排除；使用 `Origin::ALL` 可以包含它们。`Query.harnesses`、`Query.limit` 和 `Query.hits_per_doc` 用来收窄结果。

### 文本投影

`txcript::text::to_text(&common)` 是 [`txcript view`](#cli) 背后的投影：一份单向、注重 token 开销的 `Transcript<Common>` 渲染，用作 LLM 上下文。它保留消息、推理文本和紧凑的工具调用/结果；仅用于回放的载荷（加密推理、用量记账、内联图像字节）会被省略。`to_text_fragment(&common, &span)` 渲染正文的一个 `Span`，并保留每条消息在完整会话中的序号。

## npm 包

npm 包把 codec 编译为面向 Bun、Node 和浏览器的预编译 WASM。所有 I/O 由 JS 宿主负责，仅在需要转换时调用进来；`Store` 层（文件系统、SQLite、子进程）保持原生实现，不包含在 WASM 构建中。

```ts
import { convert, toCommon, fromCommon, harnesses } from "txcript";
import { readFileSync, writeFileSync } from "node:fs";

const input = readFileSync("rollout.jsonl", "utf8");

// native -> native (e.g. a Codex rollout into Claude Code's JSONL)
writeFileSync("session.jsonl", convert(input, "codex", "claude_code"));

// canonical view, and back
const common = JSON.parse(toCommon(input, "codex"));   // { meta, messages }
const pi = fromCommon(JSON.stringify(common), "pi");

harnesses(); // ["claude_code","codex","opencode","pi","campfire","cursor","cursor_desktop","grok","amp","antigravity"]
```

文本进 / 文本出：`input` 是源 harness 的原生会话文本，结果是目标 harness 的原生会话文本。无效的 harness 名称或无法解析的输入会抛出 JS `Error`。

| Harness | 会话文本 |
|---|---|
| `claude_code`, `codex`, `pi`, `campfire` | 会话 JSONL |
| `opencode` | `opencode export` 输出的 JSON |
| `cursor` | 该会话 `store.db` 的 JSON 导出 |
| `cursor_desktop` | 该会话 `state.vscdb` 行的 JSON 转储 |
| `grok` | 会话目录中各文件打包成的 JSON |
| `amp` | `amp threads export` 输出的 JSON |
| `antigravity` | 对话数据库的 JSON 转储，protobuf blob 以十六进制编码 |

若要改为从源码构建 wasm：

```sh
git clone https://github.com/skillsynchq/txcript.git
cd txcript
bun run setup        # once: wasm target + wasm-bindgen-cli
bun run build        # produces ./pkg
```

## 格式文档

这些会话记录格式并非都有厂商提供的官方文档。[`docs/formats/`](../formats) 为每个 harness 提供一份文档，涵盖会话在磁盘上的位置、发现机制如何找到它们、对格式各部分的逐一剖析及其怪癖，并且每条论断都标注了出处：官方文档、harness 自身的开源序列化代码（附有锁定到具体 commit 的 permalink），或逆向工程。

## 开发

```sh
cargo test                                          # native suite
cargo test --no-default-features                    # without the SQLite store
bun run build && bun examples/convert.ts <file> <from> <to>
```

二进制程序位于独立的 workspace crate（`cli/`，包名 `txcript-cli`）中，因此它的依赖（clap）不会波及库的使用者。

## 许可证

[Apache-2.0](../../LICENSE)
