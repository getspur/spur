use anyhow::{anyhow, Context, Result};
use spur_acp::SessionId;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::process::Command;
use tracing::debug;

/// Monotonic counter to guarantee unique snapshot branch names under concurrency.
static SNAPSHOT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Manages git worktree lifecycle for concurrent agent isolation.
pub struct WorktreeManager {
    pub repo_root: PathBuf,
    pub active: HashMap<String, WorktreeInfo>,
}

/// Info about an active worktree.
pub struct WorktreeInfo {
    pub session_id: SessionId,
    pub path: PathBuf,
    pub branch: String,
    pub base_commit: String,
    pub agent: String,
    pub created_at: Instant,
}

/// Result of attempting to merge worker changes.
pub enum MergeResult {
    Success,
    Conflict { files: Vec<String> },
}

impl WorktreeManager {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            active: HashMap::new(),
        }
    }

    /// Run a git command with the given args, optionally in a specific directory.
    /// Returns stdout on success, or an error containing stderr on failure.
    async fn run_git(&self, args: &[&str], cwd: Option<&Path>) -> Result<String> {
        let work_dir = cwd.unwrap_or(&self.repo_root);

        debug!(
            command = %format!("git {}", args.join(" ")),
            cwd = %work_dir.display(),
            "running git command"
        );

        let output = Command::new("git")
            .args(args)
            .current_dir(work_dir)
            .output()
            .await
            .context("failed to execute git command")?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Ok(stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(anyhow!(
                "git {} failed (exit {}): {}",
                args.first().unwrap_or(&""),
                output.status.code().unwrap_or(-1),
                stderr,
            ))
        }
    }

    /// Snapshot the brain's current state (including uncommitted changes) onto a
    /// temporary branch. Returns the snapshot branch name.
    ///
    /// Uses git plumbing (`stash create` → `rev-parse ^{tree}` → `commit-tree`)
    /// instead of a temp worktree, making this safe for concurrent invocation
    /// (e.g. parallel plan dispatch).
    pub async fn snapshot_brain_state(&self) -> Result<String> {
        // Only tracked modifications count as dirty — untracked files (??) are
        // ignored because `git stash create` doesn't capture them anyway.
        let status = self.run_git(&["status", "--porcelain"], None).await?;
        let dirty = status.lines().any(|l| !l.starts_with("??"));

        // Unique branch name: timestamp for humans + atomic counter for concurrency.
        let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
        let seq = SNAPSHOT_SEQ.fetch_add(1, Ordering::Relaxed);
        let branch_name = format!("spur/brain-snapshot-{timestamp}-{seq}");

        if dirty {
            // Retry stash create up to 3 times — concurrent calls can hit
            // index.lock contention when multiple plan tasks snapshot in parallel.
            let mut stash_ref = String::new();
            for attempt in 0..3 {
                match self.run_git(&["stash", "create"], None).await {
                    Ok(r) => {
                        stash_ref = r;
                        break;
                    }
                    Err(e) if attempt < 2 => {
                        debug!(attempt, error = %e, "stash create contention, retrying");
                        tokio::time::sleep(std::time::Duration::from_millis(
                            50 * (attempt as u64 + 1),
                        ))
                        .await;
                    }
                    Err(e) => return Err(e).context("failed to create stash after retries"),
                }
            }

            if !stash_ref.is_empty() {
                // Extract the tree from the stash commit (captures dirty state).
                let tree_spec = format!("{stash_ref}^{{tree}}");
                let tree = self
                    .run_git(&["rev-parse", &tree_spec], None)
                    .await
                    .context("failed to extract tree from stash")?;

                // Create a clean single-parent commit with the dirty-state tree.
                let commit = self
                    .run_git(
                        &[
                            "commit-tree",
                            &tree,
                            "-p",
                            "HEAD",
                            "-m",
                            "spur: brain snapshot",
                        ],
                        None,
                    )
                    .await
                    .context("failed to create snapshot commit")?;

                // Point the snapshot branch at this commit.
                self.run_git(&["branch", &branch_name, &commit], None)
                    .await
                    .context("failed to create snapshot branch")?;
            } else {
                // stash create returned empty despite dirty status — branch at HEAD.
                self.run_git(&["branch", &branch_name, "HEAD"], None)
                    .await
                    .context("failed to create snapshot branch")?;
            }
        } else {
            self.run_git(&["branch", &branch_name, "HEAD"], None)
                .await
                .context("failed to create snapshot branch")?;
        }

        Ok(branch_name)
    }

    /// Create a new worktree for an agent session, branching from `base_branch`.
    pub async fn create_worktree(
        &mut self,
        session_id: &SessionId,
        agent: &str,
        base_branch: &str,
    ) -> Result<WorktreeInfo> {
        let session_str = session_id.to_string();
        let worktree_path = self.repo_root.join(".spur/worktrees").join(&session_str);
        let branch_name = format!("spur/worker-{agent}-{session_str}");

        let worktree_path_str = worktree_path
            .to_str()
            .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?;

        // Resolve the base commit before creating the worktree so we can record it.
        let base_commit = self
            .run_git(&["rev-parse", base_branch], None)
            .await
            .with_context(|| format!("failed to resolve base branch '{base_branch}'"))?;

        self.run_git(
            &[
                "worktree",
                "add",
                worktree_path_str,
                "-b",
                &branch_name,
                base_branch,
            ],
            None,
        )
        .await
        .with_context(|| format!("failed to create worktree at {worktree_path_str}"))?;

        let info = WorktreeInfo {
            session_id: session_id.clone(),
            path: worktree_path,
            branch: branch_name,
            base_commit,
            agent: agent.to_string(),
            created_at: Instant::now(),
        };

        self.active.insert(session_str, info);

        // Return a reference-safe copy.
        let stored = self.active.get(&session_id.to_string()).unwrap();
        Ok(WorktreeInfo {
            session_id: stored.session_id.clone(),
            path: stored.path.clone(),
            branch: stored.branch.clone(),
            base_commit: stored.base_commit.clone(),
            agent: stored.agent.clone(),
            created_at: stored.created_at,
        })
    }

    /// Collect the diff of the worker's task. Returns:
    /// - `(Some(diff), "HEAD")` if the worker left uncommitted changes.
    /// - `(Some(diff), "base_commit..HEAD")` if the worker already committed
    ///   (HEAD-relative diff is empty, but base..HEAD has content).
    /// - `(None, "base_commit..HEAD")` if the worker produced no changes at
    ///   all. Caller distinguishes "no changes" from "collection failed" via
    ///   the returned basis.
    pub async fn collect_diff(
        &self,
        session_id: &SessionId,
    ) -> Result<(Option<String>, &'static str)> {
        let info = self.lookup(session_id)?;

        // First: HEAD-relative (uncommitted changes).
        let head_diff = self
            .run_git(&["diff", "HEAD"], Some(&info.path))
            .await
            .context("failed to collect HEAD-relative diff")?;

        if !head_diff.is_empty() {
            return Ok((Some(head_diff), "HEAD"));
        }

        // Fallback: base_commit..HEAD (worker self-committed).
        let base_spec = format!("{}..HEAD", info.base_commit);
        let base_diff = self
            .run_git(&["diff", &base_spec], Some(&info.path))
            .await
            .context("failed to collect base..HEAD diff")?;

        if base_diff.is_empty() {
            Ok((None, "base_commit..HEAD"))
        } else {
            Ok((Some(base_diff), "base_commit..HEAD"))
        }
    }

    /// Stage and commit all changes in a worker's worktree.
    /// No-op if there is nothing to commit.
    pub async fn commit_worker_changes(&self, session_id: &SessionId, message: &str) -> Result<()> {
        let info = self.lookup(session_id)?;

        self.run_git(&["add", "-A"], Some(&info.path))
            .await
            .context("failed to stage changes")?;

        // Check if there is anything staged to commit.
        let status = self
            .run_git(&["status", "--porcelain"], Some(&info.path))
            .await?;

        if status.is_empty() {
            debug!(session = %session_id, "nothing to commit");
            return Ok(());
        }

        self.run_git(&["commit", "-m", message], Some(&info.path))
            .await
            .context("failed to commit worker changes")?;

        Ok(())
    }

    /// Persist `output_text` as a side-channel artifact for `session_id`.
    /// Thin wrapper over `crate::artifact::persist` that resolves the
    /// worktree path from `self.active`, keeping the call site in the
    /// orchestrator simple.
    ///
    /// Does not interact with the worker branch's tree — artifacts are
    /// git objects under `refs/spur/artifacts/<session-id>`, orthogonal
    /// to `worker_branch`.
    pub async fn persist_artifact(
        &self,
        session_id: &SessionId,
        output_text: &str,
        kind: spur_acp::ArtifactKind,
    ) -> anyhow::Result<spur_acp::WorkerArtifact> {
        let info = self.lookup(session_id)?;
        crate::artifact::persist(&info.path, &session_id.to_string(), output_text, kind).await
    }

    /// Cherry-pick the worker's latest commit onto `target_branch`.
    pub async fn merge_worker(
        &self,
        session_id: &SessionId,
        target_branch: &str,
    ) -> Result<MergeResult> {
        let info = self.lookup(session_id)?;

        // Ensure we are on the target branch in the main repo.
        self.run_git(&["checkout", target_branch], None)
            .await
            .with_context(|| format!("failed to checkout target branch '{target_branch}'"))?;

        let result = self.run_git(&["cherry-pick", &info.branch], None).await;

        match result {
            Ok(_) => Ok(MergeResult::Success),
            Err(_) => {
                // Determine which files are in conflict.
                let conflict_output = self
                    .run_git(&["diff", "--name-only", "--diff-filter=U"], None)
                    .await
                    .unwrap_or_default();

                let files: Vec<String> = conflict_output
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| l.to_string())
                    .collect();

                // Abort the failed cherry-pick so the repo is not left in a
                // broken state.
                let _ = self.run_git(&["cherry-pick", "--abort"], None).await;

                Ok(MergeResult::Conflict { files })
            }
        }
    }

    /// Remove a worker's worktree and its branch, cleaning up all resources.
    pub async fn remove_worktree(&mut self, session_id: &SessionId) -> Result<()> {
        let session_str = session_id.to_string();
        let info = self
            .active
            .remove(&session_str)
            .ok_or_else(|| anyhow!("no active worktree for session {session_str}"))?;

        let path_str = info
            .path
            .to_str()
            .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?
            .to_string();

        self.run_git(&["worktree", "remove", &path_str, "--force"], None)
            .await
            .with_context(|| format!("failed to remove worktree at {path_str}"))?;

        self.run_git(&["branch", "-D", &info.branch], None)
            .await
            .with_context(|| format!("failed to delete branch '{}'", info.branch))?;

        Ok(())
    }

    /// Remove the worktree directory but keep the branch alive for future merge.
    /// Returns the preserved branch name.
    pub async fn detach_worktree(&mut self, session_id: &SessionId) -> Result<String> {
        let session_str = session_id.to_string();
        let info = self
            .active
            .remove(&session_str)
            .ok_or_else(|| anyhow!("no active worktree for session {session_str}"))?;

        let path_str = info
            .path
            .to_str()
            .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?
            .to_string();

        self.run_git(&["worktree", "remove", &path_str, "--force"], None)
            .await
            .with_context(|| format!("failed to detach worktree at {path_str}"))?;

        // Branch intentionally NOT deleted — preserved for brain review + merge.
        debug!(branch = %info.branch, "detached worktree, branch preserved");
        Ok(info.branch)
    }

    /// Remove worktrees that have been active longer than `max_age`.
    /// Returns the number of worktrees cleaned up.
    pub async fn cleanup_stale(&mut self, max_age: Duration) -> Result<usize> {
        let now = Instant::now();

        let stale_sessions: Vec<String> = self
            .active
            .iter()
            .filter(|(_, info)| now.duration_since(info.created_at) > max_age)
            .map(|(key, _)| key.clone())
            .collect();

        let count = stale_sessions.len();

        for session_str in stale_sessions {
            let session_id = SessionId(session_str.clone());
            if let Err(e) = self.remove_worktree(&session_id).await {
                debug!(
                    session = %session_str,
                    error = %e,
                    "failed to clean up stale worktree, skipping"
                );
            }
        }

        Ok(count)
    }

    /// Return the number of currently active worktrees.
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Look up a worktree by session ID.
    fn lookup(&self, session_id: &SessionId) -> Result<&WorktreeInfo> {
        let key = session_id.to_string();
        self.active
            .get(&key)
            .ok_or_else(|| anyhow!("no active worktree for session {key}"))
    }

    /// Remove orphaned SPUR worktrees left on disk from previous runs.
    /// Discovers worktrees via `git worktree list --porcelain` and removes
    /// any with a `spur/` branch prefix that aren't in `self.active`.
    pub async fn cleanup_orphans(&self) -> Result<usize> {
        let output = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&self.repo_root)
            .output()
            .await
            .context("failed to list worktrees")?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut removed = 0usize;

        // Parse porcelain output: blocks separated by blank lines.
        // Each block has "worktree <path>" and optionally "branch <ref>".
        let mut wt_path: Option<&str> = None;
        for line in stdout.lines().chain(std::iter::once("")) {
            if line.is_empty() {
                wt_path = None;
                continue;
            }
            if let Some(p) = line.strip_prefix("worktree ") {
                wt_path = Some(p);
            }
            if let Some(branch) = line.strip_prefix("branch ") {
                if branch.contains("spur/") {
                    if let Some(path) = wt_path {
                        // Skip if it's tracked in our active set
                        if self
                            .active
                            .values()
                            .any(|info| info.path.to_str() == Some(path))
                        {
                            continue;
                        }
                        debug!(path = %path, branch = %branch, "removing orphaned spur worktree");
                        let rm = Command::new("git")
                            .args(["worktree", "remove", "--force", path])
                            .current_dir(&self.repo_root)
                            .output()
                            .await;
                        match rm {
                            Ok(o) if o.status.success() => removed += 1,
                            Ok(o) => {
                                let stderr = String::from_utf8_lossy(&o.stderr);
                                tracing::warn!(path = %path, err = %stderr, "failed to remove orphaned worktree");
                            }
                            Err(e) => {
                                tracing::warn!(path = %path, err = %e, "failed to remove orphaned worktree");
                            }
                        }
                    }
                }
            }
        }

        // Clean up stale snapshot branches not referenced by any active worktree.
        let branches_output = self
            .run_git(&["branch", "--list", "spur/brain-snapshot-*"], None)
            .await
            .unwrap_or_default();

        // Collect branches that are still in use as a base for active worktrees.
        let active_bases: Vec<&str> = self
            .active
            .values()
            .map(|info| info.branch.as_str())
            .collect();

        for branch in branches_output.lines().map(|l| l.trim()) {
            if branch.is_empty() {
                continue;
            }
            // Don't delete if any active worker was branched from this snapshot.
            if active_bases.iter().any(|b| b.contains(branch)) {
                continue;
            }
            debug!(branch = %branch, "removing stale snapshot branch");
            let _ = self.run_git(&["branch", "-D", branch], None).await;
        }

        Ok(removed)
    }
}

#[cfg(test)]
impl WorktreeManager {
    pub fn new_for_test(repo_root: std::path::PathBuf) -> Self {
        Self {
            repo_root,
            active: std::collections::HashMap::new(),
        }
    }

    pub fn register_for_test(
        &mut self,
        session_id: SessionId,
        path: std::path::PathBuf,
        branch: String,
        base_commit: String,
        agent: String,
    ) {
        let key = session_id.to_string();
        self.active.insert(
            key,
            WorktreeInfo {
                session_id,
                path,
                branch,
                base_commit,
                agent,
                created_at: std::time::Instant::now(),
            },
        );
    }
}

#[cfg(test)]
mod tests_option_e {
    use super::*;

    /// Run a sequence of git commands in a dir. Panics on first error —
    /// test scaffolding only.
    async fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = tokio::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .await
            .expect("git command failed to spawn");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Build a minimal repo with one "base" commit, return base_sha.
    async fn seed_base_repo(tmp: &std::path::Path) -> String {
        git(tmp, &["init", "-q", "-b", "main"]).await;
        git(tmp, &["config", "user.email", "test@example.com"]).await;
        git(tmp, &["config", "user.name", "Test"]).await;
        tokio::fs::write(tmp.join("a.txt"), "base\n").await.unwrap();
        git(tmp, &["add", "a.txt"]).await;
        git(tmp, &["commit", "-q", "-m", "base"]).await;
        git(tmp, &["rev-parse", "HEAD"]).await
    }

    #[tokio::test]
    async fn collect_diff_falls_back_to_base_when_head_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base_sha = seed_base_repo(tmp.path()).await;

        // Worker commits their change (the scenario that broke bd-1mh.2).
        tokio::fs::write(tmp.path().join("a.txt"), "worker change\n")
            .await
            .unwrap();
        git(tmp.path(), &["add", "a.txt"]).await;
        git(tmp.path(), &["commit", "-q", "-m", "worker commit"]).await;

        // Working tree is clean; `git diff HEAD` is empty.
        // Fallback to base_commit..HEAD should capture the worker's commit.
        let sid = SessionId("s1".to_string());
        let mut manager = WorktreeManager::new_for_test(tmp.path().to_path_buf());
        manager.register_for_test(
            sid.clone(),
            tmp.path().to_path_buf(),
            "main".to_string(),
            base_sha.clone(),
            "test-agent".to_string(),
        );

        let (diff, basis) = manager.collect_diff(&sid).await.expect("collect_diff ok");
        let diff = diff.expect("expected Some(diff) via fallback, got None");
        assert!(
            diff.contains("worker change"),
            "diff should contain worker's change, got: {diff}"
        );
        assert_eq!(basis, "base_commit..HEAD");
    }

    #[tokio::test]
    async fn collect_diff_returns_head_basis_when_uncommitted_changes_exist() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base_sha = seed_base_repo(tmp.path()).await;

        // Worker leaves uncommitted changes (NOT the self-commit scenario).
        tokio::fs::write(tmp.path().join("a.txt"), "uncommitted\n")
            .await
            .unwrap();

        let sid = SessionId("s2".to_string());
        let mut manager = WorktreeManager::new_for_test(tmp.path().to_path_buf());
        manager.register_for_test(
            sid.clone(),
            tmp.path().to_path_buf(),
            "main".to_string(),
            base_sha.clone(),
            "test-agent".to_string(),
        );

        let (diff, basis) = manager.collect_diff(&sid).await.expect("collect_diff ok");
        let diff = diff.expect("expected Some(diff) from HEAD path");
        assert!(
            diff.contains("uncommitted"),
            "diff should capture uncommitted, got: {diff}"
        );
        assert_eq!(basis, "HEAD");
    }

    #[tokio::test]
    async fn collect_diff_returns_none_when_no_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let base_sha = seed_base_repo(tmp.path()).await;

        // No changes.
        let sid = SessionId("s3".to_string());
        let mut manager = WorktreeManager::new_for_test(tmp.path().to_path_buf());
        manager.register_for_test(
            sid.clone(),
            tmp.path().to_path_buf(),
            "main".to_string(),
            base_sha.clone(),
            "test-agent".to_string(),
        );

        let (diff, basis) = manager.collect_diff(&sid).await.expect("collect_diff ok");
        assert!(diff.is_none(), "expected None for no-change scenario");
        // Basis is still the attempted fallback.
        assert_eq!(basis, "base_commit..HEAD");
    }
}
