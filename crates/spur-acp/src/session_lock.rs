//! Single-attach lockfile for SPUR ACP sessions.
//!
//! Enforces the invariant that at most one orchestrator process holds an
//! active ACP attachment to a given session id. Backed by `fs4` advisory
//! locks; kernel auto-releases on process exit so no stale-lock recovery
//! is needed.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HolderInfo {
    pub pid: Option<u32>,
    pub started_at: Option<DateTime<Utc>>,
    pub tty: Option<String>,
    pub label: Option<String>,
    pub workdir: Option<PathBuf>,
}

pub struct SessionAttachGuard {
    file: std::fs::File,
    pid_path: PathBuf,
    acp_id: String,
}

pub enum AcquireOutcome {
    /// Exclusive ownership; proceed.
    Acquired(SessionAttachGuard),
    /// Filesystem rejected advisory locking (NFS/sshfs/SMB ENOTSUP/ENOLCK).
    /// Caller should attach with `fs_unsafe = true`.
    DegradedNoLock { reason: String },
    /// Another process holds it.
    Rejected { holder: HolderInfo },
    /// Unrecoverable IO error (permissions, disk full, etc.).
    Io(std::io::Error),
}

impl SessionAttachGuard {
    pub fn acp_id(&self) -> &str {
        &self.acp_id
    }

    pub fn try_acquire_or_replace(
        repo_root: &Path,
        acp_id: &str,
        existing_guard: Option<SessionAttachGuard>,
    ) -> AcquireOutcome {
        if let Some(guard) = existing_guard {
            if guard.acp_id() == acp_id {
                return AcquireOutcome::Acquired(guard);
            }
            drop(guard);
        }

        Self::try_acquire(repo_root, acp_id)
    }

    pub fn try_acquire(repo_root: &Path, acp_id: &str) -> AcquireOutcome {
        use fs4::fs_std::FileExt;

        let dir = repo_root.join(".spur").join("sessions");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return AcquireOutcome::Io(e);
        }
        let pid_path = dir.join(format!("{acp_id}.attach.lock"));

        let file = match std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&pid_path)
        {
            Ok(f) => f,
            Err(e) => return AcquireOutcome::Io(e),
        };

        match file.try_lock_exclusive() {
            Ok(true) => {
                let info = HolderInfo {
                    pid: Some(std::process::id()),
                    started_at: Some(Utc::now()),
                    tty: detect_tty(),
                    label: std::env::var("SPUR_TUI_LABEL").ok(),
                    workdir: std::env::current_dir().ok(),
                };
                let _ = write_holder_info(&pid_path, &info);

                AcquireOutcome::Acquired(SessionAttachGuard {
                    file,
                    pid_path,
                    acp_id: acp_id.to_string(),
                })
            }
            Ok(false) => {
                let holder = read_holder_info(&pid_path).unwrap_or_default();
                AcquireOutcome::Rejected { holder }
            }
            Err(e) => classify_lock_error(e, &pid_path),
        }
    }
}

fn detect_tty() -> Option<String> {
    // Phase 3 polish; return None for now.
    None
}

fn write_holder_info(path: &Path, info: &HolderInfo) -> std::io::Result<()> {
    use std::io::Write;

    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .truncate(false)
        .open(path)?;
    f.set_len(0)?;
    let json = serde_json::to_string(info).unwrap_or_else(|_| "{}".into());
    f.write_all(json.as_bytes())?;
    Ok(())
}

fn read_holder_info(path: &Path) -> Option<HolderInfo> {
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

fn classify_lock_error(e: std::io::Error, _path: &Path) -> AcquireOutcome {
    use std::io::ErrorKind;

    #[cfg(unix)]
    let is_unsupported = {
        let raw = e.raw_os_error();
        raw == Some(libc::ENOTSUP) || raw == Some(libc::ENOLCK)
    };
    #[cfg(not(unix))]
    let is_unsupported = false;

    if is_unsupported || matches!(e.kind(), ErrorKind::Unsupported) {
        AcquireOutcome::DegradedNoLock {
            reason: format!("flock unsupported on volume: {e}"),
        }
    } else {
        AcquireOutcome::Io(e)
    }
}

impl Drop for SessionAttachGuard {
    fn drop(&mut self) {
        // Touch these fields so the guard's ownership metadata remains
        // intentional even though cleanup only needs the path.
        let _ = &self.file;
        let _ = &self.acp_id;
        // Best-effort cleanup; kernel releases the flock on file close.
        let _ = std::fs::remove_file(&self.pid_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn acquire_then_release_drops_lock() {
        let tmp = TempDir::new().unwrap();
        match SessionAttachGuard::try_acquire(tmp.path(), "test-session") {
            AcquireOutcome::Acquired(guard) => {
                drop(guard);
                // Re-acquire should succeed after Drop releases the flock.
                match SessionAttachGuard::try_acquire(tmp.path(), "test-session") {
                    AcquireOutcome::Acquired(_) => {}
                    other => panic!(
                        "expected Acquired after release, got {:?}",
                        std::mem::discriminant(&other)
                    ),
                }
            }
            other => panic!(
                "expected Acquired, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn replace_drops_mismatched_guard_and_acquires_new_id() {
        let tmp = TempDir::new().unwrap();
        let guard_a = match SessionAttachGuard::try_acquire(tmp.path(), "A") {
            AcquireOutcome::Acquired(g) => g,
            other => panic!(
                "expected Acquired for A, got {:?}",
                std::mem::discriminant(&other)
            ),
        };

        let guard_b =
            match SessionAttachGuard::try_acquire_or_replace(tmp.path(), "B", Some(guard_a)) {
                AcquireOutcome::Acquired(g) => g,
                other => panic!(
                    "expected Acquired for B, got {:?}",
                    std::mem::discriminant(&other)
                ),
            };

        assert_eq!(guard_b.acp_id(), "B");
        match SessionAttachGuard::try_acquire(tmp.path(), "A") {
            AcquireOutcome::Acquired(_) => {}
            other => panic!(
                "expected A to be released, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn concurrent_acquire_in_same_process_returns_rejected_with_pid() {
        let tmp = TempDir::new().unwrap();
        let first = match SessionAttachGuard::try_acquire(tmp.path(), "shared") {
            AcquireOutcome::Acquired(g) => g,
            other => panic!(
                "expected first Acquired, got {:?}",
                std::mem::discriminant(&other)
            ),
        };
        match SessionAttachGuard::try_acquire(tmp.path(), "shared") {
            AcquireOutcome::Rejected { holder } => {
                assert_eq!(holder.pid, Some(std::process::id()));
                assert!(
                    holder.started_at.is_some(),
                    "started_at should be populated"
                );
            }
            other => panic!(
                "expected Rejected, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
        drop(first);
    }

    #[cfg(unix)]
    #[test]
    fn enotsup_returns_degraded_no_lock() {
        let raw_err = std::io::Error::from_raw_os_error(libc::ENOTSUP);
        let outcome = classify_lock_error(raw_err, std::path::Path::new("/tmp/x"));
        assert!(matches!(outcome, AcquireOutcome::DegradedNoLock { .. }));
    }

    #[test]
    fn set_len_zero_truncates_previous_holder_pid() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join(".spur").join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aaa.attach.lock");

        std::fs::write(
            &path,
            r#"{"pid":99999999,"label":"old-very-long-label-text"}"#,
        )
        .unwrap();

        let info = HolderInfo {
            pid: Some(1),
            ..Default::default()
        };
        write_holder_info(&path, &info).unwrap();

        let parsed: HolderInfo =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.pid, Some(1));
        let written = serde_json::to_string(&info).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().len() as usize,
            written.len()
        );
    }

    #[test]
    fn holder_info_parses_with_only_pid_field() {
        let json = r#"{"pid": 42}"#;
        let parsed: HolderInfo = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.pid, Some(42));
        assert_eq!(parsed.started_at, None);
        assert_eq!(parsed.label, None);
    }

    #[test]
    fn holder_info_parses_empty_object_to_all_none() {
        let json = "{}";
        let parsed: HolderInfo = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.pid, None);
    }
}
