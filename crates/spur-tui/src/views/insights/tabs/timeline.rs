//! Stateless renderer for the Insights timeline tab.

use crate::views::insights::state::{Granularity, InsightsSnapshot};
use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Bar, BarChart, BarGroup, Block, Borders, Paragraph},
    Frame,
};

pub struct TimelineTab;

impl TimelineTab {
    pub fn render(
        frame: &mut Frame,
        area: Rect,
        snap: &InsightsSnapshot,
        granularity: Granularity,
    ) {
        let block = Block::default().borders(Borders::ALL).title("Timeline");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let [header_area, chart_area] =
            ratatui::layout::Layout::vertical([Constraint::Length(2), Constraint::Min(0)])
                .areas(inner);

        let periods = periods(snap, granularity);
        frame.render_widget(header(granularity, &periods), header_area);

        let bars: Vec<Bar<'static>> = periods
            .iter()
            .map(|(label, cost)| {
                Bar::default()
                    .value(scaled_cost(*cost))
                    .label(short_label(label).into())
                    .text_value(dollars(*cost))
            })
            .collect();
        let max = periods
            .iter()
            .map(|(_, cost)| scaled_cost(*cost))
            .max()
            .unwrap_or(1)
            .max(1);
        let chart = BarChart::default()
            .data(BarGroup::default().bars(&bars))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Cost by period"),
            )
            .bar_width(3)
            .bar_gap(1)
            .max(max)
            .bar_style(Style::default().fg(Color::Cyan))
            .value_style(Style::default().fg(Color::White))
            .label_style(Style::default().fg(Color::DarkGray));

        frame.render_widget(chart, chart_area);
    }
}

fn header(granularity: Granularity, periods: &[(String, f64)]) -> Paragraph<'static> {
    let range = match (periods.first(), periods.last()) {
        (Some((first, _)), Some((last, _))) if first != last => {
            format!("       range: {first}..{last}")
        }
        (Some((only, _)), _) => format!("       range: {only}"),
        _ => "       range: empty".to_string(),
    };

    Paragraph::new(Line::from(vec![
        Span::raw("Granularity: "),
        selectable("Daily", "D", granularity == Granularity::Daily),
        Span::raw(" / "),
        selectable("Weekly", "W", granularity == Granularity::Weekly),
        Span::raw(" / "),
        selectable("Monthly", "M", granularity == Granularity::Monthly),
        Span::raw(range),
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

fn periods(snap: &InsightsSnapshot, granularity: Granularity) -> Vec<(String, f64)> {
    match granularity {
        Granularity::Daily => snap
            .queries
            .daily_90
            .iter()
            .map(|row| (row.day.clone(), row.cost_usd))
            .collect(),
        Granularity::Weekly => snap
            .queries
            .weekly_12
            .iter()
            .map(|row| (row.week.clone(), row.cost_usd))
            .collect(),
        Granularity::Monthly => snap
            .queries
            .monthly_6
            .iter()
            .map(|row| (row.month.clone(), row.cost_usd))
            .collect(),
    }
}

fn short_label(label: &str) -> String {
    if label.len() > 5 {
        label[label.len() - 5..].to_string()
    } else {
        label.to_string()
    }
}

fn scaled_cost(cost: f64) -> u64 {
    if cost <= 0.0 {
        0
    } else {
        ((cost * 100.0).round() as u64).max(1)
    }
}

fn dollars(cost: f64) -> String {
    format!("${:.2}", cost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::insights::state::Granularity;
    use crate::views::insights::tabs::{render_to_text, synthetic_snapshot};
    use ratatui::layout::Rect;
    use spur_context::DailyRow;

    fn day(day: &str, cost_usd: f64) -> DailyRow {
        DailyRow {
            day: day.to_string(),
            agent: "codex".to_string(),
            sessions: 1,
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd,
        }
    }

    #[test]
    fn timeline_tab_renders_daily_bars() {
        let mut snap = synthetic_snapshot();
        snap.queries.daily_90 = vec![
            day("2026-04-24", 1.0),
            day("2026-04-25", 2.0),
            day("2026-04-26", 3.0),
            day("2026-04-27", 4.0),
            day("2026-04-28", 5.0),
        ];

        let text = render_to_text(|frame| {
            TimelineTab::render(frame, Rect::new(0, 0, 120, 30), &snap, Granularity::Daily);
        });

        assert!(text.contains("Timeline"), "rendered:\n{text}");
        assert!(text.contains("Granularity"), "rendered:\n{text}");
        assert!(text.contains("2026-04-28"), "rendered:\n{text}");
        assert!(text.contains("█"), "rendered:\n{text}");
    }
}
