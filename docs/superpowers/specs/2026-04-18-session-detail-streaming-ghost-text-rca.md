# Session Detail Streaming Ghost Text — RCA

**Date:** 2026-04-18
**Status:** Findings only
**Scope:** `crates/spur-tui/src/views/session_detail.rs` and `crates/spur-tui/src/components/react_trace/`
**Method:** Follow-up code review of event ingestion, markdown stream buffering, virtual-row caching, scroll state, and frame paint behavior under sustained `AgentMessageChunk` load.

## Problem

When a session receives a high volume of streamed text, the Session Detail
trace can still look wrong in two ways:

- the visible body appears to "freeze" and then jump forward in batches
- when the user scrolls during streaming, the viewport can feel unstable or
  appear to show an older/newer slice than expected

The review focused on the `SessionDetailView` render path, the `ReactTrace`
scroll/render path, and the TUI main loop's per-frame event drain behavior.

## Executive Summary

The original markdown stale-tail explanation is no longer the best RCA for the
current codebase.

That bug was already fixed by the cursor-split renderer:

- `MarkdownStream` now exposes `items_and_tail()`
- `ReactTrace` now renders both committed items and the uncommitted tail
- the regression test `ghost_text_rc1_regression` passes

The strongest remaining explanation is a combination of:

1. **Burst coalescing in the TUI main loop** because `crates/spur-tui/src/app.rs`
   still drains up to **64** Spur events before a paint, even though the
   approved streaming design called for **8**
2. **Scroll math derived from stale last-render geometry** because
   `ReactTrace::scroll_up`, `scroll_down`, `page_up`, and `page_down` compute
   against `last_total_lines` and `last_visible_height`, which are only updated
   during render

These two together explain the symptom better than a paint-buffer bug:

- fast streams visually advance in large chunks rather than smoothly
- scroll commands issued while new rows are arriving can apply against the
  previous frame's document height, then be reinterpreted against the newer
  post-render row count

## Root Cause

### RC1. Per-frame Spur event drain cap is still too large

The TUI main loop in `crates/spur-tui/src/app.rs` still uses:

```rust
const DRAIN_CAP_PER_FRAME: u32 = 64;
```

This means the app may absorb dozens of streamed `AgentMessageChunk` events
before painting a frame. Under steady chunk traffic, the user does not see a
true "typewriter" cadence; they see larger visual jumps.

This is the most direct explanation for the "freeze, then jump" symptom during
fast streaming.

### RC2. Scroll operations use stale geometry from the previous render

`ReactTrace` scroll methods rely on:

- `last_total_lines`
- `last_visible_height`

Those fields are updated inside `render()` / `render_with_ctx()`, not when new
entries are appended.

Current behavior:

1. new streamed text arrives and increases the future virtual-row count
2. before the next paint, the user scrolls
3. scroll math runs against the **old** `last_total_lines`
4. render then rebuilds rows and clamps the viewport against the **new** total

This can make the viewport feel unstable while scrolling during streaming:

- jumps larger or smaller than intended
- follow-mode re-entry earlier than expected
- the on-screen window appearing to "slip" relative to the user's scroll input

For the remaining bug reports, this is the strongest explanation for
"ghost text while scrolling" in the current implementation.

## Previously Suspected Cause That Is Now Ruled Out

### N1. The old markdown stale-tail bug is fixed

The prior RCA centered on dirty markdown streams rendering stale cached items
until the next flush. That was accurate for an older revision, but not for the
current code:

- `MarkdownStream::items_and_tail()` exposes both the committed prefix and the
  raw uncommitted tail
- `render_agent_message_body()` renders both
- the regression test `ghost_text_rc1_regression` in
  `crates/spur-tui/src/components/react_trace/streaming_tests.rs` passes

So the current issue should not be treated as a reappearance of that exact bug
unless a new failing test demonstrates it.

## Contributing Factors

### C1. `SessionDetailView` itself is mostly a thin delegator

`crates/spur-tui/src/views/session_detail.rs` does not appear to be the primary
source of the problem.

Its role is mainly:

- ingest `AgentMessageChunk` / `AgentThoughtChunk`
- forward text into `self.react_trace`
- forward scroll commands into `ReactTrace`
- choose between `render()` and `render_with_ctx()`

So the user-visible issue is better explained by `app.rs` frame cadence and
`react_trace` scroll state than by `session_detail.rs` layout itself.

### C2. Mermaid cache invalidation is also too weak

`render_with_ctx()` uses `ctx.mermaid_registry.len() as u64` as the fence-state
cache key.

That catches insertion/removal, but not state transitions like:

- `Pending -> Ready`
- `Rendering -> Error`

when the registry length stays constant.

This is a real stale-render bug, but it is more relevant to diagram
placeholder/image transitions than to plain text streaming. It should be fixed,
but it is probably not the main driver of the user's current text symptom.

## Ruled-Out Hypotheses

### N2. Not primarily a terminal-cell clearing failure

The segmented `ReactTrace` render path initially looks suspicious because it
renders only visible sub-rects inside the trace body.

However, ratatui resets the inactive back buffer before it becomes the current
frame buffer on the next draw. That means the renderer starts from a clean
buffer each frame rather than reusing old terminal cells.

This demotes "stale pixels surviving a frame" from primary RCA to unlikely.

### N3. Not primarily provider-side cumulative chunk semantics

The reviewed streaming paths still look like ordinary incremental chunk
delivery. No evidence in this review suggests the symptom depends on providers
sending cumulative full-message prefixes.

## Detailed Evidence

### Event ingestion path is straightforward

`SessionDetailView::handle_spur_event` receives `AgentMessageChunk` and forwards
text directly to:

- `self.react_trace.append_message(...)`

This path does not itself duplicate content or intentionally delay rendering.

### Current markdown stream behavior already renders the newest tail

Relevant current behavior:

- `MarkdownStream::items_and_tail()` returns both committed items and the raw
  tail
- `render_agent_message_body()` emits both
- the local regression test for the original stale-tail bug passes

This was verified with:

```bash
cargo test -p spur-tui ghost_text_rc1_regression -- --nocapture
```

### Main-loop drain behavior still batches too aggressively

The app loop drains remaining Spur events non-blockingly before rendering, but
the cap is still `64`.

That is enough to turn many small streamed updates into one larger visible
delta, especially when the upstream source is chunking aggressively.

### Scroll state is lagging one render behind

`ReactTrace::max_offset()` derives from `last_total_lines` and
`last_visible_height`, both of which are assigned only in the render path.

So scroll behavior during active streaming is based on stale geometry by
construction.

## Why The Symptom Still Feels Like "Ghost Text"

From the user's perspective, there are two overlapping illusions:

1. The app delays paints while draining too many events, so the visible reply
   appears to stall and then jump.
2. While rows are still arriving, scroll commands operate against the previous
   render's document height, so the viewport can feel detached from the latest
   content.

That combination feels like the trace is haunted by an older version of
itself, even though the main issue is no longer the old markdown stale-tail
cache.

## Corrective Actions

### Immediate code fixes

1. Restore `DRAIN_CAP_PER_FRAME` in `crates/spur-tui/src/app.rs` to `8`.

This is the highest-signal fix for fast-streaming smoothness and brings the
implementation back in line with the approved streaming-backbone design.

2. Make scroll operations apply against fresh row metrics.

Fix shape options:

- rebuild/clamp row metrics before applying scroll intent
- store scroll deltas separately and resolve them after row rebuild
- or provide a cheap "current max offset" recomputation path for the active
  cache/content state before mutating `scroll_offset`

3. Strengthen mermaid cache invalidation.

Replace the current `mermaid_registry.len()`-based cache key with one that
changes when a fence transitions between `Pending`, `Rendering`, `Ready`, and
`Error`.

### Regression tests to add

1. `scroll_while_streaming_uses_fresh_geometry`

Sketch:

- render a trace to establish baseline `last_total_lines`
- append more streamed text without painting
- issue `scroll_up` or `page_up`
- render again
- assert the viewport lands on the expected slice rather than slipping

2. `app_main_loop_uses_eight_event_drain_cap`

Sketch:

- drive the main loop with a burst of pending Spur events
- assert that painting occurs before all queued events are drained

3. `mermaid_state_transition_invalidates_virtual_row_cache`

Sketch:

- render a fence in `Pending`
- transition it to `Ready` without changing registry size
- render again
- assert the placeholder/image rows are rebuilt

## Files Reviewed

- `crates/spur-tui/src/views/session_detail.rs`
- `crates/spur-tui/src/components/react_trace/mod.rs`
- `crates/spur-tui/src/components/react_trace/builder.rs`
- `crates/spur-tui/src/components/react_trace/render.rs`
- `crates/spur-tui/src/components/markdown_stream.rs`
- `crates/spur-tui/src/app.rs`
- `docs/superpowers/plans/2026-04-14-spurevent-stream-backbone-plan.md`
- `docs/superpowers/specs/2026-04-14-spurevent-stream-backbone-design.md`

## Conclusion

For the current codebase, the best RCA is no longer "dirty markdown streams
render stale cached items."

That bug was already fixed.

The strongest remaining explanation is:

- **frame cadence is too coarse** because the app still drains up to `64`
  Spur events before painting
- **scroll geometry is stale during concurrent streaming** because scroll math
  depends on last-render totals rather than current content

The next implementation should target those two issues first.

## Resolution (2026-04-18)

The corrective actions in this RCA were superseded by the fix design at
`docs/superpowers/specs/2026-04-18-session-detail-streaming-ghost-text-fix-design.md`.
Phase 0 (RC1), Phase 1 (F1+F2+RC2), and Phase 2 (F3) are implemented on
branch `feat/ghost-text-fix`. SIM-1, SIM-2, SIM-3 are active regression
guards. Final test count: 153 lib tests pass, 1 ignored (informational
diagnostic only).

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
