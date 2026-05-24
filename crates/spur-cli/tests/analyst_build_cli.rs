use std::process::Command;

fn spur_binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_spur"))
}

fn fixture_git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(dir.path())
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}");
    };
    run(&["init"]);
    run(&["config", "user.email", "spur@example.test"]);
    run(&["config", "user.name", "Spur Test"]);
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn hello() -> u32 { 42 }\n",
    )
    .unwrap();
    run(&["add", "src/lib.rs"]);
    run(&["commit", "-m", "initial"]);
    dir
}

#[test]
fn analyst_build_skipped_by_flag() {
    let dir = fixture_git_repo();
    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build", "--workspace", "--no-analyst", "--quiet"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !dir.path().join(".spur/analyst.duckdb").exists(),
        "analyst DB should not have been created with --no-analyst"
    );
}

#[test]
fn analyst_build_skipped_by_env() {
    let dir = fixture_git_repo();
    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build", "--workspace", "--quiet"])
        .env("SPUR_GRAPH_SKIP_ANALYST", "1")
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !dir.path().join(".spur/analyst.duckdb").exists(),
        "analyst DB should not have been created with SPUR_GRAPH_SKIP_ANALYST=1"
    );
}

#[test]
fn analyst_build_soft_fails_when_duckdb_missing() {
    let dir = fixture_git_repo();
    // Build graph first (with analyst skipped so this test stays isolated).
    let pre = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build", "--workspace", "--no-analyst", "--quiet"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn");
    assert!(pre.status.success());

    // Now invoke `analyst build` with a PATH that has no duckdb.
    let empty_path = dir.path().join("empty-path");
    std::fs::create_dir_all(&empty_path).unwrap();
    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["analyst", "build"])
        .env("PATH", empty_path)
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "soft-fail should exit 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("duckdb"),
        "expected duckdb-missing warning, got: {stderr}"
    );
    assert!(
        !dir.path().join(".spur/analyst.duckdb").exists(),
        "no DB should exist after soft-fail"
    );
}

#[test]
fn analyst_build_rejects_schema_version_mismatch() {
    let dir = fixture_git_repo();
    // Build graph first to populate parquets.
    let pre = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build", "--workspace", "--no-analyst", "--quiet"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn");
    assert!(
        pre.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pre.stderr)
    );

    // Tamper with manifest.json under the resolved CURRENT artifact.
    let current = dir.path().join(".spur/graph/CURRENT");
    let resolved = std::fs::canonicalize(&current).expect("CURRENT resolves");
    let manifest_path = resolved.join("manifest.json");
    let original = std::fs::read_to_string(&manifest_path).unwrap();
    let tampered = original.replace("spur-graph-schema-v5", "spur-graph-schema-vNEXT");
    assert_ne!(
        original, tampered,
        "fixture invariant: schema_version must have been present"
    );
    std::fs::write(&manifest_path, &tampered).unwrap();

    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["analyst", "build"])
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "schema mismatch should hard-fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("schema") && stderr.contains("vNEXT"),
        "expected schema-mismatch error, got: {stderr}"
    );
}

#[test]
fn analyst_build_atomic_under_duckdb_failure() {
    let dir = fixture_git_repo();
    // Build the graph (parquets only - skip analyst so we control the next step).
    let pre = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build", "--workspace", "--no-analyst", "--quiet"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn");
    assert!(
        pre.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pre.stderr)
    );

    // Pre-populate the analyst DB with sentinel bytes that must survive the
    // failed second invocation.
    let db_path = dir.path().join(".spur/analyst.duckdb");
    std::fs::write(&db_path, b"SENTINEL-BYTES-MUST-NOT-CHANGE").unwrap();
    let before = std::fs::read(&db_path).unwrap();

    // Stage a fake `duckdb` that always exits non-zero.
    let fake_bin_dir = dir.path().join("fake-bin");
    std::fs::create_dir_all(&fake_bin_dir).unwrap();
    let fake_duckdb = fake_bin_dir.join("duckdb");
    std::fs::write(&fake_duckdb, "#!/bin/sh\necho 'fake duckdb' >&2\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake_duckdb, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Compose a PATH that has the fake duckdb but nothing else from the host.
    let path_value = fake_bin_dir.display().to_string();

    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["analyst", "build"])
        .env("PATH", path_value)
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "soft-degrade should exit 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = std::fs::read(&db_path).unwrap();
    assert_eq!(
        before, after,
        "previous analyst DB must be byte-identical after a failed duckdb run"
    );

    // No leftover tmp files alongside the DB.
    let tmp_count = std::fs::read_dir(dir.path().join(".spur"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("analyst.duckdb.tmp-")
        })
        .count();
    assert_eq!(tmp_count, 0, "leftover tmp file(s) present after failure");
}

#[test]
fn analyst_build_concurrent_skip() {
    let dir = fixture_git_repo();
    // Build the parquets first (analyst skipped - we'll exercise it concurrently below).
    let pre = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build", "--workspace", "--no-analyst", "--quiet"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn");
    assert!(
        pre.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pre.stderr)
    );

    let duckdb_found = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("duckdb").is_file()))
        .unwrap_or(false);
    if !duckdb_found {
        eprintln!("skipping: duckdb CLI not on PATH");
        return;
    }
    let probe_db = dir.path().join(".spur/analyst.probe.duckdb");
    let probe = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["analyst", "build", "--db-path"])
        .arg(&probe_db)
        .output()
        .expect("spawn probe");
    assert!(
        probe.status.success(),
        "probe stderr: {}",
        String::from_utf8_lossy(&probe.stderr)
    );
    if !probe_db.is_file() {
        eprintln!(
            "skipping: duckdb CLI present but analyst init SQL did not produce a DB: {}",
            String::from_utf8_lossy(&probe.stderr)
        );
        return;
    }
    let _ = std::fs::remove_file(&probe_db);

    // Launch two `analyst build` invocations close enough in time that one
    // should observe the other's flock.
    let spawn = || {
        Command::new(spur_binary())
            .current_dir(dir.path())
            .args(["analyst", "build"])
            .stderr(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn")
    };
    let a = spawn();
    let b = spawn();
    let a_out = a.wait_with_output().expect("wait a");
    let b_out = b.wait_with_output().expect("wait b");

    // Both must exit 0 (one builds, the other skips).
    assert!(
        a_out.status.success(),
        "a stderr: {}",
        String::from_utf8_lossy(&a_out.stderr)
    );
    assert!(
        b_out.status.success(),
        "b stderr: {}",
        String::from_utf8_lossy(&b_out.stderr)
    );

    let combined_stderr = format!(
        "{}{}",
        String::from_utf8_lossy(&a_out.stderr),
        String::from_utf8_lossy(&b_out.stderr)
    );
    assert!(
        combined_stderr.contains("another analyst build in progress"),
        "expected at least one process to log the contention skip; got: {combined_stderr}"
    );

    // Final DB must be valid (exists and non-empty).
    let db = dir.path().join(".spur/analyst.duckdb");
    let meta = std::fs::metadata(&db).expect("DB exists after concurrent builds");
    assert!(meta.len() > 0, "DB must be non-empty");
}
