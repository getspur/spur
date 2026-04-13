use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

/// A protected byte range inside `InputBar::text` representing an atomic
/// token (today: a resource mention). Full edit semantics land in Task 12;
/// this task only stubs the field so `SubmitRouter` can consume it.
#[derive(Debug, Clone)]
pub struct ProtectedRange {
    pub start: usize,
    pub end: usize,
    pub uri: String,
    pub name: String,
}

/// A text input widget for chatting with the brain agent.
pub struct InputBar {
    /// Current input text.
    text: String,
    /// Cursor position as a byte index into `text`.
    cursor: usize,
    status: Option<String>,
    /// Sorted, non-overlapping protected ranges representing atomic tokens
    /// (e.g. resource mentions). Always empty in v1 until Task 12 lands.
    protected_ranges: Vec<ProtectedRange>,
    /// Capture of the most recent Enter-submit: `(text, ranges, interrupt)`.
    /// Populated on Enter, consumed via `take_submit_capture()`.
    submit_capture: Option<(String, Vec<ProtectedRange>, bool)>,
}

impl InputBar {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            status: None,
            protected_ranges: Vec::new(),
            submit_capture: None,
        }
    }

    /// Process a key event.
    ///
    /// Returns `Some((text, interrupt))` when the user presses Enter on non-empty
    /// input, where `interrupt` is `true` when the text starts with `'!'`.
    /// Returns `None` for all other keys.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<(String, bool)> {
        match key.code {
            KeyCode::Char(c) => {
                if let Some(idx) = self.range_at(self.cursor) {
                    self.delete_range(idx);
                }
                let at = self.cursor;
                self.text.insert(at, c);
                self.shift_ranges(at, c.len_utf8() as isize);
                self.cursor = at + c.len_utf8();
                None
            }
            KeyCode::Backspace => {
                if let Some(idx) = self.range_at(self.cursor) {
                    self.delete_range(idx);
                } else if let Some(idx) = self.range_ending_at(self.cursor) {
                    self.delete_range(idx);
                } else if let Some(idx) = self.range_starting_at(self.cursor) {
                    // Cursor sits on the left edge of an atom (e.g. after
                    // arrow-skipping into it from the right). Treat the atom
                    // as the unit-to-delete rather than the character before
                    // it — this keeps atom semantics consistent with how
                    // arrow keys treat the same boundary.
                    self.delete_range(idx);
                } else if self.cursor > 0 {
                    let prev = self.prev_char_boundary(self.cursor);
                    let delta = -((self.cursor - prev) as isize);
                    self.text.drain(prev..self.cursor);
                    self.shift_ranges(prev, delta);
                    self.cursor = prev;
                }
                None
            }
            KeyCode::Delete => {
                if let Some(idx) = self.range_at(self.cursor) {
                    self.delete_range(idx);
                } else if let Some(idx) = self.range_starting_at(self.cursor) {
                    self.delete_range(idx);
                } else if self.cursor < self.text.len() {
                    let next = self.next_char_boundary(self.cursor);
                    let delta = -((next - self.cursor) as isize);
                    self.text.drain(self.cursor..next);
                    self.shift_ranges(self.cursor, delta);
                }
                None
            }
            KeyCode::Left => {
                if let Some(idx) = self
                    .range_at(self.cursor)
                    .or_else(|| self.range_ending_at(self.cursor))
                {
                    // Cursor is inside or at the right edge of an atom — jump
                    // to its left edge (atomic skip).
                    let r = &self.protected_ranges[idx];
                    self.cursor = r.start;
                } else if self.cursor > 0 {
                    self.cursor = self.prev_char_boundary(self.cursor);
                } else if let Some(idx) = self.range_starting_at(self.cursor) {
                    // Cursor is at byte 0 with an atom starting there and
                    // nothing to its left — re-enter the atom from its left
                    // side so a subsequent character key triggers the
                    // "typing inside an atom replaces it" path.
                    let r = &self.protected_ranges[idx];
                    if r.end > r.start + 1 {
                        self.cursor = r.start + 1;
                    }
                }
                None
            }
            KeyCode::Right => {
                if let Some(idx) = self
                    .range_at(self.cursor)
                    .or_else(|| self.range_starting_at(self.cursor))
                {
                    let r = &self.protected_ranges[idx];
                    self.cursor = r.end;
                } else if self.cursor < self.text.len() {
                    self.cursor = self.next_char_boundary(self.cursor);
                }
                None
            }
            KeyCode::Home => {
                self.cursor = 0;
                None
            }
            KeyCode::End => {
                self.cursor = self.text.len();
                None
            }
            KeyCode::Enter => {
                if self.text.is_empty() {
                    return None;
                }
                let submitted = self.text.clone();
                let interrupt = submitted.starts_with('!');
                let ranges = self.protected_ranges.clone();
                self.submit_capture = Some((submitted.clone(), ranges, interrupt));
                self.clear();
                Some((submitted, interrupt))
            }
            _ => None,
        }
    }

    /// Insert a protected atom at the cursor. If the cursor is inside an
    /// existing range, that range is deleted first.
    pub fn insert_atom(
        &mut self,
        text: impl AsRef<str>,
        uri: String,
        name: String,
    ) {
        if let Some(idx) = self.range_at(self.cursor) {
            self.delete_range(idx);
        }
        let at = self.cursor;
        let s = text.as_ref();
        self.text.insert_str(at, s);
        let end = at + s.len();
        self.shift_ranges(at, s.len() as isize);
        self.protected_ranges.push(ProtectedRange {
            start: at,
            end,
            uri,
            name,
        });
        self.protected_ranges.sort_by_key(|r| r.start);
        self.cursor = end;
    }

    /// Test-only: set the cursor position without asserting anything else.
    #[doc(hidden)]
    pub fn set_text_cursor_for_test(&mut self, cursor: usize) {
        assert!(cursor <= self.text.len());
        assert!(self.text.is_char_boundary(cursor));
        self.cursor = cursor;
    }

    /// The current text content.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Current cursor byte offset in `text`.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether the input buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Reset text and cursor. Also clears `protected_ranges`. Does not
    /// reset `submit_capture` — that is consumed via `take_submit_capture`.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.protected_ranges.clear();
    }

    /// Sorted, non-overlapping protected ranges. Empty in v1 until Task 12 lands.
    pub fn protected_ranges(&self) -> &[ProtectedRange] {
        &self.protected_ranges
    }

    /// Take and reset the most recent Enter-submit capture.
    pub fn take_submit_capture(&mut self) -> Option<(String, Vec<ProtectedRange>, bool)> {
        self.submit_capture.take()
    }

    /// Replace `text` and cursor wholesale. Panics if `cursor > text.len()` or
    /// the cursor is not on a UTF-8 char boundary.
    pub fn set_text(&mut self, text: String, cursor: usize) {
        assert!(cursor <= text.len(), "cursor past end");
        assert!(text.is_char_boundary(cursor), "cursor off UTF-8 boundary");
        self.text = text;
        self.cursor = cursor;
    }

    /// Set the status label shown before the prompt (e.g. "[kiro: ready]").
    pub fn set_status(&mut self, status: Option<String>) {
        self.status = status;
    }

    /// Whether a brain-status label is currently set. Used by the dashboard
    /// empty-state hint to decide whether to render the onboarding prompt.
    pub fn has_status(&self) -> bool {
        self.status.is_some()
    }

    /// Required render height based on text length.
    ///
    /// The returned value includes the border lines (top + bottom), so the
    /// widget should be given exactly this many rows.
    ///
    /// | text bytes | inner content rows | total height |
    /// |---|---|---|
    /// | 0 – 70     | 1                  | 3            |
    /// | 71 – 140   | 2                  | 4            |
    /// | 141+       | 3                  | 5            |
    ///
    /// We follow the task spec which says 1/2/3, interpreted as inner rows;
    /// adding 2 for the Block border makes the widget self-contained.
    pub fn required_height(&self) -> u16 {
        let len = self.text.len();
        let inner = if len <= 70 {
            1_u16
        } else if len <= 140 {
            2
        } else {
            3
        };
        // +2 for top and bottom border lines.
        inner + 2
    }

    /// Render the input bar into `area`.
    ///
    /// Displays a green-bordered box with a `> ` prompt and a block cursor
    /// (`█`) at the current cursor position. Protected ranges (atoms) are
    /// styled cyan + underlined.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green))
            .title(Span::styled(
                " INSERT ",
                Style::default().fg(Color::Green),
            ));

        let atom_style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::UNDERLINED);
        let plain_style = Style::default();
        let cursor_style = Style::default().fg(Color::Green);

        let mut spans = Vec::new();

        // Status label (if set)
        if let Some(ref status) = self.status {
            spans.push(Span::styled(
                format!("{} ", status),
                Style::default().fg(Color::DarkGray),
            ));
        }

        // Prompt
        spans.push(Span::raw("> "));

        // Build split points: 0, len, cursor, and each range boundary.
        let mut splits: Vec<usize> = Vec::with_capacity(4 + self.protected_ranges.len() * 2);
        splits.push(0);
        splits.push(self.text.len());
        splits.push(self.cursor);
        for r in &self.protected_ranges {
            splits.push(r.start);
            splits.push(r.end);
        }
        splits.sort();
        splits.dedup();

        let in_range = |byte: usize| -> bool {
            self.protected_ranges
                .iter()
                .any(|r| byte >= r.start && byte < r.end)
        };

        for window in splits.windows(2) {
            let (a, b) = (window[0], window[1]);
            // Insert cursor glyph at the left boundary of this slice if it
            // falls there and there is text following (otherwise handled below).
            if self.cursor == a && self.cursor < self.text.len() {
                spans.push(Span::styled("\u{2588}", cursor_style));
            }
            if a == b {
                continue;
            }
            let style = if in_range(a) { atom_style } else { plain_style };
            spans.push(Span::styled(self.text[a..b].to_string(), style));
        }

        // Cursor at end of text (including empty text).
        if self.cursor == self.text.len() {
            spans.push(Span::styled("\u{2588}", cursor_style));
        }

        let paragraph = Paragraph::new(Line::from(spans)).block(block);
        frame.render_widget(paragraph, area);
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Return the byte index of the start of the character that ends at `pos`.
    fn prev_char_boundary(&self, pos: usize) -> usize {
        let mut idx = pos.saturating_sub(1);
        while idx > 0 && !self.text.is_char_boundary(idx) {
            idx -= 1;
        }
        idx
    }

    /// Return the byte index of the start of the next character after `pos`.
    fn next_char_boundary(&self, pos: usize) -> usize {
        let mut idx = pos + 1;
        while idx <= self.text.len() && !self.text.is_char_boundary(idx) {
            idx += 1;
        }
        idx.min(self.text.len())
    }

    /// Index of the protected range that strictly contains `pos`
    /// (i.e. `r.start < pos < r.end`). Use [`range_starting_at`] /
    /// [`range_ending_at`] for adjacency at the boundaries.
    fn range_at(&self, pos: usize) -> Option<usize> {
        self.protected_ranges
            .iter()
            .position(|r| pos > r.start && pos < r.end)
    }

    /// Index of the protected range that ends exactly at `pos`.
    fn range_ending_at(&self, pos: usize) -> Option<usize> {
        self.protected_ranges.iter().position(|r| r.end == pos)
    }

    /// Index of the protected range that starts exactly at `pos`.
    fn range_starting_at(&self, pos: usize) -> Option<usize> {
        self.protected_ranges.iter().position(|r| r.start == pos)
    }

    /// Shift all ranges with `start >= at` by `delta` bytes.
    fn shift_ranges(&mut self, at: usize, delta: isize) {
        for r in &mut self.protected_ranges {
            if r.start >= at {
                r.start = (r.start as isize + delta) as usize;
                r.end = (r.end as isize + delta) as usize;
            }
        }
    }

    /// Remove the range at `idx`, drain its bytes from `text`, place the
    /// cursor at the range start, and shift trailing ranges left.
    fn delete_range(&mut self, idx: usize) {
        let r = self.protected_ranges.remove(idx);
        let len = r.end - r.start;
        self.text.drain(r.start..r.end);
        self.cursor = r.start;
        self.shift_ranges(r.start, -(len as isize));
    }
}

impl Default for InputBar {
    fn default() -> Self {
        Self::new()
    }
}
