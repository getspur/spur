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
use crate::components::issues_panel::{IssueLineageContext, IssueLineageView, IssuesPanel};
use crate::components::status_bar::{HintOverride, StatusBar, StatusBarProps};
use crate::components::tombstone::Tombstone;

use super::View;

const TEXT_STATUS_HINT: &str =
    "[Text] j/k: Nav  v: Graph Mode  PgUp/PgDn: Scroll  Esc: Close Detail  q: Quit";
const TEXT_STATUS_HINT_COMPACT: &str = "[Text] j/k: Nav  v: Graph  Esc: Close";
const TEXT_STATUS_HINT_EPIC: &str =
    "[Text] j/k: Nav  v: Graph Mode  E: Execute Epic  PgUp/PgDn: Scroll  Esc: Close Detail  q: Quit";
const TEXT_STATUS_HINT_EPIC_COMPACT: &str = "[Text] j/k: Nav  E: Execute Epic  v: Graph";
const TEXT_STATUS_HINT_PLAN_EPIC: &str =
    "[Text] j/k: Nav  p: Open Plan  v: Graph Mode  PgUp/PgDn: Scroll  Esc: Close Detail  q: Quit";
const TEXT_STATUS_HINT_PLAN_EPIC_COMPACT: &str = "[Text] j/k: Nav  p: Plan  v: Graph";
const GRAPH_STATUS_HINT: &str =
    "[Graph] j/k: Nav  v: Text Mode  PgUp/PgDn: Scroll  Esc: Close Graph  q: Quit";
const GRAPH_STATUS_HINT_COMPACT: &str = "[Graph] j/k: Nav  v: Text  Esc: Close";
const GRAPH_STATUS_HINT_EPIC: &str =
    "[Graph] j/k: Nav  v: Text Mode  E: Execute Epic  PgUp/PgDn: Scroll  Esc: Close Graph  q: Quit";
const GRAPH_STATUS_HINT_EPIC_COMPACT: &str = "[Graph] j/k: Nav  E: Execute Epic  v: Text";
const GRAPH_STATUS_HINT_PLAN_EPIC: &str =
    "[Graph] j/k: Nav  p: Open Plan  v: Text Mode  PgUp/PgDn: Scroll  Esc: Close Graph  q: Quit";
const GRAPH_STATUS_HINT_PLAN_EPIC_COMPACT: &str = "[Graph] j/k: Nav  p: Plan  v: Text";
const LIST_STATUS_HINT: &str =
    "[List] j/k: Nav  Enter/o: Open Detail  v: View Graph  W: Work  r: Refresh  q: Quit";
const LIST_STATUS_HINT_COMPACT: &str = "[List] j/k: Nav  o: Open  W: Work  r: Refresh";
const LIST_STATUS_HINT_EPIC: &str =
    "[List] j/k: Nav  Enter/o: Open Detail  v: View Graph  E: Execute Epic  W: Work  r: Refresh  q: Quit";
const LIST_STATUS_HINT_EPIC_COMPACT: &str = "[List] j/k: Nav  o: Open  E: Execute Epic  W: Work";
const LIST_STATUS_HINT_PLAN_EPIC: &str =
    "[List] j/k: Nav  Enter/o: Open Detail  v: Graph  p: Open Plan  W: Work  r: Refresh  q: Quit";
const LIST_STATUS_HINT_PLAN_EPIC_COMPACT: &str = "[List] j/k: Nav  o: Open  p: Plan  W: Work";

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

/// Inc 3 (bd-d587.3): caller-supplied intent for `open_external_detail`.
/// `Default`/`FocusText` open the detail in Text mode (palette-style entry,
/// no graph). `FocusGraph` arms the post-load mode flip so that once the
/// detail + graph have arrived, the right pane shows Graph view.
///
/// For plan-backed epics, the PM layer resolves this to the plan label scope
/// (`spur:plan-id:<id>`) instead of treating the epic as an ordinary
/// `--graph-root`. That keeps the UI low-friction: users open the epic, and
/// the backend chooses the correct issue-graph projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenMode {
    #[default]
    Default,
    FocusText,
    FocusGraph,
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
    /// Inc 3 (bd-d587.3): id to select in the left list once it appears in
    /// `tracked_issues`. Set by `open_external_detail` when the id isn't yet
    /// present; drained on the next `IssuesLoaded` that contains it.
    pending_select: Option<String>,
    /// Inc 3 (bd-d587.3): mode to apply when `IssueDetailFetched` transitions
    /// `Loading -> Loaded`. Replaces the hardcoded `DetailMode::Text` reset
    /// so the View-Epic intent (`OpenMode::FocusGraph`) survives the fetch.
    post_load_mode: Option<DetailMode>,
    /// Inc 3 (bd-d587.3): action to drain after `handle_spur_event`. Used
    /// when `open_external_detail(_, FocusGraph)` needs to fire
    /// `Action::GetIssueGraph` once the detail has loaded — the view itself
    /// can't dispatch actions, only stash them for the app to pick up via
    /// `take_pending_action`.
    pending_action: Option<Action>,
    /// Error from the most recent `list_issues` failure surfaced via
    /// `IssueCommandError` (e.g. corrupt `.beads/issues.jsonl`). Rendered in
    /// the empty-list pane so the user sees the cause instead of a misleading
    /// "No issues loaded" placeholder. Cleared on the next `IssuesLoaded`.
    last_refresh_error: Option<String>,
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
            pending_select: None,
            post_load_mode: None,
            pending_action: None,
            last_refresh_error: None,
        }
    }

    /// Inc 3 (bd-d587.3): drain the pending follow-up action stashed by
    /// `open_external_detail` / `IssueDetailFetched`. The app polls this
    /// after dispatching events to the view so it can route the action
    /// through `process_action` (the only way to actually execute it).
    pub fn take_pending_action(&mut self) -> Option<Action> {
        self.pending_action.take()
    }

    /// True when `open_external_detail` armed `pending_select` because the
    /// requested id wasn't yet in `tracked_issues`. The app uses this to
    /// fire `RefreshIssues` so the queued id actually lands in the list and
    /// the row gets selected — otherwise the right-pane detail and the
    /// left-pane selection would stay out of sync.
    pub fn has_pending_select(&self) -> bool {
        self.pending_select.is_some()
    }

    /// Inc 3 (bd-d587.3) follow-up: drain all armed `open_external_detail`
    /// state so that a subsequent fresh detail open or error tear-down does
    /// not inherit stale `post_load_mode` / `pending_select` / `pending_action`
    /// / `graph_loading` from a previous (possibly errored) external open.
    fn reset_armed_state(&mut self) {
        self.pending_select = None;
        self.post_load_mode = None;
        self.pending_action = None;
        self.graph_loading = None;
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

    fn prefetch_selected_graph(&mut self) {
        if self.tracked_issues.len() < 2 {
            return;
        }
        let Some(id) = self.selected_issue_id() else {
            return;
        };
        if self.graph_cache.contains_key(&id) || self.graph_loading.as_deref() == Some(id.as_str())
        {
            return;
        }

        self.graph_error = None;
        self.graph_loading = Some(id.clone());
        self.pending_action = Some(Action::GetIssueGraph { id });
    }

    fn lineage_loading_root_id(&self, selected_id: &str) -> String {
        self.cached_lineage_parent_id(selected_id)
            .or_else(|| self.prefix_lineage_parent_id(selected_id))
            .unwrap_or(selected_id)
            .to_string()
    }

    fn cached_lineage_parent_id<'a>(&'a self, selected_id: &str) -> Option<&'a str> {
        self.graph_cache
            .values()
            .flat_map(|(_, edges)| edges)
            .find(|edge| {
                edge.from == selected_id && Self::is_lineage_parent_edge(edge.edge_type.as_deref())
            })
            .map(|edge| edge.to.as_str())
    }

    fn prefix_lineage_parent_id<'a>(&'a self, selected_id: &str) -> Option<&'a str> {
        self.tracked_issues
            .iter()
            .filter(|issue| {
                selected_id
                    .strip_prefix(issue.id.as_str())
                    .is_some_and(|suffix| suffix.starts_with('.'))
            })
            .max_by_key(|issue| issue.id.len())
            .map(|issue| issue.id.as_str())
    }

    fn is_lineage_parent_edge(edge_type: Option<&str>) -> bool {
        matches!(edge_type, Some("parent-child") | Some("related"))
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

    /// Inc 3 (bd-d587.3): open a detail pane on `id` from outside this view
    /// (e.g., PlanBrowser View-Epic). `mode` controls the post-load detail
    /// mode and arms graph pre-fetch when applicable. Plan-backed epics are
    /// still opened by epic id; the PM layer maps them to the full plan label
    /// graph so callers do not need to know the plan UUID.
    ///
    /// - `OpenMode::Default` / `OpenMode::FocusText`: detail shown in Text
    ///   mode; user can press `v` to flip to Graph.
    /// - `OpenMode::FocusGraph`: list-row selected immediately if id is
    ///   present (else queued via `pending_select` for next `IssuesLoaded`),
    ///   `Action::GetIssueGraph` stashed in `pending_action` for the app to
    ///   drain, and `post_load_mode` armed so `IssueDetailFetched` flips to
    ///   Graph mode.
    pub fn open_external_detail(&mut self, id: String, mode: OpenMode) {
        // bd-d587.3 follow-up: clear any state armed by a previous external
        // open so non-FocusGraph calls don't inherit stale Graph mode.
        self.reset_armed_state();
        // Move the left-list selection if the id is already tracked; else
        // stash for the next IssuesLoaded.
        if !self.issues_panel.select_by_id(&id, &self.tracked_issues) {
            self.pending_select = Some(id.clone());
        }

        self.issue_focus = IssueFocus::Loading { id: id.clone() };
        self.detail_mode = DetailMode::Text;
        self.issue_detail_pane.reset();
        self.graph_error = None;

        if matches!(mode, OpenMode::FocusGraph) {
            self.post_load_mode = Some(DetailMode::Graph);
            // Eagerly request the graph so the cache is populated by the
            // time IssueDetailFetched flips to Graph mode.
            self.graph_loading = Some(id.clone());
            self.pending_action = Some(Action::GetIssueGraph { id });
        }
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

    fn selected_implementation_plan_id(&self) -> Option<String> {
        let selected_id = self.selected_issue_id()?;
        let selected = self.selected_issue()?;

        find_plan_id_label(&selected.labels)
            .or_else(|| match &self.issue_focus {
                IssueFocus::Loaded { id, issue } if id == &selected_id => {
                    find_plan_id_label(&issue.labels)
                }
                _ => None,
            })
            .map(str::to_string)
    }

    fn open_selected_plan(&self) -> Option<Action> {
        self.selected_implementation_plan_id()
            .map(|plan_id| Action::OpenPlanInBrowser { plan_id })
            .or(Some(Action::FlashHint {
                message: "Selected issue has no implementation plan".into(),
            }))
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

    fn invalidate_graph_cache_preserving_inflight(&mut self) {
        self.graph_cache.clear();
        if self.graph_loading.is_none() {
            self.graph_error = None;
        }
    }

    fn request_selected_detail(&mut self) -> Option<Action> {
        // bd-d587.3 follow-up: a fresh user-driven detail open must not
        // inherit `post_load_mode` from a prior external open.
        self.reset_armed_state();
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
        if let Some(plan_id) = self.selected_implementation_plan_id() {
            return Some(Action::FlashHint {
                message: format!(
                    "Epic {} already has implementation plan {}; press p to open it",
                    issue.id, plan_id
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
                    // NavigateBack pops the view_history stack so we return to
                    // the actual previous view (e.g. PlanBrowser when entered
                    // via the 'o' work-item shortcut). NavigateTo(Dashboard) would
                    // clear the stack and skip past PlanBrowser entirely.
                    Some(Action::NavigateBack)
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
            KeyCode::Char('p') if key.modifiers.is_empty() => self.open_selected_plan(),

            // Navigation
            KeyCode::Char('j') | KeyCode::Down if key.modifiers.is_empty() => {
                self.issues_panel.select_next(1, self.tracked_issues.len());
                self.prefetch_selected_graph();
                Some(Action::SelectNextBy(1))
            }
            KeyCode::Char('k') | KeyCode::Up if key.modifiers.is_empty() => {
                self.issues_panel.select_prev(1, self.tracked_issues.len());
                self.prefetch_selected_graph();
                Some(Action::SelectPrevBy(1))
            }
            KeyCode::Char('g') if key.modifiers.is_empty() => {
                self.issues_panel.select_first();
                self.prefetch_selected_graph();
                None
            }
            KeyCode::Char('G') if key.modifiers.is_empty() => {
                let count = self.tracked_issues.len();
                self.issues_panel.select_last(count);
                self.prefetch_selected_graph();
                None
            }

            // Enter — fetch detail for the selected row, or close if already
            // viewing that same row's detail. If the list is empty, fall back
            // to closing any open detail so the pane never lingers without a
            // corresponding row.
            KeyCode::Enter if key.modifiers.is_empty() => self.request_selected_detail(),
            KeyCode::Char('o') if key.modifiers.is_empty() => self.request_selected_detail(),

            // Status actions
            KeyCode::Char('O') if key.modifiers.is_empty() => self.update_status("open", false),
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
        let selected_id = self.selected_issue_id();
        let lineage_mode_active = self.graph_cache.values().any(|(nodes, _)| nodes.len() > 1);
        let selected_graph_loading = selected_id
            .as_deref()
            .is_some_and(|id| self.graph_loading.as_deref() == Some(id));

        let lineage_height = selected_id
            .as_deref()
            .and_then(|id| self.graph_cache.get(id).map(|(nodes, _)| nodes.len()))
            .filter(|count| *count > 1)
            .or_else(|| {
                if lineage_mode_active && selected_graph_loading {
                    Some(issue_count)
                } else {
                    None
                }
            })
            .map(|count| (count as u16).saturating_add(6));

        let issues_height = if issue_count == 0 {
            3u16 // placeholder height
        } else if let Some(lineage_height) = lineage_height {
            lineage_height.max(8).min((area.height * 55 / 100).max(8))
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
            let (title, body, fg) = if let Some(err) = &self.last_refresh_error {
                (
                    " Issues — load failed ",
                    format!("Failed to load issues: {err}\nPress 'r' to retry."),
                    Color::Red,
                )
            } else {
                (
                    " Issues ",
                    "No issues loaded. Press 'r' to refresh.".to_string(),
                    Color::DarkGray,
                )
            };
            let block = Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(fg));
            let inner = block.inner(chunks[0]);
            frame.render_widget(block, chunks[0]);
            let msg = Paragraph::new(body).style(Style::default().fg(fg));
            frame.render_widget(msg, inner);
        } else {
            let lineage = if let Some(id) = selected_id.as_deref() {
                if let Some((nodes, edges)) = self.graph_cache.get(id) {
                    Some(IssueLineageView::Loaded(IssueLineageContext {
                        root_id: id,
                        nodes,
                        edges,
                    }))
                } else if lineage_mode_active && self.graph_loading.as_deref() == Some(id) {
                    let root_id = self.lineage_loading_root_id(id);
                    Some(IssueLineageView::Loading { root_id })
                } else {
                    None
                }
            } else {
                None
            };
            self.issues_panel
                .render_with_lineage(&self.tracked_issues, lineage, frame, chunks[0]);
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
        let has_plan = self.selected_implementation_plan_id().is_some();
        let has_execute = self.selected_issue_is_epic() && !has_plan;
        let mode_hint = match self.issue_focus {
            IssueFocus::Loaded { .. } | IssueFocus::Loading { .. } => {
                Some(match self.detail_mode {
                    DetailMode::Text if has_plan => Self::hint_override(
                        TEXT_STATUS_HINT_PLAN_EPIC,
                        TEXT_STATUS_HINT_PLAN_EPIC_COMPACT,
                    ),
                    DetailMode::Text if has_execute => {
                        Self::hint_override(TEXT_STATUS_HINT_EPIC, TEXT_STATUS_HINT_EPIC_COMPACT)
                    }
                    DetailMode::Text => {
                        Self::hint_override(TEXT_STATUS_HINT, TEXT_STATUS_HINT_COMPACT)
                    }
                    DetailMode::Graph if has_plan => Self::hint_override(
                        GRAPH_STATUS_HINT_PLAN_EPIC,
                        GRAPH_STATUS_HINT_PLAN_EPIC_COMPACT,
                    ),
                    DetailMode::Graph if has_execute => {
                        Self::hint_override(GRAPH_STATUS_HINT_EPIC, GRAPH_STATUS_HINT_EPIC_COMPACT)
                    }
                    DetailMode::Graph => {
                        Self::hint_override(GRAPH_STATUS_HINT, GRAPH_STATUS_HINT_COMPACT)
                    }
                })
            }
            IssueFocus::None if issue_count > 0 && has_plan => Some(Self::hint_override(
                LIST_STATUS_HINT_PLAN_EPIC,
                LIST_STATUS_HINT_PLAN_EPIC_COMPACT,
            )),
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
                self.invalidate_graph_cache_preserving_inflight();
                self.last_refresh_error = None;

                // Capture id-to-preserve BEFORE replacing tracked_issues so
                // we can keep the same logical row (not the same numerical
                // index) selected across refreshes. Priority order:
                //   1. pending_select — armed by open_external_detail when
                //      the requested id wasn't yet in tracked_issues.
                //   2. The id of the open detail (Loading / Loaded). Keeps
                //      the left list consistent with what the right pane is
                //      currently showing — e.g. PlanBrowser View-Epic 'e'
                //      should leave the epic row highlighted, not idx 0.
                //   3. The previously-selected row's id (read from the OLD
                //      tracked_issues, not the new one) so user-driven
                //      scrolling survives passive refreshes.
                let preferred_id: Option<String> = self
                    .pending_select
                    .clone()
                    .or_else(|| match &self.issue_focus {
                        IssueFocus::Loading { id } | IssueFocus::Loaded { id, .. } => {
                            Some(id.clone())
                        }
                        IssueFocus::None => None,
                    })
                    .or_else(|| {
                        self.issues_panel
                            .selected_id(&self.tracked_issues)
                            .map(String::from)
                    });

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
                        labels: i.labels.clone(),
                        url: String::new(),
                        priority: i.priority,
                        issue_type: i.issue_type.clone(),
                        assignee: i.assignee.clone(),
                    })
                    .collect();

                if !self.tracked_issues.is_empty() {
                    let selected = preferred_id
                        .as_deref()
                        .is_some_and(|id| self.issues_panel.select_by_id(id, &self.tracked_issues));
                    if !selected {
                        self.issues_panel.select_first();
                    }
                    // Drain pending_select once it lands in tracked_issues —
                    // the selection above already moved if the id matched.
                    if let Some(pending) = self.pending_select.as_deref() {
                        if self.tracked_issues.iter().any(|i| i.id == pending) {
                            self.pending_select = None;
                        }
                    }
                    self.prefetch_selected_graph();
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
                        // Inc 3 (bd-d587.3): apply the post-load mode armed
                        // by `open_external_detail(_, FocusGraph)`. Falls back
                        // to Text for the default palette-style entry path.
                        self.detail_mode = self.post_load_mode.take().unwrap_or(DetailMode::Text);
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

            spur_acp::SpurEventBody::IssueCommandError {
                error, operation, ..
            } => {
                if matches!(operation.as_str(), "GetIssueGraph" | "get_graph")
                    && self.graph_loading.is_some()
                {
                    self.graph_error = Some(error.clone());
                    self.graph_loading = None;
                    // bd-d587.3 follow-up: a graph-fetch error must not leave
                    // post_load_mode armed for a future unrelated detail fetch.
                    self.post_load_mode = None;
                    self.pending_action = None;
                } else if matches!(self.issue_focus, IssueFocus::Loading { .. }) {
                    self.issue_focus = IssueFocus::None;
                    // bd-d587.3 follow-up: same rationale on detail-fetch error.
                    self.reset_armed_state();
                } else if operation == "list_issues" || operation == "RefreshIssues" {
                    self.last_refresh_error = Some(error.clone());
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

fn find_plan_id_label(labels: &[String]) -> Option<&str> {
    labels
        .iter()
        .find_map(|label| label.strip_prefix("spur:plan-id:"))
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_core::ExecutorLineage;

    use crate::views::{View, ViewContext};

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn issue(id: &str, issue_type: &str, labels: Vec<String>) -> spur_pm::IssueSummary {
        spur_pm::IssueSummary {
            id: id.into(),
            source: spur_pm::PmSource::Beads,
            title: "Epic".into(),
            status: "open".into(),
            labels,
            url: format!("beads://{id}"),
            priority: Some(1),
            issue_type: Some(issue_type.into()),
            assignee: None,
        }
    }

    #[test]
    fn p_opens_plan_browser_for_plan_backed_epic() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![issue(
            "bd-1",
            "epic",
            vec!["spur:plan-id:plan-1".into(), "spur:plan-complete".into()],
        )]);

        let action = view.handle_key(key(KeyCode::Char('p')), &ctx);

        assert!(matches!(
            action,
            Some(Action::OpenPlanInBrowser { plan_id }) if plan_id == "plan-1"
        ));
    }

    #[test]
    fn p_opens_plan_browser_for_plan_backed_bug() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![issue(
            "bd-1",
            "bug",
            vec!["spur:plan-id:plan-1".into(), "spur:plan-complete".into()],
        )]);

        let action = view.handle_key(key(KeyCode::Char('p')), &ctx);

        assert!(matches!(
            action,
            Some(Action::OpenPlanInBrowser { plan_id }) if plan_id == "plan-1"
        ));
    }

    #[test]
    fn execute_is_blocked_for_plan_backed_epic() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![issue(
            "bd-1",
            "epic",
            vec!["spur:plan-id:plan-1".into(), "spur:plan-complete".into()],
        )]);

        let action = view.handle_key(key(KeyCode::Char('E')), &ctx);

        assert!(matches!(
            action,
            Some(Action::FlashHint { message })
                if message.contains("already has implementation plan")
                    && message.contains("press p")
        ));
    }
}
