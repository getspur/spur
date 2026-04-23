# Persisted Plan Reconciler Dispatch Hotfix Design

**Date:** 2026-04-23
**Status:** approved for implementation
**Drivers:**
- [docs/rca/2026-04-22-persisted-plan-control-loop-grounding.md](/Volumes/Projects/spur/docs/rca/2026-04-22-persisted-plan-control-loop-grounding.md:1)
- [docs/superpowers/specs/2026-04-22-persisted-plan-state-model-hardening-design.md](/Volumes/Projects/spur/docs/superpowers/specs/2026-04-22-persisted-plan-state-model-hardening-design.md:1)

**Related code:**
- `crates/spur-mcp/src/server.rs`
- `crates/spur-mcp/src/plan/reconciler.rs`
- `crates/spur-mcp/tests/reconciler_tick.rs`
- `crates/spur-core/src/orchestrator.rs`

---

## Goal

Restore prompt dispatch for persisted ready plan tasks without changing the core ownership model of dispatch in this patch.

The hotfix covers four bounded outcomes:

1. make `submit_plan` / `execute_epic` wake the actual running reconciler on the default production path
2. prove the fix with end-to-end tests that exercise the real server startup wiring
3. preserve and pin stale-dispatch recovery so restart remains compensating rather than destructive
4. harden two adjacent liveness hazards that can still suppress dispatch after the wake path is repaired

---

## Non-Goals

- no direct dispatch from `submit_plan` or `execute_epic` handlers
- no plan-engine rewrite or PM authority flip
- no repo-scoped daemon or out-of-process reconciler in this patch
- no change to ephemeral `run_plan` execution
- no new backend abstraction beyond what existing `PmService` / Beads paths already provide

---

## Current Problem

The current failure is not that the reconciler rejects ready work. The failure is earlier:

1. persisted plan submission writes the beads graph successfully
2. ready detection works
3. the explicit wake from the handler never reaches the spawned reconciler
4. dispatch therefore depends on slower or brittle fallback paths

Grounded evidence:

- Orchestrator always wires `set_reconciler_enabled(reconciler_enabled, None)` on the production path.
- `McpCallbackServer::fast_forward_reconciler()` only notifies `self.reconciler_fast_forward`.
- `McpCallbackServer::start()` creates a private `Notify` when that field is `None`, then passes the private value into `Reconciler::new(...)`.
- The private `Notify` is not written back to the server, so the handler wake path is dead by construction.
- Live beads state for the stalled plan showed ready tasks with no `spur:delegation-id:*` label and no dispatch audit comment, which means `persist_dispatch_intent(...)` never ran.

Secondary hazards remain even after the lost wake is fixed:

- `tick_once()` currently aborts the whole tick if `project_plan_from_beads(...)` fails for one ready summary.
- `monitor_journal_appends(...)` exits on any metadata error, so a transient journal-state blip can permanently disable journal-triggered wakeups.
- The reconciler still dies with the brain session because it is owned by `McpCallbackServer::start()`.

That last point is real, but it is a follow-on architecture problem rather than the minimal hotfix target.

---

## Approaches Considered

### Option A: Directly dispatch from `submit_plan` / `execute_epic`

This is the wrong fix.

It would hide the wake-path bug by bypassing the reconciler and would fork persisted execution semantics between:

- handler-time dispatch
- reconciler-time dispatch
- startup recovery / replay dispatch

That would reopen the same RAM-vs-Beads authority split the persisted-plan work was specifically trying to close.

### Option B: Keep reconciler ownership, but make the server own the effective wake handle

This is the recommended option.

If the caller enables the reconciler with `None`, the server should materialize and retain a default `Notify` at configuration time. `start()` then reuses that stored handle instead of manufacturing a private one.

This keeps the current architecture intact:

- handlers still only persist + wake
- reconciler still owns dispatch
- startup reclaim still uses the same wake channel
- tests can finally cover the real production path

### Option C: Store the private `Notify` back into the server from `start()`

This would work, but it is clumsier than Option B because `start()` already runs on shared server state and would need additional interior mutability just to publish the synthesized handle.

It also makes the invariant less obvious: the configured wake handle and the running wake handle should be the same object before `start()` begins.

### Option D: Jump straight to a repo-scoped reconciler daemon

This is the right long-term direction, but it is too large for the immediate incident.

The current production outage is a dead wake path plus weak regression coverage. The daemon design should follow after the hotfix has restored dispatch and covered the seam with executable tests.

---

## Chosen Design

### 1. Server-owned default fast-forward channel

When `set_reconciler_enabled(true, None)` is called, `McpCallbackServer` should create and store a default `Arc<Notify>` immediately.

Rules:

- if the caller passes `Some(notify)`, preserve it exactly
- if the caller passes `None` and `enable == true`, materialize one default `Notify` and retain it on the server
- if `enable == false`, clear the stored wake handle
- `start()` must use the server-stored handle and must not create a second private fallback channel

This is the smallest change that makes `fast_forward_reconciler()` and the spawned reconciler talk to the same object.

### 2. Handler contract remains asynchronous

Persisted handlers still must not enqueue worker requests directly.

Rules:

- `submit_plan(persist_as_epic=true)` continues to persist the graph, install in-memory projection state, and wake the reconciler
- `execute_epic(...)` continues to persist scope labels, install projected state, and wake the reconciler
- worker dispatch remains exclusively in the reconciler path

This preserves the architecture: handlers record intent, the reconciler performs action.

### 3. Recovery remains compensation-based and must stay proven

The hotfix must not accidentally regress startup cleanup for stale dispatch labels.

Rules:

- startup recovery continues to call `resolve_dispatch_orphan(...)` during persisted-plan reclaim
- the test suite must pin that a stale `spur:delegation-id:*` label with no matching completion audit is cleared before the plan is allowed back into normal dispatch flow
- the hotfix must not introduce a second recovery path that disagrees with `resolve_dispatch_orphan(...)`

### 4. Tick isolation and journal resilience are phase-two hardening in the same patch train

These are not the incident root cause, but they are close enough to the same dispatch loop that they should be fixed while the code is already under test.

Rules:

- one malformed or partially-projectable ready plan must not abort dispatch for other ready plans in the same tick
- transient metadata failure while polling `.beads/journal` must not permanently kill journal-triggered wakeups
- these hardening changes stay local to `reconciler.rs` and its tests

### 5. Durable reconciler remains a follow-on design

This document explicitly does not claim that the hotfix solves durable execution fully.

After the hotfix lands, the next design item is a repo-scoped reconciler lifecycle that survives brain-session churn.

---

## Component Changes

### `crates/spur-mcp/src/server.rs`

- make `set_reconciler_enabled(...)` own the default wake-handle materialization
- remove the private fallback `Notify` creation from `start()`
- keep `fast_forward_reconciler()` unchanged in contract, but ensure it now targets the real running reconciler
- preserve startup reclaim behavior and its post-start wake

### `crates/spur-mcp/src/plan/reconciler.rs`

- keep dispatch ownership in `tick_once()`
- isolate per-plan projection failure so one bad plan does not starve the rest of the ready queue
- harden `monitor_journal_appends(...)` so transient metadata failures do not terminate the polling loop

### `crates/spur-core/src/orchestrator.rs`

- no behavioral redesign in this patch
- keep `set_reconciler_enabled(reconciler_enabled, None)` as the production call site; the server-side defaulting fix should make that path work

### Tests

- add e2e coverage that starts the real callback server with the production-style `None` notify path
- add stale-dispatch recovery regression coverage on startup reclaim
- add unit/integration coverage for per-plan tick isolation and journal-monitor resilience

---

## Error Handling

- waking a disabled reconciler remains a no-op
- failing to project one ready plan must log and continue, not abort the whole tick
- journal-monitor errors should degrade to retrying polling, not silently exiting
- stale dispatch labels without completion audits remain recoverable via `resolve_dispatch_orphan(...)`

---

## Verification

At minimum:

- a failing test that proves persisted `submit_plan` does not dispatch directly from the handler, but does dispatch promptly once the server is started with reconciler enabled on the default `None` path
- the same bounded-dispatch proof for `execute_epic`
- a startup recovery test that clears stale dispatch intent before normal dispatch resumes
- a test that one bad projected plan does not starve dispatch for a second good plan
- a test that journal monitoring survives a transient metadata failure and still wakes on a later append

Relevant suites to extend:

- `cargo test -p spur-mcp --test reconciler_tick -- --nocapture`
- `cargo test -p spur-mcp --test persisted_authority_flip -- --nocapture`
- `cargo test -p spur-mcp --test submit_plan_persist -- --nocapture`

---

## Expected Outcome

After this hotfix:

- persisted ready tasks dispatch promptly on the real production wiring
- the test suite covers the exact server/reconciler seam that failed in production
- startup recovery for stale dispatch intent stays intact and explicit
- the dispatch loop is less fragile in the face of bad plan projection and journal polling glitches

After this hotfix, but not because of it:

- the system still needs a durable reconciler lifecycle design if persisted plans are expected to make progress independently of an active brain session
