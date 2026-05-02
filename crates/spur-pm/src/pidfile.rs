//! OS-level advisory pidfile for single-brain-per-`.beads/` (I4).
//!
//! Uses `fs2::FileExt::try_lock_exclusive` — non-blocking, advisory. On
//! acquire success the file contains the current PID; on drop, the lock
//! releases. The file remains on disk and is overwritten by the next holder,
//! which avoids false single-session breakage when one process drops its lock
//! while another has already reacquired it.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use fs2::FileExt;

#[allow(clippy::incompatible_msrv)]
pub struct PidFileGuard {
    file: Option<File>,
    path: PathBuf,
}

impl std::fmt::Debug for PidFileGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PidFileGuard")
            .field("path", &self.path)
            .finish()
    }
}

impl PidFileGuard {
    /// Attempt to acquire the pidfile at `path`. Returns `Err` if another
    /// live process holds the lock; returns `Ok(guard)` on success,
    /// including the case where a stale pidfile exists (previous holder
    /// crashed — OS already released the kernel lock).
    pub fn acquire(path: &Path) -> anyhow::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("opening pidfile at {path:?}"))?;
        file.try_lock_exclusive().map_err(|e| {
            let holder = std::fs::read_to_string(path)
                .ok()
                .map(|pid| pid.trim().to_string())
                .filter(|pid| !pid.is_empty());
            match holder {
                Some(pid) => anyhow!(
                    "pidfile {:?} held by another brain session (holder pid {}): {e}",
                    path,
                    pid
                ),
                None => anyhow!("pidfile {:?} held by another brain session: {e}", path),
            }
        })?;
        let mut f = &file;
        f.set_len(0)?;
        writeln!(f, "{}", std::process::id())?;
        Ok(Self {
            file: Some(file),
            path: path.to_path_buf(),
        })
    }
}

#[allow(clippy::incompatible_msrv)]
impl Drop for PidFileGuard {
    fn drop(&mut self) {
        if let Some(f) = self.file.take() {
            let _ = f.unlock();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn acquire_then_second_acquire_fails() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".spur-brain.pid");
        let _g1 = PidFileGuard::acquire(&path).unwrap();
        let err = PidFileGuard::acquire(&path).unwrap_err();
        assert!(format!("{err}").contains("held by another"));
    }

    #[test]
    fn second_acquire_reports_holder_pid() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".spur-brain.pid");
        let _g1 = PidFileGuard::acquire(&path).unwrap();
        let err = PidFileGuard::acquire(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains(&std::process::id().to_string()),
            "expected pidfile lock error to report holder pid, got: {msg}"
        );
    }

    #[test]
    fn drop_releases_for_reacquire() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".spur-brain.pid");
        {
            let _g = PidFileGuard::acquire(&path).unwrap();
        }
        let _g2 = PidFileGuard::acquire(&path).unwrap();
    }

    #[test]
    fn stale_file_is_acquirable() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".spur-brain.pid");
        std::fs::write(&path, "99999\n").unwrap();
        let _g = PidFileGuard::acquire(&path).expect("stale pidfile should be reacquirable");
    }

    #[test]
    fn pidfile_remains_for_next_holder() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".spur-brain.pid");
        {
            let _g1 = PidFileGuard::acquire(&path).unwrap();
        }
        assert!(
            path.exists(),
            "pidfile path must remain for continuity across restarts"
        );
        let _g2 = PidFileGuard::acquire(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            std::process::id().to_string()
        );
    }
}
