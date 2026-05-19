# Review: bd-mp246 telemetry gap + tick-liveness counters

## 1. Verdict
Approve with changes (Request changes for specific telemetry noise and counter semantics before landing).

## 2. Confirmed Correct
* **Deduplication:** Skipping the second epic using `seen_plan_ids` in the global path is safe; `list_ready` queries by `plan_id` and therefore fetches tasks for all epics sharing that ID in one pass.
* **Counter Race-Safety:** Tick counters (`last_tick_plans_enumerated`, `last_tick_plans_dispatched`) in `outcomes.rs` are safe. `tick_once` runs sequentially per reconciler, and `mark_tick` is called strictly at the beginning of each loop.
* **Test Fix (worker_server_audit):** Scoping `sink.events.lock()` correctly resolves `clippy::await_holding_lock` without introducing a TOCTOU window. The test asserts a past event from memory before awaiting the backend, which is structurally safe.

## 3. Specific Concerns

* **CRITICAL: Misleading telemetry for plans owned by other brains**
  * *Location:* `crates/spur-mcp/src/plan/reconciler/ready.rs` (lines added inside the `for epic in epics` block, near line 69-72 in diff)
  * *Issue:* The new code calls `self.record_no_ready(Some(plan_id))` if `plan_summaries.is_empty()`. Because the ownership check (`plan_allows_dispatch`) happens *inside* the subsequent loop over summaries, it is never executed for empty lists. Thus, if a plan is actively owned by Another Brain but temporarily has no ready tasks, this brain will globally log `NoReadyTasks(NoMatchingRows)` for it, generating false alarms and severe telemetry noise.
  * *Fix:* Check ownership directly in the global `for epic in epics` loop *before* querying `adv.list_ready(...)`. Use `crate::plan::ownership::classify_owner(&epic.labels, ...)` and `continue` if the epic is `OwnedByOther`.

* **IMPORTANT: `last_tick_plans_dispatched` tracks tasks, not plans**
  * *Location:* `crates/spur-mcp/src/plan/outcomes.rs` (in `record_dispatch`, around line 355)
  * *Issue:* The counter is incremented inside `record_dispatch`, which is called once per *task*. If a plan yields 5 ready tasks, the counter increments by 5, leading operators to falsely believe 5 unique plans were processed.
  * *Fix:* Rename the variable and struct field to `last_tick_tasks_dispatched` to reflect reality, or use a `HashSet` during the tick to track unique `plan_id`s dispatched.

## 4. Missing Test Coverage
* The test `global_reconciler_records_plan_no_ready_when_list_ready_empty_for_that_plan` bypasses actual `beads` behavior. By injecting `ScriptedReadyPm` to intercept `list_ready` and return empty, it solely validates the reconciler's telemetry logic. It does not verify if the real `MockPm` or actual `beads` correctly evaluates readiness in the user's scenario.
  * There is no test exercising the global path's interaction with a plan owned by *another brain* that has zero ready tasks. This gap allowed the P1 cross-brain telemetry bug to slip through.

## 5. Things I Would Have Done Differently
* I would have early-exited `OwnedByOther` epics before making the `list_ready` I/O call in the global loop, saving unnecessary backend queries for other brains' plans.
* Instead of adding scalar tick counters to `ReconcilerStatus`, I would have aggregated a `HashSet<String>`'s length of enumerated/dispatched `plan_id`s. This entirely eliminates task vs. plan counting ambiguities.
* I would have written the regression test using standard `MockPm` task states (e.g., seeding tasks that are genuinely blocked) rather than relying on a hardcoded interceptor (`ScriptedReadyPm`) that ignores the state of the mock PM.