<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">Poursuivez votre conversation dans un autre agent de programmation.</p>

<p align="center">
  <a href="../../README.md">English</a> | <a href="README.ja.md">日本語</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.zh-TW.md">繁體中文</a> | <a href="README.ko.md">한국어</a> | <a href="README.de.md">Deutsch</a> | <a href="README.es.md">Español</a> | Français | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (Brasil)</a> | <a href="README.ru.md">Русский</a> | <a href="README.mr.md">मराठी</a> | <a href="README.ta.md">தமிழ்</a>
</p>

<p align="center">
  <a href="https://crates.io/crates/txcript"><img src="https://img.shields.io/crates/v/txcript?logo=rust&color=4c71f2" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/txcript"><img src="https://img.shields.io/npm/v/txcript?logo=npm&color=4c71f2" alt="npm"></a>
  <a href="https://docs.rs/txcript"><img src="https://img.shields.io/docsrs/txcript?logo=docsdotrs" alt="docs.rs"></a>
  <a href="https://github.com/skillsynchq/txcript/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/skillsynchq/txcript/ci.yml?branch=main&logo=github&label=ci" alt="CI"></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-555" alt="Licence"></a>
</p>

txcript est une bibliothèque de conversion de sessions d'agents. Commencez une conversation dans Claude Code et poursuivez-la dans Codex, en conservant les messages, le raisonnement et l'historique des outils dans la mesure où la destination les prend en charge.

Créez des outils de recherche, de lecture et d'édition de sessions à partir d'un modèle de transcription commun. txcript gère les formats propres à chaque agent et fournit une API Rust, un paquet JavaScript et une CLI.

[Essayer la CLI](#try-the-cli) · [Utiliser la bibliothèque](#use-the-library) · [Agents pris en charge](#supported-agents) · [Documentation](#documentation)

<p align="center">
  <img src="../assets/demo.gif" alt="Une session OpenCode poursuivie dans Claude Code avec txcript" width="680">
</p>

<a name="try-the-cli"></a>

## Essayer la CLI

Téléchargez un binaire pour macOS, Linux ou Windows depuis les [versions publiées](https://github.com/skillsynchq/txcript/releases), ou installez depuis les sources avec Rust 1.96 ou une version ultérieure :

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli --locked
```

Trouvez une session Claude Code et poursuivez-la dans Codex :

```sh
txcript list --from claude_code
txcript continue <session-id> --with codex
```

Utilisez un ID de la liste ; un préfixe non ambigu convient aussi. txcript écrit une nouvelle session au format natif et lance Codex dans le répertoire de travail enregistré. La session source est conservée. Installez l'agent de destination et connectez-vous avant de poursuivre.

Autres façons d'utiliser vos sessions :

```sh
txcript query "relay bug"                # rechercher dans l'historique local
txcript view <session-id>                # lire une conversation dans le terminal
txcript crop <session-id>                # modifier ou raccourcir l'historique dans une nouvelle copie
txcript export <session-id> --out run.json
```

Transférez `run.json` sur une autre machine, puis exécutez `txcript continue ./run.json --with claude_code`. Transférez les fichiers du projet séparément.

Exécutez `txcript mcp` pour permettre à un client MCP de lister, rechercher et lire les sessions précédentes. Ses outils sont en lecture seule. Consultez la [référence CLI](../usage.md#cli) pour les filtres, les plages de messages et l'intégration au shell.

<a name="use-the-library"></a>

## Utiliser la bibliothèque

### Rust

```sh
cargo add txcript
```

Convertissez une transcription Claude Code au format natif de Codex :

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

Utilisez les implémentations de `Store` pour découvrir, charger et enregistrer les sessions dans le stockage propre à chaque agent. Convertissez-les en `Transcript<Common>` pour rechercher, découper ou afficher des conversations avec la même API pour tous les agents. Consultez l'[API Rust](https://docs.rs/txcript) et les [exemples](../usage.md#rust-crate).

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

Le paquet inclut du WebAssembly précompilé pour Node et Bun. Il convertit les transcriptions et y effectue des recherches en mémoire ; votre application gère les fichiers et le stockage. Consultez la [référence JavaScript](../usage.md#npm-package).

<a name="supported-agents"></a>

## Agents pris en charge

Chaque nom renvoie à la documentation du format correspondant. Utilisez l'ID avec `--from` et `--with`.

| Agent | ID | Lire depuis | Poursuivre dans |
|---|---|:---:|:---:|
| [Claude Code](../formats/claude-code.md) | `claude_code` | Oui | Oui |
| [Codex](../formats/codex.md) | `codex` | Oui | Oui |
| [OpenCode](../formats/opencode.md) | `opencode` | Oui | Oui |
| [Cursor CLI](../formats/cursor.md) | `cursor` | Oui | Oui |
| [Cursor desktop](../formats/cursor-desktop.md) | `cursor_desktop` | Oui | Oui |
| [pi](../formats/pi.md) | `pi` | Oui | Oui |
| [Campfire](../formats/campfire.md) | `campfire` | Oui | Oui |
| [Cowork](../formats/cowork.md) | `cowork` | Oui | Oui |
| [Grok CLI](../formats/grok.md) | `grok` | Oui | Oui |
| [Grok Bot](../formats/grok-bot.md) | `grok_bot` | Oui | Via la passerelle locale |
| [fx](../formats/fx.md) | `fx` | Oui | Oui |
| [Antigravity](../formats/antigravity.md) | `antigravity` | Oui | Oui |
| [uji](../formats/uji.md) | `uji` | Oui | Oui |
| [Hermes Agent](../formats/hermes.md) | `hermes` | Oui | Non |
| [Amp](../formats/amp.md) | `amp` | Oui | Non |
| [Claude Chat](../formats/claude-chat.md) | `claude_chat` | Compte en ligne | Non |
| [ChatGPT](../formats/chatgpt.md) | `chatgpt` | Compte en ligne | Non |

La découverte locale exclut les comptes en ligne. Sélectionnez explicitement `--from claude_chat` ou `--from chatgpt` pour les lire. Ces sources utilisent des API web privées et réutilisent la connexion d'une application existante ; leurs prérequis et limites figurent dans les documents liés.

### Ajouter un autre agent

Un agent sans adaptateur natif peut produire du [Simple](../formats/simple.md), le format JSON d'échange de txcript. Enregistrez un document comme celui-ci dans `run.json` :

```json
{
  "messages": [
    { "role": "user", "content": "Trouve pourquoi les tests échouent." },
    { "role": "assistant", "content": "L'horloge des tests utilise l'heure locale." }
  ]
}
```

```sh
txcript continue ./run.json --with claude_code
```

Simple représente aussi le raisonnement, les appels d'outils, leurs résultats, les images et les métadonnées. C'est le format produit par `txcript export`.

## Ce qui est conservé

Le modèle commun représente les messages, le raisonnement, les appels d'outils et leurs résultats, les images, les métadonnées et la consommation de tokens. Ce qui est conservé dépend de ce que la source enregistre et de ce que la destination peut représenter. Les enregistrements propres à un agent et les champs non pris en charge peuvent être perdus.

La conversion transfère l'historique de la conversation. La destination fournit ses propres instructions système et outils, et les fichiers du projet doivent être disponibles séparément. Le chargement et l'enregistrement natifs offrent des garanties de préservation différentes de celles de la conversion ; consultez les [limites de chaque format](../formats/README.md).

<a name="documentation"></a>

## Documentation

- [Référence CLI](../usage.md#cli) : commandes, recherche, découpage, MCP et intégration au shell.
- [API Rust](https://docs.rs/txcript) et [référence JavaScript](../usage.md#npm-package).
- [Formats de transcription](../formats/README.md) : organisation du stockage, correspondances et limites, avec sources et notes de rétro-ingénierie.
- [Développement](../usage.md#development) et [guide des tests](../../tests/README.md).
- [Contribuer](../../CONTRIBUTING.md) · [Signaler une vulnérabilité](../../SECURITY.md).
- [Historique des modifications](../../CHANGELOG.md) · [Signaler un problème](https://github.com/skillsynchq/txcript/issues).

## Licence

[Apache-2.0](../../LICENSE)
