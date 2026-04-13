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
    known_fences: Vec<FenceRef>,
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
        // ── Stage 1: pre-scan raw_text for closed ```mermaid fences ───────
        let mut discovered: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        {
            use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
            let parser = Parser::new_ext(&self.raw_text, Options::empty()).into_offset_iter();
            let mut open_fence_start: Option<usize> = None;
            let mut buf = String::new();
            for (ev, range) in parser {
                match ev {
                    Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                        if info.as_ref().trim().eq_ignore_ascii_case("mermaid") {
                            open_fence_start = Some(range.start);
                            buf.clear();
                        }
                    }
                    Event::Text(t) if open_fence_start.is_some() => {
                        buf.push_str(&t);
                    }
                    Event::End(TagEnd::CodeBlock) => {
                        if let Some(start) = open_fence_start.take() {
                            // pulldown-cmark auto-closes open fences at EOF.
                            // Distinguish a truly closed fence by checking
                            // that the slice just before range.end ends with
                            // a closing ``` (3+ backticks after trimming ws).
                            let slice = self.raw_text[..range.end]
                                .trim_end_matches(['\n', '\r', ' ', '\t']);
                            if slice.ends_with("```") {
                                discovered
                                    .push((start..range.end, std::mem::take(&mut buf)));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // ── Stage 2: match against known_fences; assign ids to new ones ───
        let mut new_fences: Vec<FenceRef> = Vec::new();
        let mut refreshed: Vec<FenceRef> = Vec::with_capacity(discovered.len());
        for (range, code) in discovered {
            let existing = self
                .known_fences
                .iter()
                .find(|f| f.byte_range == range)
                .cloned();
            match existing {
                Some(f) => refreshed.push(f),
                None => {
                    let id = MermaidId(self.next_fence_id);
                    self.next_fence_id += 1;
                    let f = FenceRef { id, byte_range: range, code };
                    new_fences.push(f.clone());
                    refreshed.push(f);
                }
            }
        }
        self.known_fences = refreshed.clone();

        // ── Stage 3: build transformed input with sentinel lines ──────────
        let transformed = {
            let mut out = String::with_capacity(self.raw_text.len());
            let mut cursor = 0usize;
            for f in &refreshed {
                if f.byte_range.start > cursor {
                    out.push_str(&self.raw_text[cursor..f.byte_range.start]);
                }
                // Surrounding blank lines ensure pulldown-cmark treats the
                // sentinel as its own paragraph, not a continuation.
                out.push_str(&format!("\n\u{0000}MERMAID:{}\u{0000}\n", f.id.0));
                cursor = f.byte_range.end;
            }
            if cursor < self.raw_text.len() {
                out.push_str(&self.raw_text[cursor..]);
            }
            out
        };

        // ── Stage 4: parse transformed text via tui-markdown ──────────────
        if transformed.is_empty() {
            self.cached_lines.clear();
            return new_fences;
        }
        let text = tui_markdown::from_str(&transformed);
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

        // ── Stage 5: post-process — swap sentinels for placeholders ───────
        for line in &mut self.cached_lines {
            let raw: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let trimmed = raw.trim();
            if let Some(rest) = trimmed
                .strip_prefix('\u{0000}')
                .and_then(|s| s.strip_suffix('\u{0000}'))
                .and_then(|s| s.strip_prefix("MERMAID:"))
            {
                let placeholder = format!("[📊 mermaid #{rest} · press Alt-v to view]");
                *line = ratatui::text::Line::from(ratatui::text::Span::styled(
                    placeholder,
                    ratatui::style::Style::default()
                        .fg(ratatui::style::Color::Magenta)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ));
            }
        }

        new_fences
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
