use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};
use spur_acp::{
    PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask, SessionId, SpurEvent, SpurEventBody,
};
use spur_core::{ExecutorLineage, PlanProjectionStore, SessionSynopsisProjection};
use spur_tui::action::Action;
use spur_tui::app::BrainStatus;
use spur_tui::views::plan_inspector::PlanInspectorView;
use spur_tui::views::{View, ViewContext};

fn sample_plan_store() -> PlanProjectionStore {
    let mut store = PlanProjectionStore::default();
    store.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
        session_id: SessionId("brain-1".into()),
        snapshot: Box::new(PlanSnapshot {
            plan_id: "plan-1".into(),
            status: "running".into(),
            progress: "1/4 done".into(),
            next_action:
                "Use get_task_diff to review each awaiting task, then review_task to approve or reject."
                    .into(),
            ready_to_merge: false,
            counts: PlanSnapshotCounts {
                pending: 2,
                approved: 1,
                awaiting_review: 1,
                ..Default::default()
            },
            tasks: vec![
                task("task-contract", 0, &[], Some("bd-1"), "approved"),
                task(
                    "task-projection",
                    1,
                    &["task-contract"],
                    Some("bd-2"),
                    "awaiting_review",
                ),
                task(
                    "task-app",
                    2,
                    &["task-projection"],
                    Some("bd-3"),
                    "dispatched",
                ),
                task(
                    "task-inspector",
                    3,
                    &["task-contract", "task-app"],
                    Some("bd-4"),
                    "pending",
                ),
            ],
        }),
    }));
    store
}

fn out_of_stage_order_plan_store() -> PlanProjectionStore {
    let mut store = PlanProjectionStore::default();
    store.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
        session_id: SessionId("brain-1".into()),
        snapshot: Box::new(PlanSnapshot {
            plan_id: "plan-1".into(),
            status: "running".into(),
            progress: "1/4 done".into(),
            next_action:
                "Use get_task_diff to review each awaiting task, then review_task to approve or reject."
                    .into(),
            ready_to_merge: false,
            counts: PlanSnapshotCounts {
                pending: 2,
                approved: 1,
                awaiting_review: 1,
                ..Default::default()
            },
            tasks: vec![
                task(
                    "task-projection",
                    1,
                    &["task-contract"],
                    Some("bd-2"),
                    "awaiting_review",
                ),
                task("task-contract", 0, &[], Some("bd-1"), "approved"),
                task(
                    "task-app",
                    2,
                    &["task-projection"],
                    Some("bd-3"),
                    "dispatched",
                ),
                task(
                    "task-inspector",
                    3,
                    &["task-contract", "task-app"],
                    Some("bd-4"),
                    "pending",
                ),
            ],
        }),
    }));
    store
}

fn task(
    task_id: &str,
    stage_idx: usize,
    depends_on: &[&str],
    issue_id: Option<&str>,
    status: &str,
) -> PlanSnapshotTask {
    let blocked_by = if stage_idx == 0 {
        Vec::new()
    } else {
        depends_on.iter().map(|dep| dep.to_string()).collect()
    };
    PlanSnapshotTask {
        task_id: task_id.into(),
        task_name: task_id.into(),
        agent: "codex".into(),
        issue_id: issue_id.map(str::to_string),
        status: status.into(),
        attempt: if status == "dispatched" { 1 } else { 0 },
        max_attempts: 3,
        depends_on: depends_on.iter().map(|dep| dep.to_string()).collect(),
        blocked_by,
        unblocks: Vec::new(),
        summary: None,
        feedback: None,
        error: None,
        worker_branch: None,
        delegation_id: if status == "dispatched" {
            Some("del-1".into())
        } else {
            None
        },
        diff_summary: None,
        mutation_id: None,
        superseded_by: Vec::new(),
        next_action: if status == "awaiting_review" {
            "review".into()
        } else {
            "wait".into()
        },
    }
}

fn sample_lineage() -> ExecutorLineage {
    use spur_acp::{LifecycleState, Role};

    let mut lineage = ExecutorLineage::new();
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "exec-1".into(),
        parent_id: None,
        session_id: SessionId("worker-1".into()),
        agent: "codex".into(),
        role: Role::Executor,
        task_spec: "placeholder".into(),
    }));
    lineage.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
        from: SessionId("brain-1".into()),
        to_agent: "codex".into(),
        task: "task-app".into(),
        request_id: "req-1".into(),
        delegation_plan: None,
        issue_id: Some("bd-3".into()),
    }));
    lineage.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: SessionId("brain-1".into()),
        request_id: "req-1".into(),
        executor_id: "exec-1".into(),
    }));
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
        id: "exec-1".into(),
        phase: LifecycleState::Running,
    }));
    lineage
}

fn lineage_with_stale_and_live_executor_for_same_issue() -> ExecutorLineage {
    use spur_acp::{LifecycleState, Role};

    let mut lineage = ExecutorLineage::new();
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "exec-z".into(),
        parent_id: None,
        session_id: SessionId("worker-z".into()),
        agent: "codex".into(),
        role: Role::Executor,
        task_spec: "stale".into(),
    }));
    lineage.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
        from: SessionId("brain-1".into()),
        to_agent: "codex".into(),
        task: "task-app".into(),
        request_id: "req-z".into(),
        delegation_plan: None,
        issue_id: Some("bd-3".into()),
    }));
    lineage.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: SessionId("brain-1".into()),
        request_id: "req-z".into(),
        executor_id: "exec-z".into(),
    }));
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
        id: "exec-z".into(),
        phase: LifecycleState::Succeeded,
    }));

    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "exec-a".into(),
        parent_id: None,
        session_id: SessionId("worker-a".into()),
        agent: "codex".into(),
        role: Role::Executor,
        task_spec: "live".into(),
    }));
    lineage.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
        from: SessionId("brain-1".into()),
        to_agent: "codex".into(),
        task: "task-app".into(),
        request_id: "req-a".into(),
        delegation_plan: None,
        issue_id: Some("bd-3".into()),
    }));
    lineage.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: SessionId("brain-1".into()),
        request_id: "req-a".into(),
        executor_id: "exec-a".into(),
    }));
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
        id: "exec-a".into(),
        phase: LifecycleState::Running,
    }));
    lineage
}

fn plan_store_with_selected_task_app() -> PlanProjectionStore {
    let mut store = PlanProjectionStore::default();
    store.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
        session_id: SessionId("brain-1".into()),
        snapshot: Box::new(PlanSnapshot {
            plan_id: "plan-1".into(),
            status: "running".into(),
            progress: "1/1 done".into(),
            next_action:
                "Use get_task_diff to review each awaiting task, then review_task to approve or reject."
                    .into(),
            ready_to_merge: false,
            counts: PlanSnapshotCounts {
                dispatched: 1,
                ..Default::default()
            },
            tasks: vec![task("task-app", 0, &[], Some("bd-3"), "dispatched")],
        }),
    }));
    store
}

fn buffer_contains(buf: &Buffer, needle: &str) -> bool {
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }
    s.contains(needle)
}

#[test]
fn plan_inspector_renders_wide_lane_board() {
    let backend = TestBackend::new(120, 32);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view = PlanInspectorView::new(SessionId("brain-1".into()));
    let plans = sample_plan_store();
    let lineage = sample_lineage();
    let synopsis = SessionSynopsisProjection::new();
    let ctx = ViewContext {
        lineage: &lineage,
        plan_projection: &plans,
        synopsis: &synopsis,
        brain_status: &BrainStatus::Idle,
        license_badge: None,
        flag_summary: None,
        tombstone: None,
        transient_hint_override: None,
    };

    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    assert!(buffer_contains(&buffer, "Stage 0"));
    assert!(buffer_contains(&buffer, "Stage 1"));
    assert!(buffer_contains(&buffer, "Task detail"));
    assert!(buffer_contains(&buffer, "codex:run"));
}

#[test]
fn plan_inspector_renders_stacked_layout_below_90_cols() {
    let backend = TestBackend::new(80, 28);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view = PlanInspectorView::new(SessionId("brain-1".into()));
    let plans = sample_plan_store();
    let lineage = sample_lineage();
    let synopsis = SessionSynopsisProjection::new();
    let ctx = ViewContext {
        lineage: &lineage,
        plan_projection: &plans,
        synopsis: &synopsis,
        brain_status: &BrainStatus::Idle,
        license_badge: None,
        flag_summary: None,
        tombstone: None,
        transient_hint_override: None,
    };

    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    assert!(buffer_contains(&buffer, "Selected:"));
    assert!(buffer_contains(&buffer, "Stage 1"));
}

#[test]
fn plan_inspector_stacked_mode_j_moves_across_stage_boundaries() {
    let backend = TestBackend::new(80, 28);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view = PlanInspectorView::new(SessionId("brain-1".into()));
    let plans = sample_plan_store();
    let lineage = sample_lineage();
    let synopsis = SessionSynopsisProjection::new();
    let ctx = ViewContext {
        lineage: &lineage,
        plan_projection: &plans,
        synopsis: &synopsis,
        brain_status: &BrainStatus::Idle,
        license_badge: None,
        flag_summary: None,
        tombstone: None,
        transient_hint_override: None,
    };

    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let action = view.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), &ctx);
    assert!(action.is_none(), "navigation should remain in-view");
    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    assert!(
        buffer_contains(&buffer, "Selected: task-projection"),
        "stacked navigation should move to the next visible task across stages"
    );
}

#[test]
fn plan_inspector_stacked_mode_uses_visible_stage_order_when_tasks_are_permuted() {
    let backend = TestBackend::new(80, 28);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view = PlanInspectorView::new(SessionId("brain-1".into()));
    let plans = out_of_stage_order_plan_store();
    let lineage = sample_lineage();
    let synopsis = SessionSynopsisProjection::new();
    let ctx = ViewContext {
        lineage: &lineage,
        plan_projection: &plans,
        synopsis: &synopsis,
        brain_status: &BrainStatus::Idle,
        license_badge: None,
        flag_summary: None,
        tombstone: None,
        transient_hint_override: None,
    };

    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let initial = terminal.backend().buffer().clone();
    assert!(
        buffer_contains(&initial, "Selected: task-contract"),
        "stacked mode should seed selection from the first visible task, not the raw vector order"
    );

    let action = view.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), &ctx);
    assert!(action.is_none(), "navigation should remain in-view");
    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let after_j = terminal.backend().buffer().clone();
    assert!(
        buffer_contains(&after_j, "Selected: task-projection"),
        "stacked mode j/k navigation should follow the rendered stage-grouped order"
    );
}

#[test]
fn plan_inspector_alt_p_requests_navigate_back() {
    let mut view = PlanInspectorView::new(SessionId("brain-1".into()));
    let plans = sample_plan_store();
    let lineage = sample_lineage();
    let synopsis = SessionSynopsisProjection::new();
    let ctx = ViewContext {
        lineage: &lineage,
        plan_projection: &plans,
        synopsis: &synopsis,
        brain_status: &BrainStatus::Idle,
        license_badge: None,
        flag_summary: None,
        tombstone: None,
        transient_hint_override: None,
    };

    let action = view.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT), &ctx);
    assert!(matches!(action, Some(Action::NavigateBack)));
}

#[test]
fn plan_inspector_esc_requests_navigate_back() {
    let mut view = PlanInspectorView::new(SessionId("brain-1".into()));
    let plans = sample_plan_store();
    let lineage = sample_lineage();
    let synopsis = SessionSynopsisProjection::new();
    let ctx = ViewContext {
        lineage: &lineage,
        plan_projection: &plans,
        synopsis: &synopsis,
        brain_status: &BrainStatus::Idle,
        license_badge: None,
        flag_summary: None,
        tombstone: None,
        transient_hint_override: None,
    };

    let action = view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &ctx);
    assert!(matches!(action, Some(Action::NavigateBack)));
}

#[test]
fn plan_inspector_prefers_live_executor_state_over_stale_higher_id() {
    let backend = TestBackend::new(120, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view = PlanInspectorView::new(SessionId("brain-1".into()));
    let plans = plan_store_with_selected_task_app();
    let lineage = lineage_with_stale_and_live_executor_for_same_issue();
    let synopsis = SessionSynopsisProjection::new();
    let ctx = ViewContext {
        lineage: &lineage,
        plan_projection: &plans,
        synopsis: &synopsis,
        brain_status: &BrainStatus::Idle,
        license_badge: None,
        flag_summary: None,
        tombstone: None,
        transient_hint_override: None,
    };

    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    assert!(
        buffer_contains(&buffer, "codex:run"),
        "board should surface the live executor rather than a stale higher-id executor"
    );
    assert!(
        buffer_contains(&buffer, "codex running"),
        "detail pane should join against the live executor for the selected task"
    );
}

#[test]
fn plan_inspector_renders_blocked_deps_and_retry_chips() {
    let backend = TestBackend::new(160, 32);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view = PlanInspectorView::new(SessionId("brain-1".into()));
    let plans = plan_store_with_blocked_and_retry();
    let lineage = sample_lineage();
    let synopsis = SessionSynopsisProjection::new();
    let ctx = ViewContext {
        lineage: &lineage,
        plan_projection: &plans,
        synopsis: &synopsis,
        brain_status: &BrainStatus::Idle,
        license_badge: None,
        flag_summary: None,
        tombstone: None,
        transient_hint_override: None,
    };

    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    // Navigate to Stage 1 (child task) so detail pane shows blocked_by.
    let action = view.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE), &ctx);
    assert!(action.is_none(), "navigation should remain in-view");
    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    assert!(
        buffer_contains(&buffer, "blocked:"),
        "board should show blocked chip for tasks with unsatisfied dependencies"
    );
    assert!(
        buffer_contains(&buffer, "↑parent"),
        "board should show upstream dependency hint"
    );
    assert!(
        buffer_contains(&buffer, "retry 2/3"),
        "board should show retry chip when attempt > 1"
    );
    assert!(
        buffer_contains(&buffer, "blocked_by:"),
        "detail pane should render blocked_by with prominent styling"
    );
}

#[test]
fn plan_inspector_stacked_mode_shows_meta_chips() {
    let backend = TestBackend::new(80, 28);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view = PlanInspectorView::new(SessionId("brain-1".into()));
    let plans = plan_store_with_blocked_and_retry();
    let lineage = sample_lineage();
    let synopsis = SessionSynopsisProjection::new();
    let ctx = ViewContext {
        lineage: &lineage,
        plan_projection: &plans,
        synopsis: &synopsis,
        brain_status: &BrainStatus::Idle,
        license_badge: None,
        flag_summary: None,
        tombstone: None,
        transient_hint_override: None,
    };

    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    assert!(
        buffer_contains(&buffer, "blocked:"),
        "stacked board should show blocked chip"
    );
    assert!(
        buffer_contains(&buffer, "↑parent"),
        "stacked board should show dependency hint"
    );
    assert!(
        buffer_contains(&buffer, "retry 2/3"),
        "stacked board should show retry chip"
    );
}

fn plan_store_with_blocked_and_retry() -> PlanProjectionStore {
    let mut store = PlanProjectionStore::default();
    store.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
        session_id: SessionId("brain-1".into()),
        snapshot: Box::new(PlanSnapshot {
            plan_id: "plan-1".into(),
            status: "running".into(),
            progress: "0/2 done".into(),
            next_action: "waiting for dependencies".into(),
            ready_to_merge: false,
            counts: PlanSnapshotCounts {
                pending: 2,
                ..Default::default()
            },
            tasks: vec![
                PlanSnapshotTask {
                    task_id: "parent".into(),
                    task_name: "parent".into(),
                    agent: "codex".into(),
                    issue_id: Some("bd-parent".into()),
                    status: "approved".into(),
                    attempt: 1,
                    max_attempts: 3,
                    depends_on: vec![],
                    blocked_by: vec![],
                    unblocks: vec!["child".into()],
                    summary: None,
                    feedback: None,
                    error: None,
                    worker_branch: None,
                    delegation_id: None,
                    diff_summary: None,
                    mutation_id: None,
                    superseded_by: vec![],
                    next_action: "".into(),
                },
                PlanSnapshotTask {
                    task_id: "child".into(),
                    task_name: "child".into(),
                    agent: "kiro".into(),
                    issue_id: Some("bd-child".into()),
                    status: "pending".into(),
                    attempt: 2,
                    max_attempts: 3,
                    depends_on: vec!["parent".into()],
                    blocked_by: vec!["parent".into()],
                    unblocks: vec![],
                    summary: None,
                    feedback: None,
                    error: None,
                    worker_branch: None,
                    delegation_id: None,
                    diff_summary: None,
                    mutation_id: None,
                    superseded_by: vec![],
                    next_action: "wait".into(),
                },
            ],
        }),
    }));
    store
}
