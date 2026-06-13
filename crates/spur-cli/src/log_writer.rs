//! Date-aware basepath helper for spur.log rotation.
//!
//! `file-rotate` puts the active file at `<basepath>` and rotated chunks
//! at `<basepath>.0[.gz]`, `<basepath>.1[.gz]`, etc. We choose the basepath
//! per-process as `spur.log.YYYY-MM-DD-<pid>` so the active file still matches
//! the existing `spur.log.YYYY-MM-DD*` runbook glob, and rotated chunks
//! become `spur.log.YYYY-MM-DD-<pid>.0.gz`, `.1.gz`, etc. The PID keeps
//! separate `file-rotate` instances from sharing a basepath; otherwise their
//! in-memory suffix maps can miss chunks created by another process and panic
//! the `tracing-appender` worker on rotation.
//!
//! Mid-session date rollover is out of scope (sessions are short relative
//! to a day; SIGUSR1 rebuild is a future enhancement).

use chrono::Utc;
use file_rotate::{compression::Compression, suffix::AppendCount, ContentLimit, FileRotate};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

/// Compute today's process-unique basepath under the given log dir.
///
/// The PID avoids cross-process `file-rotate` collisions where two writers
/// share a basepath, one creates a rotated destination unknown to the other's
/// in-memory suffix map, and the `tracing-appender` worker panics on rotation.
pub fn today_basepath(log_dir: &Path) -> PathBuf {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let pid = std::process::id();
    log_dir.join(format!("spur.log.{today}-{pid}"))
}

/// Build the `FileRotate` instance configured per `[log]` section defaults.
pub fn build_rotator(
    log_dir: &Path,
    max_file_bytes: u64,
    max_files: usize,
) -> FileRotate<AppendCount> {
    let basepath = today_basepath(log_dir);
    // file-rotate 0.8 requires read+create+append on the OpenOptions when
    // provided. Unix mode 0o600 is set via OpenOptionsExt::mode.
    let mut open_opts = OpenOptions::new();
    open_opts.read(true).create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open_opts.mode(0o600);
    }
    FileRotate::new(
        basepath,
        AppendCount::new(max_files),
        ContentLimit::Bytes(max_file_bytes as usize),
        Compression::OnRotate(0),
        Some(open_opts),
    )
}

#[cfg(test)]
mod tests {
    use super::today_basepath;
    use chrono::Utc;
    use std::path::Path;

    #[test]
    fn today_basepath_preserves_date_prefix_and_includes_process_id() {
        let path = today_basepath(Path::new("/tmp/spur-logs"));
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("utf-8 file name");
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let pid = std::process::id().to_string();

        assert!(
            file_name.starts_with(&format!("spur.log.{today}")),
            "file name {file_name:?} should preserve spur.log.<date> prefix",
        );
        assert!(
            file_name.contains(&pid),
            "file name {file_name:?} should include process id {pid}",
        );
    }
}
