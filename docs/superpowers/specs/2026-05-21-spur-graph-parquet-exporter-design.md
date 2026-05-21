# spur-graph Parquet Exporter — Design Spec

**Date:** 2026-05-21
**Status:** Approved (brainstorming)
**Tracking issue:** bd-1rqxk
**Companion POC:** `crates/spur-context/poc/duckdb-analyst/` (commit 754b07a8)

## 1. Summary

Migrate `spur-graph`'s persisted artifact from a single multi-megabyte JSON file to a directory of Apache Parquet files written via pure-Rust `arrow-rs` + `parquet` crates. JSON `write_artifact` is deleted in the same change. Canonical-JSON content hashing stays in memory only. The result: 8–14× smaller on disk, 5–8× faster cold load, ~30× lower peak RSS, and the load-time OOM that the POC observed under `read_json` + `UNNEST` disappears. The output is directly consumable by DuckDB + DuckPGQ + Onager without any libduckdb linkage anywhere in SPUR's compiled crates.

## 2. Goals

- Write the graph index as Parquet (`nodes`, `edges`, `edges_unresolved`, `files`, `file_manifests`, `tombstones`) plus a small `manifest.json`.
- Delete the JSON full-artifact writer (`write_artifact`) at cutover.
- Keep `artifact_content_hash_blake3_hex` byte-identical to today so existing pointer files in checked-out worktrees remain valid.
- Preserve the incremental-build flow (`artifact_from_facts_incremental`) unchanged.
- Enable the spur-context DuckDB MCP capability (bd-1rqxk acceptance criteria 2–6) without modification to the data layer.
- Leave `spur-graph` free of any `libduckdb` linkage.

## 3. Non-goals

- Arrow IPC (Feather v2) dual-write for mmap zero-copy reads. Promote only when a measured in-process Rust reader pins load latency.
- Apache GraphAr directory/metadata convention. Promote when a concrete external consumer (Kùzu, Spark, GraphScope) benefits.
- Predicate-pushdown partial reads in the incremental build path. Tracked as future optimization B (see §11.4).
- Switching the content-hash basis to Parquet bytes. Stays canonical-JSON-in-memory.
- Changing the `.spur/graph-index.pointer.json` schema in a backwards-incompatible way.

## 4. Background

`spur-graph`'s persisted artifact today is a single JSON document at `.git/spur-graph/artifacts/<manifest_version>/<hash>.json`, surfaced via `.spur/graph-index.pointer.json`. At SPUR's current scale (27.5k symbols, 47k resolved edges, 1.5k files) the artifact is ~42 MB. The May 2026 POC (`crates/spur-context/poc/duckdb-analyst/`) demonstrated that DuckDB ingestion of this JSON via `read_json` + `UNNEST` materializes the entire document as a single struct-laden row, and any window function over the unnest blows past 5 GB peak RSS. The POC worked around this with a streaming view and a split node-id mapping, but the underlying problem is structural: JSON-on-disk for a graph artifact does not scale.

Projected wins from switching to Parquet (ZSTD-3, dictionary-encoded high-redundancy columns) on SPUR-shaped data:

| Dimension | JSON today | Parquet (projected) |
|---|---|---|
| On-disk size | 42 MB | 3–6 MB (8–14× smaller) |
| Cold load to in-memory artifact | 250–500 ms | 50–80 ms (5–8× faster) |
| Peak RSS during load | 5 GB+ (OOM under UNNEST window) | 150–250 MB (~30× lower) |
| Column projection | impossible | inherent — read only columns needed |
| `WHERE src_id = X` ("callers of X") | full scan | row-group pruning to ~one group |

At 10× SPUR's current size, JSON becomes unusable on developer hardware; Parquet stays flat. This is the asymptotic argument; the constant-factor speedup is a bonus.

## 5. On-disk layout

```
.git/spur-graph/artifacts/<manifest_version>/<hash>.parquet/
  nodes.parquet
  edges.parquet
  edges_unresolved.parquet
  files.parquet
  file_manifests.parquet
  tombstones.parquet
  manifest.json

.spur/graph/CURRENT         (symlink or pointer file → the latest <hash>.parquet/ dir)
.spur/graph-index.pointer.json  (existing pointer schema, with canonical_artifact_path
                                 now ending in `.parquet/` instead of `.json`)
```

**Rationale.** Bytes live in git internals (`.git/spur-graph/artifacts/...`), where `store/cache.rs` already content-addresses and GC's them. `.spur/graph/CURRENT` exposes a stable, discoverable path that the DuckDB MCP server and any ad-hoc `duckdb` invocation can read without knowing about git layout. One source of truth (the git-internal directory), one user-facing handle (`CURRENT`).

The pointer file (`.spur/graph-index.pointer.json`) keeps its existing schema. Its `canonical_artifact_path` field changes from a `.json` file path to a `.parquet/` directory path. The field name is preserved for forward compatibility; "path" can be a directory.

## 6. Parquet schemas

All files: ZSTD level 3, row group size 64 K rows.

### 6.1 `nodes.parquet`

| Column | Arrow type | Encoding | Nullable | Notes |
|---|---|---|---|---|
| `id` | Utf8 | PLAIN | no | `stable_symbol_id` (hex VARCHAR, ~16 chars) |
| `node_id` | Int64 | PLAIN | no | Dense BIGINT assigned at export — `enumerate()` over the sorted symbol list |
| `qualified_name` | Utf8 | DICTIONARY | no |  |
| `entity_name` | Utf8 | DICTIONARY | yes |  |
| `kind` | Utf8 | DICTIONARY | no | NodeKind discriminator, ~15 distinct values |
| `file_path` | Utf8 | DICTIONARY | no | ~1.5k distinct vs. 27k rows; high dictionary win |
| `line_start` | Int32 | PLAIN | yes |  |
| `line_end` | Int32 | PLAIN | yes |  |
| `enclosing_scope` | Utf8 | DICTIONARY | yes |  |
| `anchor_hash` | Utf8 | PLAIN | yes | Preserved from `GraphSymbolArtifact` for round-trip identity |
| `byte_range_start` | Int64 | PLAIN | yes | Preserved from `GraphSymbolArtifact.byte_range[0]` |
| `byte_range_end` | Int64 | PLAIN | yes | Preserved from `GraphSymbolArtifact.byte_range[1]` |

**Sort key: `(file_path ASC, id ASC)`.** Enables future predicate-pushdown incremental loads keyed on changed files (deferred optimization B). Does not change the v1 read path; the full-artifact reader iterates sequentially either way.

### 6.2 `edges.parquet`

| Column | Arrow type | Encoding | Nullable | Notes |
|---|---|---|---|---|
| `src` | Utf8 | DICTIONARY | no | Matches `nodes.id` |
| `dst` | Utf8 | DICTIONARY | no |  |
| `src_id` | Int64 | PLAIN | no | Resolved at export — `nodes.id → node_id` lookup |
| `dst_id` | Int64 | PLAIN | no |  |
| `target_label` | Utf8 | DICTIONARY | yes | `GraphEdgeArtifact.target_label` |
| `kind` | Utf8 | DICTIONARY | no | RelationKind, ~10 distinct values |
| `confidence` | Utf8 | DICTIONARY | no | ~3 distinct values |
| `confidence_score` | Float64 | PLAIN | yes |  |

**Sort key: `(src_id ASC, dst_id ASC)`.** Columnar analogue of CSR. Row-group statistics let DuckDB skip row groups for `WHERE src_id = X` ("callers of X").

### 6.3 `edges_unresolved.parquet`

Only those edges where the target did not resolve to a known symbol (~59% of edges in today's SPUR graph — dynamic dispatch, macro bodies, HOF arguments).

| Column | Arrow type | Encoding | Nullable |
|---|---|---|---|
| `src` | Utf8 | DICTIONARY | no |
| `src_id` | Int64 | PLAIN | no |
| `target_label` | Utf8 | DICTIONARY | yes |
| `kind` | Utf8 | DICTIONARY | no |
| `confidence` | Utf8 | DICTIONARY | no |
| `confidence_score` | Float64 | PLAIN | yes |

Sort key: `src_id ASC`.

### 6.4 `files.parquet`

| Column | Arrow type | Encoding | Nullable |
|---|---|---|---|
| `file_id` | Utf8 | PLAIN | no |
| `file_path` | Utf8 | DICTIONARY | no |

Sort key: `file_path ASC`.

### 6.5 `file_manifests.parquet`

| Column | Arrow type | Encoding | Nullable | Notes |
|---|---|---|---|---|
| `stable_file_id` | Utf8 | PLAIN | no |  |
| `path` | Utf8 | DICTIONARY | no |  |
| `content_oid` | Utf8 | PLAIN | yes | git blob OID |
| `node_ids` | List(Int64) | PLAIN | no | Per-file node ids — used eagerly by `artifact_from_facts_incremental` |

`node_ids` is stored eagerly as `LIST<INT64>` (not derived via JOIN) to keep `read_artifact_parquet` a straightforward round-trip producing a byte-equivalent `GraphIndexArtifact`. The few-KB redundancy cost is dwarfed by the simplicity win.

### 6.6 `tombstones.parquet`

Mirrors `GraphTombstoneEntry { path: String, stable_file_id: String }`.

| Column | Arrow type | Encoding | Nullable |
|---|---|---|---|
| `path` | Utf8 | DICTIONARY | no |
| `stable_file_id` | Utf8 | PLAIN | no |

Sort key: `path ASC`.

### 6.7 `manifest.json`

Small JSON metadata file inside each `<hash>.parquet/` directory:

```json
{
  "graph_index_version": "spur-graph-phase2",
  "schema_version": "spur-graph-schema-v5",
  "manifest_version": "<sha256 of query bytes>",
  "graph_content_hash": "<blake3 hex>",
  "indexed_commit_oid": "<git oid or null>",
  "extractor_version": "<EXTRACTOR_VERSION>",
  "row_counts": {
    "nodes": 27559,
    "edges": 47169,
    "edges_unresolved": 68070,
    "files": 1548,
    "file_manifests": 1548,
    "tombstones": 0
  },
  "parquet_writer": {
    "compression": "zstd-3",
    "row_group_size": 65536
  }
}
```

JSON is fine here because the file is small (a few hundred bytes), not the artifact body. The pointer file (`.spur/graph-index.pointer.json`) and this manifest are the only JSON that survives the cutover.

## 7. Surface API

New public API on `spur_graph` (re-exported from `store/parquet.rs`):

```rust
/// Writes the artifact as a Parquet directory inside `base_dir`. The directory
/// name is the graph_content_hash. Returns the absolute path written.
pub fn write_artifact_parquet(
    artifact: &GraphIndexArtifact,
    base_dir: &Path,
) -> anyhow::Result<PathBuf>;

/// Reads a Parquet directory back into a GraphIndexArtifact byte-equivalent
/// to the one that was written.
pub fn read_artifact_parquet(dir: &Path) -> anyhow::Result<GraphIndexArtifact>;

/// Updates the .spur/graph/CURRENT pointer to point at the given hash directory.
pub fn write_current_pointer(worktree_root: &Path, hash_dir: &Path) -> anyhow::Result<()>;

/// Reads .spur/graph/CURRENT and returns the absolute path of the live
/// Parquet directory.
pub fn read_current_pointer(worktree_root: &Path) -> anyhow::Result<PathBuf>;
```

The existing `load_artifact(path: &Path)` function in `schema.rs` is updated to detect whether `path` is a `.json` file (legacy, returns an error post-cutover) or a `.parquet/` directory (delegates to `read_artifact_parquet`). All call sites pass directory paths after PR3 (§9).

`write_artifact` (the JSON full-artifact writer) is removed.

## 8. `store/` module refactor

```
crates/spur-graph/src/store/
├── mod.rs            ─ re-exports
├── build.rs          ─ NEW. Receives artifact_from_facts, artifact_from_facts_incremental,
│                       buckets_from_facts, compose_artifact, anchor_hash,
│                       qualified_name, and the ~1700 lines of construction logic
│                       currently misfiled in json.rs.
├── canonical_hash.rs ─ NEW (renamed from json.rs at end of cutover). ~30 lines:
│                       GraphArtifactBodyForHash + artifact_content_hash_blake3_hex.
├── pointer.rs        ─ NEW. Small JSON metadata I/O for .spur/graph-index.pointer.json
│                       and .spur/graph/CURRENT.
├── parquet.rs        ─ NEW. write_artifact_parquet, read_artifact_parquet,
│                       schemas, RecordBatch builders.
├── cache.rs          ─ MODIFIED. Writes Parquet directory instead of JSON file.
│                       Dedup by content hash unchanged.
└── snapshot.rs       ─ unchanged.
```

`mod.rs` `pub use` re-exports preserve public symbols where possible: `artifact_from_facts`, `artifact_from_facts_incremental`, `BuildMode`, `EXTRACTOR_VERSION`, `SCHEMA_VERSION`, `current_manifest_version`. Symbols that no longer exist (`write_artifact`) are removed and every caller is migrated in the same change-set.

Cargo:

```toml
[features]
default = ["parquet"]
parquet = ["dep:parquet", "dep:arrow-array", "dep:arrow-schema"]

[dependencies]
parquet      = { workspace = true, optional = true, default-features = false, features = ["zstd"] }
arrow-array  = { workspace = true, optional = true }
arrow-schema = { workspace = true, optional = true }
```

All three are pure Rust. Workspace-pinned. No native linkage; no libduckdb anywhere. Default-on so `cargo build` works as today; `--no-default-features` is an escape hatch for environments that explicitly want neither writer (rare).

## 9. Migration sequence

bd-1rqxk becomes the parent epic. Each PR is independently mergeable.

### PR1 — Split `store/json.rs` (mechanical)
Move ~1700 lines of construction logic from `json.rs` into `build.rs`. `mod.rs` re-exports unchanged. Zero behavior delta. `cargo check -p spur-graph -p spur-cli -p spur-tui -p spur-mcp` passes without changes elsewhere.

### PR2 — Add `store/parquet.rs` behind a Cargo feature
Implement `write_artifact_parquet`, `read_artifact_parquet`, schemas, RecordBatch builders. Add `arrow-array`, `arrow-schema`, `parquet` to workspace dependencies. Default-on `parquet` feature. No caller changes. Round-trip identity test for the writer/reader pair (Family 1.1, §12). At this point both JSON and Parquet writers coexist; only the round-trip test exercises Parquet.

### PR3 — Cutover (the hard PR)
Migrate every reader off JSON `load_artifact("<...>.json")` onto Parquet:

| Crate | File | Change |
|---|---|---|
| `spur-graph` | `schema.rs` | `load_artifact` detects directory vs. file; delegates to `read_artifact_parquet` |
| `spur-graph` | `store/cache.rs` | Write `.parquet/` directory; update WORKTREE_ARTIFACT_PATH semantics |
| `spur-mcp` | `server/handlers/code_graph.rs` | Update GRAPH_ARTIFACT_RELATIVE_PATH; calls go via the new pointer-resolved path |
| `spur-tui` | `mentions/code_graph/source.rs` | Same |
| `spur-tui` | `mentions/registry.rs` | Update legacy-path constant; keep fallback that reads old JSON for one release cycle if found, with a deprecation warning |
| `spur-cli` | `commands/graph.rs` | `graph build` writes Parquet; incremental load reads Parquet; remove `write_artifact` import |

Pointer file `canonical_artifact_path` now references a `.parquet/` directory. Delete `write_artifact` from `json.rs`. Rename `json.rs` → `canonical_hash.rs`. Move pointer I/O into the new `pointer.rs`.

Test surface:
- All existing `spur-graph`, `spur-mcp`, `spur-tui`, `spur-cli` test suites pass with their fixtures re-emitted as Parquet (or via temporary fixtures generated from `write_artifact_parquet`).
- Family 1.3 incremental-merge integrity test (§12) explicitly exercises Parquet-load → `artifact_from_facts_incremental` → Parquet-write.

One-time migration: on first invocation of `spur graph build` post-upgrade, the existing `<hash>.json` files in `.git/spur-graph/artifacts/...` become orphaned. A small `store::cache::sweep_legacy_json()` is invoked at cache-init time to delete them. Idempotent.

### PR4 — DuckDB MCP enablement (the bd-1rqxk capability)
Adds `crates/spur-context/src/sql/schema_code_graph.sql` (views over Parquet), the DuckDB MCP server crate or wiring, the `data-analyst` brain skill, and the smoke test from bd-1rqxk's original ACs. Depends on PR1–PR3 being merged.

## 10. Content hash flow

The post-cutover hash flow is byte-identical to today's:

1. `GraphArtifactBodyForHash { files, symbols, edges, file_manifests, graph_content_hash, manifest_version, tombstones }`.
2. `serde_json::to_vec(&body)` — canonical, sorted-key serialization performed **in memory only**.
3. `blake3::hash(canonical_bytes).to_hex()`.

The function lives in `canonical_hash.rs` (renamed from `json.rs`). It is the only remaining use of `serde_json` for full-artifact serialization in `spur-graph`. Pointer files and `manifest.json` use serde_json for their small payloads independently.

Existing hashes remain valid. Existing `.spur/graph-index.pointer.json` files in checked-out worktrees continue to identify the same graph state; on first `graph build` post-upgrade, the bytes for that hash are re-written as Parquet at a new directory path, and the pointer's `canonical_artifact_path` field updates to point at the new directory.

A hash-stability snapshot test (Family 1.2, §12) guards against accidental changes to the canonical serialization.

## 11. Incremental builds

### 11.1 Today's flow

`spur-cli graph build` (incremental case):
1. `load_artifact(.spur/graph-index.pointer.json → canonical path)`
2. Compute `GraphFacts` for changed files via git oid diff.
3. `artifact_from_facts_incremental(&prev_artifact, &new_facts, &worktree_root)` → merged `GraphIndexArtifact`.
4. `write_artifact(&merged, &canonical_path)`.

### 11.2 Post-cutover flow

Same shape, different I/O:
1. `read_current_pointer(&worktree_root)` → `<hash>.parquet/` directory.
2. `read_artifact_parquet(dir)` → in-memory `GraphIndexArtifact`.
3. `artifact_from_facts_incremental(...)` — **unchanged.**
4. `write_artifact_parquet(&merged, &base_dir)` → new `<hash>.parquet/`.
5. `write_current_pointer(&worktree_root, &new_hash_dir)`.

`artifact_from_facts_incremental` is not modified. It receives a full `GraphIndexArtifact` exactly as before. The expected cold-load time is ~50–80 ms (vs. 250–500 ms for JSON today), so the round-trip-then-merge path is already faster than the status quo.

### 11.3 `file_manifests.node_ids` round-trip

Per §6.5, `file_manifests.parquet` carries `node_ids LIST<INT64>` eagerly. This preserves byte-equivalence with the in-memory `GraphFileManifestEntry`. The reader does not need to JOIN or recompute.

### 11.4 Deferred optimization B — predicate-pushdown partial load

When only a small fraction of files change, the full round-trip becomes wasteful: ~95% of the data is loaded and re-emitted unchanged. A future optimization (out of scope for this spec) introduces:

```rust
pub fn read_artifact_partial(
    dir: &Path,
    changed_file_paths: &[&str],
) -> anyhow::Result<PartialArtifact>;
```

`nodes.parquet`'s `(file_path, id)` sort key (committed to in §6.1) is the enabling decision. Row-group statistics on `file_path` let Parquet skip groups untouched by `changed_file_paths`. `artifact_from_facts_incremental` would be refactored to accept the partial input and merge with implicitly-loaded unchanged buckets. Tracked as a follow-up issue post-bd-1rqxk.

## 12. Test plan

### Family 1 — Correctness (mandatory)

**1.1 Round-trip identity.** A representative `GraphIndexArtifact` (the existing test fixture used by `spur-graph/tests/extractor.rs`) is written via `write_artifact_parquet`, read back via `read_artifact_parquet`, and compared field-by-field to the input. All fields equal, ordering preserved. Asserts the contract that Parquet preserves everything JSON did.

**1.2 Hash stability.** `artifact_content_hash_blake3_hex` against the test fixture produces a snapshot-tested hex string. The snapshot is committed. Any unintended change to the canonical-JSON serialization will fail this test before invalidating user pointers in the wild.

**1.3 Incremental merge integrity.** Existing `spur-graph/tests/extractor.rs` suite runs against Parquet-loaded prev-artifacts. Same assertions on merged output; the existing merge logic is unmodified, so this is a passive test that the new I/O layer is transparent.

### Family 2 — Schema invariants (mandatory)

**2.1 Sort order.** Read `nodes.parquet`, assert columns sorted by `(file_path, id)`. Read `edges.parquet`, assert sorted by `(src_id, dst_id)`. Read `edges_unresolved.parquet`, assert sorted by `src_id`.

**2.2 Dictionary encoding present.** Read Parquet footer; assert `kind`, `confidence`, `file_path`, `qualified_name`, `enclosing_scope` use DICTIONARY encoding. Test breaks on encoder regression.

**2.3 Compression.** Footer-level assert: every column chunk uses ZSTD compression with level encoded in metadata where available.

**2.4 DuckDB round-trip.** When the `duckdb` CLI is available on `$PATH`, spawn it, run `SELECT COUNT(*) FROM read_parquet(...)` against each output file, and compare to in-Rust row counts. Catches any subtle Arrow→Parquet→DuckDB incompatibility. The test is `#[ignore]`-gated behind `which::which("duckdb")`; CI gains the assertion only if `duckdb` is added to the CI image. Until then the test is opt-in for local development and acts as documentation of the interop contract.

### Family 3 — Performance benchmark (without asserts)

**3.1 Bench in `crates/spur-graph/benches/parquet.rs`** (Criterion or hand-rolled `Instant`-based; whichever matches existing SPUR conventions). Measures:
- `write_artifact_parquet` wall-clock and peak RSS on the SPUR fixture.
- `read_artifact_parquet` wall-clock and peak RSS on the SPUR fixture.
- `artifact_content_hash_blake3_hex` wall-clock (regression guard on the canonical serialization).

No asserted thresholds — numbers age across hardware. Committed for trend tracking.

### Out of scope

- Property-test fuzzing of arbitrary artifact shapes. Existing fixtures cover real shapes.
- Cross-tool compatibility matrix (Polars, Spark, pyarrow). DuckDB is the only consumer; Family 2.4 covers it.
- A formal migration-from-JSON regression test. PR3's reader-migration coverage exercises this in CI.

## 13. Risks & rollback

**Risk: arrow-rs Parquet writer is not bit-for-bit deterministic across versions.**
Page boundaries, dictionary ordering can shift on a `parquet` crate upgrade. Impact: the content hash flow is unaffected because it does **not** hash Parquet bytes — it hashes the canonical-JSON in-memory form (§10). Family 1.2 snapshot guards the canonical form. Parquet-byte non-determinism is therefore confined to disk layout, not identity, and is acceptable.

**Risk: a reader missed in PR3 still calls `load_artifact("<...>.json")` against a path that no longer exists.**
Mitigation: PR3 deletes `write_artifact` outright. Any remaining JSON-path caller fails at compile time (no such function) or at runtime with a clear error from `load_artifact`'s directory-vs-file detection. CI runs the full workspace test suite, which exercises every documented reader.

**Risk: existing `.spur/graph-index.pointer.json` files in user worktrees reference a `.json` artifact path that no longer exists after upgrade.**
Mitigation: on next `spur graph build`, the cache code does not find the legacy file and behaves as a clean rebuild (the content hash is unchanged, so the rebuilt directory has the same name). The pointer is updated to the new path. One-time cost of a single graph rebuild per upgrade — already cheap.

**Risk: a downstream tool (out-of-tree, not visible in the workspace grep) reads `.spur/graph-index.json` directly.**
Mitigation: the `mentions/registry.rs` legacy-path fallback in PR3 logs a deprecation warning if the legacy file is encountered. We do not preserve the JSON writer to support unknown out-of-tree readers; the deprecation is intentional.

**Rollback.** If a critical issue surfaces post-PR3, revert PR3 (the cutover). PR1 and PR2 remain in place (no behavior change). The workspace returns to JSON writes. No data loss; existing Parquet directories become inert until reads are restored.

## 14. Open follow-ups (tracked, not in this spec)

| ID | Item | Trigger |
|---|---|---|
| FU-A | Load DuckPGQ extension in the MCP server | ≥10 brain queries hand-rolling recursive CTEs that PGQ MATCH would express |
| FU-B | Load Onager extension in the MCP server | ≥10 brain queries attempting centrality/community via recursive SQL |
| FU-C | Apache GraphAr layering on top of Parquet | Kùzu's GraphAr loader stabilizes, or a Spark/GraphScope batch job is on the deck |
| FU-D | Predicate-pushdown partial load (§11.4) | Measured slowness on incremental builds where ≤5% of files change |
| FU-E | Arrow IPC dual-write for mmap zero-copy | An in-process Rust reader pins load latency below what Parquet decode allows |
| FU-F | Drop canonical-JSON hashing in favor of stable struct hash | A `SCHEMA_VERSION` bump that already invalidates pointers, making the hash basis swap free |

## 15. References

- bd-1rqxk — tracking issue with all design amendments.
- `crates/spur-context/poc/duckdb-analyst/` (commit 754b07a8) — POC demonstrating the load-time pathology of the current JSON path.
- DuckDB documentation: `read_parquet`, predicate pushdown, row group statistics.
- DuckPGQ — Property Graph documentation, VLDB 2023 paper.
- Onager — graph analytics extension for DuckDB.
- Apache GraphAr — Format Specification, incubating project.
- arrow-rs and parquet crates — pure-Rust implementations under `arrow-rs` project.
