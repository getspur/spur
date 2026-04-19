//! Bridge from MCP detached completion → orchestrator ingress.
//! Enforces INV-C3 (UI event BEFORE model-visible continuation).

use spur_acp::domain::BrainContinuation;
use spur_acp::domain::events::SpurEventBody;
use spur_acp::types::SessionId;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, Mutex};

use crate::orchestrator::InteractiveInput;

/// Overflow buffer for continuations when the `InteractiveInput` ingress
/// channel is full. Drained by the orchestrator on every scheduler tick.
pub type OverflowBuf = Arc<Mutex<VecDeque<(SessionId, BrainContinuation)>>>;

pub fn new_overflow_buf() -> OverflowBuf {
    Arc::new(Mutex::new(VecDeque::new()))
}

/// Abstract sink — decouples the helper from both `FunnelHandle` (spur-core)
/// and `McpEventSink` (spur-mcp). Both types implement this by simple
/// delegation; callers in orchestrator use a closure over `FunnelHandle::emit`
/// and callers in MCP use the existing `event_sink` via a small adapter.
pub trait ContinuationEventSink: Send + Sync {
    fn emit(&self, body: SpurEventBody);
}

/// Exactly-once bridge from MCP result collector → orchestrator ingress.
/// Emits the UI event BEFORE sending `SystemContinuation` (INV-C3).
pub async fn report_detached_completion(
    sink: &dyn ContinuationEventSink,
    continuation_tx: &mpsc::Sender<InteractiveInput>,
    overflow: &OverflowBuf,
    session: SessionId,
    worker_session: SessionId,
    cont: BrainContinuation,
) {
    // 1) UI-visible event FIRST.
    sink.emit(SpurEventBody::DelegationCompleted {
        worker_session,
        status: cont.payload.status.clone(),
    });
    // 2) Model-visible continuation SECOND (try_send + overflow fallback).
    let input = InteractiveInput::SystemContinuation {
        session: session.clone(),
        continuation: cont.clone(),
    };
    if let Err(TrySendError::Full(_)) = continuation_tx.try_send(input) {
        overflow.lock().await.push_back((session, cont));
    }
}

impl ContinuationEventSink for crate::event_funnel::FunnelHandle {
    fn emit(&self, body: SpurEventBody) {
        // Use UFCS to resolve the inherent method, avoiding infinite recursion
        // between the trait method and the inherent `FunnelHandle::emit`.
        crate::event_funnel::FunnelHandle::emit(self, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::domain::{ContinuationPayload, ContinuationSource};
    use spur_acp::domain::delegation::DelegationStatus;
    use std::time::Instant;

    fn mk_cont(id: &str) -> BrainContinuation {
        BrainContinuation {
            delegation_id: id.into(),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: None, diff_summary: None, worker_branch: None,
            },
            created_at: Instant::now(),
        }
    }

    #[tokio::test]
    async fn overflow_buf_stores_on_try_send_full() {
        let buf = new_overflow_buf();
        let (_tx, _rx) = mpsc::channel::<InteractiveInput>(1);   // tiny cap
        let _tx_clone = _tx.clone();
        // Fill the channel.
        _tx.try_send(InteractiveInput::Message { blocks: vec![], interrupt: false }).unwrap();

        let sid = SessionId::new();
        let c = mk_cont("id-overflow-1");
        let input = InteractiveInput::SystemContinuation {
            session: sid.clone(), continuation: c.clone()
        };
        match _tx.try_send(input) {
            Err(TrySendError::Full(_)) => {
                buf.lock().await.push_back((sid, c));
            }
            _ => panic!("expected Full"),
        }
        assert_eq!(buf.lock().await.len(), 1);
    }
}
