# Tier Revamp Plan C M0.5 — `CLI_CORE_LICENSE_ACTIVATE` Enforcement

> **For agentic workers:** This plan is small (≈40 lines of code,
> 1 source file, 2 test files modified). Execute task-by-task with
> red-green TDD per existing M0 pattern.

**Goal:** Land the 9th `cli_core_*` key from M0's deferred backlog —
gate `CLI_CORE_LICENSE_ACTIVATE` inside `auth::run` on the `Login`
variant only — and produce the first true binary-level denial e2e
that proves a stripped-key `FeatureGate` exits the `spur` binary
non-zero with a typed error message at the boundary.

**Why M0 deferred this:** Gating `Commands::Auth { command }` at
the dispatch level creates a brick condition. A tampered Pro JWT
that strips `cli_core_license_activate` would permanently lock
`spur auth login`, leaving the user with no recovery path even
though `Logout` / `Refresh` / `Status` should always work. The
correct enforcement is *inside* `auth::run` on the `Login` arm
specifically.

**M0.5 closes that gap:**
- `Login` is gated → tampered tiers cannot escalate to higher tiers
  by replaying a license activation against a stripped policy.
- `Logout` / `Refresh` / `Status` are explicitly NOT gated → the
  brick path stays open. Users on a tampered tier can recover by
  calling `spur auth logout` (which falls back to the embedded
  community policy where `cli_core_license_activate` is granted).

**Tech Stack:** Rust 2021, anyhow, the existing `spur-license`
crate. Tests reuse the `FakeProvider` test-support scaffold and the
`assert_cmd` dev-dep added in M0 Task 5.

---

## Spec grounding

- Plan C M0 plan v3 (`2026-04-28-tier-revamp-plan-c-m0-cli-guards.md`)
  Architecture > Special case → defers `CLI_CORE_LICENSE_ACTIVATE`
  to "M0.5".
- Plan C survey § 1.6 — 9th of the 9 `cli_core_*` keys.
- `crates/spur-cli/src/commands/auth.rs:46-69` — the dispatch
  surface where the gate lands.
- `crates/spur-cli/tests/auth_fake_provider.rs:9-11` —
  `test_feature_gate()` helper already injects an arbitrary gate
  via `SpurLicense::from_provider`. M0.5 reuses this mechanism.
- M0 finding (committed in `5e67e1d2`): the `SPUR_LICENSE_DEV_PLAN`
  policy gap (filed in
  `2026-04-28-tier-revamp-policy-gap-enterprise-tier.md`) gives us
  a debug-build empty-tier fixture for binary-level denial tests.

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-cli/src/commands/auth.rs` | Modify | Gate `login_inner` on `CLI_CORE_LICENSE_ACTIVATE` before `activate(...)` |
| `crates/spur-cli/tests/auth_fake_provider.rs` | Modify | Add 4 unit tests: Login denial; Logout / Refresh / Status pass-through with empty-Pro gate |
| `crates/spur-cli/tests/cli_core_gate_e2e.rs` | Modify | Add 1 binary-level denial test using `SPUR_LICENSE_DEV_PLAN=enterprise` as the tampered fixture |

No fixture files. The "tampered policy" is an `empty_pro_gate()`
helper following the same pattern as M0 Task 4.

---

## Task 1: Gate `login_inner` on `CLI_CORE_LICENSE_ACTIVATE`

**Files:**
- Modify: `crates/spur-cli/src/commands/auth.rs:46-93`
- Modify: `crates/spur-cli/tests/auth_fake_provider.rs`

- [ ] **Step 1: Write 4 failing unit tests.**

Append to `tests/auth_fake_provider.rs`:

```rust
// Plan C M0.5 — gating CLI_CORE_LICENSE_ACTIVATE inside auth::run on
// the `Login` variant only. Logout/Refresh/Status are intentionally
// NOT gated so a tampered tier never bricks the recovery path.

use std::collections::BTreeSet;
use spur_license::{FeatureKey, LicenseState as LS, Plan as P};

fn empty_pro_gate() -> Arc<FeatureGate> {
    let g = FeatureGate::new(PolicyResolver::embedded());
    g.update_state(&LS::active_validated(P::Pro, BTreeSet::new()));
    Arc::new(g)
}

#[tokio::test]
async fn login_blocked_by_empty_pro_gate_returns_typed_error() {
    let fake = Arc::new(FakeProvider::new(LicenseState::inactive("fresh")));
    fake.push_activate_result(Ok(LicenseState::active_validated(
        Plan::Pro, Default::default(),
    )));
    let license = SpurLicense::from_provider(fake.clone(), empty_pro_gate());

    let err = run_with_license(
        AuthCommands::Login { key: "test-key".into(), format: OutputFormat::Plain },
        license,
    )
    .await
    .expect_err("login must be gated when cli_core_license_activate is absent");

    let msg = format!("{err:#}");
    assert!(
        msg.contains("cli_core_license_activate"),
        "error must name the gated key: {msg}"
    );
    assert_eq!(
        fake.activate_call_count(), 0,
        "gate must fire BEFORE provider.activate to prevent escalation against a tampered tier"
    );
}

#[tokio::test]
async fn logout_passes_through_empty_pro_gate() {
    let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
    let license = SpurLicense::from_provider(fake.clone(), empty_pro_gate());
    run_with_license(AuthCommands::Logout { format: OutputFormat::Plain }, license)
        .await
        .expect("logout must remain ungated to preserve brick recovery path");
    assert_eq!(fake.deactivate_call_count(), 1);
}

#[tokio::test]
async fn refresh_passes_through_empty_pro_gate() {
    let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
    fake.push_validate_result(Ok(LicenseState::active_cached()));
    let license = SpurLicense::from_provider(fake.clone(), empty_pro_gate());
    run_with_license(AuthCommands::Refresh { format: OutputFormat::Plain }, license)
        .await
        .expect("refresh must remain ungated to preserve brick recovery path");
    assert_eq!(fake.validate_call_count(), 1);
}

#[tokio::test]
async fn status_passes_through_empty_pro_gate() {
    let fake = Arc::new(FakeProvider::new(LicenseState::active_cached()));
    let license = SpurLicense::from_provider(fake.clone(), empty_pro_gate());
    run_with_license(AuthCommands::Status { format: OutputFormat::Plain }, license)
        .await
        .expect("status must remain ungated to preserve brick recovery path");
    // Status doesn't invoke any provider RPC; just reads current_state.
}
```

Note: the existing imports at the top (`Arc`, `FeatureGate`,
`PolicyResolver`, etc.) cover everything; only `FeatureKey` and
`BTreeSet` are new. Add to the existing import block.

- [ ] **Step 2: Run tests to verify they fail.**

```bash
scripts/spur-cargo test -p spur-cli --test auth_fake_provider login_blocked
```
Expected: FAIL — login currently bypasses the gate, so the assertion
that `activate_call_count() == 0` fails (it's 1 today).

- [ ] **Step 3: Gate `login_inner` in `auth.rs`.**

Modify `crates/spur-cli/src/commands/auth.rs` `login_inner`:

```rust
async fn login_inner(license: &SpurLicense, key: &str) -> Result<LicenseState> {
    // Plan C M0.5 — gate the activation surface specifically. We
    // deliberately do NOT gate Logout/Refresh/Status: a tampered tier
    // that strips `cli_core_license_activate` must still let the user
    // recover via `spur auth logout` (which falls back to the
    // embedded community policy where the key is granted).
    let gate = license.feature_gate();
    spur_license::require_feature(&gate, spur_license::FeatureKey::CLI_CORE_LICENSE_ACTIVATE)?;
    ensure_configured(license)?;
    Ok(license.activate(key).await?)
}
```

The `?` on `require_feature` auto-converts `FeatureGateError` →
`anyhow::Error` via thiserror's `Error` impl + anyhow's blanket
`From`.

- [ ] **Step 4: Run tests to verify they pass.**

```bash
scripts/spur-cargo test -p spur-cli --test auth_fake_provider
```
Expected: all tests pass — 4 new + 6 existing (existing happy-path
tests use `test_feature_gate()` which constructs from embedded
community policy and so retain `cli_core_license_activate`).

- [ ] **Step 5: Verify existing auth_cli.rs e2e isn't broken.**

```bash
scripts/spur-cargo test -p spur-cli --test auth_cli
```
Expected: still passes. The CLI-level `auth login` test exits with
a `NotConfigured` error today (no `SPUR_LICENSESEAT_API_KEY`), and
the gate fires *before* the configured-check, so the failure mode
changes from "config error" to "gate error" only when both env
strip + dev-plan-enterprise are set — neither is set in the
existing test.

- [ ] **Step 6: Run clippy + fmt.**

```bash
scripts/spur-cargo clippy -p spur-cli --tests -- -D warnings
scripts/spur-cargo fmt -p spur-cli -- --check
```
Expected: clean.

- [ ] **Step 7: Commit.**

```bash
git add crates/spur-cli/src/commands/auth.rs crates/spur-cli/tests/auth_fake_provider.rs
git commit -m "feat(spur-cli): C.1 M0.5 gate auth login on cli_core_license_activate"
```

---

## Task 2: Binary-level denial e2e

**Files:**
- Modify: `crates/spur-cli/tests/cli_core_gate_e2e.rs`

> **Why this is the first real wiring proof (codex 🔴 unblock):**
> M0 Task 5's smoke proved the binary launches and the community-tier
> happy path doesn't break. M0.5 produces the *denial* leg — the
> binary exits non-zero with a typed-error message when the gate
> denies. Together they form the full wiring assertion that M0
> alone could not.

> **Fixture coupling caveat:** This test uses
> `SPUR_LICENSE_DEV_PLAN=enterprise` to force an empty-Pro tier
> in the spawned binary. That works *today* because the embedded
> policy lacks an `enterprise` block (per the policy-gap follow-up
> doc). When option B of that doc lands (embed `enterprise` =
> `@inherit:pro`), this fixture stops producing an empty-Pro tier
> and this test must switch to a different tampered-state mechanism
> (e.g., a test-support env var like `SPUR_LICENSE_TEST_STRIP_KEYS`
> proposed in policy-gap doc option A). The test comment makes
> the coupling explicit.

- [ ] **Step 1: Add the denial e2e.**

Append to `crates/spur-cli/tests/cli_core_gate_e2e.rs`:

```rust
#[test]
fn spur_auth_login_exits_nonzero_when_cli_core_license_activate_denied() {
    // Plan C M0.5 — first true wiring assertion at the binary
    // boundary. The dev-only `SPUR_LICENSE_DEV_PLAN=enterprise` env
    // var forces the spawned `spur` to resolve an empty Enterprise
    // tier (because the embedded policy currently has no `enterprise`
    // block — see policy-gap follow-up). With zero features, the
    // gate denies `cli_core_license_activate` and `spur auth login`
    // exits non-zero before reaching the licenseseat provider.
    //
    // FIXTURE COUPLING: when policy-gap option B lands (embed
    // `enterprise` = @inherit:pro), this approach stops producing an
    // empty tier. Switch to a test-support strip-keys mechanism then.
    let assert = Command::cargo_bin("spur")
        .expect("spur binary builds")
        .env("SPUR_LICENSE_DEV_PLAN", "enterprise")
        .args(["auth", "login", "--key", "irrelevant-fixture-key"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("cli_core_license_activate"),
        "stderr must name the denied key, got:\n{stderr}"
    );
}
```

- [ ] **Step 2: Run the new test.**

```bash
scripts/spur-cargo test -p spur-cli --test cli_core_gate_e2e
```
Expected: 3 tests pass (2 existing + 1 new denial).

- [ ] **Step 3: Run the full spur-cli test suite to verify no regressions.**

```bash
scripts/spur-cargo test -p spur-cli
```
Expected: all tests pass.

- [ ] **Step 4: Run clippy + fmt.**

```bash
scripts/spur-cargo clippy -p spur-cli --tests -- -D warnings
scripts/spur-cargo fmt -p spur-cli -- --check
```
Expected: clean.

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-cli/tests/cli_core_gate_e2e.rs
git commit -m "test(spur-cli): C.1 M0.5 binary-level denial e2e for cli_core_license_activate"
```

---

## Task 3: Update parameterized invariant test

**Files:**
- Modify: `crates/spur-cli/tests/cli_core_gates.rs`

The `auth_arm_remains_ungated_in_m0` test in
`tests/cli_core_gates.rs:80-90` documents M0's deferred state.
M0.5 supersedes that documentation: the *registry* still lists
the key for community grants, but the *enforcement* now lives
inside `auth::run` rather than nowhere.

- [ ] **Step 1: Replace the test's purpose.**

```rust
#[test]
fn auth_login_is_gated_inside_auth_run_in_m0p5() {
    // Plan C M0.5 — `CLI_CORE_LICENSE_ACTIVATE` is now gated, but
    // not at the dispatch level. Enforcement lives inside
    // `crates/spur-cli/src/commands/auth.rs::login_inner` so that
    // Logout/Refresh/Status remain ungated and the brick-recovery
    // path stays open for tampered tiers.
    //
    // Registry assertion: community gate must still grant the key
    // so daily-driver Free users can run `spur auth login` against
    // a fresh community-policy install.
    let gate = community_gate();
    assert!(gate.has(FeatureKey::CLI_CORE_LICENSE_ACTIVATE));

    // The invariant that "denial returns FeatureGateError::Denied"
    // is exercised at the binary boundary by
    // `cli_core_gate_e2e::spur_auth_login_exits_nonzero_*`, not here,
    // because the helper-level test cannot exercise the in-process
    // `auth::run` path through clap dispatch.
}
```

- [ ] **Step 2: Run + commit.**

```bash
scripts/spur-cargo test -p spur-cli --test cli_core_gates
git add crates/spur-cli/tests/cli_core_gates.rs
git commit -m "test(spur-cli): C.1 M0.5 update auth-gate invariant doc-test for in-auth gating"
```

---

## Task 4: Workspace sweep

- [ ] **Step 1:** `scripts/spur-cargo build --workspace`
- [ ] **Step 2:** `scripts/spur-cargo test -p spur-license -p spur-cli`
- [ ] **Step 3:** `scripts/spur-cargo clippy -p spur-license -p spur-cli --tests -- -D warnings`
- [ ] **Step 4:** `scripts/spur-cargo fmt -p spur-cli -- --check`

Expected: all green, no commits.

---

## Acceptance criteria

- [ ] `Login` arm gated on `CLI_CORE_LICENSE_ACTIVATE` inside
      `login_inner`, fires *before* `ensure_configured` and
      *before* `provider.activate(...)` so a tampered tier cannot
      escalate.
- [ ] `Logout` / `Refresh` / `Status` arms are NOT gated (verified
      by 3 dedicated unit tests with empty-Pro `FeatureGate`).
- [ ] Activation-call counter on `FakeProvider` proves the gate
      fires before the provider in the denial path
      (`activate_call_count() == 0` when login is gated).
- [ ] Binary-level denial e2e proves `spur auth login` exits
      non-zero with stderr containing `cli_core_license_activate`
      under the `SPUR_LICENSE_DEV_PLAN=enterprise` fixture.
- [ ] M0's `auth_arm_remains_ungated_in_m0` test is replaced
      with `auth_login_is_gated_inside_auth_run_in_m0p5` documenting
      the new state.
- [ ] All M0 acceptance criteria still hold (no regression).

## Out of scope for M0.5

- Replacing the `SPUR_LICENSE_DEV_PLAN=enterprise` fixture coupling.
  Tracked in `2026-04-28-tier-revamp-policy-gap-enterprise-tier.md`
  as option A/B in that doc.
- Trial-flow integration (Plan D). The `Login` denial path will
  eventually fork into "tampered tier → upgrade modal" and "trial
  tier → trial activation flow"; M0.5 only ships the gate.
- Other `Auth` subcommand additions (e.g., `spur auth deactivate
  --all`). When/if those land, they must declare their own gating
  policy explicitly in this same plan family.
