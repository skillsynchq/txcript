# Qoder

Qoder CLI uses [Claude Code's wire format](claude-code.md): JSONL conversation
records wrapping Anthropic messages, linked by `parentUuid`. Its surrounding
bookkeeping, titles, and storage layout differ. This document is
**reverse-engineered** from real local qodercli 1.1.65 sessions;
`src/harness/qoder.rs` is the authoritative mapping.

```
~/.qoder/projects/                    ($QODER_CONFIG_DIR/projects overrides)
└── <encoded-cwd>/
    ├── <session>.jsonl               conversation + bookkeeping records
    └── <session>/                   Qoder-internal sidecars
        ├── state.json
        └── compression-v2/
```

## On disk

Sessions live at `~/.qoder/projects/<encoded-cwd>/<session>.jsonl`.
`QODER_CONFIG_DIR` overrides the config root when set and non-empty.
The China build, `qoderclicn`, uses `~/.qoder-cn` and ignores that variable;
set `QODER_CONFIG_DIR="$HOME/.qoder-cn"` to point txcript at its sessions.

Project names replace every UTF-16 code unit outside `[A-Za-z0-9]` with
`-`. If the encoded name exceeds 200 characters, it becomes the first 200
characters plus `-` plus `base36(abs(djb2-xor(original path)))`. The hash
uses the original path's UTF-16 units, seed 5381, and signed 32-bit
`h = (h * 33) ^ unit` arithmetic.

`QoderStore` discovers JSONL files exactly two levels below the projects
root, without descending into sidecar directories, and lists a file only
when it holds at least one line with a `type` and a `sessionId`. Neither `state.json`
nor `compression-v2/` is read or written; resume with full history was
verified without them. Native delete mirrors `qodercli --delete-session`:
only the `.jsonl` is removed, leaving the sidecar directory.

## Dissection of a transcript

`Qoder` shares Claude Code's native `Record` body and conversation codec.
Tool names and arguments are the same (`Read`, `Edit`, `Write`, `Bash`,
`Grep`, `Glob`, …); tool results ride on `user` lines. See
[claude-code.md](claude-code.md#dissection-of-a-transcript) for block mappings.

| Their name | What it is | Maps to |
|---|---|---|
| `user` / `assistant` | Envelope wrapping an Anthropic message | User / assistant `Message` |
| `attachment` | Hook output, skill and agent listings | `Record::Other`, no Common turn |
| `active-leaf` | Last-turn pointer | `Record::Other`, bookkeeping |
| `runtime-config`, `workspace-directories`, `worktree-state` | Runtime and workspace state | `Record::Other`, bookkeeping |
| `last-prompt` | Prompt bookkeeping | `Record::Other`, no Common turn |
| `custom-title` | User-set title (`qodercli -n <name>`) | `Meta.title`, highest precedence |
| `ai-title` | Generated title | `Meta.title` when no custom title exists |
| `relocated`, `file-history-snapshot` | Relocation and file-history bookkeeping | `Record::Other`, no Common turn |
| informational `system` | Notices and other bookkeeping | `Record::Other`, no Common turn |

Title precedence is `custom-title` > `ai-title`; Qoder has no `summary`
line. `from_common` writes conversation lines as Claude Code does, with an
`ai-title` line first when a title exists and a trailing `active-leaf`
naming the last turn. Model names, message ids, and usage from other
harnesses are accepted as-is.

## Lineage

`--fork-session` stamps copied lines with
`forkedFrom: {sessionId, messageUuid}`. The first such `sessionId` supplies
a Fork `Meta.lineage` parent, both at discovery and on load.

## Caveats

Common conversion loses bookkeeping, envelope extras, and unmodeled blocks
as in [Claude Code](claude-code.md#caveats), and `Meta.lineage` is read but
not written back. A foreign thinking signature is written through unchanged;
Qoder resumes with it. Native load/save retains bookkeeping records.

Resume with `qodercli --resume <id>`. Override txcript's launch command with
`TRANSCRIPT_QODER_RESUME_CMD`, for example
`TRANSCRIPT_QODER_RESUME_CMD='qoderclicn --resume {id}'` for the China build.

## References

- Observed local qodercli 1.1.65 sessions, including forks, long project
  paths, deletion, and resume without sidecars.
- Conversation mapping: [claude-code.md](claude-code.md).

The authoritative txcript mapping is `src/harness/qoder.rs`, sharing the
conversation codec in `src/harness/claude_code.rs`.

Last verified: 2026-10-05, against src/harness/qoder.rs and real local qodercli 1.1.65 sessions.
