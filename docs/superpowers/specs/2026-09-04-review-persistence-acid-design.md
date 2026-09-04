# ACID Review Persistence Design

**Issue:** `bd-2zxiu`  
**Status:** Implemented

## Problem

`review_task` builds one logical review transition from several PM mutations:

1. change the task issue status and labels;
2. add the human-readable brain review comment;
3. add the terminal approval/rejection sentinel on the task;
4. add the task-transition sentinel on the plan epic.

Non-advisory mode currently submits those mutations one at a time. Each
`BeadsCrateAdapter::update_issue` may itself use more than one SQLite
transaction. A later failure therefore leaves a durable prefix, including the
forbidden `closed + no terminal audit` snapshot. Retrying only the failed suffix
prevents duplicate comments but cannot undo the exposed prefix.

## Invariant

For a task participating in a terminal review, the observable tuple
`(issue_state, terminal_audit)` must be one of:

- `(open, absent)` before review;
- `(closed, approval)` after approval;
- `(closed, rejection)` after rejection.

`(closed, absent)` is forbidden. The pre-implementation SOLVE check uses
`data_integrity.mutually_consistent`: both complete states are satisfiable and
the partial state is unsatisfiable with
`data_integrity.mutually_consistent.violation`.

A `SystemReviewVerdict` is intentionally not terminal task evidence. It is an
authenticated request for the reconciler to apply a decision; only the
approval/rejection/review-feedback audit committed with the issue mutation may
advance projected task state.

## Transaction boundary

Add an atomic multi-issue update operation to the PM abstraction. The Beads
implementation executes the full ordered update list inside one
`SqliteStorage::mutate` call, which provides an IMMEDIATE SQLite transaction.
That transaction includes issue status, closed timestamp, label changes,
comments, Beads events, dirty markers, blocked-cache invalidation, and a durable
idempotency marker.

The operation is deliberately unavailable on backends that cannot provide the
contract. Non-advisory reviews already require the Beads advanced surface, so a
backend without atomic updates fails before any write. Advisory mode keeps its
existing best-effort behavior. Non-advisory mode also rejects a missing PM
backend or a task without a durable issue ID before publishing candidate state.

The transaction preserves the complete current `IssueUpdate` surface so the
same primitive can also make a single mixed field/label/comment update atomic.
Review currently uses status, labels, and comments. Labels, issue existence,
the idempotency key, and optimistic concurrency preconditions are validated
before any mutation becomes durable.

## Retry and read-back

The caller supplies a deterministic idempotency key containing plan, task,
attempt, and maker delegation, deliberately excluding the decision. Approve,
reject, and request-changes calls for the same review epoch therefore share one
identity. The caller also supplies the observed per-issue comment counts; the
transaction compares them before writing, so a differently keyed stale writer
cannot overwrite a review that just committed. The Beads transaction inserts a
namespaced marker in the existing metadata table. Marker and review rows commit
together:

- no marker means the transaction may be attempted;
- an existing marker means the exact logical transition already committed and
  a retry is a no-op;
- a transaction error rolls back both marker and every review mutation.

For a newly applied transaction, the existing comment-count read-back must
advance before the in-memory plan cache is replaced. An already-applied result
is itself a durable marker read-back from a later transaction and is accepted
without appending comments again.

## ACID properties

- **Atomicity:** one SQLite transaction contains all task and epic mutations.
- **Consistency:** preflight validation plus the allowed-snapshot invariant
  prevents a committed terminal status without its audit.
- **Isolation:** the adapter's cross-process write lock and SQLite IMMEDIATE
  transaction serialize writers; in-transaction comment-count compare-and-set
  guards prevent two decisions based on the same review snapshot from both
  committing.
- **Durability:** cache publication follows successful commit and read-back;
  the idempotency marker survives process restart.

## Failure behavior

A trigger-injected failure on a late audit-comment insert must leave the task
open, preserve review-ready labels, add no review comments or events, mark no
issue dirty, and persist no idempotency marker. Removing the trigger and
retrying must commit the complete transition. Repeating the successful request
with the same key must not duplicate comments. A competing decision using stale
preconditions must fail without changing the winning status or audits.
