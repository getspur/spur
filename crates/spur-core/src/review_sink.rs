use std::collections::HashMap;
use std::sync::Arc;

use spur_acp::ReviewDecision;
use tokio::sync::{oneshot, Mutex};

use crate::ExecutorId;

/// Routes TUI `ReviewDecision`s back to the orchestrator task that is
/// awaiting one for a specific `(executor_id, attempt_n)`.
///
/// **Ordering invariant**: the orchestrator MUST call `register` and
/// receive an `oneshot::Receiver` BEFORE emitting
/// `ExecutorReviewRequested`. This guarantees the TUI's
/// `SubmitReview` response can always find the matching sender. A
/// `SubmitReview` that arrives with no registered entry (late after
/// timeout, late after brain-cancel) is dropped with a debug log —
/// this is an expected race, not an error.
///
/// Internally a map `ExecutorId → (attempt_n, oneshot::Sender)`. The
/// attempt_n guard prevents a stale decision (for a superseded
/// attempt) from delivering to the sender registered for the next
/// attempt.
///
/// `ReviewSink` is a newtype over `Arc<Mutex<_>>`; `Clone` is cheap
/// and yields another handle to the same sink.
type SinkMap = HashMap<ExecutorId, (u32, oneshot::Sender<ReviewDecision>)>;

#[derive(Clone)]
pub struct ReviewSink {
    inner: Arc<Mutex<SinkMap>>,
}

impl ReviewSink {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a pending review. Returns the receiver the caller awaits.
    /// Errors if an entry already exists for this executor_id.
    pub async fn register(
        &self,
        executor_id: ExecutorId,
        attempt_n: u32,
    ) -> Result<oneshot::Receiver<ReviewDecision>, ReviewSinkError> {
        let (tx, rx) = oneshot::channel();
        let mut map = self.inner.lock().await;
        if map.contains_key(&executor_id) {
            return Err(ReviewSinkError::AlreadyRegistered);
        }
        map.insert(executor_id, (attempt_n, tx));
        Ok(rx)
    }

    /// Submit a decision. Returns true if routed, false if dropped
    /// (unknown executor_id or attempt_n mismatch).
    ///
    /// The "unknown executor_id" path is logged at `debug!` because it
    /// fires on legitimate races (timeout-then-late-submit,
    /// brain-cancel-then-late-submit). The "attempt_n mismatch" path is
    /// logged at `warn!` because it indicates a TUI sent a decision for
    /// a superseded attempt — the operator likely clicked on a stale
    /// review card.
    pub async fn submit(
        &self,
        executor_id: ExecutorId,
        attempt_n: u32,
        decision: ReviewDecision,
    ) -> bool {
        let mut map = self.inner.lock().await;
        let stored = match map.get(&executor_id) {
            Some((n, _)) => *n,
            None => {
                tracing::debug!(
                    executor_id = %executor_id.0,
                    "review decision dropped — no pending review registered"
                );
                return false;
            }
        };
        if stored != attempt_n {
            tracing::warn!(
                executor_id = %executor_id.0,
                got = attempt_n,
                expected = stored,
                "review decision dropped — attempt_n mismatch"
            );
            return false;
        }
        let (_, tx) = map.remove(&executor_id).expect("present per check above");
        tx.send(decision).is_ok()
    }

    /// Explicitly remove a pending review (used by timeout and
    /// brain-cancellation paths to avoid stale entries).
    pub async fn remove(&self, executor_id: &ExecutorId) {
        self.inner.lock().await.remove(executor_id);
    }
}

impl Default for ReviewSink {
    fn default() -> Self {
        Self::new()
    }
}

/// INV-4: only a registered review slot can emit
/// `ExecutorReviewRequested`. Construction goes exclusively through
/// `ReviewSink::register_handle`.
pub struct ReviewHandle {
    eid: ExecutorId,
    attempt_n: u32,
    rx: oneshot::Receiver<ReviewDecision>,
}

impl ReviewHandle {
    /// Emit `ExecutorReviewRequested` for this registered review.
    /// Takes the funnel by shared reference — the handle does NOT own it.
    pub fn emit_requested(
        &self,
        funnel: &crate::event_funnel::FunnelHandle,
        kind: spur_acp::ReviewKind,
        payload: spur_acp::ReviewPayload,
    ) {
        funnel.emit(spur_acp::SpurEventBody::ExecutorReviewRequested {
            id: self.eid.0.clone(),
            attempt_n: self.attempt_n,
            kind,
            payload,
        });
    }

    pub fn executor_id(&self) -> &ExecutorId {
        &self.eid
    }

    pub fn attempt_n(&self) -> u32 {
        self.attempt_n
    }

    /// Consume the handle and yield the receiver for the caller to
    /// `select!` on. After this call, the handle is gone — so no further
    /// `emit_requested` can fire for the same registration.
    pub fn into_rx(self) -> oneshot::Receiver<ReviewDecision> {
        self.rx
    }
}

impl ReviewSink {
    /// INV-4: register a pending review and return a `ReviewHandle` that
    /// is the ONLY way to emit `ExecutorReviewRequested` for this slot.
    /// Errors if an entry already exists for this executor_id.
    pub async fn register_handle(
        &self,
        eid: ExecutorId,
        attempt_n: u32,
    ) -> Result<ReviewHandle, ReviewSinkError> {
        let rx = self.register(eid.clone(), attempt_n).await?;
        Ok(ReviewHandle { eid, attempt_n, rx })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReviewSinkError {
    #[error("a review is already registered for this executor_id")]
    AlreadyRegistered,
}
