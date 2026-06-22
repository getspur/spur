pub(super) fn setup_overlay_conflict(
    status: &spur_acp::DelegationStatus,
) -> Option<(&str, &[String])> {
    match status {
        spur_acp::DelegationStatus::SetupFailed {
            error:
                spur_acp::AttemptSetupError::OverlayConflict {
                    source_task_id,
                    files,
                },
        } => Some((source_task_id.as_str(), files.as_slice())),
        _ => None,
    }
}

pub(super) async fn persist_setup_overlay_conflict(
    pm: &dyn crate::plan::PmLike,
    issue_id: &str,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    delegation_id: &str,
    source_task_id: &str,
    files: &[String],
) -> anyhow::Result<()> {
    crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    )
    .map_err(|error| anyhow::anyhow!(crate::server::feature_error_message(error)))?;
    let adv = pm
        .advanced()
        .ok_or_else(|| anyhow::anyhow!("setup conflict routing requires beads backend"))?;
    let signal_id = uuid::Uuid::new_v4().to_string();
    let reason = serde_json::to_string(&serde_json::json!({
        "dep_task_id": source_task_id,
        "files": files,
    }))?;

    adv.add_comment(
        issue_id,
        &crate::plan::audit_sentinel::encode_comment(
            &crate::plan::audit_sentinel::AuditSentinelKind::Signal {
                signal_id: signal_id.clone(),
                delegation_id: String::new(),
                kind: "integration-conflict".to_string(),
                severity: 1.0,
                reason,
            },
        ),
    )
    .await?;
    let signal_comment = format!(
        "{}\n{}",
        crate::plan::signals::SENTINEL_PREFIX,
        serde_json::to_string(&serde_json::json!({
            "signal_id": signal_id,
            "kind": "integration_conflict",
            "dep_task_id": source_task_id,
            "files": files,
        }))?
    );
    adv.add_comment(issue_id, &signal_comment).await?;

    crate::plan::clear_dispatch_intent(pm, issue_id, delegation_id).await?;
    pm.update_issue(
        issue_id,
        spur_pm::IssueUpdate {
            add_labels: vec![crate::plan::labels::SIGNAL_LABEL_INTEGRATION_CONFLICT.to_string()],
            ..Default::default()
        },
    )
    .await?;
    tracing::warn!(
        %plan_id,
        %issue_id,
        %delegation_id,
        dep_task_id = %source_task_id,
        files = ?files,
        "routed setup overlay conflict to integration-conflict signal"
    );
    Ok(())
}

async fn persist_predispatch_overlay_conflict(
    pm: &dyn crate::plan::PmLike,
    issue_id: &str,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    source_task_id: &str,
    files: &[String],
) -> anyhow::Result<()> {
    crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    )
    .map_err(|error| anyhow::anyhow!(crate::server::feature_error_message(error)))?;
    let adv = pm
        .advanced()
        .ok_or_else(|| anyhow::anyhow!("setup conflict routing requires beads backend"))?;
    let signal_id = uuid::Uuid::new_v4().to_string();
    let reason = serde_json::to_string(&serde_json::json!({
        "dep_task_id": source_task_id,
        "files": files,
    }))?;

    adv.add_comment(
        issue_id,
        &crate::plan::audit_sentinel::encode_comment(
            &crate::plan::audit_sentinel::AuditSentinelKind::Signal {
                signal_id: signal_id.clone(),
                delegation_id: String::new(),
                kind: "integration-conflict".to_string(),
                severity: 1.0,
                reason,
            },
        ),
    )
    .await?;
    let signal_comment = format!(
        "{}\n{}",
        crate::plan::signals::SENTINEL_PREFIX,
        serde_json::to_string(&serde_json::json!({
            "signal_id": signal_id,
            "kind": "integration_conflict",
            "dep_task_id": source_task_id,
            "files": files,
        }))?
    );
    adv.add_comment(issue_id, &signal_comment).await?;

    pm.update_issue(
        issue_id,
        spur_pm::IssueUpdate {
            add_labels: vec![crate::plan::labels::SIGNAL_LABEL_INTEGRATION_CONFLICT.to_string()],
            ..Default::default()
        },
    )
    .await?;
    tracing::warn!(
        %plan_id,
        %issue_id,
        dep_task_id = %source_task_id,
        files = ?files,
        "routed predispatch overlay conflict to integration-conflict signal"
    );
    Ok(())
}

impl super::Reconciler {
    pub(super) async fn transition_to_blocked_on_setup_conflict(
        &self,
        plan_id: &str,
        task_id: &str,
        dep_task_id: &str,
        files: &[String],
    ) -> anyhow::Result<()> {
        let projected = self.project_plan_from_beads(plan_id).await?;
        let issue_id = projected
            .tasks
            .iter()
            .find(|entry| entry.spec.task_id == task_id)
            .and_then(|entry| entry.spec.issue_id.as_deref())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "task '{task_id}' in plan '{plan_id}' has no issue_id for setup-conflict transition"
                )
            })?;

        persist_predispatch_overlay_conflict(
            self.pm.as_ref(),
            issue_id,
            self.feature_gate.as_ref(),
            plan_id,
            dep_task_id,
            files,
        )
        .await?;
        self.fast_forward.notify_one();
        self.emit_snapshot_for_plan(plan_id).await;
        Ok(())
    }
}
