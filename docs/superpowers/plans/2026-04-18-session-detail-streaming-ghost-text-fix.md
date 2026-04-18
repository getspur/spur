# Session Detail Streaming Ghost Text Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate ghost text in the Session Detail trace by ensuring the rendered row sequence depends only on raw bytes (not on flush state) and the viewport anchors to a content position (not to a row index).

**Architecture:** Three sequenced phases. Phase 0 lowers the per-frame event drain cap. Phase 1 introduces `MarkdownStream::preview_items` so tail and items emit identical row sequences for the same bytes, replaces the `registry.len()` fence cache key with a state hash, and adds a fresh-row-count helper for scroll mutators. Phase 2 replaces `scroll_offset: usize` with `ScrollAnchor::Byte { entry_idx, byte_offset }` resolved at render time against per-row byte ranges emitted by `build_virtual_rows`.

**Tech Stack:** Rust, ratatui, pulldown-cmark via tui-markdown, tokio broadcast.

**Companion docs:**
- Spec: `docs/superpowers/specs/2026-04-18-session-detail-streaming-ghost-text-fix-design.md`
- RCA: `docs/superpowers/specs/2026-04-18-session-detail-streaming-ghost-text-rca.md`
- Diagnostic SIM-1..8 already live in `crates/spur-tui/src/components/react_trace/streaming_tests.rs`.

---

## File map

| File | Change | Phase |
|---|---|---|
| `crates/spur-tui/src/app.rs` | Constant change line 1627 | 0 |
| `crates/spur-tui/src/components/markdown_stream.rs` | New `preview_items` method + extracted pure helper | 1 |
| `crates/spur-tui/src/components/react_trace/builder.rs` | `render_agent_message_body` uses `preview_items` | 1 |
| `crates/spur-tui/src/components/react_trace/render.rs` | Replace `fence_gen` with `fence_state_hash`; resolve anchor at slice time | 1, 2 |
| `crates/spur-tui/src/components/react_trace/mod.rs` | Add `current_row_count` helper; replace `scroll_offset` with `ScrollAnchor`; update mutators; eviction | 1, 2 |
| `crates/spur-tui/src/components/react_trace/types.rs` | No change to `VirtualRow` (byte ranges via parallel vec) | 2 |
| `crates/spur-tui/src/components/react_trace/streaming_tests.rs` | Promote ignored SIMs; add new regression tests | 1, 2 |

---

## Phase 0 — Cadence (single task)

### Task 0.1: Lower drain cap from 64 to 8

**Files:**
- Modify: `crates/spur-tui/src/app.rs:1627`

- [ ] **Step 1: Read the surrounding comment context to confirm intent**

Read `crates/spur-tui/src/app.rs:1620-1650` and verify the comment block already documents the cap as a streaming-smoothness mechanism. The change is a constant adjustment that brings the implementation in line with the approved streaming-backbone design.

- [ ] **Step 2: Make the change**

In `crates/spur-tui/src/app.rs`, change line 1627 from:

```rust
        const DRAIN_CAP_PER_FRAME: u32 = 64;
```

to:

```rust
        const DRAIN_CAP_PER_FRAME: u32 = 8;
```

- [ ] **Step 3: Build to confirm no breakage**

Run: `cargo build -p spur-tui --features markdown`
Expected: success.

- [ ] **Step 4: Run existing tests**

Run: `cargo test -p spur-tui --features markdown --lib`
Expected: all existing tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "fix(spur-tui): restore DRAIN_CAP_PER_FRAME to 8 for smoother streaming"
```

---

## Phase 1 — Symmetric rendering and cache correctness

### Task 1.1: Add failing test for `preview_items` purity

**Files:**
- Modify: `crates/spur-tui/tests/markdown_stream_tests.rs`

- [ ] **Step 1: Locate existing test patterns**

Read `crates/spur-tui/tests/markdown_stream_tests.rs:1-30` to confirm the integration-test layout.

- [ ] **Step 2: Append the failing test**

Append to `crates/spur-tui/tests/markdown_stream_tests.rs`:

```rust
/// preview_items must be pure: two consecutive calls with the same input
/// return identical Vec<StreamItem> and do not mutate cursor state.
#[test]
fn preview_items_is_pure() {
    use spur_tui::components::markdown_stream::{MarkdownStream, StateLookup};
    let mut s = MarkdownStream::new();
    s.append("# Heading\n\nProse paragraph.\n\n```rust\nfn x() {}\n```\n\nmore");

    let flushed_before = s.flushed_byte_len_for_tests();
    let items_a = s.preview_items(&StateLookup::empty());
    let items_b = s.preview_items(&StateLookup::empty());
    let flushed_after = s.flushed_byte_len_for_tests();

    assert_eq!(flushed_before, flushed_after,
        "preview_items must not mutate flushed_byte_len");
    assert_eq!(items_a.len(), items_b.len(),
        "preview_items must be deterministic across calls");
}

/// preview_items output must match cached_items after flush_final for the
/// same raw_text. This is the F1 invariant: tail rendering produces what
/// final flush would produce.
#[test]
fn preview_items_matches_post_final_flush() {
    use spur_tui::components::markdown_stream::{MarkdownStream, StateLookup};
    let payload = "# H\n\nP1.\n\n- a\n- b\n\ntail";

    let mut preview_stream = MarkdownStream::new();
    preview_stream.append(payload);
    let preview = preview_stream.preview_items(&StateLookup::empty());

    let mut flushed_stream = MarkdownStream::new();
    flushed_stream.append(payload);
    flushed_stream.flush_final(&StateLookup::empty());
    let after = flushed_stream.items().to_vec();

    assert_eq!(preview.len(), after.len(),
        "preview_items must produce same StreamItem count as post-flush_final");
}
```

- [ ] **Step 3: Verify test fails to compile**

Run: `cargo test -p spur-tui --features markdown --test markdown_stream_tests preview_items_is_pure`
Expected: compile error — `preview_items` does not exist.

- [ ] **Step 4: Commit the failing test**

```bash
git add crates/spur-tui/tests/markdown_stream_tests.rs
git commit -m "test(spur-tui): failing tests for MarkdownStream::preview_items (F1)"
```

### Task 1.2: Implement `preview_items` in MarkdownStream

**Files:**
- Modify: `crates/spur-tui/src/components/markdown_stream.rs`

- [ ] **Step 1: Read the existing `rebuild` and `build_items_for` flow**

Read `crates/spur-tui/src/components/markdown_stream.rs:512-549` (rebuild) and `400-510` (build_items_for) to understand the pure vs side-effecting parts.

- [ ] **Step 2: Add `preview_items` using the clone-and-flush_final pattern**

`MarkdownStream` already derives `Clone` (line 175). The simplest pure implementation is to clone the stream, run `flush_final` on the clone, and return its `cached_items`. The clone discards the persistent side effects.

Add this method to the `impl MarkdownStream` block (place after `flush_final`, around line 318):

```rust
    /// Returns the StreamItems that `flush_final` would produce for the
    /// current `raw_text`, without mutating self. Callers use this to render
    /// the tail with the same row sequence final flush would emit, eliminating
    /// the tail-vs-items asymmetry that caused ghost text (RCA Layer 2A).
    ///
    /// Pure with respect to self. Cost is one pulldown parse pass per call;
    /// the parent VirtualRow cache amortizes across renders.
    pub fn preview_items(&self, states: &StateLookup<'_>) -> Vec<StreamItem> {
        if self.raw_text.is_empty() {
            return Vec::new();
        }
        let mut clone = self.clone();
        // flush_final allows EOF closure (permit_eof_closure=true), so the
        // entire raw_text is committed and trailing tail bytes get the same
        // paragraph context they would after TurnComplete.
        let _ = clone.flush_final(states);
        clone.cached_items
    }
```

- [ ] **Step 3: Run the F1 tests**

Run: `cargo test -p spur-tui --features markdown --test markdown_stream_tests preview_items`
Expected: both `preview_items_is_pure` and `preview_items_matches_post_final_flush` pass.

- [ ] **Step 4: Run the full markdown_stream test suite**

Run: `cargo test -p spur-tui --features markdown --test markdown_stream_tests`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/markdown_stream.rs
git commit -m "feat(spur-tui): add MarkdownStream::preview_items for symmetric rendering (F1)"
```

### Task 1.3: Switch `render_agent_message_body` to use `preview_items`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/builder.rs:367-423`

- [ ] **Step 1: Re-read the current builder**

Read `crates/spur-tui/src/components/react_trace/builder.rs:367-423`. Note the function takes `stream: &MarkdownStream` and `fence_state: &HashMap<MermaidId, FenceRender>`. It currently uses `stream.items_and_tail()`.

- [ ] **Step 2: Construct a `StateLookup` from the FenceRender map**

The builder receives `&HashMap<MermaidId, FenceRender>` but `preview_items` needs a `&StateLookup<'_>`. Read `crates/spur-tui/src/components/markdown_stream.rs:18-46` for the `StateLookup` struct.

The straightforward path: walk the FenceRender map and bucket ids by state into local `HashSet<MermaidId>` for errors and pending, then construct `StateLookup` from them. Add a helper at the top of `render_agent_message_body`:

```rust
    use std::collections::HashSet;
    let mut errors: HashSet<crate::components::mermaid::MermaidId> = HashSet::new();
    let mut pending: HashSet<crate::components::mermaid::MermaidId> = HashSet::new();
    for (id, render) in fence_state {
        match render {
            crate::components::mermaid::FenceRender::Error => { errors.insert(*id); }
            crate::components::mermaid::FenceRender::Pending => { pending.insert(*id); }
            crate::components::mermaid::FenceRender::Ready(_) => {}
        }
    }
    let state_lookup = crate::components::markdown_stream::StateLookup {
        errors: &errors,
        pending: &pending,
    };
```

- [ ] **Step 3: Replace the items+tail rendering with preview_items**

Replace the body of `render_agent_message_body` (lines 378-422). Substitute the existing tail-iteration block with iteration over `stream.preview_items(&state_lookup)`. The fence-handling logic (lines 392-409) stays — preview_items emits the same `StreamItem::Fence(id)` boundaries.

After the build:

```rust
    let items = stream.preview_items(&state_lookup);

    for item in &items {
        match item {
            StreamItem::Text(text_lines) => {
                for line in text_lines {
                    let mut spans = vec![Span::raw("   ")];
                    spans.extend(line.spans.iter().cloned());
                    let mut new_line = Line::from(spans);
                    new_line.style = line.style;
                    new_line.alignment = line.alignment;
                    emit_line(new_line);
                }
            }
            StreamItem::Fence(id) => match fence_state.get(id).copied() {
                Some(FenceRender::Ready(h)) if h > 0 => {
                    emit_fence_image(*id, h);
                }
                other => {
                    let render = match other {
                        Some(FenceRender::Error) => FenceRender::Error,
                        _ => FenceRender::Pending,
                    };
                    let placeholder = fence_placeholder_line(*id, render);
                    let mut spans = vec![Span::raw("   ")];
                    spans.extend(placeholder.spans.iter().cloned());
                    let mut line = Line::from(spans);
                    line.style = placeholder.style;
                    line.alignment = placeholder.alignment;
                    emit_line(line);
                }
            },
        }
    }
    // The trailing tail-iteration block (current lines 414-422) is removed:
    // preview_items already covers the entire raw_text under
    // permit_eof_closure=true.
```

- [ ] **Step 4: Run unit tests**

Run: `cargo test -p spur-tui --features markdown --lib`
Expected: all tests pass. The two ignored simulations SIM-1 and SIM-2 still ignored — they will be promoted in Task 1.4.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/builder.rs
git commit -m "fix(spur-tui): render AgentMessage body via preview_items (F1)"
```

### Task 1.4: Promote SIM-1 and SIM-2 from `#[ignore]`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/streaming_tests.rs`

- [ ] **Step 1: Remove the `#[ignore]` attributes**

In `crates/spur-tui/src/components/react_trace/streaming_tests.rs`, find these lines and delete them:

```rust
#[ignore = "diagnostic for ghost-text Layer 2A — currently failing, awaiting fix"]
```

(immediately above `fn sim_tail_to_items_reflow_row_delta()`)

```rust
#[ignore = "diagnostic for ghost-text Layer 3E — currently failing, awaiting fix"]
```

(immediately above `fn sim_viewport_content_shifts_under_flush_with_no_input()`)

Update the doc-comment status lines to read:
- `/// Status: REGRESSION GUARD. Verified by F1 (preview_items).`
- `/// Status: REGRESSION GUARD. Verified by F1 (preview_items) for the streaming-flush case.`

- [ ] **Step 2: Run the promoted tests**

Run: `cargo test -p spur-tui --features markdown --lib streaming_tests::sim_tail streaming_tests::sim_viewport`
Expected: both pass.

- [ ] **Step 3: Run full test suite**

Run: `cargo test -p spur-tui --features markdown`
Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "test(spur-tui): promote SIM-1 and SIM-2 to active regression guards"
```

### Task 1.5: Add failing test for `fence_state_hash`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/render.rs`

- [ ] **Step 1: Locate the cache-key block**

Read `crates/spur-tui/src/components/react_trace/render.rs:325-345`. Note `fence_gen = ctx.mermaid_registry.len() as u64`.

- [ ] **Step 2: Append the failing test to the file's test module**

Append at the end of `render.rs`:

```rust
#[cfg(all(test, feature = "markdown"))]
mod fence_state_hash_tests {
    use super::*;
    use crate::components::mermaid::{MermaidId, MermaidState};
    use std::collections::HashMap;

    #[test]
    fn empty_registry_has_stable_hash() {
        let r: HashMap<MermaidId, MermaidState> = HashMap::new();
        let a = fence_state_hash(&r);
        let b = fence_state_hash(&r);
        assert_eq!(a, b);
    }

    #[test]
    fn pending_to_error_changes_hash() {
        let mut r: HashMap<MermaidId, MermaidState> = HashMap::new();
        r.insert(MermaidId(0), MermaidState::Pending { code: "g{}".into() });
        let h1 = fence_state_hash(&r);
        r.insert(MermaidId(0), MermaidState::Error { message: "boom".into() });
        let h2 = fence_state_hash(&r);
        assert_ne!(h1, h2,
            "Pending→Error must change fence_state_hash so cache invalidates");
    }

    #[test]
    fn order_independent() {
        let mut a: HashMap<MermaidId, MermaidState> = HashMap::new();
        a.insert(MermaidId(0), MermaidState::Pending { code: "x".into() });
        a.insert(MermaidId(1), MermaidState::Rendering);
        let mut b: HashMap<MermaidId, MermaidState> = HashMap::new();
        b.insert(MermaidId(1), MermaidState::Rendering);
        b.insert(MermaidId(0), MermaidState::Pending { code: "x".into() });
        assert_eq!(fence_state_hash(&a), fence_state_hash(&b),
            "iteration order must not affect hash (must sort by id)");
    }
}
```

- [ ] **Step 3: Verify test fails to compile**

Run: `cargo test -p spur-tui --features markdown --lib render::fence_state_hash_tests`
Expected: compile error — `fence_state_hash` undefined.

- [ ] **Step 4: Commit failing test**

```bash
git add crates/spur-tui/src/components/react_trace/render.rs
git commit -m "test(spur-tui): failing tests for fence_state_hash (F2)"
```

### Task 1.6: Implement `fence_state_hash` and replace `fence_gen`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/render.rs`

- [ ] **Step 1: Add the helper function at the top of the file (after `use` block)**

Add to `crates/spur-tui/src/components/react_trace/render.rs`:

```rust
/// Hash of the mermaid registry's per-fence state. Replaces the
/// `registry.len()` cache key, which missed Pending/Rendering/Ready/Error
/// transitions when registry size stayed constant.
///
/// Sorts by MermaidId so iteration order doesn't affect the hash.
/// For Ready state, includes image dimensions because they affect
/// ImageRow height.
#[cfg(feature = "markdown")]
pub(crate) fn fence_state_hash(
    registry: &std::collections::HashMap<
        crate::components::mermaid::MermaidId,
        crate::components::mermaid::MermaidState,
    >,
) -> u64 {
    use std::hash::{Hash, Hasher};
    use crate::components::mermaid::MermaidState;

    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut entries: Vec<_> = registry.iter().collect();
    entries.sort_by_key(|(id, _)| id.0);
    for (id, state) in entries {
        id.0.hash(&mut h);
        std::mem::discriminant(state).hash(&mut h);
        if let MermaidState::Ready { image, .. } = state {
            image.width().hash(&mut h);
            image.height().hash(&mut h);
        }
    }
    h.finish()
}
```

- [ ] **Step 2: Replace `fence_gen` computation**

In `render_with_ctx`, change line 329 from:

```rust
        let fence_gen = ctx.mermaid_registry.len() as u64;
```

to:

```rust
        let fence_gen = fence_state_hash(ctx.mermaid_registry);
```

Apply the same change in `render` (the non-ctx variant) if it has an analogous line — search for `mermaid_registry.len()` in `render.rs` and replace each.

- [ ] **Step 3: Run the F2 tests**

Run: `cargo test -p spur-tui --features markdown --lib fence_state_hash_tests`
Expected: all three tests pass.

- [ ] **Step 4: Run the full lib test suite**

Run: `cargo test -p spur-tui --features markdown --lib`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/render.rs
git commit -m "fix(spur-tui): use fence_state_hash so Pending→Ready invalidates cache (F2)"
```

### Task 1.7: Add `current_row_count` helper and use it in scroll mutators (RC2)

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs`

- [ ] **Step 1: Add a failing test that proves the current bug**

Append to the `streaming_tests.rs` test module:

```rust
/// RC2 — scroll mutators must use fresh row metrics, not stale
/// last_total_lines from a previous render.
#[test]
fn scroll_uses_fresh_row_count_after_append() {
    let mut trace = ReactTrace::new_for_tests();
    trace.append_message("line one\nline two\nline three", "claude", "10:00".into());
    // First render establishes last_total_lines.
    let _ = trace.build_virtual_rows_for_tests(0, 80, &std::collections::HashMap::new(), None);

    // Append more content WITHOUT rendering.
    trace.append_message(
        "\nline four\nline five\nline six\nline seven", "claude", "10:00".into());

    // scroll_up should NOT clamp against the stale (small) last_total_lines.
    // Fresh row count should be larger; scroll_up sets is_following=false and
    // moves offset down by 1 from max.
    trace.scroll_to_bottom();
    let bottom_then_up_should_have_room = trace.scroll_offset_for_tests();
    trace.scroll_up();
    let after_scroll_up = trace.scroll_offset_for_tests();
    assert!(after_scroll_up < bottom_then_up_should_have_room,
        "scroll_up should reveal a new offset when fresh row count is larger; \
         got {} after, {} before", after_scroll_up, bottom_then_up_should_have_room);
}
```

If `scroll_offset_for_tests` doesn't exist yet, add it to the test-only impl in `mod.rs`:

```rust
#[cfg(test)]
impl ReactTrace {
    pub fn scroll_offset_for_tests(&self) -> usize {
        self.scroll_offset
    }
}
```

(Place adjacent to other `_for_tests` methods around line 800.)

- [ ] **Step 2: Verify test fails**

Run: `cargo test -p spur-tui --features markdown --lib scroll_uses_fresh_row_count_after_append`
Expected: FAIL — `bottom_then_up_should_have_room` equals `after_scroll_up` because scroll_to_bottom uses stale max_offset.

- [ ] **Step 3: Add `current_row_count` helper to ReactTrace**

In `crates/spur-tui/src/components/react_trace/mod.rs`, add this method to the `impl ReactTrace` block where scroll mutators live (around line 305):

```rust
    /// Compute the current total row count using the most-recent width
    /// hint, walking entries directly. Used by scroll mutators to clamp
    /// against fresh metrics rather than the last-render `last_total_lines`.
    ///
    /// O(entries × wrap-cost). Acceptable because scroll mutators run on
    /// user input, not on the streaming hot path.
    #[cfg(feature = "markdown")]
    fn current_row_count(&self) -> usize {
        let width_hint = self.last_render_width.unwrap_or(80);
        let states = std::collections::HashMap::new();
        let (rows, _) = self.build_virtual_rows(0, width_hint, &states, None);
        rows.len()
    }

    #[cfg(not(feature = "markdown"))]
    fn current_row_count(&self) -> usize {
        self.last_total_lines
    }
```

- [ ] **Step 4: Add `last_render_width` field**

The helper above needs `self.last_render_width: Option<u16>`. Add it to the `ReactTrace` struct (near `last_total_lines` around line 33-35):

```rust
    pub(super) last_render_width: Option<u16>,
```

Initialize in `new()` (around line 60-65):

```rust
            last_render_width: None,
```

In `render.rs`, set it where `last_total_lines` is set (lines 264-265 and 406-407):

```rust
        self.last_render_width = Some(effective_width);
```

- [ ] **Step 5: Update `max_offset` to use fresh metrics**

Replace `max_offset` in `mod.rs:305-308`:

```rust
    fn max_offset(&self) -> usize {
        self.current_row_count().saturating_sub(self.last_visible_height)
    }
```

- [ ] **Step 6: Run the RC2 test**

Run: `cargo test -p spur-tui --features markdown --lib scroll_uses_fresh_row_count_after_append`
Expected: PASS.

- [ ] **Step 7: Run full lib tests**

Run: `cargo test -p spur-tui --features markdown --lib`
Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/
git commit -m "fix(spur-tui): scroll mutators use fresh row count via current_row_count (RC2)"
```

### Task 1.8: Phase 1 verification

- [ ] **Step 1: Run the full crate test suite**

Run: `cargo test -p spur-tui --features markdown`
Expected: all pass.

- [ ] **Step 2: Confirm SIM-1, SIM-2, RC2 are no longer failing**

Run: `cargo test -p spur-tui --features markdown --lib streaming_tests`
Expected: SIM-1 and SIM-2 pass, RC2 test passes, ignored count down by 2.

- [ ] **Step 3: Build all targets**

Run: `cargo build -p spur-tui --all-features`
Expected: success.

- [ ] **Step 4: Tag the Phase 1 commit (optional)**

If using tags:

```bash
git tag phase1-ghost-text-fix
```

---

## Phase 2 — Byte-offset content anchor

### Task 2.1: Add `ScrollAnchor` enum

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/types.rs`

- [ ] **Step 1: Add the enum definition**

Append to `crates/spur-tui/src/components/react_trace/types.rs`:

```rust
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
    Byte { entry_idx: usize, byte_offset: usize },
}

impl Default for ScrollAnchor {
    fn default() -> Self {
        ScrollAnchor::Following
    }
}
```

- [ ] **Step 2: Build to confirm**

Run: `cargo build -p spur-tui --features markdown`
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/types.rs
git commit -m "feat(spur-tui): add ScrollAnchor enum (F3 scaffolding)"
```

### Task 2.2: Make `build_virtual_rows` return per-row byte ranges

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/builder.rs`
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs` (test helper signature)
- Modify: `crates/spur-tui/src/components/react_trace/render.rs` (callers)

- [ ] **Step 1: Update return type**

Change the signature of `build_virtual_rows` in `builder.rs` (search for `pub(crate) fn build_virtual_rows`) from:

```rust
pub(crate) fn build_virtual_rows(...) -> (Vec<VirtualRow>, Vec<usize>)
```

to:

```rust
pub(crate) fn build_virtual_rows(...) -> (
    Vec<VirtualRow>,
    Vec<usize>,
    Vec<Option<std::ops::Range<usize>>>,
)
```

The third element is co-indexed with the rows vec; `None` means the row is non-textual (ImageRow without a meaningful single byte) or synthetic (timestamp header line, blank separator).

- [ ] **Step 2: Populate byte ranges as rows are built**

Inside `build_virtual_rows`, alongside the existing `rows` vec, build a parallel `byte_ranges: Vec<Option<Range<usize>>>`. For each `push_wrapped` invocation that emits one or more `VirtualRow::Text`, push `None` for each (synthetic header rows have no byte mapping). For `StreamItem::Text` rows emitted by `render_agent_message_body`, the call site has access to the line's byte range via the items it iterates — but the current code doesn't track that. As an intermediate v1, push `None` for synthetic rows (timestamp, separators) and push a coarse "entry-wide range" for content rows: range = `(entry_byte_start, entry_byte_end)` where these come from the enclosing entry's raw_text length tracker.

This gives the resolver enough granularity to find the right entry but only row-precision within the entry. SIM-3 still passes because entry-level granularity is sufficient when reflow happens within an entry.

For an exact-byte-precision implementation (deferred to a later refinement), see "Future work" at the end of this plan.

Concretely, accumulate a per-entry byte cursor as you walk entries; for each row emitted, push `Some(entry_start_bytes..entry_end_bytes)`. Entry text length:

```rust
let entry_byte_len = match &entry.kind {
    TraceKind::AgentMessage { .. } => entry
        .markdown
        .as_ref()
        .map(|s| s.raw_text().len())
        .unwrap_or(entry.text.len()),
    _ => entry.text.len(),
};
```

- [ ] **Step 3: Update test helper signature**

In `mod.rs`, find `build_virtual_rows_for_tests` (around line 787) and update its return type to match.

- [ ] **Step 4: Update render.rs callers**

In `render.rs`, every call site of `build_virtual_rows(...)` must destructure into three values:

```rust
let (rows, entry_row_starts, byte_ranges) = self.build_virtual_rows(...);
```

Store `byte_ranges` in `VirtualRowCacheEntry` alongside `rows`. Add the field:

```rust
struct VirtualRowCacheEntry {
    rows: Vec<VirtualRow>,
    entry_row_starts: Vec<usize>,
    byte_ranges: Vec<Option<Range<usize>>>,
    width: u16,
    generation: u64,
    fence_gen: u64,
}
```

Update incremental rebuild to preserve byte_ranges symmetry: when truncating `rows` from `entry_row_starts[dirty_idx]`, also truncate `byte_ranges` to the same length.

- [ ] **Step 5: Update existing tests**

Tests calling `build_virtual_rows_for_tests` must destructure the third value (often discarded with `_`):

```rust
let (rows, _, _) = trace.build_virtual_rows_for_tests(0, 80, &states, None);
```

Apply across `streaming_tests.rs` (every existing call). Use a single `sed`-style edit per call site or rely on the compiler to flag each one.

- [ ] **Step 6: Build**

Run: `cargo build -p spur-tui --features markdown --tests`
Expected: success after all callers fixed.

- [ ] **Step 7: Run tests**

Run: `cargo test -p spur-tui --features markdown --lib`
Expected: all pass (byte_ranges is added but not yet used by any logic; tests should be unaffected).

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/
git commit -m "feat(spur-tui): emit per-row byte ranges from build_virtual_rows (F3 scaffolding)"
```

### Task 2.3: Add `resolve_anchor` function with tests

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/render.rs`

- [ ] **Step 1: Add failing tests**

Append to `render.rs`:

```rust
#[cfg(all(test, feature = "markdown"))]
mod resolve_anchor_tests {
    use super::*;
    use crate::components::react_trace::types::ScrollAnchor;
    use std::ops::Range;

    fn ranges(slices: &[Option<Range<usize>>]) -> Vec<Option<Range<usize>>> {
        slices.to_vec()
    }

    #[test]
    fn following_resolves_to_max_offset() {
        let ranges = ranges(&[Some(0..10), Some(0..10), Some(0..10)]);
        let entry_starts = vec![0, 1, 2];
        let row = resolve_anchor(
            &ScrollAnchor::Following, &ranges, &entry_starts, 3, 1);
        assert_eq!(row, 2, "Following clamps to total - visible_height");
    }

    #[test]
    fn byte_anchor_resolves_to_containing_row() {
        // Two entries: entry 0 spans rows 0..2 with bytes 0..50; entry 1
        // spans rows 2..4 with bytes 0..30.
        let ranges = ranges(&[
            Some(0..50), Some(0..50),
            Some(0..30), Some(0..30),
        ]);
        let entry_starts = vec![0, 2];
        let anchor = ScrollAnchor::Byte { entry_idx: 1, byte_offset: 15 };
        let row = resolve_anchor(&anchor, &ranges, &entry_starts, 4, 2);
        assert_eq!(row, 2, "byte 15 in entry 1 lands on its first row");
    }

    #[test]
    fn evicted_entry_snaps_to_zero() {
        let ranges = ranges(&[Some(0..10)]);
        let entry_starts = vec![0];
        let anchor = ScrollAnchor::Byte { entry_idx: 99, byte_offset: 0 };
        let row = resolve_anchor(&anchor, &ranges, &entry_starts, 1, 1);
        assert_eq!(row, 0, "anchor pointing at evicted entry snaps to 0");
    }
}
```

- [ ] **Step 2: Verify test fails**

Run: `cargo test -p spur-tui --features markdown --lib resolve_anchor_tests`
Expected: compile error — `resolve_anchor` undefined.

- [ ] **Step 3: Implement `resolve_anchor`**

Add to `render.rs`:

```rust
/// Resolve a ScrollAnchor to an effective row index.
///
/// `Following` clamps to `total_rows - visible_height`.
/// `Byte` walks `entry_row_starts` to the entry, then finds the first
/// row whose `byte_ranges[i]` contains `byte_offset`. If the anchor's
/// entry was evicted (entry_idx >= entry_row_starts.len()), snaps to 0.
/// If the byte position falls in a gap (consolidated by reflow), snaps
/// to the nearest preceding row whose range covers a byte ≤ byte_offset.
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
        ScrollAnchor::Byte { entry_idx, byte_offset } => {
            if *entry_idx >= entry_row_starts.len() {
                return 0;
            }
            let row_start = entry_row_starts[*entry_idx];
            let row_end = entry_row_starts
                .get(*entry_idx + 1)
                .copied()
                .unwrap_or(total_rows);

            // First row whose range contains byte_offset.
            for i in row_start..row_end.min(byte_ranges.len()) {
                if let Some(r) = &byte_ranges[i] {
                    if r.contains(byte_offset) {
                        return i;
                    }
                }
            }
            // Snap to last preceding row with range start <= byte_offset.
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

- [ ] **Step 4: Run resolver tests**

Run: `cargo test -p spur-tui --features markdown --lib resolve_anchor_tests`
Expected: all three pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/render.rs
git commit -m "feat(spur-tui): add resolve_anchor for byte-offset viewport anchoring (F3)"
```

### Task 2.4: Replace `scroll_offset` field with `anchor`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs`

- [ ] **Step 1: Replace the field**

In `crates/spur-tui/src/components/react_trace/mod.rs`, find:

```rust
    pub(super) scroll_offset: usize,
```

(around line 29). Replace with:

```rust
    pub(super) anchor: crate::components::react_trace::types::ScrollAnchor,
```

In `new()` (around line 60), replace:

```rust
            scroll_offset: 0,
```

with:

```rust
            anchor: crate::components::react_trace::types::ScrollAnchor::default(),
```

- [ ] **Step 2: Replace `is_following` field with derived accessor**

Find the `is_following: bool` field and remove it. Add a method:

```rust
    pub fn is_following(&self) -> bool {
        matches!(self.anchor, crate::components::react_trace::types::ScrollAnchor::Following)
    }
```

Replace all `self.is_following` reads with `self.is_following()`. Replace all `self.is_following = true/false` writes with anchor mutation (covered in Task 2.5).

- [ ] **Step 3: Build**

Run: `cargo build -p spur-tui --features markdown`
Expected: many compile errors — fix in next tasks. This is intentional progress.

- [ ] **Step 4: Commit the partial state with a WIP marker**

```bash
git add -p crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "wip(spur-tui): introduce anchor field; mutators not yet updated"
```

### Task 2.5: Update scroll mutators to operate on anchor

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs:310-368`

- [ ] **Step 1: Rewrite each scroll mutator**

Replace the scroll mutator block (mod.rs:310-368) with:

```rust
    /// Move viewport up by one row by re-anchoring to the byte position
    /// of the previous row.
    pub fn scroll_up(&mut self) {
        self.shift_anchor_by(-1);
    }

    pub fn scroll_up_by(&mut self, lines: usize) {
        self.shift_anchor_by(-(lines as isize));
    }

    pub fn scroll_down(&mut self) {
        self.shift_anchor_by(1);
    }

    pub fn scroll_down_by(&mut self, lines: usize) {
        self.shift_anchor_by(lines as isize);
    }

    pub fn page_up(&mut self) {
        let jump = self.last_visible_height.saturating_sub(2).max(1) as isize;
        self.shift_anchor_by(-jump);
    }

    pub fn page_down(&mut self) {
        let jump = self.last_visible_height.saturating_sub(2).max(1) as isize;
        self.shift_anchor_by(jump);
    }

    pub fn scroll_to_top(&mut self) {
        self.anchor = crate::components::react_trace::types::ScrollAnchor::Byte {
            entry_idx: 0,
            byte_offset: 0,
        };
    }

    pub fn scroll_to_bottom(&mut self) {
        self.anchor = crate::components::react_trace::types::ScrollAnchor::Following;
    }

    /// Apply a row delta to the current anchor by:
    /// 1. resolving the current anchor to a row index using fresh metrics,
    /// 2. computing the target row,
    /// 3. converting back to a byte anchor at the target row.
    /// If the target row is the last visible row, transitions to Following.
    #[cfg(feature = "markdown")]
    fn shift_anchor_by(&mut self, delta: isize) {
        use crate::components::react_trace::types::ScrollAnchor;

        let width = self.last_render_width.unwrap_or(80);
        let states = std::collections::HashMap::new();
        let (_rows, entry_row_starts, byte_ranges) =
            self.build_virtual_rows(0, width, &states, None);
        let total = byte_ranges.len();
        let visible_h = self.last_visible_height.max(1);

        let current_row = crate::components::react_trace::render::resolve_anchor(
            &self.anchor, &byte_ranges, &entry_row_starts, total, visible_h);

        let target = (current_row as isize + delta)
            .max(0)
            .min(total.saturating_sub(visible_h) as isize) as usize;

        if target >= total.saturating_sub(visible_h) {
            self.anchor = ScrollAnchor::Following;
            return;
        }

        // Convert target row back to a byte anchor.
        let (entry_idx, byte_offset) = row_to_byte_anchor(
            target, &byte_ranges, &entry_row_starts);
        self.anchor = ScrollAnchor::Byte { entry_idx, byte_offset };
    }

    #[cfg(not(feature = "markdown"))]
    fn shift_anchor_by(&mut self, _delta: isize) {
        // Non-markdown build keeps scroll_offset semantics; no-op for now.
    }
```

Add the helper at module scope (above `impl ReactTrace`):

```rust
#[cfg(feature = "markdown")]
fn row_to_byte_anchor(
    row: usize,
    byte_ranges: &[Option<std::ops::Range<usize>>],
    entry_row_starts: &[usize],
) -> (usize, usize) {
    // Find the entry containing this row.
    let entry_idx = match entry_row_starts.binary_search(&row) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };
    let byte_offset = byte_ranges
        .get(row)
        .and_then(|r| r.as_ref())
        .map(|r| r.start)
        .unwrap_or(0);
    (entry_idx, byte_offset)
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p spur-tui --features markdown`
Expected: errors should now be limited to render.rs (Task 2.6).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "fix(spur-tui): scroll mutators operate on ScrollAnchor (F3)"
```

### Task 2.6: Update render path to resolve anchor at slice time

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/render.rs:266-280, 405-417`

- [ ] **Step 1: Replace `scroll_offset` clamp logic**

In `render` (around line 266) and `render_with_ctx` (around line 405), replace:

```rust
        let max_offset = total_lines.saturating_sub(visible_height);
        let offset = if self.is_following {
            max_offset
        } else {
            self.scroll_offset.min(max_offset)
        };
```

with:

```rust
        let offset = resolve_anchor(
            &self.anchor,
            &byte_ranges,
            &entry_row_starts,
            total_lines,
            visible_height,
        );
```

`byte_ranges` and `entry_row_starts` come from the cache (or from the just-built rows on cache miss).

- [ ] **Step 2: Build**

Run: `cargo build -p spur-tui --features markdown`
Expected: success.

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-tui --features markdown --lib`
Expected: many existing tests pass; some scroll-literal tests fail (fixed in Task 2.8).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/render.rs
git commit -m "fix(spur-tui): render resolves anchor to row at slice time (F3)"
```

### Task 2.7: Update `push()` eviction to adjust anchor

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs:289-303`

- [ ] **Step 1: Replace the eviction adjustment**

In `push()` (around line 289), replace:

```rust
        if self.entries.len() > MAX_LOG_ENTRIES {
            let drain = self.entries.len() - MAX_LOG_ENTRIES;
            self.entries.drain(..drain);
            self.scroll_offset = self.scroll_offset.saturating_sub(drain);
            self.invalidate_cache();
        } else {
            self.mark_dirty_from(self.entries.len().saturating_sub(2));
        }
```

with:

```rust
        if self.entries.len() > MAX_LOG_ENTRIES {
            let drain = self.entries.len() - MAX_LOG_ENTRIES;
            self.entries.drain(..drain);
            // Adjust anchor's entry_idx; if anchor pointed at evicted entry,
            // snap to (0, 0).
            if let crate::components::react_trace::types::ScrollAnchor::Byte {
                entry_idx,
                byte_offset,
            } = self.anchor
            {
                if entry_idx < drain {
                    self.anchor = crate::components::react_trace::types::ScrollAnchor::Byte {
                        entry_idx: 0,
                        byte_offset: 0,
                    };
                } else {
                    self.anchor = crate::components::react_trace::types::ScrollAnchor::Byte {
                        entry_idx: entry_idx - drain,
                        byte_offset,
                    };
                }
            }
            self.invalidate_cache();
        } else {
            self.mark_dirty_from(self.entries.len().saturating_sub(2));
        }
```

- [ ] **Step 2: Build and test**

Run: `cargo test -p spur-tui --features markdown --lib`
Expected: anchor-eviction handling correct; existing eviction tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "fix(spur-tui): adjust ScrollAnchor on entry eviction (F3)"
```

### Task 2.8: Migrate existing tests using literal `scroll_offset`

**Files:**
- Modify: any test using `scroll_offset:` literal or `trace.scroll_offset` direct access.

- [ ] **Step 1: Find all references**

Run: `grep -rn 'scroll_offset' crates/spur-tui/src crates/spur-tui/tests` (use the Grep tool, not Bash).

- [ ] **Step 2: Update each test**

For tests reading `scroll_offset` directly: replace with `scroll_offset_for_tests()` (already exists from Task 1.7) OR with `is_following()`/anchor inspection depending on what the test asserts.

For tests setting `trace.scroll_offset = N`: replace with the appropriate `scroll_to_*` or `scroll_*_by` mutator that establishes the same effective position.

Where the test's intent was "scroll to a specific row", the new equivalent is `scroll_to_top` then `scroll_down_by(N)` after rendering once to populate `last_visible_height`.

- [ ] **Step 3: Build and test**

Run: `cargo test -p spur-tui --features markdown`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "test(spur-tui): migrate scroll_offset literals to ScrollAnchor mutators"
```

### Task 2.9: Promote SIM-3 and add new F3 regression tests

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/streaming_tests.rs`

- [ ] **Step 1: Promote SIM-3**

Remove the `#[ignore = "..."]` attribute above `sim_fix_content_anchor_eliminates_ghost_text`. Update the doc-comment status to "REGRESSION GUARD. Verified by F1 + F3."

- [ ] **Step 2: Add new F3-specific regression tests**

Append to `streaming_tests.rs`:

```rust
/// F3 regression: anchor on byte X in entry N survives a width resize.
#[test]
fn phase2_f3_anchor_byte_offset_survives_width_resize() {
    let mut trace = ReactTrace::new_for_tests();
    trace.append_message(
        "A long paragraph with the recognizable phrase MARKER inside that wraps differently at different widths.",
        "claude", "10:00".into());
    trace.force_flush_all(&StateLookup::empty());

    // Anchor a viewport at width 80.
    let (rows_w80, _, _) = trace.build_virtual_rows_for_tests(
        0, 80, &std::collections::HashMap::new(), None);
    let _ = rows_w80;
    trace.scroll_to_top();
    trace.scroll_down_by(1);

    // Snapshot anchor.
    let anchor_before = trace.anchor_for_tests();

    // Re-render at width 60 — wrapping changes substantially.
    let (_rows_w60, _, _) = trace.build_virtual_rows_for_tests(
        0, 60, &std::collections::HashMap::new(), None);
    let anchor_after = trace.anchor_for_tests();

    assert_eq!(anchor_before, anchor_after,
        "F3: ScrollAnchor must be invariant under width change");
}

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
        trace.append_message(
            &format!("entry {} content", i), "claude", "10:00".into());
    }

    let anchor = trace.anchor_for_tests();
    match anchor {
        ScrollAnchor::Byte { entry_idx, byte_offset } => {
            assert!(entry_idx < trace.entries_for_tests().len(),
                "anchor.entry_idx must point at a surviving entry");
            assert!(byte_offset == 0 || entry_idx > 0,
                "evicted-entry anchor must snap to (0, 0)");
        }
        ScrollAnchor::Following => {
            // Acceptable: streaming pushed user back to bottom.
        }
    }
}
```

Add the `anchor_for_tests` accessor to the `#[cfg(test)] impl ReactTrace` block in `mod.rs`:

```rust
    pub fn anchor_for_tests(&self) -> crate::components::react_trace::types::ScrollAnchor {
        self.anchor
    }
```

- [ ] **Step 3: Run the new tests**

Run: `cargo test -p spur-tui --features markdown --lib phase2_f3`
Expected: both pass.

- [ ] **Step 4: Run full suite**

Run: `cargo test -p spur-tui --features markdown`
Expected: all pass; SIM-3 no longer ignored.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/
git commit -m "test(spur-tui): promote SIM-3 and add F3 regression guards"
```

### Task 2.10: Phase 2 verification and RCA cross-reference update

- [ ] **Step 1: Run full crate suite at all feature combinations**

```bash
cargo test -p spur-tui --features markdown
cargo test -p spur-tui --no-default-features
cargo test -p spur-tui
```

Expected: all pass.

- [ ] **Step 2: Build all targets**

Run: `cargo build -p spur-tui --all-features`
Expected: success.

- [ ] **Step 3: Update the RCA companion to record fix landing**

Append to `docs/superpowers/specs/2026-04-18-session-detail-streaming-ghost-text-rca.md`:

```markdown
## Resolution (2026-04-18)

The corrective actions in this RCA were superseded by the fix design at
`docs/superpowers/specs/2026-04-18-session-detail-streaming-ghost-text-fix-design.md`.
Phase 0 (RC1), Phase 1 (F1+F2+RC2), and Phase 2 (F3) are implemented.
SIM-1, SIM-2, SIM-3 are active regression guards.
```

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-04-18-session-detail-streaming-ghost-text-rca.md
git commit -m "docs(spur): record ghost-text fix resolution in RCA"
```

---

## Future work (out of scope for this plan)

**Per-line byte ranges (vs entry-level).** Task 2.2 emits coarse entry-wide byte ranges per row. This is sufficient for the SIM-1..3 failure modes (intra-entry reflow). For sub-entry-precision anchoring (e.g., scrolling within a long single AgentMessage), `build_virtual_rows` would need to track which subrange of the entry's bytes each visual row maps to. This requires plumbing pulldown's per-event byte ranges through `StreamItem::Text` and the wrap-line step. Defer until a real failure mode demands it.

**`preview_items` memoization.** The clone-and-flush_final implementation parses on every render. If profiling shows this is hot, add a `RefCell<Option<(usize, u64, Vec<StreamItem>)>>` cache on `MarkdownStream` keyed on `(raw_text.len(), fence_state_hash)`.

**Removing `last_total_lines` and `last_visible_height` fields.** With F3 in place, these are only needed to size scroll deltas (`page_up`/`page_down`'s jump distance) and as the width hint fallback. They can be reduced to a single `last_render_geometry: Option<(u16, usize)>` for clarity.
