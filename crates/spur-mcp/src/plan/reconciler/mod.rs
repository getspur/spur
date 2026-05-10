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
//! errors. Ready observation enforces the epic activation guard: a plan must
//! carry `spur:plan-complete` and must not carry `spur:plan-pending`.
//!
//! # Spawn wiring
//!
//! In v0c the reconciler is wired into `server.rs` startup with a live
//! `ReconcilerDispatchCtx`, so persisted plans are reclaimed and dispatched by
//! the same loop that owns completion writeback.
//!
//! # Submodule layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | `leases` | Sweep expired dispatch leases and reclaim orphaned dispatches. |
//! | `conflict` | Detect and persist overlay conflicts (setup and predispatch). |
//! | `ready` | Observe ready tasks from beads backend with plan-activation guards. |
//! | `base_spec` | Git helpers and base-spec construction for dispatch overlays. |
//! | `guards` | Plan activation guards (`plan_allows_dispatch`, `plan_allows_writes`). |
//! | `terminal` | Reconcile terminal epics: completion audits, auto-merge, auto-PR. |
//! | `tests` | Unit tests, mocks, and shared fixtures for the reconciler. |

mod leases;

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::Notify;
use tokio_util::task::TaskTracker;

use crate::plan::outcomes::{
    DispatchOutcome, NoReadyReason, OutcomeLogDecision, OutcomeStore, SkipReason, StuckTask,
};

mod ready;

mod conflict;

mod base_spec;

use base_spec::plan_dispatch_base_spec;

mod guards;

pub use terminal::ReconcilerAutomation;
mod terminal;

pub(crate) fn beads_journal_path(repo_root: &std::path::Path) -> std::path::PathBuf {
    repo_root.join(".beads").join("journal")
}

pub(crate) async fn monitor_journal_appends(path: std::path::PathBuf, notify: Arc<Notify>) {
    let mut last_len = match tokio::fs::metadata(&path).await {
        Ok(meta) => meta.len(),
        Err(error) => {
            tracing::debug!(
                %error,
                ?path,
                "journal metadata unavailable at startup; disabling poller"
            );
            return;
        }
    };
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let next_len = match tokio::fs::metadata(&path).await {
            Ok(meta) => meta.len(),
            Err(error) => {
                tracing::debug!(%error, ?path, "journal metadata unavailable; retrying");
                continue;
            }
        };
        if next_len > last_len {
            last_len = next_len;
            notify.notify_one();
        } else {
            last_len = next_len;
        }
    }
}

fn issue_to_summary(issue: spur_pm::Issue) -> spur_pm::IssueSummary {
    spur_pm::IssueSummary {
        id: issue.id.clone(),
        source: issue.source,
        title: issue.title,
        status: issue.status,
        labels: issue.labels,
        url: issue.url,
        priority: issue.priority,
        issue_type: issue.issue_type,
        assignee: issue.assignee,
    }
}

fn task_id_from_labels_or_issue(labels: &[String], issue_id: &str) -> String {
    labels
        .iter()
        .find_map(|label| crate::plan::labels::parse_plan_task_id(label))
        .unwrap_or_else(|| issue_id.to_string())
}

fn unresolved_blocker_issue_ids(
    projected: &crate::plan::PlanState,
    task: &crate::plan::PlanTaskEntry,
) -> Vec<String> {
    let by_task_id = projected
        .tasks
        .iter()
        .map(|entry| (entry.spec.task_id.as_str(), entry))
        .collect::<HashMap<_, _>>();
    let by_issue_id = projected
        .tasks
        .iter()
        .filter_map(|entry| {
            entry
                .spec
                .issue_id
                .as_deref()
                .map(|issue_id| (issue_id, entry))
        })
        .collect::<HashMap<_, _>>();

    let mut blockers = task
        .spec
        .depends_on
        .iter()
        .filter_map(|dependency| {
            let dependency_entry = by_task_id
                .get(dependency.as_str())
                .or_else(|| by_issue_id.get(dependency.as_str()));
            match dependency_entry {
                Some(entry)
                    if matches!(
                        entry.status,
                        crate::plan::PlanTaskStatus::Approved { .. }
                            | crate::plan::PlanTaskStatus::Cancelled { .. }
                            | crate::plan::PlanTaskStatus::Superseded { .. }
                    ) =>
                {
                    None
                }
                Some(entry) => Some(
                    entry
                        .spec
                        .issue_id
                        .clone()
                        .unwrap_or_else(|| entry.spec.task_id.clone()),
                ),
                None => Some(dependency.clone()),
            }
        })
        .collect::<Vec<_>>();
    blockers.sort();
    blockers.dedup();
    blockers
}

async fn projected_plan_for_ready<Fut>(
    hydrated_plan_state: Option<Arc<crate::plan::PlanState>>,
    project: Fut,
) -> anyhow::Result<Arc<crate::plan::PlanState>>
where
    Fut: Future<Output = anyhow::Result<crate::plan::PlanState>>,
{
    if let Some(plan_state) = hydrated_plan_state {
        return Ok(plan_state);
    }

    Ok(Arc::new(project.await?))
}

async fn prune_projected_terminal_task_outcomes(
    outcomes: &Arc<tokio::sync::Mutex<OutcomeStore>>,
    plan_id: &str,
    tasks: &[crate::plan::PlanTaskEntry],
) {
    let terminal_task_ids = tasks
        .iter()
        .filter(|task| task.status.is_terminal())
        .map(|task| task.spec.task_id.clone())
        .collect::<Vec<_>>();
    if terminal_task_ids.is_empty() {
        return;
    }

    let mut outcomes = outcomes.lock().await;
    for task_id in terminal_task_ids {
        outcomes.prune_task(plan_id, &task_id);
    }
}

#[derive(Clone)]
pub struct ReconcilerDispatchCtx {
    pub delegation_tx: tokio::sync::mpsc::Sender<crate::tools::DelegationRequest>,
    pub task_tracker: TaskTracker,
    pub brain_session_id: spur_acp::BrainSessionId,
    pub event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
    pub materializer: Arc<crate::outcome_materializer::OutcomeMaterializer>,
    pub continuation_ctx: Arc<crate::server::DetachedContinuationCtx>,
}

#[derive(Debug, Clone)]
pub struct HydratedReady {
    pub summary: spur_pm::IssueSummary,
    pub plan_state: Option<Arc<crate::plan::PlanState>>,
}

/// Strategy for the pre-dispatch overlay dry-run. Production uses `Real`;
/// tests inject deterministic outcomes without touching git.
#[derive(Debug, Clone, Default)]
pub enum PreviewStrategy {
    #[default]
    Real,
    AlwaysClean,
    AlwaysConflict {
        dep_task_id: String,
        files: Vec<String>,
    },
}

pub struct ReconcilerConfig {
    pub base_interval: Duration,
    pub idle_ceiling: Duration,
    pub backoff_factor: u32,
    pub dispatch_lease_duration: Duration,
    pub repo_root: PathBuf,
    pub predispatch_preview: PreviewStrategy,
}

impl Default for ReconcilerConfig {
    fn default() -> Self {
        Self {
            base_interval: Duration::from_secs(3),
            idle_ceiling: Duration::from_secs(30),
            backoff_factor: 2,
            dispatch_lease_duration: Duration::from_secs(600),
            repo_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            predispatch_preview: PreviewStrategy::Real,
        }
    }
}

pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanDispatchState {
    Allowed,
    PlanMissingCompleteEpic,
    EpicNotOpen { epic_id: String },
    PlanHasPendingEpic { epic_id: String },
    PlanOwnedByAnotherBrain { epic_id: String, owner: String },
}

impl PlanDispatchState {
    fn skip_reason(&self) -> Option<SkipReason> {
        match self {
            Self::Allowed => None,
            Self::PlanMissingCompleteEpic => Some(SkipReason::PlanMissingCompleteEpic),
            Self::EpicNotOpen { .. } => Some(SkipReason::EpicNotOpen),
            Self::PlanHasPendingEpic { .. } => Some(SkipReason::PlanHasPendingEpic),
            Self::PlanOwnedByAnotherBrain { owner, .. } => {
                Some(SkipReason::PlanOwnedByAnotherBrain {
                    owner: owner.clone(),
                })
            }
        }
    }
}

pub struct Reconciler {
    pub(super) config: ReconcilerConfig,
    pub(super) pm: Arc<dyn crate::plan::PmLike>,
    pub(super) fast_forward: Arc<Notify>,
    pub(super) dispatch: Option<ReconcilerDispatchCtx>,
    pub(super) plan_id: Option<String>,
    pub(super) auto_merge_approved_plans: bool,
    pub(super) automation: Option<Arc<dyn ReconcilerAutomation>>,
    pub(super) journal_wake: Option<Arc<Notify>>,
    pub(super) feature_gate: Arc<spur_license::FeatureGate>,
    pub(super) outcomes: Arc<tokio::sync::Mutex<OutcomeStore>>,
    pub(super) clock: Arc<dyn Clock>,
}

impl Reconciler {
    pub fn new(
        config: ReconcilerConfig,
        pm: Arc<spur_pm::PmService>,
        fast_forward: Arc<Notify>,
        dispatch: Option<ReconcilerDispatchCtx>,
        plan_id: Option<String>,
        feature_gate: Arc<spur_license::FeatureGate>,
    ) -> Self {
        Self::new_with_pm_like(
            config,
            pm as Arc<dyn crate::plan::PmLike>,
            fast_forward,
            dispatch,
            plan_id,
            feature_gate,
        )
    }

    pub fn new_with_pm_like(
        config: ReconcilerConfig,
        pm: Arc<dyn crate::plan::PmLike>,
        fast_forward: Arc<Notify>,
        dispatch: Option<ReconcilerDispatchCtx>,
        plan_id: Option<String>,
        feature_gate: Arc<spur_license::FeatureGate>,
    ) -> Self {
        Self {
            config,
            pm,
            fast_forward,
            dispatch,
            plan_id,
            auto_merge_approved_plans: false,
            automation: None,
            journal_wake: None,
            feature_gate,
            outcomes: Arc::new(tokio::sync::Mutex::new(OutcomeStore::default())),
            clock: Arc::new(SystemClock),
        }
    }

    pub fn set_outcomes(&mut self, outcomes: Arc<tokio::sync::Mutex<OutcomeStore>>) {
        self.outcomes = outcomes;
    }

    pub fn outcomes(&self) -> Arc<tokio::sync::Mutex<OutcomeStore>> {
        Arc::clone(&self.outcomes)
    }

    pub fn set_clock(&mut self, clock: Arc<dyn Clock>) {
        self.clock = clock;
    }

    pub fn set_journal_wake(&mut self, notify: Arc<Notify>) {
        self.journal_wake = Some(notify);
    }

    pub fn set_auto_merge_approved_plans(&mut self, enabled: bool) {
        self.auto_merge_approved_plans = enabled;
    }

    pub fn set_automation(&mut self, automation: Arc<dyn ReconcilerAutomation>) {
        self.automation = Some(automation);
    }

    fn now(&self) -> SystemTime {
        self.clock.now()
    }

    async fn mark_tick(&self) {
        let now = self.now();
        self.outcomes.lock().await.mark_tick(now);
    }

    async fn record_outcome(&self, plan_id: Option<&str>, outcome: DispatchOutcome) {
        self.outcomes.lock().await.record_outcome(plan_id, outcome);
    }

    async fn record_no_ready(&self, plan_id: Option<&str>) {
        let now = self.now();
        let outcome = DispatchOutcome::NoReadyTasks {
            plan_id: plan_id.unwrap_or("*").to_string(),
            reason: NoReadyReason::NoMatchingRows,
            timestamp: now,
        };
        self.record_outcome(plan_id, outcome).await;
    }

    async fn record_no_dispatch_context(&self, ready_count: usize) {
        let now = self.now();
        self.outcomes.lock().await.record_no_dispatch_context(
            self.plan_id.as_deref(),
            ready_count,
            now,
        );
    }

    async fn record_dispatched(
        &self,
        plan_id: &str,
        task_id: &str,
        agent: &str,
        delegation_id: &str,
        agent_fallback: bool,
    ) {
        let now = self.now();
        self.outcomes.lock().await.record_dispatched(
            plan_id,
            task_id,
            agent,
            delegation_id,
            agent_fallback,
            now,
        );
    }

    async fn record_skipped(&self, plan_id: Option<&str>, task_id: &str, reason: SkipReason) {
        let now = self.now();
        let reason_for_log = reason.clone();
        let decision = self
            .outcomes
            .lock()
            .await
            .record_skipped(plan_id, task_id, reason, now);
        self.log_skip_decision(plan_id, task_id, &reason_for_log, decision);
    }

    fn log_skip_decision(
        &self,
        plan_id: Option<&str>,
        task_id: &str,
        reason: &SkipReason,
        decision: OutcomeLogDecision,
    ) {
        if decision.state_changed {
            tracing::info!(
                plan_id = plan_id.unwrap_or(""),
                %task_id,
                ?reason,
                "reconciler skip reason changed"
            );
        }
        if let Some(StuckTask { stuck_since, .. }) = decision.stuck_warn {
            tracing::warn!(
                plan_id = plan_id.unwrap_or(""),
                %task_id,
                ?reason,
                ?stuck_since,
                "reconciler task stuck on skip reason"
            );
        }
    }

    async fn project_plan_from_beads(
        &self,
        plan_id: &str,
    ) -> anyhow::Result<crate::plan::PlanState> {
        let projected = crate::plan::projector::project_plan_from_beads(
            self.pm.as_ref(),
            plan_id,
            self.feature_gate.as_ref(),
        )
        .await?;
        prune_projected_terminal_task_outcomes(&self.outcomes, plan_id, &projected.tasks).await;
        Ok(projected)
    }

    async fn active_plan_handle(
        &self,
        plan_id: &str,
    ) -> Option<Arc<tokio::sync::Mutex<crate::plan::PlanState>>> {
        match self.project_plan_from_beads(plan_id).await {
            Ok(projected) => Some(Arc::new(tokio::sync::Mutex::new(projected))),
            Err(error) => {
                tracing::warn!(
                    %plan_id,
                    "predispatch preview: failed to project active plan handle: {error}"
                );
                None
            }
        }
    }

    async fn emit_snapshot_for_plan(&self, plan_id: &str) {
        let Some(sink) = self
            .dispatch
            .as_ref()
            .and_then(|dispatch| dispatch.event_sink.as_deref())
        else {
            return;
        };

        match self.project_plan_from_beads(plan_id).await {
            Ok(projected) => crate::plan::snapshot::emit_plan_snapshot(Some(sink), &projected),
            Err(error) => tracing::warn!(%plan_id, "failed to project plan snapshot: {error}"),
        }
    }

    pub async fn run(self, cancel: tokio::sync::oneshot::Receiver<()>) {
        let mut interval = self.config.base_interval;
        let journal_wake = self.journal_wake.clone();
        tokio::pin!(cancel);
        loop {
            let journal_fut = async {
                if let Some(ref n) = journal_wake {
                    n.notified().await;
                } else {
                    std::future::pending().await
                }
            };
            tokio::pin!(journal_fut);
            tokio::select! {
                _ = &mut cancel => {
                    tracing::info!("reconciler received cancel");
                    break;
                }
                _ = self.fast_forward.notified() => {
                    tracing::debug!("reconciler fast-forward triggered");
                    interval = self.config.base_interval;
                }
                _ = &mut journal_fut => {
                    tracing::debug!("reconciler journal append triggered");
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
        self.mark_tick().await;
        let mut did_work = self.reconcile_terminal_epics().await?;

        let Some(dispatch) = &self.dispatch else {
            let ready_ids = self.observe_ready().await?;
            for id in &ready_ids {
                tracing::debug!(%id, "reconciler observed ready task");
            }
            if !ready_ids.is_empty() {
                self.record_no_dispatch_context(ready_ids.len()).await;
            }
            return Ok(did_work || !ready_ids.is_empty());
        };

        did_work |= self.sweep_expired_dispatch_leases(dispatch).await?;

        let ready = self.observe_ready_summaries().await?;

        for hydrated in ready {
            let summary = hydrated.summary;
            let Some(plan_id) = summary
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_plan_id(label))
            else {
                self.record_skipped(None, &summary.id, SkipReason::MissingPlanId)
                    .await;
                continue;
            };

            let projected = match projected_plan_for_ready(
                hydrated.plan_state,
                self.project_plan_from_beads(plan_id),
            )
            .await
            {
                Ok(projected) => projected,
                Err(error) => {
                    tracing::warn!(
                        issue_id = %summary.id,
                        %plan_id,
                        "reconciler skipping ready summary after plan projection failed: {error}"
                    );
                    self.record_skipped(
                        Some(plan_id),
                        &summary.id,
                        SkipReason::ProjectorFailed {
                            error: error.to_string(),
                        },
                    )
                    .await;
                    continue;
                }
            };
            let Some(task) = projected
                .tasks
                .iter()
                .find(|task| task.spec.issue_id.as_deref() == Some(summary.id.as_str()))
            else {
                self.record_skipped(
                    Some(plan_id),
                    &summary.id,
                    SkipReason::TaskMissingFromProjection,
                )
                .await;
                continue;
            };
            if !matches!(task.status, crate::plan::PlanTaskStatus::Ready) {
                let blocked_by = unresolved_blocker_issue_ids(&projected, task);
                self.record_skipped(
                    Some(plan_id),
                    &task.spec.task_id,
                    SkipReason::TaskStatusNotReady { blocked_by },
                )
                .await;
                continue;
            }

            let delegation_id = crate::plan::labels::mint_delegation_id();
            let task_attempt = task.attempt;
            let agent_fallback = !summary
                .labels
                .iter()
                .any(|label| crate::plan::labels::parse_agent(label).is_some());
            let base_spec = match plan_dispatch_base_spec(
                &projected,
                &task.spec.task_id,
                &self.config.repo_root,
            )
            .await
            {
                Ok(base_spec) => base_spec,
                Err(error) => {
                    tracing::warn!(
                        issue_id = %summary.id,
                        %plan_id,
                        task_id = %task.spec.task_id,
                        "reconciler skipping ready task after base spec build failed: {error}"
                    );
                    self.record_skipped(
                        Some(plan_id),
                        &task.spec.task_id,
                        SkipReason::BaseSpecBuildFailed {
                            error: error.to_string(),
                        },
                    )
                    .await;
                    continue;
                }
            };
            let has_overlays = matches!(
                &base_spec,
                crate::tools::BaseSpec::WithOverlay { overlays, .. } if !overlays.is_empty()
            );
            let preview_outcome = if has_overlays {
                match &self.config.predispatch_preview {
                    PreviewStrategy::AlwaysClean => None,
                    PreviewStrategy::AlwaysConflict { dep_task_id, files } => {
                        Some(crate::tool_schemas::PreviewConflict {
                            dep_task_id: dep_task_id.clone(),
                            files: files.clone(),
                        })
                    }
                    PreviewStrategy::Real => match self.active_plan_handle(plan_id).await {
                        Some(plan_arc) => {
                            match crate::plan::preview::preview_overlay(
                                &plan_arc,
                                plan_id,
                                &task.spec.task_id,
                                &self.config.repo_root,
                            )
                            .await
                            {
                                Ok(output) => output.conflict,
                                Err(error) => {
                                    tracing::warn!(
                                        %plan_id,
                                        task_id = %task.spec.task_id,
                                        "predispatch preview: helper errored, falling through to live dispatch: {error}"
                                    );
                                    None
                                }
                            }
                        }
                        None => {
                            tracing::warn!(
                                %plan_id,
                                task_id = %task.spec.task_id,
                                "predispatch preview: no active plan handle; skipping check"
                            );
                            None
                        }
                    },
                }
            } else {
                None
            };
            if let Some(conflict) = preview_outcome {
                tracing::info!(
                    %plan_id,
                    task_id = %task.spec.task_id,
                    dep_task_id = %conflict.dep_task_id,
                    files = ?conflict.files,
                    "predispatch preview: overlay conflict predicted; blocking without worker spawn"
                );
                if let Err(error) = self
                    .transition_to_blocked_on_setup_conflict(
                        plan_id,
                        &task.spec.task_id,
                        &conflict.dep_task_id,
                        &conflict.files,
                    )
                    .await
                {
                    tracing::warn!(
                        %plan_id,
                        task_id = %task.spec.task_id,
                        "predispatch preview: failed to persist setup-conflict transition: {error}"
                    );
                    self.record_skipped(
                        Some(plan_id),
                        &task.spec.task_id,
                        SkipReason::PersistError {
                            msg: error.to_string(),
                        },
                    )
                    .await;
                    continue;
                }
                self.record_skipped(
                    Some(plan_id),
                    &task.spec.task_id,
                    SkipReason::PredispatchOverlayConflict {
                        dep_task_id: conflict.dep_task_id,
                        files: conflict.files,
                    },
                )
                .await;
                continue;
            }
            if let Err(error) = crate::plan::persist_dispatch_intent(
                self.pm.as_ref(),
                &summary.id,
                self.feature_gate.as_ref(),
                plan_id,
                &delegation_id,
                &task.spec.agent,
                task_attempt,
                self.config.dispatch_lease_duration,
            )
            .await
            {
                tracing::warn!(
                    issue_id = %summary.id,
                    %plan_id,
                    task_id = %task.spec.task_id,
                    "reconciler skipping ready task after persist_dispatch_intent failed: {error}"
                );
                self.record_skipped(
                    Some(plan_id),
                    &task.spec.task_id,
                    SkipReason::PersistDispatchIntentFailed {
                        error: error.to_string(),
                    },
                )
                .await;
                continue;
            }

            let (respond_to, rx) = tokio::sync::oneshot::channel();
            let (dispatched_base_oid_tx, dispatched_base_oid_rx) =
                tokio::sync::watch::channel(None);
            let task_text = crate::plan::build_dispatch_task_text(task);
            let request = crate::tools::DelegationRequest {
                id: delegation_id.clone().into(),
                agent: task.spec.agent.clone(),
                task: task_text,
                context_files: task.spec.context_files.clone(),
                respond_to,
                brain_session_id: dispatch.brain_session_id.clone(),
                delegation_plan: None,
                issue_id: task.spec.issue_id.clone(),
                base: Some(base_spec),
                dispatched_base_oid_tx: Some(dispatched_base_oid_tx),
                attempt_tracker: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
                    task_attempt,
                )),
                enable_worker_mcp: None,
            };

            // INV-S3 audit: dispatch intent is durable in beads before this
            // in-process request becomes observable by the orchestrator.
            if let Err(error) = dispatch.delegation_tx.send(request).await {
                self.record_skipped(
                    Some(plan_id),
                    &task.spec.task_id,
                    SkipReason::DispatchSendFailed {
                        msg: error.to_string(),
                    },
                )
                .await;
                if let Err(e) = crate::plan::clear_dispatch_intent(
                    self.pm.as_ref(),
                    &summary.id,
                    &delegation_id,
                )
                .await
                {
                    tracing::warn!(
                        target: "spur.dispatch_intent_cleanup",
                        %plan_id,
                        task_id = %task.spec.task_id,
                        %delegation_id,
                        "dispatch cleanup failed: {e}"
                    );
                }
                let mut update = crate::plan::dispatch_send_failure_update(&delegation_id, &[]);
                update.remove_labels.clear();
                if let Err(e) = self.pm.update_issue(&summary.id, update).await {
                    tracing::warn!(
                        target: "spur.dispatch_intent_cleanup",
                        %plan_id,
                        task_id = %task.spec.task_id,
                        %delegation_id,
                        "dispatch cleanup failed: {e}"
                    );
                }
                tracing::warn!(
                    issue_id = %summary.id,
                    %delegation_id,
                    "reconciler send failed: {error}"
                );
                continue;
            }

            self.record_dispatched(
                plan_id,
                &task.spec.task_id,
                &task.spec.agent,
                &delegation_id,
                agent_fallback,
            )
            .await;
            self.emit_snapshot_for_plan(plan_id).await;

            let pm = Arc::clone(&self.pm);
            let plan_id = plan_id.to_string();
            let task_id = task.spec.task_id.clone();
            let issue_id = summary.id.clone();
            let delegation_id_for_completion = delegation_id.clone();
            let fast_forward = Arc::clone(&self.fast_forward);
            let event_sink = dispatch.event_sink.clone();
            let brain_session_id = dispatch.brain_session_id.clone();
            let materializer = Arc::clone(&dispatch.materializer);
            let continuation_ctx = Arc::clone(&dispatch.continuation_ctx);
            let feature_gate = Arc::clone(&self.feature_gate);
            let outcomes = Arc::clone(&self.outcomes);
            dispatch.task_tracker.spawn(async move {
                let result = match rx.await {
                    Ok(result) => result,
                    Err(_) => {
                        tracing::warn!(
                            %plan_id,
                            %task_id,
                            %issue_id,
                            %delegation_id_for_completion,
                            "reconciler completion receiver dropped before result persisted"
                        );
                        let error = "orchestrator disconnected".to_string();
                        spur_acp::DelegationResult {
                            status: spur_acp::DelegationStatus::Failed {
                                error: error.clone(),
                            },
                            diff: None,
                            diff_summary: None,
                            summary: Some(error),
                            estimated_cost_usd: 0.0,
                            worker_branch: None,
                            artifact: None,
                        }
                    }
                };

                if let Some((source_task_id, files)) =
                    conflict::setup_overlay_conflict(&result.status)
                {
                    if let Err(error) = conflict::persist_setup_overlay_conflict(
                        pm.as_ref(),
                        &issue_id,
                        feature_gate.as_ref(),
                        &plan_id,
                        &delegation_id_for_completion,
                        source_task_id,
                        files,
                    )
                    .await
                    {
                        tracing::warn!(
                            %plan_id,
                            %task_id,
                            %issue_id,
                            %delegation_id_for_completion,
                            "reconciler setup conflict persistence failed: {error}"
                        );
                    } else {
                        fast_forward.notify_one();
                    }

                    match crate::plan::projector::project_plan_from_beads(
                        pm.as_ref(),
                        &plan_id,
                        feature_gate.as_ref(),
                    )
                    .await
                    {
                        Ok(projected) => {
                            if let Some(sink) = event_sink.as_deref() {
                                crate::plan::snapshot::emit_plan_snapshot(Some(sink), &projected);
                            }
                        }
                        Err(error) => tracing::warn!(
                            %plan_id,
                            %task_id,
                            "failed to project plan snapshot after setup conflict: {error}"
                        ),
                    }
                    return;
                }

                // INV-S3 audit: the worker result and base-OID watch value are
                // consumed only by this completion writer; events,
                // continuations, and projected state are emitted after the
                // completion audit/update below succeeds.
                let dispatched_base_oid = dispatched_base_oid_rx.borrow().clone();

                let deferred = match crate::plan::persist_worker_completion_and_notify(
                    pm.as_ref(),
                    &issue_id,
                    feature_gate.as_ref(),
                    &plan_id,
                    &delegation_id_for_completion,
                    &Some(Arc::clone(&fast_forward)),
                    &result,
                    &brain_session_id,
                    task_attempt,
                    &materializer,
                    dispatched_base_oid,
                    Some(&task_id),
                )
                .await
                {
                    Ok(payload) => payload,
                    Err(error) => {
                        tracing::warn!(
                            %plan_id,
                            %task_id,
                            %issue_id,
                            %delegation_id_for_completion,
                            "reconciler completion persistence failed: {error}"
                        );
                        None
                    }
                };

                match crate::plan::projector::project_plan_from_beads(
                    pm.as_ref(),
                    &plan_id,
                    feature_gate.as_ref(),
                )
                .await
                {
                    Ok(projected) => {
                        prune_projected_terminal_task_outcomes(
                            &outcomes,
                            &plan_id,
                            &projected.tasks,
                        )
                        .await;
                        if let Some(sink) = event_sink.as_deref() {
                            crate::plan::snapshot::emit_plan_snapshot(Some(sink), &projected);
                        }
                    }
                    Err(error) => tracing::warn!(
                        %plan_id,
                        %task_id,
                        "failed to project plan snapshot after completion: {error}"
                    ),
                }
                if let Some(deferred) = deferred {
                    deferred
                        .deliver(event_sink.as_deref(), continuation_ctx.as_ref())
                        .await;
                }
            });

            did_work = true;
        }

        Ok(did_work)
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests;
