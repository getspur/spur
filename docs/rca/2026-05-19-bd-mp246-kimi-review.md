# bd-mp246 Review — Kimi (Pass 2 of 2)

## Section 1 — Verdict

**Approve-with-changes.** P1 fixes the proven telemetry gap and P3 adds useful counters, but the unbounded global outcome growth from per-plan `NoReadyTasks` amplification and the single-tick counter test gap need addressing before this commit is production-safe at scale.

## Section 2 — Confirmed-Correct Items

- `NoReadyTasks` is now recorded per-plan in the global enumeration loop (`ready.rs:72-73`), closing the silent-skip hole identified in the RCA.
- Counters are incremented in both the plan-scoped branch (`ready.rs:36`) and the global branch (`ready.rs:82`) and correctly reset in `mark_tick` (`outcomes.rs:272-276`).
- `seen_plan_ids` dedup prevents redundant `list_ready` calls for duplicate epics carrying the same plan-id label (`ready.rs:61-63`).
- The test-only clippy fix in `worker_server_audit.rs` correctly scopes `MutexGuard` drops before await points (`worker_server_audit.rs:415-432`, `536-553`).
- Existing reconciler test suite (33 tests) remains green; no regressions in prop-tests or lease logic.

## Section 3 — Specific Concerns

### 1. `NoReadyReason::NoMatchingRows` is semantically imprecise for global empty-ready diagnosis
**Severity: important**  
`record_no_ready` hardcodes `NoMatchingRows` at `mod.rs:860`. In the global path, `plan_summaries.is_empty()` (`ready.rs:72`) means the plan-id filter returned zero ready issues, but that could be because (a) all tasks are already closed/done, (b) all tasks are blocked, deferred, or pinned, or (c) a genuine DB visibility defect like the one suspected in bd-mp246.  
**Cite:** `mod.rs:856-860`, `ready.rs:72-73`.  
**Concrete fix:** Either add a new `NoReadyReason::PlanEnumeratedButEmpty` for the global path, or include a `ready_filter_explanation` field in `NoReadyTasks` so operators can distinguish "plan has no open tasks" from "plan should have tasks but the query hid them."

### 2. Global `recent_outcomes` is unbounded; per-plan transition rings evict real history in minutes
**Severity: critical**  
`OutcomeBuffer::push_transition` enforces `TRANSITION_RING_CAP = 64` per plan (`outcomes.rs:6,172-180`), but `OutcomeStore::global_recent_outcomes()` concatenates *all* per-plan buffers with **no global cap** (`outcomes.rs:424-433`). Every tick now emits one `NoReadyTasks` per idle plan.  
**Cite:** `outcomes.rs:424-433`, `outcomes.rs:6`, `ready.rs:72-73`.  
**Concrete fix:** (a) Cap `global_recent_outcomes()` to a fixed window (e.g., last 256 transitions globally), or (b) rate-limit `NoReadyTasks` per plan to once per `STUCK_DURATION` (120 s) unless the reason changes, so idle plans do not drown the buffer.

### 3. `tick_once` counter reset is safe inside `run()` but raceable if called externally
**Severity: important**  
The `run()` loop at `mod.rs:976-1030` awaits `tick_once()` to completion before selecting again, so `fast_forward` notifications cannot spawn overlapping ticks. However, `tick_once` is `pub` and tests call it directly. If any production path ever calls it concurrently, `mark_tick` (`mod.rs:840-843`) can reset counters while `record_tick_plans_enumerated` (`mod.rs:845-850`) or `record_dispatched` (`outcomes.rs:354-355`) is incrementing them.  
**Cite:** `mod.rs:976-1030`, `mod.rs:840-850`, `outcomes.rs:272-276`.  
**Concrete fix:** Document the invariant that `tick_once` must not be invoked concurrently outside `run()`, or wrap the tick body in a `tokio::sync::Mutex<()>` held for the duration of `tick_once`.

### 4. Counter test does not verify reset semantics across multiple ticks
**Severity: important**  
`global_reconciler_status_reports_plans_enumerated_and_dispatched_per_tick` (`tests.rs:1000-1028`) calls `tick_once()` exactly once. A single-tick snapshot cannot falsify the hypothesis that counters accumulate indefinitely instead of resetting on `mark_tick`.  
**Cite:** `tests.rs:1000-1028`, `tests.rs:1021`.  
**Concrete fix:** Add a second `tick_once()` call (with P2 already in-flight so no new dispatch occurs) and assert `last_tick_plans_dispatched == 0` and `last_tick_plans_enumerated == 2`, proving reset happened.

### 5. `seen_plan_ids` dedup binds to the first epic returned by `list_issues`, not the canonical one
**Severity: nit**  
`ready.rs:61-63` skips later epics sharing a plan-id. `find_plan_epic` in `server/sync.rs:33-102` canonicalizes duplicates by inspecting `PlanSubmit` audit comments. The reconciler, however, uses the epic only to extract the `plan_id` label; the downstream `list_ready` call is plan-scoped, and `project_plan_from_beads` separately canonicalizes when it needs the epic. Therefore non-canonical binding in the enumeration loop does not affect dispatch correctness today.  
**Cite:** `ready.rs:61-63`, `server/sync.rs:33-102`.  
**Concrete fix:** None required unless future code starts using the enumerated epic ID for dispatch decisions.

## Section 4 — Quantitative Buffer-Bloat Analysis

**Parameters:**
- `TRANSITION_RING_CAP = 64` per plan (`outcomes.rs:6`).
- Default `base_interval = 3 s`, `idle_ceiling = 30 s` (`mod.rs:707-708`).
- Assume 200 plan-complete epics, 1 has ready work, 199 are idle.

**Per-plan behavior:**
- Each idle plan receives one `NoReadyTasks` entry in its own 64-slot transition ring per tick.
- With `did_work = false` (idle system), tick interval backs off to 30 s. Ring fills in `64 × 30 s = 1,920 s` ≈ **32 minutes**.
- With `did_work = true` (active system), interval stays at 3 s. Ring fills in `64 × 3 s = 192 s` ≈ **3.2 minutes**.
- Once the ring is full, each new `NoReadyTasks` evicts the oldest transition for that *same* plan. Prior per-plan history (e.g., an earlier `NoDispatchContext` or a rare `Dispatched` outcome) disappears.

**Global behavior:**
- `global_recent_outcomes()` concatenates all per-plan buffers with **no limit** (`outcomes.rs:424-433`).
- Upper bound global entries = 200 plans × 64 ring slots = **12,800 entries**, plus any `latest_per_task` dispatches.
- Serialized size estimate: `NoReadyTasks` JSON ≈ 200 bytes. 12,800 × 200 B = **2.56 MB** per `get_reconciler_status` poll.
- Operational impact: status polls become multi-megabyte JSON payloads; operators using `get_reconciler_status` to debug plans will receive a wall of `NoReadyTasks` noise with no global truncation.

**Conclusion:** The per-plan ring cap provides local boundedness, but the global merge is unbounded and the per-plan ring is too short to retain meaningful history for actively-idle plans. The fix in concern 2 (global cap or rate-limit) is required before this ships to a workspace with >50 active plans.

## Section 5 — Operator Playbook for the Next bd-mp246 Occurrence

With the counters from P3, operators can now isolate the dispatch root cause in 3–5 steps:

1. **Prove tick traversal:** Poll `get_reconciler_status` and check `last_tick_plans_enumerated`. If it is >0 and includes the failing plan’s tick, the reconciler *is* visiting the plan. If it is 0 for multiple consecutive polls, the reconciler loop is stalled (liveness bug, not dispatch gating).

2. **Prove list_ready emptiness:** Look at `recent_outcomes` for the specific plan-id. If `NoReadyTasks { reason: NoMatchingRows }` appears repeatedly, the plan was enumerated but `list_ready` returned empty. This points the finger at the beads query layer (visibility, blocked cache, or filter logic), not the reconciler’s dispatch gate.

3. **Check for dispatch gate blocking:** If `last_tick_plans_dispatched > 0` for other plans but 0 for the failing plan, and the failing plan has *no* `NoReadyTasks` in its recent outcomes, the task passed `list_ready` but was rejected later in hydration, projection, or lease checking. Inspect `stuck_tasks` for `SkipReason` entries.

4. **Cross-reference timing:** Compare the plan’s `plan-complete` audit timestamp (in beads comments) against `last_tick_at`. If `last_tick_at` is earlier than `plan-complete`, the plan became ready after the most recent tick; wait or force-reclaim. If `last_tick_at` is later and `NoReadyTasks` is absent, the plan was never enumerated—check whether the epic carries `spur:plan-complete` and is not closed.

5. **Capture a DB snapshot before cleanup:** The moment a plan is suspected of being "invisible," run `br ready --label spur:plan-id:<id>` (or equivalent direct `list_ready` call) and save the output. If the direct query returns tasks while the reconciler records `NoReadyTasks`, the bug is in the reconciler’s filter construction (e.g., limit or label mismatch). If the direct query also returns empty, the bug is in beads visibility or the issue’s ready state.
