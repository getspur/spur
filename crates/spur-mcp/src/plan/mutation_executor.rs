//! Apply a `MutationBatch` with write-ahead audit, downstream rewire, and
//! post-mutation cycle detection.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use spur_pm::{BeadsAdvanced, DependencyCycle, IssueCreate, IssueFilter, IssueUpdate, PmService};

use super::audit_sentinel::{encode_comment as audit_encode, AuditSentinelKind};
use super::labels::{mutation_id_label, signal_processed_label, superseded_by_labels};
use super::mutation::{DepRewirePolicy, MutationBatch, PlanMutationOp};

const ISSUE_SCAN_LIMIT: usize = 10_000;

#[derive(Debug)]
struct SplitExecution {
    parent_id: String,
    original_parent_status: String,
    child_ids: Vec<String>,
    removed_parent_from: Vec<String>,
}

pub async fn apply_mutation(pm: Arc<PmService>, batch: &MutationBatch) -> Result<Vec<String>> {
    let adv = pm
        .advanced()
        .ok_or_else(|| anyhow!("mutation requires beads backend"))?;

    adv.add_comment(
        &batch.trigger_task_id,
        &audit_encode(&AuditSentinelKind::MutationPlan {
            mutation_id: batch.mutation_id.to_string(),
            op: batch.op_tag().to_string(),
            trigger_signal_id: batch.trigger_signal_id.map(|u| u.to_string()),
            trigger_task_id: batch.trigger_task_id.clone(),
        }),
    )
    .await
    .context("mutation-plan audit write-ahead")?;

    let mut children_created: Vec<String> = Vec::new();
    let mut executed_splits: Vec<SplitExecution> = Vec::new();

    for op in &batch.ops {
        match op {
            PlanMutationOp::SplitTask {
                parent,
                children,
                dep_rewire,
            } => {
                let parent_issue = pm
                    .get_issue(parent)
                    .await
                    .with_context(|| format!("load parent issue {parent}"))?;
                let original_downstreams = downstream_issue_ids(pm.as_ref(), parent).await?;

                let mut child_ids: Vec<String> = Vec::with_capacity(children.len());
                for draft in children {
                    let id = pm
                        .create_issue(IssueCreate {
                            title: draft.title.clone(),
                            description: Some(draft.description.clone()),
                            issue_type: Some("task".into()),
                            parent: None,
                            assignee: draft.assignee.clone(),
                            priority: draft.priority,
                            labels: vec![mutation_id_label(&batch.mutation_id)],
                            ..Default::default()
                        })
                        .await
                        .context("create child issue")?;
                    child_ids.push(id);
                }
                children_created.extend(child_ids.iter().cloned());
                for child_id in &child_ids {
                    pm.add_dependency(child_id, parent)
                        .await
                        .with_context(|| format!("add child dependency {child_id} -> {parent}"))?;
                }

                let rewire_plan = rewire_plan(dep_rewire, &original_downstreams, &child_ids)
                    .with_context(|| {
                        format!(
                            "build rewire plan for parent {parent} and mutation {}",
                            batch.mutation_id
                        )
                    })?;

                for (issue_id, depends_on_id) in &rewire_plan.inter_child_edges {
                    pm.add_dependency(issue_id, depends_on_id)
                        .await
                        .with_context(|| {
                            format!("add inter-child dependency {issue_id} -> {depends_on_id}")
                        })?;
                }

                for downstream in &rewire_plan.removed_parent_from {
                    adv.remove_dependency(downstream, parent)
                        .await
                        .with_context(|| {
                            format!("remove downstream dependency {downstream} -> {parent}")
                        })?;
                }

                for (issue_id, depends_on_id) in &rewire_plan.downstream_edges {
                    pm.add_dependency(issue_id, depends_on_id)
                        .await
                        .with_context(|| {
                            format!("add downstream dependency {issue_id} -> {depends_on_id}")
                        })?;
                }

                pm.update_issue(
                    parent,
                    IssueUpdate {
                        status: Some(pm.closed_status().to_string()),
                        ..Default::default()
                    },
                )
                .await
                .with_context(|| format!("mark parent {parent} as superseded"))?;
                add_labels_individually(pm.as_ref(), parent, &superseded_by_labels(&child_ids))
                    .await
                    .with_context(|| format!("label parent {parent} with superseded-by markers"))?;

                executed_splits.push(SplitExecution {
                    parent_id: parent.clone(),
                    original_parent_status: parent_issue.status,
                    child_ids,
                    removed_parent_from: rewire_plan.removed_parent_from,
                });
            }
        }
    }

    let cycles = dep_cycles_with_fallback(adv).await?;
    if !cycles.is_empty() {
        let rollback_status = match rollback_mutation(pm.clone(), &executed_splits).await {
            Ok(()) => "completed".to_string(),
            Err(err) => {
                let status = format!("failed: {err}");
                adv.add_comment(
                    &batch.trigger_task_id,
                    &audit_encode(&AuditSentinelKind::MutationInvariantViolation {
                        mutation_id: batch.mutation_id.to_string(),
                        violation: "cycle".into(),
                        rollback_status: status.clone(),
                    }),
                )
                .await
                .context("emit mutation-invariant-violation after rollback failure")?;
                anyhow::bail!(
                    "mutation {} rolled back after cycle detection but rollback failed: {err}",
                    batch.mutation_id
                );
            }
        };

        adv.add_comment(
            &batch.trigger_task_id,
            &audit_encode(&AuditSentinelKind::MutationInvariantViolation {
                mutation_id: batch.mutation_id.to_string(),
                violation: "cycle".into(),
                rollback_status,
            }),
        )
        .await
        .context("emit mutation-invariant-violation")?;

        anyhow::bail!("mutation {} rolled back: cycle detected", batch.mutation_id);
    }

    adv.add_comment(
        &batch.trigger_task_id,
        &audit_encode(&AuditSentinelKind::MutationCommit {
            mutation_id: batch.mutation_id.to_string(),
            children_created: children_created.clone(),
        }),
    )
    .await
    .context("emit mutation-commit audit")?;

    pm.update_issue(
        &batch.trigger_task_id,
        IssueUpdate {
            add_labels: vec![signal_processed_label(&batch.mutation_id)],
            ..Default::default()
        },
    )
    .await
    .context("mark triggering signal processed")?;

    Ok(children_created)
}

#[derive(Debug)]
struct RewirePlan {
    inter_child_edges: Vec<(String, String)>,
    removed_parent_from: Vec<String>,
    downstream_edges: Vec<(String, String)>,
}

fn rewire_plan(
    dep_rewire: &DepRewirePolicy,
    original_downstreams: &[String],
    child_ids: &[String],
) -> Result<RewirePlan> {
    let mut inter_child_edges = Vec::new();
    let mut removed_parent_from = Vec::new();
    let mut downstream_edges = Vec::new();

    match dep_rewire {
        DepRewirePolicy::Pipeline { tail_index } => {
            let tail = child_ids
                .get(*tail_index)
                .ok_or_else(|| anyhow!("pipeline tail_index out of range"))?
                .clone();

            for window in child_ids.windows(2) {
                inter_child_edges.push((window[1].clone(), window[0].clone()));
            }

            removed_parent_from.extend(original_downstreams.iter().cloned());
            for downstream in original_downstreams {
                downstream_edges.push((downstream.clone(), tail.clone()));
            }
        }
        DepRewirePolicy::Barrier => {
            removed_parent_from.extend(original_downstreams.iter().cloned());
            for downstream in original_downstreams {
                for child_id in child_ids {
                    downstream_edges.push((downstream.clone(), child_id.clone()));
                }
            }
        }
        DepRewirePolicy::Explicit { edges } => {
            let original_downstreams: HashSet<&str> =
                original_downstreams.iter().map(String::as_str).collect();
            let mut removed: HashSet<String> = HashSet::new();

            for (child_idx, downstream) in edges {
                let child_id = child_ids
                    .get(*child_idx)
                    .ok_or_else(|| anyhow!("explicit edge child_idx out of range"))?
                    .clone();
                if original_downstreams.contains(downstream.as_str()) {
                    removed.insert(downstream.clone());
                }
                downstream_edges.push((downstream.clone(), child_id));
            }

            removed_parent_from.extend(removed);
            removed_parent_from.sort();
        }
    }

    Ok(RewirePlan {
        inter_child_edges,
        removed_parent_from,
        downstream_edges,
    })
}

async fn rollback_mutation(pm: Arc<PmService>, executed_splits: &[SplitExecution]) -> Result<()> {
    let adv = pm
        .advanced()
        .ok_or_else(|| anyhow!("rollback requires beads backend"))?;

    for split in executed_splits.iter().rev() {
        remove_dependencies_touching_children(pm.as_ref(), adv, &split.child_ids).await?;

        for downstream in &split.removed_parent_from {
            pm.add_dependency(downstream, &split.parent_id)
                .await
                .with_context(|| {
                    format!(
                        "restore original downstream dependency {downstream} -> {}",
                        split.parent_id
                    )
                })?;
        }

        for child_id in &split.child_ids {
            pm.update_issue(
                child_id,
                IssueUpdate {
                    status: Some(pm.closed_status().to_string()),
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("close rolled-back child {child_id}"))?;
        }

        pm.update_issue(
            &split.parent_id,
            IssueUpdate {
                status: Some(split.original_parent_status.clone()),
                ..Default::default()
            },
        )
        .await
        .with_context(|| format!("restore parent {}", split.parent_id))?;
        remove_labels_individually(
            pm.as_ref(),
            &split.parent_id,
            &superseded_by_labels(&split.child_ids),
        )
        .await
        .with_context(|| format!("clear superseded-by labels from {}", split.parent_id))?;
    }

    Ok(())
}

async fn remove_dependencies_touching_children(
    pm: &PmService,
    adv: &dyn BeadsAdvanced,
    child_ids: &[String],
) -> Result<()> {
    let child_set: HashSet<&str> = child_ids.iter().map(String::as_str).collect();
    let issues = list_all_issue_ids(pm).await?;
    let mut pairs: HashSet<(String, String)> = HashSet::new();

    for issue_id in issues {
        let issue = pm
            .get_issue(&issue_id)
            .await
            .with_context(|| format!("load issue {issue_id} during rollback"))?;
        for dep in issue.blocked_by {
            if child_set.contains(issue.id.as_str()) || child_set.contains(dep.as_str()) {
                pairs.insert((issue.id.clone(), dep));
            }
        }
    }

    for (issue_id, depends_on_id) in pairs {
        adv.remove_dependency(&issue_id, &depends_on_id)
            .await
            .with_context(|| format!("remove rollback dependency {issue_id} -> {depends_on_id}"))?;
    }

    Ok(())
}

async fn downstream_issue_ids(pm: &PmService, parent_id: &str) -> Result<Vec<String>> {
    let mut downstreams = Vec::new();
    for issue_id in list_all_issue_ids(pm).await? {
        if issue_id == parent_id {
            continue;
        }
        let issue = pm
            .get_issue(&issue_id)
            .await
            .with_context(|| format!("load issue {issue_id} while scanning downstream deps"))?;
        if issue.blocked_by.iter().any(|dep| dep == parent_id) {
            downstreams.push(issue.id);
        }
    }
    Ok(downstreams)
}

async fn list_all_issue_ids(pm: &PmService) -> Result<Vec<String>> {
    Ok(pm
        .list_issues(IssueFilter {
            limit: Some(ISSUE_SCAN_LIMIT),
            ..Default::default()
        })
        .await
        .context("list issues for mutation scan")?
        .into_iter()
        .map(|issue| issue.id)
        .collect())
}

async fn dep_cycles_with_fallback(adv: &dyn BeadsAdvanced) -> Result<Vec<DependencyCycle>> {
    match adv.dep_cycles().await {
        Ok(cycles) => Ok(cycles),
        Err(err) => {
            let err_text = err.to_string();
            let Some(raw_json) = err_text.split("\nraw: ").nth(1) else {
                return Err(err).context("dep_cycles check");
            };
            let parsed: Value =
                serde_json::from_str(raw_json).context("dep_cycles fallback JSON parse")?;
            let Some(raw_cycles) = parsed.get("cycles").and_then(Value::as_array) else {
                return Err(err).context("dep_cycles check");
            };

            let cycles = raw_cycles
                .iter()
                .filter_map(Value::as_array)
                .map(|cycle| DependencyCycle {
                    issues: cycle
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect(),
                })
                .collect::<Vec<_>>();
            Ok(cycles)
        }
    }
}

async fn add_labels_individually(pm: &PmService, issue_id: &str, labels: &[String]) -> Result<()> {
    for label in labels {
        pm.update_issue(
            issue_id,
            IssueUpdate {
                add_labels: vec![label.clone()],
                ..Default::default()
            },
        )
        .await
        .with_context(|| format!("add label {label} to {issue_id}"))?;
    }
    Ok(())
}

async fn remove_labels_individually(
    pm: &PmService,
    issue_id: &str,
    labels: &[String],
) -> Result<()> {
    for label in labels {
        pm.update_issue(
            issue_id,
            IssueUpdate {
                remove_labels: vec![label.clone()],
                ..Default::default()
            },
        )
        .await
        .with_context(|| format!("remove label {label} from {issue_id}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn barrier_rewire_replaces_parent_for_all_downstreams() {
        let plan = rewire_plan(
            &DepRewirePolicy::Barrier,
            &["bd-4".into(), "bd-5".into()],
            &["bd-2".into(), "bd-3".into()],
        )
        .unwrap();

        assert_eq!(
            plan.removed_parent_from,
            vec!["bd-4".to_string(), "bd-5".to_string()]
        );
        assert_eq!(
            plan.downstream_edges,
            vec![
                ("bd-4".to_string(), "bd-2".to_string()),
                ("bd-4".to_string(), "bd-3".to_string()),
                ("bd-5".to_string(), "bd-2".to_string()),
                ("bd-5".to_string(), "bd-3".to_string()),
            ]
        );
    }

    #[test]
    fn explicit_rewire_validates_child_index() {
        let err = rewire_plan(
            &DepRewirePolicy::Explicit {
                edges: vec![(2, "bd-4".into())],
            },
            &["bd-4".into()],
            &["bd-2".into(), "bd-3".into()],
        )
        .unwrap_err();

        assert!(err.to_string().contains("out of range"));
    }
}
