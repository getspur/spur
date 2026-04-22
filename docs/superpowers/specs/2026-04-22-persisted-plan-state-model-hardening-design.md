# Persisted Plan State-Model Hardening Design

**Date:** 2026-04-22
**Status:** approved for implementation
**Drivers:** [docs/rca/2026-04-22-persisted-plan-control-loop-grounding.md](/Volumes/Projects/spur/docs/rca/2026-04-22-persisted-plan-control-loop-grounding.md:1)

---

## Goal

Close the remaining persisted-plan seam defects without reopening the old RAM-vs-Beads authority split.

The change set covers three bounded areas:

1. durable persisted-plan terminal semantics
2. stronger cache-vs-authority boundaries around `active_plans`
3. failure-path recovery hardening for dispatch/completion orphan windows

---

## Non-Goals

- no new authority flip away from Beads
- no broad plan-engine rewrite
- no forced removal of legacy projector compatibility in one step
- no change to direct lightweight `delegate_to_worker` brainstorming flows

---

## Current Problem

The persisted path is already fundamentally correct: Beads is durable truth, the reconciler dispatches persisted work, and the brain reviews projected persisted state.

The remaining gaps are seam gaps:

- persisted plans emit `PlanReadyToMerge` on the durable path, but `PlanCompleted` still comes from ephemeral execution
- persisted reads are mostly routed through `load_or_project_plan`, but `active_plans` still looks like a plausible authority object
- dispatch/completion recovery is compensation-based and needs stronger invariants and coverage
- projector compatibility with legacy labels should stay narrow and explicit

---

## Approaches Considered

### Option A: Minimal test-only hardening

Add tests around the current behavior and leave lifecycle/cache semantics unchanged.

This is too weak. It documents the seams but does not reduce the chance of future semantic drift.

### Option B: Targeted state-model hardening

Add a durable persisted terminal event path, make persisted reads project-first by construction, and harden compensation/recovery with focused invariants and tests.

This is the recommended option. It closes the highest-risk seams without broadening scope or changing the direct delegation model.

### Option C: Full state-machine rewrite

Replace `active_plans`, projector compatibility, and reconciler transitions with a new persisted-only state engine.

This is too expensive for the current gap. The RCA does not justify a rewrite.

---

## Chosen Design

### 1. Persisted lifecycle symmetry

Persisted plans gain a durable terminal event path alongside the existing durable ready-to-merge path.

Rules:

- `PlanReadyToMerge` remains the durable "all approved and integration pending" signal
- `PlanCompleted` is emitted for persisted plans when the projected plan reaches terminal closure, regardless of whether the close path is mergeable
- the durable emitter lives on the reconciler/closure path, not the ephemeral executor
- event emission must be idempotent across recovery/replay, using existing durable closure/audit state to avoid duplicate terminal events

This makes persisted plans and brain continuations reason about terminal state with the same vocabulary as ephemeral plans.

### 2. Cache-vs-authority boundary

`active_plans` remains a projection cache, but the code should make that fact harder to violate.

Rules:

- all persisted reads go through one loader that projects from Beads for persisted plans
- direct cache reads for persisted plans are removed or narrowed behind helper APIs
- helper naming should make the distinction obvious: ephemeral plans may be served directly from memory; persisted plans must be projected/refreshed
- tests should prove that stale persisted cache entries do not win over Beads state

This does not delete `active_plans`; it turns it into an implementation detail instead of a tempting authority surface.

### 3. Recovery hardening

The dispatch/completion path stays compensation-based, but the invariant surface becomes explicit and tested.

Rules:

- dispatch intent write, send failure handling, completion writeback, and orphan clearing each get targeted regression coverage
- restart recovery must be able to project persisted state, clear stale dispatch markers, and leave the plan readable through the normal projection path
- where atomicity is unavailable, invariants must fail loudly in tests instead of relying on implicit behavior

### 4. Projector cleanup

Legacy compatibility stays in place only where migration still requires it.

Rules:

- compatibility helpers stay localized in projector code
- new code should not add new callers that reason directly on legacy labels
- tests should pin namespaced labels as canonical behavior and legacy labels as compatibility behavior

---

## Component Changes

### `crates/spur-mcp/src/plan/reconciler.rs`

- extend durable closure handling to emit `PlanCompleted` for persisted terminal plans
- preserve current `PlanReadyToMerge` behavior
- guard emission so replay/recovery does not spam terminal lifecycle events

### `crates/spur-mcp/src/server.rs`

- narrow persisted-plan access through projection-aware helpers
- keep ephemeral-plan access fast and in-memory
- strengthen startup recovery / load paths so projected state is the only persisted read surface

### `crates/spur-mcp/src/plan/projector.rs`

- isolate legacy compatibility in small helpers
- add tests that distinguish canonical namespaced labels from compatibility acceptance

### `crates/spur-mcp/src/plan/mod.rs`

- keep `run_plan` ephemeral-only
- ensure helper boundaries reflect that persisted terminal emission does not belong here

### Tests

- add focused persisted lifecycle event coverage
- add persisted cache-boundary coverage
- add dispatch/completion orphan recovery coverage

---

## Error Handling

- replay/recovery must not duplicate terminal lifecycle events
- failed send after persisted dispatch intent must remain recoverable by orphan cleanup
- persisted reads should fail with a projection/load error, not silently fall back to stale cache state

---

## Verification

At minimum:

- targeted `spur-mcp` tests covering persisted lifecycle events
- targeted `spur-mcp` tests covering projection over stale cache
- targeted `spur-mcp` tests covering orphan dispatch/completion recovery

Regression suites already relevant:

- `cargo test -p spur-mcp --test e2e_closure_v0e -- --nocapture`
- `cargo test -p spur-mcp --test persisted_authority_flip -- --nocapture`

---

## Expected Outcome

After this work:

- persisted plans have a durable terminal lifecycle story
- the codebase makes it materially harder to treat `active_plans` as truth
- recovery semantics are backed by executable tests, not just architectural intent
- the projector remains migration-safe without spreading label ambiguity further
