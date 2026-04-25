use crate::event_funnel::FunnelHandle;
use crate::peer_mailbox::guard::{PeerMessageGuard, StrandedMessage};
use crate::peer_mailbox::ledger::{AcceptOutcome, PeerMailboxLedger, TransitionOutcome};
use crate::peer_mailbox::limits::Limits;
use spur_acp::domain::events::SpurEventBody;
use spur_acp::domain::peer_message::{
    LedgerState, PeerMessageEnvelope, PeerMessageId, TerminalOutcome,
};
use spur_mcp::plan::scope_snapshot::{EdgeCheck, PlanScopeSnapshot};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RouterError {
    #[error("rejected: {reason}")]
    Rejected { reason: String },
    #[error("ledger error: {0}")]
    Ledger(String),
}

pub struct PeerMailboxRouter {
    ledger: Arc<dyn PeerMailboxLedger>,
    funnel: FunnelHandle,
    reconciler_tx: UnboundedSender<StrandedMessage>,
    limits: Limits,
    brain_session_id: String,
}

impl PeerMailboxRouter {
    pub fn new(
        ledger: Arc<dyn PeerMailboxLedger>,
        funnel: FunnelHandle,
        reconciler_tx: UnboundedSender<StrandedMessage>,
        limits: Limits,
        brain_session_id: String,
    ) -> Self {
        Self {
            ledger,
            funnel,
            reconciler_tx,
            limits,
            brain_session_id,
        }
    }

    pub async fn accept_or_reject(
        &self,
        request: PeerMessageEnvelope,
        snapshot: &PlanScopeSnapshot,
    ) -> Result<PeerMessageGuard, RouterError> {
        // Body size cap is checked before any other validation or funnel emit.
        if request.body.len() > self.limits.max_peer_message_size {
            return Err(self.reject(request, "body_size_exceeded"));
        }

        if request.plan_version != snapshot.plan_version {
            return Err(self.reject(request, "plan_version_changed"));
        }

        match snapshot.check_peer_edge(&request.source_delegation_id, &request.target_delegation_id)
        {
            EdgeCheck::Allowed => {}
            EdgeCheck::NotInDag => return Err(self.reject(request, "not_in_dag")),
            EdgeCheck::SourceMissing => return Err(self.reject(request, "source_missing")),
            EdgeCheck::TargetMissing => return Err(self.reject(request, "target_missing")),
            EdgeCheck::SourceSuperseded => return Err(self.reject(request, "source_superseded")),
            EdgeCheck::TargetSuperseded => return Err(self.reject(request, "target_superseded")),
            EdgeCheck::SourceTerminal => return Err(self.reject(request, "source_terminal")),
        }

        let envelope = request.clone();
        let accept_outcome = self
            .ledger
            .accept(envelope.clone())
            .await
            .map_err(|err| RouterError::Ledger(err.to_string()))?;

        match accept_outcome {
            AcceptOutcome::Created => {
                self.funnel.emit(SpurEventBody::WorkerPeerMessageAccepted {
                    brain_session_id: self.brain_session_id.clone(),
                    message_id: envelope.message_id,
                    source_delegation_id: envelope.source_delegation_id.clone(),
                    target_delegation_id: envelope.target_delegation_id.clone(),
                    kind: envelope.kind,
                    sequence: envelope.sequence,
                });
            }
            AcceptOutcome::AlreadyAccepted => {
                tracing::debug!(
                    message_id = ?envelope.message_id,
                    "peer mailbox accept replay; skipping duplicate accepted event"
                );
            }
        }

        Ok(PeerMessageGuard::wrap(
            envelope.message_id,
            self.reconciler_tx.clone(),
            TerminalOutcome::Undeliverable {
                reason: "guard_dropped_unfinalized".into(),
            },
        ))
    }

    fn reject(&self, request: PeerMessageEnvelope, reason: &str) -> RouterError {
        self.funnel.emit(SpurEventBody::WorkerPeerMessageRejected {
            brain_session_id: self.brain_session_id.clone(),
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
                return Err(RouterError::Ledger(
                    "unsupported terminal outcome".to_string(),
                ))
            }
        };

        match self
            .ledger
            .transition(message_id, next)
            .await
            .map_err(|err| RouterError::Ledger(err.to_string()))?
        {
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

        if let Some(entry) = self.ledger.get(message_id).await {
            let target_delegation_id = entry.envelope.target_delegation_id;
            let body = match outcome {
                TerminalOutcome::Consumed => SpurEventBody::WorkerPeerMessageConsumed {
                    brain_session_id: self.brain_session_id.clone(),
                    message_id: *message_id,
                    target_delegation_id,
                },
                TerminalOutcome::Ignored { reason } => SpurEventBody::WorkerPeerMessageIgnored {
                    brain_session_id: self.brain_session_id.clone(),
                    message_id: *message_id,
                    target_delegation_id,
                    reason,
                },
                TerminalOutcome::Expired => SpurEventBody::WorkerPeerMessageExpired {
                    brain_session_id: self.brain_session_id.clone(),
                    message_id: *message_id,
                    target_delegation_id,
                },
                TerminalOutcome::Dropped { reason } => SpurEventBody::WorkerPeerMessageDropped {
                    brain_session_id: self.brain_session_id.clone(),
                    message_id: *message_id,
                    target_delegation_id,
                    reason,
                },
                TerminalOutcome::Undeliverable { reason } => {
                    SpurEventBody::WorkerPeerMessageUndeliverable {
                        brain_session_id: self.brain_session_id.clone(),
                        message_id: *message_id,
                        target_delegation_id,
                        reason,
                    }
                }
                _ => {
                    return Err(RouterError::Ledger(
                        "unsupported terminal outcome".to_string(),
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
    use spur_acp::domain::delegation::DelegationId;
    use spur_acp::domain::events::SpurEventBody;
    use spur_acp::domain::peer_message::MessageKind;
    use spur_mcp::plan::scope_snapshot::PlanScopeSnapshot;
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
        let router =
            PeerMailboxRouter::new(ledger.clone(), funnel, tx, Limits::default(), "bs".into());
        (router, ledger, event_rx)
    }

    #[tokio::test]
    async fn accept_succeeds_for_allowed_edge() {
        let (router, _ledger, _events) = fixture().await;
        let snap = snapshot_allowing("src", "tgt");
        let env = envelope("src", "tgt");

        let guard = router.accept_or_reject(env, &snap).await.unwrap();

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

        let err = match router.accept_or_reject(env, &snap).await {
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

        let err = match router.accept_or_reject(env, &snap).await {
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
    async fn accept_replay_does_not_emit_duplicate_event() {
        let (router, _ledger, mut events) = fixture().await;
        let snap = snapshot_allowing("src", "tgt");
        let env = envelope("src", "tgt");

        let first = router.accept_or_reject(env.clone(), &snap).await.unwrap();
        first
            .finalize(GuardOutcome::Terminal(TerminalOutcome::Consumed))
            .await;
        let second = router.accept_or_reject(env, &snap).await.unwrap();
        second
            .finalize(GuardOutcome::Terminal(TerminalOutcome::Consumed))
            .await;

        let mut accepted_count = 0;
        while let Ok(event) = events.try_recv() {
            if matches!(event, SpurEventBody::WorkerPeerMessageAccepted { .. }) {
                accepted_count += 1;
            }
        }
        assert_eq!(accepted_count, 1);
    }
}
