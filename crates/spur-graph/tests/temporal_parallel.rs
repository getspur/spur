use std::collections::BTreeMap;
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::Command;

use spur_graph::git_walk::{run_full_walk_into, GitWalkConfig};
use spur_graph::{
    load_artifact, write_artifact_parquet, CommitIndexArtifact, GraphArtifactManifest,
    GraphIndexArtifact, ShardIndexEntry, TemporalShardConfig, TemporalShardSink, WriteOptions,
};
use tempfile::TempDir;

const NORMALIZED_INDEXED_AT: &str = "<normalized-indexed-at>";

#[derive(Debug)]
struct WalkOutput {
    graph: GraphIndexArtifact,
    commits: CommitIndexArtifact,
    reloaded: GraphIndexArtifact,
    shard_index: Vec<ShardIndexEntry>,
    manifest: GraphArtifactManifest,
    artifact_files: BTreeMap<PathBuf, Vec<u8>>,
    normalized_semantic_bytes: Vec<u8>,
}

#[test]
fn serial_parallel_temporal_artifacts_and_shards_are_deterministic() -> anyhow::Result<()> {
    let source = TempDir::new()?;
    build_fixture(source.path());

    let serial = construct_from_fresh_clone(source.path(), 1)?;
    let parallel_runs = [
        construct_from_fresh_clone(source.path(), 8)?,
        construct_from_fresh_clone(source.path(), 8)?,
        construct_from_fresh_clone(source.path(), 8)?,
    ];

    for (run, parallel) in parallel_runs.iter().enumerate() {
        assert_walk_output_eq(
            &format!("jobs=1 vs jobs=8 run {}", run + 1),
            &serial,
            parallel,
        );
    }
    for run in 1..parallel_runs.len() {
        assert_walk_output_eq(
            &format!("jobs=8 run 1 vs run {}", run + 1),
            &parallel_runs[0],
            &parallel_runs[run],
        );
    }

    Ok(())
}

fn construct_from_fresh_clone(source: &Path, jobs: usize) -> anyhow::Result<WalkOutput> {
    let run_dir = TempDir::new()?;
    let worktree = run_dir.path().join("repo");
    clone_repo(source, &worktree);

    let artifact_dir = run_dir.path().join("artifact");
    let mut sink = TemporalShardSink::new(
        artifact_dir.clone(),
        TemporalShardConfig {
            max_rows_per_shard: 8,
            max_commits_per_shard: 2,
        },
    )?;
    let config = GitWalkConfig {
        temporal_jobs: NonZeroUsize::new(jobs).expect("jobs must be non-zero"),
        ..GitWalkConfig::default()
    };
    let (graph, commits) = run_full_walk_into(&worktree, &config, None, Some(&mut sink))?;
    assert!(graph.temporal_edges.is_empty());
    assert!(graph.symbol_snapshots.is_empty());
    let shard_index = sink.finalize()?;
    assert!(
        shard_index.len() > 1,
        "fixture must exercise shard rotation"
    );

    let written_dir = write_artifact_parquet(
        &graph,
        &artifact_dir,
        WriteOptions::default(),
        shard_index.clone(),
    )?;
    let reloaded = load_artifact(&written_dir)?;
    assert!(!reloaded.temporal_edges.is_empty());
    assert!(!reloaded.symbol_snapshots.is_empty());
    let manifest_bytes = fs::read(written_dir.join("manifest.json"))?;
    let manifest: GraphArtifactManifest = serde_json::from_slice(&manifest_bytes)?;
    assert_eq!(manifest.temporal_shards, shard_index);

    let mut normalized_commits = commits.clone();
    assert!(
        !normalized_commits.indexed_at.is_empty(),
        "walk must record its construction timestamp"
    );
    // run_full_walk_into assigns this field from Utc::now. It is the only
    // nondeterministic timestamp normalized by this test; commit author times,
    // shard time bounds, and every ordered payload remain untouched.
    normalized_commits.indexed_at = NORMALIZED_INDEXED_AT.to_owned();
    let normalized_semantic_bytes =
        serde_json::to_vec(&(&graph, &normalized_commits, &reloaded, &manifest))?;

    Ok(WalkOutput {
        graph,
        commits,
        reloaded,
        shard_index,
        manifest,
        artifact_files: collect_artifact_files(&written_dir)?,
        normalized_semantic_bytes,
    })
}

fn assert_walk_output_eq(label: &str, expected: &WalkOutput, actual: &WalkOutput) {
    assert_graph_artifact_eq(label, &expected.graph, &actual.graph);
    assert_commit_index_eq(label, &expected.commits, &actual.commits);
    assert_graph_artifact_eq(label, &expected.reloaded, &actual.reloaded);
    assert_eq!(
        expected.shard_index, actual.shard_index,
        "{label}: shard index"
    );
    assert_eq!(expected.manifest, actual.manifest, "{label}: manifest");
    assert_eq!(
        expected.normalized_semantic_bytes, actual.normalized_semantic_bytes,
        "{label}: normalized semantic bytes"
    );

    assert_eq!(
        expected.artifact_files.keys().collect::<Vec<_>>(),
        actual.artifact_files.keys().collect::<Vec<_>>(),
        "{label}: artifact file set"
    );
    for (path, expected_bytes) in &expected.artifact_files {
        assert_eq!(
            expected_bytes,
            actual
                .artifact_files
                .get(path)
                .expect("file set checked above"),
            "{label}: artifact bytes for {}",
            path.display()
        );
    }
}

fn assert_graph_artifact_eq(
    label: &str,
    expected: &GraphIndexArtifact,
    actual: &GraphIndexArtifact,
) {
    macro_rules! assert_ordered_field {
        ($field:ident) => {
            assert_eq!(
                expected.$field,
                actual.$field,
                "{label}: graph field {}",
                stringify!($field)
            );
        };
    }

    assert_ordered_field!(header);
    assert_ordered_field!(manifest_version);
    assert_ordered_field!(graph_content_hash);
    assert_ordered_field!(file_manifests);
    assert_ordered_field!(files);
    assert_ordered_field!(file_node_ids);
    assert_ordered_field!(symbols);
    assert_ordered_field!(symbol_node_ids);
    assert_ordered_field!(edges);
    assert_ordered_field!(tombstones);
    assert_ordered_field!(diagnostics);
    assert_ordered_field!(commits);
    assert_ordered_field!(symbol_snapshots);
    assert_ordered_field!(temporal_edges);
}

fn assert_commit_index_eq(
    label: &str,
    expected: &CommitIndexArtifact,
    actual: &CommitIndexArtifact,
) {
    assert_eq!(
        expected.schema_version, actual.schema_version,
        "{label}: schema"
    );
    assert_eq!(expected.commits, actual.commits, "{label}: commits");
    assert_eq!(expected.refs, actual.refs, "{label}: refs");
    assert_eq!(
        expected.walk_strategy, actual.walk_strategy,
        "{label}: walk strategy"
    );
    assert!(!expected.indexed_at.is_empty());
    assert!(!actual.indexed_at.is_empty());
}

fn collect_artifact_files(root: &Path) -> anyhow::Result<BTreeMap<PathBuf, Vec<u8>>> {
    fn visit(
        root: &Path,
        dir: &Path,
        files: &mut BTreeMap<PathBuf, Vec<u8>>,
    ) -> anyhow::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                visit(root, &path, files)?;
            } else {
                files.insert(path.strip_prefix(root)?.to_path_buf(), fs::read(path)?);
            }
        }
        Ok(())
    }

    // BTreeMap canonicalizes only filesystem path lookup. The Parquet bytes are
    // compared verbatim, so temporal row and shard order cannot be sorted away.
    let mut files = BTreeMap::new();
    visit(root, root, &mut files)?;
    Ok(files)
}

fn build_fixture(repo: &Path) {
    git(repo, &["init", "-q", "-b", "main"]);
    git(
        repo,
        &["config", "user.email", "temporal-parity@example.invalid"],
    );
    git(repo, &["config", "user.name", "Temporal Parity"]);

    fs::write(repo.join("lib.rs"), b"pub fn root() -> u8 { 1 }\n").unwrap();
    commit(repo, "root", 0);

    git(repo, &["checkout", "-q", "-b", "side"]);
    fs::write(repo.join("side.py"), b"def side():\n    return 2\n").unwrap();
    commit(repo, "side", 1);

    git(repo, &["checkout", "-q", "main"]);
    fs::write(repo.join("lib.rs"), b"pub fn root() -> u8 { 3 }\n").unwrap();
    commit(repo, "main", 2);
    git(
        repo,
        &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
    );

    fs::rename(repo.join("lib.rs"), repo.join("core.rs")).unwrap();
    commit(repo, "rename", 4);
    for generation in 5..=12 {
        fs::write(
            repo.join("core.rs"),
            format!(
                "pub fn root() -> u8 {{ {generation} }}\npub fn generation_{generation}() -> u8 {{ {generation} }}\n"
            ),
        )
        .unwrap();
        commit(repo, &format!("generation {generation}"), generation);
    }
}

fn commit(repo: &Path, message: &str, ordinal: usize) {
    git(repo, &["add", "-A"]);
    let timestamp = format!("@{} +0000", 1_700_000_000 + ordinal as i64);
    let output = Command::new("git")
        .current_dir(repo)
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
}

fn clone_repo(source: &Path, destination: &Path) {
    let output = Command::new("git")
        .args(["clone", "--quiet", "--local", "--no-hardlinks"])
        .arg(source)
        .arg(destination)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git clone failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
