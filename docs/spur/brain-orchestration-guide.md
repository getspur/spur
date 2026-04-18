# Brain Orchestration Guide

Practical guidance for agents acting as the *brain* in a Spur session — whether that's `claude-code-acp`, `gpt-5-acp`, `kiro`, or another MCP-speaking orchestrator. Describes the three delegation patterns and when to use each.

## TL;DR decision tree

1. **One task, no dependencies.** → `delegate_to_worker(agent, task, context_files?, delegation_plan)`
2. **Several independent tasks you want to run in parallel.** → `delegate_parallel(tasks[], delegation_plan?)`
3. **Multi-task DAG with dependencies (2+ tasks + edges).** → `submit_plan(tasks[], delegation_plan)`
4. **DAG that must survive the session OR be visible to other humans/agents in beads.** → `submit_plan(..., persist_as_epic=true, epic_title=...)`

If unsure between #2 and #3: use #3. The orchestrator auto-runs independent tasks in parallel once their deps are satisfied.

## The three patterns in detail

### Pattern A — Single task

```json
{
  "name": "delegate_to_worker",
  "arguments": {
    "agent": "claude-code-acp",
    "task": "CONTEXT: ...\nGOAL: ...\nCONSTRAINTS: ...\nEXPECTED_OUTPUT: ...",
    "context_files": ["src/a.rs", "src/b.rs"],
    "delegation_plan": {
      "chosen": "claude-code-acp",
      "rationale": "Multi-file refactor; generalist fits."
    }
  }
}
```

Worker runs, returns a diff. You inspect. No review loop (single shot). Use for small edits, one-off bug fixes, or prototyping.

### Pattern B — Parallel independent tasks

```json
{
  "name": "delegate_parallel",
  "arguments": {
    "tasks": [
      {
        "agent": "claude-code-acp",
        "task": "...",
        "context_files": ["src/a.rs"],
        "issue_id": "bd-101",
        "delegation_plan": {"chosen": "claude-code-acp", "rationale": "..."}
      },
      {
        "agent": "codex",
        "task": "...",
        "context_files": ["src/b.rs"],
        "issue_id": "bd-102",
        "delegation_plan": {"chosen": "codex", "rationale": "..."}
      }
    ]
  }
}
```

Tasks must be **truly independent** — no shared state, no file overlaps. Each runs in its own worktree so parallel edits don't corrupt the repo, but merge conflicts at PR time are still your problem.

### Pattern C — Plan with dependencies

```json
{
  "name": "submit_plan",
  "arguments": {
    "tasks": [
      {"task_id": "setup", "agent": "claude-code-acp", "task": "...", "depends_on": []},
      {"task_id": "impl_a", "agent": "claude-code-acp", "task": "...", "depends_on": ["setup"]},
      {"task_id": "impl_b", "agent": "codex", "task": "...", "depends_on": ["setup"]},
      {"task_id": "wire",   "agent": "claude-code-acp", "task": "...", "depends_on": ["impl_a", "impl_b"]}
    ],
    "delegation_plan": {"chosen": "mixed", "rationale": "Diamond DAG; parallel middle."}
  }
}
```

The orchestrator:
1. Dispatches `setup` immediately.
2. Dispatches `impl_a` + `impl_b` in parallel once `setup` approves.
3. Dispatches `wire` once both `impl_a` and `impl_b` approve.

Response includes `plan_id`. Poll via `get_plan_status(plan_id)`.

### Pattern D — Persisted plan (Pattern C + beads epic)

```json
{
  "name": "submit_plan",
  "arguments": {
    "tasks": [ ... same as Pattern C ... ],
    "delegation_plan": { ... },
    "persist_as_epic": true,
    "epic_title": "Refactor auth flow — Q2",
    "epic_body": "Full design in docs/superpowers/plans/2026-04-18-auth-refactor.md"
  }
}
```

Creates a beads epic with child issues for each task, linked by `depends_on` edges and labeled `spur.plan_id=<plan_id>`, `spur.plan_task_id=<task_id>`, `spur.agent=<agent>`. Response adds `epic_id` + `task_map` so you can cross-reference.

**Use this when:**
- The plan spans multiple sessions (restart safety).
- Humans outside Spur should see progress (beads UI / CLI / dashboard).
- You want `review_task(approve)` to auto-close the corresponding beads child.

**Do NOT use this when:**
- The plan is ephemeral (session-local work).
- You're prototyping and don't want extra state to clean up.

**Requirement:** the session's PmService must be a beads backend. Non-beads backends reject `persist_as_epic=true` with `-32000`.

## The review loop

After `submit_plan` (or `execute_epic`) returns, the orchestrator takes over dispatch. Your job becomes **reviewer**:

```
loop {
  status = get_plan_status(plan_id)
  if status.has_task_in("awaiting_review"):
    for task in status.tasks where status == "awaiting_review":
      diff = get_task_diff(plan_id, task.task_id)
      decision = your_review(diff)
      review_task(plan_id, task.task_id, decision, feedback?)
  if status.all_tasks_approved():
    break
  sleep 2s  (or use status.ready_to_merge as your exit signal)
}
```

Decisions:
- `approve` → task marked done, dependents auto-dispatched. If `persist_as_epic=true`, the beads child closes too.
- `reject` → task terminal. Pending/ready dependents cascade-fail. Use for work that's fundamentally misconceived.
- `request_changes` → re-dispatch the worker with `feedback` verbatim. Max 3 attempts per task. Use for fixable issues.

When all tasks are approved and `ready_to_merge` is true:

```json
{"name": "create_pr", "arguments": {"title": "...", "body": "...", "branch": "main"}}
```

## Picking the right agent per task

Call `list_available_workers` if you're unsure. Summary heuristics (see `defaults.toml` for authoritative descriptors):

| Task shape | Preferred agent |
|---|---|
| Multi-file refactor; writing new modules from spec | `claude-code-acp` |
| Single-file mechanical edits; language idiom translation | `codex` |
| Spec-driven workflows (Kiro `/spec-*` commands) | `kiro` |
| Multi-modal (images, diagrams) | `gemini` |

Always set `delegation_plan.chosen` + `delegation_plan.rationale` on each task. The reviewer uses these to detect agent-routing mismatches.

## Error recovery playbook

| Symptom | Action |
|---|---|
| `get_plan_status` shows a task in `failed` state after 3 attempts | Inspect the last attempt's diff via `get_task_diff(..., attempt=N)`. If salvageable, `reject` + re-plan; else `reject` + tell the user. |
| `review_task(request_changes)` returns `remaining_attempts=0` | You've exhausted retries. `approve` (if partial work is usable) or `reject`. |
| Dependent tasks cascade-failed after you `reject`ed a predecessor | Expected behavior. Re-plan the affected subgraph as a new `submit_plan` with `depends_on_external` references to the approved predecessors. |
| `submit_plan` returns `-32000 "persist_as_epic requires a beads PM backend"` | You tried `persist_as_epic=true` on a GitHub/Linear/Plane backend. Either drop the flag (ephemeral plan) or add beads to the repo. |
| `cancel_delegation` returns `-32601 "Internal operation not yet wired"` | Cancellation is not implemented yet. The worker keeps running; tell the user. |

## Interaction with writing-plans-shaped markdown

Spur's writing-plans skill (main Claude Code session) produces `docs/superpowers/plans/YYYY-MM-DD-<slug>.md`. When a brain sees such a file (e.g., in the user's request), it can:

1. Read the file.
2. Compose a `submit_plan` payload with one task per `## Task N` section.
3. Set `epic_body` = link to the plan file (so the beads epic has a back-reference).
4. Set each child task's `task` field to the CONTEXT/GOAL/CONSTRAINTS/EXPECTED_OUTPUT extracted from that task's steps.

This pattern keeps the plan.md as the human-readable artifact and beads as the execution tracking layer. Drift between the two is acceptable as long as the plan.md path + commit SHA are recorded on the epic.

## Escape hatches

- Need one off-plan quick task mid-execution? → `delegate_to_worker` directly. Orchestrator plan state is unaffected.
- Need to abort an in-flight plan? → reject all pending tasks (cascades). Then start a fresh `submit_plan`.
- Need to hand a plan off to a different brain? → plan state is keyed on `plan_id`; both brains polling `get_plan_status` with that ID see the same state (if they share the MCP server).
