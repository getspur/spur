//! Public shims for integration tests. Not part of the stable API.
use spur_acp::DiffSummary;

pub struct RetryAttemptPublic {
    pub attempt_n: u32,
    pub summary: String,
    pub diff_summary: Option<DiffSummary>,
    pub feedback: String,
}

pub fn render_retry_context_public(
    history: &[RetryAttemptPublic],
    original_task: &str,
    current_feedback: &str,
) -> String {
    let internal: Vec<super::RetryAttempt> = history
        .iter()
        .map(|a| super::RetryAttempt {
            attempt_n: a.attempt_n,
            summary: a.summary.clone(),
            diff_summary: a.diff_summary.clone(),
            feedback: a.feedback.clone(),
        })
        .collect();
    super::render_retry_context(&internal, original_task, current_feedback)
}

// ─── Review gate helpers ──────────────────────────────────────────
// Test-only. Production code uses ReviewSink::register_handle (INV-4).

use super::{
    apply_decision_to_candidate, DelegationStatus, ExecutorId, ReviewSink, TimeoutFallback,
};
use crate::review_sink::ReviewSinkError;

/// Register a pending review on the sink. Returns the receiver the
/// caller awaits.
///
/// **Test-only** — production code uses `ReviewSink::register_handle` (INV-4).
pub async fn register_gate(
    executor_id: ExecutorId,
    attempt_n: u32,
    review_sink: &ReviewSink,
) -> Result<tokio::sync::oneshot::Receiver<spur_acp::ReviewDecision>, ReviewSinkError> {
    review_sink.register(executor_id, attempt_n).await
}

/// Wait for a review decision (or timeout) and shape the final
/// `DelegationStatus`.
///
/// **Does NOT handle `Retry`** — returns `Failed` if Retry arrives.
///
/// **Test-only** — production code uses `ReviewHandle::into_rx` (INV-4).
pub async fn wait_gate(
    rx: tokio::sync::oneshot::Receiver<spur_acp::ReviewDecision>,
    executor_id: ExecutorId,
    candidate_status: DelegationStatus,
    review_timeout: std::time::Duration,
    timeout_fallback: TimeoutFallback,
    review_sink: ReviewSink,
) -> DelegationStatus {
    tokio::select! {
        recv_result = rx => {
            match recv_result {
                Ok(decision) => apply_decision_to_candidate(decision, candidate_status),
                Err(_) => {
                    review_sink.remove(&executor_id).await;
                    DelegationStatus::TimedOut {
                        waited_for: review_timeout,
                        fallback: timeout_fallback,
                    }
                }
            }
        }
        _ = tokio::time::sleep(review_timeout) => {
            review_sink.remove(&executor_id).await;
            DelegationStatus::TimedOut {
                waited_for: review_timeout,
                fallback: timeout_fallback,
            }
        }
    }
}

/// Register + wait composition.
///
/// **Test-only** — production code uses `ReviewSink::register_handle` (INV-4).
pub async fn run_gate_for_candidate(
    executor_id: ExecutorId,
    attempt_n: u32,
    candidate_status: DelegationStatus,
    review_timeout: std::time::Duration,
    timeout_fallback: TimeoutFallback,
    review_sink: ReviewSink,
) -> DelegationStatus {
    let rx = match register_gate(executor_id.clone(), attempt_n, &review_sink).await {
        Ok(rx) => rx,
        Err(e) => {
            tracing::error!(
                executor_id = %executor_id.0,
                error = %e,
                "review_sink registration failed"
            );
            return DelegationStatus::Failed {
                error: format!("review registration failed: {e}"),
            };
        }
    };
    wait_gate(
        rx,
        executor_id,
        candidate_status,
        review_timeout,
        timeout_fallback,
        review_sink,
    )
    .await
}

// ─── MCP shutdown helpers ─────────────────────────────────────────
// Test-only. Expose the private `shutdown_mcp_server` function and
// its dependencies so integration tests can call them directly.

use std::sync::Arc;
use tokio_util::task::AbortOnDropHandle;

/// Mirror of the private `RetirableMcpServer` trait for integration
/// tests. Implement this on fake servers to drive `shutdown_mcp_server`.
///
/// **Test-only.**
pub trait RetirableMcpServer: Send + Sync {
    fn mark_retiring(&self);
    fn cancel_in_flight_workers(&self);
    fn force_abort(&self);
    fn shutdown(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>>;
}

/// Adapts the public `test_support::RetirableMcpServer` trait to the
/// private `super::RetirableMcpServer` trait.
struct RetirableMcpServerAdapter<S: RetirableMcpServer + ?Sized>(Arc<S>);

impl<S: RetirableMcpServer + ?Sized> super::session::RetirableMcpServer
    for RetirableMcpServerAdapter<S>
{
    fn mark_retiring(&self) {
        self.0.mark_retiring();
    }
    fn cancel_in_flight_workers(&self) {
        self.0.cancel_in_flight_workers();
    }
    fn force_abort(&self) {
        self.0.force_abort();
    }
    fn shutdown(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        self.0.shutdown()
    }
}

/// The MCP shutdown timeout constant (5 s).
///
/// **Test-only** — used by `shutdown_mcp_server_bounded` to set the
/// assertion epsilon.
#[doc(hidden)]
pub const MCP_SHUTDOWN_TIMEOUT_MS: u64 = super::session::MCP_SHUTDOWN_TIMEOUT.as_millis() as u64;

/// Call `shutdown_mcp_server` with a fake `RetirableMcpServer`.
///
/// **Test-only.**
pub async fn call_shutdown_mcp_server<S: RetirableMcpServer + ?Sized>(
    funnel: &crate::event_funnel::FunnelHandle,
    session: &spur_acp::types::SessionId,
    mcp_server: Option<Arc<S>>,
    mcp_guard: Option<AbortOnDropHandle<()>>,
) {
    // Wrap the public-trait server in the adapter so it satisfies
    // the private `super::RetirableMcpServer` bound.
    let mut adapted: Option<Arc<dyn super::session::RetirableMcpServer>> = mcp_server.map(|s| {
        Arc::new(RetirableMcpServerAdapter(s)) as Arc<dyn super::session::RetirableMcpServer>
    });
    let mut guard_slot: Option<AbortOnDropHandle<()>> = mcp_guard;
    super::session::shutdown_mcp_server(funnel, session, &mut adapted, Some(&mut guard_slot)).await;
}

/// Wraps `register_gate` + `wait_gate` in a retry loop.
///
/// On `Retry`, bumps `attempt_n` and re-enters. Bounded by
/// `max_review_retries`.
///
/// Uses `crate::retry_loop::RetryLoop` for the bound check and
/// exhaustion status — shares invariants with the production retry
/// gate in `execute_delegation`.
///
/// **Test-only** — production code uses `ReviewSink::register_handle` (INV-4).
pub async fn run_gate_with_retries(
    executor_id: ExecutorId,
    candidate_status: DelegationStatus,
    review_timeout: std::time::Duration,
    timeout_fallback: TimeoutFallback,
    max_review_retries: u32,
    review_sink: ReviewSink,
) -> DelegationStatus {
    use crate::retry_loop::{RetryLoop, RetryOutcome};
    use spur_acp::ReviewDecision;

    RetryLoop::new(max_review_retries)
        .run(|attempt_n| {
            let executor_id = executor_id.clone();
            let review_sink = review_sink.clone();
            let candidate_status = candidate_status.clone();
            let timeout_fallback = timeout_fallback.clone();
            async move {
                let rx = match register_gate(executor_id.clone(), attempt_n, &review_sink).await {
                    Ok(rx) => rx,
                    Err(e) => {
                        return RetryOutcome::Terminal(DelegationStatus::Failed {
                            error: format!("review registration failed: {e}"),
                        });
                    }
                };

                let decision = tokio::select! {
                    r = rx => r.ok(),
                    _ = tokio::time::sleep(review_timeout) => {
                        review_sink.remove(&executor_id).await;
                        return RetryOutcome::Terminal(DelegationStatus::TimedOut {
                            waited_for: review_timeout,
                            fallback: timeout_fallback,
                        });
                    }
                };

                match decision {
                    Some(ReviewDecision::Approve) => RetryOutcome::Terminal(candidate_status),
                    Some(ReviewDecision::Reject { reason }) => {
                        RetryOutcome::Terminal(DelegationStatus::Rejected { reason })
                    }
                    Some(ReviewDecision::Modify { note }) => {
                        RetryOutcome::Terminal(DelegationStatus::Modified {
                            reviewer_note: note,
                        })
                    }
                    Some(ReviewDecision::Retry { .. }) => RetryOutcome::Retry,
                    None => {
                        review_sink.remove(&executor_id).await;
                        RetryOutcome::Terminal(DelegationStatus::TimedOut {
                            waited_for: review_timeout,
                            fallback: timeout_fallback,
                        })
                    }
                }
            }
        })
        .await
}
