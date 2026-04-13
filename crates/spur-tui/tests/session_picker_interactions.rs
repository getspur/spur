use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::action::Action;
use spur_tui::views::session_picker::SessionPickerView;
use spur_tui::views::View;

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

#[test]
fn n_key_on_picker_emits_new_session_requested() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("test-agent".into(), vec![]);
    let action = picker.handle_key(key('n'));
    assert!(
        matches!(action, Some(Action::NewSessionRequested)),
        "expected NewSessionRequested, got {action:?}"
    );
}

#[test]
fn enter_on_new_session_row_emits_new_session_requested() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("test-agent".into(), vec![]);
    // Cursor defaults to [+ New session] row at index 0.
    let action = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(action, Some(Action::NewSessionRequested)));
}
