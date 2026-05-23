use super::*;

impl App {
    pub fn new(user_input_tx: Option<mpsc::Sender<UserInput>>, start_in_picker: bool) -> Self {
        Self::new_with_config(
            user_input_tx,
            start_in_picker,
            std::sync::Arc::new(spur_acp::SpurConfig::default()),
            crate::landing::LandingDecision::ShowDashboard,
        )
    }

    pub(super) fn default_license_state(message: &str) -> LicenseStateEvent {
        LicenseStateEvent {
            status: LicenseStatusEvent::Inactive,
            subject_kind: LicenseSubjectKind::Unknown,
            plan: EventLicensePlan::Unknown,
            features: Default::default(),
            expires_at: None,
            binding_mode: LicenseBindingMode::Unknown,
            offline_ok: false,
            status_text: message.to_string(),
        }
    }

    pub fn new_with_license(
        user_input_tx: Option<mpsc::Sender<UserInput>>,
        start_in_picker: bool,
        config: std::sync::Arc<spur_acp::SpurConfig>,
        license_state: LicenseStateEvent,
        landing: crate::landing::LandingDecision,
        config_path: Option<std::path::PathBuf>,
    ) -> Self {
        Self::build_with_license_state(
            user_input_tx,
            start_in_picker.then_some(None),
            config,
            license_state,
            landing,
            config_path,
            None,
        )
    }

    pub fn new_with_config(
        user_input_tx: Option<mpsc::Sender<UserInput>>,
        start_in_picker: bool,
        config: std::sync::Arc<spur_acp::SpurConfig>,
        landing: crate::landing::LandingDecision,
    ) -> Self {
        Self::new_with_license(
            user_input_tx,
            start_in_picker,
            config,
            Self::default_license_state(PLACEHOLDER_STATUS_TEXT),
            landing,
            None,
        )
    }

    pub(super) fn build_with_license_state(
        user_input_tx: Option<mpsc::Sender<UserInput>>,
        start_in_picker_with_preselect: Option<Option<String>>,
        config: std::sync::Arc<spur_acp::SpurConfig>,
        license_state: LicenseStateEvent,
        landing: crate::landing::LandingDecision,
        config_path: Option<std::path::PathBuf>,
        upgrade_rx: Option<UpgradeReceiver>,
    ) -> Self {
        let metadata_path = std::path::PathBuf::from(".spur").join("session_metadata.json");
        let mut app = Self::build_with_license_state_from_metadata_path(
            user_input_tx,
            start_in_picker_with_preselect,
            config,
            license_state,
            landing,
            metadata_path,
            config_path,
        );
        app.upgrade_rx = upgrade_rx;
        app
    }

    pub(super) fn build_with_license_state_from_metadata_path(
        user_input_tx: Option<mpsc::Sender<UserInput>>,
        start_in_picker_with_preselect: Option<Option<String>>,
        config: std::sync::Arc<spur_acp::SpurConfig>,
        license_state: LicenseStateEvent,
        landing: crate::landing::LandingDecision,
        metadata_path: std::path::PathBuf,
        config_path: Option<std::path::PathBuf>,
    ) -> Self {
        let metadata_store = SessionMetadataStore::load(&metadata_path);
        let start_in_picker = start_in_picker_with_preselect.is_some();

        // Resolve the active theme from `tui.theme` config. The runtime
        // loader logs and falls back internally; it never panics.
        let active_theme_name = config.tui.theme.clone();
        let (theme, theme_outcome) = crate::theme::load_runtime_theme(&active_theme_name);
        tracing::info!(
            target: "spur_tui::theme",
            theme = %theme.name,
            outcome = ?theme_outcome,
            "active theme resolved"
        );
        let theme = std::sync::Arc::new(theme);
        let (current_view, session_picker) = if let Some(preselect) = start_in_picker_with_preselect
        {
            let mut picker = SessionPickerView::with_preselect(preselect);
            picker.set_metadata(metadata_store.metadata().clone());
            (ViewId::SessionPicker, Some(picker))
        } else {
            (ViewId::Dashboard, None)
        };

        #[cfg(feature = "markdown")]
        let mermaid_picker = Picker::from_query_stdio().ok();
        #[cfg(feature = "markdown")]
        let (mermaid_tx, mermaid_rx) = tokio::sync::mpsc::unbounded_channel();
        #[cfg(feature = "analytics")]
        let live_cost_cache = std::sync::Arc::new(RwLock::new(LiveCostCache::default()));
        #[cfg(feature = "analytics")]
        let live_cost_active_sessions =
            std::sync::Arc::new(RwLock::new(std::collections::HashSet::new()));
        #[cfg(feature = "analytics")]
        let dashboard = DashboardView::with_cache(live_cost_cache.clone());
        #[cfg(not(feature = "analytics"))]
        let dashboard = DashboardView::new();

        let mut app = Self {
            current_view,
            view_history: Vec::new(),
            dashboard,
            session_detail: None,
            session_picker,
            plan_browser: None,
            plan_inspector: None,
            issue_browser: None,
            help_visible: false,
            quit_confirm_visible: false,
            collision_modal: None,
            upgrade_modal: None,
            should_quit: false,
            dirty: true, // initial render
            user_warning: None,
            upgrade_rx: None,
            user_input_tx,
            #[cfg(any(test, debug_assertions))]
            user_input_rx_for_test: None,
            brain_status: BrainStatus::Idle,
            brain_name: None,
            pending_first_user_message: None,
            pending_permission: None,
            lineage: ExecutorLineage::new(),
            #[cfg(feature = "analytics")]
            analytics_engine: None,
            #[cfg(feature = "analytics")]
            live_cost_cache: Some(live_cost_cache),
            #[cfg(feature = "analytics")]
            live_cost_active_sessions: Some(live_cost_active_sessions),
            #[cfg(feature = "analytics")]
            live_cost_signal_tx: None,
            #[cfg(feature = "analytics")]
            live_cost_handle: None,
            #[cfg(feature = "analytics")]
            insights_view: None,
            #[cfg(feature = "analytics")]
            insights_init: None,
            plan_projection: PlanProjectionStore::new(),
            synopsis: SessionSynopsisProjection::new(),
            worker_streams: crate::worker_streams::WorkerStreams::new(),
            #[cfg(feature = "markdown")]
            mermaid_picker,
            #[cfg(feature = "markdown")]
            mermaid_rx,
            #[cfg(feature = "markdown")]
            mermaid_tx,
            #[cfg(feature = "markdown")]
            mermaid_viewer: None,
            license_state,
            license_badge: None,
            flag_summary: None,
            feature_gate: spur_license::FeatureGate::new(
                spur_license::policy::PolicyResolver::embedded(),
            ),
            metadata_store,
            edit_mode: EditMode::from(config.tui.edit_mode),
            tombstones: crate::components::tombstone::TombstoneSlots::new(),
            tombstone_undo_replay: false,
            config,
            config_path,
            theme,
            active_theme_name,
            palette_visible: false,
            palette_state: crate::components::palette::PaletteState::new(),
            transient_hint: None,
            legacy_archive_hint_shown: false,
            legacy_issue_close_hint_shown: false,
            dashboard_tab_empty_deprecation_shown: false,
            esc_chain: VecDeque::new(),
            landing,
            #[cfg(any(test, debug_assertions))]
            last_action: None,
        };

        // `App::default_license_state` is a local "no runtime seed" placeholder.
        // Real provider states, including inactive LicenseSeat states, still
        // hydrate the gate through the normal fail-closed path.
        if !is_placeholder_license_state(&app.license_state) {
            let initial_license_state = license_state_event_to_state(&app.license_state);
            app.feature_gate.update_state(&initial_license_state);
        }

        // Propagate the config-derived edit_mode to the dashboard's input bar.
        // `InputBar::new()` hardcodes EditMode::Emacs; without this sync, a
        // user with `tui.edit_mode = "vim"` would see Emacs on the dashboard
        // composer until they toggled. SessionDetail is None at boot and
        // receives the mode on instantiation, so it does not need syncing here.
        app.dashboard.set_edit_mode(app.edit_mode);
        app.dashboard
            .set_disable_paste_burst(app.config.tui.disable_paste_burst);

        // Apply landing-specific setup
        if let crate::landing::LandingDecision::SetupRequired = &app.landing {
            app.dashboard.set_agents_configured(false);
        }
        if app.metadata_store.is_read_only() {
            app.show_user_warning(READ_ONLY_STARTUP_WARNING.to_string());
        }
        app.sync_dashboard_workers();
        #[cfg(feature = "analytics")]
        {
            app.sync_live_cost_active_sessions();
            app.spawn_live_cost_refresh();
        }

        app.license_badge = license_badge_from_state(&app.license_state);
        app.flag_summary = compute_flag_summary();

        // Validate every agent entry. Fatal errors abort the agent (but we don't
        // crash the whole TUI — other agents may still work). Warnings are logged
        // and we continue.
        for entry in &app.config.agents.entries {
            match spur_acp::validate_agent_config(entry) {
                Ok(()) => {}
                Err(errors) => {
                    for e in errors {
                        if e.is_fatal() {
                            tracing::error!(agent = %entry.name, error = %e,
                                "agent config validation failed; this agent will not be usable");
                        } else {
                            tracing::warn!(agent = %entry.name, warning = %e,
                                "agent config validation warning");
                        }
                    }
                }
            }
        }

        if start_in_picker {
            if let Some(ref tx) = app.user_input_tx {
                let _ = tx.try_send(UserInput::ListSessions);
            }
        }

        app.sync_input_history();

        app
    }
    /// Persist a draft to metadata. Callable both from the `Action::SaveDraft`
    /// handler (debounced tick path) and same-tick from exit-session boundaries
    /// via `force_flush_active_draft`.
    pub(super) fn apply_save_draft(&mut self, session_id: String, draft: String) {
        let entry = self.metadata_store.entry_mut(&session_id);
        if entry.draft != draft {
            entry.draft = draft;
            self.persist_metadata("draft");
        }
    }

    /// Append a submitted message to the global input history (dedup + cap).
    pub(super) fn push_input_history_entry(&mut self, entry: InputHistoryEntry) -> bool {
        if entry.snapshot.text.trim().is_empty() {
            return false;
        }
        let changed = {
            let hist = &mut self.metadata_store.metadata_mut().input_history;
            Self::merge_input_history_entry(hist, entry)
        };
        if changed {
            self.persist_metadata("input history");
            self.sync_input_history();
        }
        changed
    }

    pub(super) fn merge_input_history_entry(
        hist: &mut Vec<InputHistoryEntry>,
        entry: InputHistoryEntry,
    ) -> bool {
        if entry.snapshot.text.trim().is_empty() {
            return false;
        }
        hist.retain(|existing| !existing.same_recall_state(&entry));
        hist.push(entry);
        if hist.len() > HISTORY_CAP {
            hist.remove(0);
        }
        true
    }

    /// Reseed all active InputBars with the current global history.
    pub(super) fn sync_input_history(&mut self) {
        let hist = self.metadata_store.metadata().input_history.clone();
        self.dashboard.seed_input_history(hist.clone());
        if let Some(ref mut detail) = self.session_detail {
            detail.seed_input_history(hist);
        }
    }

    /// Synchronously flush the active SessionDetailView's unsent InputBar text
    /// to metadata, bypassing the 500ms debounce. Call at user-intent "exit
    /// session" boundaries (opening the picker, quit-confirm proceed, brain
    /// respawn for a different session id) so metadata reflects the latest
    /// on-screen text before anything reads it. No-op when no detail is active
    /// or the draft is unchanged since the last persist.
    pub(super) fn force_flush_active_draft(&mut self) {
        let Some(detail) = self.session_detail.as_mut() else {
            return;
        };
        if let Some(Action::SaveDraft { session_id, draft }) = detail.force_save_draft() {
            self.apply_save_draft(session_id, draft);
        }
    }

    /// Returns `Some(sid)` if the currently-active session has a non-empty
    /// persisted draft; else `None`. Used by the picker to decide whether to
    /// show the switch-safety confirm banner.
    fn compute_draft_session(&self) -> Option<String> {
        let detail = self.session_detail.as_ref()?;
        let sid = detail.session_id().0.clone();
        let has = self
            .metadata_store
            .entry(&sid)
            .map(|e| !e.draft.is_empty())
            .unwrap_or(false);
        if has {
            Some(sid)
        } else {
            None
        }
    }

    /// Push the current metadata snapshot AND current-draft awareness into the
    /// picker if one exists. Call from any action that mutates metadata.
    pub(super) fn refresh_picker_metadata(&mut self) {
        let draft = self.compute_draft_session();
        let current = self
            .session_detail
            .as_ref()
            .map(|d| d.session_id().0.clone());
        if let Some(ref mut picker) = self.session_picker {
            picker.set_metadata(self.metadata_store.metadata().clone());
            picker.set_current_session_has_draft(draft);
            picker.set_current_session_id(current);
        }
    }

    /// Push current brain status to both views' InputBars.
    pub(super) fn sync_brain_status(&mut self) {
        let session_attached = self
            .session_detail
            .as_ref()
            .is_some_and(|detail| !detail.is_cleared());
        let status_str = match &self.brain_status {
            BrainStatus::Idle => "idle",
            BrainStatus::Connecting => "connecting",
            BrainStatus::Connected => "connected",
            BrainStatus::Thinking => "thinking",
            BrainStatus::Streaming => "streaming",
            BrainStatus::Ready => "ready",
            BrainStatus::Error(_) => "error",
        };

        self.dashboard
            .set_brain_status(self.brain_name.as_deref(), status_str, session_attached);

        if let Some(ref mut detail) = self.session_detail {
            detail.set_brain_status(status_str);
        }
    }

    /// Read-only access to per-executor `ReactTrace` instances.
    pub fn worker_streams(&self) -> &crate::worker_streams::WorkerStreams {
        &self.worker_streams
    }

    /// Mutable access to per-executor `ReactTrace` instances.
    pub fn worker_streams_mut(&mut self) -> &mut crate::worker_streams::WorkerStreams {
        &mut self.worker_streams
    }

    pub fn plan_projection(&self) -> &PlanProjectionStore {
        &self.plan_projection
    }

    pub fn synopsis(&self) -> &SessionSynopsisProjection {
        &self.synopsis
    }
}
