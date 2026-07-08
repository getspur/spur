//! Git identity of the working tree (repo directory name, active branch,
//! short commit hash) for display in the status bar.
//!
//! Resolution shells out to `git` so worktrees, packed refs, and detached
//! HEAD all behave; `GitInfoCache` bounds that cost to a slow refresh
//! cadence so render paths never fork a process per frame.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Snapshot of the working tree's git identity for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitInfo {
    /// Basename of the repository toplevel directory.
    pub repo_name: String,
    /// Active branch; `None` on a detached HEAD.
    pub branch: Option<String>,
    /// Abbreviated HEAD commit hash; `None` before the first commit.
    pub short_hash: Option<String>,
}

impl GitInfo {
    /// Resolve the git identity of `dir`, or `None` when `dir` is not
    /// inside a git work tree (or `git` is unavailable).
    pub fn resolve(dir: &Path) -> Option<Self> {
        let toplevel = git_stdout(dir, &["rev-parse", "--show-toplevel"])?;
        let repo_name = Path::new(&toplevel)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| toplevel.clone());
        // symbolic-ref exits non-zero on a detached HEAD; that is the signal.
        let branch = git_stdout(dir, &["symbolic-ref", "--short", "-q", "HEAD"]);
        // rev-parse HEAD exits non-zero on an unborn branch (no commits yet).
        let short_hash = git_stdout(dir, &["rev-parse", "--short", "HEAD"]);
        Some(GitInfo {
            repo_name,
            branch,
            short_hash,
        })
    }
}

fn git_stdout(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout)
    }
}

/// Commits land frequently during agent sessions, so the snapshot re-resolves
/// on a slow cadence instead of once at startup, but never per frame.
const REFRESH_INTERVAL: Duration = Duration::from_secs(15);

/// Interval-gated cache around [`GitInfo::resolve`], safe to poke from a
/// render path: `refresh_if_stale` forks `git` at most once per interval.
#[derive(Debug)]
pub struct GitInfoCache {
    dir: PathBuf,
    interval: Duration,
    info: Option<GitInfo>,
    last_refresh: Option<Instant>,
}

impl GitInfoCache {
    pub fn new(dir: PathBuf) -> Self {
        Self::with_interval(dir, REFRESH_INTERVAL)
    }

    /// Cache with an explicit refresh interval (tests pass `Duration::ZERO`).
    pub fn with_interval(dir: PathBuf, interval: Duration) -> Self {
        Self {
            dir,
            interval,
            info: None,
            last_refresh: None,
        }
    }

    /// Re-resolve when never resolved or the interval has elapsed.
    pub fn refresh_if_stale(&mut self) {
        let fresh = self
            .last_refresh
            .is_some_and(|at| at.elapsed() < self.interval);
        if fresh {
            return;
        }
        self.info = GitInfo::resolve(&self.dir);
        self.last_refresh = Some(Instant::now());
    }

    /// Last resolved snapshot; `None` until the first refresh or when the
    /// directory is not a git work tree.
    pub fn current(&self) -> Option<&GitInfo> {
        self.info.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    fn run_git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git invocation failed");
        assert!(
            out.status.success(),
            "git {args:?} failed: stderr={} stdout={}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
    }

    fn init_repo_with_commit(repo: &Path) {
        run_git(repo, &["init", "-b", "main"]);
        run_git(repo, &["config", "user.name", "t"]);
        run_git(repo, &["config", "user.email", "t@example.com"]);
        run_git(repo, &["config", "commit.gpgsign", "false"]);
        run_git(repo, &["commit", "--allow-empty", "-m", "init"]);
    }

    #[test]
    fn resolve_returns_none_outside_git_repo() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(GitInfo::resolve(tmp.path()), None);
    }

    #[test]
    fn resolve_reports_repo_name_branch_and_short_hash() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_commit(tmp.path());

        let info = GitInfo::resolve(tmp.path()).expect("repo resolves");
        let expected_name = tmp.path().file_name().unwrap().to_string_lossy();
        assert_eq!(info.repo_name, expected_name);
        assert_eq!(info.branch.as_deref(), Some("main"));
        let hash = info.short_hash.expect("commit present");
        assert!(
            hash.len() >= 7 && hash.chars().all(|c| c.is_ascii_hexdigit()),
            "expected short hex hash, got: {hash}"
        );
    }

    #[test]
    fn resolve_reports_detached_head_without_branch() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_commit(tmp.path());
        run_git(tmp.path(), &["checkout", "--detach"]);

        let info = GitInfo::resolve(tmp.path()).expect("repo resolves");
        assert_eq!(info.branch, None);
        assert!(info.short_hash.is_some());
    }

    #[test]
    fn resolve_unborn_branch_has_branch_but_no_hash() {
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-b", "main"]);

        let info = GitInfo::resolve(tmp.path()).expect("repo resolves");
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert_eq!(info.short_hash, None);
    }

    #[test]
    fn cache_is_empty_until_refreshed() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_commit(tmp.path());

        let mut cache = GitInfoCache::with_interval(tmp.path().to_path_buf(), Duration::ZERO);
        assert!(cache.current().is_none());
        cache.refresh_if_stale();
        assert!(cache.current().is_some());
    }

    #[test]
    fn cache_refresh_picks_up_new_commits_when_interval_elapsed() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_commit(tmp.path());

        let mut cache = GitInfoCache::with_interval(tmp.path().to_path_buf(), Duration::ZERO);
        cache.refresh_if_stale();
        let first = cache.current().unwrap().short_hash.clone();
        run_git(tmp.path(), &["commit", "--allow-empty", "-m", "two"]);
        cache.refresh_if_stale();
        let second = cache.current().unwrap().short_hash.clone();
        assert_ne!(first, second, "expected refresh to observe the new commit");
    }

    #[test]
    fn cache_does_not_reresolve_within_interval() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo_with_commit(tmp.path());

        let mut cache =
            GitInfoCache::with_interval(tmp.path().to_path_buf(), Duration::from_secs(3600));
        cache.refresh_if_stale();
        let first = cache.current().unwrap().short_hash.clone();
        run_git(tmp.path(), &["commit", "--allow-empty", "-m", "two"]);
        cache.refresh_if_stale();
        let second = cache.current().unwrap().short_hash.clone();
        assert_eq!(first, second, "expected cached value inside the interval");
    }
}
