//! JSONL durable event sink (Phase S3).
//!
//! Subscribes to the orchestrator's broadcast channel and appends every
//! `SpurEvent` as one line of JSON to `~/.spur/events/{pid}-{ts}.ndjson`.
//! Size-based rotation; log-and-drop on write error (never crashes the
//! orchestrator on disk-full).

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use tokio::sync::broadcast;

use spur_acp::domain::events::SpurEvent;

/// Maximum file size before rotation. Override with
/// `SPUR_EVENT_LOG_MAX_BYTES`.
const DEFAULT_MAX_BYTES: u64 = 128 * 1024 * 1024; // 128 MB
const FLUSH_BYTES: usize = 64 * 1024;              // 64 KB buffer threshold
const FLUSH_INTERVAL_MS: u64 = 100;

/// Per-process rotation counter. Guarantees unique filenames even when
/// two rotations happen within the same millisecond.
static ROTATION_SEQ: AtomicU64 = AtomicU64::new(0);

/// Spawn the sink task. Returns immediately; the task runs until the
/// broadcast channel closes (orchestrator shutdown).
pub fn spawn_sink(mut rx: broadcast::Receiver<SpurEvent>) {
    let events_dir = events_dir();
    if let Err(e) = fs::create_dir_all(&events_dir) {
        tracing::error!(error = %e, dir = %events_dir.display(),
            "event_sink: failed to create events dir; sink disabled");
        return;
    }

    tokio::spawn(async move {
        let mut state = match SinkState::open(&events_dir) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e,
                    "event_sink: failed to open first log file; sink disabled");
                return;
            }
        };

        let mut flush_timer = tokio::time::interval(
            std::time::Duration::from_millis(FLUSH_INTERVAL_MS),
        );

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
}

impl SinkState {
    fn open(dir: &PathBuf) -> std::io::Result<Self> {
        let max_bytes = std::env::var("SPUR_EVENT_LOG_MAX_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MAX_BYTES);
        let path = rotated_path(dir);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            dir: dir.clone(),
            writer: BufWriter::with_capacity(FLUSH_BYTES, file),
            current_path: path,
            bytes_in_file: bytes,
            max_bytes,
        })
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
        let file = OpenOptions::new().create(true).append(true).open(&new_path)?;
        self.writer = BufWriter::with_capacity(FLUSH_BYTES, file);
        self.current_path = new_path;
        self.bytes_in_file = 0;
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

fn events_dir() -> PathBuf {
    PathBuf::from(".spur/events")
}

fn rotated_path(dir: &PathBuf) -> PathBuf {
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
    fn open_with_max(dir: &PathBuf, max_bytes: u64) -> SinkState {
        let path = rotated_path(dir);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
        SinkState {
            dir: dir.clone(),
            writer: BufWriter::with_capacity(FLUSH_BYTES, file),
            current_path: path,
            bytes_in_file: bytes,
            max_bytes,
        }
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
            body: SpurEventBody::TurnComplete { session: SessionId("s1".to_string()) },
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
            body: SpurEventBody::TurnComplete { session: SessionId("s1".to_string()) },
        };
        state.write_event(&event).unwrap();
        // No sleep needed — `rotated_path()`'s ROTATION_SEQ counter
        // guarantees unique filenames per-process even within one ms.
        state.write_event(&event).unwrap();
        state.flush().unwrap();

        let files: Vec<_> = fs::read_dir(&dir).unwrap().collect();
        assert!(files.len() >= 2, "expected rotation; got {} file(s)", files.len());
    }
}
