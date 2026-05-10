use std::path::Path;

pub(super) fn is_hex_oid(spec: &str) -> bool {
    spec.len() == 40 && spec.chars().all(|ch| ch.is_ascii_hexdigit())
}

pub(super) async fn git_rev_parse(repo_root: &Path, spec: &str) -> anyhow::Result<String> {
    let output = tokio::process::Command::new("git")
        .args(["rev-parse", spec])
        .current_dir(repo_root)
        .output()
        .await
        .map_err(|error| anyhow::anyhow!("failed to execute git rev-parse {spec}: {error}"))?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    if is_hex_oid(spec) {
        return Ok(spec.to_string());
    }

    anyhow::bail!(
        "git rev-parse {spec} failed (exit {}): {}",
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

pub(super) async fn plan_dispatch_base_spec(
    plan_state: &crate::plan::PlanState,
    task_id: &str,
    repo_root: &Path,
) -> anyhow::Result<crate::tools::BaseSpec> {
    if let Some(name) = single_parent_approved_worker_branch(plan_state, task_id) {
        return Ok(crate::tools::BaseSpec::Branch { name });
    }

    let dep_closure = plan_state.approved_dep_closure(task_id);
    let mut overlays = Vec::with_capacity(dep_closure.len());

    for dep in dep_closure {
        let base_oid = dep.dispatched_base_oid.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "approved dependency {} is missing dispatched_base_oid",
                dep.spec.task_id
            )
        })?;
        let worker_branch = dep.worker_branch.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "approved dependency {} is missing worker_branch",
                dep.spec.task_id
            )
        })?;
        let tip_oid = git_rev_parse(repo_root, worker_branch).await?;
        overlays.push(crate::tools::OverlayCommit {
            source_task_id: dep.spec.task_id.clone(),
            base_oid,
            tip_oid,
        });
    }

    Ok(crate::tools::BaseSpec::WithOverlay {
        base: crate::tools::BaseTarget::Branch {
            name: plan_state
                .base_snapshot_branch
                .clone()
                .unwrap_or_else(|| "HEAD".to_string()),
        },
        overlays,
    })
}

pub(super) fn single_parent_approved_worker_branch(
    plan_state: &crate::plan::PlanState,
    task_id: &str,
) -> Option<String> {
    let task = plan_state
        .tasks
        .iter()
        .find(|entry| entry.spec.task_id == task_id)?;
    let [dep_task_id] = task.spec.depends_on.as_slice() else {
        return None;
    };
    let dep = plan_state
        .tasks
        .iter()
        .find(|entry| entry.spec.task_id == *dep_task_id)?;

    if !matches!(dep.status, crate::plan::PlanTaskStatus::Approved { .. }) {
        return None;
    }

    dep.worker_branch
        .as_deref()
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned)
}
