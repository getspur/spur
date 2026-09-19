# Golden evaluation for Grok terminal compatibility

Issue: bd-1mp6c. Follow-up to bd-3enge.

## Contract

For recognized packed Grok Bash commands, normalization must reproduce the
explicit program and argument bytes. Execution must match independent golden
stdout/stderr/exit expectations and the equivalent split request. Existing
argv, other agents, other interpreters, and malformed wrappers must remain
unchanged. Python, JavaScript, Ruby, Perl, and shell syntax belong inside the
script payload; do not expand the workaround to parse those languages.

Use a static JSON corpus with IDs, requirements, script files, explicit argv,
literal packed requests, and independent expected output and exit status.
Include real Grok double-quoted wrapping, POSIX single quotes, apostrophes,
backslashes, Unicode, heredocs, pipelines, redirection, stderr, nonzero exits,
stdin EOF, environment values, executable/script paths with spaces, and
multi-line scripts. Use Bash `-c` for hermetic execution; parse-test `-lc`
without loading the user's startup profile.

## Implementation order

1. Add the corpus and a Rust test module exercising the production normalizer.
2. Replay canonical and packed terminal requests through NativeAcpConnection
   using a deterministic Python ACP peer. Exercise create/wait/output/release
   plus a second prompt in the same session. Missing optional interpreters are
   explicit skips; Python and shell are required.
3. Add an inherited-output-pipe diagnostic with a child held by an explicit
   release sentinel. Record command exit and terminal completion separately.
   A detected delay is a known lifecycle finding, not a successful golden
   expectation or proof of the user's original stall.
4. Run crate tests through scripts/spur-cargo, retain a machine-readable report
   and document executed/skipped cases and limits. No billed agent calls.
5. Verify bounded lifecycle observations with the workflow rule catalog.

The deliverable is an evaluation baseline. Do not silently broaden shell
parsing or change terminal lifecycle semantics merely to make the baseline
green. Any confirmed production fix needs its own failing regression and
explicitly documented behavior.
