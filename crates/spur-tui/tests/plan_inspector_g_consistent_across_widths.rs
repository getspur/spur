use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};
use spur_acp::{
    PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask, SessionId, SpurEvent, SpurEventBody,
};
use spur_core::{ExecutorLineage, PlanProjectionStore, SessionSynopsisProjection};
use spur_tui::app::BrainStatus;
use spur_tui::views::plan_inspector::PlanInspectorView;
use spur_tui::views::{View, ViewContext};

fn plan_store() -> PlanProjectionStore {
    let mut store = PlanProjectionStore::default();
    store.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
        session_id: SessionId("brain-1".into()),
        snapshot: Box::new(PlanSnapshot {
            plan_id: "plan-gg".into(),
            epic_id: None,
            status: "running".into(),
            progress: "0/9 done".into(),
            next_action: "keep moving".into(),
            ready_to_merge: false,
            counts: PlanSnapshotCounts {
                pending: 9,
                ..Default::default()
            },
            tasks: vec![
                task("s0-a", 0),
                task("s0-b", 0),
                task("s0-c", 0),
                task("s1-a", 1),
                task("s1-b", 1),
                task("s1-c", 1),
                task("s2-a", 2),
                task("s2-b", 2),
                task("s2-c", 2),
            ],
            owner_brain_session_id: None,
            owner_token: None,
            owner_acquired_at: None,
        }),
    }));
    store
}

fn task(task_id: &str, stage_idx: usize) -> PlanSnapshotTask {
    let depends_on = match stage_idx {
        0 => Vec::new(),
        1 => vec!["s0-a".to_string()],
        2 => vec!["s1-a".to_string()],
        _ => Vec::new(),
    };
    PlanSnapshotTask {
        task_id: task_id.into(),
        task_name: task_id.into(),
        agent: "codex".into(),
        issue_id: Some(format!("bd-{task_id}")),
        status: "pending".into(),
        attempt: 0,
        max_attempts: 3,
        depends_on,
        blocked_by: Vec::new(),
        unblocks: Vec::new(),
        summary: None,
        feedback: None,
        error: None,
        worker_branch: None,
        delegation_id: None,
        diff_summary: None,
        mutation_id: None,
        superseded_by: Vec::new(),
        next_action: "wait".into(),
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn buffer_text(buf: &Buffer) -> String {
    let mut text = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            text.push_str(buf[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

fn assert_selected_task(buf: &Buffer, task_id: &str) {
    let text = buffer_text(buf);
    let needle = format!("{task_id} · codex");
    assert!(
        text.contains(&needle),
        "expected selected task detail to contain {needle:?}, got:\n{text}"
    );
}

fn assert_g_and_shift_g_are_lane_local_at_width(width: u16) {
    let backend = TestBackend::new(width, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view = PlanInspectorView::new(SessionId("brain-1".into()));
    let plans = plan_store();
    let lineage = ExecutorLineage::new();
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
        theme: spur_tui::theme::fallback_theme(),
    };

    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let _ = view.handle_key(key(KeyCode::Char('l')), &ctx);
    let _ = view.handle_key(key(KeyCode::Char('j')), &ctx);
    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    assert_selected_task(terminal.backend().buffer(), "s1-b");

    // g/G are intentionally lane-local at every width. The width-dependent
    // j/k stacked-mode behavior is a separate follow-up, not part of this
    // regression pin.
    let action = view.handle_key(key(KeyCode::Char('g')), &ctx);
    assert!(action.is_none(), "g should remain in-view");
    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    assert_selected_task(terminal.backend().buffer(), "s1-a");

    let action = view.handle_key(key(KeyCode::Char('G')), &ctx);
    assert!(action.is_none(), "G should remain in-view");
    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    assert_selected_task(terminal.backend().buffer(), "s1-c");
}

#[test]
fn g_and_shift_g_are_lane_local_at_narrow_width() {
    assert_g_and_shift_g_are_lane_local_at_width(50);
}

#[test]
fn g_and_shift_g_are_lane_local_at_wide_width() {
    assert_g_and_shift_g_are_lane_local_at_width(120);
}
