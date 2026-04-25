use crate::event_funnel::FunnelHandle;
use crate::peer_mailbox::ledger::PeerMailboxLedger;
use spur_acp::domain::events::SpurEventBody;
use spur_acp::domain::peer_message::{LedgerState, PeerMessageId, TerminalOutcome};
use std::sync::Arc;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// Sent to the reconciler when a `PeerMessageGuard` drops without finalize.
#[derive(Debug, Clone)]
pub struct StrandedMessage {
    pub message_id: PeerMessageId,
    pub default_outcome: TerminalOutcome,
}

/// Outcome that `finalize` records on the ledger.
#[derive(Debug, Clone)]
pub enum GuardOutcome {
    Terminal(TerminalOutcome),
}

/// RAII guard that ensures every accepted-but-not-terminal peer message
/// reaches a terminal state, even on panic or task abort.
///
/// Construct via `PeerMessageGuard::wrap`. Resolve via `finalize().await`.
/// If dropped without finalize, the guard enqueues a `StrandedMessage` onto
/// the reconciler mpsc (sync, non-blocking) and logs a `tracing::error!`.
/// It never performs async work in `Drop`.
pub struct PeerMessageGuard {
    message_id: PeerMessageId,
    reconciler_tx: UnboundedSender<StrandedMessage>,
    default_outcome: TerminalOutcome,
    finalized: bool,
}

impl PeerMessageGuard {
    pub fn wrap(
        message_id: PeerMessageId,
        reconciler_tx: UnboundedSender<StrandedMessage>,
        default_outcome: TerminalOutcome,
    ) -> Self {
        Self {
            message_id,
            reconciler_tx,
            default_outcome,
            finalized: false,
        }
    }

    /// Normal-path resolution. Marks the guard finalized; `Drop` becomes a no-op.
    /// The caller is responsible for performing the actual ledger transition,
    /// beads write, and event emission before calling this.
    pub async fn finalize(mut self, _outcome: GuardOutcome) {
        self.finalized = true;
    }

    pub fn message_id(&self) -> &PeerMessageId {
        &self.message_id
    }
}

impl Drop for PeerMessageGuard {
    fn drop(&mut self) {
        if self.finalized {
            return;
        }

        tracing::error!(
            message_id = ?self.message_id,
            "PeerMessageGuard dropped without finalize; enqueueing stranded recovery"
        );
        let _ = self.reconciler_tx.send(StrandedMessage {
            message_id: self.message_id,
            default_outcome: self.default_outcome.clone(),
        });
    }
}

/// Long-lived task that drains the stranded-message mpsc and applies recovery
/// transitions. Spawned at orchestrator boot; survives across attempts.
pub async fn run_reconciler_loop(
    mut rx: UnboundedReceiver<StrandedMessage>,
    ledger: Arc<dyn PeerMailboxLedger>,
    funnel: FunnelHandle,
    brain_session_id: String,
) {
    while let Some(stranded) = rx.recv().await {
        let reason = match &stranded.default_outcome {
            TerminalOutcome::Undeliverable { reason } => reason.clone(),
            _ => "guard_dropped_unfinalized".into(),
        };

        if let Ok(_) = ledger
            .transition(&stranded.message_id, LedgerState::Undeliverable)
            .await
        {
            if let Some(entry) = ledger.get(&stranded.message_id).await {
                funnel.emit(SpurEventBody::WorkerPeerMessageUndeliverable {
                    brain_session_id: brain_session_id.clone(),
                    message_id: stranded.message_id,
                    target_delegation_id: entry.envelope.target_delegation_id,
                    reason,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    #[tokio::test]
    async fn drop_without_finalize_enqueues_stranded() {
        let (tx, mut rx) = unbounded_channel();
        let id: PeerMessageId =
            serde_json::from_str("\"00000000-0000-0000-0000-000000000101\"").unwrap();
        {
            let _guard = PeerMessageGuard::wrap(
                id,
                tx,
                TerminalOutcome::Undeliverable {
                    reason: "test".into(),
                },
            );
        }

        let stranded = rx.recv().await.expect("expected stranded message");
        assert_eq!(stranded.message_id, id);
    }

    #[tokio::test]
    async fn finalize_prevents_stranded_enqueue() {
        let (tx, mut rx) = unbounded_channel();
        let id: PeerMessageId =
            serde_json::from_str("\"00000000-0000-0000-0000-000000000102\"").unwrap();
        let guard = PeerMessageGuard::wrap(
            id,
            tx,
            TerminalOutcome::Undeliverable {
                reason: "test".into(),
            },
        );

        guard
            .finalize(GuardOutcome::Terminal(TerminalOutcome::Consumed))
            .await;

        assert!(rx.try_recv().is_err());
    }
}
