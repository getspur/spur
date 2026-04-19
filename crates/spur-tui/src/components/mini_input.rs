//! Single-line text buffer used by `PickerShell` as its query surface.
//!
//! Deliberately narrow: no newline insertion, no protected ranges, no history,
//! no vim mode, no undo. When a feature request would grow this past ~120 LOC,
//! redesign — do not extend incrementally.

/// Single-line text buffer with a byte-offset cursor.
pub struct MiniInput {
    text: String,
    cursor: usize, // byte offset into text; always on a UTF-8 char boundary
}

impl MiniInput {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Insert arbitrary text, stripping any `\n` or `\r` characters.
    pub fn paste(&mut self, s: &str) {
        let cleaned: String = s.chars().filter(|c| *c != '\n' && *c != '\r').collect();
        self.text.insert_str(self.cursor, &cleaned);
        self.cursor += cleaned.len();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.text[..self.cursor]
            .chars()
            .next_back()
            .expect("cursor > 0 implies a prev char");
        let new_cursor = self.cursor - prev.len_utf8();
        self.text.drain(new_cursor..self.cursor);
        self.cursor = new_cursor;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.text[self.cursor..]
            .chars()
            .next()
            .expect("cursor < len implies a next char");
        let end = self.cursor + next.len_utf8();
        self.text.drain(self.cursor..end);
    }

    pub fn left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.text[..self.cursor]
            .chars()
            .next_back()
            .expect("cursor > 0 implies a prev char");
        self.cursor -= prev.len_utf8();
    }

    pub fn right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.text[self.cursor..]
            .chars()
            .next()
            .expect("cursor < len implies a next char");
        self.cursor += next.len_utf8();
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
}

impl Default for MiniInput {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_empty() {
        let m = MiniInput::new();
        assert_eq!(m.text(), "");
        assert_eq!(m.cursor(), 0);
    }

    #[test]
    fn insert_ascii_chars_advances_cursor() {
        let mut m = MiniInput::new();
        m.insert_char('h');
        m.insert_char('i');
        assert_eq!(m.text(), "hi");
        assert_eq!(m.cursor(), 2);
    }

    #[test]
    fn insert_multibyte_uses_utf8_byte_len() {
        let mut m = MiniInput::new();
        m.insert_char('你');
        m.insert_char('好');
        assert_eq!(m.text(), "你好");
        assert_eq!(m.cursor(), 6);
    }

    #[test]
    fn backspace_removes_prev_char() {
        let mut m = MiniInput::new();
        m.insert_char('a');
        m.insert_char('b');
        m.backspace();
        assert_eq!(m.text(), "a");
        assert_eq!(m.cursor(), 1);
    }

    #[test]
    fn backspace_on_empty_is_noop() {
        let mut m = MiniInput::new();
        m.backspace();
        assert_eq!(m.text(), "");
        assert_eq!(m.cursor(), 0);
    }

    #[test]
    fn backspace_multibyte() {
        let mut m = MiniInput::new();
        m.insert_char('你');
        m.backspace();
        assert_eq!(m.text(), "");
        assert_eq!(m.cursor(), 0);
    }

    #[test]
    fn delete_removes_next_char() {
        let mut m = MiniInput::new();
        m.insert_char('a');
        m.insert_char('b');
        m.left();
        m.delete();
        assert_eq!(m.text(), "a");
        assert_eq!(m.cursor(), 1);
    }

    #[test]
    fn left_right_bound_at_edges() {
        let mut m = MiniInput::new();
        m.left(); // no-op at start
        assert_eq!(m.cursor(), 0);
        m.insert_char('a');
        m.right(); // no-op at end
        assert_eq!(m.cursor(), 1);
    }

    #[test]
    fn home_end() {
        let mut m = MiniInput::new();
        m.insert_char('a');
        m.insert_char('b');
        m.home();
        assert_eq!(m.cursor(), 0);
        m.end();
        assert_eq!(m.cursor(), 2);
    }

    #[test]
    fn paste_strips_newlines() {
        let mut m = MiniInput::new();
        m.paste("hello\nworld\r\nmore");
        assert_eq!(m.text(), "helloworldmore");
        assert_eq!(m.cursor(), "helloworldmore".len());
    }

    #[test]
    fn clear_resets() {
        let mut m = MiniInput::new();
        m.insert_char('a');
        m.clear();
        assert_eq!(m.text(), "");
        assert_eq!(m.cursor(), 0);
    }
}
