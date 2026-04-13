use std::collections::HashMap;
use std::sync::Arc;

use spur_acp::ReviewDecision;
use tokio::sync::{oneshot, Mutex};

use crate::ExecutorId;

/// Routes TUI `ReviewDecision`s back to the orchestrator task that is
/// awaiting one for a specific `(executor_id, attempt_n)`.
///
/// Internally a map `ExecutorId → (attempt_n, oneshot::Sender)`. The
/// attempt_n guard prevents a stale decision (e.g., for a superseded
/// attempt) from delivering to the sender registered for the next
/// attempt.
pub struct ReviewSink {
    inner: Arc<Mutex<HashMap<ExecutorId, (u32, oneshot::Sender<ReviewDecision>)>>>,
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
    pub async fn submit(
        &self,
        executor_id: ExecutorId,
        attempt_n: u32,
        decision: ReviewDecision,
    ) -> bool {
        let mut map = self.inner.lock().await;
        match map.get(&executor_id) {
            Some((stored, _)) if *stored != attempt_n => {
                tracing::warn!(
                    executor_id = %executor_id.0,
                    got = attempt_n,
                    expected = *stored,
                    "review decision dropped — attempt_n mismatch"
                );
                false
            }
            Some(_) => {
                // attempt_n matches — pop and send.
                let (_, tx) = map.remove(&executor_id).expect("checked above");
                tx.send(decision).is_ok()
            }
            None => {
                tracing::warn!(
                    executor_id = %executor_id.0,
                    "review decision dropped — no pending review registered"
                );
                false
            }
        }
    }

    /// Explicitly remove a pending review (used by timeout and
    /// brain-cancellation paths to avoid stale entries).
    pub async fn remove(&self, executor_id: &ExecutorId) {
        self.inner.lock().await.remove(executor_id);
    }

    pub fn share(&self) -> Arc<Self> {
        // `ReviewSink` itself holds an `Arc<Mutex<_>>`; callers clone via Arc.
        Arc::new(Self {
            inner: Arc::clone(&self.inner),
        })
    }
}

impl Default for ReviewSink {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReviewSinkError {
    #[error("a review is already registered for this executor_id")]
    AlreadyRegistered,
}
