# VHS E2E Spike Findings

Date: 2026-07-07
Scope: `scripts/e2e/vhs/**` only
Recommendation: **No-Go for adopting VHS as SPUR's TUI golden E2E layer from this environment.**

## Summary

VHS 0.11.0 installed and can drive an out-of-process terminal session through
`ttyd`, and the committed spike harness exercises the three requested journeys
against a stand-in TUI with stable normalized text goldens. The real `spur tui`
journeys did not run because the required local `spur-cli` build failed twice
with exit 137 before producing `target/debug/spur`.

That makes this a documented No-Go for adopting VHS from this spike branch, not
a pass against the real binary. The useful framework finding is that VHS can
express these journeys with `Wait+Screen@15s` and no `Sleep`, but raw `.txt`
goldens include nondeterministic command-entry snapshots. The runner therefore
keeps raw VHS output under `actual/raw/` and diffs a normalized `.txt` screen
sample selected by journey markers.

## Sources Checked

- VHS README and command reference: https://github.com/charmbracelet/vhs
- Installed VHS 0.11.0 README: `/opt/homebrew/Cellar/vhs/0.11.0/README.md`
- VHS 0.11.0 text-output implementation: https://raw.githubusercontent.com/charmbracelet/vhs/v0.11.0/testing.go
- VHS 0.11.0 ttyd launcher implementation: https://raw.githubusercontent.com/charmbracelet/vhs/v0.11.0/tty.go
- Official CI action: https://github.com/charmbracelet/vhs-action
- Prior phantom-test findings: `docs/superpowers/specs/2026-07-07-phantom-test-e2e-spike-findings.md`

The README documents `Wait+Screen@10ms /World/`, `Set Shell`, pixel-based
`Set Width`/`Set Height`, `Screenshot`, and text golden usage via `.txt` or
`.ascii` output. The source confirms `.txt` output writes the current xterm
buffer after VHS commands, separated by an 80-character rule.

## What Was Added

- `scripts/e2e/vhs/check-vhs.sh`
- `scripts/e2e/vhs/run-vhs-suite.sh`
- `scripts/e2e/vhs/bin/run-spur-tui.sh`
- `scripts/e2e/vhs/bin/standin-spur`
- `scripts/e2e/vhs/tapes/{cold-launch,help-overlay,clean-quit}.tape`
- `scripts/e2e/vhs/goldens/{cold-launch,help-overlay,clean-quit}.txt`

The wrapper implements the requested isolation shape: temp cwd containing
`.spur/`, isolated `HOME` and XDG directories, `$HOME/.spur/onboarded`,
`SPUR_NO_UPGRADE_CHECK=1`, `SPUR_TUI_MOUSE_CAPTURE=0`, and
`SPUR_LICENSE_TEST_STRIP_KEYS=""`.

The runner defaults to `target/debug/spur`, accepts `SPUR_BIN=/path/to/spur`,
and has `SPUR_VHS_STANDIN=1` for the framework-only probe used after the local
build failed.

## Install Footprint and Pinning

Installed tool versions:

```text
vhs version 0.11.0
ttyd version 1.7.7-unknown
ffmpeg version 8.1.2 Copyright (c) 2000-2026 the FFmpeg developers
```

Homebrew selected `vhs 0.11.0`, installed `ttyd 1.7.7_11`, and upgraded the
existing `ffmpeg` chain. The resolver reported these direct VHS dependencies:
`sdl3`, `json-c`, `libwebsockets`, and `ttyd`; the ffmpeg-related upgrades
included `libvmaf`, `libvpx`, `ca-certificates`, `openssl@3`, `opus`,
`sdl2-compat`, `svt-av1`, `x264`, `x265`, `ffmpeg`, and `libevent`.

The install had one local Homebrew issue: `ca-certificates` postinstall hung
while regenerating the keychain bundle. Terminating that stuck postinstall let
Homebrew continue, and the tools were usable, but `brew install vhs` exited 1.
`check-vhs.sh --install` is pinned by verification rather than by formula lock:
it runs `brew install vhs` and then requires `vhs --version` to report exactly
0.11.0. If Homebrew advances, use the pinned GitHub release URL noted by the
script and install `ttyd`/`ffmpeg` separately.

## Real SPUR Binary Result

The spike did **not** drive the real SPUR TUI. Two focused local builds were
attempted as requested:

```text
$ SPUR_REMOTE=0 scripts/spur-cargo build -p spur-cli
...
Compiling toon_rust v0.1.1 (...)
process exited with code 137
```

```text
$ SPUR_REMOTE=0 CARGO_BUILD_JOBS=1 scripts/spur-cargo build -p spur-cli --jobs 1
Compiling datafusion v53.1.0
Compiling lance-table v6.0.0
Compiling lance-datafusion v6.0.0
Compiling lance-index v6.0.0
Compiling lance v6.0.0
process exited with code 137
```

After those failures, `target/debug/spur` was still missing. Per the time-box
rule, real-binary attempts stopped and the VHS ergonomics were evaluated with
`scripts/e2e/vhs/bin/standin-spur`.

## Tape Reality

All committed tapes use:

- `Set Shell bash`
- `Set Width 1067`
- `Set Height 600`
- `Set FontSize 20`
- `Set Padding 0`
- `Set Margin 0`
- `Wait+Screen@15s /.../`
- `Hide`/`Show` around typed commands to reduce command-entry noise

No committed tape uses `Sleep`. The waits expressed all journey conditions:
`No agents configured`, `Keyboard environment`, `Quit spur?`, and
`VHS_SPUR_EXITED status=0`.

Sizing caveat: VHS `Set Width`/`Set Height` are pixels, not terminal cells. On
this macOS install, `Set Width 1067`, `Set Height 600`, `Set FontSize 20`,
zero padding, and zero margin produced `stty size` = `24 80`. This should be
treated as a pinned local xterm.js calibration, not a portable columns/rows API.

Other syntax findings:

- `Output` paths must be relative in the tape; an absolute `/var/.../probe.txt`
  path failed validation.
- `Set Shell` accepts shell names such as `bash`; it rejected arguments and
  relative wrapper paths, so the tapes launch `./bin/run-spur-tui.sh` via
  `Type`/`Enter`.
- `Screenshot` is supported by VHS but intentionally unused; no image/video
  artifacts are committed.

## Golden Determinism

Raw `.txt` output was **not** byte-stable. VHS writes multiple xterm buffers
throughout the tape, and early buffers captured partial shell input such as
`> ./bi` or a bare prompt. Those frames varied across runs.

The committed runner masks this by extracting stable screen segments from the
raw VHS output into `actual/<journey>.txt`, then diffs those normalized files
against checked-in goldens. The raw files remain under `actual/raw/` for local
diagnostics and are ignored by git.

Three stand-in runs were byte-stable after normalization:

```text
=== run 1 ===
PASS cold-launch runtime=2s golden=stable
PASS help-overlay runtime=1s golden=stable
PASS clean-quit runtime=1s golden=stable
201710eeab2702788bae8e5b366baa0497cb58004a586b4457b0be3be5e8c29b  scripts/e2e/vhs/actual/clean-quit.txt
2aaff897428139e7257929ce9bc1d1cd17c16f5a0e323a239b38a30c4e6df437  scripts/e2e/vhs/actual/help-overlay.txt
8c8e1b91e46fedf1fc9f924ba3ce1e7290728ca9dd8003e746c3e65dc96f768f  scripts/e2e/vhs/actual/cold-launch.txt

=== run 2 ===
PASS cold-launch runtime=1s golden=stable
PASS help-overlay runtime=2s golden=stable
PASS clean-quit runtime=1s golden=stable
201710eeab2702788bae8e5b366baa0497cb58004a586b4457b0be3be5e8c29b  scripts/e2e/vhs/actual/clean-quit.txt
2aaff897428139e7257929ce9bc1d1cd17c16f5a0e323a239b38a30c4e6df437  scripts/e2e/vhs/actual/help-overlay.txt
8c8e1b91e46fedf1fc9f924ba3ce1e7290728ca9dd8003e746c3e65dc96f768f  scripts/e2e/vhs/actual/cold-launch.txt

=== run 3 ===
PASS cold-launch runtime=1s golden=stable
PASS help-overlay runtime=2s golden=stable
PASS clean-quit runtime=1s golden=stable
201710eeab2702788bae8e5b366baa0497cb58004a586b4457b0be3be5e8c29b  scripts/e2e/vhs/actual/clean-quit.txt
2aaff897428139e7257929ce9bc1d1cd17c16f5a0e323a239b38a30c4e6df437  scripts/e2e/vhs/actual/help-overlay.txt
8c8e1b91e46fedf1fc9f924ba3ce1e7290728ca9dd8003e746c3e65dc96f768f  scripts/e2e/vhs/actual/cold-launch.txt
```

## Failure Diagnostics

The diff is readable after normalization. A mismatch prints a standard unified
diff, then a one-line failure summary, for example:

```text
--- goldens/clean-quit.txt
+++ actual/clean-quit.txt
@@ ...
-VHS_SPUR_EXITED status=0
+VHS_SPUR_EXITED status=130
FAIL clean-quit runtime=1s golden=mismatch
```

Before normalization, diagnostics were noisy but still intelligible: the first
stability attempt showed partial prompt/input diffs (`> ./bi`, `> .`) rather
than TUI behavior. That is the main golden-maintenance risk for raw VHS `.txt`.

VHS itself also surfaced useful wait failures. A stand-in race produced:

```text
timeout waiting for "Screen VHS_SPUR_EXITED status=0" to match VHS_SPUR_EXITED status=0;
last value was: Quit spur?
...
[y] yes   [n] no
```

That last-screen value is useful for debugging wait failures.

## CI Story

VHS has an official GitHub Action, `charmbracelet/vhs-action`, and the README
explicitly describes `.txt`/`.ascii` output as an integration-testing golden
workflow. This spike does not add CI wiring. If revisited, CI should install the
pinned VHS version plus `ttyd` and `ffmpeg`, build the SPUR binary, then run
`scripts/e2e/vhs/run-vhs-suite.sh`.

## Maintenance Risks

- Requires browser/ttyd/ffmpeg infrastructure for what is logically a text TUI
  test. The install footprint is larger than a Rust PTY harness.
- Raw `.txt` output is command-timing-sensitive. A normalizer is required for
  stable goldens in this workflow.
- Pixel calibration is fragile. `Set Width`/`Set Height` do not directly encode
  `80x24`, so font or xterm.js rendering changes could shift cells.
- `Set Shell` is less flexible than expected; env isolation belongs in a typed
  wrapper command rather than the shell setting.
- The first tiny VHS probe failed once with `could not open ttyd: navigation
  failed: net::ERR_CONNECTION_REFUSED`; a retry succeeded. That points to
  startup timing sensitivity around the ttyd/browser connection.

## Recommendation

No-Go for adopting VHS from this spike branch, because the required proof point
was three journeys passing against the real `spur tui`, and the local build
failed twice before a binary existed.

Relative to alternatives:

- **Shell-use CLI assertion approach:** lower install footprint and likely
  enough for non-interactive CLI contracts, but it cannot naturally validate
  TUI key flows or screen states.
- **Hand-rolled `portable-pty` + `vt100` harness:** still the strongest next
  candidate. It should avoid browser/ffmpeg/ttyd dependencies, encode terminal
  size directly as rows/cols, and keep raw screen text under our control.
- **VHS:** viable as a documentation/demo recorder and plausible as an outer
  e2e harness, but SPUR would need normalized output, pinned pixel calibration,
  and a successful real-binary build before considering it mature enough.
