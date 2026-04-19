//! Brain turn scheduler — see
//! `docs/superpowers/specs/2026-04-19-brain-async-continuation-design.md`.
//!
//! Pure-sync policy. No tokio primitives; unit-testable without a runtime.

use spur_acp::domain::BrainContinuation;
use spur_acp::types::SessionId;
use std::collections::{HashSet, VecDeque};
use std::time::Instant;

use crate::orchestrator::InteractiveInput;

/// Owns the split-lane queues and scheduling policy for brain turns.
pub struct BrainScheduler {
    pending_user:          VecDeque<InteractiveInput>,
    pending_continuations: VecDeque<BrainContinuation>,
    delivered_ids:         HashSet<String>,
    active_session:        Option<SessionId>,
    turn_in_flight:        bool,
    cancel_grace_until:    Option<Instant>,
}

impl BrainScheduler {
    pub fn new(active_session: Option<SessionId>) -> Self {
        Self {
            pending_user: VecDeque::new(),
            pending_continuations: VecDeque::new(),
            delivered_ids: HashSet::new(),
            active_session,
            turn_in_flight: false,
            cancel_grace_until: None,
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

    #[cfg(test)]
    pub(crate) fn pending_user_len(&self) -> usize { self.pending_user.len() }
    #[cfg(test)]
    pub(crate) fn pending_continuation_len(&self) -> usize { self.pending_continuations.len() }
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
}
