//! B0 regression: `run_plan` must not exit while any task is in
//! `AwaitingReview`. Before B0, the loop broke on `in_flight.is_empty()`,
//! which fires as soon as the last dispatched task transitions to
//! `AwaitingReview` via `spawn_completion_future`. DN-6 exit cleanup
//! then stamped that task as `Failed` with error
//! "Plan exited with task awaiting review" — even though the brain
//! was about to approve it. See docs/rca/2026-04-19-phase3a-worker-dispatch-failure-modes.md.
//!
//! This test builds a single-task plan with the task pre-set to
//! `AwaitingReview`, runs `run_plan` for long enough that any premature
//! exit would fire DN-6 cleanup, and asserts the task is still
//! `AwaitingReview` (not `Failed`). Without B0 this panics; with B0 it
//! passes because `run_plan` stays in its polling wait.

use spur_mcp::plan::{run_plan, PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

mod common;

fn test_materializer() -> Arc<spur_mcp::outcome_materializer::OutcomeMaterializer> {
    Arc::new(spur_mcp::outcome_materializer::OutcomeMaterializer::new(
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
    ))
}

#[tokio::test]
async fn run_plan_stays_alive_while_task_awaiting_review() {
    let state = PlanState {
        plan_id: "b0-test".into(),
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("b".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: spur_mcp::plan::PlanMergeState::NotStarted,
        epic_id: None,
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "t0".into(),
                agent: "a".into(),
                task: "T0".into(),
                depends_on: vec![],
                issue_id: None,
                context_files: vec![],
            },
            status: PlanTaskStatus::AwaitingReview { summary: None },
            result: None,
            worker_branch: None,
            attempt: 1,
            history: vec![],
            last_delegation_id: None,
            dispatched_base_oid: None,
        }],
    };

    let plan = Arc::new(Mutex::new(state));
    let (delegation_tx, _delegation_rx) = mpsc::channel(8);

    // Spawn run_plan as a background task — it must NOT exit on its own
    // while the task is AwaitingReview.
    let plan_for_runner = Arc::clone(&plan);
    let runner = tokio::spawn(async move {
        run_plan(
            plan_for_runner,
            delegation_tx,
            None,
            None,
            None,
            test_materializer(),
            common::server_builder::pro_feature_gate(),
        )
        .await;
    });

    // Park well past any reasonable "immediate exit" path. 500ms is
    // generous — without B0 the loop exits in microseconds and DN-6
    // cleanup fires synchronously.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The task MUST still be AwaitingReview. If run_plan exited and DN-6
    // fired, it would be Failed with "Plan exited with task awaiting review".
    let status = {
        let p = plan.lock().await;
        p.tasks[0].status.clone()
    };
    assert!(
        matches!(status, PlanTaskStatus::AwaitingReview { .. }),
        "B0 regression: run_plan exited prematurely and DN-6 stamped \
         the AwaitingReview task; current status = {:?}",
        status
    );

    // run_plan is still alive (polling). Dropping the delegation channel
    // is fine — the loop never tries to send on it while waiting.
    runner.abort();
    let _ = runner.await;
}
