<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">Continua la conversazione in un altro agente di programmazione.</p>

<p align="center">
  <a href="../../README.md">English</a> | <a href="README.ja.md">日本語</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.zh-TW.md">繁體中文</a> | <a href="README.ko.md">한국어</a> | <a href="README.de.md">Deutsch</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | Italiano | <a href="README.pt-BR.md">Português (Brasil)</a> | <a href="README.ru.md">Русский</a> | <a href="README.mr.md">मराठी</a> | <a href="README.ta.md">தமிழ்</a>
</p>

<p align="center">
  <a href="https://crates.io/crates/txcript"><img src="https://img.shields.io/crates/v/txcript?logo=rust&color=4c71f2" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/txcript"><img src="https://img.shields.io/npm/v/txcript?logo=npm&color=4c71f2" alt="npm"></a>
  <a href="https://docs.rs/txcript"><img src="https://img.shields.io/docsrs/txcript?logo=docsdotrs" alt="docs.rs"></a>
  <a href="https://github.com/skillsynchq/txcript/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/skillsynchq/txcript/ci.yml?branch=main&logo=github&label=ci" alt="CI"></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-555" alt="Licenza"></a>
</p>

txcript è una libreria per convertire le sessioni degli agenti. Inizia una conversazione in Claude Code e proseguila in Codex, conservando messaggi, ragionamento e cronologia degli strumenti nei limiti di ciò che la destinazione supporta.

Crea strumenti per cercare, visualizzare e modificare sessioni usando un unico modello di trascrizione. txcript gestisce i formati specifici degli agenti e offre un'API Rust, un pacchetto JavaScript e una CLI.

[Prova la CLI](#try-the-cli) · [Usa la libreria](#use-the-library) · [Agenti supportati](#supported-agents) · [Documentazione](#documentation)

<p align="center">
  <img src="../assets/demo.gif" alt="Una sessione OpenCode proseguita in Claude Code con txcript" width="680">
</p>

<a name="try-the-cli"></a>

## Prova la CLI

Scarica un binario per macOS, Linux o Windows dalle [release](https://github.com/skillsynchq/txcript/releases), oppure installa dai sorgenti con Rust 1.96 o successivo:

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli --locked
```

Trova una sessione Claude Code e proseguila in Codex:

```sh
txcript list --from claude_code
txcript continue <session-id> --with codex
```

Usa un ID dell'elenco; va bene anche un prefisso non ambiguo. txcript scrive una nuova sessione in formato nativo e avvia Codex nella directory di lavoro registrata. La sessione originale viene conservata. Installa l'agente di destinazione ed effettua l'accesso prima di continuare.

Altri modi per lavorare con le sessioni:

```sh
txcript query "relay bug"                # cerca nella cronologia locale
txcript view <session-id>                # leggi una conversazione nel terminale
txcript crop <session-id>                # modifica o ritaglia la cronologia in una nuova copia
txcript export <session-id> --out run.json
```

Trasferisci `run.json` su un'altra macchina e continua con `txcript continue ./run.json --with claude_code`. Trasferisci separatamente i file del progetto.

Esegui `txcript mcp` per consentire a un client MCP di elencare, cercare e leggere le sessioni precedenti. Gli strumenti sono di sola lettura. Consulta il [riferimento CLI](../usage.md#cli) per filtri, intervalli di messaggi e integrazione con la shell.

<a name="use-the-library"></a>

## Usa la libreria

### Rust

```sh
cargo add txcript
```

Converti una trascrizione Claude Code nel formato nativo di Codex:

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

Usa le implementazioni di `Store` per individuare, caricare e salvare sessioni nell'archivio di ciascun agente. Converti in `Transcript<Common>` per cercare, ritagliare o visualizzare conversazioni con la stessa API per tutti gli agenti. Consulta l'[API Rust](https://docs.rs/txcript) e gli [esempi](../usage.md#rust-crate).

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

Il pacchetto include WebAssembly precompilato per Node e Bun. Converte le trascrizioni ed esegue ricerche in memoria; l'applicazione gestisce file e archiviazione. Consulta il [riferimento JavaScript](../usage.md#npm-package).

<a name="supported-agents"></a>

## Agenti supportati

Ogni nome rimanda alla documentazione del relativo formato. Usa l'ID con `--from` e `--with`.

| Agente | ID | Lettura da | Continuazione in |
|---|---|:---:|:---:|
| [Claude Code](../formats/claude-code.md) | `claude_code` | Sì | Sì |
| [Codex](../formats/codex.md) | `codex` | Sì | Sì |
| [OpenCode](../formats/opencode.md) | `opencode` | Sì | Sì |
| [Cursor CLI](../formats/cursor.md) | `cursor` | Sì | Sì |
| [Cursor desktop](../formats/cursor-desktop.md) | `cursor_desktop` | Sì | Sì |
| [pi](../formats/pi.md) | `pi` | Sì | Sì |
| [Campfire](../formats/campfire.md) | `campfire` | Sì | Sì |
| [Cowork](../formats/cowork.md) | `cowork` | Sì | Sì |
| [Grok CLI](../formats/grok.md) | `grok` | Sì | Sì |
| [Grok Bot](../formats/grok-bot.md) | `grok_bot` | Sì | Tramite gateway locale |
| [fx](../formats/fx.md) | `fx` | Sì | Sì |
| [Antigravity](../formats/antigravity.md) | `antigravity` | Sì | Sì |
| [uji](../formats/uji.md) | `uji` | Sì | Sì |
| [Hermes Agent](../formats/hermes.md) | `hermes` | Sì | No |
| [Amp](../formats/amp.md) | `amp` | Sì | No |
| [Claude Chat](../formats/claude-chat.md) | `claude_chat` | Account online | No |
| [ChatGPT](../formats/chatgpt.md) | `chatgpt` | Account online | No |

La ricerca delle sessioni locali esclude gli account online. Seleziona esplicitamente `--from claude_chat` o `--from chatgpt` per leggerli. Queste fonti usano API web private e riutilizzano l'accesso di un'applicazione esistente; requisiti e limiti sono descritti nei documenti collegati.

### Aggiungi un altro agente

Un agente senza adattatore nativo può generare [Simple](../formats/simple.md), il formato JSON di scambio di txcript. Salva un documento come questo in `run.json`:

```json
{
  "messages": [
    { "role": "user", "content": "Scopri perché i test falliscono." },
    { "role": "assistant", "content": "L'orologio dei test usa l'ora locale." }
  ]
}
```

```sh
txcript continue ./run.json --with claude_code
```

Simple rappresenta anche ragionamento, chiamate agli strumenti, risultati, immagini e metadati. È il formato prodotto da `txcript export`.

## Cosa viene conservato

Il modello comune rappresenta messaggi, ragionamento, chiamate agli strumenti e relativi risultati, immagini, metadati e consumo di token. Ciò che viene conservato dipende da quanto registra l'origine e da cosa può rappresentare la destinazione. I record specifici di un agente e i campi non supportati possono andare persi.

La conversione trasferisce la cronologia della conversazione. La destinazione fornisce le proprie istruzioni di sistema e i propri strumenti, mentre i file del progetto devono essere disponibili separatamente. Il caricamento e il salvataggio nativi hanno garanzie di conservazione diverse dalla conversione; consulta i [limiti di ciascun formato](../formats/README.md).

<a name="documentation"></a>

## Documentazione

- [Riferimento CLI](../usage.md#cli): comandi, ricerca, ritaglio, MCP e integrazione con la shell.
- [API Rust](https://docs.rs/txcript) e [riferimento JavaScript](../usage.md#npm-package).
- [Formati delle trascrizioni](../formats/README.md): struttura degli archivi, corrispondenze e limiti, con fonti e note di reverse engineering.
- [Sviluppo](../usage.md#development) e [guida ai test](../../tests/README.md).
- [Contribuire](../../CONTRIBUTING.md) · [Segnalare una vulnerabilità](../../SECURITY.md).
- [Registro delle modifiche](../../CHANGELOG.md) · [Segnala un problema](https://github.com/skillsynchq/txcript/issues).

## Licenza

[Apache-2.0](../../LICENSE)
