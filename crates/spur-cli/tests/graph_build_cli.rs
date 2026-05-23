use std::process::Command;

use spur_graph::{read_artifact_parquet, read_current_pointer};

fn spur_binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_spur"))
}

fn fixture_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join(".git")).expect("mkdir .git");
    std::fs::create_dir_all(dir.path().join("src")).expect("mkdir src");
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub struct Engine;\n\npub fn run() -> Engine {\n    Engine\n}\n",
    )
    .expect("write source");
    dir
}

fn fixture_git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    run_git(dir.path(), &["init"]);
    run_git(dir.path(), &["config", "user.email", "spur@example.test"]);
    run_git(dir.path(), &["config", "user.name", "Spur Test"]);
    std::fs::create_dir_all(dir.path().join("src")).expect("mkdir src");
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub struct Engine;\n\npub fn run() -> Engine {\n    Engine\n}\n",
    )
    .expect("write source");
    run_git(dir.path(), &["add", "src/lib.rs"]);
    run_git(dir.path(), &["commit", "-m", "initial"]);
    dir
}

fn run_git(root: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn graph_build_writes_default_index_and_prints_summary() {
    let dir = fixture_tree();

    let output = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn spur graph build");

    assert!(
        output.status.success(),
        "expected success; stderr = {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("files: 1"), "{stdout}");
    assert!(stdout.contains("nodes:"), "{stdout}");
    assert!(stdout.contains("edges:"), "{stdout}");
    assert!(
        stdout.contains(".spur/graph"),
        "expected default output path in stdout, got: {stdout}"
    );

    let artifact_path = read_current_pointer(dir.path()).expect("read CURRENT");
    assert!(artifact_path.is_dir(), "expected graph index artifact dir");
    let artifact = read_artifact_parquet(&artifact_path).expect("load artifact");
    assert_eq!(artifact.files.len(), 1);
    assert!(artifact
        .symbols
        .iter()
        .any(|symbol| symbol.entity_name == "Engine"));
}

#[test]
fn graph_build_workspace_flag_uses_worktree_root() {
    let dir = fixture_tree();
    let nested = dir.path().join("src");

    let output = Command::new(spur_binary())
        .current_dir(&nested)
        .args(["graph", "build", "--workspace"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn spur graph build");

    assert!(
        output.status.success(),
        "expected success; stderr = {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let artifact_path = read_current_pointer(dir.path()).expect("read CURRENT");
    assert!(artifact_path.is_dir(), "expected graph index artifact dir");
}

#[test]
fn graph_build_quiet_suppresses_progress_and_honors_output() {
    let dir = fixture_tree();
    let output_path = dir.path().join("custom-index");

    let output = Command::new(spur_binary())
        .current_dir(dir.path())
        .args([
            "graph",
            "build",
            "--quiet",
            "--no-analyst",
            "--output",
            output_path.to_str().expect("utf8 path"),
        ])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn spur graph build");

    assert!(
        output.status.success(),
        "expected success; stderr = {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("Building"), "{stdout}");
    assert!(stdout.contains("files: 1"), "{stdout}");
    let artifacts = parquet_artifact_dirs(&output_path);
    assert_eq!(artifacts.len(), 1, "expected one custom parquet artifact");
}

#[test]
fn graph_build_default_uses_canonical_cache_and_reports_it() {
    let dir = fixture_git_repo();

    let output = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn spur graph build");

    assert!(
        output.status.success(),
        "expected success; stderr = {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    let artifact_path = read_current_pointer(dir.path()).expect("read CURRENT");
    let artifact = read_artifact_parquet(&artifact_path).expect("load artifact");
    let git_common_dir = dir.path().join(".git").canonicalize().expect("common dir");
    let canonical = spur_graph::store::cache::lookup_canonical(
        &git_common_dir,
        &artifact.manifest_version,
        &artifact.graph_content_hash,
    )
    .expect("canonical cache path");

    assert!(canonical.is_dir(), "expected canonical artifact directory");
    assert!(
        stdout.contains(&format!("canonical: {}", canonical.display())),
        "expected canonical path in stdout, got: {stdout}"
    );
    assert_eq!(artifact_path, canonical);
}

#[test]
fn graph_build_reads_pointer_artifact_when_default_json_is_missing() {
    let dir = fixture_git_repo();

    let first = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn initial spur graph build");

    assert!(
        first.status.success(),
        "expected initial success; stderr = {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let artifact_path = read_current_pointer(dir.path()).expect("read CURRENT");
    assert!(artifact_path.is_dir(), "expected default worktree artifact");
    std::fs::remove_file(dir.path().join(".spur/graph/CURRENT")).expect("remove CURRENT pointer");
    assert!(
        dir.path().join(".spur/graph-index.pointer.json").is_file(),
        "expected pointer file to remain"
    );

    let second = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn second spur graph build");

    assert!(
        second.status.success(),
        "expected second success; stderr = {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        stdout.contains("mode: Incremental"),
        "expected build to reuse pointer artifact, got: {stdout}"
    );
    assert!(
        read_current_pointer(dir.path())
            .expect("read rewritten CURRENT")
            .is_dir(),
        "expected default CURRENT rewrite"
    );
}

#[test]
fn graph_build_custom_output_bypasses_canonical_cache() {
    let dir = fixture_git_repo();
    let output_path = dir.path().join("custom-index");

    let output = Command::new(spur_binary())
        .current_dir(dir.path())
        .args([
            "graph",
            "build",
            "--no-analyst",
            "--output",
            output_path.to_str().expect("utf8 path"),
        ])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn spur graph build");

    assert!(
        output.status.success(),
        "expected success; stderr = {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    let artifacts = parquet_artifact_dirs(&output_path);
    assert_eq!(artifacts.len(), 1, "expected one custom parquet artifact");
    assert!(
        !dir.path().join(".spur/graph").exists(),
        "custom output should not install the default worktree artifact"
    );
    assert!(
        !dir.path().join(".git/spur-graph").exists(),
        "custom output should not write the canonical cache"
    );
    assert!(
        !stdout.contains("canonical:"),
        "custom output should not report canonical cache path: {stdout}"
    );
}

#[test]
fn graph_build_rejects_legacy_json_output_path() {
    let dir = fixture_tree();
    let output_path = dir.path().join("custom-index.json");

    let output = Command::new(spur_binary())
        .current_dir(dir.path())
        .args([
            "graph",
            "build",
            "--output",
            output_path.to_str().expect("utf8 path"),
        ])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn spur graph build");

    assert!(
        !output.status.success(),
        "expected legacy file output to fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Parquet directory") && stderr.contains("--output"),
        "expected clear directory-layout error, got: {stderr}"
    );
}

#[test]
fn graph_build_triggers_analyst_rebuild_when_duckdb_present() {
    // Skip if duckdb isn't on PATH - soft-fail path is covered separately.
    let duckdb_found = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("duckdb").is_file()))
        .unwrap_or(false);
    if !duckdb_found {
        eprintln!("skipping: duckdb CLI not on PATH");
        return;
    }

    let dir = fixture_git_repo();
    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build", "--workspace", "--quiet"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let db = dir.path().join(".spur/analyst.duckdb");
    assert!(db.is_file(), "analyst DB should exist at {}", db.display());
}

fn parquet_artifact_dirs(base: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut dirs = std::fs::read_dir(base)
        .unwrap_or_else(|err| panic!("read output base `{}`: {err}", base.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| {
            path.is_dir() && path.extension().and_then(|ext| ext.to_str()) == Some("parquet")
        })
        .collect::<Vec<_>>();
    dirs.sort();
    dirs
}
