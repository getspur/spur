use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders},
    Frame,
};
use tui_textarea::{CursorMove, Input, Key, TextArea};

/// A protected byte range inside the text representing an atomic token
/// (e.g., a resource mention). These ranges are skipped atomically by
/// cursor movement and deleted as a unit.
#[derive(Debug, Clone)]
pub struct ProtectedRange {
    pub start: usize,
    pub end: usize,
    pub uri: String,
    pub name: String,
}

/// Editing mode for the input bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditMode {
    /// Emacs-style keybindings (default, uses TextArea's built-ins)
    #[default]
    Emacs,
    /// Vim modal editing
    Vim(VimMode),
}

/// Vim modal states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimMode {
    Normal,
    Insert,
    Visual,
    Operator(char),
}

const HISTORY_CAP: usize = 100;

/// A text input widget for chatting with the brain agent, built on tui-textarea.
pub struct InputBar {
    /// The underlying textarea widget.
    textarea: TextArea<'static>,
    /// Current editing mode.
    mode: EditMode,
    /// Vim state for pending two-key sequences (e.g., `gg`).
    vim_pending: Option<Input>,
    /// Sorted, non-overlapping protected ranges representing atomic tokens.
    protected_ranges: Vec<ProtectedRange>,
    /// Line cache: byte offset where each line starts.
    line_cache: Vec<usize>,
    /// Status label shown before the prompt.
    status: Option<String>,
    /// Capture of the most recent Enter-submit: `(text, ranges, interrupt)`.
    submit_capture: Option<(String, Vec<ProtectedRange>, bool)>,
    /// Submitted input history, oldest first. Capped at [`HISTORY_CAP`].
    history: Vec<String>,
    /// `None` = editing live draft; `Some(i)` = browsing `history[i]`.
    history_cursor: Option<usize>,
    /// Stashed live input when the user enters history-browsing mode.
    draft: String,
}

impl InputBar {
    pub fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(Style::default());
        textarea.set_cursor_style(Style::default().fg(Color::Green));
        textarea.set_max_histories(0); // Disable undo/redo to prevent protected range desync

        Self {
            textarea,
            mode: EditMode::Emacs,
            vim_pending: None,
            protected_ranges: Vec::new(),
            line_cache: vec![0],
            status: None,
            submit_capture: None,
            history: Vec::new(),
            history_cursor: None,
            draft: String::new(),
        }
    }

    /// Set the editing mode.
    pub fn set_mode(&mut self, mode: EditMode) {
        self.mode = mode;
        // Update cursor style based on mode
        let cursor_style = match mode {
            EditMode::Emacs => Style::default().fg(Color::Green),
            EditMode::Vim(VimMode::Normal) => Style::default()
                .fg(Color::Reset)
                .add_modifier(Modifier::REVERSED),
            EditMode::Vim(VimMode::Insert) => Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::REVERSED),
            EditMode::Vim(VimMode::Visual) => Style::default()
                .fg(Color::LightYellow)
                .add_modifier(Modifier::REVERSED),
            EditMode::Vim(VimMode::Operator(_)) => Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::REVERSED),
        };
        self.textarea.set_cursor_style(cursor_style);
    }

    /// Get the current editing mode.
    pub fn mode(&self) -> EditMode {
        self.mode
    }

    /// Toggle between Emacs and Vim(Normal) modes.
    pub fn toggle_mode(&mut self) {
        match self.mode {
            EditMode::Emacs => self.set_mode(EditMode::Vim(VimMode::Normal)),
            EditMode::Vim(_) => self.set_mode(EditMode::Emacs),
        }
    }

    /// Returns true when the input bar needs to consume Esc (Vim Insert/Visual/Operator).
    pub fn wants_esc(&self) -> bool {
        matches!(
            self.mode,
            EditMode::Vim(VimMode::Insert)
                | EditMode::Vim(VimMode::Visual)
                | EditMode::Vim(VimMode::Operator(_))
        )
    }

    /// Returns true when in Vim Normal mode (views may need to handle nav keys directly).
    pub fn is_vim_normal(&self) -> bool {
        matches!(self.mode, EditMode::Vim(VimMode::Normal))
    }

    /// Process a key event.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<(String, bool)> {
        let input = self.keyevent_to_input(key);

        match self.mode {
            EditMode::Emacs => self.handle_emacs_input(key, input),
            EditMode::Vim(mode) => self.handle_vim_input(key, input, mode),
        }
    }

    fn keyevent_to_input(&self, key: KeyEvent) -> Input {
        Input {
            key: match key.code {
                KeyCode::Char(c) => Key::Char(c),
                KeyCode::Backspace => Key::Backspace,
                KeyCode::Enter => Key::Enter,
                KeyCode::Left => Key::Left,
                KeyCode::Right => Key::Right,
                KeyCode::Up => Key::Up,
                KeyCode::Down => Key::Down,
                KeyCode::Home => Key::Home,
                KeyCode::End => Key::End,
                KeyCode::PageUp => Key::PageUp,
                KeyCode::PageDown => Key::PageDown,
                KeyCode::Tab => Key::Tab,
                KeyCode::BackTab => Key::Tab, // BackTab treated as Tab
                KeyCode::Delete => Key::Delete,
                KeyCode::Insert => Key::Null, // Insert not supported
                KeyCode::Esc => Key::Esc,
                KeyCode::F(n) => Key::F(n),
                _ => Key::Null,
            },
            ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
            alt: key.modifiers.contains(KeyModifiers::ALT),
            shift: key.modifiers.contains(KeyModifiers::SHIFT),
        }
    }

    fn handle_emacs_input(&mut self, key: KeyEvent, input: Input) -> Option<(String, bool)> {
        // Handle protected range logic for special keys
        match key.code {
            KeyCode::Left => {
                self.move_cursor_back();
                return None;
            }
            KeyCode::Right => {
                self.move_cursor_forward();
                return None;
            }
            KeyCode::Backspace => {
                self.delete_char_before_cursor();
                return None;
            }
            KeyCode::Delete => {
                self.delete_char_after_cursor();
                return None;
            }
            // Ctrl+J: insert newline (Kitty keyboard protocol)
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.textarea.insert_newline();
                self.rebuild_line_cache();
                return None;
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.history_prev();
                return None;
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.history_next();
                return None;
            }
            // Ctrl+U: delete to start of line
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let cursor = self.cursor_to_byte();
                if cursor > 0 {
                    // Find start of current line
                    let text = self.text();
                    let line_start = text[..cursor].rfind('\n').map(|i| i + 1).unwrap_or(0);
                    let delete_len = cursor - line_start;
                    self.textarea.move_cursor(CursorMove::Head);
                    self.textarea.delete_str(delete_len);
                    self.rebuild_line_cache();
                    self.protected_ranges.retain(|r| r.start >= line_start);
                    self.shift_ranges(line_start, -(delete_len as isize));
                }
                return None;
            }
            // Ctrl+K: delete to end of line
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let cursor = self.cursor_to_byte();
                let text = self.text();
                let line_end = text[cursor..]
                    .find('\n')
                    .map(|i| cursor + i)
                    .unwrap_or(text.len());
                if line_end > cursor {
                    let _delete_len = line_end - cursor;
                    self.textarea.delete_line_by_end();
                    self.rebuild_line_cache();
                    self.protected_ranges.retain(|r| r.end <= cursor);
                }
                return None;
            }
            // Ctrl+W: delete previous word
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let cursor = self.cursor_to_byte();
                if cursor > 0 {
                    let text = self.text();
                    let bytes = text.as_bytes();
                    let mut i = cursor;
                    // Skip trailing whitespace
                    while i > 0 && bytes[i - 1].is_ascii_whitespace() {
                        i -= 1;
                    }
                    // Skip word chars
                    while i > 0 && !bytes[i - 1].is_ascii_whitespace() {
                        i -= 1;
                    }
                    let delete_len = cursor - i;
                    self.move_cursor_to_byte(i);
                    self.textarea.delete_str(delete_len);
                    self.rebuild_line_cache();
                    self.protected_ranges.retain(|r| r.start >= i);
                    self.shift_ranges(i, -(delete_len as isize));
                }
                return None;
            }
            KeyCode::Char(c) => {
                self.insert_char_with_protected_check(c);
                return None;
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.textarea.insert_newline();
                self.rebuild_line_cache();
                return None;
            }
            KeyCode::Enter => {
                return self.submit();
            }
            _ => {}
        }

        // Delegate to textarea for other keys
        self.textarea.input(input);
        self.rebuild_line_cache();
        None
    }

    fn handle_vim_input(
        &mut self,
        key: KeyEvent,
        input: Input,
        mode: VimMode,
    ) -> Option<(String, bool)> {
        match mode {
            VimMode::Normal | VimMode::Visual | VimMode::Operator(_) => {
                self.handle_vim_normal_input(key, input, mode)
            }
            VimMode::Insert => self.handle_vim_insert_input(key, input),
        }
    }

    fn handle_vim_normal_input(
        &mut self,
        _key: KeyEvent,
        input: Input,
        mode: VimMode,
    ) -> Option<(String, bool)> {
        if input.key == Key::Null {
            return None;
        }

        // Handle pending two-key sequences (gg, dd, yy, cc)
        if let Some(pending) = self.vim_pending.take() {
            match pending.key {
                Key::Char('g') if matches!(input.key, Key::Char('g')) => {
                    self.textarea.move_cursor(CursorMove::Top);
                    return self.vim_complete_operator(mode);
                }
                Key::Char(op) if matches!(mode, VimMode::Operator(c) if c == op) => {
                    // dd, yy, cc — select whole line
                    if let Key::Char(c) = input.key {
                        if c == op {
                            self.textarea.move_cursor(CursorMove::Head);
                            self.textarea.start_selection();
                            let cursor = self.textarea.cursor();
                            self.textarea.move_cursor(CursorMove::Down);
                            if cursor == self.textarea.cursor() {
                                self.textarea.move_cursor(CursorMove::End);
                            }
                            return self.vim_complete_operator(mode);
                        }
                    }
                    // Pending didn't match — fall through
                }
                _ => {} // Pending didn't match — fall through
            }
        }

        match input {
            // ── Movement ────────────────────────────────────────────
            Input { key: Key::Char('h'), .. } => self.move_cursor_back(),
            Input { key: Key::Char('j'), .. } => self.textarea.move_cursor(CursorMove::Down),
            Input { key: Key::Char('k'), .. } => self.textarea.move_cursor(CursorMove::Up),
            Input { key: Key::Char('l'), .. } => self.move_cursor_forward(),
            Input { key: Key::Char('w'), .. } => self.textarea.move_cursor(CursorMove::WordForward),
            Input { key: Key::Char('e'), ctrl: false, .. } => {
                self.textarea.move_cursor(CursorMove::WordEnd);
                if matches!(mode, VimMode::Operator(_)) {
                    self.textarea.move_cursor(CursorMove::Forward);
                }
            }
            Input { key: Key::Char('b'), ctrl: false, .. } => {
                self.textarea.move_cursor(CursorMove::WordBack);
            }
            Input { key: Key::Char('^'), .. } => self.textarea.move_cursor(CursorMove::Head),
            Input { key: Key::Char('0'), .. } => self.textarea.move_cursor(CursorMove::Head),
            Input { key: Key::Char('$'), .. } => self.textarea.move_cursor(CursorMove::End),
            Input { key: Key::Char('g'), ctrl: false, .. } => {
                self.vim_pending = Some(input);
                return None;
            }
            Input { key: Key::Char('G'), ctrl: false, .. } => {
                self.textarea.move_cursor(CursorMove::Bottom);
            }

            // ── Editing (Normal only) ───────────────────────────────
            Input { key: Key::Char('D'), .. } if mode == VimMode::Normal => {
                self.textarea.delete_line_by_end();
                self.rebuild_line_cache();
                self.protected_ranges.clear();
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return None;
            }
            Input { key: Key::Char('C'), .. } if mode == VimMode::Normal => {
                self.textarea.delete_line_by_end();
                self.rebuild_line_cache();
                self.protected_ranges.clear();
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return None;
            }
            Input { key: Key::Char('x'), .. } => {
                self.delete_char_after_cursor();
                return None;
            }
            Input { key: Key::Char('p'), .. } if mode == VimMode::Normal => {
                self.textarea.paste();
                self.rebuild_line_cache();
                self.protected_ranges.clear();
                return None;
            }

            // ── Operator entry (Normal → Operator) ──────────────────
            Input { key: Key::Char(op @ ('d' | 'c' | 'y')), ctrl: false, .. }
                if mode == VimMode::Normal =>
            {
                self.textarea.start_selection();
                self.set_mode(EditMode::Vim(VimMode::Operator(op)));
                self.vim_pending = Some(input);
                return None;
            }

            // ── Mode entry ──────────────────────────────────────────
            Input { key: Key::Char('i'), .. } if mode != VimMode::Visual => {
                self.textarea.cancel_selection();
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return None;
            }
            Input { key: Key::Char('a'), .. } if mode != VimMode::Visual => {
                self.textarea.cancel_selection();
                self.textarea.move_cursor(CursorMove::Forward);
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return None;
            }
            Input { key: Key::Char('A'), .. } if mode != VimMode::Visual => {
                self.textarea.cancel_selection();
                self.textarea.move_cursor(CursorMove::End);
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return None;
            }
            Input { key: Key::Char('I'), .. } if mode != VimMode::Visual => {
                self.textarea.cancel_selection();
                self.textarea.move_cursor(CursorMove::Head);
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return None;
            }
            Input { key: Key::Char('o'), .. } if mode == VimMode::Normal => {
                self.textarea.move_cursor(CursorMove::End);
                self.textarea.insert_newline();
                self.rebuild_line_cache();
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return None;
            }
            Input { key: Key::Char('O'), .. } if mode == VimMode::Normal => {
                self.textarea.move_cursor(CursorMove::Head);
                self.textarea.insert_newline();
                self.textarea.move_cursor(CursorMove::Up);
                self.rebuild_line_cache();
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return None;
            }

            // ── Visual mode ─────────────────────────────────────────
            Input { key: Key::Char('v'), ctrl: false, .. } if mode == VimMode::Normal => {
                self.textarea.start_selection();
                self.set_mode(EditMode::Vim(VimMode::Visual));
                return None;
            }
            Input { key: Key::Char('V'), ctrl: false, .. } if mode == VimMode::Normal => {
                self.textarea.move_cursor(CursorMove::Head);
                self.textarea.start_selection();
                self.textarea.move_cursor(CursorMove::End);
                self.set_mode(EditMode::Vim(VimMode::Visual));
                return None;
            }
            Input { key: Key::Esc, .. }
            | Input { key: Key::Char('v'), ctrl: false, .. }
                if mode == VimMode::Visual =>
            {
                self.textarea.cancel_selection();
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return None;
            }

            // ── Visual operations ───────────────────────────────────
            Input { key: Key::Char('y'), ctrl: false, .. } if mode == VimMode::Visual => {
                self.textarea.move_cursor(CursorMove::Forward);
                self.textarea.copy();
                self.textarea.cancel_selection();
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return None;
            }
            Input { key: Key::Char('d'), ctrl: false, .. } if mode == VimMode::Visual => {
                self.textarea.move_cursor(CursorMove::Forward);
                self.textarea.cut();
                self.rebuild_line_cache();
                self.protected_ranges.clear();
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return None;
            }
            Input { key: Key::Char('c'), ctrl: false, .. } if mode == VimMode::Visual => {
                self.textarea.move_cursor(CursorMove::Forward);
                self.textarea.cut();
                self.rebuild_line_cache();
                self.protected_ranges.clear();
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return None;
            }

            // ── Scroll ──────────────────────────────────────────────
            Input { key: Key::Char('d'), ctrl: true, .. } => {
                self.textarea.scroll(tui_textarea::Scrolling::HalfPageDown);
            }
            Input { key: Key::Char('u'), ctrl: true, .. } => {
                self.textarea.scroll(tui_textarea::Scrolling::HalfPageUp);
            }
            Input { key: Key::Char('f'), ctrl: true, .. } => {
                self.textarea.scroll(tui_textarea::Scrolling::PageDown);
            }
            Input { key: Key::Char('b'), ctrl: true, .. } => {
                self.textarea.scroll(tui_textarea::Scrolling::PageUp);
            }
            Input { key: Key::Char('e'), ctrl: true, .. } => {
                self.textarea.scroll((1, 0));
            }
            Input { key: Key::Char('y'), ctrl: true, .. } => {
                self.textarea.scroll((-1, 0));
            }

            // ── Esc / Enter ─────────────────────────────────────────
            Input { key: Key::Esc, .. } => {
                self.textarea.cancel_selection();
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return None;
            }
            Input { key: Key::Enter, alt: true, .. } => {
                self.textarea.insert_newline();
                self.rebuild_line_cache();
                return None;
            }
            Input { key: Key::Enter, .. } => {
                return self.submit();
            }
            _ => return None,
        }

        // After movement, complete pending operator
        self.vim_complete_operator(mode)
    }

    /// Complete a pending operator (d/c/y) after a movement.
    fn vim_complete_operator(&mut self, mode: VimMode) -> Option<(String, bool)> {
        match mode {
            VimMode::Operator('y') => {
                self.textarea.copy();
                self.textarea.cancel_selection();
                self.set_mode(EditMode::Vim(VimMode::Normal));
            }
            VimMode::Operator('d') => {
                self.textarea.cut();
                self.rebuild_line_cache();
                self.protected_ranges.clear();
                self.set_mode(EditMode::Vim(VimMode::Normal));
            }
            VimMode::Operator('c') => {
                self.textarea.cut();
                self.rebuild_line_cache();
                self.protected_ranges.clear();
                self.set_mode(EditMode::Vim(VimMode::Insert));
            }
            _ => {}
        }
        None
    }

    fn handle_vim_insert_input(&mut self, key: KeyEvent, input: Input) -> Option<(String, bool)> {
        match key.code {
            KeyCode::Esc => {
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return None;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return None;
            }
            // Alt+J / Ctrl+J: insert newline
            KeyCode::Char('j')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.textarea.insert_newline();
                self.rebuild_line_cache();
                return None;
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.textarea.insert_newline();
                self.rebuild_line_cache();
                return None;
            }
            KeyCode::Left => {
                self.move_cursor_back();
                return None;
            }
            KeyCode::Right => {
                self.move_cursor_forward();
                return None;
            }
            KeyCode::Backspace => {
                self.delete_char_before_cursor();
                return None;
            }
            KeyCode::Delete => {
                self.delete_char_after_cursor();
                return None;
            }
            KeyCode::Char(c) => {
                self.insert_char_with_protected_check(c);
                return None;
            }
            KeyCode::Enter => {
                return self.submit();
            }
            _ => {}
        }

        self.textarea.input(input);
        self.rebuild_line_cache();
        None
    }

    // ── Protected Range Helpers ─────────────────────────────────────────────

    /// Convert cursor (row, col) to byte offset.
    fn cursor_to_byte(&self) -> usize {
        let (row, col) = self.textarea.cursor();
        let lines = self.textarea.lines();
        if row >= lines.len() {
            return self.line_cache.last().copied().unwrap_or(0);
        }
        let line = &lines[row];
        let byte_offset = line
            .char_indices()
            .nth(col)
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        self.line_cache.get(row).copied().unwrap_or(0) + byte_offset
    }

    /// Logical (row, char_col) → byte offset within `lines()[row]`.
    fn char_col_to_byte(&self, row: usize, char_col: usize) -> usize {
        let lines = self.textarea.lines();
        if row >= lines.len() {
            return 0;
        }
        let line = &lines[row];
        line.char_indices()
            .nth(char_col)
            .map(|(i, _)| i)
            .unwrap_or(line.len())
    }

    /// Rebuild the line cache after text modification.
    fn rebuild_line_cache(&mut self) {
        self.line_cache.clear();
        let mut offset = 0;
        for line in self.textarea.lines() {
            self.line_cache.push(offset);
            offset += line.len() + 1; // +1 for newline
        }
        // Ensure at least one entry
        if self.line_cache.is_empty() {
            self.line_cache.push(0);
        }
    }

    /// Index of the protected range that strictly contains the cursor.
    fn range_at_cursor(&self) -> Option<usize> {
        let pos = self.cursor_to_byte();
        self.protected_ranges
            .iter()
            .position(|r| pos > r.start && pos < r.end)
    }

    /// Index of the protected range that starts at cursor.
    fn range_starting_at_cursor(&self) -> Option<usize> {
        let pos = self.cursor_to_byte();
        self.protected_ranges.iter().position(|r| r.start == pos)
    }

    /// Index of the protected range that ends at cursor.
    fn range_ending_at_cursor(&self) -> Option<usize> {
        let pos = self.cursor_to_byte();
        self.protected_ranges.iter().position(|r| r.end == pos)
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

    /// Delete a protected range by index.
    fn delete_range(&mut self, idx: usize) {
        let r = self.protected_ranges.remove(idx);
        let len = r.end - r.start;
        // Move cursor to range start
        self.move_cursor_to_byte(r.start);
        // Delete the text
        self.textarea.delete_str(len);
        self.rebuild_line_cache();
    }

    /// Move cursor to a specific byte offset.
    fn move_cursor_to_byte(&mut self, byte_pos: usize) {
        // Find the line containing this byte offset
        for (row, &line_start) in self.line_cache.iter().enumerate() {
            let line_end = self.line_cache.get(row + 1).copied().unwrap_or(usize::MAX);
            if byte_pos >= line_start && byte_pos < line_end {
                let col = byte_pos - line_start;
                let lines = self.textarea.lines();
                if row < lines.len() {
                    let line = &lines[row];
                    // Convert byte offset to char position
                    let char_pos = line
                        .char_indices()
                        .position(|(i, _)| i >= col)
                        .unwrap_or(line.chars().count());
                    self.textarea
                        .move_cursor(CursorMove::Jump(row as u16, char_pos as u16));
                }
                return;
            }
        }
    }

    /// Move cursor back, skipping protected ranges atomically.
    fn move_cursor_back(&mut self) {
        let cursor = self.cursor_to_byte();
        if let Some(idx) = self
            .range_at_cursor()
            .or_else(|| self.range_ending_at_cursor())
        {
            // Cursor is inside or at the right edge of an atom — jump to its left edge
            let r = &self.protected_ranges[idx];
            self.move_cursor_to_byte(r.start);
        } else if cursor > 0 {
            self.textarea.move_cursor(CursorMove::Back);
        } else if let Some(idx) = self.range_starting_at_cursor() {
            // Cursor is at byte 0 with an atom starting there — re-enter the atom
            let r = &self.protected_ranges[idx];
            if r.end > r.start + 1 {
                self.move_cursor_to_byte(r.start + 1);
            }
        }
    }

    /// Move cursor forward, skipping protected ranges atomically.
    fn move_cursor_forward(&mut self) {
        if let Some(idx) = self
            .range_at_cursor()
            .or_else(|| self.range_starting_at_cursor())
        {
            let r = &self.protected_ranges[idx];
            self.move_cursor_to_byte(r.end);
        } else {
            self.textarea.move_cursor(CursorMove::Forward);
        }
    }

    /// Delete character before cursor, handling protected ranges.
    fn delete_char_before_cursor(&mut self) {
        if let Some(idx) = self
            .range_at_cursor()
            .or_else(|| self.range_ending_at_cursor())
            .or_else(|| self.range_starting_at_cursor())
        {
            self.delete_range(idx);
        } else {
            self.textarea.delete_char();
            self.rebuild_line_cache();
            let cursor = self.cursor_to_byte();
            self.shift_ranges(cursor, -1);
        }
        self.history_cursor = None;
    }

    /// Delete character after cursor, handling protected ranges.
    fn delete_char_after_cursor(&mut self) {
        if let Some(idx) = self
            .range_at_cursor()
            .or_else(|| self.range_starting_at_cursor())
        {
            self.delete_range(idx);
        } else {
            self.textarea.delete_next_char();
            self.rebuild_line_cache();
            let cursor = self.cursor_to_byte();
            self.shift_ranges(cursor, -1);
        }
        self.history_cursor = None;
    }

    /// Insert a character, replacing protected range if inside one.
    fn insert_char_with_protected_check(&mut self, c: char) {
        // Delete range if cursor is strictly inside it
        if let Some(idx) = self.range_at_cursor() {
            self.delete_range(idx);
        }
        let cursor = self.cursor_to_byte();
        self.textarea.insert_char(c);
        self.rebuild_line_cache();
        self.shift_ranges(cursor, c.len_utf8() as isize);
        self.history_cursor = None;
    }

    /// Submit the current text.
    fn submit(&mut self) -> Option<(String, bool)> {
        let text = self.textarea.lines().join("\n");
        if text.is_empty() {
            return None;
        }
        let interrupt = text.starts_with('!');
        let ranges = self.protected_ranges.clone();
        self.submit_capture = Some((text.clone(), ranges, interrupt));

        // Push to history
        self.history.push(text.clone());
        if self.history.len() > HISTORY_CAP {
            self.history.remove(0);
        }
        self.history_cursor = None;
        self.draft.clear();
        self.clear();

        Some((text, interrupt))
    }

    // ── Public API ───────────────────────────────────────────────────────────

    /// Insert a protected atom at the cursor.
    pub fn insert_atom(&mut self, text: impl AsRef<str>, uri: String, name: String) {
        if let Some(idx) = self.range_at_cursor() {
            self.delete_range(idx);
        }
        let cursor = self.cursor_to_byte();
        let s = text.as_ref();
        self.textarea.insert_str(s);
        self.rebuild_line_cache();
        let end = cursor + s.len();
        self.shift_ranges(cursor, s.len() as isize);
        self.protected_ranges.push(ProtectedRange {
            start: cursor,
            end,
            uri,
            name,
        });
        self.protected_ranges.sort_by_key(|r| r.start);
    }

    /// Insert pasted text (may contain newlines). Does not trigger submit.
    pub fn insert_paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(idx) = self.range_at_cursor() {
            self.delete_range(idx);
        }
        let cursor = self.cursor_to_byte();
        let mut lines = text.split('\n');
        if let Some(first) = lines.next() {
            if !first.is_empty() {
                self.textarea.insert_str(first);
            }
            for line in lines {
                self.textarea.insert_newline();
                if !line.is_empty() {
                    self.textarea.insert_str(line);
                }
            }
        }
        self.rebuild_line_cache();
        let new_cursor = self.cursor_to_byte();
        let delta = new_cursor as isize - cursor as isize;
        if delta != 0 {
            self.shift_ranges(cursor, delta);
        }
        self.history_cursor = None;
    }

    /// The current text content.
    pub fn text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Current cursor byte offset.
    pub fn cursor(&self) -> usize {
        self.cursor_to_byte()
    }

    /// Whether the input buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.textarea.is_empty()
    }

    /// Reset text and cursor.
    pub fn clear(&mut self) {
        let mode = self.mode;
        self.textarea = TextArea::default();
        self.textarea.set_cursor_line_style(Style::default());
        self.line_cache = vec![0];
        self.protected_ranges.clear();
        self.set_mode(mode);
    }

    /// Sorted, non-overlapping protected ranges.
    pub fn protected_ranges(&self) -> &[ProtectedRange] {
        &self.protected_ranges
    }

    /// Take and reset the most recent Enter-submit capture.
    pub fn take_submit_capture(&mut self) -> Option<(String, Vec<ProtectedRange>, bool)> {
        self.submit_capture.take()
    }

    /// Replace text and cursor wholesale.
    pub fn set_text(&mut self, text: String, cursor: usize) {
        let mode = self.mode;
        let lines: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
        self.textarea = TextArea::new(lines);
        self.textarea.set_cursor_line_style(Style::default());
        self.rebuild_line_cache();
        self.move_cursor_to_byte(cursor);
        self.protected_ranges.clear();
        self.set_mode(mode);
    }

    /// Set the status label.
    pub fn set_status(&mut self, status: Option<String>) {
        self.status = status;
    }

    /// Whether a status label is set.
    pub fn has_status(&self) -> bool {
        self.status.is_some()
    }

    /// Replace the in-memory history with persisted entries (e.g. loaded
    /// from `session_metadata.json` at startup).
    pub fn seed_history(&mut self, entries: Vec<String>) {
        self.history = entries;
        self.history_cursor = None;
    }

    /// Current history entries (for persistence).
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Navigate to previous history entry.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_cursor {
            None => {
                self.draft = self.text();
                let idx = self.history.len() - 1;
                self.history_cursor = Some(idx);
                self.load_history_entry(idx);
            }
            Some(i) if i > 0 => {
                self.history_cursor = Some(i - 1);
                self.load_history_entry(i - 1);
            }
            _ => {}
        }
    }

    /// Navigate to next history entry.
    pub fn history_next(&mut self) {
        match self.history_cursor {
            Some(i) if i < self.history.len() - 1 => {
                self.history_cursor = Some(i + 1);
                self.load_history_entry(i + 1);
            }
            Some(_) => {
                self.history_cursor = None;
                let draft = std::mem::take(&mut self.draft);
                let len = draft.len();
                self.set_text(draft, len);
            }
            None => {}
        }
    }

    fn load_history_entry(&mut self, idx: usize) {
        let entry = self.history[idx].clone();
        let len = entry.len();
        self.protected_ranges.clear();
        self.set_text(entry, len);
    }

    /// Test-only: set cursor position.
    #[doc(hidden)]
    pub fn set_text_cursor_for_test(&mut self, cursor: usize) {
        self.move_cursor_to_byte(cursor);
    }

    /// Required render height given the available `width`.
    ///
    /// Includes 2 rows for top+bottom borders. The inner rows are the
    /// visual-row count produced by the soft-wrap layer, clamped to
    /// `[1, 5]` so the input bar never dominates the view.
    pub fn required_height(&self, width: u16) -> u16 {
        let inner_w = width.saturating_sub(2);
        if inner_w == 0 {
            return 3;
        }

        let mut lines: Vec<String> = self.textarea.lines().to_vec();
        if lines.is_empty() {
            lines.push(String::new());
        }

        let layout = crate::components::input_bar_wrap::wrap(&lines, inner_w);
        let inner = layout.visual_height().clamp(1, 5);
        inner + 2
    }

    /// Render the input bar.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let mode_str = match self.mode {
            EditMode::Emacs => " INSERT ",
            EditMode::Vim(VimMode::Normal) => " VIM·NORMAL ",
            EditMode::Vim(VimMode::Insert) => " VIM·INSERT ",
            EditMode::Vim(VimMode::Visual) => " VIM·VISUAL ",
            EditMode::Vim(VimMode::Operator(_)) => " VIM·OP ",
        };

        let title = if let Some(ref status) = self.status {
            format!("{} {}", status, mode_str)
        } else {
            mode_str.to_string()
        };

        let border_color = match self.mode {
            EditMode::Vim(VimMode::Normal) => Color::Yellow,
            EditMode::Vim(VimMode::Visual) => Color::LightYellow,
            _ => Color::Green,
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(title, Style::default().fg(border_color)));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        // Compute wrap layout for the buffer against the inner width.
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let layout =
            crate::components::input_bar_wrap::wrap(&lines, inner.width);

        // Cursor in visual coordinates.
        let (cursor_row, cursor_ccol) = self.textarea.cursor();
        let cursor_byte = self.char_col_to_byte(cursor_row, cursor_ccol);
        let (cursor_vr, cursor_vc) =
            layout.logical_to_visual(cursor_row, cursor_byte);

        // Vertical scroll: keep the cursor within the visible window.
        let visible = inner.height as usize;
        let total = layout.visual_height() as usize;
        let view_top = if total <= visible {
            0
        } else if cursor_vr >= visible {
            cursor_vr + 1 - visible
        } else {
            0
        };

        // Selection range in bytes (once, for quick intersection per grapheme).
        let selection =
            self.textarea.selection_range().map(|((sr, sc), (er, ec))| {
                let sb = self.char_col_to_byte(sr, sc);
                let eb = self.char_col_to_byte(er, ec);
                (sr, sb, er, eb)
            });

        // Build visible lines.
        let last_vr = (view_top + visible).min(total);
        let mut out_lines: Vec<ratatui::text::Line<'static>> =
            Vec::with_capacity(last_vr - view_top);

        for vi in view_top..last_vr {
            let vr = &layout.rows[vi];
            let logical = &lines[vr.logical_row];

            let mut spans: Vec<Span<'static>> = Vec::with_capacity(vr.graphemes.len());
            for g in &vr.graphemes {
                let piece_slice = &logical[g.byte_start..g.byte_end];
                // Substitute visual expansions for special graphemes.
                let piece: String = if piece_slice == "\t" {
                    " ".repeat(crate::components::input_bar_wrap::TAB_WIDTH)
                } else {
                    piece_slice.to_string()
                };

                let mut style = Style::default();

                // Atom styling: light-blue + underline for graphemes inside
                // any protected range that lives on this logical line.
                for atom in &self.protected_ranges {
                    if vr.logical_row == 0
                        && g.byte_start >= atom.start
                        && g.byte_end <= atom.end
                    {
                        style = style
                            .fg(Color::LightBlue)
                            .add_modifier(Modifier::UNDERLINED);
                    }
                }

                // Selection styling.
                if let Some((sr, sb, er, eb)) = selection {
                    let in_sel = if sr == er && vr.logical_row == sr {
                        g.byte_start >= sb && g.byte_end <= eb
                    } else if vr.logical_row == sr {
                        g.byte_start >= sb
                    } else if vr.logical_row == er {
                        g.byte_end <= eb
                    } else {
                        vr.logical_row > sr && vr.logical_row < er
                    };
                    if in_sel {
                        style = style.add_modifier(Modifier::REVERSED);
                    }
                }

                spans.push(Span::styled(piece, style));
            }

            out_lines.push(ratatui::text::Line::from(spans));
        }

        let paragraph = ratatui::widgets::Paragraph::new(out_lines);
        frame.render_widget(paragraph, inner);

        // Place the cursor cell if it is within the visible window.
        if cursor_vr >= view_top && cursor_vr < last_vr {
            let cx = inner.x + cursor_vc as u16;
            let cy = inner.y + (cursor_vr - view_top) as u16;
            frame.set_cursor_position((cx, cy));
        }
    }
}

#[cfg(test)]
mod required_height_tests {
    use super::*;

    #[test]
    fn required_height_empty_is_3() {
        // 1 visual row + 2 border rows.
        let bar = InputBar::new();
        assert_eq!(bar.required_height(80), 3);
    }

    #[test]
    fn required_height_wraps_long_ascii_line() {
        let mut bar = InputBar::new();
        bar.set_text("a".repeat(200), 200);
        // 200 / 80 = 3 visual rows (200 = 2*80 + 40) = ceil → 3.
        // Plus 2 border rows = 5. Clamp max is 5.
        assert_eq!(bar.required_height(82), 5); // inner width = 80
    }

    #[test]
    fn required_height_clamps_at_max_5_plus_borders() {
        let mut bar = InputBar::new();
        bar.set_text("a".repeat(10_000), 0);
        assert_eq!(bar.required_height(82), 7); // clamp(inner, 1, 5) + 2
    }

    #[test]
    fn required_height_cjk_counts_cells() {
        let mut bar = InputBar::new();
        // 10 CJK chars = 20 cells → fits in inner width 20 on one row.
        bar.set_text("你好世界你好世界你好".to_string(), 0);
        assert_eq!(bar.required_height(22), 3); // inner width = 20 → 1 row
    }
}

impl Default for InputBar {
    fn default() -> Self {
        Self::new()
    }
}
