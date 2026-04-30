use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use spur_acp::SpurEvent;

use crate::action::{Action, IssueAction, ViewId};
use crate::components::issue_detail_pane::IssueDetailPane;
use crate::components::issues_panel::IssuesPanel;
use crate::components::status_bar::{HintOverride, StatusBar, StatusBarProps};
use crate::components::tombstone::Tombstone;

use super::View;

// ── Issue focus state machine ───────────────────────────────────────────

#[derive(Debug, Clone)]
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

// ── Helpers ─────────────────────────────────────────────────────────────

/// Convert spur_acp mirror type back to spur_pm::Issue for TUI rendering.
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
        status: e.status.clone(),
        priority: e.priority,
        issue_type: e.issue_type.clone(),
        assignee: e.assignee.clone(),
        due_at: e.due_at,
        blocked_by: e.blocked_by.clone(),
        labels: e.labels.clone(),
        url: e.url.clone(),
        body: e.body.clone(),
        created_at: e.created_at,
        updated_at: e.updated_at,
    }
}

// ── View ────────────────────────────────────────────────────────────────

pub struct IssueBrowserView {
    tracked_issues: Vec<spur_pm::IssueSummary>,
    issues_panel: IssuesPanel,
    issue_detail_pane: IssueDetailPane,
    issue_focus: IssueFocus,
}

impl Default for IssueBrowserView {
    fn default() -> Self {
        Self::new()
    }
}

impl IssueBrowserView {
    pub fn new() -> Self {
        Self {
            tracked_issues: Vec::new(),
            issues_panel: IssuesPanel::new(),
            issue_detail_pane: IssueDetailPane::new(),
            issue_focus: IssueFocus::None,
        }
    }

    pub fn tracked_issues(&self) -> &[spur_pm::IssueSummary] {
        &self.tracked_issues
    }

    pub fn seed_issues(&mut self, issues: Vec<spur_pm::IssueSummary>) {
        self.tracked_issues = issues;
        if !self.tracked_issues.is_empty() {
            self.issues_panel.select_first();
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_issues_for_test(&mut self, issues: Vec<spur_pm::IssueSummary>) {
        self.seed_issues(issues);
    }

    pub fn issue_detail_visible(&self) -> bool {
        matches!(self.issue_focus, IssueFocus::Loaded { .. })
    }

    pub fn scroll_issue_detail_up_by(&mut self, lines: u16) {
        self.issue_detail_pane.scroll_up_by(lines);
    }

    pub fn scroll_issue_detail_down_by(&mut self, lines: u16) {
        self.issue_detail_pane.scroll_down_by(lines);
    }

    pub fn issues_panel_mut(&mut self) -> &mut IssuesPanel {
        &mut self.issues_panel
    }

    // ── Key handling ────────────────────────────────────────────────────

    fn handle_key_inner(&mut self, key: KeyEvent) -> Option<Action> {
        let key = super::normalize_macos_option(key);

        match key.code {
            KeyCode::Esc => {
                if matches!(self.issue_focus, IssueFocus::Loaded { .. }) {
                    self.issue_focus = IssueFocus::None;
                    None
                } else {
                    Some(Action::NavigateTo(ViewId::Dashboard))
                }
            }
            KeyCode::Char('q') if key.modifiers.is_empty() => Some(Action::Quit),
            KeyCode::Char('?') if key.modifiers.is_empty() => Some(Action::ShowHelp),
            KeyCode::Char('s') if key.modifiers.is_empty() => Some(Action::RequestSessions),
            KeyCode::Char('r') if key.modifiers.is_empty() => Some(Action::RefreshIssues),

            // Navigation
            KeyCode::Char('j') | KeyCode::Down if key.modifiers.is_empty() => {
                self.issues_panel.select_next(1, self.tracked_issues.len());
                Some(Action::SelectNextBy(1))
            }
            KeyCode::Char('k') | KeyCode::Up if key.modifiers.is_empty() => {
                self.issues_panel.select_prev(1, self.tracked_issues.len());
                Some(Action::SelectPrevBy(1))
            }
            KeyCode::Char('g') if key.modifiers.is_empty() => {
                self.issues_panel.select_first();
                None
            }
            KeyCode::Char('G') if key.modifiers.is_empty() => {
                let count = self.tracked_issues.len();
                self.issues_panel.select_last(count);
                None
            }

            // Enter — fetch detail for the selected row, or close if already
            // viewing that same row's detail. If the list is empty, fall back
            // to closing any open detail so the pane never lingers without a
            // corresponding row.
            KeyCode::Enter if key.modifiers.is_empty() => {
                let selected = self
                    .issues_panel
                    .selected_id(&self.tracked_issues)
                    .map(String::from);
                match (&self.issue_focus, selected) {
                    (IssueFocus::Loaded { id: loaded_id, issue: _ }, Some(sel))
                        if loaded_id == &sel =>
                    {
                        self.issue_focus = IssueFocus::None;
                        None
                    }
                    (_, Some(sel)) => {
                        self.issue_focus = IssueFocus::Loading { id: sel.clone() };
                        self.issue_detail_pane.reset();
                        Some(Action::Issue(IssueAction::ViewDetail { id: sel }))
                    }
                    (IssueFocus::Loaded { .. }, None) => {
                        self.issue_focus = IssueFocus::None;
                        None
                    }
                    (_, None) => None,
                }
            }

            // Status actions
            KeyCode::Char('o') if key.modifiers.is_empty() => self.update_status("open", false),
            KeyCode::Char('w') if key.modifiers.is_empty() => {
                self.update_status("in_progress", false)
            }
            KeyCode::Char('b') if key.modifiers.is_empty() => self.update_status("blocked", false),
            KeyCode::Char('x') if key.modifiers.is_empty() => self.update_status("closed", false),
            KeyCode::Char('d') if key.modifiers.is_empty() => self.update_status("closed", true),
            KeyCode::Char('W') if key.modifiers.is_empty() => {
                let id = match &self.issue_focus {
                    IssueFocus::Loaded { id, .. } => Some(id.clone()),
                    _ => self
                        .issues_panel
                        .selected_id(&self.tracked_issues)
                        .map(String::from),
                };
                id.map(|id| Action::Issue(IssueAction::WorkOn { id }))
            }

            // Detail scroll (when loaded)
            KeyCode::PageUp if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                self.issue_detail_pane.scroll_up_by(10);
                Some(Action::ScrollUp)
            }
            KeyCode::PageDown if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                self.issue_detail_pane.scroll_down_by(10);
                Some(Action::ScrollDown)
            }

            _ => None,
        }
    }

    fn update_status(&self, status: &str, via_legacy_key: bool) -> Option<Action> {
        let id = match &self.issue_focus {
            IssueFocus::Loaded { id, .. } => Some(id.clone()),
            _ => self
                .issues_panel
                .selected_id(&self.tracked_issues)
                .map(String::from),
        };
        id.map(|id| {
            Action::Issue(IssueAction::UpdateStatus {
                id,
                status: status.into(),
                via_legacy_key,
            })
        })
    }

    // ── Render ──────────────────────────────────────────────────────────

    fn render_inner(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        tombstone: Option<&Tombstone>,
        view_hint_override: Option<HintOverride<'_>>,
    ) {
        let issue_count = self.tracked_issues.len();

        let issues_height = if issue_count == 0 {
            3u16 // placeholder height
        } else {
            IssuesPanel::computed_height(issue_count, area.height)
                .max(4)
                .min(area.height * 40 / 100)
        };

        let has_detail = matches!(self.issue_focus, IssueFocus::Loaded { .. });
        let detail_min = if has_detail { 8 } else { 3 };

        let constraints = vec![
            Constraint::Length(issues_height),
            Constraint::Min(detail_min),
            Constraint::Length(1), // status bar
        ];
        let chunks = Layout::vertical(constraints).split(area);

        // ── Issues panel ──────────────────────────────────────────────────
        if issue_count == 0 {
            let block = Block::default()
                .title(" Issues ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray));
            let inner = block.inner(chunks[0]);
            frame.render_widget(block, chunks[0]);
            let msg = Paragraph::new("No issues loaded. Press 'r' to refresh.")
                .style(Style::default().fg(Color::DarkGray));
            frame.render_widget(msg, inner);
        } else {
            self.issues_panel
                .render(&self.tracked_issues, frame, chunks[0]);
        }

        // ── Detail or placeholder ────────────────────────────────────────
        match &self.issue_focus {
            IssueFocus::Loading { id } => {
                IssueDetailPane::render_loading(id, frame, chunks[1]);
            }
            IssueFocus::Loaded { issue, .. } => {
                self.issue_detail_pane.render(issue, frame, chunks[1]);
            }
            IssueFocus::None => {
                let block = Block::default()
                    .title(" Issue Detail ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray));
                let inner = block.inner(chunks[1]);
                frame.render_widget(block, chunks[1]);
                let hint = if issue_count == 0 {
                    "No issue selected"
                } else {
                    "Press Enter to view issue detail"
                };
                let msg = Paragraph::new(hint)
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(ratatui::layout::Alignment::Center);
                frame.render_widget(msg, inner);
            }
        }

        // ── Status bar ────────────────────────────────────────────────────
        StatusBar::render(
            frame,
            chunks[2],
            StatusBarProps {
                view: &ViewId::IssueBrowser,
                tombstone,
                running: 0,
                pending_review: 0,
                total_cost: 0.0,
                elapsed: "",
                current_mode: None,
                current_model_label: None,
                current_effort_label: None,
                usage_supported: false,
                context_used: None,
                context_size: None,
                stream_in_flight: false,
                esc_consumed_by_composer: false,
                issue_count,
                alert_summary: None,
                license_badge: None,
                flag_summary: None,
                view_hint_override,
            },
        );
    }
}

impl View for IssueBrowserView {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &super::ViewContext) -> Option<Action> {
        self.handle_key_inner(key)
    }

    fn handle_spur_event(&mut self, event: &SpurEvent, _ctx: &super::ViewContext) {
        match &event.body {
            spur_acp::SpurEventBody::IssuesLoaded { issues } => {
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
                // Reset selection if it would now be out of bounds
                if !self.tracked_issues.is_empty() {
                    let idx = self
                        .issues_panel
                        .selected_id(&self.tracked_issues)
                        .and_then(|id| self.tracked_issues.iter().position(|i| i.id == id))
                        .unwrap_or(0);
                    self.issues_panel.select_first(); // will be overridden by select_next below
                    for _ in 0..idx {
                        self.issues_panel.select_next(1, self.tracked_issues.len());
                    }
                }
            }

            spur_acp::SpurEventBody::IssueUpdated {
                id,
                status,
                assignee,
                ..
            } => {
                if let Some(issue) = self.tracked_issues.iter_mut().find(|i| i.id == *id) {
                    if let Some(s) = status {
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
                    if *focus_id == *id {
                        if let Some(s) = status {
                            issue.status = s.clone();
                        }
                        if let Some(a) = assignee {
                            issue.assignee = Some(a.clone());
                        }
                    }
                }
            }

            spur_acp::SpurEventBody::IssueDetailFetched {
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

            spur_acp::SpurEventBody::IssueCommandError { .. } => {
                if matches!(self.issue_focus, IssueFocus::Loading { .. }) {
                    self.issue_focus = IssueFocus::None;
                }
            }

            _ => {}
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &super::ViewContext) {
        self.render_inner(frame, area, ctx.tombstone, ctx.transient_hint_override);
    }

    fn tick(&mut self) {
        // No animation state yet
    }
}
