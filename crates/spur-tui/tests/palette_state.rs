use spur_tui::components::palette::{PaletteKind, PalettePayload, PaletteResult, PaletteState};

#[test]
fn empty_state_has_empty_query_and_no_cursor_movement() {
    let state = PaletteState::new();
    assert_eq!(state.query(), "");
    assert_eq!(state.ranked().len(), 0);
    assert_eq!(state.cursor(), 0);
}

#[test]
fn push_raw_accumulates_without_ranking() {
    let mut state = PaletteState::new();
    state.push_raw(vec![
        PaletteResult {
            kind: PaletteKind::Command,
            label: "/plan".into(),
            subtitle: "cmd · toggle plan mode".into(),
            payload: PalettePayload::Command { name: "/plan".into() },
        },
    ]);
    // With empty query, raw results pass through as ranked (input order preserved,
    // matching `commands::fuzzy::rank` semantics).
    assert_eq!(state.ranked().len(), 1);
    assert_eq!(state.ranked()[0].label, "/plan");
}
