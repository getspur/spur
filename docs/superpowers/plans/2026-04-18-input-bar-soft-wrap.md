# InputBar Soft-Wrap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `InputBar` soft-wrap long lines visually while keeping `tui-textarea` as the edit model and preserving the `protected_ranges` byte invariant.

**Architecture:** Introduce a pure `wrap(lines, width) -> WrapLayout` function in a new `components/input_bar_wrap.rs` module that owns bidirectional logical↔visual coordinate mapping. Rewrite `InputBar::render()` to paint through the layout (no longer calling `frame.render_widget(&self.textarea, ...)`). Rewrite `InputBar::required_height()` to call `WrapLayout::visual_height()`. Add visual-line `Up`/`Down` for Emacs and Vim Insert modes; Vim Normal `j`/`k` stay logical.

**Tech Stack:** Rust, ratatui 0.29, tui-textarea 0.7, `unicode-segmentation` 1.13, `unicode-width` 0.2 (all already in `Cargo.lock`).

**Design spec:** `docs/superpowers/specs/2026-04-18-input-bar-soft-wrap-design.md`
**Grounding simulation:** `/tmp/wrap-sim/` (13 adversarial cases, all passing)

---

## File Structure

- **Create** `crates/spur-tui/src/components/input_bar_wrap.rs` — pure wrap module. Owns `Grapheme`, `VisualRow`, `WrapLayout`, `wrap()`, `TAB_WIDTH`. No ratatui types except `u16`. Heavy unit-test section.
- **Modify** `crates/spur-tui/src/components/mod.rs` — one line: `pub mod input_bar_wrap;`
- **Modify** `crates/spur-tui/src/components/input_bar.rs`:
  - Rewrite `required_height()` (lines 978–999) to use `WrapLayout::visual_height()`.
  - Rewrite `render()` (lines 1002–1031) to paint via the layout.
  - Add two helpers: `visual_line_up()`, `visual_line_down()`.
  - Intercept `KeyCode::Up`/`KeyCode::Down` in `handle_emacs_input` and `handle_vim_insert_input` to call the new helpers.
- **Unchanged:** `session_detail.rs`, `dashboard.rs`, keymap code in vim-normal, `protected_ranges` logic, all existing `Ctrl+*` bindings.

---

## Task 1: Scaffold the wrap module with empty types

**Files:**
- Create: `crates/spur-tui/src/components/input_bar_wrap.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`

- [ ] **Step 1: Create the module file with type shells.**

Write `crates/spur-tui/src/components/input_bar_wrap.rs`:

```rust
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
```

- [ ] **Step 2: Register the module.**

Edit `crates/spur-tui/src/components/mod.rs` — add `pub mod input_bar_wrap;` in alphabetical order (between `input_bar` and `issue_detail_pane`):

```rust
pub mod input_bar;
pub mod input_bar_wrap;
pub mod issue_detail_pane;
```

- [ ] **Step 3: Verify it compiles.**

Run: `cargo build -p spur-tui`
Expected: SUCCESS (warning about `unimplemented!` unreachable is fine).

- [ ] **Step 4: Commit.**

```bash
git add crates/spur-tui/src/components/input_bar_wrap.rs \
        crates/spur-tui/src/components/mod.rs
git commit -m "feat(spur-tui): scaffold input_bar_wrap module types"
```

---

## Task 2: Add grapheme-width helper with tab expansion

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar_wrap.rs`

- [ ] **Step 1: Write failing tests.**

Append to `input_bar_wrap.rs`:

```rust
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
```

- [ ] **Step 2: Run the tests, confirm they fail.**

Run: `cargo test -p spur-tui --lib input_bar_wrap`
Expected: FAIL (function `grapheme_cells` not found).

- [ ] **Step 3: Implement the helper.**

Add above `wrap()` in `input_bar_wrap.rs`:

```rust
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
```

- [ ] **Step 4: Run the tests, confirm they pass.**

Run: `cargo test -p spur-tui --lib input_bar_wrap`
Expected: 6 tests pass.

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-tui/src/components/input_bar_wrap.rs
git commit -m "feat(input_bar_wrap): grapheme_cells with tab and unicode handling"
```

---

## Task 3: Implement the wrap algorithm

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar_wrap.rs`

- [ ] **Step 1: Add failing tests for wrap geometry.**

Append to the `tests` module:

```rust
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
```

- [ ] **Step 2: Run tests, confirm they fail.**

Run: `cargo test -p spur-tui --lib input_bar_wrap`
Expected: all 8 new tests FAIL (panics in `unimplemented!`).

- [ ] **Step 3: Replace the `wrap()` stub with the real implementation.**

In `input_bar_wrap.rs` replace the `unimplemented!` body:

```rust
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
```

- [ ] **Step 4: Run the tests, confirm they pass.**

Run: `cargo test -p spur-tui --lib input_bar_wrap`
Expected: all 14 tests pass (6 from Task 2 + 8 from Task 3).

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-tui/src/components/input_bar_wrap.rs
git commit -m "feat(input_bar_wrap): word-boundary wrap with grapheme fallback"
```

---

## Task 4: Add logical↔visual coordinate mapping

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar_wrap.rs`

- [ ] **Step 1: Add failing tests for mapping identity.**

Append to the `tests` module:

```rust
    /// Assert round-trip identity over every grapheme boundary + EOL, tolerating
    /// the legal vrow-boundary aliasing (end of vrow N ≡ start of vrow N+1).
    fn assert_roundtrip(lines: &[String], layout: &WrapLayout, case: &str) {
        for (row, line) in lines.iter().enumerate() {
            let mut positions: Vec<usize> =
                line.grapheme_indices(true).map(|(i, _)| i).collect();
            positions.push(line.len());

            for &bc in &positions {
                let (vr, vc) = layout.logical_to_visual(row, bc);
                let (row2, bc2) = layout.visual_to_logical(vr, vc);
                if row2 == row && bc2 == bc {
                    continue;
                }
                let (_, vend) = layout.line_to_vrows[row];
                let at_vrow_boundary = vr + 1 < vend
                    && bc == layout.rows[vr].byte_end
                    && bc2 == layout.rows[vr + 1].byte_start;
                assert!(
                    at_vrow_boundary,
                    "[{case}] round-trip FAILED: (row={row}, bc={bc}) → vis=({vr},{vc}) → ({row2},{bc2})"
                );
            }
        }
    }

    #[test]
    fn mapping_roundtrip_ascii() {
        let lines = v(&["the quick brown fox jumps over the lazy dog"]);
        let layout = wrap(&lines, 20);
        assert_roundtrip(&lines, &layout, "ascii");
    }

    #[test]
    fn mapping_roundtrip_cjk() {
        let lines = v(&["你好世界你好世界你好"]);
        let layout = wrap(&lines, 10);
        assert_roundtrip(&lines, &layout, "cjk");
    }

    #[test]
    fn mapping_roundtrip_zwj_emoji() {
        let family = "👨\u{200D}👩\u{200D}👧";
        let lines = v(&[&format!("ab{family}cd")]);
        let layout = wrap(&lines, 5);
        assert_roundtrip(&lines, &layout, "zwj");
    }

    #[test]
    fn mapping_roundtrip_combining_mark() {
        let lines = v(&["cafe\u{0301} shop"]);
        let layout = wrap(&lines, 6);
        assert_roundtrip(&lines, &layout, "combining");
    }

    #[test]
    fn mapping_roundtrip_tabs() {
        let lines = v(&["a\tbcd\tef"]);
        let layout = wrap(&lines, 8);
        assert_roundtrip(&lines, &layout, "tabs");
    }

    #[test]
    fn mapping_roundtrip_multiple_lines() {
        let lines = v(&["first line here", "second", "", "fourth line long enough"]);
        let layout = wrap(&lines, 12);
        assert_roundtrip(&lines, &layout, "multiline");
    }

    #[test]
    fn mapping_roundtrip_width_one() {
        let lines = v(&["abc 你 x"]);
        let layout = wrap(&lines, 1);
        assert_roundtrip(&lines, &layout, "width1");
    }

    #[test]
    fn mapping_eol_sits_at_end_of_last_vrow() {
        let lines = v(&["hello"]);
        let layout = wrap(&lines, 80);
        let (vr, vc) = layout.logical_to_visual(0, "hello".len());
        assert_eq!(vr, 0);
        assert_eq!(vc, 5);
    }
```

- [ ] **Step 2: Run tests, confirm they fail.**

Run: `cargo test -p spur-tui --lib input_bar_wrap`
Expected: 8 new tests FAIL (no such methods).

- [ ] **Step 3: Implement the two mapping methods on `WrapLayout`.**

In `input_bar_wrap.rs`, extend the existing `impl WrapLayout` block:

```rust
impl WrapLayout {
    pub fn visual_height(&self) -> u16 {
        self.rows.len() as u16
    }

    /// Map logical `(row, byte_col)` → visual `(vrow, vcol)`.
    ///
    /// `byte_col` is a byte offset within `lines[row]`. Positions must lie on
    /// a grapheme boundary or at end-of-line; other positions are clamped to
    /// the next grapheme boundary.
    pub fn logical_to_visual(&self, row: usize, byte_col: usize) -> (usize, usize) {
        let (vstart, vend) = self.line_to_vrows[row];
        for vi in vstart..vend {
            let vr = &self.rows[vi];
            if byte_col >= vr.byte_start && byte_col < vr.byte_end {
                let mut vcol: u16 = 0;
                for g in &vr.graphemes {
                    if g.byte_start >= byte_col {
                        break;
                    }
                    vcol += g.width as u16;
                }
                return (vi, vcol as usize);
            }
        }
        // EOL: place at the end of the last vrow on this logical line.
        let last = vend - 1;
        (last, self.rows[last].used_cells as usize)
    }

    /// Map visual `(vrow, vcol)` → logical `(row, byte_col)`.
    ///
    /// `vcol` is clamped to the row's `used_cells`. If `vcol` lands inside a
    /// wide grapheme, the result is the grapheme's starting byte.
    pub fn visual_to_logical(&self, vrow: usize, vcol: usize) -> (usize, usize) {
        let vr = &self.rows[vrow];
        let mut cells: u16 = 0;
        for g in &vr.graphemes {
            if cells as usize + g.width as usize > vcol {
                return (vr.logical_row, g.byte_start);
            }
            cells += g.width as u16;
        }
        (vr.logical_row, vr.byte_end)
    }
}
```

(Remove the previous single-method `impl WrapLayout { pub fn visual_height ... }` block so there's only one.)

- [ ] **Step 4: Run the tests, confirm they pass.**

Run: `cargo test -p spur-tui --lib input_bar_wrap`
Expected: 22 tests pass (14 from prior + 8 new).

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-tui/src/components/input_bar_wrap.rs
git commit -m "feat(input_bar_wrap): bidirectional logical<->visual mapping"
```

---

## Task 5: Add atom-conservation and visual-height oracle tests

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar_wrap.rs`

- [ ] **Step 1: Add failing tests.**

Append to the `tests` module:

```rust
    #[test]
    fn atom_cells_conserved_across_wrap_boundary() {
        let text = "text before @resource/long-name.txt text after";
        let lines = v(&[text]);
        let layout = wrap(&lines, 20);

        let atom_start = text.find('@').unwrap();
        let atom_end = atom_start + "@resource/long-name.txt".len();

        let mut counted_cells: u16 = 0;
        for vr in &layout.rows {
            for g in &vr.graphemes {
                if g.byte_start >= atom_start && g.byte_end <= atom_end {
                    counted_cells += g.width as u16;
                }
            }
        }

        let expected: u16 = text[atom_start..atom_end]
            .graphemes(true)
            .map(|g| grapheme_cells(g) as u16)
            .sum();
        assert_eq!(counted_cells, expected, "atom cells must be conserved across wrap");
    }

    #[test]
    fn visual_height_80w_empty_is_1() {
        assert_eq!(wrap(&v(&[""]), 80).visual_height(), 1);
    }

    #[test]
    fn visual_height_80w_80chars_is_1() {
        let s = "a".repeat(80);
        assert_eq!(wrap(&v(&[&s]), 80).visual_height(), 1);
    }

    #[test]
    fn visual_height_80w_81chars_is_2() {
        let s = "a".repeat(81);
        assert_eq!(wrap(&v(&[&s]), 80).visual_height(), 2);
    }

    #[test]
    fn visual_height_40w_200chars_is_5() {
        let s = "a".repeat(200);
        assert_eq!(wrap(&v(&[&s]), 40).visual_height(), 5);
    }
```

- [ ] **Step 2: Run tests, confirm they pass.**

Run: `cargo test -p spur-tui --lib input_bar_wrap`
Expected: 27 tests pass (22 + 5). These are oracle-style tests that the existing `wrap()` already satisfies — they exist to pin the contract.

- [ ] **Step 3: Commit.**

```bash
git add crates/spur-tui/src/components/input_bar_wrap.rs
git commit -m "test(input_bar_wrap): atom conservation and visual_height oracles"
```

---

## Task 6: Switch `required_height` to use `WrapLayout`

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`

- [ ] **Step 1: Add failing test.**

At the end of `input_bar.rs`, before the final `impl Default`, add:

```rust
#[cfg(test)]
mod required_height_tests {
    use super::*;

    #[test]
    fn required_height_empty_is_3() {
        // 1 visual row + 2 border rows.
        let bar = InputBar::new();
        assert_eq!(bar.required_height(80), 3);
    }

    #[test]
    fn required_height_wraps_long_ascii_line() {
        let mut bar = InputBar::new();
        bar.set_text("a".repeat(200), 200);
        // 200 / 80 = 3 visual rows (200 = 2*80 + 40) = ceil → 3.
        // Plus 2 border rows = 5. Clamp max is 5.
        assert_eq!(bar.required_height(82), 5); // inner width = 80
    }

    #[test]
    fn required_height_clamps_at_max_5_plus_borders() {
        let mut bar = InputBar::new();
        bar.set_text("a".repeat(10_000), 0);
        assert_eq!(bar.required_height(82), 7); // clamp(inner, 1, 5) + 2
    }

    #[test]
    fn required_height_cjk_counts_cells() {
        let mut bar = InputBar::new();
        // 10 CJK chars = 20 cells → fits in inner width 20 on one row.
        bar.set_text("你好世界你好世界你好".to_string(), 0);
        assert_eq!(bar.required_height(22), 3); // inner width = 20 → 1 row
    }
}
```

- [ ] **Step 2: Run the tests, confirm they fail.**

Run: `cargo test -p spur-tui --lib input_bar::required_height_tests`
Expected: FAIL — current `required_height` returns wrong numbers for CJK and clamp.

- [ ] **Step 3: Rewrite `required_height`.**

In `input_bar.rs`, replace the body of `required_height` (lines 978–999):

```rust
    /// Required render height given the available `width`.
    ///
    /// Includes 2 rows for top+bottom borders. The inner rows are the
    /// visual-row count produced by the soft-wrap layer, clamped to
    /// `[1, 5]` so the input bar never dominates the view.
    pub fn required_height(&self, width: u16) -> u16 {
        let inner_w = width.saturating_sub(2);
        if inner_w == 0 {
            return 3;
        }

        // Include the status prefix + ">" marker in the first logical line's
        // budget by reducing the effective width for the first visual row.
        // For simplicity and correctness, we account for the prefix by
        // feeding it as a prepended segment: we add a sentinel space-padded
        // string in front of line 0 purely for the height calculation.
        let prefix_len = self
            .status
            .as_ref()
            .map(|s| s.len() + 1)
            .unwrap_or(0)
            + 2;

        let mut lines: Vec<String> = self.textarea.lines().to_vec();
        if lines.is_empty() {
            lines.push(String::new());
        }
        // Pad the first line with `prefix_len` ASCII spaces so wrap accounts
        // for the on-screen prefix. (Spaces are 1 cell each, matching how
        // the prefix is drawn.)
        lines[0] = format!("{}{}", " ".repeat(prefix_len), lines[0]);

        let layout = crate::components::input_bar_wrap::wrap(&lines, inner_w);
        let inner = layout.visual_height().clamp(1, 5);
        inner + 2
    }
```

- [ ] **Step 4: Run the tests, confirm they pass.**

Run: `cargo test -p spur-tui --lib input_bar::required_height_tests`
Expected: 4 tests pass.

- [ ] **Step 5: Run the whole `spur-tui` test suite to catch regressions.**

Run: `cargo test -p spur-tui`
Expected: all existing tests still pass.

- [ ] **Step 6: Commit.**

```bash
git add crates/spur-tui/src/components/input_bar.rs
git commit -m "feat(input_bar): required_height uses WrapLayout::visual_height"
```

---

## Task 7: Rewrite `render()` to paint through the wrap layout

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`

- [ ] **Step 1: Add a helper to convert char-column to byte offset.**

Inside `impl InputBar` in `input_bar.rs`, add this private helper near `cursor_to_byte` (around line 611):

```rust
    /// Logical (row, char_col) → byte offset within `lines()[row]`.
    fn char_col_to_byte(&self, row: usize, char_col: usize) -> usize {
        let lines = self.textarea.lines();
        if row >= lines.len() {
            return 0;
        }
        let line = &lines[row];
        line.char_indices()
            .nth(char_col)
            .map(|(i, _)| i)
            .unwrap_or(line.len())
    }
```

- [ ] **Step 2: Rewrite `render()`.**

Replace the body of `render()` (around line 1002) with:

```rust
    /// Render the input bar.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let mode_str = match self.mode {
            EditMode::Emacs => " INSERT ",
            EditMode::Vim(VimMode::Normal) => " VIM·NORMAL ",
            EditMode::Vim(VimMode::Insert) => " VIM·INSERT ",
            EditMode::Vim(VimMode::Visual) => " VIM·VISUAL ",
            EditMode::Vim(VimMode::Operator(_)) => " VIM·OP ",
        };

        let title = if let Some(ref status) = self.status {
            format!("{} {}", status, mode_str)
        } else {
            mode_str.to_string()
        };

        let border_color = match self.mode {
            EditMode::Vim(VimMode::Normal) => Color::Yellow,
            EditMode::Vim(VimMode::Visual) => Color::LightYellow,
            _ => Color::Green,
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(title, Style::default().fg(border_color)));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        // Compute wrap layout for the buffer against the inner width.
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let layout =
            crate::components::input_bar_wrap::wrap(&lines, inner.width);

        // Cursor in visual coordinates.
        let (cursor_row, cursor_ccol) = self.textarea.cursor();
        let cursor_byte = self.char_col_to_byte(cursor_row, cursor_ccol);
        let (cursor_vr, cursor_vc) =
            layout.logical_to_visual(cursor_row, cursor_byte);

        // Vertical scroll: keep the cursor within the visible window.
        let visible = inner.height as usize;
        let total = layout.visual_height() as usize;
        let view_top = if total <= visible {
            0
        } else if cursor_vr >= visible {
            cursor_vr + 1 - visible
        } else {
            0
        };

        // Build visible lines.
        let last_vr = (view_top + visible).min(total);
        let mut out_lines: Vec<ratatui::text::Line<'static>> =
            Vec::with_capacity(last_vr - view_top);

        // Selection range in bytes for quick intersection check.
        let selection =
            self.textarea.selection_range().map(|((sr, sc), (er, ec))| {
                let sb = self.char_col_to_byte(sr, sc);
                let eb = self.char_col_to_byte(er, ec);
                (sr, sb, er, eb)
            });

        for vi in view_top..last_vr {
            let vr = &layout.rows[vi];
            let logical = &lines[vr.logical_row];
            let row_text = &logical[vr.byte_start..vr.byte_end];

            // One span per grapheme with its style — simple, correct,
            // matches the widths used for cursor placement.
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(vr.graphemes.len());
            for g in &vr.graphemes {
                // Slice grapheme out of the logical line (not row_text) using
                // absolute byte offsets on the line.
                let piece_slice = &logical[g.byte_start..g.byte_end];
                // Substitute visual expansions for special graphemes.
                let piece: String = if piece_slice == "\t" {
                    " ".repeat(crate::components::input_bar_wrap::TAB_WIDTH)
                } else {
                    piece_slice.to_string()
                };

                let mut style = Style::default();

                // Atom styling.
                for atom in &self.protected_ranges {
                    // Atom's absolute byte range is within its logical line
                    // (we only track single-line atoms for now — a multi-line
                    // atom's render is the concatenation of the per-line
                    // slices handled implicitly by the logical-row filter).
                    if vr.logical_row == 0
                        && g.byte_start >= atom.start
                        && g.byte_end <= atom.end
                    {
                        style = style
                            .fg(Color::LightBlue)
                            .add_modifier(Modifier::UNDERLINED);
                    }
                }

                // Selection styling.
                if let Some((sr, sb, er, eb)) = selection {
                    let in_sel = if sr == er && vr.logical_row == sr {
                        g.byte_start >= sb && g.byte_end <= eb
                    } else if vr.logical_row == sr {
                        g.byte_start >= sb
                    } else if vr.logical_row == er {
                        g.byte_end <= eb
                    } else {
                        vr.logical_row > sr && vr.logical_row < er
                    };
                    if in_sel {
                        style = style.add_modifier(Modifier::REVERSED);
                    }
                }

                spans.push(Span::styled(piece, style));
            }

            // Trailing blank span to clear any residue right of content.
            let _ = row_text; // silence unused warning if kept for debugging.
            out_lines.push(ratatui::text::Line::from(spans));
        }

        let paragraph = ratatui::widgets::Paragraph::new(out_lines);
        frame.render_widget(paragraph, inner);

        // Place the cursor cell if it is within the visible window.
        if cursor_vr >= view_top && cursor_vr < last_vr {
            let cx = inner.x + cursor_vc as u16;
            let cy = inner.y + (cursor_vr - view_top) as u16;
            frame.set_cursor_position((cx, cy));
        }
    }
```

- [ ] **Step 3: Build to catch type errors.**

Run: `cargo build -p spur-tui`
Expected: SUCCESS. (`Span::styled` takes `Into<Cow<'static, str>>`; passing `String` is fine.)

- [ ] **Step 4: Run the full test suite.**

Run: `cargo test -p spur-tui`
Expected: all tests pass, including existing `input_bar` and `required_height_tests`.

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-tui/src/components/input_bar.rs
git commit -m "feat(input_bar): render via WrapLayout with visual cursor placement"
```

---

## Task 8: Add visual-line `Up`/`Down` and wire them in Emacs + Vim-Insert

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`

- [ ] **Step 1: Add failing tests.**

Append to `required_height_tests` mod:

```rust
    #[test]
    fn visual_down_crosses_wrap_boundary() {
        let mut bar = InputBar::new();
        // 16 ASCII chars, wrapped at width 5 (inner) → 4 vrows of 5,5,5,1.
        bar.set_text("abcdefghijklmnop".to_string(), 3);
        // width arg to visual nav reflects the inner width of the render area.
        bar.visual_line_down(5);
        // Cursor moves from byte 3 (vrow 0, vcol 3) to vrow 1, vcol 3 → byte 8.
        assert_eq!(bar.cursor(), 8);
    }

    #[test]
    fn visual_up_inverse_of_down() {
        let mut bar = InputBar::new();
        bar.set_text("abcdefghijklmnop".to_string(), 3);
        bar.visual_line_down(5);
        bar.visual_line_up(5);
        assert_eq!(bar.cursor(), 3);
    }

    #[test]
    fn visual_down_at_last_vrow_is_noop() {
        let mut bar = InputBar::new();
        bar.set_text("abc".to_string(), 3);
        bar.visual_line_down(80);
        assert_eq!(bar.cursor(), 3);
    }
```

- [ ] **Step 2: Run, confirm they fail.**

Run: `cargo test -p spur-tui --lib input_bar::required_height_tests`
Expected: 3 new tests FAIL (methods missing).

- [ ] **Step 3: Implement the helpers.**

Inside `impl InputBar`, add near the other cursor helpers:

```rust
    /// Visual-line Down: move cursor one visual row down, preserving vcol.
    /// `inner_width` is the inner drawing width (frame.inner.width). Public so
    /// key handlers can reach it; callers in arrow-key paths pass the last
    /// rendered width via state.
    pub fn visual_line_down(&mut self, inner_width: u16) {
        self.visual_line_move(inner_width, 1);
    }

    /// Visual-line Up.
    pub fn visual_line_up(&mut self, inner_width: u16) {
        self.visual_line_move(inner_width, -1);
    }

    fn visual_line_move(&mut self, inner_width: u16, delta: i32) {
        if inner_width == 0 {
            return;
        }
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let layout = crate::components::input_bar_wrap::wrap(&lines, inner_width);

        let (row, ccol) = self.textarea.cursor();
        let byte = self.char_col_to_byte(row, ccol);
        let (vr, vc) = layout.logical_to_visual(row, byte);

        let target_vr = (vr as i32 + delta).clamp(0, layout.rows.len() as i32 - 1) as usize;
        if target_vr == vr {
            return;
        }
        let max_vc = layout.rows[target_vr].used_cells as usize;
        let target_vc = vc.min(max_vc);
        let (target_row, target_byte) = layout.visual_to_logical(target_vr, target_vc);
        self.move_cursor_to_byte(target_byte_abs(&lines, target_row, target_byte));
    }
```

And add the free helper near the bottom of the file (just above `impl Default for InputBar`):

```rust
/// Convert per-line byte offset to absolute byte offset across all lines.
fn target_byte_abs(lines: &[String], row: usize, byte_col: usize) -> usize {
    let mut acc = 0usize;
    for (i, l) in lines.iter().enumerate() {
        if i == row {
            return acc + byte_col;
        }
        acc += l.len() + 1; // +1 for '\n'
    }
    acc
}
```

- [ ] **Step 4: Run the tests, confirm they pass.**

Run: `cargo test -p spur-tui --lib input_bar::required_height_tests`
Expected: all 7 tests in that module pass.

- [ ] **Step 5: Wire the helpers into key handlers.**

In `input_bar.rs`, `handle_emacs_input` (around line 176), replace the current handling for `KeyCode::Up` and `KeyCode::Down`. There is no existing explicit handler — arrow Up/Down currently falls through to `self.textarea.input(input)` at the bottom (line 281). Add explicit cases BEFORE the catch-all, right next to the existing `KeyCode::Left`/`Right` handlers:

```rust
            KeyCode::Up => {
                self.visual_line_up(self.last_inner_width());
                return None;
            }
            KeyCode::Down => {
                self.visual_line_down(self.last_inner_width());
                return None;
            }
```

Do the same in `handle_vim_insert_input` (around line 577), adjacent to the Left/Right handlers:

```rust
            KeyCode::Up => {
                self.visual_line_up(self.last_inner_width());
                return None;
            }
            KeyCode::Down => {
                self.visual_line_down(self.last_inner_width());
                return None;
            }
```

- [ ] **Step 6: Add the `last_inner_width` state field.**

Arrow keys arrive before any render, so `InputBar` needs to remember the last inner width that `render()` computed against. Add a field and plumb it through.

In the `InputBar` struct (around line 44), add:

```rust
    /// Last inner width observed in `render()`; used by visual-line nav when
    /// arrow keys fire before a fresh render.
    last_inner_width: u16,
```

In `InputBar::new()` (around line 68), initialize it:

```rust
            last_inner_width: 80,
```

In `InputBar::clear()` (around line 874), preserve it across clears:

```rust
    pub fn clear(&mut self) {
        let mode = self.mode;
        let last_w = self.last_inner_width;
        self.textarea = TextArea::default();
        self.textarea.set_cursor_line_style(Style::default());
        self.line_cache = vec![0];
        self.protected_ranges.clear();
        self.last_inner_width = last_w;
        self.set_mode(mode);
    }
```

And in `set_text` (around line 894) do the same preservation.

At the top of `render()` (just after computing `let inner = block.inner(area);`), stash it. Since `render` takes `&self`, change it by using interior mutation — actually, `render(&self)` can't mutate. Instead, expose a setter and have callers call it in their layout code.

Add a public setter:

```rust
    /// Record the last rendered inner width so arrow-key nav can compute
    /// visual rows before the next render.
    pub fn set_last_inner_width(&mut self, width: u16) {
        self.last_inner_width = width;
    }

    fn last_inner_width(&self) -> u16 {
        self.last_inner_width
    }
```

In `dashboard.rs` just before each `self.input_bar.render(frame, input_bar_area);` call (lines 394, 481), add:

```rust
self.input_bar.set_last_inner_width(input_bar_area.width.saturating_sub(2));
```

In `session_detail.rs` just before `self.input_bar.render(frame, chunks[3]);` (line 1675), add:

```rust
self.input_bar.set_last_inner_width(chunks[3].width.saturating_sub(2));
```

- [ ] **Step 7: Build and run all tests.**

Run: `cargo test -p spur-tui`
Expected: all pass.

- [ ] **Step 8: Commit.**

```bash
git add crates/spur-tui/src/components/input_bar.rs \
        crates/spur-tui/src/views/dashboard.rs \
        crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(input_bar): visual-line Up/Down for Emacs and Vim Insert"
```

---

## Task 9: Manual smoke test

**Files:**
- None modified; interactive verification.

- [ ] **Step 1: Run the TUI binary.**

Run: `cargo run -p spur-tui` (or whatever the project's TUI entry is — check with `cargo run -p spur-tui --bin --list`).

- [ ] **Step 2: Exercise soft-wrap.**

- Type a single long line (>80 chars of ASCII) — confirm it wraps visually rather than scrolling sideways, and the cursor stays on the glyph.
- Press `Down` / `Up` arrow keys — confirm the cursor moves between visual rows of the same logical line.
- Paste a CJK string (`你好世界你好世界你好` × 3) — confirm wrap respects cell width (not byte length).
- Paste a ZWJ family emoji `👨‍👩‍👧` — confirm single-cursor-step crosses the whole cluster.
- Type `@` to invoke a completion (if wired) and insert an atom; confirm the atom stays visually contiguous even when it wraps.
- Enter Vim Normal mode (`Esc`) — confirm `j`/`k` still do logical-line navigation.

- [ ] **Step 3: Resize the terminal mid-input** — confirm re-wrap is automatic (no stale layout).

- [ ] **Step 4: If anything looks wrong, open an issue and revert. Otherwise, commit a note.**

```bash
git commit --allow-empty -m "chore(input_bar): smoke test passed for soft-wrap"
```

---

## Task 10: Clean up dead code & self-review

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`

- [ ] **Step 1: Run clippy.**

Run: `cargo clippy -p spur-tui -- -D warnings`
Expected: no warnings. Fix any that appear (typically unused imports now that `tui_textarea::Input` is still used for editing but maybe one import can be trimmed).

- [ ] **Step 2: Run the full workspace tests.**

Run: `cargo test --workspace`
Expected: all pass. Fix any downstream impact.

- [ ] **Step 3: Verify `frame.render_widget(&self.textarea, ...)` is no longer called.**

Run: `cargo clippy -p spur-tui` and also grep:

Use Grep for `render_widget\(.*textarea` in `crates/spur-tui/src` — expected: no matches.

- [ ] **Step 4: Commit any clippy fixes.**

```bash
git add -u
git commit -m "chore(input_bar): clippy cleanup after soft-wrap"
```

---

## Verification checklist

- [ ] All 27 unit tests in `input_bar_wrap` pass.
- [ ] All 7 tests in `input_bar::required_height_tests` pass.
- [ ] `cargo test --workspace` green.
- [ ] `cargo clippy -p spur-tui -- -D warnings` green.
- [ ] Manual smoke test: long ASCII, CJK, ZWJ emoji, atom-over-wrap, Vim Normal `j`/`k`, terminal resize all correct.
- [ ] No call site other than `InputBar::render` calls `frame.render_widget` on a `TextArea`.

---

## Self-review notes

- **Spec coverage:** Every section of the spec maps to a task. Coordinate systems → Tasks 1, 4. Wrap algorithm → Tasks 2, 3. WrapLayout shape → Task 1. Rendering → Task 7. `required_height` → Task 6. Visual-line nav → Task 8. Protected ranges preserved → no code change, verified by Task 7 selection/atom styling + Task 5 conservation test. Module layout → Task 1. Testing → Tasks 2–5, 8. Out-of-scope items (bidi, UAX#14, Vim-Normal visual `j`/`k`) are intentionally not planned.
- **Placeholders:** none — every step has exact code or commands.
- **Type consistency:** `WrapLayout` methods (`visual_height`, `logical_to_visual`, `visual_to_logical`) are named identically everywhere they're called. `TAB_WIDTH` (not `TAB_SIZE`) used consistently. `inner_width` parameter name consistent between the two nav helpers. `set_last_inner_width` / `last_inner_width` paired getter/setter.
- **One likely snag:** in Task 7, existing styling for the cursor glyph by `tui-textarea` is gone (we set the cursor via `frame.set_cursor_position`, which uses the terminal's default cursor). If the project requires the block-style cursor in Vim Normal, Task 7 needs an extra reverse-style paint on the cursor cell — see risk register in the spec. Recommend verifying during Task 9 smoke test; if needed, add a Task 11 to paint `out_lines[cursor_vr - view_top].spans[cursor_vc_in_graphemes]` with `Modifier::REVERSED` under Vim Normal.
