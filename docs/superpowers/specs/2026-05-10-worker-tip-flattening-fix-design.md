# Design: Eliminate worker-tip flattening to fix staging branch conflicts

## 1. Context & Problem
During the execution of plan tasks with dependencies, the spur-worktree manager prepares a worker branch by checking out the plan's base commit and then cherry-picking any approved dependencies (overlays). The HEAD at this point is the dispatched_base_oid.

When the worker finishes, finalize_worker_branch normalizes the branch. Currently, it computes commits_before using plan_base_commit. Since plan_base_commit predates the overlays, the overlay commits are counted as worker commits. The branch is then soft-reset to plan_base_commit and squashed. This results in a final worker tip that contains *both* the overlays and the worker's changes, flattened into a single commit whose parent is the plan base.

Later, when build_staging_branch attempts to integrate this task by cherry-picking dispatched_base_oid..worker_tip, it picks that single flattened commit. Since the flattened commit includes the overlay changes, it causes a conflict when applied to a staging branch that already has the overlay changes.

## 2. Proposed Approaches

### Option 1: Topology-Preservation (Recommended)
Update the WorktreeManager to record the dispatched_base_oid as the base_commit for the worker's active WorktreeInfo after overlays are applied.
- **Trade-offs**: Extremely localized fix. Preserves the exact DAG semantics where workers only see their declared dependencies. Maintains the 'one commit per task' invariant for the brain, but correctly roots that commit at the last overlay tip.
- **Complexity**: Low.

### Option 2: Sequential Integration
Maintain a running spur/plan-integration/{plan_id} branch. When a task is approved, it is merged here. Downstream tasks are dispatched from this branch instead of applying overlays.
- **Trade-offs**: Broadens the base for parallel workers, potentially leaking independent changes between parallel tasks. Adds significant state management complexity to the reconciler.
- **Complexity**: High.

### Option 3: Hybrid
Use topology preservation for linear chains, and sequential integration for merge topologies.
- **Trade-offs**: Unnecessary complexity.

## 3. Recommended Design (Option 1)
We will implement Option 1 (Topology-Preservation).

### Components
1. **WorktreeManager (crates/spur-worktree/src/manager.rs)**:
   - Add a new method pub fn update_base_commit(&mut self, session_id: &SessionId, new_base_commit: String) -> Result<()>
   - This method updates info.base_commit for the given worker session.

2. **run_one_worker_attempt (crates/spur-core/src/orchestrator/delegation/worker_attempt.rs)**:
   - After apply_overlays successfully returns, worktrees.resolve_head(&worktree_info.path) produces the dispatched_base_oid.
   - Call worktrees.update_base_commit(&worker_session, dispatched_base_oid.clone()) so that subsequent commit counting and squashing ignores the overlays.

## 4. Acceptance Criteria
- finalize_worker_branch preserves the commit chain (worker commit parent = last overlay).
- Staging branch builds cleanly for overlapping dependency chains.
- Parallel execution semantics remain unchanged.
