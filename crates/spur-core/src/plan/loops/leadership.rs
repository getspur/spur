use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};

pub(crate) const LOOP_RUNTIME_LOCK_PATH: &str = ".spur/loop-runtime.lock";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LoopRuntimeHolder {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub label: Option<String>,
    pub workdir: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct LoopRuntimeLeadership {
    _file: std::fs::File,
}

#[derive(Debug)]
pub(crate) enum LoopRuntimeLeadershipOutcome {
    Acquired(LoopRuntimeLeadership),
    Standby { holder: Option<LoopRuntimeHolder> },
    Unsafe { reason: String },
    Io(std::io::Error),
}

impl LoopRuntimeLeadership {
    pub(crate) fn try_acquire(repo_root: &Path) -> LoopRuntimeLeadershipOutcome {
        use fs4::fs_std::FileExt as _;

        let lock_path = repo_root.join(LOOP_RUNTIME_LOCK_PATH);
        let Some(parent) = lock_path.parent() else {
            return LoopRuntimeLeadershipOutcome::Io(std::io::Error::other(
                "loop runtime lock has no parent directory",
            ));
        };
        if let Err(error) = std::fs::create_dir_all(parent) {
            return LoopRuntimeLeadershipOutcome::Io(error);
        }

        let mut file = match std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(error) => return LoopRuntimeLeadershipOutcome::Io(error),
        };

        match file.try_lock_exclusive() {
            Ok(true) => {
                let holder = LoopRuntimeHolder {
                    pid: std::process::id(),
                    started_at: Utc::now(),
                    label: std::env::var("SPUR_TUI_LABEL").ok(),
                    workdir: std::env::current_dir().ok(),
                };
                if let Err(error) = write_holder_info(&mut file, &holder) {
                    return LoopRuntimeLeadershipOutcome::Io(error);
                }
                LoopRuntimeLeadershipOutcome::Acquired(Self { _file: file })
            }
            Ok(false) => LoopRuntimeLeadershipOutcome::Standby {
                holder: read_holder_info(&lock_path),
            },
            Err(error) => classify_lock_error(error),
        }
    }
}

fn write_holder_info(file: &mut std::fs::File, holder: &LoopRuntimeHolder) -> std::io::Result<()> {
    let payload = serde_json::to_vec(holder).map_err(std::io::Error::other)?;
    file.set_len(0)?;
    file.rewind()?;
    file.write_all(&payload)?;
    file.flush()
}

fn read_holder_info(lock_path: &Path) -> Option<LoopRuntimeHolder> {
    let payload = std::fs::read(lock_path).ok()?;
    serde_json::from_slice(&payload).ok()
}

fn classify_lock_error(error: std::io::Error) -> LoopRuntimeLeadershipOutcome {
    #[cfg(unix)]
    let unsupported = {
        let raw = error.raw_os_error();
        raw == Some(libc::ENOTSUP) || raw == Some(libc::EOPNOTSUPP) || raw == Some(libc::ENOLCK)
    };
    #[cfg(not(unix))]
    let unsupported = false;

    if unsupported || error.kind() == std::io::ErrorKind::Unsupported {
        LoopRuntimeLeadershipOutcome::Unsafe {
            reason: format!("advisory locking is unsupported for the loop runtime: {error}"),
        }
    } else {
        LoopRuntimeLeadershipOutcome::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    struct ChildLockHolder(Child);

    impl ChildLockHolder {
        fn wait(mut self) {
            let status = self.0.wait().expect("child lock holder must exit");
            assert!(status.success(), "child lock holder failed: {status}");
            std::mem::forget(self);
        }
    }

    impl Drop for ChildLockHolder {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn acquired(repo_root: &std::path::Path) -> LoopRuntimeLeadership {
        match LoopRuntimeLeadership::try_acquire(repo_root) {
            LoopRuntimeLeadershipOutcome::Acquired(leadership) => leadership,
            LoopRuntimeLeadershipOutcome::Standby { holder } => {
                panic!("expected leadership, found standby holder {holder:?}")
            }
            LoopRuntimeLeadershipOutcome::Unsafe { reason } => {
                panic!("expected leadership, locking was unsafe: {reason}")
            }
            LoopRuntimeLeadershipOutcome::Io(error) => {
                panic!("expected leadership, got I/O error: {error}")
            }
        }
    }

    fn wait_for_path(path: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(Instant::now() < deadline, "timed out waiting for {path:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn first_guard_acquires_and_writes_holder_metadata() {
        let repo = TempDir::new().unwrap();
        let _guard = acquired(repo.path());
        let lock_path = repo.path().join(LOOP_RUNTIME_LOCK_PATH);
        let holder: LoopRuntimeHolder =
            serde_json::from_str(&std::fs::read_to_string(lock_path).unwrap()).unwrap();

        assert_eq!(holder.pid, std::process::id());
        assert!(holder.workdir.is_some());
    }

    #[test]
    fn child_process_holder_forces_standby_then_release_allows_takeover() {
        let repo = TempDir::new().unwrap();
        let ready = repo.path().join("child-ready");
        let release = repo.path().join("child-release");
        let child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("plan::loops::leadership::tests::child_process_lock_holder")
            .arg("--nocapture")
            .env("SPUR_LOOP_RUNTIME_CHILD_REPO", repo.path())
            .env("SPUR_LOOP_RUNTIME_CHILD_READY", &ready)
            .env("SPUR_LOOP_RUNTIME_CHILD_RELEASE", &release)
            .spawn()
            .unwrap();
        let child = ChildLockHolder(child);
        wait_for_path(&ready);

        match LoopRuntimeLeadership::try_acquire(repo.path()) {
            LoopRuntimeLeadershipOutcome::Standby {
                holder: Some(holder),
            } => assert_ne!(holder.pid, std::process::id()),
            other => panic!("expected standby with holder metadata, got {other:?}"),
        }

        std::fs::write(&release, b"release").unwrap();
        child.wait();
        let _successor = acquired(repo.path());
    }

    #[test]
    fn child_process_lock_holder() {
        let Ok(repo) = std::env::var("SPUR_LOOP_RUNTIME_CHILD_REPO") else {
            return;
        };
        let ready = std::env::var("SPUR_LOOP_RUNTIME_CHILD_READY").unwrap();
        let release = std::env::var("SPUR_LOOP_RUNTIME_CHILD_RELEASE").unwrap();
        let _guard = acquired(std::path::Path::new(&repo));
        std::fs::write(ready, b"ready").unwrap();
        wait_for_path(std::path::Path::new(&release));
    }

    #[test]
    fn different_repositories_have_independent_leaders() {
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();

        let _first_guard = acquired(first.path());
        let _second_guard = acquired(second.path());
    }

    #[test]
    fn unsupported_advisory_locking_fails_closed() {
        let error = std::io::Error::from_raw_os_error(libc::ENOTSUP);
        match classify_lock_error(error) {
            LoopRuntimeLeadershipOutcome::Unsafe { reason } => {
                assert!(reason.contains("unsupported"));
            }
            other => panic!("unsupported locking must be unsafe, got {other:?}"),
        }
    }

    #[test]
    fn acquiring_runtime_leadership_does_not_dirty_a_clean_repository() {
        let repo = TempDir::new().unwrap();
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(repo.path())
            .status()
            .expect("git init");
        assert!(status.success());
        std::fs::write(
            repo.path().join(".gitignore"),
            include_str!("../../../../../.gitignore"),
        )
        .unwrap();
        let status = Command::new("git")
            .args(["add", ".gitignore"])
            .current_dir(repo.path())
            .status()
            .expect("git add");
        assert!(status.success());
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=SPUR Test",
                "-c",
                "user.email=spur-test@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "test fixture",
            ])
            .current_dir(repo.path())
            .status()
            .expect("git commit");
        assert!(status.success());

        let _guard = acquired(repo.path());
        let output = Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=all"])
            .current_dir(repo.path())
            .output()
            .expect("git status");
        assert!(output.status.success());
        assert!(
            output.stdout.is_empty(),
            "runtime startup dirtied repository: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
