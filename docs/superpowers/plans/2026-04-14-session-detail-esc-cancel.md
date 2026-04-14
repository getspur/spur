# Session Detail — Esc to Cancel In-Flight Stream: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user halt an in-flight agent response by pressing `Esc` inside `SessionDetailView`, routing the halt through the existing ACP `session/cancel` infrastructure.

**Architecture:** New `Action::CancelStream` → `UserInput::CancelStream` → `InteractiveInput::CancelStream`, matched by the orchestrator's streaming `select!` loop as a new arm that calls `b.connection.cancel(&b.acp_session_id)` and arms the existing 5s force-timeout (no follow-on message is queued, unlike the `!…` interrupt path). The view tracks two local bools (`stream_in_flight`, `cancelling_in_flight`) plus a `cancel_mode` populated from `AgentSessionReady`, so Esc handling is gated on live-stream state and feedback text is transport-aware.

**Tech Stack:** Rust (workspace: `spur-acp`, `spur-core`, `spur-tui`), Tokio, `async-trait`, `agent-client-protocol` SDK, Ratatui, crossterm.

**Companion spec:** `docs/superpowers/specs/2026-04-14-session-detail-esc-cancel-design.md`

---

## File map

- **Create:** *(none — all changes land in existing files)*
- **Modify:**
  - `crates/spur-acp/src/types.rs` — add `CancelMode` enum alongside `TransportKind`.
  - `crates/spur-acp/src/lib.rs` — re-export `CancelMode`.
  - `crates/spur-acp/src/domain/events.rs` — add `cancel_mode: CancelMode` field to `SpurEventBody::AgentSessionReady`.
  - `crates/spur-core/src/orchestrator.rs` — `cancel_mode_for(TransportKind) -> CancelMode` helper; populate new field at both `AgentSessionReady` emit sites (~:1187, ~:1331); new `InteractiveInput::CancelStream { session }` variant; inner `select!` arm inside the streaming loop (~:663–742); outer-loop drop arm (no-op log).
  - `crates/spur-tui/src/action.rs` — `Action::CancelStream { session: SessionId }`.
  - `crates/spur-tui/src/app.rs` — `UserInput::CancelStream { session: SessionId }`; `process_action` dispatcher maps `Action::CancelStream` → `UserInput::CancelStream`.
  - `crates/spur-cli/src/main.rs` — converter arm `spur_tui::UserInput::CancelStream { session } → spur_core::InteractiveInput::CancelStream { session }`.
  - `crates/spur-tui/src/views/session_detail.rs` — three new state fields; `handle_spur_event` updates for `AgentMessageChunk`/`AgentThoughtChunk`/`TurnComplete`/`AgentSessionReady`; `handle_key_inner` top-priority Esc branch; helper `push_cancel_note`; status-label override when `cancelling_in_flight`.
  - `crates/spur-tui/src/components/status_bar.rs` — new `stream_in_flight: bool` field on `StatusBarProps`; render `[Esc]stop` hint when true (and the existing `[Esc]back` hint when false).

---

## Task 1: Add `CancelMode` type in `spur-acp`

**Files:**
- Modify: `crates/spur-acp/src/types.rs`
- Modify: `crates/spur-acp/src/lib.rs`
- Test: `crates/spur-acp/src/types.rs` (doc-style unit test appended to the file)

- [ ] **Step 1: Append a unit test at the bottom of `crates/spur-acp/src/types.rs`**

At the end of the file, under `#[cfg(test)] mod tests { … }` (create the module if it does not already exist — `rg '#\[cfg\(test\)\]' crates/spur-acp/src/types.rs` first):

```rust
#[cfg(test)]
mod cancel_mode_tests {
    use super::CancelMode;

    #[test]
    fn cancel_mode_is_copy_and_equatable() {
        let a = CancelMode::AcpSoft;
        let b = a; // Copy
        assert_eq!(a, b);
        assert_ne!(CancelMode::AcpSoft, CancelMode::ProcessKill);
    }
}
```

- [ ] **Step 2: Run the test, expect compile error "CancelMode not found"**

Run: `cargo test -p spur-acp cancel_mode_is_copy_and_equatable --no-run`
Expected: compile error `error[E0412]: cannot find type 'CancelMode' in module 'super'`.

- [ ] **Step 3: Add the `CancelMode` enum immediately after `TransportKind` in `types.rs`**

Insert after the closing `}` of `TransportKind` (around line 120):

```rust
// ─── Cancel Mode ───────────────────────────────────────────────────────

/// How `AgentConnection::cancel` behaves for a given transport.
///
/// `AcpSoft` is a true ACP `session/cancel` notification — the agent
/// continues to exist and the session remains addressable.
///
/// `ProcessKill` tears down the underlying subprocess (SIGTERM for
/// `Stdio`, SIGKILL for `CliWrap`/`StreamJson`). The next interaction
/// with that agent requires respawning.
///
/// Used by the TUI to show transport-aware cancel feedback. See
/// `docs/superpowers/specs/2026-04-14-session-detail-esc-cancel-design.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelMode {
    /// ACP `session/cancel` notification; process stays alive.
    AcpSoft,
    /// Transport kills the subprocess on cancel.
    ProcessKill,
}
```

- [ ] **Step 4: Re-export `CancelMode` from the crate root**

Open `crates/spur-acp/src/lib.rs`. Find the line exporting `TransportKind` from `types` (`rg 'TransportKind' crates/spur-acp/src/lib.rs`). Add `CancelMode` to the same `pub use` list. Example (adapt to the actual surrounding lines):

```rust
pub use types::{
    AgentHealth, AgentRole, CancelMode, CostTier, PermissionRequest,
    PermissionResponse, TransportKind,
};
```

(Alphabetize as existing code does; exact neighbors depend on current file state — `rg -n 'pub use types::' crates/spur-acp/src/lib.rs` to see the canonical list.)

- [ ] **Step 5: Run the test, expect pass**

Run: `cargo test -p spur-acp cancel_mode_is_copy_and_equatable`
Expected: `test cancel_mode_tests::cancel_mode_is_copy_and_equatable ... ok`.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/types.rs crates/spur-acp/src/lib.rs
git commit -m "feat(spur-acp): add CancelMode enum for transport-aware cancel feedback"
```

---

## Task 2: Add `cancel_mode` field to `SpurEventBody::AgentSessionReady`

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`
- Test: `crates/spur-acp/src/domain/events.rs` (append test)

This is a compile-driven task: adding a required field forces every emitter and matcher to update. We deliberately do the field addition first, then fix call sites in Task 3.

- [ ] **Step 1: Add a test asserting the field exists and round-trips through construction**

Append at the bottom of `crates/spur-acp/src/domain/events.rs`, inside the existing `#[cfg(test)] mod tests { … }` block (create the module if absent):

```rust
#[cfg(test)]
mod cancel_mode_field_tests {
    use super::{SpurEvent, SpurEventBody};
    use crate::{CancelMode, SessionId};

    #[test]
    fn agent_session_ready_carries_cancel_mode() {
        let ev = SpurEvent::now(SpurEventBody::AgentSessionReady {
            session: SessionId("s".to_string()),
            acp_session_id: "acp-1".to_string(),
            brain: "kiro".to_string(),
            resumed: false,
            cancel_mode: CancelMode::AcpSoft,
        });
        match ev.body {
            SpurEventBody::AgentSessionReady { cancel_mode, .. } => {
                assert_eq!(cancel_mode, CancelMode::AcpSoft);
            }
            _ => panic!("wrong variant"),
        }
    }
}
```

- [ ] **Step 2: Run the test, expect compile error "missing field `cancel_mode`"**

Run: `cargo test -p spur-acp agent_session_ready_carries_cancel_mode --no-run`
Expected: `error[E0063]: missing field 'cancel_mode' in initializer of 'SpurEventBody'`.

- [ ] **Step 3: Add the field to the variant**

Open `crates/spur-acp/src/domain/events.rs`, find the `AgentSessionReady` variant (currently around line 109), and add the field just before the closing `}`:

```rust
AgentSessionReady {
    session: SessionId,
    acp_session_id: String,
    brain: String,
    resumed: bool,
    /// How `session/cancel` is implemented for this session's transport.
    /// The TUI uses this to render transport-aware cancel feedback.
    cancel_mode: crate::CancelMode,
},
```

- [ ] **Step 4: Run the test (expect more compile errors, from other callers)**

Run: `cargo test -p spur-acp agent_session_ready_carries_cancel_mode --no-run`
Expected: the crate's own test compiles, **but** the workspace has downstream compile errors in `spur-core/src/orchestrator.rs` and `spur-tui/src/app.rs`/`views/session_detail.rs`. That is expected and fixed in Tasks 3 and 8. Don't proceed to `cargo test` on the full workspace yet — run only the spur-acp test to keep the loop tight.

Run: `cargo test -p spur-acp agent_session_ready_carries_cancel_mode`
Expected: `test ... ok` (spur-acp alone builds fine).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs
git commit -m "feat(spur-acp): add cancel_mode field to AgentSessionReady"
```

Downstream workspace will fail to compile until Task 3. That's intentional — the task boundary is natural because the compile errors are the checklist of callers to update.

---

## Task 3: Populate `cancel_mode` at orchestrator's two `AgentSessionReady` emit sites

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`
- Test: `crates/spur-core/src/orchestrator.rs` (append a small unit test for the helper)

- [ ] **Step 1: Append a test for the new helper**

At the end of `crates/spur-core/src/orchestrator.rs`, inside the existing `#[cfg(test)] mod tests { … }` block (there is one — verify with `rg -n '#\[cfg\(test\)\]' crates/spur-core/src/orchestrator.rs | head`):

```rust
#[cfg(test)]
mod cancel_mode_helper_tests {
    use super::cancel_mode_for;
    use spur_acp::{CancelMode, types::TransportKind};

    #[test]
    fn acp_transport_is_acp_soft() {
        assert_eq!(cancel_mode_for(TransportKind::Acp), CancelMode::AcpSoft);
    }

    #[test]
    fn subprocess_transports_are_process_kill() {
        assert_eq!(cancel_mode_for(TransportKind::Stdio), CancelMode::ProcessKill);
        assert_eq!(cancel_mode_for(TransportKind::CliWrap), CancelMode::ProcessKill);
        assert_eq!(cancel_mode_for(TransportKind::StreamJson), CancelMode::ProcessKill);
    }
}
```

- [ ] **Step 2: Run the tests, expect compile error "function not found"**

Run: `cargo test -p spur-core cancel_mode_helper_tests --no-run`
Expected: `error[E0425]: cannot find function 'cancel_mode_for'`.

- [ ] **Step 3: Add the helper near `build_connection_from_transport` in `orchestrator.rs`**

Immediately above `fn build_connection_from_transport` (currently ~line 2362), add:

```rust
/// Map a transport kind to its `CancelMode`. Single source of truth used
/// by `AgentSessionReady` emitters so the TUI can render transport-aware
/// cancel feedback without re-inspecting `AgentConfig`.
pub(crate) fn cancel_mode_for(transport: spur_acp::types::TransportKind) -> spur_acp::CancelMode {
    use spur_acp::types::TransportKind;
    match transport {
        TransportKind::Acp => spur_acp::CancelMode::AcpSoft,
        TransportKind::Stdio
        | TransportKind::CliWrap
        | TransportKind::StreamJson => spur_acp::CancelMode::ProcessKill,
    }
}
```

- [ ] **Step 4: Update the first `AgentSessionReady` emit site (`create_brain_session`, ~:1187)**

Find the emit at ~line 1187 (search for `AgentSessionReady` in the file; the first hit). Add the `cancel_mode` field using the helper. `brain_cfg` is already in scope a few lines above (look for `let brain_cfg = self.registry.get(&brain_name)` around line 1139):

```rust
self.emit(SpurEvent::now(SpurEventBody::AgentSessionReady {
    session: session_id.clone(),
    acp_session_id: session_response.session_id.to_string(),
    brain: brain_name.clone(),
    resumed: false,
    cancel_mode: cancel_mode_for(brain_cfg.transport),
}));
```

- [ ] **Step 5: Update the second `AgentSessionReady` emit site (`load_brain_session`, ~:1331)**

Find the second emit (~line 1331, inside the `load_brain_session` path). The `brain_cfg` is available in that function too (there's a `self.registry.get(&brain_name)` lookup earlier — `rg -n 'brain_cfg' crates/spur-core/src/orchestrator.rs` to confirm). Update similarly:

```rust
self.emit(SpurEvent::now(SpurEventBody::AgentSessionReady {
    session: session_id.clone(),
    acp_session_id: final_acp_session_id.clone(),
    brain: brain_name.clone(),
    resumed,
    cancel_mode: cancel_mode_for(brain_cfg.transport),
}));
```

If `brain_cfg` is not in scope at that emit site, hoist a new binding at the top of the function: `let brain_transport = self.registry.get(&brain_name).map(|c| c.transport).unwrap_or(spur_acp::types::TransportKind::Acp);` and use `cancel_mode_for(brain_transport)`. Prefer the in-scope binding if present.

- [ ] **Step 6: Run the helper tests, expect pass**

Run: `cargo test -p spur-core cancel_mode_helper_tests`
Expected: both tests pass. (The orchestrator body compiles again — the `spur-acp` missing-field errors from Task 2 are resolved.)

- [ ] **Step 7: Run the full spur-core test suite to catch other breakage**

Run: `cargo build -p spur-core`
Expected: clean build.
Run: `cargo test -p spur-core --lib`
Expected: all existing tests pass (no regressions from the new field — the compiler covered every matcher).

- [ ] **Step 8: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): populate cancel_mode on AgentSessionReady emissions"
```

---

## Task 4: Add `InteractiveInput::CancelStream` variant + outer-loop drop arm

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`
- Test: `crates/spur-core/src/orchestrator.rs` (inline)

This task adds the orchestrator-side input variant and its *idle-state* handler (no-op with debug log). The inner streaming arm comes in Task 5.

- [ ] **Step 1: Add a test asserting the variant exists and can be constructed**

In the same `#[cfg(test)] mod tests` block as Task 3's tests:

```rust
#[cfg(test)]
mod cancel_stream_variant_tests {
    use super::InteractiveInput;
    use spur_acp::SessionId;

    #[test]
    fn cancel_stream_variant_constructs() {
        let _ = InteractiveInput::CancelStream {
            session: SessionId("s".to_string()),
        };
    }
}
```

- [ ] **Step 2: Run the test, expect compile error "no variant named `CancelStream`"**

Run: `cargo test -p spur-core cancel_stream_variant_constructs --no-run`
Expected: `error[E0599]: no variant or associated item named 'CancelStream' found for enum 'InteractiveInput'`.

- [ ] **Step 3: Add the variant to `InteractiveInput`**

At `crates/spur-core/src/orchestrator.rs:68`, append the new variant after `SubmitReview`:

```rust
pub enum InteractiveInput {
    Message { blocks: Vec<ContentBlock>, interrupt: bool },
    NewSessionWithMessage { blocks: Vec<ContentBlock>, interrupt: bool },
    ListSessions,
    ResumeSession { session_id: String },
    SetSessionMode { mode_id: String },
    KiroExecute { session: SessionId, command: String, args: serde_json::Value },
    SubmitReview { executor_id: String, attempt_n: u32, decision: spur_acp::ReviewDecision },
    /// Halt the currently streaming prompt (if any) via `AgentConnection::cancel`.
    /// When received inside the streaming `select!`, calls `cancel()` and arms
    /// the 5s force-timeout. When received outside the streaming loop (no
    /// active turn), dropped with a debug log (the view guards against emitting
    /// this unless a stream is in-flight, but a TurnComplete-vs-Esc race can
    /// still produce a stray one).
    CancelStream { session: SessionId },
}
```

(Preserve any doc comments that currently exist on the other variants — the snippet above is the *shape*; keep existing per-variant comments as they are.)

- [ ] **Step 4: Add the outer-loop drop arm**

The main `run_interactive` loop has a `match` on `user_input_rx.recv()` outside the streaming path. Find the existing arms (e.g., search for `InteractiveInput::SetSessionMode` around orchestrator.rs:580). Add:

```rust
InteractiveInput::CancelStream { session } => {
    tracing::debug!(
        session = %session,
        "CancelStream received outside active turn; dropping (no stream to cancel)"
    );
}
```

Place it among the outer-loop arms — somewhere alphabetically or logically near `SetSessionMode`. Exact match-arm position doesn't matter; order by existing convention in the file.

- [ ] **Step 5: Run the test, expect pass**

Run: `cargo test -p spur-core cancel_stream_variant_constructs`
Expected: test passes; full build is clean.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): add InteractiveInput::CancelStream variant + idle drop"
```

---

## Task 5: Add streaming `select!` arm to call `connection.cancel` on `CancelStream`

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`
- Test: inline (unit test of a factored helper)

This task wires the actual behavior: mid-stream `CancelStream` → `b.connection.cancel(...)` + 5s deadline, without queuing a follow-on message. We factor the deadline-arming into a pure helper so it is directly unit-testable; the `.cancel()` call itself is verified via the manual smoke test in Task 11.

- [ ] **Step 1: Add a unit test for the deadline-arming helper**

In the `#[cfg(test)] mod tests { … }` block at the bottom of `crates/spur-core/src/orchestrator.rs`, alongside the Task 4 variant test:

```rust
#[cfg(test)]
mod cancel_deadline_arm_tests {
    use super::arm_cancel_deadline;

    #[tokio::test]
    async fn arm_cancel_deadline_sets_5s_from_now() {
        let mut deadline = None;
        let before = tokio::time::Instant::now();
        arm_cancel_deadline(&mut deadline);
        let set = deadline.expect("arm_cancel_deadline must populate Some(deadline)");
        let delta = set.saturating_duration_since(before);
        assert!(
            delta >= std::time::Duration::from_millis(4_900)
                && delta <= std::time::Duration::from_millis(5_100),
            "expected ~5s deadline, got {delta:?}"
        );
    }

    #[tokio::test]
    async fn arm_cancel_deadline_overwrites_existing() {
        let old = tokio::time::Instant::now() - std::time::Duration::from_secs(60);
        let mut deadline = Some(old);
        arm_cancel_deadline(&mut deadline);
        assert!(deadline.unwrap() > old + std::time::Duration::from_secs(1));
    }
}
```

- [ ] **Step 2: Run the test, expect compile error "function not found"**

Run: `cargo test -p spur-core cancel_deadline_arm_tests --no-run`
Expected: `error[E0425]: cannot find function 'arm_cancel_deadline'`.

- [ ] **Step 3: Add the helper near the streaming loop in `orchestrator.rs`**

Above the function that contains the streaming loop (where `cancel_deadline` is declared, ~line 661), add:

```rust
/// Arm the 5-second force-end deadline used by the streaming `select!`.
/// Factored out so both the `Message { interrupt: true }` arm and the
/// new `CancelStream` arm set the deadline identically and so it is
/// directly unit-testable without a full mock orchestrator.
pub(crate) fn arm_cancel_deadline(
    deadline: &mut Option<tokio::time::Instant>,
) {
    *deadline = Some(
        tokio::time::Instant::now() + std::time::Duration::from_secs(5),
    );
}
```

- [ ] **Step 4: Run the helper tests, expect pass**

Run: `cargo test -p spur-core cancel_deadline_arm_tests`
Expected: both tests pass.

- [ ] **Step 5: Refactor the existing `interrupt: true` arm to use the helper**

Find the existing arm at ~:710–715:

```rust
if msg_interrupt {
    let _ = b.connection.cancel(&b.acp_session_id).await;
    cancel_deadline = Some(
        tokio::time::Instant::now()
            + std::time::Duration::from_secs(5),
    );
}
```

Replace the `cancel_deadline = Some(...)` line with `arm_cancel_deadline(&mut cancel_deadline);`. The `connection.cancel` call stays as-is.

- [ ] **Step 6: Add the new `select!` arm inside the streaming loop**

Open `crates/spur-core/src/orchestrator.rs` at ~line 707 (the `Some(queued) = user_input_rx.recv()` arm of the streaming `select!`). After Step 5's refactor, the `Message` arm uses `arm_cancel_deadline`. Add a new arm for `CancelStream` **before** the catch-all:

```rust
Some(queued) = user_input_rx.recv() => {
    match queued {
        InteractiveInput::Message { blocks: msg_blocks, interrupt: msg_interrupt } => {
            if msg_interrupt {
                let _ = b.connection.cancel(&b.acp_session_id).await;
                arm_cancel_deadline(&mut cancel_deadline);
            }
            let queued_blocks = if msg_interrupt {
                strip_bang_prefix(msg_blocks)
            } else {
                msg_blocks
            };
            pending_messages.push_back(InteractiveInput::Message {
                blocks: queued_blocks,
                interrupt: false,
            });
        }
        InteractiveInput::CancelStream { session } => {
            // Pure halt: cancel the stream without queuing any follow-on.
            // The `session` field is informational — the streaming loop
            // runs per-brain-session, so there is exactly one active stream.
            let _ = session;
            let _ = b.connection.cancel(&b.acp_session_id).await;
            arm_cancel_deadline(&mut cancel_deadline);
        }
        other => {
            pending_messages.push_back(other);
        }
    }
}
```

- [ ] **Step 7: Build and run all spur-core tests**

Run: `cargo build -p spur-core`
Expected: clean build.
Run: `cargo test -p spur-core`
Expected: all existing tests pass; new tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): streaming select! arm for InteractiveInput::CancelStream"
```

---

## Task 6: Add `Action::CancelStream` + `UserInput::CancelStream` + CLI converter

**Files:**
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-cli/src/main.rs`

- [ ] **Step 1: Add the `Action::CancelStream` variant**

In `crates/spur-tui/src/action.rs`, inside the `pub enum Action { … }` block, append:

```rust
/// Halt an in-flight agent stream via ACP cancel. Emitted by
/// `SessionDetailView` when the user presses `Esc` and a stream is live.
/// The orchestrator matches the corresponding `UserInput::CancelStream`
/// inside its streaming `select!` loop and calls `AgentConnection::cancel`.
CancelStream { session: SessionId },
```

- [ ] **Step 2: Add the `UserInput::CancelStream` variant**

In `crates/spur-tui/src/app.rs`, inside `pub enum UserInput { … }` (around line 28), append:

```rust
/// Halt the in-flight agent stream on the given session. Maps 1:1 to
/// `spur_core::InteractiveInput::CancelStream` via `spur-cli`.
CancelStream { session: SessionId },
```

- [ ] **Step 3: Add the action → UserInput dispatcher arm in `process_action`**

In `crates/spur-tui/src/app.rs`, find `fn process_action(&mut self, action: Action)` (around line 468). Add a new arm after `Action::KiroExecute { … }`:

```rust
Action::CancelStream { session } => {
    tracing::debug!(session = %session.0, "dispatching CancelStream to orchestrator");
    if let Some(ref tx) = self.user_input_tx {
        let _ = tx.try_send(UserInput::CancelStream { session });
    }
}
```

- [ ] **Step 4: Add the converter arm in `spur-cli`**

In `crates/spur-cli/src/main.rs` around line 417, inside the existing `match` that converts `spur_tui::UserInput` to `spur_core::InteractiveInput`, add an arm:

```rust
spur_tui::UserInput::CancelStream { session } => {
    spur_core::InteractiveInput::CancelStream { session }
}
```

- [ ] **Step 5: Build the full workspace**

Run: `cargo build --workspace`
Expected: clean build. (All three crates now compile; the cancel path is wired end-to-end.)

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/action.rs crates/spur-tui/src/app.rs crates/spur-cli/src/main.rs
git commit -m "feat(spur-tui,cli): Action/UserInput::CancelStream + dispatcher + converter"
```

---

## Task 7: Add state fields to `SessionDetailView`

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Test: inline, bottom of file

This task adds the three fields but no handlers yet — those come in Tasks 8 and 9. Ordering this separately keeps each diff small and reviewable.

- [ ] **Step 1: Add a test asserting the fields default correctly on a new view**

At the bottom of `crates/spur-tui/src/views/session_detail.rs`, inside a new `#[cfg(test)] mod cancel_state_tests { … }` block (outside the existing `invalidate_protocols_tests` module):

```rust
#[cfg(test)]
mod cancel_state_tests {
    use super::*;

    fn make_view() -> SessionDetailView {
        SessionDetailView::new(
            spur_acp::SessionId("s".to_string()),
            "claude".to_string(),
            "brain".to_string(),
            std::path::PathBuf::from("/tmp"),
        )
    }

    #[test]
    fn new_view_has_no_stream_in_flight() {
        let v = make_view();
        assert!(!v.stream_in_flight);
        assert!(!v.cancelling_in_flight);
        assert!(v.cancel_mode.is_none());
    }
}
```

- [ ] **Step 2: Run the test, expect compile errors for missing fields**

Run: `cargo test -p spur-tui cancel_state_tests --no-run`
Expected: `error[E0609]: no field 'stream_in_flight' on type 'SessionDetailView'`.

- [ ] **Step 3: Add the fields to `struct SessionDetailView`**

In the struct definition (around line 22–80), append inside the struct:

```rust
/// True from the first `AgentMessageChunk`/`AgentThoughtChunk` of a turn
/// until the matching `TurnComplete`. Used to gate `Esc`-to-cancel on
/// whether a stream is actually in flight, and to render the "Esc to
/// stop" status-bar hint.
pub(crate) stream_in_flight: bool,

/// True from the moment we dispatch `Action::CancelStream` until
/// `TurnComplete`. Overrides the streaming label with `cancelling…` and
/// prevents re-entrant cancel dispatches (the next `Esc` falls through
/// to existing handlers, e.g. NavigateBack).
pub(crate) cancelling_in_flight: bool,

/// How `AgentConnection::cancel` behaves for this session's transport.
/// Populated from `SpurEventBody::AgentSessionReady`. Used to select
/// transport-aware text for the cancel system note. `None` until
/// `AgentSessionReady` arrives; in that window, a generic fallback is
/// rendered.
pub(crate) cancel_mode: Option<spur_acp::CancelMode>,
```

- [ ] **Step 4: Initialize the fields in `fn new(…)`**

In `SessionDetailView::new`, add the three default values in the struct literal:

```rust
Self {
    // …existing fields…
    stream_in_flight: false,
    cancelling_in_flight: false,
    cancel_mode: None,
    // …existing fields…
}
```

- [ ] **Step 5: Run the test, expect pass**

Run: `cargo test -p spur-tui cancel_state_tests::new_view_has_no_stream_in_flight`
Expected: test passes.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-tui): add cancel/stream state fields to SessionDetailView"
```

---

## Task 8: Wire `handle_spur_event` transitions for stream flags & `cancel_mode`

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Test: inline

- [ ] **Step 1: Add tests for the four transitions**

Inside `mod cancel_state_tests` at the bottom of the file:

```rust
use spur_acp::{CancelMode, ContentBlock, ContentChunk, SessionId as AcpSessionId, SessionNotification, SessionUpdate, TextContent, SpurEvent, SpurEventBody};
// Adjust imports to whatever the crate actually re-exports.

fn agent_msg_chunk_event(session: &AcpSessionId) -> SpurEvent {
    let update = SessionUpdate::AgentMessageChunk(ContentChunk {
        content: ContentBlock::Text(TextContent { text: "hi".into(), annotations: None }),
    });
    let notification = SessionNotification {
        session_id: agent_client_protocol::SessionId::new(session.0.clone()),
        update,
        meta: None,
    };
    SpurEvent::now(SpurEventBody::AgentNotification {
        session: session.clone(),
        notification: Box::new(notification),
    })
}

fn turn_complete_event(session: &AcpSessionId) -> SpurEvent {
    SpurEvent::now(SpurEventBody::TurnComplete { session: session.clone() })
}

fn agent_session_ready_event(session: &AcpSessionId, mode: CancelMode) -> SpurEvent {
    SpurEvent::now(SpurEventBody::AgentSessionReady {
        session: session.clone(),
        acp_session_id: "acp-1".into(),
        brain: "claude".into(),
        resumed: false,
        cancel_mode: mode,
    })
}

#[test]
fn chunk_sets_stream_in_flight() {
    let mut v = make_view();
    let sid = v.session_id().clone();
    v.handle_spur_event(&agent_msg_chunk_event(&sid));
    assert!(v.stream_in_flight);
}

#[test]
fn turn_complete_clears_both_flags() {
    let mut v = make_view();
    let sid = v.session_id().clone();
    v.stream_in_flight = true;
    v.cancelling_in_flight = true;
    v.handle_spur_event(&turn_complete_event(&sid));
    assert!(!v.stream_in_flight);
    assert!(!v.cancelling_in_flight);
}

#[test]
fn agent_session_ready_populates_cancel_mode() {
    let mut v = make_view();
    let sid = v.session_id().clone();
    v.handle_spur_event(&agent_session_ready_event(&sid, CancelMode::AcpSoft));
    assert_eq!(v.cancel_mode, Some(CancelMode::AcpSoft));
}

#[test]
fn event_for_different_session_is_ignored() {
    let mut v = make_view();
    let other = AcpSessionId("other".to_string());
    v.handle_spur_event(&agent_msg_chunk_event(&other));
    assert!(!v.stream_in_flight);
}
```

- [ ] **Step 2: Run the tests, expect failures**

Run: `cargo test -p spur-tui cancel_state_tests --no-run`
Verify compile succeeds (the handler exists but doesn't touch the new fields yet).
Run: `cargo test -p spur-tui cancel_state_tests`
Expected: `chunk_sets_stream_in_flight`, `turn_complete_clears_both_flags`, and `agent_session_ready_populates_cancel_mode` fail; `event_for_different_session_is_ignored` passes (vacuously).

- [ ] **Step 3: Update the `AgentMessageChunk`/`AgentThoughtChunk` arms in `handle_spur_event`**

In `handle_spur_event`, inside the existing `SpurEventBody::AgentNotification` match on `notification.update`, update the two chunk arms (`AgentThoughtChunk` at ~:783 and `AgentMessageChunk` at ~:790). At the start of each, set the flag (before the existing extract-and-append logic):

```rust
spur_acp::SessionUpdate::AgentThoughtChunk(chunk) => {
    self.stream_in_flight = true;
    if let Some(text) = extract_text(chunk) {
        if !text.is_empty() {
            self.react_trace.append_think(text, Self::now_stamp());
        }
    }
}
spur_acp::SessionUpdate::AgentMessageChunk(chunk) => {
    self.stream_in_flight = true;
    // …existing body…
}
```

- [ ] **Step 4: Update the `TurnComplete` arm**

At ~:938 in `handle_spur_event` is the existing `SpurEventBody::TurnComplete { session }` branch, which already gates on `session.0 == self.session_id.0`. Inside that gate (before the `#[cfg(feature = "markdown")]` block), add:

```rust
if session.0 == self.session_id.0 {
    self.stream_in_flight = false;
    self.cancelling_in_flight = false;
    // …existing body…
}
```

- [ ] **Step 5: Update the `AgentSessionReady` arm**

At ~:995, the existing arm destructures `session`, `resumed`, and ignores the rest. Update it to also bind and store `cancel_mode`:

```rust
SpurEventBody::AgentSessionReady {
    session,
    resumed,
    cancel_mode,
    ..
} => {
    if session.0 != self.session_id.0 {
        return;
    }
    self.cancel_mode = Some(*cancel_mode);
    if *resumed {
        self.push_system_note("Resumed from prior conversation".to_string());
    }
}
```

- [ ] **Step 6: Run the tests, expect pass**

Run: `cargo test -p spur-tui cancel_state_tests`
Expected: all four tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-tui): wire stream_in_flight/cancel_mode via SpurEvent handlers"
```

---

## Task 9: Implement `Esc`-priority handler + transport-aware system note + label override

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Test: inline

- [ ] **Step 1: Add tests for the key-handling contract**

Inside `mod cancel_state_tests`:

```rust
use crate::action::Action;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn press(key: KeyCode) -> KeyEvent {
    KeyEvent::new(key, KeyModifiers::NONE)
}

#[test]
fn esc_with_stream_in_flight_emits_cancel_stream() {
    let mut v = make_view();
    v.stream_in_flight = true;
    v.cancel_mode = Some(spur_acp::CancelMode::AcpSoft);
    let action = <SessionDetailView as crate::views::View>::handle_key(&mut v, press(KeyCode::Esc));
    assert!(matches!(action, Some(Action::CancelStream { .. })));
    assert!(v.cancelling_in_flight);
}

#[test]
fn esc_when_already_cancelling_falls_through_to_navigate_back() {
    let mut v = make_view();
    v.stream_in_flight = true;
    v.cancelling_in_flight = true;
    // Input bar is empty by default → existing Esc path emits NavigateBack.
    let action = <SessionDetailView as crate::views::View>::handle_key(&mut v, press(KeyCode::Esc));
    assert!(matches!(action, Some(Action::NavigateBack)));
}

#[test]
fn esc_without_stream_preserves_navigate_back() {
    let mut v = make_view();
    // No stream in-flight.
    let action = <SessionDetailView as crate::views::View>::handle_key(&mut v, press(KeyCode::Esc));
    assert!(matches!(action, Some(Action::NavigateBack)));
}

#[test]
fn cancel_note_uses_acp_soft_text_when_mode_is_acp_soft() {
    let mut v = make_view();
    v.stream_in_flight = true;
    v.cancel_mode = Some(spur_acp::CancelMode::AcpSoft);
    let _ = <SessionDetailView as crate::views::View>::handle_key(&mut v, press(KeyCode::Esc));
    // The cancel note is the last entry in the trace.
    let trace = v.react_trace();
    let last_text = trace.last_text().unwrap_or_default();
    assert!(last_text.contains("Cancellation requested"),
            "expected AcpSoft message; got {last_text:?}");
}

#[test]
fn cancel_note_uses_process_kill_text_when_mode_is_process_kill() {
    let mut v = make_view();
    v.stream_in_flight = true;
    v.cancel_mode = Some(spur_acp::CancelMode::ProcessKill);
    let _ = <SessionDetailView as crate::views::View>::handle_key(&mut v, press(KeyCode::Esc));
    let trace = v.react_trace();
    let last_text = trace.last_text().unwrap_or_default();
    assert!(last_text.contains("Stopping agent"),
            "expected ProcessKill message; got {last_text:?}");
}

#[test]
fn cancel_note_generic_when_cancel_mode_unknown() {
    let mut v = make_view();
    v.stream_in_flight = true;
    v.cancel_mode = None;
    let _ = <SessionDetailView as crate::views::View>::handle_key(&mut v, press(KeyCode::Esc));
    let trace = v.react_trace();
    let last_text = trace.last_text().unwrap_or_default();
    assert!(last_text.contains("Cancellation requested"),
            "expected generic fallback; got {last_text:?}");
}
```

If `ReactTrace` does not currently expose `last_text()`, add it as a thin accessor for tests:

```rust
// In react_trace.rs, under `impl ReactTrace`:
#[cfg(test)]
pub fn last_text(&self) -> Option<String> {
    self.entries().last().map(|e| e.text.clone())
}
```

Use whatever the existing entries accessor is; `rg -n 'fn entries' crates/spur-tui/src/components/react_trace.rs` or `rg -n 'pub(crate) fn' crates/spur-tui/src/components/react_trace.rs` to find the right signature. If no accessor exists, read the `ReactTrace` module and add the smallest possible one (read-only, behind `#[cfg(test)]` or `pub(crate)`).

- [ ] **Step 2: Run the tests, expect failures on the new assertions**

Run: `cargo test -p spur-tui cancel_state_tests`
Expected: the new tests fail with wrong action / missing trace entry.

- [ ] **Step 3: Add the `push_cancel_note` helper on `SessionDetailView`**

Inside `impl SessionDetailView` (first `impl` block, around line 82–548), add:

```rust
/// Push a system note reflecting the active `cancel_mode`. Called when
/// the user presses `Esc` to cancel an in-flight stream.
fn push_cancel_note(&mut self) {
    let text = match self.cancel_mode {
        Some(spur_acp::CancelMode::AcpSoft) =>
            "\u{23f9} Cancellation requested — waiting for agent\u{2026}",
        Some(spur_acp::CancelMode::ProcessKill) =>
            "\u{23f9} Stopping agent (process will restart on next message)",
        None =>
            "\u{23f9} Cancellation requested",
    };
    self.react_trace.push(TraceEntry {
        kind: TraceKind::Think,
        text: text.to_string(),
        timestamp: Self::now_stamp(),
        #[cfg(feature = "markdown")]
        markdown: None,
    });
}
```

(`\u{23f9}` = ⏹, `\u{2026}` = …. Using escape codes matches the existing style elsewhere in the file.)

- [ ] **Step 4: Add the Esc-priority branch in `handle_key_inner`**

In `fn handle_key_inner` (around line 551), the first real logic is the auth-banner dismissal. Add the new branch immediately after that dismissal and **before** the Alt-m / Alt-s / Alt-v early returns. This ordering is important: Esc-cancel must win over popup dismissal, Enter-submit, and empty-input NavigateBack, but must not block the "any keystroke clears the auth banner" semantics.

```rust
fn handle_key_inner(&mut self, key: KeyEvent) -> Option<Action> {
    // Dismiss the auth banner on any keystroke (before any further routing).
    if self.auth_error.is_some() {
        self.auth_error = None;
    }

    // Priority 0: Esc-to-cancel takes precedence when a stream is in flight
    // and we're not already cancelling. Second Esc falls through to the
    // existing Esc handlers (popup dismiss / NavigateBack).
    if matches!(key.code, KeyCode::Esc)
        && self.stream_in_flight
        && !self.cancelling_in_flight
    {
        self.cancelling_in_flight = true;
        self.push_cancel_note();
        // Update the brain status label immediately so the user sees the
        // acknowledgment within one frame, without waiting for the next
        // `set_brain_status` cycle.
        self.input_bar.set_status(Some(format!(
            "[{}: cancelling\u{2026}]",
            self.agent_name
        )));
        return Some(Action::CancelStream {
            session: self.session_id.clone(),
        });
    }

    // …existing body (Alt-m, Alt-s, Alt-v, priority 1 permission, etc.)…
}
```

- [ ] **Step 5: Make `set_brain_status` respect the cancelling override**

`set_brain_status` (around line 262) currently computes the label solely from the incoming `status` string. We want `cancelling_in_flight` to *suppress* external relabels (the orchestrator will keep flipping between `Thinking`/`Streaming` on subsequent chunks until `TurnComplete`, and we don't want those to flash over our cancelling label). Modify `set_brain_status`:

```rust
pub fn set_brain_status(&mut self, status: &str) {
    if self.cancelling_in_flight {
        // Keep showing the cancelling label until TurnComplete clears the flag.
        self.input_bar.set_status(Some(format!(
            "[{}: cancelling\u{2026}]",
            self.agent_name
        )));
        return;
    }
    let label = match status {
        "idle" => None,
        "thinking" => Some(format!("[{} \u{00b7}\u{00b7}\u{00b7}]", self.agent_name)),
        // …existing arms…
    };
    self.input_bar.set_status(label);
}
```

- [ ] **Step 6: Run the tests, expect pass**

Run: `cargo test -p spur-tui cancel_state_tests`
Expected: all tests pass.

- [ ] **Step 7: Also run the pre-existing Esc/keyboard tests to catch regressions**

Run: `cargo test -p spur-tui views::session_detail`
Expected: all existing tests (e.g. `invalidate_protocols_tests`, the Alt-v tests) pass unchanged.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs crates/spur-tui/src/components/react_trace.rs
git commit -m "feat(spur-tui): Esc-priority cancel handler + transport-aware note"
```

(Only add `react_trace.rs` to the commit if Step 1 required adding `last_text`.)

---

## Task 10: Add `stream_in_flight` prop to `StatusBar` and render the `[Esc]stop` hint

**Files:**
- Modify: `crates/spur-tui/src/components/status_bar.rs`
- Modify: `crates/spur-tui/src/views/session_detail.rs` (render_inner — pass the new prop)
- Test: inline in `status_bar.rs` if there's an existing test module; otherwise a small pure-function test

- [ ] **Step 1: Add a test for the hint selection**

Open `crates/spur-tui/src/components/status_bar.rs`. If there is no existing `#[cfg(test)] mod` block, add one at the bottom. If the rendering code currently builds the hint string inline inside `render`, first **extract it into a pure helper** (this is the minimal refactor that makes the hint testable; avoid broader restructuring):

```rust
// In status_bar.rs, above `impl StatusBar`:
pub(crate) fn hint_for_session_detail(stream_in_flight: bool) -> &'static str {
    if stream_in_flight {
        " [Enter]send [Esc]stop [j/k]scroll [Alt-m]plan [?]help"
    } else {
        " [Enter]send [Esc]back [j/k]scroll [Alt-m]plan [?]help"
    }
}
```

Test:

```rust
#[cfg(test)]
mod status_bar_hint_tests {
    use super::hint_for_session_detail;

    #[test]
    fn hint_shows_stop_when_stream_in_flight() {
        let hint = hint_for_session_detail(true);
        assert!(hint.contains("[Esc]stop"), "got: {hint}");
        assert!(!hint.contains("[Esc]back"));
    }

    #[test]
    fn hint_shows_back_when_idle() {
        let hint = hint_for_session_detail(false);
        assert!(hint.contains("[Esc]back"), "got: {hint}");
        assert!(!hint.contains("[Esc]stop"));
    }
}
```

- [ ] **Step 2: Run the tests, expect compile error on missing helper**

Run: `cargo test -p spur-tui status_bar_hint_tests --no-run`
Expected: `error[E0425]: cannot find function 'hint_for_session_detail'`.

- [ ] **Step 3: Add the helper, and add `stream_in_flight` to `StatusBarProps`**

In `status_bar.rs`:

```rust
#[derive(Clone, Copy)]
pub struct StatusBarProps<'a> {
    pub view: &'a ViewId,
    pub running: usize,
    pub pending_review: usize,
    pub total_cost: f64,
    pub elapsed: &'a str,
    pub current_mode: Option<&'a str>,
    pub context_used: Option<u64>,
    pub context_size: Option<u64>,
    /// True when the SessionDetail view has an in-flight stream; toggles
    /// the status-bar hint between `[Esc]back` (idle) and `[Esc]stop` (live).
    pub stream_in_flight: bool,
}
```

And define the helper (as shown in Step 1). Update `StatusBar::render` to use it for the `ViewId::SessionDetail` arm:

```rust
let hints = match props.view {
    ViewId::Dashboard => " [i]nput [Enter]focus [r]eview [s]essions [Esc]back [?]help [q]uit",
    ViewId::SessionDetail(_) => hint_for_session_detail(props.stream_in_flight),
    ViewId::SessionPicker => " [\u{2191}\u{2193}]navigate [Enter]select [Esc]back",
    #[cfg(feature = "markdown")]
    ViewId::MermaidOverlay(_) => " [Esc]close",
};
```

- [ ] **Step 4: Pass the new prop from `SessionDetailView::render_inner`**

In `crates/spur-tui/src/views/session_detail.rs` at the `StatusBar::render(...)` call (around line 1164), add the field:

```rust
StatusBar::render(
    frame,
    chunks[3],
    StatusBarProps {
        view: &ViewId::SessionDetail(self.session_id.clone()),
        running: 0,
        pending_review: 0,
        total_cost: self.cost,
        elapsed: &elapsed,
        current_mode: self.current_mode.as_deref(),
        context_used: self.context_used,
        context_size: self.context_size,
        stream_in_flight: self.stream_in_flight && !self.cancelling_in_flight,
    },
);
```

(Once we're cancelling, the `cancelling…` label on the InputBar is the primary signal and the `[Esc]stop` hint becomes misleading; hide it by ANDing with `!cancelling_in_flight`.)

- [ ] **Step 5: Update every *other* `StatusBar::render` call site to supply the new field**

Run: `rg -n 'StatusBarProps \{' crates/spur-tui/src`
Expected call sites: `Dashboard::render`, `SessionPicker::render`, and any others. For each, add `stream_in_flight: false,` to the struct literal (only SessionDetail ever sets it to true).

- [ ] **Step 6: Run tests and build**

Run: `cargo test -p spur-tui status_bar_hint_tests`
Expected: both hint tests pass.
Run: `cargo build --workspace`
Expected: clean build.
Run: `cargo test --workspace`
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/status_bar.rs crates/spur-tui/src/views/session_detail.rs crates/spur-tui/src/views/dashboard.rs crates/spur-tui/src/views/session_picker.rs
git commit -m "feat(spur-tui): status-bar [Esc]stop hint during in-flight streams"
```

(Include any other view files touched in Step 5. Use `git status` to verify the set.)

---

## Task 11: Final integration + manual smoke + clippy/fmt

**Files:** *(none — workspace-wide verification)*

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace --all-features`
Expected: clean.

- [ ] **Step 2: Full workspace test**

Run: `cargo test --workspace --all-features`
Expected: all tests pass. If feature-gated markdown tests run here, they should pass without modification (this plan did not touch markdown code).

- [ ] **Step 3: Lint and format**

Run: `cargo fmt --all`
Run: `cargo clippy --workspace --all-features -- -D warnings`
Expected: no diagnostics. Fix any warnings introduced by the plan — typical cases: unused `session` binding in the orchestrator `CancelStream` arm (silenced with `let _ = session;` as shown), missing trailing comma in struct literals, etc.

- [ ] **Step 4: Manual smoke test — ACP transport (preferred path)**

Edit `.spur/config.toml` to ensure `brain.default = "claude-code-acp"` or `"kiro"` (either is ACP transport). Run the TUI:

```bash
cargo run -p spur-cli --bin spur -- watch
```

1. Send a long prompt (e.g. `"Write a 400-word essay on the history of Lisp."`).
2. Wait ~1s for streaming to start — observe `[claude \u25b8\u25b8\u25b8]` label and `[Esc]stop` hint.
3. Press `Esc`. Expect:
   - Trace appends `⏹ Cancellation requested — waiting for agent…`
   - InputBar label flips to `[claude: cancelling…]`
   - Hint reverts to `[Esc]back`.
   - Stream stops within 0–2s; label returns to `[claude: ready]`.
4. Press `Esc` again → expect NavigateBack (returns to Dashboard).
5. Send another prompt to the same session → expect the conversation to continue normally (session context intact).

- [ ] **Step 5: Manual smoke test — non-ACP transport (process-kill path)**

Edit `.spur/config.toml` to set `brain.default = "claude-code"` (or another `transport = "stream-json"` / `"cli-wrap"` entry). Restart the TUI. Repeat the flow above. Expect:
- Trace text reads `⏹ Stopping agent (process will restart on next message)`.
- After `TurnComplete`, the next message respawns the brain (existing lazy-spawn behavior in `orchestrator.rs:597`).

- [ ] **Step 6: Spot-check no regression in the `!…` interrupt path**

While streaming, type `!stop` and press Enter. Expect the existing behavior: cancel + send "stop" as the next prompt. This path was not modified; verifying it is unbroken is a 30-second check.

- [ ] **Step 7: Commit any fmt/clippy fixes from Step 3 that weren't captured earlier**

```bash
git status
# If anything is modified:
git add -u
git commit -m "chore: fmt + clippy cleanups for Esc-to-cancel"
```

- [ ] **Step 8: Final verification that the plan's spec is fully covered**

Re-read `docs/superpowers/specs/2026-04-14-session-detail-esc-cancel-design.md`. Every goal, non-goal, and edge case should have landed in the code:
- Goals 1–4 → Tasks 5, 8, 9, 10.
- Transport polymorphism table (Stdio/CliWrap/StreamJson/Acp cancel behavior) → unchanged (existing code); validated by manual smoke in Steps 4–5.
- Edge cases 1 (stray CancelStream) → Task 4 outer-loop drop.
- Edge case 2 (second Esc) → Task 9 gating `!self.cancelling_in_flight`.
- Edge case 3 (no brain yet) → Task 9 gating `self.stream_in_flight`.
- Edge case 4 (force-timeout) → existing orchestrator logic at ~:733–741; no change needed.
- Edge case 5 (typing during cancel) → existing `pending_messages` path.
- Edge case 6 (AgentSessionReady race) → Task 9 `None → generic` fallback.
- Edge case 7 (label collision) → Task 9 Step 5.

If any bullet lacks a code landing, loop back and add the missing piece before declaring done.

---

## Notes for the executor

- **TDD rigor:** most tasks here follow red/green; a few (Task 2, Task 6) are compile-driven because the unit test adds nothing the compiler doesn't already check. That is deliberate — do not pad those tasks with vacuous asserts.
- **Why the field on `AgentSessionReady` rather than a new event:** adding a new `SpurEventBody::CancelModeReady` variant would require dispatching order guarantees (must arrive before the first chunk). Piggy-backing on `AgentSessionReady` — which the orchestrator already emits before streaming starts — avoids that ordering hazard.
- **Why the orchestrator's `session` binding in the `CancelStream` arm is `let _ = session;`:** the streaming `select!` runs per-brain-session; there is exactly one active turn. A mismatched `session` id cannot physically occur here. The field is kept on `CancelStream` for symmetry with `SendMessage` and for the outer-loop debug log.
- **Don't extend `BrainStatus` with a `Cancelling` variant.** This was explicitly decided during brainstorming — cancellation is a view-local transient, and a single-file `bool` is simpler than an enum variant that would ripple through every matcher.
