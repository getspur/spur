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
use std::sync::Arc;

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

const MAX_INLINE_SLICE_PROTOCOLS: usize = 32;
const MAX_FULL_IMAGE_PROTOCOLS: usize = 16;
const MAX_DISPLAY_SURFACES: usize = 16;

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

#[derive(Default)]
pub struct ImageCache {
    inline: HashMap<ImageCacheKey, CachedProtocol>,
    overlay: HashMap<ImageCacheKey, CachedProtocol>,
    display_surfaces: HashMap<DisplaySurfaceKey, Arc<DynamicImage>>,
    display_surface_order: VecDeque<DisplaySurfaceKey>,
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
            &mut self.display_surfaces,
            &mut self.display_surface_order,
            &mut self.last_cell_size,
            picker,
        );
        Self::enforce_full_protocol_limit(&mut self.inline, ImageCacheKey::Mermaid(id));
        Self::get_or_build(
            &mut self.inline,
            ImageCacheKey::Mermaid(id),
            image,
            image_generation,
            picker,
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
            &mut self.display_surfaces,
            &mut self.display_surface_order,
            &mut self.last_cell_size,
            picker,
        );
        Self::enforce_full_protocol_limit(&mut self.inline, ImageCacheKey::Trace(id));
        Self::get_or_build(
            &mut self.inline,
            ImageCacheKey::Trace(id),
            image,
            image_generation,
            picker,
        )
    }

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
        Self::enforce_inline_slice_limit(&mut self.inline, key);
        Self::get_or_build(&mut self.inline, key, image, image_generation, picker)
    }

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
        Self::enforce_inline_slice_limit(&mut self.inline, key);
        Self::get_or_build(&mut self.inline, key, image, image_generation, picker)
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
            &mut self.display_surfaces,
            &mut self.display_surface_order,
            &mut self.last_cell_size,
            picker,
        );
        Self::enforce_full_protocol_limit(&mut self.overlay, ImageCacheKey::Mermaid(id));
        Self::get_or_build(
            &mut self.overlay,
            ImageCacheKey::Mermaid(id),
            image,
            image_generation,
            picker,
        )
    }

    /// Drop every protocol. Called on `Event::Resize` (terminal resize),
    /// session reset, or whenever invariants demand a full rebuild.
    pub fn invalidate_all(&mut self) {
        self.inline.clear();
        self.overlay.clear();
        self.display_surfaces.clear();
        self.display_surface_order.clear();
        self.last_cell_size = None;
    }

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
            return surface.clone();
        }

        Self::enforce_display_surface_limit(
            &mut self.display_surfaces,
            &mut self.display_surface_order,
        );
        let surface = Arc::new(build_display_surface(
            source.as_ref(),
            pane_w_cols,
            cell_w_px,
            cell_h_px,
            total_rows,
        ));
        self.display_surfaces.insert(key, surface.clone());
        self.display_surface_order.push_back(key);
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
        self.retain_display_surfaces(|key| key.source != DisplaySurfaceSource::Mermaid(id));
    }

    pub fn invalidate_trace_id(&mut self, id: TraceImageId) {
        self.inline.remove(&ImageCacheKey::Trace(id));
        self.overlay.remove(&ImageCacheKey::Trace(id));
        self.inline.retain(|key, _| {
            !matches!(key, ImageCacheKey::TraceSlice { id: slice_id, .. } if *slice_id == id)
        });
        self.retain_display_surfaces(|key| key.source != DisplaySurfaceSource::Trace(id));
    }

    /// Test/debug accessor: how many entries each map holds.
    #[cfg(any(test, debug_assertions))]
    pub fn len(&self) -> (usize, usize) {
        (self.inline.len(), self.overlay.len())
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
    ) -> &'a mut StatefulProtocol {
        match map.entry(id) {
            Entry::Occupied(o)
                if o.get().image_generation == image_generation
                    && o.get().surface_w_px == image.width()
                    && o.get().surface_h_px == image.height() =>
            {
                &mut o.into_mut().proto
            }
            Entry::Occupied(mut o) => {
                // Generation or display-surface drift — stale protocol. Rebuild in place.
                *o.get_mut() = CachedProtocol {
                    proto: picker.new_resize_protocol((**image).clone()),
                    image_generation,
                    surface_w_px: image.width(),
                    surface_h_px: image.height(),
                };
                &mut o.into_mut().proto
            }
            Entry::Vacant(v) => {
                &mut v
                    .insert(CachedProtocol {
                        proto: picker.new_resize_protocol((**image).clone()),
                        image_generation,
                        surface_w_px: image.width(),
                        surface_h_px: image.height(),
                    })
                    .proto
            }
        }
    }

    fn ensure_cell_size(
        inline: &mut HashMap<ImageCacheKey, CachedProtocol>,
        overlay: &mut HashMap<ImageCacheKey, CachedProtocol>,
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
                display_surfaces.clear();
                display_surface_order.clear();
                *last = Some(cur);
            }
            None => *last = Some(cur),
        }
    }

    fn enforce_inline_slice_limit(
        inline: &mut HashMap<ImageCacheKey, CachedProtocol>,
        current: ImageCacheKey,
    ) {
        if inline.contains_key(&current) {
            return;
        }

        let slice_count = inline.keys().filter(|key| key.is_slice()).count();
        if slice_count < MAX_INLINE_SLICE_PROTOCOLS {
            return;
        }

        let remove_count = slice_count + 1 - MAX_INLINE_SLICE_PROTOCOLS;
        let stale_keys: Vec<_> = inline
            .keys()
            .filter(|key| key.is_slice() && **key != current)
            .copied()
            .take(remove_count)
            .collect();

        for key in stale_keys {
            inline.remove(&key);
        }
    }

    fn enforce_full_protocol_limit(
        inline: &mut HashMap<ImageCacheKey, CachedProtocol>,
        current: ImageCacheKey,
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
        }
    }

    fn enforce_display_surface_limit(
        surfaces: &mut HashMap<DisplaySurfaceKey, Arc<DynamicImage>>,
        order: &mut VecDeque<DisplaySurfaceKey>,
    ) {
        while surfaces.len() >= MAX_DISPLAY_SURFACES {
            let Some(key) = order.pop_front() else {
                break;
            };
            surfaces.remove(&key);
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

    let resized = source.resize_exact(content_w, content_h, FilterType::Lanczos3);
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
