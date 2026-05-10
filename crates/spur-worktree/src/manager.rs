use anyhow::{anyhow, Context, Result};
use spur_acp::{BrainSessionId, SessionId};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tracing::debug;

/// Audit-log a worktree mutation (add or remove) with a captured backtrace
/// so we can attribute "which call site touched .spur/worktrees/<id>" when
/// triaging the vanishing-worktree class of incidents.
///
/// Uses `force_capture` (independent of `RUST_BACKTRACE`) because this hook
/// only fires on rare, expensive git invocations — the perf cost is
/// negligible relative to spawning a `git` subprocess. Remove or down-grade
/// to `Backtrace::capture` once the originating remover is identified.
fn log_worktree_op(op: &str, path: &str, branch: Option<&str>) {
    let bt = std::backtrace::Backtrace::force_capture();
    tracing::info!(
        target: "spur.worktree.audit",
        op,
        path,
        branch = branch.unwrap_or("<unknown>"),
        backtrace = %bt,
        "worktree mutation invoked"
    );
}

/// Monotonic counter to guarantee unique snapshot branch names under concurrency.
static SNAPSHOT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Manages git worktree lifecycle for concurrent agent isolation.
pub struct WorktreeManager {
    pub repo_root: PathBuf,
    pub active: HashMap<String, WorktreeInfo>,
    git_mutex: Arc<tokio::sync::Mutex<()>>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2BranchOwner {
    pub agent: String,
    pub brain_session_id: BrainSessionId,
    pub worker_session_id: SessionId,
}

/// Parse a v2 worker branch into its owner triple. Returns None for any
/// non-v2 input. Slash-delimited so hyphenated/dotted agent names like
/// `claude-code` and `gemini-2.5-pro` parse unambiguously.
pub fn parse_v2_branch(branch: &str) -> Option<V2BranchOwner> {
    let rest = branch.strip_prefix("spur/worker/v2/")?;
    let mut parts = rest.rsplitn(3, '/');
    let worker_session_str = parts.next()?;
    let brain_session_str = parts.next()?;
    let agent = parts.next()?.to_string();

    fn is_uuid(s: &str) -> bool {
        s.len() == 36
            && s.chars().enumerate().all(|(i, c)| match i {
                8 | 13 | 18 | 23 => c == '-',
                _ => c.is_ascii_hexdigit(),
            })
    }

    if !is_uuid(brain_session_str) || !is_uuid(worker_session_str) {
        return None;
    }

    Some(V2BranchOwner {
        agent,
        brain_session_id: BrainSessionId::new(SessionId(brain_session_str.into())),
        worker_session_id: SessionId(worker_session_str.into()),
    })
}

/// Result of attempting to merge worker changes.
pub enum MergeResult {
    Success,
    Conflict { files: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeCase {
    NoOp,
    AlreadyAtomic,
    CommittedDirty,
    AmendedDirty,
    Squashed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizeOutcome {
    pub case: FinalizeCase,
    pub intermediate_commits: usize,
}

#[derive(Debug)]
pub enum WorktreeError {
    Anyhow(anyhow::Error),
    OverlayConflict {
        source_task_id: String,
        files: Vec<String>,
    },
    /// Cherry-pick failed for a non-conflict reason (invalid OID, hook
    /// rejection, GPG failure, preflight rev-list failure, etc.). The
    /// underlying git error is preserved for forensics. Distinct from
    /// OverlayConflict so callers can route non-conflict failures to a different
    /// remediation path.
    CherryPickFailed {
        source_task_id: String,
        range: String,
        error: String,
    },
}

impl std::fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Anyhow(e) => write!(f, "{e}"),
            Self::OverlayConflict {
                source_task_id,
                files,
            } => {
                write!(
                    f,
                    "overlay cherry-pick conflict applying {source_task_id}: {} files",
                    files.len()
                )
            }
            Self::CherryPickFailed {
                source_task_id,
                range,
                error,
            } => write!(
                f,
                "overlay cherry-pick failed applying {source_task_id} ({range}): {error}"
            ),
        }
    }
}

impl std::error::Error for WorktreeError {}

impl From<anyhow::Error> for WorktreeError {
    fn from(e: anyhow::Error) -> Self {
        Self::Anyhow(e)
    }
}

impl WorktreeManager {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            active: HashMap::new(),
            git_mutex: Arc::new(tokio::sync::Mutex::new(())),
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

    async fn run_git_with_retry(
        &self,
        args: &[&str],
        cwd: Option<&Path>,
        lock: bool,
    ) -> Result<String> {
        const DELAYS_MS: [u64; 5] = [50, 100, 250, 500, 1000];
        let mut last_err: Option<anyhow::Error> = None;

        for (attempt, base_ms) in DELAYS_MS.iter().enumerate() {
            let res = if lock {
                let _g = self.git_mutex.lock().await;
                self.run_git(args, cwd).await
            } else {
                self.run_git(args, cwd).await
            };

            match res {
                Ok(output) => {
                    if attempt > 0 {
                        tracing::warn!(
                            target: "spur.worktree.retry",
                            attempt,
                            args = ?args,
                            "git command succeeded after retry",
                        );
                    }
                    return Ok(output);
                }
                Err(e) if attempt < DELAYS_MS.len() - 1 && Self::is_transient_git_error(&e) => {
                    tracing::debug!(
                        target: "spur.worktree.retry",
                        attempt,
                        error = %e,
                        args = ?args,
                        "transient git error, retrying",
                    );
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(*base_ms)).await;
                }
                Err(e) => return Err(e),
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow!("retry exhausted with no error captured")))
    }

    fn is_transient_git_error(e: &anyhow::Error) -> bool {
        let s = e.to_string().to_lowercase();
        s.contains("index.lock")
            || s.contains("cannot lock ref")
            || s.contains("lock file")
            || s.contains("unable to create")
            || s.contains("is locked")
    }

    /// Resolve HEAD of the given worktree path to its OID.
    pub async fn resolve_head(&self, worktree_path: &Path) -> Result<String> {
        self.run_git(&["rev-parse", "HEAD"], Some(worktree_path))
            .await
    }

    /// Create a worktree at an explicit path and branch without registering it
    /// in the active worker-session map.
    pub async fn create_worktree_at(&self, path: &Path, branch: &str, base: &str) -> Result<()> {
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?;
        log_worktree_op("create_worktree_at", path_str, Some(branch));
        self.run_git(&["worktree", "add", path_str, "-b", branch, base], None)
            .await
            .with_context(|| format!("failed to create worktree at {path_str}"))?;
        Ok(())
    }

    /// Remove a worktree at an explicit path without consulting the active
    /// worker-session map.
    pub async fn remove_worktree_at(&self, path: &Path) -> Result<()> {
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?;
        log_worktree_op("remove_worktree_at", path_str, None);
        self.run_git(
            &["worktree", "remove", path_str, "--force", "--force"],
            None,
        )
        .await
        .with_context(|| format!("failed to remove worktree at {path_str}"))?;
        Ok(())
    }

    /// Delete a branch by name.
    pub async fn delete_branch(&self, name: &str) -> Result<()> {
        self.run_git(&["branch", "-D", name], None)
            .await
            .with_context(|| format!("failed to delete branch '{name}'"))?;
        Ok(())
    }

    /// Apply a chain of overlay cherry-picks to a worker worktree.
    ///
    /// Each overlay is `(source_task_id, base_oid, tip_oid)`. Runs
    /// `git cherry-pick base_oid..tip_oid` in `worktree_path` for each.
    /// On conflict: abort cherry-pick, return structured error with the
    /// conflicting task id and file list.
    ///
    /// `worktree_path` MUST be an isolated worktree exclusively owned by the
    /// caller during this method's execution. The method mutates the worktree's
    /// working tree, index, and HEAD; concurrent invocations on the same path
    /// will corrupt the cherry-pick state.
    pub async fn apply_overlays(
        &self,
        worktree_path: &Path,
        overlays: &[(String, String, String)],
    ) -> Result<(), WorktreeError> {
        for (source_task_id, base_oid, tip_oid) in overlays {
            let range = format!("{base_oid}..{tip_oid}");
            let commit_count = match self
                .run_git(&["rev-list", "--count", &range], Some(worktree_path))
                .await
            {
                Ok(output) => output,
                Err(e) => {
                    return Err(WorktreeError::CherryPickFailed {
                        source_task_id: source_task_id.clone(),
                        range,
                        error: format!("{e}"),
                    });
                }
            };
            let commit_count =
                commit_count
                    .parse::<u64>()
                    .map_err(|e| WorktreeError::CherryPickFailed {
                        source_task_id: source_task_id.clone(),
                        range: range.clone(),
                        error: format!(
                            "git rev-list --count returned non-numeric output {commit_count:?}: {e}"
                        ),
                    })?;

            if commit_count == 0 {
                if base_oid == tip_oid {
                    tracing::debug!(
                        source_task_id = %source_task_id,
                        base = %base_oid,
                        tip = %tip_oid,
                        commit_count = 0,
                        "apply_overlays: skipping empty range"
                    );
                } else {
                    tracing::warn!(
                        source_task_id = %source_task_id,
                        base = %base_oid,
                        tip = %tip_oid,
                        commit_count = 0,
                        "apply_overlays: skipping empty range with non-equal OIDs (tip is ancestor of base, or unrelated history; likely overlay-generation bug)"
                    );
                }
                continue;
            }

            let pick_result = self
                .run_git(&["cherry-pick", &range], Some(worktree_path))
                .await;

            if let Err(e) = pick_result {
                let error = format!("{e}");
                let conflict_files = self
                    .run_git(
                        &["diff", "--name-only", "--diff-filter=U"],
                        Some(worktree_path),
                    )
                    .await
                    .unwrap_or_default()
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let cherry_pick_head_exists = worktree_path.join(".git/CHERRY_PICK_HEAD").exists();
                let is_conflict = !conflict_files.is_empty() || cherry_pick_head_exists;

                if let Err(abort_err) = self
                    .run_git(&["cherry-pick", "--abort"], Some(worktree_path))
                    .await
                {
                    tracing::warn!(
                        worktree = %worktree_path.display(),
                        error = %abort_err,
                        "cherry-pick --abort failed; worktree may be in conflicted state"
                    );
                }

                if is_conflict {
                    return Err(WorktreeError::OverlayConflict {
                        source_task_id: source_task_id.clone(),
                        files: conflict_files,
                    });
                }

                return Err(WorktreeError::CherryPickFailed {
                    source_task_id: source_task_id.clone(),
                    range,
                    error,
                });
            }
        }

        Ok(())
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
                self.run_git_with_retry(&["branch", &branch_name, &commit], None, false)
                    .await
                    .context("failed to create snapshot branch")?;
            } else {
                // stash create returned empty despite dirty status — branch at HEAD.
                self.run_git_with_retry(&["branch", &branch_name, "HEAD"], None, false)
                    .await
                    .context("failed to create snapshot branch")?;
            }
        } else {
            self.run_git_with_retry(&["branch", &branch_name, "HEAD"], None, false)
                .await
                .context("failed to create snapshot branch")?;
        }

        Ok(branch_name)
    }

    /// Create a `spur/brain-snapshot-*` branch pointed at a caller-supplied
    /// ref (branch name, tag, or commit OID). Unlike `snapshot_brain_state`,
    /// this does not stash the working tree — the brain WT is never touched.
    /// Used by `submit_plan` when the operator passed an explicit `base`.
    pub async fn snapshot_at_ref(&self, target_ref: &str) -> Result<String> {
        // Resolve to an OID first so the snapshot branch is decoupled from
        // any subsequent movement of the source ref.
        let oid = self
            .run_git(&["rev-parse", "--verify", target_ref], None)
            .await
            .with_context(|| format!("failed to resolve ref '{target_ref}'"))?;

        let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
        let seq = SNAPSHOT_SEQ.fetch_add(1, Ordering::Relaxed);
        let branch_name = format!("spur/brain-snapshot-{timestamp}-{seq}");

        self.run_git_with_retry(&["branch", &branch_name, &oid], None, false)
            .await
            .context("failed to create snapshot branch at resolved ref")?;

        Ok(branch_name)
    }

    /// Create a worktree under the v2 branch namespace
    /// `spur/worker/v2/{agent}/{brain_session_id}/{worker_session_id}`.
    pub async fn create_worktree_v2(
        &mut self,
        brain_session_id: &BrainSessionId,
        worker_session_id: &SessionId,
        agent: &str,
        base_branch: &str,
    ) -> Result<WorktreeInfo> {
        let worker_str = worker_session_id.to_string();
        let worktree_path = self.repo_root.join(".spur/worktrees").join(&worker_str);
        let branch_name = format!(
            "spur/worker/v2/{}/{}/{}",
            agent, brain_session_id, worker_str,
        );

        let worktree_path_str = worktree_path
            .to_str()
            .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?;
        let base_commit = self
            .run_git(&["rev-parse", base_branch], None)
            .await
            .with_context(|| format!("failed to resolve base branch '{base_branch}'"))?;

        log_worktree_op("create_worktree_v2", worktree_path_str, Some(&branch_name));
        self.run_git_with_retry(
            &[
                "worktree",
                "add",
                worktree_path_str,
                "-b",
                &branch_name,
                base_branch,
            ],
            None,
            true,
        )
        .await
        .with_context(|| format!("failed to create v2 worktree at {worktree_path_str}"))?;

        let info = WorktreeInfo {
            session_id: worker_session_id.clone(),
            path: worktree_path,
            branch: branch_name,
            base_commit,
            agent: agent.to_string(),
            created_at: Instant::now(),
        };
        self.active.insert(worker_str.clone(), info);

        let stored = self.active.get(&worker_str).unwrap();
        Ok(WorktreeInfo {
            session_id: stored.session_id.clone(),
            path: stored.path.clone(),
            branch: stored.branch.clone(),
            base_commit: stored.base_commit.clone(),
            agent: stored.agent.clone(),
            created_at: stored.created_at,
        })
    }

    /// Delete a snapshot branch previously created by `snapshot_brain_state`.
    /// Safe to call immediately after `create_worktree_v2` succeeds because the
    /// worktree already has its own ref state.
    pub async fn delete_snapshot_branch(&self, branch: &str) -> Result<()> {
        self.run_git(&["branch", "-D", branch], None)
            .await
            .with_context(|| format!("failed to delete snapshot branch {branch}"))?;
        Ok(())
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

    /// Normalize a worker branch to integration-ready output.
    ///
    /// Cases:
    /// - 0 commits + dirty tree: stage and create one commit with `message`.
    /// - 1 commit + clean tree: no-op, preserving the worker's commit message.
    /// - 1 commit + dirty tree: stage and `commit --amend --no-edit`, preserving
    ///   the worker's commit message while mutating the committer timestamp.
    /// - N commits + any tree state: stage dirt if present, soft-reset to the
    ///   recorded base commit, then create one commit with `message`. This
    ///   flattens intermediate commits, including merge commits, so their
    ///   messages and topology are intentionally discarded.
    /// - 0 commits + clean tree: no-op and returns [`FinalizeCase::NoOp`].
    ///
    /// Before normalizing, aborts in-progress rebase, merge, cherry-pick, or
    /// revert state so poisoned worker worktrees cannot leak partial git
    /// operations into integration.
    pub async fn finalize_worker_branch(
        &self,
        session_id: &SessionId,
        message: &str,
        bypass_hooks: bool,
    ) -> Result<FinalizeOutcome> {
        self.finalize_worker_branch_inner(session_id, message, bypass_hooks, None)
            .await
    }

    async fn finalize_worker_branch_inner(
        &self,
        session_id: &SessionId,
        message: &str,
        bypass_hooks: bool,
        forced_post_count: Option<usize>,
    ) -> Result<FinalizeOutcome> {
        let info = self.lookup(session_id)?;

        self.abort_in_progress_git_operations(&info.path).await?;

        let commits_before = self
            .worker_commit_count(&info.path, &info.base_commit)
            .await?;
        let dirty_before = self.worktree_dirty(&info.path).await?;

        let case = match (commits_before, dirty_before) {
            (0, false) => FinalizeCase::NoOp,
            (0, true) => {
                self.run_git(&["add", "-A"], Some(&info.path))
                    .await
                    .context("failed to stage dirty worker changes")?;
                self.commit_with_message(&info.path, message, bypass_hooks)
                    .await
                    .context("failed to commit dirty worker changes")?;
                FinalizeCase::CommittedDirty
            }
            (1, false) => FinalizeCase::AlreadyAtomic,
            (1, true) => {
                self.run_git(&["add", "-A"], Some(&info.path))
                    .await
                    .context("failed to stage dirty worker changes for amend")?;
                self.amend_no_edit(&info.path, bypass_hooks)
                    .await
                    .context("failed to amend dirty worker changes")?;
                FinalizeCase::AmendedDirty
            }
            (n, _) => {
                if dirty_before {
                    self.run_git(&["add", "-A"], Some(&info.path))
                        .await
                        .context("failed to stage dirty worker changes before squash")?;
                }
                self.run_git(&["reset", "--soft", &info.base_commit], Some(&info.path))
                    .await
                    .with_context(|| {
                        format!("failed to soft-reset worker branch to {}", info.base_commit)
                    })?;
                self.commit_with_message(&info.path, message, bypass_hooks)
                    .await
                    .context("failed to commit squashed worker changes")?;
                debug!(
                    session = %session_id,
                    intermediate_commits = n,
                    "squashed worker branch during finalization"
                );
                FinalizeCase::Squashed
            }
        };

        let commits_after = match forced_post_count {
            Some(count) => count,
            None => {
                self.worker_commit_count(&info.path, &info.base_commit)
                    .await?
            }
        };
        if commits_after > 1 {
            tracing::warn!(
                session = %session_id,
                commits_after,
                "worker branch finalization did not converge"
            );
            return Err(anyhow!(
                "worker branch finalization did not converge: {commits_after} commits remain"
            ));
        }

        Ok(FinalizeOutcome {
            case,
            intermediate_commits: if case == FinalizeCase::Squashed {
                commits_before
            } else {
                0
            },
        })
    }

    async fn worker_commit_count(&self, worktree_path: &Path, base_commit: &str) -> Result<usize> {
        let range = format!("{base_commit}..HEAD");
        let out = self
            .run_git(&["rev-list", "--count", &range], Some(worktree_path))
            .await
            .with_context(|| format!("failed to count worker commits in {range}"))?;
        out.trim()
            .parse::<usize>()
            .with_context(|| format!("invalid git rev-list count: {out:?}"))
    }

    async fn worktree_dirty(&self, worktree_path: &Path) -> Result<bool> {
        let status = self
            .run_git(&["status", "--porcelain"], Some(worktree_path))
            .await
            .context("failed to inspect worker status")?;
        Ok(!status.is_empty())
    }

    async fn commit_with_message(
        &self,
        worktree_path: &Path,
        message: &str,
        bypass_hooks: bool,
    ) -> Result<String> {
        if bypass_hooks {
            self.run_git(
                &["commit", "--no-verify", "--no-gpg-sign", "-m", message],
                Some(worktree_path),
            )
            .await
        } else {
            self.run_git(&["commit", "-m", message], Some(worktree_path))
                .await
        }
    }

    async fn amend_no_edit(&self, worktree_path: &Path, bypass_hooks: bool) -> Result<String> {
        if bypass_hooks {
            self.run_git(
                &[
                    "commit",
                    "--amend",
                    "--no-edit",
                    "--no-verify",
                    "--no-gpg-sign",
                ],
                Some(worktree_path),
            )
            .await
        } else {
            self.run_git(&["commit", "--amend", "--no-edit"], Some(worktree_path))
                .await
        }
    }

    async fn abort_in_progress_git_operations(&self, worktree_path: &Path) -> Result<()> {
        let git_dir = self
            .run_git(&["rev-parse", "--git-dir"], Some(worktree_path))
            .await
            .context("failed to resolve worker git dir")?;
        let git_dir = PathBuf::from(git_dir.trim());
        let git_dir = if git_dir.is_absolute() {
            git_dir
        } else {
            worktree_path.join(git_dir)
        };

        if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
            self.run_git(&["rebase", "--abort"], Some(worktree_path))
                .await
                .context("failed to abort in-progress rebase")?;
        }

        if git_dir.join("MERGE_HEAD").exists() {
            self.run_git(&["merge", "--abort"], Some(worktree_path))
                .await
                .context("failed to abort in-progress merge")?;
        }

        if git_dir.join("CHERRY_PICK_HEAD").exists() {
            self.run_git(&["cherry-pick", "--abort"], Some(worktree_path))
                .await
                .context("failed to abort in-progress cherry-pick")?;
        }

        if git_dir.join("REVERT_HEAD").exists() {
            self.run_git(&["revert", "--abort"], Some(worktree_path))
                .await
                .context("failed to abort in-progress revert")?;
        }
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

    /// Squash-merge the worker branch onto `target_branch`.
    pub async fn merge_worker(
        &self,
        session_id: &SessionId,
        target_branch: &str,
        message: &str,
    ) -> Result<MergeResult> {
        let info = self.lookup(session_id)?;

        // Ensure we are on the target branch in the main repo.
        self.run_git(&["checkout", target_branch], None)
            .await
            .with_context(|| format!("failed to checkout target branch '{target_branch}'"))?;

        let result = self
            .run_git(&["merge", "--squash", &info.branch], None)
            .await;

        match result {
            Ok(_) => {
                let staged = Command::new("git")
                    .args(["diff", "--cached", "--quiet"])
                    .current_dir(&self.repo_root)
                    .output()
                    .await
                    .context("failed to execute staged squash merge check")?;
                match staged.status.code() {
                    Some(0) => {
                        debug!(
                            session = %session_id,
                            branch = %info.branch,
                            target_branch = %target_branch,
                            "squash merge produced no staged changes"
                        );
                        return Ok(MergeResult::Success);
                    }
                    Some(1) => {}
                    _ => {
                        let stderr = String::from_utf8_lossy(&staged.stderr).trim().to_string();
                        return Err(anyhow!(
                            "git diff --cached --quiet failed (exit {}): {}",
                            staged.status.code().unwrap_or(-1),
                            stderr
                        ));
                    }
                }

                self.run_git(&["commit", "-m", message], None)
                    .await
                    .context("failed to commit squash merge")?;
                Ok(MergeResult::Success)
            }
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

                // Abort the failed merge so the repo is not left in a broken
                // state.
                if self.run_git(&["merge", "--abort"], None).await.is_err() {
                    let _ = self.run_git(&["reset", "--merge"], None).await;
                }

                Ok(MergeResult::Conflict { files })
            }
        }
    }

    /// Remove a worker's worktree and its branch, cleaning up all resources.
    pub async fn remove_worktree(&mut self, session_id: &SessionId) -> Result<()> {
        let session_str = session_id.to_string();
        // Peek first; only remove from the in-memory map after git commands succeed,
        // so a failed git remove leaves the entry intact for retry/cleanup.
        let info = self
            .active
            .get(&session_str)
            .ok_or_else(|| anyhow!("no active worktree for session {session_str}"))?;
        let path_str = info
            .path
            .to_str()
            .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?
            .to_string();
        let branch = info.branch.clone();

        log_worktree_op("remove_worktree", &path_str, Some(&branch));
        // Run git operations; if any fail, return without mutating self.active.
        self.run_git(
            &["worktree", "remove", &path_str, "--force", "--force"],
            None,
        )
        .await
        .with_context(|| format!("failed to remove worktree at {path_str}"))?;
        self.run_git(&["branch", "-D", &branch], None)
            .await
            .with_context(|| format!("failed to delete branch '{branch}'"))?;

        // ONLY now: remove from self.active.
        self.active.remove(&session_str);
        Ok(())
    }

    /// Remove the worktree directory but keep the branch alive for future merge.
    /// Returns the preserved branch name.
    pub async fn detach_worktree(&mut self, session_id: &SessionId) -> Result<String> {
        let session_str = session_id.to_string();
        // Peek first; only remove from the in-memory map after git succeeds,
        // so a failed git remove leaves the entry intact for retry/cleanup.
        let info = self
            .active
            .get(&session_str)
            .ok_or_else(|| anyhow!("no active worktree for session {session_str}"))?;
        let path_str = info
            .path
            .to_str()
            .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?
            .to_string();
        let branch = info.branch.clone();

        log_worktree_op("detach_worktree", &path_str, Some(&branch));
        // Run git operations; if any fail, return without mutating self.active.
        self.run_git(
            &["worktree", "remove", &path_str, "--force", "--force"],
            None,
        )
        .await
        .with_context(|| format!("failed to detach worktree at {path_str}"))?;

        // Branch intentionally NOT deleted — preserved for brain review + merge.
        self.active.remove(&session_str);
        debug!(branch = %branch, "detached worktree, branch preserved");
        Ok(branch)
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
    /// any backed by a v2 worker branch (`refs/heads/spur/worker/v2/...`)
    /// that aren't in `self.active`. Pre-v2 / snapshot / user branches are
    /// never touched by this function (per spec invariant I-7).
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
                if branch.starts_with("refs/heads/spur/worker/v2/") {
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
                        log_worktree_op("cleanup_orphans", path, Some(branch));
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
            git_mutex: Arc::new(tokio::sync::Mutex::new(())),
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
mod overlay_tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn run_git(repo: &std::path::Path, args: &[&str]) -> String {
        let out = StdCommand::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn init_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        run_git(dir.path(), &["init", "-q", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "t@t"]);
        run_git(dir.path(), &["config", "user.name", "t"]);
        std::fs::write(dir.path().join("README"), "init\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-q", "-m", "init"]);
        dir
    }

    #[tokio::test]
    async fn apply_overlays_clean_cherry_picks() {
        let dir = init_repo();
        let main_oid = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "-B", "task1", "main"]);
        std::fs::write(dir.path().join("foo.rs"), "// foo\n").unwrap();
        run_git(dir.path(), &["add", "foo.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "task1"]);
        let task1_tip = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "main"]);
        let worker_path = dir.path().join("worker_wt");
        run_git(
            dir.path(),
            &[
                "worktree",
                "add",
                worker_path.to_str().unwrap(),
                "-b",
                "worker1",
                "main",
            ],
        );

        let mgr = WorktreeManager::new(dir.path().to_path_buf());
        mgr.apply_overlays(&worker_path, &[("task1".into(), main_oid, task1_tip)])
            .await
            .expect("clean cherry-pick should succeed");

        assert!(
            worker_path.join("foo.rs").exists(),
            "overlay should have brought foo.rs into worker worktree"
        );
    }

    #[tokio::test]
    async fn apply_overlays_returns_overlay_conflict_on_conflict() {
        let dir = init_repo();

        std::fs::write(dir.path().join("foo.rs"), "shared\n").unwrap();
        run_git(dir.path(), &["add", "foo.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "shared base"]);
        let base_oid = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "-B", "task1", &base_oid]);
        std::fs::write(dir.path().join("foo.rs"), "task1 version\n").unwrap();
        run_git(dir.path(), &["add", "foo.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "task1"]);

        run_git(dir.path(), &["checkout", "-q", "-B", "task2", &base_oid]);
        std::fs::write(dir.path().join("foo.rs"), "task2 version\n").unwrap();
        run_git(dir.path(), &["add", "foo.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "task2"]);
        let task2_tip = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "main"]);
        let worker_path = dir.path().join("worker_wt");
        run_git(
            dir.path(),
            &[
                "worktree",
                "add",
                worker_path.to_str().unwrap(),
                "-b",
                "worker1",
                "task1",
            ],
        );

        let mgr = WorktreeManager::new(dir.path().to_path_buf());
        let result = mgr
            .apply_overlays(&worker_path, &[("task2".into(), base_oid, task2_tip)])
            .await;
        match result {
            Err(WorktreeError::OverlayConflict {
                source_task_id,
                files,
            }) => {
                assert_eq!(source_task_id, "task2");
                assert!(
                    files.iter().any(|f| f == "foo.rs"),
                    "expected foo.rs in conflict files: {files:?}"
                );
            }
            other => panic!("expected OverlayConflict, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_overlays_skips_empty_range_silently() {
        let dir = init_repo();
        let main_oid = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "main"]);
        let worker_path = dir.path().join("worker_wt");
        run_git(
            dir.path(),
            &[
                "worktree",
                "add",
                worker_path.to_str().unwrap(),
                "-b",
                "worker1",
                "main",
            ],
        );

        let head_before = run_git(&worker_path, &["rev-parse", "HEAD"]);

        let mgr = WorktreeManager::new(dir.path().to_path_buf());
        let result = mgr
            .apply_overlays(
                &worker_path,
                &[("task1".into(), main_oid.clone(), main_oid)],
            )
            .await;
        result.expect("empty overlay range should be skipped");

        let head_after = run_git(&worker_path, &["rev-parse", "HEAD"]);
        assert_eq!(
            head_after, head_before,
            "empty overlay range must leave HEAD unchanged"
        );
        let cherry_pick_head = run_git(
            &worker_path,
            &["rev-parse", "--git-path", "CHERRY_PICK_HEAD"],
        );
        let cherry_pick_head = std::path::PathBuf::from(cherry_pick_head);
        let cherry_pick_head = if cherry_pick_head.is_absolute() {
            cherry_pick_head
        } else {
            worker_path.join(cherry_pick_head)
        };
        assert!(
            !cherry_pick_head.exists(),
            "empty overlay range must not leave cherry-pick state"
        );
    }

    #[tokio::test]
    async fn apply_overlays_skips_empty_range_in_chain() {
        let dir = init_repo();
        let main_oid = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "-B", "task1", "main"]);
        std::fs::write(dir.path().join("foo.rs"), "// foo\n").unwrap();
        run_git(dir.path(), &["add", "foo.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "task1"]);
        let task1_tip = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "-B", "task3", "main"]);
        std::fs::write(dir.path().join("bar.rs"), "// bar\n").unwrap();
        run_git(dir.path(), &["add", "bar.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "task3"]);
        let task3_tip = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "main"]);
        let worker_path = dir.path().join("worker_wt");
        run_git(
            dir.path(),
            &[
                "worktree",
                "add",
                worker_path.to_str().unwrap(),
                "-b",
                "worker1",
                "main",
            ],
        );

        let mgr = WorktreeManager::new(dir.path().to_path_buf());
        mgr.apply_overlays(
            &worker_path,
            &[
                ("task1".into(), main_oid.clone(), task1_tip),
                ("task2".into(), main_oid.clone(), main_oid.clone()),
                ("task3".into(), main_oid, task3_tip),
            ],
        )
        .await
        .expect("empty middle overlay range should be skipped");

        assert!(
            worker_path.join("foo.rs").exists(),
            "first overlay should have been applied"
        );
        assert!(
            worker_path.join("bar.rs").exists(),
            "third overlay should have been applied after empty range"
        );
        assert_eq!(
            run_git(&worker_path, &["rev-list", "--count", "main..HEAD"]),
            "2",
            "worker HEAD should advance by exactly the two non-empty overlays"
        );
    }

    #[tokio::test]
    async fn apply_overlays_skips_empty_first_in_chain() {
        let dir = init_repo();
        let main_oid = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "-B", "task2", "main"]);
        std::fs::write(dir.path().join("foo.rs"), "// foo\n").unwrap();
        run_git(dir.path(), &["add", "foo.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "task2"]);
        let task2_tip = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "-B", "task3", &task2_tip]);
        std::fs::write(dir.path().join("bar.rs"), "// bar\n").unwrap();
        run_git(dir.path(), &["add", "bar.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "task3"]);
        let task3_tip = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "main"]);
        let worker_path = dir.path().join("worker_wt");
        run_git(
            dir.path(),
            &[
                "worktree",
                "add",
                worker_path.to_str().unwrap(),
                "-b",
                "worker1",
                "main",
            ],
        );

        let mgr = WorktreeManager::new(dir.path().to_path_buf());
        mgr.apply_overlays(
            &worker_path,
            &[
                ("task1".into(), main_oid.clone(), main_oid.clone()),
                ("task2".into(), main_oid, task2_tip.clone()),
                ("task3".into(), task2_tip, task3_tip),
            ],
        )
        .await
        .expect("empty first overlay range should be skipped");

        assert!(
            worker_path.join("foo.rs").exists(),
            "second overlay should have been applied after empty first range"
        );
        assert!(
            worker_path.join("bar.rs").exists(),
            "third overlay should have been applied after empty first range"
        );
        assert_eq!(
            run_git(&worker_path, &["rev-list", "--count", "main..HEAD"]),
            "2",
            "worker HEAD should advance by exactly the two non-empty overlays"
        );
    }

    #[tokio::test]
    async fn apply_overlays_skips_empty_last_in_chain() {
        let dir = init_repo();
        let main_oid = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "-B", "task1", "main"]);
        std::fs::write(dir.path().join("foo.rs"), "// foo\n").unwrap();
        run_git(dir.path(), &["add", "foo.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "task1"]);
        let task1_tip = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "-B", "task2", &task1_tip]);
        std::fs::write(dir.path().join("bar.rs"), "// bar\n").unwrap();
        run_git(dir.path(), &["add", "bar.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "task2"]);
        let task2_tip = run_git(dir.path(), &["rev-parse", "HEAD"]);

        run_git(dir.path(), &["checkout", "-q", "main"]);
        let worker_path = dir.path().join("worker_wt");
        run_git(
            dir.path(),
            &[
                "worktree",
                "add",
                worker_path.to_str().unwrap(),
                "-b",
                "worker1",
                "main",
            ],
        );

        let mgr = WorktreeManager::new(dir.path().to_path_buf());
        mgr.apply_overlays(
            &worker_path,
            &[
                ("task1".into(), main_oid.clone(), task1_tip.clone()),
                ("task2".into(), task1_tip, task2_tip.clone()),
                ("task3".into(), task2_tip.clone(), task2_tip),
            ],
        )
        .await
        .expect("empty last overlay range should be skipped");

        assert!(
            worker_path.join("foo.rs").exists(),
            "first overlay should have been applied before empty last range"
        );
        assert!(
            worker_path.join("bar.rs").exists(),
            "second overlay should have been applied before empty last range"
        );
        assert_eq!(
            run_git(&worker_path, &["rev-list", "--count", "main..HEAD"]),
            "2",
            "worker HEAD should advance by exactly the two non-empty overlays"
        );
    }

    #[tokio::test]
    async fn apply_overlays_skips_backwards_range_with_warn() {
        let dir = init_repo();

        run_git(dir.path(), &["checkout", "-q", "main"]);
        std::fs::write(dir.path().join("a.rs"), "// a\n").unwrap();
        run_git(dir.path(), &["add", "a.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "A"]);
        let a_oid = run_git(dir.path(), &["rev-parse", "HEAD"]);

        std::fs::write(dir.path().join("b.rs"), "// b\n").unwrap();
        run_git(dir.path(), &["add", "b.rs"]);
        run_git(dir.path(), &["commit", "-q", "-m", "B"]);
        let b_oid = run_git(dir.path(), &["rev-parse", "HEAD"]);

        let worker_path = dir.path().join("worker_wt");
        run_git(
            dir.path(),
            &[
                "worktree",
                "add",
                worker_path.to_str().unwrap(),
                "-b",
                "worker1",
                "main",
            ],
        );
        let head_before = run_git(&worker_path, &["rev-parse", "HEAD"]);

        let mgr = WorktreeManager::new(dir.path().to_path_buf());
        // The implementation emits a warn-level telemetry signal for this malformed
        // non-equal empty range, but still treats it as a skip rather than an error.
        mgr.apply_overlays(&worker_path, &[("task1".into(), b_oid, a_oid)])
            .await
            .expect("backwards overlay range should be skipped");

        let head_after = run_git(&worker_path, &["rev-parse", "HEAD"]);
        assert_eq!(
            head_after, head_before,
            "backwards overlay range must leave HEAD unchanged"
        );
    }
}

#[cfg(test)]
mod finalize_worker_branch_tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn run_git(repo: &std::path::Path, args: &[&str]) -> String {
        let out = StdCommand::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn try_git(repo: &std::path::Path, args: &[&str]) -> std::process::Output {
        StdCommand::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap()
    }

    fn init_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        run_git(dir.path(), &["init", "-q", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "t@t"]);
        run_git(dir.path(), &["config", "user.name", "t"]);
        std::fs::write(dir.path().join("README.md"), "base\n").unwrap();
        run_git(dir.path(), &["add", "README.md"]);
        run_git(dir.path(), &["commit", "-q", "-m", "base"]);
        dir
    }

    fn setup_worker(dir: &TempDir) -> (WorktreeManager, std::path::PathBuf, SessionId, String) {
        let base = run_git(dir.path(), &["rev-parse", "HEAD"]);
        let worker_path = dir.path().join("worker");
        run_git(
            dir.path(),
            &[
                "worktree",
                "add",
                worker_path.to_str().unwrap(),
                "-b",
                "worker",
                "main",
            ],
        );
        let session = SessionId("worker-session".into());
        let mut manager = WorktreeManager::new_for_test(dir.path().to_path_buf());
        manager.register_for_test(
            session.clone(),
            worker_path.clone(),
            "worker".into(),
            base.clone(),
            "codex".into(),
        );
        (manager, worker_path, session, base)
    }

    fn commit_count(worker_path: &std::path::Path, base: &str) -> String {
        run_git(
            worker_path,
            &["rev-list", "--count", &format!("{base}..HEAD")],
        )
    }

    fn git_path(worker_path: &std::path::Path, path: &str) -> std::path::PathBuf {
        let out = run_git(worker_path, &["rev-parse", "--git-path", path]);
        let path = std::path::PathBuf::from(out);
        if path.is_absolute() {
            path
        } else {
            worker_path.join(path)
        }
    }

    #[cfg(unix)]
    fn install_rejecting_pre_commit(worker_path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        let hook = git_path(worker_path, "hooks/pre-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\necho hook rejected >&2\nexit 7\n").unwrap();
        let mut mode = std::fs::metadata(&hook).unwrap().permissions();
        mode.set_mode(0o755);
        std::fs::set_permissions(&hook, mode).unwrap();
    }

    #[tokio::test]
    async fn finalize_worker_branch_commits_zero_commits_dirty_tree() {
        let dir = init_repo();
        let (manager, worker_path, session, base) = setup_worker(&dir);
        std::fs::write(worker_path.join("one.txt"), "one\n").unwrap();

        let outcome = manager
            .finalize_worker_branch(&session, "task message", false)
            .await
            .unwrap();

        assert_eq!(outcome.case, FinalizeCase::CommittedDirty);
        assert_eq!(commit_count(&worker_path, &base), "1");
        assert_eq!(
            run_git(&worker_path, &["log", "-1", "--format=%s"]),
            "task message"
        );
        assert_eq!(run_git(&worker_path, &["status", "--porcelain"]), "");
    }

    #[tokio::test]
    async fn finalize_worker_branch_preserves_one_clean_commit() {
        let dir = init_repo();
        let (manager, worker_path, session, base) = setup_worker(&dir);
        std::fs::write(worker_path.join("one.txt"), "one\n").unwrap();
        run_git(&worker_path, &["add", "one.txt"]);
        run_git(&worker_path, &["commit", "-q", "-m", "worker message"]);
        let head_before = run_git(&worker_path, &["rev-parse", "HEAD"]);

        let outcome = manager
            .finalize_worker_branch(&session, "task message", false)
            .await
            .unwrap();

        assert_eq!(outcome.case, FinalizeCase::AlreadyAtomic);
        assert_eq!(commit_count(&worker_path, &base), "1");
        assert_eq!(run_git(&worker_path, &["rev-parse", "HEAD"]), head_before);
        assert_eq!(
            run_git(&worker_path, &["log", "-1", "--format=%s"]),
            "worker message"
        );
    }

    #[tokio::test]
    async fn finalize_worker_branch_amends_one_commit_with_dirty_tree() {
        let dir = init_repo();
        let (manager, worker_path, session, base) = setup_worker(&dir);
        std::fs::write(worker_path.join("one.txt"), "one\n").unwrap();
        run_git(&worker_path, &["add", "one.txt"]);
        run_git(&worker_path, &["commit", "-q", "-m", "worker message"]);
        std::fs::write(worker_path.join("two.txt"), "two\n").unwrap();

        let outcome = manager
            .finalize_worker_branch(&session, "task message", false)
            .await
            .unwrap();

        assert_eq!(outcome.case, FinalizeCase::AmendedDirty);
        assert_eq!(commit_count(&worker_path, &base), "1");
        assert_eq!(
            run_git(&worker_path, &["log", "-1", "--format=%s"]),
            "worker message"
        );
        assert_eq!(run_git(&worker_path, &["status", "--porcelain"]), "");
        assert!(worker_path.join("two.txt").exists());
    }

    #[tokio::test]
    async fn finalize_worker_branch_squashes_many_commits_and_dirty_tree() {
        let dir = init_repo();
        let (manager, worker_path, session, base) = setup_worker(&dir);
        for i in 1..=3 {
            std::fs::write(worker_path.join(format!("{i}.txt")), format!("{i}\n")).unwrap();
            run_git(&worker_path, &["add", "."]);
            run_git(&worker_path, &["commit", "-q", "-m", &format!("wip {i}")]);
        }
        std::fs::write(worker_path.join("dirty.txt"), "dirty\n").unwrap();

        let outcome = manager
            .finalize_worker_branch(&session, "task message", false)
            .await
            .unwrap();

        assert_eq!(outcome.case, FinalizeCase::Squashed);
        assert_eq!(outcome.intermediate_commits, 3);
        assert_eq!(commit_count(&worker_path, &base), "1");
        assert_eq!(
            run_git(&worker_path, &["log", "-1", "--format=%s"]),
            "task message"
        );
        assert_eq!(run_git(&worker_path, &["status", "--porcelain"]), "");
    }

    #[tokio::test]
    async fn finalize_worker_branch_noops_zero_commits_clean_tree() {
        let dir = init_repo();
        let (manager, worker_path, session, base) = setup_worker(&dir);

        let outcome = manager
            .finalize_worker_branch(&session, "task message", false)
            .await
            .unwrap();

        assert_eq!(outcome.case, FinalizeCase::NoOp);
        assert_eq!(commit_count(&worker_path, &base), "0");
    }

    #[tokio::test]
    async fn finalize_worker_branch_aborts_in_progress_merge_before_normalizing() {
        let dir = init_repo();
        std::fs::write(dir.path().join("conflict.txt"), "base\n").unwrap();
        run_git(dir.path(), &["add", "conflict.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "conflict base"]);
        run_git(dir.path(), &["checkout", "-q", "-b", "other"]);
        std::fs::write(dir.path().join("conflict.txt"), "other\n").unwrap();
        run_git(dir.path(), &["add", "conflict.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "other"]);
        run_git(dir.path(), &["checkout", "-q", "main"]);
        let (manager, worker_path, session, base) = setup_worker(&dir);
        std::fs::write(worker_path.join("conflict.txt"), "worker\n").unwrap();
        run_git(&worker_path, &["add", "conflict.txt"]);
        run_git(&worker_path, &["commit", "-q", "-m", "worker"]);
        let merge = try_git(&worker_path, &["merge", "other"]);
        assert!(!merge.status.success(), "merge should conflict");

        let outcome = manager
            .finalize_worker_branch(&session, "task message", false)
            .await
            .unwrap();

        assert_eq!(outcome.case, FinalizeCase::AlreadyAtomic);
        assert!(!git_path(&worker_path, "MERGE_HEAD").exists());
        assert_eq!(commit_count(&worker_path, &base), "1");
    }

    #[tokio::test]
    async fn finalize_worker_branch_aborts_in_progress_rebase_before_normalizing() {
        let dir = init_repo();
        std::fs::write(dir.path().join("conflict.txt"), "base\n").unwrap();
        run_git(dir.path(), &["add", "conflict.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "conflict base"]);
        run_git(dir.path(), &["checkout", "-q", "-b", "other"]);
        std::fs::write(dir.path().join("conflict.txt"), "other\n").unwrap();
        run_git(dir.path(), &["add", "conflict.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "other"]);
        run_git(dir.path(), &["checkout", "-q", "main"]);
        let (manager, worker_path, session, base) = setup_worker(&dir);
        std::fs::write(worker_path.join("conflict.txt"), "worker\n").unwrap();
        run_git(&worker_path, &["add", "conflict.txt"]);
        run_git(&worker_path, &["commit", "-q", "-m", "worker"]);
        let rebase = try_git(&worker_path, &["rebase", "other"]);
        assert!(!rebase.status.success(), "rebase should conflict");

        let outcome = manager
            .finalize_worker_branch(&session, "task message", false)
            .await
            .unwrap();

        assert_eq!(outcome.case, FinalizeCase::AlreadyAtomic);
        assert!(!git_path(&worker_path, "rebase-merge").exists());
        assert_eq!(commit_count(&worker_path, &base), "1");
    }

    #[tokio::test]
    async fn finalize_worker_branch_returns_err_when_post_count_exceeds_one() {
        let dir = init_repo();
        let (manager, worker_path, session, _base) = setup_worker(&dir);
        std::fs::write(worker_path.join("one.txt"), "one\n").unwrap();

        let err = manager
            .finalize_worker_branch_inner(&session, "task message", false, Some(2))
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("did not converge"),
            "expected convergence error, got {err:#}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn finalize_worker_branch_does_not_bypass_hooks_by_default() {
        let dir = init_repo();
        let (manager, worker_path, session, _base) = setup_worker(&dir);
        install_rejecting_pre_commit(&worker_path);
        std::fs::write(worker_path.join("one.txt"), "one\n").unwrap();

        let err = manager
            .finalize_worker_branch(&session, "task message", false)
            .await
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("failed to commit dirty worker changes"),
            "expected hook failure, got {err:#}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn finalize_worker_branch_bypasses_hooks_when_configured() {
        let dir = init_repo();
        let (manager, worker_path, session, base) = setup_worker(&dir);
        install_rejecting_pre_commit(&worker_path);
        std::fs::write(worker_path.join("one.txt"), "one\n").unwrap();

        let outcome = manager
            .finalize_worker_branch(&session, "task message", true)
            .await
            .unwrap();

        assert_eq!(outcome.case, FinalizeCase::CommittedDirty);
        assert_eq!(commit_count(&worker_path, &base), "1");
    }
}

#[cfg(test)]
mod merge_worker_tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn run_git(repo: &std::path::Path, args: &[&str]) -> String {
        let out = StdCommand::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn init_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        run_git(dir.path(), &["init", "-q", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "t@t"]);
        run_git(dir.path(), &["config", "user.name", "t"]);
        std::fs::write(dir.path().join("README"), "init\n").unwrap();
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-q", "-m", "init"]);
        dir
    }

    fn manager_with_worker(
        dir: &TempDir,
        session_id: &SessionId,
        branch: &str,
    ) -> (WorktreeManager, std::path::PathBuf, String) {
        let base_commit = run_git(dir.path(), &["rev-parse", "HEAD"]);
        let worker_path = dir.path().join(".spur/worktrees/worker");
        run_git(
            dir.path(),
            &[
                "worktree",
                "add",
                worker_path.to_str().unwrap(),
                "-b",
                branch,
                "main",
            ],
        );

        let mut manager = WorktreeManager::new_for_test(dir.path().to_path_buf());
        manager.register_for_test(
            session_id.clone(),
            worker_path.clone(),
            branch.to_string(),
            base_commit.clone(),
            "codex".to_string(),
        );
        (manager, worker_path, base_commit)
    }

    #[tokio::test]
    async fn merge_worker_squashes_multiple_worker_commits_with_message() {
        let dir = init_repo();
        let session_id = SessionId("550e8400-e29b-41d4-a716-446655440000".into());
        let (manager, worker_path, main_before) =
            manager_with_worker(&dir, &session_id, "worker/multi");

        std::fs::write(worker_path.join("one.txt"), "one\n").unwrap();
        run_git(&worker_path, &["add", "one.txt"]);
        run_git(&worker_path, &["commit", "-q", "-m", "worker one"]);
        std::fs::write(worker_path.join("two.txt"), "two\n").unwrap();
        run_git(&worker_path, &["add", "two.txt"]);
        run_git(&worker_path, &["commit", "-q", "-m", "worker two"]);

        let result = manager
            .merge_worker(&session_id, "main", "test-msg")
            .await
            .expect("merge should run");
        assert!(matches!(result, MergeResult::Success));

        let main_after = run_git(dir.path(), &["rev-parse", "main"]);
        assert_ne!(main_after, main_before, "merge should create one commit");
        assert_eq!(
            run_git(
                dir.path(),
                &["rev-list", "--count", &format!("{main_before}..main")]
            ),
            "1",
            "worker commits must be squashed into one target commit"
        );
        assert_eq!(
            run_git(dir.path(), &["log", "-1", "--format=%s", "main"]),
            "test-msg"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("one.txt")).unwrap(),
            "one\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("two.txt")).unwrap(),
            "two\n"
        );
    }

    #[tokio::test]
    async fn merge_worker_skips_commit_when_branch_delta_already_on_target() {
        let dir = init_repo();
        let session_id = SessionId("550e8400-e29b-41d4-a716-446655440000".into());
        let (manager, worker_path, _) =
            manager_with_worker(&dir, &session_id, "worker/already-applied");

        std::fs::write(worker_path.join("done.txt"), "done\n").unwrap();
        run_git(&worker_path, &["add", "done.txt"]);
        run_git(&worker_path, &["commit", "-q", "-m", "worker done"]);
        run_git(dir.path(), &["checkout", "-q", "main"]);
        std::fs::write(dir.path().join("done.txt"), "done\n").unwrap();
        run_git(dir.path(), &["add", "done.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "already applied"]);
        let main_before = run_git(dir.path(), &["rev-parse", "main"]);

        let result = manager
            .merge_worker(&session_id, "main", "should-not-commit")
            .await
            .expect("empty merge should run");
        assert!(matches!(result, MergeResult::Success));

        let main_after = run_git(dir.path(), &["rev-parse", "main"]);
        assert_eq!(main_after, main_before, "empty squash must not commit");
        assert_eq!(
            run_git(dir.path(), &["log", "-1", "--format=%s", "main"]),
            "already applied"
        );
    }

    #[tokio::test]
    async fn merge_worker_returns_conflict_files_and_aborts_squash_merge() {
        let dir = init_repo();
        std::fs::write(dir.path().join("shared.txt"), "base\n").unwrap();
        run_git(dir.path(), &["add", "shared.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "shared base"]);

        let session_id = SessionId("550e8400-e29b-41d4-a716-446655440000".into());
        let (manager, worker_path, _) = manager_with_worker(&dir, &session_id, "worker/conflict");

        std::fs::write(worker_path.join("shared.txt"), "worker\n").unwrap();
        run_git(&worker_path, &["add", "shared.txt"]);
        run_git(&worker_path, &["commit", "-q", "-m", "worker conflict"]);

        run_git(dir.path(), &["checkout", "-q", "main"]);
        std::fs::write(dir.path().join("shared.txt"), "main\n").unwrap();
        run_git(dir.path(), &["add", "shared.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "main conflict"]);
        let main_before = run_git(dir.path(), &["rev-parse", "main"]);

        let result = manager
            .merge_worker(&session_id, "main", "conflict-msg")
            .await
            .expect("conflict should be reported, not returned as an error");

        match result {
            MergeResult::Conflict { files } => {
                assert_eq!(files, vec!["shared.txt".to_string()]);
            }
            MergeResult::Success => panic!("expected conflict"),
        }
        assert_eq!(run_git(dir.path(), &["rev-parse", "main"]), main_before);
        assert_eq!(
            run_git(
                dir.path(),
                &["status", "--porcelain", "--untracked-files=no"]
            ),
            "",
            "merge abort should leave target worktree clean"
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
    async fn remove_worktree_keeps_in_memory_entry_when_git_fails() {
        use spur_acp::SessionId;
        let tmp = tempfile::TempDir::new().unwrap();
        let _base_sha = seed_base_repo(tmp.path()).await;

        let sid = SessionId("s1".into());
        let mut manager = WorktreeManager::new_for_test(tmp.path().to_path_buf());
        manager.register_for_test(
            sid.clone(),
            tmp.path().join("nonexistent"), // path doesn't exist; git remove will fail
            "spur/worker/v2/x/x/x".to_string(),
            "deadbeef".to_string(),
            "test".to_string(),
        );
        assert_eq!(manager.active_count(), 1);

        let res = manager.remove_worktree(&sid).await;
        assert!(res.is_err(), "git should have failed; {res:?}");
        assert_eq!(
            manager.active_count(),
            1,
            "in-memory entry must NOT be removed when git remove fails"
        );
    }

    #[tokio::test]
    async fn detach_worktree_keeps_in_memory_entry_when_git_fails() {
        use spur_acp::SessionId;
        let tmp = tempfile::TempDir::new().unwrap();
        let _base_sha = seed_base_repo(tmp.path()).await;

        let sid = SessionId("s2".into());
        let mut manager = WorktreeManager::new_for_test(tmp.path().to_path_buf());
        manager.register_for_test(
            sid.clone(),
            tmp.path().join("nonexistent"), // path doesn't exist; git remove will fail
            "spur/worker/v2/x/x/x".to_string(),
            "deadbeef".to_string(),
            "test".to_string(),
        );
        assert_eq!(manager.active_count(), 1);

        let res = manager.detach_worktree(&sid).await;
        assert!(res.is_err(), "git should have failed; {res:?}");
        assert_eq!(
            manager.active_count(),
            1,
            "in-memory entry must NOT be removed when git detach fails"
        );
    }

    #[tokio::test]
    async fn cleanup_orphans_only_touches_v2_worker_namespace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _base_sha = seed_base_repo(tmp.path()).await;

        // Create three branches: legacy, v2, and a non-SPUR user branch.
        let manager = WorktreeManager::new_for_test(tmp.path().to_path_buf());
        manager
            .run_git(&["branch", "spur/worker-legacy-deadbeef"], None)
            .await
            .expect("setup: create legacy branch");
        manager
            .run_git(
                &[
                    "branch",
                    "spur/worker/v2/codex/550e8400-e29b-41d4-a716-446655440000/deadbeef-1111-2222-3333-444455556666",
                ],
                None,
            )
            .await
            .expect("setup: create v2 branch");
        manager
            .run_git(&["branch", "feature/userwork"], None)
            .await
            .expect("setup: create user branch");

        // No worktrees back any of these branches, so cleanup_orphans should
        // not delete any worktrees AND must not touch user branches.
        let _removed = manager.cleanup_orphans().await.unwrap_or(0);

        let branches = manager
            .run_git(&["branch", "--list"], None)
            .await
            .unwrap_or_default();
        assert!(
            branches.contains("feature/userwork"),
            "user branches must NEVER be touched by cleanup_orphans"
        );
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

    #[tokio::test]
    async fn snapshot_branch_deleted_after_create_worktree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _base_sha = seed_base_repo(tmp.path()).await;

        let mut manager = WorktreeManager::new(tmp.path().to_path_buf());
        let snapshot_branch = manager
            .snapshot_brain_state()
            .await
            .expect("snapshot brain state");

        let branches_before = manager
            .run_git(&["branch", "--list", &snapshot_branch], None)
            .await
            .expect("list snapshot branch before create_worktree");
        assert!(
            branches_before.contains(&snapshot_branch),
            "snapshot branch must exist before worktree creation"
        );

        let sid = SessionId("s-snapshot".to_string());
        let brain = BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440000".into()));
        manager
            .create_worktree_v2(&brain, &sid, "codex", &snapshot_branch)
            .await
            .expect("create worktree");

        manager
            .delete_snapshot_branch(&snapshot_branch)
            .await
            .expect("delete snapshot branch");

        let branches_after = manager
            .run_git(&["branch", "--list", &snapshot_branch], None)
            .await
            .expect("list snapshot branch after delete");
        assert!(
            !branches_after.contains(&snapshot_branch),
            "snapshot branch must be deleted after worktree creation"
        );

        let wt_path = &manager.active.get(&sid.to_string()).unwrap().path;
        let status = manager
            .run_git(&["status", "--porcelain"], Some(wt_path))
            .await
            .expect("worktree status after snapshot deletion");
        assert!(status.is_empty(), "worktree should remain usable");
    }

    #[tokio::test]
    async fn create_worktree_uses_v2_namespace() {
        use spur_acp::SessionId;
        let tmp = tempfile::TempDir::new().unwrap();
        let _base_sha = seed_base_repo(tmp.path()).await;

        let mut manager = WorktreeManager::new(tmp.path().to_path_buf());
        let brain =
            spur_acp::BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440000".into()));
        let worker = SessionId("deadbeef-1111-2222-3333-444455556666".into());

        let info = manager
            .create_worktree_v2(&brain, &worker, "codex", "main")
            .await
            .expect("create v2 worktree");
        assert_eq!(
            info.branch,
            "spur/worker/v2/codex/550e8400-e29b-41d4-a716-446655440000/deadbeef-1111-2222-3333-444455556666"
        );
    }

    #[test]
    fn classifies_git_lock_failures_as_transient() {
        let err = anyhow!(
            "git worktree failed (exit 128): fatal: cannot lock ref 'refs/heads/spur/worker/v2/codex/b/w': is locked"
        );
        assert!(WorktreeManager::is_transient_git_error(&err));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn parallel_worktree_creation_survives_lock_pressure() {
        let tmp = tempfile::TempDir::new().unwrap();
        let _base_sha = seed_base_repo(tmp.path()).await;
        let brain =
            spur_acp::BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440000".into()));
        let mut workers = Vec::new();
        let mut ref_locks = Vec::new();
        for n in 0..8 {
            let worker = SessionId(format!("00000000-0000-0000-0000-{n:012x}"));
            let lock_path = tmp
                .path()
                .join(".git/refs/heads/spur/worker/v2/codex")
                .join(brain.to_string())
                .join(format!("{worker}.lock"));
            tokio::fs::create_dir_all(lock_path.parent().unwrap())
                .await
                .expect("create ref lock parent");
            tokio::fs::write(&lock_path, "held by test\n")
                .await
                .expect("create synthetic ref lock");
            workers.push(worker);
            ref_locks.push(lock_path);
        }

        let unlock_paths = ref_locks.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            for path in unlock_paths {
                let _ = tokio::fs::remove_file(path).await;
            }
        });

        let mut set = tokio::task::JoinSet::new();
        for worker in workers {
            let repo_root = tmp.path().to_path_buf();
            let brain = brain.clone();
            set.spawn(async move {
                let mut manager = WorktreeManager::new(repo_root);
                manager
                    .create_worktree_v2(&brain, &worker, "codex", "main")
                    .await
                    .map(|info| info.branch)
            });
        }

        let mut branches = Vec::new();
        while let Some(result) = set.join_next().await {
            let branch = result.expect("task join").expect("create v2 worktree");
            branches.push(branch);
        }

        assert_eq!(branches.len(), 8);
        let manager = WorktreeManager::new(tmp.path().to_path_buf());
        for branch in branches {
            manager
                .run_git(&["rev-parse", "--verify", &branch], None)
                .await
                .expect("created worker branch should resolve");
        }
    }
}

#[cfg(test)]
mod v2_branch_tests {
    use super::*;

    fn make(agent: &str, brain: &str, worker: &str) -> String {
        format!("spur/worker/v2/{agent}/{brain}/{worker}")
    }

    #[test]
    fn parse_v2_simple_agent() {
        let b = make(
            "claude",
            "550e8400-e29b-41d4-a716-446655440000",
            "deadbeef-1111-2222-3333-444455556666",
        );
        let p = parse_v2_branch(&b).expect("parses");
        assert_eq!(p.agent, "claude");
    }

    #[test]
    fn parse_v2_hyphenated_agent() {
        let b = make(
            "claude-code",
            "550e8400-e29b-41d4-a716-446655440000",
            "deadbeef-1111-2222-3333-444455556666",
        );
        let p = parse_v2_branch(&b).expect("parses");
        assert_eq!(p.agent, "claude-code");
    }

    #[test]
    fn parse_v2_dotted_agent() {
        let b = make(
            "gemini-2.5-pro",
            "550e8400-e29b-41d4-a716-446655440000",
            "deadbeef-1111-2222-3333-444455556666",
        );
        let p = parse_v2_branch(&b).expect("parses");
        assert_eq!(p.agent, "gemini-2.5-pro");
    }

    #[test]
    fn parse_v2_rejects_pre_v2_format() {
        let b = "spur/worker-claude-deadbeef-1111-2222-3333-444455556666";
        assert!(parse_v2_branch(b).is_none());
    }

    #[test]
    fn parse_v2_rejects_when_session_not_uuid() {
        let b = "spur/worker/v2/claude/not-a-uuid/deadbeef-1111-2222-3333-444455556666";
        assert!(parse_v2_branch(b).is_none());
    }
}

#[cfg(test)]
mod snapshot_at_ref_tests {
    use super::WorktreeManager;
    use std::process::Command;
    use tempfile::TempDir;

    fn run_git(repo: &std::path::Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git spawn");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn capture_git(repo: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git spawn");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[tokio::test]
    async fn snapshot_at_ref_creates_branch_at_named_oid_without_touching_wt() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path();
        run_git(repo, &["init", "-q", "-b", "main"]);
        run_git(repo, &["config", "user.email", "t@t"]);
        run_git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a"), "1").unwrap();
        run_git(repo, &["add", "a"]);
        run_git(repo, &["commit", "-q", "-m", "first"]);
        let main_oid = capture_git(repo, &["rev-parse", "HEAD"]);

        // Branch off main, advance, then jump back so HEAD != target_branch's tip.
        run_git(repo, &["checkout", "-q", "-b", "target"]);
        std::fs::write(repo.join("b"), "2").unwrap();
        run_git(repo, &["add", "b"]);
        run_git(repo, &["commit", "-q", "-m", "second"]);
        let target_oid = capture_git(repo, &["rev-parse", "HEAD"]);
        run_git(repo, &["checkout", "-q", "main"]);

        // Make WT dirty — must NOT cause snapshot_at_ref to fail.
        std::fs::write(repo.join("a"), "dirty").unwrap();

        let manager = WorktreeManager::new(repo.to_path_buf());
        let snap_branch = manager
            .snapshot_at_ref(&target_oid)
            .await
            .expect("snapshot_at_ref must succeed despite dirty WT");

        assert!(
            snap_branch.starts_with("spur/brain-snapshot-"),
            "snapshot branch name must follow convention; got {snap_branch}"
        );
        let snap_oid = capture_git(repo, &["rev-parse", &snap_branch]);
        assert_eq!(
            snap_oid, target_oid,
            "snapshot must point at the requested target OID, not main HEAD ({main_oid}) and not a stash commit"
        );
        // Brain WT untouched (file still says "dirty" because we never stashed).
        let a_contents = std::fs::read_to_string(repo.join("a")).unwrap();
        assert_eq!(a_contents, "dirty");
    }

    #[tokio::test]
    async fn snapshot_at_ref_resolves_branch_name_to_oid() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path();
        run_git(repo, &["init", "-q", "-b", "main"]);
        run_git(repo, &["config", "user.email", "t@t"]);
        run_git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a"), "1").unwrap();
        run_git(repo, &["add", "a"]);
        run_git(repo, &["commit", "-q", "-m", "first"]);
        run_git(repo, &["branch", "feature/x"]);
        let feature_oid = capture_git(repo, &["rev-parse", "feature/x"]);

        let manager = WorktreeManager::new(repo.to_path_buf());
        let snap_branch = manager.snapshot_at_ref("feature/x").await.unwrap();
        let snap_oid = capture_git(repo, &["rev-parse", &snap_branch]);
        assert_eq!(snap_oid, feature_oid);
    }

    #[tokio::test]
    async fn snapshot_at_ref_fails_loudly_on_unknown_ref() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path();
        run_git(repo, &["init", "-q", "-b", "main"]);
        run_git(repo, &["config", "user.email", "t@t"]);
        run_git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a"), "1").unwrap();
        run_git(repo, &["add", "a"]);
        run_git(repo, &["commit", "-q", "-m", "first"]);

        let manager = WorktreeManager::new(repo.to_path_buf());
        let err = manager.snapshot_at_ref("does/not/exist").await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("does/not/exist") || msg.to_lowercase().contains("unknown"),
            "error must mention the bad ref; got: {msg}"
        );
    }
}
