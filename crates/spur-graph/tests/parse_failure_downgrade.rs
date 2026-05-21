use std::path::{Path, PathBuf};
use std::process::Command;

use spur_graph::git_walk::GitWalkConfig;
use spur_graph::schema::{ChangeKind, EdgeEndpoint, RelationKind};
use tempfile::TempDir;

#[test]
fn parse_failure_downgrades_file_to_file_level_with_diagnostic() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    write(
        dir.path(),
        "valid.rs",
        b"pub fn valid_symbol() -> u32 { 7 }\n",
    );
    write(dir.path(), "corrupt.rs", &[0xff, 0xfe, 0xfd, b'\n']);
    let add_sha = commit(dir.path(), "add valid and corrupt rust files");
    write(
        dir.path(),
        "valid.rs",
        b"pub fn valid_symbol() -> u32 { 8 }\n",
    );
    write(
        dir.path(),
        "corrupt.rs",
        b"pub fn formerly_corrupt() -> u32 { 9 }\n",
    );
    let fix_sha = commit(dir.path(), "replace corrupt rust file");

    let (graph, _commits) =
        spur_graph::git_walk::run_full_walk_into(dir.path(), &GitWalkConfig::default()).unwrap();

    assert!(graph.temporal_edges.iter().any(|edge| {
        edge.source
            == (EdgeEndpoint::Commit {
                sha: add_sha.clone(),
            })
            && edge.target
                == (EdgeEndpoint::File {
                    path: PathBuf::from("valid.rs"),
                })
            && edge.relation == RelationKind::Touches
            && edge.change_kind == Some(ChangeKind::Added)
    }));
    assert!(graph.temporal_edges.iter().any(|edge| {
        edge.source
            == (EdgeEndpoint::Commit {
                sha: add_sha.clone(),
            })
            && edge.target
                == (EdgeEndpoint::File {
                    path: PathBuf::from("corrupt.rs"),
                })
            && edge.relation == RelationKind::Touches
            && edge.change_kind == Some(ChangeKind::Added)
    }));
    assert!(graph.temporal_edges.iter().any(|edge| {
        edge.source
            == (EdgeEndpoint::Commit {
                sha: fix_sha.clone(),
            })
            && edge.target
                == (EdgeEndpoint::File {
                    path: PathBuf::from("corrupt.rs"),
                })
            && edge.relation == RelationKind::Touches
            && edge.change_kind == Some(ChangeKind::Modified)
    }));

    assert!(graph
        .symbol_snapshots
        .iter()
        .any(|snapshot| snapshot.entity_name == "valid_symbol"));
    assert!(!graph
        .symbol_snapshots
        .iter()
        .any(|snapshot| snapshot.file_path == PathBuf::from("corrupt.rs")));
    assert!(graph.diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("parse_failed")
            && diagnostic.contains("file=corrupt.rs")
            && diagnostic.contains("side=right")
            && diagnostic.contains(&format!("sha={add_sha}"))
    }));
    assert!(graph.diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("parse_failed")
            && diagnostic.contains("file=corrupt.rs")
            && diagnostic.contains("side=left")
            && diagnostic.contains(&format!("sha={fix_sha}"))
    }));
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
