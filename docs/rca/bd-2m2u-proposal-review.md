# Critical Review: Proposed Execution Plan for bd-2m2u

**Reviewer:** Kimi Code CLI  
**Date:** 2026-05-11  
**Grounded against:** `crates/spur-mcp/src/plan/{mod.rs, projector.rs, reconciler.rs, mutation.rs, mutation_executor.rs, audit_sentinel.rs, signal_watcher.rs}`, `crates/spur-mcp/src/server/handlers/plan.rs`, `docs/architecture-spur-mcp.md`

---

## Executive Summary

The proposal is **architecturally sound and largely already implemented** in the current codebase. Phases 0, 1, 2a-2d, and even 2e infrastructure exist in production code today. The review therefore focuses on **gaps between the proposal and the implementation**, **missing edge-case coverage**, **latent race conditions**, and **documentation debt** that should be addressed before the work is considered complete.

**Verdict:** The proposal should be accepted with revisions. The remaining work is smaller than the proposal suggests because most phases have already shipped.

---

## 1. What Is Already Implemented (Proposal vs. Reality)

| Proposal Phase | Claimed Status | Actual Status in Code |
|---|---|---|
| **Phase 0** - Fix attempt counter | Prerequisite, blocks everything | **Shipped.** `project_attempt_facts` (projector.rs:54-65) counts `Dispatch` sentinels. Tests exist (mod.rs:6539). |
| **Phase 1** - Auto-retry once | Depends on Phase 0 | **Shipped.** `should_auto_retry` (mod.rs:804), `RetryRequested` sentinel, `build_failure_recovery_task` (mod.rs:1185), `persist_completion_result_with_retry_for_task` (mod.rs:2351). Tests exist (mod.rs:7457, 7759). |
| **Phase 2a** - Generic rollback | Foundation refactor | **Shipped.** `ExecutedOp` enum + `ReversibleOp` trait (mutation_executor.rs:506-517, 572-596). `rollback_executed_ops_in_reverse` (mutation_executor.rs:733-752). |
| **Phase 2b** - Extend `MutationCommit` | Foundation schema | **Shipped.** `MutationCommit` carries `op_tags` and `affected_task_ids` (audit_sentinel.rs:206-219). `MutationBatch::op_tags()` (mutation.rs:137-139). |
| **Phase 2c** - 3 new ops + MCP | Core recovery surface | **Shipped.** `RetryTask`, `ModifyTaskSpec`, `AbandonTask` in `PlanMutationOp` (mutation.rs:49-72). `submit_plan_mutation` MCP wired in `server/handlers/plan.rs:1110`. |
| **Phase 2d** - Escalation routing | Option A chosen | **Shipped.** `EscalatedToBrain` status (mod.rs:89-93), `EscalationRequested` sentinel, `ContinuationSource::PlanTaskEscalated` (spur-acp). Direct continuation push (mod.rs:2788-2810). |
| **Phase 2e** - Extra ops (gated) | Optional, deferred | **Partially shipped.** `InsertTaskBefore`, `AddDependency`, `CancelTask` exist in `PlanMutationOp` and have `apply_*` helpers (mutation_executor.rs:286-322). `RetryExhaustedProposer` does **not** yet exist. |
| **Phase 4** - Documentation | Final step | **NOT shipped.** `docs/architecture-spur-mcp.md` contains no lifecycle/recovery section. `AGENTS.md` lacks `signal:escalated` semantics. |

### Key Takeaway

The proposal describes a 4-phase (0-3) execution plan, but the codebase already contains the implementation for phases 0-2e. The only significant remaining gap is **documentation** (Phase 4). However, several **tests proposed in the RCA do not yet exist**, and there are **edge cases in the implemented code** that the proposal does not address.

---

## 2. Critical Findings

### C1: Missing Test Coverage for Core Scenarios

The proposal's "Order of Operations" section lists extensive TDD tests. Many are missing from the current codebase:

- `worker_failure_at_attempt_1_resets_to_pending_with_amended_prompt`
- `invariant_violation_at_attempt_1_retries_with_recovery_prompt`
- `auto_retry_concurrent_with_request_changes_first_writer_wins` (race coverage)
- `phase1_terminal_failure_promoted_to_escalated_to_brain_when_phase2d_enabled`
- `brain_directed_retries_capped_by_max_attempts`
- `submit_plan_mutation_clears_signal_escalated_label_on_success`
- `submit_plan_mutation_rolls_back_on_cycle_detection`

**Impact:** The auto-retry and escalation paths are implemented but under-tested. The existing tests cover the audit-emission layer (mod.rs:7457, 7759) but not the end-to-end reconciler dispatch path or the MCP tool layer.

**Recommendation:** Add the missing tests before declaring the fix complete. Priority: the race-condition test (`first_writer_wins`) and the `MAX_ATTEMPTS` integration test.

---

### C2: Latent Race Condition - Duplicate Retry Audits on Idempotent Re-processing

`persist_completion_result_with_retry_for_task` deduplicates the **Completion** audit via `already_emitted` (computed from `completion_audit_already_emitted`), but it does **not** deduplicate the **RetryRequested** or **EscalationRequested** audits.

If the reconciler's completion handler retries persistence (e.g., network timeout between beads writes), a second call with `already_emitted=true` will:
1. Skip the Completion audit (correct)
2. Emit a **second** RetryRequested audit (harmless but noisy)
3. Apply the same issue update again (idempotent, but indicates a logic gap)

**Code reference:** mod.rs:2382-2446

**Recommendation:** Add deduplication for `RetryRequested` and `EscalationRequested` analogous to `completion_audit_already_emitted`, or document the explicit decision to allow duplicates.

---

### C3: `apply_abandon_task` Cascade Closes Descendants Regardless of Current Status

The implementation of `AbandonTask { cascade_descendants: true }` (mutation_executor.rs:1434-1493) collects transitive descendants and closes them all, **even if a descendant was already `Approved`**.

The rollback restores original statuses, so a failed mutation is recoverable. But if rollback itself partially fails (e.g., network blip during restore), an already-Approved descendant could be left in `Failed` state.

**Recommendation:** The proposal should specify whether `AbandonTask` cascade is meant to be "close everything in subtree unconditionally" or "close only non-terminal descendants." The current behavior is the former; if the intent is the latter, add a status guard before closing.

---

### C4: `EscalatedToBrain` Tasks Silence Coexisting Scope-Drift Signals

The `SignalWatcher` (signal_watcher.rs:75-150) requires the `READY_FOR_REVIEW` label. `completion_escalation_update` removes that label and adds `signal:escalated`. This means:

- Escalated tasks are invisible to the autonomous `SignalWatcher` pipeline (intentional, per Option A)
- **But** if a worker emitted a `ScopeDrift` signal *before* failing, that signal is also invisible to the watcher because `READY_FOR_REVIEW` is gone
- The brain receives `PlanTaskEscalated` continuation, but the scope-drift signal metadata is not included in the continuation payload

**Impact:** Brain loses access to worker signals that might inform the recovery decision.

**Recommendation:** Either (a) extend `PlanTaskEscalatedEventPayload` to include recent worker signals, or (b) document explicitly that brain must call `get_plan_status` and inspect the audit log for signals before composing a mutation.

---

### C5: `build_failure_recovery_task` Hardcodes Git-Specific Instructions

The template (mod.rs:1225-1228) includes:

> "Inspect the worker branch state with `git log <base>..<branch>`, identify what went wrong..."

This is inappropriate for non-git workers (e.g., web-search agents, API-call-only workers). The proposal raised this as Open Question #4 but provided no resolution.

**Recommendation:** Add an `agent_class` parameter to `build_failure_recovery_task` and fork the recovery instruction paragraph per class. At minimum, add a `#[doc = "Note: This template assumes a git-capable worker."]` warning.

---

### C6: `MutationBatch::op_tag()` Returns Only the First Op's Tag

The `MutationPlan` write-ahead audit uses `batch.op_tag()` (mutation_executor.rs:79), which returns the tag of the **first** op only. If a brain submits a batch like `[ModifyTaskSpec, RetryTask]`, the audit records only `"modify_task_spec"`, making post-mortem analysis misleading.

**Code reference:** mutation.rs:131-133

**Recommendation:** Change the `MutationPlan` audit to use `batch.op_tags().join(",")` or store the full list. This is a one-line fix.

---

### C7: `should_auto_retry` Uses Magic Number Instead of Named Constant

The proposal defines `AUTO_RETRY_BUDGET = 1` as a named constant, but the implementation uses:

```rust
fn should_auto_retry(attempt: u32) -> bool {
    attempt <= 1  // literal, not AUTO_RETRY_BUDGET
}
```

**Recommendation:** Introduce the named constant as proposed, or add a doc comment explaining that `1` means "original dispatch + one auto-retry."

---

### C8: `submit_plan_mutation` Event Emission Has Brittle `plan_id` Recovery

In `server/handlers/plan.rs:1182-1185`, `plan_id` for the `PlanMutationApplied` event is derived from the trigger task's beads labels. If the trigger task lacks a `spur:plan-id:*` label (e.g., due to manual beads editing), the event emits `plan_id: ""`.

**Impact:** TUI/dashboard consumers may fail to route the event to the correct plan.

**Recommendation:** Require `plan_id` as an explicit parameter in the `submit_plan_mutation` MCP tool, or fail the call if it cannot be recovered from labels.

---

## 3. Architectural Observations

### A1: Two-Source-of-Truth Risk in `ModifyTaskSpec`

`ModifyTaskSpec` updates the beads issue body/labels/deps directly AND emits an extended `TaskSpec` audit. The projector then reads the audit to override live beads fields. This creates a temporal dependency: if the beads update succeeds but the audit emission fails, the projector will show stale state on the next read.

The rollback mechanism mitigates this for failures, but not for **audit emission dropping** (e.g., beads API timeout on comment creation). The proposal does not discuss this.

**Mitigation:** The current code is consistent with how `SplitTask` already works (beads writes + audit emission). Acceptable if documented.

### A2: `EscalatedToBrain` Is Not Terminal, but Also Not "Ready"

The proposal's state diagram shows `EscalatedToBrain -> Pending` (via brain mutation) and `EscalatedToBrain -> Failed` (via abandon). The implementation correctly excludes `EscalatedToBrain` from `is_terminal()` (mod.rs:97-105). However, `recompute_open_statuses` (projector.rs:453-481) also excludes it from dependency resolution, meaning:

- Descendants of an escalated task remain `Pending` forever
- The plan stalls until brain acts

This is **intentional and correct**, but the proposal does not discuss operational implications: a plan with an escalated task will never auto-complete, and there's no timeout mechanism.

**Recommendation:** Document the stall semantics explicitly. Consider a future enhancement (out of scope for bd-2m2u) to add an escalation timeout or auto-abandon after N hours.

### A3: Phase 2d Option A Defers Autonomous Recovery, but Phase 2e May Never Ship

The proposal defers `RetryExhaustedProposer` to Phase 2e, gated on Phase 2c proven in production. If Phase 2e never ships, the system has no autonomous recovery for retry-exhausted tasks - brain involvement is mandatory.

This is a product decision, not a technical flaw. But the proposal should be explicit that **Option A makes the system strictly dependent on brain availability** for all retry-exhausted failures.

---

## 4. Minor Issues and Polish

| # | Issue | Location | Severity |
|---|---|---|---|
| M1 | `project_closed_status` scans for `RetryRequested` but not `EscalationRequested` - if an escalated issue is manually closed, it projects as `Failed` rather than `EscalatedToBrain` | projector.rs:392-451 | Low |
| M2 | `latest_audit_advances_next_attempt` handles `RetryRequested` and `ReviewFeedback`, but not `EscalationResolved` - after brain resolves escalation, attempt counter may be off by one until next dispatch | projector.rs:67-81 | Low |
| M3 | `completion_retry_update` and `completion_escalation_update` both remove `READY_FOR_REVIEW` but do not remove `delegation-id:*` labels - stale delegation labels could confuse the projector | mod.rs:(completion update fns) | Low |
| M4 | `build_failure_recovery_task` includes `worker_branch` in history even when the branch is `None`, rendering "(branch: no branch)" in the prompt - slightly confusing for workers | mod.rs:1217-1223 | Cosmetic |

---

## 5. Recommendations Summary

### Must Do (Before Closing bd-2m2u)

1. **Add missing tests** - Priority: race-condition test, MAX_ATTEMPTS integration test, `submit_plan_mutation` rollback test.
2. **Write Phase 4 documentation** - Add "Task Lifecycle and Recovery" section to `docs/architecture-spur-mcp.md`; update `AGENTS.md` with `signal:escalated` semantics.
3. **Fix `MutationBatch::op_tag()`** - Use full `op_tags()` list in `MutationPlan` audit.
4. **Document the `EscalatedToBrain` stall semantics** - Brain must act; plan does not auto-complete; no timeout exists.

### Should Do (Post-Merge Polish)

5. **Add deduplication for `RetryRequested`/`EscalationRequested`** audits.
6. **Fork `build_failure_recovery_task` template** per agent class to remove git-specific instructions for non-git workers.
7. **Require explicit `plan_id`** in `submit_plan_mutation` MCP tool.
8. **Clarify `AbandonTask` cascade semantics** - close unconditionally vs. close only non-terminal descendants.

### Out of Scope (Acceptable as Documented Limitations)

- Escalation timeout / auto-abandon (product decision)
- `RetryExhaustedProposer` autonomous recovery (Phase 2e, gated)
- Cross-plan mutations (correctly out of scope)

---

## 6. Conclusion

The proposal correctly identified the root cause (attempt counter broken, five failure sites bypassing the budget, beads issue closed on retryable failures) and designed the right fix (count-based projection, centralized auto-retry chokepoint, escalation to brain via existing mutation infrastructure). The implementation that followed the proposal is clean, well-structured, and reuses existing architectural seams (`PlanMutationOp`, `ReversibleOp`, audit sentinels) rather than building parallel systems.

The main remaining risks are:
- **Under-tested race conditions** (duplicate retry audits, concurrent brain mutation)
- **Documentation debt** (Phase 4 not started)
- **Minor edge cases** (non-git agents, abandoned descendant status, coexisting signals)

With the "Must Do" items addressed, the proposal can be considered sound and complete.
