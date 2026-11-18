<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">別のコーディングエージェントで会話を続けられます。</p>

<p align="center">
  <a href="../../README.md">English</a> | 日本語 | <a href="README.zh-CN.md">简体中文</a> | <a href="README.zh-TW.md">繁體中文</a> | <a href="README.ko.md">한국어</a> | <a href="README.de.md">Deutsch</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (Brasil)</a> | <a href="README.ru.md">Русский</a> | <a href="README.mr.md">मराठी</a> | <a href="README.ta.md">தமிழ்</a>
</p>

<p align="center">
  <a href="https://crates.io/crates/txcript"><img src="https://img.shields.io/crates/v/txcript?logo=rust&color=4c71f2" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/txcript"><img src="https://img.shields.io/npm/v/txcript?logo=npm&color=4c71f2" alt="npm"></a>
  <a href="https://docs.rs/txcript"><img src="https://img.shields.io/docsrs/txcript?logo=docsdotrs" alt="docs.rs"></a>
  <a href="https://github.com/skillsynchq/txcript/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/skillsynchq/txcript/ci.yml?branch=main&logo=github&label=ci" alt="CI"></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-555" alt="ライセンス"></a>
</p>

txcript はエージェントのセッションを変換するライブラリです。Claude Code で始めた会話を Codex で続けられます。メッセージ、推論、ツール履歴は、移行先が対応している範囲で引き継がれます。

共通の会話モデルを使って、セッションの検索・閲覧・編集ツールを作れます。エージェントごとの形式は txcript が処理し、Rust API、JavaScript パッケージ、CLI を提供します。

[CLI を試す](#try-the-cli) · [ライブラリを使う](#use-the-library) · [対応エージェント](#supported-agents) · [ドキュメント](#documentation)

<p align="center">
  <img src="../assets/demo.gif" alt="txcript を使って OpenCode のセッションを Claude Code で続ける" width="680">
</p>

<a name="try-the-cli"></a>

## CLI を試す

[リリース](https://github.com/skillsynchq/txcript/releases)から macOS、Linux、Windows 向けのバイナリをダウンロードするか、Rust 1.96 以降でソースからインストールしてください。

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli --locked
```

Claude Code のセッションを探し、Codex で続けます。

```sh
txcript list --from claude_code
txcript continue <session-id> --with codex
```

一覧に表示された ID を指定します。一意に特定できる接頭辞でも構いません。txcript は移行先のネイティブ形式で新しいセッションを書き込み、記録された作業ディレクトリで Codex を起動します。元のセッションは保持されます。続行する前に、移行先のエージェントをインストールしてログインしてください。

セッションは次のようにも扱えます。

```sh
txcript query "relay bug"                # ローカルのセッション履歴を検索
txcript view <session-id>                # ターミナルで会話を読む
txcript crop <session-id>                # 履歴を編集・切り出して新しいコピーを作成
txcript export <session-id> --out run.json
```

`run.json` を別のマシンに移し、`txcript continue ./run.json --with claude_code` で続けられます。プロジェクトのファイルは別途移してください。

`txcript mcp` を実行すると、MCP クライアントから過去のセッションを一覧表示・検索・閲覧できます。ツールは読み取り専用です。フィルター、メッセージ範囲、シェル連携については [CLI リファレンス](../usage.md#cli)を参照してください。

<a name="use-the-library"></a>

## ライブラリを使う

### Rust

```sh
cargo add txcript
```

Claude Code の会話記録を Codex のネイティブ形式に変換します。

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

`Store` の各実装を使うと、エージェント固有の保存先からセッションを検出・読み込み・保存できます。`Transcript<Common>` に変換すれば、エージェントを問わず同じ API で会話を検索・切り出し・表示できます。[Rust API](https://docs.rs/txcript) と[使用例](../usage.md#rust-crate)を参照してください。

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

パッケージには Node と Bun 向けのビルド済み WebAssembly が含まれます。セッションのテキストをメモリ上で変換・検索し、ファイルの読み書きや保存先の管理はアプリケーションが行います。[JavaScript リファレンス](../usage.md#npm-package)を参照してください。

<a name="supported-agents"></a>

## 対応エージェント

各エージェント名から形式のドキュメントを開けます。ID は `--from` と `--with` に指定します。

| エージェント | ID | 読み込み元 | 続行先 |
|---|---|:---:|:---:|
| [Claude Code](../formats/claude-code.md) | `claude_code` | 対応 | 対応 |
| [Codex](../formats/codex.md) | `codex` | 対応 | 対応 |
| [OpenCode](../formats/opencode.md) | `opencode` | 対応 | 対応 |
| [Cursor CLI](../formats/cursor.md) | `cursor` | 対応 | 対応 |
| [Cursor desktop](../formats/cursor-desktop.md) | `cursor_desktop` | 対応 | 対応 |
| [pi](../formats/pi.md) | `pi` | 対応 | 対応 |
| [Campfire](../formats/campfire.md) | `campfire` | 対応 | 対応 |
| [Cowork](../formats/cowork.md) | `cowork` | 対応 | 対応 |
| [Grok CLI](../formats/grok.md) | `grok` | 対応 | 対応 |
| [Grok Bot](../formats/grok-bot.md) | `grok_bot` | 対応 | ローカルゲートウェイ経由 |
| [fx](../formats/fx.md) | `fx` | 対応 | 対応 |
| [Antigravity](../formats/antigravity.md) | `antigravity` | 対応 | 対応 |
| [uji](../formats/uji.md) | `uji` | 対応 | 対応 |
| [Hermes Agent](../formats/hermes.md) | `hermes` | 対応 | 非対応 |
| [Amp](../formats/amp.md) | `amp` | 対応 | 非対応 |
| [Claude Chat](../formats/claude-chat.md) | `claude_chat` | オンラインアカウント | 非対応 |
| [ChatGPT](../formats/chatgpt.md) | `chatgpt` | オンラインアカウント | 非対応 |

ローカルのセッション検出ではオンラインアカウントにアクセスしません。読み込む場合は `--from claude_chat` または `--from chatgpt` を明示的に指定してください。これらは非公開の Web API と既存アプリのログイン情報を利用します。要件と制限はリンク先のドキュメントに記載しています。

### 別のエージェントを接続する

専用アダプターがないエージェントでも、txcript の JSON 交換形式 [Simple](../formats/simple.md) を出力できます。次のようなドキュメントを `run.json` として保存してください。

```json
{
  "messages": [
    { "role": "user", "content": "テストが失敗する原因を調べて。" },
    { "role": "assistant", "content": "テストの時計がローカル時刻を使っています。" }
  ]
}
```

```sh
txcript continue ./run.json --with claude_code
```

Simple は推論、ツール呼び出し、結果、画像、メタデータも表現できます。`txcript export` の出力もこの形式です。

## 引き継がれる内容

共通モデルはメッセージ、推論、ツール呼び出しとその結果、画像、メタデータ、トークン使用量を表現します。変換後に残る内容は、移行元が記録する情報と移行先が表現できる情報によって決まります。エージェント固有のレコードや未対応のフィールドは失われる場合があります。

変換で引き継ぐのは会話履歴です。システム指示とツールは移行先が提供し、プロジェクトのファイルは別途用意する必要があります。ネイティブ形式の読み込み・保存と形式間の変換では、保持が保証される内容が異なります。[各形式の注意点](../formats/README.md)を参照してください。

<a name="documentation"></a>

## ドキュメント

- [CLI リファレンス](../usage.md#cli)：コマンド、検索、切り出し、MCP、シェル連携。
- [Rust API](https://docs.rs/txcript) と [JavaScript リファレンス](../usage.md#npm-package)。
- [会話記録の形式](../formats/README.md)：保存構造、変換時の対応関係、制限事項、出典、リバースエンジニアリングのメモ。
- [開発](../usage.md#development)と[テストガイド](../../tests/README.md)。
- [貢献ガイド](../../CONTRIBUTING.md) · [脆弱性の報告](../../SECURITY.md).
- [変更履歴](../../CHANGELOG.md) · [問題を報告](https://github.com/skillsynchq/txcript/issues)。

## ライセンス

[Apache-2.0](../../LICENSE)
