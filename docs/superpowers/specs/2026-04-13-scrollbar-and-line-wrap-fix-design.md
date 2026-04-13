# Scrollbar visibility + row-exact line wrap — Design spec

**Date:** 2026-04-13
**Status:** Draft — ready for implementation after user review
**Scope:** `crates/spur-tui/src/components/react_trace.rs` + one new module for the wrap helper

## Problem

User screenshot shows the session trace with:
- A small, near-minimum-size scrollbar thumb near the bottom-right of the panel.
- Mid-document content visible (sections 8 and 9 of a numbered list).
- User's complaint: *"scrollbar is not reflect with the content, there are many lines is hidden."*

## First-principles analysis

Two independent bugs compound. A third suspected bug turned out to be a false alarm after re-analysis.

### Bug 1 — `ScrollbarState` missing `viewport_content_length` *(UX)*

`react_trace.rs:518`:

```rust
let mut scrollbar_state = ScrollbarState::new(total_lines).position(offset);
```

`ScrollbarState::new` sets only `content_length`. The unset `viewport_content_length` defaults to **0**, and ratatui renders the thumb at its minimum-possible size in that case. The thumb cannot convey "how much content is visible vs hidden" because it doesn't know the viewport size.

**Result:** a one-cell dot regardless of how much is hidden. This is exactly the thumb in the screenshot.

### Bug 2 — `total_lines` mismatch with ratatui's internal wrap *(correctness)*

`react_trace.rs:486-496`:

```rust
let total_lines: usize = lines.iter()
    .map(|line| {
        let w = line.width() as u16;
        if w == 0 || effective_width == 0 { 1usize }
        else { ((w + effective_width - 1) / effective_width) as usize }
    })
    .sum();
```

This is **character-wrap**: `ceil(line_width / effective_width)`. But `Paragraph::new(lines).wrap(Wrap { trim: false })` renders with **word-wrap** (`WordWrapper` in ratatui's reflow module). The two counts diverge on content with mid-line whitespace:

- Word-wrap produces **fewer** rows when a trailing word fits cleanly after a whitespace break that char-wrap would miss.
- Word-wrap produces **more** rows when a word straddles a column boundary and must move to the next row.

The divergence is usually a few rows per 30-line paragraph, but always present. The consequence:

- `max_offset = our_total − visible_height` is wrong.
- `is_following = true` pins `scroll_offset = max_offset` (our value).
- Paragraph's internal wrap produces a different row count `R`. It skips our offset rows and renders what's left. If `R > our_total`, the final `R − our_total` rows are unreachable — **the tail is hidden**.
- Scrollbar reports `position/our_total`, so thumb appears near the end even though the true end is below.

This matches the symptom exactly.

### Bug 3 — Scrollbar overlays text column *(false alarm)*

Re-analysis: `Scrollbar(VerticalRight)` draws on the rightmost column of the passed area. The passed area equals `area` (the outer Block rect), whose rightmost column is the Block's **right border** — not the inner text region. `Paragraph::block(...)` confines text to `block.inner(area)`, which is 2 cols narrower than `area`. So the scrollbar overdraws the right border, not text. This is expected and correct. **No fix needed.**

## Approach

Fix both real bugs together.

### Fix A — Set `viewport_content_length` on `ScrollbarState`

One-line change:

```rust
let mut scrollbar_state = ScrollbarState::new(total_lines)
    .position(offset)
    .viewport_content_length(visible_height);
```

Produces a thumb whose size is proportional to `visible_height / total_lines` and whose position reflects where we are in the content — what users expect a scrollbar to show.

### Fix C — Row-exact wrap: pre-wrap spans ourselves, disable `Paragraph::wrap`

Replace the approximate char-wrap count with a deterministic pre-wrap that we also render. Concretely:

1. Add a helper `wrap_line_to_width(line: &Line, width: u16) -> Vec<Line>` that returns one or more `Line`s each with `line.width() <= width`.
2. In `render()`, after building `lines: Vec<Line>`, expand via `flat_map(|l| wrap_line_to_width(&l, effective_width))` into `wrapped: Vec<Line>`.
3. Use `wrapped.len()` as `total_lines` — now exact.
4. Remove `.wrap(Wrap { trim: false })` from the `Paragraph` call. Pass `wrapped` directly; each Line is already ≤ `effective_width`, so ratatui will not need to wrap.
5. `Paragraph::scroll((offset, 0))` now operates on row-exact offsets. `max_offset = wrapped.len() - visible_height` is exact; follow pins to the true last row; tail is always reachable.

### Wrap helper — contract

- **Input:** `&Line`, `width: u16`.
- **Output:** `Vec<Line>` where:
  - Each returned Line has `.width() as u16 <= width`.
  - Concatenating all returned Lines reproduces the source content (span styles preserved, text order preserved).
  - If the source fits in `width`, returns `vec![source.clone()]`.
  - If `width == 0`, returns `vec![source.clone()]` (degenerate — pass through, let ratatui do whatever).
  - Empty lines (width 0) pass through as a single empty Line.
- **Algorithm:** greedy word-wrap over grapheme clusters:
  - Walk each grapheme left-to-right. Track accumulated width and the index of the last whitespace-grapheme encountered.
  - When adding the next grapheme would overflow `width`, emit the current line ending at the last whitespace (dropping the trailing whitespace from the emitted line, continuing from just after it).
  - If no whitespace was seen on the current line (a single word longer than `width`), break at the character boundary.
- **Span preservation:** the helper reconstructs Lines as `Vec<Span>` such that each Span groups consecutive characters with the same `Style`. Mid-word breaks may split a single source Span into two; the split halves share style.
- **Unicode handling:** use `unicode_width::UnicodeWidthChar::width(c).unwrap_or(1)` per `char`. (We treat a `char` as a grapheme approximation; combining marks are rare in LLM markdown output and the worst-case behavior is a one-column miscount — acceptable.)

### Dependency

Add `unicode-width` to `crates/spur-tui/Cargo.toml`. This is already transitively available via `ratatui` but importing it directly makes the version explicit.

## File changes

| File | Change |
|---|---|
| `crates/spur-tui/src/components/line_wrap.rs` | **New.** `wrap_line_to_width` helper + unit tests. |
| `crates/spur-tui/src/components/mod.rs` | Add `pub mod line_wrap;` |
| `crates/spur-tui/src/components/react_trace.rs` | Apply Fix A and Fix C in `render()`. Remove char-wrap `total_lines` computation; use `wrap_line_to_width` output. |
| `crates/spur-tui/Cargo.toml` | Add `unicode-width` dep. |

## Testing

Unit tests for `wrap_line_to_width` covering:

1. Empty line → returns `[empty Line]`.
2. Line fits within width → returns `[clone]`.
3. `width == 0` → returns `[clone]` (no wrap attempted).
4. Plain ASCII text, multiple words, width forces one break → two Lines, clean word-break, no trailing space on line 1.
5. Plain ASCII text, width forces three breaks → four Lines.
6. Single word longer than width → splits at char boundary, each split ≤ width.
7. Mixed styles: timestamp-span + indent-span + body-span where body triggers a break → spans preserved, body span split across two output Lines with same style.
8. Line with 2-column-wide characters (emoji or CJK) that straddles a boundary → correct width accounting.
9. Leading whitespace preserved on first output Line.
10. Trailing whitespace at a break position is dropped (not carried onto the next line).
11. Invariant check: `output.iter().map(|l| l.width()).max() <= width` for every input.

No runtime test — the symptom is visual; correctness comes from the unit tests plus a manual smoke test after landing.

## Acceptance

- Workspace builds cleanly.
- All new tests pass.
- Manual smoke: scrollbar thumb size now proportional to visible content; dragging scroll-to-bottom reaches the true last row of the trace; no visual content appears to be below the viewport when "following" is active.
- No visible regressions in the existing trace rendering (emoji, timestamps, indentation all render as before).

## Non-goals

- Rewriting the trace data model or introducing turn-grouping (H2 fix is a separate concern).
- Caching wrapped lines across frames (wrap is re-run every frame; optimize later if profiling shows it).
- Changing the scrollbar orientation or style.
- Fixing the Phase-1 probe-F miscount (already fixed in `ece987d`).

## Decision log

- Considered using `textwrap` crate: rejected because it operates on strings and would require lossy span round-tripping.
- Considered matching ratatui's internal reflow via reflection/private API: rejected as brittle and version-locked.
- Considered keeping `.wrap()` and just improving the `total_lines` formula to approximate word-wrap: rejected because approximation can never be exact; row-exact scrolling is the clean design.
