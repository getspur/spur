//! Plan C M0 (wave C.1) — binary-level happy-path smoke.
//!
//! Proves the `spur` binary still launches and the gated community-tier
//! daily-driver paths still return 0 after wave C.1's `require_cli_gate`
//! calls land. Functions as a regression net: if a future commit
//! accidentally inverts a gate's logic or wires the wrong key, the
//! community-tier happy path goes red.
//!
//! It does NOT prove the gate fires under denial. A real denial e2e
//! (binary exits non-zero + stderr names the missing key) needs either
//! a tampered policy fixture or a Pro JWT with stripped entitlements;
//! both are deferred to **M0.5** alongside the
//! `CLI_CORE_LICENSE_ACTIVATE` enforcement (the same fixture serves
//! both). Until M0.5, every test in this file passes whether or not
//! the per-arm `require_cli_gate(...)?` calls are present — that is a
//! known limitation, not a hidden bug.

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
