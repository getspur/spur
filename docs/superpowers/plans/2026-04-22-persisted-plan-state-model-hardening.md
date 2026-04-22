# Persisted Plan State-Model Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the remaining persisted-plan seam defects by adding durable terminal semantics, enforcing projection-first persisted reads, and hardening orphan recovery coverage.

**Architecture:** Persisted plans remain Beads-authoritative. The reconciler owns durable terminal lifecycle emission, `active_plans` stays a projection cache behind projection-aware helpers, and failure-path recovery is tightened with focused invariant tests rather than a plan-engine rewrite.

**Tech Stack:** Rust 2021, Tokio, `spur-mcp`, `spur-acp`, Beads-backed PM integration tests

---

### Task 1: Persisted lifecycle signaling

**Files:**
- Modify: `crates/spur-mcp/src/plan/reconciler.rs`
- Modify: `crates/spur-acp/src/domain/events.rs`
- Test: `crates/spur-mcp/tests/persisted_authority_flip.rs`

- [ ] **Step 1: Add a failing persisted terminal-event test**

Add a test that closes a persisted plan on the durable path and asserts `PlanCompleted` is emitted once from the reconciler path.

- [ ] **Step 2: Run the targeted test and verify it fails for the right reason**

Run: `cargo test -p spur-mcp persisted_plan_emits_terminal_event_from_reconciler -- --nocapture`

Expected: FAIL because persisted closure emits `PlanReadyToMerge` only.

- [ ] **Step 3: Implement durable terminal-event emission**

Emit `SpurEventBody::PlanCompleted` from the persisted closure path in the reconciler, with idempotence tied to durable closure state.

- [ ] **Step 4: Re-run the targeted test**

Run: `cargo test -p spur-mcp persisted_plan_emits_terminal_event_from_reconciler -- --nocapture`

Expected: PASS.

### Task 2: Projection-first persisted reads

**Files:**
- Modify: `crates/spur-mcp/src/server.rs`
- Test: `crates/spur-mcp/tests/persisted_authority_flip.rs`

- [ ] **Step 1: Add a failing stale-cache test**

Add a test that inserts a stale persisted entry into `active_plans`, mutates Beads state, and asserts the public persisted read path returns projected Beads state rather than cached RAM state.

- [ ] **Step 2: Run the targeted test and verify it fails**

Run: `cargo test -p spur-mcp persisted_reads_project_over_stale_cache -- --nocapture`

Expected: FAIL because the stale cached entry is still observable through the current helper path.

- [ ] **Step 3: Refactor persisted reads behind one projection-aware helper**

Separate ephemeral and persisted access clearly enough that persisted readers cannot accidentally consume stale cache entries.

- [ ] **Step 4: Re-run the targeted test**

Run: `cargo test -p spur-mcp persisted_reads_project_over_stale_cache -- --nocapture`

Expected: PASS.

### Task 3: Recovery hardening and compatibility pinning

**Files:**
- Modify: `crates/spur-mcp/src/plan/projector.rs`
- Modify: `crates/spur-mcp/src/server.rs`
- Test: `crates/spur-mcp/tests/mutation_rollback_compensation.rs`
- Test: `crates/spur-mcp/tests/persisted_authority_flip.rs`

- [ ] **Step 1: Add failing recovery and compatibility tests**

Add:

- a recovery test that leaves persisted dispatch markers in an orphan state and asserts startup recovery clears them
- a projector test that pins namespaced labels as canonical and legacy labels as compatibility input only

- [ ] **Step 2: Run the targeted tests and verify they fail**

Run: `cargo test -p spur-mcp dispatch_orphan_recovery_restores_projected_state -- --nocapture`

Run: `cargo test -p spur-mcp projector_prefers_namespaced_plan_labels -- --nocapture`

Expected: at least one FAIL caused by the current seam behavior.

- [ ] **Step 3: Implement the minimal recovery/projector changes**

Keep compatibility localized, make recovery invariants explicit, and avoid widening the accepted label surface.

- [ ] **Step 4: Re-run the targeted tests**

Run:

- `cargo test -p spur-mcp dispatch_orphan_recovery_restores_projected_state -- --nocapture`
- `cargo test -p spur-mcp projector_prefers_namespaced_plan_labels -- --nocapture`

Expected: PASS.

### Task 4: Regression verification

**Files:**
- Modify: `docs/rca/2026-04-22-persisted-plan-control-loop-grounding.md` if the grounded risk wording changes materially

- [ ] **Step 1: Run focused regression suites**

Run:

- `cargo test -p spur-mcp --test e2e_closure_v0e -- --nocapture`
- `cargo test -p spur-mcp --test persisted_authority_flip -- --nocapture`
- `cargo test -p spur-mcp --test mutation_rollback_compensation -- --nocapture`

Expected: PASS.

- [ ] **Step 2: Update RCA wording only if behavior changed materially**

If the changes close one of the listed seams, update the RCA to reflect the new grounded state.
