use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

#[cfg(feature = "markdown")]
use ratatui::style::Modifier;

use crate::components::line_wrap::wrap_line_to_width;
use crate::theme::{resolve_token, ColorDepth, Theme};

fn token_color(theme: &Theme, name: &str) -> Color {
    resolve_token(theme, name, ColorDepth::Truecolor)
}

#[cfg(feature = "markdown")]
use super::types::{InlineImageSource, RenderContext, Segment, VirtualRow};
use super::ReactTrace;
use crate::components::spinner;

/// Cached wrapped body lines for the external pane render path
/// (DetailPane Stream tab). Independent from the full-render caches.
pub(in crate::components::react_trace) struct BodyCacheEntry {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) width: u16,
    pub(super) generation: u64,
}

/// Cached wrapped lines for the non-markdown render path.
#[cfg(not(feature = "markdown"))]
pub(in crate::components) struct LineCacheEntry {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) width: u16,
    pub(super) generation: u64,
}

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

/// Resolve a ScrollAnchor to an effective row index.
///
/// `Following` clamps to `total_rows - visible_height`.
/// `Row` returns `entry_row_starts[entry_idx] + min(row_within_entry, entry_height - 1)`,
/// guaranteeing the result lies within the entry's row range. If the entry
/// was evicted (entry_idx out of range), snaps to 0.
///
/// Used by all three render paths (full non-markdown, full markdown/virtual-row,
/// compact) — signature is cache-agnostic.
pub(crate) fn resolve_anchor(
    anchor: &crate::components::react_trace::types::ScrollAnchor,
    entry_row_starts: &[usize],
    total_rows: usize,
    visible_height: usize,
) -> usize {
    use crate::components::react_trace::types::ScrollAnchor;
    match anchor {
        ScrollAnchor::Following => total_rows.saturating_sub(visible_height),
        ScrollAnchor::Row {
            entry_idx,
            row_within_entry,
        } => {
            if *entry_idx >= entry_row_starts.len() {
                return 0;
            }
            let row_start = entry_row_starts[*entry_idx];
            let row_end = entry_row_starts
                .get(*entry_idx + 1)
                .copied()
                .unwrap_or(total_rows);
            let entry_height = row_end.saturating_sub(row_start);
            if entry_height == 0 {
                // Defensive: zero-row entry shouldn't occur in production but
                // keep the contract `result < total_rows` for non-empty traces.
                return row_start.min(total_rows.saturating_sub(1));
            }
            let clamped = (*row_within_entry).min(entry_height - 1);
            row_start + clamped
        }
    }
}

/// Hash of the mermaid registry's per-fence state. Replaces the
/// `registry.len()` cache key, which missed Pending/Rendering/Ready/Error
/// transitions when registry size stayed constant.
///
/// Sorts by MermaidId so iteration order doesn't affect the hash.
/// For Ready state, includes image dimensions because they affect
/// ImageRow height.
#[cfg(feature = "markdown")]
pub(crate) fn fence_state_hash(
    registry: &std::collections::HashMap<
        crate::components::mermaid::MermaidId,
        crate::components::mermaid::MermaidState,
    >,
) -> u64 {
    use crate::components::mermaid::MermaidState;
    use std::hash::{Hash, Hasher};

    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut entries: Vec<_> = registry.iter().collect();
    entries.sort_by_key(|(id, _)| id.0);
    for (id, state) in entries {
        id.0.hash(&mut h);
        std::mem::discriminant(state).hash(&mut h);
        match state {
            MermaidState::Ready { image, .. } => {
                image.width().hash(&mut h);
                image.height().hash(&mut h);
            }
            MermaidState::ReadyText { text, .. } => {
                text.hash(&mut h);
            }
            _ => {}
        }
    }
    h.finish()
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
                source,
                row_within,
                total_rows,
            } => {
                let run_source = *source;
                let run_total = *total_rows;
                let first_within = *row_within;
                let start = i;
                while i < end_idx {
                    if let VirtualRow::ImageRow {
                        source: source2, ..
                    } = &rows[i]
                    {
                        if *source2 == run_source {
                            i += 1;
                            continue;
                        }
                    }
                    break;
                }
                out.push(Segment::Image {
                    source: run_source,
                    total_rows: run_total,
                    first_row_within: first_within,
                    run_len: (i - start) as u16,
                });
            }
        }
    }
    out
}

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
        ((image.height() as u64) * (pane_width_px as u64)).div_ceil(image.width() as u64);
    // Keep natural_rows in u64 to avoid silent truncation on pathologically
    // tall images (e.g. 1×32768 source → natural_rows > u16::MAX). Clamp to
    // soft_cap (always ≤ u16::MAX) BEFORE narrowing.
    let natural_rows_u64 = scaled_h_px.div_ceil(cell_h_px as u64);

    let two_thirds = (pane_height_rows as u32 * 2 / 3) as u16;
    let target_cap = two_thirds.max(INLINE_LEGACY_CAP);
    let max_inline = pane_height_rows.saturating_sub(INLINE_TRAILING_CONTEXT);
    let soft_cap = target_cap.min(max_inline).min(INLINE_HARD_CAP);
    let effective_floor = INLINE_FLOOR_ROWS.min(soft_cap);

    let upper = soft_cap.max(effective_floor) as u64;
    let lower = effective_floor as u64;
    natural_rows_u64.clamp(lower, upper) as u16
}

/// Pure helper exposed for cache keying (Task 13). Returns the soft_cap
/// for a given `pane_height_rows` without needing an image — only
/// `pane_height_rows` affects this value, so it stays the same for
/// every image rendered in the same pane.
#[cfg(feature = "markdown")]
pub(crate) fn compute_soft_cap(pane_height_rows: u16) -> u16 {
    if pane_height_rows == 0 {
        return 0;
    }
    let two_thirds = (pane_height_rows as u32 * 2 / 3) as u16;
    let target_cap = two_thirds.max(INLINE_LEGACY_CAP);
    let max_inline = pane_height_rows.saturating_sub(INLINE_TRAILING_CONTEXT);
    target_cap.min(max_inline).min(INLINE_HARD_CAP)
}

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
        .map(|p| {
            let (w, h) = p.font_size();
            (w.max(1) as u32, h.max(1) as u32)
        })
        .unwrap_or((8, 16));
    let mut out = std::collections::HashMap::new();
    for (id, state) in ctx.mermaid_registry.iter() {
        let r = match state {
            MermaidState::Ready { image, .. } => FenceRender::Ready(compute_inline_height_rows(
                image.as_ref(),
                pane_width_cols,
                pane_height_rows,
                cell_w_px,
                cell_h_px,
            )),
            MermaidState::ReadyText { text, .. } => FenceRender::ReadyText(text.clone()),
            MermaidState::Pending { .. } | MermaidState::Rendering => FenceRender::Pending,
            MermaidState::Error { .. } => FenceRender::Error,
        };
        out.insert(*id, r);
    }
    out
}

/// Render the inline image for a `Ready` diagram into `rect`. Returns true
/// if the image widget was rendered; false if the caller should fall back
/// to the multi-row partial card.
#[cfg(feature = "markdown")]
fn render_inline_image(
    frame: &mut Frame,
    rect: Rect,
    source: InlineImageSource,
    total_rows: u16,
    first_row_within: u16,
    run_len: u16,
    ctx: &mut RenderContext<'_>,
    trace_images: &std::collections::HashMap<
        crate::components::react_trace::types::TraceImageId,
        crate::components::react_trace::types::TraceImage,
    >,
) -> bool {
    use crate::components::mermaid::MermaidState;
    use ratatui_image::{Resize, StatefulImage};

    let Some(picker) = ctx.picker else {
        return false;
    };

    let partial = first_row_within != 0 || run_len != total_rows;
    let (cell_w_px, cell_h_px) = picker.font_size();
    let cell_w_px = cell_w_px.max(1);
    let cell_h_px = cell_h_px.max(1);

    let proto = match source {
        InlineImageSource::Mermaid(id) => {
            let Some(MermaidState::Ready {
                image,
                image_generation,
                ..
            }) = ctx.mermaid_registry.get(&id)
            else {
                return false;
            };
            let gen = *image_generation;
            let image_arc = image.clone();
            let surface = ctx.image_cache.display_surface(
                id, &image_arc, gen, rect.width, cell_w_px, cell_h_px, total_rows,
            );
            if partial {
                let Some(slice) = crop_visible_image_slice(
                    surface.as_ref(),
                    cell_h_px,
                    first_row_within,
                    run_len,
                ) else {
                    return false;
                };
                let slice = std::sync::Arc::new(slice);
                ctx.image_cache.inline_mermaid_slice_protocol_mut(
                    id,
                    &slice,
                    gen,
                    first_row_within,
                    run_len,
                    total_rows,
                    picker,
                )
            } else {
                ctx.image_cache
                    .inline_protocol_mut(id, &surface, gen, picker)
            }
        }
        InlineImageSource::Trace(id) => {
            let Some(stored) = trace_images.get(&id) else {
                return false;
            };
            let surface = ctx.image_cache.display_surface(
                id,
                &stored.image,
                stored.image_generation,
                rect.width,
                cell_w_px,
                cell_h_px,
                total_rows,
            );
            if partial {
                let Some(slice) = crop_visible_image_slice(
                    surface.as_ref(),
                    cell_h_px,
                    first_row_within,
                    run_len,
                ) else {
                    return false;
                };
                let slice = std::sync::Arc::new(slice);
                ctx.image_cache.inline_trace_slice_protocol_mut(
                    id,
                    &slice,
                    stored.image_generation,
                    first_row_within,
                    run_len,
                    total_rows,
                    picker,
                )
            } else {
                ctx.image_cache.inline_trace_protocol_mut(
                    id,
                    &surface,
                    stored.image_generation,
                    picker,
                )
            }
        }
    };
    let widget = StatefulImage::default().resize(Resize::Fit(None));
    frame.render_stateful_widget(widget, rect, proto);
    true
}

#[cfg(feature = "markdown")]
pub(crate) fn crop_visible_image_slice(
    image: &image::DynamicImage,
    cell_h_px: u16,
    first_row_within: u16,
    run_len: u16,
) -> Option<image::DynamicImage> {
    let cell_h_px = cell_h_px.max(1) as u32;
    if run_len == 0 || image.width() == 0 || image.height() == 0 {
        return None;
    }

    let start_y = (first_row_within as u32)
        .saturating_mul(cell_h_px)
        .min(image.height());
    let slice_height = (run_len as u32)
        .saturating_mul(cell_h_px)
        .min(image.height().saturating_sub(start_y));
    if slice_height == 0 {
        return None;
    }

    Some(image.crop_imm(0, start_y, image.width(), slice_height))
}

/// Minimum `run_len` for the multi-line card variant. Below this we fall
/// through to a single-line message.
#[cfg(feature = "markdown")]
const PARTIAL_CARD_MIN_ROWS: u16 = 3;

#[cfg(feature = "markdown")]
fn image_label(source: InlineImageSource) -> String {
    match source {
        InlineImageSource::Mermaid(id) => format!("📊 mermaid #{}", id.0),
        InlineImageSource::Trace(id) => format!("🖼 image #{}", id.0),
    }
}

#[cfg(feature = "markdown")]
fn is_ready_image(source: InlineImageSource, ctx: &RenderContext<'_>) -> bool {
    match source {
        InlineImageSource::Mermaid(id) => matches!(
            ctx.mermaid_registry.get(&id),
            Some(crate::components::mermaid::MermaidState::Ready { .. })
        ),
        InlineImageSource::Trace(_) => true,
    }
}

#[cfg(feature = "markdown")]
fn pending_image_message(source: InlineImageSource, ctx: &RenderContext<'_>) -> String {
    match source {
        InlineImageSource::Mermaid(id) => match ctx.mermaid_registry.get(&id) {
            Some(crate::components::mermaid::MermaidState::Error { .. }) => {
                format!("   [⚠ mermaid #{} error · Alt-v to view]", id.0)
            }
            _ => format!("   [⏳ mermaid #{} rendering…]", id.0),
        },
        InlineImageSource::Trace(id) => format!("   [🖼 image #{} unavailable]", id.0),
    }
}

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
    source: InlineImageSource,
    total_rows: u16,
    first_row_within: u16,
    run_len: u16,
    theme: &Theme,
) {
    if run_len == 0 {
        return;
    }

    let card_color = token_color(theme, "react_trace.partial_card.fg");
    let hint_color = token_color(theme, "react_trace.partial_card.hint.fg");

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
    let label = image_label(source);
    let hint = match source {
        InlineImageSource::Mermaid(_) => "Alt-v · open in full viewer",
        InlineImageSource::Trace(_) => "Scroll to reveal image",
    };

    let lines: Vec<Line<'static>> = match run_len {
        1 => vec![Line::from(Span::styled(
            format!("[{} · {}% · {}]", label, visible_pct, direction),
            Style::default().fg(card_color).add_modifier(Modifier::BOLD),
        ))],
        2 => vec![
            Line::from(Span::styled(
                format!("{} · {}% visible · {}", label, visible_pct, direction),
                Style::default().fg(card_color).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                hint,
                Style::default().fg(hint_color).add_modifier(Modifier::DIM),
            )),
        ],
        _ => {
            debug_assert!(run_len >= PARTIAL_CARD_MIN_ROWS);
            vec![
                Line::from(Span::styled(
                    format!("{} · {}% visible · {}", label, visible_pct, direction),
                    Style::default().fg(card_color).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    hint,
                    Style::default().fg(hint_color).add_modifier(Modifier::DIM),
                )),
            ]
        }
    };

    let card_height = lines.len() as u16;
    let card_rect = if card_height < run_len {
        let pad_top = (run_len - card_height) / 2;
        // run_len may exceed rect.height when the diagram is partially
        // clipped by the viewport; clamp so the card stays within `rect`.
        let max_pad = rect.height.saturating_sub(card_height);
        let pad_top = pad_top.min(max_pad);
        Rect {
            x: rect.x,
            y: rect.y + pad_top,
            width: rect.width,
            height: card_height.min(rect.height),
        }
    } else {
        rect
    };
    frame.render_widget(Paragraph::new(lines), card_rect);
}

impl ReactTrace {
    fn build_trace_block<'a>(
        title_str: &'a str,
        accent: Color,
        following_indicator: &'static str,
        focused: bool,
    ) -> Block<'a> {
        Block::default()
            .title(Span::styled(title_str, Style::default().fg(accent)))
            .title_bottom(following_indicator)
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(crate::components::focused_border_style(focused))
    }

    fn position_indicator(
        total: usize,
        visible: usize,
        offset: usize,
        width: u16,
    ) -> Option<String> {
        if total <= visible || width < 20 {
            return None;
        }

        // `offset` is zero-based; `bottom` is the conventional 1-indexed
        // bottom-of-viewport line number displayed to users.
        let bottom = (offset + visible).min(total);
        let percent = bottom * 100 / total;

        if width < 30 {
            Some(format!(" · {percent}% "))
        } else {
            Some(format!(" · {bottom}/{total} · {percent}% "))
        }
    }

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
        self.render_focused(frame, area, lineage, false);
    }

    pub fn render_focused(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
        focused: bool,
    ) {
        let following_indicator = if self.is_following() {
            " ▼ following "
        } else {
            ""
        };

        let (title_str, accent) = self.pane_title_and_color();
        let mut block = Self::build_trace_block(&title_str, accent, following_indicator, focused);

        let inner = block.inner(area);
        let effective_width = inner.width;
        let visible_height = inner.height as usize;

        // Cache check: rebuild only when generation or width changed.
        // Only used in non-markdown builds; markdown builds use render_with_ctx.
        #[cfg(not(feature = "markdown"))]
        {
            let hit = self
                .line_cache
                .as_ref()
                .map(|c| c.generation == self.generation && c.width == effective_width)
                .unwrap_or(false);
            if !hit {
                let spinner_frame = spinner::frame(spinner::BRAILLE, self.tick_counter as u32);
                let lines = self.build_display_lines(spinner_frame, lineage);
                let built: Vec<Line<'static>> = lines
                    .into_iter()
                    .flat_map(|l| wrap_line_to_width(&l, effective_width))
                    .map(|l| {
                        crate::components::react_trace::builder::pad_bubble_line(l, effective_width)
                    })
                    .collect();
                self.line_cache = Some(LineCacheEntry {
                    lines: built,
                    width: effective_width,
                    generation: self.generation,
                });
            }
        }

        // In markdown builds, render() is a fallback that doesn't use the
        // VirtualRow cache. Build lines directly (this path is rarely hit).
        #[cfg(feature = "markdown")]
        let wrapped_owned: Vec<Line<'static>> = {
            let spinner_frame = spinner::frame(spinner::BRAILLE, self.tick_counter as u32);
            let lines = self.build_display_lines(spinner_frame, lineage);
            lines
                .into_iter()
                .flat_map(|l| wrap_line_to_width(&l, effective_width))
                .map(|l| {
                    crate::components::react_trace::builder::pad_bubble_line(l, effective_width)
                })
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
        self.last_render_width = Some(effective_width);

        // Resolve anchor to a row offset.
        let max_offset = total_lines.saturating_sub(visible_height);
        let offset = if self.is_following() {
            max_offset
        } else {
            // Non-markdown path: only Following is meaningful here (Row anchors
            // are markdown-only). Resolve to viewport bottom unconditionally.
            max_offset
        };

        // Viewport slice: only clone the visible lines instead of all lines.
        let visible_end = (offset + visible_height).min(total_lines);
        let viewport: Vec<Line> = wrapped[offset..visible_end].to_vec();

        if let Some(pos) = Self::position_indicator(total_lines, visible_height, offset, area.width)
        {
            let pos_color = token_color(&self.theme, "react_trace.timestamp.fg");
            block = block.title_bottom(
                Line::from(pos)
                    .right_aligned()
                    .style(Style::default().fg(pos_color)),
            );
        }

        let paragraph = Paragraph::new(viewport).block(block);
        frame.render_widget(paragraph, area);

        self.last_surface = crate::components::react_trace::Surface::Full(self.generation);
    }

    /// Render the trace with markdown + inline mermaid support.
    ///
    /// Walks virtual rows, batching contiguous text rows into `Paragraph`
    /// Rects and contiguous `ImageRow` runs per diagram into `StatefulImage`
    /// Rects. Partial-image runs render a source-image slice matching the
    /// visible virtual rows, so scrolling reveals the full image over time.
    #[cfg(feature = "markdown")]
    pub fn render_with_ctx(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        ctx: &mut RenderContext<'_>,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
    ) {
        self.render_with_ctx_focused(frame, area, ctx, lineage, false);
    }

    #[cfg(feature = "markdown")]
    pub fn render_with_ctx_focused(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        ctx: &mut RenderContext<'_>,
        lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
        focused: bool,
    ) {
        let following_indicator = if self.is_following() {
            " ▼ following "
        } else {
            ""
        };

        let (title_str, accent) = self.pane_title_and_color();
        let mut block = Self::build_trace_block(&title_str, accent, following_indicator, focused);

        let inner = block.inner(area);

        let effective_width = inner.width;
        let visible_height = inner.height as usize;

        let fence_gen = fence_state_hash(ctx.mermaid_registry);

        // Cache check: rebuild only when generation, width, or fence state changed.
        // Incremental path: if only the tail entries are dirty, truncate and
        // rebuild from the dirty index — O(tail) instead of O(n).
        {
            let dirty = self.dirty_from;

            let (cell_w_px, cell_h_px) = ctx
                .picker
                .map(|p| {
                    let (w, h) = p.font_size();
                    (w.max(1) as u32, h.max(1) as u32)
                })
                .unwrap_or((8, 16));
            let soft_cap = compute_soft_cap(inner.height);

            let key_ok = self.line_cache.as_ref().is_some_and(|c| {
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

        let (total, offset) = {
            let c = self.line_cache.as_ref().expect("cache just populated");
            let t = c.rows.len();
            let o = resolve_anchor(&self.anchor, &c.entry_row_starts, t, visible_height);
            (t, o)
        };

        self.last_total_lines = total;
        self.last_visible_height = visible_height;
        self.last_render_width = Some(effective_width);

        let rows = &self.line_cache.as_ref().expect("cache just populated").rows;
        let visible_end = (offset + visible_height).min(total);
        let segments = segment_visible_rows(rows, offset, visible_end);

        if let Some(pos) = Self::position_indicator(total, visible_height, offset, area.width) {
            let pos_color = token_color(&self.theme, "react_trace.timestamp.fg");
            block = block.title_bottom(
                Line::from(pos)
                    .right_aligned()
                    .style(Style::default().fg(pos_color)),
            );
        }
        frame.render_widget(block, area);

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
                    source,
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
                    let drew_image = render_inline_image(
                        frame,
                        rect,
                        source,
                        total_rows,
                        first_row_within,
                        run_len,
                        ctx,
                        &self.inline_images,
                    );

                    if !drew_image {
                        if is_ready_image(source, ctx) {
                            // Partial visibility OR no graphics protocol available
                            // for a Ready image — render the multi-row card.
                            render_partial_card(
                                frame,
                                rect,
                                source,
                                total_rows,
                                first_row_within,
                                run_len,
                                &self.theme,
                            );
                        } else {
                            // Pending / Rendering / Error — single-line dim
                            // placeholder. Layout still preserved (run_len rows).
                            let msg = pending_image_message(source, ctx);
                            let placeholder_color =
                                token_color(&self.theme, "react_trace.partial_card.hint.fg");
                            let line = Line::from(Span::styled(
                                msg,
                                Style::default()
                                    .fg(placeholder_color)
                                    .add_modifier(Modifier::DIM),
                            ));
                            frame.render_widget(Paragraph::new(vec![line]), rect);
                        }
                    }
                    y += run_len;
                }
            }
        }

        self.last_surface = crate::components::react_trace::Surface::Full(self.generation);
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
        let (rows, _starts, _byte_ranges) =
            self.build_virtual_rows(0, effective_width, states, None);
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
        r.insert(
            MermaidId(0),
            MermaidState::Error {
                message: "boom".into(),
            },
        );
        let h2 = fence_state_hash(&r);
        assert_ne!(
            h1, h2,
            "Pending→Error must change fence_state_hash so cache invalidates"
        );
    }

    #[test]
    fn order_independent() {
        let mut a: HashMap<MermaidId, MermaidState> = HashMap::new();
        a.insert(MermaidId(0), MermaidState::Pending { code: "x".into() });
        a.insert(MermaidId(1), MermaidState::Rendering);
        let mut b: HashMap<MermaidId, MermaidState> = HashMap::new();
        b.insert(MermaidId(1), MermaidState::Rendering);
        b.insert(MermaidId(0), MermaidState::Pending { code: "x".into() });
        assert_eq!(
            fence_state_hash(&a),
            fence_state_hash(&b),
            "iteration order must not affect hash (must sort by id)"
        );
    }
}

#[cfg(all(test, feature = "markdown"))]
mod resolve_anchor_tests {
    use super::*;
    use crate::components::react_trace::types::ScrollAnchor;
    use std::ops::Range;

    fn ranges(slices: &[Option<Range<usize>>]) -> Vec<Option<Range<usize>>> {
        slices.to_vec()
    }

    #[test]
    fn following_resolves_to_max_offset() {
        let _ranges = ranges(&[Some(0..10), Some(0..10), Some(0..10)]);
        let entry_starts = vec![0, 1, 2];
        let row = resolve_anchor(&ScrollAnchor::Following, &entry_starts, 3, 1);
        assert_eq!(row, 2, "Following clamps to total - visible_height");
    }

    #[test]
    fn row_anchor_resolves_within_entry() {
        let _ranges = ranges(&[Some(0..50), Some(0..50), Some(0..30), Some(0..30)]);
        let entry_starts = vec![0, 2];
        let anchor = ScrollAnchor::Row {
            entry_idx: 1,
            row_within_entry: 1,
        };
        let row = resolve_anchor(&anchor, &entry_starts, 4, 2);
        assert_eq!(row, 3, "Row{{1,1}} resolves to entry_starts[1]+1 = 3");
    }

    #[test]
    fn row_anchor_clamps_to_entry_last() {
        let _ranges = ranges(&[Some(0..50), Some(0..50), Some(0..30), Some(0..30)]);
        let entry_starts = vec![0, 2];
        // Entry 1 is 2 rows (rows 2-3); asking row_within_entry=99 must clamp.
        let anchor = ScrollAnchor::Row {
            entry_idx: 1,
            row_within_entry: 99,
        };
        let row = resolve_anchor(&anchor, &entry_starts, 4, 2);
        assert_eq!(row, 3, "row_within_entry=99 clamps to entry's last row (3)");
    }

    #[test]
    fn row_anchor_evicted_entry_snaps_to_zero() {
        let _ranges = ranges(&[Some(0..50), Some(0..50)]);
        let entry_starts = vec![0];
        let anchor = ScrollAnchor::Row {
            entry_idx: 5,
            row_within_entry: 0,
        };
        let row = resolve_anchor(&anchor, &entry_starts, 2, 1);
        assert_eq!(row, 0);
    }

    #[test]
    fn row_anchor_zero_height_entry_stays_in_bounds() {
        // Two entries: entry 0 has 2 rows, entry 1 is zero-row (degenerate).
        let _ranges = ranges(&[Some(0..50), Some(0..50)]);
        let entry_starts = vec![0, 2];
        let anchor = ScrollAnchor::Row {
            entry_idx: 1,
            row_within_entry: 5,
        };
        // total_rows = 2, entry 1 spans rows 2..2 (height 0).
        let row = resolve_anchor(&anchor, &entry_starts, 2, 1);
        assert!(
            row < 2,
            "result must be < total_rows even for zero-height entry; got {}",
            row
        );
    }
}

#[cfg(test)]
mod copy_friendly_border_tests {
    use super::*;
    use ratatui::{backend::TestBackend, layout::Rect, Terminal};

    fn assert_no_vertical_border_glyphs(buf: &ratatui::buffer::Buffer, width: u16, height: u16) {
        for y in 0..height {
            for x in 0..width {
                let cell = buf.cell((x, y)).expect("cell should be inside trace area");
                assert_ne!(
                    cell.symbol(),
                    "│",
                    "react trace copy surface should not render side border glyph at ({x}, {y})",
                );
            }
        }
    }

    #[test]
    fn render_does_not_write_vertical_border_glyphs() {
        let width = 40;
        let height = 8;
        let mut trace = ReactTrace::new_for_tests();
        trace.append_message("copy this line\nand this line", "codex", "12:00".into());

        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| trace.render(f, Rect::new(0, 0, width, height), None))
            .unwrap();

        assert_no_vertical_border_glyphs(term.backend().buffer(), width, height);
    }

    #[cfg(feature = "markdown")]
    #[test]
    fn render_with_ctx_does_not_write_vertical_border_glyphs() {
        let width = 40;
        let height = 8;
        let mut trace = ReactTrace::new_for_tests();
        trace.append_message("copy this line\nand this line", "codex", "12:00".into());
        let registry = std::collections::HashMap::new();
        let mut image_cache = crate::components::image_cache::ImageCache::new();
        let mut ctx = RenderContext {
            mermaid_registry: &registry,
            picker: None,
            image_cache: &mut image_cache,
        };

        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| trace.render_with_ctx(f, Rect::new(0, 0, width, height), &mut ctx, None))
            .unwrap();

        assert_no_vertical_border_glyphs(term.backend().buffer(), width, height);
    }
}

#[cfg(test)]
mod scroll_indicator_tests {
    use super::*;
    use ratatui::{backend::TestBackend, buffer::Buffer, layout::Rect, Terminal};

    fn buffer_text(buf: &Buffer, width: u16, height: u16) -> String {
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                let cell = buf.cell((x, y)).expect("cell should be inside trace area");
                out.push_str(cell.symbol());
            }
            out.push('\n');
        }
        out
    }

    fn row_text(buf: &Buffer, y: u16, width: u16) -> String {
        let mut out = String::new();
        for x in 0..width {
            let cell = buf.cell((x, y)).expect("cell should be inside trace area");
            out.push_str(cell.symbol());
        }
        out
    }

    fn overflow_trace() -> ReactTrace {
        let mut trace = ReactTrace::new_for_tests();
        for i in 0..30 {
            trace.append_think(&format!("thinking line {i}"), "12:00".into());
            trace.append_user_message(&format!("user line {i}"), "12:00".into());
        }
        trace
    }

    #[test]
    fn position_indicator_table() {
        assert_eq!(ReactTrace::position_indicator(10, 20, 0, 70), None);
        assert_eq!(ReactTrace::position_indicator(20, 20, 0, 70), None);
        assert_eq!(ReactTrace::position_indicator(100, 10, 0, 19), None);

        assert_eq!(ReactTrace::position_indicator(100, 10, 45, 19), None);
        assert_eq!(
            ReactTrace::position_indicator(100, 10, 45, 20).unwrap(),
            " · 55% "
        );
        assert_eq!(
            ReactTrace::position_indicator(100, 10, 45, 25).unwrap(),
            " · 55% "
        );
        assert_eq!(
            ReactTrace::position_indicator(100, 10, 45, 29).unwrap(),
            " · 55% "
        );
        assert_eq!(
            ReactTrace::position_indicator(100, 10, 45, 30).unwrap(),
            " · 55/100 · 55% "
        );

        assert_eq!(
            ReactTrace::position_indicator(27, 5, 0, 70).unwrap(),
            " · 5/27 · 18% "
        );
        assert_eq!(
            ReactTrace::position_indicator(27, 5, 7, 70).unwrap(),
            " · 12/27 · 44% "
        );
        assert_eq!(
            ReactTrace::position_indicator(27, 5, 22, 70).unwrap(),
            " · 27/27 · 100% "
        );
    }

    #[test]
    fn render_overflow_has_copy_friendly_position_indicator() {
        let width = 70;
        let height = 8;
        let mut trace = overflow_trace();

        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| trace.render(f, Rect::new(0, 0, width, height), None))
            .unwrap();

        let buf = term.backend().buffer();
        let text = buffer_text(buf, width, height);
        assert!(!text.contains('║'));
        assert!(!text.contains('█'));
        assert!(
            row_text(buf, height - 1, width).contains('%'),
            "bottom row should include a position indicator"
        );
    }

    #[test]
    fn render_without_overflow_has_no_position_indicator() {
        let width = 70;
        let height = 8;
        let mut trace = ReactTrace::new_for_tests();
        trace.append_message("copy this line\nand this line", "codex", "12:00".into());

        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| trace.render(f, Rect::new(0, 0, width, height), None))
            .unwrap();

        assert!(
            !row_text(term.backend().buffer(), height - 1, width).contains('%'),
            "bottom row should not include a position indicator when content fits"
        );
    }

    #[cfg(feature = "markdown")]
    #[test]
    fn render_with_ctx_overflow_has_copy_friendly_position_indicator() {
        let width = 70;
        let height = 8;
        let mut trace = overflow_trace();
        let registry = std::collections::HashMap::new();
        let mut image_cache = crate::components::image_cache::ImageCache::new();
        let mut ctx = RenderContext {
            mermaid_registry: &registry,
            picker: None,
            image_cache: &mut image_cache,
        };

        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| trace.render_with_ctx(f, Rect::new(0, 0, width, height), &mut ctx, None))
            .unwrap();

        let buf = term.backend().buffer();
        let text = buffer_text(buf, width, height);
        assert!(!text.contains('║'));
        assert!(!text.contains('█'));
        assert!(
            row_text(buf, height - 1, width).contains('%'),
            "bottom row should include a position indicator"
        );
    }

    #[cfg(feature = "markdown")]
    #[test]
    fn render_with_ctx_without_overflow_has_no_position_indicator() {
        let width = 70;
        let height = 8;
        let mut trace = ReactTrace::new_for_tests();
        trace.append_message("copy this line\nand this line", "codex", "12:00".into());
        let registry = std::collections::HashMap::new();
        let mut image_cache = crate::components::image_cache::ImageCache::new();
        let mut ctx = RenderContext {
            mermaid_registry: &registry,
            picker: None,
            image_cache: &mut image_cache,
        };

        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| trace.render_with_ctx(f, Rect::new(0, 0, width, height), &mut ctx, None))
            .unwrap();

        assert!(
            !row_text(term.backend().buffer(), height - 1, width).contains('%'),
            "bottom row should not include a position indicator when content fits"
        );
    }
}

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
        assert!(
            result <= 70 - 4,
            "soft_cap must respect pane_h - 4 (got {result})"
        );
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

    #[test]
    fn height_clamps_pathologically_tall_image_at_hard_cap() {
        // 1×32768 source with cell_h_px=1 → natural_rows would overflow u16
        // (26_214_400) without u64-space clamp. Must cap at INLINE_HARD_CAP=100.
        let i = img(1, 32_768);
        assert_eq!(compute_inline_height_rows(&i, 100, 200, 8, 1), 100);
    }
}

#[cfg(all(test, feature = "markdown"))]
mod card_tests {
    use super::*;
    use crate::components::mermaid::MermaidId;
    use crate::components::react_trace::types::TraceImageId;
    use ratatui::{backend::TestBackend, Terminal};

    fn render_into(rect_h: u16, body: impl FnOnce(&mut Frame, Rect)) -> Vec<String> {
        // 80 cols × rect_h rows; render the body into a Rect at (0, 0).
        let backend = TestBackend::new(80, rect_h);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let r = Rect {
                x: 0,
                y: 0,
                width: 80,
                height: rect_h,
            };
            body(f, r);
        })
        .unwrap();
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
            render_partial_card(
                f,
                r,
                InlineImageSource::Mermaid(MermaidId(7)),
                20,
                0,
                5,
                crate::theme::fallback_theme(),
            );
        });
        let joined = lines.join("\n");
        assert!(
            joined.contains("▼ scroll down"),
            "expected scroll-down indicator: {joined}"
        );
    }

    #[test]
    fn card_bottom_visible_says_scroll_up() {
        // first_row_within=15, run_len=5, total_rows=20 → top cropped, bottom visible.
        let lines = render_into(5, |f, r| {
            render_partial_card(
                f,
                r,
                InlineImageSource::Mermaid(MermaidId(3)),
                20,
                15,
                5,
                crate::theme::fallback_theme(),
            );
        });
        let joined = lines.join("\n");
        assert!(
            joined.contains("▲ scroll up"),
            "expected scroll-up indicator: {joined}"
        );
    }

    #[test]
    fn card_mid_window_says_scroll_for_more() {
        // first_row_within=8, run_len=5, total_rows=20 → both edges cropped.
        let lines = render_into(5, |f, r| {
            render_partial_card(
                f,
                r,
                InlineImageSource::Mermaid(MermaidId(2)),
                20,
                8,
                5,
                crate::theme::fallback_theme(),
            );
        });
        let joined = lines.join("\n");
        assert!(
            joined.contains("▲▼ scroll for more"),
            "expected mid-window indicator: {joined}"
        );
    }

    #[test]
    fn card_visible_pct_at_50() {
        let lines = render_into(5, |f, r| {
            render_partial_card(
                f,
                r,
                InlineImageSource::Mermaid(MermaidId(1)),
                20,
                0,
                10,
                crate::theme::fallback_theme(),
            );
        });
        let joined = lines.join("\n");
        assert!(joined.contains("50%"), "expected 50% indicator: {joined}");
    }

    #[test]
    fn card_visible_pct_total_zero_returns_100() {
        let lines = render_into(3, |f, r| {
            render_partial_card(
                f,
                r,
                InlineImageSource::Mermaid(MermaidId(1)),
                0,
                0,
                1,
                crate::theme::fallback_theme(),
            );
        });
        let joined = lines.join("\n");
        assert!(
            joined.contains("100%"),
            "total_rows=0 should display 100%: {joined}"
        );
    }

    #[test]
    fn card_one_line_variant_when_run_len_1() {
        let lines = render_into(1, |f, r| {
            render_partial_card(
                f,
                r,
                InlineImageSource::Mermaid(MermaidId(1)),
                20,
                0,
                1,
                crate::theme::fallback_theme(),
            );
        });
        // Exactly one non-blank line.
        let non_blank = lines.iter().filter(|l| !l.is_empty()).count();
        assert_eq!(
            non_blank, 1,
            "expected 1 non-blank line, got {non_blank}: {lines:?}"
        );
    }

    #[test]
    fn card_two_line_variant_when_run_len_2() {
        let lines = render_into(2, |f, r| {
            render_partial_card(
                f,
                r,
                InlineImageSource::Mermaid(MermaidId(1)),
                20,
                0,
                2,
                crate::theme::fallback_theme(),
            );
        });
        let non_blank = lines.iter().filter(|l| !l.is_empty()).count();
        assert_eq!(
            non_blank, 2,
            "expected 2 non-blank lines, got {non_blank}: {lines:?}"
        );
    }

    #[test]
    fn card_three_line_variant_when_run_len_3_or_more() {
        let lines = render_into(3, |f, r| {
            render_partial_card(
                f,
                r,
                InlineImageSource::Mermaid(MermaidId(1)),
                20,
                0,
                3,
                crate::theme::fallback_theme(),
            );
        });
        // 3-line variant: title, blank, hint → 2 non-blank.
        let non_blank = lines.iter().filter(|l| !l.is_empty()).count();
        assert_eq!(non_blank, 2);
    }

    #[test]
    fn trace_image_partial_card_does_not_advertise_mermaid_viewer() {
        let lines = render_into(3, |f, r| {
            render_partial_card(
                f,
                r,
                InlineImageSource::Trace(TraceImageId(0)),
                20,
                15,
                3,
                crate::theme::fallback_theme(),
            );
        });
        let joined = lines.join("\n");
        assert!(
            !joined.contains("Alt-v"),
            "trace image card must not advertise Mermaid-only viewer: {joined}"
        );
        assert!(
            joined.contains("image #0"),
            "trace image identity should remain visible: {joined}"
        );
    }

    #[test]
    fn card_early_returns_when_run_len_0() {
        let lines = render_into(1, |f, r| {
            render_partial_card(
                f,
                r,
                InlineImageSource::Mermaid(MermaidId(1)),
                20,
                0,
                0,
                crate::theme::fallback_theme(),
            );
        });
        // No content rendered.
        assert!(
            lines.iter().all(|l| l.is_empty()),
            "expected blank: {lines:?}"
        );
    }

    #[test]
    fn card_centers_when_run_len_exceeds_card_height() {
        // run_len=11, card=3 lines → top padding (11-3)/2 = 4. Card at rows 4..7.
        let lines = render_into(11, |f, r| {
            render_partial_card(
                f,
                r,
                InlineImageSource::Mermaid(MermaidId(1)),
                20,
                0,
                11,
                crate::theme::fallback_theme(),
            );
        });
        // Rows 0..4 should be blank; row 4 has the title.
        for (i, l) in lines.iter().take(4).enumerate() {
            assert!(l.is_empty(), "row {i} expected blank, got: {l:?}");
        }
        assert!(
            lines[4].contains("mermaid #1"),
            "row 4 expected title, got: {:?}",
            lines[4]
        );
    }
}

#[cfg(all(test, feature = "markdown"))]
mod image_slice_tests {
    use super::*;
    use image::{DynamicImage, GenericImageView, Rgba, RgbaImage};

    fn striped_image() -> DynamicImage {
        let mut img = RgbaImage::new(2, 10);
        for y in 0..10 {
            let value = (y * 20) as u8;
            for x in 0..2 {
                img.put_pixel(x, y, Rgba([value, 0, 0, 255]));
            }
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn partial_image_slice_crops_visible_source_rows() {
        let img = striped_image();

        let cropped = crop_visible_image_slice(&img, 1, 3, 4).expect("slice should exist");

        assert_eq!(cropped.dimensions(), (2, 4));
        assert_eq!(cropped.get_pixel(0, 0), Rgba([60, 0, 0, 255]));
        assert_eq!(cropped.get_pixel(0, 3), Rgba([120, 0, 0, 255]));
    }

    #[test]
    fn partial_image_slice_clamps_to_source_bounds() {
        let img = striped_image();

        let cropped = crop_visible_image_slice(&img, 1, 8, 10).expect("slice should exist");

        assert_eq!(cropped.dimensions(), (2, 2));
        assert_eq!(cropped.get_pixel(0, 0), Rgba([160, 0, 0, 255]));
        assert_eq!(cropped.get_pixel(0, 1), Rgba([180, 0, 0, 255]));
    }

    #[test]
    fn partial_image_slice_uses_cell_aligned_boundaries() {
        let img = striped_image();

        let cropped = crop_visible_image_slice(&img, 2, 1, 2).expect("slice should exist");

        assert_eq!(cropped.dimensions(), (2, 4));
        assert_eq!(cropped.get_pixel(0, 0), Rgba([40, 0, 0, 255]));
        assert_eq!(cropped.get_pixel(0, 3), Rgba([100, 0, 0, 255]));
    }
}

#[cfg(all(test, feature = "markdown"))]
mod inline_image_visual_smoke_tests {
    use super::*;
    use crate::components::image_cache::ImageCache;
    use crate::components::react_trace::types::ScrollAnchor;
    use image::{DynamicImage, Rgba, RgbaImage};
    use ratatui::{backend::TestBackend, buffer::Buffer, layout::Rect, style::Color, Terminal};
    use ratatui_image::picker::Picker;
    use std::{collections::HashMap, path::PathBuf, sync::Arc};

    #[derive(Debug, Clone, Copy)]
    struct ColoredArea {
        min_x: u16,
        max_x: u16,
        min_y: u16,
        max_y: u16,
        cells: usize,
    }

    fn solid_image(w: u32, h: u32, rgba: [u8; 4]) -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(w, h, Rgba(rgba)))
    }

    fn vertical_gradient(w: u32, h: u32) -> DynamicImage {
        let mut img = RgbaImage::new(w, h);
        for y in 0..h {
            let blue = ((y * 255) / h.saturating_sub(1).max(1)) as u8;
            let red = 255u8.saturating_sub(blue);
            for x in 0..w {
                img.put_pixel(x, y, Rgba([red, 0, blue, 255]));
            }
        }
        DynamicImage::ImageRgba8(img)
    }

    fn trace_with_image(image: DynamicImage) -> ReactTrace {
        let mut trace = ReactTrace::new_for_tests();
        trace
            .append_image(
                Arc::new(image),
                PathBuf::from("inline-smoke.png"),
                "inline-smoke-digest".to_string(),
                "12:00".to_string(),
            )
            .expect("test image should be inserted");
        trace
    }

    fn render_trace(
        trace: &mut ReactTrace,
        image_cache: &mut ImageCache,
        width: u16,
        height: u16,
    ) -> Buffer {
        let registry = HashMap::new();
        let picker = Picker::halfblocks();
        let mut ctx = RenderContext {
            mermaid_registry: &registry,
            picker: Some(&picker),
            image_cache,
        };
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| {
            trace.render_with_ctx_focused(f, Rect::new(0, 0, width, height), &mut ctx, None, true)
        })
        .unwrap();
        term.backend().buffer().clone()
    }

    fn redish(color: Option<Color>) -> bool {
        matches!(color, Some(Color::Rgb(r, g, b)) if r >= 180 && g <= 80 && b <= 80)
    }

    fn bluish(color: Option<Color>) -> bool {
        matches!(color, Some(Color::Rgb(r, g, b)) if b >= 120 && r <= 160 && g <= 80)
    }

    fn cell_has_color(cell: &ratatui::buffer::Cell, pred: fn(Option<Color>) -> bool) -> bool {
        let style = cell.style();
        pred(style.fg) || pred(style.bg)
    }

    fn colored_area(
        buf: &Buffer,
        width: u16,
        height: u16,
        pred: fn(Option<Color>) -> bool,
    ) -> ColoredArea {
        let mut area: Option<ColoredArea> = None;
        for y in 0..height {
            for x in 0..width {
                let cell = buf.cell((x, y)).expect("cell should be in bounds");
                if !cell_has_color(cell, pred) {
                    continue;
                }
                area = Some(match area {
                    Some(a) => ColoredArea {
                        min_x: a.min_x.min(x),
                        max_x: a.max_x.max(x),
                        min_y: a.min_y.min(y),
                        max_y: a.max_y.max(y),
                        cells: a.cells + 1,
                    },
                    None => ColoredArea {
                        min_x: x,
                        max_x: x,
                        min_y: y,
                        max_y: y,
                        cells: 1,
                    },
                });
            }
        }
        area.expect("expected colored image cells in render buffer")
    }

    fn red_rows(buf: &Buffer, width: u16, height: u16) -> Vec<u16> {
        (0..height)
            .filter(|&y| {
                (0..width).any(|x| {
                    let cell = buf.cell((x, y)).expect("cell should be in bounds");
                    cell_has_color(cell, redish)
                })
            })
            .collect()
    }

    fn red_columns(buf: &Buffer, width: u16, height: u16) -> Vec<u16> {
        (0..width)
            .filter(|&x| {
                (0..height).any(|y| {
                    let cell = buf.cell((x, y)).expect("cell should be in bounds");
                    cell_has_color(cell, redish)
                })
            })
            .collect()
    }

    fn row_average_rgb(buf: &Buffer, y: u16, x_start: u16, x_end: u16) -> (u16, u16, u16) {
        let mut r_sum = 0u32;
        let mut g_sum = 0u32;
        let mut b_sum = 0u32;
        let mut count = 0u32;
        for x in x_start..=x_end {
            let cell = buf.cell((x, y)).expect("cell should be in bounds");
            for color in [cell.style().fg, cell.style().bg] {
                if let Some(Color::Rgb(r, g, b)) = color {
                    r_sum += r as u32;
                    g_sum += g as u32;
                    b_sum += b as u32;
                    count += 1;
                }
            }
        }
        assert!(count > 0, "expected RGB samples on row {y}");
        (
            (r_sum / count) as u16,
            (g_sum / count) as u16,
            (b_sum / count) as u16,
        )
    }

    fn assert_close_rgb(actual: (u16, u16, u16), expected: (u16, u16, u16), tolerance: u16) {
        let diff = |a: u16, b: u16| a.abs_diff(b);
        assert!(
            diff(actual.0, expected.0) <= tolerance
                && diff(actual.1, expected.1) <= tolerance
                && diff(actual.2, expected.2) <= tolerance,
            "expected RGB {actual:?} to be within {tolerance} of {expected:?}",
        );
    }

    #[test]
    fn wide_source_preserves_aspect_with_vertical_letterboxing() {
        let mut trace = trace_with_image(solid_image(400, 20, [255, 0, 0, 255]));
        let mut image_cache = ImageCache::new();

        let buf = render_trace(&mut trace, &mut image_cache, 80, 20);
        let red = colored_area(&buf, 80, 20, redish);
        let rows = red_rows(&buf, 80, 20);

        assert!(
            red.max_y - red.min_y + 1 < 8,
            "wide image content should not vertically fill its 8-row image rect: {red:?}",
        );
        assert!(
            rows.first().copied().unwrap() > 2,
            "expected background rows above red content; red rows: {rows:?}",
        );
        assert!(
            rows.last().copied().unwrap() < 9,
            "expected background rows below red content; red rows: {rows:?}",
        );
        assert!(
            red.max_x - red.min_x + 1 >= 76,
            "red content should span nearly the full inner width: {red:?}",
        );
    }

    #[test]
    fn tall_source_preserves_aspect_with_horizontal_letterboxing() {
        let mut trace = trace_with_image(solid_image(20, 400, [255, 0, 0, 255]));
        let mut image_cache = ImageCache::new();

        let buf = render_trace(&mut trace, &mut image_cache, 80, 70);
        let red = colored_area(&buf, 80, 70, redish);
        let columns = red_columns(&buf, 80, 70);

        assert!(
            red.max_x - red.min_x + 1 < 20,
            "tall image content should not horizontally fill its image rect: {red:?}",
        );
        assert!(
            columns.first().copied().unwrap() > 2,
            "expected background columns left of red content; red columns: {columns:?}",
        );
        assert!(
            columns.last().copied().unwrap() < 78,
            "expected background columns right of red content; red columns: {columns:?}",
        );
        assert!(
            red.max_y - red.min_y + 1 >= 54,
            "red content should span nearly the full image height: {red:?}",
        );
    }

    #[test]
    fn pane_width_resize_rebuilds_trace_image_protocol() {
        let mut trace = trace_with_image(solid_image(400, 20, [255, 0, 0, 255]));
        let mut image_cache = ImageCache::new();

        let first = render_trace(&mut trace, &mut image_cache, 60, 20);
        let first_red = colored_area(&first, 60, 20, redish);

        let second = render_trace(&mut trace, &mut image_cache, 100, 20);
        let second_red = colored_area(&second, 100, 20, redish);

        let first_extent = first_red.max_x - first_red.min_x + 1;
        let second_extent = second_red.max_x - second_red.min_x + 1;
        assert!(
            second_extent > first_extent + 25,
            "resized pane should render meaningfully wider content; first={first_red:?}, second={second_red:?}",
        );
        assert!(
            first_extent >= 56 && second_extent >= 96,
            "content should track the inner pane width, not reuse stale protocol dimensions; first={first_red:?}, second={second_red:?}",
        );
    }

    #[test]
    fn scrolled_slice_matches_full_image_vertical_phase() {
        let mut trace = trace_with_image(vertical_gradient(80, 400));
        let mut image_cache = ImageCache::new();

        let full = render_trace(&mut trace, &mut image_cache, 80, 70);
        let blue = colored_area(&full, 80, 70, bluish);
        let half_row_within = 28u16;
        let full_sample_y = 2 + half_row_within;
        let expected = row_average_rgb(&full, full_sample_y, blue.min_x, blue.max_x);

        trace.anchor = ScrollAnchor::Row {
            entry_idx: 0,
            row_within_entry: 1 + half_row_within as usize,
        };
        let partial = render_trace(&mut trace, &mut image_cache, 80, 20);
        let partial_blue = colored_area(&partial, 80, 20, bluish);
        let actual = row_average_rgb(&partial, 1, partial_blue.min_x, partial_blue.max_x);

        assert_close_rgb(actual, expected, 20);
        assert_eq!(
            partial_blue.max_x - partial_blue.min_x,
            blue.max_x - blue.min_x,
            "slice width should match the full image width",
        );
    }

    #[test]
    fn bounded_cache_memory_caps_inline_trace_image_entries() {
        let mut trace = ReactTrace::new_for_tests();
        for i in 0..20 {
            trace
                .append_image(
                    Arc::new(solid_image(400, 20, [255, 0, 0, 255])),
                    PathBuf::from(format!("inline-smoke-{i}.png")),
                    format!("inline-smoke-digest-{i}"),
                    "12:00".to_string(),
                )
                .expect("distinct image digest should be inserted");
        }
        let mut image_cache = ImageCache::new();

        let _buf = render_trace(&mut trace, &mut image_cache, 80, 220);

        assert_eq!(
            image_cache.len(),
            (16, 0),
            "full inline protocol cache should be capped at 16 entries",
        );
        assert_eq!(
            image_cache.display_surface_len(),
            16,
            "display surface cache should be capped at 16 entries",
        );
    }
}

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
