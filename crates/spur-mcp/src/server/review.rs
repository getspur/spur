use super::*;

impl McpCallbackServer {
    pub(crate) async fn merge_plan_impl(
        &self,
        plan_id: &str,
    ) -> anyhow::Result<crate::plan::PlanMergeState> {
        let repo_root = match self.repo_root.as_ref() {
            Some(root) => root.clone(),
            None => anyhow::bail!("Repository root not configured; merge_plan is unavailable"),
        };

        let had_cached_entry = self.active_plans.lock().await.contains_key(plan_id);
        let plan_arc = match self.load_or_project_plan(plan_id).await {
            Ok(plan_arc) => plan_arc,
            Err(_) => anyhow::bail!("Unknown plan_id: '{plan_id}'"),
        };

        let (base_snapshot_branch, base_snapshot_oid, tasks, merge_state, epic_id) = {
            let state = plan_arc.lock().await;
            let status = crate::plan::build_plan_status(plan_id, &state);
            let ready = status
                .get("ready_to_merge")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !ready {
                anyhow::bail!("plan '{plan_id}' is not fully approved yet");
            }
            (
                state.base_snapshot_branch.clone(),
                state.base_snapshot_oid.clone(),
                state.tasks.clone(),
                state.merge_state.clone(),
                state.epic_id.clone(),
            )
        };

        if !matches!(merge_state, crate::plan::PlanMergeState::NotStarted) {
            return Ok(merge_state);
        }

        let persisted_bootstrap = if !had_cached_entry {
            match (self.pm_service.as_deref(), epic_id.as_deref()) {
                (Some(pm), Some(epic_id)) => {
                    match read_persisted_plan_bootstrap(
                        pm,
                        self.feature_gate.as_ref(),
                        plan_id,
                        epic_id,
                    )
                    .await
                    {
                        Ok(bootstrap) => Some(bootstrap),
                        Err(error) => anyhow::bail!(error),
                    }
                }
                _ => None,
            }
        } else {
            None
        };

        let base_snapshot_ref = match persisted_bootstrap
            .as_ref()
            .and_then(PersistedPlanBootstrap::preferred_base_ref)
            .map(str::to_string)
            .or(base_snapshot_oid)
            .or(base_snapshot_branch)
        {
            Some(reference) => reference,
            None => anyhow::bail!(
                "plan '{plan_id}' has no captured base snapshot; resubmit the plan before calling merge_plan"
            ),
        };

        let task_specs: Vec<crate::plan::PlanTask> =
            tasks.iter().map(|entry| entry.spec.clone()).collect();
        let order = topological_order(&task_specs).map_err(|e| anyhow::anyhow!(e))?;

        let mut ordered_branches = Vec::with_capacity(order.len());
        for idx in order {
            let entry = &tasks[idx];
            if !matches!(entry.status, crate::plan::PlanTaskStatus::Approved { .. }) {
                anyhow::bail!(
                    "plan '{plan_id}' became non-approved while merge_plan was preparing task '{}'",
                    entry.spec.task_id
                );
            }
            let Some(worker_branch) = entry.worker_branch.clone() else {
                anyhow::bail!(
                    "approved task '{}' has no worker_branch; cannot integrate plan",
                    entry.spec.task_id
                );
            };
            ordered_branches.push((entry.spec.task_id.clone(), worker_branch));
        }

        let merge_branch = format!(
            "spur/plan-merge-{plan_id}-{}",
            uuid::Uuid::new_v4().simple()
        );

        let merge_state = match integrate_plan_branches(
            &repo_root,
            &base_snapshot_ref,
            &merge_branch,
            &ordered_branches,
        )
        .await
        {
            Ok(state) => state,
            Err(error) => crate::plan::PlanMergeState::Failed { error },
        };
        let merged_successfully =
            matches!(merge_state, crate::plan::PlanMergeState::Succeeded { .. });

        {
            let mut state = plan_arc.lock().await;
            state.merge_state = merge_state.clone();
        }

        if merged_successfully {
            if let (Some(pm), Some(epic_id)) = (self.pm_service.as_ref(), epic_id.as_deref()) {
                if let Err(error) = apply_issue_update(
                    pm.as_ref(),
                    epic_id,
                    spur_pm::IssueUpdate {
                        remove_labels: vec![crate::plan::labels::INTEGRATION_PENDING.to_string()],
                        ..Default::default()
                    },
                )
                .await
                {
                    tracing::warn!(
                        plan_id = %plan_id,
                        epic_id = %epic_id,
                        "failed to clear integration-pending on epic after merge: {error}"
                    );
                }
            }
        }

        Ok(merge_state)
    }

    pub(crate) async fn create_pr_impl(&self, params: spur_pm::PrParams) -> anyhow::Result<String> {
        self.pm_service
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No PR service configured"))?
            .create_pr(params)
            .await
    }

    /// Refuse the operation unless the current brain owns the epic for `plan_id`.
    ///
    /// Returns `Ok(())` only when (a) PM service is unavailable (in-memory-only
    /// paths have no durable epic to gate on, so we stay permissive) or
    /// (b) the epic exists and `classify_owner` resolves to `OwnedByCurrent`.
    /// Any other state (`OwnedByOther`, `Ambiguous`, `Unowned`,
    /// missing/duplicate epic, lookup failure) yields `Err((code, message))`
    /// for the caller to wrap into a `JsonRpcResponse` with its own request id.
    ///
    /// Mirrors the gating shape used at the top of `handle_resume_plan`.
    /// Unlike `handle_resume_plan`, we do NOT auto-claim on `Unowned` here:
    /// these endpoints are mid-lifecycle operations, not entry points, and
    /// auto-claiming would mask bugs where a plan reaches review/merge with
    /// no recorded owner.
    pub(crate) async fn preview_task_base_impl(
        &self,
        input: crate::tool_schemas::PreviewTaskBaseInput,
    ) -> anyhow::Result<crate::tool_schemas::PreviewTaskBaseOutput> {
        let repo_root = self
            .repo_root
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Repository root not configured"))?;
        let plan_arc = self
            .load_or_project_plan(&input.plan_id)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        crate::plan::preview::preview_overlay(&plan_arc, &input.plan_id, &input.task_id, &repo_root)
            .await
    }

    pub(crate) async fn run_clobber_detector_for_review(
        &self,
        plan_arc: &Arc<tokio::sync::Mutex<crate::plan::PlanState>>,
        task_id: &str,
    ) -> Result<ClobberReviewReport, String> {
        let Some(repo_root) = self.repo_root.as_deref() else {
            return Ok(ClobberReviewReport::default());
        };

        let (issue_id, worker_branch, prior_candidates) = {
            let state = plan_arc.lock().await;
            let Some(current) = state
                .tasks
                .iter()
                .find(|entry| entry.spec.task_id == task_id)
            else {
                return Ok(ClobberReviewReport::default());
            };
            let Some(worker_branch) = current
                .worker_branch
                .clone()
                .filter(|branch| !branch.is_empty())
            else {
                return Ok(ClobberReviewReport::default());
            };
            let prior_candidates = state
                .tasks
                .iter()
                .filter(|entry| entry.spec.task_id != task_id)
                .filter(|entry| {
                    matches!(entry.status, crate::plan::PlanTaskStatus::Approved { .. })
                })
                .filter_map(|entry| {
                    let branch_name = entry.worker_branch.clone()?;
                    Some((entry.spec.task_id.clone(), branch_name))
                })
                .collect::<Vec<_>>();
            (
                current.spec.issue_id.clone(),
                worker_branch,
                prior_candidates,
            )
        };

        if prior_candidates.is_empty() {
            return Ok(ClobberReviewReport::default());
        }

        let mut warnings = Vec::new();
        let mut priors = Vec::with_capacity(prior_candidates.len());
        for (prior_task_id, branch_name) in prior_candidates {
            let tip_oid = match run_git_capture(
                repo_root,
                None,
                &["rev-parse", branch_name.as_str()],
            )
            .await
            {
                Ok(oid) => oid,
                Err(error) => {
                    tracing::warn!(
                        task_id = %prior_task_id,
                        branch = %branch_name,
                        "review_task clobber detector skipped prior: {error}"
                    );
                    warnings.push(format!(
                        "clobber detector skipped prior task '{prior_task_id}': {error}"
                    ));
                    continue;
                }
            };
            priors.push(crate::plan::clobber_detector::PriorTip {
                task_id: prior_task_id,
                branch_name,
                tip_oid,
            });
        }

        if priors.is_empty() {
            return Ok(ClobberReviewReport {
                signals: Vec::new(),
                warnings,
            });
        }

        let report = crate::plan::clobber_detector::run(repo_root, &worker_branch, &priors);
        if report.signals.is_empty() {
            return Ok(ClobberReviewReport {
                signals: Vec::new(),
                warnings,
            });
        }

        if let (Some(pm), Some(issue_id)) = (self.pm_service.as_deref(), issue_id.as_deref()) {
            if let Err(error) = require_feature(
                FeatureKey::PM_PRO_BEADS_ADVANCED,
                self.feature_gate.as_ref(),
            ) {
                let message = feature_error_message(error);
                tracing::warn!(
                    issue_id = %issue_id,
                    "review_task clobber detector signal persistence skipped: {message}"
                );
                warnings.push(format!(
                    "clobber detector could not write signal comments for issue '{issue_id}': {message}"
                ));
                return Ok(ClobberReviewReport {
                    signals: report.signals,
                    warnings,
                });
            }
            if let Some(advanced) = pm.advanced() {
                for signal in &report.signals {
                    if let Err(error) = advanced
                        .add_comment(issue_id, &crate::plan::signals::encode_comment(signal))
                        .await
                    {
                        tracing::warn!(
                            issue_id = %issue_id,
                            signal_id = %signal.signal_id(),
                            "review_task clobber detector signal comment failed: {error}"
                        );
                        warnings.push(format!(
                            "clobber detector failed to write signal comment for issue '{issue_id}': {error}"
                        ));
                    }
                }

                let mut add_labels = report
                    .signals
                    .iter()
                    .map(|signal| crate::plan::labels::signal_kind(signal.kind_label()))
                    .collect::<Vec<_>>();
                add_labels.sort();
                add_labels.dedup();
                if let Err(error) = pm
                    .update_issue(
                        issue_id,
                        IssueUpdate {
                            add_labels,
                            ..Default::default()
                        },
                    )
                    .await
                {
                    tracing::warn!(
                        issue_id = %issue_id,
                        "review_task clobber detector signal label failed: {error}"
                    );
                    warnings.push(format!(
                        "clobber detector failed to add signal label for issue '{issue_id}': {error}"
                    ));
                }
            } else {
                warnings.push(format!(
                    "clobber detector could not write signal comments for issue '{issue_id}': beads advanced API unavailable"
                ));
            }
        }

        Ok(ClobberReviewReport {
            signals: report.signals,
            warnings,
        })
    }
}
