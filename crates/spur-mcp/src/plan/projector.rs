use std::collections::{HashMap, HashSet};

use spur_acp::{BrainSessionId, SessionId};

use super::{PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
use crate::plan::audit_sentinel::{AuditSentinelKind, CompletionState, EpicCompletionOutcome};

const LEGACY_DELEGATION_ID_PREFIX: &str = "delegation-id:";
const LEGACY_READY_FOR_REVIEW: &str = "ready-for-review";
const LEGACY_REVIEW_REJECTED: &str = "review-rejected";
const MUTATION_ID_PREFIX: &str = "spur:mutation-id:";
const SUPERSEDED_BY_PREFIX: &str = "spur:superseded-by:";

pub fn sort_projection_comments(mut comments: Vec<spur_pm::Comment>) -> Vec<spur_pm::Comment> {
    comments.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    comments
}

pub fn collect_sorted_audits(
    comments: Vec<spur_pm::Comment>,
) -> Vec<crate::plan::audit_sentinel::AuditSentinelKind> {
    sort_projection_comments(comments)
        .into_iter()
        .filter_map(|comment| crate::plan::audit_sentinel::parse_comment(&comment.body))
        .filter_map(|result| result.ok())
        .collect()
}

pub fn project_attempt_facts(audits: &[AuditSentinelKind]) -> (u32, Option<String>) {
    let mut attempt = 1u32;
    let mut last_delegation_id = None;

    for audit in audits {
        if let AuditSentinelKind::Dispatch {
            delegation_id,
            attempt: dispatch_attempt,
            ..
        } = audit
        {
            attempt = *dispatch_attempt;
            last_delegation_id = Some(delegation_id.clone());
        }
    }

    (attempt, last_delegation_id)
}

pub fn latest_completion_facts(
    audits: &[AuditSentinelKind],
) -> Option<(
    CompletionState,
    Option<String>,
    Option<String>,
    bool,
    Option<String>,
)> {
    let mut latest = None;

    for audit in audits {
        if let AuditSentinelKind::Completion {
            completion_state,
            worker_branch,
            result_summary,
            superseded,
            dispatched_base_oid,
            ..
        } = audit
        {
            latest = Some((
                *completion_state,
                worker_branch.clone(),
                result_summary.clone(),
                *superseded,
                dispatched_base_oid.clone(),
            ));
        }
    }

    latest
}

pub fn latest_task_spec(audits: &[AuditSentinelKind]) -> Option<(String, Vec<String>)> {
    for audit in audits.iter().rev() {
        if let AuditSentinelKind::TaskSpec {
            task_id,
            context_files,
        } = audit
        {
            return Some((task_id.clone(), context_files.clone()));
        }
    }

    None
}

/// Derive the human-readable outcome summary string from the most recent
/// `EpicCompletion` audit matching the given `plan_id` and `epic_id`.
///
/// This is the durable source of truth for PR body text in the auto-merge
/// hook; callers must not use live-projected `ProjectedEpicCompletion`
/// directly.
pub fn epic_completion_outcome_summary(
    audits: &[AuditSentinelKind],
    plan_id: &str,
    epic_id: &str,
) -> Option<&'static str> {
    for audit in audits.iter().rev() {
        if let AuditSentinelKind::EpicCompletion {
            outcome,
            plan_id: pid,
            epic_id: eid,
        } = audit
        {
            if pid == plan_id && eid == epic_id {
                return Some(match outcome {
                    EpicCompletionOutcome::AllApproved => "All approved",
                    EpicCompletionOutcome::TerminalWithFailures => "Terminal with failures",
                });
            }
        }
    }
    None
}

fn parse_delegation_id_compat(label: &str) -> Option<&str> {
    crate::plan::labels::parse_delegation_id(label)
        .or_else(|| label.strip_prefix(LEGACY_DELEGATION_ID_PREFIX))
}

pub(crate) fn has_ready_for_review_label_compat(labels: &[String]) -> bool {
    labels.iter().any(|label| {
        label == crate::plan::labels::READY_FOR_REVIEW || label == LEGACY_READY_FOR_REVIEW
    })
}

fn has_review_rejected_label(labels: &[String]) -> bool {
    labels.iter().any(|label| {
        label == crate::plan::labels::REVIEW_REJECTED || label == LEGACY_REVIEW_REJECTED
    })
}

fn task_id_for_issue(issue: &spur_pm::Issue) -> String {
    issue
        .labels
        .iter()
        .find_map(|label| crate::plan::labels::parse_plan_task_id(label))
        .unwrap_or_else(|| issue.id.clone())
}

fn agent_for_issue(issue: &spur_pm::Issue) -> (String, bool) {
    if let Some(agent) = issue
        .labels
        .iter()
        .find_map(|label| crate::plan::labels::parse_agent(label))
    {
        (agent.to_string(), false)
    } else {
        ("codex".to_string(), true)
    }
}

fn superseded_status(issue: &spur_pm::Issue) -> Option<PlanTaskStatus> {
    let by: Vec<String> = issue
        .labels
        .iter()
        .filter_map(|label| label.strip_prefix(SUPERSEDED_BY_PREFIX))
        .map(str::to_string)
        .collect();
    let mutation_id = issue
        .labels
        .iter()
        .find_map(|label| label.strip_prefix(MUTATION_ID_PREFIX))
        .map(str::to_string);

    if by.is_empty() && mutation_id.is_none() {
        None
    } else {
        Some(PlanTaskStatus::Superseded {
            mutation_id: mutation_id.unwrap_or_else(|| "unknown".to_string()),
            by,
        })
    }
}

pub fn project_status_for_issue(
    issue: &spur_pm::Issue,
    audits: &[AuditSentinelKind],
    ready_now: bool,
    closed_status: &str,
) -> PlanTaskStatus {
    if issue.status == closed_status {
        return project_closed_status(issue, audits);
    }

    if let Some(delegation_id) = issue
        .labels
        .iter()
        .find_map(|label| parse_delegation_id_compat(label))
    {
        return PlanTaskStatus::Dispatched {
            delegation_id: delegation_id.to_string(),
        };
    }

    if has_ready_for_review_label_compat(&issue.labels) {
        let summary =
            latest_completion_facts(audits).and_then(|(_, _, result_summary, _, _)| result_summary);
        return PlanTaskStatus::AwaitingReview { summary };
    }

    if ready_now {
        PlanTaskStatus::Ready
    } else {
        PlanTaskStatus::Pending
    }
}

pub fn project_closed_status(
    issue: &spur_pm::Issue,
    audits: &[AuditSentinelKind],
) -> PlanTaskStatus {
    for audit in audits.iter().rev() {
        match audit {
            AuditSentinelKind::Approval { .. } => {
                let summary = latest_completion_facts(audits)
                    .and_then(|(_, _, result_summary, _, _)| result_summary);
                return PlanTaskStatus::Approved { summary };
            }
            AuditSentinelKind::Rejection { feedback, .. } => {
                return PlanTaskStatus::Rejected {
                    feedback: Some(feedback.clone()),
                };
            }
            AuditSentinelKind::Completion {
                completion_state,
                result_summary,
                ..
            } => match completion_state {
                CompletionState::Failed => {
                    return PlanTaskStatus::Failed {
                        error: result_summary
                            .clone()
                            .unwrap_or_else(|| "worker failed".to_string()),
                    };
                }
                CompletionState::Cancelled => {
                    return PlanTaskStatus::Cancelled {
                        reason: result_summary
                            .clone()
                            .unwrap_or_else(|| "worker cancelled".to_string()),
                    };
                }
                CompletionState::Superseded => {
                    if let Some(status) = superseded_status(issue) {
                        return status;
                    }
                    return PlanTaskStatus::Pending;
                }
                CompletionState::AwaitingReview => {}
            },
            _ => {}
        }
    }

    if let Some(status) = superseded_status(issue) {
        status
    } else if has_review_rejected_label(&issue.labels) {
        PlanTaskStatus::Rejected { feedback: None }
    } else {
        let summary =
            latest_completion_facts(audits).and_then(|(_, _, result_summary, _, _)| result_summary);
        PlanTaskStatus::Approved { summary }
    }
}

pub fn recompute_open_statuses(tasks: &mut [PlanTaskEntry]) {
    let approved_or_cancelled: HashSet<String> = tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                PlanTaskStatus::Approved { .. }
                    | PlanTaskStatus::Cancelled { .. }
                    | PlanTaskStatus::Superseded { .. }
            )
        })
        .map(|task| task.spec.task_id.clone())
        .collect();

    for task in tasks {
        if matches!(task.status, PlanTaskStatus::Pending | PlanTaskStatus::Ready) {
            let ready = task
                .spec
                .depends_on
                .iter()
                .all(|dependency| approved_or_cancelled.contains(dependency));
            task.status = if ready {
                PlanTaskStatus::Ready
            } else {
                PlanTaskStatus::Pending
            };
        }
    }
}

pub fn plan_submit_base_snapshot(audits: &[AuditSentinelKind]) -> Option<String> {
    audits.iter().rev().find_map(|audit| {
        if let AuditSentinelKind::PlanSubmit {
            base_snapshot_branch,
            ..
        } = audit
        {
            base_snapshot_branch.clone()
        } else {
            None
        }
    })
}

pub fn plan_submit_base_snapshot_oid(audits: &[AuditSentinelKind]) -> Option<String> {
    audits.iter().rev().find_map(|audit| {
        if let AuditSentinelKind::PlanSubmit {
            base_snapshot_oid, ..
        } = audit
        {
            base_snapshot_oid.clone()
        } else {
            None
        }
    })
}

pub fn plan_submit_brain_session_id(audits: &[AuditSentinelKind]) -> Option<BrainSessionId> {
    audits.iter().rev().find_map(|audit| {
        if let AuditSentinelKind::PlanSubmit {
            brain_session_id: Some(brain_session_id),
            ..
        } = audit
        {
            Some(BrainSessionId::new(SessionId(brain_session_id.clone())))
        } else {
            None
        }
    })
}

pub async fn project_plan_from_beads(
    pm: &spur_pm::PmService,
    plan_id: &str,
    feature_gate: &spur_license::FeatureGate,
) -> anyhow::Result<PlanState> {
    let mut summary_by_id = HashMap::new();
    for status in [
        Some("open".to_string()),
        Some("in_progress".to_string()),
        Some(pm.closed_status().to_string()),
    ] {
        for summary in pm
            .list_issues(spur_pm::IssueFilter {
                labels: vec![crate::plan::labels::plan_id(plan_id)],
                status,
                limit: Some(1_000),
                ..Default::default()
            })
            .await?
        {
            summary_by_id.insert(summary.id.clone(), summary);
        }
    }
    let summaries: Vec<spur_pm::IssueSummary> = summary_by_id.into_values().collect();
    let mut issues = Vec::with_capacity(summaries.len());
    for summary in summaries {
        issues.push(pm.get_issue(&summary.id).await?);
    }

    issues.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });

    let epic = issues
        .iter()
        .find(|issue| issue.issue_type.as_deref() == Some("epic"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("persisted plan {plan_id} has no epic"))?;
    let tasks: Vec<spur_pm::Issue> = issues
        .into_iter()
        .filter(|issue| issue.issue_type.as_deref() == Some("task"))
        .collect();

    crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    )
    .map_err(|error| anyhow::anyhow!(crate::server::feature_error_message(error)))?;
    let adv = pm
        .advanced()
        .ok_or_else(|| anyhow::anyhow!("persisted projector requires beads backend"))?;
    let epic_audits = collect_sorted_audits(adv.list_comments(&epic.id).await?);
    let closed_status = pm.closed_status().to_string();
    struct ProjectedTask {
        issue: spur_pm::Issue,
        audits: Vec<crate::plan::audit_sentinel::AuditSentinelKind>,
        task_id: String,
        context_files: Vec<String>,
    }

    let mut projected_tasks = Vec::with_capacity(tasks.len());
    for task_issue in tasks {
        let audits = collect_sorted_audits(adv.list_comments(&task_issue.id).await?);
        let task_spec = latest_task_spec(&audits);
        let (task_id, context_files) =
            task_spec.unwrap_or_else(|| (task_id_for_issue(&task_issue), Vec::new()));
        projected_tasks.push(ProjectedTask {
            issue: task_issue,
            audits,
            task_id,
            context_files,
        });
    }

    let task_id_by_issue_id: HashMap<String, String> = projected_tasks
        .iter()
        .map(|task| (task.issue.id.clone(), task.task_id.clone()))
        .collect();
    let mut entries = Vec::with_capacity(projected_tasks.len());

    for projected_task in projected_tasks {
        let (attempt, last_delegation_id) = project_attempt_facts(&projected_task.audits);
        let completion = latest_completion_facts(&projected_task.audits);
        let (worker_branch, dispatched_base_oid) = completion
            .map(|(_, worker_branch, _, _, dispatched_base_oid)| {
                (worker_branch, dispatched_base_oid)
            })
            .unwrap_or((None, None));
        let status = project_status_for_issue(
            &projected_task.issue,
            &projected_task.audits,
            false,
            &closed_status,
        );
        let depends_on = projected_task
            .issue
            .blocked_by
            .iter()
            .filter(|dependency| *dependency != &epic.id)
            .map(|dependency| {
                task_id_by_issue_id
                    .get(dependency)
                    .cloned()
                    .unwrap_or_else(|| dependency.clone())
            })
            .collect();

        let (agent, _agent_fallback) = agent_for_issue(&projected_task.issue);

        entries.push(PlanTaskEntry {
            spec: PlanTask {
                task_id: projected_task.task_id,
                agent,
                task: projected_task.issue.body.clone(),
                depends_on,
                issue_id: Some(projected_task.issue.id.clone()),
                context_files: projected_task.context_files,
            },
            status,
            result: None,
            worker_branch,
            attempt,
            history: Vec::new(),
            last_delegation_id,
            dispatched_base_oid,
        });
    }

    recompute_open_statuses(&mut entries);

    let brain_session_id = plan_submit_brain_session_id(&epic_audits)
        .unwrap_or_else(|| BrainSessionId::new(SessionId(format!("persisted-plan:{plan_id}"))));

    Ok(PlanState {
        plan_id: plan_id.to_string(),
        tasks: entries,
        brain_session_id,
        base_snapshot_branch: plan_submit_base_snapshot(&epic_audits),
        base_snapshot_oid: plan_submit_base_snapshot_oid(&epic_audits),
        merge_state: crate::plan::PlanMergeState::NotStarted,
        epic_id: Some(epic.id),
    })
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use spur_pm::Comment;

    use super::{AuditSentinelKind, CompletionState, PlanTask, PlanTaskEntry, PlanTaskStatus};
    use crate::plan::audit_sentinel::EpicCompletionOutcome;

    #[test]
    fn sort_projection_comments_orders_by_created_at_then_id() {
        let comments = vec![
            Comment {
                id: "c-3".into(),
                body: "third".into(),
                actor: "spur".into(),
                created_at: Utc.with_ymd_and_hms(2026, 4, 21, 10, 0, 2).unwrap(),
            },
            Comment {
                id: "c-2".into(),
                body: "same-second-b".into(),
                actor: "spur".into(),
                created_at: Utc.with_ymd_and_hms(2026, 4, 21, 10, 0, 1).unwrap(),
            },
            Comment {
                id: "c-1".into(),
                body: "same-second-a".into(),
                actor: "spur".into(),
                created_at: Utc.with_ymd_and_hms(2026, 4, 21, 10, 0, 1).unwrap(),
            },
        ];

        let ordered = super::sort_projection_comments(comments);
        let ids: Vec<String> = ordered.into_iter().map(|comment| comment.id).collect();
        assert_eq!(
            ids,
            vec!["c-1".to_string(), "c-2".to_string(), "c-3".to_string()]
        );
    }

    #[test]
    fn collect_sorted_audits_skips_non_audit_comments() {
        let comments = vec![
            Comment {
                id: "c-2".into(),
                body: crate::plan::audit_sentinel::encode_comment(
                    &crate::plan::audit_sentinel::AuditSentinelKind::Approval {
                        delegation_id: "del-A".into(),
                    },
                ),
                actor: "spur".into(),
                created_at: Utc.with_ymd_and_hms(2026, 4, 21, 10, 0, 2).unwrap(),
            },
            Comment {
                id: "c-1".into(),
                body: "ordinary human comment".into(),
                actor: "human".into(),
                created_at: Utc.with_ymd_and_hms(2026, 4, 21, 10, 0, 1).unwrap(),
            },
        ];

        let audits = super::collect_sorted_audits(comments);
        assert_eq!(audits.len(), 1);
        assert!(matches!(
            audits[0],
            crate::plan::audit_sentinel::AuditSentinelKind::Approval { .. }
        ));
    }

    #[test]
    fn latest_dispatch_sets_attempt_and_last_delegation_id() {
        let audits = vec![
            AuditSentinelKind::Dispatch {
                delegation_id: "del-1".into(),
                worker: "codex".into(),
                attempt: 1,
            },
            AuditSentinelKind::Dispatch {
                delegation_id: "del-2".into(),
                worker: "codex".into(),
                attempt: 2,
            },
        ];

        let (attempt, last_delegation_id) = super::project_attempt_facts(&audits);
        assert_eq!(attempt, 2);
        assert_eq!(last_delegation_id.as_deref(), Some("del-2"));
    }

    fn issue(
        id: &str,
        status: &str,
        labels: Vec<String>,
        blocked_by: Vec<String>,
    ) -> spur_pm::Issue {
        spur_pm::Issue {
            id: id.into(),
            source: spur_pm::PmSource::Beads,
            title: format!("Issue {id}"),
            body: format!("Body for {id}"),
            status: status.into(),
            labels,
            assignee: None,
            url: format!("beads://{id}"),
            priority: Some(2),
            issue_type: Some("task".into()),
            blocked_by,
            due_at: None,
            created_at: Utc.with_ymd_and_hms(2026, 4, 21, 10, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 4, 21, 10, 0, 0).unwrap(),
        }
    }

    #[test]
    fn latest_completion_carries_state_branch_and_summary() {
        let audits = vec![AuditSentinelKind::Completion {
            delegation_id: "del-1".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("feat/task".into()),
            result_summary: Some("3 files changed".into()),
            artifact_uri: None,
            dispatched_base_oid: Some("base-oid".into()),
        }];

        let facts = super::latest_completion_facts(&audits).expect("completion facts");
        assert_eq!(facts.0, CompletionState::AwaitingReview);
        assert_eq!(facts.1.as_deref(), Some("feat/task"));
        assert_eq!(facts.2.as_deref(), Some("3 files changed"));
        assert!(!facts.3);
        assert_eq!(facts.4.as_deref(), Some("base-oid"));
    }

    #[test]
    fn open_task_with_delegation_label_projects_dispatched() {
        let issue = issue(
            "bd-2",
            "open",
            vec![crate::plan::labels::delegation_id("del-A")],
            Vec::new(),
        );

        let status = super::project_status_for_issue(&issue, &[], true, "closed");
        assert!(
            matches!(status, PlanTaskStatus::Dispatched { delegation_id } if delegation_id == "del-A")
        );
    }

    #[test]
    fn open_task_with_ready_for_review_projects_awaiting_review() {
        let issue = issue(
            "bd-2",
            "open",
            vec![crate::plan::labels::READY_FOR_REVIEW.to_string()],
            Vec::new(),
        );
        let audits = vec![AuditSentinelKind::Completion {
            delegation_id: "del-A".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("feat/task".into()),
            result_summary: Some("looks good".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
        }];

        let status = super::project_status_for_issue(&issue, &audits, true, "closed");
        assert!(
            matches!(status, PlanTaskStatus::AwaitingReview { summary } if summary.as_deref() == Some("looks good"))
        );
    }

    #[test]
    fn closed_task_with_rejection_audit_projects_rejected() {
        let issue = issue(
            "bd-9",
            "closed",
            vec![crate::plan::labels::REVIEW_REJECTED.to_string()],
            Vec::new(),
        );
        let audits = vec![AuditSentinelKind::Rejection {
            delegation_id: "del-A".into(),
            feedback: "needs a retry".into(),
        }];

        let status = super::project_closed_status(&issue, &audits);
        assert!(
            matches!(status, PlanTaskStatus::Rejected { feedback } if feedback.as_deref() == Some("needs a retry"))
        );
    }

    #[test]
    fn closed_task_with_failed_completion_projects_failed() {
        let issue = issue("bd-9", "closed", Vec::new(), Vec::new());
        let audits = vec![AuditSentinelKind::Completion {
            delegation_id: "del-A".into(),
            completion_state: CompletionState::Failed,
            superseded: false,
            worker_branch: None,
            result_summary: Some("cargo test failed".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
        }];

        let status = super::project_closed_status(&issue, &audits);
        assert!(matches!(status, PlanTaskStatus::Failed { error } if error == "cargo test failed"));
    }

    #[test]
    fn recompute_open_statuses_marks_unblocked_pending_tasks_ready() {
        let mut tasks = vec![
            PlanTaskEntry {
                spec: PlanTask {
                    task_id: "a".into(),
                    agent: "codex".into(),
                    task: "A".into(),
                    depends_on: Vec::new(),
                    issue_id: Some("bd-1".into()),
                    context_files: Vec::new(),
                },
                status: PlanTaskStatus::Approved { summary: None },
                result: None,
                worker_branch: None,
                attempt: 1,
                history: Vec::new(),
                last_delegation_id: Some("del-a".into()),
                dispatched_base_oid: None,
            },
            PlanTaskEntry {
                spec: PlanTask {
                    task_id: "b".into(),
                    agent: "codex".into(),
                    task: "B".into(),
                    depends_on: vec!["a".into()],
                    issue_id: Some("bd-2".into()),
                    context_files: Vec::new(),
                },
                status: PlanTaskStatus::Pending,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: Vec::new(),
                last_delegation_id: None,
                dispatched_base_oid: None,
            },
        ];

        super::recompute_open_statuses(&mut tasks);
        assert!(matches!(tasks[1].status, PlanTaskStatus::Ready));
    }

    #[test]
    fn plan_submit_audit_reconstructs_base_snapshot_branch() {
        let audits = vec![AuditSentinelKind::PlanSubmit {
            plan_id: "plan-1".into(),
            epic_issue_id: "bd-epic".into(),
            task_ids: vec!["bd-1".into()],
            base_snapshot_branch: Some("refs/heads/main".into()),
            base_snapshot_oid: None,
            execution_mode: Some("submit_plan".into()),
            brain_session_id: None,
        }];

        let base_snapshot_branch = super::plan_submit_base_snapshot(&audits);
        assert_eq!(base_snapshot_branch.as_deref(), Some("refs/heads/main"));
    }

    #[test]
    fn plan_submit_audit_reconstructs_brain_session_id() {
        let audits = vec![AuditSentinelKind::PlanSubmit {
            plan_id: "plan-1".into(),
            epic_issue_id: "bd-epic".into(),
            task_ids: vec!["bd-1".into()],
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            execution_mode: Some("submit_plan".into()),
            brain_session_id: Some("brain-123".into()),
        }];

        let brain_session_id = super::plan_submit_brain_session_id(&audits);
        assert_eq!(
            brain_session_id
                .as_ref()
                .map(|id| id.as_session_id().0.as_str()),
            Some("brain-123")
        );
    }

    #[test]
    fn epic_completion_outcome_summary_derives_all_approved_from_durable_audit() {
        let audits = vec![AuditSentinelKind::EpicCompletion {
            outcome: EpicCompletionOutcome::AllApproved,
            plan_id: "P1".into(),
            epic_id: "bd-epic-1".into(),
        }];
        assert_eq!(
            super::epic_completion_outcome_summary(&audits, "P1", "bd-epic-1"),
            Some("All approved")
        );
    }

    #[test]
    fn epic_completion_outcome_summary_derives_terminal_with_failures_from_durable_audit() {
        let audits = vec![AuditSentinelKind::EpicCompletion {
            outcome: EpicCompletionOutcome::TerminalWithFailures,
            plan_id: "P1".into(),
            epic_id: "bd-epic-1".into(),
        }];
        assert_eq!(
            super::epic_completion_outcome_summary(&audits, "P1", "bd-epic-1"),
            Some("Terminal with failures")
        );
    }

    #[test]
    fn epic_completion_outcome_summary_returns_none_when_missing() {
        let audits = vec![AuditSentinelKind::Approval {
            delegation_id: "del-A".into(),
        }];
        assert_eq!(
            super::epic_completion_outcome_summary(&audits, "P1", "bd-epic-1"),
            None
        );
    }

    #[test]
    fn epic_completion_outcome_summary_uses_latest_matching_audit() {
        let audits = vec![
            AuditSentinelKind::EpicCompletion {
                outcome: EpicCompletionOutcome::AllApproved,
                plan_id: "P1".into(),
                epic_id: "bd-epic-1".into(),
            },
            AuditSentinelKind::EpicCompletion {
                outcome: EpicCompletionOutcome::TerminalWithFailures,
                plan_id: "P1".into(),
                epic_id: "bd-epic-1".into(),
            },
        ];
        assert_eq!(
            super::epic_completion_outcome_summary(&audits, "P1", "bd-epic-1"),
            Some("Terminal with failures")
        );
    }
}
