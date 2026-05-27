use std::time::Instant;

use spur_acp::SessionId;

use crate::action::Action;
use crate::components::input_bar::{ActivityKind, EditMode, InputBar};
use crate::components::react_trace::{ReactTrace, TraceEntry, TraceKind};
use crate::input_history::InputHistoryEntry;

use super::{
    brain_chat_trace, FocusedSessionPanel, LoadState, SessionDetailView, READY_BANNER_TEXT,
};

impl SessionDetailView {
    pub fn new(
        session_id: SessionId,
        agent_name: String,
        role: String,
        cwd: std::path::PathBuf,
        agent_cfg: std::sync::Arc<spur_acp::AgentConfig>,
        worker_snapshot: Vec<crate::mentions::WorkerMentionDescriptor>,
    ) -> Self {
        Self::new_with_issue_snapshot(
            session_id,
            agent_name,
            role,
            cwd,
            agent_cfg,
            worker_snapshot,
            Vec::new(),
        )
    }

    pub fn new_with_issue_snapshot(
        session_id: SessionId,
        agent_name: String,
        role: String,
        cwd: std::path::PathBuf,
        agent_cfg: std::sync::Arc<spur_acp::AgentConfig>,
        worker_snapshot: Vec<crate::mentions::WorkerMentionDescriptor>,
        issue_snapshot: Vec<spur_pm::IssueSummary>,
    ) -> Self {
        let command_registry =
            crate::commands::CommandRegistry::from_configs(std::slice::from_ref(&*agent_cfg));
        let agent_kind = agent_cfg.kind;
        let known_worker_names: std::collections::HashSet<String> =
            worker_snapshot.iter().map(|d| d.name.clone()).collect();
        let mut mention_registry = if role == "brain" {
            crate::mentions::MentionRegistry::for_brain_session(worker_snapshot)
        } else {
            crate::mentions::MentionRegistry::for_direct_session()
        }
        .with_code_graph_from_env();
        mention_registry.set_issue_snapshot(
            issue_snapshot
                .iter()
                .map(crate::mentions::IssueMentionDescriptor::from)
                .collect(),
        );
        Self {
            session_id,
            agent_name,
            role,
            agent_cfg,
            react_trace: brain_chat_trace(agent_kind),
            input_bar: InputBar::new(),
            cost: 0.0,
            started_at: Instant::now(),
            current_mode: None,
            command_registry,
            context_used: None,
            context_size: None,
            auth_error: None,
            completion: crate::components::input_completion::InputCompletionPort::new(),
            mention_registry: std::rc::Rc::new(std::cell::RefCell::new(mention_registry)),
            cwd: spur_graph::resolve_worktree_root_from(cwd),
            #[cfg(feature = "markdown")]
            mermaid_registry: std::collections::HashMap::new(),
            #[cfg(feature = "markdown")]
            mermaid_registry_version: 0,
            #[cfg(feature = "markdown")]
            image_cache: crate::components::image_cache::ImageCache::new(),
            #[cfg(feature = "markdown")]
            in_flight_renders: std::collections::HashSet::new(),
            #[cfg(feature = "markdown")]
            next_image_generation: 0,
            #[cfg(feature = "markdown")]
            pending_fence_actions: std::collections::VecDeque::new(),
            #[cfg(feature = "markdown")]
            render_picker: None,
            last_draft_change_at: None,
            last_persisted_draft: String::new(),
            resume_banner: None,
            stream_in_flight: false,
            cancelling_in_flight: false,
            cancel_confirm_open: false,
            cancel_hint_until: None,
            cancel_mode: None,
            fs_unsafe: false,
            workers_panel_collapsed: false,
            focused_panel: FocusedSessionPanel::ReactTrace,
            tool_depth: std::collections::HashMap::new(),
            known_worker_names,
            cleared: false,
            ready_banner: None,
            load_state: LoadState::Ready,
            session_config_options: Vec::new(),
            pending_model_override: None,
            spur_agent_caps: None,
        }
    }

    /// Test-only convenience constructor — wraps `new()` with sensible
    /// defaults so unit tests don't have to repeat the full argument list.
    #[cfg(any(test, debug_assertions))]
    pub fn new_for_tests() -> Self {
        Self::new(
            spur_acp::SessionId("test".to_string()),
            "claude".to_string(),
            "brain".to_string(),
            std::path::PathBuf::from("/tmp"),
            std::sync::Arc::new(spur_acp::AgentConfig::with_defaults("claude")),
            Vec::new(),
        )
    }

    /// Minimal `SessionDetailView` for palette-integration tests — only
    /// `command_registry` is meaningfully populated. Not suitable for tests
    /// that exercise render, input bar, or trace paths.
    ///
    /// MAINTENANCE: This is a manual struct literal because `SessionDetailView`
    /// holds types without a meaningful `Default` (`Instant::now()`,
    /// `Rc<RefCell<MentionRegistry>>`, `Arc<AgentConfig>`, `PathBuf`).
    /// **Every new field added to `SessionDetailView` must also be added
    /// here, otherwise palette tests will fail to compile.** Keep the field
    /// initializers in the same order as the struct definition for easy
    /// audit.
    #[cfg(any(test, debug_assertions))]
    pub fn new_for_palette_test(
        command_registry: crate::commands::registry::CommandRegistry,
    ) -> Self {
        let mention_registry = crate::mentions::MentionRegistry::for_direct_session();
        Self {
            session_id: spur_acp::SessionId("palette-test".into()),
            agent_name: "palette-test-agent".to_string(),
            role: "brain".to_string(),
            agent_cfg: std::sync::Arc::new(spur_acp::AgentConfig::with_defaults(
                "palette-test-agent",
            )),
            react_trace: brain_chat_trace(spur_acp::AgentKind::Generic),
            input_bar: crate::components::input_bar::InputBar::new(),
            cost: 0.0,
            started_at: std::time::Instant::now(),
            current_mode: None,
            command_registry,
            context_used: None,
            context_size: None,
            auth_error: None,
            completion: crate::components::input_completion::InputCompletionPort::new(),
            mention_registry: std::rc::Rc::new(std::cell::RefCell::new(mention_registry)),
            cwd: spur_graph::resolve_worktree_root(),
            #[cfg(feature = "markdown")]
            mermaid_registry: std::collections::HashMap::new(),
            #[cfg(feature = "markdown")]
            mermaid_registry_version: 0,
            #[cfg(feature = "markdown")]
            image_cache: crate::components::image_cache::ImageCache::new(),
            #[cfg(feature = "markdown")]
            in_flight_renders: std::collections::HashSet::new(),
            #[cfg(feature = "markdown")]
            next_image_generation: 0,
            #[cfg(feature = "markdown")]
            pending_fence_actions: std::collections::VecDeque::new(),
            #[cfg(feature = "markdown")]
            render_picker: None,
            last_draft_change_at: None,
            last_persisted_draft: String::new(),
            resume_banner: None,
            stream_in_flight: false,
            cancelling_in_flight: false,
            cancel_confirm_open: false,
            cancel_hint_until: None,
            cancel_mode: None,
            fs_unsafe: false,
            workers_panel_collapsed: false,
            focused_panel: FocusedSessionPanel::ReactTrace,
            tool_depth: std::collections::HashMap::new(),
            known_worker_names: std::collections::HashSet::new(),
            cleared: false,
            ready_banner: None,
            load_state: LoadState::Ready,
            session_config_options: Vec::new(),
            pending_model_override: None,
            spur_agent_caps: None,
        }
    }

    pub fn set_issue_snapshot(&mut self, issues: Vec<spur_pm::IssueSummary>) {
        let descriptors = issues
            .iter()
            .map(crate::mentions::IssueMentionDescriptor::from)
            .collect();
        self.mention_registry
            .borrow_mut()
            .set_issue_snapshot(descriptors);
    }

    #[cfg(feature = "markdown")]
    fn bump_mermaid_registry_version(&mut self) {
        self.mermaid_registry_version = self.mermaid_registry_version.wrapping_add(1);
    }

    #[cfg(feature = "markdown")]
    pub(crate) fn mermaid_registry_insert(
        &mut self,
        id: crate::components::mermaid::MermaidId,
        state: crate::components::mermaid::MermaidState,
    ) -> Option<crate::components::mermaid::MermaidState> {
        self.bump_mermaid_registry_version();
        self.mermaid_registry.insert(id, state)
    }

    #[cfg(feature = "markdown")]
    pub(crate) fn mermaid_registry_clear(&mut self) {
        self.bump_mermaid_registry_version();
        self.mermaid_registry.clear();
    }

    /// Construct a pre-ready `SessionDetailView` for a session that has been
    /// navigated to optimistically (before the resume pipeline completes).
    /// Starts in `LoadState::Retiring`. Transitions via `handle_spur_event`
    /// as milestone events arrive (Tranche 2 Task 5).
    pub fn for_session(session_id: SessionId) -> Self {
        let mention_registry =
            crate::mentions::MentionRegistry::for_direct_session().with_code_graph_from_env();
        let agent_cfg = std::sync::Arc::new(spur_acp::AgentConfig::with_defaults(""));
        let command_registry =
            crate::commands::CommandRegistry::from_configs(std::slice::from_ref(&*agent_cfg));
        Self {
            session_id,
            agent_name: String::new(),
            role: String::new(),
            agent_cfg,
            react_trace: brain_chat_trace(spur_acp::AgentKind::Generic),
            input_bar: InputBar::new(),
            cost: 0.0,
            started_at: std::time::Instant::now(),
            current_mode: None,
            command_registry,
            context_used: None,
            context_size: None,
            auth_error: None,
            completion: crate::components::input_completion::InputCompletionPort::new(),
            mention_registry: std::rc::Rc::new(std::cell::RefCell::new(mention_registry)),
            cwd: spur_graph::resolve_worktree_root(),
            #[cfg(feature = "markdown")]
            mermaid_registry: std::collections::HashMap::new(),
            #[cfg(feature = "markdown")]
            mermaid_registry_version: 0,
            #[cfg(feature = "markdown")]
            image_cache: crate::components::image_cache::ImageCache::new(),
            #[cfg(feature = "markdown")]
            in_flight_renders: std::collections::HashSet::new(),
            #[cfg(feature = "markdown")]
            next_image_generation: 0,
            #[cfg(feature = "markdown")]
            pending_fence_actions: std::collections::VecDeque::new(),
            #[cfg(feature = "markdown")]
            render_picker: None,
            last_draft_change_at: None,
            last_persisted_draft: String::new(),
            resume_banner: None,
            stream_in_flight: false,
            cancelling_in_flight: false,
            cancel_confirm_open: false,
            cancel_hint_until: None,
            cancel_mode: None,
            fs_unsafe: false,
            workers_panel_collapsed: false,
            focused_panel: FocusedSessionPanel::ReactTrace,
            tool_depth: std::collections::HashMap::new(),
            known_worker_names: std::collections::HashSet::new(),
            cleared: false,
            ready_banner: None,
            load_state: LoadState::Retiring,
            session_config_options: Vec::new(),
            pending_model_override: None,
            spur_agent_caps: None,
        }
    }

    /// Return the current `LoadState` for this view.
    pub fn load_state(&self) -> &LoadState {
        &self.load_state
    }

    /// Show the resume banner for an auto-resumed session. Called by App on
    /// startup after reading session metadata.
    pub fn show_resume_banner(&mut self, title: String, quit_ago: String) {
        self.resume_banner = Some(crate::components::resume_banner::ResumeBanner::new(
            title, quit_ago,
        ));
    }

    /// Wipe conversation-scoped state in place so the same view can host
    /// the next prompt without reconstruction.
    ///
    /// Called by `Action::ClearSession` (eager, gated on a successful
    /// `tx.try_send`) and by the `BrainRetired{UserClear}` event arm
    /// (defensive, idempotent).
    ///
    /// # Classification policy
    ///
    /// Every field on `SessionDetailView` MUST have a deliberate
    /// classification here. When adding a new field, update this method
    /// AND update `SessionDetailView::new` / `new_for_palette_test` per
    /// the existing maintenance rule (see doc comment at line ~174).
    ///
    /// **Cleared** (reset to the empty/default value for that field):
    /// - Conversation: `react_trace` (content wiped; keeps its `AgentKind`
    ///   and `mermaid_enabled` config), `tool_depth`, `mermaid_registry`,
    ///   `pending_fence_actions`.
    /// - Header/status (Task 3): `cost`, `started_at`, `current_mode`
    ///   (plus `react_trace.set_mode(None)` — see §3.3 of spec), `context_used`,
    ///   `context_size`, `auth_error`.
    /// - Stream flags (Task 3): `stream_in_flight`, `cancelling_in_flight`.
    /// - UI transient: `resume_banner`, `completion`, `cancel_hint_until`.
    /// - Draft debounce locals (Task 4): `last_persisted_draft`,
    ///   `last_draft_change_at`.
    /// - Marks set: `cleared = true`, `ready_banner = Some(...)`.
    ///
    /// **Preserved** (the view survives, only its conversation is wiped):
    /// - `session_id`, `agent_name`, `role`, `agent_cfg`, `cwd`,
    ///   `command_registry`, `mention_registry`, `input_bar`,
    ///   `cancel_mode`, `workers_panel_collapsed`, `known_worker_names`,
    ///   `focused_panel`, `render_picker` (mermaid picker).
    pub fn reset_for_clear(&mut self) {
        tracing::debug!(
            session = %self.session_id.0,
            "SessionDetailView::reset_for_clear"
        );
        // Conversation / caches.
        self.react_trace.clear();
        self.tool_depth.clear();
        #[cfg(feature = "markdown")]
        {
            self.invalidate_inline_protocols();
            self.mermaid_registry_clear();
            self.pending_fence_actions.clear();
        }
        self.completion.reset();
        self.resume_banner = None;

        // Header / status.
        self.cost = 0.0;
        self.started_at = std::time::Instant::now();
        self.current_mode = None;
        self.react_trace.set_mode(None); // mirror for pane-title badge (set_current_mode pattern)
        self.context_used = None;
        self.context_size = None;
        self.auth_error = None;
        // Stream flags.
        self.stream_in_flight = false;
        self.cancelling_in_flight = false;
        self.cancel_confirm_open = false;
        self.cancel_hint_until = None;

        // Draft debounce locals (spec §3.5). Gate is ALSO at the source in
        // force_save_draft/draft_save_action — this local wipe is
        // belt-and-suspenders for the debounce's own state machine.
        self.last_persisted_draft.clear();
        self.last_draft_change_at = None;

        // Marks.
        self.cleared = true;
        self.ready_banner = Some(READY_BANNER_TEXT.to_string());
    }

    /// Return SessionDetail to its root transient UI state.
    pub fn reset_to_root(&mut self) {
        self.completion.reset();
        self.resume_banner = None;
        self.focused_panel = FocusedSessionPanel::ReactTrace;
        self.cancel_hint_until = None;
    }

    pub fn focused_panel(&self) -> FocusedSessionPanel {
        self.focused_panel
    }

    /// Whether the resume banner is currently visible (not dismissed and
    /// within its 3s auto-fade window).
    pub fn banner_is_visible(&self) -> bool {
        self.resume_banner
            .as_ref()
            .map(|b| b.should_render())
            .unwrap_or(false)
            || self.ready_banner.is_some()
    }

    /// Test-only: current banner state, if a resume banner is present.
    #[cfg(any(test, debug_assertions))]
    pub fn banner_state(&self) -> Option<crate::components::resume_banner::BannerState> {
        self.resume_banner.as_ref().map(|b| b.state())
    }

    /// Test/debug helper. Overrides the internal debounce clock so tests can
    /// simulate an elapsed debounce window without sleeping.
    pub fn test_set_last_draft_change(&mut self, at: std::time::Instant) {
        self.last_draft_change_at = Some(at);
    }

    /// If the InputBar text has changed and >= 500ms have elapsed since the
    /// last change, returns a `SaveDraft` action and arms the debounce for
    /// the next edit. Otherwise returns `None`. Called from `App`'s tick loop.
    pub fn draft_save_action(&mut self) -> Option<Action> {
        if self.cleared {
            self.last_draft_change_at = None;
            return None;
        }
        let at = self.last_draft_change_at?;
        if at.elapsed() < std::time::Duration::from_millis(500) {
            return None;
        }
        let current = self.input_bar.text().to_string();
        if current == self.last_persisted_draft {
            self.last_draft_change_at = None;
            return None;
        }
        self.last_persisted_draft = current.clone();
        self.last_draft_change_at = None;
        Some(Action::SaveDraft {
            session_id: self.session_id.0.clone(),
            draft: current,
        })
    }

    /// Synchronously flush the current InputBar text to a `SaveDraft` action
    /// regardless of whether the 500ms debounce has elapsed. Returns `None` if
    /// the draft is unchanged since the last persist (no-op). Callers must
    /// apply the returned action **same-tick** at user-intent boundaries
    /// (opening the picker, quit-confirm proceed, brain respawn with a
    /// different session id) so metadata reflects the latest on-screen text
    /// before any code reads it (e.g., the confirm-switch banner decision).
    pub fn force_save_draft(&mut self) -> Option<Action> {
        if self.cleared {
            // A cleared view's session_id is opaque; any SaveDraft keyed
            // on it would corrupt the retired session's metadata.
            // Carry-over into the next view happens in the App-side
            // replacement path via `restore_draft`. See spec §3.5.
            self.last_draft_change_at = None;
            return None;
        }
        let current = self.input_bar.text().to_string();
        if current == self.last_persisted_draft {
            self.last_draft_change_at = None;
            return None;
        }
        self.last_persisted_draft = current.clone();
        self.last_draft_change_at = None;
        Some(Action::SaveDraft {
            session_id: self.session_id.0.clone(),
            draft: current,
        })
    }

    /// Pre-fill the InputBar with a previously-saved draft. Empty string = no-op.
    /// Also marks the draft as persisted so the next debounce tick doesn't
    /// re-save the same text.
    pub fn restore_draft(&mut self, draft: &str) {
        if draft.is_empty() {
            return;
        }
        self.input_bar.set_text(draft.to_string(), draft.len());
        self.dispatch_intent(crate::components::completion_trigger::IntentEvent::SetText);
        self.last_persisted_draft = draft.to_string();
    }

    pub fn prefill_input(&mut self, text: String) {
        let cursor = text.len();
        self.completion.reset();
        self.input_bar.set_text(text, cursor);
        self.last_draft_change_at = Some(std::time::Instant::now());
    }

    /// Seed the InputBar with global input history (loaded from metadata).
    pub fn seed_input_history(&mut self, entries: Vec<InputHistoryEntry>) {
        self.input_bar.seed_history(entries);
    }

    /// Current text content of the InputBar (read-only accessor for tests).
    pub fn input_bar_text(&self) -> String {
        self.input_bar.text()
    }

    /// True once this view has been reset for `/clear` and is awaiting replacement.
    pub fn is_cleared(&self) -> bool {
        self.cleared
    }

    /// The ready-banner text for this view, if any.
    #[cfg(test)]
    pub fn ready_banner_text(&self) -> Option<&str> {
        self.ready_banner.as_deref()
    }

    /// True if this view currently carries a resume banner (not fade-state-aware).
    pub fn has_resume_banner(&self) -> bool {
        self.resume_banner.is_some()
    }

    /// Install the graphics `Picker` used to build inline mermaid protocols.
    /// Called by `App` once after view construction. Cheap clone of a small value.
    /// Also forwards the capability bit into `ReactTrace` so new per-entry
    /// streams render ```mermaid fences as plain code in text mode.
    #[cfg(feature = "markdown")]
    pub fn set_render_picker(&mut self, picker: Option<ratatui_image::picker::Picker>) {
        let enabled = picker.is_some();
        self.render_picker = picker;
        self.react_trace.set_mermaid_enabled(enabled);
    }

    /// Whether a graphics picker is installed (and therefore Alt+V should
    /// open the mermaid overlay instead of falling through to the composer).
    #[cfg(feature = "markdown")]
    pub(super) fn has_render_picker(&self) -> bool {
        self.render_picker.is_some()
    }

    /// No-op fallback when markdown feature is disabled.
    #[cfg(not(feature = "markdown"))]
    pub(super) fn has_render_picker(&self) -> bool {
        false
    }

    /// Drop every cached protocol so they are rebuilt at the new Rect size
    /// on the next render. Called on terminal resize (app.rs:876) and on
    /// session reset.
    #[cfg(feature = "markdown")]
    pub fn invalidate_inline_protocols(&mut self) {
        self.image_cache.invalidate_all();
    }

    /// The session ID this view tracks.
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Number of entries in the trace (for debug logging).
    pub fn trace_entry_count(&self) -> usize {
        self.react_trace.entry_count()
    }

    /// Expose the react trace for inspection (used in tests).
    pub fn react_trace(&self) -> &ReactTrace {
        &self.react_trace
    }

    /// Merged slash-command registry (spur-local + agent-advertised).
    pub fn command_registry(&self) -> &crate::commands::CommandRegistry {
        &self.command_registry
    }

    /// Lowercase agent identifier used for namespacing commands in the
    /// registry (e.g. `"claude"`, `"kiro"`).
    pub(crate) fn agent_handle_for_commands(&self) -> String {
        self.agent_cfg.effective_handle()
    }

    /// Handle an ACP `SessionUpdate::AvailableCommandsUpdate`. Builds
    /// `CommandEntry` values using the session's `agent_cfg.commands` and
    /// stores them in the registry.
    pub fn apply_available_commands(&mut self, commands: &[spur_acp::AvailableCommand]) {
        let handle = self.agent_handle_for_commands();
        let entries: Vec<_> = commands
            .iter()
            .map(|c| crate::agents::build_entry(&handle, &self.agent_cfg.commands, c))
            .collect();
        self.command_registry.set_agent_commands(&handle, entries);
    }

    /// Handle a `SpurEventBody::CommandRegistryDirty` payload. Synthesizes
    /// advertised slash entries from the cached `config_options` and stores
    /// the snapshot so the popup's slash-arg picker can fetch live choices
    /// via `CompletionEnv.session_config_options`.
    pub fn apply_advertised_commands(
        &mut self,
        caps: Option<&spur_acp::SpurAgentCaps>,
        options: &[spur_acp::SessionConfigOption],
    ) {
        let handle = self.agent_handle_for_commands();
        let entries = match caps {
            Some(caps) => {
                crate::commands::advertised::AdvertisedSource::entries_from_caps(&handle, caps)
            }
            None => crate::commands::advertised::AdvertisedSource::entries(&handle, options),
        };
        // TODO(adapter-models-picker): synthesized /model entries from models-only caps do not
        // yet provide picker candidates from `SessionModelState.available_models`.
        self.command_registry
            .set_advertised_commands(&handle, entries);
        self.session_config_options = options.to_vec();
        if options.iter().any(|option| option.id.0.as_ref() == "model") {
            self.pending_model_override = None;
        }
    }

    pub(super) fn resolved_model_label(&self) -> Option<String> {
        let caps = self.spur_agent_caps.as_deref();
        spur_acp::SpurAgentCaps::model_label_from_config_options(&self.session_config_options)
            .map(str::to_owned)
            .or_else(|| self.pending_model_override.clone())
            .or_else(|| caps.and_then(spur_acp::SpurAgentCaps::current_model_label))
    }

    /// Cache the agent capabilities for this session. Captured by the
    /// orchestrator after `session/new` and plumbed through the resume
    /// pipeline; populated `Some(_)` for fresh sessions, left `None` for
    /// sessions resumed before M9 wires `LoadSessionResponse` into
    /// `SpurAgentCaps`. `None` is treated as permissive on read paths.
    pub fn set_spur_agent_caps(&mut self, caps: Option<std::sync::Arc<spur_acp::SpurAgentCaps>>) {
        self.spur_agent_caps = caps;
    }

    pub(crate) fn spur_agent_caps_cloned(&self) -> Option<std::sync::Arc<spur_acp::SpurAgentCaps>> {
        self.spur_agent_caps.clone()
    }

    /// Slash-command popup view: the merged registry filtered by the
    /// cached `SpurAgentCaps`. When caps are absent (resumed sessions
    /// pre-M9), the unfiltered list is returned so pickers stay visible.
    pub fn available_slash_commands(&self) -> Vec<crate::commands::CommandEntry> {
        self.command_registry
            .available_commands_for_session(self.spur_agent_caps.as_deref())
    }

    /// Test-only accessor for the cached snapshot of advertised session
    /// config options.
    #[cfg(any(test, debug_assertions))]
    pub fn session_config_options_for_test(&self) -> &[spur_acp::SessionConfigOption] {
        &self.session_config_options
    }

    /// Test-only accessor: flattened trace text for each entry,
    /// oldest→newest. Used by integration tests in
    /// `tests/session_update_handling.rs`.
    #[doc(hidden)]
    pub fn trace_snapshot_for_test(&self) -> Vec<String> {
        self.react_trace
            .entries_for_test()
            .iter()
            .map(|e| e.text.clone())
            .collect()
    }

    /// Current local time formatted as HH:MM:SS.
    pub(super) fn now_stamp() -> String {
        crate::components::now_stamp()
    }

    /// Agent kind for this session, used by the adapter layer.
    pub(super) fn agent_kind(&self) -> spur_acp::AgentKind {
        self.agent_cfg.kind
    }

    /// Set the current session mode and propagate it into the trace pane so
    /// the pane-title badge renders the new mode.
    pub(crate) fn set_current_mode(&mut self, mode: Option<String>) {
        self.current_mode = mode.clone();
        self.react_trace.set_mode(mode);
    }

    pub fn set_edit_mode(&mut self, mode: EditMode) {
        self.input_bar.set_mode(mode);
    }

    pub fn set_disable_paste_burst(&mut self, disabled: bool) {
        self.input_bar.set_disable_paste_burst(disabled);
    }

    pub(crate) fn input_bar_active_non_empty(&self) -> bool {
        !self.input_bar.is_empty()
    }

    pub(crate) fn completion_active(&self) -> bool {
        self.completion.is_active()
    }

    pub(crate) fn open_theme_picker(&mut self, active_theme_name: &str) {
        self.completion.open_theme_picker(active_theme_name);
    }

    pub fn handle_paste(&mut self, text: &str) {
        self.input_bar.insert_paste(text);
        self.dispatch_intent(crate::components::completion_trigger::IntentEvent::Pasted);
    }

    /// Format elapsed time since view was opened.
    pub(super) fn elapsed(&self) -> String {
        crate::components::format_elapsed(self.started_at)
    }

    /// Update the brain status label shown in the InputBar.
    pub fn set_brain_status(&mut self, status: &str) {
        if self.cancelling_in_flight {
            self.input_bar.set_status(
                Some(format!("[{}: cancelling{{spinner}}]", self.agent_name)),
                ActivityKind::Cancelling,
            );
            return;
        }
        let mention_count = self.input_bar.protected_ranges().len();
        let mention_suffix = if mention_count > 0 {
            format!(
                " \u{00b7} {} mention{}",
                mention_count,
                if mention_count > 1 { "s" } else { "" }
            )
        } else {
            String::new()
        };

        let (label, activity) = match status {
            "idle" => {
                if mention_count > 0 {
                    (
                        Some(format!(
                            "[{} mention{}]",
                            mention_count,
                            if mention_count > 1 { "s" } else { "" }
                        )),
                        ActivityKind::Idle,
                    )
                } else {
                    (None, ActivityKind::Idle)
                }
            }
            "thinking" => (
                Some(format!(
                    "[{} {{spinner}}{}]",
                    self.agent_name, mention_suffix
                )),
                ActivityKind::Thinking,
            ),
            "connecting" => (
                Some(format!(
                    "[{}: connecting {{spinner}}{}]",
                    self.agent_name, mention_suffix
                )),
                ActivityKind::Connecting,
            ),
            "connected" => (
                Some(format!(
                    "[{}: connected{}]",
                    self.agent_name, mention_suffix
                )),
                ActivityKind::Idle,
            ),
            "streaming" => (
                Some(format!(
                    "[{} {{spinner}}{}]",
                    self.agent_name, mention_suffix
                )),
                ActivityKind::Streaming,
            ),
            "ready" => (
                Some(format!("[{}: ready{}]", self.agent_name, mention_suffix)),
                ActivityKind::Idle,
            ),
            "error" => (
                Some(format!("[{}: error{}]", self.agent_name, mention_suffix)),
                ActivityKind::Idle,
            ),
            other if other.starts_with("delegat") => (
                Some(format!(
                    "[{}: {}{}]",
                    self.agent_name, other, mention_suffix
                )),
                ActivityKind::Thinking,
            ),
            other => (
                Some(format!(
                    "[{}: {}{}]",
                    self.agent_name, other, mention_suffix
                )),
                ActivityKind::Idle,
            ),
        };
        self.input_bar.set_status(label, activity);
    }

    pub fn scroll_up(&mut self) {
        self.react_trace.scroll_up();
    }

    pub fn scroll_down(&mut self) {
        self.react_trace.scroll_down();
    }

    pub fn scroll_up_by(&mut self, lines: usize) {
        self.react_trace.scroll_up_by(lines);
    }

    pub fn scroll_down_by(&mut self, lines: usize) {
        self.react_trace.scroll_down_by(lines);
    }

    /// Add a user message to the ReAct trace for instant feedback.
    pub fn push_user_message(&mut self, text: &str) {
        self.react_trace.push(TraceEntry {
            kind: TraceKind::UserMessage,
            text: text.to_string(),
            timestamp: Self::now_stamp(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    pub fn append_user_message(&mut self, text: &str) {
        self.react_trace
            .append_user_message(text, Self::now_stamp());
    }

    /// Push a system-note trace entry (informational message from the TUI
    /// itself, e.g. stubbed kiro execution).
    pub fn push_system_note(&mut self, msg: impl Into<String>) {
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Observe { payload: None },
            text: msg.into(),
            timestamp: Self::now_stamp(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    /// Push a trace entry showing the current session cost.
    pub fn push_cost_note(&mut self) {
        let msg = format!("Session cost: ${:.2}", self.cost);
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Think,
            text: msg,
            timestamp: Self::now_stamp(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    /// Push a trace entry telling the user how to persist their current
    /// edit-mode choice. Called whenever the runtime mode diverges from the
    /// configured mode after `Alt-i` or `/vim` — fires per divergent toggle,
    /// not once per session, so repeated cycles will repeat the hint.
    pub fn push_persist_hint(&mut self, mode_label: &str) {
        let msg = format!(
            "{mode_label} mode (session). Persist: spur config set tui.edit_mode {}",
            mode_label.to_ascii_lowercase()
        );
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Think,
            text: msg,
            timestamp: Self::now_stamp(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    /// Push a system note reflecting the active `cancel_mode`. Called when
    /// the user presses `Esc` to cancel an in-flight stream.
    pub(super) fn push_cancel_note(&mut self) {
        let text = match self.cancel_mode {
            Some(spur_acp::CancelMode::AcpSoft) => {
                "\u{23f9} Cancellation requested \u{2014} waiting for agent\u{2026}"
            }
            Some(spur_acp::CancelMode::ProcessKill) => {
                "\u{23f9} Stopping agent (process will restart on next message)"
            }
            None => "\u{23f9} Cancellation requested",
        };
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Think,
            text: text.to_string(),
            timestamp: Self::now_stamp(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    /// Replay conversation history from disk into the trace.
    pub fn replay_history(&mut self, entries: &[spur_acp::HistoryEntry]) {
        // Header to distinguish replayed history from live conversation.
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Think,
            text: "--- Session history (replayed from disk) ---".to_string(),
            timestamp: String::new(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });

        for entry in entries {
            match entry.role.as_str() {
                "user" => {
                    self.react_trace.push(TraceEntry {
                        kind: TraceKind::UserMessage,
                        text: entry.text.clone(),
                        timestamp: String::new(),
                        #[cfg(feature = "markdown")]
                        markdown: None,
                    });
                }
                "assistant" => {
                    self.react_trace.push(TraceEntry {
                        kind: TraceKind::AgentMessage {
                            agent: self.agent_name.clone(),
                        },
                        text: entry.text.clone(),
                        timestamp: String::new(),
                        #[cfg(feature = "markdown")]
                        markdown: None,
                    });
                }
                _ => {}
            }
        }

        // Footer separator before live session.
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Think,
            text: "--- End of history. New messages below ---".to_string(),
            timestamp: String::new(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    pub fn resolve_pending_permissions(&mut self) {
        self.react_trace.resolve_pending_permissions();
    }

    #[cfg(feature = "markdown")]
    pub fn handle_mermaid_completed(
        &mut self,
        ref_id: crate::components::mermaid::MermaidId,
        target_width: u32,
        result: Result<crate::components::mermaid::MermaidRenderOutput, String>,
    ) {
        use crate::components::mermaid::{MermaidRenderOutput, MermaidState};

        // Always release the in-flight slot, success or failure.
        self.in_flight_renders.remove(&ref_id);

        // Retain the source code from the previous state so a future
        // re-raster on bucket-up can re-dispatch without reaching back
        // into MarkdownStream.
        let code = match self.mermaid_registry.get(&ref_id) {
            Some(MermaidState::Pending { code }) => code.clone(),
            Some(MermaidState::Ready { code, .. }) => code.clone(),
            Some(MermaidState::ReadyText { code, .. }) => code.clone(),
            _ => String::new(),
        };

        let state = match result {
            Ok(MermaidRenderOutput::Image(image)) => {
                self.next_image_generation = self.next_image_generation.saturating_add(1);
                MermaidState::Ready {
                    image,
                    code,
                    rastered_at_bucket: target_width,
                    image_generation: self.next_image_generation,
                }
            }
            Ok(MermaidRenderOutput::Text(text)) => MermaidState::ReadyText {
                text,
                code,
                rendered_at_width: target_width,
            },
            Err(message) => MermaidState::Error { message },
        };
        self.mermaid_registry_insert(ref_id, state);

        // Mark every markdown stream dirty so the next tick's maybe_flush
        // rebuilds placeholders — transitions Pending→Ready (📊) or →Error (⚠).
        self.react_trace.mark_all_streams_dirty();
    }

    /// Inspect Ready diagrams; emit re-raster requests for any whose
    /// `rastered_at_bucket` is below the current pane's bucket. Coalesced
    /// via `in_flight_renders` so only one request per id can be live.
    /// Two-phase (collect → mutate) for borrow-checker robustness.
    #[cfg(feature = "markdown")]
    pub fn maybe_request_rerasters(&mut self, pane_cols: u16, cell_w_px: u16) {
        use crate::components::mermaid::{raster_width_for_pane, MermaidState};
        let pane_w_px = (pane_cols as u32).saturating_mul(cell_w_px as u32);
        let new_bucket = raster_width_for_pane(pane_w_px);

        let candidates: Vec<(crate::components::mermaid::MermaidId, String)> = self
            .mermaid_registry
            .iter()
            .filter_map(|(id, state)| match state {
                MermaidState::Ready {
                    rastered_at_bucket,
                    code,
                    ..
                } if *rastered_at_bucket < new_bucket && !self.in_flight_renders.contains(id) => {
                    Some((*id, code.clone()))
                }
                MermaidState::ReadyText { .. } => None,
                _ => None,
            })
            .collect();

        for (id, code) in candidates {
            self.in_flight_renders.insert(id);
            self.pending_fence_actions
                .push_back(crate::action::Action::MermaidRenderRequest {
                    session: self.session_id.clone(),
                    ref_id: id,
                    code,
                    target_width: new_bucket,
                });
        }
    }

    #[cfg(feature = "markdown")]
    pub fn take_pending_actions(&mut self) -> Vec<crate::action::Action> {
        self.pending_fence_actions.drain(..).collect()
    }

    pub fn push_permission(&mut self, description: &str, countdown: u8) {
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Permission {
                description: description.to_string(),
                pending: true,
                countdown,
            },
            text: String::new(),
            timestamp: Self::now_stamp(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    // ── Completion popup wiring ─────────────────────────────────────────

    pub(super) fn dispatch_intent(
        &mut self,
        event: crate::components::completion_trigger::IntentEvent,
    ) {
        use crate::components::input_completion::CompletionEnv;
        let env = CompletionEnv {
            command_registry: &self.command_registry,
            mention_registry: &self.mention_registry,
            cwd: &self.cwd,
            scope: crate::mentions::CompletionScope::Session(&self.session_id),
            session_config_options: &self.session_config_options,
        };
        self.completion.dispatch(event, &mut self.input_bar, &env);
    }

    /// Build (error_ids, pending_ids) sets from the mermaid registry for use
    /// in constructing a `StateLookup`.
    #[cfg(feature = "markdown")]
    pub(super) fn build_state_lookup_sets(
        &self,
    ) -> (
        std::collections::HashSet<crate::components::mermaid::MermaidId>,
        std::collections::HashSet<crate::components::mermaid::MermaidId>,
    ) {
        use crate::components::mermaid::MermaidState;
        let mut errors = std::collections::HashSet::new();
        let mut pending = std::collections::HashSet::new();
        for (id, state) in &self.mermaid_registry {
            match state {
                MermaidState::Error { .. } => {
                    errors.insert(*id);
                }
                MermaidState::Pending { .. } | MermaidState::Rendering => {
                    pending.insert(*id);
                }
                MermaidState::Ready { .. } | MermaidState::ReadyText { .. } => {}
            }
        }
        (errors, pending)
    }
}
