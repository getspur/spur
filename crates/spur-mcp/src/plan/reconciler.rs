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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::Notify;
use tokio_util::task::TaskTracker;

use crate::plan::audit_sentinel::AuditSentinelKind;
use crate::plan::outcomes::{
    DispatchOutcome, NoReadyReason, OutcomeLogDecision, OutcomeStore, SkipReason, StuckTask,
};
use spur_pm::{IssueFilter, PmService, ReadyFilter};

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

fn is_hex_oid(spec: &str) -> bool {
    spec.len() == 40 && spec.chars().all(|ch| ch.is_ascii_hexdigit())
}

async fn git_rev_parse(repo_root: &Path, spec: &str) -> anyhow::Result<String> {
    let output = tokio::process::Command::new("git")
        .args(["rev-parse", spec])
        .current_dir(repo_root)
        .output()
        .await
        .map_err(|error| anyhow::anyhow!("failed to execute git rev-parse {spec}: {error}"))?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    if is_hex_oid(spec) {
        return Ok(spec.to_string());
    }

    anyhow::bail!(
        "git rev-parse {spec} failed (exit {}): {}",
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

async fn plan_dispatch_base_spec(
    plan_state: &crate::plan::PlanState,
    task_id: &str,
    repo_root: &Path,
) -> anyhow::Result<crate::tools::BaseSpec> {
    let dep_closure = plan_state.approved_dep_closure(task_id);
    let mut overlays = Vec::with_capacity(dep_closure.len());

    for dep in dep_closure {
        let base_oid = dep.dispatched_base_oid.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "approved dependency {} is missing dispatched_base_oid",
                dep.spec.task_id
            )
        })?;
        let worker_branch = dep.worker_branch.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "approved dependency {} is missing worker_branch",
                dep.spec.task_id
            )
        })?;
        let tip_oid = git_rev_parse(repo_root, worker_branch).await?;
        overlays.push(crate::tools::OverlayCommit {
            source_task_id: dep.spec.task_id.clone(),
            base_oid,
            tip_oid,
        });
    }

    Ok(crate::tools::BaseSpec::WithOverlay {
        base: crate::tools::BaseTarget::Branch {
            name: plan_state
                .base_snapshot_branch
                .clone()
                .unwrap_or_else(|| "HEAD".to_string()),
        },
        overlays,
    })
}

fn setup_overlay_conflict(status: &spur_acp::DelegationStatus) -> Option<(&str, &[String])> {
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

async fn persist_setup_overlay_conflict(
    pm: &spur_pm::PmService,
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

fn child_summary_may_have_terminal_projection(
    child: &spur_pm::IssueSummary,
    closed_status: &str,
) -> bool {
    child.status == closed_status || matches!(child.status.as_str(), "failed" | "cancelled")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectedEpicCompletion {
    audit_outcome: crate::plan::audit_sentinel::EpicCompletionOutcome,
    add_integration_pending: bool,
    approved_count: u32,
    rejected_count: u32,
    failed_count: u32,
    cancelled_count: u32,
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

    let mut approved_count = 0u32;
    let mut rejected_count = 0u32;
    let mut failed_count = 0u32;
    let mut cancelled_count = 0u32;

    for child in children {
        let rejected = child.status == "rejected"
            || child.labels.iter().any(|label| {
                matches!(
                    label.as_str(),
                    "rejected" | "review-rejected" | crate::plan::labels::REVIEW_REJECTED
                )
            });
        if rejected {
            rejected_count += 1;
            continue;
        }

        match child.status.as_str() {
            "failed" => failed_count += 1,
            "cancelled" => cancelled_count += 1,
            status if status == closed_status => approved_count += 1,
            _ => return None,
        }
    }

    let has_terminal_failures = rejected_count > 0 || failed_count > 0 || cancelled_count > 0;

    Some(ProjectedEpicCompletion {
        audit_outcome: if has_terminal_failures {
            crate::plan::audit_sentinel::EpicCompletionOutcome::TerminalWithFailures
        } else {
            crate::plan::audit_sentinel::EpicCompletionOutcome::AllApproved
        },
        add_integration_pending: !has_terminal_failures,
        approved_count,
        rejected_count,
        failed_count,
        cancelled_count,
    })
}

#[derive(Clone)]
pub struct ReconcilerDispatchCtx {
    pub delegation_tx: tokio::sync::mpsc::Sender<crate::tools::DelegationRequest>,
    pub task_tracker: TaskTracker,
    pub brain_session_id: spur_acp::BrainSessionId,
    pub event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
    pub materializer: Arc<crate::outcome_materializer::OutcomeMaterializer>,
}

pub struct ReconcilerConfig {
    pub base_interval: Duration,
    pub idle_ceiling: Duration,
    pub backoff_factor: u32,
    pub dispatch_lease_duration: Duration,
    pub repo_root: PathBuf,
}

impl Default for ReconcilerConfig {
    fn default() -> Self {
        Self {
            base_interval: Duration::from_secs(3),
            idle_ceiling: Duration::from_secs(30),
            backoff_factor: 2,
            dispatch_lease_duration: Duration::from_secs(600),
            repo_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
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
}

impl PlanDispatchState {
    fn skip_reason(&self) -> Option<SkipReason> {
        match self {
            Self::Allowed => None,
            Self::PlanMissingCompleteEpic => Some(SkipReason::PlanMissingCompleteEpic),
            Self::EpicNotOpen { .. } => Some(SkipReason::EpicNotOpen),
            Self::PlanHasPendingEpic { .. } => Some(SkipReason::PlanHasPendingEpic),
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
    spur_pm::PrParams {
        title: format!("[SPUR] {epic_title} ({plan_id})"),
        body: format!(
            "Auto-created for plan `{plan_id}`.\n\nOutcome: {outcome_summary}\nMerge branch: {merge_branch}"
        ),
        head_branch: merge_branch.to_string(),
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
    journal_wake: Option<Arc<Notify>>,
    feature_gate: Arc<spur_license::FeatureGate>,
    outcomes: Arc<tokio::sync::Mutex<OutcomeStore>>,
    clock: Arc<dyn Clock>,
}

impl Reconciler {
    pub fn new(
        config: ReconcilerConfig,
        pm: Arc<PmService>,
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

        for summary in ready {
            let Some(plan_id) = summary
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_plan_id(label))
            else {
                self.record_skipped(None, &summary.id, SkipReason::MissingPlanId)
                    .await;
                continue;
            };

            let projected = match self.project_plan_from_beads(plan_id).await {
                Ok(projected) => projected,
                Err(error) => {
                    tracing::warn!(
                        issue_id = %summary.id,
                        %plan_id,
                        "reconciler skipping ready summary after plan projection failed: {error}"
                    );
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

            let delegation_id = uuid::Uuid::new_v4().to_string();
            let task_attempt = task.attempt;
            let agent_fallback = !summary
                .labels
                .iter()
                .any(|label| crate::plan::labels::parse_agent(label).is_some());
            let base_spec =
                plan_dispatch_base_spec(&projected, &task.spec.task_id, &self.config.repo_root)
                    .await?;
            crate::plan::persist_dispatch_intent(
                self.pm.as_ref(),
                &summary.id,
                self.feature_gate.as_ref(),
                plan_id,
                &delegation_id,
                &task.spec.agent,
                task_attempt,
                self.config.dispatch_lease_duration,
            )
            .await?;

            let (respond_to, rx) = tokio::sync::oneshot::channel();
            let (dispatched_base_oid_tx, dispatched_base_oid_rx) =
                tokio::sync::watch::channel(None);
            let request = crate::tools::DelegationRequest {
                id: delegation_id.clone().into(),
                agent: task.spec.agent.clone(),
                task: task.spec.task.clone(),
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
            };

            if let Err(error) = dispatch.delegation_tx.send(request).await {
                self.record_skipped(
                    Some(plan_id),
                    &task.spec.task_id,
                    SkipReason::DispatchSendFailed {
                        msg: error.to_string(),
                    },
                )
                .await;
                crate::plan::clear_dispatch_intent(self.pm.as_ref(), &summary.id, &delegation_id)
                    .await?;
                let mut update = crate::plan::dispatch_send_failure_update(&delegation_id, &[]);
                update.remove_labels.clear();
                self.pm.update_issue(&summary.id, update).await?;
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

                if let Some((source_task_id, files)) = setup_overlay_conflict(&result.status) {
                    if let Err(error) = persist_setup_overlay_conflict(
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

                let dispatched_base_oid = dispatched_base_oid_rx.borrow().clone();

                if let Err(error) = crate::plan::persist_worker_completion_and_notify(
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
            });

            did_work = true;
        }

        Ok(did_work)
    }

    async fn sweep_expired_dispatch_leases(
        &self,
        dispatch: &ReconcilerDispatchCtx,
    ) -> anyhow::Result<bool> {
        crate::server::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            self.feature_gate.as_ref(),
        )
        .map_err(|error| anyhow::anyhow!(crate::server::feature_error_message(error)))?;
        let Some(adv) = self.pm.advanced() else {
            return Ok(false);
        };

        let mut summaries_by_id = HashMap::new();
        for status in ["open", "in_progress"] {
            let mut filter_labels = Vec::new();
            if let Some(plan_id) = self.plan_id.as_deref() {
                filter_labels.push(crate::plan::labels::plan_id(plan_id));
            }
            for summary in self
                .pm
                .list_issues(IssueFilter {
                    labels: filter_labels,
                    status: Some(status.to_string()),
                    issue_type: Some("task".to_string()),
                    limit: Some(10_000),
                    ..Default::default()
                })
                .await?
            {
                summaries_by_id.entry(summary.id.clone()).or_insert(summary);
            }
        }

        let now = chrono::Utc::now().timestamp();
        let mut did_work = false;

        for summary in summaries_by_id.into_values() {
            let Some(delegation_id) = summary
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_delegation_id(label))
                .map(str::to_string)
            else {
                continue;
            };
            let Some(expires_at) = summary
                .labels
                .iter()
                .filter_map(|label| crate::plan::labels::parse_lease_expires_at(label))
                .max()
            else {
                let plan_id = summary
                    .labels
                    .iter()
                    .find_map(|label| crate::plan::labels::parse_plan_id(label));
                let task_id = task_id_from_labels_or_issue(&summary.labels, &summary.id);
                self.record_skipped(plan_id, &task_id, SkipReason::MissingDispatchLeaseExpiry)
                    .await;
                continue;
            };
            if now <= expires_at {
                continue;
            }

            let Some(plan_id) = summary
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_plan_id(label))
                .map(str::to_string)
            else {
                tracing::warn!(
                    issue_id = %summary.id,
                    %delegation_id,
                    "expired dispatch lease has no plan id label; skipping"
                );
                continue;
            };
            let task_id = summary
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_plan_task_id(label))
                .unwrap_or_else(|| summary.id.clone());
            let age_secs = now.saturating_sub(expires_at);
            let audits = crate::plan::projector::collect_sorted_audits_for_issue(
                &summary.id,
                adv.list_comments(&summary.id).await?,
            );
            let (attempt, _) = crate::plan::projector::project_attempt_facts(&audits);
            let orphan_reason = format!("dispatch lease expired at {expires_at} (age {age_secs}s)");
            // Match on `delegation_id` only, ignoring `reason`. Safe because
            // delegation IDs are unique per dispatch lifecycle: dispatch mints
            // a fresh UUID and `clear_dispatch_intent` removes the label, so
            // any prior `DispatchOrphanCleared` (e.g. `restart-orphan-cleared`
            // from `resolve_dispatch_orphan`) on this delegation_id makes a
            // duplicate lease-expired audit redundant.
            let orphan_audit_exists = audits.iter().any(|audit| {
                matches!(
                    audit,
                    crate::plan::audit_sentinel::AuditSentinelKind::DispatchOrphanCleared {
                        delegation_id: found_delegation_id,
                        ..
                    } if found_delegation_id == &delegation_id
                )
            });
            if !orphan_audit_exists {
                adv.add_comment(
                    &summary.id,
                    &crate::plan::audit_sentinel::encode_comment(
                        &crate::plan::audit_sentinel::AuditSentinelKind::DispatchOrphanCleared {
                            delegation_id: delegation_id.clone(),
                            reason: orphan_reason.clone(),
                        },
                    ),
                )
                .await?;
            }

            let result = spur_acp::DelegationResult {
                status: spur_acp::DelegationStatus::Failed {
                    error: "dispatch lease expired".to_string(),
                },
                diff: None,
                diff_summary: None,
                summary: Some("dispatch lease expired".to_string()),
                estimated_cost_usd: 0.0,
                worker_branch: None,
                artifact: None,
            };
            crate::plan::persist_system_completion_and_notify(
                self.pm.as_ref(),
                &summary.id,
                self.feature_gate.as_ref(),
                &plan_id,
                &delegation_id,
                crate::plan::audit_sentinel::CompletionState::Failed,
                &Some(Arc::clone(&self.fast_forward)),
                &result,
                &dispatch.brain_session_id,
                attempt,
                &dispatch.materializer,
                None,
            )
            .await?;

            tracing::warn!(
                target: "spur.dispatch_lease",
                %plan_id,
                %task_id,
                issue_id = %summary.id,
                %delegation_id,
                expires_at,
                age_secs,
                "reclaimed expired dispatch lease"
            );
            if let Some(sink) = dispatch.event_sink.as_deref() {
                sink.emit(spur_acp::SpurEventBody::DispatchLeaseExpired {
                    plan_id: plan_id.clone(),
                    task_id: task_id.clone(),
                    issue_id: summary.id.clone(),
                    delegation_id: delegation_id.clone(),
                    expired_at: expires_at,
                    age_secs,
                });
            }
            self.emit_snapshot_for_plan(&plan_id).await;
            did_work = true;
        }

        Ok(did_work)
    }

    async fn reconcile_terminal_epics(&self) -> anyhow::Result<bool> {
        crate::server::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            self.feature_gate.as_ref(),
        )
        .map_err(|error| anyhow::anyhow!(crate::server::feature_error_message(error)))?;
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
                self.record_skipped(None, &epic.id, SkipReason::MissingPlanId)
                    .await;
                continue;
            };

            let mut children = self
                .pm
                .list_issues(IssueFilter {
                    labels: vec![crate::plan::labels::plan_id(plan_id)],
                    issue_type: Some("task".into()),
                    limit: Some(10_000),
                    ..Default::default()
                })
                .await?;
            let closed_children = self
                .pm
                .list_issues(IssueFilter {
                    labels: vec![crate::plan::labels::plan_id(plan_id)],
                    status: Some(closed_status.clone()),
                    issue_type: Some("task".into()),
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

            if children
                .iter()
                .any(|child| child_summary_may_have_terminal_projection(child, &closed_status))
            {
                if let Err(error) = self.project_plan_from_beads(plan_id).await {
                    tracing::warn!(
                        %plan_id,
                        epic_id = %epic.id,
                        "failed to project terminal child statuses for outcome pruning: {error}"
                    );
                }
            }

            let Some(outcome) = classify_epic_completion(&children, &closed_status) else {
                // Intentional no-op: non-terminal epics emit no outcome.
                // Terminal classification is computed elsewhere; absence here is normal.
                continue;
            };

            let mut audits = crate::plan::projector::collect_sorted_audits_for_issue(
                &epic.id,
                adv.list_comments(&epic.id).await?,
            );
            let has_epic_completion = audits.iter().any(|audit| {
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
                self.outcomes.lock().await.drop_plan(plan_id);
                if !outcome.add_integration_pending
                    && epic
                        .labels
                        .contains(&crate::plan::labels::INTEGRATION_PENDING.to_string())
                {
                    self.pm
                        .update_issue(
                            &epic.id,
                            spur_pm::IssueUpdate {
                                remove_labels: vec![
                                    crate::plan::labels::INTEGRATION_PENDING.to_string()
                                ],
                                ..Default::default()
                            },
                        )
                        .await?;
                    did_work = true;
                }
                if !has_epic_completion {
                    let emitted = crate::plan::emit_epic_completion_audit(
                        adv,
                        &epic.id,
                        plan_id,
                        outcome.audit_outcome,
                    )
                    .await;
                    if emitted.is_ok() {
                        audits.push(AuditSentinelKind::EpicCompletion {
                            outcome: outcome.audit_outcome,
                            plan_id: plan_id.to_string(),
                            epic_id: epic.id.clone(),
                        });
                    }
                    if let Some(sink) = self
                        .dispatch
                        .as_ref()
                        .and_then(|dispatch| dispatch.event_sink.as_ref())
                    {
                        sink.emit(spur_acp::SpurEventBody::PlanCompleted {
                            plan_id: plan_id.to_string(),
                            approved: outcome.approved_count,
                            rejected: outcome.rejected_count,
                            failed: outcome.failed_count,
                            cancelled: outcome.cancelled_count,
                        });
                    }
                    did_work = true;
                }

                // v0e: opt-in auto-merge / auto-PR on durable all-approved state.
                if self.auto_merge_approved_plans
                    && outcome.add_integration_pending
                    && epic
                        .labels
                        .contains(&crate::plan::labels::INTEGRATION_PENDING.to_string())
                {
                    if let Some(automation) = self.automation.as_ref() {
                        if let Some(outcome_summary) =
                            crate::plan::projector::epic_completion_outcome_summary(
                                &audits, plan_id, &epic.id,
                            )
                        {
                            let mut merge_succeeded = false;
                            match automation.merge_plan(plan_id).await {
                                Ok(crate::plan::PlanMergeState::Succeeded {
                                    merge_branch, ..
                                }) => {
                                    let params = build_auto_pr_params(
                                        plan_id,
                                        &epic.title,
                                        outcome_summary,
                                        &merge_branch,
                                    );
                                    if let Err(e) = automation.create_pr(params).await {
                                        tracing::warn!(%plan_id, "auto-merge PR creation failed: {e}");
                                    } else {
                                        merge_succeeded = true;
                                    }
                                }
                                Ok(crate::plan::PlanMergeState::Conflict {
                                    conflict_task_id,
                                    ..
                                }) => {
                                    tracing::warn!(%plan_id, %conflict_task_id, "auto-merge detected conflict");
                                }
                                Ok(crate::plan::PlanMergeState::Failed { error }) => {
                                    tracing::warn!(%plan_id, "auto-merge failed: {error}");
                                }
                                Ok(crate::plan::PlanMergeState::NotStarted) => {
                                    tracing::warn!(%plan_id, "auto-merge returned NotStarted unexpectedly");
                                }
                                Err(e) => {
                                    tracing::warn!(%plan_id, "auto-merge error: {e}");
                                }
                            }
                            if merge_succeeded {
                                if let Err(e) = self
                                    .pm
                                    .update_issue(
                                        &epic.id,
                                        spur_pm::IssueUpdate {
                                            remove_labels: vec![
                                                crate::plan::labels::INTEGRATION_PENDING
                                                    .to_string(),
                                            ],
                                            ..Default::default()
                                        },
                                    )
                                    .await
                                {
                                    tracing::warn!(
                                        %plan_id,
                                        epic_id = %epic.id,
                                        "failed to remove integration-pending label: {e}"
                                    );
                                }
                            }
                            did_work = true;
                        }
                    }
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
            self.outcomes.lock().await.drop_plan(plan_id);
            if !has_epic_completion {
                if let Err(error) = crate::plan::emit_epic_completion_audit(
                    adv,
                    &epic.id,
                    plan_id,
                    outcome.audit_outcome,
                )
                .await
                {
                    tracing::warn!(
                        plan_id = %plan_id,
                        epic_id = %epic.id,
                        "epic completion audit emission failed on close: {error}"
                    );
                }
            }
            if !has_epic_completion {
                if let Some(sink) = self
                    .dispatch
                    .as_ref()
                    .and_then(|dispatch| dispatch.event_sink.as_ref())
                {
                    sink.emit(spur_acp::SpurEventBody::PlanCompleted {
                        plan_id: plan_id.to_string(),
                        approved: outcome.approved_count,
                        rejected: outcome.rejected_count,
                        failed: outcome.failed_count,
                        cancelled: outcome.cancelled_count,
                    });
                }
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
                PlanDispatchState::EpicNotOpen { .. }
            ) {
                self.outcomes.lock().await.drop_plan(plan_id);
                return Ok(Vec::new());
            }
            adv.list_ready(ReadyFilter {
                labels_all: vec![crate::plan::labels::plan_id(plan_id)],
                // Limit is per plan; global reconcilers enumerate plans first.
                limit: Some(1000),
                ..Default::default()
            })
            .await?
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
                for summary in adv
                    .list_ready(ReadyFilter {
                        labels_all: vec![crate::plan::labels::plan_id(plan_id)],
                        // Limit is per plan; global reconcilers enumerate plans first.
                        limit: Some(1000),
                        ..Default::default()
                    })
                    .await?
                {
                    if seen_summary_ids.insert(summary.id.clone()) {
                        summaries.push(summary);
                    }
                }
            }
            summaries
        };
        let had_ready_summaries = !summaries.is_empty();

        let mut hydrated = Vec::with_capacity(summaries.len());
        let mut seen_issue_ids = HashSet::new();
        let mut plan_activation_cache = HashMap::new();
        for summary in summaries {
            let issue = self.pm.get_issue(&summary.id).await?;
            match issue.issue_type.as_deref() {
                Some("task") => {
                    if let Some(plan_id) = issue
                        .labels
                        .iter()
                        .find_map(|label| crate::plan::labels::parse_plan_id(label))
                        .map(str::to_string)
                    {
                        let dispatch_state = self
                            .plan_allows_dispatch(&plan_id, &mut plan_activation_cache)
                            .await?;
                        if let Some(reason) = dispatch_state.skip_reason() {
                            let task_id = task_id_from_labels_or_issue(&issue.labels, &issue.id);
                            self.record_skipped(Some(&plan_id), &task_id, reason).await;
                            continue;
                        }
                    }
                    if seen_issue_ids.insert(issue.id.clone()) {
                        hydrated.push(issue_to_summary(issue));
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
                    let dispatch_state = self
                        .plan_allows_dispatch(&plan_id, &mut plan_activation_cache)
                        .await?;
                    if let Some(reason) = dispatch_state.skip_reason() {
                        self.record_skipped(Some(&plan_id), &issue.id, reason).await;
                        continue;
                    }
                    let projected = self.project_plan_from_beads(&plan_id).await?;
                    for task in &projected.tasks {
                        if !matches!(task.status, crate::plan::PlanTaskStatus::Ready) {
                            let blocked_by = unresolved_blocker_issue_ids(&projected, task);
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
                        let task_issue = self.pm.get_issue(issue_id).await?;
                        hydrated.push(issue_to_summary(task_issue));
                    }
                }
                _ => {
                    let plan_id = issue
                        .labels
                        .iter()
                        .find_map(|label| crate::plan::labels::parse_plan_id(label));
                    let task_id = task_id_from_labels_or_issue(&issue.labels, &issue.id);
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

    pub async fn plan_allows_dispatch(
        &self,
        plan_id: &str,
        cache: &mut HashMap<String, PlanDispatchState>,
    ) -> anyhow::Result<PlanDispatchState> {
        if let Some(state) = cache.get(plan_id) {
            return Ok(state.clone());
        }

        let mut open_complete_epic = None;
        let mut closed_complete_epic = None;
        let mut pending_epic = None;
        for summary in self
            .pm
            .list_issues(IssueFilter {
                labels: vec![crate::plan::labels::plan_id(plan_id)],
                issue_type: Some("epic".to_string()),
                include_closed: true,
                limit: Some(100),
                ..Default::default()
            })
            .await?
        {
            let epic = self.pm.get_issue(&summary.id).await?;
            if pending_epic.is_none()
                && epic
                    .labels
                    .iter()
                    .any(|label| label == crate::plan::labels::PLAN_PENDING)
            {
                pending_epic = Some(epic.id.clone());
            }
            if epic
                .labels
                .iter()
                .any(|label| label == crate::plan::labels::PLAN_COMPLETE)
            {
                if epic.status == "open" {
                    open_complete_epic = Some(epic.id.clone());
                } else if closed_complete_epic.is_none() {
                    closed_complete_epic = Some(epic.id.clone());
                }
            }
        }

        let state = if let Some(epic_id) = pending_epic {
            PlanDispatchState::PlanHasPendingEpic { epic_id }
        } else if open_complete_epic.is_some() {
            PlanDispatchState::Allowed
        } else if let Some(epic_id) = closed_complete_epic {
            PlanDispatchState::EpicNotOpen { epic_id }
        } else {
            PlanDispatchState::PlanMissingCompleteEpic
        };
        if !matches!(state, PlanDispatchState::Allowed) {
            tracing::debug!(
                plan_id = %plan_id,
                ?state,
                "reconciler suppressed ready tasks for inactive plan"
            );
        }
        cache.insert(plan_id.to_string(), state.clone());
        Ok(state)
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
            .map(|summary| summary.id)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pro_feature_gate() -> Arc<spur_license::FeatureGate> {
        let gate = Arc::new(spur_license::FeatureGate::new(
            spur_license::policy::PolicyResolver::embedded(),
        ));
        let features =
            std::collections::BTreeSet::from([spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED
                .as_str()
                .to_string()]);
        gate.update_state(&spur_license::LicenseState::active_validated(
            spur_license::Plan::Pro,
            features,
        ));
        gate
    }

    #[test]
    fn reconciler_dispatch_ctx_can_be_cloned_for_server_startup() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<crate::tools::DelegationRequest>(1);
        let ctx = super::ReconcilerDispatchCtx {
            delegation_tx: tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
            event_sink: None,
            materializer: Arc::new(crate::outcome_materializer::OutcomeMaterializer::new(
                Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            )),
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
            ..Default::default()
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

    /// Regression: journal monitor must exit promptly when aborted so that
    /// graceful shutdown does not hang forever awaiting the handle and so
    /// that abort/drop does not leak a detached polling task.
    #[tokio::test]
    async fn journal_monitor_exits_on_abort_without_hang() {
        use std::time::Duration;

        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("journal");
        tokio::fs::write(&path, b"x").await.expect("write");
        let notify = Arc::new(Notify::new());
        let handle = tokio::spawn(monitor_journal_appends(path, notify));
        handle.abort();
        let result = tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("monitor must exit within 1s of abort");
        assert!(
            result.is_err() && result.unwrap_err().is_cancelled(),
            "monitor must be cancelled, not panic"
        );
    }

    #[tokio::test]
    async fn monitor_journal_appends_survives_transient_metadata_error() {
        use std::time::Duration;

        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("journal");
        let hidden = dir.path().join("journal.hidden");
        tokio::fs::write(&path, b"seed")
            .await
            .expect("write seed journal");

        let notify = Arc::new(Notify::new());
        let handle = tokio::spawn(monitor_journal_appends(path.clone(), Arc::clone(&notify)));

        tokio::time::sleep(Duration::from_millis(50)).await;
        tokio::fs::rename(&path, &hidden)
            .await
            .expect("hide journal to force metadata failure");
        tokio::time::sleep(Duration::from_millis(350)).await;
        tokio::fs::write(&path, b"seed-after-retry")
            .await
            .expect("recreate journal with appended content");

        tokio::time::timeout(Duration::from_secs(2), notify.notified())
            .await
            .expect("monitor should retry transient metadata failures and wake on later append");

        handle.abort();
        let _ = handle.await;
    }

    #[test]
    fn auto_pr_params_include_plan_id_and_summary() {
        let params =
            super::build_auto_pr_params("plan-123", "Epic title", "All approved", "spur/merge-1");
        assert!(
            params.title.contains("plan-123"),
            "title missing plan_id: {}",
            params.title
        );
        assert!(
            params.body.contains("All approved"),
            "body missing outcome: {}",
            params.body
        );
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
            self.actions
                .lock()
                .await
                .push(format!("pr:{}", params.title));
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
            Command::new("br")
                .args(["init"])
                .current_dir(repo)
                .output()
                .expect("br init")
                .status
                .success(),
            "br init failed"
        );

        let epic_out = Command::new("br")
            .args(["create", "--type", "epic", "--title", "Test Epic", "--json"])
            .current_dir(repo)
            .output()
            .expect("br create epic");
        let epic_json = String::from_utf8_lossy(&epic_out.stdout);
        let epic_id: String = serde_json::from_str::<serde_json::Value>(&epic_json).unwrap()["id"]
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
            let task_id: String = serde_json::from_str::<serde_json::Value>(&task_json).unwrap()
                ["id"]
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
            pro_feature_gate(),
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

    /// Focused regression: when durable EpicCompletion audit emission fails
    /// (e.g. disk-full / read-only database), the reconciler must suppress
    /// merge_plan / create_pr even though the epic is closed and carries
    /// integration-pending. Without this guard the old code would proceed
    /// because it unconditionally appended a synthetic EpicCompletion to the
    /// local audits vector.
    #[tokio::test]
    async fn failed_epic_completion_audit_suppresses_automation() {
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
            eprintln!(
                "skipping failed_epic_completion_audit_suppresses_automation: `br` not on PATH"
            );
            return;
        }

        let dir = TempDir::new().expect("tempdir");
        let repo = dir.path();

        assert!(
            Command::new("br")
                .args(["init"])
                .current_dir(repo)
                .output()
                .expect("br init")
                .status
                .success(),
            "br init failed"
        );

        let epic_out = Command::new("br")
            .args(["create", "--type", "epic", "--title", "Test Epic", "--json"])
            .current_dir(repo)
            .output()
            .expect("br create epic");
        let epic_json = String::from_utf8_lossy(&epic_out.stdout);
        let epic_id: String = serde_json::from_str::<serde_json::Value>(&epic_json).unwrap()["id"]
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
            let task_id: String = serde_json::from_str::<serde_json::Value>(&task_json).unwrap()
                ["id"]
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

        // Make the beads database read-only so that add_comment (and therefore
        // emit_epic_completion_audit) fails, while list_issues/list_comments
        // continue to work because SQLite opens read-only for queries.
        let db_path = repo.join(".beads").join("beads.db");
        let mut perms = std::fs::metadata(&db_path)
            .expect("db metadata")
            .permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&db_path, perms).expect("set readonly");

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
            pro_feature_gate(),
        );
        reconciler.set_auto_merge_approved_plans(true);
        reconciler.set_automation(automation);

        reconciler.tick_once().await.unwrap();

        let recorded = actions.lock().await;
        assert!(
            recorded.is_empty(),
            "failed epic-completion audit must suppress automation, got: {:?}",
            *recorded
        );
    }

    #[tokio::test]
    async fn hybrid_journal_probe_disables_itself_when_missing() {
        let notify = Arc::new(Notify::new());
        let path = std::path::PathBuf::from("/nonexistent/path/.beads/journal");
        // The monitor must exit gracefully (not panic or hang) when the journal is absent.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            super::monitor_journal_appends(path, notify),
        )
        .await;
        assert!(
            result.is_ok(),
            "journal monitor must exit when path is missing, not hang"
        );
    }
}
