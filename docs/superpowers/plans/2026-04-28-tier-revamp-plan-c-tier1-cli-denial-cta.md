# Tier Revamp Plan C — Tier 1: CLI Denial → Upgrade CTA

> **Status:** ✅ SHIPPED 2026-04-28 (commits `94a6ff9d` plan,
> `f28374ba` impl, `1c17732a` smoke). See **§ Post-merge addendum**
> at the end of this doc for the canonical landed shape — sections
> below preserve the v1 prescription as the audit trail of the
> 3-gate review evolution.

> **For agentic workers:** This plan ships in 2 atomic implementation
> tasks; each task is delegated to a fresh worker and gated by a
> 3-reviewer panel before merge. The brain (orchestrator) judges
> reviewer output and decides accept / iterate.

**Goal:** Reverse the "anger without recovery" regression introduced
by Plan C M0 + M0.5. Every gated denial today bubbles a terse
`anyhow::Error` to stderr (e.g. `Error: feature 'cli_core_exec' is
not available on tier 'community'`) — product-correct but offers
zero recovery affordance. Tier 1 wraps the top-level error rendering
in a `FeatureGateError`-aware path that prints a structured
upgrade-CTA: original error + `spur auth status` + `spur auth login
--key …` recovery lines.

**Why this is the next move (per L9-MCTS verdict):**
- Plan C M0 + M0.5 shipped 9 enforced `cli_core_*` keys + the
  auth-Login enforcement. No conversion mechanism wired yet.
- Continuing to ship more gates (Plan C M1) before the conversion
  pipeline exists amplifies the regression.
- Tier 1 is the smallest atomic increment that restores recovery
  guidance to denied users. ~50 LOC, low risk, foundation pattern
  for Tier 2 (TUI modal) and Tier 3 (trial JWT CTA).

**Tech Stack:** Rust 2021, anyhow, std `IsTerminal`, existing
`assert_cmd` test scaffold (added in M0 Task 5). No new deps.

---

## Spec grounding

- Plan C M0 plan v3
  (`2026-04-28-tier-revamp-plan-c-m0-cli-guards.md`) §"Why M0 v1
  was wrong about the Display message" — codex review verdict that
  recovery affordances belong in the caller layer, not the
  typed-error string.
- `crates/spur-license/src/gate.rs` — `FeatureGateError::Denied {
  key: FeatureKey, tier: Tier }` is the typed-error contract every
  gate-fire site uses.
- `crates/spur-cli/src/main.rs:386-393` — current top-level
  `#[tokio::main] async fn main() -> Result<()>` returns to Rust's
  default `Termination` impl which prints `Error: {display}` to
  stderr and exits non-zero.
- `crates/spur-license/src/lib.rs:60-69` — `Plan` enum (Community /
  StarterLtd / BuilderLtd / FounderLtd / Pro / Team / Enterprise /
  Unknown) for tier-aware copy branching if needed.
- M0.5 `cli_core_gate_e2e.rs` — `SPUR_LICENSE_TEST_STRIP_KEYS`
  fixture for binary-level denial e2e tests.

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-cli/src/main.rs` | Modify | Refactor `main` → `main` + `run()`, add CTA renderer dispatch |
| `crates/spur-cli/src/upgrade_cta.rs` | Create | New private module containing the CTA renderer + chain-walk helper |
| `crates/spur-cli/tests/cli_core_gate_e2e.rs` | Modify | Add binary-level CTA assertion test |
| `crates/spur-cli/tests/upgrade_cta_render.rs` | Create | Unit-level test for the renderer (dependency-free) |

No new deps. `std::io::IsTerminal` is stable since Rust 1.70 — verify the workspace MSRV
in `Cargo.toml`/`rust-toolchain.toml` supports it (it does — 1.74+ has been the floor for
months).

---

## Task 1: Implement CTA renderer + main refactor

**Worker assignment:** claude-code (implementer). Reviewers: kimi,
gemini, claude-code (3 gates in parallel).

**Files:**
- Modify: `crates/spur-cli/src/main.rs:386` (refactor `main`)
- Create: `crates/spur-cli/src/upgrade_cta.rs`

### Subtask 1a: Create the renderer module

Create `crates/spur-cli/src/upgrade_cta.rs`:

```rust
//! CTA renderer for `FeatureGateError`. Translates a typed gate
//! denial into structured stderr output with concrete recovery
//! affordances. TTY-gated so piped/scripted output stays clean.

use spur_license::FeatureGateError;

/// Walk an `anyhow::Error` chain looking for a `FeatureGateError`
/// root cause. Returns `Some(&FeatureGateError)` if found.
///
/// The chain walk is required (not just `downcast_ref` on the top
/// error) because gate-checks may be wrapped via `.context(...)`
/// in callers we don't directly control. anyhow's `chain()` walks
/// the source links; the first matching downcast wins.
pub(crate) fn find_gate_error(err: &anyhow::Error) -> Option<&FeatureGateError> {
    err.chain()
        .find_map(|e| e.downcast_ref::<FeatureGateError>())
}

/// Format the structured CTA for a `FeatureGateError`. Returns
/// the multi-line stderr string. Caller is responsible for
/// printing it (so tests can capture without writing to stderr).
///
/// Output shape (subject to reviewer iteration):
///
/// ```text
/// Error: feature `cli_core_exec` is not available on tier `Community`
///
/// To unlock this feature:
///   • View tier comparison:  spur auth status
///   • Activate a license:    spur auth login --key <KEY>
///
/// If you have a license but it appears stripped or expired, run
/// `spur auth logout` then re-login to fall back to a fresh
/// community-tier baseline before activating.
/// ```
pub(crate) fn format_upgrade_cta(gate_err: &FeatureGateError) -> String {
    let FeatureGateError::Denied { key, tier } = gate_err;
    let mut out = String::new();
    out.push_str(&format!("Error: {gate_err}\n"));
    out.push('\n');
    out.push_str("To unlock this feature:\n");
    out.push_str("  \u{2022} View tier comparison:  spur auth status\n");
    out.push_str("  \u{2022} Activate a license:    spur auth login --key <KEY>\n");
    out.push('\n');
    out.push_str(
        "If you have a license but it appears stripped or expired, run\n\
         `spur auth logout` then re-login to fall back to a fresh\n\
         community-tier baseline before activating.\n",
    );
    // Reference the unused fields explicitly so future reviewers see
    // they're available for richer per-tier copy in Tier 2 / Tier 3.
    let _ = (key, tier);
    out
}
```

### Subtask 1b: Refactor `main`

In `crates/spur-cli/src/main.rs`:

1. Add module declaration near the top (after `mod onboarding;`):

```rust
mod upgrade_cta;
```

2. Add `IsTerminal` import in the `use` block:

```rust
use std::io::IsTerminal;
```

3. Refactor the existing `#[tokio::main] async fn main() -> Result<()>`
   into a thin shim that wraps a renamed `run()`:

```rust
#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        render_top_level_error(&err);
        std::process::exit(1);
    }
}

/// Render the top-level error. If stderr is a TTY and the error
/// chain contains a `FeatureGateError`, render the structured
/// upgrade CTA. Otherwise fall through to anyhow's chain-printing
/// (`{:#}`) for full debug context.
fn render_top_level_error(err: &anyhow::Error) {
    if std::io::stderr().is_terminal() {
        if let Some(gate_err) = upgrade_cta::find_gate_error(err) {
            eprint!("{}", upgrade_cta::format_upgrade_cta(gate_err));
            return;
        }
    }
    eprintln!("Error: {err:#}");
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let repo_root = std::env::current_dir()?;

    let tui_mode = matches!(cli.command, Commands::Tui { .. });
    let _tracing_guard = init_tracing(tui_mode, &repo_root)?;

    match cli.command {
        // ... (entire existing match block, unchanged) ...
    }
}
```

The body of the existing `match cli.command { … }` block moves
verbatim into `run()`. No semantic changes inside the match arms.

### Acceptance for Task 1

- [ ] `crates/spur-cli/src/upgrade_cta.rs` exists with the two `pub(crate)`
      functions
- [ ] `main.rs::main` is the thin shim; `run()` holds the prior body
- [ ] `render_top_level_error` is private to `main.rs` (not exported)
- [ ] `std::io::stderr().is_terminal()` is the TTY gate (no `atty` crate)
- [ ] Workspace builds clean: `scripts/spur-cargo build -p spur-cli`
- [ ] No clippy warnings: `scripts/spur-cargo clippy -p spur-cli -- -D warnings`
- [ ] No fmt diff: `scripts/spur-cargo fmt -p spur-cli -- --check`

---

## Task 2: Add tests for the CTA renderer

**Worker assignment:** claude-code (implementer). Reviewers: kimi,
gemini, claude-code (3 gates in parallel).

### Subtask 2a: Unit test for the renderer

Create `crates/spur-cli/tests/upgrade_cta_render.rs`:

```rust
//! Plan C Tier 1 — unit test for the FeatureGateError CTA renderer.
//! Tests the formatter directly without touching the TTY gate.
//!
//! Note: `upgrade_cta` is `pub(crate)` so this test lives at the
//! crate boundary (cannot import private items from the binary
//! `main.rs` directly). The expected workaround is to call into
//! the helper through a public re-export OR to test via the
//! binary-level path. Since `spur-cli` exposes a `lib.rs`, the
//! cleanest path is to make `upgrade_cta` reachable from the lib
//! target. Workers should resolve this in Task 1's implementation
//! by adding `pub(crate) mod upgrade_cta;` to lib.rs as well, or
//! by exposing the formatter as a `pub(crate)` symbol that
//! integration tests can reach via the lib target.
//!
//! Concrete shape: ensure the renderer output names the key,
//! references `spur auth status` and `spur auth login`, and
//! mentions logout-recovery for tampered tiers.

#![cfg(unix)]

use spur_cli::upgrade_cta::{find_gate_error, format_upgrade_cta};
use spur_license::{FeatureGateError, FeatureKey, Tier};

#[test]
fn cta_names_the_denied_key() {
    let err = FeatureGateError::Denied {
        key: FeatureKey::CLI_CORE_EXEC,
        tier: Tier::Community,
    };
    let out = format_upgrade_cta(&err);
    assert!(out.contains("cli_core_exec"), "CTA must name key: {out}");
}

#[test]
fn cta_lists_recovery_affordances() {
    let err = FeatureGateError::Denied {
        key: FeatureKey::CLI_CORE_RUN,
        tier: Tier::Community,
    };
    let out = format_upgrade_cta(&err);
    assert!(
        out.contains("spur auth status"),
        "CTA must mention status: {out}"
    );
    assert!(
        out.contains("spur auth login --key"),
        "CTA must mention login: {out}"
    );
    assert!(
        out.contains("spur auth logout"),
        "CTA must mention logout for tampered-tier recovery: {out}"
    );
}

#[test]
fn find_gate_error_returns_some_when_root_is_gate_error() {
    let err = anyhow::Error::from(FeatureGateError::Denied {
        key: FeatureKey::CLI_CORE_INIT,
        tier: Tier::Community,
    });
    assert!(
        find_gate_error(&err).is_some(),
        "must find gate error at root"
    );
}

#[test]
fn find_gate_error_walks_anyhow_context_chain() {
    let err = anyhow::Error::from(FeatureGateError::Denied {
        key: FeatureKey::CLI_CORE_TUI,
        tier: Tier::Community,
    })
    .context("while preparing TUI startup");
    assert!(
        find_gate_error(&err).is_some(),
        "must find gate error through .context() wrap"
    );
}

#[test]
fn find_gate_error_returns_none_for_unrelated_anyhow_error() {
    let err = anyhow::anyhow!("totally unrelated I/O failure");
    assert!(find_gate_error(&err).is_none());
}
```

### Subtask 2b: Add binary-level CTA assertion to `cli_core_gate_e2e.rs`

Append to `crates/spur-cli/tests/cli_core_gate_e2e.rs`:

```rust
#[test]
fn spur_exec_under_stripped_key_renders_upgrade_cta() {
    // Plan C Tier 1 — binary-level proof that denied gates render
    // the structured CTA via `render_top_level_error`.
    //
    // We strip `cli_core_exec` via the SPUR_LICENSE_TEST_STRIP_KEYS
    // hook, run `spur exec`, and assert stderr contains:
    // - the typed-error key name
    // - the `spur auth status` recovery line
    // - the `spur auth login` recovery line
    //
    // Note: `assert_cmd::Command` does not allocate a TTY for the
    // child, so `is_terminal()` returns false in the spawn and the
    // CTA path is bypassed in favor of the plain `Error: {err:#}`
    // output. To exercise the CTA path under assert_cmd, we set
    // `CLICOLOR_FORCE=1` ... actually this doesn't work because
    // IsTerminal checks the underlying fd, not env.
    //
    // RESOLUTION: the binary-level assertion checks only that the
    // typed-error key name reaches stderr (which works in both
    // CTA-enabled and CTA-disabled paths). The full CTA shape is
    // covered by the unit test in `upgrade_cta_render.rs`. This
    // mirrors the M0/M0.5 split (helper-level for shape, binary
    // for wiring).
    let assert = Command::cargo_bin("spur")
        .expect("spur binary builds")
        .env("SPUR_LICENSE_TEST_STRIP_KEYS", "cli_core_exec")
        .env_remove("SPUR_LICENSE_DEV_PLAN")
        .args(["exec", "--agent", "claude-code", "irrelevant-task"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("cli_core_exec"),
        "stderr must name the denied key, got:\n{stderr}",
    );
}
```

### Acceptance for Task 2

- [ ] 5 unit tests in `tests/upgrade_cta_render.rs` (name-the-key,
      lists-recovery, root-downcast, context-chain-walk, unrelated-
      error-returns-none)
- [ ] 1 binary-level test in `tests/cli_core_gate_e2e.rs` adding
      a stripped-key assertion for `spur exec`
- [ ] All tests pass: `scripts/spur-cargo test -p spur-cli`
- [ ] No clippy warnings; no fmt diff

---

## Final sweep (judge-only, not delegated)

After Task 1 and Task 2 both pass their 3-gate review and merge:

- [ ] `scripts/spur-cargo build --workspace`
- [ ] `scripts/spur-cargo test -p spur-license -p spur-cli`
- [ ] `scripts/spur-cargo clippy -p spur-license -p spur-cli --tests -- -D warnings`
- [ ] `scripts/spur-cargo fmt -p spur-license -p spur-cli -- --check`
- [ ] Manually verify under `SPUR_LICENSE_TEST_STRIP_KEYS=cli_core_exec` that
      `cargo run -p spur-cli -- exec --agent foo --task bar` renders the CTA
      on a real TTY (the assert_cmd path can't validate this directly)
- [ ] Total commits expected: 3 (impl, tests, possibly Cargo.lock if any)

## Acceptance criteria for Tier 1 as a whole

- [ ] Every CLI-mode `FeatureGateError` denial that escapes through
      anyhow renders the structured CTA when stderr is a TTY
- [ ] Piped/scripted output (non-TTY stderr) still uses the plain
      `Error: {err:#}` path so tooling parsing isn't broken
- [ ] Anyhow `.context(...)`-wrapped gate errors are still detected
      via `chain()` walk, not just root downcast
- [ ] Unit tests cover renderer output + chain-walk semantics
- [ ] Binary-level test proves the wired path through clap dispatch
- [ ] No regressions in M0 / M0.5 acceptance criteria

## Out of scope for Tier 1 (deferred)

- **Per-key user-facing labels** (e.g. translating `cli_core_exec`
  → "the `spur exec` subcommand"). Tier 2 work — needs a registry
  table or `FeatureKey::user_facing_label()` method.
- **Tier-aware CTA branching** (different copy for Community vs
  tampered-Pro vs trial-expired). Tier 1 ships generic copy that
  works for all tiers. Tier 3 (trial JWT) refines.
- **TUI modal rendering of denials**. Tier 2.
- **JSON-formatted error output for tooling**. Future `--output
  json` flag work, not Tier 1.
- **URL CTAs** (e.g. `https://getspur.com/pricing`). Needs product
  authority to commit canonical URLs. Tier 1 ships text-only
  affordances using existing `spur auth …` subcommands.

## Self-review (pre-dispatch checklist)

- [x] Spec coverage: every gate-fire site already uses `?` to
      bubble `FeatureGateError` via anyhow's `From` impl, so the
      chain-walk lookup catches all of them
- [x] No new deps; `IsTerminal` is in std since 1.70
- [x] Test isolation: `SPUR_LICENSE_TEST_STRIP_KEYS` fixture pattern
      reused; both new test paths strip `SPUR_LICENSE_DEV_PLAN`
      defensively
- [x] No URLs (avoids product-authority gap)
- [x] No per-key labels (defers Tier 2 dependency)
- [x] TTY gate via std (no atty crate)
- [x] Anyhow chain-walk (defensive against `.context(...)` wrapping)
- [x] `pub(crate)` visibility (no public API surface from spur-cli)

## Note on lib-target visibility

Task 1's `mod upgrade_cta;` declares the module in `main.rs`. To
allow integration tests in `tests/upgrade_cta_render.rs` to reach
the helpers, the implementer should ALSO declare `pub(crate) mod
upgrade_cta;` in `lib.rs` (which the crate already exports for
test use — see `Cargo.toml [lib]` block). The `pub(crate)`
visibility means external consumers of `spur_cli` can't reach the
helpers, but integration tests inside the crate can.

If the worker prefers to keep the module strictly bin-private,
the alternative is to test the renderer via the binary-level
`assert_cmd` path only (no unit test). The plan recommends the
lib-export approach because (a) it gives faster failure isolation
in CI and (b) the unit tests don't need a child-process spawn.

---

## Post-merge addendum (2026-04-28)

The 3-gate review (kimi + gemini + claude-code on v1; codex deep +
gemini side-by-side on v2) drove four intentional deviations from
the v1 prescription above. All deltas are net-positive for Tier 2 /
Tier 3 reuse.

**Canonical landed shape:**

| Concern | v1 prescription | v2 landed | Driver |
|---|---|---|---|
| Renderer crate | `crates/spur-cli/src/upgrade_cta.rs` | `crates/spur-license/src/upgrade_cta.rs` | gemini 🔴: `spur-cli` is a binary crate; future `spur-tui` capability-tease modal would hit a circular dep. Renderer lives next to `FeatureGateError` typed-error contract for cross-crate reuse. |
| Function visibility | `pub(crate)` | `pub` | kimi 🔴: integration tests compile as external crates; `pub(crate)` blocks them. `pub` exposes via `spur_license::upgrade_cta::{find_gate_error, format_upgrade_cta}`. |
| Exit pattern | `std::process::exit(1)` | `ExitCode::FAILURE` from `main` | gemini 🟡: idiomatic Rust; allows tokio runtime to drain cleanly. |
| Dead-code block | `if let FeatureGateError::Denied { key, tier } = ... { let _ = ... }` | dropped | gemini 🟡: doc-comment on `format_upgrade_cta` carries the future-tier-aware-copy hint without silent code. |
| Unit test location | `crates/spur-cli/tests/upgrade_cta_render.rs` | `crates/spur-license/src/upgrade_cta.rs::tests` (cfg(test)) | Ripple of moving the renderer to `spur-license`. Tests live with the code. |

**Out of scope for Tier 1, all honored:**
- ✅ No per-key user-facing labels (Tier 2)
- ✅ No tier-aware copy branching (Tier 3)
- ✅ No TUI modal rendering (Tier 2)
- ✅ No JSON output (deferred future work)
- ✅ No URL CTAs (no product authority)

**Open follow-ups surfaced by post-merge review (codex):**

1. **Doc-comment tightening** (trivial): `render_top_level_error`'s
   doc says `{err:#}` provides "full debug context" — actually
   only the Display chain. Tighten language. Lands in same commit
   as this addendum.
2. **`SPUR_FORCE_TTY` test hook** (testability gap): `assert_cmd`
   does not allocate a pty for the child, so the TTY-gated CTA
   dispatch path has no automated regression net. Manual pty
   verification was performed. Filed as
   `2026-04-28-tier-revamp-tier1-followup-tty-test-hook.md`.
3. **Foundation-claim caveat**: `format_upgrade_cta -> String` is
   sufficient for Tier 1 (CLI eprint!) and Tier 2 (TUI Paragraph
   widget). Tier 3 (trial JWT) may want a structured
   `Cta { lines: Vec<Line>, actions: Vec<Action>, trial_available: bool }`
   instead. YAGNI for now; refactor when Tier 3 actually needs it.

**Foundation API stable for Tier 2 / Tier 3:**

```rust
use spur_license::upgrade_cta::{find_gate_error, format_upgrade_cta};

// At any error-rendering boundary:
if let Some(gate_err) = find_gate_error(&anyhow_err) {
    let cta_text = format_upgrade_cta(gate_err);
    // Tier 1: eprint!("{cta_text}")
    // Tier 2: render_modal(Paragraph::new(cta_text))
    // Tier 3: format_upgrade_cta_with_trial(gate_err, trial_state)
}
```

Plan doc preserved as audit trail above; this addendum is the
canonical reference for Tier 2 / Tier 3 implementers.

**Tier 2 has now landed** (commits f5cf3a87 / af0ae021 / 13fb4740 +
cleanup commit on top, rebased onto current main): TUI capability-tease
modal, MVP gate site at `Action::SendMessage`, and the `SPUR_FORCE_TTY`
test hook that closes follow-up #2 above. See
`docs/superpowers/plans/2026-04-28-tier-revamp-plan-c-tier2-tui-upgrade-modal.md`
for the Tier 2 plan + post-merge addendum.
