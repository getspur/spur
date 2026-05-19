use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::plan::outcomes::SkipReason;
use spur_pm::{IssueFilter, ReadyFilter};

impl super::Reconciler {
    pub async fn observe_ready_summaries(&self) -> anyhow::Result<Vec<super::HydratedReady>> {
        crate::server::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            self.feature_gate.as_ref(),
        )
        .map_err(|error| anyhow::anyhow!(crate::server::feature_error_message(error)))?;
        let Some(adv) = self.pm.advanced() else {
            anyhow::bail!("reconciler: no advanced (beads) backend available");
        };

        let summaries = if let Some(plan_id) = self.plan_id.as_deref() {
            let mut plan_activation_cache = HashMap::new();
            if matches!(
                self.plan_allows_dispatch(plan_id, &mut plan_activation_cache)
                    .await?,
                super::PlanDispatchState::EpicNotOpen { .. }
            ) {
                self.outcomes.lock().await.drop_plan(plan_id);
                return Ok(Vec::new());
            }
            let summaries = adv
                .list_ready(ReadyFilter {
                    labels_all: vec![crate::plan::labels::plan_id(plan_id)],
                    // Limit is per plan; global reconcilers enumerate plans first.
                    limit: Some(1000),
                    ..Default::default()
                })
                .await?;
            self.record_tick_plans_enumerated(1).await;
            summaries
        } else {
            let epics = self
                .pm
                .list_issues(IssueFilter {
                    labels: vec![crate::plan::labels::PLAN_COMPLETE.to_string()],
                    issue_type: Some("epic".into()),
                    limit: Some(10_000),
                    ..Default::default()
                })
                .await?;
            let mut summaries = Vec::new();
            let mut seen_summary_ids = HashSet::new();
            let mut seen_plan_ids = HashSet::new();
            for epic in epics {
                let Some(plan_id) = epic
                    .labels
                    .iter()
                    .find_map(|label| crate::plan::labels::parse_plan_id(label))
                else {
                    self.record_skipped(None, &epic.id, SkipReason::MissingPlanId)
                        .await;
                    continue;
                };
                if !seen_plan_ids.insert(plan_id.to_string()) {
                    continue;
                }
                let plan_summaries = adv
                    .list_ready(ReadyFilter {
                        labels_all: vec![crate::plan::labels::plan_id(plan_id)],
                        // Limit is per plan; global reconcilers enumerate plans first.
                        limit: Some(1000),
                        ..Default::default()
                    })
                    .await?;
                if plan_summaries.is_empty() {
                    self.record_no_ready(Some(plan_id)).await;
                    continue;
                }
                for summary in plan_summaries {
                    if seen_summary_ids.insert(summary.id.clone()) {
                        summaries.push(summary);
                    }
                }
            }
            self.record_tick_plans_enumerated(seen_plan_ids.len()).await;
            summaries
        };
        let had_ready_summaries = !summaries.is_empty();

        let mut hydrated = Vec::with_capacity(summaries.len());
        let mut seen_issue_ids = HashSet::new();
        let mut plan_activation_cache = HashMap::new();
        for summary in summaries {
            let issue = match self.pm.get_issue(&summary.id).await {
                Ok(issue) => issue,
                Err(error) => {
                    let plan_id = summary
                        .labels
                        .iter()
                        .find_map(|label| crate::plan::labels::parse_plan_id(label));
                    tracing::warn!(
                        issue_id = %summary.id,
                        plan_id = plan_id.unwrap_or(""),
                        "reconciler skipping ready summary after get_issue failed: {error}"
                    );
                    self.record_skipped(
                        plan_id,
                        &summary.id,
                        SkipReason::HydrationGetIssueFailed {
                            error: error.to_string(),
                        },
                    )
                    .await;
                    continue;
                }
            };
            match issue.issue_type.as_deref() {
                Some("task") => {
                    if let Some(plan_id) = issue
                        .labels
                        .iter()
                        .find_map(|label| crate::plan::labels::parse_plan_id(label))
                        .map(str::to_string)
                    {
                        let dispatch_state = match self
                            .plan_allows_dispatch(&plan_id, &mut plan_activation_cache)
                            .await
                        {
                            Ok(state) => state,
                            Err(error) => {
                                let task_id =
                                    super::task_id_from_labels_or_issue(&issue.labels, &issue.id);
                                tracing::warn!(
                                    %plan_id,
                                    %task_id,
                                    issue_id = %issue.id,
                                    "reconciler skipping ready task after plan_allows_dispatch failed: {error}"
                                );
                                self.record_skipped(
                                    Some(&plan_id),
                                    &task_id,
                                    SkipReason::PlanAllowsDispatchFailed {
                                        error: error.to_string(),
                                    },
                                )
                                .await;
                                continue;
                            }
                        };
                        if let Some(reason) = dispatch_state.skip_reason() {
                            let task_id =
                                super::task_id_from_labels_or_issue(&issue.labels, &issue.id);
                            self.record_skipped(Some(&plan_id), &task_id, reason).await;
                            continue;
                        }
                    }
                    if seen_issue_ids.insert(issue.id.clone()) {
                        hydrated.push(super::HydratedReady {
                            summary: super::issue_to_summary(issue),
                            plan_state: None,
                        });
                    }
                }
                Some("epic") => {
                    let Some(plan_id) = issue
                        .labels
                        .iter()
                        .find_map(|label| crate::plan::labels::parse_plan_id(label))
                        .map(str::to_string)
                    else {
                        self.record_skipped(None, &issue.id, SkipReason::MissingPlanId)
                            .await;
                        continue;
                    };
                    let dispatch_state = match self
                        .plan_allows_dispatch(&plan_id, &mut plan_activation_cache)
                        .await
                    {
                        Ok(state) => state,
                        Err(error) => {
                            tracing::warn!(
                                %plan_id,
                                issue_id = %issue.id,
                                "reconciler skipping ready epic after plan_allows_dispatch failed: {error}"
                            );
                            self.record_skipped(
                                Some(&plan_id),
                                &issue.id,
                                SkipReason::PlanAllowsDispatchFailed {
                                    error: error.to_string(),
                                },
                            )
                            .await;
                            continue;
                        }
                    };
                    if let Some(reason) = dispatch_state.skip_reason() {
                        self.record_skipped(Some(&plan_id), &issue.id, reason).await;
                        continue;
                    }
                    let projected = match self.project_plan_from_beads(&plan_id).await {
                        Ok(projected) => Arc::new(projected),
                        Err(error) => {
                            tracing::warn!(
                                %plan_id,
                                issue_id = %issue.id,
                                "reconciler skipping ready epic after plan projection failed: {error}"
                            );
                            self.record_skipped(
                                Some(&plan_id),
                                &issue.id,
                                SkipReason::ProjectorFailed {
                                    error: error.to_string(),
                                },
                            )
                            .await;
                            continue;
                        }
                    };
                    for task in &projected.tasks {
                        if !matches!(task.status, crate::plan::PlanTaskStatus::Ready) {
                            let blocked_by = super::unresolved_blocker_issue_ids(&projected, task);
                            self.record_skipped(
                                Some(&plan_id),
                                &task.spec.task_id,
                                SkipReason::TaskStatusNotReady { blocked_by },
                            )
                            .await;
                            continue;
                        }
                        let Some(issue_id) = task.spec.issue_id.as_ref() else {
                            self.record_skipped(
                                Some(&plan_id),
                                &task.spec.task_id,
                                SkipReason::MissingIssueId,
                            )
                            .await;
                            continue;
                        };
                        if !seen_issue_ids.insert(issue_id.clone()) {
                            self.record_skipped(
                                Some(&plan_id),
                                &task.spec.task_id,
                                SkipReason::DuplicateIssueId,
                            )
                            .await;
                            continue;
                        }
                        let task_issue = match self.pm.get_issue(issue_id).await {
                            Ok(task_issue) => task_issue,
                            Err(error) => {
                                tracing::warn!(
                                    %plan_id,
                                    task_id = %task.spec.task_id,
                                    %issue_id,
                                    "reconciler skipping ready task after get_issue failed: {error}"
                                );
                                self.record_skipped(
                                    Some(&plan_id),
                                    &task.spec.task_id,
                                    SkipReason::HydrationGetIssueFailed {
                                        error: error.to_string(),
                                    },
                                )
                                .await;
                                continue;
                            }
                        };
                        hydrated.push(super::HydratedReady {
                            summary: super::issue_to_summary(task_issue),
                            plan_state: Some(Arc::clone(&projected)),
                        });
                    }
                }
                _ => {
                    let plan_id = issue
                        .labels
                        .iter()
                        .find_map(|label| crate::plan::labels::parse_plan_id(label));
                    let task_id = super::task_id_from_labels_or_issue(&issue.labels, &issue.id);
                    self.record_skipped(
                        plan_id,
                        &task_id,
                        SkipReason::UnsupportedReadyIssueType {
                            issue_type: issue.issue_type.clone(),
                        },
                    )
                    .await;
                }
            }
        }

        if !had_ready_summaries {
            self.record_no_ready(self.plan_id.as_deref()).await;
        }

        Ok(hydrated)
    }

    /// Returns the IDs of ready tasks under the configured plan filter,
    /// preserving the labels from the beads ready summaries.
    pub async fn observe_ready(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .observe_ready_summaries()
            .await?
            .into_iter()
            .map(|ready| ready.summary.id)
            .collect())
    }

    /// Fallback ready-task query using `br ready` (BeadsAdvanced) directly.
    ///
    /// # Degraded-mode semantics
    ///
    /// `spur:plan-complete` and `spur:plan-pending` are epic-only markers, so
    /// they cannot be included in the task-level `ReadyFilter`. The fallback
    /// scopes by `spur:plan-id:<id>` and then hydrates each candidate task's
    /// plan epic before returning it. Tasks for incomplete or pending plans are
    /// suppressed.
    pub async fn observe_ready_via_br(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .observe_ready_summaries()
            .await?
            .into_iter()
            .map(|ready| ready.summary.id)
            .collect())
    }
}
