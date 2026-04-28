//! Stateless renderer for the Insights overview tab.

use crate::views::insights::state::InsightsSnapshot;
use crate::views::insights::widgets::{render_kpi_strip, render_sparkline};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Gauge, List, ListItem},
    Frame,
};

pub struct OverviewTab;

impl OverviewTab {
    pub fn render(frame: &mut Frame, area: Rect, snap: &InsightsSnapshot) {
        let block = Block::default().borders(Borders::ALL).title("Overview");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let [kpis_area, spark_area, provenance_area, lists_area] = Layout::vertical([
            Constraint::Length(5),
            Constraint::Length(5),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .areas(inner);

        render_kpi_strip(frame, kpis_area, &snap.kpis);
        let rows = snap
            .queries
            .daily_90
            .get(60..90)
            .unwrap_or(snap.queries.daily_90.as_slice());
        render_sparkline(frame, spark_area, rows, "7d Sparkline");
        Self::render_provenance(frame, provenance_area, snap);
        Self::render_top_lists(frame, lists_area, snap);
    }

    fn render_provenance(frame: &mut Frame, area: Rect, snap: &InsightsSnapshot) {
        let split = &snap.kpis.cost_source_split;
        let ratio = (split.native_pct / 100.0).clamp(0.0, 1.0);
        let label = format!(
            "{:.1}% native | {:.1}% priced | {:.1}% unpriced",
            split.native_pct, split.priced_pct, split.unpriced_pct
        );
        let gauge = Gauge::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Cost provenance"),
            )
            .ratio(ratio)
            .label(label)
            .gauge_style(Style::default().fg(Color::Green));

        frame.render_widget(gauge, area);
    }

    fn render_top_lists(frame: &mut Frame, area: Rect, snap: &InsightsSnapshot) {
        let chunks = Layout::horizontal([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(area);

        frame.render_widget(
            top_list(
                "Top agents (30d)",
                sorted_top(
                    snap.queries
                        .by_agent_30d
                        .iter()
                        .map(|row| (row.agent.as_str(), row.cost_usd)),
                ),
            ),
            chunks[0],
        );
        frame.render_widget(
            top_list(
                "Top models (30d)",
                sorted_top(
                    snap.queries
                        .by_model_30d
                        .iter()
                        .map(|row| (row.model.as_str(), row.total_cost)),
                ),
            ),
            chunks[1],
        );
        frame.render_widget(
            top_list(
                "Top projects (30d)",
                sorted_top(
                    snap.queries
                        .by_project_30d
                        .iter()
                        .map(|row| (row.project.as_str(), row.cost_usd)),
                ),
            ),
            chunks[2],
        );
    }
}

fn top_list(title: &'static str, rows: Vec<(String, f64)>) -> List<'static> {
    let items: Vec<ListItem<'static>> = rows
        .into_iter()
        .enumerate()
        .map(|(idx, (name, cost))| {
            ListItem::new(format!("{}. {:<18} {}", idx + 1, name, dollars(cost)))
        })
        .collect();

    List::new(items).block(Block::default().borders(Borders::ALL).title(title))
}

fn sorted_top<'a>(rows: impl Iterator<Item = (&'a str, f64)>) -> Vec<(String, f64)> {
    let mut rows: Vec<_> = rows.map(|(name, cost)| (name.to_string(), cost)).collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    rows.into_iter().take(3).collect()
}

fn dollars(cost: f64) -> String {
    format!("${:.2}", cost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::insights::tabs::{render_to_text, synthetic_snapshot};
    use ratatui::layout::Rect;

    #[test]
    fn overview_tab_renders_kpis_and_sparkline() {
        let snap = synthetic_snapshot();
        let text = render_to_text(|frame| {
            OverviewTab::render(frame, Rect::new(0, 0, 120, 30), &snap);
        });

        assert!(text.contains("Overview"), "rendered:\n{text}");
        assert!(text.contains("$4.21"), "rendered:\n{text}");
        assert!(text.contains("$28.40"), "rendered:\n{text}");
        assert!(text.contains("$112.00"), "rendered:\n{text}");
        assert!(text.contains("47.8%"), "rendered:\n{text}");
    }
}
