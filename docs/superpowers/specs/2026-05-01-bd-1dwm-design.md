# bd-1dwm — Design Spec: Worker Worktree Dep-Closure Integration

**Status:** Approved (brainstorming complete; ready for plan)
**Issue:** [bd-1dwm](beads://bd-1dwm)
**Inputs:**
- Issue body (root-cause analysis, four candidate options A/B/C/D)
- `docs/superpowers/specs/2026-05-01-bd-1dwm-gemini-perspective.md` (architect; introduced Option E)
- `docs/superpowers/specs/2026-05-01-bd-1dwm-kimi-review.md` (API ergonomics; identified `preview_task_base` gap and `create_merge_task` deferral)
- `docs/superpowers/specs/2026-05-01-bd-1dwm-codex-review.md` (implementation feasibility; surfaced BWC, persistence, lineage, GC, and diff-regression issues)
- MCTS first-principles re-evaluation (this brainstorming session)

---

## Problem

Workers in Spur's plan engine execute against a filesystem state derived from `WorktreeManager::snapshot_brain_state()` (`crates/spur-worktree/src/manager.rs:117`), which branches from `repo_root` HEAD. The brain's HEAD never advances mid-plan — approved task branches stay isolated until `merge_plan` runs at completion. Downstream tasks whose specs depend on an upstream approved task's NEW FILE do not see that file. The worker silently re-implements (clobbering at merge time), fails to compile, or imports a stub. The bd-2dww plan exhibited this catastrophically: ~700 LoC across 5 changes lost; 4 manual recovery commits required.

## Goal

Make every worker dispatch see the merged contents of its declared transitive dependencies — and only those — without introducing persistent shared state that complicates retry/reject/supersede semantics.

## Non-Goals

- Not changing `merge_plan`'s integration mechanism (it remains topo cherry-pick of worker tips onto base).
- Not auto-resolving conflicts. Conflicts surface to the operator with full context.
- Not addressing undeclared sibling-leak (e.g., M2's worker accidentally reading M1's file when M2 only declared dep on root). G-strict's strict isolation prevents this by construction.
- Not implementing `create_merge_task` DAG primitive in this scope (deferred to Phase 2).
- Not requiring `BaseSpec` on all delegations day-one (BWC migration window in Phase 2).

## Architecture

### Core operation: G-strict (stateless dep-closure cherry-pick at dispatch)

At dispatch of plan task N:

1. Compute the transitive closure of N's `depends_on` over tasks currently in `Approved` status. Result: an ordered list of `(dep_task_id, dep.dispatched_base_oid, dep.worker_branch.tip_oid)` tuples in topological order.
2. Snapshot brain state via existing `snapshot_brain_state()` (preserves brain's uncommitted changes) — unchanged.
3. Create worker worktree from snapshot via existing `create_worktree()` — unchanged.
4. **NEW**: in the worker worktree, cherry-pick `(dep.dispatched_base_oid .. dep.worker_branch.tip)` for each dep in topological order. On conflict: abort cherry-pick, remove worktree, return structured `OverlayConflict { dep_task_id, files }` error.
5. Record `worker.dispatched_base_oid = HEAD` of the worker worktree immediately after the last successful cherry-pick (= worktree HEAD before the worker's first commit).
6. Hand worktree to worker. Worker proceeds normally; its commits land on top of the overlay.

`merge_plan` is unchanged. Worker contributions remain `worker.dispatched_base_oid..worker_branch.tip` (was `base_snapshot..worker_branch.tip` — see Diff-Regression Fix below).

### Why G-strict over the alternatives

| Property | Option A (FF integration) | Option E (cherry-pick integration branch) | **G-strict** |
|---|---|---|---|
| Parallel siblings | Broken (FF fails) | Works | Works |
| Reject-after-approve | N/A | Requires reset+replay logic | Zero work (next dispatch's overlay just excludes rejected dep) |
| Brain must-fix on approved branch | N/A | Integration tip becomes stale | Automatic (overlay reads dep's CURRENT tip on next dispatch) |
| Concurrent plans | Per-plan branch + mutex | Per-plan branch + mutex | No shared state |
| Determinism | Depends on approval order | Depends on approval order + must-fix timing | Pure function of (approved_set, declared_deps) |
| Conflict surface | All approval-time conflicts | All approval-time conflicts | Only when a downstream actually depends on the conflicting pair |
| New primitive | New persistent branch + lifecycle | New persistent branch + lifecycle | **Reuses `merge_plan`'s cherry-pick mechanism** |
| Cost per dispatch | O(1) | O(1) | O(deps × commits) ≈ ~500ms typical, negligible vs worker init |

The reframe that makes G-strict obvious: **the bug isn't "we lack an integration branch", it's "we cherry-pick deps once at end (`merge_plan`) instead of also at start (dispatch)."** The composition operation is the same; G-strict invokes it in miniature at dispatch.

### Companion: D — clobber detector

At review time (in `review_task`), diff the worker's added/modified files against prior approved tips in the same plan. If the worker creates a non-trivial file already present on a prior approved tip with substantially different content, emit a new signal `signal:potential_clobber { conflicting_task_id, file }`. Brain treats as a hard hint to reject.

D ships as Phase 0 — independently valuable insurance against a recurrence of bd-2dww-class loss while G-strict is built.

### Companion: `BaseSpec` (additive API)

`DelegationRequest` gains an Optional `base: Option<BaseSpec>` field:

```rust
pub enum BaseSpec {
    RepoMain,
    Branch(String),
    Commit(String),
    WithOverlay { base: Box<BaseSpec>, overlays: Vec<OverlayCommit> },
}

pub struct OverlayCommit {
    pub source_task_id: String,
    pub base_oid: String,   // start of cherry-pick range
    pub tip_oid: String,    // end of cherry-pick range
}
```

- `None` → legacy behavior (`RepoMain` snapshot, today's path). Existing brains keep working.
- Plan engine, when dispatching a plan task, ALWAYS passes `Some(WithOverlay { base: Branch(plan.base_snapshot_branch), overlays: <computed closure> })`.
- Ad-hoc delegations may opt into non-RepoMain bases (Phase 2 — out of scope here, but the schema supports it).

Tool schemas (`crates/spur-mcp/src/tool_schemas.rs:11-24`, `28-45`) keep `deny_unknown_fields` but `base` is added as Optional to avoid breaking existing callers. After Phase 2 migration window, brains may be required to send `base` explicitly.

### Companion: `preview_task_base` MCP tool

Read-only tool for the brain (or operator) to inspect what overlay the engine would compute for a given plan task without dispatching:

```
preview_task_base(plan_id, task_id) → {
  overlays: [{ task_id, base_oid, tip_oid, commit_count }, ...],
  predicted_base_oid: string,         // HEAD after applying overlays, if clean
  conflicts: [{ task_id, files, message }] | null,
}
```

The brain calls this BEFORE approving a downstream task or BEFORE asking for dispatch in cases where conflict is a real risk. Returns the same `OverlayConflict` shape that dispatch would produce.

### Conflict-routing flow

When G-strict's cherry-pick fails at dispatch:

1. `WorktreeManager` returns `WorktreeError::OverlayConflict { dep_task_id, files }` (NEW variant).
2. `run_one_worker_attempt` (`crates/spur-core/src/orchestrator.rs:6310`) translates to `AttemptSetupError::OverlayConflict { dep_task_id, files }`.
3. Plan reconciler recognizes this shape, writes `signal:integration-conflict v1` sentinel comment + audit entry on the task issue, and **leaves issue status Open** (not Failed). Plan task transitions to a NEW status `BlockedOnSetupConflict { dep_task_id, files }` distinct from `Failed`.
4. Plan UI surfaces `BlockedOnSetupConflict` as a distinct state with files list and dep id (NOT collapsed into Failed).
5. Brain receives continuation + signal, decides: retry (after dep updated), introduce a manual merge task (Phase 2 will give a primitive; Phase 1 = manual `update_issue` + `add_dependency`), re-spec downstream task to drop conflicting dep, or abort plan.

### Persistence touch-points

`dispatched_base_oid` MUST persist or restart/projection silently loses it. Required updates:

- `PlanTaskEntry` (`crates/spur-mcp/src/plan/mod.rs:107-125`): add `dispatched_base_oid: Option<String>`. Per-attempt records in `history` also need it.
- `PlanCompletionAudit` (`crates/spur-mcp/src/plan/audit_sentinel.rs:35-50`, `92-106`): include `dispatched_base_oid` so projector can re-hydrate after restart.
- `PlanProjector` (`crates/spur-mcp/src/plan/projector.rs:52-75`, `449-463`): emit and consume `dispatched_base_oid` in projected state.

### Lineage event extension

The `DelegationRequested` event currently lacks base provenance, and the documented retry-text-loss issue at `crates/spur-core/src/orchestrator.rs:6291-6297` already drops constraint-augmented tasks at the legacy lineage adapter. To prevent BaseSpec from being invisible to lineage:

- Add `base: Option<BaseSpec>` to `SpurEventBody::DelegationRequested`.
- Or: emit a new dedicated event `SpurEventBody::DispatchOverlayApplied { request_id, base_spec, dispatched_base_oid, overlay_task_ids }` immediately before agent init.

This is a pre-existing-debt fix that bd-1dwm forces. Either approach works; recommendation: emit the new dedicated event (smaller blast radius, doesn't change `DelegationRequested` schema).

### Diff-regression fix (codex finding)

`get_task_diff` (`crates/spur-mcp/src/server.rs:4429-4446`) currently reconstructs the per-task diff as `base_snapshot..worker_branch`. With overlays, that range now includes dep commits — review diff would be polluted. Change to `dispatched_base_oid..worker_branch` so reviewers see exactly the worker's net contribution.

### Worker-output single-commit invariant

Worker branches will contain `<overlay commits> + <worker commits>`. `merge_plan` cherry-picks only the tip. This is acceptable iff worker output is a single final commit at the tip. Today there's an implicit cleanup pass; we MUST make this an explicit asserted invariant: at completion-audit time, verify `worker.dispatched_base_oid..worker_branch` is exactly one commit (or apply a squash if not). Failure → audit-time error, signal to brain.

### WorktreeAuthority adoption

`crates/spur-core/src/worktree_authority.rs:112-124` only owns `spur/worker/v2/...` branches. The G-strict path uses the existing legacy `spur/worker-{agent}-{session_id}` naming. Two options:
- (a) Migrate G-strict paths to v2 naming so authority GC owns them automatically.
- (b) Extend authority to also recognize legacy branches (already may be needed pre-bd-1dwm).

Recommendation: (a) — migrate to v2 naming as part of Phase 1. Legacy non-plan dispatches stay on the old path until separately migrated.

## Components

| Component | Role | Files |
|---|---|---|
| `WorktreeManager` overlay support | Cherry-pick dep ranges into worker worktree; return structured `OverlayConflict` | `crates/spur-worktree/src/manager.rs` |
| Reconciler dispatch overlay computation | Walk `depends_on` closure over Approved; build overlay list | `crates/spur-mcp/src/plan/reconciler.rs:625-656`, `crates/spur-mcp/src/plan/mod.rs:1740-1807` |
| `BaseSpec` schema (Optional) | Carry explicit base + overlay through `delegate_to_worker` / `delegate_parallel` | `crates/spur-mcp/src/tool_schemas.rs:11-45`, `crates/spur-mcp/src/tools.rs:17-43` |
| Orchestrator BaseSpec application | Translate request → worktree creation; apply overlays before agent init | `crates/spur-core/src/orchestrator.rs:6263-6318` |
| `dispatched_base_oid` persistence | Store on entry, audit, projector | `crates/spur-mcp/src/plan/mod.rs:92-125`, `audit_sentinel.rs:35-50`, `projector.rs:52-75` |
| `OverlayConflict` → signal routing | New `signal:integration-conflict` sentinel; new plan task status `BlockedOnSetupConflict` | `crates/spur-mcp/src/plan/signals.rs:15-28`, `crates/spur-mcp/src/server.rs:1188-1272`, `signal_watcher.rs:89-110` |
| `preview_task_base` MCP tool | Dry-run overlay computation (no worktree) | `crates/spur-mcp/src/tools.rs`, `crates/spur-mcp/src/server.rs` (new handler) |
| Lineage `DispatchOverlayApplied` event | Visibility into which overlay a dispatch saw | `crates/spur-core/src/lineage/adapter.rs:123-190`, `SpurEventBody` definition |
| `get_task_diff` fix | Use `dispatched_base_oid..worker_branch` not `base_snapshot..worker_branch` | `crates/spur-mcp/src/server.rs:4429-4446` |
| Worker-output single-commit invariant | Assert at completion-audit | `crates/spur-mcp/src/plan/audit_sentinel.rs` |
| WorktreeAuthority v2 migration | G-strict worker branches use v2 naming | `crates/spur-worktree/src/manager.rs:201`, `worktree_authority.rs:112-124` |
| D — clobber detector | Diff worker output against prior approved tips at review; emit `signal:potential_clobber` | `crates/spur-mcp/src/server.rs` (review_task), signals enum |

## Data Flow

### Successful dispatch under G-strict

```
brain → submit_plan(plan with depends_on edges)
  → plan engine stores PlanState, base_snapshot_branch
  → reconciler dispatches Ready tasks one at a time

For each task N becoming Ready:
  reconciler:
    overlays = closure(N.depends_on, Approved tasks)
    base_spec = WithOverlay { base: Branch(plan.base_snapshot), overlays }
    delegate_to_worker(N, base=base_spec)
  ↓
  orchestrator.run_one_worker_attempt:
    snapshot_brain_state()                        # unchanged
    create_worktree(base=Branch)                  # unchanged
    for overlay in base_spec.overlays:            # NEW
      cherry_pick(overlay.base..overlay.tip)      # in worker worktree
    dispatched_base_oid = worktree.HEAD           # NEW
    emit DispatchOverlayApplied { ... }           # NEW
    record dispatched_base_oid on PlanTaskEntry   # NEW
    initialize_agent + start worker               # unchanged
  ↓
  worker runs, commits land, emits audit
  ↓
  brain reviews diff (now using dispatched_base_oid..tip — codex fix)
  ↓
  brain calls review_task(approve)
  ↓
  reconciler runs D detector (clobber check vs prior tips)
  ↓
  task → Approved; entry.worker_branch.tip recorded
  ↓
  next Ready task picks up dep tips fresh
```

### Conflict path

```
overlay cherry-pick conflict in worker worktree
  ↓
WorktreeManager returns OverlayConflict { dep_task_id, files }
  ↓
orchestrator → AttemptSetupError::OverlayConflict
  ↓
reconciler:
  - writes signal:integration-conflict sentinel + audit
  - sets entry.status = BlockedOnSetupConflict { dep_task_id, files }
  - issue stays Open
  - emits DelegationFailed event with conflict context
  ↓
plan UI shows BlockedOnSetupConflict (distinct state)
  ↓
brain sees signal + decides: retry, manual merge task, re-spec, abort
```

## Error Handling

| Failure | Detection | Routing | Brain action |
|---|---|---|---|
| Overlay cherry-pick conflict at dispatch | `git cherry-pick` exit code | `OverlayConflict` → `signal:integration-conflict`; status `BlockedOnSetupConflict`; issue stays Open | Inspect conflicting files; introduce manual merge task or re-spec downstream |
| Overlay dep has no `dispatched_base_oid` (e.g., legacy task missing field) | Reconciler pre-flight check | Plan task fails fast with diagnostic; emit `signal:invariant_violation` | Operator-level fix (re-run audit projector) |
| `get_task_diff` called for legacy task without `dispatched_base_oid` | Fallback to `base_snapshot..tip` with warning | Telemetry event; no error | None (BWC) |
| Worker output is multi-commit (invariant violation) | Completion audit check | Reject completion; signal to brain | Brain instructs worker to squash, or auto-squash via tool |
| Brain submits plan with non-DAG `depends_on` (cycle) | Existing topological_order check | Submit fails | Re-spec |
| Two parallel dispatches contend on git ops | Each has own worktree → no contention | N/A | N/A |
| `preview_task_base` called for non-existent plan/task | Tool input validation | Error response | Brain corrects call |
| D detector false positive (legitimate file replacement) | Brain reads signal as advisory, not blocker | Brain approves anyway; signal stays in audit trail | None (auditable) |

## Testing

### Unit
- `WorktreeManager` overlay path: clean apply, conflict detection, OID recording.
- Reconciler overlay computation: closure walk over Approved, topological ordering, exclusion of non-Approved deps.
- `BaseSpec` schema deserialization: Optional → None preserves legacy behavior.
- `dispatched_base_oid` round-trip through audit + projector.
- `get_task_diff` uses `dispatched_base_oid` when present, falls back when absent.
- D detector: clobber detection on file overlap; non-clobber for unrelated files.

### Integration
- 3-task plan, .1 creates `foo.rs`, .2 modifies `foo.rs`, .3 imports both — all three approve and merge cleanly without manual recovery (the bd-2dww canonical reproducer).
- 2 parallel siblings, each modifies `foo.rs` differently → first approves cleanly; second triggers `BlockedOnSetupConflict` only at dispatch of a downstream that depends on both.
- Reject-after-approve: M1 approved → cherry-picked into M2's overlay → M1 rejected → M2 re-dispatched → overlay no longer includes M1.
- Brain must-fix push to M1's worker branch after approval → next dispatch of M2 sees the must-fix.
- Restart mid-plan: kill orchestrator after .2 approves but before .3 dispatches; restart; .3 picks up correct overlay from persisted `dispatched_base_oid`.
- `preview_task_base` returns the same overlay + conflicts as actual dispatch.

### End-to-end
- Re-run a bd-2dww-shaped plan synthetically; verify zero LoC loss vs the historical recovery commits.

## Build Sequence

### Phase 0 — Clobber Detector (D) — Standalone PR (~1-2 days)

1. Extend `signal:` enum to include `potential_clobber { conflicting_task_id, file }`.
2. Implement detector in `review_task` handler: diff worker output against prior approved task tips in the same plan; emit signal if non-trivial recreation detected.
3. Plan UI surfaces signal in task review pane.
4. Tests: clobber positive, clobber negative, multi-clobber, false-positive resilience.

### Phase 1 — G-strict + BaseSpec + Preview + Conflict Routing (~4-5 days)

Sub-steps in dependency order:

1. **Schema additions (Optional first):** `BaseSpec` enum + `OverlayCommit`; add Optional `base` to `DelegationRequest`, `delegate_to_worker` / `delegate_parallel` schemas. `dispatched_base_oid` on `PlanTaskEntry` (Optional); audit and projector pass-through. Migration helpers for existing plans (re-projection backfills nothing — pre-existing tasks have None).
2. **WorktreeManager overlay support:** new `apply_overlays(&mut Worktree, overlays: &[OverlayCommit])`; structured `OverlayConflict` error. Unit tests.
3. **Orchestrator BaseSpec application:** wire `BaseSpec` from request → resolve base ref → create worktree → apply overlays → record `dispatched_base_oid`. Lineage event `DispatchOverlayApplied` emitted before agent init.
4. **Reconciler overlay computation:** closure walk + topo order; produces `WithOverlay` BaseSpec for plan dispatches. Non-plan dispatches receive `None` (legacy path).
5. **Conflict routing:** new `BlockedOnSetupConflict` plan task status; reconciler writes `signal:integration-conflict`; plan UI distinct state; reviewer can retry or escalate.
6. **`preview_task_base` MCP tool:** dry-run handler reuses overlay computation + WorktreeManager's pre-flight check (apply overlays in throwaway worktree, capture conflicts, discard).
7. **`get_task_diff` fix:** use `dispatched_base_oid..worker_branch` when present, fallback otherwise.
8. **Worker-output single-commit invariant:** completion-audit assertion; squash helper if needed.
9. **WorktreeAuthority v2 naming migration** for G-strict-created worker branches.
10. **End-to-end test:** synthetic bd-2dww reproducer.

### Phase 2 — Out of scope (deferred) (~2-3 days, separate spec)

- `WithOverlay` for ad-hoc (non-plan) delegations + opt-in brain control.
- `create_merge_task` MCP primitive for DAG-level conflict resolution.
- Flip `base` from Optional to Required on `delegate_to_worker` after migration window.
- Brain-side override of computed overlay for exceptional plan tasks.

## Open Questions

None blocking. The following remain for Phase 2 design:

- Exact shape of `create_merge_task`: does the brain author the merge commit directly, or does it spawn a "merge worker" with its own dispatch flow?
- Should `BaseSpec::WithOverlay` allow nested overlays (overlay-of-overlay), or always flatten? Recommendation: flatten; nesting offers no operational benefit and adds parsing complexity.

## Risks Accepted

1. **Stuck-at-dispatch** is a NEW operator-handled state. Phase 1 mitigates with `preview_task_base` (operator can see conflicts before dispatch); Phase 2 adds `create_merge_task` (operator can resolve in-band). Without these, brain's only recourse is manual issue update + `add_dependency` calls.
2. **Loss of integration-branch audit trail.** E (rejected) would have left a `git log spur/plan-integration/{plan_id}` for forensics. G-strict's overlays are ephemeral. Mitigation: `DispatchOverlayApplied` lineage event records overlay metadata; `dispatched_base_oid` persisted on entry. Forensic queries go through lineage instead of `git log`.
3. **Per-dispatch O(deps × commits) cost.** Pathological plans (50+ tasks, full-diamond DAG) could see ~5-15s of cherry-pick. YAGNI for now; add caching keyed on `(plan_id, sorted_dep_tip_oids)` if metrics justify.
4. **Lineage adapter pre-existing debt.** The retry-text-loss issue at `orchestrator.rs:6291-6297` is pre-existing but G-strict surfaces a new vector for it (BaseSpec invisible in retries). Phase 1 mitigation = dedicated `DispatchOverlayApplied` event. Full fix of the lineage adapter is separate work.
