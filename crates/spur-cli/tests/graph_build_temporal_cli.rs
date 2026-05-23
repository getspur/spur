use std::path::{Path, PathBuf};
use std::process::Command;

use spur_cli::commands::graph::{self, GraphBuildOptions};
use spur_graph::store::commit_index::{load_artifact as load_commit_index_artifact, load_pointer};
use spur_graph::{load_artifact, read_artifact_header_parquet};

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

#[test]
fn graph_build_with_temporal_writes_temporal_parquets_and_manifest_counts() {
    let _temporal_env = EnvGuard::remove("SPUR_GRAPH_WITH_TEMPORAL");
    let repo = temporal_repo_fixture();
    let output = repo.path().join("graph-output");

    graph::build(GraphBuildOptions {
        root: Some(repo.path().to_path_buf()),
        workspace: false,
        output: Some(output.clone()),
        quiet: true,
        skip_analyst: true,
        with_temporal: true,
    })
    .expect("graph build with temporal");

    let artifact_dir = single_parquet_artifact_dir(&output);
    assert!(artifact_dir.join("commits.parquet").is_file());
    assert!(artifact_dir.join("symbol_snapshots.parquet").is_file());
    assert!(artifact_dir.join("temporal_edges.parquet").is_file());

    let manifest = read_artifact_header_parquet(&artifact_dir).expect("read manifest");
    assert!(
        manifest.row_counts.commits >= 2,
        "expected at least two commits, got {}",
        manifest.row_counts.commits
    );
    assert!(
        manifest.row_counts.symbol_snapshots >= 1,
        "expected at least one symbol snapshot, got {}",
        manifest.row_counts.symbol_snapshots
    );
    assert!(
        manifest.row_counts.temporal_edges >= 1,
        "expected at least one temporal edge, got {}",
        manifest.row_counts.temporal_edges
    );

    let artifact = load_artifact(&artifact_dir).expect("round-trip artifact");
    assert_eq!(artifact.commits.len(), manifest.row_counts.commits);

    let pointer = load_pointer(repo.path())
        .expect("load commit-index pointer")
        .expect("commit-index pointer exists");
    let commit_index =
        load_commit_index_artifact(repo.path(), &pointer).expect("load commit-index artifact");
    assert_eq!(commit_index.commits.len(), manifest.row_counts.commits);
}

#[test]
fn graph_build_without_temporal_leaves_temporal_parquets_absent() {
    let _temporal_env = EnvGuard::remove("SPUR_GRAPH_WITH_TEMPORAL");
    let repo = temporal_repo_fixture();
    let output = repo.path().join("graph-output");

    graph::build(GraphBuildOptions {
        root: Some(repo.path().to_path_buf()),
        workspace: false,
        output: Some(output.clone()),
        quiet: true,
        skip_analyst: true,
        with_temporal: false,
    })
    .expect("graph build without temporal");

    let artifact_dir = single_parquet_artifact_dir(&output);
    assert!(!artifact_dir.join("commits.parquet").exists());
    assert!(!artifact_dir.join("symbol_snapshots.parquet").exists());
    assert!(!artifact_dir.join("temporal_edges.parquet").exists());

    let manifest = read_artifact_header_parquet(&artifact_dir).expect("read manifest");
    assert_eq!(manifest.row_counts.commits, 0);
}

fn temporal_repo_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "spur@example.test"]);
    git(dir.path(), &["config", "user.name", "Spur Test"]);

    std::fs::create_dir_all(dir.path().join("src")).expect("mkdir src");
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn value() -> u32 {\n    1\n}\n",
    )
    .expect("write initial source");
    git(dir.path(), &["add", "src/lib.rs"]);
    git(dir.path(), &["commit", "-q", "-m", "initial"]);

    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn value() -> u32 {\n    2\n}\n",
    )
    .expect("write updated source");
    git(dir.path(), &["add", "src/lib.rs"]);
    git(dir.path(), &["commit", "-q", "-m", "update value"]);

    dir
}

fn single_parquet_artifact_dir(base: &Path) -> PathBuf {
    let mut dirs = std::fs::read_dir(base)
        .unwrap_or_else(|error| panic!("read output base `{}`: {error}", base.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| {
            path.is_dir() && path.extension().and_then(|ext| ext.to_str()) == Some("parquet")
        })
        .collect::<Vec<_>>();
    dirs.sort();
    assert_eq!(dirs.len(), 1, "expected one parquet artifact dir");
    dirs.remove(0)
}

fn git(root: &Path, args: &[&str]) {
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
