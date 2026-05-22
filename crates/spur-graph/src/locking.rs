//! Shared file-locking primitives used by spur-graph and downstream crates.
//!
//! Centralizes the cross-platform `fs2::FileExt::try_lock_exclusive` retry
//! discipline so flock semantics stay consistent across the workspace
//! (relevant to the open flock-leak RCA at
//! `docs/rca/2026-05-18-flock-leak-pid14282/`).

use std::fs::File;
use std::io;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fs2::FileExt;

/// How long to sleep between flock retry attempts. Caller-visible default;
/// individual call sites can pass their own deadline via `timeout`.
pub const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);

/// Try to take an exclusive flock on `file`, retrying until `timeout` elapses.
///
/// Returns `Ok(true)` on success, `Ok(false)` if the deadline expired while the
/// lock was still contended, and `Err(_)` for any other I/O error.
pub fn try_lock_exclusive_with_timeout(file: &File, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(true),
            Err(err) if is_lock_contended(&err) => {
                if Instant::now() >= deadline {
                    return Ok(false);
                }
                thread::sleep(
                    LOCK_RETRY_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            Err(err) => return Err(err).context("failed to acquire file lock"),
        }
    }
}

/// True if `err` represents a contended (non-fatal) flock attempt.
///
/// macOS and Linux disagree on the underlying errno for non-blocking flock
/// contention; this predicate normalizes them.
pub fn is_lock_contended(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::AlreadyExists | io::ErrorKind::PermissionDenied
    )
}
