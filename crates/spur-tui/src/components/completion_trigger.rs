/// The kind of prefix that opened the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    /// Slash-command: `/…`. v1 only fires at byte offset 0.
    Slash,
    /// Resource mention: `@…`. Fires anywhere after whitespace or at offset 0.
    Mention,
}

/// An active popup trigger detected in the InputBar text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub kind: TriggerKind,
    /// Byte offset of the trigger char (`/` or `@`) in `text`.
    pub prefix_start: usize,
    /// The query between the trigger char and the cursor (no leading char).
    pub query: String,
}

/// User-intent event fed to the trigger state machine. Classified at the
/// dispatch site (InputBar::handle_key for key events; session_detail
/// emits the non-key variants at their call sites). See the design spec
/// for the full transition table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentEvent {
    /// User pressed a printable character key. Carries the character.
    TypedChar(char),
    /// User deleted characters (Backspace, Delete, Ctrl+K, Ctrl+U, Ctrl+W,
    /// or atomic range deletion). May remove multiple bytes.
    DeletedChar,
    /// Pure cursor motion — arrows, Home, End, Ctrl-A/E, word motion,
    /// vim h/j/k/l/w/b/etc., visual-line up/down, `g`/`G`, mouse click.
    MovedCursor,
    /// `input_bar.insert_paste(...)` ran.
    Pasted,
    /// `input_bar.set_text(...)` / `set_state(...)` / history-recall swap ran.
    SetText,
    /// Picker accepted a selection; the view is about to `insert_atom` or
    /// `set_state`. Emitted at the accept call site.
    Accepted,
    /// Picker cancelled (Esc / Ctrl+C). Emitted at the cancel call site.
    Dismissed,
    /// Buffer submitted (Enter). Emitted alongside `HandleOutcome::Submit`.
    Submitted,
    /// The key event was not handled (e.g., vim intermediate pending).
    NoOp,
}

/// Internal state of the trigger detector.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TriggerState {
    Idle,
    Composing {
        kind: TriggerKind,
        prefix_start: usize,
    },
}

impl Default for TriggerState {
    fn default() -> Self {
        TriggerState::Idle
    }
}

/// Transition emitted by `TriggerDetector::step` describing what the view
/// should do with its active `PickerShell`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerTransition {
    /// No change — neither the previous nor current input state had a
    /// trigger. The view should do nothing to its picker_shell (it may
    /// be holding a history shell, which this detector does not manage).
    None,
    /// A new trigger appeared (either first-ever or a change in kind /
    /// prefix_start from the last trigger). The view should open a fresh
    /// PickerShell with a source matching `trigger.kind`.
    Open { trigger: Trigger },
    /// The trigger's kind and prefix_start match the last step's trigger;
    /// only the query text changed. The view should forward `query` to the
    /// existing shell via `set_query_from_input_bar`.
    Update { query: String },
    /// The last step had a trigger; the current step does not. The view
    /// should close its trigger-driven PickerShell.
    Close,
}

/// Stateful trigger recognizer. Trigger liveness is a function of user
/// intent events — not text content. See the design spec.
///
/// History-mode shells (`QueryMode::OwnedByShell`) are NOT managed by this
/// detector; the view must skip calling `step` while such a shell is open.
#[derive(Debug, Default)]
pub struct TriggerDetector {
    state: TriggerState,
}

impl TriggerDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when the detector is in the Idle state. The view uses this
    /// to skip the detector entirely on non-opening events (fast path).
    pub fn is_idle(&self) -> bool {
        matches!(self.state, TriggerState::Idle)
    }

    /// Reset the detector to Idle. Call after the view accepts or cancels
    /// a trigger-driven shell.
    pub fn reset(&mut self) {
        self.state = TriggerState::Idle;
    }

    /// Feed an intent event plus the current text/cursor context. Returns
    /// a transition describing what should happen to the picker shell.
    pub fn step(
        &mut self,
        event: IntentEvent,
        text: &str,
        cursor: usize,
        protected_ranges: &[crate::components::input_bar::ProtectedRange],
    ) -> TriggerTransition {
        // Defensive re-check: if Composing state references a prefix_start
        // that no longer holds the trigger char (upstream path forgot to
        // send Pasted/SetText), force Idle + Close.
        if let TriggerState::Composing { kind, prefix_start } = self.state {
            let expected = match kind {
                TriggerKind::Mention => '@',
                TriggerKind::Slash => '/',
            };
            let still_valid = prefix_start < text.len()
                && text[prefix_start..].chars().next() == Some(expected);
            if !still_valid {
                self.state = TriggerState::Idle;
                return TriggerTransition::Close;
            }
        }

        match (&self.state, &event) {
            // Fast Idle cases — just stay Idle.
            (TriggerState::Idle, IntentEvent::NoOp)
            | (TriggerState::Idle, IntentEvent::MovedCursor)
            | (TriggerState::Idle, IntentEvent::DeletedChar)
            | (TriggerState::Idle, IntentEvent::Pasted)
            | (TriggerState::Idle, IntentEvent::SetText)
            | (TriggerState::Idle, IntentEvent::Accepted)
            | (TriggerState::Idle, IntentEvent::Dismissed)
            | (TriggerState::Idle, IntentEvent::Submitted) => TriggerTransition::None,

            // Idle + TypedChar: maybe open.
            (TriggerState::Idle, IntentEvent::TypedChar(c)) => {
                self.maybe_open(*c, text, cursor, protected_ranges)
            }

            // Composing + anything: delegated.
            (TriggerState::Composing { .. }, _) => {
                self.advance_composing(event, text, cursor)
            }
        }
    }

    /// Idle → Composing transition logic for TypedChar events.
    fn maybe_open(
        &mut self,
        c: char,
        text: &str,
        cursor: usize,
        protected_ranges: &[crate::components::input_bar::ProtectedRange],
    ) -> TriggerTransition {
        // cursor is post-type; the typed char lives at cursor-1 byte-wise.
        let typed_byte = cursor.saturating_sub(c.len_utf8());
        if typed_byte >= text.len() {
            return TriggerTransition::None;
        }

        // Guard: the typed char's byte position must not be the start of a
        // protected range (I4 — committed atoms are opaque).
        if protected_ranges.iter().any(|r| r.start == typed_byte) {
            return TriggerTransition::None;
        }

        match c {
            '/' => {
                if typed_byte != 0 {
                    return TriggerTransition::None;
                }
                self.state = TriggerState::Composing {
                    kind: TriggerKind::Slash,
                    prefix_start: 0,
                };
                TriggerTransition::Open {
                    trigger: Trigger {
                        kind: TriggerKind::Slash,
                        prefix_start: 0,
                        query: String::new(),
                    },
                }
            }
            '@' => {
                // Boundary: offset 0 OR prev char is whitespace.
                let prev_is_boundary = typed_byte == 0
                    || text[..typed_byte]
                        .chars()
                        .last()
                        .is_none_or(|ch| ch.is_whitespace());
                if !prev_is_boundary {
                    return TriggerTransition::None;
                }
                self.state = TriggerState::Composing {
                    kind: TriggerKind::Mention,
                    prefix_start: typed_byte,
                };
                TriggerTransition::Open {
                    trigger: Trigger {
                        kind: TriggerKind::Mention,
                        prefix_start: typed_byte,
                        query: String::new(),
                    },
                }
            }
            _ => TriggerTransition::None,
        }
    }

    /// Composing → Composing|Idle transition logic.
    fn advance_composing(
        &mut self,
        event: IntentEvent,
        text: &str,
        cursor: usize,
    ) -> TriggerTransition {
        let (_kind, prefix_start) = match self.state {
            TriggerState::Composing { kind, prefix_start } => (kind, prefix_start),
            TriggerState::Idle => unreachable!("called with Idle state"),
        };

        // Terminal events — always Close.
        match event {
            IntentEvent::Pasted
            | IntentEvent::SetText
            | IntentEvent::Accepted
            | IntentEvent::Dismissed
            | IntentEvent::Submitted => {
                self.state = TriggerState::Idle;
                return TriggerTransition::Close;
            }
            IntentEvent::NoOp => {
                return TriggerTransition::None;
            }
            _ => {}
        }

        // Whitespace TypedChar closes.
        if let IntentEvent::TypedChar(c) = event {
            if c.is_whitespace() {
                self.state = TriggerState::Idle;
                return TriggerTransition::Close;
            }
        }

        // Determine window_end: first whitespace at or after prefix_start+1,
        // else text.len().
        let query_region_start = prefix_start + 1;
        let window_end = text[query_region_start.min(text.len())..]
            .char_indices()
            .find(|(_, ch)| ch.is_whitespace())
            .map(|(i, _)| query_region_start + i)
            .unwrap_or(text.len());

        // MovedCursor: close if cursor outside window.
        if matches!(event, IntentEvent::MovedCursor) {
            let in_window = cursor > prefix_start && cursor <= window_end;
            if !in_window {
                self.state = TriggerState::Idle;
                return TriggerTransition::Close;
            }
        }

        // DeletedChar: the defensive re-check at the top of step() handles
        // "trigger char gone → Close". Here we just fall through to Update.

        // Compute query slice as text[prefix_start+1 .. cursor], clamped.
        let clamped_end = cursor.min(window_end).min(text.len());
        let query_start = query_region_start.min(clamped_end);
        let query = text[query_start..clamped_end].to_string();

        TriggerTransition::Update { query }
    }
}
