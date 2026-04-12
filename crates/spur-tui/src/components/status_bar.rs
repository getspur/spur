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
    ) {
        let hints = match view {
            ViewId::Dashboard => " [i]nput [Enter]session [r]un [c]ost [?]help [q]uit",
            ViewId::SessionDetail(_) => " [Enter]send [Esc]back [j/k]scroll [?]help",
            ViewId::SessionPicker => " [j/k]navigate [Enter]resume [Esc]back",
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
