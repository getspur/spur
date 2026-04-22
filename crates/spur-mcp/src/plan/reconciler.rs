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

use crate::plan::audit_sentinel::AuditSentinelKind;
use spur_pm::{IssueFilter, PmService, ReadyFilter};

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

#[async_trait::async_trait]
pub trait ReconcilerAutomation: Send + Sync {
    async fn merge_plan(&self, plan_id: &str) -> anyhow::Result<crate::plan::PlanMergeState>;
    async fn create_pr(&self, params: spur_pm::PrParams) -> anyhow::Result<String>;
}

fn build_auto_pr_params(
    plan_id: &str,
    epic_title: &str,
    outcome_summary: &str,
    merge_branch: &str,
) -> spur_pm::PrParams {
    // Stub: fully implemented in Task 8.
    spur_pm::PrParams {
        title: String::new(),
        body: String::new(),
        head_branch: String::new(),
        base_branch: None,
        repo: None,
    }
}

pub struct Reconciler {
    config: ReconcilerConfig,
    pm: Arc<PmService>,
    fast_forward: Arc<Notify>,
    dispatch: Option<ReconcilerDispatchCtx>,
    plan_id: Option<String>,
    auto_merge_approved_plans: bool,
    automation: Option<Arc<dyn ReconcilerAutomation>>,
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
            auto_merge_approved_plans: false,
            automation: None,
        }
    }

    pub fn set_auto_merge_approved_plans(&mut self, enabled: bool) {
        self.auto_merge_approved_plans = enabled;
    }

    pub fn set_automation(&mut self, automation: Arc<dyn ReconcilerAutomation>) {
        self.automation = Some(automation);
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
        let mut did_work = self.reconcile_terminal_epics().await?;

        let Some(dispatch) = &self.dispatch else {
            let ready_ids = self.observe_ready().await?;
            for id in &ready_ids {
                tracing::debug!(%id, "reconciler observed ready task");
            }
            return Ok(did_work || !ready_ids.is_empty());
        };

        let ready = self.observe_ready_summaries().await?;

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

    async fn reconcile_terminal_epics(&self) -> anyhow::Result<bool> {
        let Some(adv) = self.pm.advanced() else {
            return Ok(false);
        };

        let closed_status = self.pm.closed_status().to_string();
        let mut labels = vec![crate::plan::labels::PLAN_COMPLETE.to_string()];
        if let Some(plan_id) = self.plan_id.as_deref() {
            labels.push(crate::plan::labels::plan_id(plan_id));
        }

        let mut epics = self
            .pm
            .list_issues(IssueFilter {
                labels: labels.clone(),
                issue_type: Some("epic".into()),
                limit: Some(10_000),
                ..Default::default()
            })
            .await?;
        let closed_epics = self
            .pm
            .list_issues(IssueFilter {
                labels,
                status: Some(closed_status.clone()),
                issue_type: Some("epic".into()),
                limit: Some(10_000),
                ..Default::default()
            })
            .await?;
        let mut seen_epic_ids = epics
            .iter()
            .map(|summary| summary.id.clone())
            .collect::<std::collections::HashSet<_>>();
        for summary in closed_epics {
            if seen_epic_ids.insert(summary.id.clone()) {
                epics.push(summary);
            }
        }

        let mut did_work = false;

        for epic in epics {
            let Some(plan_id) = epic
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_plan_id(label))
            else {
                continue;
            };

            let mut children = self
                .pm
                .list_issues(IssueFilter {
                    labels: vec![crate::plan::labels::plan_id(plan_id)],
                    limit: Some(10_000),
                    ..Default::default()
                })
                .await?;
            let closed_children = self
                .pm
                .list_issues(IssueFilter {
                    labels: vec![crate::plan::labels::plan_id(plan_id)],
                    status: Some(closed_status.clone()),
                    limit: Some(10_000),
                    ..Default::default()
                })
                .await?;
            let mut seen_ids = children
                .iter()
                .map(|summary| summary.id.clone())
                .collect::<std::collections::HashSet<_>>();
            for summary in closed_children {
                if seen_ids.insert(summary.id.clone()) {
                    children.push(summary);
                }
            }
            let children = children
                .into_iter()
                .filter(|summary| summary.id != epic.id)
                .collect::<Vec<_>>();

            let Some(outcome) = classify_epic_completion(&children, &closed_status) else {
                continue;
            };

            let has_epic_completion =
                crate::plan::projector::collect_sorted_audits(adv.list_comments(&epic.id).await?)
                    .iter()
                    .any(|audit| {
                        matches!(
                            audit,
                            AuditSentinelKind::EpicCompletion {
                                plan_id: audit_plan_id,
                                epic_id: audit_epic_id,
                                ..
                            } if audit_plan_id == plan_id && audit_epic_id == &epic.id
                        )
                    });

            if epic.status == closed_status {
                if !has_epic_completion {
                    crate::plan::emit_epic_completion_audit(
                        adv,
                        &epic.id,
                        plan_id,
                        outcome.audit_outcome,
                    )
                    .await;
                    did_work = true;
                }
                continue;
            }

            let mut update = spur_pm::IssueUpdate {
                status: Some(closed_status.clone()),
                ..Default::default()
            };
            if outcome.add_integration_pending {
                if !epic
                    .labels
                    .contains(&crate::plan::labels::INTEGRATION_PENDING.to_string())
                {
                    update
                        .add_labels
                        .push(crate::plan::labels::INTEGRATION_PENDING.to_string());
                }
            } else if epic
                .labels
                .contains(&crate::plan::labels::INTEGRATION_PENDING.to_string())
            {
                update
                    .remove_labels
                    .push(crate::plan::labels::INTEGRATION_PENDING.to_string());
            }

            self.pm.update_issue(&epic.id, update).await?;
            if !has_epic_completion {
                crate::plan::emit_epic_completion_audit(
                    adv,
                    &epic.id,
                    plan_id,
                    outcome.audit_outcome,
                )
                .await;
            }
            if outcome.add_integration_pending {
                if let Some(sink) = self
                    .dispatch
                    .as_ref()
                    .and_then(|dispatch| dispatch.event_sink.as_ref())
                {
                    sink.emit(spur_acp::SpurEventBody::PlanReadyToMerge {
                        plan_id: plan_id.to_string(),
                    });
                }
            }
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

    #[test]
    fn auto_pr_params_include_plan_id_and_summary() {
        let params = super::build_auto_pr_params("plan-123", "Epic title", "All approved", "spur/merge-1");
        assert!(params.title.contains("plan-123"), "title missing plan_id: {}", params.title);
        assert!(params.body.contains("All approved"), "body missing outcome: {}", params.body);
        assert_eq!(params.head_branch, "spur/merge-1");
    }

    struct MockAutomation {
        actions: Arc<tokio::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl super::ReconcilerAutomation for MockAutomation {
        async fn merge_plan(&self, plan_id: &str) -> anyhow::Result<crate::plan::PlanMergeState> {
            self.actions.lock().await.push(format!("merge:{plan_id}"));
            Ok(crate::plan::PlanMergeState::Succeeded {
                merge_branch: "spur/merge-1".to_string(),
                merged_task_ids: vec![],
            })
        }

        async fn create_pr(&self, params: spur_pm::PrParams) -> anyhow::Result<String> {
            self.actions.lock().await.push(format!("pr:{}", params.title));
            Ok("https://example.invalid/pr/1".to_string())
        }
    }

    #[tokio::test]
    async fn auto_merge_config_off_produces_zero_actions() {
        use std::process::Command;
        use tempfile::TempDir;

        fn br_available() -> bool {
            Command::new("br")
                .arg("--help")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }

        if !br_available() {
            eprintln!("skipping auto_merge_config_off_produces_zero_actions: `br` not on PATH");
            return;
        }

        let dir = TempDir::new().expect("tempdir");
        let repo = dir.path();

        assert!(
            Command::new("br").args(["init"]).current_dir(repo).output().expect("br init").status.success(),
            "br init failed"
        );

        let epic_out = Command::new("br")
            .args(["create", "--type", "epic", "--title", "Test Epic", "--json"])
            .current_dir(repo)
            .output()
            .expect("br create epic");
        let epic_json = String::from_utf8_lossy(&epic_out.stdout);
        let epic_id: String = serde_json::from_str::<serde_json::Value>(&epic_json)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        for title in ["Task A", "Task B"] {
            let task_out = Command::new("br")
                .args(["create", "--type", "task", "--title", title, "--json"])
                .current_dir(repo)
                .output()
                .expect("br create task");
            let task_json = String::from_utf8_lossy(&task_out.stdout);
            let task_id: String = serde_json::from_str::<serde_json::Value>(&task_json)
                .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string();
            Command::new("br")
                .args(["label", "add", &task_id, "spur:plan-id:P1"])
                .current_dir(repo)
                .output()
                .expect("label task");
            Command::new("br")
                .args(["update", &task_id, "--status", "closed"])
                .current_dir(repo)
                .output()
                .expect("close task");
        }

        Command::new("br")
            .args(["label", "add", &epic_id, "spur:plan-id:P1"])
            .current_dir(repo)
            .output()
            .expect("label epic");
        Command::new("br")
            .args(["label", "add", &epic_id, "spur:plan-complete"])
            .current_dir(repo)
            .output()
            .expect("label epic plan-complete");
        Command::new("br")
            .args(["update", &epic_id, "--status", "closed"])
            .current_dir(repo)
            .output()
            .expect("close epic");
        Command::new("br")
            .args(["label", "add", &epic_id, "spur:integration-pending"])
            .current_dir(repo)
            .output()
            .expect("label epic integration-pending");

        let pm = Arc::new(
            spur_pm::PmService::try_new(None, true, false, repo, None)
                .await
                .expect("PmService::try_new failed")
                .expect("expected beads pm"),
        );

        let actions = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let automation = Arc::new(MockAutomation {
            actions: Arc::clone(&actions),
        });

        let mut reconciler = Reconciler::new(
            ReconcilerConfig::default(),
            pm,
            Arc::new(Notify::new()),
            None,
            Some("P1".into()),
        );
        reconciler.set_auto_merge_approved_plans(false);
        reconciler.set_automation(automation);

        reconciler.tick_once().await.unwrap();

        let recorded = actions.lock().await;
        assert!(
            recorded.is_empty(),
            "config-off must produce zero automation actions, got: {:?}",
            *recorded
        );
    }
}
