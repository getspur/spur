use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

pub struct HelpOverlay;

impl HelpOverlay {
    pub fn render(frame: &mut Frame, area: Rect, mermaid_enabled: bool) {
        let width = 66u16.min(area.width.saturating_sub(4));
        let height = 42u16.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let popup_area = Rect::new(x, y, width, height);

        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Help ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        let paragraph = Paragraph::new(Self::lines(mermaid_enabled)).block(block);
        frame.render_widget(paragraph, popup_area);
    }

    /// Build the help text. Exposed so tests can assert on contents without
    /// constructing a `Frame`. `mermaid_enabled` suppresses Alt-v and the
    /// Mermaid Viewer section when false.
    pub fn lines(mermaid_enabled: bool) -> Vec<Line<'static>> {
        let header = |t: &'static str| {
            Line::from(Span::styled(
                t,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ))
        };

        let mut out: Vec<Line<'static>> = vec![
            header(" Dashboard — Lineage Tree"),
            Line::from("  j / k              Move selection in lineage tree"),
            Line::from("  Enter              Focus selected node"),
            Line::from("  Esc                Unfocus (return to log) / quit"),
            Line::from("  \u{2190} / \u{2192}               Cycle detail tabs (when focused)"),
            Line::from("  c                  Toggle collapse on selected subtree"),
            Line::from("  r                  Jump to next pending review"),
            Line::from("  a / d / m / R      Approve / deny / modify / retry (review tab)"),
            Line::from(""),
            header(" Dashboard — General"),
            Line::from("  j/k, Up/Down       Scroll activity log"),
            Line::from("  g / G              Jump to top / bottom"),
            Line::from("  Tab                Cycle panel focus"),
            Line::from("  v                  Toggle verbose mode"),
            Line::from("  s                  Open session picker"),
            Line::from("  q, Esc             Quit"),
            Line::from(""),
            header(" Session Picker"),
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
            Line::from("  Esc                Clear filter \u{2192} back"),
            Line::from(""),
            header(" Session Detail"),
            Line::from("  (type)             Input goes to chat bar"),
            Line::from("  Enter              Send message"),
            Line::from("  ! + Enter          Interrupt & send"),
            Line::from("  Esc                Back to Dashboard"),
            Line::from("  y / n / a          Permission: yes/no/always"),
            Line::from("  Alt-m              Toggle plan mode"),
        ];

        if mermaid_enabled {
            out.push(Line::from("  Alt-v              Open mermaid diagram viewer"));
        }

        out.push(Line::from(""));

        if mermaid_enabled {
            out.push(header(" Mermaid Viewer (overlay)"));
            out.push(Line::from("  [ / ]              Cycle diagrams"));
            out.push(Line::from("  q / Esc            Close overlay"));
            out.push(Line::from(""));
        }

        out.push(Line::from(Span::styled(
            " Press ? or Esc to close",
            Style::default().fg(Color::DarkGray),
        )));

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn help_lines(mermaid_enabled: bool) -> Vec<String> {
        HelpOverlay::lines(mermaid_enabled)
            .into_iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect()
    }

    #[test]
    fn image_mode_mentions_alt_v_and_mermaid_viewer() {
        let joined = help_lines(true).join("\n");
        assert!(joined.contains("Alt-v"), "expected Alt-v in image-mode help: {joined}");
        assert!(
            joined.contains("Mermaid Viewer"),
            "expected Mermaid Viewer section: {joined}"
        );
    }

    #[test]
    fn text_mode_omits_alt_v_and_mermaid_viewer() {
        let joined = help_lines(false).join("\n");
        assert!(!joined.contains("Alt-v"), "Alt-v must be hidden in text mode: {joined}");
        assert!(
            !joined.contains("Mermaid Viewer"),
            "Mermaid Viewer section must be hidden: {joined}"
        );
    }
}
