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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum TriggerState {
    #[default]
    Idle,
    Composing {
        kind: TriggerKind,
        prefix_start: usize,
    },
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
            let still_valid =
                prefix_start < text.len() && text[prefix_start..].starts_with(expected);
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
            (TriggerState::Composing { .. }, _) => self.advance_composing(event, text, cursor),
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

#[cfg(test)]
mod detector_tests {
    use super::*;
    use crate::components::input_bar::ProtectedRange;

    fn d() -> TriggerDetector {
        TriggerDetector::new()
    }

    // ── Task 3: Idle transitions (11 tests) ──────────────────────────

    #[test]
    fn idle_typed_at_at_offset_zero_opens_mention() {
        let mut det = d();
        let t = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
        match t {
            TriggerTransition::Open { trigger } => {
                assert_eq!(trigger.kind, TriggerKind::Mention);
                assert_eq!(trigger.prefix_start, 0);
                assert_eq!(trigger.query, "");
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn idle_typed_slash_at_offset_zero_opens_slash() {
        let mut det = d();
        let t = det.step(IntentEvent::TypedChar('/'), "/", 1, &[]);
        match t {
            TriggerTransition::Open { trigger } => {
                assert_eq!(trigger.kind, TriggerKind::Slash);
                assert_eq!(trigger.prefix_start, 0);
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn idle_typed_slash_at_nonzero_offset_stays_idle() {
        let mut det = d();
        let t = det.step(IntentEvent::TypedChar('/'), "a/", 2, &[]);
        assert!(matches!(t, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn idle_typed_at_after_non_whitespace_stays_idle() {
        let mut det = d();
        let t = det.step(IntentEvent::TypedChar('@'), "foo@", 4, &[]);
        assert!(matches!(t, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn idle_typed_at_after_whitespace_opens() {
        let mut det = d();
        let t = det.step(IntentEvent::TypedChar('@'), "foo @", 5, &[]);
        match t {
            TriggerTransition::Open { trigger } => {
                assert_eq!(trigger.prefix_start, 4);
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn idle_typed_at_where_byte_is_atom_start_stays_idle() {
        let mut det = d();
        let ranges = [ProtectedRange {
            start: 0,
            end: 4,
            uri: "u".into(),
            name: "n".into(),
        }];
        let t = det.step(IntentEvent::TypedChar('@'), "@foo", 1, &ranges);
        assert!(matches!(t, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn idle_moved_cursor_stays_idle_emits_none() {
        let mut det = d();
        let t = det.step(IntentEvent::MovedCursor, "hello @world", 12, &[]);
        assert!(matches!(t, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn idle_deleted_char_stays_idle() {
        let mut det = d();
        let t = det.step(IntentEvent::DeletedChar, "hello", 5, &[]);
        assert!(matches!(t, TriggerTransition::None));
    }

    #[test]
    fn idle_pasted_stays_idle() {
        let mut det = d();
        let t = det.step(IntentEvent::Pasted, "pasted @alice text", 18, &[]);
        assert!(matches!(t, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn idle_set_text_stays_idle() {
        let mut det = d();
        let t = det.step(IntentEvent::SetText, "recalled @foo", 13, &[]);
        assert!(matches!(t, TriggerTransition::None));
    }

    #[test]
    fn idle_noop_emits_none() {
        let mut det = d();
        let t = det.step(IntentEvent::NoOp, "", 0, &[]);
        assert!(matches!(t, TriggerTransition::None));
    }

    // ── Task 4: Composing transitions (12 tests) ─────────────────────

    #[test]
    fn composing_typed_char_emits_update_with_growing_query() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
        let t = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[]);
        match t {
            TriggerTransition::Update { query } => assert_eq!(query, "f"),
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn composing_deleted_char_emits_update_with_shrunken_query() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
        let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[]);
        let _ = det.step(IntentEvent::TypedChar('o'), "@fo", 3, &[]);
        let t = det.step(IntentEvent::DeletedChar, "@f", 2, &[]);
        match t {
            TriggerTransition::Update { query } => assert_eq!(query, "f"),
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn composing_moved_cursor_inside_window_emits_update() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
        let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[]);
        let _ = det.step(IntentEvent::TypedChar('o'), "@fo", 3, &[]);
        let _ = det.step(IntentEvent::TypedChar('o'), "@foo", 4, &[]);
        let t = det.step(IntentEvent::MovedCursor, "@foo", 3, &[]);
        match t {
            TriggerTransition::Update { query } => assert_eq!(query, "fo"),
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn composing_typed_whitespace_emits_close() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
        let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[]);
        let t = det.step(IntentEvent::TypedChar(' '), "@f ", 3, &[]);
        assert!(matches!(t, TriggerTransition::Close));
        assert!(det.is_idle());
    }

    #[test]
    fn composing_moved_cursor_out_of_window_emits_close() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
        let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[]);
        let t = det.step(IntentEvent::MovedCursor, "@f", 0, &[]);
        assert!(matches!(t, TriggerTransition::Close));
        assert!(det.is_idle());
    }

    #[test]
    fn composing_deleted_trigger_char_emits_close_via_defensive_check() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
        let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[]);
        let _ = det.step(IntentEvent::DeletedChar, "@", 1, &[]);
        let t = det.step(IntentEvent::DeletedChar, "", 0, &[]);
        assert!(matches!(t, TriggerTransition::Close));
        assert!(det.is_idle());
    }

    #[test]
    fn composing_pasted_emits_close() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
        let t = det.step(IntentEvent::Pasted, "@ hello world", 13, &[]);
        assert!(matches!(t, TriggerTransition::Close));
        assert!(det.is_idle());
    }

    #[test]
    fn composing_set_text_emits_close() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
        let t = det.step(IntentEvent::SetText, "recalled text", 13, &[]);
        assert!(matches!(t, TriggerTransition::Close));
    }

    #[test]
    fn composing_accepted_emits_close() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
        let t = det.step(IntentEvent::Accepted, "@atom", 5, &[]);
        assert!(matches!(t, TriggerTransition::Close));
    }

    #[test]
    fn composing_dismissed_emits_close() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
        let t = det.step(IntentEvent::Dismissed, "@", 1, &[]);
        assert!(matches!(t, TriggerTransition::Close));
    }

    #[test]
    fn composing_submitted_emits_close() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
        let t = det.step(IntentEvent::Submitted, "", 0, &[]);
        assert!(matches!(t, TriggerTransition::Close));
    }

    #[test]
    fn composing_noop_emits_none_and_stays_composing() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
        let t = det.step(IntentEvent::NoOp, "@", 1, &[]);
        assert!(matches!(t, TriggerTransition::None));
        assert!(!det.is_idle());
    }

    // ── Task 5: journeys + defensive re-check (6 tests) ──────────────

    #[test]
    fn j1_power_user_walks_cursor_across_atoms_zero_opens() {
        let mut det = d();
        let text = "@src/foo.rs and @docs/bar.md";
        let ranges = [
            ProtectedRange {
                start: 0,
                end: 11,
                uri: "a".into(),
                name: "a".into(),
            },
            ProtectedRange {
                start: 16,
                end: 28,
                uri: "b".into(),
                name: "b".into(),
            },
        ];
        let mut opens = 0;
        for cursor in 0..=text.len() {
            let t = det.step(IntentEvent::MovedCursor, text, cursor, &ranges);
            if matches!(t, TriggerTransition::Open { .. }) {
                opens += 1;
            }
        }
        assert_eq!(opens, 0, "cursor motion must never open picker");
    }

    #[test]
    fn j6_auto_repeat_left_arrow_across_stray_at_zero_opens() {
        let mut det = d();
        let text = "please see @foo bar";
        let mut opens = 0;
        for _ in 0..50 {
            let t = det.step(IntentEvent::MovedCursor, text, 15, &[]);
            if matches!(t, TriggerTransition::Open { .. }) {
                opens += 1;
            }
        }
        assert_eq!(opens, 0);
    }

    #[test]
    fn j7_selection_drag_across_stray_at_zero_opens() {
        let mut det = d();
        let text = "text @alice more";
        let mut opens = 0;
        for cursor in 0..=text.len() {
            let t = det.step(IntentEvent::MovedCursor, text, cursor, &[]);
            if matches!(t, TriggerTransition::Open { .. }) {
                opens += 1;
            }
        }
        assert_eq!(opens, 0);
    }

    #[test]
    fn j2b_typo_fix_after_esc_stays_closed_on_motion() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
        let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[]);
        let _ = det.step(IntentEvent::TypedChar('o'), "@fo", 3, &[]);
        let close = det.step(IntentEvent::Dismissed, "@fo", 3, &[]);
        assert!(matches!(close, TriggerTransition::Close));
        let mot = det.step(IntentEvent::MovedCursor, "@fo", 2, &[]);
        assert!(matches!(mot, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn reset_puts_detector_in_idle() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
        assert!(!det.is_idle());
        det.reset();
        assert!(det.is_idle());
    }

    #[test]
    fn defensive_reset_when_prefix_start_past_text_len() {
        // White-box: construct a Composing state with prefix_start past text.
        let mut det = TriggerDetector {
            state: TriggerState::Composing {
                kind: TriggerKind::Mention,
                prefix_start: 100,
            },
        };
        let t = det.step(IntentEvent::MovedCursor, "abc", 3, &[]);
        assert!(matches!(t, TriggerTransition::Close));
        assert!(det.is_idle());
    }
}
