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

#[allow(dead_code)] // fields wired up in Task 15 (sweep_once)
pub struct WorktreeAuthority {
    repo_root: Arc<PathBuf>,
    self_held: SelfHeldSet,
    config: AuthorityConfig,
    last_seen_alive: tokio::sync::Mutex<HashMap<BrainSessionId, Instant>>,
}

impl WorktreeAuthority {
    pub fn new(repo_root: PathBuf, self_held: SelfHeldSet, config: AuthorityConfig) -> Self {
        Self {
            repo_root: Arc::new(repo_root),
            self_held,
            config,
            last_seen_alive: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> &AuthorityConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
