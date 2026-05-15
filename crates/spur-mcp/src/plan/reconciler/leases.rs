use std::collections::HashMap;
use std::sync::Arc;

use spur_pm::IssueFilter;

use super::{task_id_from_labels_or_issue, Reconciler, ReconcilerDispatchCtx};
use crate::plan::outcomes::SkipReason;

impl Reconciler {
    pub(super) async fn sweep_expired_dispatch_leases(
        &self,
        dispatch: &ReconcilerDispatchCtx,
    ) -> anyhow::Result<bool> {
        crate::server::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            self.feature_gate.as_ref(),
        )
        .map_err(|error| anyhow::anyhow!(crate::server::feature_error_message(error)))?;
        let Some(adv) = self.pm.advanced() else {
            return Ok(false);
        };

        let mut summaries_by_id = HashMap::new();
        for status in ["open", "in_progress"] {
            let mut filter_labels = Vec::new();
            if let Some(plan_id) = self.plan_id.as_deref() {
                filter_labels.push(crate::plan::labels::plan_id(plan_id));
            }
            for summary in self
                .pm
                .list_issues(IssueFilter {
                    labels: filter_labels,
                    status: Some(status.to_string()),
                    issue_type: Some("task".to_string()),
                    limit: Some(10_000),
                    ..Default::default()
                })
                .await?
            {
                summaries_by_id.entry(summary.id.clone()).or_insert(summary);
            }
        }

        let now = chrono::Utc::now().timestamp();
        let mut did_work = false;
        let mut plan_activation_cache = HashMap::new();

        for summary in summaries_by_id.into_values() {
            let Some(expires_at) = summary
                .labels
                .iter()
                .filter_map(|label| crate::plan::labels::parse_lease_expires_at(label))
                .max()
            else {
                let plan_id = summary
                    .labels
                    .iter()
                    .find_map(|label| crate::plan::labels::parse_plan_id(label));
                let task_id = task_id_from_labels_or_issue(&summary.labels, &summary.id);
                self.record_skipped(plan_id, &task_id, SkipReason::MissingDispatchLeaseExpiry)
                    .await;
                continue;
            };
            if now <= expires_at {
                continue;
            }

            let Some(plan_id) = summary
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_plan_id(label))
                .map(str::to_string)
            else {
                tracing::warn!(
                    issue_id = %summary.id,
                    "expired dispatch lease has no plan id label; skipping"
                );
                continue;
            };
            let task_id = summary
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_plan_task_id(label))
                .unwrap_or_else(|| summary.id.clone());
            let write_state = self
                .plan_allows_writes(&plan_id, None, &mut plan_activation_cache)
                .await?;
            if let Some(reason) = write_state.skip_reason() {
                self.record_skipped(Some(&plan_id), &task_id, reason).await;
                continue;
            }

            let age_secs = now.saturating_sub(expires_at);
            let audits = crate::plan::projector::collect_sorted_audits_for_issue(
                &summary.id,
                adv.list_comments(&summary.id).await?,
            )?;
            let delegation_label = summary
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_delegation_id(label));
            let Some(delegation_id) = (match (
                crate::plan::projector::current_delegation_from_audits(&audits),
                delegation_label,
            ) {
                (Some(audit_id), Some(label_id)) if audit_id == label_id => Some(audit_id),
                (Some(audit_id), Some(_)) => {
                    crate::plan::projector::emit_label_audit_drift(
                        "delegation-id",
                        "mismatch",
                        &summary.id,
                    );
                    Some(audit_id)
                }
                (Some(audit_id), None) => {
                    crate::plan::projector::emit_label_audit_drift(
                        "delegation-id",
                        "audit_only",
                        &summary.id,
                    );
                    Some(audit_id)
                }
                (None, Some(_label_id)) => {
                    crate::plan::projector::emit_label_audit_drift(
                        "delegation-id",
                        "label_only",
                        &summary.id,
                    );
                    None
                }
                (None, None) => None,
            }) else {
                continue;
            };
            let (attempt, _) = crate::plan::projector::project_attempt_facts(&audits);
            let orphan_reason = format!("dispatch lease expired at {expires_at} (age {age_secs}s)");
            // Match on `delegation_id` only, ignoring `reason`. Safe because
            // delegation IDs are unique per dispatch lifecycle: dispatch mints
            // a fresh UUID and `clear_dispatch_intent` removes the label, so
            // any prior `DispatchOrphanCleared` (e.g. `restart-orphan-cleared`
            // from `resolve_dispatch_orphan`) on this delegation_id makes a
            // duplicate lease-expired audit redundant.
            let orphan_audit_exists = audits.iter().any(|audit| {
                matches!(
                    audit,
                    crate::plan::audit_sentinel::AuditSentinelKind::DispatchOrphanCleared {
                        delegation_id: found_delegation_id,
                        ..
                    } if found_delegation_id == &delegation_id
                )
            });
            if !orphan_audit_exists {
                adv.add_comment(
                    &summary.id,
                    &crate::plan::audit_sentinel::encode_comment(
                        &crate::plan::audit_sentinel::AuditSentinelKind::DispatchOrphanCleared {
                            delegation_id: delegation_id.clone(),
                            reason: orphan_reason.clone(),
                        },
                    ),
                )
                .await?;
            }

            let result = spur_acp::DelegationResult {
                status: spur_acp::DelegationStatus::Failed {
                    error: "dispatch lease expired".to_string(),
                },
                diff: None,
                diff_summary: None,
                summary: Some("dispatch lease expired".to_string()),
                estimated_cost_usd: 0.0,
                worker_branch: None,
                artifact: None,
            };
            let deferred = crate::plan::persist_system_completion_and_notify(
                self.pm.as_ref(),
                &summary.id,
                self.feature_gate.as_ref(),
                &plan_id,
                &delegation_id,
                crate::plan::audit_sentinel::CompletionState::Failed,
                &Some(Arc::clone(&self.fast_forward)),
                &result,
                &dispatch.brain_session_id,
                attempt,
                &dispatch.materializer,
                None,
                None,
                Some(&task_id),
            )
            .await?;

            tracing::warn!(
                target: "spur.dispatch_lease",
                %plan_id,
                %task_id,
                issue_id = %summary.id,
                %delegation_id,
                expires_at,
                age_secs,
                "reclaimed expired dispatch lease"
            );
            if let Some(sink) = dispatch.event_sink.as_deref() {
                sink.emit(spur_acp::SpurEventBody::DispatchLeaseExpired {
                    plan_id: plan_id.clone(),
                    task_id: task_id.clone(),
                    issue_id: summary.id.clone(),
                    delegation_id: delegation_id.clone(),
                    expired_at: expires_at,
                    age_secs,
                });
            }
            self.emit_snapshot_for_plan(&plan_id).await;
            if let Some(deferred) = deferred {
                deferred
                    .deliver(
                        dispatch.event_sink.as_deref(),
                        dispatch.continuation_ctx.as_ref(),
                    )
                    .await;
            }
            did_work = true;
        }

        Ok(did_work)
    }
}
