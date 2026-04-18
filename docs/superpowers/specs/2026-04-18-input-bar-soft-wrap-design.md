# InputBar Soft-Wrap — Design Spec

**Date:** 2026-04-18
**Component:** `crates/spur-tui/src/components/input_bar.rs`
**Status:** Approved (brainstorming + grounded simulation)

## Problem

`InputBar` is built on `tui-textarea` 0.7.0, which has no soft-wrap. Long
single-line input scrolls horizontally instead of wrapping. `required_height()`
estimates wrap rows by byte length (line 984), which is wrong for CJK, emoji,
and combining marks, and which reserves vertical space the textarea never
uses — producing blank rows below the cursor.

The root cause is the absence of an intermediate visual-coordinate system.
`tui-textarea` maps `(logical_row, logical_col)` one-to-one onto screen cells
with horizontal scroll; there is no `(vrow, vcol)` layer.

## Decision

Introduce a pure function

```rust
fn wrap(lines: &[String], width: u16) -> WrapLayout
```

that owns bidirectional mapping between **logical** (row, byte-offset) and
**visual** (vrow, vcol) coordinates, and rewrite `InputBar::render()` to paint
through this layout. `tui-textarea` remains the model (buffer, cursor, edit
primitives, selection); we stop using it as the renderer.

This was selected over three alternatives:

- `Paragraph::wrap` — would require a parallel re-implementation of the same
  wrap algorithm to place the cursor, guaranteeing drift on edge cases.
- Injecting soft-newlines into the buffer — corrupts `protected_ranges` byte
  offsets on every resize.
- Forking/replacing `tui-textarea` — re-ports the whole component and ~40
  keybindings for a bug fix.

### Coordinate systems

| Space | Unit | Source of truth |
|---|---|---|
| Byte | UTF-8 offset into a logical line | `protected_ranges`, `cursor_to_byte` |
| Logical | `(row, col)` into `textarea.lines()` | `tui-textarea` cursor |
| Visual | `(vrow, vcol)` cell on screen | `WrapLayout` (new) |

Byte↔Logical is existing code. Logical↔Visual is the new abstraction.

### Wrap algorithm

Word-boundary break with grapheme-cluster fallback:

1. Iterate grapheme clusters via `unicode-segmentation` (already in
   `Cargo.lock`).
2. Width per grapheme via `unicode-width` (already in `Cargo.lock`), with
   `\t` expanded to `TAB_WIDTH = 4` cells.
3. Greedily pack graphemes into a `VisualRow` until the next grapheme would
   exceed `width`.
4. On overflow, scan the current row for the trailing-most whitespace (ASCII
   whitespace or CJK break punctuation `、。，．　`). If found, break there and
   carry the post-whitespace graphemes to the next row. Trailing whitespace
   stays on the current row.
5. If no break opportunity exists (single long word), fall back to a grapheme
   break at the overflow point.
6. An empty logical line always produces one `VisualRow` of width 0.

### `WrapLayout` shape

```rust
struct Grapheme {
    byte_start: usize,   // within logical line
    byte_end: usize,
    width: u8,           // display cells
}

struct VisualRow {
    logical_row: usize,
    byte_start: usize,   // within logical line
    byte_end: usize,
    graphemes: Vec<Grapheme>,
    used_cells: u16,
}

struct WrapLayout {
    rows: Vec<VisualRow>,
    line_to_vrows: Vec<(usize, usize)>, // start_vrow, end_vrow_exclusive
    width: u16,
}

impl WrapLayout {
    fn visual_height(&self) -> u16;
    fn logical_to_visual(&self, row: usize, byte_col: usize) -> (usize, usize);
    fn visual_to_logical(&self, vrow: usize, vcol: usize) -> (usize, usize);
}
```

Both mapping functions must round-trip for every grapheme-boundary and EOL
position. This is the correctness invariant; it is enforced by a unit-test
helper that drives every grapheme start plus EOL through
`logical → visual → logical` and asserts identity (modulo the legal
end-of-vrow / start-of-next-vrow aliasing).

### Rendering

`InputBar::render()` replaces `frame.render_widget(&self.textarea, inner)`
with:

1. `let layout = wrap(self.textarea.lines(), inner.width - h_scroll_margin);`
2. Apply vertical scroll offset `view_top_vrow` so the cursor's `vrow` stays
   within `[view_top_vrow, view_top_vrow + inner.height)`.
3. Build a `Vec<Line>` from visible visual rows, styling each grapheme range:
   - Atoms (from `protected_ranges`) get an accent style.
   - Selection (from `textarea.selection_range()`, converted to visual) gets
     the reversed style.
   - Everything else uses the default style.
4. Render via `Paragraph::new(lines)` with wrap disabled (wrap is already
   applied).
5. Set the cursor cell explicitly with `frame.set_cursor_position(...)` at the
   visual coordinate of the logical cursor.

### `required_height`

Replaced with `WrapLayout::visual_height()` plus border rows, clamped to
`[1, 5]` as today. This fixes the CJK / emoji mis-estimate; no callers change
(`session_detail.rs:1597`, `dashboard.rs:376`, `dashboard.rs:419` already call
it per-frame with the correct width).

### Cursor navigation (per hybrid-C from brainstorming)

Visual-line semantics apply in **Emacs** mode and **Vim Insert** mode for
arrow `Up`/`Down`:

1. `let (vr, vc) = layout.logical_to_visual(cursor_row, cursor_byte_col);`
2. `let target_vr = vr ± 1;` (clamped to `[0, layout.rows.len())`)
3. Snap desired column: `let target_vc = vc.min(layout.rows[target_vr].used_cells);`
4. `let (r, bc) = layout.visual_to_logical(target_vr, target_vc);`
5. Convert `bc` (byte) to char-column for `CursorMove::Jump`.

**Vim Normal** mode keeps `j`/`k` logical (call-through to `CursorMove::Down`
/ `Up` unchanged) — matches real vim with `nowrap`.

### Protected ranges

Unchanged. `protected_ranges` remains byte-indexed against the logical buffer.
The wrap layer is render-only; it never mutates `textarea`. Atoms that cross a
wrap boundary paint as two contiguous styled runs; cell count is conserved
(verified in simulation case 11).

## Module layout

Add a private module inside the same file or split into a submodule folder —
to be decided in the plan based on existing codebase convention. Target:

- `wrap.rs` — `Grapheme`, `VisualRow`, `WrapLayout`, `wrap()`, `TAB_WIDTH`.
  Pure, no ratatui types, no I/O. Unit-testable.
- `input_bar.rs` — consumes `WrapLayout` in `render()`, `required_height()`,
  and the two visual-line nav helpers.

## Testing

Port the grounding simulation at `/tmp/wrap-sim/src/main.rs` into `#[test]`
functions alongside `wrap.rs`. Cases:

1. ASCII long line — wrap geometry.
2. Empty buffer — 1 visual row.
3. CJK — width 2 per grapheme, wrap at cell count not byte count.
4. ZWJ emoji family — one grapheme, width 2, no cursor slicing mid-cluster.
5. Combining mark — grapheme cluster unity.
6. Tab expansion at `TAB_WIDTH = 4`.
7. Word-boundary break preferred over mid-word.
8. Long-word fallback to grapheme break.
9. Multiple logical lines including empty.
10. Cursor `Down` crosses wrap boundary.
11. Protected range spans wrap boundary — cell count conserved.
12. Width = 1 pathology.
13. `visual_height` oracle for (80,80), (80,81), (40,200), etc.

Plus a round-trip property test: for any `(lines, width)`, every grapheme
start and EOL position satisfies `visual_to_logical(logical_to_visual(p)) = p`
(with the documented vrow-boundary aliasing).

Ratatui `Buffer`-level snapshot tests for render output are scoped to the
implementation plan, not this design.

## Out of scope

- Right-to-left / bidirectional text (ratatui does not support bidi).
- UAX #14 full line-breaking compliance (future enhancement if needed).
- Grapheme-aware cursor movement in Vim Normal mode (`j`/`k` stay logical).
- Virtual scroll for histories longer than 5 visible rows — covered
  lightly by `view_top_vrow`; advanced viewport behaviors deferred.

## Risk register

| Risk | Mitigation |
|---|---|
| `tui-textarea` updates break `selection_range()` API | Pinned to 0.7.0; API verified present. |
| `required_height()` change shifts parent layouts | Same callers, same signature, same clamp range; output only changes for CJK/emoji/long lines (which were wrong before). |
| Perf regression from per-frame wrap | Chat input bounded < ~1 KB; wrap is O(n) grapheme pass, microseconds. No caching in v1. |
| Atom styling desync at wrap boundary | Verified by simulation (case 11: 23 cells conserved). |

## Dependencies

None added. `unicode-segmentation 1.13.2` and `unicode-width 0.2.0` are
already in `Cargo.lock` (transitively via ratatui).

## Grounding artifact

`/tmp/wrap-sim/` contains the standalone simulation that exercised the
algorithm against 13 adversarial cases; all passed. Its `main.rs` is the
seed for the test suite in `wrap.rs`.
