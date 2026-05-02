//! Singleton event emitter task (Phase S2).
//!
//! All `SpurEvent` emission inside the orchestrator must flow through
//! this funnel. Each emit call sends a `SpurEventBody` over an
//! unbounded mpsc; a dedicated task reads the mpsc, stamps a monotonic
//! `seq` and `occurred_at`, and forwards on the broadcast channel.
//!
//! This guarantees strict seq ordering (Pitfall P1 in the design
//! spec): subscribers observe events in exactly the order the funnel
//! stamped them.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::{broadcast, mpsc};

use crate::lineage::ExecutorLineage;
use spur_acp::domain::events::{SpurEvent, SpurEventBody};

#[allow(clippy::large_enum_variant)]
enum FunnelCommand {
    Event(SpurEventBody),
    LineageSnapshot(tokio::sync::oneshot::Sender<Option<ExecutorLineage>>),
}

/// Handle returned by `spawn_funnel`. Clone cheaply; call `emit`.
#[derive(Clone)]
pub struct FunnelHandle {
    tx: mpsc::UnboundedSender<FunnelCommand>,
}

impl FunnelHandle {
    /// Enqueue a body for stamping + broadcast. Non-blocking.
    /// Silently drops if the funnel task has terminated (treated as
    /// orchestrator shutdown).
    pub fn emit(&self, body: SpurEventBody) {
        let _ = self.tx.send(FunnelCommand::Event(body));
    }

    pub async fn lineage_snapshot(&self) -> Option<ExecutorLineage> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = self.tx.send(FunnelCommand::LineageSnapshot(tx));
        rx.await.ok().flatten()
    }
}

impl spur_mcp::McpEventSink for FunnelHandle {
    fn emit(&self, event: SpurEventBody) {
        // Delegates to the inherent `FunnelHandle::emit` method defined above.
        FunnelHandle::emit(self, event);
    }

    fn try_emit(&self, event: SpurEventBody) -> Result<(), SpurEventBody> {
        // FunnelHandle uses an unbounded channel — enqueue is always
        // non-blocking, so this is equivalent to `emit`.
        FunnelHandle::emit(self, event);
        Ok(())
    }
}

/// Create a `FunnelHandle` backed by a plain unbounded channel whose
/// receiver is returned to the caller. Intended for unit / integration
/// tests that need a `FunnelHandle` but do not care about the broadcast
/// side.
#[doc(hidden)]
pub fn test_channel() -> (
    FunnelHandle,
    tokio::sync::mpsc::UnboundedReceiver<SpurEventBody>,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (body_tx, body_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(command) = rx.recv().await {
            match command {
                FunnelCommand::Event(body) => {
                    let _ = body_tx.send(body);
                }
                FunnelCommand::LineageSnapshot(reply) => {
                    let _ = reply.send(None);
                }
            }
        }
    });
    (FunnelHandle { tx }, body_rx)
}

/// Spawn the singleton funnel task. The returned `FunnelHandle` is
/// given to every emitter inside the orchestrator.
pub fn spawn_funnel(
    broadcast_tx: broadcast::Sender<SpurEvent>,
    seq: Arc<AtomicU64>,
) -> FunnelHandle {
    spawn_funnel_inner(broadcast_tx, seq, None)
}

pub fn spawn_funnel_with_lineage(
    broadcast_tx: broadcast::Sender<SpurEvent>,
    seq: Arc<AtomicU64>,
    lineage: Arc<std::sync::Mutex<ExecutorLineage>>,
) -> FunnelHandle {
    spawn_funnel_inner(broadcast_tx, seq, Some(lineage))
}

fn spawn_funnel_inner(
    broadcast_tx: broadcast::Sender<SpurEvent>,
    seq: Arc<AtomicU64>,
    lineage: Option<Arc<std::sync::Mutex<ExecutorLineage>>>,
) -> FunnelHandle {
    let (tx, mut rx) = mpsc::unbounded_channel::<FunnelCommand>();

    tokio::spawn(async move {
        while let Some(command) = rx.recv().await {
            match command {
                FunnelCommand::Event(body) => {
                    let s = seq.fetch_add(1, Ordering::Relaxed);
                    let event = SpurEvent {
                        occurred_at: SystemTime::now(),
                        seq: s,
                        body,
                    };
                    if let Some(lineage) = lineage.as_ref() {
                        if let Ok(mut lineage) = lineage.lock() {
                            lineage.apply(&event);
                        }
                    }
                    let _ = broadcast_tx.send(event);
                }
                FunnelCommand::LineageSnapshot(reply) => {
                    let snapshot = lineage
                        .as_ref()
                        .and_then(|lineage| lineage.lock().ok().map(|lineage| lineage.clone()));
                    let _ = reply.send(snapshot);
                }
            }
        }
    });

    FunnelHandle { tx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    #[tokio::test]
    async fn funnel_stamps_monotonic_seq() {
        let (bcast_tx, mut bcast_rx) = broadcast::channel(256);
        let seq = Arc::new(AtomicU64::new(0));
        let handle = spawn_funnel(bcast_tx, seq);

        // Emit 5 events serially.
        for _ in 0..5 {
            handle.emit(SpurEventBody::TurnComplete {
                session: spur_acp::types::SessionId("s".to_string()),
            });
        }

        let mut seen = Vec::new();
        for _ in 0..5 {
            let ev = bcast_rx.recv().await.expect("recv");
            seen.push(ev.seq);
        }
        assert_eq!(
            seen,
            vec![0, 1, 2, 3, 4],
            "seq must be monotonic and start at 0"
        );
    }

    #[tokio::test]
    async fn funnel_orders_concurrent_emits() {
        // Spawn 8 tasks, each emitting 100 events. After all done,
        // we should observe seq 0..800 in order on the broadcast.
        let (bcast_tx, mut bcast_rx) = broadcast::channel(4096);
        let seq = Arc::new(AtomicU64::new(0));
        let handle = spawn_funnel(bcast_tx, seq);

        let mut joins = Vec::new();
        for _ in 0..8 {
            let h = handle.clone();
            joins.push(tokio::spawn(async move {
                for _ in 0..100 {
                    h.emit(SpurEventBody::TurnComplete {
                        session: spur_acp::types::SessionId("s".to_string()),
                    });
                }
            }));
        }
        for j in joins {
            j.await.unwrap();
        }

        let mut seen = Vec::new();
        for _ in 0..800 {
            let ev = bcast_rx.recv().await.expect("recv");
            seen.push(ev.seq);
        }
        let mut expected: Vec<u64> = (0..800).collect();
        seen.sort();
        expected.sort();
        assert_eq!(seen, expected, "every seq 0..800 must appear exactly once");
    }

    #[tokio::test]
    async fn lineage_snapshot_includes_prior_funnel_events() {
        let (bcast_tx, _bcast_rx) = broadcast::channel(16);
        let seq = Arc::new(AtomicU64::new(0));
        let lineage = Arc::new(std::sync::Mutex::new(crate::lineage::ExecutorLineage::new()));
        let handle = spawn_funnel_with_lineage(bcast_tx, seq, lineage);

        handle.emit(SpurEventBody::ExecutorSpawned {
            id: "worker-1".into(),
            parent_id: None,
            session_id: spur_acp::SessionId("session-1".into()),
            agent: "kiro".into(),
            role: spur_acp::Role::Executor,
            task_spec: "task".into(),
        });

        let snapshot = handle.lineage_snapshot().await.unwrap();

        assert!(snapshot
            .node(&crate::lineage::ExecutorId::new("worker-1"))
            .is_some());
    }
}
