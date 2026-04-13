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

use spur_acp::{DelegationStatus, SpurEvent};

use crate::action::{Action, ViewId};
use crate::components::activity_log::ActivityLog;
use crate::components::agents_tree::AgentsTree;
use crate::components::input_bar::InputBar;
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
    input_bar: InputBar,
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
            input_bar: InputBar::new(),
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
        crate::components::now_stamp()
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


    /// Get the first session ID.
    fn first_session_id(&self) -> Option<String> {
        self.session_agent.keys().next().cloned()
    }

    pub fn scroll_activity_up(&mut self) {
        self.activity_log.scroll_up();
    }

    pub fn scroll_activity_down(&mut self) {
        self.activity_log.scroll_down(20);
    }

    pub fn scroll_activity_up_by(&mut self, lines: usize) {
        self.activity_log.scroll_up_by(lines);
    }

    pub fn scroll_activity_down_by(&mut self, lines: usize) {
        self.activity_log.scroll_down_by(lines, 20);
    }

    /// Whether any agents are in an active (animating) state.
    pub fn has_active_agents(&self) -> bool {
        self.agents.iter().any(|a| matches!(a.status.as_str(), "working" | "spawned"))
    }

    /// Look up the (agent_name, role) for a given session id.
    pub fn agent_info_for_session(&self, session_id: &str) -> Option<(String, String)> {
        self.session_agent
            .get(session_id)
            .map(|(role, agent)| (agent.clone(), role.clone()))
    }

    /// Update the brain status label shown in the InputBar.
    pub fn set_brain_status(&mut self, name: Option<&str>, status: &str) {
        let label = match (name, status) {
            (_, "idle") => None,
            (Some(n), "thinking") => Some(format!("[{} \u{00b7}\u{00b7}\u{00b7}]", n)),
            (Some(n), "streaming") => Some(format!("[{} \u{25b8}\u{25b8}\u{25b8}]", n)),
            (Some(n), "ready") => Some(format!("[{}: ready]", n)),
            (Some(n), "error") => Some(format!("[{}: error]", n)),
            (None, _) => None,
            (Some(n), other) => Some(format!("[{}: {}]", n, other)),
        };
        self.input_bar.set_status(label);
    }
}

impl View for DashboardView {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        // Priority 1: If key is printable or editing, route to InputBar
        let is_editing_key = matches!(
            key.code,
            KeyCode::Char(_)
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Enter
        );

        if is_editing_key {
            // Check if InputBar handles it (Enter on non-empty submits)
            if let Some((text, interrupt)) = self.input_bar.handle_key(key) {
                // Text submitted — send as message
                return Some(Action::SendMessage {
                    session: spur_acp::SessionId::new(),
                    text,
                    interrupt,
                });
            }

            // If InputBar was empty and user typed a navigation char, treat as nav
            if self.input_bar.text().len() == 1 {
                let ch = self.input_bar.text().chars().next().unwrap();
                match ch {
                    'j' => {
                        self.input_bar.clear();
                        self.activity_log.scroll_down(20);
                        return Some(Action::ScrollDown);
                    }
                    'k' => {
                        self.input_bar.clear();
                        self.activity_log.scroll_up();
                        return Some(Action::ScrollUp);
                    }
                    'g' => {
                        self.input_bar.clear();
                        self.activity_log.scroll_to_top();
                        return Some(Action::ScrollToTop);
                    }
                    'G' => {
                        self.input_bar.clear();
                        self.activity_log.scroll_to_bottom();
                        return Some(Action::ScrollToBottom);
                    }
                    'v' => {
                        self.input_bar.clear();
                        self.verbose = !self.verbose;
                        return Some(Action::ToggleVerbose);
                    }
                    'q' => {
                        self.input_bar.clear();
                        return Some(Action::Quit);
                    }
                    '?' => {
                        self.input_bar.clear();
                        return Some(Action::ShowHelp);
                    }
                    's' => {
                        self.input_bar.clear();
                        return Some(Action::RequestSessions);
                    }
                    _ => {}
                }
            }

            // Enter on empty InputBar → drill into session (if any)
            if key.code == KeyCode::Enter && self.input_bar.is_empty() {
                return self.first_session_id().map(|sid| {
                    Action::NavigateTo(ViewId::SessionDetail(spur_acp::SessionId(sid)))
                });
            }

            return None;
        }

        // Priority 2: Non-editing keys when InputBar is empty
        if self.input_bar.is_empty() {
            match key.code {
                KeyCode::Up => {
                    self.activity_log.scroll_up();
                    return Some(Action::ScrollUp);
                }
                KeyCode::Down => {
                    self.activity_log.scroll_down(20);
                    return Some(Action::ScrollDown);
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
                    return Some(Action::CycleFocus);
                }
                KeyCode::Esc => return Some(Action::Quit),
                _ => {}
            }
        }

        None
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

            SpurEvent::AgentNotification { session, notification } => {
                let prefix = self.prefix_for_session(&session.0);
                match &notification.update {
                    spur_acp::SessionUpdate::AgentThoughtChunk(chunk)
                    | spur_acp::SessionUpdate::AgentMessageChunk(chunk) => {
                        if let spur_acp::ContentBlock::Text(tc) = &chunk.content {
                            let trimmed = tc.text.trim();
                            if !trimmed.is_empty() {
                                let entry = self.text_batch
                                    .entry(session.0.clone())
                                    .or_insert_with(|| (String::new(), Instant::now()));
                                entry.0.push_str(trimmed);
                                if entry.0.len() > 200 {
                                    let mut start = entry.0.len() - 200;
                                    while !entry.0.is_char_boundary(start) {
                                        start += 1;
                                    }
                                    entry.0 = entry.0[start..].to_string();
                                }
                                entry.1 = Instant::now();
                            }
                        }
                    }
                    spur_acp::SessionUpdate::ToolCall(tc) => {
                        self.activity_log.push(LogEntry {
                            timestamp: Self::now_stamp(),
                            prefix,
                            message: format!("\u{1f527} Tool: {}", tc.title),
                            kind: LogEntryKind::Act,
                        });
                    }
                    spur_acp::SessionUpdate::ToolCallUpdate(_) => {
                        // Not logged in dashboard (condensed view)
                    }
                    _ => {
                        // Other variants -- update status
                        self.set_agent_status_for_session(&session.0, "working");
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

            SpurEvent::TurnComplete { session } => {
                let prefix = self.prefix_for_session(&session.0);
                self.set_agent_status_for_session(&session.0, "idle");
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: "Turn complete".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEvent::BrainError { session, message } => {
                let prefix = self.prefix_for_session(&session.0);
                self.set_agent_status_for_session(&session.0, "error");
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: format!("Brain error: {}", message),
                    kind: LogEntryKind::Error,
                });
            }
            _ => {}
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
                    let mut start = text.len() - 50;
                    while !text.is_char_boundary(start) {
                        start += 1;
                    }
                    format!("\u{25b8} ...{}", &text[start..])
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
                    "Multi-agent orchestrator",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Type a task below to start",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Press [s] to browse sessions",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            let paragraph = Paragraph::new(lines)
                .alignment(ratatui::layout::Alignment::Center);

            let input_height = self.input_bar.required_height();
            let chunks = Layout::vertical([
                Constraint::Min(4),
                Constraint::Length(input_height),
                Constraint::Length(1),
            ])
            .split(area);

            let v_pad = chunks[0].height.saturating_sub(6) / 2;
            let content_area = Rect {
                x: chunks[0].x,
                y: chunks[0].y + v_pad,
                width: chunks[0].width,
                height: chunks[0].height.saturating_sub(v_pad),
            };
            frame.render_widget(paragraph, content_area);
            self.input_bar.render(frame, chunks[1]);
            StatusBar::render(
                frame,
                chunks[2],
                &ViewId::Dashboard,
                self.total_cost(),
                &self.elapsed(),
                None,
                None,
                None,
            );
            return;
        }

        let agents_height = (self.agents.len() as u16 + 2)
            .clamp(4, area.height * 40 / 100)
            .min(12);

        let input_height = self.input_bar.required_height();

        let chunks = Layout::vertical([
            Constraint::Length(agents_height),    // agents tree
            Constraint::Min(4),                  // activity log (fills)
            Constraint::Length(input_height),     // input bar
            Constraint::Length(1),                // status bar
        ])
        .split(area);

        self.agents_tree.render(frame, chunks[0], &self.agents);
        self.activity_log.render(frame, chunks[1]);
        self.input_bar.render(frame, chunks[2]);
        StatusBar::render(
            frame,
            chunks[3],
            &ViewId::Dashboard,
            self.total_cost(),
            &self.elapsed(),
            None,
            None,
            None,
        );
    }
}
