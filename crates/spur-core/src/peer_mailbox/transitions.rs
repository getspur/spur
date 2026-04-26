use crate::event_funnel::FunnelHandle;
use crate::peer_mailbox::ledger::{is_terminal, LedgerError, PeerMailboxLedger, TransitionOutcome};
use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::peer_message::{LedgerState, PeerMessageId};
use spur_acp::{BrainSessionId, SpurEventBody};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerTransitionKind {
    DeliveredInflight,
    Delivered,
}

impl PeerTransitionKind {
    pub fn as_audit_str(&self) -> &'static str {
        match self {
            Self::DeliveredInflight => "delivered_inflight",
            Self::Delivered => "delivered",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TransitionAuditOutcome {
    Changed,
    Unchanged(LedgerState),
    TerminalSkip(LedgerState),
    /// Helper has already emitted `WorkerPeerMessageAuditFailed`. The carried
    /// `String` is the underlying `LedgerError`'s `to_string()` so the caller
    /// can preserve the `?err` field on its `tracing::warn!` line.
    AuditFailed(String),
}

pub async fn transition_with_audit(
    ledger: &dyn PeerMailboxLedger,
    funnel: &FunnelHandle,
    brain_session_id: &BrainSessionId,
    target_delegation_id: &DelegationId,
    message_id: PeerMessageId,
    target_state: LedgerState,
    transition_kind: PeerTransitionKind,
) -> TransitionAuditOutcome {
    match ledger.transition(&message_id, target_state).await {
        Ok(TransitionOutcome::Changed { .. }) => TransitionAuditOutcome::Changed,
        Ok(TransitionOutcome::Unchanged(state)) => TransitionAuditOutcome::Unchanged(state),
        Err(LedgerError::InvalidTransition { from, .. }) if is_terminal(from) => {
            TransitionAuditOutcome::TerminalSkip(from)
        }
        Err(err) => {
            let error = err.to_string();
            funnel.emit(SpurEventBody::WorkerPeerMessageAuditFailed {
                brain_session_id: brain_session_id.to_string(),
                message_id,
                target_delegation_id: target_delegation_id.clone(),
                transition_kind: transition_kind.as_audit_str().to_string(),
                error: error.clone(),
            });
            TransitionAuditOutcome::AuditFailed(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::event_funnel::test_channel;
    use crate::peer_mailbox::ledger::{InMemoryLedger, PeerMailboxLedger};
    use spur_acp::domain::delegation::DelegationId;
    use spur_acp::domain::events::SpurEventBody;
    use spur_acp::domain::peer_message::{
        LedgerState, MessageKind, PeerMessageEnvelope, PeerMessageId,
    };
    use spur_acp::{BrainSessionId, SessionId};
    use tokio::sync::mpsc::UnboundedReceiver;
    use uuid::Uuid;

    fn envelope() -> PeerMessageEnvelope {
        PeerMessageEnvelope {
            schema: "spur-peer-message/v1".into(),
            message_id: PeerMessageId(Uuid::new_v4()),
            source_delegation_id: DelegationId("src".into()),
            target_delegation_id: DelegationId("tgt".into()),
            source_issue_id: "i1".into(),
            target_issue_id: "i2".into(),
            source_plan_task_id: "ta".into(),
            target_plan_task_id: "tb".into(),
            source_executor_id: "ex".into(),
            plan_version: 1,
            kind: MessageKind::Question,
            body: "hi".into(),
            sequence: 1,
        }
    }

    async fn drain_events(events: &mut UnboundedReceiver<SpurEventBody>) -> Vec<SpurEventBody> {
        let mut out = Vec::new();
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(20), events.recv()).await {
                Ok(Some(event)) => out.push(event),
                Ok(None) | Err(_) => break,
            }
        }
        out
    }

    fn has_audit_failed(events: &[SpurEventBody]) -> bool {
        events
            .iter()
            .any(|event| matches!(event, SpurEventBody::WorkerPeerMessageAuditFailed { .. }))
    }

    #[tokio::test]
    async fn transition_with_audit_returns_changed_on_normal_path() {
        let ledger = InMemoryLedger::new();
        let env = envelope();
        ledger.accept(env.clone()).await.unwrap();
        let (funnel, mut events) = test_channel();
        let brain_session_id = BrainSessionId::new(SessionId("bs".into()));

        let outcome = transition_with_audit(
            &ledger,
            &funnel,
            &brain_session_id,
            &env.target_delegation_id,
            env.message_id,
            LedgerState::DeliveredInflight,
            PeerTransitionKind::DeliveredInflight,
        )
        .await;

        assert!(matches!(outcome, TransitionAuditOutcome::Changed));
        assert!(!has_audit_failed(&drain_events(&mut events).await));
    }

    #[tokio::test]
    async fn transition_with_audit_returns_unchanged_on_idempotent_target() {
        let ledger = InMemoryLedger::new();
        let env = envelope();
        ledger.accept(env.clone()).await.unwrap();
        ledger
            .transition(&env.message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();
        let (funnel, mut events) = test_channel();
        let brain_session_id = BrainSessionId::new(SessionId("bs".into()));

        let outcome = transition_with_audit(
            &ledger,
            &funnel,
            &brain_session_id,
            &env.target_delegation_id,
            env.message_id,
            LedgerState::DeliveredInflight,
            PeerTransitionKind::DeliveredInflight,
        )
        .await;

        assert!(matches!(
            outcome,
            TransitionAuditOutcome::Unchanged(LedgerState::DeliveredInflight)
        ));
        assert!(!has_audit_failed(&drain_events(&mut events).await));
    }

    #[tokio::test]
    async fn transition_with_audit_returns_terminal_skip_when_already_terminal() {
        let ledger = InMemoryLedger::new();
        let env = envelope();
        ledger.accept(env.clone()).await.unwrap();
        ledger
            .transition(&env.message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();
        ledger
            .transition(&env.message_id, LedgerState::Delivered)
            .await
            .unwrap();
        ledger
            .transition(&env.message_id, LedgerState::Consumed)
            .await
            .unwrap();
        let (funnel, mut events) = test_channel();
        let brain_session_id = BrainSessionId::new(SessionId("bs".into()));

        let outcome = transition_with_audit(
            &ledger,
            &funnel,
            &brain_session_id,
            &env.target_delegation_id,
            env.message_id,
            LedgerState::Delivered,
            PeerTransitionKind::Delivered,
        )
        .await;

        assert!(matches!(
            outcome,
            TransitionAuditOutcome::TerminalSkip(LedgerState::Consumed)
        ));
        assert!(!has_audit_failed(&drain_events(&mut events).await));
    }

    #[tokio::test]
    async fn transition_with_audit_emits_audit_failed_on_invalid_non_terminal_transition() {
        let ledger = InMemoryLedger::new();
        let env = envelope();
        ledger.accept(env.clone()).await.unwrap();
        ledger
            .transition(&env.message_id, LedgerState::Queued)
            .await
            .unwrap();
        let (funnel, mut events) = test_channel();
        let brain_session_id = BrainSessionId::new(SessionId("bs".into()));

        let outcome = transition_with_audit(
            &ledger,
            &funnel,
            &brain_session_id,
            &env.target_delegation_id,
            env.message_id,
            LedgerState::Delivered,
            PeerTransitionKind::Delivered,
        )
        .await;

        let outcome_err = match outcome {
            TransitionAuditOutcome::AuditFailed(err) => err,
            other => panic!("expected AuditFailed, got {other:?}"),
        };
        assert!(
            !outcome_err.is_empty(),
            "AuditFailed should carry the LedgerError text for caller-side warn logging"
        );
        let audit_failed: Vec<_> = drain_events(&mut events)
            .await
            .into_iter()
            .filter_map(|event| match event {
                SpurEventBody::WorkerPeerMessageAuditFailed {
                    message_id,
                    target_delegation_id,
                    transition_kind,
                    error,
                    ..
                } => Some((message_id, target_delegation_id, transition_kind, error)),
                _ => None,
            })
            .collect();
        assert_eq!(audit_failed.len(), 1);
        let (event_message_id, event_target, event_kind, event_error) = &audit_failed[0];
        assert_eq!(event_message_id, &env.message_id);
        assert_eq!(event_target, &env.target_delegation_id);
        assert_eq!(event_kind, "delivered");
        assert!(
            !event_error.is_empty(),
            "WorkerPeerMessageAuditFailed.error must carry the LedgerError text"
        );
        assert_eq!(event_error, &outcome_err);
    }
}
