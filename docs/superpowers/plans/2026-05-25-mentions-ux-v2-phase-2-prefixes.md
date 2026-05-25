# @-Mention Phase 2 — Optional Disambiguator Prefixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add optional `@/`, `@#`, `@:` prefix detection that hard-filters the mention picker to Files, Issues, and Code symbols respectively, while bare `@foo` keeps its unified fuzzy behavior.

**Architecture:** Extend `TriggerDetector` to track an `Option<MentionKind>` filter on Mention triggers. The filter is set when the user types one of three prefix chars *immediately after* `@`; the filter char goes into the buffer but is stripped from the reported `query`. Backspacing the filter char reverts to unified mode without closing the picker. The filter threads through `Trigger` → `MentionQuerySource` → `MentionRegistry::query`, which hard-filters entries by kind and emits a single section header that names the active filter.

**Tech Stack:** Rust, `spur-tui` crate (binary terminal UI). Tests via `cargo test -p spur-tui`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-05-13-mentions-ux-v2-design.md` §3.2, §4, §4.1.
**Issue:** `bd-1id`.

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `crates/spur-tui/src/components/completion_trigger.rs` | modify | Add `kind_filter: Option<MentionKind>` to `Trigger`; teach `advance_composing` to consume a prefix char immediately after `@` and strip it from `query`; revert to unified on backspace. |
| `crates/spur-tui/src/mentions/registry.rs` | modify | Add `kind_filter: Option<MentionKind>` parameter to `MentionRegistry::query`; hard-filter the candidate set; emit a single filtered section header (`── Files (filter: @/) ──` etc). |
| `crates/spur-tui/src/components/query_source.rs` | modify | Store `kind_filter` in `MentionQuerySource`; pass it to `registry.query()`; expose a setter so the port can update it without recreating the shell. |
| `crates/spur-tui/src/components/input_completion.rs` | modify | On `Open` transitions, pass `trigger.kind_filter` into `MentionQuerySource::new`. On `Update` transitions, also push the latest filter (read from `TriggerDetector`) into the existing `MentionQuerySource` so backspacing the filter char swaps modes without flicker. |
| `crates/spur-tui/tests/mentions_v2_picker.rs` | modify (append tests) | Integration tests: `@/`, `@#`, `@:` route to filtered registry queries; backspace reverts. |

`crates/spur-tui/src/mentions/mod.rs` already re-exports `MentionKind`, so no public-surface bump beyond `Trigger`.

---

## Conventions assumed (from `completion_trigger.rs` review)

- `@` opens a Mention trigger only at byte 0 or after whitespace (unchanged).
- The trigger char (`@`) lives at `prefix_start` in the buffer; query slice is `text[prefix_start+1 .. cursor]` (clamped at first whitespace).
- Detector receives `IntentEvent` from the input dispatch site; backspace arrives as `DeletedChar`; printable keys arrive as `TypedChar(c)`.
- The first `Open` transition uses `kind_filter: None`. The filter is set on the *immediately following* `TypedChar('/'|'#'|':')` when and only when the current query is empty. This matches the spec wording "next typed char".

---

## Task 1 — Add `kind_filter` field to `Trigger` and detector state

**Files:**
- Modify: `crates/spur-tui/src/components/completion_trigger.rs`

- [ ] **Step 1: Add field to `Trigger` and `TriggerState::Composing`**

In `crates/spur-tui/src/components/completion_trigger.rs`, change the `Trigger` struct (currently at lines 14–22):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub kind: TriggerKind,
    pub prefix_start: usize,
    pub query: String,
    /// Active kind filter for `Mention` triggers. `None` for unified mode,
    /// `Some(File)` when the user typed `@/`, `Some(Issue)` for `@#`,
    /// `Some(CodeSymbol)` for `@:`. Always `None` for Slash and SlashArg.
    pub kind_filter: Option<crate::mentions::MentionKind>,
}
```

Change `TriggerState::Composing` (currently at lines 64–72):

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum TriggerState {
    #[default]
    Idle,
    Composing {
        kind: TriggerKindInternal,
        prefix_start: usize,
        kind_filter: Option<crate::mentions::MentionKind>,
    },
}
```

Fix every existing `TriggerState::Composing { ... }` constructor and match-pattern in the file to include `kind_filter`. The `maybe_open` cases (lines 274–306) construct with `kind_filter: None`. All match patterns elsewhere can use `kind_filter: _` to ignore the new field. The `current_prefix_start` accessor (lines 128–133) match pattern needs the new field added.

Also, in the `Open` `Trigger { ... }` literals at lines 278–283 and 300–306, add `kind_filter: None`.

- [ ] **Step 2: Add `current_kind_filter()` accessor**

After `current_prefix_start()`, add:

```rust
/// The active kind filter (`Mention` triggers only). `None` while Idle
/// or in unified Mention mode. Updated as the user types/backspaces the
/// filter char.
pub fn current_kind_filter(&self) -> Option<crate::mentions::MentionKind> {
    match &self.state {
        TriggerState::Composing { kind_filter, .. } => kind_filter.clone(),
        TriggerState::Idle => None,
    }
}
```

- [ ] **Step 3: Update defensive re-check block**

The existing block at lines 161–179 pattern-matches `TriggerState::Composing { kind, prefix_start }`. Add `kind_filter: _` to the pattern so it compiles. No behavioral change.

- [ ] **Step 4: Compile-check**

Run: `cargo check -p spur-tui`
Expected: no errors. Existing tests may have stale struct literals — fix any compile errors by adding `kind_filter: None` to `Trigger { ... }` and `kind_filter: None` (or `kind_filter: _` in patterns) to every `TriggerState::Composing { ... }` literal in the same file. The internal `#[cfg(test)]` block has one white-box construction at lines 845–850; add `kind_filter: None`.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/completion_trigger.rs
git commit -m "feat(mentions): add kind_filter field to Trigger and detector state"
```

---

## Task 2 — Detect `/`, `#`, `:` as filter chars in `advance_composing`

**Files:**
- Modify: `crates/spur-tui/src/components/completion_trigger.rs`

The contract: while in `Composing { kind: Mention, kind_filter: None }`, if the user types `/`, `#`, or `:` and the current query is empty (i.e. cursor == prefix_start + 1, the buffer position right after `@`), promote the state to `kind_filter = Some(...)` and emit `Update { query: "" }`. The filter char IS in the buffer but stripped from the reported query (the query slice starts at `prefix_start + 1 + filter_char_len`).

- [ ] **Step 1: Write failing tests for prefix detection**

Add these unit tests inside the existing `mod detector_tests` block in `crates/spur-tui/src/components/completion_trigger.rs` (after the existing Task 5 block, near line 854):

```rust
// ── Phase 2: prefix disambiguator detection ──────────────────────

#[test]
fn p2_slash_after_at_sets_file_filter() {
    use crate::mentions::MentionKind;
    let mut det = d();
    let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
    let t = det.step(IntentEvent::TypedChar('/'), "@/", 2, &[], |_| false);
    match t {
        TriggerTransition::Update { query } => assert_eq!(query, ""),
        other => panic!("expected Update, got {other:?}"),
    }
    assert_eq!(det.current_kind_filter(), Some(MentionKind::File));
}

#[test]
fn p2_hash_after_at_sets_issue_filter() {
    use crate::mentions::MentionKind;
    let mut det = d();
    let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
    let t = det.step(IntentEvent::TypedChar('#'), "@#", 2, &[], |_| false);
    match t {
        TriggerTransition::Update { query } => assert_eq!(query, ""),
        other => panic!("expected Update, got {other:?}"),
    }
    assert_eq!(det.current_kind_filter(), Some(MentionKind::Issue));
}

#[test]
fn p2_colon_after_at_sets_code_symbol_filter() {
    use crate::mentions::MentionKind;
    let mut det = d();
    let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
    let t = det.step(IntentEvent::TypedChar(':'), "@:", 2, &[], |_| false);
    match t {
        TriggerTransition::Update { query } => assert_eq!(query, ""),
        other => panic!("expected Update, got {other:?}"),
    }
    assert_eq!(det.current_kind_filter(), Some(MentionKind::CodeSymbol));
}

#[test]
fn p2_query_after_prefix_strips_filter_char() {
    use crate::mentions::MentionKind;
    let mut det = d();
    let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
    let _ = det.step(IntentEvent::TypedChar('/'), "@/", 2, &[], |_| false);
    let _ = det.step(IntentEvent::TypedChar('f'), "@/f", 3, &[], |_| false);
    let _ = det.step(IntentEvent::TypedChar('o'), "@/fo", 4, &[], |_| false);
    let t = det.step(IntentEvent::TypedChar('o'), "@/foo", 5, &[], |_| false);
    match t {
        TriggerTransition::Update { query } => assert_eq!(query, "foo"),
        other => panic!("expected Update, got {other:?}"),
    }
    assert_eq!(det.current_kind_filter(), Some(MentionKind::File));
}

#[test]
fn p2_hash_with_query_strips_filter_char() {
    use crate::mentions::MentionKind;
    let mut det = d();
    let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
    let _ = det.step(IntentEvent::TypedChar('#'), "@#", 2, &[], |_| false);
    let _ = det.step(IntentEvent::TypedChar('b'), "@#b", 3, &[], |_| false);
    let t = det.step(IntentEvent::TypedChar('d'), "@#bd", 4, &[], |_| false);
    match t {
        TriggerTransition::Update { query } => assert_eq!(query, "bd"),
        other => panic!("expected Update, got {other:?}"),
    }
    assert_eq!(det.current_kind_filter(), Some(MentionKind::Issue));
}

#[test]
fn p2_prefix_only_recognized_when_query_is_empty() {
    // `@f/` is NOT a filter — the `/` is part of the query.
    let mut det = d();
    let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
    let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[], |_| false);
    let t = det.step(IntentEvent::TypedChar('/'), "@f/", 3, &[], |_| false);
    match t {
        TriggerTransition::Update { query } => assert_eq!(query, "f/"),
        other => panic!("expected Update, got {other:?}"),
    }
    assert_eq!(det.current_kind_filter(), None);
}

#[test]
fn p2_boundary_paste_with_prefix_does_not_open_filter() {
    // Pasting mid-text `text @/x` does not open at all (Pasted is terminal
    // for Composing and Idle Pasted does not open Mention). The defensive
    // boundary rule keeps Phase 1 behavior intact.
    let mut det = d();
    let t = det.step(IntentEvent::Pasted, "text @/x", 8, &[], |_| false);
    assert!(matches!(t, TriggerTransition::None));
    assert!(det.is_idle());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui --lib completion_trigger::detector_tests::p2_ -- --nocapture`
Expected: FAIL — the detector currently doesn't strip the filter char or set `kind_filter`.

- [ ] **Step 3: Implement prefix detection in `advance_composing`**

In `advance_composing` (currently lines 313–375), modify so the filter-char promotion happens for `IntentEvent::TypedChar(c)` when:
- `kind == Mention` (currently always true in this branch)
- current `kind_filter` is `None`
- `cursor == prefix_start + 2` (i.e. exactly one byte past `@`)
- `c` is `/`, `#`, or `:`

Concretely, change the destructure at lines 319–322 to also capture filter:

```rust
let (prefix_start, kind_filter) = match &self.state {
    TriggerState::Composing {
        kind, prefix_start, kind_filter,
    } => {
        let _ = kind; // Mention/Slash both fall through this fn; SlashArg is dispatched separately.
        (*prefix_start, kind_filter.clone())
    }
    TriggerState::Idle => unreachable!("called with Idle state"),
};
```

Then *before* the existing whitespace-closes block (line 341), insert prefix promotion:

```rust
// Phase 2: a `/`, `#`, or `:` typed immediately after `@` (i.e. with
// an empty query so far) promotes the trigger to filtered mode. The
// filter char IS in the buffer but is stripped from the reported query.
// Only applies to Mention triggers in unified mode.
if matches!(&self.state, TriggerState::Composing { kind: TriggerKindInternal::Mention, .. })
    && kind_filter.is_none()
{
    if let IntentEvent::TypedChar(c) = event {
        if cursor == prefix_start + 1 + c.len_utf8() {
            let new_filter = match c {
                '/' => Some(crate::mentions::MentionKind::File),
                '#' => Some(crate::mentions::MentionKind::Issue),
                ':' => Some(crate::mentions::MentionKind::CodeSymbol),
                _ => None,
            };
            if let Some(filter) = new_filter {
                self.state = TriggerState::Composing {
                    kind: TriggerKindInternal::Mention,
                    prefix_start,
                    kind_filter: Some(filter),
                };
                return TriggerTransition::Update { query: String::new() };
            }
        }
    }
}
```

Then, where the query slice is computed (lines 369–372), adjust the start to skip the filter char when a filter is set. After the existing line:

```rust
let query_region_start = prefix_start + 1;
```

…add (immediately after, before `window_end` computation):

```rust
// Skip the filter char in the reported query when a filter is active.
// Filter chars (`/`, `#`, `:`) are all single-byte ASCII.
let query_region_start = if kind_filter.is_some() {
    (query_region_start + 1).min(text.len())
} else {
    query_region_start
};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui --lib completion_trigger::detector_tests::p2_`
Expected: all 7 new tests PASS.

- [ ] **Step 5: Run full existing trigger tests to check for regression**

Run: `cargo test -p spur-tui --lib completion_trigger`
Expected: every existing Phase 1 test still PASSES (boundary tests, comparator, defensive re-check, etc.).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/completion_trigger.rs
git commit -m "feat(mentions): detect @/, @#, @: as kind filters in advance_composing"
```

---

## Task 3 — Backspace-revert: deleting the filter char restores unified mode

**Files:**
- Modify: `crates/spur-tui/src/components/completion_trigger.rs`

The contract: while in `Composing { Mention, kind_filter: Some(_) }`, a `DeletedChar` event that brings cursor back to `prefix_start + 1` (i.e. just past `@`, filter char now gone from buffer) must clear the filter and emit `Update { query: "" }`. The picker must NOT close.

- [ ] **Step 1: Write failing test**

Append to `mod detector_tests` in `crates/spur-tui/src/components/completion_trigger.rs`:

```rust
#[test]
fn p2_backspace_filter_char_reverts_to_unified() {
    use crate::mentions::MentionKind;
    let mut det = d();
    let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
    let _ = det.step(IntentEvent::TypedChar('/'), "@/", 2, &[], |_| false);
    assert_eq!(det.current_kind_filter(), Some(MentionKind::File));

    // Backspace removes the `/`; buffer is now "@", cursor at 1.
    let t = det.step(IntentEvent::DeletedChar, "@", 1, &[], |_| false);
    match t {
        TriggerTransition::Update { query } => assert_eq!(query, ""),
        other => panic!("expected Update (NOT Close), got {other:?}"),
    }
    assert_eq!(det.current_kind_filter(), None);
    assert!(!det.is_idle(), "picker should remain open");
}

#[test]
fn p2_backspace_does_not_revert_when_query_present() {
    use crate::mentions::MentionKind;
    let mut det = d();
    let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[], |_| false);
    let _ = det.step(IntentEvent::TypedChar('/'), "@/", 2, &[], |_| false);
    let _ = det.step(IntentEvent::TypedChar('f'), "@/f", 3, &[], |_| false);
    let _ = det.step(IntentEvent::TypedChar('o'), "@/fo", 4, &[], |_| false);

    // Delete the `o`. Buffer "@/f", cursor 3. Filter must stay set.
    let t = det.step(IntentEvent::DeletedChar, "@/f", 3, &[], |_| false);
    match t {
        TriggerTransition::Update { query } => assert_eq!(query, "f"),
        other => panic!("expected Update, got {other:?}"),
    }
    assert_eq!(det.current_kind_filter(), Some(MentionKind::File));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui --lib completion_trigger::detector_tests::p2_backspace`
Expected: FAIL (filter is not cleared on backspace yet).

- [ ] **Step 3: Implement backspace-revert in `advance_composing`**

In `advance_composing`, after the prefix-promotion block from Task 2 and before the whitespace-close block, add:

```rust
// Phase 2: backspacing the filter char reverts to unified mode.
// Detection: filter is currently set AND cursor is now at prefix_start+1
// (the buffer position right after `@`, meaning the filter char was
// just deleted). Stay Composing; drop the filter; emit Update.
if kind_filter.is_some()
    && matches!(event, IntentEvent::DeletedChar)
    && cursor == prefix_start + 1
{
    self.state = TriggerState::Composing {
        kind: TriggerKindInternal::Mention,
        prefix_start,
        kind_filter: None,
    };
    return TriggerTransition::Update { query: String::new() };
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui --lib completion_trigger::detector_tests::p2_`
Expected: all Phase 2 tests PASS.

Then run: `cargo test -p spur-tui --lib completion_trigger`
Expected: full module PASSES (no regression in Phase 1 backspace tests, especially `composing_deleted_trigger_char_emits_close_via_defensive_check`).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/completion_trigger.rs
git commit -m "feat(mentions): backspace over filter char reverts to unified mode"
```

---

## Task 4 — Thread `kind_filter` through `MentionRegistry::query`

**Files:**
- Modify: `crates/spur-tui/src/mentions/registry.rs`

- [ ] **Step 1: Add unit test for filtered empty-query**

Append to `mod tests` in `crates/spur-tui/src/mentions/registry.rs`:

```rust
#[test]
fn empty_query_with_file_filter_returns_only_files_with_named_header() {
    let mut registry = test_registry(vec![
        Box::new(WorkerMentionSource::new(vec![WorkerMentionDescriptor {
            name: "alpha".into(),
            description: None,
            tier: None,
        }])),
        Box::new(StaticSource {
            name: "file",
            entries: vec![mention(MentionKind::File, 1, "src/main.rs".into())],
        }),
        Box::new(IssueMentionSource::new(vec![issue("bd-1", "Issue", None)])),
    ]);
    let results = registry.query_filtered(
        CompletionScope::PreSession,
        Path::new("."),
        "",
        128,
        Some(MentionKind::File),
    );
    let headers: Vec<&str> = results.iter().filter_map(|e| e.section_header).collect();
    assert_eq!(headers, vec!["Files (filter: @/)"]);
    let content: Vec<&MentionEntry> = results
        .iter()
        .filter(|e| e.section_header.is_none())
        .collect();
    assert!(content.iter().all(|e| matches!(e.kind, MentionKind::File | MentionKind::Directory)));
    assert_eq!(content.len(), 1);
}

#[test]
fn typed_query_with_issue_filter_excludes_other_kinds() {
    let mut registry = test_registry(vec![
        Box::new(IssueMentionSource::new(vec![issue("bd-foo", "Foo issue", None)])),
        Box::new(StaticSource {
            name: "file",
            entries: vec![mention(MentionKind::File, 1, "foo.rs".into())],
        }),
    ]);
    let results = registry.query_filtered(
        CompletionScope::PreSession,
        Path::new("."),
        "foo",
        16,
        Some(MentionKind::Issue),
    );
    let content: Vec<&MentionEntry> = results
        .iter()
        .filter(|e| e.section_header.is_none())
        .collect();
    assert!(content.iter().all(|e| e.kind == MentionKind::Issue));
    assert_eq!(content.len(), 1);
    assert_eq!(content[0].display, "bd-foo Foo issue");
}

#[test]
fn typed_query_with_code_symbol_filter_excludes_code_files() {
    let mut symbol = mention(MentionKind::CodeSymbol, 1, "FooStruct".into());
    symbol.code_path = Some("src/foo.rs".into());
    let mut code_file = mention(MentionKind::CodeFile, 2, "src/foo.rs".into());
    code_file.code_path = Some("src/foo.rs".into());
    let mut registry = test_registry(vec![Box::new(StaticSource {
        name: "code",
        entries: vec![symbol, code_file],
    })]);
    let results = registry.query_filtered(
        CompletionScope::PreSession,
        Path::new("."),
        "foo",
        16,
        Some(MentionKind::CodeSymbol),
    );
    let content: Vec<&MentionEntry> = results
        .iter()
        .filter(|e| e.section_header.is_none())
        .collect();
    assert!(content.iter().all(|e| e.kind == MentionKind::CodeSymbol));
    assert_eq!(content.len(), 1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui --lib mentions::registry::tests -- --nocapture 2>&1 | head -40`
Expected: FAIL with "method `query_filtered` not found".

- [ ] **Step 3: Add `query_filtered` and helpers**

In `crates/spur-tui/src/mentions/registry.rs`, replace the existing `pub fn query(...)` signature (line 310) by introducing `query_filtered` as the primary method and keeping `query` as a thin wrapper for back-compat. Rename the method body and add the filter parameter:

```rust
pub fn query(
    &mut self,
    scope: CompletionScope<'_>,
    cwd: &Path,
    query: &str,
    limit: usize,
) -> Vec<MentionEntry> {
    self.query_filtered(scope, cwd, query, limit, None)
}

pub fn query_filtered(
    &mut self,
    scope: CompletionScope<'_>,
    cwd: &Path,
    query: &str,
    limit: usize,
    kind_filter: Option<MentionKind>,
) -> Vec<MentionEntry> {
    // … existing body of `query`, with the following changes …
}
```

Move the entire body of the current `query` (lines 311–466) into `query_filtered`. Then apply these targeted edits inside `query_filtered`:

(a) Right after the `dedup_file_entries_with_code_files(&mut all_entries);` call (line 366), insert filter pre-pass:

```rust
if let Some(filter) = &kind_filter {
    all_entries.retain(|entry| kind_matches_filter(&entry.kind, filter));
}
let entries = all_entries.as_slice();
```

Delete the now-duplicate `let entries = all_entries.as_slice();` line that follows.

(b) In the empty-query branch (line 369), when `kind_filter` is set, emit a single filtered section and skip the four-section composition. Insert right after `if query.is_empty() {`:

```rust
if let Some(filter) = &kind_filter {
    let (cap, header) = match filter {
        MentionKind::File | MentionKind::Directory => (limit, "Files (filter: @/)"),
        MentionKind::Issue => (limit, "Issues (filter: @#)"),
        MentionKind::CodeSymbol => (limit, "Code Symbols (filter: @:)"),
        // Other kinds aren't reachable via prefix; safety fallback.
        _ => (limit, "Filtered"),
    };
    let mut section: Vec<MentionEntry> =
        entries.iter().map(|e| (*e).clone()).take(cap).collect();
    if section.is_empty() {
        return Vec::new();
    }
    // Use existing empty-query sort rules per filter kind:
    sort_section_for_filter(&mut section, filter);
    let mut rows = Vec::with_capacity(section.len() + 1);
    append_section_rows(&mut rows, header, &section);
    rows.truncate(limit);
    return rows;
}
```

Note: `append_section_rows` currently takes a `&'static str`. Add an overloaded helper or change its signature to accept `&str` via cloning. Since the header strings are static literals, just inline a copy of the helper for the filtered branch (or change `append_section_rows` to accept `&'static str` and pass `&'static str` literals — which all four headers above are).

Actually the cleanest fix: change `MentionEntry.section_header` from `Option<&'static str>` (entry.rs line 20) to `Option<String>`. Then `append_section_rows` becomes:

```rust
fn append_section_rows(rows: &mut Vec<MentionEntry>, header: impl Into<String>, section: &[MentionEntry]) {
    if section.is_empty() { return; }
    let header_text: String = header.into();
    let display = format!("── {header_text} ──");
    rows.push(MentionEntry {
        section_header: Some(header_text),
        kind: MentionKind::File,
        uri: String::new(),
        display,
        ..MentionEntry::default()
    });
    rows.extend(section.iter().cloned());
}
```

Update `entry.rs` line 20:

```rust
pub section_header: Option<String>,
```

Update every callsite that compares `section_header == Some("Workers")` etc. to `section_header.as_deref() == Some("Workers")`. The call-sites are in `registry.rs` tests (lines 1492, 1503, 178–195 of mentions_v2_picker.rs) and `query_source.rs` lines 537, 546, 551 (`m.section_header.is_some()` already works unchanged) and 538 (`m.display.clone()` already works).

For the four static literals used by Phase 1 (`"Workers"`, `"Files"`, `"Issues"`, `"Code"`) keep them as string-literal arguments to `append_section_rows`. Compiler will auto-borrow into `String` via `Into`.

(c) In the typed-query branch (line 440), no additional change is needed — the pre-pass filter at (a) already restricts the candidate set, so the existing scorer/comparator operates correctly on a smaller universe.

Then add these helpers at the bottom of `registry.rs`, before `#[cfg(test)] mod tests`:

```rust
fn kind_matches_filter(kind: &MentionKind, filter: &MentionKind) -> bool {
    match filter {
        // `@/` matches both files and directories.
        MentionKind::File => matches!(kind, MentionKind::File | MentionKind::Directory),
        MentionKind::Issue => matches!(kind, MentionKind::Issue),
        MentionKind::CodeSymbol => matches!(kind, MentionKind::CodeSymbol),
        // Other kinds cannot be selected via prefix; never matches.
        MentionKind::Directory | MentionKind::CodeFile | MentionKind::Worker => false,
    }
}

fn sort_section_for_filter(section: &mut Vec<MentionEntry>, filter: &MentionKind) {
    match filter {
        MentionKind::File => section.sort_by(|a, b| {
            path_depth(&a.display)
                .cmp(&path_depth(&b.display))
                .then(a.display.len().cmp(&b.display.len()))
                .then(a.display.cmp(&b.display))
                .then(a.uri.cmp(&b.uri))
        }),
        MentionKind::Issue => {
            // Already in newest-first order from IssueMentionSource.
        }
        MentionKind::CodeSymbol => section.sort_by(|a, b| {
            a.display.len().cmp(&b.display.len()).then(a.display.cmp(&b.display))
        }),
        _ => {}
    }
}
```

- [ ] **Step 4: Update `MentionEntry` literal in `append_section_rows`**

The existing `append_section_rows` constructs a `MentionEntry` with explicit `None` for every field (lines 632–644). Replace with the spread-default form shown above so future fields don't have to be updated here.

- [ ] **Step 5: Run new tests**

Run: `cargo test -p spur-tui --lib mentions::registry::tests::empty_query_with_file_filter mentions::registry::tests::typed_query_with_issue_filter mentions::registry::tests::typed_query_with_code_symbol_filter`
Expected: all three PASS.

- [ ] **Step 6: Run full registry tests + Phase 1 regression**

Run: `cargo test -p spur-tui --lib mentions`
Expected: all existing tests PASS, including `comparator_is_strict_weak_ordering`, `empty_query_caps_each_kind`, `empty_query_keeps_most_recent_issues_first`, `empty_query_emits_section_headers_in_order`.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/mentions/registry.rs crates/spur-tui/src/mentions/entry.rs
git commit -m "feat(mentions): add kind_filter param to MentionRegistry::query"
```

---

## Task 5 — Wire `kind_filter` through `MentionQuerySource` and the completion port

**Files:**
- Modify: `crates/spur-tui/src/components/query_source.rs`
- Modify: `crates/spur-tui/src/components/input_completion.rs`

- [ ] **Step 1: Extend `MentionQuerySource` with `kind_filter`**

In `crates/spur-tui/src/components/query_source.rs` at the struct definition (lines 333–344), add a field:

```rust
pub struct MentionQuerySource {
    registry: Rc<RefCell<crate::mentions::MentionRegistry>>,
    scope: MentionSourceScope,
    cwd: std::path::PathBuf,
    prefix_start: usize,
    last_hits: Vec<crate::mentions::MentionEntry>,
    kind_filter: Option<crate::mentions::MentionKind>,
}
```

Change `MentionQuerySource::new` (lines 363–383) to accept and store `kind_filter`:

```rust
pub fn new(
    registry: Rc<RefCell<crate::mentions::MentionRegistry>>,
    scope: crate::mentions::CompletionScope<'_>,
    cwd: std::path::PathBuf,
    prefix_start: usize,
    kind_filter: Option<crate::mentions::MentionKind>,
) -> Self {
    // … existing body … set kind_filter at the end.
}
```

Add a setter:

```rust
pub fn set_kind_filter(&mut self, kind_filter: Option<crate::mentions::MentionKind>) {
    self.kind_filter = kind_filter;
}
```

Change the `refresh` body (line 515) to use `query_filtered`:

```rust
let hits = self.registry.borrow_mut().query_filtered(
    self.scope.as_completion_scope(),
    &self.cwd,
    query,
    20,
    self.kind_filter.clone(),
);
```

- [ ] **Step 2: Update every existing `MentionQuerySource::new` callsite**

There are 7 callsites (1 in `input_completion.rs`, 6 in `query_source.rs` tests). For each, append `, None` as the new last argument.

- `crates/spur-tui/src/components/input_completion.rs:126–131` — replace with:

```rust
let src = MentionQuerySource::new(
    Rc::clone(env.mention_registry),
    env.scope,
    env.cwd.to_path_buf(),
    trigger.prefix_start,
    trigger.kind_filter.clone(),
);
```

- `crates/spur-tui/src/components/query_source.rs:1075, 1087, 1106, 1139, 1179, 1232` — append `, None` before the closing `)`.

- [ ] **Step 3: Forward filter changes from `Update` transitions**

In `input_completion.rs` at line 95–103, the `Update { query }` branch currently only updates the query. After updating query, also push the latest filter:

```rust
TriggerTransition::Update { query } => {
    if let Some(shell) = self.picker_shell.as_mut() {
        // Push current filter into the active MentionQuerySource so
        // backspacing the filter char swaps modes without reopening
        // the shell.
        let new_filter = self.trigger_detector.current_kind_filter();
        shell.set_mention_kind_filter(new_filter);

        if shell.should_debounce_input_bar_updates() {
            self.pending_mention_query = Some((Instant::now(), query));
        } else {
            shell.set_query_from_input_bar(&query);
        }
    }
}
```

- [ ] **Step 4: Add `set_mention_kind_filter` to `PickerShell`**

Find `PickerShell` (referenced from `crate::components::picker_shell`). Add a method that downcasts/forwards into `MentionQuerySource` if the active source is a mention source. Read the file first:

Run: `cat crates/spur-tui/src/components/picker_shell.rs | head -80`

Add a method that calls into the boxed `QuerySource` via a new trait method `as_mention_query_source_mut(&mut self) -> Option<&mut MentionQuerySource>` (default impl `None`), implemented on `MentionQuerySource` to return `Some(self)`. Then:

```rust
impl PickerShell {
    pub fn set_mention_kind_filter(&mut self, filter: Option<crate::mentions::MentionKind>) {
        if let Some(src) = self.source_mut().as_mention_query_source_mut() {
            src.set_kind_filter(filter);
        }
    }
}
```

If `PickerShell` doesn't expose `source_mut()`, add a `&mut dyn QuerySource` accessor.

Add the trait method to the `QuerySource` trait in `query_source.rs`:

```rust
fn as_mention_query_source_mut(&mut self) -> Option<&mut MentionQuerySource> {
    None
}
```

And override on `MentionQuerySource`:

```rust
impl QuerySource for MentionQuerySource {
    // … existing methods …
    fn as_mention_query_source_mut(&mut self) -> Option<&mut MentionQuerySource> {
        Some(self)
    }
}
```

- [ ] **Step 5: Compile-check**

Run: `cargo check -p spur-tui --tests`
Expected: no errors. Fix any leftover callsite signature mismatches reported by the compiler — they should all be `MentionQuerySource::new` calls missing the final `None` argument, or pattern matches missing `kind_filter` on `TriggerState::Composing`.

- [ ] **Step 6: Run all spur-tui unit tests**

Run: `cargo test -p spur-tui --lib`
Expected: all PASS. If any Phase 1 picker test now fails because section-header comparisons changed from `&'static str` to `String`, update them to use `.as_deref()`.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/query_source.rs crates/spur-tui/src/components/input_completion.rs crates/spur-tui/src/components/picker_shell.rs
git commit -m "feat(mentions): thread kind_filter through MentionQuerySource and picker shell"
```

---

## Task 6 — Integration tests in `tests/mentions_v2_picker.rs`

**Files:**
- Modify: `crates/spur-tui/tests/mentions_v2_picker.rs`

- [ ] **Step 1: Add three end-to-end tests**

Append to `crates/spur-tui/tests/mentions_v2_picker.rs`:

```rust
#[test]
fn prefix_slash_filters_picker_to_files() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let previous_env = std::env::var_os(CODE_GRAPH_INDEX_ENV);

    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("alpha.rs"), "// fixture").expect("write file");

    let graph_path = write_graph_fixture(
        tmp.path(),
        serde_json::json!({
            "header": { "graph_index_version": TEST_GRAPH_INDEX_VERSION },
            "files": [], "symbols": []
        }),
    );
    std::env::set_var(CODE_GRAPH_INDEX_ENV, &graph_path);

    let workers = vec![WorkerMentionDescriptor {
        name: "alpha-worker".into(),
        description: None,
        tier: None,
    }];

    let mut view = SessionDetailView::new(
        SessionId::new(),
        "claude".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
        workers.clone(),
    );
    view.set_issue_snapshot(vec![issue_summary("bd-x", "Title")]);

    type_str(&mut view, "@/");
    assert!(view.completion_active_for_test());
    let rendered = render_text(&mut view, 160, 48);
    assert!(rendered.contains("Files (filter: @/)"),
        "header should name the active filter; rendered=\n{rendered}");
    assert!(!rendered.contains("── Workers ──"),
        "workers must be hidden under @/ filter; rendered=\n{rendered}");
    assert!(!rendered.contains("── Issues ──"),
        "issues must be hidden under @/ filter; rendered=\n{rendered}");
    assert!(rendered.contains("alpha.rs"),
        "file fixture should appear; rendered=\n{rendered}");
    assert!(!rendered.contains("alpha-worker"),
        "worker must not appear under @/ filter");

    match previous_env {
        Some(p) => std::env::set_var(CODE_GRAPH_INDEX_ENV, p),
        None => std::env::remove_var(CODE_GRAPH_INDEX_ENV),
    }
}

#[test]
fn prefix_hash_filters_picker_to_issues() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let previous_env = std::env::var_os(CODE_GRAPH_INDEX_ENV);

    let tmp = tempfile::tempdir().expect("tempdir");
    let graph_path = write_graph_fixture(
        tmp.path(),
        serde_json::json!({
            "header": { "graph_index_version": TEST_GRAPH_INDEX_VERSION },
            "files": [], "symbols": []
        }),
    );
    std::env::set_var(CODE_GRAPH_INDEX_ENV, &graph_path);

    let mut view = SessionDetailView::new(
        SessionId::new(),
        "claude".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
        vec![WorkerMentionDescriptor { name: "wkr".into(), description: None, tier: None }],
    );
    view.set_issue_snapshot(vec![issue_summary("bd-123", "Filter target")]);

    type_str(&mut view, "@#");
    assert!(view.completion_active_for_test());
    let rendered = render_text(&mut view, 160, 48);
    assert!(rendered.contains("Issues (filter: @#)"),
        "header should name @# filter; rendered=\n{rendered}");
    assert!(rendered.contains("bd-123"));
    assert!(!rendered.contains("── Workers ──"));

    match previous_env {
        Some(p) => std::env::set_var(CODE_GRAPH_INDEX_ENV, p),
        None => std::env::remove_var(CODE_GRAPH_INDEX_ENV),
    }
}

#[test]
fn prefix_colon_filters_picker_to_code_symbols() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let previous_env = std::env::var_os(CODE_GRAPH_INDEX_ENV);

    let tmp = tempfile::tempdir().expect("tempdir");
    let graph_path = write_graph_fixture(
        tmp.path(),
        serde_json::json!({
            "header": { "graph_index_version": TEST_GRAPH_INDEX_VERSION },
            "files": [],
            "symbols": [
                {"stable_symbol_id": "sym-1", "file_path": "src/x.rs", "byte_range": [0,1],
                 "line_range": [1,1], "entity_name": "FilterMe", "symbol_kind": "fn",
                 "anchor_hash": "1", "enclosing_scope": "module x"}
            ]
        }),
    );
    std::env::set_var(CODE_GRAPH_INDEX_ENV, &graph_path);

    let mut view = SessionDetailView::new(
        SessionId::new(),
        "claude".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
        Vec::new(),
    );

    type_str(&mut view, "@:");
    assert!(view.completion_active_for_test());
    let rendered = render_text(&mut view, 160, 48);
    assert!(rendered.contains("Code Symbols (filter: @:)"),
        "header should name @: filter; rendered=\n{rendered}");
    assert!(rendered.contains("FilterMe"));

    match previous_env {
        Some(p) => std::env::set_var(CODE_GRAPH_INDEX_ENV, p),
        None => std::env::remove_var(CODE_GRAPH_INDEX_ENV),
    }
}

#[test]
fn backspace_filter_char_reverts_picker_to_unified() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let previous_env = std::env::var_os(CODE_GRAPH_INDEX_ENV);

    let tmp = tempfile::tempdir().expect("tempdir");
    let graph_path = write_graph_fixture(
        tmp.path(),
        serde_json::json!({
            "header": { "graph_index_version": TEST_GRAPH_INDEX_VERSION },
            "files": [], "symbols": []
        }),
    );
    std::env::set_var(CODE_GRAPH_INDEX_ENV, &graph_path);

    let mut view = SessionDetailView::new(
        SessionId::new(),
        "claude".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
        vec![WorkerMentionDescriptor { name: "alice".into(), description: None, tier: None }],
    );
    view.set_issue_snapshot(vec![issue_summary("bd-1", "Title")]);

    type_str(&mut view, "@/");
    let filtered = render_text(&mut view, 160, 48);
    assert!(filtered.contains("Files (filter: @/)"));

    press(&mut view, KeyCode::Backspace);
    assert!(view.completion_active_for_test(),
        "backspacing filter char must NOT close the picker");
    let unified = render_text(&mut view, 160, 48);
    assert!(unified.contains("── Workers ──"),
        "after backspace, unified workers section returns; rendered=\n{unified}");
    assert!(!unified.contains("(filter:"),
        "no filter header should be present in unified mode");

    match previous_env {
        Some(p) => std::env::set_var(CODE_GRAPH_INDEX_ENV, p),
        None => std::env::remove_var(CODE_GRAPH_INDEX_ENV),
    }
}
```

- [ ] **Step 2: Run integration tests**

Run: `cargo test -p spur-tui --test mentions_v2_picker -- --nocapture`
Expected: all four new tests PASS, plus the two existing tests (`empty_at_shows_sectioned_picker`, `typed_query_prefers_files_within_window`) still PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/tests/mentions_v2_picker.rs
git commit -m "test(mentions): integration tests for @/, @#, @: prefix filters"
```

---

## Task 7 — Full verification + close issue

- [ ] **Step 1: Run the entire spur-tui test suite**

Run: `cargo test -p spur-tui`
Expected: all tests PASS.

- [ ] **Step 2: Run clippy + fmt**

Run: `cargo clippy -p spur-tui --tests -- -D warnings && cargo fmt -p spur-tui -- --check`
Expected: clean. Fix any warnings.

- [ ] **Step 3: Manual smoke (optional but recommended)**

Run: `cargo run --bin spur` and verify in a session:
- `@` opens the unified picker (workers/files/issues/code sections).
- `@/` filters to files only, header reads `── Files (filter: @/) ──`.
- `@/foo` searches only file names containing `foo`.
- Backspace over the `/` returns to the unified picker without closing.
- `@#bd` filters to issues containing `bd`.
- `@:Foo` filters to code symbols.
- `@foo` (no filter char) still returns unified fuzzy results.

If any step fails, file follow-up issue and DO NOT mark bd-1id complete.

- [ ] **Step 4: Update the beads issue**

Comment summarizing what shipped, then close:

Run via the MCP tool `mcp__spur-mcp__update_issue` with:
- `id: "bd-1id"`
- `status: "closed"`
- `comment`: paste links to the commits and call out that Phase 1 telemetry decision still stands (this implementation lands the design but the *enablement* decision was made by user instruction).

---

## Self-review checklist (pre-execution)

- [x] Spec §3.2 / §4.1 coverage: tasks 2–3 cover detector, task 4 covers registry, task 5 covers UI render, task 6 covers integration.
- [x] All acceptance criteria from bd-1id map to a task:
  - `@/foo<Tab>` → Task 5 (accept path unchanged; registry filter via Task 4).
  - `@#bd-<Tab>` → same.
  - `@:Sym<Tab>` → same.
  - Bare `@foo` unchanged → Phase 1 regression run in Task 2.5, 4.6, 5.6.
  - Backspace revert → Task 3.
  - Active filter visible in header → Task 4 step 3 (header text).
  - `completion_trigger.rs` unit tests → Task 2 + Task 3.
  - Integration test → Task 6.
  - Phase 1 tests still pass → Tasks 2.5, 4.6, 5.6, 7.1.
- [x] No placeholders / no "TBD" / no "see above" code references.
- [x] Type names consistent: `kind_filter: Option<MentionKind>`, header strings static `&str` literals.

---

## Risks called out by the spec (still apply)

- **Detection ambiguity**: a user mid-typing `@text` who then deletes back to `@` and types `:` may briefly see a filtered picker. Spec accepts this — the filter is "next char after `@`", and the empty-query check in Task 2 enforces that.
- **Pasted prefixes**: `text @/foo` pasted does NOT open with filter — boundary rule unchanged. Covered by `p2_boundary_paste_with_prefix_does_not_open_filter`.
- **Backwards compatibility**: `Trigger` is `pub` and used across the picker port; adding a field is a minor break for any out-of-tree consumer. None exists in this monorepo.
