use std::time::Duration;

use crossterm::event::{Event, KeyCode};
use futures::StreamExt;
use ratatui::Frame;
use tokio::sync::{broadcast, mpsc};

use spur_acp::{SessionId, SpurEvent};

use crate::action::{Action, ViewId};
use crate::components::help_overlay::HelpOverlay;
use crate::tui;
use crate::views::dashboard::DashboardView;
use crate::views::session_detail::SessionDetailView;
use crate::views::View;

// ─── Supporting types ──────────────────────────────────────────────────

/// A user input message destined for a specific agent session.
pub struct UserInput {
    pub session: SessionId,
    pub text: String,
    pub interrupt: bool,
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
    help_visible: bool,
    should_quit: bool,
    dirty: bool,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
    brain_status: BrainStatus,
    brain_name: Option<String>,
}

impl App {
    pub fn new(user_input_tx: Option<mpsc::Sender<UserInput>>) -> Self {
        Self {
            current_view: ViewId::Dashboard,
            dashboard: DashboardView::new(),
            session_detail: None,
            help_visible: false,
            should_quit: false,
            dirty: true, // initial render
            user_input_tx,
            brain_status: BrainStatus::Idle,
            brain_name: None,
        }
    }

    /// Dispatch a crossterm event (keyboard, resize, etc.) to the active view.
    pub fn handle_crossterm_event(&mut self, event: Event) {
        if let Event::Key(key) = event {
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
            };

            if let Some(action) = action {
                self.process_action(action);
            }
            self.dirty = true;
        }
        if let Event::Resize(_, _) = event {
            self.dirty = true;
        }
    }

    /// Forward a SpurEvent to all views that need it.
    pub fn handle_spur_event(&mut self, event: SpurEvent) {
        self.dirty = true;

        // Track brain status transitions
        match &event {
            SpurEvent::BrainSpawned { agent, session } => {
                self.brain_status = BrainStatus::Thinking;
                self.brain_name = Some(agent.clone());

                // Always replace SessionDetailView on BrainSpawned
                self.session_detail = Some(SessionDetailView::new(
                    session.clone(),
                    agent.clone(),
                    "brain".to_string(),
                ));

                // Auto-navigate from Dashboard
                if matches!(self.current_view, ViewId::Dashboard) {
                    self.current_view = ViewId::SessionDetail(session.clone());
                }
            }
            SpurEvent::AgentOutput { session: _, .. } => {
                // Transition Thinking → Streaming on first output
                if self.brain_status == BrainStatus::Thinking {
                    self.brain_status = BrainStatus::Streaming;
                }
            }
            SpurEvent::TurnComplete { .. } => {
                self.brain_status = BrainStatus::Ready;
            }
            SpurEvent::BrainError { message, .. } => {
                self.brain_status = BrainStatus::Error(message.clone());
            }
            _ => {}
        }

        // Forward to views
        self.dashboard.handle_spur_event(&event);
        if let Some(ref mut detail) = self.session_detail {
            detail.handle_spur_event(&event);
        }
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

            Action::NavigateBack => {
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

                if let Some(ref tx) = self.user_input_tx {
                    let input = UserInput {
                        session,
                        text,
                        interrupt,
                    };
                    let _ = tx.try_send(input);
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

            // Scroll actions are already handled inside the views' handle_key methods.
            Action::ScrollUp
            | Action::ScrollDown
            | Action::ScrollToTop
            | Action::ScrollToBottom
            | Action::CycleFocus
            | Action::Tick => {}
        }
    }

    /// Tick the active view (for animations, batched text flush, etc.).
    pub fn tick(&mut self) {
        // Only mark dirty for ticks when there are active agents (spinners animating)
        // or text batches to flush.
        match self.current_view {
            ViewId::Dashboard => {
                self.dashboard.tick();
                if self.dashboard.has_active_agents() {
                    self.dirty = true;
                }
            }
            ViewId::SessionDetail(_) => {
                if let Some(ref mut detail) = self.session_detail {
                    detail.tick();
                    self.dirty = true; // session detail always has activity
                }
            }
        }
    }

    /// Render the active view, then overlay help if visible.
    pub fn render(&self, frame: &mut Frame) {
        let area = frame.area();

        match self.current_view {
            ViewId::Dashboard => self.dashboard.render(frame, area),
            ViewId::SessionDetail(_) => {
                if let Some(ref detail) = self.session_detail {
                    detail.render(frame, area);
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
) -> anyhow::Result<()> {
    let mut terminal = tui::setup()?;
    let mut app = App::new(user_input_tx);
    let mut tick_interval = tokio::time::interval(Duration::from_millis(33));
    let mut event_stream = crossterm::event::EventStream::new();
    let mut event_rx = event_rx;

    loop {
        tokio::select! {
            Some(Ok(crossterm_event)) = event_stream.next() => {
                app.handle_crossterm_event(crossterm_event);
            }
            result = event_rx.recv() => {
                match result {
                    Ok(spur_event) => {
                        app.handle_spur_event(spur_event);
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Lost some events due to slow consumption; continue.
                        let _ = n;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        app.should_quit = true;
                    }
                }
            }
            _ = tick_interval.tick() => {
                app.tick();
            }
        }

        if app.dirty {
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
