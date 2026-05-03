use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style, Stylize},
    widgets::{Block, Cell, Row, Table, TableState},
    Frame,
};

use spur_pm::IssueSummary;

pub struct IssuesPanel {
    table_state: TableState,
    focused: bool,
}

impl IssuesPanel {
    pub fn new() -> Self {
        Self {
            table_state: TableState::default(),
            focused: false,
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn select_next(&mut self, count: usize, issue_count: usize) {
        if issue_count == 0 {
            return;
        }
        let current = self.table_state.selected().unwrap_or(0);
        let next = (current + count) % issue_count;
        self.table_state.select(Some(next));
    }

    pub fn select_prev(&mut self, count: usize, issue_count: usize) {
        if issue_count == 0 {
            return;
        }
        let current = self.table_state.selected().unwrap_or(0);
        let prev = (current + issue_count - (count % issue_count)) % issue_count;
        self.table_state.select(Some(prev));
    }

    pub fn select_first(&mut self) {
        self.table_state.select(Some(0));
    }

    pub fn select_last(&mut self, issue_count: usize) {
        if issue_count > 0 {
            self.table_state.select(Some(issue_count - 1));
        }
    }

    pub fn selected_id<'a>(&self, issues: &'a [IssueSummary]) -> Option<&'a str> {
        let idx = self.table_state.selected()?;
        issues.get(idx).map(|i| i.id.as_str())
    }

    /// Inc 3 (bd-d587.3): select the row whose id matches `id`. Returns `true`
    /// if the id was found and selection was moved; `false` otherwise (caller
    /// can stash the id for a later retry once `tracked_issues` is updated).
    pub fn select_by_id(&mut self, id: &str, issues: &[IssueSummary]) -> bool {
        if let Some(idx) = issues.iter().position(|i| i.id == id) {
            self.table_state.select(Some(idx));
            true
        } else {
            false
        }
    }

    pub fn render(&mut self, issues: &[IssueSummary], frame: &mut Frame, area: Rect) {
        if issues.is_empty() {
            return;
        }

        let header = Row::new(["ID", "P", "Type", "Status", "Assignee", "Title"])
            .style(Style::default().bold());

        let rows: Vec<Row> = issues
            .iter()
            .map(|issue| {
                let priority_cell = match issue.priority {
                    Some(0) => Cell::from("P0").fg(Color::Red),
                    Some(1) => Cell::from("P1").fg(Color::Yellow),
                    Some(2) => Cell::from("P2").fg(Color::White),
                    Some(3) => Cell::from("P3").fg(Color::DarkGray),
                    Some(4) => Cell::from("P4").fg(Color::DarkGray),
                    _ => Cell::from("--").fg(Color::DarkGray),
                };

                let status_cell = match issue.status.as_str() {
                    "open" => Cell::from("open").fg(Color::Green),
                    "in_progress" => Cell::from("wip").fg(Color::Cyan),
                    "blocked" => Cell::from("blk").fg(Color::Red),
                    "closed" => Cell::from("done").fg(Color::DarkGray),
                    other => Cell::from(other.to_string()).fg(Color::White),
                };

                Row::new([
                    Cell::from(issue.id.as_str()),
                    priority_cell,
                    Cell::from(issue.issue_type.as_deref().unwrap_or("--")),
                    status_cell,
                    Cell::from(issue.assignee.as_deref().unwrap_or("--")),
                    Cell::from(issue.title.as_str()),
                ])
            })
            .collect();

        let widths = [
            Constraint::Length(8),
            Constraint::Length(2),
            Constraint::Length(7),
            Constraint::Length(4),
            Constraint::Length(10),
            Constraint::Min(20),
        ];

        let selected_idx = self.table_state.selected().unwrap_or(0);
        let total = issues.len();
        let (border_style, title) = if self.focused {
            (
                Style::default().fg(Color::Cyan),
                format!(
                    " Issues {}/{} — [j/k] select · [Enter] detail · [W]ork ",
                    selected_idx + 1,
                    total
                ),
            )
        } else {
            (
                Style::default(),
                format!(" Issues {}/{} ", selected_idx + 1, total),
            )
        };

        let table = Table::new(rows, widths)
            .header(header)
            .block(Block::bordered().title(title).border_style(border_style))
            .row_highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            );

        frame.render_stateful_widget(table, area, &mut self.table_state);
    }

    pub fn computed_height(issue_count: usize, available_height: u16) -> u16 {
        let max_panel = (available_height / 4).max(3);
        (issue_count as u16 + 3).min(max_panel)
    }
}

impl Default for IssuesPanel {
    fn default() -> Self {
        Self::new()
    }
}
