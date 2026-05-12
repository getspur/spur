use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use spur_acp::{GraphEdgeEvent, GraphNodeEvent, SpurEvent};

use crate::action::{Action, IssueAction, ViewId};
use crate::components::execute_modal::{ExecuteModal, ExecuteModalVariant};
use crate::components::issue_detail_pane::IssueDetailPane;
use crate::components::issue_graph_pane::IssueGraphPane;
use crate::components::issue_utils::{find_plan_id_label, has_label, has_plan_task_label};
use crate::components::issues_panel::IssuesPanel;
use crate::components::status_bar::{HintOverride, StatusBar, StatusBarProps};
use crate::components::tombstone::Tombstone;

use super::View;

const TEXT_STATUS_HINT: &str =
    "[Text] j/k: Nav  v: Graph Mode  PgUp/PgDn: Scroll  Esc: Close Detail  q: Quit";
const TEXT_STATUS_HINT_COMPACT: &str = "[Text] j/k: Nav  v: Graph  Esc: Close";
const TEXT_STATUS_HINT_EPIC: &str =
    "[Text] j/k: Nav  v: Graph Mode  e: Execute Item  PgUp/PgDn: Scroll  Esc: Close Detail  q: Quit";
const TEXT_STATUS_HINT_EPIC_COMPACT: &str = "[Text] j/k: Nav  e: Execute Item  v: Graph";
const TEXT_STATUS_HINT_PLAN_EPIC: &str =
    "[Text] j/k: Nav  p: Open Plan  v: Graph Mode  PgUp/PgDn: Scroll  Esc: Close Detail  q: Quit";
const TEXT_STATUS_HINT_PLAN_EPIC_COMPACT: &str = "[Text] j/k: Nav  p: Plan  v: Graph";
const GRAPH_STATUS_HINT: &str =
    "[Graph] j/k: Nav  v: Text Mode  PgUp/PgDn: Scroll  Esc: Close Graph  q: Quit";
const GRAPH_STATUS_HINT_COMPACT: &str = "[Graph] j/k: Nav  v: Text  Esc: Close";
const GRAPH_STATUS_HINT_EPIC: &str =
    "[Graph] j/k: Nav  v: Text Mode  e: Execute Item  PgUp/PgDn: Scroll  Esc: Close Graph  q: Quit";
const GRAPH_STATUS_HINT_EPIC_COMPACT: &str = "[Graph] j/k: Nav  e: Execute Item  v: Text";
const GRAPH_STATUS_HINT_PLAN_EPIC: &str =
    "[Graph] j/k: Nav  p: Open Plan  v: Text Mode  PgUp/PgDn: Scroll  Esc: Close Graph  q: Quit";
const GRAPH_STATUS_HINT_PLAN_EPIC_COMPACT: &str = "[Graph] j/k: Nav  p: Plan  v: Text";
const LIST_STATUS_HINT: &str =
    "[List] j/k: Nav  Enter/o: Open Detail  v: View Graph  W: Work  r: Refresh  q: Quit";
const LIST_STATUS_HINT_COMPACT: &str = "[List] j/k: Nav  o: Open  W: Work  r: Refresh";
const LIST_STATUS_HINT_EPIC: &str =
    "[List] j/k: Nav  Enter/o: Open Detail  v: View Graph  e: Execute Item  W: Work  r: Refresh  q: Quit";
const LIST_STATUS_HINT_EPIC_COMPACT: &str = "[List] j/k: Nav  o: Open  e: Execute Item  W: Work";
const LIST_STATUS_HINT_PLAN_EPIC: &str =
    "[List] j/k: Nav  Enter/o: Open Detail  v: Graph  p: Open Plan  W: Work  r: Refresh  q: Quit";
const LIST_STATUS_HINT_PLAN_EPIC_COMPACT: &str = "[List] j/k: Nav  o: Open  p: Plan  W: Work";
const GRAPH_CACHE_CAPACITY: usize = 32;
const PREFETCH_DEBOUNCE_MS: u64 = 200;

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
        external_ref: None,
        source_system: None,
        source_repo: None,
    }
}

fn sort_issues_parent_first(issues: &mut [spur_pm::IssueSummary]) {
    issues.sort_by(compare_issue_parent_first);
}

fn compare_issue_parent_first(a: &spur_pm::IssueSummary, b: &spur_pm::IssueSummary) -> Ordering {
    let a_root = issue_root_id(&a.id);
    let b_root = issue_root_id(&b.id);

    if a_root == b_root {
        if is_issue_ancestor(&a.id, &b.id) {
            return Ordering::Less;
        }
        if is_issue_ancestor(&b.id, &a.id) {
            return Ordering::Greater;
        }

        let type_order = match (a.issue_type.as_deref(), b.issue_type.as_deref()) {
            (Some("epic"), other) if other != Some("epic") => Ordering::Less,
            (other, Some("epic")) if other != Some("epic") => Ordering::Greater,
            _ => Ordering::Equal,
        };
        if type_order != Ordering::Equal {
            return type_order;
        }

        let depth_order = issue_depth(&a.id).cmp(&issue_depth(&b.id));
        if depth_order != Ordering::Equal {
            return depth_order;
        }
    }

    Ordering::Equal
}

fn issue_root_id(id: &str) -> &str {
    id.split_once('.').map(|(root, _)| root).unwrap_or(id)
}

fn issue_depth(id: &str) -> usize {
    id.matches('.').count()
}

fn is_issue_ancestor(parent: &str, child: &str) -> bool {
    child
        .strip_prefix(parent)
        .is_some_and(|suffix| suffix.starts_with('.'))
}

// ── View ────────────────────────────────────────────────────────────────

pub struct IssueBrowserView {
    tracked_issues: Vec<spur_pm::IssueSummary>,
    issues_panel: IssuesPanel,
    filter_mode: bool,
    issue_detail_pane: IssueDetailPane,
    issue_focus: IssueFocus,
    detail_mode: DetailMode,
    graph_pane: IssueGraphPane,
    graph_data_epoch: u64,
    detail_data_epoch: u64,
    detail_request_epochs: HashMap<String, u64>,
    graph_request_epochs: HashMap<String, u64>,
    graph_cache: HashMap<String, (Vec<GraphNodeEvent>, Vec<GraphEdgeEvent>)>,
    graph_cache_order: VecDeque<String>,
    graph_loading: Option<String>,
    graph_error: Option<(String, String)>,
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
    /// Debounced preview graph prefetch armed by list navigation.
    pending_prefetch: Option<(String, Instant)>,
    /// Error from the most recent `list_issues` failure surfaced via
    /// `IssueCommandError` (e.g. corrupt `.beads/issues.jsonl`). Rendered in
    /// the empty-list pane so the user sees the cause instead of a misleading
    /// "No issues loaded" placeholder. Cleared on the next `IssuesLoaded`.
    last_refresh_error: Option<String>,
    last_issues_panel_height: u16,
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
            filter_mode: false,
            issue_detail_pane: IssueDetailPane::new(),
            issue_focus: IssueFocus::None,
            detail_mode: DetailMode::Text,
            graph_pane: IssueGraphPane::new(),
            graph_data_epoch: 0,
            detail_data_epoch: 0,
            detail_request_epochs: HashMap::new(),
            graph_request_epochs: HashMap::new(),
            graph_cache: HashMap::new(),
            graph_cache_order: VecDeque::new(),
            graph_loading: None,
            graph_error: None,
            execute_modal: None,
            pending_select: None,
            post_load_mode: None,
            pending_action: None,
            pending_prefetch: None,
            last_refresh_error: None,
            last_issues_panel_height: 0,
        }
    }

    /// Inc 3 (bd-d587.3): drain the pending follow-up action stashed by
    /// `open_external_detail` / `IssueDetailFetched`. The app polls this
    /// after dispatching events to the view so it can route the action
    /// through `process_action` (the only way to actually execute it).
    pub fn take_pending_action(&mut self) -> Option<Action> {
        if self.pending_action.is_none() {
            self.flush_due_prefetch();
        }
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
    /// / `pending_prefetch` / `graph_loading` from a previous (possibly
    /// errored) external open.
    fn reset_armed_state(&mut self) {
        self.pending_select = None;
        self.post_load_mode = None;
        self.pending_action = None;
        self.pending_prefetch = None;
        self.graph_loading = None;
    }

    pub fn tracked_issues(&self) -> &[spur_pm::IssueSummary] {
        &self.tracked_issues
    }

    pub fn seed_issues(&mut self, mut issues: Vec<spur_pm::IssueSummary>) {
        self.last_refresh_error = None;
        self.bump_graph_data_epoch();
        self.bump_detail_data_epoch();
        self.invalidate_graph_cache();
        sort_issues_parent_first(&mut issues);
        self.tracked_issues = issues;
        if !self.tracked_issues.is_empty() {
            self.issues_panel.select_first(self.tracked_issues.len());
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_issues_for_test(&mut self, issues: Vec<spur_pm::IssueSummary>) {
        self.seed_issues(issues);
    }

    #[cfg(any(test, debug_assertions))]
    pub fn age_pending_prefetch_for_test(&mut self, age: Duration) {
        if let Some((_, scheduled_at)) = self.pending_prefetch.as_mut() {
            *scheduled_at -= age;
        }
    }

    pub fn issue_detail_visible(&self) -> bool {
        matches!(self.issue_focus, IssueFocus::Loaded { .. })
    }

    pub fn is_filter_mode(&self) -> bool {
        self.filter_mode
    }

    fn prefetch_selected_graph(&mut self) {
        if self.tracked_issues.len() < 2 {
            self.pending_prefetch = None;
            return;
        }
        let Some(id) = self.selected_issue_id() else {
            self.pending_prefetch = None;
            return;
        };
        if self.graph_cache.contains_key(&id) || self.graph_loading.as_deref() == Some(id.as_str())
        {
            return;
        }

        self.graph_error = None;
        self.pending_prefetch = Some((id, Instant::now()));
    }

    fn flush_due_prefetch(&mut self) {
        if self.pending_action.is_some() {
            return;
        }
        let Some((id, scheduled_at)) = self.pending_prefetch.clone() else {
            return;
        };
        if Instant::now().saturating_duration_since(scheduled_at)
            < Duration::from_millis(PREFETCH_DEBOUNCE_MS)
        {
            return;
        }

        self.pending_prefetch = None;
        if self.selected_issue_id().as_deref() != Some(id.as_str()) {
            return;
        }
        if self.graph_cache.contains_key(&id) || self.graph_loading.as_deref() == Some(id.as_str())
        {
            return;
        }

        self.graph_error = None;
        self.graph_loading = Some(id.clone());
        self.pending_action = Some(self.get_issue_graph_action(id));
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
        self.detail_request_epochs
            .insert(id.clone(), self.detail_data_epoch);
        self.detail_mode = DetailMode::Text;
        self.issue_detail_pane.reset();
        self.graph_error = None;

        if matches!(mode, OpenMode::FocusGraph) {
            self.post_load_mode = Some(DetailMode::Graph);
            // Eagerly request the graph so the cache is populated by the
            // time IssueDetailFetched flips to Graph mode.
            self.graph_loading = Some(id.clone());
            self.pending_action = Some(self.get_issue_graph_action(id));
        }
    }

    fn selected_issue_id(&self) -> Option<String> {
        self.issues_panel
            .selected_id(&self.tracked_issues)
            .map(String::from)
    }

    fn half_page_issue_rows(&self) -> usize {
        if self.last_issues_panel_height == 0 {
            10
        } else {
            usize::from((self.last_issues_panel_height / 2).max(1))
        }
    }

    fn page_issue_rows(&self) -> usize {
        if self.last_issues_panel_height == 0 {
            20
        } else {
            usize::from(self.last_issues_panel_height)
        }
    }

    fn select_next_issue_rows(&mut self, rows: usize) -> Option<Action> {
        self.issues_panel
            .select_next(rows, self.tracked_issues.len());
        self.prefetch_selected_graph();
        Some(Action::SelectNextBy(rows))
    }

    fn select_prev_issue_rows(&mut self, rows: usize) -> Option<Action> {
        self.issues_panel
            .select_prev(rows, self.tracked_issues.len());
        self.prefetch_selected_graph();
        Some(Action::SelectPrevBy(rows))
    }

    /// Graph errors are request-id scoped; render callers must not treat them
    /// as global detail-pane state.
    fn graph_error_for(&self, issue_id: &str) -> Option<&str> {
        self.graph_error
            .as_ref()
            .and_then(|(id, message)| (id == issue_id).then_some(message.as_str()))
    }

    fn selected_issue(&self) -> Option<&spur_pm::IssueSummary> {
        let selected_id = self.issues_panel.selected_id(&self.tracked_issues)?;
        self.tracked_issues
            .iter()
            .find(|issue| issue.id == selected_id)
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

    fn request_graph_if_needed(&mut self, id: String) -> Option<Action> {
        if self.graph_cache.contains_key(&id) {
            self.graph_loading = None;
            None
        } else if self.graph_loading.as_deref() == Some(id.as_str()) {
            None
        } else {
            self.graph_loading = Some(id.clone());
            Some(self.get_issue_graph_action(id))
        }
    }

    fn bump_graph_data_epoch(&mut self) {
        self.graph_data_epoch = self.graph_data_epoch.wrapping_add(1);
    }

    fn bump_detail_data_epoch(&mut self) {
        self.detail_data_epoch = self.detail_data_epoch.wrapping_add(1);
    }

    fn get_issue_graph_action(&mut self, id: String) -> Action {
        self.graph_request_epochs
            .insert(id.clone(), self.graph_data_epoch);
        Action::GetIssueGraph { id }
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
        self.graph_cache_order.clear();
        self.graph_loading = None;
        self.pending_prefetch = None;
        self.graph_error = None;
    }

    fn invalidate_graph_cache_preserving_inflight(&mut self) {
        self.graph_cache.clear();
        self.graph_cache_order.clear();
        self.pending_prefetch = None;
        if self.graph_loading.is_none() {
            self.graph_error = None;
        }
    }

    fn insert_graph_cache(
        &mut self,
        key: String,
        nodes: Vec<GraphNodeEvent>,
        edges: Vec<GraphEdgeEvent>,
    ) {
        if self.graph_cache.contains_key(&key) {
            self.graph_cache_order.retain(|existing| existing != &key);
        }

        self.graph_cache.insert(key.clone(), (nodes, edges));
        self.graph_cache_order.push_back(key);

        while self.graph_cache.len() > GRAPH_CACHE_CAPACITY {
            let Some(evicted) = self.graph_cache_order.pop_front() else {
                break;
            };
            self.graph_cache.remove(&evicted);
        }
    }

    fn invalidate_graph_cache_entries_containing_issue(&mut self, issue_id: &str) {
        let keys_to_remove = self
            .graph_cache
            .iter()
            .filter(|(_, (nodes, edges))| {
                nodes.iter().any(|node| node.id == issue_id)
                    || edges
                        .iter()
                        .any(|edge| edge.from == issue_id || edge.to == issue_id)
            })
            .map(|(key, _)| key.clone())
            .collect::<HashSet<_>>();

        if keys_to_remove.is_empty() {
            return;
        }

        self.graph_cache
            .retain(|key, _| !keys_to_remove.contains(key));
        self.graph_cache_order
            .retain(|key| !keys_to_remove.contains(key));
    }

    fn request_selected_detail(&mut self) -> Option<Action> {
        self.request_selected_detail_with_post_load_mode(None)
    }

    fn request_selected_detail_with_post_load_mode(
        &mut self,
        post_load_mode: Option<DetailMode>,
    ) -> Option<Action> {
        // bd-d587.3 follow-up: a fresh user-driven detail open must not
        // inherit `post_load_mode` from a prior external open.
        self.reset_armed_state();
        self.post_load_mode = post_load_mode;
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
                self.detail_request_epochs
                    .insert(sel.clone(), self.detail_data_epoch);
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
            (_, None) => {
                self.post_load_mode = None;
                None
            }
        }
    }

    fn toggle_detail_mode(&mut self) -> Option<Action> {
        match &self.issue_focus {
            IssueFocus::Loaded { id, .. } => match self.detail_mode {
                DetailMode::Text => {
                    self.detail_mode = DetailMode::Graph;
                    self.graph_pane.reset();
                    self.graph_error = None;
                    self.pending_prefetch = None;
                    self.request_graph_if_needed(id.clone())
                }
                DetailMode::Graph => {
                    self.detail_mode = DetailMode::Text;
                    None
                }
            },
            IssueFocus::None => {
                let action =
                    self.request_selected_detail_with_post_load_mode(Some(DetailMode::Graph));
                if action.is_some() {
                    if let Some(id) = self.selected_issue_id() {
                        self.pending_action = self.request_graph_if_needed(id);
                    }
                }
                action
            }
            IssueFocus::Loading { .. } => None,
        }
    }

    fn open_execute_modal(&mut self) -> Option<Action> {
        let issue = self.selected_issue()?;
        // Removed type check: execution is now generalized to any work item type.
        if let Some(plan_id) = self.selected_implementation_plan_id() {
            return Some(Action::FlashHint {
                message: format!(
                    "Work item {} already has implementation plan {}; press p to open it",
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
                    Some(Action::Issue(IssueAction::Execute { id }))
                }
                KeyCode::Char('e') => {
                    let id = modal.epic_id.clone();
                    self.execute_modal = None;
                    Some(Action::Issue(IssueAction::ExecuteEdit { id }))
                }
                KeyCode::Esc => {
                    self.execute_modal = None;
                    None
                }
                _ => None,
            };
        }

        if self.filter_mode {
            let accepts_text_input =
                key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
            return match key.code {
                KeyCode::Esc => {
                    self.filter_mode = false;
                    self.issues_panel.clear_filter(&self.tracked_issues);
                    None
                }
                KeyCode::Enter => {
                    self.filter_mode = false;
                    None
                }
                KeyCode::Backspace => {
                    let mut query = self.issues_panel.filter_query().to_string();
                    query.pop();
                    self.issues_panel.set_filter(&query, &self.tracked_issues);
                    None
                }
                KeyCode::Char(c) if accepts_text_input => {
                    let mut query = self.issues_panel.filter_query().to_string();
                    query.push(c);
                    self.issues_panel.set_filter(&query, &self.tracked_issues);
                    None
                }
                _ => None,
            };
        }

        match key.code {
            KeyCode::Char('/') if key.modifiers.is_empty() => {
                self.filter_mode = true;
                None
            }
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
                self.bump_graph_data_epoch();
                self.bump_detail_data_epoch();

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
            KeyCode::Char('J') if key.modifiers == KeyModifiers::SHIFT => {
                self.select_next_issue_rows(5)
            }
            KeyCode::Char('K') if key.modifiers == KeyModifiers::SHIFT => {
                self.select_prev_issue_rows(5)
            }
            KeyCode::Char('d') if key.modifiers == KeyModifiers::CONTROL => {
                self.select_next_issue_rows(self.half_page_issue_rows())
            }
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                self.select_prev_issue_rows(self.half_page_issue_rows())
            }
            KeyCode::Char('g') if key.modifiers.is_empty() => {
                self.issues_panel.select_first(self.tracked_issues.len());
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
                let id = self
                    .issues_panel
                    .selected_id(&self.tracked_issues)
                    .map(String::from);
                id.map(|id| Action::Issue(IssueAction::WorkOn { id }))
            }
            KeyCode::Char('e') if key.modifiers.is_empty() => self.open_execute_modal(),

            // Detail scroll (when loaded)
            KeyCode::PageUp if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                self.scroll_issue_detail_up_by(10);
                Some(Action::ScrollUp)
            }
            KeyCode::PageDown if matches!(self.issue_focus, IssueFocus::Loaded { .. }) => {
                self.scroll_issue_detail_down_by(10);
                Some(Action::ScrollDown)
            }
            KeyCode::PageUp
                if matches!(
                    self.issue_focus,
                    IssueFocus::None | IssueFocus::Loading { .. }
                ) =>
            {
                self.select_prev_issue_rows(self.page_issue_rows())
            }
            KeyCode::PageDown
                if matches!(
                    self.issue_focus,
                    IssueFocus::None | IssueFocus::Loading { .. }
                ) =>
            {
                self.select_next_issue_rows(self.page_issue_rows())
            }

            _ => None,
        }
    }

    fn update_status(&self, status: &str, via_legacy_key: bool) -> Option<Action> {
        let id = self
            .issues_panel
            .selected_id(&self.tracked_issues)
            .map(String::from);
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
        theme: &crate::theme::Theme,
        tombstone: Option<&Tombstone>,
        view_hint_override: Option<HintOverride<'_>>,
    ) {
        let issue_count = self.tracked_issues.len();
        let show_filter_bar = self.filter_mode || !self.issues_panel.filter_query().is_empty();

        let issues_height = if issue_count == 0 {
            3u16 // placeholder height
        } else {
            IssuesPanel::computed_height(issue_count, area.height)
                .max(4)
                .min(area.height * 40 / 100)
        };
        let filter_bar_height = if show_filter_bar { 1 } else { 0 };
        let issues_section_height = issues_height.saturating_add(filter_bar_height);
        self.last_issues_panel_height = issues_height;

        let has_detail = matches!(self.issue_focus, IssueFocus::Loaded { .. });
        let detail_min = if has_detail { 8 } else { 3 };

        let constraints = vec![
            Constraint::Length(issues_section_height),
            Constraint::Min(detail_min),
            Constraint::Length(1), // status bar
        ];
        let chunks = Layout::vertical(constraints).split(area);

        // ── Issues panel ──────────────────────────────────────────────────
        let issues_area = if show_filter_bar {
            let issue_chunks =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(chunks[0]);
            let query = self.issues_panel.filter_query();
            let line = Line::from(vec![
                Span::styled(" /", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}{}", query, if self.filter_mode { "_" } else { "" }),
                    Style::default().fg(if self.filter_mode {
                        Color::Cyan
                    } else {
                        Color::Gray
                    }),
                ),
            ]);
            frame.render_widget(Paragraph::new(line), issue_chunks[0]);
            issue_chunks[1]
        } else {
            chunks[0]
        };

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
            let inner = block.inner(issues_area);
            frame.render_widget(block, issues_area);
            let msg = Paragraph::new(body).style(Style::default().fg(fg));
            frame.render_widget(msg, inner);
        } else {
            self.issues_panel
                .render(&self.tracked_issues, frame, issues_area);
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
                    if let Some(error) = self.graph_error_for(id) {
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
        let has_execute = self.selected_issue().is_some() && !has_plan;
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
                theme,
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
                self.bump_graph_data_epoch();
                self.bump_detail_data_epoch();
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

                let mut loaded_issues = issues
                    .iter()
                    .filter(|issue| !is_plan_artifact_summary(issue))
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
                        description: i.description.clone(),
                    })
                    .collect::<Vec<_>>();
                sort_issues_parent_first(&mut loaded_issues);
                self.tracked_issues = loaded_issues;

                if !self.tracked_issues.is_empty() {
                    let selected = preferred_id
                        .as_deref()
                        .is_some_and(|id| self.issues_panel.select_by_id(id, &self.tracked_issues));
                    if !selected {
                        self.issues_panel.select_first(self.tracked_issues.len());
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
                source: _,
                id,
                status,
                assignee,
            } => {
                self.bump_graph_data_epoch();
                self.bump_detail_data_epoch();
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
                self.invalidate_graph_cache_entries_containing_issue(id);
            }

            spur_acp::SpurEventBody::IssueDetailFetched {
                requested_id,
                issue,
            } => {
                if let Some(req_epoch) = self.detail_request_epochs.remove(requested_id) {
                    if req_epoch < self.detail_data_epoch {
                        tracing::info!(
                            target: "issue_probe",
                            site = "detail_response_stale",
                            id = %requested_id,
                            req_epoch,
                            current_epoch = self.detail_data_epoch,
                        );
                        let matches_loading = match &self.issue_focus {
                            IssueFocus::Loading { id: loading_id } => requested_id == loading_id,
                            _ => false,
                        };
                        if matches_loading {
                            self.issue_focus = IssueFocus::None;
                            self.issue_detail_pane.reset();
                            self.reset_armed_state();
                        }
                        return;
                    }
                }
                // PROBE: issue_detail_latency — confirm the TUI received the
                // event and whether `issue_focus` was still Loading for it
                // (a no-op match here means the user navigated away or Esc'd
                // before the round-trip completed, which still costs CPU).
                let still_loading_match = matches!(
                    &self.issue_focus,
                    IssueFocus::Loading { id } if id == requested_id
                );
                let body_len: usize = issue.body.len();
                let n_labels: usize = issue.labels.len();
                let ts_ns: u64 = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                tracing::info!(
                    target: "issue_probe",
                    site = "tui_event_received",
                    id = %requested_id,
                    body_len = body_len,
                    n_labels = n_labels,
                    still_loading_match = still_loading_match,
                    ts_ns = ts_ns,
                    "IssueDetailFetched arrived in TUI view",
                );
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
                if let Some(req_epoch) = self.graph_request_epochs.remove(requested_id) {
                    if req_epoch < self.graph_data_epoch {
                        tracing::info!(
                            target: "issue_probe",
                            site = "graph_response_stale",
                            id = %requested_id,
                            req_epoch,
                            current_epoch = self.graph_data_epoch,
                        );
                        if self.graph_loading.as_deref() == Some(requested_id.as_str()) {
                            self.graph_loading = None;
                        }
                        return;
                    }
                }

                let matches_loading = self.graph_loading.as_deref() == Some(requested_id.as_str());
                let matches_selected = self
                    .issues_panel
                    .selected_id(&self.tracked_issues)
                    .is_some_and(|id| id == requested_id.as_str());
                if !matches_loading && !matches_selected {
                    return;
                }

                self.insert_graph_cache(requested_id.clone(), nodes.clone(), edges.clone());
                self.graph_loading = None;
                self.graph_error = None;
            }

            spur_acp::SpurEventBody::IssueCommandError {
                error,
                operation,
                id,
            } => {
                if operation == "list_issues" || operation == "RefreshIssues" {
                    self.last_refresh_error = Some(error.clone());
                } else if matches!(operation.as_str(), "GetIssueGraph" | "get_graph") {
                    if id.as_deref() == self.graph_loading.as_deref() {
                        if let Some(id) = id {
                            self.graph_error = Some((id.clone(), error.clone()));
                        }
                        self.graph_loading = None;
                        // bd-d587.3 follow-up: a graph-fetch error must not leave
                        // post_load_mode armed for a future unrelated detail fetch.
                        self.post_load_mode = None;
                        self.pending_action = None;
                    }
                } else if operation == "GetIssueDetail" {
                    let matches_loading = match &self.issue_focus {
                        IssueFocus::Loading { id: loading_id } => {
                            id.as_deref() == Some(loading_id.as_str())
                        }
                        _ => false,
                    };
                    if matches_loading {
                        self.issue_focus = IssueFocus::None;
                        self.issue_detail_pane.reset();
                        // bd-d587.3 follow-up: same rationale on detail-fetch error.
                        self.reset_armed_state();
                    }
                }
            }

            _ => {}
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &super::ViewContext) {
        self.render_inner(
            frame,
            area,
            ctx.theme,
            ctx.tombstone,
            ctx.transient_hint_override,
        );
    }

    fn tick(&mut self) {
        self.flush_due_prefetch();
    }
}

fn is_plan_artifact_summary(issue: &spur_acp::IssueSummaryEvent) -> bool {
    issue.issue_type.as_deref() == Some("plan")
        || has_label(&issue.labels, "spur:plan-complete")
        || has_plan_task_label(&issue.labels)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};
    use spur_core::ExecutorLineage;

    use crate::views::{View, ViewContext};

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn modified_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn seed_fast_scroll_issues(view: &mut IssueBrowserView, count: usize) {
        view.tracked_issues = (0..count)
            .map(|idx| issue(&format!("bd-{idx:02}"), "task", Vec::new()))
            .collect();
        view.issues_panel.select_first(view.tracked_issues.len());
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
            description: None,
        }
    }

    fn issue_detail_event(id: &str) -> spur_acp::IssueDetailEvent {
        let now = chrono::Utc::now();
        spur_acp::IssueDetailEvent {
            id: id.into(),
            source: "beads".into(),
            title: "Current detail".into(),
            body: "body".into(),
            status: "open".into(),
            labels: Vec::new(),
            assignee: None,
            url: String::new(),
            priority: Some(0),
            issue_type: Some("task".into()),
            blocked_by: Vec::new(),
            due_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn issue_detail(id: &str, body: &str) -> spur_pm::Issue {
        let now = chrono::Utc::now();
        spur_pm::Issue {
            id: id.into(),
            source: spur_pm::PmSource::Beads,
            title: "Current detail".into(),
            body: body.into(),
            status: "open".into(),
            labels: Vec::new(),
            assignee: None,
            url: String::new(),
            priority: Some(1),
            issue_type: Some("task".into()),
            blocked_by: Vec::new(),
            due_at: None,
            created_at: now,
            updated_at: now,
            external_ref: None,
            source_system: None,
            source_repo: None,
        }
    }

    fn issue_command_error(operation: &str, id: Option<&str>) -> spur_acp::SpurEvent {
        spur_acp::SpurEvent::now(spur_acp::SpurEventBody::IssueCommandError {
            operation: operation.into(),
            error: "failed".into(),
            id: id.map(String::from),
        })
    }

    fn rendered_text_at(view: &mut IssueBrowserView, width: u16, height: u16) -> String {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| view.render(frame, frame.area(), &ctx))
            .unwrap();
        let buf = terminal.backend().buffer();
        let mut rendered = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                rendered.push_str(buf[(x, y)].symbol());
            }
            rendered.push('\n');
        }
        rendered
    }

    fn rendered_text(view: &mut IssueBrowserView) -> String {
        rendered_text_at(view, 100, 18)
    }

    #[test]
    fn seed_issues_clears_last_refresh_error() {
        let mut view = IssueBrowserView::new();
        view.last_refresh_error = Some("old refresh error".into());

        view.seed_issues(vec![issue("bd-1", "task", Vec::new())]);

        assert!(view.last_refresh_error.is_none());
    }

    #[test]
    fn stale_detail_error_does_not_clear_current_loading_focus() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.issue_focus = IssueFocus::Loading {
            id: "current".into(),
        };

        view.handle_spur_event(&issue_command_error("GetIssueDetail", Some("stale")), &ctx);

        assert!(matches!(
            view.issue_focus,
            IssueFocus::Loading { ref id } if id == "current"
        ));
    }

    #[test]
    fn list_error_while_detail_loading_preserves_focus_and_records_refresh_error() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.issue_focus = IssueFocus::Loading {
            id: "current".into(),
        };

        view.handle_spur_event(&issue_command_error("list_issues", None), &ctx);

        assert!(matches!(
            view.issue_focus,
            IssueFocus::Loading { ref id } if id == "current"
        ));
        assert_eq!(view.last_refresh_error.as_deref(), Some("failed"));
    }

    #[test]
    fn matching_detail_error_clears_focus_and_resets_detail_pane() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.issue_detail_pane.scroll_down_by(3);
        view.issue_focus = IssueFocus::Loading {
            id: "current".into(),
        };

        view.handle_spur_event(
            &issue_command_error("GetIssueDetail", Some("current")),
            &ctx,
        );

        assert!(matches!(view.issue_focus, IssueFocus::None));

        view.issue_focus = IssueFocus::Loaded {
            id: "current".into(),
            issue: Box::new(issue_detail(
                "current",
                "body line 1\nbody line 2\nbody line 3\nbody line 4",
            )),
        };
        let rendered = rendered_text(&mut view);
        assert!(
            rendered.contains("body line 1"),
            "detail pane scroll offset should reset after load error:\n{rendered}"
        );
    }

    #[test]
    fn stale_graph_error_does_not_clear_current_graph_loading() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.graph_loading = Some("current".into());

        view.handle_spur_event(&issue_command_error("GetIssueGraph", Some("stale")), &ctx);

        assert_eq!(view.graph_loading.as_deref(), Some("current"));
        assert!(view.graph_error.is_none());
    }

    #[test]
    fn graph_error_for_other_id_does_not_poison_currently_loaded_graph() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.graph_loading = Some("A".into());
        view.handle_spur_event(&subgraph_loaded_event("A", vec![graph_node("A")]), &ctx);
        view.issue_focus = IssueFocus::Loaded {
            id: "A".into(),
            issue: Box::new(issue_detail("A", "A body")),
        };
        view.detail_mode = DetailMode::Graph;
        view.graph_loading = Some("B".into());

        view.handle_spur_event(&graph_error_event(Some("B")), &ctx);

        assert!(view.graph_error_for("A").is_none());
        assert_eq!(view.graph_error_for("B"), Some("graph failed"));

        let rendered = rendered_text(&mut view);
        assert!(
            rendered.contains("Issue Graph: A (1 nodes)"),
            "cached graph for A should render despite B's failed prefetch:\n{rendered}"
        );
        assert!(
            !rendered.contains("Graph error: graph failed"),
            "B's graph error should not render in A's detail pane:\n{rendered}"
        );
    }

    fn graph_node(id: &str) -> GraphNodeEvent {
        GraphNodeEvent {
            id: id.into(),
            title: Some(id.into()),
            status: Some("open".into()),
            priority: Some(1),
            labels: Vec::new(),
            pagerank: None,
        }
    }

    fn graph_edge(from: &str, to: &str) -> GraphEdgeEvent {
        GraphEdgeEvent {
            from: from.into(),
            to: to.into(),
            edge_type: Some("blocks".into()),
        }
    }

    fn subgraph_loaded_event(requested_id: &str, nodes: Vec<GraphNodeEvent>) -> SpurEvent {
        subgraph_loaded_event_with_edges(requested_id, nodes, Vec::new())
    }

    fn subgraph_loaded_event_with_edges(
        requested_id: &str,
        nodes: Vec<GraphNodeEvent>,
        edges: Vec<GraphEdgeEvent>,
    ) -> SpurEvent {
        SpurEvent::now(spur_acp::SpurEventBody::IssueSubgraphLoaded {
            requested_id: requested_id.into(),
            nodes,
            edges,
        })
    }

    fn graph_error_event(id: Option<&str>) -> SpurEvent {
        SpurEvent::now(spur_acp::SpurEventBody::IssueCommandError {
            operation: "GetIssueGraph".into(),
            error: "graph failed".into(),
            id: id.map(str::to_string),
        })
    }

    #[test]
    fn slash_enters_filter_mode_and_consumes_event() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![issue("bd-1", "task", Vec::new())]);

        let action = view.handle_key(key(KeyCode::Char('/')), &ctx);

        assert!(action.is_none());
        assert!(view.filter_mode);
    }

    #[test]
    fn char_in_filter_mode_appends_to_panel_filter_query() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![issue("bd-auth", "task", Vec::new())]);

        view.handle_key(key(KeyCode::Char('/')), &ctx);
        for c in ['a', 'u', 't', 'h'] {
            view.handle_key(key(KeyCode::Char(c)), &ctx);
        }

        assert_eq!(view.issues_panel.filter_query(), "auth");
    }

    #[test]
    fn backspace_in_filter_mode_pops_panel_filter_query() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![issue("bd-auth", "task", Vec::new())]);

        view.handle_key(key(KeyCode::Char('/')), &ctx);
        for c in ['a', 'u', 't', 'h'] {
            view.handle_key(key(KeyCode::Char(c)), &ctx);
        }
        view.handle_key(key(KeyCode::Backspace), &ctx);

        assert_eq!(view.issues_panel.filter_query(), "aut");
    }

    #[test]
    fn esc_in_filter_mode_clears_filter_and_exits_mode() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![issue("bd-auth", "task", Vec::new())]);

        view.handle_key(key(KeyCode::Char('/')), &ctx);
        view.handle_key(key(KeyCode::Char('a')), &ctx);
        view.handle_key(key(KeyCode::Esc), &ctx);

        assert!(!view.filter_mode);
        assert_eq!(view.issues_panel.filter_query(), "");
    }

    #[test]
    fn enter_in_filter_mode_keeps_filter_and_exits_mode() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![issue("bd-auth", "task", Vec::new())]);

        view.handle_key(key(KeyCode::Char('/')), &ctx);
        view.handle_key(key(KeyCode::Char('a')), &ctx);
        view.handle_key(key(KeyCode::Enter), &ctx);

        assert!(!view.filter_mode);
        assert_ne!(view.issues_panel.filter_query(), "");
    }

    #[test]
    fn j_in_filter_mode_does_not_navigate_and_appends_j_to_query() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![
            issue("bd-j-1", "task", Vec::new()),
            issue("bd-2", "task", Vec::new()),
        ]);
        let selected_before = view.selected_issue_id();

        view.handle_key(key(KeyCode::Char('/')), &ctx);
        let action = view.handle_key(key(KeyCode::Char('j')), &ctx);

        assert!(action.is_none());
        assert_eq!(view.issues_panel.filter_query(), "j");
        assert_eq!(view.selected_issue_id(), selected_before);
    }

    #[test]
    fn j_outside_filter_mode_still_navigates() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![
            issue("bd-1", "task", Vec::new()),
            issue("bd-2", "task", Vec::new()),
        ]);

        let action = view.handle_key(key(KeyCode::Char('j')), &ctx);

        assert!(matches!(action, Some(Action::SelectNextBy(1))));
        assert_eq!(view.selected_issue_id().as_deref(), Some("bd-2"));
    }

    #[test]
    fn slash_with_empty_issues_enters_filter_mode_without_panic() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(Vec::new());

        let action = view.handle_key(key(KeyCode::Char('/')), &ctx);

        assert!(action.is_none());
        assert!(view.filter_mode);
    }

    #[test]
    fn execute_modal_blocks_filter_mode() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.execute_modal = Some(ExecuteModal {
            epic_id: "bd-1".into(),
            epic_title: "Epic".into(),
            variant: ExecuteModalVariant::Confirm,
        });

        let action = view.handle_key(key(KeyCode::Char('/')), &ctx);

        assert!(action.is_none());
        assert!(!view.filter_mode);
    }

    #[test]
    fn execute_modal_e_emits_edit_action() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.execute_modal = Some(ExecuteModal {
            epic_id: "bd-1".into(),
            epic_title: "Epic".into(),
            variant: ExecuteModalVariant::Confirm,
        });

        let action = view.handle_key(key(KeyCode::Char('e')), &ctx);

        assert!(matches!(
            action,
            Some(Action::Issue(IssueAction::ExecuteEdit { ref id })) if id == "bd-1"
        ));
        assert!(view.execute_modal.is_none());
    }

    #[test]
    fn update_status_targets_highlighted_row_when_detail_is_loaded_for_another_issue() {
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![
            issue("bd-A", "task", Vec::new()),
            issue("bd-B", "task", Vec::new()),
        ]);
        view.issue_focus = IssueFocus::Loaded {
            id: "bd-A".into(),
            issue: Box::new(issue_detail("bd-A", "A body")),
        };
        view.issues_panel.select_next(1, view.tracked_issues.len());

        let action = view.update_status("in_progress", false);

        assert!(matches!(
            action,
            Some(Action::Issue(IssueAction::UpdateStatus { ref id, .. })) if id == "bd-B"
        ));
    }

    #[test]
    fn graph_cache_evicts_oldest_entries_after_32_insertions() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();

        for idx in 0..33 {
            let id = format!("bd-{idx}");
            view.graph_loading = Some(id.clone());
            view.handle_spur_event(&subgraph_loaded_event(&id, vec![graph_node(&id)]), &ctx);
        }

        assert_eq!(view.graph_cache.len(), 32);
        assert!(!view.graph_cache.contains_key("bd-0"));
        assert!(view.graph_cache.contains_key("bd-1"));
        assert!(view.graph_cache.contains_key("bd-32"));
    }

    #[test]
    fn graph_cache_epoch_drops_stale_response() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();

        let action = view.request_graph_if_needed("bd-root".into());
        assert!(matches!(
            action,
            Some(Action::GetIssueGraph { ref id }) if id == "bd-root"
        ));

        view.handle_spur_event(
            &SpurEvent::now(spur_acp::SpurEventBody::IssueUpdated {
                source: "beads".into(),
                id: "bd-root".into(),
                status: Some("closed".into()),
                assignee: None,
            }),
            &ctx,
        );
        view.handle_spur_event(
            &subgraph_loaded_event("bd-root", vec![graph_node("bd-root")]),
            &ctx,
        );

        assert!(!view.graph_cache.contains_key("bd-root"));
    }

    #[test]
    fn graph_cache_epoch_keeps_fresh_response() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();

        let action = view.request_graph_if_needed("bd-root".into());
        assert!(matches!(
            action,
            Some(Action::GetIssueGraph { ref id }) if id == "bd-root"
        ));

        view.handle_spur_event(
            &subgraph_loaded_event("bd-root", vec![graph_node("bd-root")]),
            &ctx,
        );

        assert!(view.graph_cache.contains_key("bd-root"));
    }

    #[test]
    fn detail_cache_epoch_drops_stale_response() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();

        view.open_external_detail("bd-root".into(), OpenMode::FocusText);

        view.handle_spur_event(
            &SpurEvent::now(spur_acp::SpurEventBody::IssueUpdated {
                source: "beads".into(),
                id: "bd-root".into(),
                status: Some("closed".into()),
                assignee: None,
            }),
            &ctx,
        );

        view.handle_spur_event(
            &SpurEvent::now(spur_acp::SpurEventBody::IssueDetailFetched {
                requested_id: "bd-root".into(),
                issue: issue_detail_event("bd-root"),
            }),
            &ctx,
        );

        assert!(matches!(view.issue_focus, IssueFocus::None));
    }

    #[test]
    fn detail_cache_epoch_keeps_fresh_response() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();

        view.open_external_detail("bd-root".into(), OpenMode::FocusText);

        view.handle_spur_event(
            &SpurEvent::now(spur_acp::SpurEventBody::IssueDetailFetched {
                requested_id: "bd-root".into(),
                issue: issue_detail_event("bd-root"),
            }),
            &ctx,
        );

        assert!(matches!(
            view.issue_focus,
            IssueFocus::Loaded { ref id, .. } if id == "bd-root"
        ));
    }

    #[test]
    fn issue_updated_invalidates_graph_cache_entries_containing_updated_issue() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();

        view.graph_loading = Some("bd-root".into());
        view.handle_spur_event(
            &subgraph_loaded_event(
                "bd-root",
                vec![graph_node("bd-root"), graph_node("bd-child")],
            ),
            &ctx,
        );
        view.graph_loading = Some("bd-other".into());
        view.handle_spur_event(
            &subgraph_loaded_event("bd-other", vec![graph_node("bd-other")]),
            &ctx,
        );

        view.handle_spur_event(
            &SpurEvent::now(spur_acp::SpurEventBody::IssueUpdated {
                source: "beads".into(),
                id: "bd-child".into(),
                status: Some("closed".into()),
                assignee: None,
            }),
            &ctx,
        );

        assert!(!view.graph_cache.contains_key("bd-root"));
        assert!(view.graph_cache.contains_key("bd-other"));
    }

    #[test]
    fn issue_updated_invalidates_cache_when_issue_is_only_edge_endpoint() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();

        view.graph_loading = Some("bd-root".into());
        view.handle_spur_event(
            &subgraph_loaded_event_with_edges(
                "bd-root",
                vec![graph_node("bd-root")],
                vec![graph_edge("bd-other", "bd-root")],
            ),
            &ctx,
        );

        view.handle_spur_event(
            &SpurEvent::now(spur_acp::SpurEventBody::IssueUpdated {
                source: "beads".into(),
                id: "bd-other".into(),
                status: Some("closed".into()),
                assignee: None,
            }),
            &ctx,
        );

        assert!(!view.graph_cache.contains_key("bd-root"));
    }

    #[test]
    fn v_from_list_mode_arms_graph_mode_for_detail_load() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![issue("bd-1", "bug", Vec::new())]);

        let action = view.handle_key(key(KeyCode::Char('v')), &ctx);

        assert!(matches!(
            action,
            Some(Action::Issue(IssueAction::ViewDetail { ref id })) if id == "bd-1"
        ));
        assert_eq!(view.post_load_mode, Some(DetailMode::Graph));
    }

    #[test]
    fn rapid_navigation_prefetch_debounces_to_last_selected_issue() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![
            issue("bd-1", "bug", Vec::new()),
            issue("bd-2", "bug", Vec::new()),
            issue("bd-3", "bug", Vec::new()),
            issue("bd-4", "bug", Vec::new()),
        ]);

        view.handle_key(key(KeyCode::Char('j')), &ctx);
        view.handle_key(key(KeyCode::Char('j')), &ctx);
        view.handle_key(key(KeyCode::Char('j')), &ctx);

        assert!(
            view.take_pending_action().is_none(),
            "navigation prefetch should not dispatch synchronously"
        );

        let (id, scheduled_at) = view
            .pending_prefetch
            .take()
            .expect("rapid navigation should arm a pending prefetch");
        view.pending_prefetch = Some((
            id,
            scheduled_at - std::time::Duration::from_millis(PREFETCH_DEBOUNCE_MS),
        ));

        view.flush_due_prefetch();

        assert!(matches!(
            view.take_pending_action(),
            Some(Action::GetIssueGraph { ref id }) if id == "bd-4"
        ));
        assert!(
            view.take_pending_action().is_none(),
            "only the final stable selection should dispatch"
        );
    }

    #[test]
    fn g_and_shift_g_schedule_debounced_prefetch() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![
            issue("bd-1", "bug", Vec::new()),
            issue("bd-2", "bug", Vec::new()),
        ]);

        let first_action = view.handle_key(key(KeyCode::Char('g')), &ctx);

        assert!(first_action.is_none());
        assert!(view.pending_action.is_none());
        assert!(matches!(
            view.pending_prefetch,
            Some((ref id, _)) if id == "bd-1"
        ));

        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![
            issue("bd-1", "bug", Vec::new()),
            issue("bd-2", "bug", Vec::new()),
        ]);

        let last_action = view.handle_key(key(KeyCode::Char('G')), &ctx);

        assert!(last_action.is_none());
        assert!(view.pending_action.is_none());
        assert!(matches!(
            view.pending_prefetch,
            Some((ref id, _)) if id == "bd-2"
        ));
    }

    #[test]
    fn fast_scroll_modified_keys_move_issue_selection() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);

        let mut view = IssueBrowserView::new();
        seed_fast_scroll_issues(&mut view, 30);

        let action = view.handle_key(modified_key(KeyCode::Char('J'), KeyModifiers::SHIFT), &ctx);
        assert!(
            matches!(action, Some(Action::SelectNextBy(5))),
            "expected Shift-J to move down 5 rows, got {action:?}"
        );
        assert_eq!(view.selected_issue_id().as_deref(), Some("bd-05"));

        let action = view.handle_key(modified_key(KeyCode::Char('K'), KeyModifiers::SHIFT), &ctx);
        assert!(
            matches!(action, Some(Action::SelectPrevBy(5))),
            "expected Shift-K to move up 5 rows, got {action:?}"
        );
        assert_eq!(view.selected_issue_id().as_deref(), Some("bd-00"));

        rendered_text_at(&mut view, 100, 80);
        let action = view.handle_key(
            modified_key(KeyCode::Char('d'), KeyModifiers::CONTROL),
            &ctx,
        );
        assert!(
            matches!(action, Some(Action::SelectNextBy(10))),
            "expected Ctrl-D to move down half of a 20-row panel, got {action:?}"
        );
        assert_eq!(view.selected_issue_id().as_deref(), Some("bd-10"));

        let action = view.handle_key(
            modified_key(KeyCode::Char('u'), KeyModifiers::CONTROL),
            &ctx,
        );
        assert!(
            matches!(action, Some(Action::SelectPrevBy(10))),
            "expected Ctrl-U to move up half of a 20-row panel, got {action:?}"
        );
        assert_eq!(view.selected_issue_id().as_deref(), Some("bd-00"));
    }

    #[test]
    fn page_keys_target_list_unless_detail_is_loaded() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);

        let mut list_view = IssueBrowserView::new();
        seed_fast_scroll_issues(&mut list_view, 30);
        rendered_text_at(&mut list_view, 100, 80);
        let before = list_view.selected_issue_id();

        let action = list_view.handle_key(key(KeyCode::PageDown), &ctx);

        assert!(
            matches!(action, Some(Action::SelectNextBy(_))),
            "expected PageDown without detail to move the list, got {action:?}"
        );
        assert_ne!(list_view.selected_issue_id(), before);

        let mut loading_view = IssueBrowserView::new();
        seed_fast_scroll_issues(&mut loading_view, 30);
        loading_view.issue_focus = IssueFocus::Loading { id: "bd-00".into() };
        rendered_text_at(&mut loading_view, 100, 80);
        let action = loading_view.handle_key(key(KeyCode::PageUp), &ctx);
        assert!(
            matches!(action, Some(Action::SelectPrevBy(_))),
            "expected PageUp while detail is loading to move the list, got {action:?}"
        );

        let mut detail_view = IssueBrowserView::new();
        seed_fast_scroll_issues(&mut detail_view, 30);
        let body = (1..=30)
            .map(|line| format!("body line {line:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        detail_view.issue_focus = IssueFocus::Loaded {
            id: "bd-00".into(),
            issue: Box::new(issue_detail("bd-00", &body)),
        };

        let before = rendered_text(&mut detail_view);
        assert!(
            before.contains("body line 01"),
            "detail should start at the top before PageDown:\n{before}"
        );

        let action = detail_view.handle_key(key(KeyCode::PageDown), &ctx);

        assert!(
            matches!(action, Some(Action::ScrollDown)),
            "expected PageDown with loaded detail to scroll detail, got {action:?}"
        );
        let after = rendered_text(&mut detail_view);
        assert!(
            after.contains("body line 11"),
            "detail should scroll by the existing 10-line PageDown behavior:\n{after}"
        );
    }

    #[test]
    fn graph_error_without_id_does_not_clear_armed_graph_state() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.graph_loading = Some("bd-1".into());
        view.post_load_mode = Some(DetailMode::Graph);
        view.pending_action = Some(Action::GetIssueGraph { id: "bd-1".into() });

        view.handle_spur_event(&graph_error_event(None), &ctx);

        assert_eq!(view.graph_loading.as_deref(), Some("bd-1"));
        assert_eq!(view.post_load_mode, Some(DetailMode::Graph));
        assert!(matches!(
            view.pending_action,
            Some(Action::GetIssueGraph { ref id }) if id == "bd-1"
        ));
        assert!(view.graph_error.is_none());
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
    fn execute_is_blocked_for_plan_backed_item() {
        let lineage = ExecutorLineage::new();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = IssueBrowserView::new();
        view.set_issues_for_test(vec![issue(
            "bd-1",
            "epic",
            vec!["spur:plan-id:plan-1".into(), "spur:plan-complete".into()],
        )]);

        let action = view.handle_key(key(KeyCode::Char('e')), &ctx);

        assert!(matches!(
            action,
            Some(Action::FlashHint { message })
                if message.contains("already has implementation plan")
                    && message.contains("press p")
        ));
    }
}
