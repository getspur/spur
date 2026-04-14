//! Init UX tests: contract guard + behavioral tests for `spur init`.
//!
//! Kept as one file because the contract test is trivial and colocating
//! it with the behavioral tests means future contributors see the
//! install-hint requirement when they open this file.

#![cfg(unix)]

#[test]
fn install_hints_cover_all_seed_agents() {
    // Can't access the private const directly from an integration test.
    // Re-encode it here via a parallel list. If the two drift, the test
    // fails and forces the contributor to update both sides.
    //
    // Alternative: expose INSTALL_HINTS as pub from a lib target. That
    // would be cleaner, but spur-cli is a binary-only crate and adding
    // a lib target just for this is overkill. Keep the parallel list.
    let expected_names: &[&str] = &[
        "claude-code",
        "kiro",
        "claude-code-acp",
        "codex",
        "gemini",
    ];
    let seeds = spur_acp::config::load_seed_template();
    for agent in &seeds.entries {
        assert!(
            expected_names.contains(&agent.name.as_str()),
            "seed agent `{}` has no INSTALL_HINTS entry — add one to \
             crates/spur-cli/src/main.rs AND to expected_names in this test",
            agent.name
        );
    }
    // Also check the reverse direction: no orphan expected_names that
    // aren't in seeds (would indicate a stale hint for a deleted agent).
    let seed_names: Vec<_> = seeds.entries.iter().map(|a| a.name.as_str()).collect();
    for expected in expected_names {
        assert!(
            seed_names.contains(expected),
            "expected_names has `{expected}` but it's not in seed template"
        );
    }
}

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::Mutex;
use tempfile::TempDir;

/// Serialize tests that spawn subprocesses to avoid tempdir/PATH
/// collision if run in parallel.
static LOCK: Mutex<()> = Mutex::new(());

fn stub_binary(dir: &std::path::Path, name: &str) {
    let path = dir.join(name);
    fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
}

fn spur() -> Command {
    Command::new(env!("CARGO_BIN_EXE_spur"))
}

#[test]
fn init_with_zero_agents_writes_no_config() {
    let _g = LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();

    let status = spur()
        .current_dir(tmp.path())
        .env("PATH", format!("{}:/usr/bin", tmp.path().display()))
        .arg("init")
        .status()
        .expect("spawn spur init");

    assert!(status.success(), "spur init should exit 0 even with no agents");
    assert!(
        !tmp.path().join(".spur/config.toml").exists(),
        "spur init with zero agents must NOT write .spur/config.toml"
    );
}

#[test]
fn init_with_existing_config_requires_force() {
    let _g = LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".spur")).unwrap();
    let existing = "# pre-existing, must not be touched\n";
    fs::write(tmp.path().join(".spur/config.toml"), existing).unwrap();
    stub_binary(tmp.path(), "claude");

    let status = spur()
        .current_dir(tmp.path())
        .env("PATH", format!("{}:/usr/bin", tmp.path().display()))
        .arg("init")
        .status()
        .expect("spawn spur init");

    assert!(status.success(), "overwrite refusal should exit 0, not error");
    let after = fs::read_to_string(tmp.path().join(".spur/config.toml")).unwrap();
    assert_eq!(
        after, existing,
        "config must NOT be modified without --force"
    );
}

#[test]
fn init_with_force_overwrites_and_sets_adaptive_brain() {
    let _g = LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".spur")).unwrap();
    fs::write(tmp.path().join(".spur/config.toml"), "# stale\n").unwrap();

    // Stub only kiro. Post-Spec-3 baseline brain.default is hardcoded
    // to claude-code — adaptive selection must pick kiro instead
    // (because claude-code isn't on PATH in this test).
    stub_binary(tmp.path(), "kiro-cli");

    let status = spur()
        .current_dir(tmp.path())
        .env("PATH", format!("{}:/usr/bin", tmp.path().display()))
        .args(["init", "--force"])
        .status()
        .expect("spawn spur init --force");

    assert!(status.success());
    let after = fs::read_to_string(tmp.path().join(".spur/config.toml")).unwrap();

    assert!(
        !after.contains("# stale"),
        "--force must overwrite the old file"
    );
    assert!(
        after.contains("name = \"kiro\""),
        "new config must contain the registered agent; got:\n{after}"
    );
    // Adaptive-brain assertion: brain.default must point at the
    // REGISTERED agent (kiro), not the hardcoded claude-code.
    assert!(
        after.contains("default = \"kiro\""),
        "brain.default must adapt to installed agents (kiro here); got:\n{after}"
    );
}
