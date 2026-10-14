<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">Setze dein Gespräch in einem anderen Coding-Agenten fort.</p>

<p align="center">
  <a href="../../README.md">English</a> | <a href="README.ja.md">日本語</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.zh-TW.md">繁體中文</a> | <a href="README.ko.md">한국어</a> | Deutsch | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (Brasil)</a> | <a href="README.ru.md">Русский</a> | <a href="README.mr.md">मराठी</a> | <a href="README.ta.md">தமிழ்</a>
</p>

<p align="center">
  <a href="https://crates.io/crates/txcript"><img src="https://img.shields.io/crates/v/txcript?logo=rust&color=4c71f2" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/txcript"><img src="https://img.shields.io/npm/v/txcript?logo=npm&color=4c71f2" alt="npm"></a>
  <a href="https://docs.rs/txcript"><img src="https://img.shields.io/docsrs/txcript?logo=docsdotrs" alt="docs.rs"></a>
  <a href="https://github.com/skillsynchq/txcript/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/skillsynchq/txcript/ci.yml?branch=main&logo=github&label=ci" alt="CI"></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-555" alt="Lizenz"></a>
</p>

txcript ist eine Bibliothek zum Konvertieren von Agentensitzungen. Beginne ein Gespräch in Claude Code und setze es in Codex fort. Nachrichten, Gedankengänge und der Werkzeugverlauf werden übernommen, soweit das Ziel sie unterstützt.

Entwickle Suchfunktionen, Ansichten und Editoren für Sitzungen auf Basis eines gemeinsamen Transkriptmodells. txcript übernimmt die agentenspezifischen Formate und bietet eine Rust-API, ein JavaScript-Paket und eine CLI.

[CLI ausprobieren](#try-the-cli) · [Bibliothek verwenden](#use-the-library) · [Unterstützte Agenten](#supported-agents) · [Dokumentation](#documentation)

<p align="center">
  <img src="../assets/demo.gif" alt="Eine OpenCode-Sitzung wird mit txcript in Claude Code fortgesetzt" width="680">
</p>

<a name="try-the-cli"></a>

## CLI ausprobieren

Lade eine Binärdatei für macOS, Linux oder Windows von der [Releases-Seite](https://github.com/skillsynchq/txcript/releases) herunter, oder installiere aus dem Quellcode mit Rust 1.96 oder neuer:

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli --locked
```

Finde eine Claude-Code-Sitzung und setze sie in Codex fort:

```sh
txcript list --from claude_code
txcript continue <session-id> --with codex
```

Verwende eine ID aus der Liste; ein eindeutiges Präfix genügt ebenfalls. txcript schreibt eine neue Sitzung im nativen Format und startet Codex im gespeicherten Arbeitsverzeichnis. Die ursprüngliche Sitzung bleibt erhalten. Installiere den Zielagenten und melde dich dort an, bevor du fortfährst.

Weitere Möglichkeiten, mit Sitzungen zu arbeiten:

```sh
txcript query "relay bug"                # lokalen Sitzungsverlauf durchsuchen
txcript view <session-id>                # ein Gespräch im Terminal lesen
txcript crop <session-id>                # Verlauf als neue Kopie bearbeiten oder kürzen
txcript export <session-id> --out run.json
```

Übertrage `run.json` auf einen anderen Rechner und fahre mit `txcript continue ./run.json --with claude_code` fort. Übertrage die Projektdateien separat.

Starte `txcript mcp`, damit ein MCP-Client frühere Sitzungen auflisten, durchsuchen und lesen kann. Die Werkzeuge greifen nur lesend zu. Filter, Nachrichtenbereiche und Shell-Integration sind in der [CLI-Referenz](../usage.md#cli) beschrieben.

<a name="use-the-library"></a>

## Bibliothek verwenden

### Rust

```sh
cargo add txcript
```

Konvertiere ein Claude-Code-Transkript in das native Codex-Format:

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

Verwende die `Store`-Implementierungen, um Sitzungen im Speicher des jeweiligen Agenten zu finden, zu laden und zu speichern. Konvertiere sie in `Transcript<Common>`, um Gespräche agentenübergreifend mit derselben API zu durchsuchen, zu kürzen oder darzustellen. Siehe [Rust-API](https://docs.rs/txcript) und [Beispiele](../usage.md#rust-crate).

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

Das Paket enthält vorgefertigtes WebAssembly für Node und Bun. Es konvertiert und durchsucht Sitzungstext im Arbeitsspeicher; deine Anwendung übernimmt Dateien und Speicherung. Siehe [JavaScript-Referenz](../usage.md#npm-package).

<a name="supported-agents"></a>

## Unterstützte Agenten

Jeder Name führt zur Dokumentation des jeweiligen Formats. Verwende die ID mit `--from` und `--with`.

| Agent | ID | Lesen aus | Fortsetzen in |
|---|---|:---:|:---:|
| [Claude Code](../formats/claude-code.md) | `claude_code` | Ja | Ja |
| [Codex](../formats/codex.md) | `codex` | Ja | Ja |
| [OpenCode](../formats/opencode.md) | `opencode` | Ja | Ja |
| [Cursor CLI](../formats/cursor.md) | `cursor` | Ja | Ja |
| [Cursor desktop](../formats/cursor-desktop.md) | `cursor_desktop` | Ja | Ja |
| [pi](../formats/pi.md) | `pi` | Ja | Ja |
| [Campfire](../formats/campfire.md) | `campfire` | Ja | Ja |
| [Cowork](../formats/cowork.md) | `cowork` | Ja | Ja |
| [Grok CLI](../formats/grok.md) | `grok` | Ja | Ja |
| [Grok Bot](../formats/grok-bot.md) | `grok_bot` | Ja | Über lokales Gateway |
| [fx](../formats/fx.md) | `fx` | Ja | Ja |
| [Antigravity](../formats/antigravity.md) | `antigravity` | Ja | Ja |
| [uji](../formats/uji.md) | `uji` | Ja | Ja |
| [Hermes Agent](../formats/hermes.md) | `hermes` | Ja | Nein |
| [Amp](../formats/amp.md) | `amp` | Ja | Nein |
| [Claude Chat](../formats/claude-chat.md) | `claude_chat` | Online-Konto | Nein |
| [ChatGPT](../formats/chatgpt.md) | `chatgpt` | Online-Konto | Nein |

Die lokale Sitzungssuche lässt Online-Konten aus. Wähle ausdrücklich `--from claude_chat` oder `--from chatgpt`, um sie zu lesen. Diese Quellen verwenden private Web-APIs und die bestehende Anmeldung einer App. Voraussetzungen und Einschränkungen stehen in den verlinkten Dokumenten.

### Weiteren Agenten anbinden

Ein Agent ohne nativen Adapter kann [Simple](../formats/simple.md) ausgeben, das JSON-Austauschformat von txcript. Speichere ein Dokument wie dieses als `run.json`:

```json
{
  "messages": [
    { "role": "user", "content": "Finde heraus, warum die Tests fehlschlagen." },
    { "role": "assistant", "content": "Die Testuhr verwendet die Ortszeit." }
  ]
}
```

```sh
txcript continue ./run.json --with claude_code
```

Simple kann auch Gedankengänge, Werkzeugaufrufe, Ergebnisse, Bilder und Metadaten darstellen. `txcript export` gibt dieses Format aus.

## Was erhalten bleibt

Das gemeinsame Modell stellt Nachrichten, Gedankengänge, Werkzeugaufrufe und deren Ergebnisse, Bilder, Metadaten und Tokenverbrauch dar. Was die Konvertierung erhält, hängt davon ab, was die Quelle aufzeichnet und das Ziel darstellen kann. Agentenspezifische Datensätze und nicht unterstützte Felder können verloren gehen.

Die Konvertierung überträgt den Gesprächsverlauf. Das Ziel stellt seine eigenen Systemanweisungen und Werkzeuge bereit; Projektdateien müssen separat verfügbar sein. Natives Laden und Speichern bietet andere Erhaltungsgarantien als die Konvertierung. Siehe die [Einschränkungen der einzelnen Formate](../formats/README.md).

<a name="documentation"></a>

## Dokumentation

- [CLI-Referenz](../usage.md#cli): Befehle, Suche, Kürzen, MCP und Shell-Integration.
- [Rust-API](https://docs.rs/txcript) und [JavaScript-Referenz](../usage.md#npm-package).
- [Transkriptformate](../formats/README.md): Speicheraufbau, Zuordnungen und Einschränkungen mit Quellen und Reverse-Engineering-Notizen.
- [Entwicklung](../usage.md#development) und [Testleitfaden](../../tests/README.md).
- [Mitwirken](../../CONTRIBUTING.md) · [Eine Sicherheitslücke melden](../../SECURITY.md).
- [Änderungsprotokoll](../../CHANGELOG.md) · [Problem melden](https://github.com/skillsynchq/txcript/issues).

## Lizenz

[Apache-2.0](../../LICENSE)
