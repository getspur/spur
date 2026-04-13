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
        // Center a 66x30 popup to accommodate the expanded key listing.
        let width = 66u16.min(area.width.saturating_sub(4));
        let height = 42u16.min(area.height.saturating_sub(4));
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
                " Dashboard — Lineage Tree",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  j / k              Move selection in lineage tree"),
            Line::from("  Enter              Focus selected node"),
            Line::from("  Esc                Unfocus (return to log) / quit"),
            Line::from("  \u{2190} / \u{2192}               Cycle detail tabs (when focused)"),
            Line::from("  c                  Toggle collapse on selected subtree"),
            Line::from("  r                  Jump to next pending review"),
            Line::from("  a / d / m / R      Approve / deny / modify / retry (review tab)"),
            Line::from(""),
            Line::from(Span::styled(
                " Dashboard — General",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  j/k, Up/Down       Scroll activity log"),
            Line::from("  g / G              Jump to top / bottom"),
            Line::from("  Tab                Cycle panel focus"),
            Line::from("  v                  Toggle verbose mode"),
            Line::from("  s                  Open session picker"),
            Line::from("  q, Esc             Quit"),
            Line::from(""),
            Line::from(Span::styled(
                " Session Picker",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  j/k, Up/Down       Navigate list"),
            Line::from("  Enter              Resume / create (on [+ New])"),
            Line::from("  /                  Focus search field"),
            Line::from("  n                  New session"),
            Line::from("  R                  Rename selected"),
            Line::from("  d                  Archive (or unarchive)"),
            Line::from("  p                  Toggle pin"),
            Line::from("  a                  Toggle show-archived"),
            Line::from("  P                  Toggle preview pane"),
            Line::from("  r                  Refresh list"),
            Line::from("  Esc                Clear filter → back"),
            Line::from(""),
            Line::from(Span::styled(
                " Session Detail",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  (type)             Input goes to chat bar"),
            Line::from("  Enter              Send message"),
            Line::from("  ! + Enter          Interrupt & send"),
            Line::from("  Esc                Back to Dashboard"),
            Line::from("  y / n / a          Permission: yes/no/always"),
            Line::from("  Alt-m              Toggle plan mode"),
            Line::from("  Alt-v              Open mermaid diagram viewer"),
            Line::from(""),
            Line::from(Span::styled(
                " Mermaid Viewer (overlay)",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  [ / ]              Cycle diagrams"),
            Line::from("  q / Esc            Close overlay"),
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
