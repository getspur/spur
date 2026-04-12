pub mod activity_log;
pub mod agents_tree;
pub mod help_overlay;
pub mod input_bar;
pub mod react_trace;
pub mod status_bar;

use std::time::Instant;

/// Tracked state for a single agent.
#[derive(Debug, Clone)]
pub struct AgentState {
    pub name: String,
    pub role: String,
    pub status: String,
    pub parent: Option<String>,
    pub started_at: Option<Instant>,
    pub cost: f64,
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
