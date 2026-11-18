<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">உங்கள் உரையாடலை வேறொரு நிரலாக்க ஏஜென்டில் தொடருங்கள்.</p>

<p align="center">
  <a href="../../README.md">English</a> | <a href="README.ja.md">日本語</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.zh-TW.md">繁體中文</a> | <a href="README.ko.md">한국어</a> | <a href="README.de.md">Deutsch</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (Brasil)</a> | <a href="README.ru.md">Русский</a> | <a href="README.mr.md">मराठी</a> | தமிழ்
</p>

<p align="center">
  <a href="https://crates.io/crates/txcript"><img src="https://img.shields.io/crates/v/txcript?logo=rust&color=4c71f2" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/txcript"><img src="https://img.shields.io/npm/v/txcript?logo=npm&color=4c71f2" alt="npm"></a>
  <a href="https://docs.rs/txcript"><img src="https://img.shields.io/docsrs/txcript?logo=docsdotrs" alt="docs.rs"></a>
  <a href="https://github.com/skillsynchq/txcript/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/skillsynchq/txcript/ci.yml?branch=main&logo=github&label=ci" alt="CI"></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-555" alt="உரிமம்"></a>
</p>

txcript என்பது ஏஜென்ட் அமர்வுகளை மாற்றும் நூலகம். Claude Code-இல் உரையாடலைத் தொடங்கி Codex-இல் தொடரலாம். செய்திகள், பகுத்தறிவு மற்றும் கருவிப் பயன்பாட்டு வரலாறு ஆகியவை இலக்கு ஏஜென்ட் ஆதரிக்கும் அளவுக்குக் கொண்டு செல்லப்படும்.

ஒரே உரையாடல் பதிவு மாதிரியைப் பயன்படுத்தி அமர்வுகளைத் தேடும், பார்க்கும், திருத்தும் கருவிகளை உருவாக்கலாம். ஒவ்வொரு ஏஜென்டிற்குமான வடிவங்களை txcript கையாளுகிறது; Rust API, JavaScript தொகுப்பு மற்றும் CLI ஆகியவற்றை வழங்குகிறது.

[CLI-ஐ முயற்சிக்கவும்](#try-the-cli) · [நூலகத்தைப் பயன்படுத்தவும்](#use-the-library) · [ஆதரிக்கப்படும் ஏஜென்ட்கள்](#supported-agents) · [ஆவணங்கள்](#documentation)

<p align="center">
  <img src="../assets/demo.gif" alt="txcript மூலம் OpenCode அமர்வை Claude Code-இல் தொடருதல்" width="680">
</p>

<a name="try-the-cli"></a>

## CLI-ஐ முயற்சிக்கவும்

macOS, Linux அல்லது Windows-க்கான இயங்கும் கோப்பை [வெளியீடுகள் பக்கத்திலிருந்து](https://github.com/skillsynchq/txcript/releases) பதிவிறக்கவும். அல்லது Rust 1.96 அல்லது அதற்குப் பிந்தைய பதிப்பைப் பயன்படுத்தி மூலக் குறியீட்டிலிருந்து நிறுவவும்:

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli --locked
```

Claude Code அமர்வைக் கண்டறிந்து Codex-இல் தொடருங்கள்:

```sh
txcript list --from claude_code
txcript continue <session-id> --with codex
```

பட்டியலிலுள்ள ID-ஐப் பயன்படுத்துங்கள்; அமர்வைத் தனித்துவமாக அடையாளம் காணும் ID-இன் தொடக்கப் பகுதியும் போதும். txcript இலக்கு ஏஜென்டின் சொந்த வடிவத்தில் புதிய அமர்வை எழுதி, பதிவுசெய்யப்பட்ட பணிக் கோப்பகத்தில் Codex-ஐத் தொடங்கும். மூல அமர்வு தக்கவைக்கப்படும். தொடர்வதற்கு முன் இலக்கு ஏஜென்டை நிறுவி உள்நுழையுங்கள்.

அமர்வுகளுடன் வேலை செய்வதற்கான மற்ற வழிகள்:

```sh
txcript query "relay bug"                # உள்ளூர் அமர்வு வரலாற்றில் தேடவும்
txcript view <session-id>                # முனையத்தில் உரையாடலைப் படிக்கவும்
txcript crop <session-id>                # வரலாற்றைத் திருத்தி அல்லது வெட்டி புதிய நகலாக்கவும்
txcript export <session-id> --out run.json
```

`run.json`-ஐ வேறொரு கணினிக்கு மாற்றி, `txcript continue ./run.json --with claude_code` மூலம் தொடருங்கள். திட்டக் கோப்புகளைத் தனியாக மாற்றுங்கள்.

`txcript mcp`-ஐ இயக்கினால் MCP கிளையண்ட் முந்தைய அமர்வுகளைப் பட்டியலிடவும், தேடவும், படிக்கவும் முடியும். இந்தக் கருவிகள் படிப்பதற்கு மட்டுமே பயன்படும். வடிகட்டிகள், செய்தி வரம்புகள் மற்றும் ஷெல் ஒருங்கிணைப்புக்கு [CLI கையேட்டைப்](../usage.md#cli) பார்க்கவும்.

<a name="use-the-library"></a>

## நூலகத்தைப் பயன்படுத்தவும்

### Rust

```sh
cargo add txcript
```

Claude Code உரையாடல் பதிவை Codex-இன் சொந்த வடிவத்திற்கு மாற்றுங்கள்:

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

ஒவ்வொரு ஏஜென்டின் சேமிப்பகத்திலும் அமர்வுகளைக் கண்டறிய, ஏற்ற, சேமிக்க `Store` செயலாக்கங்களைப் பயன்படுத்துங்கள். `Transcript<Common>`-க்கு மாற்றிய பிறகு, வெவ்வேறு ஏஜென்ட்களின் உரையாடல்களை ஒரே API மூலம் தேடலாம், வெட்டலாம் அல்லது காட்டலாம். [Rust API](https://docs.rs/txcript) மற்றும் [எடுத்துக்காட்டுகளைப்](../usage.md#rust-crate) பார்க்கவும்.

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

இந்தத் தொகுப்பில் Node மற்றும் Bun-க்காக முன்கூட்டியே உருவாக்கப்பட்ட WebAssembly உள்ளது. இது நினைவகத்தில் அமர்வு உரையை மாற்றவும் தேடவும் செய்கிறது; கோப்புகளையும் சேமிப்பையும் உங்கள் பயன்பாடு கையாளும். [JavaScript கையேட்டைப்](../usage.md#npm-package) பார்க்கவும்.

<a name="supported-agents"></a>

## ஆதரிக்கப்படும் ஏஜென்ட்கள்

ஒவ்வொரு பெயரும் அதன் வடிவ ஆவணத்திற்கான இணைப்பு. ID-ஐ `--from` மற்றும் `--with` உடன் பயன்படுத்துங்கள்.

| ஏஜென்ட் | ID | இதிலிருந்து படிக்கலாம் | இதில் தொடரலாம் |
|---|---|:---:|:---:|
| [Claude Code](../formats/claude-code.md) | `claude_code` | ஆம் | ஆம் |
| [Codex](../formats/codex.md) | `codex` | ஆம் | ஆம் |
| [OpenCode](../formats/opencode.md) | `opencode` | ஆம் | ஆம் |
| [Cursor CLI](../formats/cursor.md) | `cursor` | ஆம் | ஆம் |
| [Cursor desktop](../formats/cursor-desktop.md) | `cursor_desktop` | ஆம் | ஆம் |
| [pi](../formats/pi.md) | `pi` | ஆம் | ஆம் |
| [Campfire](../formats/campfire.md) | `campfire` | ஆம் | ஆம் |
| [Cowork](../formats/cowork.md) | `cowork` | ஆம் | ஆம் |
| [Grok CLI](../formats/grok.md) | `grok` | ஆம் | ஆம் |
| [Grok Bot](../formats/grok-bot.md) | `grok_bot` | ஆம் | உள்ளூர் நுழைவாயில் வழியாக |
| [fx](../formats/fx.md) | `fx` | ஆம் | ஆம் |
| [Antigravity](../formats/antigravity.md) | `antigravity` | ஆம் | ஆம் |
| [uji](../formats/uji.md) | `uji` | ஆம் | ஆம் |
| [Hermes Agent](../formats/hermes.md) | `hermes` | ஆம் | இல்லை |
| [Amp](../formats/amp.md) | `amp` | ஆம் | இல்லை |
| [Claude Chat](../formats/claude-chat.md) | `claude_chat` | இணையக் கணக்கு | இல்லை |
| [ChatGPT](../formats/chatgpt.md) | `chatgpt` | இணையக் கணக்கு | இல்லை |

உள்ளூர் அமர்வுகளைக் கண்டறியும்போது இணையக் கணக்குகள் அணுகப்படாது. அவற்றைப் படிக்க `--from claude_chat` அல்லது `--from chatgpt`-ஐ வெளிப்படையாகக் குறிப்பிடுங்கள். இந்த மூலங்கள் தனிப்பட்ட வலை API-களையும் ஏற்கெனவே உள்ள பயன்பாட்டின் உள்நுழைவையும் பயன்படுத்துகின்றன. தேவைகளும் வரம்புகளும் இணைக்கப்பட்ட ஆவணங்களில் உள்ளன.

### மற்றொரு ஏஜென்டை இணைக்கவும்

தனிப்பட்ட அடாப்டர் இல்லாத ஏஜென்டும் txcript-இன் JSON பரிமாற்ற வடிவமான [Simple](../formats/simple.md)-ஐ வெளியிடலாம். கீழே உள்ளதைப் போன்ற ஆவணத்தை `run.json` எனச் சேமிக்கவும்:

```json
{
  "messages": [
    { "role": "user", "content": "சோதனைகள் ஏன் தோல்வியடைகின்றன என்று கண்டறி." },
    { "role": "assistant", "content": "சோதனைக் கடிகாரம் உள்ளூர் நேரத்தைப் பயன்படுத்துகிறது." }
  ]
}
```

```sh
txcript continue ./run.json --with claude_code
```

Simple பகுத்தறிவு, கருவி அழைப்புகள், முடிவுகள், படங்கள் மற்றும் மெட்டாடேட்டாவையும் குறிப்பிட முடியும். `txcript export` வெளியிடுவதும் இந்த வடிவத்தில்தான்.

## எவை தக்கவைக்கப்படும்

பொது மாதிரி செய்திகள், பகுத்தறிவு, கருவி அழைப்புகள் மற்றும் அவற்றின் முடிவுகள், படங்கள், மெட்டாடேட்டா, டோக்கன் பயன்பாடு ஆகியவற்றைக் குறிக்கிறது. மாற்றிய பிறகு எவை தக்கவைக்கப்படும் என்பது மூலம் எதைப் பதிவு செய்கிறது, இலக்கால் எதைப் பிரதிநிதித்துவப்படுத்த முடியும் என்பதைப் பொறுத்தது. ஏஜென்டிற்கே உரிய பதிவுகளும் ஆதரிக்கப்படாத புலங்களும் இழக்கப்படலாம்.

மாற்றம் உரையாடல் வரலாற்றைக் கொண்டு செல்கிறது. இலக்கு ஏஜென்ட் அதன் சொந்த சிஸ்டம் வழிமுறைகளையும் கருவிகளையும் வழங்குகிறது; திட்டக் கோப்புகள் தனியாகக் கிடைக்க வேண்டும். சொந்த வடிவத்தில் ஏற்றுதல் மற்றும் சேமித்தல், வடிவங்களுக்கு இடையிலான மாற்றம் ஆகியவற்றில் தகவல் பாதுகாப்பதற்கான உத்தரவாதங்கள் வேறுபடுகின்றன. [ஒவ்வொரு வடிவத்தின் வரம்புகளையும்](../formats/README.md) பார்க்கவும்.

<a name="documentation"></a>

## ஆவணங்கள்

- [CLI கையேடு](../usage.md#cli): கட்டளைகள், தேடல், வெட்டுதல், MCP மற்றும் ஷெல் ஒருங்கிணைப்பு.
- [Rust API](https://docs.rs/txcript) மற்றும் [JavaScript கையேடு](../usage.md#npm-package).
- [உரையாடல் பதிவு வடிவங்கள்](../formats/README.md): சேமிப்பக அமைப்பு, வடிவங்களுக்கு இடையிலான பொருத்தங்கள், வரம்புகள், ஆதாரங்கள் மற்றும் பின்னோக்குப் பொறியியல் குறிப்புகள்.
- [உருவாக்கம்](../usage.md#development) மற்றும் [சோதனை வழிகாட்டி](../../tests/README.md).
- [பங்களிப்பு வழிகாட்டி](../../CONTRIBUTING.md) · [பாதுகாப்புக் குறைபாட்டைப் புகாரளிக்கவும்](../../SECURITY.md).
- [மாற்றங்களின் பதிவு](../../CHANGELOG.md) · [சிக்கலைப் புகாரளிக்கவும்](https://github.com/skillsynchq/txcript/issues).

## உரிமம்

[Apache-2.0](../../LICENSE)
