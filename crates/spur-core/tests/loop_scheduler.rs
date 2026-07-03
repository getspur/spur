//! Integration tests for the loop scheduler sweep.

#![recursion_limit = "256"]

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use spur_acp::domain::continuation::{BrainContinuation, ContinuationSource};
use spur_acp::{BrainSessionId, SessionId};
use spur_core::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_core::plan::labels;
use spur_core::plan::loops::spec::{LoopGovernors, LoopSpec};
use spur_core::plan::reconciler::{
    Clock, Reconciler, ReconcilerConfig, ReconcilerDispatch, ReconcilerDispatchCtx,
};
use spur_core::plan::test_util::MockPm;
use spur_core::plan::PmLike;
use spur_core::server::DetachedContinuationCtx;
use tokio::sync::{mpsc, Notify};

#[allow(dead_code)]
mod common;

#[derive(Debug)]
struct FixedClock {
    now: SystemTime,
}

impl Clock for FixedClock {
    fn now(&self) -> SystemTime {
        self.now
    }
}

fn unix(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

fn test_materializer() -> Arc<spur_core::outcome_materializer::OutcomeMaterializer> {
    Arc::new(spur_core::outcome_materializer::OutcomeMaterializer::new(
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
    ))
}

fn capture_continuations() -> (
    Arc<DetachedContinuationCtx>,
    mpsc::UnboundedReceiver<(BrainContinuation, String)>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let ctx = DetachedContinuationCtx {
        on_complete: Arc::new(move |continuation, worker_session| {
            let tx = tx.clone();
            Box::pin(async move {
                tx.send((continuation, worker_session))
                    .expect("record loop continuation");
            })
        }),
    };
    (Arc::new(ctx), rx)
}

fn dispatch_ctx(continuation_ctx: Arc<DetachedContinuationCtx>) -> Arc<dyn ReconcilerDispatch> {
    let (delegation_tx, _delegation_rx) = mpsc::channel(1);
    Arc::new(ReconcilerDispatchCtx {
        delegation_tx,
        task_tracker: tokio_util::task::TaskTracker::new(),
        brain_session_id: BrainSessionId::new(SessionId("brain-loop-test".to_string())),
        event_sink: None,
        materializer: test_materializer(),
        continuation_ctx,
    })
}

fn reconciler(
    pm: Arc<MockPm>,
    config: ReconcilerConfig,
    now_secs: u64,
    continuation_ctx: Arc<DetachedContinuationCtx>,
) -> Reconciler {
    let pm_like: Arc<dyn PmLike> = pm;
    let mut reconciler = Reconciler::new_with_pm_like(
        config,
        pm_like,
        Arc::new(Notify::new()),
        Some(dispatch_ctx(continuation_ctx)),
        None,
        common::server_builder::pro_feature_gate(),
    );
    reconciler.set_clock(Arc::new(FixedClock {
        now: unix(now_secs),
    }));
    reconciler
}

fn loop_spec(loop_id: &str, cadence_secs: u64) -> LoopSpec {
    LoopSpec {
        loop_id: loop_id.to_string(),
        goal: "Keep CI green".to_string(),
        pattern: Some("ci-sweeper".to_string()),
        cadence_secs,
        autonomy: labels::AutonomyLevel::L1,
        template: serde_json::json!({
            "tasks": [
                {
                    "task_id": "triage",
                    "agent": "codex",
                    "task": "Check CI and propose follow-up work.",
                    "depends_on": []
                }
            ]
        }),
        governors: LoopGovernors::default(),
        escalation: None,
    }
}

async fn create_loop_issue(
    pm: &MockPm,
    spec: &LoopSpec,
    next_run: Option<i64>,
    extra_labels: Vec<String>,
) -> String {
    let mut labels = vec![labels::loop_id_label(&spec.loop_id)];
    if let Some(ts) = next_run {
        labels.push(labels::loop_next_run_label(ts));
    }
    labels.extend(extra_labels);
    pm.create_issue(spur_pm::IssueCreate {
        title: format!("Loop {}", spec.loop_id),
        description: Some(spec.to_sentinel_body()),
        issue_type: Some("task".to_string()),
        priority: Some(2),
        labels,
        ..Default::default()
    })
    .await
    .expect("create loop issue")
}

async fn create_generation_epic(
    pm: &MockPm,
    loop_id: &str,
    generation: u32,
    status: &str,
) -> String {
    let epic_id = pm
        .create_issue(spur_pm::IssueCreate {
            title: format!("Loop generation {generation}"),
            issue_type: Some("epic".to_string()),
            priority: Some(2),
            labels: vec![
                labels::loop_id_label(loop_id),
                labels::loop_generation_label(generation),
            ],
            ..Default::default()
        })
        .await
        .expect("create generation epic");
    if status != "open" {
        pm.update_issue(
            &epic_id,
            spur_pm::IssueUpdate {
                status: Some(status.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("update generation status");
    }
    epic_id
}

async fn loop_next_run(pm: &MockPm, loop_issue_id: &str) -> Option<i64> {
    pm.issue(loop_issue_id)
        .await
        .labels
        .iter()
        .find_map(|label| labels::parse_loop_next_run(label))
}

async fn loop_runs(pm: &MockPm, loop_issue_id: &str) -> Vec<AuditSentinelKind> {
    pm.comments(loop_issue_id)
        .await
        .into_iter()
        .filter_map(|comment| audit_sentinel::parse_comment(&comment.body))
        .filter_map(Result::ok)
        .filter(|kind| matches!(kind, AuditSentinelKind::LoopRun { .. }))
        .collect()
}

async fn seed_loop_run(
    pm: &MockPm,
    loop_issue_id: &str,
    loop_id: &str,
    generation: u32,
    outcome: &str,
    ended_at: i64,
) {
    let advanced = pm.advanced().expect("mock pm exposes beads advanced");
    advanced
        .add_comment(
            loop_issue_id,
            &audit_sentinel::encode_comment(&AuditSentinelKind::LoopRun {
                loop_id: loop_id.to_string(),
                generation,
                plan_id: format!("plan-{generation}"),
                outcome: outcome.to_string(),
                tasks_discovered: 1,
                approved: u32::from(outcome == "approved"),
                rejected: 0,
                failed: u32::from(outcome == "failed"),
                cancelled: 0,
                escalations: 0,
                cost_micros: 0,
                started_at: ended_at.saturating_sub(60),
                ended_at,
            }),
        )
        .await
        .expect("seed loop run");
}

#[tokio::test(start_paused = true)]
async fn due_loop_pushes_loop_due_continuation_and_bumps_next_run() {
    let pm = MockPm::new().arc();
    let now = 1_782_950_000;
    let spec = loop_spec("0f47ac10b58cc4372", 60);
    let loop_issue_id = create_loop_issue(pm.as_ref(), &spec, Some(now - 1), Vec::new()).await;
    let (continuation_ctx, mut continuation_rx) = capture_continuations();
    let reconciler = reconciler(
        Arc::clone(&pm),
        ReconcilerConfig::default(),
        now as u64,
        continuation_ctx,
    );

    let did_work = reconciler.tick_once().await.expect("tick_once");

    assert!(did_work, "due loop should count as scheduler work");
    let (continuation, worker_session) = continuation_rx
        .recv()
        .await
        .expect("loop-due continuation should be pushed");
    assert_eq!(continuation.source, ContinuationSource::LoopDue);
    assert_eq!(worker_session, continuation.delegation_id.as_str());
    let summary = continuation
        .payload
        .summary
        .as_deref()
        .expect("loop-due continuation summary");
    assert!(summary.contains(&spec.loop_id));
    assert!(summary.contains("Keep CI green"));
    assert!(summary.contains("generation 1"));
    assert!(summary.contains("\"triage\""));
    assert_eq!(
        loop_next_run(pm.as_ref(), &loop_issue_id).await,
        Some(now + 60)
    );
}

#[tokio::test(start_paused = true)]
async fn undue_loop_is_untouched() {
    let pm = MockPm::new().arc();
    let now = 1_782_950_000;
    let spec = loop_spec("1f47ac10b58cc4372", 120);
    let loop_issue_id = create_loop_issue(pm.as_ref(), &spec, Some(now + 30), Vec::new()).await;
    let (continuation_ctx, mut continuation_rx) = capture_continuations();
    let reconciler = reconciler(
        Arc::clone(&pm),
        ReconcilerConfig::default(),
        now as u64,
        continuation_ctx,
    );

    reconciler.tick_once().await.expect("tick_once");

    assert!(
        continuation_rx.try_recv().is_err(),
        "undue loop must not push a continuation"
    );
    assert_eq!(
        loop_next_run(pm.as_ref(), &loop_issue_id).await,
        Some(now + 30)
    );
    assert!(loop_runs(pm.as_ref(), &loop_issue_id).await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn killed_loop_is_never_rearmed_by_sweep() {
    let pm = MockPm::new().arc();
    let now = 1_782_950_000;
    let spec = loop_spec("retiredloop001", 60);
    let loop_issue_id = create_loop_issue(pm.as_ref(), &spec, Some(now - 1), Vec::new()).await;
    pm.update_issue(
        &loop_issue_id,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close killed loop issue");
    let (continuation_ctx, mut continuation_rx) = capture_continuations();
    let reconciler = reconciler(
        Arc::clone(&pm),
        ReconcilerConfig::default(),
        now as u64,
        continuation_ctx,
    );

    let did_work = reconciler.tick_once().await.expect("tick_once");

    assert!(
        !did_work,
        "closed loop issue must be outside the scheduler's open-issue sweep"
    );
    assert!(
        continuation_rx.try_recv().is_err(),
        "closed loop issue must not push a loop-due continuation"
    );
    assert_eq!(
        loop_next_run(pm.as_ref(), &loop_issue_id).await,
        Some(now - 1),
        "stale next-run label on a closed loop must not be replaced"
    );
    assert!(
        loop_runs(pm.as_ref(), &loop_issue_id).await.is_empty(),
        "closed loop issue must not receive scheduler run records"
    );
}

#[tokio::test(start_paused = true)]
async fn budget_exhausted_skip_runs_do_not_count_against_daily_generation_cap() {
    let pm = MockPm::new().arc();
    let now = 1_782_950_000;
    let mut spec = loop_spec("4f47ac10b58cc4372", 60);
    spec.governors.max_generations_per_day = Some(1);
    let loop_issue_id = create_loop_issue(pm.as_ref(), &spec, Some(now - 1), Vec::new()).await;
    seed_loop_run(
        pm.as_ref(),
        &loop_issue_id,
        &spec.loop_id,
        1,
        "approved",
        now - 90_000,
    )
    .await;
    seed_loop_run(
        pm.as_ref(),
        &loop_issue_id,
        &spec.loop_id,
        2,
        "budget_exhausted",
        now - 120,
    )
    .await;
    seed_loop_run(
        pm.as_ref(),
        &loop_issue_id,
        &spec.loop_id,
        3,
        "budget_exhausted",
        now - 60,
    )
    .await;
    let (continuation_ctx, mut continuation_rx) = capture_continuations();
    let reconciler = reconciler(
        Arc::clone(&pm),
        ReconcilerConfig::default(),
        now as u64,
        continuation_ctx,
    );

    let did_work = reconciler.tick_once().await.expect("tick_once");

    assert!(did_work, "due loop should count as scheduler work");
    let (continuation, _worker_session) = continuation_rx
        .try_recv()
        .expect("recent budget skips must not suppress loop-due continuation");
    assert_eq!(continuation.source, ContinuationSource::LoopDue);
    assert_eq!(
        loop_next_run(pm.as_ref(), &loop_issue_id).await,
        Some(now + 60)
    );
}

#[tokio::test(start_paused = true)]
async fn live_generation_causes_skipped_overlap_run_record_and_rearm() {
    let pm = MockPm::new().arc();
    let now = 1_782_950_000;
    let spec = loop_spec("2f47ac10b58cc4372", 300);
    let loop_issue_id = create_loop_issue(pm.as_ref(), &spec, Some(now - 1), Vec::new()).await;
    create_generation_epic(pm.as_ref(), &spec.loop_id, 3, "open").await;
    let (continuation_ctx, mut continuation_rx) = capture_continuations();
    let reconciler = reconciler(
        Arc::clone(&pm),
        ReconcilerConfig::default(),
        now as u64,
        continuation_ctx,
    );

    let did_work = reconciler.tick_once().await.expect("tick_once");

    assert!(did_work, "overlap skip should count as scheduler work");
    assert!(
        continuation_rx.try_recv().is_err(),
        "overlap skip must not push a loop-due continuation"
    );
    assert_eq!(
        loop_next_run(pm.as_ref(), &loop_issue_id).await,
        Some(now + 300)
    );
    let runs = loop_runs(pm.as_ref(), &loop_issue_id).await;
    assert_eq!(runs.len(), 1);
    assert!(matches!(
        &runs[0],
        AuditSentinelKind::LoopRun {
            loop_id,
            generation: 4,
            outcome,
            ..
        } if loop_id == &spec.loop_id && outcome == "skipped_overlap"
    ));
}

#[tokio::test(start_paused = true)]
async fn paused_loop_and_global_pause_never_fire() {
    let now = 1_782_950_000;

    for (config, extra_labels, assertion) in [
        (
            ReconcilerConfig {
                loops_enabled: false,
                ..Default::default()
            },
            Vec::new(),
            "disabled loops",
        ),
        (
            ReconcilerConfig::default(),
            vec![labels::LOOP_PAUSED.to_string()],
            "loop pause",
        ),
        (
            ReconcilerConfig {
                pause_all_loops: true,
                ..Default::default()
            },
            Vec::new(),
            "global pause",
        ),
    ] {
        let pm = MockPm::new().arc();
        let spec = loop_spec(&assertion.replace(' ', ""), 60);
        let loop_issue_id =
            create_loop_issue(pm.as_ref(), &spec, Some(now - 1), extra_labels).await;
        let (continuation_ctx, mut continuation_rx) = capture_continuations();
        let reconciler = reconciler(Arc::clone(&pm), config, now as u64, continuation_ctx);

        reconciler.tick_once().await.expect("tick_once");

        assert!(
            continuation_rx.try_recv().is_err(),
            "{assertion} must not push a continuation"
        );
        assert_eq!(
            loop_next_run(pm.as_ref(), &loop_issue_id).await,
            Some(now - 1),
            "{assertion} must not re-arm"
        );
        assert!(
            loop_runs(pm.as_ref(), &loop_issue_id).await.is_empty(),
            "{assertion} must not record a run"
        );
    }
}
