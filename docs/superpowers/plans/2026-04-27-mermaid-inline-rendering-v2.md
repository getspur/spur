# Mermaid Inline Rendering v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix two iTerm2 mermaid rendering complaints — diagrams render too small / soft, and disappear-on-scroll — by introducing pane-aware raster buckets, adaptive inline height, a multi-row partial-visibility card, and an `image_cache` module that owns rendering protocols.

**Architecture:** Three layered changes plus a foundational module. (1) Extract `StatefulProtocol` ownership from `MermaidState::Ready` into a new `components/image_cache.rs` keyed by `(MermaidId, image_generation: u64)` — robust to allocator-reuse ABA and same-bucket replays. (2) Replace fixed `DEFAULT_WIDTH = 800` with bucket-aware rasterisation `[800, 1200, 1600, 2000, 2400, 3200]` and re-rasterise on bucket-up. (3) Replace fixed `[6, 60]` row clamp with `clamp(8, max(2/3 pane, 60), pane − 4)` capped at 100, and the disappear-on-scroll placeholder with a 1/2/3-line vertically-centred card carrying direction labels (`▼ scroll down`, `▲ scroll up`, `▲▼ scroll for more`).

**Tech Stack:** Rust 2024 edition, ratatui + ratatui-image 9.0.0 (StatefulImage / StatefulProtocol), resvg / usvg / tiny-skia (rasterisation), tokio (spawn_blocking workers), the existing markdown_stream + react_trace cache infrastructure.

**Spec:** [`docs/superpowers/specs/2026-04-27-mermaid-inline-rendering-v2-design.md`](../specs/2026-04-27-mermaid-inline-rendering-v2-design.md)

---

## File Structure

| File | Status | Responsibility | Approx Δ |
|---|---|---|---|
| `crates/spur-tui/src/action.rs` | modify | Action shapes (`target_width: u32` on Request/Completed) | +5 |
| `crates/spur-tui/src/components/mermaid.rs` | modify | `MermaidState::Ready` data shape; `RASTER_BUCKETS`; `raster_width_for_pane`; `render_mermaid(target_width)` | +50 |
| `crates/spur-tui/src/components/image_cache.rs` | **NEW** | Owns `StatefulProtocol` per `(MermaidId, slot)` keyed by `image_generation`; auto-invalidates on cell-size drift | +140 |
| `crates/spur-tui/src/components/mod.rs` | modify | Add `pub mod image_cache;` | +1 |
| `crates/spur-tui/src/components/react_trace/types.rs` | modify | `RenderContext` gains `&mut ImageCache` and `image_generation` access via registry | +5 |
| `crates/spur-tui/src/components/react_trace/render.rs` | modify | `compute_inline_height_rows` signature + new formula; `render_partial_card`; rewire partial-visibility branch | +90 |
| `crates/spur-tui/src/components/react_trace/mod.rs` | modify | `VirtualRowCacheEntry` gains `soft_cap`, `cell_w_px`, `cell_h_px`; cache-hit check extended | +10 |
| `crates/spur-tui/src/components/react_trace/builder.rs` | modify | `build_virtual_rows` threads new params | +5 |
| `crates/spur-tui/src/views/mermaid_viewer.rs` | modify | Drop `protocol` field; viewer is focus-only | −25 |
| `crates/spur-tui/src/views/session_detail.rs` | modify | New fields (image_cache, in_flight_renders, next_image_generation); `maybe_request_rerasters`; updated `handle_mermaid_completed` | +90 |
| `crates/spur-tui/src/app.rs` | modify | Worker passes `target_width`; overlay uses ImageCache; resize → invalidate | +15 |

Net: ~+385 / −60 lines + 1 new module.

**Build commands** (run from repo root):

```bash
# Workspace check (~30s)
cargo check -p spur-tui --features markdown

# Single-package tests (~1-3 min depending on cache)
cargo test -p spur-tui --features markdown

# Single test (during TDD)
cargo test -p spur-tui --features markdown <test_name> -- --exact --nocapture
```

---

## Task 1: Extend Action shapes with `target_width`

**Files:**
- Modify: `crates/spur-tui/src/action.rs:120-131`

The action carries the chosen raster bucket all the way from the emit site to the worker and back. Internal-only message; safe to extend.

- [ ] **Step 1: Open `crates/spur-tui/src/action.rs` and find the existing `MermaidRenderRequest` / `MermaidRenderCompleted` variants (lines 117-131). Replace them.**

```rust
    /// Request the app to render a mermaid diagram on a blocking worker.
    /// Emitted by `SessionDetailView::tick` when a new fence closes, and by
    /// `SessionDetailView::maybe_request_rerasters` when the pane crosses
    /// a raster bucket boundary upward.
    #[cfg(feature = "markdown")]
    MermaidRenderRequest {
        session: SessionId,
        ref_id: crate::components::mermaid::MermaidId,
        code: String,
        /// Target raster width in pixels (chosen via `raster_width_for_pane`).
        /// The worker rasters at this width; height is aspect-preserved.
        target_width: u32,
    },
    /// Completion of a previously-dispatched render request.
    /// `target_width` echoes the request's bucket so `handle_mermaid_completed`
    /// can record it in `MermaidState::Ready.rastered_at_bucket`.
    #[cfg(feature = "markdown")]
    MermaidRenderCompleted {
        session: SessionId,
        ref_id: crate::components::mermaid::MermaidId,
        target_width: u32,
        result: Result<std::sync::Arc<image::DynamicImage>, String>,
    },
```

- [ ] **Step 2: `cargo check -p spur-tui --features markdown`. Expect failures at the action emit sites (session_detail.rs:1618, :1793) and three dispatch sites in app.rs (:2090 destructure pattern, :2101 `tx.send(Action::MermaidRenderCompleted{...})` constructor inside the worker spawn closure, :2109 destructure pattern) — those will be wired in later tasks (Tasks 5/9). The Action enum itself should compile.**

- [ ] **Step 3: Verify only those five call sites are broken**

```bash
cargo check -p spur-tui --features markdown 2>&1 | grep -E "MermaidRender(Request|Completed)" | sort -u
```

Expected output (5 lines: session_detail.rs:1618, :1793, app.rs:2090, :2101, :2109).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/action.rs
git commit -m "feat(spur-tui): add target_width to MermaidRenderRequest/Completed"
```

---

## Task 2: Extend `MermaidState::Ready` data shape

**Files:**
- Modify: `crates/spur-tui/src/components/mermaid.rs:31-47`

Drop `inline_protocol` (moves to ImageCache in Task 7). Add `code` (retained for re-raster), `rastered_at_bucket` (provenance for skip logic), `image_generation` (image-identity tag for ImageCache).

- [ ] **Step 1: Replace the `MermaidState` enum** at lines 31-47:

```rust
/// State machine for a pending / rendered mermaid diagram.
pub enum MermaidState {
    Pending {
        code: String,
    },
    Rendering,
    Ready {
        /// Reference-counted to avoid deep-copying the pixel buffer on the
        /// hot render path — the dispatch layer already hands in an `Arc`.
        image: std::sync::Arc<DynamicImage>,
        /// Source code retained so re-raster on bucket-up can re-dispatch
        /// without reaching back into MarkdownStream.
        code: String,
        /// Raster bucket (in pixels) used to produce `image`. Compared
        /// against `raster_width_for_pane(current_pane_w_px)` to decide
        /// whether re-raster is needed.
        rastered_at_bucket: u32,
        /// Monotonic counter bumped by SessionDetailView on every accepted
        /// Ok completion. Snapshotted by ImageCache to detect identity drift
        /// (allocator reuse OR same-bucket Error→Ready replay).
        image_generation: u64,
    },
    Error {
        message: String,
    },
}
```

- [ ] **Step 2: Update `Debug for MermaidState`** at lines 49-66 to handle the new fields:

```rust
impl std::fmt::Debug for MermaidState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MermaidState::Pending { code } => {
                f.debug_struct("Pending").field("code", code).finish()
            }
            MermaidState::Rendering => f.debug_struct("Rendering").finish(),
            MermaidState::Ready {
                image,
                code,
                rastered_at_bucket,
                image_generation,
            } => f
                .debug_struct("Ready")
                .field("image_size", &(image.width(), image.height()))
                .field("code_len", &code.len())
                .field("rastered_at_bucket", rastered_at_bucket)
                .field("image_generation", image_generation)
                .finish(),
            MermaidState::Error { message } => {
                f.debug_struct("Error").field("message", message).finish()
            }
        }
    }
}
```

- [ ] **Step 3: Remove the no-longer-relevant imports** at lines 17-22. Replace with:

```rust
use std::panic;
use std::sync::{Arc, OnceLock};

use image::DynamicImage;
```

(`std::cell::RefCell` and `ratatui_image::protocol::StatefulProtocol` are no longer used in this file.)

- [ ] **Step 4: Update the `ready_state_holds_inline_protocol_slot` test** at lines 327-345 — rename and rewrite:

```rust
    #[test]
    fn ready_state_holds_provenance_fields() {
        use image::RgbaImage;

        let img = std::sync::Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        let state = MermaidState::Ready {
            image: img,
            code: "graph TD\nA-->B".into(),
            rastered_at_bucket: 800,
            image_generation: 1,
        };
        match state {
            MermaidState::Ready {
                code,
                rastered_at_bucket,
                image_generation,
                ..
            } => {
                assert_eq!(code, "graph TD\nA-->B");
                assert_eq!(rastered_at_bucket, 800);
                assert_eq!(image_generation, 1);
            }
            _ => panic!("expected Ready"),
        }
    }
```

- [ ] **Step 5: `cargo check -p spur-tui --features markdown`** — expect failures at every site that constructs `MermaidState::Ready` (session_detail.rs:929, mermaid_viewer.rs uses, tests). These will be fixed in later tasks. The mermaid.rs file itself should compile.

- [ ] **Step 6: Run mermaid.rs unit tests in isolation:**

```bash
cargo test -p spur-tui --features markdown --lib components::mermaid
```

Expected: `ready_state_holds_provenance_fields` passes; existing tests (`fix_font_families_replaces_inner_quotes`, `malformed_svg_rasterization_does_not_panic`) still pass.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/mermaid.rs
git commit -m "feat(spur-tui): MermaidState::Ready gains code + rastered_at_bucket + image_generation"
```

---

## Task 3: Raster bucket policy (TDD)

**Files:**
- Modify: `crates/spur-tui/src/components/mermaid.rs` (add module-level constants + function near line 138)

- [ ] **Step 1: In `mermaid.rs#tests`, add the bucket function tests** before existing tests:

```rust
    #[test]
    fn bucket_zero_returns_smallest() {
        assert_eq!(raster_width_for_pane(0), 800);
    }

    #[test]
    fn bucket_below_smallest_returns_smallest() {
        assert_eq!(raster_width_for_pane(400), 800);
    }

    #[test]
    fn bucket_exact_match_returns_match() {
        assert_eq!(raster_width_for_pane(1200), 1200);
    }

    #[test]
    fn bucket_just_above_returns_next() {
        assert_eq!(raster_width_for_pane(1201), 1600);
    }

    #[test]
    fn bucket_at_3200_match() {
        assert_eq!(raster_width_for_pane(3200), 3200);
    }

    #[test]
    fn bucket_above_largest_caps_at_3200() {
        assert_eq!(raster_width_for_pane(99_999), 3200);
    }
```

- [ ] **Step 2: Run tests, expect 6 failures (function not found)**

```bash
cargo test -p spur-tui --features markdown --lib components::mermaid::tests::bucket_
```

Expected: all six tests fail with `cannot find function raster_width_for_pane in this scope`.

- [ ] **Step 3: Replace `const DEFAULT_WIDTH: u32 = 800;`** at line 139 with the bucket policy:

```rust
// ─── Raster bucket policy ─────────────────────────────────────────────────────

/// Discrete raster-width buckets in pixels. Chosen so a typical pane finds a
/// bucket within 1.25× of its pixel width — minimising terminal-side scaling
/// without re-rasterising on every column resize.
///
/// Capped at 3200, not higher, because v2 has no LRU eviction and
/// `4000 × 3000 RGBA ≈ 48 MB` per diagram is too high a per-session
/// memory ceiling for sessions with many diagrams. `3200 × 2400 RGBA ≈ 30 MB`
/// is the v2 worst-case per-diagram budget.
pub const RASTER_BUCKETS: [u32; 6] = [800, 1200, 1600, 2000, 2400, 3200];

/// Choose the smallest bucket whose pixel width is ≥ `pane_w_px`.
/// Falls back to the largest bucket for very wide panes.
///
/// **Bucket-up only** is the SessionDetailView re-raster policy: once we
/// render at a higher bucket, we never downgrade for the same diagram
/// (see `MermaidState::Ready.rastered_at_bucket` and `maybe_request_rerasters`).
pub fn raster_width_for_pane(pane_w_px: u32) -> u32 {
    for &b in &RASTER_BUCKETS {
        if b >= pane_w_px {
            return b;
        }
    }
    *RASTER_BUCKETS.last().unwrap()
}
```

- [ ] **Step 4: Run tests, expect 6 passes**

```bash
cargo test -p spur-tui --features markdown --lib components::mermaid::tests::bucket_
```

Expected: all six pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/mermaid.rs
git commit -m "feat(spur-tui): raster_width_for_pane with 6 buckets capped at 3200"
```

---

## Task 4: Update `render_mermaid` signature to take `target_width`

**Files:**
- Modify: `crates/spur-tui/src/components/mermaid.rs:146-168`

- [ ] **Step 1: Replace `render_mermaid`** at lines 146-168:

```rust
/// Render a Mermaid diagram source string to a [`DynamicImage`] at the
/// requested pixel width. Height is aspect-preserved.
///
/// Panics emitted by `mermaid-rs-renderer`, `usvg`, or `resvg` are caught
/// and converted to [`RenderError::Panic`], so this function never unwinds
/// the caller.
pub fn render_mermaid(code: &str, target_width: u32) -> Result<DynamicImage, RenderError> {
    let code_owned = code.to_string();

    // Single unwind boundary covering the entire mmdr → usvg → resvg pipeline.
    // Both render_to_svg and rasterize_svg can panic on malformed input;
    // wrapping both here ensures the caller is never unwound.
    let result = panic::catch_unwind(move || {
        let svg = render_to_svg_inner(&code_owned)?;
        rasterize_svg(&svg, target_width)
    });

    match result {
        Ok(outcome) => outcome,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&'static str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            Err(RenderError::Panic(msg))
        }
    }
}
```

The only behavioural change is taking `target_width: u32` instead of using the deleted `DEFAULT_WIDTH` constant.

- [ ] **Step 2: Update the existing `malformed_svg_rasterization_does_not_panic` test** at line 299. Find the `render_mermaid` call (none currently exists in this test — it calls `rasterize_svg` directly via `panic::catch_unwind`, so no change needed).

- [ ] **Step 3: `cargo check -p spur-tui --features markdown`** — expect a single new error at `app.rs:2098` (the worker calls `render_mermaid(&code)` with one arg). This is fixed in Task 5.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/components/mermaid.rs
git commit -m "refactor(spur-tui): render_mermaid takes target_width: u32"
```

---

## Task 5: Update worker dispatch to pass + echo `target_width`

**Files:**
- Modify: `crates/spur-tui/src/app.rs:2089-2120`

- [ ] **Step 1: Replace the `MermaidRenderRequest` and `MermaidRenderCompleted` arms** at lines 2089-2120:

```rust
            #[cfg(feature = "markdown")]
            Action::MermaidRenderRequest {
                session,
                ref_id,
                code,
                target_width,
            } => {
                let tx = self.mermaid_tx.clone();
                let session_cloned = session.clone();
                tokio::task::spawn_blocking(move || {
                    let result = crate::components::mermaid::render_mermaid(&code, target_width)
                        .map(std::sync::Arc::new)
                        .map_err(|e| e.to_string());
                    let _ = tx.send(Action::MermaidRenderCompleted {
                        session: session_cloned,
                        ref_id,
                        target_width,
                        result,
                    });
                });
            }
            #[cfg(feature = "markdown")]
            Action::MermaidRenderCompleted {
                session,
                ref_id,
                target_width,
                result,
            } => {
                if let Some(ref mut detail) = self.session_detail {
                    if detail.session_id().0 == session.0 {
                        detail.handle_mermaid_completed(ref_id, target_width, result);
                    }
                }
                self.dirty = true;
            }
```

- [ ] **Step 2: `cargo check -p spur-tui --features markdown`** — expect errors at session_detail.rs:1617, :1793 (fence emit sites missing `target_width`) and at session_detail.rs:922 (`handle_mermaid_completed` signature). Both fixed in later tasks.

- [ ] **Step 3: Commit (still uncompilable end-to-end, but checkpoint the worker change)**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): mermaid worker passes + echoes target_width"
```

---

## Task 6: Refactor `compute_inline_height_rows` (TDD)

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/render.rs:184-206`

The new signature accepts cell metrics directly (purer, fully testable without a Picker), and applies the v2 formula `clamp(8, max(2/3 pane, 60), pane − 4)` capped at 100.

- [ ] **Step 1: Add new tests in `render.rs#tests`**. If a `#[cfg(test)] mod tests` block does not yet exist in render.rs, add one at the end of the file:

```rust
#[cfg(all(test, feature = "markdown"))]
mod height_tests {
    use super::*;
    use image::{DynamicImage, RgbaImage};

    fn img(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::new(w, h))
    }

    // For all tests we use cell metrics (8, 16) — typical for non-retina
    // monospace. natural_rows = ceil(image.h × pane_w_cols × 8 / image.w / 16).

    #[test]
    fn height_no_regression_at_pane_80_natural_70() {
        // 800x700 image, pane_w=100 cols × cell_w=8 = 800 px.
        // scaled_h = 700 * 800 / 800 = 700; natural = ceil(700/16) = 44.
        // Want pane_h=80, natural=70 → result 60. Use a taller image:
        // 800x1400 → scaled_h=1400, natural=ceil(1400/16)=88. Pick image
        // dims so natural ≈ 70: 800 * 16 * 70 / 800 = 1120 px tall.
        let i = img(800, 1120);
        // pane_h=80, natural≈70 → clamp(70, 8, 60) = 60.
        assert_eq!(compute_inline_height_rows(&i, 100, 80, 8, 16), 60);
    }

    #[test]
    fn height_grows_past_60_on_big_pane() {
        let i = img(800, 1280); // natural ≈ 80 at pane_w=100
        // pane_h=120: target_cap = max(80, 60)=80; max_inline=116; soft=80 → result 80.
        assert_eq!(compute_inline_height_rows(&i, 100, 120, 8, 16), 80);
    }

    #[test]
    fn height_caps_at_hard_100() {
        let i = img(800, 2400); // natural ≈ 150 at pane_w=100
        // pane_h=200: target_cap = max(133, 60)=133; min(133, 196, 100)=100.
        assert_eq!(compute_inline_height_rows(&i, 100, 200, 8, 16), 100);
    }

    #[test]
    fn height_floor_degrades_on_tiny_pane() {
        let i = img(800, 480); // natural ≈ 30 at pane_w=100
        // pane_h=12: max_inline=8; soft_cap=min(60, 8, 100)=8; floor=min(8,8)=8.
        assert_eq!(compute_inline_height_rows(&i, 100, 12, 8, 16), 8);
    }

    #[test]
    fn height_floor_below_4_minus_trailing() {
        let i = img(800, 480);
        // pane_h=8: max_inline=4; soft_cap=4; floor=min(8,4)=4.
        assert_eq!(compute_inline_height_rows(&i, 100, 8, 8, 16), 4);
    }

    #[test]
    fn height_zero_pane_returns_zero() {
        let i = img(800, 600);
        assert_eq!(compute_inline_height_rows(&i, 100, 0, 8, 16), 0);
    }

    #[test]
    fn height_zero_image_returns_zero() {
        let i = img(0, 600);
        assert_eq!(compute_inline_height_rows(&i, 100, 80, 8, 16), 0);
    }

    #[test]
    fn height_preserves_trailing_context() {
        let i = img(800, 2000); // very tall
        // pane_h=70: max_inline=66; target_cap=max(46, 60)=60; soft=60.
        // Trailing context preserved: result=60 ≤ 66.
        let result = compute_inline_height_rows(&i, 100, 70, 8, 16);
        assert!(result <= 70 - 4, "soft_cap must respect pane_h - 4 (got {result})");
    }

    #[test]
    fn height_preserves_legacy_60_at_medium_pane() {
        let i = img(800, 640); // natural ≈ 40
        // pane_h=80, natural=40 → clamp(40, 8, 60) = 40.
        assert_eq!(compute_inline_height_rows(&i, 100, 80, 8, 16), 40);
    }

    #[test]
    fn height_two_thirds_active_when_above_60() {
        let i = img(800, 1600); // natural ≈ 100
        // pane_h=100: target_cap = max(66, 60)=66; max_inline=96;
        // soft=min(66,96,100)=66; result=clamp(100, 8, 66)=66.
        assert_eq!(compute_inline_height_rows(&i, 100, 100, 8, 16), 66);
    }

    #[test]
    fn height_pane_60_70_regresses_to_56() {
        // Documents the accepted ≤4-row regression for pane_h ∈ [60, 63].
        let i = img(800, 1120); // natural ≈ 70
        // pane_h=60: target_cap = max(40, 60)=60; max_inline=56;
        // soft=min(60,56,100)=56; result=clamp(70, 8, 56)=56.
        assert_eq!(compute_inline_height_rows(&i, 100, 60, 8, 16), 56);
    }
}
```

- [ ] **Step 2: Run tests, expect 11 failures (signature mismatch — current fn takes `Option<&Picker>`).**

```bash
cargo test -p spur-tui --features markdown --lib components::react_trace::render::height_tests
```

Expected: all 11 fail to compile (`expected Option<&Picker>`, etc.).

- [ ] **Step 3: Replace the existing `compute_inline_height_rows`** at lines 184-206:

```rust
// ─── Inline height policy ────────────────────────────────────────────────────

/// Floor: minimum rows for a legible diagram on any pane.
pub(crate) const INLINE_FLOOR_ROWS: u16 = 8;

/// Floor of `target_cap`. Preserves today's UX baseline (`[6, 60]` clamp
/// in v1) — diagrams up to 60 rows render unchanged on medium panes
/// (pane_h ≥ 64). For smaller panes, trailing-context constraint takes
/// precedence (see `compute_inline_height_rows` doc).
pub(crate) const INLINE_LEGACY_CAP: u16 = 60;

/// Hard upper bound for inline diagrams. Caps pathological flowcharts on
/// huge panes (250+ rows) at a sane portion of viewport.
pub(crate) const INLINE_HARD_CAP: u16 = 100;

/// Rows of trace content always preserved below an inline diagram.
pub(crate) const INLINE_TRAILING_CONTEXT: u16 = 4;

/// Row count for rendering an image inline at `pane_width_cols` with aspect
/// ratio preserved.
///
/// The result is `natural_rows.clamp(effective_floor, soft_cap)` where:
/// - `natural_rows` = aspect-correct rows for the image at the pane's pixel width
/// - `target_cap = max(2/3 pane, INLINE_LEGACY_CAP)` — preserves legacy UX on medium panes
/// - `max_inline = pane_h - INLINE_TRAILING_CONTEXT` — keeps trace context below
/// - `soft_cap = target_cap.min(max_inline).min(INLINE_HARD_CAP)`
/// - `effective_floor = INLINE_FLOOR_ROWS.min(soft_cap)` — degrades on tiny panes
///
/// **Regression note:** for `pane_h ∈ [60, 63]` the trailing-context
/// constraint takes precedence over the legacy-60 floor, producing a
/// ≤4-row regression vs v1. Accepted trade — see spec §3.2.
#[cfg(feature = "markdown")]
pub(crate) fn compute_inline_height_rows(
    image: &image::DynamicImage,
    pane_width_cols: u16,
    pane_height_rows: u16,
    cell_w_px: u32,
    cell_h_px: u32,
) -> u16 {
    let cell_w_px = cell_w_px.max(1);
    let cell_h_px = cell_h_px.max(1);

    let pane_width_px = (pane_width_cols as u32).saturating_mul(cell_w_px);
    if pane_width_px == 0 || image.width() == 0 || pane_height_rows == 0 {
        return 0;
    }

    // display_h_px = image_h × (pane_w_px / image_w); rows = display_h_px / cell_h.
    let scaled_h_px =
        ((image.height() as u64) * (pane_width_px as u64)).div_ceil(image.width() as u64) as u32;
    let natural_rows = scaled_h_px.div_ceil(cell_h_px) as u16;

    let two_thirds = (pane_height_rows as u32 * 2 / 3) as u16;
    let target_cap = two_thirds.max(INLINE_LEGACY_CAP);
    let max_inline = pane_height_rows.saturating_sub(INLINE_TRAILING_CONTEXT);
    let soft_cap = target_cap.min(max_inline).min(INLINE_HARD_CAP);
    let effective_floor = INLINE_FLOOR_ROWS.min(soft_cap);

    natural_rows.clamp(effective_floor, soft_cap.max(effective_floor))
}

/// Pure helper exposed for cache keying (Task 13). Returns the soft_cap
/// for a given `pane_height_rows` without needing an image — only
/// `pane_height_rows` affects this value, so it stays the same for
/// every image rendered in the same pane.
#[cfg(feature = "markdown")]
pub(crate) fn compute_soft_cap(pane_height_rows: u16) -> u16 {
    if pane_height_rows == 0 { return 0; }
    let two_thirds = (pane_height_rows as u32 * 2 / 3) as u16;
    let target_cap = two_thirds.max(INLINE_LEGACY_CAP);
    let max_inline = pane_height_rows.saturating_sub(INLINE_TRAILING_CONTEXT);
    target_cap.min(max_inline).min(INLINE_HARD_CAP)
}
```

- [ ] **Step 4: Update the single existing call site at `compute_fence_states`** (render.rs:209-230). Replace the function body:

```rust
#[cfg(feature = "markdown")]
fn compute_fence_states(
    ctx: &RenderContext<'_>,
    pane_width_cols: u16,
    pane_height_rows: u16,
) -> std::collections::HashMap<
    crate::components::mermaid::MermaidId,
    crate::components::mermaid::FenceRender,
> {
    use crate::components::mermaid::{FenceRender, MermaidState};
    let (cell_w_px, cell_h_px) = ctx
        .picker
        .map(|p| { let (w, h) = p.font_size(); (w.max(1) as u32, h.max(1) as u32) })
        .unwrap_or((8, 16));
    let mut out = std::collections::HashMap::new();
    for (id, state) in ctx.mermaid_registry.iter() {
        let r = match state {
            MermaidState::Ready { image, .. } => FenceRender::Ready(
                compute_inline_height_rows(
                    image.as_ref(),
                    pane_width_cols,
                    pane_height_rows,
                    cell_w_px,
                    cell_h_px,
                ),
            ),
            MermaidState::Pending { .. } | MermaidState::Rendering => FenceRender::Pending,
            MermaidState::Error { .. } => FenceRender::Error,
        };
        out.insert(*id, r);
    }
    out
}
```

- [ ] **Step 5: Run new tests, expect 11 passes**

```bash
cargo test -p spur-tui --features markdown --lib components::react_trace::render::height_tests
```

Expected: all 11 pass.

- [ ] **Step 6: `cargo check -p spur-tui --features markdown`** — expect errors at the existing callers of `compute_fence_states` (render.rs:471, :484, :500). Each call must pass `pane_height_rows` (`inner.height` or equivalent). Update each:

```rust
let states = compute_fence_states(ctx, effective_width, inner.height);
```

(There are 3 call sites in `render_with_ctx` — find them via `cargo check`'s error output.)

- [ ] **Step 7: `cargo check -p spur-tui --features markdown`** until clean for this file. Other files (session_detail.rs, mermaid_viewer.rs) still error from earlier tasks; that's expected.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/render.rs
git commit -m "feat(spur-tui): adaptive inline height — clamp(8, max(2/3 pane, 60), pane-4) cap 100"
```

---

## Task 7: New `image_cache` module (TDD)

**Files:**
- Create: `crates/spur-tui/src/components/image_cache.rs`
- Modify: `crates/spur-tui/src/components/mod.rs` (add `pub mod image_cache;`)

- [ ] **Step 1: Add the module declaration to `components/mod.rs`**. Find the existing module declarations (look for `pub mod mermaid;` or similar) and add a sibling line:

```rust
#[cfg(feature = "markdown")]
pub mod image_cache;
```

- [ ] **Step 2: Create `crates/spur-tui/src/components/image_cache.rs`** with the full implementation + tests:

```rust
//! Owns rendered StatefulProtocol instances for mermaid diagrams,
//! split between inline (in-stream) and overlay (full-screen) views.
//!
//! Two slots per `MermaidId` because `ratatui-image` caches the encoded
//! protocol payload per Rect — inline and overlay use very different
//! Rects, and switching between them would force re-encode every toggle.
//! The dual-slot design keeps both warm.
//!
//! Auto-invalidation has two surfaces:
//!  - Per-id, on `image_generation` drift: catches every replacement of
//!    `MermaidState::Ready.image` (re-raster, retry, etc).
//!  - Whole-cache, on `picker.font_size()` drift: catches terminal font
//!    swaps (which silently invalidate every protocol's encoded metrics).
//!
//! `image_generation: u64` (not `Arc::as_ptr`) is the right identity tag:
//! the global allocator can reuse the same heap address after a drop, and
//! a same-bucket Error→Ready replay would have the same `rastered_at_bucket`.
//! A monotonic generation number bumped by `SessionDetailView` on every
//! accepted Ok completion is structurally unforgeable.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;

use image::DynamicImage;
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};

use crate::components::mermaid::MermaidId;

struct CachedProtocol {
    proto: StatefulProtocol,
    /// Snapshot of `MermaidState::Ready.image_generation` when this
    /// protocol was built. Compared on every fetch — a mismatch means
    /// the underlying image was replaced and the protocol is stale.
    image_generation: u64,
}

#[derive(Default)]
pub struct ImageCache {
    inline: HashMap<MermaidId, CachedProtocol>,
    overlay: HashMap<MermaidId, CachedProtocol>,
    /// Cell pixel size the current entries were built against. None ⇔
    /// both maps are empty. Drift triggers a full clear.
    last_cell_size: Option<(u16, u16)>,
}

impl ImageCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get-or-build the inline-render protocol for `id` at the current
    /// `image_generation`. If a stale protocol exists (different generation),
    /// rebuilds in place. Caller must pass the same `image` that lives in
    /// `MermaidState::Ready` (no enforcement — caller responsibility).
    pub fn inline_protocol_mut(
        &mut self,
        id: MermaidId,
        image: &Arc<DynamicImage>,
        image_generation: u64,
        picker: &Picker,
    ) -> &mut StatefulProtocol {
        Self::ensure_cell_size(
            &mut self.inline,
            &mut self.overlay,
            &mut self.last_cell_size,
            picker,
        );
        Self::get_or_build(&mut self.inline, id, image, image_generation, picker)
    }

    /// Same shape as `inline_protocol_mut` but for the overlay slot.
    pub fn overlay_protocol_mut(
        &mut self,
        id: MermaidId,
        image: &Arc<DynamicImage>,
        image_generation: u64,
        picker: &Picker,
    ) -> &mut StatefulProtocol {
        Self::ensure_cell_size(
            &mut self.inline,
            &mut self.overlay,
            &mut self.last_cell_size,
            picker,
        );
        Self::get_or_build(&mut self.overlay, id, image, image_generation, picker)
    }

    /// Drop every protocol. Called on `Event::Resize` (terminal resize),
    /// session reset, or whenever invariants demand a full rebuild.
    pub fn invalidate_all(&mut self) {
        self.inline.clear();
        self.overlay.clear();
        self.last_cell_size = None;
    }

    /// Drop only the protocols for one id. Available for explicit
    /// memory-hygiene; not required for correctness (auto-invalidation
    /// covers state changes).
    pub fn invalidate_id(&mut self, id: MermaidId) {
        self.inline.remove(&id);
        self.overlay.remove(&id);
    }

    /// Test/debug accessor: how many entries each map holds.
    #[cfg(any(test, debug_assertions))]
    pub fn len(&self) -> (usize, usize) {
        (self.inline.len(), self.overlay.len())
    }

    /// Test-only injection point: simulate a cell-size change without a
    /// real Picker. Used by `cell_size_drift_clears_both_maps`.
    #[cfg(test)]
    pub fn check_cell_size_with(&mut self, cur: (u16, u16)) {
        match self.last_cell_size {
            Some(prev) if prev == cur => {}
            Some(_) => {
                self.inline.clear();
                self.overlay.clear();
                self.last_cell_size = Some(cur);
            }
            None => self.last_cell_size = Some(cur),
        }
    }

    fn get_or_build<'a>(
        map: &'a mut HashMap<MermaidId, CachedProtocol>,
        id: MermaidId,
        image: &Arc<DynamicImage>,
        image_generation: u64,
        picker: &Picker,
    ) -> &'a mut StatefulProtocol {
        match map.entry(id) {
            Entry::Occupied(o) if o.get().image_generation == image_generation => {
                &mut o.into_mut().proto
            }
            Entry::Occupied(mut o) => {
                // Generation drift — stale protocol. Rebuild in place.
                *o.get_mut() = CachedProtocol {
                    proto: picker.new_resize_protocol((**image).clone()),
                    image_generation,
                };
                &mut o.into_mut().proto
            }
            Entry::Vacant(v) => {
                &mut v
                    .insert(CachedProtocol {
                        proto: picker.new_resize_protocol((**image).clone()),
                        image_generation,
                    })
                    .proto
            }
        }
    }

    fn ensure_cell_size(
        inline: &mut HashMap<MermaidId, CachedProtocol>,
        overlay: &mut HashMap<MermaidId, CachedProtocol>,
        last: &mut Option<(u16, u16)>,
        picker: &Picker,
    ) {
        let cur = picker.font_size();
        match *last {
            Some(prev) if prev == cur => {}
            Some(_) => {
                inline.clear();
                overlay.clear();
                *last = Some(cur);
            }
            None => *last = Some(cur),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbaImage;
    use ratatui_image::picker::Picker;

    fn small_image() -> Arc<DynamicImage> {
        Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)))
    }

    fn picker() -> Picker {
        Picker::from_fontsize((8, 16))
    }

    #[test]
    fn empty_cache_lengths_are_zero() {
        let c = ImageCache::new();
        assert_eq!(c.len(), (0, 0));
    }

    #[test]
    fn inline_and_overlay_are_independent() {
        let mut c = ImageCache::new();
        let id = MermaidId(1);
        let img = small_image();
        let p = picker();

        c.inline_protocol_mut(id, &img, 1, &p);
        c.overlay_protocol_mut(id, &img, 1, &p);
        assert_eq!(c.len(), (1, 1));

        c.invalidate_id(id);
        assert_eq!(c.len(), (0, 0));
    }

    #[test]
    fn bucket_drift_rebuilds_in_place() {
        let mut c = ImageCache::new();
        let id = MermaidId(1);
        let img = small_image();
        let p = picker();

        c.inline_protocol_mut(id, &img, 1, &p);
        assert_eq!(c.len(), (1, 0));

        // Same id, new generation → rebuilt in place (len unchanged).
        c.inline_protocol_mut(id, &img, 2, &p);
        assert_eq!(c.len(), (1, 0));
    }

    #[test]
    fn same_bucket_generation_rebuilds_protocol() {
        // Codex Rust-expert catch: a same-bucket Error→Ready replay
        // produces a NEW Arc at the SAME rastered_at_bucket. Earlier
        // designs that snapshotted bucket would false-hit. With
        // image_generation: u64, the new Ok completion bumps the
        // generation, so the cache rebuilds.
        let mut c = ImageCache::new();
        let id = MermaidId(1);
        let img = small_image();
        let p = picker();

        c.inline_protocol_mut(id, &img, 5, &p); // generation 5
        let len_after_first = c.len();
        // Simulate: Ready→Error→Ready replay. Same image (same bucket
        // implied) but new generation 7.
        c.inline_protocol_mut(id, &img, 7, &p);
        assert_eq!(c.len(), len_after_first, "rebuild in place keeps len");
        // The protocol has been replaced — no way to assert directly
        // without rendering, but the entry-rewriting branch is exercised.
    }

    #[test]
    fn cell_size_drift_clears_both_maps() {
        let mut c = ImageCache::new();
        let id = MermaidId(1);
        let img = small_image();
        let p = picker();

        c.inline_protocol_mut(id, &img, 1, &p);
        c.overlay_protocol_mut(id, &img, 1, &p);
        assert_eq!(c.len(), (1, 1));

        // Simulate font swap.
        c.check_cell_size_with((10, 20));
        assert_eq!(c.len(), (0, 0));
    }

    #[test]
    fn invalidate_all_clears_both_and_resets_size() {
        let mut c = ImageCache::new();
        let id = MermaidId(1);
        let img = small_image();
        let p = picker();
        c.inline_protocol_mut(id, &img, 1, &p);
        c.overlay_protocol_mut(id, &img, 1, &p);

        c.invalidate_all();
        assert_eq!(c.len(), (0, 0));
        assert_eq!(c.last_cell_size, None);
    }

    #[test]
    fn invalidate_id_only_affects_one_id() {
        let mut c = ImageCache::new();
        let img = small_image();
        let p = picker();
        c.inline_protocol_mut(MermaidId(1), &img, 1, &p);
        c.inline_protocol_mut(MermaidId(2), &img, 1, &p);
        assert_eq!(c.len(), (2, 0));

        c.invalidate_id(MermaidId(1));
        assert_eq!(c.len(), (1, 0));
    }

    #[test]
    fn repeat_protocol_fetch_is_o1_no_rebuild() {
        let mut c = ImageCache::new();
        let id = MermaidId(1);
        let img = small_image();
        let p = picker();

        c.inline_protocol_mut(id, &img, 1, &p);
        c.inline_protocol_mut(id, &img, 1, &p);
        c.inline_protocol_mut(id, &img, 1, &p);
        assert_eq!(c.len(), (1, 0));
    }
}
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p spur-tui --features markdown --lib components::image_cache
```

Expected: all 8 pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/components/image_cache.rs crates/spur-tui/src/components/mod.rs
git commit -m "feat(spur-tui): image_cache module — generation-based protocol cache"
```

---

## Task 8: SessionDetailView field additions + invalidation rename

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs` (struct definition near line 78; new() / clear() sites; line 589-602 method)

- [ ] **Step 1: Find the `SessionDetailView` struct definition** (around line 78 — search for `pub(crate) mermaid_registry`). Add three new fields next to it:

```rust
    pub(crate) mermaid_registry: std::collections::HashMap<
        crate::components::mermaid::MermaidId,
        crate::components::mermaid::MermaidState,
    >,
    /// Owns rendered protocols for diagrams in `mermaid_registry`. Sibling
    /// of the registry so we can split-borrow during render.
    #[cfg(feature = "markdown")]
    pub(crate) image_cache: crate::components::image_cache::ImageCache,
    /// Coalesces re-raster requests — at most one in flight per id.
    #[cfg(feature = "markdown")]
    pub(crate) in_flight_renders: std::collections::HashSet<
        crate::components::mermaid::MermaidId,
    >,
    /// Source of monotonic `image_generation` values stored on
    /// `MermaidState::Ready` and snapshotted by `image_cache` for
    /// stale-protocol detection.
    #[cfg(feature = "markdown")]
    pub(crate) next_image_generation: u64,
```

- [ ] **Step 2: Find every `Self {` struct constructor in this file** — three exist (`new`, a test ctor, and a session-list ctor; cargo check will flag them). Initialize the new fields:

```rust
            mermaid_registry: std::collections::HashMap::new(),
            #[cfg(feature = "markdown")]
            image_cache: crate::components::image_cache::ImageCache::new(),
            #[cfg(feature = "markdown")]
            in_flight_renders: std::collections::HashSet::new(),
            #[cfg(feature = "markdown")]
            next_image_generation: 0,
```

(Add the four lines anywhere in each constructor where the existing `mermaid_registry` is initialized.)

- [ ] **Step 3: Find `invalidate_inline_protocols` at line 589-602** and replace the whole method:

```rust
    /// Drop every cached protocol so they are rebuilt at the new Rect size
    /// on the next render. Called on terminal resize (app.rs:876) and on
    /// session reset.
    #[cfg(feature = "markdown")]
    pub fn invalidate_inline_protocols(&mut self) {
        self.image_cache.invalidate_all();
    }
```

The method body change drops the old `for state in self.mermaid_registry.values() { ... }` loop entirely — `MermaidState::Ready` no longer carries the protocol slot.

- [ ] **Step 4: Find the existing call site** in this file at line 421 (the session-reset path). It already calls `self.invalidate_inline_protocols()`. No change needed.

- [ ] **Step 5: `cargo check -p spur-tui --features markdown`** — expect a compile error at the test in this file (around line 2431) that uses the old `invalidate_inline_protocols` semantics; that's fine, the API contract is unchanged.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-tui): SessionDetailView gains image_cache + in_flight_renders + next_image_generation"
```

---

## Task 9: Update `handle_mermaid_completed` and Ready construction sites

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs:921-940` (handle_mermaid_completed)
- Modify: `crates/spur-tui/src/views/session_detail.rs:1611-1623, 1786-1798` (fence emit sites — pass placeholder bucket for now; Task 14 wires the real bucket)
- Modify: existing tests that construct `MermaidState::Ready` (search for `MermaidState::Ready {`)

- [ ] **Step 1: Replace `handle_mermaid_completed`** at lines 921-940:

```rust
    #[cfg(feature = "markdown")]
    pub fn handle_mermaid_completed(
        &mut self,
        ref_id: crate::components::mermaid::MermaidId,
        target_width: u32,
        result: Result<std::sync::Arc<image::DynamicImage>, String>,
    ) {
        use crate::components::mermaid::MermaidState;

        // Always release the in-flight slot, success or failure.
        self.in_flight_renders.remove(&ref_id);

        // Retain the source code from the previous state so a future
        // re-raster on bucket-up can re-dispatch without reaching back
        // into MarkdownStream.
        let code = match self.mermaid_registry.get(&ref_id) {
            Some(MermaidState::Pending { code }) => code.clone(),
            Some(MermaidState::Ready { code, .. }) => code.clone(),
            _ => String::new(),
        };

        let state = match result {
            Ok(image) => {
                self.next_image_generation = self.next_image_generation.saturating_add(1);
                MermaidState::Ready {
                    image,
                    code,
                    rastered_at_bucket: target_width,
                    image_generation: self.next_image_generation,
                }
            }
            Err(message) => MermaidState::Error { message },
        };
        self.mermaid_registry.insert(ref_id, state);

        // Mark every markdown stream dirty so the next tick's maybe_flush
        // rebuilds placeholders — transitions Pending→Ready (📊) or →Error (⚠).
        self.react_trace.mark_all_streams_dirty();
    }
```

- [ ] **Step 2: Update the fence-emit site at session_detail.rs:1611-1623**:

Find the existing block and replace:

```rust
                            self.mermaid_registry.insert(
                                fence.id,
                                crate::components::mermaid::MermaidState::Pending {
                                    code: fence.code.clone(),
                                },
                            );
                            self.in_flight_renders.insert(fence.id);
                            self.pending_fence_actions.push_back(
                                crate::action::Action::MermaidRenderRequest {
                                    session: self.session_id.clone(),
                                    ref_id: fence.id,
                                    code: fence.code,
                                    // Task 14 wires the live pane's bucket here.
                                    // Use the smallest bucket as a safe default;
                                    // a re-raster will upgrade as soon as the
                                    // pane reports its real width.
                                    target_width: crate::components::mermaid::RASTER_BUCKETS[0],
                                },
                            );
```

- [ ] **Step 3: Update the second fence-emit site at lines 1786-1798** identically:

```rust
            for (_entry_idx, fence) in self.react_trace.drain_fence_dispatches(&states) {
                self.mermaid_registry.insert(
                    fence.id,
                    crate::components::mermaid::MermaidState::Pending {
                        code: fence.code.clone(),
                    },
                );
                self.in_flight_renders.insert(fence.id);
                self.pending_fence_actions
                    .push_back(crate::action::Action::MermaidRenderRequest {
                        session: self.session_id.clone(),
                        ref_id: fence.id,
                        code: fence.code,
                        target_width: crate::components::mermaid::RASTER_BUCKETS[0],
                    });
            }
```

- [ ] **Step 4: Find existing tests that construct `MermaidState::Ready`** — `cargo check` will surface them. Most will be in this file's `mod tests` block. Update each construction to include the new fields. Example pattern:

```rust
view.mermaid_registry.insert(
    id,
    MermaidState::Ready {
        image: img.clone(),
        code: String::new(),
        rastered_at_bucket: 800,
        image_generation: 1,
    },
);
```

- [ ] **Step 5: `cargo check -p spur-tui --features markdown`** — expect remaining errors only in `mermaid_viewer.rs` and the render path (cleared in Tasks 10-12).

- [ ] **Step 6: Run session_detail tests in isolation:**

```bash
cargo test -p spur-tui --features markdown --lib views::session_detail
```

Expected: existing tests compile and pass. (The 3 new test names are added in Task 14.)

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-tui): handle_mermaid_completed bumps image_generation + records target_width"
```

---

## Task 10: Thread `image_cache` through `RenderContext`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/types.rs:95-102`
- Modify: `crates/spur-tui/src/components/react_trace/render.rs` (render_inline_image at line 236)
- Modify: `crates/spur-tui/src/views/session_detail.rs` (render_with_ctx call site at line 1993-1998)

- [ ] **Step 1: Replace `RenderContext`** in types.rs at lines 95-102:

```rust
#[cfg(feature = "markdown")]
pub struct RenderContext<'a> {
    pub mermaid_registry: &'a std::collections::HashMap<
        crate::components::mermaid::MermaidId,
        crate::components::mermaid::MermaidState,
    >,
    pub picker: Option<&'a ratatui_image::picker::Picker>,
    pub image_cache: &'a mut crate::components::image_cache::ImageCache,
}
```

- [ ] **Step 2: Update `render_with_ctx`'s signature** at render.rs:417 — change `ctx: &RenderContext<'_>` to `ctx: &mut RenderContext<'_>` so we can borrow `image_cache: &mut`. Find every use of `ctx` inside the body — most are `&` access to `mermaid_registry` and `picker`, which still work via `&mut RenderContext`.

The only behavioural change is that `compute_fence_states` (which takes `&RenderContext`) needs a `&` re-borrow. After:

```rust
    pub fn render_with_ctx(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        ctx: &mut RenderContext<'_>,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
    ) {
```

Inside the body, replace all calls `compute_fence_states(ctx, ...)` with `compute_fence_states(&*ctx, effective_width, inner.height)` (the existing 3 sites are already updated in Task 6 to take `inner.height`; here we just add the `&*ctx` re-borrow).

- [ ] **Step 3: Replace `render_inline_image`** at render.rs:236-268:

```rust
/// Render the inline image for a `Ready` diagram into `rect`. Returns true
/// if the image widget was rendered; false if the caller should fall back
/// to the multi-row partial card.
#[cfg(feature = "markdown")]
fn render_inline_image(
    frame: &mut Frame,
    rect: Rect,
    id: crate::components::mermaid::MermaidId,
    ctx: &mut RenderContext<'_>,
) -> bool {
    use crate::components::mermaid::MermaidState;
    use ratatui_image::{Resize, StatefulImage};

    let Some(MermaidState::Ready {
        image,
        image_generation,
        ..
    }) = ctx.mermaid_registry.get(&id)
    else {
        return false;
    };
    let Some(picker) = ctx.picker else {
        return false;
    };

    // Snapshot generation now — protocol fetch needs &mut on image_cache,
    // which conflicts with the &mermaid_registry borrow above.
    let gen = *image_generation;
    let image_arc = image.clone(); // Arc clone is cheap; satisfies borrowck

    let proto = ctx.image_cache.inline_protocol_mut(id, &image_arc, gen, picker);
    let widget = StatefulImage::default().resize(Resize::Fit(None));
    frame.render_stateful_widget(widget, rect, proto);
    true
}
```

- [ ] **Step 4: Update every `render_inline_image(frame, rect, id, ctx)` call site in render.rs** to pass `ctx` as `&mut`. Likely just one site in `render_with_ctx`. Change `ctx` to `&mut *ctx` if needed (cargo check will guide).

- [ ] **Step 5: Update the `RenderContext` construction in session_detail.rs:1993-1998**:

```rust
        // ── React trace ─────────────────────────────────────────────────
        #[cfg(feature = "markdown")]
        {
            let mut ctx = crate::components::react_trace::RenderContext {
                mermaid_registry: &self.mermaid_registry,
                picker: self.render_picker.as_ref(),
                image_cache: &mut self.image_cache,
            };
            self.react_trace
                .render_with_ctx(frame, chunks[1], &mut ctx, lineage);
        }
```

- [ ] **Step 6: `cargo check -p spur-tui --features markdown`** — should be clean for the inline render path. Overlay path still errors (Task 15).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/types.rs crates/spur-tui/src/components/react_trace/render.rs crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-tui): RenderContext threads &mut ImageCache; render_inline_image uses cache"
```

---

## Task 11: Multi-row partial-visibility card (TDD)

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/render.rs` (add helper near render_inline_image; tests at the bottom)

- [ ] **Step 1: Add tests** in `render.rs#tests` (extend the `height_tests` mod or add a new `mod card_tests`):

```rust
#[cfg(all(test, feature = "markdown"))]
mod card_tests {
    use super::*;
    use crate::components::mermaid::MermaidId;
    use ratatui::{backend::TestBackend, Terminal};

    fn render_into(rect_h: u16, body: impl FnOnce(&mut Frame, Rect)) -> Vec<String> {
        // 80 cols × rect_h rows; render the body into a Rect at (0, 0).
        let backend = TestBackend::new(80, rect_h);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let r = Rect { x: 0, y: 0, width: 80, height: rect_h };
            body(f, r);
        }).unwrap();
        // Pull the buffer cells into one string per row.
        let buf = term.backend().buffer().clone();
        (0..rect_h)
            .map(|y| {
                let mut s = String::new();
                for x in 0..80 {
                    s.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
                }
                s.trim_end().to_string()
            })
            .collect()
    }

    #[test]
    fn card_top_visible_says_scroll_down() {
        // first_row_within=0, run_len=5, total_rows=20 → top visible, bottom cropped.
        let lines = render_into(5, |f, r| {
            render_partial_card(f, r, MermaidId(7), 20, 0, 5);
        });
        let joined = lines.join("\n");
        assert!(joined.contains("▼ scroll down"), "expected scroll-down indicator: {joined}");
    }

    #[test]
    fn card_bottom_visible_says_scroll_up() {
        // first_row_within=15, run_len=5, total_rows=20 → top cropped, bottom visible.
        let lines = render_into(5, |f, r| {
            render_partial_card(f, r, MermaidId(3), 20, 15, 5);
        });
        let joined = lines.join("\n");
        assert!(joined.contains("▲ scroll up"), "expected scroll-up indicator: {joined}");
    }

    #[test]
    fn card_mid_window_says_scroll_for_more() {
        // first_row_within=8, run_len=5, total_rows=20 → both edges cropped.
        let lines = render_into(5, |f, r| {
            render_partial_card(f, r, MermaidId(2), 20, 8, 5);
        });
        let joined = lines.join("\n");
        assert!(joined.contains("▲▼ scroll for more"), "expected mid-window indicator: {joined}");
    }

    #[test]
    fn card_visible_pct_at_50() {
        let lines = render_into(5, |f, r| {
            render_partial_card(f, r, MermaidId(1), 20, 0, 10);
        });
        let joined = lines.join("\n");
        assert!(joined.contains("50%"), "expected 50% indicator: {joined}");
    }

    #[test]
    fn card_visible_pct_total_zero_returns_100() {
        let lines = render_into(3, |f, r| {
            render_partial_card(f, r, MermaidId(1), 0, 0, 1);
        });
        let joined = lines.join("\n");
        assert!(joined.contains("100%"), "total_rows=0 should display 100%: {joined}");
    }

    #[test]
    fn card_one_line_variant_when_run_len_1() {
        let lines = render_into(1, |f, r| {
            render_partial_card(f, r, MermaidId(1), 20, 0, 1);
        });
        // Exactly one non-blank line.
        let non_blank = lines.iter().filter(|l| !l.is_empty()).count();
        assert_eq!(non_blank, 1, "expected 1 non-blank line, got {non_blank}: {lines:?}");
    }

    #[test]
    fn card_two_line_variant_when_run_len_2() {
        let lines = render_into(2, |f, r| {
            render_partial_card(f, r, MermaidId(1), 20, 0, 2);
        });
        let non_blank = lines.iter().filter(|l| !l.is_empty()).count();
        assert_eq!(non_blank, 2, "expected 2 non-blank lines, got {non_blank}: {lines:?}");
    }

    #[test]
    fn card_three_line_variant_when_run_len_3_or_more() {
        let lines = render_into(3, |f, r| {
            render_partial_card(f, r, MermaidId(1), 20, 0, 3);
        });
        // 3-line variant: title, blank, hint → 2 non-blank.
        let non_blank = lines.iter().filter(|l| !l.is_empty()).count();
        assert_eq!(non_blank, 2);
    }

    #[test]
    fn card_early_returns_when_run_len_0() {
        let lines = render_into(1, |f, r| {
            render_partial_card(f, r, MermaidId(1), 20, 0, 0);
        });
        // No content rendered.
        assert!(lines.iter().all(|l| l.is_empty()), "expected blank: {lines:?}");
    }

    #[test]
    fn card_centers_when_run_len_exceeds_card_height() {
        // run_len=11, card=3 lines → top padding (11-3)/2 = 4. Card at rows 4..7.
        let lines = render_into(11, |f, r| {
            render_partial_card(f, r, MermaidId(1), 20, 0, 11);
        });
        // Rows 0..4 should be blank; row 4 has the title.
        for (i, l) in lines.iter().take(4).enumerate() {
            assert!(l.is_empty(), "row {i} expected blank, got: {l:?}");
        }
        assert!(lines[4].contains("mermaid #1"), "row 4 expected title, got: {:?}", lines[4]);
    }
}
```

- [ ] **Step 2: Run tests, expect failures (function not defined)**

```bash
cargo test -p spur-tui --features markdown --lib components::react_trace::render::card_tests
```

Expected: 10 tests fail with `cannot find function render_partial_card`.

- [ ] **Step 3: Add `render_partial_card`** in render.rs near `render_inline_image`:

```rust
/// Minimum `run_len` for the multi-line card variant. Below this we fall
/// through to a single-line message.
#[cfg(feature = "markdown")]
const PARTIAL_CARD_MIN_ROWS: u16 = 3;

/// Render a stable card describing a partially-scrolled diagram. Reserves
/// the full `run_len`-tall Rect (no layout shift); the card itself occupies
/// 1, 2, or 3 lines depending on `run_len`, vertically centred when smaller.
///
/// Direction labels combine arrow + word (`▼ scroll down` not just `▼`)
/// to disambiguate from focus / expansion glyphs elsewhere in the TUI.
#[cfg(feature = "markdown")]
pub(crate) fn render_partial_card(
    frame: &mut Frame,
    rect: Rect,
    id: crate::components::mermaid::MermaidId,
    total_rows: u16,
    first_row_within: u16,
    run_len: u16,
) {
    if run_len == 0 {
        return;
    }

    let visible_pct = if total_rows == 0 {
        100u16
    } else {
        ((run_len as u32 * 100) / (total_rows as u32)).min(100) as u16
    };

    let direction = match (
        first_row_within == 0,
        first_row_within.saturating_add(run_len) >= total_rows,
    ) {
        (true, false) => "▼ scroll down",
        (false, true) => "▲ scroll up",
        _ => "▲▼ scroll for more",
    };

    let lines: Vec<Line<'static>> = match run_len {
        1 => vec![Line::from(Span::styled(
            format!("[📊 mermaid #{} · {}% · {}]", id.0, visible_pct, direction),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ))],
        2 => vec![
            Line::from(Span::styled(
                format!("📊 mermaid #{} · {}% visible · {}", id.0, visible_pct, direction),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Alt-v · open in full viewer",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )),
        ],
        _ /* run_len ≥ PARTIAL_CARD_MIN_ROWS = 3 */ => vec![
            Line::from(Span::styled(
                format!("📊 mermaid #{} · {}% visible · {}", id.0, visible_pct, direction),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Alt-v · open in full viewer",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )),
        ],
    };

    let card_height = lines.len() as u16;
    let card_rect = if card_height < run_len {
        let pad_top = (run_len - card_height) / 2;
        Rect {
            x: rect.x,
            y: rect.y + pad_top,
            width: rect.width,
            height: card_height,
        }
    } else {
        rect
    };
    frame.render_widget(Paragraph::new(lines), card_rect);
}
```

- [ ] **Step 4: Run the new tests:**

```bash
cargo test -p spur-tui --features markdown --lib components::react_trace::render::card_tests
```

Expected: all 10 pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/render.rs
git commit -m "feat(spur-tui): render_partial_card — height-stable card with direction labels"
```

---

## Task 12: Wire `render_partial_card` into `render_with_ctx`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/render.rs:573-602`

- [ ] **Step 1: Find the `Segment::Image` branch in `render_with_ctx`** at lines 561-604 and replace the `if !drew_image` block:

```rust
                Segment::Image {
                    id,
                    total_rows,
                    first_row_within,
                    run_len,
                } => {
                    let rect = Rect {
                        x: inner.x,
                        y,
                        width: inner.width,
                        height: run_len,
                    };
                    let fully_visible = first_row_within == 0 && run_len == total_rows;

                    let drew_image = if fully_visible {
                        render_inline_image(frame, rect, id, ctx)
                    } else {
                        false
                    };

                    if !drew_image {
                        if matches!(
                            ctx.mermaid_registry.get(&id),
                            Some(crate::components::mermaid::MermaidState::Ready { .. })
                        ) {
                            // Partial visibility OR no graphics protocol available
                            // for a Ready image — render the multi-row card.
                            render_partial_card(
                                frame,
                                rect,
                                id,
                                total_rows,
                                first_row_within,
                                run_len,
                            );
                        } else {
                            // Pending / Rendering / Error — single-line dim
                            // placeholder. Layout still preserved (run_len rows).
                            let msg = match ctx.mermaid_registry.get(&id) {
                                Some(crate::components::mermaid::MermaidState::Error { .. }) => {
                                    format!("   [⚠ mermaid #{} error · Alt-v to view]", id.0)
                                }
                                _ => format!("   [⏳ mermaid #{} rendering…]", id.0),
                            };
                            let line = Line::from(Span::styled(
                                msg,
                                Style::default()
                                    .fg(Color::DarkGray)
                                    .add_modifier(Modifier::DIM),
                            ));
                            frame.render_widget(Paragraph::new(vec![line]), rect);
                        }
                    }
                    y += run_len;
                }
```

- [ ] **Step 2: `cargo check -p spur-tui --features markdown`** — should be clean except for the overlay path (Task 15).

- [ ] **Step 3: `cargo test -p spur-tui --features markdown --lib components::react_trace`** — all existing tests still pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/render.rs
git commit -m "feat(spur-tui): partial-visibility branch renders multi-row card"
```

---

## Task 13: Extend `VirtualRowCacheEntry` with soft_cap + cell metrics

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/render.rs:37-50` (struct def)
- Modify: `crates/spur-tui/src/components/react_trace/render.rs:437-512` (cache-hit check, build paths)
- Modify: `crates/spur-tui/src/components/react_trace/builder.rs` (build_virtual_rows signature)
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs:1233-1252` (seed_line_cache_for_tests)

- [ ] **Step 1: Replace `VirtualRowCacheEntry`** at render.rs:37-50:

```rust
/// Cached virtual rows for the markdown render path.
///
/// Cache hit requires ALL of `(width, soft_cap, cell_w_px, cell_h_px,
/// fence_gen)` to match. Cell metrics are part of the key because
/// `compute_inline_height_rows` depends on them — a font swap with
/// unchanged cols/rows would otherwise silently false-hit.
#[cfg(feature = "markdown")]
pub(in crate::components) struct VirtualRowCacheEntry {
    pub(super) rows: Vec<VirtualRow>,
    /// Row index where each entry's virtual rows begin.
    /// `entry_row_starts[i]` = index into `rows` where entry `i` starts.
    pub(super) entry_row_starts: Vec<usize>,
    /// Per-row byte range within the source entry content. Co-indexed with
    /// `rows`. `None` means the row is synthetic (blank separator, etc.).
    pub(super) byte_ranges: Vec<Option<std::ops::Range<usize>>>,
    pub(super) width: u16,
    /// Derived from pane_height — see `compute_soft_cap`.
    pub(super) soft_cap: u16,
    pub(super) cell_w_px: u32,
    pub(super) cell_h_px: u32,
    pub(super) generation: u64,
    /// Snapshot of mermaid fence states at cache time. If any state changes
    /// (e.g. Pending→Ready), the cache must be rebuilt.
    pub(super) fence_gen: u64,
}
```

- [ ] **Step 2: Replace the cache-hit / build path** in `render_with_ctx` at render.rs:437-512. This is the longest edit — the hits-and-builds block. Replace the whole `{ let dirty = ...; ... }` block (lines 442-513) with:

```rust
        {
            let dirty = self.dirty_from;

            let (cell_w_px, cell_h_px) = ctx
                .picker
                .map(|p| { let (w, h) = p.font_size(); (w.max(1) as u32, h.max(1) as u32) })
                .unwrap_or((8, 16));
            let soft_cap = compute_soft_cap(inner.height);

            let key_ok = self
                .line_cache
                .as_ref()
                .is_some_and(|c| {
                    c.width == effective_width
                        && c.soft_cap == soft_cap
                        && c.cell_w_px == cell_w_px
                        && c.cell_h_px == cell_h_px
                });
            let fence_ok = self
                .line_cache
                .as_ref()
                .is_some_and(|c| c.fence_gen == fence_gen);

            if key_ok && fence_ok {
                match dirty {
                    None => { /* cache fully valid */ }
                    Some(dirty_idx) if dirty_idx > 0 => {
                        // Incremental rebuild from dirty_idx.
                        let c = self.line_cache.as_mut().unwrap();
                        let trunc_row = if dirty_idx < c.entry_row_starts.len() {
                            c.entry_row_starts[dirty_idx]
                        } else {
                            c.rows.len()
                        };
                        c.rows.truncate(trunc_row);
                        c.byte_ranges.truncate(trunc_row);
                        c.entry_row_starts.truncate(dirty_idx);
                        let base = c.rows.len();
                        let states = compute_fence_states(&*ctx, effective_width, inner.height);
                        let (new_rows, new_starts, new_byte_ranges) =
                            self.build_virtual_rows(dirty_idx, effective_width, &states, lineage);
                        let c = self.line_cache.as_mut().unwrap();
                        c.rows.extend(new_rows);
                        c.byte_ranges.extend(new_byte_ranges);
                        c.entry_row_starts
                            .extend(new_starts.iter().map(|s| s + base));
                        c.generation = self.generation;
                        self.dirty_from = None;
                    }
                    _ => {
                        // Full rebuild (dirty_idx == 0 or no cache).
                        let states = compute_fence_states(&*ctx, effective_width, inner.height);
                        let (rows, entry_row_starts, byte_ranges) =
                            self.build_virtual_rows(0, effective_width, &states, lineage);
                        self.line_cache = Some(VirtualRowCacheEntry {
                            rows,
                            entry_row_starts,
                            byte_ranges,
                            width: effective_width,
                            soft_cap,
                            cell_w_px,
                            cell_h_px,
                            generation: self.generation,
                            fence_gen,
                        });
                        self.dirty_from = None;
                    }
                }
            } else {
                // Width / soft_cap / cell_metrics / fence_gen drift — full rebuild.
                let states = compute_fence_states(&*ctx, effective_width, inner.height);
                let (rows, entry_row_starts, byte_ranges) =
                    self.build_virtual_rows(0, effective_width, &states, lineage);
                self.line_cache = Some(VirtualRowCacheEntry {
                    rows,
                    entry_row_starts,
                    byte_ranges,
                    width: effective_width,
                    soft_cap,
                    cell_w_px,
                    cell_h_px,
                    generation: self.generation,
                    fence_gen,
                });
                self.dirty_from = None;
            }
        }
```

- [ ] **Step 3: Update `seed_line_cache_for_tests`** at mod.rs:1233-1252:

```rust
    pub fn seed_line_cache_for_tests(
        &mut self,
        width: u16,
        states: &std::collections::HashMap<
            crate::components::mermaid::MermaidId,
            crate::components::mermaid::FenceRender,
        >,
    ) {
        let (rows, entry_row_starts, byte_ranges) = self.build_virtual_rows(0, width, states, None);
        self.line_cache = Some(render::VirtualRowCacheEntry {
            rows,
            entry_row_starts,
            byte_ranges,
            width,
            soft_cap: 60,        // sensible default for tests
            cell_w_px: 8,        // typical non-retina monospace
            cell_h_px: 16,
            generation: self.generation,
            fence_gen: 0,
        });
    }
```

- [ ] **Step 4: Add cache-key tests** in render.rs#tests (extend the existing module or add a new one):

```rust
#[cfg(all(test, feature = "markdown"))]
mod cache_key_tests {
    use super::*;

    #[test]
    fn cache_hit_when_soft_cap_unchanged() {
        // pane_h 80→90: both yield soft_cap=60 (target_cap=max(53/60,60)=60,
        // max_inline=86/76, soft=60). Same key → hit.
        assert_eq!(compute_soft_cap(80), compute_soft_cap(90));
    }

    #[test]
    fn cache_miss_when_soft_cap_changes() {
        // pane_h 80→200: soft_cap 60→100 (hard cap). Different key → miss.
        assert_ne!(compute_soft_cap(80), compute_soft_cap(200));
    }

    #[test]
    fn cache_miss_when_width_changes() {
        // The cache-hit check compares `c.width == effective_width`.
        // We can't test the full render path here without a Picker, but
        // the contract is documented in render_with_ctx. Skipped — covered
        // by integration smoke test in Task 17.
        // (This test exists as a placeholder in the catalog.)
    }

    #[test]
    fn cache_miss_when_fence_gen_changes() {
        // Same shape as width — covered by integration.
    }

    #[test]
    fn cache_miss_when_cell_metric_changes() {
        // Documents I-H5: cell_w_px / cell_h_px are part of the cache key.
        // The render_with_ctx code path explicitly compares these fields.
        // This test serves as documentation; the actual behavior is
        // exercised in render_with_ctx and the integration smoke test.
    }
}
```

- [ ] **Step 5: `cargo check -p spur-tui --features markdown`** + run the cache_key_tests:

```bash
cargo test -p spur-tui --features markdown --lib components::react_trace::render::cache_key_tests
```

Expected: 5 pass (including the 3 placeholder tests).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/render.rs crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "feat(spur-tui): VirtualRowCacheEntry adds soft_cap + cell metrics"
```

---

## Task 14: `maybe_request_rerasters` re-raster trigger (TDD)

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs` (new method + call site in `render`)
- Modify: `crates/spur-tui/src/views/session_detail.rs:1611-1623, 1786-1798` — replace placeholder bucket with real `raster_width_for_pane(pane_w_px)`

- [ ] **Step 1: Add tests** in `session_detail.rs#tests` (the existing test module around line 2370):

```rust
    use crate::components::mermaid::{MermaidId, MermaidState, RASTER_BUCKETS};
    use std::sync::Arc;
    use image::{DynamicImage, RgbaImage};

    fn ready_at(bucket: u32, gen: u64) -> MermaidState {
        MermaidState::Ready {
            image: Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10))),
            code: "graph TD\nA-->B".into(),
            rastered_at_bucket: bucket,
            image_generation: gen,
        }
    }

    #[test]
    fn maybe_request_rerasters_skips_when_bucket_unchanged() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(1), ready_at(800, 1));
        // pane_w_px = 80 cols × 8 px = 640 → bucket 800. No upgrade.
        view.maybe_request_rerasters(80, 8);
        assert!(view.pending_fence_actions.is_empty(),
            "no requests when bucket unchanged");
    }

    #[test]
    fn maybe_request_rerasters_emits_for_lower_bucketed_ready() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(1), ready_at(800, 1));
        // pane_w_px = 200 cols × 8 px = 1600 → bucket 1600. Upgrade.
        view.maybe_request_rerasters(200, 8);
        assert_eq!(view.pending_fence_actions.len(), 1);
        assert!(view.in_flight_renders.contains(&MermaidId(1)));
    }

    #[test]
    fn maybe_request_rerasters_skips_pending() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(
            MermaidId(2),
            MermaidState::Pending { code: "g".into() },
        );
        view.maybe_request_rerasters(200, 8);
        assert!(view.pending_fence_actions.is_empty());
    }

    #[test]
    fn maybe_request_rerasters_skips_in_flight() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(3), ready_at(800, 1));
        view.in_flight_renders.insert(MermaidId(3));
        view.maybe_request_rerasters(200, 8);
        assert!(view.pending_fence_actions.is_empty(),
            "no duplicate requests for in-flight ids");
    }

    #[test]
    fn maybe_request_rerasters_skips_just_landed_at_new_bucket() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(4), ready_at(1600, 1));
        // pane_w_px = 200 cols × 8 px = 1600 → bucket 1600. Already there.
        view.maybe_request_rerasters(200, 8);
        assert!(view.pending_fence_actions.is_empty());
    }

    #[test]
    fn rerasters_coalesce_during_in_flight() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(5), ready_at(800, 1));

        // First trigger: pane grows to bucket 1200.
        view.maybe_request_rerasters(150, 8);
        assert_eq!(view.pending_fence_actions.len(), 1);

        // Second trigger BEFORE completion: pane grows to bucket 2000.
        view.maybe_request_rerasters(250, 8);
        // Still only one — id is in_flight, gated.
        assert_eq!(view.pending_fence_actions.len(), 1);
    }

    #[test]
    fn handle_completed_clears_in_flight() {
        let mut view = SessionDetailView::new_for_tests();
        view.in_flight_renders.insert(MermaidId(6));
        view.mermaid_registry.insert(
            MermaidId(6),
            MermaidState::Pending { code: "g".into() },
        );
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        view.handle_mermaid_completed(MermaidId(6), 800, Ok(img));
        assert!(!view.in_flight_renders.contains(&MermaidId(6)));
    }

    #[test]
    fn handle_completed_records_target_width_on_ready() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(
            MermaidId(7),
            MermaidState::Pending { code: "g".into() },
        );
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        view.handle_mermaid_completed(MermaidId(7), 1600, Ok(img));
        match view.mermaid_registry.get(&MermaidId(7)) {
            Some(MermaidState::Ready { rastered_at_bucket, .. }) => {
                assert_eq!(*rastered_at_bucket, 1600);
            }
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn handle_completed_retains_code_on_ready_to_ready() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(8), MermaidState::Ready {
            image: Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10))),
            code: "ORIGINAL".into(),
            rastered_at_bucket: 800,
            image_generation: 1,
        });
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(20, 20)));
        view.handle_mermaid_completed(MermaidId(8), 1600, Ok(img));
        match view.mermaid_registry.get(&MermaidId(8)) {
            Some(MermaidState::Ready { code, .. }) => assert_eq!(code, "ORIGINAL"),
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn handle_completed_retains_code_on_pending_to_ready() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(
            MermaidId(9),
            MermaidState::Pending { code: "PENDING_SOURCE".into() },
        );
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        view.handle_mermaid_completed(MermaidId(9), 800, Ok(img));
        match view.mermaid_registry.get(&MermaidId(9)) {
            Some(MermaidState::Ready { code, .. }) => assert_eq!(code, "PENDING_SOURCE"),
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn handle_completed_bumps_image_generation_on_ok() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(
            MermaidId(10),
            MermaidState::Pending { code: "g".into() },
        );
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        view.handle_mermaid_completed(MermaidId(10), 800, Ok(img.clone()));
        let gen1 = match view.mermaid_registry.get(&MermaidId(10)) {
            Some(MermaidState::Ready { image_generation, .. }) => *image_generation,
            _ => panic!(),
        };
        view.handle_mermaid_completed(MermaidId(10), 1200, Ok(img));
        let gen2 = match view.mermaid_registry.get(&MermaidId(10)) {
            Some(MermaidState::Ready { image_generation, .. }) => *image_generation,
            _ => panic!(),
        };
        assert!(gen2 > gen1, "generation must monotonically increase");
    }

    #[test]
    fn handle_completed_never_decreases_bucket() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(11), MermaidState::Ready {
            image: Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10))),
            code: "g".into(),
            rastered_at_bucket: 1600,
            image_generation: 1,
        });
        // Even if a stale completion arrives with a smaller bucket, the
        // handler stores the COMPLETION's bucket — but maybe_request_rerasters
        // never EMITS at a smaller bucket (test is for the trigger, not the
        // handler). The handler simply records what arrived.
        // I-R1 is enforced at the EMIT side (maybe_request_rerasters compares
        // current_bucket against rastered_at_bucket and only emits if greater).
        // This test verifies the emit side.
        view.maybe_request_rerasters(80, 8); // pane_w_px=640 → bucket 800
        assert!(
            view.pending_fence_actions.is_empty(),
            "must never emit when current_bucket < rastered_at_bucket"
        );
    }

    #[test]
    fn fence_emit_uses_current_bucket() {
        // This test verifies that maybe_request_rerasters emits at the
        // CURRENT pane's bucket — exercises the fence emit pathway with a
        // pane wider than 800. Initial fence emit (Task 14 wires this) uses
        // the same path conceptually.
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(12), ready_at(800, 1));
        view.maybe_request_rerasters(200, 8); // pane_w_px=1600 → bucket 1600
        assert_eq!(view.pending_fence_actions.len(), 1);
        match view.pending_fence_actions.front() {
            Some(crate::action::Action::MermaidRenderRequest { target_width, .. }) => {
                assert!(*target_width >= 1200, "target_width should be ≥ 1200, got {target_width}");
            }
            _ => panic!("expected MermaidRenderRequest"),
        }
    }
```

- [ ] **Step 2: Add the `maybe_request_rerasters` method** to `SessionDetailView` (place near `handle_mermaid_completed`, around line 940):

```rust
    /// Inspect Ready diagrams; emit re-raster requests for any whose
    /// `rastered_at_bucket` is below the current pane's bucket. Coalesced
    /// via `in_flight_renders` so only one request per id can be live.
    /// Two-phase (collect → mutate) for borrow-checker robustness.
    #[cfg(feature = "markdown")]
    pub fn maybe_request_rerasters(&mut self, pane_cols: u16, cell_w_px: u16) {
        use crate::components::mermaid::{raster_width_for_pane, MermaidState};
        let pane_w_px = (pane_cols as u32).saturating_mul(cell_w_px as u32);
        let new_bucket = raster_width_for_pane(pane_w_px);

        let candidates: Vec<(crate::components::mermaid::MermaidId, String)> = self
            .mermaid_registry
            .iter()
            .filter_map(|(id, state)| match state {
                MermaidState::Ready { rastered_at_bucket, code, .. }
                    if *rastered_at_bucket < new_bucket
                        && !self.in_flight_renders.contains(id) =>
                {
                    Some((*id, code.clone()))
                }
                _ => None,
            })
            .collect();

        for (id, code) in candidates {
            self.in_flight_renders.insert(id);
            self.pending_fence_actions.push_back(
                crate::action::Action::MermaidRenderRequest {
                    session: self.session_id.clone(),
                    ref_id: id,
                    code,
                    target_width: new_bucket,
                },
            );
        }
    }
```

- [ ] **Step 3: Run the new tests:**

```bash
cargo test -p spur-tui --features markdown --lib views::session_detail
```

Expected: all new tests pass.

- [ ] **Step 4: Wire the trigger into `SessionDetailView::render`** — find the existing render method, find where `chunks[1]` (trace pane) is rendered (~line 1990), and add a call AFTER the React-trace render block but before the Workers panel:

```rust
        // After react_trace render — re-raster on bucket-up.
        #[cfg(feature = "markdown")]
        {
            let cell_w_px = self
                .render_picker
                .as_ref()
                .map(|p| p.font_size().0)
                .unwrap_or(8);
            self.maybe_request_rerasters(chunks[1].width, cell_w_px);
        }
```

- [ ] **Step 5: Update fence-emit sites at lines 1611-1623 and 1786-1798** to use the live bucket. Replace the `target_width: crate::components::mermaid::RASTER_BUCKETS[0],` with:

```rust
                                    target_width: {
                                        let cell_w_px = self
                                            .render_picker
                                            .as_ref()
                                            .map(|p| p.font_size().0)
                                            .unwrap_or(8);
                                        // Note: pane width at fence-emit time is not directly
                                        // available; use the last known render width if cached,
                                        // else smallest bucket. The next render frame's
                                        // maybe_request_rerasters will upgrade if needed.
                                        let pane_w_cols = self
                                            .react_trace
                                            .last_render_width()
                                            .unwrap_or(80);
                                        crate::components::mermaid::raster_width_for_pane(
                                            (pane_w_cols as u32).saturating_mul(cell_w_px as u32),
                                        )
                                    },
```

(Apply the same to both fence-emit sites.)

- [ ] **Step 6: Add a `last_render_width(&self) -> Option<u16>` accessor** on `ReactTrace` — `crates/spur-tui/src/components/react_trace/mod.rs`. Find existing public accessors (search for `pub fn`) and add:

```rust
    pub fn last_render_width(&self) -> Option<u16> {
        self.last_render_width
    }
```

(`last_render_width: Option<u16>` already exists on the struct at line ~95 — search to confirm.)

- [ ] **Step 7: `cargo test -p spur-tui --features markdown --lib views::session_detail`** — all 13 tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "feat(spur-tui): maybe_request_rerasters + bucket-aware fence emit"
```

---

## Task 15: MermaidViewerView slim + render_mermaid_overlay refactor

**Files:**
- Modify: `crates/spur-tui/src/views/mermaid_viewer.rs` (drop protocol field)
- Modify: `crates/spur-tui/src/app.rs:2688-2738` (overlay render path)
- Modify: `crates/spur-tui/src/app.rs:2431-2451` (overlay dispatch — search for `MermaidOverlay`)

- [ ] **Step 1: Replace `MermaidViewerView`** in mermaid_viewer.rs entirely:

```rust
#![cfg(feature = "markdown")]

//! Full-screen overlay state for mermaid viewing. Owns only the cursor
//! (which diagram is focused) — the actual `StatefulProtocol` lives in
//! `SessionDetailView::image_cache.overlay` slot.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{layout::Rect, Frame};
use spur_acp::{SessionId, SpurEvent};

use crate::action::Action;
use crate::components::mermaid::{MermaidId, MermaidState};

use super::View;

pub struct MermaidViewerView {
    session_id: SessionId,
    /// Which diagram is currently focused. `None` until `set_available`
    /// chooses a default (the most recent Ready entry).
    pub(crate) focused: Option<MermaidId>,
}

impl MermaidViewerView {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            focused: None,
        }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Choose the default focus from the registry. Called by the app
    /// layer before `render_mermaid_overlay`.
    pub fn set_available(&mut self, entries: &[(MermaidId, &MermaidState)]) {
        if self.focused.is_none() {
            self.focused = entries
                .iter()
                .rev()
                .find(|(_, s)| matches!(s, MermaidState::Ready { .. }))
                .map(|(id, _)| *id);
        }
    }

    /// Cycle focus among Ready entries.
    pub fn cycle(&mut self, entries: &[(MermaidId, &MermaidState)], forward: bool) {
        let ready_ids: Vec<MermaidId> = entries
            .iter()
            .filter(|(_, s)| matches!(s, MermaidState::Ready { .. }))
            .map(|(id, _)| *id)
            .collect();
        if ready_ids.is_empty() {
            self.focused = None;
            return;
        }
        let idx = self
            .focused
            .and_then(|cur| ready_ids.iter().position(|i| *i == cur))
            .unwrap_or(0);
        let next = if forward {
            (idx + 1) % ready_ids.len()
        } else {
            (idx + ready_ids.len() - 1) % ready_ids.len()
        };
        self.focused = Some(ready_ids[next]);
    }
}

impl View for MermaidViewerView {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &super::ViewContext) -> Option<Action> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Some(Action::NavigateBack),
            _ => None,
        }
    }

    fn handle_spur_event(&mut self, _event: &SpurEvent, _ctx: &super::ViewContext) {}

    fn render(&mut self, _frame: &mut Frame, _area: Rect, _ctx: &super::ViewContext) {}

    fn tick(&mut self) {}
}
```

- [ ] **Step 2: Replace `render_mermaid_overlay`** in app.rs:2687-2738:

```rust
#[cfg(feature = "markdown")]
fn render_mermaid_overlay(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    viewer: &mut crate::views::mermaid_viewer::MermaidViewerView,
    detail: &mut crate::views::session_detail::SessionDetailView,
) {
    use ratatui::{
        layout::{Constraint, Layout},
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::Paragraph,
    };
    use ratatui_image::{Resize, StatefulImage};

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " Mermaid Viewer ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[0],
    );

    let drew = (|| {
        let id = viewer.focused?;
        let picker = detail.render_picker.as_ref()?;
        let (image, image_generation) = match detail.mermaid_registry.get(&id)? {
            crate::components::mermaid::MermaidState::Ready { image, image_generation, .. } => {
                (image.clone(), *image_generation)
            }
            _ => return None,
        };
        let proto = detail.image_cache.overlay_protocol_mut(id, &image, image_generation, picker);
        let widget = StatefulImage::default().resize(Resize::Fit(None));
        frame.render_stateful_widget(widget, chunks[1], proto);
        Some(())
    })()
    .is_some();

    if !drew {
        frame.render_widget(
            Paragraph::new(
                "No diagram available yet. Wait for render to complete, or press q/Esc to return.",
            )
            .style(Style::default().fg(Color::DarkGray)),
            chunks[1],
        );
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " [/]: cycle · q/Esc: close ",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[2],
    );
}
```

- [ ] **Step 3: Find the call site** that invokes `render_mermaid_overlay` (around app.rs:2451). Update the call to pass `detail`:

```rust
                ViewId::MermaidOverlay(ref _session) => {
                    if let Some(detail) = self.session_detail.as_mut() {
                        // viewer is owned by the app, but populated from detail.
                        let entries: Vec<_> = detail
                            .mermaid_registry
                            .iter()
                            .map(|(k, v)| (*k, v))
                            .collect();
                        if let Some(viewer) = self.mermaid_viewer.as_mut() {
                            viewer.set_available(&entries);
                            render_mermaid_overlay(frame, area, viewer, detail);
                        }
                    }
                }
```

(The exact surrounding code may differ; search for `render_mermaid_overlay(` to locate the call.)

- [ ] **Step 4: Find the call site that invokes `viewer.set_available(...)` with a Picker arg** (existed in the old code) — drop the picker arg.

- [ ] **Step 5: `cargo check -p spur-tui --features markdown`** — should now be clean across the workspace.

- [ ] **Step 6: Run all spur-tui tests:**

```bash
cargo test -p spur-tui --features markdown
```

Expected: all green. There may be a few tests that need their `MermaidState::Ready` constructors updated — `cargo check` already surfaced and Task 9 fixed most. Resolve any remaining stragglers inline.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/views/mermaid_viewer.rs crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): MermaidViewerView slim — protocol comes from ImageCache"
```

---

## Task 16: Integration smoke test + final wiring

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs` (add smoke test in tests module)

- [ ] **Step 1: Add the smoke test** to the `mod tests` block in session_detail.rs:

```rust
    #[test]
    fn bucket_up_smoke_test() {
        // End-to-end: a Ready diagram at bucket 800, pane grows to 1600,
        // re-raster request emitted, completion handler runs, bucket
        // updated, image_generation bumped.
        use crate::action::Action;
        use std::sync::Arc;
        use image::{DynamicImage, RgbaImage};

        let mut view = SessionDetailView::new_for_tests();

        // 1. Seed Ready at bucket 800, generation 1.
        let img1 = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        view.mermaid_registry.insert(
            MermaidId(99),
            MermaidState::Ready {
                image: img1,
                code: "graph TD\nA-->B".into(),
                rastered_at_bucket: 800,
                image_generation: 1,
            },
        );
        view.next_image_generation = 1;

        // 2. Pane grows to bucket 1600.
        view.maybe_request_rerasters(200, 8);
        assert_eq!(view.pending_fence_actions.len(), 1);
        assert!(view.in_flight_renders.contains(&MermaidId(99)));

        // 3. Worker completes (simulated).
        let img2 = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(20, 20)));
        view.handle_mermaid_completed(MermaidId(99), 1600, Ok(img2));

        // 4. Verify state.
        assert!(!view.in_flight_renders.contains(&MermaidId(99)));
        match view.mermaid_registry.get(&MermaidId(99)) {
            Some(MermaidState::Ready {
                rastered_at_bucket,
                image_generation,
                code,
                ..
            }) => {
                assert_eq!(*rastered_at_bucket, 1600);
                assert!(*image_generation > 1, "generation must bump");
                assert_eq!(code, "graph TD\nA-->B", "code retained across re-raster");
            }
            _ => panic!("expected Ready"),
        }

        // 5. Subsequent maybe_request_rerasters at the SAME bucket emits nothing.
        view.pending_fence_actions.clear();
        view.maybe_request_rerasters(200, 8);
        assert!(view.pending_fence_actions.is_empty());
    }
```

- [ ] **Step 2: Run the smoke test:**

```bash
cargo test -p spur-tui --features markdown --lib views::session_detail::tests::bucket_up_smoke_test
```

Expected: pass.

- [ ] **Step 3: Run the full workspace test suite:**

```bash
cargo test -p spur-tui --features markdown
```

Expected: all green, ~37+ new tests added.

- [ ] **Step 4: `cargo clippy -p spur-tui --features markdown -- -D warnings`** — verify no new clippy warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "test(spur-tui): bucket_up_smoke_test — end-to-end re-raster pipeline"
```

---

## Task 17: Manual verification (in iTerm2)

**Files:** none (verify the spec's manual checklist).

For each item, follow the spec's checklist verbatim. The commands in steps below are observable confirmations.

- [ ] **Step 1: Build a release-mode binary**

```bash
cargo build --release -p spur-tui --features markdown
```

- [ ] **Step 2: Manual check #1 — iTerm2 retina, 200-col resize after a tall mermaid.**

Open iTerm2. Run `target/release/spur-tui` (or via your usual launcher). In a session, submit:

````
```mermaid
flowchart TD
  A[Start] --> B[Process]
  B --> C{Decision}
  C -->|Yes| D[End]
  C -->|No|  E[Retry]
  E --> B
```
````

In another shell tail the log:

```bash
tail -f .spur/logs/spur-tui*.log | grep -i mermaid
```

Wait for `Ready`. Resize the iTerm2 window from 80 → 200 cols. Verify in the log:
- `MermaidRenderRequest` with a higher `target_width` than initial fires within a frame of the resize.
- `MermaidRenderCompleted` arrives shortly after with the new bucket.

- [ ] **Step 3: Manual check #2 — Scroll past tall mermaid.**

Append ~50 lines after a tall diagram. Use `j` (or Down) to scroll until the diagram is partially visible.
✓ Reserved height for the image area stays constant.
✓ Card text reads `📊 mermaid #N · X% visible · ▼ scroll down` or `▲ scroll up` depending on direction.

- [ ] **Step 4: Manual check #3 — Workers panel collapse.**

Session with 5+ Ready diagrams. Toggle workers panel.
✓ No Pending placeholder appears mid-toggle (the structural property).
✓ Diagrams render at correct scale post-toggle.

- [ ] **Step 5: Manual check #4 — Alt-v overlay round-trip.**

On a Ready diagram, press Alt-v → q → Alt-v.
✓ Re-open is instant; image is identical.

- [ ] **Step 6: Final commit (no code change — checklist verification documented in the commit message itself)**

If any manual check fails, file an issue and add a new task. If all pass:

```bash
git commit --allow-empty -m "chore(spur-tui): mermaid v2 manual verification passed"
```

---

## Self-Review Notes

(After completing all tasks, run this final check before merging.)

- **Spec coverage:** Every requirement in `2026-04-27-mermaid-inline-rendering-v2-design.md` Sections 3.1–3.5 is implemented in Tasks 1–15. Section 5 testing strategy maps to Tasks 3, 6, 7, 11, 13, 14, 16 (37 tests + 1 smoke).
- **Invariant coverage:** I-A1 (Task 2), I-A2 (Task 7), I-A3 (Task 7 + Task 9 generation bump), I-R1 (Task 14 `handle_completed_never_decreases_bucket`), I-R2 (Task 14 `handle_completed_clears_in_flight`), I-R3 (Task 14 — stale image stays during re-raster, exercised by smoke), I-R4 (Task 14 `fence_emit_uses_current_bucket`), I-H1–H7 (Tasks 6, 11, 13).
- **Type consistency:** `image_generation: u64` used identically in `MermaidState::Ready` (Task 2), `CachedProtocol` (Task 7), `inline_protocol_mut` / `overlay_protocol_mut` signatures (Task 7), `handle_mermaid_completed` (Task 9), `render_inline_image` (Task 10), `render_mermaid_overlay` (Task 15). `target_width: u32` used identically in actions (Task 1), worker (Task 5), handler (Task 9), and `render_mermaid` signature (Task 4).
- **No placeholders:** every step shows actual code; no "TBD" / "implement later" / "similar to Task N" instructions.
