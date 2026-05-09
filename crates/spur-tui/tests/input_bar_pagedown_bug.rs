use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::components::input_bar::InputBar;

#[test]
fn pagedown_skips_atom() {
    let mut bar = InputBar::new();
    bar.set_text("123456789012\n123456789012\n123456789012".into(), 0);
    // Replace second line with an atom
    bar.set_text_cursor_for_test(13);
    bar.insert_atom("@my-file.txt", "file:///foo".into(), "my-file.txt".into());
    bar.set_text_cursor_for_test(3); // Column 3

    let _ = bar.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));

    let cursor = bar.cursor();
    assert!(
        cursor <= 13 || cursor >= 25,
        "Cursor ended up inside the atom! It is at {}",
        cursor
    );
}
