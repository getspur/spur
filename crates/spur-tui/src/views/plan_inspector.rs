use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph},
    Frame,
};
use spur_acp::{SessionId, SpurEvent};
use spur_core::TrackedPlan;

use crate::action::{Action, IssueAction};

use super::View;

pub struct PlanInspectorView {
    session_id: SessionId,
    selected_task_id: Option<String>,
    stacked_mode: bool,
    open_issue_id: Option<String>,
    issue_states: HashMap<String, TaskIssueState>,
}

#[derive(Debug)]
enum TaskIssueState {
    Loading,
    Loaded(spur_pm::Issue),
    Error(String),
}

impl PlanInspectorView {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            selected_task_id: None,
            stacked_mode: false,
            open_issue_id: None,
            issue_states: HashMap::new(),
        }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    fn set_selected_task_id(&mut self, task_id: Option<String>) {
        if self.selected_task_id.as_deref() != task_id.as_deref() {
            self.open_issue_id = None;
        }
        self.selected_task_id = task_id;
    }

    fn selected_issue_id<'a>(&self, task: &'a spur_core::TrackedTask) -> Option<&'a str> {
        task.issue_id.as_deref()
    }

    fn toggle_issue_detail(&mut self, task: &spur_core::TrackedTask) -> Option<Action> {
        let issue_id = self.selected_issue_id(task)?;
        if self.open_issue_id.as_deref() == Some(issue_id) {
            self.open_issue_id = None;
            return None;
        }

        self.open_issue_id = Some(issue_id.to_string());
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
            Some(TaskIssueState::Loaded(issue)) => (Some(issue), None),
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
        let current = self.current_stage(plan) as isize;
        let next = (current + delta).clamp(0, Self::max_stage(plan) as isize) as usize;
        self.select_first_in_stage(plan, next);
    }

    fn move_task(&mut self, plan: &TrackedPlan, delta: isize) {
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
}

impl View for PlanInspectorView {
    fn handle_key(&mut self, key: KeyEvent, ctx: &super::ViewContext) -> Option<Action> {
        let key = super::normalize_macos_option(key);
        let plan = ctx.plan_projection.current_for_session(&self.session_id);
        if let Some(plan) = plan {
            self.ensure_selection(plan);
            match key.code {
                KeyCode::Char('h') | KeyCode::Left => self.move_lane(plan, -1),
                KeyCode::Char('l') | KeyCode::Right => self.move_lane(plan, 1),
                KeyCode::Char('j') | KeyCode::Down => self.move_task(plan, 1),
                KeyCode::Char('k') | KeyCode::Up => self.move_task(plan, -1),
                KeyCode::Char('g') if key.modifiers.is_empty() => self.jump_lane_start(plan),
                KeyCode::Char('G') => self.jump_lane_end(plan),
                KeyCode::Enter if key.modifiers.is_empty() => {
                    if let Some(task) = self.selected_task(plan) {
                        return self.toggle_issue_detail(task);
                    }
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc => Some(Action::NavigateBack),
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
                    TaskIssueState::Loaded(Self::detail_event_to_issue(issue)),
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
                    if matches!(
                        self.issue_states.get(open_issue_id),
                        Some(TaskIssueState::Loading)
                    ) {
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
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);
        self.stacked_mode = area.width < 90;

        if let Some(plan) = ctx.plan_projection.current_for_session(&self.session_id) {
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
            let header_rows =
                Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(chunks[0]);
            let header_cols =
                Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
                    .split(header_rows[0]);

            let status_color = plan_status_color(&plan.status);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!(" Plan: {} ", plan.plan_id),
                        Style::default()
                            .fg(Color::Cyan)
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
                    .gauge_style(Style::default().fg(Color::Green))
                    .style(Style::default().fg(Color::DarkGray)),
                header_cols[1],
            );

            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" next: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(truncate_display(
                        &plan.next_action,
                        area.width.saturating_sub(8) as usize,
                    )),
                ])),
                header_rows[1],
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
                    );
                }
                if let Some(task) = selected {
                    crate::components::plan_task_detail::render_task_detail(
                        frame,
                        detail_area,
                        task,
                        live_node,
                        issue_detail.0,
                        issue_detail.1,
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
                        task,
                        live_node,
                        issue_detail.0,
                        issue_detail.1,
                    );
                }
            }
        } else {
            frame.render_widget(
                Paragraph::new("No tracked plan for this session yet.")
                    .block(Block::default().borders(Borders::ALL)),
                chunks[1],
            );
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                " h/l: lane  j/k: task  Enter: issue detail  g/G: ends  Alt+P/Esc: close ",
                Style::default().fg(Color::DarkGray),
            )])),
            chunks[2],
        );
    }

    fn tick(&mut self) {}
}

fn plan_status_color(status: &str) -> Color {
    match status {
        "running" | "active" => Color::Yellow,
        "approved" | "completed" | "success" => Color::Green,
        "failed" | "rejected" | "error" => Color::Red,
        "cancelled" => Color::Magenta,
        _ => Color::DarkGray,
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
