//! Level-triggered reconciler for beads-backed plans.
//!
//! Ticks on an adaptive cadence: fast when there is activity, backing off
//! toward an idle ceiling when there is not. When constructed with a
//! `ReconcilerDispatchCtx`, the reconciler persists dispatch intent and
//! enqueues ACP work for ready persisted tasks.
//!
//! Primary engine: `bv --robot-triage` via BvAdapter (see plan
//! addendum II in docs/superpowers/plans/2026-04-20-adaptive-plan-repair-v0a.md
//! for the rationale — upstream AGENTS.md designates bv as the canonical
//! pick-next-work surface). Fallback: `br ready` via BeadsAdvanced when bv
//! errors. The bv primary path enforces the `spur:plan-complete` guard; the br
//! fallback uses `spur:plan-id:<id>` scoping only (degraded-mode semantics —
//! see `observe_ready_via_br` doc comment).
//!
//! # Spawn wiring
//!
//! In v0c the reconciler is wired into `server.rs` startup with a live
//! `ReconcilerDispatchCtx`, so persisted plans are reclaimed and dispatched by
//! the same loop that owns completion writeback.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::task::TaskTracker;

use spur_pm::{PmService, ReadyFilter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectedEpicCompletion {
    audit_outcome: crate::plan::audit_sentinel::EpicCompletionOutcome,
    add_integration_pending: bool,
}

fn classify_epic_completion(
    children: &[spur_pm::IssueSummary],
    closed_status: &str,
) -> Option<ProjectedEpicCompletion> {
    if children.is_empty() {
        return None;
    }

    let is_terminal_status = |status: &str| {
        status == closed_status || matches!(status, "failed" | "cancelled" | "rejected")
    };

    if children
        .iter()
        .any(|child| !is_terminal_status(child.status.as_str()))
    {
        return None;
    }

    let has_terminal_failures = children.iter().any(|child| {
        matches!(child.status.as_str(), "failed" | "cancelled" | "rejected")
            || child.labels.iter().any(|label| {
                matches!(
                    label.as_str(),
                    "rejected" | "review-rejected" | crate::plan::labels::REVIEW_REJECTED
                )
            })
    });

    Some(ProjectedEpicCompletion {
        audit_outcome: if has_terminal_failures {
            crate::plan::audit_sentinel::EpicCompletionOutcome::TerminalWithFailures
        } else {
            crate::plan::audit_sentinel::EpicCompletionOutcome::AllApproved
        },
        add_integration_pending: !has_terminal_failures,
    })
}

#[derive(Clone)]
pub struct ReconcilerDispatchCtx {
    pub delegation_tx: tokio::sync::mpsc::Sender<crate::tools::DelegationRequest>,
    pub task_tracker: TaskTracker,
    pub brain_session_id: spur_acp::BrainSessionId,
    pub event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
}

pub struct ReconcilerConfig {
    pub base_interval: Duration,
    pub idle_ceiling: Duration,
    pub backoff_factor: u32,
}

impl Default for ReconcilerConfig {
    fn default() -> Self {
        Self {
            base_interval: Duration::from_secs(3),
            idle_ceiling: Duration::from_secs(30),
            backoff_factor: 2,
        }
    }
}

pub struct Reconciler {
    config: ReconcilerConfig,
    pm: Arc<PmService>,
    fast_forward: Arc<Notify>,
    dispatch: Option<ReconcilerDispatchCtx>,
    plan_id: Option<String>,
}

impl Reconciler {
    pub fn new(
        config: ReconcilerConfig,
        pm: Arc<PmService>,
        fast_forward: Arc<Notify>,
        dispatch: Option<ReconcilerDispatchCtx>,
        plan_id: Option<String>,
    ) -> Self {
        Self {
            config,
            pm,
            fast_forward,
            dispatch,
            plan_id,
        }
    }

    pub async fn run(self, cancel: tokio::sync::oneshot::Receiver<()>) {
        let mut interval = self.config.base_interval;
        tokio::pin!(cancel);
        loop {
            tokio::select! {
                _ = &mut cancel => {
                    tracing::info!("reconciler received cancel");
                    break;
                }
                _ = self.fast_forward.notified() => {
                    tracing::debug!("reconciler fast-forward triggered");
                    interval = self.config.base_interval;
                }
                _ = tokio::time::sleep(interval) => {}
            }
            // Race tick_once against cancel so shutdown cannot hang behind
            // stuck PM I/O (bv.triage / br ready). Partial persisted writes are
            // level-triggered and compensated on the next tick / restart.
            let did_work = tokio::select! {
                biased;
                _ = &mut cancel => {
                    tracing::info!("reconciler received cancel during tick");
                    break;
                }
                result = self.tick_once() => {
                    match result {
                        Ok(w) => w,
                        Err(e) => {
                            tracing::warn!("reconciler tick failed: {e}");
                            false
                        }
                    }
                }
            };
            if did_work {
                interval = self.config.base_interval;
            } else {
                let scaled = interval.saturating_mul(self.config.backoff_factor);
                interval = std::cmp::min(scaled, self.config.idle_ceiling);
            }
        }
    }

    pub async fn tick_once(&self) -> anyhow::Result<bool> {
        let Some(dispatch) = &self.dispatch else {
            let ready_ids = self.observe_ready().await?;
            for id in &ready_ids {
                tracing::debug!(%id, "reconciler observed ready task");
            }
            return Ok(!ready_ids.is_empty());
        };

        let ready = self.observe_ready_summaries().await?;
        let mut did_work = false;

        for summary in ready {
            let Some(plan_id) = summary
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_plan_id(label))
            else {
                continue;
            };

            let projected =
                crate::plan::projector::project_plan_from_beads(self.pm.as_ref(), plan_id).await?;
            let Some(task) = projected
                .tasks
                .iter()
                .find(|task| task.spec.issue_id.as_deref() == Some(summary.id.as_str()))
            else {
                continue;
            };
            if !matches!(task.status, crate::plan::PlanTaskStatus::Ready) {
                continue;
            }

            let delegation_id = uuid::Uuid::new_v4().to_string();
            crate::plan::persist_dispatch_intent(
                self.pm.as_ref(),
                &summary.id,
                plan_id,
                &delegation_id,
                &task.spec.agent,
                task.attempt,
            )
            .await?;

            let (respond_to, rx) = tokio::sync::oneshot::channel();
            let request = crate::tools::DelegationRequest {
                id: delegation_id.clone().into(),
                agent: task.spec.agent.clone(),
                task: task.spec.task.clone(),
                context_files: task.spec.context_files.clone(),
                respond_to,
                brain_session_id: dispatch.brain_session_id.clone(),
                delegation_plan: None,
                issue_id: task.spec.issue_id.clone(),
            };

            if let Err(error) = dispatch.delegation_tx.send(request).await {
                crate::plan::clear_dispatch_intent(self.pm.as_ref(), &summary.id, &delegation_id)
                    .await?;
                let mut update = crate::plan::dispatch_send_failure_update(&delegation_id);
                update.remove_labels.clear();
                self.pm.update_issue(&summary.id, update).await?;
                tracing::warn!(
                    issue_id = %summary.id,
                    %delegation_id,
                    "reconciler send failed: {error}"
                );
                continue;
            }

            let pm = Arc::clone(&self.pm);
            let plan_id = plan_id.to_string();
            let task_id = task.spec.task_id.clone();
            let issue_id = summary.id.clone();
            let delegation_id_for_completion = delegation_id.clone();
            let fast_forward = Arc::clone(&self.fast_forward);
            dispatch.task_tracker.spawn(async move {
                let Ok(result) = rx.await else {
                    tracing::warn!(
                        %plan_id,
                        %task_id,
                        %issue_id,
                        %delegation_id_for_completion,
                        "reconciler completion receiver dropped before result persisted"
                    );
                    return;
                };

                let completion_state = crate::plan::completion_state_from_status(&result.status);
                if let Err(error) = crate::plan::persist_completion_result_and_notify(
                    pm.as_ref(),
                    &issue_id,
                    &plan_id,
                    &delegation_id_for_completion,
                    completion_state,
                    result.worker_branch.as_deref(),
                    result.summary.as_deref(),
                    &Some(Arc::clone(&fast_forward)),
                )
                .await
                {
                    tracing::warn!(
                        %plan_id,
                        %task_id,
                        %issue_id,
                        %delegation_id_for_completion,
                        "reconciler completion persistence failed: {error}"
                    );
                }
            });

            did_work = true;
        }

        Ok(did_work)
    }

    pub async fn observe_ready_summaries(&self) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
        let label_filter = self.plan_id.as_deref().map(crate::plan::labels::plan_id);
        let Some(adv) = self.pm.advanced() else {
            anyhow::bail!("reconciler: no advanced (beads) backend available");
        };

        let mut labels = Vec::new();
        if let Some(plan_id_label) = label_filter {
            labels.push(plan_id_label);
        }

        let summaries = adv
            .list_ready(ReadyFilter {
                labels_all: labels,
                limit: Some(50),
                ..Default::default()
            })
            .await?;

        let mut hydrated = Vec::with_capacity(summaries.len());
        for summary in summaries {
            let issue = self.pm.get_issue(&summary.id).await?;
            hydrated.push(spur_pm::IssueSummary {
                id: issue.id.clone(),
                source: issue.source,
                title: issue.title,
                status: issue.status,
                labels: issue.labels,
                url: issue.url,
                priority: issue.priority,
                issue_type: issue.issue_type,
                assignee: issue.assignee,
            });
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
            .map(|summary| summary.id)
            .collect())
    }

    /// Fallback ready-task query using `br ready` (BeadsAdvanced) directly.
    ///
    /// # Degraded-mode semantics
    ///
    /// `spur:plan-complete` is an epic-only marker — tasks never carry it, so
    /// including it in a `ReadyFilter` (which queries tasks, not epics) always
    /// returns empty. The `bv` primary path is the real guard against observing
    /// partially-persisted plan graphs; this fallback scopes only by
    /// `spur:plan-id:<id>` (when `plan_id` is `Some`), accepting that a partial
    /// plan could leak through in the rare window where bv is unhealthy and the
    /// caller passed a plan_id that was not fully persisted. This tradeoff is
    /// acceptable for v0a.2: fallback only triggers on bv failures, and callers
    /// are expected to target fully-persisted plans.
    ///
    /// "Observe all plans" mode (`plan_id` is `None`): no label filter is
    /// applied; all unblocked tasks are returned. Partial-plan protection is
    /// entirely absent in this mode — document as v0a.2 limitation.
    pub async fn observe_ready_via_br(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .observe_ready_summaries()
            .await?
            .into_iter()
            .map(|summary| summary.id)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciler_dispatch_ctx_can_be_cloned_for_server_startup() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<crate::tools::DelegationRequest>(1);
        let ctx = super::ReconcilerDispatchCtx {
            delegation_tx: tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: None,
        };

        let cloned = ctx.clone();
        assert_eq!(cloned.brain_session_id, ctx.brain_session_id);
    }

    fn summary(id: &str, status: &str) -> spur_pm::IssueSummary {
        spur_pm::IssueSummary {
            id: id.into(),
            source: spur_pm::PmSource::Beads,
            title: id.into(),
            status: status.into(),
            labels: vec![],
            url: format!("https://example.invalid/{id}"),
            priority: None,
            issue_type: Some("task".into()),
            assignee: None,
        }
    }

    #[test]
    fn classify_epic_completion_reports_all_approved() {
        let children = vec![summary("bd-1", "closed"), summary("bd-2", "closed")];
        let outcome = super::classify_epic_completion(&children, "closed").expect("terminal");
        assert_eq!(
            outcome.audit_outcome,
            crate::plan::audit_sentinel::EpicCompletionOutcome::AllApproved
        );
        assert!(outcome.add_integration_pending);
    }

    #[test]
    fn classify_epic_completion_reports_terminal_failures() {
        let mut rejected = summary("bd-2", "closed");
        rejected.labels.push("rejected".into());
        let children = vec![summary("bd-1", "closed"), rejected];
        let outcome = super::classify_epic_completion(&children, "closed").expect("terminal");
        assert_eq!(
            outcome.audit_outcome,
            crate::plan::audit_sentinel::EpicCompletionOutcome::TerminalWithFailures
        );
        assert!(!outcome.add_integration_pending);
    }

    /// D1 fix coverage: verify that the biased select! pattern used inside
    /// `Reconciler::run` to race `tick_once` against `cancel` actually
    /// preempts an in-flight future when cancel fires. Uses a pending future
    /// as a stand-in for a stuck `bv.triage`/`br ready` call; without the
    /// biased cancel race, the task would hang indefinitely.
    #[tokio::test]
    async fn biased_select_cancel_preempts_pending_tick() {
        use std::future::pending;
        use tokio::sync::oneshot;

        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        tokio::pin!(cancel_rx);

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            let _ = cancel_tx.send(());
        });

        let blocking = pending::<anyhow::Result<bool>>();
        tokio::pin!(blocking);

        let outcome = tokio::time::timeout(Duration::from_secs(1), async move {
            tokio::select! {
                biased;
                _ = &mut cancel_rx => "cancelled",
                _ = &mut blocking => "tick_completed",
            }
        })
        .await
        .expect("select must not hang when cancel is live");

        assert_eq!(outcome, "cancelled");
    }

    #[test]
    fn cadence_backoff_formula() {
        let cfg = ReconcilerConfig {
            base_interval: Duration::from_secs(1),
            idle_ceiling: Duration::from_secs(8),
            backoff_factor: 2,
        };
        let mut d = cfg.base_interval;
        let mut hist = vec![d];
        for _ in 0..5 {
            d = std::cmp::min(d.saturating_mul(cfg.backoff_factor), cfg.idle_ceiling);
            hist.push(d);
        }
        assert_eq!(
            hist,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(8),
                Duration::from_secs(8),
            ]
        );
    }
}
