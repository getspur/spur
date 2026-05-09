use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::components::input_bar::{EditMode, InputBar, VimMode};

#[test]
fn vim_dd_deletes_atoms() {
    let mut bar = InputBar::new();
    bar.set_text("hello\n".into(), 0);
    bar.set_text_cursor_for_test(6);
    bar.insert_atom("@foo", "file:///foo".into(), "foo".into());
    bar.set_text_cursor_for_test(6); // Cursor on second line ("@foo")
    bar.set_mode_for_test(EditMode::Vim(VimMode::Normal));

    // Press d d
    let _ = bar.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
    let _ = bar.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));

    println!("Text is: {}", bar.text());
    println!("Protected ranges: {:?}", bar.protected_ranges());

    assert!(
        bar.protected_ranges().is_empty(),
        "Atom should be deleted by dd!"
    );
}
