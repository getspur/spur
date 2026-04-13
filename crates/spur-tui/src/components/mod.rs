pub mod activity_log;
pub mod detail_pane;
pub mod agents_tree;
pub mod help_overlay;
pub mod input_bar;
pub mod line_wrap;
pub mod react_trace;
pub mod review_card;
pub mod status_bar;

use ratatui::style::{Color, Style};
use std::time::Instant;

// ─── Shared constants ───────────────────────────────────────────────

/// Braille spinner frames for animating active agents.
pub const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

// ─── Shared utility functions ───────────────────────────────────────

/// Current local time formatted as HH:MM:SS.
pub fn now_stamp() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// Format an elapsed duration from a start time as `Xm YYs`.
pub fn format_elapsed(started_at: Instant) -> String {
    let secs = started_at.elapsed().as_secs();
    format!("{}m {:02}s", secs / 60, secs % 60)
}

/// Border style for focused (cyan) vs unfocused (dark gray) panels.
pub fn focused_border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// A single entry in the activity log.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: String,
    pub prefix: String,
    pub message: String,
    pub kind: LogEntryKind,
}

/// What kind of log entry this is (for styling and filtering).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEntryKind {
    Think,
    Act,
    Observe,
    Delegate,
    Complete,
    Error,
    UserMessage,
    Permission,
    Info,
}

/// Maximum log entries before oldest are evicted.
pub const MAX_LOG_ENTRIES: usize = 5_000;
