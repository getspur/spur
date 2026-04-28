//! Stateless renderer for the Insights breakdown tab.

use crate::views::insights::state::{Dimension, InsightsSnapshot};
use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};

pub struct BreakdownTab;

impl BreakdownTab {
    pub fn render(frame: &mut Frame, area: Rect, snap: &InsightsSnapshot, dimension: Dimension) {
        let block = Block::default().borders(Borders::ALL).title("Breakdown");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let [header_area, table_area] =
            ratatui::layout::Layout::vertical([Constraint::Length(2), Constraint::Min(0)])
                .areas(inner);

        frame.render_widget(dimension_header(dimension), header_area);
        frame.render_widget(table(rows(snap, dimension), dimension), table_area);
    }
}

#[derive(Debug)]
struct BreakdownRow {
    name: String,
    sessions: i64,
    input_tokens: i64,
    output_tokens: i64,
    cost: f64,
}

fn dimension_header(dimension: Dimension) -> Paragraph<'static> {
    Paragraph::new(Line::from(vec![
        Span::raw("Dimension: "),
        selectable("Agent", "A", dimension == Dimension::Agent),
        Span::raw(" / "),
        selectable("Model", "M", dimension == Dimension::Model),
        Span::raw(" / "),
        selectable("Project", "P", dimension == Dimension::Project),
        Span::raw("       window: last 30 days"),
    ]))
}

fn selectable(label: &'static str, key: &'static str, active: bool) -> Span<'static> {
    let text = if active {
        format!("[{key}]{}", &label[1..])
    } else {
        label.to_string()
    };
    let style = if active {
        Style::default().add_modifier(Modifier::UNDERLINED)
    } else {
        Style::default()
    };
    Span::styled(text, style)
}

fn rows(snap: &InsightsSnapshot, dimension: Dimension) -> Vec<BreakdownRow> {
    let mut rows: Vec<_> = match dimension {
        Dimension::Agent => snap
            .queries
            .by_agent_30d
            .iter()
            .map(|row| BreakdownRow {
                name: row.agent.clone(),
                sessions: row.sessions,
                input_tokens: row.input_tokens,
                output_tokens: row.output_tokens,
                cost: row.cost_usd,
            })
            .collect(),
        Dimension::Model => snap
            .queries
            .by_model_30d
            .iter()
            .map(|row| BreakdownRow {
                name: row.model.clone(),
                sessions: row.requests,
                input_tokens: row.input_tokens,
                output_tokens: row.output_tokens,
                cost: row.total_cost,
            })
            .collect(),
        Dimension::Project => snap
            .queries
            .by_project_30d
            .iter()
            .map(|row| BreakdownRow {
                name: row.project.clone(),
                sessions: row.sessions,
                input_tokens: row.input_tokens,
                output_tokens: row.output_tokens,
                cost: row.cost_usd,
            })
            .collect(),
    };

    rows.sort_by(|a, b| b.cost.total_cmp(&a.cost));
    rows
}

fn table(rows: Vec<BreakdownRow>, dimension: Dimension) -> Table<'static> {
    let title = match dimension {
        Dimension::Agent => "Agent",
        Dimension::Model => "Model",
        Dimension::Project => "Project",
    };
    let header = Row::new([
        Cell::from(title),
        Cell::from("Sessions"),
        Cell::from("Tokens (in/out)"),
        Cell::from("Cost"),
        Cell::from("Cost source"),
    ])
    .style(Style::default().fg(Color::Yellow));

    let rows = rows.into_iter().map(|row| {
        // TODO(c-cleanup): wire cost_source per row when DTOs surface it
        let cost_source = "—";
        Row::new([
            Cell::from(row.name),
            Cell::from(row.sessions.to_string()),
            Cell::from(tokens(row.input_tokens, row.output_tokens)),
            Cell::from(dollars(row.cost)),
            Cell::from(cost_source),
        ])
    });

    Table::new(
        rows,
        [
            Constraint::Min(24),
            Constraint::Length(12),
            Constraint::Length(22),
            Constraint::Length(12),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("30d spend"))
}

fn tokens(input: i64, output: i64) -> String {
    format!("{} / {}", compact(input), compact(output))
}

fn compact(value: i64) -> String {
    let value = value as f64;
    if value.abs() >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value.abs() >= 1_000.0 {
        format!("{:.1}K", value / 1_000.0)
    } else {
        format!("{value:.0}")
    }
}

fn dollars(cost: f64) -> String {
    format!("${:.2}", cost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::insights::state::Dimension;
    use crate::views::insights::tabs::{render_to_text, synthetic_snapshot};
    use ratatui::layout::Rect;
    use spur_context::ModelRow;

    fn model(name: &str, total_cost: f64) -> ModelRow {
        ModelRow {
            model: name.to_string(),
            agent: "codex".to_string(),
            requests: 8,
            input_tokens: 1_000,
            output_tokens: 250,
            avg_cost: total_cost / 8.0,
            total_cost,
        }
    }

    #[test]
    fn breakdown_tab_renders_model_rows() {
        let mut snap = synthetic_snapshot();
        snap.queries.by_model_30d = vec![
            model("claude-opus-4-5", 74.50),
            model("gpt-5-codex", 52.40),
            model("claude-sonnet-4", 14.62),
        ];

        let text = render_to_text(|frame| {
            BreakdownTab::render(frame, Rect::new(0, 0, 120, 30), &snap, Dimension::Model);
        });

        assert!(text.contains("Breakdown"), "rendered:\n{text}");
        assert!(text.contains("[M]odel"), "rendered:\n{text}");
        assert!(text.contains("claude-opus-4-5"), "rendered:\n{text}");
        assert!(text.contains("gpt-5-codex"), "rendered:\n{text}");
        assert!(text.contains("claude-sonnet-4"), "rendered:\n{text}");
    }
}
