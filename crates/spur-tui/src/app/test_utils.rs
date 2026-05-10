use super::*;

impl App {
    pub fn new_with_metadata_path_for_test(metadata_path: std::path::PathBuf) -> Self {
        Self::build_with_license_state_from_metadata_path(
            None,
            None,
            std::sync::Arc::new(spur_acp::SpurConfig::default()),
            Self::default_license_state(PLACEHOLDER_STATUS_TEXT),
            crate::landing::LandingDecision::ShowDashboard,
            metadata_path,
            None,
        )
    }

    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn new_with_metadata_path_in_picker_for_test(metadata_path: std::path::PathBuf) -> Self {
        Self::build_with_license_state_from_metadata_path(
            None,
            Some(None),
            std::sync::Arc::new(spur_acp::SpurConfig::default()),
            Self::default_license_state(PLACEHOLDER_STATUS_TEXT),
            crate::landing::LandingDecision::ShowDashboard,
            metadata_path,
            None,
        )
    }

    /// Test-only accessor: borrow the current `SessionDetailView`.
    #[doc(hidden)]
    pub fn session_detail_for_test(
        &self,
    ) -> Option<&crate::views::session_detail::SessionDetailView> {
        self.session_detail.as_ref()
    }

    /// Test-only accessor: borrow the current licensing snapshot.
    #[doc(hidden)]
    pub fn license_state_for_test(&self) -> &LicenseStateEvent {
        &self.license_state
    }

    /// Test-only accessor: borrow the current licensing badge projection.
    #[doc(hidden)]
    pub fn license_badge_for_test(&self) -> Option<&LicenseBadge> {
        self.license_badge.as_ref()
    }

    pub(crate) fn feature_enabled_for_test(&self, key: spur_license::FeatureKey) -> bool {
        spur_license::require_feature(&self.feature_gate, key).is_ok()
    }

    /// Test-only accessor: borrow the first message waiting for trace seeding.
    #[doc(hidden)]
    pub fn pending_first_user_message_for_test(&self) -> Option<&str> {
        self.pending_first_user_message.as_deref()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn handle_undo_for_test(&mut self) {
        let _ = self.handle_undo();
    }

    #[cfg(any(test, debug_assertions))]
    pub fn tombstones_for_test(&mut self) -> &mut crate::components::tombstone::TombstoneSlots {
        &mut self.tombstones
    }

    #[cfg(any(test, debug_assertions))]
    fn ensure_user_input_capture_for_test(&mut self) {
        if self.user_input_rx_for_test.is_none() {
            let (tx, rx) = mpsc::channel::<UserInput>(16);
            self.user_input_tx = Some(tx);
            self.user_input_rx_for_test = Some(rx);
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn add_pending_review_for_test(&mut self, executor_id: &str, attempt_n: u32) {
        use spur_acp::{ReviewKind, ReviewPayload, Role};

        self.ensure_user_input_capture_for_test();

        let executor = spur_core::ExecutorId(executor_id.to_string());
        if self.lineage.node(&executor).is_none() {
            self.lineage
                .apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
                    id: executor_id.into(),
                    parent_id: None,
                    session_id: SessionId(format!("session-{executor_id}")),
                    agent: "codex".into(),
                    role: Role::Executor,
                    task_spec: "test task".into(),
                }));
        }

        self.lineage
            .apply(&SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
                id: executor_id.into(),
                attempt_n,
                kind: ReviewKind::Completion,
                payload: ReviewPayload {
                    summary: "test pending review".into(),
                    diff_summary: None,
                    pr_url: None,
                    error: None,
                    delegation_plan: None,
                    chosen_matches_dispatched: None,
                    peer_influence: None,
                },
            }));
    }

    #[cfg(any(test, debug_assertions))]
    pub fn user_input_sent_for_test(&mut self) -> bool {
        self.user_input_sent_for_test_matching(None)
    }

    #[cfg(any(test, debug_assertions))]
    pub fn user_input_sent_for_test_with_executor(&mut self, executor_id: &str) -> bool {
        self.user_input_sent_for_test_matching(Some(executor_id))
    }

    #[cfg(any(test, debug_assertions))]
    fn user_input_sent_for_test_matching(&mut self, expected_executor_id: Option<&str>) -> bool {
        let Some(rx) = self.user_input_rx_for_test.as_mut() else {
            return false;
        };

        let mut found = false;
        while let Ok(input) = rx.try_recv() {
            if let UserInput::SubmitReview { executor_id, .. } = input {
                let matches_expected = match expected_executor_id {
                    Some(expected) => executor_id == expected,
                    None => true,
                };
                found |= matches_expected;
            }
        }
        found
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_tracked_issues_for_test(&mut self, issues: Vec<spur_pm::IssueSummary>) {
        if self.issue_browser.is_none() {
            self.issue_browser = Some(IssueBrowserView::new());
        }
        if let Some(browser) = self.issue_browser.as_mut() {
            browser.set_issues_for_test(issues);
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_edit_mode_for_test(&mut self, mode: EditMode) {
        self.edit_mode = mode;
        self.dashboard.set_edit_mode(mode);
        if let Some(detail) = self.session_detail.as_mut() {
            detail.set_edit_mode(mode);
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn esc_chain_len_for_test(&self) -> usize {
        self.esc_chain.len()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn session_picker_for_test(&self) -> Option<&SessionPickerView> {
        self.session_picker.as_ref()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_session_picker_current_session_has_draft_for_test(
        &mut self,
        session_id: Option<String>,
    ) {
        if let Some(picker) = self.session_picker.as_mut() {
            picker.set_current_session_has_draft(session_id);
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_metadata_store_for_test(&mut self, store: SessionMetadataStore) {
        self.metadata_store = store;
    }

    #[cfg(any(test, debug_assertions))]
    pub fn metadata_store_for_test(&self) -> &SessionMetadataStore {
        &self.metadata_store
    }

    #[cfg(any(test, debug_assertions))]
    pub fn persist_metadata_for_test(&mut self, context: &'static str) -> bool {
        self.persist_metadata(context)
    }

    #[cfg(any(test, debug_assertions))]
    pub fn handle_crossterm_event_for_test(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::Event;
        self.handle_crossterm_event(Event::Key(key));
    }

    #[cfg(any(test, debug_assertions))]
    pub fn dashboard_is_configured(&self) -> bool {
        self.dashboard.agents_configured()
    }

    /// Test-only accessor for the Dashboard view.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn dashboard_for_test(&self) -> &crate::views::dashboard::DashboardView {
        &self.dashboard
    }

    /// Test-only mutable accessor for the Dashboard view.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn dashboard_mut_for_test(&mut self) -> &mut crate::views::dashboard::DashboardView {
        &mut self.dashboard
    }

    #[cfg(any(test, debug_assertions))]
    pub fn open_dashboard_slash_picker_for_test(&mut self) {
        self.current_view = ViewId::Dashboard;
        self.dashboard.open_slash_picker_for_test();
    }

    pub fn current_view_for_test(&self) -> &ViewId {
        &self.current_view
    }

    pub fn transient_hint_for_test(&self) -> Option<&TransientHint> {
        self.transient_hint.as_ref()
    }

    pub fn flash_hint_short_for_test(&mut self, msg: &str) {
        self.flash_hint_short(msg);
    }

    pub fn flash_hint_for_test(&mut self, msg: &str, duration: Duration) {
        self.flash_hint(msg, duration);
    }

    pub fn tick_transient_hint_for_test(&mut self, now: Instant) {
        self.tick_transient_hint(now);
    }

    pub fn transient_hint_text(&self) -> Option<&str> {
        self.transient_hint.as_ref().map(|hint| hint.text.as_str())
    }

    pub fn is_help_visible_for_test(&self) -> bool {
        self.help_visible
    }

    pub fn is_quit_confirm_visible_for_test(&self) -> bool {
        self.quit_confirm_visible
    }

    pub fn is_collision_modal_visible_for_test(&self) -> bool {
        self.collision_modal.is_some()
    }

    pub fn set_collision_modal_for_test(
        &mut self,
        acp_id: impl Into<String>,
        holder: spur_acp::session_lock::HolderInfo,
    ) {
        self.collision_modal = Some(CollisionModalState {
            acp_id: acp_id.into(),
            holder,
        });
    }

    pub fn is_upgrade_modal_visible_for_test(&self) -> bool {
        self.upgrade_modal.is_some()
    }

    pub fn set_upgrade_modal_for_test(
        &mut self,
        err: spur_license::FeatureGateError,
        required_tier: Option<spur_license::Plan>,
    ) {
        self.upgrade_modal = Some(UpgradeModalState { err, required_tier });
    }

    pub fn is_palette_visible(&self) -> bool {
        self.palette_visible
    }

    pub fn try_open_palette_for_test(&mut self) {
        self.open_palette();
    }

    pub fn seed_palette_with_session_for_test(&mut self, session_id: &str, label: &str) {
        use crate::components::palette::{PaletteKind, PalettePayload, PaletteResult};

        self.palette_state.reset();
        self.palette_state.push_raw(vec![PaletteResult {
            kind: PaletteKind::Session,
            label: label.to_string(),
            subtitle: format!("session · {}", session_id),
            payload: PalettePayload::Session {
                session_id: session_id.to_string(),
            },
        }]);
    }

    #[cfg(any(test, debug_assertions))]
    pub fn last_action_for_test(&self) -> Option<crate::action::Action> {
        self.last_action.clone()
    }

    pub fn palette_state_for_test(&self) -> &crate::components::palette::PaletteState {
        &self.palette_state
    }

    pub fn palette_state_for_test_mut(&mut self) -> &mut crate::components::palette::PaletteState {
        &mut self.palette_state
    }

    pub fn user_warning_for_test(&self) -> Option<&str> {
        self.user_warning.as_deref()
    }

    pub fn new_for_palette_test() -> Self {
        Self::new(None, false)
    }

    #[cfg(any(test, debug_assertions))]
    pub fn seed_session_detail_with_dynamic_command_for_test(
        &mut self,
        handle: &str,
        name: &str,
        description: &str,
    ) {
        use crate::commands::registry::CommandRegistry;
        use spur_acp::{AvailableCommand, CommandsConfig};

        let cfg = CommandsConfig::default();
        let entry =
            crate::agents::build_entry(handle, &cfg, &AvailableCommand::new(name, description));
        let mut registry = CommandRegistry::new();
        registry.set_agent_commands(handle, vec![entry]);
        self.session_detail =
            Some(crate::views::session_detail::SessionDetailView::new_for_palette_test(registry));
    }

    #[cfg(any(test, debug_assertions))]
    pub fn age_issue_browser_prefetch_for_test(&mut self, age: Duration) {
        if let Some(view) = self.issue_browser.as_mut() {
            view.age_pending_prefetch_for_test(age);
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn age_esc_chain_for_test(&mut self, duration: Duration) {
        for instant in &mut self.esc_chain {
            if let Some(aged) = instant.checked_sub(duration) {
                *instant = aged;
            }
        }
    }

    #[cfg(any(test, debug_assertions))]
    /// Minimal `App` for unit tests. Avoids disk I/O from
    /// `SessionMetadataStore::load`.
    pub fn new_for_tests() -> Self {
        App::new(None, false)
    }
}
