// rustc resolves submodules of integration-test files relative to
// `tests/` (not the test file's directory), so #[path] is required to
// keep the helper co-located under tests/palette_integration/.
#[path = "palette_integration/util.rs"]
mod util;

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
    assert!(
        !app.is_palette_visible(),
        "open_palette must refuse to open while help_visible is true"
    );
}

#[test]
fn palette_session_accept_emits_resume_session_action() {
    let mut app = new_app();
    app.handle_crossterm_event(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
    // Inject a fake session result directly via test hook.
    app.seed_palette_with_session_for_test("s1", "refactor-auth");
    // Enter accepts.
    app.handle_crossterm_event(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        !app.is_palette_visible(),
        "palette should close after accept"
    );
    let last = app
        .last_action_for_test()
        .expect("an Action should have been dispatched");
    match last {
        spur_tui::action::Action::ResumeSession { session_id } => {
            assert_eq!(session_id, "s1");
        }
        other => panic!("expected ResumeSession, got {:?}", other),
    }
}

#[test]
fn open_palette_surfaces_session_command_registry_entries() {
    let mut app = util::app_with_seeded_session_and_dynamic_command(
        "codex",
        "review",
        "Review the current diff",
    );
    app.try_open_palette_for_test();
    let state = app.palette_state_for_test();
    let labels: Vec<&str> = state.iter_ranked().map(|r| r.label.as_str()).collect();
    assert!(
        labels.contains(&"review"),
        "expected the dynamic /review command to appear in the palette; got: {labels:?}"
    );
}

#[test]
fn open_palette_surfaces_view_entries_and_sessions_requests_picker() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_tui::action::Action;
    use spur_tui::components::palette::PaletteKind;

    let mut app = spur_tui::app::App::new_for_palette_test();
    app.try_open_palette_for_test();

    let labels: Vec<&str> = app
        .palette_state_for_test()
        .iter_ranked()
        .map(|r| r.label.as_str())
        .collect();
    for expected in ["Dashboard", "Issues", "Sprints", "Sessions", "Insights"] {
        assert!(
            labels.contains(&expected),
            "expected {expected} view entry in palette; got: {labels:?}"
        );
    }
    let view_labels: Vec<&str> = app
        .palette_state_for_test()
        .iter_ranked()
        .filter(|r| r.kind == PaletteKind::View)
        .map(|r| r.label.as_str())
        .collect();
    assert_eq!(
        view_labels,
        vec!["Dashboard", "Issues", "Sprints", "Sessions", "Insights"]
    );

    app.palette_state_for_test_mut().set_query("iss");
    let selected = app
        .palette_state_for_test()
        .selected()
        .expect("iss should match a palette entry");
    assert_eq!(selected.label, "Issues");

    app.palette_state_for_test_mut().set_query("ses");
    let selected = app
        .palette_state_for_test()
        .selected()
        .expect("ses should match a palette entry");
    assert_eq!(selected.label, "Sessions");

    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(app.last_action_for_test(), Some(Action::RequestSessions)),
        "Sessions palette entry must request picker setup; got {:?}",
        app.last_action_for_test()
    );
}

#[test]
fn accept_spur_local_command_emits_concrete_action() {
    // Why seed a dynamic /anything command? It exercises the Some(view)
    // arm of result_to_action's borrow-shim. We then query /help to
    // confirm spur-local resolution still works through the borrowed
    // (rather than fallback) registry. The third test in this file covers
    // the None arm explicitly.
    // /help is a spur-local command → SubmitDecision::Local { Action::ShowHelp }.
    // Palette should emit Action::ShowHelp on Accept.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_tui::action::Action;
    let mut app = util::app_with_seeded_session_and_dynamic_command("codex", "anything", "unused");
    app.try_open_palette_for_test();
    app.palette_state_for_test_mut().set_query("help");
    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(app.last_action_for_test(), Some(Action::ShowHelp)),
        "expected Action::ShowHelp; got {:?}",
        app.last_action_for_test()
    );
}

#[test]
fn accept_agent_dynamic_command_emits_send_message() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_tui::action::Action;
    let mut app = util::app_with_seeded_session_and_dynamic_command(
        "codex",
        "review",
        "Review the current diff",
    );
    app.try_open_palette_for_test();
    app.palette_state_for_test_mut().set_query("review");
    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    // CommandsConfig::default() sets dispatch = DispatchKind::PromptText,
    // so build_entry produces a Dispatch::PromptText entry and routing
    // always lands on SubmitDecision::Send → Action::SendMessage. A
    // VendorExec test would need to seed a CommandsConfig with
    // DispatchKind::VendorExec and a non-empty exec_method.
    let action = app.last_action_for_test();
    assert!(
        matches!(action, Some(Action::SendMessage { .. })),
        "expected Action::SendMessage; got {:?}",
        action
    );
}

#[test]
fn accept_spur_local_command_works_without_session() {
    // No session_detail; spur-local /help still works (it's resident in
    // the empty CommandRegistry's SpurLocalSource fallback).
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_tui::action::Action;
    let mut app = spur_tui::app::App::new_for_palette_test();
    // No session_detail seeded → fallback CommandRegistry::new() with
    // only spur-local entries.
    app.try_open_palette_for_test();
    app.palette_state_for_test_mut().set_query("help");
    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(app.last_action_for_test(), Some(Action::ShowHelp)),
        "spur-local /help should work without a session; got {:?}",
        app.last_action_for_test()
    );
}
