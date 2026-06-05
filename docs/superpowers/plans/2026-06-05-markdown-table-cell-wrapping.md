# Markdown Table Cell Soft-Wrapping Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** _(none — direct from grounded investigation; see RCA below)_
**Design epic:** _(none — small contained fix, no separate design epic)_

**Goal:** Wrap long markdown-table cell content across multiple physical grid lines so wide tables stay aligned instead of overflowing and being hard-wrapped by ratatui (which slices box-drawing borders).

**Architecture:** All work is contained to `crates/spur-tui/src/components/markdown_stream.rs` (a self-contained leaf cluster — verified zero cross-crate blast radius). Today the grid renderer pads each cell to its unbounded max-content width and never wraps; the only width mitigation is an all-or-nothing `render_table_records` fallback that destroys 2D structure. We add (1) a per-cell unicode-aware word-wrap helper, (2) a column-width budgeting helper that shrinks the widest columns to fit `render_width`, and (3) wire both into `render_table_grid` so a logical row emits N physical lines with borders redrawn on each. The under-budget path is byte-identical to today (keeps all existing golden tests green); only the over-budget path changes from overflow → wrap.

**Tech Stack:** Rust 2021, `unicode-width` (`UnicodeWidthStr` / `UnicodeWidthChar`, already imported in this file), `ratatui` `Line`/`Span`.

---

## RCA / Why this change

The TUI renders GFM tables as box-drawing grids via `render_table_grid` → `format_table_row` → `pad_cell`. `pad_cell` pads a cell to its column's max-content display width with **no upper bound and no intra-cell wrapping**. When a cell is long, the emitted grid line exceeds the viewport and ratatui hard-wraps it at the grapheme boundary, cutting straight through `│ ┌ ├` borders — the observed mangled alignment.

The live render paths *do* plumb a width (`builder.rs:813`, `render.rs:839`/`864`, `mod.rs:947` all pass `Some(effective_width.saturating_sub(3))`), reaching `render_markdown_table(table, render_width)`. Today that width only drives the binary grid-vs-records decision at `markdown_stream.rs:486-490`. We replace that binary with budgeted-grid-with-wrapping, demoting `render_table_records` to the degenerate "even the minimum grid can't fit" case.

**Invariant preserved:** `replace_markdown_tables_in_lines` (`markdown_stream.rs:340-354`) advances `line_idx` by **raw source-row count** and substitutes `table.rendered_lines` wholesale, so emitting *more* physical lines inside a rendered table does NOT break the `line_sequence_matches_table` / `pipe_count` anchor. Wrapping is safe against the streaming matcher.

---

## File Structure

- Modify: `crates/spur-tui/src/components/markdown_stream.rs`
  - Add `wrap_cell_to_width` (new free fn)
  - Add `budget_column_widths` + `MIN_COL_WIDTH` const (new free fn)
  - Add `format_table_row_wrapped` (new free fn)
  - Rewrite `render_table_grid` body (lines 514-529) to emit wrapped rows
  - Rewrite `render_markdown_table` width branch (lines 486-492)
  - Add unit + golden tests in the existing `stream_item_tests` module

No other files change.

---

### Task 1: Pure wrap + budget helpers

**Task ID:** `task-1` (used in `depends_on` references)

**Files:**
- Modify: `crates/spur-tui/src/components/markdown_stream.rs` (add two free functions + one const near the existing table helpers, ~line 686 next to `truncate_to_width`; add unit tests in `stream_item_tests`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `wrap_cell_to_width` and `budget_column_widths` compile as private free functions.
- [ ] New unit tests pass: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-tui wrap_cell_to_width budget_column_widths`
- [ ] No existing test regresses (helpers are not yet wired into rendering).
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-tui -- -D warnings` clean.

**Suggested Worker:** codex (single file, mechanical pure functions)

**Scope Boundary:**
- IN scope: add the two helper functions, the `MIN_COL_WIDTH` const, and their unit tests in `markdown_stream.rs`.
- OUT of scope: `render_table_grid`, `format_table_row`, `render_markdown_table`, `render_table_records`, any other file. Do NOT wire the helpers into the renderer yet (that is task-2).
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Write the failing tests** (add inside `mod stream_item_tests`, alongside the existing table tests near line 1654):

```rust
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
    assert!(lines.iter().all(|l| display_width(l) <= 8), "got: {lines:?}");
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
    assert!(out[0] >= MIN_COL_WIDTH && out[1] >= MIN_COL_WIDTH, "got: {out:?}");
    assert!(out[1] == 5, "narrow column should not shrink below its content: {out:?}");
}

#[test]
fn budget_column_widths_returns_none_when_floor_cannot_fit() {
    // 3 columns need 3*MIN_COL_WIDTH + chrome(10) at minimum; width 12 is too small.
    let widths = vec![10usize, 10, 10];
    assert_eq!(budget_column_widths(&widths, 12), None);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-tui wrap_cell_to_width budget_column_widths -- --nocapture`
Expected: FAIL — `wrap_cell_to_width` / `budget_column_widths` not defined.

- [ ] **Step 3: Write the implementation** (add near `truncate_to_width`, after line ~701):

```rust
/// Minimum content width a column may be shrunk to when budgeting a grid to a
/// terminal width. Below this, the records layout is used instead.
const MIN_COL_WIDTH: usize = 4;

/// Wrap `cell` into physical lines each no wider than `width` display columns.
/// Breaks on ASCII spaces first; a single token wider than `width` is hard-split
/// on char boundaries by display width (never mid-emoji, never mid-wide-char).
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-tui wrap_cell_to_width budget_column_widths -- --nocapture`
Expected: PASS (8 new tests).

- [ ] **Step 5: Clippy + commit**

```bash
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-tui -- -D warnings
git add crates/spur-tui/src/components/markdown_stream.rs
git commit -m "feat(spur-tui): task-1 add unicode cell-wrap + column-budget helpers"
```

**Scope Drift Checkpoint:**
- If you need to modify any rendering function to make the helpers compile → emit `scope_drift` (they are standalone and should not require it).
- If `display_width` / `UnicodeWidthChar` are not already in scope in this file → they are (used by `truncate_to_width`); if not, emit `risk`.

---

### Task 2: Wire wrapping into the grid renderer

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-tui/src/components/markdown_stream.rs`
  - Add `format_table_row_wrapped`
  - Rewrite `render_table_grid` (lines 514-529)
  - Rewrite the width branch of `render_markdown_table` (lines 486-492)
  - Add a golden wrapping test in `stream_item_tests`

**Depends on:** task-1

**Acceptance Criteria:**
- [ ] Over-budget tables render as a multi-line aligned grid; no emitted grid line exceeds the budget width.
- [ ] All existing table golden tests still pass byte-for-byte: `basic_gfm_table_renders_as_aligned_grid`, `gfm_table_pads_uneven_column_widths`, `gfm_table_is_wrapped_and_preserves_line_boundaries`, `streamed_wide_header_only_table_never_renders_raw_at_narrow_width`.
- [ ] New golden test `over_budget_table_wraps_cells_within_grid` passes.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-tui` green (lib + integration).
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-tui -- -D warnings` clean.

**Suggested Worker:** codex (single file; mechanical wiring against interfaces task-1 defined)

**Scope Boundary:**
- IN scope: `format_table_row_wrapped`, `render_table_grid`, the `render_markdown_table` width branch, and the new golden test in `markdown_stream.rs`.
- OUT of scope: `wrap_cell_to_width` / `budget_column_widths` bodies (use as-is from task-1), `render_table_records` body, `builder.rs` / `render.rs` / `mod.rs` width plumbing (already passes `Some(width)`), any other file.
- If you discover the width is NOT reaching `render_markdown_table` at runtime → emit `scope_drift` (do not change the view layer in this task).

**Implementation:**

- [ ] **Step 1: Write the failing golden test** (add in `mod stream_item_tests`):

```rust
#[test]
fn over_budget_table_wraps_cells_within_grid() {
    // A two-column table whose grid is far wider than the render width must
    // wrap the long cell across multiple physical lines WITHOUT any grid line
    // exceeding the budget, and without falling back to the "Header: value"
    // records layout.
    let table = MarkdownTable {
        alignments: vec![Alignment::None, Alignment::None],
        header: vec!["Criterion".to_string(), "Verdict".to_string()],
        rows: vec![vec![
            "Resolver References arm gates on function_singleton_safe".to_string(),
            "exact match".to_string(),
        ]],
    };
    let width: u16 = 32;
    let lines = render_markdown_table(&table, Some(width));
    let rendered: Vec<String> = lines.iter().map(line_plain_text).collect();

    // Still a grid (top border present), not records ("Criterion: ..." prose).
    assert!(rendered[0].starts_with('┌'), "want grid top border:\n{rendered:#?}");
    assert!(
        !rendered.iter().any(|l| l.starts_with("Criterion:")),
        "must not use records fallback:\n{rendered:#?}"
    );
    // No physical line exceeds the budget.
    for l in &rendered {
        assert!(display_width(l) <= width as usize, "line over budget: {l:?}");
    }
    // The long criterion text wrapped onto more than one body line.
    let body_lines = rendered.iter().filter(|l| l.starts_with('│')).count();
    assert!(body_lines >= 3, "expected wrapped multi-line row:\n{rendered:#?}");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-tui over_budget_table_wraps_cells_within_grid -- --nocapture`
Expected: FAIL — current code either overflows (line > 32) or takes the records branch.

- [ ] **Step 3: Add `format_table_row_wrapped`** (next to `format_table_row`, ~line 593):

```rust
/// Render one logical row as one-or-more physical grid lines, wrapping each
/// cell to its column width. Every physical line carries full borders; cells
/// with fewer wrapped lines are blank-padded. When every cell fits on one line
/// this is byte-identical to `format_table_row`.
fn format_table_row_wrapped(
    cells: &[String],
    widths: &[usize],
    alignments: &[Alignment],
) -> Vec<String> {
    let wrapped: Vec<Vec<String>> = (0..widths.len())
        .map(|idx| {
            let cell = cells.get(idx).map(String::as_str).unwrap_or("");
            wrap_cell_to_width(cell, widths[idx])
        })
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let mut out = Vec::with_capacity(height);
    for line_idx in 0..height {
        let mut s = String::new();
        s.push('│');
        for (idx, cell_lines) in wrapped.iter().enumerate() {
            let piece = cell_lines.get(line_idx).map(String::as_str).unwrap_or("");
            s.push(' ');
            s.push_str(&pad_cell(
                piece,
                widths[idx],
                alignments.get(idx).copied().unwrap_or(Alignment::None),
            ));
            s.push(' ');
            s.push('│');
        }
        out.push(s);
    }
    out
}
```

- [ ] **Step 4: Rewrite `render_table_grid`** (replace lines 514-529 body) to emit wrapped rows:

```rust
fn render_table_grid(
    header: &[String],
    rows: &[Vec<String>],
    widths: &[usize],
    alignments: &[Alignment],
) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(rows.len() + 4);
    out.push(table_line(top_border(widths)));
    for line in format_table_row_wrapped(header, widths, alignments) {
        out.push(table_line(line));
    }
    out.push(table_line(header_border(widths)));
    for row in rows {
        for line in format_table_row_wrapped(row, widths, alignments) {
            out.push(table_line(line));
        }
    }
    out.push(table_line(bottom_border(widths)));
    out
}
```

> Note: `format_table_row` may now be unused. If clippy flags it dead, delete it
> (its single caller was `render_table_grid`). Confirm with
> `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-tui -- -D warnings`.

- [ ] **Step 5: Rewrite the width branch of `render_markdown_table`** (replace lines 486-490):

```rust
    if let Some(max_width) = render_width.map(usize::from).filter(|width| *width > 0) {
        if table_grid_width(&widths) > max_width {
            if let Some(budgeted) = budget_column_widths(&widths, max_width) {
                return render_table_grid(&header, &rows, &budgeted, &table.alignments);
            }
            // Even a floor-width grid cannot fit → records layout (data rows only).
            if !rows.is_empty() {
                return render_table_records(&header, &rows, max_width);
            }
        }
    }
```

(Leave the trailing `render_table_grid(&header, &rows, &widths, &table.alignments)` at line 492 as the under-budget / no-width default.)

- [ ] **Step 6: Run the full crate suite**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-tui`
Expected: PASS — new wrap test green; the four existing golden tests unchanged (under-budget output is byte-identical because `height == 1`).

- [ ] **Step 7: Clippy + commit**

```bash
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-tui -- -D warnings
git add crates/spur-tui/src/components/markdown_stream.rs
git commit -m "fix(spur-tui): task-2 soft-wrap long table cells within the grid"
```

**Scope Drift Checkpoint:**
- If existing golden tests change output → STOP, the under-budget path must stay byte-identical; investigate before editing the goldens, and emit `risk` if the goldens genuinely need updating.
- If wiring requires touching the view layer (`builder.rs`/`render.rs`/`mod.rs`) → emit `scope_drift`.

---

## Self-Review

**Spec coverage:** The grounded RCA requires (a) unicode-aware per-cell wrapping, (b) budgeted column widths, (c) grid emission of multi-line rows, (d) records demoted to degenerate fallback. task-1 covers (a)+(b); task-2 covers (c)+(d). Covered.

**Placeholder scan:** No TBD/TODO; every code step is concrete.

**Type consistency:** `wrap_cell_to_width(&str, usize) -> Vec<String>` and `budget_column_widths(&[usize], usize) -> Option<Vec<usize>>` defined in task-1 are consumed verbatim in task-2 (`format_table_row_wrapped`, `render_markdown_table`). `MIN_COL_WIDTH` defined once in task-1, referenced in task-1 tests only. `MarkdownTable` / `Alignment` / `line_plain_text` / `display_width` already exist in-file and are used by the task-2 test.

**DAG validation:** `task-1` (root) → `task-2`. Linear, acyclic. Serialized deliberately: both tasks edit the same file, so a chain avoids merge collisions (justified deviation from max-parallelism).

**beads compatibility:** Each task has a unique ID, explicit `depends_on`, verifiable acceptance criteria (named test runs + clippy), and a scope boundary naming exact in/out functions.
