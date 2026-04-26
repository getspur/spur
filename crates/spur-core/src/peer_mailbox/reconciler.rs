use crate::event_funnel::FunnelHandle;
use crate::peer_mailbox::ledger::PeerMailboxLedger;
use crate::peer_mailbox::{transition_with_audit, PeerTransitionKind, TransitionAuditOutcome};
use spur_acp::domain::events::SpurEventBody;
use spur_acp::domain::peer_message::LedgerState;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileCounts {
    /// Count of `WorkerPeerMessageAuditFailed` events emitted during
    /// reconciliation.
    ///
    /// Migration note: prior to bd-cpf.5b, this field was always 0
    /// because the reconciler did not emit `WorkerPeerMessageAuditFailed`
    /// on transition errors. After bd-cpf.5b it reflects the real count.
    /// Dashboards filtering on `== 0` should switch to alerting on the
    /// `WorkerPeerMessageAuditFailed` event type with
    /// `transition_kind == "reconcile_to_delivered"` instead.
    pub audit_failed_emitted: u32,
    pub inflight_forced_to_delivered: u32,
    /// Count of reconciler entries that were already in `Delivered` state
    /// when the reconciler attempted to force them there. This reflects
    /// benign races where another actor (post-prompt path, concurrent
    /// reconcile) advanced the state between `non_terminal_entries()`
    /// snapshot and the transition call.
    /// Stage-1: always 0 (no concurrent actor). Stage-2: expected non-zero
    /// under crash-loop or periodic-reconcile scenarios.
    pub inflight_already_delivered: u32,
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
    let brain_session_id_typed =
        spur_acp::BrainSessionId::new(spur_acp::types::SessionId(brain_session_id.clone()));

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

                match transition_with_audit(
                    ledger.as_ref(),
                    &funnel,
                    &brain_session_id_typed,
                    &entry.envelope.target_delegation_id,
                    entry.envelope.message_id,
                    LedgerState::Delivered,
                    PeerTransitionKind::ReconcileToDelivered,
                )
                .await
                {
                    TransitionAuditOutcome::Changed => {
                        counts.inflight_forced_to_delivered += 1;
                        let target_prompt_id = entry
                            .injected_into_prompts
                            .iter()
                            .min()
                            .cloned()
                            .unwrap_or_default();
                        // `injected_chars: 0` is a known distortion: the
                        // ledger entry carries `injected_into_prompts` (prompt
                        // IDs) but not the byte count recorded by the normal
                        // post-prompt path in `orchestrator.rs`. Lineage and
                        // budget consumers should treat reconciler-emitted
                        // `Delivered` events as "char count unknown, not zero."
                        // Stage-2 fix: persist `injected_bytes` on the ledger
                        // entry at `record_injection` time.
                        funnel.emit(SpurEventBody::WorkerPeerMessageDelivered {
                            brain_session_id: brain_session_id.clone(),
                            message_id: entry.envelope.message_id,
                            target_delegation_id: entry.envelope.target_delegation_id.clone(),
                            target_prompt_id,
                            injected_chars: 0,
                        });
                    }
                    TransitionAuditOutcome::Unchanged(state) => {
                        // Benign race: another actor (post-prompt path,
                        // concurrent reconcile) advanced the entry to
                        // Delivered between `non_terminal_entries()`
                        // snapshot and this transition. Do not emit
                        // `WorkerPeerMessageDelivered`; the lineage projection
                        // would otherwise clobber the real `injected_chars`
                        // value with our placeholder 0.
                        counts.inflight_already_delivered += 1;
                        tracing::debug!(
                            message_id = ?entry.envelope.message_id,
                            from = ?entry.state,
                            observed_state = ?state,
                            "reconciler: skipped emit — entry already at target state (concurrent advance)"
                        );
                    }
                    TransitionAuditOutcome::TerminalSkip(state) => {
                        // Benign race: another actor (worker ack, drain,
                        // post-prompt) terminalized the message between
                        // `non_terminal_entries()` and this transition.
                        tracing::debug!(
                            message_id = ?entry.envelope.message_id,
                            from = ?entry.state,
                            terminal_state = ?state,
                            "reconciler: transition skipped because message reached terminal state via concurrent actor"
                        );
                    }
                    TransitionAuditOutcome::AuditFailed(err) => {
                        tracing::warn!(
                            message_id = ?entry.envelope.message_id,
                            from = ?entry.state,
                            to = ?LedgerState::Delivered,
                            %err,
                            "peer mailbox startup reconcile transition failed"
                        );
                        counts.audit_failed_emitted += 1;
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
        inflight_already_delivered: counts.inflight_already_delivered,
        inflight_stranded: counts.inflight_stranded,
        inflight_reverted_to_queued: 0,
        guards_re_wrapped: counts.guards_re_wrapped,
    });

    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_mailbox::ledger::{
        AcceptOutcome, InMemoryLedger, InjectionOutcome, LedgerEntry, LedgerError,
        PeerMailboxLedger, TransitionOutcome,
    };
    use spur_acp::domain::delegation::DelegationId;
    use spur_acp::domain::events::SpurEventBody;
    use spur_acp::domain::peer_message::{
        LedgerState, MessageKind, PeerMessageEnvelope, PeerMessageId,
    };
    use std::collections::HashSet;
    use tokio::sync::Mutex;
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

    struct FaultInjectionLedger {
        inner: Arc<InMemoryLedger>,
        fail_for: Mutex<HashSet<PeerMessageId>>,
    }

    impl FaultInjectionLedger {
        fn new(inner: Arc<InMemoryLedger>) -> Self {
            Self {
                inner,
                fail_for: Mutex::new(HashSet::new()),
            }
        }

        async fn fail_transition_for(&self, message_id: PeerMessageId) {
            self.fail_for.lock().await.insert(message_id);
        }
    }

    #[async_trait::async_trait]
    impl PeerMailboxLedger for FaultInjectionLedger {
        async fn accept(
            &self,
            envelope: PeerMessageEnvelope,
        ) -> Result<AcceptOutcome, LedgerError> {
            self.inner.accept(envelope).await
        }

        async fn transition(
            &self,
            message_id: &PeerMessageId,
            next: LedgerState,
        ) -> Result<TransitionOutcome, LedgerError> {
            if self.fail_for.lock().await.contains(message_id) {
                // `DeliveredInflight` is non-terminal, so this routes to
                // `AuditFailed` (not `TerminalSkip`) and matches the entry's
                // actual state at reconcile time — slightly more realistic
                // than asserting a stale `Queued` value.
                return Err(LedgerError::InvalidTransition {
                    from: LedgerState::DeliveredInflight,
                    to: next,
                });
            }

            self.inner.transition(message_id, next).await
        }

        async fn record_injection(
            &self,
            message_id: &PeerMessageId,
            target_prompt_id: &str,
        ) -> Result<InjectionOutcome, LedgerError> {
            self.inner
                .record_injection(message_id, target_prompt_id)
                .await
        }

        async fn get(&self, message_id: &PeerMessageId) -> Option<LedgerEntry> {
            self.inner.get(message_id).await
        }

        async fn pending_for_target(
            &self,
            target_delegation_id: &DelegationId,
        ) -> Vec<LedgerEntry> {
            self.inner.pending_for_target(target_delegation_id).await
        }

        async fn non_terminal_entries(&self) -> Vec<LedgerEntry> {
            self.inner.non_terminal_entries().await
        }
    }

    struct RaceSimulatingLedger {
        inner: Arc<InMemoryLedger>,
        race_for: Mutex<HashSet<PeerMessageId>>,
    }

    impl RaceSimulatingLedger {
        fn new(inner: Arc<InMemoryLedger>) -> Self {
            Self {
                inner,
                race_for: Mutex::new(HashSet::new()),
            }
        }

        async fn race_transition_for(&self, message_id: PeerMessageId) {
            self.race_for.lock().await.insert(message_id);
        }
    }

    #[async_trait::async_trait]
    impl PeerMailboxLedger for RaceSimulatingLedger {
        async fn accept(
            &self,
            envelope: PeerMessageEnvelope,
        ) -> Result<AcceptOutcome, LedgerError> {
            self.inner.accept(envelope).await
        }

        async fn transition(
            &self,
            message_id: &PeerMessageId,
            next: LedgerState,
        ) -> Result<TransitionOutcome, LedgerError> {
            if next == LedgerState::Delivered && self.race_for.lock().await.contains(message_id) {
                return Ok(TransitionOutcome::Unchanged(LedgerState::Delivered));
            }

            self.inner.transition(message_id, next).await
        }

        async fn record_injection(
            &self,
            message_id: &PeerMessageId,
            target_prompt_id: &str,
        ) -> Result<InjectionOutcome, LedgerError> {
            self.inner
                .record_injection(message_id, target_prompt_id)
                .await
        }

        async fn get(&self, message_id: &PeerMessageId) -> Option<LedgerEntry> {
            self.inner.get(message_id).await
        }

        async fn pending_for_target(
            &self,
            target_delegation_id: &DelegationId,
        ) -> Vec<LedgerEntry> {
            self.inner.pending_for_target(target_delegation_id).await
        }

        async fn non_terminal_entries(&self) -> Vec<LedgerEntry> {
            self.inner.non_terminal_entries().await
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
    async fn reconcile_increments_already_delivered_when_race_observes_unchanged() {
        let inner = Arc::new(InMemoryLedger::new());
        let ledger = Arc::new(RaceSimulatingLedger::new(inner.clone()));
        let env = envelope();

        inner.accept(env.clone()).await.unwrap();
        ledger
            .record_injection(&env.message_id, "target-prompt")
            .await
            .unwrap();
        ledger
            .transition(&env.message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();
        ledger.race_transition_for(env.message_id).await;

        let (funnel, mut events) = crate::event_funnel::test_channel();

        let counts = run_startup_reconcile(
            ledger.clone(),
            funnel,
            "brain-session".into(),
            Duration::from_millis(100),
        )
        .await;

        assert_eq!(counts.inflight_forced_to_delivered, 0);
        assert_eq!(counts.inflight_already_delivered, 1);
        let entry = inner.get(&env.message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::DeliveredInflight);

        let event = events.recv().await.expect("reconciled event");
        assert!(matches!(
            event,
            SpurEventBody::WorkerPeerMailboxReconciled {
                brain_session_id,
                inflight_forced_to_delivered: 0,
                inflight_already_delivered: 1,
                ..
            } if brain_session_id == "brain-session"
        ));
        assert!(events.try_recv().is_err());
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
                inflight_already_delivered: 0,
                inflight_stranded: 1,
                inflight_reverted_to_queued: 0,
                guards_re_wrapped: 1,
            } if brain_session_id == "brain-session"
        )));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn reconcile_emits_audit_failed_when_transition_fails_non_terminally() {
        let ledger = Arc::new(FaultInjectionLedger::new(Arc::new(InMemoryLedger::new())));
        let env = envelope();

        ledger.accept(env.clone()).await.unwrap();
        ledger
            .record_injection(&env.message_id, "target-prompt")
            .await
            .unwrap();
        ledger
            .transition(&env.message_id, LedgerState::Queued)
            .await
            .unwrap();
        ledger
            .transition(&env.message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();
        ledger.fail_transition_for(env.message_id).await;

        let (funnel, mut events) = crate::event_funnel::test_channel();

        let counts = run_startup_reconcile(
            ledger.clone(),
            funnel,
            "brain-session".into(),
            Duration::from_millis(100),
        )
        .await;

        assert_eq!(counts.audit_failed_emitted, 1);
        let entry = ledger.get(&env.message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::DeliveredInflight);

        let emitted = [
            events.recv().await.expect("audit failed event"),
            events.recv().await.expect("reconciled event"),
        ];
        let audit_failed: Vec<_> = emitted
            .iter()
            .filter_map(|event| match event {
                SpurEventBody::WorkerPeerMessageAuditFailed {
                    message_id,
                    transition_kind,
                    error,
                    ..
                } => Some((message_id, transition_kind, error)),
                _ => None,
            })
            .collect();
        assert_eq!(audit_failed.len(), 1);
        let (message_id, transition_kind, error) = audit_failed[0];
        assert_eq!(*message_id, env.message_id);
        assert_eq!(transition_kind, "reconcile_to_delivered");
        assert!(!error.is_empty());

        assert!(emitted.iter().any(|event| matches!(
            event,
            SpurEventBody::WorkerPeerMailboxReconciled {
                brain_session_id,
                audit_failed_emitted: 1,
                ..
            } if brain_session_id == "brain-session"
        )));
    }
}
