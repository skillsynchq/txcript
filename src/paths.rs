//! Directory-membership checks shared by every `--cwd` filter (`list`,
//! `query`): component-wise, canonicalized, and Windows-aware, so one
//! spelling of a directory matches all the others.

/// Strip the Windows verbatim-device prefix (`\\?\`, including `\\?\UNC\`)
/// that `canonicalize` adds, so vanished paths still match canonicalized
/// parents. Wide-char math keeps non-UTF8 paths intact.
#[cfg(windows)]
const VERBATIM_PREFIX: &[u16] = &[0x5C, 0x5C, 0x3F, 0x5C]; // `\\?\`
#[cfg(windows)]
const UNC_PREFIX: &[u16] = &[0x5C, 0x5C, 0x3F, 0x5C, 0x55, 0x4E, 0x43, 0x5C]; // `\\?\UNC\`

#[cfg(windows)]
fn strip_verbatim_prefix(path: &std::path::Path) -> std::path::PathBuf {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if let Some(rest) = wide.strip_prefix(UNC_PREFIX) {
        // `\\?\UNC\server\share` ⟺ `\\server\share`.
        let mut full = vec![0x5C, 0x5C];
        full.extend_from_slice(rest);
        std::ffi::OsString::from_wide(&full).into()
    } else if let Some(rest) = wide.strip_prefix(VERBATIM_PREFIX) {
        std::ffi::OsString::from_wide(rest).into()
    } else {
        path.to_path_buf()
    }
}

#[cfg(not(windows))]
fn strip_verbatim_prefix(path: &std::path::Path) -> std::path::PathBuf {
    path.to_path_buf()
}

/// Component-wise `starts_with` that folds case on Windows, where `C:\Users`
/// and `c:\users` are the same directory. Non-Windows keeps the exact
/// `Path::starts_with`.
fn path_starts_with(child: &std::path::Path, parent: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        let mut child_comps = child.components();
        let mut parent_comps = parent.components();
        loop {
            match (parent_comps.next(), child_comps.next()) {
                (None, _) => return true,
                (Some(_), None) => return false,
                (Some(p), Some(c)) => {
                    // Unicode lowercase approximates the filesystem's own
                    // case folding; ASCII-only would miss e.g. `é` vs `É`.
                    if p.as_os_str().to_string_lossy().to_lowercase()
                        != c.as_os_str().to_string_lossy().to_lowercase()
                    {
                        return false;
                    }
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        child.starts_with(parent)
    }
}

/// Canonicalize the longest existing prefix of `path`, re-appending the
/// vanished tail verbatim, so the parent still resolves symlinks, junctions,
/// or 8.3 short names the raw spelling would mismatch.
fn canonicalize_lenient(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path;
    loop {
        if let Ok(base) = current.canonicalize() {
            let mut out = strip_verbatim_prefix(&base);
            for component in tail.iter().rev() {
                out.push(component);
            }
            return out;
        }
        let mut components = current.components();
        match components.next_back() {
            // Empty path, or only a prefix/root remains: nothing resolvable.
            None | Some(Component::Prefix(_) | Component::RootDir) => {
                return strip_verbatim_prefix(path);
            }
            Some(last) => {
                tail.push(last.as_os_str().to_os_string());
                current = components.as_path();
            }
        }
    }
}

/// True when a session's recorded `cwd` is `dir` or anywhere under it, so a
/// monorepo session started in `repo/packages/foo` shows up when filtering
/// `repo`. The check is component-wise (`/foo/barbaz` is not under
/// `/foo/bar`). Both sides are canonicalized so different spellings of one
/// directory still match (`/tmp` vs `/private/tmp`, `$PWD` through a
/// symlink); a path that no longer exists keeps its raw spelling, so
/// vanished directories compare as plain components.
#[must_use]
pub fn under_dir(session_cwd: &str, dir: &std::path::Path) -> bool {
    path_starts_with(
        &canonicalize_lenient(std::path::Path::new(session_cwd)),
        &canonicalize_lenient(dir),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn strip_verbatim_prefix_leaves_plain_paths_alone() {
        let p = std::path::Path::new("some/relative/dir");
        assert_eq!(super::strip_verbatim_prefix(p), p.to_path_buf());
    }

    #[cfg(windows)]
    #[test]
    fn strip_verbatim_prefix_strips_device_and_unc_forms() {
        assert_eq!(
            super::strip_verbatim_prefix(std::path::Path::new(r"\\?\C:\some\repo")),
            std::path::PathBuf::from(r"C:\some\repo")
        );
        assert_eq!(
            super::strip_verbatim_prefix(std::path::Path::new(r"\\?\UNC\server\share")),
            std::path::PathBuf::from(r"\\server\share")
        );
    }

    // Drive-letter paths only parse where `\` separates components.
    // The tempdir case is the reported scenario: live `dir`, vanished child.
    #[cfg(windows)]
    #[test]
    fn under_dir_matches_vanished_child_of_live_dir() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("packages").join("foo");
        assert!(super::under_dir(gone.to_str().unwrap(), dir.path()));
    }

    // Platform-neutral shape of the same rule: a vanished child matches,
    // a sibling sharing the string prefix does not.
    #[test]
    fn under_dir_matches_vanished_child_but_not_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("packages").join("foo");
        assert!(super::under_dir(gone.to_str().unwrap(), dir.path()));
        let packages = dir.path().join("packages");
        let sibling = dir.path().join("packages-foo");
        assert!(!super::under_dir(sibling.to_str().unwrap(), &packages));
    }

    #[cfg(windows)]
    #[test]
    fn under_dir_handles_verbatim_prefixes_and_casing() {
        let dir = std::path::Path::new(r"C:\some\repo");
        assert!(super::under_dir(r"C:\some\repo\packages\foo", dir));
        assert!(super::under_dir(r"c:\Some\Repo\packages\foo", dir));
        assert!(super::under_dir(r"C:/some/repo/packages/foo", dir));
        assert!(!super::under_dir(r"C:\some\repo2", dir));
        assert!(!super::under_dir(r"C:\other\repo", dir));
    }
}
