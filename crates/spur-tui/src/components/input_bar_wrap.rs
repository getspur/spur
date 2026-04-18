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
pub fn wrap(_lines: &[String], _width: u16) -> WrapLayout {
    unimplemented!("Task 3")
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
}
