use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::theme::{resolve_token, ColorDepth, Theme};

fn token(theme: &Theme, name: &str) -> ratatui::style::Color {
    resolve_token(theme, name, ColorDepth::Truecolor)
}

/// A label/value row shown in the session preview pane.
#[derive(Debug, Clone, Default)]
pub struct PreviewRow {
    pub label: String,
    pub value_lines: Vec<String>,
    pub value_style: Option<Style>,
}

impl From<(String, String)> for PreviewRow {
    fn from((label, value): (String, String)) -> Self {
        Self {
            label,
            value_lines: vec![value],
            value_style: None,
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

fn build_lines_for_row(row: &PreviewRow, theme: &Theme) -> Vec<Line<'static>> {
    let value_style = row
        .value_style
        .unwrap_or_else(|| Style::default().fg(token(theme, "session_picker.preview.value.fg")));
    let label_style = Style::default()
        .fg(token(theme, "session_picker.preview.label.fg"))
        .add_modifier(Modifier::BOLD);

    if row.label.is_empty() {
        if row.value_lines.is_empty() {
            return vec![Line::from("")];
        }
        return row
            .value_lines
            .iter()
            .map(|value| {
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(value.clone(), value_style),
                ])
            })
            .collect();
    }

    let label = format!("  {}: ", row.label);
    let continuation = " ".repeat(label.chars().count());
    let values = if row.value_lines.is_empty() {
        vec![String::new()]
    } else {
        row.value_lines.clone()
    };

    values
        .into_iter()
        .enumerate()
        .map(|(idx, value)| {
            if idx == 0 {
                Line::from(vec![
                    Span::styled(label.clone(), label_style),
                    Span::styled(value, value_style),
                ])
            } else {
                Line::from(vec![
                    Span::raw(continuation.clone()),
                    Span::styled(value, value_style),
                ])
            }
        })
        .collect()
}

impl SessionPreview {
    pub fn render(frame: &mut Frame, area: Rect, content: &PreviewContent, theme: &Theme) {
        let block = Block::default()
            .title(" Preview ")
            .borders(Borders::TOP)
            .border_style(Style::default().fg(token(theme, "session_picker.preview.border.fg")));

        let lines: Vec<Line> = if let Some(ref msg) = content.placeholder {
            vec![Line::from(Span::styled(
                msg.clone(),
                Style::default().fg(token(theme, "session_picker.preview.label.fg")),
            ))]
        } else {
            content
                .rows
                .iter()
                .flat_map(|row| build_lines_for_row(row, theme))
                .collect()
        };

        let p = Paragraph::new(lines).block(block);
        frame.render_widget(p, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Style};

    #[test]
    fn from_tuple_creates_unstyled_single_line_row() {
        let row: PreviewRow = ("Label".to_string(), "Value".to_string()).into();
        assert_eq!(row.label, "Label");
        assert_eq!(row.value_lines, vec!["Value"]);
        assert!(row.value_style.is_none());
    }

    #[test]
    fn explicit_construction_with_style_and_lines() {
        let row = PreviewRow {
            label: "Intent".into(),
            value_lines: vec!["long".into(), "wrapped value".into()],
            value_style: Some(Style::default().fg(Color::Gray)),
        };
        assert_eq!(row.label, "Intent");
        assert_eq!(row.value_lines.len(), 2);
        assert!(row.value_style.is_some());
    }

    #[test]
    fn unstyled_value_uses_preview_value_token() {
        let row = PreviewRow {
            label: "Title".into(),
            value_lines: vec!["Draft".into()],
            value_style: None,
        };
        let expected = crate::theme::resolve_token(
            crate::theme::fallback_theme(),
            "session_picker.preview.value.fg",
            crate::theme::ColorDepth::Truecolor,
        );

        let lines = build_lines_for_row(&row, crate::theme::fallback_theme());

        assert_eq!(lines[0].spans[1].style.fg, Some(expected));
    }
}
