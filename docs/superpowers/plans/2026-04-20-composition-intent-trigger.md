# Composition-Intent Trigger Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the text-only `TriggerDetector` with an event-driven Composition-Intent state machine so cursor motion through committed atoms, paste, history-recall, and selection-drag no longer pop the `PickerShell` open.

**Architecture:** `InputBar::handle_key` returns `HandleOutcome { Submit(..) | Key(IntentEvent) }`; the view consumes the outcome and emits non-key intents (`Pasted`, `SetText`, `Accepted`, `Dismissed`, `Submitted`) at their call sites. `TriggerDetector::step(event, text, cursor, ranges)` is the new pure state machine. The view's `dispatch_intent` helper has an O(1) fast-path that returns without allocating when the detector is `Idle` and the event is non-opening.

**Tech Stack:** Rust 2024 edition, `tui_textarea`, `ratatui`, `crossterm`. No new external dependencies.

**Spec:** `docs/superpowers/specs/2026-04-20-composition-intent-trigger-design.md`

---

## File Structure

### Files modified

| File | Responsibility after this change |
|---|---|
| `crates/spur-tui/src/components/completion_trigger.rs` | Owns `IntentEvent`, `TriggerState`, event-driven `TriggerDetector::step`. Keeps `Trigger`, `TriggerKind`, `TriggerTransition` as public types. The free `detect()` function and its tests are deleted. |
| `crates/spur-tui/src/components/input_bar.rs` | `handle_key` now returns `HandleOutcome`. Every match branch classifies into `IntentEvent`. Existing behavior unchanged. |
| `crates/spur-tui/src/views/session_detail.rs` | `refresh_popup()` deleted. New `dispatch_intent(event)` helper. Consumers of `handle_key` switch to `match outcome`. Non-key edit paths (`insert_paste`, `set_text`, `set_state`, `insert_atom`, picker accept/cancel, history swap, submit) get explicit `dispatch_intent(...)` calls. |
| `crates/spur-tui/src/views/dashboard.rs` | The one `handle_key` caller there updates its match. No new plumbing (that view has no picker). |

### Files created

| File | Responsibility |
|---|---|
| `crates/spur-tui/tests/composition_intent_integration.rs` | End-to-end integration test verifying the PickerShell never opens on `MovedCursor` / `Pasted` / `SetText` events when atoms or stray `@`s are present. |

### Files deleted

| File | Reason |
|---|---|
| `crates/spur-tui/tests/completion_trigger.rs` | Exercises the removed `detect(text, cursor) -> Option<Trigger>` free function. Replaced by the new state-machine tests inside `components/completion_trigger.rs`. |

### Out of scope

`PickerShell`, `MentionQuerySource`, `SlashQuerySource`, the mention registry, history-shell (`QueryMode::OwnedByShell`) flow, and `ProtectedRange` plumbing are unchanged.

---

## Pre-flight

- [ ] **Step 0.1: Confirm a clean worktree on a feature branch.**

  Run: `git status --porcelain`
  Expected: empty output.
  If not empty, stash or commit. If on `main`, create a branch: `git checkout -b feat/composition-intent-trigger`.

- [ ] **Step 0.2: Confirm baseline builds + tests pass.**

  Run: `cargo check -p spur-tui`
  Expected: no errors.

  Run: `cargo test -p spur-tui --lib completion_trigger -- --nocapture`
  Expected: all `detector_tests` pass (we are about to delete them).

---

## Task 1: Define new public types in `completion_trigger.rs`

**Files:**
- Modify: `crates/spur-tui/src/components/completion_trigger.rs`

This task only adds new types. It does not change `detect()` or `TriggerDetector::step` yet. Keeping the old and new side-by-side lets the next tasks cut over one transition at a time.

- [ ] **Step 1.1: Add the `IntentEvent` enum above the existing `TriggerTransition`.**

  Insert immediately before the `TriggerTransition` enum (currently around line 67):

  ```rust
  /// User-intent event fed to the trigger state machine. Classified at the
  /// dispatch site (InputBar::handle_key for key events; session_detail
  /// emits the non-key variants at their call sites). See the design spec
  /// for the full transition table.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum IntentEvent {
      /// User pressed a printable character key. Carries the character.
      TypedChar(char),
      /// User deleted characters (Backspace, Delete, Ctrl+K, Ctrl+U, Ctrl+W,
      /// or atomic range deletion). May remove multiple bytes.
      DeletedChar,
      /// Pure cursor motion — arrows, Home, End, Ctrl-A/E, word motion,
      /// vim h/j/k/l/w/b/etc., visual-line up/down, `g`/`G`, mouse click.
      MovedCursor,
      /// `input_bar.insert_paste(...)` ran.
      Pasted,
      /// `input_bar.set_text(...)` / `set_state(...)` / history-recall swap ran.
      SetText,
      /// Picker accepted a selection; the view is about to `insert_atom` or
      /// `set_state`. Emitted at the accept call site.
      Accepted,
      /// Picker cancelled (Esc / Ctrl+C). Emitted at the cancel call site.
      Dismissed,
      /// Buffer submitted (Enter). Emitted alongside `HandleOutcome::Submit`.
      Submitted,
      /// The key event was not handled (e.g., vim intermediate pending).
      NoOp,
  }
  ```

- [ ] **Step 1.2: Add the `TriggerState` enum directly below `IntentEvent`.**

  ```rust
  /// Internal state of the trigger detector.
  #[derive(Debug, Clone, PartialEq, Eq)]
  enum TriggerState {
      Idle,
      Composing {
          kind: TriggerKind,
          prefix_start: usize,
      },
  }

  impl Default for TriggerState {
      fn default() -> Self {
          TriggerState::Idle
      }
  }
  ```

- [ ] **Step 1.3: Verify the file still compiles.**

  Run: `cargo check -p spur-tui`
  Expected: succeeds (the new types are unused so far; `#[allow(dead_code)]` is not needed because they will be used in the next task).

- [ ] **Step 1.4: Commit.**

  ```bash
  git add crates/spur-tui/src/components/completion_trigger.rs
  git commit -m "feat(completion-trigger): add IntentEvent + TriggerState types"
  ```

---

## Task 2: Rewrite `TriggerDetector` to carry `TriggerState` and accept `IntentEvent`

**Files:**
- Modify: `crates/spur-tui/src/components/completion_trigger.rs`

Cut over the detector's internal storage from `last: Option<Trigger>` to `state: TriggerState`, and change `step()`'s signature. Tests for the new behavior come in Task 3+.

- [ ] **Step 2.1: Replace the detector struct and its impl.**

  Replace the entire existing `TriggerDetector` struct and its `impl` block (currently lines ~94–133) with:

  ```rust
  /// Stateful trigger recognizer. Trigger liveness is a function of user
  /// intent events — not text content. See the design spec.
  ///
  /// History-mode shells (`QueryMode::OwnedByShell`) are NOT managed by this
  /// detector; the view must skip calling `step` while such a shell is open.
  #[derive(Debug, Default)]
  pub struct TriggerDetector {
      state: TriggerState,
  }

  impl TriggerDetector {
      pub fn new() -> Self {
          Self::default()
      }

      /// `true` when the detector is in the Idle state. The view uses this
      /// to skip the detector entirely on non-opening events (fast path).
      pub fn is_idle(&self) -> bool {
          matches!(self.state, TriggerState::Idle)
      }

      /// Reset the detector to Idle. Call after the view accepts or cancels
      /// a trigger-driven shell.
      pub fn reset(&mut self) {
          self.state = TriggerState::Idle;
      }

      /// Feed an intent event plus the current text/cursor context. Returns
      /// a transition describing what should happen to the picker shell.
      pub fn step(
          &mut self,
          event: IntentEvent,
          text: &str,
          cursor: usize,
          protected_ranges: &[crate::components::input_bar::ProtectedRange],
      ) -> TriggerTransition {
          // Defensive re-check: if Composing state references a prefix_start
          // that no longer holds the trigger char (upstream path forgot to
          // send Pasted/SetText), force Idle + Close.
          if let TriggerState::Composing { kind, prefix_start } = self.state {
              let expected = match kind {
                  TriggerKind::Mention => '@',
                  TriggerKind::Slash => '/',
              };
              let still_valid = prefix_start < text.len()
                  && text[prefix_start..].chars().next() == Some(expected);
              if !still_valid {
                  self.state = TriggerState::Idle;
                  return TriggerTransition::Close;
              }
          }

          match (&self.state, &event) {
              // Fast Idle cases — just stay Idle.
              (TriggerState::Idle, IntentEvent::NoOp)
              | (TriggerState::Idle, IntentEvent::MovedCursor)
              | (TriggerState::Idle, IntentEvent::DeletedChar)
              | (TriggerState::Idle, IntentEvent::Pasted)
              | (TriggerState::Idle, IntentEvent::SetText)
              | (TriggerState::Idle, IntentEvent::Accepted)
              | (TriggerState::Idle, IntentEvent::Dismissed)
              | (TriggerState::Idle, IntentEvent::Submitted) => TriggerTransition::None,

              // Idle + TypedChar: maybe open.
              (TriggerState::Idle, IntentEvent::TypedChar(c)) => {
                  self.maybe_open(*c, text, cursor, protected_ranges)
              }

              // Composing + anything: delegated.
              (TriggerState::Composing { .. }, _) => {
                  self.advance_composing(event, text, cursor)
              }
          }
      }

      /// Idle → Composing transition logic for TypedChar events.
      fn maybe_open(
          &mut self,
          c: char,
          text: &str,
          cursor: usize,
          protected_ranges: &[crate::components::input_bar::ProtectedRange],
      ) -> TriggerTransition {
          // cursor is post-type; the typed char lives at cursor-1 byte-wise.
          let typed_byte = cursor.saturating_sub(c.len_utf8());
          if typed_byte >= text.len() {
              return TriggerTransition::None;
          }

          // Guard: the typed char's byte position must not be the start of a
          // protected range (I4 — committed atoms are opaque).
          if protected_ranges.iter().any(|r| r.start == typed_byte) {
              return TriggerTransition::None;
          }

          match c {
              '/' => {
                  if typed_byte != 0 {
                      return TriggerTransition::None;
                  }
                  self.state = TriggerState::Composing {
                      kind: TriggerKind::Slash,
                      prefix_start: 0,
                  };
                  TriggerTransition::Open {
                      trigger: Trigger {
                          kind: TriggerKind::Slash,
                          prefix_start: 0,
                          query: String::new(),
                      },
                  }
              }
              '@' => {
                  // Boundary: offset 0 OR prev char is whitespace.
                  let prev_is_boundary = typed_byte == 0
                      || text[..typed_byte]
                          .chars()
                          .last()
                          .is_none_or(|ch| ch.is_whitespace());
                  if !prev_is_boundary {
                      return TriggerTransition::None;
                  }
                  self.state = TriggerState::Composing {
                      kind: TriggerKind::Mention,
                      prefix_start: typed_byte,
                  };
                  TriggerTransition::Open {
                      trigger: Trigger {
                          kind: TriggerKind::Mention,
                          prefix_start: typed_byte,
                          query: String::new(),
                      },
                  }
              }
              _ => TriggerTransition::None,
          }
      }

      /// Composing → Composing|Idle transition logic.
      fn advance_composing(
          &mut self,
          event: IntentEvent,
          text: &str,
          cursor: usize,
      ) -> TriggerTransition {
          let (kind, prefix_start) = match self.state {
              TriggerState::Composing { kind, prefix_start } => (kind, prefix_start),
              TriggerState::Idle => unreachable!("called with Idle state"),
          };

          // Terminal events — always Close.
          match event {
              IntentEvent::Pasted
              | IntentEvent::SetText
              | IntentEvent::Accepted
              | IntentEvent::Dismissed
              | IntentEvent::Submitted => {
                  self.state = TriggerState::Idle;
                  return TriggerTransition::Close;
              }
              IntentEvent::NoOp => {
                  return TriggerTransition::None;
              }
              _ => {}
          }

          // Whitespace TypedChar closes.
          if let IntentEvent::TypedChar(c) = event {
              if c.is_whitespace() {
                  self.state = TriggerState::Idle;
                  return TriggerTransition::Close;
              }
          }

          // Determine window_end: first whitespace at or after prefix_start+1,
          // else text.len().
          let query_region_start = prefix_start + 1;
          let window_end = text[query_region_start.min(text.len())..]
              .char_indices()
              .find(|(_, ch)| ch.is_whitespace())
              .map(|(i, _)| query_region_start + i)
              .unwrap_or(text.len());

          // MovedCursor: close if cursor outside window.
          if matches!(event, IntentEvent::MovedCursor) {
              let in_window = cursor > prefix_start && cursor <= window_end;
              if !in_window {
                  self.state = TriggerState::Idle;
                  return TriggerTransition::Close;
              }
          }

          // DeletedChar: the defensive re-check at the top of step() handles
          // "trigger char gone → Close". Here we just fall through to Update.

          // Compute query slice as text[prefix_start+1 .. cursor], clamped.
          let clamped_end = cursor.min(window_end).min(text.len());
          let query_start = query_region_start.min(clamped_end);
          let query = text[query_start..clamped_end].to_string();

          let _ = kind; // silences unused; kind is carried in state only.
          TriggerTransition::Update { query }
      }
  }
  ```

- [ ] **Step 2.2: Remove the old `detector_tests` module at the bottom of the file** (currently lines ~135–228). These tests exercised the old `step(text, cursor)` signature; they will be replaced in Task 3.

  Delete the entire `#[cfg(test)] mod detector_tests { ... }` block.

- [ ] **Step 2.3: Remove the `detect` free function** (currently lines ~27–65). It is no longer referenced after the test module is removed.

  Delete the entire `pub fn detect(...)` function and its doc comment.

- [ ] **Step 2.4: Delete the old integration-test file that used `detect`.**

  ```bash
  git rm crates/spur-tui/tests/completion_trigger.rs
  ```

- [ ] **Step 2.5: Verify the crate compiles.**

  Run: `cargo check -p spur-tui`

  Expected outcome: **one or two errors** at `session_detail.rs` (uses of `detector.step(text, cursor)` and `detector.reset`) and potentially `views/dashboard.rs`. These are fixed in Task 8. Library code inside `completion_trigger.rs` itself must compile cleanly.

  If `completion_trigger.rs` itself has errors, fix them before proceeding. Common: a stray reference to `last:` in the struct, or a missed `use` import.

- [ ] **Step 2.6: Commit (the build is red but the unit is self-consistent).**

  ```bash
  git add crates/spur-tui/src/components/completion_trigger.rs \
          crates/spur-tui/tests/completion_trigger.rs
  git commit -m "refactor(completion-trigger): event-driven state machine

  Replace text-only detect()+step(text,cursor) with step(event, text,
  cursor, ranges). Old free fn and its integration test are removed;
  state-machine tests land in Task 3."
  ```

---

## Task 3: State-machine unit tests — Idle transitions

**Files:**
- Modify: `crates/spur-tui/src/components/completion_trigger.rs` (append tests)

Add a new `#[cfg(test)] mod detector_tests { ... }` block at the end of the file. Tests incrementally cover each transition. They all use an empty `&[]` for `protected_ranges` unless otherwise noted.

- [ ] **Step 3.1: Write the failing Idle-entry tests.**

  Append at end of file:

  ```rust
  #[cfg(test)]
  mod detector_tests {
      use super::*;
      use crate::components::input_bar::ProtectedRange;

      fn d() -> TriggerDetector {
          TriggerDetector::new()
      }

      #[test]
      fn idle_typed_at_at_offset_zero_opens_mention() {
          let mut det = d();
          let t = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
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
      fn idle_typed_slash_at_offset_zero_opens_slash() {
          let mut det = d();
          let t = det.step(IntentEvent::TypedChar('/'), "/", 1, &[]);
          match t {
              TriggerTransition::Open { trigger } => {
                  assert_eq!(trigger.kind, TriggerKind::Slash);
                  assert_eq!(trigger.prefix_start, 0);
              }
              other => panic!("expected Open, got {other:?}"),
          }
      }

      #[test]
      fn idle_typed_slash_at_nonzero_offset_stays_idle() {
          let mut det = d();
          let t = det.step(IntentEvent::TypedChar('/'), "a/", 2, &[]);
          assert!(matches!(t, TriggerTransition::None));
          assert!(det.is_idle());
      }

      #[test]
      fn idle_typed_at_after_non_whitespace_stays_idle() {
          let mut det = d();
          let t = det.step(IntentEvent::TypedChar('@'), "foo@", 4, &[]);
          assert!(matches!(t, TriggerTransition::None));
          assert!(det.is_idle());
      }

      #[test]
      fn idle_typed_at_after_whitespace_opens() {
          let mut det = d();
          let t = det.step(IntentEvent::TypedChar('@'), "foo @", 5, &[]);
          match t {
              TriggerTransition::Open { trigger } => {
                  assert_eq!(trigger.prefix_start, 4);
              }
              other => panic!("expected Open, got {other:?}"),
          }
      }

      #[test]
      fn idle_typed_at_where_byte_is_atom_start_stays_idle() {
          // Simulates: the typed '@' lands exactly at the start of a
          // ProtectedRange (committed atom). Detector must NOT open.
          let mut det = d();
          let ranges = [ProtectedRange {
              start: 0,
              end: 4,
              uri: "u".into(),
              name: "n".into(),
          }];
          let t = det.step(IntentEvent::TypedChar('@'), "@foo", 1, &ranges);
          assert!(matches!(t, TriggerTransition::None));
          assert!(det.is_idle());
      }

      #[test]
      fn idle_moved_cursor_stays_idle_emits_none() {
          let mut det = d();
          let t = det.step(IntentEvent::MovedCursor, "hello @world", 12, &[]);
          assert!(matches!(t, TriggerTransition::None));
          assert!(det.is_idle());
      }

      #[test]
      fn idle_deleted_char_stays_idle() {
          let mut det = d();
          let t = det.step(IntentEvent::DeletedChar, "hello", 5, &[]);
          assert!(matches!(t, TriggerTransition::None));
      }

      #[test]
      fn idle_pasted_stays_idle() {
          let mut det = d();
          let t = det.step(IntentEvent::Pasted, "pasted @alice text", 18, &[]);
          assert!(matches!(t, TriggerTransition::None));
          assert!(det.is_idle());
      }

      #[test]
      fn idle_set_text_stays_idle() {
          let mut det = d();
          let t = det.step(IntentEvent::SetText, "recalled @foo", 13, &[]);
          assert!(matches!(t, TriggerTransition::None));
      }

      #[test]
      fn idle_noop_emits_none() {
          let mut det = d();
          let t = det.step(IntentEvent::NoOp, "", 0, &[]);
          assert!(matches!(t, TriggerTransition::None));
      }
  }
  ```

- [ ] **Step 3.2: Run the new tests.**

  Run: `cargo test -p spur-tui --lib completion_trigger::detector_tests -- --nocapture`
  Expected: all **11** tests pass.

- [ ] **Step 3.3: Commit.**

  ```bash
  git add crates/spur-tui/src/components/completion_trigger.rs
  git commit -m "test(completion-trigger): Idle transition coverage"
  ```

---

## Task 4: State-machine unit tests — Composing transitions

**Files:**
- Modify: `crates/spur-tui/src/components/completion_trigger.rs` (extend `detector_tests`)

- [ ] **Step 4.1: Append Composing-transition tests inside `detector_tests`.**

  Insert just before the closing `}` of `mod detector_tests`:

  ```rust
      // ── Composing → Composing: query refinement ───────────────────────

      #[test]
      fn composing_typed_char_emits_update_with_growing_query() {
          let mut det = d();
          let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
          let t = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[]);
          match t {
              TriggerTransition::Update { query } => assert_eq!(query, "f"),
              other => panic!("expected Update, got {other:?}"),
          }
      }

      #[test]
      fn composing_deleted_char_emits_update_with_shrunken_query() {
          let mut det = d();
          let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
          let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[]);
          let _ = det.step(IntentEvent::TypedChar('o'), "@fo", 3, &[]);
          let t = det.step(IntentEvent::DeletedChar, "@f", 2, &[]);
          match t {
              TriggerTransition::Update { query } => assert_eq!(query, "f"),
              other => panic!("expected Update, got {other:?}"),
          }
      }

      #[test]
      fn composing_moved_cursor_inside_window_emits_update() {
          let mut det = d();
          let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
          let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[]);
          let _ = det.step(IntentEvent::TypedChar('o'), "@fo", 3, &[]);
          let _ = det.step(IntentEvent::TypedChar('o'), "@foo", 4, &[]);
          // Cursor moves back to position 3 (between 'f' and 'o') — still in window.
          let t = det.step(IntentEvent::MovedCursor, "@foo", 3, &[]);
          match t {
              TriggerTransition::Update { query } => assert_eq!(query, "fo"),
              other => panic!("expected Update, got {other:?}"),
          }
      }

      // ── Composing → Idle: terminating events ──────────────────────────

      #[test]
      fn composing_typed_whitespace_emits_close() {
          let mut det = d();
          let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
          let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[]);
          let t = det.step(IntentEvent::TypedChar(' '), "@f ", 3, &[]);
          assert!(matches!(t, TriggerTransition::Close));
          assert!(det.is_idle());
      }

      #[test]
      fn composing_moved_cursor_out_of_window_emits_close() {
          let mut det = d();
          let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
          let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[]);
          // Cursor jumps to offset 0 (before the '@') — outside window.
          let t = det.step(IntentEvent::MovedCursor, "@f", 0, &[]);
          assert!(matches!(t, TriggerTransition::Close));
          assert!(det.is_idle());
      }

      #[test]
      fn composing_deleted_trigger_char_emits_close_via_defensive_check() {
          let mut det = d();
          let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
          let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[]);
          // Backspace twice deletes 'f' then '@'. Cursor at 0, text empty.
          let _ = det.step(IntentEvent::DeletedChar, "@", 1, &[]);
          let t = det.step(IntentEvent::DeletedChar, "", 0, &[]);
          assert!(matches!(t, TriggerTransition::Close));
          assert!(det.is_idle());
      }

      #[test]
      fn composing_pasted_emits_close() {
          let mut det = d();
          let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
          let t = det.step(IntentEvent::Pasted, "@ hello world", 13, &[]);
          assert!(matches!(t, TriggerTransition::Close));
          assert!(det.is_idle());
      }

      #[test]
      fn composing_set_text_emits_close() {
          let mut det = d();
          let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
          let t = det.step(IntentEvent::SetText, "recalled text", 13, &[]);
          assert!(matches!(t, TriggerTransition::Close));
      }

      #[test]
      fn composing_accepted_emits_close() {
          let mut det = d();
          let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
          let t = det.step(IntentEvent::Accepted, "@atom", 5, &[]);
          assert!(matches!(t, TriggerTransition::Close));
      }

      #[test]
      fn composing_dismissed_emits_close() {
          let mut det = d();
          let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
          let t = det.step(IntentEvent::Dismissed, "@", 1, &[]);
          assert!(matches!(t, TriggerTransition::Close));
      }

      #[test]
      fn composing_submitted_emits_close() {
          let mut det = d();
          let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
          let t = det.step(IntentEvent::Submitted, "", 0, &[]);
          assert!(matches!(t, TriggerTransition::Close));
      }

      #[test]
      fn composing_noop_emits_none_and_stays_composing() {
          let mut det = d();
          let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
          let t = det.step(IntentEvent::NoOp, "@", 1, &[]);
          assert!(matches!(t, TriggerTransition::None));
          assert!(!det.is_idle());
      }
  ```

- [ ] **Step 4.2: Run the new tests.**

  Run: `cargo test -p spur-tui --lib completion_trigger::detector_tests -- --nocapture`
  Expected: all **23** tests pass (11 from Task 3 + 12 new).

- [ ] **Step 4.3: Commit.**

  ```bash
  git add crates/spur-tui/src/components/completion_trigger.rs
  git commit -m "test(completion-trigger): Composing transition coverage"
  ```

---

## Task 5: State-machine unit tests — journeys & defensive re-check

**Files:**
- Modify: `crates/spur-tui/src/components/completion_trigger.rs` (extend `detector_tests`)

- [ ] **Step 5.1: Append journey-level tests.**

  Insert inside `mod detector_tests` (before its closing brace):

  ```rust
      // ── Journey tests ─────────────────────────────────────────────────

      #[test]
      fn j1_power_user_walks_cursor_across_atoms_zero_opens() {
          // Text: "@src/foo.rs and @docs/bar.md"
          //       0    5          16
          // Ranges cover both atoms.
          let mut det = d();
          let text = "@src/foo.rs and @docs/bar.md";
          let ranges = [
              ProtectedRange { start: 0, end: 11, uri: "a".into(), name: "a".into() },
              ProtectedRange { start: 16, end: 28, uri: "b".into(), name: "b".into() },
          ];
          let mut opens = 0;
          // Walk cursor from 0 to text.len() one byte at a time.
          for cursor in 0..=text.len() {
              let t = det.step(IntentEvent::MovedCursor, text, cursor, &ranges);
              if matches!(t, TriggerTransition::Open { .. }) {
                  opens += 1;
              }
          }
          assert_eq!(opens, 0, "cursor motion must never open picker");
      }

      #[test]
      fn j6_auto_repeat_left_arrow_across_stray_at_zero_opens() {
          let mut det = d();
          let text = "please see @foo bar";
          let mut opens = 0;
          for _ in 0..50 {
              let t = det.step(IntentEvent::MovedCursor, text, 15, &[]);
              if matches!(t, TriggerTransition::Open { .. }) {
                  opens += 1;
              }
          }
          assert_eq!(opens, 0);
      }

      #[test]
      fn j7_selection_drag_across_stray_at_zero_opens() {
          let mut det = d();
          let text = "text @alice more";
          let mut opens = 0;
          for cursor in 0..=text.len() {
              let t = det.step(IntentEvent::MovedCursor, text, cursor, &[]);
              if matches!(t, TriggerTransition::Open { .. }) {
                  opens += 1;
              }
          }
          assert_eq!(opens, 0);
      }

      #[test]
      fn j2b_typo_fix_after_esc_stays_closed_on_motion() {
          let mut det = d();
          let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
          let _ = det.step(IntentEvent::TypedChar('f'), "@f", 2, &[]);
          let _ = det.step(IntentEvent::TypedChar('o'), "@fo", 3, &[]);
          // User presses Esc; view emits Dismissed.
          let close = det.step(IntentEvent::Dismissed, "@fo", 3, &[]);
          assert!(matches!(close, TriggerTransition::Close));
          // User arrow-keys back into @f|o.
          let mot = det.step(IntentEvent::MovedCursor, "@fo", 2, &[]);
          assert!(matches!(mot, TriggerTransition::None));
          assert!(det.is_idle());
      }

      #[test]
      fn reset_puts_detector_in_idle() {
          let mut det = d();
          let _ = det.step(IntentEvent::TypedChar('@'), "@", 1, &[]);
          assert!(!det.is_idle());
          det.reset();
          assert!(det.is_idle());
      }

      #[test]
      fn defensive_reset_when_prefix_start_past_text_len() {
          // Force Composing manually (white-box), then call step with a
          // shorter text. Defensive check should Close.
          let mut det = TriggerDetector {
              state: TriggerState::Composing { kind: TriggerKind::Mention, prefix_start: 100 },
          };
          let t = det.step(IntentEvent::MovedCursor, "abc", 3, &[]);
          assert!(matches!(t, TriggerTransition::Close));
          assert!(det.is_idle());
      }
  ```

  Note: the `defensive_reset_when_prefix_start_past_text_len` test constructs `TriggerDetector` directly via struct-literal — this requires `TriggerState` and its `Composing` variant to be visible to the test. Since the test module is inside the same file, this works even though `TriggerState` is file-private.

- [ ] **Step 5.2: Run the new tests.**

  Run: `cargo test -p spur-tui --lib completion_trigger::detector_tests -- --nocapture`
  Expected: all **29** tests pass.

- [ ] **Step 5.3: Commit.**

  ```bash
  git add crates/spur-tui/src/components/completion_trigger.rs
  git commit -m "test(completion-trigger): journey + defensive coverage"
  ```

---

## Task 6: Add `HandleOutcome` + `IntentEvent` surface in `InputBar`

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`

This task only introduces the return type. Signature change and classification land in Task 7.

- [ ] **Step 6.1: Re-export `IntentEvent` and add `HandleOutcome`.**

  At the top of `input_bar.rs`, near the other imports, add:

  ```rust
  use crate::components::completion_trigger::IntentEvent;
  ```

  Then, just below the existing `VimMode` enum (around line 42), add:

  ```rust
  /// The result of `InputBar::handle_key`. The `Submit` variant preserves
  /// today's submit tuple; the `Key` variant carries the classified
  /// `IntentEvent` for the TriggerDetector.
  #[derive(Debug, Clone, PartialEq)]
  pub enum HandleOutcome {
      /// Buffer submitted. `String` is the submitted text, `bool` is the
      /// interrupt flag. The view also emits `IntentEvent::Submitted`.
      Submit(String, bool),
      /// Ordinary key processed; carries the classified intent.
      Key(IntentEvent),
  }
  ```

- [ ] **Step 6.2: Verify compile.**

  Run: `cargo check -p spur-tui`
  Expected: still red (same previous errors about old `detector.step` usage); but `input_bar.rs` itself compiles with the new types defined.

- [ ] **Step 6.3: Commit.**

  ```bash
  git add crates/spur-tui/src/components/input_bar.rs
  git commit -m "feat(input-bar): introduce HandleOutcome + IntentEvent import"
  ```

---

## Task 7: Rewrite `InputBar::handle_key` to return `HandleOutcome` (classifier)

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`

This is the biggest mechanical change. Every branch of `handle_emacs_input`, `handle_vim_normal_input`, `handle_vim_insert_input`, `vim_complete_operator` needs to return `HandleOutcome` instead of `Option<(String, bool)>`.

- [ ] **Step 7.1: Change the three method signatures.**

  Update lines ~149 and ~186 and ~296 and ~697:

  ```rust
  pub fn handle_key(&mut self, key: KeyEvent) -> HandleOutcome {
      let input = self.keyevent_to_input(key);
      match self.mode {
          EditMode::Emacs => self.handle_emacs_input(key, input),
          EditMode::Vim(mode) => self.handle_vim_input(key, input, mode),
      }
  }

  fn handle_emacs_input(&mut self, key: KeyEvent, input: Input) -> HandleOutcome { /* ... */ }

  fn handle_vim_input(&mut self, key: KeyEvent, input: Input, mode: VimMode) -> HandleOutcome { /* ... */ }

  fn handle_vim_normal_input(&mut self, _key: KeyEvent, input: Input, mode: VimMode) -> HandleOutcome { /* ... */ }

  fn handle_vim_insert_input(&mut self, key: KeyEvent, input: Input) -> HandleOutcome { /* ... */ }

  fn vim_complete_operator(&mut self, mode: VimMode) -> HandleOutcome { /* ... */ }
  ```

  And adjust the private `submit()` method to still return `Option<(String, bool)>` — its callers wrap the result in `HandleOutcome::Submit(..)` vs `HandleOutcome::Key(IntentEvent::NoOp)` (empty submit).

- [ ] **Step 7.2: Rewrite the `handle_emacs_input` body branches.**

  Replace the function body (lines 186–294) with this full version:

  ```rust
  fn handle_emacs_input(&mut self, key: KeyEvent, input: Input) -> HandleOutcome {
      // Handle protected range logic for special keys
      match key.code {
          KeyCode::Up => {
              self.visual_line_up(self.last_inner_width());
              return HandleOutcome::Key(IntentEvent::MovedCursor);
          }
          KeyCode::Down => {
              self.visual_line_down(self.last_inner_width());
              return HandleOutcome::Key(IntentEvent::MovedCursor);
          }
          KeyCode::Left => {
              self.move_cursor_back();
              return HandleOutcome::Key(IntentEvent::MovedCursor);
          }
          KeyCode::Right => {
              self.move_cursor_forward();
              return HandleOutcome::Key(IntentEvent::MovedCursor);
          }
          KeyCode::Backspace => {
              self.delete_char_before_cursor();
              return HandleOutcome::Key(IntentEvent::DeletedChar);
          }
          KeyCode::Delete => {
              self.delete_char_after_cursor();
              return HandleOutcome::Key(IntentEvent::DeletedChar);
          }
          KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
              self.textarea.insert_newline();
              self.rebuild_line_cache();
              return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
          }
          KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
              self.history_prev();
              return HandleOutcome::Key(IntentEvent::SetText);
          }
          KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
              self.history_next();
              return HandleOutcome::Key(IntentEvent::SetText);
          }
          KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
              let cursor = self.cursor_to_byte();
              if cursor > 0 {
                  let text = self.text();
                  let line_start = text[..cursor].rfind('\n').map(|i| i + 1).unwrap_or(0);
                  self.delete_span(line_start, cursor);
              }
              return HandleOutcome::Key(IntentEvent::DeletedChar);
          }
          KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
              let cursor = self.cursor_to_byte();
              let text = self.text();
              let line_end = text[cursor..]
                  .find('\n')
                  .map(|i| cursor + i)
                  .unwrap_or(text.len());
              if line_end > cursor {
                  self.delete_span(cursor, line_end);
              }
              return HandleOutcome::Key(IntentEvent::DeletedChar);
          }
          KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
              let cursor = self.cursor_to_byte();
              if cursor > 0 {
                  let text = self.text();
                  let mut start = cursor;
                  let mut seen_non_whitespace = false;
                  for (idx, ch) in text[..cursor].char_indices().rev() {
                      if !seen_non_whitespace {
                          start = idx;
                          if ch.is_whitespace() {
                              continue;
                          }
                          seen_non_whitespace = true;
                          continue;
                      }
                      if ch.is_whitespace() {
                          break;
                      }
                      start = idx;
                  }
                  self.delete_span(start, cursor);
              }
              return HandleOutcome::Key(IntentEvent::DeletedChar);
          }
          KeyCode::Char(c) => {
              self.insert_char_with_protected_check(c);
              return HandleOutcome::Key(IntentEvent::TypedChar(c));
          }
          KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
              self.textarea.insert_newline();
              self.rebuild_line_cache();
              return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
          }
          KeyCode::Enter => {
              return match self.submit() {
                  Some((t, interrupt)) => HandleOutcome::Submit(t, interrupt),
                  None => HandleOutcome::Key(IntentEvent::NoOp),
              };
          }
          _ => {}
      }

      // Delegate to textarea for other keys. Treat as NoOp for the detector
      // — tui_textarea handled something we don't model, but no composition
      // intent is claimed.
      self.textarea.input(input);
      self.rebuild_line_cache();
      HandleOutcome::Key(IntentEvent::NoOp)
  }
  ```

- [ ] **Step 7.3: Rewrite the vim-insert handler.**

  Replace `handle_vim_insert_input` (lines 697–758) with:

  ```rust
  fn handle_vim_insert_input(&mut self, key: KeyEvent, input: Input) -> HandleOutcome {
      match key.code {
          KeyCode::Esc => {
              self.set_mode(EditMode::Vim(VimMode::Normal));
              return HandleOutcome::Key(IntentEvent::NoOp);
          }
          KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
              self.set_mode(EditMode::Vim(VimMode::Normal));
              return HandleOutcome::Key(IntentEvent::NoOp);
          }
          KeyCode::Char('j')
              if key.modifiers.contains(KeyModifiers::CONTROL)
                  || key.modifiers.contains(KeyModifiers::ALT) =>
          {
              self.textarea.insert_newline();
              self.rebuild_line_cache();
              return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
          }
          KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
              self.textarea.insert_newline();
              self.rebuild_line_cache();
              return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
          }
          KeyCode::Up => {
              self.visual_line_up(self.last_inner_width());
              return HandleOutcome::Key(IntentEvent::MovedCursor);
          }
          KeyCode::Down => {
              self.visual_line_down(self.last_inner_width());
              return HandleOutcome::Key(IntentEvent::MovedCursor);
          }
          KeyCode::Left => {
              self.move_cursor_back();
              return HandleOutcome::Key(IntentEvent::MovedCursor);
          }
          KeyCode::Right => {
              self.move_cursor_forward();
              return HandleOutcome::Key(IntentEvent::MovedCursor);
          }
          KeyCode::Backspace => {
              self.delete_char_before_cursor();
              return HandleOutcome::Key(IntentEvent::DeletedChar);
          }
          KeyCode::Delete => {
              self.delete_char_after_cursor();
              return HandleOutcome::Key(IntentEvent::DeletedChar);
          }
          KeyCode::Char(c) => {
              self.insert_char_with_protected_check(c);
              return HandleOutcome::Key(IntentEvent::TypedChar(c));
          }
          KeyCode::Enter => {
              return match self.submit() {
                  Some((t, interrupt)) => HandleOutcome::Submit(t, interrupt),
                  None => HandleOutcome::Key(IntentEvent::NoOp),
              };
          }
          _ => {}
      }

      self.textarea.input(input);
      self.rebuild_line_cache();
      HandleOutcome::Key(IntentEvent::NoOp)
  }
  ```

- [ ] **Step 7.4: Rewrite `handle_vim_normal_input` branches.**

  Adjust every `return None;` and terminal fall-through in `handle_vim_normal_input` (lines 310–670) and `vim_complete_operator` (lines 673–695) to return an appropriate `HandleOutcome::Key(IntentEvent::…)`.

  Simpler: replace the whole function with this rewrite (covers every existing branch; semantics unchanged; only the return type changes):

  ```rust
  fn handle_vim_normal_input(
      &mut self,
      _key: KeyEvent,
      input: Input,
      mode: VimMode,
  ) -> HandleOutcome {
      if input.key == Key::Null {
          return HandleOutcome::Key(IntentEvent::NoOp);
      }

      // Handle pending two-key sequences (gg, dd, yy, cc)
      if let Some(pending) = self.vim_pending.take() {
          match pending.key {
              Key::Char('g') if matches!(input.key, Key::Char('g')) => {
                  self.textarea.move_cursor(CursorMove::Top);
                  self.vim_complete_operator(mode);
                  return HandleOutcome::Key(IntentEvent::MovedCursor);
              }
              Key::Char(op) if matches!(mode, VimMode::Operator(c) if c == op) => {
                  if let Key::Char(c) = input.key {
                      if c == op {
                          self.textarea.move_cursor(CursorMove::Head);
                          self.textarea.start_selection();
                          let cursor = self.textarea.cursor();
                          self.textarea.move_cursor(CursorMove::Down);
                          if cursor == self.textarea.cursor() {
                              self.textarea.move_cursor(CursorMove::End);
                          }
                          // Operator completion below deletes or yanks.
                          let _ = self.vim_complete_operator(mode);
                          return HandleOutcome::Key(IntentEvent::DeletedChar);
                      }
                  }
              }
              _ => {}
          }
      }

      // Keep existing dispatch; wrap each outcome.
      match input {
          // ── Movement ────────────────────────────────────────────
          Input { key: Key::Char('h'), .. } => self.move_cursor_back(),
          Input { key: Key::Char('j'), .. } => self.textarea.move_cursor(CursorMove::Down),
          Input { key: Key::Char('k'), .. } => self.textarea.move_cursor(CursorMove::Up),
          Input { key: Key::Char('l'), .. } => self.move_cursor_forward(),
          Input { key: Key::Char('w'), .. } => self.textarea.move_cursor(CursorMove::WordForward),
          Input { key: Key::Char('e'), ctrl: false, .. } => {
              self.textarea.move_cursor(CursorMove::WordEnd);
              if matches!(mode, VimMode::Operator(_)) {
                  self.textarea.move_cursor(CursorMove::Forward);
              }
          }
          Input { key: Key::Char('b'), ctrl: false, .. } => {
              self.textarea.move_cursor(CursorMove::WordBack);
          }
          Input { key: Key::Char('^'), .. } => self.textarea.move_cursor(CursorMove::Head),
          Input { key: Key::Char('0'), .. } => self.textarea.move_cursor(CursorMove::Head),
          Input { key: Key::Char('$'), .. } => self.textarea.move_cursor(CursorMove::End),
          Input { key: Key::Char('g'), ctrl: false, .. } => {
              self.vim_pending = Some(input);
              return HandleOutcome::Key(IntentEvent::NoOp);
          }
          Input { key: Key::Char('G'), ctrl: false, .. } => {
              self.textarea.move_cursor(CursorMove::Bottom);
          }

          // ── Editing (Normal only) ───────────────────────────────
          Input { key: Key::Char('D'), .. } if mode == VimMode::Normal => {
              self.textarea.delete_line_by_end();
              self.rebuild_line_cache();
              self.protected_ranges.clear();
              self.set_mode(EditMode::Vim(VimMode::Normal));
              return HandleOutcome::Key(IntentEvent::DeletedChar);
          }
          Input { key: Key::Char('C'), .. } if mode == VimMode::Normal => {
              self.textarea.delete_line_by_end();
              self.rebuild_line_cache();
              self.protected_ranges.clear();
              self.set_mode(EditMode::Vim(VimMode::Insert));
              return HandleOutcome::Key(IntentEvent::DeletedChar);
          }
          Input { key: Key::Char('x'), .. } => {
              self.delete_char_after_cursor();
              return HandleOutcome::Key(IntentEvent::DeletedChar);
          }
          Input { key: Key::Char('p'), .. } if mode == VimMode::Normal => {
              self.textarea.paste();
              self.rebuild_line_cache();
              self.protected_ranges.clear();
              return HandleOutcome::Key(IntentEvent::Pasted);
          }

          // ── Operator entry (Normal → Operator) ──────────────────
          Input { key: Key::Char(op @ ('d' | 'c' | 'y')), ctrl: false, .. } if mode == VimMode::Normal => {
              self.textarea.start_selection();
              self.set_mode(EditMode::Vim(VimMode::Operator(op)));
              self.vim_pending = Some(input);
              return HandleOutcome::Key(IntentEvent::NoOp);
          }

          // ── Mode entry ──────────────────────────────────────────
          Input { key: Key::Char('i'), .. } if mode != VimMode::Visual => {
              self.textarea.cancel_selection();
              self.set_mode(EditMode::Vim(VimMode::Insert));
              return HandleOutcome::Key(IntentEvent::NoOp);
          }
          Input { key: Key::Char('a'), .. } if mode != VimMode::Visual => {
              self.textarea.cancel_selection();
              self.textarea.move_cursor(CursorMove::Forward);
              self.set_mode(EditMode::Vim(VimMode::Insert));
              return HandleOutcome::Key(IntentEvent::MovedCursor);
          }
          Input { key: Key::Char('A'), .. } if mode != VimMode::Visual => {
              self.textarea.cancel_selection();
              self.textarea.move_cursor(CursorMove::End);
              self.set_mode(EditMode::Vim(VimMode::Insert));
              return HandleOutcome::Key(IntentEvent::MovedCursor);
          }
          Input { key: Key::Char('I'), .. } if mode != VimMode::Visual => {
              self.textarea.cancel_selection();
              self.textarea.move_cursor(CursorMove::Head);
              self.set_mode(EditMode::Vim(VimMode::Insert));
              return HandleOutcome::Key(IntentEvent::MovedCursor);
          }
          Input { key: Key::Char('o'), .. } if mode == VimMode::Normal => {
              self.textarea.move_cursor(CursorMove::End);
              self.textarea.insert_newline();
              self.rebuild_line_cache();
              self.set_mode(EditMode::Vim(VimMode::Insert));
              return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
          }
          Input { key: Key::Char('O'), .. } if mode == VimMode::Normal => {
              self.textarea.move_cursor(CursorMove::Head);
              self.textarea.insert_newline();
              self.textarea.move_cursor(CursorMove::Up);
              self.rebuild_line_cache();
              self.set_mode(EditMode::Vim(VimMode::Insert));
              return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
          }

          // ── Visual mode ─────────────────────────────────────────
          Input { key: Key::Char('v'), ctrl: false, .. } if mode == VimMode::Normal => {
              self.textarea.start_selection();
              self.set_mode(EditMode::Vim(VimMode::Visual));
              return HandleOutcome::Key(IntentEvent::NoOp);
          }
          Input { key: Key::Char('V'), ctrl: false, .. } if mode == VimMode::Normal => {
              self.textarea.move_cursor(CursorMove::Head);
              self.textarea.start_selection();
              self.textarea.move_cursor(CursorMove::End);
              self.set_mode(EditMode::Vim(VimMode::Visual));
              return HandleOutcome::Key(IntentEvent::MovedCursor);
          }
          Input { key: Key::Esc, .. }
          | Input { key: Key::Char('v'), ctrl: false, .. } if mode == VimMode::Visual => {
              self.textarea.cancel_selection();
              self.set_mode(EditMode::Vim(VimMode::Normal));
              return HandleOutcome::Key(IntentEvent::NoOp);
          }

          // ── Visual operations ───────────────────────────────────
          Input { key: Key::Char('y'), ctrl: false, .. } if mode == VimMode::Visual => {
              self.textarea.move_cursor(CursorMove::Forward);
              self.textarea.copy();
              self.textarea.cancel_selection();
              self.set_mode(EditMode::Vim(VimMode::Normal));
              return HandleOutcome::Key(IntentEvent::NoOp);
          }
          Input { key: Key::Char('d'), ctrl: false, .. } if mode == VimMode::Visual => {
              self.textarea.move_cursor(CursorMove::Forward);
              self.textarea.cut();
              self.rebuild_line_cache();
              self.protected_ranges.clear();
              self.set_mode(EditMode::Vim(VimMode::Normal));
              return HandleOutcome::Key(IntentEvent::DeletedChar);
          }
          Input { key: Key::Char('c'), ctrl: false, .. } if mode == VimMode::Visual => {
              self.textarea.move_cursor(CursorMove::Forward);
              self.textarea.cut();
              self.rebuild_line_cache();
              self.protected_ranges.clear();
              self.set_mode(EditMode::Vim(VimMode::Insert));
              return HandleOutcome::Key(IntentEvent::DeletedChar);
          }

          // ── Scroll ──────────────────────────────────────────────
          Input { key: Key::Char('d'), ctrl: true, .. } => {
              self.textarea.scroll(tui_textarea::Scrolling::HalfPageDown);
          }
          Input { key: Key::Char('u'), ctrl: true, .. } => {
              self.textarea.scroll(tui_textarea::Scrolling::HalfPageUp);
          }
          Input { key: Key::Char('f'), ctrl: true, .. } => {
              self.textarea.scroll(tui_textarea::Scrolling::PageDown);
          }
          Input { key: Key::Char('b'), ctrl: true, .. } => {
              self.textarea.scroll(tui_textarea::Scrolling::PageUp);
          }
          Input { key: Key::Char('e'), ctrl: true, .. } => {
              self.textarea.scroll((1, 0));
          }
          Input { key: Key::Char('y'), ctrl: true, .. } => {
              self.textarea.scroll((-1, 0));
          }

          // ── Arrow-key visual-line nav (Vim Normal) ──────────────
          Input { key: Key::Up, .. } => {
              self.visual_line_up(self.last_inner_width());
              return HandleOutcome::Key(IntentEvent::MovedCursor);
          }
          Input { key: Key::Down, .. } => {
              self.visual_line_down(self.last_inner_width());
              return HandleOutcome::Key(IntentEvent::MovedCursor);
          }

          // ── Esc / Enter ─────────────────────────────────────────
          Input { key: Key::Esc, .. } => {
              self.textarea.cancel_selection();
              self.set_mode(EditMode::Vim(VimMode::Normal));
              return HandleOutcome::Key(IntentEvent::NoOp);
          }
          Input { key: Key::Enter, alt: true, .. } => {
              self.textarea.insert_newline();
              self.rebuild_line_cache();
              return HandleOutcome::Key(IntentEvent::TypedChar('\n'));
          }
          Input { key: Key::Enter, .. } => {
              return match self.submit() {
                  Some((t, interrupt)) => HandleOutcome::Submit(t, interrupt),
                  None => HandleOutcome::Key(IntentEvent::NoOp),
              };
          }
          _ => return HandleOutcome::Key(IntentEvent::NoOp),
      }

      // After movement, complete pending operator and return MovedCursor.
      let _ = self.vim_complete_operator(mode);
      HandleOutcome::Key(IntentEvent::MovedCursor)
  }
  ```

- [ ] **Step 7.5: Adjust `vim_complete_operator` return type.**

  Replace its body with:

  ```rust
  fn vim_complete_operator(&mut self, mode: VimMode) -> HandleOutcome {
      match mode {
          VimMode::Operator('y') => {
              self.textarea.copy();
              self.textarea.cancel_selection();
              self.set_mode(EditMode::Vim(VimMode::Normal));
          }
          VimMode::Operator('d') => {
              self.textarea.cut();
              self.rebuild_line_cache();
              self.protected_ranges.clear();
              self.set_mode(EditMode::Vim(VimMode::Normal));
          }
          VimMode::Operator('c') => {
              self.textarea.cut();
              self.rebuild_line_cache();
              self.protected_ranges.clear();
              self.set_mode(EditMode::Vim(VimMode::Insert));
          }
          _ => {}
      }
      HandleOutcome::Key(IntentEvent::NoOp)
  }
  ```

- [ ] **Step 7.6: Fix the `handle_vim_input` dispatcher.**

  Replace its body with the same match, now returning `HandleOutcome`:

  ```rust
  fn handle_vim_input(&mut self, key: KeyEvent, input: Input, mode: VimMode) -> HandleOutcome {
      match mode {
          VimMode::Normal | VimMode::Visual | VimMode::Operator(_) => {
              self.handle_vim_normal_input(key, input, mode)
          }
          VimMode::Insert => self.handle_vim_insert_input(key, input),
      }
  }
  ```

- [ ] **Step 7.7: Verify `input_bar.rs` compiles.**

  Run: `cargo check -p spur-tui --lib`
  Expected: errors move to `session_detail.rs` and `dashboard.rs` (they still expect the old return type). `input_bar.rs` itself is clean.

- [ ] **Step 7.8: Commit.**

  ```bash
  git add crates/spur-tui/src/components/input_bar.rs
  git commit -m "refactor(input-bar): handle_key returns HandleOutcome

  Every branch in Emacs + Vim (Normal/Insert/Visual/Operator) now
  classifies its effect into an IntentEvent. submit() still returns
  its tuple internally and is lifted into HandleOutcome::Submit at the
  branch site."
  ```

---

## Task 8: Update `session_detail.rs` — consume `HandleOutcome`, add `dispatch_intent`

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 8.1: Delete `refresh_popup()` entirely.**

  Delete the entire `fn refresh_popup(&mut self)` method (currently around lines 560–626) including its imports block.

- [ ] **Step 8.2: Add the new `dispatch_intent` helper.**

  Insert this helper immediately where `refresh_popup` used to live:

  ```rust
  /// Feed a classified IntentEvent into the TriggerDetector and apply the
  /// resulting transition to `self.picker_shell`. Includes the Idle
  /// fast-path: on `Idle` state and a non-opening event, return in O(1)
  /// without fetching text/cursor/ranges from `input_bar`.
  fn dispatch_intent(&mut self, event: crate::components::completion_trigger::IntentEvent) {
      use crate::components::completion_trigger::{
          IntentEvent, TriggerKind, TriggerTransition,
      };
      use crate::components::picker_shell::PickerShell;
      use crate::components::query_source::{
          MentionQuerySource, QueryMode, SlashQuerySource, SlashRow,
      };

      // History-mode shell owns the picker; detector is inert.
      if let Some(shell) = self.picker_shell.as_ref() {
          if shell.query_mode() == QueryMode::OwnedByShell {
              self.trigger_detector.reset();
              return;
          }
      }

      // Fast path: Idle state + non-opening event → no text fetch, no alloc.
      if self.trigger_detector.is_idle()
          && !matches!(
              event,
              IntentEvent::TypedChar('@') | IntentEvent::TypedChar('/')
          )
      {
          return;
      }

      let text = self.input_bar.text();
      let cursor = self.input_bar.cursor();
      let ranges = self.input_bar.protected_ranges().to_vec();

      let transition = self
          .trigger_detector
          .step(event, &text, cursor, &ranges);

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
              self.picker_shell = None;
          }
      }
  }
  ```

  Note: cloning `ranges` into a `Vec` here is needed because `self.trigger_detector.step` takes `&mut self` on the detector and `&[ProtectedRange]` — borrowing directly from `self.input_bar` would collide if step is called in the same statement. The clone is only paid when the fast path doesn't trip, which is exactly when we are about to build/update a `PickerShell` anyway (rare compared to baseline events).

- [ ] **Step 8.3: Update the `handle_key` call site at session_detail.rs:926.**

  Replace:

  ```rust
  if self.input_bar.handle_key(key).is_some() {
      if let Some((text, ranges, interrupt)) = self.input_bar.take_submit_capture() {
          // ... existing submit handling ...
      }
      return None;
  }

  // Key was an ordinary edit (insert/delete/arrow). Re-evaluate popup state.
  self.refresh_popup();
  ```

  With:

  ```rust
  use crate::components::completion_trigger::IntentEvent;
  use crate::components::input_bar::HandleOutcome;
  match self.input_bar.handle_key(key) {
      HandleOutcome::Submit(_, _) => {
          // Notify detector before processing submit (the detector doesn't
          // care about the text; this just retires any open composition).
          self.dispatch_intent(IntentEvent::Submitted);
          if let Some((text, ranges, interrupt)) = self.input_bar.take_submit_capture() {
              // ... existing submit handling preserved below ...
          }
          return None;
      }
      HandleOutcome::Key(intent) => {
          self.dispatch_intent(intent);
      }
  }
  ```

  The existing body of the `if self.input_bar.take_submit_capture()` block (SubmitRouter dispatch, SendMessage construction, etc.) is preserved verbatim. Only the outer control flow changes.

- [ ] **Step 8.4: Emit `Pasted` / `SetText` / `Accepted` / `Dismissed` at the non-key call sites.**

  Add `self.dispatch_intent(IntentEvent::…)` immediately after each of these lines (adjust imports to bring `IntentEvent` into scope locally):

  - Line 239 (`self.input_bar.set_text(draft.to_string(), draft.len());`): append `self.dispatch_intent(crate::components::completion_trigger::IntentEvent::SetText);`
  - Line 351 (`self.input_bar.insert_paste(text);`): append `self.dispatch_intent(crate::components::completion_trigger::IntentEvent::Pasted);`
  - Line 650 (inside `replace_trigger_token`, after `self.input_bar.set_text(new_text, new_cursor);`): append `self.dispatch_intent(crate::components::completion_trigger::IntentEvent::SetText);`
    - Note: `replace_trigger_token` is called from the picker-accept path; the subsequent `Accepted` emission there supersedes this `SetText`. Emit both — the detector will Close on the first terminal event and NoOp-safe on the second.
  - Line 793 (`self.input_bar.history_prev();`): append `SetText` dispatch
  - Line 797 (`self.input_bar.history_next();`): append `SetText` dispatch
  - Line 866 (`self.input_bar.set_state(snap, len);` inside picker-accept): append `Accepted` dispatch
  - Line 877 (`self.input_bar.insert_atom(text, uri, name);`): append `Accepted` dispatch
  - Line 859 (`self.picker_shell = None;` inside `PickerAction::Cancel` arm): append `Dismissed` dispatch
  - Line 887 (second `self.picker_shell = None;` inside `PickerAction::Accept` arm): already covered by the per-accept `Accepted` dispatch above; do NOT emit a redundant `Dismissed`.

  In each case, also remove any call to `self.trigger_detector.reset()` — the `Dismissed` / `Accepted` event drives detector state. (Check lines 860, 875, 887 in the original file.)

- [ ] **Step 8.5: Verify crate compiles.**

  Run: `cargo check -p spur-tui`
  Expected: only errors remaining are in `views/dashboard.rs` (fixed next task).

- [ ] **Step 8.6: Commit.**

  ```bash
  git add crates/spur-tui/src/views/session_detail.rs
  git commit -m "refactor(session-detail): consume HandleOutcome + dispatch_intent

  refresh_popup() deleted. dispatch_intent() centralises detector
  dispatch and includes the Idle fast-path. Non-key edit paths
  (paste, set_text, set_state, insert_atom, history swap, picker
  accept/cancel, submit) now emit explicit IntentEvents."
  ```

---

## Task 9: Update `dashboard.rs` to match new return type

**Files:**
- Modify: `crates/spur-tui/src/views/dashboard.rs:783`

`dashboard.rs` has no picker, so it doesn't need `dispatch_intent`. It only needs to accept the new return type.

- [ ] **Step 9.1: Update the call site.**

  Replace:

  ```rust
  if let Some((text, interrupt)) = self.input_bar.handle_key(key) {
      // ... existing submit handling ...
  }
  ```

  With:

  ```rust
  use crate::components::input_bar::HandleOutcome;
  if let HandleOutcome::Submit(text, interrupt) = self.input_bar.handle_key(key) {
      // ... existing submit handling unchanged ...
  }
  ```

- [ ] **Step 9.2: Verify full crate compiles.**

  Run: `cargo check -p spur-tui`
  Expected: clean build.

- [ ] **Step 9.3: Commit.**

  ```bash
  git add crates/spur-tui/src/views/dashboard.rs
  git commit -m "refactor(dashboard): consume HandleOutcome::Submit"
  ```

---

## Task 10: Integration test for end-to-end Composition-Intent flow

**Files:**
- Create: `crates/spur-tui/tests/composition_intent_integration.rs`

This test drives `InputBar::handle_key` directly and asserts on the returned `IntentEvent`. It does not instantiate the full session_detail view — the `dispatch_intent` fast-path is unit-testable via detector unit tests; here we verify the **classifier** (per-key intent mapping) is correct end-to-end.

- [ ] **Step 10.1: Create the test file.**

  ```rust
  use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
  use spur_tui::components::completion_trigger::IntentEvent;
  use spur_tui::components::input_bar::{HandleOutcome, InputBar};

  fn press(bar: &mut InputBar, code: KeyCode) -> IntentEvent {
      match bar.handle_key(KeyEvent::new(code, KeyModifiers::NONE)) {
          HandleOutcome::Key(e) => e,
          HandleOutcome::Submit(_, _) => panic!("unexpected submit"),
      }
  }

  fn press_ctrl(bar: &mut InputBar, c: char) -> IntentEvent {
      match bar.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)) {
          HandleOutcome::Key(e) => e,
          HandleOutcome::Submit(_, _) => panic!("unexpected submit"),
      }
  }

  #[test]
  fn arrow_keys_classify_as_moved_cursor() {
      let mut bar = InputBar::new();
      bar.set_text("abcdef".into(), 3);
      assert!(matches!(press(&mut bar, KeyCode::Left), IntentEvent::MovedCursor));
      assert!(matches!(press(&mut bar, KeyCode::Right), IntentEvent::MovedCursor));
      assert!(matches!(press(&mut bar, KeyCode::Up), IntentEvent::MovedCursor));
      assert!(matches!(press(&mut bar, KeyCode::Down), IntentEvent::MovedCursor));
  }

  #[test]
  fn backspace_classifies_as_deleted_char() {
      let mut bar = InputBar::new();
      bar.set_text("abc".into(), 3);
      assert!(matches!(press(&mut bar, KeyCode::Backspace), IntentEvent::DeletedChar));
  }

  #[test]
  fn delete_classifies_as_deleted_char() {
      let mut bar = InputBar::new();
      bar.set_text("abc".into(), 0);
      assert!(matches!(press(&mut bar, KeyCode::Delete), IntentEvent::DeletedChar));
  }

  #[test]
  fn ctrl_k_classifies_as_deleted_char() {
      let mut bar = InputBar::new();
      bar.set_text("hello world".into(), 5);
      assert!(matches!(press_ctrl(&mut bar, 'k'), IntentEvent::DeletedChar));
  }

  #[test]
  fn ctrl_u_classifies_as_deleted_char() {
      let mut bar = InputBar::new();
      bar.set_text("hello world".into(), 5);
      assert!(matches!(press_ctrl(&mut bar, 'u'), IntentEvent::DeletedChar));
  }

  #[test]
  fn ctrl_w_classifies_as_deleted_char() {
      let mut bar = InputBar::new();
      bar.set_text("hello world".into(), 11);
      assert!(matches!(press_ctrl(&mut bar, 'w'), IntentEvent::DeletedChar));
  }

  #[test]
  fn printable_char_classifies_as_typed_char_with_value() {
      let mut bar = InputBar::new();
      match bar.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)) {
          HandleOutcome::Key(IntentEvent::TypedChar('x')) => {}
          other => panic!("expected TypedChar('x'), got {other:?}"),
      }
  }

  #[test]
  fn at_sign_classifies_as_typed_char_at() {
      let mut bar = InputBar::new();
      match bar.handle_key(KeyEvent::new(KeyCode::Char('@'), KeyModifiers::NONE)) {
          HandleOutcome::Key(IntentEvent::TypedChar('@')) => {}
          other => panic!("expected TypedChar('@'), got {other:?}"),
      }
  }

  #[test]
  fn enter_on_nonempty_returns_submit() {
      let mut bar = InputBar::new();
      bar.set_text("hello".into(), 5);
      match bar.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
          HandleOutcome::Submit(t, interrupt) => {
              assert_eq!(t, "hello");
              assert!(!interrupt);
          }
          other => panic!("expected Submit, got {other:?}"),
      }
  }

  #[test]
  fn enter_on_empty_returns_noop() {
      let mut bar = InputBar::new();
      match bar.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
          HandleOutcome::Key(IntentEvent::NoOp) => {}
          other => panic!("expected NoOp, got {other:?}"),
      }
  }

  #[test]
  fn home_end_classify_as_moved_cursor() {
      let mut bar = InputBar::new();
      bar.set_text("abc".into(), 3);
      assert!(matches!(press(&mut bar, KeyCode::Home), IntentEvent::MovedCursor));
      assert!(matches!(press(&mut bar, KeyCode::End), IntentEvent::MovedCursor));
  }
  ```

  Note: `KeyCode::Home` / `KeyCode::End` currently fall through the explicit match in `handle_emacs_input` into `self.textarea.input(input)` — that branch returns `HandleOutcome::Key(IntentEvent::NoOp)` today. If this test fails with `NoOp`, add explicit `KeyCode::Home` / `KeyCode::End` arms to `handle_emacs_input` returning `MovedCursor`. The test is the forcing function.

- [ ] **Step 10.2: Run the integration test.**

  Run: `cargo test -p spur-tui --test composition_intent_integration -- --nocapture`
  Expected: all 11 tests pass. If `home_end_classify_as_moved_cursor` fails, fix per the note above and re-run.

- [ ] **Step 10.3: Commit.**

  ```bash
  git add crates/spur-tui/tests/composition_intent_integration.rs crates/spur-tui/src/components/input_bar.rs
  git commit -m "test(composition-intent): classifier integration coverage"
  ```

---

## Task 11: Regression pass — full workspace tests + lint

**Files:** none

- [ ] **Step 11.1: Run the entire spur-tui test suite.**

  Run: `cargo test -p spur-tui`
  Expected: all tests pass. If any legacy test fails because it pattern-matched on the old `Option<(String, bool)>` return, update the match to use `HandleOutcome`.

- [ ] **Step 11.2: Run workspace tests (catches unexpected cross-crate coupling).**

  Run: `cargo test --workspace`
  Expected: clean.

- [ ] **Step 11.3: Run clippy on the touched crate.**

  Run: `cargo clippy -p spur-tui --all-targets -- -D warnings`
  Expected: clean. Fix any new warnings inline.

- [ ] **Step 11.4: Run rustfmt.**

  Run: `cargo fmt -p spur-tui`
  Expected: no diff. If there is a diff, commit it.

- [ ] **Step 11.5: Manual smoke check (dev terminal).**

  Run the TUI interactively and verify:
  - Type `@`, picker opens. Type `f`, list refines. Esc closes. Arrow-back does NOT re-open.
  - Type `@foo`, accept a mention (atom inserted). Walk cursor with ← / → across the atom — picker does NOT re-open.
  - Paste text containing a bare `@user` substring — picker does NOT open.
  - Press Ctrl+R (or Up/Down to recall) — history entry loads, picker does NOT trigger-open.

- [ ] **Step 11.6: Commit any trailing fixes.**

  ```bash
  git add -A
  git commit -m "chore(composition-intent): clippy/fmt cleanup + test updates"
  ```

---

## Task 12: Final review

- [ ] **Step 12.1: Re-read the design spec and confirm every section is implemented.**

  Open `docs/superpowers/specs/2026-04-20-composition-intent-trigger-design.md`. For each section (Goal, UX Invariants I1–I5, State machine, Module boundaries, Intent classification table, Edge cases, Data flow, Performance contract, Error handling, Testing strategy, What is removed), identify at least one task step that implements it. If any section has no corresponding implementation, add a follow-up task before merging.

- [ ] **Step 12.2: Ensure the branch is ready.**

  Run: `git log --oneline main..HEAD`
  Expected: a linear sequence of commits, each scoped per the task structure.

- [ ] **Step 12.3: Open PR (optional — user decides).**

  Do NOT open a PR automatically. Ask the user whether to open one.
