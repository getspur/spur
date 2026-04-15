use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

/// Modal shown when the user requests quit while a brain agent is attached.
/// Makes the "shutting down the agent subprocess" consequence explicit so
/// users don't kill streaming turns by accident.
pub struct QuitConfirmDialog;

impl QuitConfirmDialog {
    pub fn render(frame: &mut Frame, area: Rect, brain_name: &str) {
        let width = 56u16.min(area.width.saturating_sub(4));
        let height = 9u16.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let popup_area = Rect::new(x, y, width, height);

        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Quit spur? ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));

        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  Shut down brain agent \"{brain_name}\" and quit?"),
                Style::default().fg(Color::White),
            )),
            Line::from(Span::styled(
                "  The agent subprocess will be terminated.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "[y]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" Yes    "),
                Span::styled(
                    "[n/Esc]",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" No"),
            ]),
        ];

        let paragraph = Paragraph::new(lines).block(block);
        frame.render_widget(paragraph, popup_area);
    }
}
