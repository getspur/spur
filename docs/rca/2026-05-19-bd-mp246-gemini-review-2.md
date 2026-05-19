# Review: bd-mp246 follow-ups (cross-brain + counter + cap)

## 1. Verdict
Approve with changes: Critical fix required for GLOBAL_RECENT_CAP truncation rate, and unowned plan filtering needs tightening before landing.

## 2. Confirmed Correct
* **Counter semantics:** Renaming to last_tick_tasks_dispatched correctly aligns the code with its per-task increment behavior and improves operational accuracy.
* **Filter placement:** The OwnedByOther check is correctly placed *before* the expensive adv.list_ready I/O call (crates/spur-mcp/src/plan/reconciler/ready.rs), fixing the cross-brain performance issue Kimi noted.
* **Global sort ordering:** global_recent_outcomes now correctly sorts newest-first (DESC), which is strictly better for MCP log consumers and TUI tailing.
* **Truncation testing:** global_recent_outcomes_is_capped_across_plans effectively proves the global cap invariant prevents unbounded memory growth.

## 3. Specific Concerns

* **CRITICAL: GLOBAL_RECENT_CAP = 256 truncates real history almost instantly**
  * *Location:* crates/spur-mcp/src/plan/outcomes.rs:7
  * *Issue:* A workspace with 200 idle plans produces 200 NoReadyTasks outcomes *per tick*. A cap of 256 retains only ~1.28 ticks of history (~4 seconds). Real dispatch outcomes from active plans will be completely pushed out of the MCP status buffer by idle-plan noise before an operator can read them.
  * *Fix:* Increase GLOBAL_RECENT_CAP to at least max_plans * desired_ticks (e.g., 2000 or 5000) OR rate-limit NoReadyTasks emission to once per STUCK_DURATION per plan.

* **IMPORTANT: Unowned and Ambiguous plans still spam NoReadyTasks telemetry**
  * *Location:* crates/spur-mcp/src/plan/reconciler/ready.rs:64-73
  * *Issue:* The new filter ONLY skips PlanOwnerMatch::OwnedByOther. If a plan is Unowned (e.g. freshly created) or Ambiguous, it falls through, hits list_ready, and emits NoReadyTasks. If multiple brains are online, *all* of them will independently spam telemetry for the same Unowned plan on every tick.
  * *Fix:* Broaden the skip logic: if !matches!(classify_owner(...), PlanOwnerMatch::OwnedByCurrent) (assuming a brain should only evaluate its own plans for readiness).

* **IMPORTANT: External schema breakage from counter rename**
  * *Location:* crates/spur-mcp/src/plan/outcomes.rs:113
  * *Issue:* last_tick_plans_dispatched was completely removed from ReconcilerStatus. Any external dashboards, MCP scripts, or monitoring expecting this field in the JSON-RPC response will break.
  * *Fix:* Ensure downstream consumers are updated, or retain last_tick_plans_dispatched as a computed HashSet-backed field for backwards compatibility alongside the new task counter.

* **NIT: ASC/DESC inconsistency across snapshot() vs global_recent_outcomes()**
  * *Location:* crates/spur-mcp/src/plan/outcomes.rs:432 vs OutcomeBuffer::snapshot()
  * *Issue:* OutcomeBuffer::snapshot() sorts ASC (oldest-first). global_recent_outcomes() calls .snapshot() on each plan, does a flat-map, and re-sorts the entire list DESC. This does redundant sorting and creates a semantic inconsistency where recent_outcomes(plan) is ASC but reconciler_status().recent_outcomes is DESC.
  * *Fix:* Standardize on newest-first (DESC) for both methods.

## 4. Self-Correction (Prior Gemini Review)
In my previous review (2026-05-19-bd-mp246-gemini-review.md), I strongly recommended using a HashSet to track unique plan_ids dispatched to "eliminate counting ambiguities." I was wrong. Codex's choice to simply rename the metric to tasks_dispatched was the right architectural call. Operations needs to know true dispatch throughput (how many workers were spawned this tick). Masking 50 parallel tasks under a single plan_id using a HashSet would have destroyed vital operational visibility into worker pressure.

## 5. Things That Should NOT Block Landing
* **Observe-only Reconciler Bypass:** if let Some(dispatch) = self.dispatch.as_ref() bypasses the cross-brain filter completely when dispatch is None. This is acceptable because Reconciler::new is never constructed without a dispatch context in production (crates/spur-mcp/src/server/mod.rs:546 always passes Some(dispatch)). The regression is theoretical.
* **Stable Sort Reversal on Identical Timestamps:** Reversing the chronological order of equal timestamps in outcomes.sort_by is harmless. HashMap iteration order already makes identical timestamps from different plans interleave non-deterministically anyway.
