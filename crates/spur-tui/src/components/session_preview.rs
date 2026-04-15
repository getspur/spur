use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

/// Content passed to `SessionPreview::render` per-frame.
#[derive(Default)]
pub struct PreviewContent {
    /// Ordered label/value pairs shown as a simple metadata list.
    pub rows: Vec<(String, String)>,
    /// If present, shown instead of `rows` (e.g., the `[+ New session]` hint).
    pub placeholder: Option<String>,
}

pub struct SessionPreview;

impl SessionPreview {
    pub fn render(frame: &mut Frame, area: Rect, content: &PreviewContent) {
        let block = Block::default()
            .title(" Preview ")
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray));

        let lines: Vec<Line> = if let Some(ref msg) = content.placeholder {
            vec![Line::from(Span::styled(
                msg.clone(),
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            content
                .rows
                .iter()
                .map(|(label, value)| {
                    Line::from(vec![
                        Span::styled(
                            format!("  {}: ", label),
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(value.clone(), Style::default().fg(Color::White)),
                    ])
                })
                .collect()
        };

        let p = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false });
        frame.render_widget(p, area);
    }
}
