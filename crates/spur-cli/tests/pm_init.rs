//! Behavioral tests for `spur pm init`.
//!
//! Calls `spur_cli::commands::init::run_pm_init` directly against a
//! tempdir-backed repo root. The command is non-interactive, so we don't
//! need PTY plumbing.

#![cfg(unix)]

use spur_cli::commands::init::run_pm_init;
use tempfile::TempDir;

#[tokio::test]
async fn fresh_repo_creates_beads_and_gitignore() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    run_pm_init(root.clone()).await.expect("pm init");

    assert!(root.join(".beads").is_dir(), ".beads/ should be created");
    assert!(
        root.join(".beads").join("beads.db").is_file(),
        "beads.db should be initialized"
    );
    assert!(
        root.join(".beads").join("issues.jsonl").is_file(),
        "issues.jsonl should be created (empty is fine)"
    );

    let gi = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    for needle in [
        ".beads/beads.db",
        ".beads/beads.db-*",
        ".beads/.write.lock",
        ".beads/issues.jsonl.*.tmp",
    ] {
        assert!(
            gi.lines().any(|l| l.trim() == needle),
            ".gitignore missing line `{needle}`; got:\n{gi}"
        );
    }
    // issues.jsonl MUST NOT be ignored (it's the source of truth).
    assert!(
        !gi.lines().any(|l| l.trim() == ".beads/issues.jsonl"),
        "issues.jsonl must remain committed"
    );
}

#[tokio::test]
async fn idempotent_no_duplicate_gitignore_entries() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    run_pm_init(root.clone()).await.expect("first pm init");
    let after_first = std::fs::read_to_string(root.join(".gitignore")).unwrap();

    run_pm_init(root.clone()).await.expect("second pm init");
    let after_second = std::fs::read_to_string(root.join(".gitignore")).unwrap();

    assert_eq!(
        after_first, after_second,
        ".gitignore must be byte-identical after a second pm init"
    );

    // And no line appears twice.
    let mut seen = std::collections::HashMap::<&str, usize>::new();
    for line in after_second.lines() {
        *seen.entry(line.trim()).or_insert(0) += 1;
    }
    for needle in [
        ".beads/beads.db",
        ".beads/beads.db-*",
        ".beads/.write.lock",
        ".beads/issues.jsonl.*.tmp",
    ] {
        assert_eq!(
            seen.get(needle).copied().unwrap_or(0),
            1,
            "duplicate `{needle}`"
        );
    }
}

#[tokio::test]
async fn flips_existing_pm_beads_disabled_to_enabled() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    let spur_dir = root.join(".spur");
    std::fs::create_dir_all(&spur_dir).unwrap();
    let cfg_path = spur_dir.join("config.toml");

    // Minimal valid SpurConfig with [pm.beads] enabled = false, auto_sync = true.
    std::fs::write(
        &cfg_path,
        r#"
[project]
name = "test"

[pm.beads]
enabled = false
auto_sync = true
"#,
    )
    .unwrap();

    run_pm_init(root.clone()).await.expect("pm init");

    let written = std::fs::read_to_string(&cfg_path).unwrap();
    let parsed: spur_acp::config::SpurConfig = toml::from_str(&written).unwrap();
    let beads = parsed.pm.beads.expect("[pm.beads] should be present");
    assert!(beads.enabled, "enabled must be flipped to true");
    assert!(beads.auto_sync, "auto_sync must be preserved");
}

#[test]
fn spur_init_bootstraps_beads_on_first_run() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Stub `npx` so `spur init` discovers `claude-code` and gets past the
    // "no agents → no config" early exit. Without an agent, Phase 12 never
    // runs, so Phase 12.5 (pm init) wouldn't run either.
    let npx = root.join("npx");
    std::fs::write(&npx, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = std::fs::metadata(&npx).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&npx, perms).unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_spur"))
        .current_dir(root)
        .env("PATH", format!("{}:/usr/bin", root.display()))
        .env_remove("SPUR_LICENSE_DEV_PLAN")
        .env_remove("SPUR_LICENSE_TEST_STRIP_KEYS")
        .arg("init")
        .status()
        .expect("spawn spur init");

    assert!(status.success(), "spur init should succeed");
    assert!(
        root.join(".beads").join("beads.db").is_file(),
        "spur init must auto-bootstrap .beads/beads.db when missing"
    );
    assert!(
        root.join(".beads").join("issues.jsonl").is_file(),
        "spur init must auto-create issues.jsonl"
    );
    let gi = std::fs::read_to_string(root.join(".gitignore")).unwrap_or_default();
    assert!(
        gi.lines().any(|l| l.trim() == ".beads/beads.db"),
        ".gitignore should be patched by auto-bootstrap; got:\n{gi}"
    );
}

#[test]
fn spur_init_skips_pm_init_when_beads_already_exists() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    // Pre-populate .beads/ with a sentinel — bootstrap MUST NOT touch it.
    let beads = root.join(".beads");
    std::fs::create_dir_all(&beads).unwrap();
    std::fs::write(beads.join("sentinel.txt"), "do not delete").unwrap();

    let npx = root.join("npx");
    std::fs::write(&npx, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = std::fs::metadata(&npx).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&npx, perms).unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_spur"))
        .current_dir(root)
        .env("PATH", format!("{}:/usr/bin", root.display()))
        .env_remove("SPUR_LICENSE_DEV_PLAN")
        .env_remove("SPUR_LICENSE_TEST_STRIP_KEYS")
        .arg("init")
        .status()
        .expect("spawn spur init");

    assert!(status.success());
    assert!(
        beads.join("sentinel.txt").exists(),
        "must not touch existing .beads/"
    );
    assert!(
        !beads.join("beads.db").exists(),
        "should not initialize beads.db when .beads/ already exists"
    );
}

#[tokio::test]
async fn no_config_file_succeeds_silently() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    // No .spur/config.toml — pm init must still succeed.
    run_pm_init(root.clone())
        .await
        .expect("pm init without config");

    assert!(root.join(".beads").join("beads.db").is_file());
    assert!(!root.join(".spur").join("config.toml").exists());
}

#[test]
fn spur_init_filters_skills_to_discovered_adapter_only() {
    // With `claude-code` discovered (via stubbed `npx`) and no other agents
    // on PATH, default-on skills install must materialize `.claude/skills/`
    // and `.spur/skills/` but NOT `.gemini/skills/`, `.kiro/skills/`, etc.
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    let npx = root.join("npx");
    std::fs::write(&npx, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = std::fs::metadata(&npx).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&npx, perms).unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_spur"))
        .current_dir(root)
        .env("PATH", format!("{}:/usr/bin", root.display()))
        .env_remove("SPUR_LICENSE_DEV_PLAN")
        .env_remove("SPUR_LICENSE_TEST_STRIP_KEYS")
        .arg("init")
        .status()
        .expect("spawn spur init");
    assert!(status.success());

    // claude-code WAS discovered → its dir should exist.
    assert!(
        root.join(".claude").join("skills").is_dir(),
        ".claude/skills/ must be materialized when claude-code is on PATH"
    );
    // .spur/skills/ is the brain target — always rendered.
    assert!(
        root.join(".spur").join("skills").is_dir(),
        ".spur/skills/ must always be materialized"
    );
    // Stubbing `npx` may also satisfy `codex` (npx @zed-industries/codex-acp)
    // — that's discovery doing its job, not a filter bug. We only assert on
    // adapters whose seed agents have NO dependency on `npx`.
    for unwanted in [".gemini", ".kiro", ".opencode", ".cursor", ".kimi"] {
        let dir = root.join(unwanted).join("skills");
        assert!(
            !dir.exists(),
            "{unwanted}/skills/ should NOT be created when its agent isn't on PATH; \
             found: {}",
            dir.display()
        );
    }
}

#[test]
fn spur_init_with_skills_flag_materializes_all_adapters() {
    // `--with-skills` is the escape hatch: full fanout regardless of which
    // agents are on PATH.
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    let npx = root.join("npx");
    std::fs::write(&npx, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = std::fs::metadata(&npx).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&npx, perms).unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_spur"))
        .current_dir(root)
        .env("PATH", format!("{}:/usr/bin", root.display()))
        .env_remove("SPUR_LICENSE_DEV_PLAN")
        .env_remove("SPUR_LICENSE_TEST_STRIP_KEYS")
        .arg("init")
        .arg("--with-skills")
        .status()
        .expect("spawn spur init --with-skills");
    assert!(status.success());

    for dir in [
        ".spur/skills",
        ".claude/skills",
        ".gemini/skills",
        ".kiro/skills",
        ".opencode/skills",
        ".kimi/skills",
    ] {
        assert!(
            root.join(dir).is_dir(),
            "{dir} must be materialized with --with-skills; missing"
        );
    }
}
