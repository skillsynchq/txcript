<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">Una libreria per spostare sessioni tra harness</p>

<p align="center">
  <a href="../../README.md">English</a> | <a href="README.ja.md">日本語</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.zh-TW.md">繁體中文</a> | <a href="README.ko.md">한국어</a> | <a href="README.de.md">Deutsch</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | Italiano | <a href="README.pt-BR.md">Português (Brasil)</a> | <a href="README.ru.md">Русский</a> | <a href="README.mr.md">मराठी</a> | <a href="README.ta.md">தமிழ்</a>
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

Inizia una sessione in Claude Code, raggiungi un limite di utilizzo o un punto morto, e riprendila in Codex con l'intera conversazione, il reasoning e la cronologia degli strumenti intatti:

<p align="center">
  <img src="../assets/demo.gif" alt="txcript continue: an OpenCode session resumed in Claude Code">
</p>

txcript mappa il formato di trascrizione nativo di ogni harness attraverso un modello comune tipizzato. Il caricamento/salvataggio nativo è lossless al byte; la conversione tra harness preserva messaggi, reasoning, chiamate agli strumenti, risultati degli strumenti, immagini, metadati e dati di utilizzo ove disponibili. Viene distribuito come [**CLI**](#cli), [**crate Rust**](#crate-rust) e [**pacchetto npm**](#pacchetto-npm).

## In evidenza

- **10 harness, un solo modello**: ogni formato converte attraverso `Transcript<Common>`, quindi aggiungere un harness lo collega a tutti gli altri.
- **Round-trip lossless al byte**: caricare e salvare una sessione nel suo stesso formato la riproduce esattamente.
- **Continua ovunque**: `txcript continue <id> --with <harness>` riscrive una sessione nel formato nativo di un altro harness e lo lancia. L'originale non viene mai modificato.
- **Cerca in tutto**: ricerca fuzzy/per sottostringa su ogni sessione della macchina (sintassi in stile fzf, basata su [nucleo](https://github.com/helix-editor/nucleo)), come API di libreria, query CLI one-shot o picker interattivo.
- **Server MCP**: `txcript mcp` espone gli strumenti in sola lettura `list_sessions`, `search_sessions` e `read_session`, così gli agenti possono attingere alle sessioni passate come contesto.
- **Formati documentati**: il formato su disco di ogni harness è descritto in [`docs/formats/`](../formats), con la provenienza di ogni affermazione (documentazione ufficiale, permalink ai sorgenti o note di reverse engineering).

## Harness supportati

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

Discovery, elenco, ricerca, `view` e round-trip nativi funzionano per ogni harness. Le stringhe `id` sono quelle richieste dalla CLI e dalle API WASM.

| Harness | id | Sessioni su disco | Formato nativo | Conversione | Continua in | Doc |
|---|---|---|---|:---:|:---:|---|
| [Claude Code](https://claude.com/claude-code) | `claude_code` | `~/.claude/projects/` | JSONL | ⇄ | ✓ | [spec](../formats/claude-code.md) |
| [Codex](https://github.com/openai/codex) | `codex` | `~/.codex/sessions/` | JSONL di rollout | ⇄ | ✓ | [spec](../formats/codex.md) |
| [OpenCode](https://opencode.ai) | `opencode` | `~/.local/share/opencode/opencode.db` | SQLite | ⇄ | ✓ | [spec](../formats/opencode.md) |
| [pi](https://pi.dev) | `pi` | `~/.pi/agent/sessions/` | JSONL | ⇄ | ✓ | [spec](../formats/pi.md) |
| [Campfire](../formats/campfire.md) | `campfire` | `~/.campfire/agent/sessions/` | JSONL | ⇄ | ✓ | [spec](../formats/campfire.md) |
| [Cursor CLI](https://cursor.com/cli) | `cursor` | `~/.cursor/chats/` | SQLite | ⇄ | ✓ | [spec](../formats/cursor.md) |
| [Cursor desktop](https://cursor.com) | `cursor_desktop` | `<Cursor User dir>/globalStorage/` | SQLite | ⇄ | ✓ | [spec](../formats/cursor-desktop.md) |
| [Grok CLI](https://github.com/xai-org/grok-build) | `grok` | `~/.grok/sessions/` | directory di sessione (JSON) | ⇄ | ✓ | [spec](../formats/grok.md) |
| [Amp](https://ampcode.com) | `amp` | `~/.local/share/amp/threads/` | JSON del thread | → | — <sup>1</sup> | [spec](../formats/amp.md) |
| [Antigravity](https://antigravity.google) | `antigravity` | `~/.gemini/antigravity-cli/` | SQLite | ⇄ | ✓ | [spec](../formats/antigravity.md) |

<sup>1</sup> I thread di Amp risiedono lato server e la CLI non ha importazione: le sessioni convertono *da* Amp, ma non possono essere continuate verso di esso.

## Installazione

**CLI** (installa il binario `txcript`):

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli
# or from a checkout: cargo install --path cli
```

**Crate Rust**:

```sh
cargo add txcript
```

**Pacchetto npm** (WASM precompilato, nessuna toolchain Rust necessaria):

```sh
bun add txcript     # or: npm install txcript
```

## CLI

Scopri le sessioni locali e continuane una in qualsiasi harness:

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

`continue` scrive la sessione dove l'harness di destinazione conserva le proprie sessioni, poi lo lancia su di essa, cedendogli il terminale:

- Stesso harness: riprende l'originale sul posto.
- Cross-harness (`--with`): risintetizza la sessione nel formato nativo di destinazione. Ciò che viene scritto è sempre una copia; la sessione sorgente non viene mai modificata né rimossa.
- Il comando di lancio è specifico per harness e sovrascrivibile: imposta `TRANSCRIPT_<HARNESS>_RESUME_CMD` con un template `{id}`, ad es. `TRANSCRIPT_CODEX_RESUME_CMD="codex resume {id}"`.

`view` stampa la sessione come testo compatto, con ogni messaggio numerato da una riga `── #N ──`. `#range` seleziona i messaggi in base a quegli ordinali stampati, 1-based e inclusivi:

- `abc#7`: solo il messaggio 7
- `abc#5-12`: messaggi da 5 a 12
- `abc#5-`: dal messaggio 5 alla fine
- `abc#-10`: dall'inizio al messaggio 10

`continue` accetta lo stesso suffisso e continua solo quei messaggi come nuova sessione. Un intervallo che separerebbe una chiamata a uno strumento dal suo risultato viene rifiutato, e l'errore suggerisce l'intervallo valido più vicino.

`export` scrive la sessione come documento [Simple](../formats/simple.md), su stdout o in `--out <file>`. Il documento è il rendering completo del modello canonico — tutto ciò che `continue` porta con sé tra harness — indipendente da dove un harness conserva le proprie sessioni, così si sposta da una macchina all'altra come file:

```sh
txcript export 0dc114bf --out session.json       # on this machine
txcript continue ./session.json --with claude_code   # on the other one
```

La working directory registrata viene mantenuta quando esiste sulla macchina di importazione, altrimenti viene sostituita dalla directory in cui `continue` viene eseguito. `export` accetta lo stesso suffisso `#range` e lo stesso ambito `--from` di `view`.

### Ricerca

```sh
txcript query 'relay bug'                # one-shot: ranked hits, highlighted
txcript query                            # fzf-style picker; Enter continues
    [--from <harness>]                   #   search only <harness> (default: all)
    [--with <harness>]                   #   continue the pick in <harness>
```

Il picker è privo di dipendenze (ANSI in raw mode): digita per filtrare con la sintassi fuzzy in stile fzf, frecce / ctrl-p/n per spostarti, Invio per continuare la selezione nel suo harness (o con `--with`), Esc per annullare. Ogni riga mostra quale tipo di contenuto ha prodotto la corrispondenza: testo utente, testo assistente, thinking, uso di strumenti, output di strumenti o metadati di sessione.

### Server MCP

```sh
txcript mcp                              # stdio transport
```

Espone tre strumenti in sola lettura; i loro filtri opzionali corrispondono a quelli della CLI:

- `list_sessions(from?, cwd?)`
- `search_sessions(pattern, from?, cwd?)`
- `read_session(id, from?)`

<sub>\* Omettendo `from` si includono tutti gli harness; omettendo `cwd` non si applica alcun filtro di directory. Le sessioni senza una working directory registrata corrispondono solo quando `cwd` viene omesso.</sub>

### Completamenti shell

```sh
txcript completion zsh > ~/.zfunc/_txcript      # or wherever your fpath looks
source <(txcript completion bash)               # bash, ad hoc
txcript completion fish > ~/.config/fish/completions/txcript.fish
```

## Crate Rust

```toml
[dependencies]
txcript = "0.6"
# Drops the OpenCode SQLite store (rusqlite); the OpenCode codec stays available.
# txcript = { version = "0.6", default-features = false }
```

Tre livelli, dal più piccolo al più grande:

- `Codec`: `to_common` / `from_common` per harness; `convert::<A, B>` li concatena attraverso il modello canonico.
- `TextCodec`: `from_text` / `to_text` per analizzare e renderizzare il testo di sessione nativo di un harness, senza I/O.
- `Store`: discovery/caricamento/salvataggio su un backend reale (directory di sessione, o DB SQLite per OpenCode ed entrambi i Cursor).

Converti in memoria (senza filesystem):

```rust
use txcript::harness::{claude_code, codex};
use txcript::{Codec, TextCodec, convert};

let claude = claude_code::ClaudeCode::from_text(jsonl_text)?;          // Transcript<ClaudeCode>
let codex = convert::<claude_code::ClaudeCode, codex::Codex>(&claude)?; // Transcript<Codex>
let codex_text = codex::Codex::to_text(&codex)?;                       // native rollout JSONL
```

Oppure passa dal disco con uno `Store`:

```rust
use txcript::harness::{claude_code, codex};
use txcript::{Store, convert};

let store = claude_code::ClaudeStore::default_root().expect("home dir");
let found = store.discover()?;                       // cheap metadata scan
let claude = store.load(&found[0].reference)?;       // Transcript<ClaudeCode>

let codex = convert::<_, codex::Codex>(&claude)?;
codex::CodexStore::default_root().expect("home dir").save(&codex)?;  // resumable on disk
```

Il modello canonico è `Transcript<Common>`: `Meta` + `Vec<Message>`, dove un `Message` contiene `Block` tipizzati (`Text`, `Thinking`, `ToolUse`, `ToolResult`, `Image`) e un enum `Tool` tipizzato.

Anche i comandi slash eseguiti dall'utente nell'harness (`/release patch`) sono canonici: una chiamata `Tool::Command` sul turno utente, associata a ciò che il comando ha restituito come `ToolResult`.

### Ricerca (feature `search`, attiva di default)

`txcript::search` supporta la ricerca fuzzy e per sottostringa sulle trascrizioni tramite [nucleo](https://github.com/helix-editor/nucleo). Ricerca one-shot:

```rust
use txcript::search::{Query, search};

let hits = search(&common, &Query::fuzzy("relay bug"));   // fzf syntax: 'exact ^prefix !not
for hit in hits {
    // hit.origin: User | Assistant | Thinking | ToolUse | ToolResult | Meta
    // hit.span addresses the message; hit.highlights are char ranges into hit.line
    let messages = common.fragment(&hit.span);            // zero-copy: Option<&[Message]>
}
```

Per una ricerca in stile picker, costruisci un `Index` una volta sola e interrogalo a ogni pressione di tasto:

```rust
use txcript::search::{DocKey, Index, Query};

let mut index = Index::new();
index.insert(DocKey { harness, id }, &common);   // re-insert replaces; caller owns refresh
let matches = index.query(&Query::fuzzy("srch")); // ranked docs, best lines as hits
```

Un pattern vuoto restituisce i documenti dal più recente al più vecchio. Gli output degli strumenti sono esclusi di default; usa `Origin::ALL` per includerli. `Query.harnesses`, `Query.limit` e `Query.hits_per_doc` restringono i risultati.

### Proiezione testuale

`txcript::text::to_text(&common)` è la proiezione dietro [`txcript view`](#cli): un rendering unidirezionale e parsimonioso in token di `Transcript<Common>` da usare come contesto per LLM. Mantiene i messaggi, il testo di reasoning e chiamate/risultati compatti degli strumenti; i payload utili solo al replay (reasoning cifrato, contabilità dell'utilizzo, byte delle immagini inline) vengono omessi. `to_text_fragment(&common, &span)` renderizza uno `Span` del corpo, mantenendo l'ordinale di ogni messaggio nella sessione completa.

## Pacchetto npm

Il pacchetto npm distribuisce il codec come WASM precompilato per Bun, Node e browser. L'host JS possiede tutto l'I/O e invoca il modulo per la trasformazione; il livello `Store` (filesystem, SQLite, sottoprocessi) resta nativo ed è escluso dalla build WASM.

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

Testo in ingresso / testo in uscita: `input` è il testo di sessione nativo dell'harness sorgente e il risultato è quello di destinazione. Nomi di harness non validi o input non analizzabile sollevano un `Error` JS.

| Harness | Testo di sessione |
|---|---|
| `claude_code`, `codex`, `pi`, `campfire` | JSONL di sessione |
| `opencode` | JSON di `opencode export` |
| `cursor` | export JSON dello `store.db` della sessione |
| `cursor_desktop` | dump JSON delle righe `state.vscdb` della sessione |
| `grok` | bundle JSON dei file della directory di sessione |
| `amp` | JSON di `amp threads export` |
| `antigravity` | dump JSON del database delle conversazioni, blob protobuf codificati in esadecimale |

Per compilare invece il wasm dai sorgenti:

```sh
git clone https://github.com/skillsynchq/txcript.git
cd txcript
bun run setup        # once: wasm target + wasm-bindgen-cli
bun run build        # produces ./pkg
```

## Documentazione dei formati

Non tutti questi formati di trascrizione sono documentati dai rispettivi vendor. [`docs/formats/`](../formats) contiene un documento per harness che copre dove vivono le sessioni su disco, come la discovery le trova, una dissezione di ogni parte del formato e le sue stranezze, ciascuno etichettato con la provenienza di ciò che afferma: documentazione ufficiale, il codice di serializzazione open source dell'harness stesso (citato con permalink ancorati al commit), o reverse engineering.

## Sviluppo

```sh
cargo test                                          # native suite
cargo test --no-default-features                    # without the SQLite store
bun run build && bun examples/convert.ts <file> <from> <to>
```

Il binario vive in un proprio crate workspace (`cli/`, pacchetto `txcript-cli`), così le sue dipendenze (clap) non toccano mai i consumatori della libreria.

## Licenza

[Apache-2.0](../../LICENSE)
