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

use crate::components::trace_format::terminal_safe_text;

use super::types::{ActStatus, TraceEntry, TraceKind};
use super::ReactTrace;

/// Below 10 columns, the widest compact prefix (`"  > "`, 4 cols) plus
/// the timestamp display (`" 12:00"`, 6 cols) no longer fit on one row.
/// Emit a placeholder row instead so compact mode never relies on
/// `Paragraph::wrap` to add hidden rows.
const MINIMUM_COMPACT_WIDTH: usize = 10;

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
    /// Build the compact display lines plus the per-entry row-start vector.
    /// Returned lines have `'static` content.
    pub(super) fn build_compact_lines(&self, width: u16) -> (Vec<Line<'static>>, Vec<usize>) {
        build_compact_lines_from(&self.entries, width, None, 0)
    }

    #[cfg(test)]
    pub fn build_compact_lines_for_tests(&self, width: u16) -> Vec<Line<'static>> {
        self.build_compact_lines(width).0
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

        // Resolve anchor through the unified helper so scroll math and
        // render math share the same coordinate system. The compact cache's
        // `entry_row_starts` provides the per-entry row layout.
        let starts: &[usize] = self
            .compact_cache
            .as_ref()
            .map(|c| c.entry_row_starts.as_slice())
            .unwrap_or(&[]);
        let scroll = crate::components::react_trace::render::resolve_anchor(
            &self.anchor,
            starts,
            self.last_total_lines,
            self.last_visible_height,
        );

        let p = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll as u16, 0));
        frame.render_widget(p, area);

        // Mark this as the surface we last painted so scroll mutators pick
        // the right cache for anchor resolution.
        self.last_surface = crate::components::react_trace::Surface::Compact(self.generation);
    }

    /// Internal cache-aware line producer for `render_compact`.
    /// Returns a clone of the cached `Vec<Line>`.
    ///
    /// Keeps `cache.entry_row_starts` in lockstep with `cache.lines` across
    /// all three code paths (hit / incremental / cold).
    fn compact_lines_for_render(&mut self, gen: u64, width: u16) -> Vec<Line<'static>> {
        let entry_count = self.entries.len();
        let dirty = self.dirty_from.unwrap_or(entry_count);

        // Cache hit.
        if let Some(c) = &self.compact_cache {
            if c.generation == gen && c.width == width && c.covered_entries == entry_count {
                return c.lines.clone();
            }
        }

        // Incremental rebuild. Truncate BOTH lines and row_starts at the
        // dirty boundary, then append the tail of both.
        if let Some(c) = self.compact_cache.as_mut() {
            if c.width == width && dirty < entry_count && dirty <= c.covered_entries {
                let prefix_row_count = prefix_row_count_for_entries(&self.entries[..dirty]);
                c.lines.truncate(prefix_row_count);
                c.entry_row_starts.truncate(dirty);
                let prev_kind_tag = if dirty > 0 {
                    Some(compact_kind_tag(&self.entries[dirty - 1].kind))
                } else {
                    None
                };
                let (tail_lines, tail_starts) = build_compact_lines_from(
                    &self.entries[dirty..],
                    width,
                    prev_kind_tag,
                    prefix_row_count,
                );
                c.lines.extend(tail_lines);
                c.entry_row_starts.extend(tail_starts);
                c.generation = gen;
                c.covered_entries = entry_count;
                debug_assert_eq!(
                    c.entry_row_starts.len(),
                    c.covered_entries,
                    "entry_row_starts must have one entry per covered entry"
                );
                let lines = c.lines.clone();
                self.dirty_from = None;
                return lines;
            }
        }

        // Full rebuild.
        let (lines, entry_row_starts) = self.build_compact_lines(width);
        debug_assert_eq!(
            entry_row_starts.len(),
            entry_count,
            "cold build must produce one row-start per entry"
        );
        self.compact_cache = Some(CompactCacheEntry {
            generation: gen,
            width,
            lines: lines.clone(),
            entry_row_starts,
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
            (
                "  ▶ ",
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        }
        TraceKind::Observe { .. } => ("  ◂ ", Style::default().fg(Color::DarkGray)),
        TraceKind::Delegate { .. } => (
            "  ⇲ ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        TraceKind::UserMessage => (
            "  > ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        TraceKind::Permission { .. } => (
            "  ? ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
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
///
/// Returns `(lines, entry_row_starts)` where `entry_row_starts[i]` is the
/// row index (in the GLOBAL cache coordinate system, offset by `base_row`)
/// at which entry `i`'s content line sits. Separator rows between
/// kind-transitions are NOT counted into `entry_row_starts[i]` — each
/// `starts[i]` points at the entry's content row, not its preceding
/// separator.
///
/// `seed_prev_kind_tag` carries the kind of the entry BEFORE `entries` so
/// the first transition separator (if any) is inserted consistently with a
/// full rebuild.
///
/// `base_row` is added to every start value so the tail produced by the
/// incremental-rebuild path lines up with the prefix it is appended to.
/// Cold-build callers pass `0`.
fn build_compact_lines_from(
    entries: &[TraceEntry],
    width: u16,
    seed_prev_kind_tag: Option<&'static str>,
    base_row: usize,
) -> (Vec<Line<'static>>, Vec<usize>) {
    let w = width as usize;
    if entries.is_empty() && seed_prev_kind_tag.is_none() {
        let placeholder = vec![Line::from(Span::styled(
            "(waiting for worker output...)",
            Style::default().fg(Color::DarkGray),
        ))];
        return (placeholder, Vec::new());
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut entry_row_starts: Vec<usize> = Vec::with_capacity(entries.len());
    let mut prev_kind_tag: Option<&'static str> = seed_prev_kind_tag;
    let mut row: usize = base_row;

    for entry in entries {
        let kind_tag = compact_kind_tag(&entry.kind);
        if let Some(pk) = prev_kind_tag {
            if pk != kind_tag {
                let sep: String = match w {
                    0 => String::new(),
                    1 => "-".to_string(),
                    _ => " ".chars().chain(std::iter::repeat_n('-', w - 1)).collect(),
                };
                lines.push(Line::from(Span::styled(
                    sep,
                    Style::default().fg(Color::DarkGray),
                )));
                row += 1;
            }
        }
        prev_kind_tag = Some(kind_tag);

        // Record the row of this entry's content line BEFORE pushing it.
        entry_row_starts.push(row);

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
        let text_single_line = terminal_safe_text(&text_single_line);

        if w < MINIMUM_COMPACT_WIDTH {
            let placeholder = if w == 0 {
                String::new()
            } else {
                "…".to_string()
            };
            lines.push(Line::from(Span::styled(placeholder, style)));
            row += 1;
            continue;
        }

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
        row += 1;
    }

    (lines, entry_row_starts)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_prefixes_match_trace_kind() {
        let entries = [
            (
                TraceEntry {
                    kind: TraceKind::Think,
                    text: String::new(),
                    timestamp: "00:00".into(),
                    markdown: None,
                },
                "  · ",
            ),
            (
                TraceEntry {
                    kind: TraceKind::AgentMessage {
                        agent: "agent".into(),
                    },
                    text: String::new(),
                    timestamp: "00:00".into(),
                    markdown: None,
                },
                "  ▸ ",
            ),
            (
                TraceEntry {
                    kind: TraceKind::Act {
                        tool: "read".into(),
                        family: spur_acp::adapter::ToolFamily::Read,
                        input: spur_acp::adapter::ToolInputDisplay::Empty,
                        tool_call_id: None,
                        status: ActStatus::Completed(None),
                    },
                    text: String::new(),
                    timestamp: "00:00".into(),
                    markdown: None,
                },
                "  ▶ ",
            ),
            (
                TraceEntry {
                    kind: TraceKind::Observe { payload: None },
                    text: String::new(),
                    timestamp: "00:00".into(),
                    markdown: None,
                },
                "  ◂ ",
            ),
            (
                TraceEntry {
                    kind: TraceKind::Delegate {
                        agent: "worker".into(),
                        task: String::new(),
                        status: String::new(),
                        request_id: None,
                        executor_id: None,
                    },
                    text: String::new(),
                    timestamp: "00:00".into(),
                    markdown: None,
                },
                "  ⇲ ",
            ),
            (
                TraceEntry {
                    kind: TraceKind::UserMessage,
                    text: String::new(),
                    timestamp: "00:00".into(),
                    markdown: None,
                },
                "  > ",
            ),
            (
                TraceEntry {
                    kind: TraceKind::Permission {
                        description: "approve".into(),
                        pending: true,
                        countdown: 0,
                    },
                    text: String::new(),
                    timestamp: "00:00".into(),
                    markdown: None,
                },
                "  ? ",
            ),
        ];

        for (entry, expected) in entries {
            assert_eq!(compact_prefix_style(&entry.kind).0, expected);
        }
    }
}
