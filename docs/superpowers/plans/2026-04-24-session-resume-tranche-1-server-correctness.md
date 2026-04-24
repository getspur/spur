# Session Resume — Tranche 1: Server Correctness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Spec:** `docs/superpowers/specs/2026-04-24-session-resume-optimistic-nav-design.md`

**Goal:** Fix the two server-side correctness defects that cause the session-load halt: `BrainError` events on the resume path carry a random `SessionId::new()` instead of the real session id, and MCP guard teardown has two unbounded `.await` points that can hang the resume pipeline forever.

**Architecture:** Surgical edits inside `crates/spur-core/src/orchestrator.rs`. Two emit sites get the real `session_id` threaded through; two `guard.await` sites get wrapped in `tokio::time::timeout(MCP_SHUTDOWN_TIMEOUT, ...)`. No public API changes, no new event variants, no UX changes. This tranche is independently shippable; it makes the backend honest even without the Tranche 2 UX redesign.

**Tech Stack:** Rust 2021, Tokio, existing `SpurEventBody` schema in `spur-acp`.

**Out of scope for this tranche (follow-up beads):**
- Line 1737 has the same `SessionId::new()` defect on the non-resume brain-spawn-failure path. Fix identical in shape but different code path; file as separate follow-up issue.
- `app.rs:1564` `TrySendError::Full` handling.
- `native.rs:478` ACP `reply_rx.await` 5s timeout.

---

### Task 1: Regression test — faithful `BrainError.session` on resume-connect failure

**Files:**
- Test: `crates/spur-core/tests/brain_error_session_correlation.rs` (CREATE)

**Context:** This test locks in FP-5 ("event correlation requires faithful identity") for the resume path. It drives the orchestrator through a resume where `connect_brain` fails, captures the emitted `BrainError`, and asserts the event's `session` field equals the session id the user asked to resume — NOT a freshly generated one.

- [ ] **Step 1: Inspect existing test helpers so the test uses the same fixtures**

Run: `ls crates/spur-core/tests/` and read any file whose name contains `resume`, `brain`, or `continuation` to understand the existing test-harness helpers (e.g. `build_orchestrator`, `MockAcpConnection`, `spawn_orchestrator`). Use whichever helpers already exist for building an orchestrator instance plus a failing `connect_brain` fake. Do not invent new harness infrastructure.

Expected: you will find helpers that let you construct an `Orchestrator` with an injectable brain-connect callback.

- [ ] **Step 2: Write the failing test**

Create `crates/spur-core/tests/brain_error_session_correlation.rs` with a test that:
1. Constructs an orchestrator wired to a brain-connect fake that returns `Err(...)`.
2. Subscribes to the orchestrator's `SpurEvent` broadcast.
3. Sends `InteractiveInput::ResumeSession { session_id: TARGET }` where `TARGET` is a known `SessionId`.
4. Drives the orchestrator loop one tick (or uses whatever step function the harness exposes).
5. Filters broadcast events for `SpurEventBody::BrainError { session, .. }`.
6. Asserts `session == TARGET`.

Concrete skeleton (adapt imports/helpers to match what actually exists — do not add dependencies):

```rust
// crates/spur-core/tests/brain_error_session_correlation.rs

use spur_acp::domain::events::{SessionId, SpurEventBody};
use spur_core::orchestrator::InteractiveInput;

// Import whatever harness helpers already exist in sibling test files.
// If the harness is not in a shared module but duplicated per-file, copy
// the smallest subset needed here. Do NOT refactor the harness in this PR.

#[tokio::test]
async fn brain_error_on_resume_connect_failure_carries_requested_session_id() {
    // 1. Known session id the user asked to resume.
    let target = SessionId::from("target-session-under-test");

    // 2. Build orchestrator with a brain-connect that fails.
    //    Pattern: the existing harness should expose a way to inject
    //    a `connect_brain` result. If it doesn't, you must extend the
    //    harness minimally in this task (one small helper, same file).
    let (orch, mut events) = build_orchestrator_with_failing_connect(
        "simulated connect failure".to_string(),
    );

    // 3. Send ResumeSession.
    orch.send_input(InteractiveInput::ResumeSession {
        session_id: target.clone(),
    })
    .await;

    // 4. Collect events until we see BrainError or time out.
    let error_event = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        async {
            loop {
                let ev = events.recv().await.expect("broadcast closed");
                if let SpurEventBody::BrainError { session, message } = &ev.body {
                    return (session.clone(), message.clone());
                }
            }
        },
    )
    .await
    .expect("BrainError never emitted");

    // 5. The fix lives here: session must equal what the user requested.
    assert_eq!(
        error_event.0, target,
        "BrainError.session must carry the resume target, not SessionId::new()"
    );
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p spur-core --test brain_error_session_correlation -- --nocapture`

Expected output: test FAILS with an assertion diff showing `session` is some random UUID rather than `"target-session-under-test"`. That confirms the bug.

If the test fails to compile because `build_orchestrator_with_failing_connect` doesn't exist, create the minimum helper inside the test file itself (do NOT add it to `src/`). Keep it private to this file.

- [ ] **Step 4: Commit the failing test**

```bash
git add crates/spur-core/tests/brain_error_session_correlation.rs
git commit -m "test(spur-core): guard BrainError.session correlation on resume"
```

---

### Task 2: Fix `BrainError` emit site at `orchestrator.rs:1357-1360` (connect-brain failure)

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:1357-1360`

**Context:** In the `InteractiveInput::ResumeSession` match arm, after `connect_brain` fails, the current code emits `BrainError { session: SessionId::new(), message: error_message }`. The requested `session_id` is in scope (from the match binding at line 1336 — re-verify this when editing, the binding may be named `session_id` already or require a `.clone()`).

- [ ] **Step 1: Re-verify the in-scope session id**

Read `crates/spur-core/src/orchestrator.rs` lines 1336–1370. Confirm the `ResumeSession` arm binds the requested id as `session_id: SessionId` or similar. That local is what the fix must use.

- [ ] **Step 2: Apply the fix**

Replace:

```rust
                                        self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                            session: SessionId::new(),
                                            message: error_message,
                                        }));
```

with:

```rust
                                        self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                            session: session_id.clone(),
                                            message: error_message,
                                        }));
```

If the match binding is named something other than `session_id`, use that name instead. Do not rename the binding; just reference the existing local.

- [ ] **Step 3: Run the regression test to verify it now passes**

Run: `cargo test -p spur-core --test brain_error_session_correlation -- --nocapture`

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "fix(spur-core): thread real session id into BrainError on resume-connect failure"
```

---

### Task 3: Fix `BrainError` emit site at `orchestrator.rs:1430-1433` (load-session failure)

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:1430-1433`
- Extend: `crates/spur-core/tests/brain_error_session_correlation.rs` (add a second case)

**Context:** The second resume-path `BrainError` site fires when `load_brain_session` returns `Err(...)`. Same defect, same fix shape. Also add a second regression test case for this specific failure path to prevent regression.

- [ ] **Step 1: Add a failing-load test case**

Append to `crates/spur-core/tests/brain_error_session_correlation.rs`:

```rust
#[tokio::test]
async fn brain_error_on_resume_load_failure_carries_requested_session_id() {
    let target = SessionId::from("target-load-failure");

    // Fake: connect_brain succeeds, but load_brain_session fails.
    let (orch, mut events) = build_orchestrator_with_failing_load(
        "simulated load failure".to_string(),
    );

    orch.send_input(InteractiveInput::ResumeSession {
        session_id: target.clone(),
    })
    .await;

    let error_event = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        async {
            loop {
                let ev = events.recv().await.expect("broadcast closed");
                if let SpurEventBody::BrainError { session, message } = &ev.body {
                    return (session.clone(), message.clone());
                }
            }
        },
    )
    .await
    .expect("BrainError never emitted");

    assert_eq!(
        error_event.0, target,
        "BrainError.session must carry the resume target on load failure too"
    );
}
```

If `build_orchestrator_with_failing_load` doesn't exist, add it in the same file next to the failing-connect helper. Keep helpers private to this test file.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p spur-core --test brain_error_session_correlation brain_error_on_resume_load_failure_carries_requested_session_id -- --nocapture`

Expected: FAIL with `session` != `target-load-failure`.

- [ ] **Step 3: Re-verify the in-scope session id at line 1430**

Read `crates/spur-core/src/orchestrator.rs` lines 1366–1433. The resume arm clones `session_id` into `original_session_id` at (roughly) line 1367. Use whichever local is in scope at the emit site — `session_id`, `original_session_id`, or re-introduce a clone if the original was consumed.

- [ ] **Step 4: Apply the fix**

Replace:

```rust
                                self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                    session: SessionId::new(),
                                    message: error_message,
                                }));
```

with:

```rust
                                self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                    session: original_session_id.clone(),
                                    message: error_message,
                                }));
```

If `original_session_id` has been moved, clone `session_id` directly instead — whichever local still holds the requested id at that point in the arm.

- [ ] **Step 5: Run both test cases to verify they pass**

Run: `cargo test -p spur-core --test brain_error_session_correlation -- --nocapture`

Expected: both tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs crates/spur-core/tests/brain_error_session_correlation.rs
git commit -m "fix(spur-core): thread real session id into BrainError on resume-load failure"
```

---

### Task 4: Regression test — bounded `shutdown_mcp_server` with stuck guard

**Files:**
- Test: `crates/spur-core/tests/shutdown_mcp_server_bounded.rs` (CREATE)

**Context:** This test locks in FP-3 ("every state has a bounded exit"). It calls `shutdown_mcp_server` (or a small wrapper the test needs, see Step 2) with a `RetirableMcpServer` fake whose `shutdown()` future resolves instantly AND an `AbortOnDropHandle<()>` whose underlying task never completes. It then asserts the function returns within `MCP_SHUTDOWN_TIMEOUT + epsilon`.

Because `shutdown_mcp_server` is a private module function, the test must either:
- (a) expose it via `pub(crate)` + a new `#[cfg(test)] pub fn` re-export in `orchestrator.rs`, or
- (b) exercise the behaviour via the public `retire_active_brain` entry point.

Prefer (b) if the existing harness supports it; prefer (a) only if (b) is prohibitively complex.

- [ ] **Step 1: Decide entry point**

Read `crates/spur-core/src/orchestrator.rs` around `retire_brain_session` (line 256) and `retire_active_brain`. If either is reachable from the test harness already used in Task 1, use it. Otherwise, expose `shutdown_mcp_server` via `pub(crate)` and add a dedicated `#[doc(hidden)] #[cfg(any(test, feature = "test-exports"))] pub fn __test_shutdown_mcp_server(...) -> ...` wrapper. Keep the wrapper minimal; do not restructure real code.

- [ ] **Step 2: Write the failing test**

Create `crates/spur-core/tests/shutdown_mcp_server_bounded.rs`:

```rust
use std::sync::Arc;
use std::time::{Duration, Instant};

use spur_acp::domain::events::SessionId;
use spur_core::event_funnel::FunnelHandle;

// Fake MCP server whose shutdown() resolves immediately — this isolates
// the bug to the guard.await line, not the server.shutdown() line.
struct InstantShutdownServer;

impl spur_core::orchestrator::RetirableMcpServer for InstantShutdownServer {
    fn mark_retiring(&self) {}
    fn cancel_in_flight_workers(&self) {}
    fn force_abort(&self) {}
    fn shutdown(&self) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = ()> + Send + '_>,
    > {
        Box::pin(async {})
    }
}

#[tokio::test]
async fn shutdown_mcp_server_returns_even_if_guard_task_hangs() {
    // Guard wraps a task that never finishes — simulates a stuck guard.
    let stuck_task = tokio::spawn(async {
        futures::future::pending::<()>().await;
    });
    let guard = tokio_util::task::AbortOnDropHandle::new(stuck_task);

    let mut server: Option<Arc<dyn spur_core::orchestrator::RetirableMcpServer>> =
        Some(Arc::new(InstantShutdownServer));
    let mut guard_slot: Option<tokio_util::task::AbortOnDropHandle<()>> =
        Some(guard);

    let (funnel, _rx) = FunnelHandle::test_channel(); // use whatever the
        // existing test helpers provide; if none, construct the simplest
        // FunnelHandle that matches `shutdown_mcp_server`'s signature.

    let session = SessionId::from("under-test");

    let started = Instant::now();
    // Use whichever entry point you chose in Step 1.
    spur_core::orchestrator::__test_shutdown_mcp_server(
        &funnel,
        &session,
        &mut server,
        Some(&mut guard_slot),
    )
    .await;
    let elapsed = started.elapsed();

    // Must return well within MCP_SHUTDOWN_TIMEOUT (5s) + epsilon.
    assert!(
        elapsed < Duration::from_millis(5_500),
        "shutdown_mcp_server hung on stuck guard: took {:?}",
        elapsed
    );
}
```

Replace the `FunnelHandle::test_channel()` and `__test_shutdown_mcp_server` calls with whatever the Step 1 decision dictates.

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p spur-core --test shutdown_mcp_server_bounded -- --nocapture`

Expected: FAIL — the test hangs past 5.5s because `guard.await` at `orchestrator.rs:251` waits for the pending task forever. Tokio's test timeout (default 60s) will eventually kill it; confirm the failure is a timeout, not a compile error.

If the test compiles-fails (e.g. `RetirableMcpServer` is private), revise Step 1's entry-point choice or temporarily expose the trait via `pub(crate)`. Do not skip this step.

- [ ] **Step 4: Commit the failing test**

```bash
git add crates/spur-core/tests/shutdown_mcp_server_bounded.rs crates/spur-core/src/orchestrator.rs
git commit -m "test(spur-core): guard bounded MCP shutdown on stuck guard task"
```

---

### Task 5: Wrap both `guard.await` sites in `MCP_SHUTDOWN_TIMEOUT`

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:220-226` (early-return branch)
- Modify: `crates/spur-core/src/orchestrator.rs:249-253` (post-shutdown branch)

**Context:** Both `guard.await` sites are unbounded. Wrap each in `tokio::time::timeout(MCP_SHUTDOWN_TIMEOUT, ...)`. On elapse, log a structured warning keyed on `session` and drop the guard (`AbortOnDropHandle`'s `Drop` aborts the task, so nothing leaks).

- [ ] **Step 1: Apply the fix at the early-return branch (line 220-226)**

Replace:

```rust
    let Some(server) = mcp_server.take() else {
        if let Some(mcp_guard) = mcp_guard {
            if let Some(guard) = mcp_guard.take() {
                let _ = guard.await;
            }
        }
        return;
    };
```

with:

```rust
    let Some(server) = mcp_server.take() else {
        if let Some(mcp_guard) = mcp_guard {
            if let Some(guard) = mcp_guard.take() {
                if tokio::time::timeout(MCP_SHUTDOWN_TIMEOUT, guard)
                    .await
                    .is_err()
                {
                    warn!(
                        session = %session,
                        timeout_ms = MCP_SHUTDOWN_TIMEOUT.as_millis() as u64,
                        "MCP guard await exceeded timeout on early-return; aborting via drop"
                    );
                }
            }
        }
        return;
    };
```

- [ ] **Step 2: Apply the fix at the post-shutdown branch (line 249-253)**

Replace:

```rust
    if let Some(mcp_guard) = mcp_guard {
        if let Some(guard) = mcp_guard.take() {
            let _ = guard.await;
        }
    }
```

with:

```rust
    if let Some(mcp_guard) = mcp_guard {
        if let Some(guard) = mcp_guard.take() {
            if tokio::time::timeout(MCP_SHUTDOWN_TIMEOUT, guard)
                .await
                .is_err()
            {
                warn!(
                    session = %session,
                    timeout_ms = MCP_SHUTDOWN_TIMEOUT.as_millis() as u64,
                    "MCP guard await exceeded timeout post-shutdown; aborting via drop"
                );
            }
        }
    }
```

Both edits reuse the existing `MCP_SHUTDOWN_TIMEOUT` constant (orchestrator.rs:186). No new constant, no new event variant — the outer function already emits `SpurEventBody::McpShutdownTimeout` at line 241–244 for the server-shutdown path; the guard-timeout is a narrower safety net that only needs a log.

- [ ] **Step 3: Run the regression test to verify it passes**

Run: `cargo test -p spur-core --test shutdown_mcp_server_bounded -- --nocapture`

Expected: PASS. Elapsed time is ~5s (within MCP_SHUTDOWN_TIMEOUT + epsilon), not ~60s (tokio default timeout).

- [ ] **Step 4: Run the full spur-core test suite to verify nothing else broke**

Run: `cargo test -p spur-core`

Expected: all tests pass. Pay attention to any test named `*retire*`, `*shutdown*`, `*mcp*`, or `*brain*` — if anything newly fails, it means a consumer depended on the unbounded wait. Investigate (the spec blast-radius review said zero consumers depend on the wait, but verify).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "fix(spur-core): bound guard.await in shutdown_mcp_server at MCP_SHUTDOWN_TIMEOUT"
```

---

### Task 6: Full-workspace sanity and file the follow-up issue

- [ ] **Step 1: Run the full workspace build + tests**

Run: `cargo test --workspace --no-fail-fast`

Expected: all tests pass.

- [ ] **Step 2: Run clippy at workspace level**

Run: `cargo clippy --workspace --all-targets -- -D warnings`

Expected: zero warnings. If the `warn!` log lines introduced in Task 5 trip any `tracing` attribute lints, apply the minimum fix (e.g. include the field macros exactly as the existing `warn!` at line 236-240 uses them).

- [ ] **Step 3: File the follow-up issue for line 1737**

Create a beads issue titled `BrainError at orchestrator.rs:1737 uses SessionId::new() on fresh-spawn failure`. Body:

> Same defect pattern as the two fixed in the resume-tranche-1 PR (commit <short-sha>). Site at `crates/spur-core/src/orchestrator.rs:1737` emits `BrainError { session: SessionId::new(), ... }` when brain spawn fails on the non-resume flow. Fix: thread the session id in scope (or `SessionId::from("<pre-spawn>")` sentinel if no id is available pre-spawn) into the event. Scope: ~4 LOC. Acceptance: BrainError for fresh-spawn failure carries a stable/traceable session id, not `SessionId::new()`.

Use whatever beads CLI / MCP tool your team uses. If unsure, paste the above into a `docs/superpowers/followups/` markdown stub and note "beads issue TBD."

- [ ] **Step 4: Final commit (if Step 3 created files)**

```bash
git add docs/superpowers/followups/  # only if you used the fallback
git commit -m "docs(followup): note BrainError fresh-spawn failure site for later fix"
```

---

## Self-review — completed during authoring

- **Spec coverage:** Every item in Tranche 1 of the spec is covered — faithful `BrainError` on the two resume-path sites (Tasks 2 + 3) and bounded `guard.await` at both sites (Task 5). Out-of-scope items from the spec are deferred to follow-up issues, not silently dropped.
- **Placeholder scan:** No TBD/TODO. Every code block contains the actual edit. Test code blocks have concrete assertions and imports.
- **Type consistency:** `SessionId`, `SpurEventBody::BrainError`, `MCP_SHUTDOWN_TIMEOUT`, `AbortOnDropHandle<()>`, `RetirableMcpServer` — names used in tests match live code. The test helper names (`build_orchestrator_with_failing_connect` / `build_orchestrator_with_failing_load` / `__test_shutdown_mcp_server`) are flagged in-task as "add if not present" so an executor does not assume they exist.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-24-session-resume-tranche-1-server-correctness.md`. Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks.
2. **Inline Execution** — execute tasks in this session with checkpoints.

Which approach?
