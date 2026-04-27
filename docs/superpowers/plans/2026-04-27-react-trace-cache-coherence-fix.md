# React-Trace Cache Coherence Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate ghost text on scroll + collapse/expand toggle while streaming by encoding the cache-staleness predicate in the type system (Approach D' from the design spec).

**Architecture:** Change `Surface` enum (`crates/spur-tui/src/components/react_trace/mod.rs:36-46`) so the `Full` and `Compact` variants carry a `u64` generation snapshot stamped by the painter. `layout_for_scroll` (`mod.rs:580-602`) — used by every scroll mutator — uses match guards `Full(g) if g == self.generation` to drop stale snapshots; stale reads return `None`, causing `shift_anchor_by` to no-op until the next render rebuilds the cache.

**Tech Stack:** Rust 1.x, ratatui, tokio (TUI crate already configured). Test backend used via existing `seed_line_cache_for_tests` helper.

**Spec:** `docs/superpowers/specs/2026-04-27-react-trace-cache-coherence-fix-design.md`.

---

## File Structure

- **Modify:** `crates/spur-tui/src/components/react_trace/mod.rs`
  - `Surface` enum at lines 35-46 (variants gain `u64`)
  - `layout_for_scroll` at lines 580-602 (match guards)
  - `seed_line_cache_for_tests` at line 1253 (stamp generation)
  - Add `mod scroll_race_test;` declaration at end of file
- **Modify:** `crates/spur-tui/src/components/react_trace/render.rs`
  - Set site at line 406 (non-markdown render)
  - Set site at line 608 (markdown render)
- **Modify:** `crates/spur-tui/src/components/react_trace/compact_render.rs`
  - Set site at line 99 (compact render)
- **Create:** `crates/spur-tui/src/components/react_trace/scroll_race_test.rs`
  - Three regression tests (streaming, toggle, compact)

---

## Task 1: Streaming-chunk Staleness Regression (TDD round 1)

**Files:**
- Create: `crates/spur-tui/src/components/react_trace/scroll_race_test.rs`
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs`
- Modify: `crates/spur-tui/src/components/react_trace/render.rs`
- Modify: `crates/spur-tui/src/components/react_trace/compact_render.rs`

- [ ] **Step 1: Create the failing test file**

Create `crates/spur-tui/src/components/react_trace/scroll_race_test.rs` with this exact content:

```rust
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
```

- [ ] **Step 2: Wire the new module into the parent**

Open `crates/spur-tui/src/components/react_trace/mod.rs` and append at the very end of the file (after the existing `markdown_integration_tests` block closes — locate the file's last `}` and add a new line below it):

```rust
#[cfg(all(test, feature = "markdown"))]
mod scroll_race_test;
```

Verify it sits at module scope (not inside another `mod {}`).

- [ ] **Step 3: Confirm the test fails (RED)**

Run:
```bash
cargo test -p spur-tui --features markdown --lib scroll_race_test::streaming_chunk_does_not_corrupt_anchor_via_stale_layout 2>&1 | tail -30
```

Expected: test FAILS with assertion mismatch on `t.anchor`. (Without the fix, `shift_anchor_by` mutates the anchor against stale layout, so the post-scroll anchor differs from `anchor_after_initial_scroll`.)

If the test PASSES at this stage, STOP — the test does not exercise the bug. Re-derive the seed (entry count, body length, scroll delta) so the second `scroll_up_by(5)` would otherwise advance the anchor when the cache is fresh.

- [ ] **Step 4: Apply Approach D' to `Surface` enum**

In `crates/spur-tui/src/components/react_trace/mod.rs` lines 35-46, replace the `Surface` enum:

```rust
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum Surface {
    /// No render has happened yet in this session.
    #[default]
    None,
    /// Last painted by the full body render. The embedded `u64` is the
    /// `ReactTrace::generation` value at paint time. Readers must verify
    /// `g == self.generation` before trusting any cached layout — see
    /// `layout_for_scroll`.
    Full(u64),
    /// Last painted by `render_compact`. Same staleness contract as `Full`.
    Compact(u64),
}
```

- [ ] **Step 5: Apply match guards to `layout_for_scroll`**

In `crates/spur-tui/src/components/react_trace/mod.rs` lines 580-602, replace the function body:

```rust
fn layout_for_scroll(&self) -> Option<(Vec<usize>, usize)> {
    match self.last_surface {
        Surface::None => None,
        Surface::Compact(g) if g == self.generation => self
            .compact_cache
            .as_ref()
            .map(|c| (c.entry_row_starts.clone(), c.lines.len())),
        Surface::Full(g) if g == self.generation => {
            #[cfg(feature = "markdown")]
            {
                self.line_cache
                    .as_ref()
                    .map(|c| (c.entry_row_starts.clone(), c.rows.len()))
            }
            #[cfg(not(feature = "markdown"))]
            {
                None
            }
        }
        _ => None,
    }
}
```

- [ ] **Step 6: Stamp generation in `seed_line_cache_for_tests`**

In `crates/spur-tui/src/components/react_trace/mod.rs` line 1253, change:

```rust
self.last_surface = Surface::Full;
```

to:

```rust
self.last_surface = Surface::Full(self.generation);
```

- [ ] **Step 7: Stamp generation in non-markdown render**

In `crates/spur-tui/src/components/react_trace/render.rs` line 406, change:

```rust
self.last_surface = crate::components::react_trace::Surface::Full;
```

to:

```rust
self.last_surface = crate::components::react_trace::Surface::Full(self.generation);
```

- [ ] **Step 8: Stamp generation in markdown render**

In `crates/spur-tui/src/components/react_trace/render.rs` line 608, change:

```rust
self.last_surface = crate::components::react_trace::Surface::Full;
```

to:

```rust
self.last_surface = crate::components::react_trace::Surface::Full(self.generation);
```

- [ ] **Step 9: Stamp generation in compact render**

In `crates/spur-tui/src/components/react_trace/compact_render.rs` line 99, change:

```rust
self.last_surface = crate::components::react_trace::Surface::Compact;
```

to:

```rust
self.last_surface = crate::components::react_trace::Surface::Compact(self.generation);
```

- [ ] **Step 10: Verify the crate still compiles**

Run:
```bash
cargo check -p spur-tui --features markdown 2>&1 | tail -20
```

Expected: exit code 0, no errors. Warnings about unused variables in test scaffolding are acceptable.

If the compiler complains about a `Surface::Full` or `Surface::Compact` literal anywhere else in the workspace, search and update — all set sites must stamp `self.generation`:

```bash
rg --type rust 'Surface::Full\b|Surface::Compact\b' crates/spur-tui/src 2>&1
```

- [ ] **Step 11: Confirm the test now passes (GREEN)**

Run:
```bash
cargo test -p spur-tui --features markdown --lib scroll_race_test::streaming_chunk_does_not_corrupt_anchor_via_stale_layout 2>&1 | tail -10
```

Expected: `test result: ok. 1 passed; 0 failed`.

- [ ] **Step 12: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs \
        crates/spur-tui/src/components/react_trace/render.rs \
        crates/spur-tui/src/components/react_trace/compact_render.rs \
        crates/spur-tui/src/components/react_trace/scroll_race_test.rs
git commit -m "$(cat <<'EOF'
fix(spur-tui): encode cache-staleness predicate in Surface enum (D')

Surface::Full and Surface::Compact now carry the generation snapshot
stamped at paint time. layout_for_scroll uses match guards
(g == self.generation) so stale reads return None; shift_anchor_by
no-ops until the next render rebuilds the cache.

Fixes the ghost-text class triggered by scroll + streaming chunks or
collapse/expand toggle. See spec
docs/superpowers/specs/2026-04-27-react-trace-cache-coherence-fix-design.md.

Adds streaming-chunk regression test.
EOF
)"
```

---

## Task 2: Toggle-staleness Regression

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/scroll_race_test.rs`

- [ ] **Step 1: Append toggle test**

Append to `crates/spur-tui/src/components/react_trace/scroll_race_test.rs`:

```rust
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
```

- [ ] **Step 2: Run the new test**

```bash
cargo test -p spur-tui --features markdown --lib scroll_race_test::toggle_observe_collapsed_does_not_corrupt_anchor_via_stale_layout 2>&1 | tail -10
```

Expected: `test result: ok. 1 passed; 0 failed`.

(This test passes immediately — Task 1's fix already covers the toggle path. The test exists for coverage of the second known trigger.)

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/scroll_race_test.rs
git commit -m "test(spur-tui): regression for toggle-induced stale-layout scroll race"
```

---

## Task 3: Compact-surface Staleness Regression

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/scroll_race_test.rs`

- [ ] **Step 1: Append compact-surface test**

This test paints the compact surface via a real `render_compact` call against a
`TestBackend`, then bumps generation and confirms the stale Compact snapshot is
rejected. Append to `crates/spur-tui/src/components/react_trace/scroll_race_test.rs`:

```rust
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
```

- [ ] **Step 2: Run the new test**

```bash
cargo test -p spur-tui --features markdown --lib scroll_race_test::stale_compact_surface_returns_none_from_layout_for_scroll 2>&1 | tail -10
```

Expected: `test result: ok. 1 passed; 0 failed`.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/scroll_race_test.rs
git commit -m "test(spur-tui): regression for stale Compact-surface scroll race"
```

---

## Task 4: Final Verification

**Files:**
- (none — verification only)

- [ ] **Step 1: Run the full scroll_race_test module**

```bash
cargo test -p spur-tui --features markdown --lib scroll_race_test 2>&1 | tail -15
```

Expected: `test result: ok. 3 passed; 0 failed`.

- [ ] **Step 2: Run the existing streaming_tests to confirm no regression**

```bash
cargo test -p spur-tui --features markdown --lib streaming_tests 2>&1 | tail -10
```

Expected: existing test count passes. Specifically, `ghost_text_rc1_regression`, `sim_fix_content_anchor_eliminates_ghost_text`, and the EDGE-3 cache-staleness tests must all still pass.

- [ ] **Step 3: Run the full spur-tui test suite**

```bash
cargo test -p spur-tui --features markdown 2>&1 | tail -15
```

Expected: zero failures across the entire crate.

- [ ] **Step 4: Run clippy with deny-warnings**

```bash
cargo clippy -p spur-tui --features markdown --all-targets -- -D warnings 2>&1 | tail -20
```

Expected: exit code 0, no warnings.

If clippy flags something in the changed code (e.g. an unused `_` binding in a match arm, or a `clone` it considers unnecessary), fix inline and re-run. Do not silence with `#[allow(...)]` unless there is a documented reason.

- [ ] **Step 5: Run cargo check on the workspace to catch downstream impact**

```bash
cargo check --workspace 2>&1 | tail -10
```

Expected: exit code 0. If a downstream crate (e.g. an integration test) references `Surface::Full` or `Surface::Compact` literals, fix them — every set site must stamp `self.generation`.

- [ ] **Step 6: If any cleanup commits were made in steps 4-5, push them**

```bash
git status --short
```

If clean, no further commit. If there are unstaged fixes from clippy or downstream:

```bash
git add <files>
git commit -m "chore(spur-tui): clippy/downstream fixups for Surface generation snapshot"
```

---

## Acceptance Verification

Before declaring complete, confirm each spec acceptance criterion:

1. ✅ `Surface` enum carries generation snapshot in `Full` and `Compact` variants → Task 1 Step 4.
2. ✅ All four set sites stamp `self.generation` → Task 1 Steps 6-9.
3. ✅ `layout_for_scroll` returns `None` when snapshot generation does not match → Task 1 Step 5.
4. ✅ `invalidate_cache` is unchanged → no edits to `mod.rs:330-333`.
5. ✅ Three new tests pass (stream / toggle / compact) → Task 4 Step 1.
6. ✅ All existing tests in `crates/spur-tui` pass → Task 4 Steps 2-3.
7. ✅ `cargo clippy --features markdown -- -D warnings` is clean → Task 4 Step 4.
8. ✅ No new comments explaining what the code does — only WHY-comments → review the diff before final push.

If any criterion is unmet, return to the relevant task before claiming the implementation complete.
