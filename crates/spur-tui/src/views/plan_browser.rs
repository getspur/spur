use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use spur_acp::{
    PlanLifecycleEvent, PlanOwnerStateEvent, PlanSummaryCountsEvent, PlanSummaryEvent, SessionId,
    SpurEvent, SpurEventBody,
};

use crate::action::{Action, IssueAction, ViewId};
use crate::components::status_bar::{HintOverride, StatusBar, StatusBarProps};

use super::{View, ViewContext};

const STATUS_HINT: &str =
    " [j/k]navigate [Enter]open [R]resume [e]epic [r]refresh [b]backlog [Esc]back";
const STATUS_HINT_COMPACT: &str = " [j/k]nav [Enter]open [R]resume [e]epic [Esc]back";

#[derive(Debug, Clone)]
pub struct PlanBrowserView {
    current_session: SessionId,
    plans: Vec<PlanSummaryEvent>,
    selected: usize,
    hint: Option<String>,
}

impl PlanBrowserView {
    pub fn new(current_session: SessionId) -> Self {
        Self {
            current_session,
            plans: Vec::new(),
            selected: 0,
            hint: None,
        }
    }

    pub fn plans(&self) -> &[PlanSummaryEvent] {
        &self.plans
    }

    fn selected_plan(&self) -> Option<&PlanSummaryEvent> {
        self.plans.get(self.selected)
    }

    fn current_active_plan<'a>(
        &self,
        ctx: &'a ViewContext<'_>,
    ) -> Option<&'a spur_core::TrackedPlan> {
        ctx.plan_projection
            .current_for_session(&self.current_session)
            .filter(|plan| plan.is_active())
    }

    fn has_current_active_plan(&self, ctx: &ViewContext<'_>) -> bool {
        self.current_active_plan(ctx).is_some()
    }

    fn selected_is_current_active_plan(
        &self,
        plan: &PlanSummaryEvent,
        ctx: &ViewContext<'_>,
    ) -> bool {
        self.current_active_plan(ctx)
            .is_some_and(|active| active.plan_id == plan.plan_id)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.plans.is_empty() {
            self.selected = 0;
            return;
        }
        let len = self.plans.len() as isize;
        self.selected = (self.selected as isize + delta).clamp(0, len - 1) as usize;
    }

    fn select_first(&mut self) {
        self.selected = 0;
    }

    fn select_last(&mut self) {
        if !self.plans.is_empty() {
            self.selected = self.plans.len() - 1;
        }
    }

    fn open_selected(&self, ctx: &ViewContext<'_>) -> Option<Action> {
        let Some(plan) = self.selected_plan() else {
            return Some(Action::FlashHint {
                message: "No plan selected".into(),
            });
        };

        match &plan.owner_state {
            PlanOwnerStateEvent::Mine if self.selected_is_current_active_plan(plan, ctx) => Some(
                Action::NavigateTo(ViewId::PlanInspector(self.current_session.clone())),
            ),
            PlanOwnerStateEvent::Mine => Some(Action::FlashHint {
                message: "No active sprint projection for this brain session".into(),
            }),
            PlanOwnerStateEvent::Unowned => Some(Action::FlashHint {
                message: format!("Plan {} is unowned; press R to resume first", plan.plan_id),
            }),
            PlanOwnerStateEvent::Other { owner } => Some(Action::FlashHint {
                message: format!("Plan {} is owned by {owner}", plan.plan_id),
            }),
            PlanOwnerStateEvent::Ambiguous { .. } => Some(Action::FlashHint {
                message: format!("Plan {} has ambiguous ownership", plan.plan_id),
            }),
        }
    }

    fn resume_selected(&self, ctx: &ViewContext<'_>) -> Option<Action> {
        let Some(plan) = self.selected_plan() else {
            return Some(Action::FlashHint {
                message: "No plan selected".into(),
            });
        };

        match &plan.owner_state {
            PlanOwnerStateEvent::Unowned if self.has_current_active_plan(ctx) => {
                Some(Action::FlashHint {
                    message: "Cannot resume: current brain already owns active sprint".into(),
                })
            }
            PlanOwnerStateEvent::Unowned => Some(Action::ResumePlan {
                plan_id: plan.plan_id.clone(),
            }),
            PlanOwnerStateEvent::Mine => Some(Action::FlashHint {
                message: format!("Plan {} is already owned by this brain", plan.plan_id),
            }),
            PlanOwnerStateEvent::Other { owner } => Some(Action::FlashHint {
                message: format!("Cannot resume: plan {} is owned by {owner}", plan.plan_id),
            }),
            PlanOwnerStateEvent::Ambiguous { .. } => Some(Action::FlashHint {
                message: format!(
                    "Cannot resume: plan {} has ambiguous ownership",
                    plan.plan_id
                ),
            }),
        }
    }

    fn view_selected_epic(&self) -> Option<Action> {
        self.selected_plan()
            .map(|plan| {
                Action::Issue(IssueAction::ViewDetail {
                    id: plan.epic_id.clone(),
                })
            })
            .or(Some(Action::FlashHint {
                message: "No plan selected".into(),
            }))
    }

    fn owner_label(owner: &PlanOwnerStateEvent) -> String {
        match owner {
            PlanOwnerStateEvent::Mine => "mine".into(),
            PlanOwnerStateEvent::Unowned => "unowned".into(),
            PlanOwnerStateEvent::Other { owner } => owner.clone(),
            PlanOwnerStateEvent::Ambiguous { .. } => "ambiguous".into(),
        }
    }

    fn owner_detail(owner: &PlanOwnerStateEvent) -> String {
        match owner {
            PlanOwnerStateEvent::Mine => "mine".into(),
            PlanOwnerStateEvent::Unowned => "unowned".into(),
            PlanOwnerStateEvent::Other { owner } => format!("other: {owner}"),
            PlanOwnerStateEvent::Ambiguous { owners } if owners.is_empty() => "ambiguous".into(),
            PlanOwnerStateEvent::Ambiguous { owners } => {
                format!("ambiguous: {}", owners.join(", "))
            }
        }
    }

    fn lifecycle_label(lifecycle: PlanLifecycleEvent) -> &'static str {
        match lifecycle {
            PlanLifecycleEvent::Pending => "pending",
            PlanLifecycleEvent::Running => "running",
            PlanLifecycleEvent::AwaitingReview => "awaiting review",
            PlanLifecycleEvent::Complete => "complete",
            PlanLifecycleEvent::Failed => "failed",
            PlanLifecycleEvent::Unknown => "unknown",
        }
    }

    fn progress_text(counts: Option<&PlanSummaryCountsEvent>) -> String {
        let Some(counts) = counts else {
            return "--".into();
        };
        if counts.failed + counts.rejected + counts.cancelled > 0 {
            return "blocked".into();
        }
        if counts.total == 0 {
            "--".into()
        } else {
            format!("{}/{} done", counts.approved, counts.total)
        }
    }

    fn active_slot_line(&self, ctx: &ViewContext<'_>) -> String {
        if let Some(plan) = self.current_active_plan(ctx) {
            format!(
                "Current Sprint: {} {} {}",
                plan.plan_id, plan.status, plan.progress
            )
        } else {
            "No sprint owned by this brain.".into()
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect, ctx: &ViewContext<'_>) {
        let lines = vec![
            Line::from(self.active_slot_line(ctx)),
            Line::from("r Refresh   Enter Open   R Resume   e View Epic   b Backlog   Esc/q Back"),
        ];
        let block = Block::default()
            .title(" Sprints ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn render_plan_list(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(" Plans ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if self.plans.is_empty() {
            let msg =
                Paragraph::new("No plans found.\nPress b to open Backlog and execute an epic.")
                    .style(Style::default().fg(Color::DarkGray))
                    .alignment(Alignment::Center);
            frame.render_widget(msg, inner);
            return;
        }

        let mut lines = vec![Line::from(vec![
            Span::styled(
                "  Plan          ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Epic         ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Owner           ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "State            ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled("Progress", Style::default().add_modifier(Modifier::BOLD)),
        ])];

        for (idx, plan) in self.plans.iter().enumerate() {
            let marker = if idx == self.selected { ">" } else { " " };
            let style = if idx == self.selected {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{marker} {:<13}", truncate(&plan.plan_id, 13)),
                    style,
                ),
                Span::styled(format!("{:<13}", truncate(&plan.epic_id, 13)), style),
                Span::styled(
                    format!(
                        "{:<16}",
                        truncate(&Self::owner_label(&plan.owner_state), 16)
                    ),
                    style,
                ),
                Span::styled(
                    format!("{:<17}", Self::lifecycle_label(plan.lifecycle)),
                    style,
                ),
                Span::styled(Self::progress_text(plan.counts.as_ref()), style),
            ]));
        }

        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_detail(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(" Plan Detail ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let lines = if let Some(plan) = self.selected_plan() {
            let mut lines = vec![
                Line::from(format!("Plan: {}", plan.plan_id)),
                Line::from(format!("Epic: {}", plan.epic_id)),
                Line::from(format!("Owner: {}", Self::owner_detail(&plan.owner_state))),
                Line::from(format!("State: {}", Self::lifecycle_label(plan.lifecycle))),
                Line::from(format!(
                    "Progress: {}",
                    Self::progress_text(plan.counts.as_ref())
                )),
            ];
            if let Some(hint) = self.hint.as_ref() {
                lines.push(Line::from(Span::styled(
                    hint.clone(),
                    Style::default().fg(Color::Red),
                )));
            }
            lines
        } else if let Some(hint) = self.hint.as_ref() {
            vec![Line::from(Span::styled(
                hint.clone(),
                Style::default().fg(Color::Red),
            ))]
        } else {
            vec![Line::from("No plan selected")]
        };

        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_status(&self, frame: &mut Frame, area: Rect) {
        StatusBar::render(
            frame,
            area,
            StatusBarProps {
                view: &ViewId::PlanBrowser,
                tombstone: None,
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
                issue_count: self.plans.len(),
                alert_summary: None,
                license_badge: None,
                flag_summary: None,
                view_hint_override: Some(HintOverride {
                    full: STATUS_HINT,
                    compact: Some(STATUS_HINT_COMPACT),
                    hide_on_overflow: false,
                }),
            },
        );
    }
}

impl View for PlanBrowserView {
    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewContext) -> Option<Action> {
        let key = super::normalize_macos_option(key);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down if key.modifiers.is_empty() => {
                self.move_selection(1);
                Some(Action::SelectNextBy(1))
            }
            KeyCode::Char('k') | KeyCode::Up if key.modifiers.is_empty() => {
                self.move_selection(-1);
                Some(Action::SelectPrevBy(1))
            }
            KeyCode::Char('g') if key.modifiers.is_empty() => {
                self.select_first();
                None
            }
            KeyCode::Char('G') if key.modifiers.is_empty() => {
                self.select_last();
                None
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => Some(Action::RefreshPlans),
            KeyCode::Enter if key.modifiers.is_empty() => self.open_selected(ctx),
            KeyCode::Char('R') if key.modifiers.is_empty() => self.resume_selected(ctx),
            KeyCode::Char('e') if key.modifiers.is_empty() => self.view_selected_epic(),
            KeyCode::Char('b') if key.modifiers.is_empty() => {
                Some(Action::NavigateTo(ViewId::IssueBrowser))
            }
            KeyCode::Esc | KeyCode::Char('q') if key.modifiers.is_empty() => {
                Some(Action::NavigateBack)
            }
            _ => None,
        }
    }

    fn handle_spur_event(&mut self, event: &SpurEvent, _ctx: &ViewContext) {
        match &event.body {
            SpurEventBody::PlansLoaded { plans } => {
                let selected_plan_id = self.selected_plan().map(|plan| plan.plan_id.clone());
                self.plans = plans.clone();
                self.selected = selected_plan_id
                    .and_then(|id| self.plans.iter().position(|plan| plan.plan_id == id))
                    .unwrap_or(0);
                if self.selected >= self.plans.len() {
                    self.selected = self.plans.len().saturating_sub(1);
                }
                self.hint = None;
            }
            SpurEventBody::PlanCommandError {
                operation,
                plan_id,
                error,
            } => {
                self.hint = Some(match plan_id {
                    Some(plan_id) => format!("{operation} blocked for {plan_id}: {error}"),
                    None => format!("{operation} blocked: {error}"),
                });
            }
            _ => {}
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &ViewContext) {
        let chunks = Layout::vertical([
            Constraint::Length(4),
            Constraint::Min(6),
            Constraint::Length(8),
            Constraint::Length(1),
        ])
        .split(area);

        self.render_header(frame, chunks[0], ctx);
        self.render_plan_list(frame, chunks[1]);
        self.render_detail(frame, chunks[2]);
        self.render_status(frame, chunks[3]);
    }

    fn tick(&mut self) {}
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        value
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect::<String>()
            + "..."
    }
}
