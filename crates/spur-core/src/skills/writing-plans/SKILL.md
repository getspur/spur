---
name: writing-plans
description: Use when writing an implementation plan, creating a task breakdown, converting a spec to code, or planning work steps. Triggers on phrases like "write plan", "create plan", "plan tasks", "implementation plan", "break this into tasks", or "how should we implement this". In SPUR, produces a beads-backed DAG with dependencies, file scope boundaries, and worker routing.
role: brain
---

# SPUR Writing Plans — Specs Into Beads-Backed DAGs

## Overview

A plan in SPUR is not a markdown checklist. It is a **DAG of beads issues** managed by the orchestrator. Your job is to produce a plan that `submit_plan(persist_as_epic=true)` can consume, where every task is a beads-trackable unit with clear boundaries, dependencies, and acceptance criteria.

**Core principle:** Every task is a beads issue. Every dependency is explicit. Every worker gets isolated scope.

**Announce at start:** "I'm using the writing-plans skill to create the beads-backed implementation plan."

**Context:** This runs in the brain session after `brainstorming` has produced an approved spec and closed design epic.

**Save plan to:** `docs/superpowers/plans/YYYY-MM-DD-<feature-name>.md`
- (User preferences for plan location override this default)

## The Iron Law

```
NO DISPATCH WITHOUT A SUBMITTED PLAN
NO TASK WITHOUT A BEADS ISSUE
NO DEPENDENCY WITHOUT A VALID DAG
```

## Scope Check

If the spec covers multiple independent subsystems, it should have been broken into sub-project specs during brainstorming. If it wasn't, STOP and suggest breaking this into separate epics — one per subsystem. Each plan should produce working, testable software on its own.

## File Structure Mapping

Before defining tasks, map out which files will be created or modified and what each one is responsible for. This is where decomposition decisions get locked in.

- Design units with clear boundaries and well-defined interfaces. Each file should have one clear responsibility.
- You reason best about code you can hold in context at once, and your edits are more reliable when files are focused. Prefer smaller, focused files over large ones that do too much.
- Files that change together should live together. Split by responsibility, not by technical layer.
- In existing codebases, follow established patterns.

**SPUR-specific:** Each task should touch a focused set of files (ideally 1-3). If a task touches >5 files, it is probably doing too much and should be split. This minimizes scope-drift risk and keeps worker context windows manageable.

## Task Granularity for Worker Context

**Each task is one focused unit of work (15-30 min of worker time):**

- Small enough that a worker can hold all relevant files in context
- Large enough to produce a meaningful, reviewable diff
- Produces a commit-ready change that passes its own tests

**Bad task:** "Implement the entire auth module" (too large, worker will lose context, high scope-drift risk)

**Good task:** "Add password validation to the login handler" (focused, testable, clear boundaries)

**Task output must be commit-ready.** The worker commits at the end of each task. The brain reviews task-by-task.

## Plan Document Header

**Every plan MUST start with this header:**

```markdown
# [Feature Name] Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/<filename>.md`
**Design epic:** `<issue_id>` (closed)

**Goal:** [One sentence describing what this builds]

**Architecture:** [2-3 sentences about approach]

**Tech Stack:** [Key technologies/libraries]

---
```

## Task Structure

Each task must include fields the orchestrator needs:

```markdown
### Task N: [Component Name]

**Task ID:** `task-N` (used in `depends_on` references)

**Files:**
- Create: `exact/path/to/file.rs`
- Modify: `exact/path/to/existing.rs:123-145`
- Test: `tests/exact/path/to/test.rs`

**Depends on:** [task IDs, or "none" for root tasks]

**Acceptance Criteria:**
- [ ] Specific, verifiable outcome
- [ ] Tests pass
- [ ] No compilation errors

**Suggested Worker:** [e.g., codex for mechanical, claude-code-acp for multi-file coordination]

**Scope Boundary:**
- IN scope: [specific files/functions]
- OUT of scope: [what the worker must NOT touch]
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` signal immediately.

**Implementation:**
- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_specific_behavior() {
    let result = function(input);
    assert_eq!(result, expected);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test test_specific_behavior -- --nocapture`
Expected: FAIL with "function not defined"

- [ ] **Step 3: Write minimal implementation**

```rust
pub fn function(input: InputType) -> OutputType {
    expected
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test test_specific_behavior -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add tests/path/test.rs src/path/file.rs
git commit -m "feat(scope): add specific feature"
```
```

## Dependency DAG Rules

Follow these rules from `plan-task-discipline`:

1. **No circular dependencies.** If Task A depends on Task B, Task B must not (transitively) depend on Task A.
2. **Maximize parallelism.** Identify tasks that are truly independent and give them no `depends_on`. The orchestrator dispatches them in parallel.
3. **Minimize dependency chains.** Prefer wide DAGs over deep chains. A plan with 5 independent tasks is better than a chain of 5 sequential tasks.
4. **Interface contracts over implementation.** If Task B needs a type from Task A, Task A's task should define the interface. Task B can then depend on Task A and implement against that interface.

### beads Dependency Rewriting

When the plan is submitted, `depends_on` task IDs are rewritten to beads issue IDs via `build_epic_subgraph`. Use task IDs in the plan spec. The system maps them.

## No Placeholders

Every step must contain the actual content an engineer needs. These are **plan failures** — never write them:
- "TBD", "TODO", "implement later", "fill in details"
- "Add appropriate error handling" / "add validation" / "handle edge cases"
- "Write tests for the above" (without actual test code)
- "Similar to Task N" (repeat the code — the worker may be reading tasks out of order)
- Steps that describe what to do without showing how (code blocks required for code steps)
- References to types, functions, or methods not defined in any task

## beads Lifecycle Integration

When the plan is submitted:

1. The orchestrator creates an epic issue with `spur:plan-complete` label
2. Each task becomes a child issue with:
   - `spur:plan-id:<epic-id>`
   - `spur:plan-task-id:<task-id>`
   - Status: `draft` (later transitions to `open` → `in_progress` → `closed`)
3. The orchestrator dispatches tasks when all `depends_on` tasks are `Approved`
4. Workers must NOT close their own issues — only the brain does that via `review_task`

**Brain responsibility:** After `submit_plan`, poll `get_plan_status` or listen for `PlanCompleted` / `PlanReadyToMerge` events. Do not manually dispatch plan tasks that the orchestrator should auto-dispatch.

## Worker Routing Guidance

For each task, suggest a worker based on `brain-delegation` rules:

| Task Shape | Suggested Worker | Rationale |
|---|---|---|
| Single file, mechanical edit | codex | Low cost, fast |
| Multi-file refactor, tight coupling | claude-code-acp | Generalist, large context |
| UI/UX, spec-driven | kiro | Specialist tier |
| Test writing, validation | codex or claude-code-acp | Depends on complexity |
| Complex architectural change | claude-code-acp | Judgment, integration |

Include the suggested worker in the task's plan metadata. The brain makes the final routing decision at dispatch time.

## Signal Anticipation

For tasks with high scope-drift risk, add an explicit signal checkpoint:

```markdown
**Scope Drift Checkpoint:**
- If estimated remaining work exceeds original by >50% → emit `scope_drift`
- If you need to touch files not listed above → emit `scope_drift`
- If the approach appears unsafe → emit `risk`
```

This trains workers to signal early and gives the brain early warning.

## Self-Review

After writing the complete plan, look at the spec with fresh eyes and check the plan against it. This is a checklist you run yourself — not a subagent dispatch.

**1. Spec coverage:** Skim each section/requirement in the spec. Can you point to a task that implements it? List any gaps.

**2. Placeholder scan:** Search your plan for red flags — any of the patterns from the "No Placeholders" section above. Fix them.

**3. Type consistency:** Do the types, method signatures, and property names you used in later tasks match what you defined in earlier tasks?

**4. DAG validation:** Draw the dependency graph. Is it a valid DAG? Are there any cycles? Can any dependencies be removed to increase parallelism?

**5. beads compatibility:** Does every task have:
- A unique task ID?
- A clear `depends_on` list (even if empty)?
- Acceptance criteria that can be verified by the brain in `review_task`?
- A scope boundary that prevents cross-task contamination?

If you find issues, fix them inline. No need to re-review — just fix and move on. If you find a spec requirement with no task, add the task.

## Submission and Execution

After saving the plan, offer execution choice:

**"Plan complete and saved to `docs/superpowers/plans/<filename>.md`. Two options:**

**1. Submit to Orchestrator (recommended)** — Call `submit_plan(persist_as_epic=true)` to create beads issues and auto-dispatch workers as dependencies resolve.

**2. Review First** — Wait for user review of the plan before submission.

**Which approach?"**

**If Submit chosen:**
- Call `submit_plan` with the plan document
- The orchestrator creates the epic, child issues, and begins auto-dispatch
- Transition to monitoring mode — poll `get_plan_status` or await events

**If Review chosen:**
- Wait for user feedback
- Revise plan and re-run self-review
- Submit when user approves

## Terminal Plan States

After submission, the plan reaches terminal state when:
- All tasks are `Approved` → success, orchestrator emits `PlanReadyToMerge`
- Any task is `Rejected` and `max_review_retries` exhausted → failure
- All reachable tasks are terminal and unreachable tasks remain `Pending` → partial success

**Brain action on terminal state:**
1. Review all `Approved` tasks
2. Merge approved work
3. Explicitly close beads epic with `update_issue(status: "closed")`
4. Re-plan rejected tasks as new epics if needed

## Cross-References

- **brainstorming** — Previous skill in the chain; produces the approved spec
- **plan-task-discipline** — DAG rules, task boundaries, and lifecycle enforcement
- **beads-lifecycle** — Status state machine and label semantics for plan issues
- **brain-delegation** — Worker routing decisions at dispatch time
- **worker-signals** — How workers communicate blockers and scope drift
- **spur-way** — beads-first invariant; every task must have a beads issue
