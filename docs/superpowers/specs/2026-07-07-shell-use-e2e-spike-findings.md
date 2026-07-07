# shell-use E2E Spike Findings

Date: 2026-07-07
Scope: `scripts/e2e/shell-use/**`
Recommendation: **No-Go for adopting `shell-use` as SPUR's TUI E2E layer today.**

## Summary

`microsoft/shell-use` 0.0.1-beta.3 is scriptable and has the right high-level
shape for SPUR TUI tests: a daemon-backed out-of-process terminal session,
bounded `wait` primitives, text assertions, key input, terminal diagnostics, and
stable exit-code classes. The pinned installer in `scripts/e2e/shell-use/`
downloads the GitHub release archive into `.spur/tmp/`, verifies the release
SHA-256, and does not commit binaries.

The real SPUR TUI journeys could not be validated in this local macOS worktree:
the required `SPUR_REMOTE=0 scripts/spur-cargo build -p spur-cli` was killed
with exit 137 during final linking on two focused attempts, and no
`target/debug/spur` binary was produced. Under the spike time-box, this blocks
the real-binary portion. The committed real journey runner therefore fails fast
with an actionable missing-binary message.

To keep the framework assessment useful, a committed stand-in full-screen TUI
probe (`scripts/e2e/shell-use/standin-less.sh`) drove `less` out-of-process via
`open` + `submit`, used only shell-use wait/assert/input primitives, and passed
3/3 runs with no flakes.

## Sources Checked

- Project README and command reference: https://github.com/microsoft/shell-use
- Release page: https://github.com/microsoft/shell-use/releases/tag/v0.0.1-beta.3
- Release API metadata for asset names and SHA-256 digests:
  `https://api.github.com/repos/microsoft/shell-use/releases/tags/v0.0.1-beta.3`
- npm package metadata for the JS binding:
  `npm view @microsoft/shell-use@0.0.1-beta.3`

README notes relevant to this spike:

- The project is explicitly work-in-progress, so command behavior may change
  between beta releases.
- The CLI exposes the requested surface: session lifecycle, input, inspection,
  wait, expect, snapshots, recordings, and agent metadata.
- `agent-context` prints generated JSON for the real command surface, which was
  useful for catching a flag mismatch before journey execution.

## Pinned Install Scriptability

Added `scripts/e2e/shell-use/install.sh`.

- Pins `SHELL_USE_VERSION=0.0.1-beta.3`.
- Downloads from GitHub releases, not a moving `latest` URL.
- Installs under `.spur/tmp/shell-use/0.0.1-beta.3/<target>/bin/shell-use`,
  already covered by the repo's `.gitignore`.
- Verifies SHA-256 for macOS arm64, macOS x86_64, Linux x86_64 GNU, and Linux
  arm64 GNU release assets.
- Does not commit binaries.

Observed installer output on this machine:

```text
Installing shell-use 0.0.1-beta.3 for aarch64-apple-darwin
/Volumes/Projects/spur/.spur/worktrees/be9339cb-635a-46e5-bfc9-90baaf05dcf2/.spur/tmp/shell-use/0.0.1-beta.3/aarch64-apple-darwin/bin/shell-use
```

Version check:

```text
$ .spur/tmp/shell-use/0.0.1-beta.3/aarch64-apple-darwin/bin/shell-use --version
shell-use 0.0.1-beta.3
```

## Real SPUR Binary Result

Required build command:

```text
$ SPUR_REMOTE=0 scripts/spur-cargo build -p spur-cli
...
   Compiling spur-cli v1.7.0 (.../crates/spur-cli)
<no further compiler output during final link>
process exited with code 137
```

Second focused attempt:

```text
$ SPUR_REMOTE=0 CARGO_BUILD_JOBS=1 scripts/spur-cargo build -p spur-cli
   Compiling lance-index v6.0.0
   Compiling lance v6.0.0
process exited with code 137
```

Post-build checks:

```text
$ ls -l target/debug/spur
ls: target/debug/spur: No such file or directory

$ du -sh target
7.9G    target
```

Conclusion: local build was not feasible in this sandbox, so shell-use did not
drive the real SPUR binary in this spike.

## Committed Harness Shape

Real SPUR journey files:

- `scripts/e2e/shell-use/run.sh`
- `scripts/e2e/shell-use/lib.sh`
- `scripts/e2e/shell-use/journeys/cold-launch.sh`
- `scripts/e2e/shell-use/journeys/help-overlay.sh`
- `scripts/e2e/shell-use/journeys/clean-quit.sh`

The real harness implements the requested isolation:

- temp cwd containing `.spur/`
- isolated `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`,
  `XDG_CACHE_HOME`
- pre-created `$HOME/.spur/onboarded`
- `SPUR_NO_UPGRADE_CHECK=1`
- terminal size `80x24`

The harness uses `open --shell bash` + `submit "$SPUR_BIN tui"` so that
`wait command` and `expect exit-code 0` have shell command tracking. A direct
`shell-use run less ...` probe drove the TUI and waited for exit, but
`expect exit-code 0` returned `no command exit code tracked yet`; `open` +
`submit` avoided that gap.

Current real runner output:

```text
$ SHELL_USE_RUNS=3 scripts/e2e/shell-use/run.sh
spur binary is not executable: /Volumes/Projects/spur/.spur/worktrees/be9339cb-635a-46e5-bfc9-90baaf05dcf2/target/debug/spur
Build it with: SPUR_REMOTE=0 scripts/spur-cargo build -p spur-cli
```

## Journey Results

Because the real `spur` binary was not produced, all three requested journeys
were blocked before session start.

| Journey | Run 1 | Run 2 | Run 3 |
| --- | --- | --- | --- |
| Cold launch | Blocked: no `target/debug/spur` | Blocked | Blocked |
| Help overlay | Blocked: no `target/debug/spur` | Blocked | Blocked |
| Clean quit | Blocked: no `target/debug/spur` | Blocked | Blocked |

Stand-in full-screen TUI reliability:

```text
=== stand-in run 1/3 ===
+ shell-use --session spur-shell-use-standin-less-81245 open --shell bash --cols 80 --rows 24 --cwd /var/folders/.../spur-shell-use-less.Wgsl0F
{
  "pid": 81266,
  "recording": "/Users/kevintruong/Library/Caches/shell-use/spur-shell-use-standin-less-81245.cast",
  "session": "spur-shell-use-standin-less-81245"
}
+ shell-use --session spur-shell-use-standin-less-81245 submit less\ input.txt
+ shell-use --session spur-shell-use-standin-less-81245 wait text shell-use\ stand-in\ ready --timeout 5000
+ shell-use --session spur-shell-use-standin-less-81245 expect text shell-use\ stand-in\ ready --no-strict --timeout 5000
+ shell-use --session spur-shell-use-standin-less-81245 press q
+ shell-use --session spur-shell-use-standin-less-81245 wait command --timeout 5000
+ shell-use --session spur-shell-use-standin-less-81245 expect exit-code 0
PASS stand-in run 1/3
=== stand-in run 2/3 ===
...
PASS stand-in run 2/3
=== stand-in run 3/3 ===
...
PASS stand-in run 3/3
```

Flakes observed: 0/3 for the stand-in TUI. Real SPUR flake rate is unknown
because the journeys did not start.

## Wait-Until Quality

Positive:

- `wait text "..." --timeout MS` is precise and bounded.
- `wait command --timeout MS` works well when a TUI is launched via shell
  `open` + `submit`.
- `expect text` supports timeouts and color predicates.
- The CLI's generated `agent-context` accurately exposed defaults and flags.

Gaps:

- `expect text` is strict by default and fails if text appears more than once.
  The harness uses `--no-strict` for screen-contains assertions.
- Direct `run <program>` did not expose an exit code to `expect exit-code` in
  the stand-in probe; `wait exit` worked, but command exit-code assertions
  required `open` + `submit`.
- `wait idle` exists, but the real journeys did not need it and the stand-in
  did not rely on visual-idle polling.

No sleeps were used in the committed harness or probes.

## Failure Diagnostics

Intentional failed expectation output:

```text
locator timeout: '__definitely_missing__' not found after 250ms

Terminal content:
---START---























visible diagnostic text
input.txt (END)
---END---
intentional expect status: 1
```

This is better than many terminal harnesses: shell-use includes the terminal
content in assertion failures. The committed wrapper also dumps `state` and
`text --full` when a shell-use command fails.

## Beta Stability Observed

Observed stable in this environment for:

- install and version check
- generated command metadata via `agent-context`
- shell `open`
- `submit`
- `wait text`
- `expect text --no-strict`
- `press q`
- `wait command`
- `expect exit-code 0`
- `close`

Not validated:

- real SPUR TUI behavior
- snapshot updates/comparison
- SVG screenshots
- mouse input
- long-running daemon behavior in CI

The beta warning in the README is real maintenance risk: pinning is mandatory,
and upgrades should be treated as explicit spike work.

## CI Integration Shape

If revisited, CI should:

1. Install shell-use with `scripts/e2e/shell-use/install.sh`.
2. Build or provide a local `spur` binary before running the journeys.
3. Run `SHELL_USE_RUNS=3 scripts/e2e/shell-use/run.sh` on a runner with enough
   memory/disk to link `spur-cli` locally.
4. Preserve shell-use stdout/stderr and, on failure, upload `text --full`,
   `state`, and possibly `get-recording` cast files as CI artifacts.
5. Keep shell-use binaries under ignored scratch paths; do not vendor them.

The key unresolved CI question is whether the CI host can produce the local
binary reliably. Remote `spur-cargo` builds are faster, but this spike was
specifically about a local macOS binary and terminal automation.

## Maintenance Risk Notes

- Beta release surface can change; keep exact version and checksums pinned.
- `open` + `submit` is slightly more ceremony than `run`, but currently needed
  for exit-code assertions.
- Text assertions need `--no-strict` for contains-style checks.
- The daemon model is attractive: an emulator crash should be isolated from
  the test runner process, unlike the in-process `phantom-test` failure mode.
  This was validated structurally and with the stand-in process, but not with
  real SPUR because no binary was available.
- shell-use stores asciinema recordings in the user cache by default. CI should
  either collect or clean those artifacts deliberately.

## Alternatives

### vhs Golden-Tape Approach

Pros:

- Mature CLI for terminal recordings.
- Good for documentation and visual golden tapes.

Cons:

- Less suited to detailed interactive assertions and exit-code-aware journeys.
- Golden visual tapes are more brittle for an actively changing TUI.

Verdict: better for demos/regression videos than first-line E2E behavior tests.

### Hand-Rolled `portable-pty` + `vt100` Harness

Pros:

- Full control over environment scrubbing, process lifecycle, and diagnostics.
- Avoids a beta external daemon and moving CLI surface.
- Can be built directly into Rust integration tests once local/remote build
  questions are settled.

Cons:

- More code to maintain.
- Need to implement waits, snapshots, key encoding, and diagnostics ourselves.

Verdict: still the safer SPUR-owned fallback if we need active TUI E2E coverage
before shell-use can be validated against the real binary.

## Recommendation

No-Go for adopting shell-use as the official SPUR TUI E2E framework today,
because the required real-binary validation did not run.

Conditional next step: retry this exact harness on a host that can build
`target/debug/spur` locally. If the three real journeys pass 3/3 there, shell-use
is a credible candidate and materially better than `phantom-test` on process
isolation. Until then, prefer the hand-rolled `portable-pty` + `vt100` harness
for production test work, with vhs reserved for demo-style golden recordings.
