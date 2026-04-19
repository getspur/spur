use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::components::palette::{
    PaletteIntent, PaletteKind, PalettePayload, PaletteResult, PaletteState,
};

fn key(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

fn seed_state() -> PaletteState {
    let mut s = PaletteState::new();
    s.push_raw(vec![
        PaletteResult {
            kind: PaletteKind::Session,
            label: "refactor-auth".into(),
            subtitle: "".into(),
            payload: PalettePayload::Session { session_id: "s1".into() },
        },
        PaletteResult {
            kind: PaletteKind::Command,
            label: "/plan".into(),
            subtitle: "".into(),
            payload: PalettePayload::Command { name: "/plan".into() },
        },
    ]);
    s
}

#[test]
fn char_key_appends_to_query() {
    let mut s = seed_state();
    let i = s.handle_key(key(KeyCode::Char('r')));
    assert!(matches!(i, None));
    assert_eq!(s.query(), "r");
}

#[test]
fn backspace_pops_char() {
    let mut s = seed_state();
    s.set_query("refa");
    let i = s.handle_key(key(KeyCode::Backspace));
    assert!(matches!(i, None));
    assert_eq!(s.query(), "ref");
}

#[test]
fn down_moves_cursor_and_emits_no_intent() {
    let mut s = seed_state();
    let i = s.handle_key(key(KeyCode::Down));
    assert!(matches!(i, None));
    assert_eq!(s.cursor(), 1);
}

#[test]
fn enter_emits_accept_intent_with_selected_payload() {
    let mut s = seed_state();
    let i = s.handle_key(key(KeyCode::Enter));
    match i {
        Some(PaletteIntent::Accept(res)) => {
            assert_eq!(res.label, "refactor-auth");
        }
        other => panic!("expected Accept, got {:?}", other),
    }
}

#[test]
fn enter_with_empty_ranked_emits_no_intent() {
    let mut s = PaletteState::new();
    let i = s.handle_key(key(KeyCode::Enter));
    assert!(matches!(i, None));
}

#[test]
fn esc_emits_dismiss_intent() {
    let mut s = seed_state();
    let i = s.handle_key(key(KeyCode::Esc));
    assert!(matches!(i, Some(PaletteIntent::Dismiss)));
}

#[test]
fn tab_is_same_as_enter() {
    let mut s = seed_state();
    let i = s.handle_key(key(KeyCode::Tab));
    assert!(matches!(i, Some(PaletteIntent::Accept(_))));
}

#[test]
fn ctrl_c_dismisses() {
    let mut s = seed_state();
    let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let i = s.handle_key(ev);
    assert!(matches!(i, Some(PaletteIntent::Dismiss)));
}

#[test]
fn enter_on_trace_result_emits_accept_with_trace_payload() {
    let mut s = PaletteState::new();
    s.push_raw(vec![PaletteResult {
        kind: PaletteKind::Trace,
        label: "…some trace text…".into(),
        subtitle: "trace · entry #42".into(),
        payload: PalettePayload::Trace { entry_idx: 42 },
    }]);
    let ev = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    );
    match s.handle_key(ev) {
        Some(PaletteIntent::Accept(res)) => match res.payload {
            PalettePayload::Trace { entry_idx } => assert_eq!(entry_idx, 42),
            other => panic!("expected Trace payload, got {:?}", other),
        },
        other => panic!("expected Accept, got {:?}", other),
    }
}
