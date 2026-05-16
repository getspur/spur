use super::base_spec::{
    emit_dispatch_overlay_applied, extract_overlays, resolve_base_branch,
    snapshot_required_for_dispatch,
};
use super::file_touch::{maybe_synthesize_file_touch, FileTouchDedup};
use super::*;

/// Format a worker task string with an optional `## Relevant Files`
/// section prepended.
///
/// - When `context_files.is_empty()`, the task string is returned
///   unchanged (no section prepended).
/// - Otherwise a `## Relevant Files` header is prepended with each
///   path as a Markdown bullet, followed by a `## Task` header and
///   the original task body. Order of the bullets preserves the input
///   order.
///
/// This function does no file I/O. The worker's own Read tool is
/// responsible for opening the listed paths.
pub(crate) fn format_worker_task(task: &str, context_files: &[String]) -> String {
    if context_files.is_empty() {
        return task.to_string();
    }
    let mut out = String::with_capacity(task.len() + 128 + context_files.len() * 64);
    out.push_str("## Relevant Files\n\n");
    out.push_str(
        "The following files were declared as relevant by the caller. \
         Open them with your Read tool as needed.\n\n",
    );
    for path in context_files {
        out.push_str("- ");
        out.push_str(path);
        out.push('\n');
    }
    out.push_str("\n## Task\n\n");
    out.push_str(task);
    out
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug)]
pub(crate) enum AttemptSetupError {
    SnapshotFailed(String),
    WorktreeFailed(String),
    InitFailed(String),
    SessionFailed(String),
    OverlayConflict {
        source_task_id: String,
        files: Vec<String>,
    },
}

impl std::fmt::Display for AttemptSetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SnapshotFailed(e) => write!(f, "Failed to snapshot brain state: {e}"),
            Self::WorktreeFailed(e) => write!(f, "Failed to create worktree: {e}"),
            Self::InitFailed(e) => write!(f, "Failed to initialize worker: {e}"),
            Self::SessionFailed(e) => write!(f, "Failed to create worker session: {e}"),
            Self::OverlayConflict {
                source_task_id,
                files,
            } => write!(
                f,
                "overlay conflict applying {source_task_id}: {} files",
                files.len()
            ),
        }
    }
}

/// Outcome of one worker attempt: whatever we'd need to close out the
/// delegation OR feed into the review gate.
pub(crate) struct WorkerAttemptOutcome {
    pub(crate) worker_session: SessionId,
    pub(crate) candidate_status: DelegationStatus,
    pub(crate) diff: Option<String>,
    pub(crate) diff_summary: Option<spur_acp::DiffSummary>,
    pub(crate) summary: Option<String>,
    pub(crate) cost: f64,
    /// Path to the worktree that holds this attempt's diff.
    /// Used by `execute_delegation` to log a preserved path on
    /// `Rejected` / `TimedOut` — worktree removal is deferred to
    /// after the review gate.
    pub(crate) worktree_path: PathBuf,
    /// Side-channel artifact (persisted stdout when output > summary cap).
    /// `None` when the worker's stdout fit under the cap.
    #[allow(dead_code)] // Populated in Task 8 (artifact persistence wiring).
    pub(crate) artifact: Option<spur_acp::WorkerArtifact>,
}

/// Run a single worker attempt: snapshot brain state, create worktree,
/// spawn agent, prompt, collect diff.
///
/// `worker_session` is provided by the caller (rather than generated
/// inside) so `execute_delegation`'s Retry arm can announce the next
/// attempt's session id in `ExecutorRetryStarted.new_session_id` and
/// have it match what this function actually uses — closing the lineage
/// `Attempt.session_id ↔ worker event` linkage gap.
///
/// **Worktree lifecycle**: this function creates the worktree and
/// collects the diff, but does NOT commit or remove the worktree.
/// Commit and removal are deferred to `execute_delegation` so the
/// post-gate decision can determine whether to preserve
/// (`Rejected`/`TimedOut`) or remove (all other terminal statuses).
/// Exception: if a setup failure occurs AFTER the worktree is created
/// (e.g., agent init failure), the worktree IS cleaned up here
/// immediately — setup failures short-circuit without retry and the
/// caller's `finalize` records the error status.
///
/// Read-only context shared across worker attempt retries.
pub(crate) struct WorkerAttemptCtx<'a> {
    pub(crate) brain_session_id: &'a spur_acp::BrainSessionId,
    pub(crate) agent: &'a str,
    pub(crate) task: &'a str,
    pub(crate) request_id: &'a str,
    pub(crate) attempt: u32,
    pub(crate) agent_config: &'a spur_acp::config::AgentConfig,
    pub(crate) delegation_plan: Option<spur_acp::domain::DelegationPlan>,
    pub(crate) issue_id: Option<String>,
    pub(crate) prior_branch_for_reuse: Option<String>,
    pub(crate) peer_mailbox: Option<&'a crate::peer_mailbox::PeerMailboxBundle>,
    pub(crate) ack_tx: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    pub(crate) base: Option<BaseSpec>,
    /// Publishes the resolved post-overlay worktree HEAD back to the reconciler.
    pub(crate) dispatched_base_oid_tx: Option<tokio::sync::watch::Sender<Option<String>>>,
    pub(crate) fault_injection_hooks: &'a FaultInjectionHooks,
    /// Phase 5 / Task 26 — worker `mcp_servers` config. Empty unless the
    /// delegation request set `enable_worker_mcp = Some(true)`. Resolved
    /// once in `execute_delegation` so retries reuse the same token URL.
    pub(crate) worker_mcp_servers: &'a [McpServer],
    pub(crate) pm_service: Option<&'a PmService>,
    pub(crate) feature_gate: &'a spur_license::FeatureGate,
}

async fn persist_dispatched_base_oid_label(
    pm_service: Option<&PmService>,
    issue_id: Option<&str>,
    dispatched_base_oid: &str,
) -> Result<(), AttemptSetupError> {
    let (Some(pm), Some(issue_id)) = (pm_service, issue_id) else {
        return Ok(());
    };

    let issue = pm.get_issue(issue_id).await.map_err(|e| {
        AttemptSetupError::WorktreeFailed(format!("persist dispatched base OID label: {e:#}"))
    })?;
    let label = spur_mcp::plan::labels::dispatched_base_oid(dispatched_base_oid);
    let remove_labels = issue
        .labels
        .iter()
        .filter(|existing| {
            spur_mcp::plan::labels::parse_dispatched_base_oid(existing).is_some()
                && existing.as_str() != label
        })
        .cloned()
        .collect();

    // Residual PR4 atomicity window: `update_issue` runs add_labels and
    // remove_labels as separate beads-rust mutations (each its own SQLite
    // transaction; see crates/spur-pm/src/beads_crate/adapter.rs:341-344).
    // A process death between them leaves the issue with both old and new
    // dispatched-base-oid labels, and `recover_orphaned_dispatch`'s find_map
    // may select the stale one. See spec Risks for closure plan.
    pm.update_issue(
        issue_id,
        spur_pm::IssueUpdate {
            add_labels: vec![label],
            remove_labels,
            ..Default::default()
        },
    )
    .await
    .map_err(|e| {
        AttemptSetupError::WorktreeFailed(format!("persist dispatched base OID label: {e:#}"))
    })
}

async fn preapply_prior_branch_for_reuse(
    worktrees: &WorktreeManager,
    worktree_path: &std::path::Path,
    plan_base_oid: &str,
    prior_branch: &str,
) {
    let diff_output = match worktrees
        .diff_binary_between_refs(plan_base_oid, prior_branch)
        .await
    {
        Ok(diff) => diff,
        Err(err) => {
            tracing::warn!(
                prior_branch = prior_branch,
                plan_base_oid = plan_base_oid,
                error = %err,
                "pre-apply: git diff failed; leaving worktree at clean computed base"
            );
            return;
        }
    };

    if let Err(err) = worktrees
        .apply_patch_3way(worktree_path, &diff_output)
        .await
    {
        tracing::warn!(
            prior_branch = prior_branch,
            plan_base_oid = plan_base_oid,
            error = %err,
            "pre-apply: git apply --3way failed; scrubbing worktree",
        );
        if let Err(scrub_err) = worktrees.scrub_worktree(worktree_path).await {
            tracing::warn!(
                prior_branch = prior_branch,
                error = %scrub_err,
                "pre-apply: failed to scrub worktree after apply failure"
            );
        }
    }
}

/// Returns `Ok(WorkerAttemptOutcome)` for any flow that produced a
/// worker candidate status — success OR worker-reported errors — both
/// of which are retry-eligible (the human reviewer decides).
///
/// Returns `Err(AttemptSetupError)` only for pre-worker setup failures
/// (worktree creation, agent initialization, session creation). The
/// caller short-circuits the delegation without retry — consistent
/// with pre-T10 behavior. Per-attempt error shape is decoupled from
/// the public `DelegationResult` type.
pub(crate) async fn run_one_worker_attempt(
    worker_session: SessionId,
    ctx: WorkerAttemptCtx<'_>,
    worktrees: &mut WorktreeManager,
    funnel: &crate::event_funnel::FunnelHandle,
) -> Result<WorkerAttemptOutcome, AttemptSetupError> {
    // NOTE: DelegationRequested is emitted per-attempt here. The legacy
    // lineage adapter (lineage/adapter.rs) keys task_spec population to
    // the FIRST matching empty-task_spec executor, so on retry the
    // constraint-augmented task silently drops at the adapter boundary.
    // This is part of the broader "adapter keys off worker_session, not
    // stable executor_id" limitation documented for follow-up work.
    // The projection path (apply_inner) sees each event correctly.
    funnel.emit(SpurEventBody::DelegationRequested {
        from: ctx.brain_session_id.as_session_id().clone(),
        to_agent: ctx.agent.to_string(),
        task: ctx.task.to_string(),
        request_id: ctx.request_id.to_string(),
        delegation_plan: ctx.delegation_plan.clone(),
        issue_id: ctx.issue_id.clone(),
    });

    let start = Instant::now();

    // 1. Snapshot brain state — only when the resolved base would consume it.
    //    Explicit Branch/Commit bases bypass the WT entirely (br-osl).
    let snapshot_needed = snapshot_required_for_dispatch(ctx.base.as_ref());
    let snapshot_branch = if snapshot_needed {
        worktrees
            .snapshot_brain_state()
            .await
            .map_err(|e| AttemptSetupError::SnapshotFailed(e.to_string()))?
    } else {
        String::new()
    };

    let base_branch = ctx
        .base
        .as_ref()
        .map(|spec| resolve_base_branch(spec, &snapshot_branch))
        .unwrap_or_else(|| snapshot_branch.clone());

    let worktree_info = worktrees
        .create_worktree_v2(
            ctx.brain_session_id,
            &worker_session,
            ctx.agent,
            &base_branch,
        )
        .await
        // {e:#} walks the anyhow source chain so the underlying `git worktree add`
        // stderr (path collision, missing ref, disk full, etc.) is surfaced in the
        // returned AttemptSetupError instead of being hidden behind the top-level
        // `failed to create v2 worktree at <path>` context wrapper.
        .map_err(|e| AttemptSetupError::WorktreeFailed(format!("{e:#}")))?;

    // The snapshot branch is only needed as a base ref for worktree creation.
    // Once the worktree exists, delete it immediately to prevent ref leaks.
    // Skip when no snapshot was taken in the first place.
    if snapshot_needed {
        if let Err(e) = worktrees.delete_snapshot_branch(&snapshot_branch).await {
            tracing::debug!(
                snapshot_branch = %snapshot_branch,
                error = %e,
                "failed to delete snapshot branch after worktree creation; will leak until cleanup_orphans runs"
            );
        }
    }

    let overlays = ctx.base.as_ref().map(extract_overlays).unwrap_or_default();
    if !overlays.is_empty() {
        if let Err(e) = worktrees
            .apply_overlays(&worktree_info.path, &overlays)
            .await
        {
            let setup_err = match e {
                WorktreeError::OverlayConflict {
                    source_task_id,
                    files,
                } => AttemptSetupError::OverlayConflict {
                    source_task_id,
                    files,
                },
                other => AttemptSetupError::WorktreeFailed(other.to_string()),
            };
            let _ = worktrees.remove_worktree(&worker_session).await;
            return Err(setup_err);
        }
    }

    ctx.fault_injection_hooks.maybe_panic_after_overlay_apply();

    let dispatched_base_oid = match worktrees.resolve_head(&worktree_info.path).await {
        Ok(oid) => oid,
        Err(e) => {
            let _ = worktrees.remove_worktree(&worker_session).await;
            return Err(AttemptSetupError::WorktreeFailed(format!(
                "resolve worktree HEAD: {e:#}"
            )));
        }
    };
    worktrees
        .update_base_commit(&worker_session, dispatched_base_oid.clone())
        .map_err(|e| AttemptSetupError::WorktreeFailed(format!("update base commit: {e:#}")))?;
    if let Some(prior_branch) = ctx.prior_branch_for_reuse.as_deref() {
        preapply_prior_branch_for_reuse(
            worktrees,
            &worktree_info.path,
            &dispatched_base_oid,
            prior_branch,
        )
        .await;
    }
    if let Err(e) = persist_dispatched_base_oid_label(
        ctx.pm_service,
        ctx.issue_id.as_deref(),
        &dispatched_base_oid,
    )
    .await
    {
        let _ = worktrees.remove_worktree(&worker_session).await;
        return Err(e);
    }
    // INV-S3 audit: the resolved base OID is persisted as a beads label before
    // the watch channel publishes it to the reconciler's completion task.
    if let Some(tx) = &ctx.dispatched_base_oid_tx {
        let _ = tx.send(Some(dispatched_base_oid.clone()));
    }
    emit_dispatch_overlay_applied(
        funnel,
        ctx.request_id,
        ctx.base.as_ref(),
        &dispatched_base_oid,
        &overlays,
    );
    spur_mcp::plan::emit_worker_started_audit(
        ctx.pm_service.map(|pm| pm as &dyn spur_mcp::plan::PmLike),
        &ctx.issue_id,
        ctx.feature_gate,
        ctx.request_id,
        &worktree_info.branch,
        &worker_session.0,
        &dispatched_base_oid,
    )
    .await;

    // 2. Spawn worker agent in worktree via AgentConnection.
    // Workers never receive a permission_tx, so L2 auto-approve is
    // implicitly always on for them. skip_permissions still has effect
    // via L1a (spawn args).
    let spawn_args = ctx.agent_config.effective_args();
    let mut connection: Box<dyn AgentConnection> =
        connection::build_connection_from_transport(ctx.agent_config, spawn_args, None);

    // S5 — consume `_spur/*` ExtNotifications from this worker and
    // translate them into SpurEvent variants via the funnel. Must run
    // before `connection` is moved; `take_ext_notification_rx` only
    // needs `&mut self` but can be called exactly once per connection.
    if let Some(mut ext_rx) = connection.take_ext_notification_rx() {
        let funnel_for_ext = funnel.clone();
        let executor_id_for_ext = worker_session.0.clone();
        let brain_session_for_ext = ctx.brain_session_id.as_session_id().clone();
        let peer_mailbox_for_ext = ctx.peer_mailbox.cloned();
        let ack_tx_for_ext = ctx.ack_tx.clone();
        tokio::spawn(async move {
            while let Some(payload) = ext_rx.recv().await {
                let terminal_method = payload.method.clone();
                let terminal_params = payload.params.clone();
                crate::spur_ext_interp::interpret(
                    payload,
                    brain_session_for_ext.clone(),
                    executor_id_for_ext.clone(),
                    &funnel_for_ext,
                );
                if let (Some(bundle), Some(ack_tx)) = (&peer_mailbox_for_ext, &ack_tx_for_ext) {
                    crate::spur_ext_interp::interpret_peer_message_terminal(
                        &terminal_method,
                        terminal_params,
                        bundle,
                        ack_tx,
                        &funnel_for_ext,
                        brain_session_for_ext.0.as_str(),
                        executor_id_for_ext.as_str(),
                    )
                    .await;
                }
            }
        });
    }

    let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
    if let Err(e) = connection.initialize(init_request).await {
        let _ = worktrees.remove_worktree(&worker_session).await;
        return Err(AttemptSetupError::InitFailed(e.to_string()));
    }

    // Emit WorkerSpawned event.
    funnel.emit(SpurEventBody::WorkerSpawned {
        agent: ctx.agent.to_string(),
        session: worker_session.clone(),
        worktree: worktree_info.path.clone(),
    });
    // Correlate this executor with the brain's delegate_to_worker call
    // so the brain-side session_detail view can render an inline card.
    funnel.emit(SpurEventBody::DelegationDispatched {
        from: ctx.brain_session_id.as_session_id().clone(),
        request_id: ctx.request_id.to_string(),
        executor_id: worker_session.0.clone(),
    });

    // Phase 5 / Task 26 — worker MCP injection is gated on the delegation
    // request's `enable_worker_mcp` flag (resolved once in
    // `execute_delegation`). When the flag is unset/false this slice is
    // empty, preserving the historical "Workers get no MCP servers"
    // contract. When set, it carries exactly one `spur-worker-mcp`
    // entry whose URL embeds the per-delegation HMAC token.
    let session_response = match crate::skip_perm::new_session_with_bypass(
        &mut *connection,
        ctx.agent_config,
        worktree_info.path.clone(),
        ctx.worker_mcp_servers.to_vec(),
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            let _ = connection.shutdown().await;
            let _ = worktrees.remove_worktree(&worker_session).await;
            return Err(AttemptSetupError::SessionFailed(e.to_string()));
        }
    };

    // 3. Send task to worker.
    let prompt_text = format!(
        "Working directory: {}\n\nTask: {}",
        worktree_info.path.display(),
        ctx.task
    );
    // Pre-prompt peer-mailbox injection hook.
    let peer_context = match ctx.peer_mailbox {
        Some(bundle) => {
            // TODO(peer-mailbox): plumb context_window_chars from agent config.
            let context_window = 200_000;
            let target_delegation =
                spur_acp::domain::delegation::DelegationId(ctx.request_id.to_string());
            let limits = bundle.router.limits();
            let built = bundle
                .builder
                .build_for_target(
                    &target_delegation,
                    context_window,
                    limits.max_pending_mailbox_depth,
                    limits.max_peer_message_size,
                )
                .await;
            for inj in &built.injection_records {
                match bundle
                    .ledger
                    .record_injection(&inj.message_id, &built.target_prompt_id)
                    .await
                {
                    Ok(crate::peer_mailbox::ledger::InjectionOutcome::Injected) => {}
                    Ok(crate::peer_mailbox::ledger::InjectionOutcome::AlreadyInjected) => {
                        tracing::debug!(
                            message_id = ?inj.message_id,
                            "peer mailbox: replay injection no-op"
                        );
                    }
                    Err(err) => {
                        tracing::warn!(
                            message_id = ?inj.message_id,
                            ?err,
                            "peer mailbox: record_injection failed"
                        );
                    }
                }
            }
            Some(built)
        }
        None => None,
    };

    let mut prompt_blocks = vec![ContentBlock::Text(TextContent::new(prompt_text))];
    if let Some(pc) = &peer_context {
        if !pc.orchestrator_authored_text.is_empty() {
            prompt_blocks.insert(
                0,
                ContentBlock::Text(TextContent::new(format!(
                    "## Peer messages (orchestrator-authored)\n{}",
                    pc.orchestrator_authored_text
                ))),
            );
        }
    }
    let prompt_request = PromptRequest::new(session_response.session_id.clone(), prompt_blocks);

    let mut output_text = String::new();
    let mut worker_success = true;

    // S5 — Per-worker-attempt file-touch dedup. Owned locally (no Arc
    // needed) because the synthesizer is called synchronously from the
    // stream loop — nothing else clones or moves the instance.
    let file_touch_dedup = FileTouchDedup::new();

    // For native (ACP-transport) workers prompt() returns an empty stream;
    // notifications arrive via the connection-scoped broadcast instead.
    // drive_prompt_notifications handles both paths transparently.
    let prompt_result = crate::notification_drain::drive_prompt_notifications(
        &mut *connection,
        prompt_request,
        |notification| {
            // S5 — synthesize WorkerFileTouched from file-op ToolCalls
            // before any other notification handling.
            maybe_synthesize_file_touch(
                &notification,
                ctx.brain_session_id.as_session_id(),
                &worker_session.0,
                &file_touch_dedup,
                funnel,
            );
            // Phase 1 — stream worker notifications to TUI via event bus.
            funnel.emit(SpurEventBody::WorkerNotification {
                brain_session_id: ctx.brain_session_id.as_session_id().clone(),
                executor_id: worker_session.0.clone(),
                notification: Box::new(notification.clone()),
            });
            match &notification.update {
                SessionUpdate::AgentThoughtChunk(chunk)
                | SessionUpdate::AgentMessageChunk(chunk) => {
                    if let ContentBlock::Text(tc) = &chunk.content {
                        output_text.push_str(&tc.text);
                    }
                }
                _ => {}
            }
        },
    )
    .await;
    if let Err(e) = prompt_result {
        worker_success = false;
        output_text = format!("Failed to prompt worker: {e}");
    } else if let (Some(bundle), Some(pc)) = (ctx.peer_mailbox, peer_context) {
        use crate::peer_mailbox::{
            transition_with_audit, PeerTransitionKind, TransitionAuditOutcome,
        };
        use spur_acp::domain::peer_message::LedgerState;

        let target_delegation_id =
            spur_acp::domain::delegation::DelegationId(ctx.request_id.to_string());

        for inj in pc.injection_records {
            match transition_with_audit(
                bundle.ledger.as_ref(),
                funnel,
                ctx.brain_session_id,
                &target_delegation_id,
                inj.message_id,
                LedgerState::DeliveredInflight,
                PeerTransitionKind::DeliveredInflight,
            )
            .await
            {
                TransitionAuditOutcome::Changed => {}
                TransitionAuditOutcome::Unchanged(state) => {
                    tracing::debug!(
                        message_id = ?inj.message_id,
                        state = ?state,
                        "peer mailbox: delivered-inflight transition no-op"
                    );
                }
                TransitionAuditOutcome::TerminalSkip(state) => {
                    tracing::debug!(
                        message_id = ?inj.message_id,
                        state = ?state,
                        "post-prompt DeliveredInflight transition skipped: message already terminal"
                    );
                    continue;
                }
                TransitionAuditOutcome::AuditFailed(err) => {
                    tracing::warn!(
                        message_id = ?inj.message_id,
                        %err,
                        "peer mailbox: delivered-inflight transition failed"
                    );
                }
            }

            match transition_with_audit(
                bundle.ledger.as_ref(),
                funnel,
                ctx.brain_session_id,
                &target_delegation_id,
                inj.message_id,
                LedgerState::Delivered,
                PeerTransitionKind::Delivered,
            )
            .await
            {
                TransitionAuditOutcome::Changed => {
                    funnel.emit(spur_acp::SpurEventBody::WorkerPeerMessageDelivered {
                        brain_session_id: ctx.brain_session_id.to_string(),
                        message_id: inj.message_id,
                        target_delegation_id: target_delegation_id.clone(),
                        target_prompt_id: pc.target_prompt_id.clone(),
                        injected_chars: inj.injected_bytes,
                    });
                    // TODO(peer-mailbox): Task 14 startup reconciliation is
                    // the durable peer-mailbox audit path.
                }
                TransitionAuditOutcome::Unchanged(state) => {
                    tracing::debug!(
                        message_id = ?inj.message_id,
                        state = ?state,
                        "peer mailbox: delivered transition no-op"
                    );
                }
                TransitionAuditOutcome::TerminalSkip(state) => {
                    tracing::debug!(
                        message_id = ?inj.message_id,
                        state = ?state,
                        "post-prompt Delivered transition skipped: message already terminal"
                    );
                    continue;
                }
                TransitionAuditOutcome::AuditFailed(err) => {
                    tracing::warn!(
                        message_id = ?inj.message_id,
                        %err,
                        "peer mailbox: delivered transition failed"
                    );
                }
            }
        }
    }

    let _ = connection.shutdown().await;

    // 4. Collect diff. `basis` is either "HEAD" (uncommitted) or
    // "<base>..HEAD" (worker self-committed). We need it to compute the
    // matching diff_summary with the SAME git range — otherwise stats and
    // raw text disagree.
    let (diff, diff_basis) = worktrees
        .collect_diff(&worker_session)
        .await
        .unwrap_or((None, "HEAD"));

    // 5. Capture worktree path for execute_delegation's post-gate cleanup.
    // Commit and removal are deferred — see function doc.
    let worktree_path = worktrees
        .active
        .get(&worker_session.to_string())
        .map(|i| i.path.clone())
        .unwrap_or_default();

    // Compute structured diff stats on the SAME basis as the raw diff.
    // When collect_diff returned base..HEAD, we need to resolve the placeholder
    // to the real spec — fetch the base_commit from worktrees.
    let diff_summary = if diff.is_some() {
        let basis_spec = if diff_basis == "base_commit..HEAD" {
            // Resolve the placeholder with the actual base SHA.
            worktrees
                .active
                .get(&worker_session.to_string())
                .map(|i| format!("{}..HEAD", i.base_commit))
                .unwrap_or_else(|| "HEAD".to_string())
        } else {
            "HEAD".to_string()
        };
        build_diff_summary(&worktree_path, &basis_spec)
            .await
            .ok()
            .filter(|s| s.files_changed > 0)
    } else {
        None
    };

    let duration = start.elapsed();
    let cost = spur_cost::estimator::estimate_cost(ctx.agent_config.cost_tier, duration);

    // Attempt side-channel artifact persistence BEFORE building the
    // truncated summary. Only fires when output would otherwise lose
    // bytes to truncate_summary — the predicate is purely size-based
    // so mixed workers (diff + long rationale) and failure diagnostics
    // are both covered.
    let persist_result: Option<Result<spur_acp::WorkerArtifact, String>> = if output_text.len()
        > summary_cap_bytes()
    {
        let kind = if worker_success {
            spur_acp::ArtifactKind::Output
        } else {
            spur_acp::ArtifactKind::Diagnostic
        };
        let output_bytes = output_text.as_bytes();
        let byte_size = u64::try_from(output_bytes.len()).unwrap_or(u64::MAX);
        let key = OutcomeKey {
            brain_session_id: ctx.brain_session_id.clone(),
            delegation_id: spur_acp::DelegationId::from(ctx.request_id),
            attempt: ctx.attempt,
        };
        let metadata = OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: ContentType::Stdout,
            original_byte_size: byte_size,
            stored_byte_size: byte_size,
            sha256: sha256_hex_for_outcome(output_bytes),
        };
        let store = GitBlobOutcomeStore::new(worktrees.repo_root.clone());
        let outcome_store_result = match store.put(&key, output_bytes, &metadata).await {
            Ok(outcome_ref) => outcome_ref.as_worker_artifact(kind).ok_or_else(|| {
                "outcome store returned a non-git backend for worker artifact projection"
                    .to_string()
            }),
            Err(e) => Err(e.to_string()),
        };
        match outcome_store_result {
            Ok(a) => Some(Ok(a)),
            Err(primary_error) => {
                tracing::warn!(
                    session = %worker_session,
                    delegation_id = %ctx.request_id,
                    attempt = ctx.attempt,
                    error = %primary_error,
                    "outcome store artifact persistence failed; falling back to legacy artifact store"
                );
                match worktrees
                    .persist_artifact(&worker_session, &output_text, kind)
                    .await
                {
                    Ok(a) => Some(Ok(a)),
                    Err(fallback_error) => {
                        let error = format!(
                            "outcome store failed: {primary_error}; \
                             legacy artifact fallback failed: {fallback_error}"
                        );
                        tracing::warn!(
                            session = %worker_session,
                            error = %error,
                            "artifact persistence failed"
                        );
                        Some(Err(error))
                    }
                }
            }
        }
    } else {
        None
    };

    // Build the summary FIRST so the error-extraction path on the
    // failure branch can read from it — preserving the existing
    // behaviour at `orchestrator.rs:4116-4130` byte-for-byte.
    // (Raw-output sourcing would diverge when `SPUR_SUMMARY_MAX_BYTES`
    // is lowered below 500; we want this refactor to be a pure
    // no-op on the current failure-message semantics.)
    let summary_pre_annotation: Option<String> = if output_text.is_empty() {
        None
    } else {
        Some(truncate_summary_env_default(&output_text))
    };

    // Build the "original" error status by extracting from the
    // POST-truncation summary. Identical in shape to the existing
    // block at `orchestrator.rs:4116-4130`.
    let original_error_status = if worker_success {
        None
    } else {
        let error = summary_pre_annotation
            .as_deref()
            .map(|s| {
                let tail_bytes = 500usize.min(s.len());
                let start = {
                    let mut i = s.len().saturating_sub(tail_bytes);
                    while i < s.len() && !s.is_char_boundary(i) {
                        i += 1;
                    }
                    i
                };
                s[start..].to_string()
            })
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Worker reported errors (no output captured)".into());
        Some(DelegationStatus::Failed { error })
    };

    let (candidate_status, artifact, persist_failure_note) =
        decide_artifact_handling(worker_success, persist_result, original_error_status);

    // Surface a success-path tracing event for observability. Warn
    // on failure is already emitted above inside the persist match.
    if let Some(a) = &artifact {
        tracing::info!(
            session = %worker_session,
            object_ref = %a.object_ref,
            blob_sha = %a.blob_sha,
            size_bytes = a.size_bytes,
            "worker artifact persisted"
        );
    }

    // Apply the persist-failure annotation to the summary tail (if any).
    let summary = summary_pre_annotation.map(|mut s| {
        if let Some(note) = persist_failure_note.as_deref() {
            s.push('\n');
            s.push_str(note);
        }
        s
    });

    Ok(WorkerAttemptOutcome {
        worker_session,
        candidate_status,
        diff,
        diff_summary,
        summary,
        cost,
        worktree_path,
        artifact,
    })
}

#[cfg(test)]
mod format_worker_task_tests {
    use super::format_worker_task;

    #[test]
    fn empty_list_passes_task_through_unchanged() {
        let task = "Do the thing.";
        let out = format_worker_task(task, &[]);
        assert_eq!(out, task);
    }

    #[test]
    fn single_path_prepends_relevant_files_section() {
        let task = "Do the thing.";
        let files = vec!["src/a.rs".to_string()];
        let out = format_worker_task(task, &files);
        assert!(
            out.starts_with("## Relevant Files\n\n"),
            "expected Relevant Files header first, got: {out}",
        );
        assert!(out.contains("- src/a.rs"));
        assert!(out.contains("## Task\n\nDo the thing."));
    }

    #[test]
    fn multiple_paths_produce_ordered_bullets() {
        let files = vec![
            "crates/spur-mcp/src/server.rs".to_string(),
            "crates/spur-acp/src/adapter/claude.rs".to_string(),
        ];
        let out = format_worker_task("Go.", &files);
        let idx_first = out
            .find("- crates/spur-mcp/src/server.rs")
            .expect("first bullet");
        let idx_second = out
            .find("- crates/spur-acp/src/adapter/claude.rs")
            .expect("second bullet");
        assert!(idx_first < idx_second, "order must be preserved");
    }

    #[test]
    fn whitespace_task_body_still_gets_section_when_files_nonempty() {
        let out = format_worker_task("   ", &["x.rs".into()]);
        assert!(out.starts_with("## Relevant Files\n\n"));
        assert!(out.ends_with("   "));
    }
}

#[cfg(test)]
mod context_files_wiring_tests {
    use super::format_worker_task;

    /// Regression guard: the helper is imported where execute_delegation
    /// lives. If a refactor moves or renames it, the import here breaks
    /// before the wiring silently regresses.
    #[test]
    fn format_worker_task_is_available_in_orchestrator_module() {
        let out = format_worker_task("t", &["x".into()]);
        assert!(out.contains("## Relevant Files"));
    }
}

#[cfg(test)]
mod attempt_setup_error_chain_visibility_tests {
    use super::AttemptSetupError;
    use anyhow::Context;

    /// Pin the contract that all `AttemptSetupError::WorktreeFailed` callsites
    /// use `format!("{e:#}")` (or equivalent chain-walking format) so that the
    /// underlying error source (e.g. raw git stderr from
    /// `WorktreeManager::create_worktree_v2`) survives into the surfaced
    /// `Display` output.
    ///
    /// Before this guard, the lossy pattern `e.to_string()` was used at 5
    /// call sites in this file. That dropped the anyhow source chain entirely,
    /// leaving callers with only the top-level context (e.g. "failed to create
    /// v2 worktree at <path>") and no way to know WHY git failed. The 2026-05-16
    /// otobank investigation lost hours diagnosing repeated worktree-creation
    /// failures because the actual git error (a "fatal: ..." stderr line) was
    /// invisible to both the brain and the operator.
    ///
    /// If this test fails, you've probably reverted one of the `{e:#}` formats
    /// back to `{e}` or `e.to_string()`. Do not "fix" the test — restore the
    /// `{e:#}` format at the failing callsite.
    #[test]
    fn worktree_failed_display_preserves_source_chain() {
        // Build a 2-level anyhow chain: leaf = simulated git stderr,
        // outer = create_worktree_v2 context wrapper.
        let leaf =
            anyhow::anyhow!("git worktree add failed (exit 128): fatal: invalid reference: xyz");
        let outer = Err::<(), anyhow::Error>(leaf)
            .context("failed to create v2 worktree at /tmp/spur-test-fake-uuid")
            .unwrap_err();

        // Apply the same wrapping shape used at the create_worktree_v2 callsite.
        let setup_err = AttemptSetupError::WorktreeFailed(format!("{outer:#}"));
        let surfaced = format!("{setup_err}");

        assert!(
            surfaced.contains("invalid reference: xyz"),
            "WorktreeFailed must preserve the deepest source (git stderr). Got: {surfaced}"
        );
        assert!(
            surfaced.contains("git worktree add failed (exit 128)"),
            "WorktreeFailed must preserve the run_git wrapper context. Got: {surfaced}"
        );
        assert!(
            surfaced.contains("failed to create v2 worktree"),
            "WorktreeFailed must preserve the create_worktree_v2 top context. Got: {surfaced}"
        );
    }
}

#[cfg(test)]
mod prior_branch_preapply_tests {
    use super::preapply_prior_branch_for_reuse;
    use spur_worktree::manager::WorktreeManager;
    use std::path::Path;
    use std::process::Command;

    fn git(repo: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run git");
        if !out.status.success() {
            panic!(
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn write(path: &Path, body: &str) {
        std::fs::write(path, body).expect("write file");
    }

    fn setup_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "t@t"]);
        git(dir.path(), &["config", "user.name", "t"]);
        write(&dir.path().join("a.txt"), "base\n");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "base"]);
        dir
    }

    #[tokio::test]
    async fn happy_path_applies_prior_diff_as_dirty_worktree_edits() {
        let dir = setup_repo();
        git(dir.path(), &["checkout", "-q", "-b", "prior"]);
        write(&dir.path().join("a.txt"), "from-prior\n");
        write(&dir.path().join("b.txt"), "new-file\n");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "prior change"]);
        git(dir.path(), &["checkout", "-q", "main"]);

        let wt_path = dir.path().join("wt");
        git(
            dir.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "worker-happy",
                wt_path.to_str().expect("utf8"),
                "main",
            ],
        );
        let plan_base = git(dir.path(), &["rev-parse", "main"]);

        let manager = WorktreeManager::new(dir.path().to_path_buf());
        preapply_prior_branch_for_reuse(&manager, &wt_path, &plan_base, "prior").await;

        let status = git(&wt_path, &["status", "--porcelain"]);
        assert_ne!(status, "");
        assert!(status.contains("a.txt"));
        assert!(status.contains("b.txt"));
        assert_eq!(
            std::fs::read_to_string(wt_path.join("a.txt")).expect("read a.txt"),
            "from-prior\n"
        );
        assert_eq!(
            std::fs::read_to_string(wt_path.join("b.txt")).expect("read b.txt"),
            "new-file\n"
        );
        assert_eq!(git(&wt_path, &["rev-parse", "HEAD"]), plan_base);
    }

    #[tokio::test]
    async fn missing_branch_leaves_worktree_clean() {
        let dir = setup_repo();
        let wt_path = dir.path().join("wt");
        git(
            dir.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "worker-missing",
                wt_path.to_str().expect("utf8"),
                "main",
            ],
        );
        let plan_base = git(dir.path(), &["rev-parse", "main"]);

        let manager = WorktreeManager::new(dir.path().to_path_buf());
        preapply_prior_branch_for_reuse(&manager, &wt_path, &plan_base, "nonexistent").await;

        assert_eq!(git(&wt_path, &["status", "--porcelain"]), "");
        assert_eq!(git(&wt_path, &["rev-parse", "HEAD"]), plan_base);
    }

    #[tokio::test]
    async fn conflict_path_scrubs_partial_apply_state() {
        let dir = setup_repo();
        let plan_base = git(dir.path(), &["rev-parse", "main"]);

        git(dir.path(), &["checkout", "-q", "-b", "prior", &plan_base]);
        write(&dir.path().join("a.txt"), "prior-branch-change\n");
        git(dir.path(), &["add", "a.txt"]);
        git(dir.path(), &["commit", "-q", "-m", "prior change"]);
        git(dir.path(), &["checkout", "-q", "main"]);

        let wt_path = dir.path().join("wt");
        git(
            dir.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "worker-conflict",
                wt_path.to_str().expect("utf8"),
                "main",
            ],
        );
        write(&wt_path.join("a.txt"), "overlay-change\n");

        let manager = WorktreeManager::new(dir.path().to_path_buf());
        preapply_prior_branch_for_reuse(&manager, &wt_path, &plan_base, "prior").await;

        assert_eq!(git(&wt_path, &["status", "--porcelain"]), "");
        let body = std::fs::read_to_string(wt_path.join("a.txt")).expect("read a.txt");
        assert!(!body.contains("<<<<<<<"));
        assert_eq!(body, "base\n");
    }
}
