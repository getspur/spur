# PickerShell Phase 4 — Retire `active_trigger` via TriggerDetector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Retire `SessionDetailView::active_trigger` by encapsulating the trigger transition state machine (open / update / close / no-op) inside a new `TriggerDetector` type in `completion_trigger.rs`. Add `PickerShell::query_mode()` so the key-routing branch can distinguish trigger-driven from history shells without consulting view-owned state. No user-visible change; existing Phase 1/2/3 tests are the regression guard.

**Architecture:** One new type (`TriggerDetector`) and one new enum (`TriggerTransition`) in `completion_trigger.rs`. One new public accessor (`query_mode`) on `PickerShell`. One rewiring pass in `session_detail.rs`: remove `active_trigger` field + all 6 mutation sites, replace `refresh_popup`'s three-arm match with a `trigger_detector.step(...) -> TriggerTransition` call, replace the key-routing `is_trigger_driven = self.active_trigger.is_some()` check with `shell.query_mode() == QueryMode::ReadFromInputBar`, reset the detector on Accept/Cancel. Source construction stays in `SessionDetailView` because the registries are view-owned; the spec's "factory" wording is realized in spirit (the detector fully owns the transition state machine) without over-coupling the detector to view-internal registries.

**Tech Stack:** Rust 2021, no new dependencies.

**Spec:** `docs/superpowers/specs/2026-04-19-picker-shell-retrieval-unification-design.md` (Phase 4 section).

---

## File Structure

**Modify:**
- `crates/spur-tui/src/components/completion_trigger.rs` — add `TriggerDetector` struct, `TriggerTransition` enum, unit tests. Existing `detect()` function and `Trigger` struct unchanged.
- `crates/spur-tui/src/components/picker_shell.rs` — add one-line `query_mode()` public accessor returning `QueryMode`.
- `crates/spur-tui/src/views/session_detail.rs` — remove `active_trigger` field and all 6 use sites. Add `trigger_detector: TriggerDetector` field. Replace `refresh_popup` body. Replace key-routing `is_trigger_driven` check. Reset detector on Accept/Cancel.

**Unchanged:**
- `crates/spur-tui/src/components/{mini_input,query_source,input_bar,completion_popup}.rs`
- `crates/spur-tui/src/components/picker_shell.rs` behavior (only public API addition)
- `crates/spur-tui/src/input_history.rs`
- `crates/spur-tui/src/mentions/`, `crates/spur-tui/src/commands/`
- All test files — the existing Phase 1/2/3 test suites are the regression guard.

---

## Task 1: `TriggerDetector` + `TriggerTransition` in `completion_trigger.rs`

Stateful detector that wraps the existing pure `detect()` function. Tracks the last-seen trigger and emits a transition enum describing what the view should do with its `picker_shell`.

**Files:**
- Modify: `crates/spur-tui/src/components/completion_trigger.rs`

- [ ] **Step 1: Write the failing tests**

Append to the end of `crates/spur-tui/src/components/completion_trigger.rs`:

```rust
#[cfg(test)]
mod detector_tests {
    use super::*;

    #[test]
    fn detector_starts_with_no_trigger() {
        let mut d = TriggerDetector::new();
        let t = d.step("", 0);
        assert!(matches!(t, TriggerTransition::None));
    }

    #[test]
    fn detector_reports_open_on_first_trigger_appearance() {
        let mut d = TriggerDetector::new();
        let t = d.step("@", 1);
        match t {
            TriggerTransition::Open { trigger } => {
                assert_eq!(trigger.kind, TriggerKind::Mention);
                assert_eq!(trigger.prefix_start, 0);
                assert_eq!(trigger.query, "");
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn detector_reports_update_when_query_changes_same_trigger() {
        let mut d = TriggerDetector::new();
        let _ = d.step("@", 1);
        let t = d.step("@f", 2);
        match t {
            TriggerTransition::Update { query } => assert_eq!(query, "f"),
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn detector_reports_close_when_trigger_goes_away() {
        let mut d = TriggerDetector::new();
        let _ = d.step("@foo", 4);
        let t = d.step("@foo ", 5); // whitespace terminates mention
        assert!(matches!(t, TriggerTransition::Close));
    }

    #[test]
    fn detector_reports_open_on_kind_change_even_if_position_matches() {
        // '/help' at offset 0, then user deletes and types '@': position
        // happens to still be 0 but kind flipped from Slash to Mention —
        // this MUST be an Open (fresh shell), not an Update.
        let mut d = TriggerDetector::new();
        let _ = d.step("/", 1);
        let t = d.step("@", 1);
        match t {
            TriggerTransition::Open { trigger } => {
                assert_eq!(trigger.kind, TriggerKind::Mention);
            }
            other => panic!("expected Open on kind change, got {other:?}"),
        }
    }

    #[test]
    fn detector_reports_open_on_prefix_start_change() {
        // Mention at offset 0, then a leading space + new mention at offset 1.
        let mut d = TriggerDetector::new();
        let _ = d.step("@foo", 4);
        // After moving to a different mention trigger (e.g. " @bar"):
        let t = d.step(" @bar", 5);
        match t {
            TriggerTransition::Open { trigger } => {
                assert_eq!(trigger.prefix_start, 1);
            }
            other => panic!("expected Open on prefix_start change, got {other:?}"),
        }
    }

    #[test]
    fn detector_reports_none_when_neither_last_nor_current_has_trigger() {
        let mut d = TriggerDetector::new();
        let _ = d.step("hello", 5);
        let t = d.step("hello world", 11);
        assert!(matches!(t, TriggerTransition::None));
    }

    #[test]
    fn detector_reset_clears_last_trigger_so_next_step_reports_open() {
        let mut d = TriggerDetector::new();
        let _ = d.step("@foo", 4);
        d.reset();
        // Without reset this would be Update; after reset the detector
        // treats @foo as a fresh appearance.
        let t = d.step("@foo", 4);
        assert!(matches!(t, TriggerTransition::Open { .. }));
    }
}
```

- [ ] **Step 2: Run tests — verify failure**

Run: `cargo test -p spur-tui --lib components::completion_trigger::detector_tests`
Expected: compile error — `TriggerDetector` and `TriggerTransition` not found.

- [ ] **Step 3: Implement `TriggerDetector` and `TriggerTransition`**

Append to `crates/spur-tui/src/components/completion_trigger.rs` (above the existing `#[cfg(test)]` module if present, or at end-of-file before the new `detector_tests` module):

```rust
/// Transition emitted by `TriggerDetector::step` describing what the view
/// should do with its active `PickerShell`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerTransition {
    /// No change — neither the previous nor current input state had a
    /// trigger. The view should do nothing to its picker_shell (it may
    /// be holding a history shell, which this detector does not manage).
    None,
    /// A new trigger appeared (either first-ever or a change in kind /
    /// prefix_start from the last trigger). The view should open a fresh
    /// PickerShell with a source matching `trigger.kind`.
    Open { trigger: Trigger },
    /// The trigger's kind and prefix_start match the last step's trigger;
    /// only the query text changed. The view should forward `query` to the
    /// existing shell via `set_query_from_input_bar`.
    Update { query: String },
    /// The last step had a trigger; the current step does not. The view
    /// should close its trigger-driven PickerShell.
    Close,
}

/// Stateful wrapper over `detect()`. Remembers the last-emitted trigger so
/// consecutive `step` calls can classify transitions. History-mode shells
/// (`Ctrl+R`, `QueryMode::OwnedByShell`) are NOT managed by this detector —
/// the view checks shell mode before calling `step`, and the detector only
/// produces transitions for trigger-driven (`QueryMode::ReadFromInputBar`)
/// popups.
#[derive(Debug, Default)]
pub struct TriggerDetector {
    last: Option<Trigger>,
}

impl TriggerDetector {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Feed current (text, cursor). Returns a transition describing what
    /// should happen to the trigger-driven shell. Callers invoke this after
    /// every InputBar text change.
    pub fn step(&mut self, text: &str, cursor: usize) -> TriggerTransition {
        let new = detect(text, cursor);
        let transition = match (&self.last, &new) {
            (None, None) => TriggerTransition::None,
            (Some(old), Some(new_t))
                if old.kind == new_t.kind && old.prefix_start == new_t.prefix_start =>
            {
                TriggerTransition::Update {
                    query: new_t.query.clone(),
                }
            }
            (_, Some(new_t)) => TriggerTransition::Open {
                trigger: new_t.clone(),
            },
            (Some(_), None) => TriggerTransition::Close,
        };
        self.last = new;
        transition
    }

    /// Reset the detector's memory of the last trigger. Call after the view
    /// accepts or cancels a trigger-driven shell, so the next `step` treats
    /// a re-appearing trigger as a fresh Open rather than a spurious Update.
    pub fn reset(&mut self) {
        self.last = None;
    }
}
```

- [ ] **Step 4: Run tests — verify pass**

Run: `cargo test -p spur-tui --lib components::completion_trigger`
Expected: `test result: ok.` — existing `detect()` tests plus 8 new detector tests, all green.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/completion_trigger.rs
git commit -m "feat(spur-tui): add TriggerDetector + TriggerTransition (Phase 4)

Stateful wrapper over the pure detect() function. TriggerDetector
remembers the last trigger so consecutive step() calls classify
transitions as None / Open { trigger } / Update { query } / Close.
reset() clears memory after accept/cancel so a re-appearing trigger
is treated as a fresh Open.

Eight unit tests cover every transition path including the two
subtle ones: kind change at the same prefix_start (Open, not
Update), and reset() making a repeated Open possible.

Part of: PickerShell Phase 4

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: `PickerShell::query_mode()` public accessor

Expose the underlying source's query mode so the view's key-routing branch can distinguish trigger-driven (`ReadFromInputBar`) from history (`OwnedByShell`) shells without needing to maintain a parallel `active_trigger` field.

**Files:**
- Modify: `crates/spur-tui/src/components/picker_shell.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/src/components/picker_shell.rs` inside the existing `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn query_mode_accessor_matches_underlying_source() {
        use crate::components::query_source::QueryMode;
        let hist_src = HistoryQuerySource::new(vec![mk("a")]);
        let shell = PickerShell::open(Box::new(hist_src));
        assert_eq!(shell.query_mode(), QueryMode::OwnedByShell);
    }
```

- [ ] **Step 2: Run tests — verify failure**

Run: `cargo test -p spur-tui --lib components::picker_shell::tests::query_mode_accessor`
Expected: compile error — no method `query_mode` on `PickerShell`.

- [ ] **Step 3: Add the accessor**

In `crates/spur-tui/src/components/picker_shell.rs`, find the end of the public-API section (after `set_query_from_input_bar` is a good spot). Add:

```rust
    /// The underlying source's query mode. Used by the view's key-routing
    /// branch to distinguish trigger-driven (`ReadFromInputBar`) shells
    /// from history (`OwnedByShell`) shells without maintaining a parallel
    /// `active_trigger` field.
    pub fn query_mode(&self) -> crate::components::query_source::QueryMode {
        self.source.query_mode()
    }
```

- [ ] **Step 4: Run tests — verify pass**

Run: `cargo test -p spur-tui --lib components::picker_shell`
Expected: all previous tests + 1 new test pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/picker_shell.rs
git commit -m "feat(spur-tui): add PickerShell::query_mode accessor (Phase 4)

Public one-liner exposing self.source.query_mode() so the view can
distinguish trigger-driven (ReadFromInputBar) shells from history
(OwnedByShell) shells without consulting its own active_trigger
field. Sets up Phase 4's retirement of active_trigger.

Part of: PickerShell Phase 4

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Retire `active_trigger` from `SessionDetailView`

Replace the field with `trigger_detector: TriggerDetector`. Rewrite `refresh_popup` to dispatch on `TriggerTransition`. Rewrite key-routing to check `shell.query_mode()` instead of `active_trigger.is_some()`. Reset the detector on Accept/Cancel.

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Swap the `active_trigger` field for `trigger_detector`**

In `crates/spur-tui/src/views/session_detail.rs`, find:

```rust
    /// Currently active popup trigger (if any), derived from the InputBar
    /// text + cursor.
    active_trigger: Option<crate::components::completion_trigger::Trigger>,
```

Replace with:

```rust
    /// Stateful trigger-transition detector. Replaces the former
    /// `active_trigger` field (retired in Phase 4). History shells are
    /// not managed through this detector; see refresh_popup.
    trigger_detector: crate::components::completion_trigger::TriggerDetector,
```

In `SessionDetailView::new`, find:

```rust
            active_trigger: None,
```

Replace with:

```rust
            trigger_detector: crate::components::completion_trigger::TriggerDetector::new(),
```

- [ ] **Step 2: Rewrite the body of `refresh_popup`**

In the same file, find the `refresh_popup` method. Replace its entire body with:

```rust
    fn refresh_popup(&mut self) {
        use crate::components::completion_trigger::{TriggerKind, TriggerTransition};
        use crate::components::picker_shell::PickerShell;
        use crate::components::query_source::{
            MentionQuerySource, QueryMode, SlashQuerySource, SlashRow,
        };

        let text = self.input_bar.text();
        let cursor = self.input_bar.cursor();

        // If a history shell (OwnedByShell) is open, it steals focus from
        // the trigger-driven state machine. Do NOT feed the detector while
        // that's true, and ensure any previous trigger state is cleared.
        if let Some(shell) = self.picker_shell.as_ref() {
            if shell.query_mode() == QueryMode::OwnedByShell {
                self.trigger_detector.reset();
                return;
            }
        }

        let transition = self.trigger_detector.step(&text, cursor);
        match transition {
            TriggerTransition::None => {}
            TriggerTransition::Update { query } => {
                if let Some(shell) = self.picker_shell.as_mut() {
                    shell.set_query_from_input_bar(&query);
                }
            }
            TriggerTransition::Open { trigger } => {
                let shell = match trigger.kind {
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
                        let src = SlashQuerySource::new(rows, trigger.prefix_start);
                        PickerShell::open_with_query(Box::new(src), &trigger.query)
                    }
                    TriggerKind::Mention => {
                        let src = MentionQuerySource::new(
                            std::rc::Rc::clone(&self.mention_registry),
                            self.session_id.clone(),
                            self.cwd.clone(),
                            trigger.prefix_start,
                        );
                        PickerShell::open_with_query(Box::new(src), &trigger.query)
                    }
                };
                self.picker_shell = Some(shell);
            }
            TriggerTransition::Close => {
                // Close the trigger-driven shell. The detector has already
                // cleared its last-trigger state inside step().
                self.picker_shell = None;
            }
        }
    }
```

- [ ] **Step 3: Rewrite the key-routing branch**

Find the Priority-1.4 picker-shell block. The current code is:

```rust
        if self.picker_shell.is_some() {
            let is_trigger_driven = self.active_trigger.is_some();
            let shell_consumes = if is_trigger_driven {
                matches!(
                    key.code,
                    KeyCode::Up | KeyCode::Down | KeyCode::Esc | KeyCode::Tab | KeyCode::Enter
                ) || (key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL))
            } else {
                true
            };
```

Replace with:

```rust
        if self.picker_shell.is_some() {
            use crate::components::query_source::QueryMode;
            let shell_mode = self
                .picker_shell
                .as_ref()
                .map(|s| s.query_mode())
                .expect("is_some checked");
            let is_trigger_driven = shell_mode == QueryMode::ReadFromInputBar;
            let shell_consumes = if is_trigger_driven {
                matches!(
                    key.code,
                    KeyCode::Up | KeyCode::Down | KeyCode::Esc | KeyCode::Tab | KeyCode::Enter
                ) || (key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL))
            } else {
                true
            };
```

- [ ] **Step 4: Update Accept/Cancel arms to reset the detector**

Find the `PickerAction::Cancel` arm inside the same block:

```rust
                    PickerAction::Cancel => {
                        self.picker_shell = None;
                        self.active_trigger = None;
                    }
```

Replace with:

```rust
                    PickerAction::Cancel => {
                        self.picker_shell = None;
                        self.trigger_detector.reset();
                    }
```

Find the end of the `PickerAction::Accept` arm, where it does `self.picker_shell = None; self.active_trigger = None;`. Replace those two lines with:

```rust
                        self.picker_shell = None;
                        self.trigger_detector.reset();
```

- [ ] **Step 5: Build — catch any lingering `active_trigger` references**

Run: `cargo build -p spur-tui 2>&1 | grep -E 'active_trigger' | head -20`
Expected: no output. If there are remaining references (e.g. inside `#[cfg(test)]` blocks or debug helpers), fix them: typically by removing or replacing with `self.picker_shell.is_some()` if that's what the check meant.

Final grep sanity:

```bash
grep -rn 'active_trigger' crates/spur-tui/src/
```

Expected: zero matches.

- [ ] **Step 6: Run the existing test suite**

Run: `cargo test -p spur-tui`
Expected: full suite green, including:
- `session_detail_commands_integration` (critical regression guard — `slash_help_fires_show_help_action`, `ctrl_r_history_restore_preserves_resource_links`)
- `picker_shell_ctrl_r` (Phase 1 history path)
- `picker_shell_atom_render` (Phase 2 rendering)
- `picker_shell_trigger_parity` (Phase 3 mention/slash parity — this is the one most likely to surface a Phase 4 regression since it exercises the exact open/update/close transitions)

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "refactor(spur-tui): retire active_trigger via TriggerDetector (Phase 4)

SessionDetailView no longer carries an active_trigger: Option<Trigger>
field. The trigger-transition state machine (open / update / close)
moves behind a new trigger_detector: TriggerDetector field. The
key-routing is_trigger_driven check now consults shell.query_mode()
instead of active_trigger.is_some() — fewer fields of truth about
the popup's current kind.

refresh_popup dispatches on TriggerTransition::{None, Update, Open,
Close}. History shells (QueryMode::OwnedByShell) short-circuit the
detector and reset its memory, preserving the Phase 1 isolation
between Ctrl+R and trigger popups. Accept/Cancel arms now call
trigger_detector.reset() instead of clearing active_trigger.

Zero behavior change — all Phase 1/2/3 tests pass unchanged.

Part of: PickerShell Phase 4

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>"
```

---

## Final: Phase 4 exit verification

- [ ] **Step 1: Grep `active_trigger` across spur-tui source**

Run:

```bash
grep -rn 'active_trigger' crates/spur-tui/src/
```

Expected: zero matches.

- [ ] **Step 2: Release build**

Run: `cargo build -p spur-tui --release`
Expected: no errors.

- [ ] **Step 3: Full spur-tui test suite**

Run: `cargo test -p spur-tui`
Expected: all tests pass.

- [ ] **Step 4: Workspace build**

Run: `cargo build`
Expected: no errors.

- [ ] **Step 5: spur-tui clippy**

Run: `cargo clippy -p spur-tui --all-targets -- -D warnings`
Expected: no new warnings from Phase 4 code. Pre-existing warnings in other spur-tui files (flagged during Phase 3) may still be present; they are out of scope for this plan.

---

## Self-review results

**Spec coverage (Phase 4 section):**
- ✓ "completion_trigger::detect() becomes a TriggerDetector that emits a ... factory when a trigger is found" — interpreted pragmatically: `TriggerDetector` wraps `detect()` and emits `TriggerTransition::Open { trigger }`. Source construction stays in `SessionDetailView` because mention/command registries are view-owned; moving source factories behind the detector would tightly couple the component library to view-internal state for little payoff. The spirit of the exit criterion (no dual-source-of-truth on trigger state) is honored.
- ✓ "Grep for active_trigger returns no hits outside picker_shell.rs" — interpreted as "active_trigger retires from SessionDetailView", validated by the zero-match grep in Final Step 1. (The spec's literal wording "outside picker_shell.rs" is questionable — picker_shell.rs does not and should not contain active_trigger — so this is the actionable form.)
- ✓ "completion_trigger.rs is pure detection — no popup wiring" — after Task 1, `completion_trigger.rs` contains `Trigger`, `TriggerKind`, `detect()`, `TriggerDetector`, `TriggerTransition`. None of these reference `PickerShell`, `CompletionPopup`, or any popup widget. Detection + state machine only.

**Placeholder scan.** Every code step shows the complete replacement text. Task 3 Step 1 quotes the exact text to replace before showing the replacement — no "similar to" references.

**Type consistency:**
- `TriggerDetector::{new, step, reset}` consistent across Tasks 1, 3.
- `TriggerTransition::{None, Open { trigger }, Update { query }, Close}` consistent across Tasks 1, 3.
- `PickerShell::query_mode(&self) -> QueryMode` consistent across Tasks 2, 3.
- `QueryMode::{OwnedByShell, ReadFromInputBar}` matches the existing enum in `query_source.rs` — no shape change.

**Risk check.** The one subtle behavior in the new `refresh_popup` is the "history shell short-circuit" guard at the top. Historically, Phase 3's `refresh_popup` ended early with `if history_shell_active { self.active_trigger = None; return; }`. The Phase 4 equivalent is `if shell.query_mode() == OwnedByShell { self.trigger_detector.reset(); return; }`. Both achieve the same invariant: while Ctrl+R's history shell is open, trigger detection is suspended and any prior trigger state is cleared. Parity test `ctrl_r_history_restore_preserves_resource_links` is the regression guard for this path.

No gaps. Plan ready for execution.
