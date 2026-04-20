//! Compact single-line-per-entry render path used by the DetailPane
//! Stream tab. Mirrors the visual density of the pre-unification
//! `DetailPane::render_stream` while sharing entry state with the full
//! brain-view render.
//!
//! Draws ONLY the body — NO block, border, or title. DetailPane owns
//! the outer block.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::types::{ActStatus, TraceEntry, TraceKind};
use super::ReactTrace;

/// Cache entry for the compact render path. Keyed by `(generation, width)`.
/// Independent from the full-render `line_cache` because the two paths
/// produce different row layouts.
pub(in crate::components::react_trace) struct CompactCacheEntry {
    pub generation: u64,
    pub width: u16,
    pub lines: Vec<Line<'static>>,
    /// Row index where each entry's content line begins. `entry_row_starts[i]`
    /// is the index into `lines` of entry `i`'s content line (NOT the
    /// preceding kind-transition separator, if any).
    ///
    /// Invariant: `entry_row_starts.len() == covered_entries`, strictly
    /// non-decreasing, `entry_row_starts[0] == 0`.
    pub entry_row_starts: Vec<usize>,
    /// Number of `ReactTrace.entries` the cache covers. Anything past
    /// this index is dirty and must be rebuilt on the next render.
    pub covered_entries: usize,
}

impl ReactTrace {
    /// Build the compact display lines (one row per entry, plus optional
    /// kind-transition separators). Returned lines have `'static` content.
    pub(super) fn build_compact_lines(&self, width: u16) -> Vec<Line<'static>> {
        build_compact_lines_from(&self.entries, width, None)
    }

    #[cfg(test)]
    pub fn build_compact_lines_for_tests(&self, width: u16) -> Vec<Line<'static>> {
        self.build_compact_lines(width)
    }

    /// Paint the compact single-line-per-entry body into `area`.
    ///
    /// Does NOT draw a block/border/title — the caller (DetailPane) owns
    /// the outer block. Honours the current `ScrollAnchor` for vertical
    /// offset.
    ///
    /// **Caching.** Each call checks `compact_cache`:
    /// - **Hit** (same generation + width + entry count): O(1) clone.
    /// - **Incremental** (width unchanged, dirty_from set): truncate at
    ///   dirty row-start and rebuild tail only.
    /// - **Full rebuild** (width changed or cold): rebuild all lines.
    pub fn render_compact(&mut self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
        use ratatui::widgets::{Paragraph, Wrap};

        let width = area.width;
        self.last_render_width = Some(width);
        self.last_visible_height = area.height as usize;
        let gen = self.generation;

        let lines = self.compact_lines_for_render(gen, width);
        self.last_total_lines = lines.len();

        let scroll = match self.anchor {
            crate::components::react_trace::types::ScrollAnchor::Following => {
                self.last_total_lines
                    .saturating_sub(self.last_visible_height)
            }
            crate::components::react_trace::types::ScrollAnchor::Row {
                entry_idx,
                row_within_entry,
            } => {
                let total = self.last_total_lines;
                let max = total.saturating_sub(self.last_visible_height);
                (entry_idx + row_within_entry).min(max)
            }
        };

        let p = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll as u16, 0));
        frame.render_widget(p, area);
    }

    /// Internal cache-aware line producer for `render_compact`.
    /// Returns a clone of the cached `Vec<Line>`.
    fn compact_lines_for_render(&mut self, gen: u64, width: u16) -> Vec<Line<'static>> {
        let entry_count = self.entries.len();
        let dirty = self.dirty_from.unwrap_or(entry_count);

        // Cache hit.
        if let Some(c) = &self.compact_cache {
            if c.generation == gen && c.width == width && c.covered_entries == entry_count {
                return c.lines.clone();
            }
        }

        // Incremental rebuild.
        if let Some(c) = self.compact_cache.as_mut() {
            if c.width == width && dirty < entry_count && dirty <= c.covered_entries {
                let prefix_row_count = prefix_row_count_for_entries(&self.entries[..dirty]);
                c.lines.truncate(prefix_row_count);
                let prev_kind_tag = if dirty > 0 {
                    Some(compact_kind_tag(&self.entries[dirty - 1].kind))
                } else {
                    None
                };
                let tail = build_compact_lines_from(&self.entries[dirty..], width, prev_kind_tag);
                c.lines.extend(tail);
                c.generation = gen;
                c.covered_entries = entry_count;
                let lines = c.lines.clone();
                self.dirty_from = None;
                return lines;
            }
        }

        // Full rebuild.
        let lines = self.build_compact_lines(width);
        self.compact_cache = Some(CompactCacheEntry {
            generation: gen,
            width,
            lines: lines.clone(),
            entry_row_starts: Vec::new(),
            covered_entries: entry_count,
        });
        self.dirty_from = None;
        lines
    }

    /// Drop the compact cache. Called when the owning executor loses
    /// focus (Phase 4) to free memory without losing the entries
    /// themselves.
    pub fn drop_compact_cache(&mut self) {
        self.compact_cache = None;
    }
}

fn compact_kind_tag(k: &TraceKind) -> &'static str {
    match k {
        TraceKind::Think => "think",
        TraceKind::AgentMessage { .. } => "message",
        TraceKind::Act { .. } => "act",
        TraceKind::Observe { .. } => "observe",
        TraceKind::Delegate { .. } => "delegate",
        TraceKind::UserMessage => "user",
        TraceKind::Permission { .. } => "permission",
    }
}

fn compact_prefix_style(k: &TraceKind) -> (&'static str, Style) {
    match k {
        TraceKind::Think => ("  · ", Style::default().fg(Color::DarkGray)),
        TraceKind::AgentMessage { .. } => ("  ▸ ", Style::default().fg(Color::White)),
        TraceKind::Act { status, .. } => {
            let color = match status {
                ActStatus::Pending | ActStatus::InProgress { .. } => Color::Yellow,
                ActStatus::Completed(_) => Color::Green,
                ActStatus::Failed(_) => Color::Red,
            };
            ("  ▶ ", Style::default().fg(color).add_modifier(Modifier::BOLD))
        }
        TraceKind::Observe { .. } => ("  ◂ ", Style::default().fg(Color::DarkGray)),
        TraceKind::Delegate { .. } => (
            "  ⇲ ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        TraceKind::UserMessage => (
            "  > ",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        TraceKind::Permission { .. } => (
            "  ? ",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
    }
}

/// Row count produced by rendering `entries` via compact mode: one line
/// per entry plus one separator per kind transition. Used for the
/// truncate point in incremental rebuilds.
fn prefix_row_count_for_entries(entries: &[TraceEntry]) -> usize {
    if entries.is_empty() {
        return 0;
    }
    let mut rows = 0usize;
    let mut prev_kind_tag: Option<&'static str> = None;
    for e in entries {
        let tag = compact_kind_tag(&e.kind);
        if let Some(pk) = prev_kind_tag {
            if pk != tag {
                rows += 1; // separator
            }
        }
        prev_kind_tag = Some(tag);
        rows += 1;
    }
    rows
}

/// Core of `build_compact_lines` and the incremental-rebuild path.
/// `seed_prev_kind_tag` carries the kind of the entry BEFORE `entries`
/// (for incremental mode), so the first transition separator is inserted
/// consistently with a full rebuild.
fn build_compact_lines_from(
    entries: &[TraceEntry],
    width: u16,
    seed_prev_kind_tag: Option<&'static str>,
) -> Vec<Line<'static>> {
    let w = width as usize;
    if entries.is_empty() && seed_prev_kind_tag.is_none() {
        return vec![Line::from(Span::styled(
            "(waiting for worker output…)",
            Style::default().fg(Color::DarkGray),
        ))];
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut prev_kind_tag: Option<&'static str> = seed_prev_kind_tag;

    for entry in entries {
        let kind_tag = compact_kind_tag(&entry.kind);
        if let Some(pk) = prev_kind_tag {
            if pk != kind_tag {
                let sep: String = " ─"
                    .chars()
                    .chain(std::iter::repeat_n('─', w.saturating_sub(3)))
                    .collect();
                lines.push(Line::from(Span::styled(
                    sep,
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        prev_kind_tag = Some(kind_tag);

        let (prefix, style) = compact_prefix_style(&entry.kind);
        let ts = entry.timestamp.clone();
        let ts_display = format!(" {}", ts);

        // In markdown mode `entry.text` is empty; fall back to raw_text.
        #[cfg(feature = "markdown")]
        let raw: &str = entry
            .markdown
            .as_ref()
            .map(|s| s.raw_text())
            .filter(|s| !s.is_empty())
            .unwrap_or(&entry.text);
        #[cfg(not(feature = "markdown"))]
        let raw: &str = &entry.text;

        let text_single_line: String = raw
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();

        let prefix_cols = UnicodeWidthStr::width(prefix);
        let ts_cols = UnicodeWidthStr::width(ts_display.as_str());
        let text_budget = w.saturating_sub(prefix_cols + ts_cols + 1);
        let display_text = truncate_to_width(&text_single_line, text_budget);
        let display_cols = UnicodeWidthStr::width(display_text.as_str());
        let pad = w.saturating_sub(prefix_cols + display_cols + ts_cols);
        let padding: String = " ".repeat(pad);

        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), style),
            Span::styled(display_text, style),
            Span::raw(padding),
            Span::styled(ts_display, Style::default().fg(Color::DarkGray)),
        ]));
    }

    lines
}

fn truncate_to_width(s: &str, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    let full_width = UnicodeWidthStr::width(s);
    if full_width <= max_cols {
        return s.to_string();
    }
    let target = max_cols.saturating_sub(1);
    let mut cols = 0;
    let mut end = 0;
    for (i, ch) in s.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if cols + cw > target {
            break;
        }
        cols += cw;
        end = i + ch.len_utf8();
    }
    format!("{}…", &s[..end])
}
