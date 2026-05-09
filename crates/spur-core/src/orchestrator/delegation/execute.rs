use super::peer_mailbox::drain_peer_acks_with_timeout;
use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_delegation(
    agent: String,
    original_task: String,
    context_files: Vec<String>,
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
    worker_mcp_fetcher: WorkerMcpFetcher,
    pm_service: Option<Arc<PmService>>,
    feature_gate: Arc<spur_license::FeatureGate>,
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

    // Phase 5 / Task 26 — resolve the worker `mcp_servers` vec ONCE per
    // delegation. When `enable_worker_mcp` is unset/false the vec is
    // empty (preserving the historical "Workers get no MCP servers"
    // contract). When `Some(true)`, the per-`BrainSession` worker MCP
    // server is ensured (lazy boot via `WorkerMcpFetcher::ensure`) and
    // a 1-hour HMAC token is minted; the token rides ONLY in the
    // structured `mcp_servers` URL — never in argv or env.
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
                task: &current_task,
                request_id: &request_id,
                attempt: attempt_n,
                agent_config: &agent_config,
                delegation_plan: delegation_plan.clone(),
                issue_id: issue_id.clone(),
                peer_mailbox: peer_mailbox.as_ref(),
                ack_tx: ack_tx.clone(),
                base: base.clone(),
                dispatched_base_oid_tx: dispatched_base_oid_tx.clone(),
                fault_injection_hooks: &fault_injection_hooks,
                worker_mcp_servers: &worker_mcp_dispatch_vec,
                pm_service: pm_service.as_deref(),
                feature_gate: feature_gate.as_ref(),
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
                    )
                    .await,
                    executor_id.clone(),
                );
            }
        };

        total_cost += outcome.cost;

        // On first attempt, capture executor_id from worker_session.
        if executor_id.is_none() {
            executor_id = Some(ExecutorId::new(outcome.worker_session.0.clone()));
        }
        let eid = executor_id.clone().unwrap();

        // No review gate — commit/remove then emit DelegationCompleted.
        if !agent_config.review.review_required {
            let preserved_branch = apply_worktree_cleanup(
                &mut worktrees,
                &outcome.worker_session,
                &outcome.candidate_status,
                &outcome.diff,
                &agent,
                &outcome.worktree_path,
            )
            .await;
            return (
                finalize(
                    &funnel,
                    &worker_mcp_servers,
                    pm_service.as_ref(),
                    &brain_session_id,
                    &request_id,
                    issue_id.as_deref(),
                    outcome.worker_session,
                    outcome.candidate_status,
                    outcome.diff,
                    outcome.diff_summary,
                    outcome.summary,
                    total_cost,
                    preserved_branch,
                    None,
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
                let preserved_branch = apply_worktree_cleanup(
                    &mut worktrees,
                    &outcome.worker_session,
                    &failed_status,
                    &outcome.diff,
                    &agent,
                    &outcome.worktree_path,
                )
                .await;
                return (
                    finalize(
                        &funnel,
                        &worker_mcp_servers,
                        pm_service.as_ref(),
                        &brain_session_id,
                        &request_id,
                        issue_id.as_deref(),
                        outcome.worker_session,
                        failed_status,
                        outcome.diff,
                        outcome.diff_summary.clone(),
                        outcome.summary,
                        total_cost,
                        preserved_branch,
                        None,
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
            summary: outcome.summary.clone().unwrap_or_default(),
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
                let preserved_branch = apply_worktree_cleanup(
                    &mut worktrees,
                    &outcome.worker_session,
                    &final_status,
                    &outcome.diff,
                    &agent,
                    &outcome.worktree_path,
                )
                .await;
                return (
                    finalize(
                        &funnel,
                        &worker_mcp_servers,
                        pm_service.as_ref(),
                        &brain_session_id,
                        &request_id,
                        issue_id.as_deref(),
                        outcome.worker_session,
                        final_status,
                        outcome.diff,
                        outcome.diff_summary.clone(),
                        outcome.summary,
                        total_cost,
                        preserved_branch,
                        None,
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
                let preserved_branch = apply_worktree_cleanup(
                    &mut worktrees,
                    &outcome.worker_session,
                    &final_status,
                    &outcome.diff,
                    &agent,
                    &outcome.worktree_path,
                )
                .await;
                return (
                    finalize(
                        &funnel,
                        &worker_mcp_servers,
                        pm_service.as_ref(),
                        &brain_session_id,
                        &request_id,
                        issue_id.as_deref(),
                        outcome.worker_session,
                        final_status,
                        outcome.diff,
                        outcome.diff_summary.clone(),
                        outcome.summary,
                        total_cost,
                        preserved_branch,
                        None,
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
                let preserved_branch = apply_worktree_cleanup(
                    &mut worktrees,
                    &outcome.worker_session,
                    &final_status,
                    &outcome.diff,
                    &agent,
                    &outcome.worktree_path,
                )
                .await;
                return (
                    finalize(
                        &funnel,
                        &worker_mcp_servers,
                        pm_service.as_ref(),
                        &brain_session_id,
                        &request_id,
                        issue_id.as_deref(),
                        outcome.worker_session,
                        final_status,
                        outcome.diff,
                        outcome.diff_summary.clone(),
                        outcome.summary,
                        total_cost,
                        preserved_branch,
                        None,
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
                let preserved_branch = apply_worktree_cleanup(
                    &mut worktrees,
                    &outcome.worker_session,
                    &final_status,
                    &outcome.diff,
                    &agent,
                    &outcome.worktree_path,
                )
                .await;
                return (
                    finalize(
                        &funnel,
                        &worker_mcp_servers,
                        pm_service.as_ref(),
                        &brain_session_id,
                        &request_id,
                        issue_id.as_deref(),
                        outcome.worker_session,
                        final_status,
                        outcome.diff,
                        outcome.diff_summary.clone(),
                        outcome.summary,
                        total_cost,
                        preserved_branch,
                        None,
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
                    let preserved_branch = apply_worktree_cleanup(
                        &mut worktrees,
                        &outcome.worker_session,
                        &final_status,
                        &outcome.diff,
                        &agent,
                        &outcome.worktree_path,
                    )
                    .await;
                    return (
                        finalize(
                            &funnel,
                            &worker_mcp_servers,
                            pm_service.as_ref(),
                            &brain_session_id,
                            &request_id,
                            issue_id.as_deref(),
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.diff_summary.clone(),
                            outcome.summary,
                            total_cost,
                            preserved_branch,
                            None,
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
                    summary: outcome.summary.clone().unwrap_or_default(),
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
                let preserved_branch = apply_worktree_cleanup(
                    &mut worktrees,
                    &outcome.worker_session,
                    &final_status,
                    &outcome.diff,
                    &agent,
                    &outcome.worktree_path,
                )
                .await;
                return (
                    finalize(
                        &funnel,
                        &worker_mcp_servers,
                        pm_service.as_ref(),
                        &brain_session_id,
                        &request_id,
                        issue_id.as_deref(),
                        outcome.worker_session,
                        final_status,
                        outcome.diff,
                        outcome.diff_summary.clone(),
                        outcome.summary,
                        total_cost,
                        preserved_branch,
                        None,
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
