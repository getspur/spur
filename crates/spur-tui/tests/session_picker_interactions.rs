use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::SessionInfo;
use spur_tui::action::Action;
use spur_tui::views::session_picker::SessionPickerView;
use spur_tui::views::View;

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

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
fn cursor_default_lands_on_last_active_when_present() {
    let mut picker = SessionPickerView::new();
    let mut meta = spur_tui::session_metadata::SessionMetadata::default();
    meta.last_active_session_id = Some("a2".to_string());
    picker.set_metadata(meta);
    picker.set_sessions(
        "t".into(),
        vec![
            session("a1", "alpha"),
            session("a2", "beta"),
            session("a3", "gamma"),
        ],
    );
    // a2 is the second session; in virtual cursor space [+ New]=0, a1=1, a2=2.
    assert_eq!(picker.cursor(), 2);
}

#[test]
fn cursor_default_falls_back_to_first_row_when_last_active_absent() {
    let mut picker = SessionPickerView::new();
    let meta = spur_tui::session_metadata::SessionMetadata::default();
    picker.set_metadata(meta);
    picker.set_sessions(
        "t".into(),
        vec![session("a1", "alpha"), session("a2", "beta")],
    );
    assert_eq!(picker.cursor(), 1);
}

#[test]
fn cursor_default_falls_back_to_zero_when_no_sessions() {
    let mut picker = SessionPickerView::new();
    picker.set_metadata(spur_tui::session_metadata::SessionMetadata::default());
    picker.set_sessions("t".into(), vec![]);
    assert_eq!(picker.cursor(), 0);
}

#[test]
fn cursor_default_falls_back_when_last_active_not_in_visible_list() {
    let mut picker = SessionPickerView::new();
    let mut meta = spur_tui::session_metadata::SessionMetadata::default();
    meta.last_active_session_id = Some("does-not-exist".to_string());
    picker.set_metadata(meta);
    picker.set_sessions("t".into(), vec![session("a1", "alpha")]);
    // last_active id is unknown → fall back to row 1.
    assert_eq!(picker.cursor(), 1);
}

#[test]
fn n_key_on_picker_emits_new_session_requested() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("test-agent".into(), vec![]);
    let action = picker.handle_key(key('n'), &test_ctx());
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
    let action = picker.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &test_ctx(),
    );
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
    let _ = picker.handle_key(key('/'), &test_ctx());
    // Type "race"
    for c in "race".chars() {
        let _ = picker.handle_key(key(c), &test_ctx());
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
    let _ = picker.handle_key(key('/'), &test_ctx());
    let _ = picker.handle_key(key('b'), &test_ctx());
    // Currently in search mode
    let action = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());
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
    let _ = picker.handle_key(key('/'), &test_ctx());
    let _ = picker.handle_key(key('b'), &test_ctx());
    // Leave search mode but keep filter.
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());
    assert_eq!(picker.visible_session_count(), 1);
    // Second Esc clears filter.
    let action = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());
    assert!(action.is_none());
    assert_eq!(picker.visible_session_count(), 2);
}

#[test]
fn esc_in_list_with_no_filter_navigates_back() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x")]);
    let action = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());
    assert!(matches!(action, Some(Action::NavigateTo(_))));
}

#[test]
fn p_key_emits_toggle_pin_for_highlighted_session() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x"), session("a2", "y")]);
    // Cursor defaults to first real session (index 1, [+ New] is at 0).
    let action = picker.handle_key(key('p'), &test_ctx());
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
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &test_ctx());
    let action = picker.handle_key(key('p'), &test_ctx());
    assert!(action.is_none());
}

#[test]
fn d_key_emits_toggle_archive_for_highlighted_session() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x")]);
    let _ = picker.handle_key(
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &test_ctx(),
    );
    let action = picker.handle_key(key('d'), &test_ctx());
    match action {
        Some(Action::ToggleSessionArchive { session_id }) => assert_eq!(session_id, "a1"),
        other => panic!("expected ToggleSessionArchive, got {other:?}"),
    }
}

#[test]
fn d_key_on_new_session_row_is_noop() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x")]);
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &test_ctx());
    let action = picker.handle_key(key('d'), &test_ctx());
    assert!(action.is_none());
}

#[test]
fn a_key_toggles_show_archived() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x")]);
    let action = picker.handle_key(key('a'), &test_ctx());
    assert!(matches!(action, Some(Action::ToggleShowArchived)));
}

#[test]
fn capital_r_enters_rename_mode_and_enter_commits() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "old title")]);
    let _ = picker.handle_key(
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &test_ctx(),
    );
    let _ = picker.handle_key(
        KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT),
        &test_ctx(),
    );
    assert!(picker.is_rename_active());
    // Clear old title by sending backspaces (the prompt pre-fills "old title" — 9 chars).
    for _ in 0..20 {
        let _ = picker.handle_key(
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &test_ctx(),
        );
    }
    for c in "new name".chars() {
        let _ = picker.handle_key(key(c), &test_ctx());
    }
    let action = picker.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &test_ctx(),
    );
    match action {
        Some(Action::RenameSession {
            session_id,
            new_title,
        }) => {
            assert_eq!(session_id, "a1");
            assert_eq!(new_title, "new name");
        }
        other => panic!("expected RenameSession, got {other:?}"),
    }
    assert!(!picker.is_rename_active());
}

#[test]
fn esc_in_rename_cancels_without_action() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "old")]);
    let _ = picker.handle_key(
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &test_ctx(),
    );
    let _ = picker.handle_key(
        KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT),
        &test_ctx(),
    );
    let _ = picker.handle_key(key('z'), &test_ctx());
    let action = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());
    assert!(action.is_none());
    assert!(!picker.is_rename_active());
}

#[test]
fn capital_p_toggles_preview_visible() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x")]);
    assert!(!picker.is_preview_visible());
    let _ = picker.handle_key(
        KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT),
        &test_ctx(),
    );
    assert!(picker.is_preview_visible());
    let _ = picker.handle_key(
        KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT),
        &test_ctx(),
    );
    assert!(!picker.is_preview_visible());
}

#[test]
fn r_key_emits_refresh_sessions() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "x")]);
    let action = picker.handle_key(key('r'), &test_ctx());
    assert!(matches!(action, Some(Action::RefreshSessions)));
}

#[test]
fn picker_preserves_cursor_and_filter_across_set_sessions() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions(
        "t".into(),
        vec![session("a1", "alpha"), session("a2", "beta")],
    );
    // Navigate to cursor=2 and set filter to "b".
    let _ = picker.handle_key(
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &test_ctx(),
    );
    let _ = picker.handle_key(
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &test_ctx(),
    );
    let _ = picker.handle_key(key('/'), &test_ctx());
    let _ = picker.handle_key(key('b'), &test_ctx());
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());

    assert_eq!(picker.cursor(), 0); // filter change reset cursor to 0
    assert_eq!(picker.filter(), "b");

    // Simulate re-receiving the same session list.
    picker.set_sessions(
        "t".into(),
        vec![session("a1", "alpha"), session("a2", "beta")],
    );

    // Cursor and filter should be preserved.
    assert_eq!(picker.cursor(), 0);
    assert_eq!(picker.filter(), "b");
}

#[test]
fn enter_switching_session_with_current_draft_shows_confirm() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions(
        "t".into(),
        vec![session("a1", "alpha"), session("a2", "beta")],
    );
    // Tell picker that session a1 has an unsent draft (simulates the App's
    // coordination — picker doesn't look up metadata itself).
    picker.set_current_session_has_draft(Some("a1".to_string()));

    // Move cursor to a2 (cursor 2 in virtual layout: [+ New]=0, a1=1, a2=2).
    let _ = picker.handle_key(
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &test_ctx(),
    );
    let _ = picker.handle_key(
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &test_ctx(),
    );

    // Enter should NOT immediately emit ResumeSession — it should open the confirm.
    let action = picker.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(action.is_none());
    assert!(picker.is_confirm_switch_visible());

    // Pressing 'y' commits the switch.
    let action = picker.handle_key(key('y'), &test_ctx());
    match action {
        Some(Action::ResumeSession { session_id }) => assert_eq!(session_id, "a2"),
        other => panic!("expected ResumeSession, got {other:?}"),
    }
    assert!(!picker.is_confirm_switch_visible());
}

#[test]
fn esc_cancels_confirm_switch() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions(
        "t".into(),
        vec![session("a1", "alpha"), session("a2", "beta")],
    );
    picker.set_current_session_has_draft(Some("a1".to_string()));
    let _ = picker.handle_key(
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &test_ctx(),
    );
    let _ = picker.handle_key(
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &test_ctx(),
    );
    let _ = picker.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &test_ctx(),
    );
    let action = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());
    assert!(action.is_none());
    assert!(!picker.is_confirm_switch_visible());
}

#[test]
fn enter_on_same_session_id_does_not_show_confirm() {
    // If user picks the session they're already in, no confirm needed.
    let mut picker = SessionPickerView::new();
    picker.set_sessions(
        "t".into(),
        vec![session("a1", "alpha"), session("a2", "beta")],
    );
    picker.set_current_session_has_draft(Some("a1".to_string()));
    // Cursor on a1 (cursor=1).
    let action = picker.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &test_ctx(),
    );
    // Should emit ResumeSession directly (same session, no switch).
    assert!(matches!(action, Some(Action::ResumeSession { session_id }) if session_id == "a1"));
    assert!(!picker.is_confirm_switch_visible());
}

#[test]
fn enter_on_new_session_row_with_draft_shows_confirm() {
    let mut picker = SessionPickerView::new();
    picker.set_sessions("t".into(), vec![session("a1", "alpha")]);
    picker.set_current_session_has_draft(Some("a1".to_string()));
    let _ = picker.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &test_ctx());
    // Cursor is at [+ New session] (cursor=0).
    let action = picker.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &test_ctx(),
    );
    // [+ New] with a current draft should also show confirm.
    assert!(action.is_none());
    assert!(picker.is_confirm_switch_visible());

    let action = picker.handle_key(key('y'), &test_ctx());
    assert!(matches!(action, Some(Action::NewSessionRequested)));
}
