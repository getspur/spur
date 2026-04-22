# RCA Confirmed Issues Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the confirmed code fixes from the 2026-04-22 lifecycle/worktree RCA, with regression tests proving the corrected behavior.

**Architecture:** The remediation splits into four implementation tracks: worktree lifecycle fixes in `spur-core`/`spur-worktree`, duplicated label-update batching in `spur-mcp`, exact durable signal dedup in `spur-mcp` mutation/watcher code, and any safe follow-through fixes that are explicitly confirmed by the RCA and can be implemented without inventing a new product API. The RCA remains the authority for scope: fix confirmed bugs, do not broaden into speculative redesign.

**Tech Stack:** Rust 2021, Tokio async, beads-backed integration tests, existing `cargo test` crate targets.

---

### Task 1: Worktree Retry and Snapshot Cleanup

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`
- Modify: `crates/spur-worktree/src/manager.rs` only if helper exposure is needed
- Test: `crates/spur-core/tests/retry_reflexion.rs`
- Test: existing unit/integration coverage near `orchestrator.rs`

- [ ] **Step 1: Add/extend a failing test for retry warning or snapshot lifecycle behavior**

Use the closest existing orchestrator-facing test or add a focused unit test around the helper path that covers:

```rust
assert!(warning.contains("fresh session ID"));
assert!(warning.contains("disk space may leak"));
```

and, if feasible without brittle process scaffolding, a snapshot cleanup assertion that post-`create_worktree` snapshot refs are deleted on the success path.

- [ ] **Step 2: Run the targeted test to verify the current behavior fails or is uncovered**

Run one of:

```bash
cargo test -p spur-core retry_reflexion -- --nocapture
```

or a narrower orchestrator unit target if you add one.

Expected: the new assertion fails before the implementation, or the missing-coverage gap is demonstrated by the new test.

- [ ] **Step 3: Implement the warning-text fix and snapshot-branch deletion**

Update `orchestrator.rs` so:

```rust
tracing::warn!(
    session = %outcome.worker_session,
    error = %e,
    "failed to remove retry-attempt worktree; retry will use a fresh session ID, but disk space may leak"
);
```

Also delete the snapshot branch immediately after `create_worktree(...)` succeeds, preserving the existing failure path if deletion itself fails:

```rust
if let Err(e) = worktrees.run_git(&["branch", "-D", &snapshot_branch], None).await {
    tracing::debug!(
        snapshot_branch = %snapshot_branch,
        error = %e,
        "failed to delete snapshot branch after worktree creation; will leak until explicit cleanup"
    );
}
```

- [ ] **Step 4: Run the targeted tests again**

Run:

```bash
cargo test -p spur-core retry_reflexion -- --nocapture
```

and any added narrower orchestrator test command.

Expected: PASS.

### Task 2: Batch Both `apply_issue_update` Copies

**Files:**
- Modify: `crates/spur-mcp/src/plan/mod.rs`
- Modify: `crates/spur-mcp/src/server.rs`
- Test: `crates/spur-mcp/tests/epic_completion.rs`
- Test: `crates/spur-mcp/tests/plan_audit_coverage.rs`

- [ ] **Step 1: Add a failing test that proves batched labels are sent in one update path**

Prefer a small unit-style test around the helper if practical; otherwise add a focused integration test that exercises multiple label additions/removals and asserts the final state still matches when labels are batched together.

Core expected behavior:

```rust
assert_eq!(final_issue.labels.contains(&plan_label), true);
assert_eq!(final_issue.labels.contains(&agent_label), true);
assert_eq!(final_issue.labels.contains(&removed_label), false);
```

- [ ] **Step 2: Run the targeted MCP tests to verify the pre-fix state**

Run:

```bash
cargo test -p spur-mcp epic_completion -- --nocapture
```

and any new focused helper test.

- [ ] **Step 3: Replace per-label loops with one batched label update in both helper copies**

Apply the same shape in both files:

```rust
if !update.add_labels.is_empty() || !update.remove_labels.is_empty() {
    pm.update_issue(
        issue_id,
        spur_pm::IssueUpdate {
            add_labels: update.add_labels,
            remove_labels: update.remove_labels,
            ..Default::default()
        },
    )
    .await?;
}
```

- [ ] **Step 4: Re-run the targeted MCP tests**

Run:

```bash
cargo test -p spur-mcp epic_completion -- --nocapture
cargo test -p spur-mcp plan_audit_coverage -- --nocapture
```

Expected: PASS.

### Task 3: Exact Durable Signal Dedup by `signal_id`

**Files:**
- Modify: `crates/spur-mcp/src/plan/labels.rs`
- Modify: `crates/spur-mcp/src/plan/signal_watcher.rs`
- Modify: `crates/spur-mcp/src/plan/mutation_executor.rs`
- Modify: `crates/spur-mcp/tests/signal_dedup.rs`
- Modify: `crates/spur-mcp/tests/mutation_write_ahead.rs`
- Modify: `crates/spur-mcp/tests/mutation_split.rs`
- Modify any other tests that construct `signal_processed_label(&batch.mutation_id)`

- [ ] **Step 1: Add a failing regression test for two distinct signals on one issue**

Extend `crates/spur-mcp/tests/signal_dedup.rs` with a case that seeds two valid signal comments with different `signal_id` values on the same task and asserts:

```rust
assert_eq!(mutations_applied, 2);
assert!(issue.labels.iter().any(|l| l == &signal_processed_label(&signal_id_1)));
assert!(issue.labels.iter().any(|l| l == &signal_processed_label(&signal_id_2)));
```

The pre-fix behavior should skip the second signal because the watcher only checks `starts_with("spur:signal-processed:")`.

- [ ] **Step 2: Run the targeted signal tests to verify failure**

Run:

```bash
cargo test -p spur-mcp signal_dedup -- --nocapture
```

Expected: FAIL in the new multi-signal case.

- [ ] **Step 3: Change durable processed markers to exact-`signal_id` semantics**

Refactor the label helper so it is keyed by signal identity:

```rust
pub fn signal_processed_label(signal_id: &uuid::Uuid) -> String {
    format!("spur:signal-processed:{}", signal_id.simple())
}
```

Then update the watcher and mutation executor so:

```rust
let signal_id = signal.signal_id();
let processed_label = signal_processed_label(&signal_id);
if issue.labels.iter().any(|label| label == &processed_label) {
    continue;
}
```

and on successful mutation commit:

```rust
add_labels: vec![signal_processed_label(&signal_id)]
```

If the current executor path lacks direct access to `signal_id`, thread it through from the selected `MutationBatch.trigger_signal_id` and fail loudly if a signal-triggered mutation commit tries to persist without it.

- [ ] **Step 4: Update tests and rerun the targeted suite**

Run:

```bash
cargo test -p spur-mcp signal_dedup -- --nocapture
cargo test -p spur-mcp mutation_write_ahead -- --nocapture
cargo test -p spur-mcp mutation_split -- --nocapture
cargo test -p spur-mcp labels_br_round_trip -- --nocapture
```

Expected: PASS.

### Task 4: Implement Remaining Confirmed Safe Fixes From the RCA

**Files:**
- Modify: `crates/spur-mcp/src/plan/mod.rs`
- Modify: `crates/spur-mcp/src/server.rs`
- Modify: `crates/spur-core/src/orchestrator.rs`
- Test: `crates/spur-mcp/tests/plan_cache_projection.rs`
- Test: other targeted tests only if the code changes require them

- [ ] **Step 1: Re-read RCA sections 3.1, 3.2, 3.4, 3.8 and decide which confirmed items are safely implementable now without inventing a new API**

Safe means:

- no speculative product surface such as a brand-new user-facing `close_plan`
- no half-fix that violates the RCA’s corrected constraints
- no broad refactor without regression tests

- [ ] **Step 2: If a remaining confirmed issue is safely implementable now, write the failing test first**

Candidate examples:

- `derive_epic_plan` narrowing with `IssueFilter.issue_type = Some("task".into())` only if the actual execute-epic contract and tests prove children are task-only
- ownership-gated orphan cleanup only if there is already a repo-exclusive lock path you can reuse safely

- [ ] **Step 3: Implement only those safe confirmed fixes**

Do **not** implement speculative or contradicted fixes such as:

- eager eviction of terminal ephemeral plans
- journal-based persisted-plan cache reuse
- two-phase epic persistence that can succeed with a partial graph

- [ ] **Step 4: Run the narrowest proving tests, then the affected crate suites**

Use targeted commands first, then broaden:

```bash
cargo test -p spur-mcp plan_cache_projection -- --nocapture
cargo test -p spur-mcp signal_dedup -- --nocapture
cargo test -p spur-core -- --nocapture
cargo test -p spur-mcp -- --nocapture
```

Expected: PASS for every changed behavior.
