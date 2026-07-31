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
pub mod grok;
pub mod opencode;
pub mod pi;

pub(crate) mod jsonl;

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
