use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

pub struct HelpOverlay;

impl HelpOverlay {
    pub fn render(frame: &mut Frame, area: Rect) {
        // Center a 60x20 popup
        let width = 60u16.min(area.width.saturating_sub(4));
        let height = 20u16.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let popup_area = Rect::new(x, y, width, height);

        // Clear the background
        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Help ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        let help_text = vec![
            Line::from(Span::styled(
                " Dashboard",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  j/k, Up/Down    Scroll activity log"),
            Line::from("  g / G           Jump to top / bottom"),
            Line::from("  Tab             Cycle panel focus"),
            Line::from("  Enter, 1-9      Drill into session"),
            Line::from("  i               Chat with brain"),
            Line::from("  v               Toggle verbose mode"),
            Line::from("  q, Esc          Quit"),
            Line::from(""),
            Line::from(Span::styled(
                " Session Detail",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  (type)          Input goes to chat bar"),
            Line::from("  Enter           Send message"),
            Line::from("  ! + Enter       Interrupt & send"),
            Line::from("  Esc             Back to Dashboard"),
            Line::from("  y / n / a       Permission: yes/no/always"),
            Line::from(""),
            Line::from(Span::styled(
                " Press ? or Esc to close",
                Style::default().fg(Color::DarkGray),
            )),
        ];

        let paragraph = Paragraph::new(help_text).block(block);
        frame.render_widget(paragraph, popup_area);
    }
}
