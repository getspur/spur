//! Stateless sparkline widget for Insights time-series summaries.

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, Sparkline},
    Frame,
};
use spur_context::DailyRow;

/// Render a cost sparkline for daily rows with a caller-supplied title.
pub fn render_sparkline(frame: &mut Frame, area: Rect, rows: &[DailyRow], title: &str) {
    let data: Vec<u64> = rows.iter().map(|d| d.cost_usd.max(0.0) as u64).collect();
    let max = data.iter().copied().max().unwrap_or(1).max(1);
    let sparkline = Sparkline::default()
        .block(Block::default().borders(Borders::ALL).title(title))
        .data(&data)
        .max(max)
        .style(Style::default().fg(Color::Cyan));

    frame.render_widget(sparkline, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, buffer::Buffer, layout::Rect, Terminal};
    use spur_context::DailyRow;

    fn render_to_text(
        width: u16,
        height: u16,
        render: impl FnOnce(&mut ratatui::Frame<'_>),
    ) -> String {
        fn buffer_text(buf: &Buffer) -> String {
            let mut rendered = String::new();
            for y in 0..buf.area.height {
                for x in 0..buf.area.width {
                    rendered.push_str(buf[(x, y)].symbol());
                }
                rendered.push('\n');
            }
            rendered
        }

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(render).unwrap();
        buffer_text(terminal.backend().buffer())
    }

    fn day(idx: usize) -> DailyRow {
        DailyRow {
            day: format!("2026-04-{idx:02}"),
            agent: "codex".to_string(),
            sessions: 1,
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd: idx as f64,
        }
    }

    #[test]
    fn renders_sparkline_with_block_title() {
        let rows: Vec<_> = (1..=30).map(day).collect();

        let text = render_to_text(60, 6, |frame| {
            render_sparkline(frame, Rect::new(0, 0, 60, 6), &rows, "Test Spark");
        });

        assert!(text.contains("Test Spark"), "rendered:\n{text}");
    }
}
