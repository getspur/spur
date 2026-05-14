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
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use image::DynamicImage;
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};

use crate::components::mermaid::MermaidId;
use crate::components::react_trace::types::TraceImageId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ImageCacheKey {
    Mermaid(MermaidId),
    Trace(TraceImageId),
    MermaidSlice {
        id: MermaidId,
        first_row_within: u16,
        run_len: u16,
        total_rows: u16,
    },
    TraceSlice {
        id: TraceImageId,
        first_row_within: u16,
        run_len: u16,
        total_rows: u16,
    },
}

const MAX_CROPPED_SLICES: usize = 128;
const MAX_INLINE_SLICE_PROTOCOLS: usize = 128;
const MAX_FULL_IMAGE_PROTOCOLS: usize = 16;
const MAX_DISPLAY_SURFACES: usize = 16;
const PERF_SAMPLE_BATCH: usize = 200;

struct DurationSampler {
    samples: [u64; PERF_SAMPLE_BATCH],
    len: usize,
}

impl DurationSampler {
    fn new() -> Self {
        Self {
            samples: [0; PERF_SAMPLE_BATCH],
            len: 0,
        }
    }

    fn observe_and_maybe_flush(&mut self, duration_us: u64) -> Option<(u64, u64)> {
        self.samples[self.len] = duration_us;
        self.len += 1;
        if self.len < PERF_SAMPLE_BATCH {
            return None;
        }
        let mut values = self.samples;
        values.sort_unstable();
        let p50 = values[(PERF_SAMPLE_BATCH - 1) / 2];
        let p95 = values[((PERF_SAMPLE_BATCH - 1) * 95) / 100];
        self.len = 0;
        Some((p50, p95))
    }
}

struct PerfStats {
    enabled: bool,
    slice_hit: AtomicU64,
    slice_miss: AtomicU64,
    crop_slice_hit: AtomicU64,
    crop_slice_miss: AtomicU64,
    display_surface_hit: AtomicU64,
    display_surface_miss: AtomicU64,
    evictions_slice: AtomicU64,
    evictions_cropped_slice: AtomicU64,
    evictions_display_surface: AtomicU64,
    evictions_full: AtomicU64,
    resize_protocol_sampler: DurationSampler,
    resize_surface_sampler: DurationSampler,
}

impl PerfStats {
    fn new() -> Self {
        Self {
            enabled: std::env::var("SPUR_TUI_IMG_PERF").as_deref() == Ok("1"),
            slice_hit: AtomicU64::new(0),
            slice_miss: AtomicU64::new(0),
            crop_slice_hit: AtomicU64::new(0),
            crop_slice_miss: AtomicU64::new(0),
            display_surface_hit: AtomicU64::new(0),
            display_surface_miss: AtomicU64::new(0),
            evictions_slice: AtomicU64::new(0),
            evictions_cropped_slice: AtomicU64::new(0),
            evictions_display_surface: AtomicU64::new(0),
            evictions_full: AtomicU64::new(0),
            resize_protocol_sampler: DurationSampler::new(),
            resize_surface_sampler: DurationSampler::new(),
        }
    }

    fn emit_summary(&self, reason: &str) {
        tracing::info!(
            target: "spur_tui::img_perf",
            reason,
            slice_hit = self.slice_hit.load(Ordering::Relaxed),
            slice_miss = self.slice_miss.load(Ordering::Relaxed),
            crop_slice_hit = self.crop_slice_hit.load(Ordering::Relaxed),
            crop_slice_miss = self.crop_slice_miss.load(Ordering::Relaxed),
            display_surface_hit = self.display_surface_hit.load(Ordering::Relaxed),
            display_surface_miss = self.display_surface_miss.load(Ordering::Relaxed),
            evictions_slice = self.evictions_slice.load(Ordering::Relaxed),
            evictions_cropped_slice = self.evictions_cropped_slice.load(Ordering::Relaxed),
            evictions_display_surface = self.evictions_display_surface.load(Ordering::Relaxed),
            evictions_full = self.evictions_full.load(Ordering::Relaxed),
            "image cache perf summary"
        );
    }
}

impl ImageCacheKey {
    fn is_slice(self) -> bool {
        matches!(
            self,
            ImageCacheKey::MermaidSlice { .. } | ImageCacheKey::TraceSlice { .. }
        )
    }
}

struct CachedProtocol {
    proto: StatefulProtocol,
    /// Snapshot of `MermaidState::Ready.image_generation` when this
    /// protocol was built. Compared on every fetch — a mismatch means
    /// the underlying image was replaced and the protocol is stale.
    image_generation: u64,
    surface_w_px: u32,
    surface_h_px: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplaySurfaceSource {
    Mermaid(MermaidId),
    Trace(TraceImageId),
}

impl From<MermaidId> for DisplaySurfaceSource {
    fn from(id: MermaidId) -> Self {
        Self::Mermaid(id)
    }
}

impl From<TraceImageId> for DisplaySurfaceSource {
    fn from(id: TraceImageId) -> Self {
        Self::Trace(id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DisplaySurfaceKey {
    source: DisplaySurfaceSource,
    image_generation: u64,
    pane_w_cols: u16,
    cell_w_px: u16,
    cell_h_px: u16,
    total_rows: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SliceKey {
    source: DisplaySurfaceSource,
    image_generation: u64,
    first_row_within: u16,
    run_len: u16,
    total_rows: u16,
    cell_h_px: u16,
}

impl SliceKey {
    pub fn new<I>(
        id: I,
        image_generation: u64,
        first_row_within: u16,
        run_len: u16,
        total_rows: u16,
        cell_h_px: u16,
    ) -> Self
    where
        I: Into<DisplaySurfaceSource>,
    {
        Self {
            source: id.into(),
            image_generation,
            first_row_within,
            run_len,
            total_rows,
            cell_h_px,
        }
    }
}

pub struct ImageCache {
    inline: HashMap<ImageCacheKey, CachedProtocol>,
    overlay: HashMap<ImageCacheKey, CachedProtocol>,
    slice_protocol_order: VecDeque<ImageCacheKey>,
    cropped_slices: HashMap<SliceKey, Arc<DynamicImage>>,
    cropped_slice_order: VecDeque<SliceKey>,
    display_surfaces: HashMap<DisplaySurfaceKey, Arc<DynamicImage>>,
    display_surface_order: VecDeque<DisplaySurfaceKey>,
    /// Cell pixel size the current entries were built against. None ⇔
    /// both maps are empty. Drift triggers a full clear.
    last_cell_size: Option<(u16, u16)>,
    perf: PerfStats,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self {
            inline: HashMap::new(),
            overlay: HashMap::new(),
            slice_protocol_order: VecDeque::new(),
            cropped_slices: HashMap::new(),
            cropped_slice_order: VecDeque::new(),
            display_surfaces: HashMap::new(),
            display_surface_order: VecDeque::new(),
            last_cell_size: None,
            perf: PerfStats::new(),
        }
    }
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
            &mut self.slice_protocol_order,
            &mut self.cropped_slices,
            &mut self.cropped_slice_order,
            &mut self.display_surfaces,
            &mut self.display_surface_order,
            &mut self.last_cell_size,
            picker,
        );
        Self::enforce_full_protocol_limit(&mut self.inline, ImageCacheKey::Mermaid(id), &self.perf);
        Self::get_or_build(
            &mut self.inline,
            ImageCacheKey::Mermaid(id),
            image,
            image_generation,
            picker,
            &mut self.perf,
        )
    }

    pub fn inline_trace_protocol_mut(
        &mut self,
        id: TraceImageId,
        image: &Arc<DynamicImage>,
        image_generation: u64,
        picker: &Picker,
    ) -> &mut StatefulProtocol {
        Self::ensure_cell_size(
            &mut self.inline,
            &mut self.overlay,
            &mut self.slice_protocol_order,
            &mut self.cropped_slices,
            &mut self.cropped_slice_order,
            &mut self.display_surfaces,
            &mut self.display_surface_order,
            &mut self.last_cell_size,
            picker,
        );
        Self::enforce_full_protocol_limit(&mut self.inline, ImageCacheKey::Trace(id), &self.perf);
        Self::get_or_build(
            &mut self.inline,
            ImageCacheKey::Trace(id),
            image,
            image_generation,
            picker,
            &mut self.perf,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn inline_mermaid_slice_protocol_mut(
        &mut self,
        id: MermaidId,
        image: &Arc<DynamicImage>,
        image_generation: u64,
        first_row_within: u16,
        run_len: u16,
        total_rows: u16,
        picker: &Picker,
    ) -> &mut StatefulProtocol {
        Self::ensure_cell_size(
            &mut self.inline,
            &mut self.overlay,
            &mut self.slice_protocol_order,
            &mut self.cropped_slices,
            &mut self.cropped_slice_order,
            &mut self.display_surfaces,
            &mut self.display_surface_order,
            &mut self.last_cell_size,
            picker,
        );
        let key = ImageCacheKey::MermaidSlice {
            id,
            first_row_within,
            run_len,
            total_rows,
        };
        Self::touch_slice_protocol_key(&mut self.slice_protocol_order, key);
        Self::enforce_inline_slice_limit(
            &mut self.inline,
            &mut self.slice_protocol_order,
            key,
            &self.perf,
        );
        Self::get_or_build(
            &mut self.inline,
            key,
            image,
            image_generation,
            picker,
            &mut self.perf,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn inline_trace_slice_protocol_mut(
        &mut self,
        id: TraceImageId,
        image: &Arc<DynamicImage>,
        image_generation: u64,
        first_row_within: u16,
        run_len: u16,
        total_rows: u16,
        picker: &Picker,
    ) -> &mut StatefulProtocol {
        Self::ensure_cell_size(
            &mut self.inline,
            &mut self.overlay,
            &mut self.slice_protocol_order,
            &mut self.cropped_slices,
            &mut self.cropped_slice_order,
            &mut self.display_surfaces,
            &mut self.display_surface_order,
            &mut self.last_cell_size,
            picker,
        );
        let key = ImageCacheKey::TraceSlice {
            id,
            first_row_within,
            run_len,
            total_rows,
        };
        Self::touch_slice_protocol_key(&mut self.slice_protocol_order, key);
        Self::enforce_inline_slice_limit(
            &mut self.inline,
            &mut self.slice_protocol_order,
            key,
            &self.perf,
        );
        Self::get_or_build(
            &mut self.inline,
            key,
            image,
            image_generation,
            picker,
            &mut self.perf,
        )
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
            &mut self.slice_protocol_order,
            &mut self.cropped_slices,
            &mut self.cropped_slice_order,
            &mut self.display_surfaces,
            &mut self.display_surface_order,
            &mut self.last_cell_size,
            picker,
        );
        Self::enforce_full_protocol_limit(
            &mut self.overlay,
            ImageCacheKey::Mermaid(id),
            &self.perf,
        );
        Self::get_or_build(
            &mut self.overlay,
            ImageCacheKey::Mermaid(id),
            image,
            image_generation,
            picker,
            &mut self.perf,
        )
    }

    /// Drop every protocol. Called on `Event::Resize` (terminal resize),
    /// session reset, or whenever invariants demand a full rebuild.
    pub fn invalidate_all(&mut self) {
        self.inline.clear();
        self.overlay.clear();
        self.slice_protocol_order.clear();
        self.cropped_slices.clear();
        self.cropped_slice_order.clear();
        self.display_surfaces.clear();
        self.display_surface_order.clear();
        self.last_cell_size = None;
    }

    pub fn get_or_build_cropped_slice<F>(
        &mut self,
        key: SliceKey,
        build: F,
    ) -> Option<Arc<DynamicImage>>
    where
        F: FnOnce() -> Option<DynamicImage>,
    {
        if let Some(slice) = self.cropped_slices.get(&key) {
            if self.perf.enabled {
                self.perf.crop_slice_hit.fetch_add(1, Ordering::Relaxed);
            }
            Self::touch_cropped_slice_key(&mut self.cropped_slice_order, key);
            return Some(slice.clone());
        }
        if self.perf.enabled {
            self.perf.crop_slice_miss.fetch_add(1, Ordering::Relaxed);
        }
        let built = Arc::new(build()?);
        Self::touch_cropped_slice_key(&mut self.cropped_slice_order, key);
        Self::enforce_cropped_slice_limit(
            &mut self.cropped_slices,
            &mut self.cropped_slice_order,
            key,
            &self.perf,
        );
        self.cropped_slices.insert(key, built.clone());
        Some(built)
    }

    pub fn has_cropped_slice(&mut self, key: SliceKey) -> bool {
        let hit = self.cropped_slices.contains_key(&key);
        if hit {
            Self::touch_cropped_slice_key(&mut self.cropped_slice_order, key);
        }
        hit
    }

    #[allow(clippy::too_many_arguments)]
    pub fn has_inline_mermaid_slice_protocol(
        &mut self,
        id: MermaidId,
        image_generation: u64,
        first_row_within: u16,
        run_len: u16,
        total_rows: u16,
        surface_w_px: u32,
        surface_h_px: u32,
    ) -> bool {
        let key = ImageCacheKey::MermaidSlice {
            id,
            first_row_within,
            run_len,
            total_rows,
        };
        self.has_inline_slice_protocol(key, image_generation, surface_w_px, surface_h_px)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn has_inline_trace_slice_protocol(
        &mut self,
        id: TraceImageId,
        image_generation: u64,
        first_row_within: u16,
        run_len: u16,
        total_rows: u16,
        surface_w_px: u32,
        surface_h_px: u32,
    ) -> bool {
        let key = ImageCacheKey::TraceSlice {
            id,
            first_row_within,
            run_len,
            total_rows,
        };
        self.has_inline_slice_protocol(key, image_generation, surface_w_px, surface_h_px)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn display_surface<I>(
        &mut self,
        id: I,
        source: &Arc<DynamicImage>,
        image_generation: u64,
        pane_w_cols: u16,
        cell_w_px: u16,
        cell_h_px: u16,
        total_rows: u16,
    ) -> Arc<DynamicImage>
    where
        I: Into<DisplaySurfaceSource>,
    {
        let key = DisplaySurfaceKey {
            source: id.into(),
            image_generation,
            pane_w_cols,
            cell_w_px,
            cell_h_px,
            total_rows,
        };

        if let Some(surface) = self.display_surfaces.get(&key) {
            if self.perf.enabled {
                self.perf
                    .display_surface_hit
                    .fetch_add(1, Ordering::Relaxed);
            }
            Self::touch_display_surface_key(&mut self.display_surface_order, key);
            return surface.clone();
        }
        if self.perf.enabled {
            self.perf
                .display_surface_miss
                .fetch_add(1, Ordering::Relaxed);
            let miss_reason = if self.display_surfaces.contains_key(&key) {
                "new_key"
            } else if self
                .display_surfaces
                .keys()
                .any(|k| k.source == key.source && k.image_generation != key.image_generation)
            {
                "generation_drift"
            } else if self.display_surfaces.keys().any(|k| {
                k.source == key.source
                    && (k.pane_w_cols != key.pane_w_cols
                        || k.cell_w_px != key.cell_w_px
                        || k.cell_h_px != key.cell_h_px
                        || k.total_rows != key.total_rows)
            }) {
                "size_drift"
            } else {
                "new_key"
            };
            tracing::debug!(target: "spur_tui::img_perf", miss_reason, "display surface miss");
        }

        Self::enforce_display_surface_limit(
            &mut self.display_surfaces,
            &mut self.display_surface_order,
            &self.perf,
        );
        let t0 = if self.perf.enabled {
            Some(Instant::now())
        } else {
            None
        };
        let surface = Arc::new(build_display_surface(
            source.as_ref(),
            pane_w_cols,
            cell_w_px,
            cell_h_px,
            total_rows,
        ));
        if let Some(start) = t0 {
            let duration_us = std::cmp::min(start.elapsed().as_micros(), u64::MAX as u128) as u64;
            if let Some((p50, p95)) = self
                .perf
                .resize_surface_sampler
                .observe_and_maybe_flush(duration_us)
            {
                tracing::info!(
                    target: "spur_tui::img_perf",
                    op = "build_display_surface.resize_exact",
                    p50_us = p50,
                    p95_us = p95,
                    sample_count = PERF_SAMPLE_BATCH,
                    "display surface resize timing"
                );
            }
            self.perf.emit_summary("display_surface_miss");
        }
        self.display_surfaces.insert(key, surface.clone());
        Self::touch_display_surface_key(&mut self.display_surface_order, key);
        surface
    }

    /// Drop only the protocols for one id. Available for explicit
    /// memory-hygiene; not required for correctness (auto-invalidation
    /// covers state changes).
    pub fn invalidate_id(&mut self, id: MermaidId) {
        self.inline.remove(&ImageCacheKey::Mermaid(id));
        self.overlay.remove(&ImageCacheKey::Mermaid(id));
        self.inline.retain(|key, _| {
            !matches!(key, ImageCacheKey::MermaidSlice { id: slice_id, .. } if *slice_id == id)
        });
        self.slice_protocol_order.retain(|key| {
            !matches!(key, ImageCacheKey::MermaidSlice { id: slice_id, .. } if *slice_id == id)
        });
        self.cropped_slices
            .retain(|key, _| key.source != DisplaySurfaceSource::Mermaid(id));
        self.cropped_slice_order
            .retain(|key| self.cropped_slices.contains_key(key));
        self.retain_display_surfaces(|key| key.source != DisplaySurfaceSource::Mermaid(id));
    }

    pub fn invalidate_trace_id(&mut self, id: TraceImageId) {
        self.inline.remove(&ImageCacheKey::Trace(id));
        self.overlay.remove(&ImageCacheKey::Trace(id));
        self.inline.retain(|key, _| {
            !matches!(key, ImageCacheKey::TraceSlice { id: slice_id, .. } if *slice_id == id)
        });
        self.slice_protocol_order.retain(
            |key| !matches!(key, ImageCacheKey::TraceSlice { id: slice_id, .. } if *slice_id == id),
        );
        self.cropped_slices
            .retain(|key, _| key.source != DisplaySurfaceSource::Trace(id));
        self.cropped_slice_order
            .retain(|key| self.cropped_slices.contains_key(key));
        self.retain_display_surfaces(|key| key.source != DisplaySurfaceSource::Trace(id));
    }

    /// Test/debug accessor: how many entries each map holds.
    #[cfg(any(test, debug_assertions))]
    pub fn len(&self) -> (usize, usize) {
        (self.inline.len(), self.overlay.len())
    }

    #[cfg(any(test, debug_assertions))]
    pub fn cropped_slice_len(&self) -> usize {
        self.cropped_slices.len()
    }

    #[cfg(test)]
    pub fn display_surface_len(&self) -> usize {
        self.display_surfaces.len()
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
                self.slice_protocol_order.clear();
                self.cropped_slices.clear();
                self.cropped_slice_order.clear();
                self.display_surfaces.clear();
                self.display_surface_order.clear();
                self.last_cell_size = Some(cur);
            }
            None => self.last_cell_size = Some(cur),
        }
    }

    fn get_or_build<'a>(
        map: &'a mut HashMap<ImageCacheKey, CachedProtocol>,
        id: ImageCacheKey,
        image: &Arc<DynamicImage>,
        image_generation: u64,
        picker: &Picker,
        perf: &mut PerfStats,
    ) -> &'a mut StatefulProtocol {
        match map.entry(id) {
            Entry::Occupied(o)
                if o.get().image_generation == image_generation
                    && o.get().surface_w_px == image.width()
                    && o.get().surface_h_px == image.height() =>
            {
                if perf.enabled && id.is_slice() {
                    perf.slice_hit.fetch_add(1, Ordering::Relaxed);
                }
                &mut o.into_mut().proto
            }
            Entry::Occupied(mut o) => {
                let miss_reason = if o.get().image_generation != image_generation {
                    "generation_drift"
                } else {
                    "size_drift"
                };
                if perf.enabled {
                    if id.is_slice() {
                        perf.slice_miss.fetch_add(1, Ordering::Relaxed);
                    }
                    tracing::debug!(target: "spur_tui::img_perf", miss_reason, "protocol miss");
                }
                // Generation or display-surface drift — stale protocol. Rebuild in place.
                let t0 = if perf.enabled {
                    Some(Instant::now())
                } else {
                    None
                };
                *o.get_mut() = CachedProtocol {
                    proto: picker.new_resize_protocol((**image).clone()),
                    image_generation,
                    surface_w_px: image.width(),
                    surface_h_px: image.height(),
                };
                if let Some(start) = t0 {
                    let duration_us =
                        std::cmp::min(start.elapsed().as_micros(), u64::MAX as u128) as u64;
                    if let Some((p50, p95)) = perf
                        .resize_protocol_sampler
                        .observe_and_maybe_flush(duration_us)
                    {
                        tracing::info!(
                            target: "spur_tui::img_perf",
                            op = "new_resize_protocol",
                            p50_us = p50,
                            p95_us = p95,
                            sample_count = PERF_SAMPLE_BATCH,
                            "protocol rebuild timing"
                        );
                    }
                }
                &mut o.into_mut().proto
            }
            Entry::Vacant(v) => {
                if perf.enabled {
                    if id.is_slice() {
                        perf.slice_miss.fetch_add(1, Ordering::Relaxed);
                    }
                    tracing::debug!(target: "spur_tui::img_perf", miss_reason = "new_key", "protocol miss");
                }
                let t0 = if perf.enabled {
                    Some(Instant::now())
                } else {
                    None
                };
                let inserted = v.insert(CachedProtocol {
                    proto: picker.new_resize_protocol((**image).clone()),
                    image_generation,
                    surface_w_px: image.width(),
                    surface_h_px: image.height(),
                });
                if let Some(start) = t0 {
                    let duration_us =
                        std::cmp::min(start.elapsed().as_micros(), u64::MAX as u128) as u64;
                    if let Some((p50, p95)) = perf
                        .resize_protocol_sampler
                        .observe_and_maybe_flush(duration_us)
                    {
                        tracing::info!(
                            target: "spur_tui::img_perf",
                            op = "new_resize_protocol",
                            p50_us = p50,
                            p95_us = p95,
                            sample_count = PERF_SAMPLE_BATCH,
                            "protocol build timing"
                        );
                    }
                }
                &mut inserted.proto
            }
        }
    }

    fn has_inline_slice_protocol(
        &mut self,
        key: ImageCacheKey,
        image_generation: u64,
        surface_w_px: u32,
        surface_h_px: u32,
    ) -> bool {
        let Some(cached) = self.inline.get(&key) else {
            return false;
        };
        let hit = cached.image_generation == image_generation
            && cached.surface_w_px == surface_w_px
            && cached.surface_h_px == surface_h_px;
        if hit {
            Self::touch_slice_protocol_key(&mut self.slice_protocol_order, key);
        }
        hit
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_cell_size(
        inline: &mut HashMap<ImageCacheKey, CachedProtocol>,
        overlay: &mut HashMap<ImageCacheKey, CachedProtocol>,
        slice_protocol_order: &mut VecDeque<ImageCacheKey>,
        cropped_slices: &mut HashMap<SliceKey, Arc<DynamicImage>>,
        cropped_slice_order: &mut VecDeque<SliceKey>,
        display_surfaces: &mut HashMap<DisplaySurfaceKey, Arc<DynamicImage>>,
        display_surface_order: &mut VecDeque<DisplaySurfaceKey>,
        last: &mut Option<(u16, u16)>,
        picker: &Picker,
    ) {
        let cur = picker.font_size();
        match *last {
            Some(prev) if prev == cur => {}
            Some(_) => {
                inline.clear();
                overlay.clear();
                slice_protocol_order.clear();
                cropped_slices.clear();
                cropped_slice_order.clear();
                display_surfaces.clear();
                display_surface_order.clear();
                *last = Some(cur);
            }
            None => *last = Some(cur),
        }
    }

    fn enforce_inline_slice_limit(
        inline: &mut HashMap<ImageCacheKey, CachedProtocol>,
        order: &mut VecDeque<ImageCacheKey>,
        current: ImageCacheKey,
        perf: &PerfStats,
    ) {
        if inline.contains_key(&current) {
            return;
        }

        while order.len() > MAX_INLINE_SLICE_PROTOCOLS {
            let Some(key) = order.pop_front() else {
                break;
            };
            if key == current || !key.is_slice() {
                continue;
            }
            inline.remove(&key);
            if perf.enabled {
                perf.evictions_slice.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn enforce_cropped_slice_limit(
        cropped_slices: &mut HashMap<SliceKey, Arc<DynamicImage>>,
        order: &mut VecDeque<SliceKey>,
        current: SliceKey,
        perf: &PerfStats,
    ) {
        while cropped_slices.len() >= MAX_CROPPED_SLICES {
            let Some(key) = order.pop_front() else {
                break;
            };
            if key == current {
                continue;
            }
            cropped_slices.remove(&key);
            if perf.enabled {
                perf.evictions_cropped_slice.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn touch_slice_protocol_key(order: &mut VecDeque<ImageCacheKey>, key: ImageCacheKey) {
        if let Some(pos) = order.iter().position(|k| *k == key) {
            order.remove(pos);
        }
        order.push_back(key);
    }

    fn touch_cropped_slice_key(order: &mut VecDeque<SliceKey>, key: SliceKey) {
        if let Some(pos) = order.iter().position(|k| *k == key) {
            order.remove(pos);
        }
        order.push_back(key);
    }

    fn touch_display_surface_key(order: &mut VecDeque<DisplaySurfaceKey>, key: DisplaySurfaceKey) {
        if let Some(pos) = order.iter().position(|k| *k == key) {
            order.remove(pos);
        }
        order.push_back(key);
    }

    fn enforce_full_protocol_limit(
        inline: &mut HashMap<ImageCacheKey, CachedProtocol>,
        current: ImageCacheKey,
        perf: &PerfStats,
    ) {
        if inline.contains_key(&current) {
            return;
        }

        let full_count = inline.keys().filter(|key| !key.is_slice()).count();
        if full_count < MAX_FULL_IMAGE_PROTOCOLS {
            return;
        }

        let remove_count = full_count + 1 - MAX_FULL_IMAGE_PROTOCOLS;
        let stale_keys: Vec<_> = inline
            .keys()
            .filter(|key| !key.is_slice() && **key != current)
            .copied()
            .take(remove_count)
            .collect();

        for key in stale_keys {
            inline.remove(&key);
            if perf.enabled {
                perf.evictions_full.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn enforce_display_surface_limit(
        surfaces: &mut HashMap<DisplaySurfaceKey, Arc<DynamicImage>>,
        order: &mut VecDeque<DisplaySurfaceKey>,
        perf: &PerfStats,
    ) {
        while surfaces.len() >= MAX_DISPLAY_SURFACES {
            let Some(key) = order.pop_front() else {
                break;
            };
            surfaces.remove(&key);
            if perf.enabled {
                perf.evictions_display_surface
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn retain_display_surfaces(&mut self, mut keep: impl FnMut(&DisplaySurfaceKey) -> bool) {
        self.display_surfaces.retain(|key, _| keep(key));
        self.display_surface_order
            .retain(|key| self.display_surfaces.contains_key(key));
    }
}

fn build_display_surface(
    source: &DynamicImage,
    pane_w_cols: u16,
    cell_w_px: u16,
    cell_h_px: u16,
    total_rows: u16,
) -> DynamicImage {
    use image::{
        imageops::{self, FilterType},
        ImageBuffer, Rgba,
    };

    let target_w = (pane_w_cols as u32)
        .saturating_mul(cell_w_px.max(1) as u32)
        .max(1);
    let target_h = (total_rows as u32)
        .saturating_mul(cell_h_px.max(1) as u32)
        .max(1);

    if source.width() == 0 || source.height() == 0 {
        return DynamicImage::new_rgba8(target_w, target_h);
    }

    let scale_w = target_w as f64 / source.width() as f64;
    let scale_h = target_h as f64 / source.height() as f64;
    let scale = scale_w.min(scale_h);
    let content_w = ((source.width() as f64 * scale).round() as u32)
        .clamp(1, target_w)
        .max(1);
    let content_h = ((source.height() as f64 * scale).round() as u32)
        .clamp(1, target_h)
        .max(1);

    let resized = source.resize_exact(content_w, content_h, FilterType::Triangle);
    let mut canvas: DynamicImage =
        ImageBuffer::from_pixel(target_w, target_h, Rgba([0u8, 0, 0, 0])).into();
    let x = ((target_w - content_w) / 2) as i64;
    let y = ((target_h - content_h) / 2) as i64;
    imageops::overlay(&mut canvas, &resized, x, y);
    canvas
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GenericImageView, Rgba, RgbaImage};
    use ratatui_image::picker::Picker;

    fn small_image() -> Arc<DynamicImage> {
        Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)))
    }

    fn picker() -> Picker {
        Picker::halfblocks()
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
        let (cell_width, cell_height) = p.font_size();
        c.check_cell_size_with((cell_width + 1, cell_height + 2));
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

    #[test]
    fn partial_inline_slices_use_distinct_cache_keys() {
        let mut c = ImageCache::new();
        let img = small_image();
        let p = picker();

        c.inline_trace_slice_protocol_mut(TraceImageId(1), &img, 1, 0, 4, 10, &p);
        c.inline_trace_slice_protocol_mut(TraceImageId(1), &img, 1, 4, 4, 10, &p);
        c.inline_trace_slice_protocol_mut(TraceImageId(1), &img, 1, 4, 4, 10, &p);

        assert_eq!(
            c.len(),
            (2, 0),
            "two distinct scroll slices should not reuse one protocol"
        );
    }

    #[test]
    fn partial_inline_slices_are_bounded() {
        let mut c = ImageCache::new();
        let img = small_image();
        let p = picker();

        for first_row in 0..(MAX_INLINE_SLICE_PROTOCOLS as u16 + 8) {
            c.inline_trace_slice_protocol_mut(TraceImageId(1), &img, 1, first_row, 4, 100, &p);
        }

        assert_eq!(
            c.len(),
            (MAX_INLINE_SLICE_PROTOCOLS, 0),
            "scrolling should not cache every historical slice"
        );
    }

    #[test]
    fn inline_slice_hits_at_capacity_do_not_evict() {
        let mut c = ImageCache::new();
        let img = small_image();
        let p = picker();
        c.perf.enabled = true;

        for first_row in 0..(MAX_INLINE_SLICE_PROTOCOLS as u16) {
            c.inline_trace_slice_protocol_mut(TraceImageId(1), &img, 1, first_row, 4, 256, &p);
        }

        let inline_slice_count =
            |cache: &ImageCache| cache.inline.iter().filter(|(k, _)| k.is_slice()).count();
        assert_eq!(inline_slice_count(&c), MAX_INLINE_SLICE_PROTOCOLS);

        for i in 0..200 {
            let first_row = (i % MAX_INLINE_SLICE_PROTOCOLS) as u16;
            c.inline_trace_slice_protocol_mut(TraceImageId(1), &img, 1, first_row, 4, 256, &p);
            assert_eq!(
                inline_slice_count(&c),
                MAX_INLINE_SLICE_PROTOCOLS,
                "slice count should stay at capacity on hit {i}"
            );
        }

        assert_eq!(c.perf.evictions_slice.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn display_surface_uses_full_render_dimensions() {
        let mut c = ImageCache::new();
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(100, 50)));

        let surface = c.display_surface(MermaidId(1), &img, 1, 10, 8, 16, 4);

        assert_eq!(surface.width(), 80);
        assert_eq!(surface.height(), 64);
        assert_eq!(c.display_surface_len(), 1);
    }

    #[test]
    fn display_surface_letterboxes_width_constrained_sources() {
        let mut c = ImageCache::new();
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            1200,
            400,
            Rgba([240, 32, 16, 255]),
        )));

        let surface = c.display_surface(MermaidId(1), &img, 1, 10, 10, 10, 10);

        assert_eq!(surface.width(), 100);
        assert_eq!(surface.height(), 100);
        assert_eq!(surface.get_pixel(50, 10), Rgba([0, 0, 0, 0]));
        assert_ne!(surface.get_pixel(50, 50), Rgba([0, 0, 0, 0]));
    }

    #[test]
    fn display_surfaces_are_bounded() {
        let mut c = ImageCache::new();
        let img = small_image();

        for id in 0..(MAX_DISPLAY_SURFACES as u64 + 1) {
            c.display_surface(TraceImageId(id), &img, 1, 10, 8, 16, 4);
        }

        assert_eq!(c.display_surface_len(), MAX_DISPLAY_SURFACES);
    }

    #[test]
    fn display_surfaces_use_lru_eviction_with_touch_on_read() {
        let mut c = ImageCache::new();
        let img = small_image();

        for id in 0..(MAX_DISPLAY_SURFACES as u64) {
            c.display_surface(TraceImageId(id), &img, 1, 10, 8, 16, 4);
        }

        // Touch id=0 so id=1 becomes least-recently-used.
        c.display_surface(TraceImageId(0), &img, 1, 10, 8, 16, 4);
        c.display_surface(
            TraceImageId(MAX_DISPLAY_SURFACES as u64),
            &img,
            1,
            10,
            8,
            16,
            4,
        );

        assert_eq!(c.display_surface_len(), MAX_DISPLAY_SURFACES);
        assert!(!c
            .display_surfaces
            .keys()
            .any(|k| matches!(k.source, DisplaySurfaceSource::Trace(TraceImageId(1)))));
        assert!(c
            .display_surfaces
            .keys()
            .any(|k| matches!(k.source, DisplaySurfaceSource::Trace(TraceImageId(0)))));
    }

    #[test]
    fn full_inline_protocols_are_bounded() {
        let mut c = ImageCache::new();
        let img = small_image();
        let p = picker();

        for id in 0..(MAX_FULL_IMAGE_PROTOCOLS as u64 + 1) {
            c.inline_trace_protocol_mut(TraceImageId(id), &img, 1, &p);
        }

        assert_eq!(c.len(), (MAX_FULL_IMAGE_PROTOCOLS, 0));
    }

    #[test]
    fn same_generation_protocol_rebuilds_when_surface_dimensions_change() {
        let mut c = ImageCache::new();
        let id = TraceImageId(9);
        let p = picker();
        let first = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(80, 40)));
        let second = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(120, 40)));

        c.inline_trace_protocol_mut(id, &first, 1, &p);
        c.inline_trace_protocol_mut(id, &second, 1, &p);

        let cached = c
            .inline
            .get(&ImageCacheKey::Trace(id))
            .expect("cached protocol");
        assert_eq!((cached.surface_w_px, cached.surface_h_px), (120, 40));
    }

    #[test]
    fn invalidate_trace_id_removes_full_slice_and_surface_entries() {
        let mut c = ImageCache::new();
        let img = small_image();
        let p = picker();
        let id = TraceImageId(7);

        c.inline_trace_protocol_mut(id, &img, 1, &p);
        c.inline_trace_slice_protocol_mut(id, &img, 1, 2, 4, 10, &p);
        c.display_surface(id, &img, 1, 10, 8, 16, 10);
        assert_eq!(c.len(), (2, 0));
        assert_eq!(c.display_surface_len(), 1);

        c.invalidate_trace_id(id);

        assert_eq!(c.len(), (0, 0));
        assert_eq!(c.display_surface_len(), 0);
    }
}
