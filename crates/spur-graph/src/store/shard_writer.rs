use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Context as _};

use super::parquet::{write_symbol_snapshots, write_temporal_edges, WriteOptions};
use super::{ShardIndexEntry, TemporalShardConfig};
use crate::{CommitArtifact, SymbolSnapshotArtifact, TemporalEdgeArtifact};

pub struct TemporalShardSink {
    out_dir: PathBuf,
    cfg: TemporalShardConfig,
    shard_idx: u32,
    commits_in_current_shard: usize,
    rows_in_current_shard: usize,
    current_time_min: Option<i64>,
    current_time_max: Option<i64>,
    current_edges: Vec<TemporalEdgeArtifact>,
    current_snapshots: Vec<SymbolSnapshotArtifact>,
    shard_index_entries: Vec<ShardIndexEntry>,
    #[cfg(any(test, debug_assertions))]
    max_resident_rows: usize,
}

impl TemporalShardSink {
    pub fn new(out_dir: PathBuf, cfg: TemporalShardConfig) -> anyhow::Result<Self> {
        if cfg.max_rows_per_shard == 0 {
            bail!("temporal shard max_rows_per_shard must be greater than zero");
        }
        if cfg.max_commits_per_shard == 0 {
            bail!("temporal shard max_commits_per_shard must be greater than zero");
        }

        Ok(Self {
            out_dir,
            cfg,
            shard_idx: 0,
            commits_in_current_shard: 0,
            rows_in_current_shard: 0,
            current_time_min: None,
            current_time_max: None,
            current_edges: Vec::new(),
            current_snapshots: Vec::new(),
            shard_index_entries: Vec::new(),
            #[cfg(any(test, debug_assertions))]
            max_resident_rows: 0,
        })
    }

    #[cfg(any(test, debug_assertions))]
    pub fn resident_rows(&self) -> usize {
        self.current_edges.len() + self.current_snapshots.len()
    }

    #[cfg(any(test, debug_assertions))]
    pub fn max_resident_rows(&self) -> usize {
        self.max_resident_rows
    }

    pub fn append_commit(
        &mut self,
        commit: &CommitArtifact,
        edges: &mut Vec<TemporalEdgeArtifact>,
        snapshots: &mut Vec<SymbolSnapshotArtifact>,
    ) -> anyhow::Result<()> {
        let rows_for_commit = edges.len() + snapshots.len();
        if rows_for_commit == 0 {
            return Ok(());
        }

        self.current_time_min = Some(self.current_time_min.map_or(commit.author_time, |current| {
            current.min(commit.author_time)
        }));
        self.current_time_max = Some(self.current_time_max.map_or(commit.author_time, |current| {
            current.max(commit.author_time)
        }));
        self.commits_in_current_shard += 1;
        self.rows_in_current_shard += rows_for_commit;
        self.current_edges.append(edges);
        self.current_snapshots.append(snapshots);
        #[cfg(any(test, debug_assertions))]
        self.observe_resident_rows();

        if self.rows_in_current_shard >= self.cfg.max_rows_per_shard
            || self.commits_in_current_shard >= self.cfg.max_commits_per_shard
        {
            self.flush_current_shard()?;
        }

        Ok(())
    }

    pub fn finalize(mut self) -> anyhow::Result<Vec<ShardIndexEntry>> {
        self.flush_current_shard()?;
        Ok(self.shard_index_entries)
    }

    fn flush_current_shard(&mut self) -> anyhow::Result<()> {
        if self.rows_in_current_shard == 0 {
            return Ok(());
        }

        let edge_count = self.current_edges.len();
        let snapshot_count = self.current_snapshots.len();
        let commit_time_min = self
            .current_time_min
            .expect("non-empty shard must have commit_time_min");
        let commit_time_max = self
            .current_time_max
            .expect("non-empty shard must have commit_time_max");

        let shard_name = format!("{:05}.parquet", self.shard_idx);
        let options = WriteOptions::default();
        if edge_count > 0 {
            let dir = self.out_dir.join("temporal_edges");
            fs::create_dir_all(&dir)
                .with_context(|| format!("failed to create `{}`", dir.display()))?;
            write_temporal_edges(&dir.join(&shard_name), &self.current_edges, &options)?;
        }
        if snapshot_count > 0 {
            let dir = self.out_dir.join("symbol_snapshots");
            fs::create_dir_all(&dir)
                .with_context(|| format!("failed to create `{}`", dir.display()))?;
            write_symbol_snapshots(&dir.join(&shard_name), &self.current_snapshots, &options)?;
        }

        self.shard_index_entries.push(ShardIndexEntry {
            shard_idx: self.shard_idx,
            commit_time_min,
            commit_time_max,
            row_count_edges: edge_count,
            row_count_snapshots: snapshot_count,
        });

        self.shard_idx += 1;
        self.commits_in_current_shard = 0;
        self.rows_in_current_shard = 0;
        self.current_time_min = None;
        self.current_time_max = None;
        self.current_edges.clear();
        self.current_snapshots.clear();
        Ok(())
    }

    #[cfg(any(test, debug_assertions))]
    fn observe_resident_rows(&mut self) {
        self.max_resident_rows = self.max_resident_rows.max(self.resident_rows());
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        CommitArtifact, EdgeEndpoint, RelationKind, SnapshotKey, SymbolSnapshotArtifact,
        TemporalEdgeArtifact,
    };

    use super::*;

    #[test]
    fn rotates_after_row_threshold_without_splitting_commit() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let mut sink = TemporalShardSink::new(
            tempdir.path().to_path_buf(),
            TemporalShardConfig {
                max_rows_per_shard: 3,
                max_commits_per_shard: 100,
            },
        )?;

        append(&mut sink, 1, 2, 0)?;
        assert!(!tempdir.path().join("temporal_edges/00000.parquet").exists());

        append(&mut sink, 2, 1, 0)?;
        assert!(tempdir
            .path()
            .join("temporal_edges/00000.parquet")
            .is_file());

        append(&mut sink, 3, 1, 0)?;
        let entries = sink.finalize()?;

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].shard_idx, 0);
        assert_eq!(entries[0].row_count_edges, 3);
        assert_eq!(entries[1].shard_idx, 1);
        assert_eq!(entries[1].row_count_edges, 1);
        assert!(tempdir
            .path()
            .join("temporal_edges/00001.parquet")
            .is_file());
        Ok(())
    }

    #[test]
    fn rotates_after_commit_threshold() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let mut sink = TemporalShardSink::new(
            tempdir.path().to_path_buf(),
            TemporalShardConfig {
                max_rows_per_shard: 100,
                max_commits_per_shard: 2,
            },
        )?;

        append(&mut sink, 10, 1, 0)?;
        append(&mut sink, 20, 1, 0)?;
        assert!(tempdir
            .path()
            .join("temporal_edges/00000.parquet")
            .is_file());

        append(&mut sink, 30, 1, 0)?;
        let entries = sink.finalize()?;

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].row_count_edges, 2);
        assert_eq!(entries[1].row_count_edges, 1);
        Ok(())
    }

    #[test]
    fn finalize_flushes_residual_shard() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let mut sink = TemporalShardSink::new(
            tempdir.path().to_path_buf(),
            TemporalShardConfig {
                max_rows_per_shard: 10,
                max_commits_per_shard: 10,
            },
        )?;

        append(&mut sink, 42, 1, 1)?;
        assert!(!tempdir.path().join("temporal_edges/00000.parquet").exists());
        assert!(!tempdir
            .path()
            .join("symbol_snapshots/00000.parquet")
            .exists());

        let entries = sink.finalize()?;

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].row_count_edges, 1);
        assert_eq!(entries[0].row_count_snapshots, 1);
        assert!(tempdir
            .path()
            .join("temporal_edges/00000.parquet")
            .is_file());
        assert!(tempdir
            .path()
            .join("symbol_snapshots/00000.parquet")
            .is_file());
        Ok(())
    }

    #[test]
    fn empty_walk_writes_no_files() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let sink = TemporalShardSink::new(
            tempdir.path().to_path_buf(),
            TemporalShardConfig {
                max_rows_per_shard: 1,
                max_commits_per_shard: 1,
            },
        )?;

        let entries = sink.finalize()?;

        assert!(entries.is_empty());
        assert!(!tempdir.path().join("temporal_edges").exists());
        assert!(!tempdir.path().join("symbol_snapshots").exists());
        Ok(())
    }

    #[test]
    fn mega_commit_larger_than_threshold_stays_in_single_shard() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let mut sink = TemporalShardSink::new(
            tempdir.path().to_path_buf(),
            TemporalShardConfig {
                max_rows_per_shard: 2,
                max_commits_per_shard: 100,
            },
        )?;

        append(&mut sink, 5, 5, 0)?;
        append(&mut sink, 6, 1, 0)?;
        let entries = sink.finalize()?;

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].row_count_edges, 5);
        assert_eq!(entries[1].row_count_edges, 1);
        Ok(())
    }

    #[test]
    fn commit_time_min_max_span_all_commits_in_shard() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let mut sink = TemporalShardSink::new(
            tempdir.path().to_path_buf(),
            TemporalShardConfig {
                max_rows_per_shard: 100,
                max_commits_per_shard: 10,
            },
        )?;

        append(&mut sink, 30, 1, 0)?;
        append(&mut sink, 10, 1, 0)?;
        append(&mut sink, 20, 1, 0)?;
        let entries = sink.finalize()?;

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].commit_time_min, 10);
        assert_eq!(entries[0].commit_time_max, 30);
        Ok(())
    }

    #[test]
    fn out_of_order_commit_times_do_not_start_new_shards() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let mut sink = TemporalShardSink::new(
            tempdir.path().to_path_buf(),
            TemporalShardConfig {
                max_rows_per_shard: 10,
                max_commits_per_shard: 10,
            },
        )?;

        append(&mut sink, 100, 1, 0)?;
        append(&mut sink, 50, 1, 0)?;
        let entries = sink.finalize()?;

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].commit_time_min, 50);
        assert_eq!(entries[0].commit_time_max, 100);
        Ok(())
    }

    fn append(
        sink: &mut TemporalShardSink,
        author_time: i64,
        edge_count: usize,
        snapshot_count: usize,
    ) -> anyhow::Result<()> {
        let commit = CommitArtifact {
            sha: format!("commit-{author_time}"),
            parents: Vec::new(),
            author_time,
            summary: "test commit".to_owned(),
        };
        let mut edges = (0..edge_count)
            .map(|idx| test_edge(&commit.sha, idx))
            .collect::<Vec<_>>();
        let mut snapshots = (0..snapshot_count)
            .map(|idx| test_snapshot(&commit.sha, idx))
            .collect::<Vec<_>>();

        sink.append_commit(&commit, &mut edges, &mut snapshots)?;

        assert!(edges.is_empty());
        assert!(snapshots.is_empty());
        Ok(())
    }

    fn test_edge(commit: &str, idx: usize) -> TemporalEdgeArtifact {
        TemporalEdgeArtifact {
            source: EdgeEndpoint::Commit {
                sha: commit.to_owned(),
            },
            target: EdgeEndpoint::Snapshot {
                key: SnapshotKey {
                    stable_symbol_id: format!("sym-{idx}"),
                    commit: commit.to_owned(),
                },
            },
            relation: RelationKind::Touches,
            parent: None,
            change_kind: None,
        }
    }

    fn test_snapshot(commit: &str, idx: usize) -> SymbolSnapshotArtifact {
        SymbolSnapshotArtifact {
            key: SnapshotKey {
                stable_symbol_id: format!("sym-{idx}"),
                commit: commit.to_owned(),
            },
            file_path: format!("src/{idx}.rs").into(),
            entity_name: format!("symbol_{idx}"),
            symbol_kind: "function".to_owned(),
            enclosing_scope: None,
            byte_range: [0, 10],
            line_range: [1, 1],
            anchor_hash: format!("anchor-{idx}"),
            tokens: vec!["fn".to_owned(), format!("symbol_{idx}")],
        }
    }
}
