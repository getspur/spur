//! Plan C M0 + M0.5 + Tier 1 (wave C.1) — binary-level gate tests.
//!
//! - **Happy-path smokes** (M0): prove the `spur` binary launches and
//!   that gated community-tier daily-driver paths still return 0
//!   after wave C.1's `require_cli_gate` calls land. Regression net
//!   for any future commit that accidentally inverts a gate or wires
//!   the wrong key.
//!
//! - **Denial e2e** (M0.5): proves `spur auth login` exits non-zero
//!   with a typed-error stderr message when the spawned binary's
//!   gate denies `cli_core_license_activate`. The first binary-level
//!   test that actually exercises the denial leg of `require_feature`
//!   through the clap dispatch path. The denial fixture is the
//!   debug-only `SPUR_LICENSE_TEST_STRIP_KEYS` env var (see
//!   `crates/spur-license/src/gate.rs::apply_test_strip_keys`).
//!
//! - **CTA wiring** (Tier 1): proves a stripped-key denial through
//!   `spur exec` reaches the binary boundary with a typed-error
//!   stderr message. The structured CTA (`spur auth status` /
//!   `spur auth login` recovery lines) is TTY-gated and `assert_cmd`
//!   does not allocate a tty for the child, so the CTA path itself
//!   is exercised by the unit tests in
//!   `crates/spur-license/src/upgrade_cta.rs::tests`. This binary
//!   test asserts only that the typed-error key name reaches stderr
//!   under the same `SPUR_LICENSE_TEST_STRIP_KEYS` fixture.

#![cfg(unix)]

use assert_cmd::Command;

#[test]
fn spur_help_exits_zero() {
    // Sanity: clap built-ins exit before our match block. Establishes
    // that the binary itself launches under cargo test.
    Command::cargo_bin("spur")
        .expect("spur binary builds in test profile")
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn spur_init_succeeds_on_default_community_tier() {
    // Default environment (no `SPUR_LICENSESEAT_API_KEY`) → embedded
    // community policy → `CLI_CORE_INIT` granted → init runs to success.
    //
    // We strip `SPUR_LICENSE_DEV_PLAN` for the same reason `init_ux.rs`
    // does: a dev-machine `enterprise` (or any unknown value) leaks
    // into the child as a tier with zero features and trips the
    // `require_cli_gate` call. The embedded policy only defines
    // `community` and `pro`.
    //
    // We narrow PATH to `/usr/bin` so no real agent binaries get
    // discovered; with zero agents `spur init` exits 0 without writing
    // `.spur/config.toml` (per `init_ux::init_with_zero_agents_writes_no_config`).
    let tmp = tempfile::tempdir().expect("tempdir");
    Command::cargo_bin("spur")
        .expect("spur binary builds")
        .current_dir(tmp.path())
        .env("PATH", "/usr/bin")
        // Strip both dev-only env vars so a dev shell that has either
        // set cannot perturb the snapshot. After the policy-gap fix,
        // unknown DEV_PLAN values fall through to community, but
        // explicit `pro` would still grant features beyond what the
        // happy-path smoke wants to assert.
        .env_remove("SPUR_LICENSE_DEV_PLAN")
        .env_remove("SPUR_LICENSE_TEST_STRIP_KEYS")
        .arg("init")
        .assert()
        .success();
}

#[test]
fn spur_auth_login_exits_nonzero_when_cli_core_license_activate_denied() {
    // Plan C M0.5 — first true wiring assertion at the binary
    // boundary. We strip `cli_core_license_activate` from the resolved
    // community-tier feature set via the debug-only test hook
    // `SPUR_LICENSE_TEST_STRIP_KEYS` (see
    // `crates/spur-license/src/gate.rs::apply_test_strip_keys`).
    // With the key stripped, the gate inside `auth::run`'s
    // `login_inner` denies and `spur auth login` exits non-zero
    // before reaching the licenseseat provider.
    //
    // The hook is `#[cfg(debug_assertions)]`-gated so it cannot leak
    // into release binaries. We also strip `SPUR_LICENSE_DEV_PLAN`
    // so a dev-machine value doesn't perturb the snapshot before the
    // strip applies (per the M0 init_ux fix pattern).
    let assert = Command::cargo_bin("spur")
        .expect("spur binary builds")
        .env("SPUR_LICENSE_TEST_STRIP_KEYS", "cli_core_license_activate")
        .env_remove("SPUR_LICENSE_DEV_PLAN")
        .args(["auth", "login", "--key", "irrelevant-fixture-key"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("cli_core_license_activate"),
        "stderr must name the denied key, got:\n{stderr}",
    );
}

#[test]
fn spur_exec_under_stripped_key_renders_typed_error_at_binary_boundary() {
    // Plan C Tier 1 — proves a `cli_core_exec` denial reaches the
    // binary boundary with a typed-error stderr message. The
    // structured upgrade CTA (`spur auth status` / `spur auth login`
    // recovery copy) is TTY-gated and assert_cmd does not allocate a
    // pty for the child, so the plain `Error: {err:#}` path renders
    // here. CTA shape is exercised by the unit tests in
    // `crates/spur-license/src/upgrade_cta.rs::tests`.
    //
    // The failure-then-key-name pattern is the regression net: if a
    // future change rewires the `cli_core_exec` gate to a different
    // key (or removes it), the strip fixture stops denying and this
    // assertion fires.
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

#[test]
fn spur_exec_under_stripped_key_renders_structured_upgrade_cta_under_force_tty() {
    // Plan C Tier 2 Task 3 — closes the Tier 1 follow-up
    // (`2026-04-28-tier-revamp-tier1-followup-tty-test-hook.md`).
    //
    // assert_cmd does not allocate a pty for the child, so
    // `is_terminal()` returns false and the CTA path is normally
    // bypassed. The debug-only `SPUR_FORCE_TTY=1` env var forces
    // the TTY-gate to true (via `is_tty_or_forced()` in
    // `crates/spur-cli/src/main.rs`), exercising the CTA renderer
    // dispatch path end-to-end at the binary boundary.
    //
    // Together with the existing
    // `spur_exec_under_stripped_key_renders_typed_error_at_binary_boundary`
    // smoke (which only asserts key-name propagation without the
    // CTA), this gives a regression net for:
    //   1. `is_terminal()` predicate inversion (e.g. `if !...`)
    //   2. dropping the `find_gate_error` branch
    //   3. renaming `format_upgrade_cta` without updating main.rs
    //
    // All three would PASS the existing key-name-only smoke but
    // FAIL this CTA-shape smoke.
    let assert = Command::cargo_bin("spur")
        .expect("spur binary builds")
        .env("SPUR_LICENSE_TEST_STRIP_KEYS", "cli_core_exec")
        .env("SPUR_FORCE_TTY", "1")
        .env_remove("SPUR_LICENSE_DEV_PLAN")
        .args(["exec", "--agent", "claude-code", "irrelevant-task"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("cli_core_exec"),
        "stderr must name the denied key, got:\n{stderr}",
    );
    assert!(
        stderr.contains("spur auth status"),
        "stderr must include `spur auth status` recovery line, got:\n{stderr}",
    );
    assert!(
        stderr.contains("spur auth login --key"),
        "stderr must include `spur auth login --key` recovery line, got:\n{stderr}",
    );
    assert!(
        stderr.contains("spur auth logout"),
        "stderr must include `spur auth logout` re-login hint, got:\n{stderr}",
    );
}
