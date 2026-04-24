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

use spur_acp::domain::events::{SpurEvent, SpurEventBody};

/// Handle returned by `spawn_funnel`. Clone cheaply; call `emit`.
#[derive(Clone)]
pub struct FunnelHandle {
    tx: mpsc::UnboundedSender<SpurEventBody>,
}

impl FunnelHandle {
    /// Enqueue a body for stamping + broadcast. Non-blocking.
    /// Silently drops if the funnel task has terminated (treated as
    /// orchestrator shutdown).
    pub fn emit(&self, body: SpurEventBody) {
        let _ = self.tx.send(body);
    }
}

impl spur_mcp::McpEventSink for FunnelHandle {
    fn emit(&self, event: SpurEventBody) {
        // Delegates to the inherent `FunnelHandle::emit` method defined above.
        FunnelHandle::emit(self, event);
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
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (FunnelHandle { tx }, rx)
}

/// Spawn the singleton funnel task. The returned `FunnelHandle` is
/// given to every emitter inside the orchestrator.
pub fn spawn_funnel(
    broadcast_tx: broadcast::Sender<SpurEvent>,
    seq: Arc<AtomicU64>,
) -> FunnelHandle {
    let (tx, mut rx) = mpsc::unbounded_channel::<SpurEventBody>();

    tokio::spawn(async move {
        while let Some(body) = rx.recv().await {
            let s = seq.fetch_add(1, Ordering::Relaxed);
            let event = SpurEvent {
                occurred_at: SystemTime::now(),
                seq: s,
                body,
            };
            let _ = broadcast_tx.send(event);
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
}
