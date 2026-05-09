use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::components::input_bar::InputBar;

#[test]
fn paste_with_selection_deletes_atoms() {
    let mut bar = InputBar::new();
    bar.set_text("bar".into(), 0);

    // Select "bar"
    let _ = bar.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    let _ = bar.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    let _ = bar.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    // Cut it
    let _ = bar.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));

    // Now insert atom
    bar.insert_atom("@foo", "file:///foo".into(), "foo".into());
    bar.set_text_cursor_for_test(0);

    // Select the atom
    let _ = bar.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    let _ = bar.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    let _ = bar.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
    let _ = bar.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));

    let _ = bar.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));

    assert!(
        bar.protected_ranges().is_empty(),
        "Atom should be deleted by paste!"
    );
    assert_eq!(bar.text(), "bar");
}
