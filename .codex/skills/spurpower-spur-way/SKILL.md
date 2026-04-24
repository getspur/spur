---
name: spurpower-spur-way
description: "Use when acting as any agent in the SPUR system — brain, worker, or reviewer. Establishes beads as the single source of truth for all collaboration state and mandates the three primitives of every transaction."
---
<!-- SPUR-MANAGED v=1 skill=spurpower-spur-way sha256=80ec38d64048f930d8ab3c236d17bfceb8a71ab7fd7916a58650cefd9821bd4f -->

# The Spur Way

## Overview

SPUR is a brain-worker collaboration system where **beads is the sole source of truth**. Every decision, action, and outcome MUST be recorded in beads. Agents that bypass beads create invisible state that corrupts retry, review, and lineage.

**Core principle:** INTENT → ACTION → RECORD. If beads doesn't have it, it didn't happen.

**Violating the letter of this rule is violating the spirit of the system.**

## The Three Primitives

Every brain-worker transaction has three irreducible steps:

1. **INTENT** — Brain decides work should happen. Expressed as a beads issue (or plan task) with `delegation_plan`, `spur:plan-id`, and `spur:agent` labels.
2. **ACTION** — Worker executes in an isolated worktree. Produces diffs, tests, and signals.
3. **RECORD** — beads persists the outcome via status transitions, `spur-audit` comments, and `signal` labels.

Skip RECORD and the next brain session, retry attempt, or reviewer has no ground truth.

## The Iron Law

```
NO DELEGATION WITHOUT A BEADS ISSUE
NO COMPLETION WITHOUT A BEADS UPDATE
NO SIGNAL WITHOUT A BEADS COMMENT + LABEL
```

## Invariant B1: beads-First

Every delegation MUST have a corresponding beads issue that exists before dispatch and is updated after completion.

**Exceptions (rare and explicit):**
- <15min exploratory tasks MAY be ephemeral IF the brain logs rationale in the nearest parent issue's comments.
- Emergency hotfixes MAY skip pre-creation IF an issue is created retroactively within 5 minutes of dispatch.

**Non-exceptions (never rationalize):**
- "I'll create the issue after" → No. Create before dispatch.
- "This is just a quick fix" → If >15min, it needs an issue.
- "The plan engine handles it" → The plan engine creates beads issues. If you're not using the plan engine, YOU create the issue.
- "Beads is slow right now" → Wait. Dispatch without RECORD is invisible work.

## What beads Knows (and therefore the system knows)

| What | Where in beads | Who writes |
|---|---|---|
| Task exists | Issue created with title, body, labels | Brain (or plan engine) |
| Task assigned | `assignee` field + `spur:agent` label | Orchestrator on dispatch |
| Work started | Status `in_progress` | Orchestrator or worker |
| Work completed | `spur-audit v1` comment with `kind: completion` | Orchestrator or worker |
| Scope changed | `spur-signal v1` comment with `kind: scope_drift` | Worker |
| Blocked | `spur-signal v1` comment with `kind: blocked` | Worker |
| Brain approved | `spur-audit v1` comment with `kind: approval` | Brain via `review_task` |
| Brain rejected | `spur-audit v1` comment with `kind: rejection` | Brain via `review_task` |
| Task superseded | `spur:superseded-by:<id>` label | Brain mutation executor |

If any row is missing for a delegation, the collaboration record is incomplete.

## Forbidden Patterns

**NEVER do these. They destroy observability:**

- Create a parallel tracking system (notes file, mental model, session memory) that competes with beads
- Update a local todo list instead of beads status
- Emit a signal in chat/text instead of a `spur-signal` sentinel comment
- Trust your memory of what a worker did — check beads
- Dispatch a worker and assume "the orchestrator will handle beads" without verifying
- Approve a worker's output without checking that beads has the completion audit

## Red Flags — STOP and Fix beads State

- About to delegate without an `issue_id` → STOP. Create issue first.
- Worker reported success but beads issue is still `open` → STOP. Update beads before approving.
- About to retry a task and can't find the previous attempt's audit trail → STOP. The audit trail is the retry context.
- Considering "I'll update beads later" → STOP. Later never comes.
- Two workers touched the same file and beads shows no conflict record → STOP. Add conflict annotation.

## For Brain Agents

**Before dispatch:**
1. Does a beads issue exist for this work? If no, create one.
2. Does the issue have `spur:plan-id` if this is plan work?
3. Is the issue status `open` (not already `in_progress` by another worker)?

**After worker returns:**
1. Check beads issue for `spur-audit` completion comment.
2. Verify issue status reflects reality (not still `open` if worker claims done).
3. Look for `signal:*` labels requiring brain response.
4. Only then call `review_task` with approve/reject.

## For Worker Agents

**Before starting work:**
1. Verify you have an `issue_id` in your task context.
2. Check beads issue status. If not `in_progress`, do not start.
3. Confirm no `spur:superseded-by` label exists (task cancelled).

**During work:**
1. If scope grows beyond original issue → emit `scope_drift` signal immediately.
2. If blocked by external dependency → emit `blocked` signal.
3. If you discover a design risk → emit `risk` signal.

**After completing work:**
1. Ensure your output is captured (diff, test results).
2. Do NOT assume the orchestrator updated beads. Verify.
3. If the orchestrator hasn't written an audit comment, the brain may not see completion.

## Rationalization Prevention

| Excuse | Reality |
|---|---|
| "Beads is just overhead" | beads IS the system. Without it, retry, review, and lineage fail. |
| "The orchestrator handles beads" | The orchestrator auto-updates on best effort. Brain and worker are responsible for correctness. |
| "I'll batch updates" | Batched updates are lost updates. Update at boundaries. |
| "Signals are optional" | Signals are how workers communicate upward. Without them, the brain is blind to blockers. |
| "This is different because..." | No. Intent → Action → Record. Every time. |

## Cross-References

- **beads-lifecycle** — Status state machine and label semantics
- **worker-signals** — How and when to emit signals
- **brain-review-gate** — beads-aware review checklist
- **plan-task-discipline** — DAG order for plan tasks
- **AGENTS.md** — Authoritative label vocabulary and sentinel formats
