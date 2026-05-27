use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::sync::LazyLock;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, System};

const DEFAULT_WRITE_LOCK_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_HEARTBEAT_MS: u64 = 2_000;
pub(crate) const WRITE_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(25);

static PROCESS_START_EPOCH_MS: LazyLock<u64> = LazyLock::new(process_start_epoch_ms);

pub(crate) enum WriteLockAttempt {
    Acquired(WriteLockGuard),
    Busy(Option<LockHolder>),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WriteLockError {
    #[error(
        "Timed out after {timeout_ms}ms waiting for write lock at {path}. \
         Another br process may be holding .write.lock; retry after it exits or investigate a stuck process.{holder_suffix}",
        path = path.display(),
        holder_suffix = holder_suffix(holder.as_ref(), *timeout_ms)
    )]
    Busy {
        path: PathBuf,
        timeout_ms: u64,
        holder: Option<LockHolder>,
    },
}

#[derive(Debug)]
pub(crate) struct WriteLockGuard {
    file: File,
    stop_tx: Option<mpsc::Sender<()>>,
    heartbeat: Option<thread::JoinHandle<()>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LockHolder {
    pub pid: Option<u32>,
    pub process_start_epoch_ms: Option<u64>,
    pub repo_root: Option<String>,
    pub argv: Option<String>,
    pub host: Option<String>,
    pub heartbeat_counter: Option<u64>,
    pub acquired_at_epoch_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct LockPayload {
    pid: u32,
    process_start_epoch_ms: u64,
    repo_root: String,
    argv: String,
    host: String,
    heartbeat_counter: u64,
    acquired_at_epoch_ms: u64,
}

pub(crate) fn try_blocking_write_lock_once(beads_dir: &Path) -> anyhow::Result<WriteLockAttempt> {
    let lock_path = beads_dir.join(".write.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("Failed to open write lock at {}", lock_path.display()))?;

    match file.try_lock_exclusive() {
        Ok(()) => {
            let guard = WriteLockGuard::new(file, beads_dir, &lock_path)?;
            Ok(WriteLockAttempt::Acquired(guard))
        }
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
            Ok(WriteLockAttempt::Busy(read_holder_payload(&lock_path)))
        }
        Err(err) => {
            anyhow::bail!(
                "Failed to acquire write lock at {}: {err}",
                lock_path.display()
            );
        }
    }
}

pub(crate) fn blocking_write_lock_with_timeout(
    beads_dir: &Path,
    lock_timeout_ms: Option<u64>,
) -> anyhow::Result<WriteLockGuard> {
    let lock_path = beads_dir.join(".write.lock");
    let file = match try_blocking_write_lock_once(beads_dir)? {
        WriteLockAttempt::Acquired(guard) => return Ok(guard),
        WriteLockAttempt::Busy(_) => OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("Failed to open write lock at {}", lock_path.display()))?,
    };

    let timeout_ms = lock_timeout_ms.unwrap_or(DEFAULT_WRITE_LOCK_TIMEOUT_MS);
    let timeout = Duration::from_millis(timeout_ms);
    let start = Instant::now();
    let mut holder = read_holder_payload(&lock_path);

    loop {
        if start.elapsed() >= timeout {
            if let Some(holder) = holder.as_ref() {
                tracing::warn!(
                    holder = %format_lock_holder(holder, timeout_ms),
                    "timed out waiting for beads write lock"
                );
            }
            return Err(WriteLockError::Busy {
                path: lock_path,
                timeout_ms,
                holder,
            }
            .into());
        }

        let remaining = timeout.saturating_sub(start.elapsed());
        thread::sleep(remaining.min(WRITE_LOCK_POLL_INTERVAL));

        match file.try_lock_exclusive() {
            Ok(()) => return WriteLockGuard::new(file, beads_dir, &lock_path),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                holder = read_holder_payload(&lock_path);
            }
            Err(err) => {
                anyhow::bail!(
                    "Failed to acquire write lock at {}: {err}",
                    lock_path.display()
                );
            }
        }
    }
}

impl WriteLockGuard {
    fn new(mut file: File, beads_dir: &Path, lock_path: &Path) -> anyhow::Result<Self> {
        let payload = LockPayload::new(beads_dir);
        write_payload(&mut file, &payload).with_context(|| {
            format!(
                "Failed to write holder payload into write lock at {}",
                lock_path.display()
            )
        })?;

        let interval = heartbeat_interval();
        let heartbeat_file = file.try_clone().ok();
        let (stop_tx, stop_rx) = mpsc::channel();
        let heartbeat = heartbeat_file.map(|mut heartbeat_file| {
            thread::spawn(move || {
                let mut payload = payload;
                while stop_rx.recv_timeout(interval).is_err() {
                    payload.heartbeat_counter = payload.heartbeat_counter.saturating_add(1);
                    let _ = write_payload(&mut heartbeat_file, &payload);
                }
            })
        });

        Ok(Self {
            file,
            stop_tx: Some(stop_tx),
            heartbeat,
        })
    }
}

impl Drop for WriteLockGuard {
    fn drop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(heartbeat) = self.heartbeat.take() {
            let _ = heartbeat.join();
        }
        let _ = self.file.set_len(0);
        let _ = self.file.seek(SeekFrom::Start(0));
        let _ = self.file.flush();
    }
}

impl LockPayload {
    fn new(beads_dir: &Path) -> Self {
        Self {
            pid: std::process::id(),
            process_start_epoch_ms: *PROCESS_START_EPOCH_MS,
            repo_root: repo_root_for(beads_dir),
            argv: std::env::args().collect::<Vec<_>>().join(" "),
            host: hostname(),
            heartbeat_counter: 0,
            acquired_at_epoch_ms: current_epoch_ms(),
        }
    }
}

fn write_payload(file: &mut File, payload: &LockPayload) -> anyhow::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    serde_json::to_writer(&mut *file, payload)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

pub(crate) fn read_holder_payload(lock_path: &Path) -> Option<LockHolder> {
    let mut file = File::open(lock_path).ok()?;
    let mut payload = String::new();
    file.read_to_string(&mut payload).ok()?;
    serde_json::from_str(payload.trim()).ok()
}

pub(crate) fn format_lock_holder(holder: &LockHolder, waited_ms: u64) -> String {
    let pid = holder
        .pid
        .map(|pid| pid.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let argv = holder
        .argv
        .as_deref()
        .filter(|argv| !argv.is_empty())
        .unwrap_or("unknown command");
    let host = holder
        .host
        .as_deref()
        .filter(|host| !host.is_empty())
        .unwrap_or("unknown host");
    let held_for = holder
        .acquired_at_epoch_ms
        .map(|acquired| human_duration(current_epoch_ms().saturating_sub(acquired)))
        .unwrap_or_else(|| "unknown duration".to_string());
    let waited = human_duration(waited_ms);

    format!(
        "beads write lock held by PID {pid} (`{argv}` for {held_for}, host {host}); waited {waited} - try again or kill the holder if stale"
    )
}

fn holder_suffix(holder: Option<&LockHolder>, waited_ms: u64) -> String {
    holder
        .map(|holder| format!(" {}", format_lock_holder(holder, waited_ms)))
        .unwrap_or_default()
}

fn heartbeat_interval() -> Duration {
    std::env::var("SPUR_BEADS_LOCK_HEARTBEAT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_HEARTBEAT_MS))
}

fn repo_root_for(beads_dir: &Path) -> String {
    let root = if beads_dir.file_name().is_some_and(|name| name == ".beads") {
        beads_dir.parent().unwrap_or(beads_dir)
    } else {
        beads_dir
    };
    root.display().to_string()
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|host| !host.trim().is_empty())
        .or_else(|| {
            Command::new("hostname").output().ok().and_then(|output| {
                if output.status.success() {
                    String::from_utf8(output.stdout)
                        .ok()
                        .map(|host| host.trim().to_string())
                        .filter(|host| !host.is_empty())
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn process_start_epoch_ms() -> u64 {
    let system = System::new_all();
    system
        .process(Pid::from_u32(std::process::id()))
        .map(|process| process.start_time().saturating_mul(1_000))
        .unwrap_or_else(current_epoch_ms)
}

fn human_duration(ms: u64) -> String {
    let secs = ms / 1_000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3_600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tempfile::TempDir;

    #[test]
    fn lock_writes_holder_payload_and_truncates_on_drop() {
        let dir = TempDir::new().unwrap();
        let lock = blocking_write_lock_with_timeout(dir.path(), Some(50)).unwrap();

        let lock_path = dir.path().join(".write.lock");
        assert!(lock_path.exists());

        let payload = std::fs::read_to_string(&lock_path).unwrap();
        let value: Value = serde_json::from_str(payload.trim_end()).unwrap();
        assert_eq!(value["pid"].as_u64(), Some(std::process::id() as u64));
        assert!(value["process_start_epoch_ms"].as_u64().is_some());
        assert_eq!(
            value["repo_root"].as_str(),
            Some(dir.path().to_str().unwrap())
        );
        assert!(value["argv"].as_str().is_some_and(|argv| !argv.is_empty()));
        assert!(value["host"].as_str().is_some_and(|host| !host.is_empty()));
        assert_eq!(value["heartbeat_counter"].as_u64(), Some(0));
        assert!(value["acquired_at_epoch_ms"].as_u64().is_some());

        drop(lock);

        assert_eq!(std::fs::metadata(lock_path).unwrap().len(), 0);
    }

    #[test]
    fn lock_times_out_when_already_held() {
        let dir = TempDir::new().unwrap();
        let held = blocking_write_lock_with_timeout(dir.path(), Some(50)).unwrap();

        let err = blocking_write_lock_with_timeout(dir.path(), Some(25)).unwrap_err();

        assert!(
            err.to_string().contains("Timed out after 25ms"),
            "unexpected error: {err:#}"
        );
        drop(held);
    }

    #[test]
    fn busy_error_includes_holder_payload_when_available() {
        let dir = TempDir::new().unwrap();
        let held = blocking_write_lock_with_timeout(dir.path(), Some(50)).unwrap();

        let err = blocking_write_lock_with_timeout(dir.path(), Some(25)).unwrap_err();
        let busy = err
            .downcast_ref::<WriteLockError>()
            .expect("timeout should be a WriteLockError");

        match busy {
            WriteLockError::Busy { holder, .. } => {
                let holder = holder.as_ref().expect("holder payload parsed");
                assert_eq!(holder.pid, Some(std::process::id()));
                assert_eq!(
                    holder.repo_root.as_deref(),
                    Some(dir.path().to_str().unwrap())
                );
            }
        }
        assert!(
            err.to_string()
                .contains("Another br process may be holding .write.lock"),
            "legacy timeout text should be preserved: {err:#}"
        );
        assert!(
            err.to_string().contains("beads write lock held by PID"),
            "holder text should augment timeout: {err:#}"
        );

        drop(held);
    }
}
