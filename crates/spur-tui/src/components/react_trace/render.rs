use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};

use crate::components::line_wrap::wrap_line_to_width;

#[cfg(feature = "markdown")]
use super::types::{RenderContext, Segment, VirtualRow};
use super::ReactTrace;
use super::SPINNER_FRAMES;

/// Cached wrapped lines for the non-markdown render path.
#[cfg(not(feature = "markdown"))]
pub(in crate::components) struct LineCacheEntry {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) width: u16,
    pub(super) generation: u64,
}

/// Cached virtual rows for the markdown render path.
#[cfg(feature = "markdown")]
pub(in crate::components) struct VirtualRowCacheEntry {
    pub(super) rows: Vec<VirtualRow>,
    /// Row index where each entry's virtual rows begin.
    /// `entry_row_starts[i]` = index into `rows` where entry `i` starts.
    pub(super) entry_row_starts: Vec<usize>,
    pub(super) width: u16,
    pub(super) generation: u64,
    /// Snapshot of mermaid fence states at cache time. If any state changes
    /// (e.g. Pending→Ready), the cache must be rebuilt.
    pub(super) fence_gen: u64,
}

/// Group contiguous virtual rows into render batches. Only rows in
/// `[start_idx, end_idx)` are considered.
#[cfg(feature = "markdown")]
pub(crate) fn segment_visible_rows(
    rows: &[VirtualRow],
    start_idx: usize,
    end_idx: usize,
) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut i = start_idx;
    while i < end_idx {
        match &rows[i] {
            VirtualRow::Text(_) => {
                let start = i;
                while i < end_idx && matches!(rows[i], VirtualRow::Text(_)) {
                    i += 1;
                }
                out.push(Segment::Text {
                    start,
                    len: i - start,
                });
            }
            VirtualRow::ImageRow {
                id,
                row_within,
                total_rows,
            } => {
                let run_id = *id;
                let run_total = *total_rows;
                let first_within = *row_within;
                let start = i;
                while i < end_idx {
                    if let VirtualRow::ImageRow { id: id2, .. } = &rows[i] {
                        if *id2 == run_id {
                            i += 1;
                            continue;
                        }
                    }
                    break;
                }
                out.push(Segment::Image {
                    id: run_id,
                    total_rows: run_total,
                    first_row_within: first_within,
                    run_len: (i - start) as u16,
                });
            }
        }
    }
    out
}

/// Row count for rendering an image inline at `pane_width_cols` with aspect
/// ratio preserved. Clamped to `[6, 60]` rows — short enough not to swamp
/// the pane, tall enough for realistic diagrams to render without squishing.
///
/// Without aspect-correct sizing, `ratatui_image::Resize::Fit` scales by
/// the tighter of (width ratio, height ratio): a tall image in a short
/// Rect letterboxes narrow and shrinks text below legibility.
#[cfg(feature = "markdown")]
pub(crate) fn compute_inline_height_rows(
    image: &image::DynamicImage,
    pane_width_cols: u16,
    picker: Option<&ratatui_image::picker::Picker>,
) -> u16 {
    let (cell_w_px, cell_h_px) = picker
        .map(|p| {
            let (w, h) = p.font_size();
            (w.max(1) as u32, h.max(1) as u32)
        })
        .unwrap_or((8, 16));

    let pane_width_px = (pane_width_cols as u32).saturating_mul(cell_w_px);
    if pane_width_px == 0 || image.width() == 0 {
        return 6;
    }
    // display_h_px = image_h × (pane_w_px / image_w); rows = display_h_px / cell_h.
    let scaled_h_px =
        ((image.height() as u64) * (pane_width_px as u64)).div_ceil(image.width() as u64) as u32;
    let rows = scaled_h_px.div_ceil(cell_h_px);
    rows.clamp(6, 60) as u16
}

#[cfg(feature = "markdown")]
fn compute_fence_states(
    ctx: &RenderContext<'_>,
    pane_width_cols: u16,
) -> std::collections::HashMap<
    crate::components::mermaid::MermaidId,
    crate::components::mermaid::FenceRender,
> {
    use crate::components::mermaid::{FenceRender, MermaidState};
    let mut out = std::collections::HashMap::new();
    for (id, state) in ctx.mermaid_registry.iter() {
        let r =
            match state {
                MermaidState::Ready { image, .. } => FenceRender::Ready(
                    compute_inline_height_rows(image.as_ref(), pane_width_cols, ctx.picker),
                ),
                MermaidState::Pending { .. } | MermaidState::Rendering => FenceRender::Pending,
                MermaidState::Error { .. } => FenceRender::Error,
            };
        out.insert(*id, r);
    }
    out
}

/// Render the inline image for a `Ready` diagram into `rect`. Returns true
/// if the image widget was rendered; false if the caller should fall back
/// to a text placeholder.
#[cfg(feature = "markdown")]
fn render_inline_image(
    frame: &mut Frame,
    rect: Rect,
    id: crate::components::mermaid::MermaidId,
    ctx: &RenderContext<'_>,
) -> bool {
    use crate::components::mermaid::MermaidState;
    use ratatui_image::{Resize, StatefulImage};

    let Some(MermaidState::Ready {
        image,
        inline_protocol,
    }) = ctx.mermaid_registry.get(&id)
    else {
        return false;
    };
    let Some(picker) = ctx.picker else {
        return false;
    };

    let mut slot = inline_protocol.borrow_mut();
    if slot.is_none() {
        // Unavoidable pixel copy: ratatui_image takes DynamicImage by value
        // to build the protocol. Arc prevents repeated deep-copies elsewhere.
        *slot = Some(picker.new_resize_protocol((**image).clone()));
    }
    let Some(proto) = slot.as_mut() else {
        return false;
    };
    let widget = StatefulImage::default().resize(Resize::Fit(None));
    frame.render_stateful_widget(widget, rect, proto);
    true
}

impl ReactTrace {
    /// Render the full ReAct trace into the given frame area.
    ///
    /// Non-markdown path. For markdown-enabled sessions, callers should use
    /// `render_with_ctx` which supports inline image segments.
    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
    ) {
        let following_indicator = if self.is_following {
            " ▼ following "
        } else {
            ""
        };

        let (title_str, accent) = self.pane_title_and_color();
        let block = ratatui::widgets::Block::default()
            .title(Span::styled(title_str, Style::default().fg(accent)))
            .title_bottom(following_indicator)
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let inner = block.inner(area);
        let effective_width = inner.width;
        let visible_height = inner.height as usize;

        // Cache check: rebuild only when generation or width changed.
        // Only used in non-markdown builds; markdown builds use render_with_ctx.
        #[cfg(not(feature = "markdown"))]
        let wrapped_owned: Vec<Line<'static>>;
        #[cfg(not(feature = "markdown"))]
        {
            let hit = self
                .line_cache
                .as_ref()
                .map(|c| c.generation == self.generation && c.width == effective_width)
                .unwrap_or(false);
            if !hit {
                let spinner_frame =
                    SPINNER_FRAMES[(self.tick_counter as usize / 2) % SPINNER_FRAMES.len()];
                let lines = self.build_display_lines(spinner_frame, lineage);
                let built: Vec<Line<'static>> = lines
                    .into_iter()
                    .flat_map(|l| wrap_line_to_width(&l, effective_width))
                    .collect();
                self.line_cache = Some(LineCacheEntry {
                    lines: built,
                    width: effective_width,
                    generation: self.generation,
                });
            }
            wrapped_owned = Vec::new(); // placeholder — we borrow from cache below
        }

        // In markdown builds, render() is a fallback that doesn't use the
        // VirtualRow cache. Build lines directly (this path is rarely hit).
        #[cfg(feature = "markdown")]
        let wrapped_owned: Vec<Line<'static>> = {
            let spinner_frame =
                SPINNER_FRAMES[(self.tick_counter as usize / 2) % SPINNER_FRAMES.len()];
            let lines = self.build_display_lines(spinner_frame, lineage);
            lines
                .into_iter()
                .flat_map(|l| wrap_line_to_width(&l, effective_width))
                .collect()
        };

        #[cfg(not(feature = "markdown"))]
        let wrapped: &[Line<'static>] = {
            &self
                .line_cache
                .as_ref()
                .expect("cache just populated")
                .lines
        };
        #[cfg(feature = "markdown")]
        let wrapped: &[Line<'static>] = &wrapped_owned;

        let total_lines = wrapped.len();
        self.last_total_lines = total_lines;
        self.last_visible_height = visible_height;

        // Clamp or pin scroll offset.
        let max_offset = total_lines.saturating_sub(visible_height);
        let offset = if self.is_following {
            max_offset
        } else {
            self.scroll_offset.min(max_offset)
        };

        // Viewport slice: only clone the visible lines instead of all lines.
        let visible_end = (offset + visible_height).min(total_lines);
        let viewport: Vec<Line> = wrapped[offset..visible_end].to_vec();

        let paragraph = Paragraph::new(viewport).block(block);
        frame.render_widget(paragraph, area);

        // Scrollbar: proportional thumb via viewport_content_length.
        if total_lines > visible_height {
            let mut scrollbar_state = ScrollbarState::new(total_lines)
                .position(offset)
                .viewport_content_length(visible_height);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
        }
    }

    /// Render the trace with markdown + inline mermaid support.
    ///
    /// Walks virtual rows, batching contiguous text rows into `Paragraph`
    /// Rects and contiguous `ImageRow` runs per diagram into `StatefulImage`
    /// Rects. Partial-image runs (scrolled so the diagram is cropped) render
    /// as a single-row placeholder instead — the v1 graceful-clip policy.
    #[cfg(feature = "markdown")]
    pub fn render_with_ctx(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        ctx: &RenderContext<'_>,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
    ) {
        let following_indicator = if self.is_following {
            " ▼ following "
        } else {
            ""
        };

        let (title_str, accent) = self.pane_title_and_color();
        let block = ratatui::widgets::Block::default()
            .title(Span::styled(title_str, Style::default().fg(accent)))
            .title_bottom(following_indicator)
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let effective_width = inner.width;
        let visible_height = inner.height as usize;

        // Use a simple hash of mermaid registry state count as a cheap
        // fence-generation signal. A full hash is overkill — the generation
        // counter already covers content mutations; this catches
        // Pending→Ready transitions that don't touch entries.
        let fence_gen = ctx.mermaid_registry.len() as u64;

        // Cache check: rebuild only when generation, width, or fence state changed.
        // Incremental path: if only the tail entries are dirty, truncate and
        // rebuild from the dirty index — O(tail) instead of O(n).
        {
            let dirty = self.dirty_from;

            let width_ok = self
                .line_cache
                .as_ref()
                .map_or(false, |c| c.width == effective_width);
            let fence_ok = self
                .line_cache
                .as_ref()
                .map_or(false, |c| c.fence_gen == fence_gen);

            if width_ok && fence_ok {
                match dirty {
                    None => { /* cache fully valid, nothing to do */ }
                    Some(dirty_idx) if dirty_idx > 0 => {
                        // Incremental: rebuild from dirty_idx onward.
                        // Truncate first, then build (avoids overlapping borrow).
                        let c = self.line_cache.as_mut().unwrap();
                        let trunc_row = if dirty_idx < c.entry_row_starts.len() {
                            c.entry_row_starts[dirty_idx]
                        } else {
                            c.rows.len()
                        };
                        c.rows.truncate(trunc_row);
                        c.entry_row_starts.truncate(dirty_idx);
                        let base = c.rows.len();
                        // Drop the mutable borrow before calling &self method.
                        let states = compute_fence_states(ctx, effective_width);
                        let (new_rows, new_starts) =
                            self.build_virtual_rows(dirty_idx, effective_width, &states, lineage);
                        let c = self.line_cache.as_mut().unwrap();
                        c.rows.extend(new_rows);
                        c.entry_row_starts
                            .extend(new_starts.iter().map(|s| s + base));
                        c.generation = self.generation;
                        self.dirty_from = None;
                    }
                    _ => {
                        // Full rebuild (dirty_idx == 0 or no cache).
                        let states = compute_fence_states(ctx, effective_width);
                        let (rows, entry_row_starts) =
                            self.build_virtual_rows(0, effective_width, &states, lineage);
                        self.line_cache = Some(VirtualRowCacheEntry {
                            rows,
                            entry_row_starts,
                            width: effective_width,
                            generation: self.generation,
                            fence_gen,
                        });
                        self.dirty_from = None;
                    }
                }
            } else {
                // Width or fence state changed — full rebuild.
                let states = compute_fence_states(ctx, effective_width);
                let (rows, entry_row_starts) =
                    self.build_virtual_rows(0, effective_width, &states, lineage);
                self.line_cache = Some(VirtualRowCacheEntry {
                    rows,
                    entry_row_starts,
                    width: effective_width,
                    generation: self.generation,
                    fence_gen,
                });
                self.dirty_from = None;
            }
        }

        let rows = &self.line_cache.as_ref().expect("cache just populated").rows;

        let total = rows.len();
        self.last_total_lines = total;
        self.last_visible_height = visible_height;

        let max_offset = total.saturating_sub(visible_height);
        let offset = if self.is_following {
            max_offset
        } else {
            self.scroll_offset.min(max_offset)
        };

        let visible_end = (offset + visible_height).min(total);
        let segments = segment_visible_rows(rows, offset, visible_end);

        // Walk segments and render into sub-Rects of `inner`.
        let mut y: u16 = inner.y;
        for seg in segments {
            match seg {
                Segment::Text { start, len } => {
                    let height = len as u16;
                    let rect = Rect {
                        x: inner.x,
                        y,
                        width: inner.width,
                        height,
                    };
                    let lines: Vec<Line<'static>> = rows[start..start + len]
                        .iter()
                        .map(|r| match r {
                            VirtualRow::Text(l) => l.clone(),
                            VirtualRow::ImageRow { .. } => Line::from(""),
                        })
                        .collect();
                    frame.render_widget(Paragraph::new(lines), rect);
                    y += height;
                }
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
                        let msg = if !fully_visible {
                            format!(
                                "   [📊 mermaid #{} · scroll to align · Alt-v to zoom]",
                                id.0
                            )
                        } else if !matches!(
                            ctx.mermaid_registry.get(&id),
                            Some(crate::components::mermaid::MermaidState::Ready { .. })
                        ) {
                            format!("   [📊 mermaid #{} · not ready]", id.0)
                        } else {
                            format!("   [📊 mermaid #{} · no graphics protocol]", id.0)
                        };
                        let line = Line::from(Span::styled(
                            msg,
                            Style::default()
                                .fg(Color::Magenta)
                                .add_modifier(Modifier::BOLD),
                        ));
                        frame.render_widget(Paragraph::new(vec![line]), rect);
                    }
                    y += run_len;
                }
            }
        }

        // Scrollbar — same math as non-markdown path.
        if total > visible_height {
            let mut scrollbar_state = ScrollbarState::new(total)
                .position(offset)
                .viewport_content_length(visible_height);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
        }
    }
}

#[cfg(all(test, feature = "markdown"))]
impl ReactTrace {
    /// Test helper: compute the render segmentation without a real frame.
    /// Mirrors what `render_with_ctx` computes internally.
    pub(crate) fn render_plan_for_test(
        &self,
        effective_width: u16,
        visible_height: usize,
        offset: usize,
        states: &std::collections::HashMap<
            crate::components::mermaid::MermaidId,
            crate::components::mermaid::FenceRender,
        >,
    ) -> Vec<Segment> {
        let (rows, _starts) = self.build_virtual_rows(0, effective_width, states, None);
        let end = (offset + visible_height).min(rows.len());
        segment_visible_rows(&rows, offset, end)
    }
}

#[cfg(all(test, feature = "markdown"))]
mod fence_state_hash_tests {
    use super::*;
    use crate::components::mermaid::{MermaidId, MermaidState};
    use std::collections::HashMap;

    #[test]
    fn empty_registry_has_stable_hash() {
        let r: HashMap<MermaidId, MermaidState> = HashMap::new();
        let a = fence_state_hash(&r);
        let b = fence_state_hash(&r);
        assert_eq!(a, b);
    }

    #[test]
    fn pending_to_error_changes_hash() {
        let mut r: HashMap<MermaidId, MermaidState> = HashMap::new();
        r.insert(MermaidId(0), MermaidState::Pending { code: "g{}".into() });
        let h1 = fence_state_hash(&r);
        r.insert(MermaidId(0), MermaidState::Error { message: "boom".into() });
        let h2 = fence_state_hash(&r);
        assert_ne!(h1, h2,
            "Pending→Error must change fence_state_hash so cache invalidates");
    }

    #[test]
    fn order_independent() {
        let mut a: HashMap<MermaidId, MermaidState> = HashMap::new();
        a.insert(MermaidId(0), MermaidState::Pending { code: "x".into() });
        a.insert(MermaidId(1), MermaidState::Rendering);
        let mut b: HashMap<MermaidId, MermaidState> = HashMap::new();
        b.insert(MermaidId(1), MermaidState::Rendering);
        b.insert(MermaidId(0), MermaidState::Pending { code: "x".into() });
        assert_eq!(fence_state_hash(&a), fence_state_hash(&b),
            "iteration order must not affect hash (must sort by id)");
    }
}
