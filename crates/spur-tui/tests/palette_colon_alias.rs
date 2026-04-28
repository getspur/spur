use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::test_support::new_app;

#[test]
fn colon_opens_palette_in_dashboard_navigate() {
    let mut app = new_app();

    assert!(!app.is_palette_visible());
    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));

    assert!(
        app.is_palette_visible(),
        "':' should open the palette in Dashboard Navigate mode"
    );
}

#[test]
fn colon_passes_through_in_compose_mode() {
    let mut app = new_app();
    app.dashboard_mut_for_test().handle_paste("hello");

    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));

    assert!(!app.is_palette_visible());
    assert_eq!(app.dashboard_for_test().input_bar_text_for_test(), "hello:");
}
