//! NDJSON replay on TUI startup — rehydrates derived projections from
//! the EventSink's durable log before the live broadcast drain loop
//! begins. See `docs/superpowers/specs/2026-04-29-ndjson-replay-startup-rehydration-design.md`.

use std::path::PathBuf;
use std::time::Duration;

/// Caller-supplied replay configuration.
///
/// `Default::default()` calls `std::process::id()` so `skip_pid` is the
/// current process. This is intentional — the only sensible default is
/// "skip my own NDJSON file because the live broadcast carries those
/// events instead".
#[derive(Debug, Clone)]
pub struct ReplayConfig {
    /// Directory containing `.ndjson` segments (typically `.spur/events`).
    pub events_dir: PathBuf,
    /// Events older than `now() - replay_horizon` are skipped at parse time.
    pub replay_horizon: Duration,
    /// Files whose filename PID matches this value are skipped wholesale.
    /// Set to `Some(std::process::id())` for the typical TUI-startup case.
    pub skip_pid: Option<u32>,
    /// Maximum bytes per line. Lines exceeding this are counted as
    /// malformed and skipped without allocating beyond the cap.
    pub max_line_bytes: usize,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            events_dir: PathBuf::from(".spur/events"),
            replay_horizon: Duration::from_secs(7 * 86400),
            skip_pid: Some(std::process::id()),
            max_line_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Telemetry returned by `replay_events`.
#[derive(Debug, Default, Clone)]
pub struct ReplayStats {
    pub files_read: usize,
    pub files_skipped_pid: usize,
    pub events_applied: u64,
    pub events_skipped_horizon: u64,
    pub malformed_lines: u64,
    pub elapsed: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_config_default_uses_current_pid() {
        let cfg = ReplayConfig::default();
        assert_eq!(cfg.skip_pid, Some(std::process::id()));
        assert_eq!(cfg.events_dir, std::path::PathBuf::from(".spur/events"));
        assert_eq!(cfg.replay_horizon, std::time::Duration::from_secs(7 * 86400));
        assert_eq!(cfg.max_line_bytes, 8 * 1024 * 1024);
    }

    #[test]
    fn replay_stats_default_is_zero() {
        let s = ReplayStats::default();
        assert_eq!(s.files_read, 0);
        assert_eq!(s.files_skipped_pid, 0);
        assert_eq!(s.events_applied, 0);
        assert_eq!(s.events_skipped_horizon, 0);
        assert_eq!(s.malformed_lines, 0);
        assert_eq!(s.elapsed, std::time::Duration::ZERO);
    }
}
