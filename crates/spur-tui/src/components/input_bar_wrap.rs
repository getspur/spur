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

/// Wrap `lines` to `width` cells. Pure function.
pub fn wrap(_lines: &[String], _width: u16) -> WrapLayout {
    unimplemented!("Task 3")
}
