# ACP probe merge resolution

Issue: bd-xsxu9. Merge 23e58cdd2 into 1c1d4643f.

The two branches independently implemented terminal callbacks. Retain one host:
the local TerminalHost with explicit off/strict/grok modes, session ownership,
corrected quoting, process-group cleanup and same-session continuation evidence.
Keep upstream asynchronous server-request dispatch, filesystem callbacks, live
configuration refresh, Pi standardization, and TUI capability refresh.

The combined host must serialize creation/lookup/closure when callbacks run on
executor threads. Teardown must prevent late creation and finish handlers before
closing their transport/log. Keep terminal waits asynchronous so they cannot
exhaust the callback worker pool. Correct filesystem line slicing and advertise
only implemented filesystem methods. Preserve all existing goldens and upstream
configuration-refresh assertions; port registry tests to the retained host.

1. Add merge regressions for filesystem slicing, terminal mode boundaries,
   asynchronous callbacks outside a client request, and closure. Record failures
   against the appropriate pre-resolution source. Git's active merge prevents
   committing only a failing test; retain red-run evidence before the merge commit.
2. Resolve the five conflicts, remove the duplicate registry/fallback, integrate
   callback lifetime and capabilities, and run both Python suites plus related
   probe tests and lint.
3. Run the full remote spur-acp suite and affected TUI config-option integration
   tests using scripts/spur-cargo. Check formatting and scoped strict Clippy.
4. Independently review the combined diff, record verification, and finish the
   existing merge without staging the user's .cargo/config.toml or untracked work.

Scope: deterministic protocol peers and real subprocesses; this does not certify
every live agent/version or rebuild/restart the user's running SPUR application.
