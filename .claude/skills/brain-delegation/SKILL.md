---
name: brain-delegation
description: >
  Core delegation framework for SPUR brain agents. Injected into every
  brain session. Handles dispatch decisions, delegation plan structure,
  task prompt formatting, and worker routing.
role: brain
activation: always
---

# Brain Delegation Framework

## When to delegate vs. do it yourself

Do it yourself when:
  - The task is <15min of work.
  - You need tight iterative control (probe, edit, probe).
  - The task requires your accumulated session context.
  - No worker's good_for meaningfully matches.

Delegate when:
  - Subtasks are independent and parallelizable (use delegate_parallel).
  - A worker's good_for directly matches the task shape.
  - Scope (LoC, files, or duration) exceeds what you want to spend your
    context window on.
  - You need fresh context isolation.

## Complex Workflows (The Spurpower Paradigm)

Do not manually orchestrate multi-step, dependent tasks. For features requiring multiple steps, testing, or strict review gates (e.g., Implement -> Review -> Test), use the **Plan Engine**:
1. Use `create_issue` to create an Epic and child tasks.
2. Wire their dependencies using the `depends_on` parameter.
3. Call `graph_plan` to verify the execution DAG.
4. Call `submit_plan(persist_as_epic=true)` or `execute_epic`.

The system will automatically manage the dispatch, parallelism, and review gates (`review_task`) for you. Tactical instructions (e.g., TDD, systematic debugging) are automatically injected into the worker nodes by the system's hermetic `.spur/skills/` environment. You do not need to prompt workers to use them.

Routing rule: prefer specialist tier when good_for matches exactly;
fall back to generalist tier otherwise. avoid_for is a SOFT signal —
you MAY override it with a stated rationale when no better agent exists.
Prefer lower-cost_tier agents for mechanical tasks; reserve higher-cost
agents for tasks requiring integration, judgment, or architectural
decisions.

Your `delegation_plan` replaces, does not supplement, other planning
artifacts you would emit FOR DELEGATION DECISIONS. Native planning
tools (Todo, plan mode, etc.) remain for intra-task work.

## Required: delegation_plan parameter

Every delegate_to_worker and delegate_parallel call should include a
`delegation_plan` argument. Content scales with complexity:

For >=2 subtasks OR >3 files touched — pass the full shape:
  {
    "candidates":    [{"agent": "...", "rationale": "..."}, ...],
    "decomposition": [{"subtask": "...", "parallelizable_with": ["..."]}],
    "chosen":        "agent-name-or-self-or-parallel",
    "rationale":     "Why this choice beats the alternatives. If
                      violating any agent's avoid_for, state why."
  }

For trivial single-step delegations — minimum shape:
  { "chosen": "agent-name", "rationale": "short justification" }

All fields are advisory; the orchestrator accepts the tool call even
with minimal or missing content. Your rationale is surfaced to the
review gate so reviewers can see what you decided and why.

If you have access to a sequential-thinking MCP tool, use it to
generate the candidates and decomposition before committing to the
delegate_* call.

## Task prompt structure (what to send workers)

Structure the `task` field of delegate_to_worker as:

  CONTEXT: {scope, constraints from this session, relevant file paths
           and short excerpts — prefer inlining over passing paths via
           context_files so the worker doesn't spend turns re-reading}
  GOAL:    {one-sentence success criterion}
  CONSTRAINTS: {what the worker must NOT do}
  EXPECTED OUTPUT: {populated from the chosen agent's output_shape
                   when declared}

For agents with declared output_shape, EXPECTED OUTPUT must restate it.
For agents with declared input_expectations, CONTEXT must satisfy those
expectations before dispatch.

## Canonical example

Task: 'Refactor the auth module to use the new SessionId format across
all callers (4 files).'

Reasoning out loud (brain's narrative text):
  This is a multi-file refactor matching claude-code-acp's good_for.
  The changes are coupled (can't parallelize across callers).

delegate_to_worker(
  agent = "claude-code-acp",
  task = "CONTEXT: Refactor the auth module. Affected files: src/auth/mod.rs,
          src/auth/session.rs, src/api/handlers.rs, src/tests/auth.rs.
          The new SessionId format is: [snippet].
          GOAL: All callers use the new format; all tests pass.
          CONSTRAINTS: Don't touch src/api/v2/; don't modify the database schema.
          EXPECTED OUTPUT: Unified diff + summary paragraph + test plan bullets.",
  delegation_plan = {
    "candidates": [
      {"agent": "claude-code-acp", "rationale": "multi-file refactor matches good_for"},
      {"agent": "codex", "rationale": "cheaper but avoid_for = multi-file coordination"}
    ],
    "decomposition": [
      {"subtask": "refactor auth + callers", "parallelizable_with": []}
    ],
    "chosen": "claude-code-acp",
    "rationale": "multi-file refactor + coupled callers; codex's avoid_for excludes it."
  }
)
