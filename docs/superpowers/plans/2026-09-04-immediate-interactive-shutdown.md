# Immediate Interactive Shutdown Implementation Plan

> Source design: `docs/superpowers/specs/2026-09-04-immediate-interactive-shutdown-design.ipynb`
> Formal contract: `@spec IMMEDIATE-SHUTDOWN`
> Approved design epic: `bd-28i8`
> Implementation epic: `bd-ihy9`

## Goal

Make confirmed interactive quit stop accepting work, force-stop owned brain/worker/MCP/runtime resources concurrently, await one bounded abort-unwind barrier, and return without entering the existing stacked 5-second graceful windows. Keep session swaps, restarts, and ordinary retirement graceful.

## Constraints

- Follow RED → GREEN → REFACTOR for every task; production code is forbidden before the task's targeted test fails for the expected reason.
- Before RED, load the relevant `solve_rule_spec` family summary and persist a task-specific pre-solve. After GREEN, persist the same model with facts matching the landed implementation.
- Use `scripts/spur-cargo`; never invoke bare `cargo`.
- Preserve draft persistence, terminal restoration, `BrainRetired(Shutdown)`, repository-leadership ownership, and child-process termination.
- Confirmed exit may discard in-flight agent/tool output and make final cost/telemetry writes best effort.
- Work the beads DAG sequentially in this brain session; do not submit it for automatic worker dispatch.

## Task DAG

```text
bd-1u6v  worker MCP immediate barrier
   ├──> bd-3c55  project runtime immediate mode ──┐
   └──> bd-3mkg  active brain/root MCP fast stop ├──> bd-1lcd  host wiring
                                                   ┘
```

## Task 1 — `bd-1u6v`: Add zero-grace worker MCP abort barrier

**Files:** `crates/spur-core/src/worker_server.rs`

1. Pre-solve the lifecycle `Running → Fenced → Aborting → Stopped`, requiring handler registration to close before abort and `active_count == 0` on return; persist the solve ID on the bead.
2. Add a regression test around the existing permanently-hung progress call. Call the new immediate API under a short outer timeout without releasing the sink, and assert it returns with `active_count() == 0`.
3. Run the test and record the expected compile/test failure:
   `scripts/spur-cargo test -p spur-core worker_server::tests::shutdown_immediately_aborts_permanently_hung_call -- --exact`
4. Commit the RED test as `test(spur-core): bd-1u6v cover immediate worker MCP abort`.
5. Implement the smallest dedicated immediate method by fencing/cancelling the server and joining the zero-grace background shutdown with handler abort acknowledgement. Do not change `shutdown(Duration)` semantics.
6. Re-run the focused test and the worker-server shutdown test cluster.
7. Post-solve the landed ordering, record test evidence and solve ID, then commit the fix.

## Task 2 — `bd-3c55`: Add immediate project runtime shutdown mode

**Depends on:** `bd-1u6v`
**Files:** `crates/spur-core/src/orchestrator/loop_runtime.rs`

1. Pre-solve the two-mode workflow: graceful supervisor shutdown retains existing drain semantics; immediate shutdown fences first, then joins delegation, worker MCP, and root MCP aborts before releasing leadership.
2. Add a test double whose graceful shutdown blocks and whose immediate shutdown acknowledges separately. Assert the supervisor's immediate API chooses only the immediate branch and completes without releasing the graceful blocker.
3. Run the focused test RED:
   `scripts/spur-cargo test -p spur-core orchestrator::loop_runtime::tests::immediate_supervisor_shutdown_skips_graceful_runtime_drain -- --exact`
4. Commit the RED test.
5. Add an explicit immediate method to the runtime trait/supervisor and implement it for `RunningProjectLoopRuntime`. Reuse Task 1's worker primitive, abort delegation, and join worker/root drains concurrently. Preserve restart and ordinary `shutdown()` behavior.
6. Run the focused test plus `scripts/spur-cargo test -p spur-core orchestrator::loop_runtime::tests`.
7. Post-solve the landed mode split and parallel resource ordering; record evidence and commit the fix.

## Task 3 — `bd-3mkg`: Force-stop active brain and MCP trees on process exit

**Depends on:** `bd-1u6v`
**Files:** `crates/spur-core/src/orchestrator/session.rs` and focused session shutdown tests

1. Pre-solve the active-brain fast path, requiring ownership removal and `BrainRetired` emission before transport drop/resource abort, with worker/root drains independent and joined before return.
2. Add a fake retirable MCP server whose graceful shutdown remains pending and whose force-abort waiter records acknowledgement. Test that the immediate helper never polls the graceful future. Extend the active-brain test connection with a Drop probe and assert transport release occurs before the resource barrier returns.
3. Run focused tests RED:
   `scripts/spur-cargo test -p spur-core shutdown_mcp_server_immediately`
   `scripts/spur-cargo test -p spur-core shutdown_active_brain_emits_brain_retired_shutdown -- --exact`
4. Commit the RED tests.
5. Keep `retire_active_brain` and `shutdown_mcp_server` unchanged. Rework only `shutdown_active_brain` to emit/fence synchronously, abort pump/delegation work, drop transport ownership, and concurrently await immediate worker/root MCP teardown. An unused pre-connected transport is dropped rather than gracefully drained.
6. Run the focused session tests and existing bounded graceful MCP-shutdown tests.
7. Post-solve the landed ordering, record evidence, and commit the fix.

## Task 4 — `bd-1lcd`: Wire one bounded interactive shutdown barrier

**Depends on:** `bd-3c55`, `bd-3mkg`
**Files:** `crates/spur-core/src/orchestrator/interactive_loop.rs`, `crates/spur-interactive/src/host.rs`, and focused integration tests

1. Pre-solve schedule makespan for concurrent active-brain/preconnection and project-runtime shutdown followed by one host deadline; reject models containing subsystem grace waits or a second host window.
2. Add/strengthen a host regression test with an outstanding sender and a non-terminating orchestrator handle. It must show the host aborts and acknowledges the task within the single configured bound, with no 30-second phase.
3. Run the focused test RED:
   `scripts/spur-cargo test -p spur-interactive --test host_api shutdown_completes_promptly_even_with_outstanding_continuation_sender -- --exact`
4. Commit the RED test.
5. In interactive cleanup, join immediate brain/preconnection and project-runtime branches concurrently. In the host, retain cancellation-first fencing, use one emergency timeout, abort the orchestrator on expiry, and await the aborted handle before returning.
6. Run focused tests, then:
   `scripts/spur-cargo test -p spur-interactive`
   `scripts/spur-cargo test -p spur-core orchestrator::loop_runtime::tests`
7. Post-solve the final schedule and safety workflow; record evidence and commit the fix.

## Review remediation — `bd-2opm`: Harden runtime shutdown races

Independent review found that cancellation could drop a partially-started project runtime and that a force request arriving after graceful drain selection could no longer preempt the grace window.

- RED: `54f9574ae` added startup-cleanup, late-escalation, and worker-handler escalation tests.
- GREEN: `4f8e16ab0` made runtime startup cancellation-aware, represented shutdown as one force-aware drain, fanned out delegation/worker/root teardown, and retained leadership through every acknowledgement.
- Verification: all 15 loop-runtime tests and all 3 worker shutdown tests passed.
- Solver evidence: pre-solve `sol_71d9fbbdfcbe4272`; post-solve `sol_d8427d8a21074937` passed all eight workflow bindings at horizon 3.

## Review remediation — `bd-2hsw`: Fence Ctrl+C ingress and prove ordering

Independent review also found that an already-cancelled interactive loop could win an unbiased ready-input branch and dispatch queued work, while the active-brain test did not prove that transport ownership dropped before the held cleanup barrier.

- RED: `650dbfc6d` added cancellation-biased admission contracts and a held-barrier transport ordering test.
- GREEN: `ea00de6b9` fenced overflow/scheduler/spawn/prompt admission, biased every shutdown select, placed `PromptDispatched` inside the admitted prompt future, and factored transport drop before the joined delegation/MCP barrier.
- Verification: both ingress tests, the held-barrier test, and the existing active-brain retirement test passed.
- Solver evidence: pre-solve `sol_8f45bd7f20a941c5`; post-solve `sol_b1cd7e65b64847a1` passed all eight workflow bindings at horizon 4.

## Review remediation — `bd-1hgbf`: Make delegation shutdown transitive

A second independent review found that aborting the top-level delegation driver did not prove that its per-delegation `JoinSet` children had been dropped, and that a worker server inserted while the parent unwound could survive the runtime's first cache sweep.

- RED: `5540bc237` added a blocked child-drop probe and a late worker-server insertion race.
- GREEN: `c0eb27696` structurally nested delegation futures under the parent task and added a final worker-server sweep after delegation, root MCP, and worker MCP teardown join.
- Verification: all 3 focused delegation shutdown tests and all 16 loop-runtime tests passed.
- Solver evidence: pre-solve `sol_5b4a909913644cfa`; post-solve `sol_324be3598a2b4e1a` passed the transitive shutdown workflow.

## Review remediation — `bd-30h0e`: Make HTTP connection shutdown transitive

The same review found that aborting Axum's outer `serve` future can detach already accepted connection tasks, allowing a partial HTTP request to survive the worker MCP shutdown acknowledgement.

- RED: `90bd5e713` added a raw partial-HTTP connection that remained open after the old immediate shutdown path.
- GREEN: `3e50eebab` introduced a cancellation-aware Axum listener/stream wrapper, tracked every accepted socket, rejected late accepts, and waited for connection guards before the final handler barrier.
- Verification: all 4 worker MCP immediate/graceful shutdown tests passed, including closure of the accepted partial connection.
- Integration evidence: the adapter was checked against the pinned Axum 0.8.9 `Listener` and `serve` implementation; Axum spawns each accepted connection, so the new SPUR-owned connection tracker supplies the missing transitive barrier.
- Solver evidence: pre-solve `sol_d874dbb17d994229`; post-solve `sol_040ce41f8fbc42a0` passed the connection-barrier workflow.

## Review remediation — `bd-bcyln`: Race brain startup against shutdown

The review also found that warm-connect and lazy first-turn startup awaited agent bootstrap directly, so Ctrl+C could remain blocked behind a hung connect/session future even though ordinary prompt work was fenced.

- RED: `2c4937779` added a polled pending-startup probe whose RAII drop must happen before shutdown acknowledgment.
- GREEN: `21ccb04b5` applies shutdown-priority selection to warm-connect, brain switch, new session, list/resume connection, session load, and lazy first-turn startup.
- Verification: all 3 shutdown-ingress tests plus 4 session milestone, correlation, and history-owner integration tests passed.
- Solver evidence: pre-solve `sol_03e03b5fa2a84034`; post-solve `sol_7345b6ed9eb7432f` passed initial-state, transition, safety, and bounded-reachability rules at horizon 5.

## Review remediation — `bd-itc99`: Preserve brain ownership across reconnect and history

Final review found that automatic reconnect still owned the dead/replacement `BrainSession` inside uncancellable futures, and explicit resume kept its loaded session local while waiting for history. Either path could detach the plain delegation handle when the host emergency abort fired.

- RED: `5412f7c3f` added pre-cancelled reconnect ownership and pending-history cancellation tests.
- GREEN: `d17090b61` makes reconnect take the caller's `brain` slot, immediately and transitively retires the dead session, races only bootstrap-local ownership against shutdown, installs the replacement before history, and gives resume history waits shutdown priority. Rare startup futures are boxed to keep the interactive-loop frame bounded.
- Verification: all 22 shutdown-filtered `spur-core` tests passed. Strict no-deps clippy reports only four known pre-existing findings and no new large-future warnings.
- Solver evidence: pre-solve `sol_f77731ae250a4d2a`; post-solve `sol_632cc311148d44c3` passed initial-state, transition, safety, and bounded-reachability rules at horizon 8, excluding `DetachedBrain` and `HistoryWaitAfterShutdown`.

## Review remediation — `bd-1n8gz`: Drop cancellation registrations structurally

Final review also found that dropping a nested delegation skipped its async registry-removal tail, leaving one stale cancellation token per interrupted request even though no task or process remained alive.

- RED: `ff5df26b8` strengthened the parent/child drop-acknowledgement test to require `CancelOutcome::NotFound` after the parent joins; the old path returned `Cancelled`.
- GREEN: `74b0f9fab` gives each structural child a cancellation-registration RAII guard and provides a synchronous, Drop-safe registry removal primitive. Typed abort-reason work remains async and no synchronous lock is held across an await.
- Verification: the exact structural-abort regression passed, as did all 3 cancellation-control behavior tests.
- Solver evidence: pre-solve `sol_30ce22f9a02c4a0c`; post-solve `sol_f8fc56c7e464409c` passed the registration-drop-before-parent-ack workflow at horizon 5, excluding `ParentAckWithRegistration`.

## Review remediation — `bd-otdln`: Join partially published brain startup

Review found that cancellation could drop startup after it had acquired root MCP and guard ownership but before the completed `BrainSession` reached the caller.

- RED: `51fe029c9` added a partial-startup barrier that requires transport drop and root/guard acknowledgement before cancellation returns.
- GREEN: `b7b645ce9` carries cancellation as `BrainStartupOutcome`, keeps resource-owning startup out of outer `select!` drops, reconciles the peer mailbox before publication, and leaves only a synchronous no-fail publication tail.
- Verification: the session ownership module passed 26 tests with 1 fixture-dependent test ignored at that point.
- Solver evidence: pre-solve `sol_056b61e6fbbb4a9b`; post-solve `sol_54b82c4c42834876` proved transport release precedes root/guard acknowledgement and caller return.

## Review remediation — `bd-1behx`: Retain retirement ownership through force

Review found that late force escalation could drop shutdown futures after they had taken root, notification-pump, or transport ownership, and timeout paths could still return a stale reusable transport.

- RED: `f250534ec` and `41a30b37c` added late-escalation and taken-handle acknowledgement regressions.
- GREEN: `83127ad40` pins one retirement future across escalation; aborts and joins notification, delegation, root, guard, and worker resources; drops transport on every non-graceful outcome; and performs a final worker-server sweep after the delegation parent joins.
- Verification: all 27 active session ownership tests passed with 1 ignored; all 3 force-aware shutdown tests and all 5 legacy retirement tests passed.
- Solver evidence: repair pre-solve `sol_0355997b8bde4264`; post-solve `sol_fd977154215a4078` found the unsafe ownership counterexample UNSAT.

## Review remediation — `bd-30unc`: Fence interactive waits and own root HTTP children

The final review found three emergency-host gaps: `BrainSession` dropped a detachable delegation handle, immediate/reconnect paths aborted notification pumps without joining them, and the root MCP server inherited Axum's detached accepted-connection tasks.

- RED: `1f0ad49ff` requires delegation-parent abort on `BrainSession` drop; `98af4e1e4` requires force shutdown to join an admitted partial HTTP connection.
- GREEN: `20f95b9f5` makes delegation and root owners abort-on-drop, fences admitted interactive RPC/prompt/terminal/pump waits, joins notification pumps in active and reconnect teardown, and replaces generic Axum serving with SPUR-owned per-connection tasks plus a force token and transitive tracker barrier. The root stop callback also joins its server-specific signal watcher.
- Verification: 28 session ownership tests passed with 1 ignored; the 26-test shutdown unit slice passed; both streamable-HTTP transport tests passed; all integration targets compiled in the broad filtered run and the pending-collector shutdown integration passed. The run later encountered a reproducible unrelated beads fixture failure before its shutdown assertion (`execute_epic` reported that the generated epic had no children).
- Solver evidence: pre-solve `sol_99132f5d82cb4ea5`; post-solve `sol_a746d2d953c94993` found the unsafe early-exit/detached-child counterexample UNSAT in 27 ms.

## Final verification

1. `scripts/spur-cargo fmt --all -- --check`
2. `scripts/spur-cargo test -p spur-core`
3. `scripts/spur-cargo test -p spur-interactive`
4. `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-core -p spur-interactive -- -D warnings`
5. Inspect the exact diff, confirm no unrelated files are staged, and run a final solver verification of `IMMEDIATE-SHUTDOWN` against the implemented lifecycle.
6. Apply the repository review gate, close verified child beads, then close `bd-ihy9` only when all acceptance criteria hold.
