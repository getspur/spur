# Grok ACP shell errors and session continuation

Date: 2026-09-18. Issue: bd-3enge.

## Result

Reproduced a terminal spawn error with **Grok 1.0.5 (5115b46bc909)**, model
`grok-4.6`, using strict executable/argument handling. Grok still sends the
entire `/bin/bash -lc "…"` invocation as `terminal/create.params.command`,
without a separate `args` array. Directly spawning that string produces
`ENOENT`. SPUR already has a narrowly scoped workaround in
`normalize_grok_terminal_command` in `crates/spur-acp/src/connection/native.rs`.

**The reported stuck session was not reproduced.** Grok answered the next
prompt after the spawn error. With the existing SPUR argv workaround emulated
by the probe, it also continued after a command exited 7, executed a recovery
command in that same turn, and answered another prompt in the same session.
No Rust runtime change is justified by these observations alone.

The evaluation did uncover and fix two probe defects:

1. The original probe advertised filesystem and terminal callbacks but only
   implemented permission requests. It generated its own `-32601` terminal
   failures, so it could not establish shell support. Default capabilities now
   reflect implementation; real terminal execution is explicitly opt-in.
2. Prompt timeouts previously still returned CLI exit code 0. Prompt RPC errors
   and timeouts now return 2; a timeout sends cancellation and stops the prompt
   sequence. A tool failure with a completed prompt remains valid diagnostic
   evidence and does not itself make the probe fail.

## Live matrix

All requests used an isolated `/tmp/spur-grok-acp-eval` directory, no injected
SPUR MCP server, `--no-auto-update`, `--no-try-set`, and a 90-second per-prompt
deadline. No repository edits were requested from Grok.

| Case | Terminal evidence | Turn result | Same-session continuation |
|---|---|---|---|
| Original probe, shell `printf` | Two create requests; both rejected with `-32601` by the probe | `end_turn`, with an error explanation | Original probe had no follow-up facility |
| Strict ACP argv, shell `printf` | One create request; `-32603` with `ENOENT` for the packed command | `end_turn` | Next prompt returned `SPUR_AFTER_SPAWN_ERROR_OK` |
| Grok compatibility, shell `printf` | Full create → wait → output → release; exit 0, expected output | `end_turn` | Second turn accepted |
| Same compatibility session, intentional failure and recovery | Exit 7 with `SPUR_EXPECTED_FAILURE`, then a separate command exiting 0 with `SPUR_RECOVERED` | `end_turn` | Third prompt returned `SPUR_FOLLOW_UP_OK` |

The compatibility run completed three terminal lifecycles and all three
prompts. The strict run completed both prompts. Grok marked the exit-7 tool
call completed rather than failed; the command's `exitCode`, not only the tool
status or assistant text, establishes the failure.

The shell also printed a local startup warning:
`~/.bash_profile: line 10: /tmp/spur-shell-install/env: No such file or directory`.
It did not prevent exit-0 commands or subsequent prompts in these runs. The
profile was not changed.

## Probe changes

`scripts/probe_acp_capabilities.py` now provides:

- `--terminal-mode off` (default): does not advertise terminal support.
- `--terminal-mode strict`: real ACP terminal callbacks using command + args.
- `--terminal-mode grok`: the same narrow packed Bash argv policy used by SPUR;
  it does not shell-interpret arbitrary executable strings.
- `--follow-up-prompt TEXT`: repeatable, billed prompts in the original session.
- `prompt_results`: per-turn RPC status, assistant text, and failed tool IDs;
  the original `prompt_result` remains the first turn for compatibility.
- Asynchronous wait callbacks, bounded UTF-8 output, stdin EOF, session-scoped
  terminal handles, process-group termination, release, and final cleanup.
- Exit 0 for completed probes, 1 for hard failures, 2 for prompt RPC failure or
  timeout. Full callback/response evidence remains in the JSONL frame log.

Filesystem callbacks remain unimplemented and are no longer advertised.
Enabling terminal mode executes agent-proposed commands locally; it is not a
sandbox. The permission selection flag retains its existing meaning.

Example reproduction (the scratch directory must already exist):

```bash
python3 scripts/probe_acp_capabilities.py \
  --command grok \
  --args='--no-auto-update agent --always-approve stdio' \
  --cwd /tmp/spur-grok-acp-eval \
  --terminal-mode grok --no-try-set --always-approve \
  --prompt 'Run printf "shell-ok\n", then report its result. Do not change files.' \
  --follow-up-prompt 'Reply CONTINUATION_OK without tools.' \
  --timeout 90 --label grok-terminal-continuation
```

Use `--terminal-mode strict` for the packed-command spawn-error comparison.

## Verification and solver interpretation

Regression tests use real subprocesses and a deterministic ACP peer. Coverage
includes nonzero exits, spawn errors, packed shell quoting, stdin EOF, cwd/env,
UTF-8 truncation and zero-byte output limits, overlapping wait/kill, release,
cleanup, session isolation, malformed callbacks, nullable output limits,
same-session continuation, and cancellation without overlapping timed-out
prompts. The new tests were observed failing before implementation.

Final result: **114 tests passed** across the three probe suites below; Ruff
lint, formatting, and the scoped diff whitespace check passed.

```bash
python3 -m unittest scripts.test_probe_acp_terminals \
  scripts.test_probe_acp_capabilities scripts.test_probe_acp_subagents -q
ruff check scripts/probe_acp_capabilities.py scripts/test_probe_acp_terminals.py
ruff format --check scripts/probe_acp_capabilities.py scripts/test_probe_acp_terminals.py
```

Catalog-first evaluation selected `workflow` / `bounded_trace` with
`initial_state_allowed`, `transition_allowed`, `safety_invariant`, and
`bounded_reachability`. Caller-normalized observed traces passed:

| Trace | Horizon | Raw status / outcome | Persisted result |
|---|---:|---|---|
| Strict spawn error → next prompt completed | 5 | `sat` / `pass` | `sol_db361435f11742a7` |
| Successful shell → nonzero exit → recovery → third prompt completed | 9 | `sat` / `pass` | `sol_1d8f6c91bfe84ff4` |

Pre-implementation lifecycle feasibility: `sol_b7018719638e4fe0`, `sat` / `pass`.
Inputs and flattened raw post-evaluation solver envelopes are in
[the solver artifact](2026-09-18-grok-terminal-solver.json).
These verify the declared finite traces only. They do **not** prove that every
Grok or SPUR execution eventually continues, or that the user's failing
session is repaired.

## Evidence and remaining limits

Local redacted JSONL and structured reports are retained under
`.spur/logs/grok-terminal-continuation-20260918/`, with `baseline`, `strict`, and
`compat` filenames. These are local runtime evidence, not committed fixtures.
The repository's separate Grok 1.0.13 capability fixture is not the tested
binary: both current PATH entries resolve to Grok 1.0.5. No upgrade was made.

This tests Grok over stdio with the Python probe. It does not exercise the
whole SPUR TUI, permission UI, Rust notification drain, resumption of the
reported session, or its injected MCP services. The recently modified Grok
session logs for this repository did not contain the searched terminal spawn
error strings; that absence does not identify the failing session.

To diagnose the remaining report, correlate the exact failing session and
error with its terminal callback response, prompt result (or missing result),
and subsequent prompt send. Distinguish command exit failure, callback RPC
failure, missing prompt completion, and a completed turn whose next prompt
never reaches Grok. Do not add blind prompt retries: a command might already
have executed even when the client has not seen completion.

Protocol reference: [ACP terminal lifecycle](https://agentclientprotocol.com/protocol/v1/terminals).
