pub const DEFAULT_TEMPORAL_SHARD_MAX_ROWS: usize = 100_000;
pub const DEFAULT_TEMPORAL_SHARD_MAX_COMMITS: usize = 5_000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ShardIndexEntry {
    pub shard_idx: u32,
    pub commit_time_min: i64,
    pub commit_time_max: i64,
    pub row_count_edges: usize,
    pub row_count_snapshots: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TemporalShardConfig {
    pub max_rows_per_shard: usize,
    pub max_commits_per_shard: usize,
}

impl Default for TemporalShardConfig {
    fn default() -> Self {
        Self {
            max_rows_per_shard: DEFAULT_TEMPORAL_SHARD_MAX_ROWS,
            max_commits_per_shard: DEFAULT_TEMPORAL_SHARD_MAX_COMMITS,
        }
    }
}
