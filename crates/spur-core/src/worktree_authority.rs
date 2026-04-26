//! Lease-aware worktree garbage collection.
//!
//! See `docs/superpowers/specs/2026-04-26-worktree-authority-design.md`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use spur_acp::{session_liveness::SelfHeldSet, BrainSessionId};

#[derive(Debug, Clone)]
pub struct AuthorityConfig {
    pub sweep_interval: Duration,
    pub quarantine_grace: Duration,
    pub fs_unsafe_skip: bool,
}

impl Default for AuthorityConfig {
    fn default() -> Self {
        Self {
            sweep_interval: Duration::from_secs(15 * 60),
            quarantine_grace: Duration::from_secs(30),
            fs_unsafe_skip: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub probed: usize,
    pub swept: usize,
    pub skipped_self: usize,
    pub skipped_live: usize,
    pub skipped_quarantine: usize,
    pub skipped_unknown_owner: usize,
    pub skipped_fs_unsafe: usize,
    pub remove_failures: usize,
}

#[derive(Debug)]
pub enum AuthorityError {
    Io(std::io::Error),
    Git(String),
}

impl std::fmt::Display for AuthorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Git(s) => write!(f, "git: {s}"),
        }
    }
}

impl std::error::Error for AuthorityError {}

impl From<std::io::Error> for AuthorityError {
    fn from(e: std::io::Error) -> Self {
        AuthorityError::Io(e)
    }
}

pub struct WorktreeAuthority {
    #[allow(dead_code)] // wired up in Task 15 (sweep_once)
    repo_root: Arc<PathBuf>,
    #[allow(dead_code)] // wired up in Task 15 (sweep_once)
    self_held: SelfHeldSet,
    config: AuthorityConfig,
    #[allow(dead_code)] // wired up in Task 15 (sweep_once)
    last_seen_alive: tokio::sync::Mutex<HashMap<BrainSessionId, Instant>>,
    #[allow(dead_code)] // wired up in Task 15 (sweep_once) for SweepReport telemetry
    funnel: crate::event_funnel::FunnelHandle,
}

impl WorktreeAuthority {
    pub fn new(
        repo_root: PathBuf,
        self_held: SelfHeldSet,
        funnel: crate::event_funnel::FunnelHandle,
        config: AuthorityConfig,
    ) -> Self {
        Self {
            repo_root: Arc::new(repo_root),
            self_held,
            config,
            last_seen_alive: tokio::sync::Mutex::new(HashMap::new()),
            funnel,
        }
    }

    pub fn config(&self) -> &AuthorityConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_funnel;

    #[test]
    fn default_config_has_sane_values() {
        let c = AuthorityConfig::default();
        assert_eq!(c.sweep_interval, Duration::from_secs(900));
        assert_eq!(c.quarantine_grace, Duration::from_secs(30));
        assert!(c.fs_unsafe_skip);
    }

    #[test]
    fn sweep_report_default_is_all_zero() {
        let r = SweepReport::default();
        assert_eq!(r.probed, 0);
        assert_eq!(r.swept, 0);
        assert_eq!(r.skipped_self, 0);
        assert_eq!(r.skipped_live, 0);
        assert_eq!(r.skipped_quarantine, 0);
        assert_eq!(r.skipped_unknown_owner, 0);
        assert_eq!(r.skipped_fs_unsafe, 0);
        assert_eq!(r.remove_failures, 0);
    }

    #[test]
    fn authority_error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "boom");
        let auth_err: AuthorityError = io_err.into();
        match auth_err {
            AuthorityError::Io(_) => {}
            AuthorityError::Git(s) => panic!("expected Io variant, got Git({s})"),
        }
    }

    #[tokio::test]
    async fn new_constructs_with_funnel() {
        let (funnel, _rx) = event_funnel::test_channel();
        let authority = WorktreeAuthority::new(
            std::path::PathBuf::from("/tmp/test-repo"),
            SelfHeldSet::new(),
            funnel,
            AuthorityConfig::default(),
        );
        assert_eq!(authority.config().quarantine_grace, Duration::from_secs(30));
    }
}
