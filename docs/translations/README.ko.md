<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">다른 코딩 에이전트에서 대화를 이어 가세요.</p>

<p align="center">
  <a href="../../README.md">English</a> | <a href="README.ja.md">日本語</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.zh-TW.md">繁體中文</a> | 한국어 | <a href="README.de.md">Deutsch</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (Brasil)</a> | <a href="README.ru.md">Русский</a> | <a href="README.mr.md">मराठी</a> | <a href="README.ta.md">தமிழ்</a>
</p>

<p align="center">
  <a href="https://crates.io/crates/txcript"><img src="https://img.shields.io/crates/v/txcript?logo=rust&color=4c71f2" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/txcript"><img src="https://img.shields.io/npm/v/txcript?logo=npm&color=4c71f2" alt="npm"></a>
  <a href="https://docs.rs/txcript"><img src="https://img.shields.io/docsrs/txcript?logo=docsdotrs" alt="docs.rs"></a>
  <a href="https://github.com/skillsynchq/txcript/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/skillsynchq/txcript/ci.yml?branch=main&logo=github&label=ci" alt="CI"></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-555" alt="라이선스"></a>
</p>

txcript는 에이전트 세션을 변환하는 라이브러리입니다. Claude Code에서 시작한 대화를 Codex에서 이어 갈 수 있습니다. 메시지, 추론, 도구 사용 기록은 대상 에이전트가 지원하는 범위에서 전달됩니다.

하나의 대화 기록 모델로 세션 검색, 뷰어, 편집기를 만들 수 있습니다. txcript가 에이전트별 형식을 처리하며 Rust API, JavaScript 패키지, CLI를 제공합니다.

[CLI 사용해 보기](#try-the-cli) · [라이브러리 사용하기](#use-the-library) · [지원 에이전트](#supported-agents) · [문서](#documentation)

<p align="center">
  <img src="../assets/demo.gif" alt="txcript로 OpenCode 세션을 Claude Code에서 이어 가는 모습" width="680">
</p>

<a name="try-the-cli"></a>

## CLI 사용해 보기

[릴리스](https://github.com/skillsynchq/txcript/releases)에서 macOS, Linux, Windows용 바이너리를 내려받거나, Rust 1.96 이상으로 소스에서 설치하세요.

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli --locked
```

Claude Code 세션을 찾아 Codex에서 이어 가세요.

```sh
txcript list --from claude_code
txcript continue <session-id> --with codex
```

목록에 있는 ID를 사용하세요. 세션을 유일하게 식별할 수 있는 접두사도 가능합니다. txcript는 대상의 기본 형식으로 새 세션을 저장하고 기록된 작업 디렉터리에서 Codex를 실행합니다. 원본 세션은 유지됩니다. 진행하기 전에 대상 에이전트를 설치하고 로그인하세요.

세션을 다루는 다른 방법:

```sh
txcript query "relay bug"                # 로컬 세션 기록 검색
txcript view <session-id>                # 터미널에서 대화 읽기
txcript crop <session-id>                # 기록을 편집하거나 잘라 새 복사본으로 저장
txcript export <session-id> --out run.json
```

`run.json`을 다른 컴퓨터로 옮긴 뒤 `txcript continue ./run.json --with claude_code`로 이어 갈 수 있습니다. 프로젝트 파일은 별도로 옮기세요.

`txcript mcp`를 실행하면 MCP 클라이언트가 이전 세션의 목록을 보고, 검색하고, 읽을 수 있습니다. 도구는 읽기 전용입니다. 필터, 메시지 범위, 셸 연동은 [CLI 참조](../usage.md#cli)를 확인하세요.

<a name="use-the-library"></a>

## 라이브러리 사용하기

### Rust

```sh
cargo add txcript
```

Claude Code 대화 기록을 Codex의 기본 형식으로 변환합니다.

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

`Store` 구현을 사용하면 각 에이전트의 저장소에서 세션을 찾고, 불러오고, 저장할 수 있습니다. `Transcript<Common>`으로 변환하면 에이전트에 관계없이 같은 API로 대화를 검색하고, 자르고, 표시할 수 있습니다. [Rust API](https://docs.rs/txcript)와 [예제](../usage.md#rust-crate)를 확인하세요.

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

패키지에는 Node와 Bun용으로 미리 빌드된 WebAssembly가 포함되어 있습니다. 메모리에서 세션 텍스트를 변환하고 검색하며, 파일과 저장소는 애플리케이션이 관리합니다. [JavaScript 참조](../usage.md#npm-package)를 확인하세요.

<a name="supported-agents"></a>

## 지원 에이전트

각 이름은 해당 형식의 문서로 연결됩니다. ID를 `--from`과 `--with`에 사용하세요.

| 에이전트 | ID | 읽기 지원 | 이어서 진행 |
|---|---|:---:|:---:|
| [Claude Code](../formats/claude-code.md) | `claude_code` | 지원 | 지원 |
| [Codex](../formats/codex.md) | `codex` | 지원 | 지원 |
| [OpenCode](../formats/opencode.md) | `opencode` | 지원 | 지원 |
| [Cursor CLI](../formats/cursor.md) | `cursor` | 지원 | 지원 |
| [Cursor desktop](../formats/cursor-desktop.md) | `cursor_desktop` | 지원 | 지원 |
| [pi](../formats/pi.md) | `pi` | 지원 | 지원 |
| [Campfire](../formats/campfire.md) | `campfire` | 지원 | 지원 |
| [Cowork](../formats/cowork.md) | `cowork` | 지원 | 지원 |
| [Grok CLI](../formats/grok.md) | `grok` | 지원 | 지원 |
| [Grok Bot](../formats/grok-bot.md) | `grok_bot` | 지원 | 로컬 게이트웨이 사용 |
| [fx](../formats/fx.md) | `fx` | 지원 | 지원 |
| [Antigravity](../formats/antigravity.md) | `antigravity` | 지원 | 지원 |
| [uji](../formats/uji.md) | `uji` | 지원 | 지원 |
| [Hermes Agent](../formats/hermes.md) | `hermes` | 지원 | 미지원 |
| [Amp](../formats/amp.md) | `amp` | 지원 | 미지원 |
| [Claude Chat](../formats/claude-chat.md) | `claude_chat` | 온라인 계정 | 미지원 |
| [ChatGPT](../formats/chatgpt.md) | `chatgpt` | 온라인 계정 | 미지원 |

로컬 세션 탐색은 온라인 계정에 접근하지 않습니다. 온라인 계정을 읽으려면 `--from claude_chat` 또는 `--from chatgpt`를 명시적으로 선택하세요. 이 소스들은 비공개 웹 API와 기존 앱의 로그인 정보를 사용합니다. 요구 사항과 제한은 링크된 문서에 있습니다.

### 다른 에이전트 연결하기

전용 어댑터가 없는 에이전트도 txcript의 JSON 교환 형식인 [Simple](../formats/simple.md)을 출력할 수 있습니다. 다음과 같은 문서를 `run.json`으로 저장하세요.

```json
{
  "messages": [
    { "role": "user", "content": "테스트가 실패하는 이유를 찾아 줘." },
    { "role": "assistant", "content": "테스트 시계가 현지 시간을 사용하고 있습니다." }
  ]
}
```

```sh
txcript continue ./run.json --with claude_code
```

Simple은 추론, 도구 호출, 결과, 이미지, 메타데이터도 표현합니다. `txcript export`가 출력하는 형식도 Simple입니다.

## 유지되는 내용

공통 모델은 메시지, 추론, 도구 호출과 결과, 이미지, 메타데이터, 토큰 사용량을 표현합니다. 변환 후 유지되는 내용은 원본이 기록한 정보와 대상이 표현할 수 있는 정보에 따라 달라집니다. 에이전트 고유 레코드와 지원하지 않는 필드는 사라질 수 있습니다.

변환은 대화 기록을 전달합니다. 대상 에이전트는 자체 시스템 지침과 도구를 제공하며, 프로젝트 파일은 별도로 준비해야 합니다. 기본 형식의 불러오기·저장과 형식 간 변환은 보존을 보장하는 범위가 다릅니다. [각 형식의 주의 사항](../formats/README.md)을 확인하세요.

<a name="documentation"></a>

## 문서

- [CLI 참조](../usage.md#cli): 명령, 검색, 자르기, MCP, 셸 연동.
- [Rust API](https://docs.rs/txcript)와 [JavaScript 참조](../usage.md#npm-package).
- [대화 기록 형식](../formats/README.md): 저장 구조, 변환 대응 관계, 제한, 출처, 리버스 엔지니어링 기록.
- [개발](../usage.md#development) 및 [테스트 가이드](../../tests/README.md).
- [기여 안내](../../CONTRIBUTING.md) · [보안 취약점 신고](../../SECURITY.md).
- [변경 기록](../../CHANGELOG.md) · [문제 보고](https://github.com/skillsynchq/txcript/issues).

## 라이선스

[Apache-2.0](../../LICENSE)
