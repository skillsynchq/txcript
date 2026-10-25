//! Text-level codec dispatch: parse a harness's native session text into the
//! canonical model, or render the canonical model back to native text, keyed
//! by a runtime [`HarnessId`].
//!
//! No filesystem, no `SQLite`, no subprocess: this is the codec layer alone
//! (see [`TextCodec`]), so it is available on every target, WASM included. The
//! WASM bindings and [`fidelity`](crate::fidelity) both route through it.

use crate::harness::{
    amp, antigravity, campfire, chatgpt, claude_chat, claude_code, codex, cowork, cowork_remote,
    cursor, cursor_desktop, fx, grok, grok_bot, hermes, opencode, pi, simple,
};
use crate::transcript::{Codec, Common, HarnessId, TextCodec, Transcript};

/// Parse `harness`'s native session text into the canonical model.
///
/// # Errors
/// When the text is not that harness's session format.
pub fn parse_to_common(harness: HarnessId, text: &str) -> crate::Result<Transcript<Common>> {
    fn go<H: TextCodec + Codec>(text: &str) -> crate::Result<Transcript<Common>> {
        H::to_common(&H::from_text(text)?)
    }
    match harness {
        HarnessId::ClaudeCode => go::<claude_code::ClaudeCode>(text),
        HarnessId::ClaudeChat => go::<claude_chat::ClaudeChat>(text),
        HarnessId::CoworkRemote => go::<cowork_remote::CoworkRemote>(text),
        HarnessId::ChatGpt => go::<chatgpt::ChatGpt>(text),
        HarnessId::Codex => go::<codex::Codex>(text),
        HarnessId::OpenCode => go::<opencode::OpenCode>(text),
        HarnessId::Pi => go::<pi::Pi>(text),
        HarnessId::Campfire => go::<campfire::Campfire>(text),
        HarnessId::Cursor => go::<cursor::Cursor>(text),
        HarnessId::CursorDesktop => go::<cursor_desktop::CursorDesktop>(text),
        HarnessId::Grok => go::<grok::Grok>(text),
        HarnessId::GrokBot => go::<grok_bot::GrokBot>(text),
        HarnessId::Fx => go::<fx::Fx>(text),
        HarnessId::Hermes => go::<hermes::Hermes>(text),
        HarnessId::Amp => go::<amp::Amp>(text),
        HarnessId::Antigravity => go::<antigravity::Antigravity>(text),
        HarnessId::Simple => go::<simple::Simple>(text),
        HarnessId::Cowork => go::<cowork::Cowork>(text),
    }
}

/// Render the canonical model as `harness`'s native session text.
///
/// # Errors
/// When the harness cannot represent the transcript (the live read-only
/// sources, for one).
pub fn render_from_common(
    harness: HarnessId,
    common: &Transcript<Common>,
) -> crate::Result<String> {
    fn go<H: TextCodec + Codec>(common: &Transcript<Common>) -> crate::Result<String> {
        H::to_text(&H::from_common(common)?)
    }
    match harness {
        HarnessId::ClaudeCode => go::<claude_code::ClaudeCode>(common),
        HarnessId::ClaudeChat => go::<claude_chat::ClaudeChat>(common),
        HarnessId::CoworkRemote => go::<cowork_remote::CoworkRemote>(common),
        HarnessId::ChatGpt => go::<chatgpt::ChatGpt>(common),
        HarnessId::Codex => go::<codex::Codex>(common),
        HarnessId::OpenCode => go::<opencode::OpenCode>(common),
        HarnessId::Pi => go::<pi::Pi>(common),
        HarnessId::Campfire => go::<campfire::Campfire>(common),
        HarnessId::Cursor => go::<cursor::Cursor>(common),
        HarnessId::CursorDesktop => go::<cursor_desktop::CursorDesktop>(common),
        HarnessId::Grok => go::<grok::Grok>(common),
        HarnessId::GrokBot => go::<grok_bot::GrokBot>(common),
        HarnessId::Fx => go::<fx::Fx>(common),
        HarnessId::Hermes => go::<hermes::Hermes>(common),
        HarnessId::Amp => go::<amp::Amp>(common),
        HarnessId::Antigravity => go::<antigravity::Antigravity>(common),
        HarnessId::Simple => go::<simple::Simple>(common),
        HarnessId::Cowork => go::<cowork::Cowork>(common),
    }
}
