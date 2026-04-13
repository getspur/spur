# Mermaid Inline Rendering Design

**Status:** Approved
**Date:** 2026-04-13
**Supersedes parts of:** `2026-04-13-session-detail-markdown-mermaid-design.md` (the Alt-v–only overlay rendering path)

## Goal

Render mermaid diagrams inline with the surrounding prose in `ReactTrace`, so a single agent message containing multiple `\`\`\`mermaid` fences displays all of them in-place — no context switch required. The existing Alt-v overlay is retained as a zoom-to-fullscreen action for diagrams that are too large to read at inline size.

## Motivation

The v1 integration (shipped) renders fences as `[📊 mermaid #N · press Alt-v to view]` placeholders. The user reported two problems:

1. **Context-switch tax:** every diagram requires Alt-v → view → close. Cognitive cost dominates value for small diagrams.
2. **Only one-at-a-time:** when an agent message contains N diagrams, the overlay shows one; the user cannot correlate multiple diagrams with their prose context.

The registry + lifecycle state machine from v1 are sound. Only the **render path** needs to change.

## Non-Goals

- Inline rendering for replayed-history entries (entries re-hydrated from disk). Today these have `markdown: None`; adding inline rendering requires pushing replay text through `MarkdownStream`, which is a separate feature.
- Smooth pixel-accurate scrolling through a partially-clipped image. Ship a graceful placeholder fallback first; the pixel-accurate path is deferred as v2.
- Per-diagram user-configurable inline height.
- LRU cache for protocol memory (premature at current diagram counts).

## Architecture

The core change: stop flattening `MarkdownStream` output to a `Vec<Line>` with text-only placeholders. Instead emit a structured item list; during render, flatten to a virtual-row list where image items occupy multiple rows. This lets scroll math stay row-based while allowing the render path to allocate sub-Rects for `ratatui-image` widgets.

### Structural change: `StreamItem`

```rust
pub enum StreamItem {
    Text(Vec<Line<'static>>),   // wrapped prose (0..N lines)
    Fence(MermaidId),           // one diagram placeholder position
}
```

`MarkdownStream::items() -> &[StreamItem]` becomes the primary accessor. The existing `lines()` stays as a back-compat view that substitutes placeholder text for each `Fence(id)` — non-markdown code paths and tests keep working during transition.

### Virtual-row flattening in `ReactTrace::render`

Per entry, expand its `StreamItem` list into a `Vec<VirtualRow>`:

```rust
enum VirtualRow<'a> {
    Text(&'a Line<'static>),
    ImageRow { id: MermaidId, row_within: u16, total_rows: u16 },
}
```

Image `total_rows` is derived at render time from the diagram state:

- `Ready { image }`: `ceil(image.height_px / cell_height_px)`, clamped to `[6, 20]`.
- `Pending`: 1 row (the `⏳` placeholder line).
- `Error`: 1 row (the `⚠` placeholder line).

Cell height in pixels comes from `ratatui_image::picker::Picker::font_size().1` (the picker we already build in `app.rs`). Fallback to 16px if the picker is unavailable.

### Render walker

Walk visible virtual rows (between `scroll_offset` and `scroll_offset + visible_height`). Collapse runs:

- **Contiguous `Text` rows** → one `Paragraph` rendered into a single `Rect`.
- **Contiguous `ImageRow` with the same `MermaidId`** → one image Rect:
  - If the run's length equals `total_rows` (the entire diagram is visible), render `StatefulImage` into the Rect via the diagram's `StatefulProtocol`.
  - If the run is shorter (top or bottom of the diagram is clipped by scroll), render the `[📊 mermaid #N · Alt-v to zoom]` placeholder as a single line into the Rect's top row. This avoids "half-image" artifacts. **v2 would crop the image to the visible pixel slice; explicitly deferred.**

The outer container is still bounded by the Session block; the difference is we no longer hand everything to one `Paragraph::scroll()` — we render segment-by-segment using `Layout::vertical` on the visible Rect.

### Protocol lifecycle

`MermaidState::Ready` grows one field:

```rust
Ready {
    image: DynamicImage,
    inline_protocol: RefCell<Option<StatefulProtocol>>,
}
```

- Built lazily on the first frame the diagram is visible, at exactly its inline Rect size (pane_width × `total_rows`-worth-of-pixels). Re-encode on resize only.
- On crossterm `Resize` events, `app.rs` calls `session_detail.invalidate_inline_protocols()`, which walks every `MermaidState::Ready` and sets its `inline_protocol` to `None`. Rebuilt lazily next render.
- The overlay's separate `StatefulProtocol` (in `mermaid_viewer.rs`) is unaffected — the two live side-by-side.
- `RefCell` is chosen so `View::render`'s existing `&self` signature does not change (cascading through the `View` trait). Interior mutability is scoped to the render-time protocol cache; no other code paths touch it.

### Alt-v overlay: retained as zoom

The overlay is demoted to a secondary UX: press Alt-v to zoom the currently-focused inline diagram to full-screen. Useful for detailed flowcharts where the 6–20 row inline strip is too cramped.

Focus heuristic: the first `Ready` diagram whose virtual rows intersect the current viewport. If none intersect, fall back to the last `Ready` diagram (existing behavior).

### Memory profile

Per-diagram inline protocol ≈ `inline_width_px × inline_height_px × 4` bytes of RGBA, plus the protocol's internal state. For typical sizes (640×360): ≈ 1 MB each. A session with 50 diagrams ≈ 50 MB. Accepted; LRU bounds deferred.

## Data Flow

```
AgentMessageChunk
        │
        ▼
MarkdownStream::append  (raw text accumulation, debounced)
        │
        ▼
MarkdownStream::rebuild ─► emits Vec<StreamItem>
        │
        │   (new mermaid fence detected)
        ▼
Action::MermaidRenderRequest { id, code }
        │
        ▼
spawn_blocking(render_mermaid) → DynamicImage
        │
        ▼
Action::MermaidRenderCompleted { id, result }
        │
        ▼
SessionDetailView::handle_mermaid_completed
        │   inserts Ready { image, inline_protocol: RefCell<None> }
        ▼
ReactTrace::render (next frame)
        │
        │  walks VirtualRows, finds ImageRow(id, ...)
        │  borrow_mut() the RefCell, lazily build Protocol if None
        ▼
StatefulImage renders into Rect
```

## Error Handling

- Panics in rasterization: already caught in `render_mermaid` via `catch_unwind`. Unchanged.
- Picker unavailable (non-graphics terminal): the protocol build is skipped; inline render falls back to the text placeholder line. Same graceful degradation as v1.
- Protocol build failure (e.g., picker OK but encoding step fails): log at `warn!` level, store `None`, render the text placeholder for that frame. Retry on next resize or diagram re-render.

## Testing Strategy

Unit-level (no live terminal):

1. `MarkdownStream::items()` returns `[Text, Fence, Text]` for input `prose\n\`\`\`mermaid\n...\`\`\`\nmore prose`.
2. `MarkdownStream::items()` correctly interleaves multiple fences in one stream.
3. `lines()` back-compat view still produces the placeholder strings for existing snapshot tests.
4. `ReactTrace` virtual row count = text_lines + sum(image_heights) across all entries.
5. Scroll-offset past an image hides it entirely (no ImageRow in visible slice).
6. Scroll-offset in the middle of an image's row range produces a `run.len() < total_rows` → test asserts the placeholder text is rendered into that Rect rather than the image widget.
7. Resize invalidation empties every `inline_protocol` RefCell.

Integration-level (behind `cfg(feature = "markdown")`):

8. Existing `first_chunk_renders_body_before_debounce_flush` still passes.
9. Existing `post_flush_rendered_lines_still_show_text` still passes.
10. New: a trace entry with two fences produces two distinct `Fence` items and, after both render completions, two distinct image-row runs in the virtual row list.

Manual verification:
- Render a real agent message with two diagrams; confirm both are visible inline.
- Scroll up/down through them; confirm placeholder replaces half-images.
- Resize the terminal; confirm diagrams re-render at the new width.
- Alt-v on an inline diagram opens the zoom overlay.

## Scope Summary

| File | Lines changed (approx) | Notes |
|------|------------------------|-------|
| `components/markdown_stream.rs` | +50 | `StreamItem`, `items()`, back-compat `lines()` |
| `components/react_trace.rs` | +150 | Virtual-row flattening + item-based render walker |
| `components/mermaid.rs` | +20 | `inline_protocol: RefCell<Option<StatefulProtocol>>` on Ready |
| `views/session_detail.rs` | +30 | `invalidate_inline_protocols`, picker plumbing |
| `app.rs` | +15 | crossterm `Resize` → invalidate hook |
| Tests | +120 | Covering tests 1–10 above |

## Risks

1. **Paragraph-to-segment render change may regress scroll UX.** Mitigation: keep `scroll_to_bottom` / following-mode semantics identical; add visual test sweep after Task 5.
2. **Protocol rebuild on resize can stutter if many diagrams exist.** Mitigation: lazy rebuild (on first visibility), not eager. Only visible diagrams pay the cost.
3. **ratatui-image 9.0 API drift.** Mitigation: verify `Picker::new_resize_protocol` returns a `StatefulProtocol` whose Rect we fully control; if the signature differs, fall back to per-draw `StatefulImage` with cached last-Rect sentinel to avoid re-encoding unchanged sizes.
4. **`RefCell` panic on nested borrow.** Mitigation: the only mutation site is the render walker; audit to ensure no re-entry via action dispatch mid-render.
