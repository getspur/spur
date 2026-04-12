use std::collections::HashMap;
use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::broadcast;

use spur_acp::{DelegationStatus, SessionEvent, SpurEvent};

use crate::events::handle_terminal_events;
use crate::ui;

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

/// Tracked state for an active session.
#[allow(dead_code)]
pub struct SessionState {
    pub id: String,
    pub agent: String,
    pub role: String,
    pub status: String,
    pub duration: String,
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
    /// Recent events for the session log.
    pub event_log: Vec<LogEntry>,
    /// Active sessions.
    pub sessions: Vec<SessionState>,
    /// Cumulative cost in USD.
    pub total_cost: f64,
    /// Cost broken down by agent name.
    pub cost_by_agent: HashMap<String, f64>,
    /// Which panel has focus.
    pub selected_panel: Panel,
    /// Whether the user has requested to quit.
    pub should_quit: bool,
    /// Scroll offset for the log panel.
    pub log_scroll: u16,
    /// Map from session-id to agent name (for log prefixes).
    session_agent: HashMap<String, (String, String)>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            agents: Vec::new(),
            event_log: Vec::new(),
            sessions: Vec::new(),
            total_cost: 0.0,
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
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let total_secs = now.as_secs();
        let hours = (total_secs / 3600) % 24;
        let minutes = (total_secs / 60) % 60;
        let seconds = total_secs % 60;
        format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
    }

    fn push_log(&mut self, prefix: impl Into<String>, message: impl Into<String>) {
        self.event_log.push(LogEntry {
            timestamp: Self::now_stamp(),
            prefix: prefix.into(),
            message: message.into(),
        });
        // Auto-scroll to bottom
        let len = self.event_log.len() as u16;
        self.log_scroll = len.saturating_sub(1);
    }

    /// Lookup or build a prefix like "[brain:kiro]" from a session id.
    fn prefix_for_session(&self, session_id: &str) -> String {
        if let Some((role, agent)) = self.session_agent.get(session_id) {
            format!("[{}:{}]", role, agent)
        } else {
            format!("[session:{}]", &session_id[..8.min(session_id.len())])
        }
    }

    fn find_agent_mut(&mut self, name: &str) -> Option<&mut AgentState> {
        self.agents.iter_mut().find(|a| a.name == name)
    }

    /// Process a single SpurEvent, updating app state accordingly.
    pub fn process_event(&mut self, event: SpurEvent) {
        match event {
            SpurEvent::BrainSpawned { agent, session } => {
                let sid = session.0.clone();
                self.session_agent
                    .insert(sid.clone(), ("brain".into(), agent.clone()));
                if self.find_agent_mut(&agent).is_none() {
                    self.agents.push(AgentState {
                        name: agent.clone(),
                        role: "brain".into(),
                        status: "spawned".into(),
                    });
                } else if let Some(a) = self.find_agent_mut(&agent) {
                    a.status = "spawned".into();
                }
                self.sessions.push(SessionState {
                    id: sid,
                    agent: agent.clone(),
                    role: "brain".into(),
                    status: "spawned".into(),
                    duration: String::new(),
                });
                self.push_log(
                    format!("[brain:{}]", agent),
                    "Brain agent spawned".to_string(),
                );
            }

            SpurEvent::WorkerSpawned {
                agent,
                session,
                worktree: _,
            } => {
                let sid = session.0.clone();
                self.session_agent
                    .insert(sid.clone(), ("worker".into(), agent.clone()));
                if self.find_agent_mut(&agent).is_none() {
                    self.agents.push(AgentState {
                        name: agent.clone(),
                        role: "worker".into(),
                        status: "spawned".into(),
                    });
                } else if let Some(a) = self.find_agent_mut(&agent) {
                    a.status = "spawned".into();
                }
                self.sessions.push(SessionState {
                    id: sid,
                    agent: agent.clone(),
                    role: "worker".into(),
                    status: "spawned".into(),
                    duration: String::new(),
                });
                self.push_log(
                    format!("[worker:{}]", agent),
                    "Worker agent spawned".to_string(),
                );
            }

            SpurEvent::AgentOutput { session, event: se } => {
                let prefix = self.prefix_for_session(&session.0);
                match se {
                    SessionEvent::TextDelta(text) => {
                        // Trim for display; skip empty deltas
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            self.push_log(&prefix, trimmed.to_string());
                        }
                    }
                    SessionEvent::StatusUpdate(status) => {
                        let status_str = format!("{:?}", status).to_lowercase();
                        // Update agent status
                        if let Some((_role, agent_name)) =
                            self.session_agent.get(&session.0).cloned()
                        {
                            if let Some(a) = self.find_agent_mut(&agent_name) {
                                a.status = status_str.clone();
                            }
                        }
                        self.push_log(&prefix, format!("Status: {}", status_str));
                    }
                    SessionEvent::ToolCallStart { name, .. } => {
                        self.push_log(&prefix, format!("Tool call: {}", name));
                    }
                    SessionEvent::ToolCallResult { .. } => {
                        // tool results are usually verbose; skip
                    }
                    SessionEvent::Error { message, .. } => {
                        if let Some((_role, agent_name)) =
                            self.session_agent.get(&session.0).cloned()
                        {
                            if let Some(a) = self.find_agent_mut(&agent_name) {
                                a.status = "error".into();
                            }
                        }
                        self.push_log(&prefix, format!("Error: {}", message));
                    }
                    SessionEvent::RateLimitHit { retry_after } => {
                        if let Some((_role, agent_name)) =
                            self.session_agent.get(&session.0).cloned()
                        {
                            if let Some(a) = self.find_agent_mut(&agent_name) {
                                a.status = "rate-limited".into();
                            }
                        }
                        let msg = match retry_after {
                            Some(d) => format!("Rate limited (retry after {}s)", d.as_secs()),
                            None => "Rate limited".to_string(),
                        };
                        self.push_log(&prefix, msg);
                    }
                    SessionEvent::Complete { .. } => {
                        if let Some((_role, agent_name)) =
                            self.session_agent.get(&session.0).cloned()
                        {
                            if let Some(a) = self.find_agent_mut(&agent_name) {
                                a.status = "done".into();
                            }
                        }
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
                if let Some((_role, agent_name)) =
                    self.session_agent.get(&worker_session.0).cloned()
                {
                    if let Some(a) = self.find_agent_mut(&agent_name) {
                        a.status = match &status {
                            DelegationStatus::Success => "done",
                            DelegationStatus::Failed { .. } => "error",
                            DelegationStatus::Conflict { .. } => "error",
                            DelegationStatus::Timeout => "error",
                        }
                        .into();
                    }
                }
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
                // Update session status
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == session.0) {
                    s.status = if success {
                        "done".into()
                    } else {
                        "failed".into()
                    };
                }
                if let Some((_role, agent_name)) = self.session_agent.get(&session.0).cloned() {
                    if let Some(a) = self.find_agent_mut(&agent_name) {
                        a.status = if success { "done" } else { "error" }.into();
                    }
                }
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
                if let Some(a) = self.find_agent_mut(&agent) {
                    a.status = "rate-limited".into();
                }
                let msg = match retry_after {
                    Some(d) => format!("Rate limited (retry after {}s)", d.as_secs()),
                    None => "Rate limited".to_string(),
                };
                self.push_log(format!("[{}]", agent), msg);
            }

            SpurEvent::BrainFailover { from, to } => {
                if let Some(a) = self.find_agent_mut(&from) {
                    a.status = "error".into();
                }
                self.push_log("[spur]", format!("Brain failover: {} -> {}", from, to));
            }

            SpurEvent::CostUpdate {
                session: _,
                agent,
                estimated_cost_usd,
            } => {
                let entry = self.cost_by_agent.entry(agent).or_insert(0.0);
                *entry += estimated_cost_usd;
                self.total_cost += estimated_cost_usd;
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
                    // continue draining
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    // Channel closed — will exit after final render
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
