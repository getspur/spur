#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

mod support;

use spur_graph::git_walk::{run_full_walk_into, GitWalkConfig};
use spur_graph::schema::GRAPH_INDEX_VERSION_TEMPORAL;
use spur_graph::store::commit_index::{save_artifact, save_pointer, CommitIndexPointer};
use spur_graph::store::{
    read_artifact_header_parquet, write_artifact_parquet, write_current_pointer, TemporalShardSink,
    WriteOptions,
};
use spur_graph::{
    load_temporal_artifact_parquet, CommitIndexArtifact, GraphArtifactManifest, GraphIndexArtifact,
    TemporalShardConfig,
};
use support::git_repo::GitRepo;
use tempfile::TempDir;

#[test]
fn sink_on_200_commit_history_drains_rows_and_rotates_shards() {
    let repo = GitRepo::new();
    append_synthetic_history(&repo, 1..=200);

    let (baseline, _) = run_walk(repo.path(), None);
    let expected_edges = baseline.temporal_edges.len();
    let expected_snapshots = baseline.symbol_snapshots.len();
    assert!(expected_edges > 200);
    assert!(expected_snapshots > 200);

    let built = build_sharded(
        repo.path(),
        TemporalShardConfig {
            max_rows_per_shard: 50,
            max_commits_per_shard: 20,
        },
    );

    assert!(built.graph.temporal_edges.is_empty());
    assert!(built.graph.symbol_snapshots.is_empty());
    assert_eq!(total_edge_rows(&built.manifest), expected_edges);
    assert_eq!(total_snapshot_rows(&built.manifest), expected_snapshots);
    assert!(built.manifest.temporal_shards.len() > 1);
}

#[test]
fn sink_on_and_sink_off_round_trip_temporal_rows_match() {
    let repo = GitRepo::new();
    append_synthetic_history(&repo, 1..=40);

    let sink_on = build_sharded(
        repo.path(),
        TemporalShardConfig {
            max_rows_per_shard: 12,
            max_commits_per_shard: 10,
        },
    );
    let sink_off = build_single_shard(repo.path());

    assert_debug_set_eq(
        &sink_on.temporal.temporal_edges,
        &sink_off.temporal.temporal_edges,
    );
    assert_debug_set_eq(
        &sink_on.temporal.symbol_snapshots,
        &sink_off.temporal.symbol_snapshots,
    );
}

#[test]
fn temporal_reader_handles_single_shard_and_many_shards() {
    let repo = GitRepo::new();
    append_synthetic_history(&repo, 1..=24);

    let single = build_single_shard(repo.path());
    assert_eq!(single.manifest.temporal_shards.len(), 1);
    assert_eq!(
        single.temporal.temporal_edges.len(),
        single.graph.temporal_edges.len()
    );
    assert_eq!(
        single.temporal.symbol_snapshots.len(),
        single.graph.symbol_snapshots.len()
    );

    let many = build_sharded(
        repo.path(),
        TemporalShardConfig {
            max_rows_per_shard: 8,
            max_commits_per_shard: 4,
        },
    );
    assert!(many.manifest.temporal_shards.len() > 1);
    assert_eq!(
        many.temporal.temporal_edges.len(),
        total_edge_rows(&many.manifest)
    );
    assert_eq!(
        many.temporal.symbol_snapshots.len(),
        total_snapshot_rows(&many.manifest)
    );
}

#[test]
fn incremental_fast_forward_seed_watermark_stays_bounded() {
    let repo = GitRepo::new();
    append_synthetic_history(&repo, 1..=100);
    let first = save_sharded_for_incremental(
        repo.path(),
        TemporalShardConfig {
            max_rows_per_shard: 64,
            max_commits_per_shard: 16,
        },
    );
    let prior_rows = total_edge_rows(&first.manifest) + total_snapshot_rows(&first.manifest);
    assert!(prior_rows > 300);

    append_synthetic_history(&repo, 101..=200);
    let incremental = build_sharded(
        repo.path(),
        TemporalShardConfig {
            max_rows_per_shard: 64,
            max_commits_per_shard: 16,
        },
    );
    assert!(
        incremental.max_resident_rows < prior_rows,
        "incremental seed should not materialize all {prior_rows} prior rows; watermark was {}",
        incremental.max_resident_rows
    );

    let spur_dir = repo.path().join(".spur");
    let saved_spur_dir = repo.path().join(".spur.saved");
    fs::rename(&spur_dir, &saved_spur_dir).expect("hide incremental cache");
    let cold = build_single_shard(repo.path());
    fs::rename(&saved_spur_dir, &spur_dir).expect("restore incremental cache");

    assert_debug_set_eq(
        &incremental.temporal.temporal_edges,
        &cold.temporal.temporal_edges,
    );
    assert_debug_set_eq(
        &incremental.temporal.symbol_snapshots,
        &cold.temporal.symbol_snapshots,
    );
}

#[test]
fn merge_commit_secondary_parent_older_time_is_reflected_in_shard_min() {
    let repo = GitRepo::new();
    repo.write("src/base.rs", "pub fn base() -> u32 { 0 }\n");
    repo.commit_all_at("base", 250);
    repo.git(&["branch", "side"]);

    repo.write("src/main.rs", "pub fn primary() -> u32 { 1 }\n");
    let primary = repo.commit_all_at("primary parent", 300);

    repo.git(&["checkout", "-q", "side"]);
    repo.write("src/side.rs", "pub fn secondary() -> u32 { 2 }\n");
    let secondary = repo.commit_all_at("secondary parent", 100);

    repo.git(&["checkout", "-q", "main"]);
    repo.git_with_env(
        &["merge", "--no-ff", "-q", "side", "-m", "merge"],
        &[
            ("GIT_AUTHOR_DATE", "@400 +0000".to_owned()),
            ("GIT_COMMITTER_DATE", "@400 +0000".to_owned()),
        ],
    );

    let built = build_sharded(
        repo.path(),
        TemporalShardConfig {
            max_rows_per_shard: usize::MAX,
            max_commits_per_shard: usize::MAX,
        },
    );
    let merge_commit = built
        .graph
        .commits
        .iter()
        .find(|commit| commit.parents.len() == 2)
        .expect("merge commit");
    assert_eq!(merge_commit.parents, vec![primary, secondary]);
    assert_eq!(built.manifest.temporal_shards.len(), 1);
    assert_eq!(built.manifest.temporal_shards[0].commit_time_min, 100);
}

struct BuiltTemporal {
    _tempdir: TempDir,
    dir: PathBuf,
    graph: GraphIndexArtifact,
    commit_index: CommitIndexArtifact,
    temporal: GraphIndexArtifact,
    manifest: GraphArtifactManifest,
    max_resident_rows: usize,
}

fn build_sharded(worktree: &Path, cfg: TemporalShardConfig) -> BuiltTemporal {
    let tempdir = tempfile::tempdir().expect("artifact tempdir");
    let mut sink = TemporalShardSink::new(tempdir.path().to_path_buf(), cfg).expect("create sink");
    let (graph, commit_index) = run_walk(worktree, Some(&mut sink));
    let max_resident_rows = sink.max_resident_rows();
    let shards = sink.finalize().expect("finalize sink");
    write_artifact_parquet(&graph, tempdir.path(), WriteOptions::default(), shards)
        .expect("write sharded artifact");
    read_built_artifact(tempdir, graph, commit_index, max_resident_rows)
}

fn build_single_shard(worktree: &Path) -> BuiltTemporal {
    let tempdir = tempfile::tempdir().expect("artifact tempdir");
    let (graph, commit_index) = run_walk(worktree, None);
    write_artifact_parquet(&graph, tempdir.path(), WriteOptions::default(), Vec::new())
        .expect("write single-shard fallback artifact");
    read_built_artifact(tempdir, graph, commit_index, 0)
}

fn read_built_artifact(
    tempdir: TempDir,
    graph: GraphIndexArtifact,
    commit_index: CommitIndexArtifact,
    max_resident_rows: usize,
) -> BuiltTemporal {
    let dir = tempdir.path().to_path_buf();
    let manifest = read_artifact_header_parquet(&dir).expect("read manifest");
    let temporal = load_temporal_artifact_parquet(&dir).expect("read temporal artifact");
    BuiltTemporal {
        _tempdir: tempdir,
        dir,
        graph,
        commit_index,
        temporal,
        manifest,
        max_resident_rows,
    }
}

fn save_sharded_for_incremental(worktree: &Path, cfg: TemporalShardConfig) -> BuiltTemporal {
    let built = build_sharded(worktree, cfg);
    let artifact_dir = worktree
        .join(".spur/graph")
        .join(&built.graph.graph_content_hash);
    copy_dir_all(&built.dir, &artifact_dir).expect("copy graph artifact into repo cache");
    write_current_pointer(worktree, &artifact_dir).expect("write CURRENT pointer");
    save_commit_index(worktree, &built.commit_index);
    built
}

fn copy_dir_all(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dest = to.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn save_commit_index(worktree: &Path, commits: &CommitIndexArtifact) {
    save_artifact(worktree, ".spur/commit-index.json", commits).expect("save commit index");
    save_pointer(
        worktree,
        &CommitIndexPointer {
            schema_version: GRAPH_INDEX_VERSION_TEMPORAL
                .parse()
                .expect("temporal graph index version is numeric"),
            artifact_relative_path: ".spur/commit-index.json".to_owned(),
            indexed_at: commits.indexed_at.clone(),
            refs: commits.refs.clone(),
        },
    )
    .expect("save commit index pointer");
}

fn run_walk(
    worktree: &Path,
    sink: Option<&mut TemporalShardSink>,
) -> (GraphIndexArtifact, CommitIndexArtifact) {
    let mut config = GitWalkConfig::default();
    config.use_gix_diff = false;
    run_full_walk_into(worktree, &config, None, sink).expect("run temporal git walk")
}

fn append_synthetic_history(repo: &GitRepo, range: std::ops::RangeInclusive<u32>) {
    for idx in range {
        let path = format!("src/module_{}.rs", idx % 7);
        repo.write(&path, &synthetic_rust_source(idx));
        repo.commit_all_at(&format!("synthetic {idx}"), 1_700_000_000 + i64::from(idx));
    }
}

fn synthetic_rust_source(idx: u32) -> String {
    let symbols = (idx % 5) + 1;
    let mut source = String::new();
    for symbol in 0..symbols {
        source.push_str(&format!(
            "pub fn value_{symbol}() -> u32 {{ {} }}\n",
            idx + symbol
        ));
    }
    source.push_str(&format!(
        "pub fn aggregate() -> u32 {{ value_0() + {} }}\n",
        idx % 3
    ));
    source
}

fn total_edge_rows(manifest: &GraphArtifactManifest) -> usize {
    manifest
        .temporal_shards
        .iter()
        .map(|entry| entry.row_count_edges)
        .sum()
}

fn total_snapshot_rows(manifest: &GraphArtifactManifest) -> usize {
    manifest
        .temporal_shards
        .iter()
        .map(|entry| entry.row_count_snapshots)
        .sum()
}

fn assert_debug_set_eq<T: std::fmt::Debug>(left: &[T], right: &[T]) {
    let left = left
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<BTreeSet<_>>();
    let right = right
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<BTreeSet<_>>();
    assert_eq!(left, right);
}
