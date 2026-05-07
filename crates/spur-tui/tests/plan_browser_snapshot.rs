//! Snapshot/state tests for the Sprints plan browser MVP.

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use spur_acp::{
    PlanLifecycleEvent, PlanLoadWarningEvent, PlanOwnerStateEvent, PlanSnapshot,
    PlanSnapshotCounts, PlanSummaryCountsEvent, PlanSummaryEvent, SessionId, SpurEvent,
    SpurEventBody,
};
use spur_core::{ExecutorLineage, PlanProjectionStore, SessionSynopsisProjection};
use spur_tui::action::Action;
use spur_tui::app::BrainStatus;
use spur_tui::views::plan_browser::PlanBrowserView;
use spur_tui::views::{View, ViewContext};

static BRAIN_STATUS: BrainStatus = BrainStatus::Idle;
static SYNOPSIS: std::sync::LazyLock<SessionSynopsisProjection> =
    std::sync::LazyLock::new(SessionSynopsisProjection::new);

fn view_ctx<'a>(lineage: &'a ExecutorLineage, plans: &'a PlanProjectionStore) -> ViewContext<'a> {
    ViewContext {
        lineage,
        plan_projection: plans,
        synopsis: &SYNOPSIS,
        brain_status: &BRAIN_STATUS,
        license_badge: None,
        flag_summary: None,
        tombstone: None,
        transient_hint_override: None,
        theme: spur_tui::theme::fallback_theme(),
    }
}

fn rendered_text(terminal: &Terminal<TestBackend>) -> String {
    let buf = terminal.backend().buffer();
    let mut rendered = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            rendered.push_str(buf[(x, y)].symbol());
        }
        rendered.push('\n');
    }
    rendered
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn plan_store_with_current(plan_id: &str) -> PlanProjectionStore {
    let mut store = PlanProjectionStore::default();
    store.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
        session_id: SessionId("brain-1".into()),
        snapshot: Box::new(PlanSnapshot {
            plan_id: plan_id.into(),
            epic_id: None,
            status: "running".into(),
            progress: "2/7 done".into(),
            next_action: "review next task".into(),
            ready_to_merge: false,
            counts: PlanSnapshotCounts {
                approved: 2,
                pending: 5,
                ..Default::default()
            },
            tasks: Vec::new(),
            owner_brain_session_id: None,
            owner_token: None,
            owner_acquired_at: None,
        }),
    }));
    store
}

fn summary(
    plan_id: &str,
    owner_state: PlanOwnerStateEvent,
    lifecycle: PlanLifecycleEvent,
    approved: u32,
    total: u32,
) -> PlanSummaryEvent {
    PlanSummaryEvent {
        plan_id: plan_id.into(),
        epic_id: format!("bd-{plan_id}"),
        title: format!("Plan {plan_id}"),
        source_body_preview: Some(format!(
            "Work item {plan_id} needs enough source context to recall the problem and why it matters."
        )),
        owner_state,
        lifecycle,
        counts: Some(PlanSummaryCountsEvent {
            total,
            pending: 0,
            ready: 0,
            running: 0,
            awaiting_review: 0,
            approved,
            rejected: 0,
            failed: 0,
            cancelled: 0,
        }),
        updated_at: Some(Utc::now()),
    }
}

fn loaded_event() -> SpurEvent {
    SpurEvent::now(SpurEventBody::PlansLoaded {
        plans: vec![
            summary(
                "plan-a1",
                PlanOwnerStateEvent::Mine,
                PlanLifecycleEvent::Running,
                2,
                7,
            ),
            summary(
                "plan-b2",
                PlanOwnerStateEvent::Unowned,
                PlanLifecycleEvent::Pending,
                0,
                3,
            ),
            summary(
                "plan-c3",
                PlanOwnerStateEvent::Other {
                    owner: "other-brain".into(),
                },
                PlanLifecycleEvent::Running,
                1,
                5,
            ),
            summary(
                "plan-d4",
                PlanOwnerStateEvent::Ambiguous {
                    owners: vec!["brain-a".into(), "brain-b".into()],
                },
                PlanLifecycleEvent::Unknown,
                0,
                0,
            ),
        ],
        warnings: Vec::new(),
    })
}

#[test]
fn renders_all_owner_state_rows_with_lifecycle_and_progress() {
    let mut view = PlanBrowserView::new(SessionId("brain-1".into()));
    let lineage = ExecutorLineage::new();
    let plans = plan_store_with_current("plan-a1");
    let ctx = view_ctx(&lineage, &plans);
    view.handle_spur_event(&loaded_event(), &ctx);

    let backend = TestBackend::new(120, 22);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let rendered = rendered_text(&terminal);

    for expected in [
        "Execution slot: plan-a1 running 2/7 done",
        "plan-a1",
        "mine",
        "running",
        "2/7 done",
        "plan-b2",
        "unowned",
        "pending",
        "0/3 done",
        "other-brain",
        "ambiguous",
    ] {
        assert!(
            rendered.contains(expected),
            "expected {expected:?} in rendered output:\n{rendered}"
        );
    }
}

#[test]
fn renders_empty_state_and_empty_current_sprint_slot() {
    let mut view = PlanBrowserView::new(SessionId("brain-1".into()));
    let lineage = ExecutorLineage::new();
    let plans = PlanProjectionStore::default();
    let ctx = view_ctx(&lineage, &plans);

    let backend = TestBackend::new(90, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let rendered = rendered_text(&terminal);

    for expected in [
        "Execution slot: empty",
        "No plans found.",
        "Press b to open Backlog and execute an epic.",
    ] {
        assert!(
            rendered.contains(expected),
            "expected {expected:?} in rendered output:\n{rendered}"
        );
    }
}

#[test]
fn renders_plan_load_warning_for_stale_duplicate_epic() {
    let mut view = PlanBrowserView::new(SessionId("brain-1".into()));
    let lineage = ExecutorLineage::new();
    let plans = PlanProjectionStore::default();
    let ctx = view_ctx(&lineage, &plans);
    view.handle_spur_event(
        &SpurEvent::now(SpurEventBody::PlansLoaded {
            plans: vec![summary(
                "9e102092-3dae-40ad-b667-c4dc26ffdc90",
                PlanOwnerStateEvent::Other {
                    owner: "647b42389c5648ef88e67840f1167472".into(),
                },
                PlanLifecycleEvent::Pending,
                0,
                8,
            )],
            warnings: vec![PlanLoadWarningEvent {
                plan_id: "9e102092-3dae-40ad-b667-c4dc26ffdc90".into(),
                canonical_epic_id: Some("bd-2pb".into()),
                stale_epic_ids: vec!["bd-2e0".into()],
                canonical_owner_state: Some(PlanOwnerStateEvent::Other {
                    owner: "647b42389c5648ef88e67840f1167472".into(),
                }),
                message: "Plan 9e102092-3dae-40ad-b667-c4dc26ffdc90 has stale duplicate epic bd-2e0; using canonical epic bd-2pb.".into(),
            }],
        }),
        &ctx,
    );

    let backend = TestBackend::new(120, 22);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let rendered = rendered_text(&terminal);

    for expected in [
        "stale duplicate epic bd-2e0",
        "canonical epic bd-2pb",
        "647b42389c5648ef88e67840f1167472",
    ] {
        assert!(
            rendered.contains(expected),
            "expected {expected:?} in rendered output:\n{rendered}"
        );
    }
}

#[test]
fn blocked_claim_when_current_brain_already_has_active_mine_plan() {
    let mut view = PlanBrowserView::new(SessionId("brain-1".into()));
    let lineage = ExecutorLineage::new();
    let plans = plan_store_with_current("plan-a1");
    let ctx = view_ctx(&lineage, &plans);
    view.handle_spur_event(&loaded_event(), &ctx);

    view.handle_key(key(KeyCode::Char('j')), &ctx);
    let action = view.handle_key(key(KeyCode::Char('c')), &ctx);

    match action {
        Some(Action::FlashHint { message }) => {
            assert!(message.contains("already owns active sprint"), "{message}");
        }
        other => panic!("expected blocked FlashHint, got {other:?}"),
    }
}

#[test]
fn enter_on_active_mine_plan_opens_current_session_sprint() {
    let mut view = PlanBrowserView::new(SessionId("brain-1".into()));
    let lineage = ExecutorLineage::new();
    let plans = plan_store_with_current("plan-a1");
    let ctx = view_ctx(&lineage, &plans);
    view.handle_spur_event(&loaded_event(), &ctx);

    let action = view.handle_key(key(KeyCode::Enter), &ctx);

    assert!(
        matches!(
            action,
            Some(Action::InspectPlan {
                session_id: SessionId(ref id),
                ref plan_id
            }) if id == "brain-1" && plan_id == "plan-a1"
        ),
        "expected pinned PlanInspector navigation, got {action:?}"
    );
}

#[test]
fn enter_on_persisted_mine_without_projection_inspects_plan() {
    let mut view = PlanBrowserView::new(SessionId("brain-1".into()));
    let lineage = ExecutorLineage::new();
    let plans = PlanProjectionStore::default();
    let ctx = view_ctx(&lineage, &plans);
    view.handle_spur_event(&loaded_event(), &ctx);

    let action = view.handle_key(key(KeyCode::Enter), &ctx);

    assert!(
        matches!(
            action,
            Some(Action::InspectPlan {
                session_id: SessionId(ref id),
                ref plan_id
            }) if id == "brain-1" && plan_id == "plan-a1"
        ),
        "expected read-only InspectPlan from persisted Mine row, got {action:?}"
    );
}

#[test]
fn start_on_unowned_plan_requires_claim_even_without_projection() {
    let mut view = PlanBrowserView::new(SessionId("brain-1".into()));
    let lineage = ExecutorLineage::new();
    let plans = PlanProjectionStore::default();
    let ctx = view_ctx(&lineage, &plans);
    view.handle_spur_event(&loaded_event(), &ctx);

    view.handle_key(key(KeyCode::Char('j')), &ctx);
    let action = view.handle_key(key(KeyCode::Char('s')), &ctx);

    match action {
        Some(Action::FlashHint { message }) => {
            assert!(message.contains("press c to claim first"), "{message}");
        }
        other => panic!("expected blocked FlashHint, got {other:?}"),
    }
}

#[test]
fn selected_row_remains_visible_when_list_is_longer_than_viewport() {
    let mut view = PlanBrowserView::new(SessionId("brain-1".into()));
    let lineage = ExecutorLineage::new();
    let plans = PlanProjectionStore::default();
    let ctx = view_ctx(&lineage, &plans);
    let long_plans = (0..18)
        .map(|idx| {
            summary(
                &format!("plan-{idx:02}"),
                PlanOwnerStateEvent::Unowned,
                PlanLifecycleEvent::Pending,
                0,
                1,
            )
        })
        .collect();
    view.handle_spur_event(
        &SpurEvent::now(SpurEventBody::PlansLoaded {
            plans: long_plans,
            warnings: Vec::new(),
        }),
        &ctx,
    );
    view.handle_key(key(KeyCode::Char('G')), &ctx);

    let backend = TestBackend::new(90, 14);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let rendered = rendered_text(&terminal);

    assert!(
        rendered.contains("> plan-17"),
        "selected final row should stay visible:\n{rendered}"
    );
}
