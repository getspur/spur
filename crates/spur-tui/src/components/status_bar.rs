use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::action::ViewId;

pub struct StatusBar;

impl StatusBar {
    pub fn render(
        frame: &mut Frame,
        area: Rect,
        view: &ViewId,
        total_cost: f64,
        elapsed: &str,
        current_mode: Option<&str>,
        context_used: Option<u64>,
        context_size: Option<u64>,
    ) {
        let hints = match view {
            ViewId::Dashboard => " [i]nput [Enter]session [s]essions [?]help [q]uit",
            ViewId::SessionDetail(_) => " [Enter]send [Esc]back [j/k]scroll [?]help",
            ViewId::SessionPicker => " [\u{2191}\u{2193}]navigate [Enter]select [Esc]back",
        };

        let mode_text = current_mode
            .filter(|m| !m.is_empty())
            .map(|m| format!(" [{m}]"))
            .unwrap_or_default();

        let usage_text = match (context_used, context_size) {
            (Some(used), Some(size)) if size > 0 => {
                let pct = (used as f64 / size as f64) * 100.0;
                format!(" ctx {:.0}%", pct)
            }
            _ => String::new(),
        };

        let line = Line::from(vec![
            Span::styled(
                hints,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::DIM),
            ),
            Span::raw("  "),
            Span::styled(
                format!("${:.2}", total_cost),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!(" {} ", elapsed),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(mode_text, Style::default().fg(Color::Magenta)),
            Span::styled(usage_text, Style::default().fg(Color::LightBlue)),
            Span::raw(" "),
            Span::styled(
                "SPUR",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);

        frame.render_widget(Paragraph::new(line), area);
    }
}
