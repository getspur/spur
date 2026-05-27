use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use spur_graph::git_walk::{run_full_walk_into, GitWalkConfig};
use spur_graph::schema::{ChangeKind, EdgeEndpoint, GraphIndexArtifact, RelationKind};
use tempfile::TempDir;

#[test]
fn gix_diff_matches_cli_for_linear_rename_and_merge_history() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    std::fs::create_dir(dir.path().join("src")).unwrap();

    write(dir.path(), "src/a.rs", b"pub fn alpha() -> u32 { 1 }\n");
    commit(dir.path(), "add alpha");

    write(dir.path(), "src/a.rs", b"pub fn alpha() -> u32 { 2 }\n");
    commit(dir.path(), "modify alpha");

    git(dir.path(), &["mv", "src/a.rs", "src/b.rs"]);
    commit(dir.path(), "rename alpha file");

    git(dir.path(), &["branch", "side"]);

    write(dir.path(), "src/c.rs", b"pub fn main_only() -> u32 { 3 }\n");
    commit(dir.path(), "add main file");

    git(dir.path(), &["checkout", "-q", "side"]);
    write(dir.path(), "src/b.rs", b"pub fn alpha() -> u32 { 4 }\n");
    commit(dir.path(), "modify alpha on side");

    git(dir.path(), &["checkout", "-q", "main"]);
    git(
        dir.path(),
        &["merge", "--no-ff", "-q", "side", "-m", "merge side"],
    );

    let mut cli_config = GitWalkConfig::default();
    cli_config.use_gix_diff = false;
    let (cli_graph, cli_commits) = run_full_walk_into(dir.path(), &cli_config, None).unwrap();
    let mut gix_config = GitWalkConfig::default();
    gix_config.use_gix_diff = true;
    let (gix_graph, gix_commits) = run_full_walk_into(dir.path(), &gix_config, None).unwrap();

    assert_eq!(cli_commits.commits, gix_commits.commits);
    assert_eq!(
        snapshot_touch_pairs(&cli_graph),
        snapshot_touch_pairs(&gix_graph)
    );
}

fn snapshot_touch_pairs(graph: &GraphIndexArtifact) -> BTreeSet<(String, String, String)> {
    graph
        .temporal_edges
        .iter()
        .filter_map(|edge| {
            let EdgeEndpoint::Commit { sha } = &edge.source else {
                return None;
            };
            if edge.relation != RelationKind::Touches
                || !matches!(
                    edge.change_kind,
                    Some(ChangeKind::Added | ChangeKind::Modified | ChangeKind::Deleted)
                )
            {
                return None;
            }
            let EdgeEndpoint::Snapshot { key } = &edge.target else {
                return None;
            };
            Some((
                sha.clone(),
                key.stable_symbol_id.clone(),
                key.commit.clone(),
            ))
        })
        .collect()
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
