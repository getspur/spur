---
name: loop-generation-authoring
description: Use when a SPUR brain receives LoopDue or LoopEscalation continuations. Guides authoritative loop status lookup, generation authoring, triage review, escalation handling, and autonomy guardrails.
role: brain
---
<!-- SPUR-MANAGED v=1 skill=loop-generation-authoring sha256=0000000000000000000000000000000000000000000000000000000000000000 -->

# Loop Generation Authoring

Use this skill when you receive a `ContinuationSource::LoopDue` continuation or a
`ContinuationSource::LoopEscalation` continuation.

## LoopDue: author the next generation

Treat the continuation text as a notice only. The `LoopDue` continuation payload is
free-form summary text; loop id, generation, and template details may appear there
as prose. Do not parse generation, template, autonomy, budget, or escalation state
out of that text. Call `get_loop_status` for authoritative state before deciding
anything.

Procedure:

1. Call `get_loop_status` for the loop, with enough `recent_runs` to see the current
   run history. If the continuation text does not identify a single loop id clearly,
   stop and ask the user or inspect beads rather than guessing.
2. Check the returned state:
   - If `paused` is true, do not author a generation. Tell the user the loop is
     paused.
   - If `next_run` is still in the future or backoff means the loop is not due, do
     not author early.
   - If escalation policy or recent run history requires human attention, summarize
     that state instead of creating more work.
3. Derive the next generation from authoritative run records: use the max generation
   in `recent_runs` plus one, or generation 1 when no run record exists. Never use a
   generation number from the continuation prose.
4. Author the generation with `submit_plan` by copying `status.spec.template` tasks.
   Preserve task text, dependencies, base, and any template metadata. The first task
   is mandatory triage and must keep the `spur:loop-triage-task` label.
5. Put these labels on the epic:
   - `spur:loop-id:<loop_id>`
   - `spur:loop-generation:<n>`
   - `spur:autonomy:<loop level>`
   - `spur:loop-budget-micros:<max_cost_micros_per_generation>` when that governor
     is present
6. At L1, do not invent tasks beyond the template. ReportOnly suppresses non-triage
   dispatch, so extra work only burns tokens.
7. Watch the plan with `get_plan_status`. Review the triage output first. Approve,
   reject, or stop the generation based on the triage result and normal review
   standards.
8. Do not write loop run records yourself. Let the terminal hook write the run record
   when the generation reaches a terminal state.

## LoopEscalation: surface history to the user

On `ContinuationSource::LoopEscalation`, call `get_loop_status` and summarize the
run-record history to the user: recent generation outcomes, consecutive failures,
backoff state, paused state, and any escalation reason visible in the status.

Never self-promote loop autonomy. `set_loop_autonomy` requires explicit user
approval for the target level (`l1`, `l2`, or `l3`) before you call it.
