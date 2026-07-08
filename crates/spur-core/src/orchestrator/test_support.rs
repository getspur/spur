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

// ─── Worker-attempt helpers ───────────────────────────────────────
// Test-only. Expose the private worker-attempt path to integration tests with
// a caller-supplied AgentConnection, avoiding real vendor process spawns.

pub type WorkerConnectionFactoryForTest<'a> = dyn Fn(
        &spur_acp::config::AgentConfig,
        Vec<String>,
        &std::path::Path,
    ) -> Box<dyn spur_acp::connection::AgentConnection>
    + Send
    + Sync
    + 'a;

pub struct WorkerAttemptOutcomeForTest {
    pub worker_session: spur_acp::SessionId,
    pub candidate_status: spur_acp::DelegationStatus,
    pub diff: Option<String>,
    pub worktree_path: std::path::PathBuf,
}

pub async fn run_worker_attempt_with_connection_for_test<'a>(
    repo_root: std::path::PathBuf,
    agent_config: spur_acp::config::AgentConfig,
    profile: Option<String>,
    task: String,
    connection_factory: &'a WorkerConnectionFactoryForTest<'a>,
) -> anyhow::Result<WorkerAttemptOutcomeForTest> {
    let profile_def = match profile.as_deref() {
        Some(name) => crate::agent_profiles::AgentProfile::load(&repo_root, name)?,
        None => None,
    };
    let (model, effort) = super::delegation::execute::resolve_effective_model_effort(
        None,
        None,
        profile_def.as_ref(),
    );

    let mut worktrees = spur_worktree::manager::WorktreeManager::new(repo_root);
    let (funnel, _events_rx) = crate::event_funnel::test_channel();
    let brain_session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId::new());
    let worker_session = spur_acp::SessionId::new();
    let fault_hooks = super::FaultInjectionHooks::default();
    let feature_gate = spur_license::FeatureGate::new_with_install_id(
        spur_license::policy::PolicyResolver::embedded(),
        spur_license::InstallId::from_uuid(uuid::Uuid::nil()),
    );

    let outcome = super::delegation::run_one_worker_attempt(
        worker_session.clone(),
        super::delegation::WorkerAttemptCtx {
            brain_session_id: &brain_session_id,
            agent: agent_config.name.as_str(),
            model: model.as_deref(),
            effort: effort.as_deref(),
            profile: profile.as_deref(),
            profile_def: profile_def.as_ref(),
            skills: None,
            config_overrides: None,
            task: task.as_str(),
            request_id: "test-delegation",
            attempt: 1,
            agent_config: &agent_config,
            delegation_plan: None,
            issue_id: None,
            prior_branch_for_reuse: None,
            peer_mailbox: None,
            ack_tx: None,
            base: None,
            dispatched_base_oid_tx: None,
            fault_injection_hooks: &fault_hooks,
            worker_mcp_servers: &[],
            worker_mcp_server: None,
            pm_service: None,
            feature_gate: &feature_gate,
            connection_factory: Some(connection_factory),
        },
        &mut worktrees,
        &funnel,
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error}"))?;

    Ok(WorkerAttemptOutcomeForTest {
        worker_session: outcome.worker_session,
        candidate_status: outcome.candidate_status,
        diff: outcome.diff,
        worktree_path: outcome.worktree_path,
    })
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
