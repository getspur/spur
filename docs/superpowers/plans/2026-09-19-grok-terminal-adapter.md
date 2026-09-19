# Grok terminal adapter refactor

Issue: bd-28jrx. Spec: ../specs/2026-09-19-grok-terminal-adapter.md.

1. Record the current boundary and its finite ownership-rule failure. Retain
   the previously passing script/lifecycle tests as the behavior baseline.
2. Move packed-command parsing and diagnostics into a Grok adapter, exposed
   through agent-neutral adapter dispatch. Move parser tests and argv goldens.
3. Exercise every non-Grok dispatch identity and add Generic-agent ACP lifecycle
   coverage using split requests, preserving Grok packed coverage.
4. Format, run the full remote spur-acp suite and scoped strict Clippy, and check
   the changed Python peer. Re-verify ownership against the implemented files.
5. Review the diff independently, record evidence and limitations, then commit
   the refactor on top of cb3c16993 and close the tracked issue.

This is a behavior-preserving refactor of a tested fix, not a new lifecycle
algorithm. Do not change the terminal reader or its shared completion ownership.
