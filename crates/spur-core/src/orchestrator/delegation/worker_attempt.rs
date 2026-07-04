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

#[cfg(any(test, feature = "test-support"))]
type WorkerConnectionFactory<'a> = dyn Fn(&spur_acp::config::AgentConfig, Vec<String>, &std::path::Path) -> Box<dyn AgentConnection>
    + Send
    + Sync
    + 'a;

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
    #[allow(dead_code)]
    pub(crate) model: Option<&'a str>,
    #[allow(dead_code)]
    pub(crate) effort: Option<&'a str>,
    pub(crate) profile: Option<&'a str>,
    /// Loaded+validated by `execute_delegation` when the profile is managed;
    /// `None` means select-only pass-through.
    pub(crate) profile_def: Option<&'a crate::agent_profiles::AgentProfile>,
    #[allow(dead_code)]
    pub(crate) config_overrides: Option<&'a std::collections::HashMap<String, String>>,
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
    /// Phase 5 / Task 26 — worker `mcp_servers` config. Default-on:
    /// populated for `enable_worker_mcp = None` (omitted) or `Some(true)`;
    /// empty only when the delegation request explicitly set
    /// `enable_worker_mcp = Some(false)`. Resolved once in
    /// `execute_delegation` so retries reuse the same token URL.
    pub(crate) worker_mcp_servers: &'a [McpServer],
    pub(crate) worker_mcp_server: Option<Arc<WorkerMcpServer>>,
    pub(crate) pm_service: Option<&'a PmService>,
    pub(crate) feature_gate: &'a spur_license::FeatureGate,
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) connection_factory: Option<&'a WorkerConnectionFactory<'a>>,
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
    let label = crate::plan::labels::dispatched_base_oid(dispatched_base_oid);
    let remove_labels = issue
        .labels
        .iter()
        .filter(|existing| {
            crate::plan::labels::parse_dispatched_base_oid(existing).is_some()
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

#[allow(clippy::too_many_arguments)]
async fn apply_session_overrides(
    connection: &mut dyn AgentConnection,
    initialize: &spur_acp::InitializeResponse,
    session_response: &spur_acp::NewSessionResponse,
    agent_kind: spur_acp::types::AgentKind,
    profile: Option<&str>,
    strategy: &spur_acp::ProfileStrategy,
    model: Option<&str>,
    effort: Option<&str>,
    config_overrides: Option<&std::collections::HashMap<String, String>>,
) {
    let caps = spur_acp::SpurAgentCaps::new(initialize, session_response, agent_kind);
    let session_id = session_response.session_id.clone();
    let session_id_for_log = session_id.0.to_string();

    if let Some(profile) = profile {
        match &strategy.select {
            spur_acp::SelectStrategy::ConfigOption { id } => {
                let request = spur_acp::SetSessionConfigOptionRequest::new(
                    session_id.clone(),
                    spur_acp::SessionConfigId::new(id.as_str()),
                    spur_acp::SessionConfigValueId::new(profile),
                );
                if let Err(error) = connection.set_session_config_option(request).await {
                    tracing::warn!(
                        target: "spur::worker::profile",
                        session_id = %session_id_for_log,
                        config_id = %id,
                        value = %profile,
                        error = %error,
                        "profile selection failed; default persona"
                    );
                }
            }
            spur_acp::SelectStrategy::SessionMode => {
                let request =
                    spur_acp::SetSessionModeRequest::new(session_id.clone(), profile.to_string());
                if let Err(error) = connection.set_session_mode(request).await {
                    tracing::warn!(
                        target: "spur::worker::profile",
                        session_id = %session_id_for_log,
                        value = %profile,
                        error = %error,
                        "profile set_mode failed; default persona"
                    );
                }
            }
            spur_acp::SelectStrategy::None => {
                tracing::debug!(
                    target: "spur::worker::profile",
                    session_id = %session_id_for_log,
                    value = %profile,
                    "kind has no selection surface; skipped"
                );
            }
        }
    }

    if let Some(model) = model {
        let config_id = caps
            .model_option()
            .map(|option| option.id.clone())
            .unwrap_or_else(|| spur_acp::SessionConfigId::new("model"));
        let config_id_for_log = config_id.0.to_string();
        let request = spur_acp::SetSessionConfigOptionRequest::new(
            session_id.clone(),
            config_id,
            spur_acp::SessionConfigValueId::new(model),
        );

        if let Err(error) = connection.set_session_config_option(request).await {
            tracing::warn!(
                target: "spur::worker::model_override",
                session_id = %session_id_for_log,
                config_id = %config_id_for_log,
                value = %model,
                error = %error,
                "worker model override failed"
            );
        }
    }

    if let Some(effort) = effort {
        if let Some(option) =
            spur_acp::spur_agent_caps::thought_level_option_from(&caps.config_options)
        {
            let config_id = option.id.clone();
            let config_id_for_log = config_id.0.to_string();
            let request = spur_acp::SetSessionConfigOptionRequest::new(
                session_id.clone(),
                config_id,
                spur_acp::SessionConfigValueId::new(effort),
            );

            if let Err(error) = connection.set_session_config_option(request).await {
                tracing::warn!(
                    target: "spur::worker::effort_override",
                    session_id = %session_id_for_log,
                    config_id = %config_id_for_log,
                    value = %effort,
                    error = %error,
                    "worker effort override failed"
                );
            }
        } else {
            tracing::debug!(
                target: "spur::worker::effort_override",
                session_id = %session_id_for_log,
                value = %effort,
                "worker effort override skipped"
            );
        }
    }

    if let Some(config_overrides) = config_overrides {
        for (config_id, value) in config_overrides {
            let request = spur_acp::SetSessionConfigOptionRequest::new(
                session_id.clone(),
                spur_acp::SessionConfigId::new(config_id.as_str()),
                spur_acp::SessionConfigValueId::new(value.as_str()),
            );

            if let Err(error) = connection.set_session_config_option(request).await {
                tracing::warn!(
                    target: "spur::worker::config_override",
                    session_id = %session_id_for_log,
                    config_id = %config_id,
                    value = %value,
                    error = %error,
                    "worker config override failed"
                );
            }
        }
    }
}

async fn materialize_profile(
    worktrees: &WorktreeManager,
    worktree_path: &std::path::Path,
    kind: spur_acp::types::AgentKind,
    strategy: &spur_acp::ProfileStrategy,
    profile: &crate::agent_profiles::AgentProfile,
) {
    if !strategy.materialize {
        return;
    }

    let Some(rendered) = crate::agent_profiles::render::render_for_kind(profile, kind) else {
        return;
    };

    let target = worktree_path.join(&rendered.rel_path);
    let ours = worktrees
        .worktree_excluded_paths(worktree_path)
        .await
        .iter()
        .any(|path| path == &rendered.rel_path);
    if target.exists() && !ours {
        tracing::warn!(
            target: "spur::worker::profile",
            path = %rendered.rel_path,
            "committed agent file exists; select-only against it"
        );
        return;
    }

    if let Some(parent) = target.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                target: "spur::worker::profile",
                path = %rendered.rel_path,
                error = %error,
                "profile directory creation failed; select-only"
            );
            return;
        }
    }
    if let Err(error) = std::fs::write(&target, &rendered.contents) {
        tracing::warn!(
            target: "spur::worker::profile",
            path = %rendered.rel_path,
            error = %error,
            "profile write failed; select-only"
        );
        return;
    }

    if let Err(error) = worktrees
        .add_worktree_excludes(worktree_path, std::slice::from_ref(&rendered.rel_path))
        .await
    {
        let _ = std::fs::remove_file(&target);
        tracing::warn!(
            target: "spur::worker::profile",
            path = %rendered.rel_path,
            error = %error,
            "exclude setup failed; removed injected file, select-only"
        );
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
    if let Some(server) = &ctx.worker_mcp_server {
        server.register_delegation_worktree_root(
            ctx.request_id.to_string(),
            worktree_info.path.clone(),
        );
    }

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
    crate::plan::emit_worker_started_audit(
        ctx.pm_service.map(|pm| pm as &dyn crate::plan::PmLike),
        &ctx.issue_id,
        ctx.feature_gate,
        ctx.request_id,
        &worktree_info.branch,
        &worker_session.0,
        &dispatched_base_oid,
    )
    .await;

    let profile_strategy = spur_acp::ProfileStrategy::resolve(
        ctx.agent_config.kind,
        ctx.agent_config.profile.as_ref(),
    );
    if let Some(profile_def) = ctx.profile_def {
        materialize_profile(
            worktrees,
            &worktree_info.path,
            ctx.agent_config.kind,
            &profile_strategy,
            profile_def,
        )
        .await;
    }

    // 2. Spawn worker agent in worktree via AgentConnection.
    // Workers never receive a permission_tx, so L2 auto-approve is
    // implicitly always on for them. skip_permissions still has effect
    // via L1a (spawn args).
    let spawn_args = ctx.agent_config.effective_args();
    #[cfg(any(test, feature = "test-support"))]
    let mut connection: Box<dyn AgentConnection> =
        if let Some(connection_factory) = ctx.connection_factory {
            connection_factory(ctx.agent_config, spawn_args, &worktrees.repo_root)
        } else {
            connection::build_connection_from_transport(
                ctx.agent_config,
                spawn_args,
                None,
                &worktrees.repo_root,
            )
        };
    #[cfg(not(any(test, feature = "test-support")))]
    let mut connection: Box<dyn AgentConnection> = connection::build_connection_from_transport(
        ctx.agent_config,
        spawn_args,
        None,
        &worktrees.repo_root,
    );

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

    if let Some(request_rx) = connection.take_agent_client_request_rx() {
        crate::notification_pump::spawn_agent_client_request_pump(
            request_rx,
            worker_session.clone(),
            funnel.clone(),
        );
    }

    let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
    let init_response = match connection.initialize(init_request).await {
        Ok(response) => response,
        Err(e) => {
            let _ = worktrees.remove_worktree(&worker_session).await;
            return Err(AttemptSetupError::InitFailed(e.to_string()));
        }
    };

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
    // `execute_delegation`). Default-on: this slice carries exactly one
    // `spur-worker-mcp` entry (whose URL embeds the per-delegation HMAC
    // token) unless the flag is `Some(false)`, in which case it is empty.
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

    apply_session_overrides(
        &mut *connection,
        &init_response,
        &session_response,
        ctx.agent_config.kind,
        ctx.profile,
        &profile_strategy,
        ctx.model,
        ctx.effort,
        ctx.config_overrides,
    )
    .await;

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
    let prompt_usage = match prompt_result {
        Ok(outcome) => Some(outcome.usage),
        Err(e) => {
            worker_success = false;
            output_text = format!("Failed to prompt worker: {e}");
            None
        }
    }
    .flatten();

    if worker_success {
        if let (Some(bundle), Some(pc)) = (ctx.peer_mailbox, peer_context) {
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
    }

    close_worker_session_best_effort(&mut *connection, &worker_session).await;
    delete_worker_session_best_effort(&mut *connection, &worker_session).await;
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
    let cost = estimate_prompt_cost(
        ctx.agent_config.cost_tier,
        duration,
        prompt_usage.as_ref(),
        Some(ctx.agent_config.name.as_str()),
    );

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
            kind: spur_acp::domain::outcome::OutcomeBlobKind::RawStdout,
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

async fn close_worker_session_best_effort(
    connection: &mut dyn AgentConnection,
    worker_session: &SessionId,
) {
    match connection
        .close_session(spur_acp::CloseSessionRequest::new(worker_session.0.clone()))
        .await
    {
        Ok(_) => tracing::debug!(
            session = %worker_session,
            "closed worker ACP session during delegation teardown"
        ),
        Err(spur_acp::AcpError::CapabilityMissing(_)) => tracing::debug!(
            session = %worker_session,
            "worker ACP session close unsupported during delegation teardown"
        ),
        Err(error) => tracing::warn!(
            session = %worker_session,
            %error,
            "worker ACP session close failed during delegation teardown"
        ),
    }
}

async fn delete_worker_session_best_effort(
    connection: &mut dyn AgentConnection,
    worker_session: &SessionId,
) {
    match connection
        .delete_session(spur_acp::DeleteSessionRequest::new(
            worker_session.0.clone(),
        ))
        .await
    {
        Ok(_) => tracing::debug!(
            session = %worker_session,
            "deleted worker ACP session during delegation teardown"
        ),
        Err(spur_acp::AcpError::CapabilityMissing(_)) => tracing::debug!(
            session = %worker_session,
            "worker ACP session delete unsupported during delegation teardown"
        ),
        Err(error) => tracing::warn!(
            session = %worker_session,
            %error,
            "worker ACP session delete failed during delegation teardown"
        ),
    }
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
            "crates/spur-core/src/server/mod.rs".to_string(),
            "crates/spur-acp/src/adapter/claude.rs".to_string(),
        ];
        let out = format_worker_task("Go.", &files);
        let idx_first = out
            .find("- crates/spur-core/src/server/mod.rs")
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
mod model_effort_override_tests {
    use super::*;
    use async_trait::async_trait;
    use futures::Stream;
    use std::collections::HashMap;
    use std::path::Path;
    use std::pin::Pin;
    use std::process::Command;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct SetConfigCall {
        session_id: String,
        config_id: String,
        value: String,
    }

    struct OverrideRecordingConnection {
        calls: Arc<Mutex<Vec<SetConfigCall>>>,
        rejected_config_id: Option<String>,
    }

    #[async_trait]
    impl AgentConnection for OverrideRecordingConnection {
        async fn initialize(
            &mut self,
            _request: InitializeRequest,
        ) -> anyhow::Result<spur_acp::InitializeResponse> {
            panic!("OverrideRecordingConnection::initialize must not be called")
        }

        async fn new_session(
            &mut self,
            _cwd: PathBuf,
            _mcp_servers: Vec<McpServer>,
        ) -> anyhow::Result<spur_acp::NewSessionResponse> {
            panic!("OverrideRecordingConnection::new_session must not be called")
        }

        async fn prompt(
            &mut self,
            _request: PromptRequest,
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = spur_acp::SessionNotification> + Send>>>
        {
            Ok(Box::pin(futures::stream::empty()))
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

        async fn set_session_config_option(
            &mut self,
            request: spur_acp::SetSessionConfigOptionRequest,
        ) -> anyhow::Result<spur_acp::SetSessionConfigOptionResponse> {
            let call = SetConfigCall {
                session_id: request.session_id.0.to_string(),
                config_id: request.config_id.0.to_string(),
                value: request.value.0.to_string(),
            };
            self.calls
                .lock()
                .expect("set config recorder poisoned")
                .push(call);

            if self
                .rejected_config_id
                .as_ref()
                .is_some_and(|id| id == request.config_id.0.as_ref())
            {
                return Err(anyhow::anyhow!("rejected {}", request.config_id.0));
            }

            Ok(spur_acp::SetSessionConfigOptionResponse::new(vec![]))
        }
    }

    #[derive(Clone, Debug)]
    struct CapturedEvent {
        level: tracing::Level,
        target: String,
        fields: String,
    }

    #[derive(Clone, Default)]
    struct CapturedEvents {
        events: Arc<Mutex<Vec<CapturedEvent>>>,
    }

    impl CapturedEvents {
        fn contains(&self, level: tracing::Level, target: &str, needle: &str) -> bool {
            self.events
                .lock()
                .expect("event capture poisoned")
                .iter()
                .any(|event| {
                    event.level == level && event.target == target && event.fields.contains(needle)
                })
        }
    }

    impl tracing::Subscriber for CapturedEvents {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::DEBUG
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut visitor = StringVisitor::default();
            event.record(&mut visitor);
            self.events
                .lock()
                .expect("event capture poisoned")
                .push(CapturedEvent {
                    level: *event.metadata().level(),
                    target: event.metadata().target().to_string(),
                    fields: visitor.0,
                });
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[derive(Default)]
    struct StringVisitor(String);

    impl Visit for StringVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.push_str(&format!("{}={value:?};", field.name()));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.push_str(&format!("{}={value};", field.name()));
        }
    }

    fn fixture_select_option(
        id: &str,
        current: &str,
        choices: &[(&str, &str)],
        category: spur_acp::SessionConfigOptionCategory,
    ) -> spur_acp::SessionConfigOption {
        let opts: Vec<spur_acp::SessionConfigSelectOption> = choices
            .iter()
            .map(|(value, name)| {
                spur_acp::SessionConfigSelectOption::new(
                    spur_acp::SessionConfigValueId::new(*value),
                    *name,
                )
            })
            .collect();
        spur_acp::SessionConfigOption::select(
            spur_acp::SessionConfigId::new(id),
            id,
            spur_acp::SessionConfigValueId::new(current),
            opts,
        )
        .category(category)
    }

    fn session_response(
        config_options: Vec<spur_acp::SessionConfigOption>,
    ) -> spur_acp::NewSessionResponse {
        let mut response = spur_acp::NewSessionResponse::new(spur_acp::AcpSessionId::new("acp-1"));
        response.config_options = Some(config_options);
        response
    }

    async fn apply_with(
        calls: Arc<Mutex<Vec<SetConfigCall>>>,
        rejected_config_id: Option<&str>,
        session_response: &spur_acp::NewSessionResponse,
        model: Option<&str>,
        effort: Option<&str>,
        config_overrides: Option<&HashMap<String, String>>,
    ) -> CapturedEvents {
        let init = spur_acp::InitializeResponse::new(spur_acp::ProtocolVersion::LATEST);
        let mut connection = OverrideRecordingConnection {
            calls,
            rejected_config_id: rejected_config_id.map(str::to_owned),
        };
        let events = CapturedEvents::default();
        let _serialize = crate::tracing_test_lock::guard();
        let _guard = tracing::subscriber::set_default(events.clone());

        let strategy = spur_acp::ProfileStrategy::for_kind(spur_acp::types::AgentKind::CodexAcp);
        apply_session_overrides(
            &mut connection,
            &init,
            session_response,
            spur_acp::types::AgentKind::CodexAcp,
            None,
            &strategy,
            model,
            effort,
            config_overrides,
        )
        .await;

        events
    }

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

    fn setup_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "t@t"]);
        git(dir.path(), &["config", "user.name", "t"]);
        std::fs::write(dir.path().join("README.md"), "base\n").expect("write readme");
        std::fs::create_dir_all(dir.path().join(".spur/worktrees")).expect("create worktree dir");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "base"]);
        dir
    }

    #[tokio::test]
    async fn no_model_or_effort_makes_no_set_config_calls() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let response = session_response(vec![]);

        apply_with(Arc::clone(&calls), None, &response, None, None, None).await;

        assert!(calls
            .lock()
            .expect("set config recorder poisoned")
            .is_empty());
    }

    #[tokio::test]
    async fn model_override_uses_advertised_model_option_id() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let response = session_response(vec![fixture_select_option(
            "vendor_model",
            "default",
            &[("gpt-5-codex", "GPT-5 Codex")],
            spur_acp::SessionConfigOptionCategory::Model,
        )]);

        apply_with(
            Arc::clone(&calls),
            None,
            &response,
            Some("gpt-5-codex"),
            None,
            None,
        )
        .await;

        assert_eq!(
            *calls.lock().expect("set config recorder poisoned"),
            vec![SetConfigCall {
                session_id: "acp-1".to_string(),
                config_id: "vendor_model".to_string(),
                value: "gpt-5-codex".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn effort_override_uses_advertised_thought_level_option_id() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let response = session_response(vec![fixture_select_option(
            "thinking_level",
            "medium",
            &[("high", "High")],
            spur_acp::SessionConfigOptionCategory::ThoughtLevel,
        )]);

        apply_with(
            Arc::clone(&calls),
            None,
            &response,
            None,
            Some("high"),
            None,
        )
        .await;

        assert_eq!(
            *calls.lock().expect("set config recorder poisoned"),
            vec![SetConfigCall {
                session_id: "acp-1".to_string(),
                config_id: "thinking_level".to_string(),
                value: "high".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn model_override_is_applied_before_effort_override() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let response = session_response(vec![
            fixture_select_option(
                "model",
                "default",
                &[("gpt-5-codex", "GPT-5 Codex")],
                spur_acp::SessionConfigOptionCategory::Model,
            ),
            fixture_select_option(
                "reasoning_effort",
                "medium",
                &[("high", "High")],
                spur_acp::SessionConfigOptionCategory::ThoughtLevel,
            ),
        ]);

        apply_with(
            Arc::clone(&calls),
            None,
            &response,
            Some("gpt-5-codex"),
            Some("high"),
            None,
        )
        .await;

        assert_eq!(
            *calls.lock().expect("set config recorder poisoned"),
            vec![
                SetConfigCall {
                    session_id: "acp-1".to_string(),
                    config_id: "model".to_string(),
                    value: "gpt-5-codex".to_string(),
                },
                SetConfigCall {
                    session_id: "acp-1".to_string(),
                    config_id: "reasoning_effort".to_string(),
                    value: "high".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn rejected_model_override_warns_and_effort_still_runs() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let response = session_response(vec![
            fixture_select_option(
                "model",
                "default",
                &[("gpt-5-codex", "GPT-5 Codex")],
                spur_acp::SessionConfigOptionCategory::Model,
            ),
            fixture_select_option(
                "reasoning_effort",
                "medium",
                &[("high", "High")],
                spur_acp::SessionConfigOptionCategory::ThoughtLevel,
            ),
        ]);

        let events = apply_with(
            Arc::clone(&calls),
            Some("model"),
            &response,
            Some("gpt-5-codex"),
            Some("high"),
            None,
        )
        .await;

        assert_eq!(calls.lock().expect("set config recorder poisoned").len(), 2);
        assert!(
            events.contains(
                tracing::Level::WARN,
                "spur::worker::model_override",
                "worker model override failed"
            ),
            "expected model override warning, got {:?}",
            events.events.lock().expect("event capture poisoned")
        );
    }

    #[tokio::test]
    async fn config_overrides_apply_raw_config_ids() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let response = session_response(vec![]);
        let config_overrides = HashMap::from([
            ("mode".to_string(), "plan".to_string()),
            ("context_window".to_string(), "200000".to_string()),
        ]);

        apply_with(
            Arc::clone(&calls),
            None,
            &response,
            None,
            None,
            Some(&config_overrides),
        )
        .await;

        let mut actual = calls.lock().expect("set config recorder poisoned").clone();
        actual.sort_by(|a, b| a.config_id.cmp(&b.config_id));
        assert_eq!(
            actual,
            vec![
                SetConfigCall {
                    session_id: "acp-1".to_string(),
                    config_id: "context_window".to_string(),
                    value: "200000".to_string(),
                },
                SetConfigCall {
                    session_id: "acp-1".to_string(),
                    config_id: "mode".to_string(),
                    value: "plan".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn rejected_config_override_warns_and_other_override_still_runs() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let response = session_response(vec![]);
        let config_overrides = HashMap::from([
            ("mode".to_string(), "plan".to_string()),
            ("context_window".to_string(), "200000".to_string()),
        ]);

        let events = apply_with(
            Arc::clone(&calls),
            Some("mode"),
            &response,
            None,
            None,
            Some(&config_overrides),
        )
        .await;

        let actual = calls.lock().expect("set config recorder poisoned");
        assert_eq!(actual.len(), 2);
        assert!(actual
            .iter()
            .any(|call| call.config_id == "context_window" && call.value == "200000"));
        assert!(
            events.contains(
                tracing::Level::WARN,
                "spur::worker::config_override",
                "worker config override failed"
            ),
            "expected config override warning, got {:?}",
            events.events.lock().expect("event capture poisoned")
        );
    }

    #[tokio::test]
    async fn model_effort_and_config_overrides_apply_in_order() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let response = session_response(vec![
            fixture_select_option(
                "model",
                "default",
                &[("gpt-5-codex", "GPT-5 Codex")],
                spur_acp::SessionConfigOptionCategory::Model,
            ),
            fixture_select_option(
                "reasoning_effort",
                "medium",
                &[("high", "High")],
                spur_acp::SessionConfigOptionCategory::ThoughtLevel,
            ),
        ]);
        let config_overrides = HashMap::from([("mode".to_string(), "plan".to_string())]);

        apply_with(
            Arc::clone(&calls),
            None,
            &response,
            Some("gpt-5-codex"),
            Some("high"),
            Some(&config_overrides),
        )
        .await;

        assert_eq!(
            *calls.lock().expect("set config recorder poisoned"),
            vec![
                SetConfigCall {
                    session_id: "acp-1".to_string(),
                    config_id: "model".to_string(),
                    value: "gpt-5-codex".to_string(),
                },
                SetConfigCall {
                    session_id: "acp-1".to_string(),
                    config_id: "reasoning_effort".to_string(),
                    value: "high".to_string(),
                },
                SetConfigCall {
                    session_id: "acp-1".to_string(),
                    config_id: "mode".to_string(),
                    value: "plan".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn missing_effort_option_debugs_and_skips_effort() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let response = session_response(vec![fixture_select_option(
            "model",
            "default",
            &[("gpt-5-codex", "GPT-5 Codex")],
            spur_acp::SessionConfigOptionCategory::Model,
        )]);

        let events = apply_with(
            Arc::clone(&calls),
            None,
            &response,
            Some("gpt-5-codex"),
            Some("high"),
            None,
        )
        .await;

        assert_eq!(
            *calls.lock().expect("set config recorder poisoned"),
            vec![SetConfigCall {
                session_id: "acp-1".to_string(),
                config_id: "model".to_string(),
                value: "gpt-5-codex".to_string(),
            }]
        );
        assert!(
            events.contains(
                tracing::Level::DEBUG,
                "spur::worker::effort_override",
                "worker effort override skipped"
            ),
            "expected effort override debug event, got {:?}",
            events.events.lock().expect("event capture poisoned")
        );
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum WorkerPathEvent {
        Initialize,
        NewSession,
        SetConfig { config_id: String, value: String },
        Prompt,
    }

    struct WorkerPathRecordingConnection {
        events: Arc<Mutex<Vec<WorkerPathEvent>>>,
    }

    #[async_trait]
    impl AgentConnection for WorkerPathRecordingConnection {
        async fn initialize(
            &mut self,
            _request: InitializeRequest,
        ) -> anyhow::Result<spur_acp::InitializeResponse> {
            self.events
                .lock()
                .expect("worker path recorder poisoned")
                .push(WorkerPathEvent::Initialize);
            Ok(spur_acp::InitializeResponse::new(
                spur_acp::ProtocolVersion::LATEST,
            ))
        }

        async fn new_session(
            &mut self,
            _cwd: PathBuf,
            _mcp_servers: Vec<McpServer>,
        ) -> anyhow::Result<spur_acp::NewSessionResponse> {
            self.events
                .lock()
                .expect("worker path recorder poisoned")
                .push(WorkerPathEvent::NewSession);
            let mut response =
                spur_acp::NewSessionResponse::new(spur_acp::AcpSessionId::new("worker-acp"));
            response.config_options = Some(vec![fixture_select_option(
                "model",
                "default",
                &[("gpt-5-codex", "GPT-5 Codex")],
                spur_acp::SessionConfigOptionCategory::Model,
            )]);
            Ok(response)
        }

        async fn prompt(
            &mut self,
            _request: PromptRequest,
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = spur_acp::SessionNotification> + Send>>>
        {
            let mut events = self.events.lock().expect("worker path recorder poisoned");
            let set_config_idx = events.iter().position(|event| {
                matches!(
                    event,
                    WorkerPathEvent::SetConfig { config_id, value }
                        if config_id == "model" && value == "gpt-5-codex"
                )
            });
            let prompt_idx = events
                .iter()
                .position(|event| matches!(event, WorkerPathEvent::Prompt));
            assert!(
                set_config_idx.is_some(),
                "model override missing before prompt"
            );
            assert!(prompt_idx.is_none(), "prompt called more than once");
            events.push(WorkerPathEvent::Prompt);
            Ok(Box::pin(futures::stream::empty()))
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

        async fn set_session_config_option(
            &mut self,
            request: spur_acp::SetSessionConfigOptionRequest,
        ) -> anyhow::Result<spur_acp::SetSessionConfigOptionResponse> {
            self.events
                .lock()
                .expect("worker path recorder poisoned")
                .push(WorkerPathEvent::SetConfig {
                    config_id: request.config_id.0.to_string(),
                    value: request.value.0.to_string(),
                });
            Ok(spur_acp::SetSessionConfigOptionResponse::new(vec![]))
        }
    }

    #[tokio::test]
    async fn worker_attempt_applies_model_override_before_prompt() {
        let repo = setup_repo();
        let mut worktrees = spur_worktree::manager::WorktreeManager::new(repo.path().to_path_buf());
        let (funnel, _events_rx) = crate::event_funnel::test_channel();
        let brain_session_id = spur_acp::BrainSessionId::new(SessionId::new());
        let worker_session = SessionId::new();
        let mut agent_config = spur_acp::AgentConfig::with_defaults("codex");
        agent_config.kind = spur_acp::types::AgentKind::CodexAcp;
        let fault_hooks = FaultInjectionHooks::default();
        let feature_gate = spur_license::FeatureGate::new_with_install_id(
            spur_license::policy::PolicyResolver::embedded(),
            spur_license::InstallId::from_uuid(uuid::Uuid::nil()),
        );
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_for_factory = Arc::clone(&events);

        let outcome = run_one_worker_attempt(
            worker_session.clone(),
            WorkerAttemptCtx {
                brain_session_id: &brain_session_id,
                agent: "codex",
                model: Some("gpt-5-codex"),
                effort: None,
                profile: None,
                profile_def: None,
                config_overrides: None,
                task: "do the task",
                request_id: "delegation-1",
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
                connection_factory: Some(&move |_cfg, _spawn_args, _repo_root| {
                    Box::new(WorkerPathRecordingConnection {
                        events: Arc::clone(&events_for_factory),
                    })
                }),
            },
            &mut worktrees,
            &funnel,
        )
        .await
        .expect("worker attempt succeeds");

        assert_eq!(outcome.worker_session, worker_session);
        assert_eq!(
            *events.lock().expect("worker path recorder poisoned"),
            vec![
                WorkerPathEvent::Initialize,
                WorkerPathEvent::NewSession,
                WorkerPathEvent::SetConfig {
                    config_id: "model".to_string(),
                    value: "gpt-5-codex".to_string(),
                },
                WorkerPathEvent::Prompt,
            ]
        );
    }
}

#[cfg(test)]
mod profile_override_tests {
    use super::*;
    use async_trait::async_trait;
    use futures::Stream;
    use std::path::Path;
    use std::pin::Pin;
    use std::process::Command;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum OverrideCall {
        Config { config_id: String, value: String },
        Mode { mode_id: String },
        Prompt,
    }

    struct ProfileRecordingConnection {
        calls: Arc<Mutex<Vec<OverrideCall>>>,
        rejected_config_id: Option<String>,
        reject_mode: bool,
        session_response: spur_acp::NewSessionResponse,
    }

    #[async_trait]
    impl AgentConnection for ProfileRecordingConnection {
        async fn initialize(
            &mut self,
            _request: InitializeRequest,
        ) -> anyhow::Result<spur_acp::InitializeResponse> {
            Ok(spur_acp::InitializeResponse::new(
                spur_acp::ProtocolVersion::LATEST,
            ))
        }

        async fn new_session(
            &mut self,
            _cwd: PathBuf,
            _mcp_servers: Vec<McpServer>,
        ) -> anyhow::Result<spur_acp::NewSessionResponse> {
            Ok(self.session_response.clone())
        }

        async fn prompt(
            &mut self,
            _request: PromptRequest,
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = spur_acp::SessionNotification> + Send>>>
        {
            self.calls
                .lock()
                .expect("profile recorder poisoned")
                .push(OverrideCall::Prompt);
            Ok(Box::pin(futures::stream::empty()))
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

        async fn set_session_config_option(
            &mut self,
            request: spur_acp::SetSessionConfigOptionRequest,
        ) -> anyhow::Result<spur_acp::SetSessionConfigOptionResponse> {
            self.calls
                .lock()
                .expect("profile recorder poisoned")
                .push(OverrideCall::Config {
                    config_id: request.config_id.0.to_string(),
                    value: request.value.0.to_string(),
                });
            if self
                .rejected_config_id
                .as_ref()
                .is_some_and(|id| id == request.config_id.0.as_ref())
            {
                return Err(anyhow::anyhow!("rejected {}", request.config_id.0));
            }
            Ok(spur_acp::SetSessionConfigOptionResponse::new(vec![]))
        }

        async fn set_session_mode(
            &mut self,
            request: spur_acp::SetSessionModeRequest,
        ) -> anyhow::Result<spur_acp::SetSessionModeResponse> {
            self.calls
                .lock()
                .expect("profile recorder poisoned")
                .push(OverrideCall::Mode {
                    mode_id: request.mode_id.0.to_string(),
                });
            if self.reject_mode {
                return Err(anyhow::anyhow!("rejected {}", request.mode_id.0));
            }
            Ok(spur_acp::SetSessionModeResponse::new())
        }
    }

    #[derive(Clone, Debug)]
    struct CapturedEvent {
        level: tracing::Level,
        target: String,
        fields: String,
    }

    #[derive(Clone, Default)]
    struct CapturedEvents {
        events: Arc<Mutex<Vec<CapturedEvent>>>,
    }

    impl CapturedEvents {
        fn contains(&self, level: tracing::Level, target: &str, needle: &str) -> bool {
            self.events
                .lock()
                .expect("event capture poisoned")
                .iter()
                .any(|event| {
                    event.level == level && event.target == target && event.fields.contains(needle)
                })
        }
    }

    impl tracing::Subscriber for CapturedEvents {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::DEBUG
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut visitor = StringVisitor::default();
            event.record(&mut visitor);
            self.events
                .lock()
                .expect("event capture poisoned")
                .push(CapturedEvent {
                    level: *event.metadata().level(),
                    target: event.metadata().target().to_string(),
                    fields: visitor.0,
                });
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[derive(Default)]
    struct StringVisitor(String);

    impl Visit for StringVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.push_str(&format!("{}={value:?};", field.name()));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.push_str(&format!("{}={value};", field.name()));
        }
    }

    fn fixture_select_option(
        id: &str,
        current: &str,
        choices: &[(&str, &str)],
        category: spur_acp::SessionConfigOptionCategory,
    ) -> spur_acp::SessionConfigOption {
        let opts: Vec<spur_acp::SessionConfigSelectOption> = choices
            .iter()
            .map(|(value, name)| {
                spur_acp::SessionConfigSelectOption::new(
                    spur_acp::SessionConfigValueId::new(*value),
                    *name,
                )
            })
            .collect();
        spur_acp::SessionConfigOption::select(
            spur_acp::SessionConfigId::new(id),
            id,
            spur_acp::SessionConfigValueId::new(current),
            opts,
        )
        .category(category)
    }

    fn session_response(
        config_options: Vec<spur_acp::SessionConfigOption>,
    ) -> spur_acp::NewSessionResponse {
        let mut response = spur_acp::NewSessionResponse::new(spur_acp::AcpSessionId::new("acp-1"));
        response.config_options = Some(config_options);
        response
    }

    fn model_and_effort_session_response() -> spur_acp::NewSessionResponse {
        session_response(vec![
            fixture_select_option(
                "model",
                "default",
                &[("opus", "Opus"), ("sonnet", "Sonnet")],
                spur_acp::SessionConfigOptionCategory::Model,
            ),
            fixture_select_option(
                "reasoning_effort",
                "medium",
                &[("high", "High")],
                spur_acp::SessionConfigOptionCategory::ThoughtLevel,
            ),
        ])
    }

    async fn apply_profile_with(
        kind: spur_acp::types::AgentKind,
        strategy: spur_acp::ProfileStrategy,
        profile: Option<&str>,
        model: Option<&str>,
        effort: Option<&str>,
        rejected_config_id: Option<&str>,
        reject_mode: bool,
    ) -> (Vec<OverrideCall>, CapturedEvents) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let init = spur_acp::InitializeResponse::new(spur_acp::ProtocolVersion::LATEST);
        let response = model_and_effort_session_response();
        let mut connection = ProfileRecordingConnection {
            calls: Arc::clone(&calls),
            rejected_config_id: rejected_config_id.map(str::to_owned),
            reject_mode,
            session_response: response.clone(),
        };
        let events = CapturedEvents::default();
        let _serialize = crate::tracing_test_lock::guard();
        let _guard = tracing::subscriber::set_default(events.clone());

        apply_session_overrides(
            &mut connection,
            &init,
            &response,
            kind,
            profile,
            &strategy,
            model,
            effort,
            None,
        )
        .await;

        let recorded = calls.lock().expect("profile recorder poisoned").clone();
        (recorded, events)
    }

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

    fn setup_repo_with_committed_agent() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "t@t"]);
        git(dir.path(), &["config", "user.name", "t"]);
        std::fs::write(dir.path().join("README.md"), "base\n").expect("write readme");
        std::fs::create_dir_all(dir.path().join(".spur/worktrees")).expect("create worktree dir");
        std::fs::create_dir_all(dir.path().join(".claude/agents"))
            .expect("create committed agent dir");
        std::fs::write(
            dir.path().join(".claude/agents/code-reviewer.md"),
            "committed persona\n",
        )
        .expect("write committed agent");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "base"]);
        dir
    }

    fn managed_profile() -> crate::agent_profiles::AgentProfile {
        crate::agent_profiles::AgentProfile::parse(
            "code-reviewer",
            "---\nname: code-reviewer\ndescription: Reviews diffs\nmodel: opus\neffort: high\n---\nmanaged persona\n",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn no_profile_makes_no_selection_rpc_and_preserves_model_effort_order() {
        let (calls, _) = apply_profile_with(
            spur_acp::types::AgentKind::CodexAcp,
            spur_acp::ProfileStrategy::for_kind(spur_acp::types::AgentKind::CodexAcp),
            None,
            Some("opus"),
            Some("high"),
            None,
            false,
        )
        .await;

        assert_eq!(
            calls,
            vec![
                OverrideCall::Config {
                    config_id: "model".to_string(),
                    value: "opus".to_string(),
                },
                OverrideCall::Config {
                    config_id: "reasoning_effort".to_string(),
                    value: "high".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn claude_profile_selects_agent_before_model_and_effort() {
        let (calls, _) = apply_profile_with(
            spur_acp::types::AgentKind::ClaudeCodeAcp,
            spur_acp::ProfileStrategy::for_kind(spur_acp::types::AgentKind::ClaudeCodeAcp),
            Some("code-reviewer"),
            Some("opus"),
            Some("high"),
            None,
            false,
        )
        .await;

        assert_eq!(
            calls,
            vec![
                OverrideCall::Config {
                    config_id: "agent".to_string(),
                    value: "code-reviewer".to_string(),
                },
                OverrideCall::Config {
                    config_id: "model".to_string(),
                    value: "opus".to_string(),
                },
                OverrideCall::Config {
                    config_id: "reasoning_effort".to_string(),
                    value: "high".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn opencode_profile_selects_mode_config_option() {
        let (calls, _) = apply_profile_with(
            spur_acp::types::AgentKind::OpenCode,
            spur_acp::ProfileStrategy::for_kind(spur_acp::types::AgentKind::OpenCode),
            Some("code-reviewer"),
            None,
            None,
            None,
            false,
        )
        .await;

        assert_eq!(
            calls,
            vec![OverrideCall::Config {
                config_id: "mode".to_string(),
                value: "code-reviewer".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn kiro_profile_uses_session_mode_not_config_option() {
        let (calls, _) = apply_profile_with(
            spur_acp::types::AgentKind::Kiro,
            spur_acp::ProfileStrategy::for_kind(spur_acp::types::AgentKind::Kiro),
            Some("code-reviewer"),
            None,
            None,
            None,
            false,
        )
        .await;

        assert_eq!(
            calls,
            vec![OverrideCall::Mode {
                mode_id: "code-reviewer".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn codex_profile_has_no_selection_rpc_but_model_still_applies() {
        let (calls, _) = apply_profile_with(
            spur_acp::types::AgentKind::CodexAcp,
            spur_acp::ProfileStrategy::for_kind(spur_acp::types::AgentKind::CodexAcp),
            Some("code-reviewer"),
            Some("opus"),
            None,
            None,
            false,
        )
        .await;

        assert_eq!(
            calls,
            vec![OverrideCall::Config {
                config_id: "model".to_string(),
                value: "opus".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn rejected_profile_selection_warns_and_model_still_runs() {
        let (calls, events) = apply_profile_with(
            spur_acp::types::AgentKind::ClaudeCodeAcp,
            spur_acp::ProfileStrategy::for_kind(spur_acp::types::AgentKind::ClaudeCodeAcp),
            Some("code-reviewer"),
            Some("opus"),
            None,
            Some("agent"),
            false,
        )
        .await;

        assert_eq!(
            calls,
            vec![
                OverrideCall::Config {
                    config_id: "agent".to_string(),
                    value: "code-reviewer".to_string(),
                },
                OverrideCall::Config {
                    config_id: "model".to_string(),
                    value: "opus".to_string(),
                },
            ]
        );
        assert!(
            events.contains(
                tracing::Level::WARN,
                "spur::worker::profile",
                "profile selection failed; default persona"
            ),
            "expected profile selection warning, got {:?}",
            events.events.lock().expect("event capture poisoned")
        );
    }

    #[tokio::test]
    async fn d8_profile_defaults_feed_model_rpc_but_request_model_wins() {
        let profile = managed_profile();
        let (model, effort) =
            crate::orchestrator::delegation::execute::resolve_effective_model_effort(
                None,
                None,
                Some(&profile),
            );
        let (calls, _) = apply_profile_with(
            spur_acp::types::AgentKind::ClaudeCodeAcp,
            spur_acp::ProfileStrategy::for_kind(spur_acp::types::AgentKind::ClaudeCodeAcp),
            Some("code-reviewer"),
            model.as_deref(),
            effort.as_deref(),
            None,
            false,
        )
        .await;
        assert_eq!(
            calls,
            vec![
                OverrideCall::Config {
                    config_id: "agent".to_string(),
                    value: "code-reviewer".to_string(),
                },
                OverrideCall::Config {
                    config_id: "model".to_string(),
                    value: "opus".to_string(),
                },
                OverrideCall::Config {
                    config_id: "reasoning_effort".to_string(),
                    value: "high".to_string(),
                },
            ]
        );

        let (model, effort) =
            crate::orchestrator::delegation::execute::resolve_effective_model_effort(
                Some("sonnet"),
                None,
                Some(&profile),
            );
        let (calls, _) = apply_profile_with(
            spur_acp::types::AgentKind::ClaudeCodeAcp,
            spur_acp::ProfileStrategy::for_kind(spur_acp::types::AgentKind::ClaudeCodeAcp),
            Some("code-reviewer"),
            model.as_deref(),
            effort.as_deref(),
            None,
            false,
        )
        .await;
        assert_eq!(
            calls,
            vec![
                OverrideCall::Config {
                    config_id: "agent".to_string(),
                    value: "code-reviewer".to_string(),
                },
                OverrideCall::Config {
                    config_id: "model".to_string(),
                    value: "sonnet".to_string(),
                },
                OverrideCall::Config {
                    config_id: "reasoning_effort".to_string(),
                    value: "high".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn materialization_collision_leaves_non_owned_file_and_still_selects_profile() {
        let repo = setup_repo_with_committed_agent();
        let mut worktrees = spur_worktree::manager::WorktreeManager::new(repo.path().to_path_buf());
        let (funnel, _events_rx) = crate::event_funnel::test_channel();
        let brain_session_id = spur_acp::BrainSessionId::new(SessionId::new());
        let worker_session = SessionId::new();
        let mut agent_config = spur_acp::AgentConfig::with_defaults("claude-code");
        agent_config.kind = spur_acp::types::AgentKind::ClaudeCodeAcp;
        let profile = managed_profile();
        let fault_hooks = FaultInjectionHooks::default();
        let feature_gate = spur_license::FeatureGate::new_with_install_id(
            spur_license::policy::PolicyResolver::embedded(),
            spur_license::InstallId::from_uuid(uuid::Uuid::nil()),
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let calls_for_factory = Arc::clone(&calls);
        let session = session_response(vec![]);

        let outcome = run_one_worker_attempt(
            worker_session.clone(),
            WorkerAttemptCtx {
                brain_session_id: &brain_session_id,
                agent: "claude-code",
                profile: Some("code-reviewer"),
                profile_def: Some(&profile),
                model: None,
                effort: None,
                config_overrides: None,
                task: "do the task",
                request_id: "delegation-1",
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
                connection_factory: Some(&move |_cfg, _spawn_args, _repo_root| {
                    Box::new(ProfileRecordingConnection {
                        calls: Arc::clone(&calls_for_factory),
                        rejected_config_id: None,
                        reject_mode: false,
                        session_response: session.clone(),
                    })
                }),
            },
            &mut worktrees,
            &funnel,
        )
        .await
        .expect("worker attempt succeeds");

        assert_eq!(outcome.worker_session, worker_session);
        assert_eq!(
            std::fs::read_to_string(
                outcome
                    .worktree_path
                    .join(".claude/agents/code-reviewer.md")
            )
            .unwrap(),
            "committed persona\n"
        );
        assert_eq!(
            *calls.lock().expect("profile recorder poisoned"),
            vec![
                OverrideCall::Config {
                    config_id: "agent".to_string(),
                    value: "code-reviewer".to_string(),
                },
                OverrideCall::Prompt,
            ]
        );
    }
}

#[cfg(test)]
mod delete_worker_session_tests {
    use super::*;
    use async_trait::async_trait;
    use futures::Stream;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    struct DeleteRecordingConnection {
        deleted_sessions: Arc<Mutex<Vec<String>>>,
        fail_delete: bool,
    }

    #[async_trait]
    impl AgentConnection for DeleteRecordingConnection {
        async fn initialize(
            &mut self,
            _request: InitializeRequest,
        ) -> anyhow::Result<spur_acp::InitializeResponse> {
            panic!("DeleteRecordingConnection::initialize must not be called")
        }

        async fn new_session(
            &mut self,
            _cwd: PathBuf,
            _mcp_servers: Vec<McpServer>,
        ) -> anyhow::Result<spur_acp::NewSessionResponse> {
            panic!("DeleteRecordingConnection::new_session must not be called")
        }

        async fn prompt(
            &mut self,
            _request: PromptRequest,
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = spur_acp::SessionNotification> + Send>>>
        {
            Ok(Box::pin(futures::stream::empty()))
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

        async fn delete_session(
            &mut self,
            request: spur_acp::DeleteSessionRequest,
        ) -> Result<spur_acp::DeleteSessionResponse, spur_acp::AcpError> {
            self.deleted_sessions
                .lock()
                .expect("delete recorder poisoned")
                .push(request.session_id.to_string());
            if self.fail_delete {
                return Err(spur_acp::AcpError::CapabilityMissing("session/delete"));
            }
            Ok(spur_acp::DeleteSessionResponse::new())
        }
    }

    #[tokio::test]
    async fn delete_worker_session_best_effort_calls_connection_delete() {
        let deleted_sessions = Arc::new(Mutex::new(Vec::new()));
        let mut connection = DeleteRecordingConnection {
            deleted_sessions: Arc::clone(&deleted_sessions),
            fail_delete: false,
        };

        delete_worker_session_best_effort(
            &mut connection,
            &SessionId("worker-session".to_string()),
        )
        .await;

        assert_eq!(
            *deleted_sessions.lock().expect("delete recorder poisoned"),
            vec!["worker-session".to_string()]
        );
    }

    #[tokio::test]
    async fn delete_worker_session_best_effort_swallows_delete_errors() {
        let deleted_sessions = Arc::new(Mutex::new(Vec::new()));
        let mut connection = DeleteRecordingConnection {
            deleted_sessions: Arc::clone(&deleted_sessions),
            fail_delete: true,
        };

        delete_worker_session_best_effort(
            &mut connection,
            &SessionId("worker-session".to_string()),
        )
        .await;

        assert_eq!(
            *deleted_sessions.lock().expect("delete recorder poisoned"),
            vec!["worker-session".to_string()]
        );
    }
}

#[cfg(test)]
mod close_worker_session_tests {
    use super::*;
    use async_trait::async_trait;
    use futures::Stream;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    struct CloseRecordingConnection {
        closed_sessions: Arc<Mutex<Vec<String>>>,
        fail_close: bool,
    }

    #[async_trait]
    impl AgentConnection for CloseRecordingConnection {
        async fn initialize(
            &mut self,
            _request: InitializeRequest,
        ) -> anyhow::Result<spur_acp::InitializeResponse> {
            panic!("CloseRecordingConnection::initialize must not be called")
        }

        async fn new_session(
            &mut self,
            _cwd: PathBuf,
            _mcp_servers: Vec<McpServer>,
        ) -> anyhow::Result<spur_acp::NewSessionResponse> {
            panic!("CloseRecordingConnection::new_session must not be called")
        }

        async fn prompt(
            &mut self,
            _request: PromptRequest,
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = spur_acp::SessionNotification> + Send>>>
        {
            Ok(Box::pin(futures::stream::empty()))
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

        async fn close_session(
            &mut self,
            request: spur_acp::CloseSessionRequest,
        ) -> Result<spur_acp::CloseSessionResponse, spur_acp::AcpError> {
            self.closed_sessions
                .lock()
                .expect("close recorder poisoned")
                .push(request.session_id.to_string());
            if self.fail_close {
                return Err(spur_acp::AcpError::CapabilityMissing("session/close"));
            }
            Ok(spur_acp::CloseSessionResponse::new())
        }
    }

    #[tokio::test]
    async fn close_worker_session_best_effort_calls_connection_close() {
        let closed_sessions = Arc::new(Mutex::new(Vec::new()));
        let mut connection = CloseRecordingConnection {
            closed_sessions: Arc::clone(&closed_sessions),
            fail_close: false,
        };

        close_worker_session_best_effort(&mut connection, &SessionId("worker-session".to_string()))
            .await;

        assert_eq!(
            *closed_sessions.lock().expect("close recorder poisoned"),
            vec!["worker-session".to_string()]
        );
    }

    #[tokio::test]
    async fn close_worker_session_best_effort_swallows_close_errors() {
        let closed_sessions = Arc::new(Mutex::new(Vec::new()));
        let mut connection = CloseRecordingConnection {
            closed_sessions: Arc::clone(&closed_sessions),
            fail_close: true,
        };

        close_worker_session_best_effort(&mut connection, &SessionId("worker-session".to_string()))
            .await;

        assert_eq!(
            *closed_sessions.lock().expect("close recorder poisoned"),
            vec!["worker-session".to_string()]
        );
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
