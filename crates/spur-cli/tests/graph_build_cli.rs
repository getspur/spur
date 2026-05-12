use std::process::Command;

fn spur_binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_spur"))
}

fn fixture_repo() -> tempfile::TempDir {
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

#[test]
fn graph_build_writes_default_index_and_prints_summary() {
    let dir = fixture_repo();

    let output = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build"])
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
    let dir = fixture_repo();
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
