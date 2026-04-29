//! NDJSON replay on TUI startup — rehydrates derived projections from
//! the EventSink's durable log before the live broadcast drain loop
//! begins. See `docs/superpowers/specs/2026-04-29-ndjson-replay-startup-rehydration-design.md`.

use std::fs;
use std::path::{Path, PathBuf};
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SegmentName {
    pid: u32,
    unix_ms: u128,
    rotation_seq: u64,
}

/// Parse `{pid}-{unix_ms}-{rotation_seq}.ndjson`. Returns `None` on
/// any deviation from the format. Mirrors `event_sink.rs:221-228`.
fn parse_segment_name(name: &str) -> Option<SegmentName> {
    let stem = name.strip_suffix(".ndjson")?;
    let mut parts = stem.split('-');
    let pid: u32 = parts.next()?.parse().ok()?;
    let unix_ms: u128 = parts.next()?.parse().ok()?;
    let rotation_seq: u64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(SegmentName {
        pid,
        unix_ms,
        rotation_seq,
    })
}

/// Walk `events_dir`, parse `.ndjson` segment filenames, drop entries
/// matching `skip_pid`, sort by `(unix_ms, rotation_seq)` ascending.
///
/// Returns an empty Vec if the dir does not exist (NotFound is tolerated
/// for fresh-repo first runs). All other I/O errors propagate.
#[allow(dead_code)]
fn collect_ordered_files(
    events_dir: &Path,
    skip_pid: Option<u32>,
    stats: &mut ReplayStats,
) -> std::io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(events_dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut parsed: Vec<(SegmentName, PathBuf)> = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        let segment = match parse_segment_name(name) {
            Some(s) => s,
            None => continue,
        };
        if Some(segment.pid) == skip_pid {
            stats.files_skipped_pid += 1;
            continue;
        }
        parsed.push((segment, path));
    }

    parsed.sort_by_key(|(s, _)| (s.unix_ms, s.rotation_seq, s.pid));
    Ok(parsed.into_iter().map(|(_, p)| p).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_config_default_uses_current_pid() {
        let cfg = ReplayConfig::default();
        assert_eq!(cfg.skip_pid, Some(std::process::id()));
        assert_eq!(cfg.events_dir, std::path::PathBuf::from(".spur/events"));
        assert_eq!(
            cfg.replay_horizon,
            std::time::Duration::from_secs(7 * 86400)
        );
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

    #[test]
    fn parse_segment_name_well_formed() {
        let parsed = parse_segment_name("12345-1714000000123-7.ndjson");
        assert_eq!(
            parsed,
            Some(SegmentName {
                pid: 12345,
                unix_ms: 1714000000123,
                rotation_seq: 7
            })
        );
    }

    #[test]
    fn parse_segment_name_rejects_garbage() {
        assert_eq!(parse_segment_name("not-a-segment"), None);
        assert_eq!(parse_segment_name("12345-foo-7.ndjson"), None);
        assert_eq!(parse_segment_name("12345-100.ndjson"), None);
        assert_eq!(parse_segment_name(""), None);
        assert_eq!(parse_segment_name("12345-100-7.json"), None);
        assert_eq!(parse_segment_name("12345-100-0-1.ndjson"), None);
    }

    #[test]
    fn collect_ordered_files_skips_pid_and_sorts_by_ts_then_seq() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // Three files: A and B from another PID (chronological), C from current PID (skipped).
        let a = dir.join("100-1000-0.ndjson");
        let b = dir.join("100-1000-1.ndjson"); // same ts as A, later seq
        let earlier = dir.join("200-500-0.ndjson"); // older ts, different PID
        let mine = dir.join("999-9999-0.ndjson"); // current PID - must be skipped
        let unrelated = dir.join("readme.txt"); // not .ndjson - ignored
        let garbage = dir.join("garbage-name.ndjson"); // unparseable - ignored

        for p in [&a, &b, &earlier, &mine, &unrelated, &garbage] {
            std::fs::File::create(p).unwrap();
        }

        let mut stats = ReplayStats::default();
        let ordered = collect_ordered_files(dir, Some(999), &mut stats).unwrap();

        let names: Vec<_> = ordered
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["200-500-0.ndjson", "100-1000-0.ndjson", "100-1000-1.ndjson"],
        );
        assert_eq!(stats.files_skipped_pid, 1);
    }

    #[test]
    fn collect_ordered_files_returns_empty_for_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("does-not-exist");
        let mut stats = ReplayStats::default();
        let ordered = collect_ordered_files(&dir, None, &mut stats).unwrap();
        assert!(ordered.is_empty());
        assert_eq!(stats.files_skipped_pid, 0);
    }

    #[test]
    fn collect_ordered_files_breaks_pid_tie_deterministically() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // Two processes (pid 100, 200) both with rotation_seq=0 at the same unix_ms.
        // Without pid in the sort key, this would tie and depend on read_dir order.
        let a = dir.join("100-5000-0.ndjson");
        let b = dir.join("200-5000-0.ndjson");
        std::fs::File::create(&a).unwrap();
        std::fs::File::create(&b).unwrap();

        let mut stats = ReplayStats::default();
        let ordered = collect_ordered_files(dir, None, &mut stats).unwrap();
        let names: Vec<_> = ordered
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["100-5000-0.ndjson", "200-5000-0.ndjson"],
            "pid tiebreak should produce ascending pid order"
        );
    }

    #[test]
    fn collect_ordered_files_skips_directories_with_matching_names() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // A directory whose name parses as a segment — must NOT be returned.
        let trap_dir = dir.join("100-1000-0.ndjson");
        std::fs::create_dir(&trap_dir).unwrap();
        // A real file alongside it.
        let real = dir.join("200-2000-0.ndjson");
        std::fs::File::create(&real).unwrap();

        let mut stats = ReplayStats::default();
        let ordered = collect_ordered_files(dir, None, &mut stats).unwrap();
        let names: Vec<_> = ordered
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["200-2000-0.ndjson"],
            "directory with matching name must be filtered out"
        );
    }
}
