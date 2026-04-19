use crossterm::event::{Event as CtEvent, KeyCode, KeyEvent, KeyModifiers};
use spur_tui::test_support::new_app;

fn key(c: KeyCode, m: KeyModifiers) -> CtEvent {
    CtEvent::Key(KeyEvent::new(c, m))
}

#[test]
fn ctrl_k_opens_palette_and_esc_closes() {
    let mut app = new_app();
    assert!(!app.is_palette_visible());

    app.handle_crossterm_event(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
    assert!(app.is_palette_visible(), "Ctrl+K should open palette");

    app.handle_crossterm_event(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.is_palette_visible(), "Esc should close palette");
}

#[test]
fn ctrl_k_with_help_visible_is_swallowed_by_help() {
    let mut app = new_app();
    // Simulate opening help (via `?`).
    app.handle_crossterm_event(key(KeyCode::Char('?'), KeyModifiers::NONE));
    // Ctrl+K while help is up must NOT open the palette — help swallows keys.
    app.handle_crossterm_event(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
    assert!(!app.is_palette_visible());
}

#[test]
fn ctrl_k_while_help_visible_does_not_open_palette_even_if_help_does_not_swallow_it() {
    let mut app = new_app();
    // Open help.
    app.handle_crossterm_event(key(KeyCode::Char('?'), KeyModifiers::NONE));
    // Directly attempt to open palette via the internal path (bypassing the
    // priority chain). The guard in open_palette must prevent it.
    app.try_open_palette_for_test();
    assert!(!app.is_palette_visible(),
        "open_palette must refuse to open while help_visible is true");
}

#[test]
fn palette_session_accept_emits_resume_session_action() {
    let mut app = new_app();
    app.handle_crossterm_event(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
    // Inject a fake session result directly via test hook.
    app.seed_palette_with_session_for_test("s1", "refactor-auth");
    // Enter accepts.
    app.handle_crossterm_event(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(!app.is_palette_visible(), "palette should close after accept");
    let last = app.last_action_for_test().expect("an Action should have been dispatched");
    match last {
        spur_tui::action::Action::ResumeSession { session_id } => {
            assert_eq!(session_id, "s1");
        }
        other => panic!("expected ResumeSession, got {:?}", other),
    }
}
