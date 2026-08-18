---
name: brain-review-gate
description: Use when reviewing worker output before approving — verifies beads state matches claimed completion and enforces the review checklist that prevents invisible work from merging.
role: brain
---

# Brain Review Gate

## Overview

Approving a worker's output without verifying beads state is approving a ghost. The worker may have completed the work, but if beads doesn't record it, retry loses context, lineage shows orphan nodes, and the next brain session has no ground truth.

**Core principle:** Verify beads BEFORE approving. Evidence before decision, always.

## The Iron Law

```
NO APPROVAL WITHOUT BEADS VERIFICATION
```

## Review Checklist

For EVERY completed delegation, perform these checks IN ORDER:

### 1. Issue Status Check

Query the beads issue referenced by `issue_id`.

**Required:** Status is `in_progress` or has a transition comment explaining why it's not.

**If status is `open`:** Worker may not have started, or orchestrator update failed. Investigate.
**If status is `blocked`:** There is an unresolved signal. Read it before deciding.
**If status is `closed`:** Worker or someone else already closed it. Do not re-review.

### 2. Audit Trail Check

Read issue comments. Look for `[[spur-audit v1]]` sentinel.

**Required:** At least one audit comment exists with:
- `kind: completion` for successful work, OR
- `kind: dispatch` + subsequent relevant events

**If no audit comment:** The orchestrator may have failed to write it, or the worker bypassed the system. Ask the worker to confirm completion via proper channel, or reject and request re-submission.

### 3. Signal Scan

Check issue labels for `signal:*`.

**If `signal:scope-drift` present:**
- STOP approval.
- Read the `spur-signal` comment.
- Re-plan. The original task boundaries are violated.
- Options: split task, expand scope, or reject with new constraints.

**If `signal:blocked` present:**
- STOP approval.
- Determine if block is resolved.
- If not resolved, reject with guidance or re-assign.

**If `signal:risk` present:**
- Read the signal comment.
- Evaluate risk against project priorities.
- May approve with risk acceptance note, or reject for mitigation.

### 4. Diff Verification

Compare worker's claimed output against reality:

- `DelegationResult.diff_summary.files_changed` should match actual diff.
- If diff is unexpectedly large, check for scope creep not signaled.
- If diff is empty but worker claims completion, reject.

### 5. Artifact Consistency

Verify that test results, build outputs, or other verification evidence referenced by the worker are present and match claims.

**Required:** Worker output references verification that you can independently check.

## Approval Action

If all checks pass:

1. Call `review_task(decision: "approve", summary: "...")`.
2. The orchestrator adds `spur-audit` approval comment.
3. Do NOT close the issue yet. Brain closes explicitly when ready.

## Rejection Action

If any check fails:

1. Call `review_task(decision: "reject", summary: "...", feedback: "...")`.
2. Feedback MUST be specific and actionable:
   - Bad: "This is wrong."
   - Good: "The diff touches src/auth.rs which is out of scope per the issue. Revert auth changes and resubmit."
3. The orchestrator reverts status to `open` and adds rejection audit comment.

## Retry Action

If rejecting with intent to retry:

1. Include full retry history in feedback (orchestrator Change 3: previous attempts, summaries, reviewer feedback).
2. Set clear constraints: "Do NOT touch src/auth.rs. Use the existing validation module in src/validate/ instead."
3. Verify the worker's next attempt sees the retry context in its augmented task.

## Red Flags — STOP and Reject

- beads issue status doesn't match worker claim → STOP. Verify before deciding.
- Missing `spur-audit` completion comment → STOP. Invisible completion is not completion.
- `signal:scope-drift` unread → STOP. Read it. Re-plan.
- Diff touches files outside issue scope → STOP. Reject for scope violation.
- Worker says "tests pass" but provides no test output → STOP. Request evidence.
- About to approve because "the worker seems competent" → STOP. Verify objectively.

## Rationalization Prevention

| Excuse | Reality |
|---|---|
| "The worker already updated beads" | Verify. Workers sometimes forget. |
| "I've reviewed the diff, beads doesn't matter" | beads is the collaboration record. Diff without beads record is unmergeable. |
| "This is a minor fix, skip the checklist" | Minor fixes introduce regressions too. Run the checklist. |
| "I trust this worker from before" | Every delegation is independent. Verify every time. |
| "The orchestrator handles audit comments" | Best effort. Brain is responsible for correctness. |

## Cross-References

- **spur-way** — beads-first invariant
- **beads-lifecycle** — Status meanings and transition rules
- **worker-signals** — What signals mean and how to respond
- **verification-before-completion** — Evidence discipline for verification steps
