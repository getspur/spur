use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Style, Stylize},
    widgets::{Block, Cell, Row, Table},
    Frame,
};

use spur_pm::IssueSummary;

pub struct IssuesPanel;

impl IssuesPanel {
    pub fn render(issues: &[IssueSummary], frame: &mut Frame, area: Rect) {
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

        let table = Table::new(rows, widths)
            .header(header)
            .block(Block::bordered().title(" Issues "));

        frame.render_widget(table, area);
    }

    pub fn computed_height(issue_count: usize, available_height: u16) -> u16 {
        let max_panel = (available_height / 4).max(3);
        (issue_count as u16 + 3).min(max_panel)
    }
}
