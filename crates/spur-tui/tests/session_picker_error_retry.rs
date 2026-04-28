use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::action::Action;
use spur_tui::views::session_picker::SessionPickerView;
use spur_tui::views::View;

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn error_picker() -> SessionPickerView {
    let mut picker = SessionPickerView::new();
    picker.set_error("network error".to_string());
    picker
}

#[test]
fn r_key_in_error_state_emits_refresh_sessions() {
    let mut picker = error_picker();
    let action = picker.handle_key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        &test_ctx(),
    );

    assert!(
        matches!(action, Some(Action::RefreshSessions)),
        "r in Error state must emit RefreshSessions, got {:?}",
        action
    );
}

#[test]
fn enter_in_error_state_emits_refresh_sessions() {
    let mut picker = error_picker();
    let action = picker.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &test_ctx(),
    );

    assert!(
        matches!(action, Some(Action::RefreshSessions)),
        "Enter in Error state must emit RefreshSessions, got {:?}",
        action
    );
}

#[test]
fn esc_in_error_state_navigates_back() {
    let mut picker = error_picker();
    let action = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());

    assert!(
        matches!(action, Some(Action::NavigateTo(_))),
        "Esc in Error state must navigate back, got {:?}",
        action
    );
}
