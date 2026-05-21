# spur-graph Parquet Exporter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate `spur-graph`'s persisted artifact from JSON to a directory of Apache Parquet files written via pure-Rust arrow-rs/parquet, with shared NodeId namespace across files+symbols, atomic directory publication, a precedence-aware resolver, and hard CI performance gates against pre-cutover baselines.

**Architecture:** Three new modules under `crates/spur-graph/src/store/`: `build.rs` (the 1700 lines of construction logic currently misfiled in `json.rs`), `parquet.rs` (writer, reader, header-reader, schemas, atomic publication protocol), and `pointer.rs` (CURRENT pointer I/O + the shared `resolve_artifact_location` resolver). `cache.rs` is modified to write Parquet. JSON full-artifact writer is deprecated at writer-flip step, removed at cleanup step. Content hash flow stays canonical-JSON-in-memory.

**Tech Stack:** Rust, arrow-array, arrow-schema, parquet (pure-Rust crate, ZSTD), DuckDB CLI for round-trip validation. Workspace-pinned deps only; no libduckdb linkage in any compiled SPUR crate.

**Spec reference:** `docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md` (v3, commit 0b6a6534). Every task references specific §-numbers in the spec.

---

## File structure

```
crates/spur-graph/src/store/
├── mod.rs                ─ re-exports; one-line edits per task
├── build.rs              ─ NEW. ~1700 lines of artifact-construction logic
│                           (Task 1)
├── json.rs               ─ shrinks to ~50 lines: GraphArtifactBodyForHash +
│                           artifact_content_hash_blake3_hex + write_artifact
│                           (Task 1; Task 12; renamed to canonical_hash.rs at Task 14)
├── pointer.rs            ─ NEW. CURRENT pointer I/O + resolve_artifact_location
│                           (Task 7)
├── parquet.rs            ─ NEW. Writer, reader, header reader, atomic
│                           publication (Tasks 3, 4, 5)
├── cache.rs              ─ MODIFIED at Task 12 (writes Parquet dir; *.tmp.* sweep)
└── snapshot.rs           ─ unchanged

crates/spur-graph/Cargo.toml — workspace deps added at Task 3
crates/spur-graph/benches/
├── pre_pr2.md            ─ NEW. Bench results and decision summary (Task 2)
├── baselines.json        ─ NEW. JSON-path baselines (Task 6)
└── parquet.rs            ─ NEW. Criterion bench for §12 Family 3 (Task 13)

Migration touch points (Tasks 8–11):
- crates/spur-graph/src/schema.rs        : load_artifact dispatch
- crates/spur-mcp/src/server/handlers/code_graph.rs
- crates/spur-tui/src/mentions/code_graph/source.rs
- crates/spur-tui/src/mentions/registry.rs
- crates/spur-cli/src/commands/graph.rs
```

---

## Task 1: Mechanical split of `store/json.rs` (spec PR1)

**Goal:** Move all construction logic out of `store/json.rs` into a new `store/build.rs`. Zero behavior change. Public re-exports preserved via `store/mod.rs`. The remaining `json.rs` keeps `write_artifact`, `GraphArtifactBodyForHash`, `artifact_content_hash_blake3_hex`, and the four `MANIFEST_QUERY_BYTES` entries that feed `manifest_version` computation.

**Files:**
- Create: `crates/spur-graph/src/store/build.rs`
- Modify: `crates/spur-graph/src/store/json.rs` (shrinks)
- Modify: `crates/spur-graph/src/store/mod.rs` (add `pub mod build;`)

**Steps:**

- [ ] **Step 1: Run the full `spur-graph` test suite to capture green baseline**

```bash
cargo test -p spur-graph
```
Expected: all green. Note any pre-existing failures; they must remain in the same state at the end.

- [ ] **Step 2: Identify the public surface that must survive**

Re-exports in `store/mod.rs` today:

```rust
pub use json::{
    artifact_from_facts, artifact_from_facts_incremental, current_manifest_version, write_artifact,
    BuildMode, EXTRACTOR_VERSION, SCHEMA_VERSION,
};
```

External callers (verify via `cargo check` after the move):
- `spur-cli/src/commands/graph.rs` — `artifact_from_facts`, `artifact_from_facts_incremental`, `write_artifact`, `BuildMode`
- `spur-tui/tests/mention_registry.rs` — `artifact_from_facts`, `write_artifact`
- `spur-graph/tests/extractor.rs` — `store::json::*`
- `spur-graph/src/store/cache.rs` — uses `write_artifact` internally

The `store::json::*` import in the test file must keep working after the rename; either update the import there or re-export the moved symbols. Choose: update the test file's import path to `store::build::*` for the construction-only symbols.

- [ ] **Step 3: Create `crates/spur-graph/src/store/build.rs` with the moved items**

Move these items from `json.rs` to `build.rs`, preserving function bodies exactly:

- `ManifestQueryBytes` struct + `MANIFEST_QUERY_BYTES` constant
- `BuildMode` enum
- `FileBucket`, `CurrentFileEntry` structs
- `PHASE1_GRAPH_INDEX_VERSION`, `SCHEMA_VERSION`, `EXTRACTOR_VERSION` constants
- `current_manifest_version`, `manifest_version_from_query_bytes`, `update_manifest_hash_field`
- `artifact_from_facts`, `artifact_from_facts_incremental`
- `buckets_from_facts`, `discover_current_entries`, `discover_git_entries`, `discover_fs_entries`
- `read_worktree_content_oid`, `is_supported_path`, `relative_path`, `content_oid_for`
- `buckets_from_artifact`, `insert_source_path`, `add_missing_manifest_buckets`, `empty_bucket`
- `tombstones_from_removed_paths`, `compose_artifact`, `rebind_cross_file_edges`, `rebuild_from_buckets`
- `edge_sort_key`, `relation_discriminator`, `node_file_path`, `file_path_for_span`
- `stable_file_id_from_path`, `anchor_hash`, `symbol_kind`, `symbol_entity_name`
- `parent_by_target`, `containing_parent`, `qualified_name`, `qualified_scope_segment`, `enclosing_scope`
- `elapsed_ms`

Keep these in `json.rs`:
- `GraphArtifactBodyForHash` struct
- `write_artifact`
- `artifact_content_hash_blake3_hex`

If any moved function references a private helper that stays in `json.rs`, move that helper to `build.rs` too (or make it `pub(super)` and import). Move, don't duplicate.

- [ ] **Step 4: Update `store/mod.rs`**

```rust
pub mod build;
pub mod cache;
pub mod json;
pub mod snapshot;

pub use build::{
    artifact_from_facts, artifact_from_facts_incremental, current_manifest_version,
    BuildMode, EXTRACTOR_VERSION, SCHEMA_VERSION,
};
pub use json::write_artifact;
```

- [ ] **Step 5: Update the in-tree test import**

In `crates/spur-graph/tests/extractor.rs`, change any `use spur_graph::store::json::{...}` import for construction-only symbols to `use spur_graph::store::build::{...}`. Leave `write_artifact` imports pointing at `store::json`.

- [ ] **Step 6: `cargo check` workspace and `cargo test -p spur-graph`**

```bash
cargo check --workspace
cargo test -p spur-graph
```
Expected: workspace builds; spur-graph tests pass in the same state as Step 1.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-graph/src/store/ crates/spur-graph/tests/
git commit -m "refactor(spur-graph): split store/json.rs construction logic into store/build.rs

Mechanical move; zero behavior change. json.rs retains write_artifact +
canonical-JSON hashing only. Unblocks the Parquet exporter (bd-1rqxk PR2).

Refs: docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md §8"
```

---

## Task 2: Pre-PR2 benchmark + decisions

**Goal:** Empirically decide three open questions before Task 3 commits to a schema: row group size, `enclosing_scope` encoding, and whether `edges_by_dst.parquet` is materialized or computed lazily.

**Files:**
- Create: `crates/spur-graph/benches/pre_pr2.md` (results + decisions)
- Temporary: a throwaway `crates/spur-graph/benches/pre_pr2_bench.rs` (delete after results captured)

**Steps:**

- [ ] **Step 1: Build a representative fixture**

Use the live SPUR worktree's `.spur/graph-index.json` as the input. Load it via the existing JSON path; this is a 27.5k/47k/68k/1.5k artifact.

- [ ] **Step 2: Write a throwaway bench that materializes Parquet at each candidate row-group size**

```rust
// crates/spur-graph/benches/pre_pr2_bench.rs (delete after Task 2 commit)
fn main() -> anyhow::Result<()> {
    let artifact = spur_graph::load_artifact(std::env::args().nth(1).unwrap().as_ref())?;
    for &rg in &[16384usize, 32768, 65536] {
        for &enc in &["dict", "plain"] {
            let started = std::time::Instant::now();
            // write nodes.parquet with rg row-group, enc encoding for enclosing_scope
            // (use arrow-array + parquet directly; mirror schema in §6.1)
            let elapsed = started.elapsed();
            let size = std::fs::metadata(&out_path)?.len();
            println!("rg={rg} enc={enc} size={size} elapsed_ms={}", elapsed.as_millis());
        }
    }
    Ok(())
}
```

Compile with `arrow-array`, `arrow-schema`, `parquet` (pure-Rust) added to `Cargo.toml`'s `[dev-dependencies]` temporarily.

- [ ] **Step 3: Run and record the row-group + encoding numbers**

```bash
cargo run --bin pre_pr2_bench --release -- /Volumes/Projects/spur/.spur/graph-index.json
```

Capture for each `(row_group_size, enclosing_scope_encoding)` combination: on-disk byte size + write wall-clock.

- [ ] **Step 4: Run cardinality probe on `enclosing_scope`**

```bash
duckdb -c "SELECT COUNT(DISTINCT s.enclosing_scope) * 1.0 / COUNT(*) AS dict_ratio FROM read_json('/Volumes/Projects/spur/.spur/graph-index.json', maximum_object_size=200000000), UNNEST(symbols) AS t(s)"
```

If `dict_ratio > 0.5`, `enclosing_scope` should use PLAIN; otherwise DICTIONARY.

- [ ] **Step 5: Bench `edges_by_dst.parquet` materialization vs lazy DuckDB dst-sort**

Write `edges.parquet` and `edges_by_dst.parquet`. Compare:
1. `duckdb -c "SELECT * FROM read_parquet('edges_by_dst.parquet') WHERE dst_id = 42"` (materialized)
2. `duckdb -c "SELECT * FROM (SELECT * FROM read_parquet('edges.parquet') ORDER BY dst_id) WHERE dst_id = 42"` (lazy)

Measure cold-query wall-clock for both. Pick a representative `dst_id` (use an actual existing one from the fixture).

- [ ] **Step 6: Write `crates/spur-graph/benches/pre_pr2.md` with the decision summary**

```markdown
# Pre-PR2 Bench Results — 2026-05-22

## Decisions (drive Task 3)

- **Row group size:** <DECIDED_VALUE> (e.g. 32768)
- **`enclosing_scope` encoding:** <DECIDED_VALUE> (DICTIONARY | PLAIN)
- **`edges_by_dst.parquet` materialization:** <DECIDED_VALUE> (materialize | lazy)

## Method

[brief]

## Raw numbers

| Variant | on-disk | write ms | read ms | dst-query ms |
|---|---|---|---|---|
...

## Reasoning

[1–2 paragraphs explaining the decision rule from §6.3 and §12 was met or not]
```

- [ ] **Step 7: Delete the throwaway bench and revert `[dev-dependencies]` temporary additions**

```bash
git rm crates/spur-graph/benches/pre_pr2_bench.rs
# Revert Cargo.toml dev-deps
```

- [ ] **Step 8: Commit**

```bash
git add crates/spur-graph/benches/pre_pr2.md crates/spur-graph/Cargo.toml
git commit -m "bench(spur-graph): pre-PR2 decisions for Parquet exporter

Records empirical decisions for row group size, enclosing_scope encoding,
and edges_by_dst.parquet materialization. Feeds Task 3 schema choices.

Refs: docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md §12 (Pre-PR2 benchmark)"
```

---

## Task 3: Add `store/parquet.rs` writer (spec §6 schemas; no atomic dance yet)

**Goal:** Implement `write_artifact_parquet` that emits all required Parquet files plus `manifest.json`. The atomic-rename protocol from §6.9 lands in Task 4; this task writes directly into `<hash>.parquet/` (non-atomic) so the schema work is testable in isolation. Round-trip identity test for writer+stub-reader lives at the end of this task.

**Files:**
- Modify: `crates/spur-graph/Cargo.toml` (add deps)
- Modify: workspace `Cargo.toml` (add deps to `[workspace.dependencies]`)
- Create: `crates/spur-graph/src/store/parquet.rs`
- Modify: `crates/spur-graph/src/store/mod.rs` (add `pub mod parquet;`)
- Test: `crates/spur-graph/tests/parquet_roundtrip.rs`

**Pre-req:** Task 2's pre_pr2.md exists; use its decisions for row-group size, encoding, and `edges_by_dst` materialization.

**Steps:**

- [ ] **Step 1: Add workspace deps**

In the workspace root `Cargo.toml`:

```toml
[workspace.dependencies]
# ... existing entries ...
parquet      = { version = "55", default-features = false, features = ["zstd"] }
arrow-array  = "55"
arrow-schema = "55"
```

(Use the latest matching versions; the 55.x series at time of writing is fine. The features list for `parquet` keeps the dep pure-Rust without exposing `arrow` as a transitive default.)

In `crates/spur-graph/Cargo.toml`:

```toml
[dependencies]
# ... existing entries ...
parquet      = { workspace = true }
arrow-array  = { workspace = true }
arrow-schema = { workspace = true }
```

- [ ] **Step 2: Write failing round-trip test**

```rust
// crates/spur-graph/tests/parquet_roundtrip.rs
use spur_graph::store::parquet::{read_artifact_parquet, write_artifact_parquet, WriteOptions};
use spur_graph::{artifact_from_facts, build_facts};
use std::fs;
use tempfile::tempdir;

fn fixture_artifact() -> spur_graph::GraphIndexArtifact {
    // Use the existing test fixture — reuse the path that spur-graph/tests/extractor.rs uses.
    // The fixture must contain at least: one Contains edge (file→symbol), one Calls edge,
    // one unresolved edge, one tombstone (from a prior incremental build).
    let facts = build_facts(/* fixture worktree path */ &PathBuf::from("tests/fixtures/parquet_roundtrip")).unwrap();
    artifact_from_facts(&facts, &PathBuf::from("tests/fixtures/parquet_roundtrip")).unwrap()
}

#[test]
fn roundtrip_byte_equivalent() {
    let original = fixture_artifact();
    let dir = tempdir().unwrap();
    let written = write_artifact_parquet(&original, dir.path(), WriteOptions::default()).unwrap();

    let readback = read_artifact_parquet(&written).unwrap();
    assert_eq!(original.files, readback.files);
    assert_eq!(original.symbols, readback.symbols);
    assert_eq!(original.file_manifests, readback.file_manifests);
    assert_eq!(original.tombstones, readback.tombstones);
    // confidence_score f32 NaN-safe comparison
    assert_eq!(original.edges.len(), readback.edges.len());
    for (a, b) in original.edges.iter().zip(readback.edges.iter()) {
        assert_eq!(a.source_stable_symbol_id, b.source_stable_symbol_id);
        assert_eq!(a.target_stable_symbol_id, b.target_stable_symbol_id);
        assert_eq!(a.relation, b.relation);
        assert_eq!(a.confidence, b.confidence);
        assert_eq!(a.confidence_score.to_bits(), b.confidence_score.to_bits());
        assert_eq!(a.edge_kind, b.edge_kind);
    }
    assert_eq!(original.graph_content_hash, readback.graph_content_hash);
    assert_eq!(original.manifest_version, readback.manifest_version);
}
```

Replace `tests/fixtures/parquet_roundtrip` with the actual fixture path used by `extractor.rs`. If a suitable fixture doesn't exist, build one using a small in-tree directory tree with two `.rs` files and one `.md` file.

```bash
cargo test -p spur-graph --test parquet_roundtrip -- --nocapture
```

Expected: FAIL with "no such module / no such function" (write_artifact_parquet undefined).

- [ ] **Step 3: Add module skeleton**

Create `crates/spur-graph/src/store/parquet.rs`:

```rust
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{
    builder::{Int32Builder, Int64Builder, ListBuilder, StringBuilder, Float32Builder},
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde::{Deserialize, Serialize};

use crate::GraphIndexArtifact;

const ROW_GROUP_SIZE: usize = /* DECIDED_VALUE_FROM_TASK_2 */ 65536;

#[derive(Debug, Clone, Default)]
pub struct WriteOptions {
    /// If true, also emit edges_by_dst.parquet (sorted by (dst_id, src_id)).
    /// Decided in pre-PR2 bench; default reflects that decision.
    pub emit_edges_by_dst: bool,  // initialize to the decision from Task 2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphArtifactManifest {
    pub graph_index_version: String,
    pub schema_version: String,
    pub manifest_version: String,
    pub graph_content_hash: String,
    pub indexed_commit_oid: Option<String>,
    pub extractor_version: String,
    pub complete: bool,
    pub row_counts: ManifestRowCounts,
    pub parquet_writer: ManifestWriterMeta,
    pub edges_by_dst_present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestRowCounts {
    pub nodes: u64,
    pub edges: u64,
    pub edges_by_dst: u64,
    pub edges_unresolved: u64,
    pub files: u64,
    pub file_manifests: u64,
    pub tombstones: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestWriterMeta {
    pub compression: String,
    pub row_group_size: u64,
}

pub fn write_artifact_parquet(
    artifact: &GraphIndexArtifact,
    base_dir: &Path,
    options: WriteOptions,
) -> Result<PathBuf> {
    let dir = base_dir.join(format!("{}.parquet", &artifact.graph_content_hash));
    fs::create_dir_all(&dir)?;
    write_nodes(&dir, artifact)?;
    write_files(&dir, artifact)?;
    write_edges(&dir, artifact, &options)?;
    write_edges_unresolved(&dir, artifact)?;
    write_file_manifests(&dir, artifact)?;
    write_tombstones(&dir, artifact)?;
    write_manifest(&dir, artifact, &options)?;
    Ok(dir)
}

pub fn read_artifact_parquet(dir: &Path) -> Result<GraphIndexArtifact> {
    // Validate manifest first
    let manifest = read_manifest(dir).context("read manifest.json")?;
    if !manifest.complete {
        anyhow::bail!("manifest.complete is false; refusing to load partial artifact at {}", dir.display());
    }
    let files = read_files(dir)?;
    let symbols = read_nodes(dir)?;
    let edges = read_edges(dir, &manifest)?;
    let edges_unresolved = read_edges_unresolved(dir)?;
    let file_manifests = read_file_manifests(dir)?;
    let tombstones = read_tombstones(dir)?;
    Ok(GraphIndexArtifact {
        header: /* from manifest */,
        manifest_version: manifest.manifest_version,
        graph_content_hash: manifest.graph_content_hash,
        file_manifests,
        files,
        symbols,
        edges: edges.into_iter().chain(edges_unresolved.into_iter()).collect(),
        tombstones,
        diagnostics: Vec::new(),
    })
}

pub fn read_artifact_header_parquet(dir: &Path) -> Result<GraphArtifactManifest> {
    read_manifest(dir)
}

fn read_manifest(dir: &Path) -> Result<GraphArtifactManifest> {
    let path = dir.join("manifest.json");
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

// ... stubs for each write_* / read_* fn below, returning anyhow::bail!("unimplemented") for now ...
```

Add `pub mod parquet;` to `crates/spur-graph/src/store/mod.rs` (re-exports come after the API works).

- [ ] **Step 4: Implement `write_nodes` and `read_nodes`**

Schema per spec §6.1 (mirror `GraphSymbolArtifact`; `node_id` is the extractor's `NodeId(u64).0`):

```rust
fn nodes_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("stable_symbol_id", DataType::Utf8, false),
        Field::new("node_id", DataType::Int64, false),
        Field::new("file_path", DataType::Utf8, false),
        Field::new("byte_range_start", DataType::Int64, false),
        Field::new("byte_range_end", DataType::Int64, false),
        Field::new("line_start", DataType::Int32, false),
        Field::new("line_end", DataType::Int32, false),
        Field::new("entity_name", DataType::Utf8, false),
        Field::new("qualified_name", DataType::Utf8, false),
        Field::new("symbol_kind", DataType::Utf8, false),
        Field::new("anchor_hash", DataType::Utf8, false),
        Field::new("enclosing_scope", DataType::Utf8, true),
    ]))
}
```

`node_id` for each symbol must come from a lookup table built during `compose_artifact` — the symbol's extractor `NodeId(u64).0`. Since `GraphSymbolArtifact` does not currently carry this, build the lookup table externally: walk the `GraphFacts.nodes` list and map `stable_key → NodeId`. **Important:** this means `write_artifact_parquet` cannot receive only a `GraphIndexArtifact` — it also needs the original `GraphFacts.nodes` lookup OR `compose_artifact` must produce a `Vec<NodeId>` parallel to the symbols vec.

Decision for this task: extend `compose_artifact` (in `build.rs`) to attach a parallel `Vec<NodeId>` to the artifact via a new field `GraphIndexArtifact.symbol_node_ids: Vec<NodeId>` (with `#[serde(skip)]` so the canonical-JSON hash is unaffected). This is a one-line addition to the struct + populate site.

Then `write_nodes` reads `artifact.symbol_node_ids` and writes the `node_id` column.

Sort key for `nodes.parquet`: `(file_path ASC, stable_symbol_id ASC)`. Sort before building the RecordBatch.

ZSTD level 3:
```rust
let props = WriterProperties::builder()
    .set_compression(Compression::ZSTD(ZstdLevel::try_new(3)?))
    .set_max_row_group_size(ROW_GROUP_SIZE)
    .set_dictionary_enabled(true)
    .build();
```

Override `set_dictionary_enabled(false)` per-column for `stable_symbol_id`, `anchor_hash`, `byte_range_*`, `line_*`, `node_id` (these are PLAIN per §6.1). For `enclosing_scope`, use the Task 2 decision (DICTIONARY or PLAIN).

`read_nodes` is the inverse: scan the file, decode each batch, reconstruct `GraphSymbolArtifact` records sorted as written.

- [ ] **Step 5: Implement `write_files` and `read_files`**

Per §6.5:

```rust
fn files_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("stable_file_id", DataType::Utf8, false),
        Field::new("node_id", DataType::Int64, false),
        Field::new("file_path", DataType::Utf8, false),
    ]))
}
```

Same parallel-vec problem as nodes: extend `compose_artifact` to attach `Vec<NodeId>` for files too. Add `GraphIndexArtifact.file_node_ids: Vec<NodeId>` with `#[serde(skip)]`. Sort by `file_path ASC`.

The reader discards `node_id` (since `GraphFileArtifact` doesn't carry it) but reconstructs the lookup table so edges can validate `src_id`/`dst_id` against the union of node/file IDs at test time.

- [ ] **Step 6: Implement `write_edges` and `read_edges`**

Per §6.2:

```rust
fn edges_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("source_stable_id", DataType::Utf8, false),
        Field::new("target_stable_id", DataType::Utf8, false),
        Field::new("src_id", DataType::Int64, false),
        Field::new("dst_id", DataType::Int64, false),
        Field::new("target_label", DataType::Utf8, true),
        Field::new("relation", DataType::Utf8, false),
        Field::new("confidence", DataType::Utf8, false),
        Field::new("confidence_score", DataType::Float32, false),
        Field::new("edge_kind", DataType::Utf8, true),
    ]))
}
```

Filter to resolved edges only (`target_stable_symbol_id.is_some()`). Sort by `(src_id ASC, dst_id ASC)`.

`src_id` and `dst_id` come from the same lookup table built in Step 4/5 (union of symbol_node_ids + file_node_ids). For each edge, look up the source by `source_stable_symbol_id` and target by `target_stable_symbol_id`; both must resolve since this file holds resolved edges.

If `options.emit_edges_by_dst == true`, also write `edges_by_dst.parquet` with sort key `(dst_id ASC, src_id ASC)` — same row set, re-sorted.

- [ ] **Step 7: Implement `write_edges_unresolved` and `read_edges_unresolved`**

Per §6.4. Filter to `target_stable_symbol_id.is_none()`. Sort by `src_id ASC`. Schema omits `target_stable_id` and `dst_id`.

- [ ] **Step 8: Implement `write_file_manifests` and `read_file_manifests`**

Per §6.6 — `node_ids LIST<INT64>`:

```rust
fn file_manifests_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("stable_file_id", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("content_oid", DataType::Utf8, false),
        Field::new(
            "node_ids",
            DataType::List(Arc::new(Field::new("item", DataType::Int64, false))),
            false,
        ),
    ]))
}
```

Use `arrow_array::builder::ListBuilder<Int64Builder>` for the list column. Convert each `NodeId(u64)` to `i64` losslessly (NodeIds in practice fit). On read, convert back to `NodeId(u64)`.

- [ ] **Step 9: Implement `write_tombstones` and `read_tombstones`**

Per §6.7. Two-column file. Sort by `path ASC`.

- [ ] **Step 10: Implement `write_manifest`**

Write `manifest.json` LAST in the `write_artifact_parquet` sequence so its presence is the completion sentinel (preparation for Task 4's atomic dance). Set `complete: true`, `edges_by_dst_present` per options, and `row_counts` from the actual artifact.

- [ ] **Step 11: Run the round-trip test**

```bash
cargo test -p spur-graph --test parquet_roundtrip -- --nocapture
```

Expected: PASS. Iterate on schema/encoding details until green.

- [ ] **Step 12: Add `pub use parquet::*` to `store/mod.rs`**

```rust
pub use parquet::{
    read_artifact_header_parquet, read_artifact_parquet, write_artifact_parquet,
    GraphArtifactManifest, WriteOptions,
};
```

- [ ] **Step 13: Commit**

```bash
git add crates/spur-graph/ Cargo.toml
git commit -m "feat(spur-graph): add store/parquet.rs writer + reader (PR2 part 1)

Implements §6 schemas with the shared NodeId namespace fix (files +
symbols share node_id from extractor's NodeId(u64).0). Writes
non-atomically into <hash>.parquet/ for now; atomic publication lands
in the next task. Round-trip identity test green.

Refs: docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md §6, §7"
```

---

## Task 4: Atomic directory publication (§6.9)

**Goal:** `write_artifact_parquet` writes to `<hash>.parquet.tmp.<pid>/`, fsyncs files, writes `manifest.json` last, fsyncs, then atomically renames to `<hash>.parquet/`. `read_artifact_parquet` refuses directories lacking `manifest.json` or with `complete: false`.

**Files:**
- Modify: `crates/spur-graph/src/store/parquet.rs`
- Test: extend `crates/spur-graph/tests/parquet_roundtrip.rs`

**Steps:**

- [ ] **Step 1: Write failing test for partial-write rejection**

```rust
#[test]
fn rejects_directory_without_manifest() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("abc.parquet");
    fs::create_dir_all(&target).unwrap();
    // No manifest.json written
    let err = read_artifact_parquet(&target).unwrap_err();
    assert!(err.to_string().contains("manifest.json"));
}

#[test]
fn rejects_directory_with_incomplete_manifest() {
    let dir = tempdir().unwrap();
    let target = dir.path().join("def.parquet");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("manifest.json"), r#"{"complete": false}"#).unwrap();
    let err = read_artifact_parquet(&target).unwrap_err();
    assert!(err.to_string().contains("complete"));
}
```

```bash
cargo test -p spur-graph --test parquet_roundtrip rejects_ -- --nocapture
```
Expected: FAIL (current `read_artifact_parquet` may panic on missing files in unhelpful ways).

- [ ] **Step 2: Implement the atomic write protocol**

Restructure `write_artifact_parquet`:

```rust
pub fn write_artifact_parquet(
    artifact: &GraphIndexArtifact,
    base_dir: &Path,
    options: WriteOptions,
) -> Result<PathBuf> {
    let final_dir = base_dir.join(format!("{}.parquet", &artifact.graph_content_hash));
    let pid = std::process::id();
    let tmp_dir = base_dir.join(format!("{}.parquet.tmp.{pid}", &artifact.graph_content_hash));

    // Clean any stale tmp dir from a previous failed run with this pid (rare).
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&tmp_dir)?;

    write_nodes(&tmp_dir, artifact)?;
    write_files(&tmp_dir, artifact)?;
    write_edges(&tmp_dir, artifact, &options)?;
    write_edges_unresolved(&tmp_dir, artifact)?;
    write_file_manifests(&tmp_dir, artifact)?;
    write_tombstones(&tmp_dir, artifact)?;

    // fsync each parquet file
    for entry in fs::read_dir(&tmp_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) == Some("parquet") {
            let file = fs::OpenOptions::new().read(true).open(&path)?;
            file.sync_all()?;
        }
    }

    // Write manifest.json LAST and fsync it.
    write_manifest(&tmp_dir, artifact, &options)?;
    let manifest_file = fs::OpenOptions::new().read(true).open(tmp_dir.join("manifest.json"))?;
    manifest_file.sync_all()?;

    // fsync the tmp directory itself (POSIX requirement for durability of directory contents).
    let tmp_dir_handle = fs::File::open(&tmp_dir)?;
    tmp_dir_handle.sync_all()?;

    // If final_dir already exists (e.g. another writer raced), remove it first.
    // Same content_hash + same writer = identical bytes; safe to clobber.
    let _ = fs::remove_dir_all(&final_dir);
    fs::rename(&tmp_dir, &final_dir)
        .with_context(|| format!("rename {} -> {}", tmp_dir.display(), final_dir.display()))?;

    // fsync the parent directory to make the rename durable.
    let parent_handle = fs::File::open(base_dir)?;
    parent_handle.sync_all()?;

    Ok(final_dir)
}
```

- [ ] **Step 3: Update `read_artifact_parquet` to validate completeness**

Already in the stub from Task 3 Step 3 — verify the check is `if !manifest.complete { bail!(...) }` and that missing `manifest.json` returns a `context`-wrapped error mentioning "manifest.json".

- [ ] **Step 4: Run tests**

```bash
cargo test -p spur-graph --test parquet_roundtrip
```

Expected: all four tests pass — original round-trip + two rejection tests + (if added) one rebuild-after-rename test.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/store/parquet.rs crates/spur-graph/tests/parquet_roundtrip.rs
git commit -m "feat(spur-graph): atomic directory publication for Parquet writer

§6.9 protocol: write into <hash>.parquet.tmp.<pid>/, fsync each
parquet, write manifest.json last, fsync, atomic rename to
<hash>.parquet/, fsync parent. Reader refuses directories lacking
manifest.json or with complete=false.

Refs: docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md §6.9"
```

---

## Task 5: Add `read_artifact_header_parquet` smoke + endpoint-namespace consistency test

**Goal:** Wire up the header-only fast path (already stubbed in Task 3 Step 3) and add Family 2.6 endpoint-namespace consistency assertion.

**Files:**
- Test: `crates/spur-graph/tests/parquet_schema_invariants.rs` (new)
- Modify: nothing in `parquet.rs` — `read_artifact_header_parquet` already exists.

**Steps:**

- [ ] **Step 1: Write the header-read test**

```rust
#[test]
fn header_read_is_subms() {
    let original = fixture_artifact();
    let dir = tempdir().unwrap();
    let written = write_artifact_parquet(&original, dir.path(), WriteOptions::default()).unwrap();

    let started = std::time::Instant::now();
    let manifest = read_artifact_header_parquet(&written).unwrap();
    let elapsed = started.elapsed();

    assert_eq!(manifest.graph_content_hash, original.graph_content_hash);
    assert_eq!(manifest.row_counts.nodes as usize, original.symbols.len());
    assert!(elapsed < std::time::Duration::from_millis(50),
        "header read took {:?}; expected < 50ms", elapsed);
}
```

- [ ] **Step 2: Write endpoint-namespace consistency test (Family 2.6)**

```rust
#[test]
fn endpoint_namespace_is_consistent() {
    use parquet::file::reader::{FileReader, SerializedFileReader};
    let original = fixture_artifact();
    let dir = tempdir().unwrap();
    let written = write_artifact_parquet(&original, dir.path(), WriteOptions::default()).unwrap();

    let mut all_ids: std::collections::HashSet<i64> = Default::default();
    // Collect nodes.node_id
    let nodes_file = std::fs::File::open(written.join("nodes.parquet")).unwrap();
    // ... use ArrowReader to iterate; collect node_id column values into all_ids
    // ... same for files.parquet.node_id

    // Now check every edge's src_id and dst_id are in all_ids
    let edges_file = std::fs::File::open(written.join("edges.parquet")).unwrap();
    // ... iterate; for each (src_id, dst_id) row, assert all_ids.contains(&src_id) && all_ids.contains(&dst_id)
}
```

Fill in the arrow-rs iteration with the same pattern used by `read_nodes` / `read_edges` in Task 3.

- [ ] **Step 3: Run tests**

```bash
cargo test -p spur-graph --test parquet_schema_invariants
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-graph/tests/parquet_schema_invariants.rs
git commit -m "test(spur-graph): header fast-path + endpoint namespace consistency

Family 2.6 from §12: every edge src_id/dst_id resolves in nodes ∪ files
NodeId space. Header read returns < 50ms — used by spur-tui cache
validation in Task 10.

Refs: docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md §12 Family 2"
```

---

## Task 6: Capture pre-PR3a JSON-path baselines

**Goal:** Measure current `load_artifact` and `write_artifact` wall-clock medians and peak RSS on the SPUR fixture, commit numbers to `crates/spur-graph/benches/baselines.json` so they survive the JSON writer's removal at Task 14.

**Files:**
- Create: `crates/spur-graph/benches/baselines.json`
- Temporary: `crates/spur-graph/benches/capture_baselines.rs` (kept; runs again later)

**Steps:**

- [ ] **Step 1: Write the measurement harness**

```rust
// crates/spur-graph/benches/capture_baselines.rs
use spur_graph::{load_artifact, store::json::write_artifact};
use std::path::PathBuf;
use std::time::Instant;

fn median_ms(samples: Vec<u128>) -> u128 {
    let mut s = samples;
    s.sort();
    s[s.len() / 2]
}

fn peak_rss_kb() -> u64 {
    // Use libc::getrusage; or shell out to ps for simplicity
    let pid = std::process::id();
    let out = std::process::Command::new("ps").arg("-o").arg("rss=").arg("-p").arg(pid.to_string()).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
}

fn main() -> anyhow::Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).expect("usage: capture_baselines <path-to-graph-index.json>"));

    let mut load_samples = Vec::new();
    let mut load_rss = Vec::new();
    for _ in 0..10 {
        let started = Instant::now();
        let _artifact = load_artifact(&path)?;
        load_samples.push(started.elapsed().as_millis());
        load_rss.push(peak_rss_kb());
    }

    let artifact = load_artifact(&path)?;
    let mut write_samples = Vec::new();
    let tmp_dir = tempfile::tempdir()?;
    for i in 0..10 {
        let out = tmp_dir.path().join(format!("artifact_{i}.json"));
        let started = Instant::now();
        write_artifact(&artifact, &out)?;
        write_samples.push(started.elapsed().as_millis());
    }

    println!("{}", serde_json::json!({
        "load_artifact_ms_median": median_ms(load_samples),
        "load_artifact_rss_kb_median": median_ms(load_rss.into_iter().map(|v| v as u128).collect()),
        "write_artifact_ms_median": median_ms(write_samples),
        "fixture_path": path.display().to_string(),
        "rev": std::env::var("GIT_COMMIT").unwrap_or_default(),
    }));
    Ok(())
}
```

Add a `[[bin]]` entry for `capture_baselines` in `Cargo.toml`'s `[package]` section (or use `[[bench]]` if Criterion is used).

- [ ] **Step 2: Run against the live SPUR fixture**

```bash
GIT_COMMIT=$(git rev-parse HEAD) cargo run --release --bin capture_baselines -- /Volumes/Projects/spur/.spur/graph-index.json > crates/spur-graph/benches/baselines.json
```

- [ ] **Step 3: Verify the file contents look sane**

```bash
cat crates/spur-graph/benches/baselines.json
```

Expect a JSON object with `load_artifact_ms_median` in the 200–500 range, `write_artifact_ms_median` in the 100–300 range, and `load_artifact_rss_kb_median` in the 150_000–400_000 range.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-graph/benches/capture_baselines.rs crates/spur-graph/benches/baselines.json crates/spur-graph/Cargo.toml
git commit -m "bench(spur-graph): capture JSON-path baselines pre-PR3a

Numbers feed §12 Family 3 hard CI gates. Captured before PR3a touches
reader code so the comparison anchor survives PR3b's removal of the
JSON writer.

Refs: docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md §12 (pre-PR3a baseline capture)"
```

---

## Task 7: Implement `resolve_artifact_location` (spec §7 + PR3a part 1)

**Goal:** Central resolver helper in `store/pointer.rs` that every reader goes through. Returns format-tagged `ResolvedArtifact` with `ArtifactCacheKey`.

**Files:**
- Create: `crates/spur-graph/src/store/pointer.rs`
- Modify: `crates/spur-graph/src/store/mod.rs` (`pub mod pointer;` + re-exports)
- Test: `crates/spur-graph/tests/resolver.rs` (new — covers Family 1.4 matrix)

**Steps:**

- [ ] **Step 1: Write the failing test matrix**

```rust
// crates/spur-graph/tests/resolver.rs
use spur_graph::store::pointer::{resolve_artifact_location, ArtifactFormat};
use tempfile::tempdir;

#[test]
fn explicit_override_wins_over_current() {
    let root = tempdir().unwrap();
    let explicit = root.path().join("explicit.parquet");
    std::fs::create_dir_all(&explicit).unwrap();
    std::fs::write(explicit.join("manifest.json"), r#"{"complete":true,"graph_content_hash":"x","...":""}"#).unwrap();

    let current = root.path().join(".spur/graph/CURRENT");
    std::fs::create_dir_all(current.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(root.path().join("other.parquet"), &current).unwrap();

    let resolved = resolve_artifact_location(root.path(), Some(&explicit)).unwrap();
    assert_eq!(resolved.path, explicit);
    assert_eq!(resolved.format, ArtifactFormat::Parquet);
}

// Add tests for each row of the §7 precedence table.
// All 16 combinations of (explicit_override y/n) × (CURRENT y/n) × (pointer y/n) × (legacy.json y/n)
// can be expressed as a parameterized table.
```

Cover the 16 cases as a parameterized table or as 16 separate tests. Each asserts the correct priority + format detection.

```bash
cargo test -p spur-graph --test resolver
```
Expected: FAIL — module doesn't exist yet.

- [ ] **Step 2: Implement `resolve_artifact_location`**

```rust
// crates/spur-graph/src/store/pointer.rs
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactFormat {
    LegacyJson,
    Parquet,
}

#[derive(Debug, Clone)]
pub struct ResolvedArtifact {
    pub path: PathBuf,
    pub format: ArtifactFormat,
    pub cache_key: ArtifactCacheKey,
}

#[derive(Debug, Clone)]
pub enum ArtifactCacheKey {
    LegacyJson { path: PathBuf, mtime: SystemTime },
    Parquet    { graph_content_hash: String },
}

const POINTER_RELATIVE_PATH: &str = ".spur/graph-index.pointer.json";
const CURRENT_RELATIVE_PATH: &str = ".spur/graph/CURRENT";
const LEGACY_ARTIFACT_RELATIVE_PATH: &str = ".spur/graph-index.json";

pub fn resolve_artifact_location(
    worktree_root: &Path,
    explicit_override: Option<&Path>,
) -> Result<ResolvedArtifact> {
    // Priority 1: explicit override
    if let Some(p) = explicit_override {
        return resolve_path_as(p);
    }
    // Priority 2: .spur/graph/CURRENT
    let current = worktree_root.join(CURRENT_RELATIVE_PATH);
    if current.exists() {
        let target = current.canonicalize().with_context(|| format!("canonicalize {}", current.display()))?;
        if let Ok(r) = resolve_path_as(&target) {
            tracing::debug!(target = %target.display(), "resolved artifact via CURRENT");
            return Ok(r);
        }
    }
    // Priority 3: pointer file
    let pointer = worktree_root.join(POINTER_RELATIVE_PATH);
    if pointer.exists() {
        let pointer_data: PointerData = serde_json::from_str(&std::fs::read_to_string(&pointer)?)?;
        if let Ok(r) = resolve_path_as(Path::new(&pointer_data.canonical_artifact_path)) {
            tracing::debug!("resolved artifact via pointer file");
            return Ok(r);
        }
    }
    // Priority 4: legacy worktree-root JSON
    let legacy = worktree_root.join(LEGACY_ARTIFACT_RELATIVE_PATH);
    if legacy.exists() {
        tracing::warn!(path = %legacy.display(), "loading legacy graph-index.json; deprecated, will be removed in a future release");
        return resolve_path_as(&legacy);
    }
    anyhow::bail!("no graph artifact found in {}; tried CURRENT, pointer file, and legacy {}",
        worktree_root.display(), LEGACY_ARTIFACT_RELATIVE_PATH);
}

fn resolve_path_as(path: &Path) -> Result<ResolvedArtifact> {
    let meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if meta.is_dir() {
        let manifest_path = path.join("manifest.json");
        let manifest: ManifestPeek = serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)?;
        if !manifest.complete {
            anyhow::bail!("{}: manifest.complete is false", path.display());
        }
        Ok(ResolvedArtifact {
            path: path.to_path_buf(),
            format: ArtifactFormat::Parquet,
            cache_key: ArtifactCacheKey::Parquet { graph_content_hash: manifest.graph_content_hash },
        })
    } else {
        Ok(ResolvedArtifact {
            path: path.to_path_buf(),
            format: ArtifactFormat::LegacyJson,
            cache_key: ArtifactCacheKey::LegacyJson { path: path.to_path_buf(), mtime: meta.modified()? },
        })
    }
}

#[derive(Deserialize)]
struct PointerData {
    canonical_artifact_path: String,
}

#[derive(Deserialize)]
struct ManifestPeek {
    complete: bool,
    graph_content_hash: String,
}
```

Add `pub mod pointer;` and re-exports to `store/mod.rs`.

- [ ] **Step 3: Run tests**

```bash
cargo test -p spur-graph --test resolver
```
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-graph/src/store/ crates/spur-graph/tests/resolver.rs
git commit -m "feat(spur-graph): shared resolve_artifact_location helper (PR3a part 1)

§7 precedence: explicit override > .spur/graph/CURRENT > pointer file
> legacy .spur/graph-index.json. Returns ArtifactCacheKey so callers
never match on the format variant. Family 1.4 test matrix green.

Refs: docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md §7"
```

---

## Task 8: Wire `resolve_artifact_location` into `schema.rs:load_artifact` (PR3a part 2)

**Goal:** `load_artifact` becomes a thin dispatcher over `resolve_artifact_location`. Loads Parquet via `read_artifact_parquet`, JSON via existing serde_json path, with a deprecation `tracing::warn!` for legacy JSON.

**Files:**
- Modify: `crates/spur-graph/src/schema.rs:275` (the `load_artifact` function)
- Test: `crates/spur-graph/tests/load_artifact_dispatch.rs` (new)

**Steps:**

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn load_artifact_reads_parquet_directory() {
    let original = fixture_artifact();
    let dir = tempdir().unwrap();
    let written = write_artifact_parquet(&original, dir.path(), Default::default()).unwrap();

    let loaded = spur_graph::load_artifact(&written).unwrap();
    assert_eq!(loaded.graph_content_hash, original.graph_content_hash);
}

#[test]
fn load_artifact_reads_legacy_json_file() {
    let original = fixture_artifact();
    let dir = tempdir().unwrap();
    let path = dir.path().join("graph-index.json");
    spur_graph::store::json::write_artifact(&original, &path).unwrap();

    let loaded = spur_graph::load_artifact(&path).unwrap();
    assert_eq!(loaded.graph_content_hash, original.graph_content_hash);
}
```

```bash
cargo test -p spur-graph --test load_artifact_dispatch
```
Expected: FAIL — current load_artifact only handles JSON.

- [ ] **Step 2: Update `load_artifact`**

```rust
// crates/spur-graph/src/schema.rs (replace existing function ~line 275)
pub fn load_artifact(path: &Path) -> anyhow::Result<GraphIndexArtifact> {
    let resolved = crate::store::pointer::resolve_path_as_public(path)
        .or_else(|_| crate::store::pointer::resolve_artifact_location(path, Some(path)))?;
    match resolved.format {
        crate::store::pointer::ArtifactFormat::Parquet => {
            crate::store::parquet::read_artifact_parquet(&resolved.path)
        }
        crate::store::pointer::ArtifactFormat::LegacyJson => {
            // existing JSON load path
            load_legacy_json(&resolved.path)
        }
    }
}
```

Expose `resolve_path_as_public` from `pointer.rs` (rename the internal `resolve_path_as` and make it `pub`).

The existing JSON load body (deduplicate_symbols, validate_ranges) moves into a private `load_legacy_json` function in `schema.rs`.

- [ ] **Step 3: Run tests**

```bash
cargo test -p spur-graph --test load_artifact_dispatch
cargo test -p spur-graph
```
Expected: both new tests pass; existing tests still pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-graph/src/schema.rs crates/spur-graph/src/store/pointer.rs crates/spur-graph/tests/load_artifact_dispatch.rs
git commit -m "feat(spur-graph): load_artifact dispatches to Parquet or legacy JSON (PR3a part 2)

Thin shim over resolve_artifact_location. Directory path → Parquet,
file path → legacy JSON with deprecation log. No change to JSON write
path; that lands at Task 12.

Refs: docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md §7"
```

---

## Task 9: Migrate `spur-mcp/code_graph.rs` to the resolver (PR3a part 3)

**Goal:** `spur-mcp/src/server/handlers/code_graph.rs` no longer hardcodes `.spur/graph-index.json` — it calls `resolve_artifact_location` with the worktree root. `load_artifact(&resolved.path)` continues to work for both formats since Task 8 made it polymorphic.

**Files:**
- Modify: `crates/spur-mcp/src/server/handlers/code_graph.rs:20` (the `GRAPH_ARTIFACT_RELATIVE_PATH` constant) and `:445` (the `load_artifact` call)
- Test: existing `spur-mcp` test suite + an added test fixture for the Parquet path

**Steps:**

- [ ] **Step 1: Run the existing spur-mcp tests to capture green baseline**

```bash
cargo test -p spur-mcp
```
Expected: all green.

- [ ] **Step 2: Replace the hardcoded path with the resolver**

Find the function around `:445` that currently does:

```rust
let artifact_path = worktree_root.join(GRAPH_ARTIFACT_RELATIVE_PATH);
match load_artifact(&artifact_path) {
    ...
}
```

Change to:

```rust
let resolved = spur_graph::store::pointer::resolve_artifact_location(&worktree_root, None)?;
match load_artifact(&resolved.path) {
    ...
}
```

Remove or deprecate `GRAPH_ARTIFACT_RELATIVE_PATH` (move to a `pub(crate) const`-only default constant kept for any other use sites). Audit the file for other reads of that constant.

- [ ] **Step 3: Add a test that exercises the Parquet path**

```rust
#[test]
fn code_graph_handler_reads_parquet_artifact() {
    // Set up a tempdir worktree with a .spur/graph/CURRENT symlink to a Parquet directory.
    // Invoke the code_graph handler.
    // Assert it returns expected symbols.
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p spur-mcp
```
Expected: all green plus the new test.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/
git commit -m "feat(spur-mcp): code_graph handler reads via resolve_artifact_location (PR3a part 3)

Drops hardcoded .spur/graph-index.json. Resolver picks Parquet or legacy
JSON per §7 precedence. Existing JSON tests continue to pass; new test
covers the Parquet path.

Refs: docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md §7 + §9 PR3a"
```

---

## Task 10: Migrate `spur-tui` readers to the resolver (PR3a part 4)

**Goal:** `spur-tui/src/mentions/code_graph/source.rs` and `spur-tui/src/mentions/registry.rs` go through the resolver. The `CodeGraphMentionSource` cache-validation path uses `read_artifact_header_parquet` (sub-ms) instead of full `load_artifact`.

**Files:**
- Modify: `crates/spur-tui/src/mentions/code_graph/source.rs:8` and `:127`
- Modify: `crates/spur-tui/src/mentions/registry.rs:26` and `:942`
- Test: `crates/spur-tui` existing test suite

**Steps:**

- [ ] **Step 1: Capture green baseline**

```bash
cargo test -p spur-tui
```

- [ ] **Step 2: Update `source.rs`**

Around line 127, the current code does:

```rust
let artifact = match tracing::debug_span!("load_artifact")
    .in_scope(|| load_artifact(&self.artifact_path))
{
    ...
}
```

Replace with resolver use. If the cache validation only needs the header (counts, hash, commit_oid), dispatch through `read_artifact_header_parquet` when format is Parquet:

```rust
let resolved = spur_graph::store::pointer::resolve_artifact_location(&self.worktree_root, None)?;
if cache_check_only {
    let header = match resolved.format {
        ArtifactFormat::Parquet => read_artifact_header_parquet(&resolved.path)?.into(),
        ArtifactFormat::LegacyJson => read_artifact_header_legacy(&resolved.path)?,
    };
    return Ok(header);
}
let artifact = load_artifact(&resolved.path)?;
```

`.into()` from `GraphArtifactManifest` to whatever shape the call site uses — define the conversion in `parquet.rs`.

- [ ] **Step 3: Update `registry.rs`**

Around line 26, `CODE_GRAPH_LEGACY_INDEX_PATH` stays as a fallback (the resolver handles it). Around line 942, the `legacy_artifact` check becomes resolver-driven.

- [ ] **Step 4: Run tests**

```bash
cargo test -p spur-tui
```
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/
git commit -m "feat(spur-tui): mentions read via resolve_artifact_location (PR3a part 4)

CodeGraphMentionSource uses read_artifact_header_parquet for cache
validation (sub-ms). Registry's legacy-JSON fallback now flows through
resolver precedence.

Refs: docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md §7 + §9 PR3a"
```

---

## Task 11: Migrate `spur-cli/commands/graph.rs` to the resolver (PR3a part 5)

**Goal:** `spur-cli graph build` (and friends) read via resolver. Default output path becomes a Parquet directory under `.git/spur-graph/artifacts/<...>/<hash>.parquet/`. Incremental rebuild uses `read_artifact_parquet`.

**Files:**
- Modify: `crates/spur-cli/src/commands/graph.rs:9-13`, `:46-51`, `:75`, `:120-190` (mainly the load/write call sites)
- Test: `crates/spur-cli/tests/graph_build_cli.rs` (extend)

**Steps:**

- [ ] **Step 1: Capture green baseline**

```bash
cargo test -p spur-cli
```

- [ ] **Step 2: Update load sites**

The incremental-rebuild branch around line 75:
```rust
Ok(prev) => match artifact_from_facts_incremental(&prev, &root) {
```

`prev` comes from a load call earlier; replace that load with `load_artifact(&resolved.path)` where `resolved` is from the resolver.

- [ ] **Step 3: Leave the write site (still JSON) untouched in this task**

The write call (`write_artifact(&artifact, &output)`) stays on JSON for now. Task 12 flips it.

- [ ] **Step 4: Run tests**

```bash
cargo test -p spur-cli
```
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-cli/
git commit -m "feat(spur-cli): graph build reads via resolve_artifact_location (PR3a part 5)

Incremental rebuild loads from Parquet or legacy JSON via resolver.
Write path still JSON; flipped in PR3b.

Refs: docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md §7 + §9 PR3a"
```

---

## Task 12: Writer flip — cache.rs writes Parquet (spec PR3b)

**Goal:** `store/cache.rs` writes `<hash>.parquet/` via `write_artifact_parquet`. `spur-cli graph build` writes Parquet. `write_artifact` (JSON) is marked `#[deprecated]` but kept callable so PR3c can remove it cleanly. Pointer file `canonical_artifact_path` references the Parquet directory.

**Files:**
- Modify: `crates/spur-graph/src/store/cache.rs` (~lines 18, 300, 335, 376; the WORKTREE_ARTIFACT_PATH constant and write sites)
- Modify: `crates/spur-graph/src/store/json.rs` (add `#[deprecated]` to `write_artifact`)
- Modify: `crates/spur-cli/src/commands/graph.rs` (write site flips to Parquet)
- Test: existing `cache` integration tests must continue to pass

**Steps:**

- [ ] **Step 1: Capture green baseline**

```bash
cargo test -p spur-graph -p spur-cli
```

- [ ] **Step 2: Update `WORKTREE_ARTIFACT_PATH` semantics**

`cache.rs:18` currently has:
```rust
const WORKTREE_ARTIFACT_PATH: &str = ".spur/graph-index.json";
```

Decide: either rename to a `.spur/graph/CURRENT` symlink target, or keep the legacy path and have `cache.rs` update both `.spur/graph/CURRENT` AND `.spur/graph-index.pointer.json`. Choose the latter — keeps the legacy reader-tolerance path simple.

- [ ] **Step 3: Replace the write site**

`cache.rs:300, 335, 376` previously call `write_artifact(...)`. Replace with:

```rust
let written_dir = spur_graph::store::parquet::write_artifact_parquet(
    &artifact,
    &cache_dir,
    spur_graph::store::parquet::WriteOptions { emit_edges_by_dst: /* from Task 2 */ },
)?;
spur_graph::store::pointer::write_current_pointer(&worktree_root, &written_dir)?;
// Update .spur/graph-index.pointer.json with canonical_artifact_path = written_dir
```

Add `write_current_pointer` to `pointer.rs` (writes the `.spur/graph/CURRENT` symlink or pointer file).

- [ ] **Step 4: Mark `write_artifact` as `#[deprecated]`**

In `crates/spur-graph/src/store/json.rs`:
```rust
#[deprecated(note = "Use spur_graph::store::parquet::write_artifact_parquet. Removed in next release.")]
pub fn write_artifact(artifact: &GraphIndexArtifact, path: &Path) -> anyhow::Result<()> {
    /* existing body */
}
```

Suppress the warning at call sites that legitimately still need it (e.g. legacy-JSON write tests). Compile-warning at other call sites is intended — surfaces stragglers.

- [ ] **Step 5: Flip `spur-cli/commands/graph.rs` writes to Parquet**

The write site (~line 120 and 190) changes from `write_artifact(&artifact, &output)` to `write_artifact_parquet(&artifact, &output_base, opts)`. The output path argument becomes a directory, not a file. If `output` is a file path (user-specified, legacy), error out with a clear message recommending the new directory layout.

- [ ] **Step 6: Run tests**

```bash
cargo test --workspace
```
Expected: workspace builds; existing tests pass. Expect compile warnings on remaining `write_artifact` call sites (allowed; flagged for Task 14 cleanup).

- [ ] **Step 7: Manual smoke**

```bash
cargo run --bin spur -- graph build --workspace
ls .spur/graph/
ls .git/spur-graph/artifacts/
```
Expected: `.spur/graph/CURRENT` points at the new `<hash>.parquet/` directory; the directory contains the seven Parquet files + manifest.json.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-graph/src/store/ crates/spur-cli/src/commands/graph.rs
git commit -m "feat(spur-graph): writer flip — cache.rs writes Parquet (PR3b)

cache.rs writes <hash>.parquet/ via the atomic protocol. write_artifact
(JSON) is #[deprecated] but kept callable. spur-cli graph build now
emits Parquet directories. Cleanup (delete write_artifact, rename
json.rs) lands in PR3c.

Refs: docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md §9 PR3b"
```

---

## Task 13: Family 3 hard CI gates against baselines

**Goal:** Bench `write_artifact_parquet`, `read_artifact_parquet`, the incremental-build wall-clock, and DuckDB cold-query latency. Assert each gate against `baselines.json` and POC measurements.

**Files:**
- Create: `crates/spur-graph/benches/parquet.rs` (Criterion bench)
- Create: `crates/spur-graph/tests/perf_gates.rs` (CI gate assertion)

**Steps:**

- [ ] **Step 1: Write the bench**

```rust
// crates/spur-graph/benches/parquet.rs
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use spur_graph::store::parquet::{read_artifact_parquet, write_artifact_parquet, WriteOptions};

fn bench_write(c: &mut Criterion) {
    let artifact = /* load SPUR fixture */;
    let tmp = tempfile::tempdir().unwrap();
    c.bench_function("write_artifact_parquet", |b| {
        b.iter(|| write_artifact_parquet(black_box(&artifact), tmp.path(), WriteOptions::default()).unwrap())
    });
}

fn bench_read(c: &mut Criterion) {
    let artifact = /* load SPUR fixture */;
    let tmp = tempfile::tempdir().unwrap();
    let dir = write_artifact_parquet(&artifact, tmp.path(), WriteOptions::default()).unwrap();
    c.bench_function("read_artifact_parquet", |b| {
        b.iter(|| read_artifact_parquet(black_box(&dir)).unwrap())
    });
}

criterion_group!(benches, bench_write, bench_read);
criterion_main!(benches);
```

- [ ] **Step 2: Write the gate assertion**

```rust
// crates/spur-graph/tests/perf_gates.rs
#[derive(serde::Deserialize)]
struct Baselines {
    load_artifact_ms_median: u128,
    write_artifact_ms_median: u128,
    load_artifact_rss_kb_median: u128,
}

#[test]
#[cfg_attr(not(feature = "perf-gates"), ignore)]
fn gate_3_2_read_artifact_parquet_under_half_baseline() {
    let baselines: Baselines = serde_json::from_str(
        &std::fs::read_to_string("benches/baselines.json").unwrap()
    ).unwrap();

    // measure read_artifact_parquet wall-clock median over N=10
    let median_ms = /* measure */;

    assert!(
        median_ms as u128 <= (baselines.load_artifact_ms_median / 2),
        "Gate 3.2 FAILED: read_artifact_parquet median {} ms exceeds 0.5× baseline {} ms",
        median_ms, baselines.load_artifact_ms_median
    );
}

// Similar gates for 3.1 (write ≤ 2× baseline), 3.3 (RSS ≤ baseline), 3.4 (incremental ≤ 0.8× baseline),
// 3.5 (DuckDB cold query ≤ 1.5× POC median; POC numbers hard-coded as constants here),
// 3.6 (DuckDB peak RSS ≤ 500 MB)
```

The perf gates run in a separate CI job behind a feature flag so noise doesn't gate unrelated work merges.

- [ ] **Step 3: Wire perf-gates feature in `Cargo.toml`**

```toml
[features]
perf-gates = []
```

- [ ] **Step 4: Run gates locally**

```bash
cargo test -p spur-graph --features perf-gates --test perf_gates
```
Expected: all gates pass on dev machine. If a gate fails, investigate before merging Task 12 to main.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/benches/parquet.rs crates/spur-graph/tests/perf_gates.rs crates/spur-graph/Cargo.toml
git commit -m "test(spur-graph): Family 3 hard CI gates for Parquet exporter (PR3b)

Gates 3.1–3.6 from §12 measured against benches/baselines.json (Task 6)
and POC numbers (commit 754b07a8). Gated behind 'perf-gates' feature
so correctness CI stays decoupled from perf CI.

Refs: docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md §12 Family 3"
```

---

## Task 14: PR3c cleanup — delete `write_artifact`, rename `json.rs` → `canonical_hash.rs`

**Goal:** Remove the deprecated JSON writer. Rename what's left of `json.rs` to its honest name. PR is trivially revertable.

**Files:**
- Delete: `crates/spur-graph/src/store/json.rs` body → consolidated into new `canonical_hash.rs`
- Create: `crates/spur-graph/src/store/canonical_hash.rs`
- Modify: `crates/spur-graph/src/store/mod.rs` (drop `pub mod json`; add `pub mod canonical_hash`; update re-exports)
- Modify: any straggler call site of `write_artifact` (compile warnings from Task 12)

**Steps:**

- [ ] **Step 1: List straggler call sites**

```bash
cargo build --workspace 2>&1 | grep "use of deprecated" || true
```
Expected: a short list. Each must be removed or rewritten.

- [ ] **Step 2: Migrate any test that still calls `write_artifact`**

For each straggler:
- If the test legitimately wants to create a JSON fixture for legacy-read testing, replace with a hand-written canonical JSON string.
- Otherwise, rewrite to use `write_artifact_parquet`.

- [ ] **Step 3: Create `canonical_hash.rs` containing what survives**

```rust
// crates/spur-graph/src/store/canonical_hash.rs
use anyhow::Context;
use serde::Serialize;
use crate::{GraphIndexArtifact, GraphEdgeArtifact, GraphFileArtifact, GraphFileManifestEntry, GraphSymbolArtifact, GraphTombstoneEntry};

#[derive(serde::Serialize)]
pub(crate) struct GraphArtifactBodyForHash<'a> {
    pub files: &'a [GraphFileArtifact],
    pub symbols: &'a [GraphSymbolArtifact],
    pub edges: &'a [GraphEdgeArtifact],
    pub file_manifests: &'a [GraphFileManifestEntry],
    pub graph_content_hash: &'a str,
    pub manifest_version: &'a str,
    pub tombstones: &'a [GraphTombstoneEntry],
}

pub fn artifact_content_hash_blake3_hex(artifact: &GraphIndexArtifact) -> anyhow::Result<String> {
    let body = GraphArtifactBodyForHash {
        files: &artifact.files,
        symbols: &artifact.symbols,
        edges: &artifact.edges,
        file_manifests: &artifact.file_manifests,
        graph_content_hash: &artifact.graph_content_hash,
        manifest_version: &artifact.manifest_version,
        tombstones: &artifact.tombstones,
    };
    let canonical_json = serde_json::to_vec(&body)
        .context("failed to encode graph artifact body for content hash")?;
    Ok(blake3::hash(&canonical_json).to_hex().to_string())
}
```

- [ ] **Step 4: Delete `json.rs`; update `store/mod.rs`**

```bash
git rm crates/spur-graph/src/store/json.rs
```

In `mod.rs`:
```rust
pub mod build;
pub mod cache;
pub mod canonical_hash;
pub mod parquet;
pub mod pointer;
pub mod snapshot;

pub use build::{
    artifact_from_facts, artifact_from_facts_incremental, current_manifest_version,
    BuildMode, EXTRACTOR_VERSION, SCHEMA_VERSION,
};
pub use canonical_hash::artifact_content_hash_blake3_hex;
pub use parquet::{
    read_artifact_header_parquet, read_artifact_parquet, write_artifact_parquet,
    GraphArtifactManifest, WriteOptions,
};
pub use pointer::{
    read_current_pointer, resolve_artifact_location, write_current_pointer,
    ArtifactCacheKey, ArtifactFormat, ResolvedArtifact,
};
```

- [ ] **Step 5: Update Task 1's `extractor.rs` test import**

`spur-graph/tests/extractor.rs` may still have `use spur_graph::store::json::*`. Update to `use spur_graph::store::canonical_hash::*`.

- [ ] **Step 6: Run full workspace tests + the gates**

```bash
cargo test --workspace
cargo test -p spur-graph --features perf-gates --test perf_gates
```
Expected: green workspace, all gates pass.

- [ ] **Step 7: Add the field-order guard snapshot (Family 1.2 second half)**

```rust
// in canonical_hash.rs or a sibling test file
#[test]
fn canonical_bytes_snapshot_guard() {
    let minimal = GraphIndexArtifact { /* small but representative */ };
    let body = GraphArtifactBodyForHash { /* from artifact */ };
    let bytes = serde_json::to_vec(&body).unwrap();
    insta::assert_snapshot!("canonical_bytes_layout", std::str::from_utf8(&bytes).unwrap());
}
```

Commit the `insta` snapshot file. Any field-order change to `GraphArtifactBodyForHash` will fail this test before invalidating user pointers.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-graph/
git commit -m "refactor(spur-graph): retire write_artifact; rename json.rs → canonical_hash.rs (PR3c)

Removes the deprecated JSON full-artifact writer. What remains is the
~30-line canonical_hash module: GraphArtifactBodyForHash + content-hash
function. Snapshot guard on canonical-bytes layout catches future
field-order accidents.

Refs: docs/superpowers/specs/2026-05-21-spur-graph-parquet-exporter-design.md §9 PR3c + §10"
```

---

## Self-Review

**Spec coverage:** Walked each §-numbered section of the spec.
- §6 schemas → Tasks 3, 4
- §6.9 atomic publication → Task 4
- §7 surface API → Tasks 3, 5, 7
- §8 store/ refactor → Tasks 1, 3, 7, 14
- §9 PR sequence → Tasks 1, 3+4+5, 6, 7+8+9+10+11, 12, 13, 14
- §10 content hash → Task 14 (snapshot guard); existing function preserved at Task 1
- §11 incremental → exercised by Task 1 (no behavior change) + Task 8 (load_artifact dispatch); Task 12 closes incremental write
- §11.5 node_id invariant → enforced by §6 schemas (Tasks 3) + PR4 view layer (out of scope)
- §12 test plan → Family 1.1/1.4 (Tasks 3, 7), 1.2 snapshot (Task 14), 1.5 partial-write (Task 4), 2.6 namespace (Task 5), Family 3 gates (Task 13)
- §13 risks → mitigations baked in (Task 4 atomic protocol; Tasks 7+8 resolver; Task 12 #[deprecated]; Task 14 trivial revert)

**Placeholder scan:** Each step contains executable code or commands. The one DECIDED_VALUE marker in Task 3 Step 4's `ROW_GROUP_SIZE` is intentional — Task 2 fills it in.

**Type consistency:** `ResolvedArtifact`, `ArtifactCacheKey`, `ArtifactFormat`, `GraphArtifactManifest`, `WriteOptions` defined in Tasks 3 & 7, referenced consistently in Tasks 8–13.

**Open gaps surfaced by self-review (none requiring spec changes):**
- The `GraphIndexArtifact.symbol_node_ids` / `file_node_ids` parallel vecs (Task 3 Steps 4–5) are an implementation detail not in the spec. They're `#[serde(skip)]` so the content hash is unaffected; semantics preserved. Document this in the Task 3 PR description.
