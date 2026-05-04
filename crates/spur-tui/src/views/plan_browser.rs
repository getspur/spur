use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use spur_acp::{
    PlanLifecycleEvent, PlanOwnerStateEvent, PlanSummaryCountsEvent, PlanSummaryEvent, SessionId,
    SpurEvent, SpurEventBody,
};

use crate::action::{Action, ViewId};
use crate::components::status_bar::{HintOverride, StatusBar, StatusBarProps};

use super::{View, ViewContext};

const STATUS_HINT: &str =
    " [j/k]navigate [p]plan peek/open [o]work item peek/open [c]claim [s]start/resume [r]refresh [Esc]summary/back";
const STATUS_HINT_COMPACT: &str = " [j/k]nav [p]plan [o]item [c]claim [s]start [Esc]back";

#[derive(Debug, Clone)]
pub struct PlanBrowserView {
    current_session: SessionId,
    plans: Vec<PlanSummaryEvent>,
    selected: usize,
    detail_peek: DetailPeek,
    confirm: Option<PlanConfirm>,
    hint: Option<String>,
    pending_focus_plan_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailPeek {
    Summary,
    Plan,
    WorkItem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PlanConfirm {
    Claim { plan_id: String },
    Start { plan_id: String },
}

impl PlanBrowserView {
    pub fn new(current_session: SessionId) -> Self {
        Self {
            current_session,
            plans: Vec::new(),
            selected: 0,
            detail_peek: DetailPeek::Summary,
            confirm: None,
            hint: None,
            pending_focus_plan_id: None,
        }
    }

    pub fn plans(&self) -> &[PlanSummaryEvent] {
        &self.plans
    }

    pub fn set_current_session(&mut self, current_session: SessionId) -> bool {
        let changed = self.current_session != current_session;
        self.current_session = current_session;
        changed
    }

    pub fn focus_plan_id(&mut self, plan_id: String) {
        if let Some(index) = self.plans.iter().position(|plan| plan.plan_id == plan_id) {
            if self.selected != index {
                self.detail_peek = DetailPeek::Summary;
            }
            self.selected = index;
            self.pending_focus_plan_id = None;
        } else {
            self.pending_focus_plan_id = Some(plan_id);
        }
    }

    #[cfg(test)]
    pub fn current_session_for_test(&self) -> &SessionId {
        &self.current_session
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
            || self.plans.iter().any(|plan| {
                matches!(plan.owner_state, PlanOwnerStateEvent::Mine) && plan_is_active(plan)
            })
    }

    fn selected_is_current_active_plan(
        &self,
        plan: &PlanSummaryEvent,
        ctx: &ViewContext<'_>,
    ) -> bool {
        self.current_active_plan(ctx)
            .is_some_and(|active| active.plan_id == plan.plan_id)
            || (matches!(plan.owner_state, PlanOwnerStateEvent::Mine) && plan_is_active(plan))
    }

    fn move_selection(&mut self, delta: isize) {
        if self.plans.is_empty() {
            self.selected = 0;
            self.detail_peek = DetailPeek::Summary;
            return;
        }
        let len = self.plans.len() as isize;
        let next = (self.selected as isize + delta).clamp(0, len - 1) as usize;
        if next != self.selected {
            self.detail_peek = DetailPeek::Summary;
        }
        self.selected = next;
    }

    fn select_first(&mut self) {
        if self.selected != 0 {
            self.detail_peek = DetailPeek::Summary;
        }
        self.selected = 0;
    }

    fn select_last(&mut self) {
        if !self.plans.is_empty() {
            let next = self.plans.len() - 1;
            if next != self.selected {
                self.detail_peek = DetailPeek::Summary;
            }
            self.selected = next;
        }
    }

    fn open_selected(&self, ctx: &ViewContext<'_>) -> Option<Action> {
        if self.detail_peek == DetailPeek::WorkItem {
            return self.open_selected_work_item();
        }
        self.open_selected_implementation_plan(ctx)
    }

    fn open_selected_implementation_plan(&self, _ctx: &ViewContext<'_>) -> Option<Action> {
        let Some(plan) = self.selected_plan() else {
            return Some(Action::FlashHint {
                message: "No plan selected".into(),
            });
        };

        Some(Action::InspectPlan {
            session_id: self.current_session.clone(),
            plan_id: plan.plan_id.clone(),
        })
    }

    fn claim_selected(&mut self, ctx: &ViewContext<'_>) -> Option<Action> {
        let Some(plan) = self.selected_plan() else {
            return Some(Action::FlashHint {
                message: "No plan selected".into(),
            });
        };

        match &plan.owner_state {
            PlanOwnerStateEvent::Unowned if !plan_is_active(plan) => Some(Action::FlashHint {
                message: format!("Cannot claim: plan {} is terminal", plan.plan_id),
            }),
            PlanOwnerStateEvent::Unowned if self.has_current_active_plan(ctx) => {
                Some(Action::FlashHint {
                    message: "Cannot claim: current brain already owns active sprint".into(),
                })
            }
            PlanOwnerStateEvent::Unowned => {
                self.confirm = Some(PlanConfirm::Claim {
                    plan_id: plan.plan_id.clone(),
                });
                None
            }
            PlanOwnerStateEvent::Mine => Some(Action::FlashHint {
                message: format!("Plan {} is already claimed by this brain", plan.plan_id),
            }),
            PlanOwnerStateEvent::Other { owner } => Some(Action::FlashHint {
                message: format!("Cannot claim: plan {} is owned by {owner}", plan.plan_id),
            }),
            PlanOwnerStateEvent::Ambiguous { .. } => Some(Action::FlashHint {
                message: format!(
                    "Cannot claim: plan {} has ambiguous ownership",
                    plan.plan_id
                ),
            }),
        }
    }

    fn start_selected(&mut self, ctx: &ViewContext<'_>) -> Option<Action> {
        let Some(plan) = self.selected_plan() else {
            return Some(Action::FlashHint {
                message: "No plan selected".into(),
            });
        };

        match &plan.owner_state {
            PlanOwnerStateEvent::Unowned => Some(Action::FlashHint {
                message: format!("Plan {} is unowned; press c to claim first", plan.plan_id),
            }),
            PlanOwnerStateEvent::Mine if !plan_is_active(plan) => Some(Action::FlashHint {
                message: format!("Cannot start: plan {} is terminal", plan.plan_id),
            }),
            PlanOwnerStateEvent::Mine
                if self.has_current_active_plan(ctx)
                    && !self.selected_is_current_active_plan(plan, ctx) =>
            {
                Some(Action::FlashHint {
                    message: "Cannot start: current brain already owns active sprint".into(),
                })
            }
            PlanOwnerStateEvent::Mine => {
                self.confirm = Some(PlanConfirm::Start {
                    plan_id: plan.plan_id.clone(),
                });
                None
            }
            PlanOwnerStateEvent::Other { owner } => Some(Action::FlashHint {
                message: format!("Cannot start: plan {} is owned by {owner}", plan.plan_id),
            }),
            PlanOwnerStateEvent::Ambiguous { .. } => Some(Action::FlashHint {
                message: format!(
                    "Cannot start: plan {} has ambiguous ownership",
                    plan.plan_id
                ),
            }),
        }
    }

    fn confirm_action(&mut self) -> Option<Action> {
        let confirm = self.confirm.take()?;
        match confirm {
            PlanConfirm::Claim { plan_id } => Some(Action::ClaimPlan { plan_id }),
            PlanConfirm::Start { plan_id } => Some(Action::ResumePlan { plan_id }),
        }
    }

    fn view_selected_implementation_plan(&mut self, _ctx: &ViewContext<'_>) -> Option<Action> {
        if self.selected_plan().is_none() {
            return Some(Action::FlashHint {
                message: "No plan selected".into(),
            });
        }

        if self.detail_peek == DetailPeek::Plan {
            let Some(plan) = self.selected_plan() else {
                return Some(Action::FlashHint {
                    message: "No plan selected".into(),
                });
            };

            Some(Action::InspectPlan {
                session_id: self.current_session.clone(),
                plan_id: plan.plan_id.clone(),
            })
        } else {
            self.detail_peek = DetailPeek::Plan;
            self.hint = None;
            None
        }
    }

    fn view_selected_work_item(&mut self) -> Option<Action> {
        if self.selected_plan().is_none() {
            return Some(Action::FlashHint {
                message: "No plan selected".into(),
            });
        }

        if self.detail_peek != DetailPeek::WorkItem {
            self.detail_peek = DetailPeek::WorkItem;
            self.hint = None;
            return None;
        }

        self.open_selected_work_item()
    }

    fn open_selected_work_item(&self) -> Option<Action> {
        self.selected_plan()
            .map(|plan| Action::OpenIssueInBacklog {
                // This is the user's durable work-item entry point for the
                // plan. Today the snapshot field is still named `epic_id`,
                // but the UI treats it as the source work item so the flow is
                // not epic-only.
                id: plan.epic_id.clone(),
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
                "Execution slot: {} {} {} (one active plan per brain)",
                plan.plan_id, plan.status, plan.progress
            )
        } else if let Some(plan) = self.plans.iter().find(|plan| {
            matches!(plan.owner_state, PlanOwnerStateEvent::Mine) && plan_is_active(plan)
        }) {
            format!(
                "Execution slot: {} {} {} (one active plan per brain)",
                plan.plan_id,
                Self::lifecycle_label(plan.lifecycle),
                Self::progress_text(plan.counts.as_ref())
            )
        } else {
            "Execution slot: empty (one active plan per brain).".into()
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect, ctx: &ViewContext<'_>) {
        let lines = vec![
            Line::from(self.active_slot_line(ctx)),
            Line::from(
                "p Implementation plan/open   o Work item/open   c Claim   s Start/Resume   Enter Open visible   r Refresh",
            ),
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
                "Work item    ",
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

        let visible_rows = inner.height.saturating_sub(1) as usize;
        let start = if visible_rows == 0 || self.selected < visible_rows {
            0
        } else {
            self.selected + 1 - visible_rows
        };
        let end = if visible_rows == 0 {
            start
        } else {
            (start + visible_rows).min(self.plans.len())
        };

        for (idx, plan) in self.plans.iter().enumerate().skip(start).take(end - start) {
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
        let title = match self.detail_peek {
            DetailPeek::Summary => " Plan / Work Item Summary ",
            DetailPeek::Plan => " Implementation Plan ",
            DetailPeek::WorkItem => " Work Item Scope ",
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let lines = if let Some(plan) = self.selected_plan() {
            let mut lines = match self.detail_peek {
                DetailPeek::Summary => self.render_summary_lines(plan),
                DetailPeek::Plan => self.render_plan_lines(plan),
                DetailPeek::WorkItem => self.render_work_item_lines(plan),
            };
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

        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
    }

    fn field_line(label: &'static str, value: impl Into<String>) -> Line<'static> {
        Line::from(vec![
            Span::styled(
                format!("{label}: "),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(value.into()),
        ])
    }

    fn action_line(value: impl Into<String>) -> Line<'static> {
        Line::from(Span::styled(value.into(), Style::default().fg(Color::Cyan)))
    }

    fn render_summary_lines(&self, plan: &PlanSummaryEvent) -> Vec<Line<'static>> {
        vec![
            Self::field_line("Plan", plan.plan_id.clone()),
            Self::field_line("Work item", plan.epic_id.clone()),
            Self::field_line("Title", plan.title.clone()),
            Self::field_line("Description", Self::body_preview_text(plan)),
            Self::field_line("Owner", Self::owner_detail(&plan.owner_state)),
            Self::field_line("Lifecycle", Self::lifecycle_label(plan.lifecycle)),
            Self::field_line("Progress", Self::progress_text(plan.counts.as_ref())),
            Self::field_line("Tasks", Self::task_counts_text(plan.counts.as_ref())),
            Self::field_line("Updated", Self::updated_text(plan)),
            Self::field_line("Next", Self::next_action_text(plan)),
            Self::action_line("p: implementation plan   o: work item   c: claim   s: start/resume"),
        ]
    }

    fn render_plan_lines(&self, plan: &PlanSummaryEvent) -> Vec<Line<'static>> {
        vec![
            Self::field_line("Plan", plan.plan_id.clone()),
            Self::field_line("Work item", plan.epic_id.clone()),
            Self::field_line("Title", plan.title.clone()),
            Self::field_line("Owner", Self::owner_detail(&plan.owner_state)),
            Self::field_line("Lifecycle", Self::lifecycle_label(plan.lifecycle)),
            Self::field_line("Progress", Self::progress_text(plan.counts.as_ref())),
            Self::field_line("Tasks", Self::task_counts_text(plan.counts.as_ref())),
            Self::field_line("Updated", Self::updated_text(plan)),
            Self::field_line("Description", Self::body_preview_text(plan)),
            Self::action_line("Press p again to open the implementation plan board"),
        ]
    }

    fn render_work_item_lines(&self, plan: &PlanSummaryEvent) -> Vec<Line<'static>> {
        vec![
            Self::field_line("Work item", plan.epic_id.clone()),
            Self::field_line("Title", plan.title.clone()),
            Self::field_line("Plan", plan.plan_id.clone()),
            Self::field_line(
                "Issue graph scope",
                format!("spur:plan-id:{}", plan.plan_id),
            ),
            Self::field_line("Lifecycle", Self::lifecycle_label(plan.lifecycle)),
            Self::field_line("Description", Self::body_preview_text(plan)),
            Self::action_line("Press o again to open the source work item"),
        ]
    }

    fn body_preview_text(plan: &PlanSummaryEvent) -> String {
        plan.source_body_preview
            .as_deref()
            .map(str::trim)
            .filter(|body| !body.is_empty())
            .unwrap_or("--")
            .to_string()
    }

    fn next_action_text(plan: &PlanSummaryEvent) -> String {
        match &plan.owner_state {
            PlanOwnerStateEvent::Unowned if plan_is_active(plan) => "claim with c".into(),
            PlanOwnerStateEvent::Unowned => "terminal/unclaimable".into(),
            PlanOwnerStateEvent::Mine if plan_is_active(plan) => "start or resume with s".into(),
            PlanOwnerStateEvent::Mine => "terminal".into(),
            PlanOwnerStateEvent::Other { owner } => format!("owned by {owner}"),
            PlanOwnerStateEvent::Ambiguous { owners } if owners.is_empty() => {
                "ambiguous ownership".into()
            }
            PlanOwnerStateEvent::Ambiguous { owners } => {
                format!("ambiguous ownership: {}", owners.join(", "))
            }
        }
    }

    fn task_counts_text(counts: Option<&PlanSummaryCountsEvent>) -> String {
        let Some(counts) = counts else {
            return "--".into();
        };
        format!(
            "total {} | pending {} ready {} running {} review {} approved {} rejected {} failed {} cancelled {}",
            counts.total,
            counts.pending,
            counts.ready,
            counts.running,
            counts.awaiting_review,
            counts.approved,
            counts.rejected,
            counts.failed,
            counts.cancelled
        )
    }

    fn updated_text(plan: &PlanSummaryEvent) -> String {
        plan.updated_at
            .as_ref()
            .map(|updated_at| updated_at.to_rfc3339())
            .unwrap_or_else(|| "--".into())
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

    fn render_confirm(&self, frame: &mut Frame, area: Rect) {
        let Some(confirm) = self.confirm.as_ref() else {
            return;
        };
        let popup = centered_rect(area, 72, 9);
        frame.render_widget(Clear, popup);

        let (title, verb, body) = match confirm {
            PlanConfirm::Claim { plan_id } => (
                " Claim Plan ",
                "Claim",
                vec![
                    Line::from(format!("  Plan: {plan_id}")),
                    Line::from(""),
                    Line::from("  This binds the plan to the current brain session."),
                    Line::from("  Only one brain session can own execution for a plan."),
                    Line::from("  Claiming does not start workers yet."),
                ],
            ),
            PlanConfirm::Start { plan_id } => (
                " Start / Resume Plan ",
                "Start",
                vec![
                    Line::from(format!("  Plan: {plan_id}")),
                    Line::from(""),
                    Line::from("  This starts/resumes execution in the current brain session."),
                    Line::from("  A brain session can actively execute only one plan."),
                    Line::from("  Workers may be dispatched after this starts."),
                ],
            ),
        };

        let mut lines = body;
        lines.push(Line::from(""));
        lines.push(action_line("[Enter]", verb, "[Esc]", "Cancel", popup.width));

        let block = Block::default()
            .title(Span::styled(
                title,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_set(border::ROUNDED)
            .border_style(Style::default().fg(Color::Yellow));

        frame.render_widget(Paragraph::new(lines).block(block), popup);
    }
}

impl View for PlanBrowserView {
    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewContext) -> Option<Action> {
        let key = super::normalize_macos_option(key);
        if self.confirm.is_some() {
            return match key.code {
                KeyCode::Enter if key.modifiers.is_empty() => self.confirm_action(),
                KeyCode::Esc | KeyCode::Char('q') if key.modifiers.is_empty() => {
                    self.confirm = None;
                    None
                }
                _ => None,
            };
        }

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
            KeyCode::Char('p') if key.modifiers.is_empty() => {
                self.view_selected_implementation_plan(ctx)
            }
            KeyCode::Char('c') if key.modifiers.is_empty() => self.claim_selected(ctx),
            KeyCode::Char('s') if key.modifiers.is_empty() => self.start_selected(ctx),
            KeyCode::Char('o') if key.modifiers.is_empty() => self.view_selected_work_item(),
            KeyCode::Char('b') if key.modifiers.is_empty() => {
                Some(Action::NavigateTo(ViewId::IssueBrowser))
            }
            KeyCode::Esc if key.modifiers.is_empty() && self.detail_peek != DetailPeek::Summary => {
                self.detail_peek = DetailPeek::Summary;
                self.hint = None;
                None
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
                self.selected = self
                    .pending_focus_plan_id
                    .as_ref()
                    .and_then(|id| self.plans.iter().position(|plan| plan.plan_id == *id))
                    .or_else(|| {
                        selected_plan_id
                            .and_then(|id| self.plans.iter().position(|plan| plan.plan_id == id))
                    })
                    .unwrap_or(0);
                if self.pending_focus_plan_id.as_ref().is_some_and(|id| {
                    self.plans
                        .get(self.selected)
                        .is_some_and(|plan| plan.plan_id == *id)
                }) {
                    self.pending_focus_plan_id = None;
                }
                if self.selected >= self.plans.len() {
                    self.selected = self.plans.len().saturating_sub(1);
                }
                if self.plans.is_empty() {
                    self.detail_peek = DetailPeek::Summary;
                }
                self.hint = None;
            }
            SpurEventBody::PlanCommandError {
                operation,
                plan_id,
                error,
            } => {
                // MCP errors are conventionally prefixed with "<tool_name>: " (e.g.
                // "resume_plan: ..."). Strip the redundant prefix so the hint reads
                // "ResumePlan blocked for plan-X: <message>" instead of
                // "ResumePlan blocked for plan-X: resume_plan: <message>".
                let display_error = error
                    .split_once(": ")
                    .map(|(prefix, rest)| {
                        if prefix.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                            rest
                        } else {
                            error.as_str()
                        }
                    })
                    .unwrap_or(error.as_str());
                self.hint = Some(match plan_id {
                    Some(plan_id) => format!("{operation} blocked for {plan_id}: {display_error}"),
                    None => format!("{operation} blocked: {display_error}"),
                });
            }
            _ => {}
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &ViewContext) {
        let chunks = Layout::vertical([
            Constraint::Length(4),
            Constraint::Min(6),
            Constraint::Length(10),
            Constraint::Length(1),
        ])
        .split(area);

        self.render_header(frame, chunks[0], ctx);
        self.render_plan_list(frame, chunks[1]);
        self.render_detail(frame, chunks[2]);
        self.render_status(frame, chunks[3]);
        self.render_confirm(frame, area);
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

fn centered_rect(outer: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(outer.width);
    let height = height.min(outer.height);
    Rect {
        x: outer.x + outer.width.saturating_sub(width) / 2,
        y: outer.y + outer.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn action_line(
    left_key: &'static str,
    left_label: &'static str,
    right_key: &'static str,
    right_label: &'static str,
    popup_width: u16,
) -> Line<'static> {
    let left_width = 1 + left_key.len() + 1 + left_label.len();
    let right_width = right_key.len() + 1 + right_label.len();
    let content_width = popup_width.saturating_sub(2) as usize;
    let gap = content_width
        .saturating_sub(left_width + right_width)
        .max(1);

    Line::from(vec![
        Span::raw(" "),
        Span::styled(
            left_key,
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {left_label}{}", " ".repeat(gap))),
        Span::styled(
            right_key,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {right_label}")),
    ])
}

fn plan_is_active(plan: &PlanSummaryEvent) -> bool {
    !matches!(
        plan.lifecycle,
        PlanLifecycleEvent::Complete | PlanLifecycleEvent::Failed
    )
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_acp::{
        PlanOwnerStateEvent, PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask,
        PlanSummaryCountsEvent, SessionId, SpurEvent, SpurEventBody,
    };
    use spur_core::{ExecutorLineage, PlanProjectionStore, SessionSynopsisProjection};

    use crate::action::Action;
    use crate::app::BrainStatus;
    use crate::views::{View, ViewContext};

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn summary(plan_id: &str) -> PlanSummaryEvent {
        PlanSummaryEvent {
            plan_id: plan_id.into(),
            epic_id: "bd-epic".into(),
            title: "Epic implementation".into(),
            source_body_preview: Some(
                "Source work item describes the implementation context and acceptance constraints."
                    .into(),
            ),
            owner_state: PlanOwnerStateEvent::Unowned,
            lifecycle: PlanLifecycleEvent::Running,
            counts: Some(PlanSummaryCountsEvent {
                total: 1,
                pending: 1,
                ready: 0,
                running: 0,
                awaiting_review: 0,
                approved: 0,
                rejected: 0,
                failed: 0,
                cancelled: 0,
            }),
            updated_at: None,
        }
    }

    fn summary_with_owner(plan_id: &str, owner_state: PlanOwnerStateEvent) -> PlanSummaryEvent {
        PlanSummaryEvent {
            owner_state,
            ..summary(plan_id)
        }
    }

    fn snapshot_store(session_id: &SessionId, plan_id: &str) -> PlanProjectionStore {
        let mut store = PlanProjectionStore::new();
        store.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
            session_id: session_id.clone(),
            snapshot: Box::new(PlanSnapshot {
                plan_id: plan_id.into(),
                epic_id: None,
                status: "running".into(),
                progress: "0/1 done".into(),
                next_action: "dispatch first stage".into(),
                ready_to_merge: false,
                counts: PlanSnapshotCounts {
                    pending: 1,
                    ..Default::default()
                },
                tasks: vec![PlanSnapshotTask {
                    task_id: "stage-a".into(),
                    task_name: "Stage A".into(),
                    agent: "codex".into(),
                    issue_id: Some("bd-epic.1".into()),
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
                owner_brain_session_id: None,
                owner_token: None,
                owner_acquired_at: None,
            }),
        }));
        store
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
        }
    }

    #[test]
    fn second_p_opens_selected_implementation_plan_snapshot() {
        let session_id = SessionId("brain-1".into());
        let projection = snapshot_store(&session_id, "plan-1");
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = PlanBrowserView::new(session_id.clone());
        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::PlansLoaded {
                plans: vec![summary("plan-1")],
            }),
            &ctx,
        );

        let first = view.handle_key(key(KeyCode::Char('p')), &ctx);
        assert!(first.is_none(), "first p should show plan detail");

        let action = view.handle_key(key(KeyCode::Char('p')), &ctx);

        assert!(matches!(
            action,
            Some(Action::InspectPlan {
                session_id: observed_session,
                plan_id
            }) if observed_session == session_id && plan_id == "plan-1"
        ));
    }

    #[test]
    fn second_p_opens_and_allows_app_to_hydrate_missing_snapshot() {
        let session_id = SessionId("brain-1".into());
        let projection = PlanProjectionStore::new();
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = PlanBrowserView::new(session_id.clone());
        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::PlansLoaded {
                plans: vec![summary("plan-1")],
            }),
            &ctx,
        );

        let first = view.handle_key(key(KeyCode::Char('p')), &ctx);
        assert!(first.is_none(), "first p should show plan detail");

        let action = view.handle_key(key(KeyCode::Char('p')), &ctx);

        assert!(matches!(
            action,
            Some(Action::InspectPlan {
                session_id: observed_session,
                plan_id
            }) if observed_session == session_id && plan_id == "plan-1"
        ));
    }

    #[test]
    fn second_o_opens_selected_source_work_item() {
        let session_id = SessionId("brain-1".into());
        let projection = PlanProjectionStore::new();
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = PlanBrowserView::new(session_id);
        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::PlansLoaded {
                plans: vec![summary("plan-1")],
            }),
            &ctx,
        );

        let first = view.handle_key(key(KeyCode::Char('o')), &ctx);
        assert!(first.is_none(), "first o should show work item detail");

        let action = view.handle_key(key(KeyCode::Char('o')), &ctx);

        assert!(matches!(
            action,
            Some(Action::OpenIssueInBacklog { id }) if id == "bd-epic"
        ));
    }

    #[test]
    fn esc_returns_detail_peek_to_summary_before_leaving_plan_browser() {
        let session_id = SessionId("brain-1".into());
        let projection = PlanProjectionStore::new();
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = PlanBrowserView::new(session_id);
        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::PlansLoaded {
                plans: vec![summary("plan-1")],
            }),
            &ctx,
        );

        assert!(view.handle_key(key(KeyCode::Char('o')), &ctx).is_none());
        assert_eq!(view.detail_peek, DetailPeek::WorkItem);

        let first_esc = view.handle_key(key(KeyCode::Esc), &ctx);

        assert!(first_esc.is_none());
        assert_eq!(view.detail_peek, DetailPeek::Summary);

        let second_esc = view.handle_key(key(KeyCode::Esc), &ctx);

        assert!(matches!(second_esc, Some(Action::NavigateBack)));
    }

    #[test]
    fn pending_focus_selects_plan_after_refresh() {
        let session_id = SessionId("brain-1".into());
        let projection = PlanProjectionStore::new();
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = PlanBrowserView::new(session_id);

        view.focus_plan_id("plan-2".into());
        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::PlansLoaded {
                plans: vec![summary("plan-1"), summary("plan-2")],
            }),
            &ctx,
        );

        let first = view.handle_key(key(KeyCode::Char('p')), &ctx);
        assert!(first.is_none(), "first p should show focused plan detail");
        let action = view.handle_key(key(KeyCode::Char('p')), &ctx);

        assert!(matches!(
            action,
            Some(Action::InspectPlan { plan_id, .. }) if plan_id == "plan-2"
        ));
    }

    #[test]
    fn c_opens_claim_confirm_then_enter_claims_unowned_plan() {
        let session_id = SessionId("brain-1".into());
        let projection = PlanProjectionStore::new();
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = PlanBrowserView::new(session_id);
        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::PlansLoaded {
                plans: vec![summary("plan-1")],
            }),
            &ctx,
        );

        let first = view.handle_key(key(KeyCode::Char('c')), &ctx);
        assert!(first.is_none(), "c should open confirmation first");

        let action = view.handle_key(key(KeyCode::Enter), &ctx);

        assert!(matches!(
            action,
            Some(Action::ClaimPlan { plan_id }) if plan_id == "plan-1"
        ));
    }

    #[test]
    fn s_requires_current_brain_claim_before_start() {
        let session_id = SessionId("brain-1".into());
        let projection = PlanProjectionStore::new();
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = PlanBrowserView::new(session_id);
        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::PlansLoaded {
                plans: vec![summary("plan-1")],
            }),
            &ctx,
        );

        let action = view.handle_key(key(KeyCode::Char('s')), &ctx);

        assert!(
            matches!(action, Some(Action::FlashHint { message }) if message.contains("press c to claim first"))
        );
    }

    #[test]
    fn s_opens_start_confirm_for_owned_plan_then_enter_resumes() {
        let session_id = SessionId("brain-1".into());
        let projection = PlanProjectionStore::new();
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = PlanBrowserView::new(session_id);
        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::PlansLoaded {
                plans: vec![summary_with_owner("plan-1", PlanOwnerStateEvent::Mine)],
            }),
            &ctx,
        );

        let first = view.handle_key(key(KeyCode::Char('s')), &ctx);
        assert!(first.is_none(), "s should open confirmation first");

        let action = view.handle_key(key(KeyCode::Enter), &ctx);

        assert!(matches!(
            action,
            Some(Action::ResumePlan { plan_id }) if plan_id == "plan-1"
        ));
    }
}
