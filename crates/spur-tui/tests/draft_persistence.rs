use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::action::Action;
use spur_tui::views::session_detail::SessionDetailView;
use spur_tui::views::View;
use std::time::Duration;

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn instant_ago(duration: Duration) -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_sub(duration).unwrap_or(now)
}

#[test]
fn tick_emits_save_draft_after_debounce() {
    let sid = spur_acp::SessionId("sess-1".to_string());
    let mut view = SessionDetailView::new(
        sid.clone(),
        "claude-code-acp".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
        spur_tui::test_support::default_agent_config("claude-code-acp"),
        Vec::new(),
    );

    // Type a few characters.
    for c in "hello".chars() {
        let _ = view.handle_key(key(c), &test_ctx());
    }

    // Advance the debounce clock past 500ms via a test helper.
    view.test_set_last_draft_change(instant_ago(Duration::from_millis(600)));

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
        spur_tui::test_support::default_agent_config("claude-code-acp"),
        Vec::new(),
    );
    for c in "hi".chars() {
        let _ = view.handle_key(key(c), &test_ctx());
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
        spur_tui::test_support::default_agent_config("claude-code-acp"),
        Vec::new(),
    );
    for c in "abc".chars() {
        let _ = view.handle_key(key(c), &test_ctx());
    }
    view.test_set_last_draft_change(instant_ago(Duration::from_millis(600)));
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
        spur_tui::test_support::default_agent_config("claude-code-acp"),
        Vec::new(),
    );
    view.restore_draft("previous unsent text");
    assert_eq!(view.input_bar_text(), "previous unsent text");
}

#[test]
fn force_save_draft_emits_action_without_debounce() {
    let sid = spur_acp::SessionId("sess-1".to_string());
    let mut view = SessionDetailView::new(
        sid.clone(),
        "claude-code-acp".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
        spur_tui::test_support::default_agent_config("claude-code-acp"),
        Vec::new(),
    );
    for c in "foo".chars() {
        let _ = view.handle_key(key(c), &test_ctx());
    }
    // Do NOT advance the clock — draft_save_action would be a no-op here.
    assert!(
        view.draft_save_action().is_none(),
        "debounced path should not fire within 500ms"
    );

    // force_save_draft bypasses the debounce and emits the current text.
    match view.force_save_draft() {
        Some(Action::SaveDraft { session_id, draft }) => {
            assert_eq!(session_id, "sess-1");
            assert_eq!(draft, "foo");
        }
        other => panic!("expected SaveDraft, got {other:?}"),
    }
}

#[test]
fn force_save_draft_is_noop_when_unchanged() {
    let mut view = SessionDetailView::new(
        spur_acp::SessionId("sess-1".to_string()),
        "claude-code-acp".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
        spur_tui::test_support::default_agent_config("claude-code-acp"),
        Vec::new(),
    );
    // Empty and never persisted — unchanged, should be None.
    assert!(view.force_save_draft().is_none());

    // After a real persist, a second force-flush with no further edits is also None.
    for c in "foo".chars() {
        let _ = view.handle_key(key(c), &test_ctx());
    }
    assert!(view.force_save_draft().is_some());
    assert!(
        view.force_save_draft().is_none(),
        "second call without typing should be a no-op"
    );
}

#[test]
fn force_save_draft_clears_debounce_timer() {
    let mut view = SessionDetailView::new(
        spur_acp::SessionId("sess-1".to_string()),
        "claude-code-acp".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
        spur_tui::test_support::default_agent_config("claude-code-acp"),
        Vec::new(),
    );
    for c in "bar".chars() {
        let _ = view.handle_key(key(c), &test_ctx());
    }
    // Force-flush now, then advance the clock past the debounce window.
    assert!(view.force_save_draft().is_some());
    view.test_set_last_draft_change(instant_ago(Duration::from_millis(600)));
    // draft_save_action should NOT re-emit — force_save_draft already cleared the timer
    // and updated last_persisted_draft, so there's nothing new to save.
    assert!(view.draft_save_action().is_none());
}

#[test]
fn restore_draft_with_empty_string_is_noop() {
    let mut view = SessionDetailView::new(
        spur_acp::SessionId("sess-1".to_string()),
        "claude-code-acp".into(),
        "brain".into(),
        std::path::PathBuf::from("."),
        spur_tui::test_support::default_agent_config("claude-code-acp"),
        Vec::new(),
    );
    view.restore_draft("");
    assert_eq!(view.input_bar_text(), "");
}
