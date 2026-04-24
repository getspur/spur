# Session Resume — Tranche 2: Optimistic Navigation & LoadState Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Spec:** `docs/superpowers/specs/2026-04-24-session-resume-optimistic-nav-design.md`

**Prerequisite:** Tranche 1 (`2026-04-24-session-resume-tranche-1-server-correctness.md`) MUST be merged first. This tranche relies on `BrainError.session` carrying the real session id for correlation.

**Goal:** Eliminate the picker's local `resuming: bool` cache and move all async state to `SessionDetailView`, which renders a `LoadState` derived from milestone events. The picker dismisses in one frame; `SessionDetail` shows honest progress and surfaces failures inline.

**Architecture:** 
1. Delete `resuming: bool` from `PickerState::Populated`. 
2. On Enter for a non-current session, the picker returns `Action::ResumeSession` exactly as today — no pending flag. 
3. The app dispatcher (`app.rs`) now responds to `Action::ResumeSession` by sending the backend command AND navigating to `ViewId::SessionDetail` in the same tick.
4. `SessionDetailView` accepts an unloaded session as a first-class initial state, rendering a `LoadState` projection of the newest resume-pipeline event for that session id.
5. Five additive `SpurEventBody` variants (`SessionRetireStart`, `SessionRetireComplete`, `BrainConnecting`, `SessionLoading`, `SessionLoaded`) are emitted at phase boundaries in the orchestrator's resume pipeline. All existing consumers use `_ =>` catch-all arms, so the rollout is forward-compatible.
6. `session_picker` is added to `app.rs`'s `handle_spur_event` dispatch list. The picker's handler stays minimal (list-refresh only; it holds no async state to update).

**Tech Stack:** Rust 2021, Ratatui, existing `SpurEventBody` schema in `spur-acp`, Tokio broadcast channel.

**Out of scope:**
- Changes to `native.rs` ACP transport (covered by separate follow-up).
- `app.rs:1564` `TrySendError::Full` handling (separate follow-up).
- Snapshot / insta test refresh is flagged per-task but not batch-scripted.

---

## File structure

| Path | Responsibility | Create / Modify |
|---|---|---|
| `crates/spur-acp/src/domain/events.rs` | Add 5 milestone event variants to `SpurEventBody`. | Modify |
| `crates/spur-core/src/orchestrator.rs` | Emit milestone events at phase boundaries in the resume pipeline. | Modify |
| `crates/spur-tui/src/views/session_picker.rs` | Delete `resuming: bool`, its read sites, its render effects. | Modify |
| `crates/spur-tui/src/views/session_detail.rs` | Introduce `LoadState` enum and its event-driven update, add `SessionPending` / `SessionFailed` render branches. | Modify |
| `crates/spur-tui/src/app.rs` | Dispatch `Action::ResumeSession` to both the command channel AND `NavigateTo(SessionDetail)`; add `session_picker` to `handle_spur_event`. | Modify |
| `crates/spur-core/tests/session_milestone_events.rs` | Verify milestone events are emitted in order with faithful session ids on successful resume. | Create |
| `crates/spur-tui/src/views/session_picker.rs` (existing `mod tests`) | Add unit test asserting Enter does not wedge the picker. | Modify (add test) |
| `crates/spur-tui/tests/session_detail_load_state.rs` | Verify `LoadState` is derived from the newest milestone event for this session. | Create |
| `crates/spur-acp/tests/milestone_events_serde_roundtrip.rs` | Verify old `BrainError` + new milestone variants round-trip through serde unchanged. | Create |

---

### Task 1: Add milestone event variants to `SpurEventBody`

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs` (insert near the existing `BrainReconnecting/BrainReconnected/BrainReconnectFailed` block around line 494-515)

**Context:** Five additive variants that follow the exact shape of the existing `BrainReconnecting` family. All consumers use catch-all `_ =>` arms (verified in spec's blast-radius review), so adding variants does not break any match.

- [ ] **Step 1: Inspect the existing `BrainReconnecting/Reconnected/ReconnectFailed` block to match style**

Read `crates/spur-acp/src/domain/events.rs:483-530`. Note the doc-comment placement, field order, and `SessionId` usage.

- [ ] **Step 2: Write a failing serde round-trip test**

Create `crates/spur-acp/tests/milestone_events_serde_roundtrip.rs`:

```rust
use spur_acp::domain::events::{SessionId, SpurEvent, SpurEventBody};

fn roundtrip(body: SpurEventBody) -> SpurEventBody {
    let event = SpurEvent::now(body);
    let json = serde_json::to_string(&event).expect("serialize");
    let back: SpurEvent = serde_json::from_str(&json).expect("deserialize");
    back.body
}

#[test]
fn session_retire_start_roundtrips() {
    let body = SpurEventBody::SessionRetireStart {
        from: Some(SessionId::from("old")),
        to: SessionId::from("new"),
    };
    let back = roundtrip(body.clone());
    assert!(matches!(
        back,
        SpurEventBody::SessionRetireStart { .. }
    ));
}

#[test]
fn session_retire_complete_roundtrips() {
    let body = SpurEventBody::SessionRetireComplete {
        session: SessionId::from("s"),
    };
    assert!(matches!(
        roundtrip(body),
        SpurEventBody::SessionRetireComplete { .. }
    ));
}

#[test]
fn brain_connecting_roundtrips() {
    let body = SpurEventBody::BrainConnecting {
        session: SessionId::from("s"),
        brain_name: "claude-code".into(),
    };
    assert!(matches!(
        roundtrip(body),
        SpurEventBody::BrainConnecting { .. }
    ));
}

#[test]
fn session_loading_roundtrips() {
    let body = SpurEventBody::SessionLoading {
        session: SessionId::from("s"),
    };
    assert!(matches!(
        roundtrip(body),
        SpurEventBody::SessionLoading { .. }
    ));
}

#[test]
fn session_loaded_roundtrips() {
    let body = SpurEventBody::SessionLoaded {
        session: SessionId::from("s"),
    };
    assert!(matches!(
        roundtrip(body),
        SpurEventBody::SessionLoaded { .. }
    ));
}
```

- [ ] **Step 3: Run the tests to confirm they fail to compile (variants don't exist yet)**

Run: `cargo test -p spur-acp --test milestone_events_serde_roundtrip`

Expected: COMPILE ERROR — `SpurEventBody::SessionRetireStart` etc. unknown.

- [ ] **Step 4: Add the variants to `SpurEventBody` in `events.rs`**

Locate the `BrainReconnectFailed` variant (around line 511-515). Insert AFTER it, BEFORE the next existing variant, adding these five:

```rust
    /// Emitted at the start of `retire_active_brain` on the resume path.
    /// `from` is the session being retired (None if no active brain).
    /// `to` is the session the user asked to resume. Lets `SessionDetailView`
    /// render a "Retiring previous session…" initial state.
    SessionRetireStart {
        from: Option<SessionId>,
        to: SessionId,
    },
    /// Emitted when `retire_active_brain` completes (clean or forced).
    SessionRetireComplete {
        session: SessionId,
    },
    /// Emitted before `connect_brain` attempts to (re)spawn a brain process
    /// on the resume path. Lets the UI show "Connecting to claude-code…"
    /// while subprocess spawn (≥1s cold) is in flight.
    BrainConnecting {
        session: SessionId,
        brain_name: String,
    },
    /// Emitted before `load_brain_session` issues its ACP `session/load`
    /// RPC. Lets the UI show "Loading session history…".
    SessionLoading {
        session: SessionId,
    },
    /// Emitted after `load_brain_session` returns `Ok` and history replay
    /// has been dispatched. Terminal state for a successful resume.
    SessionLoaded {
        session: SessionId,
    },
```

- [ ] **Step 5: Run the round-trip tests to verify they pass**

Run: `cargo test -p spur-acp --test milestone_events_serde_roundtrip`

Expected: all five tests PASS.

- [ ] **Step 6: Run the full spur-acp test suite to ensure nothing else broke**

Run: `cargo test -p spur-acp`

Expected: all tests pass. If any existing test does an exhaustive `match` on `SpurEventBody` without a catch-all, it will fail to compile — that file needs a `_ => {}` arm added (minimal, same commit).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs crates/spur-acp/tests/milestone_events_serde_roundtrip.rs
git commit -m "feat(spur-acp): add resume-pipeline milestone events"
```

---

### Task 2: Emit milestone events from the resume pipeline

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (the `InteractiveInput::ResumeSession` arm, around lines 1336–1425; and inside `retire_active_brain` at ~2140–2200)
- Test: `crates/spur-core/tests/session_milestone_events.rs` (CREATE)

**Context:** Emit at the five phase boundaries so `SessionDetailView` can derive `LoadState` from ground truth rather than inferred timing.

- [ ] **Step 1: Write the failing integration test**

Create `crates/spur-core/tests/session_milestone_events.rs`:

```rust
use spur_acp::domain::events::{SessionId, SpurEventBody};
use spur_core::orchestrator::InteractiveInput;

// Use the harness style from Task 1 of Tranche 1. If helpers exist,
// reuse. If not, construct a minimal orchestrator with a successful
// connect + successful load fake.

#[tokio::test]
async fn successful_resume_emits_milestones_in_order_with_faithful_session_ids() {
    let target = SessionId::from("milestone-target");

    let (orch, mut events) = build_orchestrator_with_successful_resume();

    orch.send_input(InteractiveInput::ResumeSession {
        session_id: target.clone(),
    })
    .await;

    // Collect milestone events (ignore other event noise) until we see
    // SessionLoaded or time out.
    let mut seen: Vec<&'static str> = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        let ev = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            events.recv(),
        )
        .await;
        let Ok(Ok(ev)) = ev else { continue };
        match &ev.body {
            SpurEventBody::SessionRetireStart { to, .. } => {
                assert_eq!(to, &target);
                seen.push("SessionRetireStart");
            }
            SpurEventBody::SessionRetireComplete { session } => {
                // Retire is for the OLD session; may not equal target.
                // We only check the variant was emitted.
                let _ = session;
                seen.push("SessionRetireComplete");
            }
            SpurEventBody::BrainConnecting { session, .. } => {
                assert_eq!(session, &target);
                seen.push("BrainConnecting");
            }
            SpurEventBody::SessionLoading { session } => {
                assert_eq!(session, &target);
                seen.push("SessionLoading");
            }
            SpurEventBody::SessionLoaded { session } => {
                assert_eq!(session, &target);
                seen.push("SessionLoaded");
                break;
            }
            _ => {}
        }
    }

    // Must appear in this relative order. We don't require RetireComplete
    // always appears (cold start has no active brain to retire), so we
    // only enforce: Retire* precedes Brain*, which precedes Session*, and
    // SessionLoaded is terminal.
    let idx = |name: &str| seen.iter().position(|s| *s == name);
    assert!(idx("SessionRetireStart").is_some(), "no SessionRetireStart; got {:?}", seen);
    assert!(idx("SessionLoaded").is_some(), "no SessionLoaded; got {:?}", seen);
    assert!(idx("SessionRetireStart") < idx("SessionLoaded"));
    if let (Some(b), Some(l)) = (idx("BrainConnecting"), idx("SessionLoading")) {
        assert!(b < l, "BrainConnecting must precede SessionLoading; got {:?}", seen);
    }
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p spur-core --test session_milestone_events -- --nocapture`

Expected: FAIL — no milestones are emitted yet. Test fails because `seen` is empty (or nearly so).

- [ ] **Step 3: Emit `SessionRetireStart` / `SessionRetireComplete` inside `retire_active_brain`**

Read `crates/spur-core/src/orchestrator.rs` around line 2140–2200 (the body of `retire_active_brain`). Just BEFORE the existing `BrainRetired` emit (around line 2152), add:

```rust
        let from_session = brain.as_ref().map(|b| b.spur_session_id.clone());
        self.emit(SpurEvent::now(SpurEventBody::SessionRetireStart {
            from: from_session,
            // `to` must be the resume target. Thread it in — see Step 4.
            to: to_session.clone(),
        }));
```

This requires `retire_active_brain` to accept a `to_session: SessionId` parameter. Extend the function signature; update the call site at `orchestrator.rs:1337-1344` to pass the in-scope `session_id.clone()`. Other call sites of `retire_active_brain` that are NOT on the resume path should pass `SessionId::from("<no-target>")` or a sentinel — or, cleaner, change the parameter to `Option<SessionId>` and only emit `SessionRetireStart` when `Some`.

Prefer the `Option<SessionId>` approach:

```rust
async fn retire_active_brain(
    &self,
    brain: &mut Option<BrainSession>,
    agent_connection: &mut Option<(Arc<dyn AgentConnection>, String)>,
    scheduler: &mut crate::scheduler::BrainScheduler,
    overflow_continuations: &crate::continuation_bridge::OverflowBuf,
    reason: spur_acp::domain::events::BrainRetireReason,
    resume_target: Option<SessionId>,
) {
    let from_session = brain.as_ref().map(|b| b.spur_session_id.clone());
    if let Some(ref to) = resume_target {
        self.emit(SpurEvent::now(SpurEventBody::SessionRetireStart {
            from: from_session.clone(),
            to: to.clone(),
        }));
    }

    // ... existing retire body ...

    if let Some(ref from) = from_session {
        self.emit(SpurEvent::now(SpurEventBody::SessionRetireComplete {
            session: from.clone(),
        }));
    }
}
```

Update all call sites. For non-resume retirements (ClearSession, shutdown), pass `None`.

- [ ] **Step 4: Emit `BrainConnecting` before `connect_brain` on the resume path**

In `orchestrator.rs` around line 1348–1352, BEFORE the `connect_brain(...).await`, add:

```rust
                                let brain_name_hint = brain_override.as_deref().unwrap_or_default().to_string();
                                self.emit(SpurEvent::now(SpurEventBody::BrainConnecting {
                                    session: session_id.clone(),
                                    brain_name: brain_name_hint,
                                }));
                                match self
                                    .connect_brain(brain_override.as_deref(), permission_tx.clone())
                                    .await
```

Note: the brain name may not be fully resolved until `connect_brain` returns. The hint is acceptable — the UI only uses it for the progress label. If the hint is empty, the UI should fall back to a generic "Connecting…" string (see Task 5).

- [ ] **Step 5: Emit `SessionLoading` and `SessionLoaded` around `load_brain_session`**

In `orchestrator.rs` around line 1368, BEFORE the `load_brain_session(...).await`, add:

```rust
                        self.emit(SpurEvent::now(SpurEventBody::SessionLoading {
                            session: session_id.clone(),
                        }));
```

On the `Ok` branch (around line 1379–1425), AFTER the existing `TurnComplete` emit (currently at ~line 1423), add:

```rust
                                self.emit(SpurEvent::now(SpurEventBody::SessionLoaded {
                                    session: spur_id.clone(),
                                }));
```

`spur_id` is in scope at that point (bound at ~line 1380 from `session.spur_session_id.clone()`).

- [ ] **Step 6: Run the milestone test to verify it passes**

Run: `cargo test -p spur-core --test session_milestone_events -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Run the full spur-core suite**

Run: `cargo test -p spur-core`

Expected: PASS. If any test that constructs `retire_active_brain` arguments fails to compile, it is calling the old signature — update it with `None` for the new `resume_target` parameter.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs crates/spur-core/tests/session_milestone_events.rs
git commit -m "feat(spur-core): emit resume-pipeline milestone events at phase boundaries"
```

---

### Task 3: Delete `resuming: bool` from `SessionPickerView`

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs` (lines 40, 177, 443, 538, 890, 895, 990, 1066, 1077; plus a new unit test added to the existing `#[cfg(test)] mod tests` block)

**Context:** The picker becomes purely navigational. No pending state, no early-return guard, no spinner suffix. App-layer dispatcher (Task 4) handles both the command send and the navigation. The regression test lives next to the existing picker tests so it reuses their harness.

- [ ] **Step 1: Read the existing picker test harness**

Read the `#[cfg(test)] mod tests { ... }` block inside `crates/spur-tui/src/views/session_picker.rs`. Identify the helper that builds a populated picker with N sessions and the helper that synthesizes a `KeyEvent` for `Enter`. These are the tools you'll use in Step 2.

- [ ] **Step 2: Write a failing unit test inside the existing `mod tests` block**

Open `crates/spur-tui/src/views/session_picker.rs` and add this test inside the existing `#[cfg(test)] mod tests { ... }` block, matching the naming/harness conventions you observed in Step 1:

```rust
#[test]
fn enter_on_non_current_session_does_not_wedge_picker_into_pending_state() {
    // Populated with 3 sessions, no "current session" so every row is
    // a resume candidate. Use the existing test helper — replace
    // `populated_picker_with(3)` below with the actual helper name
    // discovered in Step 1.
    let mut picker = populated_picker_with(3);
    // Move cursor to row 1 (first non-[+ New] row).
    picker.on_key(key_event(KeyCode::Down));

    // First Enter: must return ResumeSession.
    let action1 = picker.on_key(key_event(KeyCode::Enter));
    assert!(
        matches!(action1, Some(Action::ResumeSession { .. })),
        "expected ResumeSession, got {:?}", action1
    );

    // Second Enter in the same frame: must NOT be silently eaten by a
    // pending-flag guard. Any of {ResumeSession, None-due-to-cursor,
    // Some(other)} is acceptable — what's forbidden is the pre-fix
    // behavior where `*resuming` caused an unconditional early `None`
    // return that swallowed all subsequent input.
    //
    // We assert positively: the picker still processes cursor motion.
    let before_cursor = picker.debug_cursor(); // add a test-only accessor if none exists
    picker.on_key(key_event(KeyCode::Down));
    let after_cursor = picker.debug_cursor();
    assert_ne!(
        before_cursor, after_cursor,
        "picker ignored input — resuming flag likely still present"
    );
}
```

If `debug_cursor()` doesn't exist, add it as a `#[cfg(test)] pub(super) fn debug_cursor(&self) -> usize` that returns the current cursor position from `PickerState::Populated`. If the harness helpers have different names, match them.

- [ ] **Step 3: Run to confirm the test fails**

Run: `cargo test -p spur-tui --lib session_picker::tests::enter_on_non_current_session_does_not_wedge_picker_into_pending_state`

Expected: FAIL. The pre-fix code sets `*resuming = true` on the first Enter, then the early-return at `session_picker.rs:895-897` drops the Down key, so `before_cursor == after_cursor` and the `assert_ne!` trips.

- [ ] **Step 4: Apply the picker edits — delete `resuming` everywhere**

In `crates/spur-tui/src/views/session_picker.rs`:

1. **Line 40**, remove the field from `PickerState::Populated`:
   ```rust
   // BEFORE
   Populated {
       agent: String,
       sessions: Vec<SessionInfo>,
       cursor: usize,
       resuming: bool,
       search_focused: bool,
       filter: String,
   },
   // AFTER — resuming removed
   Populated {
       agent: String,
       sessions: Vec<SessionInfo>,
       cursor: usize,
       search_focused: bool,
       filter: String,
   },
   ```

2. **Line 177** (inside the Populated constructor call): remove `resuming: false,`.

3. **Line 443** (render helper signature): remove the `resuming: bool` parameter and the corresponding call-site argument. If this is the only caller, delete the param outright. If there are multiple callers, delete the param at all of them.

4. **Line 538** (render-time spinner suffix): delete the `if is_selected && resuming { ... }` branch entirely. The picker no longer shows a row-local spinner.

5. **Line 890** (destructure): remove `resuming,` from the field list.

6. **Line 895-897** (early-return guard): delete:
   ```rust
                       if *resuming {
                           return None;
                       }
   ```

7. **Line 990-991** (Enter path): change:
   ```rust
                                           *resuming = true;
                                           Some(Action::ResumeSession { session_id: sid })
   ```
   to:
   ```rust
                                           Some(Action::ResumeSession { session_id: sid })
   ```

8. **Line 1066, 1077** (other destructure / read sites): remove the corresponding `resuming` references. Compile errors will point to each site; adjust to ignore the missing field.

If the compiler still reports `resuming` references after these edits, follow them and delete. The grep found exactly 7 usages; there should be no surprises.

- [ ] **Step 5: Run the regression test to verify it now passes**

Run: `cargo test -p spur-tui --lib session_picker::tests::enter_on_non_current_session_does_not_wedge_picker_into_pending_state`

Expected: PASS. `before_cursor != after_cursor` because the second Down key is no longer dropped.

- [ ] **Step 6: Run the full spur-tui suite — expect snapshot churn**

Run: `cargo test -p spur-tui`

Expected: the PickerView render tests may diff because the "Resuming…" suffix is gone. If any `insta` snapshot fails, run `cargo insta review` and accept the diff (it's the intended UX change).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs
# If snapshots were refreshed:
git add crates/spur-tui/src/views/snapshots/  # or wherever insta stores them
git commit -m "refactor(spur-tui): delete PickerState.resuming; picker holds no async state"
```

---

### Task 4: Dispatcher change — `Action::ResumeSession` also navigates

**Files:**
- Modify: `crates/spur-tui/src/app.rs:1562-1565`

**Context:** Today the app dispatches `Action::ResumeSession` only to the backend via `tx.try_send(UserInput::ResumeSession { ... })`. Add the immediate `NavigateTo(ViewId::SessionDetail(session_id))` so the picker dismisses in the same tick.

- [ ] **Step 1: Read the surrounding dispatcher code to match style**

Read `crates/spur-tui/src/app.rs:1550-1600`. Observe how other Actions chain side-effects (e.g. `NewSessionRequested` at ~line 1551-1558 already combines a channel send with navigation logic).

- [ ] **Step 2: Apply the fix**

Locate the `Action::ResumeSession` arm (line 1562-1565). Replace:

```rust
            Action::ResumeSession { session_id } => {
                let _ = tx.try_send(UserInput::ResumeSession { session_id });
            }
```

with:

```rust
            Action::ResumeSession { session_id } => {
                // Optimistic navigation: move to SessionDetail immediately
                // so the picker dismisses in the same tick (FP-6). The
                // SessionDetailView starts in LoadState::Retiring and
                // derives its label from incoming milestone events.
                let sid = spur_acp::SessionId(session_id.clone());
                self.navigate_to(ViewId::SessionDetail(sid));
                let _ = tx.try_send(UserInput::ResumeSession { session_id });
            }
```

Use whatever navigation method the app currently exposes (the example above assumes `self.navigate_to(...)`; if the codebase uses a different call — e.g. pushing an action back onto a queue — match that pattern).

NOTE: the `try_send` still ignores `Err(Full)`. That is a known follow-up (separate beads issue, out of scope for this tranche).

- [ ] **Step 3: Build to catch type errors**

Run: `cargo build -p spur-tui`

Expected: compiles. If `navigate_to` isn't the correct method name, inspect `app.rs` for how other Actions invoke view transitions and match that.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): app dispatches ResumeSession to both backend and navigation"
```

---

### Task 5: `SessionDetailView::LoadState` — event-derived render state

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs` (state struct, `handle_spur_event`, and render)
- Test: `crates/spur-tui/tests/session_detail_load_state.rs` (CREATE)

**Context:** `SessionDetailView` must accept an unloaded session as a first-class initial state. Introduce a `LoadState` projected from the newest milestone event whose `session` matches the view's session id. `BrainError` matching this session transitions to `Failed`. Events for other sessions are ignored.

- [ ] **Step 1: Inspect `SessionDetailView` state and event-handler structure**

Read `crates/spur-tui/src/views/session_detail.rs` around the struct definition and `handle_spur_event` (around line 1599 based on earlier grep). Understand how the view currently derives its label/header — this is where `LoadState` is rendered.

- [ ] **Step 2: Write the failing tests**

Create `crates/spur-tui/tests/session_detail_load_state.rs`:

```rust
use spur_acp::domain::events::{SessionId, SpurEvent, SpurEventBody};
use spur_tui::views::session_detail::{LoadState, SessionDetailView};

fn ev(body: SpurEventBody) -> SpurEvent {
    SpurEvent::now(body)
}

#[test]
fn initial_load_state_is_retiring() {
    let view = SessionDetailView::for_session(SessionId::from("s"));
    assert!(matches!(view.load_state(), LoadState::Retiring));
}

#[test]
fn brain_connecting_for_matching_session_transitions_to_connecting() {
    let mut view = SessionDetailView::for_session(SessionId::from("s"));
    view.handle_spur_event(&ev(SpurEventBody::BrainConnecting {
        session: SessionId::from("s"),
        brain_name: "claude-code".into(),
    }));
    assert!(matches!(view.load_state(), LoadState::Connecting { .. }));
}

#[test]
fn session_loading_transitions_to_loading() {
    let mut view = SessionDetailView::for_session(SessionId::from("s"));
    view.handle_spur_event(&ev(SpurEventBody::SessionLoading {
        session: SessionId::from("s"),
    }));
    assert!(matches!(view.load_state(), LoadState::Loading));
}

#[test]
fn session_loaded_transitions_to_ready() {
    let mut view = SessionDetailView::for_session(SessionId::from("s"));
    view.handle_spur_event(&ev(SpurEventBody::SessionLoaded {
        session: SessionId::from("s"),
    }));
    assert!(matches!(view.load_state(), LoadState::Ready));
}

#[test]
fn brain_error_for_matching_session_transitions_to_failed() {
    let mut view = SessionDetailView::for_session(SessionId::from("s"));
    view.handle_spur_event(&ev(SpurEventBody::BrainError {
        session: SessionId::from("s"),
        message: "boom".into(),
    }));
    match view.load_state() {
        LoadState::Failed { message } => assert_eq!(message, "boom"),
        other => panic!("expected Failed, got {:?}", other),
    }
}

#[test]
fn brain_error_for_different_session_is_ignored() {
    let mut view = SessionDetailView::for_session(SessionId::from("s"));
    view.handle_spur_event(&ev(SpurEventBody::BrainError {
        session: SessionId::from("other"),
        message: "boom".into(),
    }));
    // Still initial state — did not transition.
    assert!(matches!(view.load_state(), LoadState::Retiring));
}
```

If `SessionDetailView::for_session` / `.load_state()` methods don't exist yet, the tests won't compile. That IS the failing state.

- [ ] **Step 3: Run to confirm it fails to compile**

Run: `cargo test -p spur-tui --test session_detail_load_state`

Expected: COMPILE ERROR — `LoadState` and `for_session` don't exist.

- [ ] **Step 4: Add `LoadState` enum and projection to `SessionDetailView`**

Edit `crates/spur-tui/src/views/session_detail.rs`:

1. Introduce the enum near the top of the file, next to existing view-local type definitions:

```rust
/// Derived render state for a session the user has navigated to but
/// whose resume pipeline may not yet be complete. Each variant is a
/// projection of the most recent milestone event received for this
/// view's session id (FP-2, FP-4).
#[derive(Debug, Clone)]
pub enum LoadState {
    /// Default initial state when SessionDetail is entered via
    /// optimistic navigation from the picker.
    Retiring,
    Connecting { brain_name: String },
    Loading,
    Ready,
    Failed { message: String },
}
```

2. Add a `load_state: LoadState` field to `SessionDetailView`'s struct. Default in `Default` impl or `new()`: `LoadState::Retiring`.

3. Add a public constructor and accessor for testing:

```rust
impl SessionDetailView {
    pub fn for_session(session_id: SessionId) -> Self {
        let mut v = Self::default();
        v.session_id = Some(session_id);
        v.load_state = LoadState::Retiring;
        v
    }

    pub fn load_state(&self) -> &LoadState {
        &self.load_state
    }
}
```

Adjust the field paths to the real struct layout — `session_id` may already exist under a different name.

4. In `handle_spur_event`, BEFORE the existing `BrainError` arm (session_detail.rs:1599), add milestone-event handling guarded by session-id match:

```rust
        let my_sid = match self.session_id.as_ref() {
            Some(sid) => sid.clone(),
            None => return,
        };

        match &event.body {
            SpurEventBody::BrainConnecting { session, brain_name } if *session == my_sid => {
                self.load_state = LoadState::Connecting { brain_name: brain_name.clone() };
            }
            SpurEventBody::SessionLoading { session } if *session == my_sid => {
                self.load_state = LoadState::Loading;
            }
            SpurEventBody::SessionLoaded { session } if *session == my_sid => {
                self.load_state = LoadState::Ready;
            }
            SpurEventBody::BrainError { session, message } if *session == my_sid => {
                self.load_state = LoadState::Failed { message: message.clone() };
                // Fall through: the existing BrainError handler below may
                // also want to update brain_status — leave it intact.
            }
            _ => {}
        }

        // ... existing handle_spur_event body continues ...
```

If `handle_spur_event` already has a top-level `match &event.body`, integrate the new arms into it rather than adding a second match. Use the session-id guard via `if *session == my_sid`.

5. In the render method, branch on `LoadState` to choose the header/body content. For `Retiring | Connecting | Loading`, render a centered progress label (string varies per variant). For `Ready`, render the existing SessionDetail UI. For `Failed`, render an error panel with the message and a hint "press Esc to return to the picker."

A minimal renderer for the non-Ready states (specifics depend on the existing Ratatui layout):

```rust
match &self.load_state {
    LoadState::Retiring => render_centered_label(frame, area, "Retiring previous session…"),
    LoadState::Connecting { brain_name } => {
        let label = if brain_name.is_empty() {
            "Connecting to brain…".to_string()
        } else {
            format!("Connecting to {brain_name}…")
        };
        render_centered_label(frame, area, &label);
    }
    LoadState::Loading => render_centered_label(frame, area, "Loading session history…"),
    LoadState::Failed { message } => render_error_panel(frame, area, message),
    LoadState::Ready => { /* existing full render path */ }
}
```

`render_centered_label` and `render_error_panel` are small helpers in the same file (new or repurposed from existing primitives). Do not extract to a shared module; keep them inline.

- [ ] **Step 5: Run the LoadState tests to verify they pass**

Run: `cargo test -p spur-tui --test session_detail_load_state`

Expected: all six tests PASS.

- [ ] **Step 6: Run the full spur-tui suite — expect snapshot churn**

Run: `cargo test -p spur-tui`

Expected: SessionDetail render snapshots may diff for unloaded sessions because the view now renders "Retiring…" instead of whatever placeholder existed before. Review with `cargo insta review` and accept the intended diffs.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs crates/spur-tui/tests/session_detail_load_state.rs
# Plus any accepted snapshot updates.
git commit -m "feat(spur-tui): SessionDetail derives LoadState from milestone events"
```

---

### Task 6: Add `session_picker` to `handle_spur_event` dispatch

**Files:**
- Modify: `crates/spur-tui/src/app.rs:1249-1258`
- Modify: `crates/spur-tui/src/views/session_picker.rs` (give `handle_spur_event` a minimal list-refresh body)

**Context:** Today the picker is excluded from the event dispatch. It holds no async state (Task 3 removed `resuming`), so the handler is minimal — react to list-refresh events, ignore the rest.

- [ ] **Step 1: Inspect the current dispatch block**

Read `crates/spur-tui/src/app.rs:1249-1258`. Note the view-by-view dispatch pattern.

- [ ] **Step 2: Add `session_picker` to the dispatch**

Add (matching the existing style — adjust method name if not `handle_spur_event`):

```rust
            self.session_picker.handle_spur_event(&spur_event);
```

Place it alongside `self.session_detail.handle_spur_event(&spur_event);` etc.

- [ ] **Step 3: Fill in `SessionPickerView::handle_spur_event`**

Locate the existing stub at `session_picker.rs:1052-1055`. Replace the no-op body with a minimal list-refresh handler:

```rust
    pub fn handle_spur_event(&mut self, event: &spur_acp::SpurEvent) {
        match &event.body {
            // A new session started or an existing one was retired — the
            // list is stale; dispatch a RefreshSessions next tick.
            spur_acp::SpurEventBody::BrainSpawned { .. }
            | spur_acp::SpurEventBody::BrainRetired { .. } => {
                self.request_refresh = true;
            }
            _ => {}
        }
    }
```

Add a `request_refresh: bool` field to `SessionPickerView` (default `false`), and in `on_key` or the view's tick entry, convert `request_refresh` into `Some(Action::RefreshSessions)` when true (clearing the flag).

If that plumbing is excessive for this change's scope, skip the field and leave the handler as `_ => {}` — the picker still receives events; no state updates are required for Tranche 2's correctness. The refresh-on-event behavior is a nice-to-have, not the bug fix.

- [ ] **Step 4: Build + quick test run**

Run: `cargo build -p spur-tui && cargo test -p spur-tui`

Expected: compiles, tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/src/views/session_picker.rs
git commit -m "feat(spur-tui): route SpurEvents to session_picker"
```

---

### Task 7: Integration sanity + full workspace verification

- [ ] **Step 1: Run the full workspace build**

Run: `cargo build --workspace`

Expected: clean build.

- [ ] **Step 2: Run the full workspace test suite**

Run: `cargo test --workspace --no-fail-fast`

Expected: all tests pass.

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`

Expected: zero warnings.

- [ ] **Step 4: Manual smoke test**

Launch the TUI: `cargo run --bin spur -- --interactive` (or whatever command the project uses).

1. Start a session; exchange one message.
2. Open the session picker.
3. Select a different session.
4. Verify: picker dismisses immediately; SessionDetail shows "Retiring previous session…" → "Connecting…" → "Loading…" → populated. No stuck spinner on the picker.
5. Simulate a failure path if possible (e.g. kill the brain binary mid-resume): SessionDetail should show `Failed` with the error; Esc returns to picker; picker is not stuck.

Note observations in the PR description.

- [ ] **Step 5: Final commit (if any fixups were made during smoke testing)**

```bash
git add -u
git commit -m "chore: fixups from tranche-2 smoke test"
```

---

## Self-review — completed during authoring

- **Spec coverage:** All five Tranche 2 changes in the spec are covered — (3) delete `resuming`, (4) `LoadState` on SessionDetail, (5) picker on dispatch, (6) milestone events. The optional picker event-handler body is in Task 6.
- **Placeholder scan:** No TBD/TODO. The `todo!()` in Task 3 Step 1's test skeleton is explicitly flagged as a deliberate failure marker, resolved in Step 3 of that task. All code blocks show the actual change.
- **Type consistency:** `LoadState` variants used consistently in tests (Task 5) and emit sites. `SessionId`, `SpurEventBody`, `Action`, `ViewId::SessionDetail`, `SessionPickerView`, `SessionDetailView` names match live code.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-24-session-resume-tranche-2-optimistic-nav.md`. Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks.
2. **Inline Execution** — execute tasks in this session with checkpoints.

Which approach?
