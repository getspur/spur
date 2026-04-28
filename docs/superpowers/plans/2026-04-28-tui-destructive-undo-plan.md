# TUI Destructive-Action Undo (Tombstone Model)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the Gmail-toast-style tombstone undo model for every key that durably mutates external state in `spur-tui`. After this work: every destructive action commits immediately and shows a `"…Press u to undo (60s)"` toast; pressing `u` (vim) or `Ctrl+Z` (emacs) either dispatches the inverse action or cancels a queued-remote dispatch; one tombstone slot per view; triple-Esc clears all tombstones without dispatch.

**Architecture:** Single `TombstoneSlots` struct (new module at `crates/spur-tui/src/components/tombstone.rs`) owned by `App`. Tombstone install fires in `App::process_action` arms, NOT inside views. Inverse actions and queued-remote actions both dispatch through the same `process_action` path so error-handling, beads writes, and UI refresh remain on the normal codepath. The 33ms `App::tick` loop drives tombstone expiry. `flash_hint` / `flash_hint_short` are provided by the quick-fixes spec §4.11 (ships same release).

**Tech Stack:** Rust 2021, `std::time::Instant`, `std::collections::HashMap`, `std::time::Duration`. No new external crates.

**Scope:** This plan implements only the destructive-undo tombstone feature. Quick-fixes spec commits 1–11 (which provide `Action::PanicReset`, `App::flash_hint`, and the `d`→`x` migration) must already be merged before Task 3 of this plan.

---

## Spec Grounding

- Spec: `/Volumes/Projects/spur/docs/superpowers/specs/2026-04-28-tui-destructive-undo-design.md`
- `ViewId` at `crates/spur-tui/src/action.rs:188` — derives `Debug, Clone, PartialEq, Eq`; needs `Hash` added.
- `Action::IssueAction::UpdateStatus { id: String, status: String }` at `action.rs:7`.
- `Action::ToggleSessionArchive` at `action.rs:55`, `Action::ToggleSessionPin` at `action.rs:51`, `Action::RenameSession` at `action.rs:61`.
- `Action::SubmitReview` at `action.rs:124`; dispatched from `dashboard.rs:1112` and `dashboard.rs:1238`; both flow through `App::process_action` at `app.rs:2212`.
- `App::process_action` at `app.rs:1613`; `App::tick` at `app.rs:2539`.
- `App` struct at `app.rs:191`; existing fields include `edit_mode: EditMode` (app.rs:245), `current_view: ViewId` (app.rs:192), `issue_browser: Option<IssueBrowserView>` (app.rs:197).
- `RenameState` at `session_picker.rs:113` — fields `session_id: String`, `buffer: String`; needs `original_title: String` added.
- `IssueBrowserView::tracked_issues: Vec<spur_pm::IssueSummary>` (issue_browser.rs:61); `IssueSummary.status: String` (spur-pm/src/types.rs:52).
- `components/mod.rs` — module list to add `tombstone` entry.
- `App::process_action` arm for `ToggleSessionPin` at `app.rs:1968`, `ToggleSessionArchive` at `app.rs:1976`, `RenameSession` at `app.rs:1991`, `SubmitReview` at `app.rs:2212`.
- Key routing for global keys happens at `app.rs:870–1033`; `current_view()` accessor at `app.rs:2534`.
- Quick-fixes §4.11 provides `App::flash_hint(msg, duration)` and `App::flash_hint_short(msg)` — prerequisite.
- Quick-fixes §4.10 provides `Action::PanicReset` — prerequisite.

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-tui/src/action.rs` | Modify | Add `Hash` to `ViewId` derive at line 188 (Task 1) |
| `crates/spur-tui/src/components/tombstone.rs` | Create | `Tombstone`, `TombstoneKind`, `TombstoneSlots` with `install`, `evict`, `tick`, `cancel_all_without_dispatch` (Task 2) |
| `crates/spur-tui/src/components/mod.rs` | Modify | Declare `pub mod tombstone` (Task 2) |
| `crates/spur-tui/src/components/status_bar.rs` | Modify | Add `render_tombstone_badge(Option<&Tombstone>, Instant) -> Line<'a>` for Channel A (Task 3.5, spec §4.10) |
| `crates/spur-tui/src/app.rs` | Modify | Add `tombstones: TombstoneSlots` field; wire tick; wire undo handler with §4.6 gate cascade; wire per-action install arms; wire `PanicReset` arm; pass tombstone peek into status-bar render |
| `crates/spur-tui/src/views/session_picker.rs` | Modify | Add `original_title: String` to `RenameState` at line 113; capture at `Post::StartRename` at line 1502 |
| `crates/spur-tui/tests/tombstone_unit.rs` | Create | Unit tests for `TombstoneSlots` (install, evict, tick, expiry, cancel) (Task 2) |
| `crates/spur-tui/tests/tombstone_badge_render.rs` | Create | Render tests for ambient countdown badge (Task 3.5) |
| `crates/spur-tui/tests/tombstone_integration.rs` | Create | App-level integration tests for each action class and undo paths (Task 3 onward) |

**Amended task ordering (post-2026-04-28 first-principles UX review):**

1. Task 1: `Hash` derive on `ViewId` — DONE.
2. Task 2: `TombstoneSlots` module + unit tests — DONE.
3. Task 3: `App.tombstones` field + tick wire — IN FLIGHT.
4. **Task 3.5 (NEW): Channel A ambient badge render helper.** Must land BEFORE Task 4 so the undo handler has somewhere to display the countdown.
5. Task 4: undo handler with §4.6 ownership-cascade gate (10 contexts enumerated).
6. Tasks 5–9: per-action install arms.
7. Tasks 10–12: PanicReset, deprecation toast, workspace sweep.

The two-channel split (spec §4.8) means Tasks 5–9 must call BOTH channels at install:
- Channel A (badge): `tombstones.install(...)` — handled by App.
- Channel B (flash): `app.flash_hint(install_message, Duration::from_secs(2))` — install confirmation flash.

---

## Task 1 — Add `Hash` derive to `ViewId`

**Files:**
- Modify: `crates/spur-tui/src/action.rs:188`

- [ ] **Step 1: Write a failing compile-level test.**

  Create `crates/spur-tui/tests/view_id_hash.rs`:
  ```rust
  // Verifies ViewId can be used as a HashMap key.
  use std::collections::HashMap;
  use spur_tui::action::ViewId;

  #[test]
  fn view_id_is_hashable() {
      let mut map: HashMap<ViewId, u32> = HashMap::new();
      map.insert(ViewId::Dashboard, 1);
      map.insert(ViewId::IssueBrowser, 2);
      assert_eq!(map[&ViewId::Dashboard], 1);
      assert_eq!(map[&ViewId::IssueBrowser], 2);
  }
  ```

- [ ] **Step 2: Run the test to confirm it fails to compile.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test view_id_hash
  ```
  Expected: compile error — `ViewId` does not implement `Hash`.

- [ ] **Step 3: Apply the one-line patch.**

  At `crates/spur-tui/src/action.rs:188`, change:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum ViewId {
  ```
  to:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, Hash)]
  pub enum ViewId {
  ```
  `SessionId` is `String`-backed and derives `Hash`; all inner fields hash trivially.

- [ ] **Step 4: Run the test to confirm it passes.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test view_id_hash
  ```
  Expected: 1 test passes.

- [ ] **Step 5: Verify no regressions across spur-tui.**

  ```bash
  scripts/spur-cargo check -p spur-tui
  ```
  Expected: clean compile.

- [ ] **Step 6: Commit.**

  ```bash
  git add crates/spur-tui/src/action.rs crates/spur-tui/tests/view_id_hash.rs
  git commit -m "feat(spur-tui): add Hash derive to ViewId for tombstone HashMap key"
  ```

**Acceptance Criteria:**
- `ViewId` derives `Hash`.
- `HashMap<ViewId, _>` compiles without error.
- All existing `spur-tui` tests continue to pass.

---

## Task 2 — Create `tombstone.rs` module

**Files:**
- Create: `crates/spur-tui/src/components/tombstone.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`

- [ ] **Step 1: Write the unit test file first.**

  Create `crates/spur-tui/tests/tombstone_unit.rs`:
  ```rust
  use std::time::{Duration, Instant};
  use spur_tui::action::{Action, IssueAction, ViewId};
  use spur_tui::components::tombstone::{Tombstone, TombstoneKind, TombstoneSlots};

  fn reversible_tombstone(view: ViewId, now: Instant, window: Duration) -> Tombstone {
      Tombstone {
          view: view.clone(),
          kind: TombstoneKind::Reversible {
              inverse: Action::ToggleSessionArchive {
                  session_id: "sess-1".into(),
              },
          },
          label: "Archived 'test'".into(),
          created_at: now,
          expires_at: now + window,
      }
  }

  fn queued_tombstone(view: ViewId, now: Instant, window: Duration) -> Tombstone {
      Tombstone {
          view: view.clone(),
          kind: TombstoneKind::QueuedRemote {
              pending: Action::SubmitReview {
                  executor_id: "exec-1".into(),
                  attempt_n: 1,
                  decision: spur_core::ReviewDecision::Approve,
              },
          },
          label: "Approving…".into(),
          created_at: now,
          expires_at: now + window,
      }
  }

  #[test]
  fn install_and_evict_returns_tombstone() {
      let mut slots = TombstoneSlots::new();
      let now = Instant::now();
      slots.install(reversible_tombstone(ViewId::SessionPicker, now, Duration::from_secs(60)));
      let t = slots.evict(&ViewId::SessionPicker);
      assert!(t.is_some());
      assert!(slots.evict(&ViewId::SessionPicker).is_none());
  }

  #[test]
  fn install_replaces_prior_tombstone_for_same_view() {
      let mut slots = TombstoneSlots::new();
      let now = Instant::now();
      let first = reversible_tombstone(ViewId::SessionPicker, now, Duration::from_secs(60));
      let mut second = reversible_tombstone(ViewId::SessionPicker, now, Duration::from_secs(60));
      second.label = "Archived 'second'".into();
      slots.install(first);
      slots.install(second);
      let t = slots.evict(&ViewId::SessionPicker).unwrap();
      assert_eq!(t.label, "Archived 'second'");
  }

  #[test]
  fn tick_drops_expired_reversible_without_dispatch() {
      let mut slots = TombstoneSlots::new();
      let now = Instant::now();
      slots.install(reversible_tombstone(
          ViewId::SessionPicker,
          now,
          Duration::from_millis(1),
      ));
      let future = now + Duration::from_millis(10);
      let dispatched = slots.tick(future);
      assert!(dispatched.is_empty(), "reversible expiry must not dispatch anything");
      assert!(slots.evict(&ViewId::SessionPicker).is_none(), "tombstone must be evicted");
  }

  #[test]
  fn tick_dispatches_queued_remote_on_expiry() {
      let mut slots = TombstoneSlots::new();
      let now = Instant::now();
      slots.install(queued_tombstone(
          ViewId::Dashboard,
          now,
          Duration::from_millis(1),
      ));
      let future = now + Duration::from_millis(10);
      let dispatched = slots.tick(future);
      assert_eq!(dispatched.len(), 1);
      assert!(matches!(dispatched[0], Action::SubmitReview { .. }));
      assert!(slots.evict(&ViewId::Dashboard).is_none());
  }

  #[test]
  fn cancel_all_without_dispatch_drops_queued_without_emitting() {
      let mut slots = TombstoneSlots::new();
      let now = Instant::now();
      slots.install(queued_tombstone(
          ViewId::Dashboard,
          now,
          Duration::from_secs(3),
      ));
      slots.cancel_all_without_dispatch();
      // tick after cancel must not dispatch anything
      let dispatched = slots.tick(now + Duration::from_secs(10));
      assert!(dispatched.is_empty());
      assert!(slots.evict(&ViewId::Dashboard).is_none());
  }

  #[test]
  fn per_view_isolation_separate_slots() {
      let mut slots = TombstoneSlots::new();
      let now = Instant::now();
      slots.install(reversible_tombstone(ViewId::SessionPicker, now, Duration::from_secs(60)));
      assert!(slots.evict(&ViewId::Dashboard).is_none(), "Dashboard must have no tombstone");
      assert!(slots.evict(&ViewId::SessionPicker).is_some());
  }

  #[test]
  fn install_replaces_and_returns_displaced_queued_for_immediate_dispatch() {
      let mut slots = TombstoneSlots::new();
      let now = Instant::now();
      let first = queued_tombstone(ViewId::Dashboard, now, Duration::from_secs(3));
      slots.install(first);
      // Installing a second queued tombstone for same view should displace first
      let displaced = slots.install_and_get_displaced(queued_tombstone(
          ViewId::Dashboard,
          now,
          Duration::from_secs(3),
      ));
      assert!(displaced.is_some());
      assert!(matches!(
          displaced.unwrap().kind,
          TombstoneKind::QueuedRemote { .. }
      ));
  }
  ```

- [ ] **Step 2: Run the test to confirm it fails (module not yet defined).**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_unit
  ```
  Expected: compile error — `spur_tui::components::tombstone` does not exist.

- [ ] **Step 3: Create `crates/spur-tui/src/components/tombstone.rs`.**

  ```rust
  //! Tombstone model for Gmail-toast-style destructive-action undo.
  //!
  //! One slot per view. `TombstoneSlots` is owned by `App` and driven
  //! by the 33ms tick loop. Install/evict/tick are all O(views) which is
  //! bounded to ~6 entries.

  use std::collections::HashMap;
  use std::time::Instant;

  use crate::action::{Action, ViewId};

  /// A single tombstone entry for one destructive action.
  #[derive(Debug, Clone)]
  pub struct Tombstone {
      pub view: ViewId,
      pub kind: TombstoneKind,
      /// Human-readable description of the action (used in toast copy).
      pub label: String,
      pub created_at: Instant,
      /// Wall-clock deadline. Reversible: 60s. QueuedRemote: 3s.
      pub expires_at: Instant,
  }

  /// Determines what happens on undo and on expiry.
  #[derive(Debug, Clone)]
  pub enum TombstoneKind {
      /// Action already committed. `u` dispatches `inverse` through
      /// `App::process_action`. Closure-based revert rejected (spec §4.5):
      /// beads-backed mutations must go through the normal dispatch path.
      Reversible { inverse: Action },

      /// Action is client-queued; not yet dispatched. `u` drops it silently.
      /// Expiry dispatches `pending` through `App::process_action`.
      QueuedRemote { pending: Action },
  }

  /// Per-view tombstone store. One slot per `ViewId`.
  #[derive(Debug, Default)]
  pub struct TombstoneSlots {
      by_view: HashMap<ViewId, Tombstone>,
  }

  impl TombstoneSlots {
      pub fn new() -> Self {
          Self::default()
      }

      /// Install a tombstone, overwriting any prior tombstone for the same view.
      /// The displaced tombstone (if any) is discarded silently.
      /// Callers that need to dispatch a displaced QueuedRemote immediately
      /// should use `install_and_get_displaced` instead.
      pub fn install(&mut self, tombstone: Tombstone) {
          self.by_view.insert(tombstone.view.clone(), tombstone);
      }

      /// Install a tombstone, returning the prior tombstone for the same view
      /// if one existed. The caller is responsible for dispatching any displaced
      /// `QueuedRemote` immediately (spec §4.3 bullet 6: new action displaces
      /// old queue slot, old slot must fire before its 3s expires).
      pub fn install_and_get_displaced(&mut self, tombstone: Tombstone) -> Option<Tombstone> {
          self.by_view.insert(tombstone.view.clone(), tombstone)
      }

      /// Remove and return the tombstone for the given view, if any.
      /// Called by the undo handler when the user presses `u` / `Ctrl+Z`.
      pub fn evict(&mut self, view: &ViewId) -> Option<Tombstone> {
          self.by_view.remove(view)
      }

      /// Drive expiry. Called from `App::tick` on every 33ms frame.
      ///
      /// Expired reversible tombstones are silently dropped (action already
      /// committed; nothing to do). Expired `QueuedRemote` tombstones are
      /// removed and their `pending` action is returned for the caller to
      /// dispatch through `App::process_action`.
      pub fn tick(&mut self, now: Instant) -> Vec<Action> {
          let mut to_dispatch = Vec::new();
          self.by_view.retain(|_view, ts| {
              if now >= ts.expires_at {
                  if let TombstoneKind::QueuedRemote { ref pending } = ts.kind {
                      to_dispatch.push(pending.clone());
                  }
                  false // evict
              } else {
                  true // keep
              }
          });
          to_dispatch
      }

      /// Drop ALL tombstones without dispatching anything.
      ///
      /// Called by `Action::PanicReset` (quick-fixes spec §4.10). Reversible
      /// tombstones have already committed, so dropping them just prevents undo.
      /// QueuedRemote tombstones are cancelled — the action is never sent.
      /// This is the intended escape hatch: the user pressed triple-Esc because
      /// they want out, and the queued-remote action is collateral.
      pub fn cancel_all_without_dispatch(&mut self) {
          self.by_view.clear();
      }

      /// Returns true if a tombstone is active for the given view.
      pub fn has(&self, view: &ViewId) -> bool {
          self.by_view.contains_key(view)
      }

      /// Returns the active tombstone for a view without removing it
      /// (used by the render layer for countdown display).
      pub fn peek(&self, view: &ViewId) -> Option<&Tombstone> {
          self.by_view.get(view)
      }
  }
  ```

- [ ] **Step 4: Declare the module in `crates/spur-tui/src/components/mod.rs`.**

  Add after `pub mod spinner;` (line 38):
  ```rust
  pub mod tombstone;
  ```

- [ ] **Step 5: Run the unit tests.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_unit
  ```
  Expected: all 7 tests pass.

- [ ] **Step 6: Clippy + fmt.**

  ```bash
  scripts/spur-cargo clippy -p spur-tui -- -D warnings
  scripts/spur-cargo fmt -p spur-tui -- --check
  ```

- [ ] **Step 7: Commit.**

  ```bash
  git add crates/spur-tui/src/components/tombstone.rs \
          crates/spur-tui/src/components/mod.rs \
          crates/spur-tui/tests/tombstone_unit.rs
  git commit -m "feat(spur-tui): TombstoneSlots module — install/evict/tick/cancel_all_without_dispatch"
  ```

**Acceptance Criteria:**
- `TombstoneSlots` compiles and all 7 unit tests pass.
- `tick` returns only `QueuedRemote` pending actions on expiry; reversible expiry returns nothing.
- `cancel_all_without_dispatch` leaves `tick` with no dispatches even for `QueuedRemote`.
- `install_and_get_displaced` returns the displaced tombstone for caller to handle.

---

## Task 3 — Add `tombstones` field to `App` and wire tick

**Prerequisite:** Quick-fixes commits 1–11 merged (provides `App::flash_hint`, `App::flash_hint_short`, `App::transient_hint`, `Action::PanicReset`).

**Files:**
- Modify: `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Write a failing integration test asserting the field exists.**

  Create `crates/spur-tui/tests/tombstone_integration.rs` with a minimal test:
  ```rust
  // Integration tests for tombstone behavior at the App level.
  // Uses App's test-support constructor (same pattern as existing app tests).
  // Additional tests added in Tasks 5-10.

  #[test]
  fn tombstone_slots_field_accessible_via_tick() {
      // Smoke: App::tick runs without panic when tombstone slot is empty.
      // Full tick wire-up is tested by per-action integration tests.
      // This test exists to fail until Task 3 lands.
      let mut app = spur_tui::App::new_for_test();
      app.tick(); // must not panic
  }
  ```

- [ ] **Step 2: Run to confirm it fails (field/method not yet present).**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  ```
  Expected: compile error.

- [ ] **Step 3: Add `tombstones` field to `App` struct at `app.rs:191`.**

  In the `pub struct App { … }` block, add after the `edit_mode` field (app.rs:245):
  ```rust
  /// Per-view tombstone slots for Gmail-toast-style destructive-action undo.
  /// Driven by tick; install points live in process_action arms.
  tombstones: crate::components::tombstone::TombstoneSlots,
  ```

- [ ] **Step 4: Initialize in `App::new` (or `App::new_for_test`).**

  Find the `App { … }` struct literal in the constructor (around app.rs:398). Add:
  ```rust
  tombstones: crate::components::tombstone::TombstoneSlots::new(),
  ```

- [ ] **Step 5: Wire `TombstoneSlots::tick` into `App::tick` at `app.rs:2539`.**

  At the top of `App::tick`, before the `#[cfg(feature = "markdown")]` block, add:
  ```rust
  // Drive tombstone expiry. Expired reversible tombstones are silently
  // dropped (action already committed). Expired QueuedRemote tombstones
  // are dispatched now — this is the 3s deferred-dispatch path for
  // irrevocable remote actions (SubmitReview).
  let now = std::time::Instant::now();
  let expired_queued = self.tombstones.tick(now);
  for action in expired_queued {
      self.process_action(action);
  }

  // Drive transient-hint expiry (quick-fixes §4.11).
  self.tick_transient_hint(now);
  ```

  Note: `tick_transient_hint` is provided by the quick-fixes spec prerequisite.

- [ ] **Step 6: Run the integration test.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  ```
  Expected: 1 test passes.

- [ ] **Step 7: Run full spur-tui test suite for regressions.**

  ```bash
  scripts/spur-cargo test -p spur-tui
  ```

- [ ] **Step 8: Commit.**

  ```bash
  git add crates/spur-tui/src/app.rs crates/spur-tui/tests/tombstone_integration.rs
  git commit -m "feat(spur-tui): add tombstones field to App + wire tick expiry dispatch"
  ```

**Acceptance Criteria:**
- `App` has `tombstones: TombstoneSlots` field.
- `App::tick` calls `tombstones.tick(now)` and dispatches any expired `QueuedRemote` actions via `process_action`.
- Existing `App::tick` behavior (mermaid drain, permission deadline, dirty-mark) is unchanged.

---

## Task 3.5 — Render ambient countdown badge (Channel A, spec §4.10)

**Spec amendment 2026-04-28**: tombstone display split into two channels. Channel A is the ambient badge in the status bar, right-aligned, showing `[u: archived 'foo' 45s]`. This task adds the render helper and wires it into the status bar build path.

**Files:**
- Modify: `crates/spur-tui/src/components/status_bar.rs` (or whichever module owns status-bar rendering — verify with `grep -n "render_status_bar\|StatusBar" crates/spur-tui/src/`)
- Modify: `crates/spur-tui/src/app.rs` (pass tombstone peek result into status-bar render context)
- Create: `crates/spur-tui/tests/tombstone_badge_render.rs`

- [ ] **Step 1: Write failing render test.**

  Create `crates/spur-tui/tests/tombstone_badge_render.rs`:
  ```rust
  use std::time::{Duration, Instant};
  use spur_tui::action::ViewId;
  use spur_tui::components::tombstone::{Tombstone, TombstoneKind, TombstoneSlots};

  #[test]
  fn badge_renders_when_current_view_matches_slot() {
      let mut slots = TombstoneSlots::new();
      let now = Instant::now();
      slots.install(Tombstone {
          view: ViewId::SessionPicker,
          kind: TombstoneKind::Reversible {
              inverse: spur_tui::action::Action::ToggleSessionArchive {
                  session_id: "s1".into(),
                  via_legacy_key: false,
              },
          },
          label: "archived 'foo'".into(),
          created_at: now,
          expires_at: now + Duration::from_secs(45),
      });
      let badge = spur_tui::components::status_bar::render_tombstone_badge(
          slots.peek(&ViewId::SessionPicker),
          now,
      );
      let text = format!("{}", badge);  // ratatui::text::Line Display impl
      assert!(text.contains("[u:"), "expected `[u:` prefix, got: {text}");
      assert!(text.contains("archived 'foo'"), "expected label, got: {text}");
      assert!(text.contains("45s"), "expected countdown, got: {text}");
  }

  #[test]
  fn badge_returns_empty_line_when_slot_is_none() {
      let now = Instant::now();
      let badge = spur_tui::components::status_bar::render_tombstone_badge(None, now);
      let text = format!("{}", badge);
      assert!(text.is_empty(), "expected empty line, got: {text}");
  }

  #[test]
  fn badge_uses_revert_verb_for_queued_remote() {
      let mut slots = TombstoneSlots::new();
      let now = Instant::now();
      slots.install(Tombstone {
          view: ViewId::Dashboard,
          kind: TombstoneKind::QueuedRemote {
              pending: spur_tui::action::Action::SubmitReview {
                  executor_id: "x".into(),
                  attempt_n: 1,
                  decision: spur_core::ReviewDecision::Approve,
              },
          },
          label: "Approve".into(),
          created_at: now,
          expires_at: now + Duration::from_secs(2),
      });
      let badge = spur_tui::components::status_bar::render_tombstone_badge(
          slots.peek(&ViewId::Dashboard),
          now,
      );
      let text = format!("{}", badge);
      assert!(text.contains("revert"), "expected `revert` verb, got: {text}");
      assert!(text.contains("2s"), "expected 2s countdown, got: {text}");
  }

  #[test]
  fn badge_truncates_long_labels() {
      let mut slots = TombstoneSlots::new();
      let now = Instant::now();
      let long = "archived 'verylongsessionnametotest'";
      slots.install(Tombstone {
          view: ViewId::SessionPicker,
          kind: TombstoneKind::Reversible {
              inverse: spur_tui::action::Action::ToggleSessionArchive {
                  session_id: "s1".into(),
                  via_legacy_key: false,
              },
          },
          label: long.into(),
          created_at: now,
          expires_at: now + Duration::from_secs(60),
      });
      let badge = spur_tui::components::status_bar::render_tombstone_badge(
          slots.peek(&ViewId::SessionPicker),
          now,
      );
      let text = format!("{}", badge);
      assert!(text.contains("…"), "expected ellipsis truncation, got: {text}");
      assert!(text.len() <= 40, "badge text too long: {} chars in: {text}", text.len());
  }
  ```

- [ ] **Step 2: Confirm tests fail (helper not defined).**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_badge_render
  ```
  Expected: compile error — `render_tombstone_badge` does not exist.

- [ ] **Step 3: Implement `render_tombstone_badge` in `status_bar.rs`.**

  Public function signature:
  ```rust
  pub fn render_tombstone_badge<'a>(
      slot: Option<&crate::components::tombstone::Tombstone>,
      now: std::time::Instant,
  ) -> ratatui::text::Line<'a> {
      let Some(t) = slot else {
          return ratatui::text::Line::default();
      };
      let remaining = t.expires_at.saturating_duration_since(now);
      let secs = remaining.as_secs();
      let verb = match t.kind {
          crate::components::tombstone::TombstoneKind::Reversible { .. } => "u",
          crate::components::tombstone::TombstoneKind::QueuedRemote { .. } => "u: revert",
      };
      let label = if t.label.chars().count() > 24 {
          let mut truncated: String = t.label.chars().take(23).collect();
          truncated.push('…');
          truncated
      } else {
          t.label.clone()
      };
      ratatui::text::Line::from(vec![
          ratatui::text::Span::styled(
              format!("  [{}: {} {}s]", verb, label, secs),
              ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray),
          ),
      ])
  }
  ```

  Note: the `verb` returns `"u"` for Reversible (so output reads `[u: archived 'foo' 45s]`) and `"u: revert"` for QueuedRemote (so output reads `[u: revert Approve 2s]`). Verify against spec §4.10 examples.

- [ ] **Step 4: Wire badge into status-bar render path.**

  In the existing status-bar render code (likely `StatusBar::render` or a similar method), append the badge to the right-aligned segment AFTER the license badge and BEFORE any clock/right-edge element. Pass the tombstone peek as part of `StatusBarProps` or add a new field.

  The `App` render code that constructs `StatusBarProps` must compute:
  ```rust
  let tombstone_for_view = self.tombstones.peek(self.current_view());
  ```
  and pass it through.

- [ ] **Step 5: Run badge tests + spur-tui suite.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_badge_render
  scripts/spur-cargo test -p spur-tui
  ```

- [ ] **Step 6: Clippy + fmt.**

  ```bash
  scripts/spur-cargo clippy -p spur-tui -- -D warnings
  scripts/spur-cargo fmt -p spur-tui -- --check
  ```

- [ ] **Step 7: Commit.**

  ```bash
  git add crates/spur-tui/src/components/status_bar.rs \
          crates/spur-tui/src/app.rs \
          crates/spur-tui/tests/tombstone_badge_render.rs
  git commit -m "feat(spur-tui): tombstone ambient countdown badge in status bar (Channel A)"
  ```

**Acceptance Criteria:**
- `render_tombstone_badge(Option<&Tombstone>, Instant) -> Line<'a>` exists in `status_bar.rs`.
- Badge renders `[u: <label> <Ns>]` for Reversible and `[u: revert <label> <Ns>]` for QueuedRemote.
- Badge truncates labels > 24 chars with ellipsis.
- Returns empty `Line` when slot is `None`.
- Badge appears in status bar render output ONLY when `current_view == slot.view`.
- All 4 unit tests pass + spur-tui suite green.

---

## Task 4 — Implement `u` / `Ctrl+Z` undo handler at app level

**Spec amendment 2026-04-28**: §4.6 now enumerates an explicit activation-gate ownership cascade. The undo handler MUST short-circuit BEFORE evicting the tombstone for any of these contexts:

| # | Gate (early return) | `u` flows to | Slot lifecycle |
|---|---|---|---|
| 1 | `input_bar.is_active() && !input_bar.is_empty()` | input_bar (text-undo via tui-textarea) | unchanged |
| 2 | mention picker open (`@`-trigger) | picker | unchanged |
| 3 | slash command picker open (`/`-trigger) | picker | unchanged |
| 4 | history shell active (Up/Down history nav showing body) | history shell | unchanged |
| 5 | permission prompt pending (`y/n/a` waiting) | permission handler | unchanged |
| 6 | help overlay open (`?`) | block + flash `"close help to undo"` | unchanged |
| 7 | mermaid render-picker open | render-picker | unchanged |
| 8 | quit-confirm modal open | block (no-op) | unchanged |
| 9 | leader-menu popup (post-leader-key spec) | block (no-op) | unchanged |
| 10 | none of the above | tombstone undo (consume) | evicted |

This cascade matches quick-fixes T5's owner-order discipline (composer-non-empty > picker > history-shell > view-keys). The implementation MUST grep the existing context-check helpers and reuse them — do NOT introduce new boolean flags.

**Files:**
- Modify: `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Extend `tombstone_integration.rs` with undo-handler tests.**

  Append to `crates/spur-tui/tests/tombstone_integration.rs`:
  ```rust
  use std::time::{Duration, Instant};
  use spur_tui::action::{Action, ViewId};
  use spur_tui::components::tombstone::{Tombstone, TombstoneKind, TombstoneSlots};

  #[test]
  fn undo_with_no_tombstone_flashes_nothing_to_undo() {
      let mut app = spur_tui::App::new_for_test();
      // Navigate to SessionPicker view (no tombstone installed).
      app.process_action(Action::NavigateTo(ViewId::SessionPicker));
      // Simulate pressing u.
      app.handle_undo_for_test();
      // Transient hint must say "nothing to undo".
      assert!(
          app.transient_hint_text().unwrap_or("").contains("nothing to undo"),
          "expected 'nothing to undo' hint"
      );
  }

  #[test]
  fn undo_reversible_tombstone_dispatches_inverse() {
      let mut app = spur_tui::App::new_for_test();
      let now = Instant::now();
      app.tombstones_for_test().install(Tombstone {
          view: ViewId::SessionPicker,
          kind: TombstoneKind::Reversible {
              inverse: Action::ToggleSessionArchive { session_id: "s1".into() },
          },
          label: "Archived 'foo'".into(),
          created_at: now,
          expires_at: now + Duration::from_secs(60),
      });
      app.process_action(Action::NavigateTo(ViewId::SessionPicker));
      app.handle_undo_for_test();
      // Tombstone evicted.
      assert!(!app.tombstones_for_test().has(&ViewId::SessionPicker));
      // Inverse was dispatched.
      assert!(matches!(
          app.last_action_for_test(),
          Some(Action::ToggleSessionArchive { .. })
      ));
      // Hint says "Undid: …".
      assert!(app.transient_hint_text().unwrap_or("").contains("Undid"));
  }

  #[test]
  fn undo_queued_remote_cancels_without_dispatch() {
      let mut app = spur_tui::App::new_for_test();
      let now = Instant::now();
      app.tombstones_for_test().install(Tombstone {
          view: ViewId::Dashboard,
          kind: TombstoneKind::QueuedRemote {
              pending: Action::SubmitReview {
                  executor_id: "exec-1".into(),
                  attempt_n: 1,
                  decision: spur_core::ReviewDecision::Approve,
              },
          },
          label: "Approving…".into(),
          created_at: now,
          expires_at: now + Duration::from_secs(3),
      });
      // Already on Dashboard.
      app.handle_undo_for_test();
      assert!(!app.tombstones_for_test().has(&ViewId::Dashboard));
      // SubmitReview was NOT dispatched.
      assert!(!matches!(
          app.last_action_for_test(),
          Some(Action::SubmitReview { .. })
      ));
      assert!(app.transient_hint_text().unwrap_or("").contains("Cancelled"));
  }

  #[test]
  fn undo_in_compose_mode_does_not_consume_tombstone() {
      // When input bar is in Vim Insert or Emacs typing, u/Ctrl+Z must NOT
      // consume the tombstone — they flow to the input bar instead.
      // This test verifies the gate via edit_mode.
      let mut app = spur_tui::App::new_for_test();
      let now = Instant::now();
      app.tombstones_for_test().install(Tombstone {
          view: ViewId::SessionPicker,
          kind: TombstoneKind::Reversible {
              inverse: Action::ToggleSessionArchive { session_id: "s1".into() },
          },
          label: "Archived 'foo'".into(),
          created_at: now,
          expires_at: now + Duration::from_secs(60),
      });
      app.process_action(Action::NavigateTo(ViewId::SessionPicker));
      // Set vim insert mode (simulates composer active).
      app.set_edit_mode_for_test(spur_tui::components::input_bar::EditMode::Vim(
          spur_tui::components::input_bar::VimMode::Insert,
      ));
      app.handle_undo_for_test(); // must be a no-op from tombstone perspective
      // Tombstone still present.
      assert!(app.tombstones_for_test().has(&ViewId::SessionPicker));
  }
  ```

- [ ] **Step 2: Run to confirm tests fail.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  ```
  Expected: compile error — test-support methods not yet defined.

- [ ] **Step 3: Add `handle_undo` private method and test-support accessors to `App`.**

  In `crates/spur-tui/src/app.rs`, add the following:

  **Private `handle_undo` method** (near the global key handler block, around line 870):
  ```rust
  /// Undo handler for `u` (vim Normal) and `Ctrl+Z` (emacs).
  ///
  /// Gated: fires only when input bar is NOT composing (vim Normal or
  /// emacs idle). In Vim Insert or active-composition, `u`/`Ctrl+Z` must
  /// pass through to the input bar for text undo.
  fn handle_undo(&mut self) {
      // Guard: do not consume when vim Insert is active.
      let is_composing = matches!(
          self.edit_mode,
          EditMode::Vim(crate::components::input_bar::VimMode::Insert)
      );
      if is_composing {
          return; // let input bar handle text undo
      }

      let view = self.current_view.clone();
      let Some(tombstone) = self.tombstones.evict(&view) else {
          self.flash_hint_short("nothing to undo");
          return;
      };

      match tombstone.kind {
          TombstoneKind::Reversible { inverse } => {
              self.flash_hint_short(format!("Undid: {}", tombstone.label));
              self.process_action(inverse);
          }
          TombstoneKind::QueuedRemote { pending: _ } => {
              // Drop the pending action; it is never dispatched.
              self.flash_hint_short(format!("Cancelled: {}", tombstone.label));
          }
      }
  }
  ```

  Note: import `TombstoneKind` at the top of `app.rs`:
  ```rust
  use crate::components::tombstone::TombstoneKind;
  ```

  **Wire `u` and `Ctrl+Z` in the global key handler** (around the existing global hotkey dispatch at `app.rs:904`, after the quit-chord check and before the upgrade-modal block):
  ```rust
  // `u` in vim Normal or `Ctrl+Z` in emacs — destructive-action undo.
  // Must run BEFORE view dispatch so it isn't swallowed by view handlers.
  // Guarded inside handle_undo() against composer-active state.
  let is_undo = (key.code == KeyCode::Char('u') && key.modifiers.is_empty())
      || (key.code == KeyCode::Char('z')
          && key.modifiers == KeyModifiers::CONTROL);
  if is_undo {
      self.handle_undo();
      self.dirty = true;
      return;
  }
  ```

  **Test-support accessors** (behind `#[cfg(any(test, debug_assertions))]`):
  ```rust
  #[cfg(any(test, debug_assertions))]
  pub fn handle_undo_for_test(&mut self) {
      self.handle_undo();
  }

  #[cfg(any(test, debug_assertions))]
  pub fn tombstones_for_test(&mut self) -> &mut crate::components::tombstone::TombstoneSlots {
      &mut self.tombstones
  }

  #[cfg(any(test, debug_assertions))]
  pub fn transient_hint_text(&self) -> Option<&str> {
      self.transient_hint.as_ref().map(|h| h.text.as_str())
  }

  #[cfg(any(test, debug_assertions))]
  pub fn set_edit_mode_for_test(&mut self, mode: EditMode) {
      self.edit_mode = mode;
  }

  #[cfg(any(test, debug_assertions))]
  pub fn last_action_for_test(&self) -> Option<&Action> {
      self.last_action.as_ref()
  }
  ```

- [ ] **Step 4: Run the integration tests.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  ```
  Expected: all 4 new tests pass (plus the earlier smoke test = 5 total).

- [ ] **Step 5: Clippy + fmt.**

  ```bash
  scripts/spur-cargo clippy -p spur-tui -- -D warnings
  scripts/spur-cargo fmt -p spur-tui -- --check
  ```

- [ ] **Step 6: Commit.**

  ```bash
  git add crates/spur-tui/src/app.rs crates/spur-tui/tests/tombstone_integration.rs
  git commit -m "feat(spur-tui): u / Ctrl+Z undo handler at app level with view-owner gating"
  ```

**Acceptance Criteria:**
- `u` (vim Normal) and `Ctrl+Z` (emacs idle) trigger `handle_undo`.
- Pressing `u` in Vim Insert mode does NOT consume the tombstone.
- Empty tombstone slot flashes `"nothing to undo"`.
- Reversible tombstone dispatches inverse action and flashes `"Undid: …"`.
- QueuedRemote tombstone drops pending action and flashes `"Cancelled: …"`.

---

## Task 5 — Wire `Action::ToggleSessionArchive` tombstone install

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (around line 1976)

- [ ] **Step 1: Extend `tombstone_integration.rs` with archive tests.**

  Append to `crates/spur-tui/tests/tombstone_integration.rs`:
  ```rust
  #[test]
  fn tombstone_installs_on_archive_with_60s_window() {
      let mut app = spur_tui::App::new_for_test();
      let before = std::time::Instant::now();
      app.process_action(Action::ToggleSessionArchive { session_id: "s1".into() });
      let ts = app.tombstones_for_test().peek(&ViewId::SessionPicker);
      assert!(ts.is_some(), "tombstone must be installed after archive");
      let ts = ts.unwrap();
      assert!(ts.expires_at >= before + Duration::from_secs(59));
      assert!(ts.label.contains("s1") || ts.label.contains("Archive"));
      assert!(matches!(ts.kind, TombstoneKind::Reversible { .. }));
  }

  #[test]
  fn tombstone_undo_archive_redispatches_toggle() {
      let mut app = spur_tui::App::new_for_test();
      app.process_action(Action::ToggleSessionArchive { session_id: "s1".into() });
      app.process_action(Action::NavigateTo(ViewId::SessionPicker));
      app.handle_undo_for_test();
      // Inverse is same variant (toggle is its own inverse).
      assert!(matches!(
          app.last_action_for_test(),
          Some(Action::ToggleSessionArchive { .. })
      ));
  }
  ```

- [ ] **Step 2: Run to confirm tests fail.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  ```
  Expected: tests fail — no tombstone installed yet.

- [ ] **Step 3: Wrap the `ToggleSessionArchive` arm in `process_action` at `app.rs:1976`.**

  The existing arm:
  ```rust
  Action::ToggleSessionArchive { session_id } => {
      let entry = self.metadata_store.entry_mut(&session_id);
      entry.archived = !entry.archived;
      self.persist_metadata("archive toggle");
      self.refresh_picker_metadata();
      self.dirty = true;
  }
  ```
  Replace with:
  ```rust
  Action::ToggleSessionArchive { ref session_id } => {
      // Install tombstone BEFORE mutating so label captures pre-state.
      // Inverse is the same action (toggling twice returns to original).
      let label = format!("Archived '{}'", session_id);
      let now = std::time::Instant::now();
      let inverse = Action::ToggleSessionArchive { session_id: session_id.clone() };
      let displaced = self.tombstones.install_and_get_displaced(
          crate::components::tombstone::Tombstone {
              view: ViewId::SessionPicker,
              kind: crate::components::tombstone::TombstoneKind::Reversible { inverse },
              label: label.clone(),
              created_at: now,
              expires_at: now + std::time::Duration::from_secs(60),
          },
      );
      // A prior QueuedRemote displaced by this install must be dropped
      // (archive is reversible; no queued-remote in SessionPicker).
      let _ = displaced; // reversible displacement is silently discarded

      let entry = self.metadata_store.entry_mut(session_id);
      entry.archived = !entry.archived;
      self.persist_metadata("archive toggle");
      self.refresh_picker_metadata();
      self.flash_hint(
          format!("{}. Press u to undo (60s)", label),
          std::time::Duration::from_secs(60),
      );
      self.dirty = true;
  }
  ```

- [ ] **Step 4: Run the tests.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  ```

- [ ] **Step 5: Commit.**

  ```bash
  git add crates/spur-tui/src/app.rs crates/spur-tui/tests/tombstone_integration.rs
  git commit -m "feat(spur-tui): tombstone install for ToggleSessionArchive (60s reversible)"
  ```

**Acceptance Criteria:**
- `Action::ToggleSessionArchive` arm installs a 60s `Reversible` tombstone on `ViewId::SessionPicker`.
- Inverse is `Action::ToggleSessionArchive { session_id }` (toggle is self-inverse).
- `flash_hint` text includes session id and countdown copy.
- Underlying metadata mutation (`entry.archived`) still fires.

---

## Task 6 — Wire `Action::ToggleSessionPin` tombstone install

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (around line 1968)

- [ ] **Step 1: Append pin test to `tombstone_integration.rs`.**

  ```rust
  #[test]
  fn tombstone_installs_on_pin_with_60s_window() {
      let mut app = spur_tui::App::new_for_test();
      app.process_action(Action::ToggleSessionPin { session_id: "s2".into() });
      let ts = app.tombstones_for_test().peek(&ViewId::SessionPicker);
      assert!(ts.is_some());
      assert!(matches!(ts.unwrap().kind, TombstoneKind::Reversible { .. }));
  }
  ```

- [ ] **Step 2: Run to confirm failure.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  ```

- [ ] **Step 3: Wrap the `ToggleSessionPin` arm at `app.rs:1968`.**

  ```rust
  Action::ToggleSessionPin { ref session_id } => {
      let label = format!("Pinned toggle for '{}'", session_id);
      let now = std::time::Instant::now();
      let inverse = Action::ToggleSessionPin { session_id: session_id.clone() };
      self.tombstones.install(crate::components::tombstone::Tombstone {
          view: ViewId::SessionPicker,
          kind: crate::components::tombstone::TombstoneKind::Reversible { inverse },
          label: label.clone(),
          created_at: now,
          expires_at: now + std::time::Duration::from_secs(60),
      });
      let entry = self.metadata_store.entry_mut(session_id);
      entry.pinned = !entry.pinned;
      self.persist_metadata("pin toggle");
      self.refresh_picker_metadata();
      self.flash_hint(
          format!("{}. Press u to undo (60s)", label),
          std::time::Duration::from_secs(60),
      );
      self.dirty = true;
  }
  ```

- [ ] **Step 4: Run and commit.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  git add crates/spur-tui/src/app.rs crates/spur-tui/tests/tombstone_integration.rs
  git commit -m "feat(spur-tui): tombstone install for ToggleSessionPin (60s reversible)"
  ```

**Acceptance Criteria:**
- `Action::ToggleSessionPin` arm installs a 60s `Reversible` tombstone on `ViewId::SessionPicker`.
- Inverse is `Action::ToggleSessionPin { session_id }`.
- Metadata write still fires.

---

## Task 7 — Wire `Action::RenameSession` tombstone install + add `RenameState.original_title`

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs` (lines 113 and 1502)
- Modify: `crates/spur-tui/src/app.rs` (around line 1991)

The rename inverse requires the title that was in place BEFORE the rename. This must be captured when the rename mode is entered (at `Post::StartRename`, line 1502), not at commit time.

- [ ] **Step 1: Append rename test.**

  ```rust
  #[test]
  fn tombstone_installs_on_rename_with_original_title_as_inverse() {
      let mut app = spur_tui::App::new_for_test();
      // Dispatch a rename. The RenameSession action carries original_title
      // (populated by the view from RenameState.original_title).
      app.process_action(Action::RenameSession {
          session_id: "s3".into(),
          new_title: "New Name".into(),
          original_title: "Old Name".into(),
      });
      let ts = app.tombstones_for_test().peek(&ViewId::SessionPicker);
      assert!(ts.is_some());
      let ts = ts.unwrap();
      // Inverse must be RenameSession with old title.
      match &ts.kind {
          TombstoneKind::Reversible { inverse } => {
              assert!(matches!(
                  inverse,
                  Action::RenameSession { new_title, .. } if new_title == "Old Name"
              ));
          }
          _ => panic!("expected Reversible tombstone"),
      }
  }
  ```

- [ ] **Step 2: Run to confirm failure.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  ```
  Expected: compile error — `Action::RenameSession` does not have `original_title` field yet.

- [ ] **Step 3: Add `original_title` field to `Action::RenameSession` in `action.rs:61`.**

  ```rust
  /// Commit an inline rename from the picker to metadata `title_override`.
  RenameSession {
      session_id: String,
      new_title: String,
      /// Title that was in place before this rename. Used by the tombstone
      /// undo system to construct the inverse action. Populated from
      /// `RenameState.original_title` at the time the rename is committed.
      original_title: String,
  },
  ```

- [ ] **Step 4: Add `original_title` to `RenameState` in `session_picker.rs:113`.**

  ```rust
  struct RenameState {
      session_id: String,
      buffer: String,
      /// The display label in place when rename mode was entered.
      /// Used to construct the tombstone inverse action.
      original_title: String,
  }
  ```

- [ ] **Step 5: Capture `original_title` at `Post::StartRename` (session_picker.rs:1480).**

  The existing code at line 1480:
  ```rust
  post = Post::StartRename {
      session_id: sid.clone(),
      buffer,
  };
  ```
  The `buffer` variable at this point already holds the resolved display label (see the `resolve_label` call at lines 1463–1478). Use that as `original_title`:
  ```rust
  post = Post::StartRename {
      session_id: sid.clone(),
      original_title: buffer.clone(), // capture before the user edits
      buffer,
  };
  ```
  Update `Post::StartRename` enum variant (if it is a local enum) to carry `original_title: String`.

  Update `Post::StartRename` match arm at line 1502:
  ```rust
  Post::StartRename { session_id, buffer, original_title } => {
      self.rename_state = Some(RenameState { session_id, buffer, original_title });
  }
  ```

- [ ] **Step 6: Update the rename commit site to emit `original_title`.**

  Find where `Action::RenameSession` is emitted from `session_picker.rs` (the `Enter` key in rename mode). Add `original_title: rename_state.original_title.clone()` to the emitted action.

- [ ] **Step 7: Update the `RenameSession` arm in `app.rs:1991` to install tombstone.**

  ```rust
  Action::RenameSession {
      ref session_id,
      ref new_title,
      ref original_title,
  } => {
      let label = format!("Renamed '{}' → '{}'", original_title, new_title);
      let now = std::time::Instant::now();
      let inverse = Action::RenameSession {
          session_id: session_id.clone(),
          new_title: original_title.clone(),    // restore to previous title
          original_title: new_title.clone(),    // new→old for any future undo of the undo
      };
      self.tombstones.install(crate::components::tombstone::Tombstone {
          view: ViewId::SessionPicker,
          kind: crate::components::tombstone::TombstoneKind::Reversible { inverse },
          label: label.clone(),
          created_at: now,
          expires_at: now + std::time::Duration::from_secs(60),
      });
      let entry = self.metadata_store.entry_mut(session_id);
      entry.title_override = if new_title.trim().is_empty() {
          None
      } else {
          Some(new_title.clone())
      };
      self.persist_metadata("rename");
      self.refresh_picker_metadata();
      self.flash_hint(
          format!("{}. Press u to undo (60s)", label),
          std::time::Duration::from_secs(60),
      );
      self.dirty = true;
  }
  ```

- [ ] **Step 8: Fix any compile errors caused by the new `original_title` field (update all `Action::RenameSession { … }` match arms in the codebase).**

  ```bash
  scripts/spur-cargo check -p spur-tui
  ```
  Grep for any other match arms:
  ```bash
  rg "RenameSession" crates/spur-tui/src/
  ```
  Add `original_title` (or `..`) to any exhaustive matches.

- [ ] **Step 9: Run tests.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  scripts/spur-cargo test -p spur-tui
  ```

- [ ] **Step 10: Commit.**

  ```bash
  git add crates/spur-tui/src/action.rs \
          crates/spur-tui/src/views/session_picker.rs \
          crates/spur-tui/src/app.rs \
          crates/spur-tui/tests/tombstone_integration.rs
  git commit -m "feat(spur-tui): tombstone install for RenameSession + RenameState.original_title capture"
  ```

**Acceptance Criteria:**
- `RenameState` has `original_title: String` field.
- `original_title` is captured at `Post::StartRename` time (before editing begins).
- `Action::RenameSession` carries `original_title`.
- `app.rs` arm installs 60s reversible tombstone; inverse sets `new_title = original_title`.
- All existing rename tests still pass.

---

## Task 8 — Wire `Action::Issue(IssueAction::UpdateStatus)` tombstone install

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (find the `Action::Issue(IssueAction::UpdateStatus { … })` arm)

The previous-status capture reads from `IssueBrowserView::tracked_issues` before the dispatch mutates it. `IssueSummary.status` is a `String` (confirmed at `spur-pm/src/types.rs:52`).

- [ ] **Step 1: Append issue-status tests.**

  ```rust
  #[test]
  fn tombstone_installs_on_issue_status_update_with_previous_status() {
      let mut app = spur_tui::App::new_for_test();
      // Pre-load a tracked issue into the issue_browser (test helper needed).
      app.set_tracked_issues_for_test(vec![spur_pm::IssueSummary {
          id: "ISSUE-1".into(),
          source: spur_pm::PmSource::Local,
          title: "Fix bug".into(),
          status: "open".into(),
          labels: vec![],
          url: "".into(),
          priority: None,
          issue_type: None,
      }]);
      app.process_action(Action::NavigateTo(ViewId::IssueBrowser));
      app.process_action(Action::Issue(IssueAction::UpdateStatus {
          id: "ISSUE-1".into(),
          status: "closed".into(),
      }));
      let ts = app.tombstones_for_test().peek(&ViewId::IssueBrowser);
      assert!(ts.is_some(), "tombstone must be installed for issue status update");
      let ts = ts.unwrap();
      match &ts.kind {
          TombstoneKind::Reversible { inverse } => {
              assert!(matches!(
                  inverse,
                  Action::Issue(IssueAction::UpdateStatus { status, .. }) if status == "open"
              ));
          }
          _ => panic!("expected Reversible"),
      }
  }
  ```

- [ ] **Step 2: Run to confirm failure.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  ```

- [ ] **Step 3: Locate the `IssueAction::UpdateStatus` arm in `app.rs`.**

  Find the arm that handles `Action::Issue(IssueAction::UpdateStatus { … })` (search near the `Issue` arm in process_action). If the dispatch goes through `IssueAction::UpdateStatus` arm, wrap it. If issue-status dispatch flows through a PM service call, insert the tombstone before that call.

- [ ] **Step 4: Add tombstone install to the `IssueAction::UpdateStatus` arm.**

  ```rust
  Action::Issue(IssueAction::UpdateStatus { ref id, ref status }) => {
      // Capture previous status from tracked_issues BEFORE the write.
      let previous_status: String = self
          .issue_browser
          .as_ref()
          .and_then(|v| {
              v.tracked_issues()
                  .iter()
                  .find(|issue| issue.id == *id)
                  .map(|issue| issue.status.clone())
          })
          .unwrap_or_else(|| "open".into());

      let label = format!("issue '{}' → {}", id, status);
      let now = std::time::Instant::now();
      let inverse = Action::Issue(IssueAction::UpdateStatus {
          id: id.clone(),
          status: previous_status,
      });
      self.tombstones.install(crate::components::tombstone::Tombstone {
          view: ViewId::IssueBrowser,
          kind: crate::components::tombstone::TombstoneKind::Reversible { inverse },
          label: label.clone(),
          created_at: now,
          expires_at: now + std::time::Duration::from_secs(60),
      });
      self.flash_hint(
          format!("{}. Press u to undo (60s)", label),
          std::time::Duration::from_secs(60),
      );

      // Existing dispatch path continues below (PM write, UI refresh).
      // … existing arm body unchanged …
  }
  ```

  Note: if the existing arm performs a PM backend write, leave that code intact below the tombstone install.

  Also add test-support accessor for `set_tracked_issues_for_test`:
  ```rust
  #[cfg(any(test, debug_assertions))]
  pub fn set_tracked_issues_for_test(&mut self, issues: Vec<spur_pm::IssueSummary>) {
      use crate::views::View;
      if self.issue_browser.is_none() {
          self.issue_browser = Some(crate::views::issue_browser::IssueBrowserView::new());
      }
      if let Some(ref mut browser) = self.issue_browser {
          browser.set_issues_for_test(issues);
      }
  }
  ```

  And add `pub fn set_issues_for_test` to `IssueBrowserView` in `issue_browser.rs`:
  ```rust
  #[cfg(any(test, debug_assertions))]
  pub fn set_issues_for_test(&mut self, issues: Vec<spur_pm::IssueSummary>) {
      self.tracked_issues = issues;
  }
  ```

- [ ] **Step 5: Run tests.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  ```

- [ ] **Step 6: Commit.**

  ```bash
  git add crates/spur-tui/src/app.rs \
          crates/spur-tui/src/views/issue_browser.rs \
          crates/spur-tui/tests/tombstone_integration.rs
  git commit -m "feat(spur-tui): tombstone install for IssueAction::UpdateStatus with status snapshot"
  ```

**Acceptance Criteria:**
- `IssueAction::UpdateStatus` arm installs a 60s reversible tombstone on `ViewId::IssueBrowser`.
- `previous_status` is read from `tracked_issues` before the PM write.
- Inverse action is `UpdateStatus { id, status: previous_status }`.
- If inverse dispatch fails (PM write error), the existing error path surfaces the failure via `flash_hint`.

---

## Task 9 — Wire `Action::SubmitReview` queue install (3s deferred dispatch)

**Spec amendment 2026-04-28**: spec §4.5.1 now resolves the install-vs-dispatch re-entrance hazard via a NEW action variant `Action::SubmitReviewDispatch { … }` for the bare ACP send. The old `Action::SubmitReview` arm becomes install-only; the new `SubmitReviewDispatch` arm performs the actual send. Tombstone's `pending` field stores the *Dispatch* variant. The previously-proposed `executing_queued_review: bool` sentinel flag is REJECTED as a code smell (mutable shared state, panic-unsafe, magic behavior). Bifurcating into variants is type-safe, exception-safe, and self-documenting.

**Files:**
- Modify: `crates/spur-tui/src/action.rs` (add `SubmitReviewDispatch` variant)
- Modify: `crates/spur-tui/src/app.rs` (around line 2212 — bifurcate into install-arm + dispatch-arm)

This is the only `QueuedRemote` tombstone class. The user-press → `Action::SubmitReview` arm installs a tombstone with `pending = Action::SubmitReviewDispatch { … }` and does NOT actually send. The 3s tick-expiry calls `process_action(pending)` which hits the new `Action::SubmitReviewDispatch` arm — bare ACP send, no install. The displacement path (§4.3 bullet 6) also dispatches via the `SubmitReviewDispatch` variant directly.

`u` cancellation drops the tombstone (and its `pending` Dispatch variant) — never sent.

- [ ] **Step 1: Append review-queue tests.**

  ```rust
  #[test]
  fn tombstone_remote_queue_installs_and_does_not_dispatch_immediately() {
      let mut app = spur_tui::App::new_for_test();
      // Simulate a pending review node so the guard passes.
      app.add_pending_review_for_test("exec-1", 1);
      app.process_action(Action::SubmitReview {
          executor_id: "exec-1".into(),
          attempt_n: 1,
          decision: spur_core::ReviewDecision::Approve,
      });
      let ts = app.tombstones_for_test().peek(&ViewId::Dashboard);
      assert!(ts.is_some(), "tombstone must be installed");
      assert!(matches!(ts.unwrap().kind, TombstoneKind::QueuedRemote { .. }));
      // The user_input channel must NOT have received a SubmitReview yet.
      assert!(!app.user_input_sent_for_test(), "SubmitReview must not be dispatched during queue window");
  }

  #[test]
  fn tombstone_remote_queue_cancel_via_undo() {
      let mut app = spur_tui::App::new_for_test();
      app.add_pending_review_for_test("exec-1", 1);
      app.process_action(Action::SubmitReview {
          executor_id: "exec-1".into(),
          attempt_n: 1,
          decision: spur_core::ReviewDecision::Approve,
      });
      // Press u within 3s.
      app.handle_undo_for_test();
      assert!(!app.tombstones_for_test().has(&ViewId::Dashboard));
      assert!(!app.user_input_sent_for_test());
      assert!(app.transient_hint_text().unwrap_or("").contains("Cancelled"));
  }

  #[test]
  fn tombstone_remote_queue_displaced_by_next_review_dispatches_first_immediately() {
      let mut app = spur_tui::App::new_for_test();
      app.add_pending_review_for_test("exec-1", 1);
      app.add_pending_review_for_test("exec-2", 1);
      app.process_action(Action::SubmitReview {
          executor_id: "exec-1".into(),
          attempt_n: 1,
          decision: spur_core::ReviewDecision::Approve,
      });
      // Second review displaces first — first must dispatch immediately.
      app.process_action(Action::SubmitReview {
          executor_id: "exec-2".into(),
          attempt_n: 1,
          decision: spur_core::ReviewDecision::Reject,
      });
      // First review dispatched immediately.
      assert!(app.user_input_sent_for_test_with_executor("exec-1"));
      // Second review is in the queue.
      assert!(app.tombstones_for_test().has(&ViewId::Dashboard));
  }
  ```

- [ ] **Step 2: Run to confirm failure.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  ```

- [ ] **Step 3: Add `Action::SubmitReviewDispatch` variant to `action.rs`.**

  Locate the existing `Action::SubmitReview` variant. Add a sibling variant immediately after it:
  ```rust
  /// Bare ACP-dispatch path for SubmitReview. Constructed ONLY by the
  /// SubmitReview install arm (user-press) or by the displacement-flush
  /// path. The process_action arm for this variant performs the actual
  /// orchestrator send WITHOUT installing a tombstone. Tombstone's
  /// pending field stores this variant; tick-expiry dispatches it.
  /// Never emitted by views — strictly an internal dispatch primitive.
  SubmitReviewDispatch {
      executor_id: String,
      attempt_n: u32,
      decision: spur_core::ReviewDecision,
  },
  ```
  Update any exhaustive match on `Action` (search `match action` / `match self`) to handle the new variant — most can route the same as `SubmitReview` for everything except the actual dispatch site.

- [ ] **Step 4: Rewrite the `SubmitReview` arm at `app.rs:2212` (install-only).**

  ```rust
  Action::SubmitReview {
      ref executor_id,
      attempt_n,
      ref decision,
  } => {
      let has_review = self
          .lineage
          .node(&spur_core::ExecutorId(executor_id.clone()))
          .map(|n| n.pending_review.is_some())
          .unwrap_or(false);
      if !has_review {
          tracing::warn!(executor_id = %executor_id, "SubmitReview ignored: no pending review on this node");
          return;
      }

      // Construct the bare-dispatch variant for the tombstone's pending field.
      let pending_dispatch = Action::SubmitReviewDispatch {
          executor_id: executor_id.clone(),
          attempt_n,
          decision: decision.clone(),
      };
      let decision_label = format!("{:?}", decision);
      let label = format!("{}…", decision_label);
      let now = std::time::Instant::now();

      // If a prior tombstone exists for Dashboard, displace it and dispatch
      // its pending action immediately (spec §4.3 bullet 6). The displaced
      // pending is already a SubmitReviewDispatch variant, so process_action
      // will hit the dispatch arm below — no re-installation, no recursion.
      let displaced = self.tombstones.install_and_get_displaced(
          crate::components::tombstone::Tombstone {
              view: ViewId::Dashboard,
              kind: crate::components::tombstone::TombstoneKind::QueuedRemote {
                  pending: pending_dispatch,
              },
              label: label.clone(),
              created_at: now,
              expires_at: now + std::time::Duration::from_secs(3),
          },
      );
      if let Some(displaced_ts) = displaced {
          if let crate::components::tombstone::TombstoneKind::QueuedRemote { pending } = displaced_ts.kind {
              self.process_action(pending);
          }
      }

      self.flash_hint(
          format!("{}. Press u to revert (3s)", label),
          std::time::Duration::from_secs(2),
      );
      self.dirty = true;
      // Install-only — no actual send. Send happens via tick-expiry or
      // displacement-flush, both of which dispatch SubmitReviewDispatch.
  }
  ```

- [ ] **Step 4b: Add the `SubmitReviewDispatch` arm — bare ACP send.**

  Append immediately after the `SubmitReview` arm:
  ```rust
  Action::SubmitReviewDispatch {
      ref executor_id,
      attempt_n,
      ref decision,
  } => {
      if let Some(ref tx) = self.user_input_tx {
          let _ = tx.try_send(UserInput::SubmitReview {
              executor_id: executor_id.clone(),
              attempt_n,
              decision: decision.clone(),
          });
      }
      // Optimistic local state update (preserved from old SubmitReview arm).
      self.lineage.apply(&spur_acp::SpurEvent::now(
          spur_acp::SpurEventBody::ExecutorReviewResolved {
              id: executor_id.clone(),
              decision: to_wire_decision(decision),
          },
      ));
      self.flash_hint_short("Sent.");
      self.dirty = true;
  }
  ```

  Note: the `App::tick` loop (added in Task 3) requires NO changes — it already calls `process_action(action)` for each expired pending. Since the pending is now a `SubmitReviewDispatch` variant, it routes to the bare-send arm with no re-installation. The sentinel-flag approach is no longer needed and must NOT be added.

- [ ] **Step 5: Run tests.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  ```

- [ ] **Step 6: Verify both `SubmitReview` emit sites in `dashboard.rs` are covered.**

  Both `dashboard.rs:1112` and `dashboard.rs:1238` emit `Action::SubmitReview`; both flow through `App::process_action`. No view-level changes needed — the single `process_action` arm covers both.

  ```bash
  rg "SubmitReview" crates/spur-tui/src/views/dashboard.rs
  ```
  Confirm the two emit sites still emit the action unchanged.

- [ ] **Step 7: Commit.**

  ```bash
  git add crates/spur-tui/src/app.rs crates/spur-tui/tests/tombstone_integration.rs
  git commit -m "feat(spur-tui): SubmitReview 3s queue tombstone — deferred dispatch via tick, u cancels"
  ```

**Acceptance Criteria:**
- `Action::SubmitReview` from Dashboard installs a 3s `QueuedRemote` tombstone; user_input channel receives nothing until 3s elapses.
- Pressing `u` within 3s cancels; `SubmitReview` is never dispatched.
- A second `SubmitReview` within 3s displaces the first; first dispatches immediately.
- `tick` dispatches the queued action after 3s expiry and flashes `"Sent."`.
- Both `dashboard.rs:1112` and `dashboard.rs:1238` are covered by the single `process_action` arm.

---

## Task 10 — Wire `Action::PanicReset` to call `tombstones.cancel_all_without_dispatch()`

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (the `Action::PanicReset` arm added by the quick-fixes spec)

The quick-fixes spec's `Action::PanicReset` arm sets overlay flags, navigates to Dashboard root, and resets views. This task adds the tombstone-cancel call to that arm.

- [ ] **Step 1: Append panic-reset tombstone test.**

  ```rust
  #[test]
  fn tombstone_panic_esc_cancels_queued_without_dispatch() {
      let mut app = spur_tui::App::new_for_test();
      app.add_pending_review_for_test("exec-1", 1);
      app.process_action(Action::SubmitReview {
          executor_id: "exec-1".into(),
          attempt_n: 1,
          decision: spur_core::ReviewDecision::Approve,
      });
      assert!(app.tombstones_for_test().has(&ViewId::Dashboard));
      // Triple-Esc fires PanicReset.
      app.process_action(Action::PanicReset);
      // Tombstone cleared.
      assert!(!app.tombstones_for_test().has(&ViewId::Dashboard));
      // SubmitReview was NOT dispatched.
      assert!(!app.user_input_sent_for_test());
  }
  ```

- [ ] **Step 2: Run to confirm failure.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  ```
  Expected: test fails — `PanicReset` arm does not yet call `cancel_all_without_dispatch`.

- [ ] **Step 3: Add `self.tombstones.cancel_all_without_dispatch()` to the `PanicReset` arm.**

  In the `Action::PanicReset` arm (added by quick-fixes; search for it in `app.rs`), add before the navigation reset:
  ```rust
  Action::PanicReset => {
      // 1. Clear overlay flags.
      self.quit_confirm_visible = false;
      self.collision_modal = None;
      self.upgrade_modal = None;
      self.help_visible = false;
      self.palette_visible = false;
      self.palette_state.dismiss(); // or reset(); verify method name

      // 2. Cancel all tombstones WITHOUT dispatching any queued-remote.
      //    Reversible tombstones have already committed; panic just prevents undo.
      //    QueuedRemote tombstones are dropped — action is never sent.
      self.tombstones.cancel_all_without_dispatch();

      // 3. Reset to Dashboard root.
      self.current_view = ViewId::Dashboard;
      self.dashboard.reset_to_root();
      if let Some(ref mut detail) = self.session_detail {
          detail.reset_to_root();
      }

      // 4. Clear esc chain (quick-fixes §4.10 responsibility).
      self.esc_chain.clear();

      self.flash_hint_short("Returned to Dashboard root");
      self.dirty = true;
  }
  ```

- [ ] **Step 4: Run tests.**

  ```bash
  scripts/spur-cargo test -p spur-tui --test tombstone_integration
  scripts/spur-cargo test -p spur-tui
  ```

- [ ] **Step 5: Commit.**

  ```bash
  git add crates/spur-tui/src/app.rs crates/spur-tui/tests/tombstone_integration.rs
  git commit -m "feat(spur-tui): PanicReset calls tombstones.cancel_all_without_dispatch"
  ```

**Acceptance Criteria:**
- `Action::PanicReset` calls `tombstones.cancel_all_without_dispatch()`.
- After `PanicReset`, any previously queued `SubmitReview` is not dispatched even after 3s.
- After `PanicReset`, `evict` on any `ViewId` returns `None`.

---

## Task 11 — Add deprecation toast on legacy `d` (consumes quick-fixes `flash_hint_short`)

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs` (around line 1456)
- Modify: `crates/spur-tui/src/views/issue_browser.rs` (around line 165)

This task wires the deprecation toast into the `d` key handler per quick-fixes spec §4.3. The `d` key still calls the same action as `x` (archive / status=closed) but also flashes the deprecation hint.

Note: since `App::flash_hint_short` is on `App`, and views only return `Action`, the deprecation signal must be surfaced via a dedicated action or via a side-channel. The simplest approach matching the existing pattern: add a new `Action::DeprecatedKey { hint: String }` or reuse the `flash_hint` by having the view return an action that `process_action` dispatches to `flash_hint_short`. Check if quick-fixes spec already defines this — if quick-fixes §4.3 uses a dedicated mechanism, follow it.

Alternatively (and more cleanly): the `d` key in SessionPicker and IssueBrowser emits the same action as `x` + a second action for the deprecation toast. Since views return `Option<Action>` (single action), the view must internally track `one_shot_d_deprecation_shown: bool` and set a field on the view so the next render call invokes the toast — or a second action is needed. The simplest approach that avoids the view needing access to `App::flash_hint_short` directly:

Add `Action::FlashHint { text: String, duration_ms: u64 }` (one-liner variant). `process_action` for this variant calls `flash_hint`. The `d` key emits a pair of actions; since views return `Option<Action>`, fold the deprecation into a view-level `one_shot_shown` flag and emit a combined `Action::Batch` — or use the existing per-view tick to schedule the hint.

**Practical decision:** keep it simple. Add a `one_shot_d_deprecation_shown: bool` field to `SessionPickerView` and `IssueBrowserView`. When `d` fires and the flag is false, set the flag and additionally emit `Action::FlashHintShort("d → archive renamed to x")`. Add `Action::FlashHintShort(String)` to `action.rs`. Handle in `process_action` with `self.flash_hint_short(text)`.

- [ ] **Step 1: Add `Action::FlashHintShort(String)` to `action.rs`.**

  ```rust
  /// Emitted by views to request a 2s transient hint flash without the
  /// view needing direct access to App::flash_hint_short.
  FlashHintShort(String),
  ```

- [ ] **Step 2: Handle in `app.rs::process_action`.**

  ```rust
  Action::FlashHintShort(text) => {
      self.flash_hint_short(text);
  }
  ```

- [ ] **Step 3: Add `one_shot_d_deprecation_shown: bool` to `SessionPickerView`.**

  Wire in the `d` arm (session_picker.rs:1456):
  ```rust
  KeyCode::Char('d') => {
      if !self.one_shot_d_deprecation_shown {
          self.one_shot_d_deprecation_shown = true;
          // Return the deprecation hint as the primary action;
          // archive fires on next keypress OR use x.
          // Per spec §4.3: d still archives, but also toasts.
          // Emit archive action; App will also receive the hint.
      }
      hl_session_id
          .clone()
          .map(|session_id| Action::ToggleSessionArchive { session_id })
      // Deprecation toast emitted as a second dispatch via App's own
      // post-action hook — see Step 4.
  }
  ```

- [ ] **Step 4: In the `ToggleSessionArchive` arm of `process_action`, check if action was triggered by `d` (deprecated).**

  This is hard to plumb without view-state access. Simpler: detect from the view's `one_shot` flag. Add a public query method to `SessionPickerView`:
  ```rust
  pub fn take_pending_deprecation_hint(&mut self) -> Option<String> {
      // Returns and clears any pending deprecation hint text.
      self.pending_deprecation_hint.take()
  }
  ```

  In the `d` arm, set `self.pending_deprecation_hint = Some("d → archive; use x instead (d removed in N+2)".into())` alongside the action return.

  In `App`'s event loop, after dispatching the action returned from a view's `handle_key`, drain `pending_deprecation_hint` from the active view and call `flash_hint_short`. This is the same pattern used by `SessionDetailView::take_pending_actions` at `app.rs:2584`.

  Apply the same to `IssueBrowserView` for its `d` → status=closed deprecation.

- [ ] **Step 5: Write tests.**

  ```rust
  // In a new test file or appended to tombstone_integration.rs:
  #[test]
  fn d_key_in_session_picker_shows_deprecation_hint_once() {
      let mut app = spur_tui::App::new_for_test();
      app.process_action(Action::NavigateTo(ViewId::SessionPicker));
      // Simulate pressing d.
      app.dispatch_d_in_session_picker_for_test();
      assert!(
          app.transient_hint_text().unwrap_or("").contains("x"),
          "deprecation hint must mention x"
      );
      // Second press: no hint (one-shot).
      app.clear_transient_hint_for_test();
      app.dispatch_d_in_session_picker_for_test();
      assert!(app.transient_hint_text().is_none(), "hint must be one-shot");
  }
  ```

- [ ] **Step 6: Run tests and commit.**

  ```bash
  scripts/spur-cargo test -p spur-tui
  git add crates/spur-tui/src/action.rs \
          crates/spur-tui/src/views/session_picker.rs \
          crates/spur-tui/src/views/issue_browser.rs \
          crates/spur-tui/src/app.rs
  git commit -m "feat(spur-tui): deprecation toast on legacy d key (one-shot, session_picker + issue_browser)"
  ```

**Acceptance Criteria:**
- Pressing `d` in SessionPicker archives and flashes `"d → archive; use x instead"` on first press only.
- Pressing `d` in IssueBrowser sets status=closed and flashes the same one-shot deprecation.
- Second press of `d` has no hint.
- `x` key has no deprecation hint.

---

## Task 12 — Verify all tests pass (workspace sweep)

**Files:** None (verification only).

- [ ] **Step 1: Run the full spur-tui test suite.**

  ```bash
  scripts/spur-cargo test -p spur-tui
  ```
  Expected: all tests pass, including pre-existing tests and all new tombstone tests.

- [ ] **Step 2: Run workspace-wide check.**

  ```bash
  scripts/spur-cargo check --workspace
  ```
  Expected: clean.

- [ ] **Step 3: Clippy strict on spur-tui.**

  ```bash
  scripts/spur-cargo clippy -p spur-tui -- -D warnings
  ```
  Expected: clean. Common issues to watch: unused `use` imports for `TombstoneKind`, dead-code warnings on test-only accessors (suppress with `#[cfg(any(test, debug_assertions))]`), `ref` patterns in match arms.

- [ ] **Step 4: Format check.**

  ```bash
  scripts/spur-cargo fmt -p spur-tui -- --check
  ```
  Expected: clean.

- [ ] **Step 5: Run affected downstream crates.**

  The `Action::RenameSession` field addition propagates to any crate that matches on that variant:
  ```bash
  rg "RenameSession" crates/ --include="*.rs"
  scripts/spur-cargo check --workspace
  ```
  Fix any exhaustive-match breakage in `spur-mcp`, `spur-acp`, or other consumers.

- [ ] **Step 6: If anything fails, fix before tagging the release branch.**

---

## Acceptance Criteria (Plan-Level)

- [ ] `ViewId` derives `Hash`; `HashMap<ViewId, _>` compiles.
- [ ] `TombstoneSlots` lives at `crates/spur-tui/src/components/tombstone.rs`; `pub mod tombstone` declared in `components/mod.rs`.
- [ ] `App` has `tombstones: TombstoneSlots` field; tick calls `tombstones.tick(now)` and dispatches expired `QueuedRemote` actions via `process_action`.
- [ ] `u` (vim Normal) and `Ctrl+Z` (emacs idle) invoke `handle_undo`; gated against vim Insert mode.
- [ ] `ToggleSessionArchive` arm installs 60s reversible tombstone on `ViewId::SessionPicker`; inverse is same variant.
- [ ] `ToggleSessionPin` arm installs 60s reversible tombstone on `ViewId::SessionPicker`; inverse is same variant.
- [ ] `RenameSession` arm installs 60s reversible tombstone; inverse uses `original_title`; `RenameState` captures title at mode-entry.
- [ ] `IssueAction::UpdateStatus` arm installs 60s reversible tombstone on `ViewId::IssueBrowser`; previous status captured from `tracked_issues`.
- [ ] `SubmitReview` arm installs 3s `QueuedRemote` tombstone; orchestrator channel receives nothing until expiry or displacement; `u` cancels; displacement dispatches prior tombstone immediately.
- [ ] `Action::PanicReset` calls `tombstones.cancel_all_without_dispatch()`; queued reviews not dispatched after panic.
- [ ] Legacy `d` in SessionPicker and IssueBrowser shows one-shot deprecation toast via `flash_hint_short`.
- [ ] All 13 spec test scenarios covered by automated tests.
- [ ] Workspace clean: build, tests, clippy `-D warnings`, fmt.
- [ ] Ships in same release branch as quick-fixes spec commits 1–11.

---

## Critical Implementation Details

**Error handling for undo failure:** If the inverse `Action` dispatched by `handle_undo` fails at the PM backend (e.g. beads write error), the existing `IssueAction::UpdateStatus` arm's error path fires and surfaces the error. To make the error message undo-specific, extend any existing error-flash call to include context: `flash_hint_short("Undo failed: {reason}; original action stands")`. No new infrastructure needed — the existing error path is sufficient.

**Hint-slot priority (spec §4.8):** The `flash_hint` / `transient_hint` field from quick-fixes §4.11 is the single hint slot. The render layer shows `transient_hint.text` when present. Priority is enforced by the quick-fixes `tick_transient_hint` eviction, which allows a higher-priority flash (panic reset) to overwrite a lower-priority one. Tombstone toasts occupy this slot for the full window (60s or 3s), so any subsequent flash (including the panic reset flash) overwrites it. This is the correct priority behavior per spec §4.8.

**View-change tombstone eviction:** The spec (§4.2 bullet 7, §4.3 bullet 7) requires tombstones to be evicted when the user navigates away. Wire this in the `Action::NavigateTo` and `Action::NavigateBack` arms of `process_action`: before setting `current_view`, evict the tombstone for the view being departed. For `QueuedRemote` tombstones, dispatch them immediately before evicting (same as displacement behavior). Add this to each `NavigateTo` arm:
```rust
// Flush any QueuedRemote tombstone for the current view before leaving.
let departing = self.current_view.clone();
if let Some(ts) = self.tombstones.evict(&departing) {
    if let TombstoneKind::QueuedRemote { pending } = ts.kind {
        self.executing_queued_review = true;
        self.process_action(pending);
        self.executing_queued_review = false;
    }
    // Reversible tombstone is silently dropped (action already committed).
}
```

**`IssueAction::UpdateStatus` previous-status race:** The status is read from `tracked_issues` (the local cache) before the PM write. If the PM write succeeds but the cache is stale, the captured `previous_status` may be wrong. This is acceptable — the cache is updated from PM events, and the tombstone window is 60s. In practice the cache and PM state are consistent within a single user session.

**Test-support methods** added to `App` behind `#[cfg(any(test, debug_assertions))]` must not be exposed via the public API surface in release builds. All test-support accessor names end in `_for_test` to make this clear. Use `pub(crate)` visibility minimum; these are for integration tests in `crates/spur-tui/tests/`.

**`executing_queued_review` flag safety:** The flag is set to `true` only synchronously within the `tick` loop and reset to `false` immediately after. Since `App` is single-threaded (TUI event loop is `!Send`), there is no concurrency hazard. The flag approach avoids a second `Action` variant for "queued review ready to dispatch" which would add noise to the action enum.
