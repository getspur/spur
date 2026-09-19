# Grok terminal script golden evaluation

Issue: bd-1mp6c. Existing lifecycle defect: **bd-3stsw**.

The golden corpus checks the existing Grok compatibility workaround against
literal script arguments and independently specified execution results. The
Rust normalizer preserved every tested script. The evaluation found and fixed
a quoting mismatch in the Python probe, and reproduced a separate native
terminal wait delay when a descendant keeps stdout/stderr open.

## Corpus and oracle

The shared [JSON corpus](../../../crates/spur-acp/tests/fixtures/grok_terminal_golden.json)
contains 23 execution cases, five additional lexical cases, and 12 requests
that must bypass normalization. Each execution case includes canonical
`command`/`args`, literal packed single- and double-quoted wrappers, fixture
files, environment values, and expected stdout/stderr/exit code.

| Area | Cases |
| --- | --- |
| Bash/sh | Pipelines, `pipefail`, last-command status, apostrophes, literal metacharacters, multiline scripts, redirection, Unicode/data paths with spaces, environment, script file |
| Python | Inline code, quoted heredoc, script path/argv with spaces, module, pipeline stdin, stdin EOF, stderr/nonzero exit, exception |
| Other interpreters | Node inline/file, Ruby inline, Perl inline |
| Lexical boundaries | Escaped newline inside/outside double quotes, preserved single-quote backslashes, intentionally escaped dollars/backticks, nonspecial double-quote escapes |
| No normalization | Already split Bash/Python/Node, other agent, existing args, other shell, packed Python, unsupported flags, malformed/extra words, executable suffix, arbitrary command string |

Three independent checks use these fixtures:

1. Rust unit tests call the production normalizer and compare exact program and
   argument bytes. Both `-c` and `-lc` wrappers are checked: **102 normalized
   requests plus 12 passthrough requests**.
2. A deterministic ACP peer drives the real `NativeAcpConnection` callbacks:
   `terminal/create`, `wait_for_exit`, `output`, and `release`. A direct
   subprocess run first validates each explicit golden expectation, then the
   canonical request and both packed variants must match it. A second prompt
   executes another terminal command in the same session.
3. The Python capability probe replays both packed forms against the same
   expected values and checks exact lexical results against canonical argv.

The native replay checks actual Rust process creation and lifecycle handling;
the peer supplies fixed ACP requests without calling a billed model. Shell
execution uses `-c` and an empty `BASH_ENV`; `-lc` is checked lexically without
running user login profiles. Python and shell are required; unavailable
optional interpreters produce explicit skips.

## Results

| Check | Observed result |
| --- | --- |
| Rust byte/boundary tests, remote Linux aarch64 | 2 tests passed; 102 recognized requests and 12 passthrough requests |
| Native ACP execution | 22 cases passed across 66 terminal executions; same-session follow-up passed |
| Remote optional runtime | Ruby unavailable: `ruby_inline` explicitly skipped |
| Python probe, local macOS | All 23 cases passed in both wrappers; 51 lexical requests passed; full related suite: 116 tests passed |
| Native inherited-pipe diagnostic | **Fail for both split and packed commands**: terminal wait delayed after command exit |

Remote execution used Bash 5.2.15, Python 3.11.2, Node 24.21.0, and Perl
5.36.0. Ruby was exercised through the local Python probe, but has no native
Rust execution result from this builder. Exact runtime paths, versions,
per-case output, corpus hash, and lifecycle observations are retained in
[the machine-readable results](2026-09-19-grok-terminal-golden-results.json).

### Probe quoting defect fixed

Python `shlex.split` and Rust `shell_words::split` differed for backslashes
before dollars/backticks inside double quotes and escaped newlines. The probe
could therefore modify a script that SPUR's Rust normalizer preserved. Before
the correction, four execution goldens failed: literal metacharacters,
multiline shell, environment expansion, and Python heredoc.

The probe now adjusts those lexical differences before splitting. Tests also
cover intentionally preserved escapes, single quotes, and escaped newlines.
The existing narrow Grok-only `/bin/bash -c|-lc` normalization boundary is
unchanged. The failing execution tests were committed first as `4377a968b`.

### Native inherited-pipe defect remains open

The diagnostic runs Python through Bash. The parent starts a background Python
child inheriting stdout/stderr, then exits zero. The child reports readiness
and keeps its pipes open until the peer creates a release sentinel. `ps`
confirms the parent is absent or a zombie before the wait observation starts.
The test gives `terminal/wait_for_exit` 250 ms to respond before releasing the
descendant; a ten-second child deadline also bounds cleanup after interruption.

The final comparison observed the same result with **both split argv and the
packed wrapper**: parent exit, no wait response before release, then exit
zero. The follow-up prompt also succeeded after both cases. Split argv bypasses
the workaround entirely, so the workaround is not required to reproduce this
delay. Per-form observations are retained in the results artifact. The
combined verification ran the normal and ignored tests: one passed, one
failed, process exit 101.

This agrees with `terminal_reader` in
[`native.rs`](../../../crates/spur-acp/src/connection/native.rs): it drains
both output pipes before awaiting the command and publishing exit status.
The 250 ms observation demonstrates a delay under controlled pipe ownership;
it is not proof of an infinite hang or attribution of the user's original
Grok session failure. The continuation result applies after child release.

The regression remains explicitly ignored in the normal suite under
**bd-3stsw** and fails when requested. A green ordinary run therefore does
**not** mean terminal lifecycle handling is fully correct. A production fix
must define late-output and cleanup behavior and make this regression pass.

## Reproduce

```bash
python3 -m unittest scripts.test_probe_acp_terminals scripts.test_probe_acp_capabilities scripts.test_probe_acp_subagents -q
SPUR_NO_LOCAL_FALLBACK=1 scripts/spur-cargo test -p spur-acp --lib grok_golden -- --nocapture
SPUR_NO_LOCAL_FALLBACK=1 scripts/spur-cargo test -p spur-acp --test grok_terminal_golden -- --nocapture
```

Run the known failing diagnostic explicitly (current expected process exit
101, preserving its failure rather than treating it as a passing golden):

```bash
SPUR_NO_LOCAL_FALLBACK=1 scripts/spur-cargo test -p spur-acp --test grok_terminal_golden grok_golden_inherited_pipe_must_not_delay_command_exit -- --ignored --nocapture
```

Each native run prints `GROK_GOLDEN_REPORT=<json>`. To collect both the normal
results and the failing diagnostic together, use `--include-ignored` instead
of `--ignored`; the combined run also exits 101 while this defect remains.

## Solver scope and remaining gaps

The workflow catalog verifies caller-transcribed finite traces. Desired exit
reporting before release passes (`sol_0ada6a3619d74963`); the observed pending
wait violates that bounded target (`sol_b84005435f7845ee`, `unsat`/`fail`);
continuation after release passes (`sol_c3d76f622fc1439b`).
[Requests and raw solver results](2026-09-19-grok-terminal-golden-solver.json)
retain the bounds and event mapping. This checks the stated observations, not
all possible executions of Rust or unbounded liveness. The observed trace is
the same for the split control and packed request.

This baseline does not cover PowerShell/Windows, interactive TTY programs,
arbitrary interpreter versions, executable paths with spaces, mixed-stream
ordering under concurrency, or every signal/cancellation race. The exception
case checks its stderr suffix and compares the complete traceback with the
same interpreter's canonical execution. This replay makes no new claim about
which commands a particular Grok model/version will generate; the prior
[live Grok evaluation](2026-09-18-grok-terminal-continuation.md) covers that
separate question.
