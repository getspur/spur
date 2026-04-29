//! Date-aware basepath helper for spur.log rotation.
//!
//! `file-rotate` puts the active file at `<basepath>` and rotated chunks
//! at `<basepath>.0[.gz]`, `<basepath>.1[.gz]`, etc. We choose the basepath
//! per-session as `spur.log.YYYY-MM-DD` so the active file matches the
//! existing `spur.log.YYYY-MM-DD*` runbook glob, and rotated chunks
//! become `spur.log.YYYY-MM-DD.0.gz`, `.1.gz`, etc.
//!
//! Mid-session date rollover is out of scope (sessions are short relative
//! to a day; SIGUSR1 rebuild is a future enhancement).

use chrono::Utc;
use file_rotate::{compression::Compression, suffix::AppendCount, ContentLimit, FileRotate};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

/// Compute today's basepath under the given log dir.
pub fn today_basepath(log_dir: &Path) -> PathBuf {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    log_dir.join(format!("spur.log.{today}"))
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
