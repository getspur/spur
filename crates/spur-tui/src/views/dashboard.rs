use std::collections::HashMap;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
use crate::components::input_bar::{EditMode, HandleOutcome, InputBar};
use crate::components::issue_detail_pane::IssueDetailPane;
use crate::components::issues_panel::IssuesPanel;
use crate::components::status_bar::{StatusBar, StatusBarProps};
use crate::components::{LogEntry, LogEntryKind};
use crate::input_history::InputHistoryEntry;

use super::View;

/// Which panel currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Agents,
    Issues,
    Log,
}

/// State machine for issue detail focus. Invalid states are unrepresentable.
pub enum IssueFocus {
    None,
    Loading {
        id: String,
    },
    Loaded {
        id: String,
        issue: Box<spur_pm::Issue>,
    },
}

/// The main dashboard view composing AgentsTree + ActivityLog + StatusBar.
/// All agent-state is now read from `ExecutorLineage` (owned by `App`);
/// this struct only owns the activity log and UI controls.
///
/// Dashboard rendering flows through `View::render`, which delegates to
/// the private `render_with_lineage` helper so the detail pane can access
/// the event-sourced lineage projection via `ViewContext`.
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
    tracked_issues: Vec<spur_pm::IssueSummary>,
    issues_panel: IssuesPanel,
    issue_detail_pane: IssueDetailPane,
    issue_focus: IssueFocus,
    alert_summary: Option<(usize, usize, usize)>,
}

/// Convert spur_acp mirror type back to spur_pm::Issue for TUI rendering.
/// Truncate a string to a maximum display length on a UTF-8 boundary,
/// appending `…` if truncation occurred. Used for brain review feedback
/// and other free-form text that could otherwise overflow the TUI log.
fn truncate_display(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].trim_end().to_string();
    out.push('…');
    out
}

fn detail_event_to_issue(e: &spur_acp::IssueDetailEvent) -> spur_pm::Issue {
    spur_pm::Issue {
        id: e.id.clone(),
        source: match e.source.as_str() {
            "github" => spur_pm::PmSource::GitHub,
            "linear" => spur_pm::PmSource::Linear,
            "plane" => spur_pm::PmSource::Plane,
            _ => spur_pm::PmSource::Beads,
        },
        title: e.title.clone(),
        body: e.body.clone(),
        status: e.status.clone(),
        labels: e.labels.clone(),
        assignee: e.assignee.clone(),
        url: e.url.clone(),
        priority: e.priority,
        issue_type: e.issue_type.clone(),
        blocked_by: e.blocked_by.clone(),
        due_at: e.due_at,
        created_at: e.created_at,
        updated_at: e.updated_at,
    }
}

fn format_issue_badge(issue_id: &str, issues: &[spur_pm::IssueSummary]) -> String {
    let short_id: String = issue_id.chars().take(8).collect();
    if let Some(issue) = issues.iter().find(|i| i.id == *issue_id) {
        let pri = issue
            .priority
            .map(|p| format!("P{}", p))
            .unwrap_or_default();
        let max_title = 25;
        let title = if issue.title.len() > max_title {
            let mut end = max_title;
            while end < issue.title.len() && !issue.title.is_char_boundary(end) {
                end += 1;
            }
            format!("{}...", &issue.title[..end])
        } else {
            issue.title.clone()
        };
        format!("\u{25c6} {} {} {}", short_id, pri, title)
    } else {
        format!("\u{25c6} {}", short_id)
    }
}

impl Default for DashboardView {
    fn default() -> Self {
        Self::new()
    }
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
            tracked_issues: Vec::new(),
            issues_panel: IssuesPanel::new(),
            issue_detail_pane: IssueDetailPane::new(),
            issue_focus: IssueFocus::None,
            alert_summary: None,
        }
    }

    pub fn tracked_issues(&self) -> &[spur_pm::IssueSummary] {
        &self.tracked_issues
    }

    /// Current local time formatted as HH:MM:SS.
    /// Render the one-line hint above the InputBar. Shows context-sensitive
    /// hints when typing commands/mentions, or empty-state hints when idle.
    fn render_input_hint(&self, frame: &mut Frame, area: Rect, input_bar_area: Rect) {
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

        let text = self.input_bar.text();
        let hint = if text.is_empty() && !self.input_bar.has_status() {
            // Empty state hint
            Paragraph::new(Span::styled(
                " [/] command \u{00b7} [@] mention \u{00b7} [!] interrupt \u{00b7} [Alt+I] vim \u{00b7} [Alt+Enter] newline \u{00b7} ? for help",
                Style::default().fg(Color::DarkGray),
            ))
        } else if text.starts_with('/') && !text[1..].contains(char::is_whitespace) {
            // Command hint
            Paragraph::new(Span::styled(
                " Tab to select command \u{00b7} Esc to dismiss",
                Style::default().fg(Color::DarkGray),
            ))
        } else if text.contains('@')
            && !text
                .split('@')
                .next_back()
                .unwrap_or("")
                .contains(char::is_whitespace)
        {
            // Mention hint
            Paragraph::new(Span::styled(
                " Tab to select file \u{00b7} Esc to dismiss",
                Style::default().fg(Color::DarkGray),
            ))
        } else {
            return; // No hint needed
        };
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

    pub fn set_focused_panel(&mut self, panel: Panel) {
        self.focused_panel = panel;
    }

    pub fn set_edit_mode(&mut self, mode: EditMode) {
        self.input_bar.set_mode(mode);
    }

    /// Seed the InputBar with global input history (loaded from metadata).
    pub fn seed_input_history(&mut self, entries: Vec<InputHistoryEntry>) {
        self.input_bar.seed_history(entries);
    }

    pub fn handle_paste(&mut self, text: &str) {
        self.input_bar.insert_paste(text);
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
        let mention_count = self.input_bar.protected_ranges().len();
        let mention_suffix = if mention_count > 0 {
            format!(
                " \u{00b7} {} mention{}",
                mention_count,
                if mention_count > 1 { "s" } else { "" }
            )
        } else {
            String::new()
        };

        let label = match (name, status) {
            (_, "idle") => {
                if mention_count > 0 {
                    Some(format!(
                        "[{} mention{}]",
                        mention_count,
                        if mention_count > 1 { "s" } else { "" }
                    ))
                } else {
                    None
                }
            }
            (Some(n), "thinking") => Some(format!(
                "[{} \u{00b7}\u{00b7}\u{00b7}{}]",
                n, mention_suffix
            )),
            (Some(n), "streaming") => Some(format!(
                "[{} \u{25b8}\u{25b8}\u{25b8}{}]",
                n, mention_suffix
            )),
            (Some(n), "ready") => Some(format!("[{}: ready{}]", n, mention_suffix)),
            (Some(n), "error") => Some(format!("[{}: error{}]", n, mention_suffix)),
            (None, _) => {
                if mention_count > 0 {
                    Some(format!(
                        "[{} mention{}]",
                        mention_count,
                        if mention_count > 1 { "s" } else { "" }
                    ))
                } else {
                    None
                }
            }
            (Some(n), other) => Some(format!("[{}: {}{}]", n, other, mention_suffix)),
        };
        self.input_bar.set_status(label);
    }

    /// Render the dashboard with access to the current lineage projection.
    pub fn render_with_lineage(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        lineage: &ExecutorLineage,
        license_badge: Option<&crate::components::status_bar::LicenseBadge>,
        worker_streams: &mut crate::worker_streams::WorkerStreams,
    ) {
        let node_count = lineage.nodes().count();

        // Compute aggregates once for both empty and non-empty paths.
        let running = lineage
            .nodes()
            .filter(|n| {
                matches!(
                    n.phase,
                    spur_core::LifecycleState::Running | spur_core::LifecycleState::Spawning,
                )
            })
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
            let paragraph = Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center);

            let input_height = self.input_bar.required_height(area.width);
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
            self.render_input_hint(frame, area, input_bar_area);
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
                    issue_count: self.tracked_issues.len(),
                    alert_summary: self.alert_summary,
                    license_badge,
                },
            );
            return;
        }

        let agents_height = (node_count as u16 + 2)
            .clamp(4, area.height * 40 / 100)
            .min(12);

        let input_height = self.input_bar.required_height(area.width);

        let issues_height = if self.tracked_issues.is_empty() {
            0
        } else {
            crate::components::issues_panel::IssuesPanel::computed_height(
                self.tracked_issues.len(),
                area.height,
            )
        };

        let mut constraints = vec![
            Constraint::Length(agents_height), // lineage tree
        ];
        if issues_height > 0 {
            constraints.push(Constraint::Length(issues_height)); // issues panel
        }
        constraints.push(Constraint::Min(4)); // activity log (fills)
        constraints.push(Constraint::Length(input_height)); // input bar
        constraints.push(Constraint::Length(1)); // status bar

        let chunks = Layout::vertical(constraints).split(area);

        // Chunk indices depend on whether issues panel is present
        let issues_chunk = if issues_height > 0 {
            Some(1usize)
        } else {
            None
        };
        let log_chunk = if issues_height > 0 { 2 } else { 1 };
        let input_chunk = log_chunk + 1;
        let status_chunk = input_chunk + 1;

        self.agents_tree.render(frame, chunks[0], lineage);

        if let Some(ic) = issues_chunk {
            self.issues_panel
                .render(&self.tracked_issues, frame, chunks[ic]);
        }

        match &self.issue_focus {
            IssueFocus::Loading { id } => {
                IssueDetailPane::render_loading(id, frame, chunks[log_chunk]);
            }
            IssueFocus::Loaded { issue, .. } => {
                self.issue_detail_pane
                    .render(issue, frame, chunks[log_chunk]);
            }
            IssueFocus::None => match &self.focused_node {
                Some(id) => {
                    if let Some(node) = lineage.node(id) {
                        let badge = node
                            .issue_id
                            .as_ref()
                            .map(|iid| format_issue_badge(iid, &self.tracked_issues));
                        let trace = worker_streams.get_mut(&id.0);
                        self.detail_pane.render(
                            frame,
                            chunks[log_chunk],
                            node,
                            badge.as_deref(),
                            trace,
                        );
                    } else {
                        self.activity_log.render(frame, chunks[log_chunk]);
                    }
                }
                None => {
                    self.activity_log.render(frame, chunks[log_chunk]);
                }
            },
        }
        let input_bar_area = chunks[input_chunk];
        self.render_input_hint(frame, area, input_bar_area);
        self.input_bar.render(frame, input_bar_area);
        StatusBar::render(
            frame,
            chunks[status_chunk],
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
                issue_count: self.tracked_issues.len(),
                alert_summary: self.alert_summary,
                license_badge,
            },
        );
    }
}

impl DashboardView {
    fn handle_key_inner(
        &mut self,
        key: KeyEvent,
        lineage: Option<&ExecutorLineage>,
        worker_streams: &mut crate::worker_streams::WorkerStreams,
    ) -> Option<Action> {
        let key = super::normalize_macos_option(key);

        // Priority 0: Tab-cycling in detail pane when a node is focused and
        // the input bar is empty. Must be checked before the editing-key block
        // so that Left/Right are not consumed by InputBar cursor movement.
        //
        // Thread the focused executor's trace into `cycle_tab` so entering
        // the Stream tab snaps the trace to Following, and so leaving Stream
        // doesn't strand the viewport mid-history on re-entry.
        if self.input_bar.is_empty() && self.focused_node.is_some() {
            match key.code {
                KeyCode::Right => {
                    let trace = self
                        .focused_node
                        .as_ref()
                        .and_then(|id| worker_streams.get_mut(&id.0));
                    self.detail_pane.cycle_tab(true, trace);
                    return None;
                }
                KeyCode::Left => {
                    let trace = self
                        .focused_node
                        .as_ref()
                        .and_then(|id| worker_streams.get_mut(&id.0));
                    self.detail_pane.cycle_tab(false, trace);
                    return None;
                }
                _ => {}
            }
        }

        // Priority 1: If key is printable or editing, route to InputBar
        //
        // Ctrl+P / Ctrl+N → input history navigation (intercept before
        // the editing-key block so they don't get routed to InputBar).
        if matches!(key.code, KeyCode::Char('p')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.input_bar.history_prev();
            return None;
        }
        if matches!(key.code, KeyCode::Char('n')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.input_bar.history_next();
            return None;
        }

        // Alt+I → toggle vim/emacs input mode.
        if matches!(key.code, KeyCode::Char('i')) && key.modifiers.contains(KeyModifiers::ALT) {
            return Some(Action::ToggleVimMode);
        }

        // Vim Normal + empty InputBar: handle nav keys directly.
        // In Vim Normal mode, chars are consumed as commands (not inserted),
        // so the single-char-nav pattern (insert → check len==1) doesn't work.
        // Mode-entry keys (i/a/A/I/o/O) fall through to InputBar.
        if self.input_bar.is_empty() && self.input_bar.is_vim_normal() {
            if let KeyCode::Char(ch) = key.code {
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    // Review decision keys when in Review tab
                    if self.focused_node.is_some()
                        && self.detail_pane.current_tab == DetailTab::Review
                    {
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
                    }
                    let action = match ch {
                        // Quick status keys when issue detail is loaded
                        'o' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                            if let IssueFocus::Loaded { ref id, .. } = self.issue_focus {
                                return Some(Action::Issue(
                                    crate::action::IssueAction::UpdateStatus {
                                        id: id.clone(),
                                        status: "open".into(),
                                    },
                                ));
                            }
                            return None;
                        }
                        'w' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                            if let IssueFocus::Loaded { ref id, .. } = self.issue_focus {
                                return Some(Action::Issue(
                                    crate::action::IssueAction::UpdateStatus {
                                        id: id.clone(),
                                        status: "in_progress".into(),
                                    },
                                ));
                            }
                            return None;
                        }
                        'b' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                            if let IssueFocus::Loaded { ref id, .. } = self.issue_focus {
                                return Some(Action::Issue(
                                    crate::action::IssueAction::UpdateStatus {
                                        id: id.clone(),
                                        status: "blocked".into(),
                                    },
                                ));
                            }
                            return None;
                        }
                        'd' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                            if let IssueFocus::Loaded { ref id, .. } = self.issue_focus {
                                return Some(Action::Issue(
                                    crate::action::IssueAction::UpdateStatus {
                                        id: id.clone(),
                                        status: "closed".into(),
                                    },
                                ));
                            }
                            return None;
                        }
                        // I hotkey: open issue detail for focused executor
                        'I' if self.focused_node.is_some() => {
                            if let Some(ref exec_id) = self.focused_node {
                                if let Some(node) = lineage.and_then(|l| l.node(exec_id)) {
                                    if let Some(ref iid) = node.issue_id {
                                        self.issue_focus = IssueFocus::Loading { id: iid.clone() };
                                        self.issue_detail_pane.reset();
                                        return Some(Action::Issue(
                                            crate::action::IssueAction::ViewDetail {
                                                id: iid.clone(),
                                            },
                                        ));
                                    } else {
                                        self.activity_log.push(LogEntry {
                                            timestamp: Self::now_stamp(),
                                            prefix: "[tui]".into(),
                                            message: "No issue linked to this executor".into(),
                                            kind: LogEntryKind::Info,
                                        });
                                    }
                                }
                            }
                            return None;
                        }
                        // j/k scroll issue detail body when loaded
                        'j' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                            self.issue_detail_pane.scroll_down();
                            Some(Action::ScrollDown)
                        }
                        'k' if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                            self.issue_detail_pane.scroll_up();
                            Some(Action::ScrollUp)
                        }
                        'j' if self.focused_panel == Panel::Issues => {
                            self.issues_panel.select_next(1, self.tracked_issues.len());
                            Some(Action::SelectNext)
                        }
                        'k' if self.focused_panel == Panel::Issues => {
                            self.issues_panel.select_prev(1, self.tracked_issues.len());
                            Some(Action::SelectPrev)
                        }
                        'j' if self.focused_panel == Panel::Agents => Some(Action::SelectNext),
                        'j' => {
                            if let Some(ref id) = self.focused_node.clone() {
                                let trace = worker_streams.get_mut(&id.0);
                                self.detail_pane.scroll_down(trace);
                            } else {
                                self.activity_log.scroll_down(20);
                            }
                            Some(Action::ScrollDown)
                        }
                        'k' if self.focused_panel == Panel::Agents => Some(Action::SelectPrev),
                        'k' => {
                            if let Some(ref id) = self.focused_node.clone() {
                                let trace = worker_streams.get_mut(&id.0);
                                self.detail_pane.scroll_up(trace);
                            } else {
                                self.activity_log.scroll_up();
                            }
                            Some(Action::ScrollUp)
                        }
                        'W' if self.focused_panel == Panel::Issues
                            || matches!(self.issue_focus, IssueFocus::Loaded { .. }) =>
                        {
                            let id = match &self.issue_focus {
                                IssueFocus::Loaded { id, .. } => Some(id.clone()),
                                _ => self
                                    .issues_panel
                                    .selected_id(&self.tracked_issues)
                                    .map(String::from),
                            };
                            if let Some(id) = id {
                                return Some(Action::Issue(crate::action::IssueAction::WorkOn {
                                    id,
                                }));
                            }
                            return None;
                        }
                        'r' => Some(Action::JumpToReview),
                        'c' if self.focused_panel == Panel::Agents => Some(Action::ToggleCollapse),
                        'g' => {
                            if let Some(ref id) = self.focused_node.clone() {
                                let trace = worker_streams.get_mut(&id.0);
                                self.detail_pane.scroll_to_top(trace);
                            } else {
                                self.activity_log.scroll_to_top();
                            }
                            Some(Action::ScrollToTop)
                        }
                        'G' => {
                            if let Some(ref id) = self.focused_node.clone() {
                                let trace = worker_streams.get_mut(&id.0);
                                self.detail_pane.scroll_to_bottom(trace);
                            } else {
                                self.activity_log.scroll_to_bottom();
                            }
                            Some(Action::ScrollToBottom)
                        }
                        'v' => {
                            self.verbose = !self.verbose;
                            Some(Action::ToggleVerbose)
                        }
                        '?' => Some(Action::ShowHelp),
                        's' => Some(Action::RequestSessions),
                        // Mode-entry keys fall through to InputBar
                        'i' | 'a' | 'A' | 'I' | 'o' | 'O' => None,
                        _ => return None, // Unrecognized: no-op
                    };
                    if let Some(a) = action {
                        return Some(a);
                    }
                }
            }
        }

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
        ) || (key.code == KeyCode::Esc && self.input_bar.wants_esc());

        if is_editing_key {
            // Check if InputBar handles it (Enter on non-empty submits)
            if let HandleOutcome::Submit(text, interrupt) = self.input_bar.handle_key(key) {
                let blocks = vec![spur_acp::ContentBlock::Text(spur_acp::TextContent::new(
                    text,
                ))];
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
                if let ch @ ('a' | 'd' | 'm' | 'R') = ch {
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
            }

            // If InputBar was empty and user typed a navigation char, treat as nav
            if self.input_bar.text().len() == 1 {
                let ch = self.input_bar.text().chars().next().unwrap();
                match ch {
                    'j' if self.focused_panel == Panel::Issues => {
                        self.input_bar.clear();
                        self.issues_panel.select_next(1, self.tracked_issues.len());
                        return Some(Action::SelectNext);
                    }
                    'k' if self.focused_panel == Panel::Issues => {
                        self.input_bar.clear();
                        self.issues_panel.select_prev(1, self.tracked_issues.len());
                        return Some(Action::SelectPrev);
                    }
                    'j' if self.focused_panel == Panel::Agents => {
                        self.input_bar.clear();
                        return Some(Action::SelectNext);
                    }
                    'j' => {
                        self.input_bar.clear();
                        if let Some(ref id) = self.focused_node.clone() {
                            let trace = worker_streams.get_mut(&id.0);
                            self.detail_pane.scroll_down(trace);
                        } else {
                            self.activity_log.scroll_down(20);
                        }
                        return Some(Action::ScrollDown);
                    }
                    'k' if self.focused_panel == Panel::Agents => {
                        self.input_bar.clear();
                        return Some(Action::SelectPrev);
                    }
                    'k' => {
                        self.input_bar.clear();
                        if let Some(ref id) = self.focused_node.clone() {
                            let trace = worker_streams.get_mut(&id.0);
                            self.detail_pane.scroll_up(trace);
                        } else {
                            self.activity_log.scroll_up();
                        }
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
                        if let Some(ref id) = self.focused_node.clone() {
                            let trace = worker_streams.get_mut(&id.0);
                            self.detail_pane.scroll_to_top(trace);
                        } else {
                            self.activity_log.scroll_to_top();
                        }
                        return Some(Action::ScrollToTop);
                    }
                    'G' => {
                        self.input_bar.clear();
                        if let Some(ref id) = self.focused_node.clone() {
                            let trace = worker_streams.get_mut(&id.0);
                            self.detail_pane.scroll_to_bottom(trace);
                        } else {
                            self.activity_log.scroll_to_bottom();
                        }
                        return Some(Action::ScrollToBottom);
                    }
                    'v' => {
                        self.input_bar.clear();
                        self.verbose = !self.verbose;
                        return Some(Action::ToggleVerbose);
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
                if self.focused_panel == Panel::Issues {
                    if let Some(id) = self.issues_panel.selected_id(&self.tracked_issues) {
                        self.issue_focus = IssueFocus::Loading { id: id.to_string() };
                        self.issue_detail_pane.reset();
                        return Some(Action::Issue(crate::action::IssueAction::ViewDetail {
                            id: id.to_string(),
                        }));
                    }
                    return None;
                }
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
                    if matches!(self.issue_focus, IssueFocus::Loaded { .. }) {
                        self.issue_detail_pane.scroll_up();
                    } else if let Some(ref id) = self.focused_node.clone() {
                        let trace = worker_streams.get_mut(&id.0);
                        self.detail_pane.scroll_up(trace);
                    } else {
                        self.activity_log.scroll_up();
                    }
                    return Some(Action::ScrollUp);
                }
                KeyCode::Down => {
                    if matches!(self.issue_focus, IssueFocus::Loaded { .. }) {
                        self.issue_detail_pane.scroll_down();
                    } else if let Some(ref id) = self.focused_node.clone() {
                        let trace = worker_streams.get_mut(&id.0);
                        self.detail_pane.scroll_down(trace);
                    } else {
                        self.activity_log.scroll_down(20);
                    }
                    return Some(Action::ScrollDown);
                }
                KeyCode::Tab if matches!(self.issue_focus, IssueFocus::None) => {
                    self.focused_panel = match self.focused_panel {
                        Panel::Agents => {
                            if !self.tracked_issues.is_empty() {
                                Panel::Issues
                            } else {
                                Panel::Log
                            }
                        }
                        Panel::Issues => Panel::Log,
                        Panel::Log => Panel::Agents,
                    };
                    self.agents_tree
                        .set_focused(self.focused_panel == Panel::Agents);
                    self.issues_panel
                        .set_focused(self.focused_panel == Panel::Issues);
                    self.activity_log
                        .set_focused(self.focused_panel == Panel::Log);
                    return Some(Action::CycleFocus);
                }
                KeyCode::Esc if !matches!(self.issue_focus, IssueFocus::None) => {
                    self.issue_focus = IssueFocus::None;
                    return Some(Action::UnfocusNode);
                }
                KeyCode::Esc if self.focused_node.is_some() => {
                    return Some(Action::UnfocusNode);
                }
                // Esc is the universal "back" key. App decides whether that
                // returns to the active SessionDetail or becomes a no-op.
                KeyCode::Esc => return Some(Action::NavigateBack),
                _ => {}
            }
        }

        None
    }
}

impl View for DashboardView {
    fn handle_key(&mut self, key: KeyEvent, ctx: &super::ViewContext) -> Option<Action> {
        // NOTE: App bypasses this via handle_key_with_worker_streams to supply
        // the per-executor traces. This fallback uses an empty map (safe but
        // won't route scroll to ReactTrace).
        let mut empty_ws = crate::worker_streams::WorkerStreams::new();
        self.handle_key_inner(key, Some(ctx.lineage), &mut empty_ws)
    }

    fn handle_spur_event(&mut self, event: &SpurEvent, _ctx: &super::ViewContext) {
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

            SpurEventBody::AgentNotification {
                session,
                notification,
            } => {
                let prefix = Self::prefix_for_session(&session.0);
                match &notification.update {
                    spur_acp::SessionUpdate::AgentThoughtChunk(chunk)
                    | spur_acp::SessionUpdate::AgentMessageChunk(chunk) => {
                        if let spur_acp::ContentBlock::Text(tc) = &chunk.content {
                            let trimmed = tc.text.trim();
                            if !trimmed.is_empty() {
                                let entry = self
                                    .text_batch
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
                delegation_plan: _,
                issue_id: _,
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
                    DelegationStatus::Failed { error } => {
                        (format!("Delegation failed: {}", error), LogEntryKind::Error)
                    }
                    DelegationStatus::Conflict { files } => (
                        format!("Delegation conflict in {} files", files.len()),
                        LogEntryKind::Error,
                    ),
                    DelegationStatus::Timeout => {
                        ("Delegation timed out".to_string(), LogEntryKind::Error)
                    }
                    DelegationStatus::Rejected { reason } => (
                        format!("Delegation rejected: {}", reason),
                        LogEntryKind::Error,
                    ),
                    DelegationStatus::Modified { reviewer_note } => (
                        format!("Delegation modified: {}", reviewer_note),
                        LogEntryKind::Complete,
                    ),
                    DelegationStatus::TimedOut {
                        waited_for,
                        fallback,
                    } => (
                        format!(
                            "Delegation review timed out after {}s (fallback: {:?})",
                            waited_for.as_secs(),
                            fallback
                        ),
                        LogEntryKind::Error,
                    ),
                    DelegationStatus::Cancelled { reason } => (
                        format!("Delegation cancelled: {}", reason),
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

            SpurEventBody::BrainRetired { session, reason } => {
                // Pair the earlier "Brain agent spawned" entry with an
                // explicit retirement line so the activity log does not
                // show a dangling spawn after `/clear` or session switch.
                let prefix = Self::prefix_for_session(&session.0);
                let reason_label = match reason {
                    spur_acp::domain::events::BrainRetireReason::UserClear => "cleared",
                    spur_acp::domain::events::BrainRetireReason::ResumeSwitch => "switched",
                    spur_acp::domain::events::BrainRetireReason::Shutdown => "shutdown",
                    _ => "retired",
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: format!("Brain session retired ({})", reason_label),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::RateLimitDetected { agent, retry_after } => {
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

            SpurEventBody::IssueUpdated {
                source,
                id,
                status,
                assignee,
            } => {
                if let Some(issue) = self.tracked_issues.iter_mut().find(|i| i.id == *id) {
                    if let Some(ref s) = status {
                        issue.status = s.clone();
                    }
                    if let Some(a) = assignee {
                        issue.assignee = Some(a.clone());
                    }
                }
                if let IssueFocus::Loaded {
                    id: ref focus_id,
                    ref mut issue,
                } = self.issue_focus
                {
                    if focus_id == id {
                        if let Some(ref s) = status {
                            issue.status = s.clone();
                        }
                        if let Some(a) = assignee {
                            issue.assignee = Some(a.clone());
                        }
                    }
                }
                let status_suffix = status
                    .as_ref()
                    .map(|s| format!(": {}", s))
                    .unwrap_or_default();
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[pm]".to_string(),
                    message: format!("Issue {} ({}) updated{}", id, source, status_suffix),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::IssuesLoaded { issues } => {
                self.tracked_issues = issues
                    .iter()
                    .map(|i| spur_pm::IssueSummary {
                        id: i.id.clone(),
                        source: match i.source.as_str() {
                            "github" => spur_pm::PmSource::GitHub,
                            "linear" => spur_pm::PmSource::Linear,
                            "plane" => spur_pm::PmSource::Plane,
                            _ => spur_pm::PmSource::Beads,
                        },
                        title: i.title.clone(),
                        status: i.status.clone(),
                        labels: Vec::new(),
                        url: String::new(),
                        priority: i.priority,
                        issue_type: i.issue_type.clone(),
                        assignee: i.assignee.clone(),
                    })
                    .collect();
                // Sort by priority ascending (critical first)
                self.tracked_issues
                    .sort_by(|a, b| a.priority.unwrap_or(99).cmp(&b.priority.unwrap_or(99)));
                if !self.tracked_issues.is_empty() {
                    self.issues_panel.select_first();
                }
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[pm]".into(),
                    message: format!("{} issues loaded", self.tracked_issues.len()),
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
            SpurEventBody::BrainReconnecting {
                session,
                brain_name,
                reason,
            } => {
                let prefix = Self::prefix_for_session(&session.0);
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: format!("Brain '{}' reconnecting: {}", brain_name, reason),
                    kind: LogEntryKind::Info,
                });
            }
            SpurEventBody::BrainReconnected {
                session,
                brain_name,
                outcome,
            } => {
                let prefix = Self::prefix_for_session(&session.0);
                let (message, kind) = match outcome {
                    spur_acp::LoadOutcome::Restored => (
                        format!(
                            "Brain '{}' reconnected (state restored; your last prompt was dropped — retype)",
                            brain_name
                        ),
                        LogEntryKind::Info,
                    ),
                    spur_acp::LoadOutcome::FellBackToNew { reason } => (
                        format!(
                            "Brain '{}' reconnected — started FRESH ({}); retype to continue",
                            brain_name, reason
                        ),
                        LogEntryKind::Error,
                    ),
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message,
                    kind,
                });
            }
            SpurEventBody::BrainReconnectFailed {
                session,
                brain_name,
                reason,
            } => {
                let prefix = Self::prefix_for_session(&session.0);
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: format!("Brain '{}' reconnect FAILED: {}", brain_name, reason),
                    kind: LogEntryKind::Error,
                });
            }

            SpurEventBody::WorkerProgress {
                executor_id,
                name,
                pct,
                ..
            } => {
                let msg = match pct {
                    Some(p) => format!("{} ({}%)", name, p),
                    None => name.clone(),
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: Self::prefix_for_session(executor_id),
                    message: msg,
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::WorkerFileTouched {
                executor_id,
                path,
                kind,
                ..
            } => {
                let verb = match kind {
                    spur_acp::FileTouchKind::Read => "read",
                    spur_acp::FileTouchKind::Write => "wrote",
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: Self::prefix_for_session(executor_id),
                    message: format!("{} {}", verb, path.display()),
                    kind: LogEntryKind::Act,
                });
            }

            SpurEventBody::IssueDetailFetched {
                requested_id,
                issue,
            } => {
                if let IssueFocus::Loading { id } = &self.issue_focus {
                    if id == requested_id {
                        let pm_issue = detail_event_to_issue(issue);
                        self.issue_focus = IssueFocus::Loaded {
                            id: requested_id.clone(),
                            issue: Box::new(pm_issue),
                        };
                    }
                }
            }

            SpurEventBody::IssueCommandError { operation, error } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[pm]".into(),
                    message: format!("{} failed: {}", operation, error),
                    kind: LogEntryKind::Error,
                });
                if matches!(self.issue_focus, IssueFocus::Loading { .. }) {
                    self.issue_focus = IssueFocus::None;
                }
            }

            SpurEventBody::GraphAlertsSummary {
                total,
                critical,
                warning,
                details,
            } => {
                self.alert_summary = Some((*total, *critical, *warning));
                for msg in details.iter().take(5) {
                    self.activity_log.push(LogEntry {
                        timestamp: Self::now_stamp(),
                        prefix: "[graph]".into(),
                        message: msg.clone(),
                        kind: if *critical > 0 {
                            LogEntryKind::Error
                        } else {
                            LogEntryKind::Info
                        },
                    });
                }
            }

            SpurEventBody::PlanTaskReviewed {
                plan_id: _,
                task_id,
                task_name,
                decision,
                feedback,
                attempt,
                max_attempts,
            } => {
                let (icon, label, kind) = match decision.as_str() {
                    "approve" => ("✓", "approved", LogEntryKind::Complete),
                    "reject" => ("✗", "rejected", LogEntryKind::Error),
                    "request_changes" => ("↻", "requested changes on", LogEntryKind::Act),
                    _ => ("?", "reviewed", LogEntryKind::Info),
                };
                let display = task_name
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(task_id);
                let attempts_suffix = if *max_attempts > 0 {
                    format!(" (attempt {attempt}/{max_attempts})")
                } else {
                    format!(" (attempt {attempt})")
                };
                let fb_suffix = feedback
                    .as_ref()
                    .map(|f| format!(": \"{}\"", truncate_display(f, 60)))
                    .unwrap_or_default();
                // Distinct entry when attempt budget is exhausted by a reject.
                let exhausted =
                    decision == "reject" && *max_attempts > 0 && *attempt >= *max_attempts;
                let message = if exhausted {
                    format!(
                        "✗ Task \"{display}\" failed — max attempts ({max_attempts}) reached{fb_suffix}"
                    )
                } else {
                    format!("{icon} Brain {label} \"{display}\"{attempts_suffix}{fb_suffix}")
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[plan]".to_string(),
                    message,
                    kind,
                });
            }
            SpurEventBody::PlanTaskIterating {
                plan_id: _,
                task_id,
                task_name,
                attempt,
                max_attempts,
                delegation_id: _,
            } => {
                let display = task_name
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(task_id);
                let attempts_suffix = if *max_attempts > 0 {
                    format!("{attempt}/{max_attempts}")
                } else {
                    format!("{attempt}")
                };
                let final_hint = if *max_attempts > 0 && *attempt >= *max_attempts {
                    " — final"
                } else {
                    ""
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[plan]".to_string(),
                    message: format!(
                        "↻ Re-dispatched \"{display}\" (attempt {attempts_suffix}{final_hint})"
                    ),
                    kind: LogEntryKind::Act,
                });
            }

            _ => {}
        }
    }

    fn tick(&mut self) {
        self.tick_and_report_flush();
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &super::ViewContext) {
        // NOTE: App::render bypasses this method and calls render_with_lineage
        // directly so it can pass worker_streams. This fallback exists only to
        // satisfy the View trait (e.g., in tests that don't need stream traces).
        let mut empty_ws = crate::worker_streams::WorkerStreams::new();
        self.render_with_lineage(frame, area, ctx.lineage, ctx.license_badge, &mut empty_ws);
    }
}

impl DashboardView {
    /// Handle a key event with access to per-executor `ReactTrace` instances.
    /// App calls this directly instead of `View::handle_key` so that scroll
    /// actions on the Stream tab are routed to the focused executor's trace.
    pub fn handle_key_with_worker_streams(
        &mut self,
        key: KeyEvent,
        lineage: &spur_core::lineage::projection::ExecutorLineage,
        worker_streams: &mut crate::worker_streams::WorkerStreams,
    ) -> Option<crate::action::Action> {
        self.handle_key_inner(key, Some(lineage), worker_streams)
    }

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
