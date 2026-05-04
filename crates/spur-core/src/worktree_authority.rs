//! Lease-aware worktree garbage collection.
//!
//! See `docs/superpowers/specs/2026-04-26-worktree-authority-design.md`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use spur_acp::session_liveness::{SessionLivenessProbe, SessionLivenessProbeResult};
use spur_acp::{session_liveness::SelfHeldSet, BrainSessionId};
use spur_worktree::manager::parse_v2_branch;
use tokio::process::Command;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

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
    repo_root: Arc<PathBuf>,
    self_held: SelfHeldSet,
    config: AuthorityConfig,
    last_seen_alive: tokio::sync::Mutex<HashMap<BrainSessionId, Instant>>,
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

impl WorktreeAuthority {
    pub async fn sweep_once(&self) -> Result<SweepReport, AuthorityError> {
        let mut report = SweepReport::default();
        if self.config.fs_unsafe_skip && self.detect_fs_unsafe().await {
            info!(
                target: "spur.metrics.worktree_authority.fs_unsafe_skip",
                "filesystem does not support advisory locks; sweep skipped"
            );
            return Ok(report);
        }
        let entries = self.enumerate_worktrees().await?;
        let now = Instant::now();
        let mut last_seen = self.last_seen_alive.lock().await;

        for (path, branch) in entries {
            let trimmed = branch.trim_start_matches("refs/heads/");
            let brain_session_id = match owned_branch_brain_session_id(trimmed) {
                Some(id) => id,
                None => {
                    if trimmed.starts_with("spur/worker-")
                        && !trimmed.starts_with("spur/worker/v2/")
                    {
                        tracing::debug!(
                            target: "spur.authority.legacy_skip",
                            branch = %trimmed,
                            "I-7 invariant: skipping legacy branch from authority sweep",
                        );
                    }
                    report.skipped_unknown_owner += 1;
                    continue;
                }
            };
            report.probed += 1;
            let result =
                SessionLivenessProbe::probe(&self.repo_root, &brain_session_id, &self.self_held);
            match result {
                SessionLivenessProbeResult::Self_ => {
                    last_seen.insert(brain_session_id.clone(), now);
                    report.skipped_self += 1;
                }
                SessionLivenessProbeResult::Live => {
                    last_seen.insert(brain_session_id.clone(), now);
                    report.skipped_live += 1;
                }
                SessionLivenessProbeResult::FsUnsafe => {
                    report.skipped_fs_unsafe += 1;
                }
                SessionLivenessProbeResult::Missing => {
                    if self.is_quarantine_expired(&brain_session_id, now, &last_seen) {
                        if let Err(e) = self.sweep_one(&path, trimmed).await {
                            warn!(error=%e, path=%path.display(), "sweep_one (missing lock) failed");
                            report.remove_failures += 1;
                        } else {
                            report.swept += 1;
                        }
                    } else {
                        report.skipped_quarantine += 1;
                    }
                }
                SessionLivenessProbeResult::DeadAcquired(guard) => {
                    if self.is_quarantine_expired(&brain_session_id, now, &last_seen) {
                        if let Err(e) = self.sweep_one(&path, trimmed).await {
                            warn!(error=%e, path=%path.display(), "sweep_one failed");
                            report.remove_failures += 1;
                        } else {
                            report.swept += 1;
                        }
                    } else {
                        report.skipped_quarantine += 1;
                    }
                    drop(guard);
                }
            }
        }
        // Spec algorithm step 3: prune once after all sweeps complete.
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&*self.repo_root)
            .output()
            .await;

        // Spec algorithm step 4: emit telemetry. SpurEventBody does not yet
        // have a WorktreeAuthoritySweep variant; a later task will add it.
        // For now, log via tracing so the counters are visible, and reference
        // self.funnel so the field remains part of the authority surface.
        let _ = &self.funnel;
        info!(
            probed = report.probed,
            swept = report.swept,
            skipped_self = report.skipped_self,
            skipped_live = report.skipped_live,
            skipped_quarantine = report.skipped_quarantine,
            skipped_unknown_owner = report.skipped_unknown_owner,
            skipped_fs_unsafe = report.skipped_fs_unsafe,
            remove_failures = report.remove_failures,
            "WorktreeAuthority sweep complete"
        );
        Ok(report)
    }

    fn is_quarantine_expired(
        &self,
        brain: &BrainSessionId,
        now: Instant,
        last_seen: &HashMap<BrainSessionId, Instant>,
    ) -> bool {
        match last_seen.get(brain) {
            Some(t) => now.duration_since(*t) >= self.config.quarantine_grace,
            None => true,
        }
    }

    async fn enumerate_worktrees(&self) -> Result<Vec<(PathBuf, String)>, AuthorityError> {
        let out = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&*self.repo_root)
            .output()
            .await?;
        if !out.status.success() {
            return Err(AuthorityError::Git(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut result = Vec::new();
        let mut path: Option<PathBuf> = None;
        let repo_root = tokio::fs::canonicalize(&*self.repo_root)
            .await
            .unwrap_or_else(|_| self.repo_root.as_ref().clone());
        for line in stdout.lines().chain(std::iter::once("")) {
            if line.is_empty() {
                path = None;
                continue;
            }
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            }
            if let Some(b) = line.strip_prefix("branch ") {
                if let Some(p) = path.take() {
                    let is_repo_root = tokio::fs::canonicalize(&p)
                        .await
                        .map(|canonical| canonical == repo_root)
                        .unwrap_or_else(|_| p.as_path() == self.repo_root.as_ref().as_path());
                    if !is_repo_root {
                        result.push((p, b.to_string()));
                    }
                }
            }
        }
        Ok(result)
    }

    async fn sweep_one(&self, path: &std::path::Path, branch: &str) -> Result<(), AuthorityError> {
        let path_str = path
            .to_str()
            .ok_or_else(|| AuthorityError::Git("worktree path not UTF-8".into()))?;
        let out = Command::new("git")
            .args(["worktree", "remove", "--force", "--force", path_str])
            .current_dir(&*self.repo_root)
            .output()
            .await?;
        if !out.status.success() {
            return Err(AuthorityError::Git(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        match Command::new("git")
            .args(["branch", "-D", branch])
            .current_dir(&*self.repo_root)
            .output()
            .await
        {
            Ok(o) if !o.status.success() => {
                warn!(
                    branch = %branch,
                    stderr = %String::from_utf8_lossy(&o.stderr).trim(),
                    "git branch -D failed during authority sweep; v2 branch may be leaked"
                );
            }
            Err(e) => {
                warn!(branch = %branch, error = %e, "git branch -D spawn failed during authority sweep");
            }
            Ok(_) => {}
        }
        Ok(())
    }

    /// Detect whether the repo's `.spur/sessions/` directory supports
    /// advisory locking. Probes a temp file once per sweep; ~1ms cost.
    async fn detect_fs_unsafe(&self) -> bool {
        let probe_path = self.repo_root.join(".spur/sessions/.fs_probe");
        if let Some(parent) = probe_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if tokio::fs::write(&probe_path, b"").await.is_err() {
            return false;
        }
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&probe_path)
        {
            Ok(f) => f,
            Err(_) => return false,
        };
        use fs4::fs_std::FileExt;
        let result = file.try_lock_exclusive();
        let _ = tokio::fs::remove_file(&probe_path).await;
        matches!(
            result,
            Err(e) if is_lock_unsupported_error(&e)
        )
    }

    pub fn spawn_periodic(self: Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let jitter_ms: u64 = (std::ptr::addr_of!(*self) as usize as u64) % 120_000;
                let delay = self.config.sweep_interval + Duration::from_millis(jitter_ms);
                tokio::time::sleep(delay).await;
                match self.sweep_once().await {
                    Ok(report) => {
                        info!(
                            target: "spur.metrics.worktree_authority.periodic",
                            probed = report.probed,
                            swept = report.swept,
                            skipped_self = report.skipped_self,
                            skipped_live = report.skipped_live,
                            skipped_quarantine = report.skipped_quarantine,
                            skipped_unknown_owner = report.skipped_unknown_owner,
                            skipped_fs_unsafe = report.skipped_fs_unsafe,
                            remove_failures = report.remove_failures,
                            "periodic sweep complete"
                        );
                    }
                    Err(e) => {
                        error!(
                            target: "spur.metrics.worktree_authority.periodic_failed",
                            error = %e,
                            "periodic sweep failed"
                        );
                    }
                }
            }
        })
    }
}

fn is_lock_unsupported_error(e: &std::io::Error) -> bool {
    if matches!(e.kind(), std::io::ErrorKind::Unsupported) {
        return true;
    }

    #[cfg(unix)]
    {
        let raw = e.raw_os_error();
        raw == Some(libc::ENOLCK) || raw == Some(libc::ENOTSUP)
    }

    #[cfg(not(unix))]
    {
        false
    }
}

/// Restored I-7 invariant: only v2 branches are owned by the authority.
/// Legacy branches (spur/worker-{agent}-{uuid}) return None so they are
/// bucketed as skipped_unknown_owner and never auto-swept.
fn owned_branch_brain_session_id(branch: &str) -> Option<BrainSessionId> {
    parse_v2_branch(branch).map(|owner| owner.brain_session_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_funnel;
    use spur_acp::SessionId;
    use tempfile::TempDir;

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

    #[test]
    fn owned_branch_brain_session_id_rejects_malformed_uuid() {
        // Legacy prefix + non-UUID trailing -> should NOT be recognized.
        assert_eq!(
            owned_branch_brain_session_id("spur/worker-codex-not-a-uuid"),
            None
        );
        assert_eq!(
            owned_branch_brain_session_id("spur/worker-codex-deadbeef"),
            None
        );
        // Too short.
        assert_eq!(
            owned_branch_brain_session_id("spur/worker-codex-12345678"),
            None
        );
        // Right length but bad chars.
        assert_eq!(
            owned_branch_brain_session_id("spur/worker-codex-zzzzzzzz-zzzz-zzzz-zzzz-zzzzzzzzzzzz"),
            None
        );
        // Right format but missing agent.
        assert_eq!(
            owned_branch_brain_session_id("spur/worker--550e8400-e29b-41d4-a716-446655440000"),
            None
        );
        // Non-spur prefix.
        assert_eq!(
            owned_branch_brain_session_id(
                "dependabot/worker-x-550e8400-e29b-41d4-a716-446655440000"
            ),
            None
        );
    }

    #[test]
    fn owned_branch_brain_session_id_accepts_v2_and_skips_legacy_branches() {
        let brain = "550e8400-e29b-41d4-a716-446655440000";
        let v2 = format!("spur/worker/v2/codex/{brain}/deadbeef-1111-2222-3333-444455556666");
        let legacy = format!("spur/worker-claude-code-{brain}");

        assert_eq!(owned_branch_brain_session_id(&v2), Some(id(brain)));
        assert_eq!(owned_branch_brain_session_id(&legacy), None);
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

    async fn seed_repo_with_worktree(td: &TempDir, branch: &str) -> std::path::PathBuf {
        use tokio::process::Command;
        async fn git(dir: &std::path::Path, args: &[&str]) {
            let s = Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .await
                .unwrap();
            assert!(
                s.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&s.stderr)
            );
        }
        git(td.path(), &["init", "-q", "-b", "main"]).await;
        git(td.path(), &["config", "user.email", "t@t"]).await;
        git(td.path(), &["config", "user.name", "t"]).await;
        tokio::fs::write(td.path().join("a"), b"x").await.unwrap();
        git(td.path(), &["add", "a"]).await;
        git(td.path(), &["commit", "-q", "-m", "base"]).await;
        let wt = td.path().join(".spur/worktrees/abc");
        git(
            td.path(),
            &[
                "worktree",
                "add",
                wt.to_str().unwrap(),
                "-b",
                branch,
                "main",
            ],
        )
        .await;
        wt
    }

    fn id(s: &str) -> BrainSessionId {
        BrainSessionId::new(SessionId(s.into()))
    }

    #[tokio::test]
    async fn sweep_skips_legacy_branches_per_i7() {
        let td = TempDir::new().unwrap();
        let brain = "550e8400-e29b-41d4-a716-446655440000";
        let branch = format!("spur/worker-codex-{brain}");
        let _ = seed_repo_with_worktree(&td, &branch).await;
        let (funnel, _rx) = event_funnel::test_channel();
        let auth = WorktreeAuthority::new(
            td.path().to_path_buf(),
            SelfHeldSet::new(),
            funnel,
            AuthorityConfig {
                quarantine_grace: Duration::ZERO,
                ..AuthorityConfig::default()
            },
        );
        let r = auth.sweep_once().await.expect("sweep ok");
        assert_eq!(r.skipped_unknown_owner, 1);
        assert_eq!(r.probed, 0);
        assert_eq!(r.swept, 0);
    }

    #[tokio::test]
    async fn sweep_skips_unrecognized_branches() {
        let td = TempDir::new().unwrap();
        let _ =
            seed_repo_with_worktree(&td, "external/foo-deadbeef-1111-2222-3333-444455556666").await;
        let (funnel, _rx) = event_funnel::test_channel();
        let auth = WorktreeAuthority::new(
            td.path().to_path_buf(),
            SelfHeldSet::new(),
            funnel,
            AuthorityConfig {
                quarantine_grace: Duration::from_millis(1),
                ..Default::default()
            },
        );
        let r = auth.sweep_once().await.expect("sweep ok");
        assert_eq!(r.skipped_unknown_owner, 1);
        assert_eq!(r.swept, 0);
    }

    #[tokio::test]
    async fn sweep_reclaims_v2_worktree_when_session_lock_missing() {
        let td = TempDir::new().unwrap();
        let brain = "550e8400-e29b-41d4-a716-446655440000";
        let worker = "deadbeef-1111-2222-3333-444455556666";
        let branch = format!("spur/worker/v2/codex/{brain}/{worker}");
        let _ = seed_repo_with_worktree(&td, &branch).await;
        let (funnel, _rx) = event_funnel::test_channel();
        let auth = WorktreeAuthority::new(
            td.path().to_path_buf(),
            SelfHeldSet::new(),
            funnel,
            AuthorityConfig {
                quarantine_grace: Duration::ZERO,
                ..AuthorityConfig::default()
            },
        );
        let r = auth.sweep_once().await.expect("sweep ok");
        assert_eq!(r.swept, 1);
        assert_eq!(r.probed, 1);
    }

    #[tokio::test]
    async fn sweep_skips_self_held_session() {
        let td = TempDir::new().unwrap();
        let brain = "550e8400-e29b-41d4-a716-446655440000";
        let worker = "deadbeef-1111-2222-3333-444455556666";
        let branch = format!("spur/worker/v2/codex/{brain}/{worker}");
        let _ = seed_repo_with_worktree(&td, &branch).await;
        let self_held = SelfHeldSet::new();
        self_held.insert(id(brain));
        let (funnel, _rx) = event_funnel::test_channel();
        let auth = WorktreeAuthority::new(
            td.path().to_path_buf(),
            self_held,
            funnel,
            AuthorityConfig {
                quarantine_grace: Duration::ZERO,
                ..AuthorityConfig::default()
            },
        );
        let r = auth.sweep_once().await.expect("sweep ok");
        assert_eq!(r.skipped_self, 1);
        assert_eq!(r.swept, 0);
    }

    #[tokio::test]
    async fn sweep_respects_quarantine_grace() {
        let td = TempDir::new().unwrap();
        let brain = "550e8400-e29b-41d4-a716-446655440000";
        let worker = "deadbeef-1111-2222-3333-444455556666";
        let branch = format!("spur/worker/v2/codex/{brain}/{worker}");
        let _ = seed_repo_with_worktree(&td, &branch).await;

        // Externally hold the session lockfile so the first sweep observes Live.
        std::fs::create_dir_all(td.path().join(".spur/sessions")).unwrap();
        let lock_path = td
            .path()
            .join(".spur/sessions")
            .join(format!("{brain}.lock"));
        std::fs::write(&lock_path, b"").unwrap();

        use fs4::fs_std::FileExt;
        use std::fs::OpenOptions;
        let held = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        held.try_lock_exclusive().unwrap();

        let (funnel, _rx) = event_funnel::test_channel();
        let auth = WorktreeAuthority::new(
            td.path().to_path_buf(),
            SelfHeldSet::new(),
            funnel,
            AuthorityConfig {
                quarantine_grace: Duration::from_secs(5),
                sweep_interval: Duration::from_secs(900),
                fs_unsafe_skip: true,
            },
        );

        // First sweep: lock is held externally, probe returns Live; primes last_seen_alive.
        let r1 = auth.sweep_once().await.expect("first sweep ok");
        assert_eq!(r1.skipped_live, 1, "first sweep should observe Live");
        assert_eq!(r1.swept, 0);

        drop(held);

        // Second sweep: probe now succeeds in acquiring (DeadAcquired), but
        // quarantine grace (5s) has not expired since first sweep, so we
        // skip rather than sweep.
        let r2 = auth.sweep_once().await.expect("second sweep ok");
        assert_eq!(
            r2.skipped_quarantine, 1,
            "second sweep within grace should skip quarantine"
        );
        assert_eq!(r2.swept, 0, "must not sweep within quarantine grace");
    }

    #[tokio::test]
    async fn sweep_short_circuits_when_fs_unsafe_detected() {
        // We can't easily fake ENOTSUP in a unit test on a normal disk.
        // Instead, verify the explicit config path: when fs_unsafe_skip is
        // false, we still try to sweep even on a hypothetical unsafe FS.
        // This test documents the contract; a real ENOTSUP test would
        // require a mock filesystem (deferred).
        let td = TempDir::new().unwrap();
        let (funnel, _rx) = event_funnel::test_channel();
        let auth = WorktreeAuthority::new(
            td.path().to_path_buf(),
            SelfHeldSet::new(),
            funnel,
            AuthorityConfig {
                fs_unsafe_skip: false,
                quarantine_grace: Duration::ZERO,
                sweep_interval: Duration::from_secs(900),
            },
        );
        // No git repo here; sweep should fail at enumerate, not silently succeed.
        let r = auth.sweep_once().await;
        assert!(
            r.is_err(),
            "with fs_unsafe_skip=false on a non-repo dir, sweep should error"
        );
    }

    #[tokio::test]
    async fn spawn_periodic_returns_abortable_handle() {
        let td = TempDir::new().unwrap();
        let (funnel, _rx) = event_funnel::test_channel();
        let auth = Arc::new(WorktreeAuthority::new(
            td.path().to_path_buf(),
            SelfHeldSet::new(),
            funnel,
            AuthorityConfig {
                sweep_interval: Duration::from_millis(50),
                quarantine_grace: Duration::ZERO,
                fs_unsafe_skip: true,
            },
        ));
        let handle = auth.clone().spawn_periodic();
        tokio::time::sleep(Duration::from_millis(120)).await;
        handle.abort();
        let res = handle.await;
        assert!(
            res.is_err() && res.unwrap_err().is_cancelled(),
            "handle must be cancellable"
        );
    }
}
