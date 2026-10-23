<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">在另一個程式開發代理中繼續你的對話。</p>

<p align="center">
  <a href="../../README.md">English</a> | <a href="README.ja.md">日本語</a> | <a href="README.zh-CN.md">简体中文</a> | 繁體中文 | <a href="README.ko.md">한국어</a> | <a href="README.de.md">Deutsch</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (Brasil)</a> | <a href="README.ru.md">Русский</a> | <a href="README.mr.md">मराठी</a> | <a href="README.ta.md">தமிழ்</a>
</p>

<p align="center">
  <a href="https://crates.io/crates/txcript"><img src="https://img.shields.io/crates/v/txcript?logo=rust&color=4c71f2" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/txcript"><img src="https://img.shields.io/npm/v/txcript?logo=npm&color=4c71f2" alt="npm"></a>
  <a href="https://docs.rs/txcript"><img src="https://img.shields.io/docsrs/txcript?logo=docsdotrs" alt="docs.rs"></a>
  <a href="https://github.com/skillsynchq/txcript/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/skillsynchq/txcript/ci.yml?branch=main&logo=github&label=ci" alt="CI"></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-555" alt="授權條款"></a>
</p>

txcript 是用來轉換代理工作階段的函式庫。在 Claude Code 中開始對話，再到 Codex 中繼續。訊息、推理和工具歷史會在目標代理支援的範圍內保留。

使用同一個對話紀錄模型，建立工作階段搜尋、檢視和編輯工具。txcript 負責處理各代理的格式，並提供 Rust API、JavaScript 套件和 CLI。

[試用 CLI](#try-the-cli) · [使用函式庫](#use-the-library) · [支援的代理](#supported-agents) · [文件](#documentation)

<p align="center">
  <img src="../assets/demo.gif" alt="使用 txcript 將 OpenCode 工作階段移至 Claude Code 繼續" width="680">
</p>

<a name="try-the-cli"></a>

## 試用 CLI

從[發行頁面](https://github.com/skillsynchq/txcript/releases)下載適用於 macOS、Linux 或 Windows 的執行檔，或使用 Rust 1.96 以上版本從原始碼安裝：

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli --locked
```

找到一個 Claude Code 工作階段，並在 Codex 中繼續：

```sh
txcript list --from claude_code
txcript continue <session-id> --with codex
```

使用清單中的 ID，也可以使用能唯一識別工作階段的 ID 前綴。txcript 會以目標代理的原生格式寫入新工作階段，並在記錄的工作目錄中啟動 Codex。原始工作階段會保留。繼續之前，請先安裝目標代理並登入。

處理工作階段的其他方式：

```sh
txcript query "relay bug"                # 搜尋本機工作階段歷史
txcript view <session-id>                # 在終端機閱讀對話
txcript crop <session-id>                # 編輯或裁切歷史並存成新副本
txcript export <session-id> --out run.json
```

將 `run.json` 移到另一台電腦，再執行 `txcript continue ./run.json --with claude_code` 繼續對話。專案檔案需要另外移轉。

執行 `txcript mcp`，即可讓 MCP 用戶端列出、搜尋和讀取過去的工作階段。工具皆為唯讀。篩選條件、訊息範圍和 shell 整合請參閱 [CLI 參考](../usage.md#cli)。

<a name="use-the-library"></a>

## 使用函式庫

### Rust

```sh
cargo add txcript
```

將 Claude Code 對話紀錄轉換為 Codex 的原生格式：

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

使用 `Store` 實作，在各代理自己的儲存空間中尋找、載入和儲存工作階段。轉換為 `Transcript<Common>` 後，就能透過同一套 API 搜尋、裁切或呈現不同代理的對話。請參閱 [Rust API](https://docs.rs/txcript) 和[範例](../usage.md#rust-crate)。

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

套件包含適用於 Node 和 Bun 的預先編譯 WebAssembly。它在記憶體中轉換和搜尋工作階段文字，檔案與儲存由你的應用程式處理。請參閱 [JavaScript 參考](../usage.md#npm-package)。

<a name="supported-agents"></a>

## 支援的代理

每個名稱都連結至對應的格式文件。將 ID 用於 `--from` 和 `--with`。

| 代理 | ID | 讀取來源 | 繼續對話 |
|---|---|:---:|:---:|
| [Claude Code](../formats/claude-code.md) | `claude_code` | 支援 | 支援 |
| [Codex](../formats/codex.md) | `codex` | 支援 | 支援 |
| [OpenCode](../formats/opencode.md) | `opencode` | 支援 | 支援 |
| [Cursor CLI](../formats/cursor.md) | `cursor` | 支援 | 支援 |
| [Cursor desktop](../formats/cursor-desktop.md) | `cursor_desktop` | 支援 | 支援 |
| [pi](../formats/pi.md) | `pi` | 支援 | 支援 |
| [Campfire](../formats/campfire.md) | `campfire` | 支援 | 支援 |
| [Cowork](../formats/cowork.md) | `cowork` | 支援 | 支援 |
| [Grok CLI](../formats/grok.md) | `grok` | 支援 | 支援 |
| [Grok Bot](../formats/grok-bot.md) | `grok_bot` | 支援 | 透過本機閘道 |
| [fx](../formats/fx.md) | `fx` | 支援 | 支援 |
| [Antigravity](../formats/antigravity.md) | `antigravity` | 支援 | 支援 |
| [uji](../formats/uji.md) | `uji` | 支援 | 支援 |
| [Hermes Agent](../formats/hermes.md) | `hermes` | 支援 | 不支援 |
| [Amp](../formats/amp.md) | `amp` | 支援 | 不支援 |
| [Claude Chat](../formats/claude-chat.md) | `claude_chat` | 線上帳號 | 不支援 |
| [ChatGPT](../formats/chatgpt.md) | `chatgpt` | 線上帳號 | 不支援 |

本機工作階段搜尋不會存取線上帳號。若要讀取，請明確指定 `--from claude_chat` 或 `--from chatgpt`。這些來源使用私有 Web API，並沿用既有應用程式的登入狀態；需求和限制請見對應文件。

### 串接其他代理

沒有原生介接器的代理可以輸出 [Simple](../formats/simple.md)，也就是 txcript 的 JSON 交換格式。將如下文件儲存為 `run.json`：

```json
{
  "messages": [
    { "role": "user", "content": "找出測試失敗的原因。" },
    { "role": "assistant", "content": "測試時鐘使用的是本地時間。" }
  ]
}
```

```sh
txcript continue ./run.json --with claude_code
```

Simple 也能表示推理、工具呼叫、結果、圖片和中繼資料。`txcript export` 輸出的就是此格式。

## 哪些內容會保留

共用模型可表示訊息、推理、工具呼叫及其結果、圖片、中繼資料和 token 用量。轉換後能保留什麼，取決於來源記錄了什麼，以及目標能夠表示什麼。代理專屬紀錄和不支援的欄位可能會遺失。

轉換會移轉對話歷史。目標代理提供自己的系統指令和工具，專案檔案需要另外準備。原生格式的載入與儲存、格式之間的轉換，兩者的保留保證不同；請參閱[各格式的注意事項](../formats/README.md)。

<a name="documentation"></a>

## 文件

- [CLI 參考](../usage.md#cli)：命令、搜尋、裁切、MCP 和 shell 整合。
- [Rust API](https://docs.rs/txcript) 和 [JavaScript 參考](../usage.md#npm-package)。
- [對話紀錄格式](../formats/README.md)：儲存配置、對應關係和限制，附有來源與逆向工程筆記。
- [開發](../usage.md#development)與[測試指南](../../tests/README.md)。
- [貢獻指南](../../CONTRIBUTING.md) · [回報安全漏洞](../../SECURITY.md).
- [變更紀錄](../../CHANGELOG.md) · [回報問題](https://github.com/skillsynchq/txcript/issues)。

## 授權條款

[Apache-2.0](../../LICENSE)
