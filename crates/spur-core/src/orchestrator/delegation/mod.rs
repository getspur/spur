use super::*;

pub mod base_spec;
pub mod cleanup;
pub mod diff_artifact;
pub mod execute;
pub mod file_touch;
pub mod finalize;
pub mod peer_mailbox;
pub mod worker_attempt;

pub(crate) use cleanup::{apply_worktree_cleanup, WorktreeCleanupContext};
pub(crate) use diff_artifact::{
    build_diff_summary, decide_artifact_handling, sha256_hex_for_outcome, summary_cap_bytes,
    truncate_summary_env_default,
};
pub(crate) use execute::execute_delegation;
pub(crate) use finalize::{finalize, flush_then_emit_completed};
pub(crate) use worker_attempt::{
    format_worker_task, run_one_worker_attempt, AttemptSetupError, WorkerAttemptCtx,
};

fn maybe_spawn_dispatch_lease_heartbeat(
    pm_service: Option<Arc<PmService>>,
    issue_id: Option<String>,
    delegation_id: String,
    lease_duration: std::time::Duration,
    heartbeat_cadence: std::time::Duration,
    abort_handle: DelegationAbortHandle,
) -> Option<AbortOnDropHandle<()>> {
    let (Some(pm), Some(issue_id)) = (pm_service, issue_id) else {
        return None;
    };
    let heartbeat_cadence = if heartbeat_cadence.is_zero() {
        std::cmp::max(lease_duration / 3, std::time::Duration::from_secs(1))
    } else {
        heartbeat_cadence
    };
    Some(AbortOnDropHandle::new(tokio::spawn(async move {
        let lease_secs = i64::try_from(lease_duration.as_secs()).unwrap_or(i64::MAX);
        loop {
            tokio::select! {
                biased;
                _ = abort_handle.cancelled() => break,
                _ = tokio::time::sleep(heartbeat_cadence) => {}
            }

            let expires_at = chrono::Utc::now().timestamp().saturating_add(lease_secs);
            if let Err(error) = spur_mcp::plan::update_dispatch_lease(
                pm.as_ref(),
                &issue_id,
                &delegation_id,
                expires_at,
            )
            .await
            {
                tracing::warn!(
                    issue_id = %issue_id,
                    %delegation_id,
                    "dispatch lease heartbeat failed: {error}"
                );
            }
        }
    })))
}

/// Handle delegation requests from the MCP callback server.
///
/// Spawns each delegation as a separate tokio task, allowing multiple
/// workers to run concurrently. A semaphore limits the number of
/// simultaneous workers to `max_concurrent`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_delegations(
    mut channel: DelegationChannel,
    repo_root: PathBuf,
    agent_configs: Vec<spur_acp::config::AgentConfig>,
    max_concurrent: usize,
    worktree_config: WorktreeConfig,
    event_tx: broadcast::Sender<SpurEvent>,
    funnel: crate::event_funnel::FunnelHandle,
    review_sink: ReviewSink,
    pm_service: Option<Arc<PmService>>,
    feature_gate: Arc<spur_license::FeatureGate>,
    cancellation_control: CancellationControl,
    peer_mailbox: Option<crate::peer_mailbox::PeerMailboxBundle>,
    fault_injection_hooks: FaultInjectionHooks,
    dispatch_lease_duration: std::time::Duration,
    dispatch_lease_heartbeat: std::time::Duration,
    worker_mcp_fetcher: WorkerMcpFetcher,
    normalize_bypass_hooks: bool,
) {
    let semaphore = Arc::new(Semaphore::new(max_concurrent.max(1)));
    // Debounce: skip post-delegation refresh if another completed <3s ago.
    // Initial value is in the past so the first refresh always runs.
    let last_refresh_at = Arc::new(tokio::sync::Mutex::new(
        tokio::time::Instant::now() - std::time::Duration::from_secs(60),
    ));

    while let Some(request) = channel.request_rx.recv().await {
        // Destructure the request — it is not Clone, so we move each field.
        let DelegationRequest {
            id: request_id,
            agent,
            task,
            context_files,
            respond_to,
            brain_session_id,
            delegation_plan,
            issue_id,
            base,
            dispatched_base_oid_tx,
            attempt_tracker,
            enable_worker_mcp,
        } = request;
        // Phase 4: `DelegationRequest.id` is now a typed `DelegationId`
        // newtype. Downstream delegation plumbing (funnel events,
        // SessionId, PmService, log fields) still speaks plain `String`,
        // so lower the wrapper to its inner representation at the
        // orchestrator boundary rather than threading the newtype
        // through every call site.
        let request_id: String = request_id.into();

        debug!(
            agent = %agent,
            task = %task,
            "Received delegation request"
        );

        let repo_root = repo_root.clone();
        let agent_configs = agent_configs.clone();
        let semaphore = Arc::clone(&semaphore);
        let worktree_config = worktree_config.clone();
        let event_tx = event_tx.clone();
        let funnel = funnel.clone();
        let review_sink = review_sink.clone();
        let pm_service = pm_service.clone();
        let feature_gate = Arc::clone(&feature_gate);
        let last_refresh_at = Arc::clone(&last_refresh_at);
        let peer_mailbox = peer_mailbox.clone();
        let fault_injection_hooks = fault_injection_hooks.clone();
        let worker_mcp_fetcher = worker_mcp_fetcher.clone();
        // Bind the fetcher's cache for the flush helpers
        // (`flush_then_emit_completed`) which look up cached servers
        // by brain_session_id without needing the fetcher's other
        // fields.
        let worker_mcp_servers = Arc::clone(&worker_mcp_fetcher.cache);

        // INV-6: register a cancellation token BEFORE spawning so
        // cancel() arriving between dispatch and spawn still works.
        let cancel_token = {
            let cc = cancellation_control.clone();
            let (token, handle) = cc.register_with_abort_handle(request_id.clone()).await;
            (token, handle)
        };
        let (cancel_token, abort_handle) = cancel_token;
        let cancellation_control_for_task = cancellation_control.clone();

        tokio::spawn(async move {
            let mut guard = DelegationGuard {
                funnel: funnel.clone(),
                respond_to: Some(respond_to),
                request_id: request_id.clone(),
                disarmed: false,
            };

            // Acquire a permit before starting the delegation.
            let _permit = tokio::select! {
                biased;
                _ = abort_handle.cancelled() => {
                    let status = crate::delegation_watchdog::status_from_abort_reason(&abort_handle).await;
                    flush_then_emit_completed(
                        &funnel,
                        &worker_mcp_servers,
                        pm_service.as_ref(),
                        &brain_session_id,
                        &request_id,
                        issue_id.as_deref(),
                        spur_acp::types::SessionId(request_id.clone()),
                        &status,
                    )
                    .await;
                    if let Some(respond_to) = guard.respond_to.take() {
                        let _ = respond_to.send(DelegationResult {
                            status,
                            diff: None,
                            diff_summary: None,
                            summary: None,
                            estimated_cost_usd: 0.0,
                            worker_branch: None,
                            artifact: None,
                        });
                    }
                    cancellation_control_for_task.remove(&request_id).await;
                    guard.disarmed = true;
                    return;
                }
                permit = semaphore.acquire() => match permit {
                    Ok(permit) => permit,
                    Err(_) => {
                        error!("Semaphore closed — aborting delegation");
                        // Clean up the token if we abort early.
                        cancellation_control_for_task.remove(&request_id).await;
                        return; // guard fires DelegationCompleted(Failed)
                    }
                },
            };

            let heartbeat_watchdog_stop =
                crate::delegation_watchdog::maybe_spawn_heartbeat_watchdog(
                    &worktree_config,
                    request_id.clone(),
                    abort_handle.clone(),
                    &event_tx,
                );

            // Claim issue on delegation start (10f).
            if let (Some(ref issue_id), Some(ref pm)) = (&issue_id, &pm_service) {
                let worker_name = format!("spur-worker-{}", request_id);
                if let Err(e) = pm
                    .update_issue(
                        issue_id,
                        spur_pm::IssueUpdate {
                            status: Some("in_progress".into()),
                            assignee: Some(worker_name.clone()),
                            ..Default::default()
                        },
                    )
                    .await
                {
                    tracing::warn!(issue = %issue_id, "Failed to claim issue: {e}");
                } else {
                    funnel.emit(SpurEventBody::IssueUpdated {
                        source: pm.source_str().into(),
                        id: issue_id.clone(),
                        status: Some("in_progress".into()),
                        assignee: Some(worker_name),
                    });
                }
            }

            let dispatch_lease_heartbeat_handle = maybe_spawn_dispatch_lease_heartbeat(
                pm_service.clone(),
                issue_id.clone(),
                request_id.clone(),
                dispatch_lease_duration,
                dispatch_lease_heartbeat,
                abort_handle.clone(),
            );

            // No outer timeout: the review gate's own `review_timeout`
            // bounds review waits (default 30 min, configurable per
            // agent). A previous hardcoded 300s outer timeout always
            // fired before the 1800s default review timeout, cancelling
            // the delegation mid-`select!`, dropping the ReviewSink
            // entry's receiver without emitting Resolved/TimedOut, and
            // returning `DelegationStatus::Timeout` (worker-hang) to
            // the brain. That broke the spec's worker `Timeout`
            // (hang) vs review `TimedOut` (nobody reviewed) split and
            // left the TUI stuck on `AwaitingReview` because
            // `DelegationCompleted` was never emitted for the right
            // session. v1 accepts that worker-hang detection is not
            // automatic — separate concern, separate fix.
            //
            // INV-6: race execute_delegation against the per-delegation
            // cancellation token. If cancel() arrives first, we return
            // DelegationStatus::Cancelled without waiting for the worker.
            let (result, executor_id_opt) = tokio::select! {
                biased;
                _ = cancel_token.cancelled() => {
                    let executor_id_opt = match abort_handle.observed_reason().await {
                        Some(DelegationAbortReason::WorkerHeartbeatTimeout {
                            executor_id,
                            idle_for_secs: _,
                        }) if executor_id != "<not-dispatched>" => {
                            Some(ExecutorId(executor_id))
                        }
                        Some(DelegationAbortReason::BrainRequested { reason: _ })
                        | Some(DelegationAbortReason::WorkerHeartbeatTimeout {
                            executor_id: _,
                            idle_for_secs: _,
                        })
                        | None => None,
                    };
                    let status = crate::delegation_watchdog::status_from_abort_reason(&abort_handle).await;
                    // Emit DelegationCompleted so TUI, lineage, and
                    // other funnel subscribers don't see a stale
                    // "active" entry for this delegation. Routes
                    // through `flush_then_emit_completed` so the
                    // worker MCP audit summary precedes the
                    // DelegationCompleted event (Phase 5 / Task 27).
                    flush_then_emit_completed(
                        &funnel,
                        &worker_mcp_servers,
                        pm_service.as_ref(),
                        &brain_session_id,
                        &request_id,
                        issue_id.as_deref(),
                        spur_acp::types::SessionId(request_id.clone()),
                        &status,
                    )
                    .await;
                    (
                        DelegationResult {
                            status,
                            diff: None,
                            diff_summary: None,
                            summary: None,
                            estimated_cost_usd: 0.0,
                            worker_branch: None,
                            artifact: None,
                        },
                        executor_id_opt,
                    )
                }
                r = execute_delegation(
                    agent,
                    task,
                    context_files,
                    request_id.clone(),
                    brain_session_id.clone(),
                    delegation_plan,
                    issue_id.clone(),
                    repo_root,
                    agent_configs,
                    funnel.clone(),
                    review_sink.clone(),
                    attempt_tracker,
                    peer_mailbox,
                    base,
                    dispatched_base_oid_tx,
                    fault_injection_hooks,
                    enable_worker_mcp,
                    worker_mcp_fetcher,
                    pm_service.clone(),
                    feature_gate,
                    normalize_bypass_hooks,
                ) => r,
            };
            drop(dispatch_lease_heartbeat_handle);
            drop(heartbeat_watchdog_stop);
            // Always clean up the token entry (avoids stale entries
            // when the delegation completes normally before cancel fires).
            cancellation_control_for_task.remove(&request_id).await;

            // Comment on / revert issue on completion (10g).
            if let (Some(ref issue_id), Some(ref pm)) = (&issue_id, &pm_service) {
                let (new_status, comment) = match &result.status {
                    // Success — DON'T close, just comment. Brain decides when to close.
                    DelegationStatus::Success => {
                        (None, format!("Completed by SPUR delegation {}", request_id))
                    }
                    DelegationStatus::Rejected { .. } => {
                        (Some("open"), format!("Delegation {} rejected", request_id))
                    }
                    DelegationStatus::Failed { error } => (
                        Some("open"),
                        format!("Delegation {} failed: {}", request_id, error),
                    ),
                    _ => (Some("open"), format!("Delegation {} ended", request_id)),
                };

                let update = spur_pm::IssueUpdate {
                    status: new_status.map(String::from),
                    comment: Some(comment),
                    ..Default::default()
                };

                if let Err(e) = pm.update_issue(issue_id, update).await {
                    tracing::warn!(issue = %issue_id, "Failed to transition issue: {e}");
                } else if let Some(status) = new_status {
                    funnel.emit(SpurEventBody::IssueUpdated {
                        source: pm.source_str().into(),
                        id: issue_id.clone(),
                        status: Some(status.into()),
                        assignee: None,
                    });
                }
            }

            // Refresh issue list + graph alerts after delegation completes
            // so TUI picks up changes made by the worker (F19).
            // Debounce: skip if another delegation refreshed <3s ago
            // (prevents thundering herd from delegate_parallel).
            if let Some(ref pm) = pm_service {
                let mut last = last_refresh_at.lock().await;
                if last.elapsed() >= std::time::Duration::from_secs(3) {
                    *last = tokio::time::Instant::now();
                    drop(last); // release lock before async work
                    refresh_pm_state(pm, &funnel, Some(1000), false).await;
                } else {
                    tracing::debug!("Skipping post-delegation refresh (debounced)");
                }
            }

            // Normal path: disarm the guard and send result manually.
            guard.disarmed = true;
            let respond_to = guard.respond_to.take().unwrap();

            if let Err(_returned_result) = respond_to.send(result) {
                // Brain's MCP tool call was cancelled — the oneshot
                // receiver was dropped before we could deliver the
                // result. If a review was still pending on this
                // delegation, emit an audit event so the lineage
                // projection records the abandonment rather than
                // leaving an orphaned review card indefinitely.
                if let Some(ref eid) = executor_id_opt {
                    cleanup_cancelled_review(eid, "brain call cancelled", &funnel, &review_sink)
                        .await;
                }
            }
        });
    }
}

/// RAII guard that ensures every delegation emits `DelegationCompleted`
/// even on early-exit or task abort. Disarmed by the normal completion
/// path; fires on Drop otherwise.
struct DelegationGuard {
    funnel: crate::event_funnel::FunnelHandle,
    respond_to: Option<tokio::sync::oneshot::Sender<DelegationResult>>,
    request_id: String,
    disarmed: bool,
}

impl Drop for DelegationGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        error!(
            request_id = %self.request_id,
            "DelegationGuard fired — emitting DelegationCompleted(Failed)"
        );
        self.funnel.emit(SpurEventBody::DelegationCompleted {
            worker_session: SessionId(self.request_id.clone()),
            status: DelegationStatus::Failed {
                error: "delegation aborted (early exit or task cancelled)".into(),
            },
        });
        if let Some(tx) = self.respond_to.take() {
            let _ = tx.send(DelegationResult {
                status: DelegationStatus::Failed {
                    error: "delegation aborted".into(),
                },
                diff: None,
                diff_summary: None,
                summary: None,
                estimated_cost_usd: 0.0,
                worker_branch: None,
                artifact: None,
            });
        }
    }
}
