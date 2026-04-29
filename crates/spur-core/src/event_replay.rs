//! NDJSON replay on TUI startup — rehydrates derived projections from
//! the EventSink's durable log before the live broadcast drain loop
//! begins. See `docs/superpowers/specs/2026-04-29-ndjson-replay-startup-rehydration-design.md`.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use spur_acp::SpurEvent;

const FIRST_N_MALFORMED_VERBOSE: u64 = 8;

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

/// Stream every event in the NDJSON ring through `on_event`, in
/// chronological file order, applying horizon and PID filters. Returns
/// a `ReplayStats` populated with counters and elapsed time.
///
/// Malformed JSON lines and lines exceeding `max_line_bytes` are
/// counted and skipped (first N at warn-level; rest aggregated).
/// `apply()` panics inside the closure are NOT caught — they propagate.
pub fn replay_events<F>(config: &ReplayConfig, mut on_event: F) -> std::io::Result<ReplayStats>
where
    F: FnMut(&SpurEvent),
{
    let start = Instant::now();
    let mut stats = ReplayStats::default();
    let cutoff = std::time::SystemTime::now().checked_sub(config.replay_horizon);

    let ordered = collect_ordered_files(&config.events_dir, config.skip_pid, &mut stats)?;

    for path in ordered {
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        stats.files_read += 1;
        let mut reader = BufReader::with_capacity(64 * 1024, file);
        let mut buf: Vec<u8> = Vec::with_capacity(4096);

        loop {
            buf.clear();
            let limit = config.max_line_bytes as u64;
            let n = (&mut reader).take(limit).read_until(b'\n', &mut buf)?;
            if n == 0 {
                break;
            }

            let terminated = buf.last() == Some(&b'\n');
            let hit_cap = !terminated && (n as u64) == limit;
            if hit_cap {
                stats.malformed_lines += 1;
                drain_until_newline(&mut reader)?;
                continue;
            }
            let line = if terminated {
                &buf[..buf.len() - 1]
            } else {
                &buf[..]
            };

            let event: SpurEvent = match serde_json::from_slice(line) {
                Ok(ev) => ev,
                Err(e) => {
                    if stats.malformed_lines < FIRST_N_MALFORMED_VERBOSE {
                        tracing::warn!(error = %e, ?path, "malformed NDJSON line");
                    }
                    stats.malformed_lines += 1;
                    continue;
                }
            };

            if cutoff.is_some_and(|c| event.occurred_at < c) {
                stats.events_skipped_horizon += 1;
                continue;
            }

            on_event(&event);
            stats.events_applied += 1;
        }
    }

    stats.elapsed = start.elapsed();
    Ok(stats)
}

/// Consume the underlying reader's buffered bytes up to and including the
/// next `\n` (or EOF). Used to recover from over-cap lines without losing
/// the next valid line's bytes that may sit in the same `read()` chunk.
///
/// Requires `BufRead` because `Read::read` cannot put bytes back: if `\n`
/// appears mid-chunk, every byte after it would be silently consumed and
/// the next `read_until` call would resume from beyond the next event.
fn drain_until_newline<R: BufRead>(reader: &mut R) -> std::io::Result<()> {
    let mut discard: Vec<u8> = Vec::new();
    reader.read_until(b'\n', &mut discard).map(|_| ())
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

    fn write_ndjson(path: &std::path::Path, events: &[spur_acp::domain::events::SpurEvent]) {
        use std::io::Write;
        let mut f = std::fs::File::create(path).unwrap();
        for ev in events {
            writeln!(f, "{}", serde_json::to_string(ev).unwrap()).unwrap();
        }
    }

    #[test]
    fn replay_events_applies_in_order_skipping_current_pid() {
        use spur_acp::domain::events::{SpurEvent, SpurEventBody};
        use spur_acp::SessionId;
        use std::time::SystemTime;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // Prior-PID file with two events.
        let prior_path = dir.join("100-1000-0.ndjson");
        write_ndjson(
            &prior_path,
            &[
                SpurEvent {
                    occurred_at: SystemTime::UNIX_EPOCH,
                    seq: 0,
                    body: SpurEventBody::TurnComplete {
                        session: SessionId("s1".into()),
                    },
                },
                SpurEvent {
                    occurred_at: SystemTime::UNIX_EPOCH,
                    seq: 1,
                    body: SpurEventBody::TurnComplete {
                        session: SessionId("s2".into()),
                    },
                },
            ],
        );

        // Current-PID file (must be skipped).
        let mine_path = dir.join("999-9999-0.ndjson");
        write_ndjson(
            &mine_path,
            &[SpurEvent {
                occurred_at: SystemTime::UNIX_EPOCH,
                seq: 0,
                body: SpurEventBody::TurnComplete {
                    session: SessionId("never_applied".into()),
                },
            }],
        );

        let cfg = ReplayConfig {
            events_dir: dir.to_path_buf(),
            replay_horizon: Duration::from_secs(u64::MAX / 2), // effectively unbounded
            skip_pid: Some(999),
            max_line_bytes: 1024,
        };

        let mut applied: Vec<String> = Vec::new();
        let stats = replay_events(&cfg, |ev| {
            if let SpurEventBody::TurnComplete { session } = &ev.body {
                applied.push(session.0.clone());
            }
        })
        .unwrap();

        assert_eq!(applied, vec!["s1", "s2"]);
        assert_eq!(stats.files_read, 1);
        assert_eq!(stats.files_skipped_pid, 1);
        assert_eq!(stats.events_applied, 2);
        assert_eq!(stats.malformed_lines, 0);
    }

    #[test]
    fn replay_events_over_cap_line_does_not_eat_subsequent_events() {
        use spur_acp::domain::events::{SpurEvent, SpurEventBody};
        use spur_acp::SessionId;
        use std::io::Write;
        use std::time::SystemTime;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let path = dir.join("100-1000-0.ndjson");

        // Build a fixture: valid line, then a long over-cap line, then valid line.
        // The over-cap line is engineered so the closing `\n` falls EARLY in
        // a 64 KB read chunk — i.e. there is significant valid content after
        // the `\n` that the buggy drain_until_newline would silently discard.
        let valid_a = SpurEvent {
            occurred_at: SystemTime::UNIX_EPOCH,
            seq: 0,
            body: SpurEventBody::TurnComplete {
                session: SessionId("first".into()),
            },
        };
        let valid_b = SpurEvent {
            occurred_at: SystemTime::UNIX_EPOCH,
            seq: 1,
            body: SpurEventBody::TurnComplete {
                session: SessionId("after_drain".into()),
            },
        };
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "{}", serde_json::to_string(&valid_a).unwrap()).unwrap();
            // Over-cap line: 4096 'x's + \n. With max_line_bytes=512 this
            // triggers hit_cap, then drain_until_newline must skip ~3585
            // bytes within likely a single 64 KB BufReader fill. The
            // closing \n is followed by valid_b's JSON, all within one
            // BufReader fill on small files.
            let huge: Vec<u8> = std::iter::repeat(b'x').take(4096).collect();
            f.write_all(&huge).unwrap();
            writeln!(f).unwrap();
            writeln!(f, "{}", serde_json::to_string(&valid_b).unwrap()).unwrap();
        }

        let cfg = ReplayConfig {
            events_dir: dir.to_path_buf(),
            replay_horizon: Duration::from_secs(u64::MAX / 2),
            skip_pid: None,
            max_line_bytes: 512,
        };

        let mut applied: Vec<String> = Vec::new();
        let stats = replay_events(&cfg, |ev| {
            if let SpurEventBody::TurnComplete { session } = &ev.body {
                applied.push(session.0.clone());
            }
        })
        .unwrap();

        // The bug surface: with the lossy drain, "after_drain" is missing
        // because drain_until_newline reads a 64 KB chunk, finds the \n
        // partway in, and returns — silently consuming the JSON line
        // that followed in the same chunk. After the fix, both valid
        // events MUST be applied.
        assert_eq!(
            applied,
            vec!["first", "after_drain"],
            "events after an over-cap line must NOT be lost"
        );
        assert_eq!(stats.events_applied, 2);
        assert_eq!(stats.malformed_lines, 1);
    }
}
