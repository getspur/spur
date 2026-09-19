# ACP probe merge verification

Issue: bd-xsxu9. Merge parents: local 1c1d4643f and origin/main 23e58cdd2.

The sole conflicted file, scripts/probe_acp_capabilities.py, contained five
conflict blocks between independently added terminal hosts. Resolution keeps
TerminalHost and its off/strict/grok modes, corrected golden lexer, session
ownership checks, and continuation/error reporting. It incorporates upstream
asynchronous callbacks, filesystem support, configuration refresh, Pi event
standardization, and TUI capability refresh. The duplicate registry and its
generic shell fallback are removed; its signal/exit tests target the retained
host. Filesystem read/write capabilities now match implemented callbacks.

## Corrections verified during integration

- Filesystem slicing uses splitlines instead of the nonexistent str.lines.
- Config-set probing uses a notification snapshot to refresh dependent choices
  while retaining those updates and other notifications for the report.
- Terminal creation/release/closure is serialized, with a closing gate for late
  callbacks. Transport sends happen outside the terminal ownership lock.
- Process exit is observed separately from output EOF, with the native host's
  50 ms final-output grace. Held-pipe descendants can still be killed/released
  after the parent exits. Fully completed terminals do not signal stale PGIDs.
- Probe request/cancel sends are bounded. Closing stdin runs separately from
  the process deadline so a blocked writer cannot prevent termination. Teardown
  freezes terminal creation, stops transport/callback workers, then joins terminal
  cleanup. Active callback workers finish before the protocol log closes.
- Futures timeouts use the explicit concurrent.futures exception type for
  compatibility with Python versions where it is not the built-in alias.

Regression tests failed against the relevant parent or intermediate source
before the fixes. The active Git merge prevented committing a partial failing
test separately. Red-run logs are `.spur/logs/acp-merge-{red,review-red,writer-red,config-red}-20260919.log`.

## Verification

The combined Python capability, terminal, merge, and subagent suites pass:
**131 tests**, including actual shell/interpreter goldens and a full CLI test
whose agent stops reading a 4 MiB terminal response. That probe exits with the
expected timeout/failure code instead of hanging.

The full remote spur-acp suite passes: **632 tests, zero failures, one existing
ignored test**. Grok and Generic lifecycle/continuation tests and the incoming Pi
standardizer/capability snapshot tests are included. Optional interpreter cases in
the script corpus may skip when an interpreter is unavailable.

Both remote TUI config-option integration tests pass. The build emits five
dead-code warnings from unchanged spur-graph code. Scoped strict Clippy also passes with warnings denied. Rust formatting and Python Ruff checks pass. Independent review
identified and rechecked the process-group and blocked-writer corrections.

```bash
python3 -m unittest scripts.test_probe_acp_capabilities scripts.test_probe_acp_terminals scripts.test_probe_acp_merge scripts.test_probe_acp_subagents
SPUR_NO_LOCAL_FALLBACK=1 scripts/spur-cargo test -p spur-acp
SPUR_NO_LOCAL_FALLBACK=1 scripts/spur-cargo test -p spur-tui --test config_option_update_arm
SPUR_REMOTE=1 SPUR_NO_LOCAL_FALLBACK=1 scripts/spur-cargo clippy -p spur-acp --lib --test grok_terminal_golden -- -D warnings
scripts/spur-cargo fmt --all -- --check
```

[Machine-readable results](2026-09-19-acp-probe-merge-results.json) retain the
commands, counts, red-run references and review outcome. Raw logs are under
`.spur/logs/acp-merge-*-20260919.log`. These are deterministic
protocol peers and real subprocess tests, not a new live model evaluation or a
full-workspace test run. Process groups deliberately escaped by descendants,
non-Unix execution, and every supported Python version are not covered by these
runs. This task does not rebuild/restart the installed SPUR application.
