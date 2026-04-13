use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
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
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                None
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    // Find the start of the previous character.
                    let prev = self.prev_char_boundary(self.cursor);
                    self.text.drain(prev..self.cursor);
                    self.cursor = prev;
                }
                None
            }
            KeyCode::Delete => {
                if self.cursor < self.text.len() {
                    let next = self.next_char_boundary(self.cursor);
                    self.text.drain(self.cursor..next);
                }
                None
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor = self.prev_char_boundary(self.cursor);
                }
                None
            }
            KeyCode::Right => {
                if self.cursor < self.text.len() {
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
    /// (`█`) at the current cursor position.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green))
            .title(Span::styled(
                " INSERT ",
                Style::default().fg(Color::Green),
            ));

        // Split the visible text around the cursor to insert the cursor glyph.
        let before = &self.text[..self.cursor];
        let after = &self.text[self.cursor..];

        let mut spans = Vec::new();

        // Status label (if set)
        if let Some(ref status) = self.status {
            spans.push(Span::styled(
                format!("{} ", status),
                Style::default().fg(Color::DarkGray),
            ));
        }

        // Prompt + text + cursor
        spans.push(Span::raw("> "));
        spans.push(Span::raw(before));
        spans.push(Span::styled("\u{2588}", Style::default().fg(Color::Green)));
        spans.push(Span::raw(after));

        let line = Line::from(spans);

        let paragraph = Paragraph::new(line).block(block);
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
}

impl Default for InputBar {
    fn default() -> Self {
        Self::new()
    }
}
