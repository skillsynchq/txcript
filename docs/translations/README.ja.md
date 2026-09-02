<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">ハーネス間でセッションを移動するためのライブラリ</p>

<p align="center">
  <a href="../../README.md">English</a> | 日本語 | <a href="README.zh-CN.md">简体中文</a> | <a href="README.zh-TW.md">繁體中文</a> | <a href="README.ko.md">한국어</a> | <a href="README.de.md">Deutsch</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (Brasil)</a> | <a href="README.ru.md">Русский</a> | <a href="README.mr.md">मराठी</a> | <a href="README.ta.md">தமிழ்</a>
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

Claude Code でセッションを始め、使用量制限や行き詰まりに突き当たったら、会話・推論・ツール履歴をすべて保ったまま Codex で続きから再開できます:

<p align="center">
  <img src="../assets/demo.gif" alt="txcript continue: an OpenCode session resumed in Claude Code">
</p>

txcript は各ハーネスのネイティブなトランスクリプト形式を、型付きの共通モデルを介してマッピングします。ネイティブ形式のロード/セーブはバイト単位で無損失であり、ハーネス間の変換ではメッセージ、推論、ツール呼び出し、ツール結果、画像、メタデータ、使用量情報を（利用可能な範囲で）保持します。[**CLI**](#cli)、[**Rust crate**](#rust-crate)、[**npm package**](#npm-package) として提供されます。

## ハイライト

- **16 のハーネス、1 つのモデル** — すべての形式は `Transcript<Common>` を介して変換されるため、ハーネスを 1 つ追加すれば他のすべてとつながります。
- **それ以外のすべてのエージェントのための形式** — txcript が知らないエージェントでも、文書化された [Simple](../formats/simple.md) 交換用 JSON をファイルまたはストリームとして出力し、txcript に直接渡せば、そのトランスクリプトを対応ハーネスのいずれでも続行できます。
- **バイト無損失のラウンドトリップ** — セッションを自身の形式でロードして保存すると、元と完全に一致するものが再現されます。
- **どこでも続行** — `txcript continue <id> --with <harness>` はセッションを別のハーネスのネイティブ形式に書き直して起動します。元のセッションが変更されることはありません。
- **セッションを読む・持ち運ぶ** — `txcript view` は任意のセッションを内蔵ページャで開き、画像を描画できるターミナルでは画像も表示します。`txcript export` はセッションを Simple ドキュメントとして書き出し、別のマシンで `continue` がそれを取り込みます。
- **すべてを検索** — マシン上のすべてのセッションを対象にした、リテラルかつ大文字小文字を区別しない検索。ライブラリ API、ワンショットの CLI クエリ、対話型ピッカーのいずれでも利用できます。
- **MCP サーバー** — `txcript mcp` は読み取り専用の `list_sessions`、`search_sessions`、`read_session` ツールを公開し、エージェントが過去のセッションをコンテキストとして掘り起こせるようにします。
- **文書化されたフォーマット** — 各ハーネスのオンディスク形式は [`docs/formats/`](../formats) にまとめられており、各記述には出典（公式ドキュメント、ソースへのパーマリンク、またはリバースエンジニアリングのメモ）が付記されています。

## 対応ハーネス

```mermaid
flowchart LR
    claude["Claude Code"] <--> common(("Transcript&lt;Common&gt;"))
    claudechat["Claude Chat"] --> common
    chatgpt["ChatGPT"] --> common
    cowork["Cowork"] <--> common
    codex["Codex"] <--> common
    opencode["OpenCode"] <--> common
    pi["pi"] <--> common
    campfire["Campfire"] <--> common
    common <--> cursor["Cursor CLI"]
    common <--> cursordesktop["Cursor desktop"]
    common <--> grok["Grok CLI"]
    common <--> fx["fx"]
    common <--> antigravity["Antigravity"]
    simple["Simple (any agent)"] --> common
    hermes["Hermes Agent"] --> common
    amp["Amp"] --> common
```

ディスカバリ、一覧表示、検索、`view` は、バックエンドのストアを持つすべてのハーネスで動作します。`id` の文字列が、CLI と WASM API に渡す値です。

| ハーネス | id | ディスク上のセッション | ネイティブ形式 | 変換 | 続行先 | ドキュメント |
|---|---|---|---|:---:|:---:|---|
| [Claude Code](https://claude.com/claude-code) | `claude_code` | `~/.claude/projects/` | JSONL | ⇄ | ✓ | [仕様](../formats/claude-code.md) |
| [Claude Chat](https://claude.ai) | `claude_chat` | ライブの `claude.ai` アカウント <sup>4</sup> | 非公開 Web API | → | — <sup>4</sup> | [仕様](../formats/claude-chat.md) |
| [ChatGPT](https://chatgpt.com) | `chatgpt` | ライブの `chatgpt.com` アカウント <sup>5</sup> | 非公開 Web API | → | — <sup>5</sup> | [仕様](../formats/chatgpt.md) |
| [Cowork](https://claude.com/product/cowork) | `cowork` | `<Claude app data>/local-agent-mode-sessions/` | セッションレコード + Claude Code JSONL | ⇄ | ✓ | [仕様](../formats/cowork.md) |
| [Codex](https://github.com/openai/codex) | `codex` | `~/.codex/sessions/` | ロールアウト JSONL | ⇄ | ✓ | [仕様](../formats/codex.md) |
| [OpenCode](https://opencode.ai) | `opencode` | `~/.local/share/opencode/opencode.db` | SQLite | ⇄ | ✓ | [仕様](../formats/opencode.md) |
| [pi](https://pi.dev) | `pi` | `~/.pi/agent/sessions/` | JSONL | ⇄ | ✓ | [仕様](../formats/pi.md) |
| [Campfire](../formats/campfire.md) | `campfire` | `~/.campfire/agent/sessions/` | JSONL | ⇄ | ✓ | [仕様](../formats/campfire.md) |
| [Cursor CLI](https://cursor.com/cli) | `cursor` | `~/.cursor/chats/` | SQLite | ⇄ | ✓ | [仕様](../formats/cursor.md) |
| [Cursor desktop](https://cursor.com) | `cursor_desktop` | `<Cursor User dir>/globalStorage/` | SQLite | ⇄ | ✓ | [仕様](../formats/cursor-desktop.md) |
| [Grok CLI](https://github.com/xai-org/grok-build) | `grok` | `~/.grok/sessions/` | セッションディレクトリ（JSON） | ⇄ | ✓ | [仕様](../formats/grok.md) |
| [fx](https://fx.sh) | `fx` | `~/.fx/sessions/` | セッションディレクトリ（イベントログ） | ⇄ | ✓ | [仕様](../formats/fx.md) |
| Hermes Agent | `hermes` | `~/.hermes/state.db` | SQLite | → | — <sup>3</sup> | [仕様](../formats/hermes.md) |
| [Amp](https://ampcode.com) | `amp` | `~/.local/share/amp/threads/` | スレッド JSON | → | — <sup>1</sup> | [仕様](../formats/amp.md) |
| [Antigravity](https://antigravity.google) | `antigravity` | `~/.gemini/antigravity-cli/` | SQLite | ⇄ | ✓ | [仕様](../formats/antigravity.md) |
| Simple | `simple` | — <sup>2</sup> | 交換用 JSON | → | — <sup>2</sup> | [仕様](../formats/simple.md) |

<sup>1</sup> Amp のスレッドはサーバー側にあり、CLI にはインポート機能がありません: セッションは Amp *から*変換できますが、Amp へ続行することはできません。

<sup>2</sup> Simple は txcript 自身の交換形式であり、上記に載っていないあらゆるエージェントのための入り口です。アプリも管理されたディレクトリもありません: Simple セッションはドキュメント（ファイル、または stdin）であり、`txcript continue` に直接渡します。続行された会話は、それ以降ターゲットハーネスの中に置かれます。

<sup>3</sup> Hermes の `state.db` は txcript では読み取り専用であり、Hermes にはセッションのインポートコマンドがありません: セッションは Hermes *から*変換できますが、Hermes へ続行することはできません。

<sup>4</sup> Claude Chat はライブのプル専用ソースです。macOS では、`--from claude_chat` を明示的に選択すると、サインイン済みの Claude Desktop セッションが自動的に再利用されます。集約ディスカバリは Claude Chat に接続しません。環境変数で渡された資格情報は受け付けられません。任意の `TXCRIPT_CLAUDE_CHAT_ORGANIZATION_UUID` を設定するとディスカバリが 1 つの組織に絞り込まれ、設定しなければアプリのアクティブな組織が使われます。Claude Chat にはサポートされた会話 API がありません: txcript は Anthropic が監視または制限しうるプライベートエンドポイントを読み取っており、Rust クレートはディスカバリが直接呼び出される箇所でビルド時に警告します。txcript は読み取りのみを行い、保存、削除、同一ハーネスでの続行、`--with claude_chat` を拒否します。会話の中で Claude が生成したファイルも一緒に運ばれます。Claude Code へ続行すると、それらは新しいセッションの隣に書き出され、Claude Code のアーティファクトとして表示されます。Claude のデータエクスポート ZIP と `conversations.json` はサポートされません。

<sup>5</sup> ChatGPT はライブのプル専用ソースです。Claude Chat が Claude Desktop を再利用するのと同様に、`--from chatgpt` を明示的に選択すると、Codex が `CODEX_HOME/auth.json` または `~/.codex/auth.json` で管理する ChatGPT ログインが自動的に再利用されます。このアカウントは、ブラウザでサインインしているものと異なる場合があります。txcript はその資格情報ファイルを読むだけで、リフレッシュや書き換えは決して行いません。集約ディスカバリは ChatGPT に接続しませんが、正確な会話 UUID であればアカウントを列挙せずに直接読み取れます。txcript は読み取りのみを行い、保存、削除、同一ハーネスでの続行、`--with chatgpt` を拒否します。ChatGPT にはサポートされた会話 API がないため、このアクセス方法は変更または制限される可能性があります。ChatGPT のデータエクスポートアーカイブはサポートされません。

## インストール

**CLI**（`txcript` バイナリをインストール）:

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli
# or from a checkout: cargo install --path cli
```

**Rust crate**:

```sh
cargo add txcript
```

**npm package**（ビルド済み WASM、Rust ツールチェーン不要）:

```sh
bun add txcript     # or: npm install txcript
```

## CLI

ローカルのセッションを見つけて、任意のハーネスで続行します:

```sh
txcript list                             # local sessions across every harness
    [--from <harness>]                    #   only this harness's sessions
    [--cwd <dir>]                         #   only sessions recorded under <dir>
    [-n <N>]                              #   at most N sessions
    [--since <when>] [--until <when>]     #   bound the session start time
txcript continue <id>[#range]            # continue <id>, then launch its harness
    [--with <harness>]                    #   ...continuing in <harness> instead
    [--from <harness>]                    #   scope the id lookup to one harness
    [--out <dir>]                         #   write under <dir>; implies --no-resume
    [--no-resume]                         #   write the session but don't launch
txcript continue <file|->[#range]        # continue a Simple document instead:
    --with <harness> [...]                #   a file, or stdin (`-`), from any agent
txcript view <id>[#range]                # view a session; compact text when piped
    [--from <harness>]                    #   scope the id lookup to one harness
    [--no-pager]                          #   print the terminal view directly
txcript export <id>[#range]              # write a session as a Simple document
    [--from <harness>]                    #   scope the id lookup to one harness
    [--out <file>]                        #   write to <file> instead of stdout
```

セッション id は、完全な id の一意に定まる任意のプレフィックス、またはセッションの正確なタイトルです。`txcript resume` は `continue` のエイリアスです。`--since` と `--until` は RFC 3339 のタイムスタンプ、または `YYYY-MM-DD` だけの日付を受け付けます。

`continue` はセッションをターゲットハーネスがセッションを保存している場所に書き出し、続けてそのハーネスを起動してターミナルを引き渡します:

- 同一ハーネス: 元のセッションをその場で再開します。
- ハーネスをまたぐ場合（`--with`）: セッションをターゲットのネイティブ形式に書き直します。書き出されるのは常にコピーであり、ソースのセッションが変更・削除されることはありません。
- id の代わりに [Simple](../formats/simple.md) ドキュメントを渡す — `txcript continue ./run.json --with claude_code`、または `my-agent | txcript continue - --with claude_code` — と、任意のエージェントのトランスクリプトを同じ方法で取り込めます。ドキュメントにはそれ自身のハーネスがないため、`--with` は必須です。
- 起動コマンドはハーネスごとに上書きできます。`TRANSCRIPT_<HARNESS>_RESUME_CMD` を `{id}` テンプレートとして設定します。例: `TRANSCRIPT_CODEX_RESUME_CMD="codex resume {id}"`。

`view` をターミナルで実行すると内蔵ページャが開きます: `u`、`a`、`t`、`r` でユーザーメッセージ、アシスタントメッセージ、ツール呼び出し、推論の表示/非表示を切り替え、`]` と `[` でメッセージ間を移動し、`/` で表示中の内容を検索します。画像を表示できるターミナル（Ghostty、kitty、WezTerm、Konsole）では画像がインラインで描画されます。外部ページャを使うには `TXCRIPT_PAGER` を設定し、ビューを直接出力するには `--no-pager` を渡します。パイプまたはリダイレクトされた場合、`view` は MCP サーバーが提供するのと同じコンパクトなテキストを出力します。いずれの場合も各メッセージには `── #N ──` の区切り線で番号が振られ、`#range` は、その出力に表示される序数（1 始まり・両端含み）でメッセージを選択します:

- `abc#7`: メッセージ 7 のみ
- `abc#5-12`: メッセージ 5 から 12 まで
- `abc#5-`: メッセージ 5 から末尾まで
- `abc#-10`: 先頭からメッセージ 10 まで

`continue` も同じサフィックスを受け付け、その範囲のメッセージだけを新しいセッションとして続行します。ツール呼び出しをその結果から切り離してしまう範囲は拒否され、エラーには最も近い有効な範囲が提案されます。

`export` はセッションを [Simple](../formats/simple.md) ドキュメントとして、stdout または `--out <file>` に書き出します。このドキュメントは正準モデルの完全なレンダリング — `continue` がハーネス間で運ぶものすべて — であり、どのハーネスの保存場所からも切り離されているため、1 つのファイルとしてマシン間を移動できます:

```sh
txcript export 0dc114bf --out session.json       # on this machine
txcript continue ./session.json --with claude_code   # on the other one
```

記録された作業ディレクトリは、インポート先のマシンに存在すればそれを保持し、存在しなければ `continue` の実行ディレクトリに置き換えられます。`export` は `view` と同じ `#range` サフィックスと `--from` スコープを受け付けます。

### 検索

```sh
txcript query 'relay bug'                # one-shot: ranked hits, highlighted
txcript query                            # interactive picker; Enter continues
    [--from <harness>]                   #   search only <harness> (default: all)
    [--with <harness>]                   #   continue the pick in <harness>
    [--cwd <dir>]                        #   only sessions recorded under <dir>
```

パターンはリテラルかつ大文字小文字を区別せずにマッチします: `relay bug` は、スペースも含めてその正確なテキストを含む行を見つけます。

ピッカーでは、文字を入力してフィルタし、矢印キーまたは ctrl-p/n で移動、Enter で選択したセッションを元のハーネス（または `--with` で指定したハーネス）で続行、Esc でキャンセルします。各行には、どの種類のコンテンツがマッチしたか — ユーザーテキスト、アシスタントテキスト、思考、ツール使用、ツール出力、セッションメタデータ — が表示されます。

キャッシュがなければ、実行のたびにすべてのセッションが読み直されます。`--cache <path>` を渡す（または `TXCRIPT_CACHE` を設定する）と、そのパスに永続的な検索キャッシュが保持され、`query` と MCP の検索ツールは前回の実行以降に変更されたセッションだけを読み直します。このフラグはすべてのサブコマンドで受け付けられます。

### MCP サーバー

```sh
txcript mcp                              # stdio transport
```

読み取り専用のツールを 3 つ公開します。オプションのフィルタは CLI と同じです:

- `list_sessions(from?, cwd?, limit?, offset?)`
- `search_sessions(pattern, from?, cwd?)`
- `read_session(id, from?)`

<sub>\* `from` を省略するとすべてのハーネスが対象になります。`cwd` を省略するとディレクトリによるフィルタは行われません。作業ディレクトリが記録されていないセッションは、`cwd` を省略した場合にのみマッチします。</sub>

`list_sessions` は `limit` と `offset` でページングし、ページング前の総数を報告します。ライブの Claude Chat と ChatGPT ソースが一覧に載ることはありません。`read_session` は `view` と同じ `#range` サフィックスを受け付け、同じコンパクトなテキストを返します。一度に返すには大きすぎる読み取りは拒否され、サブ範囲が提案されます。`--cache` はサーバーにも適用されます。

### シェル統合

```sh
eval "$(txcript init zsh)"                      # in ~/.zshrc; or: txcript init bash
```

`init` は補完に加えて、現在のフォルダで記録されたセッションに絞ったピッカーを開く ctrl+shift+r のキーバインドを出力します。補完だけが必要な場合は、`completion` が bash、elvish、fish、powershell、zsh に対応しています:

```sh
txcript completion zsh > ~/.zfunc/_txcript      # or wherever your fpath looks
source <(txcript completion bash)               # bash, ad hoc
txcript completion fish > ~/.config/fish/completions/txcript.fish
```

## Rust crate

```toml
[dependencies]
txcript = "0.12"
# Codecs only: drops the SQLite-backed stores, the live Claude Chat and
# ChatGPT sources, and search. Every codec stays available.
# txcript = { version = "0.12", default-features = false }
```

デフォルトフィーチャー: `opencode`（SQLite ストア: OpenCode、両方の Cursor、Antigravity）、`hermes`、`claude_chat`、`chatgpt`、`search`。

小さい順に 3 つのレイヤーがあります:

- `Codec` — ハーネスごとの `to_common` / `from_common`。`convert::<A, B>` はそれらを正準モデル経由で連結します。
- `TextCodec` — `from_text` / `to_text` でハーネスのネイティブなセッションテキストをパース/レンダリングします。I/O は発生しません。
- `Store` — 実際のバックエンド（セッションディレクトリ、または OpenCode、Hermes、両方の Cursor、Antigravity 用の SQLite DB）に対して発見/ロード/保存を行います。

メモリ内で変換する場合（ファイルシステム不要）:

```rust
use txcript::harness::{claude_code, codex};
use txcript::{Codec, TextCodec, convert};

let claude = claude_code::ClaudeCode::from_text(jsonl_text)?;          // Transcript<ClaudeCode>
let codex = convert::<claude_code::ClaudeCode, codex::Codex>(&claude)?; // Transcript<Codex>
let codex_text = codex::Codex::to_text(&codex)?;                       // native rollout JSONL
```

または `Store` でディスクを経由する場合:

```rust
use txcript::harness::{claude_code, codex};
use txcript::{Store, convert};

let store = claude_code::ClaudeStore::default_root().expect("home dir");
let found = store.discover()?;                       // cheap metadata scan
let claude = store.load(&found[0].reference)?;       // Transcript<ClaudeCode>

let codex = convert::<_, codex::Codex>(&claude)?;
codex::CodexStore::default_root().expect("home dir").save(&codex)?;  // resumable on disk
```

正準モデルは `Transcript<Common>` — `Meta` + `Vec<Message>` で、`Message` は型付きの `Block`（`Text`、`Thinking`、`ToolUse`、`ToolResult`、`Image`）と型付きの `Tool` enum を保持します。

ユーザーがハーネス上で実行したスラッシュコマンド（`/release patch`）も正準的に表現されます: ユーザーターン上の `Tool::Command` 呼び出しとして扱われ、コマンドが出力として返したものが対になる `ToolResult` になります。

### 検索（`search` フィーチャー、デフォルトで有効）

`txcript::search` は、トランスクリプトに対するファジー検索（fzf 流の構文）と部分文字列検索をサポートします。ワンショット検索:

```rust
use txcript::search::{Query, search};

let hits = search(&common, &Query::substring("relay bug"));  // or Query::fuzzy for fzf syntax
for hit in hits {
    // hit.origin: User | Assistant | Thinking | ToolUse | ToolResult | Meta
    // hit.span addresses the message; hit.highlights are char ranges into hit.line
    let messages = common.fragment(&hit.span);            // zero-copy: Option<&[Message]>
}
```

ピッカー型の検索では、`Index` を一度構築してキーストロークごとにクエリします:

```rust
use txcript::search::{DocKey, Index, Query};

let mut index = Index::new();
index.insert(DocKey { harness, id }, &common);   // re-insert replaces; caller owns refresh
let matches = index.query(&Query::fuzzy("srch")); // ranked docs, best lines as hits
```

空のパターンはドキュメントを新しい順に返します。ツール出力はデフォルトで除外されます。含めるには `Origin::ALL` を使います。`Query.harnesses`、`Query.limit`、`Query.hits_per_doc` で結果を絞り込めます。

### テキスト射影

`txcript::text::to_text(&common)` は [`txcript view`](#cli) の裏にある射影です — `Transcript<Common>` を LLM のコンテキストとして使うための、一方向でトークン量を意識したレンダリングです。メッセージ、推論テキスト、コンパクトなツール呼び出し/結果を保持しつつ、暗号化された推論、使用量の計上、インライン画像バイトといった再生専用のペイロードは省きます。`to_text_fragment(&common, &span)` は本文の `Span` をレンダリングし、セッション全体における各メッセージの序数を保持します。

## npm package

npm パッケージは、Bun と Node 向けにビルド済みの WASM としてコーデックを提供します。セッションテキストをメモリ内で変換します。ディスク上のセッションの発見・読み取り・書き込みは呼び出し側の仕事であり、このパッケージに `Store` はありません。

```ts
import { convert, toCommon, fromCommon, harnesses } from "txcript";
import { readFileSync, writeFileSync } from "node:fs";

const input = readFileSync("rollout.jsonl", "utf8");

// native -> native (e.g. a Codex rollout into Claude Code's JSONL)
writeFileSync("session.jsonl", convert(input, "codex", "claude_code"));

// canonical view, and back
const common = JSON.parse(toCommon(input, "codex"));   // { meta, messages }
const pi = fromCommon(JSON.stringify(common), "pi");

harnesses(); // ["claude_code","claude_chat","chatgpt","codex","opencode","pi","campfire","cursor","cursor_desktop","grok","fx","hermes","amp","antigravity","simple","cowork"]
```

テキスト入力・テキスト出力: `input` はソースハーネスのネイティブなセッションテキストで、結果はターゲットのものになります。不正なハーネス名やパースできない入力は JS の `Error` を投げます。

検索も同梱されています。クエリはクレートの `Query` の JSON 形式です: 必須なのは `pattern` だけで、`mode` は `"substring"` に設定しない限り `"fuzzy"` になります:

```ts
import { searchTranscript, Searcher } from "txcript";

// one session, one shot: a JSON array of hits
const hits = JSON.parse(searchTranscript(input, "codex", JSON.stringify({ pattern: "relay bug", mode: "substring" })));

// picker-style: index once, query per keystroke
const index = new Searcher();
index.insert("codex", "0dc114bf", input);          // re-insert replaces
const matches = JSON.parse(index.query(JSON.stringify({ pattern: "relay bug" })));
```

| ハーネス | セッションテキスト |
|---|---|
| `claude_code`、`codex`、`pi`、`campfire` | セッション JSONL |
| `claude_chat` | ライブの会話詳細レスポンス 1 件（ソース専用。アカウントエクスポートの配列は不可） |
| `chatgpt` | ライブの会話詳細レスポンス 1 件（ソース専用。アカウントエクスポートの配列は不可） |
| `opencode` | `opencode export` の JSON |
| `cursor` | セッションの `store.db` の JSON エクスポート |
| `cursor_desktop` | セッションの `state.vscdb` 行の JSON ダンプ |
| `grok` | セッションディレクトリ内ファイルの JSON バンドル |
| `fx` | セッションディレクトリ内ファイルの JSON バンドル |
| `hermes` | `hermes sessions export` の JSON オブジェクト |
| `amp` | `amp threads export` の JSON |
| `antigravity` | 会話データベースの JSON ダンプ（protobuf blob は 16 進エンコード） |
| `simple` | [Simple](../formats/simple.md) 交換用 JSON ドキュメント |
| `cowork` | セッションレコード、Claude Code トランスクリプト、監査ログの JSON バンドル |

代わりにソースから wasm をビルドする場合:

```sh
git clone https://github.com/skillsynchq/txcript.git
cd txcript
bun run setup        # once: wasm target + wasm-bindgen-cli
bun run build        # produces ./pkg
```

## フォーマットドキュメント

これらのトランスクリプト形式のすべてがベンダーによって文書化されているわけではありません。[`docs/formats/`](../formats) にはハーネスごとに 1 つのドキュメントがあります — セッションがディスク上のどこにあるか、ディスカバリがそれをどう見つけるか、フォーマットの各部分の解剖、そしてその癖 — そして各記述には出典がタグ付けされています: 公式ドキュメント、ハーネス自身のオープンソースのシリアライズコード（コミット固定のパーマリンク付きで引用）、またはリバースエンジニアリングです。

## 開発

```sh
cargo test --workspace --all-features               # what CI runs
cargo test -p txcript --no-default-features         # codecs only: no SQLite or live stores
bun run build && bun examples/convert.ts <file> <from> <to>
git config core.hooksPath .githooks                 # pre-push runs the CI checks
```

バイナリは独立したワークスペースクレート（`cli/`、パッケージ `txcript-cli`）に置かれています。ルートのライブラリはその依存関係を一切持ちません。

## ライセンス

[Apache-2.0](../../LICENSE)
