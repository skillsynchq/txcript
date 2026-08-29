//! Per-harness implementations.
//!
//! Each harness is one flat file defining its native record types (its
//! [`Harness::Body`](crate::Harness)), its [`Codec`](crate::Codec) to and from
//! [`Common`](crate::Common), and its [`Store`](crate::Store). The core
//! compiles with none of them present.

pub mod amp;
pub mod antigravity;
pub mod campfire;
pub mod chatgpt;
pub mod claude_chat;
pub mod claude_code;
pub mod codex;
pub mod cowork;
pub mod cursor;
pub mod cursor_desktop;
pub mod dsh;
pub mod fx;
pub mod grok;
pub mod hermes;
pub mod opencode;
pub mod pi;
pub mod simple;

pub(crate) mod jsonl;

/// Map `items` across the machine's cores, dropping the `None`s and keeping
/// input order.
///
/// Discovery reads and scans one file per session, which is the bulk of what
/// `list` and `query` do before they can show anything; the work is
/// per-file-independent, so it fans out. Chunks are contiguous and
/// concatenated in order, so the result matches the sequential one exactly.
///
/// wasm32 has no threads and maps sequentially.
pub(crate) fn filter_map_parallel<T, U>(items: &[T], f: impl Fn(&T) -> Option<U> + Sync) -> Vec<U>
where
    T: Sync,
    U: Send,
{
    #[cfg(target_arch = "wasm32")]
    {
        items.iter().filter_map(f).collect()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let workers = std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(items.len());
        // One core, or too little work to be worth a thread.
        if workers <= 1 || items.len() < 8 {
            return items.iter().filter_map(f).collect();
        }
        let chunk = items.len().div_ceil(workers);
        let f = &f;
        std::thread::scope(|scope| {
            let handles: Vec<_> = items
                .chunks(chunk)
                .map(|part| scope.spawn(move || part.iter().filter_map(f).collect::<Vec<U>>()))
                .collect();
            // A worker that panicked contributes nothing, the way an
            // unreadable session does.
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_default())
                .collect()
        })
    }
}

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
