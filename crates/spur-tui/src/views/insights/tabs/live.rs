//! Stateless renderer for the Insights live tab.

use crate::views::insights::state::InsightsSnapshot;
use chrono::{DateTime, Utc};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Gauge, Paragraph},
    Frame,
};
use spur_context::LiveBlockRow;

pub struct LiveTab;

impl LiveTab {
    pub fn render(frame: &mut Frame, area: Rect, snap: &InsightsSnapshot) {
        let block = Block::default().borders(Borders::ALL).title("Live");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let max_rows = inner.height.saturating_sub(4) as usize / 2;
        let visible: Vec<_> = snap.queries.live_30min.iter().take(max_rows).collect();
        let metrics: Vec<_> = visible
            .iter()
            .map(|row| LiveMetrics::from_row(row))
            .collect();
        let avg_tpm = running_average_tpm(&metrics);
        let total_hourly: f64 = metrics.iter().filter_map(|metric| metric.hourly).sum();

        let mut constraints = vec![Constraint::Length(1), Constraint::Length(1)];
        constraints.extend(metrics.iter().map(|_| Constraint::Length(2)));
        constraints.push(Constraint::Min(1));
        let chunks = Layout::vertical(constraints).split(inner);

        frame.render_widget(
            Paragraph::new("Active sessions (last 30 min)       refresh: 5s"),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new(
                "session_id  agent          model                  tokens      tpm        burn $/min  $/hr proj",
            ),
            chunks[1],
        );

        for (idx, (row, metric)) in visible.iter().zip(metrics.iter()).enumerate() {
            render_live_row(frame, chunks[idx + 2], row, metric, avg_tpm);
        }

        if let Some(footer_area) = chunks.last().copied() {
            frame.render_widget(
                Paragraph::new(format!("total $/hr proj: {}", dollars(total_hourly))),
                footer_area,
            );
        }
    }
}

#[derive(Debug)]
struct LiveMetrics {
    token_count: i64,
    tokens_per_minute: Option<f64>,
    burn_per_minute: Option<f64>,
    hourly: Option<f64>,
}

impl LiveMetrics {
    fn from_row(row: &LiveBlockRow) -> Self {
        let token_count = row.input_tokens
            + row.output_tokens
            + row.cache_read_tokens
            + row.cache_creation_tokens;
        let minutes = minutes_since_started_at(row.started_at.as_deref());
        let burn_per_minute = minutes.map(|minutes| row.cost_usd / minutes);
        let tokens_per_minute = minutes.map(|minutes| token_count as f64 / minutes);
        let hourly = burn_per_minute.map(|burn| burn * 60.0);

        Self {
            token_count,
            tokens_per_minute,
            burn_per_minute,
            hourly,
        }
    }
}

fn render_live_row(
    frame: &mut Frame,
    area: Rect,
    row: &LiveBlockRow,
    metric: &LiveMetrics,
    avg_tpm: f64,
) {
    let [text_area, gauge_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let columns = Layout::horizontal([
        Constraint::Length(12),
        Constraint::Length(15),
        Constraint::Length(23),
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Length(12),
        Constraint::Length(12),
    ])
    .split(text_area);

    let prefix = &row.session_id[..row.session_id.len().min(8)];
    let model = row.model.as_deref().unwrap_or("—");
    let burn = metric
        .burn_per_minute
        .map(dollars)
        .unwrap_or_else(|| "—".to_string());
    let hourly = metric
        .hourly
        .map(dollars)
        .unwrap_or_else(|| "—".to_string());

    frame.render_widget(Paragraph::new(prefix.to_string()), columns[0]);
    frame.render_widget(Paragraph::new(row.agent.clone()), columns[1]);
    frame.render_widget(Paragraph::new(model.to_string()), columns[2]);
    frame.render_widget(Paragraph::new(compact(metric.token_count)), columns[3]);
    frame.render_widget(
        Paragraph::new(
            metric
                .tokens_per_minute
                .map(|tpm| format!("{:.1}/m", tpm))
                .unwrap_or_else(|| "—".to_string()),
        ),
        columns[4],
    );
    frame.render_widget(Paragraph::new(burn), columns[5]);
    frame.render_widget(Paragraph::new(hourly), columns[6]);

    let tpm_value = metric.tokens_per_minute.unwrap_or(0.0);
    let ratio = if avg_tpm > 0.0 {
        (tpm_value / avg_tpm).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let label = match metric.tokens_per_minute {
        Some(tpm) => format!("tokens/min {:.1}", tpm),
        None => "tokens/min —".to_string(),
    };
    let gauge = Gauge::default()
        .ratio(ratio)
        .label(label)
        .gauge_style(Style::default().fg(Color::Cyan));
    frame.render_widget(gauge, gauge_area);
}

fn running_average_tpm(metrics: &[LiveMetrics]) -> f64 {
    let active: Vec<f64> = metrics
        .iter()
        .filter_map(|metric| metric.tokens_per_minute)
        .filter(|tpm| *tpm > 0.0)
        .collect();
    if active.is_empty() {
        return 0.0;
    }
    active.iter().sum::<f64>() / active.len() as f64
}

fn minutes_since_started_at(started_at: Option<&str>) -> Option<f64> {
    let started = DateTime::parse_from_rfc3339(started_at?).ok()?;
    let seconds = (Utc::now() - started.with_timezone(&Utc)).num_seconds();
    (seconds > 0).then_some(seconds as f64 / 60.0)
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
    use crate::views::insights::tabs::{render_to_text, synthetic_snapshot};
    use ratatui::layout::Rect;
    use spur_context::LiveBlockRow;

    fn live(session_id: &str) -> LiveBlockRow {
        LiveBlockRow {
            session_id: session_id.to_string(),
            agent: "codex".to_string(),
            model: Some("gpt-5-codex".to_string()),
            started_at: Some("2026-04-28T00:00:00Z".to_string()),
            last_activity: None,
            input_tokens: 1_200,
            output_tokens: 300,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd: 0.60,
            events: 5,
        }
    }

    #[test]
    fn live_tab_renders_session_prefixes() {
        let mut snap = synthetic_snapshot();
        snap.queries.live_30min = vec![live("abc12345-session"), live("def45678-session")];

        let text = render_to_text(|frame| {
            LiveTab::render(frame, Rect::new(0, 0, 120, 30), &snap);
        });

        assert!(text.contains("Live"), "rendered:\n{text}");
        assert!(text.contains("abc12345"), "rendered:\n{text}");
        assert!(text.contains("def45678"), "rendered:\n{text}");
    }
}
