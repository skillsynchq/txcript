<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">तुमचा संवाद दुसऱ्या कोडिंग एजंटमध्ये पुढे सुरू ठेवा.</p>

<p align="center">
  <a href="../../README.md">English</a> | <a href="README.ja.md">日本語</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.zh-TW.md">繁體中文</a> | <a href="README.ko.md">한국어</a> | <a href="README.de.md">Deutsch</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (Brasil)</a> | <a href="README.ru.md">Русский</a> | मराठी | <a href="README.ta.md">தமிழ்</a>
</p>

<p align="center">
  <a href="https://crates.io/crates/txcript"><img src="https://img.shields.io/crates/v/txcript?logo=rust&color=4c71f2" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/txcript"><img src="https://img.shields.io/npm/v/txcript?logo=npm&color=4c71f2" alt="npm"></a>
  <a href="https://docs.rs/txcript"><img src="https://img.shields.io/docsrs/txcript?logo=docsdotrs" alt="docs.rs"></a>
  <a href="https://github.com/skillsynchq/txcript/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/skillsynchq/txcript/ci.yml?branch=main&logo=github&label=ci" alt="CI"></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-555" alt="परवाना"></a>
</p>

txcript ही एजंटची सेशन रूपांतरित करण्यासाठीची लायब्ररी आहे. Claude Code मध्ये संवाद सुरू करा आणि Codex मध्ये पुढे चालू ठेवा. संदेश, तर्कविचार आणि साधनांचा इतिहास गंतव्य एजंटच्या समर्थनानुसार हस्तांतरित होतो.

एकाच संवाद मॉडेलवर सेशन शोधण्याची, पाहण्याची आणि संपादित करण्याची साधने तयार करा. txcript प्रत्येक एजंटचे वेगळे स्वरूप हाताळते आणि Rust API, JavaScript पॅकेज व CLI पुरवते.

[CLI वापरून पाहा](#try-the-cli) · [लायब्ररी वापरा](#use-the-library) · [समर्थित एजंट](#supported-agents) · [दस्तऐवज](#documentation)

<p align="center">
  <img src="../assets/demo.gif" alt="txcript वापरून OpenCode मधील सेशन Claude Code मध्ये पुढे सुरू करणे" width="680">
</p>

<a name="try-the-cli"></a>

## CLI वापरून पाहा

[रिलीज पृष्ठावरून](https://github.com/skillsynchq/txcript/releases) macOS, Linux किंवा Windows साठी तयार बायनरी डाउनलोड करा, किंवा Rust 1.96 किंवा त्यानंतरची आवृत्ती वापरून सोर्समधून इन्स्टॉल करा:

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli --locked
```

Claude Code मधील सेशन शोधा आणि Codex मध्ये पुढे सुरू करा:

```sh
txcript list --from claude_code
txcript continue <session-id> --with codex
```

यादीतील ID वापरा; सेशन निःसंदिग्धपणे ओळखणारा ID चा सुरुवातीचा भागही चालतो. txcript गंतव्य एजंटच्या मूळ स्वरूपात नवीन सेशन लिहिते आणि नोंदवलेल्या कार्यरत डिरेक्टरीमध्ये Codex सुरू करते. मूळ सेशन जतन होते. पुढे सुरू करण्याआधी गंतव्य एजंट इन्स्टॉल करून त्यात लॉग इन करा.

सेशनवर काम करण्याचे इतर मार्ग:

```sh
txcript query "relay bug"                # स्थानिक सेशन इतिहासात शोधा
txcript view <session-id>                # टर्मिनलमध्ये संवाद वाचा
txcript crop <session-id>                # इतिहास संपादित करा किंवा कापा आणि नवीन प्रत बनवा
txcript export <session-id> --out run.json
```

`run.json` दुसऱ्या संगणकावर न्या आणि `txcript continue ./run.json --with claude_code` वापरून संवाद पुढे सुरू करा. प्रकल्पाच्या फाइल्स स्वतंत्रपणे हलवा.

`txcript mcp` चालवल्यावर MCP क्लायंटला आधीची सेशन सूचीबद्ध करता येतात, त्यांत शोधता येते आणि ती वाचता येतात. ही साधने फक्त वाचनासाठी आहेत. फिल्टर, संदेशांच्या श्रेणी आणि शेलशी जोडणीसाठी [CLI संदर्भ](../usage.md#cli) पाहा.

<a name="use-the-library"></a>

## लायब्ररी वापरा

### Rust

```sh
cargo add txcript
```

Claude Code चा संवाद रेकॉर्ड Codex च्या मूळ स्वरूपात रूपांतरित करा:

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

एजंटच्या स्वतःच्या साठवणीत सेशन शोधण्यासाठी, लोड करण्यासाठी आणि जतन करण्यासाठी `Store` ची अंमलबजावणी वापरा. `Transcript<Common>` मध्ये रूपांतर केल्यावर वेगवेगळ्या एजंटचे संवाद एकाच API ने शोधता, कापता किंवा दाखवता येतात. [Rust API](https://docs.rs/txcript) आणि [उदाहरणे](../usage.md#rust-crate) पाहा.

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

या पॅकेजमध्ये Node आणि Bun साठी आधीच बिल्ड केलेले WebAssembly आहे. ते मेमरीमध्ये सेशनचा मजकूर रूपांतरित करते आणि त्यात शोध घेते; फाइल्स व साठवण तुमचा ॲप हाताळतो. [JavaScript संदर्भ](../usage.md#npm-package) पाहा.

<a name="supported-agents"></a>

## समर्थित एजंट

प्रत्येक नाव संबंधित स्वरूपाच्या दस्तऐवजाकडे नेते. ID हा `--from` आणि `--with` सोबत वापरा.

| एजंट | ID | येथून वाचता येते | येथे पुढे सुरू करता येते |
|---|---|:---:|:---:|
| [Claude Code](../formats/claude-code.md) | `claude_code` | होय | होय |
| [Codex](../formats/codex.md) | `codex` | होय | होय |
| [OpenCode](../formats/opencode.md) | `opencode` | होय | होय |
| [Cursor CLI](../formats/cursor.md) | `cursor` | होय | होय |
| [Cursor desktop](../formats/cursor-desktop.md) | `cursor_desktop` | होय | होय |
| [pi](../formats/pi.md) | `pi` | होय | होय |
| [Campfire](../formats/campfire.md) | `campfire` | होय | होय |
| [Cowork](../formats/cowork.md) | `cowork` | होय | होय |
| [Grok CLI](../formats/grok.md) | `grok` | होय | होय |
| [Grok Bot](../formats/grok-bot.md) | `grok_bot` | होय | स्थानिक गेटवेद्वारे |
| [fx](../formats/fx.md) | `fx` | होय | होय |
| [Antigravity](../formats/antigravity.md) | `antigravity` | होय | होय |
| [uji](../formats/uji.md) | `uji` | होय | होय |
| [Hermes Agent](../formats/hermes.md) | `hermes` | होय | नाही |
| [Amp](../formats/amp.md) | `amp` | होय | नाही |
| [Claude Chat](../formats/claude-chat.md) | `claude_chat` | ऑनलाइन खाते | नाही |
| [ChatGPT](../formats/chatgpt.md) | `chatgpt` | ऑनलाइन खाते | नाही |

स्थानिक सेशन शोधताना ऑनलाइन खात्यांना संपर्क केला जात नाही. ती वाचण्यासाठी `--from claude_chat` किंवा `--from chatgpt` स्पष्टपणे निवडा. हे स्रोत खाजगी वेब API आणि आधीपासून लॉग इन असलेल्या ॲपची माहिती वापरतात; आवश्यकता आणि मर्यादा संबंधित दस्तऐवजांत दिल्या आहेत.

### आणखी एखादा एजंट जोडा

स्वतःचा अडॅप्टर नसलेला एजंटही txcript चे JSON देवाणघेवाण स्वरूप [Simple](../formats/simple.md) देऊ शकतो. खालीलप्रमाणे दस्तऐवज `run.json` म्हणून जतन करा:

```json
{
  "messages": [
    { "role": "user", "content": "चाचण्या का अयशस्वी होत आहेत ते शोध." },
    { "role": "assistant", "content": "चाचण्यांमधील घड्याळ स्थानिक वेळ वापरत आहे." }
  ]
}
```

```sh
txcript continue ./run.json --with claude_code
```

Simple मध्ये तर्कविचार, साधनांना केलेले कॉल, परिणाम, प्रतिमा आणि मेटाडेटाही मांडता येतो. `txcript export` याच स्वरूपात आउटपुट देते.

## काय जतन होते

सामायिक मॉडेलमध्ये संदेश, तर्कविचार, साधनांना केलेले कॉल व त्यांचे परिणाम, प्रतिमा, मेटाडेटा आणि टोकन वापर मांडता येतो. रूपांतरानंतर काय टिकते हे स्रोत काय नोंदवतो आणि गंतव्य काय दर्शवू शकते यावर अवलंबून असते. एजंटपुरत्या खास नोंदी आणि समर्थित नसलेली फील्ड गमावली जाऊ शकतात.

रूपांतरात संवादाचा इतिहास हस्तांतरित होतो. गंतव्य एजंट स्वतःच्या सिस्टीम सूचना आणि साधने पुरवतो; प्रकल्पाच्या फाइल्स स्वतंत्रपणे उपलब्ध असाव्या लागतात. मूळ स्वरूपात लोड व सेव्ह करणे आणि स्वरूपांमधील रूपांतर यांत माहिती जतन होण्याच्या हमी वेगवेगळ्या आहेत. [प्रत्येक स्वरूपाच्या मर्यादा](../formats/README.md) पाहा.

<a name="documentation"></a>

## दस्तऐवज

- [CLI संदर्भ](../usage.md#cli): कमांड, शोध, कापणे, MCP आणि शेलशी जोडणी.
- [Rust API](https://docs.rs/txcript) आणि [JavaScript संदर्भ](../usage.md#npm-package).
- [संवाद रेकॉर्डची स्वरूपे](../formats/README.md): साठवणीची रचना, रूपांतरातील जुळवणी आणि मर्यादा, स्रोत व रिव्हर्स इंजिनिअरिंगच्या नोंदींसह.
- [विकास](../usage.md#development) आणि [चाचणी मार्गदर्शक](../../tests/README.md).
- [योगदान मार्गदर्शक](../../CONTRIBUTING.md) · [सुरक्षा त्रुटी कळवा](../../SECURITY.md).
- [बदलांचा इतिहास](../../CHANGELOG.md) · [समस्या नोंदवा](https://github.com/skillsynchq/txcript/issues).

## परवाना

[Apache-2.0](../../LICENSE)
