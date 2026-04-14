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

use spur_acp::{DelegationStatus, SpurEvent, SpurEventBody};
use spur_core::{ExecutorId, ExecutorLineage};

use crate::action::{Action, ViewId};
use crate::components::activity_log::ActivityLog;
use crate::components::agents_tree::AgentsTree;
use crate::components::detail_pane::{DetailPane, DetailTab};
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
/// Dashboard rendering flows through `render_with_lineage` so the detail
/// pane can access the event-sourced projection owned by `App`. The
/// `View::render` method is a no-op — `App::render` dispatches directly.
pub struct DashboardView {
    agents_tree: AgentsTree,
    activity_log: ActivityLog,
    detail_pane: DetailPane,
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
            detail_pane: DetailPane::new(),
            input_bar: InputBar::new(),
            focused_panel: Panel::Log,
            focused_node: None,
            verbose: false,
            text_batch: HashMap::new(),
            start_time: Instant::now(),
        }
    }

    /// Current local time formatted as HH:MM:SS.
    /// Render the one-line empty-state hint above the InputBar when no brain
    /// is attached and the user hasn't started typing. No-op otherwise.
    fn render_empty_state_hint(&self, frame: &mut Frame, area: Rect, input_bar_area: Rect) {
        if self.input_bar.has_status() || !self.input_bar.text().is_empty() {
            return;
        }
        let hint_y = input_bar_area.y.saturating_sub(1);
        if hint_y < area.y {
            return;
        }
        let hint_area = Rect {
            x: input_bar_area.x,
            y: hint_y,
            width: input_bar_area.width,
            height: 1,
        };
        let hint = Paragraph::new(Span::styled(
            " Type to start a new session \u{00b7} s for sessions \u{00b7} ? for help",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(hint, hint_area);
    }

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

    /// Read-only access to the activity log. Intended for tests.
    pub fn activity_log(&self) -> &ActivityLog {
        &self.activity_log
    }

    pub fn set_focused_node(&mut self, id: Option<ExecutorId>) {
        self.focused_node = id;
    }

    pub fn focused_node(&self) -> Option<&ExecutorId> {
        self.focused_node.as_ref()
    }

    pub fn detail_pane(&self) -> &DetailPane {
        &self.detail_pane
    }

    pub fn detail_pane_mut(&mut self) -> &mut DetailPane {
        &mut self.detail_pane
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

        // Compute aggregates once for both empty and non-empty paths.
        let running = lineage
            .nodes()
            .filter(|n| matches!(
                n.phase,
                spur_core::LifecycleState::Running | spur_core::LifecycleState::Spawning,
            ))
            .count();
        let pending_review = lineage.pending_reviews().len();
        let total_cost: f64 = lineage
            .nodes()
            .map(|n| n.current_attempt().map(|a| a.cost_usd).unwrap_or(0.0))
            .sum();
        let elapsed = self.elapsed();

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
            let input_bar_area = chunks[1];
            self.render_empty_state_hint(frame, area, input_bar_area);
            self.input_bar.render(frame, input_bar_area);
            StatusBar::render(
                frame,
                chunks[2],
                StatusBarProps {
                    view: &ViewId::Dashboard,
                    running,
                    pending_review,
                    total_cost,
                    elapsed: &elapsed,
                    current_mode: None,
                    context_used: None,
                    context_size: None,
                    stream_in_flight: false,
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
        match &self.focused_node {
            Some(id) => {
                if let Some(node) = lineage.node(id) {
                    self.detail_pane.render(frame, chunks[1], node);
                } else {
                    self.activity_log.render(frame, chunks[1]);
                }
            }
            None => {
                self.activity_log.render(frame, chunks[1]);
            }
        }
        let input_bar_area = chunks[2];
        self.render_empty_state_hint(frame, area, input_bar_area);
        self.input_bar.render(frame, input_bar_area);
        StatusBar::render(
            frame,
            chunks[3],
            StatusBarProps {
                view: &ViewId::Dashboard,
                running,
                pending_review,
                total_cost,
                elapsed: &elapsed,
                current_mode: None,
                context_used: None,
                context_size: None,
                stream_in_flight: false,
            },
        );
    }
}

impl DashboardView {
    /// Handle a key event with access to the lineage projection so that the
    /// emitted `Action::SubmitReview` can carry the correct `attempt_n`.
    /// Called by `App` instead of the `View::handle_key` trait method.
    pub fn handle_key_with_lineage(
        &mut self,
        key: KeyEvent,
        lineage: &ExecutorLineage,
    ) -> Option<Action> {
        self.handle_key_inner(key, Some(lineage))
    }

    fn handle_key_inner(
        &mut self,
        key: KeyEvent,
        lineage: Option<&ExecutorLineage>,
    ) -> Option<Action> {
        // Priority 0: Tab-cycling in detail pane when a node is focused and
        // the input bar is empty. Must be checked before the editing-key block
        // so that Left/Right are not consumed by InputBar cursor movement.
        if self.input_bar.is_empty() && self.focused_node.is_some() {
            match key.code {
                KeyCode::Right => {
                    self.detail_pane.cycle_tab(true);
                    return None;
                }
                KeyCode::Left => {
                    self.detail_pane.cycle_tab(false);
                    return None;
                }
                _ => {}
            }
        }

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
                let blocks = vec![spur_acp::ContentBlock::Text(
                    spur_acp::TextContent::new(text),
                )];
                // Routing: if brain is attached (status is set), this is a
                // routed message to the active session — App substitutes the
                // correct SessionId. Otherwise it's an explicit new-session
                // intent that spawns a brain atomically with the first prompt.
                if self.input_bar.has_status() {
                    return Some(Action::SendMessage {
                        // Placeholder — App replaces with active session id.
                        session: spur_acp::SessionId(String::new()),
                        blocks,
                        interrupt,
                    });
                } else {
                    return Some(Action::NewSessionWithMessage { blocks, interrupt });
                }
            }

            // If InputBar has exactly one char and we're in the Review tab,
            // intercept review decision keys before general navigation.
            if self.input_bar.text().len() == 1
                && self.focused_node.is_some()
                && self.detail_pane.current_tab == DetailTab::Review
            {
                let ch = self.input_bar.text().chars().next().unwrap();
                match ch {
                    ch @ ('a' | 'd' | 'm' | 'R') => {
                        self.input_bar.clear();
                        if let Some(decision) =
                            crate::components::review_card::decision_for_key(ch, None)
                        {
                            if let Some(id) = self.focused_node.clone() {
                                let attempt_n = lineage
                                    .and_then(|l| l.node(&id))
                                    .and_then(|n| n.pending_review.as_ref().map(|r| r.attempt_n))
                                    .unwrap_or(1);
                                return Some(Action::SubmitReview {
                                    executor_id: id.0,
                                    attempt_n,
                                    decision,
                                });
                            }
                        }
                        return None;
                    }
                    _ => {}
                }
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
                KeyCode::Esc if self.focused_node.is_some() => {
                    return Some(Action::UnfocusNode);
                }
                // Esc is the universal "back" key. App decides: if an
                // active SessionDetail is alive, Esc returns to it; if
                // not, Esc quits (possibly through the quit-confirm
                // dialog when a brain is attached).
                KeyCode::Esc => return Some(Action::NavigateBack),
                _ => {}
            }
        }

        None
    }
}

impl View for DashboardView {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        debug_assert!(
            false,
            "DashboardView::handle_key called via trait — use handle_key_with_lineage; attempt_n will default to 1"
        );
        self.handle_key_inner(key, None)
    }

    fn handle_spur_event(&mut self, event: &SpurEvent) {
        match &event.body {
            SpurEventBody::BrainSpawned { agent, session: _ } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[brain:{}]", agent),
                    message: "Brain agent spawned".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::WorkerSpawned {
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

            SpurEventBody::AgentNotification { session, notification } => {
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

            SpurEventBody::DelegationRequested {
                from: _,
                to_agent,
                task,
                request_id: _,
            } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[brain]".to_string(),
                    message: format!("Delegating to {}: {}", to_agent, task),
                    kind: LogEntryKind::Delegate,
                });
            }

            SpurEventBody::DelegationCompleted {
                worker_session,
                status,
            } => {
                let prefix = Self::prefix_for_session(&worker_session.0);
                let (msg, kind) = match status {
                    DelegationStatus::Success => (
                        "Delegation completed successfully".to_string(),
                        LogEntryKind::Complete,
                    ),
                    DelegationStatus::Failed { error } => (
                        format!("Delegation failed: {}", error),
                        LogEntryKind::Error,
                    ),
                    DelegationStatus::Conflict { files } => (
                        format!("Delegation conflict in {} files", files.len()),
                        LogEntryKind::Error,
                    ),
                    DelegationStatus::Timeout => (
                        "Delegation timed out".to_string(),
                        LogEntryKind::Error,
                    ),
                    DelegationStatus::Rejected { reason } => (
                        format!("Delegation rejected: {}", reason),
                        LogEntryKind::Error,
                    ),
                    DelegationStatus::Modified { reviewer_note } => (
                        format!("Delegation modified: {}", reviewer_note),
                        LogEntryKind::Complete,
                    ),
                    DelegationStatus::TimedOut { waited_for, fallback } => (
                        format!(
                            "Delegation review timed out after {}s (fallback: {:?})",
                            waited_for.as_secs(),
                            fallback
                        ),
                        LogEntryKind::Error,
                    ),
                    _ => {
                        tracing::warn!("unknown DelegationStatus variant in dashboard activity log — update needed");
                        ("Delegation completed".to_string(), LogEntryKind::Error)
                    }
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: msg,
                    kind,
                });
            }

            SpurEventBody::SessionCompleted { session, success } => {
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

            SpurEventBody::RateLimitDetected {
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

            SpurEventBody::BrainFailover { from, to } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[spur]".to_string(),
                    message: format!("Brain failover: {} -> {}", from, to),
                    kind: LogEntryKind::Error,
                });
            }

            SpurEventBody::CostUpdate { .. } => {
                // Cost is now read from lineage.nodes().current_attempt().cost_usd
            }

            SpurEventBody::ConflictDetected { files } => {
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

            SpurEventBody::IssueReceived { source, id } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[pm]".to_string(),
                    message: format!("Issue received from {}: {}", source, id),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::PrCreated { url } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[spur]".to_string(),
                    message: format!("PR created: {}", url),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::IssueUpdated { source, id, status } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[pm]".to_string(),
                    message: format!("Issue {} ({}) updated: {}", id, source, status),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::TurnComplete { session } => {
                let prefix = Self::prefix_for_session(&session.0);
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: "Turn complete".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::BrainError { session, message } => {
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

    fn render(&self, _frame: &mut Frame, _area: Rect) {
        // SAFETY: Dashboard always renders via `render_with_lineage`. This
        // trait method is kept to satisfy `View` but should never be called.
        debug_assert!(false, "DashboardView::render called via trait — use render_with_lineage");
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
