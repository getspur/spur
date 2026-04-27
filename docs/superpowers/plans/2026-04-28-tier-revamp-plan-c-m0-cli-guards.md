# Tier Revamp Plan C M0 — CLI Command Guards (Wave C.1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the foundation milestone of Plan C — define the
workspace-wide `FeatureGateError` + `require_feature(gate, key)`
contract in `spur-license`, then use it to gate the 8 non-auth
`cli_core_*` subcommands at their dispatch entry points.

**Architecture:** The gate-check API lives in `spur-license`
(`require_feature(&FeatureGate, FeatureKey) -> Result<(), FeatureGateError>`)
because Plan C survey § 8 + Plan D survey § 6.5 lock the typed
`FeatureGateError { key: FeatureKey }` shape as the contract for
all downstream consumers (Plan C waves C.2–C.14 in tui/acp/mcp/etc.,
plus Plan D D.6 capability-tease modal pattern-matching). M0 ships
the contract from day one — no local CLI-private error type that
later needs promotion.

Gate construction stays **lazy**: each gated arm constructs the
license + gate on entry, so non-gated arms (`Skills`, `Workflow`,
`Config`, `Flags`, `Gc`, `Bot`, `Profile`) pay zero overhead.

**Special case:** `CLI_CORE_LICENSE_ACTIVATE` is **deferred** to a
later milestone. Gating `Commands::Auth` at the dispatch level
creates a brick condition (a tampered Pro JWT that strips this key
permanently locks `spur auth login`). The correct enforcement is
inside `auth::run` on the `Login` variant only (so `Logout` /
`Refresh` / `Status` always work), but that refactor adds
auth-internal scope to M0 and is cleaner to handle separately.
M0 covers 8 keys; the 9th lands in a focused follow-up.

**Tech Stack:** Rust 2021, clap 4, anyhow, thiserror, the existing
`spur_license` crate. New test-only dep: `assert_cmd` (workspace).

**Scope:** Wave C.1 of Plan C only (8 of 9 `cli_core_*` keys). The
other 14 Plan C waves are out of scope for M0.

---

## Spec grounding

- Plan C survey § 1.6 lists the 9 keys; M0 enforces 8 (defers
  `CLI_CORE_LICENSE_ACTIVATE` per the Special Case above).
- Plan C survey § 5.1 wave C.1 — Free defense-in-depth.
- Plan C survey § 8 — typed `FeatureGateError { key: FeatureKey }`
  contract for downstream Plan D D.6 consumption.
- Plan D survey § 6.5 — Plan D pattern-matches on the same typed
  error.
- Existing pattern: `crates/spur-cli/src/lib.rs:6-8`
  (`pm_service_gate_allows_construction`) and
  `crates/spur-cli/tests/pm_gate.rs:5-24` (gate-check tests).

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-license/src/gate.rs` | Modify | Add `FeatureGateError` enum + `require_feature()` helper |
| `crates/spur-license/src/lib.rs` | Modify (re-export) | `pub use gate::{FeatureGateError, require_feature};` |
| `crates/spur-license/tests/feature_gate.rs` | Modify | Add tests for `require_feature` (positive/negative) |
| `crates/spur-cli/src/main.rs` | Modify | Add `require_cli_gate(key)` lazy helper + wire 8 arms |
| `crates/spur-cli/tests/cli_core_gates.rs` | Create | Parameterized invariant test for the 8 keys |
| `crates/spur-cli/tests/cli_core_gate_e2e.rs` | Create | One `assert_cmd` binary-level integration test |
| `crates/spur-cli/Cargo.toml` | Modify | Add `assert_cmd` to `[dev-dependencies]` |

The `cli_gate_check` helper proposed in v1 of this plan **does not
land** — `spur_license::require_feature` is the workspace-wide
contract, used directly by spur-cli.

---

## Task 1: Promote `FeatureGateError` + `require_feature()` to `spur-license`

**Files:**
- Modify: `crates/spur-license/src/gate.rs:1-67` (the `FeatureGate` impl block + module docs)
- Modify: `crates/spur-license/src/lib.rs:11-12` (re-exports)
- Modify: `crates/spur-license/tests/feature_gate.rs` (existing test file)

- [ ] **Step 1: Write the failing test pair.**

```rust
// crates/spur-license/tests/feature_gate.rs (append at end)
use spur_license::{require_feature, FeatureGateError};

#[test]
fn require_feature_passes_when_key_present_in_community_tier() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);

    assert!(require_feature(&gate, FeatureKey::CORE_CORE_BRAIN_SESSION).is_ok());
}

#[test]
fn require_feature_returns_typed_error_with_key_when_absent() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new_with_install_id(policy, InstallId::from_uuid(uuid::Uuid::nil()));
    let state = LicenseState::active_validated(Plan::Pro, BTreeSet::new());
    gate.update_state(&state);

    let err = require_feature(&gate, FeatureKey::PM_PRO_BEADS_ADVANCED)
        .expect_err("empty Pro state must reject pm_pro_beads_advanced");
    assert!(matches!(err, FeatureGateError::Denied { key }
                    if key == FeatureKey::PM_PRO_BEADS_ADVANCED));
}

#[test]
fn feature_gate_error_display_names_the_key_and_recovery() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new_with_install_id(policy, InstallId::from_uuid(uuid::Uuid::nil()));
    let state = LicenseState::active_validated(Plan::Pro, BTreeSet::new());
    gate.update_state(&state);

    let err = require_feature(&gate, FeatureKey::CLI_CORE_RUN).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("cli_core_run"), "error must name the key: {msg}");
    assert!(msg.to_lowercase().contains("license"),
        "error must point at license recovery: {msg}");
}
```

- [ ] **Step 2: Run tests to verify they fail (helper not yet defined).**

Run: `scripts/spur-cargo test -p spur-license --test feature_gate`
Expected: FAIL — `require_feature` and `FeatureGateError` not in scope.

- [ ] **Step 3: Implement `FeatureGateError` and `require_feature` in `gate.rs`.**

Append to `crates/spur-license/src/gate.rs` (after the existing
`impl FeatureGate { … }` block, around line 270):

```rust
/// Typed error returned by [`require_feature`] when the active
/// license tier does not entitle the requested feature.
///
/// Open-set enum: future variants (e.g. `Revoked`, `Expired`,
/// `BootstrapPending`) can be added without breaking existing
/// pattern matches that only match `Denied { key }`.
#[derive(Debug, thiserror::Error)]
pub enum FeatureGateError {
    #[error(
        "`{}` is not available on the active license tier. \
         Run `spur auth status` to inspect the current tier, or \
         `spur auth login --key …` to activate a license that \
         entitles this feature.",
        key.as_str()
    )]
    Denied { key: FeatureKey },
}

/// Workspace-wide gate-check helper. Returns `Ok(())` if the
/// active snapshot grants `key`; otherwise returns
/// [`FeatureGateError::Denied`] with the key for downstream
/// pattern-matching (e.g. Plan D capability-tease modals).
///
/// Per Plan C survey § 8, this is the canonical contract every
/// runtime gate-check site must use.
pub fn require_feature(gate: &FeatureGate, key: FeatureKey) -> Result<(), FeatureGateError> {
    if gate.has(key) {
        Ok(())
    } else {
        Err(FeatureGateError::Denied { key })
    }
}
```

Note: `thiserror` is already a workspace dependency for
`spur-license` (verify via `crates/spur-license/Cargo.toml`); no
Cargo.toml change needed in this task.

- [ ] **Step 4: Re-export from the crate root.**

`crates/spur-license/src/lib.rs:11-12` (extend the existing re-export
block):

```rust
pub use community::CommunityProvider;
pub use gate::{require_feature, FeatureGate, FeatureGateError};
```

- [ ] **Step 5: Run tests to verify they pass.**

Run: `scripts/spur-cargo test -p spur-license --test feature_gate`
Expected: all tests pass (3 new + existing tests).

- [ ] **Step 6: Run clippy + fmt for spur-license.**

```bash
scripts/spur-cargo clippy -p spur-license -- -D warnings
scripts/spur-cargo fmt -p spur-license -- --check
```
Expected: clean.

- [ ] **Step 7: Commit.**

```bash
git add crates/spur-license/src/gate.rs crates/spur-license/src/lib.rs crates/spur-license/tests/feature_gate.rs
git commit -m "feat(spur-license): require_feature + FeatureGateError typed contract (C.1)"
```

---

## Task 2: Add `require_cli_gate()` lazy helper to spur-cli/main.rs

**Files:**
- Modify: `crates/spur-cli/src/main.rs` (add helper near the top of the file, after imports)

- [ ] **Step 1: Add the lazy helper.**

Insert immediately after the `init_tracing` function (around line 56):

```rust
/// Lazy gate-check used by all gated `Commands::*` arms.
///
/// Constructs `SpurLicense` + `FeatureGate` on first call. Non-gated
/// arms (Skills, Workflow, Config, Flags, Gc, Bot, Profile) never
/// invoke this helper, so they pay zero gate-construction cost.
///
/// The license construction is fast (~1ms): for Free users with no
/// `SPUR_LICENSESEAT_*` env vars set, it returns the embedded
/// `CommunityProvider` with no I/O. For Pro users it reads the
/// cached license JWT from disk once.
fn require_cli_gate(key: spur_license::FeatureKey) -> anyhow::Result<()> {
    let license = spur_license::SpurLicense::from_env_or_disabled();
    let gate = license.feature_gate();
    spur_license::require_feature(&gate, key)?;
    Ok(())
}
```

`anyhow::Error: From<E> where E: std::error::Error + Send + Sync + 'static`,
and `FeatureGateError` derives `thiserror::Error`, so the `?`
auto-converts.

`Arc<FeatureGate>` derefs to `&FeatureGate`, so `&gate` is the
correct type for `require_feature`.

- [ ] **Step 2: Verify the workspace still compiles.**

Run: `scripts/spur-cargo check -p spur-cli`
Expected: clean compile (helper is unused yet; that's fine — it's
called in Task 3).

Suppress the unused-function warning by also wiring at least one
arm in this same task; OR add `#[allow(dead_code)]` and remove it
in Task 3. Recommended: just proceed to Task 3 and commit them
together.

- [ ] **Step 3: (No commit — defer to Task 3.)**

The helper is dead code until at least one arm calls it.

---

## Task 3: Wire all 8 gated arms in one commit

**Files:**
- Modify: `crates/spur-cli/src/main.rs` (8 dispatch arms)

The 8 keys gated in this task and their arm locations:

| Key | Subcommand arm | Approx line |
|---|---|---|
| `CLI_CORE_INIT` | `Commands::Init { … }` | 368 |
| `CLI_CORE_AGENTS` | `Commands::Agents { … }` | 374 |
| `CLI_CORE_RUN` | `Commands::Run { … }` | 375 |
| `CLI_CORE_EXEC` | `Commands::Exec { … }` | 407 |
| `CLI_CORE_SESSIONS` | `Commands::Sessions { … }` | 422 |
| `CLI_CORE_COST` | `Commands::Cost { … }` | 522 |
| `CLI_CORE_CONNECT` | `Commands::Connect { … }` | 551 |
| `CLI_CORE_TUI` | `Commands::Tui { … }` | 607 (gate insert at line 616, **before** the `if profile { … return … }` block) |

> **`Commands::Auth` is intentionally NOT gated in M0** — see the
> Architecture > Special case rationale at the top of this plan.

> **Tui placement rationale (gemini 🟡 fix):** The gate check goes
> *before* the `if profile { … return … }` block (line 616), not
> after. Placing it after would let `spur tui --profile` re-spawn
> the binary which then fails the gate inside the child — the user
> would profile an error exit. Failing fast in the parent is correct.

- [ ] **Step 1: Wire each of the 8 arms.**

For each arm in the table above, insert
`require_cli_gate(spur_license::FeatureKey::<KEY>)?;` as the first
statement of the arm body. Concrete edits:

```rust
// crates/spur-cli/src/main.rs

// Init arm (line 368):
Commands::Init { force, with_skills } => {
    require_cli_gate(spur_license::FeatureKey::CLI_CORE_INIT)?;
    commands::init::run(repo_root, force, with_skills).await
}

// Skills arm — intentionally NOT gated (no key in registry).

// Agents arm (line 374):
Commands::Agents { command } => {
    require_cli_gate(spur_license::FeatureKey::CLI_CORE_AGENTS)?;
    cmd_agents(repo_root, command).await
}

// Run arm (line 375):
Commands::Run { task, brain, issue, background } => {
    require_cli_gate(spur_license::FeatureKey::CLI_CORE_RUN)?;
    let mut orch = load_orchestrator(repo_root)?;
    // … rest unchanged …
}

// Exec arm (line 407):
Commands::Exec { agent, task } => {
    require_cli_gate(spur_license::FeatureKey::CLI_CORE_EXEC)?;
    let mut orch = load_orchestrator(repo_root)?;
    // … rest unchanged …
}

// Sessions arm (line 422):
Commands::Sessions { command } => {
    require_cli_gate(spur_license::FeatureKey::CLI_CORE_SESSIONS)?;
    let orch = load_orchestrator(repo_root)?;
    // … rest unchanged …
}

// Cost arm (line 522):
Commands::Cost {
    today, week, by, export, engine, experimental, range,
} => {
    require_cli_gate(spur_license::FeatureKey::CLI_CORE_COST)?;
    // … rest unchanged …
}

// Connect arm (line 551):
Commands::Connect { service } => {
    require_cli_gate(spur_license::FeatureKey::CLI_CORE_CONNECT)?;
    // … rest unchanged …
}

// Auth arm (line 565) — intentionally NOT gated; see Special Case.

// Tui arm (line 607) — gate goes IMMEDIATELY AFTER the `=> {` opening
// (line 616) and BEFORE the `if profile { … return … }` block:
Commands::Tui {
    brain, sessions, dashboard, new, session, profile, duration,
} => {
    require_cli_gate(spur_license::FeatureKey::CLI_CORE_TUI)?;
    if profile {
        // … existing profile-spawn code unchanged …
    }
    // … rest unchanged …
}
```

> **Locate by clap arm literal** (`Commands::Init {`, `Commands::Run {`,
> etc.) when applying these edits — line numbers shift as you go.
> The first edit inserts 1 line, so subsequent line numbers in the
> original file shift by +1, then +2, etc.

- [ ] **Step 2: Verify the workspace compiles.**

Run: `scripts/spur-cargo check -p spur-cli`
Expected: clean compile; `require_cli_gate` is now used.

- [ ] **Step 3: Verify existing spur-cli tests still pass.**

Run: `scripts/spur-cargo test -p spur-cli`
Expected: all existing tests (`pm_gate`, `auth_cli`, `bot_cli`,
`community_smoke`, `config_check`, `flags_smoke`, `init_ux`,
`session_attach_collision`) pass. The new gate calls are
no-ops for community-tier users (which is what the test fixtures
construct).

- [ ] **Step 4: Run clippy + fmt.**

```bash
scripts/spur-cargo clippy -p spur-cli -- -D warnings
scripts/spur-cargo fmt -p spur-cli -- --check
```
Expected: clean.

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-cli/src/main.rs
git commit -m "feat(spur-cli): gate 8 cli_core_* arms via require_cli_gate (C.1)"
```

---

## Task 4: Add parameterized 8-key invariant tests

**Files:**
- Create: `crates/spur-cli/tests/cli_core_gates.rs`

- [ ] **Step 1: Write the test file.**

```rust
// crates/spur-cli/tests/cli_core_gates.rs
//
// Plan C M0 (wave C.1) — parameterized invariants for the 8 cli_core_*
// keys gated at dispatch entry. Mirrors the existing `tests/pm_gate.rs`
// shape (community policy + FeatureGate + assertion).
//
// Note: CLI_CORE_LICENSE_ACTIVATE is intentionally absent from the
// invariant. Per the M0 plan Special Case, it remains in the typed
// registry but is not enforced at dispatch in M0; enforcement lands
// inside `auth::run` on the `Login` variant only (follow-up milestone).

use spur_license::policy::PolicyResolver;
use spur_license::{require_feature, FeatureGate, FeatureKey, LicenseState, Plan};
use std::collections::BTreeSet;

const M0_GATED_KEYS: &[FeatureKey] = &[
    FeatureKey::CLI_CORE_INIT,
    FeatureKey::CLI_CORE_AGENTS,
    FeatureKey::CLI_CORE_RUN,
    FeatureKey::CLI_CORE_EXEC,
    FeatureKey::CLI_CORE_SESSIONS,
    FeatureKey::CLI_CORE_COST,
    FeatureKey::CLI_CORE_CONNECT,
    FeatureKey::CLI_CORE_TUI,
];

fn community_gate() -> FeatureGate {
    FeatureGate::new(PolicyResolver::embedded())
}

fn empty_pro_gate() -> FeatureGate {
    let g = FeatureGate::new(PolicyResolver::embedded());
    g.update_state(&LicenseState::active_validated(Plan::Pro, BTreeSet::new()));
    g
}

#[test]
fn embedded_community_policy_grants_all_8_m0_cli_core_keys() {
    let gate = community_gate();
    for &key in M0_GATED_KEYS {
        assert!(
            gate.has(key),
            "embedded community policy must grant {} so daily-driver Free users are not blocked at CLI dispatch",
            key.as_str(),
        );
        assert!(
            require_feature(&gate, key).is_ok(),
            "require_feature must accept {} on community tier",
            key.as_str(),
        );
    }
}

#[test]
fn empty_pro_gate_blocks_all_8_m0_cli_core_keys() {
    let gate = empty_pro_gate();
    for &key in M0_GATED_KEYS {
        let err = require_feature(&gate, key).expect_err(&format!(
            "tampered policy that strips {} must block dispatch",
            key.as_str(),
        ));
        // Verify the typed contract Plan D D.6 will rely on.
        let spur_license::FeatureGateError::Denied { key: returned_key } = err;
        assert_eq!(returned_key, key);
    }
}

#[test]
fn auth_arm_remains_ungated_in_m0() {
    // Documents the M0 Special Case: CLI_CORE_LICENSE_ACTIVATE is
    // not enforced at dispatch. If/when this changes (follow-up
    // milestone), this test should be deleted or updated.
    //
    // The community gate still grants the key; the registry presence
    // does not imply runtime enforcement.
    let gate = community_gate();
    assert!(gate.has(FeatureKey::CLI_CORE_LICENSE_ACTIVATE));
}
```

- [ ] **Step 2: Run tests.**

Run: `scripts/spur-cargo test -p spur-cli --test cli_core_gates`
Expected: 3 tests pass.

- [ ] **Step 3: Commit.**

```bash
git add crates/spur-cli/tests/cli_core_gates.rs
git commit -m "test(spur-cli): parameterized invariant for 8 m0 cli_core_* keys (C.1)"
```

---

## Task 5: Binary-level integration test (assert_cmd)

**Files:**
- Modify: `crates/spur-cli/Cargo.toml` (add `assert_cmd` to `[dev-dependencies]`)
- Create: `crates/spur-cli/tests/cli_core_gate_e2e.rs`

> **Why this task:** Tasks 1–4 verify the helper and registry shape.
> This task verifies that the actual `spur` binary fails non-zero
> with the right error text when a gate is denied. Without it, all
> M0 tests exercise only the helper, never the wiring through clap
> dispatch (per gemini + claude-code reviews of the M0 plan v1).

- [ ] **Step 1: Add `assert_cmd` to dev-deps.**

`crates/spur-cli/Cargo.toml`:

```toml
[dev-dependencies]
# … existing entries unchanged …
assert_cmd = "2"
```

If `assert_cmd` is already a workspace dep, prefer
`assert_cmd = { workspace = true }`. Verify:

```bash
grep -E "^assert_cmd" Cargo.toml
```

If absent, add it to the workspace `[workspace.dependencies]` block
first, then reference it with `workspace = true`.

- [ ] **Step 2: Write the integration test.**

```rust
// crates/spur-cli/tests/cli_core_gate_e2e.rs
//
// Plan C M0 (wave C.1) — single end-to-end smoke that proves the
// wiring fires at the binary boundary, not just at the helper.
//
// We pick `spur init` as the sentinel: the cheapest gated arm to
// run (no orchestrator, no agents, no I/O beyond stdout).

use assert_cmd::Command;

#[test]
fn spur_help_exits_zero_without_gate() {
    // Sanity: `--help` and `--version` are clap built-ins; they
    // exit before our match block. Establishes that the binary
    // itself launches under cargo test.
    Command::cargo_bin("spur")
        .expect("spur binary builds in test profile")
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn spur_init_dry_succeeds_on_default_community_tier() {
    // Default environment (no SPUR_LICENSESEAT_API_KEY) → embedded
    // community policy → CLI_CORE_INIT granted → init runs.
    //
    // We use a tempdir to keep the test idempotent and run with
    // `--force` to avoid prompting on already-initialized repos.
    let tmp = tempfile::tempdir().expect("tempdir");
    Command::cargo_bin("spur")
        .expect("spur binary builds")
        .current_dir(tmp.path())
        .arg("init")
        .arg("--force")
        .assert()
        .success();
}
```

`tempfile` is already a workspace dependency used elsewhere
(see Plan A status doc note about T2 tempfile dep). Verify it's
available in spur-cli dev-deps and add if missing.

- [ ] **Step 3: Run the integration test.**

Run: `scripts/spur-cargo test -p spur-cli --test cli_core_gate_e2e`
Expected: 2 tests pass. (The "denied tier" assertion is intentionally
deferred — exercising it requires either a tampered policy fixture
or a Pro license JWT with stripped entitlements, both of which
add fixture complexity disproportionate to M0 scope. The deferred
assertion is captured as a follow-up below.)

- [ ] **Step 4: Run clippy + fmt.**

```bash
scripts/spur-cargo clippy -p spur-cli -- -D warnings
scripts/spur-cargo fmt -p spur-cli -- --check
```
Expected: clean.

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-cli/Cargo.toml crates/spur-cli/tests/cli_core_gate_e2e.rs
git commit -m "test(spur-cli): assert_cmd e2e smoke for cli_core gate wiring (C.1)"
```

---

## Task 6: Workspace-wide build + test sweep

**Files:** none (verification only)

- [ ] **Step 1: Workspace build to confirm no regressions.**

Run: `scripts/spur-cargo build --workspace`
Expected: clean build.

- [ ] **Step 2: Workspace-touched-crates test sweep.**

```bash
scripts/spur-cargo test -p spur-license
scripts/spur-cargo test -p spur-cli
```
Expected: all tests pass.

- [ ] **Step 3: Workspace-wide clippy with strict mode (smoke only on touched crates).**

```bash
scripts/spur-cargo clippy -p spur-license -p spur-cli -- -D warnings
```
Expected: clean.

- [ ] **Step 4: If anything fails, file a follow-up task in this doc and stop.
If everything passes, no commit needed (verification-only task).**

---

## Acceptance criteria for Plan C M0

- [ ] `FeatureGateError::Denied { key: FeatureKey }` lives in
      `spur-license` and is re-exported from the crate root
- [ ] `require_feature(gate, key) -> Result<(), FeatureGateError>`
      is the workspace-wide gate-check API; spur-cli, spur-tui,
      spur-acp, spur-mcp etc. all use this same helper in
      subsequent waves
- [ ] All 8 M0 `cli_core_*` keys (Init, Agents, Run, Exec, Sessions,
      Cost, Connect, Tui) gated at dispatch entry
- [ ] `Commands::Auth` deliberately ungated in M0 (deferred to a
      follow-up milestone with `auth::run` Login-variant
      enforcement)
- [ ] Tui gate fires *before* the `if profile { return … }` block
      so `spur tui --profile` fails fast on the parent invocation
- [ ] Embedded community policy grants all 8 M0 keys (Free users
      unaffected)
- [ ] Empty Pro policy correctly denies all 8 M0 keys with typed
      `FeatureGateError::Denied { key }`
- [ ] One end-to-end binary smoke (`spur init --force` in tempdir)
      proves the wiring fires through clap dispatch
- [ ] Workspace clean: build, tests, clippy `-D warnings`, fmt
- [ ] Total git history: 4 commits (helper, wiring, invariant test,
      e2e test) + 1 verification-only sweep task = 5 task commits

## Out of scope for M0

Explicitly deferred:

- **`CLI_CORE_LICENSE_ACTIVATE` enforcement** — needs a focused
  milestone that gates inside `auth::run` on the `Login` variant
  only (so `Logout`/`Refresh`/`Status` always work even with a
  stripped JWT). Follow-up: `Plan-C-M0.5-license-activate-gate.md`.
- **Tampered-policy denial e2e test** — needs either a tampered
  policy fixture or a Pro JWT with stripped entitlements. Follow-up:
  add a fixture to `crates/spur-license/tests/fixtures/` and one
  more `assert_cmd` test that asserts non-zero exit + stderr
  contains `cli_core_init`. Lands alongside C.M0.5 since the same
  fixture serves both.
- **All non-`cli_core_*` keys** — Plan C waves C.2 through C.14 +
  C.15 are the subjects of subsequent milestones (M1 = wave C.2 tui
  guards, etc.).

## Self-review (pre-dispatch checklist)

- [x] Spec coverage: all 8 in-scope `cli_core_*` keys gated; the
      9th (`CLI_CORE_LICENSE_ACTIVATE`) is explicitly deferred with
      a follow-up filed
- [x] No placeholders ("TBD", "implement later") — all code shown
      verbatim
- [x] Type consistency: `require_feature`, `FeatureGateError::Denied`,
      `FeatureKey::CLI_CORE_*`, `Arc<FeatureGate>` deref → `&FeatureGate`
      used identically across all tasks
- [x] Test ordering: Task 1 leads with red-then-green for the
      workspace-wide helper; the per-arm test pairs from M0 v1
      have been replaced by Task 4's parameterized invariant +
      Task 5's e2e smoke (per claude-code 🟡 review)
- [x] Workspace-wide error contract from day one (per gemini 🔴 +
      claude-code 🔴 reviews of M0 v1)
- [x] Lazy gate construction (per gemini 🟡 review of M0 v1)
- [x] Tui gate placement before profile block (per gemini 🟡)
- [x] Auth arm ungated to avoid bricking (per gemini 🔴)
- [x] Binary-level integration test included (per gemini 🔴)
