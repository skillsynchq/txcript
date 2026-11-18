<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">Continúa tu conversación en otro agente de programación.</p>

<p align="center">
  <a href="../../README.md">English</a> | <a href="README.ja.md">日本語</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.zh-TW.md">繁體中文</a> | <a href="README.ko.md">한국어</a> | <a href="README.de.md">Deutsch</a> | Español | <a href="README.fr.md">Français</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (Brasil)</a> | <a href="README.ru.md">Русский</a> | <a href="README.mr.md">मराठी</a> | <a href="README.ta.md">தமிழ்</a>
</p>

<p align="center">
  <a href="https://crates.io/crates/txcript"><img src="https://img.shields.io/crates/v/txcript?logo=rust&color=4c71f2" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/txcript"><img src="https://img.shields.io/npm/v/txcript?logo=npm&color=4c71f2" alt="npm"></a>
  <a href="https://docs.rs/txcript"><img src="https://img.shields.io/docsrs/txcript?logo=docsdotrs" alt="docs.rs"></a>
  <a href="https://github.com/skillsynchq/txcript/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/skillsynchq/txcript/ci.yml?branch=main&logo=github&label=ci" alt="CI"></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-555" alt="Licencia"></a>
</p>

txcript es una biblioteca para convertir sesiones de agentes. Empieza una conversación en Claude Code y continúala en Codex, conservando los mensajes, el razonamiento y el historial de herramientas que admita el destino.

Crea buscadores, visores y editores de sesiones sobre un único modelo de transcripción. txcript se encarga de los formatos de cada agente y ofrece una API de Rust, un paquete JavaScript y una CLI.

[Probar la CLI](#try-the-cli) · [Usar la biblioteca](#use-the-library) · [Agentes compatibles](#supported-agents) · [Documentación](#documentation)

<p align="center">
  <img src="../assets/demo.gif" alt="Una sesión de OpenCode continuada en Claude Code con txcript" width="680">
</p>

<a name="try-the-cli"></a>

## Probar la CLI

Descarga un binario para macOS, Linux o Windows desde [Releases](https://github.com/skillsynchq/txcript/releases), o instala desde el código fuente con Rust 1.96 o posterior:

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli --locked
```

Busca una sesión de Claude Code y continúala en Codex:

```sh
txcript list --from claude_code
txcript continue <session-id> --with codex
```

Usa un ID de la lista; también sirve un prefijo que no sea ambiguo. txcript escribe una nueva sesión en formato nativo y abre Codex en el directorio de trabajo registrado. La sesión original se conserva. Instala el agente de destino e inicia sesión antes de continuar.

Otras formas de trabajar con tus sesiones:

```sh
txcript query "relay bug"                # buscar en el historial local
txcript view <session-id>                # leer una conversación en la terminal
txcript crop <session-id>                # editar o recortar el historial en una copia nueva
txcript export <session-id> --out run.json
```

Copia `run.json` a otra máquina y continúa con `txcript continue ./run.json --with claude_code`. Transfiere los archivos del proyecto por separado.

Ejecuta `txcript mcp` para que un cliente MCP pueda listar, buscar y leer sesiones anteriores. Sus herramientas son de solo lectura. Consulta la [referencia de la CLI](../usage.md#cli) para conocer los filtros, los rangos de mensajes y la integración con el shell.

<a name="use-the-library"></a>

## Usar la biblioteca

### Rust

```sh
cargo add txcript
```

Convierte una transcripción de Claude Code al formato nativo de Codex:

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

Usa las implementaciones de `Store` para descubrir, cargar y guardar sesiones en el almacenamiento de cada agente. Convierte a `Transcript<Common>` para buscar, recortar o mostrar conversaciones con la misma API en todos los agentes. Consulta la [API de Rust](https://docs.rs/txcript) y los [ejemplos](../usage.md#rust-crate).

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

El paquete incluye WebAssembly precompilado para Node y Bun. Convierte transcripciones y busca en ellas en memoria; tu aplicación se encarga de los archivos y el almacenamiento. Consulta la [referencia de JavaScript](../usage.md#npm-package).

<a name="supported-agents"></a>

## Agentes compatibles

Cada nombre enlaza a la documentación de su formato. Usa el ID con `--from` y `--with`.

| Agente | ID | Leer desde | Continuar en |
|---|---|:---:|:---:|
| [Claude Code](../formats/claude-code.md) | `claude_code` | Sí | Sí |
| [Codex](../formats/codex.md) | `codex` | Sí | Sí |
| [OpenCode](../formats/opencode.md) | `opencode` | Sí | Sí |
| [Cursor CLI](../formats/cursor.md) | `cursor` | Sí | Sí |
| [Cursor desktop](../formats/cursor-desktop.md) | `cursor_desktop` | Sí | Sí |
| [pi](../formats/pi.md) | `pi` | Sí | Sí |
| [Campfire](../formats/campfire.md) | `campfire` | Sí | Sí |
| [Cowork](../formats/cowork.md) | `cowork` | Sí | Sí |
| [Grok CLI](../formats/grok.md) | `grok` | Sí | Sí |
| [Grok Bot](../formats/grok-bot.md) | `grok_bot` | Sí | Mediante la pasarela local |
| [fx](../formats/fx.md) | `fx` | Sí | Sí |
| [Antigravity](../formats/antigravity.md) | `antigravity` | Sí | Sí |
| [uji](../formats/uji.md) | `uji` | Sí | Sí |
| [Hermes Agent](../formats/hermes.md) | `hermes` | Sí | No |
| [Amp](../formats/amp.md) | `amp` | Sí | No |
| [Claude Chat](../formats/claude-chat.md) | `claude_chat` | Cuenta en línea | No |
| [ChatGPT](../formats/chatgpt.md) | `chatgpt` | Cuenta en línea | No |

La búsqueda de sesiones locales omite las cuentas en línea. Selecciona explícitamente `--from claude_chat` o `--from chatgpt` para leerlas. Estas fuentes usan API web privadas y reutilizan el inicio de sesión de una aplicación existente; sus requisitos y limitaciones están en los documentos enlazados.

### Añadir otro agente

Un agente sin adaptador nativo puede generar [Simple](../formats/simple.md), el formato JSON de intercambio de txcript. Guarda un documento como este en `run.json`:

```json
{
  "messages": [
    { "role": "user", "content": "Averigua por qué fallan las pruebas." },
    { "role": "assistant", "content": "El reloj de las pruebas usa la hora local." }
  ]
}
```

```sh
txcript continue ./run.json --with claude_code
```

Simple también representa razonamiento, llamadas a herramientas, resultados, imágenes y metadatos. Es el formato que genera `txcript export`.

## Qué se conserva

El modelo común representa mensajes, razonamiento, llamadas a herramientas y sus resultados, imágenes, metadatos y consumo de tokens. Lo que se conserva depende de lo que registre el origen y de lo que pueda representar el destino. Los registros propios de un agente y los campos no compatibles pueden perderse.

La conversión transfiere el historial de la conversación. El destino aporta sus propias instrucciones del sistema y herramientas, y los archivos del proyecto deben estar disponibles por separado. La carga y el guardado nativos tienen garantías de conservación distintas a las de la conversión; consulta las [limitaciones de cada formato](../formats/README.md).

<a name="documentation"></a>

## Documentación

- [Referencia de la CLI](../usage.md#cli): comandos, búsqueda, recorte, MCP e integración con el shell.
- [API de Rust](https://docs.rs/txcript) y [referencia de JavaScript](../usage.md#npm-package).
- [Formatos de transcripción](../formats/README.md): estructura del almacenamiento, correspondencias y limitaciones, con fuentes y notas de ingeniería inversa.
- [Desarrollo](../usage.md#development) y [guía de pruebas](../../tests/README.md).
- [Contribuir](../../CONTRIBUTING.md) · [Informar de una vulnerabilidad](../../SECURITY.md).
- [Historial de cambios](../../CHANGELOG.md) · [Informar de un problema](https://github.com/skillsynchq/txcript/issues).

## Licencia

[Apache-2.0](../../LICENSE)
