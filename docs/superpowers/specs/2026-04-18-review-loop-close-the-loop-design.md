# Review Loop — Close-the-Loop Bundle

**Status:** design (v2)
**Date:** 2026-04-18
**Depends on:** `2026-04-17-brain-review-feedback-loop-phase2-design.md`, `2026-04-17-execute-epic-phase2.5-design.md`
**Area:** `spur-mcp` plan/review · `spur-pm` PmService
**Predecessor plan (the one this spec fixes):** `2026-04-17-execute-epic-phase2.5.md`

## Problem

Phase 2 (review loop) and Phase 2.5 (execute_epic) shipped. Live dogfooding on epic `bd-1mh` surfaced four holes in the brain → worker → review cycle:

1. **`request_changes` leaves no trace in beads.** `plan.rs:1042-1143` dispatches a new worker attempt via the in-process channel but writes nothing to the PM backend. A human auditor reading beads sees approve/reject comments but has zero record of the most-frequent review event. The enriched feedback lives only in `PlanTaskEntry.history` in RAM.
2. **`br update -s done` is rejected** by the beads config in use. Every approval emits `beads update failed: br retryable error after 2 attempts: Invalid status: done` as a warning — the task becomes `Approved` in `PlanState` while the beads issue stays `in_progress`. Silent drift between the two stores.
3. **`MAX_ATTEMPTS=3` creates a limbo state.** When `request_changes` is called on a task at `attempt == MAX_ATTEMPTS`, `review_task` returns `Err` (`plan.rs:1053-1057`) and leaves the task in `AwaitingReview` forever. `is_terminal_plan_status` (`plan.rs:912-917`) doesn't cover this case, so the plan itself never terminates. The brain sees an error string but has no structured path forward. **bd-1mh.1 is one attempt away from this state right now.**
4. **`get_task_diff` returned empty for bd-1mh.2** despite commit `95e8b73` existing on the worker branch — the brain had to review by walking git directly. Root cause is unknown; a fix cannot be designed until RCA is done.

### What's explicitly fine today

- In-process dispatch via `DelegationRequest` channel is the *signal* path; it is fast, correct, and tolerates beads outages. This spec does not touch it.
- The enriched-task prompt carries the feedback directly to the worker; beads is not on the critical path for worker context. We keep it that way.
- `PlanState` lives in RAM. Crash-restart durability is not in scope — no observed pain.

## Goals

1. **Beads becomes a complete audit trail** — every brain review decision (approve, reject, `request_changes`) produces a beads comment, best-effort, with warning-on-failure that does not block dispatch.
2. **Status mapping is configurable** — `PmService` accepts the "closed" status string at construction; default `"closed"`. `review_task` approve uses it.
3. **Plans always terminate** — `request_changes` at `MAX_ATTEMPTS` auto-transitions the task to `Rejected` (with a distinguishing feedback prefix) instead of returning `Err`. Reuses the existing rejection cascade and terminal state.
4. **RCA for diff-empty** — spike task produces an RCA doc; no code change in this spec.

## Non-goals

- **Durability** of PlanState (disk persistence, restart recovery). No observed restart pain.
- **New terminal states** (`Exhausted`, `Stalled`, `Abandoned`). Rejected in v2 review — auto-rejecting at MAX with a distinguishing feedback prefix carries the same information without new surface. See Revision history.
- **`mark_stalled` tool.** Rejected in v2 review — no observed use case that `reject` doesn't already handle. See Revision history.
- **Auto-stall on timer** (wall-clock SLA). Not needed without a stall state.
- **Per-plan MAX_ATTEMPTS override.** The const stays `3`.
- **Evidence gate on approve** (require diff hash / test name). No observed failure.
- **Comment provenance** (distinguish brain from operator in beads). Out of scope.
- **Denormalized review-thread tool.** Out of scope.
- **Worker → brain escalation channel.** Out of scope.
- **`get_task_diff` root-cause fix.** Spike delivers RCA only; fix lands in a follow-up spec if needed.
- **PmBackendInner::GitHub changes.** The closed-status config applies to beads only; GitHub's `close` semantics are separate.

## Design

Three code units + one spike.

### Unit 1 — `request_changes` writes a beads comment (after successful dispatch)

**Where:** `plan.rs:1042-1143`, inside the `"request_changes"` match arm.

**What:** **After** the `try_send` succeeds and the state mutation is committed, if `pm` is `Some` and `entry.spec.issue_id` is `Some`, build an `IssueUpdate` with only `comment` set and call `pm.update_issue(id, update).await`. Format:

```
Brain requested changes (attempt N/MAX):
<feedback>

Worker branch: <branch if present, else "(no branch yet)">
```

On error, push `"beads comment failed: {e}"` into `warnings`. Do NOT block anything — the signal path is already committed by this point.

**Why after `try_send` (not before):** the audit trail must reflect what actually happened. If the beads comment is written first and `try_send` fails, the operator sees "Brain requested changes — new attempt N+1" in beads but no worker attempt is running; the state has rolled back to `AwaitingReview` (Phase 2's snapshot-before-send pattern). The comment would lie. Writing after `try_send` success means: comment appears iff dispatch actually happened.

**Placement:** after `new_dispatches.push(...)` at `plan.rs:1142`, before the function-level event emit at `plan.rs:1179-1202`. The `warnings` vec is already being accumulated and is returned in the JSON response.

**Test:** `request_changes_writes_beads_comment_after_dispatch` — mock `PmService`, assert dispatch happens first (verify via send-order hook or sequencing assertion), assert `update_issue` was called with a comment matching the pattern.

**Out of scope for this unit:** enriching the beads comment with a structured summary of history (just the latest feedback + attempt counter + branch). Past-attempt trail stays in `entry.history` for now.

### Unit 2 — Configurable closed-status for PmService

**Where:** `spur-pm/src/service.rs`, `spur-pm/src/beads.rs`, `spur-mcp/src/plan.rs:985`, `spur-core/src/orchestrator.rs` (caller).

**What:**

1. Add a new optional parameter to `PmService::try_new`: `closed_status: Option<String>`. When `None`, default to `"closed"`. Threaded down into `BeadsAdapter` as a struct field.
2. `PmService` exposes `pub fn closed_status(&self) -> &str` returning the configured string.
3. `plan.rs:985` replaces the hardcoded `"done"` with `pm.closed_status().to_string()`.
4. Callers of `PmService::try_new` in `spur-core/src/orchestrator.rs` pass `None` (accepting the `"closed"` default); overrides can be wired through config in a follow-up if a second value is needed.

**Why a config value, not auto-detection:** querying beads for its status vocabulary at startup is a second CLI round-trip that can fail. A config string with a sensible default is simpler and deterministic. Users who hit this fix it once.

**Why a function parameter, not a config file entry:** YAGNI — we have one known user (the dogfood repo) and one known working value (`"closed"`). Wire plumbing (TOML key → SpurConfig → orchestrator → PmService) is premature until a second caller needs a different value.

**Test 1:** `closed_status_defaults_to_closed` — construct `PmService` without override, assert `closed_status() == "closed"`.
**Test 2:** `approve_uses_configured_closed_status` — mock backend, construct with `Some("resolved")`, call `review_task(approve)`, assert `update_issue` was called with `status: Some("resolved")`.

**Out of scope:** mapping per-workflow (e.g., `won't_fix`, `duplicate`). Only one closed-status is configurable.

### Unit 3 — Auto-reject `request_changes` at MAX_ATTEMPTS

**Where:** `plan.rs:1042-1057` (the MAX guard at the top of the `request_changes` branch).

**What:** When `entry.attempt >= MAX_ATTEMPTS`, instead of returning `Err`:

1. Construct an exhaustion-prefixed feedback string: `format!("retries exhausted ({N}/{MAX}): {fb}", ...)`.
2. Transition the task to `PlanTaskStatus::Rejected { feedback: Some(exhaustion_prefixed) }` (existing variant, no new surface).
3. Write a beads comment (best-effort, same pattern as Unit 1 + the existing reject branch): `"Brain rejected (retries exhausted {N}/{MAX}): {fb}"`. This is distinguishable from a first-pass reject by the prefix — an operator reading beads can tell the brain wanted more iterations but ran out of budget.
4. Run the existing rejection cascade: `mark_descendants_failed(task_id, state, &mut warnings)`.
5. Emit the existing `PlanTaskReviewed` event with `decision: "reject"` and the exhaustion-prefixed feedback. No new event variant.
6. Return `Ok(build_plan_status(...))` with `decision: "reject"` and a `warnings` entry noting `"auto-rejected: MAX_ATTEMPTS ({MAX}) reached"`.

**Why reject instead of a new Exhausted variant:** downstream consumers (is_terminal_plan_status, cascade, retry_plan_task, TUI, events) already handle `Rejected` correctly. Adding an `Exhausted` variant would force every match arm to be updated for a distinction only the feedback-prefix needs to make. YAGNI.

**Why prefix-distinguish in beads, not in state:** the state-machine consumer is code — it only cares that the task is terminal and non-approved. The distinction (brain ran out of budget vs. brain rejected on merit) is for the human reader, who parses the comment string, not the PlanTaskStatus enum.

**Test:** `request_changes_at_max_attempts_auto_rejects` — construct plan with task at `attempt = MAX_ATTEMPTS`, call `review_task(request_changes, "please rename X")`, assert:
- Status transitions to `Rejected`
- Feedback contains `"retries exhausted"`
- Beads comment was emitted with the `retries exhausted` prefix
- No new `DelegationRequest` was sent
- `is_terminal_plan_status` returns true
- Cascade descendants are marked `Failed` with `"upstream '<id>' rejected"` sentinel

### Unit 4 — Spike: RCA for `get_task_diff` empty-result

**Where:** no code change; deliverable is `docs/rca/2026-04-18-get-task-diff-empty.md`.

**What the spike must answer:**

1. Reproduction: does `get_task_diff(bd-1mh.2)` still return empty now, or was it a transient state?
2. Root cause: is the worker branch missing from the orchestrator's git view (fetch required)? Is the diff computed against the wrong base? Is a success path returning an empty string instead of an error?
3. Fix options: one-paragraph each, with cost estimate.
4. Recommendation: which follow-up to file (or "no fix needed, investigate further").

**Why a spike, not a fix:** earlier drafts pre-committed to "surface empty as error" without knowing the root cause. If the root cause is "no commits on branch" that's a valid empty state and surfacing as error is wrong. Spike first.

**Deliverable location:** `docs/rca/2026-04-18-get-task-diff-empty.md`. Format matches existing `docs/rca/2026-04-16-delegation-transport-mismatch.md`.

## Events

**No new events.** Unit 3's auto-reject uses the existing `PlanTaskReviewed { decision: "reject", ... }` event. Unit 1's comment write is a side effect with no event.

## Error handling

- **Beads write failure (any unit):** push to `warnings` in the response, continue. Signal path (dispatch, state transition) is independent.
- **Closed-status config missing:** default `"closed"`. No error at startup.
- **request_changes at MAX with pm missing:** Unit 3 still auto-rejects the state machine; beads comment is skipped (no pm); warning noted.
- **Event emit failure:** events are fire-and-forget via `sink.emit`; unchanged from Phase 2.

## Testing

One unit test per functional change (listed under each unit). Additionally:

- **`is_terminal_plan_status_unchanged`** — sanity test asserting the function's behavior hasn't shifted (since we're reusing `Rejected`, this should be a no-op regression check).

No new integration tests — units are tight, each fits in existing test harnesses.

## Data flow — before vs. after

```
Before (request_changes at attempt 2, MAX=3):
  brain → review_task("request_changes", fb)
    └─ state: AwaitingReview → Dispatched(new)
    └─ try_send enriched
    └─ (no beads write)

Before (request_changes at attempt 3, MAX=3):
  brain → review_task("request_changes", fb)
    └─ Err("task is at max attempts (3)")
    └─ state: AwaitingReview (unchanged — LIMBO)

Before (approve anywhere):
  brain → review_task("approve")
    └─ state: AwaitingReview → Approved
    ├─ pm.update_issue(status="done")  [FAILS: Invalid status]
    └─ cascade dispatch newly-ready
```

```
After (request_changes at attempt 2, MAX=3):
  brain → review_task("request_changes", fb)
    ├─ state: AwaitingReview → Dispatched(new)
    ├─ try_send enriched                                            [must succeed first]
    └─ pm.update_issue(comment="Brain requested changes (2/3): fb") [best-effort, after dispatch]

After (request_changes at attempt 3, MAX=3):
  brain → review_task("request_changes", fb)
    ├─ state: AwaitingReview → Rejected { feedback: "retries exhausted (3/3): fb" }
    ├─ cascade descendants → Failed { "upstream '{id}' rejected" }
    ├─ pm.update_issue(comment="Brain rejected (retries exhausted 3/3): fb") [best-effort]
    ├─ sink.emit(PlanTaskReviewed { decision: "reject", feedback: "retries exhausted..." })
    └─ Ok(plan_status with decision: "reject", warnings: ["auto-rejected: MAX_ATTEMPTS reached"])

After (approve anywhere):
  brain → review_task("approve")
    └─ state: AwaitingReview → Approved
    ├─ pm.update_issue(status="closed")                             [SUCCEEDS with configured value]
    └─ cascade dispatch newly-ready
```

## Files touched

| File | Units | Change |
|---|---|---|
| `crates/spur-mcp/src/plan.rs` | 1, 3 | `request_changes` beads comment write (after dispatch) · auto-reject at MAX |
| `crates/spur-pm/src/service.rs` | 2 | `closed_status()` accessor + constructor param |
| `crates/spur-pm/src/beads.rs` | 2 | `BeadsAdapter` carries closed_status field |
| `crates/spur-core/src/orchestrator.rs` | 2 | caller passes `None` to `PmService::try_new` |
| `docs/rca/2026-04-18-get-task-diff-empty.md` | 4 | new RCA (spike deliverable) |

Total estimate: **~95 LOC of production code, ~4 unit tests, 1 RCA doc.** No new enum variants, no new events, no new tools, no TUI changes.

## Success criteria

1. Running the `bd-1mh` replay end-to-end produces no `beads update failed: Invalid status` warnings.
2. `br show <issue_id>` shows a comment for every brain review decision on every attempt (approve, reject, request_changes, request_changes-at-MAX auto-reject).
3. A plan with a task that hits `request_changes` at MAX_ATTEMPTS terminates (overall status `"has_rejections"`) instead of blocking in AwaitingReview.
4. `docs/rca/2026-04-18-get-task-diff-empty.md` exists with reproduction + root cause + recommendation.

## Build order (for plan-writing)

1. **Unit 2** (closed-status config) — no state-machine change; unblocks Unit 1's comment writes from being overshadowed by approve warnings during tests.
2. **Unit 1** (request_changes beads comment after dispatch) — smallest functional change on top of Unit 2.
3. **Unit 3** (auto-reject at MAX) — reuses the rejection path; lands after Unit 1 so the beads-comment helper is factored and reusable.
4. **Unit 4** (spike) — can run in parallel with any of the above; deliverable is a doc.

## Out-of-scope reminders (re-stated for plan-writing)

- Do NOT add new `PlanTaskStatus` variants.
- Do NOT add new `SpurEventBody` variants.
- Do NOT add `mark_stalled` or any new MCP tool.
- Do NOT add PlanState disk persistence.
- Do NOT add auto-stall timer.
- Do NOT make MAX_ATTEMPTS per-plan configurable.
- Do NOT add evidence gate on approve.
- Do NOT change `get_task_diff` behavior (spike only).
- Do NOT touch the in-process dispatch channel semantics.
- Do NOT alter existing `PlanTaskReviewed` event payload.

## Revision history

**v2 (2026-04-18, post-MCTS stress-test):** scope cut in half.

Changes from v1:
- **Cut `PlanTaskStatus::Exhausted` variant** (was Unit 3). Reason: `Rejected` already handles the same state-machine role; an Exhausted variant would force every match arm, event consumer, and TUI label to handle a distinction that only the human-facing comment string cares about. Replaced with auto-reject at MAX + exhaustion-prefixed feedback.
- **Cut `PlanTaskStatus::Stalled` variant and `mark_stalled` tool** (was Unit 4). Reason: no observed use case that `reject` doesn't already handle. Speculative scope. File a follow-up if a real need emerges.
- **Cut new events `PlanTaskExhausted` and `PlanTaskStalled`.** Consequence of the above cuts.
- **Fixed Unit 1 causal ordering bug.** v1 wrote the beads comment BEFORE `try_send`. On `try_send` failure the state rolls back but the comment would already claim a new attempt is running. v2 writes the comment AFTER successful dispatch — honest audit.
- **Removed TUI changes.** No new variants to label.
- **LOC estimate:** ~180 → ~95. Tests: 5 → 4. New surface area: 2 enum variants + 2 events + 1 tool → 0.
