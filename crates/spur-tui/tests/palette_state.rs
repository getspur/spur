use spur_tui::components::palette::{PaletteKind, PalettePayload, PaletteResult, PaletteState};

fn mk(kind: PaletteKind, label: &str) -> PaletteResult {
    PaletteResult {
        kind: kind.clone(),
        label: label.into(),
        subtitle: String::new(),
        payload: match kind {
            PaletteKind::Command => PalettePayload::Command { name: label.into() },
            PaletteKind::Session => PalettePayload::Session { session_id: label.into() },
            PaletteKind::Worker => PalettePayload::Worker {
                session_id: spur_acp::SessionId(label.into()),
            },
            PaletteKind::Trace => PalettePayload::Trace { entry_idx: 0 },
        },
    }
}

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

#[test]
fn set_query_reranks_by_fuzzy_score_and_drops_unmatched() {
    let mut s = PaletteState::new();
    s.push_raw(vec![
        mk(PaletteKind::Session, "refactor-auth"),
        mk(PaletteKind::Session, "debug-ci-flake"),
        mk(PaletteKind::Worker, "refactor-auth-async"),
    ]);
    s.set_query("refac");
    let labels: Vec<&str> = s.ranked().iter().map(|r| r.label.as_str()).collect();
    assert!(labels.contains(&"refactor-auth"));
    assert!(labels.contains(&"refactor-auth-async"));
    assert!(!labels.contains(&"debug-ci-flake"));
}

#[test]
fn cursor_up_down_stay_in_bounds_and_wrap_disabled() {
    let mut s = PaletteState::new();
    s.push_raw(vec![
        mk(PaletteKind::Command, "/a"),
        mk(PaletteKind::Command, "/b"),
        mk(PaletteKind::Command, "/c"),
    ]);
    assert_eq!(s.cursor(), 0);
    s.cursor_down(); assert_eq!(s.cursor(), 1);
    s.cursor_down(); assert_eq!(s.cursor(), 2);
    s.cursor_down(); assert_eq!(s.cursor(), 2); // clamped, no wrap
    s.cursor_up();   assert_eq!(s.cursor(), 1);
    s.cursor_up();   s.cursor_up(); assert_eq!(s.cursor(), 0); // clamped at 0
}

#[test]
fn selected_returns_current_cursor_row() {
    let mut s = PaletteState::new();
    s.push_raw(vec![
        mk(PaletteKind::Command, "/alpha"),
        mk(PaletteKind::Command, "/beta"),
    ]);
    s.cursor_down();
    assert_eq!(s.selected().unwrap().label, "/beta");
}

#[test]
fn selected_returns_none_when_ranked_is_empty() {
    let s = PaletteState::new();
    assert!(s.selected().is_none());
}

#[test]
fn reset_clears_query_and_raw_but_not_state_struct() {
    let mut s = PaletteState::new();
    s.push_raw(vec![mk(PaletteKind::Command, "/x")]);
    s.set_query("x");
    s.reset();
    assert_eq!(s.query(), "");
    assert_eq!(s.ranked().len(), 0);
    assert_eq!(s.cursor(), 0);
}
