//! JSONL durable event sink (Phase S3).
//!
//! Subscribes to the orchestrator's broadcast channel and appends every
//! `SpurEvent` as one line of JSON to `~/.spur/events/{pid}-{ts}.ndjson`.
//! Size-based rotation; log-and-drop on write error (never crashes the
//! orchestrator on disk-full).

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use tokio::sync::broadcast;

use spur_acp::domain::events::SpurEvent;

/// Maximum file size before rotation. Override with
/// `SPUR_EVENT_LOG_MAX_BYTES`. Sized to play well with the
/// `LogConfig::events_max_total_bytes` cap (default 64 MB): 8 MB per file
/// × ~7 rotated chunks + 8 MB active ≈ 64 MB total on disk.
pub const DEFAULT_MAX_BYTES: u64 = 8 * 1024 * 1024; // 8 MB
const FLUSH_BYTES: usize = 64 * 1024; // 64 KB buffer threshold
const FLUSH_INTERVAL_MS: u64 = 100;

/// Per-process rotation counter. Guarantees unique filenames even when
/// two rotations happen within the same millisecond.
static ROTATION_SEQ: AtomicU64 = AtomicU64::new(0);

/// Spawn the sink task. Returns immediately; the task runs until the
/// broadcast channel closes (orchestrator shutdown).
///
/// `max_bytes` controls per-file rotation. `max_total_bytes` caps the
/// cumulative size of all `.ndjson` files in the events dir; oldest
/// files are deleted on every rotation to honour the cap.
pub fn spawn_sink(mut rx: broadcast::Receiver<SpurEvent>, max_bytes: u64, max_total_bytes: u64) {
    let events_dir = events_dir();
    if let Err(e) = fs::create_dir_all(&events_dir) {
        tracing::error!(error = %e, dir = %events_dir.display(),
            "event_sink: failed to create events dir; sink disabled");
        return;
    }

    tokio::spawn(async move {
        let mut state = match SinkState::open_with_caps(&events_dir, max_bytes, max_total_bytes) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e,
                    "event_sink: failed to open first log file; sink disabled");
                return;
            }
        };

        let mut flush_timer =
            tokio::time::interval(std::time::Duration::from_millis(FLUSH_INTERVAL_MS));

        loop {
            tokio::select! {
                res = rx.recv() => {
                    match res {
                        Ok(event) => {
                            if let Err(e) = state.write_event(&event) {
                                tracing::warn!(error = %e,
                                    "event_sink: write failed; dropping event");
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(lagged_n = n,
                                "event_sink: broadcast lagged");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = flush_timer.tick() => {
                    let _ = state.flush();
                }
            }
        }

        let _ = state.flush();
    });
}

struct SinkState {
    dir: PathBuf,
    writer: BufWriter<File>,
    current_path: PathBuf,
    bytes_in_file: u64,
    max_bytes: u64,
    /// Cap on total bytes across all `.ndjson` files in `dir`. When set,
    /// `enforce_event_cap` runs after every rotation, deleting oldest
    /// files until the cumulative size is within the cap (with
    /// `max_bytes` reserved as headroom for the active file to grow
    /// before the next rotation).
    max_total_bytes: Option<u64>,
}

impl SinkState {
    fn open(dir: &Path, max_bytes: u64) -> std::io::Result<Self> {
        let path = rotated_path(dir);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            dir: dir.to_path_buf(),
            writer: BufWriter::with_capacity(FLUSH_BYTES, file),
            current_path: path,
            bytes_in_file: bytes,
            max_bytes,
            max_total_bytes: None,
        })
    }

    /// Open a sink that, in addition to per-file rotation at `max_per_file`,
    /// enforces a total-directory byte cap of `max_total` after every
    /// rotation by deleting oldest `.ndjson` files.
    fn open_with_caps(dir: &Path, max_per_file: u64, max_total: u64) -> std::io::Result<Self> {
        let mut state = Self::open(dir, max_per_file)?;
        state.max_total_bytes = Some(max_total);
        let effective = max_total.saturating_sub(state.max_bytes);
        if let Err(e) = enforce_event_cap(&state.dir, effective, &state.current_path) {
            tracing::warn!(error = %e,
                "event_sink: enforce_event_cap failed");
        }
        Ok(state)
    }

    fn write_event(&mut self, event: &SpurEvent) -> std::io::Result<()> {
        let line = serde_json::to_string(event)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.bytes_in_file += line.len() as u64 + 1;
        if self.bytes_in_file >= self.max_bytes {
            self.rotate()?;
        }
        Ok(())
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.writer.flush()?;
        let new_path = rotated_path(&self.dir);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&new_path)?;
        self.writer = BufWriter::with_capacity(FLUSH_BYTES, file);
        self.current_path = new_path;
        self.bytes_in_file = 0;
        if let Some(cap) = self.max_total_bytes {
            // Reserve `max_bytes` of headroom so the freshly opened file
            // can grow up to its rotation threshold without pushing the
            // directory total past the user-visible cap.
            let effective = cap.saturating_sub(self.max_bytes);
            if let Err(e) = enforce_event_cap(&self.dir, effective, &self.current_path) {
                tracing::warn!(error = %e,
                    "event_sink: enforce_event_cap failed");
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

/// Garbage-collect oldest `.ndjson` files in `dir` until the cumulative
/// size of the remaining files is ≤ `cap_bytes`. The active file at
/// `protected` is never deleted (it's just been opened by the caller and
/// will be written to immediately). Returns the number of files deleted.
fn enforce_event_cap(dir: &Path, cap_bytes: u64, protected: &Path) -> std::io::Result<usize> {
    // Short-circuit the disable sentinel — no point in scanning the dir.
    if cap_bytes == u64::MAX {
        return Ok(0);
    }
    let mut entries: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("ndjson") {
            continue;
        }
        if path == protected {
            continue;
        }
        let md = match entry.metadata() {
            Ok(md) => md,
            // Tolerate concurrent deletion; just skip the entry.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        let mtime = match md.modified() {
            Ok(t) => t,
            Err(_) => continue,
        };
        entries.push((path, mtime, md.len()));
    }
    // Sort newest-first so we keep newest until we cross the cap.
    entries.sort_by(|a, b| b.1.cmp(&a.1));

    let mut running = 0u64;
    let mut deleted = 0usize;
    for (path, _mtime, size) in entries {
        running += size;
        if running > cap_bytes {
            match fs::remove_file(&path) {
                Ok(()) => deleted += 1,
                // Already gone; treat as a successful delete.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(deleted)
}

pub(crate) fn events_dir() -> PathBuf {
    PathBuf::from(".spur/events")
}

fn rotated_path(dir: &Path) -> PathBuf {
    let pid = std::process::id();
    let ts = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let n = ROTATION_SEQ.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{pid}-{ts}-{n}.ndjson"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::domain::events::{SpurEvent, SpurEventBody};
    use spur_acp::types::SessionId;

    /// Helper: open a `SinkState` with an explicit `max_bytes`, bypassing
    /// `SPUR_EVENT_LOG_MAX_BYTES` so tests don't race on the process env.
    fn open_with_max(dir: &std::path::Path, max_bytes: u64) -> SinkState {
        SinkState::open(dir, max_bytes).unwrap()
    }

    #[tokio::test]
    async fn writes_events_to_file() {
        let tmpdir = tempfile::tempdir().unwrap();
        let dir = tmpdir.path().join("events");
        fs::create_dir_all(&dir).unwrap();

        // Use a huge max_bytes so rotation never fires during this test.
        let mut state = open_with_max(&dir, u64::MAX);

        let event = SpurEvent {
            occurred_at: SystemTime::UNIX_EPOCH,
            seq: 42,
            body: SpurEventBody::TurnComplete {
                session: SessionId("s1".to_string()),
            },
        };
        state.write_event(&event).unwrap();
        state.flush().unwrap();

        let contents = fs::read_to_string(&state.current_path).unwrap();
        let line = contents.lines().next().unwrap();
        let back: SpurEvent = serde_json::from_str(line).unwrap();
        assert_eq!(back.seq, 42);
    }

    #[tokio::test]
    async fn rotates_on_size_threshold() {
        let tmpdir = tempfile::tempdir().unwrap();
        let dir = tmpdir.path().join("events");
        fs::create_dir_all(&dir).unwrap();

        // Force a tiny max_bytes so one event triggers rotation.
        let mut state = open_with_max(&dir, 10);

        let event = SpurEvent {
            occurred_at: SystemTime::UNIX_EPOCH,
            seq: 1,
            body: SpurEventBody::TurnComplete {
                session: SessionId("s1".to_string()),
            },
        };
        state.write_event(&event).unwrap();
        // No sleep needed — `rotated_path()`'s ROTATION_SEQ counter
        // guarantees unique filenames per-process even within one ms.
        state.write_event(&event).unwrap();
        state.flush().unwrap();

        let files: Vec<_> = fs::read_dir(&dir).unwrap().collect();
        assert!(
            files.len() >= 2,
            "expected rotation; got {} file(s)",
            files.len()
        );
    }

    /// Helper: produce a fake event whose JSONL representation is ≈ 2 KB.
    fn fake_event_2_kb() -> SpurEvent {
        SpurEvent {
            occurred_at: SystemTime::UNIX_EPOCH,
            seq: 0,
            body: SpurEventBody::BrainError {
                session: SessionId("test".to_string()),
                message: "x".repeat(1900),
            },
        }
    }

    #[tokio::test]
    async fn enforces_max_total_bytes_after_rotation() {
        let tmpdir = tempfile::tempdir().unwrap();
        let dir = tmpdir.path().join("events");
        fs::create_dir_all(&dir).unwrap();

        // Use a small max_total_bytes so we trigger GC quickly.
        let max_total: u64 = 256 * 1024; // 256 KB total cap
        let max_per_file: u64 = 64 * 1024; // 64 KB per file → ~4 files at cap

        let mut state = SinkState::open_with_caps(&dir, max_per_file, max_total).expect("open");

        // Write enough events to trigger ~6 rotations.
        for _ in 0..200 {
            state.write_event(&fake_event_2_kb()).expect("write");
        }
        state.flush().unwrap();

        let mut total = 0u64;
        let mut count = 0;
        for entry in fs::read_dir(&dir).expect("read_dir") {
            let entry = entry.expect("entry");
            if entry.file_name().to_string_lossy().ends_with(".ndjson") {
                total += entry.metadata().expect("md").len();
                count += 1;
            }
        }
        assert!(
            total <= max_total,
            "total bytes {} exceeds cap {}",
            total,
            max_total
        );
        assert!(count >= 1, "expected at least 1 file, got {}", count);
    }

    #[tokio::test]
    async fn open_with_caps_enforces_max_total_bytes_before_writes() {
        let tmpdir = tempfile::tempdir().unwrap();
        let dir = tmpdir.path().join("events");
        fs::create_dir_all(&dir).unwrap();

        for n in 0..4 {
            let path = dir.join(format!("old-{n}.ndjson"));
            fs::write(path, vec![b'x'; 100 * 1024]).unwrap();
        }

        let max_total: u64 = 128 * 1024;
        let max_per_file: u64 = 64 * 1024;
        let _state = SinkState::open_with_caps(&dir, max_per_file, max_total).expect("open");

        let total = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().and_then(|s| s.to_str()) == Some("ndjson"))
            .map(|entry| entry.metadata().unwrap().len())
            .sum::<u64>();

        assert!(
            total <= max_total,
            "total bytes {} exceeds cap {} before any writes",
            total,
            max_total
        );
    }
}
