//! Snapshot/state tests for the loop browser.

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use spur_acp::{LoopDetailEvent, LoopRunRecordEvent, LoopSummaryEvent, SpurEvent, SpurEventBody};
use spur_core::{ExecutorLineage, PlanProjectionStore, SessionSynopsisProjection};
use spur_tui::views::loop_browser::LoopBrowserView;
use spur_tui::views::{View, ViewContext};

static BRAIN_STATUS: spur_tui::app::BrainStatus = spur_tui::app::BrainStatus::Idle;
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
        notebook_ready: false,
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

fn summary(loop_id: &str) -> LoopSummaryEvent {
    LoopSummaryEvent {
        loop_id: loop_id.into(),
        issue_id: format!("bd-{loop_id}"),
        title: format!("Loop {loop_id}"),
        autonomy: Some("l2".into()),
        paused: false,
        retired: false,
        backoff_active: false,
        cadence_secs: 900,
        effective_interval_secs: 1800,
        next_run: Some((Utc::now() - chrono::Duration::minutes(5)).timestamp()),
        last_generation: Some(7),
        last_outcome: Some("approved".into()),
        last_cost_micros: Some(250_000),
        consecutive_failures: 0,
        goal_preview: Some("Keep CI loops moving without manual prompting.".into()),
        updated_at: None,
    }
}

fn loaded_event() -> SpurEvent {
    let mut paused = summary("paused-loop");
    paused.paused = true;
    paused.next_run = Some((Utc::now() + chrono::Duration::hours(2)).timestamp());
    paused.last_outcome = Some("failed".into());
    paused.consecutive_failures = 2;

    SpurEvent::now(SpurEventBody::LoopsLoaded {
        loops: vec![summary("active-loop"), paused],
        warnings: Vec::new(),
    })
}

fn detail(loop_id: &str) -> LoopDetailEvent {
    LoopDetailEvent {
        loop_id: loop_id.into(),
        issue_id: format!("bd-{loop_id}"),
        title: format!("Loop {loop_id}"),
        goal_preview: Some("Keep CI loops moving without manual prompting.".into()),
        cadence_secs: 900,
        effective_interval_secs: 1800,
        backoff_active: true,
        paused: false,
        next_run: None,
        consecutive_failures: 1,
        budget_micros_per_generation: Some(500_000),
        max_generations_per_day: Some(4),
        max_tasks: Some(3),
        recent_runs: vec![
            LoopRunRecordEvent {
                generation: 7,
                outcome: "approved".into(),
                cost_micros: 250_000,
                autonomy: Some("l2".into()),
            },
            LoopRunRecordEvent {
                generation: 6,
                outcome: "failed".into(),
                cost_micros: 125_000,
                autonomy: Some("l1".into()),
            },
        ],
    }
}

#[test]
fn renders_loop_rows_with_state_cadence_next_run_and_last_run() {
    let mut view = LoopBrowserView::new();
    let lineage = ExecutorLineage::new();
    let plans = PlanProjectionStore::default();
    let ctx = view_ctx(&lineage, &plans);
    view.handle_spur_event(&loaded_event(), &ctx);

    let backend = TestBackend::new(150, 28);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let rendered = rendered_text(&terminal);

    for expected in [
        "Loops",
        "active-loop",
        "paused-loop",
        "L2",
        "active",
        "paused",
        "15m",
        "30m",
        "due",
        "g7 approved",
        "fails",
    ] {
        assert!(
            rendered.contains(expected),
            "expected {expected:?} in rendered output:\n{rendered}"
        );
    }
}

#[test]
fn renders_detail_loaded_governors_and_recent_runs() {
    let mut view = LoopBrowserView::new();
    let lineage = ExecutorLineage::new();
    let plans = PlanProjectionStore::default();
    let ctx = view_ctx(&lineage, &plans);
    view.handle_spur_event(&loaded_event(), &ctx);
    view.handle_key(key(KeyCode::Enter), &ctx);
    view.handle_spur_event(
        &SpurEvent::now(SpurEventBody::LoopDetailLoaded {
            detail: detail("active-loop"),
        }),
        &ctx,
    );

    let backend = TestBackend::new(130, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let rendered = rendered_text(&terminal);

    for expected in [
        "Loop Detail",
        "budget/gen",
        "day cap 4",
        "max tasks 3",
        "Recent runs",
        "gen 7",
        "approved",
        "gen 6",
        "failed",
    ] {
        assert!(
            rendered.contains(expected),
            "expected {expected:?} in rendered output:\n{rendered}"
        );
    }
}

#[test]
fn renders_pause_confirmation_modal() {
    let mut view = LoopBrowserView::new();
    let lineage = ExecutorLineage::new();
    let plans = PlanProjectionStore::default();
    let ctx = view_ctx(&lineage, &plans);
    view.handle_spur_event(&loaded_event(), &ctx);
    view.handle_key(key(KeyCode::Char('p')), &ctx);

    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();
    let rendered = rendered_text(&terminal);

    for expected in ["Pause Loop", "active-loop", "[Enter]", "Confirm", "[Esc]"] {
        assert!(
            rendered.contains(expected),
            "expected {expected:?} in rendered output:\n{rendered}"
        );
    }
}
