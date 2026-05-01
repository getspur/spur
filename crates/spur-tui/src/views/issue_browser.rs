use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use spur_acp::{GraphEdgeEvent, GraphNodeEvent, SpurEvent};

use crate::action::{Action, IssueAction, ViewId};
use crate::components::execute_modal::{ExecuteModal, ExecuteModalVariant};
use crate::components::issue_detail_pane::IssueDetailPane;
use crate::components::issue_graph_pane::IssueGraphPane;
use crate::components::issues_panel::IssuesPanel;
use crate::components::status_bar::{HintOverride, StatusBar, StatusBarProps};
use crate::components::tombstone::Tombstone;

use super::View;

const TEXT_STATUS_HINT: &str =
    "[Text] j/k: Nav  v: Graph Mode  PgUp/PgDn: Scroll  Esc: Close Detail  q: Quit";
const TEXT_STATUS_HINT_COMPACT: &str = "[Text] j/k: Nav  v: Graph  Esc: Close";
const TEXT_STATUS_HINT_EPIC: &str =
    "[Text] j/k: Nav  v: Graph Mode  E: Execute Epic  PgUp/PgDn: Scroll  Esc: Close Detail  q: Quit";
const TEXT_STATUS_HINT_EPIC_COMPACT: &str = "[Text] j/k: Nav  E: Execute Epic  v: Graph";
const GRAPH_STATUS_HINT: &str =
    "[Graph] j/k: Nav  v: Text Mode  PgUp/PgDn: Scroll  Esc: Close Graph  q: Quit";
const GRAPH_STATUS_HINT_COMPACT: &str = "[Graph] j/k: Nav  v: Text  Esc: Close";
const GRAPH_STATUS_HINT_EPIC: &str =
    "[Graph] j/k: Nav  v: Text Mode  E: Execute Epic  PgUp/PgDn: Scroll  Esc: Close Graph  q: Quit";
const GRAPH_STATUS_HINT_EPIC_COMPACT: &str = "[Graph] j/k: Nav  E: Execute Epic  v: Text";
const LIST_STATUS_HINT: &str =
    "[List] j/k: Nav  Enter: Open Detail  v: View Graph  W: Work  r: Refresh  q: Quit";
const LIST_STATUS_HINT_COMPACT: &str = "[List] j/k: Nav  Enter: Detail  W: Work  r: Refresh";
const LIST_STATUS_HINT_EPIC: &str =
    "[List] j/k: Nav  Enter: Open Detail  v: View Graph  E: Execute Epic  W: Work  r: Refresh  q: Quit";
const LIST_STATUS_HINT_EPIC_COMPACT: &str =
    "[List] j/k: Nav  Enter: Detail  E: Execute Epic  W: Work";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailMode {
    Text,
    Graph,
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
    detail_mode: DetailMode,
    graph_pane: IssueGraphPane,
    graph_cache: HashMap<String, (Vec<GraphNodeEvent>, Vec<GraphEdgeEvent>)>,
    graph_loading: Option<String>,
    graph_error: Option<String>,
    execute_modal: Option<ExecuteModal>,
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
            detail_mode: DetailMode::Text,
            graph_pane: IssueGraphPane::new(),
            graph_cache: HashMap::new(),
            graph_loading: None,
            graph_error: None,
            execute_modal: None,
        }
    }

    pub fn tracked_issues(&self) -> &[spur_pm::IssueSummary] {
        &self.tracked_issues
    }

    pub fn seed_issues(&mut self, issues: Vec<spur_pm::IssueSummary>) {
        self.invalidate_graph_cache();
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
        match self.detail_mode {
            DetailMode::Text => self.issue_detail_pane.scroll_up_by(lines),
            DetailMode::Graph => self.graph_pane.scroll_up_by(lines),
        }
    }

    pub fn scroll_issue_detail_down_by(&mut self, lines: u16) {
        match self.detail_mode {
            DetailMode::Text => self.issue_detail_pane.scroll_down_by(lines),
            DetailMode::Graph => self.graph_pane.scroll_down_by(lines),
        }
    }

    pub fn issues_panel_mut(&mut self) -> &mut IssuesPanel {
        &mut self.issues_panel
    }

    fn selected_issue_id(&self) -> Option<String> {
        self.issues_panel
            .selected_id(&self.tracked_issues)
            .map(String::from)
    }

    fn selected_issue(&self) -> Option<&spur_pm::IssueSummary> {
        let selected_id = self.issues_panel.selected_id(&self.tracked_issues)?;
        self.tracked_issues
            .iter()
            .find(|issue| issue.id == selected_id)
    }

    fn selected_issue_is_epic(&self) -> bool {
        self.selected_issue()
            .is_some_and(|issue| issue.issue_type.as_deref() == Some("epic"))
    }

    fn hint_override(full: &'static str, compact: &'static str) -> HintOverride<'static> {
        HintOverride {
            full,
            compact: Some(compact),
            hide_on_overflow: false,
        }
    }

    pub(crate) fn invalidate_graph_cache(&mut self) {
        self.graph_cache.clear();
        self.graph_loading = None;
        self.graph_error = None;
    }

    fn request_selected_detail(&mut self) -> Option<Action> {
        let selected = self.selected_issue_id();
        match (&self.issue_focus, selected) {
            (
                IssueFocus::Loaded {
                    id: loaded_id,
                    issue: _,
                },
                Some(sel),
            ) if loaded_id == &sel => {
                self.issue_focus = IssueFocus::None;
                self.detail_mode = DetailMode::Text;
                None
            }
            (_, Some(sel)) => {
                self.issue_focus = IssueFocus::Loading { id: sel.clone() };
                self.detail_mode = DetailMode::Text;
                self.issue_detail_pane.reset();
                self.graph_error = None;
                Some(Action::Issue(IssueAction::ViewDetail { id: sel }))
            }
            (IssueFocus::Loaded { .. }, None) => {
                self.issue_focus = IssueFocus::None;
                self.detail_mode = DetailMode::Text;
                None
            }
            (_, None) => None,
        }
    }

    fn toggle_detail_mode(&mut self) -> Option<Action> {
        match &self.issue_focus {
            IssueFocus::Loaded { id, .. } => match self.detail_mode {
                DetailMode::Text => {
                    self.detail_mode = DetailMode::Graph;
                    self.graph_pane.reset();
                    self.graph_error = None;
                    if self.graph_cache.contains_key(id) {
                        self.graph_loading = None;
                        None
                    } else if self.graph_loading.as_deref() == Some(id.as_str()) {
                        None
                    } else {
                        let id = id.clone();
                        self.graph_loading = Some(id.clone());
                        Some(Action::GetIssueGraph { id })
                    }
                }
                DetailMode::Graph => {
                    self.detail_mode = DetailMode::Text;
                    None
                }
            },
            IssueFocus::None => self.request_selected_detail(),
            IssueFocus::Loading { .. } => None,
        }
    }

    fn open_execute_modal(&mut self) -> Option<Action> {
        let issue = self.selected_issue()?;
        if issue.issue_type.as_deref() != Some("epic") {
            return Some(Action::FlashHint {
                message: format!(
                    "Cannot execute: {} is not an epic. Use 'W' to WorkOn a task.",
                    issue.id
                ),
            });
        }

        self.execute_modal = Some(ExecuteModal {
            epic_id: issue.id.clone(),
            epic_title: issue.title.clone(),
            variant: ExecuteModalVariant::Confirm,
        });
        None
    }

    // ── Key handling ────────────────────────────────────────────────────

    fn handle_key_inner(&mut self, key: KeyEvent) -> Option<Action> {
        let key = super::normalize_macos_option(key);

        if let Some(modal) = self.execute_modal.as_ref() {
            return match key.code {
                KeyCode::Enter => {
                    let id = modal.epic_id.clone();
                    self.execute_modal = None;
                    Some(Action::Issue(IssueAction::ExecuteEpic { id }))
                }
                KeyCode::Esc => {
                    self.execute_modal = None;
                    None
                }
                _ => None,
            };
        }

        match key.code {
            KeyCode::Esc => {
                if matches!(self.issue_focus, IssueFocus::Loaded { .. }) {
                    self.issue_focus = IssueFocus::None;
                    self.detail_mode = DetailMode::Text;
                    None
                } else {
                    Some(Action::NavigateTo(ViewId::Dashboard))
                }
            }
            KeyCode::Char('q') if key.modifiers.is_empty() => Some(Action::Quit),
            KeyCode::Char('?') if key.modifiers.is_empty() => Some(Action::ShowHelp),
            KeyCode::Char('s') if key.modifiers.is_empty() => Some(Action::RequestSessions),
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                self.invalidate_graph_cache();
                Some(Action::RefreshIssues)
            }
            KeyCode::Char('v') if key.modifiers.is_empty() => self.toggle_detail_mode(),

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
            KeyCode::Enter if key.modifiers.is_empty() => self.request_selected_detail(),

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
            KeyCode::Char('E') if key.modifiers.is_empty() => self.open_execute_modal(),

            // Detail scroll (when loaded)
            KeyCode::PageUp if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                self.scroll_issue_detail_up_by(10);
                Some(Action::ScrollUp)
            }
            KeyCode::PageDown if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                self.scroll_issue_detail_down_by(10);
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
            IssueFocus::Loaded { id, issue } => match self.detail_mode {
                DetailMode::Text => {
                    self.issue_detail_pane.render(issue, frame, chunks[1]);
                }
                DetailMode::Graph => {
                    if let Some(error) = self.graph_error.as_deref() {
                        IssueGraphPane::render_error(id, error, frame, chunks[1]);
                    } else if let Some((nodes, edges)) = self.graph_cache.get(id) {
                        self.graph_pane.render(id, nodes, edges, frame, chunks[1]);
                    } else if self.graph_loading.as_deref() == Some(id.as_str()) {
                        IssueGraphPane::render_loading(id, frame, chunks[1]);
                    } else {
                        IssueGraphPane::render_error(
                            id,
                            "Graph not loaded; switch to Text then Graph to reload",
                            frame,
                            chunks[1],
                        );
                    }
                }
            },
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
        let has_execute = self.selected_issue_is_epic();
        let mode_hint = match self.issue_focus {
            IssueFocus::Loaded { .. } | IssueFocus::Loading { .. } => {
                Some(match self.detail_mode {
                    DetailMode::Text if has_execute => {
                        Self::hint_override(TEXT_STATUS_HINT_EPIC, TEXT_STATUS_HINT_EPIC_COMPACT)
                    }
                    DetailMode::Text => {
                        Self::hint_override(TEXT_STATUS_HINT, TEXT_STATUS_HINT_COMPACT)
                    }
                    DetailMode::Graph if has_execute => {
                        Self::hint_override(GRAPH_STATUS_HINT_EPIC, GRAPH_STATUS_HINT_EPIC_COMPACT)
                    }
                    DetailMode::Graph => {
                        Self::hint_override(GRAPH_STATUS_HINT, GRAPH_STATUS_HINT_COMPACT)
                    }
                })
            }
            IssueFocus::None if issue_count > 0 && has_execute => Some(Self::hint_override(
                LIST_STATUS_HINT_EPIC,
                LIST_STATUS_HINT_EPIC_COMPACT,
            )),
            IssueFocus::None if issue_count > 0 => Some(Self::hint_override(
                LIST_STATUS_HINT,
                LIST_STATUS_HINT_COMPACT,
            )),
            IssueFocus::None => None,
        };
        let status_hint = view_hint_override.or(mode_hint);

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
                view_hint_override: status_hint,
            },
        );

        if let Some(modal) = self.execute_modal.as_ref() {
            modal.render(frame, chunks[1]);
        }
    }
}

impl View for IssueBrowserView {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &super::ViewContext) -> Option<Action> {
        self.handle_key_inner(key)
    }

    fn handle_spur_event(&mut self, event: &SpurEvent, _ctx: &super::ViewContext) {
        match &event.body {
            spur_acp::SpurEventBody::IssuesLoaded { issues } => {
                self.invalidate_graph_cache();
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
                        self.detail_mode = DetailMode::Text;
                    }
                }
            }

            spur_acp::SpurEventBody::IssueSubgraphLoaded {
                requested_id,
                nodes,
                edges,
            } => {
                let matches_loading = self.graph_loading.as_deref() == Some(requested_id.as_str());
                let matches_selected = self
                    .issues_panel
                    .selected_id(&self.tracked_issues)
                    .is_some_and(|id| id == requested_id.as_str());
                if !matches_loading && !matches_selected {
                    return;
                }

                self.graph_cache
                    .insert(requested_id.clone(), (nodes.clone(), edges.clone()));
                self.graph_loading = None;
                self.graph_error = None;
            }

            spur_acp::SpurEventBody::IssueCommandError { error, .. } => {
                if self.graph_loading.is_some() {
                    self.graph_error = Some(error.clone());
                    self.graph_loading = None;
                } else if matches!(self.issue_focus, IssueFocus::Loading { .. }) {
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
