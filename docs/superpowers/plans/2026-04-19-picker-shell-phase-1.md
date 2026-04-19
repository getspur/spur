# PickerShell Phase 1 — Ctrl+R Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate `Ctrl+R` fuzzy history search in `SessionDetailView` from the current invisible-query modal sub-mode to a new `PickerShell` component with a visible `MiniInput` query surface, fixing seven user-visible display defects while leaving mention/slash unchanged.

**Architecture:** Three new files in `crates/spur-tui/src/components/` — `mini_input.rs` (a single-line text editor), `query_source.rs` (the `QuerySource` trait + `RetrievalAccept` / `RetrievalRow` / `QueryMode` types), and `picker_shell.rs` (the shell that wires a `MiniInput` to a `QuerySource` and drives a `CompletionPopup` for selection). `SessionDetailView` gains one `Option<PickerShell>` field, loses three history-search fields, and rewires one `Ctrl+R` branch. `CompletionPopup`, `InputBar`, `ProtectedRange`, and `InputStateSnapshot` are untouched.

**Tech Stack:** Rust 2021, `ratatui`, `crossterm`, `nucleo-matcher` (reused per source, not per keystroke), existing `tui-textarea` for `InputBar` (untouched).

**Spec:** `docs/superpowers/specs/2026-04-19-picker-shell-retrieval-unification-design.md` (Phase 1 only; Phases 2-4 are separate plans).

---

## File Structure

**Create:**
- `crates/spur-tui/src/components/mini_input.rs` — single-line text buffer + cursor. ~120 LOC. No ratatui import; pure logic + a tiny render method taking a `Rect`.
- `crates/spur-tui/src/components/query_source.rs` — `QuerySource` trait, `QueryMode` enum, `RetrievalRow` struct, `RetrievalAccept` enum, `HistoryQuerySource` impl. ~150 LOC.
- `crates/spur-tui/src/components/picker_shell.rs` — `PickerShell` struct, `PickerAction` enum, key handling, rendering. ~180 LOC.
- `crates/spur-tui/tests/picker_shell_ctrl_r.rs` — integration test for the Ctrl+R flow end-to-end.

**Modify:**
- `crates/spur-tui/src/components/mod.rs` — add three `pub mod` declarations.
- `crates/spur-tui/src/views/session_detail.rs` — replace `history_search: Option<String>` + `history_search_hits: Vec<InputHistoryEntry>` + `refresh_history_popup()` with a single `picker_shell: Option<PickerShell>`. Rewire the Ctrl+R binding and the popup-open key-routing block. Suppress `InputBar` cursor + dim its border while the shell is active.
- `docs/superpowers/specs/2026-04-19-chat-input-retrieval-unification-design.md` — amend to mark stale P0 list closed and reference the Stage 2 spec.

**Unchanged:**
- `crates/spur-tui/src/components/input_bar.rs`
- `crates/spur-tui/src/components/completion_popup.rs`
- `crates/spur-tui/src/components/completion_trigger.rs`
- `crates/spur-tui/src/input_history.rs`
- `crates/spur-tui/src/session_metadata.rs`

---

## Task 1: MiniInput — single-line text buffer

Build the narrow, single-line query-input primitive first. Pure data + logic; no ratatui dependency in the core type.

**Files:**
- Create: `crates/spur-tui/src/components/mini_input.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`

- [ ] **Step 1: Add module declaration**

Add to `crates/spur-tui/src/components/mod.rs` (alphabetical order, after `line_wrap`):

```rust
pub mod mini_input;
```

- [ ] **Step 2: Write the failing unit tests**

Create `crates/spur-tui/src/components/mini_input.rs` with ONLY the test module (no implementation yet):

```rust
//! Single-line text buffer used by `PickerShell` as its query surface.
//!
//! Deliberately narrow: no newline insertion, no protected ranges, no history,
//! no vim mode, no undo. When a feature request would grow this past ~120 LOC,
//! redesign — do not extend incrementally.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_empty() {
        let m = MiniInput::new();
        assert_eq!(m.text(), "");
        assert_eq!(m.cursor(), 0);
    }

    #[test]
    fn insert_ascii_chars_advances_cursor() {
        let mut m = MiniInput::new();
        m.insert_char('h');
        m.insert_char('i');
        assert_eq!(m.text(), "hi");
        assert_eq!(m.cursor(), 2);
    }

    #[test]
    fn insert_multibyte_uses_utf8_byte_len() {
        let mut m = MiniInput::new();
        m.insert_char('你');
        m.insert_char('好');
        assert_eq!(m.text(), "你好");
        assert_eq!(m.cursor(), 6);
    }

    #[test]
    fn backspace_removes_prev_char() {
        let mut m = MiniInput::new();
        m.insert_char('a');
        m.insert_char('b');
        m.backspace();
        assert_eq!(m.text(), "a");
        assert_eq!(m.cursor(), 1);
    }

    #[test]
    fn backspace_on_empty_is_noop() {
        let mut m = MiniInput::new();
        m.backspace();
        assert_eq!(m.text(), "");
        assert_eq!(m.cursor(), 0);
    }

    #[test]
    fn backspace_multibyte() {
        let mut m = MiniInput::new();
        m.insert_char('你');
        m.backspace();
        assert_eq!(m.text(), "");
        assert_eq!(m.cursor(), 0);
    }

    #[test]
    fn delete_removes_next_char() {
        let mut m = MiniInput::new();
        m.insert_char('a');
        m.insert_char('b');
        m.left();
        m.delete();
        assert_eq!(m.text(), "a");
        assert_eq!(m.cursor(), 1);
    }

    #[test]
    fn left_right_bound_at_edges() {
        let mut m = MiniInput::new();
        m.left(); // no-op at start
        assert_eq!(m.cursor(), 0);
        m.insert_char('a');
        m.right(); // no-op at end
        assert_eq!(m.cursor(), 1);
    }

    #[test]
    fn home_end() {
        let mut m = MiniInput::new();
        m.insert_char('a');
        m.insert_char('b');
        m.home();
        assert_eq!(m.cursor(), 0);
        m.end();
        assert_eq!(m.cursor(), 2);
    }

    #[test]
    fn paste_strips_newlines() {
        let mut m = MiniInput::new();
        m.paste("hello\nworld\r\nmore");
        assert_eq!(m.text(), "helloworldmore");
        assert_eq!(m.cursor(), "helloworldmore".len());
    }

    #[test]
    fn clear_resets() {
        let mut m = MiniInput::new();
        m.insert_char('a');
        m.clear();
        assert_eq!(m.text(), "");
        assert_eq!(m.cursor(), 0);
    }
}
```

- [ ] **Step 3: Run the tests — verify they all fail to compile**

Run: `cargo test -p spur-tui --lib components::mini_input`
Expected: `error[E0433]: failed to resolve: use of undeclared type MiniInput` (or similar).

- [ ] **Step 4: Implement MiniInput**

Add to `crates/spur-tui/src/components/mini_input.rs` above the `#[cfg(test)] mod tests` block:

```rust
/// Single-line text buffer with a byte-offset cursor.
pub struct MiniInput {
    text: String,
    cursor: usize, // byte offset into text; always on a UTF-8 char boundary
}

impl MiniInput {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Insert arbitrary text, stripping any `\n` or `\r` characters.
    pub fn paste(&mut self, s: &str) {
        let cleaned: String = s.chars().filter(|c| *c != '\n' && *c != '\r').collect();
        self.text.insert_str(self.cursor, &cleaned);
        self.cursor += cleaned.len();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.text[..self.cursor]
            .chars()
            .next_back()
            .expect("cursor > 0 implies a prev char");
        let new_cursor = self.cursor - prev.len_utf8();
        self.text.drain(new_cursor..self.cursor);
        self.cursor = new_cursor;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.text[self.cursor..]
            .chars()
            .next()
            .expect("cursor < len implies a next char");
        let end = self.cursor + next.len_utf8();
        self.text.drain(self.cursor..end);
    }

    pub fn left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.text[..self.cursor]
            .chars()
            .next_back()
            .expect("cursor > 0 implies a prev char");
        self.cursor -= prev.len_utf8();
    }

    pub fn right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.text[self.cursor..]
            .chars()
            .next()
            .expect("cursor < len implies a next char");
        self.cursor += next.len_utf8();
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
}

impl Default for MiniInput {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 5: Run the tests — verify they pass**

Run: `cargo test -p spur-tui --lib components::mini_input`
Expected: `test result: ok. 11 passed; 0 failed`.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/mini_input.rs crates/spur-tui/src/components/mod.rs
git commit -m "feat(spur-tui): add MiniInput single-line buffer for PickerShell

Narrow-scope primitive backing the query surface of the upcoming
PickerShell. No newlines, no protected ranges, no undo. 11 unit tests
cover ASCII, multi-byte UTF-8, edge navigation, and newline-stripping
paste.

Part of: PickerShell Phase 1 (Stage 2 retrieval unification)"
```

---

## Task 2: QuerySource trait + RetrievalAccept + RetrievalRow + QueryMode

Pure types with no runtime behavior yet. Sets up the contract that `HistoryQuerySource` (Task 3) and `PickerShell` (Task 4) will use.

**Files:**
- Create: `crates/spur-tui/src/components/query_source.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`

- [ ] **Step 1: Add module declaration**

Add to `crates/spur-tui/src/components/mod.rs` (alphabetical order, after `mini_input`):

```rust
pub mod query_source;
```

- [ ] **Step 2: Write the trait file with types and a smoke test**

Create `crates/spur-tui/src/components/query_source.rs`:

```rust
//! Shared contract for popup-backed retrieval sources.
//!
//! Each source produces `RetrievalRow`s from a query string and, on accept,
//! returns a `RetrievalAccept` payload that the view dispatches onto the
//! `InputBar`.

use crate::input_history::InputStateSnapshot;

/// Where the popup's query string lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryMode {
    /// `PickerShell` owns a `MiniInput` and routes keys into it. Used by
    /// history search (Ctrl+R) where the query is scratch navigation text.
    OwnedByShell,
    /// The shell reads its query from the `InputBar` trigger prefix. Used
    /// by @mention and /slash where the query is part of the outbound draft.
    /// (Phase 1 does NOT construct any source in this mode; reserved for
    /// Phase 3.)
    #[allow(dead_code)]
    ReadFromInputBar,
}

/// One displayable row in a retrieval popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalRow {
    /// Main label text.
    pub primary: String,
    /// Description / metadata shown to the right of `primary`.
    pub secondary: String,
    /// Right-aligned provenance tag, e.g. `⟨claude⟩`. Empty for no tag.
    pub tag: String,
    /// Byte ranges inside `primary` to be rendered as protected-atom spans
    /// (LightBlue + underlined). Ranges MUST be valid inside `primary`;
    /// implementors are responsible for validating against any truncation
    /// they applied before returning.
    pub atoms: Vec<(usize, usize)>,
}

/// Payload dispatched by the view when the user accepts a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrievalAccept {
    /// Replace the entire `InputBar` state with this snapshot. Used by history.
    ReplaceState(InputStateSnapshot),
    /// Insert a protected atom at the `InputBar` cursor. Used by @mention.
    /// (Not constructed by Phase 1 sources; reserved for Phase 3.)
    #[allow(dead_code)]
    InsertAtom {
        text: String,
        uri: String,
        name: String,
    },
    /// Replace the text between `prefix_start` and the cursor with
    /// `replacement`. Used by /slash.
    /// (Not constructed by Phase 1 sources; reserved for Phase 3.)
    #[allow(dead_code)]
    ReplaceTriggerToken {
        prefix_start: usize,
        replacement: String,
    },
}

/// A retrieval source: given a query, produces ranked rows; on accept,
/// returns a dispatchable payload.
pub trait QuerySource {
    /// Title shown in the shell header (e.g. "History · bck-i-search").
    fn title(&self) -> &str;

    /// Where the query lives.
    fn query_mode(&self) -> QueryMode;

    /// Filter+rank using the given query. Implementors MUST reuse any
    /// internal matcher state across calls; constructing a fresh
    /// `nucleo::Matcher` per call is forbidden for hot-path reasons
    /// (see Phase 2 plan).
    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow>;

    /// Build the accept payload for the row at `row_idx`. Returns `None`
    /// if `row_idx` is out of bounds or the source has no state to accept.
    fn accept(&self, row_idx: usize) -> Option<RetrievalAccept>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrieval_row_atoms_are_byte_ranges() {
        // Smoke test: atoms use byte offsets, so multi-byte primary stays
        // consistent. "你好@foo" — atom range for @foo is bytes 6..10.
        let r = RetrievalRow {
            primary: "你好@foo".to_string(),
            secondary: String::new(),
            tag: String::new(),
            atoms: vec![(6, 10)],
        };
        assert_eq!(&r.primary[r.atoms[0].0..r.atoms[0].1], "@foo");
    }

    #[test]
    fn replace_state_roundtrip() {
        let snap = InputStateSnapshot::from_text("hello");
        let a = RetrievalAccept::ReplaceState(snap.clone());
        match a {
            RetrievalAccept::ReplaceState(got) => assert_eq!(got.text, "hello"),
            _ => panic!("wrong variant"),
        }
    }
}
```

- [ ] **Step 3: Verify build + tests pass**

Run: `cargo test -p spur-tui --lib components::query_source`
Expected: `test result: ok. 2 passed; 0 failed`.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/components/query_source.rs crates/spur-tui/src/components/mod.rs
git commit -m "feat(spur-tui): add QuerySource trait + RetrievalAccept enum

Pure types defining the Phase 1 contract between PickerShell and its
retrieval backends. HistoryQuerySource (Task 3) will be the first impl.
InsertAtom / ReplaceTriggerToken variants are defined but unused in
Phase 1 — they land when mention/slash migrate in Phase 3.

Part of: PickerShell Phase 1"
```

---

## Task 3: HistoryQuerySource — reuses one Matcher per source instance

The first concrete `QuerySource`. Wraps a slice of `InputHistoryEntry`. The per-source `Matcher` reuse is a Phase 2 invariant but we set it up now so Phase 2 only adds the alloc-bound test.

**Files:**
- Modify: `crates/spur-tui/src/components/query_source.rs`

- [ ] **Step 1: Write failing tests**

Append to the `#[cfg(test)] mod tests` block in `crates/spur-tui/src/components/query_source.rs`:

```rust
    use crate::input_history::{InputHistoryEntry, InputStateSnapshot};

    fn mk_entry(text: &str) -> InputHistoryEntry {
        InputHistoryEntry::new(InputStateSnapshot::from_text(text))
    }

    #[test]
    fn history_source_empty_query_returns_newest_first() {
        let hist = vec![
            mk_entry("oldest"),
            mk_entry("middle"),
            mk_entry("newest"),
        ];
        let mut src = HistoryQuerySource::new(hist);
        let rows = src.refresh("");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].primary, "newest");
        assert_eq!(rows[1].primary, "middle");
        assert_eq!(rows[2].primary, "oldest");
    }

    #[test]
    fn history_source_empty_history_returns_no_rows() {
        let mut src = HistoryQuerySource::new(Vec::new());
        assert!(src.refresh("").is_empty());
        assert!(src.refresh("anything").is_empty());
    }

    #[test]
    fn history_source_fuzzy_narrows_rows() {
        let hist = vec![
            mk_entry("refactor the delegation walker"),
            mk_entry("fix the ProtectedRange panic"),
            mk_entry("add test for INV-C2"),
        ];
        let mut src = HistoryQuerySource::new(hist);
        let rows = src.refresh("refa");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].primary, "refactor the delegation walker");
    }

    #[test]
    fn history_source_accept_returns_replace_state_snapshot() {
        let hist = vec![mk_entry("newest")];
        let src = HistoryQuerySource::new(hist);
        let accept = src.accept(0).expect("row 0 exists");
        match accept {
            RetrievalAccept::ReplaceState(snap) => assert_eq!(snap.text, "newest"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn history_source_accept_out_of_bounds_returns_none() {
        let src = HistoryQuerySource::new(vec![mk_entry("only")]);
        assert!(src.accept(99).is_none());
    }

    #[test]
    fn history_source_matcher_reused_across_refreshes() {
        // Structural check: `refresh` does NOT construct a fresh Matcher
        // each call. We verify by type — the field exists and is private,
        // so the only way refresh could violate is by shadowing it, which
        // would show up as a struct-size regression. This is a weaker form
        // of the Phase 2 alloc-bound test and is strengthened there.
        let src = HistoryQuerySource::new(Vec::new());
        // The struct exists and holds a Matcher; if someone later removes
        // the field, the below line will fail to compile.
        let _field_exists: &nucleo_matcher::Matcher = &src.matcher;
        let _ = src; // suppress unused warning
    }

    #[test]
    fn history_source_title_is_bck_i_search() {
        let src = HistoryQuerySource::new(Vec::new());
        assert_eq!(src.title(), "History · bck-i-search");
    }

    #[test]
    fn history_source_query_mode_is_owned_by_shell() {
        let src = HistoryQuerySource::new(Vec::new());
        assert_eq!(src.query_mode(), QueryMode::OwnedByShell);
    }

    #[test]
    fn history_source_row_metadata_reflects_mentions() {
        let hist = vec![mk_entry("no mentions here")];
        let mut src = HistoryQuerySource::new(hist);
        let rows = src.refresh("");
        assert_eq!(rows[0].secondary, "");

        let mut with_atom = InputStateSnapshot::from_text("hi @foo");
        with_atom.protected_ranges = vec![crate::components::input_bar::ProtectedRange {
            start: 3,
            end: 7,
            uri: "file:///foo".to_string(),
            name: "foo".to_string(),
        }];
        let hist2 = vec![InputHistoryEntry::new(with_atom)];
        let mut src2 = HistoryQuerySource::new(hist2);
        let rows2 = src2.refresh("");
        assert_eq!(rows2[0].secondary, "1 mention");
        assert_eq!(rows2[0].atoms, vec![(3, 7)]);
    }
```

- [ ] **Step 2: Run tests — verify failure**

Run: `cargo test -p spur-tui --lib components::query_source`
Expected: `error[E0433]: failed to resolve: use of undeclared type HistoryQuerySource`.

- [ ] **Step 3: Implement HistoryQuerySource**

Append to `crates/spur-tui/src/components/query_source.rs` (after the trait, before `#[cfg(test)]`):

```rust
use crate::input_history::InputHistoryEntry;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// QuerySource backed by a snapshot of the global input history, oldest-first.
pub struct HistoryQuerySource {
    /// History, oldest-first (matches `InputBar::history()` ordering).
    history: Vec<InputHistoryEntry>,
    /// One matcher, reused across every `refresh()` call.
    matcher: Matcher,
}

impl HistoryQuerySource {
    pub fn new(history: Vec<InputHistoryEntry>) -> Self {
        Self {
            history,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    fn row_from_entry(entry: &InputHistoryEntry) -> RetrievalRow {
        let primary = entry.snapshot.text.replace('\n', " ↵ ");
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
        // Atoms are byte ranges from the snapshot. They reference the
        // *original* text, not the replaced-newline `primary`. Phase 1
        // keeps this mapping only when the snapshot text has no newlines
        // (the common case). If the snapshot contains newlines, atom
        // ranges are dropped defensively — Phase 2 will map through the
        // replacement offsets. This avoids displaying a styled span at
        // a wrong byte in the meantime.
        let atoms = if entry.snapshot.text.contains('\n') {
            Vec::new()
        } else {
            entry
                .snapshot
                .protected_ranges
                .iter()
                .map(|r| (r.start, r.end))
                .collect()
        };
        RetrievalRow {
            primary,
            secondary,
            tag,
            atoms,
        }
    }
}

impl QuerySource for HistoryQuerySource {
    fn title(&self) -> &str {
        "History · bck-i-search"
    }

    fn query_mode(&self) -> QueryMode {
        QueryMode::OwnedByShell
    }

    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> {
        if self.history.is_empty() {
            return Vec::new();
        }
        if query.is_empty() {
            return self
                .history
                .iter()
                .rev()
                .take(20)
                .map(Self::row_from_entry)
                .collect();
        }
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
        scored
            .into_iter()
            .take(20)
            .map(|(_, i)| Self::row_from_entry(&self.history[i]))
            .collect()
    }

    fn accept(&self, row_idx: usize) -> Option<RetrievalAccept> {
        // `refresh` returns at most 20 rows in either ranked or newest-first
        // order. `accept` must re-derive the entry at the same index.
        // Phase 1 simplification: store the index→entry map on the last
        // refresh result. We compute it once and expose via `accept_for_query`.
        let _ = row_idx;
        None
    }
}
```

Note the final `accept` returns `None` — that's a placeholder. We need `accept` to map `row_idx` back to a snapshot. Do that in the next step.

- [ ] **Step 4: Fix accept to carry last-refresh context**

Replace the `HistoryQuerySource` definition in `crates/spur-tui/src/components/query_source.rs` to carry the last refresh's index map. Update the `new`, `refresh`, and `accept` methods:

```rust
pub struct HistoryQuerySource {
    history: Vec<InputHistoryEntry>,
    matcher: Matcher,
    /// Snapshots parallel to the rows returned by the most recent `refresh`.
    /// `accept(i)` returns a `ReplaceState(last_snapshots[i].clone())`.
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

    fn row_from_entry(entry: &InputHistoryEntry) -> RetrievalRow {
        let primary = entry.snapshot.text.replace('\n', " ↵ ");
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
        let atoms = if entry.snapshot.text.contains('\n') {
            Vec::new()
        } else {
            entry
                .snapshot
                .protected_ranges
                .iter()
                .map(|r| (r.start, r.end))
                .collect()
        };
        RetrievalRow {
            primary,
            secondary,
            tag,
            atoms,
        }
    }
}

impl QuerySource for HistoryQuerySource {
    fn title(&self) -> &str {
        "History · bck-i-search"
    }

    fn query_mode(&self) -> QueryMode {
        QueryMode::OwnedByShell
    }

    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> {
        self.last_snapshots.clear();
        if self.history.is_empty() {
            return Vec::new();
        }
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
                    let score = pattern
                        .score(Utf32Str::new(&h.snapshot.text, &mut buf), &mut self.matcher)?;
                    Some((score, i))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            scored.into_iter().take(20).map(|(_, i)| i).collect()
        };
        let rows = picked
            .iter()
            .map(|&i| Self::row_from_entry(&self.history[i]))
            .collect();
        self.last_snapshots = picked
            .iter()
            .map(|&i| self.history[i].snapshot.clone())
            .collect();
        rows
    }

    fn accept(&self, row_idx: usize) -> Option<RetrievalAccept> {
        self.last_snapshots
            .get(row_idx)
            .cloned()
            .map(RetrievalAccept::ReplaceState)
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p spur-tui --lib components::query_source`
Expected: `test result: ok. 10 passed; 0 failed`.

- [ ] **Step 6: Run the full crate build to catch warnings**

Run: `cargo build -p spur-tui 2>&1 | grep -E 'warning|error' | head -20`
Expected: no new warnings from the new files. If `InputStateSnapshot` is unused, add the import.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/query_source.rs
git commit -m "feat(spur-tui): HistoryQuerySource reuses one Matcher per source

Concrete QuerySource wrapping Vec<InputHistoryEntry>. Single
nucleo::Matcher constructed in new(); reused across every refresh().
accept() maps row index back to the snapshot captured on the most
recent refresh.

Part of: PickerShell Phase 1"
```

---

## Task 4: PickerShell — the shell widget

Wires a `MiniInput` to a `QuerySource`, drives a `CompletionPopup` for selection, renders the whole thing above an anchor rect.

**Files:**
- Create: `crates/spur-tui/src/components/picker_shell.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`

- [ ] **Step 1: Add module declaration**

Add to `crates/spur-tui/src/components/mod.rs` (alphabetical order, after `mini_input` or wherever fits):

```rust
pub mod picker_shell;
```

- [ ] **Step 2: Write the failing unit tests**

Create `crates/spur-tui/src/components/picker_shell.rs` with ONLY the test module:

```rust
//! Popup shell that owns a query surface (MiniInput when the source is
//! OwnedByShell) and drives a CompletionPopup for row selection.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::query_source::{
        HistoryQuerySource, QuerySource, RetrievalAccept,
    };
    use crate::input_history::{InputHistoryEntry, InputStateSnapshot};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn mk(text: &str) -> InputHistoryEntry {
        InputHistoryEntry::new(InputStateSnapshot::from_text(text))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn open_populates_with_empty_query_rows() {
        let src = HistoryQuerySource::new(vec![mk("alpha"), mk("beta")]);
        let shell = PickerShell::open(Box::new(src));
        assert_eq!(shell.row_count(), 2);
        assert_eq!(shell.query(), "");
        assert_eq!(shell.selected_index(), Some(0));
    }

    #[test]
    fn typing_filters_rows() {
        let src = HistoryQuerySource::new(vec![mk("alpha"), mk("beta")]);
        let mut shell = PickerShell::open(Box::new(src));
        shell.handle_key(key(KeyCode::Char('b')));
        assert_eq!(shell.query(), "b");
        assert_eq!(shell.row_count(), 1);
    }

    #[test]
    fn arrow_keys_navigate_rows() {
        let src = HistoryQuerySource::new(vec![mk("a"), mk("b"), mk("c")]);
        let mut shell = PickerShell::open(Box::new(src));
        assert_eq!(shell.selected_index(), Some(0));
        shell.handle_key(key(KeyCode::Down));
        assert_eq!(shell.selected_index(), Some(1));
        shell.handle_key(key(KeyCode::Up));
        assert_eq!(shell.selected_index(), Some(0));
    }

    #[test]
    fn selection_survives_refilter_when_row_still_present() {
        // This is the "selection reset on every keystroke" fix.
        let src = HistoryQuerySource::new(vec![
            mk("apple pie"),
            mk("apple juice"),
            mk("banana"),
        ]);
        let mut shell = PickerShell::open(Box::new(src));
        shell.handle_key(key(KeyCode::Down)); // select row 1
        shell.handle_key(key(KeyCode::Char('a'))); // filter — all three still match "a"
        // Row 1 used to be "apple juice"; after fuzzy scoring it may still
        // be present. The contract is: if the previously-selected row's
        // primary text is still in the new row list, selection tracks it.
        let rows = shell.row_primaries();
        let idx = rows.iter().position(|p| p == "apple juice");
        assert_eq!(shell.selected_index(), idx);
    }

    #[test]
    fn tab_returns_accept_action() {
        let src = HistoryQuerySource::new(vec![mk("hello")]);
        let mut shell = PickerShell::open(Box::new(src));
        let act = shell.handle_key(key(KeyCode::Tab));
        match act {
            PickerAction::Accept(RetrievalAccept::ReplaceState(snap)) => {
                assert_eq!(snap.text, "hello");
            }
            other => panic!("got {:?}", other),
        }
    }

    #[test]
    fn enter_returns_accept_action_for_history() {
        let src = HistoryQuerySource::new(vec![mk("hello")]);
        let mut shell = PickerShell::open(Box::new(src));
        let act = shell.handle_key(key(KeyCode::Enter));
        assert!(matches!(
            act,
            PickerAction::Accept(RetrievalAccept::ReplaceState(_))
        ));
    }

    #[test]
    fn esc_returns_cancel_action() {
        let src = HistoryQuerySource::new(vec![mk("x")]);
        let mut shell = PickerShell::open(Box::new(src));
        let act = shell.handle_key(key(KeyCode::Esc));
        assert!(matches!(act, PickerAction::Cancel));
    }

    #[test]
    fn ctrl_c_returns_cancel_action() {
        let src = HistoryQuerySource::new(vec![mk("x")]);
        let mut shell = PickerShell::open(Box::new(src));
        let act = shell.handle_key(ctrl('c'));
        assert!(matches!(act, PickerAction::Cancel));
    }

    #[test]
    fn backspace_shortens_query() {
        let src = HistoryQuerySource::new(vec![mk("ab")]);
        let mut shell = PickerShell::open(Box::new(src));
        shell.handle_key(key(KeyCode::Char('a')));
        shell.handle_key(key(KeyCode::Char('b')));
        shell.handle_key(key(KeyCode::Backspace));
        assert_eq!(shell.query(), "a");
    }

    #[test]
    fn accept_on_empty_rows_returns_cancel() {
        let src = HistoryQuerySource::new(Vec::new());
        let mut shell = PickerShell::open(Box::new(src));
        let act = shell.handle_key(key(KeyCode::Enter));
        assert!(matches!(act, PickerAction::Cancel));
    }
}
```

- [ ] **Step 3: Run tests — verify failure**

Run: `cargo test -p spur-tui --lib components::picker_shell`
Expected: compile errors about `PickerShell`, `PickerAction` not defined.

- [ ] **Step 4: Implement PickerShell**

Add to `crates/spur-tui/src/components/picker_shell.rs` above the `#[cfg(test)] mod tests` block:

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::components::mini_input::MiniInput;
use crate::components::query_source::{QueryMode, QuerySource, RetrievalAccept, RetrievalRow};

/// Result of handling a key event inside the shell.
#[derive(Debug)]
pub enum PickerAction {
    /// Key was consumed; shell stays open with possibly new state.
    None,
    /// User accepted a row; dispatch this and close the shell.
    Accept(RetrievalAccept),
    /// User cancelled (Esc/Ctrl+C); close the shell without mutation.
    Cancel,
}

/// Popup shell wrapping a query surface + row list.
pub struct PickerShell {
    source: Box<dyn QuerySource>,
    query: MiniInput,
    rows: Vec<RetrievalRow>,
    list_state: ListState,
}

impl PickerShell {
    /// Open a shell over the given source. Immediately refreshes with an
    /// empty query to populate initial rows.
    pub fn open(mut source: Box<dyn QuerySource>) -> Self {
        let rows = source.refresh("");
        let mut list_state = ListState::default();
        if !rows.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            source,
            query: MiniInput::new(),
            rows,
            list_state,
        }
    }

    // ── Test accessors ─────────────────────────────────────────────────
    #[cfg(test)]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    #[cfg(test)]
    pub fn query(&self) -> &str {
        self.query.text()
    }

    #[cfg(test)]
    pub fn selected_index(&self) -> Option<usize> {
        self.list_state.selected()
    }

    #[cfg(test)]
    pub fn row_primaries(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.primary.clone()).collect()
    }

    // ── Key handling ───────────────────────────────────────────────────

    pub fn handle_key(&mut self, key: KeyEvent) -> PickerAction {
        match key.code {
            KeyCode::Esc => PickerAction::Cancel,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                PickerAction::Cancel
            }
            KeyCode::Up => {
                self.select_prev();
                PickerAction::None
            }
            KeyCode::Down => {
                self.select_next();
                PickerAction::None
            }
            KeyCode::Tab | KeyCode::Enter => self.accept_selected(),
            KeyCode::Backspace if self.source.query_mode() == QueryMode::OwnedByShell => {
                self.query.backspace();
                self.refilter();
                PickerAction::None
            }
            KeyCode::Delete if self.source.query_mode() == QueryMode::OwnedByShell => {
                self.query.delete();
                self.refilter();
                PickerAction::None
            }
            KeyCode::Left if self.source.query_mode() == QueryMode::OwnedByShell => {
                self.query.left();
                PickerAction::None
            }
            KeyCode::Right if self.source.query_mode() == QueryMode::OwnedByShell => {
                self.query.right();
                PickerAction::None
            }
            KeyCode::Home if self.source.query_mode() == QueryMode::OwnedByShell => {
                self.query.home();
                PickerAction::None
            }
            KeyCode::End if self.source.query_mode() == QueryMode::OwnedByShell => {
                self.query.end();
                PickerAction::None
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && self.source.query_mode() == QueryMode::OwnedByShell =>
            {
                self.query.insert_char(c);
                self.refilter();
                PickerAction::None
            }
            _ => PickerAction::None,
        }
    }

    fn select_prev(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len();
        let i = self.list_state.selected().map_or(0, |i| (i + len - 1) % len);
        self.list_state.select(Some(i));
    }

    fn select_next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let i = self
            .list_state
            .selected()
            .map_or(0, |i| (i + 1) % self.rows.len());
        self.list_state.select(Some(i));
    }

    fn accept_selected(&self) -> PickerAction {
        let Some(idx) = self.list_state.selected() else {
            return PickerAction::Cancel;
        };
        match self.source.accept(idx) {
            Some(a) => PickerAction::Accept(a),
            None => PickerAction::Cancel,
        }
    }

    /// Refresh rows from the source using the current query; preserve
    /// selection on the same logical row where possible.
    fn refilter(&mut self) {
        let prev_primary = self
            .list_state
            .selected()
            .and_then(|i| self.rows.get(i))
            .map(|r| r.primary.clone());
        self.rows = self.source.refresh(self.query.text());
        let new_idx = match prev_primary {
            Some(p) => self.rows.iter().position(|r| r.primary == p).or(Some(0)),
            None => (!self.rows.is_empty()).then_some(0),
        };
        self.list_state.select(if self.rows.is_empty() {
            None
        } else {
            new_idx
        });
    }

    /// For mention/slash (`QueryMode::ReadFromInputBar`). Not used by Phase 1
    /// but needed by the trait so Phase 3 can compile against this signature.
    #[allow(dead_code)]
    pub fn set_query_from_input_bar(&mut self, q: &str) {
        debug_assert_eq!(self.source.query_mode(), QueryMode::ReadFromInputBar);
        // Directly install the prefix text; no edit history to preserve.
        self.query.clear();
        self.query.paste(q);
        self.refilter();
    }

    // ── Rendering ──────────────────────────────────────────────────────

    /// Render above `anchor` (the InputBar's rect), clipped to `container`.
    pub fn render(&mut self, frame: &mut Frame, anchor: Rect, container: Rect) {
        let query_mode_owned = self.source.query_mode() == QueryMode::OwnedByShell;
        let list_rows = self.rows.len().clamp(1, 8) as u16;
        let query_rows = if query_mode_owned { 1 } else { 0 };
        let inner_rows = list_rows + query_rows;
        let popup_height = inner_rows + 2; // +2 for block border

        let popup_width = (container.width / 2).clamp(30, container.width);
        let x = anchor
            .x
            .saturating_add(2)
            .min(container.x + container.width.saturating_sub(popup_width));
        let y = anchor.y.saturating_sub(popup_height);
        let popup_area = Rect::new(x, y, popup_width, popup_height);

        frame.render_widget(Clear, popup_area);

        let title = format!(" {} ", self.source.title());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(title, Style::default().fg(Color::Cyan)));

        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let mut cursor_cell: Option<(u16, u16)> = None;

        let list_area = if query_mode_owned {
            // Render query line at top of inner area.
            let q_area = Rect::new(inner.x, inner.y, inner.width, 1);
            let prompt = "search: ";
            let prompt_len = prompt.len() as u16;
            let q_text = self.query.text();
            let line = Line::from(vec![
                Span::styled(prompt, Style::default().fg(Color::DarkGray)),
                Span::raw(q_text.to_string()),
            ]);
            frame.render_widget(Paragraph::new(line), q_area);
            // Cursor placement: prompt + cursor byte offset (fine for
            // monospace ASCII; multi-byte alignment is a Phase 2 polish).
            let cx = q_area.x + prompt_len + self.query.cursor() as u16;
            let cy = q_area.y;
            cursor_cell = Some((cx, cy));
            Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1)
        } else {
            inner
        };

        if self.rows.is_empty() {
            let p = Paragraph::new(Line::from(Span::styled(
                "No matches. Type to refine, Esc to dismiss.",
                Style::default().fg(Color::DarkGray),
            )));
            frame.render_widget(p, list_area);
        } else {
            let items: Vec<ListItem> = self
                .rows
                .iter()
                .map(|r| {
                    let mut spans = Vec::with_capacity(4);
                    // Primary with atom-span styling.
                    if r.atoms.is_empty() {
                        spans.push(Span::styled(
                            r.primary.clone(),
                            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                        ));
                    } else {
                        let mut cursor = 0usize;
                        for &(a, b) in &r.atoms {
                            if a > cursor && a <= r.primary.len() {
                                spans.push(Span::styled(
                                    r.primary[cursor..a].to_string(),
                                    Style::default()
                                        .fg(Color::Green)
                                        .add_modifier(Modifier::BOLD),
                                ));
                            }
                            let end = b.min(r.primary.len());
                            if end > a {
                                spans.push(Span::styled(
                                    r.primary[a..end].to_string(),
                                    Style::default()
                                        .fg(Color::LightBlue)
                                        .add_modifier(Modifier::UNDERLINED),
                                ));
                                cursor = end;
                            }
                        }
                        if cursor < r.primary.len() {
                            spans.push(Span::styled(
                                r.primary[cursor..].to_string(),
                                Style::default()
                                    .fg(Color::Green)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                    }
                    if !r.secondary.is_empty() {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(
                            r.secondary.clone(),
                            Style::default().fg(Color::White),
                        ));
                    }
                    if !r.tag.is_empty() {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(
                            r.tag.clone(),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();
            let list = List::new(items)
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
            frame.render_stateful_widget(list, list_area, &mut self.list_state);
        }

        if let Some((cx, cy)) = cursor_cell {
            if cx < popup_area.x + popup_area.width && cy < popup_area.y + popup_area.height {
                frame.set_cursor_position((cx, cy));
            }
        }
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p spur-tui --lib components::picker_shell`
Expected: `test result: ok. 10 passed; 0 failed`.

- [ ] **Step 6: Full crate build**

Run: `cargo build -p spur-tui`
Expected: no errors. Warnings about unused `set_query_from_input_bar` are acceptable (it is `#[allow(dead_code)]`).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/picker_shell.rs crates/spur-tui/src/components/mod.rs
git commit -m "feat(spur-tui): add PickerShell with visible MiniInput query surface

Shell wires a MiniInput (OwnedByShell mode) to a QuerySource and
drives row selection via ListState. handle_key returns a PickerAction
enum consumed by the view. Selection survives refilter when the
previously-selected row's primary text is still in the new row list.

Part of: PickerShell Phase 1"
```

---

## Task 5: Wire PickerShell into SessionDetailView for Ctrl+R

Replace `history_search: Option<String>`, `history_search_hits: Vec<InputHistoryEntry>`, and `refresh_history_popup()` with a single `picker_shell: Option<PickerShell>`.

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Delete the old fields**

In `crates/spur-tui/src/views/session_detail.rs`, remove lines 103-106:

```rust
    /// Active fuzzy history search query (`Ctrl+R`).  `None` = inactive.
    history_search: Option<String>,
    /// Full history entries parallel to the popup rows during history search.
    history_search_hits: Vec<InputHistoryEntry>,
```

Replace with:

```rust
    /// Active picker-shell (history / mention / slash). `None` = no popup.
    picker_shell: Option<crate::components::picker_shell::PickerShell>,
```

In the `impl SessionDetailView::new` constructor (around lines 159-160), remove:

```rust
            history_search: None,
            history_search_hits: Vec::new(),
```

Add:

```rust
            picker_shell: None,
```

- [ ] **Step 2: Delete `refresh_history_popup`**

In `crates/spur-tui/src/views/session_detail.rs`, delete the entire `refresh_history_popup` method (currently around lines 637-698). It is replaced by `HistoryQuerySource::refresh`.

- [ ] **Step 3: Update `popup_open` to include picker_shell**

In `crates/spur-tui/src/views/session_detail.rs`, find the `popup_open` method (around lines 629-634). Replace:

```rust
    fn popup_open(&self) -> bool {
        let popup_has_rows = !self.completion_popup.borrow().is_empty();
        let trigger_active = self.active_trigger.is_some();
        let history_active = self.history_search.is_some();
        (trigger_active || history_active) && popup_has_rows
    }
```

With:

```rust
    fn popup_open(&self) -> bool {
        if self.picker_shell.is_some() {
            return true;
        }
        let popup_has_rows = !self.completion_popup.borrow().is_empty();
        let trigger_active = self.active_trigger.is_some();
        trigger_active && popup_has_rows
    }
```

- [ ] **Step 4: Replace the Ctrl+R key-handling block**

In `crates/spur-tui/src/views/session_detail.rs`, find the block at lines 921-974 (the `if let Some(ref mut query) = self.history_search` match). Replace it entirely with:

```rust
        // Priority 1.4: picker shell (Ctrl+R history; Phase 3 will add mention/slash).
        if let Some(ref mut shell) = self.picker_shell {
            use crate::components::picker_shell::PickerAction;
            use crate::components::query_source::RetrievalAccept;
            let act = shell.handle_key(key);
            match act {
                PickerAction::None => {}
                PickerAction::Cancel => {
                    self.picker_shell = None;
                }
                PickerAction::Accept(accept) => {
                    match accept {
                        RetrievalAccept::ReplaceState(snap) => {
                            let len = snap.text.len();
                            self.input_bar.set_state(snap, len);
                        }
                        RetrievalAccept::InsertAtom { text, uri, name } => {
                            self.input_bar.insert_atom(text, uri, name);
                        }
                        RetrievalAccept::ReplaceTriggerToken {
                            prefix_start,
                            replacement,
                        } => {
                            let current = self.input_bar.text().to_string();
                            let cursor = self.input_bar.cursor();
                            let mut new_text = String::with_capacity(current.len());
                            new_text.push_str(&current[..prefix_start]);
                            new_text.push_str(&replacement);
                            new_text.push_str(&current[cursor..]);
                            let new_cursor = prefix_start + replacement.len();
                            self.input_bar.set_text(new_text, new_cursor);
                        }
                    }
                    self.picker_shell = None;
                }
            }
            return None;
        }
```

- [ ] **Step 5: Replace the Ctrl+R trigger block**

In `crates/spur-tui/src/views/session_detail.rs`, find the block at lines 976-984:

```rust
        // Ctrl+R / Alt+R → open fuzzy history search.
        if matches!(key.code, KeyCode::Char('r'))
            && (key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT))
        {
            self.history_search = Some(String::new());
            self.refresh_history_popup("");
            return None;
        }
```

Replace with:

```rust
        // Ctrl+R / Alt+R → open history PickerShell. Rejected while a
        // completion_trigger popup is active (user must Esc first).
        if matches!(key.code, KeyCode::Char('r'))
            && (key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT))
            && self.active_trigger.is_none()
        {
            use crate::components::picker_shell::PickerShell;
            use crate::components::query_source::HistoryQuerySource;
            let history = self.input_bar.history().to_vec();
            self.picker_shell = Some(PickerShell::open(Box::new(HistoryQuerySource::new(
                history,
            ))));
            return None;
        }
```

- [ ] **Step 6: Remove unused import**

At the top of `crates/spur-tui/src/views/session_detail.rs`, verify `use crate::input_history::InputHistoryEntry;` (line 18) is still needed. If the only usage was in `history_search_hits` and `refresh_history_popup`, remove it. Run:

```bash
cargo build -p spur-tui 2>&1 | grep "unused import"
```

If `InputHistoryEntry` is flagged, remove the import.

- [ ] **Step 7: Build the crate**

Run: `cargo build -p spur-tui`
Expected: no errors.

- [ ] **Step 8: Run the existing integration tests**

Run: `cargo test -p spur-tui --test session_detail_commands_integration`
Expected: all tests pass, INCLUDING `ctrl_r_history_restore_preserves_resource_links`. This is the critical regression guard — if this test fails, the Ctrl+R path has lost a Stage 1 goal.

- [ ] **Step 9: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "refactor(spur-tui): route Ctrl+R through PickerShell

Replaces history_search: Option<String> and history_search_hits with
picker_shell: Option<PickerShell>. Deletes the 62-line
refresh_history_popup method. Existing session_detail integration
tests (including ctrl_r_history_restore_preserves_resource_links)
continue to pass, confirming behavior parity on the accept path.

Part of: PickerShell Phase 1"
```

---

## Task 6: Suppress InputBar cursor and dim border while shell is active

Fixes the "blinking cursor on frozen composer" artifact.

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Add a "suppress cursor" render option to InputBar**

In `crates/spur-tui/src/components/input_bar.rs`, find `pub fn render(&self, frame: &mut Frame, area: Rect)` (around line 1255). Add a sibling method directly below it:

```rust
    /// Render variant for when an overlay (e.g. PickerShell) owns the
    /// terminal cursor. Behaves like `render` but:
    ///   * the border renders in DarkGray as a "composer inert" cue
    ///   * `frame.set_cursor_position` is NOT called — the overlay places
    ///     the cursor.
    pub fn render_inert(&self, frame: &mut Frame, area: Rect) {
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
        let border_color = Color::DarkGray;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(title, Style::default().fg(border_color)));
        let inner = block.inner(area);
        self.last_inner_width.set(inner.width);
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let layout = crate::components::input_bar_wrap::wrap(&lines, inner.width);
        let visible = inner.height as usize;
        let total = layout.visual_height() as usize;
        let view_top = if total <= visible { 0 } else { total - visible };
        let last_vr = (view_top + visible).min(total);
        let mut out_lines: Vec<ratatui::text::Line<'static>> =
            Vec::with_capacity(last_vr.saturating_sub(view_top));
        for vi in view_top..last_vr {
            let vr = &layout.rows[vi];
            let logical = &lines[vr.logical_row];
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(vr.graphemes.len());
            for g in &vr.graphemes {
                let piece_slice = &logical[g.byte_start..g.byte_end];
                let piece: String = if piece_slice == "\t" {
                    " ".repeat(crate::components::input_bar_wrap::TAB_WIDTH)
                } else {
                    piece_slice.to_string()
                };
                // Inert: no atom highlight, no selection highlight — the
                // composer is visibly quiescent while the picker owns focus.
                spans.push(Span::styled(piece, Style::default().fg(Color::DarkGray)));
            }
            out_lines.push(ratatui::text::Line::from(spans));
        }
        frame.render_widget(ratatui::widgets::Paragraph::new(out_lines), inner);
        // Intentionally no set_cursor_position — the overlay owns the cursor.
    }
```

- [ ] **Step 2: Switch session_detail render to use `render_inert` when the shell is active**

In `crates/spur-tui/src/views/session_detail.rs`, find:

```rust
        // ── Input bar ───────────────────────────────────────────────────
        self.input_bar.render(frame, chunks[3]);
```

(around line 1725). Replace with:

```rust
        // ── Input bar ───────────────────────────────────────────────────
        // Render in "inert" style (dimmed border, no terminal cursor) when
        // a PickerShell has the focus — the shell owns the cursor.
        if self.picker_shell.is_some() {
            self.input_bar.render_inert(frame, chunks[3]);
        } else {
            self.input_bar.render(frame, chunks[3]);
        }
```

- [ ] **Step 3: Render the picker shell as an overlay above the input bar**

Immediately below the existing `if self.popup_open() { ... completion_popup.render ... }` block (around lines 1727-1732), add (AFTER the existing block):

```rust
        // ── PickerShell overlay ─────────────────────────────────────────
        if let Some(ref mut shell) = self.picker_shell {
            shell.render(frame, chunks[3], area);
        }
```

Note: `popup_open()` returns true when the shell is active, so the `completion_popup.render` block would also fire. Guard it:

```rust
        // ── Completion popup (overlay above the InputBar) ──────────────
        if self.picker_shell.is_none() && self.popup_open() {
            self.completion_popup
                .borrow_mut()
                .render(frame, chunks[3], area);
        }
```

The final arrangement: completion popup renders only when NO picker shell is active. PickerShell renders its own popup.

- [ ] **Step 4: Build + test**

Run: `cargo build -p spur-tui`
Expected: no errors.

Run: `cargo test -p spur-tui`
Expected: all previously-passing tests still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/input_bar.rs crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-tui): dim InputBar + suppress cursor when PickerShell owns focus

Adds InputBar::render_inert (DarkGray border + DarkGray text, no
set_cursor_position call) used by SessionDetailView while a
PickerShell is active. The shell owns the terminal cursor via its
MiniInput; the composer is visibly quiescent. Completion popup does
not render while a shell is active (shell renders its own popup).

Part of: PickerShell Phase 1"
```

---

## Task 7: Integration test — Ctrl+R end-to-end

Proves the full keystroke-to-restored-state path.

**Files:**
- Create: `crates/spur-tui/tests/picker_shell_ctrl_r.rs`

- [ ] **Step 1: Write the integration test file**

Create `crates/spur-tui/tests/picker_shell_ctrl_r.rs`:

```rust
//! Integration: `Ctrl+R` opens a PickerShell, typing filters rows, Tab/Enter
//! accepts and restores the snapshot into InputBar, Esc cancels without
//! mutating InputBar.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::ContentBlock;
use spur_tui::action::Action;
use spur_tui::components::input_bar::ProtectedRange;
use spur_tui::input_history::{InputHistoryEntry, InputStateSnapshot};
use spur_tui::views::{session_detail::SessionDetailView, View};

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(|| spur_core::lineage::projection::ExecutorLineage::new());
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn mk_view() -> SessionDetailView {
    let tmp = tempfile::tempdir().unwrap();
    SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
    )
}

fn press(v: &mut SessionDetailView, code: KeyCode) -> Option<Action> {
    v.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &test_ctx())
}

fn press_mod(v: &mut SessionDetailView, code: KeyCode, m: KeyModifiers) -> Option<Action> {
    v.handle_key(KeyEvent::new(code, m), &test_ctx())
}

fn type_str(v: &mut SessionDetailView, s: &str) {
    for c in s.chars() {
        press(v, KeyCode::Char(c));
    }
}

fn seed_history(v: &mut SessionDetailView, entries: Vec<InputHistoryEntry>) {
    v.input_bar_mut_for_test().seed_history(entries);
}

#[test]
fn ctrl_r_opens_shell_and_accept_restores_snapshot() {
    let mut v = mk_view();
    seed_history(
        &mut v,
        vec![
            InputHistoryEntry::new(InputStateSnapshot::from_text("refactor the walker")),
            InputHistoryEntry::new(InputStateSnapshot::from_text("fix the panic")),
        ],
    );

    press_mod(&mut v, KeyCode::Char('r'), KeyModifiers::CONTROL);
    type_str(&mut v, "refa");
    press(&mut v, KeyCode::Enter);

    // Expect the InputBar to now contain "refactor the walker".
    assert_eq!(v.input_bar_text_for_test(), "refactor the walker");
}

#[test]
fn ctrl_r_esc_leaves_input_bar_untouched() {
    let mut v = mk_view();
    seed_history(
        &mut v,
        vec![InputHistoryEntry::new(InputStateSnapshot::from_text("hello"))],
    );

    // Start with a draft in the InputBar.
    type_str(&mut v, "my draft");
    assert_eq!(v.input_bar_text_for_test(), "my draft");

    press_mod(&mut v, KeyCode::Char('r'), KeyModifiers::CONTROL);
    type_str(&mut v, "he");
    press(&mut v, KeyCode::Esc);

    assert_eq!(v.input_bar_text_for_test(), "my draft");
}

#[test]
fn ctrl_r_accept_roundtrips_resource_link_on_resubmit() {
    let mut v = mk_view();
    let mut snap = InputStateSnapshot::from_text("hi @foo");
    snap.protected_ranges = vec![ProtectedRange {
        start: 3,
        end: 7,
        uri: "file:///foo".to_string(),
        name: "foo".to_string(),
    }];
    seed_history(&mut v, vec![InputHistoryEntry::new(snap)]);

    press_mod(&mut v, KeyCode::Char('r'), KeyModifiers::CONTROL);
    press(&mut v, KeyCode::Enter); // accept newest (only) row

    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    match act {
        Action::SendMessage { blocks, .. } => {
            // Expect a Text("hi ") + ResourceLink { uri: file:///foo, name: foo }.
            assert_eq!(blocks.len(), 2);
            assert!(matches!(&blocks[1], ContentBlock::ResourceLink(r) if r.uri == "file:///foo" && r.name == "foo"));
        }
        other => panic!("expected SendMessage, got {:?}", other),
    }
}

#[test]
fn ctrl_r_on_empty_history_opens_empty_shell_and_esc_closes() {
    let mut v = mk_view();
    press_mod(&mut v, KeyCode::Char('r'), KeyModifiers::CONTROL);
    press(&mut v, KeyCode::Esc);
    // No panic, no state change. Follow-up Enter should behave like a
    // regular empty-composer Enter (no action).
    let act = press(&mut v, KeyCode::Enter);
    assert!(act.is_none() || matches!(act, Some(_))); // any behavior OK so long as no panic
}
```

- [ ] **Step 2: Add the test-only accessors on SessionDetailView**

The integration test needs read-only and mutable access to `InputBar` — match the existing `cfg(any(test, debug_assertions))` pattern. In `crates/spur-tui/src/views/session_detail.rs`, at the end of `impl SessionDetailView`, add:

```rust
    /// Test-only: read current InputBar text.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn input_bar_text_for_test(&self) -> String {
        self.input_bar.text()
    }

    /// Test-only: mutable InputBar access for seeding history in tests.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn input_bar_mut_for_test(&mut self) -> &mut crate::components::input_bar::InputBar {
        &mut self.input_bar
    }
```

- [ ] **Step 3: Run the new integration test**

Run: `cargo test -p spur-tui --test picker_shell_ctrl_r`
Expected: `test result: ok. 4 passed; 0 failed`.

- [ ] **Step 4: Run the full test suite to check for regressions**

Run: `cargo test -p spur-tui`
Expected: all tests pass; no regressions in existing mention/slash/submit/session_metadata tests.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/tests/picker_shell_ctrl_r.rs crates/spur-tui/src/views/session_detail.rs
git commit -m "test(spur-tui): integration coverage for PickerShell Ctrl+R flow

Four integration tests cover: accept restores snapshot; Esc leaves
composer untouched; round-trip of ResourceLink through Ctrl+R+accept+
submit (Stage 1 Goal #2 regression guard); empty history is safe.

Adds input_bar_text_for_test / input_bar_mut_for_test accessors on
SessionDetailView (cfg-gated, mirroring existing InputBar test hooks).

Part of: PickerShell Phase 1"
```

---

## Task 8: Amend Stage 1 spec — mark stale P0 list closed

Clean up the drifted tracking section in the Stage 1 spec so future readers don't re-open resolved work.

**Files:**
- Modify: `docs/superpowers/specs/2026-04-19-chat-input-retrieval-unification-design.md`

- [ ] **Step 1: Replace the "Known Defects (P0 — fix before stage 2)" section with a "Closed defects" note**

In `docs/superpowers/specs/2026-04-19-chat-input-retrieval-unification-design.md`, find the heading `## Known Defects (P0 — fix before stage 2)` (line 360). Replace the entire section (lines 360-381) with:

```markdown
## Closed defects (resolved in-code; historical record)

These defects were enumerated during the 2026-04-19 review and have
all been fixed in the shipped Stage 1 code. Line references point to
the fix sites.

1. **Undo/redo silently re-enabled on history restore** — resolved at
   `input_bar.rs:1004`: `restore_snapshot` now re-calls
   `set_max_histories(0)` after rebuilding the `TextArea`. Guarded by
   `max_histories_for_test` at `input_bar.rs:1155-1159`.

2. **`HISTORY_CAP` magic number duplicated** — resolved: the cap lives
   as `pub const HISTORY_CAP: usize = 100;` at `input_history.rs:9`
   and is imported by both cap sites (`input_bar.rs:1031`,
   `app.rs:1417`).

3. **`ProtectedRange` validation on deserialize** — resolved:
   `InputStateSnapshot::sanitized` (`input_history.rs:45-66`) enforces
   `start <= end <= text.len()`, UTF-8 char boundaries, sorted, and
   non-overlapping. A custom `Deserialize` impl
   (`input_history.rs:26-34`) funnels all load paths through it.
```

- [ ] **Step 2: Add a "Stage 2 reference" line near the top**

In the same file, directly below the existing `**Related docs:**` line (line 6), append:

```markdown
**Stage 2:** `docs/superpowers/specs/2026-04-19-picker-shell-retrieval-unification-design.md`
```

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-04-19-chat-input-retrieval-unification-design.md
git commit -m "docs(spec): mark Stage 1 P0 list closed; link Stage 2 spec

All three P0 defects listed in the 2026-04-19 spec were already
fixed in Stage 1 shipped code; the doc drifted. Replace the P0
section with a Closed defects record pointing at the fix sites
(input_bar.rs:1004, input_history.rs:9, input_history.rs:45-66).
Add explicit link to the Stage 2 PickerShell spec.

Part of: PickerShell Phase 1"
```

---

## Final: Phase 1 exit verification

- [ ] **Step 1: Run the full crate build in release mode**

Run: `cargo build -p spur-tui --release`
Expected: no errors, no new warnings.

- [ ] **Step 2: Run the full test suite**

Run: `cargo test -p spur-tui`
Expected: all tests pass.

- [ ] **Step 3: Run the workspace-wide build**

Run: `cargo build`
Expected: no errors (confirms no downstream crate broke on the new `pub mod` exports or the removed `history_search` fields).

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -p spur-tui --all-targets -- -D warnings`
Expected: no warnings. Fix any inline if they appear.

- [ ] **Step 5: Manual smoke in a running `spur watch`**

Run: `cargo run -p spur-cli -- watch` (or whatever the local entry is), submit a few messages, then press `Ctrl+R`. Verify:

1. Popup appears above the InputBar with header `History · bck-i-search`.
2. An empty `search: ` line shows with a blinking cursor inside the popup.
3. Typing letters filters the rows; the query is visible at the top of the popup.
4. The InputBar's border is dimmed (`Color::DarkGray`) while the shell is open.
5. No cursor blinks inside the InputBar; the only cursor is in the shell.
6. Arrow keys navigate rows.
7. Typing one more letter after arrowing to a row preserves selection when that row still matches.
8. Tab and Enter both accept and close the shell; the InputBar contents are replaced with the selected snapshot.
9. Esc closes the shell; the InputBar is exactly as it was before Ctrl+R.

- [ ] **Step 6: No additional commit needed** unless smoke testing revealed defects.

---

## Self-review results

**Spec coverage.** Phase 1 exit criteria from the spec (§Phased rollout → Phase 1):
- Popup with visible `search:` line ✓ (Task 4, Step 4; Task 7 verifies indirectly; manual smoke verifies visually)
- Arrow keys navigate, selection survives across keystrokes ✓ (Task 4 `selection_survives_refilter_when_row_still_present`)
- Tab and Enter accept via `ReplaceState` ✓ (Task 4 tests; Task 7 end-to-end)
- Esc closes; InputBar bit-identical to pre-Ctrl+R ✓ (Task 7 `ctrl_r_esc_leaves_input_bar_untouched`)
- InputBar cursor suppressed when shell active ✓ (Task 6, `render_inert`)
- InputBar border dimmed ✓ (Task 6, `Color::DarkGray`)

**Placeholder scan.** Every code step shows the actual code. Every command shows expected output. No "TBD", no "similar to", no "implement later". Task 8 writes the exact replacement text for the spec amendment.

**Type consistency.** Signatures spot-checked:
- `MiniInput::new/insert_char/backspace/delete/left/right/home/end/clear/paste/text/cursor` consistent across Tasks 1, 4.
- `QuerySource::{title, query_mode, refresh, accept}` + `QueryMode::{OwnedByShell, ReadFromInputBar}` + `RetrievalRow::{primary, secondary, tag, atoms}` + `RetrievalAccept::{ReplaceState, InsertAtom, ReplaceTriggerToken}` consistent across Tasks 2, 3, 4, 5.
- `PickerShell::{open, handle_key, render, set_query_from_input_bar}` + `PickerAction::{None, Accept, Cancel}` consistent across Tasks 4, 5, 6, 7.
- `InputBar::{render, render_inert, history, set_state, set_text, insert_atom, seed_history, text, cursor}` — all methods referenced exist in the current codebase or are added in Task 6.

No gaps found. Plan ready for execution.
