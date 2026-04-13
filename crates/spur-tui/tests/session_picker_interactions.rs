use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::SessionInfo;
use spur_tui::action::Action;
use spur_tui::views::session_picker::SessionPickerView;
use spur_tui::views::View;

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn session(id: &str, title: &str) -> SessionInfo {
    SessionInfo::new(
        std::sync::Arc::<str>::from(id),
        std::path::PathBuf::from("/tmp"),
    )
    .title(title.to_string())
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

#[test]
fn slash_focuses_search_and_typing_filters() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions(
        "test".into(),
        vec![
            session("a1", "refactor auth"),
            session("a2", "debug race condition"),
            session("a3", "perf investigation"),
        ],
    );

    // Focus search
    let _ = picker.handle_key(key('/'));
    // Type "race"
    for c in "race".chars() {
        let _ = picker.handle_key(key(c));
    }

    assert_eq!(picker.visible_session_count(), 1);
    assert_eq!(
        picker
            .visible_session_at(0)
            .map(|s| s.session_id.0.as_ref()),
        Some("a2")
    );
}

#[test]
fn esc_in_search_returns_to_list_keeping_filter() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions(
        "t".into(),
        vec![session("a1", "alpha"), session("a2", "beta")],
    );
    let _ = picker.handle_key(key('/'));
    let _ = picker.handle_key(key('b'));
    // Currently in search mode
    let action = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(action.is_none());
    // Filter still active
    assert_eq!(picker.visible_session_count(), 1);
}

#[test]
fn esc_in_list_with_active_filter_clears_it() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions(
        "t".into(),
        vec![session("a1", "alpha"), session("a2", "beta")],
    );
    let _ = picker.handle_key(key('/'));
    let _ = picker.handle_key(key('b'));
    // Leave search mode but keep filter.
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(picker.visible_session_count(), 1);
    // Second Esc clears filter.
    let action = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(action.is_none());
    assert_eq!(picker.visible_session_count(), 2);
}

#[test]
fn esc_in_list_with_no_filter_navigates_back() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x")]);
    let action = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(action, Some(Action::NavigateTo(_))));
}

#[test]
fn p_key_emits_toggle_pin_for_highlighted_session() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x"), session("a2", "y")]);
    // Move cursor to first real session (index 1, [+ New] is at 0).
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let action = picker.handle_key(key('p'));
    match action {
        Some(Action::ToggleSessionPin { session_id }) => {
            assert_eq!(session_id, "a1");
        }
        other => panic!("expected ToggleSessionPin, got {other:?}"),
    }
}

#[test]
fn p_key_on_new_session_row_is_noop() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x")]);
    let action = picker.handle_key(key('p'));
    assert!(action.is_none());
}
