//! Tier 0 (bd-d1r): observe-only shadow projector.
//!
//! This module projects plan-task runtime state from audit sentinels only so we
//! can measure divergence from the legacy projector (which still reads issue
//! labels/status). It is temporary instrumentation and should be removed after
//! Tier 2 fully severs label/status reads from projection.

use spur_acp::{BrainSessionId, SessionId};

use super::{
    audit_sentinel::AuditSentinelKind, projector, PlanMergeState, PlanState, PlanTask,
    PlanTaskEntry, PlanTaskStatus,
};

#[derive(Debug, Clone)]
pub struct TaskAuditLog {
    pub spec: PlanTask,
    pub audits: Vec<AuditSentinelKind>,
}

pub fn shadow_project_plan_from_beads(plan_id: &str, audits: &[TaskAuditLog]) -> PlanState {
    let mut entries: Vec<PlanTaskEntry> = audits
        .iter()
        .map(|task| project_shadow_entry(&task.spec, &task.audits))
        .collect();
    projector::recompute_open_statuses(&mut entries);

    PlanState {
        plan_id: plan_id.to_string(),
        tasks: entries,
        brain_session_id: BrainSessionId::new(SessionId(format!("shadow-plan:{plan_id}"))),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: PlanMergeState::NotStarted,
        epic_id: None,
    }
}

fn project_shadow_entry(spec: &PlanTask, audits: &[AuditSentinelKind]) -> PlanTaskEntry {
    let (attempt_count, mut last_delegation_id) = projector::project_attempt_facts(audits);
    let history = projector::project_attempt_history(audits);

    let mut status = PlanTaskStatus::Pending;
    let mut worker_branch = None;
    let mut summary = None;
    let mut dispatched_base_oid = None;

    for audit in audits {
        match audit {
            AuditSentinelKind::Dispatch { delegation_id, .. } => {
                status = PlanTaskStatus::Dispatched {
                    delegation_id: delegation_id.clone(),
                };
                last_delegation_id = Some(delegation_id.clone());
            }
            AuditSentinelKind::Completion {
                completion_state,
                worker_branch: wb,
                result_summary,
                dispatched_base_oid: dbo,
                ..
            } => {
                if let Some(branch) = wb.clone() {
                    worker_branch = Some(branch);
                }
                if let Some(s) = result_summary.clone() {
                    summary = Some(s);
                }
                if let Some(base_oid) = dbo.clone() {
                    dispatched_base_oid = Some(base_oid);
                }

                status = match completion_state {
                    super::audit_sentinel::CompletionState::AwaitingReview => {
                        PlanTaskStatus::AwaitingReview {
                            summary: summary.clone(),
                        }
                    }
                    super::audit_sentinel::CompletionState::Failed => PlanTaskStatus::Failed {
                        error: summary
                            .clone()
                            .unwrap_or_else(|| "worker failed".to_string()),
                    },
                    super::audit_sentinel::CompletionState::Cancelled => {
                        PlanTaskStatus::Cancelled {
                            reason: summary
                                .clone()
                                .unwrap_or_else(|| "worker cancelled".to_string()),
                        }
                    }
                    super::audit_sentinel::CompletionState::Superseded => PlanTaskStatus::Pending,
                };
            }
            AuditSentinelKind::Approval { .. } => {
                status = PlanTaskStatus::Approved {
                    summary: summary.clone(),
                };
            }
            AuditSentinelKind::Rejection { feedback, .. } => {
                status = PlanTaskStatus::Rejected {
                    feedback: Some(feedback.clone()),
                };
            }
            AuditSentinelKind::ReviewFeedback { .. } | AuditSentinelKind::RetryRequested { .. } => {
                status = PlanTaskStatus::Pending;
            }
            AuditSentinelKind::EscalationRequested { last_error, .. } => {
                status = PlanTaskStatus::EscalatedToBrain {
                    last_error: last_error.clone(),
                };
            }
            AuditSentinelKind::Signal { kind, reason, .. }
                if kind == "integration-conflict" || kind == "integration_conflict" =>
            {
                #[derive(serde::Deserialize)]
                struct IntegrationConflictReason {
                    dep_task_id: String,
                    #[serde(default)]
                    files: Vec<String>,
                }
                let (dep_task_id, files) =
                    serde_json::from_str::<IntegrationConflictReason>(reason)
                        .map(|parsed| (parsed.dep_task_id, parsed.files))
                        .unwrap_or_else(|_| ("unknown".to_string(), Vec::new()));
                status = PlanTaskStatus::BlockedOnSetupConflict { dep_task_id, files };
            }
            _ => {}
        }
    }

    let attempt = projector::project_entry_attempt(audits, &status).max(attempt_count);

    PlanTaskEntry {
        spec: spec.clone(),
        status,
        result: None,
        worker_branch,
        attempt,
        history,
        last_delegation_id,
        dispatched_base_oid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::audit_sentinel::{AuditSentinelKind, CompletionState};

    fn sample_spec(task_id: &str) -> PlanTask {
        PlanTask {
            task_id: task_id.to_string(),
            agent: "codex".to_string(),
            task: "do work".to_string(),
            depends_on: Vec::new(),
            issue_id: Some(format!("I-{task_id}")),
            issue_title: Some(format!("Task {task_id}")),
            context_files: Vec::new(),
        }
    }

    #[test]
    fn parity_with_legacy_when_latest_completion_fields_are_present() {
        let audits = vec![
            AuditSentinelKind::Dispatch {
                delegation_id: "del-1".to_string(),
                worker: "codex".to_string(),
                attempt: 1,
            },
            AuditSentinelKind::Completion {
                delegation_id: "del-1".to_string(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("worker/task-1".to_string()),
                result_summary: Some("done".to_string()),
                artifact_uri: None,
                dispatched_base_oid: Some("abc123".to_string()),
            },
            AuditSentinelKind::Approval {
                delegation_id: "del-1".to_string(),
            },
        ];

        let spec = sample_spec("T1");
        let shadow = project_shadow_entry(&spec, &audits);

        let legacy_summary = projector::latest_completion_facts(&audits)
            .and_then(|(_, _, result_summary, _, _)| result_summary);
        let legacy_status = PlanTaskStatus::Approved {
            summary: legacy_summary,
        };
        let legacy_worker_branch = projector::latest_completion_facts(&audits)
            .and_then(|(_, worker_branch, _, _, _)| worker_branch);
        let legacy_dispatched_base_oid = projector::latest_completion_facts(&audits)
            .and_then(|(_, _, _, _, dispatched_base_oid)| dispatched_base_oid);

        assert_eq!(
            format!("{:?}", shadow.status),
            format!("{:?}", legacy_status)
        );
        assert_eq!(shadow.worker_branch, legacy_worker_branch);
        assert_eq!(shadow.dispatched_base_oid, legacy_dispatched_base_oid);
    }

    #[test]
    fn bd334_reproducer_shadow_keeps_prior_worker_branch() {
        let audits = vec![
            AuditSentinelKind::Dispatch {
                delegation_id: "del-1".to_string(),
                worker: "codex".to_string(),
                attempt: 1,
            },
            AuditSentinelKind::Completion {
                delegation_id: "del-1".to_string(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("worker/task-attempt-1".to_string()),
                result_summary: Some("first summary".to_string()),
                artifact_uri: None,
                dispatched_base_oid: Some("base-1".to_string()),
            },
            AuditSentinelKind::ReviewFeedback {
                delegation_id: "del-1".to_string(),
                attempt: 1,
                feedback: "fix please".to_string(),
                worker_branch: Some("worker/task-attempt-1".to_string()),
                summary: Some("first summary".to_string()),
                reuse_prior_worktree: Some(true),
            },
            AuditSentinelKind::Dispatch {
                delegation_id: "del-2".to_string(),
                worker: "codex".to_string(),
                attempt: 2,
            },
            AuditSentinelKind::Completion {
                delegation_id: "del-2".to_string(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: None,
                result_summary: Some("second summary".to_string()),
                artifact_uri: None,
                dispatched_base_oid: None,
            },
        ];

        let spec = sample_spec("T2");
        let shadow = project_shadow_entry(&spec, &audits);
        let legacy_worker_branch = projector::latest_completion_facts(&audits)
            .and_then(|(_, worker_branch, _, _, _)| worker_branch);

        assert_eq!(legacy_worker_branch, None);
        assert_eq!(
            shadow.worker_branch.as_deref(),
            Some("worker/task-attempt-1")
        );
        assert!(matches!(
            shadow.status,
            PlanTaskStatus::AwaitingReview { .. }
        ));
    }
}
