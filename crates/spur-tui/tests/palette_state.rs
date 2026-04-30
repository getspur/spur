use spur_tui::components::palette::{PaletteKind, PalettePayload, PaletteResult, PaletteState};

fn mk(kind: PaletteKind, label: &str) -> PaletteResult {
    PaletteResult {
        kind: kind.clone(),
        label: label.into(),
        subtitle: String::new(),
        payload: match kind {
            PaletteKind::View => PalettePayload::View {
                action: spur_tui::action::Action::NavigateTo(spur_tui::action::ViewId::Dashboard),
            },
            PaletteKind::Command => PalettePayload::Command { name: label.into() },
            PaletteKind::Session => PalettePayload::Session {
                session_id: label.into(),
            },
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
    assert_eq!(state.ranked_len(), 0);
    assert_eq!(state.cursor(), 0);
}

#[test]
fn push_raw_accumulates_without_ranking() {
    let mut state = PaletteState::new();
    state.push_raw(vec![PaletteResult {
        kind: PaletteKind::Command,
        label: "/plan".into(),
        subtitle: "cmd · toggle plan mode".into(),
        payload: PalettePayload::Command {
            name: "/plan".into(),
        },
    }]);
    // With empty query, raw results pass through as ranked (input order preserved,
    // matching `commands::fuzzy::rank` semantics).
    assert_eq!(state.ranked_len(), 1);
    assert_eq!(state.nth_ranked(0).unwrap().label, "/plan");
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
    let labels: Vec<&str> = s.iter_ranked().map(|r| r.label.as_str()).collect();
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
    s.cursor_down();
    assert_eq!(s.cursor(), 1);
    s.cursor_down();
    assert_eq!(s.cursor(), 2);
    s.cursor_down();
    assert_eq!(s.cursor(), 2); // clamped, no wrap
    s.cursor_up();
    assert_eq!(s.cursor(), 1);
    s.cursor_up();
    s.cursor_up();
    assert_eq!(s.cursor(), 0); // clamped at 0
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
    assert_eq!(s.ranked_len(), 0);
    assert_eq!(s.cursor(), 0);
}

#[test]
fn subtitle_only_match_is_ranked() {
    // Label has no match; subtitle (session id) does.
    let mut s = PaletteState::new();
    s.push_raw(vec![PaletteResult {
        kind: PaletteKind::Session,
        label: "human friendly title".into(),
        subtitle: "session · 7f3b0c1d".into(),
        payload: PalettePayload::Session {
            session_id: "7f3b0c1d".into(),
        },
    }]);
    s.set_query("7f3b");
    assert_eq!(
        s.ranked_len(),
        1,
        "query matching only the subtitle should still rank the row"
    );
    assert_eq!(s.nth_ranked(0).unwrap().label, "human friendly title");
}

#[test]
fn label_match_beats_weaker_subtitle_match() {
    // Two rows: first has the query in its subtitle only; second has it in the label.
    // The label-match row should rank above the subtitle-match row given the 0.7x weight.
    let mut s = PaletteState::new();
    s.push_raw(vec![
        PaletteResult {
            kind: PaletteKind::Session,
            label: "zzz unrelated".into(),
            subtitle: "session · alpha-match".into(),
            payload: PalettePayload::Session {
                session_id: "sub-id".into(),
            },
        },
        PaletteResult {
            kind: PaletteKind::Session,
            label: "alpha in label".into(),
            subtitle: "session · unrelated".into(),
            payload: PalettePayload::Session {
                session_id: "lbl-id".into(),
            },
        },
    ]);
    s.set_query("alpha");
    let labels: Vec<&str> = s.iter_ranked().map(|r| r.label.as_str()).collect();
    assert_eq!(labels.len(), 2);
    assert_eq!(
        labels[0], "alpha in label",
        "label match should rank above subtitle-only match"
    );
}

#[test]
fn subtitle_weight_actually_demotes_subtitle_only_matches() {
    // Two entries with IDENTICAL fuzzy-match material:
    //   * Entry A: label = "alpha" (perfect short match), subtitle = "irrelevant"
    //   * Entry B: label = "irrelevant", subtitle = "alpha" (perfect short match)
    //
    // nucleo gives both the same raw score against query "alpha". Without
    // the subtitle weight, the entries would tie and `sort_by` (stable)
    // would preserve insertion order — so the FIRST-inserted entry would
    // rank first. We insert B first deliberately, so a weight of 1.0
    // would put B at rank 0. Any weight < 1.0 puts A at rank 0.
    //
    // This test fails if SUBTITLE_WEIGHT is regressed to 1.0.
    let mut s = PaletteState::new();
    s.push_raw(vec![
        // Insert subtitle-only match FIRST so insertion-order tie-break
        // would favor it under weight=1.0.
        PaletteResult {
            kind: PaletteKind::Session,
            label: "irrelevant".into(),
            subtitle: "alpha".into(),
            payload: PalettePayload::Session {
                session_id: "b".into(),
            },
        },
        PaletteResult {
            kind: PaletteKind::Session,
            label: "alpha".into(),
            subtitle: "irrelevant".into(),
            payload: PalettePayload::Session {
                session_id: "a".into(),
            },
        },
    ]);
    s.set_query("alpha");
    assert_eq!(s.ranked_len(), 2);
    assert_eq!(
        s.nth_ranked(0).unwrap().label,
        "alpha",
        "label-only match must outrank subtitle-only match when raw scores tie; \
         this assertion fails if SUBTITLE_WEIGHT is regressed to 1.0"
    );
}
