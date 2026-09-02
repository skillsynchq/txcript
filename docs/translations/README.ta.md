<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dark.svg">
    <img src="../assets/wordmark-light.svg" alt="txcript" width="600">
  </picture>
</p>

<p align="center">ஒரு ஹார்னெஸிலிருந்து இன்னொன்றுக்கு செஷன்களை நகர்த்த ஒரு லைப்ரரி</p>

<p align="center">
  <a href="../../README.md">English</a> | <a href="README.ja.md">日本語</a> | <a href="README.zh-CN.md">简体中文</a> | <a href="README.zh-TW.md">繁體中文</a> | <a href="README.ko.md">한국어</a> | <a href="README.de.md">Deutsch</a> | <a href="README.es.md">Español</a> | <a href="README.fr.md">Français</a> | <a href="README.it.md">Italiano</a> | <a href="README.pt-BR.md">Português (Brasil)</a> | <a href="README.ru.md">Русский</a> | <a href="README.mr.md">मराठी</a> | தமிழ்
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

Claude Code-இல் ஒரு செஷனைத் தொடங்குங்கள்; usage limit வந்தாலோ, வேலை நடுவில் நின்றாலோ, முழு உரையாடல், reasoning, டூல் ஹிஸ்டரி எல்லாம் அப்படியே இருக்க, அதே செஷனை Codex-இல் தொடருங்கள்:

<p align="center">
  <img src="../assets/demo.gif" alt="txcript continue: an OpenCode session resumed in Claude Code">
</p>

txcript ஒவ்வொரு ஹார்னெஸின் நேட்டிவ் டிரான்ஸ்கிரிப்ட் ஃபார்மட்டையும் ஒரு typed பொது மாடல் வழியாக மேப் செய்கிறது. நேட்டிவ் load/save பைட்-அளவில் லாஸ்லெஸ்; ஒரு ஹார்னெஸிலிருந்து இன்னொன்றுக்கு மாற்றும்போது மெசேஜ்கள், reasoning, டூல் கால்கள், டூல் ரிசல்ட்கள், படங்கள், மெட்டாடேட்டா, usage (கிடைக்கும் இடங்களில்) அனைத்தும் பாதுகாக்கப்படுகின்றன. இது [**CLI**](#cli), [**Rust crate**](#rust-crate), [**npm package**](#npm-package) என மூன்று வடிவங்களில் கிடைக்கிறது.

## சிறப்பம்சங்கள்

- **16 ஹார்னெஸ்கள், ஒரே மாடல்**: ஒவ்வொரு ஃபார்மட்டும் `Transcript<Common>` வழியாக மாறுகிறது; ஒரு ஹார்னெஸைச் சேர்த்தால் அது மற்ற எல்லாவற்றுடனும் தானாக இணைகிறது.
- **மற்ற எல்லோருக்கும் ஒரு ஃபார்மட்**: txcript இதுவரை கேள்விப்படாத ஏஜென்ட்களும் ஆவணப்படுத்தப்பட்ட [Simple](../formats/simple.md) interchange JSON-ஐ — ஒரு file ஆகவோ stream ஆகவோ, txcript-க்கு நேரடியாகக் கொடுக்கும் வகையில் — வெளியிட்டால், அவற்றின் டிரான்ஸ்கிரிப்ட்கள் ஆதரிக்கப்படும் எந்த ஹார்னெஸிலும் தொடரும்.
- **பைட்-லாஸ்லெஸ் ரவுண்ட்-டிரிப்**: ஒரு செஷனை அதன் சொந்த ஃபார்மட்டில் load செய்து save செய்தால், அது அப்படியே திரும்பக் கிடைக்கிறது.
- **எங்கும் தொடரலாம்**: `txcript continue <id> --with <harness>` செஷனை இன்னொரு ஹார்னெஸின் நேட்டிவ் ஃபார்மட்டில் மீண்டும் எழுதி அந்த ஹார்னெஸை launch செய்கிறது. மூல செஷன் ஒருபோதும் மாற்றப்படாது.
- **செஷன்களைப் படிக்கவும் எடுத்துச் செல்லவும்**: `txcript view` எந்த செஷனையும் உள்ளமைந்த pager-இல் திறக்கிறது; படங்களை வரையும் டெர்மினல்களில் படங்களும் சேர்ந்து காட்டப்படும். `txcript export` அதை ஒரு Simple document ஆக எழுதுகிறது; இன்னொரு மெஷினில் `continue` அதை எடுத்துக்கொள்ளும்.
- **எல்லாவற்றையும் தேடுங்கள்**: மெஷினிலுள்ள ஒவ்வொரு செஷனிலும் literal, case-insensitive தேடல் — லைப்ரரி API ஆகவோ, one-shot CLI query ஆகவோ, interactive picker ஆகவோ.
- **MCP சர்வர்**: `txcript mcp` read-only `list_sessions`, `search_sessions`, `read_session` டூல்களை வெளிப்படுத்துகிறது; ஏஜென்ட்கள் பழைய செஷன்களை context ஆகப் பயன்படுத்திக்கொள்ளலாம்.
- **ஆவணப்படுத்தப்பட்ட ஃபார்மட்கள்**: ஒவ்வொரு ஹார்னெஸின் on-disk ஃபார்மட்டும் [`docs/formats/`](../formats)-இல் விரிவாக எழுதப்பட்டுள்ளது; ஒவ்வொரு கூற்றுக்கும் ஆதாரம் (அதிகாரப்பூர்வ ஆவணங்கள், source permalinks அல்லது reverse-engineering குறிப்புகள்) இணைக்கப்பட்டுள்ளது.

## ஆதரிக்கப்படும் ஹார்னெஸ்கள்

```mermaid
flowchart LR
    claude["Claude Code"] <--> common(("Transcript&lt;Common&gt;"))
    claudechat["Claude Chat"] --> common
    chatgpt["ChatGPT"] --> common
    cowork["Cowork"] <--> common
    codex["Codex"] <--> common
    opencode["OpenCode"] <--> common
    pi["pi"] <--> common
    campfire["Campfire"] <--> common
    common <--> cursor["Cursor CLI"]
    common <--> cursordesktop["Cursor desktop"]
    common <--> grok["Grok CLI"]
    common <--> fx["fx"]
    common <--> antigravity["Antigravity"]
    simple["Simple (any agent)"] --> common
    hermes["Hermes Agent"] --> common
    amp["Amp"] --> common
```

டிஸ்கவரி, பட்டியலிடல், தேடல், `view` — backing store உள்ள எல்லா ஹார்னெஸ்களுக்கும் வேலை செய்கின்றன. CLI-க்கும் WASM API-க்கும் கொடுக்க வேண்டியவை இந்த `id` strings தான்.

| ஹார்னெஸ் | id | டிஸ்கில் செஷன்கள் | நேட்டிவ் ஃபார்மட் | மாற்றம் | தொடர | டாக் |
|---|---|---|---|:---:|:---:|---|
| [Claude Code](https://claude.com/claude-code) | `claude_code` | `~/.claude/projects/` | JSONL | ⇄ | ✓ | [ஸ்பெக்](../formats/claude-code.md) |
| [Claude Chat](https://claude.ai) | `claude_chat` | live `claude.ai` கணக்கு <sup>4</sup> | private web API | → | — <sup>4</sup> | [ஸ்பெக்](../formats/claude-chat.md) |
| [ChatGPT](https://chatgpt.com) | `chatgpt` | live `chatgpt.com` கணக்கு <sup>5</sup> | private web API | → | — <sup>5</sup> | [ஸ்பெக்](../formats/chatgpt.md) |
| [Cowork](https://claude.com/product/cowork) | `cowork` | `<Claude app data>/local-agent-mode-sessions/` | செஷன் ரெக்கார்டு + Claude Code JSONL | ⇄ | ✓ | [ஸ்பெக்](../formats/cowork.md) |
| [Codex](https://github.com/openai/codex) | `codex` | `~/.codex/sessions/` | rollout JSONL | ⇄ | ✓ | [ஸ்பெக்](../formats/codex.md) |
| [OpenCode](https://opencode.ai) | `opencode` | `~/.local/share/opencode/opencode.db` | SQLite | ⇄ | ✓ | [ஸ்பெக்](../formats/opencode.md) |
| [pi](https://pi.dev) | `pi` | `~/.pi/agent/sessions/` | JSONL | ⇄ | ✓ | [ஸ்பெக்](../formats/pi.md) |
| [Campfire](../formats/campfire.md) | `campfire` | `~/.campfire/agent/sessions/` | JSONL | ⇄ | ✓ | [ஸ்பெக்](../formats/campfire.md) |
| [Cursor CLI](https://cursor.com/cli) | `cursor` | `~/.cursor/chats/` | SQLite | ⇄ | ✓ | [ஸ்பெக்](../formats/cursor.md) |
| [Cursor desktop](https://cursor.com) | `cursor_desktop` | `<Cursor User dir>/globalStorage/` | SQLite | ⇄ | ✓ | [ஸ்பெக்](../formats/cursor-desktop.md) |
| [Grok CLI](https://github.com/xai-org/grok-build) | `grok` | `~/.grok/sessions/` | செஷன் டைரக்டரி (JSON) | ⇄ | ✓ | [ஸ்பெக்](../formats/grok.md) |
| [fx](https://fx.sh) | `fx` | `~/.fx/sessions/` | செஷன் டைரக்டரி (event-log) | ⇄ | ✓ | [ஸ்பெக்](../formats/fx.md) |
| Hermes Agent | `hermes` | `~/.hermes/state.db` | SQLite | → | — <sup>3</sup> | [ஸ்பெக்](../formats/hermes.md) |
| [Amp](https://ampcode.com) | `amp` | `~/.local/share/amp/threads/` | த்ரெட் JSON | → | — <sup>1</sup> | [ஸ்பெக்](../formats/amp.md) |
| [Antigravity](https://antigravity.google) | `antigravity` | `~/.gemini/antigravity-cli/` | SQLite | ⇄ | ✓ | [ஸ்பெக்](../formats/antigravity.md) |
| Simple | `simple` | — <sup>2</sup> | interchange JSON | → | — <sup>2</sup> | [ஸ்பெக்](../formats/simple.md) |

<sup>1</sup> Amp-இன் த்ரெட்கள் சர்வர் பக்கம் இருக்கின்றன; CLI-இல் import இல்லை: செஷன்களை Amp-*இலிருந்து* மாற்றலாம், ஆனால் Amp-க்குள் தொடர முடியாது.

<sup>2</sup> Simple என்பது txcript-இன் சொந்த interchange ஃபார்மட் — மேலே பட்டியலில் இல்லாத எந்த ஏஜென்ட்டுக்கும் இதுவே நுழைவு வழி. இதற்கு ஆப் இல்லை, நிர்வகிக்கப்படும் டைரக்டரியும் இல்லை: ஒரு Simple செஷன் என்பது `txcript continue`-க்கு நேரடியாகக் கொடுக்கப்படும் ஒரு document (ஒரு file, அல்லது stdin); தொடரப்பட்ட உரையாடல் அதன் பிறகு டார்கெட் ஹார்னெஸிலேயே இருக்கும்.

<sup>3</sup> Hermes-இன் `state.db` txcript-இல் read-only; Hermes-இல் செஷன் import கட்டளையும் இல்லை: செஷன்களை Hermes-*இலிருந்து* மாற்றலாம், ஆனால் Hermes-க்குள் தொடர முடியாது.

<sup>4</sup> Claude Chat ஒரு live, pull-only source. macOS-இல், `--from claude_chat`-ஐ வெளிப்படையாகத் தேர்ந்தால், sign-in ஆகியிருக்கும் Claude Desktop செஷனை அது தானாகவே மறுபயன்படுத்தும்; மொத்த discovery Claude Chat-ஐத் தொடர்பு கொள்ளாது. environment variable-கள் வழியாகக் கொடுக்கப்படும் credentials ஏற்கப்படாது. optional `TXCRIPT_CLAUDE_CHAT_ORGANIZATION_UUID` discovery-ஐ ஒரு organization-க்குள் கட்டுப்படுத்தும்; இல்லையென்றால் ஆப்பின் active organization பயன்படுத்தப்படும். Claude Chat-க்கு ஆதரிக்கப்படும் conversation API எதுவும் இல்லை: txcript ஒரு private endpoint-ஐப் படிக்கிறது, அதை Anthropic கண்காணிக்கவோ கட்டுப்படுத்தவோ முடியும்; discovery நேரடியாக call செய்யப்படும் இடங்களில் Rust crate build நேரத்தில் எச்சரிக்கும். txcript படிக்கிறது மட்டுமே: save, delete, அதே ஹார்னெஸில் continue, `--with claude_chat` — இவற்றை மறுக்கிறது. உரையாடலில் Claude உருவாக்கிய file-கள் சேர்ந்தே வரும்; Claude Code-இல் தொடர்ந்தால், அவை புதிய செஷனுக்கு அருகிலேயே எழுதப்பட்டு Claude Code artifact-களாகக் காட்டப்படும். Claude-இன் data-export ZIP மற்றும் `conversations.json` ஆதரிக்கப்படவில்லை.

<sup>5</sup> ChatGPT ஒரு live, pull-only source. Claude Chat Claude Desktop-ஐ மறுபயன்படுத்துவது போலவே, `--from chatgpt`-ஐ வெளிப்படையாகத் தேர்ந்தால், Codex `CODEX_HOME/auth.json` அல்லது `~/.codex/auth.json`-இல் நிர்வகிக்கும் ChatGPT login தானாகவே மறுபயன்படுத்தப்படும்; அந்தக் கணக்கு browser-இல் sign-in ஆகியிருக்கும் கணக்கிலிருந்து வேறுபடலாம். txcript அந்த credential file-ஐப் படிக்கிறது மட்டுமே; அதை refresh செய்யவோ மீண்டும் எழுதவோ செய்யாது. மொத்த discovery ChatGPT-ஐத் தொடர்பு கொள்ளாது; ஆனால் ஒரு சரியான conversation UUID-ஐ, கணக்கை முழுவதும் பட்டியலிடாமல், நேரடியாகப் படிக்கலாம். txcript படிக்கிறது மட்டுமே: save, delete, அதே ஹார்னெஸில் continue, `--with chatgpt` — இவற்றை மறுக்கிறது. ChatGPT-க்கு ஆதரிக்கப்படும் conversation API எதுவும் இல்லை; அதனால் இந்த அணுகல் மாறலாம் அல்லது கட்டுப்படுத்தப்படலாம். ChatGPT data-export archive-கள் ஆதரிக்கப்படவில்லை.

## நிறுவல்

**CLI** (`txcript` பைனரியை நிறுவுகிறது):

```sh
cargo install --git https://github.com/skillsynchq/txcript txcript-cli
# or from a checkout: cargo install --path cli
```

**Rust crate**:

```sh
cargo add txcript
```

**npm package** (முன்பே build செய்யப்பட்ட WASM; Rust toolchain தேவையில்லை):

```sh
bun add txcript     # or: npm install txcript
```

## CLI

லோக்கல் செஷன்களைக் கண்டுபிடித்து, எந்த ஹார்னெஸிலும் தொடருங்கள்:

```sh
txcript list                             # local sessions across every harness
    [--from <harness>]                    #   only this harness's sessions
    [--cwd <dir>]                         #   only sessions recorded under <dir>
    [-n <N>]                              #   at most N sessions
    [--since <when>] [--until <when>]     #   bound the session start time
txcript continue <id>[#range]            # continue <id>, then launch its harness
    [--with <harness>]                    #   ...continuing in <harness> instead
    [--from <harness>]                    #   scope the id lookup to one harness
    [--out <dir>]                         #   write under <dir>; implies --no-resume
    [--no-resume]                         #   write the session but don't launch
txcript continue <file|->[#range]        # continue a Simple document instead:
    --with <harness> [...]                #   a file, or stdin (`-`), from any agent
txcript view <id>[#range]                # view a session; compact text when piped
    [--from <harness>]                    #   scope the id lookup to one harness
    [--no-pager]                          #   print the terminal view directly
txcript export <id>[#range]              # write a session as a Simple document
    [--from <harness>]                    #   scope the id lookup to one harness
    [--out <file>]                        #   write to <file> instead of stdout
```

ஒரே ஒரு செஷனுக்குப் பொருந்தும் முழு id-இன் எந்த prefix-ும், அல்லது செஷனின் சரியான தலைப்பும் செஷன் id ஆகப் பயன்படும். `txcript resume` என்பது `continue`-இன் alias. `--since`-உம் `--until`-உம் RFC 3339 timestamp-களையோ வெறும் `YYYY-MM-DD` தேதிகளையோ எடுத்துக்கொள்ளும்.

`continue` டார்கெட் ஹார்னெஸ் தன் செஷன்களை வைக்கும் இடத்திலேயே செஷனை எழுதி, பிறகு அதன் மீது அந்த ஹார்னெஸை launch செய்து டெர்மினலை ஒப்படைக்கிறது:

- அதே ஹார்னெஸ்: மூல செஷன் அந்த இடத்திலேயே resume ஆகிறது.
- ஹார்னெஸ் மாற்றம் (`--with`): செஷன் டார்கெட்டின் நேட்டிவ் ஃபார்மட்டில் மீண்டும் எழுதப்படுகிறது. எழுதப்படுவது எப்போதும் ஒரு நகல்தான்; மூல செஷன் மாற்றப்படவோ நீக்கப்படவோ இல்லை.
- id-க்குப் பதிலாக ஒரு [Simple](../formats/simple.md) document — `txcript continue ./run.json --with claude_code`, அல்லது `my-agent | txcript continue - --with claude_code` — எந்த ஏஜென்ட்டின் டிரான்ஸ்கிரிப்டையும் இதே வழியில் கொண்டு வரும்; ஒரு document-க்கு சொந்த ஹார்னெஸ் இல்லாததால் `--with` கட்டாயம்.
- launch கட்டளை ஹார்னெஸுக்கு ஒன்று, override-உம் செய்யலாம்: `TRANSCRIPT_<HARNESS>_RESUME_CMD`-ஐ `{id}` டெம்ப்ளேட்டாக செட் செய்யுங்கள், எ.கா. `TRANSCRIPT_CODEX_RESUME_CMD="codex resume {id}"`.

டெர்மினலில் `view` உள்ளமைந்த pager-ஐத் திறக்கிறது: `u`, `a`, `t`, `r` யூசர் மெசேஜ்கள், அசிஸ்டன்ட் மெசேஜ்கள், டூல் கால்கள், reasoning — இவற்றை மறைக்கவோ காட்டவோ செய்கின்றன; `]`-உம் `[`-உம் ஒரு மெசேஜிலிருந்து அடுத்த மெசேஜுக்குத் தாவுகின்றன; `/` காட்டப்படுவதில் தேடுகிறது. படங்களைக் காட்டக்கூடிய டெர்மினல்களில் (Ghostty, kitty, WezTerm, Konsole) படங்கள் inline ஆக வரையப்படும். வெளி pager-ஐப் பயன்படுத்த `TXCRIPT_PAGER`-ஐ செட் செய்யுங்கள்; அல்லது view-ஐ நேரடியாக அச்சிட `--no-pager` கொடுங்கள். pipe அல்லது redirect செய்தால், MCP சர்வர் தரும் அதே சுருக்கமான டெக்ஸ்டை `view` அச்சிடுகிறது. எப்படியிருந்தாலும் ஒவ்வொரு மெசேஜுக்கும் `── #N ──` என்ற கோட்டில் எண் கிடைக்கிறது; `#range` அந்த அச்சிடப்பட்ட எண்களின்படி மெசேஜ்களைத் தேர்கிறது — 1-இல் தொடங்கி, இரு எல்லைகளும் உட்பட:

- `abc#7`: மெசேஜ் 7 மட்டும்
- `abc#5-12`: மெசேஜ் 5 முதல் 12 வரை
- `abc#5-`: மெசேஜ் 5 முதல் இறுதி வரை
- `abc#-10`: தொடக்கம் முதல் மெசேஜ் 10 வரை

`continue`-க்கும் இதே suffix பொருந்தும்; அந்த மெசேஜ்கள் மட்டும் புதிய செஷனாகத் தொடரும். ஒரு டூல் காலை அதன் ரிசல்ட்டிலிருந்து பிரிக்கும் range நிராகரிக்கப்படும்; error-இல் அருகிலுள்ள செல்லுபடியான range பரிந்துரைக்கப்படும்.

`export` செஷனை [Simple](../formats/simple.md) document ஆக, stdout-இல் அல்லது `--out <file>`-இல் எழுதுகிறது. இந்த document, canonical மாடலின் முழு rendering — `continue` ஒரு ஹார்னெஸிலிருந்து இன்னொரு ஹார்னெஸுக்கு கொண்டு போகும் எல்லாமே — ஆகும்; இது எந்த ஹார்னெஸும் தன் செஷன்களை வைக்கும் இடத்திலிருந்தும் தனியாக இருக்கும், அதனால் இது ஒரு மெஷினிலிருந்து இன்னொரு மெஷினுக்கு ஒரு file ஆக நகரும்:

```sh
txcript export 0dc114bf --out session.json       # on this machine
txcript continue ./session.json --with claude_code   # on the other one
```

இம்போர்ட் செய்யும் மெஷினில் பதிவு செய்யப்பட்ட working directory இருந்தால் அது அப்படியே வைக்கப்படும்; இல்லையென்றால் `continue` ரன் ஆகும் directory-ஆல் மாற்றப்படும். `export`-க்கும் `view` போலவே அதே `#range` suffix-உம் `--from` scope-உம் பொருந்தும்.

### தேடல்

```sh
txcript query 'relay bug'                # one-shot: ranked hits, highlighted
txcript query                            # interactive picker; Enter continues
    [--from <harness>]                   #   search only <harness> (default: all)
    [--with <harness>]                   #   continue the pick in <harness>
    [--cwd <dir>]                        #   only sessions recorded under <dir>
```

pattern literal ஆகவும் case-insensitive ஆகவும் match ஆகும்: `relay bug` அந்தச் சரியான டெக்ஸ்டை — இடைவெளிகள் உட்பட — கொண்ட வரிகளைக் கண்டுபிடிக்கும்.

picker-இல், டைப் செய்தால் filter ஆகிறது; அம்புக்குறிகள் அல்லது ctrl-p/n-ஆல் நகரலாம்; Enter தேர்ந்த செஷனை அதன் சொந்த ஹார்னெஸில் (அல்லது `--with`-இல் குறித்ததில்) தொடர்கிறது; Esc ரத்து செய்கிறது. எந்த வகை உள்ளடக்கத்தில் match கிடைத்தது — யூசர் டெக்ஸ்ட், அசிஸ்டன்ட் டெக்ஸ்ட், thinking, டூல் யூஸ், டூல் அவுட்புட், செஷன் மெட்டாடேட்டா — என்பதை ஒவ்வொரு வரியும் காட்டுகிறது.

cache இல்லாமல், ஒவ்வொரு ரன்னும் ஒவ்வொரு செஷனையும் மீண்டும் படிக்கிறது. அந்தப் path-இல் நிலையான search cache வைத்துக்கொள்ள `--cache <path>` கொடுங்கள் (அல்லது `TXCRIPT_CACHE`-ஐ செட் செய்யுங்கள்); அப்போது `query`-உம் MCP search டூலும் கடைசி ரன்னுக்குப் பிறகு மாறிய செஷன்களை மட்டுமே மீண்டும் படிக்கும். இந்த flag எல்லா subcommand-களுக்கும் பொருந்தும்.

### MCP சர்வர்

```sh
txcript mcp                              # stdio transport
```

மூன்று read-only டூல்களை வெளிப்படுத்துகிறது; அவற்றின் optional filter-கள் CLI-உடன் ஒத்தவை:

- `list_sessions(from?, cwd?, limit?, offset?)`
- `search_sessions(pattern, from?, cwd?)`
- `read_session(id, from?)`

<sub>\* `from`-ஐ விட்டுவிட்டால் எல்லா ஹார்னெஸ்களும் சேரும்; `cwd`-ஐ விட்டுவிட்டால் directory filter எதுவும் இல்லை. working directory பதிவாகாத செஷன்கள் `cwd` விடப்பட்டால் மட்டுமே match ஆகும்.</sub>

`list_sessions` `limit`-உம் `offset`-உம் கொண்டு பக்கம் பக்கமாகத் தருகிறது; பக்கமாகப் பிரிப்பதற்கு முன்பான மொத்த எண்ணிக்கையையும் தெரிவிக்கிறது. live Claude Chat, ChatGPT source-கள் ஒருபோதும் பட்டியலிடப்படாது. `read_session` `view` போலவே அதே `#range` suffix-ஐ எடுத்து, அதே சுருக்கமான டெக்ஸ்டைத் திருப்பித் தருகிறது; முழுவதுமாகத் திருப்பித் தர முடியாத அளவு பெரிய read நிராகரிக்கப்பட்டு, sub-range-கள் பரிந்துரைக்கப்படும். `--cache` சர்வருக்கும் பொருந்தும்.

### ஷெல் ஒருங்கிணைப்பு

```sh
eval "$(txcript init zsh)"                      # in ~/.zshrc; or: txcript init bash
```

`init` கம்ப்ளீஷன்களையும், தற்போதைய folder-இல் பதிவான செஷன்களுக்கு மட்டும் scope செய்யப்பட்ட picker-ஐத் திறக்கும் ctrl+shift+r binding-ஐயும் அச்சிடுகிறது. கம்ப்ளீஷன்கள் மட்டும் வேண்டுமெனில், `completion` bash, elvish, fish, powershell, zsh-க்கு வேலை செய்கிறது:

```sh
txcript completion zsh > ~/.zfunc/_txcript      # or wherever your fpath looks
source <(txcript completion bash)               # bash, ad hoc
txcript completion fish > ~/.config/fish/completions/txcript.fish
```

## Rust crate

```toml
[dependencies]
txcript = "0.12"
# Codecs only: drops the SQLite-backed stores, the live Claude Chat and
# ChatGPT sources, and search. Every codec stays available.
# txcript = { version = "0.12", default-features = false }
```

இயல்பு features: `opencode` (SQLite store-கள்: OpenCode, இரு Cursor-கள், Antigravity), `hermes`, `claude_chat`, `chatgpt`, `search`.

சிறியது முதல் பெரியது வரை மூன்று அடுக்குகள்:

- `Codec`: ஹார்னெஸுக்கு ஒரு `to_common` / `from_common`; `convert::<A, B>` அவற்றை canonical மாடல் வழியாகச் சங்கிலியாக இணைக்கிறது.
- `TextCodec`: `from_text` / `to_text` — ஹார்னெஸின் நேட்டிவ் செஷன் டெக்ஸ்டை parse செய்யவும் render செய்யவும், I/O எதுவும் இல்லாமல்.
- `Store`: உண்மையான backend-இல் (செஷன் டைரக்டரிகள், அல்லது OpenCode, Hermes, இரு Cursor-கள், Antigravity-க்கான SQLite DB-கள்) discover/load/save.

மெமரியிலேயே மாற்றம் (filesystem இல்லை):

```rust
use txcript::harness::{claude_code, codex};
use txcript::{Codec, TextCodec, convert};

let claude = claude_code::ClaudeCode::from_text(jsonl_text)?;          // Transcript<ClaudeCode>
let codex = convert::<claude_code::ClaudeCode, codex::Codex>(&claude)?; // Transcript<Codex>
let codex_text = codex::Codex::to_text(&codex)?;                       // native rollout JSONL
```

அல்லது `Store` வழியாக டிஸ்க் மூலம்:

```rust
use txcript::harness::{claude_code, codex};
use txcript::{Store, convert};

let store = claude_code::ClaudeStore::default_root().expect("home dir");
let found = store.discover()?;                       // cheap metadata scan
let claude = store.load(&found[0].reference)?;       // Transcript<ClaudeCode>

let codex = convert::<_, codex::Codex>(&claude)?;
codex::CodexStore::default_root().expect("home dir").save(&codex)?;  // resumable on disk
```

canonical மாடல் `Transcript<Common>`: `Meta` + `Vec<Message>`; ஒரு `Message`-இல் typed `Block`-கள் (`Text`, `Thinking`, `ToolUse`, `ToolResult`, `Image`) மற்றும் typed `Tool` enum இருக்கின்றன.

யூசர் ஹார்னெஸில் ஓட்டிய slash கட்டளைகளும் (`/release patch`) canonical தான்: யூசர் டர்னில் ஒரு `Tool::Command` கால், அந்தக் கட்டளை திருப்பி அச்சிட்டது அதன் `ToolResult` ஆக இணைந்து.

### தேடல் (`search` feature, இயல்பாக இயக்கத்தில்)

`txcript::search` டிரான்ஸ்கிரிப்ட்களில் fuzzy (fzf-பாணி syntax) மற்றும் substring தேடலை ஆதரிக்கிறது. one-shot தேடல்:

```rust
use txcript::search::{Query, search};

let hits = search(&common, &Query::substring("relay bug"));  // or Query::fuzzy for fzf syntax
for hit in hits {
    // hit.origin: User | Assistant | Thinking | ToolUse | ToolResult | Meta
    // hit.span addresses the message; hit.highlights are char ranges into hit.line
    let messages = common.fragment(&hit.span);            // zero-copy: Option<&[Message]>
}
```

picker-பாணி தேடலுக்கு, `Index`-ஐ ஒருமுறை கட்டி ஒவ்வொரு keystroke-க்கும் query செய்யுங்கள்:

```rust
use txcript::search::{DocKey, Index, Query};

let mut index = Index::new();
index.insert(DocKey { harness, id }, &common);   // re-insert replaces; caller owns refresh
let matches = index.query(&Query::fuzzy("srch")); // ranked docs, best lines as hits
```

வெற்று pattern கொடுத்தால், ஆவணங்கள் புதியவை முதலில் என்ற வரிசையில் கிடைக்கும். டூல் அவுட்புட்கள் இயல்பாக விலக்கப்படுகின்றன; சேர்க்க வேண்டுமெனில் `Origin::ALL` பயன்படுத்துங்கள். `Query.harnesses`, `Query.limit`, `Query.hits_per_doc` முடிவுகளைச் சுருக்குகின்றன.

### டெக்ஸ்ட் ப்ரொஜெக்ஷன்

`txcript::text::to_text(&common)` தான் [`txcript view`](#cli)-இன் பின்னாலுள்ள projection: `Transcript<Common>`-ஐ LLM context-ஆகப் பயன்படுத்த ஒரு one-way, டோக்கன்-சிக்கனமான rendering. மெசேஜ்கள், reasoning டெக்ஸ்ட், சுருக்கமான டூல் கால்கள்/ரிசல்ட்கள் வைத்துக்கொள்ளப்படுகின்றன; replay-க்கு மட்டுமே தேவையான payload-கள் (encrypted reasoning, usage கணக்கு, inline இமேஜ் பைட்கள்) விடப்படுகின்றன. `to_text_fragment(&common, &span)` செஷன் உள்ளடக்கத்தின் ஒரு `Span`-ஐ render செய்கிறது; ஒவ்வொரு மெசேஜின் முழு-செஷன் எண்ணும் அப்படியே இருக்கும்.

## npm package

npm பேக்கேஜ் கோடெக்கை Bun, Node-க்கு முன்பே build செய்யப்பட்ட WASM ஆக வழங்குகிறது. இது செஷன் டெக்ஸ்டை மெமரியிலேயே மாற்றுகிறது; டிஸ்கில் செஷன்களைக் கண்டுபிடிப்பதும் படிப்பதும் எழுதுவதும் call செய்பவரின் வேலை, அதனால் பேக்கேஜில் `Store` இல்லை.

```ts
import { convert, toCommon, fromCommon, harnesses } from "txcript";
import { readFileSync, writeFileSync } from "node:fs";

const input = readFileSync("rollout.jsonl", "utf8");

// native -> native (e.g. a Codex rollout into Claude Code's JSONL)
writeFileSync("session.jsonl", convert(input, "codex", "claude_code"));

// canonical view, and back
const common = JSON.parse(toCommon(input, "codex"));   // { meta, messages }
const pi = fromCommon(JSON.stringify(common), "pi");

harnesses(); // ["claude_code","claude_chat","chatgpt","codex","opencode","pi","campfire","cursor","cursor_desktop","grok","fx","hermes","amp","antigravity","simple","cowork"]
```

டெக்ஸ்ட்-இன் / டெக்ஸ்ட்-அவுட்: `input` என்பது source ஹார்னெஸின் நேட்டிவ் செஷன் டெக்ஸ்ட்; result டார்கெட்டினுடையது. தவறான ஹார்னெஸ் பெயர்களோ parse ஆகாத input-களோ JS `Error` எறியும்.

தேடலும் சேர்ந்தே வருகிறது. ஒரு query என்பது crate-இன் `Query`-இன் JSON வடிவம்: `pattern` மட்டுமே கட்டாயம்; `mode` `"substring"` என்று செட் செய்யாவிட்டால் `"fuzzy"` ஆக இருக்கும்:

```ts
import { searchTranscript, Searcher } from "txcript";

// one session, one shot: a JSON array of hits
const hits = JSON.parse(searchTranscript(input, "codex", JSON.stringify({ pattern: "relay bug", mode: "substring" })));

// picker-style: index once, query per keystroke
const index = new Searcher();
index.insert("codex", "0dc114bf", input);          // re-insert replaces
const matches = JSON.parse(index.query(JSON.stringify({ pattern: "relay bug" })));
```

| ஹார்னெஸ் | செஷன் டெக்ஸ்ட் |
|---|---|
| `claude_code`, `codex`, `pi`, `campfire` | செஷன் JSONL |
| `claude_chat` | ஒரு live உரையாடலின் detail response (source மட்டும்; கணக்கு export array-கள் இல்லை) |
| `chatgpt` | ஒரு live உரையாடலின் detail response (source மட்டும்; கணக்கு export array-கள் இல்லை) |
| `opencode` | `opencode export` JSON |
| `cursor` | செஷனின் `store.db`-இன் JSON export |
| `cursor_desktop` | செஷனின் `state.vscdb` வரிசைகளின் JSON dump |
| `grok` | செஷன் டைரக்டரியின் கோப்புகளின் JSON bundle |
| `fx` | செஷன் டைரக்டரியின் கோப்புகளின் JSON bundle |
| `hermes` | `hermes sessions export` JSON object |
| `amp` | `amp threads export` JSON |
| `antigravity` | உரையாடல் database-இன் JSON dump; protobuf blob-கள் hex-encoded |
| `simple` | [Simple](../formats/simple.md) interchange JSON document |
| `cowork` | செஷன் ரெக்கார்டு, Claude Code டிரான்ஸ்கிரிப்ட், audit log — இவற்றின் JSON bundle |

மாறாக wasm-ஐ source-இலிருந்து build செய்ய:

```sh
git clone https://github.com/skillsynchq/txcript.git
cd txcript
bun run setup        # once: wasm target + wasm-bindgen-cli
bun run build        # produces ./pkg
```

## ஃபார்மட் ஆவணங்கள்

இந்த டிரான்ஸ்கிரிப்ட் ஃபார்மட்கள் எல்லாவற்றையும் அவற்றின் vendor-கள் ஆவணப்படுத்தவில்லை. [`docs/formats/`](../formats)-இல் ஹார்னெஸுக்கு ஒரு ஆவணம் உள்ளது: செஷன்கள் டிஸ்கில் எங்கே இருக்கின்றன, discovery அவற்றை எப்படிக் கண்டுபிடிக்கிறது, ஃபார்மட்டின் ஒவ்வொரு பகுதியின் விரிவான விளக்கம், அதன் விநோதங்கள் — ஒவ்வொரு கூற்றுக்கும் அதன் ஆதாரம் குறிக்கப்பட்டுள்ளது: அதிகாரப்பூர்வ ஆவணம், ஹார்னெஸின் சொந்த open-source serialization code (commit-pinned permalink-களுடன்), அல்லது reverse engineering.

## டெவலப்மென்ட்

```sh
cargo test --workspace --all-features               # what CI runs
cargo test -p txcript --no-default-features         # codecs only: no SQLite or live stores
bun run build && bun examples/convert.ts <file> <from> <to>
git config core.hooksPath .githooks                 # pre-push runs the CI checks
```

பைனரி தனி workspace crate-இல் (`cli/`, பேக்கேஜ் `txcript-cli`) இருக்கிறது; root-இல் உள்ள லைப்ரரியில் அதன் dependencies எதுவும் இல்லை.

## லைசென்ஸ்

[Apache-2.0](../../LICENSE)
