# Chat-Input Retrieval — P0 Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the three P0 fixes from the 2026-04-19 review of `docs/superpowers/specs/2026-04-19-chat-input-retrieval-unification-design.md` so stage 2 (shared retrieval) starts from a corrected base.

**Architecture:** Three independent, small fixes to the existing `spur-tui` input-history pipeline. Each is a TDD cycle (failing test → fix → green → commit). No new modules; all changes live inside `crates/spur-tui/src/{input_history.rs, components/input_bar.rs, session_metadata.rs}` plus matching test files under `crates/spur-tui/tests/`.

**Tech Stack:** Rust 2021, `tui-textarea`, `serde`, `serde_json`, `cargo test`.

---

## Spec Correction (precondition)

The spec lists Defect #2 as **"persisted `metadata.input_history` is unbounded"**. That is overstated. `merge_input_history_entry` at `crates/spur-tui/src/app.rs:1417` already caps at `100` on every push, and every writer to `metadata.input_history` (the `push_input_history_entry` path and the ACP replay backfill loop at `app.rs:566-577`) goes through `merge_input_history_entry`. Persisted growth IS bounded.

The real issue is a **duplicated magic number**: `HISTORY_CAP = 100` lives at `crates/spur-tui/src/components/input_bar.rs:44` and is hardcoded again as `100` at `app.rs:1417`. If one drifts, the in-memory and on-disk caps disagree. This is a code-hygiene defect (drift risk), not a correctness defect.

**Task 1** addresses that hygiene problem and **also updates the spec** to reflect the corrected understanding.

---

## File Map

- **Modify:** `crates/spur-tui/src/input_history.rs` — add `HISTORY_CAP` const, add `validate_and_clamp` for `InputStateSnapshot`, hook deserialize.
- **Modify:** `crates/spur-tui/src/components/input_bar.rs` — replace local `HISTORY_CAP` with re-export, fix `restore_snapshot` to re-disable undo.
- **Modify:** `crates/spur-tui/src/app.rs` — replace hardcoded `100` with `HISTORY_CAP`.
- **Modify:** `docs/superpowers/specs/2026-04-19-chat-input-retrieval-unification-design.md` — correct Defect #2 wording.
- **Create / extend:** `crates/spur-tui/tests/input_bar_editing.rs` — add undo-disabled-after-restore test.
- **Create:** `crates/spur-tui/tests/input_history_validation.rs` — add range-validation deserialize tests.

---

## Task 1: Lift `HISTORY_CAP` to shared const + correct spec

**Files:**
- Modify: `crates/spur-tui/src/input_history.rs` (add public const)
- Modify: `crates/spur-tui/src/components/input_bar.rs:44` (re-export instead of redefine)
- Modify: `crates/spur-tui/src/app.rs:1417` (use the const)
- Modify: `docs/superpowers/specs/2026-04-19-chat-input-retrieval-unification-design.md` (correct Defect #2)
- Test: `crates/spur-tui/tests/input_bar_editing.rs` (assert cap is single-sourced)

- [ ] **Step 1: Write the failing test** — add to bottom of `crates/spur-tui/tests/input_bar_editing.rs`

```rust
#[test]
fn history_cap_is_single_sourced() {
    // Compile-time guard: the cap visible to callers comes from input_history,
    // not a private duplicate inside InputBar.
    assert_eq!(spur_tui::input_history::HISTORY_CAP, 100);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --test input_bar_editing history_cap_is_single_sourced`
Expected: FAIL with `cannot find value HISTORY_CAP in module input_history` (compile error).

- [ ] **Step 3: Add the public const**

Edit `crates/spur-tui/src/input_history.rs`. After the `use` block at the top of the file (after line 4), insert:

```rust
/// Maximum number of submitted-input entries retained in both the in-memory
/// `InputBar` ring buffer and the persisted `SessionMetadata::input_history`
/// vector. Single source of truth — do not redefine this number elsewhere.
pub const HISTORY_CAP: usize = 100;
```

- [ ] **Step 4: Replace the duplicate in `input_bar.rs`**

Edit `crates/spur-tui/src/components/input_bar.rs`. Replace line 44:

```rust
const HISTORY_CAP: usize = 100;
```

with:

```rust
use crate::input_history::HISTORY_CAP;
```

(If a `use crate::input_history::{...}` line already exists at the top of the file, merge `HISTORY_CAP` into the existing brace list at line 12 instead of adding a second `use`.)

- [ ] **Step 5: Replace the hardcoded `100` in `app.rs`**

Edit `crates/spur-tui/src/app.rs`. At the top of the file, ensure the import line that pulls `InputHistoryEntry` from `input_history` (currently `use crate::input_history::InputHistoryEntry;` at line 23) also pulls `HISTORY_CAP`:

```rust
use crate::input_history::{HISTORY_CAP, InputHistoryEntry};
```

Then change `app.rs:1417`:

```rust
        if hist.len() > 100 {
            hist.remove(0);
        }
```

to:

```rust
        if hist.len() > HISTORY_CAP {
            hist.remove(0);
        }
```

- [ ] **Step 6: Run the new test + the full crate test suite**

Run: `cargo test -p spur-tui --test input_bar_editing history_cap_is_single_sourced`
Expected: PASS.

Run: `cargo test -p spur-tui`
Expected: all existing tests still pass.

- [ ] **Step 7: Update the spec to reflect reality**

Edit `docs/superpowers/specs/2026-04-19-chat-input-retrieval-unification-design.md`.

In the "Known Defects (P0 — fix before stage 2)" section, replace defect #2 in full with:

```markdown
2. **`HISTORY_CAP` magic number was duplicated.** The cap lived as
   `HISTORY_CAP = 100` at `input_bar.rs:44` and as a hardcoded `100` at
   `app.rs:1417`. The two could drift independently, causing the in-memory
   and persisted caps to disagree. The 2026-04-19 review's earlier framing
   ("persisted history is unbounded") was inaccurate — every writer to
   `metadata.input_history` already routed through `merge_input_history_entry`
   which truncates on every push. Fix: lift `HISTORY_CAP` to
   `crate::input_history` and reference it from both sites.
```

In the "Risks" table, replace the row beginning `| Metadata size grows because history now stores...` with:

```markdown
| Metadata size grows because history now stores `ProtectedRange`s and provenance | Capped at `HISTORY_CAP` (single-sourced from `input_history.rs`) at every push site. Earlier review wording suggesting the persisted layer was unbounded was incorrect; corrected after grounding `app.rs:1417`. |
```

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/src/input_history.rs \
        crates/spur-tui/src/components/input_bar.rs \
        crates/spur-tui/src/app.rs \
        crates/spur-tui/tests/input_bar_editing.rs \
        docs/superpowers/specs/2026-04-19-chat-input-retrieval-unification-design.md
git commit -m "$(cat <<'EOF'
refactor(spur-tui): single-source HISTORY_CAP and correct spec

Lift HISTORY_CAP from components/input_bar.rs into input_history.rs as
pub const so app.rs's merge_input_history_entry uses the same number.
Eliminates the magic-100 duplicate at app.rs:1417.

Also corrects spec defect #2 — earlier review claimed persisted history
was unbounded, but every writer routes through merge_input_history_entry
which already truncates per push. The actual defect is the duplicated
magic number, now fixed.
EOF
)"
```

---

## Task 2: Defect #1 — `restore_snapshot` re-disables undo/redo

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs:1001-1013` (`restore_snapshot`)
- Test: `crates/spur-tui/tests/input_bar_editing.rs` (new test)

**Background:** `InputBar::new()` at `input_bar.rs:82` calls `set_max_histories(0)` "to prevent protected range desync". `restore_snapshot` rebuilds the underlying `TextArea` from scratch (line 1005) and never re-disables undo. After the first `Ctrl+P` / `Ctrl+R` accept, undo/redo is silently on, violating the documented invariant.

- [ ] **Step 1: Write the failing test** — add to bottom of `crates/spur-tui/tests/input_bar_editing.rs`

```rust
#[test]
fn restore_snapshot_keeps_undo_disabled() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut b = InputBar::new();
    type_str(&mut b, "first prompt");
    submit(&mut b);
    type_str(&mut b, "second prompt");
    submit(&mut b);

    // Browse history backward — this triggers restore_snapshot.
    b.history_prev();
    assert_eq!(b.text(), "second prompt");

    // After restore, send Ctrl+Z. With max_histories(0) preserved, undo
    // is a no-op and the buffer stays put. If undo got re-enabled by the
    // restore, the textarea will undo the snapshot load and clear/regress.
    b.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(
        b.text(),
        "second prompt",
        "undo must remain disabled across restore_snapshot to keep \
         protected ranges in sync"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --test input_bar_editing restore_snapshot_keeps_undo_disabled`
Expected: FAIL — buffer text changes / clears because Ctrl+Z is now an active undo after the restore-snapshot rebuild.

(If Ctrl+Z is intercepted before reaching the textarea, the test will instead pass trivially. In that case, swap to a more direct probe — see Step 2b.)

- [ ] **Step 2b: (only if 2 passes spuriously) probe via TextArea directly**

If `tui-textarea`'s undo isn't reachable through `handle_key` in this codebase, replace the assertion with a direct check by exposing a test-only accessor. Add to `input_bar.rs` near `last_inner_width_for_test`:

```rust
    /// Test-only: read the textarea's currently-configured max history.
    #[cfg(test)]
    #[doc(hidden)]
    pub fn max_histories_for_test(&self) -> usize {
        // tui_textarea exposes this via the configured value; mirror what
        // we passed to set_max_histories.
        self.textarea.max_histories()
    }
```

…and rewrite the test body's final assertion to:

```rust
    assert_eq!(b.max_histories_for_test(), 0);
```

If `tui_textarea::TextArea::max_histories` doesn't exist as a getter, fall back to performing one in-place edit then a Ctrl+Z and asserting the edit was NOT undone. Pick whichever probe actually fails today against `restore_snapshot`'s missing call.

- [ ] **Step 3: Implement the fix**

Edit `crates/spur-tui/src/components/input_bar.rs`. In `restore_snapshot` (currently lines 1001-1013), after the `self.textarea = TextArea::new(lines);` line and before `self.textarea.set_cursor_line_style(Style::default());`, insert:

```rust
        self.textarea.set_max_histories(0);
```

Final body of `restore_snapshot` should read:

```rust
    fn restore_snapshot(&mut self, snapshot: &InputStateSnapshot, cursor: usize) {
        let mode = self.mode;
        let last_w = self.last_inner_width.get();
        let lines: Vec<String> = snapshot.text.split('\n').map(|s| s.to_string()).collect();
        self.textarea = TextArea::new(lines);
        self.textarea.set_max_histories(0);
        self.textarea.set_cursor_line_style(Style::default());
        self.rebuild_line_cache();
        self.move_cursor_to_byte(cursor.min(snapshot.text.len()));
        self.protected_ranges = snapshot.protected_ranges.clone();
        self.last_inner_width.set(last_w);
        self.goal_vcol = None;
        self.set_mode(mode);
    }
```

- [ ] **Step 4: Also fix the analogous gap in `clear()`**

`clear()` at `input_bar.rs:1111-1121` has the same pattern (rebuilds `TextArea::default()` without re-disabling histories). Add the same line right after the `TextArea::default()` assignment:

```rust
    pub fn clear(&mut self) {
        let mode = self.mode;
        let last_w = self.last_inner_width.get();
        self.textarea = TextArea::default();
        self.textarea.set_max_histories(0);
        self.textarea.set_cursor_line_style(Style::default());
        self.line_cache = vec![0];
        self.protected_ranges.clear();
        self.last_inner_width.set(last_w);
        self.goal_vcol = None;
        self.set_mode(mode);
    }
```

- [ ] **Step 5: Run the new test + full suite**

Run: `cargo test -p spur-tui --test input_bar_editing restore_snapshot_keeps_undo_disabled`
Expected: PASS.

Run: `cargo test -p spur-tui`
Expected: all existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/input_bar.rs \
        crates/spur-tui/tests/input_bar_editing.rs
git commit -m "$(cat <<'EOF'
fix(spur-tui): InputBar::restore_snapshot must re-disable undo

InputBar::new() calls set_max_histories(0) to prevent protected-range
desync from undo/redo. restore_snapshot (used by Ctrl+P / Ctrl+R / set_state)
rebuilt the TextArea from scratch and dropped that setting, silently
re-enabling undo on the first history navigation. Same gap existed in
clear(). Re-apply set_max_histories(0) after every textarea rebuild.

Closes Defect #1 from 2026-04-19 chat-input retrieval review.
EOF
)"
```

---

## Task 3: Range-validation contract on `InputStateSnapshot` deserialize

**Files:**
- Modify: `crates/spur-tui/src/input_history.rs` (add validation, hook custom deserialize)
- Create: `crates/spur-tui/tests/input_history_validation.rs` (new test file)

**Background:** `ProtectedRange` byte indices are trusted raw on deserialize. A corrupted or hand-edited `.spur/session_metadata.json` can yield ranges with `start > end`, `end > text.len()`, overlapping ranges, or non-`char_boundary` indices, which then drive `restore_snapshot` → `move_cursor_to_byte` and downstream `range_at_cursor` arithmetic. The codebase already enforces char-boundary discipline elsewhere (`session_detail.rs:670`); the same defense-in-depth applies here.

Decision: **drop invalid ranges on deserialize**, do not panic. Keep the snapshot's text intact (text-only recall is preferable to losing the entry entirely).

- [ ] **Step 1: Create the test file**

Create `crates/spur-tui/tests/input_history_validation.rs`:

```rust
//! Defense-in-depth: a corrupted or hand-edited session_metadata.json
//! must not produce InputStateSnapshots with invalid ProtectedRanges.
//! Invalid ranges are dropped on load; the text payload is preserved.

use spur_tui::components::input_bar::ProtectedRange;
use spur_tui::input_history::InputStateSnapshot;

fn snapshot_json(text: &str, ranges_json: &str) -> String {
    format!(
        r#"{{"text": {text:?}, "protected_ranges": {ranges_json}}}"#,
        text = text
    )
}

#[test]
fn deserialize_accepts_well_formed_snapshot() {
    let json = snapshot_json(
        "hello @foo",
        r#"[{"start": 6, "end": 10, "uri": "file:///foo", "name": "foo"}]"#,
    );
    let snap: InputStateSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(snap.text, "hello @foo");
    assert_eq!(snap.protected_ranges.len(), 1);
}

#[test]
fn deserialize_drops_range_with_end_past_text_len() {
    let json = snapshot_json(
        "short",
        r#"[{"start": 0, "end": 999, "uri": "file:///x", "name": "x"}]"#,
    );
    let snap: InputStateSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(snap.text, "short");
    assert!(snap.protected_ranges.is_empty(),
        "invalid range must be dropped, not preserved");
}

#[test]
fn deserialize_drops_range_with_start_after_end() {
    let json = snapshot_json(
        "hello world",
        r#"[{"start": 5, "end": 2, "uri": "file:///x", "name": "x"}]"#,
    );
    let snap: InputStateSnapshot = serde_json::from_str(&json).unwrap();
    assert!(snap.protected_ranges.is_empty());
}

#[test]
fn deserialize_drops_range_off_char_boundary() {
    // "héllo" — 'é' is two bytes (U+00E9 = 0xC3 0xA9) starting at index 1.
    // Index 2 is mid-codepoint and must be rejected.
    let json = snapshot_json(
        "héllo",
        r#"[{"start": 1, "end": 2, "uri": "file:///x", "name": "x"}]"#,
    );
    let snap: InputStateSnapshot = serde_json::from_str(&json).unwrap();
    assert!(snap.protected_ranges.is_empty(),
        "non-char-boundary range must be dropped");
}

#[test]
fn deserialize_drops_overlapping_ranges_keeping_first() {
    let json = snapshot_json(
        "aaaaaaaa",
        r#"[
            {"start": 0, "end": 4, "uri": "file:///a", "name": "a"},
            {"start": 2, "end": 6, "uri": "file:///b", "name": "b"}
        ]"#,
    );
    let snap: InputStateSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(snap.protected_ranges.len(), 1);
    assert_eq!(snap.protected_ranges[0].name, "a");
}

#[test]
fn deserialize_sorts_ranges_by_start() {
    let json = snapshot_json(
        "aaaaaaaaaa",
        r#"[
            {"start": 6, "end": 8, "uri": "file:///b", "name": "b"},
            {"start": 0, "end": 2, "uri": "file:///a", "name": "a"}
        ]"#,
    );
    let snap: InputStateSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(snap.protected_ranges.len(), 2);
    assert_eq!(snap.protected_ranges[0].name, "a");
    assert_eq!(snap.protected_ranges[1].name, "b");
}

#[test]
fn deserialize_preserves_text_when_all_ranges_invalid() {
    let json = snapshot_json(
        "@foo bar",
        r#"[{"start": 99, "end": 999, "uri": "file:///x", "name": "x"}]"#,
    );
    let snap: InputStateSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(snap.text, "@foo bar");
    assert!(snap.protected_ranges.is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui --test input_history_validation`
Expected: most tests FAIL — invalid ranges currently round-trip as-is because `InputStateSnapshot` derives `Deserialize` directly with no validation.

- [ ] **Step 3: Implement validation in `input_history.rs`**

Edit `crates/spur-tui/src/input_history.rs`. Replace the existing `InputStateSnapshot` struct definition (currently lines 7-13) and its derive:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct InputStateSnapshot {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub protected_ranges: Vec<ProtectedRange>,
}
```

with a custom-deserialize variant:

```rust
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct InputStateSnapshot {
    pub text: String,
    pub protected_ranges: Vec<ProtectedRange>,
}

#[derive(Deserialize)]
struct RawInputStateSnapshot {
    #[serde(default)]
    text: String,
    #[serde(default)]
    protected_ranges: Vec<ProtectedRange>,
}

impl<'de> Deserialize<'de> for InputStateSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawInputStateSnapshot::deserialize(deserializer)?;
        Ok(Self::sanitized(raw.text, raw.protected_ranges))
    }
}
```

Then add the `sanitized` constructor as an `impl InputStateSnapshot` method, placed next to `from_blocks`:

```rust
    /// Build a snapshot, dropping any `ProtectedRange` that is out of
    /// bounds, off a UTF-8 char boundary, has `start > end`, or overlaps
    /// an earlier kept range. Keeps the text intact. Defense-in-depth for
    /// hand-edited or corrupted persisted history.
    fn sanitized(text: String, ranges: Vec<ProtectedRange>) -> Self {
        let mut sorted = ranges;
        sorted.sort_by_key(|r| r.start);

        let mut kept: Vec<ProtectedRange> = Vec::with_capacity(sorted.len());
        let mut last_end: usize = 0;
        for r in sorted {
            let in_bounds = r.start <= r.end && r.end <= text.len();
            let on_boundaries =
                text.is_char_boundary(r.start) && text.is_char_boundary(r.end);
            let non_overlapping = r.start >= last_end;
            if in_bounds && on_boundaries && non_overlapping {
                last_end = r.end;
                kept.push(r);
            }
        }

        Self {
            text,
            protected_ranges: kept,
        }
    }
```

Also update the existing `new` and `from_blocks` constructors to route through `sanitized` so the same discipline applies to in-process construction:

Replace `new`:

```rust
    pub fn new(text: String, protected_ranges: Vec<ProtectedRange>) -> Self {
        Self::sanitized(text, protected_ranges)
    }
```

`from_text` and `from_blocks` already build via `Self`/`new` and naturally route through `sanitized`; verify by reading the file after editing.

- [ ] **Step 4: Add `use serde::Deserialize` if the existing import only has `Serialize, Deserialize` macro derive**

The current `use serde::{Deserialize, Serialize};` at line 1 is sufficient. Confirm no extra imports needed.

- [ ] **Step 5: Run the validation tests**

Run: `cargo test -p spur-tui --test input_history_validation`
Expected: all 7 tests PASS.

- [ ] **Step 6: Run the full crate test suite to catch regressions**

Run: `cargo test -p spur-tui`
Expected: all existing tests pass (sanitization is conservative — well-formed inputs are unchanged).

If `from_blocks`-built snapshots ever produce overlapping or unsorted ranges in tests, fix the test, not the sanitizer — `from_blocks` builds ranges in left-to-right text order and they cannot legitimately overlap.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/input_history.rs \
        crates/spur-tui/tests/input_history_validation.rs
git commit -m "$(cat <<'EOF'
fix(spur-tui): validate ProtectedRanges on InputStateSnapshot deserialize

Hand-edited or corrupted .spur/session_metadata.json could produce
ranges with end > text.len(), start > end, off-char-boundary indices,
or overlap. These then drove restore_snapshot and downstream
range_at_cursor arithmetic into invalid territory.

Add a sanitized() constructor that drops invalid ranges, keeps the
text intact, and route both Deserialize and InputStateSnapshot::new
through it. Defense-in-depth contract documented in the spec.

Closes the range-validation P0 item from 2026-04-19 chat-input
retrieval review.
EOF
)"
```

---

## Self-Review Notes

**Spec coverage:**
- Defect #1 (undo re-enabled) → Task 2.
- Defect #2 (HISTORY_CAP duplication, corrected from "unbounded") → Task 1, also amends spec text.
- Range-validation contract (spec data-model section) → Task 3.

**Type / API consistency:**
- `HISTORY_CAP` is referenced from `input_history.rs` in three places (definition, `input_bar.rs` import, `app.rs` import) — all spelled identically.
- `InputStateSnapshot::sanitized` is private; `new` becomes the public sanitizing constructor. `from_text` and `from_blocks` are unchanged callers.

**Out of scope (deferred to later P1/P2 work, per spec Next Steps):**
- Replace `#[serde(flatten)]` + untagged with explicit `v` discriminator.
- Wire `submitted_at` / `session_id` provenance into popup row UI.
- Reuse one `nucleo::Matcher` across keystrokes.
- Multi-process safety / event-sourced history log.

These are intentionally not in this plan; they are P1 / longer-term items in the spec's restructured Next Steps.
