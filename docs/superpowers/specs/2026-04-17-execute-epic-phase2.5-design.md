# Execute Epic — Phase 2.5 (Authoring Unification)

**Date:** 2026-04-17
**Status:** Approved design, pending implementation
**Phase:** 2.5 of N
**Depends on:** `2026-04-17-brain-review-feedback-loop-phase2-design.md` (Phase 2, shipped)

## Problem

The brain authors every decomposed task **twice**:

1. `create_issue(type=epic)` + `create_issue(type=task, parent=epic)` + `add_dependency` — in **beads** (tracker).
2. Inline `PlanTask[]` (with `issue_id` cross-ref) passed to `submit_plan` — in **plan executor** (runtime).

Consequences:

- **Token cost**: every orchestration session pays to reconstruct the plan from beads.
- **Two DAGs**: `beads.blocked_by` (durable) and `PlanTask.depends_on` (ephemeral in-memory). They can silently diverge when brain mutates beads mid-plan.
- **Fragile bridge**: `issue_id` is the only link between the two systems, and it's advisory.

The observed pain is **duplicate authoring**. Two-DAG divergence and restart-loss are theoretical — not observed, not part of this scope.

## Solution

Ship exactly one new MCP tool:

```
execute_epic(epic_id) → plan_id
```

Strictly a **hydration tool**: reads the epic subgraph from beads once, derives `PlanTask[]`, hands off to the existing `submit_plan` engine unchanged. After hydration, the plan runs under the existing Phase 2 review/iteration engine. Beads is updated via the same PmService side effects that exist today (`orchestrator.rs:2395–2479`).

**This is NOT a projection.** Beads is not the runtime source of truth; `PlanState` still lives in memory with current semantics. Restart-loss is unchanged from today. We are unifying **authoring**, not execution state.

## Non-Goals (Explicit)

The following are out of scope for Phase 2.5. They are open questions, **not** roadmap commitments.

- **Execution-state persistence / restart recovery.** If the orchestrator dies mid-plan, `PlanState` is lost, same as today. Document; revisit only if observed pain demands.
- **Continuous projection.** `execute_epic` is one-shot hydration. Beads mutations after the call are NOT reflected in the running plan.
- **Autonomous worker pickup** (daemon mode). Workers remain push-only. Horizontal scale across orchestrators is not addressed here.
- **Attempt-history carry-over across re-executions.** A second `execute_epic` on a terminal-state epic starts a fresh attempt budget.
- **Nested epics.** Flat children only; sub-epic children are rejected with an actionable error.
- **Deprecating `submit_plan`.** It stays as a lower-level primitive for plans not backed by beads.

## User Flow

Before (today):

```
brain: create_issue(type=epic, title="Add auth")            → bd-100
brain: create_issue(type=task, parent=bd-100, title="...")  → bd-101
brain: create_issue(type=task, parent=bd-100, title="...")  → bd-102
brain: add_dependency(bd-102, bd-101)
brain: submit_plan(tasks=[
    {task_id:"bd-101", agent:"codex", task:"...", issue_id:"bd-101"},
    {task_id:"bd-102", agent:"codex", task:"...", depends_on:["bd-101"], issue_id:"bd-102"},
])                                                          → plan_id=...
```

After (Phase 2.5):

```
brain: create_issue(type=epic, title="Add auth",
                    labels=["spur.agent=codex"])            → bd-100
brain: create_issue(type=task, parent=bd-100,
                    labels=["spur.agent=codex"])            → bd-101
brain: create_issue(type=task, parent=bd-100,
                    labels=["spur.agent=codex"])            → bd-102
brain: add_dependency(bd-102, bd-101)
brain: execute_epic(epic_id="bd-100")                       → plan_id=...
```

Net change: one tool call instead of N inline PlanTask definitions. Brain never crafts `PlanTask[]` for beads-backed work again.

## MCP Tool Contract

```jsonc
{
  "name": "execute_epic",
  "description": "Execute a beads epic: hydrate a plan from the epic's children subgraph and dispatch in dependency order. Agent routing comes from the `spur.agent=<name>` label on each child issue. Task text comes from issue.body (override via `spur.task_text=<text>` label). Rejects nested sub-epic children. External blocked_by references must already be done. After dispatch, the plan runs under the normal review engine — use get_plan_status / get_task_diff / review_task as with submit_plan. Re-calling execute_epic while a plan is active for the same epic returns the existing plan_id (idempotent). After the plan reaches a terminal state (approved / failed), a new execute_epic call starts a fresh plan with a fresh attempt budget.",
  "input_schema": {
    "type": "object",
    "properties": {
      "epic_id": {
        "type": "string",
        "description": "The beads ID of an issue with type=epic. Direct children (type=task) are executed; nested epics are rejected."
      },
      "default_agent": {
        "type": "string",
        "description": "Fallback agent name when a child has no `spur.agent=<name>` label. If a child has no label AND default_agent is absent, execute_epic errors."
      }
    },
    "required": ["epic_id"]
  }
}
```

**Returns:** the same shape as `submit_plan` plus derived-DAG metadata:

```jsonc
{
  "plan_id": "plan-abc123",
  "epic_id": "bd-100",
  "status": "running",
  "derived": {
    "task_count": 5,
    "edge_count": 4,
    "agents": { "codex": 3, "claude-code": 2 },
    "warnings": [/* e.g., "bd-104 has no spur.agent label — used default_agent" */]
  },
  "tasks": [ /* same per-task shape as get_plan_status */ ],
  "counts": { "total": 5, "pending": 3, "ready": 2, "dispatched": 0, "awaiting_review": 0, "approved": 0, "rejected": 0, "failed": 0 },
  "next_action": "Workers still running. Poll get_plan_status to monitor."
}
```

## Behavior

### 1. Subgraph Collection

- `pm.get_issue(epic_id)` → fail if `type != "epic"`.
- `pm.list_issues(filter: parent == epic_id)` → collect direct children.
- Reject any child where `type == "epic"` with error `"nested epic child {id} not supported; flatten to direct tasks"`.
- For each child, read `blocked_by`.

### 2. Agent Routing

Agent name is resolved per child in this order:

1. Label matching `spur.agent=<name>` on the child.
2. Label matching `spur.agent=<name>` on the epic (inherited default).
3. `default_agent` from the tool call.
4. Error: `"no agent for task {id}; set `spur.agent=<name>` label or pass default_agent. Known agents: [codex, claude-code, kiro, ...]"`.

Validation: the resolved agent name MUST match a configured agent in `AgentConfigs`. Unknown agent → error with the known-agent list.

### 3. Task Text

Per child:

- If a `spur.task_text=<text>` label exists, use its value as `PlanTask.task`.
- Else use `issue.body` as `PlanTask.task`.
- Strip the `spur.*` labels before using the body to avoid confusing the worker with machine-routing metadata.

### 4. Dependency Derivation

`PlanTask.depends_on = issue.blocked_by ∩ subgraph_ids`.

For each `blocked_by` entry NOT in the subgraph (external dep):
- If the external issue has `status == "done"`, include it as "pre-satisfied external" in warnings but omit from `depends_on` (the engine treats empty deps as Ready).
- Else error: `"external dependency {ext_id} not done (current status: {st}); satisfy it or remove the edge before execute_epic"`.

Cycle check runs on the subgraph via the existing `validate_plan`.

### 5. Idempotency

A `PlanRegistry: HashMap<epic_id, plan_id>` tracks active plans keyed by epic_id.

- Second `execute_epic(epic_id)` while an active plan exists for the same epic → **return the existing plan_id** (no new dispatch).
- After the plan reaches a terminal overall status (`approved`, `failed`, `has_rejections`, `has_failures`), the registry entry is cleared. A subsequent call starts a new plan with `attempt = 1` across all tasks, fresh `history = []`.

Terminal detection reuses `build_plan_status` — when `overall ∈ {"approved","failed","has_rejections","has_failures"}` the registry is cleared on the next `execute_epic` call (lazy cleanup — no background sweep).

### 6. Hand-off to Engine

After derivation, `execute_epic` builds a `Vec<PlanTask>` and invokes the same code path `submit_plan` uses today:

```rust
let plan_state = PlanState {
    plan_id: new_plan_id(),
    tasks: derived.into_iter().map(PlanTaskEntry::new).collect(),
    brain_session_id: state.brain_session_id.clone(),
};
// identical to submit_plan from here — run_plan spawns, dispatches, etc.
```

Zero changes to the scheduling kernel. Phase 2 review loop (`review_task`, `get_task_diff`, `MAX_ATTEMPTS=3`, rejection cascade) works unchanged.

## Events

No new `SpurEventBody` variants. Existing events (`PlanTaskReviewed`, `PlanTaskIterating`, `IssueUpdated`) cover the observability needs. An additional `"[plan] ▶ Executing epic \"<name>\" (5 tasks)"` activity-log entry is emitted via existing plumbing on tool success — no schema change.

## Error Handling

All errors are returned as MCP tool errors with actionable messages:

| Condition | Message |
|---|---|
| Epic not found | `"epic '{id}' not found in beads"` |
| Wrong type | `"issue '{id}' is not an epic (type={t}); use create_issue(type='epic') or change its type"` |
| Nested sub-epic | `"nested epic child '{id}' not supported; flatten to direct tasks"` |
| No agent | `"no agent for task '{id}'; set `spur.agent=<name>` label or pass default_agent. Known agents: [...]"` |
| Unknown agent | `"agent '{name}' on task '{id}' not configured. Known agents: [...]"` |
| External dep unsatisfied | `"external dependency '{ext}' not done (status={st}); satisfy it or remove the edge"` |
| Cycle detected | Reuse existing `validate_plan` message |
| Empty subgraph | `"epic '{id}' has no children; create at least one child task first"` |
| Re-execution while active | NOT an error — returns existing plan_id (idempotent by design) |

## Test Plan

Unit tests in `plan.rs` (following Phase 2 pattern):

- `execute_epic_rejects_missing_epic` — unknown id → clear error.
- `execute_epic_rejects_non_epic` — type=task → error.
- `execute_epic_rejects_nested_epic_child` — child with type=epic → error.
- `execute_epic_rejects_empty_children` — epic with no children → error.
- `execute_epic_rejects_unsatisfied_external_dep` — child blocked_by external issue with status=open → error.
- `execute_epic_allows_done_external_dep` — external done → warning in `derived.warnings`, omitted from `depends_on`.
- `execute_epic_resolves_agent_from_child_label` — `spur.agent=codex` on child → routed to codex.
- `execute_epic_inherits_agent_from_epic_label` — no child label, epic has label → inherited.
- `execute_epic_falls_back_to_default_agent` — neither label, `default_agent="claude-code"` → claude-code.
- `execute_epic_rejects_missing_agent` — neither label nor default → error with known-agent list.
- `execute_epic_rejects_unknown_agent` — label resolves to non-configured name → error.
- `execute_epic_uses_spur_task_text_override` — child with `spur.task_text=<x>` → PlanTask.task = x.
- `execute_epic_defaults_to_issue_body` — no override → PlanTask.task = issue.body.
- `execute_epic_strips_spur_labels_from_body` — body is untouched but machine labels are not leaked into worker prompt.
- `execute_epic_maps_blocked_by_to_depends_on` — child blocked_by references within subgraph → mapped.
- `execute_epic_is_idempotent_while_active` — second call returns same plan_id.
- `execute_epic_starts_fresh_after_terminal` — after all tasks approved, second call returns new plan_id with attempt=1.
- `execute_epic_cycle_rejected` — cycles in subgraph → reuse existing validate_plan error.

Integration test:
- `execute_epic_end_to_end_dispatches_and_reviews` — hydrate 2-task epic, assert both dispatched under existing engine, brain review_task(approve) transitions epic through normal flow, beads issues end in `done` state.

## Files to Touch

```
crates/spur-mcp/src/tools.rs         — add execute_epic_def() to tools_list()
crates/spur-mcp/src/plan.rs          — add execute_epic() function + PlanRegistry
crates/spur-mcp/src/server.rs        — handler handle_execute_epic()
crates/spur-mcp/src/lib.rs           — re-export execute_epic if needed
docs/superpowers/specs/…phase2.5…md  — this file
```

No changes to `spur-core/orchestrator.rs`, `spur-acp/domain/events.rs`, `spur-tui/*`, `spur-pm/*`. The hand-off to `submit_plan` reuses the existing dispatch path.

## Estimated Scope

- Tool definition + handler: ~100 LOC
- `execute_epic` core + subgraph derivation + validation: ~200 LOC
- Tests: ~200 LOC

**Total: ~500 LOC delivered in one focused commit.** Single well-defined change.

## Open Questions (Documented, NOT Planned)

These are **risks** to track, **not** work to commit. Revisit only when observed pain demands.

- **Restart recovery.** If the orchestrator dies mid-plan, `PlanState` is lost. Today's behavior; Phase 2.5 does not regress it. If this becomes observed pain, design an execution-state sidecar (sqlite in `.spur/` or JSONL).
- **Mid-plan beads mutations.** Brain calling `add_dependency` on a task whose plan is running has no effect on the running plan. If this becomes observed pain, design continuous projection or write-through.
- **Attempt-history carry-over.** Re-executing a rejected epic starts at attempt=1. If teams want historical carry-over, store attempt metadata in beads comments and read on re-execution.
- **Horizontal scale.** Single-orchestrator today. If multi-orchestrator becomes needed, design daemon mode with optimistic beads claims.

Each of these is a **future spec** if justified by usage, not a Phase 2.5 deliverable.

## Rollout

- Land in one commit.
- Ship under existing version.
- Document in `docs/spur-core-architecture.md`: "Prefer `execute_epic` for beads-backed work; `submit_plan` remains for ad-hoc plans."
- No migration — brains that already use `submit_plan` keep working.

## Success Criteria

- Brain can orchestrate a multi-task epic with: `create_issue * N` + `add_dependency * M` + `execute_epic * 1` instead of also constructing a `PlanTask[]`.
- Token cost for plan dispatch drops (measured by counting tool calls in a representative session).
- All Phase 2 review/iteration tests continue to pass unchanged (regression check).
- Zero new warnings, zero new failing tests, zero changes to the scheduling kernel.
