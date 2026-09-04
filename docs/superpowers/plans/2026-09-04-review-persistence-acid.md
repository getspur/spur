# ACID Review Persistence Implementation Plan

> Issue: `bd-2zxiu`

**Goal:** Make a non-advisory `review_task` transition and all of its terminal
audit records one atomic, durable, idempotent Beads transaction.

**Architecture:** Extend the PM interface with an atomic ordered-update method
and an applied/already-applied outcome. Implement it only for Beads using one
`SqliteStorage::mutate` closure. Route the complete `PendingBeadsOp` set through
that method and keep cache publication behind commit/read-back.

## Task 1: Lock the regression with failing tests

**Files:**

- Modify: `crates/spur-pm/src/beads_crate/issue_tracker.rs`
- Modify: `crates/spur-core/src/plan/mod.rs`

Add a Beads test that injects a failure on the terminal audit insert after an
earlier status/comment operation. Assert rollback of status, labels, comments,
events, dirty markers, and idempotency marker. Add a core test requiring one
atomic batch retry instead of successful-prefix tracking.

Run the focused tests and confirm they fail because the atomic API or behavior
does not exist. Commit the regression tests separately.

## Task 2: Add the PM atomic-update contract

**Files:**

- Modify: `crates/spur-pm/src/types.rs`
- Modify: `crates/spur-pm/src/adapter.rs`
- Modify: `crates/spur-pm/src/service.rs`

Add `AtomicUpdateOutcome`, per-issue optimistic concurrency preconditions, and
an object-safe `update_issues_atomically` method.
The default implementation must return an unsupported error rather than fall
back to sequential writes. `PmService` dispatches to Beads and rejects GitHub.

## Task 3: Implement the Beads transaction

**Files:**

- Modify: `crates/spur-pm/src/beads_crate/issue_tracker.rs`

Preflight the idempotency key, labels, issue existence, and one observed
comment-count precondition per updated issue.
Inside one `SqliteStorage::mutate`, claim the metadata marker and apply every
status, label, comment, event, dirty marker, and cache invalidation. Preserve
ordered comments. Return `AlreadyApplied` when the marker exists.

Run `scripts/spur-cargo test -p spur-pm`.

## Task 4: Route non-advisory review writes atomically

**Files:**

- Modify: `crates/spur-core/src/plan/mod.rs`

Extend `PmLike`, delegate through `PmService`, derive a decision-independent
key from plan/task/attempt/maker-delegation, and replace per-operation suffix
retry with whole-transaction retry. Reject non-advisory mode when no PM backend
exists. Retain the current post-commit comment read-back for newly applied
transactions and leave advisory mode unchanged.

Run focused review tests and `scripts/spur-cargo test -p spur-core`.

## Task 5: Verify the invariant and repository quality

Run formatting, focused tests, relevant crate tests, and clippy through
`scripts/spur-cargo`. Repeat the SOLVE snapshot verification: complete states
must pass and `closed + absent` must fail. Inspect the final diff, update and
close `bd-2zxiu`, and commit the implementation with the issue ID.

## Result

Implemented. The post-change solver verification accepts `open + absent` and
`closed + approval`, and rejects `closed + absent` with
`data_integrity.mutually_consistent.violation`.
