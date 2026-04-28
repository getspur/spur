//! Plan C M0 + M0.5 (wave C.1) — binary-level gate tests.
//!
//! - **Happy-path smokes** (M0): prove the `spur` binary launches and
//!   that gated community-tier daily-driver paths still return 0
//!   after wave C.1's `require_cli_gate` calls land. Regression net
//!   for any future commit that accidentally inverts a gate or wires
//!   the wrong key.
//!
//! - **Denial e2e** (M0.5): proves `spur auth login` exits non-zero
//!   with a typed-error stderr message when the spawned binary's
//!   gate denies `cli_core_license_activate`. This is the first
//!   binary-level test that actually exercises the denial leg of
//!   `require_feature` through the clap dispatch path.

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
        .env_remove("SPUR_LICENSE_DEV_PLAN")
        .arg("init")
        .assert()
        .success();
}

#[test]
fn spur_auth_login_exits_nonzero_when_cli_core_license_activate_denied() {
    // Plan C M0.5 — first true wiring assertion at the binary
    // boundary. The dev-only `SPUR_LICENSE_DEV_PLAN=enterprise` env
    // var forces the spawned `spur` to resolve an empty Enterprise
    // tier (because the embedded policy currently has no `enterprise`
    // block — see policy-gap follow-up doc). With zero features, the
    // gate denies `cli_core_license_activate` and `spur auth login`
    // exits non-zero before reaching the licenseseat provider.
    //
    // FIXTURE COUPLING: when policy-gap option B lands (embed
    // `enterprise` = `@inherit:pro`), this approach stops producing
    // an empty tier. Switch to a test-support strip-keys mechanism
    // then. The test will fail loudly instead of silently passing
    // because the gate would let `auth login` through and we'd hit
    // a `NotConfigured` exit (still non-zero) but stderr would no
    // longer name the gated key — the second assertion catches that.
    let assert = Command::cargo_bin("spur")
        .expect("spur binary builds")
        .env("SPUR_LICENSE_DEV_PLAN", "enterprise")
        .args(["auth", "login", "--key", "irrelevant-fixture-key"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("cli_core_license_activate"),
        "stderr must name the denied key, got:\n{stderr}",
    );
}
