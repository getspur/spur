use std::path::Path;
use std::process::Command;

use spur_graph::git_walk::GitWalkConfig;
use spur_graph::schema::{ChangeKind, EdgeEndpoint, GraphIndexArtifact, RelationKind, SnapshotKey};
use tempfile::TempDir;

#[test]
fn full_walk_emits_merge_symbol_edges_against_each_parent() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    commit(dir.path(), "base");
    git(dir.path(), &["branch", "side"]);

    write(dir.path(), "a.rs", b"pub fn only_a() -> u32 { 1 }\n");
    let parent_a = commit(dir.path(), "add only_a");

    git(dir.path(), &["checkout", "-q", "side"]);
    write(dir.path(), "b.rs", b"pub fn only_b() -> u32 { 2 }\n");
    let parent_b = commit(dir.path(), "add only_b");

    git(dir.path(), &["checkout", "-q", "main"]);
    git(
        dir.path(),
        &["merge", "--no-ff", "-q", "side", "-m", "merge"],
    );
    let merge = rev_parse(dir.path(), "HEAD");

    let (graph, commits) =
        spur_graph::git_walk::run_full_walk_into(dir.path(), &GitWalkConfig::default()).unwrap();
    let merge_commit = commits
        .commits
        .iter()
        .find(|commit| commit.sha == merge)
        .unwrap_or_else(|| panic!("missing merge commit `{merge}`"));
    assert_eq!(
        merge_commit.parents,
        vec![parent_a.clone(), parent_b.clone()]
    );

    assert_added_snapshot_against_parent(&graph, &merge, "only_b", &parent_a);
    assert_added_snapshot_against_parent(&graph, &merge, "only_a", &parent_b);
}

fn assert_added_snapshot_against_parent(
    graph: &GraphIndexArtifact,
    commit: &str,
    entity_name: &str,
    parent: &str,
) {
    let found = graph.temporal_edges.iter().any(|edge| {
        edge.source
            == (EdgeEndpoint::Commit {
                sha: commit.to_string(),
            })
            && edge.relation == RelationKind::Touches
            && edge.change_kind == Some(ChangeKind::Added)
            && edge.parent.as_deref() == Some(parent)
            && match &edge.target {
                EdgeEndpoint::Snapshot { key } => snapshot_has_name(graph, key, entity_name),
                _ => false,
            }
    });

    assert!(
        found,
        "missing Added edge for `{entity_name}` in merge `{commit}` against parent `{parent}`"
    );
}

fn snapshot_has_name(graph: &GraphIndexArtifact, key: &SnapshotKey, entity_name: &str) -> bool {
    graph
        .symbol_snapshots
        .iter()
        .any(|snapshot| snapshot.key == *key && snapshot.entity_name == entity_name)
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
    git(dir, &["commit", "-q", "--allow-empty", "-m", message]);
    rev_parse(dir, "HEAD")
}

fn rev_parse(dir: &Path, rev: &str) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(["rev-parse", rev])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git rev-parse {rev} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
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
