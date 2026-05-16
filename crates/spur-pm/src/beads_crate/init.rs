//! Init-time guards for the beads crate adapter.
//!
//! Each function is a precondition that must hold before `BeadsCrateAdapter`
//! is allowed to open the writer connection.

use beads_rust::storage::sqlite::SqliteStorage;
use beads_rust::sync;
use std::path::Path;

use crate::beads_crate::{wal_checkpoint, write_lock};

const SKIP_PROBE_ENV: &str = "SPUR_BEADS_SKIP_PROBE";

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error(
        "non-local filesystem detected at {path}: fs_type = {fs_type}. \
             flock semantics are not portable here. \
             Set allow_non_local_fs=true in config to bypass."
    )]
    NonLocalFilesystem { path: String, fs_type: String },
    #[error("pre-open SQLite quick_check failed for {path}: {message}")]
    QuickCheckFailed { path: String, message: String },
}

/// Returns Ok(()) for local filesystems; Err for known network mounts (NFS, SMB, etc.).
/// Best-effort: on platforms where we cannot determine the FS type, returns Ok.
pub fn detect_local_fs(beads_dir: &Path) -> Result<(), InitError> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let Ok(path) = CString::new(beads_dir.as_os_str().as_bytes()) else {
            return Ok(()); // path can't be C-stringified — best-effort allow
        };
        let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statfs(path.as_ptr(), &mut buf) };
        if rc != 0 {
            return Ok(());
        } // can't determine; allow
          // Magic numbers from <linux/magic.h>; kernel stores them as __u32.
          // On 32-bit Linux, libc::statfs.f_type is i32, and `i32 as u64` SIGN-extends
          // (Rust language reference, Numeric Cast). The CIFS magic 0xFF534D42 has
          // the high bit set, so a direct `as u64` would yield 0xFFFFFFFF_FF534D42
          // and never match the constant. Cast through u32 first to zero-extend.
        const NFS_SUPER_MAGIC: u64 = 0x6969;
        const SMB_SUPER_MAGIC: u64 = 0x517B;
        const CIFS_MAGIC_NUMBER: u64 = 0xFF534D42;
        let ty = buf.f_type as u32 as u64;
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
        let Ok(path) = CString::new(beads_dir.as_os_str().as_bytes()) else {
            return Ok(()); // path can't be C-stringified — best-effort allow
        };
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

fn quick_check_probe_enabled() -> bool {
    !std::env::var(SKIP_PROBE_ENV).is_ok_and(|value| value == "1")
}

fn pre_open_quick_check(db_path: &Path) -> Result<(), InitError> {
    if !quick_check_probe_enabled() || !db_path.exists() {
        return Ok(());
    }

    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|err| InitError::QuickCheckFailed {
                path: db_path.display().to_string(),
                message: err.to_string(),
            })?;
    let result: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|err| InitError::QuickCheckFailed {
            path: db_path.display().to_string(),
            message: err.to_string(),
        })?;

    if result == "ok" {
        Ok(())
    } else {
        Err(InitError::QuickCheckFailed {
            path: db_path.display().to_string(),
            message: result,
        })
    }
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

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn nul_byte_in_path_does_not_panic() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::path::PathBuf;
        // Construct a path with an interior NUL byte. CString::new will reject this.
        let bad = PathBuf::from(OsString::from_vec(vec![b'a', 0, b'b']));
        // Best-effort: must not panic. Result is `Ok(())` because we can't determine FS.
        assert!(detect_local_fs(&bad).is_ok());
    }

    /// On 32-bit Linux, `libc::statfs.f_type` is `i32`. The CIFS magic
    /// `0xFF534D42` has the high bit set, so its `i32` representation is
    /// negative. Per the Rust reference, `i32 as u64` SIGN-extends — a direct
    /// cast yields `0xFFFFFFFF_FF534D42` and never matches the magic constant.
    /// Casting through `u32` first zero-extends. This test locks the cast
    /// contract regardless of build target.
    #[test]
    fn cifs_magic_does_not_sign_extend_through_i32() {
        let signed: i32 = 0xFF534D42_u32 as i32;
        assert!(signed.is_negative());
        let direct: u64 = signed as u64;
        let via_u32: u64 = signed as u32 as u64;
        assert_eq!(direct, 0xFFFFFFFF_FF534D42_u64);
        assert_eq!(via_u32, 0xFF534D42_u64);
        assert_ne!(direct, via_u32);
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
                if age >= min_age && std::fs::remove_file(entry.path()).is_ok() {
                    removed += 1;
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

/// Cheap filesystem-only probe used to decide whether the boot-time
/// `init_writer_with_flush` sequence has anything to do. The full sequence
/// holds `.write.lock` across SQLite open + `sweep_stale_jsonl_temps` +
/// `sync::auto_flush` + WAL checkpoint, which can take several seconds on a
/// busy beads dir and starves any concurrent process trying to acquire the
/// lock during PM startup. When the filesystem signals no pending work we
/// can skip the lock entirely; the next mutation will trigger a regular
/// `auto_flush` and correct any state we left untouched.
///
/// Returns true (= safe to skip) iff ALL of:
///   * `beads.db` exists (otherwise init must run to create it),
///   * `issues.jsonl` exists (otherwise we owe an initial export),
///   * no `issues.jsonl.<pid>.tmp` files are present (no sweep work pending),
///   * `issues.jsonl` mtime is at least as recent as `beads.db` and any
///     `beads.db-wal` sidecar (no SQLite→JSONL flush is pending).
///
/// Conservative by design: any mtime/stat error or unknown state returns
/// false so the caller takes the slow lock-held path. False-positives only
/// re-introduce the (correct) original behaviour; false-negatives are the
/// only thing that would skip a legitimate flush, and we have no such path.
fn can_skip_init_flush(beads_dir: &Path) -> bool {
    let db_path = beads_dir.join("beads.db");
    if !db_path.exists() {
        return false;
    }

    let jsonl_path = beads_dir.join("issues.jsonl");
    let Ok(jsonl_meta) = std::fs::metadata(&jsonl_path) else {
        return false;
    };
    let Ok(jsonl_mtime) = jsonl_meta.modified() else {
        return false;
    };

    let entries = match std::fs::read_dir(beads_dir) {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if let Some(name_str) = name.to_str() {
            if is_jsonl_temp_file(name_str) {
                return false;
            }
        }
    }

    let Ok(db_meta) = std::fs::metadata(&db_path) else {
        return false;
    };
    if db_meta.modified().ok().is_some_and(|m| m > jsonl_mtime) {
        return false;
    }

    let wal_path = beads_dir.join("beads.db-wal");
    if let Ok(wal_meta) = std::fs::metadata(&wal_path) {
        if wal_meta.modified().ok().is_some_and(|m| m > jsonl_mtime) {
            return false;
        }
    }

    true
}

/// Initialize the writer-side storage state and flush stale SQLite changes to
/// JSONL while holding `.write.lock` for the full boot-time sequence.
///
/// Fast path: `can_skip_init_flush` (lock-free, filesystem-only) returns true
/// for a quiet, freshly-exported beads dir. In that common case we never
/// acquire the write lock — startup is O(milliseconds) instead of waiting up
/// to `lock_timeout_ms` for whatever process is doing the actual flush work.
pub(crate) fn init_writer_with_flush(
    beads_dir: &Path,
    lock_timeout_ms: u64,
    stale_tmp_min_age: Duration,
) -> anyhow::Result<()> {
    if can_skip_init_flush(beads_dir) {
        tracing::debug!(
            beads_dir = %beads_dir.display(),
            "init_writer_with_flush: fast path — no pending flush, skipping write lock"
        );
        return Ok(());
    }

    let _guard = write_lock::blocking_write_lock_with_timeout(beads_dir, Some(lock_timeout_ms))?;
    let db_path = beads_dir.join("beads.db");
    pre_open_quick_check(&db_path)?;
    let mut storage = SqliteStorage::open_with_timeout(&db_path, Some(lock_timeout_ms))?;
    let _ = sweep_stale_jsonl_temps(beads_dir, stale_tmp_min_age);
    let result = sync::auto_flush(&mut storage, beads_dir);
    drop(storage);
    wal_checkpoint::checkpoint_wal_truncate_best_effort(&db_path);
    result?;
    Ok(())
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn init_writer_with_flush_runs_clean_on_fresh_dir() {
        let dir = TempDir::new().unwrap();
        init_writer_with_flush(dir.path(), 5_000, Duration::ZERO)
            .expect("fresh dir init should succeed");
    }
}

#[cfg(test)]
mod can_skip_init_flush_tests {
    use super::*;
    use filetime::{set_file_mtime, FileTime};
    use std::fs;
    use std::time::SystemTime;
    use tempfile::TempDir;

    fn touch(path: &Path) {
        fs::write(path, b"").unwrap();
    }

    fn set_mtime(path: &Path, t: SystemTime) {
        set_file_mtime(path, FileTime::from_system_time(t)).unwrap();
    }

    #[test]
    fn returns_false_when_db_missing() {
        let dir = TempDir::new().unwrap();
        // jsonl exists but db does not — must take slow path so init can create db
        touch(&dir.path().join("issues.jsonl"));
        assert!(!can_skip_init_flush(dir.path()));
    }

    #[test]
    fn returns_false_when_jsonl_missing() {
        let dir = TempDir::new().unwrap();
        touch(&dir.path().join("beads.db"));
        assert!(!can_skip_init_flush(dir.path()));
    }

    #[test]
    fn returns_false_when_stale_tmp_present() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("beads.db");
        let jsonl = dir.path().join("issues.jsonl");
        touch(&db);
        touch(&jsonl);
        let now = SystemTime::now();
        set_mtime(&db, now);
        set_mtime(&jsonl, now);
        // A pending sweep target should force the slow path so it gets removed
        touch(&dir.path().join("issues.jsonl.12345.tmp"));
        assert!(!can_skip_init_flush(dir.path()));
    }

    #[test]
    fn returns_false_when_db_newer_than_jsonl() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("beads.db");
        let jsonl = dir.path().join("issues.jsonl");
        touch(&jsonl);
        touch(&db);
        let old = SystemTime::now() - Duration::from_secs(60);
        let new = SystemTime::now();
        set_mtime(&jsonl, old);
        set_mtime(&db, new);
        assert!(!can_skip_init_flush(dir.path()));
    }

    #[test]
    fn returns_false_when_wal_newer_than_jsonl() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("beads.db");
        let wal = dir.path().join("beads.db-wal");
        let jsonl = dir.path().join("issues.jsonl");
        touch(&db);
        touch(&jsonl);
        touch(&wal);
        let old = SystemTime::now() - Duration::from_secs(60);
        let new = SystemTime::now();
        set_mtime(&db, old);
        set_mtime(&jsonl, old);
        set_mtime(&wal, new);
        assert!(!can_skip_init_flush(dir.path()));
    }

    #[test]
    fn returns_true_when_quiet_and_jsonl_at_least_as_fresh_as_db() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("beads.db");
        let jsonl = dir.path().join("issues.jsonl");
        touch(&db);
        touch(&jsonl);
        let old = SystemTime::now() - Duration::from_secs(60);
        let new = SystemTime::now();
        set_mtime(&db, old);
        set_mtime(&jsonl, new);
        assert!(can_skip_init_flush(dir.path()));
    }

    #[test]
    fn returns_true_when_no_wal_and_jsonl_at_least_as_fresh_as_db() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("beads.db");
        let jsonl = dir.path().join("issues.jsonl");
        touch(&db);
        touch(&jsonl);
        let t = SystemTime::now() - Duration::from_secs(60);
        set_mtime(&db, t);
        set_mtime(&jsonl, t);
        assert!(can_skip_init_flush(dir.path()));
    }
}
