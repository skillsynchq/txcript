//! The user's editor, run from inside the crop editor.
//!
//! A terminal editor runs in a pseudo-terminal sized to a pane of the
//! screen: its output is parsed into a screen the pane paints, every key
//! is forwarded as the bytes a terminal would send, and the pane closes
//! when the editor exits. An editor that opens a window of its own runs
//! detached, and the crop editor waits for it to close.

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::sync::{Arc, RwLock, RwLockReadGuard};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// The editor to run: `$VISUAL`, else `$EDITOR`, else `vi`.
pub(crate) fn editor_command() -> String {
    ["VISUAL", "EDITOR"]
        .iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| "vi".to_string())
}

/// The editor's name as the user knows it: the command's program, without
/// its directory.
pub(crate) fn editor_name(command: &str) -> String {
    let program = command.split_whitespace().next().unwrap_or(command);
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
        .to_string()
}

/// Whether the command opens a window of its own rather than drawing in
/// the terminal, so it cannot run in a pane.
pub(crate) fn opens_a_window(command: &str) -> bool {
    const WINDOWED: &[&str] = &[
        "code",
        "code-insiders",
        "codium",
        "cursor",
        "windsurf",
        "subl",
        "sublime_text",
        "zed",
        "mate",
        "atom",
        "gedit",
        "kate",
        "idea",
        "webstorm",
        "pycharm",
        "clion",
        "goland",
        "rubymine",
        "bbedit",
        "gvim",
        "mvim",
        "open",
        "xdg-open",
    ];
    let name = editor_name(command);
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    if matches!(name, "emacs" | "emacsclient") {
        // Emacs draws in the terminal only when asked to.
        return !command
            .split_whitespace()
            .skip(1)
            .any(|arg| matches!(arg, "-nw" | "-t" | "--tty" | "--no-window-system"));
    }
    WINDOWED.contains(&name)
}

/// `command "<file>"` through the shell, so `$EDITOR` may carry arguments.
fn shell_invocation(command: &str) -> (&'static str, Vec<String>) {
    if cfg!(windows) {
        ("cmd", vec!["/C".into(), format!("{command} \"%1\"")])
    } else {
        (
            "sh",
            vec!["-c".into(), format!("{command} \"$1\""), "sh".into()],
        )
    }
}

/// Run `command` on `file` in the terminal itself, inheriting it, and wait.
/// The caller has already handed the terminal over.
///
/// # Errors
/// When the command cannot start or exits unsuccessfully.
pub(crate) fn run_in_terminal(command: &str, file: &Path) -> Result<(), String> {
    let (program, args) = shell_invocation(command);
    let status = std::process::Command::new(program)
        .args(args)
        .arg(file)
        .status()
        .map_err(|error| format!("starting `{command}`: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("the editor exited with {status}"))
    }
}

/// A terminal editor drawing into a pane.
pub(crate) struct Pane {
    parser: Arc<RwLock<vt100::Parser>>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn std::io::Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Pane {
    /// Start `command` on `file` in a pseudo-terminal `rows` by `cols`.
    ///
    /// # Errors
    /// When no pseudo-terminal can be opened or the command cannot start.
    pub(crate) fn open(command: &str, file: &Path, rows: u16, cols: u16) -> Result<Self, String> {
        let size = PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|error| format!("opening a terminal for the editor: {error}"))?;
        let (program, args) = shell_invocation(command);
        let mut builder = CommandBuilder::new(program);
        for arg in args {
            builder.arg(arg);
        }
        builder.arg(file);
        if let Ok(cwd) = std::env::current_dir() {
            builder.cwd(cwd);
        }
        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|error| format!("starting `{command}`: {error}"))?;
        // The editor holds the only other end now; closing ours lets the
        // reader see the end of its output when it exits.
        drop(pair.slave);
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| format!("reading from the editor: {error}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| format!("writing to the editor: {error}"))?;
        let parser = Arc::new(RwLock::new(vt100::Parser::new(size.rows, size.cols, 0)));
        let sink = Arc::clone(&parser);
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        if let Ok(mut parser) = sink.write() {
                            parser.process(&buffer[..count]);
                        }
                    }
                }
            }
        });
        Ok(Self {
            parser,
            master: pair.master,
            writer,
            child,
        })
    }

    /// The editor's screen, for painting.
    pub(crate) fn screen(&self) -> Option<RwLockReadGuard<'_, vt100::Parser>> {
        self.parser.read().ok()
    }

    pub(crate) fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        if let Ok(mut parser) = self.parser.write() {
            parser.screen_mut().set_size(rows, cols);
        }
        // A pane the editor cannot be told about keeps its old size; the
        // next paint clips it, which is the best that can be done.
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Forward a key to the editor.
    pub(crate) fn key(&mut self, key: &KeyEvent) {
        let application_cursor = self
            .parser
            .read()
            .is_ok_and(|parser| parser.screen().application_cursor());
        let bytes = encode_key(key, application_cursor);
        if !bytes.is_empty() {
            let _ = self.writer.write_all(&bytes);
            let _ = self.writer.flush();
        }
    }

    /// Forward pasted text to the editor, bracketed when it asked for that.
    pub(crate) fn paste(&mut self, text: &str) {
        let bracketed = self
            .parser
            .read()
            .is_ok_and(|parser| parser.screen().bracketed_paste());
        let _ = if bracketed {
            self.writer
                .write_all(format!("\x1b[200~{text}\x1b[201~").as_bytes())
        } else {
            self.writer.write_all(text.as_bytes())
        };
        let _ = self.writer.flush();
    }

    /// `Some` once the editor has exited: whether it did so cleanly.
    pub(crate) fn finished(&mut self) -> Option<Result<(), String>> {
        match self.child.try_wait() {
            Ok(Some(status)) if status.success() => Some(Ok(())),
            Ok(Some(status)) => Some(Err(format!("the editor exited with {status}"))),
            Ok(None) => None,
            Err(error) => Some(Err(format!("waiting for the editor: {error}"))),
        }
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        // Closing the pane must not leave an editor behind.
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
    }
}

/// An editor running in a window of its own.
pub(crate) struct Detached {
    child: std::process::Child,
}

impl Detached {
    /// # Errors
    /// When the command cannot start.
    pub(crate) fn open(command: &str, file: &Path) -> Result<Self, String> {
        let (program, args) = shell_invocation(command);
        let child = std::process::Command::new(program)
            .args(args)
            .arg(file)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|error| format!("starting `{command}`: {error}"))?;
        Ok(Self { child })
    }

    /// `Some` once the editor has exited: whether it did so cleanly.
    pub(crate) fn finished(&mut self) -> Option<Result<(), String>> {
        match self.child.try_wait() {
            Ok(Some(status)) if status.success() => Some(Ok(())),
            Ok(Some(status)) => Some(Err(format!("the editor exited with {status}"))),
            Ok(None) => None,
            Err(error) => Some(Err(format!("waiting for the editor: {error}"))),
        }
    }
}

/// The bytes a terminal sends for `key`: xterm's encoding, with the cursor
/// keys in application mode when the editor switched to it.
pub(crate) fn encode_key(key: &KeyEvent, application_cursor: bool) -> Vec<u8> {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // xterm's modifier parameter: 1 + shift(1) + alt(2) + control(4).
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(control);
    let csi = |body: &str, suffix: char| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[{body};{modifier}{suffix}").into_bytes()
        } else if body == "1" {
            format!("\x1b[{suffix}").into_bytes()
        } else {
            format!("\x1b[{body}{suffix}").into_bytes()
        }
    };
    let cursor = |letter: char| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{letter}").into_bytes()
        } else if application_cursor {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{letter}").into_bytes()
        }
    };
    let mut bytes = match key.code {
        KeyCode::Char(ch) if control => {
            let byte = match ch.to_ascii_lowercase() {
                ch @ 'a'..='z' => ch as u8 - b'a' + 1,
                ' ' | '@' | '2' => 0,
                '[' | '3' => 0x1b,
                '\\' | '4' => 0x1c,
                ']' | '5' => 0x1d,
                '^' | '6' => 0x1e,
                '_' | '7' | '-' | '/' => 0x1f,
                '?' | '8' => 0x7f,
                _ => return ch.to_string().into_bytes(),
            };
            vec![byte]
        }
        KeyCode::Char(ch) => ch.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace if control => vec![0x08],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => cursor('A'),
        KeyCode::Down => cursor('B'),
        KeyCode::Right => cursor('C'),
        KeyCode::Left => cursor('D'),
        KeyCode::Home => cursor('H'),
        KeyCode::End => cursor('F'),
        KeyCode::Insert => csi("2", '~'),
        KeyCode::Delete => csi("3", '~'),
        KeyCode::PageUp => csi("5", '~'),
        KeyCode::PageDown => csi("6", '~'),
        KeyCode::F(n @ 1..=4) => {
            let letter = (b'P' + n - 1) as char;
            if modifier > 1 {
                format!("\x1b[1;{modifier}{letter}").into_bytes()
            } else {
                format!("\x1bO{letter}").into_bytes()
            }
        }
        KeyCode::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)];
            csi(&code.to_string(), '~')
        }
        _ => Vec::new(),
    };
    // Alt sends an escape before the key, except where it is already a
    // modifier parameter.
    if alt
        && matches!(
            key.code,
            KeyCode::Char(_) | KeyCode::Enter | KeyCode::Backspace | KeyCode::Tab
        )
    {
        bytes.insert(0, 0x1b);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_editor_comes_from_the_environment_and_is_named_by_its_program() {
        assert_eq!(editor_name("/usr/bin/nvim"), "nvim");
        assert_eq!(editor_name("code --wait"), "code");
        assert_eq!(editor_name("vi"), "vi");
    }

    #[test]
    fn windowed_editors_are_told_apart_from_terminal_ones() {
        assert!(opens_a_window("code --wait"));
        assert!(opens_a_window(
            "/Applications/Zed.app/Contents/MacOS/zed -w"
        ));
        assert!(opens_a_window("emacsclient -c"));
        assert!(!opens_a_window("emacsclient -t"));
        assert!(!opens_a_window("emacs -nw"));
        assert!(!opens_a_window("nvim"));
        assert!(!opens_a_window("vim -u NONE"));
        assert!(!opens_a_window("nano"));
        assert!(!opens_a_window("hx"));
    }

    #[test]
    fn keys_encode_the_way_a_terminal_sends_them() {
        let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let with = |code, modifiers| KeyEvent::new(code, modifiers);
        assert_eq!(encode_key(&plain(KeyCode::Char('a')), false), b"a");
        assert_eq!(
            encode_key(&plain(KeyCode::Char('é')), false),
            "é".as_bytes()
        );
        assert_eq!(
            encode_key(&with(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            vec![3]
        );
        assert_eq!(
            encode_key(&with(KeyCode::Char('['), KeyModifiers::CONTROL), false),
            vec![0x1b]
        );
        assert_eq!(
            encode_key(&with(KeyCode::Char('x'), KeyModifiers::ALT), false),
            b"\x1bx"
        );
        assert_eq!(encode_key(&plain(KeyCode::Enter), false), b"\r");
        assert_eq!(encode_key(&plain(KeyCode::Backspace), false), vec![0x7f]);
        assert_eq!(encode_key(&plain(KeyCode::Esc), false), vec![0x1b]);
        assert_eq!(encode_key(&plain(KeyCode::Up), false), b"\x1b[A");
        assert_eq!(encode_key(&plain(KeyCode::Up), true), b"\x1bOA");
        assert_eq!(
            encode_key(&with(KeyCode::Up, KeyModifiers::SHIFT), true),
            b"\x1b[1;2A"
        );
        assert_eq!(encode_key(&plain(KeyCode::Home), false), b"\x1b[H");
        assert_eq!(encode_key(&plain(KeyCode::Delete), false), b"\x1b[3~");
        assert_eq!(
            encode_key(&with(KeyCode::Delete, KeyModifiers::CONTROL), false),
            b"\x1b[3;5~"
        );
        assert_eq!(encode_key(&plain(KeyCode::PageDown), false), b"\x1b[6~");
        assert_eq!(encode_key(&plain(KeyCode::F(1)), false), b"\x1bOP");
        assert_eq!(encode_key(&plain(KeyCode::F(5)), false), b"\x1b[15~");
        assert_eq!(encode_key(&plain(KeyCode::F(12)), false), b"\x1b[24~");
        assert_eq!(encode_key(&plain(KeyCode::BackTab), false), b"\x1b[Z");
        assert!(encode_key(&plain(KeyCode::CapsLock), false).is_empty());
    }

    /// Wait up to two seconds for `done` to hold.
    #[cfg(unix)]
    fn settle(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..200 {
            if done() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    #[cfg(unix)]
    #[test]
    fn a_pane_runs_the_command_on_the_file_and_carries_keys_and_output() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("message.md");
        std::fs::write(&file, "draft body\n").unwrap();

        // Print the file, then echo what is typed until EOF.
        let mut pane = Pane::open("sh -c 'cat \"$1\"; cat' _", &file, 10, 40).unwrap();
        assert!(settle(|| {
            pane.screen()
                .is_some_and(|parser| parser.screen().contents().contains("draft body"))
        }));
        for ch in "typed".chars() {
            pane.key(&KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        pane.key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(settle(|| {
            pane.screen()
                .is_some_and(|parser| parser.screen().contents().contains("typed"))
        }));
        assert!(pane.finished().is_none());
        pane.key(&KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert!(settle(|| pane.finished().is_some()));
        assert_eq!(pane.finished(), Some(Ok(())));

        let mut failing = Pane::open("false", &file, 5, 20).unwrap();
        assert!(settle(|| failing.finished().is_some()));
        assert!(failing.finished().unwrap().is_err());
        assert!(
            Pane::open("/nonexistent/editor", &file, 5, 20).is_err() || {
                let mut missing = Pane::open("/nonexistent/editor", &file, 5, 20).unwrap();
                settle(|| missing.finished().is_some()) && missing.finished().unwrap().is_err()
            }
        );
    }

    #[test]
    fn the_shell_invocation_passes_the_file_as_the_first_argument() {
        let (program, args) = shell_invocation("nvim -u NONE");
        if cfg!(windows) {
            assert_eq!(program, "cmd");
        } else {
            assert_eq!(program, "sh");
            assert_eq!(args, vec!["-c", "nvim -u NONE \"$1\"", "sh"]);
        }
    }
}
