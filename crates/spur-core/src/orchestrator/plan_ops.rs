use spur_acp::{
    PlanLifecycleEvent, PlanLoadWarningEvent, PlanOwnerStateEvent, PlanSummaryCountsEvent,
    PlanSummaryEvent, SessionId,
};
use spur_pm::{IssueSummary, PmService};
use tracing::warn;

pub(super) const PLAN_COMPLETE_LABEL: &str = "spur:plan-complete";
pub(super) const PLAN_PENDING_LABEL: &str = "spur:plan-pending";
pub(super) const PLAN_ID_PREFIX: &str = "spur:plan-id:";
pub(super) const PLAN_OWNER_PREFIX: &str = "spur:plan-owner:";
pub(super) const READY_FOR_REVIEW_LABEL: &str = "spur:ready-for-review";
pub(super) const REVIEW_REJECTED_LABEL: &str = "spur:review-rejected";

pub(super) fn parse_label_value<'a>(label: &'a str, prefix: &str) -> Option<&'a str> {
    label.strip_prefix(prefix).filter(|value| !value.is_empty())
}

pub(super) fn compact_label_component(value: &str) -> String {
    value.replace('-', "")
}

pub(super) fn plan_id_from_labels(labels: &[String]) -> Option<String> {
    labels
        .iter()
        .find_map(|label| parse_label_value(label, PLAN_ID_PREFIX))
        .map(ToOwned::to_owned)
}

pub(super) fn plan_owner_state_from_labels(
    labels: &[String],
    current_brain_session: Option<&SessionId>,
) -> PlanOwnerStateEvent {
    let mut owners: Vec<String> = labels
        .iter()
        .filter_map(|label| parse_label_value(label, PLAN_OWNER_PREFIX))
        .map(ToOwned::to_owned)
        .collect();
    owners.sort();
    owners.dedup();

    match owners.as_slice() {
        [] => PlanOwnerStateEvent::Unowned,
        [owner] => {
            let current = current_brain_session.map(|session| compact_label_component(&session.0));
            if current.as_deref() == Some(owner.as_str()) {
                PlanOwnerStateEvent::Mine
            } else {
                PlanOwnerStateEvent::Other {
                    owner: owner.clone(),
                }
            }
        }
        _ => PlanOwnerStateEvent::Ambiguous { owners },
    }
}

pub(super) fn count_plan_children(
    issues: &[IssueSummary],
    epic_id: &str,
) -> PlanSummaryCountsEvent {
    let mut counts = PlanSummaryCountsEvent {
        total: 0,
        pending: 0,
        ready: 0,
        running: 0,
        awaiting_review: 0,
        approved: 0,
        rejected: 0,
        failed: 0,
        cancelled: 0,
    };

    for issue in issues {
        if issue.id == epic_id || issue.issue_type.as_deref() == Some("epic") {
            continue;
        }

        counts.total += 1;
        let status = issue.status.as_str();
        let has_label = |needle: &str| issue.labels.iter().any(|label| label == needle);

        if status == "cancelled" {
            counts.cancelled += 1;
        } else if status == "failed" {
            counts.failed += 1;
        } else if matches!(status, "rejected" | "review-rejected")
            || has_label(REVIEW_REJECTED_LABEL)
            || has_label("rejected")
        {
            counts.rejected += 1;
        } else if matches!(status, "approved" | "closed") {
            counts.approved += 1;
        } else if status == "awaiting_review" || has_label(READY_FOR_REVIEW_LABEL) {
            counts.awaiting_review += 1;
        } else if matches!(status, "in_progress" | "running" | "dispatched") {
            counts.running += 1;
        } else if status == "ready" {
            counts.ready += 1;
        } else {
            counts.pending += 1;
        }
    }

    counts
}

pub(super) fn lifecycle_from_plan(
    epic: &IssueSummary,
    counts: Option<&PlanSummaryCountsEvent>,
) -> PlanLifecycleEvent {
    if epic.labels.iter().any(|label| label == PLAN_PENDING_LABEL) {
        return PlanLifecycleEvent::Pending;
    }
    if epic.status == "cancelled" || epic.status == "failed" {
        return PlanLifecycleEvent::Failed;
    }
    if epic.status == "closed" {
        return PlanLifecycleEvent::Complete;
    }

    let Some(counts) = counts else {
        return if epic.labels.iter().any(|label| label == PLAN_COMPLETE_LABEL) {
            PlanLifecycleEvent::Running
        } else {
            PlanLifecycleEvent::Unknown
        };
    };

    if counts.failed > 0 || counts.rejected > 0 {
        PlanLifecycleEvent::Failed
    } else if counts.total > 0 && counts.approved + counts.cancelled == counts.total {
        PlanLifecycleEvent::Complete
    } else if counts.awaiting_review > 0 {
        PlanLifecycleEvent::AwaitingReview
    } else if counts.total > 0 && counts.pending == counts.total {
        PlanLifecycleEvent::Pending
    } else if counts.total > 0 || epic.labels.iter().any(|label| label == PLAN_COMPLETE_LABEL) {
        PlanLifecycleEvent::Running
    } else {
        PlanLifecycleEvent::Unknown
    }
}

#[derive(Debug, Clone)]
pub(super) struct PlanSummaryCandidate {
    pub(super) summary: PlanSummaryEvent,
    pub(super) canonical_epic_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct PlanSummaryLoad {
    pub(super) plans: Vec<PlanSummaryEvent>,
    pub(super) warnings: Vec<PlanLoadWarningEvent>,
}

pub(super) fn owner_state_text(owner_state: &PlanOwnerStateEvent) -> String {
    match owner_state {
        PlanOwnerStateEvent::Mine => "owned by this brain".into(),
        PlanOwnerStateEvent::Unowned => "unowned".into(),
        PlanOwnerStateEvent::Other { owner } => format!("owned by {owner}"),
        PlanOwnerStateEvent::Ambiguous { owners } if owners.is_empty() => {
            "ambiguous ownership".into()
        }
        PlanOwnerStateEvent::Ambiguous { owners } => {
            format!("ambiguous ownership: {}", owners.join(", "))
        }
    }
}

pub(super) fn duplicate_plan_warning(
    plan_id: &str,
    stale_epic_ids: Vec<String>,
    canonical_summary: Option<&PlanSummaryEvent>,
) -> PlanLoadWarningEvent {
    let stale_text = stale_epic_ids.join(", ");
    let canonical_epic_id = canonical_summary.map(|summary| summary.epic_id.clone());
    let canonical_owner_state = canonical_summary.map(|summary| summary.owner_state.clone());
    let message = if let Some(summary) = canonical_summary {
        let noun = if stale_epic_ids.len() == 1 {
            "epic"
        } else {
            "epics"
        };
        format!(
            "Plan {plan_id} has stale duplicate {noun} {stale_text}; using canonical epic {} ({})",
            summary.epic_id,
            owner_state_text(&summary.owner_state)
        )
    } else {
        format!(
            "Plan {plan_id} has duplicate epics {stale_text}, but no canonical PlanSubmit audit; claim/resume may be blocked"
        )
    };

    PlanLoadWarningEvent {
        plan_id: plan_id.to_string(),
        canonical_epic_id,
        stale_epic_ids,
        canonical_owner_state,
        message,
    }
}

pub(super) fn canonicalize_plan_summary_candidates(
    candidates: Vec<PlanSummaryCandidate>,
) -> PlanSummaryLoad {
    let mut grouped: std::collections::BTreeMap<String, Vec<PlanSummaryCandidate>> =
        std::collections::BTreeMap::new();
    for candidate in candidates {
        grouped
            .entry(candidate.summary.plan_id.clone())
            .or_default()
            .push(candidate);
    }

    let mut load = PlanSummaryLoad::default();
    for (plan_id, mut group) in grouped {
        group.sort_by(|left, right| left.summary.epic_id.cmp(&right.summary.epic_id));
        if group.len() == 1 {
            load.plans.push(group.remove(0).summary);
            continue;
        }

        let epic_ids = group
            .iter()
            .map(|candidate| candidate.summary.epic_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let canonical_ids = group
            .iter()
            .filter_map(|candidate| candidate.canonical_epic_id.as_ref())
            .filter(|epic_id| epic_ids.contains(*epic_id))
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();

        if canonical_ids.len() == 1 {
            let canonical_epic_id = canonical_ids
                .into_iter()
                .next()
                .expect("one canonical id exists");
            let canonical_index = group
                .iter()
                .position(|candidate| candidate.summary.epic_id == canonical_epic_id)
                .expect("canonical id came from group candidate ids");
            let canonical = group.remove(canonical_index);
            let stale_epic_ids = group
                .iter()
                .map(|candidate| candidate.summary.epic_id.clone())
                .collect::<Vec<_>>();
            load.warnings.push(duplicate_plan_warning(
                &plan_id,
                stale_epic_ids,
                Some(&canonical.summary),
            ));
            load.plans.push(canonical.summary);
        } else {
            let duplicate_epic_ids = group
                .iter()
                .map(|candidate| candidate.summary.epic_id.clone())
                .collect::<Vec<_>>();
            load.warnings
                .push(duplicate_plan_warning(&plan_id, duplicate_epic_ids, None));
            load.plans
                .extend(group.into_iter().map(|candidate| candidate.summary));
        }
    }

    load
}

pub(super) async fn load_plan_summaries(
    pm: &PmService,
    current_brain_session: Option<&SessionId>,
) -> anyhow::Result<PlanSummaryLoad> {
    let epics = pm
        .list_issues(spur_pm::IssueFilter {
            issue_type: Some("epic".into()),
            include_closed: true,
            limit: Some(1000),
            ..Default::default()
        })
        .await?;

    let mut candidates = Vec::new();
    for epic in epics {
        let Some(plan_id) = plan_id_from_labels(&epic.labels) else {
            continue;
        };

        let plan_label = format!("{PLAN_ID_PREFIX}{plan_id}");
        let counts = match pm
            .list_issues(spur_pm::IssueFilter {
                labels: vec![plan_label],
                include_closed: true,
                limit: Some(1000),
                ..Default::default()
            })
            .await
        {
            Ok(children) => Some(count_plan_children(&children, &epic.id)),
            Err(err) => {
                warn!(
                    plan_id = %plan_id,
                    error = %err,
                    "failed to load persisted plan children for summary counts"
                );
                None
            }
        };

        let (source_body_preview, created_at, updated_at) = match pm.get_issue(&epic.id).await {
            Ok(issue) => (
                issue_body_preview(&issue.body),
                Some(issue.created_at),
                Some(issue.updated_at),
            ),
            Err(err) => {
                warn!(
                    plan_id = %plan_id,
                    epic_id = %epic.id,
                    error = %err,
                    "failed to load plan source issue body preview"
                );
                (None, None, None)
            }
        };

        candidates.push(PlanSummaryCandidate {
            summary: PlanSummaryEvent {
                plan_id,
                epic_id: epic.id.clone(),
                title: epic.title.clone(),
                source_body_preview,
                owner_state: plan_owner_state_from_labels(&epic.labels, current_brain_session),
                lifecycle: lifecycle_from_plan(&epic, counts.as_ref()),
                counts,
                updated_at,
                created_at,
            },
            canonical_epic_id: None,
        });
    }

    annotate_plan_summary_canonical_epics(pm, &mut candidates).await;

    Ok(canonicalize_plan_summary_candidates(candidates))
}

pub(super) async fn annotate_plan_summary_canonical_epics(
    pm: &PmService,
    candidates: &mut [PlanSummaryCandidate],
) {
    let duplicate_epics_by_plan = {
        let mut grouped: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
            std::collections::BTreeMap::new();
        for candidate in candidates.iter() {
            grouped
                .entry(candidate.summary.plan_id.clone())
                .or_default()
                .insert(candidate.summary.epic_id.clone());
        }
        grouped
            .into_iter()
            .filter(|(_, epic_ids)| epic_ids.len() > 1)
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    if duplicate_epics_by_plan.is_empty() {
        return;
    }

    let Some(advanced) = pm.advanced() else {
        return;
    };

    for candidate in candidates.iter_mut() {
        let Some(candidate_ids) = duplicate_epics_by_plan.get(&candidate.summary.plan_id) else {
            continue;
        };
        let comments = match advanced.list_comments(&candidate.summary.epic_id).await {
            Ok(comments) => comments,
            Err(error) => {
                warn!(
                    plan_id = %candidate.summary.plan_id,
                    epic_id = %candidate.summary.epic_id,
                    error = %error,
                    "failed to inspect PlanSubmit audit while loading duplicate plan summaries"
                );
                continue;
            }
        };
        let audits = spur_mcp::plan::projector::collect_sorted_audits_for_issue(
            &candidate.summary.epic_id,
            comments,
        );
        let audits = match audits {
            Ok(audits) => audits,
            Err(error) => {
                // Safe to skip here: this annotation path is non-authoritative summary metadata.
                warn!(
                    plan_id = %candidate.summary.plan_id,
                    epic_id = %candidate.summary.epic_id,
                    error = %error,
                    "failed to parse PlanSubmit audit while loading duplicate plan summaries"
                );
                continue;
            }
        };
        for audit in audits {
            if let spur_mcp::plan::audit_sentinel::AuditSentinelKind::PlanSubmit {
                plan_id,
                epic_issue_id,
                ..
            } = audit
            {
                if plan_id == candidate.summary.plan_id && candidate_ids.contains(&epic_issue_id) {
                    candidate.canonical_epic_id = Some(epic_issue_id);
                    break;
                }
            }
        }
    }
}

pub(super) fn issue_body_preview(body: &str) -> Option<String> {
    let body = body.trim();
    if body.is_empty() {
        return None;
    }

    let mut preview: String = body.chars().take(500).collect();
    if body.chars().count() > 500 {
        preview.push_str("...");
    }
    Some(preview)
}

#[cfg(test)]
mod plan_summary_warning_tests {
    use super::*;

    fn candidate(
        plan_id: &str,
        epic_id: &str,
        owner_state: PlanOwnerStateEvent,
        canonical_epic_id: Option<&str>,
    ) -> PlanSummaryCandidate {
        PlanSummaryCandidate {
            summary: PlanSummaryEvent {
                plan_id: plan_id.into(),
                epic_id: epic_id.into(),
                title: format!("Plan {plan_id}"),
                source_body_preview: None,
                owner_state,
                lifecycle: PlanLifecycleEvent::Pending,
                counts: None,
                updated_at: None,
                created_at: None,
            },
            canonical_epic_id: canonical_epic_id.map(str::to_string),
        }
    }

    #[test]
    fn canonicalizes_duplicate_plan_epics_and_warns_about_stale_aliases() {
        let result = canonicalize_plan_summary_candidates(vec![
            candidate(
                "plan-dup",
                "bd-stale",
                PlanOwnerStateEvent::Unowned,
                Some("bd-canonical"),
            ),
            candidate(
                "plan-dup",
                "bd-canonical",
                PlanOwnerStateEvent::Other {
                    owner: "brain-42".into(),
                },
                None,
            ),
        ]);

        assert_eq!(result.plans.len(), 1);
        assert_eq!(result.plans[0].epic_id, "bd-canonical");
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].plan_id, "plan-dup");
        assert_eq!(
            result.warnings[0].canonical_epic_id.as_deref(),
            Some("bd-canonical")
        );
        assert_eq!(result.warnings[0].stale_epic_ids, vec!["bd-stale"]);
        assert!(matches!(
            result.warnings[0].canonical_owner_state,
            Some(PlanOwnerStateEvent::Other { ref owner }) if owner == "brain-42"
        ));
        assert!(result.warnings[0].message.contains("bd-stale"));
        assert!(result.warnings[0].message.contains("bd-canonical"));
    }
}
