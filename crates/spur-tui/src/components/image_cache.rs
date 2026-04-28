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
}
