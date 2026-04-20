use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::components::completion_trigger::IntentEvent;
use spur_tui::components::input_bar::{HandleOutcome, InputBar};

fn press(bar: &mut InputBar, code: KeyCode) -> IntentEvent {
    match bar.handle_key(KeyEvent::new(code, KeyModifiers::NONE)) {
        HandleOutcome::Key(e) => e,
        HandleOutcome::Submit(_, _) => panic!("unexpected submit"),
    }
}

fn press_ctrl(bar: &mut InputBar, c: char) -> IntentEvent {
    match bar.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)) {
        HandleOutcome::Key(e) => e,
        HandleOutcome::Submit(_, _) => panic!("unexpected submit"),
    }
}

#[test]
fn arrow_keys_classify_as_moved_cursor() {
    let mut bar = InputBar::new();
    bar.set_text("abcdef".into(), 3);
    assert!(matches!(press(&mut bar, KeyCode::Left), IntentEvent::MovedCursor));
    assert!(matches!(press(&mut bar, KeyCode::Right), IntentEvent::MovedCursor));
    assert!(matches!(press(&mut bar, KeyCode::Up), IntentEvent::MovedCursor));
    assert!(matches!(press(&mut bar, KeyCode::Down), IntentEvent::MovedCursor));
}

#[test]
fn backspace_classifies_as_deleted_char() {
    let mut bar = InputBar::new();
    bar.set_text("abc".into(), 3);
    assert!(matches!(press(&mut bar, KeyCode::Backspace), IntentEvent::DeletedChar));
}

#[test]
fn delete_classifies_as_deleted_char() {
    let mut bar = InputBar::new();
    bar.set_text("abc".into(), 0);
    assert!(matches!(press(&mut bar, KeyCode::Delete), IntentEvent::DeletedChar));
}

#[test]
fn ctrl_k_classifies_as_deleted_char() {
    let mut bar = InputBar::new();
    bar.set_text("hello world".into(), 5);
    assert!(matches!(press_ctrl(&mut bar, 'k'), IntentEvent::DeletedChar));
}

#[test]
fn ctrl_u_classifies_as_deleted_char() {
    let mut bar = InputBar::new();
    bar.set_text("hello world".into(), 5);
    assert!(matches!(press_ctrl(&mut bar, 'u'), IntentEvent::DeletedChar));
}

#[test]
fn ctrl_w_classifies_as_deleted_char() {
    let mut bar = InputBar::new();
    bar.set_text("hello world".into(), 11);
    assert!(matches!(press_ctrl(&mut bar, 'w'), IntentEvent::DeletedChar));
}

#[test]
fn printable_char_classifies_as_typed_char_with_value() {
    let mut bar = InputBar::new();
    match bar.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)) {
        HandleOutcome::Key(IntentEvent::TypedChar('x')) => {}
        other => panic!("expected TypedChar('x'), got {other:?}"),
    }
}

#[test]
fn at_sign_classifies_as_typed_char_at() {
    let mut bar = InputBar::new();
    match bar.handle_key(KeyEvent::new(KeyCode::Char('@'), KeyModifiers::NONE)) {
        HandleOutcome::Key(IntentEvent::TypedChar('@')) => {}
        other => panic!("expected TypedChar('@'), got {other:?}"),
    }
}

#[test]
fn enter_on_nonempty_returns_submit() {
    let mut bar = InputBar::new();
    bar.set_text("hello".into(), 5);
    match bar.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
        HandleOutcome::Submit(t, interrupt) => {
            assert_eq!(t, "hello");
            assert!(!interrupt);
        }
        other => panic!("expected Submit, got {other:?}"),
    }
}

#[test]
fn enter_on_empty_returns_noop() {
    let mut bar = InputBar::new();
    match bar.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
        HandleOutcome::Key(IntentEvent::NoOp) => {}
        other => panic!("expected NoOp, got {other:?}"),
    }
}

#[test]
fn home_end_classify_as_moved_cursor() {
    let mut bar = InputBar::new();
    bar.set_text("abc".into(), 3);
    assert!(matches!(press(&mut bar, KeyCode::Home), IntentEvent::MovedCursor));
    assert!(matches!(press(&mut bar, KeyCode::End), IntentEvent::MovedCursor));
}
