//! Performance smoke bench for `PaletteState::rerank`.
//!
//! Runs on every `cargo test` and enforces the performance invariant
//! documented on `PaletteState`. If this test starts failing, it means a
//! refactor has reintroduced per-keystroke cloning or N-bounded allocations.
//! Fix the refactor; do NOT loosen the thresholds.
//!
//! Thresholds are split by compile profile:
//!   - Debug builds run nucleo_matcher unoptimized (Pattern::parse alone
//!     is ~10-20× slower). Debug budgets are loose enough to pass on a
//!     developer laptop without the macro profile optimisation, but still
//!     tight enough to catch pathological regressions (a reintroduced
//!     clone-per-entry path would push rerank into the 100+ ms range).
//!   - Release builds reflect real production cost — sub-millisecond at
//!     N=500. Developers doing perf work should run `cargo test --release`.

use std::time::{Duration, Instant};

use spur_tui::components::palette::{PaletteKind, PalettePayload, PaletteResult, PaletteState};

fn make_results(n: usize) -> Vec<PaletteResult> {
    (0..n)
        .map(|i| PaletteResult {
            kind: PaletteKind::Session,
            label: format!("session-{}-refactor-auth-module-endpoint-{}", i, i * 3),
            subtitle: format!("session · synthetic #{}", i),
            payload: PalettePayload::Session { session_id: format!("s{}", i) },
        })
        .collect()
}

#[test]
fn rerank_at_n500_per_keystroke_is_within_budget() {
    let mut state = PaletteState::new();
    state.extend_raw(vec![make_results(500)]);

    // Debug builds run nucleo unoptimized (~10-20× slower). A 25ms debug
    // budget still catches pathological regressions (reintroduced cloning
    // would push this to 100+ms), while 3ms in release enforces the real
    // perf contract for perf-conscious developers.
    let keystroke_budget_ms = if cfg!(debug_assertions) { 25 } else { 3 };
    let cumulative_budget_ms = if cfg!(debug_assertions) { 150 } else { 20 };

    let mut total = Duration::ZERO;
    for c in "refactor".chars() {
        let t = Instant::now();
        state.push_char(c);
        let dt = t.elapsed();
        assert!(
            dt.as_millis() < keystroke_budget_ms,
            "per-keystroke rerank exceeded {}ms at N=500: {:?} after '{}' \
             (query now {:?})",
            keystroke_budget_ms,
            dt,
            c,
            state.query()
        );
        total += dt;
    }
    assert!(
        total.as_millis() < cumulative_budget_ms,
        "cumulative 8-char rerank exceeded {}ms at N=500: {:?}",
        cumulative_budget_ms,
        total
    );

    // Correctness invariant: cursor is valid when ranked is non-empty.
    let rl = state.ranked_len();
    assert!(rl <= 500, "ranked_len exceeds N");
    if rl > 0 {
        assert!(state.selected().is_some(), "selected must be Some when ranked non-empty");
    }
}

#[test]
fn rerank_on_empty_query_at_n500_is_within_budget() {
    let mut state = PaletteState::new();
    state.extend_raw(vec![make_results(500)]);
    // Force a non-empty then back to empty to exercise the empty-query rerank path.
    state.set_query("nomatch");
    let t = Instant::now();
    state.set_query("");
    let dt = t.elapsed();
    let budget_ms = if cfg!(debug_assertions) { 10 } else { 3 };
    assert!(
        dt.as_millis() < budget_ms,
        "empty-query rerank exceeded {}ms at N=500: {:?}",
        budget_ms,
        dt
    );
    assert_eq!(state.ranked_len(), 500, "empty query must yield all entries");
}

#[test]
fn rerank_cursor_stays_valid_across_query_mutation() {
    let mut state = PaletteState::new();
    state.extend_raw(vec![make_results(500)]);
    state.cursor_down();
    state.cursor_down();
    state.cursor_down(); // cursor at 3
    state.set_query("refactor"); // shrinks ranked dramatically
    // cursor must be clamped to the new (smaller) ranked length.
    assert!(state.cursor() < state.ranked_len().max(1));
    if state.ranked_len() > 0 {
        assert!(state.selected().is_some());
    }
}
