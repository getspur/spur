use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use spur_graph::git_walk::GitWalkConfig;
use spur_graph::schema::ChangeKind;
use spur_graph::temporal::symbol_history;
use tempfile::TempDir;

#[test]
fn add_modify_modify_rename_preserves_pre_rename_symbol_identity() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());

    write(
        dir.path(),
        "src/lib.rs",
        b"pub fn alpha(input: u32) -> u32 { input + 1 }\n",
    );
    let add_sha = commit_at(dir.path(), "add alpha", 1);

    write(
        dir.path(),
        "src/lib.rs",
        b"pub fn alpha(input: u32) -> u32 { let next = input + 2; next }\n",
    );
    let modify_one_sha = commit_at(dir.path(), "modify alpha once", 2);

    write(
        dir.path(),
        "src/lib.rs",
        b"pub fn alpha(input: u32) -> u32 { let next = input + 3; next }\n",
    );
    let modify_two_sha = commit_at(dir.path(), "modify alpha twice", 3);

    std::fs::rename(
        dir.path().join("src/lib.rs"),
        dir.path().join("src/renamed.rs"),
    )
    .unwrap();
    let rename_sha = commit_at(dir.path(), "rename alpha file", 4);

    let (graph, commits) =
        spur_graph::git_walk::run_full_walk_into(dir.path(), &GitWalkConfig::default()).unwrap();
    let stable_id_at_tip = stable_id_for_snapshot(&graph, &rename_sha, "alpha");
    let history = symbol_history(&graph, &commits, &stable_id_at_tip);
    let commit_times: HashMap<_, _> = commits
        .commits
        .iter()
        .map(|commit| (commit.sha.as_str(), commit.author_time))
        .collect();

    assert_eq!(
        history
            .iter()
            .map(|(sha, _, _)| sha.as_str())
            .collect::<Vec<_>>(),
        vec![add_sha, modify_one_sha, modify_two_sha, rename_sha]
    );
    assert!(matches!(history[0].1, ChangeKind::Added));
    assert!(matches!(history[1].1, ChangeKind::Modified));
    assert!(matches!(history[2].1, ChangeKind::Modified));
    assert!(matches!(history[3].1, ChangeKind::RenamedFrom(_)));

    let pre_rename_id = &history[0].2.stable_symbol_id;
    assert_eq!(&history[1].2.stable_symbol_id, pre_rename_id);
    assert_eq!(&history[2].2.stable_symbol_id, pre_rename_id);
    assert_ne!(&history[3].2.stable_symbol_id, pre_rename_id);

    for pair in history.windows(2) {
        let earlier = commit_times[pair[0].0.as_str()];
        let later = commit_times[pair[1].0.as_str()];
        assert!(
            earlier < later,
            "history timestamps must increase: {earlier} !< {later}"
        );
    }
}

fn stable_id_for_snapshot(
    graph: &spur_graph::schema::GraphIndexArtifact,
    commit: &str,
    entity_name: &str,
) -> String {
    graph
        .symbol_snapshots
        .iter()
        .find(|snapshot| snapshot.key.commit == commit && snapshot.entity_name == entity_name)
        .unwrap_or_else(|| panic!("missing snapshot `{entity_name}` at `{commit}`"))
        .key
        .stable_symbol_id
        .clone()
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
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().expect("fixture path has parent")).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn commit_at(dir: &Path, message: &str, offset_seconds: i64) -> String {
    git(dir, &["add", "-A"]);
    let timestamp = format!("@{} +0000", 1_700_000_000 + offset_seconds);
    let output = Command::new("git")
        .current_dir(dir)
        .env("GIT_AUTHOR_DATE", &timestamp)
        .env("GIT_COMMITTER_DATE", &timestamp)
        .args(["commit", "-q", "--allow-empty", "-m", message])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
