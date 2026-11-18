<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">Continue sua conversa em outro agente de programação.</p>

<p align="center">
  <a href="../../README.md">English</a> | <a href="README.ja.md">日本語</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.zh-TW.md">繁體中文</a> | <a href="README.ko.md">한국어</a> | <a href="README.de.md">Deutsch</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.it.md">Italiano</a> | Português (Brasil) | <a href="README.ru.md">Русский</a> | <a href="README.mr.md">मराठी</a> | <a href="README.ta.md">தமிழ்</a>
</p>

<p align="center">
  <a href="https://crates.io/crates/txcript"><img src="https://img.shields.io/crates/v/txcript?logo=rust&color=4c71f2" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/txcript"><img src="https://img.shields.io/npm/v/txcript?logo=npm&color=4c71f2" alt="npm"></a>
  <a href="https://docs.rs/txcript"><img src="https://img.shields.io/docsrs/txcript?logo=docsdotrs" alt="docs.rs"></a>
  <a href="https://github.com/skillsynchq/txcript/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/skillsynchq/txcript/ci.yml?branch=main&logo=github&label=ci" alt="CI"></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-555" alt="Licença"></a>
</p>

txcript é uma biblioteca para converter sessões de agentes. Comece uma conversa no Claude Code e continue no Codex, levando as mensagens, o raciocínio e o histórico de ferramentas que o destino suporta.

Crie ferramentas de busca, visualização e edição de sessões com um único modelo de transcrição. txcript cuida dos formatos específicos de cada agente e oferece uma API Rust, um pacote JavaScript e uma CLI.

[Experimente a CLI](#try-the-cli) · [Use a biblioteca](#use-the-library) · [Agentes compatíveis](#supported-agents) · [Documentação](#documentation)

<p align="center">
  <img src="../assets/demo.gif" alt="Uma sessão do OpenCode continuada no Claude Code com txcript" width="680">
</p>

<a name="try-the-cli"></a>

## Experimente a CLI

Baixe um binário para macOS, Linux ou Windows em [Releases](https://github.com/skillsynchq/txcript/releases), ou instale a partir do código-fonte com Rust 1.96 ou mais recente:

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli --locked
```

Encontre uma sessão do Claude Code e continue no Codex:

```sh
txcript list --from claude_code
txcript continue <session-id> --with codex
```

Use um ID da lista; um prefixo sem ambiguidade também funciona. txcript grava uma nova sessão no formato nativo e inicia o Codex no diretório de trabalho registrado. A sessão original é preservada. Instale o agente de destino e faça login antes de continuar.

Outras formas de trabalhar com suas sessões:

```sh
txcript query "relay bug"                # buscar no histórico local
txcript view <session-id>                # ler uma conversa no terminal
txcript crop <session-id>                # editar ou recortar o histórico em uma nova cópia
txcript export <session-id> --out run.json
```

Transfira `run.json` para outra máquina e continue com `txcript continue ./run.json --with claude_code`. Transfira os arquivos do projeto separadamente.

Execute `txcript mcp` para que um cliente MCP possa listar, buscar e ler sessões anteriores. As ferramentas são somente leitura. Consulte a [referência da CLI](../usage.md#cli) para filtros, intervalos de mensagens e integração com o shell.

<a name="use-the-library"></a>

## Use a biblioteca

### Rust

```sh
cargo add txcript
```

Converta uma transcrição do Claude Code para o formato nativo do Codex:

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

Use as implementações de `Store` para descobrir, carregar e salvar sessões no armazenamento de cada agente. Converta para `Transcript<Common>` para buscar, recortar ou exibir conversas com a mesma API entre agentes. Consulte a [API Rust](https://docs.rs/txcript) e os [exemplos](../usage.md#rust-crate).

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

O pacote inclui WebAssembly pré-compilado para Node e Bun. Ele converte transcrições e faz buscas em memória; sua aplicação cuida dos arquivos e do armazenamento. Consulte a [referência JavaScript](../usage.md#npm-package).

<a name="supported-agents"></a>

## Agentes compatíveis

Cada nome leva à documentação do respectivo formato. Use o ID com `--from` e `--with`.

| Agente | ID | Ler de | Continuar em |
|---|---|:---:|:---:|
| [Claude Code](../formats/claude-code.md) | `claude_code` | Sim | Sim |
| [Codex](../formats/codex.md) | `codex` | Sim | Sim |
| [OpenCode](../formats/opencode.md) | `opencode` | Sim | Sim |
| [Cursor CLI](../formats/cursor.md) | `cursor` | Sim | Sim |
| [Cursor desktop](../formats/cursor-desktop.md) | `cursor_desktop` | Sim | Sim |
| [pi](../formats/pi.md) | `pi` | Sim | Sim |
| [Campfire](../formats/campfire.md) | `campfire` | Sim | Sim |
| [Cowork](../formats/cowork.md) | `cowork` | Sim | Sim |
| [Grok CLI](../formats/grok.md) | `grok` | Sim | Sim |
| [Grok Bot](../formats/grok-bot.md) | `grok_bot` | Sim | Via gateway local |
| [fx](../formats/fx.md) | `fx` | Sim | Sim |
| [Antigravity](../formats/antigravity.md) | `antigravity` | Sim | Sim |
| [uji](../formats/uji.md) | `uji` | Sim | Sim |
| [Hermes Agent](../formats/hermes.md) | `hermes` | Sim | Não |
| [Amp](../formats/amp.md) | `amp` | Sim | Não |
| [Claude Chat](../formats/claude-chat.md) | `claude_chat` | Conta online | Não |
| [ChatGPT](../formats/chatgpt.md) | `chatgpt` | Conta online | Não |

A descoberta local ignora as contas online. Selecione explicitamente `--from claude_chat` ou `--from chatgpt` para lê-las. Essas fontes usam APIs web privadas e reutilizam o login de um aplicativo existente; os requisitos e as limitações estão nos documentos vinculados.

### Adicione outro agente

Um agente sem adaptador nativo pode gerar [Simple](../formats/simple.md), o formato JSON de intercâmbio do txcript. Salve um documento como este em `run.json`:

```json
{
  "messages": [
    { "role": "user", "content": "Descubra por que os testes falham." },
    { "role": "assistant", "content": "O relógio dos testes usa o horário local." }
  ]
}
```

```sh
txcript continue ./run.json --with claude_code
```

Simple também representa raciocínio, chamadas de ferramentas, resultados, imagens e metadados. É o formato gerado por `txcript export`.

## O que é preservado

O modelo comum representa mensagens, raciocínio, chamadas de ferramentas e seus resultados, imagens, metadados e uso de tokens. O que é preservado depende do que a origem registra e do que o destino consegue representar. Registros específicos de um agente e campos sem suporte podem ser perdidos.

A conversão transfere o histórico da conversa. O destino fornece suas próprias instruções de sistema e ferramentas, e os arquivos do projeto devem estar disponíveis separadamente. O carregamento e o salvamento nativos têm garantias de preservação diferentes das da conversão; consulte as [limitações de cada formato](../formats/README.md).

<a name="documentation"></a>

## Documentação

- [Referência da CLI](../usage.md#cli): comandos, busca, recorte, MCP e integração com o shell.
- [API Rust](https://docs.rs/txcript) e [referência JavaScript](../usage.md#npm-package).
- [Formatos de transcrição](../formats/README.md): estrutura do armazenamento, mapeamentos e limitações, com fontes e notas de engenharia reversa.
- [Desenvolvimento](../usage.md#development) e [guia de testes](../../tests/README.md).
- [Contribuir](../../CONTRIBUTING.md) · [Relatar uma vulnerabilidade](../../SECURITY.md).
- [Histórico de alterações](../../CHANGELOG.md) · [Relatar um problema](https://github.com/skillsynchq/txcript/issues).

## Licença

[Apache-2.0](../../LICENSE)
