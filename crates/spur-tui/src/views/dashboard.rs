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
use spur_core::{ExecutorId, ExecutorLineage};

use crate::action::{Action, ViewId};
use crate::components::activity_log::ActivityLog;
use crate::components::agents_tree::AgentsTree;
use crate::components::input_bar::InputBar;
use crate::components::status_bar::{StatusBar, StatusBarProps};
use crate::components::{LogEntry, LogEntryKind};

use super::View;

/// Which panel currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Agents,
    Log,
}

/// The main dashboard view composing AgentsTree + ActivityLog + StatusBar.
/// All agent-state is now read from `ExecutorLineage` (owned by `App`);
/// this struct only owns the activity log and UI controls.
///
/// **Always use `render_with_lineage` from `App::render` — the `View::render`
/// method is a no-op because it cannot access the lineage.**
pub struct DashboardView {
    agents_tree: AgentsTree,
    activity_log: ActivityLog,
    input_bar: InputBar,
    focused_panel: Panel,
    focused_node: Option<ExecutorId>,
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
            focused_panel: Panel::Log,
            focused_node: None,
            verbose: false,
            text_batch: HashMap::new(),
            start_time: Instant::now(),
        }
    }

    /// Current local time formatted as HH:MM:SS.
    fn now_stamp() -> String {
        crate::components::now_stamp()
    }

    /// Build a short prefix from a session id when no lineage lookup is available.
    fn prefix_for_session(session_id: &str) -> String {
        format!("[{}]", &session_id[..8.min(session_id.len())])
    }

    /// Format elapsed time since TUI start as "Xm Ys".
    fn elapsed(&self) -> String {
        let secs = self.start_time.elapsed().as_secs();
        let m = secs / 60;
        let s = secs % 60;
        format!("{}m {:02}s", m, s)
    }

    pub fn agents_tree_mut(&mut self) -> &mut AgentsTree {
        &mut self.agents_tree
    }

    pub fn set_focused_node(&mut self, id: Option<ExecutorId>) {
        self.focused_node = id;
    }

    pub fn focused_node(&self) -> Option<&ExecutorId> {
        self.focused_node.as_ref()
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

    /// Whether any executors are in an active (animating) state.
    /// Now delegates to the lineage-aware caller; always returns false here
    /// so callers should use `has_active_executors_in_lineage` instead.
    pub fn has_active_agents(&self) -> bool {
        // Conservatively return true so the tick loop stays active when agents
        // are running. App::tick still calls this; it will be driven by dirty
        // flag from lineage events in practice.
        false
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

    /// Render the dashboard with access to the current lineage projection.
    /// This is the canonical render path; called directly from `App::render`.
    pub fn render_with_lineage(
        &self,
        frame: &mut Frame,
        area: Rect,
        lineage: &ExecutorLineage,
    ) {
        let node_count = lineage.nodes().count();

        if node_count == 0 {
            // Empty state: splash screen
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
                StatusBarProps {
                    view: &ViewId::Dashboard,
                    total_cost: lineage.nodes().filter_map(|n| n.current_attempt()).map(|a| a.cost_usd).sum(),
                    elapsed: &self.elapsed(),
                    current_mode: None,
                    context_used: None,
                    context_size: None,
                },
            );
            return;
        }

        let agents_height = (node_count as u16 + 2)
            .clamp(4, area.height * 40 / 100)
            .min(12);

        let input_height = self.input_bar.required_height();

        let chunks = Layout::vertical([
            Constraint::Length(agents_height),   // lineage tree
            Constraint::Min(4),                  // activity log (fills)
            Constraint::Length(input_height),    // input bar
            Constraint::Length(1),               // status bar
        ])
        .split(area);

        self.agents_tree.render(frame, chunks[0], lineage);
        self.activity_log.render(frame, chunks[1]);
        self.input_bar.render(frame, chunks[2]);
        StatusBar::render(
            frame,
            chunks[3],
            StatusBarProps {
                view: &ViewId::Dashboard,
                total_cost: lineage.nodes().filter_map(|n| n.current_attempt()).map(|a| a.cost_usd).sum(),
                elapsed: &self.elapsed(),
                current_mode: None,
                context_used: None,
                context_size: None,
            },
        );
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
                    'j' if self.focused_panel == Panel::Agents => {
                        self.input_bar.clear();
                        return Some(Action::SelectNext);
                    }
                    'j' => {
                        self.input_bar.clear();
                        self.activity_log.scroll_down(20);
                        return Some(Action::ScrollDown);
                    }
                    'k' if self.focused_panel == Panel::Agents => {
                        self.input_bar.clear();
                        return Some(Action::SelectPrev);
                    }
                    'k' => {
                        self.input_bar.clear();
                        self.activity_log.scroll_up();
                        return Some(Action::ScrollUp);
                    }
                    'r' => {
                        self.input_bar.clear();
                        return Some(Action::JumpToReview);
                    }
                    'c' if self.focused_panel == Panel::Agents => {
                        self.input_bar.clear();
                        return Some(Action::ToggleCollapse);
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

            // Enter on empty InputBar: FocusNode if agents panel is focused, else no-op
            if key.code == KeyCode::Enter && self.input_bar.is_empty() {
                if self.focused_panel == Panel::Agents {
                    return Some(Action::FocusNode);
                }
                return None;
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
                KeyCode::Esc if self.focused_panel == Panel::Agents && self.focused_node.is_some() => {
                    return Some(Action::UnfocusNode);
                }
                KeyCode::Esc => return Some(Action::Quit),
                _ => {}
            }
        }

        None
    }

    fn handle_spur_event(&mut self, event: &SpurEvent) {
        match event {
            SpurEvent::BrainSpawned { agent, session: _ } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[brain:{}]", agent),
                    message: "Brain agent spawned".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEvent::WorkerSpawned {
                agent,
                session: _,
                worktree: _,
            } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[worker:{}]", agent),
                    message: "Worker agent spawned".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEvent::AgentNotification { session, notification } => {
                let prefix = Self::prefix_for_session(&session.0);
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
                        // Other variants — no agent-state mutation needed; lineage handles it
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
                let prefix = Self::prefix_for_session(&worker_session.0);
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
                let prefix = Self::prefix_for_session(&session.0);
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
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[spur]".to_string(),
                    message: format!("Brain failover: {} -> {}", from, to),
                    kind: LogEntryKind::Error,
                });
            }

            SpurEvent::CostUpdate { .. } => {
                // Cost is now read from lineage.nodes().current_attempt().cost_usd
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
                let prefix = Self::prefix_for_session(&session.0);
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: "Turn complete".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEvent::BrainError { session, message } => {
                let prefix = Self::prefix_for_session(&session.0);
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
        self.tick_and_report_flush();
    }

    /// No-op: always use `render_with_lineage` for Dashboard.
    /// `App::render` calls `render_with_lineage` directly.
    fn render(&self, _frame: &mut Frame, _area: Rect) {
        // render_with_lineage is called directly from App::render
    }
}

impl DashboardView {
    /// Tick + flush batched text. Returns true iff at least one batch was
    /// flushed to the activity log (so the caller can mark the TUI dirty).
    pub fn tick_and_report_flush(&mut self) -> bool {
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
        let flushed_any = !expired.is_empty();

        for session_id in expired {
            if let Some((text, _)) = self.text_batch.remove(&session_id) {
                let prefix = Self::prefix_for_session(&session_id);
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

        flushed_any
    }
}
