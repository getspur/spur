use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::components::input_bar::InputBar;

#[test]
fn word_forward_skips_atom() {
    let mut bar = InputBar::new();
    bar.insert_atom("@my-file.txt", "file:///foo".into(), "my-file.txt".into());
    bar.set_text_cursor_for_test(0);

    bar.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT));

    let cursor = bar.cursor();
    assert!(
        cursor == 0 || cursor >= 12,
        "Cursor ended up inside the atom! It is at {}",
        cursor
    );
}
