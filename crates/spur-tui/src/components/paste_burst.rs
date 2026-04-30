//! Timing-based fallback paste detector for terminals that do not emit
//! bracketed-paste events.
//!
//! Crossterm normally reports clipboard paste as `Event::Paste`, but some
//! terminal paths degrade a paste into a stream of ordinary key events. This
//! module classifies those streams by timing: three plain character keys inside
//! `PASTE_BURST_CHAR_INTERVAL` arm the detector. The first three characters
//! have already flowed through to the `InputBar`; subsequent characters are
//! buffered until an idle tick flushes them through `InputBar::insert_paste`.
//!
//! The state machine is intentionally small:
//!
//! * Idle: no candidate burst is active. Slow typing resets the consecutive
//!   character count.
//! * Candidate: one or two fast characters have been observed, but they still
//!   pass through normally so ordinary typing has no visible delay.
//! * Active: the third fast character arms the detector. Later characters and
//!   every Enter key are buffered, then flushed after
//!   `PASTE_BURST_ACTIVE_IDLE_TIMEOUT`.
//! * Suppress-window: after a flush, a trailing Enter inside
//!   `PASTE_ENTER_SUPPRESS_WINDOW` inserts a newline instead of submitting.
//!
//! `PasteBurst` does not mutate the textarea. It only returns decisions and
//! optional flushed text. `InputBar` owns the user-visible contract: pass-through
//! keys are inserted normally, buffered keys become `NoOp`, flushed text is
//! routed through `insert_paste`, and completion triggers receive `Pasted` when
//! text is force-flushed or idle-flushed.
//!
//! The buffer is bounded so a pathological key stream cannot grow memory
//! without limit. On overflow, the existing buffer is force-flushed and the
//! current key starts the next burst buffer.
//!
//! In test builds the fallback is opt-in at the `InputBar` layer so older tests
//! that synthesize zero-delay typing keep normal Enter-submit behavior. Dedicated
//! paste-burst tests explicitly enable it before exercising this module.
// See also: /tmp/spur-research/codex/codex-rs/tui/src/bottom_pane/paste_burst.rs

use std::time::{Duration, Instant};

const PASTE_BURST_MIN_CHARS: u16 = 3;
const PASTE_ENTER_SUPPRESS_WINDOW: Duration = Duration::from_millis(120);
// Hard cap for raw key-event paste buffering. Larger streams are chunked
// through `InputBar::insert_paste` instead of growing this String unboundedly.
const MAX_BURST_BUFFER_LEN: usize = 512_000;

#[cfg(not(windows))]
const PASTE_BURST_CHAR_INTERVAL: Duration = Duration::from_millis(8);
#[cfg(windows)]
const PASTE_BURST_CHAR_INTERVAL: Duration = Duration::from_millis(30);

#[cfg(not(windows))]
const PASTE_BURST_ACTIVE_IDLE_TIMEOUT: Duration = Duration::from_millis(8);
#[cfg(windows)]
const PASTE_BURST_ACTIVE_IDLE_TIMEOUT: Duration = Duration::from_millis(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CharDecision {
    PassThrough,
    Armed,
    Buffered,
    Flushed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnterDecision {
    Submit,
    BufferNewline,
    InsertNewline,
    Flushed(String),
}

#[derive(Debug, Default)]
pub(crate) struct PasteBurst {
    last_char_at: Option<Instant>,
    last_burst_input_at: Option<Instant>,
    consecutive_chars: u16,
    suppress_enter_until: Option<Instant>,
    buffer: String,
    active: bool,
}

impl PasteBurst {
    pub(crate) fn on_char(&mut self, ch: char, now: Instant) -> CharDecision {
        self.note_char(now);

        if self.active {
            return self.buffer_char(ch, now);
        }

        if self.consecutive_chars >= PASTE_BURST_MIN_CHARS {
            self.active = true;
            self.mark_burst_input(now);
            return CharDecision::Armed;
        }

        CharDecision::PassThrough
    }

    pub(crate) fn on_enter(&mut self, now: Instant) -> EnterDecision {
        if self.active {
            return match self.buffer_char('\n', now) {
                CharDecision::Buffered => EnterDecision::BufferNewline,
                CharDecision::Flushed(text) => EnterDecision::Flushed(text),
                CharDecision::PassThrough | CharDecision::Armed => unreachable!(
                    "buffer_char only returns Buffered or Flushed while a burst is active"
                ),
            };
        }

        if self
            .suppress_enter_until
            .is_some_and(|deadline| now <= deadline)
        {
            self.suppress_enter_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            return EnterDecision::InsertNewline;
        }

        self.clear_classification_window();
        EnterDecision::Submit
    }

    pub(crate) fn flush_if_idle(&mut self, now: Instant) -> Option<String> {
        if !self.active {
            return None;
        }
        let last_input_at = self.last_burst_input_at?;
        if now.duration_since(last_input_at) <= PASTE_BURST_ACTIVE_IDLE_TIMEOUT {
            return None;
        }

        self.active = false;
        let text = std::mem::take(&mut self.buffer);
        (!text.is_empty()).then_some(text)
    }

    pub(crate) fn flush_now(&mut self) -> Option<String> {
        self.active = false;
        self.last_burst_input_at = None;
        let text = std::mem::take(&mut self.buffer);
        (!text.is_empty()).then_some(text)
    }

    pub(crate) fn clear(&mut self) {
        self.last_char_at = None;
        self.last_burst_input_at = None;
        self.consecutive_chars = 0;
        self.suppress_enter_until = None;
        self.buffer.clear();
        self.active = false;
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn is_fast_continuation(&self, now: Instant) -> bool {
        self.last_char_at
            .is_some_and(|prev| now.duration_since(prev) <= PASTE_BURST_CHAR_INTERVAL)
    }

    fn note_char(&mut self, now: Instant) {
        match self.last_char_at {
            Some(prev) if now.duration_since(prev) <= PASTE_BURST_CHAR_INTERVAL => {
                self.consecutive_chars = self.consecutive_chars.saturating_add(1);
            }
            _ => {
                self.consecutive_chars = 1;
            }
        }
        self.last_char_at = Some(now);
    }

    fn mark_burst_input(&mut self, now: Instant) {
        self.last_burst_input_at = Some(now);
        self.suppress_enter_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
    }

    fn buffer_char(&mut self, ch: char, now: Instant) -> CharDecision {
        let flushed = if self.buffer.len() + ch.len_utf8() > MAX_BURST_BUFFER_LEN {
            self.flush_now()
        } else {
            None
        };

        if flushed.is_some() {
            self.active = true;
        }
        self.buffer.push(ch);
        self.mark_burst_input(now);

        match flushed {
            Some(text) => CharDecision::Flushed(text),
            None => CharDecision::Buffered,
        }
    }

    fn clear_classification_window(&mut self) {
        self.last_char_at = None;
        self.consecutive_chars = 0;
    }

    #[cfg(test)]
    pub(crate) fn recommended_active_flush_delay() -> Duration {
        PASTE_BURST_ACTIVE_IDLE_TIMEOUT + Duration::from_millis(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_arms_after_three_fast_chars() {
        let mut burst = PasteBurst::default();
        let t0 = Instant::now();

        assert_eq!(burst.on_char('a', t0), CharDecision::PassThrough);
        assert_eq!(
            burst.on_char('b', t0 + Duration::from_millis(1)),
            CharDecision::PassThrough
        );
        assert_eq!(
            burst.on_char('c', t0 + Duration::from_millis(2)),
            CharDecision::Armed
        );
        assert!(burst.active);
    }

    #[test]
    fn enter_inside_burst_is_buffered_as_newline() {
        let (mut burst, armed_at) = active_burst();
        let now = armed_at + Duration::from_millis(1);

        assert_eq!(burst.on_enter(now), EnterDecision::BufferNewline);
        assert_eq!(
            burst.flush_if_idle(now + PasteBurst::recommended_active_flush_delay()),
            Some("\n".to_string())
        );
    }

    #[test]
    fn idle_timeout_flushes_buffered_paste() {
        let (mut burst, armed_at) = active_burst();
        let now = armed_at + Duration::from_millis(1);

        assert_eq!(burst.on_enter(now), EnterDecision::BufferNewline);
        assert_eq!(
            burst.on_char('d', now + Duration::from_millis(1)),
            CharDecision::Buffered
        );
        assert_eq!(burst.flush_if_idle(now + Duration::from_millis(1)), None);
        assert_eq!(
            burst.flush_if_idle(
                now + Duration::from_millis(1) + PasteBurst::recommended_active_flush_delay()
            ),
            Some("\nd".to_string())
        );
    }

    #[test]
    fn active_burst_buffers_single_line_chars_before_first_newline() {
        let (mut burst, armed_at) = active_burst();
        let now = armed_at + Duration::from_millis(1);

        assert_eq!(burst.on_char('d', now), CharDecision::Buffered);
        assert_eq!(
            burst.flush_if_idle(now + PasteBurst::recommended_active_flush_delay()),
            Some("d".to_string())
        );
    }

    #[test]
    fn char_overflow_flushes_existing_buffer_and_keeps_current_char_buffered() {
        let (mut burst, armed_at) = active_burst();
        let now = armed_at + Duration::from_millis(1);
        burst.buffer = "x".repeat(MAX_BURST_BUFFER_LEN);

        match burst.on_char('y', now) {
            CharDecision::Flushed(text) => {
                assert_eq!(text.len(), MAX_BURST_BUFFER_LEN);
                assert!(text.bytes().all(|byte| byte == b'x'));
            }
            decision => panic!("expected overflow flush, got {decision:?}"),
        }

        assert_eq!(
            burst.flush_if_idle(now + PasteBurst::recommended_active_flush_delay()),
            Some("y".to_string())
        );
    }

    #[test]
    fn enter_outside_burst_is_submit() {
        let mut burst = PasteBurst::default();

        assert_eq!(burst.on_enter(Instant::now()), EnterDecision::Submit);
    }

    #[test]
    fn post_flush_suppress_window_catches_trailing_enter() {
        let (mut burst, armed_at) = active_burst();
        let now = armed_at + Duration::from_millis(1);
        assert_eq!(burst.on_enter(now), EnterDecision::BufferNewline);
        assert_eq!(
            burst.flush_if_idle(now + PasteBurst::recommended_active_flush_delay()),
            Some("\n".to_string())
        );

        assert_eq!(
            burst.on_enter(now + PasteBurst::recommended_active_flush_delay()),
            EnterDecision::InsertNewline
        );
    }

    fn active_burst() -> (PasteBurst, Instant) {
        let mut burst = PasteBurst::default();
        let t0 = Instant::now();
        assert_eq!(burst.on_char('a', t0), CharDecision::PassThrough);
        assert_eq!(
            burst.on_char('b', t0 + Duration::from_millis(1)),
            CharDecision::PassThrough
        );
        assert_eq!(
            burst.on_char(' ', t0 + Duration::from_millis(2)),
            CharDecision::Armed
        );
        (burst, t0 + Duration::from_millis(2))
    }
}
