# Session Detail Streaming Ghost Text — Fix Design

**Date:** 2026-04-18
**Status:** Approved (brainstorming complete, plan pending)
**Scope:** `crates/spur-tui/src/{app.rs, components/react_trace/, components/markdown_stream.rs}`
**Companion docs:**
- RCA: `docs/superpowers/specs/2026-04-18-session-detail-streaming-ghost-text-rca.md`
- This spec supersedes the RCA's "Corrective Actions" section.

## Problem

The Session Detail trace exhibits "ghost text" during streaming: visible
content shifts without user input. The original RCA correctly identified
two amplifiers (drain cap, stale scroll geometry) but missed the structural
cause. L9 review with simulation tests SIM-1..8 (in
`crates/spur-tui/src/components/react_trace/streaming_tests.rs`)
empirically established:

- **Layer 2A (proven by SIM-1):** mid-stream `flush_now` produces a different
  row sequence than tail-mode rendering for the same bytes, because trailing
  unflushed bytes lack the paragraph context final-flush gives them. With a
  payload of `# Heading + code fence + list + tail`, row count drops from 14
  to 13 across a single flush with zero new bytes.
- **Layer 3E (proven by SIM-2):** `scroll_offset` is a row index, not a
  content anchor. When the row sequence reflows, the same `scroll_offset`
  exposes different content. SIM-2 reproduces a viewport-content shift with
  zero user input.
- **F3-alone insufficiency (proven by SIM-3):** a content anchor stabilizes
  the top of the viewport but does not stabilize rows below it when the row
  sequence itself reflows. F1 and F3 are orthogonal and both required.
- **F1 sharper than first scoped (proven by SIM-5):** post-final-flush
  rendering is bijective with raw bytes; mid-stream-flush rendering is not.
  F1's job is to make tail-mode emit the same row sequence final flush
  would emit.

## Goals

1. Eliminate ghost text under all eight identified scenarios (cadence,
   tail→items reflow, stale cache, scroll-while-stream race, width resize,
   mermaid state transitions, eviction, and >64KB safety-cap blowout).
2. Establish the invariant `visible(N+1) = visible(N) shifted by f(I)`
   where `I` is user input and `f` is independent of producer events.
3. Promote SIM-1, SIM-2, SIM-3 from `#[ignore]` to active regression
   guards once each phase lands.

## Non-Goals

- Rewriting the markdown parser. We continue to use `pulldown-cmark` via
  `tui-markdown`.
- Changing the `MarkdownStream` debounce policy. The 50ms debounce stays.
- Changing `SpurEvent` ingestion semantics.

## Approach: three sequenced phases

### Phase 0 — Cadence (10 minutes)

Restore the approved drain cap.

**Change:** `crates/spur-tui/src/app.rs:1627` — `DRAIN_CAP_PER_FRAME: 64 → 8`.

This brings the streaming-backbone design back to its approved value and
removes the burst-coalescing amplifier. Single commit, no API change.

### Phase 1 — Symmetric rendering and cache correctness (~2 days)

Three small fixes that together eliminate intra-entry ghost text.

#### F1 (sharp): tail rendering matches what final flush would produce

**Where:** `crates/spur-tui/src/components/react_trace/builder.rs`
(`render_agent_message_body`) and a new helper in
`crates/spur-tui/src/components/markdown_stream.rs`.

**Design:** Today, `render_agent_message_body` emits `items` via the
pulldown-derived `StreamItem::Text(Vec<Line>)` and emits `tail` via plain
`tail.lines()`. The two paths produce different row sequences for the
same bytes when the tail straddles structural markdown boundaries
(lists, headings, fences).

The fix introduces a **preview-flush** path on `MarkdownStream`:

```rust
// MarkdownStream
/// Render the current raw_text as if final flush had occurred, without
/// mutating any of the stream's persistent state. Returns the items
/// vector that final flush would produce. Pure with respect to self.
pub fn preview_items(&self, states: &StateLookup<'_>) -> Vec<StreamItem>;
```

`render_agent_message_body` then uses `stream.preview_items(states)` for
its entire output, ignoring the committed/uncommitted split. The
`flushed_byte_len` cursor and `cached_items` retain their existing role
of *amortizing* parse cost: incremental cache invalidation still keys on
them, but the rendered output no longer depends on them.

**Why this is sound:** `preview_items` parses the same bytes through the
same pulldown invocation as `flush_final` would. By construction, the
row sequence is identical to what the user will see after the next flush.
Reflow at flush time becomes a no-op for the rendered output.

**Cost analysis:** `preview_items` is invoked per render of an
`AgentMessage` entry. Cost is dominated by pulldown-cmark's parse, which
is O(raw_text bytes). The existing `cached_items` cache is preserved as
a per-stream memoization keyed on `(raw_text.len(), fence_state_hash)`.
A render that hits the cache is free. The only overhead is when the
cache misses, which corresponds to events that already require a parse.

#### F2: per-stream cache key includes flushed byte position

**Where:** `crates/spur-tui/src/components/react_trace/render.rs`
(`render_with_ctx`, the cache-check block at lines 334-401) and
`VirtualRowCacheEntry`.

**Design:** Extend `VirtualRowCacheEntry` with a per-stream digest:

```rust
struct VirtualRowCacheEntry {
    rows: Vec<VirtualRow>,
    entry_row_starts: Vec<usize>,
    width: u16,
    generation: u64,
    fence_gen: u64,
    // NEW: vector indexed by entry_idx, holds (raw_text.len(), flushed_byte_len)
    // for each AgentMessage entry. Re-render if any entry's digest changed.
    stream_digests: Vec<Option<StreamDigest>>,
}

#[derive(PartialEq, Eq)]
struct StreamDigest {
    raw_text_len: usize,
    flushed_byte_len: usize,
}
```

Cache invalidation extends to compare `stream_digests`. This catches the
silent-flush case (debounce timer fires between two renders without
chunk arrival) which the current `generation`/`fence_gen` keys miss.

#### RC2 (folded into Phase 1): scroll operations apply to fresh row metrics

**Where:** `crates/spur-tui/src/components/react_trace/mod.rs`
(scroll_up, scroll_down, page_up, page_down, scroll_to_bottom) and a
new fast-path in render.

**Design:** Each scroll mutator must call a cheap "what's the current
row count?" function before clamping. Today they read
`self.last_total_lines`, which is updated only inside `render*()`.

The fix introduces a private `current_row_count(width_hint: u16) -> usize`
that consults the cache if valid for the cached width, or recomputes
metrics by walking entries with `build_virtual_rows(0, width_hint, ...)`
without caching. Width hint is the most-recent `last_visible_height`
context's width. Scroll mutators call this before clamping.

For the `scroll_to_bottom` path called from `append_message`, the same
helper applies: clamp against fresh max_offset, not stale.

### Phase 2 — Content-anchor scroll model (~2 days)

#### F3: byte-offset anchor

**Where:** `crates/spur-tui/src/components/react_trace/mod.rs` (scroll
state and mutators), `react_trace/render.rs` (resolution at render time),
`react_trace/builder.rs` (per-row byte-range emission).

**Design:**

Replace `scroll_offset: usize` with:

```rust
enum ScrollAnchor {
    /// Pinned to the bottom of the document — auto-follow new content.
    Following,
    /// Pinned to a specific byte position within an entry.
    Byte { entry_idx: usize, byte_offset: usize },
}
```

`build_virtual_rows` is extended to emit per-row byte ranges via a
new `VirtualRow::Text { line, byte_range: Option<Range<usize>> }`
variant or a parallel `Vec<Option<Range<usize>>>` returned alongside
the rows. Byte ranges are produced naturally during the
`StreamItem::Text` walk: pulldown gives us the byte range each `Event`
covers; the `wrap_line_to_width` step proportionally subdivides those
ranges across visual rows.

Anchor resolution at render time:
1. Walk `entry_row_starts` to find the row range for `entry_idx`.
2. Within that range, binary-search the per-row byte ranges for the row
   whose range contains `byte_offset`.
3. If `byte_offset` falls in a gap (deleted by reflow), snap to the
   nearest preceding row whose byte range includes a byte ≤
   `byte_offset`.
4. The resolved row index becomes the effective `scroll_offset` for the
   viewport slice.

Scroll mutators update the anchor:
- `scroll_up`: resolve current row, find row above, anchor to that row's
  starting byte.
- `scroll_down`: symmetric. If resolution lands at the last row of the
  document, transition to `Following`.
- `page_up`/`page_down`: same logic, jumping by `visible_height - 2`.
- `scroll_to_bottom`: set `Following`.
- `scroll_to_top`: anchor to `(0, 0)`.

Eviction (`push()` evicts old entries when over capacity): if the
anchor's `entry_idx` was evicted, snap to `(0, 0)` of the first
surviving entry.

`is_following` becomes a derived property: `matches!(anchor, ScrollAnchor::Following)`.

**Width resize:** byte_offset is invariant under width change. Resize
just re-resolves the anchor; viewport content stays the same.

**Mermaid state transition:** Pending → Ready changes the row count for
a fence (placeholder→image). Anchor resolution falls through naturally:
if anchor is on the placeholder row, snap to first ImageRow of the
fence; if anchor is past the fence, the row index just shifts and
re-resolves.

## Components and data flow

After all three phases:

```
SpurEvent (chunk)
    │
    ▼
SessionDetailView::handle_spur_event
    │ (drain cap = 8, Phase 0)
    ▼
ReactTrace::append_message
    │ — appends to MarkdownStream::raw_text
    │ — bumps generation
    │ — sets dirty_from
    ▼
ReactTrace::render_with_ctx
    │ — cache check (Phase 1 F2: includes stream_digests)
    │ — incremental rebuild from dirty_from
    │ — build_virtual_rows emits VirtualRow + byte_range (Phase 2 F3)
    │     └─ render_agent_message_body uses stream.preview_items() (Phase 1 F1)
    │ — anchor resolved against fresh per-row byte ranges (Phase 2 F3)
    │ — viewport slice paints
    ▼
ratatui Frame
```

## Test plan

### Promoted to active regression guards
- `sim_tail_to_items_reflow_row_delta` (after Phase 1 F1 lands)
- `sim_viewport_content_shifts_under_flush_with_no_input` (after Phase 1 F1)
- `sim_fix_content_anchor_eliminates_ghost_text` (after Phase 2 F3)

### New tests added with implementation
- `phase1_f1_preview_items_is_pure`: calling `preview_items` twice
  returns the same Vec<StreamItem> and does not mutate cursor.
- `phase1_f1_preview_matches_final_flush`: for representative payloads
  (prose, heading + fence + list, nested list, table), assert
  `preview_items(s)` and final-flushed `s.items()` produce identical
  StreamItem sequences.
- `phase1_f2_silent_flush_invalidates_cache`: render entry, advance
  flushed_byte_len without bumping generation, render again — assert
  rebuild occurred via stream_digest change.
- `phase1_rc2_scroll_uses_fresh_row_count`: append text without
  rendering, call `scroll_up`, render — assert scroll_offset clamped
  against fresh totals.
- `phase2_f3_anchor_byte_offset_survives_reflow`: anchor at a specific
  byte position in an entry; trigger flush that reflows; assert
  resolution lands on a row containing that byte.
- `phase2_f3_anchor_survives_eviction`: anchor in entry 0; push enough
  entries to evict entry 0; assert anchor snaps to first surviving
  entry without panic.
- `phase2_f3_anchor_survives_width_resize`: anchor at a byte;
  re-render at half width; assert resolved row contains the same
  byte.
- `phase2_f3_anchor_blank_line_snap_to_preceding`: anchor on a blank
  line that's consolidated by reflow; assert resolution snaps to the
  preceding non-blank row.

### Existing tests to update
Tests using literal `scroll_offset: usize` values (search for
`scroll_offset` outside the `react_trace` module) need rewriting to
use `ScrollAnchor::Byte { ... }`. Estimated ~10 occurrences based on
prior grep.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `preview_items` parse cost on every render | Memoize at MarkdownStream level keyed on `(raw_text.len(), fence_state_hash)`. Existing incremental cache continues to amortize across renders. |
| Per-row byte_range inflates VirtualRow size | Use `Option<Range<usize>>` (24 bytes when populated, 8 when None). Acceptable for viewport-bounded slices. |
| Anchor on blank line consolidated by reflow | Snap-to-preceding policy spec'd above. SIM-3 demonstrates the failure mode the policy must address. |
| F3 touches scroll API broadly | All scroll mutators follow the same pattern (resolve → mutate → reanchor). Bounded scope. Tests catch any caller miss. |
| `is_following` derivation may miss state callers | Audit `is_following` references; provide a backward-compat method that derives from anchor. |

## Migration order rationale

- Phase 0 first because it's a constant change with no API impact and
  immediate user-visible improvement.
- Phase 1 second because it eliminates the dominant ghost-text class
  (intra-entry tail→items shift) without introducing new API surface.
  Existing scroll API stays.
- Phase 2 last because it changes the scroll API. Once Phase 1 ships,
  the urgency drops, and Phase 2 can be designed and reviewed
  carefully.

## Files modified

- `crates/spur-tui/src/app.rs` — Phase 0 only.
- `crates/spur-tui/src/components/markdown_stream.rs` — Phase 1 (preview_items).
- `crates/spur-tui/src/components/react_trace/builder.rs` — Phase 1 (use preview_items), Phase 2 (emit byte_range).
- `crates/spur-tui/src/components/react_trace/render.rs` — Phase 1 (F2 cache key, RC2), Phase 2 (anchor resolution).
- `crates/spur-tui/src/components/react_trace/mod.rs` — Phase 1 (RC2 fresh metrics helper), Phase 2 (ScrollAnchor enum, mutators).
- `crates/spur-tui/src/components/react_trace/types.rs` — Phase 2 (VirtualRow byte_range or sibling vec).
- `crates/spur-tui/src/components/react_trace/streaming_tests.rs` — promote ignored SIMs, add new regression tests.

## Open questions

None blocking implementation. Two design choices to confirm during
implementation:

1. byte_range carrier: extend `VirtualRow::Text` variant or return a
   parallel `Vec<Option<Range<usize>>>` from `build_virtual_rows`?
   Parallel vec is less invasive; embedded is more cohesive. Pick during
   plan-writing.
2. `preview_items` memoization key: `(raw_text.len(), fence_state_hash)`
   versus full content hash. Length-based key is faster but assumes
   raw_text is append-only (which it is). Confirm append-only invariant
   holds.
