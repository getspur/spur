#![cfg(unix)]

use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use spur_graph::git_walk::{run_full_walk_into, GitWalkConfig};
use spur_graph::schema::GRAPH_INDEX_VERSION_TEMPORAL;
use spur_graph::store::commit_index::{save_artifact, save_pointer, CommitIndexPointer};
use tempfile::TempDir;

#[test]
fn incremental_run_full_walk_uses_prior_pointer_to_ingest_only_new_commits() {
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());

    append_commits(repo.path(), 1..=5);
    let (first_graph, first_commits) =
        run_full_walk_into(repo.path(), &GitWalkConfig::default()).unwrap();
    save_temporal_artifacts(repo.path(), &first_graph, &first_commits);

    let git_log = repo.path().join("git-show.log");
    let wrapper_dir = TempDir::new().unwrap();
    let _git_guard = GitWrapperGuard::install(wrapper_dir.path(), &git_log);

    fs::write(&git_log, "").unwrap();
    append_commits(repo.path(), 6..=8);
    let (incremental_graph, incremental_commits) =
        run_full_walk_into(repo.path(), &GitWalkConfig::default()).unwrap();

    let ingested_commits = logged_show_calls(&git_log);
    assert_eq!(
        ingested_commits.len(),
        3,
        "second pass should ingest only new commits, got {ingested_commits:?}"
    );

    let spur_dir = repo.path().join(".spur");
    let saved_spur_dir = repo.path().join(".spur.saved");
    fs::rename(&spur_dir, &saved_spur_dir).unwrap();
    let (cold_graph, cold_commits) =
        run_full_walk_into(repo.path(), &GitWalkConfig::default()).unwrap();
    fs::rename(&saved_spur_dir, &spur_dir).unwrap();

    assert_eq!(incremental_graph, cold_graph);
    assert_eq!(incremental_commits.commits, cold_commits.commits);
    assert_eq!(incremental_commits.refs, cold_commits.refs);
    assert_eq!(
        incremental_commits.walk_strategy,
        cold_commits.walk_strategy
    );
}

fn save_temporal_artifacts(
    worktree: &Path,
    graph: &spur_graph::schema::GraphIndexArtifact,
    commits: &spur_graph::schema::CommitIndexArtifact,
) {
    spur_graph::store::write_artifact(graph, &worktree.join(".spur/graph-index.json"))
        .expect("save graph artifact");
    save_artifact(worktree, ".spur/commit-index.json", commits).expect("save commit index");
    save_pointer(
        worktree,
        &CommitIndexPointer {
            schema_version: current_schema_version(),
            artifact_relative_path: ".spur/commit-index.json".to_string(),
            indexed_at: commits.indexed_at.clone(),
            refs: commits.refs.clone(),
        },
    )
    .expect("save commit index pointer");
}

fn append_commits(worktree: &Path, range: std::ops::RangeInclusive<u32>) -> Vec<String> {
    range
        .map(|value| {
            write(
                worktree,
                "lib.rs",
                format!("pub fn value() -> u32 {{ {value} }}\n").as_bytes(),
            );
            commit(worktree, &format!("value {value}"))
        })
        .collect()
}

fn current_schema_version() -> u32 {
    GRAPH_INDEX_VERSION_TEMPORAL
        .parse()
        .expect("temporal graph index version is numeric")
}

fn logged_show_calls(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .filter(|line| !line.is_empty())
        .collect()
}

struct GitWrapperGuard {
    old_path: Option<OsString>,
    old_log: Option<OsString>,
    old_real_git: Option<OsString>,
}

impl GitWrapperGuard {
    fn install(wrapper_dir: &Path, log_path: &Path) -> Self {
        let old_path = env::var_os("PATH");
        let old_log = env::var_os("SPUR_GIT_SHOW_LOG");
        let old_real_git = env::var_os("SPUR_REAL_GIT");
        let real_git = find_real_git(old_path.as_ref().expect("PATH must be set"));

        let wrapper_path = wrapper_dir.join("git");
        fs::write(
            &wrapper_path,
            "#!/bin/sh\n\
             if [ \"$1\" = \"show\" ] && [ \"$2\" = \"-s\" ]; then\n\
             \techo \"$*\" >> \"$SPUR_GIT_SHOW_LOG\"\n\
             fi\n\
             exec \"$SPUR_REAL_GIT\" \"$@\"\n",
        )
        .expect("write git wrapper");
        let mut permissions = fs::metadata(&wrapper_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper_path, permissions).unwrap();

        let mut paths = vec![wrapper_dir.to_path_buf()];
        paths.extend(env::split_paths(
            old_path.as_ref().expect("PATH must be set"),
        ));
        env::set_var("PATH", env::join_paths(paths).unwrap());
        env::set_var("SPUR_GIT_SHOW_LOG", log_path);
        env::set_var("SPUR_REAL_GIT", real_git);

        Self {
            old_path,
            old_log,
            old_real_git,
        }
    }
}

impl Drop for GitWrapperGuard {
    fn drop(&mut self) {
        restore_var("PATH", self.old_path.take());
        restore_var("SPUR_GIT_SHOW_LOG", self.old_log.take());
        restore_var("SPUR_REAL_GIT", self.old_real_git.take());
    }
}

fn restore_var(name: &str, value: Option<OsString>) {
    if let Some(value) = value {
        env::set_var(name, value);
    } else {
        env::remove_var(name);
    }
}

fn find_real_git(path: &OsString) -> PathBuf {
    env::split_paths(path)
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("could not locate real git in PATH"))
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
    fs::write(dir.join(path), contents).unwrap();
}

fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["add", "lib.rs"]);
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
