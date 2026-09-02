//! The pager behind `txcript view`: a viewport over the rendered session
//! with controls over what it shows.
//!
//! The pager owns a re-renderable [`Document`], so a control is a change of
//! [`Filters`] followed by a re-render — the same text projection the
//! direct and external-pager paths print, minus the parts switched off.
//! The renderer's ANSI output is parsed into styled spans here, so there is
//! one source of truth for the presentation. Search runs over what is shown:
//! hiding everything but user messages *is* "search only user messages".
//!
//! Keys: `j`/`k`/arrows scroll, space/`f`/page-down and `b`/page-up page,
//! `ctrl-d`/`ctrl-u` half-page, `g`/`G` jump to the ends, `]`/`[` jump
//! between messages; `u`, `a`, `t`, `r` toggle user messages, assistant
//! messages, tool calls, and reasoning; `/` searches (smart case: all
//! lowercase matches either case), `n`/`N` step through matches, escape
//! clears the search; `q` quits.
//!
//! Lines wrap here, at the terminal's width: the renderer leaves body text
//! unwrapped so the direct and external-pager paths keep it whole. The
//! viewport, message jumps, and search hits all work in visual rows.
//!
//! Inline images ride on Unicode placeholder cells (see `graphics.rs`);
//! each placeholder and its diacritics is one grapheme, so the cell grid
//! carries it through unchanged and wrapping never splits it.

use std::io::Write as _;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::{DefaultTerminal, Frame};
use txcript::Span as MessageSpan;
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

use crate::view::{Document, Filters, Rendered, render_width};

/// Page `first`, the document rendered at `width` under the default
/// filters, until the user quits.
pub fn run(document: Document, first: Rendered, width: usize) -> Result<(), String> {
    let (mut terminal, cleanup) = init_terminal("pager")?;
    let result = Pager::new(document, first, width).run(&mut terminal);
    finish_after_restore(result, cleanup.try_restore())
}

/// Interactively choose a message range from `document`. Enter confirms the
/// current selection; q, escape, and ctrl-c cancel without writing anything.
pub fn crop(
    document: Document,
    first: Rendered,
    width: usize,
    initial: Option<MessageSpan>,
) -> Result<Option<MessageSpan>, String> {
    let (mut terminal, cleanup) = init_terminal("crop editor")?;
    let result = Cropper::new(document, first, width, initial)
        .and_then(|mut cropper| cropper.run(&mut terminal));
    finish_after_restore(result, cleanup.try_restore())
}

struct TerminalCleanup {
    armed: bool,
}

impl TerminalCleanup {
    const fn new() -> Self {
        Self { armed: true }
    }

    fn try_restore(mut self) -> std::io::Result<()> {
        let result = ratatui::try_restore();
        if result.is_ok() {
            self.armed = false;
        }
        result
    }
}

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        if self.armed {
            ratatui::restore();
        }
    }
}

fn init_terminal(label: &str) -> Result<(DefaultTerminal, TerminalCleanup), String> {
    match ratatui::try_init() {
        Ok(terminal) => Ok((terminal, TerminalCleanup::new())),
        Err(error) => {
            let cleanup = ratatui::try_restore();
            let detail = cleanup.err().map_or_else(String::new, |cleanup| {
                format!("; cleanup also failed: {cleanup}")
            });
            Err(format!("starting the {label}: {error}{detail}"))
        }
    }
}

fn finish_after_restore<T>(
    result: Result<T, String>,
    restore: std::io::Result<()>,
) -> Result<T, String> {
    restore.map_err(|error| format!("restoring the terminal: {error}"))?;
    result
}

/// The message-level crop selection, independent of terminal rendering so its
/// boundary behavior can be tested without a TTY.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CropSelection {
    cursor: usize,
    start: usize,
    end: usize,
    total: usize,
}

impl CropSelection {
    fn new(total: usize, initial: Option<MessageSpan>) -> Result<Self, String> {
        if total == 0 {
            return Err("cannot crop an empty session".to_string());
        }
        let span = initial.unwrap_or(MessageSpan(0..total));
        if span.0.start >= span.0.end || span.0.end > total {
            return Err(format!(
                "invalid initial crop range {}..{} for a session with {total} messages",
                span.0.start, span.0.end
            ));
        }
        Ok(Self {
            cursor: span.0.start,
            start: span.0.start,
            end: span.0.end,
            total,
        })
    }

    fn span(&self) -> MessageSpan {
        MessageSpan(self.start..self.end)
    }

    fn move_cursor(&mut self, delta: isize) {
        self.cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(self.total.saturating_sub(1));
    }

    fn mark_start(&mut self) {
        self.start = self.cursor;
        if self.start >= self.end {
            self.end = self.cursor + 1;
        }
    }

    fn mark_end(&mut self) {
        self.end = self.cursor + 1;
        if self.end <= self.start {
            self.start = self.cursor;
        }
    }
}

/// The full state of the pager between key presses.
struct Pager {
    document: Document,
    filters: Filters,
    width: usize,
    lines: Vec<Styled>,
    /// Line indices of the message rules in `lines`.
    message_starts: Vec<usize>,
    /// `lines` wrapped to `cols`: the rows the screen shows.
    visual: Vec<Row>,
    /// The first visual row of each line.
    line_first_row: Vec<usize>,
    /// Columns the current layout was wrapped to.
    cols: usize,
    /// The first visual row on screen.
    top: usize,
    /// Content rows on screen, as of the last draw.
    rows: usize,
    search: Search,
    /// The search query being typed, if the prompt is open.
    prompt: Option<String>,
    /// Image transmissions the terminal has not received yet.
    pending_images: Vec<Vec<u8>>,
    quit: bool,
}

/// One screen row: a slice of a line.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    line: usize,
    range: std::ops::Range<usize>,
}

/// A committed search: the query and the visual rows holding a match.
#[derive(Default)]
struct Search {
    query: String,
    matches: Vec<usize>,
    /// Index into `matches` of the current one.
    current: Option<usize>,
}

impl Pager {
    fn new(document: Document, first: Rendered, width: usize) -> Self {
        let mut pager = Self {
            document,
            filters: Filters::default(),
            width,
            lines: Vec::new(),
            message_starts: Vec::new(),
            visual: Vec::new(),
            line_first_row: Vec::new(),
            cols: width,
            top: 0,
            rows: content_rows(24),
            search: Search::default(),
            prompt: None,
            pending_images: Vec::new(),
            quit: false,
        };
        pager.accept(first);
        pager
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<(), String> {
        while !self.quit {
            // Image data belongs to the screen it is sent on, so it goes out
            // now, on the alternate screen, before the placeholders that
            // reference it are drawn.
            if !self.pending_images.is_empty() {
                let mut stdout = std::io::stdout().lock();
                for transmission in self.pending_images.drain(..) {
                    stdout
                        .write_all(&transmission)
                        .map_err(|error| format!("sending images to the terminal: {error}"))?;
                }
                stdout
                    .flush()
                    .map_err(|error| format!("sending images to the terminal: {error}"))?;
            }
            terminal
                .draw(|frame| self.draw(frame))
                .map_err(|error| format!("drawing the pager: {error}"))?;
            match event::read().map_err(|error| format!("reading input: {error}"))? {
                Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key),
                Event::Resize(columns, _) => self.resize(usize::from(columns)),
                Event::Paste(text) => {
                    if let Some(prompt) = &mut self.prompt {
                        prompt.push_str(&text);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Take a fresh render: parse its lines, queue its images, keep the
    /// scroll position and search valid against the new text.
    fn accept(&mut self, rendered: Rendered) {
        self.lines = rendered.text.lines().map(parse_ansi).collect();
        self.message_starts = rendered.message_starts;
        self.pending_images.extend(rendered.transmissions);
        self.relayout();
    }

    /// Wrap the lines to the current width and remap everything that
    /// points at rows: the scroll position and the search hits.
    fn relayout(&mut self) {
        self.visual.clear();
        self.line_first_row.clear();
        for (index, line) in self.lines.iter().enumerate() {
            self.line_first_row.push(self.visual.len());
            self.visual
                .extend(wrap(&line.plain, self.cols).map(|range| Row { line: index, range }));
        }
        self.top = self.top.min(self.visual.len().saturating_sub(1));
        self.refresh_search();
    }

    fn rerender(&mut self) {
        if let Some(rendered) = self.document.render(self.width, self.filters) {
            self.accept(rendered);
        }
    }

    fn resize(&mut self, columns: usize) {
        let width = render_width(columns);
        if width != self.width {
            self.width = width;
            self.rerender();
        }
    }

    // `q`, escape, and ctrl-c all quit; the guards keep them apart.
    #[allow(clippy::match_same_arms)]
    fn key(&mut self, key: KeyEvent) {
        if self.prompt.is_some() {
            self.prompt_key(key);
            return;
        }
        let rows = self.rows;
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc if !self.search.query.is_empty() => self.search = Search::default(),
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if control => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.scroll_to(self.top + 1, rows),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_to(self.top.saturating_sub(1), rows),
            KeyCode::Char('d') if control => self.scroll_to(self.top + rows / 2, rows),
            KeyCode::Char('u') if control => {
                self.scroll_to(self.top.saturating_sub(rows / 2), rows);
            }
            KeyCode::Char(' ' | 'f') | KeyCode::PageDown => self.scroll_to(self.top + rows, rows),
            KeyCode::Char('b') | KeyCode::PageUp => {
                self.scroll_to(self.top.saturating_sub(rows), rows);
            }
            KeyCode::Char('g') | KeyCode::Home => self.scroll_to(0, rows),
            KeyCode::Char('G') | KeyCode::End => self.scroll_to(usize::MAX, rows),
            KeyCode::Char(']') => {
                let top = self.top;
                let next = self.message_rows().find(|start| *start > top);
                if let Some(start) = next {
                    self.scroll_to(start, rows);
                }
            }
            KeyCode::Char('[') => {
                let top = self.top;
                let start = self
                    .message_rows()
                    .filter(|start| *start < top)
                    .last()
                    .unwrap_or(0);
                self.scroll_to(start, rows);
            }
            KeyCode::Char('u') => self.toggle(|filters| &mut filters.user),
            KeyCode::Char('a') => self.toggle(|filters| &mut filters.assistant),
            KeyCode::Char('t') => self.toggle(|filters| &mut filters.tools),
            KeyCode::Char('r') => self.toggle(|filters| &mut filters.reasoning),
            KeyCode::Char('/') => self.prompt = Some(String::new()),
            KeyCode::Char('n') => self.step_match(true),
            KeyCode::Char('N') => self.step_match(false),
            _ => {}
        }
    }

    fn prompt_key(&mut self, key: KeyEvent) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Char('c' | 'g') if control => self.prompt = None,
            KeyCode::Char('u') if control => prompt.clear(),
            KeyCode::Char('w') if control => {
                let trimmed = prompt.trim_end();
                let cut = trimmed.rfind(' ').map_or(0, |at| at + 1);
                prompt.truncate(cut);
            }
            KeyCode::Backspace => {
                prompt.pop();
            }
            KeyCode::Enter => {
                let query = self.prompt.take().unwrap_or_default();
                self.search = Search {
                    query,
                    ..Search::default()
                };
                self.refresh_search();
                // Land on the first match at or below the top of the screen.
                let rows = self.rows;
                if let Some(index) = self
                    .search
                    .matches
                    .iter()
                    .position(|line| *line >= self.top)
                {
                    self.search.current = Some(index);
                    self.scroll_to(self.search.matches[index], rows);
                } else if !self.search.matches.is_empty() {
                    self.search.current = Some(0);
                    self.scroll_to(self.search.matches[0], rows);
                }
            }
            // Any character, shifted or not — that is what a query is.
            KeyCode::Char(ch) if !control && !key.modifiers.contains(KeyModifiers::ALT) => {
                prompt.push(ch);
            }
            _ => {}
        }
    }

    fn toggle(&mut self, flag: impl FnOnce(&mut Filters) -> &mut bool) {
        let flag = flag(&mut self.filters);
        *flag = !*flag;
        self.rerender();
    }

    /// The visual rows the message rules start on.
    fn message_rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.message_starts
            .iter()
            .filter_map(|line| self.line_first_row.get(*line).copied())
    }

    fn scroll_to(&mut self, row: usize, rows: usize) {
        let max_top = self.visual.len().saturating_sub(rows);
        self.top = row.min(max_top);
    }

    /// Move to the next (or previous) match from the current one, wrapping.
    fn step_match(&mut self, forward: bool) {
        if self.search.matches.is_empty() {
            return;
        }
        let rows = self.rows;
        let count = self.search.matches.len();
        let next = match self.search.current {
            Some(current) if forward => (current + 1) % count,
            Some(current) => (current + count - 1) % count,
            None if forward => self
                .search
                .matches
                .iter()
                .position(|line| *line > self.top)
                .unwrap_or(0),
            None => self
                .search
                .matches
                .iter()
                .rposition(|line| *line < self.top)
                .unwrap_or(count - 1),
        };
        self.search.current = Some(next);
        self.scroll_to(self.search.matches[next], rows);
    }

    /// Recompute which visual rows hold a match for the current query: the
    /// row each match starts on.
    fn refresh_search(&mut self) {
        if self.search.query.is_empty() {
            self.search.matches.clear();
            self.search.current = None;
            return;
        }
        let case_sensitive = smart_case(&self.search.query);
        self.search.matches = self
            .visual
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                find_all(
                    &self.lines[row.line].plain,
                    &self.search.query,
                    case_sensitive,
                )
                .iter()
                .any(|hit| row.range.contains(&hit.start))
            })
            .map(|(index, _)| index)
            .collect();
        self.search.current = self
            .search
            .current
            .filter(|current| *current < self.search.matches.len());
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let rows = content_rows(usize::from(area.height));
        self.rows = rows;
        if usize::from(area.width) != self.cols {
            self.cols = usize::from(area.width);
            self.relayout();
        }
        // A resize may have shrunk the text under the scroll position.
        self.scroll_to(self.top, rows);
        let case_sensitive = smart_case(&self.search.query);
        let text: Vec<Line> = self
            .visual
            .iter()
            .skip(self.top)
            .take(rows)
            .map(|row| {
                let line = &self.lines[row.line];
                let hits = if self.search.query.is_empty() {
                    Vec::new()
                } else {
                    find_all(&line.plain, &self.search.query, case_sensitive)
                };
                Line::from(row_spans(line, &row.range, &hits))
            })
            .collect();
        let content = Rect {
            height: area.height.saturating_sub(1),
            ..area
        };
        frame.render_widget(Paragraph::new(Text::from(text)), content);

        let status_area = Rect {
            y: area.y + area.height.saturating_sub(1),
            height: 1,
            ..area
        };
        let status = if let Some(prompt) = &self.prompt {
            format!("/{prompt}")
        } else {
            let matches = match (self.search.current, self.search.matches.len()) {
                (_, 0) if self.search.query.is_empty() => String::new(),
                (_, 0) => "  no matches".to_string(),
                (Some(current), count) => format!("  match {}/{count}", current + 1),
                (None, count) => format!("  {count} matches"),
            };
            format!("{}{matches}", self.document.status(self.filters))
        };
        let style = if self.prompt.is_some() {
            Style::new()
        } else {
            Style::new().add_modifier(Modifier::REVERSED)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(status, style))).style(style),
            status_area,
        );
        if let Some(prompt) = &self.prompt {
            let column = u16::try_from(prompt.chars().count() + 1).unwrap_or(u16::MAX);
            frame.set_cursor_position((
                status_area.x + column.min(status_area.width.saturating_sub(1)),
                status_area.y,
            ));
        }
    }
}

/// Interactive crop editor over the same rendered document as `view`.
struct Cropper {
    document: Document,
    color: bool,
    width: usize,
    lines: Vec<Styled>,
    message_starts: Vec<usize>,
    visual: Vec<Row>,
    line_first_row: Vec<usize>,
    cols: usize,
    top: usize,
    rows: usize,
    selection: CropSelection,
    pending_images: Vec<Vec<u8>>,
    notice: Option<String>,
    result: Option<MessageSpan>,
    done: bool,
}

impl Cropper {
    fn new(
        document: Document,
        first: Rendered,
        width: usize,
        initial: Option<MessageSpan>,
    ) -> Result<Self, String> {
        let selection = CropSelection::new(document.message_count(), initial)?;
        let color = document.color_enabled();
        let mut cropper = Self {
            document,
            color,
            width,
            lines: Vec::new(),
            message_starts: Vec::new(),
            visual: Vec::new(),
            line_first_row: Vec::new(),
            cols: width,
            top: 0,
            rows: content_rows(24),
            selection,
            pending_images: Vec::new(),
            notice: None,
            result: None,
            done: false,
        };
        cropper.accept(first);
        cropper.focus_cursor();
        Ok(cropper)
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<Option<MessageSpan>, String> {
        while !self.done {
            if !self.pending_images.is_empty() {
                let mut stdout = std::io::stdout().lock();
                for transmission in self.pending_images.drain(..) {
                    stdout
                        .write_all(&transmission)
                        .map_err(|error| format!("sending images to the terminal: {error}"))?;
                }
                stdout
                    .flush()
                    .map_err(|error| format!("sending images to the terminal: {error}"))?;
            }
            terminal
                .draw(|frame| self.draw(frame))
                .map_err(|error| format!("drawing the crop editor: {error}"))?;
            match event::read().map_err(|error| format!("reading input: {error}"))? {
                Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key),
                Event::Resize(columns, _) => self.resize(usize::from(columns)),
                _ => {}
            }
        }
        Ok(self.result.clone())
    }

    fn accept(&mut self, rendered: Rendered) {
        self.lines = rendered.text.lines().map(parse_ansi).collect();
        self.message_starts = rendered.message_starts;
        self.pending_images.extend(rendered.transmissions);
        self.relayout();
    }

    fn relayout(&mut self) {
        self.visual.clear();
        self.line_first_row.clear();
        for (index, line) in self.lines.iter().enumerate() {
            self.line_first_row.push(self.visual.len());
            self.visual
                .extend(wrap(&line.plain, self.cols).map(|range| Row { line: index, range }));
        }
        self.top = self.top.min(self.visual.len().saturating_sub(1));
    }

    fn resize(&mut self, columns: usize) {
        let width = crop_render_width(columns);
        if width != self.width {
            self.width = width;
            if let Some(rendered) = self.document.render(self.width, Filters::crop()) {
                self.accept(rendered);
                self.focus_cursor();
            }
        }
    }

    fn key(&mut self, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.done = true,
            KeyCode::Char('c') if control => self.done = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.page_cursor(true),
            KeyCode::PageUp | KeyCode::Char('b') => self.page_cursor(false),
            KeyCode::Char('g') | KeyCode::Home => self.move_cursor(isize::MIN),
            KeyCode::Char('G') | KeyCode::End => self.move_cursor(isize::MAX),
            KeyCode::Char('[' | 's') => {
                self.selection.mark_start();
                self.notice = None;
            }
            KeyCode::Char(']' | 'e') => {
                self.selection.mark_end();
                self.notice = None;
            }
            KeyCode::Enter => self.confirm(),
            _ => {}
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        self.selection.move_cursor(delta);
        self.notice = None;
        self.focus_cursor();
    }

    fn page_cursor(&mut self, forward: bool) {
        let rows = self
            .message_starts
            .iter()
            .filter_map(|line| self.line_first_row.get(*line).copied())
            .collect::<Vec<_>>();
        let current = self.selection.cursor;
        let Some(&current_row) = rows.get(current) else {
            return;
        };
        let next = if forward {
            let target = current_row.saturating_add(self.rows);
            rows.partition_point(|row| *row <= target)
                .saturating_sub(1)
                .max(current.saturating_add(1))
                .min(self.selection.total - 1)
        } else {
            let target = current_row.saturating_sub(self.rows);
            rows.partition_point(|row| *row < target)
                .min(current.saturating_sub(1))
        };
        self.selection.cursor = next;
        self.notice = None;
        self.focus_cursor();
    }

    fn focus_cursor(&mut self) {
        let Some(&line) = self.message_starts.get(self.selection.cursor) else {
            return;
        };
        let Some(&row) = self.line_first_row.get(line) else {
            return;
        };
        if row < self.top {
            self.top = row;
        } else if row >= self.top + self.rows {
            self.top = row.saturating_sub(self.rows.saturating_sub(1));
        }
        let max_top = self.visual.len().saturating_sub(self.rows);
        self.top = self.top.min(max_top);
    }

    fn confirm(&mut self) {
        let span = self.selection.span();
        match self.document.validate_crop(&span) {
            Ok(()) => {
                self.result = Some(span);
                self.done = true;
            }
            Err(txcript::CropError::SplitToolPair { nearest, .. }) => {
                self.notice = Some(format!(
                    "nearest valid: #{}–{}; selection splits a tool call/result",
                    nearest.0.start + 1,
                    nearest.0.end
                ));
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
    }

    fn update_viewport(&mut self, columns: usize, rows: usize) {
        self.rows = content_rows(rows);
        let content_columns = crop_render_width(columns);
        if content_columns != self.cols {
            self.cols = content_columns;
            self.relayout();
        }
        self.focus_cursor();
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        self.update_viewport(usize::from(area.width), usize::from(area.height));
        let selected = self.selection.span();
        let selected_start_row = self
            .message_starts
            .get(selected.0.start)
            .and_then(|line| self.line_first_row.get(*line))
            .copied()
            .unwrap_or(0);
        let selected_end_row = self
            .message_starts
            .get(selected.0.end)
            .and_then(|line| self.line_first_row.get(*line))
            .copied()
            .unwrap_or(self.visual.len());
        let cursor_row = self
            .message_starts
            .get(self.selection.cursor)
            .and_then(|line| self.line_first_row.get(*line))
            .copied();
        let visible = self
            .visual
            .iter()
            .enumerate()
            .skip(self.top)
            .take(self.rows)
            .map(|(row_index, row)| {
                let line = &self.lines[row.line];
                let edge = if row_index < selected_start_row || row_index >= selected_end_row {
                    CropEdge::Outside
                } else if selected_end_row == selected_start_row + 1 {
                    CropEdge::Only
                } else if row_index == selected_start_row {
                    CropEdge::Start
                } else if row_index + 1 == selected_end_row {
                    CropEdge::End
                } else {
                    CropEdge::Middle
                };
                (
                    Line::from(crop_gutter(cursor_row == Some(row_index), edge, self.color)),
                    Line::from(row_spans(line, &row.range, &[])),
                )
            })
            .collect::<Vec<_>>();
        let gutter = Rect {
            height: area.height.saturating_sub(1),
            width: 3.min(area.width),
            ..area
        };
        let content = Rect {
            x: area.x.saturating_add(gutter.width),
            width: area.width.saturating_sub(gutter.width),
            ..gutter
        };
        frame.render_widget(
            Paragraph::new(Text::from(
                visible
                    .iter()
                    .map(|(marker, _)| marker.clone())
                    .collect::<Vec<_>>(),
            )),
            gutter,
        );
        frame.render_widget(
            Paragraph::new(Text::from(
                visible
                    .into_iter()
                    .map(|(_, line)| line)
                    .collect::<Vec<_>>(),
            )),
            content,
        );

        let status_area = Rect {
            y: area.y + area.height.saturating_sub(1),
            height: 1,
            ..area
        };
        let status = self.notice.as_ref().map_or_else(
            || crop_status(&selected, self.selection.cursor, self.color),
            |notice| {
                let style = if self.color {
                    Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().add_modifier(Modifier::BOLD)
                };
                Line::from(Span::styled(format!(" {notice}"), style))
            },
        );
        frame.render_widget(Paragraph::new(status), status_area);
    }
}

#[derive(Clone, Copy)]
enum CropEdge {
    Start,
    Middle,
    End,
    Only,
    Outside,
}

fn crop_gutter(cursor: bool, edge: CropEdge, color: bool) -> Span<'static> {
    let pointer = if cursor { '›' } else { ' ' };
    let rail = match edge {
        CropEdge::Start => '┌',
        CropEdge::Middle => '│',
        CropEdge::End => '└',
        CropEdge::Only => '─',
        CropEdge::Outside => ' ',
    };
    let mut style = if cursor {
        Style::new().add_modifier(Modifier::BOLD)
    } else if !matches!(edge, CropEdge::Outside) {
        Style::new().add_modifier(Modifier::DIM)
    } else {
        Style::new()
    };
    if color && (cursor || !matches!(edge, CropEdge::Outside)) {
        style = style.fg(Color::Cyan);
    }
    Span::styled(format!("{pointer}{rail} "), style)
}

fn crop_status(selected: &MessageSpan, cursor: usize, color: bool) -> Line<'static> {
    let accent = if color {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    };
    let key = Style::new().add_modifier(Modifier::BOLD);
    Line::from(vec![
        Span::styled(" CROP ", accent),
        Span::raw("  KEEP "),
        Span::styled(format!("#{}–{}", selected.0.start + 1, selected.0.end), key),
        Span::raw(format!(" · {}", crop_message_count(selected.0.len()))),
        Span::raw("   CURSOR "),
        Span::styled(format!("#{}", cursor + 1), key),
        Span::raw("   "),
        Span::styled("[", key),
        Span::raw(" start   "),
        Span::styled("]", key),
        Span::raw(" end   "),
        Span::styled("ENTER", key),
        Span::raw(" crop   "),
        Span::styled("Q", key),
        Span::raw(" cancel"),
    ])
}

pub(crate) fn crop_render_width(columns: usize) -> usize {
    columns.saturating_sub(3).clamp(1, 120)
}

fn crop_message_count(count: usize) -> String {
    let noun = if count == 1 { "message" } else { "messages" };
    format!("{count} {noun}")
}

/// How many screen rows `text` (the renderer's ANSI output) takes at
/// `columns` wide, wrapped the way the pager wraps it.
pub(crate) fn visual_rows(text: &str, columns: usize) -> usize {
    text.lines()
        .map(|line| wrap(&parse_ansi(line).plain, columns).count())
        .sum()
}

/// Rows left for content once the status line has its own.
const fn content_rows(rows: usize) -> usize {
    if rows > 1 { rows - 1 } else { 1 }
}

/// A line parsed from the renderer's ANSI output: the styled runs, and the
/// plain text search and highlighting index into.
#[derive(Debug, Default, PartialEq, Eq)]
struct Styled {
    plain: String,
    /// `(byte range in plain, style)` runs covering `plain` in order.
    runs: Vec<(std::ops::Range<usize>, Style)>,
}

/// Parse the SGR sequences the renderer emits (`ESC [ … m`) into styled
/// runs. Anything else after an escape is dropped through the next `m`.
fn parse_ansi(line: &str) -> Styled {
    let mut styled = Styled::default();
    let mut style = Style::new();
    let mut run_start = 0usize;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            let mut params = String::new();
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
                params.push(next);
            }
            if styled.plain.len() > run_start {
                styled.runs.push((run_start..styled.plain.len(), style));
            }
            run_start = styled.plain.len();
            style = apply_sgr(style, &params);
        } else if ch == '\t' {
            // The cell grid has no tab stops.
            styled.plain.push_str("    ");
        } else {
            styled.plain.push(ch);
        }
    }
    if styled.plain.len() > run_start {
        styled.runs.push((run_start..styled.plain.len(), style));
    }
    styled
}

/// `style` after the SGR parameter list `params` (e.g. `1;36`, `38;2;1;2;3`).
fn apply_sgr(mut style: Style, params: &str) -> Style {
    let mut codes = params.split(';').map(|code| code.parse::<u8>().ok());
    while let Some(code) = codes.next() {
        style = match code {
            Some(0) | None => Style::new(),
            Some(1) => style.add_modifier(Modifier::BOLD),
            Some(2) => style.add_modifier(Modifier::DIM),
            Some(7) => style.add_modifier(Modifier::REVERSED),
            Some(30) => style.fg(Color::Black),
            Some(31) => style.fg(Color::Red),
            Some(32) => style.fg(Color::Green),
            Some(33) => style.fg(Color::Yellow),
            Some(34) => style.fg(Color::Blue),
            Some(35) => style.fg(Color::Magenta),
            Some(36) => style.fg(Color::Cyan),
            Some(37) => style.fg(Color::Gray),
            Some(38) => match codes.next().flatten() {
                Some(5) => match codes.next().flatten() {
                    Some(index) => style.fg(Color::Indexed(index)),
                    None => style,
                },
                Some(2) => {
                    let mut rgb = [0u8; 3];
                    for channel in &mut rgb {
                        *channel = codes.next().flatten().unwrap_or(0);
                    }
                    style.fg(Color::Rgb(rgb[0], rgb[1], rgb[2]))
                }
                _ => style,
            },
            Some(39) => style.fg(Color::Reset),
            _ => style,
        };
    }
    style
}

/// Case-sensitive when the query has an uppercase letter; otherwise not.
fn smart_case(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

/// Byte ranges in `text` where `query` occurs; case-insensitivity is ASCII.
fn find_all(text: &str, query: &str, case_sensitive: bool) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    if query.is_empty() || query.len() > text.len() {
        return ranges;
    }
    let (haystack, needle) = (text.as_bytes(), query.as_bytes());
    let mut at = 0;
    while at + needle.len() <= haystack.len() {
        let window = &haystack[at..at + needle.len()];
        let hit = text.is_char_boundary(at)
            && text.is_char_boundary(at + needle.len())
            && if case_sensitive {
                window == needle
            } else {
                window.eq_ignore_ascii_case(needle)
            };
        if hit {
            ranges.push(at..at + needle.len());
            at += needle.len();
        } else {
            at += 1;
        }
    }
    ranges
}

/// Byte ranges of `plain` that fit `width` columns each: broken after the
/// last space that fits, or mid-word when none does, and never before a
/// zero-width character (a combining mark stays on its base).
fn wrap(plain: &str, width: usize) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut start = 0;
    let mut used = 0;
    let mut after_space = None;
    for (at, ch) in plain.char_indices() {
        let cell = ch.width().unwrap_or(0);
        if cell > 0 && used + cell > width {
            if ch == ' ' {
                // A space that would overflow ends the row and is dropped.
                rows.push(start..at);
                start = at + ch.len_utf8();
                used = 0;
                after_space = None;
                continue;
            }
            let end = after_space.filter(|end| *end > start).unwrap_or(at);
            rows.push(start..end);
            start = end;
            after_space = None;
            used = plain[start..at].width() + cell;
        } else {
            used += cell;
        }
        if ch == ' ' {
            after_space = Some(at + 1);
        }
    }
    rows.push(start..plain.len());
    rows.into_iter()
}

/// The runs of `line` within `row` as spans, with `hits` (search matches,
/// in the line's byte offsets) drawn reversed.
fn row_spans<'a>(
    line: &'a Styled,
    row: &std::ops::Range<usize>,
    hits: &[std::ops::Range<usize>],
) -> Vec<Span<'a>> {
    let mut spans = Vec::new();
    for (run, style) in &line.runs {
        let run = run.start.max(row.start)..run.end.min(row.end);
        if run.start >= run.end {
            continue;
        }
        let mut at = run.start;
        for hit in hits {
            let start = hit.start.max(run.start);
            let end = hit.end.min(run.end);
            if start >= end {
                continue;
            }
            if at < start {
                spans.push(Span::styled(&line.plain[at..start], *style));
            }
            spans.push(Span::styled(
                &line.plain[start..end],
                style.add_modifier(Modifier::REVERSED),
            ));
            at = end;
        }
        if at < run.end {
            spans.push(Span::styled(&line.plain[at..run.end], *style));
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_sequences_become_styled_runs_over_plain_text() {
        let line = parse_ansi("\x1b[1;36m── Message #1\x1b[0m plain \x1b[38;2;9;8;7mX\x1b[0m");
        assert_eq!(line.plain, "── Message #1 plain X");
        assert_eq!(line.runs.len(), 3);
        assert_eq!(
            line.runs[0].1,
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        );
        assert_eq!(line.runs[1].1, Style::new());
        assert_eq!(line.runs[2].1, Style::new().fg(Color::Rgb(9, 8, 7)));
        assert_eq!(&line.plain[line.runs[2].0.clone()], "X");
        assert_eq!(parse_ansi("").runs, Vec::new());
        assert_eq!(
            parse_ansi("\x1b[2mdim\x1b[0m").runs[0].1,
            Style::new().add_modifier(Modifier::DIM)
        );
    }

    #[test]
    fn placeholder_cells_survive_parsing_as_plain_text() {
        let row = "\x1b[38;2;1;2;3m\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\u{0305}\u{030D}\x1b[0m";
        let line = parse_ansi(row);
        assert_eq!(line.plain.chars().count(), 6);
        assert_eq!(line.runs.len(), 1);
        assert_eq!(line.runs[0].1, Style::new().fg(Color::Rgb(1, 2, 3)));
    }

    #[test]
    fn search_is_smart_case_and_finds_every_occurrence() {
        assert!(!smart_case("message"));
        assert!(smart_case("Message"));
        assert_eq!(
            find_all("Message message MESSAGE", "message", false),
            vec![0..7, 8..15, 16..23]
        );
        assert_eq!(
            find_all("Message message MESSAGE", "Message", true),
            vec![0..7]
        );
        assert_eq!(find_all("héllo héllo", "héllo", false), vec![0..6, 7..13]);
        assert!(find_all("short", "much longer", false).is_empty());
        assert!(find_all("anything", "", false).is_empty());
    }

    #[test]
    fn highlights_split_runs_at_match_boundaries() {
        let line = parse_ansi("\x1b[1mbold text\x1b[0m and more");
        let spans = row_spans(
            &line,
            &(0..line.plain.len()),
            &find_all(&line.plain, "text and", true),
        );
        let pieces: Vec<(&str, bool)> = spans
            .iter()
            .map(|span| {
                (
                    span.content.as_ref(),
                    span.style.add_modifier.contains(Modifier::REVERSED),
                )
            })
            .collect();
        assert_eq!(
            pieces,
            vec![
                ("bold ", false),
                ("text", true),
                (" and", true),
                (" more", false)
            ]
        );
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert!(!spans[2].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn wrapping_breaks_at_spaces_then_mid_word_and_keeps_marks_on_their_base() {
        let rows = |text: &str, width| -> Vec<String> {
            wrap(text, width)
                .map(|range| text[range].to_string())
                .collect()
        };
        assert_eq!(rows("", 10), vec![""]);
        assert_eq!(rows("short", 10), vec!["short"]);
        assert_eq!(
            rows("the quick brown fox", 10),
            vec!["the quick ", "brown fox"]
        );
        // A space that fits stays at the row's end; one that would not is dropped.
        assert_eq!(rows("exactly10 x", 10), vec!["exactly10 ", "x"]);
        assert_eq!(rows("exactly-10 x", 10), vec!["exactly-10", "x"]);
        assert_eq!(
            rows("abcdefghijklmnop", 5),
            vec!["abcde", "fghij", "klmno", "p"]
        );
        // Combining marks (zero width) never start a row.
        let cells = "\u{10EEEE}\u{0305}\u{0305}".repeat(4);
        assert_eq!(
            rows(&cells, 3),
            vec![
                "\u{10EEEE}\u{0305}\u{0305}".repeat(3),
                "\u{10EEEE}\u{0305}\u{0305}".to_string()
            ]
        );
        // Tabs were expanded at parse time, so a wrapped code line stays aligned.
        assert_eq!(parse_ansi("\tx").plain, "    x");
        assert_eq!(visual_rows("\x1b[1mone two three\x1b[0m\nfour\n", 8), 3);
    }

    #[test]
    fn wrapped_rows_carry_scrolling_search_and_message_jumps() {
        let text = "one two three four five six\nMESSAGE rule\nseven eight nine ten\n";
        let mut pager = pager(text, vec![1]);
        pager.cols = 10;
        pager.relayout();
        // "one two three four five six" → 3 rows, rule → 2, last → 3.
        assert_eq!(pager.visual.len(), 8);
        assert_eq!(pager.line_first_row, vec![0, 3, 5]);
        press(&mut pager, KeyCode::Char(']'), 4);
        assert_eq!(
            pager.top, 3,
            "the message rule's first row, clamped to max top"
        );
        press(&mut pager, KeyCode::Char('/'), 4);
        for ch in "nine".chars() {
            press(&mut pager, KeyCode::Char(ch), 4);
        }
        press(&mut pager, KeyCode::Enter, 4);
        assert_eq!(pager.search.matches, vec![6], "the row the hit starts on");
    }

    fn pager(text: &str, message_starts: Vec<usize>) -> Pager {
        let document = Document::new(
            txcript::Transcript::new(
                txcript::common::Meta {
                    id: "s".into(),
                    timestamp: chrono::DateTime::UNIX_EPOCH,
                    cwd: None,
                    git_branch: None,
                    title: None,
                    cli_version: None,
                    model: None,
                },
                Vec::new(),
            ),
            txcript::Span(0..0),
            false,
            None,
        );
        Pager::new(
            document,
            Rendered {
                text: text.to_string(),
                transmissions: Vec::new(),
                message_starts,
            },
            80,
        )
    }

    fn press(pager: &mut Pager, code: KeyCode, rows: usize) {
        pager.rows = rows;
        pager.key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn scrolling_and_message_jumps_stay_in_bounds() {
        let text = (0..30)
            .map(|n| format!("line {n}\n"))
            .collect::<Vec<_>>()
            .concat();
        let mut pager = pager(&text, vec![5, 12, 20]);
        press(&mut pager, KeyCode::Char('j'), 10);
        assert_eq!(pager.top, 1);
        press(&mut pager, KeyCode::Char(']'), 10);
        assert_eq!(pager.top, 5);
        press(&mut pager, KeyCode::Char(']'), 10);
        assert_eq!(pager.top, 12);
        press(&mut pager, KeyCode::Char('['), 10);
        assert_eq!(pager.top, 5);
        press(&mut pager, KeyCode::Char('G'), 10);
        assert_eq!(pager.top, 20);
        press(&mut pager, KeyCode::Char(' '), 10);
        assert_eq!(pager.top, 20);
        press(&mut pager, KeyCode::Char('g'), 10);
        assert_eq!(pager.top, 0);
        press(&mut pager, KeyCode::Char('q'), 10);
        assert!(pager.quit);
    }

    #[test]
    fn the_search_prompt_takes_shifted_characters_and_steps_through_matches() {
        let text = "alpha\nBeta_1\ngamma\nbeta_2\ndelta\n";
        let mut pager = pager(text, Vec::new());
        press(&mut pager, KeyCode::Char('/'), 3);
        for ch in "Beta_".chars() {
            pager.key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::SHIFT));
        }
        assert_eq!(pager.prompt.as_deref(), Some("Beta_"));
        press(&mut pager, KeyCode::Enter, 3);
        assert_eq!(pager.search.query, "Beta_");
        // Uppercase in the query: case-sensitive, so only line 1 matches.
        assert_eq!(pager.search.matches, vec![1]);
        assert_eq!(pager.top, 1);

        press(&mut pager, KeyCode::Char('/'), 3);
        for ch in "beta_".chars() {
            press(&mut pager, KeyCode::Char(ch), 3);
        }
        press(&mut pager, KeyCode::Enter, 3);
        assert_eq!(pager.search.matches, vec![1, 3]);
        press(&mut pager, KeyCode::Char('n'), 3);
        assert_eq!(pager.search.current, Some(1));
        assert_eq!(pager.top, 2, "the last screen starts at line 2");
        press(&mut pager, KeyCode::Char('n'), 3);
        assert_eq!(pager.search.current, Some(0));
        press(&mut pager, KeyCode::Esc, 3);
        assert!(pager.search.query.is_empty());
        assert!(!pager.quit);
        press(&mut pager, KeyCode::Esc, 3);
        assert!(pager.quit);
    }

    #[test]
    fn crop_selection_defaults_to_the_full_session_and_clamps_navigation() {
        let mut selection = CropSelection::new(4, None).unwrap();
        assert_eq!(selection.span(), txcript::Span(0..4));
        assert_eq!(selection.cursor, 0);

        selection.move_cursor(-1);
        assert_eq!(selection.cursor, 0);
        selection.move_cursor(99);
        assert_eq!(selection.cursor, 3);
    }

    #[test]
    fn crop_selection_marks_and_normalizes_both_edges() {
        let mut selection = CropSelection::new(5, Some(txcript::Span(1..4))).unwrap();
        assert_eq!(selection.span(), txcript::Span(1..4));

        selection.cursor = 3;
        selection.mark_start();
        assert_eq!(selection.span(), txcript::Span(3..4));

        selection.cursor = 1;
        selection.mark_end();
        assert_eq!(selection.span(), txcript::Span(1..2));
    }

    #[test]
    fn crop_selection_rejects_empty_sessions_and_invalid_initial_ranges() {
        assert!(CropSelection::new(0, None).is_err());
        assert!(CropSelection::new(3, Some(txcript::Span(2..2))).is_err());
        assert!(CropSelection::new(3, Some(txcript::Span(1..4))).is_err());
    }

    #[test]
    fn cropper_marks_a_range_confirms_it_and_can_cancel() {
        let mut editor = cropper(4);
        editor.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        editor.key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE));
        editor.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        editor.key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE));
        editor.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(editor.done);
        assert_eq!(editor.result, Some(txcript::Span(1..3)));

        let mut cancelled = cropper(2);
        cancelled.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(cancelled.done);
        assert_eq!(cancelled.result, None);
    }

    #[test]
    fn cropper_keeps_the_cursor_visible_when_the_terminal_height_changes() {
        let mut editor = cropper(4);
        editor.selection.cursor = 3;
        editor.update_viewport(80, 2);

        let line = editor.message_starts[editor.selection.cursor];
        let row = editor.line_first_row[line];
        assert!(row >= editor.top);
        assert!(row < editor.top + editor.rows);
    }

    #[test]
    fn cropper_pages_by_rendered_rows_instead_of_message_count() {
        let mut editor = cropper(20);
        editor.update_viewport(80, 5);

        editor.key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));

        assert!(editor.selection.cursor > 0);
        assert!(editor.selection.cursor <= 2);
        editor.key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(editor.selection.cursor, 0);
    }

    #[test]
    fn crop_selection_uses_a_gutter_without_restyling_transcript_text() {
        let start = crop_gutter(true, CropEdge::Start, true);
        let selected = crop_gutter(false, CropEdge::Middle, true);
        let end = crop_gutter(false, CropEdge::End, true);
        let outside = crop_gutter(true, CropEdge::Outside, true);

        assert_eq!(start.content.as_ref(), "›┌ ");
        assert_eq!(selected.content.as_ref(), " │ ");
        assert_eq!(end.content.as_ref(), " └ ");
        assert_eq!(outside.content.as_ref(), "›  ");
        for marker in [start, selected, end, outside] {
            assert_eq!(marker.style.bg, None);
        }
    }

    #[test]
    fn crop_gutter_uses_the_actual_available_terminal_width() {
        assert_eq!(crop_render_width(2), 1);
        assert_eq!(crop_render_width(3), 1);
        assert_eq!(crop_render_width(39), 36);
        assert_eq!(crop_render_width(40), 37);
        assert_eq!(crop_render_width(41), 38);
        assert_eq!(crop_render_width(42), 39);
        assert_eq!(crop_render_width(80), 77);
        assert_eq!(crop_render_width(200), 120);
    }

    #[test]
    fn crop_status_pluralizes_message_count() {
        assert_eq!(crop_message_count(1), "1 message");
        assert_eq!(crop_message_count(2), "2 messages");
    }

    #[test]
    fn terminal_restoration_failure_blocks_the_crop_result() {
        let result = Ok(Some(txcript::Span(1..2)));
        let restore = Err(std::io::Error::other("restore failed"));

        let error = finish_after_restore(result, restore).unwrap_err();

        assert!(error.contains("restoring the terminal"));
        assert!(error.contains("restore failed"));
    }

    fn cropper(message_count: usize) -> Cropper {
        let body = (0..message_count)
            .map(|index| txcript::common::Message {
                role: if index % 2 == 0 {
                    txcript::common::Role::User
                } else {
                    txcript::common::Role::Assistant
                },
                content: vec![txcript::common::Block::Text {
                    text: format!("message {index}"),
                }],
                timestamp: chrono::DateTime::UNIX_EPOCH,
                model: None,
                stop_reason: None,
                usage: None,
            })
            .collect::<Vec<_>>();
        let common = txcript::Transcript::new(
            txcript::common::Meta {
                id: "crop-test".into(),
                timestamp: chrono::DateTime::UNIX_EPOCH,
                cwd: None,
                git_branch: None,
                title: None,
                cli_version: None,
                model: None,
            },
            body,
        );
        let mut document = Document::new(common, txcript::Span(0..message_count), false, None);
        let rendered = document.render(80, Filters::default()).unwrap();
        Cropper::new(document, rendered, 80, None).unwrap()
    }
}
