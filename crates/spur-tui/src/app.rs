use std::time::Duration;

use crossterm::event::{Event, KeyCode, MouseEvent, MouseEventKind};
use futures::StreamExt;
use ratatui::Frame;
use tokio::sync::{broadcast, mpsc};
use tokio::time::timeout;

use spur_acp::{SessionId, SpurEvent, SpurEventBody};
use spur_core::ExecutorLineage;

#[cfg(feature = "markdown")]
use ratatui_image::picker::Picker;

use crate::action::{Action, ViewId};
use crate::components::help_overlay::HelpOverlay;
use crate::components::quit_confirm::QuitConfirmDialog;
use crate::session_metadata::SessionMetadataStore;
use crate::tui;
use crate::views::dashboard::DashboardView;
use crate::views::session_detail::SessionDetailView;
use crate::views::session_picker::SessionPickerView;
use crate::views::View;

// ─── Supporting types ──────────────────────────────────────────────────

/// A user input message or control command sent from the TUI to the backend.
pub enum UserInput {
    Message {
        session: SessionId,
        blocks: Vec<spur_acp::ContentBlock>,
        interrupt: bool,
    },
    /// Spawn a new brain session and send these blocks as the first prompt
    /// atomically. Emitted by the TUI when the user types into Dashboard's
    /// InputBar with no brain attached, or from the picker's
    /// NewSessionRequested path.
    NewSessionWithMessage {
        blocks: Vec<spur_acp::ContentBlock>,
        interrupt: bool,
    },
    ListSessions,
    ResumeSession {
        session_id: String,
    },
    /// Request the orchestrator to call `set_session_mode` on the current
    /// brain session with the given mode id (e.g. `"plan"`, `"default"`).
    SetSessionMode {
        mode_id: String,
    },
    SubmitReview {
        executor_id: String,
        /// The attempt_n from the pending review card the user acted on.
        /// The orchestrator's dispatcher uses this as a supersession guard.
        attempt_n: u32,
        decision: spur_core::ReviewDecision,
    },
    /// Invoke an agent vendor-extension RPC on the active brain session.
    VendorExec {
        session: SessionId,
        method: String,
        command: String,
        args: serde_json::Value,
    },
}

/// Tracks the brain agent's current state for status indicators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrainStatus {
    Idle,
    Thinking,
    Streaming,
    Ready,
    Error(String),
}

// ─── App state ─────────────────────────────────────────────────────────

pub struct App {
    current_view: ViewId,
    dashboard: DashboardView,
    session_detail: Option<SessionDetailView>,
    session_picker: Option<SessionPickerView>,
    help_visible: bool,
    /// Shown when the user requests quit while a brain is attached. While
    /// visible, all input is captured by the dialog.
    quit_confirm_visible: bool,
    should_quit: bool,
    dirty: bool,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
    brain_status: BrainStatus,
    brain_name: Option<String>,
    pending_permission: Option<(spur_acp::types::PermissionRequest, std::time::Instant)>,
    /// Event-sourced projection of brain → executor lineage.
    lineage: ExecutorLineage,
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_picker: Option<Picker>,
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_rx: tokio::sync::mpsc::UnboundedReceiver<Action>,
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_tx: tokio::sync::mpsc::UnboundedSender<Action>,
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_viewer: Option<crate::views::mermaid_viewer::MermaidViewerView>,
    metadata_store: SessionMetadataStore,
    /// Loaded Spur configuration. Used to resolve per-agent `AgentConfig`
    /// at session-creation time (see `resolve_agent_config`). Defaults to
    /// `SpurConfig::default()` when no config is supplied.
    config: std::sync::Arc<spur_acp::SpurConfig>,
}

impl App {
    pub fn new(user_input_tx: Option<mpsc::Sender<UserInput>>, start_in_picker: bool) -> Self {
        Self::new_with_config(
            user_input_tx,
            start_in_picker,
            std::sync::Arc::new(spur_acp::SpurConfig::default()),
        )
    }

    pub fn new_with_config(
        user_input_tx: Option<mpsc::Sender<UserInput>>,
        start_in_picker: bool,
        config: std::sync::Arc<spur_acp::SpurConfig>,
    ) -> Self {
        let metadata_path = std::path::PathBuf::from(".spur").join("session_metadata.json");
        let metadata_store = SessionMetadataStore::load(&metadata_path);

        let (current_view, session_picker) = if start_in_picker {
            let mut picker = SessionPickerView::new();
            picker.set_metadata(metadata_store.metadata().clone());
            (ViewId::SessionPicker, Some(picker))
        } else {
            (ViewId::Dashboard, None)
        };

        #[cfg(feature = "markdown")]
        let mermaid_picker = Picker::from_query_stdio().ok();
        #[cfg(feature = "markdown")]
        let (mermaid_tx, mermaid_rx) = tokio::sync::mpsc::unbounded_channel();

        let app = Self {
            current_view,
            dashboard: DashboardView::new(),
            session_detail: None,
            session_picker,
            help_visible: false,
            quit_confirm_visible: false,
            should_quit: false,
            dirty: true, // initial render
            user_input_tx,
            brain_status: BrainStatus::Idle,
            brain_name: None,
            pending_permission: None,
            lineage: ExecutorLineage::new(),
            #[cfg(feature = "markdown")]
            mermaid_picker,
            #[cfg(feature = "markdown")]
            mermaid_rx,
            #[cfg(feature = "markdown")]
            mermaid_tx,
            #[cfg(feature = "markdown")]
            mermaid_viewer: None,
            metadata_store,
            config,
        };

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

        app
    }

    /// Test-only accessor: borrow the current `SessionDetailView`.
    #[doc(hidden)]
    pub fn session_detail_for_test(&self) -> Option<&crate::views::session_detail::SessionDetailView> {
        self.session_detail.as_ref()
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
        spur_acp::AgentConfig {
            name: name.to_string(),
            command: String::new(),
            args: vec![],
            transport: spur_acp::types::TransportKind::Acp,
            role: spur_acp::types::AgentRole::Both,
            capabilities: vec![],
            cost_tier: spur_acp::types::CostTier::Medium,
            rate_limit_window: None,
            review: Default::default(),
            display: Default::default(),
            commands: Default::default(),
            permissions: Default::default(),
            skip_permissions: false,
            skip_permissions_args: vec![],
            skip_permissions_session_mode: None,
        }
    }

    /// Dispatch a crossterm event (keyboard, resize, mouse, etc.) to the active view.
    pub fn handle_crossterm_event(&mut self, event: Event) {
        match event {
            Event::Key(key) => {
                // Quit-confirm dialog takes priority: it captures every key.
                if self.quit_confirm_visible {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                            // Flush any unsent draft to disk before we exit so
                            // the next `spur watch` restores the latest text.
                            self.force_flush_active_draft();
                            self.quit_confirm_visible = false;
                            self.should_quit = true;
                        }
                        _ => {
                            // Anything else (n/N/Esc/q/…) cancels the quit.
                            self.quit_confirm_visible = false;
                        }
                    }
                    self.dirty = true;
                    return;
                }

                // Help overlay intercepts ? (toggle) and Esc (close) before views.
                if self.help_visible {
                    match key.code {
                        KeyCode::Char('?') | KeyCode::Esc => {
                            self.help_visible = false;
                            return;
                        }
                        _ => return, // swallow all keys while help is visible
                    }
                }

                let action = match self.current_view {
                    ViewId::Dashboard => self.dashboard.handle_key_with_lineage(key, &self.lineage),
                    ViewId::SessionDetail(_) => {
                        if let Some(ref mut detail) = self.session_detail {
                            detail.handle_key(key)
                        } else {
                            None
                        }
                    }
                    ViewId::SessionPicker => {
                        self.session_picker.as_mut().and_then(|p| p.handle_key(key))
                    }
                    #[cfg(feature = "markdown")]
                    ViewId::MermaidOverlay(_) => {
                        if let Some(viewer) = self.mermaid_viewer.as_mut() {
                            match key.code {
                                KeyCode::Char('[') | KeyCode::Char(']') => {
                                    if let Some(detail) = self.session_detail.as_ref() {
                                        let entries: Vec<_> = detail
                                            .mermaid_registry
                                            .iter()
                                            .map(|(k, v)| (*k, v))
                                            .collect();
                                        viewer.cycle(&entries, key.code == KeyCode::Char(']'));
                                        self.dirty = true;
                                    }
                                    None
                                }
                                _ => viewer.handle_key(key),
                            }
                        } else {
                            None
                        }
                    }
                };

                if let Some(action) = action {
                    self.process_action(action);
                }
                self.dirty = true;
            }
            Event::Mouse(mouse) => {
                self.handle_mouse_event(mouse);
            }
            Event::Resize(_, _) => {
                #[cfg(feature = "markdown")]
                if let Some(detail) = self.session_detail.as_mut() {
                    detail.invalidate_inline_protocols();
                }
                self.dirty = true;
            }
            _ => {}
        }
    }

    /// Handle mouse scroll events. Only scroll wheel is processed —
    /// clicks and drags are ignored to avoid tmux/terminal conflicts.
    fn handle_mouse_event(&mut self, event: MouseEvent) {
        let lines: usize = match event.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => 3,
            _ => return,
        };
        let is_up = matches!(event.kind, MouseEventKind::ScrollUp);

        match self.current_view {
            ViewId::Dashboard => {
                if is_up {
                    self.dashboard.scroll_activity_up_by(lines);
                } else {
                    self.dashboard.scroll_activity_down_by(lines);
                }
            }
            ViewId::SessionDetail(_) => {
                if let Some(ref mut detail) = self.session_detail {
                    if is_up {
                        detail.scroll_up_by(lines);
                    } else {
                        detail.scroll_down_by(lines);
                    }
                }
            }
            ViewId::SessionPicker => {
                // No mouse scroll in v1 picker.
            }
            #[cfg(feature = "markdown")]
            ViewId::MermaidOverlay(_) => {}
        }
        self.dirty = true;
    }

    /// Forward a SpurEvent to all views that need it.
    pub fn handle_spur_event(&mut self, event: SpurEvent) {
        // Always fold into the lineage projection first. The projection is a
        // pure function of the event stream — view code reads from it later.
        self.lineage.apply(&event);

        self.dirty = true;

        // Handle session list responses before forwarding to views
        match &event.body {
            SpurEventBody::SessionsListed { agent, sessions } => {
                if let Some(ref mut picker) = self.session_picker {
                    picker.set_sessions(agent.clone(), sessions.clone());
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
                    let is_sentinel = session.0.is_empty()
                        || session.0 == "00000000-0000-0000-0000-000000000000";
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
                return;
            }
            _ => {}
        }

        // Track brain status transitions
        match &event.body {
            SpurEventBody::BrainSpawned { agent, session } => {
                self.brain_status = BrainStatus::Thinking;
                self.brain_name = Some(agent.clone());

                // Only create a new SessionDetailView if none exists or the
                // session ID changed. Replacing unconditionally would wipe any
                // user message that was just pushed to the trace.
                let needs_new = match &self.session_detail {
                    Some(detail) => detail.session_id() != session,
                    None => true,
                };
                if needs_new {
                    // Defensive: if we're replacing an existing detail whose
                    // session id differs (agent-side session change / brain
                    // respawn), flush its unsent draft first so it's
                    // recoverable under the old id rather than vanishing with
                    // the old view.
                    self.force_flush_active_draft();
                    let agent_cfg = self.resolve_agent_config(agent);
                    let mut view = SessionDetailView::new(
                        session.clone(),
                        agent.clone(),
                        "brain".to_string(),
                        std::env::current_dir().unwrap_or_default(),
                        agent_cfg,
                    );
                    #[cfg(feature = "markdown")]
                    view.set_render_picker(self.mermaid_picker.clone());
                    // Restore draft from metadata, if any.
                    if let Some(entry) = self.metadata_store.entry(&session.0) {
                        view.restore_draft(&entry.draft);
                    }
                    // Auto-resume banner: if this session matches the
                    // last_active pointer read at startup, show the banner.
                    // Clear the pointer afterward so a second spawn this run
                    // doesn't re-trigger the banner.
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
                            self.metadata_store
                                .metadata()
                                .last_active_at
                                .as_deref(),
                        );
                        view.show_resume_banner(title, quit_ago);
                        self.metadata_store.clear_last_active();
                        if let Err(e) = self.metadata_store.save() {
                            tracing::warn!(error = %e, "failed to persist cleared last_active");
                        }
                    }
                    self.session_detail = Some(view);
                }

                // Auto-navigate from Dashboard or SessionPicker
                if matches!(self.current_view, ViewId::Dashboard | ViewId::SessionPicker) {
                    self.current_view = ViewId::SessionDetail(session.clone());
                }
            }
            SpurEventBody::AgentSessionReady {
                session,
                acp_session_id,
                brain,
                resumed: _,
            } => {
                self.metadata_store
                    .set_acp_mapping(&session.0, acp_session_id, brain);
                if let Err(e) = self.metadata_store.save() {
                    tracing::warn!(
                        error = %e,
                        session = %session.0,
                        "failed to persist AgentSessionReady metadata"
                    );
                }
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
                self.metadata_store
                    .set_last_active(session.0.clone(), now);
                if let Err(e) = self.metadata_store.save() {
                    tracing::warn!(error = %e, "failed to persist last_active on TurnComplete");
                }
            }
            SpurEventBody::BrainError { message, .. } => {
                self.brain_status = BrainStatus::Error(message.clone());
            }
            _ => {}
        }

        // Forward to views
        self.dashboard.handle_spur_event(&event);
        if let Some(ref mut detail) = self.session_detail {
            detail.handle_spur_event(&event);
        }

        // Sync status to InputBars
        self.sync_brain_status();
    }

    /// Process a single Action returned by a view.
    fn process_action(&mut self, action: Action) {
        match action {
            Action::Quit => {
                // If a brain is attached, show the confirmation dialog so
                // the user is aware the agent subprocess will be terminated.
                // Otherwise exit immediately — nothing at risk.
                if self.brain_name.is_some() {
                    self.quit_confirm_visible = true;
                } else {
                    self.should_quit = true;
                }
            }

            Action::NavigateTo(ViewId::SessionDetail(ref session_id)) => {
                if self.session_detail.is_some() {
                    // Just switch view — don't recreate. BrainSpawned is the only creator.
                    self.current_view = ViewId::SessionDetail(session_id.clone());
                }
                // If no session_detail exists (no brain spawned), ignore.
            }

            Action::NavigateTo(ViewId::Dashboard) => {
                self.current_view = ViewId::Dashboard;
                // session_detail kept alive (same as NavigateBack)
            }

            Action::NavigateTo(ViewId::SessionPicker) => {
                self.current_view = ViewId::SessionPicker;
            }

            #[cfg(feature = "markdown")]
            Action::NavigateTo(ViewId::MermaidOverlay(ref session)) => {
                use crate::views::mermaid_viewer::MermaidViewerView;
                self.mermaid_viewer = Some(MermaidViewerView::new(session.clone()));
                self.current_view = ViewId::MermaidOverlay(session.clone());
                self.dirty = true;
            }

            Action::NavigateBack => {
                #[cfg(feature = "markdown")]
                if let ViewId::MermaidOverlay(ref session) = self.current_view {
                    self.current_view = ViewId::SessionDetail(session.clone());
                    self.mermaid_viewer = None;
                    self.dirty = true;
                    return;
                }
                // From Dashboard: if an active session exists, return to it
                // (the natural "back" from the activity log); if not, fall
                // back to Quit — matching the previous `Esc = Quit on an
                // empty Dashboard` behavior. Quit still respects the
                // quit-confirm dialog when a brain is attached.
                if matches!(self.current_view, ViewId::Dashboard) {
                    if let Some(ref detail) = self.session_detail {
                        self.current_view = ViewId::SessionDetail(detail.session_id().clone());
                        self.dirty = true;
                    } else if self.brain_name.is_some() {
                        self.quit_confirm_visible = true;
                    } else {
                        self.should_quit = true;
                    }
                    return;
                }
                // From SessionDetail (or any other view): go to Dashboard.
                self.current_view = ViewId::Dashboard;
                // Note: session_detail is intentionally kept alive so it
                // continues accumulating events while the Dashboard is shown.
            }

            Action::SendMessage {
                mut session,
                blocks,
                interrupt,
            } => {
                // Empty session means "route to the currently active session".
                // Dashboard's InputBar emits this when a brain is attached.
                if session.0.is_empty() {
                    if let Some(ref detail) = self.session_detail {
                        session = detail.session_id().clone();
                    } else {
                        tracing::warn!(
                            "SendMessage with empty session and no active session_detail — \
                             dropping (caller should have used NewSessionWithMessage)"
                        );
                        return;
                    }
                }

                // Transition to Thinking when sending a message
                if matches!(self.brain_status, BrainStatus::Ready | BrainStatus::Idle | BrainStatus::Error(_)) {
                    self.brain_status = BrainStatus::Thinking;
                }

                let preview = crate::commands::submit_router::blocks_preview(&blocks);

                tracing::info!(
                    text_len = preview.len(),
                    block_count = blocks.len(),
                    has_session_detail = self.session_detail.is_some(),
                    view = ?self.current_view,
                    brain_status = ?self.brain_status,
                    "SendMessage: pushing user message to trace"
                );

                // Add user message to Session Detail trace for instant feedback.
                // If session_detail doesn't exist yet, the caller should have
                // used NewSessionWithMessage; the dropped-message warning
                // above covers that path.
                if let Some(ref mut detail) = self.session_detail {
                    detail.push_user_message(&preview);
                    tracing::info!(
                        entries = detail.trace_entry_count(),
                        "SendMessage: pushed to session_detail"
                    );
                } else {
                    tracing::warn!(
                        "SendMessage: session_detail is None — no local echo (orchestrator owns the prompt)"
                    );
                }

                if let Some(ref tx) = self.user_input_tx {
                    let input = UserInput::Message {
                        session,
                        blocks,
                        interrupt,
                    };
                    let _ = tx.try_send(input);
                }

                self.sync_brain_status();
            }

            Action::NewSessionWithMessage { blocks, interrupt } => {
                // Transition to Thinking so the UI reflects work-in-flight
                // immediately; the orchestrator will spawn a brain and send
                // the prompt atomically.
                if matches!(
                    self.brain_status,
                    BrainStatus::Ready | BrainStatus::Idle | BrainStatus::Error(_)
                ) {
                    self.brain_status = BrainStatus::Thinking;
                }

                // Note: we do NOT buffer the preview locally. The
                // orchestrator owns the typed text from here on and will
                // deliver it to the brain atomically when the session spawns.
                // The first-turn user message will appear in the trace via
                // the normal AgentNotification stream once the agent echoes
                // or acts on it. Buffering here caused BUG-1 (cross-session
                // replay into an unrelated session that happens to spawn next).

                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::NewSessionWithMessage {
                        blocks,
                        interrupt,
                    });
                }
                self.sync_brain_status();
                self.dirty = true;
            }

            Action::VendorExec { session, method, command, args } => {
                if let Some(ref mut detail) = self.session_detail {
                    let handle = detail.agent_handle_for_commands();
                    detail.push_system_note(format!(
                        "\u{27e8}{handle}\u{27e9} /{} queued",
                        command
                    ));
                }
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::VendorExec {
                        session,
                        method,
                        command,
                        args,
                    });
                }
            }

            Action::RequestSessions => {
                // Flush any unsent typing in the active SessionDetail into
                // metadata *before* the picker reads metadata to decide the
                // confirm-switch banner. Bypasses the 500ms debounce so text
                // typed within the debounce window is not lost on switch.
                self.force_flush_active_draft();
                // Retain the picker across opens so cursor + filter survive navigation.
                if self.session_picker.is_none() {
                    self.session_picker = Some(SessionPickerView::new());
                }
                self.refresh_picker_metadata();
                self.current_view = ViewId::SessionPicker;
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ListSessions);
                }
                self.dirty = true;
            }

            Action::ResumeSession { session_id } => {
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ResumeSession { session_id });
                }
            }

            Action::ToggleSessionPin { session_id } => {
                let entry = self.metadata_store.entry_mut(&session_id);
                entry.pinned = !entry.pinned;
                if let Err(e) = self.metadata_store.save() {
                    tracing::warn!(error = %e, "failed to persist pin toggle");
                }
                self.refresh_picker_metadata();
                self.dirty = true;
            }

            Action::ToggleSessionArchive { session_id } => {
                let entry = self.metadata_store.entry_mut(&session_id);
                entry.archived = !entry.archived;
                if let Err(e) = self.metadata_store.save() {
                    tracing::warn!(error = %e, "failed to persist archive toggle");
                }
                self.refresh_picker_metadata();
                self.dirty = true;
            }

            Action::ToggleShowArchived => {
                if let Some(ref mut picker) = self.session_picker {
                    picker.toggle_show_archived();
                }
                self.dirty = true;
            }

            Action::RenameSession {
                session_id,
                new_title,
            } => {
                let entry = self.metadata_store.entry_mut(&session_id);
                entry.title_override = if new_title.trim().is_empty() {
                    None
                } else {
                    Some(new_title)
                };
                if let Err(e) = self.metadata_store.save() {
                    tracing::warn!(error = %e, "failed to persist rename");
                }
                self.refresh_picker_metadata();
                self.dirty = true;
            }

            Action::SaveDraft { session_id, draft } => {
                self.apply_save_draft(session_id, draft);
            }

            Action::RefreshSessions => {
                if let Some(tx) = self.user_input_tx.as_ref() {
                    let _ = tx.try_send(crate::UserInput::ListSessions);
                }
                self.dirty = true;
            }

            Action::NewSessionRequested => {
                // Shut down the current brain atomically so picker [+ New session]
                // doesn't leave the old agent subprocess's session running.
                // Orchestrator's NewSessionWithMessage arm with empty blocks is
                // defined as "retire current brain, defer spawn to next Message."
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::NewSessionWithMessage {
                        blocks: vec![],
                        interrupt: false,
                    });
                }
                self.current_view = ViewId::Dashboard;
                self.dirty = true;
            }

            Action::TogglePlanMode => {
                // Cycle between "plan" and "default". If mode is unknown, assume
                // we're in "default" and jump to "plan".
                let current = self
                    .session_detail
                    .as_ref()
                    .and_then(|d| d.current_mode.as_deref());
                let next = match current {
                    Some("plan") => "default",
                    _ => "plan",
                };
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::SetSessionMode {
                        mode_id: next.to_string(),
                    });
                }
                // Optimistic update so the status bar reflects the toggle
                // immediately; orchestrator will emit CurrentModeUpdate to
                // reconcile if the agent rejects the mode id.
                if let Some(ref mut detail) = self.session_detail {
                    detail.current_mode = Some(next.to_string());
                }
            }

            Action::ToggleVerbose => {
                // Verbose mode is tracked by the dashboard view internally.
                // We toggle it via a dedicated method or re-send the key.
                // For now, the dashboard already handles this in handle_key.
            }

            Action::ShowHelp => {
                self.help_visible = true;
            }

            Action::HideHelp => {
                self.help_visible = false;
            }

            Action::ShowSessionCost => {
                if let Some(ref mut detail) = self.session_detail {
                    detail.push_cost_note();
                }
            }

            Action::PermissionGrant(choice) => {
                use crate::action::PermissionChoice;
                if let Some((perm, _)) = self.pending_permission.take() {
                    match choice {
                        PermissionChoice::Allow => {
                            let id = perm.args.options.first()
                                .map(|o| o.option_id.to_string())
                                .unwrap_or_else(|| "allow".to_string());
                            let _ = perm.reply_tx.send(spur_acp::types::PermissionResponse {
                                option_id: id,
                            });
                        }
                        PermissionChoice::AlwaysAllow => {
                            let id = perm.args.options.iter()
                                .find(|o| o.name.to_lowercase().contains("always"))
                                .or(perm.args.options.first())
                                .map(|o| o.option_id.to_string())
                                .unwrap_or_else(|| "allow".to_string());
                            let _ = perm.reply_tx.send(spur_acp::types::PermissionResponse {
                                option_id: id,
                            });
                        }
                        PermissionChoice::Deny => {
                            // Drop reply_tx (signals denial to ACP thread)
                            drop(perm);
                        }
                    }
                }
                self.clear_pending_permission_trace();
            }

            Action::SelectNext => {
                self.dashboard.agents_tree_mut().select_next(&self.lineage);
            }
            Action::SelectPrev => {
                self.dashboard.agents_tree_mut().select_prev(&self.lineage);
            }
            Action::FocusNode => {
                let selected = self.dashboard.agents_tree_mut().selected().cloned();
                if let Some(id) = selected {
                    self.dashboard.set_focused_node(Some(id));
                }
            }
            Action::UnfocusNode => {
                self.dashboard.set_focused_node(None);
            }
            Action::JumpToReview => {
                // Cycle through pending reviews in insertion order. Skip the
                // currently-focused node so repeated `r` presses advance to
                // the next review instead of re-landing on the same one.
                let current = self.dashboard.focused_node().cloned();
                let reviews = self.lineage.pending_reviews();
                let next = reviews
                    .iter()
                    .position(|id| Some(id) == current.as_ref())
                    .and_then(|i| reviews.get(i + 1).cloned())
                    .or_else(|| reviews.into_iter().next());
                if let Some(id) = next {
                    self.dashboard.agents_tree_mut().set_selected(Some(id.clone()));
                    self.dashboard.set_focused_node(Some(id));
                    self.dashboard.detail_pane_mut().current_tab =
                        crate::components::detail_pane::DetailTab::Review;
                }
            }
            Action::ToggleCollapse => {
                let selected = self.dashboard.agents_tree_mut().selected().cloned();
                if let Some(id) = selected {
                    self.dashboard.agents_tree_mut().toggle_collapsed(&id);
                }
            }
            Action::SubmitReview { executor_id, attempt_n, decision } => {
                let has_review = self
                    .lineage
                    .node(&spur_core::ExecutorId(executor_id.clone()))
                    .map(|n| n.pending_review.is_some())
                    .unwrap_or(false);
                if !has_review {
                    tracing::warn!(executor_id = %executor_id, "SubmitReview ignored: no pending review on this node");
                    return;
                }
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::SubmitReview {
                        executor_id: executor_id.clone(),
                        attempt_n,
                        decision: decision.clone(),
                    });
                }
                // Optimistically reflect the resolution locally so the UI
                // updates immediately without waiting for the authoritative
                // event to round-trip.
                self.lineage.apply(&spur_acp::SpurEvent::now(spur_acp::SpurEventBody::ExecutorReviewResolved {
                    id: executor_id,
                    decision: to_wire_decision(&decision),
                }));
            }

            #[cfg(feature = "markdown")]
            Action::MermaidRenderRequest { session, ref_id, code } => {
                let tx = self.mermaid_tx.clone();
                let session_cloned = session.clone();
                tokio::task::spawn_blocking(move || {
                    let result = crate::components::mermaid::render_mermaid(&code)
                        .map(std::sync::Arc::new)
                        .map_err(|e| e.to_string());
                    let _ = tx.send(Action::MermaidRenderCompleted {
                        session: session_cloned,
                        ref_id,
                        result,
                    });
                });
            }
            #[cfg(feature = "markdown")]
            Action::MermaidRenderCompleted { session, ref_id, result } => {
                if let Some(ref mut detail) = self.session_detail {
                    if detail.session_id().0 == session.0 {
                        detail.handle_mermaid_completed(ref_id, result);
                    }
                }
                self.dirty = true;
            }

            // Scroll actions are already handled inside the views' handle_key methods.
            Action::ScrollUp
            | Action::ScrollDown
            | Action::ScrollToTop
            | Action::ScrollToBottom
            | Action::CycleFocus
            | Action::Tick => {}
        }
    }

    fn handle_permission_request(&mut self, request: spur_acp::types::PermissionRequest) {
        // Auto-deny any existing pending permission (drops old reply_tx)
        self.pending_permission.take();

        // Extract description from SDK args
        let description = request.args.tool_call.fields.title
            .clone()
            .unwrap_or_else(|| "Tool call".to_string());

        // Push permission entry to the active session's trace
        if let Some(ref mut detail) = self.session_detail {
            detail.push_permission(&description, 30);
        }

        // Store with deadline
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        self.pending_permission = Some((request, deadline));
        self.dirty = true;
    }

    /// Mark all pending permission trace entries as resolved.
    fn clear_pending_permission_trace(&mut self) {
        if let Some(ref mut detail) = self.session_detail {
            detail.resolve_pending_permissions();
        }
    }

    /// Persist a draft to metadata. Callable both from the `Action::SaveDraft`
    /// handler (debounced tick path) and same-tick from exit-session boundaries
    /// via `force_flush_active_draft`.
    fn apply_save_draft(&mut self, session_id: String, draft: String) {
        let entry = self.metadata_store.entry_mut(&session_id);
        if entry.draft != draft {
            entry.draft = draft;
            if let Err(e) = self.metadata_store.save() {
                tracing::warn!(error = %e, "failed to persist draft");
            }
        }
    }

    /// Synchronously flush the active SessionDetailView's unsent InputBar text
    /// to metadata, bypassing the 500ms debounce. Call at user-intent "exit
    /// session" boundaries (opening the picker, quit-confirm proceed, brain
    /// respawn for a different session id) so metadata reflects the latest
    /// on-screen text before anything reads it. No-op when no detail is active
    /// or the draft is unchanged since the last persist.
    fn force_flush_active_draft(&mut self) {
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
    fn refresh_picker_metadata(&mut self) {
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
    fn sync_brain_status(&mut self) {
        let status_str = match &self.brain_status {
            BrainStatus::Idle => "idle",
            BrainStatus::Thinking => "thinking",
            BrainStatus::Streaming => "streaming",
            BrainStatus::Ready => "ready",
            BrainStatus::Error(_) => "error",
        };

        self.dashboard
            .set_brain_status(self.brain_name.as_deref(), status_str);

        if let Some(ref mut detail) = self.session_detail {
            detail.set_brain_status(status_str);
        }
    }

    /// Tick the active view (for animations, batched text flush, etc.).
    pub fn tick(&mut self) {
        #[cfg(feature = "markdown")]
        {
            while let Ok(action) = self.mermaid_rx.try_recv() {
                self.process_action(action);
            }
        }

        if let Some((_, deadline)) = &self.pending_permission {
            if std::time::Instant::now() >= *deadline {
                self.pending_permission.take(); // drops reply_tx → auto-deny
                self.clear_pending_permission_trace();
                self.dirty = true;
            }
        }

        // Only mark dirty for ticks when there are active agents (spinners animating)
        // or text batches to flush.
        match self.current_view {
            ViewId::Dashboard => {
                let flushed_batch = self.dashboard.tick_and_report_flush();
                // Mark dirty when executors are actively running (spinners animate)
                use spur_core::LifecycleState;
                let has_active = self.lineage.nodes().any(|n| {
                    matches!(
                        n.phase,
                        LifecycleState::Running
                            | LifecycleState::Spawning
                            | LifecycleState::Resuming
                    )
                });
                if has_active || flushed_batch {
                    self.dirty = true;
                }
            }
            ViewId::SessionDetail(_) => {
                if let Some(ref mut detail) = self.session_detail {
                    detail.tick();
                    self.dirty = true; // session detail always has activity
                }
                #[cfg(feature = "markdown")]
                {
                    let pending: Vec<Action> = self
                        .session_detail
                        .as_mut()
                        .map(|d| d.take_pending_actions())
                        .unwrap_or_default();
                    for action in pending {
                        self.process_action(action);
                    }
                }
                // Debounced draft persistence — fires ~500ms after the last
                // InputBar keystroke, then no-ops until the next change.
                let draft_action = self
                    .session_detail
                    .as_mut()
                    .and_then(|d| d.draft_save_action());
                if let Some(action) = draft_action {
                    self.process_action(action);
                }
            }
            ViewId::SessionPicker => {
                self.session_picker.as_mut().map(|p| p.tick());
            }
            #[cfg(feature = "markdown")]
            ViewId::MermaidOverlay(_) => {
                // The underlying session detail continues receiving
                // AgentMessageChunks while the overlay is open. Tick it so
                // debounced flushes and fence dispatches don't stall.
                if let Some(ref mut detail) = self.session_detail {
                    detail.tick();
                    let pending = detail.take_pending_actions();
                    for action in pending {
                        self.process_action(action);
                    }
                    self.dirty = true;
                }
            }
        }
    }

    /// Render the active view, then overlay help if visible.
    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();

        match self.current_view.clone() {
            ViewId::Dashboard => self.dashboard.render_with_lineage(frame, area, &self.lineage),
            ViewId::SessionDetail(_) => {
                if let Some(ref detail) = self.session_detail {
                    detail.render_with_lineage(frame, area, &self.lineage);
                }
            }
            ViewId::SessionPicker => {
                self.session_picker.as_ref().map(|p| p.render(frame, area));
            }
            #[cfg(feature = "markdown")]
            ViewId::MermaidOverlay(ref session) => {
                let session_matches = self
                    .session_detail
                    .as_ref()
                    .map(|d| d.session_id().0 == session.0)
                    .unwrap_or(false);
                if session_matches {
                    // Collect entries while holding an immutable borrow of
                    // session_detail. The borrow ends after this block so we
                    // can then mutably borrow mermaid_viewer.
                    let entries: Vec<(
                        crate::components::mermaid::MermaidId,
                        &crate::components::mermaid::MermaidState,
                    )> = self
                        .session_detail
                        .as_ref()
                        .map(|d| d.mermaid_registry.iter().map(|(k, v)| (*k, v)).collect())
                        .unwrap_or_default();
                    if let Some(viewer) = self.mermaid_viewer.as_mut() {
                        viewer.set_available(&entries, self.mermaid_picker.as_ref());
                        render_mermaid_overlay(frame, area, viewer);
                    }
                }
            }
        }

        if self.help_visible {
            #[cfg(feature = "markdown")]
            let mermaid_enabled = self.mermaid_picker.is_some();
            #[cfg(not(feature = "markdown"))]
            let mermaid_enabled = false;
            HelpOverlay::render(frame, area, mermaid_enabled);
        }

        if self.quit_confirm_visible {
            let brain = self.brain_name.as_deref().unwrap_or("(unknown)");
            QuitConfirmDialog::render(frame, area, brain);
        }
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
    run_tui_with_config(
        event_rx,
        user_input_tx,
        perm_rx,
        start_in_picker,
        std::sync::Arc::new(spur_acp::SpurConfig::default()),
    )
    .await
}

pub async fn run_tui_with_config(
    event_rx: broadcast::Receiver<SpurEvent>,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
    mut perm_rx: Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>>,
    start_in_picker: bool,
    config: std::sync::Arc<spur_acp::SpurConfig>,
) -> anyhow::Result<()> {
    let mut terminal = tui::setup()?;
    let mut app = App::new_with_config(user_input_tx, start_in_picker, config);
    let mut tick_interval = tokio::time::interval(Duration::from_millis(33));
    let mut event_stream = crossterm::event::EventStream::new();
    let mut event_rx = event_rx;

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
        }

        // Phase 2: Drain all remaining crossterm events (non-blocking).
        // This collapses bursts of mouse scroll events into one render pass.
        loop {
            match timeout(Duration::ZERO, event_stream.next()).await {
                Ok(Some(Ok(ev))) => {
                    crossterm_drained += 1;
                    app.handle_crossterm_event(ev);
                }
                _ => break,
            }
        }

        // Phase 3: Drain all remaining spur events (non-blocking).
        loop {
            match event_rx.try_recv() {
                Ok(spur_event) => {
                    spur_drained += 1;
                    app.handle_spur_event(spur_event);
                }
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    tracing::warn!(
                        streaming_probe = true,
                        site = "E_broadcast_lag",
                        lagged_n = n,
                        "TUI broadcast receiver lagged (drain phase) — events dropped"
                    );
                    continue;
                }
                Err(_) => break,
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

    tui::teardown(&mut terminal)?;
    Ok(())
}

// ─── Free helpers ──────────────────────────────────────────────────────

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
            state.current_mode = Some(u.current_mode_id.to_string());
        }
        AvailableCommandsUpdate(u) => {
            state.apply_available_commands(&u.available_commands);
        }
        UsageUpdate(u) => {
            state.context_used = Some(u.used);
            state.context_size = Some(u.size);
        }
        _ => {
            tracing::trace!("apply_session_update: unhandled variant");
        }
    }
}

fn to_wire_decision(d: &spur_core::ReviewDecision) -> spur_acp::ReviewDecision {
    d.clone()
}

#[cfg(feature = "markdown")]
fn render_mermaid_overlay(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    viewer: &mut crate::views::mermaid_viewer::MermaidViewerView,
) {
    use ratatui::{
        layout::{Constraint, Layout},
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::Paragraph,
    };
    use ratatui_image::{Resize, StatefulImage};

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " Mermaid Viewer ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))),
        chunks[0],
    );

    if let Some(protocol) = viewer.protocol_mut() {
        let widget = StatefulImage::default().resize(Resize::Fit(None));
        frame.render_stateful_widget(widget, chunks[1], protocol);
    } else {
        frame.render_widget(
            Paragraph::new(
                "No diagram available yet. Wait for render to complete, or press q/Esc to return.",
            )
            .style(Style::default().fg(Color::DarkGray)),
            chunks[1],
        );
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " [/]: cycle · q/Esc: close ",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[2],
    );
}

/// Human-friendly relative time for the resume banner ("5m ago", "2h ago").
/// Returns "recently" if the input is missing or unparseable.
fn humanize_since(iso: Option<&str>) -> String {
    let Some(iso) = iso else {
        return "recently".into();
    };
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(iso) else {
        return "recently".into();
    };
    let secs = chrono::Utc::now().signed_duration_since(dt).num_seconds();
    if secs < 60 {
        "just now".into()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}
