use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap},
    Frame,
};
use spur_acp::{
    PlanLifecycleEvent, PlanLoadWarningEvent, PlanOwnerStateEvent, PlanSummaryCountsEvent,
    PlanSummaryEvent, SessionId, SpurEvent, SpurEventBody,
};

use crate::action::{Action, ViewId};
use crate::components::status_bar::{HintOverride, StatusBar, StatusBarProps};
use crate::theme::{resolve_token, ColorDepth, Theme};

use super::{View, ViewContext};

fn token(theme: &Theme, name: &str) -> Color {
    resolve_token(theme, name, ColorDepth::Truecolor)
}

const STATUS_HINT: &str =
    " [j/k]navigate [p]plan peek/open [o]work item peek/open [c]claim [s]start/resume [S]sort [f]filter [r]refresh [Esc]summary/back";
const STATUS_HINT_COMPACT: &str =
    " [j/k]nav [p]plan [o]item [c]claim [s]start [S]sort [f]filter [Esc]back";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    #[default]
    UpdatedDesc,
    CreatedDesc,
    Title,
    Lifecycle,
    Owner,
}

impl SortMode {
    fn next(self) -> Self {
        match self {
            SortMode::UpdatedDesc => SortMode::CreatedDesc,
            SortMode::CreatedDesc => SortMode::Title,
            SortMode::Title => SortMode::Lifecycle,
            SortMode::Lifecycle => SortMode::Owner,
            SortMode::Owner => SortMode::UpdatedDesc,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SortMode::UpdatedDesc => "updated\u{2193}",
            SortMode::CreatedDesc => "created\u{2193}",
            SortMode::Title => "title",
            SortMode::Lifecycle => "lifecycle",
            SortMode::Owner => "owner",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterMode {
    #[default]
    All,
    Mine,
    Unowned,
    Active,
    Terminal,
}

impl FilterMode {
    fn next(self) -> Self {
        match self {
            FilterMode::All => FilterMode::Mine,
            FilterMode::Mine => FilterMode::Unowned,
            FilterMode::Unowned => FilterMode::Active,
            FilterMode::Active => FilterMode::Terminal,
            FilterMode::Terminal => FilterMode::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            FilterMode::All => "all",
            FilterMode::Mine => "mine",
            FilterMode::Unowned => "unowned",
            FilterMode::Active => "active",
            FilterMode::Terminal => "terminal",
        }
    }

    fn matches(self, plan: &PlanSummaryEvent) -> bool {
        match self {
            FilterMode::All => true,
            FilterMode::Mine => matches!(plan.owner_state, PlanOwnerStateEvent::Mine),
            FilterMode::Unowned => matches!(plan.owner_state, PlanOwnerStateEvent::Unowned),
            FilterMode::Active => plan_is_active(plan),
            FilterMode::Terminal => !plan_is_active(plan),
        }
    }
}

pub fn format_relative_time(ts: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - ts).num_seconds();
    if secs < 0 {
        return "just now".into();
    }
    if secs < 60 {
        return "just now".into();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if days < 30 {
        return format!("{days}d ago");
    }
    if days < 365 {
        let months = days / 30;
        return format!("{months}mo ago");
    }
    let years = days / 365;
    format!("{years}y ago")
}

fn format_relative_opt(ts: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    ts.map(|t| format_relative_time(t, now))
        .unwrap_or_else(|| "--".into())
}

#[derive(Debug, Clone)]
pub struct PlanBrowserView {
    current_session: SessionId,
    plans: Vec<PlanSummaryEvent>,
    warnings: Vec<PlanLoadWarningEvent>,
    /// Index into `view_index`, not `plans`. Selection is over the
    /// filtered+sorted view; the underlying `plans` order is preserved
    /// so `focus_plan_id` and `pending_focus_plan_id` remain meaningful.
    selected: usize,
    detail_peek: DetailPeek,
    confirm: Option<PlanConfirm>,
    hint: Option<String>,
    pending_focus_plan_id: Option<String>,
    sort_mode: SortMode,
    filter_mode: FilterMode,
    view_index: Vec<usize>,
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
    ForceClaim { plan_id: String },
    Start { plan_id: String },
}

impl PlanBrowserView {
    pub fn new(current_session: SessionId) -> Self {
        Self {
            current_session,
            plans: Vec::new(),
            warnings: Vec::new(),
            selected: 0,
            detail_peek: DetailPeek::Summary,
            confirm: None,
            hint: None,
            pending_focus_plan_id: None,
            sort_mode: SortMode::default(),
            filter_mode: FilterMode::default(),
            view_index: Vec::new(),
        }
    }

    fn recompute_view(&mut self) {
        let mut indices: Vec<usize> = self
            .plans
            .iter()
            .enumerate()
            .filter(|(_, plan)| self.filter_mode.matches(plan))
            .map(|(i, _)| i)
            .collect();
        let plans = &self.plans;
        let mode = self.sort_mode;
        indices.sort_by(|&a, &b| {
            let pa = &plans[a];
            let pb = &plans[b];
            let primary = match mode {
                SortMode::UpdatedDesc => pb.updated_at.cmp(&pa.updated_at),
                SortMode::CreatedDesc => pb.created_at.cmp(&pa.created_at),
                SortMode::Title => pa.title.cmp(&pb.title),
                SortMode::Lifecycle => {
                    Self::lifecycle_label(pa.lifecycle).cmp(Self::lifecycle_label(pb.lifecycle))
                }
                SortMode::Owner => {
                    Self::owner_label(&pa.owner_state).cmp(&Self::owner_label(&pb.owner_state))
                }
            };
            primary.then_with(|| pa.plan_id.cmp(&pb.plan_id))
        });
        self.view_index = indices;
        if self.view_index.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.view_index.len() {
            self.selected = self.view_index.len() - 1;
        }
    }

    fn current_plan_index(&self) -> Option<usize> {
        self.view_index.get(self.selected).copied()
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
        let view_pos = self
            .plans
            .iter()
            .position(|plan| plan.plan_id == plan_id)
            .and_then(|plan_idx| self.view_index.iter().position(|&i| i == plan_idx));
        if let Some(index) = view_pos {
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
        self.current_plan_index().and_then(|i| self.plans.get(i))
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
        if self.view_index.is_empty() {
            self.selected = 0;
            self.detail_peek = DetailPeek::Summary;
            return;
        }
        let len = self.view_index.len() as isize;
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
        if !self.view_index.is_empty() {
            let next = self.view_index.len() - 1;
            if next != self.selected {
                self.detail_peek = DetailPeek::Summary;
            }
            self.selected = next;
        }
    }

    fn cycle_sort(&mut self) {
        self.sort_mode = self.sort_mode.next();
        let anchor = self.current_plan_index();
        self.recompute_view();
        if let Some(plan_idx) = anchor {
            if let Some(new_pos) = self.view_index.iter().position(|&i| i == plan_idx) {
                self.selected = new_pos;
            }
        }
        self.detail_peek = DetailPeek::Summary;
        self.hint = None;
    }

    fn cycle_filter(&mut self) {
        self.filter_mode = self.filter_mode.next();
        let anchor = self.current_plan_index();
        self.recompute_view();
        if let Some(plan_idx) = anchor {
            if let Some(new_pos) = self.view_index.iter().position(|&i| i == plan_idx) {
                self.selected = new_pos;
            }
        }
        self.detail_peek = DetailPeek::Summary;
        self.hint = None;
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
                    message: "Cannot claim: current brain already owns active plan".into(),
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
            PlanOwnerStateEvent::Other { .. } => {
                if !plan_is_active(plan) {
                    Some(Action::FlashHint {
                        message: format!("Cannot claim: plan {} is terminal", plan.plan_id),
                    })
                } else if self.has_current_active_plan(ctx) {
                    Some(Action::FlashHint {
                        message: "Cannot claim: current brain already owns active plan".into(),
                    })
                } else {
                    self.confirm = Some(PlanConfirm::ForceClaim {
                        plan_id: plan.plan_id.clone(),
                    });
                    None
                }
            }
            PlanOwnerStateEvent::Ambiguous { .. } => {
                if !plan_is_active(plan) {
                    Some(Action::FlashHint {
                        message: format!("Cannot claim: plan {} is terminal", plan.plan_id),
                    })
                } else if self.has_current_active_plan(ctx) {
                    Some(Action::FlashHint {
                        message: "Cannot claim: current brain already owns active plan".into(),
                    })
                } else {
                    self.confirm = Some(PlanConfirm::ForceClaim {
                        plan_id: plan.plan_id.clone(),
                    });
                    None
                }
            }
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
                    message: "Cannot start: current brain already owns active plan".into(),
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
            PlanConfirm::ForceClaim { plan_id } => Some(Action::ForceReclaimPlan { plan_id }),
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
            Line::from(format!(
                "Sort: {}   Filter: {}   (S sort  f filter)",
                self.sort_mode.label(),
                self.filter_mode.label()
            )),
        ];
        let block = Block::default()
            .title(" Plans ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(token(ctx.theme, "plan_browser.border.fg")));
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn render_plan_list(&self, frame: &mut Frame, area: Rect, theme: &Theme, now: DateTime<Utc>) {
        let block = Block::default()
            .title(" Plans ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(token(theme, "plan_browser.border.fg")));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if self.view_index.is_empty() {
            let msg = if self.plans.is_empty() {
                "No plans found.\nPress b to open Backlog and execute an epic.".to_string()
            } else {
                format!(
                    "No plans match filter '{}'. Press f to cycle.",
                    self.filter_mode.label()
                )
            };
            let para = Paragraph::new(msg)
                .style(Style::default().fg(token(theme, "plan_browser.empty.fg")))
                .alignment(Alignment::Center);
            frame.render_widget(para, inner);
            return;
        }

        let header = Row::new([
            Cell::from("Plan").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Work item").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Title").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Owner").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("State").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Progress").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Updated").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Created").style(Style::default().add_modifier(Modifier::BOLD)),
        ]);

        let rows: Vec<Row> = self
            .view_index
            .iter()
            .map(|&i| &self.plans[i])
            .map(|plan| {
                Row::new([
                    Cell::from(truncate(&plan.plan_id, 16)),
                    Cell::from(truncate(&plan.epic_id, 12)),
                    Cell::from(plan.title.as_str()),
                    Cell::from(truncate(&Self::owner_label(&plan.owner_state), 12)),
                    Cell::from(Self::lifecycle_label(plan.lifecycle)),
                    Cell::from(Self::progress_text(plan.counts.as_ref())),
                    Cell::from(format_relative_opt(plan.updated_at, now)),
                    Cell::from(format_relative_opt(plan.created_at, now)),
                ])
            })
            .collect();

        let widths = [
            Constraint::Length(16),
            Constraint::Length(12),
            Constraint::Min(12),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
        ];

        let table = Table::new(rows, widths)
            .header(header)
            .highlight_symbol("> ")
            .highlight_spacing(ratatui::widgets::HighlightSpacing::Always)
            .row_highlight_style(Style::default().fg(token(theme, "plan_browser.row.selected.fg")));

        let mut state = ratatui::widgets::TableState::default();
        state.select(Some(self.selected));

        frame.render_stateful_widget(table, inner, &mut state);
    }

    fn render_detail(&self, frame: &mut Frame, area: Rect, theme: &Theme, now: DateTime<Utc>) {
        let title = match self.detail_peek {
            DetailPeek::Summary => " Plan / Work Item Summary ",
            DetailPeek::Plan => " Implementation Plan ",
            DetailPeek::WorkItem => " Work Item Scope ",
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(token(theme, "plan_browser.border.fg")));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let lines = if let Some(plan) = self.selected_plan() {
            let mut lines = match self.detail_peek {
                DetailPeek::Summary => self.render_summary_lines(plan, theme, now),
                DetailPeek::Plan => self.render_plan_lines(plan, theme, now),
                DetailPeek::WorkItem => self.render_work_item_lines(plan, theme),
            };
            let notice = if let Some(hint) = self.hint.as_ref() {
                Some(Line::from(Span::styled(
                    hint.clone(),
                    Style::default().fg(token(theme, "plan_browser.notice.error.fg")),
                )))
            } else {
                self.warnings.first().map(|warning| {
                    Line::from(Span::styled(
                        format!("Warning: {}", warning.message),
                        Style::default().fg(token(theme, "plan_browser.notice.warning.fg")),
                    ))
                })
            };
            if let Some(notice) = notice {
                lines.insert(0, Line::from(""));
                lines.insert(0, notice);
            }
            lines
        } else if let Some(hint) = self.hint.as_ref() {
            vec![Line::from(Span::styled(
                hint.clone(),
                Style::default().fg(token(theme, "plan_browser.notice.error.fg")),
            ))]
        } else {
            vec![Line::from("No plan selected")]
        };

        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
    }

    fn field_line(label: &'static str, value: impl Into<String>, theme: &Theme) -> Line<'static> {
        Line::from(vec![
            Span::styled(
                format!("{label}: "),
                Style::default()
                    .fg(token(theme, "plan_browser.field.label.fg"))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(value.into()),
        ])
    }

    fn action_line(value: impl Into<String>, theme: &Theme) -> Line<'static> {
        Line::from(Span::styled(
            value.into(),
            Style::default().fg(token(theme, "plan_browser.action_line.fg")),
        ))
    }

    fn render_summary_lines(
        &self,
        plan: &PlanSummaryEvent,
        theme: &Theme,
        now: DateTime<Utc>,
    ) -> Vec<Line<'static>> {
        vec![
            Self::field_line("Plan", plan.plan_id.clone(), theme),
            Self::field_line("Work item", plan.epic_id.clone(), theme),
            Self::field_line("Title", plan.title.clone(), theme),
            Self::field_line("Description", Self::body_preview_text(plan), theme),
            Self::field_line("Owner", Self::owner_detail(&plan.owner_state), theme),
            Self::field_line("Lifecycle", Self::lifecycle_label(plan.lifecycle), theme),
            Self::field_line("Progress", Self::progress_text(plan.counts.as_ref()), theme),
            Self::field_line("Tasks", Self::task_counts_text(plan.counts.as_ref()), theme),
            Self::field_line("Updated", Self::updated_text(plan, now), theme),
            Self::field_line("Created", Self::created_text(plan, now), theme),
            Self::field_line("Next", Self::next_action_text(plan), theme),
            Self::action_line(
                "p: implementation plan   o: work item   c: claim   s: start/resume",
                theme,
            ),
        ]
    }

    fn render_plan_lines(
        &self,
        plan: &PlanSummaryEvent,
        theme: &Theme,
        now: DateTime<Utc>,
    ) -> Vec<Line<'static>> {
        vec![
            Self::field_line("Plan", plan.plan_id.clone(), theme),
            Self::field_line("Work item", plan.epic_id.clone(), theme),
            Self::field_line("Title", plan.title.clone(), theme),
            Self::field_line("Owner", Self::owner_detail(&plan.owner_state), theme),
            Self::field_line("Lifecycle", Self::lifecycle_label(plan.lifecycle), theme),
            Self::field_line("Progress", Self::progress_text(plan.counts.as_ref()), theme),
            Self::field_line("Tasks", Self::task_counts_text(plan.counts.as_ref()), theme),
            Self::field_line("Updated", Self::updated_text(plan, now), theme),
            Self::field_line("Created", Self::created_text(plan, now), theme),
            Self::field_line("Description", Self::body_preview_text(plan), theme),
            Self::action_line("Press p again to open the implementation plan board", theme),
        ]
    }

    fn render_work_item_lines(&self, plan: &PlanSummaryEvent, theme: &Theme) -> Vec<Line<'static>> {
        vec![
            Self::field_line("Work item", plan.epic_id.clone(), theme),
            Self::field_line("Title", plan.title.clone(), theme),
            Self::field_line("Plan", plan.plan_id.clone(), theme),
            Self::field_line(
                "Issue graph scope",
                format!("spur:plan-id:{}", plan.plan_id),
                theme,
            ),
            Self::field_line("Lifecycle", Self::lifecycle_label(plan.lifecycle), theme),
            Self::field_line("Description", Self::body_preview_text(plan), theme),
            Self::action_line("Press o again to open the source work item", theme),
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

    fn updated_text(plan: &PlanSummaryEvent, now: DateTime<Utc>) -> String {
        format_relative_opt(plan.updated_at, now)
    }

    fn created_text(plan: &PlanSummaryEvent, now: DateTime<Utc>) -> String {
        format_relative_opt(plan.created_at, now)
    }

    fn render_status(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        StatusBar::render(
            frame,
            area,
            StatusBarProps {
                view: &ViewId::PlanBrowser,
                theme,
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

    fn render_confirm(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
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
            PlanConfirm::ForceClaim { plan_id } => (
                " Force Claim Plan ",
                "Force Claim",
                vec![
                    Line::from(format!("  Plan: {plan_id}")),
                    Line::from(""),
                    Line::from("  This plan is currently owned by another brain or ambiguous."),
                    Line::from("  Forcing claim will clobber the other brain's execution state."),
                    Line::from("  Only do this if the other brain is dead or stuck."),
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
        lines.push(action_line(
            "[Enter]",
            verb,
            "[Esc]",
            "Cancel",
            popup.width,
            theme,
        ));

        let block = Block::default()
            .title(Span::styled(
                title,
                Style::default()
                    .fg(token(theme, "plan_browser.confirm.title.fg"))
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_set(border::ROUNDED)
            .border_style(Style::default().fg(token(theme, "plan_browser.confirm.border.fg")));

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
            KeyCode::Char('S')
                if key.modifiers.is_empty()
                    || key.modifiers == crossterm::event::KeyModifiers::SHIFT =>
            {
                self.cycle_sort();
                None
            }
            KeyCode::Char('f') if key.modifiers.is_empty() => {
                self.cycle_filter();
                None
            }
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
            SpurEventBody::PlansLoaded { plans, warnings } => {
                let selected_plan_id = self.selected_plan().map(|plan| plan.plan_id.clone());
                self.plans = plans.clone();
                self.warnings = warnings.clone();
                self.recompute_view();
                let resolved_view_pos = self
                    .pending_focus_plan_id
                    .as_ref()
                    .and_then(|id| self.plans.iter().position(|plan| plan.plan_id == *id))
                    .and_then(|plan_idx| self.view_index.iter().position(|&i| i == plan_idx))
                    .or_else(|| {
                        selected_plan_id
                            .as_ref()
                            .and_then(|id| self.plans.iter().position(|plan| plan.plan_id == *id))
                            .and_then(|plan_idx| {
                                self.view_index.iter().position(|&i| i == plan_idx)
                            })
                    });
                self.selected = resolved_view_pos.unwrap_or(0);
                if let Some(pending) = self.pending_focus_plan_id.clone() {
                    if self
                        .selected_plan()
                        .is_some_and(|plan| plan.plan_id == pending)
                    {
                        self.pending_focus_plan_id = None;
                    }
                }
                if self.view_index.is_empty() {
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
        let now = Utc::now();
        let chunks = Layout::vertical([
            Constraint::Length(5),
            Constraint::Min(6),
            Constraint::Length(11),
            Constraint::Length(1),
        ])
        .split(area);

        self.render_header(frame, chunks[0], ctx);
        self.render_plan_list(frame, chunks[1], ctx.theme, now);
        self.render_detail(frame, chunks[2], ctx.theme, now);
        self.render_status(frame, chunks[3], ctx.theme);
        self.render_confirm(frame, area, ctx.theme);
    }

    fn tick(&mut self) {}
}

fn truncate(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        value.to_string()
    } else if max_chars <= 3 {
        value.chars().take(max_chars).collect()
    } else {
        value.chars().take(max_chars - 3).collect::<String>() + "..."
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
    theme: &Theme,
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
                .fg(token(theme, "plan_browser.confirm.primary_key.fg"))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {left_label}{}", " ".repeat(gap))),
        Span::styled(
            right_key,
            Style::default()
                .fg(token(theme, "plan_browser.confirm.cancel_key.fg"))
                .add_modifier(Modifier::BOLD),
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
    use chrono::TimeZone;
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
            created_at: None,
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
            theme: crate::theme::fallback_theme(),
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
                warnings: Vec::new(),
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
                warnings: Vec::new(),
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
                warnings: Vec::new(),
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
                warnings: Vec::new(),
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
                warnings: Vec::new(),
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
                warnings: Vec::new(),
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
    fn c_opens_force_claim_confirm_for_other_owner_then_enter_force_claims() {
        let session_id = SessionId("brain-1".into());
        let projection = PlanProjectionStore::new();
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = PlanBrowserView::new(session_id);
        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::PlansLoaded {
                plans: vec![summary_with_owner(
                    "plan-1",
                    PlanOwnerStateEvent::Other {
                        owner: "brain-2".into(),
                    },
                )],
                warnings: Vec::new(),
            }),
            &ctx,
        );

        let first = view.handle_key(key(KeyCode::Char('c')), &ctx);
        assert!(first.is_none(), "c should open confirmation first");

        let action = view.handle_key(key(KeyCode::Enter), &ctx);

        assert!(matches!(
            action,
            Some(Action::ForceReclaimPlan { plan_id }) if plan_id == "plan-1"
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
                warnings: Vec::new(),
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
                warnings: Vec::new(),
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

    #[test]
    fn format_relative_time_buckets_match_spec() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let cases = [
            (now - chrono::Duration::seconds(5), "just now"),
            (now - chrono::Duration::seconds(59), "just now"),
            (now - chrono::Duration::seconds(60), "1m ago"),
            (now - chrono::Duration::minutes(59), "59m ago"),
            (now - chrono::Duration::minutes(60), "1h ago"),
            (now - chrono::Duration::hours(23), "23h ago"),
            (now - chrono::Duration::hours(24), "1d ago"),
            (now - chrono::Duration::days(29), "29d ago"),
            (now - chrono::Duration::days(30), "1mo ago"),
            (now - chrono::Duration::days(364), "12mo ago"),
            (now - chrono::Duration::days(365), "1y ago"),
            (now - chrono::Duration::days(400), "1y ago"),
            (now + chrono::Duration::seconds(10), "just now"),
        ];
        for (ts, expected) in cases {
            assert_eq!(format_relative_time(ts, now), expected, "ts={ts}");
        }
    }

    fn summary_with_times(
        plan_id: &str,
        updated: Option<chrono::DateTime<chrono::Utc>>,
        created: Option<chrono::DateTime<chrono::Utc>>,
    ) -> PlanSummaryEvent {
        PlanSummaryEvent {
            updated_at: updated,
            created_at: created,
            ..summary(plan_id)
        }
    }

    fn loaded(plans: Vec<PlanSummaryEvent>) -> SpurEvent {
        SpurEvent::now(SpurEventBody::PlansLoaded {
            plans,
            warnings: Vec::new(),
        })
    }

    #[test]
    fn capital_s_cycles_sort_mode_and_reorders_view() {
        let session_id = SessionId("brain-1".into());
        let projection = PlanProjectionStore::new();
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = PlanBrowserView::new(session_id);
        let t0 = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        view.handle_spur_event(
            &loaded(vec![
                summary_with_times("plan-old", Some(t0), Some(t0)),
                summary_with_times(
                    "plan-new",
                    Some(t0 + chrono::Duration::hours(1)),
                    Some(t0 + chrono::Duration::hours(1)),
                ),
            ]),
            &ctx,
        );

        // Default UpdatedDesc: newest first.
        assert_eq!(view.selected_plan().unwrap().plan_id, "plan-new");

        // Cycle: UpdatedDesc -> CreatedDesc (still newest first).
        view.handle_key(key(KeyCode::Char('S')), &ctx);
        assert_eq!(view.sort_mode, SortMode::CreatedDesc);
        assert_eq!(view.selected_plan().unwrap().plan_id, "plan-new");

        // Cycle: CreatedDesc -> Title (alphabetical).
        view.handle_key(key(KeyCode::Char('S')), &ctx);
        assert_eq!(view.sort_mode, SortMode::Title);

        // Five cycles -> back to UpdatedDesc.
        for _ in 0..3 {
            view.handle_key(key(KeyCode::Char('S')), &ctx);
        }
        assert_eq!(view.sort_mode, SortMode::UpdatedDesc);
    }

    #[test]
    fn f_cycles_filter_and_hides_non_matching_plans() {
        let session_id = SessionId("brain-1".into());
        let projection = PlanProjectionStore::new();
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = PlanBrowserView::new(session_id);
        view.handle_spur_event(
            &loaded(vec![
                summary_with_owner("plan-mine", PlanOwnerStateEvent::Mine),
                summary_with_owner("plan-unowned", PlanOwnerStateEvent::Unowned),
            ]),
            &ctx,
        );

        assert_eq!(view.view_index.len(), 2);

        view.handle_key(key(KeyCode::Char('f')), &ctx);
        assert_eq!(view.filter_mode, FilterMode::Mine);
        assert_eq!(view.view_index.len(), 1);
        assert_eq!(view.selected_plan().unwrap().plan_id, "plan-mine");

        view.handle_key(key(KeyCode::Char('f')), &ctx);
        assert_eq!(view.filter_mode, FilterMode::Unowned);
        assert_eq!(view.view_index.len(), 1);
        assert_eq!(view.selected_plan().unwrap().plan_id, "plan-unowned");

        // Cycle through Active, Terminal back to All.
        for _ in 0..3 {
            view.handle_key(key(KeyCode::Char('f')), &ctx);
        }
        assert_eq!(view.filter_mode, FilterMode::All);
        assert_eq!(view.view_index.len(), 2);
    }

    #[test]
    fn confirm_popup_swallows_sort_and_filter_keys() {
        let session_id = SessionId("brain-1".into());
        let projection = PlanProjectionStore::new();
        let lineage = ExecutorLineage::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = PlanBrowserView::new(session_id);
        view.handle_spur_event(&loaded(vec![summary("plan-1")]), &ctx);
        view.handle_key(key(KeyCode::Char('c')), &ctx);
        assert!(view.confirm.is_some());

        // While confirm popup is open, S/f must NOT cycle sort/filter.
        view.handle_key(key(KeyCode::Char('S')), &ctx);
        assert_eq!(view.sort_mode, SortMode::UpdatedDesc);
        view.handle_key(key(KeyCode::Char('f')), &ctx);
        assert_eq!(view.filter_mode, FilterMode::All);
    }
}
