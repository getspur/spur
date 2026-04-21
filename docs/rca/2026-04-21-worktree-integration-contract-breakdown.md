# RCA: Worktree Integration Contract Breakdown — `spur-worktree` Under Real Upstream Load

**Date:** 2026-04-21
**Author:** L9 Staff Engineer
**Status:** RCA + Architectural Finding
**Target Components:** `spur-worktree::manager`, `spur-worktree::artifact`, `spur-core::orchestrator`, `spur-mcp::tools`, `spur-mcp::plan`
**Precedents:** `2026-04-19-parallel-execution-file-isolation.md`, `2026-04-19-phase3a-worker-dispatch-failure-modes.md`

---

## 0. Executive Summary

MCTS first-principles analysis of `spur-worktree` against the real upstream callers (`delegate_to_worker`, `delegate_parallel`, `submit_plan`) reveals **7 flaws**, 2 critical. The architectural root cause is a missing integration phase: the worktree system implements isolation (branch + worktree) and observation (diff collection) correctly, but approved worker changes are never integrated into the main tree. `merge_worker()` exists as dead code. The brain receives diff text as information, not applied state, making `delegate_parallel` with overlapping files a guaranteed source of undetectable conflicts.

---

## 1. Methodology

Each branching decision in the delegation flow was treated as an MCTS node. For each node, the worktree contract was evaluated against the actual code paths triggered by `delegate_to_worker` and `delegate_parallel`. Simulations trace the real function calls, not idealized flows.

Key source files analyzed:
- `crates/spur-worktree/src/manager.rs` (658 lines) — worktree CRUD, snapshot, diff, merge
- `crates/spur-worktree/src/artifact.rs` (276 lines) — side-channel blob persistence
- `crates/spur-core/src/orchestrator.rs` (4857 lines) — delegation lifecycle, review gate, retry loop
- `crates/spur-mcp/src/tools.rs` (673 lines) — MCP tool surface
- `crates/spur-mcp/src/server.rs` (2784 lines) — delegation handler, inline wait, continuation bridge
- `crates/spur-mcp/src/plan/mod.rs` (3673 lines) — DAG plan executor

---

## 2. Flaw Inventory

| # | Flaw | Severity | Trigger | Root Cause |
|---|------|----------|---------|------------|
| 1 | Approved worker changes never integrated into main tree | CRITICAL | Every `delegate_to_worker` approve | `merge_worker` dead code; `detach_worktree` preserves branch but nobody cherry-picks |
| 2 | Parallel delegations create undetected conflicts | CRITICAL | `delegate_parallel` with overlapping files | No merge/conflict check between worker branches |
| 3 | Per-delegation `WorktreeManager` has zero cross-visibility | HIGH | Concurrent delegations + cleanup call | Each `execute_delegation` creates isolated instance |
| 4 | Snapshot-to-worktree TOCTOU gap | HIGH | `inline_wait=0` + brain edits after delegation returns | Two-step non-atomic snapshot → create |
| 5 | `commit_worker_changes` failure → inconsistent `DelegationResult` | MEDIUM | Pre-commit hook, GPG signing, disk full | Error logged but status remains `Success` |
| 6 | Snapshot branch leak, no automatic reclamation | MEDIUM | Every delegation (accumulates over time) | `self.worktrees` on orchestrator unused; per-delegation manager dropped |
| 7 | Parallel snapshots are temporally inconsistent | MEDIUM | `delegate_parallel` during brain edits | Independent `snapshot_brain_state()` calls per task |

---

## 3. Detailed Analysis

### 3.1 CRITICAL: Approved changes never integrated — `merge_worker` is dead code

**First principle:** A worktree system has three phases — isolate, execute, integrate. `spur-worktree` implements isolation and execution but the integration phase is a ghost.

**Evidence:**

- `WorktreeManager::merge_worker()` (`manager.rs:296-331`) cherry-picks a worker branch onto a target branch. Grep across the entire repository: **zero call sites**. Only the definition exists.
- On Approve, `apply_worktree_cleanup` (`orchestrator.rs:3593-3631`) does:
  1. `commit_worker_changes` — commits in the *worker's* worktree
  2. `detach_worktree` — removes the directory, preserves the branch `spur/worker-<agent>-<session>`
  3. Returns `Some(branch_name)` → becomes `DelegationResult.worker_branch`
- The brain receives `worker_branch: Some("spur/worker-codex-abc123")` and `diff: Some("...")` but the changes **do not exist in the main worktree**.
- The plan engine confirms this is by-design — `plan/mod.rs:1236`:
  > `"All tasks approved. Use create_pr with a worker_branch to create a pull request."`
- `create_pr` (`server.rs:1920-1956`) pushes a branch to GitHub and opens a PR. It does NOT merge the branch into the local tree.

**Simulation — `delegate_to_worker`, happy path:**

```mermaid
sequenceDiagram
    participant B as Brain (main worktree)
    participant O as Orchestrator
    participant WT as WorktreeManager
    participant W as Worker (.spur/worktrees/abc/)

    B->>O: delegate_to_worker("fix foo.rs")
    O->>WT: snapshot_brain_state()
    WT-->>O: spur/brain-snapshot-X
    O->>WT: create_worktree(session, "codex", snapshot)
    WT-->>O: .spur/worktrees/abc/ (branch: spur/worker-codex-abc)
    O->>W: spawn + prompt in .spur/worktrees/abc/
    W->>W: edits foo.rs
    W-->>O: stream ends
    O->>WT: collect_diff(session)
    WT-->>O: diff text = "--- a/foo.rs\n+++ b/foo.rs..."
    Note over O: Review gate: brain approves
    O->>WT: commit_worker_changes(session, "spur: worker codex output")
    O->>WT: detach_worktree(session)
    WT-->>O: Some("spur/worker-codex-abc")
    O-->>B: DelegationResult {status: Success, diff, worker_branch: Some("spur/worker-codex-abc")}

    Note over B: ❌ foo.rs in main worktree is UNCHANGED
    Note over B: Changes exist only on branch spur/worker-codex-abc
    Note over B: Brain must manually apply diff or call create_pr
```

**Impact:** The brain believes work is "done" (status: Success) but the main tree is unchanged. Worker changes are stranded on an orphaned branch. For `submit_plan` with N approved tasks, N branches exist with no merge strategy.

**Remediation paths:**
- (A) Call `merge_worker` after approve, cherry-picking each worker branch into main — handles single-delegation case but conflicts with parallel (see §3.2).
- (B) Rebase-style: after all plan tasks are approved, replay branches in topological order with conflict detection.
- (C) PR-only: accept that local merge is not supported; require `create_pr` for integration. Document as a constraint.

---

### 3.2 CRITICAL: `delegate_parallel` creates divergent branches with no conflict detection

**First principle:** Parallel workers sharing a common ancestor must have a merge strategy. Without one, conflicts are invisible until attempted merge — and `merge_worker` is dead code.

**Simulation — `delegate_parallel` with overlapping files:**

```mermaid
sequenceDiagram
    participant B as Brain
    participant O as Orchestrator
    participant WT_A as WorktreeManager_A (delegation A)
    participant WT_B as WorktreeManager_B (delegation B)
    participant W_A as Worker A
    participant W_B as Worker B

    B->>O: delegate_parallel([task1="refactor foo.rs", task2="add tests to foo.rs"])

    par Delegation A
        O->>WT_A: snapshot_brain_state() → spur/brain-snapshot-100
        O->>WT_A: create_worktree(s1, "codex", snapshot-100)
        O->>W_A: prompt in .spur/worktrees/s1/
        W_A->>W_A: refactors foo.rs (lines 1-50 rewritten)
    and Delegation B
        O->>WT_B: snapshot_brain_state() → spur/brain-snapshot-101
        O->>WT_B: create_worktree(s2, "codex", snapshot-101)
        O->>W_B: prompt in .spur/worktrees/s2/
        W_B->>W_B: adds tests to foo.rs (appends lines 51-80)
    end

    par Completion A
        W_A-->>O: done
        O->>WT_A: collect_diff → diff_A
        Note over O: Brain approves A
        O->>WT_A: commit_worker_changes → detach_worktree
        WT_A-->>O: branch spur/worker-codex-s1
    and Completion B
        W_B-->>O: done
        O->>WT_B: collect_diff → diff_B
        Note over O: Brain approves B
        O->>WT_B: commit_worker_changes → detach_worktree
        WT_B-->>O: branch spur/worker-codex-s2
    end

    Note over B: ❌ branch s1 rewrites foo.rs (1-50)
    Note over B: ❌ branch s2 adds to foo.rs (51-80) based on ORIGINAL
    Note over B: ❌ Cherry-picking both → GUARANTEED conflict
    Note over B: ❌ No code anywhere detects or reports this
```

Both branches share the same ancestor but diverge. `merge_worker` would detect the conflict (it does `cherry-pick` + `--abort` on failure returning `MergeResult::Conflict`), but nobody calls it. The brain gets two diffs that look compatible in text but are git-incompatible.

**Relationship to `2026-04-19-parallel-execution-file-isolation.md`:** That RCA identified the same symptom from the plan-execution angle and proposed a two-tier file isolation solution (predictive manifest + runtime sandbox). This RCA confirms the problem at the worktree layer and identifies the deeper structural cause: `merge_worker` is dead code, so even with manifests, there is no code path to integrate or detect conflicts.

---

### 3.3 HIGH: Per-delegation `WorktreeManager` has zero cross-visibility

**First principle:** A resource manager must have a complete inventory of the resources it manages.

**Evidence:** `orchestrator.rs:3046`:
```rust
let mut worktrees = WorktreeManager::new(repo_root);
```

Each `execute_delegation` call creates a **fresh** `WorktreeManager`. The `active` HashMap starts empty and only tracks THAT delegation's single worktree.

**Consequences:**

| Operation | What happens | Why it's wrong |
|-----------|-------------|----------------|
| `active_count()` | Returns 0 or 1 | Cannot see other delegations' worktrees |
| `cleanup_stale()` | Only checks its own worktree | Orphans from other delegations survive |
| `cleanup_orphans()` | May delete ACTIVE worktrees from other delegations | `self.active` doesn't track them |

**Simulation — `cleanup_orphans` race during concurrent delegations:**

```mermaid
sequenceDiagram
    participant D_A as Delegation A (WorktreeManager_A)
    participant D_B as Delegation B (WorktreeManager_B)
    participant GIT as Git repo

    D_A->>GIT: create_worktree(s1) → .spur/worktrees/s1/
    Note over D_A: WorktreeManager_A.active = {s1}

    D_B->>GIT: create_worktree(s2) → .spur/worktrees/s2/
    Note over D_B: WorktreeManager_B.active = {s2}

    Note over D_B: cleanup_orphans() runs...
    D_B->>GIT: git worktree list --porcelain
    GIT-->>D_B: shows s1 AND s2
    D_B->>D_B: s1 NOT in self.active → treat as orphan!
    D_B->>GIT: git worktree remove --force .spur/worktrees/s1/
    Note over D_A: ❌ Delegation A's worktree DESTROYED while worker still running
```

**Current mitigation:** `cleanup_orphans` is never called in the orchestrator. The `self.worktrees` field on the `Orchestrator` struct (line 376) is used only to construct per-delegation managers. This is a loaded gun — any future code that calls `cleanup_orphans` as periodic maintenance will break active delegations.

---

### 3.4 HIGH: Snapshot-to-worktree TOCTOU gap

**First principle:** A snapshot must be atomic with respect to the state it captures.

**Evidence:** `run_one_worker_attempt` (`orchestrator.rs:3999-4007`):
```rust
let snapshot_branch = worktrees.snapshot_brain_state().await...;
let worktree_info = worktrees.create_worktree(&worker_session, ctx.agent, &snapshot_branch).await...;
```

Between these two `await` points, the brain is still active in the main worktree. In the `inline_wait_ms = 0` (default) path, `delegate_to_worker` returns `{status: "pending"}` immediately, and the brain's turn **continues**. The brain can edit files after the snapshot but before the worktree is created.

**Deeper problem:** `snapshot_brain_state` creates a branch ref in the main repo. This branch can be deleted by another concurrent delegation's cleanup (§3.3) or by a brain running `git branch -D`. If the snapshot branch is deleted between the two calls, `create_worktree` fails with a delegation-level `DelegationStatus::Failed`.

**Simulation — brain edits during TOCTOU window:**

```mermaid
sequenceDiagram
    participant B as Brain
    participant O as Orchestrator
    participant WT as WorktreeManager

    B->>O: delegate_to_worker("fix auth")
    O->>WT: snapshot_brain_state()
    WT-->>O: spur/brain-snapshot-X (captures foo.rs at v1)
    Note over B: Brain continues its turn...
    B->>B: edits foo.rs → v2
    Note over O: create_worktree called AFTER brain edited
    O->>WT: create_worktree(session, "codex", spur/brain-snapshot-X)
    Note over WT: Worker branches from snapshot-X (foo.rs v1)
    Note over O: ❌ Worker's base is v1, but main tree is now v2
    Note over O: ❌ Worker's diff will conflict with brain's own v2 changes
```

---

### 3.5 MEDIUM: `commit_worker_changes` failure → inconsistent `DelegationResult`

**First principle:** If a function claims to commit changes, failure should not produce a result that pretends success.

**Evidence:** `apply_worktree_cleanup` (`orchestrator.rs:3601-3608`):
```rust
if should_commit_worker_diff(final_status) && diff.is_some() {
    if let Err(e) = worktrees.commit_worker_changes(worker_session, ...).await {
        tracing::warn!(error = %e, "failed to commit worker diff");
    }
}
```

On commit failure (pre-commit hook, GPG signing, disk full):
1. The warning is logged but execution continues
2. `detach_worktree` removes the directory but preserves the branch
3. The branch has NO commit with the worker's changes (they remain as uncommitted diffs in a now-deleted worktree directory)
4. `finalize` returns `DelegationResult { status: Success, worker_branch: Some("spur/worker-...") }`

**The brain receives `status: Success` and a `worker_branch` pointing to a branch with no worker commit.** The diff text and the branch state are inconsistent. `git show <worker_branch>` shows the pre-worker snapshot, not the worker's output.

**Correct behavior:** Commit failure on an approved delegation should downgrade to `DelegationStatus::Failed { error: "commit_worker_changes failed: ..." }` rather than returning Success with an invalid branch.

---

### 3.6 MEDIUM: Snapshot branch leak — no automatic reclamation

**First principle:** Every resource allocation must have a corresponding deallocation path.

`snapshot_brain_state` creates branches like `spur/brain-snapshot-20260421120000-0` in the main repo. After `create_worktree` uses the snapshot as a base, the snapshot branch is no longer needed. But:

1. The orchestrator's `self.worktrees` (which has `cleanup_orphans`) is never used for cleanup in the delegation path
2. Each delegation's `WorktreeManager` is dropped after `execute_delegation` returns — nobody calls cleanup on it
3. Snapshot branches accumulate indefinitely

`cleanup_orphans` attempts to handle this (`manager.rs:482-504`) but its guard check is wrong:

```rust
let active_bases: Vec<&str> = self.active.values().map(|info| info.branch.as_str()).collect();
for branch in branches_output.lines().map(|l| l.trim()) {
    if active_bases.iter().any(|b| b.contains(branch)) {
        continue; // don't delete
    }
    // delete the branch
}
```

Worker branch `spur/worker-codex-abc` does NOT contain the string `spur/brain-snapshot-X`, so this check never protects snapshot branches. Snapshots are always eligible for deletion (correct — they're not needed after worktree creation) but the check also never *deliberately* reclaims them. They just accumulate as orphaned refs.

**Measurement:** After 100 delegations, there are ~100 `spur/brain-snapshot-*` branches and ~100 `refs/spur/artifacts/*` refs with no GC path.

---

### 3.7 MEDIUM: Parallel snapshots are temporally inconsistent

**First principle:** Parallel workers operating on the same logical base should start from the same snapshot.

Each of the N tasks in `delegate_parallel` creates its own `WorktreeManager` and calls `snapshot_brain_state()` independently (`orchestrator.rs:3008-3012`). These calls happen at slightly different times due to tokio scheduling. If the brain modifies files between the first and last snapshot, workers get different bases.

The `stash create` retry loop (3 attempts with 50ms/100ms/150ms backoff, `manager.rs:96-111`) makes this worse — a delayed retry captures a different state than the first attempt would have.

```mermaid
flowchart LR
    subgraph "delegate_parallel: 3 tasks"
        T1["Task 1: snapshot at t=0ms<br/>sees: foo.rs v1"]
        T2["Task 2: snapshot at t=5ms<br/>sees: foo.rs v2 (brain edited)"]
        T3["Task 3: snapshot at t=12ms<br/>stash retry → sees: foo.rs v2"]
    end

    T1 --> W1["Worker 1 bases off v1"]
    T2 --> W2["Worker 2 bases off v2"]
    T3 --> W3["Worker 3 bases off v2"]

    W1 --> C["❌ Worker 1's diff is incompatible<br/>with Workers 2 & 3"]
```

**Current mitigation:** The brain's `delegate_parallel` tool description (`tools.rs:67`) says `"MUST demonstrate subtasks are independent — no shared state."` This is an LLM-prompting constraint, not a system invariant. No code enforces it.

---

## 4. The Architectural Root Cause

The system implements two of three phases correctly:

```
Isolate → Execute → Observe → [REVIEW] → Integrate
   ✓          ✓         ✓          ✓         ✗ MISSING
```

The integration contract is broken at two levels:

1. **Single delegation:** `merge_worker()` exists but is dead code. `detach_worktree` preserves the branch but nobody cherry-picks it. The brain receives diff text, not applied state.
2. **Parallel delegations:** N approved workers produce N stranded branches. No code detects or reports conflicts between them. `create_pr` pushes one branch at a time to GitHub — it does not merge locally and has no awareness of multi-branch conflict.

The two dead integration paths:

| Path | Status | What it does | Why it's dead |
|------|--------|-------------|--------------|
| `WorktreeManager::merge_worker()` | Dead code | Cherry-picks worker branch onto target branch | Zero call sites in codebase |
| `MCP create_pr` | Live but insufficient | Pushes one branch to GitHub, opens PR | Doesn't merge locally; no multi-branch awareness |

---

## 5. Recommended Remediation

### Phase 1: Stop the bleeding (no new code, fix semantics)

| Item | Change | Risk |
|------|--------|------|
| 5.1 | On `commit_worker_changes` failure in `apply_worktree_cleanup`, downgrade status to `Failed` instead of returning `Success` with invalid branch | Low — changes error path only |
| 5.2 | Add `#[allow(dead_code)]` + doc comment on `merge_worker` noting it's intentionally unused pending integration design | None — documentation only |
| 5.3 | Document in `delegate_parallel` tool description that approved changes are NOT auto-merged and may conflict | None — prompt-only |

### Phase 2: Single-delegation integration

| Item | Change | Risk |
|------|--------|------|
| 5.4 | After Approve, call `merge_worker` to cherry-pick the worker branch into main (or a designated integration branch) | Medium — cherry-pick conflicts must be handled |
| 5.5 | On `MergeResult::Conflict`, return `DelegationStatus::Conflict { files }` instead of `Success` | Medium — new status variant, downstream consumers must handle it |
| 5.6 | `cleanup_orphans` guard fix: track snapshot branches in `self.active` or use a separate `self.snapshots` set so active snapshots aren't deleted | Low |

### Phase 3: Parallel-delegation conflict detection

| Item | Change | Risk |
|------|--------|------|
| 5.7 | Shared `WorktreeManager` (or `Arc<Mutex<WorktreeManager>>`) across delegations within a brain session, so `active_count()` and `cleanup_orphans()` have full visibility | Medium — requires &mut self → Arc<Mutex<>> refactor |
| 5.8 | After all plan tasks are approved, attempt ordered merge with conflict detection before returning `ready_to_merge: true` | High — changes plan completion semantics |
| 5.9 | Single shared snapshot per `delegate_parallel` call (snapshot once, pass branch name to all N tasks) | Medium — requires refactoring `run_one_worker_attempt` to accept snapshot_branch as arg |

### Phase 4: GC

| Item | Change | Risk |
|------|--------|------|
| 5.10 | Periodic `cleanup_orphans` call from the orchestrator (with proper active-set tracking) | Low — currently never called |
| 5.11 | Delete snapshot branches after worktree creation succeeds (they're only needed as the base ref) | Low — one `git branch -D` after `create_worktree` |

---

## 6. Appendix: Complete Delegation Lifecycle State Machine

```mermaid
stateDiagram-v2
    [*] --> DelegationRequested: brain calls delegate_to_worker / delegate_parallel

    DelegationRequested --> Snapshotting: acquire semaphore permit

    Snapshotting --> WorktreeCreated: snapshot_brain_state → create_worktree
    Snapshotting --> SetupFailed: stash create / branch fails

    WorktreeCreated --> WorkerInitialized: connection.initialize()
    WorkerInitialized --> SessionCreated: new_session_with_bypass()
    SessionCreated --> WorkerStreaming: prompt(task)

    WorkerStreaming --> DiffCollected: stream ends (success or error)
    DiffCollected --> ArtifactPersisted: output > cap → persist_artifact

    ArtifactPersisted --> NoReview: review_required=false
    ArtifactPersisted --> AwaitingReview: review_required=true

    NoReview --> CommitAndDetach: should_commit → commit → detach_worktree
    NoReview --> RemoveOnly: !should_commit → remove_worktree

    AwaitingReview --> Approved: ReviewDecision::Approve
    AwaitingReview --> Rejected: ReviewDecision::Reject
    AwaitingReview --> Modified: ReviewDecision::Modify
    AwaitingReview --> Retry: ReviewDecision::Retry
    AwaitingReview --> TimedOut: review_timeout

    Approved --> CommitAndDetach
    Modified --> CommitAndDetach
    Rejected --> PreserveWorktree: worktree dir kept for inspection
    TimedOut --> PreserveWorktree: fallback=Reject|Abandon
    TimedOut --> CommitAndDetach: fallback=Approve

    Retry --> Snapshotting: bump attempt_n, new session, exponential backoff
    Retry --> RetryExceeded: attempt_n > max_review_retries → Failed

    CommitAndDetach --> DelegationCompleted: finalize(Success, worker_branch=Some)
    RemoveOnly --> DelegationCompleted: finalize(status, worker_branch=None)
    PreserveWorktree --> DelegationCompleted: finalize(status, worker_branch=None)
    SetupFailed --> DelegationCompleted: finalize(Failed, worker_branch=None)
    RetryExceeded --> DelegationCompleted: finalize(Failed, worker_branch=None)

    DelegationCompleted --> StrandedBranch: worker_branch=Some → NO MERGE
    DelegationCompleted --> [*]: respond_to.send(result)

    state StrandedBranch {
        [*] --> NoIntegration: merge_worker never called
        NoIntegration --> BrainManualAction: brain must create_pr or manually apply diff
    }
```

---

## 7. Appendix: Flaw Interaction Map

```mermaid
flowchart TD
    F1["FLAW 1: merge_worker dead code<br/>(CRITICAL)"]
    F2["FLAW 2: parallel conflict undetected<br/>(CRITICAL)"]
    F3["FLAW 3: no cross-visibility<br/>(HIGH)"]
    F4["FLAW 4: TOCTOU snapshot→worktree<br/>(HIGH)"]
    F5["FLAW 5: commit failure → Success<br/>(MEDIUM)"]
    F6["FLAW 6: snapshot branch leak<br/>(MEDIUM)"]
    F7["FLAW 7: temporal inconsistency<br/>(MEDIUM)"]

    F1 -->|enables| F2
    F3 -->|enables| F6
    F4 -->|amplifies| F7
    F5 -->|corrupts| F1

    F1 -->|root cause| MISSING["Missing Integration Phase<br/>Isolate → Execute → Observe → ✓ → ✗ Integrate"]
    F2 -->|root cause| MISSING

    style F1 fill:#c0392b,color:#fff
    style F2 fill:#c0392b,color:#fff
    style F3 fill:#e67e22,color:#fff
    style F4 fill:#e67e22,color:#fff
    style F5 fill:#f39c12,color:#fff
    style F6 fill:#f39c12,color:#fff
    style F7 fill:#f39c12,color:#fff
    style MISSING fill:#8e44ad,color:#fff
```
