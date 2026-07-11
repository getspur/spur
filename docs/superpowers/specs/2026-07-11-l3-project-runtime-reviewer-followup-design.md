# L3 Project Runtime Reviewer Follow-up Design

**Status:** Approved by operator on 2026-07-11

**Base implementation:** WIP commit 203c1b835 on
spur/worker/v2/codex/86e96d7c00dfba35/385b6495-22b1-48f4-9c9b-dcea5c9da22e

**Supersedes:** The unattended-review, historical-owner adoption, and bounded-shutdown
details in the 2026-07-10 L3 project runtime leadership design. All other parts of
that design remain in force.

## Goal

Finish the repository-scoped L3 runtime without requiring an active owner
BrainSession, while restoring a real maker/checker boundary, preventing leadership
overlap during slow cancellation, and making pre-upgrade ownership transfer an
explicit operator action.

## Decisions

1. A fresh reviewer worker, not a BrainSession and not the maker callback, decides
   whether a completed system-owned L3 task is approved or returned for changes.
2. A valid mark-noop signal is evidence for the reviewer and is not a review blocker.
   Other unresolved worker signals remain fail-closed.
3. Leadership remains held until every old runtime child acknowledges termination.
   If graceful shutdown reaches its deadline, the guard moves to a detached drain
   task rather than being dropped early.
4. The project runtime never auto-adopts a brain-owned pre-upgrade generation.
   Operators may explicitly migrate one after verifying that its former owner is no
   longer active.

## Non-goals

- An always-on daemon.
- A hidden or synthetic ACP brain session.
- Semantic code review performed by hard-coded allowlists.
- Automatic liveness detection for an older binary that cannot publish the new owner
  lease.
- Replacing the existing plan projector or review transition implementation.
- Different reviewer vendors or models. A distinct session and delegation is the
  required boundary; the configured maker agent may also be used as the reviewer.

## Durable Reviewer Protocol

### Review identity

The maker attempt is identified by:

~~~
(plan_id, task_id, attempt, maker_delegation_id, maker_worker_branch)
~~~

There may be at most one live review attempt for that identity. A reviewer attempt has
its own delegation ID and a companion beads issue. The companion issue exists before
dispatch and is not a child plan task, so it cannot enter the generation DAG or
recursively require another reviewer.

The target task receives audit sentinels that bind the two identities:

~~~rust
SystemReviewDispatch {
    plan_id: String,
    task_id: String,
    attempt: u32,
    maker_delegation_id: String,
    reviewer_delegation_id: String,
    review_issue_id: String,
}

SystemReviewVerdict {
    maker_delegation_id: String,
    reviewer_delegation_id: String,
    review_issue_id: String,
    decision: SystemReviewDecision,
    feedback: String,
    evidence: Vec<String>,
}
~~~

The dispatch audit is written before the in-process delegation send. The tuple above
is the idempotency key for crash recovery. Reconciliation reuses a live companion
issue or repairs an expired review attempt; it never creates two live reviewer
delegations for one maker attempt.

### Review eligibility and signals

A maker result is eligible only when all of these remain true:

- The plan is system-owned L3 and the target task is AwaitingReview.
- The latest maker completion is AwaitingReview, non-superseded, and has a worker
  branch.
- The completion delegation ID matches the projected task's latest delegation ID.
- No unresolved blocking signal is attached to the target.

Signal classification is comment/audit based rather than a prefix-only label check:

- MarkNoop is allowed and included in the reviewer prompt.
- ScopeDrift, Escalate, RetryExhausted, PotentialClobber, risk, blocked, and unknown
  signal kinds block while unresolved.
- A processed signal does not remain blocking merely because its signal-kind label is
  retained for history.

An empty maker diff without MarkNoop is not auto-approved. It is shown to the reviewer,
who must request changes unless the task genuinely produced a reviewable non-code
artifact covered by the task acceptance criteria.

### Reviewer dispatch

The project reconciler creates a regular companion beads task carrying labels that
identify it as a system review and link it to the target issue and maker delegation.
It then dispatches a fresh worker session:

- base: the maker worker branch;
- agent: the maker task's configured agent;
- profile/model/effort: inherited unless the task already supplies an explicit
  reviewer override in a future version;
- worker MCP enabled;
- issue_id: the companion review issue, satisfying beads-first intent and normal
  dispatch observability.

The reviewer prompt is read-only. It requires the worker to inspect get_task_diff,
the branch diff, relevant tests, acceptance criteria, and signals, then make exactly
one submit_review_verdict call. Any code changes made by the reviewer stay on its
throwaway branch and are never merged into the maker result.

### Authenticated verdict tool

Worker MCP gains submit_review_verdict with:

~~~json
{
  "target_issue_id": "bd-...",
  "decision": "approve | request_changes",
  "feedback": "non-empty explanation",
  "evidence": ["command/result or inspected invariant"]
}
~~~

The handler uses WorkerCallContext.delegation_id and accepts the call only when:

1. the target's latest pending SystemReviewDispatch names that reviewer delegation;
2. the bound maker completion is still the current completion;
3. the companion review issue is still live and bound to the same IDs; and
4. no verdict for that reviewer delegation already conflicts with the request.

The handler writes SystemReviewVerdict to the target but does not close the companion
issue while the reviewer process may still be unwinding. Repeating the byte-equivalent
verdict is idempotent. A maker delegation, stale reviewer, or arbitrary worker receives
Unauthorized and cannot mutate review state.

### Applying the verdict

The reconciler consumes a valid verdict through the existing non-advisory review
transition:

- approve maps to review_task approve for the maker delegation;
- request_changes maps to review_task request_changes with reuse_prior_worktree=true;
- request_changes at the existing maximum maker attempts follows the existing
  auto-rejection rule.

The reviewer worker never writes Approval directly. The reconciler checks the durable
binding again immediately before applying the transition, then records the normal
approval or review-feedback audit. After observing the reviewer result, it closes the
companion issue idempotently; this also repairs a late generic completion update that
reopened the companion. Replays are harmless because the target is no longer
AwaitingReview after a consumed verdict.

### Reviewer failure and recovery

A reviewer process result is review-only evidence: its branch is never integrated and
the absence of code changes is not an error when a durable verdict exists. A reviewer
result without a durable verdict does not advance the maker task.
The companion review issue is reopened after the review dispatch lease expires.
Reconciliation may retry a review attempt up to three times. Exhaustion adds a
blocking review-failed signal/audit to the target and parks it for explicit
intervention; it never falls back to metadata-only approval.

After leader crash, the successor reconstructs pending review dispatches and verdicts
from beads. It may repair a missing in-process dispatch only after the recorded review
lease is stale. This uses the same durable-intent-before-send ordering as maker task
dispatch.

## Shutdown and Leadership Handoff

The runtime shutdown deadline bounds how long the interactive shutdown caller waits;
it does not authorize overlapping leaders.

The supervisor owns both the advisory leadership guard and the running runtime. On
retirement:

1. Stop new scheduler/reconciler work and request cancellation.
2. Wait through the normal grace deadline.
3. Abort any remaining async child and force-abort callback-server tasks.
4. Await acknowledgement from every aborted JoinHandle.
5. Drop the leadership guard only after step 4.

If step 4 is not complete at the caller's deadline, the supervisor moves the guard and
remaining drain future into a detached LeaderDraining task. The public shutdown call
may return, but an in-process standby cannot acquire the repository lock until that
drain task completes. Process death remains safe because the operating system closes
the lock file descriptor.

Tests must use a controllable drain that outlives the grace deadline and prove a second
supervisor cannot enter LeaderRunning until the drain acknowledgement is released.
A real runtime test must also prove the aborted delegation JoinHandle is awaited,
not merely observed once after yield_now.

## Manual Legacy Migration

SystemL3Only reconciliation lists and heartbeats only epics already owned by
spur-loop-runtime. It does not install grace leases on brain-owned historical plans
and does not reclaim them after time.

The existing force_reclaim_plan operator tool gains target_owner:

- current_brain: existing behavior and default;
- system_l3: allowed only for an open L3 generation epic and requires confirm=true.

For system_l3 the tool atomically removes old owner, lease, and owner-token labels;
installs spur:plan-owner:spur-loop-runtime plus a fresh token/lease; and appends the
existing PlanForceReclaimed audit with the operator reason. The operator is responsible
for first stopping or otherwise verifying the former owner. This is deliberate
governance, not an inferred liveness decision.

Mixed-version documentation must state that old generations remain parked until this
explicit migration. New generations created by the new runtime are system-owned and
continue to fail over automatically between current-version TUIs.

## Required Tests

### Reviewer protocol

- MarkNoop reaches a reviewer; an unresolved blocking signal does not.
- A processed historical signal does not block solely because its label remains.
- A maker delegation cannot submit a reviewer verdict.
- A stale reviewer cannot verdict a newer maker completion.
- Dispatch intent is durable before reviewer send and replay creates at most one live
  review.
- Approve advances dependants; request_changes reuses the maker branch.
- Reviewer completion without verdict never approves.
- Three reviewer failures park the target rather than auto-approving.

### Shutdown

- Standby cannot acquire while the former leader's drain is unacknowledged.
- The real delegation JoinHandle is awaited after abort.
- Once drain acknowledges, exactly one standby promotes.

### Migration

- System runtime never modifies a brain-owned legacy L3 epic.
- force_reclaim_plan target_owner=system_l3 rejects non-L3 and missing confirmation.
- Explicit migration writes the system owner, fresh fencing labels, and audit.
- Migrated work is subsequently visible to SystemL3Only reconciliation.

### End to end

With no active BrainSession, a due two-task L3 generation dispatches maker T1, creates
a distinct reviewer delegation for T1, consumes its authenticated approval, dispatches
T2, repeats review, and writes the terminal LoopRun. Two concurrent TUIs still produce
one maker and one reviewer per attempt.

## Acceptance Criteria

- No owner BrainSession is required for scheduling, maker dispatch, reviewer dispatch,
  verdict application, retries, terminal projection, or failover.
- No code path approves an L3 maker solely from Completion metadata.
- Exactly one distinct reviewer verdict authorizes each approval.
- MarkNoop is reviewable and other unresolved signals fail closed.
- In-process leadership never overlaps an unacknowledged prior runtime.
- Legacy brain-owned plans are changed only by an explicit confirmed operator action.
- Focused tests, the complete spur-core suite subject only to an independently
  reproduced pre-existing failure, formatting, and remote clippy are clean.
