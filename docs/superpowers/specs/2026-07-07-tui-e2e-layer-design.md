# TUI E2E Layer — Design Spec

Date: 2026-07-07
Status: Draft for implementation
Decision record: `2026-07-07-tui-e2e-layer-decision.md` (role-split: vhs visual /
shell-use behavioral). Spike evidence:
`2026-07-07-phantom-test-e2e-spike-findings.md`,
`2026-07-07-vhs-e2e-spike-findings.md`, shell-use findings (merged with harness).

## 1. Summary

Build out SPUR's end-to-end TUI test layer on the two validated drivers:

- **vhs (pinned 0.11.0)** — visual-regression tapes + screen goldens.
- **shell-use (pinned 0.0.1-beta.3)** — behavioral journeys with programmatic
  wait/expect/exit-code assertions.

Both drive the real compiled `spur` binary in a real PTY, out-of-process. The
journeys — not the drivers — are the long-lived asset: shared vocabulary,
shared isolation fixture, portable between drivers in hours.

**Authoring rule: asserting how it looks → vhs tape; asserting what it does →
shell-use journey.**

### Goals

1. Catch regressions in-process `TestBackend` tests structurally cannot see:
   terminal init/teardown, crossterm escape parsing, the real tokio event loop
   (`run_tui_with_license`), startup panics, clean exit, resize.
2. Deterministic in CI and locally; failures diagnosable by AI workers.
3. Grow from the 3 seed journeys to ACP-scripted flows without redesign.

### Non-goals

- Replacing in-process tests (`TestBackend`, `test_support`, golden render
  tests) — those remain the bulk of coverage per the SIT/UAT plan docs.
- Cross-terminal-emulator conformance testing (one emulator per driver is
  enough; we test SPUR, not terminals).
- Performance/latency benchmarking of the TUI.

## 2. Architecture

```
tier 1  in-process   TestBackend + event injection      (crates/spur-tui/tests)
tier 2  behavioral   shell-use daemon → PTY → spur tui  (scripts/e2e/shell-use)
tier 3  visual       vhs (ttyd+chromium) → PTY → spur   (scripts/e2e/vhs)
```

Component view of tiers 2–3 (everything under `scripts/e2e/`):

```
scripts/e2e/
├── lib/
│   ├── isolate.sh          # NEW shared fixture: isolated workspace/HOME/XDG
│   └── spur-bin.sh         # NEW shared binary resolution + build hint
├── run-all.sh              # NEW top-level runner: both suites, one exit code
├── JOURNEYS.md             # NEW journey catalog (single source of truth)
├── shell-use/
│   ├── install.sh          # pinned + sha256-verified release download (exists)
│   ├── lib.sh              # session lifecycle + assertion DSL (exists)
│   ├── run.sh              # suite runner, SHELL_USE_RUNS=N (exists)
│   └── journeys/*.sh       # one file per behavioral journey (3 exist)
└── vhs/
    ├── check-vhs.sh        # pinned toolchain check, --install (exists)
    ├── run-vhs-suite.sh    # tape runner + normalizer + golden diff (exists)
    ├── bin/run-spur-tui.sh # env-isolation wrapper the tapes launch (exists)
    ├── tapes/*.tape        # one tape per visual journey (3 exist)
    └── goldens/*.txt       # normalized real-binary screens (3 exist)
```

## 3. Shared infrastructure (to build)

### 3.1 `lib/isolate.sh` — one fixture, two consumers

Today the isolation recipe is duplicated in `shell-use/lib.sh` and
`vhs/bin/run-spur-tui.sh`. Extract the single source of truth:

- temp workspace dir containing `.spur/` (cwd for the binary)
- isolated `HOME`, `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`,
  `XDG_CACHE_HOME`
- pre-created `$HOME/.spur/onboarded` (skips first-run prompt)
- `SPUR_NO_UPGRADE_CHECK=1`, `SPUR_TUI_MOUSE_CAPTURE=0`,
  `SPUR_LICENSE_TEST_STRIP_KEYS=`, `CI=false`
- optional fixture hooks (see §6): `SPUR_E2E_FIXTURE=<name>` copies
  `scripts/e2e/fixtures/<name>/` into the workspace (agent config, prompts,
  fake-agent bin dir prepended to PATH)

Contract: `isolate.sh` prints the workspace root; callers export the env vars
it emits. Cleanup on trap. Both drivers consume it; a future portable-pty
harness (the documented fallback) consumes it unchanged.

### 3.2 `lib/spur-bin.sh` — binary resolution contract

Resolution order: `$SPUR_BIN` → `<repo-root>/target/debug/spur` → error with
the build hint (`SPUR_REMOTE=0 scripts/spur-cargo build -p spur-cli` locally;
plain `cargo build -p spur-cli --locked` in GHA). Never builds implicitly —
building is the caller's job (keeps runners fast and failure modes obvious).

### 3.3 `run-all.sh` — one entry point

Runs `shell-use/run.sh` then `vhs/run-vhs-suite.sh`; nonzero if either fails;
`SPUR_E2E_ONLY=behavioral|visual` filter; on failure collects artifacts into
`scripts/e2e/.artifacts/` (raw vhs txt, shell-use `text --full` + `state`
dumps, `~/Library/Caches/shell-use/*.cast` recordings) for CI upload.

### 3.4 `JOURNEYS.md` — the catalog

One row per journey: id, user story, side (behavioral/visual/both), fixture,
wait-strings used, owning tape/script. This is the portability ledger — the
wait-strings and step sequences are what port between drivers. PR rule: adding
or changing a journey updates the catalog in the same commit.

## 4. Behavioral side (shell-use) — conventions

- **Session lifecycle:** one shell-use session per journey, named
  `spur-<run>-<journey>-$$`; `open --shell bash` + `submit "$SPUR_BIN tui"`
  (not `run`) so `wait command` / `expect exit-code` have command tracking;
  `close` + workspace cleanup on trap.
- **Assertion DSL** (exists in `lib.sh`, keep as the only surface journeys
  touch): `wait_text`, `expect_text` (always `--no-strict` for contains
  semantics), `type_text`, `press_key`, `wait_command_done`,
  `expect_exit_code`, `quit_cleanly`.
- **Timeouts:** every wait bounded; default `SHELL_USE_TIMEOUT_MS=10000`,
  overridable; CI sets a 3× multiplier env instead of editing journeys.
- **No sleeps. No retries inside a journey.** A journey that needs a retry is
  a bug in the journey or the app.
- **Diagnostics:** any failed command dumps `state` + `text --full` (exists).
- **Prohibited:** `expect snapshot` / `__snapshots__` — goldens live only on
  the vhs side (decision-doc guardrail: no golden corpus on the beta tool).
- **Upgrades:** version bump = dedicated spike commit updating
  `install.sh` pins + checksums + a full 3× suite run recorded in the commit
  message. Never drive-by.

## 5. Visual side (vhs) — conventions

- **Tape shape** (per existing tapes): `Set Shell bash`, pixel geometry pinned
  to yield an 80×24 grid (`Width 1067 / Height 600 / FontSize 20 / Padding 0 /
  Margin 0`), `TypingSpeed 0ms`, `Hide` around setup keystrokes, launch via
  `bin/run-spur-tui.sh`, **`Wait+Screen@15s /regex/` as the only
  synchronization primitive**. `Sleep` requires a justification comment in the
  tape and an entry in the findings/catalog; today there are zero.
- **Exit assertions** use the wrapper's sentinel: `run-spur-tui.sh` clears the
  screen and prints `VHS_SPUR_EXITED status=N` after the binary exits.
- **Normalizer contract** (in `run-vhs-suite.sh`): each journey declares the
  screen segment(s) to extract by anchor regex; only extracted segments are
  golden-diffed. This is the churn firewall — volatile regions (status-bar
  timer, spinners) stay outside anchored segments. Adding a journey = tape +
  normalizer case + golden, in one commit.
- **Golden lifecycle:** `SPUR_VHS_UPDATE=1 scripts/e2e/vhs/run-vhs-suite.sh`
  re-records; goldens are plain text and MUST be reviewed in the PR diff (same
  discipline as `render_golden.rs` + `UPDATE_GOLDEN=1`). A golden change with
  no intended visual change is a regression signal, not an update.
- **Stability bar:** new tapes must pass 3 consecutive runs with byte-identical
  goldens before merge (the spike bar).

## 6. Fixtures for ACP-scripted journeys (phase 2)

The seed journeys run agent-less ("No agents configured" landing). Deeper
journeys need a configured, deterministic agent. Grounding in existing code:

- `spur init` discovers agents on PATH and writes `<cwd>/.spur/config.toml`
  (`crates/spur-cli/tests/init_ux.rs` — including the `stub_which` pattern of
  planting fake agent executables on a controlled PATH).
- Per-agent profiles live at `.spur/agents/<name>.md` (see
  `write_agent_profile` in `crates/spur-tui/src/mentions/registry.rs` and the
  `spur-probe-echo` fixture used by picker tests).
- The TUI resolves config via `App::resolve_agent_config` /
  `fallback_agent_config` (`crates/spur-tui/src/app/events.rs`).

Design: `scripts/e2e/fixtures/<name>/` directories materialized by
`isolate.sh` into the journey workspace:

- `fixtures/no-agents/` — empty (the current default, made explicit).
- `fixtures/echo-agent/` — a pre-written `.spur/config.toml` declaring one
  agent whose command is a **scripted fake ACP executable** (shell or tiny
  Rust bin) that speaks just enough ACP to accept a session and stream a
  canned response. The `stub_which` test pattern proves the PATH-planting
  mechanics; the ACP transcript format is the open design item (§10).

Target journeys this unlocks (behavioral side): "type message → submit →
canned agent reply renders", "Ctrl-C interrupt mid-response → quit prompt",
"session appears in picker after submit".

## 7. CI integration

New job (either `e2e` in `ci.yml` or a dedicated `e2e.yml`), modeled on the
existing `check-test` job conventions (ubuntu-24.04, bare cargo with `--locked`,
`~/.cargo` cache only — `scripts/spur-cargo` stays local/VM, per repo policy;
GHA runners use plain cargo exactly like `ci.yml` does today):

1. `cargo build -p spur-cli --locked` (debug) — produces `target/debug/spur`.
2. Install drivers, both cached: `scripts/e2e/shell-use/install.sh` (static
   binary, sha256-pinned — the script already carries Linux x86_64/arm64
   digests) and vhs 0.11.0 via `charmbracelet/vhs-action` or pinned `.deb`
   (pulls ttyd + ffmpeg; vhs downloads headless Chromium on first run — cache
   `~/.cache/rod`).
3. `scripts/e2e/run-all.sh` (single run per journey; the 3× stability bar is a
   merge-time authoring requirement, not a per-CI-run cost).
4. `actions/upload-artifact` of `scripts/e2e/.artifacts/` on failure.

Gating: **advisory (non-required) for the first two weeks**, then required
once the observed flake rate is 0 across ≥50 CI runs. Track any flake as an
issue tagged `e2e-flake`; two unexplained flakes in a week demotes the job to
advisory until root-caused (no auto-retry masking — retries hide exactly the
nondeterminism this layer exists to catch).

Linux validation note: both drivers passed only on macOS so far. First CI PR
must treat "suites green on ubuntu-24.04" as its acceptance criterion
(shell-use daemon lifecycle on Linux is the named unknown from the findings).

## 8. Local developer / agent workflow

- Build once: `SPUR_REMOTE=0 scripts/spur-cargo build -p spur-cli`.
- Everything: `scripts/e2e/run-all.sh`. One suite: `SPUR_E2E_ONLY=…`.
- One behavioral journey: `scripts/e2e/shell-use/journeys/<j>.sh`.
- Re-record goldens after an intended visual change:
  `SPUR_VHS_UPDATE=1 scripts/e2e/vhs/run-vhs-suite.sh`, review the diff,
  commit goldens with the change that caused them.
- Interactive debugging of a journey: shell-use sessions are inspectable
  (`shell-use --session <s> text --full`, `.cast` recordings in the user
  cache) — this doubles as the "agent drives the TUI" harness.

## 9. Rollout phases

| Phase | Deliverable | Acceptance |
|---|---|---|
| 1 | `lib/isolate.sh`, `lib/spur-bin.sh`, `run-all.sh`, `JOURNEYS.md`; both suites refactored onto shared lib | both suites 3×3 green locally, no behavior change |
| 2 | CI job (advisory) incl. Linux validation of both drivers | green on ubuntu-24.04, artifacts upload on failure |
| 3 | Journey growth, agent-less: session-picker open/filter, palette open, resize (SIGWINCH via shell-use / new tape geometry), paste-atom UAT (F3 from SIT/UAT scenarios doc) | catalog rows + 3× stability each |
| 4 | `echo-agent` fixture + first 3 ACP-scripted behavioral journeys | canned-reply journey green 3× |
| 5 | CI job flips to required | ≥50 advisory runs, 0 flakes |

Each phase is one plan-doc task group; commit convention
`test(scripts): e2e-<n> …` / `docs(specs): e2e-<n> …`.

## 10. Risks, mitigations, open questions

| Risk | Mitigation |
|---|---|
| shell-use beta churn / abandonment (predecessor precedent) | pins + checksums; no goldens on it; 10-line journeys; exit trigger: no 1.0 in 12 months or two breaking upgrades → port behavioral journeys to portable-pty/vt100 fallback |
| vhs `.txt` output format changes upstream (testing is an unofficial use) | version pinned; normalizer isolates asserted segments; goldens re-recordable in one command |
| Golden churn from fast TUI iteration | normalizer anchors only stable regions; goldens reviewed as part of the causing PR |
| Linux behavior differs from macOS spikes | phase-2 acceptance gate; both drivers are out-of-process so failures are diagnosable from dumps |
| CI binary build too slow on GHA | debug build of one crate with warm `~/.cargo` cache; if still slow, reuse `spur-tui-build.yml` artifact or a dedicated build job with `actions/cache` on target (explicitly deviating from ci.yml's no-target-cache stance is a follow-up decision) |
| Fake-ACP-agent fixture complexity | start from `init_ux.rs` `stub_which` mechanics; keep transcript canned, not interactive; open question below |

Open questions (to resolve in phase 4 design):

1. Minimal ACP handshake the fake agent must implement for `spur tui` to treat
   it as connected (survey `crates/spur-acp` client expectations; candidate:
   reuse the existing test stub if `spur-acp` tests already ship one).
2. Whether resize journeys are expressible in vhs (tape geometry is fixed per
   tape) — likely behavioral-side-only via shell-use once it exposes resize;
   otherwise defer to the portable-pty fallback harness.
3. Whether `run-all.sh` should also invoke the in-process suite
   (`spur-cargo test -p spur-tui`) for a single "all TUI tests" entry point,
   or stay e2e-only (leaning: e2e-only; CI already runs tier 1).
