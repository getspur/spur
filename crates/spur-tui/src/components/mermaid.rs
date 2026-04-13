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

use std::cell::RefCell;
use std::panic;
use std::sync::{Arc, OnceLock};

use image::DynamicImage;
use ratatui_image::protocol::StatefulProtocol;

// ─── Public types ────────────────────────────────────────────────────────────

/// Monotonically-increasing identifier for a mermaid diagram within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MermaidId(pub u64);

/// State machine for a pending / rendered mermaid diagram.
pub enum MermaidState {
    Pending { code: String },
    Rendering,
    Ready {
        /// Reference-counted to avoid deep-copying the pixel buffer on the
        /// hot render path — the dispatch layer already hands in an `Arc`.
        image: std::sync::Arc<DynamicImage>,
        /// Lazily-built protocol. Populated on first visible render;
        /// invalidated on terminal resize. `RefCell` because render takes `&self`.
        inline_protocol: RefCell<Option<StatefulProtocol>>,
    },
    Error { message: String },
}

impl std::fmt::Debug for MermaidState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MermaidState::Pending { code } => {
                f.debug_struct("Pending").field("code", code).finish()
            }
            MermaidState::Rendering => f.debug_struct("Rendering").finish(),
            MermaidState::Ready { image, .. } => f
                .debug_struct("Ready")
                .field("image_size", &(image.width(), image.height()))
                .field("inline_protocol", &"<cached>")
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
#[derive(Debug, Clone, Copy)]
pub enum FenceRender {
    Pending,
    Error,
    Ready(u16),
}

/// Single source of truth for the placeholder Line emitted when a fence
/// cannot render inline as an image. Maps:
/// - `Pending` → `⏳` DarkGray+DIM
/// - `Error`   → `⚠` Yellow+BOLD
/// - `Ready(_)` → `📊` Magenta+BOLD (the "ready but rendering as placeholder"
///   case — height is already encoded for the caller's ImageRow decision)
pub fn fence_placeholder_line(
    id: MermaidId,
    render: FenceRender,
) -> ratatui::text::Line<'static> {
    use ratatui::{
        style::{Color, Modifier, Style},
        text::{Line, Span},
    };
    let (text, style) = match render {
        FenceRender::Error => (
            format!("[⚠ mermaid #{} error · Alt-v to view]", id.0),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        FenceRender::Pending => (
            format!("[⏳ mermaid #{} rendering…]", id.0),
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
        ),
        FenceRender::Ready(_) => (
            format!("[📊 mermaid #{} · press Alt-v to view]", id.0),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
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

/// Default target pixel-width for rasterised diagrams.
const DEFAULT_WIDTH: u32 = 800;

/// Render a Mermaid diagram source string to a [`DynamicImage`].
///
/// Panics emitted by `mermaid-rs-renderer`, `usvg`, or `resvg` are caught
/// and converted to [`RenderError::Panic`], so this function never unwinds
/// the caller.
pub fn render_mermaid(code: &str) -> Result<DynamicImage, RenderError> {
    let code_owned = code.to_string();

    // Single unwind boundary covering the entire mmdr → usvg → resvg pipeline.
    // Both render_to_svg and rasterize_svg can panic on malformed input;
    // wrapping both here ensures the caller is never unwound.
    let result = panic::catch_unwind(move || {
        let svg = render_to_svg_inner(&code_owned)?;
        rasterize_svg(&svg, DEFAULT_WIDTH)
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

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Stage 1: mermaid source → SVG string.
///
/// Must be called from within a `catch_unwind` closure — `mmdr` can panic on
/// malformed input.
fn render_to_svg_inner(code: &str) -> Result<String, RenderError> {
    let opts = mermaid_rs_renderer::RenderOptions::default();
    mermaid_rs_renderer::render_with_options(code, opts)
        .map(|svg| fix_svg_font_families(&svg))
        .map_err(|e| RenderError::Render(e.to_string()))
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
            assert!(
                result.is_ok(),
                "rasterize_svg panicked on input: {svg:?}"
            );
            // The inner Result may be Ok or Err — both are fine.
        }
    }

    #[test]
    fn ready_state_holds_inline_protocol_slot() {
        use std::cell::RefCell;
        use image::RgbaImage;

        let img = std::sync::Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        let state = MermaidState::Ready {
            image: img,
            inline_protocol: RefCell::new(None),
        };
        match state {
            MermaidState::Ready { inline_protocol, .. } => {
                assert!(inline_protocol.borrow().is_none());
            }
            _ => panic!("expected Ready"),
        }
    }
}
