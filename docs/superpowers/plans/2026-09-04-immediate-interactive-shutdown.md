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

## Final verification

1. `scripts/spur-cargo fmt --all -- --check`
2. `scripts/spur-cargo test -p spur-core`
3. `scripts/spur-cargo test -p spur-interactive`
4. `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-core -p spur-interactive -- -D warnings`
5. Inspect the exact diff, confirm no unrelated files are staged, and run a final solver verification of `IMMEDIATE-SHUTDOWN` against the implemented lifecycle.
6. Apply the repository review gate, close verified child beads, then close `bd-ihy9` only when all acceptance criteria hold.
