#![cfg(all(madsim, feature = "madsim-sim"))]
#![allow(unexpected_cfgs)]

extern crate madsim_tokio as tokio;

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use futures::future::BoxFuture;
use spur_acp::{BrainSessionId, DelegationResult, DelegationStatus, SessionId};
use spur_mcp::events::McpEventSink;
use spur_mcp::outcome_materializer::OutcomeMaterializer;
use spur_mcp::plan::audit_sentinel::{AuditSentinelKind, CompletionState};
use spur_mcp::plan::labels;
use spur_mcp::plan::reconciler::{
    Clock, PreviewStrategy, Reconciler, ReconcilerConfig, ReconcilerDispatch,
};
use spur_mcp::plan::{PlanState, PlanTaskStatus, PmLike};
use spur_pm::{BeadsAdvanced, Issue, IssueFilter, IssueSummary, PmSource, ReadyFilter};
use tokio::sync::{mpsc, oneshot, Notify};

const PLAN_ID: &str = "plan-madsim";
const EPIC_ID: &str = "bd-epic";
const BRAIN: &str = "brain-madsim";

#[test]
fn happy_pending_ready_dispatched_awaiting_review_approved_complete() {
    run_seed(0x5eed_0001, happy_scenario);
}

#[test]
fn edge_lease_expiry_silent_worker_reclaims_and_redispatches() {
    run_seed(0x5eed_0002, edge_lease_expiry_scenario);
}

#[test]
fn edge_setup_conflict_blocks_and_does_not_auto_clear() {
    run_seed(0x5eed_0003, edge_setup_conflict_scenario);
}

#[test]
fn edge_terminal_before_dispatch_closes_epic_before_stale_ready_dispatch() {
    run_seed(0x5eed_0004, edge_terminal_before_dispatch_scenario);
}

#[test]
fn edge_cancel_mid_tick_keeps_durable_dispatch_for_next_recovery() {
    run_seed(0x5eed_0005, edge_cancel_mid_tick_scenario);
}

#[test]
fn edge_fast_forward_storm_ticks_without_starvation() {
    run_seed(0x5eed_0006, edge_fast_forward_storm_scenario);
}

#[test]
fn edge_completion_projection_timeout_does_not_redeliver_on_next_tick() {
    run_seed(
        0x5eed_0007,
        edge_completion_projection_timeout_does_not_redeliver_on_next_tick_scenario,
    );
}

async fn happy_scenario() {
    print_seed("happy_pending_ready_dispatched_awaiting_review_approved_complete");
    let harness = Harness::new(SimPlan::chain3());

    for task_id in ["T1", "T2", "T3"] {
        let request = harness.tick_and_receive_dispatch().await;
        harness.complete_success(request).await;
        harness.wait_for_status(task_id, is_awaiting_review).await;
        harness.approve(task_id).await;
    }

    harness.reconciler.tick_once().await.unwrap();

    assert!(harness.pm.epic_is_closed());
    assert_eq!(
        harness.pm.statuses(),
        vec![
            ("T1".to_string(), "approved".to_string()),
            ("T2".to_string(), "approved".to_string()),
            ("T3".to_string(), "approved".to_string()),
        ]
    );
    assert!(harness.sink.count_plan_completed() >= 1);
}

async fn edge_lease_expiry_scenario() {
    print_seed("edge_lease_expiry_silent_worker_reclaims_and_redispatches");
    let harness = Harness::new(SimPlan::single_dispatched_with_expired_lease());

    harness.reconciler.tick_once().await.unwrap();
    let request = harness.recv_dispatch().await;

    assert_eq!(harness.pm.dispatch_count("T1"), 2);
    assert_eq!(harness.pm.project_status("T1").await, "dispatched");
    assert_eq!(request.issue_id.as_deref(), Some("bd-t1"));
}

async fn edge_setup_conflict_scenario() {
    print_seed("edge_setup_conflict_blocks_and_does_not_auto_clear");
    let harness = Harness::new_with_config(SimPlan::two_approved_parents_ready_child(), |cfg| {
        cfg.predispatch_preview = PreviewStrategy::AlwaysConflict {
            dep_task_id: "T1".to_string(),
            files: vec!["src/lib.rs".to_string()],
        };
    });

    harness.reconciler.tick_once().await.unwrap();
    harness.assert_no_dispatch().await;

    let projected = harness.pm.project().await;
    let task = projected
        .tasks
        .iter()
        .find(|task| task.spec.task_id == "T3")
        .unwrap();
    assert!(matches!(
        task.status,
        PlanTaskStatus::BlockedOnSetupConflict { .. }
    ));
    assert_eq!(harness.continuations.load(Ordering::SeqCst), 1);

    harness.reconciler.tick_once().await.unwrap();
    harness.assert_no_dispatch().await;
    assert!(matches!(
        harness
            .pm
            .project()
            .await
            .tasks
            .iter()
            .find(|task| task.spec.task_id == "T3")
            .unwrap()
            .status,
        PlanTaskStatus::BlockedOnSetupConflict { .. }
    ));
}

async fn edge_terminal_before_dispatch_scenario() {
    print_seed("edge_terminal_before_dispatch_closes_epic_before_stale_ready_dispatch");
    let harness = Harness::new(SimPlan::chain3());

    harness.pm.approve_direct("T1");
    harness.pm.approve_direct("T2");
    harness.pm.approve_direct("T3");
    harness.pm.force_stale_ready("bd-t3");

    harness.reconciler.tick_once().await.unwrap();

    assert!(harness.pm.epic_is_closed());
    harness.assert_no_dispatch().await;
}

async fn edge_cancel_mid_tick_scenario() {
    print_seed("edge_cancel_mid_tick_keeps_durable_dispatch_for_next_recovery");
    let harness = Harness::new_with_config(SimPlan::single_ready(), |cfg| {
        cfg.base_interval = Duration::from_millis(1);
        cfg.dispatch_lease_duration = Duration::from_secs(0);
    });
    let Harness {
        pm,
        reconciler,
        dispatch_tx,
        dispatch_rx,
        fast_forward,
        sink,
        continuations,
    } = harness;
    dispatch_tx.send(dummy_request()).await.unwrap();

    let (cancel_tx, cancel_rx) = oneshot::channel();
    let observed = pm.dispatch_intent_observed();
    let run = tokio::spawn(reconciler.run(cancel_rx));

    fast_forward.notify_one();
    observed.notified().await;
    cancel_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), run)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(pm.dispatch_count("T1"), 1);

    let _ = dispatch_rx.lock().unwrap().recv().await;
    let recovery = Harness::from_existing_pm(pm, sink, continuations);
    recovery.reconciler.tick_once().await.unwrap();
    let request = recovery.recv_dispatch().await;

    assert_eq!(request.issue_id.as_deref(), Some("bd-t1"));
    assert_eq!(recovery.pm.dispatch_count("T1"), 2);
}

async fn edge_fast_forward_storm_scenario() {
    print_seed("edge_fast_forward_storm_ticks_without_starvation");
    let harness = Harness::new_with_config(SimPlan::single_ready(), |cfg| {
        cfg.base_interval = Duration::from_secs(60);
    });
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let ready_polled = harness.pm.ready_polled();
    let fast_forward = Arc::clone(&harness.fast_forward);
    let run = tokio::spawn(harness.reconciler.run(cancel_rx));

    for _ in 0..128 {
        fast_forward.notify_one();
    }
    ready_polled.notified().await;
    cancel_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), run)
        .await
        .unwrap()
        .unwrap();

    assert!(harness.pm.ready_poll_count() >= 1);
}

async fn edge_completion_projection_timeout_does_not_redeliver_on_next_tick_scenario() {
    print_seed("edge_completion_projection_timeout_does_not_redeliver_on_next_tick");
    let harness = Harness::new(SimPlan::single_ready());
    harness.pm.delay_next_projection_after_completion();

    let request = harness.tick_and_receive_dispatch().await;
    harness.complete_success(request).await;
    harness.wait_for_continuations(1).await;

    assert_eq!(harness.continuations.load(Ordering::SeqCst), 1);
    assert_eq!(harness.sink.count_plan_task_awaiting_review(), 1);

    harness.reconciler.tick_once().await.unwrap();
    tokio::time::sleep(Duration::from_millis(25)).await;

    assert_eq!(
        harness.continuations.load(Ordering::SeqCst),
        1,
        "second tick must not reconstruct and redeliver the same completion"
    );
    assert_eq!(
        harness.sink.count_plan_task_awaiting_review(),
        1,
        "second tick must not emit a duplicate awaiting-review event"
    );
    assert_eq!(harness.pm.dispatch_count("T1"), 1);
}

fn run_seed<Fut>(seed: u64, f: fn() -> Fut)
where
    Fut: Future<Output = ()> + 'static,
{
    let mut builder = madsim::runtime::Builder::from_env();
    builder.seed = std::env::var("MADSIM_TEST_SEED")
        .ok()
        .and_then(|seed| seed.parse::<u64>().ok())
        .unwrap_or(seed);
    builder.count = 1;
    builder.jobs = 1;
    builder.run(f);
}

fn print_seed(name: &'static str) {
    eprintln!(
        "madsim_reconciler scenario={name} seed={}",
        madsim::runtime::Handle::current().seed()
    );
}

struct SimDispatch {
    delegation_tx: mpsc::Sender<spur_mcp::tools::DelegationRequest>,
    brain_session_id: BrainSessionId,
    event_sink: Option<Arc<dyn McpEventSink>>,
    materializer: Arc<OutcomeMaterializer>,
    continuation_ctx: Arc<spur_mcp::server::DetachedContinuationCtx>,
}

#[async_trait]
impl ReconcilerDispatch for SimDispatch {
    async fn send_delegation(
        &self,
        request: spur_mcp::tools::DelegationRequest,
    ) -> anyhow::Result<()> {
        self.delegation_tx
            .send(request)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    fn track_task(&self, fut: BoxFuture<'static, ()>) {
        tokio::spawn(fut);
    }

    fn event_sink(&self) -> Option<&Arc<dyn McpEventSink>> {
        self.event_sink.as_ref()
    }

    fn materializer(&self) -> &Arc<OutcomeMaterializer> {
        &self.materializer
    }

    fn continuation_ctx(&self) -> &Arc<spur_mcp::server::DetachedContinuationCtx> {
        &self.continuation_ctx
    }

    fn brain_session_id(&self) -> &BrainSessionId {
        &self.brain_session_id
    }
}

struct Harness {
    pm: Arc<SimPm>,
    reconciler: Reconciler,
    dispatch_tx: mpsc::Sender<spur_mcp::tools::DelegationRequest>,
    dispatch_rx: Mutex<mpsc::Receiver<spur_mcp::tools::DelegationRequest>>,
    fast_forward: Arc<Notify>,
    sink: Arc<RecordingEventSink>,
    continuations: Arc<AtomicUsize>,
}

struct SimClock;

impl Clock for SimClock {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    fn sleep<'a>(&'a self, duration: Duration) -> BoxFuture<'a, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

impl Harness {
    fn new(plan: SimPlan) -> Self {
        Self::new_with_config(plan, |_| {})
    }

    fn new_with_config(plan: SimPlan, configure: impl FnOnce(&mut ReconcilerConfig)) -> Self {
        let pm = Arc::new(SimPm::new(plan));
        Self::from_existing_pm_with_config(pm, Arc::default(), Arc::default(), configure)
    }

    fn from_existing_pm(
        pm: Arc<SimPm>,
        sink: Arc<RecordingEventSink>,
        continuations: Arc<AtomicUsize>,
    ) -> Self {
        Self::from_existing_pm_with_config(pm, sink, continuations, |_| {})
    }

    fn from_existing_pm_with_config(
        pm: Arc<SimPm>,
        sink: Arc<RecordingEventSink>,
        continuations: Arc<AtomicUsize>,
        configure: impl FnOnce(&mut ReconcilerConfig),
    ) -> Self {
        let (dispatch_tx, dispatch_rx) = mpsc::channel(8);
        let fast_forward = Arc::new(Notify::new());
        let continuation_count = Arc::clone(&continuations);
        let continuation_ctx = Arc::new(spur_mcp::server::DetachedContinuationCtx {
            on_complete: Arc::new(move |_, _| {
                let continuation_count = Arc::clone(&continuation_count);
                Box::pin(async move {
                    continuation_count.fetch_add(1, Ordering::SeqCst);
                })
            }),
        });
        let dispatch = Arc::new(SimDispatch {
            delegation_tx: dispatch_tx.clone(),
            brain_session_id: brain_session_id(),
            event_sink: Some(sink.clone()),
            materializer: Arc::new(OutcomeMaterializer::new(Arc::new(
                spur_blob_store::MemoryOutcomeStore::new(),
            ))),
            continuation_ctx,
        }) as Arc<dyn ReconcilerDispatch>;
        let mut config = ReconcilerConfig {
            base_interval: Duration::from_millis(5),
            idle_ceiling: Duration::from_millis(20),
            backoff_factor: 2,
            dispatch_lease_duration: Duration::from_secs(60),
            label_only_dispatch_grace: Duration::from_secs(0),
            repo_root: PathBuf::from("."),
            predispatch_preview: PreviewStrategy::AlwaysClean,
        };
        configure(&mut config);
        let mut reconciler = Reconciler::new_with_pm_like(
            config,
            pm.clone() as Arc<dyn PmLike>,
            fast_forward.clone(),
            Some(dispatch),
            Some(PLAN_ID.to_string()),
            pro_feature_gate(),
        );
        reconciler.set_clock(Arc::new(SimClock));
        Self {
            pm,
            reconciler,
            dispatch_tx,
            dispatch_rx: Mutex::new(dispatch_rx),
            fast_forward,
            sink,
            continuations,
        }
    }

    async fn tick_and_receive_dispatch(&self) -> spur_mcp::tools::DelegationRequest {
        self.reconciler.tick_once().await.unwrap();
        self.recv_dispatch().await
    }

    async fn recv_dispatch(&self) -> spur_mcp::tools::DelegationRequest {
        tokio::time::timeout(Duration::from_secs(1), async {
            self.dispatch_rx.lock().unwrap().recv().await.unwrap()
        })
        .await
        .unwrap()
    }

    async fn assert_no_dispatch(&self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(5), async {
                self.dispatch_rx.lock().unwrap().recv().await
            })
            .await
            .is_err(),
            "unexpected dispatch request"
        );
    }

    async fn complete_success(&self, request: spur_mcp::tools::DelegationRequest) {
        request
            .respond_to
            .send(DelegationResult {
                status: DelegationStatus::Success,
                diff: None,
                diff_summary: None,
                summary: Some("worker complete".to_string()),
                estimated_cost_usd: 0.0,
                worker_branch: Some(format!("spur/{}", request.id.as_str())),
                artifact: None,
            })
            .unwrap();
    }

    async fn wait_for_status(&self, task_id: &str, pred: fn(&PlanTaskStatus) -> bool) {
        for _ in 0..20 {
            let projected = self.pm.project().await;
            if projected
                .tasks
                .iter()
                .find(|task| task.spec.task_id == task_id)
                .is_some_and(|task| pred(&task.status))
            {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("task {task_id} did not reach expected status");
    }

    async fn approve(&self, task_id: &str) {
        self.pm.approve_from_current(task_id);
        self.reconciler.tick_once().await.unwrap();
    }

    async fn wait_for_continuations(&self, expected: usize) {
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                if self.continuations.load(Ordering::SeqCst) >= expected {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("continuation count did not reach expected value");
    }
}

fn is_awaiting_review(status: &PlanTaskStatus) -> bool {
    matches!(status, PlanTaskStatus::AwaitingReview { .. })
}

#[derive(Default)]
struct RecordingEventSink {
    events: Mutex<Vec<spur_acp::SpurEventBody>>,
}

impl RecordingEventSink {
    fn count_plan_completed(&self) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(event, spur_acp::SpurEventBody::PlanCompleted { .. }))
            .count()
    }

    fn count_plan_task_awaiting_review(&self) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    spur_acp::SpurEventBody::PlanTaskAwaitingReview { .. }
                )
            })
            .count()
    }
}

impl McpEventSink for RecordingEventSink {
    fn emit(&self, event: spur_acp::SpurEventBody) {
        self.events.lock().unwrap().push(event);
    }
}

#[derive(Clone)]
struct SimPlan {
    tasks: Vec<SimTask>,
}

#[derive(Clone)]
struct SimTask {
    task_id: &'static str,
    issue_id: &'static str,
    depends_on: Vec<&'static str>,
    initial: InitialTaskState,
}

#[derive(Clone)]
enum InitialTaskState {
    Open,
    Approved,
    DispatchedExpired,
}

impl SimPlan {
    fn single_ready() -> Self {
        Self {
            tasks: vec![SimTask::open("T1", "bd-t1", vec![])],
        }
    }

    fn chain3() -> Self {
        Self {
            tasks: vec![
                SimTask::open("T1", "bd-t1", vec![]),
                SimTask::open("T2", "bd-t2", vec!["T1"]),
                SimTask::open("T3", "bd-t3", vec!["T2"]),
            ],
        }
    }

    fn single_dispatched_with_expired_lease() -> Self {
        Self {
            tasks: vec![SimTask {
                task_id: "T1",
                issue_id: "bd-t1",
                depends_on: vec![],
                initial: InitialTaskState::DispatchedExpired,
            }],
        }
    }

    fn two_approved_parents_ready_child() -> Self {
        Self {
            tasks: vec![
                SimTask {
                    task_id: "T1",
                    issue_id: "bd-t1",
                    depends_on: vec![],
                    initial: InitialTaskState::Approved,
                },
                SimTask {
                    task_id: "T2",
                    issue_id: "bd-t2",
                    depends_on: vec![],
                    initial: InitialTaskState::Approved,
                },
                SimTask::open("T3", "bd-t3", vec!["T1", "T2"]),
            ],
        }
    }
}

impl SimTask {
    fn open(task_id: &'static str, issue_id: &'static str, depends_on: Vec<&'static str>) -> Self {
        Self {
            task_id,
            issue_id,
            depends_on,
            initial: InitialTaskState::Open,
        }
    }
}

struct SimPm {
    state: Mutex<SimState>,
    dispatch_intent_observed: Arc<Notify>,
    ready_polled: Arc<Notify>,
}

struct SimState {
    issues: BTreeMap<String, Issue>,
    task_issue_by_id: BTreeMap<String, String>,
    comments: BTreeMap<String, Vec<spur_pm::Comment>>,
    stale_ready: BTreeSet<String>,
    dispatch_counts: BTreeMap<String, usize>,
    ready_poll_count: usize,
    comment_seq: u64,
    delay_projection_after_completion: bool,
    delay_next_comment_list: bool,
}

impl SimPm {
    fn new(plan: SimPlan) -> Self {
        let mut state = SimState {
            issues: BTreeMap::new(),
            task_issue_by_id: BTreeMap::new(),
            comments: BTreeMap::new(),
            stale_ready: BTreeSet::new(),
            dispatch_counts: BTreeMap::new(),
            ready_poll_count: 0,
            comment_seq: 0,
            delay_projection_after_completion: false,
            delay_next_comment_list: false,
        };
        state.insert_plan(plan);
        Self {
            state: Mutex::new(state),
            dispatch_intent_observed: Arc::new(Notify::new()),
            ready_polled: Arc::new(Notify::new()),
        }
    }

    fn dispatch_intent_observed(&self) -> Arc<Notify> {
        Arc::clone(&self.dispatch_intent_observed)
    }

    fn ready_polled(&self) -> Arc<Notify> {
        Arc::clone(&self.ready_polled)
    }

    fn ready_poll_count(&self) -> usize {
        self.state.lock().unwrap().ready_poll_count
    }

    fn delay_next_projection_after_completion(&self) {
        self.state.lock().unwrap().delay_projection_after_completion = true;
    }

    async fn project(&self) -> PlanState {
        spur_mcp::plan::projector::project_plan_from_beads(
            self,
            PLAN_ID,
            pro_feature_gate().as_ref(),
        )
        .await
        .unwrap()
    }

    async fn project_status(&self, task_id: &str) -> String {
        let projected = self.project().await;
        let status = &projected
            .tasks
            .iter()
            .find(|task| task.spec.task_id == task_id)
            .unwrap()
            .status;
        match status {
            PlanTaskStatus::Pending => "pending",
            PlanTaskStatus::Ready => "ready",
            PlanTaskStatus::Dispatched { .. } => "dispatched",
            PlanTaskStatus::AwaitingReview { .. } => "awaiting_review",
            PlanTaskStatus::Approved { .. } => "approved",
            PlanTaskStatus::Rejected { .. } => "rejected",
            PlanTaskStatus::Failed { .. } => "failed",
            PlanTaskStatus::Cancelled { .. } => "cancelled",
            PlanTaskStatus::Superseded { .. } => "superseded",
            PlanTaskStatus::BlockedOnSetupConflict { .. } => "blocked_on_setup_conflict",
            PlanTaskStatus::EscalatedToBrain { .. } => "escalated_to_brain",
        }
        .to_string()
    }

    fn statuses(&self) -> Vec<(String, String)> {
        let state = self.state.lock().unwrap();
        state
            .task_issue_by_id
            .keys()
            .map(|task_id| {
                let issue = state.issue_for_task(task_id);
                (task_id.clone(), issue.status.clone())
            })
            .collect()
    }

    fn dispatch_count(&self, task_id: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .dispatch_counts
            .get(task_id)
            .copied()
            .unwrap_or(0)
    }

    fn epic_is_closed(&self) -> bool {
        self.state.lock().unwrap().issues[EPIC_ID].status == "closed"
    }

    fn approve_from_current(&self, task_id: &str) {
        let mut state = self.state.lock().unwrap();
        let issue_id = state.task_issue_by_id[task_id].clone();
        let delegation_id = state.latest_dispatch(&issue_id).unwrap();
        state.add_audit(&issue_id, AuditSentinelKind::Approval { delegation_id });
        let issue = state.issues.get_mut(&issue_id).unwrap();
        issue.status = "closed".to_string();
        issue.labels.retain(|label| {
            label != labels::READY_FOR_REVIEW && labels::parse_delegation_id(label).is_none()
        });
    }

    fn approve_direct(&self, task_id: &str) {
        let mut state = self.state.lock().unwrap();
        state.approve_direct(task_id);
    }

    fn force_stale_ready(&self, issue_id: &str) {
        self.state
            .lock()
            .unwrap()
            .stale_ready
            .insert(issue_id.to_string());
    }
}

impl SimState {
    fn insert_plan(&mut self, plan: SimPlan) {
        self.issues.insert(
            EPIC_ID.to_string(),
            issue(
                EPIC_ID,
                "epic",
                "open",
                vec![
                    labels::plan_id(PLAN_ID),
                    labels::PLAN_COMPLETE.to_string(),
                    labels::plan_owner(BRAIN),
                ],
            ),
        );
        self.add_audit(
            EPIC_ID,
            AuditSentinelKind::PlanSubmit {
                plan_id: PLAN_ID.to_string(),
                epic_issue_id: EPIC_ID.to_string(),
                task_ids: plan
                    .tasks
                    .iter()
                    .map(|task| task.issue_id.to_string())
                    .collect(),
                base_snapshot_branch: Some("HEAD".to_string()),
                base_snapshot_oid: None,
                execution_mode: None,
                brain_session_id: Some(BRAIN.to_string()),
                explicit_base: None,
            },
        );

        let issue_by_task = plan
            .tasks
            .iter()
            .map(|task| (task.task_id, task.issue_id))
            .collect::<BTreeMap<_, _>>();
        for task in plan.tasks {
            let blocked_by = task
                .depends_on
                .iter()
                .map(|dep| issue_by_task[dep].to_string())
                .collect::<Vec<_>>();
            let mut item = issue(
                task.issue_id,
                "task",
                "open",
                vec![
                    labels::plan_id(PLAN_ID),
                    labels::plan_task_id(task.task_id),
                    labels::plan_owner(BRAIN),
                    "spur:agent:codex".to_string(),
                ],
            );
            item.body = format!("Implement {}", task.task_id);
            item.blocked_by = blocked_by;
            self.task_issue_by_id
                .insert(task.task_id.to_string(), task.issue_id.to_string());
            self.issues.insert(task.issue_id.to_string(), item);
            self.add_audit(
                task.issue_id,
                AuditSentinelKind::TaskSpec {
                    task_id: task.task_id.to_string(),
                    context_files: Vec::new(),
                    task_text: Some(format!("Implement {}", task.task_id)),
                    agent: Some("codex".to_string()),
                    depends_on: Some(task.depends_on.iter().map(|dep| dep.to_string()).collect()),
                },
            );
            match task.initial {
                InitialTaskState::Open => {}
                InitialTaskState::Approved => self.approve_direct(task.task_id),
                InitialTaskState::DispatchedExpired => {
                    self.mark_dispatched(task.issue_id, task.task_id, "del-expired", 1);
                    let item = self.issues.get_mut(task.issue_id).unwrap();
                    item.labels.push(labels::lease_expires_at(0));
                }
            }
        }
    }

    fn approve_direct(&mut self, task_id: &str) {
        let issue_id = self.task_issue_by_id[task_id].clone();
        let delegation_id = format!("del-approved-{task_id}");
        self.mark_dispatched(&issue_id, task_id, &delegation_id, 1);
        self.add_audit(
            &issue_id,
            AuditSentinelKind::Completion {
                delegation_id: delegation_id.clone(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some(format!("spur/{task_id}")),
                result_summary: Some("approved setup".to_string()),
                artifact_uri: None,
                dispatched_base_oid: Some(hex_oid(task_id)),
            },
        );
        self.add_audit(&issue_id, AuditSentinelKind::Approval { delegation_id });
        let issue = self.issues.get_mut(&issue_id).unwrap();
        issue.status = "closed".to_string();
        issue.labels.retain(|label| {
            label != labels::READY_FOR_REVIEW && labels::parse_delegation_id(label).is_none()
        });
    }

    fn mark_dispatched(
        &mut self,
        issue_id: &str,
        task_id: &str,
        delegation_id: &str,
        attempt: u32,
    ) {
        self.add_audit(
            issue_id,
            AuditSentinelKind::Dispatch {
                delegation_id: delegation_id.to_string(),
                worker: "codex".to_string(),
                attempt,
            },
        );
        self.dispatch_counts
            .entry(task_id.to_string())
            .and_modify(|count| *count += 1)
            .or_insert(1);
        let issue = self.issues.get_mut(issue_id).unwrap();
        issue.labels.push(labels::delegation_id(delegation_id));
    }

    fn add_audit(&mut self, issue_id: &str, kind: AuditSentinelKind) {
        self.add_comment(
            issue_id,
            spur_mcp::plan::audit_sentinel::encode_comment(&kind),
        );
    }

    fn add_comment(&mut self, issue_id: &str, body: String) {
        self.comment_seq += 1;
        self.comments
            .entry(issue_id.to_string())
            .or_default()
            .push(spur_pm::Comment {
                id: self.comment_seq.to_string(),
                body,
                actor: "madsim".to_string(),
                created_at: Utc.timestamp_opt(self.comment_seq as i64, 0).unwrap(),
            });
    }

    fn latest_dispatch(&self, issue_id: &str) -> Option<String> {
        let audits = spur_mcp::plan::projector::collect_sorted_audits_for_issue(
            issue_id,
            self.comments.get(issue_id).cloned().unwrap_or_default(),
        )
        .unwrap();
        audits.iter().rev().find_map(|audit| match audit {
            AuditSentinelKind::Dispatch { delegation_id, .. } => Some(delegation_id.clone()),
            _ => None,
        })
    }

    fn issue_for_task(&self, task_id: &str) -> &Issue {
        &self.issues[&self.task_issue_by_id[task_id]]
    }
}

#[async_trait]
impl PmLike for SimPm {
    async fn get_issue(&self, id: &str) -> anyhow::Result<Issue> {
        Ok(self.state.lock().unwrap().issues[id].clone())
    }

    async fn list_issues(&self, filter: IssueFilter) -> anyhow::Result<Vec<IssueSummary>> {
        let state = self.state.lock().unwrap();
        Ok(state
            .issues
            .values()
            .filter(|issue| {
                filter
                    .issue_type
                    .as_ref()
                    .is_none_or(|issue_type| issue.issue_type.as_ref() == Some(issue_type))
                    && filter
                        .status
                        .as_ref()
                        .is_none_or(|status| &issue.status == status)
                    && filter
                        .labels
                        .iter()
                        .all(|label| issue.labels.contains(label))
            })
            .map(summary)
            .collect())
    }

    async fn update_issue(&self, id: &str, update: spur_pm::IssueUpdate) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        let issue = state.issues.get_mut(id).unwrap();
        if let Some(status) = update.status {
            issue.status = status;
        }
        if let Some(body) = update.body {
            issue.body = body;
        }
        for remove in update.remove_labels {
            issue.labels.retain(|label| label != &remove);
        }
        for add in update.add_labels {
            if !issue.labels.contains(&add) {
                issue.labels.push(add);
            }
        }
        if let Some(comment) = update.comment {
            state.add_comment(id, comment);
        }
        Ok(())
    }

    async fn issue_labels(&self, id: &str) -> anyhow::Result<Vec<String>> {
        Ok(self.state.lock().unwrap().issues[id].labels.clone())
    }

    fn closed_status(&self) -> &str {
        "closed"
    }

    fn advanced(&self) -> Option<&dyn BeadsAdvanced> {
        Some(self)
    }
}

#[async_trait]
impl BeadsAdvanced for SimPm {
    async fn list_ready(&self, filter: ReadyFilter) -> anyhow::Result<Vec<IssueSummary>> {
        let mut state = self.state.lock().unwrap();
        state.ready_poll_count += 1;
        self.ready_polled.notify_waiters();
        let forced = state.stale_ready.clone();
        Ok(state
            .issues
            .values()
            .filter(|issue| {
                if forced.contains(&issue.id) {
                    return true;
                }
                issue.issue_type.as_deref() == Some("task")
                    && issue.status == "open"
                    && filter
                        .labels_all
                        .iter()
                        .all(|label| issue.labels.contains(label))
                    && issue.blocked_by.iter().all(|dep| {
                        state
                            .issues
                            .get(dep)
                            .is_some_and(|dep_issue| dep_issue.status == "closed")
                    })
                    && !issue
                        .labels
                        .iter()
                        .any(|label| labels::parse_delegation_id(label).is_some())
            })
            .map(summary)
            .collect())
    }

    async fn list_comments(&self, issue_id: &str) -> anyhow::Result<Vec<spur_pm::Comment>> {
        let should_delay = {
            let mut state = self.state.lock().unwrap();
            let should_delay = state.delay_next_comment_list;
            if should_delay {
                state.delay_next_comment_list = false;
            }
            should_delay
        };
        if should_delay {
            tokio::time::sleep(Duration::from_secs(11)).await;
        }
        Ok(self
            .state
            .lock()
            .unwrap()
            .comments
            .get(issue_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn add_comment(&self, issue_id: &str, body: &str) -> anyhow::Result<String> {
        let mut state = self.state.lock().unwrap();
        if state.delay_projection_after_completion
            && matches!(
                spur_mcp::plan::audit_sentinel::parse_comment(body),
                Some(Ok(AuditSentinelKind::Completion { .. }))
            )
        {
            state.delay_projection_after_completion = false;
            state.delay_next_comment_list = true;
        }
        if let Some(AuditSentinelKind::Dispatch { .. }) =
            spur_mcp::plan::audit_sentinel::parse_comment(body).and_then(Result::ok)
        {
            let task_id = state
                .task_issue_by_id
                .iter()
                .find_map(|(task_id, found_issue_id)| {
                    (found_issue_id == issue_id).then(|| task_id.clone())
                })
                .unwrap_or_else(|| issue_id.to_string());
            state
                .dispatch_counts
                .entry(task_id)
                .and_modify(|count| *count += 1)
                .or_insert(1);
            self.dispatch_intent_observed.notify_waiters();
        }
        state.add_comment(issue_id, body.to_string());
        Ok(state.comment_seq.to_string())
    }

    async fn remove_dependency(&self, _issue_id: &str, _depends_on_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn dep_cycles(&self) -> anyhow::Result<Vec<spur_pm::DependencyCycle>> {
        Ok(Vec::new())
    }
}

fn dummy_request() -> spur_mcp::tools::DelegationRequest {
    let (respond_to, _rx) = oneshot::channel();
    spur_mcp::tools::DelegationRequest {
        id: "dummy".to_string().into(),
        agent: "codex".to_string(),
        task: "dummy".to_string(),
        context_files: Vec::new(),
        prior_branch_for_reuse: None,
        respond_to,
        brain_session_id: brain_session_id(),
        delegation_plan: None,
        issue_id: Some("dummy".to_string()),
        base: None,
        dispatched_base_oid_tx: None,
        attempt_tracker: Arc::new(std::sync::atomic::AtomicU32::new(1)),
        enable_worker_mcp: None,
    }
}

fn pro_feature_gate() -> Arc<spur_license::FeatureGate> {
    let gate = Arc::new(spur_license::FeatureGate::new(
        spur_license::policy::PolicyResolver::embedded(),
    ));
    gate.update_state(&spur_license::LicenseState::active_validated(
        spur_license::Plan::Pro,
        BTreeSet::from([spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED
            .as_str()
            .to_string()]),
    ));
    gate
}

fn brain_session_id() -> BrainSessionId {
    BrainSessionId::new(SessionId(BRAIN.to_string()))
}

fn issue(id: &str, issue_type: &str, status: &str, labels: Vec<String>) -> Issue {
    Issue {
        id: id.to_string(),
        source: PmSource::Beads,
        title: id.to_string(),
        body: id.to_string(),
        status: status.to_string(),
        labels,
        assignee: None,
        url: format!("https://example.invalid/{id}"),
        priority: None,
        issue_type: Some(issue_type.to_string()),
        blocked_by: Vec::new(),
        due_at: None,
        source_system: None,
        source_repo: None,
        external_ref: None,
        created_at: Utc.timestamp_opt(0, 0).unwrap(),
        updated_at: Utc.timestamp_opt(0, 0).unwrap(),
    }
}

fn summary(issue: &Issue) -> IssueSummary {
    IssueSummary {
        id: issue.id.clone(),
        source: issue.source.clone(),
        title: issue.title.clone(),
        status: issue.status.clone(),
        labels: issue.labels.clone(),
        url: issue.url.clone(),
        priority: issue.priority,
        issue_type: issue.issue_type.clone(),
        assignee: issue.assignee.clone(),
        description: Some(issue.body.clone()),
    }
}

fn hex_oid(seed: &str) -> String {
    let byte = seed.as_bytes().first().copied().unwrap_or(b'a');
    format!("{byte:02x}").repeat(20)
}
