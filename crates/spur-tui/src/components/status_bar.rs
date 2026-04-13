use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::action::ViewId;

pub struct StatusBar;

/// Everything the status bar needs to render one frame.
#[derive(Clone, Copy)]
pub struct StatusBarProps<'a> {
    pub view: &'a ViewId,
    pub total_cost: f64,
    pub elapsed: &'a str,
    pub current_mode: Option<&'a str>,
    pub context_used: Option<u64>,
    pub context_size: Option<u64>,
}

impl StatusBar {
    pub fn render(frame: &mut Frame, area: Rect, props: StatusBarProps<'_>) {
        let hints = match props.view {
            ViewId::Dashboard => " [i]nput [Enter]session [s]essions [?]help [q]uit",
            ViewId::SessionDetail(_) => " [Enter]send [Esc]back [j/k]scroll [?]help",
            ViewId::SessionPicker => " [\u{2191}\u{2193}]navigate [Enter]select [Esc]back",
        };

        let mode_text = props
            .current_mode
            .filter(|m| !m.is_empty())
            .map(|m| format!(" [{m}]"))
            .unwrap_or_default();

        let usage_text = match (props.context_used, props.context_size) {
            (Some(used), Some(size)) if size > 0 => {
                let pct = (used as f64 / size as f64) * 100.0;
                format!(" ctx {:.0}%", pct)
            }
            _ => String::new(),
        };

        let right = Line::from(vec![
            Span::styled(
                format!("${:.2}", props.total_cost),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!(" {} ", props.elapsed),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(mode_text, Style::default().fg(Color::Magenta)),
            Span::styled(usage_text, Style::default().fg(Color::LightBlue)),
            Span::raw(" "),
            Span::styled(
                "SPUR",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        ]);

        let right_width = right.width() as u16;
        let hints_line = Line::from(Span::styled(
            hints,
            Style::default().fg(Color::White).add_modifier(Modifier::DIM),
        ));

        // Right-align the metric/brand group; let the hints take the rest.
        let [hints_area, right_area] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(right_width.max(1)),
        ])
        .areas(area);

        frame.render_widget(Paragraph::new(hints_line), hints_area);
        frame.render_widget(Paragraph::new(right).right_aligned(), right_area);
    }
}
