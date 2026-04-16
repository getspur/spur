use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::components::input_bar::InputBar;

fn press(bar: &mut InputBar, code: KeyCode) {
    bar.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}
fn ctrl(bar: &mut InputBar, c: char) {
    bar.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
}
fn type_str(bar: &mut InputBar, s: &str) {
    for c in s.chars() {
        press(bar, KeyCode::Char(c));
    }
}
fn submit(bar: &mut InputBar) {
    press(bar, KeyCode::Enter);
}

// ── Ctrl+U: kill to start ───────────────────────────────────────────────

#[test]
fn ctrl_u_deletes_to_start_of_line() {
    let mut b = InputBar::new();
    type_str(&mut b, "hello world");
    // Move cursor to after "hello "
    b.set_text_cursor_for_test(6);
    ctrl(&mut b, 'u');
    assert_eq!(b.text(), "world");
    assert_eq!(b.cursor(), 0);
}

#[test]
fn ctrl_u_at_start_is_noop() {
    let mut b = InputBar::new();
    type_str(&mut b, "hello");
    b.set_text_cursor_for_test(0);
    ctrl(&mut b, 'u');
    assert_eq!(b.text(), "hello");
}

// ── Ctrl+K: kill to end ─────────────────────────────────────────────────

#[test]
fn ctrl_k_deletes_to_end_of_line() {
    let mut b = InputBar::new();
    type_str(&mut b, "hello world");
    b.set_text_cursor_for_test(5);
    ctrl(&mut b, 'k');
    assert_eq!(b.text(), "hello");
    assert_eq!(b.cursor(), 5);
}

#[test]
fn ctrl_k_at_end_is_noop() {
    let mut b = InputBar::new();
    type_str(&mut b, "hello");
    ctrl(&mut b, 'k');
    assert_eq!(b.text(), "hello");
}

// ── Ctrl+W: delete previous word ────────────────────────────────────────

#[test]
fn ctrl_w_deletes_previous_word() {
    let mut b = InputBar::new();
    type_str(&mut b, "hello world");
    ctrl(&mut b, 'w');
    assert_eq!(b.text(), "hello ");
    assert_eq!(b.cursor(), 6);
}

#[test]
fn ctrl_w_skips_trailing_whitespace() {
    let mut b = InputBar::new();
    type_str(&mut b, "hello   ");
    ctrl(&mut b, 'w');
    assert_eq!(b.text(), "");
    assert_eq!(b.cursor(), 0);
}

#[test]
fn ctrl_w_at_start_is_noop() {
    let mut b = InputBar::new();
    type_str(&mut b, "hello");
    b.set_text_cursor_for_test(0);
    ctrl(&mut b, 'w');
    assert_eq!(b.text(), "hello");
}

// ── History: Ctrl+P / Ctrl+N ────────────────────────────────────────────

#[test]
fn history_prev_recalls_last_submitted() {
    let mut b = InputBar::new();
    type_str(&mut b, "first");
    submit(&mut b);
    type_str(&mut b, "second");
    submit(&mut b);

    b.history_prev();
    assert_eq!(b.text(), "second");
    b.history_prev();
    assert_eq!(b.text(), "first");
}

#[test]
fn history_prev_on_empty_history_is_noop() {
    let mut b = InputBar::new();
    type_str(&mut b, "draft");
    b.history_prev();
    assert_eq!(b.text(), "draft");
}

#[test]
fn history_next_restores_draft() {
    let mut b = InputBar::new();
    type_str(&mut b, "submitted");
    submit(&mut b);
    type_str(&mut b, "my draft");

    b.history_prev();
    assert_eq!(b.text(), "submitted");

    b.history_next();
    assert_eq!(b.text(), "my draft");
}

#[test]
fn history_next_past_newest_is_noop() {
    let mut b = InputBar::new();
    type_str(&mut b, "one");
    submit(&mut b);

    // Already at newest (no browsing), next should be noop
    b.history_next();
    assert_eq!(b.text(), "");
}

#[test]
fn history_prev_stops_at_oldest() {
    let mut b = InputBar::new();
    type_str(&mut b, "only");
    submit(&mut b);

    b.history_prev();
    assert_eq!(b.text(), "only");
    b.history_prev(); // should stay at oldest
    assert_eq!(b.text(), "only");
}

#[test]
fn enter_pushes_to_history() {
    let mut b = InputBar::new();
    type_str(&mut b, "msg1");
    submit(&mut b);
    type_str(&mut b, "msg2");
    submit(&mut b);
    type_str(&mut b, "msg3");
    submit(&mut b);

    b.history_prev();
    assert_eq!(b.text(), "msg3");
    b.history_prev();
    assert_eq!(b.text(), "msg2");
    b.history_prev();
    assert_eq!(b.text(), "msg1");
}

#[test]
fn typing_while_browsing_history_exits_history_mode() {
    let mut b = InputBar::new();
    type_str(&mut b, "first");
    submit(&mut b);
    type_str(&mut b, "second");
    submit(&mut b);

    // Browse back to "first"
    b.history_prev();
    b.history_prev();
    assert_eq!(b.text(), "first");

    // Type a character — should exit history mode
    press(&mut b, KeyCode::Char('!'));
    assert_eq!(b.text(), "first!");

    // Ctrl+P should now save "first!" as draft and recall "second"
    b.history_prev();
    assert_eq!(b.text(), "second");

    // Ctrl+N back should restore "first!" (the modified draft)
    b.history_next();
    assert_eq!(b.text(), "first!");
}
