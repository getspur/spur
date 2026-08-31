use super::peer_mailbox::drain_peer_acks_with_timeout;
use super::*;
use crate::orchestrator::worker_mcp::resolve_worker_mcp_enabled;

pub(crate) fn resolve_effective_model_effort(
    request_model: Option<&str>,
    request_effort: Option<&str>,
    profile_def: Option<&crate::agent_profiles::AgentProfile>,
) -> (Option<String>, Option<String>) {
    let effective_model = request_model
        .map(str::to_owned)
        .or_else(|| profile_def.and_then(|profile| profile.model.clone()));
    let effective_effort = request_effort
        .map(str::to_owned)
        .or_else(|| profile_def.and_then(|profile| profile.effort.clone()));
    (effective_model, effective_effort)
}

fn bounded_inline_summary(summary: Option<&str>) -> String {
    summary
        .map(truncate_summary_env_default)
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
async fn finalize_worker_outcome(
    funnel: &crate::event_funnel::FunnelHandle,
    worker_mcp_servers: &Arc<DashMap<spur_acp::BrainSessionId, Arc<WorkerMcpServer>>>,
    pm_service: Option<&Arc<PmService>>,
    brain_session_id: &spur_acp::BrainSessionId,
    delegation_id: &str,
    issue_id: Option<&str>,
    outcome: WorkerAttemptOutcome,
    final_status: DelegationStatus,
    total_cost: f64,
    worker_branch: Option<String>,
    normalization_warning: Option<String>,
) -> DelegationResult {
    finalize(
        funnel,
        worker_mcp_servers,
        pm_service,
        brain_session_id,
        delegation_id,
        issue_id,
        outcome.worker_session,
        final_status,
        outcome.diff,
        outcome.diff_summary,
        outcome.summary,
        total_cost,
        worker_branch,
        outcome.artifact,
        normalization_warning,
        Some(outcome.resolved_config),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_delegation(
    agent: String,
    profile: Option<String>,
    skills: Option<Vec<String>>,
    model: Option<String>,
    effort: Option<String>,
    config_overrides: Option<std::collections::HashMap<String, String>>,
    original_task: String,
    context_files: Vec<String>,
    prior_branch_for_reuse: Option<String>,
    request_id: String,
    brain_session_id: spur_acp::BrainSessionId,
    delegation_plan: Option<spur_acp::domain::DelegationPlan>,
    issue_id: Option<String>,
    repo_root: PathBuf,
    agent_configs: Vec<spur_acp::config::AgentConfig>,
    funnel: crate::event_funnel::FunnelHandle,
    review_sink: ReviewSink,
    attempt_tracker: Arc<std::sync::atomic::AtomicU32>,
    peer_mailbox: Option<crate::peer_mailbox::PeerMailboxBundle>,
    base: Option<BaseSpec>,
    dispatched_base_oid_tx: Option<tokio::sync::watch::Sender<Option<String>>>,
    fault_injection_hooks: FaultInjectionHooks,
    enable_worker_mcp: Option<bool>,
    worker_mcp_default: bool,
    worker_mcp_fetcher: WorkerMcpFetcher,
    pm_service: Option<Arc<PmService>>,
    feature_gate: Arc<spur_license::FeatureGate>,
    normalize_bypass_hooks: bool,
) -> (DelegationResult, Option<ExecutorId>) {
    // Bind cache for the flush helpers (`finalize` + abort path)
    // which look up cached servers by brain_session_id without
    // needing the fetcher's other fields.
    let worker_mcp_servers: Arc<DashMap<spur_acp::BrainSessionId, Arc<WorkerMcpServer>>> =
        Arc::clone(&worker_mcp_fetcher.cache);
    // Shadow `original_task` with the Relevant Files-prepended form
    // so retry loops at orchestrator.rs:3013 reuse the formatted
    // base. No-op when context_files is empty.
    let original_task = format_worker_task(&original_task, &context_files);
    // `__`-prefixed agent names are reserved for internal operations.
    // __cancel_delegation no longer routes through this path (INV-6 —
    // cancellation now goes through CancellationControl). Any other
    // `__`-prefixed name is an unsupported internal operation.
    if agent.starts_with("__") {
        return (
            DelegationResult {
                resolved_config: None,
                status: DelegationStatus::Failed {
                    error: format!("Unsupported internal operation: {agent}"),
                },
                diff: None,
                diff_summary: None,
                summary: None,
                estimated_cost_usd: 0.0,
                worker_branch: None,
                artifact: None,
            },
            None,
        );
    }

    let registry = AgentRegistry::load(agent_configs);

    let agent_config = match registry.get(&agent) {
        Some(c) => c.clone(),
        None => {
            return (
                DelegationResult {
                    resolved_config: None,
                    status: DelegationStatus::Failed {
                        error: format!("Worker agent '{}' not found", agent),
                    },
                    diff: None,
                    diff_summary: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                    artifact: None,
                },
                None,
            );
        }
    };

    let profile_def = match profile.as_deref() {
        Some(name) => match crate::agent_profiles::AgentProfile::load(&repo_root, name) {
            Ok(profile_def) => profile_def,
            Err(error) => {
                return (
                    DelegationResult {
                        resolved_config: None,
                        status: DelegationStatus::Failed {
                            error: format!("Failed to load worker profile '{name}': {error:#}"),
                        },
                        diff: None,
                        diff_summary: None,
                        summary: None,
                        estimated_cost_usd: 0.0,
                        worker_branch: None,
                        artifact: None,
                    },
                    None,
                );
            }
        },
        None => None,
    };
    let (effective_model, effective_effort) =
        resolve_effective_model_effort(model.as_deref(), effort.as_deref(), profile_def.as_ref());

    // Phase 5 / Task 26 — resolve the worker `mcp_servers` vec ONCE per
    // delegation. An explicit per-delegation setting takes precedence;
    // omitted settings inherit the configured built-in default.
    let enable_worker_mcp = Some(resolve_worker_mcp_enabled(
        enable_worker_mcp,
        worker_mcp_default,
    ));
    let worker_mcp_dispatch_vec = match build_worker_mcp_servers_with(enable_worker_mcp, || {
        worker_mcp_fetcher.fetch_url_token(&brain_session_id, &request_id)
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                request_id = %request_id,
                brain_session_id = %brain_session_id,
                error = %e,
                "worker MCP dispatch failed; aborting delegation"
            );
            return (
                DelegationResult {
                    resolved_config: None,
                    status: DelegationStatus::Failed {
                        error: format!("worker MCP unavailable: {e}"),
                    },
                    diff: None,
                    diff_summary: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                    artifact: None,
                },
                None,
            );
        }
    };
    let worker_mcp_server = if worker_mcp_dispatch_vec.is_empty() {
        None
    } else {
        worker_mcp_fetcher
            .cache
            .get(&brain_session_id)
            .map(|server| Arc::clone(server.value()))
    };
    let worker_mcp_tool_names = worker_mcp_server
        .as_deref()
        .map(WorkerMcpServer::claude_tool_names);

    let mut current_task = original_task.clone();
    // Retry-history accumulator. Each retry attempt pushes its
    // prior attempt's (summary, diff_summary, reviewer feedback)
    // so the NEXT attempt's prompt can reference what was tried.
    // 2 KB bloat cap drops oldest entries first.
    let mut retry_history: Vec<RetryAttempt> = Vec::new();
    let mut attempt_n: u32 = 1;
    // Stable across retries; captured from the first worker session.
    let mut executor_id: Option<ExecutorId> = None;
    // Accumulated cost across all attempts in this delegation.
    let mut total_cost: f64 = 0.0;

    // WorktreeManager owned here (not inside run_one_worker_attempt)
    // so execute_delegation can make post-gate commit/remove decisions.
    // Each delegation task gets its own manager (concurrent delegations
    // do not share mutable state). Retries reuse the same manager.
    let mut worktrees = WorktreeManager::new(repo_root);

    // Worker session for the *next* attempt. Generated here (not
    // inside run_one_worker_attempt) so the Retry arm can emit
    // ExecutorRetryStarted.new_session_id matching the session id
    // the next attempt will actually use — closing the lineage
    // Attempt.session_id ↔ worker event linkage.
    let first_worker_session = SessionId::new();
    let mut next_worker_session = first_worker_session;

    loop {
        attempt_tracker.store(attempt_n, Ordering::SeqCst);
        let (ack_tx, ack_rx) = if peer_mailbox.is_some() {
            let (ack_tx, ack_rx) = tokio::sync::mpsc::unbounded_channel();
            (Some(ack_tx), Some(ack_rx))
        } else {
            (None, None)
        };
        let outcome = match run_one_worker_attempt(
            next_worker_session.clone(),
            WorkerAttemptCtx {
                brain_session_id: &brain_session_id,
                agent: &agent,
                model: effective_model.as_deref(),
                effort: effective_effort.as_deref(),
                profile: profile.as_deref(),
                profile_def: profile_def.as_ref(),
                skills: skills.clone(),
                config_overrides: config_overrides.as_ref(),
                task: &current_task,
                request_id: &request_id,
                attempt: attempt_n,
                agent_config: &agent_config,
                delegation_plan: delegation_plan.clone(),
                issue_id: issue_id.clone(),
                prior_branch_for_reuse: prior_branch_for_reuse.clone(),
                peer_mailbox: peer_mailbox.as_ref(),
                ack_tx: ack_tx.clone(),
                base: base.clone(),
                dispatched_base_oid_tx: dispatched_base_oid_tx.clone(),
                fault_injection_hooks: &fault_injection_hooks,
                worker_mcp_servers: &worker_mcp_dispatch_vec,
                worker_mcp_server: worker_mcp_server.as_ref().map(Arc::clone),
                worker_mcp_tool_names,
                pm_service: pm_service.as_deref(),
                feature_gate: feature_gate.as_ref(),
                #[cfg(any(test, feature = "test-support"))]
                connection_factory: None,
            },
            &mut worktrees,
            &funnel,
        )
        .await
        {
            Ok(o) => o,
            Err(setup_err) => {
                // Setup failures short-circuit the entire
                // delegation without retry — retrying a
                // worktree-creation failure is not spec'd
                // behavior. We still call finalize so
                // DelegationCompleted is emitted (the worker
                // session was named, even if no worker actually
                // ran).
                let status = match setup_err {
                    AttemptSetupError::OverlayConflict {
                        source_task_id,
                        files,
                    } => DelegationStatus::SetupFailed {
                        error: spur_acp::AttemptSetupError::OverlayConflict {
                            source_task_id,
                            files,
                        },
                    },
                    AttemptSetupError::SnapshotFailed(error) => DelegationStatus::Failed {
                        error: spur_acp::AttemptSetupError::SnapshotFailed { error }.to_string(),
                    },
                    AttemptSetupError::WorktreeFailed(error) => DelegationStatus::Failed {
                        error: spur_acp::AttemptSetupError::WorktreeFailed { error }.to_string(),
                    },
                    AttemptSetupError::SkillProjectionFailed(error) => DelegationStatus::Failed {
                        error: format!("Failed to project worker skills: {error}"),
                    },
                    AttemptSetupError::InitFailed(error) => DelegationStatus::Failed {
                        error: spur_acp::AttemptSetupError::InitFailed { error }.to_string(),
                    },
                    AttemptSetupError::SessionFailed(error) => DelegationStatus::Failed {
                        error: spur_acp::AttemptSetupError::SessionFailed { error }.to_string(),
                    },
                };
                return (
                    finalize(
                        &funnel,
                        &worker_mcp_servers,
                        pm_service.as_ref(),
                        &brain_session_id,
                        &request_id,
                        issue_id.as_deref(),
                        next_worker_session,
                        status,
                        None,
                        None,
                        None,
                        total_cost,
                        None,
                        None,
                        None,
                        None,
                    )
                    .await,
                    executor_id.clone(),
                );
            }
        };

        if let Some(c) = outcome.cost {
            total_cost += c;
        }

        // On first attempt, capture executor_id from worker_session.
        if executor_id.is_none() {
            executor_id = Some(ExecutorId::new(outcome.worker_session.0.clone()));
        }
        let eid = executor_id.clone().unwrap();

        // No review gate — commit/remove then emit DelegationCompleted.
        if !agent_config.review.review_required {
            let final_status = outcome.candidate_status.clone();
            let cleanup = apply_worktree_cleanup(
                &mut worktrees,
                &outcome.worker_session,
                &final_status,
                WorktreeCleanupContext {
                    agent: &agent,
                    worktree_path: &outcome.worktree_path,
                    bypass_hooks: normalize_bypass_hooks,
                    pm_service: pm_service.as_ref(),
                    issue_id: issue_id.as_deref(),
                },
            )
            .await;
            return (
                finalize_worker_outcome(
                    &funnel,
                    &worker_mcp_servers,
                    pm_service.as_ref(),
                    &brain_session_id,
                    &request_id,
                    issue_id.as_deref(),
                    outcome,
                    final_status,
                    total_cost,
                    cleanup.worker_branch,
                    cleanup.normalization_warning,
                )
                .await,
                executor_id.clone(),
            );
        }

        // INV-4: obtain a ReviewHandle first — it is the ONLY way to
        // emit `ExecutorReviewRequested` for this slot, enforced at
        // the type level. `register_handle` wraps `ReviewSink::register`
        // so the ordering invariant (register-before-emit) is preserved.
        let handle = match review_sink.register_handle(eid.clone(), attempt_n).await {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(
                    executor_id = %eid.0,
                    attempt_n,
                    error = %e,
                    "review_sink registration failed — skipping review gate"
                );
                // Worker DID run; emit DelegationCompleted via
                // finalize so the lineage projection records the
                // terminal Failed status (preserves the
                // "every terminal emits DelegationCompleted"
                // invariant). Registration failure → Failed (not
                // preserved; no useful diff to inspect).
                let failed_status = DelegationStatus::Failed {
                    error: format!("review registration failed: {e}"),
                };
                let cleanup = apply_worktree_cleanup(
                    &mut worktrees,
                    &outcome.worker_session,
                    &failed_status,
                    WorktreeCleanupContext {
                        agent: &agent,
                        worktree_path: &outcome.worktree_path,
                        bypass_hooks: normalize_bypass_hooks,
                        pm_service: pm_service.as_ref(),
                        issue_id: issue_id.as_deref(),
                    },
                )
                .await;
                return (
                    finalize_worker_outcome(
                        &funnel,
                        &worker_mcp_servers,
                        pm_service.as_ref(),
                        &brain_session_id,
                        &request_id,
                        issue_id.as_deref(),
                        outcome,
                        failed_status,
                        total_cost,
                        cleanup.worker_branch,
                        cleanup.normalization_warning,
                    )
                    .await,
                    executor_id.clone(),
                );
            }
        };

        funnel.emit(SpurEventBody::ExecutorPhaseChanged {
            id: eid.0.clone(),
            phase: LifecycleState::AwaitingReview,
        });

        let plan = delegation_plan.as_ref();
        let chosen_matches_dispatched = plan
            .and_then(|p| p.chosen.as_ref())
            .map(|c| normalize_agent_name(c) == normalize_agent_name(&agent));

        if chosen_matches_dispatched == Some(false) {
            tracing::warn!(
                session = %brain_session_id,
                chosen = %plan.and_then(|p| p.chosen.as_deref()).unwrap_or(""),
                dispatched = %agent,
                "delegation_plan.chosen does not match dispatched agent",
            );
        }

        drop(ack_tx);
        if let (Some(bundle), Some(ack_rx)) = (peer_mailbox.as_ref(), ack_rx) {
            let limits = bundle.router.limits();
            let quiet_window = std::time::Duration::from_millis(limits.drain_quiet_window_ms);
            let drain_max_total = std::time::Duration::from_millis(limits.drain_max_total_ms);
            drain_peer_acks_with_timeout(
                bundle,
                &spur_acp::domain::delegation::DelegationId(request_id.clone()),
                quiet_window,
                drain_max_total,
                &brain_session_id,
                &funnel,
                ack_rx,
            )
            .await;
        }

        let peer_influence = if peer_mailbox.is_some() {
            use crate::lineage::types::PeerEdgeState;

            let target = spur_acp::domain::delegation::DelegationId(request_id.clone());
            let mut summary = spur_acp::PeerInfluenceSummary::default();
            if let Some(lineage) = funnel.lineage_snapshot().await {
                let inbound = lineage.peer_edges_inbound_for_delegation(&target);
                let outbound = lineage.peer_edges_for_delegation(&target);

                for edge in inbound {
                    match edge.state {
                        PeerEdgeState::Consumed => summary.inbound_consumed += 1,
                        PeerEdgeState::Ignored => summary.inbound_ignored += 1,
                        PeerEdgeState::Undeliverable
                        | PeerEdgeState::Dropped
                        | PeerEdgeState::Expired
                        | PeerEdgeState::Rejected => summary.undelivered += 1,
                        _ => {}
                    }
                }
                summary.outbound_emitted = u32::try_from(outbound.len()).unwrap_or(u32::MAX);
            }
            // from_unreviewed_source stays false in Stage 1; it needs
            // brain-state lookup that is intentionally out of scope here.
            Some(summary)
        } else {
            None
        };

        let review_payload = ReviewPayload {
            summary: bounded_inline_summary(outcome.summary.as_deref()),
            diff_summary: outcome.diff_summary.clone(),
            pr_url: None,
            error: None,
            delegation_plan: delegation_plan.clone(),
            chosen_matches_dispatched,
            peer_influence,
        };

        // Emit via the handle — type-enforced: no handle → no emit.
        handle.emit_requested(&funnel, ReviewKind::Completion, review_payload);

        // Consume the handle to get the receiver for the decision loop.
        let rx = handle.into_rx();

        // Inline decision-loop (so we can intercept Retry before
        // apply_decision_to_candidate maps it to Failed).
        use spur_acp::ReviewDecision;
        let decision_result = tokio::select! {
            r = rx => r.ok(),
            _ = tokio::time::sleep(agent_config.review.review_timeout) => {
                review_sink.remove(&eid).await;
                let final_status = DelegationStatus::TimedOut {
                    waited_for: agent_config.review.review_timeout,
                    fallback: agent_config.review.review_timeout_default.clone(),
                };
                // Emit cancellation so the lineage projection clears
                // pending_review (DelegationCompleted alone does not).
                funnel.emit(SpurEventBody::ExecutorReviewCancelled {
                    id: eid.0.clone(),
                    reason: "review timeout".to_string(),
                });
                // TimedOut → preserve worktree (no commit).
                let cleanup = apply_worktree_cleanup(
                    &mut worktrees,
                    &outcome.worker_session,
                    &final_status,
                    WorktreeCleanupContext {
                        agent: &agent,
                        worktree_path: &outcome.worktree_path,
                        bypass_hooks: normalize_bypass_hooks,
                        pm_service: pm_service.as_ref(),
                        issue_id: issue_id.as_deref(),
                    },
                )
                .await;
                return (
                    finalize_worker_outcome(
                        &funnel,
                        &worker_mcp_servers,
                        pm_service.as_ref(),
                        &brain_session_id,
                        &request_id,
                        issue_id.as_deref(),
                        outcome,
                        final_status,
                        total_cost,
                        cleanup.worker_branch,
                        cleanup.normalization_warning,
                    )
                    .await,
                    executor_id.clone(),
                );
            }
        };

        match decision_result {
            Some(ReviewDecision::Approve) => {
                let final_status = outcome.candidate_status.clone();
                funnel.emit(SpurEventBody::ExecutorReviewResolved {
                    id: eid.0.clone(),
                    decision: ReviewDecision::Approve,
                });
                // Approve → commit + remove.
                let cleanup = apply_worktree_cleanup(
                    &mut worktrees,
                    &outcome.worker_session,
                    &final_status,
                    WorktreeCleanupContext {
                        agent: &agent,
                        worktree_path: &outcome.worktree_path,
                        bypass_hooks: normalize_bypass_hooks,
                        pm_service: pm_service.as_ref(),
                        issue_id: issue_id.as_deref(),
                    },
                )
                .await;
                return (
                    finalize_worker_outcome(
                        &funnel,
                        &worker_mcp_servers,
                        pm_service.as_ref(),
                        &brain_session_id,
                        &request_id,
                        issue_id.as_deref(),
                        outcome,
                        final_status,
                        total_cost,
                        cleanup.worker_branch,
                        cleanup.normalization_warning,
                    )
                    .await,
                    executor_id.clone(),
                );
            }
            Some(ReviewDecision::Reject { reason }) => {
                let final_status = DelegationStatus::Rejected {
                    reason: reason.clone(),
                };
                funnel.emit(SpurEventBody::ExecutorReviewResolved {
                    id: eid.0.clone(),
                    decision: ReviewDecision::Reject { reason },
                });
                // Rejected → no commit, preserve worktree.
                let cleanup = apply_worktree_cleanup(
                    &mut worktrees,
                    &outcome.worker_session,
                    &final_status,
                    WorktreeCleanupContext {
                        agent: &agent,
                        worktree_path: &outcome.worktree_path,
                        bypass_hooks: normalize_bypass_hooks,
                        pm_service: pm_service.as_ref(),
                        issue_id: issue_id.as_deref(),
                    },
                )
                .await;
                return (
                    finalize_worker_outcome(
                        &funnel,
                        &worker_mcp_servers,
                        pm_service.as_ref(),
                        &brain_session_id,
                        &request_id,
                        issue_id.as_deref(),
                        outcome,
                        final_status,
                        total_cost,
                        cleanup.worker_branch,
                        cleanup.normalization_warning,
                    )
                    .await,
                    executor_id.clone(),
                );
            }
            Some(ReviewDecision::Modify { note }) => {
                let final_status = DelegationStatus::Modified {
                    reviewer_note: note.clone(),
                };
                funnel.emit(SpurEventBody::ExecutorReviewResolved {
                    id: eid.0.clone(),
                    decision: ReviewDecision::Modify { note },
                });
                // Modified → commit + remove (approved with reviewer note).
                let cleanup = apply_worktree_cleanup(
                    &mut worktrees,
                    &outcome.worker_session,
                    &final_status,
                    WorktreeCleanupContext {
                        agent: &agent,
                        worktree_path: &outcome.worktree_path,
                        bypass_hooks: normalize_bypass_hooks,
                        pm_service: pm_service.as_ref(),
                        issue_id: issue_id.as_deref(),
                    },
                )
                .await;
                return (
                    finalize_worker_outcome(
                        &funnel,
                        &worker_mcp_servers,
                        pm_service.as_ref(),
                        &brain_session_id,
                        &request_id,
                        issue_id.as_deref(),
                        outcome,
                        final_status,
                        total_cost,
                        cleanup.worker_branch,
                        cleanup.normalization_warning,
                    )
                    .await,
                    executor_id.clone(),
                );
            }
            Some(ReviewDecision::Retry { new_constraints }) => {
                // DN-2: bound check + exhaustion status live in
                // `crate::retry_loop::RetryLoop` — shared with
                // `test_support::run_gate_with_retries`. Both sites
                // share the strict `>` semantic and the exact error
                // string format. Changes to retry semantics should
                // touch `retry_loop.rs`, not this site.
                //
                // `>` (not `>=`): spec's "Retry × 4 when
                // max_review_retries = 3 produces Failed" means 3
                // retries are allowed (attempts bump 1→2→3→4), and
                // the 4th Retry decision fails.
                if let Some(final_status) = crate::retry_loop::RetryLoop::check_exceeded(
                    attempt_n,
                    agent_config.review.max_review_retries,
                ) {
                    // Retry limit → Failed (remove, no commit).
                    let cleanup = apply_worktree_cleanup(
                        &mut worktrees,
                        &outcome.worker_session,
                        &final_status,
                        WorktreeCleanupContext {
                            agent: &agent,
                            worktree_path: &outcome.worktree_path,
                            bypass_hooks: normalize_bypass_hooks,
                            pm_service: pm_service.as_ref(),
                            issue_id: issue_id.as_deref(),
                        },
                    )
                    .await;
                    return (
                        finalize_worker_outcome(
                            &funnel,
                            &worker_mcp_servers,
                            pm_service.as_ref(),
                            &brain_session_id,
                            &request_id,
                            issue_id.as_deref(),
                            outcome,
                            final_status,
                            total_cost,
                            cleanup.worker_branch,
                            cleanup.normalization_warning,
                        )
                        .await,
                        executor_id.clone(),
                    );
                }

                // Retry: generate the NEXT attempt's session id
                // FIRST so we can announce it in
                // ExecutorRetryStarted (matching what
                // run_one_worker_attempt will use on the next
                // iteration). The lineage projection treats
                // new_session_id as the Attempt.session_id of
                // the next attempt; emitting a fresh-but-unused
                // id here would silently dangle.
                let retry_session = SessionId::new();
                funnel.emit(SpurEventBody::ExecutorRetryStarted {
                    id: eid.0.clone(),
                    attempt_n: attempt_n + 1,
                    reason: new_constraints.clone(),
                    new_session_id: retry_session.clone(),
                });

                // Record this attempt in the retry history before re-prompting.
                // See docs/superpowers/specs/2026-04-14-brain-worker-refinement-design.md
                // for the rationale — inverts the original
                // "prevent compounding" choice in favor of the
                // Reflexion pattern, with a 2KB bloat cap as the
                // mitigation.
                retry_history.push(RetryAttempt {
                    attempt_n,
                    summary: bounded_inline_summary(outcome.summary.as_deref()),
                    diff_summary: outcome.diff_summary.clone(),
                    feedback: new_constraints.clone(),
                });
                apply_bloat_cap(&mut retry_history, 2048);

                current_task =
                    render_retry_context(&retry_history, &original_task, &new_constraints);
                attempt_n += 1;
                next_worker_session = retry_session;

                // Retry intermediates are never preserved — remove
                // the current attempt's worktree before spawning
                // the next attempt. No commit (intermediate diff is
                // moot once the retry produces its own diff).
                //
                // Log (don't swallow) failures: retries use a fresh
                // SessionId, so collision is impossible, but disk space
                // may leak until manual cleanup or cleanup_orphans runs.
                if let Err(e) = worktrees.remove_worktree(&outcome.worker_session).await {
                    tracing::warn!(
                        session = %outcome.worker_session,
                        error = %e,
                        "failed to remove retry-attempt worktree; retry will use a fresh session ID, but disk space may leak"
                    );
                }

                // Exponential backoff: 1s, 2s, 4s, 8s, … capped at 30s.
                let backoff_secs = std::cmp::min(1u64 << (attempt_n.saturating_sub(1) as u64), 30);
                tracing::info!(
                    attempt_n = attempt_n,
                    backoff_secs = backoff_secs,
                    "retry backoff before next attempt"
                );
                tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;

                continue;
            }
            None => {
                // Sender dropped — treat as timeout.
                review_sink.remove(&eid).await;
                let final_status = DelegationStatus::TimedOut {
                    waited_for: agent_config.review.review_timeout,
                    fallback: agent_config.review.review_timeout_default.clone(),
                };
                // Emit cancellation so the lineage projection clears
                // pending_review (DelegationCompleted alone does not).
                funnel.emit(SpurEventBody::ExecutorReviewCancelled {
                    id: eid.0.clone(),
                    reason: "review sender dropped".to_string(),
                });
                // Sender-drop TimedOut → preserve worktree (no commit).
                let cleanup = apply_worktree_cleanup(
                    &mut worktrees,
                    &outcome.worker_session,
                    &final_status,
                    WorktreeCleanupContext {
                        agent: &agent,
                        worktree_path: &outcome.worktree_path,
                        bypass_hooks: normalize_bypass_hooks,
                        pm_service: pm_service.as_ref(),
                        issue_id: issue_id.as_deref(),
                    },
                )
                .await;
                return (
                    finalize_worker_outcome(
                        &funnel,
                        &worker_mcp_servers,
                        pm_service.as_ref(),
                        &brain_session_id,
                        &request_id,
                        issue_id.as_deref(),
                        outcome,
                        final_status,
                        total_cost,
                        cleanup.worker_branch,
                        cleanup.normalization_warning,
                    )
                    .await,
                    executor_id.clone(),
                );
            }
        }
    }
}

/// One retry attempt's surviving state, kept in memory across the
/// retry loop so later attempts can see the history. Module-local;
/// does not leak into public API.
#[derive(Debug, Clone)]
pub(crate) struct RetryAttempt {
    pub(crate) attempt_n: u32,
    pub(crate) summary: String,
    pub(crate) diff_summary: Option<spur_acp::DiffSummary>,
    /// Reviewer's `new_constraints` verbatim, the feedback that
    /// triggered this retry decision.
    pub(crate) feedback: String,
}

/// Render the augmented task prompt fed to the NEXT retry attempt.
///
/// Layout:
///   {original_task}
///
///   --- Previous attempts ---
///   Attempt N:
///     What was tried: {summary}
///     Files touched: {files_changed} changed, +{ins}/-{del}
///     Reviewer feedback: {feedback}
///   ...
///
///   --- Your task ---
///   Address the reviewer's most recent feedback above. Do NOT repeat
///   approaches that were rejected earlier — the reviewer sees the
///   same history and will reject a repeat.
///
///   Most recent feedback:
///   {current_feedback}
pub(crate) fn render_retry_context(
    history: &[RetryAttempt],
    original_task: &str,
    current_feedback: &str,
) -> String {
    let mut out = String::with_capacity(original_task.len() + current_feedback.len() + 512);
    out.push_str(original_task);

    if !history.is_empty() {
        out.push_str("\n\n--- Previous attempts ---\n");
        for a in history {
            out.push_str(&format!("\nAttempt {}:\n", a.attempt_n));
            out.push_str(&format!("  What was tried: {}\n", a.summary));
            if let Some(ds) = &a.diff_summary {
                out.push_str(&format!(
                    "  Files touched: {} changed, +{}/-{}\n",
                    ds.files_changed, ds.insertions, ds.deletions
                ));
            }
            out.push_str(&format!("  Reviewer feedback: {}\n", a.feedback));
        }
    }

    out.push_str(
        "\n--- Your task ---\n\
         Address the reviewer's most recent feedback above. Do NOT repeat \
         approaches that were rejected earlier — the reviewer sees the \
         same history and will reject a repeat.\n\n\
         Most recent feedback:\n",
    );
    out.push_str(current_feedback);
    out
}

/// Drop oldest attempts until the total in-memory summary+feedback
/// footprint fits under `max_bytes`. Preserves the most recent
/// attempts (those are most relevant to the current feedback).
pub(crate) fn apply_bloat_cap(history: &mut Vec<RetryAttempt>, max_bytes: usize) {
    fn size(a: &RetryAttempt) -> usize {
        a.summary.len() + a.feedback.len()
    }
    while history.iter().map(size).sum::<usize>() > max_bytes && !history.is_empty() {
        history.remove(0);
    }
}

#[cfg(test)]
mod full_summary_retention_tests {
    use super::{bounded_inline_summary, summary_cap_bytes};

    #[test]
    fn full_summary_retention_clips_inline_consumer_projection() {
        let full_summary = "x".repeat(summary_cap_bytes() + 1);

        let inline = bounded_inline_summary(Some(&full_summary));

        assert_ne!(inline, full_summary);
        assert!(inline.contains("chars omitted"));
    }
}

#[cfg(test)]
mod production_outcome_roundtrip_tests {
    use super::*;
    use crate::handlers::{fetch_outcome_artifact, WorkerCallContext};
    use crate::outcome_materializer::OutcomeMaterializer;
    use async_trait::async_trait;
    use futures::Stream;
    use spur_acp::domain::ContinuationSource;
    use spur_acp::{
        AcpSessionId, AcpToolCall, BrainSessionId, ContentChunk, DelegationId, InitializeResponse,
        NewSessionResponse, SessionNotification, TextContent,
    };
    use spur_blob_store::{MemoryOutcomeStore, OutcomeStore};
    use std::pin::Pin;
    use std::process::Command;

    struct ScriptedConnection {
        notifications: Vec<SessionNotification>,
    }

    #[async_trait]
    impl AgentConnection for ScriptedConnection {
        async fn initialize(
            &mut self,
            _request: InitializeRequest,
        ) -> anyhow::Result<InitializeResponse> {
            Ok(InitializeResponse::new(ProtocolVersion::LATEST))
        }

        async fn new_session(
            &mut self,
            _cwd: PathBuf,
            _mcp_servers: Vec<McpServer>,
        ) -> anyhow::Result<NewSessionResponse> {
            Ok(NewSessionResponse::new(AcpSessionId::new(
                "scripted-session",
            )))
        }

        async fn prompt(
            &mut self,
            _request: PromptRequest,
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
            Ok(Box::pin(futures::stream::iter(self.notifications.clone())))
        }

        async fn cancel(&mut self, _session_id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn shutdown(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        fn health(&self) -> AgentHealth {
            AgentHealth::Ready
        }
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git command must start");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn setup_repo() -> tempfile::TempDir {
        let repo = tempfile::TempDir::new().expect("tempdir");
        run_git(repo.path(), &["init", "-q", "-b", "main"]);
        run_git(repo.path(), &["config", "user.email", "test@spur.local"]);
        run_git(repo.path(), &["config", "user.name", "SPUR Test"]);
        std::fs::write(repo.path().join("README.md"), "base\n").expect("write seed");
        run_git(repo.path(), &["add", "README.md"]);
        run_git(repo.path(), &["commit", "-q", "-m", "seed"]);
        repo
    }

    fn text_update(text: impl Into<String>, thought: bool) -> SessionUpdate {
        let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text.into())));
        if thought {
            SessionUpdate::AgentThoughtChunk(chunk)
        } else {
            SessionUpdate::AgentMessageChunk(chunk)
        }
    }

    async fn run_scripted_attempt(
        repo_root: PathBuf,
        brain_session_id: &BrainSessionId,
        delegation_id: &str,
        final_message: &str,
        funnel: &crate::event_funnel::FunnelHandle,
    ) -> WorkerAttemptOutcome {
        let session_id = AcpSessionId::new("scripted-session");
        let notifications = vec![
            SessionNotification::new(
                session_id.clone(),
                text_update("earlier assistant message", false),
            ),
            SessionNotification::new(
                session_id.clone(),
                SessionUpdate::ToolCall(AcpToolCall::new("tool-1", "read")),
            ),
            SessionNotification::new(
                session_id.clone(),
                text_update("private post-tool reasoning", true),
            ),
            SessionNotification::new(session_id.clone(), text_update("final: ", false)),
            SessionNotification::new(session_id, text_update(final_message, false)),
        ];
        let notifications_for_factory = notifications.clone();
        let mut worktrees = WorktreeManager::new(repo_root);
        let mut agent_config = spur_acp::AgentConfig::with_defaults("scripted");
        agent_config.kind = spur_acp::types::AgentKind::Generic;
        let worker_session = SessionId::new();
        let fault_hooks = FaultInjectionHooks::default();
        let feature_gate = spur_license::FeatureGate::new_with_install_id(
            spur_license::policy::PolicyResolver::embedded(),
            spur_license::InstallId::from_uuid(uuid::Uuid::nil()),
        );

        run_one_worker_attempt(
            worker_session,
            WorkerAttemptCtx {
                brain_session_id,
                agent: "scripted",
                model: None,
                effort: None,
                profile: None,
                profile_def: None,
                skills: None,
                config_overrides: None,
                task: "produce the final response",
                request_id: delegation_id,
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
                worker_mcp_tool_names: None,
                pm_service: None,
                feature_gate: &feature_gate,
                connection_factory: Some(&move |_cfg, _args, _env, _repo_root| {
                    Box::new(ScriptedConnection {
                        notifications: notifications_for_factory.clone(),
                    })
                }),
            },
            &mut worktrees,
            funnel,
        )
        .await
        .expect("scripted worker attempt must succeed")
    }

    #[tokio::test]
    async fn production_terminal_paths_round_trip_complete_latest_message_and_artifact() {
        let repo = setup_repo();
        let (funnel, _events_rx) = crate::event_funnel::test_channel();
        let brain_session_id = BrainSessionId::new(SessionId::new());
        let worker_servers: Arc<DashMap<BrainSessionId, Arc<WorkerMcpServer>>> =
            Arc::new(DashMap::new());
        let final_tail = "x".repeat(summary_cap_bytes() + 64);
        let expected_summary = format!("final: {final_tail}");
        let captured = run_scripted_attempt(
            repo.path().to_path_buf(),
            &brain_session_id,
            "production-roundtrip",
            &final_tail,
            &funnel,
        )
        .await;
        assert_eq!(captured.summary.as_deref(), Some(expected_summary.as_str()));
        assert!(
            captured.artifact.is_some(),
            "long response must persist an artifact"
        );

        let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let materializer = OutcomeMaterializer::new(store.clone());
        let statuses = vec![
            DelegationStatus::Success,
            DelegationStatus::Failed {
                error: "review registration failed".into(),
            },
            DelegationStatus::Rejected {
                reason: "review rejected".into(),
            },
            DelegationStatus::Modified {
                reviewer_note: "approved with changes".into(),
            },
            DelegationStatus::TimedOut {
                waited_for: Duration::from_secs(1),
                fallback: TimeoutFallback::Abandon,
            },
        ];

        for (index, status) in statuses.into_iter().enumerate() {
            let delegation_id: DelegationId = format!("terminal-path-{index}").into();
            let result = finalize_worker_outcome(
                &funnel,
                &worker_servers,
                None,
                &brain_session_id,
                delegation_id.as_str(),
                None,
                captured.clone(),
                status,
                0.0,
                None,
                Some("normalization diagnostic".into()),
            )
            .await;
            assert_eq!(result.summary.as_deref(), Some(expected_summary.as_str()));
            assert!(
                result.artifact.is_some(),
                "terminal path {index} lost artifact"
            );
            assert_eq!(
                result
                    .resolved_config
                    .as_ref()
                    .expect("worker outcome has resolved config")
                    .outcome_warning
                    .as_deref(),
                Some("normalization diagnostic")
            );

            let continuation = materializer
                .materialize(
                    result,
                    delegation_id.clone(),
                    1,
                    brain_session_id.clone(),
                    ContinuationSource::Inline,
                    None,
                )
                .await;
            assert_ne!(
                continuation.payload.summary.as_deref(),
                Some(expected_summary.as_str()),
                "continuation path {index} must remain bounded"
            );
            assert_eq!(
                continuation
                    .payload
                    .resolved_config
                    .as_ref()
                    .expect("continuation carries resolved config")
                    .outcome_warning
                    .as_deref(),
                Some("normalization diagnostic")
            );

            let response = fetch_outcome_artifact(
                &materializer,
                store.as_ref(),
                &WorkerCallContext {
                    delegation_id: delegation_id.to_string(),
                    brain_session_id: brain_session_id.as_session_id().to_string(),
                },
                serde_json::json!({
                    "delegation_id": delegation_id.as_str(),
                    "attempt": 1,
                    "section": "summary"
                }),
            )
            .await
            .expect("terminal summary fetch must succeed");
            let projected: serde_json::Value = serde_json::from_str(
                response["content"][0]["text"]
                    .as_str()
                    .expect("fetch response text"),
            )
            .expect("summary section must be JSON");
            assert_eq!(projected["summary"], expected_summary);
        }

        let setup_failure = finalize(
            &funnel,
            &worker_servers,
            None,
            &brain_session_id,
            "setup-failure",
            None,
            SessionId::new(),
            DelegationStatus::SetupFailed {
                error: spur_acp::AttemptSetupError::InitFailed {
                    error: "connection unavailable".into(),
                },
            },
            None,
            None,
            None,
            0.0,
            None,
            None,
            None,
            None,
        )
        .await;
        assert!(setup_failure.artifact.is_none());
    }
}

#[cfg(test)]
mod retry_context_tests {
    use super::{apply_bloat_cap, render_retry_context, RetryAttempt};
    use spur_acp::DiffSummary;
    use std::path::PathBuf;

    fn att(n: u32, summary: &str, feedback: &str) -> RetryAttempt {
        RetryAttempt {
            attempt_n: n,
            summary: summary.into(),
            diff_summary: Some(DiffSummary {
                files_changed: 1,
                insertions: 10,
                deletions: 2,
                files: vec![PathBuf::from("f.rs")],
            }),
            feedback: feedback.into(),
        }
    }

    #[test]
    fn render_includes_original_task_and_all_attempts_and_current_feedback() {
        let history = vec![
            att(1, "tried approach A", "needs tests"),
            att(2, "tried approach B", "still too slow"),
        ];
        let out = render_retry_context(&history, "make foo fast", "use async");
        assert!(out.contains("make foo fast"));
        assert!(out.contains("Attempt 1"));
        assert!(out.contains("tried approach A"));
        assert!(out.contains("needs tests"));
        assert!(out.contains("Attempt 2"));
        assert!(out.contains("tried approach B"));
        assert!(out.contains("still too slow"));
        assert!(out.contains("use async"));
        assert!(out.contains("1 changed"));
        assert!(out.contains("+10"));
        assert!(out.contains("-2"));
    }

    #[test]
    fn render_handles_empty_history() {
        let out = render_retry_context(&[], "task", "feedback");
        assert!(out.contains("task"));
        assert!(out.contains("feedback"));
        assert!(!out.contains("Attempt 1"));
    }

    #[test]
    fn apply_bloat_cap_drops_oldest_first() {
        let big = "x".repeat(1000);
        let mut history = vec![
            att(1, &big, "fb1"),
            att(2, &big, "fb2"),
            att(3, &big, "fb3"),
        ];
        apply_bloat_cap(&mut history, 2000);
        assert!(history.iter().all(|a| a.attempt_n != 1));
        assert!(history.iter().any(|a| a.attempt_n == 3));
    }

    #[test]
    fn apply_bloat_cap_is_noop_when_under_cap() {
        let mut history = vec![att(1, "s", "f")];
        apply_bloat_cap(&mut history, 10_000);
        assert_eq!(history.len(), 1);
    }
}
