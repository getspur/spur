# TUI E2E Layer — Adoption Decision

Date: 2026-07-07
Status: **Decided** — role-split adoption of vhs (visual) + shell-use (behavioral)
Related: `2026-07-07-phantom-test-e2e-spike-findings.md`,
`2026-07-07-vhs-e2e-spike-findings.md` (worker branch carries the shell-use
findings doc; merged alongside), `scripts/e2e/vhs/`, `scripts/e2e/shell-use/`

## Problem

SPUR's TUI test suite is entirely in-process (ratatui `TestBackend`, direct
`handle_crossterm_event` injection, golden files via `UPDATE_GOLDEN=1`). Nothing
drives the real compiled binary in a real PTY, so terminal init/teardown,
crossterm escape parsing, the real tokio event loop, startup panics, resize,
and clean-exit behavior are untested. The requirement: adopt a mature existing
end-to-end TUI test framework ("Playwright for TUI") rather than self-building
a harness.

## Candidates evaluated (all spiked against the real `spur tui` binary)

| Candidate | Spike result | Verdict |
|---|---|---|
| `phantom-test` (Rust crate, Ghostty VT in-process) | Native SIGSEGV on real-binary launch, 3/3, no diagnostics possible | **No-Go** |
| charmbracelet **vhs** v0.11.0 (tape + golden diff, out-of-process) | 9/9 journeys, goldens byte-stable across 3 runs, 1–2s/journey | **Go** |
| microsoft **shell-use** 0.0.1-beta.3 (CLI wait/expect verbs, out-of-process daemon) | 9/9 journeys, zero flakes, best failure diagnostics (full screen dump) | **Go** |

Industry survey context: no mature batteries-included Rust-native TUI e2e
framework exists (zellij, helix, television all hand-rolled); the two proven
patterns are PTY+emulator golden files and programmatic wait/expect drivers.
Out-of-process emulation is a hard requirement here — the phantom-test failure
mode (native emulator bindings crashing inside the cargo test process) cannot
happen when the emulator lives in a separate binary.

## Decision

Adopt **both survivors in split roles**, journeys kept portable between them:

- **vhs (pinned 0.11.0)** owns **visual-regression e2e**: all screen goldens
  live here (`scripts/e2e/vhs/`, `SPUR_VHS_UPDATE=1` to re-record — the repo's
  existing `UPDATE_GOLDEN` idiom). Side benefit: tapes double as demo
  recordings.
- **shell-use (pinned beta.3, checksummed install)** owns **behavioral e2e**:
  wait/expect/exit-code journeys (`scripts/e2e/shell-use/`), the growth path
  for ACP-scripted flows needing mid-flight assertions. Its snapshot feature is
  deliberately NOT used — no golden corpus accumulates on the beta tool.

**Authoring rule:** asserting how it *looks* → vhs tape; asserting what it
*does* → shell-use journey.

## Why not a single winner

The two natural weightings pick different solo winners: maturity-first picks
vhs (4 years of releases, 20k stars) but rides an upstream-uncovenanted testing
path with a hard assertion ceiling (exit codes already needed a sentinel hack
on day one); capability/agent-ergonomics-first picks shell-use, but it is a
week-old beta whose predecessor (tui-test) Microsoft already dead-ended once.
The role-split assigns each tool the sub-role where its weakness does not bind,
and survived sensitivity analysis (either tool dying) that collapsed both solo
options. Marginal cost is near zero: both harnesses exist, pass 9/9, and CI
adds one brew formula plus one pinned static binary.

## Guardrails

1. Both versions stay pinned; upgrades are explicit spike work, never drive-by.
2. Journeys share vocabulary (wait-strings, 80x24 size, env-isolation wrapper:
   temp cwd + isolated HOME/XDG + `.spur/onboarded` marker +
   `SPUR_NO_UPGRADE_CHECK=1`) so either side ports in hours.
3. **Exit trigger for shell-use:** no stable 1.0 within 12 months, or two
   breaking upgrades — migrate behavioral journeys to the documented fallback
   (thin `portable-pty` + `vt100` harness, ~200-400 lines) or vhs wait-chains.
4. Goldens only on the vhs side (see above).

## Open follow-ups

- CI wiring: runners need a local `spur` binary (the spike blocker was a
  cold-cache link OOM in worker sandboxes; warm-cache local build takes ~3m).
  The zigbuild macOS cross path (zig-1..zig-7) and Linux VM builds are the
  enablers. vhs needs ttyd+ffmpeg+headless-Chromium (official vhs-action);
  shell-use is a single static binary.
- Validate shell-use daemon lifecycle on Linux CI.
- Extend journeys beyond the initial three (cold launch, help overlay, clean
  quit) toward ACP-scripted flows on the behavioral side.
