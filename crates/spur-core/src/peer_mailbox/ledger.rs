use async_trait::async_trait;
use spur_acp::domain::peer_message::{LedgerState, PeerMessageEnvelope, PeerMessageId};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("invalid transition from {from:?} to {to:?}")]
    InvalidTransition { from: LedgerState, to: LedgerState },
}

#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub envelope: PeerMessageEnvelope,
    pub state: LedgerState,
    /// Set of `target_prompt_id`s into which this message has been recorded
    /// as injected. Used for at-most-once injection per prompt.
    pub injected_into_prompts: HashSet<String>,
}

#[async_trait]
pub trait PeerMailboxLedger: Send + Sync {
    async fn accept(&self, envelope: PeerMessageEnvelope) -> Result<(), LedgerError>;
    async fn transition(
        &self,
        message_id: &PeerMessageId,
        next: LedgerState,
    ) -> Result<LedgerState, LedgerError>;
    async fn record_injection(
        &self,
        message_id: &PeerMessageId,
        target_prompt_id: &str,
    ) -> Result<bool, LedgerError>;
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
    async fn accept(&self, envelope: PeerMessageEnvelope) -> Result<(), LedgerError> {
        let mut g = self.inner.lock().await;
        let id = envelope.message_id.clone();
        // Idempotent: if already accepted (or further along), no-op.
        if g.contains_key(&id) {
            return Ok(());
        }
        g.insert(
            id,
            LedgerEntry {
                envelope,
                state: LedgerState::Accepted,
                injected_into_prompts: HashSet::new(),
            },
        );
        Ok(())
    }

    async fn transition(
        &self,
        message_id: &PeerMessageId,
        next: LedgerState,
    ) -> Result<LedgerState, LedgerError> {
        let mut g = self.inner.lock().await;
        let entry = g
            .get_mut(message_id)
            .ok_or(LedgerError::InvalidTransition {
                from: LedgerState::Rejected,
                to: next,
            })?;
        // Idempotency: same-state transitions are no-ops.
        if entry.state == next {
            return Ok(next);
        }
        entry.state = next;
        Ok(next)
    }

    async fn record_injection(
        &self,
        message_id: &PeerMessageId,
        target_prompt_id: &str,
    ) -> Result<bool, LedgerError> {
        let mut g = self.inner.lock().await;
        if let Some(entry) = g.get_mut(message_id) {
            Ok(entry.injected_into_prompts.insert(target_prompt_id.into()))
        } else {
            Err(LedgerError::InvalidTransition {
                from: LedgerState::Rejected,
                to: LedgerState::DeliveredInflight,
            })
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
    use spur_acp::domain::peer_message::{LedgerState, MessageKind, PeerMessageEnvelope};

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

    #[tokio::test]
    async fn accept_is_idempotent() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();
        ledger.accept(env.clone()).await.unwrap();
        let entry = ledger.get(&env.message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::Accepted);
    }

    #[tokio::test]
    async fn record_injection_returns_false_on_duplicate() {
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
        assert!(first);
        assert!(!second);
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
        ledger
            .transition(&env.message_id, LedgerState::Consumed)
            .await
            .unwrap();
        assert_eq!(
            ledger
                .pending_for_target(&env.target_delegation_id)
                .await
                .len(),
            0
        );
    }
}
