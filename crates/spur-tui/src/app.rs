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
        text: String,
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
        decision: spur_core::ReviewDecision,
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
    should_quit: bool,
    dirty: bool,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
    brain_status: BrainStatus,
    brain_name: Option<String>,
    /// User messages buffered before session_detail exists.
    pending_user_messages: Vec<String>,
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
}

impl App {
    pub fn new(user_input_tx: Option<mpsc::Sender<UserInput>>, start_in_picker: bool) -> Self {
        let (current_view, session_picker) = if start_in_picker {
            (ViewId::SessionPicker, Some(SessionPickerView::new()))
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
            should_quit: false,
            dirty: true, // initial render
            user_input_tx,
            brain_status: BrainStatus::Idle,
            brain_name: None,
            pending_user_messages: Vec::new(),
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
        };

        if start_in_picker {
            if let Some(ref tx) = app.user_input_tx {
                let _ = tx.try_send(UserInput::ListSessions);
            }
        }

        app
    }

    /// Dispatch a crossterm event (keyboard, resize, mouse, etc.) to the active view.
    pub fn handle_crossterm_event(&mut self, event: Event) {
        match event {
            Event::Key(key) => {
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
                    ViewId::Dashboard => self.dashboard.handle_key(key),
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
                    let mut view = SessionDetailView::new(
                        session.clone(),
                        agent.clone(),
                        "brain".to_string(),
                    );
                    // Replay any user messages that were buffered before the view existed.
                    for msg in self.pending_user_messages.drain(..) {
                        view.push_user_message(&msg);
                    }
                    self.session_detail = Some(view);
                }

                // Auto-navigate from Dashboard or SessionPicker
                if matches!(self.current_view, ViewId::Dashboard | ViewId::SessionPicker) {
                    self.current_view = ViewId::SessionDetail(session.clone());
                }
            }
            SpurEventBody::AgentNotification { session: _, .. } => {
                // Transition Thinking → Streaming on first output
                if self.brain_status == BrainStatus::Thinking {
                    self.brain_status = BrainStatus::Streaming;
                }
            }
            SpurEventBody::TurnComplete { .. } => {
                self.brain_status = BrainStatus::Ready;
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
                self.should_quit = true;
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
                self.current_view = ViewId::Dashboard;
                // Note: session_detail is intentionally kept alive so it
                // continues accumulating events while the Dashboard is shown.
            }

            Action::SendMessage {
                session,
                text,
                interrupt,
            } => {
                // Transition to Thinking when sending a message
                if matches!(self.brain_status, BrainStatus::Ready | BrainStatus::Idle | BrainStatus::Error(_)) {
                    self.brain_status = BrainStatus::Thinking;
                }

                tracing::info!(
                    text_len = text.len(),
                    has_session_detail = self.session_detail.is_some(),
                    view = ?self.current_view,
                    brain_status = ?self.brain_status,
                    "SendMessage: pushing user message to trace"
                );

                // Add user message to Session Detail trace for instant feedback.
                // If session_detail doesn't exist yet (first message before
                // BrainSpawned), buffer it for replay when the view is created.
                if let Some(ref mut detail) = self.session_detail {
                    detail.push_user_message(&text);
                    tracing::info!(
                        entries = detail.trace_entry_count(),
                        "SendMessage: pushed to session_detail"
                    );
                } else {
                    tracing::warn!("SendMessage: session_detail is None, buffering");
                    self.pending_user_messages.push(text.clone());
                }

                if let Some(ref tx) = self.user_input_tx {
                    let input = UserInput::Message {
                        session,
                        text,
                        interrupt,
                    };
                    let _ = tx.try_send(input);
                }

                self.sync_brain_status();
            }

            Action::RequestSessions => {
                self.session_picker = Some(SessionPickerView::new());
                self.current_view = ViewId::SessionPicker;
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ListSessions);
                }
            }

            Action::ResumeSession { session_id } => {
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ResumeSession { session_id });
                }
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
            Action::SubmitReview { executor_id, decision } => {
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
                    detail.render(frame, area);
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
            HelpOverlay::render(frame, area);
        }
    }
}

// ─── Main TUI entry point ──────────────────────────────────────────────

/// Run the TUI dashboard, consuming events from the broadcast receiver.
pub async fn run_tui(
    event_rx: broadcast::Receiver<SpurEvent>,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
    mut perm_rx: Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>>,
    start_in_picker: bool,
) -> anyhow::Result<()> {
    let mut terminal = tui::setup()?;
    let mut app = App::new(user_input_tx, start_in_picker);
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
            state.available_commands = u
                .available_commands
                .iter()
                .map(|c| c.name.clone())
                .collect();
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
