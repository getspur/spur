use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::components::input_bar::InputBar;

fn press(bar: &mut InputBar, code: KeyCode) {
    bar.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}
fn type_str(bar: &mut InputBar, s: &str) {
    for c in s.chars() {
        press(bar, KeyCode::Char(c));
    }
}

#[test]
fn insert_atom_creates_range_and_places_cursor_at_end() {
    let mut b = InputBar::new();
    type_str(&mut b, "hi ");
    b.insert_atom("@src/foo.rs", "file:///abs/src/foo.rs".into(), "src/foo.rs".into());
    assert_eq!(b.text(), "hi @src/foo.rs");
    assert_eq!(b.cursor(), b.text().len());
    assert_eq!(b.protected_ranges().len(), 1);
    let r = &b.protected_ranges()[0];
    assert_eq!(&b.text()[r.start..r.end], "@src/foo.rs");
}

#[test]
fn backspace_at_atom_end_deletes_whole_atom() {
    let mut b = InputBar::new();
    type_str(&mut b, "hi ");
    b.insert_atom("@src/foo.rs", "file:///a".into(), "src/foo.rs".into());
    press(&mut b, KeyCode::Backspace);
    assert_eq!(b.text(), "hi ");
    assert_eq!(b.cursor(), 3);
    assert!(b.protected_ranges().is_empty());
}

#[test]
fn backspace_inside_atom_deletes_whole_atom() {
    let mut b = InputBar::new();
    type_str(&mut b, "hi ");
    b.insert_atom("@src/foo.rs", "file:///a".into(), "src/foo.rs".into());
    press(&mut b, KeyCode::Left);
    press(&mut b, KeyCode::Backspace);
    assert_eq!(b.text(), "hi ");
    assert!(b.protected_ranges().is_empty());
}

#[test]
fn right_arrow_skips_atom_atomically() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " x");
    b.set_text_cursor_for_test(0);
    press(&mut b, KeyCode::Right);
    assert_eq!(b.cursor(), 5);
}

#[test]
fn left_arrow_skips_atom_atomically() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    press(&mut b, KeyCode::Left);
    assert_eq!(b.cursor(), 0);
}

#[test]
fn typing_inside_atom_deletes_atom_then_inserts() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    press(&mut b, KeyCode::Left);
    press(&mut b, KeyCode::Left);
    press(&mut b, KeyCode::Char('z'));
    assert_eq!(b.text(), "z");
    assert!(b.protected_ranges().is_empty());
    assert_eq!(b.cursor(), 1);
}

#[test]
fn range_shifts_when_text_inserted_before_it() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    press(&mut b, KeyCode::Home);
    type_str(&mut b, "xy ");
    assert_eq!(b.text(), "xy @a.rs");
    let r = &b.protected_ranges()[0];
    assert_eq!(r.start, 3);
    assert_eq!(r.end, 8);
}

#[test]
fn two_atoms_preserve_sort_order() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " and ");
    b.insert_atom("@b.rs", "file:///b".into(), "b.rs".into());
    let ranges = b.protected_ranges();
    assert_eq!(ranges.len(), 2);
    assert!(ranges[0].start < ranges[1].start);
    assert_eq!(&b.text()[ranges[0].start..ranges[0].end], "@a.rs");
    assert_eq!(&b.text()[ranges[1].start..ranges[1].end], "@b.rs");
}

#[test]
fn clear_removes_ranges() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    b.clear();
    assert_eq!(b.text(), "");
    assert!(b.protected_ranges().is_empty());
}

#[test]
fn forward_delete_at_atom_start_deletes_whole_atom() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    press(&mut b, KeyCode::Home);
    press(&mut b, KeyCode::Delete);
    assert_eq!(b.text(), "");
    assert!(b.protected_ranges().is_empty());
}

#[test]
fn enter_captures_text_and_ranges() {
    let mut b = InputBar::new();
    type_str(&mut b, "hi ");
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, "!");
    let result = b.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(result.is_some());
    let (text, ranges, interrupt) = b.take_submit_capture().expect("capture");
    assert_eq!(text, "hi @a.rs!");
    assert_eq!(ranges.len(), 1);
    assert_eq!(&text[ranges[0].start..ranges[0].end], "@a.rs");
    assert!(!interrupt); // '!' at end, not start
}
