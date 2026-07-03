use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::plan::audit_sentinel::AuditSentinelKind;
use crate::plan::outcomes::SkipReason;
use spur_pm::IssueFilter;

#[async_trait::async_trait]
pub trait ReconcilerAutomation: Send + Sync {
    async fn merge_plan(&self, plan_id: &str) -> anyhow::Result<crate::plan::PlanMergeState>;
    async fn create_pr(&self, params: spur_pm::PrParams) -> anyhow::Result<String>;
}

fn child_summary_may_have_terminal_projection(
    child: &spur_pm::IssueSummary,
    closed_status: &str,
) -> bool {
    child.status == closed_status || matches!(child.status.as_str(), "failed" | "cancelled")
}

fn system_time_to_unix_seconds(now: SystemTime) -> i64 {
    now.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProjectedEpicCompletion {
    pub(super) audit_outcome: crate::plan::audit_sentinel::EpicCompletionOutcome,
    pub(super) add_integration_pending: bool,
    pub(super) approved_count: u32,
    pub(super) rejected_count: u32,
    pub(super) failed_count: u32,
    pub(super) cancelled_count: u32,
}

pub(super) fn classify_epic_completion(
    children: &[spur_pm::IssueSummary],
    closed_status: &str,
) -> Option<ProjectedEpicCompletion> {
    if children.is_empty() {
        return None;
    }

    let is_terminal_status = |status: &str| {
        status == closed_status || matches!(status, "failed" | "cancelled" | "rejected")
    };

    if children
        .iter()
        .any(|child| !is_terminal_status(child.status.as_str()))
    {
        return None;
    }

    let mut approved_count = 0u32;
    let mut rejected_count = 0u32;
    let mut failed_count = 0u32;
    let mut cancelled_count = 0u32;

    for child in children {
        let rejected = child.status == "rejected"
            || child.labels.iter().any(|label| {
                matches!(
                    label.as_str(),
                    "rejected" | "review-rejected" | crate::plan::labels::REVIEW_REJECTED
                )
            });
        if rejected {
            rejected_count += 1;
            continue;
        }

        match child.status.as_str() {
            "failed" => failed_count += 1,
            "cancelled" => cancelled_count += 1,
            status if status == closed_status => approved_count += 1,
            _ => return None,
        }
    }

    let has_terminal_failures = rejected_count > 0 || failed_count > 0 || cancelled_count > 0;

    Some(ProjectedEpicCompletion {
        audit_outcome: if has_terminal_failures {
            crate::plan::audit_sentinel::EpicCompletionOutcome::TerminalWithFailures
        } else {
            crate::plan::audit_sentinel::EpicCompletionOutcome::AllApproved
        },
        add_integration_pending: !has_terminal_failures,
        approved_count,
        rejected_count,
        failed_count,
        cancelled_count,
    })
}

pub(super) fn build_auto_pr_params(
    plan_id: &str,
    epic_title: &str,
    outcome_summary: &str,
    merge_branch: &str,
) -> spur_pm::PrParams {
    spur_pm::PrParams {
        title: format!("[SPUR] {epic_title} ({plan_id})"),
        body: format!(
            "Auto-created for plan `{plan_id}`.\n\nOutcome: {outcome_summary}\nMerge branch: {merge_branch}"
        ),
        head_branch: merge_branch.to_string(),
        base_branch: None,
        repo: None,
    }
}

impl super::Reconciler {
    async fn maybe_emit_loop_run_record(
        &self,
        adv: &dyn spur_pm::BeadsAdvanced,
        epic: &spur_pm::IssueSummary,
        plan_id: &str,
        children: &[spur_pm::IssueSummary],
        outcome: ProjectedEpicCompletion,
    ) -> anyhow::Result<bool> {
        let Some(loop_id) = epic
            .labels
            .iter()
            .find_map(|label| crate::plan::labels::parse_loop_id(label))
            .map(str::to_string)
        else {
            return Ok(false);
        };
        let Some(generation) = epic
            .labels
            .iter()
            .find_map(|label| crate::plan::labels::parse_loop_generation(label))
        else {
            tracing::warn!(
                %plan_id,
                epic_id = %epic.id,
                %loop_id,
                "loop-labeled terminal epic is missing loop generation label; skipping loop-run audit"
            );
            return Ok(false);
        };

        let loop_issues = self
            .pm
            .list_issues(IssueFilter {
                labels: vec![crate::plan::labels::loop_id_label(&loop_id)],
                issue_type: Some("task".into()),
                limit: Some(2),
                ..Default::default()
            })
            .await?;
        let Some(loop_issue) = loop_issues.first() else {
            tracing::warn!(
                %plan_id,
                epic_id = %epic.id,
                %loop_id,
                generation,
                "loop-labeled terminal epic has no matching loop issue; skipping loop-run audit"
            );
            return Ok(false);
        };
        if loop_issues.len() > 1 {
            tracing::warn!(
                %plan_id,
                epic_id = %epic.id,
                %loop_id,
                generation,
                "multiple loop issues carry the same loop id; using first match for loop-run audit"
            );
        }

        let loop_audits = crate::plan::projector::collect_sorted_audits_for_issue(
            &loop_issue.id,
            adv.list_comments(&loop_issue.id).await?,
        )?;
        if loop_audits.iter().any(|audit| {
            matches!(
                audit,
                AuditSentinelKind::LoopRun {
                    loop_id: audit_loop_id,
                    generation: audit_generation,
                    ..
                } if audit_loop_id == &loop_id && *audit_generation == generation
            )
        }) {
            return Ok(false);
        }

        let mut child_audits = Vec::new();
        for child in children {
            child_audits.extend(crate::plan::projector::collect_sorted_audits_for_issue(
                &child.id,
                adv.list_comments(&child.id).await?,
            )?);
        }

        let terminal_counts = crate::plan::loops::run_record::LoopRunOutcome {
            tasks_discovered: outcome
                .approved_count
                .saturating_add(outcome.rejected_count)
                .saturating_add(outcome.failed_count)
                .saturating_add(outcome.cancelled_count),
            approved: outcome.approved_count,
            rejected: outcome.rejected_count,
            failed: outcome.failed_count,
            cancelled: outcome.cancelled_count,
        };
        let now = system_time_to_unix_seconds(self.now());
        let autonomy = epic
            .labels
            .iter()
            .filter_map(|label| crate::plan::labels::parse_autonomy(label))
            .max()
            .map(|level| level.as_str().to_string());
        let run = crate::plan::loops::run_record::build_loop_run(
            &loop_id,
            generation,
            plan_id,
            autonomy,
            terminal_counts,
            &child_audits,
            now,
        );
        adv.add_comment(
            &loop_issue.id,
            &crate::plan::audit_sentinel::encode_comment(&run),
        )
        .await?;
        Ok(true)
    }

    pub(super) async fn reconcile_terminal_epics(&self) -> anyhow::Result<bool> {
        crate::server::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            self.feature_gate.as_ref(),
        )
        .map_err(|error| anyhow::anyhow!(crate::server::feature_error_message(error)))?;
        let Some(adv) = self.pm.advanced() else {
            return Ok(false);
        };

        let closed_status = self.pm.closed_status().to_string();
        let mut labels = vec![crate::plan::labels::PLAN_COMPLETE.to_string()];
        if let Some(plan_id) = self.plan_id.as_deref() {
            labels.push(crate::plan::labels::plan_id(plan_id));
        }

        let mut epics = self
            .pm
            .list_issues(IssueFilter {
                labels: labels.clone(),
                issue_type: Some("epic".into()),
                limit: Some(10_000),
                ..Default::default()
            })
            .await?;
        let closed_epics = self
            .pm
            .list_issues(IssueFilter {
                labels,
                status: Some(closed_status.clone()),
                issue_type: Some("epic".into()),
                limit: Some(10_000),
                ..Default::default()
            })
            .await?;
        let mut seen_epic_ids = epics
            .iter()
            .map(|summary| summary.id.clone())
            .collect::<std::collections::HashSet<_>>();
        for summary in closed_epics {
            if seen_epic_ids.insert(summary.id.clone()) {
                epics.push(summary);
            }
        }

        let mut did_work = false;
        let mut plan_activation_cache = HashMap::new();

        for epic in epics {
            let Some(plan_id) = epic
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_plan_id(label))
            else {
                self.record_skipped(None, &epic.id, SkipReason::MissingPlanId)
                    .await;
                continue;
            };

            if self.dispatch.is_none() {
                continue;
            }
            let write_state = self
                .plan_allows_writes(plan_id, Some(&epic), &mut plan_activation_cache)
                .await?;
            if let Some(reason) = write_state.skip_reason() {
                self.record_skipped(Some(plan_id), &epic.id, reason).await;
                continue;
            }

            let mut children = self
                .pm
                .list_issues(IssueFilter {
                    labels: vec![crate::plan::labels::plan_id(plan_id)],
                    issue_type: Some("task".into()),
                    limit: Some(10_000),
                    ..Default::default()
                })
                .await?;
            let closed_children = self
                .pm
                .list_issues(IssueFilter {
                    labels: vec![crate::plan::labels::plan_id(plan_id)],
                    status: Some(closed_status.clone()),
                    issue_type: Some("task".into()),
                    limit: Some(10_000),
                    ..Default::default()
                })
                .await?;
            let mut seen_ids = children
                .iter()
                .map(|summary| summary.id.clone())
                .collect::<std::collections::HashSet<_>>();
            for summary in closed_children {
                if seen_ids.insert(summary.id.clone()) {
                    children.push(summary);
                }
            }
            let mut children = children
                .into_iter()
                .filter(|summary| summary.id != epic.id)
                .collect::<Vec<_>>();

            if children
                .iter()
                .any(|child| child_summary_may_have_terminal_projection(child, &closed_status))
            {
                match self.project_plan_from_beads(plan_id).await {
                    Ok(projected) => {
                        for child in &mut children {
                            let Some(task) = projected
                                .tasks
                                .iter()
                                .find(|task| task.spec.issue_id.as_deref() == Some(&child.id))
                            else {
                                continue;
                            };
                            child.status = match task.status {
                                crate::plan::PlanTaskStatus::Cancelled { .. }
                                | crate::plan::PlanTaskStatus::Superseded { .. } => {
                                    "cancelled".to_string()
                                }
                                crate::plan::PlanTaskStatus::Failed { .. } => "failed".to_string(),
                                crate::plan::PlanTaskStatus::Rejected { .. } => {
                                    "rejected".to_string()
                                }
                                crate::plan::PlanTaskStatus::Approved { .. } => {
                                    closed_status.clone()
                                }
                                _ => child.status.clone(),
                            };
                            let terminal_status_consistent = match task.status {
                                crate::plan::PlanTaskStatus::Cancelled { .. }
                                | crate::plan::PlanTaskStatus::Superseded { .. } => {
                                    child.status == "cancelled"
                                }
                                crate::plan::PlanTaskStatus::Failed { .. } => {
                                    child.status == "failed"
                                }
                                crate::plan::PlanTaskStatus::Rejected { .. } => {
                                    child.status == "rejected"
                                }
                                crate::plan::PlanTaskStatus::Approved { .. } => {
                                    child.status == closed_status
                                }
                                _ => true,
                            };
                            assert!(
                                terminal_status_consistent,
                                "invariant:reconciler_terminal projected terminal mapping mismatch violated (plan_id={}, issue_id={}) expected child.status to match projected terminal task status, got projected_status={:?}, child_status={:?}, closed_status={:?}",
                                plan_id,
                                child.id,
                                task.status,
                                child.status,
                                closed_status,
                            );
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            %plan_id,
                            epic_id = %epic.id,
                            "failed to project terminal child statuses for outcome pruning: {error}"
                        );
                    }
                }
            }

            let Some(outcome) = classify_epic_completion(&children, &closed_status) else {
                // Intentional no-op: non-terminal epics emit no outcome.
                // Terminal classification is computed elsewhere; absence here is normal.
                continue;
            };

            let mut audits = crate::plan::projector::collect_sorted_audits_for_issue(
                &epic.id,
                adv.list_comments(&epic.id).await?,
            )?;
            let has_epic_completion = audits.iter().any(|audit| {
                matches!(
                    audit,
                    AuditSentinelKind::EpicCompletion {
                        plan_id: audit_plan_id,
                        epic_id: audit_epic_id,
                        ..
                    } if audit_plan_id == plan_id && audit_epic_id == &epic.id
                )
            });

            if epic.status == closed_status {
                self.outcomes.lock().await.drop_plan(plan_id);
                if !outcome.add_integration_pending
                    && epic
                        .labels
                        .contains(&crate::plan::labels::INTEGRATION_PENDING.to_string())
                {
                    self.pm
                        .update_issue(
                            &epic.id,
                            spur_pm::IssueUpdate {
                                remove_labels: vec![
                                    crate::plan::labels::INTEGRATION_PENDING.to_string()
                                ],
                                ..Default::default()
                            },
                        )
                        .await?;
                    did_work = true;
                }
                if !has_epic_completion {
                    let emitted = crate::plan::emit_epic_completion_audit(
                        adv,
                        &epic.id,
                        plan_id,
                        outcome.audit_outcome,
                    )
                    .await;
                    if emitted.is_ok() {
                        audits.push(AuditSentinelKind::EpicCompletion {
                            outcome: outcome.audit_outcome,
                            plan_id: plan_id.to_string(),
                            epic_id: epic.id.clone(),
                        });
                    }
                    if let Some(sink) = self
                        .dispatch
                        .as_ref()
                        .and_then(|dispatch| dispatch.event_sink())
                    {
                        sink.emit(spur_acp::SpurEventBody::PlanCompleted {
                            plan_id: plan_id.to_string(),
                            approved: outcome.approved_count,
                            rejected: outcome.rejected_count,
                            failed: outcome.failed_count,
                            cancelled: outcome.cancelled_count,
                        });
                    }
                    if let Some(dispatch) = self.dispatch.as_ref() {
                        crate::plan::push_plan_completed_continuation(
                            dispatch.continuation_ctx().as_ref(),
                            dispatch.materializer(),
                            dispatch.brain_session_id(),
                            plan_id,
                            outcome.approved_count,
                            outcome.rejected_count,
                            outcome.failed_count,
                            outcome.cancelled_count,
                        )
                        .await;
                    }
                    did_work = true;
                }
                if self
                    .maybe_emit_loop_run_record(adv, &epic, plan_id, &children, outcome)
                    .await?
                {
                    did_work = true;
                }

                // v0e: opt-in auto-merge / auto-PR on durable all-approved state.
                if self.auto_merge_approved_plans
                    && outcome.add_integration_pending
                    && epic
                        .labels
                        .contains(&crate::plan::labels::INTEGRATION_PENDING.to_string())
                {
                    if let Some(automation) = self.automation.as_ref() {
                        if let Some(outcome_summary) =
                            crate::plan::projector::epic_completion_outcome_summary(
                                &audits, plan_id, &epic.id,
                            )
                        {
                            let mut merge_succeeded = false;
                            match automation.merge_plan(plan_id).await {
                                Ok(crate::plan::PlanMergeState::Succeeded {
                                    merge_branch, ..
                                }) => {
                                    let params = build_auto_pr_params(
                                        plan_id,
                                        &epic.title,
                                        outcome_summary,
                                        &merge_branch,
                                    );
                                    if let Err(e) = automation.create_pr(params).await {
                                        tracing::warn!(%plan_id, "auto-merge PR creation failed: {e}");
                                    } else {
                                        merge_succeeded = true;
                                    }
                                }
                                Ok(crate::plan::PlanMergeState::Conflict {
                                    conflict_task_id,
                                    ..
                                }) => {
                                    tracing::warn!(%plan_id, %conflict_task_id, "auto-merge detected conflict");
                                }
                                Ok(crate::plan::PlanMergeState::Failed { error }) => {
                                    tracing::warn!(%plan_id, "auto-merge failed: {error}");
                                }
                                Ok(crate::plan::PlanMergeState::NotStarted) => {
                                    tracing::warn!(%plan_id, "auto-merge returned NotStarted unexpectedly");
                                }
                                Err(e) => {
                                    tracing::warn!(%plan_id, "auto-merge error: {e}");
                                }
                            }
                            if merge_succeeded {
                                if let Err(e) = self
                                    .pm
                                    .update_issue(
                                        &epic.id,
                                        spur_pm::IssueUpdate {
                                            remove_labels: vec![
                                                crate::plan::labels::INTEGRATION_PENDING
                                                    .to_string(),
                                            ],
                                            ..Default::default()
                                        },
                                    )
                                    .await
                                {
                                    tracing::warn!(
                                        %plan_id,
                                        epic_id = %epic.id,
                                        "failed to remove integration-pending label: {e}"
                                    );
                                }
                            }
                            did_work = true;
                        }
                    }
                }
                continue;
            }

            let mut update = spur_pm::IssueUpdate {
                status: Some(closed_status.clone()),
                ..Default::default()
            };
            if outcome.add_integration_pending {
                if !epic
                    .labels
                    .contains(&crate::plan::labels::INTEGRATION_PENDING.to_string())
                {
                    update
                        .add_labels
                        .push(crate::plan::labels::INTEGRATION_PENDING.to_string());
                }
            } else if epic
                .labels
                .contains(&crate::plan::labels::INTEGRATION_PENDING.to_string())
            {
                update
                    .remove_labels
                    .push(crate::plan::labels::INTEGRATION_PENDING.to_string());
            }

            self.pm.update_issue(&epic.id, update).await?;
            self.outcomes.lock().await.drop_plan(plan_id);
            if !has_epic_completion {
                if let Err(error) = crate::plan::emit_epic_completion_audit(
                    adv,
                    &epic.id,
                    plan_id,
                    outcome.audit_outcome,
                )
                .await
                {
                    tracing::warn!(
                        plan_id = %plan_id,
                        epic_id = %epic.id,
                        "epic completion audit emission failed on close: {error}"
                    );
                }
            }
            if !has_epic_completion {
                if let Some(sink) = self
                    .dispatch
                    .as_ref()
                    .and_then(|dispatch| dispatch.event_sink())
                {
                    sink.emit(spur_acp::SpurEventBody::PlanCompleted {
                        plan_id: plan_id.to_string(),
                        approved: outcome.approved_count,
                        rejected: outcome.rejected_count,
                        failed: outcome.failed_count,
                        cancelled: outcome.cancelled_count,
                    });
                }
                if let Some(dispatch) = self.dispatch.as_ref() {
                    crate::plan::push_plan_completed_continuation(
                        dispatch.continuation_ctx().as_ref(),
                        dispatch.materializer(),
                        dispatch.brain_session_id(),
                        plan_id,
                        outcome.approved_count,
                        outcome.rejected_count,
                        outcome.failed_count,
                        outcome.cancelled_count,
                    )
                    .await;
                }
            }
            self.maybe_emit_loop_run_record(adv, &epic, plan_id, &children, outcome)
                .await?;
            if outcome.add_integration_pending {
                if let Some(sink) = self
                    .dispatch
                    .as_ref()
                    .and_then(|dispatch| dispatch.event_sink())
                {
                    sink.emit(spur_acp::SpurEventBody::PlanReadyToMerge {
                        plan_id: plan_id.to_string(),
                    });
                }
            }
            did_work = true;
        }

        Ok(did_work)
    }
}
