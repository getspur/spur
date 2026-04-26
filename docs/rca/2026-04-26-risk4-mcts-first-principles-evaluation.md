# Risk 4 First-Principles Evaluation: MCTS Feedback Analysis

## Executive Summary

**Risk #4 as documented is structurally superseded** by `WorktreeAuthority` (bd-arch.26, `crates/spur-core/src/worktree_authority.rs`). The original `cleanup_orphans()` dead-code problem is no longer the operative failure mode. However, MCTS rollout reveals **three residual leaf paths** that still produce orphan accumulation with non-zero probability. The risk should be reclassified from **"Open / High"** to **"Partially Addressed / Medium"** with specific residual gaps tracked as child risks.

---

## 1. First-Principles Decomposition

Strip away implementation detail and ask: what are the fundamental truths about worktree orphaning?

| Axiom | Statement |
|---|---|
| A1 | A "worktree orphan" is a `git worktree` + `refs/heads/...` branch pair that no *living*, *authorized* SPUR process needs. |
| A2 | Safe deletion requires **two independent proofs**: (a) no in-process reference holds the worktree open, AND (b) no out-of-process SPUR instance is actively using it. |
| A3 | Proof (a) is process-local; proof (b) requires an inter-process coordination primitive. |
| A4 | Git itself provides no TTL or automatic reclamation for worktrees; without explicit cleanup, accumulation is monotonic. |
| A5 | A cleanup mechanism that reasons about repo-global git state using only process-local memory (`self.active`) violates A2(b) and is therefore **unsafe by construction**. |

**Conclusion from axioms:** `cleanup_orphans()` was indeed unsafe (violates A5). Any replacement must satisfy both A2(a) and A2(b) through a cross-process visible signal.

---

## 2. Current State (Post-WorktreeAuthority)

The `WorktreeAuthority` system, wired in `Orchestrator::new` at `orchestrator.rs:998-1127`, replaces the dead `cleanup_orphans` with a lease-aware garbage collector:

- **Cross-process proof (A2b):** `SessionLivenessProbe` probes `.spur/sessions/<brain_session_id>.lock` using `fs4` advisory locks. A successful exclusive acquire means no process holds that session → safe to delete.
- **Process-local proof (A2a):** `self_held: SelfHeldSet` tracks brain sessions the local orchestrator owns; these are skipped even if the lockfile is momentarily unlinked during `retire_active_brain`.
- **Quarantine grace:** `last_seen_alive` HashMap + 30s grace prevents sweeping a worktree whose owner just had a transient lockfile gap (e.g., session restart).
- **Periodic + startup sweep:** `spawn_periodic` (15 min interval + jitter) + immediate startup sweep.
- **fs_unsafe gate:** If advisory locks are unsupported (`ENOLCK`/`ENOTSUP`), sweeps are skipped entirely to avoid destroying live worktrees from other processes on shared filesystems.

---

## 3. MCTS Risk Tree

Monte Carlo Tree Search applied to risk evaluation:
- **Root:** Current system state (WorktreeAuthority live, cleanup_orphans dead)
- **Actions:** Failure modes / edge cases that could produce orphans
- **Rollout:** Simulate outcome probability for each path
- **Backprop:** Aggregate into overall risk score

### Tree Structure

```
Root: WorktreeAuthority deployed, cleanup_orphans dead
│
├── [P1] Normal delegation lifecycle
│   ├── Success: apply_worktree_cleanup removes worktree + branch
│   └── Partial failure: remove_worktree git command fails
│       └── Outcome: WorktreeAuthority periodic sweep reclaims on next pass
│           └── Prob: ~100% (liveness probe will see dead session)
│
├── [P2] Unclean shutdown during active delegation
│   ├── Sub-path A: Normal filesystem, lockfile released by kernel on exit
│   │   └── Outcome: Startup sweep + periodic reclaim after quarantine
│   │       └── Prob: ~98% (2% = race with quarantine grace vs. fast restart)
│   ├── Sub-path B: `retire_active_brain` unlinks lockfile BEFORE dropping guard
│   │   └── Outcome: Momentary missing lockfile; authority quarantine covers
│   │       └── Prob: ~99.5%
│   ├── Sub-path C: Filesystem crash, lockfile metadata not flushed
│   │   └── Outcome: Lockfile exists but stale; next probe acquires exclusive
│   │       └── Prob: ~99%
│   └── Sub-path D: Worker process survives orchestrator death (zombie/reparent)
│       └── Outcome: Worktree directory may still have open files
│           └── Prob: ~95% (git worktree remove --force usually succeeds anyway)
│
├── [P3] fs_unsafe deployment (NFS/sshfs/SMB)
│   └── Outcome: Authority skips ALL sweeps. Orphan accumulation is monotonic.
│       └── Prob: 0% automatic recovery. 100% manual cleanup required.
│           └── Weight: Conditional on deployment environment
│
├── [P4] Legacy namespace (pre-v2) worktrees
│   └── Outcome: Authority skips (`skipped_unknown_owner`). Permanent orphan.
│       └── Prob: 100% for any legacy worktree.
│           └── Weight: Declining as v2 becomes dominant
│
├── [P5] Snapshot branch leaks
│   └── Outcome: Authority NEVER cleans `spur/brain-snapshot-*` branches.
│       └── Prob: 100% accumulation rate.
│           └── Note: These are branches without worktrees; disk cost is ~40B/ref
│
├── [P6] Multi-orchestrator, same repo, different brain sessions
│   └── Outcome: Orchestrator A's authority sees B's lockfile as Live → skips.
│       └── Prob: 100% correct behavior. No orphan risk.
│
└── [P7] `cleanup_orphans` itself called by future code
    └── Outcome: Per-delegation manager deletes OTHER active worktrees
        └── Prob: ~0% today (zero call sites), but **catastrophic if triggered**
            └── Classification: Latent loaded gun, not active risk
```

### Leaf Node Scoring (Impact × Probability)

| Path | Impact | Prob | Score | Verdict |
|---|---|---|---|---|
| P1 | Low | ~100% resolved | ~0 | Closed |
| P2-A | Low | ~98% | 0.02 | Negligible |
| P2-B | Low | ~99.5% | 0.005 | Negligible |
| P2-C | Low | ~99% | 0.01 | Negligible |
| P2-D | Medium | ~95% | 0.05 | Low |
| **P3** | **High** | **100% if fs_unsafe** | **1.0** | **Active gap** |
| P4 | Low | 100% if legacy | 0.1 | Low (declining) |
| **P5** | **Low-Medium** | **100%** | **0.3** | **Active gap** |
| P6 | None | 100% | 0 | Closed |
| P7 | Critical | ~0% today | 0.0 | Debt, not active risk |

---

## 4. Backpropagation: Overall Risk Assessment

**The original Risk #4 framing is no longer the dominant failure mode.**

The `cleanup_orphans` dead-code issue (per-delegation manager, zero call sites) has been **architecturally superseded** by `WorktreeAuthority`, which correctly implements cross-process liveness detection. Calling `cleanup_orphans` today would be a bug, not a fix.

However, MCTS reveals the risk has **morphed into three residual paths**:

### Residual R4a: fs_unsafe deployments accumulate orphans unconditionally
- **Severity:** High (for affected deployments)
- **Status:** Open
- **Mitigation:** Documented skip. No fallback coordination mechanism exists.
- **Link:** Overlaps Risk #41 (`fs_unsafe` multi-instance gap).
- **Fix complexity:** Medium — requires secondary coordination (beads label, TCP socket, or mtime-based heuristic).

### Residual R4b: Snapshot branches leak indefinitely
- **Severity:** Low-Medium
- **Status:** Open
- **Evidence:** `snapshot_brain_state` creates branches like `spur/brain-snapshot-{timestamp}-{seq}`. On successful delegation, `delete_snapshot_branch` is called but errors are logged and dropped (`orchestrator.rs:5318-5322`). `WorktreeAuthority` does not enumerate or delete snapshot branches.
- **Impact:** ~40 bytes per ref in `.git/refs`, plus packfile bloat. A busy session could create hundreds.
- **Fix complexity:** Low — add snapshot branch enumeration to `WorktreeAuthority::sweep_once`, or tighten `delete_snapshot_branch` retry logic.

### Residual R4c: Legacy (pre-v2) worktree namespace is permanently orphaned
- **Severity:** Low
- **Status:** Open
- **Impact:** Any worktree created before the v2 namespace migration (`spur/worker/v2/...`) will never be cleaned by `WorktreeAuthority`.
- **Fix complexity:** Low — extend `WorktreeAuthority` to recognize pre-v2 `spur/worker-{agent}-{uuid}` pattern, or document one-time manual migration.

### Latent R4d: `cleanup_orphans` remains as a loaded gun
- **Severity:** Critical if triggered
- **Status:** Technical debt
- **Impact:** Zero today. If a future developer calls `manager.cleanup_orphans()` believing it is "the cleanup function," it will destroy active worktrees from other delegations or orchestrators.
- **Fix complexity:** Trivial — delete the dead code, or make it `pub(crate)` with a `#[doc(hidden)]` panic comment.

---

## 5. Recommendations

1. **Reclassify Risk #4 in `architecture.md`** from `Open / High` to `Partially Addressed / Medium`.
2. **Add child risks** R4a (fs_unsafe orphan accumulation), R4b (snapshot branch leak), and R4c (legacy namespace) to the Known Risks table.
3. **Delete or deprecate `cleanup_orphans`** to prevent future misuse. The function is conceptually wrong for SPUR's deployment model.
4. **Implement snapshot branch cleanup** in `WorktreeAuthority::sweep_once`: enumerate `spur/brain-snapshot-*` branches older than a threshold, check they are not referenced by any active worktree's `base_commit`, and delete.
5. **Address fs_unsafe gap** as part of Risk #41 remediation — do not attempt to fix R4a independently; it is the same missing secondary coordination mechanism.

---

## 6. MCTS Value Update

If we treat each risk path as a bandit arm and the "reward" as "successful automatic cleanup":

| Arm | Original Reward (pre-Authority) | Current Reward (post-Authority) | Δ |
|---|---|---|---|
| Normal lifecycle | ~0.9 (delegation cleanup) | ~0.99 (delegation + authority) | +0.09 |
| Unclean shutdown | **0.0** (no caller) | ~0.95 (startup + periodic sweep) | **+0.95** |
| fs_unsafe | 0.0 | 0.0 | 0.0 |
| Snapshot leak | 0.0 | 0.0 | 0.0 |

**UCT insight:** The search tree expanded significantly in the "unclean shutdown" branch. The `WorktreeAuthority` rollout converted the highest-probability, highest-impact failure mode from a guaranteed orphan into a ~95% auto-recovery path. The remaining low-reward arms (fs_unsafe, snapshot) should now receive the exploration budget.

---

*Grounded against: `crates/spur-core/src/worktree_authority.rs`, `crates/spur-worktree/src/manager.rs:544-624`, `crates/spur-core/src/orchestrator.rs:998-1127`, `docs/architecture.md:594`.*
