pub mod activity_log;
pub mod agents_tree;
pub mod completion_popup;
pub mod completion_trigger;
pub mod detail_pane;
pub mod diff_viewer;
pub mod help_overlay;
pub mod inline_executor_card;
pub mod input_bar;
pub mod line_wrap;
#[cfg(feature = "markdown")]
pub mod markdown_stream;
#[cfg(feature = "markdown")]
pub mod mermaid;
pub mod quit_confirm;
pub mod react_trace;
pub mod resume_banner;
pub mod review_card;
pub mod session_preview;
pub mod status_bar;
pub(crate) mod trace_format;
pub mod workers_panel;

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
