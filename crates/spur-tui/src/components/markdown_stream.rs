//! Streaming markdown renderer for `AgentMessage` trace entries.
//!
//! Design: `append` is cheap (string push). `maybe_flush` is driven by the
//! UI tick; it rebuilds `cached_lines` when the stream has been dirty for
//! more than `DEBOUNCE` (50 ms) or when `flush_now` is called
//! (at `TurnComplete`).

use std::time::{Duration, Instant};

use ratatui::text::Line;

use super::mermaid::MermaidId;

pub const DEBOUNCE: Duration = Duration::from_millis(50);

/// Internal per-fence record (populated in Task 4; empty in this skeleton).
#[derive(Debug, Clone)]
pub struct FenceRef {
    pub id: MermaidId,
    pub byte_range: std::ops::Range<usize>,
    pub code: String,
}

/// Accumulated-text markdown renderer.
#[derive(Debug, Default)]
pub struct MarkdownStream {
    raw_text: String,
    dirty_since: Option<Instant>,
    cached_lines: Vec<Line<'static>>,
    /// Populated in Task 4 when mermaid fence extraction is implemented.
    #[allow(dead_code)]
    known_fences: Vec<FenceRef>,
    /// Populated in Task 4 when mermaid fence extraction is implemented.
    #[allow(dead_code)]
    next_fence_id: u64,
}

impl MarkdownStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a chunk of text. Cheap — does not reparse.
    pub fn append(&mut self, text: &str) {
        self.raw_text.push_str(text);
        self.dirty_since.get_or_insert_with(Instant::now);
    }

    /// Flush if the debounce window has elapsed. Returns any newly-detected
    /// mermaid fences (always empty in this skeleton; Task 4 adds the logic).
    pub fn maybe_flush(&mut self) -> Vec<FenceRef> {
        match self.dirty_since {
            Some(t) if t.elapsed() >= DEBOUNCE => self.flush_now(),
            _ => Vec::new(),
        }
    }

    /// Force a flush immediately.
    pub fn flush_now(&mut self) -> Vec<FenceRef> {
        self.dirty_since = None;
        self.rebuild()
    }

    pub fn lines(&self) -> &[Line<'static>] {
        &self.cached_lines
    }

    /// Test-only helper: returns the concatenated text content of each
    /// cached Line for simple equality assertions in tests.
    pub fn cached_lines_debug(&self) -> Vec<String> {
        self.cached_lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect()
    }

    pub fn raw_text(&self) -> &str {
        &self.raw_text
    }

    /// Rebuild `cached_lines` from `raw_text`.
    fn rebuild(&mut self) -> Vec<FenceRef> {
        // Handle empty raw_text — tui-markdown may return a Text with one
        // empty Line, but our test expects cached_lines_debug() to be empty.
        if self.raw_text.is_empty() {
            self.cached_lines.clear();
            return Vec::new();
        }
        let text = tui_markdown::from_str(&self.raw_text);
        // tui-markdown returns `ratatui_core::text::Line` (ratatui-core 0.1)
        // which is a distinct type from `ratatui::text::Line` (ratatui 0.29).
        // We convert field-by-field to preserve all style information.
        self.cached_lines = text
            .lines
            .into_iter()
            .map(|line| {
                let spans: Vec<ratatui::text::Span<'static>> =
                    line.spans.into_iter().map(convert_span).collect();
                let mut out = ratatui::text::Line::from(spans);
                out.style = convert_style(line.style);
                out.alignment = line.alignment.map(convert_alignment);
                out
            })
            .collect();
        Vec::new()
    }
}

// ───────── ratatui-core 0.1 → ratatui 0.29 type conversions ─────────
//
// tui-markdown 0.3 uses ratatui-core 0.1 types; the rest of this crate
// uses ratatui 0.29 types. The two have parallel, semantically identical
// definitions but live in different crates, so we bridge them explicitly.

fn convert_color(c: ratatui_core::style::Color) -> ratatui::style::Color {
    use ratatui_core::style::Color as C;
    use ratatui::style::Color as R;
    match c {
        C::Reset => R::Reset,
        C::Black => R::Black,
        C::Red => R::Red,
        C::Green => R::Green,
        C::Yellow => R::Yellow,
        C::Blue => R::Blue,
        C::Magenta => R::Magenta,
        C::Cyan => R::Cyan,
        C::Gray => R::Gray,
        C::DarkGray => R::DarkGray,
        C::LightRed => R::LightRed,
        C::LightGreen => R::LightGreen,
        C::LightYellow => R::LightYellow,
        C::LightBlue => R::LightBlue,
        C::LightMagenta => R::LightMagenta,
        C::LightCyan => R::LightCyan,
        C::White => R::White,
        C::Rgb(r, g, b) => R::Rgb(r, g, b),
        C::Indexed(i) => R::Indexed(i),
    }
}

fn convert_style(s: ratatui_core::style::Style) -> ratatui::style::Style {
    ratatui::style::Style {
        fg: s.fg.map(convert_color),
        bg: s.bg.map(convert_color),
        // ratatui-core 0.1 only exposes `underline_color` behind its own
        // `underline-color` feature, which tui-markdown does not enable.
        // Since no underline colour is encoded in tui-markdown output, we
        // leave the field as None here rather than using a cfg gate that
        // would be fragile to upstream changes.
        underline_color: None,
        add_modifier: ratatui::style::Modifier::from_bits_retain(s.add_modifier.bits()),
        sub_modifier: ratatui::style::Modifier::from_bits_retain(s.sub_modifier.bits()),
    }
}

fn convert_alignment(a: ratatui_core::layout::Alignment) -> ratatui::layout::Alignment {
    match a {
        ratatui_core::layout::Alignment::Left => ratatui::layout::Alignment::Left,
        ratatui_core::layout::Alignment::Center => ratatui::layout::Alignment::Center,
        ratatui_core::layout::Alignment::Right => ratatui::layout::Alignment::Right,
    }
}

fn convert_span(span: ratatui_core::text::Span<'_>) -> ratatui::text::Span<'static> {
    ratatui::text::Span {
        content: std::borrow::Cow::Owned(span.content.into_owned()),
        style: convert_style(span.style),
    }
}
