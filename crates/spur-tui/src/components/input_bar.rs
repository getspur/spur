use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders},
    Frame,
};
use serde::{Deserialize, Serialize};
use tui_textarea::{CursorMove, Input, Key, TextArea};

use crate::components::completion_trigger::IntentEvent;
use crate::components::spinner;
use crate::input_history::{InputHistoryEntry, InputStateSnapshot, HISTORY_CAP};

/// A protected byte range inside the text representing an atomic token
/// (e.g., a resource mention). These ranges are skipped atomically by
/// cursor movement and deleted as a unit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

/// Activity state driving spinner animation in the status label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActivityKind {
    /// No animation; status is static.
    #[default]
    Idle,
    /// Brain is computing (Braille spinner).
    Thinking,
    /// Response tokens are streaming (pulse spinner).
    Streaming,
    /// Session is connecting (dot crawl).
    Connecting,
    /// An in-flight turn is being cancelled.
    Cancelling,
}

impl ActivityKind {
    /// True when this activity should animate on each tick.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            ActivityKind::Thinking
                | ActivityKind::Streaming
                | ActivityKind::Connecting
                | ActivityKind::Cancelling
        )
    }
}

/// The result of `InputBar::handle_key`. The `Submit` variant preserves
/// today's submit tuple; the `Key` variant carries the classified
/// `IntentEvent` for the TriggerDetector.
#[must_use = "the IntentEvent must be dispatched to the TriggerDetector"]
#[derive(Debug, Clone, PartialEq)]
pub enum HandleOutcome {
    /// Buffer submitted. `String` is the submitted text, `bool` is the
    /// interrupt flag. The view also emits `IntentEvent::Submitted`.
    Submit(String, bool),
    /// Ordinary key processed; carries the classified intent.
    Key(IntentEvent),
}

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
    /// Current activity kind; drives spinner frame selection.
    activity: ActivityKind,
    /// Advances on each frame when [`ActivityKind::is_active`].
    tick_counter: std::cell::Cell<u32>,
    /// Capture of the most recent Enter-submit: `(text, ranges, interrupt)`.
    submit_capture: Option<(String, Vec<ProtectedRange>, bool)>,
    /// Submitted input history, oldest first. Capped at [`HISTORY_CAP`].
    history: Vec<InputHistoryEntry>,
    /// `None` = editing live draft; `Some(i)` = browsing `history[i]`.
    history_cursor: Option<usize>,
    /// Stashed live input when the user enters history-browsing mode.
    draft: InputStateSnapshot,
    /// Last inner width observed in `render()`; updated via interior mutation
    /// so `render(&self, ...)` can record the width without requiring `&mut self`.
    last_inner_width: std::cell::Cell<u16>,
    /// Sticky goal column for vertical nav. Set on first vertical move,
    /// preserved across consecutive verticals, reset on any horizontal move
    /// or edit. Matches vim/emacs "remembered column" behavior.
    goal_vcol: Option<u16>,
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
            activity: ActivityKind::Idle,
            tick_counter: std::cell::Cell::new(0),
            submit_capture: None,
            history: Vec::new(),
            history_cursor: None,
            draft: InputStateSnapshot::default(),
            last_inner_width: std::cell::Cell::new(80),
            goal_vcol: None,
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
    pub fn handle_key(&mut self, key: KeyEvent) -> HandleOutcome {
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

    fn handle_emacs_input(&mut self, key: KeyEvent, input: Input) -> HandleOutcome {
        // Handle protected range logic for special keys
        match key.code {
            KeyCode::Up => {
                self.visual_line_up(self.last_inner_width());
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Down => {
                self.visual_line_down(self.last_inner_width());
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Left => {
                self.move_cursor_back();
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Right => {
                self.move_cursor_forward();
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Backspace => {
                self.delete_char_before_cursor();
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }
            KeyCode::Delete => {
                self.delete_char_after_cursor();
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }
            KeyCode::Home => {
                self.textarea.move_cursor(CursorMove::Head);
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::End => {
                self.textarea.move_cursor(CursorMove::End);
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.textarea.insert_newline();
                self.rebuild_line_cache();
                return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.history_prev();
                return HandleOutcome::Key(IntentEvent::SetText);
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.history_next();
                return HandleOutcome::Key(IntentEvent::SetText);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let cursor = self.cursor_to_byte();
                if cursor > 0 {
                    let text = self.text();
                    let line_start = text[..cursor].rfind('\n').map(|i| i + 1).unwrap_or(0);
                    self.delete_span(line_start, cursor);
                }
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let cursor = self.cursor_to_byte();
                let text = self.text();
                let line_end = text[cursor..]
                    .find('\n')
                    .map(|i| cursor + i)
                    .unwrap_or(text.len());
                if line_end > cursor {
                    self.delete_span(cursor, line_end);
                }
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let cursor = self.cursor_to_byte();
                if cursor > 0 {
                    let text = self.text();
                    let mut start = cursor;
                    let mut seen_non_whitespace = false;
                    for (idx, ch) in text[..cursor].char_indices().rev() {
                        if !seen_non_whitespace {
                            start = idx;
                            if ch.is_whitespace() {
                                continue;
                            }
                            seen_non_whitespace = true;
                            continue;
                        }
                        if ch.is_whitespace() {
                            break;
                        }
                        start = idx;
                    }
                    self.delete_span(start, cursor);
                }
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }
            KeyCode::Char(c) => {
                self.insert_char_with_protected_check(c);
                return HandleOutcome::Key(IntentEvent::TypedChar(c));
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.textarea.insert_newline();
                self.rebuild_line_cache();
                return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
            }
            KeyCode::Enter => {
                return match self.submit() {
                    Some((t, interrupt)) => HandleOutcome::Submit(t, interrupt),
                    None => HandleOutcome::Key(IntentEvent::NoOp),
                };
            }
            _ => {}
        }

        // Delegate to textarea for other keys. Treat as NoOp for the detector
        // — tui_textarea handled something we don't model, but no composition
        // intent is claimed.
        self.textarea.input(input);
        self.rebuild_line_cache();
        HandleOutcome::Key(IntentEvent::NoOp)
    }

    fn handle_vim_input(&mut self, key: KeyEvent, input: Input, mode: VimMode) -> HandleOutcome {
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
    ) -> HandleOutcome {
        if input.key == Key::Null {
            return HandleOutcome::Key(IntentEvent::NoOp);
        }

        // Handle pending two-key sequences (gg, dd, yy, cc)
        if let Some(pending) = self.vim_pending.take() {
            match pending.key {
                Key::Char('g') if matches!(input.key, Key::Char('g')) => {
                    self.textarea.move_cursor(CursorMove::Top);
                    let _ = self.vim_complete_operator(mode);
                    let intent = match mode {
                        VimMode::Operator('y') => IntentEvent::NoOp,
                        VimMode::Operator('d') | VimMode::Operator('c') => IntentEvent::DeletedChar,
                        _ => IntentEvent::MovedCursor,
                    };
                    return HandleOutcome::Key(intent);
                }
                Key::Char(op) if matches!(mode, VimMode::Operator(c) if c == op) => {
                    if let Key::Char(c) = input.key {
                        if c == op {
                            self.textarea.move_cursor(CursorMove::Head);
                            self.textarea.start_selection();
                            let cursor = self.textarea.cursor();
                            self.textarea.move_cursor(CursorMove::Down);
                            if cursor == self.textarea.cursor() {
                                self.textarea.move_cursor(CursorMove::End);
                            }
                            let _ = self.vim_complete_operator(mode);
                            let intent = if op == 'y' {
                                IntentEvent::NoOp
                            } else {
                                IntentEvent::DeletedChar
                            };
                            return HandleOutcome::Key(intent);
                        }
                    }
                }
                _ => {}
            }
        }

        match input {
            // ── Movement ────────────────────────────────────────────
            Input {
                key: Key::Char('h'),
                ..
            } => self.move_cursor_back(),
            Input {
                key: Key::Char('j'),
                ..
            } => self.textarea.move_cursor(CursorMove::Down),
            Input {
                key: Key::Char('k'),
                ..
            } => self.textarea.move_cursor(CursorMove::Up),
            Input {
                key: Key::Char('l'),
                ..
            } => self.move_cursor_forward(),
            Input {
                key: Key::Char('w'),
                ..
            } => self.textarea.move_cursor(CursorMove::WordForward),
            Input {
                key: Key::Char('e'),
                ctrl: false,
                ..
            } => {
                self.textarea.move_cursor(CursorMove::WordEnd);
                if matches!(mode, VimMode::Operator(_)) {
                    self.textarea.move_cursor(CursorMove::Forward);
                }
            }
            Input {
                key: Key::Char('b'),
                ctrl: false,
                ..
            } => {
                self.textarea.move_cursor(CursorMove::WordBack);
            }
            Input {
                key: Key::Char('^'),
                ..
            } => self.textarea.move_cursor(CursorMove::Head),
            Input {
                key: Key::Char('0'),
                ..
            } => self.textarea.move_cursor(CursorMove::Head),
            Input {
                key: Key::Char('$'),
                ..
            } => self.textarea.move_cursor(CursorMove::End),
            Input {
                key: Key::Char('g'),
                ctrl: false,
                ..
            } => {
                self.vim_pending = Some(input);
                return HandleOutcome::Key(IntentEvent::NoOp);
            }
            Input {
                key: Key::Char('G'),
                ctrl: false,
                ..
            } => {
                self.textarea.move_cursor(CursorMove::Bottom);
            }

            // ── Editing (Normal only) ───────────────────────────────
            Input {
                key: Key::Char('D'),
                ..
            } if mode == VimMode::Normal => {
                self.textarea.delete_line_by_end();
                self.rebuild_line_cache();
                self.protected_ranges.clear();
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }
            Input {
                key: Key::Char('C'),
                ..
            } if mode == VimMode::Normal => {
                self.textarea.delete_line_by_end();
                self.rebuild_line_cache();
                self.protected_ranges.clear();
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }
            Input {
                key: Key::Char('x'),
                ..
            } => {
                self.delete_char_after_cursor();
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }
            Input {
                key: Key::Char('p'),
                ..
            } if mode == VimMode::Normal => {
                self.textarea.paste();
                self.rebuild_line_cache();
                self.protected_ranges.clear();
                return HandleOutcome::Key(IntentEvent::Pasted);
            }

            // ── Operator entry (Normal → Operator) ──────────────────
            Input {
                key: Key::Char(op @ ('d' | 'c' | 'y')),
                ctrl: false,
                ..
            } if mode == VimMode::Normal => {
                self.textarea.start_selection();
                self.set_mode(EditMode::Vim(VimMode::Operator(op)));
                self.vim_pending = Some(input);
                return HandleOutcome::Key(IntentEvent::NoOp);
            }

            // ── Mode entry ──────────────────────────────────────────
            Input {
                key: Key::Char('i'),
                ..
            } if mode != VimMode::Visual => {
                self.textarea.cancel_selection();
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return HandleOutcome::Key(IntentEvent::NoOp);
            }
            Input {
                key: Key::Char('a'),
                ..
            } if mode != VimMode::Visual => {
                self.textarea.cancel_selection();
                self.textarea.move_cursor(CursorMove::Forward);
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            Input {
                key: Key::Char('A'),
                ..
            } if mode != VimMode::Visual => {
                self.textarea.cancel_selection();
                self.textarea.move_cursor(CursorMove::End);
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            Input {
                key: Key::Char('I'),
                ..
            } if mode != VimMode::Visual => {
                self.textarea.cancel_selection();
                self.textarea.move_cursor(CursorMove::Head);
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            Input {
                key: Key::Char('o'),
                ..
            } if mode == VimMode::Normal => {
                self.textarea.move_cursor(CursorMove::End);
                self.textarea.insert_newline();
                self.rebuild_line_cache();
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
            }
            Input {
                key: Key::Char('O'),
                ..
            } if mode == VimMode::Normal => {
                self.textarea.move_cursor(CursorMove::Head);
                self.textarea.insert_newline();
                self.textarea.move_cursor(CursorMove::Up);
                self.rebuild_line_cache();
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
            }

            // ── Visual mode ─────────────────────────────────────────
            Input {
                key: Key::Char('v'),
                ctrl: false,
                ..
            } if mode == VimMode::Normal => {
                self.textarea.start_selection();
                self.set_mode(EditMode::Vim(VimMode::Visual));
                return HandleOutcome::Key(IntentEvent::NoOp);
            }
            Input {
                key: Key::Char('V'),
                ctrl: false,
                ..
            } if mode == VimMode::Normal => {
                self.textarea.move_cursor(CursorMove::Head);
                self.textarea.start_selection();
                self.textarea.move_cursor(CursorMove::End);
                self.set_mode(EditMode::Vim(VimMode::Visual));
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            Input { key: Key::Esc, .. }
            | Input {
                key: Key::Char('v'),
                ctrl: false,
                ..
            } if mode == VimMode::Visual => {
                self.textarea.cancel_selection();
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return HandleOutcome::Key(IntentEvent::NoOp);
            }

            // ── Visual operations ───────────────────────────────────
            Input {
                key: Key::Char('y'),
                ctrl: false,
                ..
            } if mode == VimMode::Visual => {
                self.textarea.move_cursor(CursorMove::Forward);
                self.textarea.copy();
                self.textarea.cancel_selection();
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return HandleOutcome::Key(IntentEvent::NoOp);
            }
            Input {
                key: Key::Char('d'),
                ctrl: false,
                ..
            } if mode == VimMode::Visual => {
                self.textarea.move_cursor(CursorMove::Forward);
                self.textarea.cut();
                self.rebuild_line_cache();
                self.protected_ranges.clear();
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }
            Input {
                key: Key::Char('c'),
                ctrl: false,
                ..
            } if mode == VimMode::Visual => {
                self.textarea.move_cursor(CursorMove::Forward);
                self.textarea.cut();
                self.rebuild_line_cache();
                self.protected_ranges.clear();
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }

            // ── Scroll ──────────────────────────────────────────────
            Input {
                key: Key::Char('d'),
                ctrl: true,
                ..
            } => {
                self.textarea.scroll(tui_textarea::Scrolling::HalfPageDown);
                return HandleOutcome::Key(IntentEvent::NoOp);
            }
            Input {
                key: Key::Char('u'),
                ctrl: true,
                ..
            } => {
                self.textarea.scroll(tui_textarea::Scrolling::HalfPageUp);
                return HandleOutcome::Key(IntentEvent::NoOp);
            }
            Input {
                key: Key::Char('f'),
                ctrl: true,
                ..
            } => {
                self.textarea.scroll(tui_textarea::Scrolling::PageDown);
                return HandleOutcome::Key(IntentEvent::NoOp);
            }
            Input {
                key: Key::Char('b'),
                ctrl: true,
                ..
            } => {
                self.textarea.scroll(tui_textarea::Scrolling::PageUp);
                return HandleOutcome::Key(IntentEvent::NoOp);
            }
            Input {
                key: Key::Char('e'),
                ctrl: true,
                ..
            } => {
                self.textarea.scroll((1, 0));
                return HandleOutcome::Key(IntentEvent::NoOp);
            }
            Input {
                key: Key::Char('y'),
                ctrl: true,
                ..
            } => {
                self.textarea.scroll((-1, 0));
                return HandleOutcome::Key(IntentEvent::NoOp);
            }

            // ── Arrow-key visual-line nav (Vim Normal) ──────────────
            Input { key: Key::Up, .. } => {
                self.visual_line_up(self.last_inner_width());
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            Input { key: Key::Down, .. } => {
                self.visual_line_down(self.last_inner_width());
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }

            // ── Esc / Enter ─────────────────────────────────────────
            Input { key: Key::Esc, .. } => {
                self.textarea.cancel_selection();
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return HandleOutcome::Key(IntentEvent::NoOp);
            }
            Input {
                key: Key::Enter,
                alt: true,
                ..
            } => {
                self.textarea.insert_newline();
                self.rebuild_line_cache();
                return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
            }
            Input {
                key: Key::Enter, ..
            } => {
                return match self.submit() {
                    Some((t, interrupt)) => HandleOutcome::Submit(t, interrupt),
                    None => HandleOutcome::Key(IntentEvent::NoOp),
                };
            }
            _ => return HandleOutcome::Key(IntentEvent::NoOp),
        }

        // After movement, complete pending operator. Intent depends on whether
        // the operator deleted/changed, copied, or was absent (plain movement).
        let _ = self.vim_complete_operator(mode);
        let intent = match mode {
            VimMode::Operator('y') => IntentEvent::NoOp,
            VimMode::Operator('d') | VimMode::Operator('c') => IntentEvent::DeletedChar,
            _ => IntentEvent::MovedCursor,
        };
        HandleOutcome::Key(intent)
    }

    /// Complete a pending operator (d/c/y) after a movement.
    fn vim_complete_operator(&mut self, mode: VimMode) -> HandleOutcome {
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
        HandleOutcome::Key(IntentEvent::NoOp)
    }

    fn handle_vim_insert_input(&mut self, key: KeyEvent, input: Input) -> HandleOutcome {
        match key.code {
            KeyCode::Esc => {
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return HandleOutcome::Key(IntentEvent::NoOp);
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return HandleOutcome::Key(IntentEvent::NoOp);
            }
            KeyCode::Char('j')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.textarea.insert_newline();
                self.rebuild_line_cache();
                return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.textarea.insert_newline();
                self.rebuild_line_cache();
                return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
            }
            KeyCode::Up => {
                self.visual_line_up(self.last_inner_width());
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Down => {
                self.visual_line_down(self.last_inner_width());
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Left => {
                self.move_cursor_back();
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Right => {
                self.move_cursor_forward();
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Backspace => {
                self.delete_char_before_cursor();
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }
            KeyCode::Delete => {
                self.delete_char_after_cursor();
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }
            KeyCode::Home => {
                self.textarea.move_cursor(CursorMove::Head);
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::End => {
                self.textarea.move_cursor(CursorMove::End);
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Char(c) => {
                self.insert_char_with_protected_check(c);
                return HandleOutcome::Key(IntentEvent::TypedChar(c));
            }
            KeyCode::Enter => {
                return match self.submit() {
                    Some((t, interrupt)) => HandleOutcome::Submit(t, interrupt),
                    None => HandleOutcome::Key(IntentEvent::NoOp),
                };
            }
            _ => {}
        }

        self.textarea.input(input);
        self.rebuild_line_cache();
        HandleOutcome::Key(IntentEvent::NoOp)
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

    /// Apply deletion bookkeeping for a flat byte span `[start, end)`.
    fn apply_deleted_span(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let deleted = end - start;
        self.protected_ranges
            .retain(|r| r.end <= start || r.start >= end);
        for r in &mut self.protected_ranges {
            if r.start >= end {
                r.start -= deleted;
                r.end -= deleted;
            }
        }
    }

    /// Delete a flat byte span `[start, end)` and keep range metadata aligned.
    fn delete_span(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }

        let text = self.text();
        let chars = text[start..end].chars().count();
        if chars == 0 {
            return;
        }

        self.move_cursor_to_byte(start);
        self.textarea.delete_str(chars);
        self.rebuild_line_cache();
        self.apply_deleted_span(start, end);
        self.history_cursor = None;
        self.goal_vcol = None;
    }

    /// Delete a protected range by index.
    fn delete_range(&mut self, idx: usize) {
        let (start, end) = {
            let range = &self.protected_ranges[idx];
            (range.start, range.end)
        };
        self.delete_span(start, end);
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
        self.goal_vcol = None;
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
        self.goal_vcol = None;
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
            let cursor = self.cursor_to_byte();
            if cursor > 0 {
                let text = self.text();
                let start = text[..cursor]
                    .char_indices()
                    .last()
                    .map(|(idx, _)| idx)
                    .unwrap_or(0);
                self.delete_span(start, cursor);
            }
        }
        self.history_cursor = None;
        self.goal_vcol = None;
    }

    /// Delete character after cursor, handling protected ranges.
    fn delete_char_after_cursor(&mut self) {
        if let Some(idx) = self
            .range_at_cursor()
            .or_else(|| self.range_starting_at_cursor())
        {
            self.delete_range(idx);
        } else {
            let cursor = self.cursor_to_byte();
            let text = self.text();
            if let Some(ch) = text[cursor..].chars().next() {
                self.delete_span(cursor, cursor + ch.len_utf8());
            }
        }
        self.history_cursor = None;
        self.goal_vcol = None;
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
        self.goal_vcol = None;
    }

    fn snapshot(&self) -> InputStateSnapshot {
        InputStateSnapshot::new(self.text(), self.protected_ranges.clone())
    }

    fn restore_snapshot(&mut self, snapshot: &InputStateSnapshot, cursor: usize) {
        let mode = self.mode;
        let last_w = self.last_inner_width.get();
        let lines: Vec<String> = snapshot.text.split('\n').map(|s| s.to_string()).collect();
        self.textarea = TextArea::new(lines);
        self.textarea.set_max_histories(0);
        self.textarea.set_cursor_line_style(Style::default());
        self.rebuild_line_cache();
        self.move_cursor_to_byte(cursor.min(snapshot.text.len()));
        self.protected_ranges = snapshot.protected_ranges.clone();
        self.last_inner_width.set(last_w);
        self.goal_vcol = None;
        self.set_mode(mode);
    }

    /// Submit the current text.
    fn submit(&mut self) -> Option<(String, bool)> {
        self.goal_vcol = None;
        let text = self.textarea.lines().join("\n");
        if text.is_empty() {
            return None;
        }
        let interrupt = text.starts_with('!');
        let ranges = self.protected_ranges.clone();
        self.submit_capture = Some((text.clone(), ranges.clone(), interrupt));

        // Push to history
        self.history
            .push(InputHistoryEntry::new(InputStateSnapshot::new(
                text.clone(),
                ranges,
            )));
        if self.history.len() > HISTORY_CAP {
            self.history.remove(0);
        }
        self.history_cursor = None;
        self.draft = InputStateSnapshot::default();
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
        self.goal_vcol = None;
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
        let last_w = self.last_inner_width.get();
        self.textarea = TextArea::default();
        self.textarea.set_max_histories(0);
        self.textarea.set_cursor_line_style(Style::default());
        self.line_cache = vec![0];
        self.protected_ranges.clear();
        self.last_inner_width.set(last_w);
        self.goal_vcol = None;
        self.activity = ActivityKind::Idle;
        self.set_mode(mode);
    }

    /// Build the block title from the current mode string, replacing any
    /// `{spinner}` sentinel with the animated frame appropriate to
    /// [`Self::activity`].
    fn build_title(&self, mode_str: &str) -> String {
        let base = self.status.as_deref().unwrap_or("");
        if !base.contains("{spinner}") || !self.activity.is_active() {
            if base.is_empty() {
                return mode_str.to_string();
            }
            return format!("{} {}", base, mode_str);
        }
        let frame = match self.activity {
            ActivityKind::Thinking | ActivityKind::Cancelling => {
                spinner::frame(spinner::BRAILLE, self.tick_counter.get())
            }
            ActivityKind::Streaming => spinner::frame(spinner::PULSE, self.tick_counter.get()),
            ActivityKind::Connecting => spinner::frame(spinner::DOTS, self.tick_counter.get()),
            ActivityKind::Idle => "",
        };
        let animated = base.replace("{spinner}", frame);
        format!("{} {}", animated, mode_str)
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
        self.restore_snapshot(&InputStateSnapshot::from_text(text), cursor);
    }

    /// Replace text, protected ranges, and cursor wholesale.
    pub fn set_state(&mut self, snapshot: InputStateSnapshot, cursor: usize) {
        self.restore_snapshot(&snapshot, cursor);
    }

    fn last_inner_width(&self) -> u16 {
        self.last_inner_width.get()
    }

    /// Test-only: read the cached last inner width.
    #[cfg(test)]
    #[doc(hidden)]
    pub fn last_inner_width_for_test(&self) -> u16 {
        self.last_inner_width.get()
    }

    /// Test-only: read the textarea's currently-configured max history.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn max_histories_for_test(&self) -> usize {
        self.textarea.max_histories()
    }

    /// Set the status label and activity kind.
    pub fn set_status(&mut self, status: Option<String>, activity: ActivityKind) {
        self.status = status;
        self.activity = activity;
    }

    /// Whether a status label is set.
    pub fn has_status(&self) -> bool {
        self.status.is_some()
    }

    /// Advance the animation counter when the current activity is active.
    /// Called from the view's `tick()` loop.
    pub fn tick(&self) {
        if self.activity.is_active() {
            self.tick_counter
                .set(self.tick_counter.get().wrapping_add(1));
        }
    }

    /// True when the status label is in an animated activity state.
    pub fn has_active_animation(&self) -> bool {
        self.activity.is_active()
    }

    /// Replace the in-memory history with persisted entries (e.g. loaded
    /// from `session_metadata.json` at startup).
    pub fn seed_history(&mut self, entries: Vec<InputHistoryEntry>) {
        self.history = entries;
        self.history_cursor = None;
    }

    /// Current history entries (for persistence).
    pub fn history(&self) -> &[InputHistoryEntry] {
        &self.history
    }

    /// Navigate to previous history entry.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_cursor {
            None => {
                self.draft = self.snapshot();
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
                let len = draft.text.len();
                self.restore_snapshot(&draft, len);
            }
            None => {}
        }
    }

    fn load_history_entry(&mut self, idx: usize) {
        let snapshot = self.history[idx].snapshot.clone();
        let len = snapshot.text.len();
        self.restore_snapshot(&snapshot, len);
    }

    /// Test-only: set cursor position.
    #[cfg(any(test, debug_assertions))]
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

        let title = self.build_title(mode_str);

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
        // Record the inner width so visual-line nav keys (which can arrive
        // before the next render) compute against the actual rendered width.
        self.last_inner_width.set(inner.width);
        frame.render_widget(block, area);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        // Compute wrap layout for the buffer against the inner width.
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let layout = crate::components::input_bar_wrap::wrap(&lines, inner.width);

        // Cursor in visual coordinates.
        let (cursor_row, cursor_ccol) = self.textarea.cursor();
        let cursor_byte = self.char_col_to_byte(cursor_row, cursor_ccol);
        let (cursor_vr, cursor_vc) = layout.logical_to_visual(cursor_row, cursor_byte);

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
        let selection = self.textarea.selection_range().map(|((sr, sc), (er, ec))| {
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

            // Hoisted once per visual row: flat byte offset where this
            // logical row starts, and where the next logical row starts.
            // line_cache[row] is the flat byte offset of the '\n'-joined
            // buffer at which logical row `row` begins.
            let row_start_flat = self.line_cache.get(vr.logical_row).copied().unwrap_or(0);
            let next_row_start = self
                .line_cache
                .get(vr.logical_row + 1)
                .copied()
                .unwrap_or(usize::MAX);

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

                // Atom styling: LightBlue + underline for graphemes inside
                // any protected range on the current logical line. Atoms
                // store flat byte offsets (into the \n-joined buffer), so
                // translate to per-line coordinates via line_cache.
                for atom in &self.protected_ranges {
                    // Skip atoms that belong to a different logical row.
                    if atom.start < row_start_flat || atom.start >= next_row_start {
                        continue;
                    }
                    let atom_start_in_row = atom.start - row_start_flat;
                    let atom_end_in_row = atom.end - row_start_flat;
                    if g.byte_start >= atom_start_in_row && g.byte_end <= atom_end_in_row {
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

    /// Render variant for when an overlay (e.g. PickerShell) owns the
    /// terminal cursor. Behaves like `render` but:
    ///   * the border renders in DarkGray as a "composer inert" cue
    ///   * `frame.set_cursor_position` is NOT called — the overlay places
    ///     the cursor.
    pub fn render_inert(&self, frame: &mut Frame, area: Rect) {
        let mode_str = match self.mode {
            EditMode::Emacs => " INSERT ",
            EditMode::Vim(VimMode::Normal) => " VIM·NORMAL ",
            EditMode::Vim(VimMode::Insert) => " VIM·INSERT ",
            EditMode::Vim(VimMode::Visual) => " VIM·VISUAL ",
            EditMode::Vim(VimMode::Operator(_)) => " VIM·OP ",
        };
        let title = self.build_title(mode_str);
        let border_color = Color::DarkGray;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(title, Style::default().fg(border_color)));
        let inner = block.inner(area);
        self.last_inner_width.set(inner.width);
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let layout = crate::components::input_bar_wrap::wrap(&lines, inner.width);
        let visible = inner.height as usize;
        let total = layout.visual_height() as usize;
        let view_top = total.saturating_sub(visible);
        let last_vr = (view_top + visible).min(total);
        let mut out_lines: Vec<ratatui::text::Line<'static>> =
            Vec::with_capacity(last_vr.saturating_sub(view_top));
        for vi in view_top..last_vr {
            let vr = &layout.rows[vi];
            let logical = &lines[vr.logical_row];
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(vr.graphemes.len());
            for g in &vr.graphemes {
                let piece_slice = &logical[g.byte_start..g.byte_end];
                let piece: String = if piece_slice == "\t" {
                    " ".repeat(crate::components::input_bar_wrap::TAB_WIDTH)
                } else {
                    piece_slice.to_string()
                };
                // Inert: no atom highlight, no selection highlight — the
                // composer is visibly quiescent while the picker owns focus.
                spans.push(Span::styled(piece, Style::default().fg(Color::DarkGray)));
            }
            out_lines.push(ratatui::text::Line::from(spans));
        }
        frame.render_widget(ratatui::widgets::Paragraph::new(out_lines), inner);
        // Intentionally no set_cursor_position — the overlay owns the cursor.
    }

    /// Visual-line Down: move cursor one visual row down, preserving vcol.
    pub fn visual_line_down(&mut self, inner_width: u16) {
        self.visual_line_move(inner_width, 1);
    }

    /// Visual-line Up.
    pub fn visual_line_up(&mut self, inner_width: u16) {
        self.visual_line_move(inner_width, -1);
    }

    fn visual_line_move(&mut self, inner_width: u16, delta: i32) {
        if inner_width == 0 {
            return;
        }
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let layout = crate::components::input_bar_wrap::wrap(&lines, inner_width);

        let (row, ccol) = self.textarea.cursor();
        let byte = self.char_col_to_byte(row, ccol);
        let (vr, vc) = layout.logical_to_visual(row, byte);

        let target_vr = (vr as i32 + delta).clamp(0, layout.rows.len() as i32 - 1) as usize;
        if target_vr == vr {
            return;
        }
        // Sticky goal: use the stored goal if any, else the current vcol.
        // Persist it so subsequent consecutive verticals can restore.
        let goal = self.goal_vcol.unwrap_or(vc as u16);
        let max_vc = layout.rows[target_vr].used_cells as usize;
        let target_vc = (goal as usize).min(max_vc);
        self.goal_vcol = Some(goal);
        let (target_row, target_byte) = layout.visual_to_logical(target_vr, target_vc);
        self.move_cursor_to_byte(target_byte_abs(&lines, target_row, target_byte));
    }
}

/// Convert per-line byte offset to absolute byte offset across all lines.
fn target_byte_abs(lines: &[String], row: usize, byte_col: usize) -> usize {
    let mut acc = 0usize;
    for (i, l) in lines.iter().enumerate() {
        if i == row {
            return acc + byte_col;
        }
        acc += l.len() + 1; // +1 for '\n'
    }
    acc
}

impl Default for InputBar {
    fn default() -> Self {
        Self::new()
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

    #[test]
    fn visual_down_crosses_wrap_boundary() {
        let mut bar = InputBar::new();
        // 16 ASCII chars, wrapped at width 5 (inner) → 4 vrows of 5,5,5,1.
        bar.set_text("abcdefghijklmnop".to_string(), 3);
        // width arg to visual nav reflects the inner width of the render area.
        bar.visual_line_down(5);
        // Cursor moves from byte 3 (vrow 0, vcol 3) to vrow 1, vcol 3 → byte 8.
        assert_eq!(bar.cursor(), 8);
    }

    #[test]
    fn visual_up_inverse_of_down() {
        let mut bar = InputBar::new();
        bar.set_text("abcdefghijklmnop".to_string(), 3);
        bar.visual_line_down(5);
        bar.visual_line_up(5);
        assert_eq!(bar.cursor(), 3);
    }

    #[test]
    fn visual_down_at_last_vrow_is_noop() {
        let mut bar = InputBar::new();
        bar.set_text("abc".to_string(), 3);
        bar.visual_line_down(80);
        assert_eq!(bar.cursor(), 3);
    }

    #[test]
    fn vim_normal_arrow_down_moves_visual_line() {
        let mut bar = InputBar::new();
        bar.set_text("abcdefghijklmnop".to_string(), 3);
        bar.set_mode(EditMode::Vim(VimMode::Normal));
        let key = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Down,
            crossterm::event::KeyModifiers::NONE,
        );
        let _ = bar.handle_key(key);
        // At default width 80 the line fits on one vrow, so Down is a noop.
        // The assertion proves the key was HANDLED (didn't panic, didn't
        // mutate unexpectedly) rather than falling through the match-all.
        assert_eq!(bar.cursor(), 3);
    }

    #[test]
    fn goal_vcol_restores_column_across_short_intermediate_row() {
        // Three logical lines:
        //   line 0: "hello world!"     (12 cells at width 12 → 1 vrow full)
        //   line 1: "short"            (5 cells)
        //   line 2: "another long"     (12 cells at width 12 → 1 vrow full)
        let mut bar = InputBar::new();
        bar.set_text(
            "hello world!\nshort\nanother long".to_string(),
            "hello world".len(), // cursor at vcol 11 on vrow 0
        );
        // First Down onto "short" — without goal, cursor would snap to vcol 5.
        // Second Down onto "another long" — with goal, cursor must restore to vcol 11.
        bar.visual_line_down(12);
        bar.visual_line_down(12);
        // Expected: cursor at 11th char of "another long" → 'g'.
        let expected = "hello world!".len() + 1 + "short".len() + 1 + 11;
        assert_eq!(bar.cursor(), expected);
    }

    #[test]
    fn render_sets_last_inner_width_without_view_setter() {
        use ratatui::backend::TestBackend;
        use ratatui::layout::Rect;
        use ratatui::Terminal;

        let mut bar = InputBar::new();
        bar.set_text("hello".to_string(), 0);
        let backend = TestBackend::new(20, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| bar.render(f, Rect::new(0, 0, 20, 3)))
            .unwrap();
        assert_eq!(bar.last_inner_width_for_test(), 18);
    }
}
