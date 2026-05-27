# Temporal Graph Streaming Shards — Design

**Date:** 2026-05-27 (amended after code-* second-pass review by `codex` + `gemini`)
**Status:** Draft (awaiting user review)
**Crate:** `spur-graph` (+ small ripple into `spur-cli`, `spur-context`)
**Motivating case:** `spur graph build --workspace --with-temporal` on a large long-history repo (target: `/Volumes/Projects/duckdb`, ~60K commits) consumes multi-GB of memory because all temporal data is accumulated in unbounded `Vec`s on `GraphIndexArtifact` and flushed in a single pass at the end.

---

## Problem

`run_full_walk_into` (`crates/spur-graph/src/git_walk.rs:189-298`) walks the full commit history and pushes into three unbounded vectors on the in-memory artifact:

- `graph.commits` — one `CommitArtifact` per commit (small, ~200 B/commit).
- `graph.temporal_edges` — one `TemporalEdgeArtifact` per changed file per commit, plus up to two symbol edges per changed symbol (200–500 B/row; can be 10–50M rows on duckdb-scale repos).
- `graph.symbol_snapshots` — one `SymbolSnapshotArtifact` per changed symbol; carries a `tokens: Vec<String>` payload that is the dominant per-row cost (1–4 KB each).

After the walk, `write_artifact_parquet` (`crates/spur-graph/src/store/parquet.rs:182-347`) writes the artifact to disk. Inside, `write_temporal_edges` (`parquet.rs:1277-1317`) materializes a full `Vec<TemporalEdgeRow>` derived from the source rows for sorting before write — effectively a second full pass over the edge table.

There is no streaming or chunking today. Parquet's internal row-group size (`PARQUET_ROW_GROUP_SIZE = 16_384`) only applies at serialization, after the data is already fully resident.

The incremental path is no rescue: when `--with-temporal` runs against a fast-forward base, `run_full_walk_into` calls `load_temporal_artifact_parquet` (`git_walk.rs:349`) and loads the **entire** prior temporal parquet into RAM before appending new rows.

For the duckdb repo this comfortably exceeds available RAM and degrades the whole machine.

## Goal

Bound peak memory of `spur graph build --with-temporal` to a small constant (target: < 200 MB sustained for the temporal pipeline alone) regardless of repo history depth, by:

1. Streaming temporal artifacts to disk in commit-window shards as the walk progresses (full and incremental paths).
2. Sharded read on the fast-forward seed path so incremental builds don't materialize the prior artifact in full.

## Non-goals

- **Read-side `TemporalIndex` streaming.** `crates/spur-graph/src/temporal.rs:94-160` builds hashmaps over fully loaded edge/snapshot vectors. Bounding query-time RAM is a separate ticket. This spec covers `spur graph build`, not analyst/MCP query memory. Tracked as a follow-up.
- **Switching the temporal sink to DuckDB.** Defer.
- **Parallelizing shard writes.** Sequential walk; the bottleneck is memory, not throughput.
- **Removing the in-memory `graph.temporal_edges` / `graph.symbol_snapshots` fields.** They remain on `GraphIndexArtifact` as the per-commit landing buffer that the sink drains. Tests/fixtures that construct artifacts in memory without a sink keep working unchanged.

## Approach

Emit temporal artifacts as **multiple Parquet shards** inside the existing artifact directory. The CLI build path hoists the artifact's atomic-staging directory out of `write_artifact_parquet` and threads it into `run_full_walk_into` alongside a `TemporalShardSink`. The sink lazily opens a new shard, drains the per-commit `Vec`s after each commit, and rotates shards when a threshold is hit. `write_artifact_parquet` finalizes the rest of the artifact (commits, files, symbols, structural edges) into the same staging dir and atomic-renames as today.

### Shard ordering — what's actually true

`walk_commits` runs `git rev-list --topo-order --reverse [--first-parent] HEAD` (`git_walk.rs:399-412`). This gives **walk-order** monotonicity, not `commit_time` monotonicity:

- Topological order is partial; reverse-topo doesn't sort by author/committer time.
- Merge commits emit edges against **every** parent (`git_walk.rs:584-607`, asserted by `tests/merge_commit_diff.rs:37-43`), and a secondary parent's `commit_time` may precede the current commit's.
- The current temporal_edges writer sorts by `target_stable_symbol_id` then `source_commit` (`parquet.rs:1313-1317`) — *not* by time anyway.

Therefore each shard records explicit `commit_time_min` and `commit_time_max` in its `ShardIndexEntry` for predicate-prune purposes. Consumers that need time-ordered traversal merge/sort at read; we make no global time-sort guarantee at the shard layer. Parquet row-group min/max statistics inside each shard still allow per-shard pruning.

### Output layout

```
.spur/graph/<content_hash>.parquet/
  manifest.json                      # gains: temporal_shards: Vec<ShardIndexEntry>
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

`commits.parquet` stays single-file (small, one row per commit). Only the two heavy tables shard.

### Flush policy

Flush a shard when **either** condition triggers (whichever comes first):

- `temporal_edges.len() >= 100_000` rows, **OR**
- 5_000 commits walked since the last shard rotation.

A single `append_commit` call's rows always land in the same shard, even if it overshoots the row threshold — preserves per-commit atomicity (downstream queries that range-scan by `commit_sha` see all rows for a commit in one file).

Exposed on `GraphBuildOptions`:

```rust
pub struct TemporalShardConfig {
    pub max_rows_per_shard: usize,    // default 100_000
    pub max_commits_per_shard: usize, // default 5_000
}
```

### Cache invalidation — bump SCHEMA_VERSION

`graph_content_hash` (`build.rs:979-984`) is derived from file `content_oid`s only — it does **not** depend on parquet layout. Old single-file artifacts won't automatically rebuild. We bump `SCHEMA_VERSION` at `crates/spur-graph/src/store/build.rs:23` (currently `"spur-graph-schema-v6"` → `"spur-graph-schema-v7"`), which flows through `manifest_version` (dynamically hashed over schema + extractor + tree-sitter query bytes at `build.rs:130-149`) and into the canonical cache directory name (`cache.rs:151-159`). Stale artifacts are then cache-missed at load and rebuilt with the sharded layout.

### Manifest schema — additive, defaulted

`GraphArtifactManifest` (`parquet.rs:49-62`) and `GraphArtifactRowCounts` (`parquet.rs:63-74`) currently use `#[serde(deny_unknown_fields)]`. We **keep** `deny_unknown_fields` (defense-in-depth) and add the new fields with `#[serde(default)]`:

```rust
pub struct GraphArtifactManifest {
    // … existing …
    #[serde(default)]
    pub temporal_shards: Vec<ShardIndexEntry>,
}

pub struct GraphArtifactRowCounts {
    // … existing …
    // temporal_edges / symbol_snapshots row counts become per-shard sums
    // computed at finalize time from the sink — schema unchanged otherwise.
}
```

Combined with the `SCHEMA_VERSION` bump, old artifacts are cache-missed before they can hit the deserializer, so the `#[serde(default)]` is belt-and-suspenders for hand-built fixtures.

## Architecture

### New types — `crates/spur-graph/src/store/shard_writer.rs` (new file)

```rust
pub struct TemporalShardSink {
    out_dir: PathBuf,           // caller-owned staging dir (see "Temp-dir hoisting")
    cfg: TemporalShardConfig,
    edges_writer: Option<ArrowParquetWriter<TemporalEdgeRow>>,
    snapshots_writer: Option<ArrowParquetWriter<SymbolSnapshotRow>>,
    shard_idx: u32,
    commits_in_current_shard: usize,
    rows_in_current_shard: usize,
    current_time_min: i64,
    current_time_max: i64,
    shard_index_entries: Vec<ShardIndexEntry>,
}

pub struct ShardIndexEntry {
    pub shard_idx: u32,
    pub commit_time_min: i64,
    pub commit_time_max: i64,
    pub row_count_edges: usize,
    pub row_count_snapshots: usize,
}

impl TemporalShardSink {
    pub fn new(out_dir: PathBuf, cfg: TemporalShardConfig) -> Result<Self>;

    /// Drains `edges` and `snapshots` into the current shard writers,
    /// rotating shards when thresholds are exceeded. `commit` provides
    /// `author_time` for the shard's `commit_time_{min,max}` bounds.
    /// All rows from a single call land in the same shard (per-commit
    /// atomicity), even if the row threshold is overshot.
    pub fn append_commit(
        &mut self,
        commit: &CommitArtifact,
        edges: &mut Vec<TemporalEdgeArtifact>,
        snapshots: &mut Vec<SymbolSnapshotArtifact>,
    ) -> Result<()>;

    /// Closes the current shard if any; returns the populated shard index.
    pub fn finalize(self) -> Result<Vec<ShardIndexEntry>>;
}
```

### Temp-dir hoisting

Today `write_artifact_parquet` (`parquet.rs:182-194`) creates its own temp directory then atomic-renames to the canonical hash-dir at the end. The shard sink needs to write into that same staging dir so partial shards never leak into the published artifact.

Hoist the staging-dir lifecycle into a new helper, `crates/spur-graph/src/store/artifact_staging.rs`:

```rust
pub struct ArtifactStagingDir {
    staging_path: PathBuf,
    final_path: PathBuf,
    // RAII drop = best-effort remove staging dir if not committed
}

impl ArtifactStagingDir {
    pub fn new(canonical_root: &Path, content_hash: &str) -> Result<Self>;
    pub fn path(&self) -> &Path;
    pub fn commit(self) -> Result<PathBuf>;  // atomic rename
}
```

Callers (CLI `build`, incremental rebuild, `write_canonical_atomically`) construct the staging dir, pass `staging.path()` into both `run_full_walk_into` (for the sink) and `write_artifact_parquet` (for the rest of the artifact), then call `staging.commit()`. Cancellation/drop cleans up the partial staging dir.

`write_artifact_parquet` loses its internal temp_dir creation — it becomes "write all the tables into this directory."

### Modified call sites

1. **`run_full_walk_into`** (`git_walk.rs:189`)
   - New parameter: `sink: Option<&mut TemporalShardSink>`. `None` preserves current behavior — pushes into the in-memory Vecs and lets `write_artifact_parquet` write a single shard at the end. `Some` drains the Vecs each commit.
   - After the per-commit pushes (block at `git_walk.rs:250-289`), if sink is `Some`, call `sink.append_commit(&commit, &mut graph.temporal_edges, &mut graph.symbol_snapshots)`.
   - The incremental fast-forward branch (`git_walk.rs:306-375`) gets the same sink-aware treatment, but its **seed-load** step (currently `load_temporal_artifact_parquet`) is replaced with a **streaming shard reader** that emits prior rows directly into the sink without buffering — see next change.

2. **`load_temporal_artifact_parquet`** (`parquet.rs:?` — invoked from `git_walk.rs:349`)
   - Add a streaming variant `stream_temporal_artifact_parquet(dir, callback)` that walks the sharded layout shard-by-shard, yielding RecordBatches to a callback. Used by the fast-forward seed path to feed the sink without ever holding the prior artifact in memory.
   - Keep the existing collecting variant for tests/in-memory consumers.

3. **`write_artifact_parquet`** (`parquet.rs:182`)
   - Signature: take `out_dir: &Path` (caller-owned, no internal temp_dir).
   - When `graph.temporal_edges` and `graph.symbol_snapshots` are empty (sink already drained them), skip those tables — the sink already wrote them.
   - When non-empty (no sink path, e.g. test fixtures that build a `GraphIndexArtifact` in memory and call `write_artifact_parquet` directly), emit `temporal_edges/00000.parquet` and `symbol_snapshots/00000.parquet` as single-shard fallback, with a single-entry `temporal_shards` in the manifest. This keeps fixture tests unchanged.
   - Update `manifest.json` to include `temporal_shards`.

4. **`read_temporal_edges`** (`parquet.rs:1945`), **`read_temporal_artifact_parquet`** (`parquet.rs:501`), and the streaming variant introduced in #2
   - Replace single-file open with: read manifest, enumerate `temporal_shards`, open each `temporal_edges/{N:05}.parquet`, chain RecordBatch iterators in shard order. Same for `symbol_snapshots/*`.
   - If `manifest.temporal_shards` is empty AND legacy `temporal_edges.parquet` is present, return error (shouldn't happen post-`SCHEMA_VERSION` bump, but defensive).
   - Reader API surface is unchanged; only the internal file-iteration changes.

### CLI surface

`crates/spur-cli/src/commands/graph.rs::build` — hidden flags:

- `--temporal-max-rows-per-shard <N>` (default 100_000)
- `--temporal-max-commits-per-shard <N>` (default 5_000)

Not surfaced in `--help` of the default path; for tuning and tests.

## Data flow

```
CLI build (or rebuild path)
  ├─ stage = ArtifactStagingDir::new(canonical_root, hash)
  ├─ sink  = TemporalShardSink::new(stage.path(), cfg)
  ├─ run_full_walk_into(opts, &mut graph, Some(&mut sink)):
  │   for sha in commit_shas:
  │     push to graph.commits
  │     push N to graph.temporal_edges
  │     push M to graph.symbol_snapshots
  │     sink.append_commit(&commit, &mut edges, &mut snapshots)
  │       ├─ if first call this shard: open writers in stage.path()
  │       ├─ stream rows into current shard writers
  │       ├─ edges.clear(); snapshots.clear()
  │       └─ if threshold: close current writers, append ShardIndexEntry,
  │          increment shard_idx
  ├─ shard_index = sink.finalize()
  ├─ write_artifact_parquet(stage.path(), &graph):
  │     writes commits.parquet, files, symbols, structural edges into stage.path()
  │     manifest.temporal_shards = shard_index
  └─ stage.commit()  → atomic rename into canonical hash-dir
```

## Error handling

- **Shard writer open failure**: propagate; staging dir cleanup via `ArtifactStagingDir::drop`.
- **Partial shard on crash**: the staging dir is never atomically committed, so the canonical hash-dir is untouched. Next build attempt starts fresh.
- **Empty walk** (zero commits): no shard files written; `manifest.temporal_shards = []`. Read path returns empty iterators for both tables.
- **Empty trailing residual** (all flushed cleanly): no extra shard file emitted; index ends at the last real shard.

## Testing

### Unit tests (new)

In `crates/spur-graph/src/store/shard_writer.rs`:
- Rotation on row threshold.
- Rotation on commit threshold.
- Residual flush (small tail).
- Empty walk (no shards, empty index).
- Mega-commit larger than threshold goes into one shard (per-commit atomicity).
- `ShardIndexEntry` `commit_time_{min,max}` correctly span all author_times in the shard.
- Out-of-order commit times within a shard (synthetic merge with older parent) still produce correct min/max — i.e. don't assume monotonicity.

### Integration tests (new)

In `crates/spur-graph/tests/temporal_streaming.rs`:
- 200-commit synthetic history with varied change widths; assert resident `temporal_edges` Vec is empty at end-of-build, and total rows written ≈ sum of per-shard row counts.
- Round-trip parity: build with sink ON, read back via `read_temporal_artifact_parquet`; build same history with sink OFF (single-shard fallback path); assert set-equality on temporal edge rows.
- Reader handles 1-shard, N-shard cases.
- Incremental fast-forward: build over commits 1..100, then incremental build to 1..200; assert prior-artifact streaming seed path never materializes the full prior temporal artifact (check via a sink-instrumentation counter that asserts max-resident-rows < threshold).
- Merge commit with secondary parent's `commit_time` older than primary parent's: shard's `commit_time_min` correctly reflects the older time.

### Updated tests / fixtures (explicit migration list)

The following all reference `temporal_edges.parquet` / `symbol_snapshots.parquet` by literal filename and must be migrated:

| File | What changes |
|---|---|
| `crates/spur-graph/tests/parquet_roundtrip.rs` | fixtures write `temporal_edges/00000.parquet` |
| `crates/spur-graph/tests/temporal_resolution.rs` | same |
| `crates/spur-graph/tests/parquet_schema_invariants.rs` | verify `temporal_shards` manifest field |
| `crates/spur-graph/benches/parquet.rs`, `benches/incremental.rs` | fixture writer paths |
| `crates/spur-cli/tests/graph_build_temporal_cli.rs:49` | `.join("temporal_edges.parquet").is_file()` → check shard dir / manifest |
| `crates/spur-cli/tests/analyst_temporal_views.rs:233-247` | fixture builders that mutate `artifact.temporal_edges` in memory and call `write_artifact_parquet` → either keep using the single-shard fallback path (sink=None) or migrate to streaming API |

### Production downstream consumers (must be updated)

These read temporal parquet by literal filename and break with the sharded layout:

| File | Change |
|---|---|
| `crates/spur-cli/src/commands/analyst.rs:262-267` | detect temporal presence via `manifest.temporal_shards.is_empty()` rather than file-existence check |
| `crates/spur-context/poc/duckdb-analyst/init_temporal.sql:8-15,64-72` | replace `temporal_edges.parquet` with `temporal_edges/*.parquet` (DuckDB native glob) |

The `spur-mcp` handlers all go through `GraphQueryClient::temporal_index → read_temporal_artifact_parquet` (verified at `crates/spur-mcp/src/server/handlers/code_graph.rs:814,903,1410,1507-1510` and `crates/spur-graph/src/query_client.rs:746-756`) and need no changes.

## Risk assessment (spur-analyst grounding)

| Symbol | BR | hot callers | self churn 90d |
|---|---|---|---|
| `write_artifact_parquet` | 7.0 | 4 | 0 |
| `run_full_walk_into` | 5.9 | 6 | 0 |
| `write_temporal_edges` | 0.0 | 0 | 0 |
| `read_temporal_edges` | 0.0 | 0 | 0 |
| `read_temporal_artifact_parquet` | 0.76 | 1 | 0 |

- **Low risk** for the temporal-specific helpers — all BR=0 and self-churn=0.
- **Medium risk** for the orchestrators (`run_full_walk_into`, `write_artifact_parquet`). Mitigation: signature changes are minimal and all 16 callers of `run_full_walk_into` are local (1 prod + 15 tests/benches). Sink defaults to `None` for non-CLI callers preserving behavior.
- **New surface**: `ArtifactStagingDir` is a small RAII helper; risk concentrated in the one prod caller (`spur-cli/src/commands/graph.rs::build`).

## Success criteria

1. `spur graph build --workspace --with-temporal` on `/Volumes/Projects/duckdb` completes without OOM on a 16 GB machine.
2. Peak RSS attributable to the build's temporal pipeline < 200 MB sustained, independent of repo history depth (measured under `MallocStackLogging` or `dtruss`).
3. Incremental `--with-temporal` over a fast-forward range also bounded < 200 MB (no full prior-artifact load).
4. All existing parquet round-trip, temporal-resolution, and analyst-temporal-views tests pass against the sharded layout.
5. New streaming-specific tests pass (rotation, residual, empty, mega-commit atomicity, round-trip parity, incremental seed, merge non-monotonic time).
6. Read path API unchanged: `read_temporal_artifact_parquet` and `GraphQueryClient::temporal_index` return the same shape.

## Out-of-scope follow-ups (explicit)

- **Read-side `TemporalIndex` streaming** (`crates/spur-graph/src/temporal.rs:94-160`). Sharding the writer bounds build memory; queries still build full HashMaps. Track as a separate ticket — likely candidate for DuckDB-backed lookups.
- **DuckDB as the temporal sink** instead of Parquet.
- **Parallelizing shard writes** within a single build.
- **Append-only incremental rebuild** (write only the new shards, leave prior shards on disk). Sharding makes this feasible; current spec still rewrites the artifact dir to a new content_hash on rebuild.
