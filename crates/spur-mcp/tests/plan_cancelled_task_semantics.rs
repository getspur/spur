use spur_mcp::plan::{PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

mod common;

#[tokio::test]
async fn approve_does_not_enqueue_new_dispatches() {
    // Persisted-authority flip: approval must only persist the decision.
    // The reconciler, not review_task, owns any follow-up dispatch.
    let state = PlanState {
        plan_id: "p1".into(),
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("b".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: spur_mcp::plan::PlanMergeState::NotStarted,
        epic_id: None,
        tasks: vec![
            PlanTaskEntry {
                spec: PlanTask {
                    task_id: "t0".into(),
                    agent: "a".into(),
                    task: "T0".into(),
                    depends_on: vec![],
                    issue_id: None,
                    issue_title: None,
                    context_files: vec![],
                },
                status: PlanTaskStatus::AwaitingReview { summary: None },
                result: None,
                worker_branch: None,
                attempt: 1,
                history: vec![],
                last_delegation_id: None,
                dispatched_base_oid: None,
            },
            PlanTaskEntry {
                spec: PlanTask {
                    task_id: "t1".into(),
                    agent: "a".into(),
                    task: "T1".into(),
                    depends_on: vec![],
                    issue_id: None,
                    issue_title: None,
                    context_files: vec![],
                },
                status: PlanTaskStatus::Cancelled { reason: "x".into() },
                result: None,
                worker_branch: None,
                attempt: 1,
                history: vec![],
                last_delegation_id: None,
                dispatched_base_oid: None,
            },
            PlanTaskEntry {
                spec: PlanTask {
                    task_id: "t2".into(),
                    agent: "a".into(),
                    task: "T2".into(),
                    depends_on: vec!["t1".into(), "t0".into()],
                    issue_id: None,
                    issue_title: None,
                    context_files: vec![],
                },
                status: PlanTaskStatus::Pending,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: vec![],
                last_delegation_id: None,
                dispatched_base_oid: None,
            },
        ],
    };

    let (dtx, mut drx) = mpsc::channel(8);
    let tracker = tokio_util::task::TaskTracker::new();
    let plan = Arc::new(Mutex::new(state));

    let _ = spur_mcp::plan::handle_review_task(
        plan.clone(),
        "p1",
        "t0",
        "approve",
        None,
        None,
        None,
        Some(&dtx),
        Some(&tracker),
        common::server_builder::pro_feature_gate(),
    )
    .await
    .unwrap();

    assert!(
        matches!(
            drx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ),
        "approve must not enqueue a follow-up dispatch"
    );

    let locked = plan.lock().await;
    assert!(
        matches!(locked.tasks[2].status, PlanTaskStatus::Ready),
        "approve should leave newly-unblocked dependents ready for the reconciler"
    );
}
