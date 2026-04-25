use async_trait::async_trait;
use spur_acp::domain::peer_message::{LedgerState, PeerMessageEnvelope, PeerMessageId};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("ledger entry {0:?} not found")]
    NotFound(PeerMessageId),
    #[error("invalid transition from {from:?} to {to:?}")]
    InvalidTransition { from: LedgerState, to: LedgerState },
    #[error("entry {id:?} already in terminal state {state:?}")]
    AlreadyTerminal {
        id: PeerMessageId,
        state: LedgerState,
    },
}

/// Outcome of `accept()`. Lets callers distinguish the first acceptance
/// (which warrants emitting `WorkerPeerMessageAccepted`) from a replay
/// re-acceptance (which must remain a no-op) without consulting state
/// out-of-band.
#[derive(Debug, PartialEq, Eq)]
pub enum AcceptOutcome {
    Created,
    AlreadyAccepted,
}

/// Outcome of `transition()`. `Changed` means the state moved; `Unchanged`
/// means the requested state matched current state (a replay no-op). The
/// Router uses this to decide whether to emit a fresh audit event.
#[derive(Debug, PartialEq, Eq)]
pub enum TransitionOutcome {
    Changed { from: LedgerState, to: LedgerState },
    Unchanged(LedgerState),
}

/// Outcome of `record_injection()`. Replaces the previous `bool` return so
/// call sites are explicit about idempotent replay vs first-time injection.
#[derive(Debug, PartialEq, Eq)]
pub enum InjectionOutcome {
    Injected,
    AlreadyInjected,
}

#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub envelope: PeerMessageEnvelope,
    pub state: LedgerState,
    /// Set of `target_prompt_id`s into which this message has been recorded
    /// as injected. Used for at-most-once injection per prompt.
    pub injected_into_prompts: HashSet<String>,
}

fn is_terminal(state: LedgerState) -> bool {
    matches!(
        state,
        LedgerState::Rejected
            | LedgerState::Consumed
            | LedgerState::Ignored
            | LedgerState::Expired
            | LedgerState::Dropped
            | LedgerState::Undeliverable
    )
}

fn is_valid_transition(from: LedgerState, to: LedgerState) -> bool {
    if from == to {
        return true;
    }

    // Unknown is a forward-compatibility state, not part of the canonical
    // ledger matrix. Transitions involving it intentionally fall through as
    // invalid.
    matches!(
        (from, to),
        (
            LedgerState::Accepted,
            LedgerState::Queued | LedgerState::DeliveredInflight | LedgerState::Undeliverable
        ) | (
            LedgerState::Queued,
            LedgerState::DeliveredInflight
                | LedgerState::Expired
                | LedgerState::Dropped
                | LedgerState::Undeliverable
        ) | (
            LedgerState::DeliveredInflight,
            LedgerState::Queued | LedgerState::Delivered
        ) | (
            LedgerState::Delivered,
            LedgerState::Consumed
                | LedgerState::Ignored
                | LedgerState::Expired
                | LedgerState::Dropped
        )
    )
}

#[async_trait]
pub trait PeerMailboxLedger: Send + Sync {
    async fn accept(&self, envelope: PeerMessageEnvelope) -> Result<AcceptOutcome, LedgerError>;
    async fn transition(
        &self,
        message_id: &PeerMessageId,
        next: LedgerState,
    ) -> Result<TransitionOutcome, LedgerError>;
    async fn record_injection(
        &self,
        message_id: &PeerMessageId,
        target_prompt_id: &str,
    ) -> Result<InjectionOutcome, LedgerError>;
    async fn get(&self, message_id: &PeerMessageId) -> Option<LedgerEntry>;
    async fn pending_for_target(
        &self,
        target_delegation_id: &spur_acp::domain::delegation::DelegationId,
    ) -> Vec<LedgerEntry>;
    async fn non_terminal_entries(&self) -> Vec<LedgerEntry>;
}

#[derive(Default)]
pub struct InMemoryLedger {
    inner: Arc<Mutex<HashMap<PeerMessageId, LedgerEntry>>>,
}

impl InMemoryLedger {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl PeerMailboxLedger for InMemoryLedger {
    async fn accept(&self, envelope: PeerMessageEnvelope) -> Result<AcceptOutcome, LedgerError> {
        let mut g = self.inner.lock().await;
        let id = envelope.message_id;
        if let Some(existing) = g.get(&id) {
            // Replay-safe: idempotent on collision. Caller can choose to
            // log AlreadyAccepted but must not emit a duplicate audit
            // event. Terminal collisions surface as a distinct error so
            // the Router can classify them without inspecting state.
            if is_terminal(existing.state) {
                return Err(LedgerError::AlreadyTerminal {
                    id,
                    state: existing.state,
                });
            }
            return Ok(AcceptOutcome::AlreadyAccepted);
        }
        g.insert(
            id,
            LedgerEntry {
                envelope,
                state: LedgerState::Accepted,
                injected_into_prompts: HashSet::new(),
            },
        );
        Ok(AcceptOutcome::Created)
    }

    async fn transition(
        &self,
        message_id: &PeerMessageId,
        next: LedgerState,
    ) -> Result<TransitionOutcome, LedgerError> {
        let mut g = self.inner.lock().await;
        let entry = g
            .get_mut(message_id)
            .ok_or(LedgerError::NotFound(*message_id))?;
        // Idempotency: same-state transitions are observable no-ops.
        if entry.state == next {
            return Ok(TransitionOutcome::Unchanged(next));
        }
        if !is_valid_transition(entry.state, next) {
            return Err(LedgerError::InvalidTransition {
                from: entry.state,
                to: next,
            });
        }
        let from = entry.state;
        entry.state = next;
        Ok(TransitionOutcome::Changed { from, to: next })
    }

    async fn record_injection(
        &self,
        message_id: &PeerMessageId,
        target_prompt_id: &str,
    ) -> Result<InjectionOutcome, LedgerError> {
        let mut g = self.inner.lock().await;
        let entry = g
            .get_mut(message_id)
            .ok_or(LedgerError::NotFound(*message_id))?;
        if entry.injected_into_prompts.insert(target_prompt_id.into()) {
            Ok(InjectionOutcome::Injected)
        } else {
            Ok(InjectionOutcome::AlreadyInjected)
        }
    }

    async fn get(&self, message_id: &PeerMessageId) -> Option<LedgerEntry> {
        self.inner.lock().await.get(message_id).cloned()
    }

    async fn pending_for_target(
        &self,
        target_delegation_id: &spur_acp::domain::delegation::DelegationId,
    ) -> Vec<LedgerEntry> {
        self.inner
            .lock()
            .await
            .values()
            .filter(|e| {
                &e.envelope.target_delegation_id == target_delegation_id
                    && matches!(e.state, LedgerState::Accepted | LedgerState::Queued)
            })
            .cloned()
            .collect()
    }

    async fn non_terminal_entries(&self) -> Vec<LedgerEntry> {
        self.inner
            .lock()
            .await
            .values()
            .filter(|e| {
                !matches!(
                    e.state,
                    LedgerState::Rejected
                        | LedgerState::Consumed
                        | LedgerState::Ignored
                        | LedgerState::Expired
                        | LedgerState::Dropped
                        | LedgerState::Undeliverable
                )
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::domain::delegation::DelegationId;
    use spur_acp::domain::peer_message::{
        LedgerState, MessageKind, PeerMessageEnvelope, PeerMessageId,
    };

    fn envelope(msg: &str) -> PeerMessageEnvelope {
        PeerMessageEnvelope {
            schema: "spur-peer-message/v1".into(),
            message_id: serde_json::from_str("\"00000000-0000-0000-0000-000000000001\"").unwrap(),
            source_delegation_id: DelegationId("src".into()),
            target_delegation_id: DelegationId("tgt".into()),
            source_issue_id: "i1".into(),
            target_issue_id: "i2".into(),
            source_plan_task_id: "ta".into(),
            target_plan_task_id: "tb".into(),
            source_executor_id: "ex".into(),
            plan_version: 1,
            kind: MessageKind::Question,
            body: msg.into(),
            sequence: 1,
        }
    }

    async fn transition_to_consumed(ledger: &InMemoryLedger, id: &PeerMessageId) {
        ledger.transition(id, LedgerState::Queued).await.unwrap();
        ledger
            .transition(id, LedgerState::DeliveredInflight)
            .await
            .unwrap();
        ledger.transition(id, LedgerState::Delivered).await.unwrap();
        ledger.transition(id, LedgerState::Consumed).await.unwrap();
    }

    #[tokio::test]
    async fn accept_distinguishes_created_from_already_accepted() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        let first = ledger.accept(env.clone()).await.unwrap();
        let second = ledger.accept(env.clone()).await.unwrap();
        assert_eq!(first, AcceptOutcome::Created);
        assert_eq!(second, AcceptOutcome::AlreadyAccepted);
        let entry = ledger.get(&env.message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::Accepted);
    }

    #[tokio::test]
    async fn accept_after_terminal_returns_already_terminal_error() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();
        transition_to_consumed(&ledger, &env.message_id).await;
        let err = ledger.accept(env.clone()).await.unwrap_err();
        assert!(matches!(
            err,
            LedgerError::AlreadyTerminal {
                state: LedgerState::Consumed,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn transition_reports_changed_vs_unchanged() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();
        let first = ledger
            .transition(&env.message_id, LedgerState::Queued)
            .await
            .unwrap();
        let second = ledger
            .transition(&env.message_id, LedgerState::Queued)
            .await
            .unwrap();
        assert!(matches!(first, TransitionOutcome::Changed { .. }));
        assert!(matches!(second, TransitionOutcome::Unchanged(_)));
    }

    #[tokio::test]
    async fn transition_accepted_to_queued_is_valid() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();

        let outcome = ledger
            .transition(&env.message_id, LedgerState::Queued)
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            TransitionOutcome::Changed {
                from: LedgerState::Accepted,
                to: LedgerState::Queued
            }
        ));
    }

    #[tokio::test]
    async fn transition_accepted_to_consumed_is_invalid() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();

        let err = ledger
            .transition(&env.message_id, LedgerState::Consumed)
            .await
            .unwrap_err();

        assert!(matches!(err, LedgerError::InvalidTransition { .. }));
    }

    #[tokio::test]
    async fn transition_queued_to_ignored_is_invalid() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();
        ledger
            .transition(&env.message_id, LedgerState::Queued)
            .await
            .unwrap();

        let err = ledger
            .transition(&env.message_id, LedgerState::Ignored)
            .await
            .unwrap_err();

        assert!(matches!(err, LedgerError::InvalidTransition { .. }));
    }

    #[tokio::test]
    async fn transition_delivered_to_accepted_is_invalid() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();
        ledger
            .transition(&env.message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();
        ledger
            .transition(&env.message_id, LedgerState::Delivered)
            .await
            .unwrap();

        let err = ledger
            .transition(&env.message_id, LedgerState::Accepted)
            .await
            .unwrap_err();

        assert!(matches!(err, LedgerError::InvalidTransition { .. }));
    }

    #[tokio::test]
    async fn transition_delivered_inflight_to_queued_is_valid() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();
        ledger
            .transition(&env.message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();

        let outcome = ledger
            .transition(&env.message_id, LedgerState::Queued)
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            TransitionOutcome::Changed {
                from: LedgerState::DeliveredInflight,
                to: LedgerState::Queued
            }
        ));
    }

    #[tokio::test]
    async fn transition_delivered_inflight_to_delivered_is_valid() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();
        ledger
            .transition(&env.message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();

        let outcome = ledger
            .transition(&env.message_id, LedgerState::Delivered)
            .await
            .unwrap();

        assert!(matches!(
            outcome,
            TransitionOutcome::Changed {
                from: LedgerState::DeliveredInflight,
                to: LedgerState::Delivered
            }
        ));
    }

    #[tokio::test]
    async fn transition_terminal_state_rejects_any_outgoing() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();
        transition_to_consumed(&ledger, &env.message_id).await;

        let err = ledger
            .transition(&env.message_id, LedgerState::Queued)
            .await
            .unwrap_err();

        assert!(matches!(err, LedgerError::InvalidTransition { .. }));
    }

    #[tokio::test]
    async fn transition_accepted_to_rejected_is_invalid() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();

        let err = ledger
            .transition(&env.message_id, LedgerState::Rejected)
            .await
            .unwrap_err();

        assert!(matches!(err, LedgerError::InvalidTransition { .. }));
    }

    #[tokio::test]
    async fn transition_from_terminal_state_is_invalid() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();
        transition_to_consumed(&ledger, &env.message_id).await;
        let err = ledger
            .transition(&env.message_id, LedgerState::Queued)
            .await
            .unwrap_err();
        assert!(matches!(err, LedgerError::InvalidTransition { .. }));
    }

    #[tokio::test]
    async fn transition_on_missing_id_returns_not_found() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        let err = ledger
            .transition(&env.message_id, LedgerState::Queued)
            .await
            .unwrap_err();
        assert!(matches!(err, LedgerError::NotFound(_)));
    }

    #[tokio::test]
    async fn record_injection_returns_typed_outcome() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();
        let first = ledger
            .record_injection(&env.message_id, "prompt-1")
            .await
            .unwrap();
        let second = ledger
            .record_injection(&env.message_id, "prompt-1")
            .await
            .unwrap();
        assert_eq!(first, InjectionOutcome::Injected);
        assert_eq!(second, InjectionOutcome::AlreadyInjected);
    }

    #[tokio::test]
    async fn record_injection_on_missing_id_returns_not_found() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        let err = ledger
            .record_injection(&env.message_id, "prompt-1")
            .await
            .unwrap_err();
        assert!(matches!(err, LedgerError::NotFound(_)));
    }

    #[tokio::test]
    async fn pending_for_target_excludes_terminal_states() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();
        assert_eq!(
            ledger
                .pending_for_target(&env.target_delegation_id)
                .await
                .len(),
            1
        );
        transition_to_consumed(&ledger, &env.message_id).await;
        assert_eq!(
            ledger
                .pending_for_target(&env.target_delegation_id)
                .await
                .len(),
            0
        );
    }
}
