# Brain connection respawn — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When the active brain's ACP subprocess dies mid-session, detect the death at the next RPC call, transparently spawn a replacement, reattach via `session/load` (or surface state loss if that fails), and emit new `BrainReconnecting` / `BrainReconnected` / `BrainReconnectFailed` events. A simple 2-failures-in-60s circuit breaker prevents infinite respawn loops.

**Architecture:** Inline recovery in the existing sequential `run_interactive` loop — no supervisor task, no `Arc<Mutex>`. A new helper `Orchestrator::try_reconnect_brain` reuses the existing `connect_brain` + `load_brain_session` helpers, returning a fresh `BrainSession` plus a `LoadOutcome` that says whether `session/load` restored state or fell back to a new session. Call sites that emit `BrainError` on connection death (the prompt and vendor-exec arms) attempt reconnect first before falling through to the existing teardown.

**Tech Stack:** Rust, tokio, anyhow, agent_client_protocol. Touches `crates/spur-acp/src/domain/events.rs`, `crates/spur-core/src/orchestrator.rs`, `crates/spur-tui/src/views/session_detail.rs`, `crates/spur-tui/src/views/dashboard.rs`, and adds a test fixture + integration test. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-04-15-brain-connection-respawn-design.md`.

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-acp/src/domain/events.rs` | Modify | Define `LoadOutcome` and add three new `SpurEventBody` variants for reconnect lifecycle. |
| `crates/spur-acp/src/lib.rs` | Modify | Re-export `LoadOutcome`. |
| `crates/spur-core/src/orchestrator.rs` | Modify | Extend `load_brain_session` signature to carry `LoadOutcome`; add `is_connection_death` + `try_reconnect_brain`; wrap the two BrainError-emitting RPC sites with reconnect attempts; plumb circuit-breaker state through `run_interactive`. |
| `crates/spur-tui/src/views/session_detail.rs` | Modify | Render the three reconnect events as trace entries. |
| `crates/spur-tui/src/views/dashboard.rs` | Modify | Same trace-entry rendering pattern (symmetric with `BrainError`). |
| `crates/spur-acp/tests/fixtures/agent_dies_on_second_prompt.sh` | Create | Mock ACP agent that handles initialize + session/new + first prompt, then exits after receiving the second prompt. Supports reconnect integration tests. |
| `crates/spur-core/tests/brain_reconnect.rs` | Create | Integration test driving the mock agent; asserts the Reconnecting → Reconnected sequence, `SpurEvent.seq` monotonicity, and circuit-breaker behavior. |

---

## Task 1: Introduce `LoadOutcome` and new reconnect event variants

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs` (SpurEventBody enum, ~line 120+)
- Modify: `crates/spur-acp/src/lib.rs` (add `LoadOutcome` to the re-exports block that already re-exports `SpurEventBody`, `SessionId`, etc.)

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-acp/src/domain/events.rs` (inside an existing `#[cfg(test)] mod tests { ... }` block if present; otherwise add one at the bottom of the file):

```rust
#[cfg(test)]
mod reconnect_event_tests {
    use super::*;
    use crate::SessionId;

    #[test]
    fn load_outcome_variants_construct() {
        let _ = LoadOutcome::Restored;
        let _ = LoadOutcome::FellBackToNew { reason: "session/load returned error".into() };
    }

    #[test]
    fn brain_reconnect_events_construct() {
        let s = SessionId::new();
        let _ = SpurEventBody::BrainReconnecting {
            session: s.clone(),
            brain_name: "kiro".into(),
            reason: "ACP thread died during prompt".into(),
        };
        let _ = SpurEventBody::BrainReconnected {
            session: s.clone(),
            brain_name: "kiro".into(),
            outcome: LoadOutcome::Restored,
        };
        let _ = SpurEventBody::BrainReconnectFailed {
            session: s,
            brain_name: "kiro".into(),
            reason: "circuit breaker tripped".into(),
        };
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-acp --lib domain::events::reconnect_event_tests`
Expected: FAIL — `LoadOutcome` unknown and the three variants don't exist.

- [ ] **Step 3: Add `LoadOutcome` and the three variants**

In `crates/spur-acp/src/domain/events.rs`, immediately above `pub enum SpurEventBody` (around line 118), add:

```rust
/// Result of attempting `session/load` on a brain connection. Returned
/// from `load_brain_session` so the caller can distinguish "state
/// actually came back" from "we silently created a fresh session."
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    /// `session/load` returned the prior session state.
    Restored,
    /// `session/load` failed (unsupported, or errored) and we started a
    /// new session. `reason` is the underlying error.
    FellBackToNew { reason: String },
}
```

Then inside `pub enum SpurEventBody`, append three new variants near the existing `BrainError` variant (keep the ordering: `BrainError`, then the three new ones):

```rust
    /// Brain subprocess appears to have died; a reconnect attempt is
    /// starting. Emitted BEFORE `connect_brain` runs so the TUI can
    /// display a banner immediately (subprocess spawn takes >1s).
    BrainReconnecting {
        session: SessionId,
        brain_name: String,
        /// Human-readable reason (usually the RPC error that tripped
        /// the detector).
        reason: String,
    },
    /// Reconnect succeeded. `outcome` says whether session state was
    /// restored or we fell back to a fresh session.
    BrainReconnected {
        session: SessionId,
        brain_name: String,
        outcome: LoadOutcome,
    },
    /// Reconnect attempt failed OR the circuit breaker tripped. The
    /// brain stays unset and the user must take an explicit action to
    /// retry.
    BrainReconnectFailed {
        session: SessionId,
        brain_name: String,
        reason: String,
    },
```

- [ ] **Step 4: Re-export `LoadOutcome` from the crate root**

In `crates/spur-acp/src/lib.rs`, locate the existing `pub use` block that re-exports `SpurEventBody`, `SessionId`, and friends (search for `pub use crate::domain::events::` or `pub use domain::events::`). Add `LoadOutcome` to that list. Example (your existing list will differ — only add `LoadOutcome`):

```rust
pub use crate::domain::events::{
    LoadOutcome, SessionId, SpurEvent, SpurEventBody, /* …existing names… */
};
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p spur-acp --lib domain::events::reconnect_event_tests`
Expected: PASS.

- [ ] **Step 6: Run the full spur-acp suite to catch any exhaustive `match` on SpurEventBody that now has missing arms**

Run: `cargo test -p spur-acp`
Expected: PASS.

If compilation fails in a downstream consumer, locate each non-exhaustive match on `SpurEventBody` and add `_ => { /* ignore new variants */ }` or handle the variant explicitly. Only change matches that fail compilation.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs crates/spur-acp/src/lib.rs
git commit -m "feat(events): add LoadOutcome + BrainReconnect{ing,ed,Failed} variants

Precursors to auto-reconnect. LoadOutcome makes session/load's silent
new_session fallback observable; the three BrainReconnect variants let
the TUI render reconnect lifecycle banners."
```

---

## Task 2: Plumb `LoadOutcome` out of `load_brain_session`

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (`load_brain_session` signature + body around line 1231-1398, and its single existing call site in the `ResumeSession` arm around line 536-545)

The silent-fallback path is at `orchestrator.rs:1316-1332` today. We surface it.

- [ ] **Step 1: Update the signature and body to return `LoadOutcome`**

In `load_brain_session` (starts ~line 1231), change the return type:

```rust
async fn load_brain_session(
    &mut self,
    mut connection: Box<dyn spur_acp::AgentConnection>,
    brain_name: String,
    _permission_tx: Option<
        tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
    >,
    acp_session_id: String,
) -> Result<(
    BrainSession,
    std::pin::Pin<Box<dyn futures::Stream<Item = spur_acp::SessionNotification> + Send>>,
    spur_acp::LoadOutcome,
)> {
```

At the tuple-destructuring `let (final_acp_session_id, history_stream, resumed) = match ...` (around line 1306), capture the reason on fallback:

```rust
let (final_acp_session_id, history_stream, resumed, load_outcome) =
    match crate::skip_perm::load_session_with_bypass(
        &mut *connection,
        &brain_cfg,
        acp_session_id.clone(),
        self.repo_root.clone(),
        mcp_servers.clone(),
    )
    .await
    {
        Ok(stream) => {
            debug!(brain = %brain_name, "load_session succeeded");
            (acp_session_id, Some(stream), true, spur_acp::LoadOutcome::Restored)
        }
        Err(e) => {
            warn!(brain = %brain_name, error = %e, "load_session failed, falling back to new_session");
            let fallback_reason = e.to_string();
            let session_response = crate::skip_perm::new_session_with_bypass(
                &mut *connection,
                &brain_cfg,
                self.repo_root.clone(),
                mcp_servers,
            )
            .await
            .context("Failed to create fallback session after load_session failure")?;
            (
                session_response.session_id.to_string(),
                None,
                false,
                spur_acp::LoadOutcome::FellBackToNew { reason: fallback_reason },
            )
        }
    };
```

At the end of `load_brain_session`, change the `Ok((brain_session, history_stream))` return to include `load_outcome`. Find the current return (around line 1396-1398) and replace:

```rust
Ok((brain_session, history_stream, load_outcome))
```

If `history_stream` isn't already unwrapped into the required `Pin<Box<dyn Stream>>` type at the return site, wrap the `None` case with `futures::stream::empty()`:

```rust
let history_stream: std::pin::Pin<Box<dyn futures::Stream<Item = spur_acp::SessionNotification> + Send>> =
    match history_stream {
        Some(s) => s,
        None => Box::pin(futures::stream::empty()),
    };
```

(Check the current code — if this wrapping already exists, don't duplicate it.)

- [ ] **Step 2: Update the single existing call site (ResumeSession arm)**

In the `ResumeSession` match arm around `orchestrator.rs:537-545`, change the destructure from:

```rust
Ok((session, mut history_stream)) => {
```

to:

```rust
Ok((session, mut history_stream, _load_outcome)) => {
```

(The resume path doesn't surface the outcome yet — that's not this task. We just accept the new return shape.)

- [ ] **Step 3: Build and run tests to confirm the shape change doesn't regress resume**

Run: `cargo build -p spur-core && cargo test -p spur-core --test init_agents`
Expected: build + tests PASS.

- [ ] **Step 4: Run the full spur-core suite**

Run: `cargo test -p spur-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "refactor(orchestrator): surface LoadOutcome from load_brain_session

session/load silently falls back to new_session when the agent refuses
or errors. Capture that fallback (plus the underlying reason) in a
LoadOutcome returned alongside the BrainSession. The existing
ResumeSession call site ignores it for now; the upcoming reconnect
path consumes it to distinguish restored vs. fresh sessions."
```

---

## Task 3: Add `is_connection_death` helper

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (add free function or `impl Orchestrator` method near `retire_active_brain` around line 1039)

- [ ] **Step 1: Write the failing test**

In the `#[cfg(test)] mod tests` block at the bottom of `orchestrator.rs` (or add one if absent), append:

```rust
#[test]
fn is_connection_death_detects_known_patterns() {
    let e1 = anyhow::anyhow!("NativeAcpConnection 'kiro': ACP thread died during ext_method");
    assert!(is_connection_death(&e1));

    let e2 = anyhow::anyhow!("NativeAcpConnection 'kiro': ACP thread died");
    assert!(is_connection_death(&e2));

    let e3 = anyhow::anyhow!("Internal error: \"server shut down unexpectedly\"");
    assert!(is_connection_death(&e3));

    let e4 = anyhow::anyhow!("prompt rejected: invalid session id");
    assert!(!is_connection_death(&e4));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p spur-core --lib orchestrator::tests::is_connection_death_detects_known_patterns`
Expected: FAIL — `is_connection_death` is not defined.

- [ ] **Step 3: Add the helper**

At the top level of `orchestrator.rs` (below the `use` block, above `impl Orchestrator`), add:

```rust
/// Detect whether an error from an `AgentConnection` RPC indicates the
/// underlying subprocess has died (pipe closed, ACP thread exited, etc.),
/// versus a normal request-level error (auth needed, invalid session, etc.).
///
/// Match against well-known error-message fragments emitted by
/// `NativeAcpConnection` and the ACP SDK. This is a pragmatic string-match —
/// a more structured signal would require a new trait method on
/// `AgentConnection`; revisit if the set of transports grows.
pub(crate) fn is_connection_death(err: &anyhow::Error) -> bool {
    let msg = err.to_string();
    msg.contains("ACP thread died")
        || msg.contains("server shut down unexpectedly")
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p spur-core --lib orchestrator::tests::is_connection_death_detects_known_patterns`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(orchestrator): add is_connection_death error classifier

Pragmatic string-match against the two known 'subprocess is gone' error
patterns emitted by NativeAcpConnection (AcpCommand channel closed) and
the ACP SDK (stdio pipe closed mid-RPC). Callers use this to decide
whether to attempt reconnect vs. surface a normal BrainError."
```

---

## Task 4: Add `try_reconnect_brain` helper

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (add method on `impl Orchestrator`, near `load_brain_session` around line 1231)

- [ ] **Step 1: Add the method**

Inside `impl Orchestrator`, place this method directly after `load_brain_session`:

```rust
/// Attempt to reconnect after a brain subprocess death. Drops the dead
/// `BrainSession` (closing its stdio and aborting its helper tasks),
/// spawns a fresh connection via `connect_brain`, then reattaches via
/// `load_brain_session` using the old `acp_session_id`.
///
/// On success returns the new `BrainSession` and the `LoadOutcome`
/// distinguishing "session/load restored state" from "we fell back to
/// a new session". On failure the caller must surface
/// `BrainReconnectFailed` and leave `brain = None`.
///
/// The caller (not this helper) is responsible for emitting
/// `BrainReconnecting` BEFORE invoking this, and
/// `BrainReconnected` / `BrainReconnectFailed` after.
async fn try_reconnect_brain(
    &mut self,
    dead_brain: BrainSession,
    permission_tx: Option<
        tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
    >,
    brain_override: Option<&str>,
) -> Result<(BrainSession, spur_acp::LoadOutcome)> {
    let acp_session_id = dead_brain.acp_session_id.clone();
    let brain_name_hint = dead_brain.brain_name.clone();

    // Drop the dead session: abort helper tasks, close stdio.
    dead_brain.delegation_handle.abort();
    if let Some(h) = dead_brain.notification_pump_handle {
        h.abort();
    }
    dead_brain.mcp_handle.abort();
    drop(dead_brain.connection);

    // Fresh connection + reattach.
    let (connection, brain_name) = self
        .connect_brain(brain_override, permission_tx.clone())
        .await
        .with_context(|| format!("reconnect: connect_brain failed for '{brain_name_hint}'"))?;

    let (new_session, mut history_stream, outcome) = self
        .load_brain_session(connection, brain_name, permission_tx, acp_session_id)
        .await
        .with_context(|| format!("reconnect: load_brain_session failed for '{brain_name_hint}'"))?;

    // Drain the history stream to keep the pump contract (same pattern as
    // the ResumeSession arm). We do NOT re-emit AgentNotification events
    // here — the TUI already rendered the pre-death transcript.
    while let Some(_notification) = history_stream.next().await {}

    Ok((new_session, outcome))
}
```

- [ ] **Step 2: Build to verify the method type-checks**

Run: `cargo build -p spur-core`
Expected: build PASS (no new tests yet — integration test comes in Task 6).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(orchestrator): add try_reconnect_brain helper

Drops the dead BrainSession (aborting its delegation, MCP, and
notification-pump handles + closing stdio), spawns a fresh connection
via connect_brain, and reattaches via load_brain_session with the old
acp_session_id. Returns the new BrainSession plus the LoadOutcome the
caller uses to populate BrainReconnected.outcome. The caller owns the
BrainReconnecting / BrainReconnected / BrainReconnectFailed emissions."
```

---

## Task 5: Wire reconnect into the prompt + vendor-exec error branches

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` — the VendorExec arm (around line 618-630) and the Message/prompt arm (around line 724-747), plus add circuit-breaker state at the top of `run_interactive` (around line 435).

- [ ] **Step 1: Add circuit-breaker locals**

In `run_interactive`, immediately after `let mut agent_connection: Option<...> = None;` (around line 439), add:

```rust
let mut reconnect_failures: std::collections::VecDeque<std::time::Instant> =
    std::collections::VecDeque::new();
const RECONNECT_CIRCUIT_LIMIT: usize = 2;
const RECONNECT_CIRCUIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
```

- [ ] **Step 2: Factor reconnect + event-emission into a local closure-like helper**

Because the reconnect-and-emit sequence needs to run from two different match arms, add a private method on `impl Orchestrator` near `try_reconnect_brain`:

```rust
/// Wrap `try_reconnect_brain` with the three event emissions and the
/// circuit-breaker bookkeeping. Returns `Some(new_brain)` if reconnect
/// succeeded and the caller should swap it back into the run_interactive
/// `brain` slot; returns `None` if reconnect was skipped (circuit open) or
/// failed (in which case `BrainReconnectFailed` has already been emitted).
async fn reconnect_with_events(
    &mut self,
    dead_brain: BrainSession,
    permission_tx: Option<
        tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
    >,
    brain_override: Option<&str>,
    trigger_reason: String,
    failures: &mut std::collections::VecDeque<std::time::Instant>,
) -> Option<BrainSession> {
    let spur_session_id = dead_brain.spur_session_id.clone();
    let brain_name = dead_brain.brain_name.clone();

    // Trim stale failure timestamps and check the breaker.
    let now = std::time::Instant::now();
    while let Some(front) = failures.front() {
        if now.duration_since(*front) > RECONNECT_CIRCUIT_WINDOW {
            failures.pop_front();
        } else {
            break;
        }
    }
    if failures.len() >= RECONNECT_CIRCUIT_LIMIT {
        self.emit(SpurEvent::now(SpurEventBody::BrainReconnectFailed {
            session: spur_session_id,
            brain_name,
            reason: format!(
                "circuit breaker: {} reconnect failures within {:?}",
                failures.len(),
                RECONNECT_CIRCUIT_WINDOW
            ),
        }));
        return None;
    }

    self.emit(SpurEvent::now(SpurEventBody::BrainReconnecting {
        session: spur_session_id.clone(),
        brain_name: brain_name.clone(),
        reason: trigger_reason,
    }));

    match self
        .try_reconnect_brain(dead_brain, permission_tx, brain_override)
        .await
    {
        Ok((new_brain, outcome)) => {
            failures.clear(); // success resets the window
            self.emit(SpurEvent::now(SpurEventBody::BrainReconnected {
                session: new_brain.spur_session_id.clone(),
                brain_name: new_brain.brain_name.clone(),
                outcome,
            }));
            Some(new_brain)
        }
        Err(e) => {
            failures.push_back(std::time::Instant::now());
            self.emit(SpurEvent::now(SpurEventBody::BrainReconnectFailed {
                session: spur_session_id,
                brain_name,
                reason: e.to_string(),
            }));
            None
        }
    }
}
```

- [ ] **Step 3: Update the VendorExec arm**

Replace the existing VendorExec error branch at `orchestrator.rs:618-630`. The current code:

```rust
Err(e) => {
    warn!(
        brain = %b.brain_name,
        method = %method,
        error = %e,
        "vendor exec call failed"
    );
    self.emit(SpurEvent::now(SpurEventBody::BrainError {
        session,
        message: format!("vendor exec `{}` failed: {}", method, e),
    }));
}
```

becomes:

```rust
Err(e) => {
    warn!(
        brain = %b.brain_name,
        method = %method,
        error = %e,
        "vendor exec call failed"
    );
    if is_connection_death(&e) {
        // Take the brain out so we can move it into the reconnect path.
        if let Some(dead) = brain.take() {
            let reason = format!("vendor exec `{method}` died: {e}");
            if let Some(new_brain) = self
                .reconnect_with_events(
                    dead,
                    permission_tx.clone(),
                    brain_override.as_deref(),
                    reason,
                    &mut reconnect_failures,
                )
                .await
            {
                brain = Some(new_brain);
            }
            // If reconnect failed, BrainReconnectFailed was already
            // emitted; `brain` stays None.
        }
    } else {
        self.emit(SpurEvent::now(SpurEventBody::BrainError {
            session,
            message: format!("vendor exec `{}` failed: {}", method, e),
        }));
    }
}
```

- [ ] **Step 4: Update the prompt-error branch**

Replace the existing `Err(e)` branch on `b.connection.prompt(...)` at `orchestrator.rs:724-747`. The current code aborts handles, shuts down, sets `brain = None`, emits `BrainError`, and `continue`s. The reconnect version:

```rust
let mut stream = match b.connection.prompt(prompt_request).await {
    Ok(s) => s,
    Err(e) => {
        error!(error = %e, "Brain prompt failed");
        if Self::is_auth_required_error(&e) {
            self.emit(SpurEvent::now(SpurEventBody::AuthRequired {
                session: b.spur_session_id.clone(),
                message: Self::auth_required_banner(),
            }));
            b.delegation_handle.abort();
            if let Some(h) = b.notification_pump_handle.take() {
                h.abort();
            }
            b.mcp_handle.abort();
            let _ = b.connection.shutdown().await;
            brain = None;
            continue;
        }
        if is_connection_death(&e) {
            // Move the dead brain out; reconnect_with_events consumes it.
            let dead = brain.take().expect("brain.as_mut() just held it");
            let reason = format!("prompt died: {e}");
            if let Some(new_brain) = self
                .reconnect_with_events(
                    dead,
                    permission_tx.clone(),
                    brain_override.as_deref(),
                    reason,
                    &mut reconnect_failures,
                )
                .await
            {
                brain = Some(new_brain);
            }
            // Whether reconnect succeeded or not, we drop this turn's
            // prompt — the user must retype. (See spec non-goals.)
            continue;
        }
        // Non-connection-death error: existing teardown.
        self.emit(SpurEvent::now(SpurEventBody::BrainError {
            session: b.spur_session_id.clone(),
            message: e.to_string(),
        }));
        b.delegation_handle.abort();
        if let Some(h) = b.notification_pump_handle.take() {
            h.abort();
        }
        b.mcp_handle.abort();
        let _ = b.connection.shutdown().await;
        brain = None;
        continue;
    }
};
```

Note: the `b` binding in this block is the `&mut BrainSession` from `let b = brain.as_mut().unwrap();` earlier in the Message arm. `brain.take()` below ends that borrow — ordering matters: call `brain.take()` only after every use of `b` in this error branch. The code above reads from `b` only inside the auth-required and the non-connection-death fallback branches; the connection-death branch does `brain.take()` cleanly.

If the compiler rejects the borrow, split the error handling into a preliminary classification that computes `(is_auth, is_dead, err_string)` first using `b`, then drops `b` and acts.

- [ ] **Step 5: Build and run the existing test suite**

Run: `cargo build -p spur-core && cargo test -p spur-core`
Expected: PASS. No new behavioral tests yet (that's Task 6); this step catches regressions.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(orchestrator): auto-reconnect brain on subprocess death

On connection-death errors from prompt() or call_ext(), the active
BrainSession is moved into reconnect_with_events, which emits
BrainReconnecting, calls try_reconnect_brain, then emits
BrainReconnected (with LoadOutcome) or BrainReconnectFailed. A local
VecDeque<Instant> implements a 2-failures-in-60s circuit breaker. The
in-flight turn is dropped (per spec non-goal: no automatic replay) —
the user retypes the prompt."
```

---

## Task 6: Integration test with a scripted agent that dies on second prompt

**Files:**
- Create: `crates/spur-acp/tests/fixtures/agent_dies_on_second_prompt.sh`
- Create: `crates/spur-core/tests/brain_reconnect.rs`

- [ ] **Step 1: Write the mock agent fixture**

Create `crates/spur-acp/tests/fixtures/agent_dies_on_second_prompt.sh`:

```bash
#!/bin/bash
# Mock ACP agent for reconnect tests.
#
# - First run (before death): handles initialize, session/new, session/prompt.
#   After emitting the prompt_response, exits cleanly so the next prompt
#   call finds the stdio pipe closed and surfaces an "ACP thread died"
#   error.
# - Subsequent runs (after respawn): handles initialize, session/load
#   returning the same sessionId, then a normal session/prompt reply.
#
# Counter file is needed because each spawn is a fresh process; we use
# a sidecar file next to the script.

set -u
COUNTER_FILE="${KIRO_DEATH_COUNTER:-/tmp/spur_death_counter}"
count=$(cat "$COUNTER_FILE" 2>/dev/null || echo 0)
count=$((count + 1))
echo "$count" > "$COUNTER_FILE"

while IFS= read -r line; do
    method=$(printf '%s' "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
    id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')

    case "$method" in
        initialize)
            echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true,"promptCapabilities":{}},"authMethods":[]}}'
            ;;
        session/new)
            echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"sessionId":"reconnect-test-session"}}'
            ;;
        session/load)
            # Always succeed — simulates an agent that persists session state.
            echo '{"jsonrpc":"2.0","id":'"$id"',"result":{}}'
            ;;
        session/prompt)
            echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"stopReason":"end_turn"}}'
            if [ "$count" = "1" ]; then
                # First spawn: exit after replying so the next prompt
                # request (sent to a future spawn) triggers reconnect.
                exit 0
            fi
            ;;
    esac
done
```

Make it executable:
```bash
chmod +x crates/spur-acp/tests/fixtures/agent_dies_on_second_prompt.sh
```

- [ ] **Step 2: Write the integration test**

Create `crates/spur-core/tests/brain_reconnect.rs`:

```rust
//! Integration test for brain auto-reconnect on subprocess death.
//!
//! Drives `run_interactive` via its public entry point using the mock
//! agent in `tests/fixtures/agent_dies_on_second_prompt.sh`. Asserts:
//! 1. The first prompt completes normally.
//! 2. The second prompt triggers BrainReconnecting → BrainReconnected.
//! 3. SpurEvent.seq remains strictly monotonic across the reconnect.

use spur_acp::{LoadOutcome, SpurEventBody};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn brain_reconnects_after_subprocess_death() {
    // Clear the counter from any prior test run.
    let counter_path = std::env::temp_dir().join("spur_death_counter_test1");
    let _ = std::fs::remove_file(&counter_path);

    let (events, mut harness) =
        crate::common::spawn_orchestrator_with_mock_agent(
            "agent_dies_on_second_prompt.sh",
            &counter_path,
        )
        .await;

    // First prompt: should complete normally.
    harness.send_user_text("hello").await;
    harness.wait_for_turn_complete().await;

    // Second prompt: the script exited after the first prompt reply, so
    // the underlying stdio has already closed. The orchestrator's
    // prompt() call on a dead pipe should trigger reconnect.
    harness.send_user_text("still there?").await;

    let (reconnecting, reconnected) = harness
        .wait_for_reconnect_pair()
        .await
        .expect("expected BrainReconnecting followed by BrainReconnected");

    assert!(
        matches!(reconnecting.body, SpurEventBody::BrainReconnecting { .. }),
        "first event must be BrainReconnecting, got {:?}",
        reconnecting.body
    );
    match &reconnected.body {
        SpurEventBody::BrainReconnected { outcome, .. } => {
            // Our mock agent returns loadSession success → Restored.
            assert_eq!(*outcome, LoadOutcome::Restored);
        }
        other => panic!("second event must be BrainReconnected, got {:?}", other),
    }

    // SpurEvent.seq monotonicity across the reconnect window.
    let seqs: Vec<u64> = events
        .lock()
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    for pair in seqs.windows(2) {
        assert!(pair[0] < pair[1], "SpurEvent.seq must be strictly monotonic");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_circuit_breaker_trips_after_two_failures() {
    // Mock agent that ALWAYS dies (counter logic reversed: every spawn
    // exits after first prompt). Using a second counter file keeps tests
    // independent.
    let counter_path = std::env::temp_dir().join("spur_death_counter_test2");
    let _ = std::fs::remove_file(&counter_path);

    // Use a fixture that dies on every spawn. For this first revision
    // we reuse the same script: since its "first spawn" logic checks the
    // counter file and we reset it to 0 via a separate path, spawn 1
    // exits after prompt; spawn 2 (the reconnect) also sees count=1 on
    // its *own* counter path, so it also exits. Good enough to exercise
    // the breaker.
    let (events, mut harness) =
        crate::common::spawn_orchestrator_with_mock_agent(
            "agent_dies_on_second_prompt.sh",
            &counter_path,
        )
        .await;

    // Drive three failures — the breaker should trip on the third attempt.
    harness.send_user_text("p1").await;
    harness.wait_for_turn_complete().await;
    harness.send_user_text("p2").await; // triggers reconnect #1
    harness.send_user_text("p3").await; // if reconnect #1 succeeded and then died, triggers #2
    harness.send_user_text("p4").await; // if #2 died, expect circuit to trip here

    let failed = harness
        .wait_for_reconnect_failed(std::time::Duration::from_secs(10))
        .await;

    assert!(
        failed.is_some(),
        "expected BrainReconnectFailed after circuit breaker trips"
    );

    drop(events);
}

// Common harness lives in a sibling tests/common.rs if the repo already
// has one; otherwise inline a minimal spawner here. Keep the harness
// parameterized on the fixture filename and counter-file path so each
// test owns its own state.
mod common;
```

If `tests/common/` already has a harness for orchestrator integration tests, extend it instead of adding a new one. Search first:

Run: `ls crates/spur-core/tests/ && grep -rln "spawn_orchestrator" crates/spur-core/tests/ 2>/dev/null`

If the repo has no shared harness, write `crates/spur-core/tests/common/mod.rs` that:
1. Spawns an `Orchestrator` with a minimal `SpurConfig` whose only agent entry points its `command` at the fixture script (set `KIRO_DEATH_COUNTER` via `env`).
2. Returns the `broadcast::Receiver<SpurEvent>` and a small `TestHarness` with methods `send_user_text(&mut self, text: &str)`, `wait_for_turn_complete`, `wait_for_reconnect_pair`, `wait_for_reconnect_failed`.

The harness methods read from the receiver with timeouts; shape each method around a `tokio::time::timeout(Duration::from_secs(5), loop { … })` that matches on `SpurEventBody` variants.

- [ ] **Step 3: Run the new tests**

Run: `cargo test -p spur-core --test brain_reconnect`
Expected: both tests PASS.

If the first test times out, add tracing and inspect the event sequence. The most likely failure mode is the mock script not writing its counter correctly — test it manually by running it directly with a canned stdin.

- [ ] **Step 4: Run the full spur-core + spur-acp test suites to catch cross-crate regressions**

Run: `cargo test -p spur-core -p spur-acp`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/tests/fixtures/agent_dies_on_second_prompt.sh \
        crates/spur-core/tests/brain_reconnect.rs \
        crates/spur-core/tests/common
git commit -m "test(orchestrator): integration coverage for brain reconnect

Mock agent script exits cleanly after its first prompt reply; a
follow-up prompt triggers the reconnect path. Asserts the
BrainReconnecting → BrainReconnected pair, LoadOutcome::Restored via
the script's loadSession stub, and SpurEvent.seq monotonicity across
the reconnect window. Second test exercises the 2-in-60s circuit
breaker by letting three reconnect attempts die in a row."
```

---

## Task 7: TUI — render the three reconnect events as trace entries

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs` (the `match SpurEventBody` block around the existing `BrainError` handler near line 1138)
- Modify: `crates/spur-tui/src/views/dashboard.rs` (symmetric handler around line 770)

- [ ] **Step 1: Extend the SpurEventBody match in `session_detail.rs`**

Find the existing `SpurEventBody::BrainError { session, message }` arm (around line 1138). Add three new arms BELOW it (before the final catch-all `_ =>` if present):

```rust
SpurEventBody::BrainReconnecting { session, brain_name, reason } => {
    if session.0 == self.session_id.0 {
        self.react_trace.push(TraceEntry {
            kind: TraceEntryKind::SystemNote,
            text: format!("brain '{brain_name}' reconnecting… ({reason})"),
            timestamp: chrono::Utc::now(),
        });
    }
}
SpurEventBody::BrainReconnected { session, brain_name, outcome } => {
    if session.0 == self.session_id.0 {
        let (text, kind) = match outcome {
            spur_acp::LoadOutcome::Restored => (
                format!("brain '{brain_name}' reconnected — state restored"),
                TraceEntryKind::SystemNote,
            ),
            spur_acp::LoadOutcome::FellBackToNew { reason } => (
                format!(
                    "brain '{brain_name}' reconnected — state LOST, \
                     session/load failed ({reason}); check context"
                ),
                TraceEntryKind::Warning,
            ),
        };
        self.react_trace.push(TraceEntry {
            kind,
            text,
            timestamp: chrono::Utc::now(),
        });
    }
}
SpurEventBody::BrainReconnectFailed { session, brain_name, reason } => {
    if session.0 == self.session_id.0 {
        self.react_trace.push(TraceEntry {
            kind: TraceEntryKind::Error,
            text: format!("brain '{brain_name}' reconnect failed: {reason}"),
            timestamp: chrono::Utc::now(),
        });
    }
}
```

**Before editing**, read the surrounding code to confirm the exact `TraceEntry` / `TraceEntryKind` constructor shape — this plan's snippet assumes the fields match the existing `BrainError` arm's construction. If the actual shape differs (e.g. different field names, no `kind` field, different timestamp source), mirror the `BrainError` arm's exact construction and only vary the `text`.

- [ ] **Step 2: Extend the symmetric handler in `dashboard.rs`**

Find the existing `BrainError` arm around line 770. Add the same three arms directly after it, using whatever construction pattern dashboard.rs uses (likely `Self::prefix_for_session` + a string). Keep the rendering minimal: one log line per event, tagged with the brain name and the event variant. If `dashboard.rs` only logs `BrainError` with a single `.push_log(...)`-style call, do the same for the three new variants.

- [ ] **Step 3: Build the TUI**

Run: `cargo build -p spur-tui`
Expected: build PASS.

- [ ] **Step 4: Run the tui test suite**

Run: `cargo test -p spur-tui`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs \
        crates/spur-tui/src/views/dashboard.rs
git commit -m "feat(tui): render BrainReconnect events in trace + dashboard

Three new trace entries: a system note when reconnect starts, a
(colored) line when it succeeds (warning color if state was lost via
session/load fallback), and an error line when the circuit breaker
trips. Mirrors the existing BrainError rendering."
```

---

## Task 8: Invariant audit — trailing-notification grace timer + full spur-invariants-reviewer pass

**Files:** none (verification + optional defensive fix)

- [ ] **Step 1: Read `crates/spur-core/src/notification_drain.rs` end-to-end**

Specifically verify that in `drive_prompt_notifications`, when the broadcast returns `RecvError::Closed` (because the connection died and its sender dropped), the select loop exits cleanly rather than spinning or panicking. Read the loop body and the `BcastOutcome::Closed` handling.

- [ ] **Step 2: If the handler is correct, move on. If not, add a failing test + fix.**

The expected correct behavior is: `Closed` returns from the async block without pushing a notification, and the `select!` falls through to the next iteration; since the prompt future has already resolved to `Err(…)` earlier (the RPC error that triggered the reconnect), the function returns. No leak, no panic.

If the code instead `.unwrap()`s the broadcast recv, add a test that closes the sender mid-drain and asserts `drive_prompt_notifications` returns cleanly (not panics). Then fix by matching on the error and returning `Ok(())`.

This step is intentionally contingent — the spec flagged it as "highest-risk unknown; if broader redesign needed, pull out into its own spec." If the pre-existing handler is correct, commit nothing. If it needs a fix but the fix is >30 min of work, open a follow-up spec and skip this step.

- [ ] **Step 3: Dispatch the `spur-invariants-reviewer` agent on the full branch diff**

Run the repo-provided agent against all commits in this plan (the agent is listed in the plan controller's agent registry under `spur-invariants-reviewer`). Give it:
- The SHAs of the commits from Tasks 1-7.
- A hint: "touches connection lifecycle, SpurEvent emissions, broadcast subscribers, delegation handle lifetimes."

If the reviewer flags invariants at risk (SpurEvent.seq monotonicity, broadcast sizing, TUI drain cap, append_message walkback, ACP trailing-notification grace), fix each finding inline and re-dispatch.

- [ ] **Step 4: (Optional) Commit any audit-driven fixes with a descriptive message**

If Step 2 or Step 3 produced real fixes:

```bash
git add <files touched>
git commit -m "fix(reconnect): invariant-audit follow-ups

<enumerate each finding and its fix>"
```

- [ ] **Step 5: Manual smoke test with kiro**

With the branch built:

1. `cargo run -p spur-cli -- --brain kiro`.
2. Send a prompt; wait for reply.
3. From another terminal: `pkill -9 kiro-cli`.
4. In the TUI, send another prompt.
5. Expected: "brain 'kiro' reconnecting…" trace entry, then "reconnected — state restored" (if kiro persists state) OR "reconnected — state LOST" (if it doesn't). The new prompt should round-trip.
6. Repeat pkill twice more in quick succession to confirm the circuit breaker trips and emits `BrainReconnectFailed`.

No commit for this step.

---

## Self-Review

**1. Spec coverage:**

| Spec section | Tasks |
|---|---|
| B1 (surface LoadOutcome) | Task 1 (enum), Task 2 (plumb through load_brain_session) |
| B2 (inline reconnect helper) | Task 3 (is_connection_death), Task 4 (try_reconnect_brain), Task 5 (integration + circuit breaker) |
| B3 (events + circuit breaker) | Task 1 (event variants), Task 5 (circuit breaker state + emissions) |
| TUI rendering (explicit spec item) | Task 7 |
| Acceptance #1 (Reconnecting → Reconnected within 5s) | Task 6 first test |
| Acceptance #2 (SpurEvent.seq monotonic) | Task 6 first test final assertion |
| Acceptance #3 (circuit breaker) | Task 6 second test |
| Acceptance #4 (LoadOutcome::FellBackToNew path + TUI) | Task 7 (warning path); ancillary test can extend Task 6 if needed |
| Acceptance #5 (cargo test passes + spur-invariants-reviewer passes) | Task 8 |
| Acceptance #6 (manual kiro kill test) | Task 8 Step 5 |
| Invariant sub-task (grace timer) | Task 8 |

**2. Placeholder scan:** every step has concrete code or exact commands. The one intentional contingency is Task 8 Step 2 ("if pre-existing handler is correct, commit nothing") — that's an honest gate, not a placeholder.

**3. Type consistency:**
- `LoadOutcome` defined in Task 1 (spur-acp), consumed in Task 2 (load_brain_session return), Task 4 (try_reconnect_brain return), Task 5 (reconnect_with_events / BrainReconnected.outcome), Task 7 (TUI match).
- `is_connection_death` defined as `pub(crate) fn` in Task 3, called in Task 5.
- `reconnect_failures: VecDeque<Instant>`, `RECONNECT_CIRCUIT_LIMIT`, `RECONNECT_CIRCUIT_WINDOW` defined together in Task 5 Step 1, consumed in Task 5 Step 2's method.
- Three event variants have identical field shapes across Task 1 (definition), Task 5 (emission), Task 7 (match).
- `BrainSession` field list (`connection`, `delegation_handle`, `mcp_handle`, `notification_pump_handle`, `acp_session_id`, `spur_session_id`, `brain_name`) matches the current struct and all three places that move fields out of it.
