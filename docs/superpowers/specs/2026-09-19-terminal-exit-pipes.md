# Terminal exit and inherited output pipes

Issue: bd-3stsw. User authorized fixing the native ACP stall exposed by the
Grok terminal goldens. The failure also occurs with split argv.

## Required behavior

Observe the command's exit concurrently with stdout/stderr collection. Once
the command exits, drain ordinary final output until both streams close or a
50 ms grace expires, whichever comes first, then publish the exit status.
The grace is a single latency policy, not a proof that all descendants have
finished; it sits below the existing 250 ms held-pipe regression window.
Scheduler delays remain outside that policy guarantee.

Continue collecting descendant output after publishing status while the
terminal remains allocated. This preserves the output snapshot for ordinary
commands and allows late output without withholding command completion.
Exit status is published once and never changed by later kill/release.

The terminal reader owns the Child and process cleanup. Kill requests signal
the isolated terminal process group on Unix, preserving output and terminal
allocation. Release and connection shutdown signal cleanup, reap the command
if needed, close output readers, and remove terminal state. Cleanup does not
wait for pipe EOF. Do not signal a group once its reader has completed.
Explicit release is the boundary after which further output is discarded.

Keep existing Grok normalization and other ACP behavior unchanged. Windows
retains direct-child termination; Unix detached process groups are the tested
descendant-cleanup boundary. Descendants that intentionally escape the group
are outside this change's process-tree guarantee.

## Evidence

Enable the existing split/packed regression and verify parent output, late
descendant output, and same-session continuation. Exercise kill/release both
while the parent runs and after parent exit with inherited pipes; exercise
connection shutdown. Preserve nonzero exits and buffered/truncated output.
Run remote spur-acp tests through scripts/spur-cargo and model the observed
bounded traces using workflow catalog rules. Solver checks caller-supplied
traces; runtime tests establish implementation evidence.
