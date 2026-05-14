use super::*;

pub(crate) struct ActiveOwnedPlan {
    pub(crate) plan_id: String,
    pub(crate) epic_id: String,
}

pub(crate) async fn find_plan_epic(
    pm: &dyn crate::plan::PmLike,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    operation: &str,
) -> Result<IssueSummary, String> {
    let epics = pm
        .list_issues(IssueFilter {
            labels: vec![crate::plan::labels::plan_id(plan_id)],
            issue_type: Some("epic".to_string()),
            include_closed: true,
            limit: Some(10),
            ..Default::default()
        })
        .await
        .map_err(|error| format!("{operation}: failed to find plan: {error}"))?;

    if epics.is_empty() {
        return Err(format!("{operation}: plan not found: {plan_id}"));
    }

    if epics.len() == 1 {
        return Ok(epics.into_iter().next().expect("non-empty epics"));
    }

    if require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate).is_ok() {
        let Some(advanced) = pm.advanced() else {
            return Err(format!(
                "{operation}: ambiguous plan lookup for {plan_id}; beads advanced backend is unavailable"
            ));
        };
        let candidate_ids = epics
            .iter()
            .map(|epic| epic.id.clone())
            .collect::<HashSet<_>>();
        let mut canonical_ids = HashSet::new();
        let mut critical_parse_error_epics = Vec::new();

        for epic in &epics {
            match advanced.list_comments(&epic.id).await {
                Ok(comments) => {
                    let audits = match crate::plan::projector::collect_sorted_audits_for_issue(
                        &epic.id, comments,
                    ) {
                        Ok(audits) => audits,
                        Err(error) => {
                            tracing::warn!(
                                epic_id = %epic.id,
                                plan_id = %plan_id,
                                operation = %operation,
                                error = %error,
                                "failed to parse plan-submit audits while resolving duplicate plan epics"
                            );
                            critical_parse_error_epics.push(epic.id.clone());
                            continue;
                        }
                    };
                    for audit in audits {
                        if let crate::plan::audit_sentinel::AuditSentinelKind::PlanSubmit {
                            plan_id: audit_plan_id,
                            epic_issue_id,
                            ..
                        } = audit
                        {
                            if audit_plan_id == plan_id && candidate_ids.contains(&epic_issue_id) {
                                canonical_ids.insert(epic_issue_id);
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        epic_id = %epic.id,
                        plan_id = %plan_id,
                        operation = %operation,
                        error = %error,
                        "failed to inspect plan-submit audits while resolving duplicate plan epics"
                    );
                }
            }
        }

        if canonical_ids.len() == 1 {
            let canonical_id = canonical_ids
                .into_iter()
                .next()
                .expect("canonical_ids has one entry");
            if let Some(epic) = epics.iter().find(|epic| epic.id == canonical_id).cloned() {
                tracing::warn!(
                    plan_id = %plan_id,
                    operation = %operation,
                    canonical_epic = %epic.id,
                    "resolved duplicate plan epics via PlanSubmit audit canonical epic"
                );
                return Ok(epic);
            }
        } else if !canonical_ids.is_empty() {
            let mut ids = canonical_ids.into_iter().collect::<Vec<_>>();
            ids.sort();
            return Err(format!(
                "{operation}: ambiguous plan lookup for {plan_id}; PlanSubmit audits disagree on canonical epics: {}",
                ids.join(", ")
            ));
        }

        if !critical_parse_error_epics.is_empty() {
            critical_parse_error_epics.sort();
            critical_parse_error_epics.dedup();
            return Err(format!(
                "{operation}: find_plan_epic: plan_id {plan_id} not found; {} epic(s) had unparseable critical sentinels: [{}]",
                critical_parse_error_epics.len(),
                critical_parse_error_epics.join(", ")
            ));
        }
    }

    let epic_ids = epics
        .iter()
        .map(|epic| epic.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "{operation}: ambiguous plan lookup for {plan_id}; multiple epics matched: {epic_ids}"
    ))
}

pub(crate) async fn apply_issue_update(
    pm: &dyn crate::plan::PmLike,
    issue_id: &str,
    mut update: spur_pm::IssueUpdate,
) -> anyhow::Result<()> {
    let core_update = spur_pm::IssueUpdate {
        status: update.status.take(),
        comment: update.comment.take(),
        priority: update.priority.take(),
        assignee: update.assignee.take(),
        ..Default::default()
    };
    if core_update.status.is_some()
        || core_update.comment.is_some()
        || core_update.priority.is_some()
        || core_update.assignee.is_some()
    {
        pm.update_issue(issue_id, core_update).await?;
    }

    if !update.add_labels.is_empty() || !update.remove_labels.is_empty() {
        pm.update_issue(
            issue_id,
            spur_pm::IssueUpdate {
                add_labels: update.add_labels,
                remove_labels: update.remove_labels,
                ..Default::default()
            },
        )
        .await?;
    }

    Ok(())
}

#[cfg(test)]
pub(crate) fn discover_plan_ids(issues: &[spur_pm::IssueSummary]) -> Vec<String> {
    let mut plan_ids = std::collections::BTreeSet::new();
    for issue in issues {
        if issue.status != "open" || issue.issue_type.as_deref() != Some("epic") {
            continue;
        }
        if issue
            .labels
            .iter()
            .any(|label| label == crate::plan::labels::PLAN_PENDING)
            || !issue
                .labels
                .iter()
                .any(|label| label == crate::plan::labels::PLAN_COMPLETE)
        {
            continue;
        }
        for label in &issue.labels {
            if let Some(plan_id) = crate::plan::labels::parse_plan_id(label) {
                plan_ids.insert(plan_id.to_string());
            }
        }
    }
    plan_ids.into_iter().collect()
}

pub(crate) fn discover_plan_ids_owned_by(
    issues: &[spur_pm::IssueSummary],
    current_brain_session: &spur_acp::SessionId,
) -> Vec<String> {
    let mut plan_ids = std::collections::BTreeSet::new();
    for issue in issues {
        if issue.status != "open" || issue.issue_type.as_deref() != Some("epic") {
            continue;
        }
        if issue
            .labels
            .iter()
            .any(|label| label == crate::plan::labels::PLAN_PENDING)
            || !issue
                .labels
                .iter()
                .any(|label| label == crate::plan::labels::PLAN_COMPLETE)
        {
            continue;
        }
        match crate::plan::ownership::classify_owner(&issue.labels, current_brain_session) {
            crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {}
            crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                tracing::debug!(
                    epic_id = %issue.id,
                    %owner,
                    "startup recovery skipped plan owned by another brain"
                );
                continue;
            }
            crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => {
                tracing::debug!(
                    epic_id = %issue.id,
                    owner = %owners.join(","),
                    "startup recovery skipped plan with ambiguous owner labels"
                );
                continue;
            }
            crate::plan::ownership::PlanOwnerMatch::Unowned => {
                tracing::debug!(
                    epic_id = %issue.id,
                    "startup recovery skipped unowned plan"
                );
                continue;
            }
        }
        for label in &issue.labels {
            if let Some(plan_id) = crate::plan::labels::parse_plan_id(label) {
                plan_ids.insert(plan_id.to_string());
            }
        }
    }
    plan_ids.into_iter().collect()
}

pub(crate) fn mutation_orphan_ids(
    audits: &[crate::plan::audit_sentinel::AuditSentinelKind],
) -> Vec<String> {
    let planned: std::collections::BTreeSet<String> = audits
        .iter()
        .filter_map(|audit| {
            if let crate::plan::audit_sentinel::AuditSentinelKind::MutationPlan {
                mutation_id,
                ..
            } = audit
            {
                Some(mutation_id.clone())
            } else {
                None
            }
        })
        .collect();
    let terminal: std::collections::BTreeSet<String> = audits
        .iter()
        .filter_map(|audit| match audit {
            crate::plan::audit_sentinel::AuditSentinelKind::MutationCommit {
                mutation_id, ..
            } => Some(mutation_id.clone()),
            crate::plan::audit_sentinel::AuditSentinelKind::MutationInvariantViolation {
                mutation_id,
                ..
            } => Some(mutation_id.clone()),
            _ => None,
        })
        .collect();

    planned.difference(&terminal).cloned().collect()
}

pub(crate) fn replace_execution_labels(
    issue: &spur_pm::Issue,
    plan_id: &str,
    agent_name: &str,
) -> spur_pm::IssueUpdate {
    let add_labels = vec![
        crate::plan::labels::plan_id(plan_id),
        crate::plan::labels::agent(agent_name),
    ];
    let mut remove_labels = Vec::new();
    for label in &issue.labels {
        if crate::plan::labels::parse_plan_id(label).is_some()
            || crate::plan::labels::parse_agent(label).is_some()
        {
            remove_labels.push(label.clone());
        }
    }
    filter_remove_labels(&mut remove_labels, &add_labels);

    spur_pm::IssueUpdate {
        add_labels,
        remove_labels,
        ..Default::default()
    }
}

pub(crate) fn replace_task_execution_labels(
    issue: &spur_pm::Issue,
    plan_id: &str,
    task_id: &str,
    agent_name: &str,
) -> spur_pm::IssueUpdate {
    let mut update = replace_execution_labels(issue, plan_id, agent_name);
    update
        .add_labels
        .push(crate::plan::labels::plan_task_id(task_id));
    for label in &issue.labels {
        if crate::plan::labels::parse_plan_task_id(label).is_some() {
            update.remove_labels.push(label.clone());
        }
    }
    filter_remove_labels(&mut update.remove_labels, &update.add_labels);
    update
}

/// Drop any label from `remove_labels` that also appears in `add_labels`.
///
/// The beads CLI processes adds before removes, so an "add X then remove X"
/// pair on the same issue would strip a label we just (idempotently) added.
/// Filter the no-op pair out before issuing the update.
pub(crate) fn filter_remove_labels(remove_labels: &mut Vec<String>, add_labels: &[String]) {
    let add_set: std::collections::HashSet<&str> = add_labels.iter().map(String::as_str).collect();
    remove_labels.retain(|label| !add_set.contains(label.as_str()));
}

pub(crate) fn persisted_plan_epic_plan_id(issue: &spur_pm::Issue) -> Option<&str> {
    if issue.issue_type.as_deref() != Some("epic") {
        return None;
    }

    let is_persisted_plan_scope = issue
        .labels
        .iter()
        .any(|label| label == crate::plan::labels::PLAN_COMPLETE)
        || issue
            .labels
            .iter()
            .any(|label| label == crate::plan::labels::PLAN_PENDING);
    if !is_persisted_plan_scope {
        return None;
    }

    issue
        .labels
        .iter()
        .find_map(|label| crate::plan::labels::parse_plan_id(label))
}

pub(crate) fn invert_label_update(update: &spur_pm::IssueUpdate) -> spur_pm::IssueUpdate {
    spur_pm::IssueUpdate {
        add_labels: update.remove_labels.clone(),
        remove_labels: update.add_labels.clone(),
        ..Default::default()
    }
}

pub(crate) fn legacy_reclaim_needed(has_rev1_merge_base_metadata: bool) -> bool {
    !has_rev1_merge_base_metadata
}

pub(crate) async fn any_open_epic_lacks_rev1_metadata(
    pm: &dyn crate::plan::PmLike,
    feature_gate: &spur_license::FeatureGate,
) -> anyhow::Result<bool> {
    #[cfg(any(test, feature = "test-support"))]
    pause_startup_recovery_if_probed().await;
    let epics = pm
        .list_issues(spur_pm::IssueFilter {
            status: Some("open".to_string()),
            issue_type: Some("epic".to_string()),
            limit: Some(1_000),
            ..Default::default()
        })
        .await?;

    require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate)
        .map_err(|error| anyhow::anyhow!(feature_error_message(error)))?;
    let Some(adv) = pm.advanced() else {
        return Ok(false);
    };

    for epic in &epics {
        if epic
            .labels
            .iter()
            .any(|label| label == crate::plan::labels::PLAN_PENDING)
        {
            continue;
        }
        if let Some(plan_id) = epic
            .labels
            .iter()
            .find_map(|l| crate::plan::labels::parse_plan_id(l))
        {
            let comments = adv.list_comments(&epic.id).await?;
            let audits =
                crate::plan::projector::collect_sorted_audits_for_issue(&epic.id, comments)?;
            let has_rev1_metadata = audits.iter().any(|audit| {
                matches!(
                    audit,
                    crate::plan::audit_sentinel::AuditSentinelKind::PlanSubmit {
                        plan_id: pid,
                        base_snapshot_branch: Some(_),
                        base_snapshot_oid: Some(_),
                        ..
                    } if pid == plan_id
                )
            });
            if !has_rev1_metadata {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[doc(hidden)]
pub async fn compensate_mutation_orphans(
    pm: Arc<spur_pm::PmService>,
    feature_gate: Arc<spur_license::FeatureGate>,
    task_id: &str,
) -> anyhow::Result<()> {
    require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate.as_ref())
        .map_err(|error| anyhow::anyhow!(feature_error_message(error)))?;
    let adv = pm
        .advanced()
        .ok_or_else(|| anyhow::anyhow!("mutation recovery requires beads backend"))?;
    let audits = crate::plan::projector::collect_sorted_audits_for_issue(
        task_id,
        adv.list_comments(task_id).await?,
    )?;

    for mutation_id in mutation_orphan_ids(&audits) {
        if let Ok(uuid) = uuid::Uuid::parse_str(&mutation_id) {
            let mutation_label = crate::plan::labels::mutation_id_label(&uuid);
            let summaries = pm
                .list_issues(spur_pm::IssueFilter {
                    labels: vec![mutation_label],
                    limit: Some(1_000),
                    ..Default::default()
                })
                .await?;
            let child_ids: Vec<String> = summaries.into_iter().map(|summary| summary.id).collect();
            for child_id in &child_ids {
                let child_issue = pm.get_issue(child_id).await?;
                let remove_labels = child_issue
                    .labels
                    .iter()
                    .filter(|label| {
                        crate::plan::labels::parse_plan_id(label).is_some()
                            || crate::plan::labels::parse_plan_task_id(label).is_some()
                            || crate::plan::labels::parse_agent(label).is_some()
                    })
                    .cloned()
                    .collect();
                apply_issue_update(
                    pm.as_ref(),
                    child_id,
                    spur_pm::IssueUpdate {
                        status: Some(pm.closed_status().to_string()),
                        remove_labels,
                        ..Default::default()
                    },
                )
                .await?;
            }
            apply_issue_update(
                pm.as_ref(),
                task_id,
                spur_pm::IssueUpdate {
                    status: Some("open".to_string()),
                    remove_labels: crate::plan::labels::superseded_by_labels(&child_ids),
                    ..Default::default()
                },
            )
            .await?;
        }

        adv.add_comment(
            task_id,
            &crate::plan::audit_sentinel::encode_comment(
                &crate::plan::audit_sentinel::AuditSentinelKind::MutationInvariantViolation {
                    mutation_id: mutation_id.clone(),
                    violation: "restart-orphan".into(),
                    rollback_status: "compensated".into(),
                    rollback_ops_succeeded: Vec::new(),
                    rollback_ops_failed: Vec::new(),
                },
            ),
        )
        .await?;
    }
    Ok(())
}

#[doc(hidden)]
pub async fn resolve_dispatch_orphan(
    pm: Arc<spur_pm::PmService>,
    feature_gate: Arc<spur_license::FeatureGate>,
    task_id: &str,
) -> anyhow::Result<bool> {
    let issue = pm.get_issue(task_id).await?;
    if issue.status != "open" {
        return Ok(false);
    }
    let Some(delegation_id) = issue.labels.iter().find_map(|label| {
        crate::plan::labels::parse_delegation_id(label)
            .or_else(|| label.strip_prefix("delegation-id:"))
    }) else {
        return Ok(false);
    };
    if crate::plan::projector::has_ready_for_review_label_compat(&issue.labels) {
        return Ok(false);
    }

    require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate.as_ref())
        .map_err(|error| anyhow::anyhow!(feature_error_message(error)))?;
    let adv = pm
        .advanced()
        .ok_or_else(|| anyhow::anyhow!("dispatch recovery requires beads backend"))?;
    let audits = crate::plan::projector::collect_sorted_audits_for_issue(
        task_id,
        adv.list_comments(task_id).await?,
    )?;
    if audits.iter().any(|audit| matches!(
        audit,
        crate::plan::audit_sentinel::AuditSentinelKind::Completion { delegation_id: did, .. } if did == delegation_id
    )) {
        return Ok(false);
    }

    adv.add_comment(
        task_id,
        &crate::plan::audit_sentinel::encode_comment(
            &crate::plan::audit_sentinel::AuditSentinelKind::DispatchOrphanCleared {
                delegation_id: delegation_id.to_string(),
                reason: ORPHAN_CLEAR_REASON_RESTART.into(),
            },
        ),
    )
    .await?;
    crate::plan::clear_dispatch_intent(pm.as_ref(), task_id, delegation_id).await?;
    Ok(true)
}
