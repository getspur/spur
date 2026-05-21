use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use spur_graph::git_walk::{plan_incremental_walk, GitWalkConfig, IncrementalPlan};
use spur_graph::temporal::{resolve_symbol_at, symbol_history, Resolution};
use tempfile::TempDir;

struct Step {
    label: &'static str,
    apply: fn(&Path),
}

struct ScriptedHistory {
    shas: BTreeMap<&'static str, String>,
}

fn build_history(dir: &Path) -> ScriptedHistory {
    init_repo(dir);

    let steps: &[Step] = &[
        Step {
            label: "add",
            apply: |dir| {
                write(
                    dir,
                    "lib.rs",
                    b"pub fn a(input: u32) -> u32 { let doubled = input * 2; doubled + 10 }\npub fn helper() -> u32 { 1 }\n",
                );
            },
        },
        Step {
            label: "modify",
            apply: |dir| {
                write(
                    dir,
                    "lib.rs",
                    b"pub fn a(input: u32) -> u32 { let doubled = input * 2; doubled + 10 }\npub fn helper() -> u32 { 2 }\n",
                );
            },
        },
        Step {
            label: "delete",
            apply: |dir| {
                write(
                    dir,
                    "lib.rs",
                    b"pub fn a(input: u32) -> u32 { let doubled = input * 2; doubled + 10 }\n",
                );
            },
        },
        Step {
            label: "rename-file",
            apply: |dir| {
                std::fs::rename(dir.join("lib.rs"), dir.join("renamed.rs")).unwrap();
            },
        },
        Step {
            label: "rename-symbol",
            apply: |dir| {
                write(
                    dir,
                    "renamed.rs",
                    b"pub fn b(input: u32) -> u32 { let doubled = input * 2; doubled + 10 }\n",
                );
            },
        },
        Step {
            label: "squash-readd",
            apply: |dir| {
                write(dir, "helper.rs", b"pub fn helper() -> u32 { 3 }\n");
            },
        },
    ];

    let mut shas = BTreeMap::new();
    for step in steps {
        (step.apply)(dir);
        shas.insert(step.label, commit(dir, step.label));
        if step.label == "modify" {
            git(dir, &["branch", "side"]);
        }
    }

    git(dir, &["checkout", "-q", "side"]);
    write(dir, "side.rs", b"pub fn side_symbol() -> u32 { 20 }\n");
    shas.insert("side", commit(dir, "side"));

    git(dir, &["checkout", "-q", "main"]);
    git(dir, &["merge", "--no-ff", "-q", "side", "-m", "merge"]);
    shas.insert("merge", rev_parse(dir, "HEAD"));

    ScriptedHistory { shas }
}

#[test]
fn property_resolve_at_anchor_matches_script() {
    let dir = TempDir::new().unwrap();
    let history = build_history(dir.path());

    let (graph, commits) =
        spur_graph::git_walk::run_full_walk_into(dir.path(), &GitWalkConfig::default()).unwrap();
    let add_sha = history.sha("add");
    let merge_sha = history.sha("merge");
    let initial_a = stable_id_for_snapshot(&graph, add_sha, "a");
    let final_b = stable_id_for_entity(&graph, "b");

    let resolution = resolve_symbol_at(&graph, &commits, &initial_a, add_sha, merge_sha);
    match resolution {
        Resolution::Found { value, .. } => assert_eq!(value, final_b),
        other => panic!("expected Found({final_b}), got {other:?}"),
    }

    let history_events = symbol_history(&graph, &commits, &initial_a);
    assert!(
        history_events.len() >= 3,
        "expected add + file rename + symbol rename history, got {history_events:?}"
    );
}

#[test]
fn resolve_at_intermediate_commit_returns_latest_prior_snapshot() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());

    write(
        dir.path(),
        "lib.rs",
        b"pub fn tracked(input: u32) -> u32 { input + 1 }\n",
    );
    let c1 = commit(dir.path(), "add tracked");

    write(
        dir.path(),
        "other.rs",
        b"pub fn unrelated() -> u32 { 10 }\n",
    );
    let c2 = commit(dir.path(), "add unrelated");

    write(
        dir.path(),
        "lib.rs",
        b"pub fn tracked(input: u32) -> u32 { input + 2 }\n",
    );
    let c3 = commit(dir.path(), "modify tracked");

    let (graph, commits) =
        spur_graph::git_walk::run_full_walk_into(dir.path(), &GitWalkConfig::default()).unwrap();
    let stable_id = stable_id_for_snapshot(&graph, &c1, "tracked");

    assert!(
        !graph
            .symbol_snapshots
            .iter()
            .any(|snapshot| snapshot.key.commit == c2
                && snapshot.key.stable_symbol_id == stable_id),
        "fixture must not contain a tracked snapshot at the intermediate commit"
    );
    assert_eq!(stable_id_for_snapshot(&graph, &c3, "tracked"), stable_id);

    let at_intermediate = resolve_symbol_at(&graph, &commits, &stable_id, &c2, &c2);
    match at_intermediate {
        Resolution::Found { value, .. } => assert_eq!(value, stable_id),
        other => panic!("expected latest prior snapshot from {c1}, got {other:?}"),
    }

    let through_later_target = resolve_symbol_at(&graph, &commits, &stable_id, &c2, &c3);
    match through_later_target {
        Resolution::Found { value, .. } => assert_eq!(value, stable_id),
        other => panic!("expected latest prior snapshot from {c1}, got {other:?}"),
    }
}

#[test]
fn shallow_clone_fails_closed() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    write(dir.path(), "lib.rs", b"pub fn a() {}\n");
    let sha = commit(dir.path(), "init");
    std::fs::write(dir.path().join(".git/shallow"), format!("{sha}\n")).unwrap();

    let result = spur_graph::git_walk::run_full_walk_into(dir.path(), &GitWalkConfig::default());

    assert!(result.is_err(), "shallow repo must fail closed");
}

#[test]
fn force_push_recovery_rebuilds_diverged_range() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    write(dir.path(), "lib.rs", b"pub fn a() -> u32 { 1 }\n");
    let sha1 = commit(dir.path(), "c1");
    write(dir.path(), "lib.rs", b"pub fn a() -> u32 { 2 }\n");
    let sha2 = commit(dir.path(), "c2");
    let (_old_graph, old_commits) =
        spur_graph::git_walk::run_full_walk_into(dir.path(), &GitWalkConfig::default()).unwrap();
    assert!(old_commits.commits.iter().any(|commit| commit.sha == sha2));

    git(dir.path(), &["reset", "--hard", &sha1]);
    write(dir.path(), "lib.rs", b"pub fn a() -> u32 { 3 }\n");
    let replacement = commit(dir.path(), "c2 replacement");

    let plan = plan_incremental_walk(dir.path(), Some(&sha2), &replacement).unwrap();
    assert!(matches!(
        plan,
        IncrementalPlan::ForcePushRecover {
            merge_base: Some(base),
            to,
        } if base == sha1 && to == replacement
    ));

    let (_new_graph, new_commits) =
        spur_graph::git_walk::run_full_walk_into(dir.path(), &GitWalkConfig::default()).unwrap();
    assert!(new_commits
        .commits
        .iter()
        .any(|commit| commit.sha == replacement));
    assert!(!new_commits.commits.iter().any(|commit| commit.sha == sha2));
}

impl ScriptedHistory {
    fn sha(&self, label: &'static str) -> &str {
        self.shas
            .get(label)
            .unwrap_or_else(|| panic!("missing scripted commit `{label}`"))
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

fn stable_id_for_entity(
    graph: &spur_graph::schema::GraphIndexArtifact,
    entity_name: &str,
) -> String {
    let mut snapshots: Vec<_> = graph
        .symbol_snapshots
        .iter()
        .filter(|snapshot| snapshot.entity_name == entity_name)
        .collect();
    snapshots.sort_by(|left, right| left.key.commit.cmp(&right.key.commit));
    snapshots
        .last()
        .unwrap_or_else(|| panic!("missing snapshot `{entity_name}`"))
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
