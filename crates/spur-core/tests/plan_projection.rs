use spur_acp::{
    PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask, SessionId, SpurEvent, SpurEventBody,
};
use spur_core::PlanProjectionStore;

fn snapshot_event(session_id: &SessionId, snapshot: PlanSnapshot) -> SpurEvent {
    SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
        session_id: session_id.clone(),
        snapshot: Box::new(snapshot),
    })
}

fn sample_snapshot(plan_id: &str, status: &str, tasks: Vec<PlanSnapshotTask>) -> PlanSnapshot {
    PlanSnapshot {
        plan_id: plan_id.to_string(),
        epic_id: None,
        status: status.to_string(),
        progress: "1/1 done".to_string(),
        next_action: "review".to_string(),
        ready_to_merge: status == "approved",
        counts: PlanSnapshotCounts {
            pending: tasks.iter().filter(|task| task.status == "pending").count() as u32,
            ready: tasks.iter().filter(|task| task.status == "ready").count() as u32,
            dispatched: tasks
                .iter()
                .filter(|task| task.status == "dispatched")
                .count() as u32,
            awaiting_review: tasks
                .iter()
                .filter(|task| task.status == "awaiting_review")
                .count() as u32,
            approved: tasks
                .iter()
                .filter(|task| task.status == "approved")
                .count() as u32,
            rejected: tasks
                .iter()
                .filter(|task| task.status == "rejected")
                .count() as u32,
            failed: tasks.iter().filter(|task| task.status == "failed").count() as u32,
            cancelled: tasks
                .iter()
                .filter(|task| task.status == "cancelled")
                .count() as u32,
        },
        tasks,
        owner_brain_session_id: None,
        owner_token: None,
        owner_acquired_at: None,
    }
}

fn sample_task(task_id: &str, depends_on: &[&str], issue_id: Option<&str>) -> PlanSnapshotTask {
    PlanSnapshotTask {
        task_id: task_id.to_string(),
        task_name: task_id.to_string(),
        agent: "codex".to_string(),
        issue_id: issue_id.map(str::to_string),
        status: "pending".to_string(),
        attempt: 0,
        max_attempts: 3,
        depends_on: depends_on.iter().map(|dep| dep.to_string()).collect(),
        blocked_by: depends_on.iter().map(|dep| dep.to_string()).collect(),
        unblocks: Vec::new(),
        summary: None,
        feedback: None,
        error: None,
        worker_branch: None,
        delegation_id: None,
        diff_summary: None,
        mutation_id: None,
        superseded_by: Vec::new(),
        next_action: "wait".to_string(),
    }
}

#[test]
fn current_for_session_prefers_active_plan() {
    let session = SessionId("brain-1".into());
    let mut store = PlanProjectionStore::default();

    store.apply(&snapshot_event(
        &session,
        sample_snapshot(
            "p-old",
            "approved",
            vec![sample_task("t-old", &[], Some("bd-1"))],
        ),
    ));
    store.apply(&snapshot_event(
        &session,
        sample_snapshot(
            "p-new",
            "running",
            vec![sample_task("t-new", &[], Some("bd-2"))],
        ),
    ));

    let current = store.current_for_session(&session).expect("current plan");
    assert_eq!(current.plan_id, "p-new");
}

#[test]
fn current_for_session_keeps_active_plan_over_late_terminal_snapshot() {
    let session = SessionId("brain-1".into());
    let mut store = PlanProjectionStore::default();

    store.apply(&snapshot_event(
        &session,
        sample_snapshot(
            "p-active",
            "running",
            vec![sample_task("t-active", &[], Some("bd-1"))],
        ),
    ));
    store.apply(&snapshot_event(
        &session,
        sample_snapshot(
            "p-terminal",
            "approved",
            vec![sample_task("t-terminal", &[], Some("bd-2"))],
        ),
    ));

    let current = store.current_for_session(&session).expect("current plan");
    assert_eq!(current.plan_id, "p-active");
}

#[test]
fn projection_derives_stage_depth_from_dependencies() {
    let session = SessionId("brain-1".into());
    let mut store = PlanProjectionStore::default();

    store.apply(&snapshot_event(
        &session,
        sample_snapshot(
            "p-123",
            "running",
            vec![
                sample_task("task-contract", &[], Some("bd-1")),
                sample_task("task-projection", &["task-contract"], Some("bd-2")),
                sample_task("task-app", &["task-projection"], Some("bd-3")),
                sample_task(
                    "task-inspector",
                    &["task-contract", "task-app"],
                    Some("bd-4"),
                ),
            ],
        ),
    ));

    let plan = store.plan("p-123").expect("tracked plan");
    assert_eq!(plan.task("task-contract").unwrap().stage_idx, 0);
    assert_eq!(plan.task("task-projection").unwrap().stage_idx, 1);
    assert_eq!(plan.task("task-app").unwrap().stage_idx, 2);
    assert_eq!(plan.task("task-inspector").unwrap().stage_idx, 3);
}

#[test]
fn projection_preserves_issue_id_for_lineage_join() {
    let session = SessionId("brain-1".into());
    let mut store = PlanProjectionStore::default();

    store.apply(&snapshot_event(
        &session,
        sample_snapshot(
            "p-join",
            "running",
            vec![sample_task("task-join", &[], Some("bd-99"))],
        ),
    ));

    let plan = store.plan("p-join").expect("tracked plan");
    assert_eq!(
        plan.task("task-join").unwrap().issue_id.as_deref(),
        Some("bd-99")
    );
}
