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
        "claude-code-sj",
        "kiro",
        "claude-code",
        "codex-bin",
        "codex",
        "gemini",
        "opencode",
        "kimi",
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
    let mut c = Command::new(env!("CARGO_BIN_EXE_spur"));
    // The embedded policy only defines `community` and `pro` tiers, so a
    // dev-machine `SPUR_LICENSE_DEV_PLAN=enterprise` (or any other unknown
    // value) leaks into the child as a tier with zero features and trips
    // every `require_cli_gate(...)` call (Plan C wave C.1). Tests in this
    // file exercise the default community-tier path, so always strip the
    // override before spawning.
    c.env_remove("SPUR_LICENSE_DEV_PLAN");
    c
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

    assert!(
        status.success(),
        "spur init should exit 0 even with no agents"
    );
    assert!(
        !tmp.path().join(".spur/config.toml").exists(),
        "spur init with zero agents must NOT write .spur/config.toml"
    );
}

#[test]
fn init_with_existing_config_merges_agents() {
    let _g = LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".spur")).unwrap();

    // Pre-existing config with custom brain and bot settings.
    let existing = r#"
[brain]
default = "kiro"

[bot.telegram]
enabled = true
operator_user_id = 12345
"#;
    fs::write(tmp.path().join(".spur/config.toml"), existing).unwrap();
    stub_binary(tmp.path(), "claude");

    let status = spur()
        .current_dir(tmp.path())
        .env("PATH", format!("{}:/usr/bin", tmp.path().display()))
        .arg("init")
        .status()
        .expect("spawn spur init");

    assert!(status.success(), "merge init should exit 0");
    let after = fs::read_to_string(tmp.path().join(".spur/config.toml")).unwrap();

    // Should merge the discovered claude agent.
    assert!(
        after.contains(r#"name = "claude-code-sj""#),
        "should merge discovered agent; got:\n{after}"
    );

    // Should preserve existing bot config.
    assert!(
        after.contains("enabled = true"),
        "should preserve bot config; got:\n{after}"
    );
    assert!(
        after.contains("operator_user_id = 12345"),
        "should preserve bot operator; got:\n{after}"
    );

    // Brain should be recomputed (kiro is not on PATH here, claude-code-sj is).
    assert!(
        after.contains(r#"default = "claude-code-sj""#),
        "should recompute brain to discovered agent; got:\n{after}"
    );
}

#[test]
fn init_with_force_resets_agents_preserves_non_agent_config() {
    let _g = LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".spur")).unwrap();

    // Config with a manually-added agent and bot settings.
    let existing = r#"
[[agents.entries]]
name = "custom-agent"
command = "custom"
transport = "acp"
role = "both"

[brain]
default = "custom-agent"

[bot.telegram]
enabled = true
operator_user_id = 99999
"#;
    fs::write(tmp.path().join(".spur/config.toml"), existing).unwrap();

    // Only kiro-cli is on PATH.
    stub_binary(tmp.path(), "kiro-cli");

    let status = spur()
        .current_dir(tmp.path())
        .env("PATH", format!("{}:/usr/bin", tmp.path().display()))
        .args(["init", "--force"])
        .status()
        .expect("spawn spur init --force");

    assert!(status.success(), "force init should exit 0");
    let after = fs::read_to_string(tmp.path().join(".spur/config.toml")).unwrap();

    // --force should drop the custom agent.
    assert!(
        !after.contains("custom-agent"),
        "--force should reset to discovered-only; got:\n{after}"
    );

    // Should preserve bot config.
    assert!(
        after.contains("enabled = true"),
        "should preserve bot config; got:\n{after}"
    );
    assert!(
        after.contains("operator_user_id = 99999"),
        "should preserve bot operator; got:\n{after}"
    );

    // Brain should adapt to the discovered agent.
    assert!(
        after.contains(r#"default = "kiro""#),
        "brain should adapt to kiro; got:\n{after}"
    );
}
