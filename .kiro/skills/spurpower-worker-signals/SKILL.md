---
name: spurpower-worker-signals
description: "Use when encountering unexpected complexity, blockers, or scope changes during a delegated task — teaches exact signal emission protocol so the brain sees problems in time to re-plan."
---
<!-- SPUR-MANAGED v=1 skill=spurpower-worker-signals sha256=3c67c54c7a3a4bfa3fe136c1e99c0f9ec5f36af1e8c7b35c4b5b33a5ef830f0c -->

# Worker Signals

## Overview

Workers discover problems the brain cannot predict: scope is larger than estimated, a dependency is broken, a design assumption is wrong. The worker MUST communicate these upward via structured signals. Chat messages and task summaries are lost; signals are durable and actionable.

**Core principle:** Signal early, signal precisely, signal in beads.

## The Iron Law

```
NO SCOPE CHANGE WITHOUT A SIGNAL
NO BLOCKER WITHOUT A SIGNAL
NO RISK WITHOUT A SIGNAL
```

## When to Emit

**Emit immediately when ANY of these occur:**

- Estimated remaining work exceeds original estimate by >50%
- Task requires modifying files outside the original issue scope
- A dependency (another issue, external API, environment) is unavailable
- Current approach appears unsafe or introduces architectural risk
- Task cannot complete as specified without renegotiating acceptance criteria

**Do NOT emit for:**
- Normal implementation challenges within original scope
- Expected test failures during TDD red phase
- Questions about implementation detail (ask in task context, not signal)

## Signal Kinds

| Kind | When | Severity guidance |
|---|---|---|
| `scope_drift` | Work exceeds original boundaries | `high` if >3 new subsystems or >100 LoC unexpected |
| `blocked` | External dependency prevents progress | `high` if no workaround exists; `medium` if workaround is ugly |
| `risk` | Design risk that may affect other tasks | `high` if affects >2 other tasks or core invariant |
| `completion` | Task done, ready for review | Always emit via normal completion path; use signal only if bypassing orchestrator |

## Exact Emission Format

A signal has TWO mandatory parts. Either alone is incomplete and will be ignored or misprocessed.

### Part 1: Comment Sentinel

Write a comment on the beads issue with this exact format:

```markdown
[[spur-signal v1]]
{
  "signal_id": "<uuid-v4>",
  "kind": "<kind>",
  "severity": <0.0-1.0>,
  "reason": "<one sentence describing what changed>",
  "estimated_subtasks": <number of additional tasks needed, 0 if unknown>
}
```

**Field rules:**
- `signal_id`: UUID v4, fresh for every signal. Never reuse.
- `kind`: One of `scope_drift`, `blocked`, `risk`, `completion`.
- `severity`: Float 0.0–1.0. >0.7 is `high` bucket, 0.4–0.7 is `medium`, <0.4 is `low`.
- `reason`: One sentence, specific. Bad: "Things got complicated." Good: "Auth refactor requires touching 4 new subsystems: session, cookie, csrf, oauth."
- `estimated_subtasks`: Integer. 0 if unknown. Not a promise, just an estimate.

### Part 2: Label

Add a label to the beads issue:

```
signal:<kind>
```

Or with severity bucket:

```
signal:<kind>:high
signal:<kind>:medium
signal:<kind>:low
```

**The label is how the brain's signal watcher finds this issue.** Without the label, the comment is invisible to automated polling.

## Deduplication

Before emitting, check existing labels and comments:

1. Query issue labels. If `signal:<kind>` already exists with similar `reason`, do not duplicate.
2. If the prior signal has `spur:signal-processed:<compact-uuid>` label, the brain already handled it. Emit a NEW signal with a new `signal_id` if the situation changed.
3. Never emit the same `signal_id` twice. The brain deduplicates by ID.

## Example: Scope Drift

**Scenario:** Task was "Add email validation to signup form." Worker discovers the validation library is incompatible with the existing form framework and requires migrating the entire form system.

**Action:**

```markdown
[[spur-signal v1]]
{
  "signal_id": "550e8400-e29b-41d4-a716-446655440000",
  "kind": "scope_drift",
  "severity": 0.85,
  "reason": "Email validation library incompatible with legacy form framework; migration touches 6 forms across 3 modules",
  "estimated_subtasks": 3
}
```

Label added: `signal:scope-drift:high`

**Then:** Stop work. Do not continue into the drifted scope. The brain will re-plan.

## Example: Blocked

**Scenario:** Task depends on API endpoint `/v2/verify` which returns 503 on every request.

**Action:**

```markdown
[[spur-signal v1]]
{
  "signal_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
  "kind": "blocked",
  "severity": 0.9,
  "reason": "Dependency endpoint /v2/verify unavailable (503) since 2026-04-22T09:00Z; blocking all verification logic",
  "estimated_subtasks": 0
}
```

Label added: `signal:blocked:high`

**Then:** Document what you've done so far. Wait for brain unblock or re-assignment.

## Red Flags — STOP and Signal

- "I'll just handle the extra scope myself" → STOP. Signal scope_drift.
- "The dependency might come back online soon" → STOP. Signal blocked now; update if unblocked.
- "This is a small risk, not worth bothering the brain" → STOP. Small risks compound. Signal risk.
- "I already mentioned this in my task summary" → STOP. Task summaries are ephemeral. Signals are durable.
- "I don't know the exact severity, so I won't emit" → STOP. Estimate. 0.5 if genuinely unsure.

## Rationalization Prevention

| Excuse | Reality |
|---|---|
| "The brain will see my task output" | Task output is not structured. The brain polls signals specifically. |
| "I'll finish the extra scope quickly" | Quick scope creep is still scope creep. Signal first. |
| "The dependency is probably temporary" | Temporary blockages have unknown duration. Signal so the brain can re-plan. |
| "I don't want to look like I'm giving up" | Signals are professional communication, not surrender. |
| "The signal format is too much work" | Copy the template. Fill four fields. Takes 30 seconds. |

## Cross-References

- **spur-way** — beads-first invariant and why signals matter
- **beads-lifecycle** — How signals translate to status transitions
- **AGENTS.md** — Authoritative label vocabulary
