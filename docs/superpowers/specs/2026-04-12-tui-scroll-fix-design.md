# TUI Session Scroll Fix

## Problem

`ReactTrace` scroll logic operates on entry counts, but `Paragraph::scroll((offset, 0))` expects rendered-line offsets. A single trace entry (THINK block with 50 lines of text + header + blank separator) renders as 52+ lines, but scroll increments by 1 entry. This makes scrolling through long sessions nearly impossible — `j`/`k` barely move, `G` doesn't reach the true bottom, and there's no page-based navigation.

Root cause: two conflated coordinate systems (entry-based vs rendered-line-based).

Reference: [ratatui#2342](https://github.com/ratatui/ratatui/issues/2342) — known open issue. The community-recommended workaround is `Line::width() / effective_width` to compute true rendered height per line.

## Design

### Core fix: rendered-line-based scroll offset

Add cached render metrics to `ReactTrace`:

```rust
struct ReactTrace {
    // ... existing fields ...
    last_total_lines: usize,      // total rendered lines from last render()
    last_visible_height: usize,   // visible area height from last render()
}
```

In `render()`, after building `Vec<Line>`:

1. Compute `inner = block.inner(area)` to get the content area inside the bordered block. Use `inner.width` as the effective width and `inner.height` as the visible height. This is how ratatui internally determines the rendering area — no manual border math.
2. Sum rendered height per line: `ceil(line.width() / inner.width)`, with zero-width lines counting as 1.
3. Store as `last_total_lines` and `last_visible_height`.
4. If `is_following`, pin `scroll_offset` to `total - visible`.
5. Otherwise clamp `scroll_offset` to `max(0, total - visible)`.
6. Pass `scroll_offset` to `Paragraph::scroll()` as today.

### Scroll methods

All methods operate in rendered-line space using the cached metrics:

| Method | Behavior |
|--------|----------|
| `scroll_up()` | offset -= 1; is_following = false |
| `scroll_down()` | offset += 1; re-engage following if at bottom |
| `page_up()` | offset -= (visible - 2); is_following = false |
| `page_down()` | offset += (visible - 2); re-engage if at bottom |
| `scroll_to_top()` | offset = 0; is_following = false |
| `scroll_to_bottom()` | offset = total - visible; is_following = true |

The `-2` overlap on page moves provides reading context (same convention as `less`).

### Auto-follow state machine

```
FOLLOWING ──scroll_up/page_up──→ DETACHED
    ^                                |
    |                                |
    └──G / scroll past bottom────────┘
```

- **FOLLOWING**: `render()` pins offset to bottom. New content auto-scrolls.
- **DETACHED**: offset stays fixed. New content arrives but viewport doesn't move.
- **Re-engage**: `G` key, or `scroll_down`/`page_down` reaching `offset >= total - visible`.

### Keybindings

| Key | Action |
|-----|--------|
| `j` / `Down` | scroll down 1 line |
| `k` / `Up` | scroll up 1 line |
| `Page Down` | page down (visible - 2 lines) |
| `Page Up` | page up (visible - 2 lines) |
| `G` | jump to bottom, re-engage follow |
| `g` | jump to top, detach |

`Page Up` / `Page Down` are new bindings. `j`/`k`/`g`/`G` keep existing binding locations but now operate in the correct coordinate system. `Up`/`Down` in the non-editing section unchanged.

### Scrollbar indicator

Add ratatui's built-in `Scrollbar` widget alongside the `Paragraph` in `render()`:

```rust
let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
let mut scrollbar_state = ScrollbarState::new(total_lines).position(scroll_offset);
frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
```

Visual cue for position and content size. No new crate dependency — `Scrollbar` is part of ratatui.

### Rendered height calculation

```rust
fn rendered_height(line: &Line, effective_width: u16) -> u16 {
    let w = line.width() as u16;
    if w == 0 || effective_width == 0 { 1 } else { (w + effective_width - 1) / effective_width }
}
```

Uses `Line::width()` which is backed by `unicode_width` — the same calculation ratatui uses internally for wrapping. Consistent by construction. The `effective_width` should be `block.inner(area).width` to match ratatui's internal content area.

## Files changed

1. **`crates/spur-tui/src/components/react_trace.rs`**:
   - Add `last_total_lines`, `last_visible_height` fields.
   - Add `rendered_height()` helper.
   - Update `scroll_up`, `scroll_down`, `scroll_to_top`, `scroll_to_bottom`.
   - Add `page_up()`, `page_down()` methods.
   - Update `render()`: compute total lines, cache metrics, clamp offset, render scrollbar.

2. **`crates/spur-tui/src/views/session_detail.rs`**:
   - Add `PageUp` / `PageDown` key handling in priority 3 section.
   - Remove hardcoded `20` from `scroll_down(20)` calls — scroll methods no longer take a visible_height param (they use cached value).

## What does NOT change

- `TraceEntry` / `TraceKind` types.
- `ActivityLog` component (dashboard scroll is a separate concern).
- Event handling, SpurEvent flow, or any other component.
