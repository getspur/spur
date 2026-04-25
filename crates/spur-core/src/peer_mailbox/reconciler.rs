use crate::event_funnel::FunnelHandle;
use crate::peer_mailbox::ledger::PeerMailboxLedger;
use spur_acp::domain::events::SpurEventBody;
use spur_acp::domain::peer_message::LedgerState;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileCounts {
    pub audit_failed_emitted: u32,
    pub inflight_forced_to_delivered: u32,
    pub inflight_reverted_to_queued: u32,
    pub guards_re_wrapped: u32,
}

pub async fn run_startup_reconcile(
    ledger: Arc<dyn PeerMailboxLedger>,
    funnel: FunnelHandle,
    brain_session_id: String,
    drain_quiet_window: Duration,
) -> ReconcileCounts {
    let _ = drain_quiet_window;
    let entries = ledger.non_terminal_entries().await;
    let mut counts = ReconcileCounts::default();

    for entry in entries {
        match entry.state {
            LedgerState::DeliveredInflight => {
                let next = if entry.injected_into_prompts.is_empty() {
                    LedgerState::Queued
                } else {
                    LedgerState::Delivered
                };

                match ledger.transition(&entry.envelope.message_id, next).await {
                    Ok(_) if next == LedgerState::Delivered => {
                        counts.inflight_forced_to_delivered += 1;
                    }
                    Ok(_) => {
                        counts.inflight_reverted_to_queued += 1;
                    }
                    Err(err) => {
                        tracing::warn!(
                            message_id = ?entry.envelope.message_id,
                            from = ?entry.state,
                            to = ?next,
                            ?err,
                            "peer mailbox startup reconcile transition failed"
                        );
                    }
                }
            }
            LedgerState::Accepted | LedgerState::Queued => {
                counts.guards_re_wrapped += 1;
            }
            _ => {}
        }
    }

    funnel.emit(SpurEventBody::WorkerPeerMailboxReconciled {
        brain_session_id,
        audit_failed_emitted: counts.audit_failed_emitted,
        inflight_forced_to_delivered: counts.inflight_forced_to_delivered,
        inflight_reverted_to_queued: counts.inflight_reverted_to_queued,
        guards_re_wrapped: counts.guards_re_wrapped,
    });

    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_mailbox::ledger::{InMemoryLedger, PeerMailboxLedger};
    use spur_acp::domain::delegation::DelegationId;
    use spur_acp::domain::events::SpurEventBody;
    use spur_acp::domain::peer_message::{
        LedgerState, MessageKind, PeerMessageEnvelope, PeerMessageId,
    };
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

    #[tokio::test]
    async fn reconcile_forces_inflight_with_injection_to_delivered() {
        let ledger = Arc::new(InMemoryLedger::new());
        let env = envelope();
        ledger.accept(env.clone()).await.unwrap();
        ledger
            .record_injection(&env.message_id, "target-prompt")
            .await
            .unwrap();
        ledger
            .transition(&env.message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();
        let (funnel, mut events) = crate::event_funnel::test_channel();

        let counts = run_startup_reconcile(
            ledger.clone(),
            funnel,
            "brain-session".into(),
            Duration::from_millis(100),
        )
        .await;

        assert_eq!(counts.inflight_forced_to_delivered, 1);
        let entry = ledger.get(&env.message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::Delivered);

        let event = events.try_recv().expect("reconciled event");
        assert!(matches!(
            event,
            SpurEventBody::WorkerPeerMailboxReconciled {
                brain_session_id,
                inflight_forced_to_delivered: 1,
                inflight_reverted_to_queued: 0,
                ..
            } if brain_session_id == "brain-session"
        ));
    }

    #[tokio::test]
    async fn reconcile_reverts_inflight_without_injection_to_queued() {
        let ledger = Arc::new(InMemoryLedger::new());
        let env = envelope();
        ledger.accept(env.clone()).await.unwrap();
        ledger
            .transition(&env.message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();
        let (funnel, mut events) = crate::event_funnel::test_channel();

        let counts = run_startup_reconcile(
            ledger.clone(),
            funnel,
            "brain-session".into(),
            Duration::from_millis(100),
        )
        .await;

        assert_eq!(counts.inflight_reverted_to_queued, 1);
        let entry = ledger.get(&env.message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::Queued);

        let event = events.try_recv().expect("reconciled event");
        assert!(matches!(
            event,
            SpurEventBody::WorkerPeerMailboxReconciled {
                brain_session_id,
                inflight_forced_to_delivered: 0,
                inflight_reverted_to_queued: 1,
                ..
            } if brain_session_id == "brain-session"
        ));
    }
}
