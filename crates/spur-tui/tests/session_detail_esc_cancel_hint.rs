use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use spur_tui::action::Action;
use spur_tui::views::{session_detail::SessionDetailView, View};

const CANCEL_HINT: &str = "Esc cancelled the active turn. Press Esc again to go back.";

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn mk_view() -> SessionDetailView {
    SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
        spur_tui::test_support::default_agent_config("claude"),
        Vec::new(),
    )
}

fn press_esc(v: &mut SessionDetailView) -> Option<Action> {
    v.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx())
}

fn press_y(v: &mut SessionDetailView) -> Option<Action> {
    v.handle_key(
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        &test_ctx(),
    )
}

/// Simulate the full Esc-then-confirm flow: opens the cancel-confirm modal
/// then immediately dispatches via `y`. Returns the second action (the
/// confirmation) which is what callers care about. The first Esc returns
/// `None` (modal opens, no action yet).
fn esc_then_confirm(v: &mut SessionDetailView) -> Option<Action> {
    let opened = press_esc(v);
    assert!(
        opened.is_none(),
        "first Esc during stream must open the cancel-confirm modal, not dispatch (got {opened:?})"
    );
    press_y(v)
}

fn render_session_detail(view: &mut SessionDetailView) -> String {
    let backend = TestBackend::new(160, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let ctx = test_ctx();

    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();

    let buffer = terminal.backend().buffer();
    let mut output = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            output.push_str(buffer[(x, y)].symbol());
        }
        output.push('\n');
    }
    output
}

#[test]
fn first_esc_during_stream_emits_cancel_and_sets_hint() {
    let mut detail = mk_view();
    detail.set_stream_in_flight_for_test(true);

    // The full user flow: Esc opens the modal, `y` confirms and dispatches.
    let action = esc_then_confirm(&mut detail);

    assert!(matches!(action, Some(Action::CancelStream { .. })));
    assert!(
        detail
            .cancel_hint_until_for_test()
            .is_some_and(|t| t > Instant::now()),
        "cancel hint must be set in the future after Esc-cancel"
    );
    let rendered = render_session_detail(&mut detail);
    assert!(
        rendered.contains("cancelled the active turn"),
        "status bar should render cancel hint via view_hint_override:\n{rendered}"
    );
}

#[test]
fn second_esc_after_cancel_emits_navigate_back_and_clears_hint() {
    let mut detail = mk_view();
    detail.set_stream_in_flight_for_test(true);
    assert!(matches!(
        esc_then_confirm(&mut detail),
        Some(Action::CancelStream { .. })
    ));

    let action = press_esc(&mut detail);

    assert!(matches!(action, Some(Action::NavigateBack)));
    assert!(detail.cancel_hint_until_for_test().is_none());
}

#[test]
fn hint_expires_after_2_seconds() {
    let mut detail = mk_view();
    detail.set_cancel_hint_until_for_test(Some(Instant::now() - Duration::from_millis(1)));

    let rendered = render_session_detail(&mut detail);

    assert!(
        !rendered.contains(CANCEL_HINT),
        "expired cancel hint should not override the status bar:\n{rendered}"
    );
}

#[test]
fn reset_to_root_clears_cancel_hint() {
    let mut detail = mk_view();
    detail.set_stream_in_flight_for_test(true);
    assert!(matches!(
        esc_then_confirm(&mut detail),
        Some(Action::CancelStream { .. })
    ));

    detail.reset_to_root();

    assert!(detail.cancel_hint_until_for_test().is_none());
}

/// Render the view at `(width, height)` and return the rendered string.
fn render_at(detail: &mut SessionDetailView, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let ctx = test_ctx();
    terminal
        .draw(|frame| detail.render(frame, frame.area(), &ctx))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut output = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            output.push_str(buffer[(x, y)].symbol());
        }
        output.push('\n');
    }
    output
}

#[test]
fn confirm_modal_renders_full_layout_on_normal_terminal() {
    let mut detail = mk_view();
    detail.set_stream_in_flight_for_test(true);
    // Esc opens modal — `y` would dispatch, so we stop after the open.
    assert!(press_esc(&mut detail).is_none());

    let rendered = render_at(&mut detail, 80, 24);
    assert!(
        rendered.contains("Cancel turn?"),
        "modal title row missing on 80x24:\n{rendered}"
    );
    assert!(
        rendered.contains("[y]es") && rendered.contains("[n]o"),
        "modal options missing on 80x24:\n{rendered}"
    );
}

#[test]
fn confirm_modal_renders_compact_fallback_on_tiny_terminal() {
    // Regression: previously the modal returned early (rendering nothing)
    // when the terminal was smaller than 50×5, leaving the user in an
    // invisible focus trap because the key handler still swallowed all
    // input. The fallback must always render *something* the user can see.
    let mut detail = mk_view();
    detail.set_stream_in_flight_for_test(true);
    assert!(press_esc(&mut detail).is_none());

    // 30×3: too small for the full bordered modal, must trigger compact fallback.
    let rendered = render_at(&mut detail, 30, 3);
    assert!(
        rendered.contains("Cancel turn?"),
        "compact fallback must still surface 'Cancel turn?' on 30x3:\n{rendered}"
    );
}
