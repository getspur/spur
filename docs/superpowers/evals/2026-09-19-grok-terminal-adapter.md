# Grok terminal compatibility moved into the adapter

Issue: bd-28jrx. Follow-up to cb3c16993; history is preserved.

The user's boundary concern was correct for packed-command compatibility.
`adapter/grok.rs` now owns the narrow Bash parser, Unix gate, and compatibility
diagnostic. `adapter::normalize_terminal_command` dispatches by agent and returns
replacement argv only when needed. The native terminal handler consumes that
result without knowing Grok's command shape or diagnostic policy.

The previous pipe fix remains shared. Source comparison and independent review
confirmed terminal state/completion ownership, process isolation, output/exit
handling, kill/release, and shutdown are byte-for-byte unchanged. Generic-agent
integration runs now demonstrate the lifecycle behavior independently of the
Grok adapter. Older model/notification compatibility in native.rs is outside this
terminal-focused refactor.

## Runtime verification

The full remote Linux crate run exited zero: **626 passed, zero failed, one
pre-existing ignored test**, across 36 suite summaries. Parser unit tests moved
with the adapter; their acceptance predicates are unchanged. The existing
non-Grok control was expanded to cover every other declared agent identity.

- 102 exact Grok argv checks, including `-c` and `-lc`.
- 459 packed-command passthrough checks across nine non-Grok agent kinds.
- 12 unchanged negative/passthrough corpus cases.
- 22 script cases in three command forms: 66 successful native executions.
  The optional Ruby case was explicitly skipped because the builder lacks Ruby.
- Grok and Generic both pass inherited-pipe exit, descendant kill/release,
  shutdown cleanup after published exit, final-output retention, truncation,
  and nonzero exit-status checks. Grok covers split and packed inherited-pipe
  requests; Generic covers protocol-correct split argv.
- All nine integration replays complete a follow-up terminal prompt in the
  same session.

```bash
SPUR_NO_LOCAL_FALLBACK=1 scripts/spur-cargo test -p spur-acp -- --nocapture
SPUR_REMOTE=1 SPUR_NO_LOCAL_FALLBACK=1 scripts/spur-cargo clippy -p spur-acp --lib --test grok_terminal_golden -- -D warnings
scripts/spur-cargo fmt --all -- --check
ruff check crates/spur-acp/tests/fixtures/grok_terminal_golden_peer.py scripts/probe_acp_capabilities.py
ruff format --check crates/spur-acp/tests/fixtures/grok_terminal_golden_peer.py scripts/probe_acp_capabilities.py
```

[Machine-readable results](2026-09-19-grok-terminal-adapter-results.json) include
the unchanged corpus hash and actual peer reports. Raw local logs are in
`.spur/logs/grok-adapter-{crate,clippy,fmt}-20260919.log`. Formatting and Python
lint checks passed. Strict scoped Clippy also exited zero.
The earlier broad `--tests` Clippy failures in unchanged integration files are
documented in [the lifecycle evaluation](2026-09-19-terminal-exit-fix.md);
that broader command was not repeated for this refactor.

## Solver and review

The configuration catalog's `configuration.attribute_allowed_pair` rejects the
old supplied ownership facts (`sol_d59e83567e4c49ba`, `unsat`/`fail`) for the
Grok parser and diagnostic, while accepting the shared lifecycle owner. The
implemented assignments pass (`sol_471cf27d082e4431`, `sat`/`pass`).
[Requests and raw result envelopes](2026-09-19-grok-terminal-adapter-solver.json)
preserve the finite policy and outcomes. These checks verify declared ownership;
they do not inspect source or prove parsing equivalence or unbounded liveness.
The runtime goldens and source review supply that separate evidence.

Independent review found no blockers. This is deterministic ACP testing with
real subprocesses, not a new live Grok model evaluation. Non-Unix behavior keeps
the existing no-op parser gate and was not exercised by the Unix suite. The
installed SPUR binary has not been rebuilt or restarted by this refactor.
