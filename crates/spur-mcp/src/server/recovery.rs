use super::*;

impl McpCallbackServer {
    pub(crate) fn request_startup_recovery(&self) {
        let mut state = self.startup_recovery.lock().unwrap();
        if state.handle.is_none() {
            state.pending = true;
        }
    }

    /// Spawn legacy persisted-plan recovery after the brain session id is
    /// available. Safe no-op when startup did not request recovery, when the
    /// task is already running, or when the brain has not been bound yet.
    #[doc(hidden)]
    pub fn spawn_startup_recovery_if_ready(self: Arc<Self>) {
        let mut state = self.startup_recovery.lock().unwrap();
        if !state.pending || state.handle.is_some() {
            return;
        }
        if self.task_tracker.is_closed() {
            state.pending = false;
            return;
        }
        if self.brain_session_id.get().is_none() {
            return;
        }
        let Some(pm) = self.pm_service.as_ref().cloned() else {
            state.pending = false;
            return;
        };
        if self
            .require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED)
            .is_err()
            || pm.advanced().is_none()
        {
            state.pending = false;
            return;
        }

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let server = Arc::clone(&self);
        let handle = AbortOnDropHandle::new(tokio::spawn(async move {
            let result = tokio::select! {
                _ = cancel_rx => {
                    tracing::debug!("persisted-plan startup recovery cancelled");
                    return;
                }
                result = server.reclaim_persisted_plans_on_startup(pm) => result,
            };
            if let Err(error) = result {
                tracing::warn!(%error, "persisted-plan startup recovery failed");
            }
        }));

        state.pending = false;
        state.handle = Some(StartupRecoveryTaskHandle {
            cancel_tx: Some(cancel_tx),
            handle,
        });
    }

    /// Return the feature gate snapshot shared with the license runtime.
    pub(crate) async fn recover_persisted_plans(
        &self,
        pm: Arc<spur_pm::PmService>,
    ) -> anyhow::Result<()> {
        let brain_session_id = self.brain_session_id_ready().await.clone();
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

        for plan_id in discover_plan_ids_owned_by(&epics, brain_session_id.as_session_id()) {
            let projected = crate::plan::projector::project_plan_from_beads(
                pm.as_ref(),
                &plan_id,
                self.feature_gate.as_ref(),
            )
            .await?;
            for task in &projected.tasks {
                if let Some(issue_id) = &task.spec.issue_id {
                    compensate_mutation_orphans(
                        Arc::clone(&pm),
                        Arc::clone(&self.feature_gate),
                        issue_id,
                    )
                    .await?;
                    let _ = resolve_dispatch_orphan(
                        Arc::clone(&pm),
                        Arc::clone(&self.feature_gate),
                        issue_id,
                    )
                    .await?;
                }
            }
            let refreshed = crate::plan::projector::project_plan_from_beads(
                pm.as_ref(),
                &plan_id,
                self.feature_gate.as_ref(),
            )
            .await?;
            self.install_projected_plan(refreshed, true).await;
        }

        Ok(())
    }

    pub(crate) async fn sweep_stale_pending_plans_on_startup(
        &self,
        pm: Arc<spur_pm::PmService>,
    ) -> anyhow::Result<()> {
        #[cfg(any(test, feature = "test-support"))]
        pause_startup_recovery_if_probed().await;
        let pending_epics = pm
            .list_issues(spur_pm::IssueFilter {
                labels: vec![crate::plan::labels::PLAN_PENDING.to_string()],
                status: Some("open".to_string()),
                issue_type: Some("epic".to_string()),
                limit: Some(1_000),
                ..Default::default()
            })
            .await?;
        if pending_epics.is_empty() {
            return Ok(());
        }

        let now = chrono::Utc::now();
        let grace = chrono::Duration::from_std(self.plan_pending_grace)
            .unwrap_or_else(|_| chrono::Duration::hours(1));
        for summary in pending_epics {
            let epic = pm.get_issue(&summary.id).await?;
            let age = now.signed_duration_since(epic.created_at);
            if age < grace {
                continue;
            }

            let age_secs = age.num_seconds();
            let plan_id = epic
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_plan_id(label))
                .map(str::to_string);
            let Some(plan_id_value) = plan_id.as_deref() else {
                self.emit_plan_pending_sweep_event(
                    None,
                    &epic.id,
                    "skipped",
                    0,
                    age_secs,
                    "pending epic has no spur:plan-id label",
                );
                continue;
            };

            let children = self
                .list_plan_task_issues_for_pending_sweep(pm.as_ref(), plan_id_value)
                .await?;
            let mut skip_reason: Option<String> = None;
            for child in &children {
                match self
                    .pending_sweep_allows_child_status(pm.as_ref(), child)
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => {
                        skip_reason = Some(format!(
                            "child '{}' is not open or previously quarantined",
                            child.id
                        ));
                        break;
                    }
                    Err(err) => {
                        skip_reason = Some(format!(
                            "comment lookup failed for child '{}': {err}",
                            child.id
                        ));
                        break;
                    }
                }
            }
            if let Some(reason) = skip_reason {
                self.emit_plan_pending_sweep_event(
                    plan_id.clone(),
                    &epic.id,
                    "skipped",
                    children.len() as u32,
                    age_secs,
                    &reason,
                );
                continue;
            }

            let comment = format!(
                "{PLAN_PENDING_SWEEP_COMMENT_PREFIX} `{}` (epic `{}`): graph stayed `{}` for {}s without flipping to `{}`. Children quarantined: {}.",
                plan_id_value,
                epic.id,
                crate::plan::labels::PLAN_PENDING,
                age_secs,
                crate::plan::labels::PLAN_COMPLETE,
                children.len()
            );
            let terminal_status = pm.closed_status().to_string();
            for child in &children {
                if child.status != "open" {
                    continue;
                }
                pm.update_issue(
                    &child.id,
                    IssueUpdate {
                        status: Some(terminal_status.clone()),
                        comment: Some(comment.clone()),
                        ..Default::default()
                    },
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to quarantine stale pending-plan child '{}'",
                        child.id
                    )
                })?;
            }
            pm.update_issue(
                &epic.id,
                IssueUpdate {
                    status: Some(terminal_status),
                    comment: Some(comment),
                    remove_labels: vec![crate::plan::labels::PLAN_PENDING.to_string()],
                    ..Default::default()
                },
            )
            .await
            .with_context(|| {
                format!("failed to quarantine stale pending-plan epic '{}'", epic.id)
            })?;

            self.emit_plan_pending_sweep_event(
                plan_id,
                &epic.id,
                "quarantined",
                children.len() as u32,
                age_secs,
                "stale pending plan exceeded grace",
            );
        }

        Ok(())
    }

    pub(crate) async fn pending_sweep_allows_child_status(
        &self,
        pm: &spur_pm::PmService,
        child: &spur_pm::Issue,
    ) -> anyhow::Result<bool> {
        if child.status == "open" {
            return Ok(true);
        }
        self.issue_has_plan_pending_sweep_comment(pm, &child.id)
            .await
    }

    pub(crate) async fn issue_has_plan_pending_sweep_comment(
        &self,
        pm: &spur_pm::PmService,
        issue_id: &str,
    ) -> anyhow::Result<bool> {
        if require_feature(
            FeatureKey::PM_PRO_BEADS_ADVANCED,
            self.feature_gate.as_ref(),
        )
        .is_err()
        {
            return Ok(false);
        }
        let Some(advanced) = pm.advanced() else {
            return Ok(false);
        };
        let comments = advanced.list_comments(issue_id).await?;
        Ok(comments
            .iter()
            .any(|comment| comment.body.starts_with(PLAN_PENDING_SWEEP_COMMENT_PREFIX)))
    }

    pub(crate) async fn list_plan_task_issues_for_pending_sweep(
        &self,
        pm: &spur_pm::PmService,
        plan_id: &str,
    ) -> anyhow::Result<Vec<spur_pm::Issue>> {
        let summaries = pm
            .list_issues(IssueFilter {
                labels: vec![crate::plan::labels::plan_id(plan_id)],
                issue_type: Some("task".to_string()),
                include_closed: true,
                limit: Some(1_000),
                ..Default::default()
            })
            .await?;

        let mut issues = Vec::with_capacity(summaries.len());
        for summary in summaries {
            issues.push(pm.get_issue(&summary.id).await?);
        }
        Ok(issues)
    }

    pub(crate) fn emit_plan_pending_sweep_event(
        &self,
        plan_id: Option<String>,
        epic_id: &str,
        action: &str,
        child_count: u32,
        age_secs: i64,
        reason: &str,
    ) {
        tracing::warn!(
            target: "spur.plan_pending_sweep",
            plan_id = plan_id.as_deref().unwrap_or(""),
            %epic_id,
            %action,
            child_count,
            age_secs,
            %reason,
            "startup pending-plan sweep action"
        );
        if let Some(sink) = self.event_sink.as_deref() {
            sink.emit(spur_acp::SpurEventBody::PlanPendingSweep {
                plan_id,
                epic_id: epic_id.to_string(),
                action: action.to_string(),
                child_count,
                age_secs,
                reason: reason.to_string(),
            });
        }
    }

    pub(crate) async fn reclaim_persisted_plans_on_startup(
        &self,
        pm: Arc<spur_pm::PmService>,
    ) -> anyhow::Result<()> {
        debug!("startup recovery maintenance started");

        debug!("startup pending-plan sweep started");
        match self
            .sweep_stale_pending_plans_on_startup(Arc::clone(&pm))
            .await
        {
            Ok(()) => {
                debug!("startup pending-plan sweep finished");
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "startup pending-plan sweep failed; continuing startup recovery"
                );
            }
        }

        debug!("startup rev1 metadata check started");
        let has_rev1_metadata = match any_open_epic_lacks_rev1_metadata(
            pm.as_ref(),
            self.feature_gate.as_ref(),
        )
        .await
        {
            Ok(lacks_rev1_metadata) => {
                let has_rev1_metadata = !lacks_rev1_metadata;
                debug!(
                    has_rev1_metadata,
                    legacy_reclaim_needed = legacy_reclaim_needed(has_rev1_metadata),
                    "startup rev1 metadata check finished"
                );
                has_rev1_metadata
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "startup rev1 metadata check failed; skipping legacy persisted-plan recovery"
                );
                debug!("startup recovery maintenance finished");
                return Ok(());
            }
        };

        if legacy_reclaim_needed(has_rev1_metadata) {
            debug!("legacy persisted-plan startup recovery started");
            match self.recover_persisted_plans(pm).await {
                Ok(()) => {
                    debug!("legacy persisted-plan startup recovery finished");
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "legacy persisted-plan startup recovery failed"
                    );
                }
            }
        } else {
            debug!("legacy persisted-plan startup recovery skipped");
        }

        debug!("startup recovery maintenance finished");
        Ok(())
    }
}
