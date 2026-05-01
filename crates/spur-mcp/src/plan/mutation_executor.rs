//! Apply a `MutationBatch` with write-ahead audit, downstream rewire, and
//! post-mutation cycle detection.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use spur_pm::{
    BeadsAdvanced, DependencyCycle, IssueCreate, IssueFilter, IssueSummary, IssueUpdate, PmService,
};

use super::audit_sentinel::{encode_comment as audit_encode, AuditSentinelKind, OpDescription};
use super::labels::{mutation_id_label, signal_processed_label, superseded_by_labels};
use super::mutation::{DepRewirePolicy, MutationBatch, PlanMutationOp};

const ISSUE_SCAN_PAGE_SIZE: usize = 500;

#[derive(Debug)]
struct SplitExecution {
    parent_id: String,
    original_parent_status: String,
    child_ids: Vec<String>,
    removed_parent_from: Vec<String>,
}

#[derive(Debug, Default)]
struct RollbackReport {
    succeeded: Vec<OpDescription>,
    failed: Vec<(OpDescription, String)>,
}

impl RollbackReport {
    fn record_success(&mut self, kind: &str, issue_id: &str, depends_on_id: Option<&str>) {
        self.succeeded
            .push(rollback_op(kind, issue_id, depends_on_id));
    }

    fn record_failure(
        &mut self,
        kind: &str,
        issue_id: &str,
        depends_on_id: Option<&str>,
        error: impl ToString,
    ) {
        self.failed.push((
            rollback_op(kind, issue_id, depends_on_id),
            error.to_string(),
        ));
    }
}

pub async fn apply_mutation(
    pm: Arc<PmService>,
    feature_gate: Arc<spur_license::FeatureGate>,
    batch: &MutationBatch,
) -> Result<Vec<String>> {
    crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate.as_ref(),
    )
    .map_err(|error| anyhow!(crate::server::feature_error_message(error)))?;
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
                let parent_plan_id = parent_issue
                    .labels
                    .iter()
                    .find_map(|label| super::labels::parse_plan_id(label))
                    .map(str::to_string);
                let parent_agent = parent_issue
                    .labels
                    .iter()
                    .find_map(|label| super::labels::parse_agent(label))
                    .map(str::to_string);
                if parent_plan_id.is_some() ^ parent_agent.is_some() {
                    anyhow::bail!(
                        "parent issue {parent} has inconsistent persisted scope labels (plan_id={:?}, agent={:?})",
                        parent_plan_id,
                        parent_agent
                    );
                }
                let parent_context_files = super::projector::latest_task_spec(
                    &super::projector::collect_sorted_audits_for_issue(
                        parent,
                        adv.list_comments(parent)
                            .await
                            .with_context(|| format!("list comments for parent issue {parent}"))?,
                    ),
                )
                .map(|(_, context_files)| context_files)
                .unwrap_or_default();
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
                    if let (Some(parent_plan_id), Some(parent_agent)) =
                        (parent_plan_id.as_deref(), parent_agent.as_deref())
                    {
                        add_labels_individually(
                            pm.as_ref(),
                            &id,
                            &[
                                super::labels::plan_id(parent_plan_id),
                                super::labels::plan_task_id(&id),
                                super::labels::agent(parent_agent),
                            ],
                        )
                        .await
                        .with_context(|| format!("persist plan scope on child {id}"))?;
                    }
                    if parent_plan_id.is_some() && !parent_context_files.is_empty() {
                        super::emit_task_spec_audit(adv, &id, &id, &parent_context_files)
                            .await
                            .with_context(|| format!("persist child task spec {id}"))?;
                    }
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
        let rollback_report =
            rollback_mutation(pm.clone(), Arc::clone(&feature_gate), &executed_splits).await;
        let rollback_status = if rollback_report.failed.is_empty() {
            "completed".to_string()
        } else {
            format!(
                "failed: {} rollback compensation op(s) failed",
                rollback_report.failed.len()
            )
        };

        adv.add_comment(
            &batch.trigger_task_id,
            &audit_encode(&AuditSentinelKind::MutationInvariantViolation {
                mutation_id: batch.mutation_id.to_string(),
                violation: "cycle".into(),
                rollback_status,
                rollback_ops_succeeded: rollback_report.succeeded.clone(),
                rollback_ops_failed: rollback_report.failed.clone(),
            }),
        )
        .await
        .context("emit mutation-invariant-violation")?;

        if !rollback_report.failed.is_empty() {
            let rollback_failure = rollback_report
                .failed
                .iter()
                .map(|(op, err)| {
                    let dependency = op
                        .depends_on_id
                        .as_deref()
                        .map(|dep| format!(" -> {dep}"))
                        .unwrap_or_default();
                    format!("{} {}{}: {}", op.kind, op.issue_id, dependency, err)
                })
                .collect::<Vec<_>>()
                .join("; ");
            anyhow::bail!(
                "mutation {} rolled back after cycle detection but rollback failed: {}",
                batch.mutation_id,
                rollback_failure
            );
        }

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

    if let Some(signal_id) = batch.trigger_signal_id {
        pm.update_issue(
            &batch.trigger_task_id,
            IssueUpdate {
                add_labels: vec![signal_processed_label(&signal_id)],
                ..Default::default()
            },
        )
        .await
        .context("mark triggering signal processed")?;
    }

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

async fn rollback_mutation(
    pm: Arc<PmService>,
    feature_gate: Arc<spur_license::FeatureGate>,
    executed_splits: &[SplitExecution],
) -> RollbackReport {
    let mut report = RollbackReport::default();
    if let Err(error) = crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate.as_ref(),
    ) {
        if let Some(split) = executed_splits.last() {
            report.record_failure(
                "rollback_setup",
                &split.parent_id,
                None,
                crate::server::feature_error_message(error),
            );
        }
        return report;
    }
    let Some(adv) = pm.advanced() else {
        if let Some(split) = executed_splits.last() {
            report.record_failure(
                "rollback_setup",
                &split.parent_id,
                None,
                "rollback requires beads backend",
            );
        }
        return report;
    };

    for split in executed_splits.iter().rev() {
        match dependencies_touching_children(pm.as_ref(), &split.child_ids).await {
            Ok(pairs) => {
                for (issue_id, depends_on_id) in pairs {
                    match adv.remove_dependency(&issue_id, &depends_on_id).await {
                        Ok(()) => {
                            report.record_success(
                                "remove_dependency",
                                &issue_id,
                                Some(&depends_on_id),
                            );
                        }
                        Err(error) => {
                            report.record_failure(
                                "remove_dependency",
                                &issue_id,
                                Some(&depends_on_id),
                                format!("{error:#}"),
                            );
                        }
                    }
                }
            }
            Err(error) => {
                report.record_failure(
                    "remove_dependency",
                    &split.parent_id,
                    None,
                    format!("scan rollback dependencies: {error:#}"),
                );
            }
        }

        for downstream in &split.removed_parent_from {
            match pm.add_dependency(downstream, &split.parent_id).await {
                Ok(()) => {
                    report.record_success("restore_dependency", downstream, Some(&split.parent_id));
                }
                Err(error) => {
                    report.record_failure(
                        "restore_dependency",
                        downstream,
                        Some(&split.parent_id),
                        format!(
                            "restore original downstream dependency {downstream} -> {}: {error:#}",
                            split.parent_id
                        ),
                    );
                }
            }
        }

        for child_id in &split.child_ids {
            match pm
                .update_issue(
                    child_id,
                    IssueUpdate {
                        status: Some(pm.closed_status().to_string()),
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(()) => report.record_success("close_child_issue", child_id, None),
                Err(error) => {
                    report.record_failure(
                        "close_child_issue",
                        child_id,
                        None,
                        format!("close rolled-back child {child_id}: {error:#}"),
                    );
                }
            }
        }

        match pm
            .update_issue(
                &split.parent_id,
                IssueUpdate {
                    status: Some(split.original_parent_status.clone()),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(()) => report.record_success("restore_parent_status", &split.parent_id, None),
            Err(error) => {
                report.record_failure(
                    "restore_parent_status",
                    &split.parent_id,
                    None,
                    format!("restore parent {}: {error:#}", split.parent_id),
                );
            }
        }
        for label in superseded_by_labels(&split.child_ids) {
            let child_id = label
                .strip_prefix("spur:superseded-by:")
                .unwrap_or_default()
                .to_string();
            match pm
                .update_issue(
                    &split.parent_id,
                    IssueUpdate {
                        remove_labels: vec![label],
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(()) => {
                    report.record_success(
                        "clear_superseded_by_label",
                        &split.parent_id,
                        Some(&child_id),
                    );
                }
                Err(error) => {
                    report.record_failure(
                        "clear_superseded_by_label",
                        &split.parent_id,
                        Some(&child_id),
                        format!(
                            "clear superseded-by labels from {}: {error:#}",
                            split.parent_id
                        ),
                    );
                }
            }
        }
    }

    report
}

fn rollback_op(kind: &str, issue_id: &str, depends_on_id: Option<&str>) -> OpDescription {
    OpDescription {
        kind: kind.to_string(),
        issue_id: issue_id.to_string(),
        depends_on_id: depends_on_id.map(str::to_string),
    }
}

async fn dependencies_touching_children(
    pm: &PmService,
    child_ids: &[String],
) -> Result<Vec<(String, String)>> {
    let child_set: HashSet<&str> = child_ids.iter().map(String::as_str).collect();
    let issues = list_all_issue_ids(pm).await?;
    let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();

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

    Ok(pairs.into_iter().collect())
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

fn apply_issue_scan_page(
    out: &mut Vec<String>,
    offset: usize,
    page: Vec<IssueSummary>,
) -> Option<usize> {
    let page_len = page.len();
    out.extend(page.into_iter().map(|issue| issue.id));
    if page_len < ISSUE_SCAN_PAGE_SIZE {
        None
    } else {
        Some(offset + page_len)
    }
}

async fn list_all_issue_ids(pm: &PmService) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    loop {
        let page = pm
            .list_issues(IssueFilter {
                limit: Some(ISSUE_SCAN_PAGE_SIZE),
                offset: Some(offset),
                ..Default::default()
            })
            .await
            .context("list issues for mutation scan page")?;
        match apply_issue_scan_page(&mut out, offset, page) {
            Some(next_offset) => offset = next_offset,
            None => break,
        }
    }
    Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;
    use spur_pm::{IssueSummary, PmSource};

    fn summary(id: &str) -> IssueSummary {
        IssueSummary {
            id: id.to_string(),
            source: PmSource::Beads,
            title: format!("Issue {id}"),
            status: "open".into(),
            labels: Vec::new(),
            url: format!("beads://{id}"),
            priority: None,
            issue_type: Some("task".into()),
            assignee: None,
        }
    }

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

    #[test]
    fn apply_issue_scan_page_advances_after_full_page() {
        let mut out = Vec::new();
        let page = (0..ISSUE_SCAN_PAGE_SIZE)
            .map(|idx| summary(&format!("bd-{idx}")))
            .collect();

        let next = apply_issue_scan_page(&mut out, 0, page);

        assert_eq!(out.len(), ISSUE_SCAN_PAGE_SIZE);
        assert_eq!(next, Some(ISSUE_SCAN_PAGE_SIZE));
    }

    #[test]
    fn apply_issue_scan_page_stops_after_tail_page() {
        let mut out = Vec::new();
        let full_page = (0..ISSUE_SCAN_PAGE_SIZE)
            .map(|idx| summary(&format!("bd-{idx}")))
            .collect();
        let tail_page = vec![summary("bd-tail-1"), summary("bd-tail-2")];

        let next = apply_issue_scan_page(&mut out, 0, full_page);
        assert_eq!(next, Some(ISSUE_SCAN_PAGE_SIZE));

        let final_next = apply_issue_scan_page(&mut out, next.unwrap(), tail_page);

        assert_eq!(final_next, None);
        assert_eq!(out.len(), ISSUE_SCAN_PAGE_SIZE + 2);
        assert_eq!(out.last().map(|id| id.as_str()), Some("bd-tail-2"));
    }

    #[test]
    fn apply_issue_scan_page_handles_empty_page() {
        let mut out = Vec::new();

        let next = apply_issue_scan_page(&mut out, 0, Vec::new());

        assert_eq!(next, None);
        assert!(out.is_empty());
    }
}
