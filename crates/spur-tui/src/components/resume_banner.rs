use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::action::Action;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerState {
    /// Banner is fully visible, consuming keys.
    Visible,
    /// Banner is fading out (300ms transition).
    Fading,
    /// Banner is hidden but may re-nudge on idle.
    Hidden,
    /// User has sent 3+ messages; banner never returns.
    PermanentlyDismissed,
}

pub struct ResumeBanner {
    title: String,
    quit_ago: String,
    state: BannerState,
    state_changed_at: Instant,
    messages_sent: u32,
}

impl ResumeBanner {
    const FADE_DURATION_MS: u64 = 300;
    const RENUDGE_IDLE_S: u64 = 5;

    pub fn new(title: String, quit_ago: String) -> Self {
        Self {
            title,
            quit_ago,
            state: BannerState::Visible,
            state_changed_at: Instant::now(),
            messages_sent: 0,
        }
    }

    pub fn state(&self) -> BannerState {
        self.state
    }

    pub fn record_message_sent(&mut self) {
        self.messages_sent += 1;
        if self.messages_sent >= 3 {
            self.state = BannerState::PermanentlyDismissed;
        }
    }

    pub fn should_render(&self) -> bool {
        match self.state {
            BannerState::Visible => true,
            BannerState::Fading => {
                self.state_changed_at.elapsed().as_millis() < Self::FADE_DURATION_MS as u128
            }
            BannerState::Hidden | BannerState::PermanentlyDismissed => false,
        }
    }

    pub fn is_consuming_keys(&self) -> bool {
        self.state == BannerState::Visible
    }

    /// Process a keystroke. Returns Some(Action) if the key maps to a banner
    /// action (n = new, s = sessions), or None if the key just dismisses.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if !self.is_consuming_keys() {
            return None;
        }
        match key.code {
            KeyCode::Esc => {
                self.state = BannerState::Fading;
                self.state_changed_at = Instant::now();
                None
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                self.state = BannerState::PermanentlyDismissed;
                Some(Action::NewSessionRequested)
            }
            KeyCode::Char('s') | KeyCode::Char('S') => {
                self.state = BannerState::PermanentlyDismissed;
                Some(Action::RequestSessions)
            }
            _ => {
                // Any other key fades the banner but does not consume the action
                self.state = BannerState::Fading;
                self.state_changed_at = Instant::now();
                None
            }
        }
    }

    pub fn tick(&mut self) {
        // Advance Fading -> Hidden when fade completes
        if self.state == BannerState::Fading
            && self.state_changed_at.elapsed().as_millis() >= Self::FADE_DURATION_MS as u128
        {
            self.state = BannerState::Hidden;
            self.state_changed_at = Instant::now();
        }
    }

    /// Call when the view has been idle (no keystrokes) for a while.
    /// Returns true if the banner should re-nudge (transition Hidden -> Visible).
    pub fn maybe_renudge(&mut self) -> bool {
        if self.state != BannerState::Hidden {
            return false;
        }
        if self.state_changed_at.elapsed().as_secs() >= Self::RENUDGE_IDLE_S {
            self.state = BannerState::Visible;
            self.state_changed_at = Instant::now();
            return true;
        }
        false
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        if !self.should_render() {
            return;
        }
        let alpha = match self.state {
            BannerState::Fading => {
                let elapsed = self.state_changed_at.elapsed().as_millis() as f64;
                let max = Self::FADE_DURATION_MS as f64;
                1.0 - (elapsed / max).min(1.0)
            }
            _ => 1.0,
        };
        // ratatui doesn't support true alpha; simulate with color intensity
        let fg = if alpha < 0.5 {
            Color::DarkGray
        } else {
            Color::White
        };

        let line = Line::from(vec![
            Span::styled(" Resumed: ", Style::default().fg(Color::Green)),
            Span::styled(self.title.clone(), Style::default().fg(fg)),
            Span::styled(
                format!(" - quit {} ", self.quit_ago),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                "- [Esc] stay - [n] new - [s] sessions",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }
}
