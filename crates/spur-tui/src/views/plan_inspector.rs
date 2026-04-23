use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use spur_acp::{SessionId, SpurEvent};
use spur_core::TrackedPlan;

use crate::action::Action;

use super::View;

pub struct PlanInspectorView {
    session_id: SessionId,
    selected_task_id: Option<String>,
    stacked_mode: bool,
}

impl PlanInspectorView {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            selected_task_id: None,
            stacked_mode: false,
        }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
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
        self.selected_task_id = crate::components::plan_stage_board::stage_grouped_tasks(plan)
            .first()
            .map(|task| task.task_id.clone());
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
        self.selected_task_id = Self::tasks_in_stage(plan, stage_idx)
            .first()
            .map(|task| task.task_id.clone());
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
        self.selected_task_id = Some(tasks[next].task_id.clone());
    }

    fn jump_lane_start(&mut self, plan: &TrackedPlan) {
        self.select_first_in_stage(plan, self.current_stage(plan));
    }

    fn jump_lane_end(&mut self, plan: &TrackedPlan) {
        let stage_idx = self.current_stage(plan);
        if let Some(task) = Self::tasks_in_stage(plan, stage_idx).last() {
            self.selected_task_id = Some(task.task_id.clone());
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

    fn handle_spur_event(&mut self, _event: &SpurEvent, _ctx: &super::ViewContext) {}

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &super::ViewContext) {
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);
        self.stacked_mode = area.width < 90;

        if let Some(plan) = ctx.plan_projection.current_for_session(&self.session_id) {
            self.ensure_selection(plan);
            let selected = self.selected_task(plan);
            let live_node =
                selected
                    .and_then(|task| task.issue_id.as_deref())
                    .and_then(|issue_id| {
                        crate::components::plan_stage_board::preferred_live_node(
                            ctx.lineage,
                            issue_id,
                        )
                    });

            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!(" Plan Inspector {} ", plan.plan_id),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!(
                        "{}  {}  next: {}",
                        plan.status, plan.progress, plan.next_action
                    )),
                ])),
                chunks[0],
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
                        ctx.lineage,
                    );
                }
                if let Some(task) = selected {
                    crate::components::plan_task_detail::render_task_detail(
                        frame,
                        detail_area,
                        task,
                        live_node,
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
                " h/l: lane  j/k: task  g/G: ends  Alt+P/Esc: close ",
                Style::default().fg(Color::DarkGray),
            )])),
            chunks[2],
        );
    }

    fn tick(&mut self) {}
}
