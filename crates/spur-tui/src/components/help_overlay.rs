use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

pub struct HelpOverlay;

impl HelpOverlay {
    pub fn render(frame: &mut Frame, area: Rect, mermaid_enabled: bool, issues_enabled: bool) {
        let width = 66u16.min(area.width.saturating_sub(4));
        let height = 50u16.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let popup_area = Rect::new(x, y, width, height);

        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Help ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        let paragraph = Paragraph::new(Self::lines(mermaid_enabled, issues_enabled)).block(block);
        frame.render_widget(paragraph, popup_area);
    }

    /// Build the help text. Exposed so tests can assert on contents without
    /// constructing a `Frame`. `mermaid_enabled` suppresses Alt-v and the
    /// Mermaid Viewer section when false. `issues_enabled` suppresses the
    /// Issues sections when false.
    pub fn lines(mermaid_enabled: bool, issues_enabled: bool) -> Vec<Line<'static>> {
        let header = |t: &'static str| {
            Line::from(Span::styled(
                t,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
        };

        let mut out: Vec<Line<'static>> = vec![
            header(" Dashboard — Modes"),
            Line::from("  [NAV]              Navigation mode — keys control panels"),
            Line::from("  [INSERT]           Compose mode — keys go to input bar"),
            Line::from("  Esc                Exit Compose → Navigate"),
            Line::from("  i / a / I / A / o  Enter Compose (Vim Normal)"),
            Line::from("  Enter              Focus input bar (Emacs)"),
            Line::from(""),
            header(" Dashboard — Navigation"),
            Line::from("  j/k, Up/Down       Scroll or navigate"),
            Line::from("  PgUp / PgDn        Page scroll"),
            Line::from("  Ctrl+d / Ctrl+u    Half-page scroll"),
            Line::from("  g / G              Jump to top / bottom"),
            Line::from("  Tab / Shift+Tab    Cycle panel focus (Agents ↔ Log)"),
            Line::from("  1                  Jump to Agents panel"),
            Line::from("  3                  Jump to Log panel"),
            Line::from("  z                  Zoom / restore panels"),
            Line::from("  v                  Toggle verbose mode"),
            Line::from("  s                  Open session picker"),
            Line::from("  q                  Quit"),
            Line::from("  Ctrl-C             Quit (press twice to force)"),
            Line::from(""),
            header(" Dashboard — Lineage Tree"),
            Line::from("  j / k              Move selection in lineage tree"),
            Line::from("  Enter              Focus selected agent"),
            Line::from("  c                  Toggle collapse on selected subtree"),
            Line::from("  N                  Jump to next pending review"),
            Line::from("  P                  Jump to previous pending review"),
            Line::from(""),
            header(" Dashboard — Detail Pane (when agent focused)"),
            Line::from("  h / l / \u{2190} / \u{2192}  Cycle detail tabs"),
            Line::from(
                "  Ctrl+1-5           Jump to Stream / Artifacts / Attempts / Task / Review",
            ),
            Line::from("  j / k              Scroll tab content"),
            Line::from("  Ctrl+d / Ctrl+u    Half-page scroll"),
            Line::from("  o                  Toggle observe collapsed (Stream tab)"),
            Line::from("  Esc                Unfocus agent (return to log)"),
            Line::from(""),
            header(" Dashboard — Review Tab"),
            Line::from("  A                  Approve"),
            Line::from("  D                  Deny"),
            Line::from("  M                  Modify + approve"),
            Line::from("  R                  Retry"),
            Line::from(""),
        ];

        if issues_enabled {
            out.push(header(" Issue Browser (press 2 from Dashboard)"));
            out.push(Line::from("  j / k, Up/Down   Navigate issue list"));
            out.push(Line::from("  g / G            First / last issue"));
            out.push(Line::from("  Enter            Open / close issue detail"));
            out.push(Line::from("  o                Set status: open"));
            out.push(Line::from("  w                Set status: in progress"));
            out.push(Line::from("  b                Set status: blocked"));
            out.push(Line::from("  x                Set status: closed"));
            out.push(Line::from("  d                Deprecated close alias"));
            out.push(Line::from("  W                Work on issue"));
            out.push(Line::from("  PgUp / PgDn      Scroll detail"));
            out.push(Line::from("  s                Open session picker"));
            out.push(Line::from(
                "  Esc              Close detail or back to Dashboard",
            ));
            out.push(Line::from(""));
        }

        out.extend(vec![
            header(" Session Picker"),
            Line::from("  j/k, Up/Down       Navigate list"),
            Line::from("  Enter              Resume / create (on [+ New])"),
            Line::from("  /                  Focus search field"),
            Line::from("  n                  New session"),
            Line::from("  R                  Rename selected"),
            Line::from("  x                  Archive (or unarchive)"),
            Line::from("  d                  Deprecated archive alias"),
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
            Line::from("  Alt-d              Toggle workers panel"),
            Line::from(""),
            header(" Editing Shortcuts"),
            Line::from("  Ctrl-P / Ctrl-N    Previous / next input history"),
            Line::from("  Ctrl-R / Alt-R     Fuzzy search input history"),
            Line::from("  Ctrl-U             Delete to start of line"),
            Line::from("  Ctrl-K             Delete to end of line"),
            Line::from("  Ctrl-W             Delete previous word"),
        ]);

        if mermaid_enabled {
            out.push(Line::from(
                "  Alt-v              Open mermaid diagram viewer",
            ));
        }

        out.push(Line::from(""));

        if mermaid_enabled {
            out.push(header(" Mermaid Viewer (overlay)"));
            out.push(Line::from("  [ / ]              Cycle diagrams"));
            out.push(Line::from("  q / Esc            Close overlay"));
            out.push(Line::from(""));
        }

        // Mouse capture is enabled so the scroll wheel works, which means
        // the terminal no longer owns drag events for native text selection.
        // Every major terminal supports a modifier-drag bypass — hold the
        // modifier while dragging to hand drag events back to the terminal
        // so you can select and copy text as usual.
        out.push(header(" Text Selection (copy/paste)"));
        out.push(Line::from(
            "  Option+drag        iTerm2 / WezTerm / Ghostty (macOS)",
        ));
        out.push(Line::from("  Fn+drag            macOS Terminal.app"));
        out.push(Line::from(
            "  Shift+drag         Kitty / Alacritty / GNOME / Konsole",
        ));
        out.push(Line::from("  Alt+drag           Windows Terminal"));
        out.push(Line::from(Span::styled(
            "  (mouse capture intercepts drag so the wheel can scroll;",
            Style::default().fg(Color::DarkGray),
        )));
        out.push(Line::from(Span::styled(
            "   the modifier tells your terminal to keep the drag locally)",
            Style::default().fg(Color::DarkGray),
        )));
        out.push(Line::from(""));

        out.push(header(" macOS: Alt Keybindings"));
        out.push(Line::from(Span::styled(
            "  Alt shortcuts work automatically for US-QWERTY layouts.",
            Style::default().fg(Color::DarkGray),
        )));
        out.push(Line::from(Span::styled(
            "  For other layouts, enable \"Use Option as Meta Key\" in your",
            Style::default().fg(Color::DarkGray),
        )));
        out.push(Line::from(Span::styled(
            "  terminal settings (iTerm2 / Terminal.app / Alacritty / Kitty).",
            Style::default().fg(Color::DarkGray),
        )));
        out.push(Line::from(""));

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

    fn help_lines(mermaid_enabled: bool, issues_enabled: bool) -> Vec<String> {
        HelpOverlay::lines(mermaid_enabled, issues_enabled)
            .into_iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn image_mode_mentions_alt_v_and_mermaid_viewer() {
        let joined = help_lines(true, false).join("\n");
        assert!(
            joined.contains("Alt-v"),
            "expected Alt-v in image-mode help: {joined}"
        );
        assert!(
            joined.contains("Mermaid Viewer"),
            "expected Mermaid Viewer section: {joined}"
        );
    }

    #[test]
    fn text_mode_omits_alt_v_and_mermaid_viewer() {
        let joined = help_lines(false, false).join("\n");
        assert!(
            !joined.contains("Alt-v"),
            "Alt-v must be hidden in text mode: {joined}"
        );
        assert!(
            !joined.contains("Mermaid Viewer"),
            "Mermaid Viewer section must be hidden: {joined}"
        );
    }

    #[test]
    fn help_advertises_terminal_modifier_bypass_for_text_selection() {
        let joined = help_lines(false, false).join("\n");
        assert!(
            joined.contains("Text Selection"),
            "help must advertise text-selection bypass: {joined}"
        );
        assert!(joined.contains("Option+drag"), "must mention Option+drag");
        assert!(joined.contains("Shift+drag"), "must mention Shift+drag");
    }

    #[test]
    fn help_advertises_ctrl_c_quit_flow() {
        let joined = help_lines(false, false).join("\n");
        assert!(
            joined.contains("Ctrl-C             Quit"),
            "help must advertise Ctrl-C quit: {joined}"
        );
        assert!(
            !joined.contains("q, Esc             Quit"),
            "legacy Esc quit hint should be removed: {joined}"
        );
    }

    #[test]
    fn issues_enabled_shows_issue_browser_section() {
        let joined = help_lines(false, true).join("\n");
        assert!(
            joined.contains("Issue Browser"),
            "expected Issue Browser section: {joined}"
        );
        assert!(
            !joined.contains("Issue Detail (overlay)"),
            "Issue Detail overlay section must be removed: {joined}"
        );
        assert!(
            !joined.contains("Dashboard — Issues"),
            "Dashboard — Issues section must be removed: {joined}"
        );
        assert!(
            joined.contains('W'),
            "expected W hotkey in issues help: {joined}"
        );
    }

    #[test]
    fn issues_disabled_omits_issue_browser_section() {
        let joined = help_lines(false, false).join("\n");
        assert!(
            !joined.contains("Issue Browser"),
            "Issue Browser must be hidden when issues_enabled=false: {joined}"
        );
    }

    #[test]
    fn help_has_detail_pane_and_review_tab_sections() {
        let joined = help_lines(false, false).join("\n");
        assert!(
            joined.contains("Detail Pane (when agent focused)"),
            "expected Detail Pane section: {joined}"
        );
        assert!(
            joined.contains("Review Tab"),
            "expected Review Tab section: {joined}"
        );
    }
}
