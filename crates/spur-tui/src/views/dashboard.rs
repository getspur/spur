use std::collections::HashMap;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use spur_acp::{DelegationStatus, SessionEvent, SpurEvent};

use crate::action::{Action, ViewId};
use crate::components::activity_log::ActivityLog;
use crate::components::agents_tree::AgentsTree;
use crate::components::status_bar::StatusBar;
use crate::components::{AgentState, LogEntry, LogEntryKind};

use super::View;

/// Which panel currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Agents,
    Log,
}

/// The main dashboard view composing AgentsTree + ActivityLog + StatusBar.
pub struct DashboardView {
    agents_tree: AgentsTree,
    activity_log: ActivityLog,
    agents: Vec<AgentState>,
    cost_by_agent: HashMap<String, f64>,
    session_agent: HashMap<String, (String, String)>,
    focused_panel: Panel,
    verbose: bool,
    text_batch: HashMap<String, (String, Instant)>,
    start_time: Instant,
}

impl DashboardView {
    pub fn new() -> Self {
        let mut activity_log = ActivityLog::new("Activity");
        activity_log.set_focused(true);

        Self {
            agents_tree: AgentsTree::new(),
            activity_log,
            agents: Vec::new(),
            cost_by_agent: HashMap::new(),
            session_agent: HashMap::new(),
            focused_panel: Panel::Log,
            verbose: false,
            text_batch: HashMap::new(),
            start_time: Instant::now(),
        }
    }

    /// Current local time formatted as HH:MM:SS.
    fn now_stamp() -> String {
        chrono::Local::now().format("%H:%M:%S").to_string()
    }

    /// Build a prefix like "[brain:kiro]" from a session id.
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
    fn handle_agent_spawned(&mut self, agent: String, session_id: String, role: &str) {
        self.session_agent
            .insert(session_id, (role.into(), agent.clone()));
        match self.agents.iter_mut().find(|a| a.name == agent) {
            Some(a) => {
                a.status = "spawned".into();
                a.started_at = Some(Instant::now());
            }
            None => self.agents.push(AgentState {
                name: agent.clone(),
                role: role.into(),
                status: "spawned".into(),
                parent: if role == "worker" {
                    // Find a brain agent to parent under
                    self.agents
                        .iter()
                        .find(|a| a.role == "brain")
                        .map(|a| a.name.clone())
                } else {
                    None
                },
                started_at: Some(Instant::now()),
                cost: 0.0,
            }),
        }
        let role_label = role
            .chars()
            .next()
            .unwrap_or('?')
            .to_uppercase()
            .collect::<String>()
            + &role[1..];
        self.activity_log.push(LogEntry {
            timestamp: Self::now_stamp(),
            prefix: format!("[{}:{}]", role, agent),
            message: format!("{} agent spawned", role_label),
            kind: LogEntryKind::Info,
        });
    }

    /// Compute total cost from per-agent costs.
    fn total_cost(&self) -> f64 {
        self.cost_by_agent.values().sum()
    }

    /// Format elapsed time since TUI start as "Xm Ys".
    fn elapsed(&self) -> String {
        let secs = self.start_time.elapsed().as_secs();
        let m = secs / 60;
        let s = secs % 60;
        format!("{}m {:02}s", m, s)
    }

    /// Get the Nth session ID (1-indexed) from session_agent.
    fn nth_session_id(&self, n: usize) -> Option<String> {
        self.session_agent
            .keys()
            .nth(n.saturating_sub(1))
            .cloned()
    }

    /// Get the first session ID.
    fn first_session_id(&self) -> Option<String> {
        self.session_agent.keys().next().cloned()
    }

    /// Look up the (agent_name, role) for a given session id.
    pub fn agent_info_for_session(&self, session_id: &str) -> Option<(String, String)> {
        self.session_agent
            .get(session_id)
            .map(|(role, agent)| (agent.clone(), role.clone()))
    }
}

impl View for DashboardView {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                // Default visible height estimate; actual value depends on render area
                self.activity_log.scroll_down(20);
                Some(Action::ScrollDown)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.activity_log.scroll_up();
                Some(Action::ScrollUp)
            }
            KeyCode::Char('g') => {
                self.activity_log.scroll_to_top();
                Some(Action::ScrollToTop)
            }
            KeyCode::Char('G') => {
                self.activity_log.scroll_to_bottom();
                Some(Action::ScrollToBottom)
            }
            KeyCode::Tab => {
                self.focused_panel = match self.focused_panel {
                    Panel::Agents => Panel::Log,
                    Panel::Log => Panel::Agents,
                };
                self.agents_tree
                    .set_focused(self.focused_panel == Panel::Agents);
                self.activity_log
                    .set_focused(self.focused_panel == Panel::Log);
                Some(Action::CycleFocus)
            }
            KeyCode::Enter => self
                .first_session_id()
                .map(|sid| Action::NavigateTo(ViewId::SessionDetail(spur_acp::SessionId(sid)))),
            KeyCode::Char(c @ '1'..='9') => {
                let n = (c as u8 - b'0') as usize;
                self.nth_session_id(n)
                    .map(|sid| Action::NavigateTo(ViewId::SessionDetail(spur_acp::SessionId(sid))))
            }
            KeyCode::Char('v') => Some(Action::ToggleVerbose),
            KeyCode::Char('i') => self
                .first_session_id()
                .map(|sid| Action::NavigateTo(ViewId::SessionDetail(spur_acp::SessionId(sid)))),
            KeyCode::Char('?') => Some(Action::ShowHelp),
            KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
            _ => None,
        }
    }

    fn handle_spur_event(&mut self, event: &SpurEvent) {
        match event {
            SpurEvent::BrainSpawned { agent, session } => {
                self.handle_agent_spawned(agent.clone(), session.0.clone(), "brain");
            }

            SpurEvent::WorkerSpawned {
                agent,
                session,
                worktree: _,
            } => {
                self.handle_agent_spawned(agent.clone(), session.0.clone(), "worker");
            }

            SpurEvent::AgentOutput {
                session,
                event: se,
            } => {
                let prefix = self.prefix_for_session(&session.0);
                match se {
                    SessionEvent::TextDelta(text) => {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            if self.verbose {
                                self.activity_log.push(LogEntry {
                                    timestamp: Self::now_stamp(),
                                    prefix: prefix.clone(),
                                    message: trimmed.to_string(),
                                    kind: LogEntryKind::Think,
                                });
                            } else {
                                // Accumulate in text_batch for batched flushing
                                let entry = self
                                    .text_batch
                                    .entry(session.0.clone())
                                    .or_insert_with(|| (String::new(), Instant::now()));
                                entry.0.push_str(trimmed);
                                entry.1 = Instant::now();
                            }
                        }
                    }
                    SessionEvent::ToolCallStart { name, .. } => {
                        self.activity_log.push(LogEntry {
                            timestamp: Self::now_stamp(),
                            prefix,
                            message: format!("\u{1f527} Tool: {}", name),
                            kind: LogEntryKind::Act,
                        });
                    }
                    SessionEvent::ToolCallResult { .. } => {
                        // Not logged in dashboard (condensed view)
                    }
                    SessionEvent::StatusUpdate(status) => {
                        let status_str = format!("{:?}", status).to_lowercase();
                        self.set_agent_status_for_session(&session.0, &status_str);
                        self.activity_log.push(LogEntry {
                            timestamp: Self::now_stamp(),
                            prefix,
                            message: format!("Status: {}", status_str),
                            kind: LogEntryKind::Info,
                        });
                    }
                    SessionEvent::Error { message, .. } => {
                        self.set_agent_status_for_session(&session.0, "error");
                        self.activity_log.push(LogEntry {
                            timestamp: Self::now_stamp(),
                            prefix,
                            message: format!("Error: {}", message),
                            kind: LogEntryKind::Error,
                        });
                    }
                    SessionEvent::RateLimitHit { retry_after } => {
                        self.set_agent_status_for_session(&session.0, "rate-limited");
                        let msg = match retry_after {
                            Some(d) => {
                                format!("Rate limited (retry after {}s)", d.as_secs())
                            }
                            None => "Rate limited".to_string(),
                        };
                        self.activity_log.push(LogEntry {
                            timestamp: Self::now_stamp(),
                            prefix,
                            message: msg,
                            kind: LogEntryKind::Info,
                        });
                    }
                    SessionEvent::Complete { .. } => {
                        self.set_agent_status_for_session(&session.0, "done");
                        self.activity_log.push(LogEntry {
                            timestamp: Self::now_stamp(),
                            prefix,
                            message: "Session complete".to_string(),
                            kind: LogEntryKind::Complete,
                        });
                    }
                }
            }

            SpurEvent::DelegationRequested {
                from: _,
                to_agent,
                task,
            } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[brain]".to_string(),
                    message: format!("Delegating to {}: {}", to_agent, task),
                    kind: LogEntryKind::Delegate,
                });
            }

            SpurEvent::DelegationCompleted {
                worker_session,
                status,
            } => {
                let prefix = self.prefix_for_session(&worker_session.0);
                let status_str = match status {
                    DelegationStatus::Success => "done",
                    _ => "error",
                };
                self.set_agent_status_for_session(&worker_session.0, status_str);
                let msg = match status {
                    DelegationStatus::Success => {
                        "Delegation completed successfully".to_string()
                    }
                    DelegationStatus::Failed { error } => {
                        format!("Delegation failed: {}", error)
                    }
                    DelegationStatus::Conflict { files } => {
                        format!("Delegation conflict in {} files", files.len())
                    }
                    DelegationStatus::Timeout => "Delegation timed out".to_string(),
                };
                let kind = match status {
                    DelegationStatus::Success => LogEntryKind::Complete,
                    _ => LogEntryKind::Error,
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: msg,
                    kind,
                });
            }

            SpurEvent::SessionCompleted { session, success } => {
                let prefix = self.prefix_for_session(&session.0);
                let status = if *success { "done" } else { "error" };
                self.set_agent_status_for_session(&session.0, status);
                let msg = if *success {
                    "Session completed successfully"
                } else {
                    "Session failed"
                };
                let kind = if *success {
                    LogEntryKind::Complete
                } else {
                    LogEntryKind::Error
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: msg.to_string(),
                    kind,
                });
            }

            SpurEvent::RateLimitDetected {
                agent,
                retry_after,
            } => {
                if let Some(a) = self.agents.iter_mut().find(|a| a.name == *agent) {
                    a.status = "rate-limited".into();
                }
                let msg = match retry_after {
                    Some(d) => format!("Rate limited (retry after {}s)", d.as_secs()),
                    None => "Rate limited".to_string(),
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[{}]", agent),
                    message: msg,
                    kind: LogEntryKind::Info,
                });
            }

            SpurEvent::BrainFailover { from, to } => {
                if let Some(a) = self.agents.iter_mut().find(|a| a.name == *from) {
                    a.status = "error".into();
                }
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[spur]".to_string(),
                    message: format!("Brain failover: {} -> {}", from, to),
                    kind: LogEntryKind::Error,
                });
            }

            SpurEvent::CostUpdate {
                session: _,
                agent,
                estimated_cost_usd,
            } => {
                *self.cost_by_agent.entry(agent.clone()).or_insert(0.0) += estimated_cost_usd;
                // Also update the agent's cost field
                if let Some(a) = self.agents.iter_mut().find(|a| a.name == *agent) {
                    a.cost = *self.cost_by_agent.get(agent.as_str()).unwrap_or(&0.0);
                }
            }

            SpurEvent::ConflictDetected { files } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[spur]".to_string(),
                    message: format!(
                        "Conflict detected in {} file(s): {}",
                        files.len(),
                        files
                            .iter()
                            .map(|f| f.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEvent::IssueReceived { source, id } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[pm]".to_string(),
                    message: format!("Issue received from {}: {}", source, id),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEvent::PrCreated { url } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[spur]".to_string(),
                    message: format!("PR created: {}", url),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEvent::IssueUpdated { source, id, status } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[pm]".to_string(),
                    message: format!("Issue {} ({}) updated: {}", id, source, status),
                    kind: LogEntryKind::Info,
                });
            }
        }
    }

    fn tick(&mut self) {
        self.agents_tree.tick();

        // Flush text batches older than 500ms
        let threshold = std::time::Duration::from_millis(500);
        let now = Instant::now();
        let expired: Vec<String> = self
            .text_batch
            .iter()
            .filter(|(_, (_, ts))| now.duration_since(*ts) > threshold)
            .map(|(k, _)| k.clone())
            .collect();

        for session_id in expired {
            if let Some((text, _)) = self.text_batch.remove(&session_id) {
                let prefix = self.prefix_for_session(&session_id);
                // Take the last 50 chars for a condensed view
                let display = if text.len() > 50 {
                    format!("\u{25b8} ...{}", &text[text.len() - 50..])
                } else {
                    format!("\u{25b8} {}", text)
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: display,
                    kind: LogEntryKind::Think,
                });
            }
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        if self.agents.is_empty() {
            // Empty state: centered welcome message
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "SPUR",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Waiting for agents to spawn...",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Run `spur run <issue>` to start",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            let paragraph = Paragraph::new(lines)
                .alignment(ratatui::layout::Alignment::Center);

            // Use the full area minus 1 line for the status bar
            let chunks = Layout::vertical([
                Constraint::Min(4),
                Constraint::Length(1),
            ])
            .split(area);

            // Center vertically
            let v_pad = chunks[0].height.saturating_sub(6) / 2;
            let content_area = Rect {
                x: chunks[0].x,
                y: chunks[0].y + v_pad,
                width: chunks[0].width,
                height: chunks[0].height.saturating_sub(v_pad),
            };
            frame.render_widget(paragraph, content_area);
            StatusBar::render(
                frame,
                chunks[1],
                &ViewId::Dashboard,
                self.total_cost(),
                &self.elapsed(),
            );
            return;
        }

        // Normal layout: agents tree on top, activity log fills middle, status bar at bottom
        let agents_height = (self.agents.len() as u16 + 2)
            .clamp(4, area.height * 40 / 100)
            .min(12);

        let chunks = Layout::vertical([
            Constraint::Length(agents_height), // agents tree
            Constraint::Min(4),               // activity log (fills)
            Constraint::Length(1),             // status bar
        ])
        .split(area);

        self.agents_tree.render(frame, chunks[0], &self.agents);
        self.activity_log.render(frame, chunks[1]);
        StatusBar::render(
            frame,
            chunks[2],
            &ViewId::Dashboard,
            self.total_cost(),
            &self.elapsed(),
        );
    }
}
