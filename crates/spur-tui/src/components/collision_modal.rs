use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use spur_acp::session_lock::HolderInfo;

/// Modal shown when the user attempts to attach to a session that is
/// currently held by another `spur tui` process.
pub struct CollisionModal;

impl CollisionModal {
    pub fn render(frame: &mut Frame, area: Rect, session_label: &str, holder: &HolderInfo) {
        let width = 70u16.min(area.width.saturating_sub(4));
        let height = 14u16.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let popup_area = Rect::new(x, y, width, height);

        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .title(" Session attached in another window ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));

        let identity = if let Some(label) = holder.label.as_deref() {
            format!("  holder: {label}")
        } else if let Some(tty) = holder.tty.as_deref() {
            format!("  holder: {tty}")
        } else if let Some(pid) = holder.pid {
            let when = holder
                .started_at
                .map(|t| format!(" (started {})", t.format("%H:%M")))
                .unwrap_or_default();
            format!("  holder: PID {pid}{when}")
        } else {
            "  holder: another window (no metadata)".to_string()
        };

        let workdir_line = holder
            .workdir
            .as_ref()
            .map(|w| format!("  workdir: {}", w.display()))
            .unwrap_or_default();

        let kill_line = holder
            .pid
            .map(|pid| format!("    kill {pid}"))
            .unwrap_or_else(|| "    (no PID available - close the other window manually)".into());

        let lines = vec![
            Line::from(""),
            Line::from(format!("  {session_label}")),
            Line::from(Span::styled(identity, Style::default().fg(Color::DarkGray))),
            Line::from(Span::styled(
                workdir_line,
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "[N]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" new session   "),
                Span::styled(
                    "[P]",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" picker   "),
                Span::styled(
                    "[Esc]",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" cancel"),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "  To take over manually, run in your shell:",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(kill_line, Style::default().fg(Color::White))),
            Line::from(Span::styled(
                "  then press [Enter] to retry attach.",
                Style::default().fg(Color::DarkGray),
            )),
        ];

        frame.render_widget(Paragraph::new(lines).block(block), popup_area);
    }
}
