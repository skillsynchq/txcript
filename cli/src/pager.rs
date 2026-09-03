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
//! The crop editor (`txcript crop`) shares the viewport and adds an edit:
//! which messages are kept. Space removes or restores the message at the
//! cursor; `v` (or `[` and `]`) opens a selection that `x`, `r`, and `t`
//! remove, restore, or keep alone; `:N` and `:A-B` go to and select by
//! number; `e` opens the message in the user's editor (in a pane, or with
//! `E` in the whole terminal) and applies the file it saves; `u`/`U` undo
//! and redo both cuts and edits; `?` shows every key. Removed messages
//! collapse to their header, an overview shows the whole session one cell
//! per message, and Enter saves the kept runs of the edited session.
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
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use tui_term::widget::{Cursor, PseudoTerminal};
use txcript::common::Message;
use txcript::{Common, Span as MessageSpan, Transcript};
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

use crate::view::{Document, Filters, MessageKind, Rendered, render_width};
use crate::{draft, editpane};

/// Page `first`, the document rendered at `width` under the default
/// filters, until the user quits.
pub fn run(document: Document, first: Rendered, width: usize) -> Result<(), String> {
    let (mut terminal, cleanup) = init_terminal("pager")?;
    let result = Pager::new(document, first, width).run(&mut terminal);
    finish_after_restore(result, cleanup.try_restore())
}

/// What the crop editor produced: the session as edited, and the runs of
/// it to keep.
pub struct Cropped {
    pub common: Transcript<Common>,
    pub spans: Vec<MessageSpan>,
    /// How many messages were edited in place.
    pub edited: usize,
}

/// Interactively edit `document`: which messages to keep, and their text;
/// `initial` starts with only that range kept. Enter confirms; q, escape,
/// and ctrl-c cancel without writing anything.
pub fn crop(
    document: Document,
    first: Rendered,
    width: usize,
    initial: Option<MessageSpan>,
) -> Result<Option<Cropped>, String> {
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

/// The editing state of a crop, independent of terminal rendering so it can
/// be tested without a TTY: which messages are kept, where the cursor is,
/// what is selected, and the history behind it.
///
/// Edits snap to tool pairs: removing or restoring one half of a call and
/// its result takes the other half along, so a save can never split them.
#[derive(Debug, Clone, PartialEq)]
struct CropEdit {
    kept: Vec<bool>,
    cursor: usize,
    /// The fixed end of the selection while one is open; the cursor is the
    /// moving end.
    anchor: Option<usize>,
    /// For each message, the messages that stay with it: a tool call's
    /// result, a result's call.
    links: Vec<Vec<usize>>,
    undo: Vec<Step>,
    redo: Vec<Step>,
}

/// One entry of the edit history.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// The kept set before (in the undo stack) or after (in the redo stack).
    Kept(Vec<bool>),
    /// A message's text changed.
    Message {
        index: usize,
        before: Box<Message>,
        after: Box<Message>,
    },
}

/// What undoing or redoing a step asks of the document.
#[derive(Debug, Clone, PartialEq)]
enum Reverted {
    Kept,
    Message { index: usize, message: Message },
}

impl CropEdit {
    fn new(total: usize, pairs: &[(usize, usize)]) -> Result<Self, String> {
        if total == 0 {
            return Err("cannot crop an empty session".to_string());
        }
        let mut links = vec![Vec::new(); total];
        for &(tool_use, tool_result) in pairs {
            if tool_use < total && tool_result < total {
                links[tool_use].push(tool_result);
                links[tool_result].push(tool_use);
            }
        }
        Ok(Self {
            kept: vec![true; total],
            cursor: 0,
            anchor: None,
            links,
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }

    fn total(&self) -> usize {
        self.kept.len()
    }

    fn kept_count(&self) -> usize {
        self.kept.iter().filter(|kept| **kept).count()
    }

    fn removed_count(&self) -> usize {
        self.total() - self.kept_count()
    }

    /// Whether there is anything to undo: the state differs from what the
    /// editor opened with.
    fn edited(&self) -> bool {
        !self.undo.is_empty()
    }

    const fn selecting(&self) -> bool {
        self.anchor.is_some()
    }

    /// The messages an edit acts on: the selection, or just the cursor.
    fn selection(&self) -> std::ops::Range<usize> {
        let anchor = self.anchor.unwrap_or(self.cursor);
        anchor.min(self.cursor)..anchor.max(self.cursor) + 1
    }

    fn move_cursor(&mut self, delta: isize) {
        self.cursor = self
            .cursor
            .saturating_add_signed(delta)
            .min(self.total() - 1);
    }

    fn jump_to(&mut self, index: usize) {
        self.cursor = index.min(self.total() - 1);
    }

    fn select(&mut self, range: std::ops::Range<usize>) {
        let end = range.end.min(self.total());
        if range.start >= end {
            return;
        }
        self.anchor = Some(range.start);
        self.cursor = end - 1;
    }

    fn toggle_selecting(&mut self) {
        self.anchor = if self.anchor.is_some() {
            None
        } else {
            Some(self.cursor)
        };
    }

    fn mark_start(&mut self) {
        self.anchor = Some(self.cursor);
    }

    /// End the selection at the cursor; without a start it runs from the
    /// first message.
    fn mark_end(&mut self) {
        if self.anchor.is_none() {
            self.anchor = Some(0);
        }
    }

    fn clear_selection(&mut self) {
        self.anchor = None;
    }

    /// `range` plus every message linked to it, transitively, sorted.
    fn closure(&self, range: std::ops::Range<usize>) -> Vec<usize> {
        let mut included = vec![false; self.total()];
        let mut queue: std::collections::VecDeque<usize> = range.clone().collect();
        for index in range {
            included[index] = true;
        }
        while let Some(index) = queue.pop_front() {
            for &other in &self.links[index] {
                if !included[other] {
                    included[other] = true;
                    queue.push_back(other);
                }
            }
        }
        included
            .iter()
            .enumerate()
            .filter(|(_, included)| **included)
            .map(|(index, _)| index)
            .collect()
    }

    /// Make `next` the current state, remembering the old one; a no-op
    /// change leaves the history alone.
    fn commit(&mut self, next: Vec<bool>) -> bool {
        if next == self.kept {
            return false;
        }
        let before = std::mem::replace(&mut self.kept, next);
        self.record(Step::Kept(before));
        true
    }

    fn record(&mut self, step: Step) {
        self.undo.push(step);
        if self.undo.len() > 500 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Remember that a message's text changed, so it can be undone.
    fn record_message(&mut self, index: usize, before: Message, after: Message) {
        self.record(Step::Message {
            index,
            before: Box::new(before),
            after: Box::new(after),
        });
    }

    /// Set the selection (and what is linked to it) to `keep`; the
    /// selection closes. Returns the linked messages that changed along
    /// with it, so the editor can say so.
    fn set_selection(&mut self, keep: bool) -> Vec<usize> {
        let selection = self.selection();
        let mut next = self.kept.clone();
        let mut extra = Vec::new();
        for index in self.closure(selection.clone()) {
            if !selection.contains(&index) && next[index] != keep {
                extra.push(index);
            }
            next[index] = keep;
        }
        self.commit(next);
        self.anchor = None;
        extra
    }

    fn cut(&mut self) -> Vec<usize> {
        self.set_selection(false)
    }

    fn restore(&mut self) -> Vec<usize> {
        self.set_selection(true)
    }

    /// Remove what is kept, restore what is removed: a selection that is
    /// entirely removed comes back, anything else goes.
    fn toggle(&mut self) -> (bool, Vec<usize>) {
        let all_removed = self.selection().all(|index| !self.kept[index]);
        if all_removed {
            (true, self.restore())
        } else {
            (false, self.cut())
        }
    }

    /// Keep the selection and what is linked to it; remove everything
    /// else. Returns the linked messages kept along with it.
    fn keep_only(&mut self) -> Vec<usize> {
        let selection = self.selection();
        let mut next = vec![false; self.total()];
        let mut extra = Vec::new();
        for index in self.closure(selection.clone()) {
            if !selection.contains(&index) {
                extra.push(index);
            }
            next[index] = true;
        }
        self.commit(next);
        self.anchor = None;
        extra
    }

    fn undo(&mut self) -> Option<Reverted> {
        let step = self.undo.pop()?;
        self.anchor = None;
        Some(match step {
            Step::Kept(before) => {
                self.redo
                    .push(Step::Kept(std::mem::replace(&mut self.kept, before)));
                Reverted::Kept
            }
            Step::Message {
                index,
                before,
                after,
            } => {
                let message = (*before).clone();
                self.redo.push(Step::Message {
                    index,
                    before,
                    after,
                });
                Reverted::Message { index, message }
            }
        })
    }

    fn redo(&mut self) -> Option<Reverted> {
        let step = self.redo.pop()?;
        self.anchor = None;
        Some(match step {
            Step::Kept(after) => {
                self.undo
                    .push(Step::Kept(std::mem::replace(&mut self.kept, after)));
                Reverted::Kept
            }
            Step::Message {
                index,
                before,
                after,
            } => {
                let message = (*after).clone();
                self.undo.push(Step::Message {
                    index,
                    before,
                    after,
                });
                Reverted::Message { index, message }
            }
        })
    }

    /// The kept messages as maximal runs, in order.
    fn kept_spans(&self) -> Vec<MessageSpan> {
        let mut spans = Vec::new();
        let mut start = None;
        for (index, kept) in self.kept.iter().enumerate() {
            match (kept, start) {
                (true, None) => start = Some(index),
                (false, Some(from)) => {
                    spans.push(MessageSpan(from..index));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(from) = start {
            spans.push(MessageSpan(from..self.total()));
        }
        spans
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
///
/// Removed messages collapse to their header, dimmed, so the whole edit
/// stays readable at a glance; the timeline above the status line is the
/// same edit one cell per message.
struct Cropper {
    document: Document,
    color: bool,
    width: usize,
    lines: Vec<Styled>,
    message_starts: Vec<usize>,
    kinds: Vec<MessageKind>,
    visual: Vec<Row>,
    line_first_row: Vec<usize>,
    cols: usize,
    top: usize,
    rows: usize,
    /// Where the overview goes: beside the text when the window is wider
    /// than the text needs, under it otherwise.
    overview: Overview,
    edit: CropEdit,
    /// The messages as the editor opened them, to tell edited ones apart.
    original: Vec<Message>,
    edited: Vec<bool>,
    /// A message out in the user's editor.
    editing: Option<Editing>,
    /// The editor itself, while `editing`.
    editor: Option<Editor>,
    /// An editor to run in the terminal itself, once the screen is free.
    takeover: Option<String>,
    /// The screen as of the last draw, for sizing a pane before it opens.
    area: (u16, u16),
    pending_images: Vec<Vec<u8>>,
    notice: Option<String>,
    /// The `:` prompt being typed, if open.
    prompt: Option<String>,
    mode: Mode,
    result: Option<Vec<MessageSpan>>,
    done: bool,
}

impl Cropper {
    fn new(
        document: Document,
        first: Rendered,
        width: usize,
        initial: Option<MessageSpan>,
    ) -> Result<Self, String> {
        let pairs = document.tool_pairs().map_err(|error| error.to_string())?;
        let mut edit = CropEdit::new(document.message_count(), &pairs)?;
        let mut notice = None;
        if let Some(span) = initial {
            if span.0.start >= span.0.end || span.0.end > edit.total() {
                return Err(format!(
                    "invalid initial crop range {}..{} for a session with {} messages",
                    span.0.start,
                    span.0.end,
                    edit.total()
                ));
            }
            edit.select(span.0.clone());
            let extra = edit.keep_only();
            notice = snapped_notice("kept", &extra);
            // The range is where the edit starts, not an edit to undo.
            edit.undo.clear();
            edit.jump_to(span.0.start);
        }
        let color = document.color_enabled();
        let kinds = document.message_kinds();
        let mut cropper = Self {
            document,
            color,
            width,
            lines: Vec::new(),
            message_starts: Vec::new(),
            kinds,
            visual: Vec::new(),
            line_first_row: Vec::new(),
            cols: width,
            top: 0,
            rows: crop_content_rows(24),
            overview: Overview::Bottom,
            edit,
            original: Vec::new(),
            edited: Vec::new(),
            editing: None,
            editor: None,
            takeover: None,
            area: (80, 24),
            pending_images: Vec::new(),
            notice,
            prompt: None,
            mode: Mode::Editing,
            result: None,
            done: false,
        };
        cropper.original = cropper.document.messages().to_vec();
        cropper.edited = vec![false; cropper.original.len()];
        cropper.accept(first);
        cropper.focus_cursor();
        Ok(cropper)
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<Option<Cropped>, String> {
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
            if self.editor.is_some() {
                // The editor paints and exits on its own schedule: look in
                // on it between keys.
                let ready = event::poll(std::time::Duration::from_millis(16))
                    .map_err(|error| format!("reading input: {error}"))?;
                if !ready {
                    self.poll_editor();
                    continue;
                }
            }
            match event::read().map_err(|error| format!("reading input: {error}"))? {
                Event::Key(key) if key.kind != KeyEventKind::Release => match &mut self.editor {
                    Some(Editor::Pane(pane)) => pane.key(&key),
                    Some(Editor::Window(_)) => {
                        if key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            self.stop_waiting();
                        }
                    }
                    None => self.key(key),
                },
                Event::Paste(text) => match &mut self.editor {
                    Some(Editor::Pane(pane)) => pane.paste(&text),
                    Some(Editor::Window(_)) => {}
                    None => {
                        if let Some(prompt) = &mut self.prompt {
                            prompt.push_str(&text);
                        }
                    }
                },
                _ => {}
            }
            if let Some(command) = self.takeover.take() {
                self.run_takeover(terminal, &command)?;
            }
            self.poll_editor();
        }
        Ok(self.result.take().map(|spans| Cropped {
            common: self.document.transcript().clone(),
            spans,
            edited: self.edited.iter().filter(|edited| **edited).count(),
        }))
    }

    /// Open the message at the cursor in the user's editor.
    fn begin_edit(&mut self, how: How) {
        let index = self.edit.cursor;
        if !self.edit.kept[index] {
            self.notice = Some(format!("#{} is removed · restore it to edit it", index + 1));
            return;
        }
        let Some(text) = self.document.message(index).and_then(draft::draft) else {
            self.notice = Some(format!("nothing in #{} can be edited", index + 1));
            return;
        };
        let path = std::env::temp_dir().join(format!(
            "txcript-{}-message-{}.md",
            std::process::id(),
            index + 1
        ));
        if let Err(error) = std::fs::write(&path, &text) {
            self.notice = Some(format!("cannot write the message to edit: {error}"));
            return;
        }
        let command = editpane::editor_command();
        let editing = Editing {
            index,
            path,
            original: text,
            name: editpane::editor_name(&command),
        };
        if how == How::Takeover {
            self.editing = Some(editing);
            self.takeover = Some(command);
            self.notice = None;
            return;
        }
        let editor = if editpane::opens_a_window(&command) {
            editpane::Detached::open(&command, &editing.path).map(Editor::Window)
        } else {
            let (width, height) = self.area;
            let (rows, cols) = Self::layout(usize::from(width), usize::from(height), true)
                .pane
                .map_or((height, width), |pane| pane_inner(pane, self.overview));
            editpane::Pane::open(&command, &editing.path, rows, cols).map(Editor::Pane)
        };
        match editor {
            Ok(editor) => {
                self.editor = Some(editor);
                self.editing = Some(editing);
                self.notice = None;
            }
            Err(error) => {
                let _ = std::fs::remove_file(&editing.path);
                self.notice = Some(error);
            }
        }
    }

    /// Hand the terminal to the editor until it exits, then take it back
    /// and apply the file.
    fn run_takeover(
        &mut self,
        terminal: &mut DefaultTerminal,
        command: &str,
    ) -> Result<(), String> {
        let Some(editing) = self.editing.take() else {
            return Ok(());
        };
        let leave = disable_raw_mode()
            .and_then(|()| execute!(std::io::stdout(), LeaveAlternateScreen))
            .map_err(|error| format!("handing the terminal to the editor: {error}"));
        let outcome = match leave {
            Ok(()) => editpane::run_in_terminal(command, &editing.path),
            Err(error) => Err(error),
        };
        enable_raw_mode()
            .and_then(|()| execute!(std::io::stdout(), EnterAlternateScreen))
            .map_err(|error| format!("taking the terminal back from the editor: {error}"))?;
        terminal
            .clear()
            .map_err(|error| format!("redrawing after the editor: {error}"))?;
        self.conclude_edit(&editing, outcome);
        self.relayout();
        self.focus_cursor();
        Ok(())
    }

    /// Notice whether the editor has exited, and take its result.
    fn poll_editor(&mut self) {
        let finished = match &mut self.editor {
            Some(Editor::Pane(pane)) => pane.finished(),
            Some(Editor::Window(window)) => window.finished(),
            None => None,
        };
        let Some(outcome) = finished else {
            return;
        };
        self.editor = None;
        if let Some(editing) = self.editing.take() {
            self.conclude_edit(&editing, outcome);
            self.relayout();
            self.focus_cursor();
        }
    }

    /// Give up on a windowed editor that has not closed; the file is left
    /// alone.
    fn stop_waiting(&mut self) {
        self.editor = None;
        if let Some(editing) = self.editing.take() {
            self.notice = Some(format!(
                "stopped waiting for {} · #{} is unchanged",
                editing.name,
                editing.index + 1
            ));
        }
    }

    /// Apply what the editor left in the file, and say what happened.
    fn conclude_edit(&mut self, editing: &Editing, outcome: Result<(), String>) {
        let ordinal = editing.index + 1;
        let text = match outcome.and_then(|()| {
            std::fs::read_to_string(&editing.path)
                .map_err(|error| format!("reading the edited message: {error}"))
        }) {
            Ok(text) => text,
            Err(error) => {
                let _ = std::fs::remove_file(&editing.path);
                self.notice = Some(format!("#{ordinal} not changed · {error}"));
                return;
            }
        };
        if text == editing.original {
            let _ = std::fs::remove_file(&editing.path);
            self.notice = Some(format!("#{ordinal} unchanged"));
            return;
        }
        let Some(before) = self.document.message(editing.index).cloned() else {
            return;
        };
        match draft::apply(&before, &text) {
            Ok(after) if after == before => {
                let _ = std::fs::remove_file(&editing.path);
                self.notice = Some(format!("#{ordinal} unchanged"));
            }
            Ok(after) => {
                let _ = std::fs::remove_file(&editing.path);
                self.edit
                    .record_message(editing.index, before, after.clone());
                self.put_message(editing.index, after);
                self.notice = Some(format!("#{ordinal} edited"));
            }
            Err(error) => {
                self.notice = Some(format!(
                    "#{ordinal} not changed · {error} · your text is kept at {}",
                    editing.path.display()
                ));
            }
        }
    }

    /// Put `message` at `index` and show it.
    fn put_message(&mut self, index: usize, message: Message) {
        self.document.replace_message(index, message);
        if let (Some(edited), Some(original)) =
            (self.edited.get_mut(index), self.original.get(index))
        {
            *edited = self.document.message(index) != Some(original);
        }
        if let Some(rendered) = self.document.render(self.width, Filters::crop()) {
            self.accept(rendered);
        }
    }

    /// Reflect an undo or redo in the document.
    fn revert(&mut self, reverted: Option<Reverted>, missing: &str) {
        match reverted {
            None => self.after_edit(Some(missing.to_string())),
            Some(Reverted::Kept) => self.after_edit(None),
            Some(Reverted::Message { index, message }) => {
                self.put_message(index, message);
                self.after_edit(None);
            }
        }
    }

    fn accept(&mut self, rendered: Rendered) {
        self.lines = rendered.text.lines().map(parse_ansi).collect();
        self.message_starts = rendered.message_starts;
        self.pending_images.extend(rendered.transmissions);
        self.relayout();
    }

    /// The message a line belongs to: a message owns its header rule, the
    /// blank line before it, and everything up to the next message's blank
    /// line. Lines before the first message belong to none.
    fn message_of_line(&self, line: usize) -> Option<usize> {
        self.message_starts
            .partition_point(|start| start.saturating_sub(1) <= line)
            .checked_sub(1)
    }

    /// Whether a line is on screen: a removed message shows only its rule.
    fn line_shown(&self, line: usize) -> bool {
        match self.message_of_line(line) {
            Some(message) if !self.edit.kept[message] => self.message_starts[message] == line,
            _ => true,
        }
    }

    fn relayout(&mut self) {
        self.visual.clear();
        self.line_first_row.clear();
        for index in 0..self.lines.len() {
            self.line_first_row.push(self.visual.len());
            if !self.line_shown(index) {
                continue;
            }
            let plain = &self.lines[index].plain;
            self.visual
                .extend(wrap(plain, self.cols).map(|range| Row { line: index, range }));
        }
        self.top = self.top.min(self.visual.len().saturating_sub(1));
    }

    fn key(&mut self, key: KeyEvent) {
        if self.mode == Mode::Help {
            self.mode = Mode::Editing;
            return;
        }
        if self.prompt.is_some() {
            self.prompt_key(key);
            return;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let was_armed = std::mem::replace(&mut self.mode, Mode::Editing) == Mode::QuitArmed;
        match key.code {
            KeyCode::Char('c') if control => self.done = true,
            KeyCode::Esc if self.edit.selecting() => {
                self.edit.clear_selection();
                self.notice = None;
            }
            KeyCode::Char('q') | KeyCode::Esc => self.quit(was_armed),
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::PageDown | KeyCode::Char('f') => self.page_cursor(true),
            KeyCode::PageUp | KeyCode::Char('b') => self.page_cursor(false),
            KeyCode::Char('g') | KeyCode::Home => self.move_cursor(isize::MIN),
            KeyCode::Char('G') | KeyCode::End => self.move_cursor(isize::MAX),
            KeyCode::Char('}') => self.jump_user_turn(true),
            KeyCode::Char('{') => self.jump_user_turn(false),
            KeyCode::Char('v') => {
                self.edit.toggle_selecting();
                self.notice = None;
            }
            KeyCode::Char('[') => {
                self.edit.mark_start();
                self.notice = None;
            }
            KeyCode::Char(']') => {
                self.edit.mark_end();
                self.notice = None;
            }
            KeyCode::Char(' ') => {
                let (restored, extra) = self.edit.toggle();
                self.after_edit(snapped_notice(
                    if restored { "restored" } else { "removed" },
                    &extra,
                ));
            }
            KeyCode::Char('x' | 'd') | KeyCode::Delete | KeyCode::Backspace => {
                let extra = self.edit.cut();
                self.after_edit(snapped_notice("removed", &extra));
            }
            KeyCode::Char('r') if !control => {
                let extra = self.edit.restore();
                self.after_edit(snapped_notice("restored", &extra));
            }
            KeyCode::Char('t') => {
                let extra = self.edit.keep_only();
                self.after_edit(snapped_notice("kept", &extra));
            }
            KeyCode::Char('u') => {
                let reverted = self.edit.undo();
                self.revert(reverted, "nothing to undo");
            }
            KeyCode::Char('U') => {
                let reverted = self.edit.redo();
                self.revert(reverted, "nothing to redo");
            }
            KeyCode::Char('r') if control => {
                let reverted = self.edit.redo();
                self.revert(reverted, "nothing to redo");
            }
            KeyCode::Char('e') => self.begin_edit(How::Pane),
            KeyCode::Char('E') => self.begin_edit(How::Takeover),
            KeyCode::Char(':') => {
                self.prompt = Some(String::new());
                self.notice = None;
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Enter => self.confirm(),
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
            KeyCode::Backspace if prompt.is_empty() => self.prompt = None,
            KeyCode::Backspace => {
                prompt.pop();
            }
            KeyCode::Enter => {
                let entered = self.prompt.take().unwrap_or_default();
                self.go_to(entered.trim());
            }
            KeyCode::Char(ch) if !control && !key.modifiers.contains(KeyModifiers::ALT) => {
                prompt.push(ch);
            }
            _ => {}
        }
    }

    /// `N` moves the cursor to message N; `A-B` (or `A-`, `-B`) selects
    /// that range.
    fn go_to(&mut self, entered: &str) {
        if entered.is_empty() {
            return;
        }
        let Some(request) = crate::fragment::parse_range(entered) else {
            self.notice = Some(format!(
                "`{entered}` is not a message number or range like 3-10"
            ));
            return;
        };
        match request.resolve(self.edit.total()) {
            Ok(span) if span.0.len() == 1 => {
                self.edit.jump_to(span.0.start);
                self.notice = None;
                self.focus_cursor();
            }
            Ok(span) => {
                self.edit.select(span.0);
                self.notice = None;
                self.focus_cursor();
            }
            Err(error) => self.notice = Some(error),
        }
    }

    fn quit(&mut self, was_armed: bool) {
        if self.edit.edited() && !was_armed {
            self.mode = Mode::QuitArmed;
            self.notice =
                Some("your edits are not saved · press q again to leave anyway".to_string());
        } else {
            self.done = true;
        }
    }

    fn after_edit(&mut self, notice: Option<String>) {
        self.notice = notice;
        self.relayout();
        self.focus_cursor();
    }

    fn move_cursor(&mut self, delta: isize) {
        self.edit.move_cursor(delta);
        self.notice = None;
        self.focus_cursor();
    }

    fn jump_user_turn(&mut self, forward: bool) {
        let current = self.edit.cursor;
        let target = if forward {
            self.kinds
                .iter()
                .enumerate()
                .skip(current + 1)
                .find(|(_, kind)| **kind == MessageKind::User)
                .map(|(index, _)| index)
        } else {
            self.kinds
                .iter()
                .enumerate()
                .take(current)
                .rev()
                .find(|(_, kind)| **kind == MessageKind::User)
                .map(|(index, _)| index)
        };
        if let Some(index) = target {
            self.edit.jump_to(index);
            self.notice = None;
            self.focus_cursor();
        }
    }

    /// The first visual row of each message's rule.
    fn message_rows(&self) -> Vec<usize> {
        self.message_starts
            .iter()
            .filter_map(|line| self.line_first_row.get(*line).copied())
            .collect()
    }

    fn page_cursor(&mut self, forward: bool) {
        let rows = self.message_rows();
        let current = self.edit.cursor;
        let Some(&current_row) = rows.get(current) else {
            return;
        };
        let next = if forward {
            let target = current_row.saturating_add(self.rows);
            rows.partition_point(|row| *row <= target)
                .saturating_sub(1)
                .max(current.saturating_add(1))
                .min(self.edit.total() - 1)
        } else {
            let target = current_row.saturating_sub(self.rows);
            rows.partition_point(|row| *row < target)
                .min(current.saturating_sub(1))
        };
        self.edit.jump_to(next);
        self.notice = None;
        self.focus_cursor();
    }

    fn focus_cursor(&mut self) {
        let Some(&line) = self.message_starts.get(self.edit.cursor) else {
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
        let spans = self.edit.kept_spans();
        if spans.is_empty() {
            self.notice = Some("nothing is kept · restore at least one message".to_string());
            return;
        }
        match self.document.validate_crop_to(&spans) {
            Ok(()) => {
                self.result = Some(spans);
                self.done = true;
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
    }

    /// How a `width` by `height` screen is divided: the columns and rows
    /// the transcript gets, and the editor pane's place when one is open.
    /// The overview takes the right edge or the bottom by the window's
    /// shape; a pane takes the same place, and the overview yields to it.
    fn layout(width: usize, height: usize, pane: bool) -> Layout {
        let overview = if width.saturating_sub(3 + crop_render_width(width)) >= SIDE_MARGIN {
            Overview::Side
        } else {
            Overview::Bottom
        };
        let rect = |x: usize, y: usize, w: usize, h: usize| Rect {
            x: u16::try_from(x).unwrap_or(u16::MAX),
            y: u16::try_from(y).unwrap_or(u16::MAX),
            width: u16::try_from(w).unwrap_or(u16::MAX),
            height: u16::try_from(h).unwrap_or(u16::MAX),
        };
        match (pane, overview) {
            (false, Overview::Side) => Layout {
                overview,
                columns: width,
                rows: content_rows(height),
                pane: None,
            },
            (false, Overview::Bottom) => Layout {
                overview,
                columns: width,
                rows: crop_content_rows(height),
                pane: None,
            },
            (true, Overview::Side) => {
                let split = width / 2;
                Layout {
                    overview,
                    columns: split,
                    rows: content_rows(height),
                    pane: Some(rect(split, 0, width - split, content_rows(height))),
                }
            }
            (true, Overview::Bottom) => {
                let split = height / 2;
                Layout {
                    overview,
                    columns: width,
                    rows: content_rows(split),
                    pane: Some(rect(0, split, width, height.saturating_sub(split + 1))),
                }
            }
        }
    }

    fn update_viewport(&mut self, columns: usize, rows: usize) {
        let pane = matches!(self.editor, Some(Editor::Pane(_)));
        let layout = Self::layout(columns, rows, pane);
        self.overview = layout.overview;
        self.rows = layout.rows;
        let width = crop_render_width(layout.columns);
        if width != self.width {
            self.width = width;
            self.cols = width;
            if let Some(rendered) = self.document.render(self.width, Filters::crop()) {
                self.accept(rendered);
            }
        } else if width != self.cols {
            self.cols = width;
            self.relayout();
        }
        self.focus_cursor();
    }

    /// The messages with any row on screen.
    fn messages_on_screen(&self) -> std::ops::Range<usize> {
        let first = self
            .visual
            .get(self.top)
            .and_then(|row| self.message_of_line(row.line))
            .unwrap_or(0);
        let last = self
            .visual
            .get((self.top + self.rows).saturating_sub(1))
            .or(self.visual.last())
            .and_then(|row| self.message_of_line(row.line))
            .unwrap_or(first);
        first..last.max(first) + 1
    }

    /// The screen's rows as gutter marker and text, from the top of the
    /// viewport.
    fn visible_lines(
        &self,
        selection: Option<&std::ops::Range<usize>>,
    ) -> Vec<(Line<'static>, Line<'static>)> {
        let selected_rows = selection.map(|selection| {
            let start = self.message_rows()[selection.start];
            let end = self
                .message_starts
                .get(selection.end)
                .and_then(|line| self.line_first_row.get(line.saturating_sub(1)))
                .copied()
                .unwrap_or(self.visual.len());
            start..end.max(start + 1)
        });
        let cursor_row = self.message_rows().get(self.edit.cursor).copied();
        self.visual
            .iter()
            .enumerate()
            .skip(self.top)
            .take(self.rows)
            .map(|(row_index, row)| {
                let message = self.message_of_line(row.line);
                let removed = message.is_some_and(|message| !self.edit.kept[message]);
                let edited = message.is_some_and(|message| self.edited[message]);
                let rule_row = message.is_some_and(|message| {
                    self.message_starts[message] == row.line && row.range.start == 0
                });
                let edge = match &selected_rows {
                    Some(rows) if rows.contains(&row_index) => {
                        if rows.len() == 1 {
                            CropEdge::Only
                        } else if row_index == rows.start {
                            CropEdge::Start
                        } else if row_index + 1 == rows.end {
                            CropEdge::End
                        } else {
                            CropEdge::Middle
                        }
                    }
                    _ if removed => CropEdge::Removed,
                    _ if edited => CropEdge::Edited,
                    _ => CropEdge::Outside,
                };
                let line = &self.lines[row.line];
                let text = if removed || (edited && rule_row) {
                    let rule = if removed {
                        labelled_rule(
                            line,
                            self.cols,
                            "removed",
                            Style::new().add_modifier(Modifier::DIM),
                        )
                    } else {
                        let style = line.runs.first().map_or(Style::new(), |(_, style)| *style);
                        labelled_rule(line, self.cols, "edited", style)
                    };
                    Line::from(
                        row_spans(&rule, &(0..rule.plain.len()), &[])
                            .into_iter()
                            .map(|span| Span::styled(span.content.into_owned(), span.style))
                            .collect::<Vec<_>>(),
                    )
                } else {
                    Line::from(
                        row_spans(line, &row.range, &[])
                            .into_iter()
                            .map(|span| Span::styled(span.content.into_owned(), span.style))
                            .collect::<Vec<_>>(),
                    )
                };
                (
                    Line::from(crop_gutter(cursor_row == Some(row_index), edge, self.color)),
                    text,
                )
            })
            .collect()
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        self.area = (area.width, area.height);
        self.update_viewport(usize::from(area.width), usize::from(area.height));
        let pane_open = matches!(self.editor, Some(Editor::Pane(_)));
        let layout = Self::layout(usize::from(area.width), usize::from(area.height), pane_open);
        let selection = self.edit.selecting().then(|| self.edit.selection());
        let visible = self.visible_lines(selection.as_ref());
        let gutter = Rect {
            height: u16::try_from(self.rows)
                .unwrap_or(u16::MAX)
                .min(area.height),
            width: 3.min(area.width),
            ..area
        };
        let content = Rect {
            x: area.x.saturating_add(gutter.width),
            width: u16::try_from(layout.columns)
                .unwrap_or(u16::MAX)
                .saturating_sub(gutter.width),
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

        self.draw_beside(frame, area, &layout, pane_open, selection.as_ref());

        let status_area = Rect {
            y: area.y + area.height.saturating_sub(1),
            height: 1,
            ..area
        };
        let status = if let Some(prompt) = &self.prompt {
            Line::from(format!(":{prompt}"))
        } else {
            crop_status(
                &self.edit,
                self.notice.as_deref(),
                self.editing.as_ref(),
                matches!(self.editor, Some(Editor::Window(_))),
                self.color,
            )
        };
        frame.render_widget(Paragraph::new(status), status_area);
        if let Some(prompt) = &self.prompt {
            let column = u16::try_from(prompt.chars().count() + 1).unwrap_or(u16::MAX);
            frame.set_cursor_position((
                status_area.x + column.min(status_area.width.saturating_sub(1)),
                status_area.y,
            ));
        }

        if self.mode == Mode::Help {
            draw_help(frame, area, self.color);
        }
    }

    /// Paint what sits beside or under the transcript: the editor pane
    /// while one is open, else the overview.
    fn draw_beside(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        layout: &Layout,
        pane_open: bool,
        selection: Option<&std::ops::Range<usize>>,
    ) {
        if let Some(pane) = layout.pane.filter(|_| pane_open) {
            self.draw_pane(
                frame,
                Rect {
                    x: area.x + pane.x,
                    y: area.y + pane.y,
                    ..pane
                },
            );
        } else if self.overview == Overview::Side {
            let width = u16::try_from(MINIMAP_WIDTH).unwrap_or(u16::MAX);
            let minimap_area = Rect {
                x: area.x + area.width.saturating_sub(width),
                y: area.y,
                width,
                height: u16::try_from(self.rows)
                    .unwrap_or(u16::MAX)
                    .min(area.height),
            };
            let lines = minimap(
                &self.kinds,
                &self.edit.kept,
                self.edit.cursor,
                selection,
                self.messages_on_screen(),
                self.rows,
                self.color,
            );
            frame.render_widget(Paragraph::new(Text::from(lines)), minimap_area);
        } else if area.height >= 4 {
            // A rule closes the transcript before the timeline.
            let rule_area = Rect {
                y: area.y + area.height - 3,
                height: 1,
                ..area
            };
            let rule = Span::styled(
                "─".repeat(usize::from(area.width)),
                Style::new().add_modifier(Modifier::DIM),
            );
            frame.render_widget(Paragraph::new(Line::from(rule)), rule_area);
            let timeline_area = Rect {
                y: area.y + area.height - 2,
                height: 1,
                ..area
            };
            let timeline = timeline(
                &self.kinds,
                &self.edit.kept,
                self.edit.cursor,
                selection,
                usize::from(area.width),
                self.color,
            );
            frame.render_widget(Paragraph::new(timeline), timeline_area);
        }
    }

    /// Paint the editor's screen into `pane`, behind a border on the side
    /// facing the transcript, and put the cursor where the editor has it.
    fn draw_pane(&mut self, frame: &mut Frame, pane: Rect) {
        let Some(editing) = &self.editing else {
            return;
        };
        let Some(Editor::Pane(editor)) = &mut self.editor else {
            return;
        };
        let (rows, cols) = pane_inner(pane, self.overview);
        editor.resize(rows, cols);
        let title = format!(
            " {} · Message #{} · save and quit to apply ",
            editing.name,
            editing.index + 1
        );
        let border = if self.color {
            Style::new().fg(Color::Cyan)
        } else {
            Style::new()
        };
        let block = Block::default()
            .borders(match self.overview {
                Overview::Side => Borders::LEFT | Borders::TOP,
                Overview::Bottom => Borders::TOP,
            })
            .border_style(border)
            .title(title);
        let inner = block.inner(pane);
        let Some(parser) = editor.screen() else {
            return;
        };
        let screen = parser.screen();
        frame.render_widget(Clear, pane);
        frame.render_widget(
            PseudoTerminal::new(screen)
                .cursor(Cursor::default().visibility(false))
                .block(block),
            pane,
        );
        if !screen.hide_cursor() {
            let (row, col) = screen.cursor_position();
            frame.set_cursor_position((
                inner.x + col.min(inner.width.saturating_sub(1)),
                inner.y + row.min(inner.height.saturating_sub(1)),
            ));
        }
    }
}

/// The screen divided up: see [`Cropper::layout`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct Layout {
    overview: Overview,
    columns: usize,
    rows: usize,
    pane: Option<Rect>,
}

/// The rows and columns inside a pane's border.
fn pane_inner(pane: Rect, overview: Overview) -> (u16, u16) {
    match overview {
        Overview::Side => (pane.height.saturating_sub(1), pane.width.saturating_sub(1)),
        Overview::Bottom => (pane.height.saturating_sub(1), pane.width),
    }
}

/// A message out in the user's editor.
struct Editing {
    index: usize,
    path: std::path::PathBuf,
    /// The file as written, to tell an untouched save from an edit.
    original: String,
    /// The editor's name, for the pane title and notices.
    name: String,
}

/// How the editor is shown: in a pane of this screen, or given the whole
/// terminal until it exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum How {
    Pane,
    Takeover,
}

/// Where the editor runs.
enum Editor {
    Pane(editpane::Pane),
    Window(editpane::Detached),
}

/// What the next key means: an edit, closing the help, or confirming a
/// quit that would drop unsaved edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Editing,
    Help,
    QuitArmed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Overview {
    Side,
    Bottom,
}

/// Spare columns beyond the text before the overview moves beside it:
/// enough that the window reads as wider than the text, not merely wide
/// enough to squeeze the minimap in.
const SIDE_MARGIN: usize = 24;

/// The side minimap's columns: a rail, the cursor pointer, and the bars.
const MINIMAP_WIDTH: usize = 6;

/// The session down the right edge, one row per message (or per run of
/// messages when they outnumber the rows): a long heavy line for a user
/// turn, shorter for the assistant, a light line for a tool call, a dot for
/// its result; dimmed once removed, pointed at the cursor, colored across
/// the selection. The rail thickens beside the messages on screen.
fn minimap(
    kinds: &[MessageKind],
    kept: &[bool],
    cursor: usize,
    selection: Option<&std::ops::Range<usize>>,
    on_screen: std::ops::Range<usize>,
    rows: usize,
    color: bool,
) -> Vec<Line<'static>> {
    let total = kinds.len();
    if total == 0 || rows == 0 {
        return Vec::new();
    }
    let per_cell = total.div_ceil(rows);
    let bar_width = MINIMAP_WIDTH - 2;
    let mut lines = Vec::new();
    for first in (0..total).step_by(per_cell) {
        let bucket = first..(first + per_cell).min(total);
        let any_kept = bucket.clone().any(|index| kept[index]);
        let bar = bucket
            .clone()
            .filter(|index| !any_kept || kept[*index])
            .map(|index| kinds[index])
            .max_by_key(|kind| match kind {
                MessageKind::User => 3,
                MessageKind::Assistant => 2,
                MessageKind::ToolCall => 1,
                MessageKind::ToolResult => 0,
            })
            .map_or("", |kind| match kind {
                MessageKind::User => "━━━━",
                MessageKind::Assistant => "━━",
                MessageKind::ToolCall => "─",
                MessageKind::ToolResult => "·",
            });
        let selected = selection
            .is_some_and(|selection| bucket.start < selection.end && selection.start < bucket.end);
        let mut style = Style::new();
        if !any_kept {
            style = style.add_modifier(Modifier::DIM);
        }
        if selected {
            style = if color {
                style.fg(Color::Yellow)
            } else {
                style.add_modifier(Modifier::UNDERLINED)
            };
        } else if color && any_kept {
            style = style.fg(Color::Cyan);
        }
        let at_cursor = bucket.contains(&cursor);
        if at_cursor {
            style = style.add_modifier(Modifier::BOLD);
        }
        let showing = bucket.start < on_screen.end && on_screen.start < bucket.end;
        let rail = Span::styled(
            if showing { "┃" } else { "│" },
            if showing {
                Style::new()
            } else {
                Style::new().add_modifier(Modifier::DIM)
            },
        );
        let pointer = Span::styled(
            if at_cursor { "›" } else { " " },
            Style::new().add_modifier(Modifier::BOLD),
        );
        lines.push(Line::from(vec![
            rail,
            pointer,
            Span::styled(format!("{bar:<bar_width$}"), style),
        ]));
    }
    lines
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CropEdge {
    Start,
    Middle,
    End,
    Only,
    Removed,
    Edited,
    Outside,
}

fn crop_gutter(cursor: bool, edge: CropEdge, color: bool) -> Span<'static> {
    let pointer = if cursor { '›' } else { ' ' };
    let rail = match edge {
        CropEdge::Start => '┌',
        CropEdge::Middle => '│',
        CropEdge::End => '└',
        CropEdge::Only => '─',
        CropEdge::Removed => '×',
        CropEdge::Edited => '✎',
        CropEdge::Outside => ' ',
    };
    let mut style = if cursor {
        Style::new().add_modifier(Modifier::BOLD)
    } else if !matches!(edge, CropEdge::Outside) {
        Style::new().add_modifier(Modifier::DIM)
    } else {
        Style::new()
    };
    if color {
        if edge == CropEdge::Removed {
            style = style.fg(Color::Red);
        } else if cursor || edge != CropEdge::Outside {
            style = style.fg(Color::Cyan);
        }
    }
    Span::styled(format!("{pointer}{rail} "), style)
}

/// A message's header with a note after its label: `removed`, dimmed, in
/// place of the body that no longer shows; or `edited`.
fn labelled_rule(rule: &Styled, width: usize, note: &str, style: Style) -> Styled {
    let label = rule.plain.trim_end_matches('─').trim_end();
    let mut plain = format!("{label} · {note} ");
    let used = plain.width();
    plain.push_str(&"─".repeat(width.saturating_sub(used).max(2)));
    let end = plain.len();
    Styled {
        plain,
        runs: vec![(0..end, style)],
    }
}

/// `also removed #14 · tool calls stay with their results`, for the
/// messages an edit took along; nothing when it took none.
fn snapped_notice(verb: &str, extra: &[usize]) -> Option<String> {
    if extra.is_empty() {
        return None;
    }
    let listed = extra
        .iter()
        .map(|index| format!("#{}", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "also {verb} {listed} · tool calls stay with their results"
    ))
}

/// One cell per message (or per run of messages when they outnumber the
/// columns): taller for a user turn, shorter for the assistant and for tool
/// traffic; dimmed once removed, marked at the cursor and across the
/// selection.
fn timeline(
    kinds: &[MessageKind],
    kept: &[bool],
    cursor: usize,
    selection: Option<&std::ops::Range<usize>>,
    width: usize,
    color: bool,
) -> Line<'static> {
    let total = kinds.len();
    let left = " 1 ".to_string();
    let right = format!(" {total}");
    let track = width.saturating_sub(left.width() + right.width());
    if total == 0 || track < 4 {
        return Line::default();
    }
    let per_cell = total.div_ceil(track);
    let mut spans = vec![Span::styled(left, Style::new().add_modifier(Modifier::DIM))];
    for first in (0..total).step_by(per_cell) {
        let bucket = first..(first + per_cell).min(total);
        let any_kept = bucket.clone().any(|index| kept[index]);
        let glyph = bucket
            .clone()
            .filter(|index| !any_kept || kept[*index])
            .map(|index| kinds[index])
            .max_by_key(|kind| match kind {
                MessageKind::User => 3,
                MessageKind::Assistant => 2,
                MessageKind::ToolCall => 1,
                MessageKind::ToolResult => 0,
            })
            .map_or(' ', |kind| match kind {
                MessageKind::User => '█',
                MessageKind::Assistant => '▅',
                MessageKind::ToolCall => '▃',
                MessageKind::ToolResult => '▁',
            });
        let selected = selection
            .is_some_and(|selection| bucket.start < selection.end && selection.start < bucket.end);
        let mut style = Style::new();
        if !any_kept {
            style = style.add_modifier(Modifier::DIM);
        }
        if selected {
            style = if color {
                style.fg(Color::Yellow)
            } else {
                style.add_modifier(Modifier::UNDERLINED)
            };
        } else if color && any_kept {
            style = style.fg(Color::Cyan);
        }
        if bucket.contains(&cursor) {
            style = style.add_modifier(Modifier::REVERSED);
        }
        spans.push(Span::styled(glyph.to_string(), style));
    }
    spans.push(Span::styled(
        right,
        Style::new().add_modifier(Modifier::DIM),
    ));
    Line::from(spans)
}

fn crop_status(
    edit: &CropEdit,
    notice: Option<&str>,
    editing: Option<&Editing>,
    windowed: bool,
    color: bool,
) -> Line<'static> {
    let accent = if color {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::new().add_modifier(Modifier::BOLD)
    };
    let key = Style::new().add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled(" CROP ", accent), Span::raw("  ")];
    let at_removed = !edit.kept[edit.cursor];
    let kept = if edit.removed_count() == 0 {
        format!("all {} kept", edit.total())
    } else {
        format!("{}/{} kept", edit.kept_count(), edit.total())
    };
    spans.push(Span::raw(kept));
    spans.push(Span::raw(" · "));
    let selecting = edit.selecting();
    if selecting {
        let selection = edit.selection();
        spans.push(Span::styled(
            format!("#{}–{} selected", selection.start + 1, selection.end),
            key,
        ));
    } else {
        spans.push(Span::styled(format!("#{}", edit.cursor + 1), key));
    }
    spans.push(Span::raw("  "));
    if let Some(editing) = editing {
        let text = if windowed {
            format!(
                "editing #{} in {} · waiting for it to close · CTRL-C stop waiting",
                editing.index + 1,
                editing.name
            )
        } else {
            format!("editing #{} in {}", editing.index + 1, editing.name)
        };
        spans.push(Span::raw(text));
        return Line::from(spans);
    }
    if let Some(notice) = notice {
        let style = if color {
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::new().add_modifier(Modifier::BOLD)
        };
        spans.push(Span::styled(notice.to_string(), style));
        return Line::from(spans);
    }
    let hints: &[(&str, &str)] = if selecting {
        &[
            ("X", "remove"),
            ("R", "restore"),
            ("T", "keep only"),
            ("ESC", "clear"),
        ]
    } else {
        &[
            ("SPACE", if at_removed { "restore" } else { "remove" }),
            ("V", "select"),
            ("E", "edit"),
            ("ENTER", "save"),
            ("?", "help"),
        ]
    };
    for (index, (name, action)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled((*name).to_string(), key));
        spans.push(Span::raw(format!(" {action}")));
    }
    Line::from(spans)
}

const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Move",
        &[
            ("j k ↑ ↓", "one message"),
            ("f b PgDn PgUp", "one screen"),
            ("g G", "first / last message"),
            ("{ }", "previous / next user turn"),
            (":N  :A-B", "go to #N / select #A to #B"),
        ],
    ),
    (
        "Select",
        &[
            ("v", "start or stop selecting"),
            ("[ ]", "selection start / end at the cursor"),
            ("Esc", "clear the selection"),
        ],
    ),
    (
        "Edit",
        &[
            ("Space", "remove, or restore if removed"),
            ("x d Del", "remove"),
            ("r", "restore"),
            ("t", "keep only the selection"),
            ("e", "edit the message in your editor, in a pane"),
            ("E", "edit with the editor taking the whole terminal"),
            ("u U", "undo / redo"),
        ],
    ),
    (
        "Finish",
        &[
            ("Enter", "save the kept messages as a new session"),
            ("q", "leave without saving"),
        ],
    ),
];

const HELP_NOTE: &str = "Removed messages collapse to their header. A tool call and its result are removed or restored together. Editing opens the message in your editor, beside or under the transcript.";

fn help_lines() -> Vec<Line<'static>> {
    let key = Style::new().add_modifier(Modifier::BOLD);
    let mut lines = Vec::new();
    for (group, entries) in HELP {
        for (index, (keys, action)) in entries.iter().enumerate() {
            let label = if index == 0 { *group } else { "" };
            lines.push(Line::from(vec![
                Span::styled(format!(" {label:<8}"), key),
                Span::styled(format!("{keys:<15}"), key),
                Span::raw((*action).to_string()),
            ]));
        }
        lines.push(Line::default());
    }
    lines.push(Line::from(format!(" {HELP_NOTE}")));
    lines
}

fn draw_help(frame: &mut Frame, area: Rect, color: bool) {
    let lines = help_lines();
    let inner_width = lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(0)
        .min(usize::from(area.width).saturating_sub(4));
    let width = u16::try_from(inner_width + 4)
        .unwrap_or(u16::MAX)
        .min(area.width);
    let height = u16::try_from(lines.len() + 2)
        .unwrap_or(u16::MAX)
        .min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let border = if color {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(" Crop editor · any key closes ");
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(block),
        popup,
    );
}

pub(crate) fn crop_render_width(columns: usize) -> usize {
    columns.saturating_sub(3).clamp(1, 120)
}

/// Rows left for content once the rule, timeline, and status line have
/// theirs.
const fn crop_content_rows(rows: usize) -> usize {
    if rows > 4 { rows - 3 } else { 1 }
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
    fn crop_edit_starts_with_everything_kept_and_clamps_navigation() {
        let mut edit = CropEdit::new(4, &[]).unwrap();
        assert_eq!(edit.kept_spans(), vec![txcript::Span(0..4)]);
        assert_eq!(edit.cursor, 0);
        assert!(!edit.edited());
        assert_eq!(edit.selection(), 0..1);

        edit.move_cursor(-1);
        assert_eq!(edit.cursor, 0);
        edit.move_cursor(99);
        assert_eq!(edit.cursor, 3);
        assert!(CropEdit::new(0, &[]).is_err());
    }

    #[test]
    fn crop_edit_removes_the_middle_and_keeps_both_sides() {
        let mut edit = CropEdit::new(12, &[]).unwrap();
        // Remove #3–10, keep everything else.
        edit.select(2..10);
        assert_eq!(edit.selection(), 2..10);
        assert!(edit.cut().is_empty());
        assert!(!edit.selecting());
        assert_eq!(
            edit.kept_spans(),
            vec![txcript::Span(0..2), txcript::Span(10..12)]
        );
        assert_eq!(edit.removed_count(), 8);
        assert!(edit.edited());
    }

    #[test]
    fn crop_edit_toggles_selects_with_marks_and_trims() {
        let mut edit = CropEdit::new(5, &[]).unwrap();
        edit.jump_to(1);
        assert_eq!(edit.toggle(), (false, Vec::new()));
        assert!(!edit.kept[1]);
        assert_eq!(edit.toggle(), (true, Vec::new()));
        assert!(edit.kept[1]);

        // `[` at 1, move to 3, `]`: the selection is 1..=3.
        edit.mark_start();
        edit.jump_to(3);
        edit.mark_end();
        assert_eq!(edit.selection(), 1..4);
        assert!(edit.keep_only().is_empty());
        assert_eq!(edit.kept_spans(), vec![txcript::Span(1..4)]);

        // `]` alone selects from the first message.
        edit.jump_to(2);
        edit.mark_end();
        assert_eq!(edit.selection(), 0..3);
        edit.toggle_selecting();
        assert!(!edit.selecting());
        edit.toggle_selecting();
        assert_eq!(edit.anchor, Some(2));
    }

    #[test]
    fn crop_edit_snaps_to_tool_pairs_and_reports_what_it_took_along() {
        // 1 calls a tool, 2 answers it; 3 calls two tools answered by 4 and 5.
        let mut edit = CropEdit::new(7, &[(1, 2), (3, 4), (3, 5)]).unwrap();
        edit.jump_to(2);
        assert_eq!(edit.cut(), vec![1]);
        assert_eq!(
            edit.kept_spans(),
            vec![txcript::Span(0..1), txcript::Span(3..7)]
        );

        edit.jump_to(4);
        assert_eq!(edit.cut(), vec![3, 5]);
        assert_eq!(
            edit.kept_spans(),
            vec![txcript::Span(0..1), txcript::Span(6..7)]
        );

        edit.jump_to(3);
        assert_eq!(edit.restore(), vec![4, 5]);
        // Already-kept partners are not reported.
        edit.jump_to(1);
        edit.restore();
        edit.jump_to(2);
        assert!(edit.restore().is_empty());

        edit.select(3..4);
        assert_eq!(edit.keep_only(), vec![4, 5]);
        assert_eq!(edit.kept_spans(), vec![txcript::Span(3..6)]);
    }

    #[test]
    fn crop_edit_undoes_and_redoes_without_recording_no_ops() {
        let mut edit = CropEdit::new(3, &[]).unwrap();
        assert!(edit.undo().is_none());
        edit.jump_to(1);
        edit.cut();
        edit.jump_to(2);
        edit.cut();
        // Restoring a kept message changes nothing and is not history.
        edit.jump_to(0);
        edit.restore();
        assert_eq!(edit.undo.len(), 2);

        assert!(edit.undo().is_some());
        assert_eq!(
            edit.kept_spans(),
            vec![txcript::Span(0..1), txcript::Span(2..3)]
        );
        assert!(edit.undo().is_some());
        assert_eq!(edit.kept_spans(), vec![txcript::Span(0..3)]);
        assert!(!edit.edited());
        assert!(edit.redo().is_some());
        assert_eq!(
            edit.kept_spans(),
            vec![txcript::Span(0..1), txcript::Span(2..3)]
        );
        // A new edit drops the redo branch.
        edit.jump_to(0);
        edit.cut();
        assert!(edit.redo().is_none());
    }

    #[test]
    fn cropper_removes_a_range_confirms_it_and_can_cancel() {
        let mut editor = cropper(6);
        press_crop(&mut editor, KeyCode::Down);
        press_crop(&mut editor, KeyCode::Char('v'));
        press_crop(&mut editor, KeyCode::Down);
        press_crop(&mut editor, KeyCode::Down);
        press_crop(&mut editor, KeyCode::Char('x'));
        press_crop(&mut editor, KeyCode::Enter);
        assert!(editor.done);
        assert_eq!(
            editor.result,
            Some(vec![txcript::Span(0..1), txcript::Span(4..6)])
        );

        let mut cancelled = cropper(2);
        press_crop(&mut cancelled, KeyCode::Esc);
        assert!(cancelled.done);
        assert_eq!(cancelled.result, None);
    }

    #[test]
    fn cropper_asks_twice_before_leaving_unsaved_edits_behind() {
        let mut editor = cropper(3);
        press_crop(&mut editor, KeyCode::Char(' '));
        press_crop(&mut editor, KeyCode::Char('q'));
        assert!(!editor.done);
        assert!(
            editor
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("not saved")
        );
        // Any other key disarms the quit.
        press_crop(&mut editor, KeyCode::Down);
        press_crop(&mut editor, KeyCode::Char('q'));
        assert!(!editor.done);
        press_crop(&mut editor, KeyCode::Char('q'));
        assert!(editor.done);
        assert_eq!(editor.result, None);
    }

    #[test]
    fn cropper_refuses_to_save_nothing() {
        let mut editor = cropper(2);
        press_crop(&mut editor, KeyCode::Char(']'));
        press_crop(&mut editor, KeyCode::Down);
        press_crop(&mut editor, KeyCode::Char(']'));
        press_crop(&mut editor, KeyCode::Char('d'));
        press_crop(&mut editor, KeyCode::Enter);
        assert!(!editor.done);
        assert!(
            editor
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("nothing is kept")
        );
        press_crop(&mut editor, KeyCode::Char('u'));
        press_crop(&mut editor, KeyCode::Enter);
        assert_eq!(editor.result, Some(vec![txcript::Span(0..2)]));
    }

    #[test]
    fn cropper_takes_a_number_or_range_at_the_prompt() {
        let mut editor = cropper(10);
        for ch in ":7".chars() {
            press_crop(&mut editor, KeyCode::Char(ch));
        }
        press_crop(&mut editor, KeyCode::Enter);
        assert_eq!(editor.edit.cursor, 6);
        assert!(!editor.edit.selecting());

        for ch in ":3-5".chars() {
            press_crop(&mut editor, KeyCode::Char(ch));
        }
        press_crop(&mut editor, KeyCode::Enter);
        assert_eq!(editor.edit.selection(), 2..5);
        press_crop(&mut editor, KeyCode::Char('t'));
        assert_eq!(editor.edit.kept_spans(), vec![txcript::Span(2..5)]);

        for ch in ":x".chars() {
            press_crop(&mut editor, KeyCode::Char(ch));
        }
        press_crop(&mut editor, KeyCode::Enter);
        assert!(
            editor
                .notice
                .as_deref()
                .unwrap_or_default()
                .contains("3-10")
        );
    }

    #[test]
    fn cropper_preselects_an_initial_range_as_the_kept_set() {
        let editor = cropper_with(5, Some(txcript::Span(1..3)));
        assert_eq!(editor.edit.kept_spans(), vec![txcript::Span(1..3)]);
        assert_eq!(editor.edit.cursor, 1);
        assert!(
            !editor.edit.edited(),
            "the range is the starting point, not an edit"
        );
        assert!(cropper_new(3, Some(txcript::Span(2..2))).is_err());
        assert!(cropper_new(3, Some(txcript::Span(1..4))).is_err());
    }

    #[test]
    fn cropper_collapses_removed_messages_to_their_header() {
        let mut editor = cropper(3);
        let before = editor.visual.len();
        press_crop(&mut editor, KeyCode::Down);
        press_crop(&mut editor, KeyCode::Char('x'));
        assert!(editor.visual.len() < before);
        // The removed message's rule row is still there to land on.
        let rule = editor.message_starts[1];
        assert!(editor.visual.iter().any(|row| row.line == rule));
        assert!(!editor.visual.iter().any(|row| row.line == rule + 1));
        assert_eq!(editor.message_of_line(rule), Some(1));
        assert_eq!(editor.message_of_line(rule - 1), Some(1));
        assert_eq!(editor.message_of_line(0), None);
        press_crop(&mut editor, KeyCode::Char('u'));
        assert_eq!(editor.visual.len(), before);
    }

    #[test]
    fn cropper_jumps_between_user_turns_and_opens_help() {
        let mut editor = cropper(6);
        press_crop(&mut editor, KeyCode::Char('}'));
        assert_eq!(editor.edit.cursor, 2);
        press_crop(&mut editor, KeyCode::Char('}'));
        assert_eq!(editor.edit.cursor, 4);
        press_crop(&mut editor, KeyCode::Char('{'));
        assert_eq!(editor.edit.cursor, 2);
        press_crop(&mut editor, KeyCode::Char('?'));
        assert_eq!(editor.mode, Mode::Help);
        press_crop(&mut editor, KeyCode::Char('q'));
        assert_eq!(editor.mode, Mode::Editing);
        assert!(!editor.done);
    }

    #[test]
    fn cropper_keeps_the_cursor_visible_when_the_terminal_height_changes() {
        let mut editor = cropper(4);
        editor.edit.jump_to(3);
        editor.update_viewport(80, 3);

        let line = editor.message_starts[editor.edit.cursor];
        let row = editor.line_first_row[line];
        assert!(row >= editor.top);
        assert!(row < editor.top + editor.rows);
    }

    #[test]
    fn cropper_pages_by_rendered_rows_instead_of_message_count() {
        let mut editor = cropper(20);
        editor.update_viewport(80, 7);

        press_crop(&mut editor, KeyCode::PageDown);

        assert!(editor.edit.cursor > 0);
        assert!(editor.edit.cursor <= 2);
        press_crop(&mut editor, KeyCode::PageUp);
        assert_eq!(editor.edit.cursor, 0);
    }

    #[test]
    fn crop_gutter_marks_selection_removal_and_cursor_without_backgrounds() {
        let start = crop_gutter(true, CropEdge::Start, true);
        let selected = crop_gutter(false, CropEdge::Middle, true);
        let end = crop_gutter(false, CropEdge::End, true);
        let removed = crop_gutter(false, CropEdge::Removed, true);
        let outside = crop_gutter(true, CropEdge::Outside, true);

        assert_eq!(start.content.as_ref(), "›┌ ");
        assert_eq!(selected.content.as_ref(), " │ ");
        assert_eq!(end.content.as_ref(), " └ ");
        assert_eq!(removed.content.as_ref(), " × ");
        assert_eq!(removed.style.fg, Some(Color::Red));
        assert_eq!(outside.content.as_ref(), "›  ");
        for marker in [start, selected, end, removed, outside] {
            assert_eq!(marker.style.bg, None);
        }
    }

    #[test]
    fn removed_rule_keeps_the_label_and_says_so() {
        let rule = parse_ansi("\x1b[1;36m── Message #2 · Assistant ──────────\x1b[0m");
        let removed = labelled_rule(
            &rule,
            40,
            "removed",
            Style::new().add_modifier(Modifier::DIM),
        );
        assert!(
            removed
                .plain
                .starts_with("── Message #2 · Assistant · removed ─")
        );
        assert_eq!(removed.plain.width(), 40);
        assert!(removed.runs[0].1.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn minimap_runs_down_the_side_and_marks_what_is_on_screen() {
        use MessageKind as K;
        let kinds = vec![K::User, K::ToolCall, K::ToolResult, K::Assistant, K::User];
        let kept = vec![true, false, false, true, true];
        let lines = minimap(&kinds, &kept, 3, Some(&(0..2)), 1..3, 10, true);
        assert_eq!(lines.len(), 5);
        let bar = |index: usize| lines[index].spans[2].content.trim_end().to_string();
        assert_eq!(bar(0), "━━━━");
        assert_eq!(bar(1), "─");
        assert_eq!(bar(2), "·");
        assert_eq!(bar(3), "━━");
        assert!(lines.iter().all(|line| line.width() == MINIMAP_WIDTH));
        assert_eq!(lines[0].spans[0].content.as_ref(), "│");
        assert_eq!(lines[1].spans[0].content.as_ref(), "┃");
        assert_eq!(lines[2].spans[0].content.as_ref(), "┃");
        assert_eq!(lines[3].spans[0].content.as_ref(), "│");
        assert_eq!(lines[0].spans[2].style.fg, Some(Color::Yellow));
        assert!(lines[2].spans[2].style.add_modifier.contains(Modifier::DIM));
        assert_eq!(lines[3].spans[1].content.as_ref(), "›");
        assert_eq!(lines[0].spans[1].content.as_ref(), " ");
        assert!(
            lines[3].spans[2]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        // Ten messages in four rows: three per row, the longest bar wins.
        let kinds = vec![K::ToolResult; 10];
        let mut kinds_with_user = kinds.clone();
        kinds_with_user[4] = K::User;
        let lines = minimap(&kinds_with_user, &[true; 10], 0, None, 0..1, 4, false);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[1].spans[2].content.trim_end(), "━━━━");
    }

    #[test]
    fn the_overview_moves_beside_the_text_when_the_window_is_wider_than_it() {
        let mut editor = cropper(4);
        editor.update_viewport(80, 24);
        assert_eq!(editor.overview, Overview::Bottom);
        assert_eq!(editor.rows, 21);
        // 120 columns of text, a 3-column gutter, and 17 to spare: bottom.
        editor.update_viewport(140, 24);
        assert_eq!(editor.overview, Overview::Bottom);
        editor.update_viewport(190, 24);
        assert_eq!(editor.overview, Overview::Side);
        assert_eq!(editor.rows, 23);
        assert_eq!(editor.messages_on_screen(), 0..4);
        editor.update_viewport(80, 5);
        assert_eq!(editor.messages_on_screen(), 0..1);
    }

    #[test]
    fn timeline_buckets_messages_to_fit_and_marks_cursor_and_selection() {
        use MessageKind as K;
        let kinds = vec![K::User, K::ToolCall, K::ToolResult, K::Assistant, K::User];
        let kept = vec![true, false, false, true, true];
        let line = timeline(&kinds, &kept, 3, Some(&(0..2)), 20, true);
        let cells: Vec<&Span> = line.spans.iter().skip(1).take(5).collect();
        assert_eq!(
            cells
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<Vec<_>>(),
            vec!["█", "▃", "▁", "▅", "█"]
        );
        // Selected, kept: yellow. Selected, removed: yellow and dim.
        assert_eq!(cells[0].style.fg, Some(Color::Yellow));
        assert_eq!(cells[1].style.fg, Some(Color::Yellow));
        assert!(cells[1].style.add_modifier.contains(Modifier::DIM));
        // Removed, unselected: dim, no color. Cursor: reversed.
        assert!(cells[2].style.add_modifier.contains(Modifier::DIM));
        assert_eq!(cells[2].style.fg, None);
        assert!(cells[3].style.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(cells[4].style.fg, Some(Color::Cyan));

        // Ten messages in a five-cell track: two per cell, the taller wins.
        let kinds = vec![
            K::ToolResult,
            K::User,
            K::Assistant,
            K::ToolCall,
            K::User,
            K::User,
            K::Assistant,
            K::Assistant,
            K::ToolCall,
            K::ToolResult,
        ];
        let kept = vec![true; 10];
        let line = timeline(&kinds, &kept, 0, None, 5 + " 1 ".len() + " 10".len(), false);
        let cells: Vec<&str> = line
            .spans
            .iter()
            .skip(1)
            .take(5)
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(cells, vec!["█", "▅", "█", "▅", "▃"]);
        assert!(timeline(&kinds, &kept, 0, None, 4, false).spans.is_empty());
    }

    #[test]
    fn crop_status_shows_the_keys_for_the_current_state() {
        let mut edit = CropEdit::new(4, &[]).unwrap();
        let plain = |line: Line| {
            line.spans
                .iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        };
        let idle = plain(crop_status(&edit, None, None, false, false));
        assert!(idle.contains("all 4 kept"));
        assert!(idle.contains("#1"));
        assert!(idle.contains("SPACE remove"));
        assert!(idle.contains("E edit"));
        assert!(idle.contains("? help"));

        edit.jump_to(1);
        edit.cut();
        edit.jump_to(2);
        edit.toggle_selecting();
        edit.jump_to(3);
        let selecting = plain(crop_status(&edit, None, None, false, false));
        assert!(selecting.contains("3/4 kept"));
        assert!(selecting.contains("#3–4 selected"));
        assert!(selecting.contains("T keep only"));

        let noticed = plain(crop_status(
            &edit,
            Some("also removed #2"),
            None,
            false,
            false,
        ));
        assert!(noticed.contains("also removed #2"));
        assert!(!noticed.contains("keep only"));
        assert_eq!(snapped_notice("removed", &[]), None);
        assert_eq!(
            snapped_notice("removed", &[13, 15]).as_deref(),
            Some("also removed #14, #16 · tool calls stay with their results")
        );
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
        assert_eq!(crop_content_rows(24), 21);
        assert_eq!(crop_content_rows(4), 1);
        assert_eq!(crop_content_rows(2), 1);
    }

    #[test]
    fn terminal_restoration_failure_blocks_the_crop_result() {
        let result = Ok(Some(vec![txcript::Span(1..2)]));
        let restore = Err(std::io::Error::other("restore failed"));

        let error = finish_after_restore(result, restore).unwrap_err();

        assert!(error.contains("restoring the terminal"));
        assert!(error.contains("restore failed"));
    }

    #[test]
    fn crop_edit_keeps_message_edits_in_the_same_history_as_cuts() {
        let mut edit = CropEdit::new(3, &[]).unwrap();
        let before = text_message("old");
        let after = text_message("new");
        edit.jump_to(1);
        edit.cut();
        edit.record_message(2, before.clone(), after.clone());
        assert!(edit.edited());
        assert_eq!(
            edit.undo(),
            Some(Reverted::Message {
                index: 2,
                message: before.clone()
            })
        );
        assert_eq!(edit.undo(), Some(Reverted::Kept));
        assert_eq!(edit.kept_spans(), vec![txcript::Span(0..3)]);
        assert_eq!(edit.redo(), Some(Reverted::Kept));
        assert_eq!(
            edit.redo(),
            Some(Reverted::Message {
                index: 2,
                message: after
            })
        );
        assert_eq!(edit.redo(), None);
        // A new edit after an undo drops the redo branch.
        edit.undo();
        edit.record_message(0, before.clone(), text_message("newer"));
        assert_eq!(edit.redo(), None);
    }

    #[test]
    fn an_edited_file_changes_the_message_and_undo_puts_it_back() {
        let mut editor = cropper(3);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("message-2.md");
        let original = draft::draft(editor.document.message(1).unwrap()).unwrap();
        std::fs::write(&path, original.replace("message 1", "message one, edited")).unwrap();
        let editing = Editing {
            index: 1,
            path: path.clone(),
            original: original.clone(),
            name: "vi".into(),
        };
        editor.conclude_edit(&editing, Ok(()));
        assert_eq!(editor.notice.as_deref(), Some("#2 edited"));
        assert!(editor.edited[1]);
        assert!(!path.exists(), "an applied draft is cleaned up");
        assert_eq!(
            editor.document.message(1).unwrap().content,
            vec![txcript::common::Block::Text {
                text: "message one, edited".into()
            }]
        );
        assert!(
            editor
                .lines
                .iter()
                .any(|line| line.plain.contains("message one, edited"))
        );
        press_crop(&mut editor, KeyCode::Char('u'));
        assert!(!editor.edited[1]);
        assert_eq!(editor.document.message(1), editor.original.get(1));
        press_crop(&mut editor, KeyCode::Char('U'));
        assert!(editor.edited[1]);

        // An untouched file changes nothing.
        std::fs::write(&path, &original).unwrap();
        let editing = Editing {
            index: 1,
            path: path.clone(),
            original,
            name: "vi".into(),
        };
        editor.conclude_edit(&editing, Ok(()));
        assert_eq!(editor.notice.as_deref(), Some("#2 unchanged"));

        // An editor that failed leaves the message alone.
        std::fs::write(&path, "whatever").unwrap();
        editor.conclude_edit(
            &editing_at(&path, 1),
            Err("the editor exited with exit status: 1".into()),
        );
        assert!(
            editor
                .notice
                .as_deref()
                .unwrap()
                .starts_with("#2 not changed")
        );
        assert!(!path.exists());

        // A file that no longer parses keeps the user's text around: a
        // message with two blocks needs its headings back.
        let mut two_blocks = editor.document.message(1).unwrap().clone();
        two_blocks.content.push(txcript::common::Block::Text {
            text: "second".into(),
        });
        editor.document.replace_message(1, two_blocks);
        std::fs::write(&path, "no headings here").unwrap();
        editor.conclude_edit(&editing_at(&path, 1), Ok(()));
        let notice = editor.notice.clone().unwrap();
        assert!(notice.contains("heading is missing"), "{notice}");
        assert!(notice.contains(&path.display().to_string()));
        assert!(path.exists());
    }

    #[test]
    fn editing_is_refused_on_a_removed_message_and_needs_something_to_edit() {
        let mut editor = cropper(2);
        press_crop(&mut editor, KeyCode::Char('x'));
        press_crop(&mut editor, KeyCode::Char('e'));
        assert!(editor.editing.is_none());
        assert!(editor.notice.as_deref().unwrap().contains("restore it"));
        assert_eq!(editor.result, None);
    }

    #[test]
    fn a_pane_takes_the_overview_side_or_the_bottom() {
        let wide = Cropper::layout(190, 40, true);
        assert_eq!(wide.overview, Overview::Side);
        assert_eq!(wide.columns, 95);
        assert_eq!(wide.rows, 39);
        let pane = wide.pane.unwrap();
        assert_eq!((pane.x, pane.y, pane.width, pane.height), (95, 0, 95, 39));
        assert_eq!(pane_inner(pane, Overview::Side), (38, 94));

        let tall = Cropper::layout(100, 40, true);
        assert_eq!(tall.overview, Overview::Bottom);
        assert_eq!(tall.columns, 100);
        assert_eq!(tall.rows, 19);
        let pane = tall.pane.unwrap();
        assert_eq!((pane.x, pane.y, pane.width, pane.height), (0, 20, 100, 19));
        assert_eq!(pane_inner(pane, Overview::Bottom), (18, 100));
        assert_eq!(Cropper::layout(100, 40, false).pane, None);
    }

    #[test]
    fn edited_messages_are_marked_in_the_gutter_and_the_rule() {
        let marker = crop_gutter(false, CropEdge::Edited, true);
        assert_eq!(marker.content.as_ref(), " ✎ ");
        let rule = parse_ansi("\x1b[1;36m── Message #2 · Assistant ──────────\x1b[0m");
        let style = rule.runs[0].1;
        let edited = labelled_rule(&rule, 40, "edited", style);
        assert!(
            edited
                .plain
                .starts_with("── Message #2 · Assistant · edited ─")
        );
        assert_eq!(edited.plain.width(), 40);
        assert_eq!(edited.runs[0].1, style);
    }

    fn text_message(text: &str) -> Message {
        Message {
            role: txcript::common::Role::User,
            content: vec![txcript::common::Block::Text { text: text.into() }],
            timestamp: chrono::DateTime::UNIX_EPOCH,
            model: None,
            stop_reason: None,
            usage: None,
        }
    }

    fn editing_at(path: &std::path::Path, index: usize) -> Editing {
        Editing {
            index,
            path: path.to_path_buf(),
            original: String::new(),
            name: "vi".into(),
        }
    }

    fn press_crop(editor: &mut Cropper, code: KeyCode) {
        editor.key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn cropper(message_count: usize) -> Cropper {
        cropper_with(message_count, None)
    }

    fn cropper_with(message_count: usize, initial: Option<txcript::Span>) -> Cropper {
        cropper_new(message_count, initial).unwrap()
    }

    fn cropper_new(
        message_count: usize,
        initial: Option<txcript::Span>,
    ) -> Result<Cropper, String> {
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
        let rendered = document.render(80, Filters::crop()).unwrap();
        Cropper::new(document, rendered, 80, initial)
    }
}
