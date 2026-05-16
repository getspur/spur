use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::components::input_bar::{EditMode, InputBar, VimMode};

#[test]
fn vim_w_skips_atom() {
    let mut bar = InputBar::new();
    bar.insert_atom("@my-file.txt", "file:///foo".into(), "my-file.txt".into());
    bar.set_text_cursor_for_test(0);
    bar.set_mode_for_test(EditMode::Vim(VimMode::Normal));

    let _ = bar.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));

    let cursor = bar.cursor();
    assert!(
        cursor == 0 || cursor >= 12,
        "Vim w: Cursor ended up inside the atom! It is at {}",
        cursor
    );
}
