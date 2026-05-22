use std::process::Command;

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
        stdout.contains(".spur/graph-index.json"),
        "expected default output path in stdout, got: {stdout}"
    );

    let artifact_path = dir.path().join(".spur/graph-index.json");
    assert!(artifact_path.is_file(), "expected graph index artifact");
    let artifact = spur_graph::load_artifact(&artifact_path).expect("load artifact");
    assert_eq!(artifact.files.len(), 1);
    assert!(artifact
        .symbols
        .iter()
        .any(|symbol| symbol.entity_name == "Engine"));
}

#[test]
fn graph_build_quiet_suppresses_progress_and_honors_output() {
    let dir = fixture_tree();
    let output_path = dir.path().join("custom-index.json");

    let output = Command::new(spur_binary())
        .current_dir(dir.path())
        .args([
            "graph",
            "build",
            "--quiet",
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
    assert!(output_path.is_file(), "expected custom output artifact");
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

    let artifact_path = dir.path().join(".spur/graph-index.json");
    let artifact = spur_graph::load_artifact(&artifact_path).expect("load artifact");
    let git_common_dir = dir.path().join(".git").canonicalize().expect("common dir");
    let canonical = spur_graph::store::cache::lookup_canonical(
        &git_common_dir,
        &artifact.manifest_version,
        &artifact.graph_content_hash,
    )
    .expect("canonical cache path");

    assert!(canonical.is_file(), "expected canonical artifact");
    assert!(
        stdout.contains(&format!("canonical: {}", canonical.display())),
        "expected canonical path in stdout, got: {stdout}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        assert_eq!(
            std::fs::metadata(&canonical)
                .expect("canonical metadata")
                .ino(),
            std::fs::metadata(&artifact_path)
                .expect("worktree artifact metadata")
                .ino(),
            "expected worktree artifact to be hardlinked to canonical"
        );
    }
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

    let artifact_path = dir.path().join(".spur/graph-index.json");
    assert!(
        artifact_path.is_file(),
        "expected default worktree artifact"
    );
    std::fs::remove_file(&artifact_path).expect("remove worktree artifact");
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
    assert!(artifact_path.is_file(), "expected default artifact rewrite");
}

#[test]
fn graph_build_custom_output_bypasses_canonical_cache() {
    let dir = fixture_git_repo();
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
        output.status.success(),
        "expected success; stderr = {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output_path.is_file(), "expected custom output artifact");
    assert!(
        !dir.path().join(".spur/graph-index.json").exists(),
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
