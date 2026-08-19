//! Per-harness implementations.
//!
//! Each harness is one flat file defining its native record types (its
//! [`Harness::Body`](crate::Harness)), its [`Codec`](crate::Codec) to and from
//! [`Common`](crate::Common), and its [`Store`](crate::Store). The core
//! compiles with none of them present.

pub mod amp;
pub mod antigravity;
pub mod campfire;
pub mod claude_code;
pub mod codex;
pub mod cursor;
pub mod cursor_desktop;
pub mod grok;
pub mod hermes;
pub mod opencode;
pub mod pi;
pub mod simple;

pub(crate) mod jsonl;

/// Guard a transcript-supplied session id before it becomes a path segment.
///
/// Ids are copied verbatim out of session files, so a store must treat them
/// as untrusted: joined into a path, `../x` escapes the store root and an
/// absolute id replaces it entirely. Only a single, plain component is
/// accepted — no separators, no `.`/`..`, no drive-style `:`, no control
/// characters, not empty.
pub(crate) fn checked_id_component(harness: &'static str, id: &str) -> crate::Result<()> {
    let ok = !id.is_empty()
        && id != "."
        && id != ".."
        && !id.contains(['/', '\\', ':'])
        && !id.chars().any(char::is_control);
    if ok {
        Ok(())
    } else {
        Err(crate::Error::Malformed {
            harness,
            detail: format!(
                "session id `{}` is not usable as a file name",
                id.escape_debug()
            ),
        })
    }
}

/// The user's home directory, resolved the way the harness CLIs themselves
/// resolve it: `$HOME` on Unix; on Windows `%USERPROFILE%` first (Node's
/// `os.homedir()` and Rust's home crates ignore `$HOME` there), with `$HOME`
/// as a fallback for MSYS/Cygwin-style shells.
pub(crate) fn home_dir() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("HOME"));
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.filter(|v| !v.is_empty()).map(std::path::PathBuf::from)
}
