#![cfg(unix)]

use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};

use spur_graph::git_walk::{run_full_walk_into, GitWalkConfig};
use spur_graph::schema::GRAPH_INDEX_VERSION_TEMPORAL;
use spur_graph::store::commit_index::{save_artifact, save_pointer, CommitIndexPointer};
use spur_graph::store::{
    write_artifact_parquet, write_current_pointer, ArtifactStagingDir, TemporalShardSink,
    WriteOptions,
};
use spur_graph::{load_artifact, TemporalShardConfig};
use tempfile::TempDir;

#[test]
fn incremental_run_full_walk_uses_prior_pointer_to_ingest_only_new_commits() {
    let _guard = test_env_lock();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let mut config = GitWalkConfig::default();
    config.use_gix_diff = false;

    append_commits(repo.path(), 1..=5);
    let (first_graph, first_commits) =
        run_full_walk_into(repo.path(), &config, None, None).unwrap();
    save_temporal_artifacts(repo.path(), &first_graph, &first_commits);

    let git_log = repo.path().join("git-show.log");
    let wrapper_dir = TempDir::new().unwrap();
    let _git_guard = GitWrapperGuard::install(wrapper_dir.path(), &git_log);

    fs::write(&git_log, "").unwrap();
    append_commits(repo.path(), 6..=8);
    let (incremental_graph, incremental_commits) =
        run_full_walk_into(repo.path(), &config, None, None).unwrap();

    let ingested_commits = logged_show_calls(&git_log);
    assert_eq!(
        ingested_commits.len(),
        3,
        "second pass should ingest only new commits, got {ingested_commits:?}"
    );

    let spur_dir = repo.path().join(".spur");
    let saved_spur_dir = repo.path().join(".spur.saved");
    fs::rename(&spur_dir, &saved_spur_dir).unwrap();
    let (cold_graph, cold_commits) = run_full_walk_into(repo.path(), &config, None, None).unwrap();
    fs::rename(&saved_spur_dir, &spur_dir).unwrap();

    assert_eq!(incremental_graph, cold_graph);
    assert_eq!(incremental_commits.commits, cold_commits.commits);
    assert_eq!(incremental_commits.refs, cold_commits.refs);
    assert_eq!(
        incremental_commits.walk_strategy,
        cold_commits.walk_strategy
    );
}

#[test]
fn run_full_walk_with_sink_drains_temporal_rows_into_shards() {
    let _guard = test_env_lock();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let mut config = GitWalkConfig::default();
    config.use_gix_diff = false;

    append_commits(repo.path(), 1..=3);

    let artifact_dir = TempDir::new().unwrap();
    let mut sink = TemporalShardSink::new(
        artifact_dir.path().to_path_buf(),
        TemporalShardConfig {
            max_rows_per_shard: 2,
            max_commits_per_shard: 10,
        },
    )
    .unwrap();

    let (graph, _commits) =
        run_full_walk_into(repo.path(), &config, None, Some(&mut sink)).unwrap();
    assert!(graph.temporal_edges.is_empty());
    assert!(graph.symbol_snapshots.is_empty());

    let shards = sink.finalize().unwrap();
    assert!(!shards.is_empty());
    let temporal_edges: usize = shards.iter().map(|entry| entry.row_count_edges).sum();
    let symbol_snapshots: usize = shards.iter().map(|entry| entry.row_count_snapshots).sum();
    assert!(temporal_edges > 0);
    assert!(symbol_snapshots > 0);

    write_artifact_parquet(&graph, artifact_dir.path(), WriteOptions::default(), shards)
        .expect("write sharded graph parquet artifact");
    let reloaded = load_artifact(artifact_dir.path()).expect("read sharded graph parquet artifact");
    assert_eq!(reloaded.temporal_edges.len(), temporal_edges);
    assert_eq!(reloaded.symbol_snapshots.len(), symbol_snapshots);
}

#[test]
fn incremental_run_full_walk_with_sink_streams_prior_temporal_rows() {
    let _guard = test_env_lock();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let mut config = GitWalkConfig::default();
    config.use_gix_diff = false;

    append_commits(repo.path(), 1..=5);
    let (first_graph, first_commits) =
        run_full_walk_into(repo.path(), &config, None, None).unwrap();
    save_temporal_artifacts(repo.path(), &first_graph, &first_commits);

    append_commits(repo.path(), 6..=8);
    let artifact_dir = TempDir::new().unwrap();
    let mut sink = TemporalShardSink::new(
        artifact_dir.path().to_path_buf(),
        TemporalShardConfig {
            max_rows_per_shard: 3,
            max_commits_per_shard: 10,
        },
    )
    .unwrap();

    let (incremental_graph, incremental_commits) =
        run_full_walk_into(repo.path(), &config, None, Some(&mut sink)).unwrap();
    assert!(incremental_graph.temporal_edges.is_empty());
    assert!(incremental_graph.symbol_snapshots.is_empty());
    let shards = sink.finalize().unwrap();
    assert!(shards.len() > 1);

    write_artifact_parquet(
        &incremental_graph,
        artifact_dir.path(),
        WriteOptions::default(),
        shards,
    )
    .expect("write streamed incremental graph parquet artifact");
    let streamed = load_artifact(artifact_dir.path())
        .expect("read streamed incremental graph parquet artifact");

    let spur_dir = repo.path().join(".spur");
    let saved_spur_dir = repo.path().join(".spur.saved");
    fs::rename(&spur_dir, &saved_spur_dir).unwrap();
    let (cold_graph, cold_commits) = run_full_walk_into(repo.path(), &config, None, None).unwrap();
    fs::rename(&saved_spur_dir, &spur_dir).unwrap();

    assert_eq!(incremental_commits.commits, cold_commits.commits);
    assert_eq!(streamed.commits, cold_graph.commits);
    assert_debug_set_eq(&streamed.temporal_edges, &cold_graph.temporal_edges);
    assert_debug_set_eq(&streamed.symbol_snapshots, &cold_graph.symbol_snapshots);
}

fn assert_debug_set_eq<T: std::fmt::Debug>(left: &[T], right: &[T]) {
    let mut left_debug = left
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>();
    let mut right_debug = right
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>();
    left_debug.sort();
    right_debug.sort();
    assert_eq!(left_debug, right_debug);
}

fn test_env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

fn save_temporal_artifacts(
    worktree: &Path,
    graph: &spur_graph::schema::GraphIndexArtifact,
    commits: &spur_graph::schema::CommitIndexArtifact,
) {
    let artifact_base = worktree.join(".spur/graph");
    let staging =
        ArtifactStagingDir::new(&artifact_base, &graph.graph_content_hash).expect("stage graph");
    write_artifact_parquet(graph, staging.path(), WriteOptions::default(), Vec::new())
        .expect("save graph parquet artifact");
    let artifact_dir = staging.commit().expect("commit graph parquet artifact");
    write_current_pointer(worktree, &artifact_dir).expect("save graph CURRENT pointer");
    save_artifact(worktree, ".spur/commit-index.json", commits).expect("save commit index");
    save_pointer(
        worktree,
        &CommitIndexPointer {
            schema_version: current_schema_version(),
            artifact_relative_path: ".spur/commit-index.json".to_owned(),
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
