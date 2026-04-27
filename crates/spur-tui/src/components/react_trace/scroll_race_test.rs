//! Regression tests for cache coherence under generation-bumping mutations.
//!
//! Bug class: layout_for_scroll read entry_row_starts/rows.len() without
//! verifying that the cache's generation matched self.generation. Streaming
//! chunks (via mark_dirty_from) and collapse/expand toggles (via
//! invalidate_cache) bumped the trace generation, leaving layout_for_scroll
//! exposing pre-mutation layout. shift_anchor_by then mutated the anchor
//! against stale coordinates; the next render snapped the viewport, surfacing
//! as ghost text.
//!
//! Fix: Surface::{Full, Compact} carry the painted-with generation; the match
//! guard in layout_for_scroll drops stale snapshots. See spec
//! docs/superpowers/specs/2026-04-27-react-trace-cache-coherence-fix-design.md.

#![cfg(all(test, feature = "markdown"))]

use super::types::{TraceEntry, TraceKind};
use super::ReactTrace;
use std::collections::HashMap;

/// Build a trace with `n` Think entries whose body wraps to multiple rows at
/// width 80, ensuring the row total exceeds the viewport so scroll math has
/// somewhere to move the anchor.
fn seeded_trace(n: usize) -> ReactTrace {
    let mut t = ReactTrace::new();
    for i in 0..n {
        t.push(TraceEntry {
            kind: TraceKind::Think,
            text: format!("entry {i} line A\nentry {i} line B\nentry {i} line C"),
            timestamp: format!("10:{:02}", i % 60),
            markdown: None,
        });
    }
    t
}

#[test]
fn streaming_chunk_does_not_corrupt_anchor_via_stale_layout() {
    let mut t = seeded_trace(50);
    t.last_visible_height = 10;
    t.seed_line_cache_for_tests(80, &HashMap::new());

    // Seed an off-Following anchor so shift_anchor_by has something to mutate
    // (Following is a single fixed point; Row anchors are what get corrupted).
    t.scroll_up_by(5);
    let anchor_after_initial_scroll = t.anchor;

    // Simulate a streaming chunk: bump generation without re-seeding the cache.
    t.mark_dirty_from_for_update(t.entry_count() - 1);

    // Attempt to scroll. Without the fix, layout_for_scroll returns stale
    // entry_row_starts and shift_anchor_by mutates the anchor against
    // pre-streaming coordinates. With the fix, the match guard rejects the
    // stale snapshot and shift_anchor_by no-ops.
    t.scroll_up_by(5);

    assert_eq!(
        t.anchor, anchor_after_initial_scroll,
        "scroll must no-op while line_cache.generation != self.generation"
    );
}

#[test]
fn toggle_observe_collapsed_does_not_corrupt_anchor_via_stale_layout() {
    let mut t = seeded_trace(50);
    t.last_visible_height = 10;
    t.seed_line_cache_for_tests(80, &HashMap::new());

    t.scroll_up_by(5);
    let anchor_after_initial_scroll = t.anchor;

    // Toggle bumps generation via invalidate_cache without re-seeding cache.
    t.toggle_observe_collapsed();

    t.scroll_up_by(5);

    assert_eq!(
        t.anchor, anchor_after_initial_scroll,
        "scroll must no-op after toggle_observe_collapsed bumps generation"
    );
}

#[test]
fn stale_compact_surface_rejects_scroll_input() {
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::Terminal;

    let mut t = seeded_trace(50);
    t.last_visible_height = 12;

    // Paint the compact surface via a real render. This populates
    // compact_cache AND stamps last_surface = Compact(self.generation).
    let backend = TestBackend::new(80, 12);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal
        .draw(|f| {
            t.render_compact(f, Rect::new(0, 0, 80, 12));
        })
        .expect("render_compact draw");

    // Move the anchor off Following so a follow-up scroll has something to
    // mutate when the cache is fresh.
    t.scroll_up_by(3);
    let anchor_after_initial_scroll = t.anchor;

    // Bump generation without re-rendering. The Compact snapshot in
    // last_surface is now stale.
    t.toggle_observe_collapsed();

    // The match guard `Surface::Compact(g) if g == self.generation` must
    // fail; layout_for_scroll returns None; shift_anchor_by no-ops.
    t.scroll_up_by(3);

    assert_eq!(
        t.anchor, anchor_after_initial_scroll,
        "stale Compact snapshot must reject scroll input"
    );
}
