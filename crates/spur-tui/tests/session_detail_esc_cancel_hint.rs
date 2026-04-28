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

    let action = press_esc(&mut detail);

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
        press_esc(&mut detail),
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
        press_esc(&mut detail),
        Some(Action::CancelStream { .. })
    ));

    detail.reset_to_root();

    assert!(detail.cancel_hint_until_for_test().is_none());
}
