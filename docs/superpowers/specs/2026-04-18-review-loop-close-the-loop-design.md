# Review Loop — Close-the-Loop Bundle

**Status:** design
**Date:** 2026-04-18
**Depends on:** `2026-04-17-brain-review-feedback-loop-phase2-design.md`, `2026-04-17-execute-epic-phase2.5-design.md`
**Area:** `spur-mcp` plan/review · `spur-pm` PmService · `spur-tui` dashboard
**Predecessor plan (the one this spec fixes):** `2026-04-17-execute-epic-phase2.5.md`

## Problem

Phase 2 (review loop) and Phase 2.5 (execute_epic) shipped. Live dogfooding on epic `bd-1mh` surfaced four holes in the brain → worker → review cycle:

1. **`request_changes` leaves no trace in beads.** `plan.rs:1042-1143` dispatches a new worker attempt via the in-process channel but writes nothing to the PM backend. A human auditor reading beads sees approve/reject comments but has zero record of the most-frequent review event. The enriched feedback lives only in `PlanTaskEntry.history` in RAM.
2. **`br update -s done` is rejected** by the beads config in use. Every approval emits `beads update failed: br retryable error after 2 attempts: Invalid status: done` as a warning — the task becomes `Approved` in `PlanState` while the beads issue stays `in_progress`. Silent drift between the two stores.
3. **`MAX_ATTEMPTS=3` creates a limbo state.** When `request_changes` is called on a task at `attempt == MAX_ATTEMPTS`, `review_task` returns `Err` (`plan.rs:1053-1057`) and leaves the task in `AwaitingReview` forever. `is_terminal_plan_status` (`plan.rs:912-917`) doesn't cover this case, so the plan itself never terminates. The brain must manually call `reject`, but nothing in the event stream prompts it to do so. **bd-1mh.1 is one attempt away from this state right now.**
4. **`get_task_diff` returned empty for bd-1mh.2** despite commit `95e8b73` existing on the worker branch — the brain had to review by walking git directly. Root cause is unknown; a fix cannot be designed until RCA is done.

### What's explicitly fine today

- In-process dispatch via `DelegationRequest` channel is the *signal* path; it is fast, correct, and tolerates beads outages. This spec does not touch it.
- The enriched-task prompt carries the feedback directly to the worker; beads is not on the critical path for worker context. We keep it that way.
- `PlanState` lives in RAM. Crash-restart durability is not in scope — no observed pain.

## Goals

1. **Beads becomes a complete audit trail** — every brain review decision (approve, reject, `request_changes`) produces a beads comment, best-effort, with warning-on-failure that does not block dispatch.
2. **Status mapping is configurable** — `PmService` accepts the "closed" status string at construction; default `"closed"`, overridable. `review_task` approve uses it.
3. **Plans always terminate** — introduce `PlanTaskStatus::Exhausted { last_feedback }` for the "MAX_ATTEMPTS reached" case; include it in `is_terminal_plan_status`; TUI labels it.
4. **Stall escape hatch** — add `mark_stalled` tool so brain or operator can manually move an `AwaitingReview` task to a terminal `Stalled { reason }` state.
5. **RCA for diff-empty** — spike task produces an RCA doc; no code change in this spec.

## Non-goals

- **Durability** of PlanState (disk persistence, restart recovery). No observed restart pain.
- **Auto-stall on timer** (wall-clock SLA). Manual only for now; revisit after we see manual usage.
- **Per-plan MAX_ATTEMPTS override.** The const stays `3`.
- **Evidence gate on approve** (require diff hash / test name). No observed failure.
- **Comment provenance** (distinguish brain from operator in beads). Out of scope.
- **Denormalized review-thread tool** (pull all prior beads comments for an issue). Out of scope.
- **Worker → brain escalation channel.** Out of scope.
- **`get_task_diff` root-cause fix.** Spike delivers RCA only; fix lands in a follow-up spec if needed.
- **PmBackendInner::GitHub changes.** The closed-status config applies to beads only; GitHub's `close` semantics are separate.

## Design

Five units, scaled to complexity.

### Unit 1 — `request_changes` writes a beads comment

**Where:** `plan.rs:1042-1143`, inside the `"request_changes"` match arm.

**What:** Before the `try_send`, if `pm` is `Some` and `entry.spec.issue_id` is `Some`, build an `IssueUpdate` with only `comment` set and call `pm.update_issue(id, update).await`. Format:

```
Brain requested changes (attempt N/MAX):
<feedback>

Worker branch: <branch if present, else "(no branch yet)">
```

On error, push `"beads comment failed: {e}"` into `warnings`. Do NOT block the dispatch — the signal path stays independent of beads.

**Why before `try_send`:** so the audit trail reflects the causal order the operator sees (comment first, then a new worker attempt). If `try_send` fails, the comment still landed — that's acceptable: the operator will see "brain requested changes" in beads and can manually retry via existing mechanisms.

**Test:** `request_changes_writes_beads_comment_before_dispatch` — mock `PmService`, assert `update_issue` was called with a comment matching the pattern, assert dispatch also happened.

**Out of scope for this unit:** enriching the beads comment with a structured summary of history (just the latest feedback + attempt counter + branch). Past-attempt trail stays in `entry.history` for now.

### Unit 2 — Configurable closed-status for PmService

**Where:** `spur-pm/src/service.rs`, `spur-pm/src/beads.rs`, `spur-mcp/src/plan.rs:985`.

**What:**

1. Add a new optional parameter to `PmService::try_new`: `closed_status: Option<String>`. When `None`, default to `"closed"`. Threaded down into `BeadsAdapter` as a struct field.
2. `PmService` exposes `pub fn closed_status(&self) -> &str` returning the configured string.
3. `plan.rs:985` replaces the hardcoded `"done"` with `pm.closed_status().to_string()`.
4. Callers of `PmService::try_new` in `spur-core/src/orchestrator.rs` pass `None` (accepting the `"closed"` default); overrides can be wired through config in a follow-up if a second value is needed.

**Why a config value, not auto-detection:** querying beads for its status vocabulary at startup is a second CLI round-trip that can fail. A config string with a sensible default is simpler and deterministic. Users who hit this fix it once.

**Why a function parameter, not a config file entry:** YAGNI — we have one known user (the dogfood repo) and one known working value (`"closed"`). Wire plumbing (TOML key → SpurConfig → orchestrator → PmService) is premature until a second caller needs a different value.

**Test 1:** `closed_status_defaults_to_closed` — construct `PmService` without override, assert `closed_status() == "closed"`.
**Test 2:** `approve_uses_configured_closed_status` — mock backend, override to `"resolved"`, call `review_task(approve)`, assert `update_issue` was called with `status: Some("resolved")`.

**Out of scope:** mapping per-workflow (e.g., `won't_fix`, `duplicate`). Only one closed-status is configurable.

### Unit 3 — `PlanTaskStatus::Exhausted`

**Where:** `plan.rs:36-63` (enum), `plan.rs:912-917` (`is_terminal_plan_status`), `plan.rs:1042-1057` (request_changes MAX guard), `plan.rs` status counters (`760-766`, `800-832`), TUI label map.

**What:**

1. Add variant:
   ```rust
   Exhausted {
       last_feedback: String,
       attempts: u32,
   },
   ```
2. In `request_changes`, when `entry.attempt >= MAX_ATTEMPTS`, instead of returning `Err`:
   - Transition to `PlanTaskStatus::Exhausted { last_feedback: fb.to_string(), attempts: entry.attempt }`.
   - Write a beads comment (same best-effort pattern as Unit 1): `"Brain exhausted retries (N/MAX): <feedback>"`.
   - Emit the new `PlanTaskExhausted` event (see Events section).
   - Return `Ok(build_plan_status(...))` with a `decision: "exhausted"` field, no new dispatch.
3. Extend `is_terminal_plan_status` to include an overall status that reflects presence of `Exhausted` tasks. Naming: if any task is `Exhausted` and no `Failed`/`Rejected` are also present, overall becomes `"exhausted"`. If mixed, `"has_failures"` wins (same precedence as today).
4. Counter in `build_plan_status`: add `n_exhausted`.
5. TUI dashboard: label `Exhausted` with a distinct color (suggested: red italic) and show `attempts: N/MAX` inline.

**Why transition instead of error:** the brain gets a structured outcome it can see via `get_plan_status`. Today it gets an Err string and has to remember the task is still AwaitingReview. This is the limbo bug.

**Test:** `request_changes_at_max_attempts_transitions_to_exhausted` — construct plan with task at `attempt = MAX_ATTEMPTS`, call `request_changes`, assert status is `Exhausted`, assert no new `DelegationRequest` was sent, assert `is_terminal_plan_status` returns true for the overall plan.

**Descendant handling:** `Exhausted` is terminal but NOT approved. Per the existing rejection cascade pattern (`mark_descendants_failed`, `plan.rs:1209+`), we cascade descendants to `Failed { error: "upstream '{id}' exhausted" }`. Same BFS logic; new sentinel string. `retry_plan_task` (Phase 2.5) already unblocks descendants by prefix-matching `"upstream "` in the error string, so a future `retry_plan_task(exhausted_id)` would correctly reverse the cascade — verify this in the test by asserting the sentinel starts with `"upstream "`.

### Unit 4 — `mark_stalled` tool

**Where:** new `PlanTaskStatus::Stalled { reason: String }` variant in `plan.rs`; new `mark_stalled_def()` in `tools.rs`; new `handle_mark_stalled` in `server.rs`; `plan.rs::mark_stalled` function.

**What:**

1. Add `PlanTaskStatus::Stalled { reason: String }`.
2. Add to `is_terminal_plan_status`: `"stalled"` when any task is Stalled and no Failed/Rejected/Exhausted present; mixed follows existing precedence.
3. Add `mark_stalled(plan_id, task_id, reason)` function in `plan.rs`:
   - Validate task is currently `AwaitingReview`. (Refuse for other states — we don't want to stall a dispatched task; that would race with completion.)
   - Transition to `Stalled { reason }`.
   - Write beads comment: `"Plan task marked stalled: {reason}"`.
   - Emit `PlanTaskStalled { plan_id, task_id, task_name, reason }` event.
   - Cascade descendants to `Failed { error: "upstream '{id}' stalled" }`.
4. Expose as MCP tool `mark_stalled` with schema `{ plan_id: string, task_id: string, reason: string }`.

**Why AwaitingReview-only:** the brain may decide a review is impossible (unclear deliverable, wrong worker, etc.). This is the only state where a manual "give up but not reject" makes sense. Dispatched tasks should be handled via cancellation; Exhausted tasks are terminal already.

**Test:** `mark_stalled_transitions_and_cascades` — plan with two tasks, second depends on first; call `mark_stalled` on the first while it's AwaitingReview; assert first becomes Stalled, second becomes Failed with "upstream stalled" error.

**Out of scope:** auto-stall on timer. This unit is explicitly manual-only. If manual usage shows a consistent trigger condition, Phase 3 can auto-stall.

### Unit 5 — Spike: RCA for `get_task_diff` empty-result

**Where:** no code change; deliverable is `docs/rca/2026-04-18-get-task-diff-empty.md`.

**What the spike must answer:**

1. Reproduction: does `get_task_diff(bd-1mh.2)` still return empty now, or was it a transient state?
2. Root cause: is the worker branch missing from the orchestrator's git view (fetch required)? Is the diff computed against the wrong base? Is a success path returning an empty string instead of an error?
3. Fix options: one-paragraph each, with cost estimate.
4. Recommendation: which follow-up to file (or "no fix needed, investigate further").

**Why a spike, not a fix:** Options B3 in brainstorming pre-committed to "surface empty as error" without knowing the root cause. If the root cause is "no commits on branch" that's a valid empty state and surfacing as error is wrong. Spike first.

**Deliverable location:** `docs/rca/2026-04-18-get-task-diff-empty.md`. Format matches existing `docs/rca/2026-04-16-delegation-transport-mismatch.md`.

## Events

New or renamed ACP events in `spur-acp/src/domain/events.rs`:

| Event | Trigger | Payload additions |
|---|---|---|
| `PlanTaskReviewed` (existing) | approve, reject, request_changes | — (no changes) |
| `PlanTaskExhausted` (new) | Unit 3 — transition to Exhausted | `plan_id`, `task_id`, `task_name: Option<String>`, `attempts: u32`, `last_feedback: String` |
| `PlanTaskStalled` (new) | Unit 4 — `mark_stalled` | `plan_id`, `task_id`, `task_name: Option<String>`, `reason: String` |

Both new events get `#[serde(default)]` on added fields to stay wire-compatible with older TUIs. TUI dashboard handles both as terminal-colored log entries.

**Design note on event shape:** I considered overloading `PlanTaskReviewed { decision: "exhausted" | "stalled" }` instead of adding two new variants. Rejected because `decision` is a string field parsed by the TUI and by downstream consumers; two new enum variants make the type system enforce handling, and TUI can match exhaustively. Costs one enum variant per outcome; worth it.

## Error handling

- **Beads write failure (any unit):** push to `warnings` in the response, continue. Signal path (dispatch, state transition) is independent.
- **Closed-status config missing:** default `"closed"`. No error at startup.
- **`mark_stalled` on non-AwaitingReview task:** returns `Err("cannot stall task in status {X}: only AwaitingReview is stallable")`.
- **Exhausted cascade:** reuses existing `mark_descendants_failed` BFS. No new error paths.
- **Event emit failure:** events are fire-and-forget via `sink.emit`; unchanged from Phase 2.

## Testing

One unit test per functional change (listed under each unit). Additionally:

- **`is_terminal_plan_status_covers_all_terminal_variants`** — parameterized over `approved`, `failed`, `has_failures`, `has_rejections`, `partial`, `exhausted`, `stalled`; asserts all return true.
- **`plan_state_serializes_all_new_variants`** — serde round-trip for `Exhausted` and `Stalled` via `build_plan_status` JSON.

No integration tests in this spec — units are tight, each fits in existing test harnesses.

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
```

```
After (request_changes at attempt 2, MAX=3):
  brain → review_task("request_changes", fb)
    ├─ pm.update_issue(comment="Brain requested changes (2/3): fb") [best-effort]
    └─ state: AwaitingReview → Dispatched(new)
    └─ try_send enriched

After (request_changes at attempt 3, MAX=3):
  brain → review_task("request_changes", fb)
    ├─ pm.update_issue(comment="Brain exhausted retries (3/3): fb") [best-effort]
    ├─ state: AwaitingReview → Exhausted { last_feedback, attempts: 3 }
    ├─ cascade descendants → Failed { "upstream '{id}' exhausted" }
    ├─ sink.emit(PlanTaskExhausted)
    └─ Ok(plan_status with decision: "exhausted")
```

## Files touched

| File | Units | Change |
|---|---|---|
| `crates/spur-mcp/src/plan.rs` | 1, 3, 4 | `request_changes` beads write · new variants · new functions |
| `crates/spur-mcp/src/tools.rs` | 4 | `mark_stalled_def()` |
| `crates/spur-mcp/src/server.rs` | 4 | `handle_mark_stalled` |
| `crates/spur-pm/src/service.rs` | 2 | `closed_status()` accessor |
| `crates/spur-pm/src/beads.rs` | 2 | `BeadsAdapter` carries closed_status field |
| `crates/spur-pm/src/types.rs` or config | 2 | config surface for closed_status |
| `crates/spur-acp/src/domain/events.rs` | 3, 4 | `PlanTaskExhausted`, `PlanTaskStalled` |
| `crates/spur-tui/src/views/dashboard.rs` | 3, 4 | labels for Exhausted, Stalled |
| `docs/rca/2026-04-18-get-task-diff-empty.md` | 5 | new RCA (spike deliverable) |

Total estimate: ~180 LOC of production code, ~5 unit tests, 1 RCA doc.

## Success criteria

1. Running the `bd-1mh` replay end-to-end produces no `beads update failed` warnings.
2. `br show <issue_id>` shows a comment for every brain review decision (approve, reject, request_changes, exhausted).
3. A plan with a task that hits `request_changes` at MAX_ATTEMPTS terminates (overall status `"exhausted"`) instead of blocking.
4. Operator can call `mark_stalled(plan_id, task_id, "unclear deliverable")` on an AwaitingReview task and see the plan terminate with overall `"stalled"`.
5. `docs/rca/2026-04-18-get-task-diff-empty.md` exists with reproduction + root cause + recommendation.

## Build order (for plan-writing)

1. **Unit 2** (closed-status config) — no new state machine changes; unblocks Unit 1's comment writes from emitting misleading warnings during tests.
2. **Unit 1** (request_changes beads comment) — smallest functional change on top of Unit 2.
3. **Unit 3** (Exhausted state) — largest state-machine change; best done with Unit 1/2 already green.
4. **Unit 4** (mark_stalled tool) — parallels Unit 3 shape; lands after Unit 3 so enum changes are settled.
5. **Unit 5** (spike) — can run in parallel with any of the above; deliverable is a doc.

## Out-of-scope reminders (re-stated for plan-writing)

- Do NOT add PlanState disk persistence.
- Do NOT add auto-stall timer.
- Do NOT make MAX_ATTEMPTS per-plan configurable.
- Do NOT add evidence gate on approve.
- Do NOT change `get_task_diff` behavior (spike only).
- Do NOT touch the in-process dispatch channel semantics.
- Do NOT alter existing `PlanTaskReviewed` event payload.
