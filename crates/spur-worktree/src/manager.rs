use anyhow::{anyhow, Context, Result};
use spur_acp::SessionId;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::process::Command;
use tracing::debug;

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
    pub async fn snapshot_brain_state(&self) -> Result<String> {
        // Check for uncommitted changes.
        let status = self.run_git(&["status", "--porcelain"], None).await?;
        let dirty = !status.is_empty();

        // Build a unique snapshot branch name.
        let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S%3f");
        let branch_name = format!("spur/brain-snapshot-{timestamp}");

        // Create the snapshot branch at HEAD.
        self.run_git(&["branch", &branch_name, "HEAD"], None)
            .await
            .context("failed to create snapshot branch")?;

        if dirty {
            // Create a stash ref without touching the working directory.
            let stash_ref = self
                .run_git(&["stash", "create"], None)
                .await
                .context("failed to create stash")?;

            if !stash_ref.is_empty() {
                // Check out the snapshot branch in a detached manner to apply
                // the stash without disturbing the real working tree. We use a
                // temporary worktree for this operation.
                let tmp_dir = self.repo_root.join(".spur/tmp-snapshot");

                // Create a temp worktree on the snapshot branch.
                self.run_git(
                    &[
                        "worktree",
                        "add",
                        tmp_dir.to_str().unwrap(),
                        &branch_name,
                    ],
                    None,
                )
                .await
                .context("failed to create temp worktree for snapshot")?;

                // Apply the stash in the temp worktree.
                let apply_result = self
                    .run_git(&["stash", "apply", &stash_ref], Some(&tmp_dir))
                    .await;

                if apply_result.is_ok() {
                    // Stage everything and commit.
                    self.run_git(&["add", "-A"], Some(&tmp_dir)).await?;
                    self.run_git(
                        &["commit", "-m", "spur: brain snapshot"],
                        Some(&tmp_dir),
                    )
                    .await
                    .context("failed to commit snapshot")?;
                }

                // Clean up the temp worktree.
                let _ = self
                    .run_git(
                        &[
                            "worktree",
                            "remove",
                            tmp_dir.to_str().unwrap(),
                            "--force",
                        ],
                        None,
                    )
                    .await;
            }
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
        let worktree_path = self
            .repo_root
            .join(".spur/worktrees")
            .join(&session_str);
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
        .with_context(|| {
            format!("failed to create worktree at {worktree_path_str}")
        })?;

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

    /// Collect the diff of uncommitted changes in a worker's worktree.
    /// Returns `None` if there are no changes.
    pub async fn collect_diff(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<String>> {
        let info = self.lookup(session_id)?;

        let diff = self
            .run_git(&["diff", "HEAD"], Some(&info.path))
            .await
            .context("failed to collect diff")?;

        if diff.is_empty() {
            Ok(None)
        } else {
            Ok(Some(diff))
        }
    }

    /// Stage and commit all changes in a worker's worktree.
    /// No-op if there is nothing to commit.
    pub async fn commit_worker_changes(
        &self,
        session_id: &SessionId,
        message: &str,
    ) -> Result<()> {
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
            .with_context(|| {
                format!("failed to checkout target branch '{target_branch}'")
            })?;

        let result = self
            .run_git(&["cherry-pick", &info.branch], None)
            .await;

        match result {
            Ok(_) => Ok(MergeResult::Success),
            Err(_) => {
                // Determine which files are in conflict.
                let conflict_output = self
                    .run_git(
                        &["diff", "--name-only", "--diff-filter=U"],
                        None,
                    )
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
    pub async fn remove_worktree(
        &mut self,
        session_id: &SessionId,
    ) -> Result<()> {
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
            .with_context(|| {
                format!("failed to delete branch '{}'", info.branch)
            })?;

        Ok(())
    }

    /// Remove worktrees that have been active longer than `max_age`.
    /// Returns the number of worktrees cleaned up.
    pub async fn cleanup_stale(&mut self, max_age: Duration) -> Result<usize> {
        let now = Instant::now();

        let stale_sessions: Vec<String> = self
            .active
            .iter()
            .filter(|(_, info)| {
                now.duration_since(info.created_at) > max_age
            })
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
}
