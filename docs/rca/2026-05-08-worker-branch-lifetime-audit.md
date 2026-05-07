# Worker Branch Lifetime Audit for Single-Parent Fast Path

Date: 2026-05-08

## Summary Verdict

**Verdict: holds for SPUR-owned branch mutation paths, assuming the approved task has a recorded `worker_branch`.**

The single-parent fast path consumes an approved direct dependency's recorded worker branch directly: `plan_dispatch_base_spec` returns `BaseSpec::Branch { name }` when `single_parent_approved_worker_branch` finds exactly one approved parent with a non-empty `worker_branch` (`crates/spur-mcp/src/plan/reconciler.rs:188`, `crates/spur-mcp/src/plan/reconciler.rs:193`, `crates/spur-mcp/src/plan/reconciler.rs:232`, `crates/spur-mcp/src/plan/reconciler.rs:248`, `crates/spur-mcp/src/plan/reconciler.rs:252`). The orchestrator uses that branch as the base ref for a fresh v2 worker branch (`crates/spur-core/src/orchestrator.rs:8016`, `crates/spur-core/src/orchestrator.rs:8022`, `crates/spur-worktree/src/manager.rs:504`, `crates/spur-worktree/src/manager.rs:521`, `crates/spur-worktree/src/manager.rs:527`).

No audited internal cleanup path deletes a detached approved v2 worker branch before direct children terminate. The important distinction is that approved work uses `detach_worktree`, which removes only the worktree directory and deliberately keeps the branch (`crates/spur-worktree/src/manager.rs:726`, `crates/spur-worktree/src/manager.rs:745`, `crates/spur-worktree/src/manager.rs:752`). The branch then disappears from `git worktree list --porcelain`, so both `cleanup_orphans` and `WorktreeAuthority` sweeps ignore it because they enumerate worktrees, not detached branch refs (`crates/spur-worktree/src/manager.rs:804`, `crates/spur-worktree/src/manager.rs:805`, `crates/spur-core/src/worktree_authority.rs:99`, `crates/spur-core/src/worktree_authority.rs:108`, `crates/spur-core/src/worktree_authority.rs:224`).

This audit does **not** prove safety against out-of-band `git branch -D spur/worker/v2/...` by a human or external tool. It also does not assert that every approved task necessarily has a branch: merge and dispatch paths fail closed when `worker_branch` is absent (`crates/spur-mcp/src/plan/reconciler.rs:197`, `crates/spur-mcp/src/plan/reconciler.rs:207`, `crates/spur-mcp/src/server.rs:4022`).

## V2 Worker-Branch Mutation Sites

### Creation

- `create_worktree_v2` creates exactly one worker branch under `spur/worker/v2/{agent}/{brain_session_id}/{worker_session_id}` (`crates/spur-worktree/src/manager.rs:502`, `crates/spur-worktree/src/manager.rs:513`, `crates/spur-worktree/src/manager.rs:527`, `crates/spur-worktree/src/manager.rs:532`). It resolves the selected base first (`crates/spur-worktree/src/manager.rs:521`) and records the worker branch in the manager's active map (`crates/spur-worktree/src/manager.rs:542`, `crates/spur-worktree/src/manager.rs:550`).

### Approved completion preservation

- Review approval in the orchestrator calls `apply_worktree_cleanup` for `Approve` and `Modify` decisions (`crates/spur-core/src/orchestrator.rs:7058`, `crates/spur-core/src/orchestrator.rs:7066`, `crates/spur-core/src/orchestrator.rs:7135`, `crates/spur-core/src/orchestrator.rs:7144`).
- `should_commit_worker_diff` treats `Success`, `Modified`, and timeout-approve as commit-worthy (`crates/spur-core/src/orchestrator.rs:7390`, `crates/spur-core/src/orchestrator.rs:7401`, `crates/spur-core/src/orchestrator.rs:7404`).
- `apply_worktree_cleanup` commits approved diff if present, then uses `detach_worktree` for approved work (`crates/spur-core/src/orchestrator.rs:7427`, `crates/spur-core/src/orchestrator.rs:7443`, `crates/spur-core/src/orchestrator.rs:7445`).
- `detach_worktree` runs `git worktree remove --force --force` and intentionally does not delete the branch (`crates/spur-worktree/src/manager.rs:726`, `crates/spur-worktree/src/manager.rs:743`, `crates/spur-worktree/src/manager.rs:745`, `crates/spur-worktree/src/manager.rs:752`). It removes the branch from the in-memory active map only after removal succeeds (`crates/spur-worktree/src/manager.rs:753`) and returns the preserved branch name (`crates/spur-worktree/src/manager.rs:755`).
- The reconciler persists completion with `result.worker_branch` in the completion audit fields (`crates/spur-mcp/src/plan/reconciler.rs:1284`, `crates/spur-mcp/src/plan/reconciler.rs:1286`, `crates/spur-mcp/src/plan/mod.rs:2456`, `crates/spur-mcp/src/plan/mod.rs:2463`). In in-memory `run_plan`, successful worker results also copy `result.worker_branch` into the task entry (`crates/spur-mcp/src/plan/mod.rs:2917`, `crates/spur-mcp/src/plan/mod.rs:2922`).
- Approval changes only task status and beads/audit state; it does not touch git refs (`crates/spur-mcp/src/plan/mod.rs:4032`, `crates/spur-mcp/src/plan/mod.rs:4042`, `crates/spur-mcp/src/plan/mod.rs:4046`, `crates/spur-mcp/src/plan/mod.rs:4053`, `crates/spur-mcp/src/plan/mod.rs:4066`).

### Branch deletion primitives and callers

- `delete_branch` is a generic helper over `git branch -D <name>` (`crates/spur-worktree/src/manager.rs:278`, `crates/spur-worktree/src/manager.rs:280`). Its production caller is preview cleanup, whose branch name is `spur/preview-{uuid}`, not `spur/worker/v2/...` (`crates/spur-mcp/src/plan/preview.rs:108`, `crates/spur-mcp/src/plan/preview.rs:118`, `crates/spur-mcp/src/plan/preview.rs:126`, `crates/spur-mcp/src/plan/preview.rs:173`).
- `delete_snapshot_branch` is a specialized wrapper over `git branch -D` for branches previously created by `snapshot_brain_state` (`crates/spur-worktree/src/manager.rs:563`, `crates/spur-worktree/src/manager.rs:566`, `crates/spur-worktree/src/manager.rs:567`). The orchestrator only calls it when `snapshot_required_for_dispatch` created a snapshot branch (`crates/spur-core/src/orchestrator.rs:8004`, `crates/spur-core/src/orchestrator.rs:8006`, `crates/spur-core/src/orchestrator.rs:8032`, `crates/spur-core/src/orchestrator.rs:8035`).
- `remove_worktree` deletes the active worker branch after removing the active worktree (`crates/spur-worktree/src/manager.rs:693`, `crates/spur-worktree/src/manager.rs:711`, `crates/spur-worktree/src/manager.rs:717`). It is used for non-approved terminal statuses, failed setup, overlay failure, retry intermediate attempts, and detach fallback (`crates/spur-core/src/orchestrator.rs:7257`, `crates/spur-core/src/orchestrator.rs:7265`, `crates/spur-core/src/orchestrator.rs:7448`, `crates/spur-core/src/orchestrator.rs:7454`, `crates/spur-core/src/orchestrator.rs:8061`, `crates/spur-core/src/orchestrator.rs:8071`, `crates/spur-core/src/orchestrator.rs:8134`, `crates/spur-core/src/orchestrator.rs:8169`). Those calls operate on the current active attempt's branch, not on an already-detached approved dependency.
- `cleanup_stale` deletes only entries still present in `self.active` by delegating to `remove_worktree` (`crates/spur-worktree/src/manager.rs:760`, `crates/spur-worktree/src/manager.rs:763`, `crates/spur-worktree/src/manager.rs:774`). There are no production call sites outside tests (`crates/spur-worktree/src/manager.rs:760`).
- `cleanup_orphans` removes orphaned worktrees backed by `refs/heads/spur/worker/v2/...` when the worktree path is not in `self.active` (`crates/spur-worktree/src/manager.rs:799`, `crates/spur-worktree/src/manager.rs:805`, `crates/spur-worktree/src/manager.rs:826`, `crates/spur-worktree/src/manager.rs:827`, `crates/spur-worktree/src/manager.rs:830`, `crates/spur-worktree/src/manager.rs:839`). It does not issue `branch -D` for v2 worker branches. Its only `branch -D` loop is snapshot cleanup over `git branch --list spur/brain-snapshot-*` (`crates/spur-worktree/src/manager.rs:859`, `crates/spur-worktree/src/manager.rs:861`, `crates/spur-worktree/src/manager.rs:872`, `crates/spur-worktree/src/manager.rs:881`).
- `WorktreeAuthority::sweep_one` deletes a branch after removing a worktree (`crates/spur-core/src/worktree_authority.rs:264`, `crates/spur-core/src/worktree_authority.rs:281`, `crates/spur-core/src/worktree_authority.rs:291`). Its sweep enumerates worktrees (`crates/spur-core/src/worktree_authority.rs:108`, `crates/spur-core/src/worktree_authority.rs:224`, `crates/spur-core/src/worktree_authority.rs:225`) and filters by v2 branch owner (`crates/spur-core/src/worktree_authority.rs:112`, `crates/spur-core/src/worktree_authority.rs:114`). A detached approved branch has no backing worktree and is therefore not a sweep entry.

## Lifecycle Paths

### Parent approval to direct-child dispatch

The parent branch is produced before plan-task approval. Worker completion persists `worker_branch` and moves the task to awaiting review (`crates/spur-mcp/src/plan/reconciler.rs:1284`, `crates/spur-mcp/src/plan/reconciler.rs:1286`, `crates/spur-mcp/src/plan/mod.rs:2456`, `crates/spur-mcp/src/plan/mod.rs:2463`). Approval then only changes task status and recomputes readiness (`crates/spur-mcp/src/plan/mod.rs:4042`, `crates/spur-mcp/src/plan/mod.rs:4066`). A child becomes dispatchable when blockers are approved, cancelled, or superseded (`crates/spur-mcp/src/plan/reconciler.rs:121`, `crates/spur-mcp/src/plan/reconciler.rs:124`, `crates/spur-mcp/src/plan/projector.rs:393`, `crates/spur-mcp/src/plan/projector.rs:397`).

For a child with exactly one approved dependency, dispatch reads the parent branch and uses it directly (`crates/spur-mcp/src/plan/reconciler.rs:188`, `crates/spur-mcp/src/plan/reconciler.rs:193`, `crates/spur-mcp/src/plan/reconciler.rs:232`, `crates/spur-mcp/src/plan/reconciler.rs:252`). For multi-parent fallback, dispatch requires `worker_branch` and resolves it to an oid before constructing overlays (`crates/spur-mcp/src/plan/reconciler.rs:197`, `crates/spur-mcp/src/plan/reconciler.rs:207`, `crates/spur-mcp/src/plan/reconciler.rs:213`).

### Plan merge

`merge_plan` is non-destructive to approved worker branches. The tool definition says it creates a dedicated plan-scoped merge branch (`crates/spur-mcp/src/tools.rs:533`, `crates/spur-mcp/src/tools.rs:536`). The server orders approved task branches and fails if any approved task lacks `worker_branch` (`crates/spur-mcp/src/server.rs:4013`, `crates/spur-mcp/src/server.rs:4016`, `crates/spur-mcp/src/server.rs:4022`). Integration creates a temporary worktree and merge branch (`crates/spur-mcp/src/server.rs:1921`, `crates/spur-mcp/src/server.rs:1938`, `crates/spur-mcp/src/server.rs:1945`), cherry-picks worker branches (`crates/spur-mcp/src/server.rs:1952`, `crates/spur-mcp/src/server.rs:1957`), and removes only the integration worktree (`crates/spur-mcp/src/server.rs:1979`, `crates/spur-mcp/src/server.rs:2003`). It never deletes source worker branches.

Auto-merge waits for all-approved durable completion and calls the same merge path (`crates/spur-mcp/src/plan/reconciler.rs:1743`, `crates/spur-mcp/src/plan/reconciler.rs:1756`, `crates/spur-mcp/src/plan/reconciler.rs:1757`).

### Plan cancel / delegation cancel

There is no `cancel_plan` MCP tool in `tools.rs`; the cancellation tool is `cancel_delegation` (`crates/spur-mcp/src/tools.rs:637`, `crates/spur-mcp/src/tools.rs:639`, `crates/spur-mcp/src/tools.rs:1090`). `handle_cancel_delegation` either returns an already completed result or signals `CancellationControl` (`crates/spur-mcp/src/server.rs:3578`, `crates/spur-mcp/src/server.rs:3589`, `crates/spur-mcp/src/server.rs:3609`, `crates/spur-mcp/src/server.rs:3616`). It does not touch git refs.

Cancellation result collection can synthesize `DelegationStatus::Cancelled` with `worker_branch: None` for a retiring brain session (`crates/spur-mcp/src/server.rs:2411`, `crates/spur-mcp/src/server.rs:2427`, `crates/spur-mcp/src/server.rs:2435`). The plan completion path maps cancelled worker results to `PlanTaskStatus::Cancelled` (`crates/spur-mcp/src/plan/mod.rs:2941`, `crates/spur-mcp/src/plan/mod.rs:2943`). In the orchestrator cleanup policy, `Cancelled` is preserved for inspection rather than removed (`crates/spur-core/src/orchestrator.rs:7376`, `crates/spur-core/src/orchestrator.rs:7384`, `crates/spur-core/src/orchestrator.rs:7436`). None of these paths delete an already-approved parent branch.

### Task supersede

Task supersede is represented through beads status/labels and dependency rewiring, not branch deletion. The mutation executor creates child issues (`crates/spur-mcp/src/plan/mutation_executor.rs:123`, `crates/spur-mcp/src/plan/mutation_executor.rs:125`), adds child dependency edges back to the parent (`crates/spur-mcp/src/plan/mutation_executor.rs:161`, `crates/spur-mcp/src/plan/mutation_executor.rs:162`), rewires downstream dependencies (`crates/spur-mcp/src/plan/mutation_executor.rs:183`, `crates/spur-mcp/src/plan/mutation_executor.rs:191`), closes the parent (`crates/spur-mcp/src/plan/mutation_executor.rs:199`, `crates/spur-mcp/src/plan/mutation_executor.rs:202`), and adds `superseded-by` labels (`crates/spur-mcp/src/plan/mutation_executor.rs:208`). The projector turns those labels into `PlanTaskStatus::Superseded` (`crates/spur-mcp/src/plan/projector.rs:270`, `crates/spur-mcp/src/plan/projector.rs:286`).

No mutation executor path calls `delete_branch`, `delete_snapshot_branch`, `remove_worktree`, `cleanup_orphans`, or `WorktreeAuthority`. Supersede can make a task terminal, but it does not reclaim its worker branch.

### Brain crash recovery

Crash recovery preserves detached approved branches by construction. Startup recovery projects plan state from beads comments and labels (`crates/spur-mcp/src/server.rs:2190`, `crates/spur-mcp/src/server.rs:2223`, `crates/spur-mcp/src/server.rs:6235`, `crates/spur-mcp/src/server.rs:6248`, `crates/spur-mcp/src/server.rs:6249`). Projection reads `worker_branch` from latest completion facts (`crates/spur-mcp/src/plan/projector.rs:138`, `crates/spur-mcp/src/plan/projector.rs:151`, `crates/spur-mcp/src/plan/projector.rs:552`, `crates/spur-mcp/src/plan/projector.rs:553`).

Recovery maintenance compensates mutation orphans and resolves stale dispatch labels (`crates/spur-mcp/src/server.rs:6255`, `crates/spur-mcp/src/server.rs:6257`, `crates/spur-mcp/src/server.rs:6263`). `resolve_dispatch_orphan` writes a dispatch-orphan audit and clears dispatch intent labels; it does not touch git refs (`crates/spur-mcp/src/server.rs:1510`, `crates/spur-mcp/src/server.rs:1545`, `crates/spur-mcp/src/server.rs:1555`). `compensate_mutation_orphans` closes child issues and clears superseded labels; it also does not touch git refs (`crates/spur-mcp/src/server.rs:1444`, `crates/spur-mcp/src/server.rs:1470`, `crates/spur-mcp/src/server.rs:1480`).

`WorktreeAuthority` startup and periodic sweeps can delete live-looking v2 worker branches only when they are attached to a worktree belonging to a missing/dead brain session (`crates/spur-core/src/orchestrator.rs:2817`, `crates/spur-core/src/orchestrator.rs:2822`, `crates/spur-core/src/orchestrator.rs:2839`, `crates/spur-core/src/orchestrator.rs:2841`, `crates/spur-core/src/worktree_authority.rs:159`, `crates/spur-core/src/worktree_authority.rs:171`). Detached approved branches are not enumerated because `enumerate_worktrees` is based on `git worktree list --porcelain` (`crates/spur-core/src/worktree_authority.rs:224`, `crates/spur-core/src/worktree_authority.rs:225`, `crates/spur-core/src/worktree_authority.rs:249`).

## Invariant Argument

Invariant under audit:

> For every approved plan task `T_k`, `T_k.worker_branch` exists in the repository at every moment between `T_k`'s approval and the terminal status of every direct child.

The invariant holds under internal SPUR paths for the lifetime side:

1. An approved task's branch is detached, not removed. `detach_worktree` removes the directory and keeps the branch (`crates/spur-worktree/src/manager.rs:745`, `crates/spur-worktree/src/manager.rs:752`).
2. All in-repo branch deletion sites are scoped away from detached approved v2 branches:
   - Preview deletes `spur/preview-{uuid}` only (`crates/spur-mcp/src/plan/preview.rs:118`, `crates/spur-mcp/src/plan/preview.rs:173`).
   - Snapshot deletion deletes only snapshot refs created for dispatch (`crates/spur-worktree/src/manager.rs:563`, `crates/spur-core/src/orchestrator.rs:8035`).
   - `remove_worktree` acts on the current active attempt's branch (`crates/spur-worktree/src/manager.rs:698`, `crates/spur-worktree/src/manager.rs:717`); approved attempts use `detach_worktree` instead (`crates/spur-core/src/orchestrator.rs:7443`, `crates/spur-core/src/orchestrator.rs:7445`).
   - `cleanup_orphans` removes v2 worker worktrees and only deletes `spur/brain-snapshot-*` refs (`crates/spur-worktree/src/manager.rs:827`, `crates/spur-worktree/src/manager.rs:839`, `crates/spur-worktree/src/manager.rs:861`, `crates/spur-worktree/src/manager.rs:881`).
   - `WorktreeAuthority` deletes branches only as a consequence of removing enumerated worktrees (`crates/spur-core/src/worktree_authority.rs:224`, `crates/spur-core/src/worktree_authority.rs:281`, `crates/spur-core/src/worktree_authority.rs:291`).
3. Direct-child dispatch reads the parent's branch before creating the child worktree (`crates/spur-mcp/src/plan/reconciler.rs:193`, `crates/spur-core/src/orchestrator.rs:8016`, `crates/spur-worktree/src/manager.rs:521`). A missing branch is therefore detected as a dispatch/setup failure rather than silently proceeding.
4. Plan merge, cancel, supersede, and recovery paths mutate PM state, temporary worktrees, or merge branches, but not detached worker branches (`crates/spur-mcp/src/server.rs:1957`, `crates/spur-mcp/src/server.rs:2003`, `crates/spur-mcp/src/server.rs:3616`, `crates/spur-mcp/src/plan/mutation_executor.rs:199`, `crates/spur-mcp/src/server.rs:1555`, `crates/spur-mcp/src/server.rs:6257`).

The invariant is not enforced against two external classes:

- External/manual ref deletion. Nothing prevents a human or unrelated automation from running `git branch -D <approved-worker-branch>`.
- Approved-but-branchless state. The approval state machine does not independently verify that `entry.worker_branch` is `Some` when approving (`crates/spur-mcp/src/plan/mod.rs:4032`, `crates/spur-mcp/src/plan/mod.rs:4042`). Downstream dispatch and merge fail closed when the branch is absent (`crates/spur-mcp/src/plan/reconciler.rs:207`, `crates/spur-mcp/src/server.rs:4022`), but approval itself does not block. This is outside the deletion-lifetime question, but it is relevant if the invariant is read as "every approved task always has a usable branch."

## Minimal-Diff Fix If Stronger Enforcement Is Required

No code fix is required for the audited internal cleanup contract. If SPUR wants the stronger invariant "approval is impossible unless the recorded branch still resolves," the minimal change should be a guard in `handle_review_task` / `apply_decision_and_extract` before accepting `"approve"`:

1. Require `entry.worker_branch.is_some()` for approve.
2. For persisted plans with repo access, run `git rev-parse --verify <worker_branch>` before emitting the approval audit.
3. Return a validation error if the branch is missing, leaving the task in `AwaitingReview`.

Regression-test sketch:

- Create a plan task in `AwaitingReview` with `worker_branch: Some("spur/worker/missing")`.
- Delete or omit the branch in a temp git repo.
- Call `review_task` approve.
- Assert the response rejects approval, task remains `AwaitingReview`, no approval audit is emitted, and no child becomes `Ready`.

That would not expand the v2 cleanup contract. It would only make approval refuse state that the existing dispatch and merge paths already cannot consume.

## Recommended Harness Assertions

- Add an integration harness that creates `T1 -> T2`, approves `T1`, and asserts `git rev-parse --verify <T1.worker_branch>` succeeds before `T2` dispatch, during `T2` dispatch, and after `T2` terminal status.
- Add a crash-recovery harness: approve `T1`, restart/recover the persisted plan, assert the projected `T1.worker_branch` still resolves, then dispatch `T2` through the single-parent fast path.
- Add a cleanup harness that runs `WorktreeAuthority::sweep_once` and `cleanup_orphans` after `T1` is approved/detached but before `T2` dispatch; assert `T1.worker_branch` still resolves.
- Add a merge harness that runs `merge_plan` after all tasks approve and asserts all source `worker_branch` refs still resolve after merge success or conflict.
- Add a negative harness for stronger enforcement if adopted: manually delete `T1.worker_branch` before approval or child dispatch and assert the system fails with a clear diagnostic rather than silently continuing.
