# `submit_plan` Substrate Migration — Design Spec

| | |
|---|---|
| Status | Draft |
| Date | 2026-05-10 |
| Author | brain (synthesized from RCA + dual reviews under beads-first principle) |
| Replaces | dual-mode `submit_plan` (`persist_as_epic` boolean) |
| Predecessor docs | `docs/rca/2026-05-10-submit-plan-end-to-end-flow.md` (RCA), kimi v2, gemini v2 substrate-principle reviews |

---

## TL;DR

Migrate `submit_plan` from a dual-mode dispatcher (ephemeral `tokio` loop OR durable reconciler) into a single substrate-first execution path where **beads is the authoritative state for every plan, every transition, and every brain↔worker comm**. The migration is sequenced as ten reversible PRs that harden cache discipline and substrate writes BEFORE deleting the ephemeral path. Deletion is step 9, not step 1 — doing it in any other order produces a system that violates the principle worse than today.

---

## Background

### Current state

`submit_plan` (`crates/spur-mcp/src/server.rs:5344`) forks on `persist_as_epic`:

| Mode | Path | Authority |
|---|---|---|
| Ephemeral (`persist_as_epic=false`, **default**) | `submit_plan_internal` (`server.rs:5299`) → `spawn_ephemeral_plan_runner` (`server.rs:2456`) → `run_plan` (`plan/mod.rs:3151`) | In-memory `active_plans` HashMap |
| Persistent (`persist_as_epic=true`) | `build_epic_subgraph` (`server.rs:5492`) → beads epic + children + edges → `fast_forward_reconciler` → `Reconciler::tick_once` (`reconciler/mod.rs:547`) | Beads issues + labels + audit sentinels |

Both paths converge on the same `DelegationRequest` mpsc channel, the same orchestrator, the same worker runtime. They diverge only in *who owns plan state*. That divergence is the architectural defect.

### Controlling principle (non-negotiable)

> **Beads is the first-class durable substrate.** All tracking, audit, coordination, and brain↔worker communication flow through beads. Durability across that substrate is what makes SPUR a stateful system for plans, analysts, distributed task execution.

**Implications:**

- `active_plans` is NOT authoritative. It is a derivable cache.
- Latency from beads roundtrips is the **cost of durability**, accepted by design.
- Local-dev without beads is **not a goal**. Beads is mandatory.
- Brain↔worker state transitions persist to beads BEFORE any in-process notify.
- mpsc/oneshot/watch channels are **transport for execution**, never authoritative state.

### Why this spec exists

Three independent reviews (kimi v1, gemini v1+v2, kimi v2, internal code-reviewer) converged on the same core finding: **the simplification "drop ephemeral, keep persistent" is correct in direction, but executing it as code-removal first violates the principle.** The persistent path itself contains structural defects — advisory beads writes, mutation-before-persistence, non-durable `dispatched_base_oid` — that must be fixed BEFORE the ephemeral path is deleted, or the resulting single-path system is worse than today's dual-path one.

This spec is the migration plan that respects that ordering.

---

## Goals

1. **Single substrate-first execution path.** After migration, every plan is durable in beads from the first MCP call to terminal state. No in-memory authority. No ephemeral fallback.
2. **Cache discipline.** `active_plans` becomes a versioned read-through projection: safe to wipe at any time, mismatched versions trigger re-projection, mutations only flow via re-project after beads write.
3. **Persist-before-notify discipline.** Every state transition writes beads first; in-process notifications fire only after beads confirms. `apply_issue_update` failures are NEVER swallowed.
4. **Idempotency at the substrate layer.** `submit_plan` accepts a `client_idempotency_key`; mapping persists in beads (not memory) so brain restart preserves the dedup window.
5. **`dispatched_base_oid` durable before worker spawn.** Move from post-completion `watch` channel read to pre-spawn beads label/audit. `recover_orphaned_dispatch` always has an anchor.
6. **`plan_truncate_and_restart` on the substrate path.** Today it produces non-substrate ephemeral children. Fix as part of this migration.
7. **Each step independently reversible and shippable.** A stack of small PRs, each one an improvement in isolation, each behind a feature flag where state-shape changes.

## Non-goals

1. **Local-dev without beads.** Explicitly rejected. Document beads-required dev-loop.
2. **Latency optimization.** The 3s `base_interval` and beads roundtrip costs are accepted. Optimization is out of scope; if it matters later, optimize the reconciler internals, not the architectural choice.
3. **Backwards compatibility for `persist_as_epic=false` callers.** The flag is being deleted. Brains must update prompts. Auto-promotion (`false` silently treated as `true`) is the migration bridge, not a permanent contract.
4. **Replacing the reconciler with something different.** The reconciler IS the substrate-first dispatcher. Both substrate-principle reviews independently concluded this. Out of scope.

---

## Invariants

These are the enforcement obligations the new design must hold. Each invariant has a concrete check point.

### INV-S1 · Persist before notify
**Statement:** Every state transition must be durable in beads BEFORE any in-process notification fires.
**Today's violation:** `handle_review_task` (`plan/mod.rs:4797`–`4832`) mutates `PlanState` under lock, drops the lock, then writes beads async; failure is `warn!`-logged and swallowed.
**Enforcement:** Two-phase commit: write beads → confirm read-back → update cache → notify. Failure at write returns hard error; failure at read-back invalidates cache.

### INV-S2 · Cache is disposable
**Statement:** `active_plans` must be safe to wipe and rehydrate from beads at any time without loss of authority.
**Today's violation:** `load_or_project_plan` (`server.rs:7178`) short-circuits to stale cache for non-persisted plans; ephemeral plans only exist in memory.
**Enforcement:** Every cache entry carries a `beads_version` token; reads validate; mismatch re-projects. Ephemeral path deleted (PR #9).

### INV-S3 · Beads is sole brain↔worker state channel
**Statement:** mpsc/oneshot/watch carry execution transport only, never authoritative state.
**Today's violation:** `run_plan` mutates `PlanState` and treats beads as optional; `dispatched_base_oid` flows via `watch` until completion.
**Enforcement:** Every state transition that crosses a boundary is preceded by a beads write. In-process channels carry "go look at beads" signals, not "here is the new state."

### INV-S4 · Audit sentinels are load-bearing
**Statement:** Audit sentinels are durable substrate, not decorative logging.
**Today's violation:** `emit_plan_submit_audit` (`server.rs:837`) is advisory; failure is `warn!`-logged. `read_persisted_plan_bootstrap` (`server.rs:864`) depends on the audit existing for restart recovery.
**Enforcement:** Audit emission is part of the same two-phase commit as the state transition it accompanies. Failure to write the sentinel fails the transition.

### INV-S5 · Dispatch intent visible before spawn
**Statement:** A worker is never spawned without dispatch intent durable in beads first.
**Today's violation:** `run_plan` (`plan/mod.rs:3210`) sets `entry.status = Dispatched` in memory only, then sends `DelegationRequest`.
**Enforcement:** Reconciler's `persist_dispatch_intent` (`reconciler/mod.rs:748`) is the only path. Lease label, dispatch audit, and `dispatched_base_oid` all written before the request leaves the reconciler.

### INV-S6 · Idempotency persists across restart
**Statement:** Duplicate `submit_plan` calls (transport retries, brain restarts) collapse to a single beads epic.
**Today's violation:** Both forks mint fresh UUIDs (`server.rs:5310`, `5465`); no dedup table.
**Enforcement:** `client_idempotency_key → plan_id` map stored in beads (synthetic dedup epic OR labeled comments on submitter). Resolved on every `submit_plan` before epic construction.

---

## Target Architecture

### `submit_plan` (after migration)

```
submit_plan(tasks, base, client_idempotency_key?)
  │
  ├── 1. resolve idempotency (read beads dedup map)
  │       └── on hit: return existing plan_id, exit
  │
  ├── 2. validate DAG (cycle, dups, dangling, sibling-overlap)
  │
  ├── 3. resolve_plan_base
  │       └── snapshot HEAD or pin ref into beads-tracked branch
  │
  ├── 4. build_epic_subgraph (write beads: epic + children + edges + labels)
  │       └── persist idempotency mapping
  │
  ├── 5. emit PlanSubmit audit sentinel (write beads, REQUIRED)
  │
  ├── 6. populate active_plans cache from the just-written beads view
  │       └── stamp beads_version token
  │
  ├── 7. fast_forward_reconciler (notify; reconciler reads beads next tick)
  │
  └── 8. return plan_id
```

### `Reconciler::tick_once` (after migration)

Unchanged in shape — already substrate-first. Hardened in three places:

1. `persist_dispatch_intent` includes `dispatched_base_oid` write (computed from preview, not from `watch` post-completion).
2. `persist_worker_completion_and_notify` performs two-phase commit: write beads → read-back → update cache → notify.
3. Cache reads in result handler verify `beads_version` and re-project on mismatch.

### `handle_review_task` (after migration)

```
handle_review_task(plan_id, task_id, decision, feedback)
  │
  ├── 1. load_or_project_plan (cache read with beads_version validation)
  │
  ├── 2. compute decision deltas (no mutation yet)
  │
  ├── 3. write beads: status transition + audit sentinel + label changes
  │       └── on failure (after retries): invalidate cache entry, return hard error
  │
  ├── 4. read-back from beads to confirm version advanced
  │       └── on mismatch: invalidate, retry from step 1
  │
  ├── 5. update active_plans cache from re-projection
  │
  ├── 6. fast_forward_reconciler
  │
  └── 7. return result
```

### `active_plans` (after migration)

| Property | Value |
|---|---|
| Type | `Arc<Mutex<LruCache<PlanId, CachedPlan>>>` |
| Capacity | bounded (config: `plan_cache_capacity`, default 256) |
| Invalidation triggers | beads_version mismatch, explicit invalidate on write failure, LRU eviction |
| Population | only from beads projection, never from in-process construction |
| Read path | `load_or_project_plan(plan_id)` — cache hit validates version, miss projects fresh |
| Write path | does not exist; "writes" are beads operations followed by re-projection |

```rust
struct CachedPlan {
    state: PlanState,
    beads_version: BeadsVersion,    // monotonic token derived from beads
    cached_at: Instant,
}

enum BeadsVersion {
    AuditSeq(u64),       // sequence number of latest audit sentinel on epic
    UpdatedAt(SystemTime), // fallback if audit seq unavailable
}
```

---

## Migration Plan

Each PR is independently reversible. Behavior-changing PRs are gated by a flag in `.spur/config.toml` (`plan.substrate_migration.<step_name>`). Flags default ON in dev, gated rollout in prod.

### PR1 — Idempotency keys (additive)
**Scope:** Add `client_idempotency_key: Option<String>` to `submit_plan_def` (`tools.rs:818`). Persist `key → plan_id` in beads (synthetic `spur:dedup:<hash>` issue with the plan_id in body). Resolve at the top of `handle_submit_plan` (`server.rs:5344`).
**Behavior change:** None for callers that omit the key. Callers that supply it get safe retries.
**Reversibility:** Trivial — schema is additive, server logic guarded by `Option`.
**Tests:** Two `submit_plan` calls with same key → same plan_id.

### PR2 — `active_plans` versioned cache (additive)
**Scope:** Wrap `PlanState` in `CachedPlan { state, beads_version }`. Add `BeadsVersion` derivation. `load_or_project_plan` validates version on cache hit and re-projects on mismatch. No write-path changes yet.
**Behavior change:** Reads silently re-project on stale cache. No user-visible difference.
**Reversibility:** Wrap → unwrap.
**Tests:** Concurrent reconciler tick + `review_task` → cache always converges to latest beads.

### PR3 — Make beads writes non-advisory in `handle_review_task`
**Scope:** Replace `warn!`-and-swallow in `apply_issue_update` failure paths (`plan/mod.rs:4832` and surrounding) with retry + invalidate + hard error. Decision applied to cache only after beads write confirms.
**Behavior change:** Brains see `apply_issue_update` failures as errors instead of silent success.
**Reversibility:** Feature-flagged; fall back to advisory behavior when flag off.
**Tests:** Inject beads write failure → `review_task` returns error, cache state matches beads.

### PR4 — Persist `dispatched_base_oid` before worker spawn
**Scope:** In `worker_attempt.rs` (after `create_worktree_v2`, before agent session), write the OID as a beads label `spur:dispatched-base-oid:<oid>` on the task issue. The existing `watch::Sender` becomes a fast-path optimization; the beads label is the durable record.
**Behavior change:** `recover_orphaned_dispatch` reads from beads label (preferring it over watch) — works even if worker panicked between worktree creation and oneshot send.
**Reversibility:** Additive write; existing watch path unchanged.
**Tests:** Kill worker between worktree and session start → `recover_orphaned_dispatch` succeeds.

### PR5 — Fix `plan_truncate_and_restart` to substrate path
**Scope:** Extract substrate-construction logic from `handle_submit_plan` into `submit_plan_as_epic_internal` (or similar). Rewrite `handle_plan_truncate_and_restart` (`server.rs:6477`) to call it, producing a beads-backed child plan. Auto-generate `epic_title` from parent epic + staging branch.
**Behavior change:** Truncate-and-restart now produces a durable child plan, not an ephemeral one. (This is also a latent bug fix — kimi flagged the current behavior as a defect independent of the migration.)
**Reversibility:** New code path; old `submit_plan_internal` still callable.
**Tests:** Migrated `handle_plan_truncate_and_restart_happy_path` (`server.rs:8681`) using mock-PM fixture.

### PR6 — Auto-generate `epic_title` if missing
**Scope:** Strip 60 chars from `tasks[0].task` if `epic_title` omitted. Removes the friction blocker for callers that omit the field.
**Behavior change:** `submit_plan(tasks=[…])` without `epic_title` no longer rejects.
**Reversibility:** Trivial; `Option::or_else`.
**Tests:** Submit without title → epic created with auto-title.

### PR7 — Default `persist_as_epic` to `true`
**Scope:** Flip default. Document migration in tool description. Brains using the default get persistent behavior.
**Behavior change:** All new plans are persistent. Brains explicitly passing `false` still get ephemeral (deprecation phase).
**Reversibility:** Flag flip.
**Tests:** Run full integration suite — both flag values must pass during deprecation window.

### PR8 — Reject `persist_as_epic=false` (deprecation enforcement)
**Scope:** Hard-error on `persist_as_epic=false`. Emit deprecation telemetry. Brains must update prompts before this PR; PR7 is the warning, this PR is the enforcement.
**Behavior change:** Old brain prompts that explicitly set `false` break. Telemetry from PR7 informs timing.
**Reversibility:** Flip back to PR7's behavior.
**Tests:** `submit_plan(..., persist_as_epic=false)` → 400 with migration guidance.

### PR9 — Delete ephemeral path
**Scope:**
- `run_plan` (`plan/mod.rs:3151`)
- `spawn_ephemeral_plan_runner` (`server.rs:2456`)
- `submit_plan_internal` (`server.rs:5299`) — only after PR5 has migrated `plan_truncate_and_restart` off it
- `apply_worker_failure_status` (`plan/mod.rs:2260`)
- `AUTO_RETRY_BUDGET` constant (`plan/mod.rs:805`)
- `should_auto_retry` ephemeral usage (keep persistent usage at `plan/mod.rs:2426`)
- `WorkerFailureAction` enum (`plan/mod.rs:2232`)
- `persist_as_epic`, `epic_title`, `epic_body` fields from `submit_plan_def`
- ephemeral-only short-circuits in `load_or_project_plan` (`server.rs:7189`)

**Keep:**
- `EscalatedToBrain` status (verified by gemini at `projector.rs:376` — beads-projected, not ephemeral-coupled)
- `MAX_ATTEMPTS = 3` (review-driven retry, persistent-path-relevant)
- `should_auto_retry` for the persistent retry path

**Behavior change:** Single-path execution. Cleaner code, fewer match arms.
**Reversibility:** A single revert; PRs 1–8 stand alone if PR9 is rolled back.
**Tests:** Ephemeral-only tests deleted; coverage from PRs 1–8 is sufficient.

### PR10 — Audit & migrate tests
**Scope:** Convert remaining ephemeral-flavored tests to mock-PM reconciler harness. Ensure `MockPm` exists and is exercised. Audit all brain↔worker transport (`mpsc::Sender<DelegationRequest>`, `oneshot::Sender<DelegationResult>`, `watch::Sender<Option<String>>`) for any case where a state mutation is observable in-process before being durable in beads.
**Behavior change:** Test suite migrated; any audit-found state-channel violations fixed.
**Reversibility:** Test changes are revertible; channel fixes are individual mini-PRs.

### PR11 (optional, post-migration) — Reduce `active_plans` to LRU
**Scope:** With INV-S2 holding, the cache can shrink. Move from unbounded `HashMap` to bounded `LruCache`. Verify wipe-and-rehydrate works under load.
**Behavior change:** Memory bounded; cold reads pay re-projection cost.
**Tests:** Load test with cache below working-set; correctness must hold.

---

## Test Strategy

| Layer | Test approach |
|---|---|
| Validation (DAG, cycle, sibling-overlap) | Existing unit tests in `plan/mod.rs` — unchanged |
| Idempotency (PR1) | Two-call same-key returns same id; persist across simulated brain restart |
| Cache versioning (PR2) | Concurrent reconciler tick + review_task; assert cache converges; assert wipe-and-rehydrate produces identical state |
| Non-advisory writes (PR3) | Inject `apply_issue_update` failure; assert `review_task` returns error; assert cache state == beads |
| Pre-spawn OID (PR4) | Kill worker between worktree creation and session start; `recover_orphaned_dispatch` succeeds |
| Truncate-restart (PR5) | New mock-PM-based test for `handle_plan_truncate_and_restart_happy_path`; verify child plan is beads-backed |
| Auto-title (PR6) | Submit without `epic_title`; verify generated title; verify reject-on-empty-tasks |
| Default flip (PR7) | Telemetry counter for `persist_as_epic=false` calls during deprecation window |
| Hard-reject (PR8) | `persist_as_epic=false` returns deprecation error |
| Deletion (PR9) | Existing integration suite passes with ephemeral code removed |
| Audit (PR10) | Manual code review checklist + grep for any `mpsc/oneshot/watch` carrying authoritative state |

**Mock-PM harness:** Build a `MockPm` implementation of `PmService` that stores issues + labels + comments in memory, derives a `BeadsVersion` from comment count, supports concurrent access. Used by all post-PR5 tests. Replaces all uses of `__test_install_plan` in `plan/mod.rs` test fixtures.

---

## Risks & Residuals

These remain after migration completes. They are accepted under the principle.

| Risk | Severity | Mitigation |
|---|---|---|
| Beads PM is global SPOF — outage stalls all plan execution | HIGH | Reconciler retry/backoff on beads errors; surface in TUI status |
| Reconciler bug = 100% blast radius | MEDIUM | Watchdog auto-restart; integration tests verify dispatch survives reconciler restart; fuzz the tick loop |
| Beads write latency is the hard floor for state-transition latency | MEDIUM | Accepted by design; if a specific transition dominates, batch beads writes inside the two-phase commit |
| Cache invalidation races (read-through projection + concurrent writers) | MEDIUM | `BeadsVersion` token + retry-on-mismatch; bounded retry count; if exceeded, return error to caller |
| PR2 cache version mechanism does not invalidate on task-level transitions until PR3 lands; mitigated by `versioned_cache_serve` feature flag default-OFF until PR3 ships | HIGH | Gated by config key; flip ON simultaneously with PR3 |
| Audit comment growth not GC'd → projector O(N) scan slows over plan lifetime | LOW | Snapshot-audit compaction (out of scope for this spec; track as follow-up if measurable) |
| Idempotency dedup map grows unboundedly in beads | LOW | TTL on dedup entries (e.g. 24h); document bound in spec |
| INV-S6 partial-failure orphan: `record()` failure after epic build leaves no dedup entry; retry creates a second epic | MED | Detect via dedup-entry lifecycle audit; closure deferred to PR3 (two-phase commit pattern) |
| INV-S6 concurrent race: two concurrent same-key submits both build epics; second `record()` collides but server returns the new plan_id | MED | beads lacks atomic CAS at issue level; accepted residual; mitigate via brain-side serialization of same-key submits |

---

## Open Questions

1. **`BeadsVersion` token shape.** Audit sentinel sequence number (preferred — monotonic per-issue), comment count, or `updated_at` timestamp? Decision needed before PR2.
2. **Idempotency map storage.** Synthetic `spur:dedup` epic with one issue per key, OR labeled audit comments on a single dedup ledger issue? Decision needed before PR1.
3. **Two-phase commit retry policy.** What's the bound? Three attempts with exponential backoff (100ms, 500ms, 2s)? Configurable? Decision needed before PR3.
4. **Telemetry plan for PR7→PR8 deprecation window.** What signal tells us all brains have migrated? Counter on the `persist_as_epic=false` codepath, exposed via existing metrics? Decision needed before PR7.
5. **Mock-PM placement.** New `crates/spur-pm/src/mock.rs` behind a `test-util` feature, or test-only module under `crates/spur-mcp/src/plan/test_util.rs`? Decision needed before PR5.
6. **Default for `client_idempotency_key`.** Required, or auto-derived from `(brain_session_id, task_dag_hash)` if omitted? Auto-derivation gives idempotency for free but couples to brain session identity.

---

## What this spec explicitly rejects

For posterity — choices considered and rejected:

- **Gemini's prior recommendation** (run_plan as universal dispatcher, demote reconciler, beads as journal) — inverts the principle.
- **Local-dev `PmLike` escape hatch** — every prior reviewer initially proposed it; principle rejects it.
- **Big-bang deletion of ephemeral path** — internal reviewer's blocker is correct: deletion before cache hardening produces a system that violates the principle worse than the current dual-mode one.
- **Splitting `submit_plan` into `submit_plan_ephemeral` and `submit_plan_persisted`** — proposed in the original RCA's open questions; rejected because it codifies the dual-mode at the contract layer.
- **Latency optimization of the reconciler tick interval** — accepted cost; out of scope.

---

## References

- RCA: `docs/rca/2026-05-10-submit-plan-end-to-end-flow.md`
- Kimi v1 simplification review: `docs/rca/2026-05-10-submit-plan-end-to-end-flow-simplification-review.md`
- Gemini v1 first-principles eval: `docs/rca/2026-05-10-submit-plan-first-principles-evaluation.md`
- Kimi v2 substrate-principle review: `docs/rca/2026-05-10-submit-plan-substrate-principle-review-kimi.md`
- Gemini v2 substrate-principle course-correction: `docs/rca/2026-05-10-submit-plan-substrate-principle-review-gemini.md`
- Companion: `docs/superpowers/specs/2026-05-05-spur-graph-engine-design.md` (graph engine, parallel substrate-discipline cutover)
- Companion: `docs/superpowers/specs/2026-05-05-beads_rust-direct-crate-dep-design.md` (BeadsCrateAdapter — substrate I/O primitive)

---

## Change log

| Date | Change |
|---|---|
| 2026-05-10 | Initial draft, synthesized from three reviews under beads-first principle |
