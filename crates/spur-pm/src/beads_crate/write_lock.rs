use std::fs::{File, OpenOptions};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use fs2::FileExt;

const DEFAULT_WRITE_LOCK_TIMEOUT_MS: u64 = 30_000;
pub(crate) const WRITE_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(25);

pub(crate) enum WriteLockAttempt {
    Acquired(File),
    Busy,
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
        Ok(()) => Ok(WriteLockAttempt::Acquired(file)),
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(WriteLockAttempt::Busy),
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
) -> anyhow::Result<File> {
    let lock_path = beads_dir.join(".write.lock");
    let file = match try_blocking_write_lock_once(beads_dir)? {
        WriteLockAttempt::Acquired(file) => return Ok(file),
        WriteLockAttempt::Busy => OpenOptions::new()
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

    loop {
        if start.elapsed() >= timeout {
            anyhow::bail!(
                "Timed out after {timeout_ms}ms waiting for write lock at {}. \
                 Another br process may be holding .write.lock; retry after it exits or investigate a stuck process.",
                lock_path.display()
            );
        }

        let remaining = timeout.saturating_sub(start.elapsed());
        thread::sleep(remaining.min(WRITE_LOCK_POLL_INTERVAL));

        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) => {
                anyhow::bail!(
                    "Failed to acquire write lock at {}: {err}",
                    lock_path.display()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn lock_uses_beads_write_lock_file() {
        let dir = TempDir::new().unwrap();
        let lock = blocking_write_lock_with_timeout(dir.path(), Some(50)).unwrap();

        assert!(dir.path().join(".write.lock").exists());

        drop(lock);
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
}
