use chrono::{DateTime, TimeZone, Utc};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap},
    Frame,
};
use spur_acp::{LoopDetailEvent, LoopRunRecordEvent, LoopSummaryEvent, SpurEvent, SpurEventBody};

use crate::action::{Action, ViewId};
use crate::components::status_bar::{HintOverride, StatusBar, StatusBarProps};
use crate::theme::{resolve_token, ColorDepth, Theme};

use super::plan_browser::format_until;
use super::{View, ViewContext};

fn token(theme: &Theme, name: &str) -> Color {
    resolve_token(theme, name, ColorDepth::Truecolor)
}

const STATUS_HINT: &str =
    " [j/k]navigate [Enter]inspect [o]issue [p]pause/resume [x]retire [S]sort [f]filter [r]refresh [Esc]back";
const STATUS_HINT_COMPACT: &str =
    " [j/k]nav [Enter]inspect [o]issue [p]pause [x]retire [S]sort [f]filter [Esc]back";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SortMode {
    #[default]
    NextRun,
    Title,
    State,
    LastOutcome,
}

impl SortMode {
    fn next(self) -> Self {
        match self {
            SortMode::NextRun => SortMode::Title,
            SortMode::Title => SortMode::State,
            SortMode::State => SortMode::LastOutcome,
            SortMode::LastOutcome => SortMode::NextRun,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SortMode::NextRun => "next run",
            SortMode::Title => "title",
            SortMode::State => "state",
            SortMode::LastOutcome => "last outcome",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum FilterMode {
    #[default]
    All,
    Active,
    Paused,
    Retired,
}

impl FilterMode {
    fn next(self) -> Self {
        match self {
            FilterMode::All => FilterMode::Active,
            FilterMode::Active => FilterMode::Paused,
            FilterMode::Paused => FilterMode::Retired,
            FilterMode::Retired => FilterMode::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            FilterMode::All => "all",
            FilterMode::Active => "active",
            FilterMode::Paused => "paused",
            FilterMode::Retired => "retired",
        }
    }

    fn matches(self, row: &LoopRow) -> bool {
        match self {
            FilterMode::All => true,
            FilterMode::Active => matches!(row.state(), LoopState::Active),
            FilterMode::Paused => matches!(row.state(), LoopState::Paused | LoopState::AutoPaused),
            FilterMode::Retired => matches!(row.state(), LoopState::Retired),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopState {
    Active,
    Paused,
    AutoPaused,
    Retired,
}

impl LoopState {
    fn label(self) -> &'static str {
        match self {
            LoopState::Active => "active",
            LoopState::Paused => "paused",
            LoopState::AutoPaused => "auto-paused",
            LoopState::Retired => "retired",
        }
    }

    fn sort_rank(self) -> u8 {
        match self {
            LoopState::Active => 0,
            LoopState::Paused => 1,
            LoopState::AutoPaused => 2,
            LoopState::Retired => 3,
        }
    }
}

#[derive(Debug, Clone)]
struct LoopRow {
    summary: LoopSummaryEvent,
    state_override: Option<LoopState>,
}

impl LoopRow {
    fn new(summary: LoopSummaryEvent) -> Self {
        Self {
            summary,
            state_override: None,
        }
    }

    fn loop_id(&self) -> &str {
        &self.summary.loop_id
    }

    fn state(&self) -> LoopState {
        if self.summary.retired {
            LoopState::Retired
        } else if let Some(state) = self.state_override {
            state
        } else if self.summary.paused {
            LoopState::Paused
        } else {
            LoopState::Active
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailPeek {
    Summary,
    Detail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LoopConfirm {
    Pause { loop_id: String },
    Resume { loop_id: String },
    Kill { loop_id: String },
}

#[derive(Debug, Clone)]
pub struct LoopBrowserView {
    rows: Vec<LoopRow>,
    warnings: Vec<String>,
    selected: usize,
    detail_peek: DetailPeek,
    detail: Option<LoopDetailEvent>,
    confirm: Option<LoopConfirm>,
    hint: Option<String>,
    pending_focus_loop_id: Option<String>,
    sort_mode: SortMode,
    filter_mode: FilterMode,
    view_index: Vec<usize>,
}

impl LoopBrowserView {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            warnings: Vec::new(),
            selected: 0,
            detail_peek: DetailPeek::Summary,
            detail: None,
            confirm: None,
            hint: None,
            pending_focus_loop_id: None,
            sort_mode: SortMode::default(),
            filter_mode: FilterMode::default(),
            view_index: Vec::new(),
        }
    }

    pub fn focus_loop_id(&mut self, loop_id: String) {
        let view_pos = self
            .rows
            .iter()
            .position(|row| row.loop_id() == loop_id)
            .and_then(|row_idx| self.view_index.iter().position(|&i| i == row_idx));
        if let Some(index) = view_pos {
            if self.selected != index {
                self.detail_peek = DetailPeek::Summary;
            }
            self.selected = index;
            self.pending_focus_loop_id = None;
        } else {
            self.pending_focus_loop_id = Some(loop_id);
        }
    }

    #[cfg(test)]
    pub(crate) fn loop_ids_for_test(&self) -> Vec<&str> {
        self.rows.iter().map(|row| row.loop_id()).collect()
    }

    fn recompute_view(&mut self) {
        let mut indices: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| self.filter_mode.matches(row))
            .map(|(i, _)| i)
            .collect();
        let rows = &self.rows;
        let mode = self.sort_mode;
        indices.sort_by(|&a, &b| {
            let ra = &rows[a];
            let rb = &rows[b];
            let primary = match mode {
                SortMode::NextRun => Self::next_run_sort_key(ra).cmp(&Self::next_run_sort_key(rb)),
                SortMode::Title => ra.summary.title.cmp(&rb.summary.title),
                SortMode::State => ra
                    .state()
                    .sort_rank()
                    .cmp(&rb.state().sort_rank())
                    .then_with(|| ra.state().label().cmp(rb.state().label())),
                SortMode::LastOutcome => {
                    Self::last_outcome_sort_key(ra).cmp(&Self::last_outcome_sort_key(rb))
                }
            };
            primary.then_with(|| ra.loop_id().cmp(rb.loop_id()))
        });
        self.view_index = indices;
        if self.view_index.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.view_index.len() {
            self.selected = self.view_index.len() - 1;
        }
    }

    fn next_run_sort_key(row: &LoopRow) -> (u8, i64) {
        match row.state() {
            LoopState::Retired => (3, i64::MAX),
            LoopState::Paused | LoopState::AutoPaused => (2, i64::MAX),
            LoopState::Active => match row.summary.next_run {
                Some(next_run) => (0, next_run),
                None => (1, i64::MAX),
            },
        }
    }

    fn last_outcome_sort_key(row: &LoopRow) -> (u8, &str) {
        row.summary
            .last_outcome
            .as_deref()
            .map(|outcome| (0, outcome))
            .unwrap_or((1, ""))
    }

    fn current_loop_index(&self) -> Option<usize> {
        self.view_index.get(self.selected).copied()
    }

    fn selected_loop(&self) -> Option<&LoopRow> {
        self.current_loop_index().and_then(|i| self.rows.get(i))
    }

    fn current_loop_id(&self) -> Option<String> {
        self.selected_loop().map(|row| row.loop_id().to_string())
    }

    fn preserve_selection_after_recompute(&mut self, anchor: Option<String>) {
        self.recompute_view();
        if let Some(anchor) = anchor {
            if let Some(new_pos) = self
                .rows
                .iter()
                .position(|row| row.loop_id() == anchor)
                .and_then(|row_idx| self.view_index.iter().position(|&i| i == row_idx))
            {
                self.selected = new_pos;
            }
        }
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
        let anchor = self.current_loop_id();
        self.preserve_selection_after_recompute(anchor);
        self.detail_peek = DetailPeek::Summary;
        self.hint = None;
    }

    fn cycle_filter(&mut self) {
        self.filter_mode = self.filter_mode.next();
        let anchor = self.current_loop_id();
        self.preserve_selection_after_recompute(anchor);
        self.detail_peek = DetailPeek::Summary;
        self.hint = None;
    }

    fn inspect_selected(&mut self) -> Option<Action> {
        let Some(loop_id) = self.current_loop_id() else {
            return Some(Action::FlashHint {
                message: "No loop selected".into(),
            });
        };

        if self.detail_peek == DetailPeek::Detail
            && self
                .detail
                .as_ref()
                .is_some_and(|detail| detail.loop_id == loop_id)
        {
            self.detail_peek = DetailPeek::Summary;
            return None;
        }

        self.detail_peek = DetailPeek::Detail;
        self.hint = None;
        Some(Action::InspectLoop { loop_id })
    }

    fn open_selected_issue(&self) -> Option<Action> {
        self.selected_loop()
            .map(|row| Action::OpenIssueInBacklog {
                id: row.summary.issue_id.clone(),
            })
            .or(Some(Action::FlashHint {
                message: "No loop selected".into(),
            }))
    }

    fn pause_or_resume_selected(&mut self) -> Option<Action> {
        let Some(row) = self.selected_loop() else {
            return Some(Action::FlashHint {
                message: "No loop selected".into(),
            });
        };
        if matches!(row.state(), LoopState::Retired) {
            return self.retired_hint();
        }
        let loop_id = row.loop_id().to_string();
        self.confirm = Some(match row.state() {
            LoopState::Paused | LoopState::AutoPaused => LoopConfirm::Resume { loop_id },
            LoopState::Active => LoopConfirm::Pause { loop_id },
            LoopState::Retired => unreachable!("retired handled above"),
        });
        None
    }

    fn kill_selected(&mut self) -> Option<Action> {
        let Some(row) = self.selected_loop() else {
            return Some(Action::FlashHint {
                message: "No loop selected".into(),
            });
        };
        if matches!(row.state(), LoopState::Retired) {
            return self.retired_hint();
        }
        self.confirm = Some(LoopConfirm::Kill {
            loop_id: row.loop_id().to_string(),
        });
        None
    }

    fn retired_hint(&mut self) -> Option<Action> {
        let message = "loop is retired".to_string();
        self.hint = Some(message.clone());
        Some(Action::FlashHint { message })
    }

    fn confirm_action(&mut self) -> Option<Action> {
        let confirm = self.confirm.take()?;
        match confirm {
            LoopConfirm::Pause { loop_id } => Some(Action::PauseLoop { loop_id }),
            LoopConfirm::Resume { loop_id } => Some(Action::ResumeLoop { loop_id }),
            LoopConfirm::Kill { loop_id } => Some(Action::KillLoop { loop_id }),
        }
    }

    fn apply_loaded(&mut self, loops: &[LoopSummaryEvent], warnings: &[String]) {
        let selected_loop_id = self.current_loop_id();
        self.rows = loops.iter().cloned().map(LoopRow::new).collect();
        self.warnings = warnings.to_vec();
        self.recompute_view();
        let resolved_view_pos = self
            .pending_focus_loop_id
            .as_ref()
            .and_then(|id| self.rows.iter().position(|row| row.loop_id() == id))
            .and_then(|row_idx| self.view_index.iter().position(|&i| i == row_idx))
            .or_else(|| {
                selected_loop_id
                    .as_ref()
                    .and_then(|id| self.rows.iter().position(|row| row.loop_id() == id))
                    .and_then(|row_idx| self.view_index.iter().position(|&i| i == row_idx))
            });
        self.selected = resolved_view_pos.unwrap_or(0);
        if let Some(pending) = self.pending_focus_loop_id.clone() {
            if self
                .selected_loop()
                .is_some_and(|row| row.loop_id() == pending)
            {
                self.pending_focus_loop_id = None;
            }
        }
        if self.view_index.is_empty() {
            self.detail_peek = DetailPeek::Summary;
        }
        self.hint = None;
    }

    fn update_row<F>(&mut self, loop_id: &str, update: F) -> bool
    where
        F: FnOnce(&mut LoopRow),
    {
        let Some(row_idx) = self.rows.iter().position(|row| row.loop_id() == loop_id) else {
            self.hint = Some("new loop activity — press r".into());
            return false;
        };
        let anchor = self.current_loop_id();
        update(&mut self.rows[row_idx]);
        self.preserve_selection_after_recompute(anchor);
        true
    }

    fn apply_detail(&mut self, detail: &LoopDetailEvent) {
        let known = self.update_row(&detail.loop_id, |row| {
            row.summary.issue_id = detail.issue_id.clone();
            row.summary.title = detail.title.clone();
            row.summary.goal_preview = detail.goal_preview.clone();
            row.summary.cadence_secs = detail.cadence_secs;
            row.summary.effective_interval_secs = detail.effective_interval_secs;
            row.summary.backoff_active = detail.backoff_active;
            row.summary.paused = detail.paused;
            row.summary.next_run = detail.next_run;
            row.summary.consecutive_failures = detail.consecutive_failures;
            row.state_override = None;
        });
        if known {
            self.detail = Some(detail.clone());
            self.hint = None;
        }
    }

    fn apply_run_recorded(
        &mut self,
        loop_id: &str,
        generation: u32,
        outcome: &str,
        cost_micros: u64,
    ) {
        let autonomy = self
            .rows
            .iter()
            .find(|row| row.loop_id() == loop_id)
            .and_then(|row| row.summary.autonomy.clone());
        let known = self.update_row(loop_id, |row| {
            row.summary.last_generation = Some(generation);
            row.summary.last_outcome = Some(outcome.to_string());
            row.summary.last_cost_micros = Some(cost_micros);
            if outcome_indicates_failure(outcome) {
                row.summary.consecutive_failures =
                    row.summary.consecutive_failures.saturating_add(1);
            } else {
                row.summary.consecutive_failures = 0;
            }
        });
        if known {
            if let Some(detail) = self
                .detail
                .as_mut()
                .filter(|detail| detail.loop_id == loop_id)
            {
                detail.recent_runs.insert(
                    0,
                    LoopRunRecordEvent {
                        generation,
                        outcome: outcome.to_string(),
                        cost_micros,
                        autonomy,
                    },
                );
                detail.recent_runs.truncate(20);
                detail.consecutive_failures = if outcome_indicates_failure(outcome) {
                    detail.consecutive_failures.saturating_add(1)
                } else {
                    0
                };
            }
            self.hint = Some(format!("Loop {loop_id} gen {generation}: {outcome}"));
        }
    }

    fn apply_paused(&mut self, loop_id: &str, by: &str) {
        let known = self.update_row(loop_id, |row| match by {
            "paused" => {
                row.summary.paused = true;
                row.state_override = Some(LoopState::Paused);
            }
            "auto_paused" => {
                row.summary.paused = true;
                row.state_override = Some(LoopState::AutoPaused);
            }
            "resumed" => {
                row.summary.paused = false;
                row.summary.retired = false;
                row.state_override = Some(LoopState::Active);
            }
            "retired" => {
                row.summary.retired = true;
                row.summary.paused = true;
                row.state_override = Some(LoopState::Retired);
            }
            _ => {
                row.state_override = None;
            }
        });
        if known {
            if let Some(detail) = self
                .detail
                .as_mut()
                .filter(|detail| detail.loop_id == loop_id)
            {
                detail.paused = matches!(by, "paused" | "auto_paused" | "retired");
            }
            self.hint = Some(format!("Loop {loop_id}: {by}"));
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let lines = vec![
            Line::from("Global governed loops"),
            Line::from(
                "Enter Inspect   o Issue   p Pause/Resume   x Retire   r Refresh   L from Plans opens this view",
            ),
            Line::from(format!(
                "Sort: {}   Filter: {}   (S sort  f filter)",
                self.sort_mode.label(),
                self.filter_mode.label()
            )),
        ];
        let block = Block::default()
            .title(" Loops ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(token(theme, "plan_browser.border.fg")));
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn render_loop_list(&self, frame: &mut Frame, area: Rect, theme: &Theme, now: DateTime<Utc>) {
        let block = Block::default()
            .title(" Loops ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(token(theme, "plan_browser.border.fg")));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if self.view_index.is_empty() {
            let msg = if self.rows.is_empty() {
                "No loops found.\nPress r to refresh.".to_string()
            } else {
                format!(
                    "No loops match filter '{}'. Press f to cycle.",
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
            Cell::from("Loop").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Title").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Aut").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("State").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Cad→Eff").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Next run").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Last run").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Fails").style(Style::default().add_modifier(Modifier::BOLD)),
        ]);

        let rows: Vec<Row> = self
            .view_index
            .iter()
            .map(|&i| &self.rows[i])
            .map(|row| {
                Row::new([
                    Cell::from(truncate(row.loop_id(), 14)),
                    Cell::from(row.summary.title.as_str()),
                    Cell::from(Self::autonomy_label(row)),
                    Cell::from(row.state().label()),
                    Cell::from(Self::cadence_text(row)),
                    Cell::from(Self::next_run_text(row, now)),
                    Cell::from(Self::last_run_text(row)),
                    Cell::from(row.summary.consecutive_failures.to_string()),
                ])
            })
            .collect();

        let widths = [
            Constraint::Length(14),
            Constraint::Min(18),
            Constraint::Length(5),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(22),
            Constraint::Length(6),
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
            DetailPeek::Summary => " Loop Summary ",
            DetailPeek::Detail => " Loop Detail ",
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(token(theme, "plan_browser.border.fg")));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let lines = if let Some(row) = self.selected_loop() {
            let mut lines = match self.detail_for_row(row) {
                Some(detail) if self.detail_peek == DetailPeek::Detail => {
                    self.render_detail_lines(row, detail, theme, now)
                }
                _ if self.detail_peek == DetailPeek::Detail => {
                    let mut lines = self.render_summary_lines(row, theme, now);
                    lines.push(Self::action_line("Loading recent runs...", theme));
                    lines
                }
                _ => self.render_summary_lines(row, theme, now),
            };
            if let Some(notice) = self.notice_line(theme) {
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
            vec![Line::from("No loop selected")]
        };

        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
    }

    fn notice_line(&self, theme: &Theme) -> Option<Line<'static>> {
        if let Some(hint) = self.hint.as_ref() {
            Some(Line::from(Span::styled(
                hint.clone(),
                Style::default().fg(token(theme, "plan_browser.notice.error.fg")),
            )))
        } else {
            self.warnings.first().map(|warning| {
                Line::from(Span::styled(
                    format!("Warning: {warning}"),
                    Style::default().fg(token(theme, "plan_browser.notice.warning.fg")),
                ))
            })
        }
    }

    fn detail_for_row(&self, row: &LoopRow) -> Option<&LoopDetailEvent> {
        self.detail
            .as_ref()
            .filter(|detail| detail.loop_id == row.loop_id())
    }

    fn render_summary_lines(
        &self,
        row: &LoopRow,
        theme: &Theme,
        now: DateTime<Utc>,
    ) -> Vec<Line<'static>> {
        vec![
            Self::field_line("Loop", row.summary.loop_id.clone(), theme),
            Self::field_line("Issue", row.summary.issue_id.clone(), theme),
            Self::field_line("Title", row.summary.title.clone(), theme),
            Self::field_line("Goal", Self::goal_preview(row), theme),
            Self::field_line("State", row.state().label(), theme),
            Self::field_line("Next run", Self::next_run_text(row, now), theme),
            Self::field_line("Cadence", Self::cadence_text(row), theme),
            Self::field_line(
                "Governors",
                "budget/gen -- | day cap -- | max tasks --",
                theme,
            ),
            Self::field_line("Ratchet", Self::ratchet_summary(row), theme),
            Self::action_line(
                "Enter: inspect recent runs   o: issue   p: pause/resume   x: retire",
                theme,
            ),
        ]
    }

    fn render_detail_lines(
        &self,
        row: &LoopRow,
        detail: &LoopDetailEvent,
        theme: &Theme,
        now: DateTime<Utc>,
    ) -> Vec<Line<'static>> {
        let mut lines = vec![
            Self::field_line("Loop", detail.loop_id.clone(), theme),
            Self::field_line("Issue", detail.issue_id.clone(), theme),
            Self::field_line("Title", detail.title.clone(), theme),
            Self::field_line(
                "Goal",
                detail
                    .goal_preview
                    .as_deref()
                    .unwrap_or_else(|| row.summary.goal_preview.as_deref().unwrap_or("--")),
                theme,
            ),
            Self::field_line("State", row.state().label(), theme),
            Self::field_line("Next run", Self::next_run_text(row, now), theme),
            Self::field_line(
                "Governors",
                format!(
                    "budget/gen {} | day cap {} | max tasks {}",
                    format_optional_cost(detail.budget_micros_per_generation),
                    detail
                        .max_generations_per_day
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "--".into()),
                    detail
                        .max_tasks
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "--".into())
                ),
                theme,
            ),
            Self::field_line(
                "Ratchet",
                format!(
                    "last outcome {} | consecutive fails {}",
                    row.summary.last_outcome.as_deref().unwrap_or("--"),
                    detail.consecutive_failures
                ),
                theme,
            ),
            Self::field_line(
                "Cadence",
                format!(
                    "{}→{}{}",
                    format_duration_short(detail.cadence_secs),
                    format_duration_short(detail.effective_interval_secs),
                    if detail.backoff_active {
                        " backoff"
                    } else {
                        ""
                    }
                ),
                theme,
            ),
            Self::action_line("Recent runs", theme),
        ];
        if detail.recent_runs.is_empty() {
            lines.push(Line::from("  --"));
        } else {
            lines.extend(detail.recent_runs.iter().take(8).map(|run| {
                Line::from(format!(
                    "  gen {}  {}  {}  {}",
                    run.generation,
                    run.outcome,
                    format_cost_micros(run.cost_micros),
                    run.autonomy
                        .as_deref()
                        .map(format_autonomy)
                        .unwrap_or_else(|| "--".into())
                ))
            }));
        }
        lines
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

    fn autonomy_label(row: &LoopRow) -> String {
        row.summary
            .autonomy
            .as_deref()
            .map(format_autonomy)
            .unwrap_or_else(|| "--".into())
    }

    fn goal_preview(row: &LoopRow) -> String {
        row.summary
            .goal_preview
            .as_deref()
            .map(str::trim)
            .filter(|goal| !goal.is_empty())
            .unwrap_or("--")
            .to_string()
    }

    fn cadence_text(row: &LoopRow) -> String {
        let suffix = if row.summary.backoff_active { "*" } else { "" };
        format!(
            "{}→{}{}",
            format_duration_short(row.summary.cadence_secs),
            format_duration_short(row.summary.effective_interval_secs),
            suffix
        )
    }

    fn next_run_text(row: &LoopRow, now: DateTime<Utc>) -> String {
        match row.state() {
            LoopState::Paused | LoopState::AutoPaused => "paused".into(),
            LoopState::Retired => "—".into(),
            LoopState::Active => row
                .summary
                .next_run
                .and_then(|ts| Utc.timestamp_opt(ts, 0).single())
                .map(|ts| format_until(ts, now))
                .unwrap_or_else(|| "—".into()),
        }
    }

    fn last_run_text(row: &LoopRow) -> String {
        match (
            row.summary.last_generation,
            row.summary.last_outcome.as_deref(),
            row.summary.last_cost_micros,
        ) {
            (Some(generation), Some(outcome), Some(cost)) => {
                format!("g{generation} {outcome} {}", format_cost_micros(cost))
            }
            (Some(generation), Some(outcome), None) => format!("g{generation} {outcome}"),
            (Some(generation), None, _) => format!("g{generation} --"),
            _ => "--".into(),
        }
    }

    fn ratchet_summary(row: &LoopRow) -> String {
        format!(
            "last outcome {} | consecutive fails {}",
            row.summary.last_outcome.as_deref().unwrap_or("--"),
            row.summary.consecutive_failures
        )
    }

    fn render_status(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        StatusBar::render(
            frame,
            area,
            StatusBarProps {
                view: &ViewId::LoopBrowser,
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
                notebook_ready: false,
                session_lifecycle_caps: None,
                issue_count: self.rows.len(),
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
            LoopConfirm::Pause { loop_id } => (
                " Pause Loop ",
                "Confirm",
                vec![
                    Line::from(format!("  Loop: {loop_id}")),
                    Line::from(""),
                    Line::from("  Pausing suppresses scheduled generations."),
                    Line::from("  Existing plans and run history remain intact."),
                    Line::from("  Resume later with p on the paused row."),
                ],
            ),
            LoopConfirm::Resume { loop_id } => (
                " Resume Loop ",
                "Confirm",
                vec![
                    Line::from(format!("  Loop: {loop_id}")),
                    Line::from(""),
                    Line::from("  Resuming allows the scheduler to arm this loop again."),
                    Line::from("  Governors and backoff still apply."),
                    Line::from("  The next run may be recalculated by the backend."),
                ],
            ),
            LoopConfirm::Kill { loop_id } => (
                " Retire Loop ",
                "Confirm",
                vec![
                    Line::from(format!("  Loop: {loop_id}")),
                    Line::from(""),
                    Line::from("  Retiring closes the loop's source issue."),
                    Line::from("  It cannot be paused or resumed from this browser afterward."),
                    Line::from("  Use only when the loop should stop permanently."),
                ],
            ),
        };

        let mut lines = body;
        lines.push(Line::from(""));
        lines.push(confirm_action_line(
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

impl Default for LoopBrowserView {
    fn default() -> Self {
        Self::new()
    }
}

impl View for LoopBrowserView {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &ViewContext) -> Option<Action> {
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
            KeyCode::Char('r') if key.modifiers.is_empty() => Some(Action::RefreshLoops),
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
            KeyCode::Enter if key.modifiers.is_empty() => self.inspect_selected(),
            KeyCode::Char('o') if key.modifiers.is_empty() => self.open_selected_issue(),
            KeyCode::Char('p') if key.modifiers.is_empty() => self.pause_or_resume_selected(),
            KeyCode::Char('x') if key.modifiers.is_empty() => self.kill_selected(),
            KeyCode::Esc if key.modifiers.is_empty() && self.detail_peek == DetailPeek::Detail => {
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
            SpurEventBody::LoopsLoaded { loops, warnings } => self.apply_loaded(loops, warnings),
            SpurEventBody::LoopDetailLoaded { detail } => self.apply_detail(detail),
            SpurEventBody::LoopArmed {
                loop_id,
                generation,
                next_run,
            } => {
                if self.update_row(loop_id, |row| {
                    row.summary.last_generation = Some(*generation);
                    row.summary.next_run = Some(*next_run);
                    if !row.summary.retired {
                        row.summary.paused = false;
                        row.state_override = Some(LoopState::Active);
                    }
                }) {
                    if let Some(detail) = self
                        .detail
                        .as_mut()
                        .filter(|detail| detail.loop_id == *loop_id)
                    {
                        detail.next_run = Some(*next_run);
                        detail.paused = false;
                    }
                    self.hint = Some(format!(
                        "Loop {loop_id} gen {generation}: armed next {next_run}"
                    ));
                }
            }
            SpurEventBody::LoopGenerationStarted {
                loop_id,
                generation,
                plan_id,
            } => {
                if self.update_row(loop_id, |row| {
                    row.summary.last_generation = Some(*generation);
                }) {
                    self.hint = Some(format!("Loop {loop_id} gen {generation}: plan {plan_id}"));
                }
            }
            SpurEventBody::LoopRunRecorded {
                loop_id,
                generation,
                outcome,
                cost_micros,
            } => self.apply_run_recorded(loop_id, *generation, outcome, *cost_micros),
            SpurEventBody::LoopPaused { loop_id, by } => self.apply_paused(loop_id, by),
            SpurEventBody::LoopCommandError {
                operation,
                loop_id,
                error,
            } => {
                let display_error = strip_command_prefix(error);
                self.hint = Some(match loop_id {
                    Some(loop_id) => {
                        format!("{operation} blocked for {loop_id}: {display_error}")
                    }
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
            Constraint::Length(15),
            Constraint::Length(1),
        ])
        .split(area);

        self.render_header(frame, chunks[0], ctx.theme);
        self.render_loop_list(frame, chunks[1], ctx.theme, now);
        self.render_detail(frame, chunks[2], ctx.theme, now);
        self.render_status(frame, chunks[3], ctx.theme);
        self.render_confirm(frame, area, ctx.theme);
    }

    fn tick(&mut self) {}
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        value.to_string()
    }
}

fn format_autonomy(value: &str) -> String {
    value.to_ascii_uppercase()
}

fn format_duration_short(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

fn format_cost_micros(micros: u64) -> String {
    format!("${:.2}", micros as f64 / 1_000_000.0)
}

fn format_optional_cost(micros: Option<u64>) -> String {
    micros
        .map(format_cost_micros)
        .unwrap_or_else(|| "--".into())
}

fn outcome_indicates_failure(outcome: &str) -> bool {
    let lower = outcome.to_ascii_lowercase();
    lower.contains("fail") || lower.contains("reject") || lower.contains("error")
}

fn strip_command_prefix(error: &str) -> &str {
    error
        .split_once(": ")
        .map(|(prefix, rest)| {
            if prefix.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                rest
            } else {
                error
            }
        })
        .unwrap_or(error)
}

fn centered_rect(area: Rect, percent_x: u16, height: u16) -> Rect {
    let width = area.width.saturating_mul(percent_x).saturating_div(100);
    let width = width.clamp(40, area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn confirm_action_line(
    left_key: &'static str,
    left_label: &'static str,
    right_key: &'static str,
    right_label: &'static str,
    width: u16,
    theme: &Theme,
) -> Line<'static> {
    let button_text_len =
        left_key.len() + left_label.len() + right_key.len() + right_label.len() + 4;
    let left_pad = width.saturating_sub(button_text_len as u16) / 2;
    Line::from(vec![
        Span::raw(" ".repeat(left_pad as usize)),
        Span::styled(
            left_key,
            Style::default()
                .fg(token(theme, "plan_browser.confirm.primary_key.fg"))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {left_label}   ")),
        Span::styled(
            right_key,
            Style::default()
                .fg(token(theme, "plan_browser.confirm.cancel_key.fg"))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {right_label}")),
    ])
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_acp::{
        LoopDetailEvent, LoopRunRecordEvent, LoopSummaryEvent, SpurEvent, SpurEventBody,
    };
    use spur_core::{ExecutorLineage, PlanProjectionStore, SessionSynopsisProjection};

    use crate::action::Action;
    use crate::app::BrainStatus;
    use crate::views::{View, ViewContext};

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
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
            notebook_ready: false,
            theme: crate::theme::fallback_theme(),
        }
    }

    fn summary(loop_id: &str) -> LoopSummaryEvent {
        LoopSummaryEvent {
            loop_id: loop_id.into(),
            issue_id: format!("bd-{loop_id}"),
            title: format!("Loop {loop_id}"),
            autonomy: Some("l2".into()),
            paused: false,
            retired: false,
            backoff_active: false,
            cadence_secs: 900,
            effective_interval_secs: 900,
            next_run: None,
            last_generation: None,
            last_outcome: None,
            last_cost_micros: None,
            consecutive_failures: 0,
            goal_preview: Some(format!("Keep {loop_id} healthy")),
            updated_at: None,
        }
    }

    fn with_next(mut row: LoopSummaryEvent, ts: i64) -> LoopSummaryEvent {
        row.next_run = Some(ts);
        row
    }

    fn paused(mut row: LoopSummaryEvent) -> LoopSummaryEvent {
        row.paused = true;
        row
    }

    fn retired(mut row: LoopSummaryEvent) -> LoopSummaryEvent {
        row.retired = true;
        row
    }

    fn loaded(loops: Vec<LoopSummaryEvent>) -> SpurEvent {
        SpurEvent::now(SpurEventBody::LoopsLoaded {
            loops,
            warnings: Vec::new(),
        })
    }

    fn detail(loop_id: &str) -> LoopDetailEvent {
        LoopDetailEvent {
            loop_id: loop_id.into(),
            issue_id: format!("bd-{loop_id}"),
            title: format!("Loop {loop_id}"),
            goal_preview: Some("Keep production checks moving".into()),
            cadence_secs: 900,
            effective_interval_secs: 1800,
            backoff_active: true,
            paused: false,
            next_run: None,
            consecutive_failures: 1,
            budget_micros_per_generation: Some(500_000),
            max_generations_per_day: Some(4),
            max_tasks: Some(3),
            recent_runs: vec![LoopRunRecordEvent {
                generation: 7,
                outcome: "approved".into(),
                cost_micros: 250_000,
                autonomy: Some("l2".into()),
            }],
        }
    }

    #[test]
    fn loads_sort_by_next_run_and_preserves_selection_across_refresh() {
        let lineage = ExecutorLineage::new();
        let projection = PlanProjectionStore::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let mut view = LoopBrowserView::new();

        view.handle_spur_event(
            &loaded(vec![
                with_next(
                    summary("later"),
                    (now + chrono::Duration::hours(2)).timestamp(),
                ),
                with_next(
                    summary("soon"),
                    (now + chrono::Duration::minutes(15)).timestamp(),
                ),
            ]),
            &ctx,
        );

        assert_eq!(view.selected_loop().unwrap().loop_id(), "soon");
        view.handle_key(key(KeyCode::Char('j')), &ctx);
        assert_eq!(view.selected_loop().unwrap().loop_id(), "later");

        view.handle_spur_event(
            &loaded(vec![
                with_next(
                    summary("soon"),
                    (now + chrono::Duration::minutes(5)).timestamp(),
                ),
                with_next(
                    summary("later"),
                    (now + chrono::Duration::minutes(10)).timestamp(),
                ),
            ]),
            &ctx,
        );

        assert_eq!(view.selected_loop().unwrap().loop_id(), "later");
    }

    #[test]
    fn filters_active_paused_and_retired_loops() {
        let lineage = ExecutorLineage::new();
        let projection = PlanProjectionStore::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = LoopBrowserView::new();
        view.handle_spur_event(
            &loaded(vec![
                summary("active"),
                paused(summary("paused")),
                retired(summary("retired")),
            ]),
            &ctx,
        );

        assert_eq!(view.view_index.len(), 3);

        view.handle_key(key(KeyCode::Char('f')), &ctx);
        assert_eq!(view.filter_mode, FilterMode::Active);
        assert_eq!(view.selected_loop().unwrap().loop_id(), "active");

        view.handle_key(key(KeyCode::Char('f')), &ctx);
        assert_eq!(view.filter_mode, FilterMode::Paused);
        assert_eq!(view.selected_loop().unwrap().loop_id(), "paused");

        view.handle_key(key(KeyCode::Char('f')), &ctx);
        assert_eq!(view.filter_mode, FilterMode::Retired);
        assert_eq!(view.selected_loop().unwrap().loop_id(), "retired");
    }

    #[test]
    fn enter_requests_detail_then_toggles_detail_peek_closed() {
        let lineage = ExecutorLineage::new();
        let projection = PlanProjectionStore::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = LoopBrowserView::new();
        view.handle_spur_event(&loaded(vec![summary("loop-a")]), &ctx);

        let action = view.handle_key(key(KeyCode::Enter), &ctx);

        assert!(matches!(
            action,
            Some(Action::InspectLoop { loop_id }) if loop_id == "loop-a"
        ));
        assert_eq!(view.detail_peek, DetailPeek::Detail);

        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::LoopDetailLoaded {
                detail: detail("loop-a"),
            }),
            &ctx,
        );
        assert_eq!(view.detail.as_ref().unwrap().recent_runs.len(), 1);

        let second = view.handle_key(key(KeyCode::Enter), &ctx);

        assert!(second.is_none());
        assert_eq!(view.detail_peek, DetailPeek::Summary);
    }

    #[test]
    fn live_loop_events_update_rows_detail_and_unknown_activity_hint() {
        let lineage = ExecutorLineage::new();
        let projection = PlanProjectionStore::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = LoopBrowserView::new();
        view.handle_spur_event(&loaded(vec![summary("loop-a")]), &ctx);
        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::LoopDetailLoaded {
                detail: detail("loop-a"),
            }),
            &ctx,
        );
        view.detail_peek = DetailPeek::Detail;

        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::LoopArmed {
                loop_id: "loop-a".into(),
                generation: 8,
                next_run: 1_800_000_000,
            }),
            &ctx,
        );
        assert_eq!(
            view.selected_loop().unwrap().summary.last_generation,
            Some(8)
        );
        assert_eq!(
            view.selected_loop().unwrap().summary.next_run,
            Some(1_800_000_000)
        );

        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::LoopRunRecorded {
                loop_id: "loop-a".into(),
                generation: 8,
                outcome: "failed".into(),
                cost_micros: 125_000,
            }),
            &ctx,
        );
        let row = view.selected_loop().unwrap();
        assert_eq!(row.summary.last_outcome.as_deref(), Some("failed"));
        assert_eq!(view.detail.as_ref().unwrap().recent_runs[0].generation, 8);

        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::LoopPaused {
                loop_id: "loop-a".into(),
                by: "auto_paused".into(),
            }),
            &ctx,
        );
        assert_eq!(view.selected_loop().unwrap().state().label(), "auto-paused");

        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::LoopArmed {
                loop_id: "new-loop".into(),
                generation: 1,
                next_run: 1_800_000_100,
            }),
            &ctx,
        );
        assert_eq!(view.hint.as_deref(), Some("new loop activity — press r"));
    }

    #[test]
    fn pause_resume_and_kill_use_confirmation_modals() {
        let lineage = ExecutorLineage::new();
        let projection = PlanProjectionStore::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = LoopBrowserView::new();
        view.handle_spur_event(&loaded(vec![summary("loop-a")]), &ctx);

        assert!(view.handle_key(key(KeyCode::Char('p')), &ctx).is_none());
        assert!(matches!(view.confirm, Some(LoopConfirm::Pause { .. })));
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter), &ctx),
            Some(Action::PauseLoop { loop_id }) if loop_id == "loop-a"
        ));

        view.handle_spur_event(
            &SpurEvent::now(SpurEventBody::LoopPaused {
                loop_id: "loop-a".into(),
                by: "paused".into(),
            }),
            &ctx,
        );
        assert!(view.handle_key(key(KeyCode::Char('p')), &ctx).is_none());
        assert!(matches!(view.confirm, Some(LoopConfirm::Resume { .. })));
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter), &ctx),
            Some(Action::ResumeLoop { loop_id }) if loop_id == "loop-a"
        ));

        assert!(view.handle_key(key(KeyCode::Char('x')), &ctx).is_none());
        assert!(matches!(view.confirm, Some(LoopConfirm::Kill { .. })));
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter), &ctx),
            Some(Action::KillLoop { loop_id }) if loop_id == "loop-a"
        ));
    }

    #[test]
    fn retired_loops_flash_hint_instead_of_opening_mutation_modal() {
        let lineage = ExecutorLineage::new();
        let projection = PlanProjectionStore::new();
        let synopsis = SessionSynopsisProjection::new();
        let ctx = ctx(&lineage, &projection, &synopsis);
        let mut view = LoopBrowserView::new();
        view.handle_spur_event(&loaded(vec![retired(summary("loop-a"))]), &ctx);

        let action = view.handle_key(key(KeyCode::Char('p')), &ctx);

        assert!(view.confirm.is_none());
        assert!(matches!(
            action,
            Some(Action::FlashHint { message }) if message.contains("loop is retired")
        ));
    }
}
