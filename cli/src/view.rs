//! `txcript view` — inspect a session in the terminal or print compact text.
//!
//! The source is a session id or exact title, looked up like `continue`,
//! with an optional `#range` fragment (see `fragment.rs`). A terminal gets a
//! readable, colored view in the built-in pager (see `pager.rs`), whose
//! controls hide roles, tool calls, and reasoning; `TXCRIPT_PAGER` names an
//! external pager command instead, and `--no-pager` writes the view
//! directly. A pipe or redirect keeps the established compact, colorless text
//! projection. Both presentations print message numbers, so what you see is
//! what you reference.
//!
//! On a terminal that draws kitty graphics (see `graphics.rs`), images in the
//! human view are shown inline instead of noted as omitted, through the
//! pager included.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::process::ExitCode;
use std::process::{Command, Stdio};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use txcript::common::{ArtifactSource, Block, ImageSource, Message, Role, ToolOutput};
use txcript::{Common, HarnessId, Span, Transcript, text};

use crate::fragment;
use crate::graphics;
use crate::pager;

/// Resolve a `view`/`export` source — a session id or exact title, with
/// an optional `#range` — to the session's canonical transcript and the
/// parsed range request, if any.
pub fn load_source(
    source: &str,
    from: Option<HarnessId>,
) -> Result<(Transcript<Common>, Option<fragment::SpanReq>), String> {
    if let Some(loaded) = super::load_direct_claude_chat(source, from) {
        return loaded;
    }
    if let Some(loaded) = super::load_direct_chatgpt(source, from) {
        return loaded;
    }
    let sessions = super::discover_with_spinner(from)?;
    // A whole-input match (a title that itself contains `#12`) beats the
    // fragment interpretation.
    let (src, request) = match fragment::parse_ref(source) {
        (_, Some(_)) if super::find_exact(&sessions, from, source).is_some() => (source, None),
        parsed => parsed,
    };

    let session = super::find_session(&sessions, from, src)?.ok_or_else(|| {
        let (origin, scope) = if from == Some(HarnessId::ClaudeChat) {
            ("Claude Chat", String::new())
        } else if from == Some(HarnessId::ChatGpt) {
            ("ChatGPT", String::new())
        } else {
            ("local", from.map_or(String::new(), |h| format!(" {h}")))
        };
        format!(
            "no {origin}{scope} session matches `{src}` (try `{} list`)",
            crate::program()
        )
    })?;
    let common = session
        .read()
        .map_err(|e| format!("reading session `{src}`: {e}"))?;
    Ok((common, request))
}

pub fn cmd_view(source: &str, from: Option<HarnessId>, no_pager: bool) -> Result<ExitCode, String> {
    let (common, request) = load_source(source, from)?;

    let total = common.body.len();
    let span = match &request {
        Some(req) => req.resolve(total)?,
        None => Span(0..total),
    };
    let stdout_is_terminal = std::io::stdout().is_terminal();
    let (columns, rows) = terminal_size::terminal_size().map_or((80, 24), |(width, height)| {
        (usize::from(width.0), usize::from(height.0))
    });
    let width = render_width(columns);
    let out_of_bounds = || format!("range is out of bounds — the session has {total} messages");
    if !stdout_is_terminal {
        // `resolve` bounds-checked against `total`, so the render lands.
        let text = text::to_text_fragment(&common, &span).ok_or_else(out_of_bounds)?;
        return write_stream(std::io::stdout().lock(), text.as_bytes())
            .map(|()| ExitCode::SUCCESS)
            .map_err(|error| format!("writing stdout: {error}"));
    }
    let txcript_pager = nonempty_env("TXCRIPT_PAGER");
    let pager = pager_for(no_pager, txcript_pager.as_deref());
    let color = std::env::var_os("NO_COLOR").is_none();
    let mut document = Document::new(common, span, color, inline_images(pager));
    let rendered = document
        .render(width, Filters::default())
        .ok_or_else(out_of_bounds)?;
    match pager {
        // A view that fits on the screen reads better printed than paged.
        Pager::Builtin if pager::visual_rows(&rendered.text, columns) < rows => {
            output(&rendered, Pager::Direct)?;
        }
        Pager::Builtin => pager::run(document, rendered, width)?,
        pager => output(&rendered, pager)?,
    }
    Ok(ExitCode::SUCCESS)
}

/// The column the human view is laid out to: the terminal's width, within
/// reason.
pub(crate) const fn render_width(columns: usize) -> usize {
    if columns < 40 {
        40
    } else if columns > 120 {
        120
    } else {
        columns
    }
}

/// The terminal geometry for inline images, when the terminal draws kitty
/// graphics and the pager will pass placeholder cells through: the built-in
/// pager, no pager, or an external `less` new enough to be told the
/// placeholder is printable.
fn inline_images(pager: Pager<'_>) -> Option<graphics::Cells> {
    let less = match pager {
        Pager::Direct | Pager::Builtin => None,
        Pager::Command(command) => Some(less_program(command)?),
    };
    let cells = graphics::detect()?;
    less.is_none_or(graphics::less_supports_placeholders)
        .then_some(cells)
}

/// The program a configured pager command runs, if it is `less`.
fn less_program(command: &str) -> Option<&str> {
    let program = command.split_whitespace().next()?;
    (std::path::Path::new(program).file_name() == Some(std::ffi::OsStr::new("less")))
        .then_some(program)
}

/// A rendered view: the text, the graphics transmissions the terminal
/// needs before it can draw the text's image placeholders (those not yet
/// sent), and where each message starts.
pub(crate) struct Rendered {
    pub(crate) text: String,
    pub(crate) transmissions: Vec<Vec<u8>>,
    /// Line index of each message's rule, in text order.
    pub(crate) message_starts: Vec<usize>,
}

/// What the human view shows; everything, by default.
// A set of independent switches, one per pager key.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Filters {
    pub(crate) user: bool,
    pub(crate) assistant: bool,
    pub(crate) tools: bool,
    pub(crate) reasoning: bool,
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            user: true,
            assistant: true,
            tools: true,
            reasoning: true,
        }
    }
}

impl Filters {
    fn shows_role(self, role: Role) -> bool {
        match role {
            Role::User => self.user,
            Role::Assistant => self.assistant,
        }
    }

    fn shows_block(self, block: &Block) -> bool {
        match block {
            Block::ToolUse { .. } | Block::ToolResult { .. } => self.tools,
            Block::Thinking { .. } => self.reasoning,
            Block::Text { .. } | Block::Image { .. } | Block::Artifact { .. } => true,
        }
    }
}

/// A session range the pager re-renders on demand: the same text under
/// different [`Filters`] or widths, with images placed once and reused.
pub(crate) struct Document {
    common: Transcript<Common>,
    span: Span,
    color: bool,
    images: Images,
}

impl Document {
    pub(crate) fn new(
        common: Transcript<Common>,
        span: Span,
        color: bool,
        cells: Option<graphics::Cells>,
    ) -> Self {
        Self {
            common,
            span,
            color,
            images: Images::new(cells),
        }
    }

    /// The human view at `width` under `filters`; `None` when the span is
    /// out of bounds.
    pub(crate) fn render(&mut self, width: usize, filters: Filters) -> Option<Rendered> {
        render_human(
            &self.common,
            &self.span,
            self.color,
            width,
            filters,
            &mut self.images,
        )
    }

    /// The pager's prompt line: the range shown and the controls, with
    /// switched-off ones marked.
    pub(crate) fn status(&self, filters: Filters) -> String {
        let shown = match self.span.0.len() {
            0 => "none".to_string(),
            1 => format!("#{}", self.span.0.start + 1),
            _ => format!("#{}–{}", self.span.0.start + 1, self.span.0.end),
        };
        let control = |key: &str, name: &str, on: bool| {
            if on {
                format!("{key} {name}")
            } else {
                format!("{key} {name} off")
            }
        };
        format!(
            "{shown}/{}  {}  {}  {}  {}  [ ] message  / search  q quit",
            self.common.body.len(),
            control("u", "user", filters.user),
            control("a", "assistant", filters.assistant),
            control("t", "tools", filters.tools),
            control("r", "reasoning", filters.reasoning),
        )
    }
}

/// Where a terminal view goes. `PAGER` is deliberately not consulted: the
/// built-in pager is the product, and `TXCRIPT_PAGER` is the opt-out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pager<'a> {
    Direct,
    Command(&'a str),
    Builtin,
}

const fn pager_for(no_pager: bool, txcript_pager: Option<&str>) -> Pager<'_> {
    if no_pager {
        Pager::Direct
    } else if let Some(command) = txcript_pager {
        Pager::Command(command)
    } else {
        Pager::Builtin
    }
}

fn render_human(
    common: &Transcript<Common>,
    span: &Span,
    color: bool,
    width: usize,
    filters: Filters,
    images: &mut Images,
) -> Option<Rendered> {
    let messages = common.fragment(span)?;
    let mut out = String::new();
    human_header(&mut out, common, span, color);
    let message_starts = human_messages(
        &mut out,
        messages,
        span.0.start,
        color,
        width,
        filters,
        images,
    );
    Some(Rendered {
        text: out,
        transmissions: images.take_transmissions(),
        message_starts,
    })
}

/// Inline images placed while rendering: each gets the next id and its
/// transmission queued for the terminal once; re-renders reuse the
/// placeholder rows.
struct Images {
    cells: Option<graphics::Cells>,
    next_id: u32,
    placed: u32,
    remaining_bytes: usize,
    transmissions: Vec<Vec<u8>>,
    /// Placeholder rows (or the decision to omit) by message and block.
    rows: HashMap<(usize, usize), Option<String>>,
}

impl Images {
    /// Images per view; ids are one byte wide under the process's base.
    const MAX_COUNT: u32 = 255;
    /// Transmission bytes per view, so a screenshot-heavy session does not
    /// stall the terminal before the pager opens.
    const MAX_BYTES: usize = 64 * 1024 * 1024;
    /// Rows kept free around an image so it fits on one screen.
    const ROW_MARGIN: u16 = 3;
    /// The `│ ` gutter that marks image rows, in cells.
    const GUTTER_WIDTH: usize = 2;

    fn new(cells: Option<graphics::Cells>) -> Self {
        // Ids 1..=255 are reserved for the placeholder's 8-bit color form;
        // start above them, in a band derived from the pid so successive
        // views in one terminal do not repaint each other's scrollback.
        let base = ((std::process::id() % 0x7FFF) + 1) << 8;
        Self {
            cells,
            next_id: base + 1,
            placed: 0,
            remaining_bytes: Self::MAX_BYTES,
            transmissions: Vec::new(),
            rows: HashMap::new(),
        }
    }

    /// The transmissions queued since the last call.
    fn take_transmissions(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.transmissions)
    }

    /// Placeholder rows for `source`, the image at `key` (message offset,
    /// block index), each behind a gutter bar so the image reads as a
    /// quoted block rather than loose text; `None` when images are off, the
    /// view's quota is spent, or the image cannot be shown. A key seen
    /// before answers as it did the first time.
    fn place(
        &mut self,
        key: (usize, usize),
        source: &ImageSource,
        color: bool,
        width: usize,
    ) -> Option<String> {
        if let Some(rows) = self.rows.get(&key) {
            return rows.clone();
        }
        let rows = self.place_new(source, color, width);
        self.rows.insert(key, rows.clone());
        rows
    }

    fn place_new(&mut self, source: &ImageSource, color: bool, width: usize) -> Option<String> {
        let cells = self.cells?;
        if self.placed >= Self::MAX_COUNT || source.source_type != "base64" {
            return None;
        }
        let bytes = BASE64.decode(source.data.trim()).ok()?;
        let max_columns = u16::try_from(width.saturating_sub(Self::GUTTER_WIDTH))
            .unwrap_or(u16::MAX)
            .max(1);
        let placement = graphics::place(
            self.next_id,
            &bytes,
            cells,
            max_columns.min(cells.columns),
            cells.rows.saturating_sub(Self::ROW_MARGIN).max(1),
        )?;
        if placement.transmission.len() > self.remaining_bytes {
            return None;
        }
        self.remaining_bytes -= placement.transmission.len();
        self.next_id += 1;
        self.placed += 1;
        self.transmissions.push(placement.transmission);
        let gutter = format!("{} ", paint("2;34", "│", color));
        Some(
            placement
                .placeholder
                .lines()
                .fold(String::new(), |mut rows, row| {
                    let _ = writeln!(rows, "{gutter}{row}");
                    rows
                }),
        )
    }
}

fn human_header(out: &mut String, common: &Transcript<Common>, span: &Span, color: bool) {
    let title = common.meta.title.as_deref().unwrap_or(&common.meta.id);
    let _ = writeln!(out, "{}", paint("1;36", &terminal_label_safe(title), color));
    human_field(out, "ID", &common.meta.id, color);
    human_field(out, "Started", &common.meta.timestamp.to_rfc3339(), color);
    if let Some(cwd) = common.meta.cwd.as_deref().filter(|value| !value.is_empty()) {
        human_field(out, "Directory", cwd, color);
    }
    if let Some(branch) = common
        .meta
        .git_branch
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        human_field(out, "Branch", branch, color);
    }
    if let Some(model) = common
        .meta
        .model
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        human_field(out, "Model", model, color);
    }
    let shown = match span.0.len() {
        0 => "none".to_string(),
        1 => format!("#{}", span.0.start + 1),
        _ => format!("#{}–{}", span.0.start + 1, span.0.end),
    };
    human_field(
        out,
        "Messages",
        &format!("{shown} of {}", common.body.len()),
        color,
    );
}

/// Render `messages` under `filters`, returning the line index of each
/// message rule. A message whose role is hidden, or with nothing left to
/// show, is skipped whole.
fn human_messages(
    out: &mut String,
    messages: &[Message],
    start: usize,
    color: bool,
    width: usize,
    filters: Filters,
    images: &mut Images,
) -> Vec<usize> {
    let mut blocks = Blocks {
        tool_ids: HashMap::new(),
        next_tool_id: 1,
        images,
        color,
        width,
    };
    let mut message_starts = Vec::new();
    let mut lines = 0usize;
    let mut counted = 0usize;
    for (offset, message) in messages.iter().enumerate() {
        if !filters.shows_role(message.role)
            || !message
                .content
                .iter()
                .any(|block| filters.shows_block(block))
        {
            continue;
        }
        let ordinal = start + offset + 1;
        let role = match message.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
        };
        lines += out[counted..].bytes().filter(|byte| *byte == b'\n').count();
        counted = out.len();
        // The rule follows the blank line `human_rule` opens with.
        message_starts.push(lines + 1);
        human_rule(out, &format!("Message #{ordinal} · {role}"), width, color);
        for (index, block) in message.content.iter().enumerate() {
            if filters.shows_block(block) {
                blocks.render(out, (offset, index), block);
            }
        }
    }
    message_starts
}

/// Per-block rendering state for one view: tool numbering and images.
struct Blocks<'a, 'm> {
    tool_ids: HashMap<&'m str, usize>,
    next_tool_id: usize,
    images: &'a mut Images,
    color: bool,
    width: usize,
}

impl<'m> Blocks<'_, 'm> {
    fn render(&mut self, out: &mut String, key: (usize, usize), block: &'m Block) {
        let color = self.color;
        match block {
            Block::Text { text } => human_body(out, text),
            Block::Thinking { text, .. } => {
                human_section(out, "Thinking", "2;35", color);
                human_body(out, text);
            }
            Block::ToolUse { id, tool } => {
                let short_id = human_tool_id(&mut self.tool_ids, &mut self.next_tool_id, id);
                let (name, input) = tool.to_canonical();
                human_section(
                    out,
                    &format!("Tool #{short_id} · {}", terminal_label_safe(&name)),
                    "1;33",
                    color,
                );
                let body = match input {
                    serde_json::Value::Null => String::new(),
                    serde_json::Value::Object(ref map) if map.is_empty() => String::new(),
                    value => {
                        serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
                    }
                };
                human_body(out, &body);
            }
            Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let short_id =
                    human_tool_id(&mut self.tool_ids, &mut self.next_tool_id, tool_use_id);
                let suffix = if *is_error { " · Error" } else { "" };
                human_section(
                    out,
                    &format!("Result #{short_id}{suffix}"),
                    if *is_error { "1;31" } else { "1;32" },
                    color,
                );
                match content {
                    ToolOutput::Text(text) => human_body(out, text),
                    ToolOutput::Json(value) => {
                        let body = serde_json::to_string_pretty(value)
                            .unwrap_or_else(|_| value.to_string());
                        human_body(out, &body);
                    }
                }
            }
            Block::Image { source } => {
                let media_type = terminal_label_safe(&source.media_type);
                match self.images.place(key, source, color, self.width) {
                    Some(placeholder) => {
                        human_section(out, &format!("Image · {media_type}"), "2;34", color);
                        out.push_str(&placeholder);
                    }
                    None => {
                        human_section(out, &format!("Image · {media_type} omitted"), "2;34", color);
                    }
                }
            }
            Block::Artifact { artifact } => {
                human_section(
                    out,
                    &format!("Artifact · {}", terminal_label_safe(&artifact.name)),
                    "1;34",
                    color,
                );
                let body = match &artifact.source {
                    ArtifactSource::Path { path, .. } => path.as_str(),
                    ArtifactSource::Text { text, .. } => text.as_str(),
                    ArtifactSource::Base64 { .. } => "binary data omitted",
                };
                human_body(out, body);
            }
        }
    }
}

fn human_field(out: &mut String, label: &str, value: &str, color: bool) {
    let _ = writeln!(
        out,
        "{}  {}",
        paint("2", &format!("{label:<10}"), color),
        terminal_label_safe(value)
    );
}

fn human_rule(out: &mut String, label: &str, width: usize, color: bool) {
    let prefix = format!("── {label} ");
    let suffix = "─".repeat(width.saturating_sub(prefix.chars().count()).max(2));
    let _ = writeln!(
        out,
        "\n{}",
        paint("1;36", &format!("{prefix}{suffix}"), color)
    );
}

fn human_section(out: &mut String, label: &str, code: &str, color: bool) {
    let _ = writeln!(out, "\n{}", paint(code, &format!("▸ {label}"), color));
}

fn human_body(out: &mut String, text: &str) {
    out.push_str(&terminal_safe(text));
    if !text.ends_with('\n') {
        out.push('\n');
    }
}

fn human_tool_id<'a>(
    ids: &mut HashMap<&'a str, usize>,
    next_id: &mut usize,
    provider_id: &'a str,
) -> usize {
    *ids.entry(provider_id).or_insert_with(|| {
        let id = *next_id;
        *next_id += 1;
        id
    })
}

fn terminal_safe(text: &str) -> String {
    text.chars()
        .flat_map(|ch| match ch {
            '\n' | '\t' => ch.to_string().chars().collect::<Vec<_>>(),
            ch if ch.is_control() => ch.escape_default().collect(),
            ch => vec![ch],
        })
        .collect()
}

fn terminal_label_safe(text: &str) -> String {
    text.chars()
        .flat_map(|ch| {
            if ch.is_control() {
                ch.escape_default().collect()
            } else {
                vec![ch]
            }
        })
        .collect()
}

fn paint(code: &str, text: &str, color: bool) -> String {
    if color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn output(rendered: &Rendered, pager: Pager<'_>) -> Result<(), String> {
    let bytes = rendered.text.as_bytes();
    // Images go to the terminal first, so they exist by the time their
    // placeholders are drawn — whoever draws them.
    if !rendered.transmissions.is_empty() {
        let mut stdout = std::io::stdout().lock();
        for transmission in &rendered.transmissions {
            write_stream(&mut stdout, transmission)
                .map_err(|error| format!("writing images to the terminal: {error}"))?;
        }
        std::io::Write::flush(&mut stdout)
            .map_err(|error| format!("writing images to the terminal: {error}"))?;
    }
    let mut command = match pager {
        Pager::Direct => {
            return write_stream(std::io::stdout().lock(), bytes)
                .map_err(|error| format!("writing stdout: {error}"));
        }
        Pager::Builtin => {
            return Err("the built-in pager is not driven through `output`".to_string());
        }
        Pager::Command(command) => shell_command(command),
    };
    if !rendered.transmissions.is_empty() {
        let existing = std::env::var("LESSUTFCHARDEF").ok();
        command.env(
            "LESSUTFCHARDEF",
            graphics::less_char_definitions(existing.as_deref()),
        );
    }
    command.stdin(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("starting pager: {error}"))?;
    if let Some(stdin) = child.stdin.take()
        && let Err(error) = write_stream(stdin, bytes)
    {
        // Do not leave a pager behind if its stdin fails for a reason other
        // than the normal early-close/BrokenPipe case handled by
        // `write_stream`.
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("writing to pager: {error}"));
    }
    let status = child
        .wait()
        .map_err(|error| format!("waiting for pager: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("pager exited with {status}"))
    }
}

#[cfg(unix)]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("sh");
    shell.arg("-c").arg(command);
    shell
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.arg("/C").arg(command);
    shell
}

fn write_stream(mut writer: impl std::io::Write, bytes: &[u8]) -> Result<(), std::io::Error> {
    match writer.write_all(bytes) {
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use chrono::DateTime;
    use clap::Parser as _;
    use txcript::common::{Block, ImageSource, Message, Meta, Role};

    use super::*;

    fn transcript() -> Transcript<Common> {
        Transcript::new(
            Meta {
                id: "session-123".into(),
                timestamp: DateTime::UNIX_EPOCH,
                cwd: Some("/work/project".into()),
                git_branch: Some("main".into()),
                title: Some("Fix the parser".into()),
                cli_version: None,
                model: Some("test-model".into()),
            },
            vec![Message {
                role: Role::User,
                content: vec![Block::Text {
                    text: "Please fix it.".into(),
                }],
                timestamp: DateTime::UNIX_EPOCH,
                model: None,
                stop_reason: None,
                usage: None,
            }],
        )
    }

    #[test]
    fn terminal_rendering_is_human_facing_while_pipeline_rendering_stays_compact() {
        let common = transcript();
        let span = Span(0..1);

        let terminal = Document::new(common.clone(), span.clone(), false, None)
            .render(60, Filters::default())
            .unwrap()
            .text;
        let pipeline = text::to_text_fragment(&common, &span).unwrap();

        assert!(terminal.contains("Fix the parser"));
        assert!(terminal.contains("Message #1 · User"));
        assert!(terminal.contains("Please fix it."));
        assert!(!terminal.contains("[session]"));

        assert!(pipeline.starts_with("[session]\n"));
        assert!(pipeline.contains("── #1 ──\n[user]\nPlease fix it."));
    }

    #[test]
    fn human_ranges_keep_full_session_ordinals() {
        let mut common = transcript();
        let template = common.body[0].clone();
        common.body = vec![template; 7];

        let rendered = Document::new(common, Span(4..7), false, None)
            .render(60, Filters::default())
            .unwrap()
            .text;

        assert!(rendered.contains("Messages    #5–7 of 7"));
        assert!(rendered.contains("Message #5 · User"));
        assert!(rendered.contains("Message #6 · User"));
        assert!(rendered.contains("Message #7 · User"));
        assert!(!rendered.contains("Message #4 · User"));
    }

    #[test]
    fn images_render_as_placeholders_only_when_the_terminal_draws_them() {
        let mut common = transcript();
        let png = {
            let image = txcript_png();
            BASE64.encode(image)
        };
        common.body[0].content.push(Block::Image {
            source: ImageSource {
                source_type: "base64".into(),
                media_type: "image/png".into(),
                data: png,
            },
        });
        let span = Span(0..1);
        let cells = graphics::Cells {
            cell_width: 10,
            cell_height: 20,
            columns: 100,
            rows: 40,
        };

        let plain = Document::new(common.clone(), span.clone(), false, None)
            .render(60, Filters::default())
            .unwrap();
        assert!(plain.text.contains("▸ Image · image/png omitted"));
        assert!(plain.transmissions.is_empty());

        let mut document = Document::new(common.clone(), span.clone(), false, Some(cells));
        let drawn = document.render(60, Filters::default()).unwrap();
        assert!(drawn.text.contains("▸ Image · image/png\n│ \x1b[38;2;"));
        assert!(!drawn.text.contains("omitted"));
        // A 40×20 image is 4 cells wide and one row tall, behind the gutter.
        assert_eq!(drawn.text.matches('\u{10EEEE}').count(), 4);
        assert_eq!(drawn.text.matches("│ ").count(), 1);
        assert_eq!(drawn.transmissions.len(), 1);
        assert!(drawn.transmissions[0].starts_with(b"\x1b_Ga=T,U=1,q=2,i="));

        // A re-render reuses the placement: same rows, nothing new to send.
        let again = document.render(60, Filters::default()).unwrap();
        assert_eq!(again.text, drawn.text);
        assert!(again.transmissions.is_empty());

        // Compact output never carries images.
        let compact = text::to_text_fragment(&common, &span).unwrap();
        assert!(!compact.contains('\u{10EEEE}'));
    }

    fn txcript_png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(40, 20, image::Rgba([10, 200, 30, 255]));
        let mut out = io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn only_less_pagers_take_inline_images() {
        assert_eq!(less_program("less -R"), Some("less"));
        assert_eq!(
            less_program("/opt/homebrew/bin/less"),
            Some("/opt/homebrew/bin/less")
        );
        assert_eq!(less_program("  less"), Some("less"));
        assert_eq!(less_program("bat --paging=always"), None);
        assert_eq!(less_program("lesspipe"), None);
        assert_eq!(less_program(""), None);
    }

    #[test]
    fn no_pager_is_accepted_by_the_view_command() {
        let cli =
            crate::Cli::try_parse_from(["txcript", "view", "session-123", "--no-pager"]).unwrap();
        assert!(matches!(
            cli.command,
            crate::Command::Session(crate::SessionCommand::View { no_pager: true, .. })
        ));
    }

    #[test]
    fn pager_selection_respects_the_flag_and_the_override() {
        assert_eq!(
            pager_for(false, Some("custom --flag")),
            Pager::Command("custom --flag")
        );
        assert_eq!(pager_for(false, None), Pager::Builtin);
        assert_eq!(pager_for(true, None), Pager::Direct);
        assert_eq!(pager_for(true, Some("custom")), Pager::Direct);
        assert_eq!(render_width(20), 40);
        assert_eq!(render_width(100), 100);
        assert_eq!(render_width(300), 120);
    }

    #[test]
    fn filters_drop_roles_and_blocks_and_message_starts_track_the_rules() {
        let mut common = transcript();
        let user = common.body[0].clone();
        let assistant = Message {
            role: Role::Assistant,
            content: vec![
                Block::Thinking {
                    text: "pondering".into(),
                    signature: None,
                    encrypted: None,
                },
                Block::Text {
                    text: "On it.".into(),
                },
                Block::ToolUse {
                    id: "call-1".into(),
                    tool: txcript::common::Tool::Raw {
                        tool_name: "shell".into(),
                        input: serde_json::json!({"cmd": "ls"}),
                    },
                },
            ],
            ..user.clone()
        };
        let result = Message {
            role: Role::User,
            content: vec![Block::ToolResult {
                tool_use_id: "call-1".into(),
                content: txcript::common::ToolOutput::Text("files".into()),
                is_error: false,
            }],
            ..user.clone()
        };
        common.body = vec![user, assistant, result];
        let mut document = Document::new(common, Span(0..3), false, None);

        let all = document.render(60, Filters::default()).unwrap();
        let lines: Vec<&str> = all.text.lines().collect();
        assert_eq!(all.message_starts.len(), 3);
        for (ordinal, start) in all.message_starts.iter().enumerate() {
            assert!(
                lines[*start].starts_with(&format!("── Message #{} ·", ordinal + 1)),
                "line {start}: {:?}",
                lines[*start]
            );
        }
        assert!(all.text.contains("pondering"));
        assert!(all.text.contains("Tool #1 · shell"));
        assert!(all.text.contains("Result #1"));

        let no_tools = document
            .render(
                60,
                Filters {
                    tools: false,
                    ..Filters::default()
                },
            )
            .unwrap();
        assert!(!no_tools.text.contains("Tool #1"));
        assert!(!no_tools.text.contains("Result #1"));
        // The result-only message has nothing left and is skipped whole.
        assert!(!no_tools.text.contains("Message #3"));
        assert_eq!(no_tools.message_starts.len(), 2);

        let no_assistant = document
            .render(
                60,
                Filters {
                    assistant: false,
                    ..Filters::default()
                },
            )
            .unwrap();
        assert!(!no_assistant.text.contains("Message #2"));
        assert!(no_assistant.text.contains("Message #1 · User"));
        assert!(no_assistant.text.contains("Message #3 · User"));

        let no_reasoning = document
            .render(
                60,
                Filters {
                    reasoning: false,
                    ..Filters::default()
                },
            )
            .unwrap();
        assert!(!no_reasoning.text.contains("pondering"));
        assert!(no_reasoning.text.contains("On it."));

        let status = document.status(Filters {
            user: false,
            ..Filters::default()
        });
        assert!(status.starts_with("#1–3/3  u user off  a assistant  t tools  r reasoning"));
    }

    struct FailingWriter(io::ErrorKind);

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(self.0))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn only_broken_pipe_output_errors_are_ignored() {
        assert!(write_stream(FailingWriter(io::ErrorKind::BrokenPipe), b"text").is_ok());
        let error = write_stream(FailingWriter(io::ErrorKind::WriteZero), b"text").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WriteZero);
    }

    #[test]
    fn human_output_escapes_terminal_control_sequences() {
        let safe = terminal_safe("before\x1b]52;c;payload\x07after\r\nnext\tcolumn");
        assert!(
            !safe
                .chars()
                .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
        );
        assert!(safe.contains("\\u{1b}]52;c;payload\\u{7}after\\r"));
        assert!(safe.ends_with("\nnext\tcolumn"));
    }

    #[test]
    fn human_labels_escape_line_breaks_and_tabs() {
        let safe = terminal_label_safe("title\nforged field\tvalue\r");
        assert_eq!(safe, "title\\nforged field\\tvalue\\r");
        assert!(!safe.chars().any(char::is_control));
    }

    #[cfg(unix)]
    #[test]
    fn configured_pager_receives_the_rendered_text() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("pager-output");
        let command = format!("cat > {}", destination.display());

        let rendered = Rendered {
            text: "rendered session\n".into(),
            transmissions: Vec::new(),
            message_starts: Vec::new(),
        };
        output(&rendered, Pager::Command(&command)).unwrap();

        assert_eq!(
            std::fs::read_to_string(destination).unwrap(),
            "rendered session\n"
        );
    }
}
