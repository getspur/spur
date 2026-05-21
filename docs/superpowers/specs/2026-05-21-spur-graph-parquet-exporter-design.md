# spur-graph Parquet Exporter — Design Spec

**Date:** 2026-05-21 (revised 2026-05-22 after dual-worker review; see `docs/reviews/2026-05-21-parquet-exporter-review-*.md`)
**Status:** Approved (brainstorming); v2 incorporates F1–F13 from claude-code + codex review.
**Tracking issue:** bd-1rqxk
**Companion POC:** `crates/spur-context/poc/duckdb-analyst/` (commit 754b07a8)

## 1. Summary

Migrate `spur-graph`'s persisted artifact from a single multi-megabyte JSON file to a directory of Apache Parquet files written via pure-Rust `arrow-rs` + `parquet` crates. JSON `write_artifact` is deleted at the writer-flip step. Canonical-JSON content hashing stays in memory only. Projected wins are stated as honest ranges in §4 and turned into measured pass/fail gates at PR3b merge time. The output is directly consumable by DuckDB + DuckPGQ + Onager without any libduckdb linkage anywhere in SPUR's compiled crates.

## 2. Goals

- Write the graph index as Parquet — `nodes`, `edges`, `edges_by_dst`, `edges_unresolved`, `files`, `file_manifests`, `tombstones` — plus a small `manifest.json`.
- Delete the JSON full-artifact writer (`write_artifact`) at the writer-flip step. Legacy JSON readers remain importable for one release.
- Keep `artifact_content_hash_blake3_hex` byte-identical to today so existing pointer files in checked-out worktrees remain valid.
- Preserve the incremental-build flow (`artifact_from_facts_incremental`) unchanged.
- Enable the spur-context DuckDB MCP capability (bd-1rqxk acceptance criteria 2–6) without modification to the data layer.
- Leave `spur-graph` free of any `libduckdb` linkage.
- Performance: meet hard thresholds measured in CI (see §12 Family 3).

## 3. Non-goals

- Arrow IPC (Feather v2) dual-write for mmap zero-copy reads. Promote only when a measured in-process Rust reader pins load latency.
- Apache GraphAr directory/metadata convention. Promote when a concrete external consumer (Kùzu, Spark, GraphScope) benefits.
- Predicate-pushdown partial reads in the incremental build path. Tracked as future optimization B (see §11.4).
- Switching the content-hash basis to Parquet bytes. Stays canonical-JSON-in-memory.
- Changing the `.spur/graph-index.pointer.json` schema in a backwards-incompatible way.
- Removing legacy JSON read support in this issue. Stays available one release; sweep deferred.

## 4. Background

`spur-graph`'s persisted artifact today is a single JSON document at `.git/spur-graph/artifacts/<manifest_version>/<hash>.json`, surfaced via `.spur/graph-index.pointer.json`. At SPUR's current scale (27.5k symbols, 47k resolved edges, 1.5k files) the artifact is ~42 MB pretty-printed (via `serde_json::to_string_pretty` in `store/json.rs:461`). The May 2026 POC (`crates/spur-context/poc/duckdb-analyst/`) demonstrated that DuckDB ingestion of this JSON via `read_json` + `UNNEST` materializes the entire document as a single struct-laden row, and any window function over the unnest blows past 5 GB peak RSS. The POC worked around this with a streaming view and a split node-id mapping, but the underlying problem is structural: JSON-on-disk for a graph artifact does not scale.

### Projected wins (honest ranges; measured gates land in §12)

| Dimension | JSON today | Parquet (projected) | Honest range |
|---|---|---|---|
| On-disk size | 42 MB (pretty-printed) | 3–6 MB | **4–8× smaller** vs. non-pretty JSON (~20 MB); the 8–14× figure earlier in design discussion conflated pretty-print whitespace savings. |
| Cold load to in-memory artifact, Rust reader | 250–500 ms (serde_json + `deduplicate_symbols` + `validate_ranges`) | 50–80 ms decode + ~10–20 ms validation | **3–6× faster**, attributable partly to format and partly to skipping repeated validation work. |
| Peak RSS during DuckDB ingestion | 5 GB+ (OOM under UNNEST window) | 150–250 MB | **~30× lower** for the DuckDB MCP path. Note: the OOM is specific to DuckDB's read_json+UNNEST pattern, not the Rust JSON reader, which uses ~150–400 MB today. |
| Column projection / predicate pushdown | impossible | inherent | Asymptotic — see §6 row-group sizing discussion. |
| `WHERE src_id = X` (callee lookup) | full scan | row-group pruning when graph is large enough to span multiple row groups | At today's scale (47K edges in one 64K-row group), the pruning win is **zero**; it kicks in around 3–5× current size. |
| `WHERE dst_id = X` (caller lookup) | full scan | row-group pruning via `edges_by_dst.parquet` | Same asymptotic story as callee lookup. |

At 10× SPUR's current size, JSON becomes unusable on developer hardware; Parquet stays flat. This is the asymptotic argument; the constant-factor speedup at today's scale is modest.

### Numbers separated by consumer path

- **Rust reader (Rust-side `load_artifact` / `read_artifact_parquet`):** load latency is the dominant factor. Targets: ≤ today's `load_artifact` wall-clock on the SPUR fixture; ≤ today's peak RSS. Gates in §12.
- **DuckDB MCP reader (out-of-process):** peak RSS is the dominant factor — Parquet eliminates the read_json+UNNEST pathology. Targets: cold first-query latency, peak RSS.

The dimensions are measured separately at PR3b merge time.

## 5. On-disk layout

```
.git/spur-graph/artifacts/<manifest_version>/<hash>.parquet/
  nodes.parquet
  edges.parquet
  edges_by_dst.parquet
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

**Multi-worktree caveat (deferred risk).** Git worktrees share `.git/spur-graph/artifacts/...` only when `core.worktree` and the GIT_DIR layout permit it. Across `git worktree add`-created worktrees, the `.spur/graph/CURRENT` symlink may point at a path the secondary worktree's GIT_DIR cannot reach. Documented in §13; not blocking, but reviewed when SPUR adds multi-worktree workflows.

## 6. Parquet schemas

All files: ZSTD level 3. Row group size **decided pre-PR2 by benchmark** (see §12) over 16K/32K/64K against DuckDB and Rust reads on the live SPUR fixture. The 65 536 default is a placeholder pending that bench.

Schemas mirror Rust struct types exactly. Column names match Rust field names; nullability matches `Option<T>` / `T`.

### 6.1 `nodes.parquet`

Mirrors `GraphSymbolArtifact` (`crates/spur-graph/src/schema.rs:146`).

| Column | Arrow type | Encoding | Nullable | Notes |
|---|---|---|---|---|
| `stable_symbol_id` | Utf8 | PLAIN | no | hex VARCHAR |
| `node_id` | Int64 | PLAIN | no | Dense BIGINT assigned at export — `enumerate()` over the sorted symbol list. **Per-artifact only; not stable across builds. See §11.5.** |
| `file_path` | Utf8 | DICTIONARY | no | ~1.5k distinct vs. 27k rows |
| `byte_range_start` | Int64 | PLAIN | no | `GraphSymbolArtifact.byte_range[0]`; `SourceRange` is always present |
| `byte_range_end` | Int64 | PLAIN | no | `GraphSymbolArtifact.byte_range[1]` |
| `line_start` | Int32 | PLAIN | no | `GraphSymbolArtifact.line_range[0]` |
| `line_end` | Int32 | PLAIN | no | `GraphSymbolArtifact.line_range[1]` |
| `entity_name` | Utf8 | DICTIONARY | no | `String` in Rust, not `Option<String>` |
| `qualified_name` | Utf8 | DICTIONARY | no | `String` with `#[serde(default)]` |
| `symbol_kind` | Utf8 | DICTIONARY | no | NodeKind discriminator, ~15 distinct values |
| `anchor_hash` | Utf8 | PLAIN | no | `String` in Rust |
| `enclosing_scope` | Utf8 | DICTIONARY | yes | `Option<String>` in Rust |

**Sort key: `(file_path ASC, stable_symbol_id ASC)`.** Enables future predicate-pushdown incremental loads keyed on changed files (deferred optimization B). Does not change the v1 read path; the full-artifact reader iterates sequentially either way.

### 6.2 `edges.parquet`

Mirrors `GraphEdgeArtifact` (`crates/spur-graph/src/schema.rs:163`). Resolved edges only (rows where `target_stable_symbol_id` is `Some`). Unresolved edges go to `edges_unresolved.parquet` (§6.3).

| Column | Arrow type | Encoding | Nullable | Notes |
|---|---|---|---|---|
| `source_stable_symbol_id` | Utf8 | DICTIONARY | no |  |
| `target_stable_symbol_id` | Utf8 | DICTIONARY | no | This file holds resolved edges only, so the column is non-null here |
| `src_id` | Int64 | PLAIN | no | Resolved at export — `stable_symbol_id → node_id` lookup |
| `dst_id` | Int64 | PLAIN | no | Same |
| `target_label` | Utf8 | DICTIONARY | yes | `Option<String>` |
| `relation` | Utf8 | DICTIONARY | no | `RelationKind`, ~10 distinct values |
| `confidence` | Utf8 | DICTIONARY | no | `Confidence`, ~3 distinct values |
| `confidence_score` | Float64 | PLAIN | no | `f32` in Rust, widened to `Float64` for Parquet portability |
| `edge_kind` | Utf8 | DICTIONARY | yes | `Option<GraphEdgeKind>`; **MUST NOT be omitted** |

**Sort key: `(src_id ASC, dst_id ASC)`.** Enables row-group pruning for **callee** lookups (`WHERE src_id = X` returns the edges going *out* of X). For **caller** lookups (`WHERE dst_id = X` — edges coming *into* X), see §6.3 `edges_by_dst.parquet`.

### 6.3 `edges_by_dst.parquet`

Same row set and same schema as `edges.parquet`, sorted differently. Sole purpose: row-group pruning for inbound-edge queries (caller lookup).

**Sort key: `(dst_id ASC, src_id ASC)`.**

DuckDB views can `UNION ALL` it back with `edges.parquet` if a query needs both directions, but in practice the MCP server picks one file or the other based on the predicate. Adds ~3–6 MB to on-disk footprint at current scale; small relative to the value of having both pruning directions available.

The writer emits both files in one pass: collect edges into a `Vec`, sort by `(src_id, dst_id)` for `edges.parquet`, re-sort by `(dst_id, src_id)` for `edges_by_dst.parquet`. Re-sort cost is negligible (~5 ms at current scale).

### 6.4 `edges_unresolved.parquet`

Only those edges where the target did not resolve to a known symbol (~59% of edges in today's SPUR graph — dynamic dispatch, macro bodies, HOF arguments).

| Column | Arrow type | Encoding | Nullable |
|---|---|---|---|
| `source_stable_symbol_id` | Utf8 | DICTIONARY | no |
| `src_id` | Int64 | PLAIN | no |
| `target_label` | Utf8 | DICTIONARY | yes |
| `relation` | Utf8 | DICTIONARY | no |
| `confidence` | Utf8 | DICTIONARY | no |
| `confidence_score` | Float64 | PLAIN | no |
| `edge_kind` | Utf8 | DICTIONARY | yes |

Sort key: `src_id ASC`.

### 6.5 `files.parquet`

Mirrors `GraphFileArtifact` (`crates/spur-graph/src/schema.rs:105`).

| Column | Arrow type | Encoding | Nullable |
|---|---|---|---|
| `stable_file_id` | Utf8 | PLAIN | no |
| `file_path` | Utf8 | DICTIONARY | no |

Sort key: `file_path ASC`.

### 6.6 `file_manifests.parquet`

Mirrors `GraphFileManifestEntry` (`crates/spur-graph/src/schema.rs:112`).

| Column | Arrow type | Encoding | Nullable | Notes |
|---|---|---|---|---|
| `stable_file_id` | Utf8 | PLAIN | no |  |
| `path` | Utf8 | DICTIONARY | no |  |
| `content_oid` | Utf8 | PLAIN | no | `String` in Rust, not `Option<String>` |
| `node_ids` | List(Int64) | PLAIN | no | Per-file node ids — used eagerly by `artifact_from_facts_incremental` |

`node_ids` is stored eagerly as `LIST<INT64>` (not derived via JOIN) to keep `read_artifact_parquet` a straightforward round-trip producing a byte-equivalent `GraphIndexArtifact`. The few-KB redundancy cost is dwarfed by the simplicity win.

### 6.7 `tombstones.parquet`

Mirrors `GraphTombstoneEntry { path: String, stable_file_id: String }`.

| Column | Arrow type | Encoding | Nullable |
|---|---|---|---|
| `path` | Utf8 | DICTIONARY | no |
| `stable_file_id` | Utf8 | PLAIN | no |

Sort key: `path ASC`.

### 6.8 `manifest.json`

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

JSON is fine here because the file is small (a few hundred bytes), not the artifact body. The pointer file (`.spur/graph-index.pointer.json`) and this manifest are the only JSON that survives the cutover. Header-only reads (§7 `read_artifact_header_parquet`) consume this file directly.

## 7. Surface API

New public API on `spur_graph` (re-exported from `store/parquet.rs`):

```rust
/// Writes the artifact as a Parquet directory inside `base_dir`. The directory
/// name is the graph_content_hash. Returns the absolute path written. Emits
/// both edges.parquet (src-sorted) and edges_by_dst.parquet (dst-sorted) in
/// one pass.
pub fn write_artifact_parquet(
    artifact: &GraphIndexArtifact,
    base_dir: &Path,
) -> anyhow::Result<PathBuf>;

/// Reads a Parquet directory back into a GraphIndexArtifact byte-equivalent
/// to the one that was written. edges_by_dst.parquet is not read back (it is
/// a derived view of the same row set as edges.parquet).
pub fn read_artifact_parquet(dir: &Path) -> anyhow::Result<GraphIndexArtifact>;

/// Reads only the header (counts, schema version, content hash, commit oid)
/// from `<dir>/manifest.json`. Replaces the existing `read_artifact_header`
/// fast path used by spur-tui for cache validation. ~sub-millisecond.
pub fn read_artifact_header_parquet(dir: &Path) -> anyhow::Result<GraphIndexHeader>;

/// Updates the .spur/graph/CURRENT pointer to point at the given hash directory.
pub fn write_current_pointer(worktree_root: &Path, hash_dir: &Path) -> anyhow::Result<()>;

/// Reads .spur/graph/CURRENT and returns the absolute path of the live
/// Parquet directory.
pub fn read_current_pointer(worktree_root: &Path) -> anyhow::Result<PathBuf>;
```

**Shared artifact-location resolver.** A new function `resolve_artifact_location(input: &Path) -> ArtifactLocation` handles both legacy (`.json` file) and new (`.parquet/` directory) layouts in one place. `ArtifactLocation` is `enum { LegacyJson(PathBuf), Parquet(PathBuf) }`. Every caller in `spur-mcp`, `spur-tui`, `spur-cli` goes through this helper instead of doing ad-hoc file-vs-directory checks. Cache keys derive from `manifest.json` content hash, not from the directory name pattern.

The existing `load_artifact(path: &Path)` function in `schema.rs` is updated to call the resolver and dispatch to either the legacy JSON loader (one-release-deprecated, with a warning log) or `read_artifact_parquet`. All call sites pass the worktree-root path or the existing `canonical_artifact_path` from the pointer file — both work.

`write_artifact` (the JSON full-artifact writer) is removed at the writer-flip step (PR3b).

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
│                       and .spur/graph/CURRENT. Hosts resolve_artifact_location.
├── parquet.rs        ─ NEW. write_artifact_parquet, read_artifact_parquet,
│                       read_artifact_header_parquet, schemas, RecordBatch builders.
├── cache.rs          ─ MODIFIED. Writes Parquet directory instead of JSON file.
│                       Dedup by content hash unchanged.
└── snapshot.rs       ─ unchanged.
```

`mod.rs` `pub use` re-exports preserve public symbols where possible: `artifact_from_facts`, `artifact_from_facts_incremental`, `BuildMode`, `EXTRACTOR_VERSION`, `SCHEMA_VERSION`, `current_manifest_version`. Symbols that no longer exist (`write_artifact`) are removed at PR3b and every caller is migrated in the same change-set.

Cargo:

```toml
[dependencies]
parquet      = { workspace = true, default-features = false, features = ["zstd"] }
arrow-array  = { workspace = true }
arrow-schema = { workspace = true }
```

**No Cargo feature flag.** Parquet is the only on-disk format post-cutover; making it opt-out is incoherent. All three crates are pure Rust, workspace-pinned. No native linkage; no libduckdb anywhere.

## 9. Migration sequence

bd-1rqxk becomes the parent epic. Each PR is independently mergeable.

### PR1 — Split `store/json.rs` (mechanical)
Move ~1700 lines of construction logic from `json.rs` into `build.rs`. `mod.rs` re-exports unchanged. Zero behavior delta. `cargo check -p spur-graph -p spur-cli -p spur-tui -p spur-mcp` passes without changes elsewhere.

### PR2 — Add `store/parquet.rs`
Implement `write_artifact_parquet`, `read_artifact_parquet`, `read_artifact_header_parquet`, schemas, RecordBatch builders. Emit both `edges.parquet` and `edges_by_dst.parquet`. Add `arrow-array`, `arrow-schema`, `parquet` to workspace dependencies. No caller changes. Round-trip identity test for the writer/reader pair (Family 1.1, §12). Pre-PR2 row-group benchmark resolves the open 16K/32K/64K choice. At this point both JSON and Parquet writers coexist; only the round-trip test exercises Parquet.

### PR3a — Reader tolerance (new, additive, low-risk)
`load_artifact` and the new `resolve_artifact_location` accept **both** formats: legacy `.json` file (existing path) and new `.parquet/` directory. JSON writer remains alive. Every reader in `spur-mcp`, `spur-tui`, `spur-cli` is migrated to call the resolver instead of doing ad-hoc file-vs-directory checks.

| Crate | File | Change |
|---|---|---|
| `spur-graph` | `schema.rs` | `load_artifact` calls `resolve_artifact_location`; dispatches to legacy JSON or `read_artifact_parquet` |
| `spur-graph` | `store/pointer.rs` | New `resolve_artifact_location` helper |
| `spur-mcp` | `server/handlers/code_graph.rs` | Calls go via resolver; `GRAPH_ARTIFACT_RELATIVE_PATH` becomes a default the resolver may override |
| `spur-tui` | `mentions/code_graph/source.rs` | Same; header-only reads call `read_artifact_header_parquet` for Parquet dirs |
| `spur-tui` | `mentions/registry.rs` | Same |
| `spur-cli` | `commands/graph.rs` | Read path goes via resolver; write path still JSON |

At the end of PR3a, the workspace can **read** Parquet but still **writes** JSON. Safe to merge independently. Any reader missed is caught by the resolver's runtime detection.

### PR3b — Writer flip (the cutover)
`cache.rs` writes Parquet directories. Pointer file `canonical_artifact_path` references `.parquet/`. Delete `write_artifact` from `json.rs`. Rename `json.rs` → `canonical_hash.rs`.

Legacy JSON readers from PR3a remain in place for one release after PR3b. They log a deprecation warning if a legacy file is encountered. **No `sweep_legacy_json()` is invoked.** Cleanup of orphaned `.json` files in `.git/spur-graph/artifacts/...` is deferred to a later, separately tracked PR after Parquet has accumulated field data without regression.

Family 3 performance gates (§12) must pass on the SPUR fixture before this PR merges.

### PR4 — DuckDB MCP enablement (the bd-1rqxk capability)
Adds `crates/spur-context/src/sql/schema_code_graph.sql` (views over Parquet), the DuckDB MCP server crate or wiring, the `data-analyst` brain skill, and the smoke test from bd-1rqxk's original ACs. Depends on PR1–PR3b being merged.

## 10. Content hash flow

The post-cutover hash flow is byte-identical to today's:

1. `GraphArtifactBodyForHash { files, symbols, edges, file_manifests, graph_content_hash, manifest_version, tombstones }` — **the field declaration order in this struct IS the canonical serialization order.** Any reorder silently changes every hash fleet-wide.
2. `serde_json::to_vec(&body)` — canonical, sorted-key serialization performed **in memory only**.
3. `blake3::hash(canonical_bytes).to_hex()`.

The function lives in `canonical_hash.rs` (renamed from `json.rs`). It is the only remaining use of `serde_json` for full-artifact serialization in `spur-graph`. Pointer files and `manifest.json` use serde_json for their small payloads independently.

A **field-order guard test** (Family 1.2, §12) commits both the hash hex *and* a serde-snapshot of the canonical bytes from a minimal artifact. Any reorder of `GraphArtifactBodyForHash` fields fails both assertions before reaching the fleet.

Existing hashes remain valid. Existing `.spur/graph-index.pointer.json` files in checked-out worktrees continue to identify the same graph state; on first `graph build` post-PR3b, the bytes for that hash are re-written as Parquet at a new directory path, and the pointer's `canonical_artifact_path` field updates to point at the new directory.

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

`artifact_from_facts_incremental` is not modified. It receives a full `GraphIndexArtifact` exactly as before. The expected cold-load time is governed by Family 3 gates (§12); current projection is ~50–80 ms for Parquet decode plus ~10–20 ms validation, vs. ~250–500 ms for JSON.

### 11.3 `file_manifests.node_ids` round-trip

Per §6.6, `file_manifests.parquet` carries `node_ids LIST<INT64>` eagerly. This preserves byte-equivalence with the in-memory `GraphFileManifestEntry`. The reader does not need to JOIN or recompute.

### 11.4 Deferred optimization B — predicate-pushdown partial load

When only a small fraction of files change, the full round-trip becomes wasteful: ~95% of the data is loaded and re-emitted unchanged. A future optimization (out of scope for this spec) introduces:

```rust
pub fn read_artifact_partial(
    dir: &Path,
    changed_file_paths: &[&str],
) -> anyhow::Result<PartialArtifact>;
```

`nodes.parquet`'s `(file_path, stable_symbol_id)` sort key (committed to in §6.1) is the enabling decision. Row-group statistics on `file_path` let Parquet skip groups untouched by `changed_file_paths`. `artifact_from_facts_incremental` would be refactored to accept the partial input and merge with implicitly-loaded unchanged buckets. Tracked as FU-D post-bd-1rqxk.

### 11.5 `node_id` stability invariant

**`node_id` is per-artifact only. It is not stable across builds.**

`node_id` is assigned at export time by `enumerate()` over the sorted symbol list. Adding, removing, or reordering files in the worktree shifts the `node_id` of every symbol after the change. Any consumer (DuckDB MCP query, cached result set, external tool) that materializes a `node_id` and re-uses it against a subsequently rebuilt artifact will silently drift.

The reliable identifier across builds is `stable_symbol_id` (`Utf8`, content-derived). Consumers that need cross-build identity MUST use `stable_symbol_id`, JOIN against the live `nodes.parquet` to recover the current `node_id`, and never persist `node_id` outside the lifetime of a single `<hash>.parquet/` directory.

The DuckDB MCP server documentation (added in PR4) calls this invariant out explicitly. The `data-analyst` brain skill includes a discipline rule: *never materialize `node_id` into a brain-readable answer without joining back to `stable_symbol_id`*.

## 12. Test plan

### Family 1 — Correctness (mandatory)

**1.1 Round-trip identity.** A representative `GraphIndexArtifact` (the existing test fixture used by `spur-graph/tests/extractor.rs`) is written via `write_artifact_parquet`, read back via `read_artifact_parquet`, and compared field-by-field to the input. All fields equal, ordering preserved. Asserts the contract that Parquet preserves everything JSON did, including `edge_kind` and other previously-omitted fields.

**1.2 Hash stability + field-order guard.** `artifact_content_hash_blake3_hex` against the test fixture produces a snapshot-tested hex string. The snapshot is committed. Additionally, the canonical bytes (`serde_json::to_vec(&body)`) of a minimal artifact are snapshot-tested, so any reorder of `GraphArtifactBodyForHash` fields fails the snapshot diff before it can invalidate user pointers in the wild.

**1.3 Incremental merge integrity.** Existing `spur-graph/tests/extractor.rs` suite runs against Parquet-loaded prev-artifacts. Same assertions on merged output; the existing merge logic is unmodified, so this is a passive test that the new I/O layer is transparent.

**1.4 Legacy-JSON read tolerance (PR3a).** Loading an artifact via `resolve_artifact_location` returns the same `GraphIndexArtifact` whether the input is a legacy `<hash>.json` file or a new `<hash>.parquet/` directory. Both backed by the same fixture.

### Family 2 — Schema invariants (mandatory)

**2.1 Sort order.** Read `nodes.parquet`, assert columns sorted by `(file_path, stable_symbol_id)`. Read `edges.parquet`, assert sorted by `(src_id, dst_id)`. Read `edges_by_dst.parquet`, assert sorted by `(dst_id, src_id)`. Read `edges_unresolved.parquet`, assert sorted by `src_id`.

**2.2 Dictionary encoding present.** Read Parquet footer; assert `symbol_kind`, `relation`, `confidence`, `file_path`, `qualified_name`, `enclosing_scope`, `edge_kind` columns use DICTIONARY encoding. Test breaks on encoder regression.

**2.3 Compression.** Footer-level assert: every column chunk uses ZSTD compression with level encoded in metadata where available.

**2.4 DuckDB round-trip (hard CI gate).** Adds `duckdb` CLI to the SPUR CI image. Test spawns `duckdb`, runs `SELECT COUNT(*) FROM read_parquet(...)` against each output file, and compares to in-Rust row counts. Catches any Arrow→Parquet→DuckDB incompatibility. Not `#[ignore]`-gated; a missing `duckdb` binary in CI is a CI-configuration failure, not a test-skip.

### Family 3 — Performance gates (hard CI gates at PR3b merge)

Each gate is asserted on the SPUR fixture. Numbers are wall-clock medians of N=10 runs on the CI runner, peak RSS via `getrusage`.

| Gate | Path | Threshold | Notes |
|---|---|---|---|
| 3.1 | `write_artifact_parquet` on the SPUR fixture | ≤ 2× current `write_artifact` (JSON) wall-clock | We accept doubled write cost for one cycle if it lands within this gate. |
| 3.2 | `read_artifact_parquet` on the SPUR fixture | ≤ 0.5× current `load_artifact` (JSON) wall-clock | Headline reader win. |
| 3.3 | `read_artifact_parquet` peak RSS | ≤ current `load_artifact` peak RSS | No regression on the Rust path. |
| 3.4 | Full incremental build (load + merge + write), unchanged repo | ≤ 0.8× current incremental build wall-clock | End-to-end. |
| 3.5 | DuckDB cold first-query latency over `read_parquet(...)` | ≤ 250 ms | The MCP-server hot path. |
| 3.6 | DuckDB peak RSS during ingestion | ≤ 500 MB | The OOM that motivated the migration must stay buried. |

A Criterion-style or hand-rolled `Instant`-based bench in `crates/spur-graph/benches/parquet.rs` records the numbers; the gates are CI-level `assert!` in a separate integration test that reads the bench output.

### Pre-PR2 — Row-group size benchmark

Before PR2 locks the row-group size, run `write_artifact_parquet` with row-group sizes 16384, 32768, 65536 against the SPUR fixture and measure:
- on-disk size (compression ratio is row-group-size-sensitive),
- `read_artifact_parquet` wall-clock,
- DuckDB cold first-query for `WHERE src_id = X` and `WHERE dst_id = X`,
- DuckDB peak RSS.

Pick the smallest size that does not regress on-disk size by >10% relative to 65536, on the theory that smaller row groups give earlier pruning benefit as the graph grows. Lock the chosen size into `parquet_writer.row_group_size` in `manifest.json` and into the §6 spec text.

### Out of scope

- Property-test fuzzing of arbitrary artifact shapes. Existing fixtures cover real shapes.
- Cross-tool compatibility matrix (Polars, Spark, pyarrow). DuckDB is the only consumer; Family 2.4 covers it.

## 13. Risks & rollback

**Risk: arrow-rs Parquet writer is not bit-for-bit deterministic across versions.**
Page boundaries, dictionary ordering can shift on a `parquet` crate upgrade. Impact: the content hash flow is unaffected because it does **not** hash Parquet bytes — it hashes the canonical-JSON in-memory form (§10). Family 1.2 snapshot guards the canonical form. Parquet-byte non-determinism is therefore confined to disk layout, not identity, and is acceptable.

**Risk: a reader missed in PR3a/PR3b runs against a path layout it does not understand.**
Mitigation: every reader migrates through `resolve_artifact_location`. PR3a is the high-risk step; PR3b only removes the JSON writer after PR3a has been in main for at least one cycle. Any remaining caller fails through the resolver's runtime detection with a clear error message naming the path encountered.

**Risk: existing `.spur/graph-index.pointer.json` files in user worktrees reference a `.json` artifact path that no longer exists after PR3b.**
Mitigation: on next `spur graph build`, the cache code does not find the legacy file and behaves as a clean rebuild (the content hash is unchanged, so the rebuilt directory has the same name). The pointer is updated to the new path. One-time cost of a single graph rebuild per upgrade — already cheap. The legacy-JSON-read fallback from PR3a remains live, so the period between upgrade and next build is tolerated.

**Risk: a downstream tool (out-of-tree, not visible in the workspace grep) reads `.spur/graph-index.json` directly.**
Mitigation: the legacy JSON reader (added in PR3a, retained through PR3b and one further release) keeps these tools working with a deprecation warning. Removal scheduled separately after one release cycle of field data.

**Risk: multi-worktree git layouts make `.spur/graph/CURRENT` resolve to a `.git/spur-graph/artifacts/...` path the secondary worktree's GIT_DIR cannot reach.**
Mitigation: documented as a known limitation. If SPUR formalizes multi-worktree workflows, an additional follow-up makes `.spur/graph/CURRENT` resolve via a relative-path or a per-worktree copy. Not blocking for v1; the current SPUR workflows do not exercise this case.

**Risk: `node_id` instability silently corrupts long-lived consumers** (DuckDB MCP queries, cached result sets, external tools).
Mitigation: §11.5 documents the invariant; the `data-analyst` brain skill enforces it via a discipline rule; the DuckDB MCP server's tool documentation calls it out. Any consumer that ignores the invariant is on its own.

**Rollback.**
- After PR3a: trivial revert. Reader-tolerance is purely additive.
- After PR3b: revert PR3b alone. PR3a's legacy-JSON read fallback is still in place, so the workspace gracefully reads either format. JSON writer comes back via revert. Existing Parquet directories become inert until reads are restored; not deleted. No data loss.

## 14. Open follow-ups (tracked, not in this spec)

| ID | Item | Trigger |
|---|---|---|
| FU-A | Load DuckPGQ extension in the MCP server | ≥10 brain queries hand-rolling recursive CTEs that PGQ MATCH would express |
| FU-B | Load Onager extension in the MCP server | ≥10 brain queries attempting centrality/community via recursive SQL |
| FU-C | Apache GraphAr layering on top of Parquet | Kùzu's GraphAr loader stabilizes, or a Spark/GraphScope batch job is on the deck |
| FU-D | Predicate-pushdown partial load (§11.4) | Measured slowness on incremental builds where ≤5% of files change |
| FU-E | Arrow IPC dual-write for mmap zero-copy | An in-process Rust reader pins load latency below what Parquet decode allows |
| FU-F | Drop canonical-JSON hashing in favor of stable struct hash | A `SCHEMA_VERSION` bump that already invalidates pointers, making the hash basis swap free |
| FU-G | Sweep orphaned legacy `<hash>.json` artifacts | One release of field data after PR3b without regression |
| FU-H | Multi-worktree-safe `.spur/graph/CURRENT` resolution | SPUR formalizes multi-worktree workflows |

## 15. References

- bd-1rqxk — tracking issue with all design amendments.
- `crates/spur-context/poc/duckdb-analyst/` (commit 754b07a8) — POC demonstrating the load-time pathology of the current JSON path.
- `docs/reviews/2026-05-21-parquet-exporter-review-claude-code.md` — review feedback that drove v2.
- `docs/reviews/2026-05-21-parquet-exporter-review-codex.md` — review feedback that drove v2.
- DuckDB documentation: `read_parquet`, predicate pushdown, row group statistics.
- DuckPGQ — Property Graph documentation, VLDB 2023 paper.
- Onager — graph analytics extension for DuckDB.
- Apache GraphAr — Format Specification, incubating project.
- arrow-rs and parquet crates — pure-Rust implementations under `arrow-rs` project.
