# Native ACP terminal exit fix

Issue: bd-3stsw. This fixes the lifecycle defect reproduced by the
[Grok script goldens](2026-09-19-grok-terminal-golden.md). It applies to the
native ACP terminal host for every agent; Grok's argv workaround is unchanged.

## Cause and behavior

Previously, `terminal_reader` drained stdout/stderr to EOF before waiting for
the command and publishing its exit status. A background descendant could
hold a pipe open after the command exited. Kill/release/shutdown also targeted
only the parent PID, leaving those descendants alive.

The reader now observes command exit concurrently with output. It drains final
output until EOF or a 50 ms grace expires, then publishes status exactly once.
It continues collecting late output while the terminal remains allocated.
Ordinary commands whose pipes close do not incur the grace delay.

Kill/release/shutdown send controls to the task owning the Child. On Unix it
signals the isolated terminal process group and reaps the direct child when
needed. Release stops output collection without waiting for pipe EOF. Shutdown
signals every remaining reader before joining them. A shared completion handle
stays in the terminal map until release finishes, so a cancelled release
callback cannot prevent shutdown from joining its cleanup.

## Verification

The failing regressions were committed first in `c9bc6aa1d`. The remote
pre-fix run failed held-pipe exit, descendant cleanup, and shutdown tests while
the ordinary script corpus passed. The strengthened cleanup tests explicitly
wait for ACP exit publication before exercising the exited-parent path.

The focused post-fix native run passed all five tests:

- Both split argv and packed Grok commands report exit before the held child
  is released, within the regression's 250 ms observation window.
- Parent output and later descendant output remain readable; follow-up prompts
  continue in the same session.
- Kill and release stop parent/descendant processes both while the parent runs
  and after its exit has been reported. Kill retains output and preserves an
  already published exit status.
- Shutdown stops the held descendant after parent exit publication.
- 512 KiB of final output is retained, a 4096-byte limit marks truncation, and
  both executions preserve nonzero exit code 11.

The final shared-completion implementation passed the full remote crate suite:
**626 tests passed, zero failed, one pre-existing orphan-sweep test ignored**.
The inherited-pipe regression is no longer ignored. The ordinary corpus ran
22 cases in three command forms (66 executions); Ruby was explicitly skipped
because the remote builder lacks that optional interpreter.

```bash
SPUR_NO_LOCAL_FALLBACK=1 scripts/spur-cargo test -p spur-acp
SPUR_NO_LOCAL_FALLBACK=1 scripts/spur-cargo test -p spur-acp --test grok_terminal_golden -- --nocapture
SPUR_REMOTE=1 SPUR_NO_LOCAL_FALLBACK=1 scripts/spur-cargo clippy -p spur-acp --lib --test grok_terminal_golden -- -D warnings
```

[Machine-readable results](2026-09-19-terminal-exit-fix-results.json) retain
pre/post observations and the full-suite summary. Raw logs are under
`.spur/logs/terminal-exit-fix-*-20260919.log`. Rust formatting and Python
fixture lint/format checks passed. Independent review found no remaining
blocking issues after the release/disconnect ownership correction.
Strict Clippy passed for the library and changed integration test.

The broader `clippy -p spur-acp --tests -- -D warnings` run still exits 101
because of pre-existing lint failures in `agent_model_catalog.rs`,
`native_trailing_notification.rs`, `process_kill_on_drop.rs`, and
`skip_permissions_config.rs`. Those integration-test files were not changed.
The scoped command above checks the library and changed integration test.
The patch removes the now-unfulfilled shutdown-loop lint expectation and
updates five equivalent borrow patterns in the touched native source file.

## Solver evidence and limits

Workflow catalog verification rejected the old bounded exit trace
(`sol_dbe145bc10324a23`, `unsat`/`fail`) and accepted the implemented split and
packed observations (`sol_e9e3aed36d5449ce`, `sol_0855fd9851bb4c82`,
`sat`/`pass`). Cleanup after published exit also passed
(`sol_62bed5a7162a4c6d`).
[Full requests and solver envelopes](2026-09-19-terminal-exit-fix-solver.json)
retain the finite horizons and mapping. These checks verify supplied traces;
runtime tests establish the behavior. Neither proves unbounded liveness or
eliminates OS scheduling delays.

Unix descendants retaining the terminal pipes within its process group are
the reproduced and tested case. Processes deliberately escaping that group
are outside the cleanup guarantee; Windows retains direct-child termination
and was not exercised by these Unix tests. Explicit release ends output
retention. No live model calls were used for this fix, and the original user
session's trace has not been matched to the reproduction. An already running
SPUR binary must be rebuilt/restarted to use the source change.
