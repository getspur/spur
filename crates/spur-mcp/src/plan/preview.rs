//! Overlay dry-run helper. Computes the predicted post-overlay HEAD for a plan
//! task by creating a throwaway worktree, applying the approved-dep overlay
//! closure, and either capturing the HEAD oid or surfacing an `OverlayConflict`.
//!
//! Used by:
//!   - the `preview_task_base` MCP tool (read-only brain inspection), and
//!   - the reconciler's pre-dispatch check (transitions to BlockedOnSetupConflict
//!     before spawning a worker on a predicted conflict; see br-xh7 task 2).

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use tokio::sync::Mutex;

use spur_worktree::{manager::WorktreeError, WorktreeManager};

use crate::tool_schemas::{PreviewConflict, PreviewTaskBaseOutput};
use crate::tools::OverlayCommit;

/// Compute the overlay preview for `task_id` in `plan_state`. The throwaway
/// worktree and branch are always cleaned up before returning.
///
/// `repo_root` is the workspace root used to materialize the preview worktree
/// under `.spur/worktrees/preview/<uuid>`.
///
/// Returns:
///   - `Ok(PreviewTaskBaseOutput)` on success: `predicted_base_oid` is `Some`
///     when overlays apply cleanly, `None` plus `conflict` populated on
///     `OverlayConflict`.
///   - `Err(_)` for any other error (git invocation failure, missing dep oid,
///     worktree creation failure).
pub async fn preview_overlay(
    plan_state: &Arc<Mutex<crate::plan::PlanState>>,
    plan_id: &str,
    task_id: &str,
    repo_root: &Path,
) -> anyhow::Result<PreviewTaskBaseOutput> {
    let (base_ref, overlay_sources) = {
        let state = plan_state.lock().await;
        if !state
            .tasks
            .iter()
            .any(|entry| entry.spec.task_id == task_id)
        {
            anyhow::bail!("Unknown task_id '{task_id}' in plan '{plan_id}'");
        }

        let overlay_sources = state
            .approved_dep_closure(task_id)
            .into_iter()
            .filter_map(|dep| {
                let dep_task_id = dep.spec.task_id.as_str();
                let Some(base_oid) = dep.dispatched_base_oid.clone() else {
                    // T0 invariant guarantees dispatched_base_oid is Some for Approved deps; this is belt-and-suspenders.
                    tracing::warn!(
                        plan_id,
                        task_id,
                        dep_task_id,
                        "preview_overlay: skipping approved dep without dispatched_base_oid"
                    );
                    return None;
                };
                let Some(worker_branch) = dep.worker_branch.as_ref().cloned() else {
                    tracing::warn!(
                        plan_id,
                        task_id,
                        dep_task_id,
                        "preview_overlay: skipping approved dep without worker_branch"
                    );
                    return None;
                };
                Some((dep.spec.task_id.clone(), base_oid, worker_branch))
            })
            .collect::<Vec<_>>();

        let base_ref = state
            .base_snapshot_branch
            .clone()
            .or_else(|| state.base_snapshot_oid.clone())
            .unwrap_or_else(|| "HEAD".to_string());

        (base_ref, overlay_sources)
    };

    let mut overlays = Vec::with_capacity(overlay_sources.len());
    for (source_task_id, base_oid, worker_branch) in overlay_sources {
        let tip_oid = crate::server::run_git_capture(
            repo_root,
            None,
            &["rev-parse", "--verify", worker_branch.as_str()],
        )
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "failed to resolve worker branch '{}' for dependency {}: {}",
                worker_branch,
                source_task_id,
                error
            )
        })?;
        overlays.push(OverlayCommit {
            source_task_id,
            base_oid,
            tip_oid,
        });
    }

    let preview_id = uuid::Uuid::new_v4().simple().to_string();
    let throwaway_path = repo_root.join(".spur/worktrees/preview").join(&preview_id);
    if let Some(parent) = throwaway_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create preview worktree parent {}",
                parent.display()
            )
        })?;
    }
    let throwaway_branch = format!("spur/preview-{preview_id}");
    let manager = WorktreeManager::new(repo_root.to_path_buf());

    if let Err(error) = manager
        .create_worktree_at(&throwaway_path, &throwaway_branch, &base_ref)
        .await
    {
        let _ = manager.remove_worktree_at(&throwaway_path).await;
        let _ = manager.delete_branch(&throwaway_branch).await;
        return Err(error);
    }

    let overlay_args = overlays
        .iter()
        .map(|overlay| {
            (
                overlay.source_task_id.clone(),
                overlay.base_oid.clone(),
                overlay.tip_oid.clone(),
            )
        })
        .collect::<Vec<_>>();

    let preview_result = match manager.apply_overlays(&throwaway_path, &overlay_args).await {
        Ok(()) => match manager.resolve_head(&throwaway_path).await {
            Ok(head) => Ok(PreviewTaskBaseOutput {
                overlays,
                predicted_base_oid: Some(head),
                conflict: None,
            }),
            Err(error) => Err(error.context("failed to resolve preview HEAD")),
        },
        Err(WorktreeError::OverlayConflict {
            source_task_id,
            files,
        }) => Ok(PreviewTaskBaseOutput {
            overlays,
            predicted_base_oid: None,
            conflict: Some(PreviewConflict {
                dep_task_id: source_task_id,
                files,
            }),
        }),
        Err(other) => Err(anyhow::anyhow!("preview overlay failed: {other}")),
    };

    if let Err(error) = manager.remove_worktree_at(&throwaway_path).await {
        tracing::warn!(
            %plan_id,
            %task_id,
            path = %throwaway_path.display(),
            error = %error,
            "preview cleanup: remove_worktree_at failed"
        );
    }
    if let Err(error) = manager.delete_branch(&throwaway_branch).await {
        tracing::warn!(
            %plan_id,
            %task_id,
            branch = %throwaway_branch,
            error = %error,
            "preview cleanup: delete_branch failed"
        );
    }

    preview_result
}
