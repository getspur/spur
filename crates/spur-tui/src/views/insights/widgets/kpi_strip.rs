//! Stateless KPI strip widget for the Insights overview tab.

use crate::views::insights::state::Kpis;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

/// Render the four overview KPI cards.
pub fn render_kpi_strip(frame: &mut Frame, area: Rect, kpis: &Kpis) {
    let chunks = Layout::horizontal([
        Constraint::Percentage(25),
        Constraint::Percentage(25),
        Constraint::Percentage(25),
        Constraint::Percentage(25),
    ])
    .split(area);

    let cards = [
        (
            "Today",
            format!(
                "{}\nactive sessions: {}",
                dollars(kpis.today_cost),
                kpis.active_session_count
            ),
        ),
        ("7d", format!("{}\nlast 7 days", dollars(kpis.last_7d_cost))),
        (
            "30d",
            format!("{}\nlast 30 days", dollars(kpis.last_30d_cost)),
        ),
        (
            "Cache hit",
            format!("{:.1}%\nread reuse", kpis.cache_hit_pct),
        ),
    ];

    for (idx, (title, body)) in cards.into_iter().enumerate() {
        let paragraph = Paragraph::new(body)
            .block(Block::default().borders(Borders::ALL).title(title))
            .style(Style::default().fg(Color::White));
        frame.render_widget(paragraph, chunks[idx]);
    }
}

fn dollars(cost: f64) -> String {
    format!("${:.2}", cost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::insights::state::Kpis;
    use ratatui::{backend::TestBackend, buffer::Buffer, layout::Rect, Terminal};

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

    #[test]
    fn renders_four_kpi_cards() {
        let kpis = Kpis {
            today_cost: 4.21,
            last_7d_cost: 28.40,
            last_30d_cost: 112.00,
            cache_hit_pct: 47.8,
            active_session_count: 3,
            ..Default::default()
        };

        let text = render_to_text(120, 10, |frame| {
            render_kpi_strip(frame, Rect::new(0, 0, 120, 10), &kpis);
        });

        assert!(text.contains("$4.21"), "rendered:\n{text}");
        assert!(text.contains("$28.40"), "rendered:\n{text}");
        assert!(text.contains("$112.00"), "rendered:\n{text}");
        assert!(text.contains("47.8%"), "rendered:\n{text}");
    }
}
