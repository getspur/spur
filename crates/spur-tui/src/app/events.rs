use super::*;
use futures::StreamExt;
use tokio::sync::broadcast;
use tokio::sync::oneshot;
use tokio::time::timeout;

type UpgradeResult = Result<Option<spur_core::UpgradeBanner>, oneshot::error::RecvError>;

pub(super) async fn next_upgrade_result(upgrade_rx: &mut Option<UpgradeReceiver>) -> UpgradeResult {
    match upgrade_rx.as_mut() {
        Some(rx) => rx.await,
        None => std::future::pending().await,
    }
}

impl App {
    pub(super) fn handle_upgrade_result(&mut self, result: UpgradeResult) {
        self.upgrade_rx = None;
        if let Ok(Some(info)) = result {
            self.show_user_warning(format!(
                "SPUR {} is available; current {}. Run: spur upgrade",
                info.latest, info.current
            ));
        }
    }

    fn update_license_state(&mut self, license_state: LicenseStateEvent) {
        let resolved = license_state_event_to_state(&license_state);
        self.feature_gate.update_state(&resolved);
        self.license_badge = license_badge_from_state(&license_state);
        self.license_state = license_state;
        self.dirty = true;
    }

    /// Look up the `AgentConfig` for an agent by name (`AgentConfig::name`)
    /// in the loaded `SpurConfig`. Falls back to a minimal synthesized
    /// config when the agent isn't declared — this preserves startup
    /// behavior when no `.spur/config.toml` is present.
    fn resolve_agent_config(&self, name: &str) -> std::sync::Arc<spur_acp::AgentConfig> {
        self.config
            .agents
            .entries
            .iter()
            .find(|e| e.name == name)
            .cloned()
            .map(std::sync::Arc::new)
            .unwrap_or_else(|| {
                tracing::warn!(
                    agent = %name,
                    "agent not found in config.toml — using PromptText fallback; \
                     vendor-ext commands will not be registered"
                );
                std::sync::Arc::new(Self::fallback_agent_config(name))
            })
    }

    fn fallback_agent_config(name: &str) -> spur_acp::AgentConfig {
        spur_acp::AgentConfig::with_defaults(name)
    }

    /// Derive the `WorkerMentionDescriptor` snapshot from the loaded
    /// agent config. Filtered to roles that can serve as a worker
    /// (matches `AgentRegistry::worker_capable` semantics).
    fn build_worker_snapshot(&self) -> Vec<crate::mentions::WorkerMentionDescriptor> {
        use spur_acp::config::Tier;
        use spur_acp::types::AgentRole;
        self.config
            .agents
            .entries
            .iter()
            .filter(|cfg| matches!(cfg.role, AgentRole::Worker | AgentRole::Both))
            .map(|cfg| crate::mentions::WorkerMentionDescriptor {
                name: cfg.name.clone(),
                description: cfg.delegation.description.clone(),
                tier: cfg.delegation.tier.map(|t| match t {
                    Tier::Specialist => "specialist".to_string(),
                    Tier::Generalist => "generalist".to_string(),
                }),
            })
            .collect()
    }

    /// Refresh Dashboard's worker mention snapshot from the current app config.
    /// This is the canonical hook point for any future config-reload event.
    pub(super) fn sync_dashboard_workers(&mut self) {
        let workers = self.build_worker_snapshot();
        self.dashboard.set_worker_snapshot(workers);
    }

    /// Persist metadata, surfacing read-only refusals to the user via an
    /// App-owned top-level warning banner. This is deliberately not routed
    /// through `InputBar::set_status`: event handling calls `sync_brain_status`
    /// after view updates, which can overwrite InputBar status labels before
    /// the user sees the warning.
    pub(super) fn persist_metadata(&mut self, context: &'static str) -> bool {
        match self.metadata_store.save() {
            Ok(()) => true,
            Err(e) => {
                if e.downcast_ref::<ReadOnlyFutureSchema>().is_some() {
                    self.show_user_warning(format!(
                        "Read-only mode: session metadata was written by a newer SPUR. {context} not saved. Upgrade SPUR to enable writes."
                    ));
                } else {
                    tracing::warn!(error = %e, context, "failed to persist metadata");
                }
                false
            }
        }
    }

    /// Forward a SpurEvent to all views that need it.
    pub fn handle_spur_event(&mut self, event: SpurEvent) {
        // Always fold into the lineage projection first. The projection is a
        // pure function of the event stream — view code reads from it later.
        self.lineage.apply(&event);
        #[cfg(feature = "analytics")]
        self.sync_live_cost_active_sessions();
        self.plan_projection.apply(&event);
        self.synopsis.apply(&event);

        // Route worker stream updates into per-executor ReactTraces.
        // Orphan drop: skip events whose executor the lineage doesn't
        // know yet, to avoid materializing a trace with AgentKind::Generic
        // that would never be corrected. Matches the brain view's fidelity
        // ceiling (events before SessionDetailView construction are lost).
        if let spur_acp::domain::events::SpurEventBody::WorkerNotification {
            executor_id,
            notification,
            ..
        } = &event.body
        {
            let exec_id = spur_core::lineage::types::ExecutorId::new(executor_id);
            if let Some(node) = self.lineage.node(&exec_id) {
                let agent_name = node.agent.clone();
                self.worker_streams
                    .route(executor_id, &agent_name, &notification.update);
            } else {
                tracing::trace!(
                    executor_id = %executor_id,
                    "dropping WorkerNotification for unknown executor (orphan)"
                );
            }
        }

        // Seed the per-executor trace from its stream_buffer on spawn.
        // For a fresh live ExecutorSpawned the buffer is empty (harmless no-op).
        // On replay the buffer may already be populated from subsequent replayed
        // events, so the Stream tab has content for pre-existing executors before
        // new WorkerNotifications arrive. One-time per executor — subsequent
        // WorkerNotification events append on top of the seeded entries.
        if let spur_acp::domain::events::SpurEventBody::ExecutorSpawned { id, .. } = &event.body {
            let exec_id = spur_core::lineage::types::ExecutorId::new(id);
            if let Some(node) = self.lineage.node(&exec_id) {
                let agent = node.agent.clone();
                let entries: Vec<_> = node.stream_buffer.iter().cloned().collect();
                self.worker_streams
                    .seed_from_stream_buffer(id, &agent, entries.iter());
            }
        }

        // Reset per-executor trace on retry. Mirrors the lineage
        // projection's `node.stream_buffer.clear()` on the same event.
        if let spur_acp::domain::events::SpurEventBody::ExecutorRetryStarted { id, .. } =
            &event.body
        {
            self.worker_streams.reset(id);
        }

        self.dirty = true;

        // Handle session list responses before forwarding to views
        match &event.body {
            SpurEventBody::SessionsListed { agent, sessions } => {
                if let Some(ref mut picker) = self.session_picker {
                    picker.set_sessions(agent.clone(), sessions.clone(), &self.synopsis);
                }
                return;
            }
            SpurEventBody::SessionsListError { message } => {
                if let Some(ref mut picker) = self.session_picker {
                    picker.set_error(message.clone());
                }
                return;
            }
            SpurEventBody::AuthRequired { session, message } => {
                if let Some(ref mut detail) = self.session_detail {
                    // Apply when the event matches the focused session OR when
                    // the event carries a sentinel/empty session id (spawn-side
                    // failures that happen before a session id is allocated).
                    let matches_focused = session.0 == detail.session_id().0;
                    let is_sentinel =
                        session.0.is_empty() || session.0 == "00000000-0000-0000-0000-000000000000";
                    if matches_focused || is_sentinel {
                        detail.auth_error = Some(message.clone());
                    } else {
                        tracing::trace!(
                            event_session = %session.0,
                            focused_session = %detail.session_id().0,
                            "AuthRequired for non-focused session; dropping"
                        );
                    }
                } else {
                    tracing::trace!("AuthRequired received but no session_detail focused");
                }
                return;
            }
            SpurEventBody::SessionHistory { entries, .. } => {
                tracing::info!(
                    entry_count = entries.len(),
                    has_session_detail = self.session_detail.is_some(),
                    "SessionHistory: replaying history"
                );
                if let Some(ref mut detail) = self.session_detail {
                    detail.replay_history(entries);
                    tracing::info!(
                        trace_entries = detail.trace_entry_count(),
                        "SessionHistory: replay complete"
                    );
                } else {
                    tracing::warn!("SessionHistory: session_detail is None, history lost!");
                }

                // Backfill global input history from replayed user messages
                // so Ctrl-P recalls past inputs even from older sessions.
                let mut changed = false;
                {
                    let hist = &mut self.metadata_store.metadata_mut().input_history;
                    for entry in entries {
                        if entry.role == "user" {
                            let history_entry = InputHistoryEntry::from_text(entry.text.clone());
                            changed |= Self::merge_input_history_entry(hist, history_entry);
                        }
                    }
                }
                if changed {
                    self.persist_metadata("backfilled input history");
                    self.sync_input_history();
                }

                return;
            }
            // Variants outside the session-list / auth pre-routing surface.
            // They flow through to the brain-status match below and the view
            // fan-out at the end of `handle_spur_event`. Logged at debug so
            // that a future variant added to `SpurEventBody` without a
            // routing decision is visible (R3: observability requires
            // explicitness — see docs/architecture.md §Risk Register #3).
            _ => {
                tracing::debug!(
                    seq = event.seq,
                    "SpurEventBody not pre-routed by session-list match; deferring to brain-status match + view fan-out"
                );
            }
        }

        // Track brain status transitions
        match &event.body {
            SpurEventBody::BrainConnectStarted { brain } => {
                self.brain_status = BrainStatus::Connecting;
                self.brain_name = Some(brain.clone());
            }
            SpurEventBody::BrainConnected { brain } => {
                self.brain_status = BrainStatus::Connected;
                self.brain_name = Some(brain.clone());
            }
            SpurEventBody::BrainConnectFailed { brain, reason } => {
                self.brain_status = BrainStatus::Error(reason.clone());
                self.brain_name = Some(brain.clone());
                self.pending_first_user_message = None;
            }
            SpurEventBody::BrainSpawned { agent, session } => {
                self.brain_status = BrainStatus::Thinking;
                self.brain_name = Some(agent.clone());
                self.sync_dashboard_workers();

                // Only create a new SessionDetailView if none exists or the
                // session ID changed. Replacing unconditionally would wipe any
                // user message that was just pushed to the trace.
                let needs_new = match &self.session_detail {
                    Some(detail) => detail.session_id() != session,
                    None => true,
                };
                if needs_new {
                    // Carry-over: a cleared view's InputBar text belongs to the NEW
                    // session, not the retired one. Capture owned text before
                    // dropping the old view. Source-level gating in
                    // force_save_draft / draft_save_action (spec §3.5) means
                    // force_flush_active_draft is a no-op for a cleared view, so
                    // no call-site gating is required here.
                    let carryover: Option<String> = self
                        .session_detail
                        .as_ref()
                        .filter(|d| d.is_cleared())
                        .map(|d| d.input_bar_text());
                    tracing::debug!(
                        carryover_len = carryover.as_deref().map(str::len).unwrap_or(0),
                        "view-replacement: clear-carryover capture"
                    );
                    self.force_flush_active_draft();

                    let agent_cfg = self.resolve_agent_config(agent);
                    let mut view = SessionDetailView::new_with_issue_snapshot(
                        session.clone(),
                        agent.clone(),
                        "brain".to_string(),
                        std::env::current_dir().unwrap_or_default(),
                        agent_cfg,
                        self.build_worker_snapshot(),
                        self.dashboard.tracked_issues().to_vec(),
                    );
                    #[cfg(feature = "markdown")]
                    view.set_render_picker(self.mermaid_picker.clone());
                    view.seed_input_history(self.metadata_store.metadata().input_history.clone());
                    if let Some(entry) = self.metadata_store.entry(&session.0) {
                        view.restore_draft(&entry.draft);
                    }
                    // Carry-over wins over any metadata draft (which is normally
                    // empty for a freshly-minted spur_session_id anyway).
                    // restore_draft is a no-op on empty input.
                    if let Some(text) = carryover.as_deref() {
                        view.restore_draft(text);
                    }
                    // Auto-resume banner — unchanged from the pre-revision branch.
                    if self
                        .metadata_store
                        .metadata()
                        .last_active_session_id
                        .as_deref()
                        == Some(session.0.as_str())
                    {
                        let title = self
                            .metadata_store
                            .entry(&session.0)
                            .and_then(|e| e.title_override.clone())
                            .unwrap_or_else(|| agent.clone());
                        let quit_ago = humanize_since(
                            self.metadata_store.metadata().last_active_at.as_deref(),
                        );
                        view.show_resume_banner(title, quit_ago);
                        self.metadata_store.clear_last_active();
                        self.persist_metadata("cleared last_active");
                    }
                    self.session_detail = Some(view);
                }

                // Sync edit mode to newly created session detail view.
                if let Some(ref mut detail) = self.session_detail {
                    detail.set_edit_mode(self.edit_mode);
                    detail.set_disable_paste_burst(self.config.tui.disable_paste_burst);
                }

                // Auto-navigate from Dashboard or SessionPicker
                if matches!(self.current_view, ViewId::Dashboard | ViewId::SessionPicker) {
                    self.navigate_to(ViewId::SessionDetail(session.clone()));
                }
            }
            SpurEventBody::AgentSessionReady {
                session,
                acp_session_id,
                brain,
                resumed: _,
                cancel_mode: _,
                fs_unsafe: _,
                caps,
            } => {
                if let Some(ref mut detail) = self.session_detail {
                    if detail.session_id() == session {
                        detail.set_spur_agent_caps(caps.clone());
                    }
                }
                self.metadata_store
                    .set_acp_mapping(&session.0, acp_session_id, brain);
                self.persist_metadata("AgentSessionReady metadata");
            }
            SpurEventBody::SessionAttachRejected {
                acp_session_id,
                holder,
                fs_unsafe: _,
            } => {
                self.collision_modal = Some(CollisionModalState {
                    acp_id: acp_session_id.clone(),
                    holder: holder.clone(),
                });
                self.dirty = true;
            }
            SpurEventBody::AgentNotification { session: _, .. } => {
                // Transition Thinking → Streaming on first output
                if self.brain_status == BrainStatus::Thinking {
                    self.brain_status = BrainStatus::Streaming;
                }
            }
            SpurEventBody::TurnComplete { session } => {
                self.brain_status = BrainStatus::Ready;
                let now = chrono::Utc::now().to_rfc3339();
                self.metadata_store.set_last_active(session.0.clone(), now);
                self.persist_metadata("last_active");
            }
            SpurEventBody::BrainError { message, .. } => {
                self.brain_status = BrainStatus::Error(message.clone());
                self.pending_first_user_message = None;
            }
            SpurEventBody::BrainReconnecting { .. } => {
                self.brain_status = BrainStatus::Thinking;
            }
            SpurEventBody::BrainReconnected { .. } => {
                self.brain_status = BrainStatus::Ready;
            }
            SpurEventBody::BrainReconnectFailed { reason, .. } => {
                self.brain_status = BrainStatus::Error(reason.clone());
                self.pending_first_user_message = None;
            }
            SpurEventBody::SessionCompleted { .. } => {
                self.brain_status = BrainStatus::Idle;
                self.pending_first_user_message = None;
            }
            SpurEventBody::BrainRetired { reason, .. } => {
                // Null per-App state that was tied to the retired session.
                // `brain_status` is intentionally NOT touched here:
                //  - UserClear: already set to Idle by the ClearSession
                //    action handler before the event round-trips back.
                //  - ResumeSwitch: the orchestrator's ResumeSession arm
                //    is already loading the next brain; overriding to
                //    Idle would race that transition.
                self.brain_name = None;
                self.pending_first_user_message = None;
                // Clear auto-resume pointers so /clear followed by a
                // process quit before the next prompt does not cause
                // spur-cli to auto-resume the just-retired session on
                // the next launch. The next `AgentSessionReady` (on the
                // next prompt) repopulates these via `set_acp_mapping`.
                self.metadata_store.clear_last_active_full();
                self.persist_metadata("cleared last_active on BrainRetired");
                // Defensive belt-and-suspenders reset for the UserClear path.
                // Idempotent against Action::ClearSession's eager reset.
                // Gated on UserClear only:
                //  - ResumeSwitch: in-flight ResumeSession is already loading the next
                //    brain via BrainSpawned (app.rs:919-975); resetting here would
                //    briefly blank the new view mid-load.
                //  - Shutdown: terminal; reset is moot.
                if matches!(reason, BrainRetireReason::UserClear) {
                    tracing::info!("BrainRetired{{UserClear}}: defensive view reset");
                    if let Some(ref mut detail) = self.session_detail {
                        detail.reset_for_clear();
                    }
                }
            }
            SpurEventBody::LicenseUpdated { state } => {
                self.update_license_state(state.clone());
            }
            // Variants that don't affect brain status — handled by views.
            SpurEventBody::DelegationRequested { .. }
            | SpurEventBody::DelegationCompleted { .. }
            | SpurEventBody::DelegationDispatched { .. }
            | SpurEventBody::WorkerSpawned { .. }
            | SpurEventBody::WorkerNotification { .. }
            | SpurEventBody::WorkerProgress { .. }
            | SpurEventBody::WorkerFileTouched { .. }
            | SpurEventBody::WorkerHeartbeat { .. }
            | SpurEventBody::ExecutorPhaseChanged { .. }
            | SpurEventBody::ExecutorRetryStarted { .. }
            | SpurEventBody::ExecutorArtifact { .. }
            | SpurEventBody::ExecutorReviewRequested { .. }
            | SpurEventBody::ExecutorReviewResolved { .. }
            | SpurEventBody::ExecutorReviewCancelled { .. }
            | SpurEventBody::CostUpdate { .. }
            | SpurEventBody::ConflictDetected { .. }
            | SpurEventBody::RateLimitDetected { .. }
            | SpurEventBody::BrainFailover { .. }
            | SpurEventBody::IssueReceived { .. }
            | SpurEventBody::PrCreated { .. }
            | SpurEventBody::IssueUpdated { .. }
            | SpurEventBody::PlanSnapshotUpdated { .. }
            | SpurEventBody::AgentExtNotification { .. } => {}
            // Catch-all for future variants — log so we notice.
            _ => {
                tracing::debug!("unhandled SpurEventBody variant in brain status tracking");
            }
        }

        if let SpurEventBody::PromptDispatched {
            session, turn_kind, ..
        } = &event.body
        {
            let matches_active = self
                .session_detail
                .as_ref()
                .is_some_and(|detail| detail.session_id() == session);
            let should_drain = matches_active
                && matches!(turn_kind.as_str(), "user_only" | "merged")
                && self.session_detail.as_ref().is_some_and(|detail| {
                    // App handles this before SessionDetailView can add a merged-turn Think note.
                    detail.trace_entry_count() == 0
                });
            if should_drain {
                if let Some(message) = self.pending_first_user_message.take() {
                    if let Some(ref mut detail) = self.session_detail {
                        detail.append_user_message(&message);
                    }
                }
            }
        }

        // Forward to views
        let ctx = crate::views::ViewContext {
            lineage: &self.lineage,
            plan_projection: &self.plan_projection,
            synopsis: &self.synopsis,
            brain_status: &self.brain_status,
            license_badge: self.license_badge.as_ref(),
            flag_summary: self.flag_summary,
            tombstone: None,
            transient_hint_override: None,
            theme: &self.theme,
        };
        let issue_snapshot_changed = matches!(
            &event.body,
            SpurEventBody::IssuesLoaded { .. }
                | SpurEventBody::IssueUpdated { .. }
                | SpurEventBody::IssueCreated { .. }
        );
        self.dashboard.handle_spur_event(&event, &ctx);
        if issue_snapshot_changed {
            if let Some(ref mut detail) = self.session_detail {
                detail.set_issue_snapshot(self.dashboard.tracked_issues().to_vec());
            }
        }
        if let Some(ref mut picker) = self.session_picker {
            picker.handle_spur_event(&event, &ctx);
        }
        if let Some(ref mut detail) = self.session_detail {
            detail.handle_spur_event(&event, &ctx);
        }
        if let Some(ref mut inspector) = self.plan_inspector {
            inspector.handle_spur_event(&event, &ctx);
        }
        if let Some(ref mut browser) = self.plan_browser {
            browser.handle_spur_event(&event, &ctx);
        }
        let issue_browser_pending = if let Some(ref mut browser) = self.issue_browser {
            browser.handle_spur_event(&event, &ctx);
            browser.take_pending_action()
        } else {
            None
        };
        if let Some(action) = issue_browser_pending {
            self.process_action(action);
        }

        // Sync status to InputBars
        self.sync_brain_status();
    }
}

// ─── Main TUI entry point ──────────────────────────────────────────────

/// Run the TUI dashboard, consuming events from the broadcast receiver.
pub async fn run_tui(
    event_rx: broadcast::Receiver<SpurEvent>,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
    perm_rx: Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>>,
    start_in_picker: bool,
) -> anyhow::Result<()> {
    let repo_root = std::env::current_dir()
        .ok()
        .and_then(|cwd| spur_core::project_root::discover(&cwd).ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    run_tui_with_license(
        event_rx,
        user_input_tx,
        perm_rx,
        start_in_picker.then_some(None),
        std::sync::Arc::new(spur_acp::SpurConfig::default()),
        App::default_license_state(PLACEHOLDER_STATUS_TEXT),
        crate::landing::LandingDecision::ShowDashboard,
        None,
        repo_root,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_tui_with_license(
    event_rx: broadcast::Receiver<SpurEvent>,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
    mut perm_rx: Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>>,
    start_in_picker_with_preselect: Option<Option<String>>,
    config: std::sync::Arc<spur_acp::SpurConfig>,
    license_state: LicenseStateEvent,
    landing: crate::landing::LandingDecision,
    config_path: Option<std::path::PathBuf>,
    repo_root: std::path::PathBuf,
    upgrade_rx: Option<tokio::sync::oneshot::Receiver<Option<spur_core::UpgradeBanner>>>,
) -> anyhow::Result<()> {
    let mut terminal = crate::tui::setup()?;
    let notebook_daemon = crate::notebook_daemon::Daemon::spawn();
    let mut app = App::build_with_license_state(
        user_input_tx,
        start_in_picker_with_preselect,
        config.clone(),
        license_state,
        landing,
        config_path,
        upgrade_rx,
    );
    let mut tick_interval = tokio::time::interval(Duration::from_millis(33));
    let mut event_stream = crossterm::event::EventStream::new();
    let mut event_rx = event_rx;

    // === bd-1vnk: rehydrate projections from prior NDJSON before drain begins ===
    spur_core::project_root::warn_on_nested_layout(&repo_root);
    let replay_cfg = spur_core::event_replay::ReplayConfig {
        events_dir: repo_root.join(".spur").join("events"),
        replay_horizon: std::time::Duration::from_secs(config.log.event_replay_horizon_secs),
        ..Default::default()
    };
    match spur_core::event_replay::replay_events(&replay_cfg, |ev| {
        app.lineage.apply(ev);
        app.plan_projection.apply(ev);
        app.synopsis.apply(ev);
    }) {
        Ok(stats) => tracing::info!(
            target: "spur.metrics.event_replay",
            files = stats.files_read,
            skipped_pid = stats.files_skipped_pid,
            applied = stats.events_applied,
            horizon_skipped = stats.events_skipped_horizon,
            malformed = stats.malformed_lines,
            elapsed_ms = stats.elapsed.as_millis() as u64,
        ),
        Err(e) => tracing::error!(
            error = %e,
            "event replay failed; starting with empty projections"
        ),
    }
    // ============================================================================

    // Bridge OS termination signals into the event loop so SIGINT/SIGTERM/SIGHUP/SIGQUIT
    // run the same teardown as Ctrl-C/Ctrl-Q (raw mode off → alt screen exit →
    // function returns → caller drops Orchestrator). SIGKILL is uncatchable;
    // the on-startup orphan sweep is the safety net for that case.
    //
    // mpsc(1) coalesces duplicate signals via try_send: Err(Full(_)) means a
    // shutdown is already pending, which is exactly what we want.
    let (_shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, Signal, SignalKind};
        // Graceful fallback on signal-registration failure: log + skip the
        // handler instead of panicking. A panic here would exit AFTER raw
        // mode + alt screen are entered, leaving the user with a corrupt
        // terminal (no echo, stuck alt screen). Rare in practice but
        // possible in sandboxed / fork-restricted / signalfd-disabled
        // environments. (bd-2j5e.5)
        fn install(kind: SignalKind, label: &str) -> Option<Signal> {
            match signal(kind) {
                Ok(s) => Some(s),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        signal = %label,
                        "failed to install signal handler; this signal will not gracefully shut down the TUI"
                    );
                    None
                }
            }
        }
        let mut sigterm = install(SignalKind::terminate(), "SIGTERM");
        let mut sighup = install(SignalKind::hangup(), "SIGHUP");
        let mut sigquit = install(SignalKind::quit(), "SIGQUIT");
        let tx = _shutdown_tx.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = tokio::signal::ctrl_c() => {
                        if let Err(error) = result {
                            tracing::warn!(%error, "SIGINT handler failed");
                            return;
                        }
                        let _ = tx.try_send(());
                    }
                    _ = async {
                        match sigterm.as_mut() {
                            Some(s) => { s.recv().await; }
                            None => std::future::pending::<()>().await,
                        }
                    } => { let _ = tx.try_send(()); }
                    _ = async {
                        match sighup.as_mut() {
                            Some(s) => { s.recv().await; }
                            None => std::future::pending::<()>().await,
                        }
                    } => { let _ = tx.try_send(()); }
                    _ = async {
                        match sigquit.as_mut() {
                            Some(s) => { s.recv().await; }
                            None => std::future::pending::<()>().await,
                        }
                    } => { let _ = tx.try_send(()); }
                }
            }
        });
    }

    loop {
        // Count how many events feed into each render. H1' detection.
        let mut spur_drained: u32 = 0;
        let mut crossterm_drained: u32 = 0;

        // Phase 1: Wait for at least one event (async yield point).
        tokio::select! {
            Some(Ok(crossterm_event)) = event_stream.next() => {
                crossterm_drained += 1;
                app.handle_crossterm_event(crossterm_event);
            }
            result = event_rx.recv() => {
                match result {
                    Ok(spur_event) => {
                        spur_drained += 1;
                        app.handle_spur_event(spur_event);
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            streaming_probe = true,
                            site = "E_broadcast_lag",
                            lagged_n = n,
                            source = file!(),
                            line = line!(),
                            "TUI broadcast receiver lagged — events dropped"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        app.should_quit = true;
                    }
                }
            }
            _ = tick_interval.tick() => {
                app.tick();
            }
            Some(perm) = async {
                match perm_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                app.handle_permission_request(perm);
            }
            result = next_upgrade_result(&mut app.upgrade_rx) => {
                app.handle_upgrade_result(result);
            }
            _ = shutdown_rx.recv() => {
                // SIGINT / SIGTERM / SIGHUP / SIGQUIT: take the same path as a confirmed
                // Ctrl-Q. confirm_quit() flushes drafts + sets should_quit; the
                // existing loop break runs the shared tui::teardown and the
                // function returns so the caller's host.shutdown().await issues
                // killpg and unregisters the pgid registry. Bypassing Drop here
                // (e.g., via std::process::exit) defeats the orphan-reaping
                // safety guarantees on catchable signals.
                app.confirm_quit();
            }
        }

        // Phase 2: Drain all remaining crossterm events (non-blocking).
        // This collapses bursts of mouse scroll events into one render pass.
        while let Ok(Some(Ok(ev))) = timeout(Duration::ZERO, event_stream.next()).await {
            crossterm_drained += 1;
            app.handle_crossterm_event(ev);
        }

        // Phase 3: Drain remaining spur events (non-blocking), capped per frame.
        //
        // S1.c (H1') — cap at DRAIN_CAP_PER_FRAME so bursts of streaming chunks
        // don't collapse into a single paint. Leftover events drain on the next
        // iteration; no event is lost, just deferred by one frame. `Lagged`
        // counts toward the cap so a subscriber that's badly behind still makes
        // progress instead of spinning on drop notifications.
        const DRAIN_CAP_PER_FRAME: u32 = 8;
        let mut drained_this_phase: u32 = 0;
        while drained_this_phase < DRAIN_CAP_PER_FRAME {
            match event_rx.try_recv() {
                Ok(spur_event) => {
                    spur_drained += 1;
                    drained_this_phase += 1;
                    app.handle_spur_event(spur_event);
                }
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    tracing::warn!(
                        streaming_probe = true,
                        site = "E_broadcast_lag",
                        lagged_n = n,
                        source = file!(),
                        line = line!(),
                        "TUI broadcast receiver lagged (drain phase) — events dropped"
                    );
                    drained_this_phase += 1;
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Closed) => break,
            }
        }

        // Phase 4: Single render pass.
        if app.dirty {
            if spur_drained > 0 || crossterm_drained > 0 {
                tracing::debug!(
                    streaming_probe = true,
                    site = "F_frame_drain",
                    spur_drained = spur_drained,
                    crossterm_drained = crossterm_drained,
                    "rendering frame"
                );
            }
            terminal.draw(|f| app.render(f))?;
            app.dirty = false;
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal first so the user regains control immediately,
    // even if the best-effort analytics checkpoint hits its 2s timeout.
    crate::tui::teardown(&mut terminal)?;
    notebook_daemon.shutdown().await;
    app.shutdown_analytics().await;
    Ok(())
}

pub async fn run_tui_with_config(
    event_rx: broadcast::Receiver<SpurEvent>,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
    perm_rx: Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>>,
    start_in_picker: bool,
    config: std::sync::Arc<spur_acp::SpurConfig>,
    config_path: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let repo_root = std::env::current_dir()
        .ok()
        .and_then(|cwd| spur_core::project_root::discover(&cwd).ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    run_tui_with_license(
        event_rx,
        user_input_tx,
        perm_rx,
        start_in_picker.then_some(None),
        config,
        App::default_license_state(PLACEHOLDER_STATUS_TEXT),
        crate::landing::LandingDecision::ShowDashboard,
        config_path,
        repo_root,
        None,
    )
    .await
}

/// Apply read-only session-scoped `SessionUpdate` variants to a
/// `SessionDetailView`. Variants not handled here are intentionally left to
/// the trace-rendering code in `session_detail::handle_spur_event`. Unknown
/// variants log at TRACE so future protocol additions don't crash the UI.
pub(crate) fn apply_session_update(
    state: &mut SessionDetailView,
    update: &spur_acp::SessionUpdate,
) {
    use spur_acp::SessionUpdate::*;
    match update {
        CurrentModeUpdate(u) => {
            state.set_current_mode(Some(u.current_mode_id.to_string()));
        }
        AvailableCommandsUpdate(u) => {
            state.apply_available_commands(&u.available_commands);
        }
        ConfigOptionUpdate(u) => {
            // Mid-session refresh: agent advertises a new snapshot of
            // session config options (e.g. external client mutated the
            // model/effort, or codex emits the post-load snapshot). Rebuild
            // synthesized advertised commands and refresh the cached
            // snapshot so any open SlashArg picker shows live choices.
            state.apply_advertised_commands(
                state.spur_agent_caps_cloned().as_deref(),
                &u.config_options,
            );
        }
        UsageUpdate(u) => {
            state.context_used = Some(u.used);
            state.context_size = Some(u.size);
        }
        SessionInfoUpdate(_) => {
            // M9 hoist: the cached title / updated_at moved to
            // BrainSession in spur-core. Wire-side ingestion of this
            // notification onto the orchestrator entry is tracked as
            // a follow-up; the explicit arm stays here so the variant
            // is still tagged in trace logs (vs. the catch-all silent
            // drop in `apply_session_update: unhandled variant`).
            tracing::trace!(
                "SessionInfoUpdate received in spur-tui — orchestrator-side ingestion is the canonical path post-M9"
            );
        }
        _ => {
            tracing::trace!("apply_session_update: unhandled variant");
        }
    }
}
