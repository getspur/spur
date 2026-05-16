use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::components::input_bar::{HandleOutcome, InputBar};

fn press(bar: &mut InputBar, code: KeyCode, modifiers: KeyModifiers) -> HandleOutcome {
    bar.handle_key(KeyEvent::new(code, modifiers))
}

#[test]
fn insert_char_with_selection_deletes_atoms_correctly() {
    let mut bar = InputBar::new();
    bar.insert_atom("@foo", "file:///foo".into(), "foo".into());
    bar.set_text_cursor_for_test(0);

    // Select the atom
    let _ = press(&mut bar, KeyCode::Right, KeyModifiers::SHIFT);
    let _ = press(&mut bar, KeyCode::Right, KeyModifiers::SHIFT);
    let _ = press(&mut bar, KeyCode::Right, KeyModifiers::SHIFT);
    let _ = press(&mut bar, KeyCode::Right, KeyModifiers::SHIFT);

    // Insert a char, which should replace the selection.
    let _ = press(&mut bar, KeyCode::Char('a'), KeyModifiers::NONE);

    // The atom should be deleted.
    assert!(bar.protected_ranges().is_empty(), "Atom should be deleted");
    assert_eq!(bar.text(), "a");
}
