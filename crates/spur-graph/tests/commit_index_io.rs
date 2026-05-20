use std::fs;
use std::path::Path;

use spur_graph::schema::{CommitIndexArtifact, WalkStrategy, GRAPH_INDEX_VERSION_TEMPORAL};
use spur_graph::store::cache::COMMIT_INDEX_POINTER_PATH;
use spur_graph::store::commit_index::{load_artifact, load_pointer, CommitIndexPointer};

fn current_schema_version() -> u32 {
    GRAPH_INDEX_VERSION_TEMPORAL
        .parse()
        .expect("temporal graph index version is numeric")
}

fn pointer(artifact_relative_path: &str) -> CommitIndexPointer {
    CommitIndexPointer {
        schema_version: current_schema_version(),
        artifact_relative_path: artifact_relative_path.to_string(),
        indexed_at: "2026-05-21T00:00:00Z".to_string(),
        refs: Default::default(),
    }
}

fn write_pointer_fixture(worktree: &Path, schema_version: u32, artifact_relative_path: &str) {
    let path = worktree.join(COMMIT_INDEX_POINTER_PATH);
    fs::create_dir_all(path.parent().expect("pointer has parent")).expect("create pointer dir");
    fs::write(
        path,
        serde_json::json!({
            "schema_version": schema_version,
            "artifact_relative_path": artifact_relative_path,
            "indexed_at": "2026-05-21T00:00:00Z",
            "refs": {}
        })
        .to_string(),
    )
    .expect("write pointer fixture");
}

fn write_commit_index_artifact(path: &Path) {
    let artifact = CommitIndexArtifact {
        schema_version: current_schema_version(),
        commits: Vec::new(),
        refs: Default::default(),
        indexed_at: "2026-05-21T00:00:00Z".to_string(),
        walk_strategy: WalkStrategy::Reachable,
    };
    fs::create_dir_all(path.parent().expect("artifact has parent")).expect("create artifact dir");
    fs::write(
        path,
        serde_json::to_string_pretty(&artifact).expect("encode artifact"),
    )
    .expect("write artifact fixture");
}

#[test]
fn load_pointer_rejects_v1_schema_version() {
    let worktree = tempfile::tempdir().expect("tempdir");
    write_pointer_fixture(worktree.path(), 1, ".spur/commit-index.json");

    let error = load_pointer(worktree.path()).expect_err("v1 pointer should be rejected");
    let message = error.to_string();

    assert!(
        message.contains("schema_version") && message.contains('1'),
        "unexpected error: {message}"
    );
}

#[test]
fn load_artifact_rejects_absolute_artifact_relative_path() {
    let worktree = tempfile::tempdir().expect("tempdir");
    let pointer = pointer("/etc/passwd");

    let error =
        load_artifact(worktree.path(), &pointer).expect_err("absolute path should be rejected");
    let message = error.to_string();

    assert!(
        message.contains("artifact_relative_path") && message.contains("absolute"),
        "unexpected error: {message}"
    );
}

#[test]
fn load_artifact_rejects_parent_traversal() {
    let worktree = tempfile::tempdir().expect("tempdir");
    let pointer = pointer("../../foo.json");

    let error =
        load_artifact(worktree.path(), &pointer).expect_err("parent traversal should be rejected");
    let message = error.to_string();

    assert!(
        message.contains("artifact_relative_path") && message.contains("parent"),
        "unexpected error: {message}"
    );
}

#[cfg(unix)]
#[test]
fn load_artifact_rejects_path_escaping_dot_spur() {
    let worktree = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let outside_artifact = outside.path().join("commit-index.json");
    write_commit_index_artifact(&outside_artifact);

    fs::create_dir_all(worktree.path().join(".spur")).expect("create .spur");
    std::os::unix::fs::symlink(outside.path(), worktree.path().join(".spur/outside"))
        .expect("create escape symlink");

    let pointer = pointer(".spur/outside/commit-index.json");
    let error =
        load_artifact(worktree.path(), &pointer).expect_err("escaping path should be rejected");
    let message = error.to_string();

    assert!(
        message.contains("artifact_relative_path") && message.contains("escapes"),
        "unexpected error: {message}"
    );
}
