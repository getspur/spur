use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, Paragraph},
    Frame,
};
use spur_acp::{SessionId, SpurEvent};
use spur_core::{ExecutorId, ExecutorNode, LifecycleState, TrackedPlan};

use crate::action::{Action, IssueAction};
use crate::theme::{resolve_token, ColorDepth, Theme};

use super::View;

#[derive(Debug)]
pub enum PlanInspectorMode {
    Browse,
    StreamPeek {
        executor_id: String,
        task_id: String,
        state: crate::components::stream_pane::StreamViewState,
    },
}

fn token(theme: &Theme, name: &str) -> Color {
    resolve_token(theme, name, ColorDepth::Truecolor)
}

pub struct PlanInspectorView {
    session_id: SessionId,
    pinned_plan_id: Option<String>,
    selected_task_id: Option<String>,
    blocker_cycle: Option<(String, usize)>,
    stacked_mode: bool,
    open_issue_id: Option<String>,
    issue_states: HashMap<String, TaskIssueState>,
    task_detail_scroll: usize,
    mode: PlanInspectorMode,
}

#[derive(Debug)]
enum TaskIssueState {
    Loading,
    Loaded(Box<spur_pm::Issue>),
    Error(String),
}

impl PlanInspectorView {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            pinned_plan_id: None,
            selected_task_id: None,
            blocker_cycle: None,
            stacked_mode: false,
            open_issue_id: None,
            issue_states: HashMap::new(),
            task_detail_scroll: 0,
            mode: PlanInspectorMode::Browse,
        }
    }

    pub fn new_for_plan(session_id: SessionId, plan_id: String) -> Self {
        Self {
            session_id,
            pinned_plan_id: Some(plan_id),
            selected_task_id: None,
            blocker_cycle: None,
            stacked_mode: false,
            open_issue_id: None,
            issue_states: HashMap::new(),
            task_detail_scroll: 0,
            mode: PlanInspectorMode::Browse,
        }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn mode(&self) -> &PlanInspectorMode {
        &self.mode
    }

    pub fn enter_stream_peek(&mut self, executor_id: String, task_id: String) {
        self.mode = PlanInspectorMode::StreamPeek {
            executor_id,
            task_id,
            state: crate::components::stream_pane::StreamViewState::new(),
        };
    }

    pub fn leave_stream_peek(&mut self) {
        self.mode = PlanInspectorMode::Browse;
    }

    #[allow(dead_code)]
    fn peek_state_mut(&mut self) -> Option<&mut crate::components::stream_pane::StreamViewState> {
        if let PlanInspectorMode::StreamPeek { state, .. } = &mut self.mode {
            Some(state)
        } else {
            None
        }
    }

    #[cfg(test)]
    fn set_selected_task_id_for_tests(&mut self, task_id: Option<String>) {
        self.set_selected_task_id(task_id);
    }

    #[cfg(test)]
    fn set_open_issue_id_for_tests(&mut self, id: Option<String>) {
        self.open_issue_id = id;
    }

    #[cfg(test)]
    fn selected_task_id_for_tests(&self) -> Option<String> {
        self.selected_task_id.clone()
    }

    #[cfg(test)]
    fn peek_state_mut_for_tests(
        &mut self,
    ) -> Option<&mut crate::components::stream_pane::StreamViewState> {
        self.peek_state_mut()
    }

    fn active_plan<'a>(&self, ctx: &'a super::ViewContext<'_>) -> Option<&'a TrackedPlan> {
        match self.pinned_plan_id.as_deref() {
            Some(plan_id) => ctx.plan_projection.plan(plan_id),
            None => ctx.plan_projection.current_for_session(&self.session_id),
        }
    }

    fn set_selected_task_id(&mut self, task_id: Option<String>) {
        self.blocker_cycle = None;
        self.set_selected_task_id_inner(task_id);
    }

    fn set_selected_task_id_from_blocker_jump(&mut self, task_id: Option<String>) {
        self.set_selected_task_id_inner(task_id);
    }

    fn set_selected_task_id_inner(&mut self, task_id: Option<String>) {
        let previous_task_id = self.selected_task_id.as_deref();
        let selection_changed = previous_task_id != task_id.as_deref();
        if selection_changed {
            self.open_issue_id = None;
            self.task_detail_scroll = 0;
            if previous_task_id.is_some()
                && matches!(self.mode, PlanInspectorMode::StreamPeek { .. })
            {
                self.mode = PlanInspectorMode::Browse;
            }
        }
        self.selected_task_id = task_id;
    }

    fn close_issue_detail(&mut self) {
        self.open_issue_id = None;
        self.task_detail_scroll = 0;
    }

    fn scroll_task_detail_up(&mut self, lines: usize) {
        self.task_detail_scroll = self.task_detail_scroll.saturating_sub(lines);
    }

    fn scroll_task_detail_down(&mut self, lines: usize) {
        self.task_detail_scroll = self.task_detail_scroll.saturating_add(lines);
    }

    fn scroll_task_detail_to_top(&mut self) {
        self.task_detail_scroll = 0;
    }

    fn scroll_task_detail_to_bottom(&mut self) {
        self.task_detail_scroll = usize::MAX;
    }

    fn selected_issue_id<'a>(&self, task: &'a spur_core::TrackedTask) -> Option<&'a str> {
        task.issue_id.as_deref()
    }

    fn toggle_issue_detail(&mut self, task: &spur_core::TrackedTask) -> Option<Action> {
        let issue_id = self.selected_issue_id(task)?;
        if self.open_issue_id.as_deref() == Some(issue_id) {
            self.close_issue_detail();
            return None;
        }

        self.open_issue_id = Some(issue_id.to_string());
        self.task_detail_scroll = 0;
        let needs_request = !matches!(
            self.issue_states.get(issue_id),
            Some(TaskIssueState::Loaded(_))
        );
        if needs_request {
            self.issue_states
                .insert(issue_id.to_string(), TaskIssueState::Loading);
            Some(Action::Issue(IssueAction::ViewDetail {
                id: issue_id.to_string(),
            }))
        } else {
            None
        }
    }

    fn issue_detail_for_selected(
        &self,
        task: &spur_core::TrackedTask,
    ) -> (Option<&spur_pm::Issue>, Option<&str>) {
        let issue_id = match self.selected_issue_id(task) {
            Some(id) => id,
            None => return (None, None),
        };

        if self.open_issue_id.as_deref() != Some(issue_id) {
            return (None, None);
        }

        match self.issue_states.get(issue_id) {
            Some(TaskIssueState::Loaded(issue)) => (Some(issue.as_ref()), None),
            Some(TaskIssueState::Loading) => (None, Some("Loading issue detail...")),
            Some(TaskIssueState::Error(error)) => (None, Some(error.as_str())),
            None => (None, None),
        }
    }

    fn detail_event_to_issue(event: &spur_acp::IssueDetailEvent) -> spur_pm::Issue {
        spur_pm::Issue {
            id: event.id.clone(),
            source: match event.source.as_str() {
                "github" => spur_pm::PmSource::GitHub,
                "linear" => spur_pm::PmSource::Linear,
                "plane" => spur_pm::PmSource::Plane,
                _ => spur_pm::PmSource::Beads,
            },
            title: event.title.clone(),
            status: event.status.clone(),
            priority: event.priority,
            issue_type: event.issue_type.clone(),
            assignee: event.assignee.clone(),
            due_at: event.due_at,
            blocked_by: event.blocked_by.clone(),
            labels: event.labels.clone(),
            url: event.url.clone(),
            body: event.body.clone(),
            created_at: event.created_at,
            updated_at: event.updated_at,
            external_ref: None,
            source_system: None,
            source_repo: None,
        }
    }

    fn ensure_selection(&mut self, plan: &TrackedPlan) {
        let selected_exists = self
            .selected_task_id
            .as_ref()
            .and_then(|task_id| plan.task(task_id))
            .is_some();
        if selected_exists {
            return;
        }
        self.set_selected_task_id(
            crate::components::plan_stage_board::stage_grouped_tasks(plan)
                .first()
                .map(|task| task.task_id.clone()),
        );
    }

    fn selected_task<'a>(&self, plan: &'a TrackedPlan) -> Option<&'a spur_core::TrackedTask> {
        self.selected_task_id
            .as_ref()
            .and_then(|task_id| plan.task(task_id))
    }

    fn current_stage(&self, plan: &TrackedPlan) -> usize {
        self.selected_task(plan)
            .map(|task| task.stage_idx)
            .unwrap_or(0)
    }

    fn tasks_in_stage(plan: &TrackedPlan, stage_idx: usize) -> Vec<&spur_core::TrackedTask> {
        plan.tasks
            .iter()
            .filter(|task| task.stage_idx == stage_idx)
            .collect()
    }

    fn max_stage(plan: &TrackedPlan) -> usize {
        plan.tasks
            .iter()
            .map(|task| task.stage_idx)
            .max()
            .unwrap_or(0)
    }

    fn select_first_in_stage(&mut self, plan: &TrackedPlan, stage_idx: usize) {
        self.set_selected_task_id(
            Self::tasks_in_stage(plan, stage_idx)
                .first()
                .map(|task| task.task_id.clone()),
        );
    }

    fn move_lane(&mut self, plan: &TrackedPlan, delta: isize) {
        self.close_issue_detail();
        let current = self.current_stage(plan) as isize;
        let next = (current + delta).clamp(0, Self::max_stage(plan) as isize) as usize;
        self.select_first_in_stage(plan, next);
    }

    fn move_task(&mut self, plan: &TrackedPlan, delta: isize) {
        self.close_issue_detail();
        let tasks: Vec<_> = if self.stacked_mode {
            crate::components::plan_stage_board::stage_grouped_tasks(plan)
        } else {
            let stage_idx = self.current_stage(plan);
            Self::tasks_in_stage(plan, stage_idx)
        };
        if tasks.is_empty() {
            return;
        }
        let current_idx = self
            .selected_task(plan)
            .and_then(|task| {
                tasks
                    .iter()
                    .position(|candidate| candidate.task_id == task.task_id)
            })
            .unwrap_or(0) as isize;
        let next = (current_idx + delta).clamp(0, tasks.len() as isize - 1) as usize;
        self.set_selected_task_id(Some(tasks[next].task_id.clone()));
    }

    fn jump_lane_start(&mut self, plan: &TrackedPlan) {
        self.select_first_in_stage(plan, self.current_stage(plan));
    }

    fn jump_lane_end(&mut self, plan: &TrackedPlan) {
        let stage_idx = self.current_stage(plan);
        if let Some(task) = Self::tasks_in_stage(plan, stage_idx).last() {
            self.set_selected_task_id(Some(task.task_id.clone()));
        }
    }

    fn jump_to_next_blocker(&mut self, plan: &TrackedPlan) -> Option<Action> {
        let selected_id = self.selected_task_id.as_deref()?;
        let origin_id = self
            .blocker_cycle
            .as_ref()
            .and_then(|(origin_id, _)| {
                let origin = plan.task(origin_id)?;
                let selected_is_origin = selected_id == origin_id;
                let selected_is_blocker = origin
                    .blocked_by
                    .iter()
                    .any(|blocker_id| blocker_id == selected_id);
                (selected_is_origin || selected_is_blocker).then(|| origin_id.clone())
            })
            .unwrap_or_else(|| selected_id.to_string());
        let origin = match plan.task(&origin_id) {
            Some(task) => task,
            None => {
                self.blocker_cycle = None;
                return Some(Action::FlashHint {
                    message: "Selected task is no longer in the current plan".into(),
                });
            }
        };
        if origin.blocked_by.is_empty() {
            self.blocker_cycle = None;
            return Some(Action::FlashHint {
                message: "Selected task has no blockers".into(),
            });
        }

        let next_idx = self
            .blocker_cycle
            .as_ref()
            .filter(|(cycle_origin, _)| cycle_origin == &origin_id)
            .map(|(_, idx)| *idx)
            .unwrap_or(0);
        let blocker_id = origin.blocked_by[next_idx % origin.blocked_by.len()].clone();
        self.blocker_cycle = Some((origin_id, next_idx + 1));
        if plan.task(&blocker_id).is_some() {
            self.set_selected_task_id_from_blocker_jump(Some(blocker_id));
            None
        } else {
            Some(Action::FlashHint {
                message: format!("Blocker {blocker_id} is not in the current plan"),
            })
        }
    }
}

impl PlanInspectorView {
    pub fn render_with_worker_streams(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        worker_streams: &mut crate::worker_streams::WorkerStreams,
        ctx: &super::ViewContext,
    ) {
        <Self as View>::render(self, frame, area, ctx);

        if self.stream_peek_executor_is_stale(ctx) {
            self.leave_stream_peek();
            return;
        }

        if let PlanInspectorMode::StreamPeek {
            executor_id,
            task_id,
            state,
        } = &mut self.mode
        {
            let trace = worker_streams.get_mut(executor_id);
            let node = ctx.lineage.node(&ExecutorId(executor_id.clone()));
            Self::render_peek_overlay(frame, area, executor_id, task_id, node, trace, state);
        }
    }

    fn stream_peek_executor_is_stale(&self, ctx: &super::ViewContext) -> bool {
        let PlanInspectorMode::StreamPeek {
            executor_id,
            task_id,
            ..
        } = &self.mode
        else {
            return false;
        };

        let Some(task) = self
            .active_plan(ctx)
            .and_then(|plan| plan.task(task_id.as_str()))
        else {
            return true;
        };

        let current_executor_id = task.delegation_id.as_ref().and_then(|delegation_id| {
            ctx.lineage
                .executor_id_for_delegation(&spur_acp::domain::delegation::DelegationId(
                    delegation_id.clone(),
                ))
        });

        current_executor_id
            .map(|current| current.0 != *executor_id)
            .unwrap_or(true)
    }

    pub fn handle_key_with_worker_streams(
        &mut self,
        key: KeyEvent,
        worker_streams: &mut crate::worker_streams::WorkerStreams,
        ctx: &super::ViewContext,
    ) -> Option<Action> {
        let _ = worker_streams;

        if let PlanInspectorMode::StreamPeek { .. } = &self.mode {
            if let Some(action) = self.handle_peek_key(key) {
                return Some(action);
            }
            return None;
        }

        if let Some(action) = self.maybe_handle_open_peek(key, ctx) {
            return Some(action);
        }

        <Self as View>::handle_key(self, key, ctx)
    }

    fn render_peek_overlay(
        frame: &mut Frame,
        parent_area: Rect,
        executor_id: &str,
        task_id: &str,
        node: Option<&ExecutorNode>,
        trace: Option<&mut crate::components::react_trace::ReactTrace>,
        state: &mut crate::components::stream_pane::StreamViewState,
    ) {
        let area = if parent_area.width >= 60 {
            let width =
                ((parent_area.width as u32 * 80 / 100).max(60) as u16).min(parent_area.width);
            let height =
                ((parent_area.height as u32 * 60 / 100).max(8) as u16).min(parent_area.height);
            let x = parent_area.x + parent_area.width.saturating_sub(width) / 2;
            let y = parent_area.y + parent_area.height.saturating_sub(height) / 2;
            Rect::new(x, y, width, height)
        } else {
            parent_area
        };

        frame.render_widget(Clear, area);

        let title_name = node
            .map(|node| node.agent.as_str())
            .filter(|agent| !agent.is_empty())
            .unwrap_or(executor_id);
        let title_left = format!("stream: {title_name} ({task_id})");
        let title_right =
            node.and_then(|node| is_terminal_phase(node.phase).then_some("[completed]"));
        let bottom_hint = "[esc] close · [j/k] scroll · [f] follow";

        crate::components::stream_pane::render_stream(
            frame,
            area,
            &title_left,
            title_right,
            Some(bottom_hint),
            trace,
            state,
        );
    }

    fn handle_peek_key(&mut self, key: KeyEvent) -> Option<Action> {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
                self.leave_stream_peek();
                None
            }
            (KeyCode::Char('S'), m) if m == KeyModifiers::SHIFT || m == KeyModifiers::NONE => {
                let executor_id = match &self.mode {
                    PlanInspectorMode::StreamPeek { executor_id, .. } => executor_id.clone(),
                    _ => return None,
                };
                self.leave_stream_peek();
                Some(Action::FocusWorkerInDashboard {
                    executor_id,
                    tab: crate::components::detail_pane::DetailTab::Stream,
                })
            }
            _ => {
                let state = self.peek_state_mut()?;
                match (key.code, key.modifiers) {
                    (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
                        state.scroll_down_by(1);
                        None
                    }
                    (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
                        state.scroll_up_by(1);
                        None
                    }
                    (KeyCode::Char('g'), KeyModifiers::NONE) => {
                        state.scroll_to_top();
                        None
                    }
                    (KeyCode::Char('G'), _) => {
                        state.scroll_to_bottom();
                        None
                    }
                    (KeyCode::Char('f'), KeyModifiers::NONE) => {
                        state.toggle_follow();
                        None
                    }
                    _ => None,
                }
            }
        }
    }

    fn maybe_handle_open_peek(
        &mut self,
        key: KeyEvent,
        ctx: &super::ViewContext,
    ) -> Option<Action> {
        if self.open_issue_id.is_some() {
            // The issue detail overlay owns modal focus; ignore peek shortcuts here
            // and let the base view consume or ignore the key without stacking modals.
            return None;
        }

        let plan = self.active_plan(ctx)?;
        let task = self.selected_task(plan)?;
        let task_id = task.task_id.clone();
        let tab = crate::components::detail_pane::DetailTab::Stream;

        let executor_id = task.delegation_id.as_ref().and_then(|did| {
            ctx.lineage
                .executor_id_for_delegation(&spur_acp::domain::delegation::DelegationId(
                    did.clone(),
                ))
                .map(|eid| eid.0.clone())
        });

        match (key.code, key.modifiers) {
            (KeyCode::Char('s'), KeyModifiers::NONE) => match executor_id {
                Some(executor_id) => {
                    self.enter_stream_peek(executor_id, task_id);
                    None
                }
                None => Some(Action::FlashHint {
                    message: format!("Task {task_id} has no active worker yet"),
                }),
            },
            (KeyCode::Char('S'), m) if m == KeyModifiers::NONE || m == KeyModifiers::SHIFT => {
                match executor_id {
                    Some(executor_id) => Some(Action::FocusWorkerInDashboard { executor_id, tab }),
                    None => Some(Action::FlashHint {
                        message: format!("Task {task_id} has no active worker yet"),
                    }),
                }
            }
            _ => None,
        }
    }
}

fn is_terminal_phase(phase: LifecycleState) -> bool {
    matches!(
        phase,
        LifecycleState::Succeeded | LifecycleState::Failed | LifecycleState::Cancelled
    )
}

impl View for PlanInspectorView {
    fn handle_key(&mut self, key: KeyEvent, ctx: &super::ViewContext) -> Option<Action> {
        let key = super::normalize_macos_option(key);
        let plan = self.active_plan(ctx);
        if let Some(plan) = plan {
            self.ensure_selection(plan);

            if self.open_issue_id.is_some() {
                match key.code {
                    KeyCode::Char('k') | KeyCode::Up => {
                        self.scroll_task_detail_up(1);
                        return Some(Action::ScrollUp);
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        self.scroll_task_detail_down(1);
                        return Some(Action::ScrollDown);
                    }
                    KeyCode::Char('g') if key.modifiers.is_empty() => {
                        self.scroll_task_detail_to_top();
                        return Some(Action::ScrollToTop);
                    }
                    KeyCode::Char('G') => {
                        self.scroll_task_detail_to_bottom();
                        return Some(Action::ScrollToBottom);
                    }
                    KeyCode::PageUp => {
                        self.scroll_task_detail_up(10);
                        return Some(Action::ScrollUp);
                    }
                    KeyCode::PageDown => {
                        self.scroll_task_detail_down(10);
                        return Some(Action::ScrollDown);
                    }
                    _ => {}
                }
            }

            match key.code {
                KeyCode::Char('h') | KeyCode::Left => self.move_lane(plan, -1),
                KeyCode::Char('l') | KeyCode::Right => self.move_lane(plan, 1),
                KeyCode::Char('j') | KeyCode::Down => self.move_task(plan, 1),
                KeyCode::Char('k') | KeyCode::Up => self.move_task(plan, -1),
                KeyCode::Char('g') if key.modifiers.is_empty() => self.jump_lane_start(plan),
                KeyCode::Char('G') => self.jump_lane_end(plan),
                KeyCode::Char('b') if key.modifiers.is_empty() => {
                    return self.jump_to_next_blocker(plan);
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    if let Some(task) = self.selected_task(plan) {
                        if self.selected_issue_id(task).is_some() {
                            return self.toggle_issue_detail(task);
                        }
                        return Some(Action::FlashHint {
                            message: "No issue linked to selected task".to_string(),
                        });
                    }
                }
                KeyCode::Char('o') if key.modifiers.is_empty() => {
                    if let Some(epic_id) = plan.epic_id.as_ref() {
                        return Some(Action::OpenIssueInBacklog {
                            id: epic_id.clone(),
                        });
                    }
                    return Some(Action::FlashHint {
                        message: "No source work item linked to this implementation plan snapshot"
                            .into(),
                    });
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc => {
                if self.open_issue_id.is_some() {
                    self.close_issue_detail();
                    None
                } else {
                    Some(Action::NavigateBack)
                }
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(Action::NavigateBack)
            }
            _ => None,
        }
    }

    fn handle_spur_event(&mut self, event: &SpurEvent, _ctx: &super::ViewContext) {
        match &event.body {
            spur_acp::SpurEventBody::IssueDetailFetched {
                requested_id,
                issue,
            } => {
                self.issue_states.insert(
                    requested_id.clone(),
                    TaskIssueState::Loaded(Box::new(Self::detail_event_to_issue(issue))),
                );
            }
            spur_acp::SpurEventBody::IssueCommandError {
                operation,
                error,
                id: Some(id),
            } if operation == "GetIssueDetail" => {
                if let Some(TaskIssueState::Loading) = self.issue_states.get(id) {
                    self.issue_states
                        .insert(id.clone(), TaskIssueState::Error(error.clone()));
                }
            }
            spur_acp::SpurEventBody::IssueCommandError {
                operation,
                error,
                id: None,
            } if operation == "GetIssueDetail" => {
                if let Some(open_issue_id) = self.open_issue_id.as_ref() {
                    let loading_count = self
                        .issue_states
                        .values()
                        .filter(|state| matches!(state, TaskIssueState::Loading))
                        .count();
                    if loading_count == 1
                        && matches!(
                            self.issue_states.get(open_issue_id),
                            Some(TaskIssueState::Loading)
                        )
                    {
                        self.issue_states
                            .insert(open_issue_id.clone(), TaskIssueState::Error(error.clone()));
                    }
                }
            }
            _ => {}
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &super::ViewContext) {
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);
        self.stacked_mode = area.width < 90;

        let selected_task_has_issue = if let Some(plan) = self.active_plan(ctx) {
            self.ensure_selection(plan);
            let selected = self.selected_task(plan);
            let selected_stage_idx = selected.map(|t| t.stage_idx).unwrap_or(0);
            let live_node =
                selected
                    .and_then(|task| task.issue_id.as_deref())
                    .and_then(|issue_id| {
                        crate::components::plan_stage_board::preferred_live_node(
                            ctx.lineage,
                            issue_id,
                        )
                    });
            let issue_detail = selected
                .as_ref()
                .map(|task| self.issue_detail_for_selected(task))
                .unwrap_or((None, None));

            // ── Header ──────────────────────────────────────────────────────
            let header_rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(chunks[0]);
            let header_cols =
                Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
                    .split(header_rows[0]);

            let status_color = plan_status_color(ctx.theme, &plan.status);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!(" Plan: {} ", plan.plan_id),
                        Style::default()
                            .fg(token(ctx.theme, "plan_inspector.title.fg"))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" {} ", plan.status.to_uppercase()),
                        Style::default()
                            .fg(status_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("  {} ", plan.progress)),
                ])),
                header_cols[0],
            );

            let total_tasks = plan.counts.pending
                + plan.counts.ready
                + plan.counts.dispatched
                + plan.counts.awaiting_review
                + plan.counts.approved
                + plan.counts.rejected
                + plan.counts.failed
                + plan.counts.cancelled;
            let gauge_ratio = if total_tasks == 0 {
                0.0
            } else {
                plan.counts.approved as f64 / total_tasks as f64
            };
            frame.render_widget(
                Gauge::default()
                    .ratio(gauge_ratio.clamp(0.0, 1.0))
                    .label(format!("{} / {} done", plan.counts.approved, total_tasks))
                    .gauge_style(
                        Style::default().fg(token(ctx.theme, "plan_inspector.gauge.fill.fg")),
                    )
                    .style(Style::default().fg(token(ctx.theme, "plan_inspector.gauge.track.fg"))),
                header_cols[1],
            );

            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        " next: ",
                        Style::default().fg(token(ctx.theme, "plan_inspector.label.fg")),
                    ),
                    Span::raw(truncate_display(
                        &operator_summary(plan, ctx.lineage),
                        area.width.saturating_sub(8) as usize,
                    )),
                ])),
                header_rows[1],
            );

            let epic_text = plan
                .epic_id
                .as_deref()
                .map(|epic_id| format!("work item: {epic_id}"))
                .unwrap_or_else(|| "work item: --".into());
            let owner_text = plan
                .owner_brain_session_id
                .as_deref()
                .map(|owner| format!("owner: {owner}"))
                .unwrap_or_else(|| "owner: --".into());
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        " source: ",
                        Style::default().fg(token(ctx.theme, "plan_inspector.label.fg")),
                    ),
                    Span::raw(epic_text),
                    Span::styled(
                        "  ",
                        Style::default().fg(token(ctx.theme, "plan_inspector.label.fg")),
                    ),
                    Span::raw(owner_text),
                ])),
                header_rows[2],
            );

            if !self.stacked_mode {
                let [board_area, detail_area] =
                    Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                        .areas(chunks[1]);
                if let Some(selected_task_id) = self.selected_task_id.as_deref() {
                    crate::components::plan_stage_board::render_stage_board(
                        frame,
                        board_area,
                        plan,
                        selected_task_id,
                        selected_stage_idx,
                        ctx.lineage,
                        ctx.theme,
                    );
                }
                if let Some(task) = selected {
                    crate::components::plan_task_detail::render_task_detail(
                        frame,
                        detail_area,
                        plan,
                        task,
                        live_node,
                        issue_detail.0,
                        issue_detail.1,
                        self.task_detail_scroll,
                        ctx.theme,
                    );
                } else {
                    frame.render_widget(
                        Paragraph::new("No task selected")
                            .block(Block::default().borders(Borders::ALL).title("Task detail")),
                        detail_area,
                    );
                }
            } else {
                let [board_area, selected_area, detail_area] = Layout::vertical([
                    Constraint::Percentage(55),
                    Constraint::Length(1),
                    Constraint::Percentage(45),
                ])
                .areas(chunks[1]);
                if let Some(selected_task_id) = self.selected_task_id.as_deref() {
                    crate::components::plan_stage_board::render_stacked_stage_groups(
                        frame,
                        board_area,
                        plan,
                        selected_task_id,
                        ctx.lineage,
                        ctx.theme,
                    );
                }
                frame.render_widget(
                    Paragraph::new(format!(
                        "Selected: {}",
                        selected
                            .map(|task| task.task_name.as_str())
                            .unwrap_or("(none)")
                    )),
                    selected_area,
                );
                if let Some(task) = selected {
                    crate::components::plan_task_detail::render_task_detail(
                        frame,
                        detail_area,
                        plan,
                        task,
                        live_node,
                        issue_detail.0,
                        issue_detail.1,
                        self.task_detail_scroll,
                        ctx.theme,
                    );
                }
            }

            selected.and_then(|task| task.issue_id.as_deref()).is_some()
        } else {
            let message = self
                .pinned_plan_id
                .as_ref()
                .map(|plan_id| {
                    format!("No tracked implementation plan snapshot for {plan_id} yet.")
                })
                .unwrap_or_else(|| "No tracked plan for this session yet.".into());
            frame.render_widget(
                Paragraph::new(message).block(Block::default().borders(Borders::ALL)),
                chunks[1],
            );
            false
        };

        let enter_hint = if self.open_issue_id.is_some() {
            "Enter: close issue detail"
        } else if selected_task_has_issue {
            "Enter: issue detail"
        } else {
            "Enter: no linked issue"
        };
        let scroll_hint = if self.open_issue_id.is_some() {
            "  j/k: line scroll  g/G: top/btm  PgUp/PgDn: page"
        } else {
            ""
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                format!(
                    " h/l: lane  j/k: task  b: blocker  {}  o: work item  g/G: ends  Alt+P/Esc: close {}",
                    enter_hint, scroll_hint
                ),
                Style::default().fg(token(ctx.theme, "plan_inspector.footer_hint.fg")),
            )])),
            chunks[2],
        );
    }

    fn tick(&mut self) {}
}

fn plan_status_color(theme: &Theme, status: &str) -> Color {
    let key = match status {
        "running" | "active" => "plan_inspector.status.running.fg",
        "approved" | "completed" | "success" => "plan_inspector.status.success.fg",
        "failed" | "rejected" | "error" => "plan_inspector.status.failure.fg",
        "cancelled" => "plan_inspector.status.cancelled.fg",
        _ => "plan_inspector.status.unknown.fg",
    };
    token(theme, key)
}

fn operator_summary(plan: &TrackedPlan, lineage: &spur_core::ExecutorLineage) -> String {
    let mut fragments = Vec::new();
    let task_by_id: HashMap<&str, &spur_core::TrackedTask> = plan
        .tasks
        .iter()
        .map(|task| (task.task_id.as_str(), task))
        .collect();
    for task in &plan.tasks {
        if task.blocked_by.is_empty() {
            continue;
        }
        for blocker_id in &task.blocked_by {
            let Some(blocker) = task_by_id.get(blocker_id.as_str()) else {
                continue;
            };
            if matches!(blocker.status.as_str(), "rejected" | "failed" | "error") {
                fragments.push(format!(
                    "{} {} {} ago",
                    blocker.task_id,
                    operator_status_label(&blocker.status),
                    format_age(
                        plan.updated_at
                            .elapsed()
                            .ok()
                            .map(|d| d.as_secs())
                            .unwrap_or(0)
                    )
                ));
            }
        }
    }

    for task in &plan.tasks {
        if !matches!(task.status.as_str(), "dispatched" | "running" | "active") {
            continue;
        }
        let elapsed_secs = task
            .issue_id
            .as_deref()
            .and_then(|issue_id| {
                crate::components::plan_stage_board::preferred_live_node(lineage, issue_id)
            })
            .map(|node| node.elapsed_secs())
            .unwrap_or_else(|| {
                plan.updated_at
                    .elapsed()
                    .ok()
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            });
        if elapsed_secs >= 10 * 60 {
            fragments.push(format!(
                "{} running {}",
                task.task_id,
                format_age(elapsed_secs)
            ));
        }
    }

    for task in &plan.tasks {
        if task.attempt > 1 {
            fragments.push(format!(
                "{} retry {}/{}",
                task.task_id, task.attempt, task.max_attempts
            ));
        }
    }

    if fragments.is_empty() {
        plan.next_action.clone()
    } else {
        format!("risk: {}", fragments.join(" · "))
    }
}

fn operator_status_label(status: &str) -> &str {
    match status {
        "failed" | "error" => "error",
        other => other,
    }
}

fn format_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 60 * 60 {
        format!("{}m", secs / 60)
    } else if secs < 48 * 60 * 60 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

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

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_acp::{
        LifecycleState, PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask, SessionId, SpurEvent,
        SpurEventBody,
    };
    use spur_core::{ExecutorLineage, PlanProjectionStore, SessionSynopsisProjection};

    use crate::action::Action;
    use crate::app::BrainStatus;
    use crate::views::{View, ViewContext};

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_char(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn key_shift(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)
    }

    fn key_code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn ctx<'a>(
        lineage: &'a ExecutorLineage,
        projection: &'a PlanProjectionStore,
        synopsis: &'a SessionSynopsisProjection,
    ) -> ViewContext<'a> {
        ViewContext {
            lineage,
            plan_projection: projection,
            synopsis,
            brain_status: &BrainStatus::Idle,
            license_badge: None,
            flag_summary: None,
            tombstone: None,
            transient_hint_override: None,
            theme: crate::theme::fallback_theme(),
        }
    }

    fn view_context_for_tests<'a>(
        lineage: &'a ExecutorLineage,
        projection: &'a PlanProjectionStore,
    ) -> ViewContext<'a> {
        let synopsis = Box::leak(Box::new(SessionSynopsisProjection::new()));
        ctx(lineage, projection, synopsis)
    }

    fn projection_with_epic(session_id: &SessionId) -> PlanProjectionStore {
        let mut projection = PlanProjectionStore::new();
        projection.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
            session_id: session_id.clone(),
            snapshot: Box::new(PlanSnapshot {
                plan_id: "plan-1".into(),
                epic_id: Some("bd-epic".into()),
                status: "running".into(),
                progress: "0/1 done".into(),
                next_action: "dispatch first stage".into(),
                ready_to_merge: false,
                counts: PlanSnapshotCounts {
                    pending: 1,
                    ..Default::default()
                },
                tasks: vec![PlanSnapshotTask {
                    task_id: "t-12".into(),
                    task_name: "Stage A".into(),
                    agent: "codex".into(),
                    issue_id: Some("bd-epic.1".into()),
                    issue_title: None,
                    status: "pending".into(),
                    attempt: 0,
                    max_attempts: 3,
                    depends_on: Vec::new(),
                    blocked_by: Vec::new(),
                    unblocks: Vec::new(),
                    summary: None,
                    feedback: None,
                    error: None,
                    worker_branch: None,
                    delegation_id: None,
                    diff_summary: None,
                    mutation_id: None,
                    superseded_by: Vec::new(),
                    next_action: "wait".into(),
                }],
                owner_brain_session_id: Some(session_id.0.clone()),
                owner_token: None,
                owner_acquired_at: None,
            }),
        }));
        projection
    }

    fn projection_with_epic_and_worker(session_id: &SessionId) -> PlanProjectionStore {
        projection_with_epic_and_worker_for_delegation(session_id, "deleg-12")
    }

    fn projection_with_epic_and_worker_for_delegation(
        session_id: &SessionId,
        delegation_id: &str,
    ) -> PlanProjectionStore {
        let mut projection = PlanProjectionStore::new();
        projection.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
            session_id: session_id.clone(),
            snapshot: Box::new(PlanSnapshot {
                plan_id: "plan-1".into(),
                epic_id: Some("bd-epic".into()),
                status: "running".into(),
                progress: "0/1 done".into(),
                next_action: "watch worker".into(),
                ready_to_merge: false,
                counts: PlanSnapshotCounts {
                    dispatched: 1,
                    ..Default::default()
                },
                tasks: vec![PlanSnapshotTask {
                    task_id: "t-12".into(),
                    task_name: "Stage A".into(),
                    agent: "codex".into(),
                    issue_id: Some("bd-epic.1".into()),
                    issue_title: None,
                    status: "dispatched".into(),
                    attempt: 1,
                    max_attempts: 3,
                    depends_on: Vec::new(),
                    blocked_by: Vec::new(),
                    unblocks: Vec::new(),
                    summary: None,
                    feedback: None,
                    error: None,
                    worker_branch: None,
                    delegation_id: Some(delegation_id.into()),
                    diff_summary: None,
                    mutation_id: None,
                    superseded_by: Vec::new(),
                    next_action: "watch stream".into(),
                }],
                owner_brain_session_id: Some(session_id.0.clone()),
                owner_token: None,
                owner_acquired_at: None,
            }),
        }));
        projection
    }

    fn projection_with_two_running_tasks(session_id: &SessionId) -> PlanProjectionStore {
        let mut projection = PlanProjectionStore::new();
        projection.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
            session_id: session_id.clone(),
            snapshot: Box::new(PlanSnapshot {
                plan_id: "plan-1".into(),
                epic_id: Some("bd-epic".into()),
                status: "running".into(),
                progress: "0/2 done".into(),
                next_action: "watch workers".into(),
                ready_to_merge: false,
                counts: PlanSnapshotCounts {
                    dispatched: 2,
                    ..Default::default()
                },
                tasks: vec![
                    PlanSnapshotTask {
                        task_id: "t-12".into(),
                        task_name: "Stage A".into(),
                        agent: "codex".into(),
                        issue_id: Some("bd-epic.1".into()),
                        issue_title: None,
                        status: "dispatched".into(),
                        attempt: 1,
                        max_attempts: 3,
                        depends_on: Vec::new(),
                        blocked_by: Vec::new(),
                        unblocks: Vec::new(),
                        summary: None,
                        feedback: None,
                        error: None,
                        worker_branch: None,
                        delegation_id: Some("deleg-12".into()),
                        diff_summary: None,
                        mutation_id: None,
                        superseded_by: Vec::new(),
                        next_action: "watch stream".into(),
                    },
                    PlanSnapshotTask {
                        task_id: "t-13".into(),
                        task_name: "Stage B".into(),
                        agent: "codex".into(),
                        issue_id: Some("bd-epic.2".into()),
                        issue_title: None,
                        status: "dispatched".into(),
                        attempt: 1,
                        max_attempts: 3,
                        depends_on: Vec::new(),
                        blocked_by: Vec::new(),
                        unblocks: Vec::new(),
                        summary: None,
                        feedback: None,
                        error: None,
                        worker_branch: None,
                        delegation_id: Some("deleg-13".into()),
                        diff_summary: None,
                        mutation_id: None,
                        superseded_by: Vec::new(),
                        next_action: "watch stream".into(),
                    },
                ],
                owner_brain_session_id: Some(session_id.0.clone()),
                owner_token: None,
                owner_acquired_at: None,
            }),
        }));
        projection
    }

    fn lineage_with_worker_for_task(
        session_id: &SessionId,
        task_id: &str,
        worker_session: &str,
    ) -> ExecutorLineage {
        lineage_with_worker_for_task_and_delegation(session_id, task_id, worker_session, "deleg-12")
    }

    fn lineage_with_worker_for_task_and_delegation(
        session_id: &SessionId,
        task_id: &str,
        worker_session: &str,
        delegation_id: &str,
    ) -> ExecutorLineage {
        let mut lineage = ExecutorLineage::new();
        lineage.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
            agent: "codex".into(),
            session: SessionId(worker_session.into()),
            worktree: std::path::PathBuf::from("/tmp"),
        }));
        lineage.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
            from: session_id.clone(),
            to_agent: "codex".into(),
            task: task_id.into(),
            request_id: delegation_id.into(),
            delegation_plan: None,
            issue_id: Some("bd-epic.1".into()),
        }));
        lineage.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
            from: session_id.clone(),
            request_id: delegation_id.into(),
            executor_id: worker_session.into(),
        }));
        lineage
    }

    fn lineage_with_two_workers(session_id: &SessionId) -> ExecutorLineage {
        let mut lineage = ExecutorLineage::new();
        lineage.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
            agent: "codex".into(),
            session: SessionId("worker-session-1".into()),
            worktree: std::path::PathBuf::from("/tmp/worker-1"),
        }));
        lineage.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
            agent: "codex".into(),
            session: SessionId("worker-session-2".into()),
            worktree: std::path::PathBuf::from("/tmp/worker-2"),
        }));
        lineage.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
            from: session_id.clone(),
            to_agent: "codex".into(),
            task: "t-12".into(),
            request_id: "deleg-12".into(),
            delegation_plan: None,
            issue_id: Some("bd-epic.1".into()),
        }));
        lineage.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
            from: session_id.clone(),
            to_agent: "codex".into(),
            task: "t-13".into(),
            request_id: "deleg-13".into(),
            delegation_plan: None,
            issue_id: Some("bd-epic.2".into()),
        }));
        lineage.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
            from: session_id.clone(),
            request_id: "deleg-12".into(),
            executor_id: "worker-session-1".into(),
        }));
        lineage.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
            from: session_id.clone(),
            request_id: "deleg-13".into(),
            executor_id: "worker-session-2".into(),
        }));
        lineage
    }

    fn projection_with_blocker_cycle(session_id: &SessionId) -> PlanProjectionStore {
        let mut projection = PlanProjectionStore::new();
        projection.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
            session_id: session_id.clone(),
            snapshot: Box::new(PlanSnapshot {
                plan_id: "plan-1".into(),
                epic_id: Some("bd-epic".into()),
                status: "running".into(),
                progress: "0/3 done".into(),
                next_action: "resolve blockers".into(),
                ready_to_merge: false,
                counts: PlanSnapshotCounts {
                    pending: 1,
                    rejected: 1,
                    failed: 1,
                    ..Default::default()
                },
                tasks: vec![
                    PlanSnapshotTask {
                        task_id: "lint".into(),
                        task_name: "lint".into(),
                        agent: "codex".into(),
                        issue_id: Some("bd-lint".into()),
                        issue_title: None,
                        status: "rejected".into(),
                        attempt: 1,
                        max_attempts: 3,
                        depends_on: Vec::new(),
                        blocked_by: Vec::new(),
                        unblocks: vec!["ui".into()],
                        summary: None,
                        feedback: None,
                        error: None,
                        worker_branch: None,
                        delegation_id: None,
                        diff_summary: None,
                        mutation_id: None,
                        superseded_by: Vec::new(),
                        next_action: "fix lint".into(),
                    },
                    PlanSnapshotTask {
                        task_id: "api".into(),
                        task_name: "api".into(),
                        agent: "codex".into(),
                        issue_id: Some("bd-api".into()),
                        issue_title: None,
                        status: "failed".into(),
                        attempt: 2,
                        max_attempts: 3,
                        depends_on: Vec::new(),
                        blocked_by: Vec::new(),
                        unblocks: vec!["ui".into()],
                        summary: None,
                        feedback: None,
                        error: Some("build failed".into()),
                        worker_branch: None,
                        delegation_id: None,
                        diff_summary: None,
                        mutation_id: None,
                        superseded_by: Vec::new(),
                        next_action: "retry".into(),
                    },
                    PlanSnapshotTask {
                        task_id: "ui".into(),
                        task_name: "ui".into(),
                        agent: "codex".into(),
                        issue_id: Some("bd-ui".into()),
                        issue_title: None,
                        status: "pending".into(),
                        attempt: 0,
                        max_attempts: 3,
                        depends_on: vec!["lint".into(), "api".into()],
                        blocked_by: vec!["lint".into(), "api".into()],
                        unblocks: Vec::new(),
                        summary: None,
                        feedback: None,
                        error: None,
                        worker_branch: None,
                        delegation_id: None,
                        diff_summary: None,
                        mutation_id: None,
                        superseded_by: Vec::new(),
                        next_action: "wait".into(),
                    },
                ],
                owner_brain_session_id: Some(session_id.0.clone()),
                owner_token: None,
                owner_acquired_at: None,
            }),
        }));
        projection
    }

    #[test]
    fn new_view_starts_in_browse_mode() {
        let v = PlanInspectorView::new(SessionId("s".into()));
        assert!(matches!(v.mode(), PlanInspectorMode::Browse));
    }

    #[test]
    fn enter_stream_peek_sets_mode_and_initial_state() {
        let mut v = PlanInspectorView::new(SessionId("s".into()));
        v.enter_stream_peek("worker-session-1".into(), "t-12".into());
        match v.mode() {
            PlanInspectorMode::StreamPeek {
                executor_id,
                task_id,
                state,
            } => {
                assert_eq!(executor_id, "worker-session-1");
                assert_eq!(task_id, "t-12");
                assert!(state.is_following);
                assert_eq!(state.scroll_offset, 0);
            }
            _ => panic!("expected StreamPeek"),
        }
    }

    #[test]
    fn leave_stream_peek_returns_to_browse() {
        let mut v = PlanInspectorView::new(SessionId("s".into()));
        v.enter_stream_peek("w".into(), "t".into());
        v.leave_stream_peek();
        assert!(matches!(v.mode(), PlanInspectorMode::Browse));
    }

    #[test]
    fn render_with_worker_streams_does_not_panic_when_no_streams() {
        let session_id = SessionId("brain-1".into());
        let projection = projection_with_epic(&session_id);
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut ws = crate::worker_streams::WorkerStreams::new();
        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                view.render_with_worker_streams(frame, frame.area(), &mut ws, &ctx);
            })
            .unwrap();
    }

    #[test]
    fn peek_overlay_renders_in_60_col_terminal_without_panic() {
        let session_id = SessionId("brain-1".into());
        let projection = projection_with_epic_and_worker(&session_id);
        let lineage = lineage_with_worker_for_task(&session_id, "t-12", "worker-session-1");
        let ctx = view_context_for_tests(&lineage, &projection);

        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
        view.enter_stream_peek("worker-session-1".into(), "t-12".into());

        let mut ws = crate::worker_streams::WorkerStreams::new();
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                view.render_with_worker_streams(frame, frame.area(), &mut ws, &ctx);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let dump = format!("{:?}", buf);
        assert!(
            dump.contains("stream:"),
            "expected 'stream:' title in buffer"
        );
        assert!(dump.contains("t-12"), "expected task id in buffer");
    }

    #[test]
    fn peek_overlay_title_uses_agent_name_when_lineage_knows_executor() {
        let session_id = SessionId("brain-1".into());
        let projection = projection_with_epic_and_worker(&session_id);
        let lineage = lineage_with_worker_for_task(&session_id, "t-12", "worker-session-1");
        let ctx = view_context_for_tests(&lineage, &projection);

        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
        view.enter_stream_peek("worker-session-1".into(), "t-12".into());

        let mut ws = crate::worker_streams::WorkerStreams::new();
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                view.render_with_worker_streams(frame, frame.area(), &mut ws, &ctx);
            })
            .unwrap();

        let dump = format!("{:?}", terminal.backend().buffer());
        assert!(
            dump.contains("stream: codex (t-12)"),
            "expected peek title to use the agent name:\n{dump}"
        );
        assert!(
            !dump.contains("stream: worker-session-1 (t-12)"),
            "expected peek title not to expose the raw executor id:\n{dump}"
        );
    }

    #[test]
    fn peek_overlay_title_shows_completed_badge_for_terminal_executor() {
        let session_id = SessionId("brain-1".into());
        let projection = projection_with_epic_and_worker(&session_id);
        let mut lineage = lineage_with_worker_for_task(&session_id, "t-12", "worker-session-1");
        lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
            id: "worker-session-1".into(),
            phase: LifecycleState::Succeeded,
        }));
        let ctx = view_context_for_tests(&lineage, &projection);

        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
        view.enter_stream_peek("worker-session-1".into(), "t-12".into());

        let mut ws = crate::worker_streams::WorkerStreams::new();
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                view.render_with_worker_streams(frame, frame.area(), &mut ws, &ctx);
            })
            .unwrap();

        let dump = format!("{:?}", terminal.backend().buffer());
        assert!(
            dump.contains("[completed]"),
            "expected peek title to show completed badge:\n{dump}"
        );
    }

    #[test]
    fn peek_overlay_falls_back_to_fullscreen_below_60_cols() {
        let session_id = SessionId("brain-1".into());
        let projection = projection_with_epic_and_worker(&session_id);
        let lineage = lineage_with_worker_for_task(&session_id, "t-12", "worker-session-1");
        let ctx = view_context_for_tests(&lineage, &projection);

        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
        view.enter_stream_peek("worker-session-1".into(), "t-12".into());

        let mut ws = crate::worker_streams::WorkerStreams::new();
        let backend = ratatui::backend::TestBackend::new(40, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                view.render_with_worker_streams(frame, frame.area(), &mut ws, &ctx);
            })
            .unwrap();
    }

    #[test]
    fn o_opens_source_work_item_from_plan_inspector() {
        let session_id = SessionId("brain-1".into());
        let projection = projection_with_epic(&session_id);
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());

        let action = view.handle_key(key(KeyCode::Char('o')), &ctx);

        assert!(matches!(
            action,
            Some(Action::OpenIssueInBacklog { id }) if id == "bd-epic"
        ));
    }

    #[test]
    fn lowercase_s_enters_peek_when_task_has_worker() {
        let session_id = SessionId("brain-1".into());
        let projection = projection_with_epic_and_worker(&session_id);
        let lineage = lineage_with_worker_for_task(&session_id, "t-12", "worker-session-1");

        let ctx = view_context_for_tests(&lineage, &projection);
        let mut ws = crate::worker_streams::WorkerStreams::new();
        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
        view.set_selected_task_id_for_tests(Some("t-12".into()));

        let action = view.handle_key_with_worker_streams(key_char('s'), &mut ws, &ctx);

        assert!(matches!(view.mode(), PlanInspectorMode::StreamPeek { .. }));
        assert!(action.is_none());
    }

    #[test]
    fn s_does_not_open_peek_when_issue_overlay_is_active() {
        let session_id = SessionId("brain-1".into());
        let projection = projection_with_epic_and_worker(&session_id);
        let lineage = lineage_with_worker_for_task(&session_id, "t-12", "worker-session-1");

        let ctx = view_context_for_tests(&lineage, &projection);
        let mut ws = crate::worker_streams::WorkerStreams::new();
        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
        view.set_selected_task_id_for_tests(Some("t-12".into()));
        view.set_open_issue_id_for_tests(Some("bd-epic.1".into()));

        let action = view.handle_key_with_worker_streams(key_char('s'), &mut ws, &ctx);

        assert!(matches!(view.mode(), PlanInspectorMode::Browse));
        assert!(action.is_none());
    }

    #[test]
    fn lowercase_s_flashes_hint_when_task_has_no_worker() {
        let session_id = SessionId("brain-1".into());
        let projection = projection_with_epic(&session_id);
        let lineage = ExecutorLineage::new();

        let ctx = view_context_for_tests(&lineage, &projection);
        let mut ws = crate::worker_streams::WorkerStreams::new();
        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
        view.set_selected_task_id_for_tests(Some("t-12".into()));

        let action = view.handle_key_with_worker_streams(key_char('s'), &mut ws, &ctx);

        assert!(matches!(view.mode(), PlanInspectorMode::Browse));
        assert!(matches!(action, Some(Action::FlashHint { .. })));
    }

    #[test]
    fn shift_s_emits_focus_worker_action() {
        let session_id = SessionId("brain-1".into());
        let projection = projection_with_epic_and_worker(&session_id);
        let lineage = lineage_with_worker_for_task(&session_id, "t-12", "worker-session-1");

        let ctx = view_context_for_tests(&lineage, &projection);
        let mut ws = crate::worker_streams::WorkerStreams::new();
        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
        view.set_selected_task_id_for_tests(Some("t-12".into()));

        let action = view.handle_key_with_worker_streams(key_shift('S'), &mut ws, &ctx);

        match action {
            Some(Action::FocusWorkerInDashboard { executor_id, tab }) => {
                assert_eq!(executor_id, "worker-session-1");
                assert_eq!(tab, crate::components::detail_pane::DetailTab::Stream);
            }
            other => panic!("expected FocusWorkerInDashboard, got {:?}", other),
        }
    }

    #[test]
    fn esc_leaves_peek() {
        let session_id = SessionId("brain-1".into());
        let projection = projection_with_epic_and_worker(&session_id);
        let lineage = lineage_with_worker_for_task(&session_id, "t-12", "worker-session-1");
        let ctx = view_context_for_tests(&lineage, &projection);
        let mut ws = crate::worker_streams::WorkerStreams::new();

        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
        view.enter_stream_peek("worker-session-1".into(), "t-12".into());

        let action = view.handle_key_with_worker_streams(key_code(KeyCode::Esc), &mut ws, &ctx);

        assert!(matches!(view.mode(), PlanInspectorMode::Browse));
        assert!(action.is_none());
    }

    #[test]
    fn j_scrolls_peek_without_leaking_to_task_list() {
        let session_id = SessionId("brain-1".into());
        let projection = projection_with_epic_and_worker(&session_id);
        let lineage = lineage_with_worker_for_task(&session_id, "t-12", "worker-session-1");
        let ctx = view_context_for_tests(&lineage, &projection);
        let mut ws = crate::worker_streams::WorkerStreams::new();

        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
        let initial_selection = view.selected_task_id_for_tests();
        view.enter_stream_peek("worker-session-1".into(), "t-12".into());

        if let Some(state) = view.peek_state_mut_for_tests() {
            state.scroll_offset = 10;
            state.is_following = false;
        }

        let _ = view.handle_key_with_worker_streams(key_char('k'), &mut ws, &ctx);

        if let PlanInspectorMode::StreamPeek { state, .. } = view.mode() {
            assert_eq!(state.scroll_offset, 9);
        }
        assert_eq!(view.selected_task_id_for_tests(), initial_selection);
    }

    #[test]
    fn f_toggles_follow_in_peek() {
        let session_id = SessionId("brain-1".into());
        let projection = projection_with_epic_and_worker(&session_id);
        let lineage = lineage_with_worker_for_task(&session_id, "t-12", "worker-session-1");
        let ctx = view_context_for_tests(&lineage, &projection);
        let mut ws = crate::worker_streams::WorkerStreams::new();

        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
        view.enter_stream_peek("worker-session-1".into(), "t-12".into());

        let initial = match view.mode() {
            PlanInspectorMode::StreamPeek { state, .. } => state.is_following,
            _ => panic!(),
        };

        let _ = view.handle_key_with_worker_streams(key_char('f'), &mut ws, &ctx);

        if let PlanInspectorMode::StreamPeek { state, .. } = view.mode() {
            assert_eq!(state.is_following, !initial);
        }
    }

    #[test]
    fn peek_auto_closes_when_selected_task_changes() {
        let session_id = SessionId("brain-1".into());
        let projection = projection_with_two_running_tasks(&session_id);
        let lineage = lineage_with_two_workers(&session_id);
        let _ctx = view_context_for_tests(&lineage, &projection);

        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
        view.set_selected_task_id_for_tests(Some("t-12".into()));
        view.enter_stream_peek("worker-session-1".into(), "t-12".into());

        view.set_selected_task_id_for_tests(Some("t-13".into()));

        assert_eq!(view.selected_task_id_for_tests(), Some("t-13".into()));
        assert!(matches!(view.mode(), PlanInspectorMode::Browse));
    }

    #[test]
    fn peek_auto_closes_when_task_delegation_swaps_executor() {
        let session_id = SessionId("brain-1".into());
        let projection =
            projection_with_epic_and_worker_for_delegation(&session_id, "deleg-retry-1");
        let lineage = lineage_with_worker_for_task_and_delegation(
            &session_id,
            "t-12",
            "worker-session-2",
            "deleg-retry-1",
        );
        let ctx = view_context_for_tests(&lineage, &projection);
        let mut ws = crate::worker_streams::WorkerStreams::new();

        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
        view.set_selected_task_id_for_tests(Some("t-12".into()));
        view.enter_stream_peek("worker-session-1".into(), "t-12".into());

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                view.render_with_worker_streams(frame, frame.area(), &mut ws, &ctx);
            })
            .unwrap();

        assert!(matches!(view.mode(), PlanInspectorMode::Browse));
    }

    #[test]
    fn b_cycles_selection_through_selected_task_blockers() {
        let session_id = SessionId("brain-1".into());
        let projection = projection_with_blocker_cycle(&session_id);
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = PlanInspectorView::new_for_plan(session_id, "plan-1".into());
        let plan = projection.plan("plan-1").unwrap();

        view.set_selected_task_id(Some("ui".into()));
        assert!(view.handle_key(key(KeyCode::Char('b')), &ctx).is_none());
        assert_eq!(
            view.selected_task(plan).map(|task| task.task_id.as_str()),
            Some("lint")
        );

        assert!(view.handle_key(key(KeyCode::Char('b')), &ctx).is_none());
        assert_eq!(
            view.selected_task(plan).map(|task| task.task_id.as_str()),
            Some("api")
        );
    }

    #[test]
    fn b_flashes_hint_for_external_blocker() {
        let session_id = SessionId("brain-1".into());
        let mut projection = projection_with_blocker_cycle(&session_id);
        projection.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
            session_id: session_id.clone(),
            snapshot: Box::new(PlanSnapshot {
                plan_id: "external-plan".into(),
                epic_id: None,
                status: "running".into(),
                progress: "0/1 done".into(),
                next_action: "wait for external issue".into(),
                ready_to_merge: false,
                counts: PlanSnapshotCounts {
                    pending: 1,
                    ..Default::default()
                },
                tasks: vec![PlanSnapshotTask {
                    task_id: "ui".into(),
                    task_name: "ui".into(),
                    agent: "codex".into(),
                    issue_id: Some("bd-ui".into()),
                    issue_title: None,
                    status: "pending".into(),
                    attempt: 0,
                    max_attempts: 3,
                    depends_on: Vec::new(),
                    blocked_by: vec!["bd-external".into()],
                    unblocks: Vec::new(),
                    summary: None,
                    feedback: None,
                    error: None,
                    worker_branch: None,
                    delegation_id: None,
                    diff_summary: None,
                    mutation_id: None,
                    superseded_by: Vec::new(),
                    next_action: "wait".into(),
                }],
                owner_brain_session_id: Some(session_id.0.clone()),
                owner_token: None,
                owner_acquired_at: None,
            }),
        }));
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = PlanInspectorView::new_for_plan(session_id, "external-plan".into());

        assert!(matches!(
            view.handle_key(key(KeyCode::Char('b')), &ctx),
            Some(Action::FlashHint { message }) if message.contains("bd-external")
        ));
    }
}
