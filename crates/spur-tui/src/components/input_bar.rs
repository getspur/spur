use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

/// A text input widget for chatting with the brain agent.
pub struct InputBar {
    /// Current input text.
    text: String,
    /// Cursor position as a byte index into `text`.
    cursor: usize,
    status: Option<String>,
}

impl InputBar {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            status: None,
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

    /// Reset text and cursor.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
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
