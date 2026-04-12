use std::collections::HashMap;
use std::io;
use std::time::Duration;

use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::broadcast;

use spur_acp::{DelegationStatus, SessionEvent, SessionId, SpurEvent};

use crate::events::handle_terminal_events;
use crate::ui;

// ─── Constants ───────────────────────────────────────────────────────

const MAX_LOG_ENTRIES: usize = 5_000;

// ─── Supporting types ──────────────────────────────────────────────────

/// Which panel currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Agents,
    Log,
}

/// Tracked state for a single agent.
pub struct AgentState {
    pub name: String,
    pub role: String,
    pub status: String,
}

/// A single entry in the event log.
pub struct LogEntry {
    pub timestamp: String,
    pub prefix: String,
    pub message: String,
}

// ─── App state ─────────────────────────────────────────────────────────

pub struct App {
    /// Agents and their current status.
    pub agents: Vec<AgentState>,
    /// Recent events for the session log (capped at MAX_LOG_ENTRIES).
    pub event_log: Vec<LogEntry>,
    /// Cost broken down by agent name.
    pub cost_by_agent: HashMap<String, f64>,
    /// Which panel has focus.
    pub selected_panel: Panel,
    /// Whether the user has requested to quit.
    pub should_quit: bool,
    /// Scroll offset for the log panel.
    pub log_scroll: usize,
    /// Map from session-id to (role, agent_name) for log prefixes.
    session_agent: HashMap<String, (String, String)>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            agents: Vec::new(),
            event_log: Vec::new(),
            cost_by_agent: HashMap::new(),
            selected_panel: Panel::Log,
            should_quit: false,
            log_scroll: 0,
            session_agent: HashMap::new(),
        }
    }
}

impl App {
    fn now_stamp() -> String {
        chrono::Local::now().format("%H:%M:%S").to_string()
    }

    fn push_log(&mut self, prefix: impl Into<String>, message: impl Into<String>) {
        self.event_log.push(LogEntry {
            timestamp: Self::now_stamp(),
            prefix: prefix.into(),
            message: message.into(),
        });
        // Evict oldest entries when over the cap.
        if self.event_log.len() > MAX_LOG_ENTRIES {
            let drain = self.event_log.len() - MAX_LOG_ENTRIES;
            self.event_log.drain(..drain);
        }
        // Auto-scroll to bottom
        self.log_scroll = self.event_log.len().saturating_sub(1);
    }

    /// Lookup or build a prefix like "[brain:kiro]" from a session id.
    fn prefix_for_session(&self, session_id: &str) -> String {
        if let Some((role, agent)) = self.session_agent.get(session_id) {
            format!("[{}:{}]", role, agent)
        } else {
            format!("[session:{}]", &session_id[..8.min(session_id.len())])
        }
    }

    /// Update the agent status for the agent associated with the given session.
    fn set_agent_status_for_session(&mut self, session_id: &str, status: &str) {
        if let Some((_role, agent_name)) = self.session_agent.get(session_id).cloned() {
            if let Some(a) = self.agents.iter_mut().find(|a| a.name == agent_name) {
                a.status = status.into();
            }
        }
    }

    /// Common handler for BrainSpawned / WorkerSpawned events.
    fn handle_agent_spawned(&mut self, agent: String, session: SessionId, role: &str) {
        let sid = session.0.clone();
        self.session_agent
            .insert(sid, (role.into(), agent.clone()));
        match self.agents.iter_mut().find(|a| a.name == agent) {
            Some(a) => a.status = "spawned".into(),
            None => self.agents.push(AgentState {
                name: agent.clone(),
                role: role.into(),
                status: "spawned".into(),
            }),
        }
        self.push_log(
            format!("[{}:{}]", role, agent),
            format!("{} agent spawned", role.chars().next().unwrap_or('?').to_uppercase().collect::<String>() + &role[1..]),
        );
    }

    /// Compute total cost from per-agent costs.
    pub fn total_cost(&self) -> f64 {
        self.cost_by_agent.values().sum()
    }

    /// Process a single SpurEvent, updating app state accordingly.
    pub fn process_event(&mut self, event: SpurEvent) {
        match event {
            SpurEvent::BrainSpawned { agent, session } => {
                self.handle_agent_spawned(agent, session, "brain");
            }

            SpurEvent::WorkerSpawned {
                agent,
                session,
                worktree: _,
            } => {
                self.handle_agent_spawned(agent, session, "worker");
            }

            SpurEvent::AgentOutput { session, event: se } => {
                let prefix = self.prefix_for_session(&session.0);
                match se {
                    SessionEvent::TextDelta(text) => {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            self.push_log(&prefix, trimmed.to_string());
                        }
                    }
                    SessionEvent::StatusUpdate(status) => {
                        let status_str = format!("{:?}", status).to_lowercase();
                        self.set_agent_status_for_session(&session.0, &status_str);
                        self.push_log(&prefix, format!("Status: {}", status_str));
                    }
                    SessionEvent::ToolCallStart { name, .. } => {
                        self.push_log(&prefix, format!("Tool call: {}", name));
                    }
                    SessionEvent::ToolCallResult { .. } => {}
                    SessionEvent::Error { message, .. } => {
                        self.set_agent_status_for_session(&session.0, "error");
                        self.push_log(&prefix, format!("Error: {}", message));
                    }
                    SessionEvent::RateLimitHit { retry_after } => {
                        self.set_agent_status_for_session(&session.0, "rate-limited");
                        let msg = match retry_after {
                            Some(d) => format!("Rate limited (retry after {}s)", d.as_secs()),
                            None => "Rate limited".to_string(),
                        };
                        self.push_log(&prefix, msg);
                    }
                    SessionEvent::Complete { .. } => {
                        self.set_agent_status_for_session(&session.0, "done");
                        self.push_log(&prefix, "Session complete");
                    }
                }
            }

            SpurEvent::DelegationRequested {
                from: _,
                to_agent,
                task,
            } => {
                self.push_log("[brain]", format!("Delegating to {}: {}", to_agent, task));
            }

            SpurEvent::DelegationCompleted {
                worker_session,
                status,
            } => {
                let prefix = self.prefix_for_session(&worker_session.0);
                let status_str = match &status {
                    DelegationStatus::Success => "done",
                    _ => "error",
                };
                self.set_agent_status_for_session(&worker_session.0, status_str);
                let msg = match &status {
                    DelegationStatus::Success => "Delegation completed successfully".to_string(),
                    DelegationStatus::Failed { error } => {
                        format!("Delegation failed: {}", error)
                    }
                    DelegationStatus::Conflict { files } => {
                        format!("Delegation conflict in {} files", files.len())
                    }
                    DelegationStatus::Timeout => "Delegation timed out".to_string(),
                };
                self.push_log(&prefix, msg);
            }

            SpurEvent::SessionCompleted { session, success } => {
                let prefix = self.prefix_for_session(&session.0);
                let status = if success { "done" } else { "error" };
                self.set_agent_status_for_session(&session.0, status);
                let msg = if success {
                    "Session completed successfully"
                } else {
                    "Session failed"
                };
                self.push_log(&prefix, msg);
            }

            SpurEvent::RateLimitDetected {
                agent,
                retry_after,
            } => {
                if let Some(a) = self.agents.iter_mut().find(|a| a.name == agent) {
                    a.status = "rate-limited".into();
                }
                let msg = match retry_after {
                    Some(d) => format!("Rate limited (retry after {}s)", d.as_secs()),
                    None => "Rate limited".to_string(),
                };
                self.push_log(format!("[{}]", agent), msg);
            }

            SpurEvent::BrainFailover { from, to } => {
                if let Some(a) = self.agents.iter_mut().find(|a| a.name == from) {
                    a.status = "error".into();
                }
                self.push_log("[spur]", format!("Brain failover: {} -> {}", from, to));
            }

            SpurEvent::CostUpdate {
                session: _,
                agent,
                estimated_cost_usd,
            } => {
                *self.cost_by_agent.entry(agent).or_insert(0.0) += estimated_cost_usd;
            }

            SpurEvent::ConflictDetected { files } => {
                self.push_log(
                    "[spur]",
                    format!(
                        "Conflict detected in {} file(s): {}",
                        files.len(),
                        files
                            .iter()
                            .map(|f| f.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }

            SpurEvent::IssueReceived { source, id } => {
                self.push_log("[pm]", format!("Issue received from {}: {}", source, id));
            }

            SpurEvent::PrCreated { url } => {
                self.push_log("[spur]", format!("PR created: {}", url));
            }

            SpurEvent::IssueUpdated { source, id, status } => {
                self.push_log(
                    "[pm]",
                    format!("Issue {} ({}) updated: {}", id, source, status),
                );
            }
        }
    }
}

// ─── Main TUI entry point ──────────────────────────────────────────────

/// Run the TUI dashboard, consuming events from the broadcast receiver.
pub async fn run_tui(
    mut event_rx: broadcast::Receiver<SpurEvent>,
) -> anyhow::Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut app = App::default();
    let tick_rate = Duration::from_millis(100);

    // Main loop
    loop {
        // 1. Drain all pending SpurEvents (non-blocking)
        loop {
            match event_rx.try_recv() {
                Ok(ev) => app.process_event(ev),
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    app.push_log("[tui]", format!("Skipped {} events (lag)", n));
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    app.should_quit = true;
                    break;
                }
            }
        }

        // 2. Render
        terminal.draw(|f| ui::draw(f, &app))?;

        // 3. Check for quit
        if app.should_quit {
            break;
        }

        // 4. Handle keyboard events (blocking up to tick_rate)
        handle_terminal_events(&mut app, tick_rate)?;

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
