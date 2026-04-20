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

/// Decide whether a popup should be open given `(text, cursor)`.
///
/// Rules (v1):
///   * `/` fires only at byte offset 0.
///   * `@` fires at byte offset 0 OR immediately after ASCII whitespace.
///   * Any whitespace character between the trigger char and the cursor
///     closes the popup.
pub fn detect(text: &str, cursor: usize) -> Option<Trigger> {
    if cursor == 0 || cursor > text.len() {
        return None;
    }
    let before = &text[..cursor];

    // Slash: at offset 0 only.
    if let Some(query) = before.strip_prefix('/') {
        if !query.contains(char::is_whitespace) {
            return Some(Trigger {
                kind: TriggerKind::Slash,
                prefix_start: 0,
                query: query.to_string(),
            });
        }
    }

    // Mention: find the last '@' preceded by start-of-string or whitespace,
    // then verify no whitespace intervenes between '@' and cursor.
    if let Some(at_pos) = before.rfind('@') {
        let prev_is_boundary = at_pos == 0
            || before[..at_pos]
                .chars()
                .last()
                .is_none_or(|c| c.is_whitespace());
        if prev_is_boundary {
            let query = &before[at_pos + 1..];
            if !query.contains(char::is_whitespace) {
                return Some(Trigger {
                    kind: TriggerKind::Mention,
                    prefix_start: at_pos,
                    query: query.to_string(),
                });
            }
        }
    }

    None
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

/// Stateful wrapper over `detect()`. Remembers the last-emitted trigger so
/// consecutive `step` calls can classify transitions. History-mode shells
/// (`Ctrl+R`, `QueryMode::OwnedByShell`) are NOT managed by this detector —
/// the view checks shell mode before calling `step`, and the detector only
/// produces transitions for trigger-driven (`QueryMode::ReadFromInputBar`)
/// popups.
#[derive(Debug, Default)]
pub struct TriggerDetector {
    last: Option<Trigger>,
}

impl TriggerDetector {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Feed current (text, cursor). Returns a transition describing what
    /// should happen to the trigger-driven shell. Callers invoke this after
    /// every InputBar text change.
    pub fn step(&mut self, text: &str, cursor: usize) -> TriggerTransition {
        let new = detect(text, cursor);
        let transition = match (&self.last, &new) {
            (None, None) => TriggerTransition::None,
            (Some(old), Some(new_t))
                if old.kind == new_t.kind && old.prefix_start == new_t.prefix_start =>
            {
                TriggerTransition::Update {
                    query: new_t.query.clone(),
                }
            }
            (_, Some(new_t)) => TriggerTransition::Open {
                trigger: new_t.clone(),
            },
            (Some(_), None) => TriggerTransition::Close,
        };
        self.last = new;
        transition
    }

    /// Reset the detector's memory of the last trigger. Call after the view
    /// accepts or cancels a trigger-driven shell, so the next `step` treats
    /// a re-appearing trigger as a fresh Open rather than a spurious Update.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod detector_tests {
    use super::*;

    #[test]
    fn detector_starts_with_no_trigger() {
        let mut d = TriggerDetector::new();
        let t = d.step("", 0);
        assert!(matches!(t, TriggerTransition::None));
    }

    #[test]
    fn detector_reports_open_on_first_trigger_appearance() {
        let mut d = TriggerDetector::new();
        let t = d.step("@", 1);
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
    fn detector_reports_update_when_query_changes_same_trigger() {
        let mut d = TriggerDetector::new();
        let _ = d.step("@", 1);
        let t = d.step("@f", 2);
        match t {
            TriggerTransition::Update { query } => assert_eq!(query, "f"),
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn detector_reports_close_when_trigger_goes_away() {
        let mut d = TriggerDetector::new();
        let _ = d.step("@foo", 4);
        let t = d.step("@foo ", 5); // whitespace terminates mention
        assert!(matches!(t, TriggerTransition::Close));
    }

    #[test]
    fn detector_reports_open_on_kind_change_even_if_position_matches() {
        // '/help' at offset 0, then user deletes and types '@': position
        // happens to still be 0 but kind flipped from Slash to Mention —
        // this MUST be an Open (fresh shell), not an Update.
        let mut d = TriggerDetector::new();
        let _ = d.step("/", 1);
        let t = d.step("@", 1);
        match t {
            TriggerTransition::Open { trigger } => {
                assert_eq!(trigger.kind, TriggerKind::Mention);
            }
            other => panic!("expected Open on kind change, got {other:?}"),
        }
    }

    #[test]
    fn detector_reports_open_on_prefix_start_change() {
        // Mention at offset 0, then a leading space + new mention at offset 1.
        let mut d = TriggerDetector::new();
        let _ = d.step("@foo", 4);
        // After moving to a different mention trigger (e.g. " @bar"):
        let t = d.step(" @bar", 5);
        match t {
            TriggerTransition::Open { trigger } => {
                assert_eq!(trigger.prefix_start, 1);
            }
            other => panic!("expected Open on prefix_start change, got {other:?}"),
        }
    }

    #[test]
    fn detector_reports_none_when_neither_last_nor_current_has_trigger() {
        let mut d = TriggerDetector::new();
        let _ = d.step("hello", 5);
        let t = d.step("hello world", 11);
        assert!(matches!(t, TriggerTransition::None));
    }

    #[test]
    fn detector_reset_clears_last_trigger_so_next_step_reports_open() {
        let mut d = TriggerDetector::new();
        let _ = d.step("@foo", 4);
        d.reset();
        // Without reset this would be Update; after reset the detector
        // treats @foo as a fresh appearance.
        let t = d.step("@foo", 4);
        assert!(matches!(t, TriggerTransition::Open { .. }));
    }
}
