<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">Продолжайте разговор в другом агенте для программирования.</p>

<p align="center">
  <a href="../../README.md">English</a> | <a href="README.ja.md">日本語</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.zh-TW.md">繁體中文</a> | <a href="README.ko.md">한국어</a> | <a href="README.de.md">Deutsch</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (Brasil)</a> | Русский | <a href="README.mr.md">मराठी</a> | <a href="README.ta.md">தமிழ்</a>
</p>

<p align="center">
  <a href="https://crates.io/crates/txcript"><img src="https://img.shields.io/crates/v/txcript?logo=rust&color=4c71f2" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/txcript"><img src="https://img.shields.io/npm/v/txcript?logo=npm&color=4c71f2" alt="npm"></a>
  <a href="https://docs.rs/txcript"><img src="https://img.shields.io/docsrs/txcript?logo=docsdotrs" alt="docs.rs"></a>
  <a href="https://github.com/skillsynchq/txcript/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/skillsynchq/txcript/ci.yml?branch=main&logo=github&label=ci" alt="CI"></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-555" alt="Лицензия"></a>
</p>

Библиотека txcript преобразует сессии агентов. Начните разговор в Claude Code и продолжите его в Codex, перенеся сообщения, рассуждения и историю инструментов в пределах возможностей целевого агента.

Создавайте средства поиска, просмотра и редактирования сессий на основе единой модели транскрипта. txcript обрабатывает форматы отдельных агентов и предоставляет Rust API, пакет JavaScript и CLI.

[Попробовать CLI](#try-the-cli) · [Использовать библиотеку](#use-the-library) · [Поддерживаемые агенты](#supported-agents) · [Документация](#documentation)

<p align="center">
  <img src="../assets/demo.gif" alt="Сессия OpenCode, продолженная в Claude Code с помощью txcript" width="680">
</p>

<a name="try-the-cli"></a>

## Попробовать CLI

Скачайте готовый исполняемый файл для macOS, Linux или Windows со [страницы релизов](https://github.com/skillsynchq/txcript/releases) или установите из исходного кода с Rust 1.96 или новее:

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli --locked
```

Найдите сессию Claude Code и продолжите её в Codex:

```sh
txcript list --from claude_code
txcript continue <session-id> --with codex
```

Используйте ID из списка; подойдёт и однозначный префикс. txcript записывает новую сессию в родном формате и запускает Codex в сохранённом рабочем каталоге. Исходная сессия сохраняется. Заранее установите целевого агента и войдите в аккаунт.

Другие способы работы с сессиями:

```sh
txcript query "relay bug"                # поиск по локальной истории сессий
txcript view <session-id>                # чтение разговора в терминале
txcript crop <session-id>                # редактирование или обрезка истории в новой копии
txcript export <session-id> --out run.json
```

Перенесите `run.json` на другой компьютер и продолжите с помощью `txcript continue ./run.json --with claude_code`. Файлы проекта перенесите отдельно.

Запустите `txcript mcp`, чтобы клиент MCP мог получать список прошлых сессий, искать и читать их. Инструменты работают только на чтение. Фильтры, диапазоны сообщений и интеграция с оболочкой описаны в [справочнике CLI](../usage.md#cli).

<a name="use-the-library"></a>

## Использовать библиотеку

### Rust

```sh
cargo add txcript
```

Преобразуйте транскрипт Claude Code в родной формат Codex:

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

Используйте реализации `Store` для обнаружения, загрузки и сохранения сессий в хранилище соответствующего агента. Преобразуйте их в `Transcript<Common>`, чтобы искать, обрезать или отображать разговоры разных агентов через единый API. См. [Rust API](https://docs.rs/txcript) и [примеры](../usage.md#rust-crate).

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

Пакет содержит готовый WebAssembly для Node и Bun. Он преобразует текст сессий и выполняет поиск в памяти; файлы и хранение данных остаются задачей приложения. См. [справочник JavaScript](../usage.md#npm-package).

<a name="supported-agents"></a>

## Поддерживаемые агенты

Каждое название ведёт к документации соответствующего формата. Используйте ID с `--from` и `--with`.

| Агент | ID | Чтение из | Продолжение в |
|---|---|:---:|:---:|
| [Claude Code](../formats/claude-code.md) | `claude_code` | Да | Да |
| [Codex](../formats/codex.md) | `codex` | Да | Да |
| [OpenCode](../formats/opencode.md) | `opencode` | Да | Да |
| [Cursor CLI](../formats/cursor.md) | `cursor` | Да | Да |
| [Cursor desktop](../formats/cursor-desktop.md) | `cursor_desktop` | Да | Да |
| [pi](../formats/pi.md) | `pi` | Да | Да |
| [Campfire](../formats/campfire.md) | `campfire` | Да | Да |
| [Cowork](../formats/cowork.md) | `cowork` | Да | Да |
| [Grok CLI](../formats/grok.md) | `grok` | Да | Да |
| [Grok Bot](../formats/grok-bot.md) | `grok_bot` | Да | Через локальный шлюз |
| [fx](../formats/fx.md) | `fx` | Да | Да |
| [Antigravity](../formats/antigravity.md) | `antigravity` | Да | Да |
| [uji](../formats/uji.md) | `uji` | Да | Да |
| [Hermes Agent](../formats/hermes.md) | `hermes` | Да | Нет |
| [Amp](../formats/amp.md) | `amp` | Да | Нет |
| [Claude Chat](../formats/claude-chat.md) | `claude_chat` | Онлайн-аккаунт | Нет |
| [ChatGPT](../formats/chatgpt.md) | `chatgpt` | Онлайн-аккаунт | Нет |

Локальное обнаружение не обращается к онлайн-аккаунтам. Для чтения явно укажите `--from claude_chat` или `--from chatgpt`. Эти источники используют закрытые веб-API и существующий вход в приложение; требования и ограничения описаны по ссылкам в таблице.

### Подключить другого агента

Агент без собственного адаптера может выдавать [Simple](../formats/simple.md), формат обмена txcript на основе JSON. Сохраните такой документ в `run.json`:

```json
{
  "messages": [
    { "role": "user", "content": "Выясни, почему тесты падают." },
    { "role": "assistant", "content": "Часы в тестах используют местное время." }
  ]
}
```

```sh
txcript continue ./run.json --with claude_code
```

Simple также представляет рассуждения, вызовы инструментов, результаты, изображения и метаданные. Именно этот формат выдаёт `txcript export`.

## Что сохраняется

Общая модель представляет сообщения, рассуждения, вызовы инструментов и их результаты, изображения, метаданные и расход токенов. Сохранность данных зависит от того, что записывает источник и что может представить целевой агент. Записи, специфичные для агента, и неподдерживаемые поля могут потеряться.

Преобразование переносит историю разговора. Целевой агент предоставляет собственные системные инструкции и инструменты, а файлы проекта должны быть доступны отдельно. Загрузка и сохранение в родном формате дают иные гарантии сохранности, чем преобразование; см. [ограничения отдельных форматов](../formats/README.md).

<a name="documentation"></a>

## Документация

- [Справочник CLI](../usage.md#cli): команды, поиск, обрезка, MCP и интеграция с оболочкой.
- [Rust API](https://docs.rs/txcript) и [справочник JavaScript](../usage.md#npm-package).
- [Форматы транскриптов](../formats/README.md): устройство хранилищ, сопоставления и ограничения со ссылками на источники и заметками об обратной разработке.
- [Разработка](../usage.md#development) и [руководство по тестированию](../../tests/README.md).
- [Участие в разработке](../../CONTRIBUTING.md) · [Сообщить об уязвимости](../../SECURITY.md).
- [История изменений](../../CHANGELOG.md) · [Сообщить о проблеме](https://github.com/skillsynchq/txcript/issues).

## Лицензия

[Apache-2.0](../../LICENSE)
