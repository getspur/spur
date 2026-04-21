use spur_acp::domain::delegation::{DelegationResult, DelegationStatus};
use spur_acp::domain::events::SpurEventBody;
use spur_mcp::plan::{run_plan, PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
use spur_mcp::McpEventSink;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// A test sink that captures emitted event bodies synchronously.
struct CaptureSink {
    events: std::sync::Mutex<Vec<spur_acp::SpurEvent>>,
}
impl McpEventSink for CaptureSink {
    fn emit(&self, body: SpurEventBody) {
        self.events
            .lock()
            .unwrap()
            .push(spur_acp::SpurEvent::now(body));
    }
}

#[tokio::test]
async fn test_non_cascade_on_dep() {
    // Tests (a) Cancelled dep allows dispatch of dependent
    // build a PlanState with task A in Cancelled state and task B depending on A in Pending state
    let state = PlanState {
        plan_id: "p1".into(),
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("b".into())),
        base_snapshot_branch: None,
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
                    context_files: vec![],
                },
                status: PlanTaskStatus::AwaitingReview { summary: None },
                result: None,
                worker_branch: None,
                attempt: 1,
                history: vec![],
                last_delegation_id: None,
            },
            PlanTaskEntry {
                spec: PlanTask {
                    task_id: "t1".into(),
                    agent: "a".into(),
                    task: "T1".into(),
                    depends_on: vec![],
                    issue_id: None,
                    context_files: vec![],
                },
                status: PlanTaskStatus::Cancelled { reason: "x".into() },
                result: None,
                worker_branch: None,
                attempt: 1,
                history: vec![],
                last_delegation_id: None,
            },
            PlanTaskEntry {
                spec: PlanTask {
                    task_id: "t2".into(),
                    agent: "a".into(),
                    task: "T2".into(),
                    depends_on: vec!["t1".into(), "t0".into()],
                    issue_id: None,
                    context_files: vec![],
                },
                status: PlanTaskStatus::Pending,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: vec![],
                last_delegation_id: None,
            },
        ],
    };

    let (dtx, mut drx) = mpsc::channel(8);
    let tracker = tokio_util::task::TaskTracker::new();
    let plan = Arc::new(Mutex::new(state));

    // Call handle_review_task which will call dispatch_newly_ready.
    // Approving t0 will make it Approved.
    // Then dispatch_newly_ready scans all Pending tasks.
    // It sees t2, which depends on t1 (Cancelled) and t0 (now Approved).
    // Because Cancelled is accepted as a satisfied dependency, t2 should be dispatched.
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
    )
    .await
    .unwrap();

    let req2 = drx.recv().await.expect("t2 should be dispatched");
    assert_eq!(req2.task, "T2");
}

#[tokio::test]
async fn test_delegation_cancelled_result_does_not_cascade() {
    // Tests (b) DelegationStatus::Cancelled result sets PlanTaskStatus::Cancelled
    // without marking transitioned_to_failed (i.e. doesn't cascade fail dependents).
    let state = PlanState {
        plan_id: "p1".into(),
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("b".into())),
        base_snapshot_branch: None,
        merge_state: spur_mcp::plan::PlanMergeState::NotStarted,
        epic_id: None,
        tasks: vec![
            PlanTaskEntry {
                spec: PlanTask {
                    task_id: "t1".into(),
                    agent: "a".into(),
                    task: "T1".into(),
                    depends_on: vec![],
                    issue_id: None,
                    context_files: vec![],
                },
                status: PlanTaskStatus::Pending,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: vec![],
                last_delegation_id: None,
            },
            PlanTaskEntry {
                spec: PlanTask {
                    task_id: "t2".into(),
                    agent: "a".into(),
                    task: "T2".into(),
                    depends_on: vec!["t1".into()],
                    issue_id: None,
                    context_files: vec![],
                },
                status: PlanTaskStatus::Pending,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: vec![],
                last_delegation_id: None,
            },
        ],
    };

    let (dtx, mut drx) = mpsc::channel(8);
    let plan = Arc::new(Mutex::new(state));
    let plan_clone = plan.clone();

    let handle = tokio::spawn(async move {
        run_plan(plan_clone, dtx, None, None).await;
    });

    // t1 should be dispatched immediately since it has no deps.
    let req1 = drx.recv().await.expect("t1 should be dispatched");
    assert_eq!(req1.task, "T1");

    // Complete t1 with Cancelled
    let _ = req1.respond_to.send(DelegationResult {
        status: DelegationStatus::Cancelled {
            reason: "test reason".into(),
        },
        worker_branch: None,
        summary: None,
        diff: None,
        diff_summary: None,
        estimated_cost_usd: 0.0,
        artifact: None,
    });

    // Wait briefly to allow run_plan to process the completion
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // drx should be closed without t2 being dispatched,
    // OR it just hangs if run_plan has a loop bug.
    // We just want to check the final state.
    handle.abort(); // Since run_plan might loop infinitely because t2 is pending

    let locked = plan.lock().await;
    assert!(matches!(
        locked.tasks[0].status,
        PlanTaskStatus::Cancelled { .. }
    ));
    // DN-4 core invariant: Cancelled does NOT cascade via
    // `mark_descendants_failed`. Concretely: t2 is NEVER promoted to
    // Failed with `error == "Blocked by failed dependency"`. t2 may be
    // Pending, Dispatched (because DN-4's dispatch_newly_ready treats
    // Cancelled deps as satisfied), or Failed for *other* reasons
    // (e.g. a later run_plan exit cleanup stamping a different error) —
    // all of those are fine. What's forbidden is the cascade error.
    match &locked.tasks[1].status {
        PlanTaskStatus::Failed { error } => {
            assert_ne!(
                error, "Blocked by failed dependency",
                "Cancelled dep must not cascade t2 via mark_descendants_failed"
            );
        }
        PlanTaskStatus::Pending
        | PlanTaskStatus::Dispatched { .. }
        | PlanTaskStatus::AwaitingReview { .. }
        | PlanTaskStatus::Approved { .. }
        | PlanTaskStatus::Rejected { .. }
        | PlanTaskStatus::Cancelled { .. }
        | PlanTaskStatus::Superseded { .. }
        | PlanTaskStatus::Ready => {}
    }
}

#[tokio::test]
async fn test_plan_ready_to_merge_blocked_by_cancelled_and_count() {
    // Tests (c) PlanCompleted event emitted with cancelled count > 0
    // and (d) PlanReadyToMerge NOT emitted when any task is Cancelled.
    let state = PlanState {
        plan_id: "p2".into(),
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("b".into())),
        base_snapshot_branch: None,
        merge_state: spur_mcp::plan::PlanMergeState::NotStarted,
        epic_id: None,
        tasks: vec![
            PlanTaskEntry {
                spec: PlanTask {
                    task_id: "t1".into(),
                    agent: "a".into(),
                    task: "T".into(),
                    depends_on: vec![],
                    issue_id: None,
                    context_files: vec![],
                },
                status: PlanTaskStatus::Approved { summary: None },
                result: None,
                worker_branch: None,
                attempt: 1,
                history: vec![],
                last_delegation_id: None,
            },
            PlanTaskEntry {
                spec: PlanTask {
                    task_id: "t2".into(),
                    agent: "a".into(),
                    task: "T".into(),
                    depends_on: vec![],
                    issue_id: None,
                    context_files: vec![],
                },
                status: PlanTaskStatus::Cancelled { reason: "x".into() },
                result: None,
                worker_branch: None,
                attempt: 1,
                history: vec![],
                last_delegation_id: None,
            },
        ],
    };

    let sink = Arc::new(CaptureSink {
        events: std::sync::Mutex::new(Vec::new()),
    });
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;

    let (dtx, _drx) = mpsc::channel(8);

    // run_plan will immediately exit since both tasks are terminal
    run_plan(Arc::new(Mutex::new(state)), dtx, Some(sink_ref), None).await;

    let events = sink.events.lock().unwrap();

    let mut saw_completed = false;
    let mut saw_ready_to_merge = false;

    for e in events.iter() {
        match &e.body {
            SpurEventBody::PlanCompleted {
                approved,
                cancelled,
                ..
            } => {
                saw_completed = true;
                assert_eq!(*approved, 1);
                assert_eq!(*cancelled, 1);
            }
            SpurEventBody::PlanReadyToMerge { .. } => {
                saw_ready_to_merge = true;
            }
            _ => {}
        }
    }

    assert!(saw_completed, "PlanCompleted should be emitted");
    assert!(
        !saw_ready_to_merge,
        "PlanReadyToMerge should NOT be emitted because of the cancelled task"
    );
}
