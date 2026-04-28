use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

/// A label/value row shown in the session preview pane.
#[derive(Debug, Clone, Default)]
pub struct PreviewRow {
    pub label: String,
    pub value: String,
    pub value_style: Option<Style>,
    pub wrap: bool,
}

impl From<(String, String)> for PreviewRow {
    fn from((label, value): (String, String)) -> Self {
        Self {
            label,
            value,
            value_style: None,
            wrap: false,
        }
    }
}

/// Content passed to `SessionPreview::render` per-frame.
#[derive(Debug, Clone, Default)]
pub struct PreviewContent {
    /// Ordered label/value pairs shown as a simple metadata list.
    pub rows: Vec<PreviewRow>,
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
                .map(|row| {
                    let value_style = row
                        .value_style
                        .unwrap_or_else(|| Style::default().fg(Color::White));
                    if row.label.is_empty() {
                        if row.value.is_empty() {
                            // Both empty: pure blank line as visual separator.
                            Line::from("")
                        } else {
                            // Empty label: render value only with leading indent.
                            Line::from(vec![
                                Span::raw("  "),
                                Span::styled(row.value.clone(), value_style),
                            ])
                        }
                    } else {
                        Line::from(vec![
                            Span::styled(
                                format!("  {}: ", row.label),
                                Style::default()
                                    .fg(Color::DarkGray)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(row.value.clone(), value_style),
                        ])
                    }
                })
                .collect()
        };

        let p = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false });
        frame.render_widget(p, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Style};

    #[test]
    fn from_tuple_creates_unstyled_unwrapped_row() {
        let row: PreviewRow = ("Label".to_string(), "Value".to_string()).into();
        assert_eq!(row.label, "Label");
        assert_eq!(row.value, "Value");
        assert!(row.value_style.is_none());
        assert!(!row.wrap);
    }

    #[test]
    fn explicit_construction_with_style_and_wrap() {
        let row = PreviewRow {
            label: "Intent".into(),
            value: "long wrapped value".into(),
            value_style: Some(Style::default().fg(Color::Gray)),
            wrap: true,
        };
        assert_eq!(row.label, "Intent");
        assert!(row.value_style.is_some());
        assert!(row.wrap);
    }
}
