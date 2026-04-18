# Session Detail Scroll-Anchor Phase 3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `ScrollAnchor::Byte` with `ScrollAnchor::Row { entry_idx, row_within_entry }` and route `shift_anchor_by` through the cached layout, fixing the P1 sub-entry scroll bug and the P2 mermaid-state mismatch revealed by the post-merge L9 audit.

**Architecture:** A disciplined enum migration: introduce `Row` alongside `Byte`, migrate consumers one commit at a time, then drop `Byte`. `shift_anchor_by` reads `entry_row_starts` from `self.line_cache` (populated by render with the live mermaid registry) instead of re-running `build_virtual_rows` with an empty registry.

**Tech Stack:** Rust 2021, ratatui 0.26, tui-markdown, pulldown-cmark, tokio broadcast, `cargo test -p spur-tui --features markdown`.

**Spec:** `docs/superpowers/specs/2026-04-18-session-detail-scroll-anchor-phase3-design.md`

---

## File Map

| File | Responsibility | Touched By |
|---|---|---|
| `crates/spur-tui/src/components/react_trace/types.rs` | `ScrollAnchor` enum definition | T2, T5 |
| `crates/spur-tui/src/components/react_trace/render.rs` | `resolve_anchor`, `VirtualRowCacheEntry`, render unit tests | T2, T5 |
| `crates/spur-tui/src/components/react_trace/mod.rs` | `row_to_anchor`, `shift_anchor_by`, `scroll_to_top`, `push` eviction | T3, T4, T5 |
| `crates/spur-tui/src/components/react_trace/streaming_tests.rs` | SIM tests, EDGE/COUNTER tests | T1, T5, T6, T7, T8, T9 |
| `docs/superpowers/specs/2026-04-18-session-detail-streaming-ghost-text-rca.md` | Resolution footer | T10 |

---

## Task 1: Red Baseline — un-ignore failing SIMs

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/streaming_tests.rs:919, 983, 1028` (remove `#[ignore]` from SIM-9, SIM-10, SIM-11; SIM-13 not yet present, will add later)

Note: SIM-13 was added in `f91af27` but inspection shows it lives in the same module — verify by grepping. If SIM-13 has its own `#[ignore]`, remove it too in this task.

- [ ] **Step 1: Locate the four `#[ignore]` markers**

```bash
grep -n "#\[ignore = \"L9 audit" crates/spur-tui/src/components/react_trace/streaming_tests.rs
```

Expected output: 4 lines (SIM-9, SIM-10, SIM-11, SIM-13).

- [ ] **Step 2: Remove each `#[ignore = "L9 audit: ..."]` line**

For each match found in Step 1, delete that single line using Edit. Example for SIM-9:

```rust
// before
#[ignore = "L9 audit: confirmed production bug, awaiting sub-entry byte granularity fix"]
#[test]
fn sim_sub_entry_scroll_resolution() {

// after
#[test]
fn sim_sub_entry_scroll_resolution() {
```

Apply the same removal to SIM-10, SIM-11, SIM-13.

- [ ] **Step 3: Run the SIMs and confirm they fail in the expected way**

```bash
cargo test -p spur-tui --features markdown sim_sub_entry sim_mermaid_state sim_page_up sim_render_offset 2>&1 | tail -40
```

Expected: 4 failures with these assertion messages:
- `SIM-9: scroll_up did not change the anchor — sub-entry scroll is broken`
- `SIM-10: shift_anchor_by computes row count using empty fence registry`
- `SIM-11: two consecutive page_ups within a long single entry produced the same anchor`
- `SIM-13: scroll_down_by(5) did not advance the rendered row index`

- [ ] **Step 4: Commit the red baseline**

```bash
git add crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "$(cat <<'EOF'
test(spur-tui): un-ignore Phase 3 failing SIMs as red baseline

SIM-9, SIM-10, SIM-11, SIM-13 reproduce the P1 sub-entry scroll bug
and P2 mermaid-state mismatch. They fail as expected and will pass
after the Phase 3 Row-anchor migration.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Add `ScrollAnchor::Row` variant alongside `Byte`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/types.rs:94-101`
- Modify: `crates/spur-tui/src/components/react_trace/render.rs:50-93` (extend `resolve_anchor` to handle Row)
- Test: new unit test in `crates/spur-tui/src/components/react_trace/render.rs` `mod tests`

- [ ] **Step 1: Write the failing unit test for `resolve_anchor` with a Row anchor**

Append to the `mod tests` block in `crates/spur-tui/src/components/react_trace/render.rs` (after the existing `evicted_entry_snaps_to_zero` test):

```rust
#[test]
fn row_anchor_resolves_within_entry() {
    use crate::components::react_trace::types::ScrollAnchor;
    let ranges = ranges(&[Some(0..50), Some(0..50), Some(0..30), Some(0..30)]);
    let entry_starts = vec![0, 2];
    let anchor = ScrollAnchor::Row {
        entry_idx: 1,
        row_within_entry: 1,
    };
    let row = resolve_anchor(&anchor, &ranges, &entry_starts, 4, 2);
    assert_eq!(row, 3, "Row{{1,1}} resolves to entry_starts[1]+1 = 3");
}

#[test]
fn row_anchor_clamps_to_entry_last() {
    use crate::components::react_trace::types::ScrollAnchor;
    let ranges = ranges(&[Some(0..50), Some(0..50), Some(0..30), Some(0..30)]);
    let entry_starts = vec![0, 2];
    // Entry 1 is 2 rows (rows 2-3); asking row_within_entry=99 must clamp.
    let anchor = ScrollAnchor::Row {
        entry_idx: 1,
        row_within_entry: 99,
    };
    let row = resolve_anchor(&anchor, &ranges, &entry_starts, 4, 2);
    assert_eq!(row, 3, "row_within_entry=99 clamps to entry's last row (3)");
}

#[test]
fn row_anchor_evicted_entry_snaps_to_zero() {
    use crate::components::react_trace::types::ScrollAnchor;
    let ranges = ranges(&[Some(0..50), Some(0..50)]);
    let entry_starts = vec![0];
    let anchor = ScrollAnchor::Row {
        entry_idx: 5,
        row_within_entry: 0,
    };
    let row = resolve_anchor(&anchor, &ranges, &entry_starts, 2, 1);
    assert_eq!(row, 0);
}
```

- [ ] **Step 2: Run to confirm it fails (variant doesn't exist yet)**

```bash
cargo test -p spur-tui --features markdown row_anchor_resolves 2>&1 | tail -10
```

Expected: compile error `no variant or associated item named Row found for enum ScrollAnchor`.

- [ ] **Step 3: Add the `Row` variant to `ScrollAnchor`**

Edit `crates/spur-tui/src/components/react_trace/types.rs`:

```rust
// before (lines 87-101)
/// Anchor model for the trace viewport. Replaces the legacy `scroll_offset:
/// usize` row index, which was unstable under reflow (RCA Layer 3E).
///
/// `Following` tracks the bottom of the document.
/// `Byte` pins the viewport top to a specific byte position within an
/// entry's content; resolved to a row index at render time against
/// per-row byte ranges from `build_virtual_rows`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAnchor {
    Following,
    Byte {
        entry_idx: usize,
        byte_offset: usize,
    },
}

// after
/// Anchor model for the trace viewport.
///
/// `Following` tracks the bottom of the document.
/// `Row` pins the viewport to ordinal row `row_within_entry` of the entry
/// at `entry_idx`. Resolved at render time via `entry_row_starts`. Width
/// resize clamps to the entry's last row (Phase 3 trade-off; v1 byte
/// anchor was entry-coarse and snapped to entry start anyway).
/// `Byte` is the legacy variant kept during Phase 3 migration; will be
/// removed in the final task of this plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAnchor {
    Following,
    Row {
        entry_idx: usize,
        row_within_entry: usize,
    },
    Byte {
        entry_idx: usize,
        byte_offset: usize,
    },
}
```

- [ ] **Step 4: Extend `resolve_anchor` to handle the new variant**

Edit `crates/spur-tui/src/components/react_trace/render.rs:50-93`. Replace the entire function body:

```rust
#[cfg(feature = "markdown")]
pub(crate) fn resolve_anchor(
    anchor: &crate::components::react_trace::types::ScrollAnchor,
    byte_ranges: &[Option<std::ops::Range<usize>>],
    entry_row_starts: &[usize],
    total_rows: usize,
    visible_height: usize,
) -> usize {
    use crate::components::react_trace::types::ScrollAnchor;
    match anchor {
        ScrollAnchor::Following => total_rows.saturating_sub(visible_height),
        ScrollAnchor::Row {
            entry_idx,
            row_within_entry,
        } => {
            if *entry_idx >= entry_row_starts.len() {
                return 0;
            }
            let row_start = entry_row_starts[*entry_idx];
            let row_end = entry_row_starts
                .get(*entry_idx + 1)
                .copied()
                .unwrap_or(total_rows);
            let entry_height = row_end.saturating_sub(row_start);
            let clamped = (*row_within_entry).min(entry_height.saturating_sub(1));
            row_start + clamped
        }
        ScrollAnchor::Byte {
            entry_idx,
            byte_offset,
        } => {
            if *entry_idx >= entry_row_starts.len() {
                return 0;
            }
            let row_start = entry_row_starts[*entry_idx];
            let row_end = entry_row_starts
                .get(*entry_idx + 1)
                .copied()
                .unwrap_or(total_rows);
            for i in row_start..row_end.min(byte_ranges.len()) {
                if let Some(r) = &byte_ranges[i] {
                    if r.contains(byte_offset) {
                        return i;
                    }
                }
            }
            let mut snap = row_start;
            for i in row_start..row_end.min(byte_ranges.len()) {
                if let Some(r) = &byte_ranges[i] {
                    if r.start <= *byte_offset {
                        snap = i;
                    }
                }
            }
            snap
        }
    }
}
```

- [ ] **Step 5: Run unit tests to confirm Row variant resolves correctly**

```bash
cargo test -p spur-tui --features markdown -- row_anchor_resolves row_anchor_clamps row_anchor_evicted byte_anchor 2>&1 | tail -10
```

Expected: PASS for all `row_anchor_*` tests AND existing `byte_anchor_resolves_to_containing_row` (Byte variant still works).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/types.rs crates/spur-tui/src/components/react_trace/render.rs
git commit -m "$(cat <<'EOF'
feat(spur-tui): add ScrollAnchor::Row variant + resolve_anchor support

Row{entry_idx, row_within_entry} is the new scroll coordinate; resolves
via entry_row_starts[entry_idx]+row_within_entry, clamped to entry's
last row. Byte variant retained for migration; consumers updated in
follow-up tasks.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Add `row_to_anchor` function in `mod.rs`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs:58-74` (add new function alongside existing `row_to_byte_anchor`)

- [ ] **Step 1: Write the failing unit test**

Append to the bottom of `crates/spur-tui/src/components/react_trace/mod.rs` inside `mod tests`:

```rust
#[cfg(feature = "markdown")]
#[test]
fn row_to_anchor_walks_entry_row_starts() {
    let starts = vec![0, 5, 12];
    assert_eq!(super::row_to_anchor(0, &starts), (0, 0));
    assert_eq!(super::row_to_anchor(4, &starts), (0, 4));
    assert_eq!(super::row_to_anchor(5, &starts), (1, 0));
    assert_eq!(super::row_to_anchor(11, &starts), (1, 6));
    assert_eq!(super::row_to_anchor(12, &starts), (2, 0));
}
```

- [ ] **Step 2: Run to confirm it fails (function does not exist)**

```bash
cargo test -p spur-tui --features markdown row_to_anchor_walks 2>&1 | tail -10
```

Expected: compile error `cannot find function row_to_anchor in module super`.

- [ ] **Step 3: Add the function**

Insert into `crates/spur-tui/src/components/react_trace/mod.rs` immediately after the existing `row_to_byte_anchor` function (around line 74):

```rust
/// Inverse of `resolve_anchor` for the Row variant: given a row index,
/// find which entry it belongs to and the row-within-entry offset.
#[cfg(feature = "markdown")]
fn row_to_anchor(row: usize, entry_row_starts: &[usize]) -> (usize, usize) {
    if entry_row_starts.is_empty() {
        return (0, 0);
    }
    let entry_idx = match entry_row_starts.binary_search(&row) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };
    let within = row - entry_row_starts[entry_idx];
    (entry_idx, within)
}
```

- [ ] **Step 4: Run the test to confirm it passes**

```bash
cargo test -p spur-tui --features markdown row_to_anchor_walks 2>&1 | tail -5
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "$(cat <<'EOF'
feat(spur-tui): add row_to_anchor helper for Row scroll variant

Inverse of resolve_anchor: maps an absolute row index to
(entry_idx, row_within_entry) via binary search of entry_row_starts.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Rewrite `shift_anchor_by`, `scroll_to_top`, and `push` eviction for Row anchor

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs:309-341` (push)
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs:376-381` (scroll_to_top)
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs:393-419` (shift_anchor_by)

This task makes the four failing SIMs pass. It is the core of the fix.

- [ ] **Step 1: Confirm current SIM-9, 10, 11, 13 still fail (red gate)**

```bash
cargo test -p spur-tui --features markdown sim_sub_entry sim_mermaid_state sim_page_up sim_render_offset 2>&1 | tail -10
```

Expected: 4 failures (same as Task 1 Step 3).

- [ ] **Step 2: Rewrite `shift_anchor_by` to read from `line_cache`**

Replace `crates/spur-tui/src/components/react_trace/mod.rs:392-419` (the `#[cfg(feature = "markdown")] fn shift_anchor_by` body):

```rust
/// Apply a row delta to the current anchor by:
/// 1. resolving the current anchor against the cached layout from the
///    most recent render (P2-δ — guarantees scroll math uses the same
///    coordinate system render painted with),
/// 2. computing the target row,
/// 3. converting back to a Row anchor at the target row.
/// If `line_cache` is `None` (first tick before any render), this is a
/// no-op — anchor remains in its initial state.
/// If the target row is the last visible row, transitions to Following.
#[cfg(feature = "markdown")]
fn shift_anchor_by(&mut self, delta: isize) {
    use crate::components::react_trace::types::ScrollAnchor;

    let Some(cache) = self.line_cache.as_ref() else {
        return;
    };
    let total = cache.rows.len();
    let visible_h = self.last_visible_height.max(1);

    let current_row = crate::components::react_trace::render::resolve_anchor(
        &self.anchor,
        &cache.byte_ranges,
        &cache.entry_row_starts,
        total,
        visible_h,
    );

    let target = (current_row as isize + delta)
        .max(0)
        .min(total.saturating_sub(visible_h) as isize) as usize;

    if target >= total.saturating_sub(visible_h) {
        self.anchor = ScrollAnchor::Following;
        return;
    }

    let (entry_idx, row_within_entry) = row_to_anchor(target, &cache.entry_row_starts);
    self.anchor = ScrollAnchor::Row {
        entry_idx,
        row_within_entry,
    };
}
```

- [ ] **Step 3: Update `scroll_to_top` to emit a Row anchor**

Replace `crates/spur-tui/src/components/react_trace/mod.rs:376-381`:

```rust
pub fn scroll_to_top(&mut self) {
    self.anchor = crate::components::react_trace::types::ScrollAnchor::Row {
        entry_idx: 0,
        row_within_entry: 0,
    };
}
```

- [ ] **Step 4: Update `push` eviction to handle the Row variant**

Replace the eviction branch in `crates/spur-tui/src/components/react_trace/mod.rs:309-341`:

```rust
/// Push a new trace entry, evicting oldest if over capacity.
pub fn push(&mut self, entry: TraceEntry) {
    self.entries.push(entry);
    if self.entries.len() > MAX_LOG_ENTRIES {
        let drain = self.entries.len() - MAX_LOG_ENTRIES;
        self.entries.drain(..drain);
        // Adjust anchor's entry_idx; if anchor pointed at evicted entry,
        // snap to the first surviving entry's first row.
        match self.anchor {
            crate::components::react_trace::types::ScrollAnchor::Row {
                entry_idx,
                row_within_entry,
            } => {
                self.anchor = if entry_idx < drain {
                    crate::components::react_trace::types::ScrollAnchor::Row {
                        entry_idx: 0,
                        row_within_entry: 0,
                    }
                } else {
                    crate::components::react_trace::types::ScrollAnchor::Row {
                        entry_idx: entry_idx - drain,
                        row_within_entry,
                    }
                };
            }
            crate::components::react_trace::types::ScrollAnchor::Byte {
                entry_idx,
                byte_offset,
            } => {
                self.anchor = if entry_idx < drain {
                    crate::components::react_trace::types::ScrollAnchor::Byte {
                        entry_idx: 0,
                        byte_offset: 0,
                    }
                } else {
                    crate::components::react_trace::types::ScrollAnchor::Byte {
                        entry_idx: entry_idx - drain,
                        byte_offset,
                    }
                };
            }
            crate::components::react_trace::types::ScrollAnchor::Following => {}
        }
        self.invalidate_cache();
    } else {
        self.mark_dirty_from(self.entries.len().saturating_sub(2));
    }
    if self.is_following() {
        self.scroll_to_bottom();
    }
}
```

- [ ] **Step 5: Run the four formerly-failing SIMs — they should now PASS**

```bash
cargo test -p spur-tui --features markdown sim_sub_entry sim_mermaid_state sim_page_up sim_render_offset 2>&1 | tail -10
```

Expected: 4 PASS.

- [ ] **Step 6: Run the full spur-tui suite to catch regressions**

```bash
cargo test -p spur-tui --features markdown 2>&1 | tail -15
```

Expected: all tests pass (was 154 pre-Phase-3; expected 154 + 3 new from Task 2 + 1 from Task 3 = 158).

If `phase2_f3_anchor_survives_eviction` (line 983) FAILS because the user code paths now produce `Row` anchors but the test pattern matches only `Byte`, that's expected — the test will be updated in Task 5. Keep going.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "$(cat <<'EOF'
fix(spur-tui): route shift_anchor_by through line_cache; emit Row anchors

shift_anchor_by now reads entry_row_starts from self.line_cache
(populated by render with the live MermaidRegistry), eliminating the
P2 layout mismatch. Scroll mutators emit Row{entry_idx, row_within_entry}
anchors, fixing P1 sub-entry scroll. push() eviction adjusts both Row
and Byte variants during the Phase 3 migration.

SIM-9, SIM-10, SIM-11, SIM-13 now pass.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Drop `ScrollAnchor::Byte` variant; migrate remaining consumers

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/types.rs:94-107` (remove Byte)
- Modify: `crates/spur-tui/src/components/react_trace/render.rs:50-93` (drop Byte arm); `:687-700` (rewrite unit tests for Row)
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs:309-341` (drop Byte arm in `push`)
- Modify: `crates/spur-tui/src/components/react_trace/streaming_tests.rs:997, 1130-1148` (update SIM-9 + eviction-survives test patterns)

- [ ] **Step 1: Remove the Byte variant from the enum**

Edit `crates/spur-tui/src/components/react_trace/types.rs`:

```rust
// before
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAnchor {
    Following,
    Row {
        entry_idx: usize,
        row_within_entry: usize,
    },
    Byte {
        entry_idx: usize,
        byte_offset: usize,
    },
}

// after
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAnchor {
    Following,
    Row {
        entry_idx: usize,
        row_within_entry: usize,
    },
}
```

Also update the doc comment to drop the "Byte is the legacy variant" line.

- [ ] **Step 2: Run `cargo check` and let the compiler enumerate broken sites**

```bash
cargo check -p spur-tui --features markdown 2>&1 | grep -E "^error" | head -20
```

Expected errors at:
- `render.rs:60` (resolve_anchor Byte arm)
- `render.rs:688` (byte_anchor unit test)
- `render.rs:700` (a second byte_anchor test)
- `mod.rs:317` or near (push eviction Byte branch)
- `streaming_tests.rs:997` (phase2_f3_anchor_survives_eviction)
- `streaming_tests.rs:1132/1136` (SIM-9 match)

- [ ] **Step 3: Drop the Byte arm in `resolve_anchor`**

Edit `crates/spur-tui/src/components/react_trace/render.rs`. Remove the entire `ScrollAnchor::Byte { ... } => { ... }` match arm added in Task 2 Step 4. The function reduces to:

```rust
#[cfg(feature = "markdown")]
pub(crate) fn resolve_anchor(
    anchor: &crate::components::react_trace::types::ScrollAnchor,
    _byte_ranges: &[Option<std::ops::Range<usize>>],
    entry_row_starts: &[usize],
    total_rows: usize,
    visible_height: usize,
) -> usize {
    use crate::components::react_trace::types::ScrollAnchor;
    match anchor {
        ScrollAnchor::Following => total_rows.saturating_sub(visible_height),
        ScrollAnchor::Row {
            entry_idx,
            row_within_entry,
        } => {
            if *entry_idx >= entry_row_starts.len() {
                return 0;
            }
            let row_start = entry_row_starts[*entry_idx];
            let row_end = entry_row_starts
                .get(*entry_idx + 1)
                .copied()
                .unwrap_or(total_rows);
            let entry_height = row_end.saturating_sub(row_start);
            let clamped = (*row_within_entry).min(entry_height.saturating_sub(1));
            row_start + clamped
        }
    }
}
```

Note `byte_ranges` is now unused at the parameter level. Prefix with `_` rather than removing it: callers in `render_with_ctx` already pass it and changing the call site is out of scope for this task.

- [ ] **Step 4: Rewrite the two byte-anchor unit tests in `render.rs` to use Row**

Edit `crates/spur-tui/src/components/react_trace/render.rs:682-710`. Replace `byte_anchor_resolves_to_containing_row` and `evicted_entry_snaps_to_zero` with their Row equivalents (the new Row tests added in Task 2 already cover this surface — delete the obsolete byte tests):

```rust
// DELETE the byte_anchor_resolves_to_containing_row test (lines ~683-695).
// DELETE the evicted_entry_snaps_to_zero test (lines ~697-705).
// The Row equivalents (row_anchor_resolves_within_entry,
// row_anchor_clamps_to_entry_last, row_anchor_evicted_entry_snaps_to_zero)
// added in Task 2 already cover this surface.
```

- [ ] **Step 5: Drop the Byte arm in `push` eviction**

Edit `crates/spur-tui/src/components/react_trace/mod.rs`. Remove the `ScrollAnchor::Byte { ... } => { ... }` match arm. The simplified eviction:

```rust
match self.anchor {
    crate::components::react_trace::types::ScrollAnchor::Row {
        entry_idx,
        row_within_entry,
    } => {
        self.anchor = if entry_idx < drain {
            crate::components::react_trace::types::ScrollAnchor::Row {
                entry_idx: 0,
                row_within_entry: 0,
            }
        } else {
            crate::components::react_trace::types::ScrollAnchor::Row {
                entry_idx: entry_idx - drain,
                row_within_entry,
            }
        };
    }
    crate::components::react_trace::types::ScrollAnchor::Following => {}
}
```

- [ ] **Step 6: Update `phase2_f3_anchor_survives_eviction` to match Row variant**

Edit `crates/spur-tui/src/components/react_trace/streaming_tests.rs:982-1014`. Replace the match patterns:

```rust
/// F3 regression: anchor on entry that gets evicted snaps to (0, 0).
#[test]
fn phase2_f3_anchor_survives_eviction() {
    use crate::components::react_trace::types::ScrollAnchor;
    let mut trace = ReactTrace::new_for_tests();
    trace.append_message("entry 0 content", "claude", "10:00".into());
    trace.scroll_to_top();
    trace.scroll_down_by(1);

    // Force eviction by exceeding MAX_LOG_ENTRIES.
    for i in 1..2000 {
        trace.append_message(&format!("entry {} content", i), "claude", "10:00".into());
    }

    let anchor = trace.anchor_for_tests();
    match anchor {
        ScrollAnchor::Row {
            entry_idx,
            row_within_entry,
        } => {
            assert!(
                entry_idx < trace.entries_for_tests().len(),
                "anchor.entry_idx must point at a surviving entry"
            );
            assert!(
                row_within_entry == 0 || entry_idx > 0,
                "evicted-entry anchor must snap to (0, 0)"
            );
        }
        ScrollAnchor::Following => {
            // Acceptable: streaming pushed user back to bottom.
        }
    }
}
```

- [ ] **Step 7: Update SIM-9 match patterns to Row**

Edit `crates/spur-tui/src/components/react_trace/streaming_tests.rs:1129-1148`. Replace:

```rust
match (anchor_after_scroll_down, anchor_after_scroll_up) {
    (
        ScrollAnchor::Row {
            entry_idx: e1,
            row_within_entry: r1,
        },
        ScrollAnchor::Row {
            entry_idx: e2,
            row_within_entry: r2,
        },
    ) => {
        assert!(
            e1 != e2 || r1 != r2,
            "SIM-9: scroll_up did not change the anchor — sub-entry \
             scroll is broken. Both anchors are Row{{{}, {}}}.",
            e1,
            r1
        );
    }
    _ => {
        // Following or transition — acceptable.
    }
}
```

- [ ] **Step 8: `cargo check` should now succeed**

```bash
cargo check -p spur-tui --features markdown 2>&1 | tail -5
```

Expected: clean compile (warnings OK, no errors).

- [ ] **Step 9: Run the full test suite**

```bash
cargo test -p spur-tui --features markdown 2>&1 | tail -15
```

Expected: all tests pass; no `Byte` references remain.

- [ ] **Step 10: Final grep — confirm no Byte references remain**

```bash
grep -rn "ScrollAnchor::Byte\|byte_offset" crates/spur-tui/src/components/react_trace/ | grep -v "// " | grep -v "byte_ranges"
```

Expected: no output (or only comments/byte_ranges field references).

- [ ] **Step 11: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/
git commit -m "$(cat <<'EOF'
refactor(spur-tui): remove ScrollAnchor::Byte; complete Row migration

Drops the Byte variant and its consumers (resolve_anchor arm, push
eviction arm, two render.rs unit tests, two streaming_tests.rs match
patterns). The Row anchor is now the sole non-Following variant.
byte_ranges parameter to resolve_anchor retained as unused for caller
compatibility; can be dropped in a future cleanup once render_with_ctx
no longer threads it.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Add EDGE-3 stale-cache safety test

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/streaming_tests.rs` (append at end)

- [ ] **Step 1: Write the test**

Append to `crates/spur-tui/src/components/react_trace/streaming_tests.rs`:

```rust
/// EDGE-3 (Phase 3): stale `line_cache` between render and shift.
///
/// User holds page_down while content arrives. shift_anchor_by sees a
/// cache from BEFORE the new content; the next render rebuilds. Anchor
/// must remain valid (in-bounds) after the rebuild — no panic, no
/// out-of-range entry_idx.
#[test]
fn phase3_edge_stale_cache_safe() {
    use crate::components::markdown_stream::StateLookup;
    let mut trace = ReactTrace::new_for_tests();
    trace.append_message("initial content line 1", "claude", "10:00".into());
    trace.force_flush_all(&StateLookup::empty());
    let _ = trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    trace.set_visible_height_for_tests(5);

    // Position viewport
    trace.scroll_to_top();
    trace.scroll_down_by(0);

    // Append a LOT of content WITHOUT calling build_virtual_rows again
    // (simulates: shift fires before render catches up).
    for i in 0..50 {
        trace.append_message(
            &format!("late content {} with extra padding", i),
            "claude",
            "10:00".into(),
        );
    }
    trace.force_flush_all(&StateLookup::empty());

    // Shift uses the STALE cache from the first build.
    trace.scroll_down_by(3);

    // Now rebuild and confirm the anchor still resolves in-bounds.
    let (rows, starts, _byte_ranges) =
        trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    let resolved = crate::components::react_trace::render::resolve_anchor(
        &trace.anchor_for_tests(),
        &_byte_ranges,
        &starts,
        rows.len(),
        5,
    );
    assert!(
        resolved < rows.len(),
        "stale-cache shift produced out-of-range row {} (total {})",
        resolved,
        rows.len()
    );
}
```

- [ ] **Step 2: Run and confirm PASS**

```bash
cargo test -p spur-tui --features markdown phase3_edge_stale_cache 2>&1 | tail -5
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "$(cat <<'EOF'
test(spur-tui): EDGE-3 stale-cache safety regression guard

Asserts shift_anchor_by + content append + re-render produces an
in-bounds anchor (no panic, no out-of-range entry_idx).

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Add COUNTER-2 streaming-during-pageup monotonicity test

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/streaming_tests.rs` (append at end)

- [ ] **Step 1: Write the test**

Append:

```rust
/// COUNTER-2 (Phase 3): streaming + page_up monotonicity.
///
/// User holds page_up while a long message streams in. Each page_up
/// sees a slightly newer cache (entry growing). The render row index
/// must NEVER move backward (downward) between consecutive page_ups.
#[test]
fn phase3_counter_streaming_pageup_monotonic() {
    use crate::components::markdown_stream::StateLookup;
    let mut trace = ReactTrace::new_for_tests();

    // Seed with initial long message
    let mut payload = String::new();
    for i in 0..40 {
        payload.push_str(&format!("Paragraph {} body text.\n\n", i));
    }
    trace.append_message(&payload, "claude", "10:00".into());
    trace.force_flush_all(&StateLookup::empty());
    let _ = trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    trace.set_visible_height_for_tests(10);

    // Start at the bottom (Following), then walk up via page_up.
    trace.scroll_to_bottom();
    let mut prev_resolved: Option<usize> = None;
    for step in 0..5 {
        // Streaming: append more lines BEFORE the page_up.
        for j in 0..3 {
            trace.append_message(
                &format!("stream-{}-{}", step, j),
                "claude",
                "10:00".into(),
            );
        }
        trace.force_flush_all(&StateLookup::empty());
        let (rows, starts, byte_ranges) =
            trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
        // Re-build refreshes line_cache via the test harness — trigger explicitly:
        let _ = (rows.len(), starts.len(), byte_ranges.len());

        trace.page_up();
        let (rows2, starts2, byte_ranges2) =
            trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
        let resolved = crate::components::react_trace::render::resolve_anchor(
            &trace.anchor_for_tests(),
            &byte_ranges2,
            &starts2,
            rows2.len(),
            10,
        );
        if let Some(p) = prev_resolved {
            assert!(
                resolved <= p,
                "step {}: page_up moved DOWN (prev={} now={})",
                step,
                p,
                resolved
            );
        }
        prev_resolved = Some(resolved);
    }
}
```

- [ ] **Step 2: Run**

```bash
cargo test -p spur-tui --features markdown phase3_counter_streaming 2>&1 | tail -10
```

Expected: PASS. If it fails because `build_virtual_rows_for_tests` doesn't update `line_cache` (test harness inspection), fall back to invoking the trace's real `set_visible_height_for_tests`+`scroll_to_bottom`+`page_up` loop and check anchor monotonicity instead of resolved row monotonicity.

If the test as-written fails with "line_cache None" (because shift_anchor_by needs cache populated and build_virtual_rows_for_tests is read-only), then update the test to populate cache via a render call. Inspect `mod.rs:856-880` (`build_virtual_rows_for_tests`) to confirm — if it doesn't write to `self.line_cache`, switch to the alternative pattern below:

```rust
// Alternative: assert anchor monotonicity directly.
use crate::components::react_trace::types::ScrollAnchor;
let mut prev: Option<(usize, usize)> = None;
// ... in the loop ...
trace.page_up();
let cur = match trace.anchor_for_tests() {
    ScrollAnchor::Row { entry_idx, row_within_entry } => Some((entry_idx, row_within_entry)),
    _ => None,
};
if let (Some(p), Some(c)) = (prev, cur) {
    // entry_idx must not increase, OR row_within_entry must not increase if same entry
    assert!(
        c.0 < p.0 || (c.0 == p.0 && c.1 <= p.1),
        "page_up moved forward: prev={:?} now={:?}", p, c
    );
}
prev = cur;
```

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "$(cat <<'EOF'
test(spur-tui): COUNTER-2 streaming + page_up monotonicity guard

Asserts that holding page_up while content streams never produces a
forward (downward) anchor movement. Validates the cache-stale safety
of P2-δ shift_anchor_by under realistic streaming conditions.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Add EDGE-7 Pending→Ready transition stability test

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/streaming_tests.rs` (append)

- [ ] **Step 1: Write the test**

Append:

```rust
/// EDGE-7 (Phase 3): mermaid Pending→Ready transition keeps the anchor
/// in-bounds. Anchor pinned mid-entry must remain valid after the fence
/// expands from 1 row to N rows.
#[test]
fn phase3_edge_mermaid_pending_to_ready_stable() {
    use crate::components::markdown_stream::StateLookup;
    use crate::components::mermaid::{FenceRender, MermaidId};

    let mut trace = ReactTrace::new_for_tests();
    trace.append_message(
        "Intro paragraph.\n\n```mermaid\ngraph LR\nA --> B\n```\n\nOutro paragraph.",
        "claude",
        "10:00".into(),
    );
    trace.force_flush_all(&StateLookup::empty());

    // Pending state: empty FenceRender registry → 1 row for the fence.
    let pending: std::collections::HashMap<MermaidId, FenceRender> =
        std::collections::HashMap::new();
    let (rows_p, starts_p, ranges_p) =
        trace.build_virtual_rows_for_tests(0, 80, &pending, None);

    // Ready state: registry has Ready(6) → 6 rows for the fence.
    let mut ready = std::collections::HashMap::new();
    ready.insert(MermaidId(0), FenceRender::Ready(6));
    let (rows_r, starts_r, ranges_r) =
        trace.build_virtual_rows_for_tests(0, 80, &ready, None);

    assert!(
        rows_r.len() > rows_p.len(),
        "Ready layout must be taller than Pending; got pending={} ready={}",
        rows_p.len(),
        rows_r.len(),
    );

    // Pin anchor mid-entry under Pending.
    use crate::components::react_trace::types::ScrollAnchor;
    let anchor = ScrollAnchor::Row {
        entry_idx: 0,
        row_within_entry: 1,
    };
    let row_p = crate::components::react_trace::render::resolve_anchor(
        &anchor, &ranges_p, &starts_p, rows_p.len(), 5,
    );
    let row_r = crate::components::react_trace::render::resolve_anchor(
        &anchor, &ranges_r, &starts_r, rows_r.len(), 5,
    );
    assert!(row_p < rows_p.len(), "Pending: in-bounds row {} of {}", row_p, rows_p.len());
    assert!(row_r < rows_r.len(), "Ready: in-bounds row {} of {}", row_r, rows_r.len());
}
```

- [ ] **Step 2: Run**

```bash
cargo test -p spur-tui --features markdown phase3_edge_mermaid_pending 2>&1 | tail -5
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "$(cat <<'EOF'
test(spur-tui): EDGE-7 mermaid Pending→Ready anchor stability guard

Pins a Row anchor mid-entry, builds layout under Pending and Ready
fence states, and asserts the anchor resolves in-bounds in both.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Add COUNTER-3 entry-eviction renumber regression guard

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/streaming_tests.rs` (append)

- [ ] **Step 1: Write the test**

Append:

```rust
/// COUNTER-3 (Phase 3): Row anchor at entry N renumbers correctly when
/// entries 0..k are evicted (entry N now lives at index N-k).
#[test]
fn phase3_counter_eviction_renumbers_row_anchor() {
    use crate::components::markdown_stream::StateLookup;
    use crate::components::react_trace::types::ScrollAnchor;

    let mut trace = ReactTrace::new_for_tests();

    // Seed two distinguishable entries.
    trace.append_message("entry 0", "claude", "10:00".into());
    trace.append_message("entry 1 anchor here", "claude", "10:01".into());
    trace.force_flush_all(&StateLookup::empty());

    // Pin anchor at entry 1, row 0.
    trace.scroll_to_top();
    let _ = trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);
    trace.set_visible_height_for_tests(2);

    // Manually set anchor to entry 1, row 0 by scrolling down past entry 0's rows.
    // Each plain-text entry contributes ≥1 row; assume 2 rows per entry minimum
    // (content row + blank separator). Scroll 2 rows to land in entry 1.
    trace.scroll_down_by(2);

    let before = trace.anchor_for_tests();
    eprintln!("COUNTER-3 before: anchor={:?}", before);

    // Force eviction of entry 0 by appending until MAX_LOG_ENTRIES is exceeded.
    for i in 0..2000 {
        trace.append_message(&format!("filler {}", i), "claude", "10:00".into());
    }

    let after = trace.anchor_for_tests();
    eprintln!("COUNTER-3 after: anchor={:?}", after);

    // After eviction: anchor.entry_idx must be < entries.len() (no out-of-range).
    if let ScrollAnchor::Row { entry_idx, .. } = after {
        assert!(
            entry_idx < trace.entries_for_tests().len(),
            "evicted: entry_idx {} out of range (entries.len={})",
            entry_idx,
            trace.entries_for_tests().len()
        );
    }
}
```

- [ ] **Step 2: Run**

```bash
cargo test -p spur-tui --features markdown phase3_counter_eviction 2>&1 | tail -5
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "$(cat <<'EOF'
test(spur-tui): COUNTER-3 Row anchor eviction renumber guard

Asserts that after MAX_LOG_ENTRIES eviction, a Row anchor's entry_idx
remains in-range relative to surviving entries.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Update RCA + design spec resolution footer

**Files:**
- Modify: `docs/superpowers/specs/2026-04-18-session-detail-streaming-ghost-text-rca.md` (append section)
- Modify: `docs/superpowers/specs/2026-04-18-session-detail-scroll-anchor-phase3-design.md` (status header)

- [ ] **Step 1: Append a Phase 3 resolution section to the RCA**

Find the existing "Resolution (2026-04-18)" footer in `docs/superpowers/specs/2026-04-18-session-detail-streaming-ghost-text-rca.md` and append underneath:

```markdown

### Phase 3 Resolution (2026-04-18)

The Phase 1+2 ship introduced `ScrollAnchor::Byte`, which audit revealed
was entry-coarse (always `byte_offset=0`) and incompatible with the
empty mermaid registry passed to `shift_anchor_by`. Phase 3 replaced
`Byte` with `ScrollAnchor::Row { entry_idx, row_within_entry }` and
routed `shift_anchor_by` through `self.line_cache`. SIMs 9, 10, 11, 13
now pass; four new regression guards (EDGE-3, EDGE-7, COUNTER-2,
COUNTER-3) lock the fix in.

Plan: `docs/superpowers/plans/2026-04-18-session-detail-scroll-anchor-phase3.md`
Design: `docs/superpowers/specs/2026-04-18-session-detail-scroll-anchor-phase3-design.md`
```

- [ ] **Step 2: Update the design spec status to "shipped"**

Edit `docs/superpowers/specs/2026-04-18-session-detail-scroll-anchor-phase3-design.md`:

```markdown
# before
**Status:** approved (2026-04-18)

# after
**Status:** shipped (2026-04-18)
```

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-04-18-session-detail-streaming-ghost-text-rca.md docs/superpowers/specs/2026-04-18-session-detail-scroll-anchor-phase3-design.md
git commit -m "$(cat <<'EOF'
docs(spur): record scroll-anchor Phase 3 resolution

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Final Verification

Run the full crate test suite + clippy + fmt before declaring done:

```bash
cargo fmt --check -p spur-tui
cargo clippy -p spur-tui --features markdown -- -D warnings
cargo test -p spur-tui --features markdown
```

All three must pass clean.

---

## Self-Review (writing-plans Step 7)

**1. Spec coverage:**
- §P1-B Row anchor → Tasks 2, 4, 5 ✓
- §P2-δ shift reads line_cache → Task 4 ✓
- §Sequence diagrams (scroll-down, Pending→Ready) → Tasks 4, 8 ✓
- §Acceptance gates (4 SIMs un-`#[ignore]`) → Tasks 1, 4 ✓
- §Acceptance gates (4 new tests EDGE-3, EDGE-7, COUNTER-2, COUNTER-3) → Tasks 6, 7, 8, 9 ✓
- §File map (4 files, ~90 LOC) → matches Tasks 2-5 ✓
- §Risk register (eviction, stale cache, API churn) → Tasks 5, 6, 9 ✓

**2. Placeholder scan:** No TBD/TODO/"implement later"/"add error handling". Task 7 Step 2 includes a fallback pattern in case `build_virtual_rows_for_tests` doesn't write to `line_cache` — that's a documented contingency, not a placeholder.

**3. Type consistency:**
- `ScrollAnchor::Row { entry_idx, row_within_entry }` — same field names in T2, T3, T4, T5, T6-T9 ✓
- `row_to_anchor` (T3) called consistently in T4 ✓
- `resolve_anchor` signature unchanged — T2 extends, T5 simplifies but keeps signature ✓
- `line_cache: Option<VirtualRowCacheEntry>` consistent across T4, T6 ✓
- Test helper names (`new_for_tests`, `force_flush_all`, `build_virtual_rows_for_tests`, `set_visible_height_for_tests`, `anchor_for_tests`, `entries_for_tests`) match `mod.rs:851-885` ✓

No issues found.
