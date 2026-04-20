# `/clear` TUI View Reset & Ready Affordance — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `/clear` visually wipe the session pane immediately on submit and surface a "ready" banner, without corrupting draft metadata or creating a ghost-cleared-but-active-brain state, and without losing the user's post-`/clear` typing across the lazy-respawn into a fresh brain session.

**Architecture:** Eager client-side reset. Add a `reset_for_clear(&mut self)` method on `SessionDetailView` that zeros conversation-scoped fields and flips a `cleared: bool` marker. Gate BOTH draft-save paths (`force_save_draft` + `draft_save_action`) at the source on `!self.cleared` so no metadata write can target a retired session. Wire `reset_for_clear` into `Action::ClearSession` (eager, **but only after a successful `tx.try_send`** to prevent ghost-cleared state) and the `BrainRetired{UserClear}` event arm (defensive). Amend the view-replacement path at `app.rs:919-975` to carry the cleared view's InputBar contents over as the new view's draft.

**Tech Stack:** Rust 2021, `ratatui`, crate `spur-tui`. Unit tests run via `cargo test -p spur-tui`.

**Spec:** `docs/superpowers/specs/2026-04-20-clear-session-view-reset-design.md` (commits `8742b87` + `c301c58`). Plan revised after three-reviewer pass (opencode-acp, codex-acp, claude-code-acp) that surfaced 4 BLOCKERs in the first draft.

**Builds on commits:** `07c71d2`, `18fec81`, `4cfe528`.

---

## File Structure

| Path | Role |
|---|---|
| `crates/spur-tui/src/views/session_detail.rs` | Add fields (`cleared`, `ready_banner`); add `reset_for_clear`; gate `force_save_draft` + `draft_save_action` on `cleared`; render the ready banner. Hosts view-level unit tests. |
| `crates/spur-tui/src/app.rs` | Wire `Action::ClearSession` (send-first + tracing), extend `SpurEventBody::BrainRetired` arm (now at lines 1034-1055, gated on `reason == UserClear`, pattern must be changed to bind `reason`), amend view-replacement branch at lines 919-975 for carry-over. `brain_retired_tests` module grows. |

No new files. No other crate touched.

---

## Pre-flight check

- [ ] **P.1: Verify starting git state is clean**

Run: `git status --short`

Expected: no uncommitted modifications to `crates/spur-tui/`.

- [ ] **P.2: Verify baseline test suite is green**

Run: `cargo test -p spur-tui --lib`

Expected: all tests pass. Record the baseline count.

- [ ] **P.3: Verify target line ranges (read-only; do not edit)**

Confirm the following coordinates against the current tree using `Read`. If any range has drifted by more than ±5 lines, stop and re-verify before continuing.

- `crates/spur-tui/src/app.rs:1034-1055` — `SpurEventBody::BrainRetired { .. }` arm (**moved from 856-878 since the spec was first authored**; the current pattern uses the `{ .. }` wildcard and must be changed to `{ session, reason }` in Task 7).
- `crates/spur-tui/src/app.rs:919-975` — view-replacement branch (`needs_new = true`), wraps `force_flush_active_draft` call at `app.rs:932`.
- `crates/spur-tui/src/app.rs:1250-1267` — `Action::ClearSession` arm.
- `crates/spur-tui/src/app.rs:1811-1818` — `force_flush_active_draft`.
- `crates/spur-tui/src/views/session_detail.rs:32-109` — `SessionDetailView` struct fields (**NOTE: the struct has `react_trace: ReactTrace` and `mermaid_registry`; it has NO field named `trace`, `detail_pane`, or `inline_protocols` — the latter is a method `invalidate_inline_protocols()` at line 325 that operates on `mermaid_registry`**).
- `crates/spur-tui/src/views/session_detail.rs:250-266` — `draft_save_action` (debounce path, called from tick).
- `crates/spur-tui/src/views/session_detail.rs:275-287` — `force_save_draft` (user-intent-boundary path).
- `crates/spur-tui/src/views/session_detail.rs:307-309` — `input_bar_text(&self) -> String` (public, already present).
- `crates/spur-tui/src/views/session_detail.rs:325` — `invalidate_inline_protocols(&mut self)` (already present).
- `crates/spur-tui/src/views/session_detail.rs:399-402` — `set_current_mode` updates BOTH `self.current_mode` AND `self.react_trace.set_mode(mode)`; Task 3 MUST reset both.
- `crates/spur-tui/src/views/session_detail.rs:131-164` — full-form `new(..)` constructor (add new field initializers here).
- `crates/spur-tui/src/views/session_detail.rs:178-215` — `new_for_palette_test(..)` (also add new field initializers; the file's own doc comment at line 174 says "Every new field added to `SessionDetailView` must also be added here").

---

## Task 1: Scaffolding — add `cleared` flag and `ready_banner` field

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs` (struct + both constructors)

- [ ] **Step 1: Write the failing test**

Append to the existing test module (locate with `grep -n '^#\[cfg(test)\]' crates/spur-tui/src/views/session_detail.rs`):

```rust
#[test]
fn new_view_defaults_cleared_false_and_no_ready_banner() {
    let view = SessionDetailView::new_for_palette_test(
        crate::commands::CommandRegistry::default(),
    );
    assert!(!view.is_cleared(), "new view must default cleared=false");
    assert!(
        view.ready_banner_text().is_none(),
        "new view must not start with a ready banner"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p spur-tui --lib views::session_detail`

Expected: compile error — `is_cleared` and `ready_banner_text` do not exist.

- [ ] **Step 3: Add fields and accessors**

Near the bottom of the struct (after `tool_depth` and `known_worker_names` at lines ~104-109), add two fields:

```rust
/// True once this view has been reset by `/clear` and is waiting for
/// the next `BrainSpawned` to be replaced. While `cleared`, the view's
/// `session_id` is treated as opaque — `force_save_draft` and
/// `draft_save_action` both return `None` early so no metadata write
/// can target the retired session. See spec §3.5.
cleared: bool,

/// Transient banner rendered in the same layout slot as
/// `resume_banner` when the view has been cleared. Cleared by
/// construction of the next view (replacement drops it naturally).
ready_banner: Option<String>,
```

In the full-form `new(..)` constructor (lines 131-164) add, immediately before the closing `}`:

```rust
cleared: false,
ready_banner: None,
```

In `new_for_palette_test(..)` (lines 178-215) add the same two initializers, keeping the same order to satisfy the maintenance rule in the doc comment at line 174.

Below the existing `input_bar_text` method (after line 309), add:

```rust
/// True once this view has been reset for `/clear` and is awaiting replacement.
pub fn is_cleared(&self) -> bool {
    self.cleared
}

/// The ready-banner text for this view, if any.
pub fn ready_banner_text(&self) -> Option<&str> {
    self.ready_banner.as_deref()
}
```

At the top of the file (after the existing `use` block), add the constant:

```rust
const READY_BANNER_TEXT: &str =
    "✨ Session cleared — your next prompt starts a fresh brain.";
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p spur-tui --lib views::session_detail::tests::new_view_defaults_cleared_false_and_no_ready_banner`

Expected: PASS.

- [ ] **Step 5: Verify the rest of the workspace still compiles**

Run: `cargo check -p spur-tui --tests`

Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(tui): add cleared flag and ready_banner field to SessionDetailView (spec §3.1, §3.5)

Scaffolding for /clear view reset. Fields default to false/None on
both production and palette-test constructors. Public accessors
is_cleared and ready_banner_text enable subsequent TDD steps."
```

---

## Task 2: `reset_for_clear` — conversation state + caches

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

Addresses the BLOCKER that invalidated the first draft of this task: the real struct has `react_trace`, `mermaid_registry`, `tool_depth`, `picker_shell`, `trigger_detector`, `mention_registry` — **not** `trace`, `detail_pane`, or `inline_protocols` as fields.

- [ ] **Step 1: Write the failing test**

Append to the `session_detail.rs` test module. Same-file tests can access private fields directly (no test-only helpers needed):

```rust
#[test]
fn reset_for_clear_wipes_conversation_state() {
    let mut view = SessionDetailView::new_for_palette_test(
        crate::commands::CommandRegistry::default(),
    );
    // Seed state that reset_for_clear must wipe.
    view.tool_depth.insert("t1".to_string(), 2);
    #[cfg(feature = "markdown")]
    view.mermaid_registry.insert(
        crate::components::mermaid::MermaidId(1),
        crate::components::mermaid::MermaidState::Rendering,
    );

    view.reset_for_clear();

    assert!(view.tool_depth.is_empty(), "tool_depth must be cleared");
    // ReactTrace must be empty after reset — use whatever public
    // emptiness accessor exists on ReactTrace (grep
    // components/react_trace/mod.rs for `pub fn len\|is_empty\|entry_count`).
    // If no direct accessor, assert via rendered output in Task 10.
    // For now, assert the flag was set:
    assert!(view.is_cleared());
    assert_eq!(
        view.ready_banner_text(),
        Some(READY_BANNER_TEXT)
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p spur-tui --lib views::session_detail::tests::reset_for_clear_wipes_conversation_state`

Expected: compile error — `reset_for_clear` does not exist.

- [ ] **Step 3: Implement `reset_for_clear` (partial — conversation/caches only)**

Place this method on the `impl SessionDetailView` block near the other `&mut self` methods (right after `show_resume_banner` at line ~226 is a natural home):

```rust
/// Wipe conversation-scoped state in place so the same view can host
/// the next prompt without reconstruction.
///
/// Called by `Action::ClearSession` (eager, gated on a successful
/// `tx.try_send`) and by the `BrainRetired{UserClear}` event arm
/// (defensive, idempotent).
///
/// # Classification policy
///
/// Every field on `SessionDetailView` MUST have a deliberate
/// classification here. When adding a new field, update this method
/// AND update `SessionDetailView::new` / `new_for_palette_test` per
/// the existing maintenance rule (see doc comment at line ~174).
///
/// **Cleared** (reset to the empty/default value for that field):
/// - Conversation: `react_trace` (content wiped; keeps its `AgentKind`
///   and `mermaid_enabled` config), `tool_depth`, `mermaid_registry`,
///   `pending_fence_actions`.
/// - Header/status (Task 3): `cost`, `started_at`, `current_mode`
///   (plus `react_trace.set_mode(None)` — see §3.3 of spec), `context_used`,
///   `context_size`, `auth_error`.
/// - Stream flags (Task 3): `stream_in_flight`, `cancelling_in_flight`.
/// - UI transient: `resume_banner`, `picker_shell`, `trigger_detector`.
/// - Draft debounce locals (Task 4): `last_persisted_draft`,
///   `last_draft_change_at`.
/// - Marks set: `cleared = true`, `ready_banner = Some(...)`.
///
/// **Preserved** (the view survives, only its conversation is wiped):
/// - `session_id`, `agent_name`, `role`, `agent_cfg`, `cwd`,
///   `command_registry`, `mention_registry`, `input_bar`,
///   `cancel_mode`, `workers_panel_collapsed`, `known_worker_names`,
///   `render_picker` (mermaid picker).
pub fn reset_for_clear(&mut self) {
    tracing::debug!(
        session = %self.session_id.0,
        "SessionDetailView::reset_for_clear"
    );
    // Conversation / caches.
    self.react_trace.clear();
    self.tool_depth.clear();
    self.invalidate_inline_protocols();
    #[cfg(feature = "markdown")]
    {
        self.mermaid_registry.clear();
        self.pending_fence_actions.clear();
    }
    self.trigger_detector.reset();
    self.picker_shell = None;
    self.resume_banner = None;

    // Marks — header/status fields land in Task 3; draft locals in Task 4.
    self.cleared = true;
    self.ready_banner = Some(READY_BANNER_TEXT.to_string());
}
```

**If `ReactTrace::clear()` does not exist:** locate `components/react_trace/mod.rs` and grep for an existing `pub fn clear\|truncate\|reset`. If none, add one that wipes entries while preserving `AgentKind` and `mermaid_enabled`. Keep that sub-task in this step; it's a minimal one-liner. Do NOT introduce larger refactors.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p spur-tui --lib views::session_detail::tests::reset_for_clear_wipes_conversation_state`

Expected: PASS.

- [ ] **Step 5: Run the full session_detail test module**

Run: `cargo test -p spur-tui --lib views::session_detail`

Expected: all existing tests still pass (including `invalidate_clears_inline_protocols_on_all_ready_states` at line ~1787).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs crates/spur-tui/src/components/react_trace/
git commit -m "feat(tui): reset_for_clear wipes conversation state (spec §3.1)

Clears react_trace, tool_depth, mermaid state, trigger detector,
picker shell, resume banner. Sets cleared=true and ready_banner.
Header/status fields and draft locals land in subsequent commits."
```

---

## Task 3: `reset_for_clear` — header/status/stream fields + react_trace mode mirror

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

Covers spec §6 test 7. Addresses the codex-acp BLOCKER: `set_current_mode` propagates into `react_trace`, so reset must too.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn reset_for_clear_clears_header_status_fields() {
    let mut view = SessionDetailView::new_for_palette_test(
        crate::commands::CommandRegistry::default(),
    );
    // Seed via existing public APIs.
    view.set_current_mode(Some("plan".into()));
    view.cost = 1.23;
    view.context_used = Some(1234);
    view.context_size = Some(200_000);
    view.auth_error = Some("auth failed".into());
    view.stream_in_flight = true;
    view.cancelling_in_flight = true;

    view.reset_for_clear();

    assert_eq!(view.cost, 0.0);
    assert_eq!(view.current_mode, None);
    assert_eq!(view.context_used, None);
    assert_eq!(view.context_size, None);
    assert_eq!(view.auth_error, None);
    assert!(!view.stream_in_flight);
    assert!(!view.cancelling_in_flight);
    // react_trace's mode mirror must also reset. If ReactTrace has a
    // `current_mode` accessor, use it; else verify via rendered title
    // in Task 10.
    // At minimum, verify the mode was explicitly cleared:
    // (add a `pub fn current_mode(&self) -> Option<&str>` on ReactTrace
    // if none exists — one-liner read accessor is acceptable scope).
    assert_eq!(view.react_trace.current_mode(), None);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p spur-tui --lib views::session_detail::tests::reset_for_clear_clears_header_status_fields`

Expected: FAIL — header/status values remain after reset.

- [ ] **Step 3: Extend `reset_for_clear`**

Insert into the `reset_for_clear` body, immediately before the `self.cleared = true` line:

```rust
// Header / status.
self.cost = 0.0;
self.started_at = std::time::Instant::now();
self.current_mode = None;
self.react_trace.set_mode(None); // mirror for pane-title badge (set_current_mode pattern)
self.context_used = None;
self.context_size = None;
self.auth_error = None;
// Stream flags.
self.stream_in_flight = false;
self.cancelling_in_flight = false;
```

If `ReactTrace::current_mode(&self) -> Option<&str>` does not exist, add it as a read-only one-liner in `components/react_trace/mod.rs`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p spur-tui --lib views::session_detail::tests::reset_for_clear_clears_header_status_fields`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs crates/spur-tui/src/components/react_trace/
git commit -m "feat(tui): reset_for_clear wipes header/status/stream + react_trace mode mirror (spec §3.1)

Prevents a cleared pane from continuing to show retired-session
cost, timer, mode badge (both on the status line AND in the trace
pane title — set_current_mode updates both locations and reset
must too), context utilization, auth banner, or 'Esc to stop'
streaming hint."
```

---

## Task 4: Draft-save gating at the source + draft debounce locals in `reset_for_clear`

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

Addresses the codex-acp BLOCKER: gating only at the replacement-path call site is insufficient because `draft_save_action` runs from `App::tick` on every keystroke independently. Gate at the source so no `Action::SaveDraft` keyed on the retired `session_id` can ever be emitted.

- [ ] **Step 1: Write the failing test — source-level gating**

```rust
#[test]
fn cleared_view_suppresses_force_save_draft() {
    let mut view = SessionDetailView::new_for_palette_test(
        crate::commands::CommandRegistry::default(),
    );
    view.reset_for_clear();
    // Even with new text in the InputBar, force_save_draft must not
    // emit an Action keyed on the retired session_id.
    view.input_bar.set_text("new text".into(), 8);
    assert!(
        view.force_save_draft().is_none(),
        "cleared view must suppress force_save_draft"
    );
}

#[test]
fn cleared_view_suppresses_draft_save_action() {
    let mut view = SessionDetailView::new_for_palette_test(
        crate::commands::CommandRegistry::default(),
    );
    view.reset_for_clear();
    view.input_bar.set_text("new text".into(), 8);
    // Simulate a debounce trigger: set last_draft_change_at 600ms ago.
    view.test_set_last_draft_change(
        std::time::Instant::now() - std::time::Duration::from_millis(600),
    );
    assert!(
        view.draft_save_action().is_none(),
        "cleared view must suppress draft_save_action (debounce tick)"
    );
}

#[test]
fn reset_for_clear_wipes_draft_debounce_locals() {
    let mut view = SessionDetailView::new_for_palette_test(
        crate::commands::CommandRegistry::default(),
    );
    view.last_persisted_draft = "stale".into();
    view.last_draft_change_at = Some(std::time::Instant::now());
    view.reset_for_clear();
    assert_eq!(view.last_persisted_draft, "");
    assert!(view.last_draft_change_at.is_none());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p spur-tui --lib views::session_detail::tests::cleared_view_suppresses_force_save_draft views::session_detail::tests::cleared_view_suppresses_draft_save_action views::session_detail::tests::reset_for_clear_wipes_draft_debounce_locals`

Expected: FAIL (suppressions not implemented; debounce locals not cleared).

- [ ] **Step 3: Add the source-level guard to both save methods**

At the top of `force_save_draft` (line 275, before reading `input_bar.text()`):

```rust
pub fn force_save_draft(&mut self) -> Option<Action> {
    if self.cleared {
        // A cleared view's session_id is opaque; any SaveDraft keyed
        // on it would corrupt the retired session's metadata.
        // Carry-over into the next view happens in the App-side
        // replacement path via `restore_draft`. See spec §3.5.
        self.last_draft_change_at = None;
        return None;
    }
    let current = self.input_bar.text().to_string();
    // ...existing body unchanged...
}
```

Same guard at the top of `draft_save_action` (line 250):

```rust
pub fn draft_save_action(&mut self) -> Option<Action> {
    if self.cleared {
        self.last_draft_change_at = None;
        return None;
    }
    let at = self.last_draft_change_at?;
    // ...existing body unchanged...
}
```

Then extend `reset_for_clear`, adding before `self.cleared = true`:

```rust
// Draft debounce locals (spec §3.5). Gate is ALSO at the source in
// force_save_draft/draft_save_action — this local wipe is
// belt-and-suspenders for the debounce's own state machine.
self.last_persisted_draft.clear();
self.last_draft_change_at = None;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run the same command as Step 2.

Expected: PASS (all three).

- [ ] **Step 5: Run the whole view test module — no regressions**

Run: `cargo test -p spur-tui --lib views::session_detail`

Expected: all tests pass. Existing draft-persistence tests should be unaffected because they don't set `cleared = true`.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(tui): gate force_save_draft + draft_save_action on cleared (spec §3.5)

Source-level gating closes the draft-ownership leak at both
paths: the user-intent boundary (force_save_draft) and the
500ms tick-debounce (draft_save_action). Without this, any
keystroke post-/clear re-arms last_draft_change_at and the next
tick writes under the retired session_id. reset_for_clear also
wipes the debounce locals as belt-and-suspenders."
```

---

## Task 5: `reset_for_clear` idempotence

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

Spec §6 test 6. This is a **property-lock test**: the property is already structural, so the test may pass on first run. That's acceptable — the value is preventing future regressions.

- [ ] **Step 1: Write the test**

```rust
#[test]
fn reset_for_clear_is_idempotent() {
    let mut view = SessionDetailView::new_for_palette_test(
        crate::commands::CommandRegistry::default(),
    );
    view.react_trace.clear(); // normalize
    view.tool_depth.insert("seeded".into(), 1);
    view.reset_for_clear();
    let banner1 = view.ready_banner_text().map(str::to_string);
    view.reset_for_clear();
    let banner2 = view.ready_banner_text().map(str::to_string);
    assert_eq!(banner1, banner2);
    assert!(view.is_cleared());
    assert!(view.tool_depth.is_empty());
}
```

- [ ] **Step 2: Run the test (may pass immediately — acceptable)**

Run: `cargo test -p spur-tui --lib views::session_detail::tests::reset_for_clear_is_idempotent`

Expected: PASS on first run. If it fails, a side-effecting statement was introduced in earlier tasks; find and fix by making it a direct assignment.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "test(tui): lock reset_for_clear idempotence (spec §6 test 6)

Property-lock test. Double-call must be a no-op because both
Action::ClearSession and BrainRetired{UserClear} may call it."
```

---

## Task 6: Wire `Action::ClearSession` (send-first, gated reset, tracing on failure)

**Files:**
- Modify: `crates/spur-tui/src/app.rs:1250-1267` — the `Action::ClearSession` arm.
- Modify: `crates/spur-tui/src/app.rs` — `brain_retired_tests` module.

Covers spec §6 tests 1, 2, 8 AND spec §3.6 (ghost-clear prevention).

- [ ] **Step 1: Write the failing test — basic reset on successful send**

Add the following at the end of `mod brain_retired_tests`. Same-module tests can touch private `App` fields; use them directly rather than adding new `*_for_test` helpers.

```rust
#[test]
fn clear_session_resets_session_detail_on_successful_send() {
    let mut app = App::new_for_tests();
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));
    // Seed trace via whatever mutation the existing App tests use. If
    // none exists at the App level, push directly through the event
    // handler (AgentMessageChunk with a stub content). If that is
    // cumbersome, just verify the flags: the view-level tests in
    // Task 2 already cover trace wiping.

    let sid_before = app.session_detail.as_ref().unwrap().session_id().clone();
    let _ = app.process_action(Action::ClearSession);

    let detail = app.session_detail.as_ref().expect("view must still exist");
    assert!(detail.is_cleared());
    assert!(detail.ready_banner_text().is_some());
    assert_eq!(detail.session_id(), &sid_before, "session_id stays retired");
    assert_eq!(app.brain_status, BrainStatus::Idle);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p spur-tui --lib brain_retired_tests::clear_session_resets_session_detail_on_successful_send`

Expected: FAIL — `is_cleared()` returns false because the arm does not call `reset_for_clear` yet.

- [ ] **Step 3: Rewire the `Action::ClearSession` arm (send-first ordering)**

Replace the current arm body (lines 1250-1267) in full:

```rust
Action::ClearSession => {
    // /clear is a spur-local META command. Spec §3.6 requires
    // send-first ordering: if the channel send fails, the brain is
    // NOT retired, so we must NOT visually reset the view —
    // otherwise the user sees "cleared" while the stale brain is
    // still active (ghost-cleared state).
    let send_ok = match self.user_input_tx.as_ref() {
        Some(tx) => match tx.try_send(UserInput::NewSessionWithMessage {
            blocks: vec![],
            interrupt: false,
        }) {
            Ok(()) => true,
            Err(e) => {
                tracing::error!(
                    err = ?e,
                    "Action::ClearSession: user_input tx send failed — \
                     brain NOT retired; view NOT reset to avoid ghost-cleared state"
                );
                false
            }
        },
        None => {
            tracing::error!(
                "Action::ClearSession: user_input_tx is None; \
                 cannot retire brain — view NOT reset"
            );
            false
        }
    };

    if send_ok {
        self.brain_status = BrainStatus::Idle;
        if let Some(ref mut detail) = self.session_detail {
            detail.reset_for_clear();
        }
        self.sync_brain_status();
        self.dirty = true;
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p spur-tui --lib brain_retired_tests::clear_session_resets_session_detail_on_successful_send`

Expected: PASS.

- [ ] **Step 5: Write InputBar-preservation test**

```rust
#[test]
fn clear_session_preserves_input_bar_contents() {
    let mut app = App::new_for_tests();
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));
    app.session_detail
        .as_mut()
        .unwrap()
        .input_bar
        .set_text("typed before clear".into(), 18);

    let _ = app.process_action(Action::ClearSession);

    assert_eq!(
        app.session_detail.as_ref().unwrap().input_bar_text(),
        "typed before clear"
    );
}
```

Run: `cargo test -p spur-tui --lib brain_retired_tests::clear_session_preserves_input_bar_contents`

Expected: PASS (no additional production change — InputBar is Preserved per §3.1).

- [ ] **Step 6: Write streaming-at-clear test**

```rust
#[test]
fn clear_while_streaming_does_not_panic_and_resets_flags() {
    let mut app = App::new_for_tests();
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));
    app.session_detail.as_mut().unwrap().stream_in_flight = true;

    let _ = app.process_action(Action::ClearSession);

    let detail = app.session_detail.as_ref().unwrap();
    assert!(!detail.stream_in_flight);
    assert!(detail.is_cleared());
}
```

Run: `cargo test -p spur-tui --lib brain_retired_tests::clear_while_streaming_does_not_panic_and_resets_flags`

Expected: PASS (Task 3 already wires the stream flags).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "feat(tui): Action::ClearSession eagerly resets view on successful send (spec §3.3, §3.6)

Send-first ordering prevents ghost-cleared UX: if the channel send
fails, the brain is NOT retired, so the view is NOT reset and the
user sees 'nothing happened' (correct affordance — they retry).
tracing::error! on the failure path gives post-mortems something
to grep. Covers spec tests 1, 2, 8."
```

---

## Task 7: Wire `BrainRetired{UserClear}` defensive path

**Files:**
- Modify: `crates/spur-tui/src/app.rs:1034-1055` — the `BrainRetired` arm.
- Modify: `crates/spur-tui/src/app.rs` — tests in `brain_retired_tests`.

Covers spec §6 tests 3, 4, 9. The arm pattern must be changed from `{ .. }` to `{ reason, .. }` first.

- [ ] **Step 1: Write the failing test — UserClear defensive path**

```rust
#[test]
fn brain_retired_user_clear_resets_view_defensively() {
    let mut app = App::new_for_tests();
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));

    app.handle_spur_event(wrap(SpurEventBody::BrainRetired {
        session: SessionId("b1".into()),
        reason: BrainRetireReason::UserClear,
    }));

    let detail = app.session_detail.as_ref().unwrap();
    assert!(detail.is_cleared());
    assert!(detail.ready_banner_text().is_some());
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p spur-tui --lib brain_retired_tests::brain_retired_user_clear_resets_view_defensively`

Expected: FAIL — `is_cleared()` returns false because the arm does not call `reset_for_clear`.

- [ ] **Step 3: Extend the `BrainRetired` arm (lines 1034-1055)**

Change the pattern from `SpurEventBody::BrainRetired { .. }` to `SpurEventBody::BrainRetired { reason, .. }`. Then, after the existing `metadata_store.save()` block (after the closing `}` of the `if let Err(...) = ...save() { ... }` at line ~1054), insert:

```rust
// Defensive belt-and-suspenders reset for the UserClear path.
// Idempotent against Action::ClearSession's eager reset.
// Gated on UserClear only:
//  - ResumeSwitch: in-flight ResumeSession is already loading the next
//    brain via BrainSpawned (app.rs:919-975); resetting here would
//    briefly blank the new view mid-load.
//  - Shutdown: terminal; reset is moot.
if matches!(reason, BrainRetireReason::UserClear) {
    tracing::info!("BrainRetired{{UserClear}}: defensive view reset");
    if let Some(ref mut detail) = self.session_detail {
        detail.reset_for_clear();
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p spur-tui --lib brain_retired_tests::brain_retired_user_clear_resets_view_defensively`

Expected: PASS.

- [ ] **Step 5: Write the ResumeSwitch non-interference test**

```rust
#[test]
fn brain_retired_resume_switch_does_not_reset_view() {
    let mut app = App::new_for_tests();
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));

    app.handle_spur_event(wrap(SpurEventBody::BrainRetired {
        session: SessionId("b1".into()),
        reason: BrainRetireReason::ResumeSwitch,
    }));

    let detail = app.session_detail.as_ref().unwrap();
    assert!(!detail.is_cleared(), "ResumeSwitch must NOT trigger view reset");
    assert!(detail.ready_banner_text().is_none());
}
```

Run: `cargo test -p spur-tui --lib brain_retired_tests::brain_retired_resume_switch_does_not_reset_view`

Expected: PASS.

- [ ] **Step 6: Write the Shutdown no-panic test**

```rust
#[test]
fn brain_retired_shutdown_does_not_panic() {
    let mut app = App::new_for_tests();
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    }));

    app.handle_spur_event(wrap(SpurEventBody::BrainRetired {
        session: SessionId("b1".into()),
        reason: BrainRetireReason::Shutdown,
    }));

    let detail = app.session_detail.as_ref().unwrap();
    assert!(!detail.is_cleared());
}
```

Run: `cargo test -p spur-tui --lib brain_retired_tests::brain_retired_shutdown_does_not_panic`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "feat(tui): BrainRetired{UserClear} defensively resets view (spec §3.4)

Gated on UserClear; ResumeSwitch and Shutdown untouched. Arm
pattern changed from { .. } to { reason, .. } to bind the
discriminant. tracing::info! marks the reset for post-mortem.
Idempotent against Action::ClearSession. Covers spec tests 3, 4, 9."
```

---

## Task 8: Draft carry-over across view replacement

**Files:**
- Modify: `crates/spur-tui/src/app.rs:919-975` — the `needs_new = true` branch.
- Modify: `crates/spur-tui/src/app.rs` — tests.

Covers spec §3.5 and §6 tests 10, 11. **Simplified** from the first draft: source-level gating in Task 4 means the replacement path does NOT need to conditionally skip `force_flush_active_draft` — that call is now a no-op for a cleared view by construction. The only behavior added here is carry-over capture + application.

- [ ] **Step 1: Write the failing end-to-end carryover test**

```rust
#[test]
fn draft_carryover_across_clear_to_new_brain_spawn() {
    let mut app = App::new_for_tests();
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("a".into()),
    }));
    // Seed session A's saved draft.
    app.session_detail.as_mut().unwrap().input_bar.set_text("draft-A".into(), 7);
    let _ = app.process_action(Action::SaveDraft {
        session_id: "a".into(),
        draft: "draft-A".into(),
    });

    // User submits /clear.
    let _ = app.process_action(Action::ClearSession);

    // User types a new prompt into the preserved InputBar.
    app.session_detail.as_mut().unwrap().input_bar.set_text(
        "post-clear-prompt".into(),
        17,
    );

    // New brain B spawns.
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b".into()),
    }));

    // A's saved draft was NOT corrupted.
    let metadata_a_draft = app
        .metadata_store
        .entry("a")
        .map(|e| e.draft.clone())
        .unwrap_or_default();
    assert_eq!(metadata_a_draft, "draft-A");

    // New view for B has the carryover.
    let detail = app.session_detail.as_ref().unwrap();
    assert_eq!(detail.session_id().0, "b");
    assert_eq!(detail.input_bar_text(), "post-clear-prompt");
    assert_eq!(detail.last_persisted_draft, "post-clear-prompt");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p spur-tui --lib brain_retired_tests::draft_carryover_across_clear_to_new_brain_spawn`

Expected: FAIL — new view's `input_bar_text` is empty (carryover not applied). Session A's draft should already be safe (Task 4's source-level gating).

- [ ] **Step 3: Amend the view-replacement branch at `app.rs:919-975` (full replacement of the `if needs_new { ... }` block)**

Full replacement — no "preserve the rest verbatim" ambiguity. The block below is the complete body that follows `if needs_new {`:

```rust
if needs_new {
    // Carry-over: a cleared view's InputBar text belongs to the NEW
    // session, not the retired one. Capture owned text before
    // dropping the old view. Source-level gating in
    // force_save_draft / draft_save_action (spec §3.5) means
    // force_flush_active_draft is a no-op for a cleared view, so
    // no call-site gating is required here.
    let carryover: Option<String> = self
        .session_detail
        .as_ref()
        .filter(|d| d.is_cleared())
        .map(|d| d.input_bar_text());
    tracing::debug!(
        carryover_len = carryover.as_deref().map(str::len).unwrap_or(0),
        "view-replacement: clear-carryover capture"
    );
    self.force_flush_active_draft();

    let agent_cfg = self.resolve_agent_config(agent);
    let mut view = SessionDetailView::new(
        session.clone(),
        agent.clone(),
        "brain".to_string(),
        std::env::current_dir().unwrap_or_default(),
        agent_cfg,
        self.build_worker_snapshot(),
    );
    #[cfg(feature = "markdown")]
    view.set_render_picker(self.mermaid_picker.clone());
    view.seed_input_history(self.metadata_store.metadata().input_history.clone());
    if let Some(entry) = self.metadata_store.entry(&session.0) {
        view.restore_draft(&entry.draft);
    }
    // Carry-over wins over any metadata draft (which is normally
    // empty for a freshly-minted spur_session_id anyway).
    // restore_draft is a no-op on empty input.
    if let Some(text) = carryover.as_deref() {
        view.restore_draft(text);
    }
    // Auto-resume banner — unchanged from the pre-revision branch.
    if self
        .metadata_store
        .metadata()
        .last_active_session_id
        .as_deref()
        == Some(session.0.as_str())
    {
        let title = self
            .metadata_store
            .entry(&session.0)
            .and_then(|e| e.title_override.clone())
            .unwrap_or_else(|| agent.clone());
        let quit_ago = humanize_since(
            self.metadata_store.metadata().last_active_at.as_deref(),
        );
        view.show_resume_banner(title, quit_ago);
        self.metadata_store.clear_last_active();
        if let Err(e) = self.metadata_store.save() {
            tracing::warn!(error = %e, "failed to persist cleared last_active");
        }
    }
    self.session_detail = Some(view);
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p spur-tui --lib brain_retired_tests::draft_carryover_across_clear_to_new_brain_spawn`

Expected: PASS.

- [ ] **Step 5: Write the empty-carryover edge test**

```rust
#[test]
fn draft_carryover_empty_is_noop() {
    let mut app = App::new_for_tests();
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("a".into()),
    }));
    let _ = app.process_action(Action::ClearSession);
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b".into()),
    }));

    let detail = app.session_detail.as_ref().unwrap();
    assert_eq!(detail.input_bar_text(), "");
    let md = &app.metadata_store;
    assert!(md.entry("a").map(|e| e.draft.clone()).unwrap_or_default().is_empty());
    assert!(md.entry("b").map(|e| e.draft.clone()).unwrap_or_default().is_empty());
}
```

Run: `cargo test -p spur-tui --lib brain_retired_tests::draft_carryover_empty_is_noop`

Expected: PASS.

- [ ] **Step 6: Run the full spur-tui test suite**

Run: `cargo test -p spur-tui --lib`

Expected: all tests pass. Pay attention to `session_switch`, `draft`, `force_flush`, `auto_resume`.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "feat(tui): carry over post-/clear InputBar draft to new session (spec §3.5)

When BrainSpawned replaces a cleared view, capture the InputBar
text from the old view and restore it on the new view after the
normal metadata-draft restore. Source-level gating in Task 4
already prevents force_flush from corrupting the retired session's
metadata. Covers spec tests 10, 11."
```

---

## Task 9: Banner lifecycle across brain spawn

**Files:**
- Modify: `crates/spur-tui/src/app.rs` — tests only. No production change expected.

Covers spec §6 test 5.

- [ ] **Step 1: Write and run the test**

```rust
#[test]
fn clear_session_banner_cleared_on_next_brain_spawn() {
    let mut app = App::new_for_tests();
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("a".into()),
    }));
    let _ = app.process_action(Action::ClearSession);
    assert!(app.session_detail.as_ref().unwrap().ready_banner_text().is_some());

    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b".into()),
    }));

    let detail = app.session_detail.as_ref().unwrap();
    assert!(detail.ready_banner_text().is_none());
    assert!(!detail.is_cleared());
}
```

Run: `cargo test -p spur-tui --lib brain_retired_tests::clear_session_banner_cleared_on_next_brain_spawn`

Expected: PASS.

- [ ] **Step 2: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "test(tui): lock banner lifecycle across clear→respawn (spec §6 test 5)"
```

---

## Task 10: Render the ready banner (frame-based, matching resume_banner)

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs` — the render path around the `banner_is_visible` check at line ~1474 and the paint at line ~1651.

The banner must render in the same layout slot as `resume_banner`. The two are mutually exclusive per spec R3. Pick a single render shape to match whichever style the existing auto-resume paint already uses.

- [ ] **Step 1: Read the existing render shape**

Read `crates/spur-tui/src/views/session_detail.rs:1470-1520` and `:1645-1660`. Note whether the existing paint uses buffer-based (`buf`) or frame-based (`frame.render_widget(...)` / `f.render_widget(...)`) calls. **Use the same shape** — the new paint must not mix rendering styles.

- [ ] **Step 2: Write the failing render test**

```rust
#[test]
fn ready_banner_renders_when_cleared() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut view = SessionDetailView::new_for_palette_test(
        crate::commands::CommandRegistry::default(),
    );
    view.reset_for_clear();

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| {
        // Match the render entrypoint used by other session_detail tests.
        // Grep: rg 'SessionDetailView.*render\b|\.render\(.*f' crates/spur-tui/src/views/session_detail.rs
        // and reuse the existing call signature.
        view.render_for_test(f, f.size());
    }).unwrap();
    let buffer = terminal.backend().buffer();
    let rendered: String = (0..buffer.area.height)
        .map(|y| (0..buffer.area.width)
            .map(|x| buffer.get(x, y).symbol.clone())
            .collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        rendered.contains("Session cleared"),
        "ready banner text must appear. Rendered:\n{rendered}"
    );
}
```

If `render_for_test` does not exist, use the view's actual public `render` entrypoint. If all render paths take additional context (e.g., app state) that makes unit-render tests infeasible, skip the render test here and rely on Task 11's end-to-end smoke; document the skip in the commit message.

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p spur-tui --lib views::session_detail::tests::ready_banner_renders_when_cleared`

Expected: FAIL — banner not rendered.

- [ ] **Step 4: Extend the render path**

Update `banner_is_visible()` (line ~1474):

```rust
fn banner_is_visible(&self) -> bool {
    self.resume_banner.is_some() || self.ready_banner.is_some()
}
```

In the paint block (line ~1651), match the existing frame-based style (assuming this is what the existing paint uses; confirm in Step 1). Prefer `resume_banner` on collision:

```rust
if let (Some(banner), Some(rect)) = (self.resume_banner.as_ref(), resume_banner_area) {
    // Existing auto-resume paint call — unchanged.
    // ... existing line ...
    if self.ready_banner.is_some() {
        tracing::warn!(
            "ready_banner and resume_banner both set — auto-resume wins (spec R3 violation)"
        );
    }
} else if let (Some(ready_text), Some(rect)) = (self.ready_banner.as_ref(), resume_banner_area) {
    use ratatui::style::{Modifier, Style};
    use ratatui::widgets::Paragraph;
    let styled = Paragraph::new(ready_text.as_str())
        .style(Style::default().add_modifier(Modifier::DIM | Modifier::ITALIC));
    // Use the same rendering call shape as the resume_banner above — do NOT mix styles.
    f.render_widget(styled, rect);
}
```

Replace `f.render_widget` with the existing call's shape if it differs.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p spur-tui --lib views::session_detail::tests::ready_banner_renders_when_cleared`

Expected: PASS (or skipped with a commit-message note if render-test infra is infeasible).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(tui): render ready banner in cleared session pane (spec §3.2, R3)

Paints '✨ Session cleared — your next prompt starts a fresh
brain.' in the same layout slot as the auto-resume banner. The
two are mutually exclusive per R3; auto-resume wins if both are
set (tracing::warn logs the invariant violation)."
```

---

## Task 11: End-to-end smoke test

**Files:**
- Modify: `crates/spur-tui/src/app.rs` — tests.

Covers spec §4 (data-flow diagram) end-to-end.

- [ ] **Step 1: Write and run the test**

```rust
#[test]
fn clear_end_to_end_flow() {
    let mut app = App::new_for_tests();

    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("a".into()),
    }));
    app.session_detail.as_mut().unwrap().input_bar.set_text("mid-thought".into(), 11);

    let _ = app.process_action(Action::ClearSession);
    {
        let d = app.session_detail.as_ref().unwrap();
        assert!(d.is_cleared());
        assert!(d.ready_banner_text().is_some());
        assert_eq!(d.input_bar_text(), "mid-thought");
    }

    app.handle_spur_event(wrap(SpurEventBody::BrainRetired {
        session: SessionId("a".into()),
        reason: BrainRetireReason::UserClear,
    }));
    {
        let d = app.session_detail.as_ref().unwrap();
        assert!(d.is_cleared());
        assert_eq!(d.input_bar_text(), "mid-thought");
    }

    app.session_detail.as_mut().unwrap().input_bar.set_text("explain quicksort".into(), 17);
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b".into()),
    }));

    let d = app.session_detail.as_ref().unwrap();
    assert_eq!(d.session_id().0, "b");
    assert!(!d.is_cleared());
    assert!(d.ready_banner_text().is_none());
    assert_eq!(d.input_bar_text(), "explain quicksort");
}
```

Run: `cargo test -p spur-tui --lib brain_retired_tests::clear_end_to_end_flow`

Expected: PASS.

- [ ] **Step 2: Run the full workspace to confirm no regressions**

Run: `cargo test --workspace`

Expected: all tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "test(tui): e2e /clear data flow smoke (spec §4)

ClearSession → eager reset → BrainRetired{UserClear} idempotent
re-reset → new keystrokes → BrainSpawned → clean pane with
carryover draft."
```

---

## Task 12: Misuse / concurrent tests

**Files:**
- Modify: `crates/spur-tui/src/app.rs` — tests only. No production change expected unless a test surfaces a bug.

Closes the claude-code-acp CONCERN on missing misuse coverage.

- [ ] **Step 1: Double-`ClearSession` back-to-back**

```rust
#[test]
fn double_clear_session_is_idempotent() {
    let mut app = App::new_for_tests();
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("a".into()),
    }));
    let _ = app.process_action(Action::ClearSession);
    let _ = app.process_action(Action::ClearSession);
    let d = app.session_detail.as_ref().unwrap();
    assert!(d.is_cleared());
    assert!(d.ready_banner_text().is_some());
}
```

- [ ] **Step 2: `/clear` while `resume_banner` is visible (R3 mutual-exclusion)**

```rust
#[test]
fn clear_over_resume_banner_takes_precedence() {
    let mut app = App::new_for_tests();
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("a".into()),
    }));
    app.session_detail.as_mut().unwrap().show_resume_banner("t".into(), "1s ago".into());

    let _ = app.process_action(Action::ClearSession);

    let d = app.session_detail.as_ref().unwrap();
    // reset_for_clear wipes resume_banner (Task 2); ready_banner is now the only one.
    assert!(d.resume_banner().is_none(), "resume_banner must be cleared by reset_for_clear");
    assert!(d.ready_banner_text().is_some());
}
```

- [ ] **Step 3: `/clear` mid-tool-call (tool_depth non-empty, stream_in_flight=false)**

```rust
#[test]
fn clear_mid_tool_call_clears_tool_depth() {
    let mut app = App::new_for_tests();
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("a".into()),
    }));
    app.session_detail.as_mut().unwrap().tool_depth.insert("t1".into(), 1);
    app.session_detail.as_mut().unwrap().tool_depth.insert("t2".into(), 2);

    let _ = app.process_action(Action::ClearSession);

    assert!(app.session_detail.as_ref().unwrap().tool_depth.is_empty());
}
```

- [ ] **Step 4: Draft debounce tick after /clear does not save**

```rust
#[test]
fn debounce_tick_after_clear_does_not_save_to_retired_session() {
    let mut app = App::new_for_tests();
    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("a".into()),
    }));
    // User had a draft 'draft-A' saved.
    let _ = app.process_action(Action::SaveDraft {
        session_id: "a".into(),
        draft: "draft-A".into(),
    });
    // /clear + new typing.
    let _ = app.process_action(Action::ClearSession);
    app.session_detail.as_mut().unwrap().input_bar.set_text("post-clear".into(), 10);
    // Force the debounce to trigger (600ms ago).
    app.session_detail.as_mut().unwrap().test_set_last_draft_change(
        std::time::Instant::now() - std::time::Duration::from_millis(600),
    );
    // Simulate the App tick's draft-save path. If there's a
    // single-entry function, call it; else call draft_save_action
    // directly — the key contract is that any returned Action would
    // reach apply_save_draft.
    let action = app.session_detail.as_mut().unwrap().draft_save_action();
    assert!(action.is_none(), "cleared view must not emit SaveDraft from tick");

    // A's draft must still be 'draft-A'.
    assert_eq!(
        app.metadata_store.entry("a").unwrap().draft,
        "draft-A"
    );
}
```

- [ ] **Step 5: Run all the new tests**

Run: `cargo test -p spur-tui --lib brain_retired_tests::double_clear_session_is_idempotent brain_retired_tests::clear_over_resume_banner_takes_precedence brain_retired_tests::clear_mid_tool_call_clears_tool_depth brain_retired_tests::debounce_tick_after_clear_does_not_save_to_retired_session`

Expected: PASS (all four).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "test(tui): misuse/concurrent coverage for /clear (review-driven)

Double-clear idempotence, /clear vs resume_banner mutual
exclusion, mid-tool-call tool_depth clearing, debounce-tick
source-level gate enforcement. Closes review CONCERN on
missing non-happy-path coverage."
```

---

## Post-implementation verification

- [ ] **V.1: Manual smoke test**

Launch the TUI. In an active session:
1. Send a prompt and wait for a response.
2. Type `/clear` + Enter. Observe immediate wipe, ready banner, focused InputBar.
3. Type a new prompt. Observe InputBar accepts input normally.
4. Hit Enter. Observe: new brain session, banner gone, typed text was not dropped.
5. Type `/clear` again immediately — observe idempotent, no panic.
6. (If possible to force) saturate the user_input channel: observe that `/clear` does NOT visually reset; `tracing::error!` appears in logs.

---

## Self-review (author)

**Spec coverage:** Every section maps to ≥1 task.
- §3.1 Cleared list → Tasks 2, 3, 4. Preserved list → Task 1 + Task 2 policy comment.
- §3.2 Ready banner → Tasks 1, 2 (set), 10 (render).
- §3.3 Action arm → Task 6.
- §3.4 BrainRetired arm → Task 7.
- §3.5 Draft carry-over + source-level gating → Tasks 4, 8.
- §3.6 Ghost-clear prevention → Task 6 (send-first + tracing::error).
- §4 Data flow → Task 11.
- §5 Files touched → session_detail.rs and app.rs only ✓
- §6 Tests 1–11 → Task 6 (1,2,8), Task 7 (3,4,9), Task 5 (6), Task 3 (7), Task 9 (5), Task 8 (10,11).
- §7 Risks R1–R4 → R1 resolved (Tasks 4, 8); R2 render detail in Task 10 const; R3 warning in Task 10 + test in Task 12; R4 classification policy in Task 2 doc comment.

**Placeholder scan:** No "TBD/TODO"; no "similar to"; no "add appropriate error handling". Task 2 Step 3 has a conditional about adding `ReactTrace::clear` if absent — that is a scoped one-liner explicitly sized, not a placeholder.

**Type consistency:** `reset_for_clear`, `is_cleared`, `ready_banner_text`, `READY_BANNER_TEXT`, `BrainRetireReason::{UserClear, ResumeSwitch, Shutdown}`, `Action::SaveDraft { session_id, draft }` — all used identically across tasks.

**Review-driven revisions applied:** ghost-clear (claude-code-acp [BLOCKER]); struct-field correctness in Task 2 (codex-acp [BLOCKER]); react_trace mode mirror in Task 3 (codex-acp [BLOCKER]); source-level draft gating in Task 4 (codex-acp [BLOCKER]); BrainRetired line drift + pattern binding (opencode-acp); tracing instrumentation at each new path; misuse-test coverage added as Task 12; render-shape ambiguity in Task 10 resolved to match existing; commit messages cite spec sections, no dangling refs.
