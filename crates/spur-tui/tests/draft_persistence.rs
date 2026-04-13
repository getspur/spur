use std::time::Duration;
use spur_tui::action::Action;
use spur_tui::views::session_detail::SessionDetailView;
use spur_tui::views::View;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

#[test]
fn tick_emits_save_draft_after_debounce() {
    let sid = spur_acp::SessionId("sess-1".to_string());
    let mut view = SessionDetailView::new(
        sid.clone(),
        "claude-code-acp".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
    );

    // Type a few characters.
    for c in "hello".chars() {
        let _ = view.handle_key(key(c));
    }

    // Advance the debounce clock past 500ms via a test helper.
    view.test_set_last_draft_change(std::time::Instant::now() - Duration::from_millis(600));

    // draft_save_action emits SaveDraft when debounce elapsed and text changed.
    let action = view.draft_save_action();
    match action {
        Some(Action::SaveDraft { session_id, draft }) => {
            assert_eq!(session_id, "sess-1");
            assert_eq!(draft, "hello");
        }
        other => panic!("expected SaveDraft, got {other:?}"),
    }
}

#[test]
fn tick_does_not_emit_save_draft_within_debounce_window() {
    let sid = spur_acp::SessionId("sess-1".to_string());
    let mut view = SessionDetailView::new(
        sid.clone(),
        "claude-code-acp".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
    );
    for c in "hi".chars() {
        let _ = view.handle_key(key(c));
    }
    // Do NOT advance the clock — last change was ~now.
    assert!(view.draft_save_action().is_none());
}

#[test]
fn save_draft_only_fires_once_per_change() {
    let sid = spur_acp::SessionId("sess-1".to_string());
    let mut view = SessionDetailView::new(
        sid.clone(),
        "claude-code-acp".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
    );
    for c in "abc".chars() {
        let _ = view.handle_key(key(c));
    }
    view.test_set_last_draft_change(std::time::Instant::now() - Duration::from_millis(600));
    assert!(view.draft_save_action().is_some());
    // Second call without new typing: no-op.
    assert!(view.draft_save_action().is_none());
}

#[test]
fn session_view_restores_draft_from_metadata() {
    let mut view = SessionDetailView::new(
        spur_acp::SessionId("sess-1".to_string()),
        "claude-code-acp".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
    );
    view.restore_draft("previous unsent text");
    assert_eq!(view.input_bar_text(), "previous unsent text");
}

#[test]
fn restore_draft_with_empty_string_is_noop() {
    let mut view = SessionDetailView::new(
        spur_acp::SessionId("sess-1".to_string()),
        "claude-code-acp".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
    );
    view.restore_draft("");
    assert_eq!(view.input_bar_text(), "");
}
