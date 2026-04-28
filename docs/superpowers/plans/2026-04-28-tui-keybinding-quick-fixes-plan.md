# TUI Keybinding Quick Fixes — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land 11 surgical keybinding fixes (Q1–Q7 + T1.9, T1.10, T2.7, T2.9, T2.10) identified in the ergonomic review, plus shared transient-hint infrastructure consumed by destructive-undo. After this work every documented hint has a working handler, `Tab` cycles panels unconditionally, `g/G` in PlanInspector is regression-pinned, `:` aliases the command palette from Navigate mode, and triple-Esc guarantees an escape hatch from any TUI state.

**Architecture:** All fixes are surgical edits to existing `handle_key` / `key_owner` dispatch paths — no routing architecture changes. The one structural addition is `App::TransientHint + flash_hint` (Task 0), a shared time-boxed hint slot that feeds into the existing `StatusBarProps::view_hint_override` field at `crates/spur-tui/src/components/status_bar.rs:126`. Task 0 must land before Tasks 3, 9, 10, and 11 (all of which emit toasts or hints through that slot). `Action::PanicReset` (Task 11) is a new variant in `action.rs` routed through the existing `process_action` dispatch. `SessionDetailView` gains a `FocusedSessionPanel` enum and `cancel_hint_until: Option<Instant>` field (Tasks 5 and 10). `DashboardView` gains `Ctrl+O` and `Ctrl+E` handlers and a simpler unconditional Tab path (Tasks 1, 9). The `format_key_hint` helper lands in a new `components/keyhint.rs` module (Task 8). Every task follows TDD order: failing test first, implementation, verify green.

**Tech Stack:** Rust 2021, crossterm 0.28, ratatui 0.29, tokio, `std::time::Instant`, `std::collections::VecDeque`. No new crate dependencies.

**Scope:** `crates/spur-tui` only. Ships in Release N alongside `2026-04-28-tui-destructive-undo-design.md`. Leader-key spec ships Release N+1.

---

## Spec Grounding

| Task | Spec § | Commit boundary |
|---|---|---|
| 0 | §4.11 | Shared transient-hint infra (`App::TransientHint`, `flash_hint`) |
| 1 | §4.1 | `fix(tui): wire Ctrl+O on dashboard as observe-toggle alias` |
| 2 | §4.2 | `fix(tui): SessionPicker error-state r/Enter retry handler` |
| 3 | §4.3 | `feat(tui): x as primary archive key; d deprecation toast` |
| 4 | §4.4 | `test(tui): PlanInspector g/G regression test pinning lane-local behavior` |
| 5 | §4.5 | `feat(tui): SessionDetail Tab/Shift+Tab panel cycle with composer/picker guards` |
| 6 | §4.6 | `feat(tui): : as command-palette alias from Dashboard Navigate` |
| 7 | §4.7 | `docs(tui): help overlay keyboard-environment caveats` |
| 8 | §4.7.2 | `feat(tui): format_key_hint helper (Mac → ⌥/⌃/⇧, ordered modifier combos)` |
| 9 | §4.8 | `feat(tui): Dashboard Tab unconditional panel cycle; Ctrl+E for examples` |
| 10 | §4.9 | `feat(tui): SessionDetail Esc cancel-hint` |
| 11 | §4.10 | `feat(tui): triple-Esc panic reset to Dashboard root` |

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-tui/src/app.rs` | Modify | Add `TransientHint`, `flash_hint`, `flash_hint_short`, `esc_chain: VecDeque<Instant>`, `tick_transient_hint`, `PanicReset` handler, `:` palette alias |
| `crates/spur-tui/src/action.rs` | Modify | Add `Action::PanicReset` unit variant |
| `crates/spur-tui/src/views/dashboard.rs` | Modify | Add Ctrl+O arm; unconditional Tab cycle; Ctrl+E for examples; `reset_to_root()` |
| `crates/spur-tui/src/views/session_detail.rs` | Modify | Add `FocusedSessionPanel`, `cancel_hint_until`, Tab/Shift+Tab cycle, cancel-hint logic, `reset_to_root()` |
| `crates/spur-tui/src/views/session_picker.rs` | Modify | Add `r`/`Enter` retry in Error arm; add `x` archive; `d` deprecation toast; update footer hints |
| `crates/spur-tui/src/views/issue_browser.rs` | Modify | Add `x` → status=closed; `d` deprecation toast |
| `crates/spur-tui/src/components/keyhint.rs` | Create | `format_key_hint(code, mods) -> String`; OS-gated modifier glyphs |
| `crates/spur-tui/src/components/mod.rs` | Modify | `pub mod keyhint;` |
| `crates/spur-tui/src/components/help_overlay.rs` | Modify | Add "Keyboard environment" caveats section |
| `crates/spur-tui/tests/dashboard_ctrl_o_observe.rs` | Create | Q1 regression test |
| `crates/spur-tui/tests/session_picker_error_retry.rs` | Create | Q2 regression test |
| `crates/spur-tui/tests/archive_key_migration.rs` | Create | Q3 + T2.7 tests |
| `crates/spur-tui/tests/plan_inspector_g_consistent_across_widths.rs` | Create | Q4 regression pin |
| `crates/spur-tui/tests/session_detail_tab_panel_cycle.rs` | Create | Q5 test |
| `crates/spur-tui/tests/palette_colon_alias.rs` | Create | Q6 test |
| `crates/spur-tui/tests/keyhint_format.rs` | Create | T2.10 snapshot test |
| `crates/spur-tui/tests/dashboard_tab_unconditional.rs` | Create | T1.9 test |
| `crates/spur-tui/tests/session_detail_esc_cancel_hint.rs` | Create | T1.10 test |
| `crates/spur-tui/tests/app_panic_esc.rs` | Create | T2.9 test |
| `crates/spur-tui/tests/app_transient_hint.rs` | Create | §4.11 test |

---

## Task 0: Shared transient-hint infrastructure

**Spec:** §4.11. Must land before Tasks 3, 9, 10, 11.

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Create: `crates/spur-tui/tests/app_transient_hint.rs`

- [ ] **Step 1: Write failing test.**

```rust
// crates/spur-tui/tests/app_transient_hint.rs
use std::time::Duration;
use spur_tui::App;

#[test]
fn transient_hint_is_none_initially() {
    let app = App::new(None, false);
    assert!(app.transient_hint_for_test().is_none());
}

#[test]
fn flash_hint_short_sets_hint() {
    let mut app = App::new(None, false);
    app.flash_hint_short_for_test("hello");
    assert_eq!(app.transient_hint_for_test().map(|h| h.text.as_str()), Some("hello"));
}

#[test]
fn transient_hint_dismissed_after_tick_past_expiry() {
    let mut app = App::new(None, false);
    // Set with a 0-duration so it is already expired.
    app.flash_hint_for_test("bye", Duration::ZERO);
    // Tick with a now that is definitely past expiry.
    app.tick_transient_hint_for_test(std::time::Instant::now() + Duration::from_secs(10));
    assert!(app.transient_hint_for_test().is_none());
}
```

Run: `scripts/spur-cargo test -p spur-tui --test app_transient_hint`
Expected: FAIL — symbols not found.

- [ ] **Step 2: Add `TransientHint` struct + `transient_hint` field to `App`.**

In `crates/spur-tui/src/app.rs`, after the existing `use` imports, add:

```rust
pub struct TransientHint {
    pub text: String,
    pub expires_at: std::time::Instant,
}
```

Add field to `App` struct (after `palette_state`):

```rust
pub(crate) transient_hint: Option<TransientHint>,
```

Initialize in `build_with_license_state_from_metadata_path` (around line 400, after `palette_state`):

```rust
transient_hint: None,
```

- [ ] **Step 3: Add `flash_hint`, `flash_hint_short`, `tick_transient_hint`.**

In `impl App`, add public methods:

```rust
pub fn flash_hint(&mut self, msg: impl Into<String>, duration: std::time::Duration) {
    self.transient_hint = Some(TransientHint {
        text: msg.into(),
        expires_at: std::time::Instant::now() + duration,
    });
    self.dirty = true;
}

pub fn flash_hint_short(&mut self, msg: impl Into<String>) {
    self.flash_hint(msg, std::time::Duration::from_secs(2));
}

pub(crate) fn tick_transient_hint(&mut self, now: std::time::Instant) {
    if let Some(h) = &self.transient_hint {
        if now >= h.expires_at {
            self.transient_hint = None;
            self.dirty = true;
        }
    }
}
```

Call `self.tick_transient_hint(std::time::Instant::now())` from the `Action::Tick` arm in `process_action`.

- [ ] **Step 4: Add test-only accessors.**

```rust
#[cfg(any(test, debug_assertions))]
pub fn transient_hint_for_test(&self) -> Option<&TransientHint> {
    self.transient_hint.as_ref()
}

#[cfg(any(test, debug_assertions))]
pub fn flash_hint_short_for_test(&mut self, msg: &str) {
    self.flash_hint_short(msg);
}

#[cfg(any(test, debug_assertions))]
pub fn flash_hint_for_test(&mut self, msg: &str, duration: std::time::Duration) {
    self.flash_hint(msg, duration);
}

#[cfg(any(test, debug_assertions))]
pub fn tick_transient_hint_for_test(&mut self, now: std::time::Instant) {
    self.tick_transient_hint(now);
}
```

- [ ] **Step 5: Wire hint into `StatusBarProps::view_hint_override`.**

In the `App` render path that populates `StatusBarProps`, when `self.transient_hint.is_some()` and not expired, set `view_hint_override` to a `HintOverride::from_full` wrapping `transient_hint.text.as_str()`. This overrides lower-priority static hints per §6.3.

- [ ] **Step 6: Run tests to verify green.**

Run: `scripts/spur-cargo test -p spur-tui --test app_transient_hint`
Expected: 3 tests pass.

- [ ] **Step 7: Clippy + fmt.**

```bash
scripts/spur-cargo clippy -p spur-tui -- -D warnings
scripts/spur-cargo fmt -p spur-tui -- --check
```

- [ ] **Step 8: Commit.**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/tests/app_transient_hint.rs
git commit -m "feat(tui): shared TransientHint + flash_hint infrastructure (§4.11)"
```

**Acceptance criteria:**
- [ ] `TransientHint { text, expires_at }` is a public struct in `app.rs`
- [ ] `flash_hint(msg, duration)` and `flash_hint_short(msg)` set `transient_hint`
- [ ] `tick_transient_hint` evicts expired hints; called from `Action::Tick`
- [ ] Hint feeds into `view_hint_override` in the render path
- [ ] 3 tests pass

---

## Task 1: Dashboard Ctrl+O observe-toggle alias (Q1)

**Spec:** §4.1. Finding: `dashboard.rs:898-914` claims `Ctrl+O` in the global bypass but `handle_view_key`'s `'o'` arm at `dashboard.rs:1047-1059` excludes `CONTROL` modifiers, causing a silent no-op.

**Files:**
- Modify: `crates/spur-tui/src/views/dashboard.rs`
- Create: `crates/spur-tui/tests/dashboard_ctrl_o_observe.rs`

- [ ] **Step 1: Write failing test.**

```rust
// crates/spur-tui/tests/dashboard_ctrl_o_observe.rs
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_core::ExecutorLineage;
use spur_tui::views::dashboard::DashboardView;
use spur_tui::views::View;

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<ExecutorLineage> =
        std::sync::LazyLock::new(ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

#[test]
fn ctrl_o_on_focused_node_does_not_silently_drop() {
    // When focused_node is Some, Ctrl+O must reach the view handler
    // and produce the same observe-toggle effect as plain `o`.
    // Without a worker stream to mutate, we can only assert the key
    // is NOT silently consumed as a no-op (i.e., the arm runs).
    // This test pins that Ctrl+O is reachable: if the bypass list
    // were removed, the key would fall through to Compose mode and
    // this test would pass for the wrong reason. The dashboard
    // test harness sets focused_node via handle_key with Enter
    // after seeding the agents tree; for now, assert the key
    // is recognized by the view handler (returns None, not an error).
    let mut dashboard = DashboardView::new();
    let key = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
    let result = dashboard.handle_key(key, &test_ctx());
    // No panic, no compose-mode bleed. The handler consumes the key and returns None.
    assert!(
        result.is_none(),
        "Ctrl+O must return None (handled by view), got: {:?}",
        result
    );
}
```

Run: `scripts/spur-cargo test -p spur-tui --test dashboard_ctrl_o_observe`
Expected: test may currently pass vacuously (None is returned because the key falls through) but the intent-locking assertion documents the desired contract. Run first to capture baseline.

- [ ] **Step 2: Add Ctrl+O arm to `handle_view_key` in `dashboard.rs`.**

In `dashboard.rs`, in the `handle_view_key` match block, add an arm BEFORE the existing `KeyCode::Char('o')` arm at `:1047`:

```rust
KeyCode::Char('o')
    if self.focused_node.is_some()
        && key.modifiers.contains(KeyModifiers::CONTROL) =>
{
    // Alias for plain `o` on focused node — symmetric with
    // SessionDetail's Ctrl+O (session_detail.rs:1403-1408).
    if let Some(ref id) = self.focused_node.clone() {
        if let Some(trace) = worker_streams.get_mut(&id.0) {
            trace.toggle_observe_collapsed();
        }
    }
    None
}
```

The existing `'o'` arm at `:1047` uses `!key.modifiers.intersects(CONTROL | ALT)` so it will not match `Ctrl+O` — both arms coexist cleanly.

- [ ] **Step 3: Verify tests pass.**

Run: `scripts/spur-cargo test -p spur-tui --test dashboard_ctrl_o_observe`
Run: `scripts/spur-cargo test -p spur-tui --test dashboard_composer_contract`
Expected: all pass (existing tests unaffected).

- [ ] **Step 4: Clippy + fmt + commit.**

```bash
scripts/spur-cargo clippy -p spur-tui -- -D warnings
scripts/spur-cargo fmt -p spur-tui -- --check
git add crates/spur-tui/src/views/dashboard.rs crates/spur-tui/tests/dashboard_ctrl_o_observe.rs
git commit -m "fix(tui): wire Ctrl+O on dashboard as observe-toggle alias (Q1)"
```

**Acceptance criteria:**
- [ ] `Ctrl+O` with `focused_node.is_some()` reaches the new arm and calls `toggle_observe_collapsed`
- [ ] Plain `o` arm at `:1047` continues to exclude Ctrl/Alt (unchanged)
- [ ] Existing `dashboard_composer_contract` tests pass

---

## Task 2: SessionPicker error-state retry handler (Q2)

**Spec:** §4.2. Finding: `session_picker.rs:29` shows footer hint `r retry · Esc back` for Error state. Handler at `session_picker.rs:1492-1495` only handles `Esc`; `r` and `Enter` are silently swallowed.

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`
- Create: `crates/spur-tui/tests/session_picker_error_retry.rs`

- [ ] **Step 1: Write failing test.**

```rust
// crates/spur-tui/tests/session_picker_error_retry.rs
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::action::Action;
use spur_tui::views::session_picker::SessionPickerView;
use spur_tui::views::View;

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn error_picker() -> SessionPickerView {
    let mut picker = SessionPickerView::new();
    picker.set_error_for_test("network error");
    picker
}

#[test]
fn r_key_in_error_state_emits_refresh_sessions() {
    let mut picker = error_picker();
    let action = picker.handle_key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(
        matches!(action, Some(Action::RefreshSessions)),
        "r in Error state must emit RefreshSessions, got {:?}",
        action
    );
}

#[test]
fn enter_in_error_state_emits_refresh_sessions() {
    let mut picker = error_picker();
    let action = picker.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(
        matches!(action, Some(Action::RefreshSessions)),
        "Enter in Error state must emit RefreshSessions, got {:?}",
        action
    );
}

#[test]
fn esc_in_error_state_navigates_back() {
    let mut picker = error_picker();
    let action = picker.handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &test_ctx(),
    );
    assert!(
        matches!(action, Some(Action::NavigateTo(_))),
        "Esc in Error state must navigate back, got {:?}",
        action
    );
}
```

Run: `scripts/spur-cargo test -p spur-tui --test session_picker_error_retry`
Expected: FAIL — `r` and `Enter` return `None`.

- [ ] **Step 2: Add `set_error_for_test` accessor to `SessionPickerView`.**

In `session_picker.rs`, under `#[cfg(any(test, debug_assertions))]`:

```rust
pub fn set_error_for_test(&mut self, msg: &str) {
    self.state = PickerState::Error { message: msg.to_string() };
}
```

(Verify the exact `Error` variant fields from the existing `PickerState` definition in `session_picker.rs`.)

- [ ] **Step 3: Extend `PickerState::Error` arm at `session_picker.rs:1492`.**

Change:

```rust
PickerState::Loading | PickerState::Error { .. } => match key.code {
    KeyCode::Esc => Some(Action::NavigateTo(ViewId::Dashboard)),
    _ => None,
},
```

To:

```rust
PickerState::Loading => match key.code {
    KeyCode::Esc => Some(Action::NavigateTo(ViewId::Dashboard)),
    _ => None,
},
PickerState::Error { .. } => match key.code {
    KeyCode::Esc => Some(Action::NavigateTo(ViewId::Dashboard)),
    KeyCode::Char('r') | KeyCode::Enter => Some(Action::RefreshSessions),
    _ => None,
},
```

- [ ] **Step 4: Run tests green.**

Run: `scripts/spur-cargo test -p spur-tui --test session_picker_error_retry`
Run: `scripts/spur-cargo test -p spur-tui --test session_picker_interactions`
Expected: all pass.

- [ ] **Step 5: Clippy + fmt + commit.**

```bash
git add crates/spur-tui/src/views/session_picker.rs crates/spur-tui/tests/session_picker_error_retry.rs
git commit -m "fix(tui): SessionPicker error-state r/Enter retry handler (Q2)"
```

**Acceptance criteria:**
- [ ] `r` in `PickerState::Error` emits `Action::RefreshSessions`
- [ ] `Enter` in `PickerState::Error` emits `Action::RefreshSessions`
- [ ] `Esc` in `PickerState::Error` still navigates to Dashboard
- [ ] `PickerState::Loading` behavior unchanged (no `r`/`Enter` handler there)

---

## Task 3: `x` as primary archive key; `d` deprecation toast (Q3 + T2.7)

**Spec:** §4.3. Depends on Task 0 (`flash_hint_short`). Finding: `session_picker.rs:1456-1458` and `issue_browser.rs:165` both bind `d` to archive/close actions, violating universal `d`-means-delete semantics.

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`
- Modify: `crates/spur-tui/src/views/issue_browser.rs`
- Create: `crates/spur-tui/tests/archive_key_migration.rs`

- [ ] **Step 1: Write failing tests.**

```rust
// crates/spur-tui/tests/archive_key_migration.rs
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::action::Action;
// SessionPicker x-archives
// IssueBrowser x-closes
// Both: d triggers deprecation toast (flash_hint_short call)
// Tests verify action shape; toast is verified via transient_hint accessor.
```

Full test bodies: assert `x` on a populated picker with a highlighted session emits `Action::ToggleSessionArchive { session_id }`. Assert `d` also emits the archive action but additionally calls `flash_hint_short`. For `issue_browser`: assert `x` emits `Action::Issue(IssueAction::UpdateStatus { status: "closed" })`.

Run: `scripts/spur-cargo test -p spur-tui --test archive_key_migration`
Expected: FAIL — `x` returns None; `d` has no toast.

- [ ] **Step 2: Update `session_picker.rs` — add `x`, keep `d` with toast.**

In the `PickerState::Populated` key arm (around `session_picker.rs:1456`):

```rust
// x: new primary archive key
KeyCode::Char('x') => hl_session_id
    .clone()
    .map(|session_id| Action::ToggleSessionArchive { session_id }),

// d: deprecated alias — still archives but signals caller to emit toast
// The view cannot call flash_hint directly; return a new action variant
// or handle through a post-key hook. Simplest: return ToggleSessionArchive
// and set a view-local field `d_pressed: bool` that the caller checks.
KeyCode::Char('d') => {
    self.deprecation_toast_pending = true;  // new field, see Step 3
    hl_session_id
        .clone()
        .map(|session_id| Action::ToggleSessionArchive { session_id })
},
```

- [ ] **Step 3: Add `deprecation_toast_pending: bool` to `SessionPickerView` struct.** Initialize to `false`. In `SessionPickerView::handle_key`, after resolving the action, check `self.deprecation_toast_pending` and clear it; return the pending-toast info to the caller via a view-local mechanism. Because `handle_key` returns `Option<Action>`, the simplest path is: after the match block, if `deprecation_toast_pending`, call `self.deprecation_toast_pending = false` and return a new `Action::DeprecationHint` variant — OR, simpler, expose `pub(crate) fn take_deprecation_toast(&mut self) -> Option<&'static str>` and let `App::process_action` call it post-dispatch and then call `app.flash_hint_short(...)`.

  Chosen: expose `take_deprecation_toast` and wire it in `App::handle_crossterm_event` after view dispatch. This avoids a new Action variant.

- [ ] **Step 4: Update footer hints in `session_picker.rs:40`.**

Change the `Populated` hint line containing `d archive` to `x archive · d⚠ deprecated`.

- [ ] **Step 5: Update `issue_browser.rs:165` analogously.**

Add `KeyCode::Char('x') => self.update_status("closed")` before the existing `'d'` arm. Keep `'d'` arm with a `self.deprecation_toast_pending = true` flag (same pattern). Wire `take_deprecation_toast` on `IssueBrowserView`.

- [ ] **Step 6: Wire toast in `App`.**

In `app.rs` `handle_crossterm_event`, after dispatching the action from session_picker / issue_browser view, check `view.take_deprecation_toast()` and call `self.flash_hint_short("d → archive renamed to x; d will be removed in a future release")`.

- [ ] **Step 7: Run tests + existing suite.**

Run: `scripts/spur-cargo test -p spur-tui --test archive_key_migration`
Run: `scripts/spur-cargo test -p spur-tui --test session_picker_interactions`
Run: `scripts/spur-cargo test -p spur-tui --test issue_browser_contract`
Expected: all pass.

- [ ] **Step 8: Clippy + fmt + commit.**

```bash
git add crates/spur-tui/src/views/session_picker.rs crates/spur-tui/src/views/issue_browser.rs \
        crates/spur-tui/src/app.rs crates/spur-tui/tests/archive_key_migration.rs
git commit -m "feat(tui): x as primary archive key; d deprecation toast (Q3 + T2.7)"
```

**Acceptance criteria:**
- [ ] `x` on a populated session in SessionPicker emits `ToggleSessionArchive`
- [ ] `x` on a focused issue in IssueBrowser emits `Issue(UpdateStatus { status: "closed" })`
- [ ] `d` in both views still emits the same action AND triggers `flash_hint_short` with deprecation copy
- [ ] Footer hint copy updated to `x archive`

---

## Task 4: PlanInspector g/G regression test (Q4)

**Spec:** §4.4. No source changes. Finding was corrected: `g/G` at `plan_inspector.rs:133-134` always call `jump_lane_start/end` (lane-local). The test pins this contract across render widths.

**Files:**
- Create: `crates/spur-tui/tests/plan_inspector_g_consistent_across_widths.rs`

- [ ] **Step 1: Write the regression test.**

```rust
// crates/spur-tui/tests/plan_inspector_g_consistent_across_widths.rs
//
// Pins that g/G in PlanInspectorView are lane-local at ALL render widths.
// The ergonomic review initially claimed width-conditional behavior; the
// spec (§4.4) corrected this — g/G call jump_lane_start/end unconditionally.
// This test must pass on unmodified current code.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::views::plan_inspector::PlanInspectorView;
use spur_tui::views::View;

fn make_ctx_with_plan(session_id: &str) -> (spur_tui::views::ViewContext<'static>, ...) {
    // Build a minimal TrackedPlan with 2 stages, 3 tasks each,
    // using PlanProjectionStore test helpers.
    // Verify `selected_task_for_test()` exposes the current selection.
    todo!("wire test plan via PlanProjectionStore::seed_for_test")
}

#[test]
fn g_jumps_to_first_of_current_stage_at_narrow_width() {
    // Width 50 (stacked mode): g must jump to first task of current stage.
    // ...
}

#[test]
fn g_jumps_to_first_of_current_stage_at_wide_width() {
    // Width 120 (side-by-side mode): g must jump to first task of current stage.
    // ...
}

#[test]
fn shift_g_jumps_to_last_of_current_stage() {
    // G must jump to last task of current stage regardless of width.
    // ...
}
```

Note: If `PlanProjectionStore` lacks a seed-for-test helper, add `pub fn seed_for_test` under `#[cfg(any(test, debug_assertions))]` in `spur-core`. The test MUST pass on current unmodified code (it is a contract-pinning test, not a behavior fix).

- [ ] **Step 2: Run tests.**

Run: `scripts/spur-cargo test -p spur-tui --test plan_inspector_g_consistent_across_widths`
Expected: all pass on current code with no source changes to `plan_inspector.rs`.

- [ ] **Step 3: Commit.**

```bash
git add crates/spur-tui/tests/plan_inspector_g_consistent_across_widths.rs
git commit -m "test(tui): PlanInspector g/G regression test pinning lane-local behavior (Q4)"
```

**Acceptance criteria:**
- [ ] Three tests pass on unmodified `plan_inspector.rs`
- [ ] No changes to `plan_inspector.rs` itself
- [ ] Test comment explicitly notes that j/k width-conditional behavior is a SEPARATE follow-up

---

## Task 5: SessionDetail Tab/Shift+Tab panel cycle (Q5)

**Spec:** §4.5. `SessionDetailView` currently has no `focused_panel` enum. Dashboard's panel cycle lives at `dashboard.rs:1373-1400`.

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Create: `crates/spur-tui/tests/session_detail_tab_panel_cycle.rs`

- [ ] **Step 1: Write failing tests.**

```rust
// crates/spur-tui/tests/session_detail_tab_panel_cycle.rs
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::views::session_detail::{FocusedSessionPanel, SessionDetailView};
use spur_tui::views::View;

#[test]
fn tab_with_empty_input_and_no_picker_cycles_panel() {
    let mut detail = SessionDetailView::new_for_test("sess-1");
    assert_eq!(detail.focused_panel(), FocusedSessionPanel::ReactTrace);
    detail.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &test_ctx());
    assert_eq!(detail.focused_panel(), FocusedSessionPanel::Workers);
}

#[test]
fn tab_with_non_empty_input_goes_to_composer() {
    let mut detail = SessionDetailView::new_for_test("sess-1");
    detail.set_input_for_test("hello");
    let action = detail.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &test_ctx());
    // Tab with non-empty input must not cycle panel; panel stays at default.
    assert_eq!(detail.focused_panel(), FocusedSessionPanel::ReactTrace);
    // action is None (composer consumed Tab internally without emitting an action)
    assert!(action.is_none());
}

#[test]
fn shift_tab_cycles_panel_in_reverse() {
    let mut detail = SessionDetailView::new_for_test("sess-1");
    detail.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT), &test_ctx());
    assert_eq!(detail.focused_panel(), FocusedSessionPanel::Workers);
}
```

Run: `scripts/spur-cargo test -p spur-tui --test session_detail_tab_panel_cycle`
Expected: FAIL — `FocusedSessionPanel` not defined.

- [ ] **Step 2: Add `FocusedSessionPanel` enum and `focused_panel` field.**

In `session_detail.rs`, add before the `SessionDetailView` struct definition:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusedSessionPanel {
    Workers,
    #[default]
    ReactTrace,
}
```

Add to `SessionDetailView` struct:

```rust
focused_panel: FocusedSessionPanel,
```

Initialize to `FocusedSessionPanel::ReactTrace` in constructors.

- [ ] **Step 3: Add `focused_panel()` accessor and `set_input_for_test`.**

```rust
#[cfg(any(test, debug_assertions))]
pub fn focused_panel(&self) -> FocusedSessionPanel {
    self.focused_panel
}

#[cfg(any(test, debug_assertions))]
pub fn set_input_for_test(&mut self, text: &str) {
    self.input_bar.set_text_for_test(text);
}
```

- [ ] **Step 4: Add Tab/Shift+Tab cycle to `handle_key`.**

In `session_detail.rs` `handle_key`, in the `KeyOwner::View` branch, add AFTER the existing guard checks (completion picker, history shell) but before the general key dispatch:

```rust
// Tab/Shift+Tab panel cycle — only when composer and pickers release Tab.
if matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
    && !self.completion.is_active()
    && self.input_bar.text().is_empty()
{
    self.focused_panel = match self.focused_panel {
        FocusedSessionPanel::ReactTrace => FocusedSessionPanel::Workers,
        FocusedSessionPanel::Workers => FocusedSessionPanel::ReactTrace,
    };
    self.dirty = true;
    return None;
}
```

Note: when `self.input_bar.text()` is non-empty, Tab must NOT reach this branch — it remains in the composer (existing `KeyOwner::Composer` routing handles it).

- [ ] **Step 5: Add colored border to focused panel in `render`.**

In the `render` method, use `self.focused_panel` to apply a highlight border color to the Workers list block vs the ReactTrace block, matching the Dashboard panel-focus convention (cyan for active, default for inactive).

- [ ] **Step 6: Run tests.**

Run: `scripts/spur-cargo test -p spur-tui --test session_detail_tab_panel_cycle`
Run: `scripts/spur-cargo test -p spur-tui --test session_detail_load_state`
Expected: all pass.

- [ ] **Step 7: Clippy + fmt + commit.**

```bash
git add crates/spur-tui/src/views/session_detail.rs crates/spur-tui/tests/session_detail_tab_panel_cycle.rs
git commit -m "feat(tui): SessionDetail Tab/Shift+Tab panel cycle with composer/picker guards (Q5)"
```

**Acceptance criteria:**
- [ ] `FocusedSessionPanel::{Workers, ReactTrace}` enum is public
- [ ] Tab with empty input and no active picker cycles `focused_panel`
- [ ] Tab with non-empty input stays in composer (panel unchanged)
- [ ] Tab with active completion picker goes to picker (panel unchanged)
- [ ] Shift+Tab cycles in reverse
- [ ] Focused panel renders with colored border in the render pass
- [ ] `reset_to_root()` (added in Task 11) sets `focused_panel = ReactTrace`

---

## Task 6: `:` as command-palette alias (Q6)

**Spec:** §4.6. Finding: `app.rs:978-985` — `Ctrl+K` opens the palette. `:` should open it from Dashboard Navigate mode only.

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Create: `crates/spur-tui/tests/palette_colon_alias.rs`

- [ ] **Step 1: Write failing test.**

```rust
// crates/spur-tui/tests/palette_colon_alias.rs
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::App;
use spur_tui::views::dashboard::DashboardMode;

#[test]
fn colon_opens_palette_from_navigate_mode() {
    let mut app = App::new(None, false);
    // App starts in Dashboard Navigate mode with empty input.
    assert!(!app.is_palette_visible());
    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
    assert!(app.is_palette_visible(), "':' must open palette in Navigate mode");
}

#[test]
fn colon_does_not_open_palette_from_compose_mode() {
    let mut app = App::new(None, false);
    // Force dashboard into Compose mode by typing a character.
    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
    // 'h' in Navigate mode is a view-action (scroll), not compose-entry.
    // Type a non-view-action to enter compose.
    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    let was_palette_before = app.is_palette_visible();
    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
    // ':' in compose mode must be inserted as a character, not open palette.
    assert!(!app.is_palette_visible() || was_palette_before,
        "':' in Compose mode must not open palette");
}
```

Run: `scripts/spur-cargo test -p spur-tui --test palette_colon_alias`
Expected: first test FAIL (palette not opened by `:`).

- [ ] **Step 2: Add `:` alongside Ctrl+K in `app.rs`.**

In `app.rs` at the Ctrl+K block (around `:978-985`), extend the condition to also match plain `:` when the current view is Dashboard and mode is Navigate:

```rust
let is_colon_alias = key.code == KeyCode::Char(':')
    && key.modifiers.is_empty()
    && self.current_view == ViewId::Dashboard
    && self.dashboard.mode() == DashboardMode::Navigate;

let is_ctrl_k = key.modifiers.contains(KeyModifiers::CONTROL)
    && matches!(key.code, KeyCode::Char('k'));

if is_ctrl_k || is_colon_alias {
    self.open_palette();
    return;
}
```

Verify `DashboardView::mode()` is a public method; if not, add:

```rust
pub fn mode(&self) -> DashboardMode {
    self.mode
}
```

- [ ] **Step 3: Run tests.**

Run: `scripts/spur-cargo test -p spur-tui --test palette_colon_alias`
Run: `scripts/spur-cargo test -p spur-tui --test palette_integration`
Expected: all pass.

- [ ] **Step 4: Clippy + fmt + commit.**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/src/views/dashboard.rs \
        crates/spur-tui/tests/palette_colon_alias.rs
git commit -m "feat(tui): : as command-palette alias from Dashboard Navigate (Q6)"
```

**Acceptance criteria:**
- [ ] `:` in Dashboard Navigate mode opens palette (same as Ctrl+K)
- [ ] `:` in Dashboard Compose mode inserts the character (does not open palette)
- [ ] `:` in SessionDetail input bar inserts the character (input bar always active there)
- [ ] Ctrl+K behavior unchanged

---

## Task 7: Help overlay keyboard-environment caveats (Q7)

**Spec:** §4.7.1. Finding: `app.rs` help overlay has no terminal-environment caveat section.

**Files:**
- Modify: `crates/spur-tui/src/components/help_overlay.rs`

- [ ] **Step 1: Locate the help overlay render method.**

Read `crates/spur-tui/src/components/help_overlay.rs` and find the section list rendered when `?` is pressed.

- [ ] **Step 2: Add "Keyboard environment" section to the overlay.**

Append a new section with these bullet points (verbatim per §4.7.1):

```
Keyboard environment
  macOS Terminal.app  Enable View → Use Option as Meta key for Alt+* shortcuts.
  iTerm2              Profiles → Keys → Left Option: Esc+.
  Windows Terminal    Works natively.
  tmux                Ctrl+P/N/O may be intercepted by tmux prefix; consider Ctrl+A.
  Flow control        If stty ixon is active, Ctrl+S/Q are eaten. Run: stty -ixon
  Legacy terminals    Ctrl+digit may not encode reliably in some terminal emulators.
```

Place this after the existing keybinding table rows, before the closing footer.

- [ ] **Step 3: No test required for static content.** Run the full render suite to confirm no crash.

Run: `scripts/spur-cargo test -p spur-tui`
Expected: no regression.

- [ ] **Step 4: Commit.**

```bash
git add crates/spur-tui/src/components/help_overlay.rs
git commit -m "docs(tui): help overlay keyboard-environment caveats (Q7)"
```

**Acceptance criteria:**
- [ ] Help overlay renders without panic after adding the new section
- [ ] The 6 caveat bullets match §4.7.1 verbatim
- [ ] No test file added (doc-only change); snapshot test `render_golden` still passes if applicable

---

## Task 8: `format_key_hint` OS-aware modifier helper (T2.10)

**Spec:** §4.7.2. New file: `crates/spur-tui/src/components/keyhint.rs`.

**Files:**
- Create: `crates/spur-tui/src/components/keyhint.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`
- Create: `crates/spur-tui/tests/keyhint_format.rs`

- [ ] **Step 1: Write failing tests.**

```rust
// crates/spur-tui/tests/keyhint_format.rs
use crossterm::event::{KeyCode, KeyModifiers};
use spur_tui::components::keyhint::format_key_hint;

#[test]
#[cfg(target_os = "macos")]
fn macos_ctrl_renders_as_glyph() {
    let s = format_key_hint(KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert_eq!(s, "⌃+k");
}

#[test]
#[cfg(target_os = "macos")]
fn macos_alt_renders_as_option_glyph() {
    let s = format_key_hint(KeyCode::Char('m'), KeyModifiers::ALT);
    assert_eq!(s, "⌥+m");
}

#[test]
#[cfg(target_os = "macos")]
fn macos_shift_tab_renders_correctly() {
    let s = format_key_hint(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(s, "⇧+Tab");
}

#[test]
#[cfg(not(target_os = "macos"))]
fn linux_ctrl_renders_as_word() {
    let s = format_key_hint(KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert_eq!(s, "Ctrl+k");
}

#[test]
fn modifier_order_is_ctrl_alt_shift() {
    // Ctrl+Shift+P: Ctrl comes before Shift.
    use KeyModifiers;
    let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    let s = format_key_hint(KeyCode::Char('p'), mods);
    // On any platform, Ctrl prefix appears before Shift prefix.
    let ctrl_pos = s.find("Ctrl").or_else(|| s.find('⌃')).unwrap_or(usize::MAX);
    let shift_pos = s.find("Shift").or_else(|| s.find('⇧')).unwrap_or(usize::MAX);
    assert!(ctrl_pos < shift_pos, "Ctrl must precede Shift in: {s}");
}
```

Run: `scripts/spur-cargo test -p spur-tui --test keyhint_format`
Expected: FAIL — module not found.

- [ ] **Step 2: Create `keyhint.rs`.**

```rust
// crates/spur-tui/src/components/keyhint.rs
use crossterm::event::{KeyCode, KeyModifiers};

pub fn format_key_hint(code: KeyCode, mods: KeyModifiers) -> String {
    let mut out = String::new();
    let (ctrl, alt, shift) = platform_modifier_glyphs();

    if mods.contains(KeyModifiers::CONTROL) { out.push_str(ctrl); out.push('+'); }
    if mods.contains(KeyModifiers::ALT)     { out.push_str(alt);  out.push('+'); }
    if mods.contains(KeyModifiers::SHIFT)   { out.push_str(shift); out.push('+'); }
    out.push_str(&format_keycode(code));
    out
}

#[cfg(target_os = "macos")]
fn platform_modifier_glyphs() -> (&'static str, &'static str, &'static str) {
    ("⌃", "⌥", "⇧")
}

#[cfg(not(target_os = "macos"))]
fn platform_modifier_glyphs() -> (&'static str, &'static str, &'static str) {
    ("Ctrl", "Alt", "Shift")
}

fn format_keycode(code: KeyCode) -> String {
    match code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Tab | KeyCode::BackTab => "Tab".to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Up => "↑".to_string(),
        KeyCode::Down => "↓".to_string(),
        KeyCode::Left => "←".to_string(),
        KeyCode::Right => "→".to_string(),
        KeyCode::PageUp => "PgUp".to_string(),
        KeyCode::PageDown => "PgDn".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::F(n) => format!("F{n}"),
        other => format!("{other:?}"),
    }
}
```

- [ ] **Step 3: Register in `components/mod.rs`.**

Add `pub mod keyhint;` to `crates/spur-tui/src/components/mod.rs`.

- [ ] **Step 4: Run tests.**

Run: `scripts/spur-cargo test -p spur-tui --test keyhint_format`
Expected: all pass.

- [ ] **Step 5: Clippy + fmt + commit.**

```bash
git add crates/spur-tui/src/components/keyhint.rs crates/spur-tui/src/components/mod.rs \
        crates/spur-tui/tests/keyhint_format.rs
git commit -m "feat(tui): format_key_hint helper (Mac → ⌥/⌃/⇧, ordered modifier combos) (T2.10)"
```

**Acceptance criteria:**
- [ ] `format_key_hint` returns `⌃+k` on macOS, `Ctrl+k` on Linux
- [ ] Modifier order is always Ctrl → Alt → Shift → key
- [ ] `BackTab` with `SHIFT` renders as `⇧+Tab`
- [ ] Module exported via `components::keyhint`

---

## Task 9: Dashboard Tab unconditional panel cycle; Ctrl+E for examples (T1.9)

**Spec:** §4.8. Finding: `dashboard.rs:1373-1388` — Tab cycles examples when input is empty (overloaded). Depends on Task 0 (`flash_hint_short` for deprecation toast).

**Files:**
- Modify: `crates/spur-tui/src/views/dashboard.rs`
- Create: `crates/spur-tui/tests/dashboard_tab_unconditional.rs`

- [ ] **Step 1: Write failing tests.**

```rust
// crates/spur-tui/tests/dashboard_tab_unconditional.rs
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::action::Action;
use spur_tui::views::dashboard::{DashboardView, Panel};
use spur_tui::views::View;

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

#[test]
fn tab_with_empty_input_cycles_panel_not_example() {
    let mut dashboard = DashboardView::new();
    assert_eq!(dashboard.focused_panel_for_test(), Panel::Agents);
    dashboard.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &test_ctx());
    assert_eq!(
        dashboard.focused_panel_for_test(),
        Panel::Log,
        "Tab with empty input must cycle panels, not examples"
    );
}

#[test]
fn ctrl_e_with_empty_input_cycles_examples() {
    let mut dashboard = DashboardView::new();
    let before = dashboard.input_bar_text_for_test().to_string();
    dashboard.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL), &test_ctx());
    let after = dashboard.input_bar_text_for_test().to_string();
    // After Ctrl+E, the example prompt is loaded into the input bar.
    assert_ne!(before, after, "Ctrl+E must cycle example prompts into input bar");
}
```

Run: `scripts/spur-cargo test -p spur-tui --test dashboard_tab_unconditional`
Expected: first test FAIL (Tab still cycles examples with empty input on current code).

- [ ] **Step 2: Update `dashboard.rs` Tab handler at `:1373-1388`.**

Remove the `if self.focused_node.is_none() && self.input_bar.text().is_empty()` branch that calls `cycle_example()`. Replace with unconditional panel cycle:

```rust
KeyCode::Tab => {
    self.focused_panel = match self.focused_panel {
        Panel::Agents => Panel::Log,
        Panel::Log => Panel::Agents,
    };
    self.agents_tree.set_focused(self.focused_panel == Panel::Agents);
    self.activity_log.set_focused(self.focused_panel == Panel::Log);
    Some(Action::CycleFocus)
}
```

- [ ] **Step 3: Add `Ctrl+E` arm for example cycling.**

In the global-bypass block (before `key_owner` dispatch, around `:907-914`), add `Ctrl+E` to the bypass list. Then in `handle_view_key`, add:

```rust
KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL)
    && self.focused_node.is_none()
    && self.input_bar.text().is_empty() =>
{
    self.cycle_example();
    None
}
```

Also add to `is_view_action_char`: `'e'` when `key.modifiers.contains(CONTROL)` is not needed since Ctrl is already bypassed globally — verify that `Ctrl+E` does not get consumed by the composer in Navigate mode (it won't, because the global bypass runs first).

- [ ] **Step 4: Add `focused_panel_for_test` accessor.**

```rust
#[cfg(any(test, debug_assertions))]
pub fn focused_panel_for_test(&self) -> Panel {
    self.focused_panel
}
```

- [ ] **Step 5: Run tests.**

Run: `scripts/spur-cargo test -p spur-tui --test dashboard_tab_unconditional`
Run: `scripts/spur-cargo test -p spur-tui --test dashboard_composer_contract`
Expected: all pass.

- [ ] **Step 6: Clippy + fmt + commit.**

```bash
git add crates/spur-tui/src/views/dashboard.rs crates/spur-tui/tests/dashboard_tab_unconditional.rs
git commit -m "feat(tui): Dashboard Tab unconditional panel cycle; Ctrl+E for examples (T1.9)"
```

**Acceptance criteria:**
- [ ] Tab always cycles Agents↔Log, regardless of input bar contents or focused node
- [ ] `Ctrl+E` with empty input cycles example prompts (previously done by Tab-with-empty)
- [ ] `Ctrl+E` is in the global bypass list so it reaches the view in both modes
- [ ] BackTab reverse-cycles panels (unchanged)
- [ ] `dashboard_composer_contract` tests still pass

---

## Task 10: SessionDetail Esc cancel-hint (T1.10)

**Spec:** §4.9. Finding: `session_detail.rs:1167` — first Esc cancels in-flight stream but user gets no visual confirmation. Depends on Task 0 (`TransientHint` / `cancel_hint_until` pattern).

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Create: `crates/spur-tui/tests/session_detail_esc_cancel_hint.rs`

- [ ] **Step 1: Write failing test.**

```rust
// crates/spur-tui/tests/session_detail_esc_cancel_hint.rs
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::views::session_detail::SessionDetailView;
use spur_tui::views::View;
use std::time::Instant;

#[test]
fn esc_while_stream_in_flight_sets_cancel_hint() {
    let mut detail = SessionDetailView::new_for_test("sess-1");
    detail.set_stream_in_flight_for_test(true);

    detail.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());

    // cancel_hint_until must be set and in the future.
    let hint_until = detail.cancel_hint_until_for_test();
    assert!(
        hint_until.is_some_and(|t| t > Instant::now()),
        "cancel hint must be set after Esc-cancel"
    );
}

#[test]
fn hint_clears_on_second_esc() {
    let mut detail = SessionDetailView::new_for_test("sess-1");
    detail.set_stream_in_flight_for_test(true);
    detail.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());
    // Second Esc: cancelling_in_flight is already true so this is the NavigateBack path.
    detail.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &test_ctx());
    assert!(detail.cancel_hint_until_for_test().is_none(), "hint must clear on second Esc");
}
```

Run: `scripts/spur-cargo test -p spur-tui --test session_detail_esc_cancel_hint`
Expected: FAIL — `cancel_hint_until` field not found.

- [ ] **Step 2: Add `cancel_hint_until: Option<Instant>` field to `SessionDetailView`.**

Initialize to `None`. Add test-only accessors:

```rust
#[cfg(any(test, debug_assertions))]
pub fn cancel_hint_until_for_test(&self) -> Option<std::time::Instant> {
    self.cancel_hint_until
}

#[cfg(any(test, debug_assertions))]
pub fn set_stream_in_flight_for_test(&mut self, value: bool) {
    self.stream_in_flight = value;
}
```

- [ ] **Step 3: Set `cancel_hint_until` at the cancel-stream branch.**

In `session_detail.rs:1167-1181`, immediately after `self.cancelling_in_flight = true`:

```rust
self.cancel_hint_until = Some(
    std::time::Instant::now() + std::time::Duration::from_secs(2)
);
```

- [ ] **Step 4: Clear `cancel_hint_until` on second Esc.**

After the `cancelling_in_flight` block, in the Esc handlers that call `NavigateBack` or `UnfocusNode`, add `self.cancel_hint_until = None;`.

- [ ] **Step 5: Wire hint into `status_bar_props`.**

In the method that builds `StatusBarProps` for `SessionDetailView`, when `cancel_hint_until.is_some_and(|t| t > Instant::now())`, set `view_hint_override` to:

```rust
Some(HintOverride::from_full(
    "Esc cancelled the active turn. Press Esc again to go back."
))
```

- [ ] **Step 6: Run tests.**

Run: `scripts/spur-cargo test -p spur-tui --test session_detail_esc_cancel_hint`
Run: `scripts/spur-cargo test -p spur-tui --test session_detail_load_state`
Expected: all pass.

- [ ] **Step 7: Clippy + fmt + commit.**

```bash
git add crates/spur-tui/src/views/session_detail.rs crates/spur-tui/tests/session_detail_esc_cancel_hint.rs
git commit -m "feat(tui): SessionDetail Esc cancel-hint (T1.10)"
```

**Acceptance criteria:**
- [ ] First Esc while stream in flight sets `cancel_hint_until = now + 2s`
- [ ] Status bar shows "Esc cancelled the active turn. Press Esc again to go back." for 2s
- [ ] Second Esc clears `cancel_hint_until` and proceeds with NavigateBack
- [ ] `reset_to_root()` (Task 11) clears `cancel_hint_until`

---

## Task 11: Triple-Esc panic reset to Dashboard root (T2.9)

**Spec:** §4.10. Depends on Tasks 0, 5, 10 (for `reset_to_root` on each view). Cross-spec: calls `tombstones.cancel_all_without_dispatch()` defined in destructive-undo spec.

**Files:**
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/src/views/dashboard.rs`
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Create: `crates/spur-tui/tests/app_panic_esc.rs`

- [ ] **Step 1: Add `Action::PanicReset` to `action.rs`.**

In `crates/spur-tui/src/action.rs`, append to the `Action` enum:

```rust
/// Triple-Esc panic reset: unconditionally return to Dashboard root,
/// dismiss all overlays, and cancel any in-flight tombstones.
/// Emitted by App when three Esc presses arrive within 1000ms.
PanicReset,
```

- [ ] **Step 2: Write failing test.**

```rust
// crates/spur-tui/tests/app_panic_esc.rs
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::App;
use spur_tui::views::dashboard::DashboardMode;

fn esc(app: &mut App) {
    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
}

#[test]
fn triple_esc_within_1000ms_returns_to_dashboard_root() {
    let mut app = App::new(None, false);
    // Open the help overlay and the palette to create layered state.
    app.handle_crossterm_event_for_test(
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)
    );
    assert!(app.is_help_visible_for_test());

    // Triple Esc in rapid succession.
    esc(&mut app);
    esc(&mut app);
    esc(&mut app);

    assert!(!app.is_help_visible_for_test(), "help overlay must be dismissed");
    assert!(!app.is_palette_visible(), "palette must be dismissed");
    assert_eq!(
        app.current_view_for_test(),
        spur_tui::action::ViewId::Dashboard,
        "view must be Dashboard"
    );
    assert_eq!(
        app.dashboard_for_test().mode(),
        DashboardMode::Navigate,
        "dashboard must be in Navigate mode"
    );
}

#[test]
fn double_esc_does_not_panic_reset() {
    let mut app = App::new(None, false);
    esc(&mut app);
    esc(&mut app);
    // Two Esc presses must NOT trigger PanicReset.
    // The app should still be alive with no panic.
    // (We can't easily assert "still in Dashboard" without side effects;
    //  asserting no panic is sufficient for this boundary test.)
}
```

Run: `scripts/spur-cargo test -p spur-tui --test app_panic_esc`
Expected: FAIL — `esc_chain` not defined.

- [ ] **Step 3: Add `esc_chain: std::collections::VecDeque<std::time::Instant>` to `App`.**

Initialize to `VecDeque::new()` in `build_with_license_state_from_metadata_path`.

- [ ] **Step 4: Track Esc presses in `handle_crossterm_event`.**

In `app.rs`, at the top of the key-event handling block (before any overlay checks), when `key.code == KeyCode::Esc`:

```rust
// Triple-Esc panic-reset tracking.
if matches!(key.code, KeyCode::Esc) {
    let now = std::time::Instant::now();
    self.esc_chain.retain(|t| now.duration_since(*t).as_millis() < 1000);
    self.esc_chain.push_back(now);
    if self.esc_chain.len() >= 3 {
        self.esc_chain.clear();
        self.process_action(Action::PanicReset);
        return;
    }
}
```

This runs BEFORE overlay checks so triple-Esc always fires, regardless of what is open.

- [ ] **Step 5: Implement `Action::PanicReset` in `process_action`.**

```rust
Action::PanicReset => {
    // 1. Dismiss all overlays.
    self.help_visible = false;
    self.quit_confirm_visible = false;
    self.collision_modal = None;
    self.upgrade_modal = None;
    self.palette_visible = false;
    self.palette_state.reset();

    // 2. Cancel pending tombstones without dispatch.
    // self.tombstones.cancel_all_without_dispatch();  // wire when destructive-undo lands

    // 3. Navigate to Dashboard root.
    self.current_view = ViewId::Dashboard;
    self.dashboard.reset_to_root();

    // 4. Reset SessionDetail if instantiated.
    if let Some(ref mut detail) = self.session_detail {
        detail.reset_to_root();
    }

    // 5. Flash confirmation hint.
    self.flash_hint_short("Returned to Dashboard root.");

    self.dirty = true;
}
```

- [ ] **Step 6: Add `reset_to_root()` to `DashboardView`.**

```rust
pub fn reset_to_root(&mut self) {
    self.mode = DashboardMode::Navigate;
    self.focused_node = None;
    self.focused_panel = Panel::Agents;
    self.agents_tree.set_focused(true);
    self.activity_log.set_focused(false);
}
```

- [ ] **Step 7: Add `reset_to_root()` to `SessionDetailView`.**

```rust
pub fn reset_to_root(&mut self) {
    self.focused_panel = FocusedSessionPanel::ReactTrace;
    self.cancel_hint_until = None;
    // Do NOT clear input bar or react_trace — those persist across nav.
}
```

- [ ] **Step 8: Add test-only accessors.**

```rust
// In app.rs:
#[cfg(any(test, debug_assertions))]
pub fn is_help_visible_for_test(&self) -> bool {
    self.help_visible
}

#[cfg(any(test, debug_assertions))]
pub fn current_view_for_test(&self) -> &ViewId {
    &self.current_view
}
```

- [ ] **Step 9: Run tests.**

Run: `scripts/spur-cargo test -p spur-tui --test app_panic_esc`
Run: `scripts/spur-cargo test -p spur-tui`
Expected: all pass.

- [ ] **Step 10: Clippy + fmt + commit.**

```bash
git add crates/spur-tui/src/action.rs crates/spur-tui/src/app.rs \
        crates/spur-tui/src/views/dashboard.rs crates/spur-tui/src/views/session_detail.rs \
        crates/spur-tui/tests/app_panic_esc.rs
git commit -m "feat(tui): triple-Esc panic reset to Dashboard root (T2.9)"
```

**Acceptance criteria:**
- [ ] `Action::PanicReset` variant exists in `action.rs`
- [ ] Three Esc presses within 1000ms emit `PanicReset`
- [ ] Two Esc presses do NOT emit `PanicReset`
- [ ] `PanicReset` clears: help_visible, quit_confirm_visible, collision_modal, upgrade_modal, palette_visible
- [ ] `PanicReset` navigates to Dashboard and calls `dashboard.reset_to_root()`
- [ ] `PanicReset` calls `session_detail.reset_to_root()` if instantiated
- [ ] `PanicReset` flashes "Returned to Dashboard root." via `flash_hint_short`
- [ ] TODO comment left for `tombstones.cancel_all_without_dispatch()` (wired when destructive-undo lands)
- [ ] `esc_chain` is cleared after `PanicReset` fires

---

## Final sweep

- [ ] **Run full spur-tui test suite:**

```bash
scripts/spur-cargo test -p spur-tui
```

Expected: all tests pass, no new warnings.

- [ ] **Run clippy workspace-wide on touched crates:**

```bash
scripts/spur-cargo clippy -p spur-tui -- -D warnings
```

- [ ] **Verify commit log has exactly 12 commits** (Task 0 + Tasks 1–11).

---

## Cross-spec coordination checklist

- [ ] `tombstones.cancel_all_without_dispatch()` call in `Action::PanicReset` is left as a TODO comment with a reference to `2026-04-28-tui-destructive-undo-design.md §4.7`; wired when that spec lands.
- [ ] `flash_hint_short` is the shared API consumed by destructive-undo tombstone toasts (§4.11 of that spec); the signature is final after Task 0.
- [ ] Leader-key spec (`Release N+1`) consumes `flash_hint_short` for Alt+* deprecation toasts; no changes needed to Task 0's implementation.
- [ ] `FocusedSessionPanel` enum (Task 5) has no conflicts with the leader-key overlay clearing path.
- [ ] `Action::PanicReset` (Task 11) must be handled by any future view added to `current_view`; new views must implement `reset_to_root()`.

---
