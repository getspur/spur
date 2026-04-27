# Mermaid Inline Rendering v2

## Problem

Two user-visible issues with mermaid diagrams in the ReAct trace pane (iTerm2):

1. **"Diagram is too small."** Inline diagrams render soft / shrunk.
2. **"Hidden when scroll."** When a diagram scrolls past the viewport edge, its area collapses to a single-line text placeholder.

Root causes (verified against `crates/spur-tui/src/`):

| Symptom | Cause |
|---|---|
| Soft on retina iTerm2 | `DEFAULT_WIDTH = 800px` raster (`components/mermaid.rs:139`). On a 200-col retina pane the protocol up-scales 800px source pixels into 1600+ device pixels. |
| Truncated tall diagrams | `compute_inline_height_rows` clamps to `[6, 60]` (`components/react_trace/render.rs:205`). A 90-row diagram on a 120-row pane caps at 60 cells, then `Resize::Fit(None)` shrinks proportionally. |
| 1-line content on partial scroll | `render_with_ctx` treats `first_row_within != 0 || run_len != total_rows` as "draw a single placeholder line" (`render.rs:573–602`). The reserved height (`run_len`) is preserved but content collapses, leaving most of the area blank with only the message `[📊 mermaid #N · scroll to align]`. |
| Stale-protocol risk | `MermaidState::Ready { inline_protocol: RefCell<Option<StatefulProtocol>> }` couples the data model to the render cache. `invalidate_inline_protocols` exists but only fires on `Event::Resize`. Cell-size drift (font swap without SIGWINCH) silently retains stale metrics. |

## Solution Summary

Four changes, shipped together:

1. **Pane-aware raster bucket policy.** Discrete buckets `[800, 1200, 1600, 2000, 2400]`, bucket-up only, async re-rasterisation when the pane crosses a boundary. Stale image stays displayed during re-raster (no flicker).
2. **Adaptive inline height.** `target_cap = max(2/3 pane, 60)` clamped to `pane − 4`, hard cap `100`. No regression for medium panes; growth for large panes.
3. **Multi-row partial-visibility card.** When a diagram is partially scrolled, render a 1/2/3-line card vertically centred in the reserved Rect, with arrow + word direction labels (`▼ scroll down`, `▲ scroll up`, `▲▼ scroll for more`).
4. **`image_cache` module extraction.** Owns rendered `StatefulProtocol` instances (inline + overlay slots per id), auto-invalidates on `Arc<DynamicImage>` identity drift and cell-size drift. `MermaidState::Ready` becomes pure data.

Smooth-crop (showing the actual image pixels in the visible portion) is **deferred to v3** — `ratatui-image` 9.0.0's `Resize::Crop` is corner-clip only, not arbitrary mid-window source rectangles. v3 needs a custom widget around the iTerm2/Kitty source-rect protocol.

## Architecture

### File plan

| File | Status | Change |
|---|---|---|
| `components/image_cache.rs` | NEW | Owns `StatefulProtocol` per `(MermaidId, Slot)`. Auto-invalidates on Arc drift + cell-size drift. ~140 LOC. |
| `components/mermaid.rs` | modify | Drop `inline_protocol` from `Ready`; add `RASTER_BUCKETS` + `raster_width_for_pane()`; `render_mermaid` takes `target_width: u32`; `Ready` gains `code: String` + `rastered_at_bucket: u32`. |
| `components/react_trace/render.rs` | modify | `compute_inline_height_rows` accepts `(cell_w_px, cell_h_px)` directly; new soft-cap formula; new `render_partial_card` helper; partial-visibility branch rewired to call it. `RenderContext` gains `&mut ImageCache`. |
| `components/react_trace/mod.rs` | modify | `VirtualRowCacheEntry` gains `soft_cap: u16`, `cell_w_px: u32`, `cell_h_px: u32`. Cache-hit check extended (both full-cache and incremental-cache paths). `seed_line_cache_for_tests` gains the three new params. |
| `components/react_trace/builder.rs` | modify | `build_virtual_rows` threads `soft_cap` through to `compute_inline_height_rows` callers. |
| `views/session_detail.rs` | modify | Add `image_cache: ImageCache` + `in_flight_renders: HashSet<MermaidId>` fields. New methods: `maybe_request_rerasters`, `render_overlay`. `handle_mermaid_completed` signature gains `target_width: u32`. Existing fence-emit sites (lines 1617, 1792) populate `target_width`. `invalidate_inline_protocols` → `image_cache.invalidate_all()`. |
| `views/mermaid_viewer.rs` | modify | Drop `protocol` field; viewer is focus-only. `set_available` no longer builds; `protocol_mut` removed. |
| `app.rs` | modify | `Event::Resize` calls `image_cache.invalidate_all()` (via session_detail). Overlay path uses `detail.render_overlay()`. Worker passes `target_width` into `render_mermaid` and echoes back. |
| `action.rs` | modify | `target_width: u32` added to both `MermaidRenderRequest` and `MermaidRenderCompleted`. |

Net: **+285 / −60 LOC** across 9 files plus 1 new module.

### Data flow

```
SpurEvent::AgentMessageChunk → markdown_stream extracts ```mermaid fence
→ MermaidState::Pending { code }
→ Action::MermaidRenderRequest { ref_id, code, target_width: raster_width_for_pane(pane_w_px) }
→ tokio::task::spawn_blocking → render_mermaid(code, target_width) → DynamicImage
→ Action::MermaidRenderCompleted { ref_id, target_width, result }
→ handle_mermaid_completed → MermaidState::Ready { image, code, rastered_at_bucket: target_width }

Per-frame:
  RenderContext { mermaid_registry: &..., image_cache: &mut..., picker: ... }
  for each Image segment:
    if fully_visible:
      proto = image_cache.inline_protocol_mut(id, image, image_generation, picker)
      StatefulImage::Fit(None) → proto into rect
    else:
      render_partial_card(rect, id, total_rows, first_row_within, run_len)

  At end of frame:
    maybe_request_rerasters(pane_cols, cell_w_px)
      for each Ready { rastered_at_bucket } where rastered_at_bucket < current_bucket
        and not in_flight_renders:
          emit MermaidRenderRequest { ..., target_width: current_bucket }
```

## Detailed Designs

### 3.1 Raster Bucket Policy + Re-Raster Trigger

```rust
// components/mermaid.rs
// Six buckets — added 3200 to cover 4K / 5K iTerm2 panes (~3920 px wide
// on a 5K iMac with default cell width). Cap at 3200, not 4000:
// 4000×3000 RGBA ≈ 48 MB per diagram, and v2 has no LRU eviction, so
// a session with 10+ Ready diagrams could exceed 480 MB at 4000.
// 3200×2400 RGBA ≈ 30 MB caps at acceptable per-diagram budget.
const RASTER_BUCKETS: [u32; 6] = [800, 1200, 1600, 2000, 2400, 3200];

pub fn raster_width_for_pane(pane_w_px: u32) -> u32 {
    for &b in &RASTER_BUCKETS {
        if b >= pane_w_px { return b; }
    }
    *RASTER_BUCKETS.last().unwrap()
}

pub enum MermaidState {
    Pending { code: String },
    Rendering,
    Ready {
        image: Arc<DynamicImage>,
        code: String,                  // retained for re-raster
        rastered_at_bucket: u32,       // policy provenance for skip logic
        image_generation: u64,         // monotonic counter; bumped on every Ok completion;
                                       // image-identity tag for `image_cache` invalidation
                                       // (NOT the same as rastered_at_bucket — same bucket
                                       // can recur via Error→Ready retry / duplicate completion)
    },
    Error { message: String },
}

pub fn render_mermaid(code: &str, target_width: u32) -> Result<DynamicImage, RenderError>;
```

**Why discrete buckets?** A continuous formula would re-rasterise on every column resize (~50–500 ms per render). 5 buckets cover the realistic terminal width range; pane-width crossings are rare.

**Why bucket-up only?** Avoids re-raster churn on brief narrowing. Memory monotone within session per diagram (worst case: max bucket × N diagrams).

**Re-raster trigger:** `SessionDetailView::maybe_request_rerasters` runs at end of `render()`. Two-phase (collect candidates → mutate) for borrow-checker robustness.

```rust
fn maybe_request_rerasters(&mut self, pane_cols: u16, cell_w_px: u16) {
    let new_bucket = raster_width_for_pane(pane_cols as u32 * cell_w_px as u32);
    let candidates: Vec<(MermaidId, String)> = self.mermaid_registry.iter()
        .filter_map(|(id, state)| match state {
            MermaidState::Ready { rastered_at_bucket, code, .. }
                if *rastered_at_bucket < new_bucket
                && !self.in_flight_renders.contains(id) => Some((*id, code.clone())),
            _ => None,
        })
        .collect();
    for (id, code) in candidates {
        self.in_flight_renders.insert(id);
        self.pending_fence_actions.push_back(Action::MermaidRenderRequest {
            session: self.session_id.clone(),
            ref_id: id,
            code,
            target_width: new_bucket,
        });
    }
}
```

`handle_mermaid_completed` retains `code` from the previous state (Pending or prior Ready), stores `rastered_at_bucket: target_width`, bumps `next_image_generation` and stores it in the new `Ready`. Removes `ref_id` from `in_flight_renders`. Calls `react_trace.mark_all_streams_dirty()` so placeholders rebuild.

**Stale-session safety:** `tokio::task::spawn_blocking` workers are not cancellable, so a re-raster can complete after the user has navigated away from the session. The app-level `Action::MermaidRenderCompleted` handler at `app.rs:2113-2118` already guards against this — it checks `self.session_detail` exists and that `detail.session_id().0 == session.0` matches the completion's session_id before dispatching to `handle_mermaid_completed`. **Stale completions are silently dropped.** Codex Rust-expert review confirmed this guard is already correct; no change needed.

**Coalescing:** `in_flight_renders: HashSet<MermaidId>` ensures at most one re-raster in flight per id. If a second bucket-up fires before the first completes, the result lands at the older bucket, then the next frame's `maybe_request_rerasters` re-emits.

**User-perceived timing on bucket-up:**
- T+0: pane grows past boundary; existing image continues to display via cached protocol.
- T+0 (same frame): re-raster requests emitted.
- T+~50–100 ms: worker completes; new Arc lands.
- T+~50–100 ms: `image_cache` detects Arc identity drift on next render → rebuilds protocol.
- Next frame: user sees crisper rendering.

### 3.2 Adaptive Inline Height

```rust
// components/react_trace/render.rs
const INLINE_FLOOR_ROWS: u16 = 8;
const INLINE_LEGACY_CAP: u16 = 60;        // preserve today's UX baseline as floor
const INLINE_HARD_CAP:   u16 = 100;       // emergency upper bound
const INLINE_TRAILING_CONTEXT: u16 = 4;

pub(crate) fn compute_inline_height_rows(
    image: &DynamicImage,
    pane_width_cols: u16,
    pane_height_rows: u16,
    cell_w_px: u32,
    cell_h_px: u32,
) -> u16 {
    let pane_width_px = (pane_width_cols as u32).saturating_mul(cell_w_px);
    if pane_width_px == 0 || image.width() == 0 || pane_height_rows == 0 {
        return 0;
    }

    let scaled_h_px = ((image.height() as u64) * (pane_width_px as u64))
        .div_ceil(image.width() as u64) as u32;
    let natural_rows = scaled_h_px.div_ceil(cell_h_px) as u16;

    // Tier 1: max(2/3 pane, legacy 60) — preserves today's UX for medium panes,
    // grows past 60 only on big panes.
    let two_thirds = (pane_height_rows as u32 * 2 / 3) as u16;
    let target_cap = two_thirds.max(INLINE_LEGACY_CAP);

    // Tier 2: enforce trailing context + hard upper bound.
    let max_inline = pane_height_rows.saturating_sub(INLINE_TRAILING_CONTEXT);
    let soft_cap = target_cap.min(max_inline).min(INLINE_HARD_CAP);

    // Tier 3: floor must not exceed soft_cap (degrades cleanly on tiny panes).
    let effective_floor = INLINE_FLOOR_ROWS.min(soft_cap);

    natural_rows.clamp(effective_floor, soft_cap.max(effective_floor))
}
```

**Behaviour table** (no regression for `pane_h ≥ 64`; small regression for `pane_h ∈ [60, 63]`):

| pane_h | natural | today | new | note |
|---|---|---|---|---|
| 80 | 70 | 60 | **60** | legacy floor preserved |
| 80 | 40 | 40 | 40 | unchanged |
| 120 | 80 | 60 | **80** | grows past 60 |
| 200 | 150 | 60 | **100** | hard cap |
| 64 | 70 | 60 | **60** | last pane_h with no regression (max_inline=60) |
| 60 | 70 | 60 | **56** | trailing context wins (4-row regression — see below) |
| 12 | 30 | 6 | 8 | floor |
| 8 | 30 | 6 | 4 | floor degrades |
| 4 | 30 | 6 | 0 | no inline |
| 0 | * | 6 | 0 | early exit |

**Honest claim (claude-code review catch):** **No regression for `pane_h ≥ 64`.** For `pane_h ∈ [60, 63]`, the trailing-context constraint (`max_inline = pane − 4`) takes precedence over the legacy-60 floor, producing a small (≤4-row) regression — at `pane_h = 60`, today's 60-row diagram becomes 56 rows. We accept this trade because:

1. Pane heights of exactly 60–63 are rare (most terminals are ≥ 24 cols × ≥ 24 rows; trace pane is typically ≥ 70 rows on a working session).
2. The alternative (drop trailing-context when `target_cap == LEGACY_CAP`) would broadly disable the `pane − 4` context for **any pane_h < 90** because `target_cap` saturates at 60 whenever `2/3·pane < 60`. Codex called this "dangerous because it ignores `pane_h - 4` broadly, not just at `pane_h=60`."
3. With the larger raster (Section 3.1), 56 sharp rows beat 60 soft rows.

**Design rationale (codex review):** The `max(two_thirds, 60)` floor was added after the first-round worker review pointed out that 80×70 going 60 → 53 was a real UX downgrade — crisper raster does not compensate for fewer cells of physical footprint.

**Signature change (`Option<&Picker>` → `(cell_w_px, cell_h_px)`):** the function becomes pure math, fully testable without a real Picker. Caller computes the tuple once at the top of the render path:

```rust
let (cell_w_px, cell_h_px) = picker
    .map(|p| { let (w, h) = p.font_size(); (w.max(1) as u32, h.max(1) as u32) })
    .unwrap_or((8, 16));
```

### 3.3 Multi-Row Partial-Visibility Card

```rust
// components/react_trace/render.rs
const PARTIAL_CARD_MIN_ROWS: u16 = 3;

fn render_partial_card(
    frame: &mut Frame,
    rect: Rect,
    id: MermaidId,
    total_rows: u16,
    first_row_within: u16,
    run_len: u16,
) {
    if run_len == 0 { return; }

    let visible_pct = if total_rows == 0 { 100 }
        else { ((run_len as u32 * 100) / (total_rows as u32)).min(100) as u16 };

    let direction = match (
        first_row_within == 0,
        first_row_within.saturating_add(run_len) >= total_rows,
    ) {
        (true,  false) => "▼ scroll down",
        (false, true)  => "▲ scroll up",
        _              => "▲▼ scroll for more",
    };

    let lines: Vec<Line<'static>> = match run_len {
        1 => vec![Line::from(Span::styled(
            format!("[📊 mermaid #{} · {}% · {}]", id.0, visible_pct, direction),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ))],
        2 => vec![
            Line::from(Span::styled(
                format!("📊 mermaid #{} · {}% visible · {}", id.0, visible_pct, direction),
                Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Alt-v · open in full viewer",
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            )),
        ],
        _ /* run_len ≥ 3 */ => vec![
            Line::from(Span::styled(
                format!("📊 mermaid #{} · {}% visible · {}", id.0, visible_pct, direction),
                Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Alt-v · open in full viewer",
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            )),
        ],
    };

    // Vertically centre the card when run_len > card_height — avoids
    // leaving a tiny card stranded at the top of a 30-row blank rect.
    let card_height = lines.len() as u16;
    let card_rect = if card_height < run_len {
        let pad_top = (run_len - card_height) / 2;
        Rect { x: rect.x, y: rect.y + pad_top, width: rect.width, height: card_height }
    } else {
        rect
    };
    frame.render_widget(Paragraph::new(lines), card_rect);
}
```

**Partial-visibility branch rewire** in `render_with_ctx` (replaces existing `render.rs:581+`):

```rust
let fully_visible = first_row_within == 0 && run_len == total_rows;
let drew_image = if fully_visible {
    render_inline_image(frame, rect, id, ctx)   // uses ImageCache
} else { false };

if !drew_image {
    if matches!(ctx.mermaid_registry.get(&id),
                Some(MermaidState::Ready { .. })) {
        render_partial_card(frame, rect, id, total_rows, first_row_within, run_len);
    } else {
        // Pending / Rendering / Error — single-line placeholder, dim.
        // (Today's behaviour for non-Ready cases preserved.)
    }
}
```

### 3.4 Cache-Key Extension (`soft_cap` + cell metrics, not raw `pane_height`)

```rust
pub struct VirtualRowCacheEntry {
    pub rows: Vec<VirtualRow>,
    pub entry_row_starts: Vec<usize>,
    pub byte_ranges: Vec<Option<(usize, usize)>>,
    pub width: u16,
    pub soft_cap: u16,                  // derived from pane_height
    pub cell_w_px: u32,                 // gemini/kimi review catch — see below
    pub cell_h_px: u32,
    pub generation: u64,
    pub fence_gen: u64,
}
```

Cache hit when `(width, soft_cap, cell_w_px, cell_h_px, fence_gen)` all match. `effective_soft_cap` is computed once per render from `pane_height_rows` and threaded through `build_virtual_rows`. **Both the full-cache and incremental-cache validity paths must compare all five fields.**

**Why soft_cap not raw pane_height:** soft_cap is the only pane-height-derived value that affects row layout. Most pane resizes (workers panel collapse ±5 rows, terminal flicker ±1 row) don't change soft_cap because `target_cap = max(two_thirds, 60)` saturates at 60 across pane_h ∈ [12, 90]. Cache mostly hits.

**Why cell metrics in the key (gemini/kimi catch):** `compute_inline_height_rows` depends on `(cell_w_px, cell_h_px)` directly — `cell_w_px` scales `pane_width_px → scaled_h_px`; `cell_h_px` divides `scaled_h_px → natural_rows`. A terminal font swap that changes cell metrics without changing cols/rows leaves `width` and `soft_cap` identical but changes `natural_rows`. Without cell metrics in the key, the row layout cache would silently false-hit and retain stale row heights.

| pane_h transition | raw `pane_height` key | `(soft_cap, cell_metrics)` key |
|---|---|---|
| 80 → 75 (panel collapse) | rebuild | hit (both = 60, cell unchanged) |
| 80 → 95 (split) | rebuild | rebuild (60 → 63) |
| 200 → 220 | rebuild | hit (both at hard cap 100) |
| Terminal font swap (no SIGWINCH) | hit (false!) | rebuild (cell metrics drift) |

### 3.5 `image_cache` Module

```rust
// components/image_cache.rs
struct CachedProtocol {
    proto: StatefulProtocol,
    image_generation: u64,              // monotonic counter snapshot — bumped by
                                        // SessionDetailView on every accepted Ok
                                        // completion. Robust against allocator-reuse
                                        // ABA AND same-bucket Error→Ready replays.
}

#[derive(Default)]
pub struct ImageCache {
    inline:  HashMap<MermaidId, CachedProtocol>,
    overlay: HashMap<MermaidId, CachedProtocol>,
    last_cell_size: Option<(u16, u16)>,
}

impl ImageCache {
    pub fn new() -> Self { Self::default() }

    pub fn inline_protocol_mut(&mut self, id: MermaidId,
        image: &Arc<DynamicImage>, image_generation: u64,
        picker: &Picker) -> &mut StatefulProtocol;
    pub fn overlay_protocol_mut(&mut self, id: MermaidId,
        image: &Arc<DynamicImage>, image_generation: u64,
        picker: &Picker) -> &mut StatefulProtocol;

    pub fn invalidate_all(&mut self);
    pub fn invalidate_id(&mut self, id: MermaidId);

    #[cfg(any(test, debug_assertions))]
    pub fn len(&self) -> (usize, usize) { (self.inline.len(), self.overlay.len()) }

    #[cfg(test)]
    pub fn check_cell_size_with(&mut self, cur: (u16, u16));   // test-only injection
}
```

Get-or-build is 3-armed (`Entry::Occupied` hit / `Occupied` stale / `Vacant`):

```rust
fn get_or_build<'a>(map: &'a mut HashMap<MermaidId, CachedProtocol>,
                   id: MermaidId, image: &Arc<DynamicImage>,
                   image_generation: u64, picker: &Picker)
    -> &'a mut StatefulProtocol
{
    match map.entry(id) {
        Entry::Occupied(o) if o.get().image_generation == image_generation => {
            &mut o.into_mut().proto
        }
        Entry::Occupied(mut o) => {
            // Generation drift — Ready was replaced (re-raster, retry, etc).
            // Rebuild protocol from the new image.
            *o.get_mut() = CachedProtocol {
                proto: picker.new_resize_protocol((**image).clone()),
                image_generation,
            };
            &mut o.into_mut().proto
        }
        Entry::Vacant(v) => {
            &mut v.insert(CachedProtocol {
                proto: picker.new_resize_protocol((**image).clone()),
                image_generation,
            }).proto
        }
    }
}
```

**Why `image_generation: u64` not `Arc::as_ptr` (codex Rust-expert review catch):** the original spec used `Arc::as_ptr(image) as usize` as the identity tag. That's not memory-unsafe (the `usize` is never dereferenced), but it's not a structural cache guarantee either. Two failure modes:

1. **Allocator reuse (gemini ABA finding):** when the old `Arc<DynamicImage>` is dropped and the new one allocated for the same diagram, the global allocator can reuse the same heap address. `image_addr` matches → false hit → renders stale pixels.
2. **Same-bucket replay (codex Rust-expert finding):** any path that produces a fresh `Arc<DynamicImage>` at the *same* `rastered_at_bucket` (Error→Ready retry, duplicate completion, future "retry diagram" feature) would also false-hit if the cache snapshotted bucket instead of address.

`image_generation: u64` is monotonically incremented by `SessionDetailView` on every accepted `Ok` completion for any `MermaidId`. Same id + same generation = same image. Different generation = rebuild. Eliminates both failure modes structurally.

**Why two slots per id, not one:** ratatui-image caches the encoded payload but switching between a small Rect (inline) and a large Rect (overlay) recomputes footprint metadata each frame. Two slots avoids re-encode on view switch. Per-id memory overhead: a few KB.

**Why generation-based auto-invalidation is structural:** the natural call site for explicit invalidation is `handle_mermaid_completed` (state-transition handler), but the cache is read in `render_inline_image` (render path). Without auto-detection, a future maintainer adding a path that mutates `Ready.image` without bumping `image_generation` would silently break. The cache compares the snapshotted generation on every fetch — a generation drift forces rebuild. Robust by construction, not by discipline.

## API / Data Shape Changes (Consolidated)

| Item | Before | After |
|---|---|---|
| `MermaidState::Ready` | `{ image: Arc<DynamicImage>, inline_protocol: RefCell<Option<StatefulProtocol>> }` | `{ image: Arc<DynamicImage>, code: String, rastered_at_bucket: u32, image_generation: u64 }` |
| `render_mermaid` | `(code: &str) -> Result<DynamicImage, _>` | `(code: &str, target_width: u32) -> Result<DynamicImage, _>` |
| `Action::MermaidRenderRequest` | `{ session, ref_id, code }` | `{ session, ref_id, code, target_width: u32 }` |
| `Action::MermaidRenderCompleted` | `{ session, ref_id, result }` | `{ session, ref_id, target_width: u32, result }` |
| `compute_inline_height_rows` | `(image, pane_width_cols, picker: Option<&Picker>) -> u16` | `(image, pane_width_cols, pane_height_rows, cell_w_px: u32, cell_h_px: u32) -> u16` |
| `RenderContext` | `{ mermaid_registry, picker }` | `{ mermaid_registry, picker, image_cache: &'a mut ImageCache }` |
| `VirtualRowCacheEntry` | `{ rows, entry_row_starts, byte_ranges, width, generation, fence_gen }` | `{ ..., soft_cap: u16, cell_w_px: u32, cell_h_px: u32 }` (added) |
| `MermaidViewerView` | owns `protocol: Option<StatefulProtocol>` | owns `focused: Option<MermaidId>` only; protocol comes from `ImageCache` |
| `SessionDetailView` | (existing fields) | + `image_cache: ImageCache`, + `in_flight_renders: HashSet<MermaidId>`, + `next_image_generation: u64` |
| `SessionDetailView::handle_mermaid_completed` | `(ref_id, result)` | `(ref_id, target_width, result)` — bumps `next_image_generation` on `Ok` and stores it in new `Ready` |
| `ImageCache::*_protocol_mut` | (would have been) `(id, image, picker)` | `(id, image, image_generation: u64, picker)` — generation drift triggers rebuild |
| `SessionDetailView::invalidate_inline_protocols` | (method on view) | `image_cache.invalidate_all()` |
| New: `SessionDetailView::render_overlay` | — | `(&mut self, frame, area)` — encapsulates borrow split |
| New: `SessionDetailView::maybe_request_rerasters` | — | `(&mut self, pane_cols, cell_w_px)` — bucket-up emit |
| New constants | — | `RASTER_BUCKETS`, `INLINE_FLOOR_ROWS = 8`, `INLINE_LEGACY_CAP = 60`, `INLINE_HARD_CAP = 100`, `INLINE_TRAILING_CONTEXT = 4`, `PARTIAL_CARD_MIN_ROWS = 3` |

## Testing Strategy

### Automated tests (37 total)

**`components/mermaid.rs#tests` — bucket function (5):**
- `bucket_zero_returns_smallest`
- `bucket_below_smallest_returns_smallest`
- `bucket_exact_match_returns_match`
- `bucket_just_above_returns_next`
- `bucket_at_3200_match` *(NEW — 6th bucket)*
- `bucket_above_largest_caps_at_3200`

**`components/react_trace/render.rs#tests` — height + card (21):**

Height formula (11):
- `height_no_regression_at_pane_80_natural_70` *(80/70 → 60)*
- `height_grows_past_60_on_big_pane` *(120/80 → 80)*
- `height_caps_at_hard_100` *(200/150 → 100)*
- `height_floor_degrades_on_tiny_pane` *(12/30 → 8)*
- `height_floor_below_4_minus_trailing` *(8/30 → 4)*
- `height_zero_pane_returns_zero`
- `height_zero_image_returns_zero`
- `height_preserves_trailing_context`
- `height_preserves_legacy_60_at_medium_pane` *(80/40 → 40)*
- `height_two_thirds_active_when_above_60` *(100/100 → 66)*
- `height_pane_60_70_regresses_to_56` *(NEW — pins the documented small regression at pane_h=60..63)*

Partial card (10):
- `card_top_visible_says_scroll_down`
- `card_bottom_visible_says_scroll_up`
- `card_mid_window_says_scroll_for_more`
- `card_visible_pct_at_50`
- `card_visible_pct_total_zero_returns_100`
- `card_one_line_variant_when_run_len_1`
- `card_two_line_variant_when_run_len_2`
- `card_three_line_variant_when_run_len_3_or_more`
- `card_early_returns_when_run_len_0`
- `card_centers_when_run_len_exceeds_card_height`

**`components/image_cache.rs#tests` — cache lifecycle (8):**
- `empty_cache_lengths_are_zero`
- `inline_and_overlay_are_independent`
- `bucket_drift_rebuilds_in_place` *(renamed from `arc_identity_drift_rebuilds_in_place` — covers re-raster path via image_generation drift)*
- `same_bucket_generation_rebuilds_protocol` *(NEW — explicit Error→Ready→same-bucket Ready replay; codex Rust-expert catch)*
- `cell_size_drift_clears_both_maps` *(uses `check_cell_size_with`)*
- `invalidate_all_clears_both_and_resets_size`
- `invalidate_id_only_affects_one_id`
- `repeat_protocol_fetch_is_o1_no_rebuild`

**`components/react_trace/mod.rs#tests` — cache key (5):**
- `cache_hit_when_soft_cap_unchanged`
- `cache_miss_when_soft_cap_changes`
- `cache_miss_when_width_changes`
- `cache_miss_when_fence_gen_changes`
- `cache_miss_when_cell_metric_changes` *(NEW — cols/soft_cap/fence_gen identical, cell_w_px or cell_h_px differs → miss; gemini/kimi catch)*

**`views/session_detail.rs#tests` — re-raster + retention (11):**

Re-raster trigger (6):
- `maybe_request_rerasters_skips_when_bucket_unchanged`
- `maybe_request_rerasters_emits_for_lower_bucketed_ready`
- `maybe_request_rerasters_skips_pending`
- `maybe_request_rerasters_skips_in_flight`
- `maybe_request_rerasters_skips_just_landed_at_new_bucket`
- `rerasters_coalesce_during_in_flight` *(NEW — emit at 1200, fire second emit at 2000 before completion → only one request in `pending_fence_actions`; claude-code catch)*

Completion handler + initial dispatch (5):
- `handle_completed_clears_in_flight`
- `handle_completed_records_target_width_on_ready`
- `handle_completed_retains_code_on_ready_to_ready`
- `handle_completed_retains_code_on_pending_to_ready`
- `handle_completed_bumps_image_generation_on_ok` *(NEW — pins I-A3)*
- `handle_completed_never_decreases_bucket` *(NEW — pins I-R1; claude-code catch)*
- `fence_emit_uses_current_bucket` *(NEW — fresh fence emit at session_detail.rs:1617/1792 with pane_w_px > 1200 → target_width ≥ 1200; pins I-R4)*

**Integration smoke (1):**
- `bucket_up_smoke_test` — `Picker::halfblocks()`, single Ready diagram, simulate pane grow, assert request emitted, completion handler runs, registry shows new bucket + bumped image_generation, image_cache hits with rebuilt protocol.

### Manual verification (4 items)

1. **iTerm2 retina, 200-col resize after a tall mermaid.**
   Submit a `flowchart TD` with ≥6 nodes. Wait for Ready. Resize terminal 80 → 200 cols. Watch the trace logs (`tail -f .spur/logs/spur-tui.log`).
   ✓ Image stays displayed continuously — no Pending placeholder appears between resize and re-render.
   ✓ Trace log shows `MermaidRenderRequest { ..., target_width: <higher bucket> }` emitted within the same frame as resize.
   ✓ Trace log shows `MermaidRenderCompleted` arriving with the new bucket; subsequent render rebuilds protocol (`image_cache rebuild`).

2. **Scroll past a tall mermaid.**
   Append ~50 lines of text after a tall diagram. Press Down until partially visible.
   ✓ Reserved height for the image area stays constant (compare row position of subsequent text before/after scroll into partial).
   ✓ Card displays expected direction label: `▼ scroll down` if `first_row_within == 0`, `▲ scroll up` if bottom-edge is at total_rows.
   ✓ Card text reads `📊 mermaid #N · X% visible · <direction>` where X = `(run_len * 100) / total_rows`.

3. **Workers panel collapse during render.**
   Session with 5+ Ready diagrams. Toggle workers panel.
   ✓ Trace pane height changes.
   ✓ No Pending placeholder appears between toggle and post-toggle image (the structural property; "no flicker" pinned to this).
   ✓ Diagrams render at correct scale post-toggle (visual: text inside diagram boxes is legible; no aspect-ratio distortion).

4. **Alt-v overlay round-trip.**
   Open Alt-v overlay on a Ready diagram. Press q. Re-open.
   ✓ `image_cache.len()` overlay slot is non-empty after first open (instrument via debug-only log).
   ✓ Re-open shows identical pixels (visual hash check via screenshot if practical).

## Locked Invariants

| ID | Invariant |
|---|---|
| **I-A1** | `MermaidState::Ready` owns pixels (`Arc<DynamicImage>`) + provenance (`code`, `rastered_at_bucket`, `image_generation`); never owns rendering protocol. |
| **I-A2** | `ImageCache` has 2 independent slots per `MermaidId` (inline, overlay). |
| **I-A3** | `ImageCache` auto-invalidates on `image_generation` drift (per-id) and on `picker.font_size()` drift (whole cache). Generation-based identity is robust to allocator reuse AND same-bucket Error→Ready replays. |
| **I-R1** | `rastered_at_bucket` per Ready is monotone non-decreasing. |
| **I-R2** | `in_flight_renders.len() ≤ mermaid_registry.len()`; insert paired with `handle_mermaid_completed` removal (success or error). |
| **I-R3** | Stale image displayed during re-raster (no Pending flicker). |
| **I-R4** | New fences dispatched at `raster_width_for_pane(current pane)`, never always-800. |
| **I-H1** | `effective_floor = INLINE_FLOOR_ROWS.min(soft_cap)`. Floor degrades cleanly on tiny panes. |
| **I-H2** | `target_cap = max(2/3 pane, INLINE_LEGACY_CAP=60)`. No regression for `pane_h ≥ 64`; for `pane_h ∈ [60, 63]` trailing-context wins (≤4-row regression accepted, see §3.2). |
| **I-H3** | `soft_cap ≤ INLINE_HARD_CAP = 100`. Pathological diagrams never monopolise huge panes. |
| **I-H4** | `soft_cap ≤ pane_h − INLINE_TRAILING_CONTEXT = 4`. At least 4 rows of trace context preserved below diagram. |
| **I-H5** | `VirtualRowCacheEntry` cache key quintuple: `(width, soft_cap, cell_w_px, cell_h_px, fence_gen)`. Cell metrics included so font swaps without SIGWINCH still invalidate. |
| **I-H6** | `render_partial_card` vertically centres card when `card_height < run_len`. |
| **I-H7** | Direction labels combine arrow + word (`▼ scroll down`, not just `▼`). |

## Out of Scope (v3)

- **True smooth-crop.** Render the actual visible portion of the image's pixels via a custom widget around the iTerm2/Kitty source-rect protocol. `ratatui-image` 9.0.0's `Resize::Crop` is corner-clip only.
- **LRU / byte-cap eviction.** Long sessions accumulate diagrams unbounded (matches today's behaviour). Add `cap_bytes` config knob when memory pressure surfaces in the wild.
- **Picker font-size heartbeat.** Re-query `font_size()` on a periodic basis to catch terminal font swaps that don't fire SIGWINCH. Currently relies on `Event::Resize` (covers most cases).
- **`fence_state_hash` aspect-ratio classes.** Today the hash includes `image.width()` + `image.height()`, so re-raster always invalidates the cache even though row layout is identical (aspect ratio preserved). Cleanup: hash an aspect-equivalent class instead.
- **Overlay zoom / pan / fit-mode toggles.** Codex flagged that the inline placeholder hint `Alt-v to zoom` is misleading because the overlay has no zoom. v3 adds `+/-` zoom, `0` reset, `f` fit-screen, `w` fit-width, arrow / hjkl pan, and a status footer showing `3/7 · 125% · iTerm2 · 1600×900`.
- **Cancel-in-flight on bucket-up race.** Currently a result lands at older bucket then triggers a fresh re-raster. v3 could cancel.
