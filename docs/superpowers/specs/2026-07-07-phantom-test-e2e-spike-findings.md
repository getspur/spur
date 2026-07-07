# phantom-test E2E Spike Findings

Date: 2026-07-07
Scope: `crates/spur-cli` only
Recommendation: **No-Go for adopting `phantom-test` for SPUR TUI E2E tests right now.**

## Summary

`phantom-test` compiles as a plain dev-dependency on the stable toolchain used by
`scripts/spur-cargo`, and a trivial headless PTY process runs without installing
the full Phantom daemon, nightly Rust, or Zig. That satisfies the early
toolchain check.

The real SPUR TUI journey is blocked, however: starting `spur tui` under
`phantom-test` on the remote Linux test VM crashes the integration-test process
with SIGSEGV before Rust assertions can capture a screen. The crash reproduces
with one test thread and with mouse capture disabled, so this spike stops here
under the time-box rule.

## Sources Checked

- `phantom-test` docs for 0.1.0: https://docs.rs/phantom-test/latest/phantom_test/
- Phantom repository README: https://github.com/alexpasmantier/phantom
- Television harness reference: https://github.com/alexpasmantier/television/blob/main/tests/common/mod.rs

The upstream docs state that the Rust library embeds the terminal engine
directly and does not need the daemon. The full Phantom CLI still documents
nightly Rust plus Zig requirements, but this spike did not need either for
`phantom-test`.

## Dependency and Build Impact

Added only to `crates/spur-cli` dev-dependencies:

```toml
phantom-test = "0.1"
```

The lockfile adds 8 new packages:

- `phantom-test 0.1.0`
- `phantom-core 0.1.0`
- `phantom-daemon 0.1.0`
- `libghostty-vt 0.1.1`
- `libghostty-vt-sys 0.1.1`
- `nix 0.30.1`
- `int-enum 1.2.0`
- `proc-macro2-diagnostics 0.10.1`

`scripts/spur-cargo tree -p phantom-test --target x86_64-unknown-linux-gnu`
shows that `phantom-test` depends on `phantom-daemon`, `phantom-core`,
`libghostty-vt`, `nix`, `mio`, `crossbeam-channel`, `regex`, and several
already-present common Rust crates.

First compile probe:

```text
$ scripts/spur-cargo test -p spur-cli --test tui_e2e_phantom_spike --no-run
[spur-cargo] remote (test) -> aws-my VM
Locking 8 packages to latest compatible versions
Adding phantom-test v0.1.0
Adding phantom-core v0.1.0
Adding phantom-daemon v0.1.0
Adding libghostty-vt v0.1.1
Adding libghostty-vt-sys v0.1.1
Adding nix v0.30.1
Adding int-enum v1.2.0
Adding proc-macro2-diagnostics v0.10.1
Compiling libghostty-vt-sys v0.1.1
Compiling phantom-daemon v0.1.0
Compiling libghostty-vt v0.1.1
Compiling phantom-test v0.1.0
Finished `test` profile [unoptimized + debuginfo] target(s) in 9m 42s
```

Headless smoke probe:

```text
$ scripts/spur-cargo test -p spur-cli --test tui_e2e_phantom_spike
running 4 tests
test cold_launch_without_agents_renders_setup_nudge ... ignored, No-Go spike: phantom-test SIGSEGVs when starting the real SPUR TUI on the remote test VM; see docs/superpowers/specs/2026-07-07-phantom-test-e2e-spike-findings.md
test ctrl_c_then_y_exits_zero ... ignored, No-Go spike: phantom-test SIGSEGVs when starting the real SPUR TUI on the remote test VM; see docs/superpowers/specs/2026-07-07-phantom-test-e2e-spike-findings.md
test question_mark_opens_help_overlay ... ignored, No-Go spike: phantom-test SIGSEGVs when starting the real SPUR TUI on the remote test VM; see docs/superpowers/specs/2026-07-07-phantom-test-e2e-spike-findings.md
test phantom_test_runs_headless_without_daemon ... ok

test result: ok. 1 passed; 0 failed; 3 ignored; finished in 0.06s
```

That smoke probe used `phantom-test` to spawn a shell in a PTY and wait for
screen text, confirming the in-process engine works in the remote headless test
environment for a simple process.

## Test Harness Shape

The spike test file now contains:

- One non-ignored smoke test proving `phantom-test` runs headlessly without the
  daemon.
- Three ignored real-TUI repro journeys, kept as executable documentation of
  the intended coverage and exact setup.

The real journeys use:

- `env!("CARGO_BIN_EXE_spur")`
- command: `spur tui`
- PTY size: `80x24`
- isolated temp repo cwd with `.spur/`
- isolated `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`,
  `XDG_CACHE_HOME`
- prewritten `HOME/.spur/onboarded` marker to avoid the first-run TTY prompt
- `SPUR_NO_UPGRADE_CHECK=1`
- `SPUR_TUI_MOUSE_CAPTURE=0` in the final isolation attempt

No sleeps are used; the harness uses `wait().text(...)`,
`wait().stable(...)`, and `wait().exit_code(0)` with bounded timeouts.

## Intended Journeys and Results

| Journey | Expected assertion | Result |
| --- | --- | --- |
| Cold launch without agents | wait for `No agents configured`, assert `SPUR` and `spur init` | Failed: SIGSEGV before assertion |
| `?` opens help overlay | wait for `Keyboard environment`, assert `Ctrl-C` and `Press ? or Esc to close` | Not reached because the first real-TUI start crashes |
| `Ctrl-C`, `y` clean quit | wait for `Quit spur?`, assert exit code 0 | Not reached because the first real-TUI start crashes |

Focused failure runs:

```text
$ scripts/spur-cargo test -p spur-cli --test tui_e2e_phantom_spike
running 3 tests
error: test failed, to rerun pass `-p spur-cli --test tui_e2e_phantom_spike`

Caused by:
  process didn't exit successfully: `.../tui_e2e_phantom_spike-...`
  (signal: 11, SIGSEGV: invalid memory reference)
```

```text
$ scripts/spur-cargo test -p spur-cli --test tui_e2e_phantom_spike -- --test-threads=1
running 3 tests
test cold_launch_without_agents_renders_setup_nudge ... error: test failed

Caused by:
  process didn't exit successfully: `.../tui_e2e_phantom_spike-... cold_launch_without_agents_renders_setup_nudge --test-threads=1`
  (signal: 11, SIGSEGV: invalid memory reference)
```

```text
$ scripts/spur-cargo test -p spur-cli --test tui_e2e_phantom_spike cold_launch_without_agents_renders_setup_nudge -- --test-threads=1
running 1 test
test cold_launch_without_agents_renders_setup_nudge ... error: test failed

Caused by:
  process didn't exit successfully: `.../tui_e2e_phantom_spike-... cold_launch_without_agents_renders_setup_nudge --test-threads=1`
  (signal: 11, SIGSEGV: invalid memory reference)
```

The third run included `SPUR_TUI_MOUSE_CAPTURE=0`, so optional mouse capture is
not the immediate trigger. Because the crash is a process signal rather than a
Rust panic or assertion failure, the harness does not provide a useful screen
snapshot or backtrace at this layer.

Final branch verification:

```text
$ scripts/spur-cargo test -p spur-cli
...
running 4 tests
test cold_launch_without_agents_renders_setup_nudge ... ignored, No-Go spike: phantom-test SIGSEGVs when starting the real SPUR TUI on the remote test VM; see docs/superpowers/specs/2026-07-07-phantom-test-e2e-spike-findings.md
test ctrl_c_then_y_exits_zero ... ignored, No-Go spike: phantom-test SIGSEGVs when starting the real SPUR TUI on the remote test VM; see docs/superpowers/specs/2026-07-07-phantom-test-e2e-spike-findings.md
test question_mark_opens_help_overlay ... ignored, No-Go spike: phantom-test SIGSEGVs when starting the real SPUR TUI on the remote test VM; see docs/superpowers/specs/2026-07-07-phantom-test-e2e-spike-findings.md
test phantom_test_runs_headless_without_daemon ... ok

test result: ok. 1 passed; 0 failed; 3 ignored; finished in 0.06s
...
Doc-tests spur_cli

test result: ok. 0 passed; 0 failed; 0 ignored; finished in 0.00s
```

## API Ergonomics

Positive findings:

- The builder API matches the requested shape: `Phantom::new()`,
  `run(...).size(...).cwd(...).args(...).env(...).start()`.
- Wait APIs cover the basic needs: `text`, `regex`, `text_absent`, `stable`,
  `process_exit`, and `exit_code`.
- Screenshot text is easy to assert against.
- The Television harness patterns translate cleanly: central timeouts,
  stable-frame helper, and a small exit helper.

Gaps and risks:

- The dependency brings in `libghostty-vt-sys`, so native terminal-emulation
  code is in the test process. The observed SIGSEGV is consistent with that
  being a practical debugging risk.
- There is no obvious environment-clear API on the builder; the spike can set
  isolated config paths, but cannot guarantee a fully scrubbed inherited
  environment.
- Key sending uses string names such as `ctrl-c`, which is ergonomic but less
  type-checked than a Rust enum.
- Colors and styles were not validated. The lower-level screenshot JSON appears
  capable of exposing cell attributes, but the spike did not get past launch.
- Resize support exists in the underlying API, but was not exercised.

## Recommendation

No-Go for adopting `phantom-test` as SPUR's TUI E2E layer today.

The crate passes the early stable-toolchain and headless-simple-process checks,
but a real `spur tui` launch crashes the test binary under the required remote
test environment. Until that is diagnosed upstream or minimized into a stable
reproduction, adding active SPUR journeys would make `scripts/spur-cargo test
-p spur-cli` unreliable.

Fallback Option A is the better next step: build a small harness around
`portable-pty` plus `vt100`. It is less polished than `phantom-test`, but it
keeps the terminal emulator in well-known Rust crates, gives us direct control
over environment scrubbing and process lifecycle, and should be enough for the
current needs in roughly 200-400 lines.
