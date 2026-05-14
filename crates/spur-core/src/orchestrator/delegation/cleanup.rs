use super::*;
use spur_worktree::manager::{FinalizeCase, FinalizeOutcome};

/// Returns `true` if the worktree should be preserved (not removed) for
/// this final `DelegationStatus`.
///
/// Preserved:
///   - `Rejected` (human said no — operator may want to inspect diff).
///   - `TimedOut { fallback: Reject | Abandon }` (no human reviewed in
///     time AND the configured fallback says "treat as no" or "abandon";
///     preserve so a human can still inspect).
///
/// NOT preserved:
///   - `TimedOut { fallback: Approve }` — per spec, Approve fallback
///     means "auto-approve — worker's diff/summary retained as if
///     reviewed", so the diff must be committed and the worktree
///     detached (same lifecycle as a human Approve).
///   - `Success`/`Modified` (approved — changes committed on the
///     worker branch and preserved for later integration/PR creation).
///   - `Failed`/`Conflict`/`Timeout` (no real work to inspect — worker
///     hung or errored, or conflict blocked the run).
pub fn should_preserve_worktree(status: &DelegationStatus) -> bool {
    matches!(
        status,
        DelegationStatus::Rejected { .. }
            | DelegationStatus::TimedOut {
                fallback: TimeoutFallback::Reject { .. } | TimeoutFallback::Abandon,
                ..
            }
            // INV-6: preserve partial work for cancelled delegations so
            // the brain/user can inspect what was done before cancellation.
            | DelegationStatus::Cancelled { .. }
    )
}

/// Returns `true` if the worker's diff should be committed onto the
/// preserved worker branch based on the final `DelegationStatus`.
///
/// Commit on:
///   - `Success` (Approve).
///   - `Modified` (human-annotated approval).
///   - `TimedOut { fallback: Approve }` (auto-approve fallback — spec
///     says diff is "retained as if reviewed", so it must commit).
///
/// Do NOT commit on Rejected/TimedOut(Reject|Abandon) (preserve for
/// inspection), nor on Failed/Conflict/Timeout (no clean diff to keep).
pub fn should_commit_worker_diff(status: &DelegationStatus) -> bool {
    matches!(
        status,
        DelegationStatus::Success
            | DelegationStatus::Modified { .. }
            | DelegationStatus::TimedOut {
                fallback: TimeoutFallback::Approve,
                ..
            }
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WorktreeCleanupOutcome {
    pub(crate) worker_branch: Option<String>,
    pub(crate) normalization_warning: Option<String>,
}

pub(crate) struct WorktreeCleanupContext<'a> {
    pub(crate) agent: &'a str,
    pub(crate) worktree_path: &'a std::path::Path,
    pub(crate) bypass_hooks: bool,
    pub(crate) pm_service: Option<&'a Arc<PmService>>,
    pub(crate) issue_id: Option<&'a str>,
}

fn normalization_warning(outcome: &FinalizeOutcome, mark_noop: bool) -> Option<String> {
    match outcome.case {
        FinalizeCase::NoOp if mark_noop => None,
        FinalizeCase::NoOp => Some(
            "[normalize: worker produced no changes; no mark_noop signal received - review whether the task was actually a no-op]"
                .to_string(),
        ),
        FinalizeCase::Squashed => Some(format!(
            "[normalize: squashed {} worker commits into 1; please maintain a clean tree or commit exactly once]",
            outcome.intermediate_commits
        )),
        FinalizeCase::CommittedDirty => Some(
            "[normalize: committed dirty worker tree into 1 commit; please maintain a clean tree or commit exactly once]"
                .to_string(),
        ),
        FinalizeCase::AmendedDirty => Some(
            "[normalize: amended dirty worker tree into existing commit; please maintain a clean tree or commit exactly once]"
                .to_string(),
        ),
        FinalizeCase::AlreadyAtomic => None,
    }
}

async fn has_mark_noop_signal(pm_service: Option<&Arc<PmService>>, issue_id: Option<&str>) -> bool {
    let (Some(pm), Some(issue_id)) = (pm_service, issue_id) else {
        return false;
    };
    match pm.get_issue(issue_id).await {
        Ok(issue) => issue
            .labels
            .iter()
            .any(|label| label == &spur_mcp::plan::labels::signal_kind("mark-noop")),
        Err(error) => {
            tracing::warn!(issue_id, "failed to inspect mark_noop signal: {error}");
            false
        }
    }
}

/// Post-gate cleanup: commit the worker diff (if approved) and either
/// preserve or remove the worktree based on the final status.
///
/// Called from every terminal arm in `execute_delegation`. On Retry,
/// only `remove_worktree` is called (no commit — intermediate attempts
/// do not get merged into the brain tree).
pub(crate) async fn apply_worktree_cleanup(
    worktrees: &mut WorktreeManager,
    worker_session: &SessionId,
    final_status: &DelegationStatus,
    ctx: WorktreeCleanupContext<'_>,
) -> WorktreeCleanupOutcome {
    let mut normalize_warning = None;
    if should_commit_worker_diff(final_status) {
        match worktrees
            .finalize_worker_branch(
                worker_session,
                &format!("spur: worker {} output", ctx.agent),
                ctx.bypass_hooks,
            )
            .await
        {
            Ok(outcome) => {
                let mark_noop = has_mark_noop_signal(ctx.pm_service, ctx.issue_id).await;
                normalize_warning = normalization_warning(&outcome, mark_noop);
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to finalize worker branch");
            }
        }
    }

    if should_preserve_worktree(final_status) {
        tracing::info!(
            worktree = %ctx.worktree_path.display(),
            status = ?final_status,
            "preserving worktree for review inspection"
        );
        WorktreeCleanupOutcome {
            worker_branch: None,
            normalization_warning: normalize_warning,
        }
    } else if should_commit_worker_diff(final_status) {
        // Approved work: remove worktree dir but keep branch for merge.
        let captured_branch = worktrees.branch_for_session(worker_session);
        let worker_branch = match worktrees.detach_worktree(worker_session).await {
            Ok(branch) => Some(branch),
            Err(e) => {
                tracing::warn!(error = %e, "detach_worktree failed, falling back to full remove");
                let _ = worktrees.remove_worktree(worker_session).await;
                captured_branch.clone()
            }
        };
        if let (Some(captured), Some(detached)) = (&captured_branch, &worker_branch) {
            if captured != detached {
                tracing::warn!(
                    captured_branch = %captured,
                    detached_branch = %detached,
                    "pre-captured worker branch differs from detach_worktree return"
                );
            }
        }
        WorktreeCleanupOutcome {
            worker_branch,
            normalization_warning: normalize_warning,
        }
    } else {
        let _ = worktrees.remove_worktree(worker_session).await;
        WorktreeCleanupOutcome {
            worker_branch: None,
            normalization_warning: normalize_warning,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command as StdCommand;

    use super::{apply_worktree_cleanup, WorktreeCleanupContext};
    use super::{normalization_warning, FinalizeCase, FinalizeOutcome};
    use spur_acp::{BrainSessionId, DelegationStatus, SessionId};
    use spur_worktree::WorktreeManager;
    use tempfile::TempDir;

    fn run_git(repo: &Path, args: &[&str]) -> String {
        let out = StdCommand::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git command should execute");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn init_repo() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        run_git(dir.path(), &["init", "-q", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "t@t"]);
        run_git(dir.path(), &["config", "user.name", "t"]);
        std::fs::write(dir.path().join("README.md"), "base\n").expect("write base file");
        run_git(dir.path(), &["add", "README.md"]);
        run_git(dir.path(), &["commit", "-q", "-m", "base"]);
        dir
    }

    #[test]
    fn squashed_normalization_warning_mentions_intermediate_commit_count() {
        let warning = normalization_warning(
            &FinalizeOutcome {
                case: FinalizeCase::Squashed,
                intermediate_commits: 3,
            },
            false,
        )
        .expect("warning");

        assert!(warning.contains("normalize: squashed 3 worker commits into 1"));
    }

    #[test]
    fn no_op_without_mark_noop_warns() {
        let warning = normalization_warning(
            &FinalizeOutcome {
                case: FinalizeCase::NoOp,
                intermediate_commits: 0,
            },
            false,
        )
        .expect("warning");

        assert!(warning.contains("no mark_noop signal received"));
    }

    #[test]
    fn no_op_with_mark_noop_is_quiet() {
        let warning = normalization_warning(
            &FinalizeOutcome {
                case: FinalizeCase::NoOp,
                intermediate_commits: 0,
            },
            true,
        );

        assert_eq!(warning, None);
    }

    #[tokio::test]
    async fn approved_cleanup_preserves_branch_when_detach_fails() {
        let dir = init_repo();
        let mut worktrees = WorktreeManager::new(dir.path().to_path_buf());
        let brain = BrainSessionId::new(SessionId("550e8400-e29b-41d4-a716-446655440000".into()));
        let worker = SessionId("550e8400-e29b-41d4-a716-446655440001".into());

        let info = worktrees
            .create_worktree_v2(&brain, &worker, "codex", "main")
            .await
            .expect("create worktree");
        let expected_branch = info.branch.clone();

        let git_dir = dir.path().join(".git");
        let broken_git_dir = dir.path().join(".git.broken");
        std::fs::rename(&git_dir, &broken_git_dir)
            .expect("break repo metadata to force detach failure");

        let outcome = apply_worktree_cleanup(
            &mut worktrees,
            &worker,
            &DelegationStatus::Success,
            WorktreeCleanupContext {
                agent: "codex",
                worktree_path: &info.path,
                bypass_hooks: false,
                pm_service: None,
                issue_id: None,
            },
        )
        .await;

        assert_eq!(outcome.worker_branch, Some(expected_branch));

        // Best-effort restore so tempdir cleanup remains straightforward.
        let _ = std::fs::rename(&broken_git_dir, &git_dir);
    }
}
