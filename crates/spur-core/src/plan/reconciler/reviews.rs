use crate::plan::audit_sentinel::{AuditSentinelKind, CompletionState};

impl super::Reconciler {
    /// Applies the unattended checker policy for system-owned L3 tasks.
    ///
    /// The maker's completion callback never approves its own work. A later
    /// reconciliation pass re-hydrates the durable plan and task audits, then
    /// approves only a current, non-superseded successful completion with a
    /// preserved worker branch and no unresolved signal labels. Ordinary
    /// brain-owned plans continue to require `review_task`.
    pub(super) async fn reconcile_system_l3_reviews(&self) -> anyhow::Result<bool> {
        if !matches!(self.config.plan_scope, super::PlanScope::SystemL3Only) {
            return Ok(false);
        }
        crate::server::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            self.feature_gate.as_ref(),
        )?;
        let Some(advanced) = self.pm.advanced() else {
            anyhow::bail!("system L3 review requires the beads advanced backend");
        };

        let mut did_work = false;
        for plan_id in self.scoped_plan_ids().await? {
            let projected = self.project_plan_from_beads(&plan_id).await?;
            for task in projected.tasks.iter().filter(|task| {
                matches!(
                    task.status,
                    crate::plan::PlanTaskStatus::AwaitingReview { .. }
                )
            }) {
                let Some(issue_id) = task.spec.issue_id.as_deref() else {
                    tracing::warn!(%plan_id, task_id = %task.spec.task_id, "system L3 checker skipped task without issue id");
                    continue;
                };
                let issue = self.pm.get_issue(issue_id).await?;
                if issue
                    .labels
                    .iter()
                    .any(|label| label.starts_with("signal:"))
                {
                    tracing::warn!(
                        %plan_id,
                        task_id = %task.spec.task_id,
                        %issue_id,
                        "system L3 checker left task awaiting review because it carries a signal"
                    );
                    continue;
                }

                let comments = advanced.list_comments(issue_id).await?;
                let audits =
                    crate::plan::projector::collect_sorted_audits_for_issue(issue_id, comments)?;
                let Some(delegation_id) = reviewable_completion(&audits) else {
                    tracing::warn!(
                        %plan_id,
                        task_id = %task.spec.task_id,
                        %issue_id,
                        "system L3 checker rejected non-reviewable durable completion facts"
                    );
                    continue;
                };
                if task.last_delegation_id.as_deref() != Some(delegation_id) {
                    tracing::warn!(
                        %plan_id,
                        task_id = %task.spec.task_id,
                        %issue_id,
                        expected = ?task.last_delegation_id,
                        actual = %delegation_id,
                        "system L3 checker rejected stale completion delegation"
                    );
                    continue;
                }

                let approval =
                    crate::plan::audit_sentinel::encode_comment(&AuditSentinelKind::Approval {
                        delegation_id: delegation_id.to_string(),
                    });
                let update = crate::plan::approve_review_update(self.pm.closed_status(), approval);
                crate::plan::apply_issue_update(self.pm.as_ref(), issue_id, update).await?;

                tracing::info!(
                    %plan_id,
                    task_id = %task.spec.task_id,
                    %issue_id,
                    %delegation_id,
                    "system L3 checker approved durable successful completion"
                );
                if let Some(sink) = self
                    .dispatch
                    .as_ref()
                    .and_then(|dispatch| dispatch.event_sink())
                {
                    sink.emit(spur_acp::SpurEventBody::PlanTaskReviewed {
                        plan_id: plan_id.clone(),
                        task_id: task.spec.task_id.clone(),
                        task_name: task.spec.issue_title.clone(),
                        decision: "approve".to_string(),
                        feedback: Some(
                            "system L3 checker verified durable successful completion".to_string(),
                        ),
                        attempt: task.attempt,
                        max_attempts: crate::plan::MAX_ATTEMPTS,
                    });
                }
                did_work = true;
            }
        }
        if did_work {
            self.fast_forward.notify_one();
        }
        Ok(did_work)
    }
}

fn reviewable_completion(audits: &[AuditSentinelKind]) -> Option<&str> {
    let latest = audits
        .iter()
        .rev()
        .find(|audit| matches!(audit, AuditSentinelKind::Completion { .. }))?;
    match latest {
        AuditSentinelKind::Completion {
            delegation_id,
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some(_),
            ..
        } if !delegation_id.is_empty() => Some(delegation_id.as_str()),
        AuditSentinelKind::Completion { .. } => None,
        _ => unreachable!("latest audit was filtered to a completion"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checker_accepts_only_non_superseded_success_completion() {
        let successful = AuditSentinelKind::Completion {
            delegation_id: "del-ok".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("spur/worker/ok".into()),
            result_summary: Some("done".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
            estimated_cost_micros: None,
        };
        assert_eq!(reviewable_completion(&[successful]), Some("del-ok"));

        let failed = AuditSentinelKind::Completion {
            delegation_id: "del-failed".into(),
            completion_state: CompletionState::Failed,
            superseded: false,
            worker_branch: None,
            result_summary: Some("failed".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
            estimated_cost_micros: None,
        };
        assert_eq!(reviewable_completion(&[failed]), None);
    }

    #[test]
    fn checker_never_falls_back_past_the_latest_completion() {
        let older_success = AuditSentinelKind::Completion {
            delegation_id: "del-old".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("spur/worker/old".into()),
            result_summary: Some("old success".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
            estimated_cost_micros: None,
        };
        let latest_failure = AuditSentinelKind::Completion {
            delegation_id: "del-new".into(),
            completion_state: CompletionState::Failed,
            superseded: false,
            worker_branch: None,
            result_summary: Some("new failure".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
            estimated_cost_micros: None,
        };

        assert_eq!(
            reviewable_completion(&[older_success, latest_failure]),
            None,
            "a newer failed completion must fence an older success"
        );
    }
}
