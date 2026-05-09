use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::components::input_bar::{EditMode, HandleOutcome, InputBar, VimMode};

fn press(bar: &mut InputBar, code: KeyCode) {
    let _ = bar.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}
fn ctrl(bar: &mut InputBar, c: char) {
    let _ = bar.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
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
    b.insert_atom(
        "@src/foo.rs",
        "file:///abs/src/foo.rs".into(),
        "src/foo.rs".into(),
    );
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
    b.set_text_cursor_for_test(1);
    press(&mut b, KeyCode::Char('z'));
    assert_eq!(b.text(), "z");
    assert!(b.protected_ranges().is_empty());
    assert_eq!(b.cursor(), 1);
}

#[test]
fn typing_over_selection_containing_atom_removes_atom_and_rebases_later_atom() {
    let mut b = InputBar::new();
    type_str(&mut b, "x ");
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " y ");
    b.insert_atom("@b.rs", "file:///b".into(), "b.rs".into());

    b.set_mode(EditMode::Vim(VimMode::Normal));
    b.set_text_cursor_for_test(0);
    press(&mut b, KeyCode::Char('v'));
    press(&mut b, KeyCode::Char('l'));
    press(&mut b, KeyCode::Char('l'));
    press(&mut b, KeyCode::Char('l'));
    b.set_mode_for_test(EditMode::Emacs);

    press(&mut b, KeyCode::Char('z'));

    assert_eq!(b.text(), "z y @b.rs");
    let ranges = b.protected_ranges();
    assert_eq!(ranges.len(), 1);
    assert_eq!(&b.text()[ranges[0].start..ranges[0].end], "@b.rs");
    assert_eq!(ranges[0].start, "z y ".len());
}

#[test]
fn paste_over_selection_containing_atom_removes_atom() {
    let mut b = InputBar::new();
    type_str(&mut b, "x ");
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " y");

    b.set_mode(EditMode::Vim(VimMode::Normal));
    b.set_text_cursor_for_test(0);
    press(&mut b, KeyCode::Char('v'));
    press(&mut b, KeyCode::Char('l'));
    press(&mut b, KeyCode::Char('l'));
    press(&mut b, KeyCode::Char('l'));
    b.set_mode_for_test(EditMode::Emacs);

    b.insert_paste("paste");

    assert_eq!(b.text(), "paste y");
    assert!(b.protected_ranges().is_empty());
}

#[test]
fn word_forward_into_atom_snaps_to_atom_boundary() {
    let mut b = InputBar::new();
    type_str(&mut b, "x ");
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " y");

    b.set_text_cursor_for_test(2);
    let _ = b.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT));

    assert_eq!(b.cursor(), 7);
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
    assert!(matches!(result, HandleOutcome::Submit(_, _)));
    let (text, ranges, interrupt) = b.take_submit_capture().expect("capture");
    assert_eq!(text, "hi @a.rs!");
    assert_eq!(ranges.len(), 1);
    assert_eq!(&text[ranges[0].start..ranges[0].end], "@a.rs");
    assert!(!interrupt); // '!' at end, not start
}

#[test]
fn deleting_first_of_two_atoms_rebases_second_atom() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " and ");
    b.insert_atom("@b.rs", "file:///b".into(), "b.rs".into());

    press(&mut b, KeyCode::Home);
    press(&mut b, KeyCode::Delete);

    assert_eq!(b.text(), " and @b.rs");
    let ranges = b.protected_ranges();
    assert_eq!(ranges.len(), 1);
    assert_eq!(&b.text()[ranges[0].start..ranges[0].end], "@b.rs");
}

#[test]
fn ctrl_u_removes_deleted_atom_and_keeps_later_atom_shifted() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " ");
    b.insert_atom("@b.rs", "file:///b".into(), "b.rs".into());

    let second_start = b.protected_ranges()[1].start;
    b.set_text_cursor_for_test(second_start);
    ctrl(&mut b, 'u');

    assert_eq!(b.text(), "@b.rs");
    let ranges = b.protected_ranges();
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].start, 0);
    assert_eq!(&b.text()[ranges[0].start..ranges[0].end], "@b.rs");
}

#[test]
fn ctrl_k_keeps_later_line_atom_and_shifts_it() {
    let mut b = InputBar::new();
    b.set_text("hello\n".to_string(), "hello\n".len());
    b.insert_atom("@b.rs", "file:///b".into(), "b.rs".into());

    b.set_text_cursor_for_test(2);
    ctrl(&mut b, 'k');

    assert_eq!(b.text(), "he\n@b.rs");
    let ranges = b.protected_ranges();
    assert_eq!(ranges.len(), 1);
    assert_eq!(&b.text()[ranges[0].start..ranges[0].end], "@b.rs");
}

#[test]
fn ctrl_w_preserves_earlier_atom_when_deleting_previous_word() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " hello");

    ctrl(&mut b, 'w');

    assert_eq!(b.text(), "@a.rs ");
    let ranges = b.protected_ranges();
    assert_eq!(ranges.len(), 1);
    assert_eq!(&b.text()[ranges[0].start..ranges[0].end], "@a.rs");
}

#[test]
fn deleting_unicode_atom_only_removes_atom_text() {
    let mut b = InputBar::new();
    type_str(&mut b, "x ");
    b.insert_atom("@猫", "file:///cat".into(), "cat".into());
    type_str(&mut b, " y");

    let atom_end = b.protected_ranges()[0].end;
    b.set_text_cursor_for_test(atom_end);
    press(&mut b, KeyCode::Backspace);

    assert_eq!(b.text(), "x  y");
    assert!(b.protected_ranges().is_empty());
}

#[test]
fn deleting_multibyte_char_before_atom_rebases_by_utf8_width() {
    let mut b = InputBar::new();
    type_str(&mut b, "é ");
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());

    press(&mut b, KeyCode::Home);
    press(&mut b, KeyCode::Delete);

    assert_eq!(b.text(), " @a.rs");
    let ranges = b.protected_ranges();
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0].start, 1);
    assert_eq!(&b.text()[ranges[0].start..ranges[0].end], "@a.rs");
}
// ── Vim destructive edits must preserve unaffected atoms ──────────────────

#[test]
fn vim_d_preserves_atom_outside_deleted_span() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " abc tail");
    b.set_mode(EditMode::Vim(VimMode::Normal));

    // Cursor after " abc " (byte 10), before "tail"
    b.set_text_cursor_for_test(10);
    let _ = b.handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::NONE));

    assert_eq!(b.text(), "@a.rs abc ");
    assert_eq!(b.protected_ranges().len(), 1);
    let r = &b.protected_ranges()[0];
    assert_eq!(&b.text()[r.start..r.end], "@a.rs");
}

#[test]
fn vim_c_preserves_atom_outside_changed_span() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " abc tail");
    b.set_mode(EditMode::Vim(VimMode::Normal));

    b.set_text_cursor_for_test(10);
    let _ = b.handle_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE));

    assert_eq!(b.text(), "@a.rs abc ");
    assert_eq!(b.protected_ranges().len(), 1);
    let r = &b.protected_ranges()[0];
    assert_eq!(&b.text()[r.start..r.end], "@a.rs");
    assert!(matches!(b.mode(), EditMode::Vim(VimMode::Insert)));
}

#[test]
fn vim_p_rebases_existing_atom_instead_of_clearing_all_ranges() {
    let mut b = InputBar::new();
    // Build text with an atom
    type_str(&mut b, "before ");
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " after");
    b.set_mode(EditMode::Vim(VimMode::Normal));

    // Yank "before " into the internal clipboard using visual mode + y
    b.set_text_cursor_for_test(0);
    let _ = b.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    for _ in 0..7 {
        let _ = b.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
    }
    let _ = b.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

    // Move cursor to the end of the line and paste
    let _ = b.handle_key(KeyEvent::new(KeyCode::Char('$'), KeyModifiers::NONE));
    let _ = b.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));

    assert!(b.text().contains("@a.rs"));
    assert_eq!(b.protected_ranges().len(), 1);
    let r = &b.protected_ranges()[0];
    assert_eq!(&b.text()[r.start..r.end], "@a.rs");
}

#[test]
fn vim_visual_d_preserves_atom_outside_selection() {
    let mut b = InputBar::new();
    type_str(&mut b, "abc ");
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " tail");
    b.set_mode(EditMode::Vim(VimMode::Normal));

    b.set_text_cursor_for_test(0);
    let _ = b.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    for _ in 0..3 {
        let _ = b.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
    }
    let _ = b.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));

    assert_eq!(b.text(), "@a.rs tail");
    assert_eq!(b.protected_ranges().len(), 1);
    let r = &b.protected_ranges()[0];
    assert_eq!(&b.text()[r.start..r.end], "@a.rs");
}

#[test]
fn vim_dd_preserves_atom_on_other_line() {
    let mut b = InputBar::new();
    b.set_text("delete me\nkeep ".into(), "delete me\nkeep ".len());
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    b.set_mode(EditMode::Vim(VimMode::Normal));

    b.set_text_cursor_for_test(0);
    let _ = b.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
    let _ = b.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));

    assert_eq!(b.text(), "keep @a.rs");
    assert_eq!(b.protected_ranges().len(), 1);
    let r = &b.protected_ranges()[0];
    assert_eq!(&b.text()[r.start..r.end], "@a.rs");
}
