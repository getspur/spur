//! Regression: a commit-index *pointer* that references a missing artifact file
//! (an "orphaned pointer", e.g. when `.spur/commit-index.json` was cleaned while
//! the pointer survived) must degrade to a cold temporal walk, not hard-fail the
//! whole walk. Previously `load_incremental_base` propagated the `canonicalize`
//! error and `graph build` fell back to a structural-only artifact on every run,
//! never self-healing. See git_walk.rs::load_incremental_base.

use std::fs;
use std::path::Path;
use std::process::Command;

use spur_graph::git_walk::{run_full_walk_into, GitWalkConfig};
use spur_graph::schema::GRAPH_INDEX_VERSION_TEMPORAL;
use spur_graph::store::cache::COMMIT_INDEX_POINTER_PATH;

#[test]
fn orphaned_commit_index_pointer_falls_back_to_cold_walk() {
    let dir = tempfile::tempdir().expect("tempdir");
    init_repo(dir.path());
    write(dir.path(), "lib.rs", b"pub fn only_fn() -> u32 { 1 }\n");
    let head = commit(dir.path(), "add lib");

    // Plant an orphaned pointer: it references `.spur/commit-index.json`, but we
    // never write that artifact. This is the production failure state.
    write_orphaned_pointer(dir.path(), ".spur/commit-index.json");
    assert!(
        !dir.path().join(".spur/commit-index.json").exists(),
        "fixture invariant: the referenced artifact must be absent"
    );

    // Before the fix this returned Err("canonicalize commit-index artifact ...").
    let (_graph, commits) = run_full_walk_into(dir.path(), &GitWalkConfig::default(), None, None)
        .expect("orphaned pointer must degrade to a cold walk, not hard-fail");

    // The cold walk must have actually traversed history (not produced an empty
    // index), proving real self-heal rather than a silent no-op.
    assert!(
        commits.commits.iter().any(|commit| commit.sha == head),
        "cold walk should include HEAD commit `{head}`"
    );
}

fn write_orphaned_pointer(worktree: &Path, artifact_relative_path: &str) {
    let schema_version: u32 = GRAPH_INDEX_VERSION_TEMPORAL
        .parse()
        .expect("temporal graph index version is numeric");
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
    .expect("write orphaned pointer");
}

fn init_repo(dir: &Path) {
    for args in [
        ["init", "-q", "-b", "main"].as_slice(),
        ["config", "user.email", "t@t"].as_slice(),
        ["config", "user.name", "T"].as_slice(),
    ] {
        git(dir, args);
    }
}

fn write(dir: &Path, path: &str, contents: &[u8]) {
    std::fs::write(dir.join(path), contents).unwrap();
}

fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
    let output = Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success(), "git rev-parse HEAD failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
