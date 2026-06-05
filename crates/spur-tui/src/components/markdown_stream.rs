//! Streaming markdown renderer for `AgentMessage` trace entries.
//!
//! Design: `append` is cheap (string push). `maybe_flush` is driven by the
//! UI tick; it rebuilds `cached_lines` when the stream has been dirty for
//! more than `DEBOUNCE` (50 ms) or when `flush_now` is called
//! (at `TurnComplete`).

use std::time::{Duration, Instant};

use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::mermaid::MermaidId;

pub const DEBOUNCE: Duration = Duration::from_millis(50);
pub const SAFETY_CAP_BYTES: usize = 64 * 1024;

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

fn starts_unordered_list_marker(trimmed: &str) -> bool {
    let mut chars = trimmed.chars();
    matches!(chars.next(), Some('-' | '+' | '*'))
        && chars.next().map(char::is_whitespace).unwrap_or(true)
}

fn starts_ordered_list_marker(trimmed: &str) -> bool {
    let mut digit_count = 0usize;
    let mut chars = trimmed.chars().peekable();
    while matches!(chars.peek(), Some(ch) if ch.is_ascii_digit()) {
        digit_count += 1;
        chars.next();
        if digit_count > 9 {
            return false;
        }
    }

    digit_count > 0
        && matches!(chars.next(), Some('.' | ')'))
        && chars.next().map(char::is_whitespace).unwrap_or(true)
}

fn is_thematic_break_line(trimmed: &str) -> bool {
    let mut marker: Option<char> = None;
    let mut count = 0usize;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            continue;
        }
        if !matches!(ch, '-' | '*' | '_') {
            return false;
        }
        match marker {
            Some(existing) if existing != ch => return false,
            Some(_) => {}
            None => marker = Some(ch),
        }
        count += 1;
    }
    count >= 3
}

fn is_setext_underline_line(trimmed: &str) -> bool {
    !trimmed.is_empty() && trimmed.chars().all(|ch| ch == '=' || ch == '-')
}

fn is_reference_definition_line(trimmed: &str) -> bool {
    trimmed.starts_with('[') && trimmed.contains("]:")
}

fn is_context_sensitive_markdown_block_marker_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return false;
    }

    let fully_trimmed = trimmed.trim_end();
    starts_unordered_list_marker(trimmed)
        || starts_ordered_list_marker(trimmed)
        || trimmed.starts_with('#')
        || trimmed.starts_with('>')
        || is_table_row(line)
        || is_table_separator(line)
        || is_setext_underline_line(fully_trimmed)
        || is_thematic_break_line(fully_trimmed)
        || is_reference_definition_line(trimmed)
        || line.starts_with("    ")
        || line.starts_with('\t')
}

fn fence_line_content(line: &str) -> Option<&str> {
    let mut spaces = 0usize;
    for (idx, ch) in line.char_indices() {
        match ch {
            ' ' if spaces < 3 => spaces += 1,
            ' ' => return None,
            '\t' => return None,
            _ => return Some(&line[idx..]),
        }
    }
    Some("")
}

fn parse_fence_opener(line: &str) -> Option<(u8, usize)> {
    let rest = fence_line_content(line)?;
    let marker = *rest.as_bytes().first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }

    let marker_len = rest.bytes().take_while(|byte| *byte == marker).count();
    if marker_len < 3 {
        return None;
    }

    if marker == b'`' && rest[marker_len..].contains('`') {
        return None;
    }

    Some((marker, marker_len))
}

fn is_fence_closer(line: &str, marker: u8, opener_len: usize) -> bool {
    let Some(rest) = fence_line_content(line) else {
        return false;
    };
    let marker_len = rest.bytes().take_while(|byte| *byte == marker).count();
    marker_len >= opener_len && rest[marker_len..].trim().is_empty()
}

fn is_context_insensitive_incremental_delta(delta: &str) -> bool {
    let mut open_fence: Option<(u8, usize)> = None;

    for line in delta.lines() {
        if let Some((marker, opener_len)) = open_fence {
            if is_fence_closer(line, marker, opener_len) {
                open_fence = None;
            }
            continue;
        }

        if line.trim().is_empty() {
            continue;
        }

        if is_context_sensitive_markdown_block_marker_line(line) {
            return false;
        }

        if let Some(fence) = parse_fence_opener(line) {
            open_fence = Some(fence);
        }
    }

    open_fence.is_none()
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

#[derive(Debug, Clone)]
struct MarkdownTable {
    alignments: Vec<Alignment>,
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

#[derive(Debug, Clone)]
struct RenderedTable {
    source_rows: Vec<String>,
    rendered_lines: Vec<Line<'static>>,
}

fn replace_markdown_tables_in_lines(
    lines: Vec<Line<'static>>,
    parse_input: &str,
    render_width: Option<u16>,
) -> Vec<Line<'static>> {
    let tables = rendered_markdown_tables(parse_input, render_width);
    if tables.is_empty() {
        return lines;
    }

    let mut out = Vec::with_capacity(lines.len());
    let mut line_idx = 0;
    let mut table_idx = 0;

    while line_idx < lines.len() {
        let Some(table) = tables.get(table_idx) else {
            out.extend(lines[line_idx..].iter().cloned());
            break;
        };

        if line_sequence_matches_table(&lines, line_idx, &table.source_rows) {
            out.extend(table.rendered_lines.iter().cloned());
            line_idx += table.source_rows.len();
            table_idx += 1;
        } else {
            out.push(lines[line_idx].clone());
            line_idx += 1;
        }
    }

    out
}

fn rendered_markdown_tables(parse_input: &str, render_width: Option<u16>) -> Vec<RenderedTable> {
    // Source rows are sliced from each table's own byte range (reported by
    // pulldown), so the rendered grid and the source lines it must match
    // against always correspond 1:1. This deliberately replaces an earlier
    // approach that scanned for table-ish line blocks with a separate lax
    // heuristic and bailed on ALL tables when its count disagreed with
    // pulldown — a divergence that flickers constantly while a later table's
    // delimiter row streams in column-by-column (RCA: streaming grid↔raw).
    collect_markdown_tables(parse_input)
        .into_iter()
        .filter_map(|(table, range)| {
            let source_rows: Vec<String> = parse_input
                .get(range)?
                .lines()
                .filter(|line| is_table_row(line))
                .map(normalize_table_source_row)
                .collect();
            if source_rows.is_empty() {
                return None;
            }
            let mut rendered_lines = render_markdown_table(&table, render_width);
            if rendered_lines.is_empty() && !table.header.is_empty() {
                rendered_lines = render_table_grid_fallback(&table);
            }
            Some(RenderedTable {
                source_rows,
                rendered_lines,
            })
        })
        .collect()
}

fn collect_markdown_tables(input: &str) -> Vec<(MarkdownTable, std::ops::Range<usize>)> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);

    let mut tables = Vec::new();
    let mut current_table: Option<MarkdownTable> = None;
    let mut current_table_start: usize = 0;
    let mut current_row: Option<Vec<String>> = None;
    let mut current_cell = String::new();
    let mut in_header = false;
    let mut in_cell = false;

    for (event, range) in Parser::new_ext(input, options).into_offset_iter() {
        match event {
            Event::Start(Tag::Table(alignments)) => {
                current_table_start = range.start;
                current_table = Some(MarkdownTable {
                    alignments,
                    header: Vec::new(),
                    rows: Vec::new(),
                });
            }
            Event::End(TagEnd::Table) => {
                if let Some(table) = current_table.take() {
                    if !table.header.is_empty() {
                        tables.push((table, current_table_start..range.end));
                    }
                }
            }
            Event::Start(Tag::TableHead) => {
                in_header = true;
                current_row = Some(Vec::new());
            }
            Event::End(TagEnd::TableHead) => {
                if let (Some(table), Some(row)) = (current_table.as_mut(), current_row.take()) {
                    table.header = row;
                }
                in_header = false;
            }
            Event::Start(Tag::TableRow) => {
                current_row = Some(Vec::new());
            }
            Event::End(TagEnd::TableRow) => {
                if let (Some(table), Some(row)) = (current_table.as_mut(), current_row.take()) {
                    if in_header {
                        table.header = row;
                    } else {
                        table.rows.push(row);
                    }
                }
            }
            Event::Start(Tag::TableCell) => {
                in_cell = true;
                current_cell.clear();
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(row) = current_row.as_mut() {
                    row.push(normalize_table_cell(&current_cell));
                }
                current_cell.clear();
                in_cell = false;
            }
            Event::Text(text) | Event::Code(text) | Event::Html(text) | Event::InlineHtml(text)
                if in_cell =>
            {
                current_cell.push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak if in_cell => {
                current_cell.push(' ');
            }
            _ => {}
        }
    }

    tables
}

fn render_markdown_table(table: &MarkdownTable, render_width: Option<u16>) -> Vec<Line<'static>> {
    let col_count = table
        .header
        .len()
        .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
    if col_count == 0 {
        return Vec::new();
    }

    let header = normalized_row(&table.header, col_count);
    let rows: Vec<Vec<String>> = table
        .rows
        .iter()
        .map(|row| normalized_row(row, col_count))
        .collect();
    let widths = table_column_widths(&header, &rows, col_count);
    let grid_width = table_grid_width(&widths);

    if let Some(max_width) = render_width.map(usize::from).filter(|width| *width > 0) {
        if grid_width > max_width && !rows.is_empty() {
            return render_table_records(&header, &rows, max_width);
        }
    }

    render_table_grid(&header, &rows, &widths, &table.alignments)
}

fn render_table_grid_fallback(table: &MarkdownTable) -> Vec<Line<'static>> {
    let col_count = table
        .header
        .len()
        .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
    if col_count == 0 {
        return Vec::new();
    }

    let header = normalized_row(&table.header, col_count);
    let rows: Vec<Vec<String>> = table
        .rows
        .iter()
        .map(|row| normalized_row(row, col_count))
        .collect();
    let widths = table_column_widths(&header, &rows, col_count);
    render_table_grid(&header, &rows, &widths, &table.alignments)
}

fn render_table_grid(
    header: &[String],
    rows: &[Vec<String>],
    widths: &[usize],
    alignments: &[Alignment],
) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(rows.len() + 4);
    out.push(table_line(top_border(widths)));
    out.push(table_line(format_table_row(header, widths, alignments)));
    out.push(table_line(header_border(widths)));
    for row in rows {
        out.push(table_line(format_table_row(row, widths, alignments)));
    }
    out.push(table_line(bottom_border(widths)));
    out
}

fn render_table_records(
    header: &[String],
    rows: &[Vec<String>],
    max_width: usize,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for (row_idx, row) in rows.iter().enumerate() {
        if row_idx > 0 {
            out.push(Line::from(""));
        }
        for (col_idx, value) in row.iter().enumerate() {
            let label = header
                .get(col_idx)
                .filter(|label| !label.is_empty())
                .cloned()
                .unwrap_or_else(|| format!("Column {}", col_idx + 1));
            out.push(table_line(truncate_to_width(
                &format!("{label}: {value}"),
                max_width,
            )));
        }
    }
    out
}

fn normalized_row(row: &[String], col_count: usize) -> Vec<String> {
    (0..col_count)
        .map(|idx| row.get(idx).cloned().unwrap_or_default())
        .collect()
}

fn table_column_widths(header: &[String], rows: &[Vec<String>], col_count: usize) -> Vec<usize> {
    let mut widths = vec![0; col_count];
    for (idx, cell) in header.iter().enumerate() {
        widths[idx] = widths[idx].max(display_width(cell));
    }
    for row in rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(display_width(cell));
        }
    }
    widths
}

fn table_grid_width(widths: &[usize]) -> usize {
    widths.iter().sum::<usize>() + widths.len() * 3 + 1
}

fn format_table_row(cells: &[String], widths: &[usize], alignments: &[Alignment]) -> String {
    let mut out = String::new();
    out.push('│');
    for (idx, cell) in cells.iter().enumerate() {
        out.push(' ');
        out.push_str(&pad_cell(
            cell,
            widths[idx],
            alignments.get(idx).copied().unwrap_or(Alignment::None),
        ));
        out.push(' ');
        out.push('│');
    }
    out
}

fn top_border(widths: &[usize]) -> String {
    border_line('┌', '┬', '┐', widths)
}

fn header_border(widths: &[usize]) -> String {
    border_line('├', '┼', '┤', widths)
}

fn bottom_border(widths: &[usize]) -> String {
    border_line('└', '┴', '┘', widths)
}

fn border_line(left: char, separator: char, right: char, widths: &[usize]) -> String {
    let mut out = String::new();
    out.push(left);
    for (idx, width) in widths.iter().enumerate() {
        out.push_str(&"─".repeat(width + 2));
        out.push(if idx + 1 == widths.len() {
            right
        } else {
            separator
        });
    }
    out
}

fn pad_cell(cell: &str, width: usize, alignment: Alignment) -> String {
    let cell_width = display_width(cell);
    let padding = width.saturating_sub(cell_width);
    match alignment {
        Alignment::Right => format!("{}{}", " ".repeat(padding), cell),
        Alignment::Center => {
            let left = padding / 2;
            let right = padding - left;
            format!("{}{}{}", " ".repeat(left), cell, " ".repeat(right))
        }
        Alignment::None | Alignment::Left => format!("{}{}", cell, " ".repeat(padding)),
    }
}

fn table_line(content: String) -> Line<'static> {
    Line::from(Span::raw(content))
}

fn line_sequence_matches_table(
    lines: &[Line<'static>],
    start: usize,
    source_rows: &[String],
) -> bool {
    start + source_rows.len() <= lines.len()
        && source_rows.iter().enumerate().all(|(offset, source)| {
            let rendered = line_plain_text(&lines[start + offset]);
            // Cells can carry inline markdown — `**bold**`, `` `code` ``,
            // links — which tui-markdown renders as styled spans with the
            // markers stripped, so the rendered plain text never equals the raw
            // source row (which keeps the markers). Match on the table-row shape
            // instead: both are `|`-delimited rows with the same column count,
            // which inline formatting cannot change. (Stripping markers from the
            // source is NOT an option — literal underscores in identifiers like
            // `collect_table_source_blocks` would be mangled into emphasis.)
            is_table_row(&rendered) && pipe_count(&rendered) == pipe_count(source)
        })
}

/// Count of `|` bytes in a (trimmed) line. For a GFM table row this is the
/// column count plus one, and it is invariant under inline-markdown rendering,
/// so it anchors the rendered grid to its source rows without depending on
/// cell text that the renderer rewrites.
fn pipe_count(line: &str) -> usize {
    line.trim().bytes().filter(|&b| b == b'|').count()
}

fn line_plain_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn normalize_table_cell(cell: &str) -> String {
    cell.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_table_source_row(line: &str) -> String {
    line.trim().trim_end_matches("  ").trim_end().to_string()
}

fn display_width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

fn truncate_to_width(value: &str, max_width: usize) -> String {
    if display_width(value) <= max_width {
        return value.to_string();
    }

    let mut out = String::new();
    let mut width = 0;
    for ch in value.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out
}

/// Minimum content width a column may be shrunk to when budgeting a grid to a
/// terminal width. Below this, the records layout is used instead.
#[allow(dead_code)]
const MIN_COL_WIDTH: usize = 4;

/// Wrap `cell` into physical lines each no wider than `width` display columns.
/// Breaks on ASCII spaces first; a single token wider than `width` is hard-split
/// on char boundaries by display width (never mid-emoji, never mid-wide-char).
#[allow(dead_code)]
fn wrap_cell_to_width(cell: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    if display_width(cell) <= width {
        return vec![cell.to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;
    for word in cell.split(' ') {
        let word_w = display_width(word);
        let sep = usize::from(!current.is_empty());
        if current_w + sep + word_w <= width {
            if sep == 1 {
                current.push(' ');
                current_w += 1;
            }
            current.push_str(word);
            current_w += word_w;
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_w = 0;
        }
        if word_w <= width {
            current.push_str(word);
            current_w = word_w;
        } else {
            for ch in word.chars() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if current_w + cw > width && !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                    current_w = 0;
                }
                current.push(ch);
                current_w += cw;
            }
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// Shrink column widths so the rendered grid fits within `render_width` display
/// columns, reducing the widest column first down to `MIN_COL_WIDTH`. Returns
/// `None` when even a floor-width grid cannot fit — the caller should then use
/// the records layout.
#[allow(dead_code)]
fn budget_column_widths(widths: &[usize], render_width: usize) -> Option<Vec<usize>> {
    let col_count = widths.len();
    if col_count == 0 {
        return Some(Vec::new());
    }
    let chrome = col_count * 3 + 1;
    let avail = render_width.checked_sub(chrome)?;
    if avail < col_count * MIN_COL_WIDTH {
        return None;
    }
    let mut out = widths.to_vec();
    let mut total: usize = out.iter().sum();
    while total > avail {
        // Reduce the widest column (ties → lowest index) by one column.
        let (idx, w) = out
            .iter()
            .copied()
            .enumerate()
            .max_by(|(ai, aw), (bi, bw)| aw.cmp(bw).then(bi.cmp(ai)))
            .expect("col_count > 0");
        if w <= MIN_COL_WIDTH {
            break; // avail >= col_count*MIN_COL_WIDTH guarantees we already fit
        }
        out[idx] = w - 1;
        total -= 1;
    }
    Some(out)
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

    /// Byte offset up to which `cached_items` is authoritative.
    /// Invariant (C1): cached_items, known_fences, fence_placeholders
    /// jointly represent the parsed-decorated form of raw_text[..flushed_byte_len].
    flushed_byte_len: usize,

    /// Set by `flush_final` when the stream is finalized (TurnComplete).
    /// `append` after finalize is a contract violation; enforced via
    /// debug_assert (see Task 11).
    finalized: bool,

    /// Production-unused; instrumented for test verification only.
    rebuild_count: std::cell::Cell<u64>,
    /// Production-unused; tracks bytes sent through the markdown build stage.
    build_work_bytes: std::cell::Cell<u64>,
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
            flushed_byte_len: 0,
            finalized: false,
            rebuild_count: std::cell::Cell::new(0),
            build_work_bytes: std::cell::Cell::new(0),
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

    /// Test-only accessor: returns the current flushed_byte_len cursor value.
    pub fn flushed_byte_len_for_tests(&self) -> usize {
        self.flushed_byte_len
    }

    /// Test-only hook: how many times has `rebuild()` been called?
    /// Used by busy-loop regression tests.
    pub fn rebuild_count_for_tests(&self) -> u64 {
        self.rebuild_count.get()
    }

    /// Test-only hook: total bytes handed to the markdown build stage.
    pub fn build_work_bytes_for_tests(&self) -> u64 {
        self.build_work_bytes.get()
    }

    pub fn is_finalized(&self) -> bool {
        self.finalized
    }

    /// Append a chunk of text. Cheap — does not reparse.
    ///
    /// Contract: callers must not append after `flush_final`. Enforced via
    /// `debug_assert!` in debug builds; in release the state self-heals
    /// (next rebuild runs under normal cursor rule).
    pub fn append(&mut self, text: &str) {
        debug_assert!(
            !self.finalized,
            "append after flush_final is a contract violation (MarkdownStream finalized)"
        );
        self.raw_text.push_str(text);
        self.dirty_since.get_or_insert_with(Instant::now);
    }

    /// Flush if conditions warrant. Priority order:
    /// 1. Not dirty → no-op (load-bearing: prevents busy-looping when
    ///    cursor fails to advance under the heuristic).
    /// 2. Empty raw_text → no-op.
    /// 3. Tail > SAFETY_CAP_BYTES without boundary → suppress rebuild,
    ///    clear dirty_since, let plain-text tail render until TurnComplete.
    /// 4. Tail contains authoritative closure pattern → flush immediately.
    /// 5. DEBOUNCE elapsed → flush.
    /// 6. Otherwise → no-op.
    pub fn maybe_flush(&mut self, states: &StateLookup<'_>) -> Vec<FenceRef> {
        let Some(dirty_at) = self.dirty_since else {
            return Vec::new();
        };
        if self.raw_text.is_empty() {
            return Vec::new();
        }

        let tail = &self.raw_text[self.flushed_byte_len..];
        let tail_len = tail.len();

        // Safety valve: large boundary-free tail.
        if tail_len > SAFETY_CAP_BYTES && !has_authoritative_closure_pattern(tail) {
            self.dirty_since = None;
            return Vec::new();
        }

        // Fast path: authoritative closure pattern present.
        if has_authoritative_closure_pattern(tail) {
            return self.flush_now(states);
        }

        // Debounce.
        if dirty_at.elapsed() >= DEBOUNCE {
            return self.flush_now(states);
        }

        Vec::new()
    }

    /// Force a flush immediately.
    pub fn flush_now(&mut self, states: &StateLookup<'_>) -> Vec<FenceRef> {
        self.dirty_since = None;
        self.rebuild_with_width(states, /* permit_eof_closure */ false, None)
    }

    /// TurnComplete flush. Permits cursor advance past events at EOF
    /// (`range.end == raw_text.len()`) since no more bytes will arrive.
    /// Sets `finalized = true`; subsequent `append` is a contract
    /// violation (debug_assert'd, self-heals in release).
    pub fn flush_final(&mut self, states: &StateLookup<'_>) -> Vec<FenceRef> {
        self.flush_final_with_width(states, None)
    }

    fn flush_final_with_width(
        &mut self,
        states: &StateLookup<'_>,
        render_width: Option<u16>,
    ) -> Vec<FenceRef> {
        self.dirty_since = None;
        let out = self.rebuild_with_width(states, /* permit_eof_closure */ true, render_width);
        self.finalized = true;
        out
    }

    /// Returns the StreamItems that `flush_final` would produce for the
    /// current `raw_text`, without mutating self. Callers use this to render
    /// the tail with the same row sequence final flush would emit, eliminating
    /// the tail-vs-items asymmetry that caused ghost text (RCA Layer 2A).
    ///
    /// Pure with respect to self. Cost is one pulldown parse pass per call;
    /// the parent VirtualRow cache amortizes across renders.
    pub fn preview_items(&self, states: &StateLookup<'_>) -> Vec<StreamItem> {
        self.preview_items_with_optional_width(states, None)
    }

    /// Width-aware preview for render paths that already know the available
    /// body width. This keeps cached stream state width-independent while
    /// allowing markdown tables to choose a narrow-terminal fallback.
    pub fn preview_items_with_width(
        &self,
        states: &StateLookup<'_>,
        render_width: u16,
    ) -> Vec<StreamItem> {
        self.preview_items_with_optional_width(states, Some(render_width))
    }

    fn preview_items_with_optional_width(
        &self,
        states: &StateLookup<'_>,
        render_width: Option<u16>,
    ) -> Vec<StreamItem> {
        if self.raw_text.is_empty() {
            return Vec::new();
        }

        // Fast path: when no new content has been committed since the last
        // flush, the cached items are already current and we can skip the
        // clone + full reflow. Only safe for the width-independent path: a
        // width-aware caller may need a narrow-terminal table fallback that
        // the cached (width-None) items do not reflect.
        if render_width.is_none() {
            let (preview_flushed, _) =
                scan_authoritative(&self.raw_text, self.mermaid_enabled, true);
            if preview_flushed == self.flushed_byte_len {
                return self.cached_items.clone();
            }
        }

        let mut clone = self.clone();
        // flush_final allows EOF closure (permit_eof_closure=true), so the
        // entire raw_text is committed and trailing tail bytes get the same
        // paragraph context they would after TurnComplete.
        let _ = clone.flush_final_with_width(states, render_width);
        clone.cached_items
    }

    /// Force the next `maybe_flush`/`flush_now` to rebuild even if not yet
    /// dirty by time. Used when external state (mermaid registry errors)
    /// changes so the placeholder reflects the new state on the next tick.
    pub fn mark_dirty_now(&mut self) {
        let now = Instant::now();
        self.dirty_since = Some(now.checked_sub(DEBOUNCE).unwrap_or(now));
    }

    pub fn items(&self) -> &[StreamItem] {
        &self.cached_items
    }

    /// Split view of committed parsed items + uncommitted tail text.
    ///
    /// - `items`: parsed StreamItems covering `raw_text[..flushed_byte_len]`.
    /// - `tail`: `raw_text[flushed_byte_len..]`, to be rendered as plain text.
    ///
    /// Renderers must emit both: items styled, tail plain.
    pub fn items_and_tail(&self) -> (&[StreamItem], &str) {
        (&self.cached_items, &self.raw_text[self.flushed_byte_len..])
    }

    /// Whether the stream has pending changes awaiting flush.
    pub fn is_dirty(&self) -> bool {
        self.dirty_since.is_some()
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

    /// Look up the state-aware placeholder line for a previously-registered
    /// fence id. Returns `None` for ids not in `fence_placeholders`.
    /// Used by `build_display_lines` (secondary render path) to render a
    /// placeholder line without constructing a `FenceRender` HashMap.
    pub fn fence_placeholder_for(&self, id: MermaidId) -> Option<Line<'static>> {
        self.fence_placeholders.get(&id).cloned()
    }

    /// Stages 2-5 of rebuild: given a prefix of raw_text and the closed
    /// mermaid fences discovered within that prefix, produce cached_items,
    /// fence_placeholders, and the list of NEW fences (not previously
    /// known). Caller is responsible for passing a consistent (prefix,
    /// discovered_fences) pair — `scan_authoritative(&self.raw_text[..X])`
    /// semantics, with X = prefix.len().
    fn build_items_for(
        &mut self,
        prefix: &str,
        discovered_fences: Vec<(std::ops::Range<usize>, String)>,
        states: &StateLookup<'_>,
        render_width: Option<u16>,
    ) -> (
        Vec<StreamItem>,
        std::collections::HashMap<MermaidId, Line<'static>>,
        Vec<FenceRef>,
        Vec<FenceRef>, // refreshed (to be stored as self.known_fences)
    ) {
        let mut new_fences: Vec<FenceRef> = Vec::new();
        let mut refreshed: Vec<FenceRef> = Vec::with_capacity(discovered_fences.len());
        for (range, code) in discovered_fences {
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

        // Build transformed input from the prefix + sentinels.
        let transformed = {
            let mut out = String::with_capacity(prefix.len());
            let mut cursor = 0usize;
            for f in &refreshed {
                if f.byte_range.start > cursor {
                    out.push_str(&prefix[cursor..f.byte_range.start]);
                }
                out.push_str(&format!("\n\u{0000}MERMAID:{}\u{0000}\n", f.id.0));
                cursor = f.byte_range.end;
            }
            if cursor < prefix.len() {
                out.push_str(&prefix[cursor..]);
            }
            out
        };

        let transformed = inject_hard_breaks_in_tables(&transformed);

        if transformed.is_empty() {
            return (
                Vec::new(),
                std::collections::HashMap::new(),
                new_fences,
                refreshed,
            );
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
        let parsed_lines =
            replace_markdown_tables_in_lines(parsed_lines, &transformed, render_width);

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

                use super::mermaid::FenceRender;
                let render = if states.is_err(id) {
                    FenceRender::Error
                } else if states.is_pending(id) {
                    FenceRender::Pending
                } else {
                    FenceRender::Ready(1)
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

        (items, placeholders, new_fences, refreshed)
    }

    /// Rebuild `cached_items` from raw_text. Two stages:
    /// Stage 0: pulldown scan for offsets + mermaid fences.
    /// Stage 1: tui_markdown parse of raw_text[..authoritative_end].
    ///
    /// `permit_eof_closure = true` relaxes the cursor rule to allow events
    /// at EOF; used by `flush_final` on TurnComplete.
    ///
    /// Panic safety: mutations to cached_items / fence_placeholders /
    /// known_fences happen before flushed_byte_len is assigned. If any
    /// stage panics, flushed_byte_len retains its prior value; the next
    /// successful rebuild restores consistency (C1).
    fn rebuild_with_width(
        &mut self,
        states: &StateLookup<'_>,
        permit_eof_closure: bool,
        render_width: Option<u16>,
    ) -> Vec<FenceRef> {
        self.rebuild_count.set(self.rebuild_count.get() + 1);

        // Stage 0: pulldown scan.
        let (new_flushed, discovered_fences) =
            scan_authoritative(&self.raw_text, self.mermaid_enabled, permit_eof_closure);

        let old_flushed = self.flushed_byte_len;
        let committed_fence_state_stale = self.known_fences.iter().any(|f| {
            use super::mermaid::FenceRender;

            let render = if states.is_err(f.id) {
                FenceRender::Error
            } else if states.is_pending(f.id) {
                FenceRender::Pending
            } else {
                FenceRender::Ready(1)
            };
            let expected = super::mermaid::fence_placeholder_line(f.id, render);
            self.fence_placeholders
                .get(&f.id)
                .map(|line| format!("{line:?}") != format!("{expected:?}"))
                .unwrap_or(true)
        });

        let can_try_incremental = new_flushed > old_flushed
            && old_flushed > 0
            && !committed_fence_state_stale
            && self.raw_text.is_char_boundary(old_flushed)
            && self.raw_text.is_char_boundary(new_flushed);

        if can_try_incremental {
            let delta = &self.raw_text[old_flushed..new_flushed];
            let before_line = self.raw_text[..old_flushed]
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty());
            let after_line = delta.lines().find(|line| !line.trim().is_empty());
            let table_seam = matches!(
                (before_line, after_line),
                (Some(before), Some(after))
                    if (is_table_row(before) || is_table_separator(before))
                        && (is_table_row(after) || is_table_separator(after))
            );
            let delta_has_reference_definition = delta.lines().any(|line| {
                let trimmed = line.trim_start();
                trimmed.starts_with('[') && trimmed.contains("]:")
            });
            let setext_seam = matches!(
                after_line,
                Some(line)
                    if before_line.is_some()
                        && {
                            let trimmed = line.trim();
                            !trimmed.is_empty()
                                && trimmed
                                    .chars()
                                    .all(|ch| ch == '=' || ch == '-')
                        }
            );
            let context_sensitive_seam = before_line
                .map(is_context_sensitive_markdown_block_marker_line)
                .unwrap_or(false)
                || after_line
                    .map(is_context_sensitive_markdown_block_marker_line)
                    .unwrap_or(false);
            let delta_context_insensitive = is_context_insensitive_incremental_delta(delta);

            if !table_seam
                && !delta_has_reference_definition
                && !setext_seam
                && !context_sensitive_seam
                && delta_context_insensitive
            {
                let mut delta_discovered = Vec::new();
                let mut fence_crosses_seam = false;
                for (range, code) in &discovered_fences {
                    if range.end <= old_flushed {
                        continue;
                    }
                    if range.start < old_flushed {
                        fence_crosses_seam = true;
                        break;
                    }
                    delta_discovered.push((
                        (range.start - old_flushed)..(range.end - old_flushed),
                        code.clone(),
                    ));
                }

                if !fence_crosses_seam {
                    let delta_owned = delta.to_owned();
                    self.build_work_bytes
                        .set(self.build_work_bytes.get() + delta_owned.len() as u64);

                    let mut delta_builder = Self {
                        raw_text: String::new(),
                        dirty_since: None,
                        cached_items: Vec::new(),
                        fence_placeholders: std::collections::HashMap::new(),
                        known_fences: Vec::new(),
                        next_fence_id: self.next_fence_id,
                        mermaid_enabled: self.mermaid_enabled,
                        flushed_byte_len: 0,
                        finalized: self.finalized,
                        rebuild_count: std::cell::Cell::new(0),
                        build_work_bytes: std::cell::Cell::new(0),
                    };
                    let (delta_items, delta_placeholders, delta_new_fences, delta_refreshed) =
                        delta_builder.build_items_for(
                            &delta_owned,
                            delta_discovered,
                            states,
                            render_width,
                        );

                    let new_fences: Vec<FenceRef> = delta_new_fences
                        .into_iter()
                        .map(|mut fence| {
                            fence.byte_range = (fence.byte_range.start + old_flushed)
                                ..(fence.byte_range.end + old_flushed);
                            fence
                        })
                        .collect();
                    let refreshed: Vec<FenceRef> = delta_refreshed
                        .into_iter()
                        .map(|mut fence| {
                            fence.byte_range = (fence.byte_range.start + old_flushed)
                                ..(fence.byte_range.end + old_flushed);
                            fence
                        })
                        .collect();

                    if let (Some(StreamItem::Text(cached_lines)), Some(StreamItem::Text(lines))) =
                        (self.cached_items.last_mut(), delta_items.first())
                    {
                        cached_lines.reserve(lines.len());
                    }
                    self.cached_items.reserve(delta_items.len());
                    self.fence_placeholders.reserve(delta_placeholders.len());
                    self.known_fences.reserve(refreshed.len());

                    let mut delta_iter = delta_items.into_iter();
                    if let Some(first) = delta_iter.next() {
                        match (self.cached_items.last_mut(), first) {
                            (Some(StreamItem::Text(cached_lines)), StreamItem::Text(mut lines)) => {
                                cached_lines.append(&mut lines);
                            }
                            (_, item) => self.cached_items.push(item),
                        }
                    }
                    self.cached_items.extend(delta_iter);
                    self.fence_placeholders.extend(delta_placeholders);
                    self.known_fences.extend(refreshed);
                    self.next_fence_id = delta_builder.next_fence_id;
                    self.flushed_byte_len = new_flushed;

                    return new_fences;
                }
            }
        }

        // Stage 1: build items for the committed prefix.
        // `.to_owned()` decouples the borrow on self.raw_text so we can
        // then mutate self inside build_items_for without overlapping
        // borrows.
        let prefix_owned = self.raw_text[..new_flushed].to_owned();
        self.build_work_bytes
            .set(self.build_work_bytes.get() + prefix_owned.len() as u64);
        let (items, placeholders, new_fences, refreshed) =
            self.build_items_for(&prefix_owned, discovered_fences, states, render_width);

        // Stage 2: commit. flushed_byte_len assigned LAST (panic discipline).
        self.cached_items = items;
        self.fence_placeholders = placeholders;
        self.known_fences = refreshed;
        self.flushed_byte_len = new_flushed;

        new_fences
    }
}

/// Pulldown scan over `raw_text`, gathering:
/// - `authoritative_end`: max byte offset where an Event::End brings
///   nesting depth back to 0 AND `range.end < raw_text.len()` (or
///   `<= len` when `permit_eof_closure` is true for flush_final).
/// - `discovered_fences`: closed mermaid fences whose End range is also
///   before EOF (coherence with cursor advance, per Section 5.9).
///
/// Pure over `(&str, bool, bool)`. Does no `tui_markdown` work.
pub(crate) fn scan_authoritative(
    raw_text: &str,
    mermaid_enabled: bool,
    permit_eof_closure: bool,
) -> (usize, Vec<(std::ops::Range<usize>, String)>) {
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

    let mut max_end: usize = 0;
    let mut depth: i32 = 0;
    let mut discovered: Vec<(std::ops::Range<usize>, String)> = Vec::new();

    let mut open_fence_start: Option<usize> = None;
    let mut fence_buf = String::new();

    for (ev, range) in Parser::new_ext(raw_text, Options::empty()).into_offset_iter() {
        match &ev {
            Event::Start(tag) => {
                depth += 1;
                if mermaid_enabled {
                    if let Tag::CodeBlock(CodeBlockKind::Fenced(info)) = tag {
                        if info.as_ref().trim().eq_ignore_ascii_case("mermaid") {
                            open_fence_start = Some(range.start);
                            fence_buf.clear();
                        }
                    }
                }
            }
            Event::Text(t) if open_fence_start.is_some() => {
                fence_buf.push_str(t);
            }
            Event::End(tag_end) => {
                depth -= 1;
                // Authoritative cursor advance: top-level block close.
                let permitted = if permit_eof_closure {
                    range.end <= raw_text.len()
                } else {
                    range.end < raw_text.len()
                };
                if depth == 0 && permitted {
                    max_end = max_end.max(range.end);
                }
                // Mermaid fence coherence: register only truly-closed fences
                // whose End range is before EOF (or permitted at finalize).
                if matches!(tag_end, TagEnd::CodeBlock) {
                    if let Some(start) = open_fence_start.take() {
                        let slice_trimmed =
                            raw_text[..range.end].trim_end_matches(['\n', '\r', ' ', '\t']);
                        let closed_by_fence = slice_trimmed.ends_with("```");
                        let fence_permitted = if permit_eof_closure {
                            range.end <= raw_text.len()
                        } else {
                            range.end < raw_text.len()
                        };
                        if closed_by_fence && fence_permitted {
                            discovered.push((start..range.end, std::mem::take(&mut fence_buf)));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    (max_end, discovered)
}

/// Public test accessor (plain `pub fn`, not `#[cfg(test)]` gated — the
/// existing convention in this module per `cached_lines_debug()`).
pub fn scan_authoritative_for_tests(
    raw_text: &str,
    mermaid_enabled: bool,
    permit_eof_closure: bool,
) -> (usize, Vec<(std::ops::Range<usize>, String)>) {
    scan_authoritative(raw_text, mermaid_enabled, permit_eof_closure)
}

/// Cheap stateless scan for patterns that typically indicate an
/// authoritative block close. False positives allowed (wasted rebuild);
/// false negatives bounded by DEBOUNCE.
pub(crate) fn has_authoritative_closure_pattern(tail: &str) -> bool {
    // (a) Paragraph / block close: `\n\n` with content after the last
    //     occurrence. Content-after required so we don't waste a rebuild
    //     on a tail whose trailing `\n\n` is at EOF (where pulldown
    //     emits End at range.end == len — non-authoritative).
    if let Some(idx) = tail.rfind("\n\n") {
        if idx + 2 < tail.len() {
            return true;
        }
    }
    // (b) Fence close on its own line with content after.
    if let Some(idx) = tail.find("\n```") {
        let after = idx + 4;
        if tail.as_bytes().get(after) == Some(&b'\n') && after + 1 < tail.len() {
            return true;
        }
    }
    false
}

/// Public test accessor (plain `pub fn`, following the module's existing
/// convention for test-only helpers).
pub fn has_authoritative_closure_pattern_for_tests(tail: &str) -> bool {
    has_authoritative_closure_pattern(tail)
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
        // "Outro prose\n\n" followed by more content ensures the cursor
        // advances past "Outro prose" (End(Paragraph) at range.end < len).
        s.append("Intro prose\n\n```mermaid\nflowchart LR\nA-->B\n```\n\nOutro prose\n\nMore\n");
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
    fn wrap_cell_to_width_breaks_on_spaces() {
        // "much longer value" wrapped to width 10 → ["much", "longer", "value"]
        let lines = wrap_cell_to_width("much longer value", 10);
        assert_eq!(lines, vec!["much", "longer", "value"]);
    }

    #[test]
    fn wrap_cell_to_width_packs_multiple_words_per_line() {
        // width 12 fits "much longer" (11) then "value"
        let lines = wrap_cell_to_width("much longer value", 12);
        assert_eq!(lines, vec!["much longer", "value"]);
    }

    #[test]
    fn wrap_cell_to_width_hard_splits_overlong_token() {
        // A single unbreakable token longer than width is split by display width.
        let lines = wrap_cell_to_width("add_pending_edge(&edge)", 8);
        assert!(lines.len() >= 3, "got: {lines:?}");
        assert!(
            lines.iter().all(|l| display_width(l) <= 8),
            "got: {lines:?}"
        );
        assert_eq!(lines.concat(), "add_pending_edge(&edge)");
    }

    #[test]
    fn wrap_cell_to_width_is_unicode_width_aware() {
        // ✅ is display width 2; width 2 budget holds exactly one per line.
        let lines = wrap_cell_to_width("✅✅✅", 2);
        assert_eq!(lines, vec!["✅", "✅", "✅"]);
    }

    #[test]
    fn wrap_cell_to_width_short_value_is_single_line() {
        assert_eq!(wrap_cell_to_width("short", 20), vec!["short"]);
    }

    #[test]
    fn budget_column_widths_noop_when_under_budget() {
        // Two columns of content width 5 + chrome (2*3+1=7) = 17 ≤ 40 → unchanged.
        let widths = vec![5usize, 5];
        assert_eq!(budget_column_widths(&widths, 40), Some(vec![5, 5]));
    }

    #[test]
    fn budget_column_widths_shrinks_widest_first() {
        // widths [30, 5], render_width 20 → chrome 7, avail 13.
        // Widest (col 0) shrinks until total ≤ 13: [8, 5].
        let widths = vec![30usize, 5];
        let out = budget_column_widths(&widths, 20).expect("fits at floor");
        assert_eq!(out.iter().sum::<usize>(), 13);
        assert!(
            out[0] >= MIN_COL_WIDTH && out[1] >= MIN_COL_WIDTH,
            "got: {out:?}"
        );
        assert!(
            out[1] == 5,
            "narrow column should not shrink below its content: {out:?}"
        );
    }

    #[test]
    fn budget_column_widths_returns_none_when_floor_cannot_fit() {
        // 3 columns need 3*MIN_COL_WIDTH + chrome(10) at minimum; width 12 is too small.
        let widths = vec![10usize, 10, 10];
        assert_eq!(budget_column_widths(&widths, 12), None);
    }

    #[test]
    fn gfm_table_is_wrapped_and_preserves_line_boundaries() {
        // Without the wrap, tui-markdown flows these rows into one paragraph
        // line. With the wrap, they must render as separate lines.
        // Trailing paragraph ensures the cursor advances past the table block.
        let mut s = MarkdownStream::new();
        s.append("| Key | Action |\n|---|---|\n| Esc | cancel |\n| Enter | submit |\n\nEnd\n");
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
    fn basic_gfm_table_renders_as_aligned_grid() {
        let mut s = MarkdownStream::new();
        s.append("| Key | Action |\n|---|---|\n| Esc | cancel |\n");
        let _ = s.flush_final(&StateLookup::empty());

        let rendered = s.cached_lines_debug().join("\n");
        let expected = [
            "┌─────┬────────┐",
            "│ Key │ Action │",
            "├─────┼────────┤",
            "│ Esc │ cancel │",
            "└─────┴────────┘",
        ]
        .join("\n");

        assert!(
            rendered.contains(&expected),
            "expected aligned grid:\n{expected}\n\ngot:\n{rendered}"
        );
        assert!(
            !rendered.contains("|---|---|"),
            "delimiter row must not render raw:\n{rendered}"
        );
    }

    #[test]
    fn gfm_table_pads_uneven_column_widths() {
        let mut s = MarkdownStream::new();
        s.append(
            "| Name | Description |\n\
             |---|---|\n\
             | a | short |\n\
             | longer-name | much longer value |\n",
        );
        let _ = s.flush_final(&StateLookup::empty());

        let rendered = s.cached_lines_debug().join("\n");
        assert!(
            rendered.contains("│ Name        │ Description       │"),
            "header cells should be padded to column widths:\n{rendered}"
        );
        assert!(
            rendered.contains("│ a           │ short             │"),
            "short row cells should be padded:\n{rendered}"
        );
        assert!(
            rendered.contains("│ longer-name │ much longer value │"),
            "long row should preserve the widest content:\n{rendered}"
        );
    }

    #[test]
    fn streamed_wide_header_only_table_never_renders_raw_at_narrow_width() {
        fn preview_text(stream: &MarkdownStream, width: u16) -> String {
            stream
                .preview_items_with_width(&StateLookup::empty(), width)
                .iter()
                .filter_map(|item| match item {
                    StreamItem::Text(lines) => Some(
                        lines
                            .iter()
                            .map(line_plain_text)
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    StreamItem::Fence(_) => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        }

        let width = 50;
        let header = "| Feature | Description with enough width to overflow |\n";
        let separator = "|---|---|\n";
        let rows = [
            (
                "| Auth | Handles login and session refresh for users |\n",
                "Auth",
            ),
            (
                "| Billing | Presents invoices, plans, and payment history |\n",
                "Billing",
            ),
        ];
        let mut stream = MarkdownStream::new();

        stream.append(header);
        stream.append(separator);

        let header_only = preview_text(&stream, width);
        assert!(
            header_only.contains('┌') && header_only.contains('│'),
            "header-only streaming table must render as a grid:\n{header_only}"
        );
        assert!(
            header_only.contains("Feature") && header_only.contains("Description"),
            "header-only table content must not be dropped:\n{header_only}"
        );

        let mut rendered_steps = vec![header_only];
        for (row, expected) in rows {
            stream.append(row);
            let rendered = preview_text(&stream, width);
            assert!(
                rendered.contains("Feature") && rendered.contains(expected),
                "streamed table content must stay represented:\n{rendered}"
            );
            rendered_steps.push(rendered);
        }

        for rendered in rendered_steps {
            assert!(
                !rendered.contains("|---") && !rendered.contains("| Feature |"),
                "streamed wide table must never render raw markdown rows:\n{rendered}"
            );
        }
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
        // Trailing paragraph ensures cursor advances past "That's it." block.
        let mut s = MarkdownStream::new();
        s.append(
            "Here are the keybindings:\n\
             | Key | Action |\n\
             |---|---|\n\
             | Esc | cancel |\n\
             | Enter | submit |\n\
             That's it.\n\
             \nDone\n",
        );
        let _ = s.flush_now(&StateLookup::empty());
        let joined = s.cached_lines_debug().join("\n");

        assert!(!joined.contains("```"), "got:\n{joined}");
        for needle in [
            "keybindings:",
            "│ Key   │ Action │",
            "│ Esc   │ cancel │",
            "│ Enter │ submit │",
            "That's it.",
        ] {
            assert!(
                joined.lines().any(|l| l.contains(needle)),
                "expected {needle:?} on its own line; got:\n{joined}"
            );
        }
        assert!(
            !joined.contains("|---|---|"),
            "delimiter row must not render raw:\n{joined}"
        );

        let merged = joined
            .lines()
            .any(|l| l.contains("keybindings:") && l.contains("| Key"));
        assert!(!merged, "prose merged with first row:\n{joined}");
    }
}
