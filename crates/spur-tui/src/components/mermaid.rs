//! Mermaid diagram rendering.
//!
//! Embeds `mermaid-rs-renderer` (`mmdr`) as a library — produces a raster
//! `DynamicImage` from a Mermaid source string. The pipeline is:
//!
//!   1. `mermaid-rs-renderer::render_with_options` → SVG string
//!   2. Fix malformed `font-family` attribute values emitted by the renderer
//!      (inner unescaped `"` → `'`), so that usvg can parse the SVG cleanly.
//!   3. `resvg` / `usvg` / `tiny-skia` → raster RGBA pixmap
//!   4. `image::RgbaImage` → `DynamicImage`
//!
//! All panics emitted by `mermaid-rs-renderer`, `usvg`, or `resvg` (known to
//! occur on empty, malformed input or malformed SVG) are caught via a single
//! `std::panic::catch_unwind` in [`render_mermaid`], so a bad diagram never
//! unwinds the caller.

use std::panic;
use std::sync::{Arc, OnceLock};

use image::DynamicImage;

// ─── Public types ────────────────────────────────────────────────────────────

/// Monotonically-increasing identifier for a mermaid diagram within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MermaidId(pub u64);

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
    ReadyText {
        /// Native text rendering from `mermaid-text` for diagram kinds that
        /// are legible without rasterization.
        text: Arc<str>,
        /// Source code retained for consistency with raster Ready states.
        code: String,
        /// Width budget passed to the text renderer.
        rendered_at_width: u32,
    },
    Error {
        message: String,
    },
}

/// Result produced by the hybrid render pipeline before it is stored in
/// [`MermaidState`].
pub enum MermaidRendered {
    Image(DynamicImage),
    Text { text: String },
}

/// Thread/channel-friendly render completion payload.
#[derive(Debug, Clone)]
pub enum MermaidRenderOutput {
    Image(Arc<DynamicImage>),
    Text(Arc<str>),
}

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
            MermaidState::ReadyText {
                text,
                code,
                rendered_at_width,
            } => f
                .debug_struct("ReadyText")
                .field("text_len", &text.len())
                .field("code_len", &code.len())
                .field("rendered_at_width", rendered_at_width)
                .finish(),
            MermaidState::Error { message } => {
                f.debug_struct("Error").field("message", message).finish()
            }
        }
    }
}

/// Per-fence render state consumed by the inline render path and the
/// back-compat `MarkdownStream::lines()` placeholder cache. Encodes the
/// Pending / Error / Ready distinction with the pre-computed inline row
/// height for Ready.
#[derive(Debug, Clone)]
pub enum FenceRender {
    Pending,
    Error,
    Ready(u16),
    ReadyText(Arc<str>),
}

/// Single source of truth for the placeholder Line emitted when a fence
/// cannot render inline as an image. Maps:
/// - `Pending` → `⏳` DarkGray+DIM
/// - `Error`   → `⚠` Yellow+BOLD
/// - `Ready(_)` → `📊` Magenta+BOLD (the "ready but rendering as placeholder"
///   case — height is already encoded for the caller's ImageRow decision)
pub fn fence_placeholder_line(id: MermaidId, render: FenceRender) -> ratatui::text::Line<'static> {
    use ratatui::{
        style::{Color, Modifier, Style},
        text::{Line, Span},
    };
    let (text, style) = match render {
        FenceRender::Error => (
            format!("[⚠ mermaid #{} error · Alt-v to view]", id.0),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        FenceRender::Pending => (
            format!("[⏳ mermaid #{} rendering…]", id.0),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
        FenceRender::Ready(_) | FenceRender::ReadyText(_) => (
            format!("[📊 mermaid #{} · press Alt-v to view]", id.0),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    };
    Line::from(Span::styled(text, style))
}

/// Error type returned by [`render_mermaid`].
#[derive(Debug)]
pub enum RenderError {
    /// The renderer (mmdr) returned an error for this input.
    Render(String),
    /// The renderer panicked; the panic message is captured here.
    Panic(String),
    /// The rasterisation step failed (SVG parse or pixmap allocation).
    Decode(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::Render(msg) => write!(f, "mermaid render error: {msg}"),
            RenderError::Panic(msg) => write!(f, "mermaid renderer panicked: {msg}"),
            RenderError::Decode(msg) => write!(f, "mermaid rasterise error: {msg}"),
        }
    }
}

impl std::error::Error for RenderError {}

// ─── Public API ──────────────────────────────────────────────────────────────

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

/// Render a Mermaid diagram through the native text renderer when the diagram
/// kind is one of SPUR's supported text-mode kinds. Unsupported kinds and text
/// parse/render failures fall back to the existing raster pipeline.
pub fn render_mermaid_hybrid(
    code: &str,
    target_width: u32,
) -> Result<MermaidRendered, RenderError> {
    if let Some(text) = render_text_if_supported(code, target_width) {
        return Ok(MermaidRendered::Text { text });
    }

    render_mermaid(code, target_width).map(MermaidRendered::Image)
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Stage 1: mermaid source → SVG string.
///
/// Must be called from within a `catch_unwind` closure — `mmdr` can panic on
/// malformed input.
fn render_to_svg_inner(code: &str) -> Result<String, RenderError> {
    // Library defaults (50/50) produce cramped layouts for typical flowcharts.
    // Use the author's README baseline (60/80) plus a wide-aspect hint suited
    // to TUI panes (typically 2-3× wider than tall). See:
    //   https://github.com/1jehuang/mermaid-rs-renderer/blob/master/README.md
    let opts = mermaid_rs_renderer::RenderOptions::modern()
        .with_node_spacing(60.0)
        .with_rank_spacing(80.0)
        .with_preferred_aspect_ratio(2.5);
    mermaid_rs_renderer::render_with_options(code, opts)
        .map(|svg| fix_svg_font_families(&svg))
        .map_err(|e| RenderError::Render(e.to_string()))
}

fn render_text_if_supported(code: &str, target_width: u32) -> Option<String> {
    use mermaid_text::detect::DiagramKind;

    let kind = mermaid_text::detect::detect(code).ok()?;
    if !matches!(
        kind,
        DiagramKind::Flowchart | DiagramKind::State | DiagramKind::Class
    ) {
        return None;
    }

    // The existing action field is a raster pixel bucket. Convert it to a
    // terminal-column budget using the same fallback cell width used when
    // dispatching render requests without a live picker.
    let width = (target_width / 8).clamp(20, 240) as usize;
    panic::catch_unwind(|| mermaid_text::render_with_width(code, Some(width)))
        .ok()
        .and_then(Result::ok)
}

// SVG font-family post-process.
// Pattern adapted from `Epistates/treemd/src/tui/mermaid.rs` — mmdr's SVG
// output sometimes produces malformed font-family attributes that trip
// usvg. This normalizes them before rasterization.
fn fix_svg_font_families(svg: &str) -> String {
    let marker = "font-family=\"";
    let mut result = String::with_capacity(svg.len());
    let mut remaining = svg;

    while let Some(start) = remaining.find(marker) {
        result.push_str(&remaining[..start + marker.len()]);
        remaining = &remaining[start + marker.len()..];

        let chars: Vec<char> = remaining.chars().collect();
        let mut char_idx = 0;
        let mut found_close = false;

        while char_idx < chars.len() {
            if chars[char_idx] == '"' {
                let next = chars.get(char_idx + 1).copied().unwrap_or('>');
                if next == '>' || next == ' ' || next == '/' {
                    // Real closing quote — emit it and advance `remaining`.
                    result.push('"');
                    let byte_offset: usize =
                        chars[..char_idx + 1].iter().map(|c| c.len_utf8()).sum();
                    remaining = &remaining[byte_offset..];
                    found_close = true;
                    break;
                } else {
                    // Inner quote — replace with single quote.
                    result.push('\'');
                }
            } else {
                result.push(chars[char_idx]);
            }
            char_idx += 1;
        }

        if !found_close {
            remaining = "";
        }
    }

    result.push_str(remaining);
    result
}

/// Lazily-initialised system font database shared across all render calls.
fn font_database() -> Arc<resvg::usvg::fontdb::Database> {
    static DB: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        Arc::new(db)
    })
    .clone()
}

/// Stage 2: SVG string → `DynamicImage` at `target_width` pixels wide.
///
/// Must be called from within a `catch_unwind` closure — usvg/resvg can
/// panic on severely malformed SVG input.
fn rasterize_svg(svg: &str, target_width: u32) -> Result<DynamicImage, RenderError> {
    let db = font_database();

    let opts = resvg::usvg::Options {
        fontdb: db,
        ..Default::default()
    };

    let tree = resvg::usvg::Tree::from_str(svg, &opts)
        .map_err(|e| RenderError::Decode(format!("SVG parse: {e}")))?;

    let svg_size = tree.size();
    let scale = target_width as f32 / svg_size.width();
    let width = target_width;
    let height = (svg_size.height() * scale).ceil() as u32;

    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| RenderError::Decode("failed to allocate pixmap".to_string()))?;

    // Mermaid's default theme targets a white background and uses light
    // pastel fills. Without this, the rasterised SVG is transparent +
    // pale-stroked — invisible on a dark terminal. Fill opaque white first.
    pixmap.fill(resvg::tiny_skia::Color::WHITE);

    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let rgba = image::RgbaImage::from_raw(width, height, pixmap.data().to_vec())
        .ok_or_else(|| RenderError::Decode("failed to create image buffer".to_string()))?;

    Ok(DynamicImage::ImageRgba8(rgba))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn fix_font_families_replaces_inner_quotes() {
        let input = r#"<text font-family="Inter, "Segoe UI", sans-serif">hello</text>"#;
        let fixed = fix_svg_font_families(input);
        assert!(
            !fixed.contains(r#""Segoe UI""#),
            "inner quotes should be replaced"
        );
        assert!(fixed.contains("'Segoe UI'"), "should use single quotes");
    }

    /// Feeding a truncated / structurally-invalid SVG to `rasterize_svg` (via
    /// `render_mermaid`) must never panic — it should return an error or a
    /// (possibly empty) image.  This exercises the second stage of the pipeline
    /// that was previously outside the `catch_unwind` boundary.
    #[test]
    fn malformed_svg_rasterization_does_not_panic() {
        // Craft an SVG that is structurally invalid enough to stress the parse
        // path: malformed attributes, unclosed tags, garbage content.
        let bad_svgs = [
            // Completely empty string
            "",
            // Truncated SVG header
            "<svg",
            // SVG with a zero-size viewport (would produce a 0×0 pixmap)
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="0"></svg>"#,
            // Garbage that looks vaguely like XML but isn't valid SVG
            r#"<svg xmlns="http://www.w3.org/2000/svg"><<<<GARBAGE>>>></svg>"#,
            // Deeply malformed font-family to stress fix_svg_font_families + usvg
            r#"<svg xmlns="http://www.w3.org/2000/svg"><text font-family="</svg>"#,
        ];

        for svg in &bad_svgs {
            // We call render_mermaid rather than rasterize_svg directly so the
            // full catch_unwind boundary is exercised.  Because mmdr may wrap or
            // ignore the content, we synthesise the SVG stage by calling
            // rasterize_svg inside catch_unwind ourselves.
            let result = std::panic::catch_unwind(|| rasterize_svg(svg, 800));
            assert!(result.is_ok(), "rasterize_svg panicked on input: {svg:?}");
            // The inner Result may be Ok or Err — both are fine.
        }
    }

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

    #[test]
    fn hybrid_routes_flowchart_to_text() {
        let rendered = render_mermaid_hybrid("graph TD\nA[Start] --> B[End]", 800)
            .expect("flowchart should render through text path");

        match rendered {
            MermaidRendered::Text { text } => {
                assert!(text.contains("Start"));
                assert!(text.contains("End"));
            }
            MermaidRendered::Image(_) => panic!("flowchart should use text renderer"),
        }
    }

    #[test]
    fn hybrid_routes_pie_to_raster_fallback() {
        let rendered =
            render_mermaid_hybrid("pie title Pets\n    \"Dogs\" : 5\n    \"Cats\" : 3", 800)
                .expect("pie should fall back to raster renderer");

        match rendered {
            MermaidRendered::Image(image) => {
                assert!(image.width() > 0);
                assert!(image.height() > 0);
            }
            MermaidRendered::Text { .. } => panic!("pie should use raster fallback"),
        }
    }
}
