# PickerShell Phase 3 — Mention + Slash Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate `@mention` and `/slash` popups from the legacy `CompletionPopup + active_trigger` direct-wiring path onto `PickerShell`, backed by new `MentionQuerySource` and `SlashQuerySource` implementations of the `QuerySource` trait. User-visible behavior of mention and slash stays identical (query still lives in the `InputBar` trigger prefix; Tab accepts; Esc dismisses); under the hood the popup widget, row type, and accept dispatch now match the Ctrl+R history path.

**Architecture:** Two new `QuerySource` impls in `query_source.rs`. One rewiring pass in `session_detail.rs`: `refresh_popup` opens/updates/closes a single `PickerShell` instance on every `detect()` transition, accept payloads dispatch through the existing `PickerAction::Accept` → `RetrievalAccept` arm. `CompletionPopup` widget stays as a component but stops being used directly by `SessionDetailView` — it's now only reached via `PickerShell`. The `mention_registry` field changes from `MentionRegistry` to `Rc<RefCell<MentionRegistry>>` so the source can hold a cheap shared handle without lifetime gymnastics.

**Tech Stack:** Rust 2021, existing ratatui / crossterm / nucleo-matcher stack, no new dependencies.

**Spec:** `docs/superpowers/specs/2026-04-19-picker-shell-retrieval-unification-design.md` (Phase 3 section).

---

## File Structure

**Modify:**
- `crates/spur-tui/src/components/query_source.rs` — add `MentionQuerySource` + `SlashQuerySource` and their unit tests; no changes to existing types or trait.
- `crates/spur-tui/src/components/picker_shell.rs` — minor: `set_query_from_input_bar` already exists and is marked `#[allow(dead_code)]`; remove the `#[allow]` after Phase 3 wires it up. No behavior changes.
- `crates/spur-tui/src/views/session_detail.rs` — replace the body of `refresh_popup`, delete `accept_completion`, remove `active_mention_hits` field, remove `completion_popup` field and its render call. Convert `mention_registry: MentionRegistry` → `mention_registry: Rc<RefCell<MentionRegistry>>`. Keep `active_trigger: Option<Trigger>` as a carrier for `prefix_start` during accept (not dead weight; essential for the mention `ReplaceTriggerWithAtom` dispatch).

**Create:**
- `crates/spur-tui/tests/picker_shell_trigger_parity.rs` — integration tests asserting mention-accept still inserts a `ResourceLink`-producing atom and slash-accept still replaces the trigger token verbatim. Mirrors the spec's "Phase 3 parity integration tests" requirement.

**Unchanged:**
- `crates/spur-tui/src/components/mini_input.rs`
- `crates/spur-tui/src/components/input_bar.rs`
- `crates/spur-tui/src/components/completion_popup.rs` (still exists, still used by `PickerShell` internally)
- `crates/spur-tui/src/components/completion_trigger.rs` (still emits `Trigger`; session_detail uses its output)
- `crates/spur-tui/src/input_history.rs`
- `crates/spur-tui/src/mentions/` (registry API unchanged)
- `crates/spur-tui/src/commands/` (registry API unchanged)

---

## Data-model change — `RetrievalAccept::InsertAtom` gains a `replace_from` field

Mention accept needs to (a) clear the `@query` prefix range `[prefix_start..cursor]` and then (b) insert a protected atom at the vacated position. The existing `InsertAtom { text, uri, name }` variant only handles (b). Extend it:

```rust
pub enum RetrievalAccept {
    ReplaceState(InputStateSnapshot),
    InsertAtom {
        text: String,
        uri: String,
        name: String,
        /// If `Some(p)`, the view clears bytes `[p..cursor]` before
        /// inserting the atom at position `p`. Used by `@mention` accept
        /// to drop the `@query` prefix that drove the popup. Byte offset
        /// MUST be on a UTF-8 char boundary of the InputBar text at
        /// accept time.
        replace_from: Option<usize>,
    },
    ReplaceTriggerToken {
        prefix_start: usize,
        replacement: String,
    },
}
```

This is additive — Ctrl+R history (Phase 1/2) doesn't construct `InsertAtom`, so no call site breaks. The Phase 1 accept dispatch in `session_detail.rs` currently handles `InsertAtom { text, uri, name }`; Phase 3 extends that arm to honor `replace_from` (see Task 3).

---

## Task 1: `MentionQuerySource`

Wraps a shared `MentionRegistry` handle, the active `SessionId` + `cwd` for the mention query, and the trigger's `prefix_start` captured at shell-open time. Produces `RetrievalRow`s with icons on the label; accept returns `InsertAtom { text, uri, name, replace_from: Some(prefix_start) }`.

**Files:**
- Modify: `crates/spur-tui/src/components/query_source.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block in `crates/spur-tui/src/components/query_source.rs`:

```rust
    use crate::mentions::{MentionEntry, MentionKind, MentionRegistry};
    use spur_acp::SessionId;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn make_mention_registry_with_cwd(cwd: &std::path::Path) -> Rc<RefCell<MentionRegistry>> {
        let mut r = MentionRegistry::new();
        // Prime the cache by running one query; cwd must actually exist so
        // FileMentionSource returns something deterministic in tests.
        let _ = r.query(&SessionId::new(), cwd, "", 5);
        Rc::new(RefCell::new(r))
    }

    #[test]
    fn mention_source_title_is_at_mention() {
        let registry = make_mention_registry_with_cwd(std::path::Path::new("."));
        let src = MentionQuerySource::new(
            Rc::clone(&registry),
            SessionId::new(),
            std::path::PathBuf::from("."),
            1, // prefix_start — the '@' byte
        );
        assert_eq!(src.title(), "Mentions · @");
    }

    #[test]
    fn mention_source_query_mode_is_read_from_input_bar() {
        let registry = make_mention_registry_with_cwd(std::path::Path::new("."));
        let src = MentionQuerySource::new(
            Rc::clone(&registry),
            SessionId::new(),
            std::path::PathBuf::from("."),
            0,
        );
        assert_eq!(src.query_mode(), QueryMode::ReadFromInputBar);
    }

    #[test]
    fn mention_source_accept_returns_insert_atom_with_replace_from() {
        // Inject a fake mention entry by preloading the registry cache
        // manually. Since that's private, we instead use a real registry
        // over a fixed tmpdir with one file.
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("README.md");
        std::fs::write(&file_path, "x").unwrap();

        let registry = make_mention_registry_with_cwd(tmp.path());
        let mut src = MentionQuerySource::new(
            Rc::clone(&registry),
            SessionId::new(),
            tmp.path().to_path_buf(),
            1, // '@' at byte 1
        );
        let rows = src.refresh("READ");
        assert!(
            !rows.is_empty(),
            "expected at least one match for 'READ' against README.md"
        );
        let accept = src.accept(0).expect("row 0 exists");
        match accept {
            RetrievalAccept::InsertAtom {
                text,
                uri,
                name,
                replace_from,
            } => {
                assert_eq!(replace_from, Some(1));
                assert!(text.starts_with('@'));
                assert!(!uri.is_empty());
                assert!(!name.is_empty());
            }
            other => panic!("expected InsertAtom, got {other:?}"),
        }
    }

    #[test]
    fn mention_source_row_label_carries_icon_and_at_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("foo.txt"), "x").unwrap();
        let registry = make_mention_registry_with_cwd(tmp.path());
        let mut src = MentionQuerySource::new(
            Rc::clone(&registry),
            SessionId::new(),
            tmp.path().to_path_buf(),
            0,
        );
        let rows = src.refresh("foo");
        assert!(!rows.is_empty());
        // Label format matches today's legacy format: "<icon> @<display>"
        // so visual parity is preserved.
        assert!(
            rows[0].primary.contains("@foo"),
            "primary missing @foo: {:?}",
            rows[0].primary
        );
        assert!(
            rows[0].primary.starts_with(|c: char| c == '📁' || c == '📄'),
            "primary missing icon prefix: {:?}",
            rows[0].primary
        );
    }
```

- [ ] **Step 2: Run tests — verify failure**

Run: `cargo test -p spur-tui --lib components::query_source::tests::mention_source`
Expected: compile error — `MentionQuerySource` not found.

- [ ] **Step 3: Extend `RetrievalAccept::InsertAtom` with `replace_from`**

In `crates/spur-tui/src/components/query_source.rs`, replace the existing `InsertAtom` variant. Find:

```rust
    #[allow(dead_code)]
    InsertAtom {
        text: String,
        uri: String,
        name: String,
    },
```

Replace with:

```rust
    InsertAtom {
        text: String,
        uri: String,
        name: String,
        /// If `Some(p)`, the view clears bytes `[p..cursor]` before
        /// inserting the atom at position `p`. Used by `@mention` accept
        /// to drop the `@query` prefix that drove the popup. MUST be on
        /// a UTF-8 char boundary of the InputBar text at accept time.
        replace_from: Option<usize>,
    },
```

(Remove the `#[allow(dead_code)]` — Phase 3 constructs this variant.)

- [ ] **Step 4: Implement `MentionQuerySource`**

Append to `crates/spur-tui/src/components/query_source.rs` (after `HistoryQuerySource`, before the test module):

```rust
use std::cell::RefCell;
use std::rc::Rc;

/// QuerySource backed by a shared `MentionRegistry` handle. Each `refresh`
/// call re-queries the registry with the current query, so the source
/// sees fresh filesystem cache contents. Cheap handle clones; no moves.
pub struct MentionQuerySource {
    registry: Rc<RefCell<crate::mentions::MentionRegistry>>,
    session_id: spur_acp::SessionId,
    cwd: std::path::PathBuf,
    /// Byte offset in the InputBar text where the trigger's '@' lives.
    /// Captured at shell-open time, passed into `InsertAtom.replace_from`
    /// on accept so the view clears `[prefix_start..cursor]` before
    /// inserting the atom.
    prefix_start: usize,
    /// Entries parallel to the rows returned by the most recent `refresh`.
    last_hits: Vec<crate::mentions::MentionEntry>,
}

impl MentionQuerySource {
    pub fn new(
        registry: Rc<RefCell<crate::mentions::MentionRegistry>>,
        session_id: spur_acp::SessionId,
        cwd: std::path::PathBuf,
        prefix_start: usize,
    ) -> Self {
        Self {
            registry,
            session_id,
            cwd,
            prefix_start,
            last_hits: Vec::new(),
        }
    }
}

impl QuerySource for MentionQuerySource {
    fn title(&self) -> &str {
        "Mentions · @"
    }

    fn query_mode(&self) -> QueryMode {
        QueryMode::ReadFromInputBar
    }

    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> {
        use crate::mentions::MentionKind;
        let hits = self
            .registry
            .borrow_mut()
            .query(&self.session_id, &self.cwd, query, 20);
        let rows: Vec<RetrievalRow> = hits
            .iter()
            .map(|m| {
                let icon = match m.kind {
                    MentionKind::Directory => "\u{1F4C1}",
                    MentionKind::File => "\u{1F4C4}",
                };
                RetrievalRow {
                    primary: format!("{} @{}", icon, m.display),
                    secondary: String::new(),
                    tag: String::new(),
                    atoms: Vec::new(),
                }
            })
            .collect();
        self.last_hits = hits;
        rows
    }

    fn accept(&self, row_idx: usize) -> Option<RetrievalAccept> {
        let hit = self.last_hits.get(row_idx)?;
        Some(RetrievalAccept::InsertAtom {
            text: format!("@{}", hit.display),
            uri: hit.uri.clone(),
            name: hit.display.clone(),
            replace_from: Some(self.prefix_start),
        })
    }
}
```

- [ ] **Step 5: Run tests — verify pass**

Run: `cargo test -p spur-tui --lib components::query_source`
Expected: all 16 existing tests + 4 new mention tests = 20 pass. If the `README.md`-matching test fails due to nucleo scoring edge cases, relax the assertion from `!rows.is_empty()` to checking `rows.iter().any(|r| r.primary.contains("README"))` — document the change in a comment.

- [ ] **Step 6: Full crate test sweep**

Run: `cargo test -p spur-tui`
Expected: no regressions. Phase 1 and Phase 2 tests still green.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/query_source.rs
git commit -m "feat(spur-tui): add MentionQuerySource for Phase 3

New QuerySource impl backed by a shared Rc<RefCell<MentionRegistry>>.
Produces rows with the same icon + @display label the legacy popup
rendered, preserving visual parity. Accept returns
RetrievalAccept::InsertAtom { replace_from: Some(prefix_start) } so
the view clears the @query prefix before inserting the atom.

Extends RetrievalAccept::InsertAtom with an optional replace_from
field — Ctrl+R history (Phase 1/2) never constructs InsertAtom, so
no call site breaks.

Part of: PickerShell Phase 3

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: `SlashQuerySource`

Wraps an owned `CommandRegistry` snapshot (the view hands it a cloned snapshot at shell-open time). Accept returns `ReplaceTriggerToken { prefix_start, replacement: "<canonical_typed_form> " }`.

**Files:**
- Modify: `crates/spur-tui/src/components/query_source.rs`

- [ ] **Step 1: Write the failing tests**

Append to the test module:

```rust
    #[test]
    fn slash_source_title_is_slash_command() {
        let src = SlashQuerySource::new(Vec::new(), 0);
        assert_eq!(src.title(), "Commands · /");
    }

    #[test]
    fn slash_source_query_mode_is_read_from_input_bar() {
        let src = SlashQuerySource::new(Vec::new(), 0);
        assert_eq!(src.query_mode(), QueryMode::ReadFromInputBar);
    }

    #[test]
    fn slash_source_accept_returns_replace_trigger_token() {
        use crate::commands::{CommandEntry, CommandSource};
        let entries = vec![SlashRow {
            canonical: "/help".to_string(),
            description: "Show help".to_string(),
            tag: "⟨spur⟩".to_string(),
        }];
        let src = SlashQuerySource::new(entries, 0);
        let accept = src.accept(0).expect("row 0 exists");
        match accept {
            RetrievalAccept::ReplaceTriggerToken {
                prefix_start,
                replacement,
            } => {
                assert_eq!(prefix_start, 0);
                assert_eq!(replacement, "/help ");
            }
            other => panic!("expected ReplaceTriggerToken, got {other:?}"),
        }
    }

    #[test]
    fn slash_source_refresh_ranks_by_fuzzy_match_on_canonical() {
        let rows = vec![
            SlashRow {
                canonical: "/help".to_string(),
                description: "".to_string(),
                tag: "⟨spur⟩".to_string(),
            },
            SlashRow {
                canonical: "/mode".to_string(),
                description: "".to_string(),
                tag: "⟨spur⟩".to_string(),
            },
            SlashRow {
                canonical: "/claude:help".to_string(),
                description: "".to_string(),
                tag: "⟨claude⟩".to_string(),
            },
        ];
        let mut src = SlashQuerySource::new(rows, 0);
        let res = src.refresh("hel");
        assert!(res.iter().any(|r| r.primary == "/help"));
        assert!(res.iter().any(|r| r.primary == "/claude:help"));
        assert!(!res.iter().any(|r| r.primary == "/mode"));
    }

    #[test]
    fn slash_source_row_carries_tag() {
        let rows = vec![SlashRow {
            canonical: "/help".to_string(),
            description: "Show help".to_string(),
            tag: "⟨spur⟩".to_string(),
        }];
        let mut src = SlashQuerySource::new(rows, 0);
        let res = src.refresh("");
        assert_eq!(res[0].tag, "⟨spur⟩");
        assert_eq!(res[0].secondary, "Show help");
    }
```

- [ ] **Step 2: Run tests — verify failure**

Run: `cargo test -p spur-tui --lib components::query_source::tests::slash_source`
Expected: compile error — `SlashQuerySource` + `SlashRow` not found.

- [ ] **Step 3: Implement `SlashQuerySource`**

Append to `crates/spur-tui/src/components/query_source.rs` (after `MentionQuerySource`, before tests):

```rust
/// Minimal display-oriented row supplied to `SlashQuerySource`. The view
/// pre-computes these from its `CommandRegistry` at shell-open time so the
/// source doesn't take a live registry reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashRow {
    /// Canonical typed form of the command, e.g. "/help" or "/claude:help".
    pub canonical: String,
    pub description: String,
    /// Right-aligned provenance tag, e.g. "⟨spur⟩" or "⟨claude⟩". Empty for none.
    pub tag: String,
}

/// QuerySource for /slash completions. Holds a pre-computed `Vec<SlashRow>`
/// and a `prefix_start` captured at shell-open time (byte offset of the '/').
pub struct SlashQuerySource {
    rows: Vec<SlashRow>,
    matcher: Matcher,
    last_picked: Vec<SlashRow>,
    prefix_start: usize,
}

impl SlashQuerySource {
    pub fn new(rows: Vec<SlashRow>, prefix_start: usize) -> Self {
        Self {
            rows,
            matcher: Matcher::new(Config::DEFAULT),
            last_picked: Vec::new(),
            prefix_start,
        }
    }
}

impl QuerySource for SlashQuerySource {
    fn title(&self) -> &str {
        "Commands · /"
    }

    fn query_mode(&self) -> QueryMode {
        QueryMode::ReadFromInputBar
    }

    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> {
        let picked: Vec<SlashRow> = if query.is_empty() {
            self.rows.iter().take(20).cloned().collect()
        } else {
            let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, SlashRow)> = self
                .rows
                .iter()
                .filter_map(|r| {
                    buf.clear();
                    let score = pattern.score(
                        Utf32Str::new(&r.canonical, &mut buf),
                        &mut self.matcher,
                    )?;
                    Some((score, r.clone()))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            scored.into_iter().take(20).map(|(_, r)| r).collect()
        };
        let out: Vec<RetrievalRow> = picked
            .iter()
            .map(|r| RetrievalRow {
                primary: r.canonical.clone(),
                secondary: r.description.clone(),
                tag: r.tag.clone(),
                atoms: Vec::new(),
            })
            .collect();
        self.last_picked = picked;
        out
    }

    fn accept(&self, row_idx: usize) -> Option<RetrievalAccept> {
        let row = self.last_picked.get(row_idx)?;
        Some(RetrievalAccept::ReplaceTriggerToken {
            prefix_start: self.prefix_start,
            replacement: format!("{} ", row.canonical),
        })
    }
}
```

- [ ] **Step 4: Run tests — verify pass**

Run: `cargo test -p spur-tui --lib components::query_source`
Expected: 25 tests pass (20 from Task 1 + 5 new).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/query_source.rs
git commit -m "feat(spur-tui): add SlashQuerySource for Phase 3

New QuerySource impl backed by a pre-computed Vec<SlashRow>
snapshot (view hands over the snapshot at shell-open time;
source does not hold a live CommandRegistry reference). Accept
returns ReplaceTriggerToken { prefix_start, replacement:
'<canonical> ' } — same semantics as today's legacy
accept_completion arm for slash.

Part of: PickerShell Phase 3

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Rewire `SessionDetailView` to open `PickerShell` from trigger transitions

This is the behavior-change task. Replace the legacy `refresh_popup` / `accept_completion` / popup-open-routing with a single `PickerShell`-based path, while preserving every user-visible keybinding, label, and accept outcome.

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Modify: `crates/spur-tui/src/components/picker_shell.rs` (one `#[allow(dead_code)]` removal)

- [ ] **Step 1: Change `mention_registry` to `Rc<RefCell<MentionRegistry>>`**

In `crates/spur-tui/src/views/session_detail.rs`, find the struct field declaration:

```rust
    mention_registry: crate::mentions::MentionRegistry,
```

Replace with:

```rust
    mention_registry: std::rc::Rc<std::cell::RefCell<crate::mentions::MentionRegistry>>,
```

In `SessionDetailView::new`, find:

```rust
            mention_registry: crate::mentions::MentionRegistry::new(),
```

Replace with:

```rust
            mention_registry: std::rc::Rc::new(std::cell::RefCell::new(
                crate::mentions::MentionRegistry::new(),
            )),
```

- [ ] **Step 2: Delete `accept_completion`**

In `crates/spur-tui/src/views/session_detail.rs`, delete the entire `accept_completion` method (currently lines 648-679 — look for `fn accept_completion` and remove up to and including its closing brace).

- [ ] **Step 3: Delete `active_mention_hits` field + all its uses**

Remove the field declaration:

```rust
    active_mention_hits: Vec<crate::mentions::MentionEntry>,
```

Remove it from `SessionDetailView::new`:

```rust
            active_mention_hits: Vec::new(),
```

Any remaining references inside `refresh_popup` will be removed in the next step.

- [ ] **Step 4: Replace the body of `refresh_popup` with PickerShell-driven logic**

In `crates/spur-tui/src/views/session_detail.rs`, replace the entire `refresh_popup` method body:

```rust
    fn refresh_popup(&mut self) {
        use crate::components::completion_trigger::{detect, TriggerKind};
        use crate::components::picker_shell::PickerShell;
        use crate::components::query_source::{MentionQuerySource, SlashQuerySource, SlashRow};

        let text = self.input_bar.text();
        let cursor = self.input_bar.cursor();
        let new_trig = detect(&text, cursor);

        // Do NOT disturb a history-search shell (Ctrl+R) which is
        // identified by `active_trigger` being None while `picker_shell`
        // is Some. Only manage the trigger-driven shell here.
        let history_shell_active = self.picker_shell.is_some() && self.active_trigger.is_none();
        if history_shell_active {
            // Keep active_trigger in sync with current text (always None
            // here — history shell stole focus) and return.
            self.active_trigger = None;
            return;
        }

        match (&self.active_trigger, &new_trig) {
            (Some(old), Some(new))
                if old.kind == new.kind && old.prefix_start == new.prefix_start =>
            {
                // Same trigger, new query — just update the shell's query
                // from the InputBar trigger prefix.
                if let Some(shell) = self.picker_shell.as_mut() {
                    shell.set_query_from_input_bar(&new.query);
                }
                self.active_trigger = Some(new.clone());
            }
            (_, Some(new)) => {
                // Trigger transition (fresh open, or kind/prefix changed):
                // build the appropriate source and open a new shell.
                let shell = match new.kind {
                    TriggerKind::Slash => {
                        let entries = self.command_registry.list();
                        let rows: Vec<SlashRow> = entries
                            .iter()
                            .map(|e| SlashRow {
                                canonical: self.command_registry.canonical_typed_form(e),
                                description: e.description.clone(),
                                tag: match &e.source {
                                    crate::commands::CommandSource::Spur => "⟨spur⟩".into(),
                                    crate::commands::CommandSource::Agent { handle } => {
                                        format!("⟨{}⟩", handle)
                                    }
                                },
                            })
                            .collect();
                        let mut src = SlashQuerySource::new(rows, new.prefix_start);
                        let _ = src.refresh(&new.query);
                        PickerShell::open_with_query(Box::new(src), &new.query)
                    }
                    TriggerKind::Mention => {
                        let src = MentionQuerySource::new(
                            std::rc::Rc::clone(&self.mention_registry),
                            self.session_id.clone(),
                            self.cwd.clone(),
                            new.prefix_start,
                        );
                        PickerShell::open_with_query(Box::new(src), &new.query)
                    }
                };
                self.picker_shell = Some(shell);
                self.active_trigger = Some(new.clone());
            }
            (_, None) => {
                // No active trigger — close any trigger-driven shell.
                if self.active_trigger.is_some() {
                    self.picker_shell = None;
                }
                self.active_trigger = None;
            }
        }
    }
```

- [ ] **Step 5: Add `PickerShell::open_with_query` helper**

In `crates/spur-tui/src/components/picker_shell.rs`, add a sibling to `open` that accepts an initial query string for `ReadFromInputBar` sources. Remove the `#[allow(dead_code)]` from `set_query_from_input_bar` while you're in the file.

Find:

```rust
impl PickerShell {
    /// Open a shell over the given source. Immediately refreshes with an
    /// empty query to populate initial rows.
    pub fn open(mut source: Box<dyn QuerySource>) -> Self {
```

Add immediately below the existing `open`:

```rust
    /// Open a shell with an initial query (e.g. from an active trigger
    /// prefix). For `ReadFromInputBar` sources, installs `query` into the
    /// shell's internal `MiniInput` via `set_query_from_input_bar`. For
    /// `OwnedByShell` sources, uses `query` as the initial MiniInput text.
    pub fn open_with_query(source: Box<dyn QuerySource>, query: &str) -> Self {
        let mut shell = Self::open(source);
        if !query.is_empty() {
            // Use the existing set_query_from_input_bar path for trigger
            // sources; for OwnedByShell sources, fall back to pasting into
            // the MiniInput directly.
            if shell.source.query_mode() == QueryMode::ReadFromInputBar {
                shell.set_query_from_input_bar(query);
            } else {
                shell.query.paste(query);
                shell.rows = shell.source.refresh(shell.query.text());
                if !shell.rows.is_empty() {
                    shell.list_state.select(Some(0));
                }
            }
        }
        shell
    }
```

And find:

```rust
    /// For mention/slash (`QueryMode::ReadFromInputBar`). Not used by Phase 1
    /// but needed by the trait so Phase 3 can compile against this signature.
    #[allow(dead_code)]
    pub fn set_query_from_input_bar(&mut self, q: &str) {
```

Replace the attribute and doc with:

```rust
    /// For mention/slash (`QueryMode::ReadFromInputBar`). Called by the
    /// view on every InputBar text change so the shell's query mirrors
    /// the trigger prefix.
    pub fn set_query_from_input_bar(&mut self, q: &str) {
```

- [ ] **Step 6: Update the accept-dispatch arm to honor `InsertAtom.replace_from`**

In `crates/spur-tui/src/views/session_detail.rs`, find the existing `PickerAction::Accept` arm (added in Phase 1). The current `InsertAtom` branch is:

```rust
                        RetrievalAccept::InsertAtom { text, uri, name } => {
                            self.input_bar.insert_atom(text, uri, name);
                        }
```

Replace with:

```rust
                        RetrievalAccept::InsertAtom {
                            text,
                            uri,
                            name,
                            replace_from,
                        } => {
                            if let Some(prefix_start) = replace_from {
                                self.replace_trigger_token(prefix_start, "");
                            }
                            self.input_bar.insert_atom(text, uri, name);
                        }
```

And for the `ReplaceTriggerToken` branch, the existing inline byte-slice implementation is fine — but it duplicates `replace_trigger_token`. Replace the existing:

```rust
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
```

With:

```rust
                        RetrievalAccept::ReplaceTriggerToken {
                            prefix_start,
                            replacement,
                        } => {
                            self.replace_trigger_token(prefix_start, &replacement);
                        }
```

- [ ] **Step 7: Close any active trigger-driven shell when the PickerShell completes**

In the same `PickerAction::Accept` / `PickerAction::Cancel` arms, clear `active_trigger` alongside `picker_shell`. Replace:

```rust
                PickerAction::Cancel => {
                    self.picker_shell = None;
                }
```

With:

```rust
                PickerAction::Cancel => {
                    self.picker_shell = None;
                    self.active_trigger = None;
                }
```

And at the end of the `PickerAction::Accept` block, where `self.picker_shell = None;` appears, add `self.active_trigger = None;` on the next line.

- [ ] **Step 8: Remove the legacy popup-open key-routing block**

In `crates/spur-tui/src/views/session_detail.rs`, find the block starting with:

```rust
        // Priority 1.5: popup is open — route navigation/accept/dismiss keys.
        if self.popup_open() {
            match key.code {
```

Delete the entire `if self.popup_open() { match key.code { ... } }` block down through its closing `}`. All popup routing now flows through the `picker_shell` block earlier in `handle_key`.

- [ ] **Step 9: Remove `completion_popup` field and its render call**

In `crates/spur-tui/src/views/session_detail.rs`:

Delete the field:

```rust
    completion_popup: std::cell::RefCell<crate::components::completion_popup::CompletionPopup>,
```

Delete its init in `new`:

```rust
            completion_popup: std::cell::RefCell::new(
                crate::components::completion_popup::CompletionPopup::new(),
            ),
```

In the render method, delete the entire `if self.picker_shell.is_none() && self.popup_open() { self.completion_popup ... }` block — `PickerShell` now renders ALL popups.

Update `popup_open` to simply:

```rust
    fn popup_open(&self) -> bool {
        self.picker_shell.is_some()
    }
```

- [ ] **Step 10: Update the Ctrl+R guard**

In `crates/spur-tui/src/views/session_detail.rs`, find:

```rust
        if matches!(key.code, KeyCode::Char('r'))
            && (key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT))
            && self.active_trigger.is_none()
        {
```

Replace the `self.active_trigger.is_none()` check with `self.picker_shell.is_none()` — once a mention/slash shell is open, Ctrl+R should be rejected (user must Esc first):

```rust
        if matches!(key.code, KeyCode::Char('r'))
            && (key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT))
            && self.picker_shell.is_none()
        {
```

- [ ] **Step 11: Build — resolve compile errors**

Run: `cargo build -p spur-tui 2>&1 | head -60`
Expected: zero errors. Any lingering `self.completion_popup` references should surface; fix them by removing the referencing code (e.g. old select_prev / select_next calls that were in the deleted popup-routing block).

- [ ] **Step 12: Run existing tests**

Run: `cargo test -p spur-tui`
Expected: all tests pass. The existing `session_detail_commands_integration.rs` tests (`plain_text_submit_produces_text_block`, `slash_help_fires_show_help_action`, `ctrl_r_history_restore_preserves_resource_links`) MUST remain green — they exercise the exact paths we just rewired.

If `slash_help_fires_show_help_action` fails, the most likely cause is the PickerShell's key routing not returning the accept action up through `handle_key`. Check that the Accept arm in session_detail.rs returns `None` for mention/slash accept (neither produces an `Action`; they mutate InputBar instead) AND continues the key-event flow so that the second Enter submits the InputBar text.

- [ ] **Step 13: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs crates/spur-tui/src/components/picker_shell.rs
git commit -m "refactor(spur-tui): route @mention and /slash through PickerShell (Phase 3)

SessionDetailView's refresh_popup now opens/updates/closes a single
PickerShell on every completion_trigger::detect() transition.
@mention uses MentionQuerySource (Rc<RefCell<MentionRegistry>>),
/slash uses SlashQuerySource (pre-computed CommandRegistry snapshot).
Accept payloads flow through the existing PickerAction::Accept arm:
RetrievalAccept::InsertAtom honors a new replace_from field;
ReplaceTriggerToken delegates to the existing replace_trigger_token
helper (deduplicates the inline byte-slice code).

Removed: accept_completion (dead), active_mention_hits (dead),
completion_popup field + render block (dead — PickerShell renders
its own popup), the Priority-1.5 popup-open key-routing block
(dead — PickerShell.handle_key handles all popup keys). active_trigger
is kept as a companion to picker_shell: it carries prefix_start/kind
across keystrokes so refresh_popup can detect kind/position transitions.

PickerShell::open_with_query added for opening a shell with an
initial query (e.g. from an existing trigger prefix). dead_code
attribute removed from set_query_from_input_bar.

mention_registry field type changed from MentionRegistry to
Rc<RefCell<MentionRegistry>> so MentionQuerySource can hold a
cheap shared handle.

Part of: PickerShell Phase 3

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Parity integration tests for mention + slash through PickerShell

Lock the end-to-end behavior so Phase 3's refactor cannot regress today's mention/slash UX silently.

**Files:**
- Create: `crates/spur-tui/tests/picker_shell_trigger_parity.rs`

- [ ] **Step 1: Write the integration test file**

Create `crates/spur-tui/tests/picker_shell_trigger_parity.rs`:

```rust
//! Integration: @mention and /slash popups route through PickerShell but
//! preserve pre-Phase-3 user-visible behavior:
//!   * Typing `/he` + Enter + Enter dispatches ShowHelp (today's test:
//!     slash_help_fires_show_help_action).
//!   * Typing `@` opens a PickerShell with MentionQuerySource.
//!   * Tab with a selected mention row inserts a ResourceLink via
//!     insert_atom and drops the `@query` prefix.
//!   * Esc closes the shell without mutating the InputBar trigger prefix
//!     (the typed `@foo` stays as literal text).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::ContentBlock;
use spur_tui::action::Action;
use spur_tui::views::{session_detail::SessionDetailView, View};

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(|| spur_core::lineage::projection::ExecutorLineage::new());
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn mk_view_in_cwd(cwd: std::path::PathBuf) -> SessionDetailView {
    SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "brain".into(),
        cwd,
        spur_tui::test_support::default_agent_config("claude"),
    )
}

fn press(v: &mut SessionDetailView, code: KeyCode) -> Option<Action> {
    v.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &test_ctx())
}

fn type_str(v: &mut SessionDetailView, s: &str) {
    for c in s.chars() {
        press(v, KeyCode::Char(c));
    }
}

#[test]
fn slash_help_via_picker_shell_dispatches_show_help() {
    let tmp = tempfile::tempdir().unwrap();
    let mut v = mk_view_in_cwd(tmp.path().to_path_buf());

    // Type '/' → opens slash PickerShell, first row = /help (spur-local).
    type_str(&mut v, "/");
    // Enter → accept selected row, replaces '/' with '/help '
    let _ = press(&mut v, KeyCode::Enter);
    // Second Enter → submit '/help ' → ShowHelp action.
    let act = press(&mut v, KeyCode::Enter);
    assert!(
        matches!(act, Some(Action::ShowHelp)),
        "expected Some(Action::ShowHelp), got {:?}",
        act
    );
}

#[test]
fn mention_tab_inserts_resource_link_on_submit() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("NOTES.md"), "x").unwrap();
    let mut v = mk_view_in_cwd(tmp.path().to_path_buf());

    // Type "@NOT" — opens mention PickerShell; source queries registry.
    type_str(&mut v, "@NOT");
    // Tab → accept mention row; prefix '@NOT' cleared, protected atom inserted.
    let _ = press(&mut v, KeyCode::Tab);
    // Enter → submit; outbound blocks should include a ResourceLink.
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    match act {
        Action::SendMessage { blocks, .. } => {
            assert!(
                blocks.iter().any(|b| matches!(b, ContentBlock::ResourceLink(_))),
                "expected a ResourceLink in outbound blocks, got {:?}",
                blocks
            );
        }
        other => panic!("expected SendMessage, got {:?}", other),
    }
}

#[test]
fn mention_esc_leaves_typed_at_query_literal() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("NOTES.md"), "x").unwrap();
    let mut v = mk_view_in_cwd(tmp.path().to_path_buf());

    type_str(&mut v, "@NOT");
    let _ = press(&mut v, KeyCode::Esc);
    // After Esc, typed '@NOT' stays; submit carries it as plain text.
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    match act {
        Action::SendMessage { blocks, .. } => {
            // No ResourceLink (never accepted); '@NOT' is in the text block.
            assert!(
                !blocks.iter().any(|b| matches!(b, ContentBlock::ResourceLink(_))),
                "did not expect a ResourceLink after Esc, got {:?}",
                blocks
            );
            let text_concat: String = blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect();
            assert!(
                text_concat.contains("@NOT"),
                "expected '@NOT' literal in outbound text, got {:?}",
                text_concat
            );
        }
        other => panic!("expected SendMessage, got {:?}", other),
    }
}

#[test]
fn typing_space_after_at_closes_mention_shell() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("NOTES.md"), "x").unwrap();
    let mut v = mk_view_in_cwd(tmp.path().to_path_buf());

    type_str(&mut v, "@NOT");
    // Space terminates the mention trigger; detect() returns None; shell closes.
    press(&mut v, KeyCode::Char(' '));
    // Subsequent Enter submits the literal text including '@NOT '.
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    match act {
        Action::SendMessage { blocks, .. } => {
            let text_concat: String = blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect();
            assert!(text_concat.contains("@NOT "), "got {:?}", text_concat);
        }
        other => panic!("expected SendMessage, got {:?}", other),
    }
}
```

- [ ] **Step 2: Run the integration tests**

Run: `cargo test -p spur-tui --test picker_shell_trigger_parity`
Expected: 4 passed. If the mention tests fail because `MentionRegistry`'s filesystem query didn't find `NOTES.md` fast enough (caching quirk), bump the assertion to check for any `ResourceLink` rather than a specific name, and document the adjustment in a comment.

- [ ] **Step 3: Run the full spur-tui test suite**

Run: `cargo test -p spur-tui`
Expected: entire spur-tui suite green — no regressions in Phase 1 / Phase 2 tests, no regressions in pre-existing session_detail tests.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/tests/picker_shell_trigger_parity.rs
git commit -m "test(spur-tui): Phase 3 parity integration tests for mention + slash

Four scenarios locking user-visible behavior of the mention and
slash triggers now that they route through PickerShell:
  * /help via slash → ShowHelp action on second Enter.
  * @NOT → Tab accept → Enter → ResourceLink in outbound blocks.
  * @NOT → Esc → Enter → '@NOT' stays as literal text, no ResourceLink.
  * @NOT + space → trigger closes; submit includes '@NOT ' verbatim.

Part of: PickerShell Phase 3

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>"
```

---

## Final: Phase 3 exit verification

- [ ] **Step 1: Release build**

Run: `cargo build -p spur-tui --release`
Expected: no errors.

- [ ] **Step 2: Full spur-tui test suite**

Run: `cargo test -p spur-tui`
Expected: all tests pass.

- [ ] **Step 3: Workspace-wide build**

Run: `cargo build`
Expected: no errors.

- [ ] **Step 4: Workspace clippy (for spur-tui only; full workspace may still trip on pre-existing spur-core lint)**

Run: `cargo clippy -p spur-tui --all-targets -- -D warnings`
Expected: exit 0.

- [ ] **Step 5: Manual smoke** (optional; user will do this)

In a running `spur watch` session:
- Type `/`: popup opens above InputBar with `Commands · /` header. Typing letters filters.
- Tab accepts the selected row and replaces `/…` with `/<canonical> `.
- Esc closes the popup; typed `/foo` stays as literal text.
- Type `@`: popup opens with `Mentions · @` header, first rows are files in cwd.
- Tab accepts; the `@query` prefix is cleared and a LightBlue+underlined atom appears.
- Typing space after `@foo` closes the popup; `@foo ` stays in the draft.
- Ctrl+R while the mention popup is open is rejected (no-op, must Esc first).

---

## Self-review results

**Spec coverage (Phase 3 section):**
- ✓ "Implement MentionQuerySource and SlashQuerySource (QueryMode::ReadFromInputBar)" — Task 1, Task 2.
- ✓ "When completion_trigger::detect() returns Some(trig), SessionDetailView opens PickerShell::open(...) with the matching source and calls shell.set_query_from_input_bar(&trig.query) on every InputBar text change" — Task 3 Step 4 (the `(Some(old), Some(new)) if …` arm calls `set_query_from_input_bar`; trigger transitions open a fresh shell).
- ✓ "MentionQuerySource::accept → RetrievalAccept::InsertAtom { text, uri, name }" — Task 1 Step 4 (plus `replace_from: Some(prefix_start)`).
- ✓ "SlashQuerySource::accept → RetrievalAccept::ReplaceTriggerToken { prefix_start, replacement }" — Task 2 Step 3.
- ✓ "session_detail.rs::accept_completion deleted" — Task 3 Step 2.
- ✓ "SessionDetailView::active_trigger replaced by an Option<PickerShell>" — interpreted in the pragmatic sense: the trigger-state drives `picker_shell`, while `active_trigger` is retained as a metadata carrier (prefix_start, kind) so `refresh_popup` can detect kind/position transitions. The spec's intent (no dual-source-of-truth on popup open/close) is preserved because `popup_open()` now consults only `picker_shell`.
- ✓ "Integration test parity: mention-insert-atom and slash-replace-token behavior match pre-migration" — Task 4.
- ✓ "All three trigger kinds (history, mention, slash) route through the same PickerShell instance (one at a time)" — Task 3 Step 10 (Ctrl+R guard checks `picker_shell.is_none()`).

**Placeholder scan.** Every code step shows complete code. Task 3 Steps 5-6 quote existing code exactly before replacing — no "similar to" references.

**Type consistency:**
- `MentionQuerySource::new(Rc<RefCell<MentionRegistry>>, SessionId, PathBuf, usize)` — consistent across Tasks 1, 3.
- `SlashQuerySource::new(Vec<SlashRow>, usize)` — consistent across Tasks 2, 3.
- `SlashRow { canonical, description, tag }` — consistent across Tasks 2, 3.
- `RetrievalAccept::InsertAtom { text, uri, name, replace_from }` — consistent across Tasks 1 (source), 3 (view dispatch).
- `PickerShell::{open, open_with_query, set_query_from_input_bar, handle_key, render}` — consistent across Tasks 3, 4.

**Known adjustments the worker may need:**
1. If `MentionRegistry::query`'s FileMentionSource is slow or cache-miss in tests, the `make_mention_registry_with_cwd` helper may need a `.with_eager_indexing()` method or equivalent pre-warm — use the existing registry API as-is and relax assertions if needed.
2. If the `refresh_popup` logic's state-machine has a subtle hole (e.g. slash trigger opens but subsequent keystroke erroneously re-opens instead of updating), diagnose by adding a `dbg!` on `(old, new, picker_shell.is_some())` and walk through the cases — do NOT change the state-machine shape; the arms are chosen so that `(Some, None)` always closes, `(_, Some(new))` with different kind/prefix_start always reopens, and `(Some, Some)` with matching kind+prefix_start always updates.
3. The `CommandEntry` struct's exact fields are used in Task 3 Step 4 (`e.description`, `e.source`). If the struct's shape has drifted since this plan was written, adapt the field access — the intent is to carry "description, provenance tag" forward.

No gaps found. Plan ready for execution.
