use crate::event_funnel::FunnelHandle;
use crate::peer_mailbox::guard::{PeerMessageGuard, StrandedMessage};
use crate::peer_mailbox::ledger::{
    AcceptOutcome, LedgerError, PeerMailboxLedger, TransitionOutcome,
};
use crate::peer_mailbox::limits::Limits;
use crate::plan::scope_snapshot::{EdgeCheck, PlanScopeSnapshot};
use spur_acp::domain::events::SpurEventBody;
use spur_acp::domain::peer_message::{
    LedgerState, PeerMessageEnvelope, PeerMessageId, TerminalOutcome,
};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RouterError {
    /// Request-level rejection before ledger mutation, with a stable
    /// machine-readable reason emitted to the peer-message funnel.
    #[error("rejected: {reason}")]
    Rejected { reason: String },
    /// Typed failures returned by the peer-mailbox ledger implementation.
    #[error("ledger: {0}")]
    Ledger(#[from] LedgerError),
    /// Router-only invariant failures that are not ledger state-machine
    /// errors and should not be retried as ledger/storage failures.
    #[error("invariant violation: {0}")]
    InvariantViolation(String),
}

/// Outcome of a successful router accept attempt. Distinct from rejection
/// (which is signaled via `RouterError::Rejected`) and ledger errors
/// (which surface via `RouterError::Ledger`).
///
/// Distinguishes a fresh acceptance (caller receives a guard and is
/// responsible for finalize) from a replay (caller receives nothing —
/// the original handler still owns the guard). This separation is
/// critical for spec invariant "at most one guard exists per message at
/// any time": if we returned a fresh guard on replay, dropping it would
/// enqueue a stranded message and the reconciler would forcibly mark the
/// in-flight original as Undeliverable. The `AlreadyAccepted` variant
/// prevents that.
///
/// Forward-compat note: marked `#[non_exhaustive]` so future variants
/// (e.g., `Deferred`, `Buffered` for Stage-2 persistent ledger) can be
/// added without breaking external matchers. Internal same-crate matches
/// remain exhaustive — the compile-time pressure to handle new variants
/// is preserved where it matters most.
#[derive(Debug)]
#[non_exhaustive]
pub enum Acceptance {
    Created(PeerMessageGuard),
    AlreadyAccepted,
}

pub struct PeerMailboxRouter {
    ledger: Arc<dyn PeerMailboxLedger>,
    funnel: FunnelHandle,
    reconciler_tx: UnboundedSender<StrandedMessage>,
    limits: Limits,
}

impl PeerMailboxRouter {
    pub fn new(
        ledger: Arc<dyn PeerMailboxLedger>,
        funnel: FunnelHandle,
        reconciler_tx: UnboundedSender<StrandedMessage>,
        limits: Limits,
    ) -> Self {
        assert!(
            limits.drain_max_total_ms > 0,
            "peer mailbox drain_max_total_ms must be > 0"
        );
        if limits.drain_max_total_ms < limits.drain_quiet_window_ms {
            tracing::warn!(
                drain_max_total_ms = limits.drain_max_total_ms,
                drain_quiet_window_ms = limits.drain_quiet_window_ms,
                "peer mailbox drain absolute cap is below quiet window; cap wins"
            );
        }
        Self {
            ledger,
            funnel,
            reconciler_tx,
            limits,
        }
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    pub async fn accept_or_reject(
        &self,
        brain_session_id: &str,
        request: PeerMessageEnvelope,
        snapshot: &PlanScopeSnapshot,
    ) -> Result<Acceptance, RouterError> {
        // Body size cap is checked before any other validation or funnel emit.
        if request.body.len() > self.limits.max_peer_message_size {
            return Err(self.reject(brain_session_id, request, "body_size_exceeded"));
        }

        if request.plan_version != snapshot.plan_version {
            return Err(self.reject(brain_session_id, request, "plan_version_changed"));
        }

        match snapshot.check_peer_edge(&request.source_delegation_id, &request.target_delegation_id)
        {
            EdgeCheck::Allowed => {}
            EdgeCheck::NotInDag => {
                return Err(self.reject(brain_session_id, request, "not_in_dag"))
            }
            EdgeCheck::SourceMissing => {
                return Err(self.reject(brain_session_id, request, "source_missing"))
            }
            EdgeCheck::TargetMissing => {
                return Err(self.reject(brain_session_id, request, "target_missing"))
            }
            EdgeCheck::SourceSuperseded => {
                return Err(self.reject(brain_session_id, request, "source_superseded"))
            }
            EdgeCheck::TargetSuperseded => {
                return Err(self.reject(brain_session_id, request, "target_superseded"))
            }
            EdgeCheck::SourceTerminal => {
                return Err(self.reject(brain_session_id, request, "source_terminal"))
            }
        }

        let envelope = request.clone();
        let accept_outcome = self.ledger.accept(envelope.clone()).await?;

        match accept_outcome {
            AcceptOutcome::Created => {
                self.funnel.emit(SpurEventBody::WorkerPeerMessageAccepted {
                    brain_session_id: brain_session_id.to_string(),
                    message_id: envelope.message_id,
                    source_delegation_id: envelope.source_delegation_id.clone(),
                    target_delegation_id: envelope.target_delegation_id.clone(),
                    kind: envelope.kind,
                    sequence: envelope.sequence,
                });
                Ok(Acceptance::Created(PeerMessageGuard::wrap(
                    envelope.message_id,
                    self.reconciler_tx.clone(),
                    TerminalOutcome::Undeliverable {
                        reason: "guard_dropped_unfinalized".into(),
                    },
                )))
            }
            AcceptOutcome::AlreadyAccepted => {
                tracing::debug!(
                    message_id = ?envelope.message_id,
                    "peer mailbox accept replay; original handler retains guard"
                );
                Ok(Acceptance::AlreadyAccepted)
            }
        }
    }

    fn reject(
        &self,
        brain_session_id: &str,
        request: PeerMessageEnvelope,
        reason: &str,
    ) -> RouterError {
        self.funnel.emit(SpurEventBody::WorkerPeerMessageRejected {
            brain_session_id: brain_session_id.to_string(),
            message_id: request.message_id,
            source_delegation_id: request.source_delegation_id,
            target_delegation_id: request.target_delegation_id,
            reason: reason.into(),
        });
        RouterError::Rejected {
            reason: reason.into(),
        }
    }

    pub async fn record_terminal(
        &self,
        brain_session_id: &str,
        message_id: &PeerMessageId,
        outcome: TerminalOutcome,
    ) -> Result<(), RouterError> {
        let next = match &outcome {
            TerminalOutcome::Consumed => LedgerState::Consumed,
            TerminalOutcome::Ignored { .. } => LedgerState::Ignored,
            TerminalOutcome::Expired => LedgerState::Expired,
            TerminalOutcome::Dropped { .. } => LedgerState::Dropped,
            TerminalOutcome::Undeliverable { .. } => LedgerState::Undeliverable,
            _ => {
                return Err(RouterError::InvariantViolation(
                    "unsupported terminal outcome".into(),
                ))
            }
        };

        match self.ledger.transition(message_id, next).await? {
            TransitionOutcome::Changed { .. } => {}
            TransitionOutcome::Unchanged(state) => {
                tracing::debug!(
                    message_id = ?message_id,
                    state = ?state,
                    "peer mailbox terminal replay; skipping duplicate lifecycle event"
                );
                return Ok(());
            }
        }

        // After a successful `Changed` transition, the entry must still exist
        // in the ledger — InMemoryLedger never removes entries, and any
        // future persistent backend that supports eviction must serialize
        // eviction with transitions. A `None` here means an upstream
        // invariant has broken; surface it rather than silently dropping
        // the lifecycle event.
        let entry = self.ledger.get(message_id).await.ok_or_else(|| {
            RouterError::InvariantViolation(format!(
                "transition succeeded but entry missing for {:?}",
                message_id
            ))
        })?;
        {
            let target_delegation_id = entry.envelope.target_delegation_id;
            let body = match outcome {
                TerminalOutcome::Consumed => SpurEventBody::WorkerPeerMessageConsumed {
                    brain_session_id: brain_session_id.to_string(),
                    message_id: *message_id,
                    target_delegation_id,
                },
                TerminalOutcome::Ignored { reason } => SpurEventBody::WorkerPeerMessageIgnored {
                    brain_session_id: brain_session_id.to_string(),
                    message_id: *message_id,
                    target_delegation_id,
                    reason,
                },
                TerminalOutcome::Expired => SpurEventBody::WorkerPeerMessageExpired {
                    brain_session_id: brain_session_id.to_string(),
                    message_id: *message_id,
                    target_delegation_id,
                },
                TerminalOutcome::Dropped { reason } => SpurEventBody::WorkerPeerMessageDropped {
                    brain_session_id: brain_session_id.to_string(),
                    message_id: *message_id,
                    target_delegation_id,
                    reason,
                },
                TerminalOutcome::Undeliverable { reason } => {
                    SpurEventBody::WorkerPeerMessageUndeliverable {
                        brain_session_id: brain_session_id.to_string(),
                        message_id: *message_id,
                        target_delegation_id,
                        reason,
                    }
                }
                _ => {
                    return Err(RouterError::InvariantViolation(
                        "unsupported terminal outcome".into(),
                    ))
                }
            };
            self.funnel.emit(body);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_mailbox::guard::GuardOutcome;
    use crate::peer_mailbox::ledger::InMemoryLedger;
    use crate::plan::scope_snapshot::PlanScopeSnapshot;
    use spur_acp::domain::delegation::DelegationId;
    use spur_acp::domain::events::SpurEventBody;
    use spur_acp::domain::peer_message::MessageKind;
    use std::collections::{HashMap, HashSet};
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    fn snapshot_allowing(src: &str, tgt: &str) -> PlanScopeSnapshot {
        let mut delegation_to_task = HashMap::new();
        delegation_to_task.insert(DelegationId(src.into()), "ta".into());
        delegation_to_task.insert(DelegationId(tgt.into()), "tb".into());
        let mut peer_edges = HashSet::new();
        peer_edges.insert(("ta".into(), "tb".into()));
        PlanScopeSnapshot {
            plan_version: 1,
            peer_edges,
            delegation_to_task,
            delegation_to_issue: HashMap::new(),
            superseded_tasks: HashSet::new(),
            terminal_tasks: HashSet::new(),
        }
    }

    fn envelope(src: &str, tgt: &str) -> PeerMessageEnvelope {
        PeerMessageEnvelope {
            schema: "spur-peer-message/v1".into(),
            message_id: serde_json::from_str("\"00000000-0000-0000-0000-000000000201\"").unwrap(),
            source_delegation_id: DelegationId(src.into()),
            target_delegation_id: DelegationId(tgt.into()),
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

    async fn fixture() -> (
        PeerMailboxRouter,
        Arc<InMemoryLedger>,
        UnboundedReceiver<SpurEventBody>,
    ) {
        let ledger = Arc::new(InMemoryLedger::new());
        let (funnel, event_rx) = crate::event_funnel::test_channel();
        let (tx, _rx) = unbounded_channel();
        let router = PeerMailboxRouter::new(ledger.clone(), funnel, tx, Limits::default());
        (router, ledger, event_rx)
    }

    fn unwrap_created(acc: Acceptance) -> PeerMessageGuard {
        match acc {
            Acceptance::Created(g) => g,
            Acceptance::AlreadyAccepted => panic!("expected Acceptance::Created"),
        }
    }

    /// Drain all events from the funnel test channel. The funnel relay task
    /// runs on its own tokio scheduler slot, so a synchronous `try_recv()`
    /// can race ahead of the relay. We poll with a small per-recv timeout
    /// to give the relay a chance to forward, and stop once nothing
    /// arrives within that window.
    async fn drain_events(events: &mut UnboundedReceiver<SpurEventBody>) -> Vec<SpurEventBody> {
        let mut out = Vec::new();
        while let Ok(Some(event)) =
            tokio::time::timeout(std::time::Duration::from_millis(20), events.recv()).await
        {
            out.push(event);
        }
        out
    }

    #[tokio::test]
    async fn accept_succeeds_for_allowed_edge() {
        let (router, _ledger, _events) = fixture().await;
        let snap = snapshot_allowing("src", "tgt");
        let env = envelope("src", "tgt");

        let guard = unwrap_created(router.accept_or_reject("bs", env, &snap).await.unwrap());

        guard
            .finalize(GuardOutcome::Terminal(TerminalOutcome::Consumed))
            .await;
    }

    #[tokio::test]
    async fn rejects_when_not_in_dag() {
        let (router, _ledger, _events) = fixture().await;
        let mut snap = snapshot_allowing("src", "tgt");
        snap.peer_edges.clear();
        let env = envelope("src", "tgt");

        let err = match router.accept_or_reject("bs", env, &snap).await {
            Ok(_) => panic!("expected not_in_dag rejection"),
            Err(err) => err,
        };

        assert_eq!(
            err,
            RouterError::Rejected {
                reason: "not_in_dag".into()
            }
        );
    }

    #[tokio::test]
    async fn rejects_oversized_body_before_any_validation() {
        let (router, _ledger, _events) = fixture().await;
        let mut snap = snapshot_allowing("src", "tgt");
        snap.plan_version = 2;
        let mut env = envelope("src", "tgt");
        env.body = "x".repeat(100_000);

        let err = match router.accept_or_reject("bs", env, &snap).await {
            Ok(_) => panic!("expected body_size_exceeded rejection"),
            Err(err) => err,
        };

        assert_eq!(
            err,
            RouterError::Rejected {
                reason: "body_size_exceeded".into()
            }
        );
    }

    #[tokio::test]
    async fn accept_replay_returns_already_accepted_without_fresh_guard() {
        // Spec invariant: at most one guard exists per message at any time.
        // A replay must NOT spawn a second guard, otherwise dropping it
        // would enqueue a stranded message and the reconciler would
        // forcibly mark the in-flight original as Undeliverable.
        let (router, _ledger, mut events) = fixture().await;
        let snap = snapshot_allowing("src", "tgt");
        let env = envelope("src", "tgt");

        let first = unwrap_created(
            router
                .accept_or_reject("bs", env.clone(), &snap)
                .await
                .unwrap(),
        );
        // Don't finalize first yet — simulate the original handler still
        // working when the replay arrives.
        let replay = router
            .accept_or_reject("bs", env.clone(), &snap)
            .await
            .unwrap();
        assert!(matches!(replay, Acceptance::AlreadyAccepted));

        // Now finalize first; no orphaned stranded enqueue should fire.
        first
            .finalize(GuardOutcome::Terminal(TerminalOutcome::Consumed))
            .await;

        let drained = drain_events(&mut events).await;
        let accepted_count = drained
            .iter()
            .filter(|e| matches!(e, SpurEventBody::WorkerPeerMessageAccepted { .. }))
            .count();
        assert_eq!(accepted_count, 1);
    }

    #[tokio::test]
    async fn body_size_check_runs_before_dag_check() {
        // Both checks would reject this message; body size must win
        // (per spec: backpressure-class checks happen first so a
        // floodable input never reaches DAG validation).
        let (router, _ledger, _events) = fixture().await;
        let mut snap = snapshot_allowing("src", "tgt");
        snap.peer_edges.clear(); // would also fail with not_in_dag
        let mut env = envelope("src", "tgt");
        env.body = "x".repeat(100_000);

        let err = router.accept_or_reject("bs", env, &snap).await.unwrap_err();
        assert_eq!(
            err,
            RouterError::Rejected {
                reason: "body_size_exceeded".into()
            }
        );
    }

    #[tokio::test]
    async fn record_terminal_emits_lifecycle_event_once() {
        let (router, ledger, mut events) = fixture().await;
        let snap = snapshot_allowing("src", "tgt");
        let env = envelope("src", "tgt");
        let message_id = env.message_id;

        let guard = unwrap_created(router.accept_or_reject("bs", env, &snap).await.unwrap());
        ledger
            .transition(&message_id, LedgerState::Queued)
            .await
            .unwrap();
        ledger
            .transition(&message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();
        ledger
            .transition(&message_id, LedgerState::Delivered)
            .await
            .unwrap();
        // finalize() is informational only; record_terminal does the work.
        guard
            .finalize(GuardOutcome::Terminal(TerminalOutcome::Consumed))
            .await;

        router
            .record_terminal("bs", &message_id, TerminalOutcome::Consumed)
            .await
            .unwrap();
        // Replay: must be a no-op (Unchanged path).
        router
            .record_terminal("bs", &message_id, TerminalOutcome::Consumed)
            .await
            .unwrap();

        let drained = drain_events(&mut events).await;
        let consumed_count = drained
            .iter()
            .filter(|e| matches!(e, SpurEventBody::WorkerPeerMessageConsumed { .. }))
            .count();
        assert_eq!(consumed_count, 1);
    }

    #[tokio::test]
    async fn record_terminal_emits_undeliverable_with_reason() {
        let (router, _ledger, mut events) = fixture().await;
        let snap = snapshot_allowing("src", "tgt");
        let env = envelope("src", "tgt");
        let message_id = env.message_id;

        let guard = unwrap_created(router.accept_or_reject("bs", env, &snap).await.unwrap());
        guard
            .finalize(GuardOutcome::Terminal(TerminalOutcome::Consumed))
            .await;

        router
            .record_terminal(
                "bs",
                &message_id,
                TerminalOutcome::Undeliverable {
                    reason: "test_path".into(),
                },
            )
            .await
            .unwrap();

        let drained = drain_events(&mut events).await;
        let found = drained.iter().any(|e| {
            matches!(
                e,
                SpurEventBody::WorkerPeerMessageUndeliverable { reason, .. } if reason == "test_path"
            )
        });
        assert!(found, "expected one Undeliverable event with reason");
    }

    #[tokio::test]
    async fn record_terminal_on_unknown_message_returns_ledger_error() {
        let (router, _ledger, _events) = fixture().await;
        let unknown: PeerMessageId =
            serde_json::from_str("\"00000000-0000-0000-0000-000000000999\"").unwrap();
        let err = router
            .record_terminal("bs", &unknown, TerminalOutcome::Consumed)
            .await
            .unwrap_err();
        assert!(matches!(err, RouterError::Ledger(_)));
    }

    #[tokio::test]
    async fn router_error_preserves_ledger_invalid_transition_typed() {
        let (router, ledger, _events) = fixture().await;
        let snap = snapshot_allowing("src", "tgt");
        let env = envelope("src", "tgt");
        let message_id = env.message_id;

        let _guard = unwrap_created(router.accept_or_reject("bs", env, &snap).await.unwrap());
        ledger
            .transition(&message_id, LedgerState::Queued)
            .await
            .unwrap();
        ledger
            .transition(&message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();
        ledger
            .transition(&message_id, LedgerState::Delivered)
            .await
            .unwrap();
        router
            .record_terminal(
                "bs",
                &message_id,
                TerminalOutcome::Ignored {
                    reason: "ignored".into(),
                },
            )
            .await
            .unwrap();

        let err = router
            .record_terminal("bs", &message_id, TerminalOutcome::Consumed)
            .await
            .unwrap_err();

        match err {
            RouterError::Ledger(crate::peer_mailbox::ledger::LedgerError::InvalidTransition {
                from,
                to,
            }) => {
                assert_eq!(from, LedgerState::Ignored);
                assert_eq!(to, LedgerState::Consumed);
            }
            other => panic!("expected typed InvalidTransition, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn router_error_preserves_ledger_already_terminal_typed() {
        let (router, ledger, _events) = fixture().await;
        let snap = snapshot_allowing("src", "tgt");
        let env = envelope("src", "tgt");
        let message_id = env.message_id;

        let _guard = unwrap_created(
            router
                .accept_or_reject("bs", env.clone(), &snap)
                .await
                .unwrap(),
        );
        ledger
            .transition(&message_id, LedgerState::Queued)
            .await
            .unwrap();
        ledger
            .transition(&message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();
        ledger
            .transition(&message_id, LedgerState::Delivered)
            .await
            .unwrap();
        router
            .record_terminal("bs", &message_id, TerminalOutcome::Consumed)
            .await
            .unwrap();

        let err = router.accept_or_reject("bs", env, &snap).await.unwrap_err();

        match err {
            RouterError::Ledger(crate::peer_mailbox::ledger::LedgerError::AlreadyTerminal {
                state,
                ..
            }) => {
                assert_eq!(state, LedgerState::Consumed);
            }
            other => panic!("expected typed AlreadyTerminal, got {other:?}"),
        }
    }
}
