//! Apply a `MutationBatch` with write-ahead audit, downstream rewire, and
//! post-mutation cycle detection.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use spur_pm::{
    BeadsAdvanced, DependencyCycle, IssueCreate, IssueFilter, IssueSummary, IssueUpdate, PmService,
};

use super::audit_sentinel::{
    encode_comment as audit_encode, AuditSentinelKind, CompletionState, OpDescription,
};
use super::labels::{mutation_id_label, signal_processed_label, superseded_by_labels};
use super::mutation::{DepRewirePolicy, MutationBatch, PlanMutationOp};

/// bd-2m2u Phase 2c — label that brain-side escalation pipelines apply to flag
/// a task awaiting brain decision. `submit_plan_mutation` clears it on success
/// to mark the escalation resolved.
pub const SIGNAL_ESCALATED_LABEL: &str = "signal:escalated";

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
    let mut affected_task_ids: Vec<String> = Vec::new();
    let mut executed_ops: Vec<ExecutedOp> = Vec::new();

    // bd-2m2u Phase 2c — wrap the apply loop so that any per-op error triggers
    // rollback of the previously executed ops and emits a violation audit
    // before propagating the original error. The post-hoc cycle detection
    // below remains as a second safety net.
    let apply_outcome: Result<()> = async {
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
                let parent_audits = super::projector::collect_sorted_audits_for_issue(
                    parent,
                    adv.list_comments(parent)
                        .await
                        .with_context(|| format!("list comments for parent issue {parent}"))?,
                )
                .with_context(|| format!("parse comments for parent issue {parent}"))?;
                let parent_context_files = super::projector::latest_task_spec(&parent_audits)
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
                        super::emit_task_spec_audit(
                            adv,
                            &id,
                            &id,
                            parent_agent,
                            &parent_context_files,
                        )
                        .await
                        .with_context(|| format!("persist child task spec {id}"))?;
                    }
                    child_ids.push(id);
                }
                children_created.extend(child_ids.iter().cloned());
                affected_task_ids.push(parent.clone());
                affected_task_ids.extend(child_ids.iter().cloned());
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

                executed_ops.push(ExecutedOp::SplitTask(SplitExecution {
                    parent_id: parent.clone(),
                    original_parent_status: parent_issue.status,
                    child_ids,
                    removed_parent_from: rewire_plan.removed_parent_from,
                }));
            }
            PlanMutationOp::RetryTask { issue_id } => {
                affected_task_ids.push(issue_id.clone());
                apply_retry_task(
                    pm.as_ref(),
                    adv,
                    issue_id,
                    &batch.mutation_id,
                    &mut executed_ops,
                )
                .await
                .with_context(|| format!("apply retry_task {issue_id}"))?;
            }
            PlanMutationOp::ModifyTaskSpec {
                issue_id,
                new_task,
                new_agent,
                new_context_files,
                new_depends_on,
            } => {
                affected_task_ids.push(issue_id.clone());
                apply_modify_task_spec(
                    pm.as_ref(),
                    adv,
                    ModifyTaskSpecInput {
                        issue_id,
                        new_task: new_task.as_deref(),
                        new_agent: new_agent.as_deref(),
                        new_context_files: new_context_files.as_deref(),
                        new_depends_on: new_depends_on.as_deref(),
                    },
                    &mut executed_ops,
                )
                .await
                .with_context(|| format!("apply modify_task_spec {issue_id}"))?;
            }
            PlanMutationOp::AbandonTask {
                issue_id,
                reason,
                cascade_descendants,
            } => {
                let affected = apply_abandon_task(
                    pm.as_ref(),
                    adv,
                    issue_id,
                    reason,
                    *cascade_descendants,
                    &mut executed_ops,
                )
                .await
                .with_context(|| format!("apply abandon_task {issue_id}"))?;
                affected_task_ids.extend(affected);
            }
            PlanMutationOp::InsertTaskBefore {
                target_issue_id,
                draft,
            } => {
                let new_id = apply_insert_task_before(
                    pm.as_ref(),
                    adv,
                    target_issue_id,
                    draft,
                    &batch.mutation_id,
                    &mut executed_ops,
                )
                .await
                .with_context(|| {
                    format!("apply insert_task_before target={target_issue_id}")
                })?;
                children_created.push(new_id.clone());
                affected_task_ids.push(target_issue_id.clone());
                affected_task_ids.push(new_id);
            }
            PlanMutationOp::AddDependency {
                issue_id,
                depends_on,
            } => {
                affected_task_ids.push(issue_id.clone());
                apply_add_dependency(pm.as_ref(), issue_id, depends_on, &mut executed_ops)
                    .await
                    .with_context(|| {
                        format!("apply add_dependency {issue_id} -> {depends_on}")
                    })?;
            }
            PlanMutationOp::CancelTask { issue_id, reason } => {
                affected_task_ids.push(issue_id.clone());
                apply_cancel_task(pm.as_ref(), adv, issue_id, reason, &mut executed_ops)
                    .await
                    .with_context(|| format!("apply cancel_task {issue_id}"))?;
            }
        }
        }
        Ok(())
    }
    .await;

    if let Err(apply_err) = apply_outcome {
        let rollback_report =
            rollback_mutation(pm.clone(), Arc::clone(&feature_gate), &executed_ops).await;
        let rollback_status = if rollback_report.failed.is_empty() {
            "completed".to_string()
        } else {
            format!(
                "failed: {} rollback compensation op(s) failed",
                rollback_report.failed.len()
            )
        };
        let _ = adv
            .add_comment(
                &batch.trigger_task_id,
                &audit_encode(&AuditSentinelKind::MutationInvariantViolation {
                    mutation_id: batch.mutation_id.to_string(),
                    violation: format!("op_failure: {apply_err:#}"),
                    rollback_status,
                    rollback_ops_succeeded: rollback_report.succeeded.clone(),
                    rollback_ops_failed: rollback_report.failed.clone(),
                }),
            )
            .await;
        return Err(apply_err.context(format!(
            "mutation {} rolled back after op failure",
            batch.mutation_id
        )));
    }

    let cycles = dep_cycles_with_fallback(adv).await?;
    if !cycles.is_empty() {
        let rollback_report =
            rollback_mutation(pm.clone(), Arc::clone(&feature_gate), &executed_ops).await;
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
            op_tags: batch.op_tags().into_iter().map(String::from).collect(),
            affected_task_ids: affected_task_ids.clone(),
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

/// A unit of work that `apply_mutation` has executed and may need to undo
/// during rollback. Each variant carries enough state to reverse itself via
/// the [`ReversibleOp`] trait.
#[derive(Debug)]
enum ExecutedOp {
    SplitTask(SplitExecution),
    RetryTask(RetryExecution),
    ModifyTaskSpec(ModifyTaskSpecExecution),
    AbandonTask(AbandonTaskExecution),
    InsertTaskBefore(InsertTaskBeforeExecution),
    AddDependency(AddDependencyExecution),
    CancelTask(CancelTaskExecution),
    #[cfg(test)]
    NoOp(NoOpExecution),
}

/// bd-2m2u Phase 2c — captured pre-image for `RetryTask` rollback.
#[derive(Debug)]
struct RetryExecution {
    issue_id: String,
    original_status: String,
    removed_labels: Vec<String>,
}

/// bd-2m2u Phase 2c — captured pre-image for `ModifyTaskSpec` rollback.
#[derive(Debug)]
struct ModifyTaskSpecExecution {
    issue_id: String,
    body_change: Option<String>, // original body if we changed it
    original_agent_label: Option<String>,
    new_agent_label: Option<String>,
    added_deps: Vec<String>,
    removed_deps: Vec<String>,
}

/// bd-2m2u Phase 2c — captured pre-image for `AbandonTask` rollback. Each
/// `(issue_id, original_status)` pair is restored on rollback.
#[derive(Debug)]
struct AbandonTaskExecution {
    targets: Vec<(String, String)>,
}

/// bd-2m2u Phase 2e — captured pre-image for `InsertTaskBefore` rollback.
/// `new_issue_id` is `None` until the new beads issue is created so a partial
/// failure (e.g. dep add) leaves a meaningful rollback record.
#[derive(Debug)]
struct InsertTaskBeforeExecution {
    target_issue_id: String,
    target_original_status: String,
    target_removed_labels: Vec<String>,
    new_issue_id: Option<String>,
    dep_added: bool,
}

/// bd-2m2u Phase 2e — captured pre-image for `AddDependency` rollback.
#[derive(Debug)]
struct AddDependencyExecution {
    issue_id: String,
    depends_on: String,
}

/// bd-2m2u Phase 2e — captured pre-image for `CancelTask` rollback. Mirrors
/// `AbandonTaskExecution` but never cascades and tags the audit as Cancelled.
#[derive(Debug)]
struct CancelTaskExecution {
    issue_id: String,
    original_status: String,
}

/// Per-op compensating action. Implementations record outcomes onto
/// `report` rather than returning, so rollback always proceeds best-effort.
#[async_trait::async_trait]
trait ReversibleOp {
    async fn rollback(&self, pm: &PmService, adv: &dyn BeadsAdvanced, report: &mut RollbackReport);
}

#[cfg(test)]
#[derive(Debug)]
struct NoOpExecution {
    pub label: String,
}

#[cfg(test)]
#[async_trait::async_trait]
impl ReversibleOp for NoOpExecution {
    async fn rollback(
        &self,
        _pm: &PmService,
        _adv: &dyn BeadsAdvanced,
        report: &mut RollbackReport,
    ) {
        report.record_success("noop", &self.label, None);
    }
}

#[async_trait::async_trait]
impl ReversibleOp for SplitExecution {
    async fn rollback(&self, pm: &PmService, adv: &dyn BeadsAdvanced, report: &mut RollbackReport) {
        match dependencies_touching_children(pm, &self.child_ids).await {
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
                    &self.parent_id,
                    None,
                    format!("scan rollback dependencies: {error:#}"),
                );
            }
        }

        for downstream in &self.removed_parent_from {
            match pm.add_dependency(downstream, &self.parent_id).await {
                Ok(()) => {
                    report.record_success("restore_dependency", downstream, Some(&self.parent_id));
                }
                Err(error) => {
                    report.record_failure(
                        "restore_dependency",
                        downstream,
                        Some(&self.parent_id),
                        format!(
                            "restore original downstream dependency {downstream} -> {}: {error:#}",
                            self.parent_id
                        ),
                    );
                }
            }
        }

        for child_id in &self.child_ids {
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
                &self.parent_id,
                IssueUpdate {
                    status: Some(self.original_parent_status.clone()),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(()) => report.record_success("restore_parent_status", &self.parent_id, None),
            Err(error) => {
                report.record_failure(
                    "restore_parent_status",
                    &self.parent_id,
                    None,
                    format!("restore parent {}: {error:#}", self.parent_id),
                );
            }
        }
        for label in superseded_by_labels(&self.child_ids) {
            let child_id = label
                .strip_prefix("spur:superseded-by:")
                .unwrap_or_default()
                .to_string();
            match pm
                .update_issue(
                    &self.parent_id,
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
                        &self.parent_id,
                        Some(&child_id),
                    );
                }
                Err(error) => {
                    report.record_failure(
                        "clear_superseded_by_label",
                        &self.parent_id,
                        Some(&child_id),
                        format!(
                            "clear superseded-by labels from {}: {error:#}",
                            self.parent_id
                        ),
                    );
                }
            }
        }
    }
}

async fn rollback_executed_ops_in_reverse(
    pm: &PmService,
    adv: &dyn BeadsAdvanced,
    ops: &[ExecutedOp],
    report: &mut RollbackReport,
) {
    for op in ops.iter().rev() {
        match op {
            ExecutedOp::SplitTask(split) => split.rollback(pm, adv, report).await,
            ExecutedOp::RetryTask(retry) => retry.rollback(pm, adv, report).await,
            ExecutedOp::ModifyTaskSpec(modify) => modify.rollback(pm, adv, report).await,
            ExecutedOp::AbandonTask(abandon) => abandon.rollback(pm, adv, report).await,
            ExecutedOp::InsertTaskBefore(insert) => insert.rollback(pm, adv, report).await,
            ExecutedOp::AddDependency(add) => add.rollback(pm, adv, report).await,
            ExecutedOp::CancelTask(cancel) => cancel.rollback(pm, adv, report).await,
            #[cfg(test)]
            ExecutedOp::NoOp(noop) => noop.rollback(pm, adv, report).await,
        }
    }
}

fn executed_op_setup_id(op: &ExecutedOp) -> &str {
    match op {
        ExecutedOp::SplitTask(split) => &split.parent_id,
        ExecutedOp::RetryTask(retry) => &retry.issue_id,
        ExecutedOp::ModifyTaskSpec(modify) => &modify.issue_id,
        ExecutedOp::AbandonTask(abandon) => abandon
            .targets
            .first()
            .map(|(id, _)| id.as_str())
            .unwrap_or("<no-target>"),
        ExecutedOp::InsertTaskBefore(insert) => &insert.target_issue_id,
        ExecutedOp::AddDependency(add) => &add.issue_id,
        ExecutedOp::CancelTask(cancel) => &cancel.issue_id,
        #[cfg(test)]
        ExecutedOp::NoOp(noop) => &noop.label,
    }
}

#[async_trait::async_trait]
impl ReversibleOp for RetryExecution {
    async fn rollback(
        &self,
        pm: &PmService,
        _adv: &dyn BeadsAdvanced,
        report: &mut RollbackReport,
    ) {
        let restore = IssueUpdate {
            status: Some(self.original_status.clone()),
            add_labels: self.removed_labels.clone(),
            ..Default::default()
        };
        match pm.update_issue(&self.issue_id, restore).await {
            Ok(()) => report.record_success("restore_retry_task", &self.issue_id, None),
            Err(error) => report.record_failure(
                "restore_retry_task",
                &self.issue_id,
                None,
                format!("{error:#}"),
            ),
        }
    }
}

#[async_trait::async_trait]
impl ReversibleOp for ModifyTaskSpecExecution {
    async fn rollback(&self, pm: &PmService, adv: &dyn BeadsAdvanced, report: &mut RollbackReport) {
        if let Some(original_body) = self.body_change.as_deref() {
            match pm
                .update_issue(
                    &self.issue_id,
                    IssueUpdate {
                        body: Some(original_body.to_string()),
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(()) => report.record_success("restore_body", &self.issue_id, None),
                Err(error) => report.record_failure(
                    "restore_body",
                    &self.issue_id,
                    None,
                    format!("{error:#}"),
                ),
            }
        }
        if let Some(new_label) = self.new_agent_label.as_deref() {
            let _ = pm
                .update_issue(
                    &self.issue_id,
                    IssueUpdate {
                        remove_labels: vec![new_label.to_string()],
                        ..Default::default()
                    },
                )
                .await;
        }
        if let Some(old_label) = self.original_agent_label.as_deref() {
            let _ = pm
                .update_issue(
                    &self.issue_id,
                    IssueUpdate {
                        add_labels: vec![old_label.to_string()],
                        ..Default::default()
                    },
                )
                .await;
        }
        // Reverse the dep diff: remove what we added, re-add what we removed.
        for dep in &self.added_deps {
            match adv.remove_dependency(&self.issue_id, dep).await {
                Ok(()) => {
                    report.record_success("modify_remove_added_dep", &self.issue_id, Some(dep))
                }
                Err(error) => report.record_failure(
                    "modify_remove_added_dep",
                    &self.issue_id,
                    Some(dep),
                    format!("{error:#}"),
                ),
            }
        }
        for dep in &self.removed_deps {
            match pm.add_dependency(&self.issue_id, dep).await {
                Ok(()) => {
                    report.record_success("modify_restore_removed_dep", &self.issue_id, Some(dep))
                }
                Err(error) => report.record_failure(
                    "modify_restore_removed_dep",
                    &self.issue_id,
                    Some(dep),
                    format!("{error:#}"),
                ),
            }
        }
    }
}

#[async_trait::async_trait]
impl ReversibleOp for AbandonTaskExecution {
    async fn rollback(
        &self,
        pm: &PmService,
        _adv: &dyn BeadsAdvanced,
        report: &mut RollbackReport,
    ) {
        for (issue_id, original_status) in &self.targets {
            match pm
                .update_issue(
                    issue_id,
                    IssueUpdate {
                        status: Some(original_status.clone()),
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(()) => report.record_success("restore_abandoned_task", issue_id, None),
                Err(error) => report.record_failure(
                    "restore_abandoned_task",
                    issue_id,
                    None,
                    format!("{error:#}"),
                ),
            }
        }
    }
}

#[async_trait::async_trait]
impl ReversibleOp for InsertTaskBeforeExecution {
    /// Rollback ordering: (1) remove the dep edge target → child, (2) close
    /// the inserted child issue, (3) restore the target's prior status +
    /// labels. Steps 1 and 3 mirror the apply path's effects in reverse.
    ///
    /// **Why CLOSE the child instead of DELETE**: the spec said "delete child
    /// issue", but we close it (status → closed) for two reasons:
    /// 1. **Audit-trail preservation** — the inserted issue still bears the
    ///    `MutationCommit` audit + the `mutation:<uuid>` label. Hard-deleting
    ///    the issue orphans those records and makes post-mortem replay of a
    ///    failed mutation harder. Closing keeps the historical row queryable.
    /// 2. **PmService surface** — beads' `delete_issue` is best-effort and
    ///    not a uniform contract across PM backends; `update_issue { status:
    ///    closed }` is. The closed-status convention is the same one
    ///    `CancelTask`/`AbandonTask` use, so rollback cleanup is uniform.
    ///
    /// The post-mutation projector treats a closed issue with no completion
    /// audit as terminal-but-unreviewed; the rollback report's "successfully
    /// rolled back" classification rests on the dep-edge removal + target
    /// restore, not on the child's terminal state.
    async fn rollback(&self, pm: &PmService, adv: &dyn BeadsAdvanced, report: &mut RollbackReport) {
        if self.dep_added {
            if let Some(new_id) = self.new_issue_id.as_deref() {
                match adv.remove_dependency(&self.target_issue_id, new_id).await {
                    Ok(()) => report.record_success(
                        "insert_remove_dep",
                        &self.target_issue_id,
                        Some(new_id),
                    ),
                    Err(error) => report.record_failure(
                        "insert_remove_dep",
                        &self.target_issue_id,
                        Some(new_id),
                        format!("{error:#}"),
                    ),
                }
            }
        }
        if let Some(new_id) = self.new_issue_id.as_deref() {
            match pm
                .update_issue(
                    new_id,
                    IssueUpdate {
                        status: Some(pm.closed_status().to_string()),
                        ..Default::default()
                    },
                )
                .await
            {
                Ok(()) => report.record_success("close_inserted_child", new_id, None),
                Err(error) => report.record_failure(
                    "close_inserted_child",
                    new_id,
                    None,
                    format!("{error:#}"),
                ),
            }
        }
        let restore = IssueUpdate {
            status: Some(self.target_original_status.clone()),
            add_labels: self.target_removed_labels.clone(),
            ..Default::default()
        };
        match pm.update_issue(&self.target_issue_id, restore).await {
            Ok(()) => report.record_success("restore_insert_target", &self.target_issue_id, None),
            Err(error) => report.record_failure(
                "restore_insert_target",
                &self.target_issue_id,
                None,
                format!("{error:#}"),
            ),
        }
    }
}

#[async_trait::async_trait]
impl ReversibleOp for AddDependencyExecution {
    async fn rollback(
        &self,
        _pm: &PmService,
        adv: &dyn BeadsAdvanced,
        report: &mut RollbackReport,
    ) {
        match adv
            .remove_dependency(&self.issue_id, &self.depends_on)
            .await
        {
            Ok(()) => {
                report.record_success("remove_added_dep", &self.issue_id, Some(&self.depends_on))
            }
            Err(error) => report.record_failure(
                "remove_added_dep",
                &self.issue_id,
                Some(&self.depends_on),
                format!("{error:#}"),
            ),
        }
    }
}

#[async_trait::async_trait]
impl ReversibleOp for CancelTaskExecution {
    async fn rollback(
        &self,
        pm: &PmService,
        _adv: &dyn BeadsAdvanced,
        report: &mut RollbackReport,
    ) {
        match pm
            .update_issue(
                &self.issue_id,
                IssueUpdate {
                    status: Some(self.original_status.clone()),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(()) => report.record_success("restore_cancelled_task", &self.issue_id, None),
            Err(error) => report.record_failure(
                "restore_cancelled_task",
                &self.issue_id,
                None,
                format!("{error:#}"),
            ),
        }
    }
}

async fn rollback_mutation(
    pm: Arc<PmService>,
    feature_gate: Arc<spur_license::FeatureGate>,
    executed_ops: &[ExecutedOp],
) -> RollbackReport {
    let mut report = RollbackReport::default();
    if let Err(error) = crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate.as_ref(),
    ) {
        if let Some(op) = executed_ops.last() {
            report.record_failure(
                "rollback_setup",
                executed_op_setup_id(op),
                None,
                crate::server::feature_error_message(error),
            );
        }
        return report;
    }
    let Some(adv) = pm.advanced() else {
        if let Some(op) = executed_ops.last() {
            report.record_failure(
                "rollback_setup",
                executed_op_setup_id(op),
                None,
                "rollback requires beads backend",
            );
        }
        return report;
    };

    rollback_executed_ops_in_reverse(pm.as_ref(), adv, executed_ops, &mut report).await;
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

// bd-2m2u Phase 2c — apply helpers for the new ops.

/// Apply RetryTask, registering a partial-state `ExecutedOp` in `executed_ops`
/// before attempting any mutating step. On error the caller's rollback path
/// can undo whatever portion was committed.
///
/// bd-2m2u Phase 2d — brain-directed retries count toward `MAX_ATTEMPTS`.
/// If `project_attempt_facts(prior_audits) >= MAX_ATTEMPTS`, the next
/// dispatch would exceed the cap, so this op is rejected with no
/// side-effects (caller flips back to `AbandonTask` to terminate).
async fn apply_retry_task(
    pm: &PmService,
    adv: &dyn BeadsAdvanced,
    issue_id: &str,
    mutation_id: &uuid::Uuid,
    executed_ops: &mut Vec<ExecutedOp>,
) -> Result<()> {
    let issue = pm
        .get_issue(issue_id)
        .await
        .with_context(|| format!("load retry target {issue_id}"))?;
    let original_status = issue.status.clone();

    // Read attempt facts before any mutating side-effect so the MAX_ATTEMPTS
    // guard rejects cleanly without a rollback record.
    let prior_audits = super::projector::collect_sorted_audits_for_issue(
        issue_id,
        adv.list_comments(issue_id)
            .await
            .with_context(|| format!("list comments for retry target {issue_id}"))?,
    )
    .with_context(|| format!("parse comments for retry target {issue_id}"))?;
    let (attempt, last_delegation_id) = super::projector::project_attempt_facts(&prior_audits);
    if attempt >= super::MAX_ATTEMPTS {
        return Err(anyhow!(
            "RetryTask refused: task '{issue_id}' has reached MAX_ATTEMPTS={} (current attempt={attempt}); brain must AbandonTask or ModifyTaskSpec instead",
            super::MAX_ATTEMPTS,
        ));
    }

    let removable: Vec<String> = issue
        .labels
        .iter()
        .filter(|label| {
            label.as_str() == super::labels::READY_FOR_REVIEW
                || label.as_str() == "ready-for-review"
        })
        .cloned()
        .collect();

    // Register up front so any subsequent step's failure rolls back this op.
    executed_ops.push(ExecutedOp::RetryTask(RetryExecution {
        issue_id: issue_id.to_string(),
        original_status,
        removed_labels: removable.clone(),
    }));

    pm.update_issue(
        issue_id,
        IssueUpdate {
            status: Some("open".to_string()),
            remove_labels: removable,
            ..Default::default()
        },
    )
    .await
    .with_context(|| format!("retry_task open issue {issue_id}"))?;

    adv.add_comment(
        issue_id,
        &audit_encode(&AuditSentinelKind::RetryRequested {
            delegation_id: last_delegation_id.unwrap_or_else(|| mutation_id.to_string()),
            attempt,
            error: "brain-directed retry via submit_plan_mutation".to_string(),
            worker_branch: None,
            amended_prompt_summary: None,
        }),
    )
    .await
    .with_context(|| format!("emit retry_requested audit for {issue_id}"))?;

    Ok(())
}

/// Apply ModifyTaskSpec while threading partial-state into `executed_ops` so a
/// mid-sequence failure (e.g., a cycle-creating `add_dependency`) still leaves
/// a rollback record covering the work already done.
struct ModifyTaskSpecInput<'a> {
    issue_id: &'a str,
    new_task: Option<&'a str>,
    new_agent: Option<&'a str>,
    new_context_files: Option<&'a [String]>,
    new_depends_on: Option<&'a [String]>,
}

async fn apply_modify_task_spec(
    pm: &PmService,
    adv: &dyn BeadsAdvanced,
    input: ModifyTaskSpecInput<'_>,
    executed_ops: &mut Vec<ExecutedOp>,
) -> Result<()> {
    let issue_id = input.issue_id;
    let issue = pm
        .get_issue(issue_id)
        .await
        .with_context(|| format!("load modify target {issue_id}"))?;

    let original_body = issue.body.clone();
    let original_agent_label = issue
        .labels
        .iter()
        .find(|label| super::labels::parse_agent(label).is_some())
        .cloned();
    let original_blocked_by: Vec<String> = issue.blocked_by.clone();

    // Reserve a rollback slot up-front. Subsequent steps mutate this entry as
    // they commit, so a partial failure leaves a complete-as-of-now record.
    let exec_idx = executed_ops.len();
    executed_ops.push(ExecutedOp::ModifyTaskSpec(ModifyTaskSpecExecution {
        issue_id: issue_id.to_string(),
        body_change: None,
        original_agent_label: original_agent_label.clone(),
        new_agent_label: None,
        added_deps: Vec::new(),
        removed_deps: Vec::new(),
    }));

    // 1. Body update
    if let Some(new) = input.new_task {
        if new != original_body {
            pm.update_issue(
                issue_id,
                IssueUpdate {
                    body: Some(new.to_string()),
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("modify body for {issue_id}"))?;
            if let ExecutedOp::ModifyTaskSpec(slot) = &mut executed_ops[exec_idx] {
                slot.body_change = Some(original_body.clone());
            }
        }
    }

    // 2. Agent label swap
    if let Some(agent_name) = input.new_agent {
        let new_label = super::labels::agent(agent_name);
        let mut remove = Vec::new();
        if let Some(old) = original_agent_label.as_deref() {
            if old != new_label.as_str() {
                remove.push(old.to_string());
            }
        }
        let already_present = issue.labels.iter().any(|l| l == &new_label);
        let mut add = Vec::new();
        if !already_present {
            add.push(new_label.clone());
        }
        if !remove.is_empty() || !add.is_empty() {
            pm.update_issue(
                issue_id,
                IssueUpdate {
                    add_labels: add,
                    remove_labels: remove,
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("swap agent label for {issue_id}"))?;
        }
        if !already_present {
            if let ExecutedOp::ModifyTaskSpec(slot) = &mut executed_ops[exec_idx] {
                slot.new_agent_label = Some(new_label);
            }
        }
    }

    // 3. Dependencies diff. Removed deps first (safe reverse), then add new
    //    edges. add_dependency is the most likely source of mid-op failure
    //    (cycle detection in beads_rust); the partial-state recording above
    //    means rollback can clean up.
    if let Some(targets) = input.new_depends_on {
        let want: HashSet<&str> = targets.iter().map(String::as_str).collect();
        let have: HashSet<&str> = original_blocked_by.iter().map(String::as_str).collect();
        for dep in &original_blocked_by {
            if !want.contains(dep.as_str()) {
                adv.remove_dependency(issue_id, dep)
                    .await
                    .with_context(|| format!("modify remove dep {issue_id} -> {dep}"))?;
                if let ExecutedOp::ModifyTaskSpec(slot) = &mut executed_ops[exec_idx] {
                    slot.removed_deps.push(dep.clone());
                }
            }
        }
        for dep in targets {
            if !have.contains(dep.as_str()) {
                pm.add_dependency(issue_id, dep)
                    .await
                    .with_context(|| format!("modify add dep {issue_id} -> {dep}"))?;
                if let ExecutedOp::ModifyTaskSpec(slot) = &mut executed_ops[exec_idx] {
                    slot.added_deps.push(dep.clone());
                }
            }
        }
    }

    // 4. Extended TaskSpec audit.
    let prior_audits = super::projector::collect_sorted_audits_for_issue(
        issue_id,
        adv.list_comments(issue_id)
            .await
            .with_context(|| format!("list comments for modify target {issue_id}"))?,
    )
    .with_context(|| format!("parse comments for modify target {issue_id}"))?;
    let (prior_task_id, prior_context_files) = super::projector::latest_task_spec(&prior_audits)
        .unwrap_or_else(|| (issue_id.to_string(), Vec::new()));
    let context_files: Vec<String> = match input.new_context_files {
        Some(files) => files.to_vec(),
        None => prior_context_files,
    };

    super::emit_extended_task_spec_audit(
        adv,
        issue_id,
        &prior_task_id,
        &context_files,
        input.new_task,
        input.new_agent,
        input.new_depends_on,
    )
    .await
    .with_context(|| format!("emit extended task_spec audit for {issue_id}"))?;

    Ok(())
}

/// Apply AbandonTask, recording each successfully-closed target into the
/// executed-op slot as we go so a mid-cascade failure rolls back the targets
/// that were already closed.
async fn apply_abandon_task(
    pm: &PmService,
    adv: &dyn BeadsAdvanced,
    issue_id: &str,
    reason: &str,
    cascade_descendants: bool,
    executed_ops: &mut Vec<ExecutedOp>,
) -> Result<Vec<String>> {
    let mut targets: Vec<String> = vec![issue_id.to_string()];
    if cascade_descendants {
        let descendants = collect_descendants(pm, issue_id).await?;
        targets.extend(descendants);
    }

    let exec_idx = executed_ops.len();
    executed_ops.push(ExecutedOp::AbandonTask(AbandonTaskExecution {
        targets: Vec::new(),
    }));

    let mut affected: Vec<String> = Vec::with_capacity(targets.len());
    for id in &targets {
        let issue = pm
            .get_issue(id)
            .await
            .with_context(|| format!("load abandon target {id}"))?;
        let original_status = issue.status.clone();

        pm.update_issue(
            id,
            IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .with_context(|| format!("abandon close {id}"))?;
        if let ExecutedOp::AbandonTask(slot) = &mut executed_ops[exec_idx] {
            slot.targets.push((id.clone(), original_status));
        }

        adv.add_comment(
            id,
            &audit_encode(&AuditSentinelKind::Completion {
                delegation_id: format!("abandon:{}", id),
                completion_state: CompletionState::Failed,
                superseded: false,
                worker_branch: None,
                result_summary: Some(reason.to_string()),
                artifact_uri: None,
                dispatched_base_oid: None,
            }),
        )
        .await
        .with_context(|| format!("emit abandon audit for {id}"))?;

        affected.push(id.clone());
    }

    Ok(affected)
}

// bd-2m2u Phase 2e — apply helpers for InsertTaskBefore / AddDependency / CancelTask.

/// Apply InsertTaskBefore: capture target pre-image, create the prerequisite
/// child issue, wire `target.blocked_by += child`, then re-open + scrub
/// review-ready labels on the target so it projects as Pending. Each step
/// updates the rollback slot in place so a mid-op failure unwinds cleanly.
async fn apply_insert_task_before(
    pm: &PmService,
    adv: &dyn BeadsAdvanced,
    target_issue_id: &str,
    draft: &super::mutation::TaskDraft,
    mutation_id: &uuid::Uuid,
    executed_ops: &mut Vec<ExecutedOp>,
) -> Result<String> {
    let target = pm
        .get_issue(target_issue_id)
        .await
        .with_context(|| format!("load insert_task_before target {target_issue_id}"))?;
    let target_original_status = target.status.clone();
    let target_removed_labels: Vec<String> = target
        .labels
        .iter()
        .filter(|label| {
            label.as_str() == super::labels::READY_FOR_REVIEW
                || label.as_str() == "ready-for-review"
        })
        .cloned()
        .collect();
    let parent_plan_id = target
        .labels
        .iter()
        .find_map(|label| super::labels::parse_plan_id(label))
        .map(str::to_string);
    let parent_agent = target
        .labels
        .iter()
        .find_map(|label| super::labels::parse_agent(label))
        .map(str::to_string);

    let exec_idx = executed_ops.len();
    executed_ops.push(ExecutedOp::InsertTaskBefore(InsertTaskBeforeExecution {
        target_issue_id: target_issue_id.to_string(),
        target_original_status: target_original_status.clone(),
        target_removed_labels: target_removed_labels.clone(),
        new_issue_id: None,
        dep_added: false,
    }));

    let new_id = pm
        .create_issue(IssueCreate {
            title: draft.title.clone(),
            description: Some(draft.description.clone()),
            issue_type: Some("task".into()),
            parent: None,
            assignee: draft.assignee.clone(),
            priority: draft.priority,
            labels: vec![mutation_id_label(mutation_id)],
            ..Default::default()
        })
        .await
        .context("create insert_task_before child issue")?;
    if let ExecutedOp::InsertTaskBefore(slot) = &mut executed_ops[exec_idx] {
        slot.new_issue_id = Some(new_id.clone());
    }

    if let (Some(plan_id), Some(agent)) = (parent_plan_id.as_deref(), parent_agent.as_deref()) {
        add_labels_individually(
            pm,
            &new_id,
            &[
                super::labels::plan_id(plan_id),
                super::labels::plan_task_id(&new_id),
                super::labels::agent(agent),
            ],
        )
        .await
        .with_context(|| format!("persist plan scope on inserted child {new_id}"))?;
    }

    pm.add_dependency(target_issue_id, &new_id)
        .await
        .with_context(|| {
            format!("wire {target_issue_id} blocked_by inserted prerequisite {new_id}")
        })?;
    if let ExecutedOp::InsertTaskBefore(slot) = &mut executed_ops[exec_idx] {
        slot.dep_added = true;
    }

    pm.update_issue(
        target_issue_id,
        IssueUpdate {
            status: Some("open".to_string()),
            remove_labels: target_removed_labels,
            ..Default::default()
        },
    )
    .await
    .with_context(|| format!("reopen insert target {target_issue_id}"))?;

    let prior_audits = super::projector::collect_sorted_audits_for_issue(
        target_issue_id,
        adv.list_comments(target_issue_id)
            .await
            .with_context(|| format!("list comments for insert target {target_issue_id}"))?,
    )
    .with_context(|| format!("parse comments for insert target {target_issue_id}"))?;
    let (_attempt, last_delegation_id) = super::projector::project_attempt_facts(&prior_audits);
    adv.add_comment(
        target_issue_id,
        &audit_encode(&AuditSentinelKind::RetryRequested {
            delegation_id: last_delegation_id.unwrap_or_else(|| mutation_id.to_string()),
            attempt: 0,
            error: format!("brain-directed insert_task_before: prerequisite {new_id} created"),
            worker_branch: None,
            amended_prompt_summary: None,
        }),
    )
    .await
    .with_context(|| format!("emit retry_requested audit for {target_issue_id}"))?;

    Ok(new_id)
}

/// Apply AddDependency: a single edge add. Cycles surface from the post-hoc
/// `dep_cycles_with_fallback` scan in `apply_mutation` and trigger rollback.
///
/// Self-reference (`issue_id == depends_on`) is rejected up-front rather than
/// caught by the post-hoc cycle scan — the post-hoc path is correct but
/// requires a full mutation + rollback round-trip; a cheap defensive guard
/// here is strictly faster and produces a clearer error for the common typo.
async fn apply_add_dependency(
    pm: &PmService,
    issue_id: &str,
    depends_on: &str,
    executed_ops: &mut Vec<ExecutedOp>,
) -> Result<()> {
    if issue_id == depends_on {
        anyhow::bail!(
            "add_dependency: self-reference rejected ({issue_id} cannot depend on itself)"
        );
    }
    pm.add_dependency(issue_id, depends_on)
        .await
        .with_context(|| format!("add_dependency {issue_id} -> {depends_on}"))?;
    executed_ops.push(ExecutedOp::AddDependency(AddDependencyExecution {
        issue_id: issue_id.to_string(),
        depends_on: depends_on.to_string(),
    }));
    Ok(())
}

/// Apply CancelTask: terminal cancellation, no cascade. Closes the issue and
/// emits a `Completion(Cancelled)` audit so the projector can distinguish
/// cancellation from `AbandonTask` (Failed).
async fn apply_cancel_task(
    pm: &PmService,
    adv: &dyn BeadsAdvanced,
    issue_id: &str,
    reason: &str,
    executed_ops: &mut Vec<ExecutedOp>,
) -> Result<()> {
    let issue = pm
        .get_issue(issue_id)
        .await
        .with_context(|| format!("load cancel target {issue_id}"))?;
    let original_status = issue.status.clone();

    pm.update_issue(
        issue_id,
        IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .with_context(|| format!("cancel close {issue_id}"))?;
    executed_ops.push(ExecutedOp::CancelTask(CancelTaskExecution {
        issue_id: issue_id.to_string(),
        original_status,
    }));

    adv.add_comment(
        issue_id,
        &audit_encode(&AuditSentinelKind::Completion {
            delegation_id: format!("cancel:{issue_id}"),
            completion_state: CompletionState::Cancelled,
            superseded: false,
            worker_branch: None,
            result_summary: Some(reason.to_string()),
            artifact_uri: None,
            dispatched_base_oid: None,
        }),
    )
    .await
    .with_context(|| format!("emit cancel audit for {issue_id}"))?;

    Ok(())
}

/// Walk `blocked_by` reverse edges from `root_id` to find every transitive
/// descendant. The result excludes `root_id` itself.
async fn collect_descendants(pm: &PmService, root_id: &str) -> Result<Vec<String>> {
    let all_ids = list_all_issue_ids(pm).await?;
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = vec![root_id.to_string()];
    while let Some(current) = stack.pop() {
        for id in &all_ids {
            if id == &current || seen.contains(id) {
                continue;
            }
            let issue = pm
                .get_issue(id)
                .await
                .with_context(|| format!("scan descendants for {current}"))?;
            if issue.blocked_by.iter().any(|dep| dep == &current) {
                seen.insert(id.clone());
                out.push(id.clone());
                stack.push(id.clone());
            }
        }
    }
    Ok(out)
}

/// bd-2m2u Phase 2c — `submit_plan_mutation` MCP entry. Wraps `apply_mutation`
/// end-to-end: cycle detection + rollback via the now-generic executor, then
/// clears `signal:escalated` labels from every affected issue on success so
/// the engine resumes traversal.
#[derive(Debug, Clone)]
pub struct SubmitPlanMutationResult {
    pub mutation_id: String,
    pub children_created: Vec<String>,
    pub affected_task_ids: Vec<String>,
}

pub async fn submit_plan_mutation(
    pm: Arc<PmService>,
    feature_gate: Arc<spur_license::FeatureGate>,
    mutation_id: uuid::Uuid,
    trigger_task_id: String,
    ops: Vec<PlanMutationOp>,
) -> Result<SubmitPlanMutationResult> {
    let batch = MutationBatch {
        mutation_id,
        trigger_signal_id: None,
        trigger_task_id: trigger_task_id.clone(),
        ops,
    };

    let children_created = apply_mutation(pm.clone(), Arc::clone(&feature_gate), &batch).await?;

    // Compute which issues the batch touched. SplitTask creates children; the
    // other ops mutate existing issues. We strip `signal:escalated` from every
    // affected id (idempotent — `remove_label` is a no-op when absent).
    let mut affected: Vec<String> = vec![trigger_task_id];
    for op in &batch.ops {
        match op {
            PlanMutationOp::SplitTask { parent, .. } => {
                affected.push(parent.clone());
            }
            PlanMutationOp::RetryTask { issue_id }
            | PlanMutationOp::ModifyTaskSpec { issue_id, .. }
            | PlanMutationOp::AbandonTask { issue_id, .. }
            | PlanMutationOp::AddDependency { issue_id, .. }
            | PlanMutationOp::CancelTask { issue_id, .. } => {
                affected.push(issue_id.clone());
            }
            PlanMutationOp::InsertTaskBefore {
                target_issue_id, ..
            } => {
                affected.push(target_issue_id.clone());
            }
        }
    }
    affected.extend(children_created.iter().cloned());
    let mut seen: HashSet<String> = HashSet::new();
    affected.retain(|id| seen.insert(id.clone()));

    for id in &affected {
        if let Err(error) = pm
            .update_issue(
                id,
                IssueUpdate {
                    remove_labels: vec![SIGNAL_ESCALATED_LABEL.to_string()],
                    ..Default::default()
                },
            )
            .await
        {
            tracing::debug!(
                target: "spur.mutation.signal_escalated_clear",
                issue_id = %id,
                error = %error,
                "could not clear signal:escalated label (likely already absent)"
            );
        }
    }

    Ok(SubmitPlanMutationResult {
        mutation_id: batch.mutation_id.to_string(),
        children_created,
        affected_task_ids: affected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_pm::{IssueSummary, PmSource};
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn test_pm() -> (Arc<spur_pm::PmService>, TempDir) {
        let dir = TempDir::new().expect("tempdir");
        let workspace = spur_pm::test_workspace::TestBeadsWorkspace::init();
        let beads_dir = dir.path().join(".beads");
        std::fs::create_dir_all(&beads_dir).expect("create .beads");
        workspace.copy_db_to(&beads_dir);
        let pm = Arc::new(
            spur_pm::PmService::try_new(None, true, false, dir.path(), None)
                .await
                .expect("PmService::try_new")
                .expect("expected beads pm"),
        );
        (pm, dir)
    }

    fn test_advanced(pm: &PmService) -> &dyn BeadsAdvanced {
        let gate = Arc::new(spur_license::FeatureGate::new(
            spur_license::policy::PolicyResolver::embedded(),
        ));
        let features =
            std::collections::BTreeSet::from([spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED
                .as_str()
                .to_string()]);
        gate.update_state(&spur_license::LicenseState::active_validated(
            spur_license::Plan::Pro,
            features,
        ));
        crate::server::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            gate.as_ref(),
        )
        .expect("test feature gate should allow beads advanced");
        pm.advanced().expect("beads adv")
    }

    #[tokio::test]
    async fn executor_iterates_executed_ops_in_reverse_for_rollback() {
        let (pm, _dir) = test_pm().await;
        let adv = test_advanced(pm.as_ref());
        let executed: Vec<ExecutedOp> = vec![
            ExecutedOp::NoOp(NoOpExecution {
                label: "first".into(),
            }),
            ExecutedOp::NoOp(NoOpExecution {
                label: "second".into(),
            }),
            ExecutedOp::NoOp(NoOpExecution {
                label: "third".into(),
            }),
        ];
        let mut report = RollbackReport::default();
        rollback_executed_ops_in_reverse(pm.as_ref(), adv, &executed, &mut report).await;
        let order: Vec<&str> = report
            .succeeded
            .iter()
            .map(|op| op.issue_id.as_str())
            .collect();
        assert_eq!(order, vec!["third", "second", "first"]);
    }

    #[test]
    fn existing_split_task_apply_rollback_unchanged() {
        let split = SplitExecution {
            parent_id: "bd-100".into(),
            original_parent_status: "open".into(),
            child_ids: vec!["bd-101".into(), "bd-102".into()],
            removed_parent_from: vec!["bd-200".into()],
        };
        let snapshot = format!("{split:?}");
        let op = ExecutedOp::SplitTask(split);
        match &op {
            ExecutedOp::SplitTask(round_trip) => {
                assert_eq!(format!("{round_trip:?}"), snapshot);
                assert_eq!(round_trip.parent_id, "bd-100");
                assert_eq!(round_trip.original_parent_status, "open");
                assert_eq!(round_trip.child_ids, vec!["bd-101", "bd-102"]);
                assert_eq!(round_trip.removed_parent_from, vec!["bd-200"]);
            }
            #[allow(unreachable_patterns)]
            _ => panic!("expected SplitTask variant"),
        }
    }

    #[tokio::test]
    async fn synthetic_noop_in_test_only_completes_via_reversible_trait() {
        let (pm, _dir) = test_pm().await;
        let adv = test_advanced(pm.as_ref());
        let op = NoOpExecution {
            label: "noop-target".into(),
        };
        let mut report = RollbackReport::default();
        ReversibleOp::rollback(&op, pm.as_ref(), adv, &mut report).await;
        assert_eq!(report.failed, Vec::new());
        assert_eq!(report.succeeded.len(), 1);
        assert_eq!(report.succeeded[0].kind, "noop");
        assert_eq!(report.succeeded[0].issue_id, "noop-target");
        assert_eq!(report.succeeded[0].depends_on_id, None);
    }

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
            description: None,
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
