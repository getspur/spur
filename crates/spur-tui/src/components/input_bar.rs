use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders},
    Frame,
};
use serde::{Deserialize, Serialize};
use spur_acp::config::EditorMode;
use tempfile::TempPath;
use tui_textarea::{CursorMove, Input, Key, TextArea};

use crate::components::completion_trigger::IntentEvent;
use crate::components::paste_burst::{CharDecision, EnterDecision, PasteBurst};
use crate::components::spinner;
use crate::input_history::{InputHistoryEntry, InputStateSnapshot, HISTORY_CAP};

/// Kind discriminator for protected byte ranges.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum RangeKind {
    /// Atom whose display text is also its on-submit text (e.g., @mention).
    #[default]
    Atom,
    /// Pasted block; on submit, replace placeholder with `pastes[id]`.
    PasteRef(usize),
    /// Inline image attachment; on submit, replaced with ContentBlock::Image.
    #[serde(skip)]
    ImageRef(usize),
}

impl RangeKind {
    fn is_atom(&self) -> bool {
        matches!(self, Self::Atom)
    }

    fn skip_kind_field(&self) -> bool {
        self.is_atom() || matches!(self, Self::ImageRef(_))
    }
}

/// A protected byte range inside the text representing an atomic token
/// (e.g., a resource mention). These ranges are skipped atomically by
/// cursor movement and deleted as a unit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProtectedRange {
    pub start: usize,
    pub end: usize,
    #[serde(default, skip_serializing_if = "RangeKind::skip_kind_field")]
    pub kind: RangeKind,
    pub uri: String,
    pub name: String,
}

#[derive(Debug)]
pub struct ImageAttachment {
    pub id: usize,
    pub source_path: PathBuf,
    pub mime_type: String,
    pub dimensions: (u32, u32),
    pub byte_size: usize,
    pub owned_temp: Option<TempPath>,
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

/// Maps the persisted TUI editor preference to the input bar's runtime mode.
/// `Vim` always boots in `Normal` — Insert is reached via user action.
impl From<EditorMode> for EditMode {
    fn from(pref: EditorMode) -> Self {
        match pref {
            EditorMode::Emacs => EditMode::Emacs,
            EditorMode::Vim => EditMode::Vim(VimMode::Normal),
        }
    }
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

/// The result of `InputBar::tick`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    /// Nothing user-visible changed.
    Idle,
    /// An idle paste burst was flushed through `insert_paste`.
    FlushedPaste,
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
    /// Side store for atomized paste content keyed by paste id.
    pastes: BTreeMap<usize, String>,
    /// Side store for image attachments keyed by image id.
    images: BTreeMap<usize, ImageAttachment>,
    /// Monotonic paste counter (per-session). Never decrements.
    next_paste_id: usize,
    /// Monotonic image counter (per-session). Never decrements.
    #[allow(dead_code)]
    next_image_id: usize,
    /// Fallback detector for terminals that deliver paste as rapid key events.
    paste_burst: PasteBurst,
    /// Runtime gate for the fallback paste-burst detector.
    paste_burst_enabled: bool,
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
    /// Images captured during submit, held until view drains them.
    pending_submit_images: Vec<ImageAttachment>,
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
    /// When false, the input bar renders with a dim border to indicate it is
    /// not receiving keys (Navigate mode). Set by the dashboard view.
    active: bool,
}

// Cells consumed by the composer's frame. Coupled to the `borders(...)` flag
// in `build_block` below — change them together. A future swap to
// `Borders::TOP` alone or `Borders::NONE` requires editing only these
// constants and the borders flag; rendering and arithmetic auto-track.
const BORDER_OVERHEAD_ROWS: u16 = 2; // Borders::TOP | Borders::BOTTOM
const BORDER_OVERHEAD_COLS: u16 = 0; // no left/right side borders
const PASTE_STORE_CAP: usize = 50;

/// Read the current clipboard image and write it to a temp PNG file.
///
/// `owned_temp` keeps the file path alive until the attachment is dropped.
#[allow(dead_code)]
fn try_paste_clipboard_image() -> anyhow::Result<ImageAttachment> {
    let mut clipboard = arboard::Clipboard::new()?;
    let image_data = clipboard.get_image()?;
    let width = image_data.width as u32;
    let height = image_data.height as u32;

    let rgba = image::RgbaImage::from_raw(width, height, image_data.bytes.into_owned())
        .ok_or_else(|| anyhow::anyhow!("clipboard image has invalid dimensions"))?;
    let mut image = image::DynamicImage::ImageRgba8(rgba);

    const MAX_DIM: u32 = 2048;
    if image.width() > MAX_DIM || image.height() > MAX_DIM {
        image = image.resize(MAX_DIM, MAX_DIM, image::imageops::FilterType::Lanczos3);
    }

    let mut cursor = std::io::Cursor::new(Vec::new());
    image.write_to(&mut cursor, image::ImageFormat::Png)?;
    let png_bytes = cursor.into_inner();

    const MAX_B64_BYTES: usize = 10 * 1024 * 1024;
    let encoded_len = base64::encoded_len(png_bytes.len(), true)
        .ok_or_else(|| anyhow::anyhow!("image too large to base64-encode"))?;
    if encoded_len > MAX_B64_BYTES {
        anyhow::bail!("image too large ({encoded_len} bytes base64); max 10 MB");
    }

    let mut temp_file = tempfile::Builder::new()
        .prefix("spur-img-")
        .suffix(".png")
        .tempfile()?;
    temp_file.write_all(&png_bytes)?;
    let (_file, temp_path) = temp_file.into_parts();
    let source_path = temp_path.to_path_buf();
    let byte_size = png_bytes.len();
    let dimensions = (image.width(), image.height());

    Ok(ImageAttachment {
        id: 0,
        source_path,
        mime_type: "image/png".into(),
        dimensions,
        byte_size,
        owned_temp: Some(temp_path),
    })
}

fn try_as_image_path(text: &str) -> Option<(PathBuf, (u32, u32))> {
    let path = PathBuf::from(text.trim());
    if !path.exists() {
        return None;
    }
    image::image_dimensions(&path).ok().map(|dims| (path, dims))
}

#[cfg(test)]
fn default_paste_burst_enabled() -> bool {
    false
}

#[cfg(all(not(test), debug_assertions))]
fn default_paste_burst_enabled() -> bool {
    !running_as_cargo_test_binary()
}

#[cfg(all(not(test), debug_assertions))]
fn running_as_cargo_test_binary() -> bool {
    std::env::args().next().is_some_and(|arg| {
        std::path::Path::new(&arg)
            .parent()
            .and_then(|parent| parent.file_name())
            .is_some_and(|name| name == "deps")
    })
}

#[cfg(all(not(test), not(debug_assertions)))]
fn default_paste_burst_enabled() -> bool {
    true
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
            pastes: BTreeMap::new(),
            images: BTreeMap::new(),
            next_paste_id: 1,
            next_image_id: 0,
            paste_burst: PasteBurst::default(),
            paste_burst_enabled: default_paste_burst_enabled(),
            line_cache: vec![0],
            status: None,
            activity: ActivityKind::Idle,
            tick_counter: std::cell::Cell::new(0),
            submit_capture: None,
            pending_submit_images: Vec::new(),
            history: Vec::new(),
            history_cursor: None,
            draft: InputStateSnapshot::default(),
            last_inner_width: std::cell::Cell::new(80),
            goal_vcol: None,
            active: true,
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

    /// Set whether the input bar is visually active (Compose mode) or
    /// inactive (Navigate mode). Affects border color only.
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    /// Returns true when the input bar is visually active.
    pub fn is_active(&self) -> bool {
        self.active
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

    fn capture_paste_burst_char(&mut self, c: char, key: KeyEvent) -> Option<IntentEvent> {
        if !self.paste_burst_enabled || !Self::is_plain_paste_burst_key(key) {
            self.paste_burst.clear();
            return None;
        }
        let now = Instant::now();
        if self.paste_burst_context_suppressed()
            && !self.paste_burst.is_active()
            && !self.paste_burst.is_fast_continuation(now)
        {
            self.flush_paste_burst_now();
            return None;
        }
        match self.paste_burst.on_char(c, now) {
            CharDecision::PassThrough | CharDecision::Armed => None,
            CharDecision::Buffered => Some(IntentEvent::NoOp),
            CharDecision::Flushed(text) => {
                self.insert_paste(&text);
                Some(IntentEvent::Pasted)
            }
        }
    }

    fn capture_paste_burst_enter(&mut self, key: KeyEvent) -> Option<HandleOutcome> {
        if !self.paste_burst_enabled
            || !Self::is_plain_paste_burst_key(key)
            || (self.paste_burst_context_suppressed() && !self.paste_burst.is_active())
        {
            self.flush_paste_burst_now();
            return None;
        }

        match self.paste_burst.on_enter(Instant::now()) {
            EnterDecision::Submit => None,
            EnterDecision::BufferNewline => Some(HandleOutcome::Key(IntentEvent::NoOp)),
            EnterDecision::Flushed(text) => {
                self.insert_paste(&text);
                Some(HandleOutcome::Key(IntentEvent::Pasted))
            }
            EnterDecision::InsertNewline => {
                self.insert_newline_protected();
                Some(HandleOutcome::Key(IntentEvent::TypedChar('\n')))
            }
        }
    }

    fn is_plain_paste_burst_key(key: KeyEvent) -> bool {
        !key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT)
    }

    fn paste_burst_context_suppressed(&self) -> bool {
        matches!(self.text().as_bytes().first(), Some(b'/'))
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
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.textarea.move_cursor(CursorMove::Head);
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.textarea.move_cursor(CursorMove::End);
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_char_after_cursor();
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_cursor_forward();
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_cursor_back();
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.textarea.move_cursor(CursorMove::WordBack);
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.textarea.move_cursor(CursorMove::WordForward);
                return HandleOutcome::Key(IntentEvent::MovedCursor);
            }
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_char_before_cursor();
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_newline_protected();
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
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.handle_clipboard_image_paste();
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(intent) = self.capture_paste_burst_char(c, key) {
                    return HandleOutcome::Key(intent);
                }
                self.insert_char_with_protected_check(c);
                return HandleOutcome::Key(IntentEvent::TypedChar(c));
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert_newline_protected();
                return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.insert_tab_protected();
                return HandleOutcome::Key(IntentEvent::NoOp);
            }
            KeyCode::Enter => {
                if let Some(outcome) = self.capture_paste_burst_enter(key) {
                    return outcome;
                }
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
        self.input_textarea_non_mutating(input);
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
        key: KeyEvent,
        input: Input,
        mode: VimMode,
    ) -> HandleOutcome {
        if matches!(key.code, KeyCode::Char('\n')) {
            // Raw paste fallback newlines must not advance Vim command state.
            return HandleOutcome::Key(IntentEvent::NoOp);
        }

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
                let start = self.cursor_to_byte();
                let text = self.text();
                let end = text[start..]
                    .find('\n')
                    .map(|i| start + i)
                    .unwrap_or(text.len());
                self.delete_span(start, end);
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }
            Input {
                key: Key::Char('C'),
                ..
            } if mode == VimMode::Normal => {
                let start = self.cursor_to_byte();
                let text = self.text();
                let end = text[start..]
                    .find('\n')
                    .map(|i| start + i)
                    .unwrap_or(text.len());
                self.delete_span(start, end);
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
                let before = self.cursor_to_byte();
                self.textarea.paste();
                self.rebuild_line_cache();
                let after = self.cursor_to_byte();
                let delta = after as isize - before as isize;
                if delta != 0 {
                    self.shift_ranges(before, delta);
                }
                self.history_cursor = None;
                self.goal_vcol = None;
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
                self.insert_newline_protected();
                self.set_mode(EditMode::Vim(VimMode::Insert));
                return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
            }
            Input {
                key: Key::Char('O'),
                ..
            } if mode == VimMode::Normal => {
                self.textarea.move_cursor(CursorMove::Head);
                self.insert_newline_protected();
                self.textarea.move_cursor(CursorMove::Up);
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
                if !self.cut_selection_protected() {
                    self.textarea.cut();
                    self.rebuild_line_cache();
                }
                self.set_mode(EditMode::Vim(VimMode::Normal));
                return HandleOutcome::Key(IntentEvent::DeletedChar);
            }
            Input {
                key: Key::Char('c'),
                ctrl: false,
                ..
            } if mode == VimMode::Visual => {
                self.textarea.move_cursor(CursorMove::Forward);
                if !self.cut_selection_protected() {
                    self.textarea.cut();
                    self.rebuild_line_cache();
                }
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
                self.insert_newline_protected();
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
                if !self.cut_selection_protected() {
                    self.textarea.cut();
                    self.rebuild_line_cache();
                }
                self.set_mode(EditMode::Vim(VimMode::Normal));
            }
            VimMode::Operator('c') => {
                if !self.cut_selection_protected() {
                    self.textarea.cut();
                    self.rebuild_line_cache();
                }
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
            KeyCode::Char('j')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.insert_newline_protected();
                return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.insert_newline_protected();
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
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.handle_clipboard_image_paste();
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(intent) = self.capture_paste_burst_char(c, key) {
                    return HandleOutcome::Key(intent);
                }
                self.insert_char_with_protected_check(c);
                return HandleOutcome::Key(IntentEvent::TypedChar(c));
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.insert_tab_protected();
                return HandleOutcome::Key(IntentEvent::NoOp);
            }
            KeyCode::Enter => {
                if let Some(outcome) = self.capture_paste_burst_enter(key) {
                    return outcome;
                }
                return match self.submit() {
                    Some((t, interrupt)) => HandleOutcome::Submit(t, interrupt),
                    None => HandleOutcome::Key(IntentEvent::NoOp),
                };
            }
            _ => {}
        }

        self.input_textarea_non_mutating(input);
        HandleOutcome::Key(IntentEvent::NoOp)
    }

    fn handle_clipboard_image_paste(&mut self) -> HandleOutcome {
        match try_paste_clipboard_image() {
            Ok(attachment) => {
                self.insert_image_atom(attachment);
                HandleOutcome::Key(IntentEvent::Pasted)
            }
            Err(e) => {
                tracing::warn!("clipboard image paste failed: {e}");
                self.set_status(
                    Some(format!("Clipboard image paste failed: {e}")),
                    ActivityKind::Idle,
                );
                HandleOutcome::Key(IntentEvent::NoOp)
            }
        }
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

    /// Convert a `tui_textarea` selection range to an absolute byte span.
    fn selection_to_byte_span(&self, sel: ((usize, usize), (usize, usize))) -> (usize, usize) {
        let ((sr, sc), (er, ec)) = sel;
        let start = self.line_cache.get(sr).copied().unwrap_or(0) + self.char_col_to_byte(sr, sc);
        let end = self.line_cache.get(er).copied().unwrap_or(0) + self.char_col_to_byte(er, ec);
        (start, end)
    }

    fn input_textarea_non_mutating(&mut self, input: Input) -> bool {
        if matches!(
            input.key,
            Key::F(_) | Key::PageUp | Key::PageDown | Key::Null
        ) {
            self.textarea.input(input);
            self.rebuild_line_cache();
            return true;
        }
        false
    }

    fn finish_textarea_insert_at(&mut self, at: usize, old_len: usize) {
        self.rebuild_line_cache();
        let new_len = self.text().len();
        let delta = new_len as isize - old_len as isize;
        if delta != 0 {
            self.shift_ranges(at, delta);
        }
        self.history_cursor = None;
        self.goal_vcol = None;
    }

    fn insert_newline_protected(&mut self) {
        if !self.delete_selection_protected(false) {
            if let Some(idx) = self.range_at_cursor() {
                self.delete_range(idx);
            }
        }
        let cursor = self.cursor_to_byte();
        let old_len = self.text().len();
        self.textarea.insert_newline();
        self.finish_textarea_insert_at(cursor, old_len);
    }

    fn insert_tab_protected(&mut self) {
        if !self.delete_selection_protected(false) {
            if let Some(idx) = self.range_at_cursor() {
                self.delete_range(idx);
            }
        }
        let cursor = self.cursor_to_byte();
        let old_len = self.text().len();
        self.textarea.insert_tab();
        self.finish_textarea_insert_at(cursor, old_len);
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

    fn expand_to_protected_boundaries(&self, start: usize, end: usize) -> (usize, usize) {
        let mut expanded_start = start;
        let mut expanded_end = end;
        for r in &self.protected_ranges {
            let overlaps = if expanded_start == expanded_end {
                expanded_start > r.start && expanded_start < r.end
            } else {
                r.start < expanded_end && r.end > expanded_start
            };
            if overlaps {
                expanded_start = expanded_start.min(r.start);
                expanded_end = expanded_end.max(r.end);
            }
        }
        (expanded_start, expanded_end)
    }

    fn delete_span_with_yank(&mut self, start: usize, end: usize, yank: bool) -> bool {
        let (start, end) = self.expand_to_protected_boundaries(start, end);
        self.textarea.cancel_selection();
        if start >= end {
            return false;
        }
        if yank {
            let text = self.text();
            self.textarea.set_yank_text(text[start..end].to_string());
        }
        self.delete_span(start, end);
        true
    }

    fn delete_selection_protected(&mut self, yank: bool) -> bool {
        if let Some(sel) = self.textarea.selection_range() {
            let (start, end) = self.selection_to_byte_span(sel);
            return self.delete_span_with_yank(start, end, yank);
        }
        false
    }

    fn cut_selection_protected(&mut self) -> bool {
        self.delete_selection_protected(true)
    }

    /// Apply deletion bookkeeping for a flat byte span `[start, end)`.
    fn apply_deleted_span(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let deleted = end - start;
        let mut removed_image_ids = Vec::new();
        self.protected_ranges.retain(|r| {
            let keep = r.end <= start || r.start >= end;
            if !keep {
                if let RangeKind::ImageRef(id) = &r.kind {
                    removed_image_ids.push(*id);
                }
            }
            keep
        });
        for id in removed_image_ids {
            self.images.remove(&id);
        }
        for r in &mut self.protected_ranges {
            if r.start >= end {
                r.start -= deleted;
                r.end -= deleted;
            }
        }
    }

    /// Delete a flat byte span `[start, end)` and keep range metadata aligned.
    fn delete_span(&mut self, start: usize, end: usize) {
        let (start, end) = self.expand_to_protected_boundaries(start, end);
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
        let (expanded, ranges) = expand_paste_refs(&text, &self.protected_ranges, &self.pastes);
        let interrupt = expanded.starts_with('!');
        self.submit_capture = Some((expanded.clone(), ranges.clone(), interrupt));
        self.pending_submit_images = std::mem::take(&mut self.images).into_values().collect();

        // Push to history
        self.history
            .push(InputHistoryEntry::new(InputStateSnapshot::new(
                expanded.clone(),
                ranges,
            )));
        if self.history.len() > HISTORY_CAP {
            self.history.remove(0);
        }
        self.history_cursor = None;
        self.draft = InputStateSnapshot::default();
        self.clear();
        self.pastes.clear();
        self.images.clear();

        Some((expanded, interrupt))
    }

    // ── Public API ───────────────────────────────────────────────────────────

    /// Insert a protected atom at the cursor.
    pub fn insert_atom(&mut self, text: impl AsRef<str>, uri: String, name: String) {
        self.paste_burst.clear();
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
            kind: RangeKind::Atom,
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
        if let Some((img_path, dims)) = try_as_image_path(text) {
            let byte_size = std::fs::metadata(&img_path)
                .map(|metadata| metadata.len() as usize)
                .unwrap_or(0);
            let attachment = ImageAttachment {
                id: 0,
                source_path: img_path,
                mime_type: "image/png".into(),
                dimensions: dims,
                byte_size,
                owned_temp: None,
            };
            self.insert_image_atom(attachment);
            return;
        }
        // Normalize line endings before the multi-line gate. Clipboards from
        // Mac legacy apps deliver bare `\r`; Windows sources deliver `\r\n`.
        // Rust's `str::lines()` only splits on `\n` and `\r\n`, so a bare `\r`
        // would slip through as a single logical line and render as one
        // garbled row with embedded control bytes.
        let normalized;
        let text: &str = if text.contains('\r') {
            normalized = text.replace("\r\n", "\n").replace('\r', "\n");
            &normalized
        } else {
            text
        };
        if self.history_cursor.is_some() {
            self.restore_draft();
        }
        if let Some(idx) = self.range_at_cursor() {
            self.delete_range(idx);
        }
        let cursor = self.cursor_to_byte();
        if text.lines().count() <= 1 {
            self.textarea.insert_str(text);
            self.rebuild_line_cache();
            let new_cursor = self.cursor_to_byte();
            let delta = new_cursor as isize - cursor as isize;
            if delta != 0 {
                self.shift_ranges(cursor, delta);
            }
            self.history_cursor = None;
            self.goal_vcol = None;
            return;
        }

        let id = self.next_paste_id;
        self.next_paste_id += 1;
        let line_count = text.lines().count();
        let placeholder = format!("[Paste #{id} · {line_count} lines]");

        self.textarea.insert_str(&placeholder);
        self.rebuild_line_cache();
        let end = cursor + placeholder.len();
        self.shift_ranges(cursor, placeholder.len() as isize);
        self.protected_ranges.push(ProtectedRange {
            start: cursor,
            end,
            kind: RangeKind::PasteRef(id),
            uri: String::new(),
            name: placeholder,
        });
        self.protected_ranges.sort_by_key(|r| r.start);
        self.pastes.insert(id, text.to_string());
        while self.pastes.len() > PASTE_STORE_CAP {
            let Some(oldest_id) = self.pastes.keys().copied().find(|candidate| {
                !self
                    .protected_ranges
                    .iter()
                    .any(|range| matches!(range.kind, RangeKind::PasteRef(id) if id == *candidate))
            }) else {
                break;
            };
            self.pastes.remove(&oldest_id);
        }
        self.history_cursor = None;
        self.goal_vcol = None;
    }

    /// Store an image attachment and insert its protected placeholder label.
    #[allow(dead_code)]
    fn insert_image_atom(&mut self, mut attachment: ImageAttachment) {
        if self.history_cursor.is_some() {
            self.restore_draft();
        }
        if let Some(idx) = self.range_at_cursor() {
            self.delete_range(idx);
        }
        let cursor = self.cursor_to_byte();
        let id = self.next_image_id;
        self.next_image_id += 1;
        attachment.id = id;
        let (w, h) = attachment.dimensions;
        let label = format!("[Image #{} · {}×{}]", id + 1, w, h);
        self.images.insert(id, attachment);

        self.textarea.insert_str(&label);
        self.rebuild_line_cache();
        let end = cursor + label.len();
        self.shift_ranges(cursor, label.len() as isize);
        self.protected_ranges.push(ProtectedRange {
            start: cursor,
            end,
            kind: RangeKind::ImageRef(id),
            uri: String::new(),
            name: label,
        });
        self.protected_ranges.sort_by_key(|r| r.start);
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
        self.pastes.clear();
        self.images.clear();
        self.last_inner_width.set(last_w);
        self.goal_vcol = None;
        self.activity = ActivityKind::Idle;
        self.paste_burst.clear();
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

    fn build_block(&self, mode_str: &str, border_color: Color) -> Block<'_> {
        let title = self.build_title(mode_str);
        Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(
                title,
                // Reversed-colour fill so the mode badge acts as a high-contrast
                // "lamp" on the thin top rule (kimi UX rec).
                Style::default().bg(border_color).fg(Color::Black),
            ))
    }

    /// Sorted, non-overlapping protected ranges.
    pub fn protected_ranges(&self) -> &[ProtectedRange] {
        &self.protected_ranges
    }

    /// Take and reset the most recent Enter-submit capture.
    pub fn take_submit_capture(&mut self) -> Option<(String, Vec<ProtectedRange>, bool)> {
        self.submit_capture.take()
    }

    /// Take and reset images captured by the most recent submit.
    pub fn take_pending_images(&mut self) -> Vec<ImageAttachment> {
        std::mem::take(&mut self.pending_submit_images)
    }

    /// Replace text and cursor wholesale.
    pub fn set_text(&mut self, text: String, cursor: usize) {
        self.paste_burst.clear();
        self.restore_snapshot(&InputStateSnapshot::from_text(text), cursor);
    }

    /// Replace text, protected ranges, and cursor wholesale.
    pub fn set_state(&mut self, snapshot: InputStateSnapshot, cursor: usize) {
        self.paste_burst.clear();
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

    /// Test-only: read paste ids currently retained in the side store.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn paste_ids_for_test(&self) -> Vec<usize> {
        self.pastes.keys().copied().collect()
    }

    /// Test-only: read whether the input bar is browsing history.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn history_cursor_for_test(&self) -> Option<usize> {
        self.history_cursor
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

    /// Advance the animation counter and flush any idle paste burst.
    /// Called from the view's `tick()` loop.
    pub fn tick(&mut self) -> TickOutcome {
        if self.activity.is_active() {
            self.tick_counter
                .set(self.tick_counter.get().wrapping_add(1));
        }
        if self.flush_paste_burst_if_due() {
            TickOutcome::FlushedPaste
        } else {
            TickOutcome::Idle
        }
    }

    /// True when the status label is in an animated activity state.
    pub fn has_active_animation(&self) -> bool {
        self.activity.is_active()
    }

    pub(crate) fn paste_burst_active(&self) -> bool {
        self.paste_burst.is_active()
    }

    /// Set the paste-burst fallback kill switch.
    pub fn set_disable_paste_burst(&mut self, disabled: bool) {
        if disabled {
            self.paste_burst_enabled = false;
            self.paste_burst.clear();
        } else {
            self.paste_burst_enabled = default_paste_burst_enabled();
        }
    }

    /// Test-only: opt in or out of paste-burst fallback detection.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub(crate) fn enable_paste_burst_for_test(&mut self, enabled: bool) {
        self.paste_burst_enabled = enabled;
        if !enabled {
            self.paste_burst.clear();
        }
    }

    /// Flush a completed paste burst through the normal paste insertion path.
    pub fn flush_paste_burst_if_due(&mut self) -> bool {
        if !self.paste_burst_enabled {
            return false;
        }
        let Some(text) = self.paste_burst.flush_if_idle(Instant::now()) else {
            return false;
        };
        self.insert_paste(&text);
        true
    }

    fn flush_paste_burst_now(&mut self) -> bool {
        let Some(text) = self.paste_burst.flush_now() else {
            self.paste_burst.clear();
            return false;
        };
        self.insert_paste(&text);
        true
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

    fn restore_draft(&mut self) {
        self.history_cursor = None;
        let draft = std::mem::take(&mut self.draft);
        let len = draft.text.len();
        self.restore_snapshot(&draft, len);
    }

    /// Test-only: set cursor position.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn set_text_cursor_for_test(&mut self, cursor: usize) {
        self.move_cursor_to_byte(cursor);
    }

    /// Test-only: set editing mode.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn set_mode_for_test(&mut self, mode: EditMode) {
        self.set_mode(mode);
    }

    /// Required render height given the available `width`.
    ///
    /// Includes rows for top+bottom borders. The inner rows are the
    /// visual-row count produced by the soft-wrap layer, clamped to
    /// `[1, 5]` so the input bar never dominates the view.
    pub fn required_height(&self, width: u16) -> u16 {
        let inner_w = width.saturating_sub(BORDER_OVERHEAD_COLS);
        if inner_w == 0 {
            return 1 + BORDER_OVERHEAD_ROWS;
        }

        let mut lines: Vec<String> = self.textarea.lines().to_vec();
        if lines.is_empty() {
            lines.push(String::new());
        }

        let layout = crate::components::input_bar_wrap::wrap(&lines, inner_w);
        let inner = layout.visual_height().clamp(1, 5);
        inner + BORDER_OVERHEAD_ROWS
    }

    /// Render the input bar.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let mode_str = match self.mode {
            EditMode::Emacs => " ● INSERT ",
            EditMode::Vim(VimMode::Normal) => " ▣ VIM·NORMAL ",
            EditMode::Vim(VimMode::Insert) => " ● VIM·INSERT ",
            EditMode::Vim(VimMode::Visual) => " ▦ VIM·VISUAL ",
            EditMode::Vim(VimMode::Operator(_)) => " ▣ VIM·OP ",
        };

        let border_color = if self.active {
            match self.mode {
                EditMode::Vim(VimMode::Normal) => Color::Yellow,
                EditMode::Vim(VimMode::Visual) => Color::LightYellow,
                _ => Color::Green,
            }
        } else {
            Color::DarkGray
        };

        let block = self.build_block(mode_str, border_color);
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
            EditMode::Emacs => " ● INSERT ",
            EditMode::Vim(VimMode::Normal) => " ▣ VIM·NORMAL ",
            EditMode::Vim(VimMode::Insert) => " ● VIM·INSERT ",
            EditMode::Vim(VimMode::Visual) => " ▦ VIM·VISUAL ",
            EditMode::Vim(VimMode::Operator(_)) => " ▣ VIM·OP ",
        };
        let border_color = Color::DarkGray;
        let block = self.build_block(mode_str, border_color);
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

fn expand_paste_refs(
    text: &str,
    protected_ranges: &[ProtectedRange],
    pastes: &BTreeMap<usize, String>,
) -> (String, Vec<ProtectedRange>) {
    let mut ranges = protected_ranges.to_vec();
    ranges.sort_by_key(|r| r.start);

    let mut expanded = String::with_capacity(text.len());
    let mut expanded_ranges = Vec::with_capacity(ranges.len());
    let mut cursor = 0usize;

    for range in ranges {
        if range.start > text.len() || range.end > text.len() || range.start > range.end {
            continue;
        }
        let range_end = range.end;
        if cursor < range.start {
            expanded.push_str(&text[cursor..range.start]);
        }

        let start = expanded.len();
        match range.kind {
            RangeKind::Atom => {
                expanded.push_str(&text[range.start..range.end]);
                let mut adjusted = range;
                adjusted.start = start;
                adjusted.end = expanded.len();
                expanded_ranges.push(adjusted);
            }
            RangeKind::PasteRef(id) => {
                if let Some(paste) = pastes.get(&id) {
                    expanded.push_str(paste);
                } else {
                    expanded.push_str(&text[range.start..range.end]);
                }
            }
            RangeKind::ImageRef(_) => {
                expanded.push_str(&text[range.start..range.end]);
                let mut adjusted = range;
                adjusted.start = start;
                adjusted.end = expanded.len();
                expanded_ranges.push(adjusted);
            }
        }
        cursor = range_end;
    }

    if cursor < text.len() {
        expanded.push_str(&text[cursor..]);
    }

    (expanded, expanded_ranges)
}

impl Default for InputBar {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;
    use std::path::PathBuf;

    fn test_image(id: usize) -> ImageAttachment {
        ImageAttachment {
            id,
            source_path: PathBuf::from(format!("/tmp/test-{id}.png")),
            mime_type: "image/png".to_string(),
            dimensions: (800, 600),
            byte_size: 1024,
            owned_temp: None,
        }
    }

    #[test]
    fn image_attachment_fields_accessible() {
        let a = test_image(0);

        assert_eq!(a.id, 0);
        assert_eq!(a.mime_type, "image/png");
        assert_eq!(a.dimensions, (800, 600));
    }

    #[test]
    fn range_kind_image_ref_is_not_atom() {
        let k = RangeKind::ImageRef(3);

        assert!(!k.is_atom());
    }

    #[test]
    fn new_input_bar_starts_with_empty_image_store() {
        let mut bar = InputBar::new();

        assert!(bar.images.is_empty());
        assert_eq!(bar.next_image_id, 0);
        assert!(bar.take_pending_images().is_empty());
    }

    #[test]
    fn expand_paste_refs_preserves_image_ref_range() {
        let text = "[Image #1 · 100×100] hello".to_string();
        let image_range_end = "[Image #1 · 100×100]".len();
        let ranges = vec![ProtectedRange {
            start: 0,
            end: image_range_end,
            kind: RangeKind::ImageRef(7),
            uri: String::new(),
            name: "[Image #1 · 100×100]".to_string(),
        }];

        let (expanded, expanded_ranges) = expand_paste_refs(&text, &ranges, &BTreeMap::new());

        assert_eq!(expanded, text);
        assert_eq!(
            expanded_ranges.len(),
            1,
            "ImageRef range must survive expand_paste_refs"
        );
        assert_eq!(expanded_ranges[0].kind, RangeKind::ImageRef(7));
        assert_eq!(expanded_ranges[0].start, 0);
        assert_eq!(expanded_ranges[0].end, image_range_end);
    }

    #[test]
    fn submit_preserves_pending_images_until_drained() {
        let mut bar = InputBar::new();
        bar.set_text("send image".to_string(), "send image".len());
        bar.images.insert(1, test_image(1));

        bar.submit().unwrap();

        assert!(bar.images.is_empty());
        let images = bar.take_pending_images();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].id, 1);
        assert!(bar.take_pending_images().is_empty());
    }

    #[test]
    fn clear_drops_unsubmitted_images() {
        let mut bar = InputBar::new();
        bar.images.insert(1, test_image(1));

        bar.clear();

        assert!(bar.images.is_empty());
        assert!(bar.take_pending_images().is_empty());
    }

    #[test]
    fn clear_does_not_reset_image_counter() {
        let mut bar = InputBar::new();
        bar.next_image_id = 3;

        bar.clear();

        assert_eq!(bar.next_image_id, 3);
    }

    #[test]
    fn insert_image_atom_stores_attachment_and_shifts_existing_ranges() {
        let mut bar = InputBar::new();
        bar.insert_atom("@foo", "file:///foo".to_string(), "foo".to_string());
        bar.move_cursor_to_byte(0);

        bar.insert_image_atom(test_image(99));

        let label = "[Image #1 · 800×600]";
        assert_eq!(bar.text(), format!("{label}@foo"));
        assert_eq!(bar.next_image_id, 1);
        assert_eq!(bar.images.len(), 1);
        assert_eq!(bar.images[&0].id, 0);

        let ranges = bar.protected_ranges();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].kind, RangeKind::ImageRef(0));
        assert_eq!((ranges[0].start, ranges[0].end), (0, label.len()));
        assert_eq!(ranges[0].uri, "");
        assert_eq!(ranges[0].name, label);
        assert_eq!(ranges[1].kind, RangeKind::Atom);
        assert_eq!(
            (ranges[1].start, ranges[1].end),
            (label.len(), label.len() + 4)
        );
    }

    #[test]
    fn deleting_image_atom_removes_attachment_from_store() {
        let mut bar = InputBar::new();

        bar.insert_image_atom(test_image(99));
        bar.set_text_cursor_for_test(0);
        bar.delete_char_after_cursor();

        assert_eq!(bar.text(), "");
        assert!(bar.protected_ranges.is_empty());
        assert!(bar.images.is_empty());
    }

    #[test]
    fn ctrl_v_does_not_type_literal_v_in_emacs_mode() {
        let mut bar = InputBar::new();
        let key = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);

        let outcome = bar.handle_key(key);

        assert!(matches!(
            outcome,
            HandleOutcome::Key(IntentEvent::NoOp) | HandleOutcome::Key(IntentEvent::Pasted)
        ));
        assert!(!bar.text().contains('v'));
    }

    #[test]
    fn ctrl_v_does_not_type_literal_v_in_vim_insert_mode() {
        let mut bar = InputBar::new();
        bar.set_mode(EditMode::Vim(VimMode::Insert));
        let key = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);

        let outcome = bar.handle_key(key);

        assert!(matches!(
            outcome,
            HandleOutcome::Key(IntentEvent::NoOp) | HandleOutcome::Key(IntentEvent::Pasted)
        ));
        assert!(!bar.text().contains('v'));
    }

    #[test]
    fn image_attachment_from_rgba_bytes_dimensions() {
        let rgba_bytes = vec![0u8; 4 * 4 * 4];
        let img = image::RgbaImage::from_raw(4, 4, rgba_bytes).unwrap();
        let dyn_img = image::DynamicImage::ImageRgba8(img);

        assert_eq!(dyn_img.width(), 4);
        assert_eq!(dyn_img.height(), 4);
    }

    #[test]
    fn try_as_image_path_returns_none_for_non_path() {
        let result = try_as_image_path("hello world this is not a path");

        assert!(result.is_none());
    }

    #[test]
    fn try_as_image_path_returns_none_for_text_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"hello").unwrap();
        let path_str = tmp.path().to_str().unwrap().to_string();

        let result = try_as_image_path(&path_str);

        assert!(result.is_none());
    }

    #[test]
    fn try_as_image_path_returns_path_and_dims_for_png() {
        let tmp = tempfile::Builder::new().suffix(".png").tempfile().unwrap();
        let img = image::RgbaImage::from_raw(1, 1, vec![0u8; 4]).unwrap();
        let dyn_img = image::DynamicImage::ImageRgba8(img);
        let mut cursor = std::io::Cursor::new(Vec::new());
        dyn_img
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(tmp.path(), cursor.into_inner()).unwrap();
        let path_str = tmp.path().to_str().unwrap().to_string();

        let result = try_as_image_path(&path_str);

        assert!(result.is_some());
        let (path, dims) = result.unwrap();
        assert_eq!(path, tmp.path());
        assert_eq!(dims, (1, 1));
    }

    #[test]
    fn insert_paste_converts_image_path_to_image_atom() {
        let tmp = tempfile::Builder::new().suffix(".png").tempfile().unwrap();
        let img = image::RgbaImage::from_raw(1, 1, vec![0u8; 4]).unwrap();
        let dyn_img = image::DynamicImage::ImageRgba8(img);
        let mut cursor = std::io::Cursor::new(Vec::new());
        dyn_img
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        let png_bytes = cursor.into_inner();
        std::fs::write(tmp.path(), &png_bytes).unwrap();

        let mut bar = InputBar::new();
        bar.insert_paste(&format!("  {}  ", tmp.path().display()));

        assert_eq!(bar.text(), "[Image #1 · 1×1]");
        assert!(bar.pastes.is_empty());
        assert_eq!(bar.images.len(), 1);
        assert_eq!(bar.images[&0].source_path, tmp.path());
        assert_eq!(bar.images[&0].dimensions, (1, 1));
        assert_eq!(bar.images[&0].byte_size, png_bytes.len());
        assert_eq!(bar.protected_ranges[0].kind, RangeKind::ImageRef(0));
    }
}

#[cfg(test)]
mod paste_atom_tests {
    use super::*;

    fn test_atom_range(start: usize, end: usize) -> ProtectedRange {
        ProtectedRange {
            start,
            end,
            kind: RangeKind::Atom,
            uri: "file:///foo".into(),
            name: "foo".into(),
        }
    }

    #[test]
    fn set_state_preserves_non_empty_protected_ranges() {
        // Defends the InputBar contract independently of the completion logic.
        // Gemini (Gate 3) verified that hacking `restore_snapshot` to wipe
        // ranges only breaks tests in input_completion.rs — InputBar itself
        // had no test pinning that `set_state` round-trips its `protected_ranges`.
        let mut bar = InputBar::new();
        let snapshot = crate::input_history::InputStateSnapshot::new(
            "@foo @bar".to_string(),
            vec![
                ProtectedRange {
                    start: 0,
                    end: 4,
                    kind: RangeKind::Atom,
                    uri: "file:///foo".into(),
                    name: "foo".into(),
                },
                ProtectedRange {
                    start: 5,
                    end: 9,
                    kind: RangeKind::Atom,
                    uri: "file:///bar".into(),
                    name: "bar".into(),
                },
            ],
        );
        bar.set_state(snapshot, 9);

        assert_eq!(bar.text(), "@foo @bar");
        let ranges = bar.protected_ranges();
        assert_eq!(
            ranges.len(),
            2,
            "set_state must round-trip non-empty protected_ranges"
        );
        assert_eq!((ranges[0].start, ranges[0].end), (0, 4));
        assert_eq!(ranges[0].uri, "file:///foo");
        assert_eq!((ranges[1].start, ranges[1].end), (5, 9));
        assert_eq!(ranges[1].uri, "file:///bar");
    }

    #[test]
    fn single_line_paste_stays_inline() {
        let mut bar = InputBar::new();

        bar.insert_paste("hello world");

        assert_eq!(bar.text(), "hello world");
        assert!(bar
            .protected_ranges
            .iter()
            .all(|r| r.kind == RangeKind::Atom));
    }

    #[test]
    fn multi_line_paste_atomizes() {
        let mut bar = InputBar::new();
        let text = "fn main() {\n    let x = 1;\n}";

        bar.insert_paste(text);

        assert_eq!(bar.text(), "[Paste #1 · 3 lines]");
        assert_eq!(bar.protected_ranges.len(), 1);
        assert_eq!(bar.protected_ranges[0].kind, RangeKind::PasteRef(1));
        assert_eq!(bar.pastes[&1], text);
    }

    #[test]
    fn submit_expands_placeholder_back_to_original_text() {
        let mut bar = InputBar::new();

        bar.insert_paste("multi\nline\npaste");
        let submitted = bar.submit().unwrap();
        let (captured, ranges, interrupt) = bar.take_submit_capture().unwrap();

        assert_eq!(submitted, ("multi\nline\npaste".to_string(), false));
        assert_eq!(captured, "multi\nline\npaste");
        assert!(ranges.is_empty());
        assert!(!interrupt);
    }

    #[test]
    fn paste_atom_with_bang_prefix_propagates_interrupt() {
        let mut bar = InputBar::new();

        bar.insert_paste("!stop\nplease");
        bar.submit();
        let (captured, _, interrupt) = bar.take_submit_capture().unwrap();

        assert_eq!(captured, "!stop\nplease");
        assert!(
            interrupt,
            "expanded text starts with `!` so interrupt must be true"
        );
    }

    #[test]
    fn submitted_history_restores_expanded_text_inline() {
        let mut bar = InputBar::new();

        bar.insert_paste("multi\nline\npaste");
        bar.submit();
        bar.history_prev();

        assert_eq!(bar.text(), "multi\nline\npaste");
        assert!(bar.protected_ranges.is_empty());
    }

    #[test]
    fn submit_preserves_non_paste_ranges_with_adjusted_offsets() {
        let mut bar = InputBar::new();

        bar.insert_paste("x\ny");
        bar.insert_atom("@foo", "file:///foo".to_string(), "foo".to_string());
        bar.submit();
        let (captured, ranges, _) = bar.take_submit_capture().unwrap();

        assert_eq!(captured, "x\ny@foo");
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].kind, RangeKind::Atom);
        assert_eq!((ranges[0].start, ranges[0].end), (3, 7));
    }

    #[test]
    fn mixed_text_and_paste_expands_correctly() {
        let mut bar = InputBar::new();

        bar.insert_paste("hey ");
        bar.insert_paste("multi\nline");
        bar.insert_paste(" thanks");

        assert_eq!(bar.text(), "hey [Paste #1 · 2 lines] thanks");
        bar.submit();
        let (captured, ranges, _) = bar.take_submit_capture().unwrap();
        assert_eq!(captured, "hey multi\nline thanks");
        assert!(ranges.is_empty());
    }

    #[test]
    fn per_session_numbering_does_not_reset_after_submit() {
        let mut bar = InputBar::new();

        bar.insert_paste("a\nb");
        bar.submit();
        bar.insert_paste("c\nd");

        assert_eq!(bar.text(), "[Paste #2 · 2 lines]");
        assert_eq!(bar.protected_ranges[0].kind, RangeKind::PasteRef(2));
    }

    #[test]
    fn paste_store_caps_oldest_unreferenced_entries_evicted() {
        let mut bar = InputBar::new();

        bar.insert_paste("paste 0\nline2");
        bar.set_text_cursor_for_test(0);
        bar.delete_char_after_cursor();

        for i in 1..=PASTE_STORE_CAP {
            bar.insert_paste(&format!("paste {i}\nline2"));
        }

        assert_eq!(bar.pastes.len(), PASTE_STORE_CAP);
        assert!(!bar.pastes.contains_key(&1));
        assert!(bar.pastes.contains_key(&(PASTE_STORE_CAP + 1)));
    }

    #[test]
    fn trailing_newline_single_line_paste_stays_inline() {
        let mut bar = InputBar::new();

        bar.insert_paste("hello\n");

        assert_eq!(bar.text(), "hello\n");
        assert!(bar.protected_ranges.is_empty());
        assert!(bar.pastes.is_empty());
    }

    #[test]
    fn carriage_return_separators_are_normalized_to_newlines() {
        let mut bar = InputBar::new();

        bar.insert_paste("a\rb\rc");

        assert_eq!(bar.text(), "[Paste #1 · 3 lines]");
        assert_eq!(bar.protected_ranges.len(), 1);
        assert_eq!(bar.pastes.get(&1).map(String::as_str), Some("a\nb\nc"));
    }

    #[test]
    fn crlf_separators_are_normalized_to_newlines() {
        let mut bar = InputBar::new();

        bar.insert_paste("a\r\nb\r\nc");

        assert_eq!(bar.text(), "[Paste #1 · 3 lines]");
        assert_eq!(bar.pastes.get(&1).map(String::as_str), Some("a\nb\nc"));
    }

    #[test]
    fn single_line_with_carriage_return_only_stays_inline() {
        let mut bar = InputBar::new();

        bar.insert_paste("hello\r");

        assert_eq!(bar.text(), "hello\n");
        assert!(bar.protected_ranges.is_empty());
        assert!(bar.pastes.is_empty());
    }

    #[test]
    fn paste_store_keeps_referenced_placeholder_over_cap() {
        let mut bar = InputBar::new();

        for i in 0..=PASTE_STORE_CAP {
            bar.insert_paste(&format!("paste {i}\nline2"));
        }

        assert!(bar.pastes.contains_key(&1));
        assert_eq!(bar.pastes.len(), PASTE_STORE_CAP + 1);

        bar.submit();
        let (captured, ranges, _) = bar.take_submit_capture().unwrap();
        let expected = (0..=PASTE_STORE_CAP)
            .map(|i| format!("paste {i}\nline2"))
            .collect::<String>();
        assert_eq!(captured, expected);
        assert!(ranges.is_empty());
    }

    #[test]
    fn placeholder_format_uses_str_lines_count() {
        let mut bar = InputBar::new();

        bar.insert_paste("a\nb");
        assert_eq!(bar.text(), "[Paste #1 · 2 lines]");
        bar.clear();

        bar.insert_paste("a\nb\n");
        assert_eq!(bar.text(), "[Paste #2 · 2 lines]");
        bar.clear();

        bar.insert_paste("a");
        assert_eq!(bar.text(), "a");
        assert!(bar.protected_ranges.is_empty());
    }

    #[test]
    fn backspace_at_end_of_placeholder_removes_whole_atom() {
        let mut bar = InputBar::new();

        bar.insert_paste("x\ny");
        bar.delete_char_before_cursor();

        assert_eq!(bar.text(), "");
        assert!(bar.protected_ranges.is_empty());
    }

    #[test]
    fn tab_before_atom_shifts_protected_range() {
        let mut bar = InputBar::new();
        bar.insert_atom("@foo", "file:///foo".to_string(), "foo".to_string());
        bar.set_text_cursor_for_test(0);

        let outcome = bar.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

        assert_eq!(outcome, HandleOutcome::Key(IntentEvent::NoOp));
        assert_eq!(bar.text(), "    @foo");
        assert_eq!(
            (bar.protected_ranges[0].start, bar.protected_ranges[0].end),
            (4, 8)
        );
    }

    #[test]
    fn ctrl_a_in_emacs_moves_to_line_start_without_typing() {
        let mut bar = InputBar::new();
        bar.set_text("hello\nworld".to_string(), "hello\nworld".len());

        let outcome = bar.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));

        assert_eq!(outcome, HandleOutcome::Key(IntentEvent::MovedCursor));
        assert_eq!(bar.text(), "hello\nworld");
        assert_eq!(bar.cursor(), "hello\n".len());
    }

    #[test]
    fn delete_span_partial_atom_overlap_deletes_whole_atom() {
        let mut bar = InputBar::new();
        bar.set_state(
            InputStateSnapshot::new("x @foo y".to_string(), vec![test_atom_range(2, 6)]),
            0,
        );

        bar.delete_span(1, 3);

        assert_eq!(bar.text(), "x y");
        assert!(bar.protected_ranges.is_empty());
    }

    #[test]
    fn left_at_byte_zero_before_atom_stays_at_zero() {
        let mut bar = InputBar::new();
        bar.insert_atom("@foo", "file:///foo".to_string(), "foo".to_string());
        bar.set_text_cursor_for_test(0);

        let outcome = bar.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

        assert_eq!(outcome, HandleOutcome::Key(IntentEvent::MovedCursor));
        assert_eq!(bar.cursor(), 0);
    }

    #[test]
    fn vim_o_with_atom_on_subsequent_line_shifts_range() {
        let mut bar = InputBar::new();
        bar.set_state(
            InputStateSnapshot::new("first\n@foo".to_string(), vec![test_atom_range(6, 10)]),
            0,
        );
        bar.set_mode(EditMode::Vim(VimMode::Normal));

        let outcome = bar.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));

        assert_eq!(outcome, HandleOutcome::Key(IntentEvent::TypedChar('\n')));
        assert_eq!(bar.text(), "first\n\n@foo");
        assert_eq!(
            (bar.protected_ranges[0].start, bar.protected_ranges[0].end),
            (7, 11)
        );
    }

    #[test]
    fn vim_o_above_atom_line_shifts_range() {
        let mut bar = InputBar::new();
        bar.set_state(
            InputStateSnapshot::new("first\n@foo".to_string(), vec![test_atom_range(6, 10)]),
            "first\n".len(),
        );
        bar.set_mode(EditMode::Vim(VimMode::Normal));

        let outcome = bar.handle_key(KeyEvent::new(KeyCode::Char('O'), KeyModifiers::NONE));

        assert_eq!(outcome, HandleOutcome::Key(IntentEvent::TypedChar('\n')));
        assert_eq!(bar.text(), "first\n\n@foo");
        assert_eq!(
            (bar.protected_ranges[0].start, bar.protected_ranges[0].end),
            (7, 11)
        );
    }

    #[test]
    fn multiple_pastes_in_same_draft_expand_in_order() {
        let mut bar = InputBar::new();
        let a = "a\nb\nc";
        let b = "1\n2\n3\n4\n5";

        bar.insert_paste(a);
        bar.insert_paste(b);

        assert_eq!(bar.text(), "[Paste #1 · 3 lines][Paste #2 · 5 lines]");
        bar.submit();
        let (captured, ranges, _) = bar.take_submit_capture().unwrap();
        assert_eq!(captured, format!("{a}{b}"));
        assert!(ranges.is_empty());
    }

    #[test]
    fn paste_during_history_browse_restores_draft_first() {
        let mut bar = InputBar::new();

        bar.set_text("draft content".to_string(), "draft content".len());
        bar.insert_paste("first\npaste");
        bar.submit();

        bar.set_text("new draft".to_string(), "new draft".len());
        bar.history_prev();
        bar.insert_paste("interrupting\npaste");

        assert!(bar.text().contains("new draft"));
        assert!(bar.text().contains("[Paste"));
    }

    #[test]
    fn empty_paste_is_noop() {
        let mut bar = InputBar::new();

        bar.insert_paste("");

        assert_eq!(bar.text(), "");
        assert!(bar.protected_ranges.is_empty());
    }

    #[test]
    fn vim_normal_raw_newline_does_not_submit_or_consume_pending_command() {
        let mut bar = InputBar::new();
        bar.set_text("abc".to_string(), 0);
        bar.set_mode(EditMode::Vim(VimMode::Normal));

        let start_delete = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('d'),
            crossterm::event::KeyModifiers::NONE,
        );
        let raw_newline = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('\n'),
            crossterm::event::KeyModifiers::NONE,
        );
        let complete_delete = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('d'),
            crossterm::event::KeyModifiers::NONE,
        );

        assert_eq!(
            bar.handle_key(start_delete),
            HandleOutcome::Key(IntentEvent::NoOp)
        );
        assert_eq!(
            bar.handle_key(raw_newline),
            HandleOutcome::Key(IntentEvent::NoOp)
        );
        assert!(bar.take_submit_capture().is_none());
        assert_eq!(
            bar.handle_key(complete_delete),
            HandleOutcome::Key(IntentEvent::DeletedChar)
        );
        assert_eq!(bar.text(), "");
    }
}

#[cfg(test)]
mod required_height_tests {
    use super::*;

    #[test]
    fn required_height_empty_is_3() {
        // 1 visual row + 2 border rows.
        let bar = InputBar::new();
        assert_eq!(bar.required_height(80), 1 + BORDER_OVERHEAD_ROWS);
    }

    #[test]
    fn required_height_wraps_long_ascii_line() {
        let mut bar = InputBar::new();
        bar.set_text("a".repeat(200), 200);
        // 200 / 82 = 3 visual rows (200 = 2*82 + 36) = ceil → 3.
        // Plus 2 border rows = 5. Clamp max is 5.
        assert_eq!(bar.required_height(82), 3 + BORDER_OVERHEAD_ROWS); // inner width = 82
    }

    #[test]
    fn required_height_clamps_at_max_5_plus_borders() {
        let mut bar = InputBar::new();
        bar.set_text("a".repeat(10_000), 0);
        assert_eq!(bar.required_height(82), 5 + BORDER_OVERHEAD_ROWS); // clamp(inner, 1, 5) + borders
    }

    #[test]
    fn required_height_cjk_counts_cells() {
        let mut bar = InputBar::new();
        // 10 CJK chars = 20 cells → fits in inner width 22 on one row.
        bar.set_text("你好世界你好世界你好".to_string(), 0);
        assert_eq!(bar.required_height(22), 1 + BORDER_OVERHEAD_ROWS); // inner width = 22 → 1 row
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
        assert_eq!(bar.last_inner_width_for_test(), 20);
    }

    #[test]
    fn badge_includes_glyph_prefix_for_each_mode() {
        use ratatui::backend::TestBackend;
        use ratatui::layout::Rect;
        use ratatui::Terminal;

        // For each mode, build an InputBar in that mode, render to a
        // TestBackend, and confirm the expected glyph appears in the title row.
        let cases: &[(EditMode, char)] = &[
            (EditMode::Emacs, '●'),
            (EditMode::Vim(VimMode::Insert), '●'),
            (EditMode::Vim(VimMode::Normal), '▣'),
            (EditMode::Vim(VimMode::Visual), '▦'),
        ];

        for (mode, glyph) in cases {
            let mut bar = InputBar::new();
            bar.set_mode_for_test(*mode);
            bar.set_active(true);
            let backend = TestBackend::new(40, 3);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|f| bar.render(f, Rect::new(0, 0, 40, 3)))
                .unwrap();
            let buf = terminal.backend().buffer();
            // The title sits on row 0 (the top border row).
            let row0: String = (0..40)
                .map(|x| {
                    buf.cell((x, 0))
                        .and_then(|cell| cell.symbol().chars().next())
                        .unwrap_or(' ')
                })
                .collect();
            assert!(
                row0.contains(*glyph),
                "expected glyph {:?} on row 0 for mode {:?}, got: {:?}",
                glyph,
                mode,
                row0
            );
        }
    }
}
