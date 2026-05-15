use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use spur_acp::{BrainSessionId, SessionId};

use super::{PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
use crate::plan::audit_sentinel::{AuditSentinelKind, CompletionState, EpicCompletionOutcome};
use crate::plan::shadow_projector::{shadow_project_plan_from_beads, TaskAuditLog};

const LEGACY_DELEGATION_ID_PREFIX: &str = "delegation-id:";
const LEGACY_READY_FOR_REVIEW: &str = "ready-for-review";
const LEGACY_REVIEW_REJECTED: &str = "review-rejected";
const MUTATION_ID_PREFIX: &str = "spur:mutation-id:";
const SUPERSEDED_BY_PREFIX: &str = "spur:superseded-by:";
const SHADOW_PROJECTOR_ENV: &str = "SPUR_SHADOW_PROJECTOR";

fn shadow_projector_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        !matches!(
            std::env::var(SHADOW_PROJECTOR_ENV).ok().as_deref(),
            Some("off") | Some("OFF") | Some("0") | Some("false") | Some("FALSE")
        )
    })
}

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
) -> anyhow::Result<Vec<crate::plan::audit_sentinel::AuditSentinelKind>> {
    collect_sorted_audits_for_issue("<unknown>", comments)
}

pub fn collect_sorted_audits_for_issue(
    issue_id: &str,
    comments: Vec<spur_pm::Comment>,
) -> anyhow::Result<Vec<crate::plan::audit_sentinel::AuditSentinelKind>> {
    let mut audits = Vec::new();
    for comment in sort_projection_comments(comments) {
        match crate::plan::audit_sentinel::parse_comment(&comment.body) {
            Some(Ok(kind)) => audits.push(kind),
            Some(Err(crate::plan::audit_sentinel::ParseError::Critical { kind, source })) => {
                return Err(anyhow::anyhow!(
                    "critical audit sentinel parse failed for issue {issue_id}, comment {} (kind={kind}): {source}",
                    comment.id
                ));
            }
            Some(Err(error @ crate::plan::audit_sentinel::ParseError::Informational { .. })) => {
                tracing::warn!(
                    target: "spur.audit.parse_failure",
                    issue_id = %issue_id,
                    comment_id = %comment.id,
                    error = %error,
                    "audit sentinel parse failed; comment dropped from projection",
                );
            }
            None => {}
        }
    }
    Ok(audits)
}

pub fn project_attempt_facts(audits: &[AuditSentinelKind]) -> (u32, Option<String>) {
    let mut count = 0u32;
    let mut last_delegation_id = None;
    for audit in audits {
        if let AuditSentinelKind::Dispatch { delegation_id, .. } = audit {
            count += 1;
            last_delegation_id = Some(delegation_id.clone());
        }
    }
    // attempt 1 == 1st dispatch ever; saturating start ensures pre-dispatch tasks see 1.
    (count.max(1), last_delegation_id)
}

fn latest_audit_advances_next_attempt(audits: &[AuditSentinelKind]) -> bool {
    for audit in audits.iter().rev() {
        match audit {
            AuditSentinelKind::RetryRequested { .. } | AuditSentinelKind::ReviewFeedback { .. } => {
                return true
            }
            AuditSentinelKind::Dispatch { .. }
            | AuditSentinelKind::Completion { .. }
            | AuditSentinelKind::Approval { .. }
            | AuditSentinelKind::Rejection { .. } => return false,
            _ => {}
        }
    }
    false
}

pub fn project_entry_attempt(audits: &[AuditSentinelKind], status: &PlanTaskStatus) -> u32 {
    let (attempt, _) = project_attempt_facts(audits);
    if matches!(status, PlanTaskStatus::Pending | PlanTaskStatus::Ready)
        && latest_audit_advances_next_attempt(audits)
    {
        attempt.saturating_add(1)
    } else {
        attempt
    }
}

pub fn project_attempt_history(audits: &[AuditSentinelKind]) -> Vec<super::AttemptRecord> {
    audits
        .iter()
        .filter_map(|audit| match audit {
            AuditSentinelKind::ReviewFeedback {
                delegation_id: _,
                attempt,
                feedback,
                worker_branch,
                summary,
                reuse_prior_worktree,
            } => Some(super::AttemptRecord {
                attempt: *attempt,
                worker_branch: worker_branch.clone(),
                diff_summary: None,
                summary: summary.clone(),
                feedback: feedback.clone(),
                dispatched_base_oid: None,
                reuse_prior_worktree: *reuse_prior_worktree,
            }),
            AuditSentinelKind::RetryRequested {
                delegation_id: _,
                attempt,
                error,
                worker_branch,
                ..
            } => Some(super::AttemptRecord {
                attempt: *attempt,
                worker_branch: worker_branch.clone(),
                diff_summary: None,
                summary: None,
                feedback: super::worker_failure_recovery_feedback(error),
                dispatched_base_oid: None,
                reuse_prior_worktree: None,
            }),
            _ => None,
        })
        .collect()
}

pub type CompletionFacts = (
    CompletionState,
    Option<String>,
    Option<String>,
    bool,
    Option<String>,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalAuditKind {
    Approval,
    Rejection,
    CompletionFailed,
    CompletionCancelled,
}

pub fn latest_completion_facts(audits: &[AuditSentinelKind]) -> Option<CompletionFacts> {
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

    // Failed/Cancelled completions legitimately carry `None` worker_branch
    // when the worker never started (lease expiry, setup conflict).
    assert!(
        latest.as_ref().is_none_or(
            |(completion_state, worker_branch, _, _, _)| {
                !matches!(completion_state, CompletionState::AwaitingReview)
                    || worker_branch.is_some()
            }
        ),
        "invariant:latest_completion_facts worker_branch missing violated expected latest AwaitingReview Completion facts to include worker_branch, got latest={:?}",
        latest,
    );
    latest
}

/// Tier-1 projector inversion consumer: derive the currently active delegation
/// from durable audit comments only.
pub fn current_delegation_from_audits(audits: &[AuditSentinelKind]) -> Option<String> {
    let mut current: Option<String> = None;
    for audit in audits {
        match audit {
            AuditSentinelKind::Dispatch { delegation_id, .. } => {
                current = Some(delegation_id.clone());
            }
            AuditSentinelKind::Completion { delegation_id, .. }
            | AuditSentinelKind::Approval { delegation_id }
            | AuditSentinelKind::Rejection { delegation_id, .. }
            | AuditSentinelKind::ReviewFeedback { delegation_id, .. }
            | AuditSentinelKind::DispatchOrphanCleared { delegation_id, .. }
                if current.as_deref() == Some(delegation_id) =>
            {
                current = None;
            }
            _ => {}
        }
    }
    current
}

/// Tier-1 index-maintenance reconciler consumer: derive whether the current
/// attempt is awaiting review from durable audit comments only.
pub fn awaiting_review_from_audits(audits: &[AuditSentinelKind]) -> bool {
    let Some(current_delegation_id) = audits.iter().rev().find_map(|audit| match audit {
        AuditSentinelKind::Dispatch { delegation_id, .. } => Some(delegation_id.as_str()),
        _ => None,
    }) else {
        return false;
    };

    for audit in audits.iter().rev() {
        match audit {
            AuditSentinelKind::Approval { delegation_id }
            | AuditSentinelKind::Rejection { delegation_id, .. }
            | AuditSentinelKind::ReviewFeedback { delegation_id, .. }
            | AuditSentinelKind::RetryRequested { delegation_id, .. }
            | AuditSentinelKind::DispatchOrphanCleared { delegation_id, .. }
            | AuditSentinelKind::EscalationRequested {
                delegation_id: Some(delegation_id),
                ..
            } if delegation_id == current_delegation_id => {
                return false;
            }
            AuditSentinelKind::Completion {
                delegation_id,
                completion_state: CompletionState::AwaitingReview,
                ..
            } if delegation_id == current_delegation_id => return true,
            AuditSentinelKind::Completion { delegation_id, .. }
                if delegation_id == current_delegation_id =>
            {
                return false;
            }
            AuditSentinelKind::Dispatch { delegation_id, .. }
                if delegation_id == current_delegation_id =>
            {
                return false;
            }
            _ => {}
        }
    }

    false
}

/// Tier-1 shadow comparator consumer: derive the latest terminal audit kind
/// from durable audit comments only.
pub fn terminal_status_from_audits(audits: &[AuditSentinelKind]) -> Option<TerminalAuditKind> {
    for audit in audits.iter().rev() {
        match audit {
            AuditSentinelKind::Approval { .. } => return Some(TerminalAuditKind::Approval),
            AuditSentinelKind::Rejection { .. } => return Some(TerminalAuditKind::Rejection),
            AuditSentinelKind::Completion {
                completion_state: CompletionState::Failed,
                ..
            } => return Some(TerminalAuditKind::CompletionFailed),
            AuditSentinelKind::Completion {
                completion_state: CompletionState::Cancelled,
                ..
            } => return Some(TerminalAuditKind::CompletionCancelled),
            AuditSentinelKind::Dispatch { .. }
            | AuditSentinelKind::Completion {
                completion_state: CompletionState::AwaitingReview | CompletionState::Superseded,
                ..
            }
            | AuditSentinelKind::RetryRequested { .. }
            | AuditSentinelKind::ReviewFeedback { .. }
            | AuditSentinelKind::DispatchOrphanCleared { .. } => return None,
            _ => {}
        }
    }
    None
}

/// Tier-1 projector inversion consumer: derive supersession fact from durable
/// audit comments only (`mutation_id` and `by` remain label-only metadata).
pub fn superseded_from_audits(audits: &[AuditSentinelKind]) -> bool {
    for audit in audits.iter().rev() {
        match audit {
            AuditSentinelKind::Approval { .. }
            | AuditSentinelKind::Rejection { .. }
            | AuditSentinelKind::Completion {
                completion_state: CompletionState::Failed | CompletionState::Cancelled,
                ..
            }
            | AuditSentinelKind::Dispatch { .. }
            | AuditSentinelKind::RetryRequested { .. }
            | AuditSentinelKind::ReviewFeedback { .. }
            | AuditSentinelKind::EscalationRequested { .. }
            | AuditSentinelKind::Completion {
                completion_state: CompletionState::AwaitingReview,
                ..
            } => return false,
            AuditSentinelKind::Completion {
                completion_state: CompletionState::Superseded,
                ..
            } => return true,
            _ => {}
        }
    }

    false
}

/// Tier-1 index-maintenance reconciler consumer: derive escalation fact from
/// durable audit comments only.
pub fn escalated_from_audits(audits: &[AuditSentinelKind]) -> bool {
    for audit in audits.iter().rev() {
        match audit {
            AuditSentinelKind::Approval { .. }
            | AuditSentinelKind::Rejection { .. }
            | AuditSentinelKind::Completion { .. }
            | AuditSentinelKind::Dispatch { .. }
            | AuditSentinelKind::RetryRequested { .. }
            | AuditSentinelKind::ReviewFeedback { .. } => return false,
            AuditSentinelKind::EscalationRequested { .. } => return true,
            _ => {}
        }
    }

    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StatusComparison {
    Exact,
    PartialMatch {
        irreducible_fields: Vec<&'static str>,
    },
    Mismatch,
}

fn partial_compare_status(legacy: &PlanTaskStatus, shadow: &PlanTaskStatus) -> StatusComparison {
    match (legacy, shadow) {
        (PlanTaskStatus::Pending, PlanTaskStatus::Pending)
        | (PlanTaskStatus::Ready, PlanTaskStatus::Ready) => StatusComparison::Exact,
        (
            PlanTaskStatus::Dispatched {
                delegation_id: legacy_delegation_id,
            },
            PlanTaskStatus::Dispatched {
                delegation_id: shadow_delegation_id,
            },
        ) if legacy_delegation_id == shadow_delegation_id => StatusComparison::Exact,
        (
            PlanTaskStatus::AwaitingReview {
                summary: legacy_summary,
            },
            PlanTaskStatus::AwaitingReview {
                summary: shadow_summary,
            },
        ) if legacy_summary == shadow_summary => StatusComparison::Exact,
        (
            PlanTaskStatus::Approved {
                summary: legacy_summary,
            },
            PlanTaskStatus::Approved {
                summary: shadow_summary,
            },
        ) if legacy_summary == shadow_summary => StatusComparison::Exact,
        (
            PlanTaskStatus::Rejected {
                feedback: legacy_feedback,
            },
            PlanTaskStatus::Rejected {
                feedback: shadow_feedback,
            },
        ) if legacy_feedback == shadow_feedback => StatusComparison::Exact,
        (
            PlanTaskStatus::Failed {
                error: legacy_error,
            },
            PlanTaskStatus::Failed {
                error: shadow_error,
            },
        ) if legacy_error == shadow_error => StatusComparison::Exact,
        (
            PlanTaskStatus::Cancelled {
                reason: legacy_reason,
            },
            PlanTaskStatus::Cancelled {
                reason: shadow_reason,
            },
        ) if legacy_reason == shadow_reason => StatusComparison::Exact,
        (
            PlanTaskStatus::Superseded {
                mutation_id: legacy_mutation_id,
                by: legacy_by,
            },
            PlanTaskStatus::Superseded {
                mutation_id: shadow_mutation_id,
                by: shadow_by,
            },
        ) => {
            let mut irreducible_fields = Vec::new();

            if shadow_mutation_id == "unknown" && !legacy_mutation_id.is_empty() {
                irreducible_fields.push("mutation_id");
            } else if shadow_mutation_id != legacy_mutation_id {
                return StatusComparison::Mismatch;
            }

            if shadow_by.is_empty() && !legacy_by.is_empty() {
                irreducible_fields.push("by");
            } else if shadow_by != legacy_by {
                return StatusComparison::Mismatch;
            }

            if irreducible_fields.is_empty() {
                StatusComparison::Exact
            } else {
                StatusComparison::PartialMatch { irreducible_fields }
            }
        }
        (
            PlanTaskStatus::BlockedOnSetupConflict {
                dep_task_id: legacy_dep_task_id,
                files: legacy_files,
            },
            PlanTaskStatus::BlockedOnSetupConflict {
                dep_task_id: shadow_dep_task_id,
                files: shadow_files,
            },
        ) if legacy_dep_task_id == shadow_dep_task_id && legacy_files == shadow_files => {
            StatusComparison::Exact
        }
        (
            PlanTaskStatus::EscalatedToBrain {
                last_error: legacy_last_error,
            },
            PlanTaskStatus::EscalatedToBrain {
                last_error: shadow_last_error,
            },
        ) if legacy_last_error == shadow_last_error => StatusComparison::Exact,
        _ => StatusComparison::Mismatch,
    }
}

pub fn latest_task_spec(audits: &[AuditSentinelKind]) -> Option<(String, Vec<String>)> {
    for audit in audits.iter().rev() {
        if let AuditSentinelKind::TaskSpec {
            task_id,
            context_files,
            ..
        } = audit
        {
            return Some((task_id.clone(), context_files.clone()));
        }
    }

    None
}

/// Latest extended `TaskSpec` fields (bd-2m2u Phase 2c). Returns the most
/// recent `(task_text, agent, depends_on)` triple; each component is `None`
/// if it was never set in any TaskSpec audit. Used by the projector to
/// override live beads-issue fields after `ModifyTaskSpec` has been applied.
pub fn latest_extended_task_spec(
    audits: &[AuditSentinelKind],
) -> (Option<String>, Option<String>, Option<Vec<String>>) {
    let mut text = None;
    let mut agent = None;
    let mut deps = None;
    for audit in audits.iter().rev() {
        if let AuditSentinelKind::TaskSpec {
            task_text,
            agent: a,
            depends_on,
            ..
        } = audit
        {
            if text.is_none() && task_text.is_some() {
                text = task_text.clone();
            }
            if agent.is_none() && a.is_some() {
                agent = a.clone();
            }
            if deps.is_none() && depends_on.is_some() {
                deps = depends_on.clone();
            }
            if text.is_some() && agent.is_some() && deps.is_some() {
                break;
            }
        }
    }
    (text, agent, deps)
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

fn has_integration_conflict_label(labels: &[String]) -> bool {
    labels
        .iter()
        .any(|label| label == crate::plan::labels::SIGNAL_LABEL_INTEGRATION_CONFLICT)
}

fn latest_integration_conflict(audits: &[AuditSentinelKind]) -> Option<(String, Vec<String>)> {
    #[derive(serde::Deserialize)]
    struct IntegrationConflictReason {
        dep_task_id: String,
        #[serde(default)]
        files: Vec<String>,
    }

    audits.iter().rev().find_map(|audit| {
        let AuditSentinelKind::Signal { kind, reason, .. } = audit else {
            return None;
        };
        if kind != "integration-conflict" && kind != "integration_conflict" {
            return None;
        }
        let reason = serde_json::from_str::<IntegrationConflictReason>(reason).ok()?;
        Some((reason.dep_task_id, reason.files))
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
    let status = if issue.status == closed_status {
        project_closed_status(issue, audits)
    } else if has_integration_conflict_label(&issue.labels) {
        let (dep_task_id, files) =
            latest_integration_conflict(audits).unwrap_or_else(|| ("unknown".to_string(), vec![]));
        PlanTaskStatus::BlockedOnSetupConflict { dep_task_id, files }
    } else if let Some(delegation_id) = issue
        .labels
        .iter()
        .find_map(|label| parse_delegation_id_compat(label))
    {
        PlanTaskStatus::Dispatched {
            delegation_id: delegation_id.to_string(),
        }
    } else if issue
        .labels
        .iter()
        .any(|label| label.as_str() == crate::plan::mutation_executor::SIGNAL_ESCALATED_LABEL)
    {
        let last_error = audits
            .iter()
            .rev()
            .find_map(|audit| match audit {
                AuditSentinelKind::EscalationRequested { last_error, .. } => {
                    Some(last_error.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| "escalated to brain".to_string());
        PlanTaskStatus::EscalatedToBrain { last_error }
    } else if has_ready_for_review_label_compat(&issue.labels) {
        let summary =
            latest_completion_facts(audits).and_then(|(_, _, result_summary, _, _)| result_summary);
        PlanTaskStatus::AwaitingReview { summary }
    } else if ready_now {
        PlanTaskStatus::Ready
    } else {
        PlanTaskStatus::Pending
    };

    // bd-2m2u Phase 2d — `signal:escalated` marks an open issue whose
    // auto-retry budget (1 attempt) is exhausted and is awaiting a brain
    // `submit_plan_mutation` decision. The label is cleared by
    // `submit_plan_mutation` on success, so a present label is authoritative
    // for the projection.
    if issue.status == closed_status {
        let latest_terminal_audit = audits.iter().rev().find_map(|audit| match audit {
            AuditSentinelKind::Approval { .. } => Some("approval"),
            AuditSentinelKind::Rejection { .. } => Some("rejection"),
            AuditSentinelKind::Completion {
                completion_state: CompletionState::Failed { .. },
                ..
            } => Some("completion_failed"),
            AuditSentinelKind::Completion {
                completion_state: CompletionState::Cancelled { .. },
                ..
            } => Some("completion_cancelled"),
            _ => None,
        });
        let consistent = match latest_terminal_audit {
            Some("approval") => matches!(status, PlanTaskStatus::Approved { .. }),
            Some("rejection") => matches!(status, PlanTaskStatus::Rejected { .. }),
            Some("completion_failed") => matches!(status, PlanTaskStatus::Failed { .. }),
            Some("completion_cancelled") => matches!(status, PlanTaskStatus::Cancelled { .. }),
            _ => true,
        };
        assert!(
            consistent,
            "invariant:project_status_for_issue terminal audit/status mismatch violated (issue_id={}, closed_status={}) expected projected status to match latest terminal audit, got latest_terminal_audit={:?}, status={:?}",
            issue.id,
            closed_status,
            latest_terminal_audit,
            status,
        );
    }
    let has_delegation = issue
        .labels
        .iter()
        .any(|label| parse_delegation_id_compat(label).is_some());
    let has_ready = has_ready_for_review_label_compat(&issue.labels);
    // Completion audit can land before the atomic label update.
    // A polling client may race request_changes in that gap.
    // Reconciler retry-dispatch can then produce transient dual-label overlap.
    // Enforce single-label invariants only when the other label is absent.
    assert!(
        !(has_delegation && !has_ready) || matches!(status, PlanTaskStatus::Dispatched { .. }),
        "invariant:project_status_for_issue delegation label/status mismatch violated (issue_id={}, status={:?}) expected Dispatched when delegation-id label present without ready-for-review overlap, labels={:?}",
        issue.id,
        status,
        issue.labels,
    );
    assert!(
        !(has_ready && !has_delegation)
            || matches!(status, PlanTaskStatus::AwaitingReview { .. } | PlanTaskStatus::Approved { .. }),
        "invariant:project_status_for_issue ready-for-review label/status mismatch violated (issue_id={}, status={:?}) expected AwaitingReview/Approved when ready-for-review label present without delegation-id overlap, labels={:?}",
        issue.id,
        status,
        issue.labels,
    );
    status
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
            AuditSentinelKind::RetryRequested { .. } => {
                return PlanTaskStatus::Pending;
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

fn latest_terminal_audit_kind(audits: &[AuditSentinelKind]) -> Option<&'static str> {
    terminal_status_from_audits(audits).map(|kind| match kind {
        TerminalAuditKind::Approval => "approval",
        TerminalAuditKind::Rejection => "rejection",
        TerminalAuditKind::CompletionFailed => "completion_failed",
        TerminalAuditKind::CompletionCancelled => "completion_cancelled",
    })
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

fn emit_shadow_projector_mismatch_warnings(
    plan_id: &str,
    legacy: &PlanState,
    shadow: &PlanState,
    audit_comment_count: usize,
) {
    let legacy_by_task: HashMap<&str, &PlanTaskEntry> = legacy
        .tasks
        .iter()
        .map(|entry| (entry.spec.task_id.as_str(), entry))
        .collect();
    let shadow_by_task: HashMap<&str, &PlanTaskEntry> = shadow
        .tasks
        .iter()
        .map(|entry| (entry.spec.task_id.as_str(), entry))
        .collect();

    let mut task_ids: Vec<&str> = legacy_by_task
        .keys()
        .copied()
        .chain(shadow_by_task.keys().copied())
        .collect();
    task_ids.sort_unstable();
    task_ids.dedup();

    let mut differing_task_ids: Vec<String> = Vec::new();
    let mut differing_fields: Vec<String> = Vec::new();
    let mut mismatches: Vec<String> = Vec::new();

    for task_id in task_ids {
        let legacy_entry = legacy_by_task.get(task_id).copied();
        let shadow_entry = shadow_by_task.get(task_id).copied();
        let Some((legacy_entry, shadow_entry)) = legacy_entry.zip(shadow_entry) else {
            differing_task_ids.push(task_id.to_string());
            differing_fields.push("task_presence".to_string());
            mismatches.push(format!(
                "{task_id}.task_presence legacy={:?} shadow={:?}",
                legacy_entry.map(|_| "present"),
                shadow_entry.map(|_| "present"),
            ));
            continue;
        };

        match partial_compare_status(&legacy_entry.status, &shadow_entry.status) {
            StatusComparison::Exact => {}
            StatusComparison::PartialMatch { .. } => {}
            StatusComparison::Mismatch => {
                differing_task_ids.push(task_id.to_string());
                differing_fields.push("status".to_string());
                mismatches.push(format!(
                    "{task_id}.status legacy={:?} shadow={:?}",
                    legacy_entry.status, shadow_entry.status
                ));
            }
        }

        let checks = [
            (
                "worker_branch",
                format!("{:?}", legacy_entry.worker_branch),
                format!("{:?}", shadow_entry.worker_branch),
            ),
            (
                "dispatched_base_oid",
                format!("{:?}", legacy_entry.dispatched_base_oid),
                format!("{:?}", shadow_entry.dispatched_base_oid),
            ),
            (
                "last_delegation_id",
                format!("{:?}", legacy_entry.last_delegation_id),
                format!("{:?}", shadow_entry.last_delegation_id),
            ),
            (
                "attempt",
                legacy_entry.attempt.to_string(),
                shadow_entry.attempt.to_string(),
            ),
        ];

        for (field, legacy_value, shadow_value) in checks {
            if legacy_value != shadow_value {
                differing_task_ids.push(task_id.to_string());
                differing_fields.push(field.to_string());
                mismatches.push(format!(
                    "{task_id}.{field} legacy={legacy_value} shadow={shadow_value}"
                ));
            }
        }
    }

    if !mismatches.is_empty() {
        differing_task_ids.sort();
        differing_task_ids.dedup();
        differing_fields.sort();
        differing_fields.dedup();
        tracing::warn!(
            target: "spur.plan.shadow_projector",
            plan_id = %plan_id,
            task_ids = %differing_task_ids.join(","),
            fields = %differing_fields.join(","),
            mismatches = ?mismatches,
            audit_comment_count,
            "shadow-projector mismatch: legacy projection differs from audit-only fold",
        );
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
    pm: &dyn crate::plan::PmLike,
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
    let epic_audits =
        collect_sorted_audits_for_issue(&epic.id, adv.list_comments(&epic.id).await?)?;
    let closed_status = pm.closed_status().to_string();
    struct ProjectedTask {
        issue: spur_pm::Issue,
        audits: Vec<crate::plan::audit_sentinel::AuditSentinelKind>,
        task_id: String,
        context_files: Vec<String>,
    }

    let mut projected_tasks = Vec::with_capacity(tasks.len());
    for task_issue in tasks {
        let audits = collect_sorted_audits_for_issue(
            &task_issue.id,
            adv.list_comments(&task_issue.id).await?,
        )?;
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

    for projected_task in &projected_tasks {
        let (_attempt_count, last_delegation_id) = project_attempt_facts(&projected_task.audits);
        let history = project_attempt_history(&projected_task.audits);
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
        let latest_completion = latest_completion_facts(&projected_task.audits);
        let issue_is_closed = projected_task.issue.status == closed_status;
        let terminal_audit = latest_terminal_audit_kind(&projected_task.audits);
        let attempt = project_entry_attempt(&projected_task.audits, &status);
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
                task_id: projected_task.task_id.clone(),
                agent,
                task: projected_task.issue.body.clone(),
                depends_on,
                issue_id: Some(projected_task.issue.id.clone()),
                issue_title: Some(projected_task.issue.title.clone()),
                context_files: projected_task.context_files.clone(),
            },
            status,
            result: None,
            worker_branch,
            attempt,
            history,
            last_delegation_id,
            dispatched_base_oid,
        });
        let entry = entries.last().expect("entry was just pushed");
        let history_monotonic = entry
            .history
            .windows(2)
            .all(|pair| pair[0].attempt <= pair[1].attempt);
        assert!(
            history_monotonic && entry.history.iter().all(|record| record.attempt <= entry.attempt),
            "invariant:project_plan_from_beads attempt monotonicity violated (plan_id={}, task_id={}) expected history attempts to be non-decreasing and <= current attempt, got current_attempt={:?}, history_attempts={:?}",
            plan_id,
            entry.spec.task_id,
            entry.attempt,
            entry.history.iter().map(|record| record.attempt).collect::<Vec<_>>(),
        );
        assert!(
            !matches!(entry.status, PlanTaskStatus::Approved { .. } | PlanTaskStatus::AwaitingReview { .. })
                || latest_completion
                    .as_ref()
                    .and_then(|(_, worker_branch, _, _, _)| worker_branch.as_ref())
                    .is_none()
                || entry.worker_branch.is_some(),
            "invariant:project_plan_from_beads worker_branch missing after populated completion violated (plan_id={}, task_id={}) expected worker_branch=Some(_) for Approved/AwaitingReview when latest Completion has worker_branch, got status={:?}, latest_completion={:?}, entry.worker_branch={:?}",
            plan_id,
            entry.spec.task_id,
            entry.status,
            latest_completion,
            entry.worker_branch,
        );
        assert!(
            !matches!(entry.status, PlanTaskStatus::AwaitingReview { .. } | PlanTaskStatus::Approved { .. })
                || entry.dispatched_base_oid.is_some(),
            "invariant:project_plan_from_beads dispatched_base_oid missing for completion-derived status violated (plan_id={}, task_id={}) expected dispatched_base_oid=Some(_) for AwaitingReview/Approved, got status={:?}, dispatched_base_oid={:?}",
            plan_id,
            entry.spec.task_id,
            entry.status,
            entry.dispatched_base_oid,
        );
        if issue_is_closed {
            if let Some(terminal_audit) = terminal_audit {
                let consistent = match terminal_audit {
                    "approval" => matches!(entry.status, PlanTaskStatus::Approved { .. }),
                    "rejection" => matches!(entry.status, PlanTaskStatus::Rejected { .. }),
                    "completion_failed" => matches!(entry.status, PlanTaskStatus::Failed { .. }),
                    "completion_cancelled" => {
                        matches!(entry.status, PlanTaskStatus::Cancelled { .. })
                    }
                    _ => true,
                };
                assert!(
                    consistent,
                    "invariant:project_plan_from_beads terminal audit/status mismatch violated (plan_id={}, task_id={}) expected projected status to match latest terminal audit, got latest_terminal_audit={:?}, status={:?}",
                    plan_id,
                    entry.spec.task_id,
                    terminal_audit,
                    entry.status,
                );
            }
        }
    }

    recompute_open_statuses(&mut entries);

    let brain_session_id = plan_submit_brain_session_id(&epic_audits)
        .unwrap_or_else(|| BrainSessionId::new(SessionId(format!("persisted-plan:{plan_id}"))));

    let legacy_state = PlanState {
        plan_id: plan_id.to_string(),
        tasks: entries,
        brain_session_id,
        base_snapshot_branch: plan_submit_base_snapshot(&epic_audits),
        base_snapshot_oid: plan_submit_base_snapshot_oid(&epic_audits),
        merge_state: crate::plan::PlanMergeState::NotStarted,
        epic_id: Some(epic.id),
    };

    if shadow_projector_enabled() {
        let shadow_logs: Vec<TaskAuditLog> = legacy_state
            .tasks
            .iter()
            .map(|entry| {
                let audits = projected_tasks
                    .iter()
                    .find(|task| task.task_id == entry.spec.task_id)
                    .map(|task| task.audits.clone())
                    .unwrap_or_default();
                TaskAuditLog {
                    spec: entry.spec.clone(),
                    audits,
                }
            })
            .collect();
        let shadow_state = shadow_project_plan_from_beads(plan_id, &shadow_logs);
        let audit_comment_count: usize = shadow_logs.iter().map(|task| task.audits.len()).sum();
        emit_shadow_projector_mismatch_warnings(
            plan_id,
            &legacy_state,
            &shadow_state,
            audit_comment_count,
        );
    }

    Ok(legacy_state)
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use proptest::prelude::*;
    use spur_pm::Comment;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};

    use super::{
        AuditSentinelKind, BrainSessionId, CompletionState, PlanState, PlanTask, PlanTaskEntry,
        PlanTaskStatus, SessionId, TerminalAuditKind,
    };
    use crate::plan::audit_sentinel::EpicCompletionOutcome;
    use crate::plan::PlanMergeState;

    #[derive(Debug, Clone)]
    struct CapturedWarning {
        target: String,
        fields: HashMap<String, String>,
    }

    #[derive(Clone)]
    struct WarningCapture(Arc<Mutex<Vec<CapturedWarning>>>);

    impl tracing::Subscriber for WarningCapture {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::WARN
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() != tracing::Level::WARN {
                return;
            }

            let mut visitor = FieldVisitor {
                fields: HashMap::new(),
            };
            event.record(&mut visitor);
            self.0.lock().unwrap().push(CapturedWarning {
                target: event.metadata().target().to_string(),
                fields: visitor.fields,
            });
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    struct FieldVisitor {
        fields: HashMap<String, String>,
    }

    impl Visit for FieldVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.fields
                .insert(field.name().to_string(), format!("{value:?}"));
        }
    }

    fn capture_warnings<T>(f: impl FnOnce() -> T) -> (T, Vec<CapturedWarning>) {
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let subscriber = WarningCapture(Arc::clone(&warnings));

        let result = tracing::subscriber::with_default(subscriber, f);
        let captured = warnings.lock().unwrap().clone();
        (result, captured)
    }

    fn comment(id: &str, body: String, offset_secs: i64) -> Comment {
        Comment {
            id: id.into(),
            body,
            actor: "spur".into(),
            created_at: Utc.with_ymd_and_hms(2026, 4, 21, 10, 0, 0).unwrap()
                + chrono::Duration::seconds(offset_secs),
        }
    }

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

        let audits = super::collect_sorted_audits(comments).expect("projection should parse");
        assert_eq!(audits.len(), 1);
        assert!(matches!(
            audits[0],
            crate::plan::audit_sentinel::AuditSentinelKind::Approval { .. }
        ));
    }

    #[test]
    fn projector_warns_and_drops_corrupt_audit_sentinel() {
        let comments = vec![
            comment(
                "c-valid",
                crate::plan::audit_sentinel::encode_comment(
                    &crate::plan::audit_sentinel::AuditSentinelKind::Approval {
                        delegation_id: "del-A".into(),
                    },
                ),
                0,
            ),
            comment(
                "c-corrupt",
                format!("{}\nnot json", crate::plan::audit_sentinel::SENTINEL_PREFIX),
                1,
            ),
        ];

        let (audits, warnings) =
            capture_warnings(|| super::collect_sorted_audits_for_issue("bd-task-1", comments));
        let audits = audits.expect("approval-only parse should succeed");

        assert_eq!(audits.len(), 1);
        assert!(matches!(
            audits[0],
            crate::plan::audit_sentinel::AuditSentinelKind::Approval { .. }
        ));
        assert_eq!(warnings.len(), 1);

        let warning = &warnings[0];
        assert_eq!(warning.target, "spur.audit.parse_failure");
        assert_eq!(
            warning.fields.get("issue_id").map(String::as_str),
            Some("bd-task-1")
        );
        assert_eq!(
            warning.fields.get("comment_id").map(String::as_str),
            Some("c-corrupt")
        );
        assert!(
            warning
                .fields
                .get("error")
                .is_some_and(|error| error.contains("sentinel JSON parse error")),
            "warning fields should contain parse error, got {warning:?}"
        );
    }

    #[test]
    fn projector_handles_mix_of_valid_and_corrupt_sentinels() {
        let comments = vec![
            comment(
                "c-4",
                crate::plan::audit_sentinel::encode_comment(
                    &crate::plan::audit_sentinel::AuditSentinelKind::Approval {
                        delegation_id: "del-A".into(),
                    },
                ),
                3,
            ),
            comment(
                "c-2",
                format!("{}\n{{", crate::plan::audit_sentinel::SENTINEL_PREFIX),
                1,
            ),
            comment(
                "c-3",
                crate::plan::audit_sentinel::encode_comment(
                    &crate::plan::audit_sentinel::AuditSentinelKind::Completion {
                        delegation_id: "del-A".into(),
                        completion_state: CompletionState::AwaitingReview,
                        superseded: false,
                        worker_branch: Some("spur/worker-a".into()),
                        result_summary: Some("done".into()),
                        artifact_uri: None,
                        dispatched_base_oid: Some("base-oid".into()),
                    },
                ),
                2,
            ),
            comment(
                "c-1",
                crate::plan::audit_sentinel::encode_comment(
                    &crate::plan::audit_sentinel::AuditSentinelKind::Dispatch {
                        delegation_id: "del-A".into(),
                        worker: "codex".into(),
                        attempt: 1,
                    },
                ),
                0,
            ),
        ];

        let (audits, warnings) =
            capture_warnings(|| super::collect_sorted_audits_for_issue("bd-task-2", comments));
        let audits = audits.expect("mixed valid+informational parse should succeed");

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].target, "spur.audit.parse_failure");
        assert_eq!(
            warnings[0].fields.get("issue_id").map(String::as_str),
            Some("bd-task-2")
        );
        assert_eq!(
            warnings[0].fields.get("comment_id").map(String::as_str),
            Some("c-2")
        );

        let kinds: Vec<&str> = audits.iter().map(AuditSentinelKind::kind_str).collect();
        assert_eq!(kinds, vec!["dispatch", "completion", "approval"]);
    }

    #[test]
    fn projector_errors_on_corrupt_completion_sentinel() {
        let comments = vec![
            comment(
                "c-1",
                crate::plan::audit_sentinel::encode_comment(
                    &crate::plan::audit_sentinel::AuditSentinelKind::Dispatch {
                        delegation_id: "del-A".into(),
                        worker: "codex".into(),
                        attempt: 1,
                    },
                ),
                0,
            ),
            comment(
                "c-2",
                format!(
                    "{}\n{{\"kind\":\"completion\",\"delegation_id\":\"del-A\"}}",
                    crate::plan::audit_sentinel::SENTINEL_PREFIX
                ),
                1,
            ),
        ];

        let (result, warnings) = capture_warnings(|| {
            super::collect_sorted_audits_for_issue("bd-task-critical", comments)
        });
        let error = result.expect_err("malformed completion sentinel should fail projection");
        assert!(
            error
                .to_string()
                .contains("critical audit sentinel parse failed"),
            "unexpected error: {error:#}"
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn projector_errors_on_corrupt_review_feedback_sentinel() {
        let comments = vec![comment(
            "c-1",
            format!(
                "{}\n{{\"kind\":\"review-feedback\",\"delegation_id\":\"del-A\"}}",
                crate::plan::audit_sentinel::SENTINEL_PREFIX
            ),
            0,
        )];

        let (result, warnings) = capture_warnings(|| {
            super::collect_sorted_audits_for_issue("bd-task-critical", comments)
        });
        let error = result.expect_err("malformed review-feedback sentinel should fail projection");
        assert!(
            error
                .to_string()
                .contains("critical audit sentinel parse failed"),
            "unexpected error: {error:#}"
        );
        assert!(warnings.is_empty());
    }

    /// Two Dispatch sentinels project as `attempt = 2` (count of dispatches).
    /// Under count-based projection, this no longer depends on the value of
    /// the `attempt` field on each Dispatch sentinel — it counts occurrences.
    #[test]
    fn count_based_projection_returns_dispatch_count() {
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

    #[test]
    fn worker_started_does_not_advance_attempt_projection() {
        let audits = vec![
            AuditSentinelKind::Dispatch {
                delegation_id: "del-1".into(),
                worker: "codex".into(),
                attempt: 1,
            },
            AuditSentinelKind::WorkerStarted {
                delegation_id: "del-1".into(),
                worker_branch: "spur/worker/v2/codex/brain/worker".into(),
                worker_session_id: "worker".into(),
                dispatched_base_oid: "base-oid".into(),
            },
        ];

        let (attempt, last_delegation_id) = super::project_attempt_facts(&audits);
        assert_eq!(attempt, 1);
        assert_eq!(last_delegation_id.as_deref(), Some("del-1"));
    }

    #[test]
    fn project_attempt_facts_returns_one_for_no_dispatch() {
        let audits: Vec<AuditSentinelKind> = vec![];
        let (attempt, last_delegation_id) = super::project_attempt_facts(&audits);
        assert_eq!(attempt, 1);
        assert!(last_delegation_id.is_none());
    }

    #[test]
    fn project_attempt_facts_returns_count_not_field() {
        let audits = vec![
            AuditSentinelKind::Dispatch {
                delegation_id: "del-1".into(),
                worker: "codex".into(),
                attempt: 1,
            },
            AuditSentinelKind::Dispatch {
                delegation_id: "del-2".into(),
                worker: "codex".into(),
                attempt: 1,
            },
            AuditSentinelKind::Dispatch {
                delegation_id: "del-3".into(),
                worker: "codex".into(),
                attempt: 1,
            },
        ];

        let (attempt, last_delegation_id) = super::project_attempt_facts(&audits);
        assert_eq!(attempt, 3, "count-based: 3 dispatches => attempt 3");
        assert_eq!(last_delegation_id.as_deref(), Some("del-3"));
    }

    #[test]
    fn project_attempt_facts_legacy_correct_field_ignored() {
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
            AuditSentinelKind::Dispatch {
                delegation_id: "del-3".into(),
                worker: "codex".into(),
                attempt: 3,
            },
        ];

        let (attempt, last_delegation_id) = super::project_attempt_facts(&audits);
        assert_eq!(attempt, 3);
        assert_eq!(last_delegation_id.as_deref(), Some("del-3"));
    }

    #[test]
    fn project_attempt_facts_last_delegation_id_tracks_most_recent() {
        let audits = vec![
            AuditSentinelKind::Dispatch {
                delegation_id: "del-first".into(),
                worker: "codex".into(),
                attempt: 1,
            },
            AuditSentinelKind::ReviewFeedback {
                delegation_id: "del-first".into(),
                attempt: 1,
                feedback: "fix".into(),
                worker_branch: None,
                summary: None,
                reuse_prior_worktree: None,
            },
            AuditSentinelKind::Dispatch {
                delegation_id: "del-middle".into(),
                worker: "codex".into(),
                attempt: 1,
            },
            AuditSentinelKind::Dispatch {
                delegation_id: "del-latest".into(),
                worker: "codex".into(),
                attempt: 1,
            },
        ];

        let (attempt, last_delegation_id) = super::project_attempt_facts(&audits);
        assert_eq!(attempt, 3);
        assert_eq!(
            last_delegation_id.as_deref(),
            Some("del-latest"),
            "must track LAST Dispatch's delegation_id, not first or middle"
        );
    }

    #[test]
    fn project_attempt_history_reconstructs_from_review_feedback_sentinels() {
        let audits = vec![
            AuditSentinelKind::Dispatch {
                delegation_id: "del-1".into(),
                worker: "codex".into(),
                attempt: 1,
            },
            AuditSentinelKind::ReviewFeedback {
                delegation_id: "del-1".into(),
                attempt: 1,
                feedback: "fix edge case".into(),
                worker_branch: Some("spur/worker-1".into()),
                summary: Some("partial".into()),
                reuse_prior_worktree: None,
            },
            AuditSentinelKind::ReviewFeedback {
                delegation_id: "del-2".into(),
                attempt: 2,
                feedback: "also add tests".into(),
                worker_branch: None,
                summary: None,
                reuse_prior_worktree: None,
            },
        ];

        let history = super::project_attempt_history(&audits);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].attempt, 1);
        assert_eq!(history[0].feedback, "fix edge case");
        assert_eq!(history[0].worker_branch.as_deref(), Some("spur/worker-1"));
        assert_eq!(history[0].summary.as_deref(), Some("partial"));
        assert_eq!(history[1].attempt, 2);
        assert_eq!(history[1].feedback, "also add tests");
        assert!(history[1].worker_branch.is_none());
        assert!(history[1].summary.is_none());
    }

    #[test]
    fn project_attempt_history_preserves_reuse_prior_worktree_true() {
        let audits = vec![AuditSentinelKind::ReviewFeedback {
            delegation_id: "del-1".into(),
            attempt: 1,
            feedback: "fix edge case".into(),
            worker_branch: Some("spur/worker-1".into()),
            summary: Some("partial".into()),
            reuse_prior_worktree: Some(true),
        }];

        let history = super::project_attempt_history(&audits);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].reuse_prior_worktree, Some(true));
    }

    #[test]
    fn project_attempt_history_preserves_reuse_prior_worktree_none() {
        let audits = vec![AuditSentinelKind::ReviewFeedback {
            delegation_id: "del-1".into(),
            attempt: 1,
            feedback: "fix edge case".into(),
            worker_branch: Some("spur/worker-1".into()),
            summary: Some("partial".into()),
            reuse_prior_worktree: None,
        }];

        let history = super::project_attempt_history(&audits);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].reuse_prior_worktree, None);
    }

    #[test]
    fn project_attempt_history_empty_when_no_review_feedback() {
        let audits = vec![AuditSentinelKind::Dispatch {
            delegation_id: "del-1".into(),
            worker: "codex".into(),
            attempt: 1,
        }];
        let history = super::project_attempt_history(&audits);
        assert!(history.is_empty());
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
            external_ref: None,
            source_system: None,
            source_repo: None,
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
    fn projector_recovers_pending_after_retry_requested_sentinel() {
        let issue = issue("bd-9", "closed", Vec::new(), Vec::new());
        let audits = vec![
            AuditSentinelKind::Completion {
                delegation_id: "del-A".into(),
                completion_state: CompletionState::Failed,
                superseded: false,
                worker_branch: Some("spur/worker-a".into()),
                result_summary: Some("worker crashed".into()),
                artifact_uri: None,
                dispatched_base_oid: None,
            },
            AuditSentinelKind::RetryRequested {
                delegation_id: "del-A".into(),
                attempt: 1,
                error: "worker crashed".into(),
                worker_branch: Some("spur/worker-a".into()),
                amended_prompt_summary: None,
            },
        ];

        let status = super::project_closed_status(&issue, &audits);
        assert!(matches!(status, PlanTaskStatus::Pending));
    }

    #[test]
    fn projector_completion_failed_after_retry_requested_falls_back_to_failed() {
        let issue = issue("bd-9", "closed", Vec::new(), Vec::new());
        let audits = vec![
            AuditSentinelKind::RetryRequested {
                delegation_id: "del-A".into(),
                attempt: 1,
                error: "worker crashed".into(),
                worker_branch: Some("spur/worker-a".into()),
                amended_prompt_summary: None,
            },
            AuditSentinelKind::Completion {
                delegation_id: "del-B".into(),
                completion_state: CompletionState::Failed,
                superseded: false,
                worker_branch: Some("spur/worker-b".into()),
                result_summary: Some("worker crashed again".into()),
                artifact_uri: None,
                dispatched_base_oid: None,
            },
        ];

        let status = super::project_closed_status(&issue, &audits);
        assert!(
            matches!(status, PlanTaskStatus::Failed { error } if error == "worker crashed again")
        );
    }

    #[test]
    fn project_status_for_open_issue_with_signal_escalated_returns_escalated_to_brain() {
        // bd-2m2u Phase 2d — open issue carrying `signal:escalated` MUST
        // project as `EscalatedToBrain` (not Ready/Pending/AwaitingReview)
        // so the engine pauses traversal and the brain continuation drives
        // recovery via `submit_plan_mutation`.
        let issue = spur_pm::Issue {
            id: "bd-1".into(),
            source: spur_pm::PmSource::Beads,
            title: "Task".into(),
            body: "Body".into(),
            status: "open".into(),
            labels: vec![crate::plan::mutation_executor::SIGNAL_ESCALATED_LABEL.to_string()],
            assignee: None,
            url: String::new(),
            priority: None,
            issue_type: Some("task".into()),
            external_ref: None,
            source_system: None,
            source_repo: None,
            blocked_by: vec![],
            due_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let audits = vec![AuditSentinelKind::EscalationRequested {
            plan_id: "p1".into(),
            task_id: "bd-1".into(),
            attempt: 2,
            last_error: "exhausted recovery budget".into(),
            worker_branch: Some("spur/worker-x".into()),
            delegation_id: Some("del-Z".into()),
        }];

        let status = super::project_status_for_issue(&issue, &audits, true, "closed");

        match status {
            PlanTaskStatus::EscalatedToBrain { last_error } => {
                assert_eq!(last_error, "exhausted recovery budget");
            }
            other => panic!("expected EscalatedToBrain, got {other:?}"),
        }
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
                    issue_title: None,
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
                    issue_title: None,
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
    fn partial_compare_status_exact_match() {
        let legacy = PlanTaskStatus::Approved {
            summary: Some("ok".into()),
        };
        let shadow = PlanTaskStatus::Approved {
            summary: Some("ok".into()),
        };
        assert!(matches!(
            super::partial_compare_status(&legacy, &shadow),
            super::StatusComparison::Exact
        ));
    }

    #[test]
    fn partial_compare_status_superseded_label_only_partial_match() {
        let legacy = PlanTaskStatus::Superseded {
            mutation_id: "m-1".into(),
            by: vec!["t1a".into()],
        };
        let shadow = PlanTaskStatus::Superseded {
            mutation_id: "unknown".into(),
            by: Vec::new(),
        };
        assert!(matches!(
            super::partial_compare_status(&legacy, &shadow),
            super::StatusComparison::PartialMatch { .. }
        ));
    }

    #[test]
    fn partial_compare_status_mismatch() {
        let legacy = PlanTaskStatus::Approved { summary: None };
        let shadow = PlanTaskStatus::Rejected { feedback: None };
        assert!(matches!(
            super::partial_compare_status(&legacy, &shadow),
            super::StatusComparison::Mismatch
        ));
    }

    #[test]
    fn shadow_projector_suppresses_superseded_status_partial_match_only() {
        let legacy = PlanState {
            plan_id: "plan-1".into(),
            tasks: vec![PlanTaskEntry {
                spec: PlanTask {
                    task_id: "t1".into(),
                    agent: "codex".into(),
                    task: "task".into(),
                    depends_on: Vec::new(),
                    issue_id: Some("bd-1".into()),
                    issue_title: Some("Task".into()),
                    context_files: Vec::new(),
                },
                status: PlanTaskStatus::Superseded {
                    mutation_id: "m-1".into(),
                    by: vec!["t1a".into()],
                },
                result: None,
                worker_branch: Some("spur/worker-a".into()),
                attempt: 2,
                history: Vec::new(),
                last_delegation_id: Some("del-1".into()),
                dispatched_base_oid: Some("base-1".into()),
            }],
            brain_session_id: BrainSessionId::new(SessionId("brain-1".into())),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: PlanMergeState::NotStarted,
            epic_id: Some("bd-epic".into()),
        };

        let shadow = PlanState {
            plan_id: legacy.plan_id.clone(),
            tasks: vec![PlanTaskEntry {
                spec: legacy.tasks[0].spec.clone(),
                status: PlanTaskStatus::Superseded {
                    mutation_id: "unknown".into(),
                    by: Vec::new(),
                },
                result: None,
                worker_branch: Some("spur/worker-b".into()),
                attempt: legacy.tasks[0].attempt,
                history: legacy.tasks[0].history.clone(),
                last_delegation_id: legacy.tasks[0].last_delegation_id.clone(),
                dispatched_base_oid: legacy.tasks[0].dispatched_base_oid.clone(),
            }],
            brain_session_id: legacy.brain_session_id.clone(),
            base_snapshot_branch: legacy.base_snapshot_branch.clone(),
            base_snapshot_oid: legacy.base_snapshot_oid.clone(),
            merge_state: PlanMergeState::NotStarted,
            epic_id: legacy.epic_id.clone(),
        };

        let (_, warnings) = capture_warnings(|| {
            super::emit_shadow_projector_mismatch_warnings("plan-1", &legacy, &shadow, 1)
        });

        assert_eq!(
            warnings.len(),
            1,
            "worker_branch mismatch should still warn"
        );
        let warning = &warnings[0];
        assert_eq!(warning.target, "spur.plan.shadow_projector");
        assert_eq!(
            warning.fields.get("fields").map(String::as_str),
            Some("worker_branch")
        );
        assert!(
            warning
                .fields
                .get("mismatches")
                .is_some_and(|mismatches| mismatches.contains("t1.worker_branch")),
            "expected worker_branch mismatch detail, got {warning:?}"
        );
        assert!(
            warning
                .fields
                .get("mismatches")
                .is_some_and(|mismatches| !mismatches.contains("t1.status")),
            "status mismatch should be suppressed for legacy Superseded tasks, got {warning:?}"
        );
    }

    #[test]
    fn superseded_from_audits_uses_latest_terminal_state() {
        let audits = vec![
            AuditSentinelKind::Completion {
                delegation_id: "del-A".into(),
                completion_state: CompletionState::Superseded,
                superseded: true,
                worker_branch: None,
                result_summary: None,
                artifact_uri: None,
                dispatched_base_oid: None,
            },
            AuditSentinelKind::Approval {
                delegation_id: "del-A".into(),
            },
        ];
        assert!(!super::superseded_from_audits(&audits));
    }

    #[test]
    fn superseded_from_audits_dispatch_then_awaiting_review_overwrites_superseded() {
        let audits = vec![
            AuditSentinelKind::Completion {
                delegation_id: "del-A".into(),
                completion_state: CompletionState::Superseded,
                superseded: true,
                worker_branch: None,
                result_summary: None,
                artifact_uri: None,
                dispatched_base_oid: None,
            },
            AuditSentinelKind::Dispatch {
                delegation_id: "del-B".into(),
                worker: "codex".into(),
                attempt: 2,
            },
            AuditSentinelKind::Completion {
                delegation_id: "del-B".into(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("spur/worker-b".into()),
                result_summary: Some("ready".into()),
                artifact_uri: None,
                dispatched_base_oid: None,
            },
        ];
        assert!(!super::superseded_from_audits(&audits));
    }

    #[test]
    fn escalated_from_audits_uses_latest_terminal_state() {
        let audits = vec![
            AuditSentinelKind::EscalationRequested {
                plan_id: "P1".to_string(),
                task_id: "T1".to_string(),
                attempt: 1,
                last_error: "err".to_string(),
                worker_branch: None,
                delegation_id: Some("del-A".into()),
            },
            AuditSentinelKind::Completion {
                delegation_id: "del-A".into(),
                completion_state: CompletionState::Cancelled,
                superseded: false,
                worker_branch: None,
                result_summary: None,
                artifact_uri: None,
                dispatched_base_oid: None,
            },
        ];
        assert!(!super::escalated_from_audits(&audits));
    }

    #[test]
    fn escalated_from_audits_dispatch_after_escalation_clears_flag() {
        let audits = vec![
            AuditSentinelKind::EscalationRequested {
                plan_id: "P1".to_string(),
                task_id: "T1".to_string(),
                attempt: 1,
                last_error: "err".to_string(),
                worker_branch: None,
                delegation_id: Some("del-A".into()),
            },
            AuditSentinelKind::Dispatch {
                delegation_id: "del-A".into(),
                worker: "codex".into(),
                attempt: 2,
            },
        ];
        assert!(!super::escalated_from_audits(&audits));
    }

    #[test]
    fn awaiting_review_from_audits_escalation_for_current_delegation_clears_flag() {
        let audits = vec![
            AuditSentinelKind::Dispatch {
                delegation_id: "del-A".into(),
                worker: "codex".into(),
                attempt: 1,
            },
            AuditSentinelKind::Completion {
                delegation_id: "del-A".into(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("spur/worker-a".into()),
                result_summary: Some("ready".into()),
                artifact_uri: None,
                dispatched_base_oid: None,
            },
            AuditSentinelKind::EscalationRequested {
                plan_id: "P1".to_string(),
                task_id: "T1".to_string(),
                attempt: 1,
                last_error: "needs brain".to_string(),
                worker_branch: Some("spur/worker-a".into()),
                delegation_id: Some("del-A".into()),
            },
        ];
        assert!(!super::awaiting_review_from_audits(&audits));
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
            explicit_base: None,
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
            explicit_base: None,
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

    fn arb_delegation_id() -> impl Strategy<Value = String> {
        proptest::string::string_regex("del-[a-z0-9]{1,8}").expect("valid regex")
    }

    fn arb_worker_branch() -> impl Strategy<Value = String> {
        proptest::string::string_regex("spur/worker-[a-z0-9]{1,8}").expect("valid regex")
    }

    fn arb_audit_kind() -> impl Strategy<Value = AuditSentinelKind> {
        let dispatch = (arb_delegation_id(), 1u32..=5u32).prop_map(|(delegation_id, attempt)| {
            AuditSentinelKind::Dispatch {
                delegation_id,
                worker: "codex".to_string(),
                attempt,
            }
        });
        let completion = (
            arb_delegation_id(),
            prop_oneof![
                Just(CompletionState::AwaitingReview),
                Just(CompletionState::Failed),
                Just(CompletionState::Cancelled),
                Just(CompletionState::Superseded),
            ],
            proptest::option::of(arb_worker_branch()),
        )
            .prop_map(|(delegation_id, completion_state, worker_branch)| {
                AuditSentinelKind::Completion {
                    delegation_id,
                    completion_state,
                    superseded: matches!(completion_state, CompletionState::Superseded),
                    worker_branch,
                    result_summary: None,
                    artifact_uri: None,
                    dispatched_base_oid: None,
                }
            });
        let approval = arb_delegation_id()
            .prop_map(|delegation_id| AuditSentinelKind::Approval { delegation_id });
        let rejection =
            arb_delegation_id().prop_map(|delegation_id| AuditSentinelKind::Rejection {
                delegation_id,
                feedback: "needs changes".to_string(),
            });
        let review_feedback =
            (arb_delegation_id(), 1u32..=5u32).prop_map(|(delegation_id, attempt)| {
                AuditSentinelKind::ReviewFeedback {
                    delegation_id,
                    attempt,
                    feedback: "feedback".to_string(),
                    worker_branch: None,
                    summary: None,
                    reuse_prior_worktree: None,
                }
            });
        let escalation =
            arb_delegation_id().prop_map(|delegation_id| AuditSentinelKind::EscalationRequested {
                plan_id: "P1".to_string(),
                task_id: "T1".to_string(),
                attempt: 1,
                last_error: "err".to_string(),
                worker_branch: None,
                delegation_id: Some(delegation_id),
            });
        let noise = Just(AuditSentinelKind::TaskTransition {
            plan_id: "P1".to_string(),
            task_id: "T1".to_string(),
            from_status: "pending".to_string(),
            to_status: "ready".to_string(),
        });

        prop_oneof![
            dispatch,
            completion,
            approval,
            rejection,
            review_feedback,
            escalation,
            noise
        ]
    }

    #[test]
    fn current_delegation_from_audits_prefers_latest_dispatch_lifecycle() {
        let audits = vec![
            AuditSentinelKind::Dispatch {
                delegation_id: "A".to_string(),
                worker: "codex".to_string(),
                attempt: 1,
            },
            AuditSentinelKind::Dispatch {
                delegation_id: "B".to_string(),
                worker: "codex".to_string(),
                attempt: 2,
            },
            AuditSentinelKind::Completion {
                delegation_id: "B".to_string(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("spur/worker-B".to_string()),
                result_summary: None,
                artifact_uri: None,
                dispatched_base_oid: None,
            },
        ];
        assert_eq!(super::current_delegation_from_audits(&audits), None);
    }

    #[test]
    fn current_delegation_from_audits_clears_on_dispatch_orphan_cleared() {
        let audits = vec![
            AuditSentinelKind::Dispatch {
                delegation_id: "A".to_string(),
                worker: "codex".to_string(),
                attempt: 1,
            },
            AuditSentinelKind::DispatchOrphanCleared {
                delegation_id: "A".to_string(),
                reason: "dispatch send failed".to_string(),
            },
        ];
        assert_eq!(super::current_delegation_from_audits(&audits), None);
    }

    proptest! {
        #[test]
        fn partial_compare_status_from_same_audit_stream_is_never_mismatch(
            terminal_state in prop_oneof![
                Just(AuditSentinelKind::Approval { delegation_id: "del-a".into() }),
                Just(AuditSentinelKind::Rejection { delegation_id: "del-a".into(), feedback: "needs changes".into() }),
                Just(AuditSentinelKind::Completion {
                    delegation_id: "del-a".into(),
                    completion_state: CompletionState::Failed,
                    superseded: false,
                    worker_branch: None,
                    result_summary: Some("failed".into()),
                    artifact_uri: None,
                    dispatched_base_oid: None,
                }),
                Just(AuditSentinelKind::Completion {
                    delegation_id: "del-a".into(),
                    completion_state: CompletionState::Cancelled,
                    superseded: false,
                    worker_branch: None,
                    result_summary: Some("cancelled".into()),
                    artifact_uri: None,
                    dispatched_base_oid: None,
                }),
                Just(AuditSentinelKind::Completion {
                    delegation_id: "del-a".into(),
                    completion_state: CompletionState::Superseded,
                    superseded: true,
                    worker_branch: None,
                    result_summary: None,
                    artifact_uri: None,
                    dispatched_base_oid: None,
                }),
            ]
        ) {
            let legacy_status = match &terminal_state {
                AuditSentinelKind::Approval { .. } => PlanTaskStatus::Approved { summary: None },
                AuditSentinelKind::Rejection { feedback, .. } => PlanTaskStatus::Rejected { feedback: Some(feedback.clone()) },
                AuditSentinelKind::Completion { completion_state: CompletionState::Failed, .. } => {
                    PlanTaskStatus::Failed { error: "worker failed".into() }
                }
                AuditSentinelKind::Completion { completion_state: CompletionState::Cancelled, .. } => {
                    PlanTaskStatus::Cancelled { reason: "worker cancelled".into() }
                }
                AuditSentinelKind::Completion { completion_state: CompletionState::Superseded, .. } => {
                    PlanTaskStatus::Superseded {
                        mutation_id: "m-1".into(),
                        by: vec!["t1a".into()],
                    }
                }
                _ => unreachable!("terminal_state strategy only emits terminal audits"),
            };
            let shadow_status = match &terminal_state {
                AuditSentinelKind::Completion { completion_state: CompletionState::Superseded, .. } => {
                    PlanTaskStatus::Superseded {
                        mutation_id: "unknown".into(),
                        by: Vec::new(),
                    }
                }
                _ => legacy_status.clone(),
            };
            prop_assert!(
                !matches!(
                    super::partial_compare_status(&legacy_status, &shadow_status),
                    super::StatusComparison::Mismatch
                )
            );
        }

        #[test]
        fn current_delegation_from_audits_monotonicity_prefix_consistent(
            audits in proptest::collection::vec(arb_audit_kind(), 0..64)
        ) {
            for i in 0..=audits.len() {
                let prefix = &audits[..i];
                let replayed = prefix.to_vec();
                prop_assert_eq!(
                    super::current_delegation_from_audits(prefix),
                    super::current_delegation_from_audits(&replayed)
                );
            }
        }

        #[test]
        fn current_delegation_from_audits_idempotent(
            audits in proptest::collection::vec(arb_audit_kind(), 0..64)
        ) {
            let one = super::current_delegation_from_audits(&audits);
            let two = super::current_delegation_from_audits(&audits);
            prop_assert_eq!(one, two);
        }

        #[test]
        fn awaiting_review_from_audits_monotonicity_prefix_consistent(
            audits in proptest::collection::vec(arb_audit_kind(), 0..64)
        ) {
            for i in 0..=audits.len() {
                let prefix = &audits[..i];
                let replayed = prefix.to_vec();
                prop_assert_eq!(
                    super::awaiting_review_from_audits(prefix),
                    super::awaiting_review_from_audits(&replayed)
                );
            }
        }

        #[test]
        fn awaiting_review_from_audits_idempotent(
            audits in proptest::collection::vec(arb_audit_kind(), 0..64)
        ) {
            let one = super::awaiting_review_from_audits(&audits);
            let two = super::awaiting_review_from_audits(&audits);
            prop_assert_eq!(one, two);
        }

        #[test]
        fn terminal_status_from_audits_monotonicity_prefix_consistent(
            audits in proptest::collection::vec(arb_audit_kind(), 0..64)
        ) {
            for i in 0..=audits.len() {
                let prefix = &audits[..i];
                let replayed = prefix.to_vec();
                prop_assert_eq!(
                    super::terminal_status_from_audits(prefix),
                    super::terminal_status_from_audits(&replayed)
                );
            }
        }

        #[test]
        fn terminal_status_from_audits_idempotent(
            audits in proptest::collection::vec(arb_audit_kind(), 0..64)
        ) {
            let one: Option<TerminalAuditKind> = super::terminal_status_from_audits(&audits);
            let two: Option<TerminalAuditKind> = super::terminal_status_from_audits(&audits);
            prop_assert_eq!(one, two);
        }

        #[test]
        fn superseded_from_audits_monotonicity_prefix_consistent(
            audits in proptest::collection::vec(arb_audit_kind(), 0..64)
        ) {
            for i in 0..=audits.len() {
                let prefix = &audits[..i];
                let replayed = prefix.to_vec();
                prop_assert_eq!(
                    super::superseded_from_audits(prefix),
                    super::superseded_from_audits(&replayed)
                );
            }
        }

        #[test]
        fn superseded_from_audits_idempotent(
            audits in proptest::collection::vec(arb_audit_kind(), 0..64)
        ) {
            let one = super::superseded_from_audits(&audits);
            let two = super::superseded_from_audits(&audits);
            prop_assert_eq!(one, two);
        }

        #[test]
        fn escalated_from_audits_monotonicity_prefix_consistent(
            audits in proptest::collection::vec(arb_audit_kind(), 0..64)
        ) {
            for i in 0..=audits.len() {
                let prefix = &audits[..i];
                let replayed = prefix.to_vec();
                prop_assert_eq!(
                    super::escalated_from_audits(prefix),
                    super::escalated_from_audits(&replayed)
                );
            }
        }

        #[test]
        fn escalated_from_audits_idempotent(
            audits in proptest::collection::vec(arb_audit_kind(), 0..64)
        ) {
            let one = super::escalated_from_audits(&audits);
            let two = super::escalated_from_audits(&audits);
            prop_assert_eq!(one, two);
        }
    }
}
