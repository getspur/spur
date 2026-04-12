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

// ─── App state ─────────────────────────────────────────────────────────

pub struct App {
    current_view: ViewId,
    dashboard: DashboardView,
    session_detail: Option<SessionDetailView>,
    help_visible: bool,
    should_quit: bool,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
}

impl App {
    pub fn new(user_input_tx: Option<mpsc::Sender<UserInput>>) -> Self {
        Self {
            current_view: ViewId::Dashboard,
            dashboard: DashboardView::new(),
            session_detail: None,
            help_visible: false,
            should_quit: false,
            user_input_tx,
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
        }
        // Resize events are handled automatically by ratatui on next draw.
    }

    /// Forward a SpurEvent to all views that need it.
    pub fn handle_spur_event(&mut self, event: SpurEvent) {
        // Dashboard always receives events (it tracks all sessions).
        self.dashboard.handle_spur_event(&event);

        // Session detail receives events too (it filters internally by session).
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
                // Look up agent name and role from dashboard's session_agent map.
                let (agent_name, role) = self
                    .dashboard
                    .agent_info_for_session(&session_id.0)
                    .unwrap_or_else(|| {
                        let short = &session_id.0[..8.min(session_id.0.len())];
                        (short.to_string(), "unknown".to_string())
                    });

                self.session_detail = Some(SessionDetailView::new(
                    session_id.clone(),
                    agent_name,
                    role,
                ));
                self.current_view = ViewId::SessionDetail(session_id.clone());
            }

            Action::NavigateTo(ViewId::Dashboard) => {
                self.current_view = ViewId::Dashboard;
                self.session_detail = None;
            }

            Action::NavigateBack => {
                self.current_view = ViewId::Dashboard;
                self.session_detail = None;
            }

            Action::SendMessage {
                session,
                text,
                interrupt,
            } => {
                if let Some(ref tx) = self.user_input_tx {
                    let input = UserInput {
                        session,
                        text,
                        interrupt,
                    };
                    // Use try_send to avoid blocking the event loop.
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
        match self.current_view {
            ViewId::Dashboard => self.dashboard.tick(),
            ViewId::SessionDetail(_) => {
                if let Some(ref mut detail) = self.session_detail {
                    detail.tick();
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

        terminal.draw(|f| app.render(f))?;

        if app.should_quit {
            break;
        }
    }

    tui::teardown(&mut terminal)?;
    Ok(())
}
