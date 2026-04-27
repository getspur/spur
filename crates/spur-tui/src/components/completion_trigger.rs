/// The kind of prefix that opened the popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerKind {
    /// Slash-command: `/…`. v1 only fires at byte offset 0.
    Slash,
    /// Resource mention: `@…`. Fires anywhere after whitespace or at offset 0.
    Mention,
    /// Cursor in the arg region of a slash command whose registry
    /// `arg_picker_spec(command_name)` returned Some. The picker kind
    /// (typed vs free-text) is resolved by InputCompletionPort, not here.
    SlashArg { command_name: String },
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

/// Internal kind discriminator for the Composing state. Mirrors `TriggerKind`
/// but is the type stored in `TriggerState` so the field types in the public
/// API and the internal state can evolve independently.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TriggerKindInternal {
    Mention,
    Slash,
    SlashArg { command_name: String },
}

/// Internal state of the trigger detector.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum TriggerState {
    #[default]
    Idle,
    Composing {
        kind: TriggerKindInternal,
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
    ///
    /// `registry_arg_picker(name)` reports whether the command `name` has a
    /// registered arg picker; the closure isolates the detector from any
    /// CommandRegistry import. SlashArg is only opened when this returns
    /// `true` for the parsed command.
    pub fn step<R>(
        &mut self,
        event: IntentEvent,
        text: &str,
        cursor: usize,
        protected_ranges: &[crate::components::input_bar::ProtectedRange],
        registry_arg_picker: R,
    ) -> TriggerTransition
    where
        R: Fn(&str) -> bool,
    {
        // Defensive re-check: if Composing state references a prefix_start
        // that no longer holds the trigger char (upstream path forgot to
        // send Pasted/SetText), force Idle + Close.
        if let TriggerState::Composing { kind, prefix_start } = &self.state {
            let still_valid = match kind {
                TriggerKindInternal::Mention => {
                    *prefix_start < text.len() && text[*prefix_start..].starts_with('@')
                }
                TriggerKindInternal::Slash => {
                    *prefix_start < text.len() && text[*prefix_start..].starts_with('/')
                }
                TriggerKindInternal::SlashArg { command_name } => {
                    matches!(parse_slash_arg_prefix(text, cursor),
                        Some((cmd, ps)) if cmd == command_name && ps == *prefix_start)
                }
            };
            if !still_valid {
                self.state = TriggerState::Idle;
                return TriggerTransition::Close;
            }
        }

        let dispatched = match (&self.state, &event) {
            // Fast Idle cases — stay Idle for now; SlashArg detection runs
            // below.
            (TriggerState::Idle, IntentEvent::NoOp)
            | (TriggerState::Idle, IntentEvent::MovedCursor)
            | (TriggerState::Idle, IntentEvent::DeletedChar)
            | (TriggerState::Idle, IntentEvent::Pasted)
            | (TriggerState::Idle, IntentEvent::SetText)
            | (TriggerState::Idle, IntentEvent::Accepted)
            | (TriggerState::Idle, IntentEvent::Dismissed)
            | (TriggerState::Idle, IntentEvent::Submitted) => TriggerTransition::None,

            // Idle + TypedChar: maybe open Mention/Slash.
            (TriggerState::Idle, IntentEvent::TypedChar(c)) => {
                self.maybe_open(*c, text, cursor, protected_ranges)
            }

            // SlashArg has its own transition rules.
            (
                TriggerState::Composing {
                    kind: TriggerKindInternal::SlashArg { .. },
                    ..
                },
                _,
            ) => self.advance_slash_arg(event.clone(), text, cursor),

            // Mention/Slash composing — existing logic.
            (TriggerState::Composing { .. }, _) => {
                self.advance_composing(event.clone(), text, cursor)
            }
        };

        // SlashArg detection: if dispatch returned us to Idle and the event
        // is one that can mutate buffer/cursor in a way that opens a picker,
        // try to parse `^/<cmd> ` and consult the registry.
        if matches!(self.state, TriggerState::Idle) && event_can_trigger_slash_arg(&event) {
            if let Some((cmd, prefix_start)) = parse_slash_arg_prefix(text, cursor) {
                if registry_arg_picker(cmd) {
                    let command_name = cmd.to_string();
                    self.state = TriggerState::Composing {
                        kind: TriggerKindInternal::SlashArg {
                            command_name: command_name.clone(),
                        },
                        prefix_start,
                    };
                    let query = text[prefix_start..cursor.min(text.len())].to_string();
                    return TriggerTransition::Open {
                        trigger: Trigger {
                            kind: TriggerKind::SlashArg { command_name },
                            prefix_start,
                            query,
                        },
                    };
                }
            }
        }

        dispatched
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
                    kind: TriggerKindInternal::Slash,
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
                    kind: TriggerKindInternal::Mention,
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
        let prefix_start = match &self.state {
            TriggerState::Composing { prefix_start, .. } => *prefix_start,
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

    /// SlashArg → SlashArg|Idle transition logic. The defensive re-check at
    /// the top of `step` already validated `^/<cmd> ` is intact and that
    /// `prefix_start` still aligns; this just maps events to transitions.
    fn advance_slash_arg(
        &mut self,
        event: IntentEvent,
        text: &str,
        cursor: usize,
    ) -> TriggerTransition {
        let prefix_start = match &self.state {
            TriggerState::Composing {
                kind: TriggerKindInternal::SlashArg { .. },
                prefix_start,
            } => *prefix_start,
            _ => unreachable!("advance_slash_arg called outside SlashArg state"),
        };

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

        // Cursor must remain at or past the arg-region start for the popup
        // to stay open.
        if cursor < prefix_start {
            self.state = TriggerState::Idle;
            return TriggerTransition::Close;
        }

        let clamped_end = cursor.min(text.len());
        let query = text[prefix_start.min(clamped_end)..clamped_end].to_string();
        TriggerTransition::Update { query }
    }
}

/// Returns whether `event` is a category of input mutation that may newly
/// open a SlashArg trigger when the buffer matches `^/<cmd> ` and the
/// registry reports an arg picker. Pure cursor motion and lifecycle events
/// (NoOp, Accepted, Dismissed, Submitted) intentionally do not open
/// pickers — that preserves the "cursor motion never opens" invariant.
fn event_can_trigger_slash_arg(event: &IntentEvent) -> bool {
    matches!(
        event,
        IntentEvent::TypedChar(_)
            | IntentEvent::Pasted
            | IntentEvent::SetText
            | IntentEvent::DeletedChar
    )
}

/// Parse a buffer for `^/<command> ` and return `(command_name, prefix_start)`
/// where `prefix_start` is the byte offset just past the delimiting
/// whitespace. Returns `None` when:
/// - `text` does not start with `/`
/// - the command name is empty
/// - there is no whitespace after the command name
/// - the cursor is positioned before the arg region (caller hasn't moved
///   into the arg yet)
fn parse_slash_arg_prefix(text: &str, cursor: usize) -> Option<(&str, usize)> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'/') {
        return None;
    }
    let mut end_of_cmd = 1;
    while end_of_cmd < bytes.len() && !bytes[end_of_cmd].is_ascii_whitespace() {
        end_of_cmd += 1;
    }
    if end_of_cmd == 1 {
        return None;
    }
    if end_of_cmd >= bytes.len() {
        return None;
    }
    let cmd = &text[1..end_of_cmd];
    let prefix_start = end_of_cmd + 1;
    if prefix_start > cursor {
        return None;
    }
    Some((cmd, prefix_start))
}

#[cfg(test)]
mod detector_tests {
    use super::*;
    use crate::components::input_bar::{ProtectedRange, RangeKind};

    fn d() -> TriggerDetector {
        TriggerDetector::new()
    }

    // ── Task 3: Idle transitions (11 tests) ──────────────────────────

    #[test]
    fn idle_typed_at_at_offset_zero_opens_mention() {
        let mut det = d();
        let t = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
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
        let t = det.step(IntentEvent::TypedChar('/'), "/", 1, &[], |_| false);
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
        let t = det.step(IntentEvent::TypedChar('/'), "a/", 2, &[], |_| false);
        assert!(matches!(t, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn idle_typed_at_after_non_whitespace_stays_idle() {
        let mut det = d();
        let t = det.step(IntentEvent::TypedChar('@'), "foo@", 4, &[], |_| false);
        assert!(matches!(t, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn idle_typed_at_after_whitespace_opens() {
        let mut det = d();
        let t = det.step(IntentEvent::TypedChar('@'), "foo @", 5, &[], |_| false);
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
            kind: RangeKind::Atom,
            uri: "u".into(),
            name: "n".into(),
        }];
        let t = det.step(IntentEvent::TypedChar('@'), "@foo", 1, &ranges, |_| false);
        assert!(matches!(t, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn idle_moved_cursor_stays_idle_emits_none() {
        let mut det = d();
        let t = det.step(IntentEvent::MovedCursor, "hello @world", 12, &[], |_| false);
        assert!(matches!(t, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn idle_deleted_char_stays_idle() {
        let mut det = d();
        let t = det.step(IntentEvent::DeletedChar, "hello", 5, &[], |_| false);
        assert!(matches!(t, TriggerTransition::None));
    }

    #[test]
    fn idle_pasted_stays_idle() {
        let mut det = d();
        let t = det.step(IntentEvent::Pasted, "pasted @alice text", 18, &[], |_| false);
        assert!(matches!(t, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn idle_set_text_stays_idle() {
        let mut det = d();
        let t = det.step(IntentEvent::SetText, "recalled @foo", 13, &[], |_| false);
        assert!(matches!(t, TriggerTransition::None));
    }

    #[test]
    fn idle_noop_emits_none() {
        let mut det = d();
        let t = det.step(IntentEvent::NoOp, "", 0, &[], |_| false);
        assert!(matches!(t, TriggerTransition::None));
    }

    // ── Task 4: Composing transitions (12 tests) ─────────────────────

    #[test]
    fn composing_typed_char_emits_update_with_growing_query() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
        let t = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[], |_| false);
        match t {
            TriggerTransition::Update { query } => assert_eq!(query, "f"),
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn composing_deleted_char_emits_update_with_shrunken_query() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
        let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[], |_| false);
        let _ = det.step(IntentEvent::TypedChar('o'), "@fo", 3, &[], |_| false);
        let t = det.step(IntentEvent::DeletedChar, "@f", 2, &[], |_| false);
        match t {
            TriggerTransition::Update { query } => assert_eq!(query, "f"),
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn composing_moved_cursor_inside_window_emits_update() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
        let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[], |_| false);
        let _ = det.step(IntentEvent::TypedChar('o'), "@fo", 3, &[], |_| false);
        let _ = det.step(IntentEvent::TypedChar('o'), "@foo", 4, &[], |_| false);
        let t = det.step(IntentEvent::MovedCursor, "@foo", 3, &[], |_| false);
        match t {
            TriggerTransition::Update { query } => assert_eq!(query, "fo"),
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn composing_typed_whitespace_emits_close() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
        let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[], |_| false);
        let t = det.step(IntentEvent::TypedChar(' '), "@f ", 3, &[], |_| false);
        assert!(matches!(t, TriggerTransition::Close));
        assert!(det.is_idle());
    }

    #[test]
    fn composing_moved_cursor_out_of_window_emits_close() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
        let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[], |_| false);
        let t = det.step(IntentEvent::MovedCursor, "@f", 0, &[], |_| false);
        assert!(matches!(t, TriggerTransition::Close));
        assert!(det.is_idle());
    }

    #[test]
    fn composing_deleted_trigger_char_emits_close_via_defensive_check() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
        let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[], |_| false);
        let _ = det.step(IntentEvent::DeletedChar, "@", 1, &[], |_| false);
        let t = det.step(IntentEvent::DeletedChar, "", 0, &[], |_| false);
        assert!(matches!(t, TriggerTransition::Close));
        assert!(det.is_idle());
    }

    #[test]
    fn composing_pasted_emits_close() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
        let t = det.step(IntentEvent::Pasted, "@ hello world", 13, &[], |_| false);
        assert!(matches!(t, TriggerTransition::Close));
        assert!(det.is_idle());
    }

    #[test]
    fn composing_set_text_emits_close() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
        let t = det.step(IntentEvent::SetText, "recalled text", 13, &[], |_| false);
        assert!(matches!(t, TriggerTransition::Close));
    }

    #[test]
    fn composing_accepted_emits_close() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
        let t = det.step(IntentEvent::Accepted, "@atom", 5, &[], |_| false);
        assert!(matches!(t, TriggerTransition::Close));
    }

    #[test]
    fn composing_dismissed_emits_close() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
        let t = det.step(IntentEvent::Dismissed, "@", 1, &[], |_| false);
        assert!(matches!(t, TriggerTransition::Close));
    }

    #[test]
    fn composing_submitted_emits_close() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
        let t = det.step(IntentEvent::Submitted, "", 0, &[], |_| false);
        assert!(matches!(t, TriggerTransition::Close));
    }

    #[test]
    fn composing_noop_emits_none_and_stays_composing() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
        let t = det.step(IntentEvent::NoOp, "@", 1, &[], |_| false);
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
                kind: RangeKind::Atom,
                uri: "a".into(),
                name: "a".into(),
            },
            ProtectedRange {
                start: 16,
                end: 28,
                kind: RangeKind::Atom,
                uri: "b".into(),
                name: "b".into(),
            },
        ];
        let mut opens = 0;
        for cursor in 0..=text.len() {
            let t = det.step(IntentEvent::MovedCursor, text, cursor, &ranges, |_| false);
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
            let t = det.step(IntentEvent::MovedCursor, text, 15, &[], |_| false);
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
            let t = det.step(IntentEvent::MovedCursor, text, cursor, &[], |_| false);
            if matches!(t, TriggerTransition::Open { .. }) {
                opens += 1;
            }
        }
        assert_eq!(opens, 0);
    }

    #[test]
    fn j2b_typo_fix_after_esc_stays_closed_on_motion() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
        let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[], |_| false);
        let _ = det.step(IntentEvent::TypedChar('o'), "@fo", 3, &[], |_| false);
        let close = det.step(IntentEvent::Dismissed, "@fo", 3, &[], |_| false);
        assert!(matches!(close, TriggerTransition::Close));
        let mot = det.step(IntentEvent::MovedCursor, "@fo", 2, &[], |_| false);
        assert!(matches!(mot, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn reset_puts_detector_in_idle() {
        let mut det = d();
        let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
        assert!(!det.is_idle());
        det.reset();
        assert!(det.is_idle());
    }

    #[test]
    fn defensive_reset_when_prefix_start_past_text_len() {
        // White-box: construct a Composing state with prefix_start past text.
        let mut det = TriggerDetector {
            state: TriggerState::Composing {
                kind: TriggerKindInternal::Mention,
                prefix_start: 100,
            },
        };
        let t = det.step(IntentEvent::MovedCursor, "abc", 3, &[], |_| false);
        assert!(matches!(t, TriggerTransition::Close));
        assert!(det.is_idle());
    }
}

#[cfg(test)]
mod arg_picker_tests {
    use super::*;
    use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};
    use crate::commands::CommandRegistry;
    use spur_acp::adapter::arg_picker_hint::{ArgPickerHint, ArgPickerSpec};

    fn registry_with_arg_picker(names: &[&str]) -> CommandRegistry {
        let mut reg = CommandRegistry::new();
        let entries: Vec<CommandEntry> = names
            .iter()
            .map(|n| CommandEntry {
                name: (*n).into(),
                description: "test".into(),
                hint: None,
                source: CommandSource::Advertised {
                    handle: "codex".into(),
                },
                dispatch: Dispatch::SetSessionConfigOption {
                    config_id: (*n).into(),
                },
                arg_picker_spec: Some(ArgPickerSpec {
                    free_text_hint: String::new(),
                    typed_hint: Some(ArgPickerHint::ConfigOption {
                        config_id: (*n).into(),
                    }),
                }),
            })
            .collect();
        reg.set_advertised_commands("codex", entries);
        reg
    }

    fn step(
        det: &mut TriggerDetector,
        event: IntentEvent,
        text: &str,
        cursor: usize,
        registry: &CommandRegistry,
    ) -> TriggerTransition {
        det.step(event, text, cursor, &[], |name| {
            registry.arg_picker_spec(name).is_some()
        })
    }

    // ── T1–T5: regression guards (existing Mention/Slash unchanged) ─

    #[test]
    fn t1_mention_at_offset_zero_still_opens() {
        let mut det = TriggerDetector::new();
        let registry = registry_with_arg_picker(&["model"]);
        let txn = step(&mut det, IntentEvent::TypedChar('@'), "@", 1, &registry);
        match txn {
            TriggerTransition::Open { trigger } => {
                assert_eq!(trigger.kind, TriggerKind::Mention);
                assert_eq!(trigger.prefix_start, 0);
            }
            other => panic!("expected Mention Open, got {other:?}"),
        }
    }

    #[test]
    fn t2_slash_at_offset_zero_still_opens() {
        let mut det = TriggerDetector::new();
        let registry = registry_with_arg_picker(&["model"]);
        let txn = step(&mut det, IntentEvent::TypedChar('/'), "/", 1, &registry);
        match txn {
            TriggerTransition::Open { trigger } => {
                assert_eq!(trigger.kind, TriggerKind::Slash);
                assert_eq!(trigger.prefix_start, 0);
            }
            other => panic!("expected Slash Open, got {other:?}"),
        }
    }

    #[test]
    fn t3_slash_continuation_still_emits_update() {
        let mut det = TriggerDetector::new();
        let registry = registry_with_arg_picker(&["model"]);
        let _ = step(&mut det, IntentEvent::TypedChar('/'), "/", 1, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('h'), "/h", 2, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('e'), "/he", 3, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('l'), "/hel", 4, &registry);
        let txn = step(&mut det, IntentEvent::TypedChar('p'), "/help", 5, &registry);
        match txn {
            TriggerTransition::Update { query } => assert_eq!(query, "help"),
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn t4_mention_after_whitespace_still_opens() {
        let mut det = TriggerDetector::new();
        let registry = registry_with_arg_picker(&["model"]);
        let txn = step(&mut det, IntentEvent::TypedChar('@'), "foo @", 5, &registry);
        match txn {
            TriggerTransition::Open { trigger } => {
                assert_eq!(trigger.kind, TriggerKind::Mention);
                assert_eq!(trigger.prefix_start, 4);
            }
            other => panic!("expected Mention Open, got {other:?}"),
        }
    }

    #[test]
    fn t5_unregistered_slash_command_with_space_closes() {
        let mut det = TriggerDetector::new();
        let registry = registry_with_arg_picker(&["model"]); // /help not registered
        let _ = step(&mut det, IntentEvent::TypedChar('/'), "/", 1, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('h'), "/h", 2, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('e'), "/he", 3, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('l'), "/hel", 4, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('p'), "/help", 5, &registry);
        let txn = step(&mut det, IntentEvent::TypedChar(' '), "/help ", 6, &registry);
        assert!(matches!(txn, TriggerTransition::Close));
        assert!(det.is_idle());
    }

    // ── T6–T15: SlashArg behavior ────────────────────────────────────

    #[test]
    fn t6_slash_model_space_at_end_opens_slash_arg() {
        let mut det = TriggerDetector::new();
        let registry = registry_with_arg_picker(&["model"]);
        let _ = step(&mut det, IntentEvent::TypedChar('/'), "/", 1, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('m'), "/m", 2, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('o'), "/mo", 3, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('d'), "/mod", 4, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('e'), "/mode", 5, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('l'), "/model", 6, &registry);
        let txn = step(&mut det, IntentEvent::TypedChar(' '), "/model ", 7, &registry);
        match txn {
            TriggerTransition::Open { trigger } => {
                assert_eq!(
                    trigger.kind,
                    TriggerKind::SlashArg {
                        command_name: "model".into(),
                    }
                );
                assert_eq!(trigger.prefix_start, 7);
                assert_eq!(trigger.query, "");
            }
            other => panic!("expected Open SlashArg, got {other:?}"),
        }
    }

    #[test]
    fn t7_slash_effort_space_at_end_opens_slash_arg() {
        let mut det = TriggerDetector::new();
        let registry = registry_with_arg_picker(&["effort"]);
        let _ = step(&mut det, IntentEvent::TypedChar('/'), "/", 1, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('e'), "/e", 2, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('f'), "/ef", 3, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('f'), "/eff", 4, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('o'), "/effo", 5, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('r'), "/effor", 6, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('t'), "/effort", 7, &registry);
        let txn = step(&mut det, IntentEvent::TypedChar(' '), "/effort ", 8, &registry);
        match txn {
            TriggerTransition::Open { trigger } => {
                assert_eq!(
                    trigger.kind,
                    TriggerKind::SlashArg {
                        command_name: "effort".into(),
                    }
                );
                assert_eq!(trigger.prefix_start, 8);
            }
            other => panic!("expected Open SlashArg, got {other:?}"),
        }
    }

    #[test]
    fn t8_slash_command_without_trailing_space_does_not_open() {
        let mut det = TriggerDetector::new();
        let registry = registry_with_arg_picker(&["model"]);
        // Pasted "/model" — no trailing whitespace, so parse rejects.
        let txn = step(&mut det, IntentEvent::Pasted, "/model", 6, &registry);
        assert!(matches!(txn, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn t9_typing_arg_after_open_emits_update() {
        let mut det = TriggerDetector::new();
        let registry = registry_with_arg_picker(&["model"]);
        let _ = step(&mut det, IntentEvent::TypedChar('/'), "/", 1, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('m'), "/m", 2, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('o'), "/mo", 3, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('d'), "/mod", 4, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('e'), "/mode", 5, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('l'), "/model", 6, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar(' '), "/model ", 7, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('g'), "/model g", 8, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('p'), "/model gp", 9, &registry);
        let txn = step(&mut det, IntentEvent::TypedChar('t'), "/model gpt", 10, &registry);
        match txn {
            TriggerTransition::Update { query } => assert_eq!(query, "gpt"),
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn t10_delete_space_closes_slash_arg() {
        let mut det = TriggerDetector::new();
        let registry = registry_with_arg_picker(&["model"]);
        let _ = step(&mut det, IntentEvent::TypedChar('/'), "/", 1, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('m'), "/m", 2, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('o'), "/mo", 3, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('d'), "/mod", 4, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('e'), "/mode", 5, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar('l'), "/model", 6, &registry);
        let _ = step(&mut det, IntentEvent::TypedChar(' '), "/model ", 7, &registry);
        // Now in SlashArg. Delete the space.
        let txn = step(&mut det, IntentEvent::DeletedChar, "/model", 6, &registry);
        assert!(matches!(txn, TriggerTransition::Close));
        assert!(det.is_idle());
    }

    #[test]
    fn t11_cursor_outside_arg_region_does_not_open() {
        let mut det = TriggerDetector::new();
        let registry = registry_with_arg_picker(&["model"]);
        // Fresh Idle: SetText to "/model gpt" with cursor at 3 (still inside
        // "/mo", before the arg region at byte 7). parse rejects (cursor
        // before arg region) → no SlashArg open.
        let txn = step(&mut det, IntentEvent::SetText, "/model gpt", 3, &registry);
        assert!(matches!(txn, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn t12_unknown_command_does_not_open() {
        let mut det = TriggerDetector::new();
        let registry = registry_with_arg_picker(&["model"]); // /unknown not registered
        let txn = step(&mut det, IntentEvent::Pasted, "/unknown foo", 12, &registry);
        assert!(matches!(txn, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn t13_slash_not_at_column_zero_does_not_open() {
        let mut det = TriggerDetector::new();
        let registry = registry_with_arg_picker(&["model"]);
        let txn = step(
            &mut det,
            IntentEvent::Pasted,
            "prefix /model bar",
            17,
            &registry,
        );
        assert!(matches!(txn, TriggerTransition::None));
        assert!(det.is_idle());
    }

    #[test]
    fn t14_pasted_full_slash_arg_string_opens() {
        let mut det = TriggerDetector::new();
        let registry = registry_with_arg_picker(&["model"]);
        let txn = step(
            &mut det,
            IntentEvent::Pasted,
            "/model gpt-5",
            12,
            &registry,
        );
        match txn {
            TriggerTransition::Open { trigger } => {
                assert_eq!(
                    trigger.kind,
                    TriggerKind::SlashArg {
                        command_name: "model".into(),
                    }
                );
                assert_eq!(trigger.prefix_start, 7);
                assert_eq!(trigger.query, "gpt-5");
            }
            other => panic!("expected Open SlashArg, got {other:?}"),
        }
    }

    #[test]
    fn t15_command_lookup_is_case_sensitive() {
        let mut det = TriggerDetector::new();
        let registry = registry_with_arg_picker(&["model"]); // lowercase only
        let txn = step(&mut det, IntentEvent::Pasted, "/Model ", 7, &registry);
        assert!(matches!(txn, TriggerTransition::None));
        assert!(det.is_idle());
    }
}
