use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::components::input_bar::{EditMode, InputBar, VimMode};

#[test]
fn vim_visual_d_before_atom() {
    let mut bar = InputBar::new();
    bar.set_text("a".into(), 0);
    bar.set_text_cursor_for_test(1);
    bar.insert_atom("@foo", "file:///foo".into(), "foo".into());
    bar.set_text_cursor_for_test(0);
    bar.set_mode_for_test(EditMode::Vim(VimMode::Normal));

    println!("Text before: {}", bar.text());

    let _ = bar.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    let _ = bar.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));

    println!("Text is: {}", bar.text());
}
