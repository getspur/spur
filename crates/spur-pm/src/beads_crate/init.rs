//! Init-time guards for the beads crate adapter.
//!
//! Each function is a precondition that must hold before `BeadsCrateAdapter`
//! is allowed to open the writer connection.

use beads_rust::storage::sqlite::SqliteStorage;
use beads_rust::sync;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error(
        "non-local filesystem detected at {path}: fs_type = {fs_type}. \
             flock semantics are not portable here. \
             Set allow_non_local_fs=true in config to bypass."
    )]
    NonLocalFilesystem { path: String, fs_type: String },
}

/// Returns Ok(()) for local filesystems; Err for known network mounts (NFS, SMB, etc.).
/// Best-effort: on platforms where we cannot determine the FS type, returns Ok.
pub fn detect_local_fs(beads_dir: &Path) -> Result<(), InitError> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let path = CString::new(beads_dir.as_os_str().as_bytes()).unwrap();
        let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statfs(path.as_ptr(), &mut buf) };
        if rc != 0 {
            return Ok(());
        } // can't determine; allow
          // Magic numbers from <linux/magic.h>
        const NFS_SUPER_MAGIC: i64 = 0x6969;
        const SMB_SUPER_MAGIC: i64 = 0x517B;
        const CIFS_MAGIC_NUMBER: i64 = 0xFF534D42_u64 as i64;
        let ty = buf.f_type as i64;
        if ty == NFS_SUPER_MAGIC || ty == SMB_SUPER_MAGIC || ty == CIFS_MAGIC_NUMBER {
            return Err(InitError::NonLocalFilesystem {
                path: beads_dir.display().to_string(),
                fs_type: format!("0x{:x}", ty),
            });
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let path = CString::new(beads_dir.as_os_str().as_bytes()).unwrap();
        let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statfs(path.as_ptr(), &mut buf) };
        if rc != 0 {
            return Ok(());
        }
        let fs_name = unsafe {
            let raw = buf.f_fstypename.as_ptr();
            std::ffi::CStr::from_ptr(raw).to_string_lossy().into_owned()
        };
        if matches!(fs_name.as_str(), "nfs" | "smbfs" | "cifs" | "afpfs") {
            return Err(InitError::NonLocalFilesystem {
                path: beads_dir.display().to_string(),
                fs_type: fs_name,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn local_tempdir_is_local() {
        let dir = TempDir::new().unwrap();
        // tmpfs / APFS local — should pass
        assert!(detect_local_fs(dir.path()).is_ok());
    }
}

use std::time::{Duration, SystemTime};

/// Pattern matching the temp files beads_rust creates during atomic JSONL writes.
/// Per beads_rust 0.2.1 `sync::export_temp_path`:
///
/// ```ignore
/// pub(crate) fn export_temp_path(output_path: &Path) -> PathBuf {
///     output_path.with_extension(format!("jsonl.{}.tmp", std::process::id()))
/// }
/// ```
///
/// For input `issues.jsonl`, `Path::with_extension` strips `.jsonl` and appends
/// the new extension, producing `issues.jsonl.<pid>.tmp`. The PID is decimal
/// digits; there is NO random suffix. We deliberately match strictly so we
/// never touch SQLite sidecars (`-wal`, `-shm`) or the live `issues.jsonl`.
fn is_jsonl_temp_file(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("issues.jsonl.") else {
        return false;
    };
    let Some(pid_str) = rest.strip_suffix(".tmp") else {
        return false;
    };
    !pid_str.is_empty() && pid_str.bytes().all(|b| b.is_ascii_digit())
}

/// Sweep stale jsonl temp files older than `min_age`. Returns count removed.
/// Caller MUST hold .write.lock when calling this; we do not acquire it here.
#[allow(dead_code)] // wired into Section C/D adapter open path
pub(crate) fn sweep_stale_jsonl_temps(beads_dir: &Path, min_age: Duration) -> std::io::Result<u64> {
    let now = SystemTime::now();
    let mut removed = 0;
    let entries = match std::fs::read_dir(beads_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !is_jsonl_temp_file(name_str) {
            continue;
        }
        // TOCTOU: the file may have been renamed/unlinked between read_dir and
        // metadata (e.g., a concurrent atomic write completing). Skip the entry
        // instead of failing the whole sweep.
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if let Ok(modified) = meta.modified() {
            if let Ok(age) = now.duration_since(modified) {
                if age >= min_age {
                    if std::fs::remove_file(entry.path()).is_ok() {
                        removed += 1;
                    }
                }
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod sweep_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ignores_wal_and_shm_sidecars_and_live_jsonl() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("issues.jsonl"), b"x").unwrap();
        std::fs::write(dir.path().join("issues.jsonl-wal"), b"x").unwrap();
        std::fs::write(dir.path().join("issues.jsonl-shm"), b"x").unwrap();
        std::fs::write(dir.path().join("beads.db-wal"), b"x").unwrap();
        let removed = sweep_stale_jsonl_temps(dir.path(), Duration::ZERO).unwrap();
        assert_eq!(removed, 0);
        assert!(dir.path().join("issues.jsonl").exists());
        assert!(dir.path().join("issues.jsonl-wal").exists());
        assert!(dir.path().join("beads.db-wal").exists());
    }

    #[test]
    fn removes_old_jsonl_tmp_files() {
        let dir = TempDir::new().unwrap();
        // beads_rust temp scheme: `issues.jsonl.<pid>.tmp` (decimal digits only)
        let p = dir.path().join("issues.jsonl.12345.tmp");
        std::fs::write(&p, b"orphan").unwrap();
        let removed = sweep_stale_jsonl_temps(dir.path(), Duration::ZERO).unwrap();
        assert_eq!(removed, 1);
        assert!(!p.exists());
    }

    #[test]
    fn keeps_recent_tmp_files() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("issues.jsonl.99999.tmp");
        std::fs::write(&p, b"in-flight").unwrap();
        let removed = sweep_stale_jsonl_temps(dir.path(), Duration::from_secs(3600)).unwrap();
        assert_eq!(removed, 0);
        assert!(p.exists());
    }

    #[test]
    fn ignores_non_pid_lookalikes() {
        let dir = TempDir::new().unwrap();
        // Right shape but non-digit middle — must not be matched.
        let p1 = dir.path().join("issues.jsonl.abc.tmp");
        // Right prefix/suffix but empty middle.
        let p2 = dir.path().join("issues.jsonl..tmp");
        std::fs::write(&p1, b"x").unwrap();
        std::fs::write(&p2, b"x").unwrap();
        let removed = sweep_stale_jsonl_temps(dir.path(), Duration::ZERO).unwrap();
        assert_eq!(removed, 0);
        assert!(p1.exists());
        assert!(p2.exists());
    }

    #[test]
    fn tolerates_missing_dir() {
        let dir = TempDir::new().unwrap();
        let nonexistent = dir.path().join("does-not-exist");
        assert_eq!(
            sweep_stale_jsonl_temps(&nonexistent, Duration::ZERO).unwrap(),
            0
        );
    }
}

/// Open the writer connection with cross-process serialization. Holds
/// `.beads/.write.lock` for the duration of schema/migration work.
///
/// Returns the opened `SqliteStorage` ready for writes. The flock is released
/// once the storage is fully initialized (the caller's later writes will
/// re-acquire it per-write).
#[allow(dead_code)] // wired into Section C/D adapter open path
pub(crate) fn open_writer_under_migration_lock(
    beads_dir: &Path,
    lock_timeout_ms: u64,
) -> anyhow::Result<SqliteStorage> {
    // Acquire .write.lock — guards schema migration against other instances
    // racing the same first-open. `_guard: File` must stay in scope until we
    // return; dropping it releases the flock.
    let _guard = sync::blocking_write_lock_with_timeout(beads_dir, Some(lock_timeout_ms))?;
    // Opening the storage runs schema init / migrations as needed.
    let db_path = beads_dir.join("beads.db");
    let storage = SqliteStorage::open_with_timeout(&db_path, Some(lock_timeout_ms))?;
    // _guard drops here, releasing flock.
    Ok(storage)
}

/// Detect whether the SQLite db has writes that haven't been flushed to JSONL,
/// and force a flush if so. Returns the `AutoFlushResult` for inspection.
///
/// # Lock contract
///
/// Caller MUST hold `.write.lock` for the duration of the call. `auto_flush`
/// reads dirty rows from SQLite and rewrites the JSONL file; without a lock,
/// a concurrent writer could interleave and corrupt the JSONL atomic-write.
///
/// Note that `open_writer_under_migration_lock` releases its flock on return.
/// The intended Section C/D wiring is:
///   1. Acquire `sync::blocking_write_lock_with_timeout(beads_dir, …)`.
///   2. Call `detect_and_force_flush_stale_jsonl` while holding it.
///   3. Drop the lock.
///
/// Tracked in bd-5z7w (T8/T9 lock-contract design follow-up).
#[allow(dead_code)] // wired into Section C/D adapter open path
pub(crate) fn detect_and_force_flush_stale_jsonl(
    storage: &mut SqliteStorage,
    beads_dir: &Path,
    jsonl_path: &Path,
) -> anyhow::Result<beads_rust::sync::AutoFlushResult> {
    // beads_rust auto_flush is idempotent: it computes whether the JSONL is
    // out of sync with the SQLite db, and is a no-op if not. We call it
    // unconditionally at boot. `allow_external_jsonl: false` because spur-pm
    // is the single writer to the JSONL file in our model.
    let result = sync::auto_flush(storage, beads_dir, jsonl_path, false)?;
    Ok(result)
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn opens_writer_in_fresh_dir() {
        let dir = TempDir::new().unwrap();
        let _writer =
            open_writer_under_migration_lock(dir.path(), 5_000).expect("first open should succeed");
        // Subsequent open in the same process should also succeed
        let _writer2 = open_writer_under_migration_lock(dir.path(), 5_000)
            .expect("second open should succeed");
    }

    #[test]
    fn force_flush_on_fresh_db_is_no_op() {
        let dir = TempDir::new().unwrap();
        let mut storage = open_writer_under_migration_lock(dir.path(), 5_000).unwrap();
        let jsonl = dir.path().join("issues.jsonl");
        let result = detect_and_force_flush_stale_jsonl(&mut storage, dir.path(), &jsonl).unwrap();
        // Fresh db has no dirty rows; auto_flush must report no work done.
        assert!(!result.flushed, "fresh db should not trigger a flush");
        assert_eq!(result.exported_count, 0, "no rows to export on fresh db");
    }
}
