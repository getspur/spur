# Temporal Graph Streaming Shards — Design

**Date:** 2026-05-27
**Status:** Draft (awaiting user review)
**Crate:** `spur-graph`
**Motivating case:** `spur graph build --workspace --with-temporal` on a large long-history repo (target: `/Volumes/Projects/duckdb`, ~60K commits) consumes multi-GB of memory because all temporal data is accumulated in unbounded `Vec`s on `GraphIndexArtifact` and flushed in a single pass at the end.

---

## Problem

`run_full_walk_into` (`crates/spur-graph/src/git_walk.rs:189-298`) walks the full commit history and pushes into three unbounded vectors on the in-memory artifact:

- `graph.commits` — one `CommitArtifact` per commit (small, ~200 B/commit).
- `graph.temporal_edges` — one `TemporalEdgeArtifact` per changed file per commit, plus up to two symbol edges per changed symbol (200–500 B/row; can be 10–50M rows on duckdb-scale repos).
- `graph.symbol_snapshots` — one `SymbolSnapshotArtifact` per changed symbol; carries a `tokens: Vec<String>` payload that is the dominant per-row cost (1–4 KB each).

After the walk, `write_artifact_parquet` (`crates/spur-graph/src/store/parquet.rs:182-347`) writes the artifact to disk. Inside, `write_temporal_edges` (`parquet.rs:1277`) clones the full `temporal_edges` slice for sorting, **doubling** peak memory just before write.

There is no streaming or chunking today. Parquet's internal row-group size (`PARQUET_ROW_GROUP_SIZE = 16_384`) only applies at serialization, after the data is already fully resident.

For the duckdb repo this comfortably exceeds available RAM and degrades the whole machine.

## Goal

Bound peak memory of `spur graph build --with-temporal` to a small constant (target: < 200 MB sustained for the temporal pipeline alone) regardless of repo history depth, by streaming temporal artifacts to disk in commit-window shards as the walk progresses.

## Non-goals

- Streaming the **read** side. Current consumers hold full in-memory `Vec`s of temporal data; we bound write-time memory only. Read-side streaming is a separate future optimization.
- Switching the temporal sink from Parquet to DuckDB (Option C from brainstorm). Defer.
- Parallelizing shard writes. Keep the walk sequential; the bottleneck is memory, not throughput.

## Approach

Emit temporal artifacts as **multiple Parquet shards** inside the existing artifact directory. The walker holds a sink that lazily opens a new shard, accepts batches from the per-commit loop, and rotates shards when a threshold is hit.

### Key insight enabling the layout

`walk_commits` runs `git rev-list --topo-order --reverse --first-parent HEAD`. Each shard therefore covers a contiguous, monotonically increasing range of `commit_time`. Enumerating shards in numeric order reconstructs a globally sorted stream — **no k-way merge needed**, and per-shard row-group min/max pruning is preserved.

### Output layout

```
.spur/graph/<content_hash>.parquet/
  manifest.json                      # existing; gains temporal_shard_count + per-shard ranges
  commits.parquet                    # unchanged — single file
  symbol_snapshots/
    00000.parquet
    00001.parquet
    …
  temporal_edges/
    00000.parquet
    00001.parquet
    …
```

`commits.parquet` stays single-file (small enough). Only the two heavy tables shard.

The artifact directory is content-hashed; old artifacts naturally become stale and are GC'd / rebuilt. No on-disk migration.

### Flush policy

Flush a shard when **either** condition triggers (whichever comes first):

- `temporal_edges.len() >= 100_000` rows, **OR**
- 5_000 commits walked since the last flush.

These are defaults exposed on `GraphBuildOptions` so they can be overridden by tests or for tuning:

```rust
pub struct TemporalShardConfig {
    pub max_rows_per_shard: usize,    // default 100_000
    pub max_commits_per_shard: usize, // default 5_000
}
```

The dual threshold guards against pathological cases: a single mega-commit (very wide change) can exceed the row threshold inside one commit; a tiny-change repo would otherwise produce a 60K-commit shard.

## Architecture

### New types

`crates/spur-graph/src/store/shard_writer.rs` (new file):

```rust
pub struct TemporalShardSink {
    out_dir: PathBuf,           // .../<hash>.parquet/
    cfg: TemporalShardConfig,
    edges_writer: Option<ArrowParquetWriter>,
    snapshots_writer: Option<ArrowParquetWriter>,
    shard_idx: u32,
    commits_in_current_shard: usize,
    shard_index_entries: Vec<ShardIndexEntry>,  // for manifest
}

pub struct ShardIndexEntry {
    pub shard_idx: u32,
    pub commit_time_min: i64,
    pub commit_time_max: i64,
    pub row_count: usize,
}

impl TemporalShardSink {
    pub fn new(out_dir: PathBuf, cfg: TemporalShardConfig) -> Result<Self> { … }

    /// Drains `edges` and `snapshots` into the current shard writers,
    /// rotating shards when thresholds are exceeded.
    pub fn append_commit(
        &mut self,
        edges: &mut Vec<TemporalEdgeArtifact>,
        snapshots: &mut Vec<SymbolSnapshotArtifact>,
    ) -> Result<()> { … }

    /// Closes the current shard if any; emits per-shard summary.
    pub fn finalize(self) -> Result<Vec<ShardIndexEntry>> { … }
}
```

### Modified call sites

1. **`run_full_walk_into`** (git_walk.rs:189)
   - Accepts `Option<TemporalShardSink>`. None preserves legacy behavior for unit tests/benches that don't need streaming.
   - After each commit's pushes (the existing block at lines 250–289), if sink is Some, call `sink.append_commit(&mut graph.temporal_edges, &mut graph.symbol_snapshots)`. The sink decides whether thresholds are hit and either buffers or rotates.

2. **`write_artifact_parquet`** (parquet.rs:182)
   - Add path that, when the artifact has been streamed (sink already wrote shards), only writes `commits.parquet`, `files`, `symbols`, `edges`, etc., and skips the now-empty `temporal_edges`/`symbol_snapshots` vectors.
   - Otherwise (sink=None or empty), preserves existing behavior — writes single shard `temporal_edges/00000.parquet` and `symbol_snapshots/00000.parquet` to match the new uniform layout.
   - Update `manifest.json` to include `temporal_shards: Vec<ShardIndexEntry>`.

3. **`read_temporal_edges`** (parquet.rs:1945) and **`read_temporal_artifact_parquet`** (parquet.rs:501)
   - Replace single-file open with: read manifest, enumerate `temporal_shards`, open each `temporal_edges/{N:05}.parquet`, chain RecordBatch iterators. Same for `symbol_snapshots/*`.
   - If manifest lacks `temporal_shards`, fall back to scanning the directory for `temporal_edges/*.parquet` numerically (defensive).
   - Reader contract is unchanged: returns the same `Vec<TemporalEdgeArtifact>` / iterator type. Consumers don't know about shards.

### CLI surface

`crates/spur-cli/src/commands/graph.rs::build` — add hidden flags (not required for default behavior):

- `--temporal-max-rows-per-shard <N>` (default 100_000)
- `--temporal-max-commits-per-shard <N>` (default 5_000)

These are not advertised in `--help` of the default path; they exist for tuning and tests.

## Data flow

```
Commit walk loop (run_full_walk_into)
  ├─ for sha in commit_shas:
  │    │ push to graph.commits
  │    │ push N to graph.temporal_edges
  │    │ push M to graph.symbol_snapshots
  │    └─ sink.append_commit(&mut edges, &mut snapshots)
  │         ├─ if first call this shard: open writers
  │         ├─ write rows to current shard writers
  │         ├─ edges.clear(); snapshots.clear()
  │         └─ if threshold: close current writers, increment shard_idx
  └─ return graph (edges/snapshots are drained; commits remain)

write_artifact_parquet
  ├─ write commits.parquet (unchanged)
  ├─ write files, symbols, edges (the non-temporal tables, unchanged)
  ├─ if sink wrote shards: skip temporal tables
  └─ else: write single shard temporal_edges/00000.parquet, etc.

sink.finalize() → manifest.temporal_shards += [...]
```

## Error handling

- Shard writer open failure: propagate, abort build with a clear error pointing to the artifact dir.
- Partial shard left on the filesystem after a crash mid-build: the artifact dir is content-hashed against the *manifest*. A manifest without `temporal_shards` entries for a shard file present on disk means the build never published. On next build attempt, the existing partial dir is replaced via `write_canonical_atomically` (which already exists in cache.rs) — no special cleanup needed.
- Empty shard at end of walk (residual vectors empty after last flush): emit no extra file; `temporal_shards` index ends at the last real shard.

## Testing

### Unit tests (new)

In `crates/spur-graph/src/store/shard_writer.rs`:
- Rotation on row threshold.
- Rotation on commit threshold.
- Residual flush (small tail).
- Empty walk (no shards emitted; manifest entry is empty Vec).
- Mega-commit larger than threshold goes into single shard (does not split mid-commit — atomicity requirement: a single `append_commit` call's rows always land in the same shard, even if it overshoots the threshold).

### Integration tests (new)

In `crates/spur-graph/tests/temporal_streaming.rs`:
- Synthetic 200-commit history with varied change widths; assert peak memory bounded (check via `temporal_edges` Vec.capacity at end-of-build).
- Round-trip: build with streaming → read back via `read_temporal_artifact_parquet` → assert exactly the same logical rows as a non-streaming build over the same history (set equality on temporal edge rows).
- Sharded reader handles 1-shard, N-shard, and 0-shard cases.
- Manifest survives reload: build, drop sink, re-open via `read_artifact_parquet`, walk shards.

### Updated tests

- `tests/parquet_roundtrip.rs` — fixtures need to write `temporal_edges/00000.parquet` instead of `temporal_edges.parquet`.
- `tests/temporal_resolution.rs` — same.
- `tests/parquet_schema_invariants.rs` — verify shard manifest schema.
- `benches/parquet.rs`, `benches/incremental.rs` — update fixture writers.

### Backward compatibility

Old artifact directories with `temporal_edges.parquet` (single file, no `temporal_shards` in manifest) will simply fail to load → the artifact cache treats this as a cache miss → next build regenerates with the new layout. Since artifacts are content-hashed and rebuilt automatically, no explicit migration step is needed.

## Risk assessment (from spur-analyst grounding)

| Symbol | BR | hot callers | self churn 90d |
|---|---|---|---|
| `write_artifact_parquet` | 7.0 | 4 | 0 |
| `run_full_walk_into` | 5.9 | 6 | 0 |
| `write_temporal_edges` | 0.0 | 0 | 0 |
| `read_temporal_edges` | 0.0 | 0 | 0 |
| `read_temporal_artifact_parquet` | 0.76 | 1 | 0 |

- **Low risk** for the temporal-specific helpers — all BR=0 and self-churn=0.
- **Medium risk** for the orchestrators (`run_full_walk_into`, `write_artifact_parquet`); most callers are tests/benches that round-trip the artifact and will catch regressions immediately.
- **Cochange concentration**: top neighbor is `crates/spur-mcp/src/server/handlers/code_graph.rs` (11 cochanges over 90d, static edge). Audit that handler for direct temporal parquet file path references; route through the reader instead.

## Out-of-scope follow-ups (intentional)

- Read-side streaming (`Vec<TemporalEdge>` → iterator across queries).
- DuckDB as the temporal sink — enables SQL queries directly on history without intermediate Parquet read.
- Parallel shard writers — only after correctness lands.
- Incremental shard-append on `IncrementalPlan::FastForward` — current incremental path still rewrites; sharding makes append-only feasible, but is a separate change.

## Success criteria

1. `spur graph build --workspace --with-temporal` on `/Volumes/Projects/duckdb` completes without OOM on a 16 GB machine.
2. Peak RSS attributable to temporal accumulation < 200 MB (measured via `MallocStackLogging` or `dtruss` sample during run).
3. All existing parquet round-trip and temporal-resolution tests pass against the sharded layout.
4. New streaming-specific tests pass (rotation, residual, empty, mega-commit atomicity, round-trip parity).
5. Read path (`read_temporal_artifact_parquet`) is API-compatible — no consumer changes required in `spur-mcp` handlers.
