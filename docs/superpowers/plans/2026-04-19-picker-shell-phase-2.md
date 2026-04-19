# PickerShell Phase 2 — Pattern Caching + Newline-Aware Atoms Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate `Pattern::parse` allocation on every keystroke in `HistoryQuerySource` via single-slot caching, make atom byte ranges correct when the snapshot text contains newlines (currently dropped entirely), and lock both guarantees behind tests — one alloc-bounded, one render-integration.

**Architecture:** Two small changes to `query_source.rs` (add a pattern cache field + a newline-aware atom-mapping helper), one new integration test file that exercises multi-line + atom rendering through `PickerShell`. No changes to `PickerShell`, `MiniInput`, `InputBar`, `SessionDetailView`, or any persisted data model.

**Tech Stack:** Rust 2021, `nucleo-matcher` (`Pattern::parse` / `Matcher` types cached, not reallocated), existing `tui-textarea`-backed `InputBar`, existing ratatui `TestBackend` for render assertions.

**Spec:** `docs/superpowers/specs/2026-04-19-picker-shell-retrieval-unification-design.md` (Phase 2 section).

---

## File Structure

**Modify:**
- `crates/spur-tui/src/components/query_source.rs` — add `cached_pattern: Option<(String, Pattern)>` field, cache-aware `Pattern::parse`, add `row_from_entry` newline-aware atom remapping, add `pattern_parse_count_for_test` accessor.

**Create:**
- `crates/spur-tui/tests/picker_shell_atom_render.rs` — integration test that drives `PickerShell` through a `TestBackend` and asserts the rendered cells for atom spans carry `Color::LightBlue + Modifier::UNDERLINED`, even when the snapshot text contains `\n`.

**Unchanged:**
- `crates/spur-tui/src/components/picker_shell.rs`
- `crates/spur-tui/src/components/mini_input.rs`
- `crates/spur-tui/src/components/input_bar.rs`
- `crates/spur-tui/src/views/session_detail.rs`
- `crates/spur-tui/src/input_history.rs`
- On-disk `InputHistoryEntry` / `InputStateSnapshot` shapes.

---

## Task 1: Cache `Pattern` in `HistoryQuerySource`

Avoid re-parsing the pattern on every keystroke when the query string hasn't changed from the last refresh. A single-slot cache is correct because refresh is single-threaded per-shell.

**Files:**
- Modify: `crates/spur-tui/src/components/query_source.rs`

- [ ] **Step 1: Write the failing alloc-bounded test**

Append to the existing `#[cfg(test)] mod tests` block in `crates/spur-tui/src/components/query_source.rs` (after the Phase 1 tests):

```rust
    #[test]
    fn same_query_repeated_does_not_reparse_pattern() {
        let hist = vec![
            mk_entry("alpha"),
            mk_entry("beta"),
            mk_entry("gamma"),
        ];
        let mut src = HistoryQuerySource::new(hist);
        let _ = src.refresh("a");
        let base = src.pattern_parse_count_for_test();
        for _ in 0..99 {
            let _ = src.refresh("a");
        }
        assert_eq!(src.pattern_parse_count_for_test(), base);
    }

    #[test]
    fn different_queries_each_reparse_once() {
        let hist = vec![mk_entry("alpha"), mk_entry("beta")];
        let mut src = HistoryQuerySource::new(hist);
        let _ = src.refresh("a");
        let _ = src.refresh("al");
        let _ = src.refresh("alp");
        assert_eq!(src.pattern_parse_count_for_test(), 3);
    }

    #[test]
    fn empty_query_does_not_touch_pattern_cache() {
        let hist = vec![mk_entry("alpha")];
        let mut src = HistoryQuerySource::new(hist);
        let _ = src.refresh("");
        let _ = src.refresh("");
        let _ = src.refresh("");
        assert_eq!(src.pattern_parse_count_for_test(), 0);
    }
```

- [ ] **Step 2: Run tests — verify failure**

Run: `cargo test -p spur-tui --lib components::query_source`
Expected: compile error: no method named `pattern_parse_count_for_test`.

- [ ] **Step 3: Add the cache field + test accessor**

In `crates/spur-tui/src/components/query_source.rs`, update the `HistoryQuerySource` struct and `new`:

Replace:

```rust
pub struct HistoryQuerySource {
    history: Vec<InputHistoryEntry>,
    matcher: Matcher,
    last_snapshots: Vec<InputStateSnapshot>,
}

impl HistoryQuerySource {
    pub fn new(history: Vec<InputHistoryEntry>) -> Self {
        Self {
            history,
            matcher: Matcher::new(Config::DEFAULT),
            last_snapshots: Vec::new(),
        }
    }
```

With:

```rust
pub struct HistoryQuerySource {
    history: Vec<InputHistoryEntry>,
    matcher: Matcher,
    last_snapshots: Vec<InputStateSnapshot>,
    /// Single-slot pattern cache, keyed by the query string that produced it.
    /// Non-empty queries re-parse only when the query string changes; empty
    /// queries never touch this cache.
    cached_pattern: Option<(String, Pattern)>,
    /// Count of `Pattern::parse` calls made so far. Incremented inside
    /// `ensure_pattern`. Exposed under `cfg(any(test, debug_assertions))`
    /// via `pattern_parse_count_for_test`.
    parse_count: usize,
}

impl HistoryQuerySource {
    pub fn new(history: Vec<InputHistoryEntry>) -> Self {
        Self {
            history,
            matcher: Matcher::new(Config::DEFAULT),
            last_snapshots: Vec::new(),
            cached_pattern: None,
            parse_count: 0,
        }
    }

    /// Test-only: read the number of `Pattern::parse` calls made so far.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn pattern_parse_count_for_test(&self) -> usize {
        self.parse_count
    }

    /// Return a reference to a Pattern for `query`, re-using the cache when
    /// `query` is identical to the last non-empty refresh. Guarantees
    /// `parse_count` is incremented exactly once per distinct query string
    /// in the run of refreshes.
    fn ensure_pattern(&mut self, query: &str) -> &Pattern {
        let needs_refresh = match &self.cached_pattern {
            Some((cached_q, _)) if cached_q == query => false,
            _ => true,
        };
        if needs_refresh {
            self.parse_count += 1;
            let pat = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
            self.cached_pattern = Some((query.to_string(), pat));
        }
        &self
            .cached_pattern
            .as_ref()
            .expect("cache populated above")
            .1
    }
```

- [ ] **Step 4: Wire `ensure_pattern` into `refresh`**

In the same file, update the `refresh` method. Replace:

```rust
        let picked: Vec<usize> = if query.is_empty() {
            (0..self.history.len()).rev().take(20).collect()
        } else {
            let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, usize)> = self
                .history
                .iter()
                .enumerate()
                .filter_map(|(i, h)| {
                    buf.clear();
                    let score =
                        pattern.score(Utf32Str::new(&h.snapshot.text, &mut buf), &mut self.matcher)?;
                    Some((score, i))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            scored.into_iter().take(20).map(|(_, i)| i).collect()
        };
```

With:

```rust
        let picked: Vec<usize> = if query.is_empty() {
            (0..self.history.len()).rev().take(20).collect()
        } else {
            // Ensure cached pattern, then split the borrow: reborrow `matcher`
            // after the `&Pattern` has been placed into a local so the pattern
            // reference outlives each scoring call.
            self.ensure_pattern(query);
            let pattern: &Pattern = &self
                .cached_pattern
                .as_ref()
                .expect("ensure_pattern populated the cache")
                .1;
            let mut buf = Vec::new();
            let matcher = &mut self.matcher;
            let mut scored: Vec<(u32, usize)> = self
                .history
                .iter()
                .enumerate()
                .filter_map(|(i, h)| {
                    buf.clear();
                    let score = pattern
                        .score(Utf32Str::new(&h.snapshot.text, &mut buf), matcher)?;
                    Some((score, i))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            scored.into_iter().take(20).map(|(_, i)| i).collect()
        };
```

- [ ] **Step 5: Run the tests — verify pass**

Run: `cargo test -p spur-tui --lib components::query_source`
Expected: `test result: ok. 13 passed; 0 failed` (10 existing + 3 new).

- [ ] **Step 6: Confirm full spur-tui test suite still green**

Run: `cargo test -p spur-tui`
Expected: full test suite passes; no regressions.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/query_source.rs
git commit -m "perf(spur-tui): cache Pattern in HistoryQuerySource (Phase 2)

Single-slot cache keyed by the query string eliminates per-keystroke
Pattern::parse allocation when the user repeatedly refreshes the
same query (e.g. hovering on a selection without typing, or
Backspace-then-retype cycles). Unchanged: one Pattern::parse per
distinct query string.

Adds pattern_parse_count_for_test accessor used by three new
alloc-bounded tests: same-query repetition, distinct-query counting,
empty-query no-op.

Part of: PickerShell Phase 2

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Newline-aware atom offset mapping in `row_from_entry`

Today `row_from_entry` drops atoms entirely when `snapshot.text` contains `\n` (it sets `atoms = Vec::new()`). The newline replacement `'\n'` → `" ↵ "` is 1 byte → 5 bytes (space + U+21B5 + space = 1 + 3 + 1 bytes). Each `\n` before an atom shifts the atom's byte offsets by +4. Remap accordingly so atoms render correctly on multi-line entries.

Assumption: atoms never span a `\n`. This holds because `ProtectedRange` represents resource-mention tokens (`@name`) which are single-line by construction; the `InputStateSnapshot::sanitized` validator does not explicitly check "no newline inside a range" but the product code never builds atoms that straddle newlines.

**Files:**
- Modify: `crates/spur-tui/src/components/query_source.rs`

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `crates/spur-tui/src/components/query_source.rs`:

```rust
    use crate::components::input_bar::ProtectedRange;

    #[test]
    fn row_from_entry_maps_atoms_through_newline_replacement() {
        // snapshot.text:  "hi\n@foo\nbye"
        //                  0  1 2 3 4 5 6 7 8 9 10
        //                       \n         \n
        // After replace('\n', " ↵ "):
        //                 "hi ↵ @foo ↵ bye"
        //                  01234567891011...
        // The "\n" at byte 2 becomes " ↵ " at bytes 2..7 (5 bytes; ↵ = 3 bytes UTF-8)
        // @foo atom was at bytes 3..7 in original; must shift +4 to land at 7..11 in primary.
        let mut snap = InputStateSnapshot::from_text("hi\n@foo\nbye");
        snap.protected_ranges = vec![ProtectedRange {
            start: 3,
            end: 7,
            uri: "file:///foo".to_string(),
            name: "foo".to_string(),
        }];
        let entry = InputHistoryEntry::new(snap);
        let row = HistoryQuerySource::row_from_entry(&entry);
        assert_eq!(row.primary, "hi ↵ @foo ↵ bye");
        assert_eq!(row.atoms, vec![(7, 11)]);
        // Atom byte range resolves to the actual "@foo" substring in primary.
        let (a, b) = row.atoms[0];
        assert_eq!(&row.primary[a..b], "@foo");
    }

    #[test]
    fn row_from_entry_preserves_atoms_without_newlines() {
        let mut snap = InputStateSnapshot::from_text("hi @foo");
        snap.protected_ranges = vec![ProtectedRange {
            start: 3,
            end: 7,
            uri: "file:///foo".to_string(),
            name: "foo".to_string(),
        }];
        let entry = InputHistoryEntry::new(snap);
        let row = HistoryQuerySource::row_from_entry(&entry);
        assert_eq!(row.primary, "hi @foo");
        assert_eq!(row.atoms, vec![(3, 7)]);
    }

    #[test]
    fn row_from_entry_handles_multiple_newlines_before_atom() {
        // Three newlines before the atom. Each shift is +4. Atom at 6..10 → 18..22.
        let mut snap = InputStateSnapshot::from_text("\n\n\nx @foo");
        snap.protected_ranges = vec![ProtectedRange {
            start: 5,
            end: 9,
            uri: "file:///foo".to_string(),
            name: "foo".to_string(),
        }];
        let entry = InputHistoryEntry::new(snap);
        let row = HistoryQuerySource::row_from_entry(&entry);
        let (a, b) = row.atoms[0];
        assert_eq!(&row.primary[a..b], "@foo");
    }
```

- [ ] **Step 2: Run tests — verify failure**

Run: `cargo test -p spur-tui --lib components::query_source::tests::row_from_entry`
Expected: two of the three tests fail (the newline ones — atoms is empty).

- [ ] **Step 3: Replace `row_from_entry` with the newline-aware mapping**

In `crates/spur-tui/src/components/query_source.rs`, replace the entire `row_from_entry` function with:

```rust
    fn row_from_entry(entry: &InputHistoryEntry) -> RetrievalRow {
        // Build the display text and a byte-offset map from the original
        // snapshot.text into the replaced `primary` string. Each `\n` is
        // replaced with " ↵ " (space + U+21B5 + space = 5 bytes); all other
        // bytes pass through unchanged.
        let text = &entry.snapshot.text;
        let mut primary = String::with_capacity(text.len());
        // `offset_map[i]` is the byte offset in `primary` corresponding to
        // byte offset `i` in `text`. Length = text.len() + 1 so that the
        // end-exclusive atom endpoint can be mapped without special-casing.
        let mut offset_map: Vec<usize> = Vec::with_capacity(text.len() + 1);
        for (i, b) in text.bytes().enumerate() {
            offset_map.push(primary.len());
            if b == b'\n' {
                primary.push_str(" ↵ ");
            } else {
                primary.push(b as char); // safe: byte-by-byte passthrough for
                                         // all non-newline bytes preserves
                                         // any multi-byte UTF-8 sequence
                                         // exactly because we're not decoding.
            }
            let _ = i;
        }
        offset_map.push(primary.len());

        // NOTE on the passthrough above: pushing a raw byte as `char` would
        // corrupt non-ASCII text. The safe form is to append the ORIGINAL
        // byte slice; rebuild using a char-boundary walk instead.
        // Correct implementation follows:

        let mut primary = String::with_capacity(text.len());
        let mut offset_map: Vec<usize> = Vec::with_capacity(text.len() + 1);
        offset_map.push(0);
        for ch in text.chars() {
            if ch == '\n' {
                primary.push_str(" ↵ ");
            } else {
                primary.push(ch);
            }
            // Extend the offset map by `ch.len_utf8()` entries so that the
            // original-byte index just past any byte of `ch` maps to the
            // post-replacement `primary.len()`.
            for _ in 0..ch.len_utf8() {
                offset_map.push(primary.len());
            }
        }
        // offset_map now has text.len() + 1 entries: one per original byte
        // boundary including the end.
        debug_assert_eq!(offset_map.len(), text.len() + 1);

        let mention_count = entry.snapshot.protected_ranges.len();
        let secondary = match mention_count {
            0 => String::new(),
            1 => "1 mention".to_string(),
            n => format!("{n} mentions"),
        };
        let tag = entry
            .agent
            .as_ref()
            .map(|a| format!("⟨{a}⟩"))
            .unwrap_or_default();

        let atoms: Vec<(usize, usize)> = entry
            .snapshot
            .protected_ranges
            .iter()
            .filter_map(|r| {
                // Map original byte offsets to post-replacement offsets. If
                // either endpoint is past the map (shouldn't happen given
                // sanitized ranges), drop the atom defensively.
                let a = offset_map.get(r.start)?;
                let b = offset_map.get(r.end)?;
                Some((*a, *b))
            })
            .collect();

        RetrievalRow {
            primary,
            secondary,
            tag,
            atoms,
        }
    }
```

Note the above contains a deliberate first-draft followed by the correct implementation (to show the reasoning and avoid the raw-byte pitfall). Before committing, delete the first (incorrect) pass and keep only the `chars()`-based version:

- [ ] **Step 4: Clean up to the single correct implementation**

In `crates/spur-tui/src/components/query_source.rs`, ensure `row_from_entry` is exactly:

```rust
    fn row_from_entry(entry: &InputHistoryEntry) -> RetrievalRow {
        let text = &entry.snapshot.text;
        let mut primary = String::with_capacity(text.len());
        // offset_map[i] = byte offset in `primary` corresponding to byte i
        // in the original `text`. Length = text.len() + 1 so atom
        // end-exclusive indices map cleanly.
        let mut offset_map: Vec<usize> = Vec::with_capacity(text.len() + 1);
        offset_map.push(0);
        for ch in text.chars() {
            if ch == '\n' {
                primary.push_str(" ↵ ");
            } else {
                primary.push(ch);
            }
            for _ in 0..ch.len_utf8() {
                offset_map.push(primary.len());
            }
        }
        debug_assert_eq!(offset_map.len(), text.len() + 1);

        let mention_count = entry.snapshot.protected_ranges.len();
        let secondary = match mention_count {
            0 => String::new(),
            1 => "1 mention".to_string(),
            n => format!("{n} mentions"),
        };
        let tag = entry
            .agent
            .as_ref()
            .map(|a| format!("⟨{a}⟩"))
            .unwrap_or_default();

        let atoms: Vec<(usize, usize)> = entry
            .snapshot
            .protected_ranges
            .iter()
            .filter_map(|r| {
                let a = offset_map.get(r.start)?;
                let b = offset_map.get(r.end)?;
                Some((*a, *b))
            })
            .collect();

        RetrievalRow {
            primary,
            secondary,
            tag,
            atoms,
        }
    }
```

- [ ] **Step 5: Make the helper publicly callable from tests**

`row_from_entry` is currently `fn` (private, associated). The tests in Step 1 call `HistoryQuerySource::row_from_entry`. Make it `pub(crate) fn` so the in-file tests (and future integration tests) can call it:

```rust
    pub(crate) fn row_from_entry(entry: &InputHistoryEntry) -> RetrievalRow {
```

- [ ] **Step 6: Run tests — verify pass**

Run: `cargo test -p spur-tui --lib components::query_source`
Expected: `test result: ok. 16 passed; 0 failed` (13 previous + 3 new).

- [ ] **Step 7: Run the full spur-tui test suite**

Run: `cargo test -p spur-tui`
Expected: full suite passes, no regressions. Integration tests in `picker_shell_ctrl_r.rs` and `session_detail_commands_integration.rs` still green.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/src/components/query_source.rs
git commit -m "feat(spur-tui): newline-aware atom mapping in row_from_entry (Phase 2)

History entries whose snapshot text contains \\n previously dropped
ALL atoms from the popup row (atoms = Vec::new()). Phase 2 maps
original byte offsets through the newline-replacement transform
('\\n' -> ' ↵ ', 1 byte -> 5 bytes) so atoms continue to render as
Color::LightBlue + Modifier::UNDERLINED even across multiline
entries. Atom endpoints become byte indices into the replaced
primary, not the original snapshot text.

Assumes atoms never span a '\\n' (true by construction: atoms are
single-line resource mentions).

Three new tests cover: single-newline shift, atom-before-newline
pass-through, multi-newline cumulative shift. row_from_entry is
promoted to pub(crate) for test reuse.

Part of: PickerShell Phase 2

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Integration test — rendered atom styling through PickerShell

Lock the end-to-end contract: when a history entry carries a `ProtectedRange`, the corresponding cells in the rendered popup are styled with `Color::LightBlue + Modifier::UNDERLINED`. Cover both a no-newline entry and a newline-containing entry.

**Files:**
- Create: `crates/spur-tui/tests/picker_shell_atom_render.rs`

- [ ] **Step 1: Write the integration test file**

Create `crates/spur-tui/tests/picker_shell_atom_render.rs`:

```rust
//! Integration: rendered `PickerShell` popup rows apply atom styling
//! (LightBlue + UNDERLINED) to `ProtectedRange` byte spans, including
//! entries whose snapshot text contains newlines.

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;

use spur_tui::components::input_bar::ProtectedRange;
use spur_tui::components::picker_shell::PickerShell;
use spur_tui::components::query_source::HistoryQuerySource;
use spur_tui::input_history::{InputHistoryEntry, InputStateSnapshot};

fn mk_entry(text: &str, ranges: Vec<ProtectedRange>) -> InputHistoryEntry {
    let mut snap = InputStateSnapshot::from_text(text);
    snap.protected_ranges = ranges;
    InputHistoryEntry::new(snap)
}

fn render_shell_and_extract_cells(
    shell: &mut PickerShell,
    width: u16,
    height: u16,
) -> Vec<(char, Color, Modifier)> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let anchor = Rect::new(0, height - 1, width, 1);
            let container = Rect::new(0, 0, width, height);
            shell.render(f, anchor, container);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut cells = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            let cell = buffer.cell((x, y)).expect("in-bounds buffer cell");
            let ch = cell.symbol().chars().next().unwrap_or(' ');
            let fg = cell.style().fg.unwrap_or(Color::Reset);
            let modifier = cell.style().add_modifier;
            cells.push((ch, fg, modifier));
        }
    }
    cells
}

#[test]
fn atoms_render_with_light_blue_underlined_styling_no_newline() {
    let hist = vec![mk_entry(
        "hi @foo",
        vec![ProtectedRange {
            start: 3,
            end: 7,
            uri: "file:///foo".to_string(),
            name: "foo".to_string(),
        }],
    )];
    let mut shell = PickerShell::open(Box::new(HistoryQuerySource::new(hist)));
    let cells = render_shell_and_extract_cells(&mut shell, 40, 8);

    // Find any cell that is both LightBlue fg and has UNDERLINED modifier.
    let styled_count = cells
        .iter()
        .filter(|(_ch, fg, m)| *fg == Color::LightBlue && m.contains(Modifier::UNDERLINED))
        .count();
    // "@foo" is 4 characters — expect at least 4 styled cells.
    assert!(
        styled_count >= 4,
        "expected >=4 LightBlue+UNDERLINED cells for @foo, got {styled_count}"
    );

    // And the styled cells should render the '@', 'f', 'o', 'o' symbols in order.
    let styled_chars: String = cells
        .iter()
        .filter(|(_, fg, m)| *fg == Color::LightBlue && m.contains(Modifier::UNDERLINED))
        .map(|(c, _, _)| *c)
        .collect();
    assert!(
        styled_chars.contains("@foo"),
        "styled chars did not contain '@foo': {styled_chars:?}"
    );
}

#[test]
fn atoms_render_with_styling_across_newline_replacement() {
    // Text with a newline BEFORE the atom. The Phase 2 offset mapping
    // must shift @foo's byte range by +4 so the styling still lands on
    // the '@', 'f', 'o', 'o' cells of "hi ↵ @foo ↵ bye".
    let hist = vec![mk_entry(
        "hi\n@foo\nbye",
        vec![ProtectedRange {
            start: 3,
            end: 7,
            uri: "file:///foo".to_string(),
            name: "foo".to_string(),
        }],
    )];
    let mut shell = PickerShell::open(Box::new(HistoryQuerySource::new(hist)));
    let cells = render_shell_and_extract_cells(&mut shell, 40, 8);

    let styled_chars: String = cells
        .iter()
        .filter(|(_, fg, m)| *fg == Color::LightBlue && m.contains(Modifier::UNDERLINED))
        .map(|(c, _, _)| *c)
        .collect();
    assert!(
        styled_chars.contains("@foo"),
        "styled chars did not contain '@foo' on multi-line entry: {styled_chars:?}"
    );
    // Sanity: the ↵ glyph is NOT included in the styled range; it is a
    // newline marker rendered as Green+BOLD (the default row text style).
    assert!(
        !styled_chars.contains('↵'),
        "↵ newline glyph should not be in atom styling: {styled_chars:?}"
    );
}

#[test]
fn entry_without_atoms_has_no_light_blue_cells() {
    let hist = vec![mk_entry("no mentions here", vec![])];
    let mut shell = PickerShell::open(Box::new(HistoryQuerySource::new(hist)));
    let cells = render_shell_and_extract_cells(&mut shell, 40, 8);

    let styled_count = cells
        .iter()
        .filter(|(_, fg, m)| *fg == Color::LightBlue && m.contains(Modifier::UNDERLINED))
        .count();
    assert_eq!(styled_count, 0);
}
```

- [ ] **Step 2: Run the new integration test**

Run: `cargo test -p spur-tui --test picker_shell_atom_render`
Expected: `test result: ok. 3 passed; 0 failed`.

If the rendered-char assertion fails due to line wrapping or truncation, the popup width may be too narrow. `render_shell_and_extract_cells(&mut shell, 40, 8)` gives a 40-wide container; the popup will compute its own width as `clamp(max(30), container.width/2)`. If the test needs a wider container to fit `"hi ↵ @foo ↵ bye"`, bump the width argument up to 60 or 80.

- [ ] **Step 3: Run the full spur-tui test suite**

Run: `cargo test -p spur-tui`
Expected: full suite passes, no regressions.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/tests/picker_shell_atom_render.rs
git commit -m "test(spur-tui): atom render integration for PickerShell (Phase 2)

Three TestBackend-driven integration tests lock the end-to-end
guarantee that ProtectedRange spans render as Color::LightBlue +
Modifier::UNDERLINED in the popup rows, including across the
Phase 2 newline-replacement offset mapping:

  * single-line @foo entry has the '@foo' cells styled
  * multi-line hi\\n@foo\\nbye entry still has '@foo' styled after
    the 'hi ↵ @foo ↵ bye' replacement; ↵ glyph is NOT styled
  * entry without any ProtectedRange has zero styled cells

Part of: PickerShell Phase 2

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>"
```

---

## Final: Phase 2 exit verification

- [ ] **Step 1: Build spur-tui in release mode**

Run: `cargo build -p spur-tui --release`
Expected: no errors.

- [ ] **Step 2: Run the full spur-tui test suite**

Run: `cargo test -p spur-tui`
Expected: all tests pass; specifically the 16 `components::query_source` tests and the 3 `picker_shell_atom_render` tests.

- [ ] **Step 3: Run workspace-wide build**

Run: `cargo build`
Expected: no errors.

- [ ] **Step 4: Manual smoke in a running `spur watch`** (optional — the user will do this)

Press `Ctrl+R`, verify:
- Typing a query that returns a multi-line history entry shows the `@atom` spans rendered in LightBlue + underlined inside the popup row, correctly aligned against the ` ↵ ` newline markers.
- Repeatedly typing and Backspacing the same last character (e.g. `a` → `` → `a`) does not cause a visible hitch; under the hood `Pattern::parse` is called only twice in that cycle rather than four times.

---

## Self-review results

**Spec coverage (Phase 2 section):**
- ✓ "Add an internal nucleo::Matcher to each QuerySource impl" — already done in Phase 1; Phase 2 adds the `Pattern` cache that completes the spec's "No per-keystroke Matcher::new / Pattern::parse allocation" exit criterion. (Task 1.)
- ✓ "Extend RetrievalRow.atoms population in HistoryQuerySource so entry.snapshot.protected_ranges translate to byte spans on RetrievalRow.primary" — atoms translated correctly even when `primary` differs from `text` due to newline replacement. (Task 2.)
- ✓ "PickerShell's row renderer applies Color::LightBlue + Modifier::UNDERLINED to atom spans" — already shipped in Phase 1 (`picker_shell.rs:281-282`); Task 3 locks behavior with an integration test.
- ✓ "no per-keystroke Pattern::parse allocation on the Ctrl+R hot path" — Task 1 + test 1 (`same_query_repeated_does_not_reparse_pattern`).
- ✓ "Text-only backfill twins and ranges-bearing entries are visually distinct in the popup" — Task 3 `atoms_render_with_light_blue_underlined_styling_no_newline` proves the visual distinction at the cell level.

**Placeholder scan:** every code step shows the actual code. Task 2 Step 3 deliberately exhibits both the incorrect first-draft and the correct version to explain the raw-byte-cast pitfall; Step 4 is the cleanup that deletes the first version — explicit, not a placeholder.

**Type consistency:** signatures spot-checked:
- `HistoryQuerySource::{new, pattern_parse_count_for_test, ensure_pattern, row_from_entry}` consistent across Tasks 1, 2, 3.
- `Pattern::parse / Pattern::score` signatures match the `nucleo_matcher::pattern::Pattern` API already used in Phase 1 — no signature changes.
- `ProtectedRange::{start, end, uri, name}` matches the struct in `crates/spur-tui/src/components/input_bar.rs`.
- `Color::LightBlue + Modifier::UNDERLINED` assertions match the render code at `picker_shell.rs:281-282`.

No gaps. Plan ready for execution.
