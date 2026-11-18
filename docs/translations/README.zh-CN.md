<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">在另一个编程代理中继续你的对话。</p>

<p align="center">
  <a href="../../README.md">English</a> | <a href="README.ja.md">日本語</a> | 简体中文 | <a href="README.zh-TW.md">繁體中文</a> | <a href="README.ko.md">한국어</a> | <a href="README.de.md">Deutsch</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (Brasil)</a> | <a href="README.ru.md">Русский</a> | <a href="README.mr.md">मराठी</a> | <a href="README.ta.md">தமிழ்</a>
</p>

<p align="center">
  <a href="https://crates.io/crates/txcript"><img src="https://img.shields.io/crates/v/txcript?logo=rust&color=4c71f2" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/txcript"><img src="https://img.shields.io/npm/v/txcript?logo=npm&color=4c71f2" alt="npm"></a>
  <a href="https://docs.rs/txcript"><img src="https://img.shields.io/docsrs/txcript?logo=docsdotrs" alt="docs.rs"></a>
  <a href="https://github.com/skillsynchq/txcript/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/skillsynchq/txcript/ci.yml?branch=main&logo=github&label=ci" alt="CI"></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-555" alt="许可证"></a>
</p>

txcript 是一个用于转换代理会话的库。在 Claude Code 中开始对话，再到 Codex 中继续。消息、推理和工具历史会在目标代理支持的范围内保留。

基于同一个会话记录模型构建搜索、查看和编辑工具。txcript 负责处理各代理的格式，并提供 Rust API、JavaScript 包和 CLI。

[试用 CLI](#try-the-cli) · [使用库](#use-the-library) · [支持的代理](#supported-agents) · [文档](#documentation)

<p align="center">
  <img src="../assets/demo.gif" alt="使用 txcript 将 OpenCode 会话迁移到 Claude Code 中继续" width="680">
</p>

<a name="try-the-cli"></a>

## 试用 CLI

从[发布页面](https://github.com/skillsynchq/txcript/releases)下载适用于 macOS、Linux 或 Windows 的二进制文件，或使用 Rust 1.96 及以上版本从源码安装：

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli --locked
```

找到一个 Claude Code 会话，并在 Codex 中继续：

```sh
txcript list --from claude_code
txcript continue <session-id> --with codex
```

使用列表中的 ID，也可以使用能唯一确定会话的 ID 前缀。txcript 会以目标代理的原生格式写入新会话，并在记录的工作目录中启动 Codex。源会话会保留。继续之前，请先安装目标代理并登录。

处理会话的其他方式：

```sh
txcript query "relay bug"                # 搜索本地会话历史
txcript view <session-id>                # 在终端中阅读对话
txcript crop <session-id>                # 编辑或裁剪历史并保存为新副本
txcript export <session-id> --out run.json
```

将 `run.json` 移到另一台机器，然后运行 `txcript continue ./run.json --with claude_code` 继续对话。项目文件需要单独迁移。

运行 `txcript mcp`，即可让 MCP 客户端列出、搜索和读取过去的会话。工具均为只读。筛选条件、消息范围和 shell 集成请参阅 [CLI 参考](../usage.md#cli)。

<a name="use-the-library"></a>

## 使用库

### Rust

```sh
cargo add txcript
```

将 Claude Code 会话记录转换为 Codex 的原生格式：

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

使用 `Store` 实现，在各代理自己的存储中发现、加载和保存会话。转换为 `Transcript<Common>` 后，就能通过同一套 API 搜索、裁剪或呈现不同代理的对话。请参阅 [Rust API](https://docs.rs/txcript) 和[示例](../usage.md#rust-crate)。

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

该包包含适用于 Node 和 Bun 的预编译 WebAssembly。它在内存中转换和搜索会话文本，文件与存储由你的应用处理。请参阅 [JavaScript 参考](../usage.md#npm-package)。

<a name="supported-agents"></a>

## 支持的代理

每个名称都链接到对应的格式文档。将 ID 用于 `--from` 和 `--with`。

| 代理 | ID | 读取来源 | 继续会话 |
|---|---|:---:|:---:|
| [Claude Code](../formats/claude-code.md) | `claude_code` | 支持 | 支持 |
| [Codex](../formats/codex.md) | `codex` | 支持 | 支持 |
| [OpenCode](../formats/opencode.md) | `opencode` | 支持 | 支持 |
| [Cursor CLI](../formats/cursor.md) | `cursor` | 支持 | 支持 |
| [Cursor desktop](../formats/cursor-desktop.md) | `cursor_desktop` | 支持 | 支持 |
| [pi](../formats/pi.md) | `pi` | 支持 | 支持 |
| [Campfire](../formats/campfire.md) | `campfire` | 支持 | 支持 |
| [Cowork](../formats/cowork.md) | `cowork` | 支持 | 支持 |
| [Grok CLI](../formats/grok.md) | `grok` | 支持 | 支持 |
| [Grok Bot](../formats/grok-bot.md) | `grok_bot` | 支持 | 通过本地网关 |
| [fx](../formats/fx.md) | `fx` | 支持 | 支持 |
| [Antigravity](../formats/antigravity.md) | `antigravity` | 支持 | 支持 |
| [uji](../formats/uji.md) | `uji` | 支持 | 支持 |
| [Hermes Agent](../formats/hermes.md) | `hermes` | 支持 | 不支持 |
| [Amp](../formats/amp.md) | `amp` | 支持 | 不支持 |
| [Claude Chat](../formats/claude-chat.md) | `claude_chat` | 在线账户 | 不支持 |
| [ChatGPT](../formats/chatgpt.md) | `chatgpt` | 在线账户 | 不支持 |

本地会话发现不会访问在线账户。要读取它们，请显式指定 `--from claude_chat` 或 `--from chatgpt`。这些来源使用私有 Web API，并复用已有应用的登录状态；要求和限制见对应文档。

### 接入其他代理

没有原生适配器的代理可以输出 [Simple](../formats/simple.md)，即 txcript 的 JSON 交换格式。将以下这样的文档保存为 `run.json`：

```json
{
  "messages": [
    { "role": "user", "content": "找出测试失败的原因。" },
    { "role": "assistant", "content": "测试时钟使用的是本地时间。" }
  ]
}
```

```sh
txcript continue ./run.json --with claude_code
```

Simple 也能表示推理、工具调用、结果、图片和元数据。`txcript export` 输出的就是此格式。

## 哪些内容会保留

公共模型可表示消息、推理、工具调用及其结果、图片、元数据和 token 用量。转换后能保留什么，取决于来源记录了什么，以及目标能够表示什么。代理专有记录和不支持的字段可能会丢失。

转换会迁移对话历史。目标代理提供自己的系统指令和工具，项目文件需要另外准备。原生格式的加载与保存、格式间的转换，两者的保留保证不同；请参阅[各格式的注意事项](../formats/README.md)。

<a name="documentation"></a>

## 文档

- [CLI 参考](../usage.md#cli)：命令、搜索、裁剪、MCP 和 shell 集成。
- [Rust API](https://docs.rs/txcript) 和 [JavaScript 参考](../usage.md#npm-package)。
- [会话记录格式](../formats/README.md)：存储布局、映射关系和限制，附有来源与逆向工程笔记。
- [开发](../usage.md#development)和[测试指南](../../tests/README.md)。
- [贡献指南](../../CONTRIBUTING.md) · [报告安全漏洞](../../SECURITY.md).
- [更新日志](../../CHANGELOG.md) · [报告问题](https://github.com/skillsynchq/txcript/issues)。

## 许可证

[Apache-2.0](../../LICENSE)
