# bd-mp246 Review — Kimi Pass 2 of 2

## Section 1 — Verdict

**Approve-with-changes.** The three must-fix follow-ups (cross-brain filter, counter rename, global cap) are correctly implemented, but the per-plan ring eviction window and missing multi-tick counter reset test still need closure before this is production-safe at scale.

## Section 2 — Confirmed-Correct Items

- Cross-brain filter skips `OwnedByOther` plans **before** `list_ready`, eliminating false-alarm `NoReadyTasks` for foreign brains (`ready.rs:64-72`).
- Counter rename `last_tick_plans_dispatched → last_tick_tasks_dispatched` is semantically accurate and propagated through `ReconcilerStatus` (`outcomes.rs:113,269,356,468`).
- Global cap at `GLOBAL_RECENT_CAP = 256` with newest-first sort/truncate eliminates the 2.56 MB unbounded payload (`outcomes.rs:7,432-433`).
- `global_reconciler_skips_other_brain_plan_without_emitting_no_ready` is a solid regression test for the cross-brain misfire (`tests.rs:1027`).
- `global_reconciler_status_reports_tasks_dispatched_per_tick` validates the renamed counter counts tasks, not plans (`tests.rs:1115`).

## Section 3 — Specific Concerns

### 3.1 `GLOBAL_RECENT_CAP` is too small for useful cross-plan debugging
**Severity: important**
With 256 entries across 200 plans, the global view retains ~1.28 entries per plan. In an active workspace, one tick emits 200 `NoReadyTasks`; the global cap fills in 1.3 ticks. A `Dispatched` outcome from Plan A is evicted by idle-plan noise from Plans B–Z within seconds.
**Cite:** `outcomes.rs:7`, `outcomes.rs:432-433`.
**Concrete fix:** Either raise `GLOBAL_RECENT_CAP` to 1024 (still ~200 KB), or implement write-time rate-limiting for `NoReadyTasks` (concern 3.3). Falsifiable target: in a 200-plan workspace where a bug recurs, the failing plan's most recent `NoReadyTasks` must remain visible in `get_reconciler_status` for ≥30 minutes.

### 3.2 Per-plan ring eviction window unchanged; 3-minute erase remains
**Severity: critical**
`TRANSITION_RING_CAP = 64` was not touched (`outcomes.rs:6`). On an active system (3 s ticks), a single idle plan's ring fills in `64 × 3 s = 192 s` ≈ 3.2 min. Any prior `Dispatched` or `NoDispatchContext` outcome for that plan is lost once the ring fills.
**Cite:** `outcomes.rs:6`, `outcomes.rs:173-180`.
**Concrete fix:** Increase `TRANSITION_RING_CAP` to 256 or 512, or rate-limit `NoReadyTasks` per plan to once per `STUCK_DURATION` (120 s) unless the reason changes.

### 3.3 Rate-limiting `NoReadyTasks` at write time would have been the better fix
**Severity: important**
The commit chose (a) global cap over (b) per-plan rate-limit. Quantified tradeoff: with (a) alone, 200 idle plans × 1 tick/3 s = 200 `NoReadyTasks`/3 s = 4,000/minute. The 256 global cap means any non-`NoReadyTasks` outcome survives <4 seconds in the global view. With (b), rate-limiting to 1 `NoReadyTasks` per plan per 120 s reduces write pressure to 100/minute; the 256 cap then preserves 2.5 hours of mixed history, keeping `Dispatched` outcomes visible indefinitely.
**Cite:** `outcomes.rs:7`, `outcomes.rs:307-319`.
**Concrete fix:** Add dedup logic in `record_no_ready`: if the plan's most recent transition is already `NoReadyTasks { reason: NoMatchingRows }` within `STUCK_DURATION`, skip recording. This preserves real history without silencing genuine state changes.

### 3.4 Multi-tick counter reset test still missing
**Severity: important**
The commit added single-tick assertions but no test verifying counters reset across ticks. This slipped through by oversight, not by design.
**Cite:** `tests.rs:1084`, `tests.rs:1115`.
**Concrete fix:** Add ≤15 lines after the existing single-tick test:
```rust
reconciler.tick_once().await.expect("second tick");
let status = outcomes_store.lock().await.reconciler_status();
assert_eq!(status.last_tick_tasks_dispatched, 0);
assert_eq!(status.last_tick_plans_enumerated, 2);
```

### 3.5 `NoMatchingRows` semantic ambiguity partially resolved, residual ambiguity remains
**Severity: nit**
The cross-brain filter fixed the "other brain's plan" false positive. However, `NoMatchingRows` still conflates three states for our own plans: (a) all tasks closed/done, (b) tasks exist but are blocked/deferred, (c) genuine DB visibility defect. Operators cannot distinguish "plan finished" from "plan stuck" without manually inspecting beads.
**Cite:** `ready.rs:82-84`, `outcomes.rs:307-319`.
**Concrete fix:** Add `NoReadyReason::PlanCompleteNoOpenTasks` when projection shows zero ready tasks, reserving `NoMatchingRows` for when tasks *should* be ready but `list_ready` returned empty.

### 3.6 Buffer-bloat regression test does not verify newest-first retention
**Severity: important**
`global_recent_outcomes_is_capped_across_plans` (`outcomes.rs:1085`) asserts `len() <= 256` but does not verify truncation dropped the *oldest* entries. If the sort were accidentally reversed to ASC, the test would still pass while keeping stale outcomes.
**Cite:** `outcomes.rs:1085-1098`.
**Concrete fix:** Add after the existing assertion:
```rust
let outcomes = store.global_recent_outcomes();
assert!(outcomes.iter().any(|o| o.timestamp() == ts(499)));
assert!(!outcomes.iter().any(|o| o.timestamp() == ts(0)));
```

## Section 4 — Self-Correction: What the Prior Review Got Wrong

My prior review concern 3 ("`tick_once` counter reset is raceable if called externally") was **overscoped**. `tick_once` is indeed `pub`, but the only production caller is the sequential `run()` loop at `mod.rs:976-1030`. There is no actual concurrent production path; the race concern was theoretical and should have been a documentation nit, not an "important" severity item.

I also conflated counter increment sites in the prior Section 2. I implied both counters were incremented in `ready.rs:36` and `ready.rs:82`, but `last_tick_tasks_dispatched` is incremented only inside `record_dispatched` at `outcomes.rs:356`, never in `ready.rs`.

## Section 5 — Updated Operator Playbook (Incremental Delta)

1. **Prove tick traversal:** Poll `get_reconciler_status` and check `last_tick_plans_enumerated`. If >0 and includes the failing plan, the reconciler *is* visiting it.

2. **Prove list_ready emptiness:** Look at **per-plan** `recent_outcomes(plan_id)` (not the global view, which is now heavily truncated). The per-plan 64-slot ring retains ~3 min of history at 3 s ticks. If `NoReadyTasks { NoMatchingRows }` appears repeatedly, the plan is enumerated but beads returns empty.

3. **Distinguish "plan finished" from "plan blocked":** If `NoReadyTasks` appears but the plan's epic is open, suspect beads visibility or blocked-task cache. If the epic is closed, the plan is genuinely complete.

4. **Check dispatch gate blocking:** If `last_tick_tasks_dispatched > 0` for other plans but 0 for the failing plan, and the failing plan has *no* `NoReadyTasks` in its per-plan ring, the task passed `list_ready` but was rejected in hydration, projection, or lease checking. Inspect `stuck_tasks`.

5. **Capture a DB snapshot before cleanup:** The moment a plan is suspected of being "invisible," run `br ready --label spur:plan-id:<id>` directly and save the output. If the direct query returns tasks while the reconciler records `NoReadyTasks`, the bug is in the reconciler's filter construction.
