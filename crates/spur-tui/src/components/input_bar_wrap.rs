//! Pure soft-wrap for the InputBar.
//!
//! Computes a `WrapLayout` that maps between logical `(row, byte_col)` and
//! visual `(vrow, vcol)` coordinates. The wrap algorithm is word-boundary
//! with grapheme-cluster fallback, unicode-width aware.
//!
//! This module is deliberately free of ratatui types (except `u16`) so it can
//! be unit-tested without any terminal backend. See
//! `docs/superpowers/specs/2026-04-18-input-bar-soft-wrap-design.md`.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// How many display cells a `\t` expands to.
pub const TAB_WIDTH: usize = 4;

/// A single grapheme cluster with its byte range (within its logical line)
/// and display-cell width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grapheme {
    pub byte_start: usize,
    pub byte_end: usize,
    pub width: u8,
}

/// One visual row in the wrapped layout.
#[derive(Debug, Clone)]
pub struct VisualRow {
    pub logical_row: usize,
    /// Byte offset within the logical line where this vrow starts.
    pub byte_start: usize,
    /// Byte offset within the logical line where this vrow ends (exclusive).
    pub byte_end: usize,
    pub graphemes: Vec<Grapheme>,
    pub used_cells: u16,
}

/// The full wrapped layout for a buffer at a given width.
#[derive(Debug, Clone)]
pub struct WrapLayout {
    pub rows: Vec<VisualRow>,
    /// `line_to_vrows[k] = (start_idx, end_idx_exclusive)` into `rows`.
    pub line_to_vrows: Vec<(usize, usize)>,
    pub width: u16,
}

impl WrapLayout {
    /// Total visual rows.
    pub fn visual_height(&self) -> u16 {
        self.rows.len() as u16
    }
}

/// Display cells for a single grapheme cluster.
///
/// - `\t` expands to `TAB_WIDTH` cells.
/// - Zero-width standalone graphemes (e.g., `U+200B` zero-width space) are
///   reported as 1 cell to keep cursor math well-defined.
fn grapheme_cells(g: &str) -> u8 {
    if g == "\t" {
        return TAB_WIDTH as u8;
    }
    let w = UnicodeWidthStr::width(g);
    w.max(1) as u8
}

/// Wrap `lines` to `width` cells. Pure function.
pub fn wrap(lines: &[String], width: u16) -> WrapLayout {
    assert!(width > 0, "wrap width must be positive");

    let mut rows: Vec<VisualRow> = Vec::new();
    let mut line_to_vrows: Vec<(usize, usize)> = Vec::with_capacity(lines.len());

    for (logical_row, line) in lines.iter().enumerate() {
        let graphemes: Vec<Grapheme> = line
            .grapheme_indices(true)
            .map(|(byte_start, g)| Grapheme {
                byte_start,
                byte_end: byte_start + g.len(),
                width: grapheme_cells(g),
            })
            .collect();

        let vstart = rows.len();

        if graphemes.is_empty() {
            rows.push(VisualRow {
                logical_row,
                byte_start: 0,
                byte_end: 0,
                graphemes: Vec::new(),
                used_cells: 0,
            });
            line_to_vrows.push((vstart, rows.len()));
            continue;
        }

        let mut cur: Vec<Grapheme> = Vec::new();
        let mut cur_cells: u16 = 0;
        let mut cur_byte_start: usize = graphemes[0].byte_start;

        for &g in &graphemes {
            // Pathological: a single grapheme wider than the whole width.
            // Emit it on its own row to avoid an infinite loop.
            if g.width as u16 > width {
                if !cur.is_empty() {
                    let byte_end = cur.last().unwrap().byte_end;
                    let taken = std::mem::take(&mut cur);
                    rows.push(VisualRow {
                        logical_row,
                        byte_start: cur_byte_start,
                        byte_end,
                        graphemes: taken,
                        used_cells: cur_cells,
                    });
                    cur_cells = 0;
                }
                rows.push(VisualRow {
                    logical_row,
                    byte_start: g.byte_start,
                    byte_end: g.byte_end,
                    graphemes: vec![g],
                    used_cells: g.width as u16,
                });
                cur_byte_start = g.byte_end;
                continue;
            }

            if cur_cells + g.width as u16 > width {
                // Look for the trailing-most whitespace break opportunity in
                // `cur`, but leave at least one grapheme before it.
                let mut break_at: Option<usize> = None;
                for (j, cg) in cur.iter().enumerate().rev() {
                    if j > 0 && is_break_opportunity(line, cg.byte_start, cg.byte_end) {
                        break_at = Some(j);
                        break;
                    }
                }

                if let Some(bi) = break_at {
                    // The whitespace stays on the current row (trailing);
                    // carry the post-whitespace graphemes to the next row.
                    let carry: Vec<Grapheme> = cur.drain(bi + 1..).collect();
                    cur_cells = cur.iter().map(|g| g.width as u16).sum();
                    let byte_end = cur.last().unwrap().byte_end;
                    let taken = std::mem::take(&mut cur);
                    rows.push(VisualRow {
                        logical_row,
                        byte_start: cur_byte_start,
                        byte_end,
                        graphemes: taken,
                        used_cells: cur_cells,
                    });
                    cur_byte_start = carry.first().map(|g| g.byte_start).unwrap_or(g.byte_start);
                    cur = carry;
                    cur_cells = cur.iter().map(|g| g.width as u16).sum();
                } else {
                    // No word-boundary in current row → break at grapheme.
                    let byte_end = cur.last().unwrap().byte_end;
                    let taken = std::mem::take(&mut cur);
                    rows.push(VisualRow {
                        logical_row,
                        byte_start: cur_byte_start,
                        byte_end,
                        graphemes: taken,
                        used_cells: cur_cells,
                    });
                    cur_cells = 0;
                    cur_byte_start = g.byte_start;
                }
            }

            cur.push(g);
            cur_cells += g.width as u16;
        }

        if !cur.is_empty() {
            let byte_end = cur.last().unwrap().byte_end;
            rows.push(VisualRow {
                logical_row,
                byte_start: cur_byte_start,
                byte_end,
                graphemes: std::mem::take(&mut cur),
                used_cells: cur_cells,
            });
        }

        line_to_vrows.push((vstart, rows.len()));
    }

    WrapLayout { rows, line_to_vrows, width }
}

/// A position we can legally break after.
fn is_break_opportunity(line: &str, byte_start: usize, byte_end: usize) -> bool {
    let s = &line[byte_start..byte_end];
    s.chars().any(|c| c.is_whitespace())
        || matches!(s, "、" | "。" | "，" | "．" | "　")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grapheme_cells_ascii() {
        assert_eq!(grapheme_cells("a"), 1);
        assert_eq!(grapheme_cells("!"), 1);
    }

    #[test]
    fn grapheme_cells_tab_expands_to_tab_width() {
        assert_eq!(grapheme_cells("\t"), TAB_WIDTH as u8);
    }

    #[test]
    fn grapheme_cells_cjk_is_two() {
        assert_eq!(grapheme_cells("你"), 2);
        assert_eq!(grapheme_cells("界"), 2);
    }

    #[test]
    fn grapheme_cells_zwj_emoji_family_is_two() {
        // Man + ZWJ + Woman + ZWJ + Girl = one grapheme cluster, width 2.
        let family = "👨\u{200D}👩\u{200D}👧";
        assert_eq!(grapheme_cells(family), 2);
    }

    #[test]
    fn grapheme_cells_combining_mark_is_one() {
        // "e" + combining acute = one cluster, width 1.
        let e_acute = "e\u{0301}";
        assert_eq!(grapheme_cells(e_acute), 1);
    }

    #[test]
    fn grapheme_cells_zero_width_claims_one_cell_defensively() {
        // Zero-width standalone grapheme — defensively claim 1 cell so
        // cursor-positioning math never divides by zero.
        assert!(grapheme_cells("\u{200B}") >= 1);
    }

    fn v(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn wrap_empty_buffer_is_one_vrow() {
        let layout = wrap(&v(&[""]), 10);
        assert_eq!(layout.visual_height(), 1);
        assert_eq!(layout.rows[0].graphemes.len(), 0);
        assert_eq!(layout.rows[0].used_cells, 0);
    }

    #[test]
    fn wrap_ascii_long_line() {
        let layout = wrap(&v(&["the quick brown fox jumps over the lazy dog"]), 20);
        assert_eq!(layout.visual_height(), 3);
        // All vrows must belong to logical row 0.
        assert!(layout.rows.iter().all(|r| r.logical_row == 0));
    }

    #[test]
    fn wrap_cjk_counts_cells_not_bytes() {
        // 10 CJK chars, each 2 cells wide, width 10 → 2 vrows of 5 chars each.
        let layout = wrap(&v(&["你好世界你好世界你好"]), 10);
        assert_eq!(layout.visual_height(), 2);
        assert_eq!(layout.rows[0].used_cells, 10);
        assert_eq!(layout.rows[1].used_cells, 10);
    }

    #[test]
    fn wrap_multiple_logical_lines_including_empty() {
        let layout = wrap(&v(&["first line here", "second", "", "fourth line long enough"]), 12);
        // Empty line still occupies one visual row.
        assert!(layout.visual_height() >= 4);
        // line_to_vrows must cover all 4 logical lines.
        assert_eq!(layout.line_to_vrows.len(), 4);
    }

    #[test]
    fn wrap_prefers_word_boundary() {
        let layout = wrap(&v(&["hello world foo"]), 8);
        // First vrow must end with a space (word-boundary break), not mid-word.
        let first = &layout.rows[0];
        let last_g = first.graphemes.last().unwrap();
        let txt = &"hello world foo"[last_g.byte_start..last_g.byte_end];
        assert!(txt == " " || first.byte_end <= "hello".len() + 1,
                "expected word-boundary break, got {:?}", txt);
    }

    #[test]
    fn wrap_long_word_falls_back_to_grapheme_break() {
        let layout = wrap(&v(&["antidisestablishmentarianism"]), 10);
        assert!(layout.visual_height() >= 3);
        // No empty rows.
        assert!(layout.rows.iter().all(|r| r.used_cells > 0));
    }

    #[test]
    fn wrap_width_one_emits_one_grapheme_per_row() {
        let layout = wrap(&v(&["abc"]), 1);
        assert_eq!(layout.visual_height(), 3);
        for r in &layout.rows {
            assert_eq!(r.graphemes.len(), 1);
        }
    }

    #[test]
    fn wrap_tabs_respect_tab_width() {
        let layout = wrap(&v(&["a\tbcd\tef"]), 8);
        // "a\t" = 1 + 4 = 5 cells → fits. "bcd\t" = 3 + 4 = 7 cells → would
        // exceed 5 + 7 = 12 > 8, so wrap. Exact geometry verified by
        // total cell conservation.
        let total_cells: u32 = layout.rows.iter().map(|r| r.used_cells as u32).sum();
        // Source has: 'a' (1) + '\t' (4) + "bcd" (3) + '\t' (4) + "ef" (2) = 14.
        assert_eq!(total_cells, 14);
    }
}
