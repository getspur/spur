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

/// Read-only view of mermaid fence state, passed to rebuild so the
/// placeholder can reflect error/pending distinctions.
pub struct StateLookup<'a> {
    pub errors: &'a std::collections::HashSet<MermaidId>,
    pub pending: &'a std::collections::HashSet<MermaidId>,
}

impl<'a> StateLookup<'a> {
    /// A lookup with no error or pending entries — use when no state is known.
    pub fn empty() -> Self {
        static EMPTY_ERRORS: std::sync::OnceLock<std::collections::HashSet<MermaidId>> =
            std::sync::OnceLock::new();
        static EMPTY_PENDING: std::sync::OnceLock<std::collections::HashSet<MermaidId>> =
            std::sync::OnceLock::new();
        Self {
            errors: EMPTY_ERRORS.get_or_init(std::collections::HashSet::new),
            pending: EMPTY_PENDING.get_or_init(std::collections::HashSet::new),
        }
    }

    /// Returns true if the given fence id is in the error state.
    pub fn is_err(&self, id: MermaidId) -> bool {
        self.errors.contains(&id)
    }

    /// Returns true if the given fence id is in the pending/rendering state.
    pub fn is_pending(&self, id: MermaidId) -> bool {
        self.pending.contains(&id)
    }
}

/// Internal per-fence record (populated in Task 4; empty in this skeleton).
#[derive(Debug, Clone)]
pub struct FenceRef {
    pub id: MermaidId,
    pub byte_range: std::ops::Range<usize>,
    pub code: String,
}

/// Preserves fence boundaries so the render layer can allocate sub-Rects for
/// image widgets rather than folding fences into text placeholders.
#[derive(Debug, Clone)]
pub enum StreamItem {
    Text(Vec<Line<'static>>),
    Fence(MermaidId),
}

/// Whether a trimmed line looks like a GFM table row: starts and ends with
/// `|` after trimming, and has content between. Lone `|` or empty lines
/// don't count.
fn is_table_row(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 3 && t.starts_with('|') && t.ends_with('|')
}

/// Whether a line is a GFM table separator row — cells contain only `-`,
/// `:`, and whitespace. Requires `is_table_row` to be true.
fn is_table_separator(line: &str) -> bool {
    if !is_table_row(line) {
        return false;
    }
    let t = line.trim();
    let inner = &t[1..t.len() - 1];
    inner.split('|').all(|cell| {
        let c = cell.trim();
        !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':')
    })
}

/// Force GFM-table-like blocks to render with each row on its own line by
/// (a) separating the block from surrounding prose with a blank line and
/// (b) appending two trailing spaces ("  ") to every row except the last,
/// which is CommonMark's hard-break syntax.
///
/// Why: `tui-markdown 0.3` does not support the GFM tables extension.
/// Without intervention, pulldown-cmark parses consecutive `|...|` lines
/// as a single CommonMark paragraph — rows flow together into one line.
/// Hard breaks preserve line boundaries in source without producing
/// visible markers (unlike ``` fences, which tui-markdown renders as
/// literal text).
///
/// A "block" is transformed only when it has at least one separator row
/// (e.g. `|---|---|`) — otherwise it's likely not a table and we leave
/// it alone. Blocks inside existing fenced code blocks are untouched.
fn inject_hard_breaks_in_tables(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let mut out = String::with_capacity(input.len() + 16);
    let mut in_code_fence = false;
    let mut i = 0;

    // Helper: does the current output end with a blank line (or is empty)?
    let ends_with_blank = |s: &str| -> bool {
        s.is_empty() || s.ends_with("\n\n") || s.ends_with('\n') && s.len() == 1
    };

    while i < lines.len() {
        let line = lines[i];
        let trimmed_start = line.trim_start();

        // Track code-fence state so we don't transform content already
        // inside a ``` or ~~~ block.
        if trimmed_start.starts_with("```") || trimmed_start.starts_with("~~~") {
            in_code_fence = !in_code_fence;
            out.push_str(line);
            out.push('\n');
            i += 1;
            continue;
        }

        if !in_code_fence && is_table_row(line) {
            // Scan forward for consecutive table-ish rows.
            let start = i;
            while i < lines.len() && is_table_row(lines[i]) {
                i += 1;
            }
            let block = &lines[start..i];
            let has_sep = block.iter().any(|l| is_table_separator(l));

            if has_sep && block.len() >= 2 {
                // Ensure a blank line before the block so preceding prose
                // doesn't merge with the first row.
                if !ends_with_blank(&out) {
                    out.push('\n');
                }
                // Append "  " to every row except the last to force a
                // hard break at end-of-line. The last row's line break
                // naturally ends the paragraph.
                let last = block.len() - 1;
                for (idx, l) in block.iter().enumerate() {
                    out.push_str(l);
                    if idx < last && !l.ends_with("  ") {
                        out.push_str("  ");
                    }
                    out.push('\n');
                }
                // Ensure a trailing blank line so following prose doesn't
                // merge with the last row.
                if i < lines.len() && !lines[i].is_empty() {
                    out.push('\n');
                }
            } else {
                for l in block {
                    out.push_str(l);
                    out.push('\n');
                }
            }
            continue;
        }

        out.push_str(line);
        out.push('\n');
        i += 1;
    }

    out
}

/// Accumulated-text markdown renderer.
#[derive(Debug, Clone)]
pub struct MarkdownStream {
    raw_text: String,
    dirty_since: Option<Instant>,
    cached_items: Vec<StreamItem>,
    /// State-aware placeholder line keyed by fence id, populated during
    /// `rebuild` from the caller-provided `StateLookup`. Consumed by the
    /// back-compat `lines()` view so error/pending variants render
    /// identically to the pre-refactor behavior.
    fence_placeholders: std::collections::HashMap<MermaidId, Line<'static>>,
    known_fences: Vec<FenceRef>,
    next_fence_id: u64,
    /// When false, ```mermaid fences are not extracted — they flow through
    /// to tui-markdown as ordinary code blocks. Set at construction time
    /// from the terminal's image-protocol capability.
    mermaid_enabled: bool,
}

impl Default for MarkdownStream {
    fn default() -> Self {
        Self {
            raw_text: String::new(),
            dirty_since: None,
            cached_items: Vec::new(),
            fence_placeholders: std::collections::HashMap::new(),
            known_fences: Vec::new(),
            next_fence_id: 0,
            mermaid_enabled: true,
        }
    }
}

impl MarkdownStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct with explicit mermaid support. `false` makes ```mermaid
    /// fences render as ordinary code blocks (used when the terminal has
    /// no image-protocol support).
    pub fn new_with_mermaid(mermaid_enabled: bool) -> Self {
        Self {
            mermaid_enabled,
            ..Self::default()
        }
    }

    /// Append a chunk of text. Cheap — does not reparse.
    pub fn append(&mut self, text: &str) {
        self.raw_text.push_str(text);
        self.dirty_since.get_or_insert_with(Instant::now);
    }

    /// Flush if the debounce window has elapsed. Returns any newly-detected
    /// mermaid fences. Pass `states` so the placeholder can reflect errors.
    pub fn maybe_flush(&mut self, states: &StateLookup<'_>) -> Vec<FenceRef> {
        match self.dirty_since {
            Some(t) if t.elapsed() >= DEBOUNCE => self.flush_now(states),
            _ => Vec::new(),
        }
    }

    /// Force a flush immediately.
    pub fn flush_now(&mut self, states: &StateLookup<'_>) -> Vec<FenceRef> {
        self.dirty_since = None;
        self.rebuild(states)
    }

    /// Force the next `maybe_flush`/`flush_now` to rebuild even if not yet
    /// dirty by time. Used when external state (mermaid registry errors)
    /// changes so the placeholder reflects the new state on the next tick.
    pub fn mark_dirty_now(&mut self) {
        self.dirty_since = Some(Instant::now() - DEBOUNCE);
    }

    pub fn items(&self) -> &[StreamItem] {
        &self.cached_items
    }

    /// Back-compat flat view. Substitutes the placeholder text line for each
    /// `Fence(id)`. Uses a state-aware placeholder if one was captured during
    /// the most recent `rebuild`; otherwise falls back to the default
    /// Ready-style placeholder. New call sites should migrate to `items()`.
    pub fn lines(&self) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        for item in &self.cached_items {
            match item {
                StreamItem::Text(lines) => out.extend(lines.iter().cloned()),
                StreamItem::Fence(id) => {
                    if let Some(line) = self.fence_placeholders.get(id) {
                        out.push(line.clone());
                    } else {
                        let placeholder = format!("[📊 mermaid #{} · press Alt-v to view]", id.0);
                        out.push(Line::from(ratatui::text::Span::styled(
                            placeholder,
                            ratatui::style::Style::default()
                                .fg(ratatui::style::Color::Magenta)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        )));
                    }
                }
            }
        }
        out
    }

    /// Test-only helper: returns the concatenated text content of each
    /// cached Line for simple equality assertions in tests.
    pub fn cached_lines_debug(&self) -> Vec<String> {
        self.lines()
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    pub fn raw_text(&self) -> &str {
        &self.raw_text
    }

    /// Rebuild `cached_lines` from `raw_text`.
    fn rebuild(&mut self, states: &StateLookup<'_>) -> Vec<FenceRef> {
        // ── Stage 1: pre-scan raw_text for closed ```mermaid fences ───────
        // Skipped entirely when the terminal has no image-protocol support —
        // mermaid fences then flow through tui-markdown as ordinary code.
        let mut discovered: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        if self.mermaid_enabled {
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
                                discovered.push((start..range.end, std::mem::take(&mut buf)));
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
                    let f = FenceRef {
                        id,
                        byte_range: range,
                        code,
                    };
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

        // ── Stage 3.5: force table-like rows onto their own lines ────────
        // `tui-markdown` does not yet render GFM tables; without this,
        // rows are reflowed into a single paragraph line. Injecting
        // CommonMark hard breaks preserves line boundaries without
        // producing any visible markers in the output.
        let transformed = inject_hard_breaks_in_tables(&transformed);

        // ── Stage 4: parse transformed text via tui-markdown ──────────────
        if transformed.is_empty() {
            self.cached_items.clear();
            self.fence_placeholders.clear();
            return new_fences;
        }
        let text = tui_markdown::from_str(&transformed);
        let parsed_lines: Vec<ratatui::text::Line<'static>> = text
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

        // ── Stage 5 (revised): split lines into StreamItems by fence sentinels ──
        let mut items: Vec<StreamItem> = Vec::new();
        let mut current_text: Vec<ratatui::text::Line<'static>> = Vec::new();
        let mut placeholders: std::collections::HashMap<MermaidId, Line<'static>> =
            std::collections::HashMap::new();

        for line in parsed_lines {
            let raw: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let trimmed = raw.trim();
            if let Some(rest) = trimmed
                .strip_prefix('\u{0000}')
                .and_then(|s| s.strip_suffix('\u{0000}'))
                .and_then(|s| s.strip_prefix("MERMAID:"))
            {
                if !current_text.is_empty() {
                    items.push(StreamItem::Text(std::mem::take(&mut current_text)));
                }
                let id_num: u64 = rest.parse().unwrap_or(0);
                let id = MermaidId(id_num);

                // State-aware placeholder for the back-compat `lines()`
                // view. The shared helper keeps this text in sync with the
                // inline render path in react_trace.rs.
                use super::mermaid::FenceRender;
                let render = if states.is_err(id) {
                    FenceRender::Error
                } else if states.is_pending(id) {
                    FenceRender::Pending
                } else {
                    FenceRender::Ready(1) // height unused in the 📊 fallback
                };
                placeholders.insert(id, super::mermaid::fence_placeholder_line(id, render));
                items.push(StreamItem::Fence(id));
            } else {
                current_text.push(line);
            }
        }
        if !current_text.is_empty() {
            items.push(StreamItem::Text(current_text));
        }

        self.cached_items = items;
        self.fence_placeholders = placeholders;

        new_fences
    }
}

// ───────── ratatui-core 0.1 → ratatui 0.29 type conversions ─────────
//
// tui-markdown 0.3 uses ratatui-core 0.1 types; the rest of this crate
// uses ratatui 0.29 types. The two have parallel, semantically identical
// definitions but live in different crates, so we bridge them explicitly.

fn convert_color(c: ratatui_core::style::Color) -> ratatui::style::Color {
    use ratatui::style::Color as R;
    use ratatui_core::style::Color as C;
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

#[cfg(test)]
mod stream_item_tests {
    use super::*;
    use crate::components::markdown_stream::StateLookup;

    #[test]
    fn items_splits_text_and_fences() {
        let mut s = MarkdownStream::new();
        s.append("Intro prose\n\n```mermaid\nflowchart LR\nA-->B\n```\n\nOutro prose\n");
        let _ = s.flush_now(&StateLookup::empty());

        let items = s.items();
        assert_eq!(items.len(), 3, "expected Text, Fence, Text; got {items:?}");
        assert!(matches!(items[0], StreamItem::Text(_)));
        assert!(matches!(items[1], StreamItem::Fence(_)));
        assert!(matches!(items[2], StreamItem::Text(_)));
    }

    #[test]
    fn items_preserves_multiple_fences() {
        let mut s = MarkdownStream::new();
        s.append(
            "A\n\n```mermaid\ngraph TD\nA-->B\n```\n\nB\n\n```mermaid\ngraph TD\nX-->Y\n```\n\nC\n",
        );
        let _ = s.flush_now(&StateLookup::empty());
        let items = s.items();
        let fence_count = items
            .iter()
            .filter(|i| matches!(i, StreamItem::Fence(_)))
            .count();
        assert_eq!(fence_count, 2, "expected two fences; got items: {items:?}");
    }

    #[test]
    fn lines_back_compat_still_emits_placeholders() {
        let mut s = MarkdownStream::new();
        s.append("Intro\n\n```mermaid\ngraph TD\nA-->B\n```\n");
        let _ = s.flush_now(&StateLookup::empty());
        let joined: String = s
            .lines()
            .iter()
            .flat_map(|l| l.spans.iter().map(|sp| sp.content.as_ref()))
            .collect();
        assert!(
            joined.contains("mermaid #0"),
            "expected placeholder in back-compat lines(): {joined:?}"
        );
    }

    #[test]
    fn text_mode_renders_mermaid_fence_as_code_block() {
        let mut s = MarkdownStream::new_with_mermaid(false);
        s.append("Intro\n\n```mermaid\nflowchart LR\nA-->B\n```\n\nOutro\n");
        let _ = s.flush_now(&StateLookup::empty());

        // No fence item should be produced — mermaid should fall through to
        // tui-markdown as an ordinary code block.
        let has_fence = s
            .items()
            .iter()
            .any(|it| matches!(it, StreamItem::Fence(_)));
        assert!(
            !has_fence,
            "text mode must not produce Fence items: {:?}",
            s.items()
        );

        // Body text should appear verbatim in the rendered output.
        let joined = s.cached_lines_debug().join("\n");
        assert!(
            joined.contains("flowchart LR"),
            "expected mermaid source in output: {joined}"
        );
        assert!(
            joined.contains("A-->B"),
            "expected mermaid source in output: {joined}"
        );
    }

    // ── Table-wrapping tests ────────────────────────────────────────────

    #[test]
    fn gfm_table_is_wrapped_and_preserves_line_boundaries() {
        // Without the wrap, tui-markdown flows these rows into one paragraph
        // line. With the wrap, they must render as separate lines.
        let mut s = MarkdownStream::new();
        s.append("| Key | Action |\n|---|---|\n| Esc | cancel |\n| Enter | submit |\n");
        let _ = s.flush_now(&StateLookup::empty());

        let joined = s.cached_lines_debug().join("\n");
        let row_lines: Vec<_> = joined
            .lines()
            .filter(|l| l.contains("Esc") || l.contains("Enter"))
            .collect();
        assert_eq!(
            row_lines.len(),
            2,
            "each table row must render on its own line; got:\n{joined}"
        );
    }

    #[test]
    fn non_table_pipe_lines_are_not_transformed() {
        // Lines with `|` but without a separator row must NOT be treated as
        // tables. This avoids false positives on shell pipelines, regex
        // alternations, etc.
        let input = "Use `ls | grep foo` to filter.\nAnother | pipe line.\n";
        let out = inject_hard_breaks_in_tables(input);
        assert_eq!(
            out, input,
            "non-table pipe content must pass through unchanged; got:\n{out}"
        );
    }

    #[test]
    fn transform_ignores_content_inside_existing_code_fence() {
        // A table-like block INSIDE a ``` fence must be left alone — no
        // trailing "  " injected into rows.
        let input = "```\n| A | B |\n|---|---|\n| 1 | 2 |\n```\n";
        let out = inject_hard_breaks_in_tables(input);
        assert_eq!(
            out, input,
            "content inside existing code fence must pass through unchanged:\n{out}"
        );
    }

    #[test]
    fn transformed_table_has_no_visible_backtick_markers() {
        // End-to-end: the rendered output must not contain literal ``` that
        // would appear as visible text to the user (the tui-markdown 0.3
        // renderer emits ``` lines for code blocks, which is why we use
        // hard breaks instead of fencing).
        let mut s = MarkdownStream::new();
        s.append("| Key | Action |\n|---|---|\n| Esc | cancel |\n");
        let _ = s.flush_now(&StateLookup::empty());
        let joined = s.cached_lines_debug().join("\n");
        assert!(
            !joined.contains("```"),
            "rendered output must not contain ``` markers; got:\n{joined}"
        );
    }

    #[test]
    fn prose_adjacent_table_separates_first_row_from_prose() {
        // When prose precedes a table without a blank line, the first row
        // must still render on its own line (not merged with prose).
        let mut s = MarkdownStream::new();
        s.append("Here are bindings:\n| Key | Action |\n|---|---|\n| Esc | cancel |\n");
        let _ = s.flush_now(&StateLookup::empty());
        let joined = s.cached_lines_debug().join("\n");
        let lines: Vec<&str> = joined.lines().collect();
        let prose_and_row_on_same_line = lines
            .iter()
            .any(|l| l.contains("bindings:") && l.contains("| Key"));
        assert!(
            !prose_and_row_on_same_line,
            "prose must not merge with first table row; got:\n{joined}"
        );
    }

    #[test]
    fn table_row_detection() {
        assert!(is_table_row("| a | b |"));
        assert!(is_table_row("  | a | b |  "));
        assert!(is_table_row("|---|---|"));
        assert!(!is_table_row("text"));
        assert!(!is_table_row("|"));
        assert!(!is_table_row("||"));
        assert!(!is_table_row(""));
    }

    #[test]
    fn table_separator_detection() {
        assert!(is_table_separator("|---|---|"));
        assert!(is_table_separator("| --- | :---: | ---: |"));
        assert!(!is_table_separator("| a | b |"));
        assert!(!is_table_separator("|---| b |"));
    }

    #[test]
    fn end_to_end_table_sandwiched_between_prose_renders_cleanly() {
        // Realistic LLM output: prose, table, more prose, no blank-line
        // separators. Must render every row on its own line, separated
        // from surrounding prose, with no visible ``` markers.
        let mut s = MarkdownStream::new();
        s.append(
            "Here are the keybindings:\n\
             | Key | Action |\n\
             |---|---|\n\
             | Esc | cancel |\n\
             | Enter | submit |\n\
             That's it.\n",
        );
        let _ = s.flush_now(&StateLookup::empty());
        let joined = s.cached_lines_debug().join("\n");

        assert!(!joined.contains("```"), "got:\n{joined}");
        for needle in [
            "keybindings:",
            "| Key | Action |",
            "|---|---|",
            "| Esc | cancel |",
            "| Enter | submit |",
            "That's it.",
        ] {
            assert!(
                joined.lines().any(|l| l.contains(needle)),
                "expected {needle:?} on its own line; got:\n{joined}"
            );
        }

        let merged = joined
            .lines()
            .any(|l| l.contains("keybindings:") && l.contains("| Key"));
        assert!(!merged, "prose merged with first row:\n{joined}");
    }
}
