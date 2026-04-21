use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

/// Modal shown when the user requests quit.
/// Makes the exit consequence explicit and advertises the force-quit chord.
pub struct QuitConfirmDialog;

impl QuitConfirmDialog {
    pub fn render(frame: &mut Frame, area: Rect, brain_name: Option<&str>) {
        let width = 56u16.min(area.width.saturating_sub(4));
        let height = 10u16.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let popup_area = Rect::new(x, y, width, height);

        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Quit spur? ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));

        let prompt = match brain_name {
            Some(name) => format!("  Shut down brain agent \"{name}\" and quit?"),
            None => "  Quit spur?".to_string(),
        };
        let detail = match brain_name {
            Some(_) => "  The agent subprocess will be terminated.",
            None => "  Unsent input is saved before exit.",
        };

        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(prompt, Style::default().fg(Color::White))),
            Line::from(Span::styled(detail, Style::default().fg(Color::DarkGray))),
            Line::from(Span::styled(
                "  Press Ctrl+C again to exit immediately.",
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
                Span::raw(" No    "),
                Span::styled(
                    "[Ctrl+C]",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" Force"),
            ]),
        ];

        let paragraph = Paragraph::new(lines).block(block);
        frame.render_widget(paragraph, popup_area);
    }
}
