use std::path::Path;
use std::process::Command;

use spur_graph::git_walk::GitWalkConfig;
use spur_graph::schema::{ChangeKind, EdgeEndpoint, RenamePrev, SnapshotKey};
use spur_graph::temporal::symbol_history;
use tempfile::TempDir;

#[test]
fn snapshot_rename_edges_drive_symbol_history() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());

    write(
        dir.path(),
        "src/lib.rs",
        b"pub fn alpha(input: u32) -> u32 { let doubled = input * 2; doubled + 10 }\n",
    );
    let add_sha = commit(dir.path(), "add alpha");

    std::fs::rename(
        dir.path().join("src/lib.rs"),
        dir.path().join("src/renamed.rs"),
    )
    .unwrap();
    let rename_file_sha = commit(dir.path(), "rename file");

    write(
        dir.path(),
        "src/renamed.rs",
        b"pub fn beta(input: u32) -> u32 { let doubled = input * 2; doubled + 10 }\n",
    );
    let rename_symbol_sha = commit(dir.path(), "rename symbol");

    let (graph, commits) =
        spur_graph::git_walk::run_full_walk_into(dir.path(), &GitWalkConfig::default()).unwrap();

    let add_key = snapshot_key(&graph, &add_sha, "alpha");
    let file_rename_key = snapshot_key(&graph, &rename_file_sha, "alpha");
    let symbol_rename_key = snapshot_key(&graph, &rename_symbol_sha, "beta");
    let file_rename_previous = commit_edge_rename_previous(&graph, &file_rename_key);
    let symbol_rename_previous = commit_edge_rename_previous(&graph, &symbol_rename_key);

    assert_eq!(
        file_rename_previous.stable_symbol_id,
        add_key.stable_symbol_id
    );
    assert_eq!(
        symbol_rename_previous.stable_symbol_id,
        file_rename_key.stable_symbol_id
    );
    assert_snapshot_rename_edge(&graph, &file_rename_previous, &file_rename_key);
    assert_snapshot_rename_edge(&graph, &symbol_rename_previous, &symbol_rename_key);

    let mut graph_without_commit_rename_predecessors = graph.clone();
    for edge in &mut graph_without_commit_rename_predecessors.temporal_edges {
        if matches!(
            (&edge.source, &edge.target, &edge.change_kind),
            (
                EdgeEndpoint::Commit { .. },
                EdgeEndpoint::Snapshot { .. },
                Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(_)))
            )
        ) {
            edge.change_kind = Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(SnapshotKey {
                stable_symbol_id: "detached".into(),
                commit: "detached".into(),
            })));
        }
    }

    let history = symbol_history(
        &graph_without_commit_rename_predecessors,
        &commits,
        &add_key.stable_symbol_id,
    );
    let history_keys: Vec<_> = history.iter().map(|(_, _, key)| key.clone()).collect();

    assert_eq!(
        history_keys,
        vec![add_key, file_rename_key, symbol_rename_key]
    );
}

fn assert_snapshot_rename_edge(
    graph: &spur_graph::schema::GraphIndexArtifact,
    previous: &SnapshotKey,
    next: &SnapshotKey,
) {
    assert!(
        graph.temporal_edges.iter().any(|edge| {
            matches!(
                (&edge.source, &edge.target, &edge.change_kind),
                (
                    EdgeEndpoint::Snapshot { key: source },
                    EdgeEndpoint::Snapshot { key: target },
                    Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(rename_previous)))
                ) if source == previous && target == next && rename_previous == previous
            )
        }),
        "missing snapshot rename edge {previous:?} -> {next:?}"
    );
}

fn commit_edge_rename_previous(
    graph: &spur_graph::schema::GraphIndexArtifact,
    key: &SnapshotKey,
) -> SnapshotKey {
    graph
        .temporal_edges
        .iter()
        .find_map(
            |edge| match (&edge.source, &edge.target, &edge.change_kind) {
                (
                    EdgeEndpoint::Commit { .. },
                    EdgeEndpoint::Snapshot { key: target },
                    Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(previous))),
                ) if target == key => Some(previous.clone()),
                _ => None,
            },
        )
        .unwrap_or_else(|| panic!("missing commit rename edge for {key:?}"))
}

fn snapshot_key(
    graph: &spur_graph::schema::GraphIndexArtifact,
    commit: &str,
    entity_name: &str,
) -> SnapshotKey {
    graph
        .symbol_snapshots
        .iter()
        .find(|snapshot| snapshot.key.commit == commit && snapshot.entity_name == entity_name)
        .unwrap_or_else(|| panic!("missing snapshot `{entity_name}` at `{commit}`"))
        .key
        .clone()
}

fn init_repo(dir: &Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
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
