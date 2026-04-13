use std::time::Instant;

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Shown at the top of SessionDetailView when a session was auto-resumed on
/// startup. Non-blocking — fades after 3s or on first keystroke. Keys are
/// not consumed, just dismiss the banner.
pub struct ResumeBanner {
    title: String,
    quit_ago: String,
    shown_at: Instant,
    dismissed: bool,
}

impl ResumeBanner {
    pub fn new(title: String, quit_ago: String) -> Self {
        Self {
            title,
            quit_ago,
            shown_at: Instant::now(),
            dismissed: false,
        }
    }

    pub fn should_render(&self) -> bool {
        !self.dismissed && self.shown_at.elapsed() < std::time::Duration::from_secs(3)
    }

    pub fn dismiss(&mut self) {
        self.dismissed = true;
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        if !self.should_render() {
            return;
        }
        let line = Line::from(vec![
            Span::styled(" Resumed: ", Style::default().fg(Color::Green)),
            Span::styled(self.title.clone(), Style::default().fg(Color::White)),
            Span::styled(
                format!(" \u{00b7} quit {} ", self.quit_ago),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                "\u{00b7} [s] picker \u{00b7} [n] new \u{00b7} [Esc] dismiss",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }
}
