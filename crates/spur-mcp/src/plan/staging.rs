//! Plan-recovery staging: build a `spur/plan-staging/{plan_id}` branch by
//! cherry-picking approved task tips in DAG order, supersede remaining tasks
//! in the original plan, and shape a new plan rooted at the staging branch.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Context;
use spur_worktree::{manager::WorktreeError, WorktreeManager};

use crate::plan::{PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};

/// Result of attempting to build the staging branch.
#[derive(Debug)]
pub struct StagingBuild {
    pub branch: String,
    pub merged_task_ids: Vec<String>,
    pub conflict: Option<crate::tool_schemas::StagingConflict>,
}

/// Walk approved tasks in DAG order and cherry-pick each task's
/// `[dispatched_base_oid..worker_branch tip]` onto a fresh staging branch.
///
/// On the first conflict, stops immediately and returns the partial branch at
/// the last successfully applied tip. The throwaway worktree is removed before
/// returning; the staging branch is preserved for the restarted plan.
pub async fn build_staging_branch(
    plan: &PlanState,
    repo_root: &Path,
) -> anyhow::Result<StagingBuild> {
    let branch_name = format!("spur/plan-staging/{}", plan.plan_id);
    let base_ref = plan
        .base_snapshot_branch
        .clone()
        .or_else(|| plan.base_snapshot_oid.clone())
        .unwrap_or_else(|| "HEAD".to_string());

    let approved_in_topo_order: Vec<&PlanTaskEntry> = plan
        .topo_ordered_tasks()
        .into_iter()
        .filter(|entry| matches!(entry.status, PlanTaskStatus::Approved { .. }))
        .filter(|entry| entry.worker_branch.is_some() && entry.dispatched_base_oid.is_some())
        .collect();

    let manager = WorktreeManager::new(repo_root.to_path_buf());
    let staging_id = uuid::Uuid::new_v4().simple().to_string();
    let staging_path = repo_root.join(".spur/worktrees/staging").join(&staging_id);
    if let Some(parent) = staging_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create staging worktree parent {}",
                parent.display()
            )
        })?;
    }

    if let Err(error) = manager
        .create_worktree_at(&staging_path, &branch_name, &base_ref)
        .await
    {
        let _ = manager.remove_worktree_at(&staging_path).await;
        return Err(error.context("failed to create staging worktree"));
    }

    let mut merged = Vec::new();
    let mut conflict = None;
    let mut fatal_error = None;

    for entry in approved_in_topo_order {
        let source_task_id = entry.spec.task_id.clone();
        let base_oid = entry
            .dispatched_base_oid
            .as_ref()
            .expect("filtered for dispatched_base_oid")
            .clone();
        let worker_branch = entry
            .worker_branch
            .as_ref()
            .expect("filtered for worker_branch")
            .clone();
        let tip_oid = match crate::server::run_git_capture(
            repo_root,
            None,
            &["rev-parse", "--verify", worker_branch.as_str()],
        )
        .await
        {
            Ok(oid) => oid,
            Err(error) => {
                fatal_error = Some(anyhow::anyhow!(
                    "failed to resolve worker branch '{}' for approved task {}: {}",
                    worker_branch,
                    source_task_id,
                    error
                ));
                break;
            }
        };
        let overlay = vec![(source_task_id.clone(), base_oid, tip_oid)];
        match manager.apply_overlays(&staging_path, &overlay).await {
            Ok(()) => merged.push(source_task_id),
            Err(WorktreeError::OverlayConflict {
                source_task_id,
                files,
            }) => {
                conflict = Some(crate::tool_schemas::StagingConflict {
                    dep_task_id: source_task_id,
                    files,
                });
                break;
            }
            Err(other) => {
                fatal_error = Some(anyhow::anyhow!("staging cherry-pick failed: {other}"));
                break;
            }
        }
    }

    if let Err(error) = manager.remove_worktree_at(&staging_path).await {
        tracing::warn!(
            path = %staging_path.display(),
            error = %error,
            "staging cleanup: remove_worktree_at failed"
        );
    }

    if let Some(error) = fatal_error {
        if let Err(delete_error) = manager.delete_branch(&branch_name).await {
            tracing::warn!(
                branch = %branch_name,
                error = %delete_error,
                "staging cleanup: delete_branch failed after fatal error"
            );
        }
        return Err(error);
    }

    Ok(StagingBuild {
        branch: branch_name,
        merged_task_ids: merged,
        conflict,
    })
}

/// Produce the task list for the restarted plan and the original task IDs to
/// mark Superseded. Approved work is omitted because it is now represented by
/// the staging branch. Dependencies pointing to omitted tasks are pruned.
pub fn shape_new_plan(plan: &PlanState) -> (Vec<PlanTask>, Vec<String>) {
    let carried_ids: HashSet<String> = plan
        .tasks
        .iter()
        .filter(|entry| should_carry_forward(&entry.status))
        .map(|entry| entry.spec.task_id.clone())
        .collect();

    let mut new_tasks = Vec::new();
    let mut superseded = Vec::new();
    for entry in &plan.tasks {
        if !carried_ids.contains(&entry.spec.task_id) {
            continue;
        }
        superseded.push(entry.spec.task_id.clone());
        let mut spec = entry.spec.clone();
        spec.issue_id = None;
        spec.depends_on
            .retain(|dep_task_id| carried_ids.contains(dep_task_id));
        new_tasks.push(spec);
    }
    (new_tasks, superseded)
}

fn should_carry_forward(status: &PlanTaskStatus) -> bool {
    matches!(
        status,
        PlanTaskStatus::Pending
            | PlanTaskStatus::Ready
            | PlanTaskStatus::Dispatched { .. }
            | PlanTaskStatus::AwaitingReview { .. }
            | PlanTaskStatus::BlockedOnSetupConflict { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{PlanMergeState, PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
    use spur_acp::{BrainSessionId, SessionId};
    use tempfile::TempDir;

    async fn run_git(repo: &std::path::Path, args: &[&str]) -> String {
        crate::server::run_git_capture(repo, None, args)
            .await
            .unwrap_or_else(|error| panic!("git {args:?} failed: {error}"))
    }

    async fn init_repo() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        run_git(dir.path(), &["init", "-q", "-b", "main"]).await;
        run_git(dir.path(), &["config", "user.email", "test@spur"]).await;
        run_git(dir.path(), &["config", "user.name", "spur-test"]).await;
        std::fs::write(dir.path().join("README.md"), "seed\n").expect("write seed");
        run_git(dir.path(), &["add", "README.md"]).await;
        run_git(dir.path(), &["commit", "-q", "-m", "seed"]).await;
        dir
    }

    async fn commit_file_on_branch(
        repo: &std::path::Path,
        branch: &str,
        base: &str,
        path: &str,
        content: &str,
    ) -> String {
        run_git(repo, &["checkout", "-q", "-B", branch, base]).await;
        std::fs::write(repo.join(path), content).expect("write worker file");
        run_git(repo, &["add", path]).await;
        run_git(repo, &["commit", "-q", "-m", &format!("write {path}")]).await;
        let tip = run_git(repo, &["rev-parse", "--verify", "HEAD"]).await;
        run_git(repo, &["checkout", "-q", "main"]).await;
        tip
    }

    fn entry_for(task_id: &str, deps: &[&str], status: PlanTaskStatus) -> PlanTaskEntry {
        PlanTaskEntry {
            spec: PlanTask {
                task_id: task_id.into(),
                agent: "test-agent".into(),
                task: "test".into(),
                depends_on: deps.iter().map(|s| s.to_string()).collect(),
                issue_id: Some(format!("bd-{task_id}")),
                context_files: vec![],
            },
            status,
            result: None,
            worker_branch: None,
            attempt: 1,
            history: vec![],
            last_delegation_id: None,
            dispatched_base_oid: None,
        }
    }

    fn approved_entry(
        task_id: &str,
        deps: &[&str],
        worker_branch: &str,
        dispatched_base_oid: &str,
    ) -> PlanTaskEntry {
        let mut entry = entry_for(task_id, deps, PlanTaskStatus::Approved { summary: None });
        entry.worker_branch = Some(worker_branch.to_string());
        entry.dispatched_base_oid = Some(dispatched_base_oid.to_string());
        entry
    }

    fn plan_with(entries: Vec<PlanTaskEntry>) -> PlanState {
        PlanState {
            plan_id: "test-plan".into(),
            tasks: entries,
            brain_session_id: BrainSessionId::from(SessionId("test".into())),
            base_snapshot_branch: Some("main".into()),
            base_snapshot_oid: None,
            merge_state: PlanMergeState::NotStarted,
            epic_id: None,
        }
    }

    #[test]
    fn shape_new_plan_excludes_approved_includes_pending_and_blocked() {
        let plan = plan_with(vec![
            entry_for("A", &[], PlanTaskStatus::Approved { summary: None }),
            entry_for(
                "B",
                &["A"],
                PlanTaskStatus::BlockedOnSetupConflict {
                    dep_task_id: "C".into(),
                    files: vec!["x.rs".into()],
                },
            ),
            entry_for("C", &["A"], PlanTaskStatus::Approved { summary: None }),
            entry_for("D", &["B", "C"], PlanTaskStatus::Pending),
        ]);
        let (new_tasks, superseded) = shape_new_plan(&plan);
        let new_ids: Vec<&str> = new_tasks.iter().map(|t| t.task_id.as_str()).collect();
        assert_eq!(new_ids, vec!["B", "D"]);
        assert_eq!(superseded, vec!["B".to_string(), "D".to_string()]);
        assert_eq!(new_tasks[0].depends_on, Vec::<String>::new());
        assert_eq!(new_tasks[1].depends_on, vec!["B".to_string()]);
        assert!(new_tasks.iter().all(|task| task.issue_id.is_none()));
    }

    #[tokio::test]
    async fn build_staging_branch_cherry_picks_approved_tips_in_dag_order() {
        let dir = init_repo().await;
        let base_oid = run_git(dir.path(), &["rev-parse", "--verify", "main"]).await;
        commit_file_on_branch(dir.path(), "spur/test-task-a", "main", "a.txt", "task A\n").await;
        commit_file_on_branch(dir.path(), "spur/test-task-b", "main", "b.txt", "task B\n").await;

        let plan = plan_with(vec![
            approved_entry("B", &["A"], "spur/test-task-b", &base_oid),
            approved_entry("A", &[], "spur/test-task-a", &base_oid),
            entry_for("C", &["B"], PlanTaskStatus::Pending),
        ]);

        let build = build_staging_branch(&plan, dir.path())
            .await
            .expect("build staging branch");
        assert_eq!(build.branch, "spur/plan-staging/test-plan");
        assert_eq!(build.merged_task_ids, vec!["A", "B"]);
        assert!(build.conflict.is_none(), "{:?}", build.conflict);
        assert_eq!(
            run_git(dir.path(), &["show", "spur/plan-staging/test-plan:a.txt"]).await,
            "task A"
        );
        assert_eq!(
            run_git(dir.path(), &["show", "spur/plan-staging/test-plan:b.txt"]).await,
            "task B"
        );
        assert_staging_worktrees_cleaned(dir.path());
    }

    #[tokio::test]
    async fn build_staging_branch_stops_on_first_conflict_and_keeps_partial_branch() {
        let dir = init_repo().await;
        std::fs::write(dir.path().join("conflict.txt"), "base\n").expect("write conflict base");
        run_git(dir.path(), &["add", "conflict.txt"]).await;
        run_git(dir.path(), &["commit", "-q", "-m", "conflict base"]).await;
        let base_oid = run_git(dir.path(), &["rev-parse", "--verify", "main"]).await;
        commit_file_on_branch(
            dir.path(),
            "spur/test-task-a",
            "main",
            "conflict.txt",
            "task A\n",
        )
        .await;
        commit_file_on_branch(
            dir.path(),
            "spur/test-task-b",
            "main",
            "conflict.txt",
            "task B\n",
        )
        .await;

        let plan = plan_with(vec![
            approved_entry("A", &[], "spur/test-task-a", &base_oid),
            approved_entry("B", &[], "spur/test-task-b", &base_oid),
            entry_for(
                "C",
                &["A", "B"],
                PlanTaskStatus::BlockedOnSetupConflict {
                    dep_task_id: "B".into(),
                    files: vec!["conflict.txt".into()],
                },
            ),
        ]);

        let build = build_staging_branch(&plan, dir.path())
            .await
            .expect("build returns partial branch on conflict");
        assert_eq!(build.merged_task_ids, vec!["A"]);
        let conflict = build.conflict.expect("expected conflict");
        assert_eq!(conflict.dep_task_id, "B");
        assert!(
            conflict.files.iter().any(|file| file == "conflict.txt"),
            "expected conflict.txt in files: {:?}",
            conflict.files
        );
        assert_eq!(
            run_git(
                dir.path(),
                &["show", "spur/plan-staging/test-plan:conflict.txt"],
            )
            .await,
            "task A"
        );
        assert_staging_worktrees_cleaned(dir.path());
    }

    fn assert_staging_worktrees_cleaned(repo: &std::path::Path) {
        let staging_root = repo.join(".spur/worktrees/staging");
        let is_empty = std::fs::read_dir(staging_root)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true);
        assert!(is_empty, "staging worktree directory should be empty");
    }
}
