# L3 Project Runtime Leadership Design

**Status:** Approved for implementation planning
**Date:** 2026-07-10

## Problem

SPUR documents L3 loops as unattended, engine-armed work. The current runtime does
not meet that contract: the loop scheduler, reconciler dispatch context, worker
delegation channel, and MCP server are all owned by an active brain session. Retiring
that brain shuts down the scheduler and prevents a due L3 generation from starting.

Moving L3 work merely from `BrainSession` to `Orchestrator` is insufficient. Multiple
SPUR TUI processes may be open against the same repository and beads issue database.
If every process starts an independent project runtime, each may observe the same due
loop and create or dispatch duplicate work.

## Goals

- Run L3 generations without any active brain session while at least one SPUR process
  for the repository remains alive.
- Elect exactly one SPUR process to schedule and execute L3 work for a repository.
- Recover automatically when the elected process exits or crashes.
- Keep the beads issue database as the durable source of truth for loop and plan state.
- Preserve L1/L2 as brain-armed modes.
- Establish a runtime boundary that can move into an always-on daemon later.

## Non-goals

- Running loops after every SPUR process has exited. That requires the later daemon
  phase.
- Cross-host leadership on filesystems without reliable advisory locks.
- Cross-process live event fanout between TUI processes. Non-leaders observe durable
  progress through issue refreshes.
- Redesigning L1/L2 ownership or selecting between active brains in different SPUR
  processes.
- Replacing the beads adapter's existing short-lived write lock.

## Current-State Findings

- `Reconciler::run_loop_scheduler_sweep` returns without doing work when its
  session-owned `ReconcilerDispatch` is absent.
- The L3 branch directly persists the stored template, but overwrites the generation's
  owner with `dispatch.brain_session_id()`.
- `McpCallbackServer::enable_reconciler` requires a bound brain session ID.
- `BrainSession` owns the MCP server, reconciler task, and delegation handler and shuts
  them down during retirement.
- The original loop design explicitly describes L3 as engine-armed and says live-brain
  dependence is intentional only for L1/L2.
- SPUR already uses `fs4` exclusive advisory locks with holder metadata for
  cross-process session attachment. The beads adapter also serializes individual
  writes under a filesystem lock, but that lock is not held long enough to elect a
  scheduler leader.

## Decision Summary

1. Add a project-owned L3 runtime whose lifetime is the owning SPUR process, not a
   brain session.
2. Gate that runtime with a repository-scoped, long-held exclusive advisory lock at
   `.spur/loop-runtime.lock`.
3. Only the lock holder may scan, persist, dispatch, reconcile, or merge L3 work.
4. Other SPUR processes remain passive standbys and periodically retry leadership.
5. Use a stable system execution identity for L3 plans instead of the creator brain
   session identity.
6. Fail closed when advisory locking is unsupported or indeterminate.
7. Keep L1/L2 scheduling in brain-session runtimes and exclude L3 from those sweeps.

## Architecture

### Repository leadership guard

Introduce a `LoopRuntimeLeadership` guard following the existing
`SessionAttachGuard` pattern:

- Open `.spur/loop-runtime.lock` without truncating it.
- Attempt `fs4::FileExt::try_lock_exclusive`.
- Keep the open file descriptor in the guard for the full L3 runtime lifetime.
- Write diagnostic holder metadata after acquisition: process ID, start time,
  optional TUI label, and working directory.
- Return one of three outcomes: `Acquired`, `Standby { holder }`, or `Unsafe/Error`.

Dropping the guard releases leadership through the operating system. The lock file may
remain on disk; file existence never implies ownership.

Unlike interactive session attachment, L3 leadership has no degraded no-lock mode.
`ENOTSUP`, `ENOLCK`, and equivalent conditions disable L3 execution and emit a clear
diagnostic. Duplicate unattended execution is more dangerous than parking a due loop.

### Project L3 runtime

`Orchestrator` owns a `ProjectLoopRuntimeSupervisor` for its repository. The supervisor
exists independently of `BrainSession` and has these states:

```text
Standby -> Acquiring -> LeaderRunning -> LeaderStopping -> Standby
                  \-> UnsafeDisabled
```

- `Standby` retries the non-blocking lock on a bounded interval.
- `LeaderRunning` owns the leadership guard, L3-scoped scheduler/reconciler, worker
  delegation handler, task tracker, fast-forward notification, and event sink.
- `LeaderStopping` first stops/drains runtime work and only then drops the guard, so a
  healthy handoff cannot overlap two runtimes.
- `UnsafeDisabled` does not retry aggressively; it reports that the filesystem cannot
  safely host unattended L3 work.

Normal application shutdown calls the supervisor's bounded async shutdown before the
`Orchestrator` is dropped. `Drop` remains the abort fallback; aborting the supervisor
task drops its leadership guard. Therefore phase one runs L3 only while some SPUR
process remains open.

### Explicit reconciliation scopes

Add explicit scope rather than relying on incidental owner mismatches:

- Project runtime: `LoopSweepScope::L3Only` and `PlanScope::SystemL3Only`.
- Brain-session runtime: `LoopSweepScope::BrainArmedOnly` and its existing plan-owner
  scope.

The project runtime ignores L1/L2 loops and non-system plans. Brain-session runtimes
ignore L3 loop triggers and system-owned L3 plans. These filters prevent duplicate
scheduling and make the two runtime roles auditable in tests.

### System execution identity

New L3 generations use the reserved system owner ID `spur-loop-runtime`. The ID is
stable across process handoffs and is scoped by the repository's issue database. It is
represented through the existing `BrainSessionId` interfaces only where legacy
plan/delegation APIs still require that type. It is execution ownership, not creator
identity.

The loop issue and submission audit remain the provenance record for who created or
promoted the loop. No active creator session is required, and retiring that session
does not change L3 ownership.

Existing L3 generations owned by a real brain are not stolen while that owner is live.
The project runtime adopts or resumes them only through the existing persisted
ownership/lease recovery rules after the prior owner is stale.

The project runtime composes worker dispatch and worker-MCP dependencies under the
system identity. It does not create an ACP brain connection, brain prompt session, or
brain tool registry.

### Generation identity and crash repair

The durable generation key is `(loop_id, generation)`, represented by the existing
`spur:loop-id:*` and `spur:loop-generation:*` plan labels. Before persisting a new L3
plan, the leader queries for that exact key across open and terminal generation plans.

- If no exact generation exists and no older generation is live, persist it normally.
- If the exact generation already exists, do not persist another plan and do not write
  a `skipped_overlap` run. Repair any missing next-run label, then resume reconciliation
  if the plan is non-terminal or repair terminal projection if its run record is
  missing.
- If a different, older generation is still live, retain the existing
  `skipped_overlap` behavior.

This exact-key repair path closes the crash window between generation persistence and
loop re-arm. The leadership lock prevents concurrent healthy writers; durable identity
makes takeover idempotent.

## Runtime Flows

### Startup with multiple TUIs

1. Each SPUR process starts its project runtime supervisor.
2. Each attempts the same repository lock.
3. One process acquires it and starts the L3 runtime.
4. All others record the holder as leader and remain standby.
5. Every process may read and render loop/plan state from the shared issue database,
   but only the leader mutates L3 execution state.

### Due L3 generation

1. The leader observes a due, open, unpaused L3 loop.
2. Existing governor, overlap, backoff, and template validation checks run unchanged.
3. The stored template is persisted as a generation plan under the system owner.
4. The L3-scoped reconciler discovers the system-owned plan and dispatches workers.
5. Run records, task state, leases, and completion remain durable in beads.
6. The loop is re-armed for its next cadence.

No brain continuation, brain prompt, or active brain MCP server participates.

### Leader retirement or graceful process exit

1. Stop accepting new L3 scheduling decisions.
2. Shut down/drain the project reconciler and delegation handler using the established
   bounded shutdown behavior.
3. Drop the leadership guard.
4. A standby acquires the lock and reconstructs state from beads.

### Leader crash

The OS releases the advisory lock when the process dies. A standby then acquires it.
Recovery depends on the durable boundary at which the crash occurred:

- Before plan persistence: the successor creates the still-due generation.
- After plan persistence but before loop re-arm: exact-generation lookup prevents a
  duplicate, and the successor repairs the schedule without recording a false overlap.
- During delegation: dispatch leases and startup recovery reclaim or re-dispatch the
  task after the old lease expires.
- After terminal persistence: the successor projects the terminal state and completes
  any missing loop run record/re-arm work idempotently.

The leadership lock prevents healthy concurrent leaders; existing plan and dispatch
leases provide crash recovery after leadership changes.

## Concurrency Invariants

1. At most one process holds `.spur/loop-runtime.lock` for a repository.
2. A process must hold the guard before starting any L3 scheduler or reconciler task.
3. The guard outlives every task that can mutate L3 execution state.
4. Brain-session reconcilers never arm L3 generations.
5. The project runtime never arms L1/L2 generations.
6. The project runtime never dispatches a non-system-owned plan.
7. Lock acquisition failure never falls back to unsafe execution.
8. Durable labels, sentinels, and dispatch leases remain authoritative after failover.
9. At most one plan may exist for an exact `(loop_id, generation)` key under supported
   same-version operation.

## Error Handling and Observability

- A held lock is normal standby state, not an error.
- Holder metadata is logged so operators can identify the executing TUI process.
- Unsupported locking emits a prominent warning and leaves L3 loops due.
- A runtime startup failure keeps the elected process in a degraded leader state with
  bounded retries; it does not rapidly release/reacquire the lock across processes.
- Non-leading TUIs show durable state after PM refresh but do not receive the leader's
  process-local live event stream in phase one.
- Existing loop auto-pause, budget, overlap, and escalation behavior remains intact.
  With no locally attached brain, auto-pause and escalation state is persisted in
  beads, but phase one does not route a live continuation to a brain in another SPUR
  process. Operators see it through refreshed loop status.

## Testing Strategy

### Lock tests

- First guard in a temporary repository acquires leadership.
- A subprocess holding the same repository lock forces the test process into standby
  with holder metadata, proving real cross-process exclusion rather than relying on
  same-process lock semantics.
- Dropping the first guard lets the second acquire leadership.
- Different repositories can have independent leaders.
- Unsupported-lock injection fails closed and never starts the runtime.

### Scheduler and scope tests

- Project scope selects L3 and excludes L1/L2.
- Brain-armed scope selects L1/L2 and excludes L3.
- A brain-session reconciler cannot persist an L3 generation.
- A project reconciler cannot dispatch a brain-owned non-L3 plan.

### Multi-process/runtime tests

- Two supervisors sharing one repository start exactly one L3 runtime.
- A due L3 loop produces exactly one generation while both supervisors are alive.
- Dropping the leader promotes the standby and later due work continues.
- Retiring the loop creator brain does not stop L3 scheduling or dispatch.
- With no active brain, L1/L2 remain due and do not create generations.
- A crash-window fixture with an already-persisted live generation does not duplicate
  it after takeover.
- Exact-generation repair re-arms without writing a false `skipped_overlap` record.

### Regression verification

- Existing loop scheduler, reconciler, ownership, dispatch lease, and terminal
  projection suites continue to pass.
- Full `spur-core` tests run through `scripts/spur-cargo`.
- Remote clippy and workspace formatting remain clean.

## Migration and Compatibility

- `LoopSpec` and existing loop labels require no schema change.
- New L3 plans use system ownership; existing plans retain their current owner until
  normal recovery safely transfers them.
- Existing runtime switches (`loops_enabled`, `pause_all_loops`) also gate the project
  runtime.
- When multiple current-version and older SPUR processes share a repository, the older
  process does not understand the leadership lock. Safe mixed-version operation is not
  guaranteed; unattended L3 should require all active processes to use the version
  containing this design.

## Alternatives Rejected

### Keep a hidden brain session alive

This retains the incorrect lifecycle, consumes agent resources, and makes recovery
depend on an unrelated ACP transport.

### Start one L3 runtime per TUI and rely only on overlap checks

The read-check-write sequence is not a leader election primitive. Two processes can
both observe no live generation before either persists one.

### Use the beads write lock as scheduler leadership

The adapter holds its lock only around individual writes. It serializes mutations but
does not stop every process from independently deciding to execute the same due loop.

### Move all reconciliation out of brain sessions immediately

This is a valid longer-term simplification, but it broadens phase one into a full plan
ownership and continuation-routing refactor. A dedicated, explicitly scoped L3 runtime
delivers the approved behavior with a smaller blast radius.

## Future Daemon Phase

The daemon will host the same `ProjectLoopRuntimeSupervisor` and contend for the same
repository lock. TUI processes remain safe standbys. An optional coordinated handoff
or daemon-preference policy can be added later without changing loop persistence,
system ownership, scheduler scopes, or failover rules.
