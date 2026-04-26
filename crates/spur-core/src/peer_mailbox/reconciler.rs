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
    pub inflight_stranded: u32,
    /// Deprecated under post-bd-cpf.3 logic; retained for event wire
    /// compatibility and always emitted as 0.
    pub inflight_reverted_to_queued: u32,
    pub guards_re_wrapped: u32,
}

pub async fn run_startup_reconcile(
    ledger: Arc<dyn PeerMailboxLedger>,
    funnel: FunnelHandle,
    brain_session_id: String,
    drain_quiet_window: Duration,
) -> ReconcileCounts {
    // TODO(peer-mailbox): drain_quiet_window is currently unused;
    // run_startup_reconcile forces terminal transitions immediately
    // on boot. Future work: respect the quiet window so an orchestrator
    // restarting mid-flight gives in-flight workers a grace period
    // before forcing them to terminal. Tracked separately.
    let _ = drain_quiet_window;
    let entries = ledger.non_terminal_entries().await;
    let mut counts = ReconcileCounts::default();

    for entry in entries {
        match entry.state {
            LedgerState::DeliveredInflight => {
                if entry.injected_into_prompts.is_empty() {
                    tracing::debug!(
                        message_id = ?entry.envelope.message_id,
                        "reconciler: DeliveredInflight without injection records - emitting Stranded, not transitioning"
                    );
                    funnel.emit(SpurEventBody::WorkerPeerMessageReconciledStranded {
                        brain_session_id: brain_session_id.clone(),
                        message_id: entry.envelope.message_id,
                        target_delegation_id: entry.envelope.target_delegation_id.clone(),
                        state: entry.state,
                        reason: "delivered_inflight_without_injection_records".into(),
                    });
                    counts.inflight_stranded += 1;
                    continue;
                }

                match ledger
                    .transition(&entry.envelope.message_id, LedgerState::Delivered)
                    .await
                {
                    Ok(_) => {
                        counts.inflight_forced_to_delivered += 1;
                        let target_prompt_id = entry
                            .injected_into_prompts
                            .iter()
                            .min()
                            .cloned()
                            .unwrap_or_default();
                        funnel.emit(SpurEventBody::WorkerPeerMessageDelivered {
                            brain_session_id: brain_session_id.clone(),
                            message_id: entry.envelope.message_id,
                            target_delegation_id: entry.envelope.target_delegation_id.clone(),
                            target_prompt_id,
                            injected_chars: 0,
                        });
                    }
                    Err(err) => {
                        tracing::warn!(
                            message_id = ?entry.envelope.message_id,
                            from = ?entry.state,
                            to = ?LedgerState::Delivered,
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
        inflight_stranded: counts.inflight_stranded,
        inflight_reverted_to_queued: 0,
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

        let emitted = [
            events.recv().await.expect("delivered event"),
            events.recv().await.expect("reconciled event"),
        ];
        assert!(emitted.iter().any(|event| matches!(
            event,
            SpurEventBody::WorkerPeerMessageDelivered {
                message_id,
                ..
            } if *message_id == env.message_id
        )));
        assert!(emitted.iter().any(|event| matches!(
            event,
            SpurEventBody::WorkerPeerMailboxReconciled {
                brain_session_id,
                inflight_forced_to_delivered: 1,
                inflight_reverted_to_queued: 0,
                ..
            } if brain_session_id == "brain-session"
        )));
    }

    #[tokio::test]
    async fn reconcile_emits_stranded_event_for_inflight_without_injections() {
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

        assert_eq!(counts.inflight_stranded, 1);
        assert_eq!(counts.inflight_reverted_to_queued, 0);
        let entry = ledger.get(&env.message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::DeliveredInflight);

        let event = events.recv().await.expect("stranded event");
        assert!(matches!(
            event,
            SpurEventBody::WorkerPeerMessageReconciledStranded {
                brain_session_id,
                message_id,
                target_delegation_id,
                state: LedgerState::DeliveredInflight,
                reason,
            } if brain_session_id == "brain-session"
                && message_id == env.message_id
                && target_delegation_id == env.target_delegation_id
                && reason == "delivered_inflight_without_injection_records"
        ));

        let event = events.recv().await.expect("reconciled event");
        assert!(matches!(
            event,
            SpurEventBody::WorkerPeerMailboxReconciled {
                brain_session_id,
                inflight_forced_to_delivered: 0,
                inflight_stranded: 1,
                inflight_reverted_to_queued: 0,
                ..
            } if brain_session_id == "brain-session"
        ));
    }

    #[tokio::test]
    async fn reconcile_handles_mixed_non_terminal_states_in_single_pass() {
        let ledger = Arc::new(InMemoryLedger::new());
        let injected = envelope();
        let uninjected = envelope();
        let accepted = envelope();

        ledger.accept(injected.clone()).await.unwrap();
        ledger
            .record_injection(&injected.message_id, "target-prompt")
            .await
            .unwrap();
        ledger
            .transition(&injected.message_id, LedgerState::Queued)
            .await
            .unwrap();
        ledger
            .transition(&injected.message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();

        ledger.accept(uninjected.clone()).await.unwrap();
        ledger
            .transition(&uninjected.message_id, LedgerState::Queued)
            .await
            .unwrap();
        ledger
            .transition(&uninjected.message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();

        ledger.accept(accepted.clone()).await.unwrap();

        let (funnel, mut events) = crate::event_funnel::test_channel();

        let counts = run_startup_reconcile(
            ledger.clone(),
            funnel,
            "brain-session".into(),
            Duration::from_millis(100),
        )
        .await;

        assert_eq!(counts.inflight_forced_to_delivered, 1);
        assert_eq!(counts.inflight_stranded, 1);
        assert_eq!(counts.inflight_reverted_to_queued, 0);
        assert_eq!(counts.guards_re_wrapped, 1);

        let injected_entry = ledger.get(&injected.message_id).await.unwrap();
        assert_eq!(injected_entry.state, LedgerState::Delivered);
        let uninjected_entry = ledger.get(&uninjected.message_id).await.unwrap();
        assert_eq!(uninjected_entry.state, LedgerState::DeliveredInflight);
        let accepted_entry = ledger.get(&accepted.message_id).await.unwrap();
        assert_eq!(accepted_entry.state, LedgerState::Accepted);

        let emitted = [
            events.recv().await.expect("delivered event"),
            events.recv().await.expect("stranded event"),
            events.recv().await.expect("reconciled event"),
        ];
        assert!(emitted.iter().any(|event| matches!(
            event,
            SpurEventBody::WorkerPeerMessageDelivered {
                message_id,
                ..
            } if *message_id == injected.message_id
        )));
        assert!(emitted.iter().any(|event| matches!(
            event,
            SpurEventBody::WorkerPeerMessageReconciledStranded {
                brain_session_id,
                message_id,
                state: LedgerState::DeliveredInflight,
                reason,
                ..
            } if brain_session_id == "brain-session"
                && *message_id == uninjected.message_id
                && reason == "delivered_inflight_without_injection_records"
        )));

        assert!(emitted.iter().any(|event| matches!(
            event,
            SpurEventBody::WorkerPeerMailboxReconciled {
                brain_session_id,
                audit_failed_emitted: 0,
                inflight_forced_to_delivered: 1,
                inflight_stranded: 1,
                inflight_reverted_to_queued: 0,
                guards_re_wrapped: 1,
            } if brain_session_id == "brain-session"
        )));
        assert!(events.try_recv().is_err());
    }
}
