# Persisted Plan Reconciler Dispatch Hotfix Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-04-23-persisted-plan-reconciler-dispatch-hotfix-design.md`
**Design epic:** not recorded in this session; create and close a beads design epic before `submit_plan(persist_as_epic=true)`

**Goal:** Restore prompt dispatch for persisted ready tasks on the default production path, then harden the reconciler loop against the two adjacent failure modes discovered during RCA.

**Architecture:** Keep persisted dispatch reconciler-owned. The hotfix makes the server own the effective fast-forward `Notify` when the orchestrator passes `None`, proves that path with end-to-end tests, then adds two local hardening changes in `reconciler.rs`: per-plan projection isolation and journal-monitor retry behavior. Startup stale-dispatch recovery remains compensation-based and is pinned with regression coverage rather than redesigned.

**Tech Stack:** Rust 2021, Tokio, `spur-mcp`, Beads-backed integration tests using `br`, JSON-RPC test helpers on `McpCallbackServer`

---

## Decisions Locked By This Plan

1. Persisted `submit_plan` and `execute_epic` remain non-dispatching handlers. The only actor that enqueues persisted worker requests is the reconciler.
2. The minimal wake fix is server-side default handle ownership, not direct dispatch and not interior-mutable publication from `start()`.
3. The production call site in `orchestrator.rs` stays `set_reconciler_enabled(reconciler_enabled, None)`. The server fix must make that path correct.
4. Startup stale-dispatch cleanup via `resolve_dispatch_orphan(...)` remains authoritative for restart recovery in this patch.
5. The repo-scoped durable reconciler is out of scope for this implementation plan and should become a follow-on design after this hotfix lands.

## File Structure

Files touched by this plan:

- `crates/spur-mcp/src/server.rs`
  - owns reconciler enablement, startup wiring, startup reclaim, and test-only tool helpers
  - tasks `hotfix-2` and `hotfix-3` touch this file
- `crates/spur-mcp/src/plan/reconciler.rs`
  - owns the dispatch loop, per-plan projection flow, and journal monitor
  - tasks `hotfix-4` and `hotfix-5` touch this file
- `crates/spur-mcp/tests/reconciler_tick.rs`
  - best existing integration surface for reconciler-startup and dispatch-loop behavior
  - tasks `hotfix-1`, `hotfix-4`, and `hotfix-5` touch this file
- `crates/spur-mcp/tests/persisted_authority_flip.rs`
  - existing persisted-plan restart-recovery surface
  - task `hotfix-3` touches this file

Out of scope:

- `crates/spur-core/src/orchestrator.rs` except as a referenced call site
- daemonization / repo-scoped reconciler ownership
- any direct changes to ephemeral `run_plan`

## Execution Order / Constraints

1. Run tasks in order. `server.rs` and `reconciler.rs` are hot files and should not be edited in parallel.
2. Keep TDD strict for behavior changes: red test commit first, then fix commit.
3. Preserve the existing contract that persisted handlers do not dispatch directly from the handler body.
4. Use bounded timeouts in end-to-end tests so failures are diagnostic rather than hanging.
5. If a “regression pin” test already passes on current code, commit it as a test-only checkpoint and move on; do not invent a production change just to satisfy red/green theater.

---

### Task 1: Add failing end-to-end tests for the default fast-forward path

**Task ID:** `hotfix-1`

**Files:**
- Modify: `crates/spur-mcp/tests/reconciler_tick.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] A started `McpCallbackServer` configured with `set_reconciler_enabled(true, None)` fails on current code to dispatch persisted ready work within a bounded timeout.
- [ ] Coverage exists for both `submit_plan(persist_as_epic=true)` and `execute_epic(...)`.
- [ ] The existing direct-dispatch contract remains covered by the handler-only tests in `submit_plan_persist.rs` and `e2e_closure_v0e.rs`; the new tests only prove bounded eventual dispatch on the started-server path.

**Suggested Worker:** `claude-code`

**Scope Boundary:**
- IN scope: `reconciler_tick.rs` integration harnesses, temp repo helpers, started server path
- OUT of scope: production code
- If you discover you need production changes just to make the test compile, emit `scope_drift` immediately.

**Implementation:**
- [ ] **Step 1: Write the failing tests**

Add two tests that:

1. create a temp Beads repo
2. create/start `McpCallbackServer`
3. call `set_reconciler_enabled(true, None)`
4. submit persisted work via `__test_call_submit_plan(...)` or `__test_call_execute_epic(...)`
5. keep the existing handler-only “must not dispatch directly” tests untouched
6. wait for `channel.request_rx.recv()` with a bounded timeout such as `Duration::from_secs(1)`

Suggested names:

```rust
#[tokio::test]
async fn submit_plan_default_notify_path_dispatches_ready_task() {}

#[tokio::test]
async fn execute_epic_default_notify_path_dispatches_ready_task() {}
```

- [ ] **Step 2: Run the red tests**

Run:

```bash
cargo test -p spur-mcp submit_plan_default_notify_path_dispatches_ready_task -- --nocapture
cargo test -p spur-mcp execute_epic_default_notify_path_dispatches_ready_task -- --nocapture
```

Expected: FAIL on current code because the default `None` notify path never wakes the real spawned reconciler.

- [ ] **Step 3: Commit the red test**

```bash
git add crates/spur-mcp/tests/reconciler_tick.rs
git commit -m "test(spur-mcp): H1 cover default reconciler wake path"
```

---

### Task 2: Repair the lost wake-handle wiring in `server.rs`

**Task ID:** `hotfix-2`

**Files:**
- Modify: `crates/spur-mcp/src/server.rs`
- Modify: `crates/spur-mcp/tests/reconciler_tick.rs`

**Depends on:** `hotfix-1`

**Acceptance Criteria:**
- [ ] `set_reconciler_enabled(true, None)` materializes and retains a default fast-forward `Notify`.
- [ ] `start()` uses the stored handle and does not create a second private fallback `Notify`.
- [ ] The new end-to-end tests pass, while the existing handler-only no-direct-dispatch tests remain green.

**Suggested Worker:** `claude-code`

**Scope Boundary:**
- IN scope: reconciler enablement wiring and the corresponding tests
- OUT of scope: daemonization, direct dispatch, orchestrator behavior changes
- If the fix appears to require `orchestrator.rs`, stop and re-check the design first.

**Implementation:**
- [ ] **Step 1: Implement the minimal server-owned notify fix**

Refactor `set_reconciler_enabled(...)` so it owns default handle creation:

```rust
pub fn set_reconciler_enabled(
    &mut self,
    enable: bool,
    fast_forward: Option<Arc<tokio::sync::Notify>>,
) {
    self.reconciler_enabled = enable;
    self.reconciler_fast_forward = if enable {
        Some(fast_forward.unwrap_or_else(|| Arc::new(tokio::sync::Notify::new())))
    } else {
        None
    };
}
```

Then update `start()` to reuse `self.reconciler_fast_forward.as_ref().cloned()` directly instead of manufacturing a private fallback.

- [ ] **Step 2: Add a small unit guard if helpful**

If the implementation benefits from a tighter guard, add a focused unit test in `server.rs` that proves `fast_forward_reconciler()` wakes a default-materialized notify after `set_reconciler_enabled(true, None)`.

- [ ] **Step 3: Run the focused green tests**

Run:

```bash
cargo test -p spur-mcp submit_plan_default_notify_path_dispatches_ready_task -- --nocapture
cargo test -p spur-mcp execute_epic_default_notify_path_dispatches_ready_task -- --nocapture
cargo test -p spur-mcp fast_forward_reconciler_uses_configured_notify -- --nocapture
```

Expected: PASS.

- [ ] **Step 4: Commit the fix**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/tests/reconciler_tick.rs
git commit -m "fix(spur-mcp): H2 restore persisted reconciler wake path"
```

---

### Task 3: Pin startup stale-dispatch recovery on the hotfixed path

**Task ID:** `hotfix-3`

**Files:**
- Modify: `crates/spur-mcp/tests/persisted_authority_flip.rs`
- Modify: `crates/spur-mcp/src/server.rs` only if the new regression exposes a real bug

**Depends on:** `hotfix-2`

**Acceptance Criteria:**
- [ ] A persisted task with a stale `spur:delegation-id:*` label and no completion audit is cleared by startup recovery.
- [ ] After recovery, normal reconciler dispatch can proceed on the default `None` notify path.
- [ ] If current code already satisfies this after `hotfix-2`, the task lands as a test-only regression pin.

**Suggested Worker:** `claude-code`

**Scope Boundary:**
- IN scope: startup reclaim / stale-dispatch recovery coverage
- OUT of scope: redesigning orphan recovery semantics
- If the regression reveals that orphan cleanup and new dispatch compete, stop and surface it as a separate issue.

**Implementation:**
- [ ] **Step 1: Add the regression**

Clone the existing persisted restart harness and add a test such as:

```rust
#[tokio::test]
async fn startup_reclaim_clears_stale_dispatch_before_redispatch() {}
```

The fixture should:

1. persist a plan/task
2. add a stale `spur:delegation-id:*` label with no completion audit
3. start `McpCallbackServer` with `set_reconciler_enabled(true, None)`
4. assert startup reclaim clears the stale dispatch marker
5. assert a fresh delegation request is eventually received

- [ ] **Step 2: Run the regression**

Run:

```bash
cargo test -p spur-mcp startup_reclaim_clears_stale_dispatch_before_redispatch -- --nocapture
```

Expected:

- PASS without production changes if current recovery still works after `hotfix-2`, or
- FAIL for a real restart-recovery seam that needs a minimal fix

- [ ] **Step 3: Commit**

If test-only:

```bash
git add crates/spur-mcp/tests/persisted_authority_flip.rs
git commit -m "test(spur-mcp): H3 pin startup dispatch orphan recovery"
```

If a production fix is required:

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/tests/persisted_authority_flip.rs
git commit -m "fix(spur-mcp): H3 preserve startup dispatch recovery"
```

---

### Task 4: Isolate bad-plan projection failures inside a reconciler tick

**Task ID:** `hotfix-4`

**Files:**
- Modify: `crates/spur-mcp/src/plan/reconciler.rs`
- Modify: `crates/spur-mcp/tests/reconciler_tick.rs`

**Depends on:** `hotfix-3`

**Acceptance Criteria:**
- [ ] One malformed or unprojectable ready plan does not abort dispatch for a second valid ready plan in the same tick.
- [ ] The failure is logged and skipped, not silently swallowed.
- [ ] Existing happy-path dispatch tests continue to pass.

**Suggested Worker:** `claude-code`

**Scope Boundary:**
- IN scope: `tick_once()` loop isolation and the corresponding test fixture
- OUT of scope: broader projector redesign
- If this grows beyond one loop body and one integration fixture, stop and re-scope.

**Implementation:**
- [ ] **Step 1: Add a failing isolation test**

Add an integration test that runs `Reconciler::tick_once()` in observe-all-plans mode with:

- one malformed ready task labeled with a bogus `spur:plan-id:*`
- one valid ready task under a real persisted plan

Suggested name:

```rust
#[tokio::test]
async fn tick_once_skips_broken_plan_and_dispatches_other_ready_work() {}
```

- [ ] **Step 2: Run the red test**

Run:

```bash
cargo test -p spur-mcp tick_once_skips_broken_plan_and_dispatches_other_ready_work -- --nocapture
```

Expected: FAIL on current code because `project_plan_from_beads(...)?` aborts the whole tick.

- [ ] **Step 3: Implement the minimal isolation**

Wrap the per-summary projection/lookup body in `match` / `if let Err(error)` logging so one bad plan `continue`s without aborting the entire tick.

- [ ] **Step 4: Verify green**

Run:

```bash
cargo test -p spur-mcp tick_once_skips_broken_plan_and_dispatches_other_ready_work -- --nocapture
cargo test -p spur-mcp submit_plan_default_notify_path_dispatches_ready_task -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/plan/reconciler.rs crates/spur-mcp/tests/reconciler_tick.rs
git commit -m "fix(spur-mcp): H4 isolate broken plan projection"
```

---

### Task 5: Harden journal wake polling against transient metadata errors

**Task ID:** `hotfix-5`

**Files:**
- Modify: `crates/spur-mcp/src/plan/reconciler.rs`
- Modify: `crates/spur-mcp/tests/reconciler_tick.rs`

**Depends on:** `hotfix-4`

**Acceptance Criteria:**
- [ ] `monitor_journal_appends(...)` survives a transient missing-file or metadata-error window.
- [ ] A later append still wakes the watcher.
- [ ] The polling loop remains bounded and does not busy-spin on error.

**Suggested Worker:** `claude-code`

**Scope Boundary:**
- IN scope: journal poller behavior and its test
- OUT of scope: replacing the poller with a filesystem watcher
- If the fix requires introducing a new crate, stop and ask first.

**Implementation:**
- [ ] **Step 1: Add a failing resilience test**

Add a test such as:

```rust
#[tokio::test]
async fn monitor_journal_appends_survives_transient_metadata_error() {}
```

Suggested flow:

1. create a temp journal path
2. start `monitor_journal_appends(...)`
3. remove or rename the file so metadata temporarily fails
4. recreate/append to the journal
5. assert the notify still fires

- [ ] **Step 2: Run the red test**

Run:

```bash
cargo test -p spur-mcp monitor_journal_appends_survives_transient_metadata_error -- --nocapture
```

Expected: FAIL on current code because the loop breaks on metadata error.

- [ ] **Step 3: Implement the minimal retry behavior**

Keep the 250 ms sleep cadence, but treat metadata errors as a retryable state:

```rust
let next_len = match tokio::fs::metadata(&path).await {
    Ok(meta) => meta.len(),
    Err(error) => {
        tracing::debug!(%error, ?path, "journal metadata unavailable; retrying");
        continue;
    }
};
```

Make sure the retry path does not spin without the existing sleep.

- [ ] **Step 4: Verify green**

Run:

```bash
cargo test -p spur-mcp monitor_journal_appends_survives_transient_metadata_error -- --nocapture
cargo test -p spur-mcp --test reconciler_tick -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/plan/reconciler.rs crates/spur-mcp/tests/reconciler_tick.rs
git commit -m "fix(spur-mcp): H5 harden journal wake polling"
```

---

## Final Verification

After `hotfix-5`, run the focused persisted-plan regression suite:

```bash
cargo test -p spur-mcp --test reconciler_tick -- --nocapture
cargo test -p spur-mcp --test persisted_authority_flip -- --nocapture
cargo test -p spur-mcp --test submit_plan_persist -- --nocapture
```

Expected: PASS.

If any of these fail because the durable lifecycle model itself is leaking through, stop and write the repo-scoped reconciler follow-on spec before attempting more production fixes.
