//! Brain turn scheduler — see
//! `docs/superpowers/specs/2026-04-19-brain-async-continuation-design.md`.
//!
//! Pure-sync policy. No tokio primitives; unit-testable without a runtime.

use spur_acp::domain::BrainContinuation;
use spur_acp::types::SessionId;
use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::orchestrator::InteractiveInput;

pub const CANCEL_GRACE_DEFAULT: Duration = Duration::from_millis(750);

#[derive(Debug)]
pub enum ScheduledAction {
    /// Fire a user turn. Caller flattens into `PromptRequest`.
    UserPrompt(InteractiveInput),
    /// Fire an autonomous continuation turn with these coalesced continuations.
    ContinuationPrompt(Vec<BrainContinuation>),
    /// Fire a merged turn: user input foreground, continuations as background blocks.
    MergedPrompt {
        user: InteractiveInput,
        continuations: Vec<BrainContinuation>,
    },
    /// Nothing to do.
    Idle,
}

/// Owns the split-lane queues and scheduling policy for brain turns.
pub struct BrainScheduler {
    /// FIFO queue of user-originated inputs (from the TUI). Always drains
    /// before continuations (INV-C4 human-priority).
    pending_user:          VecDeque<InteractiveInput>,
    /// FIFO queue of detached worker outcomes awaiting a safe scheduling
    /// window. Populated via `push_continuation`; drained in coalesced
    /// batches by `next()` (Task 4).
    pending_continuations: VecDeque<BrainContinuation>,
    /// Already-delivered delegation IDs for dedup (INV-C5 idempotency).
    /// Grows without a cap in v1 — long-lived sessions with many async
    /// delegations will accumulate entries. Bound is a future concern
    /// (e.g. LRU cap at 2048) if observed; N for a typical session is
    /// small. Migrates to `HashSet<DelegationId>` when INV-1 lands.
    delivered_ids:         HashSet<String>,
    /// Active brain session guard for G2 session-swap eviction. `None`
    /// before the first brain spawns. Uses `SessionId` to match the
    /// orchestrator's `brain.acp_session_id` idiom; will migrate to
    /// `BrainSessionId` as part of the broader INV-2 orchestrator
    /// migration (not this spec).
    active_session:        Option<SessionId>,
    /// INV-C6: at most one `session/prompt` in flight per brain session.
    /// When `true`, `next()` (Task 4) returns `ScheduledAction::Idle`.
    turn_in_flight:        bool,
    /// G5 post-cancel grace: autonomous continuation turns are suppressed
    /// until this instant elapses. A user prompt arriving during grace
    /// clears the window (user intent trumps grace).
    cancel_grace_until:    Option<Instant>,
    /// G5: post-cancel grace window duration. Default `CANCEL_GRACE_DEFAULT`
    /// (750 ms), overridable at construction time via `SPUR_CANCEL_GRACE_MS`.
    cancel_grace_window:   Duration,
}

impl BrainScheduler {
    pub fn new(active_session: Option<SessionId>) -> Self {
        let cancel_grace_window = std::env::var("SPUR_CANCEL_GRACE_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(CANCEL_GRACE_DEFAULT);
        Self {
            pending_user: VecDeque::new(),
            pending_continuations: VecDeque::new(),
            delivered_ids: HashSet::new(),
            active_session,
            turn_in_flight: false,
            cancel_grace_until: None,
            cancel_grace_window,
        }
    }

    pub fn push_user(&mut self, input: InteractiveInput) {
        self.pending_user.push_back(input);
    }

    /// Idempotent: duplicate `delegation_id` pushes are dropped silently.
    pub fn push_continuation(&mut self, c: BrainContinuation) {
        if self.delivered_ids.contains(&c.delegation_id) {
            return;
        }
        if self.pending_continuations.iter().any(|q| q.delegation_id == c.delegation_id) {
            return;
        }
        self.pending_continuations.push_back(c);
    }

    pub fn note_turn_started(&mut self) {
        self.turn_in_flight = true;
    }

    pub fn note_turn_finished(&mut self) {
        self.turn_in_flight = false;
    }

    /// Call AFTER cancel has fully resolved (stream drained / force-timeout fired).
    pub fn note_cancel_resolved(&mut self, now: Instant) {
        self.cancel_grace_until = Some(now + self.cancel_grace_window);
    }

    /// Clears the grace window if any user input is queued.
    ///
    /// Called at the top of `next()` unconditionally, BEFORE the in-flight
    /// gate. This is intentional: the grace window represents "do not
    /// harass the user with autonomous continuations just after cancel";
    /// once user intent is established by a queued item, grace is stale
    /// regardless of whether we are currently in-flight or can yet dispatch.
    fn clear_grace_if_user_arrived(&mut self) {
        if !self.pending_user.is_empty() {
            self.cancel_grace_until = None;
        }
    }

    fn in_cancel_grace(&self, now: Instant) -> bool {
        match self.cancel_grace_until {
            Some(t) => now < t,
            None => false,
        }
    }

    /// Pure sync: given the current clock, return the next action.
    /// Mutates internal queues for any action that delivers continuations.
    ///
    /// **At-most-once delivery:** a continuation popped into any prompt
    /// variant is recorded in `delivered_ids` immediately. If the caller
    /// fails to actually deliver the prompt to the brain, those
    /// continuations are lost. This is consistent with the spec's INV-C5
    /// idempotency rule and the scheduler's "report and decide" role —
    /// failure recovery is the dispatcher's responsibility, not the
    /// scheduler's.
    pub fn next(&mut self, now: Instant) -> ScheduledAction {
        self.clear_grace_if_user_arrived();

        if self.turn_in_flight {
            return ScheduledAction::Idle;
        }

        // User priority.
        if let Some(user) = self.pending_user.pop_front() {
            let continuations = if self.pending_continuations.is_empty() {
                Vec::new()
            } else {
                self.drain_continuations_for_delivery()
            };
            if continuations.is_empty() {
                return ScheduledAction::UserPrompt(user);
            }
            return ScheduledAction::MergedPrompt { user, continuations };
        }

        // No user queued: can we fire an autonomous continuation?
        if self.pending_continuations.is_empty() {
            return ScheduledAction::Idle;
        }
        if self.in_cancel_grace(now) {
            return ScheduledAction::Idle;
        }
        ScheduledAction::ContinuationPrompt(self.drain_continuations_for_delivery())
    }

    /// Drains ALL pending continuations. Merge-byte-budget enforcement
    /// lives at the prompt-builder layer (Task 7), not here — the
    /// scheduler hands over the full batch and the builder spills.
    fn drain_continuations_for_delivery(&mut self) -> Vec<BrainContinuation> {
        let batch: Vec<_> = self.pending_continuations.drain(..).collect();
        for c in &batch {
            self.delivered_ids.insert(c.delegation_id.clone());
        }
        batch
    }

    /// Evict continuations tagged for a session other than the new active
    /// one. Returns the evicted continuations so the caller can emit
    /// `ContinuationDropped` events for audit.
    pub fn note_session_swap(&mut self, new_active: Option<SessionId>) -> Vec<BrainContinuation> {
        // Continuations don't currently carry their own SessionId; the
        // scheduler's `active_session` acts as the lane guard. On swap,
        // every currently-pending continuation becomes stale.
        let evicted: Vec<_> = self.pending_continuations.drain(..).collect();
        self.active_session = new_active;
        // Do NOT insert evicted ids into delivered_ids — they were dropped,
        // not delivered; future re-push of the same id under the new brain
        // should still be accepted if semantically valid.
        evicted
    }

    #[cfg(test)]
    pub(crate) fn pending_user_len(&self) -> usize { self.pending_user.len() }
    #[cfg(test)]
    pub(crate) fn pending_continuation_len(&self) -> usize { self.pending_continuations.len() }
}

/// RAII guard: sets `turn_in_flight = true` on `arm`, clears on `Drop`.
/// Task 8 uses this to guarantee `note_turn_finished()` is called on
/// every exit path from the streaming loop (normal, cancel, error).
///
/// **Safety note:** `std::mem::forget(guard)` defeats the guard and leaves
/// `turn_in_flight == true` permanently, effectively freezing the scheduler
/// in Idle forever. This is a known Rust RAII limitation — do not `forget`
/// a TurnGuard.
#[must_use = "TurnGuard must be bound to a variable; an unbound guard drops immediately and returns turn_in_flight to false"]
pub struct TurnGuard<'a> {
    sched: &'a mut BrainScheduler,
}

impl<'a> TurnGuard<'a> {
    pub fn arm(sched: &'a mut BrainScheduler) -> Self {
        sched.note_turn_started();
        Self { sched }
    }
}

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        self.sched.note_turn_finished();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::domain::{BrainContinuation, ContinuationPayload, ContinuationSource};
    use spur_acp::domain::delegation::DelegationStatus;
    use spur_acp::types::SessionId;
    use std::time::Instant;

    fn mk_cont(id: &str) -> BrainContinuation {
        BrainContinuation {
            delegation_id: id.into(),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: None, diff_summary: None, worker_branch: None,
            },
            created_at: Instant::now(),
        }
    }

    #[test]
    fn new_scheduler_is_empty() {
        let s = BrainScheduler::new(Some(SessionId::new()));
        assert_eq!(s.pending_user_len(), 0);
        assert_eq!(s.pending_continuation_len(), 0);
    }

    #[test]
    fn push_continuation_dedups_by_delegation_id() {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        s.push_continuation(mk_cont("id-1"));
        s.push_continuation(mk_cont("id-1"));           // duplicate — no-op
        assert_eq!(s.pending_continuation_len(), 1);
        s.push_continuation(mk_cont("id-2"));
        assert_eq!(s.pending_continuation_len(), 2);
    }

    #[test]
    fn next_is_idle_when_everything_empty() {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        assert!(matches!(s.next(Instant::now()), ScheduledAction::Idle));
    }

    #[test]
    fn next_returns_user_prompt_when_user_queued_and_idle() {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        s.push_user(InteractiveInput::Message { blocks: vec![], interrupt: false });
        assert!(matches!(s.next(Instant::now()), ScheduledAction::UserPrompt(_)));
    }

    #[test]
    fn next_returns_idle_while_turn_in_flight_even_if_user_queued() {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        s.push_user(InteractiveInput::Message { blocks: vec![], interrupt: false });
        s.note_turn_started();
        assert!(matches!(s.next(Instant::now()), ScheduledAction::Idle));
    }

    #[test]
    fn next_fires_continuation_only_when_idle_and_no_user_pending() {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        s.push_continuation(mk_cont("id-1"));
        match s.next(Instant::now()) {
            ScheduledAction::ContinuationPrompt(cs) => assert_eq!(cs.len(), 1),
            other => panic!("expected ContinuationPrompt, got {:?}", other),
        }
    }

    #[test]
    fn next_user_beats_continuation_when_both_queued() {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        s.push_continuation(mk_cont("id-1"));
        s.push_user(InteractiveInput::Message { blocks: vec![], interrupt: false });
        match s.next(Instant::now()) {
            ScheduledAction::MergedPrompt { continuations, .. } => {
                assert_eq!(continuations.len(), 1);
            }
            other => panic!("expected MergedPrompt, got {:?}", other),
        }
    }

    #[test]
    fn next_suppresses_continuation_during_cancel_grace() {
        let now = Instant::now();
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        s.push_continuation(mk_cont("id-1"));
        s.note_cancel_resolved(now);
        // Inside the grace window: Idle.
        assert!(matches!(s.next(now + std::time::Duration::from_millis(100)), ScheduledAction::Idle));
        // After grace: fires.
        assert!(matches!(
            s.next(now + std::time::Duration::from_millis(2000)),
            ScheduledAction::ContinuationPrompt(_)
        ));
    }

    #[test]
    fn next_coalesces_multiple_continuations_fifo() {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        s.push_continuation(mk_cont("id-1"));
        s.push_continuation(mk_cont("id-2"));
        s.push_continuation(mk_cont("id-3"));
        match s.next(Instant::now()) {
            ScheduledAction::ContinuationPrompt(cs) => {
                assert_eq!(cs.len(), 3);
                assert_eq!(cs[0].delegation_id, "id-1");
                assert_eq!(cs[2].delegation_id, "id-3");
            }
            _ => panic!("expected ContinuationPrompt"),
        }
    }

    #[test]
    fn grace_cleared_when_user_arrives_during_in_flight_turn() {
        let now = Instant::now();
        let mut s = BrainScheduler::new(Some(SessionId::new()));

        s.push_continuation(mk_cont("id-1"));
        s.note_turn_started();              // in-flight
        s.note_cancel_resolved(now);        // grace armed
        s.push_user(InteractiveInput::Message { blocks: vec![], interrupt: false });

        // In-flight => Idle, but grace is cleared as a side effect.
        assert!(matches!(s.next(now + std::time::Duration::from_millis(100)), ScheduledAction::Idle));

        s.note_turn_finished();

        // Grace already cleared: user wins with merged continuation.
        match s.next(now + std::time::Duration::from_millis(200)) {
            ScheduledAction::MergedPrompt { continuations, .. } => {
                assert_eq!(continuations.len(), 1);
            }
            other => panic!("expected MergedPrompt, got {:?}", other),
        }
    }

    #[test]
    fn session_swap_evicts_stale_continuations_and_returns_them() {
        let sid_a = SessionId::new();
        let sid_b = SessionId::new();
        let mut s = BrainScheduler::new(Some(sid_a.clone()));
        s.push_continuation(mk_cont("id-1"));
        s.push_continuation(mk_cont("id-2"));

        let evicted = s.note_session_swap(Some(sid_b));
        assert_eq!(evicted.len(), 2);
        assert_eq!(s.pending_continuation_len(), 0);
    }

    #[test]
    fn push_continuation_after_delivery_is_noop() {
        let mut s = BrainScheduler::new(Some(SessionId::new()));
        s.push_continuation(mk_cont("id-1"));

        // Drain via next().
        match s.next(Instant::now()) {
            ScheduledAction::ContinuationPrompt(cs) => assert_eq!(cs.len(), 1),
            _ => panic!("expected ContinuationPrompt"),
        }

        // Re-push the same id — should be a no-op because delivered_ids remembers it.
        s.push_continuation(mk_cont("id-1"));
        assert_eq!(s.pending_continuation_len(), 0);
        assert!(matches!(s.next(Instant::now()), ScheduledAction::Idle));
    }
}
