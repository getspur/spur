# spur-graph Parquet Exporter — Design Spec

**Date:** 2026-05-21 (v1); revised 2026-05-22 (v2 after first dual-worker review); revised 2026-05-22 (v3 after second dual-worker review).
**Status:** Approved (brainstorming); v3 incorporates G1–G11 from the v2 review.
**Tracking issue:** bd-1rqxk
**Companion POC:** `crates/spur-context/poc/duckdb-analyst/` (commit 754b07a8)
**Reviews:**
- v1 review: `docs/reviews/2026-05-21-parquet-exporter-review-claude-code.md`, `docs/reviews/2026-05-21-parquet-exporter-review-codex.md`.
- v2 review: `docs/reviews/2026-05-22-parquet-exporter-v2-review-claude-code.md`, `docs/reviews/2026-05-22-parquet-exporter-v2-review-codex.md`.

## 1. Summary

Migrate `spur-graph`'s persisted artifact from a single multi-megabyte JSON file to a directory of Apache Parquet files written via pure-Rust `arrow-rs` + `parquet` crates. JSON `write_artifact` is deprecated at the writer-flip step and removed in a follow-up cleanup. Canonical-JSON content hashing stays in memory only. Projected wins are stated as honest ranges in §4 and turned into hard pass/fail gates measured against pre-cutover baselines (§12).

The on-disk schema mirrors the existing in-memory artifact precisely: `node_id` is the extractor's `NodeId(u64)`, not a fresh enumeration; endpoint tables (`nodes.parquet`, `files.parquet`) participate in one shared NodeId namespace; edge source/target columns are polymorphic and resolve against the union of those tables. Round-trip identity is byte-equivalent.

The output is consumable by DuckDB + DuckPGQ + Onager without any libduckdb linkage anywhere in SPUR's compiled crates. The DuckDB MCP exposes `stable_symbol_id` (the content-stable identifier) by default; dense integer IDs needed by graph algorithms are derived at query time, not materialized on disk.

## 2. Goals

- Write the graph index as Parquet — `nodes`, `edges`, `edges_unresolved`, `files`, `file_manifests`, `tombstones` — plus a small `manifest.json`. A candidate `edges_by_dst.parquet` is **bench-gated** for inclusion (§6.3, §12 pre-PR2 bench).
- Deprecate the JSON full-artifact writer (`write_artifact`) at PR3b; remove at PR3c. Legacy JSON readers remain importable for one release.
- Keep `artifact_content_hash_blake3_hex` byte-identical to today so existing pointer files in checked-out worktrees remain valid.
- Preserve the incremental-build flow (`artifact_from_facts_incremental`) unchanged. `file_manifests.node_ids` round-trips byte-equivalent.
- Enable the spur-context DuckDB MCP capability (bd-1rqxk acceptance criteria 2–6) without modification to the data layer.
- Leave `spur-graph` free of any `libduckdb` linkage.
- Performance: meet hard thresholds measured against baselines captured before PR3a (§12 Family 3).
- Atomic directory publication: a partially-written `<hash>.parquet/` is never observable by a reader (§6.9).

## 3. Non-goals

- Arrow IPC (Feather v2) dual-write for mmap zero-copy reads. Promote only when a measured in-process Rust reader pins load latency.
- Apache GraphAr directory/metadata convention. Promote when a concrete external consumer (Kùzu, Spark, GraphScope) benefits.
- Predicate-pushdown partial reads in the incremental build path. Tracked as future optimization B (see §11.4).
- Switching the content-hash basis to Parquet bytes. Stays canonical-JSON-in-memory.
- Changing the `.spur/graph-index.pointer.json` schema in a backwards-incompatible way.
- Removing legacy JSON read support in this issue. Stays available one release; sweep deferred (FU-G).
- Materializing dense integer IDs (Onager-friendly) on disk. Derived at query time by the DuckDB MCP server.

## 4. Background

`spur-graph`'s persisted artifact today is a single JSON document at `.git/spur-graph/artifacts/<manifest_version>/<hash>.json`, surfaced via `.spur/graph-index.pointer.json`. At SPUR's current scale (27.5k symbols, 47k resolved edges, 1.5k files) the artifact is ~42 MB pretty-printed (via `serde_json::to_string_pretty` in `store/json.rs:461`). The May 2026 POC (`crates/spur-context/poc/duckdb-analyst/`) demonstrated that DuckDB ingestion of this JSON via `read_json` + `UNNEST` materializes the entire document as a single struct-laden row, and any window function over the unnest blows past 5 GB peak RSS.

### Projected wins (honest ranges; measured gates land in §12)

| Dimension | JSON today | Parquet (projected) | Honest range |
|---|---|---|---|
| On-disk size | 42 MB (pretty-printed) | 3–6 MB | **4–8× smaller** vs. non-pretty JSON (~20 MB); the 8–14× figure from v1 discussion conflated pretty-print whitespace savings. |
| Cold load to in-memory artifact, Rust reader | 250–500 ms (serde_json + `deduplicate_symbols` + `validate_ranges`) | 50–80 ms decode + ~10–20 ms validation | **3–6× faster**, attributable partly to format and partly to skipping repeated validation work. |
| Peak RSS during DuckDB ingestion | 5 GB+ (OOM under UNNEST window) | 150–250 MB | **~30× lower** for the DuckDB MCP path. The OOM is specific to DuckDB's read_json+UNNEST pattern; the Rust JSON reader uses ~150–400 MB today. |
| Column projection / predicate pushdown | impossible | inherent | Asymptotic — see §6 row-group sizing discussion. |
| `WHERE src_id = X` (callee lookup) | full scan | row-group pruning when graph spans multiple row groups | At today's scale the pruning win is **zero**; it kicks in around 3–5× current size. |
| `WHERE dst_id = X` (caller lookup) | full scan | row-group pruning via `edges_by_dst.parquet` (bench-gated, §6.3) or lazy DuckDB dst-sort | Same asymptotic story as callee lookup. |

### Numbers separated by consumer path

- **Rust reader (Rust-side `load_artifact` / `read_artifact_parquet`):** load latency is the dominant factor. Targets: ≤ baseline `load_artifact` wall-clock captured before PR3a; ≤ baseline peak RSS. Gates in §12.
- **DuckDB MCP reader (out-of-process):** peak RSS is the dominant factor — Parquet eliminates the read_json+UNNEST pathology. Targets: cold first-query latency, peak RSS.

## 5. On-disk layout

```
.git/spur-graph/artifacts/<manifest_version>/<hash>.parquet/
  nodes.parquet
  edges.parquet
  edges_by_dst.parquet         (bench-gated — see §6.3 / §12)
  edges_unresolved.parquet
  files.parquet
  file_manifests.parquet
  tombstones.parquet
  manifest.json                (written LAST; presence is the directory's atomicity sentinel — see §6.9)

.spur/graph/CURRENT             (symlink or pointer file → the latest <hash>.parquet/ dir)
.spur/graph-index.pointer.json  (existing pointer schema, with canonical_artifact_path
                                 now ending in `.parquet/` instead of `.json`)
```

**Rationale.** Bytes live in git internals (`.git/spur-graph/artifacts/...`), where `store/cache.rs` already content-addresses and GC's them. `.spur/graph/CURRENT` exposes a stable, discoverable path. One source of truth (the git-internal directory), one user-facing handle.

**Multi-worktree caveat (deferred risk; FU-H).** Git worktrees share `.git/spur-graph/artifacts/...` only when `core.worktree` and the GIT_DIR layout permit it. Documented in §13.

## 6. Parquet schemas

All files: ZSTD level 3. Row group size **decided pre-PR2 by benchmark** (see §12) over 16K/32K/64K against DuckDB and Rust reads on the live SPUR fixture. The 65 536 default is a placeholder pending that bench.

Schemas mirror Rust struct types exactly. Column names match Rust field names. Nullability matches `Option<T>` / `T`. Numeric widths match Rust types (no widening; `f32` stays `Float32`).

### Endpoint node-id namespace (applies to §6.1, §6.2, §6.3, §6.4, §6.5)

There is **one** integer node-id namespace, owned by the extractor: `NodeId(u64)` (`crates/spur-graph/src/identity.rs:25`). Every node — files **and** symbols — has a NodeId in this shared space. The on-disk schema preserves these extractor-assigned IDs exactly; the writer does NOT re-enumerate.

| Entity | Identifying string | Identifying integer |
|---|---|---|
| Symbol | `stable_symbol_id` (Utf8) | `node_id` (Int64; the extractor's `NodeId(u64).0`) |
| File | `stable_file_id` (Utf8) | `node_id` (Int64; same NodeId namespace as symbols) |

Edge endpoint columns (`source_stable_id`, `target_stable_id`, `src_id`, `dst_id`) are **polymorphic** — they may refer to either a symbol or a file. Resolution unions `nodes.parquet` and `files.parquet`. The DuckDB MCP server provides a `endpoints` view that exposes the union:

```sql
CREATE OR REPLACE VIEW endpoints AS
SELECT stable_symbol_id AS stable_id, node_id, 'symbol' AS endpoint_kind, file_path FROM nodes
UNION ALL
SELECT stable_file_id  AS stable_id, node_id, 'file'   AS endpoint_kind, file_path FROM files;
```

Dense integer IDs needed by graph algorithms (e.g. Onager's `(src, dst) BIGINT` requirement) are derived at query time over `endpoints`, not materialized on disk. The POC's `node_ids` table (`crates/spur-context/poc/duckdb-analyst/init.sql`) demonstrates the runtime mapping.

### 6.1 `nodes.parquet`

Mirrors `GraphSymbolArtifact` (`crates/spur-graph/src/schema.rs:146`).

| Column | Arrow type | Encoding | Nullable | Notes |
|---|---|---|---|---|
| `stable_symbol_id` | Utf8 | PLAIN | no | hex VARCHAR |
| `node_id` | Int64 | PLAIN | no | The extractor's `NodeId(u64).0` for this symbol. **Same namespace as `files.node_id` and `file_manifests.node_ids`.** Not re-enumerated. |
| `file_path` | Utf8 | DICTIONARY | no | ~1.5k distinct vs. 27k rows |
| `byte_range_start` | Int64 | PLAIN | no | `byte_range[0]`; `SourceRange` is always present |
| `byte_range_end` | Int64 | PLAIN | no | `byte_range[1]` |
| `line_start` | Int32 | PLAIN | no | `line_range[0]` |
| `line_end` | Int32 | PLAIN | no | `line_range[1]` |
| `entity_name` | Utf8 | DICTIONARY | no | `String` in Rust |
| `qualified_name` | Utf8 | DICTIONARY | no | `String` with `#[serde(default)]` |
| `symbol_kind` | Utf8 | DICTIONARY | no | NodeKind discriminator |
| `anchor_hash` | Utf8 | PLAIN | no | `String` |
| `enclosing_scope` | Utf8 | DICTIONARY / PLAIN | yes | Encoding decided in pre-PR2 bench: DICTIONARY if cardinality ratio ≤ 0.5, else PLAIN |

**Sort key: `(file_path ASC, stable_symbol_id ASC)`.** Enables future predicate-pushdown incremental loads keyed on changed files (FU-D).

### 6.2 `edges.parquet`

Mirrors `GraphEdgeArtifact` (`crates/spur-graph/src/schema.rs:163`). Resolved edges only (rows where `target_stable_symbol_id` is `Some`). Unresolved edges go to `edges_unresolved.parquet` (§6.4).

| Column | Arrow type | Encoding | Nullable | Notes |
|---|---|---|---|---|
| `source_stable_id` | Utf8 | DICTIONARY | no | The Rust field `source_stable_symbol_id` is misleadingly named — it may carry either `stable_symbol_id` OR `stable_file_id` (e.g. for `Contains` edges from a file to its symbols). The Parquet column drops the `_symbol` suffix to reflect the polymorphism. |
| `target_stable_id` | Utf8 | DICTIONARY | no | Resolved edges only; never null in this file. |
| `src_id` | Int64 | PLAIN | no | Extractor NodeId; lookup space is `nodes ∪ files`. |
| `dst_id` | Int64 | PLAIN | no | Same. |
| `target_label` | Utf8 | DICTIONARY | yes | `Option<String>` |
| `relation` | Utf8 | DICTIONARY | no | `RelationKind` discriminator |
| `confidence` | Utf8 | DICTIONARY | no | `Confidence` discriminator |
| `confidence_score` | Float32 | PLAIN | no | `f32` in Rust; **not widened** — preserves bit-equivalence (round-trip identity is exact for finite values; Family 1.1 uses NaN/inf-aware comparison) |
| `edge_kind` | Utf8 | DICTIONARY | yes | `Option<GraphEdgeKind>` |

**Sort key: `(src_id ASC, dst_id ASC)`.** Enables row-group pruning for **callee** lookups (`WHERE src_id = X` returns the edges going *out* of X). For **caller** lookups (`WHERE dst_id = X`), see §6.3.

### 6.3 `edges_by_dst.parquet` (bench-gated)

Same row set and same schema as `edges.parquet`, sorted by `(dst_id ASC, src_id ASC)`. **Inclusion in the on-disk layout is contingent on the pre-PR2 benchmark (§12) showing it beats lazy DuckDB dst-sort over `edges.parquet` materially.** If the bench shows lazy materialization is competitive, this file is dropped and the DuckDB MCP's `edges_by_dst` view computes the sort at query time.

If included, the writer emits both `edges.parquet` and `edges_by_dst.parquet` in one pass (sort once for src, re-sort once for dst, ~5 ms additional at current scale). Adds ~3–6 MB on-disk at current scale.

If excluded, §11.4 (predicate-pushdown partial load) and the DuckDB MCP's caller-lookup view rely on lazy dst-sort. No bytes lost; round-trip unaffected (the file is purely a derived index).

The pre-PR2 bench compares:
1. **Materialized:** `WHERE dst_id = X` against `edges_by_dst.parquet` directly.
2. **Lazy:** `WHERE dst_id = X` against a DuckDB view that does `SELECT * FROM edges ORDER BY dst_id` lazily (DuckDB's adaptive index may amortize across queries).

Decision rule: materialize if cold-query latency improves ≥2× **and** the +50% on-disk edge cost is acceptable given projected graph growth.

### 6.4 `edges_unresolved.parquet`

Only those edges where the target did not resolve to a known symbol (~59% of edges in today's SPUR graph).

| Column | Arrow type | Encoding | Nullable |
|---|---|---|---|
| `source_stable_id` | Utf8 | DICTIONARY | no |
| `src_id` | Int64 | PLAIN | no |
| `target_label` | Utf8 | DICTIONARY | yes |
| `relation` | Utf8 | DICTIONARY | no |
| `confidence` | Utf8 | DICTIONARY | no |
| `confidence_score` | Float32 | PLAIN | no |
| `edge_kind` | Utf8 | DICTIONARY | yes |

Sort key: `src_id ASC`. Same polymorphic-endpoint semantics as §6.2.

### 6.5 `files.parquet`

Mirrors `GraphFileArtifact` (`crates/spur-graph/src/schema.rs:105`) plus the extractor's NodeId for endpoint resolution.

| Column | Arrow type | Encoding | Nullable | Notes |
|---|---|---|---|---|
| `stable_file_id` | Utf8 | PLAIN | no |  |
| `node_id` | Int64 | PLAIN | no | The extractor's `NodeId(u64).0` for this file. Same namespace as `nodes.node_id`. Files participate as edge endpoints (e.g. `Contains` edges from file → symbol). |
| `file_path` | Utf8 | DICTIONARY | no |  |

Sort key: `file_path ASC`.

Note: `node_id` for files is not part of the in-memory `GraphFileArtifact` struct today; it is recovered by the writer from the `GraphFacts.nodes` lookup table during materialization. On round-trip, the reader discards it back into the `GraphFileArtifact` view (which doesn't carry it). The column exists solely so polymorphic edges resolve cleanly at query time.

### 6.6 `file_manifests.parquet`

Mirrors `GraphFileManifestEntry` (`crates/spur-graph/src/schema.rs:112`).

| Column | Arrow type | Encoding | Nullable | Notes |
|---|---|---|---|---|
| `stable_file_id` | Utf8 | PLAIN | no |  |
| `path` | Utf8 | DICTIONARY | no |  |
| `content_oid` | Utf8 | PLAIN | no | `String`, not `Option<String>` |
| `node_ids` | List(Int64) | PLAIN | no | Per-file symbol NodeIds in the **same namespace as `nodes.node_id`** (the extractor's `NodeId(u64).0`). Eagerly stored; round-trips byte-equivalent with `Vec<NodeId>`. |

### 6.7 `tombstones.parquet`

Mirrors `GraphTombstoneEntry { path: String, stable_file_id: String }`.

| Column | Arrow type | Encoding | Nullable |
|---|---|---|---|
| `path` | Utf8 | DICTIONARY | no |
| `stable_file_id` | Utf8 | PLAIN | no |

Sort key: `path ASC`.

### 6.8 `manifest.json`

Small JSON metadata file inside each `<hash>.parquet/` directory. **Written last; its presence is the sentinel that the directory is complete (see §6.9).**

```json
{
  "graph_index_version": "spur-graph-phase2",
  "schema_version": "spur-graph-schema-v5",
  "manifest_version": "<sha256 of query bytes>",
  "graph_content_hash": "<blake3 hex>",
  "indexed_commit_oid": "<git oid or null>",
  "extractor_version": "<EXTRACTOR_VERSION>",
  "complete": true,
  "row_counts": {
    "nodes": 27559,
    "edges": 47169,
    "edges_by_dst": 47169,
    "edges_unresolved": 68070,
    "files": 1548,
    "file_manifests": 1548,
    "tombstones": 0
  },
  "parquet_writer": {
    "compression": "zstd-3",
    "row_group_size": 65536
  },
  "edges_by_dst_present": true
}
```

`complete: true` and `edges_by_dst_present` are read by the resolver / reader to validate atomicity and known-layout. `read_artifact_header_parquet` (§7) returns the parsed manifest as a new `GraphArtifactManifest` struct (distinct from `GraphIndexHeader`, which only carries `graph_index_version` and `content_hash_blake3` today).

### 6.9 Atomic directory publication

A multi-file Parquet directory cannot be atomically created with a single syscall, but its **observable state** can be made atomic by treating `manifest.json` as the completion sentinel.

Writer procedure:

1. `mkdir <base_dir>/<hash>.parquet.tmp.<pid>`
2. Write every `*.parquet` file inside the temp directory. Each individual Parquet file's writer call already does its own internal temp-then-rename.
3. `fsync` each `*.parquet` file.
4. Write `manifest.json` LAST. `fsync` it.
5. `fsync` the temp directory.
6. `rename(<...>.parquet.tmp.<pid>, <...>.parquet)` — atomic on POSIX for same-filesystem renames.
7. `fsync` the parent directory.

Reader / resolver procedure:

1. `resolve_artifact_location` finds a candidate `<hash>.parquet/` directory.
2. Validate `manifest.json` exists, parses, and `complete == true`. If absent or `complete: false`, treat as a partial write — log an error and refuse to load. Caller falls through to legacy JSON or a clean rebuild.
3. Validate `manifest.json.row_counts` against each `*.parquet` footer's row count before deserializing.

Crash recovery: on next `graph build`, `store::cache` scans `<base_dir>` for `*.tmp.*` directories older than a threshold (e.g. 1 hour) and deletes them. Same-process crash mid-write leaves a `.tmp.<pid>` directory that the next clean rebuild reclaims.

## 7. Surface API

New public API on `spur_graph` (re-exported from `store/parquet.rs`):

```rust
/// Writes the artifact as a Parquet directory inside `base_dir` using the
/// atomic-publication protocol of §6.9. Emits edges.parquet and (if
/// configured per pre-PR2 bench result) edges_by_dst.parquet in one pass.
pub fn write_artifact_parquet(
    artifact: &GraphIndexArtifact,
    base_dir: &Path,
    options: WriteOptions,  // includes whether to emit edges_by_dst
) -> anyhow::Result<PathBuf>;

/// Reads a Parquet directory back into a GraphIndexArtifact byte-equivalent
/// to the one that was written. Refuses to load directories whose manifest.json
/// is missing or has `complete: false`.
pub fn read_artifact_parquet(dir: &Path) -> anyhow::Result<GraphIndexArtifact>;

/// Reads only manifest.json into a GraphArtifactManifest struct (~sub-millisecond).
/// Used by spur-tui for cache validation without paying full artifact decode.
/// GraphArtifactManifest is a NEW type, distinct from GraphIndexHeader, holding
/// the full manifest contents (counts, schema_version, indexed_commit_oid, etc.).
pub fn read_artifact_header_parquet(dir: &Path) -> anyhow::Result<GraphArtifactManifest>;

/// Updates the .spur/graph/CURRENT pointer to point at the given hash directory.
pub fn write_current_pointer(worktree_root: &Path, hash_dir: &Path) -> anyhow::Result<()>;

/// Reads .spur/graph/CURRENT and returns the absolute path of the live
/// Parquet directory.
pub fn read_current_pointer(worktree_root: &Path) -> anyhow::Result<PathBuf>;

/// Shared resolver: every reader in spur-mcp, spur-tui, spur-cli goes
/// through this helper. Returns the format-tagged location after applying
/// the precedence rules below. Crucially, business logic in callers
/// NEVER matches on ArtifactLocation directly — they get back a
/// validated path and the resolver caches by `manifest.json.graph_content_hash`
/// (for Parquet) or `(path, mtime)` (for legacy JSON).
pub fn resolve_artifact_location(
    worktree_root: &Path,
    explicit_override: Option<&Path>,
) -> anyhow::Result<ResolvedArtifact>;

pub struct ResolvedArtifact {
    pub path: PathBuf,
    pub format: ArtifactFormat,        // LegacyJson | Parquet
    pub cache_key: ArtifactCacheKey,
}

pub enum ArtifactCacheKey {
    LegacyJson { path: PathBuf, mtime: SystemTime },
    Parquet    { graph_content_hash: String },
}
```

### Resolver precedence

When multiple potential artifact locations exist, the resolver picks the first that passes validation, in this order:

| Priority | Input | Treated as |
|---|---|---|
| 1 | `explicit_override` argument (e.g. test fixtures, `SPUR_CODE_GRAPH_INDEX` env var) | Whichever format the path points at (file → JSON; directory → Parquet) |
| 2 | `.spur/graph/CURRENT` (new) | Parquet |
| 3 | `.spur/graph-index.pointer.json` → `canonical_artifact_path` (existing pointer) | Whichever format the path points at |
| 4 | `.spur/graph-index.json` (legacy worktree-root JSON) | Legacy JSON |

A location is **valid** if:
- LegacyJson: the file exists and parses as `GraphIndexArtifact`.
- Parquet: the directory exists, `manifest.json` is present, parses, and `complete == true`.

The resolver logs the priority level that won and what it skipped, so debugging is trivial when multiple stale locations co-exist.

`load_artifact(path: &Path)` in `schema.rs` is updated to call `resolve_artifact_location` with `explicit_override = Some(path)` and dispatch to the appropriate loader. Callers that pass a directory get Parquet; callers that pass a file get the legacy reader with a deprecation warning.

`write_artifact` (the JSON full-artifact writer) is marked `#[deprecated]` at PR3b and removed at PR3c.

## 8. `store/` module refactor

```
crates/spur-graph/src/store/
├── mod.rs            ─ re-exports
├── build.rs          ─ NEW. Receives artifact_from_facts, artifact_from_facts_incremental,
│                       buckets_from_facts, compose_artifact, anchor_hash,
│                       qualified_name, and the ~1700 lines of construction logic
│                       currently misfiled in json.rs.
├── canonical_hash.rs ─ NEW (renamed from json.rs at PR3c). ~30 lines:
│                       GraphArtifactBodyForHash + artifact_content_hash_blake3_hex.
├── pointer.rs        ─ NEW. Pointer-file and CURRENT I/O; hosts resolve_artifact_location.
├── parquet.rs        ─ NEW. write_artifact_parquet, read_artifact_parquet,
│                       read_artifact_header_parquet, schemas, RecordBatch builders,
│                       atomic publication protocol (§6.9).
├── cache.rs          ─ MODIFIED. Writes Parquet directory instead of JSON file
│                       (PR3b). Dedup by content hash unchanged. Crash-recovery
│                       sweep for *.tmp.* directories (§6.9).
└── snapshot.rs       ─ unchanged.
```

Cargo:

```toml
[dependencies]
parquet      = { workspace = true, default-features = false, features = ["zstd"] }
arrow-array  = { workspace = true }
arrow-schema = { workspace = true }
```

**No Cargo feature flag.** Parquet is the only on-disk format post-cutover. All three crates are pure Rust, workspace-pinned. No native linkage; no libduckdb anywhere.

## 9. Migration sequence

bd-1rqxk becomes the parent epic. Each PR is independently mergeable.

### PR1 — Split `store/json.rs` (mechanical)
Move ~1700 lines of construction logic from `json.rs` into `build.rs`. `mod.rs` re-exports unchanged. Zero behavior delta.

### PR2 — Add `store/parquet.rs`
Implement the §6 + §6.9 surface. Pre-PR2 benchmark (§12) resolves:
- Row-group size (16K/32K/64K),
- `enclosing_scope` encoding (DICTIONARY vs PLAIN),
- `edges_by_dst.parquet` (materialize on disk vs lazy DuckDB dst-sort).

No caller changes; both writers coexist. Round-trip identity test for the writer/reader pair (Family 1.1, §12). **Performance baselines** (current JSON `write_artifact` and `load_artifact` wall-clock + peak RSS) are captured into a committed JSON file (`crates/spur-graph/benches/baselines.json`) so they survive PR3b's removal of the JSON writer.

### PR3a — Reader tolerance (new, additive, low-risk)
`load_artifact` and the new `resolve_artifact_location` accept **both** formats per §7 precedence. JSON writer remains alive. Every reader in `spur-mcp`, `spur-tui`, `spur-cli` is migrated to call the resolver. Test matrix (Family 1.4) covers every current caller path.

At the end of PR3a, the workspace can **read** Parquet but still **writes** JSON. Safe to merge independently.

### PR3b — Writer flip (cutover; the only step that changes write behavior)
`cache.rs` writes Parquet directories using the §6.9 atomic protocol. Pointer file `canonical_artifact_path` references `.parquet/`. `write_artifact` (JSON) is marked `#[deprecated]` but remains callable so any straggler caller produces a compile warning, not a build failure. Family 3 performance gates (§12) must pass against pre-PR3a baselines before merge.

### PR3c — Cleanup (separable; rollback-safe)
Delete `write_artifact`, rename `json.rs` → `canonical_hash.rs`, drop the deprecation. Trivially revertable if needed; reverting PR3c alone restores `write_artifact` without affecting the writer flip.

### PR4 — DuckDB MCP enablement (the bd-1rqxk capability)
Adds `crates/spur-context/src/sql/schema_code_graph.sql` (views over Parquet, including the `endpoints` UNION view in §6), the DuckDB MCP server crate or wiring, the `data-analyst` brain skill, and the smoke test from bd-1rqxk's original ACs. **MCP views expose `stable_symbol_id` by default.** Dense integer IDs needed by Onager are derived at query time inside an internal view; tool outputs visible to the brain do not include raw `node_id` columns unless the user explicitly opts in. Depends on PR1–PR3b being merged.

## 10. Content hash flow

The post-cutover hash flow is byte-identical to today's:

1. `GraphArtifactBodyForHash { files, symbols, edges, file_manifests, graph_content_hash, manifest_version, tombstones }` — **the field declaration order in this struct IS the canonical serialization order.** Any reorder silently changes every hash fleet-wide.
2. `serde_json::to_vec(&body)` — canonical, sorted-key serialization performed **in memory only**.
3. `blake3::hash(canonical_bytes).to_hex()`.

The function lives in `canonical_hash.rs` (renamed at PR3c). A **field-order guard test** (Family 1.2, §12) commits both the hash hex *and* a serde-snapshot of the canonical bytes from a minimal artifact.

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
1. `resolve_artifact_location(&worktree_root, None)` → `<hash>.parquet/` directory.
2. `read_artifact_parquet(dir)` → in-memory `GraphIndexArtifact`.
3. `artifact_from_facts_incremental(...)` — **unchanged.**
4. `write_artifact_parquet(&merged, &base_dir, options)` → new `<hash>.parquet/` via §6.9.
5. `write_current_pointer(&worktree_root, &new_hash_dir)`.

### 11.3 `file_manifests.node_ids` round-trip

Per §6.6, `file_manifests.parquet` carries `node_ids LIST<INT64>` eagerly, in the **same NodeId namespace** as `nodes.node_id` and `files.node_id`. The reader populates `GraphFileManifestEntry.node_ids: Vec<NodeId>` directly from the column — no remap, no JOIN. Round-trip is byte-equivalent.

### 11.4 Deferred optimization B — predicate-pushdown partial load (FU-D)

When only a small fraction of files change, the full round-trip becomes wasteful. A future optimization introduces `read_artifact_partial(dir, changed_file_paths)` that leverages `nodes.parquet`'s `(file_path, stable_symbol_id)` sort key for row-group pruning. Tracked as FU-D.

### 11.5 `node_id` semantics & cross-build stability

`node_id` in `nodes.parquet` and `files.parquet` IS the extractor's `NodeId(u64).0`. This NodeId is content-determined by the extractor's identity rules (`crates/spur-graph/src/identity.rs` and the content-hashing logic): the same `(file_path, stable_*_id)` reliably maps to the same NodeId across rebuilds as long as the extractor schema and identity rules are unchanged.

**`node_id` IS suitable for cross-build joins against `file_manifests.node_ids` within a stable extractor schema.** It is the in-memory NodeId, not an export-time dense enumeration.

**`node_id` IS NOT a dense integer suitable for graph algorithms** (Onager's `(src, dst) BIGINT` requirement assumes contiguous values starting from 0). When the DuckDB MCP runs a centrality / community algorithm, it builds a dense space at query time via a window function over the `endpoints` view (§6 endpoint namespace). The dense IDs are **per-query**, never persisted, and never surfaced to the brain.

**Brain-facing rule (enforced by the `data-analyst` skill in PR4):** result rows returned to the brain reference `stable_symbol_id` / `stable_file_id`. The raw `node_id` column is not exposed by default; PR4's DuckDB MCP views hide it behind an internal view. If the brain materializes a result set and reuses it later, it joins back to `endpoints.stable_id` — which IS stable across builds at the content-hash level. This converts §11.5 from a discipline rule (v2) into a view-layer enforcement (v3).

## 12. Test plan

### Pre-PR2 benchmark (input to PR2 decisions)

Bench on the SPUR fixture (~27.5k nodes, 47k edges) and a synthetic 10× fixture:
- **Row-group size:** 16384, 32768, 65536. Decide on smallest size whose on-disk size is within 10% of 65536 and whose DuckDB cold-query latency wins on `WHERE src_id = X`.
- **`enclosing_scope` encoding:** DICTIONARY vs PLAIN. Measure file size + Rust read latency. Pick DICTIONARY only if dict ratio ≤ 0.5.
- **`edges_by_dst.parquet` materialization vs lazy DuckDB dst-sort:** measure cold-query latency for `WHERE dst_id = X` under both. Materialize only if ≥2× improvement and the +50% on-disk edge size is acceptable.

Results are committed to `crates/spur-graph/benches/pre_pr2.md` with a decision summary at the top. PR2 code matches the decisions.

### Pre-PR3a baseline capture (preserves comparison anchor)

Before PR3a touches reader code, capture current JSON `write_artifact` and `load_artifact` wall-clock medians (N=10) and peak RSS on the SPUR fixture into `crates/spur-graph/benches/baselines.json`. Committed and immutable; survives PR3b's removal of the JSON writer.

### Family 1 — Correctness (mandatory)

**1.1 Round-trip identity.** A representative `GraphIndexArtifact` is written via `write_artifact_parquet`, read back via `read_artifact_parquet`, and compared field-by-field. `confidence_score: f32` comparison uses bit-pattern equality (`f32::to_bits`) to handle NaN/inf precisely. All other fields use derived `PartialEq`.

**1.2 Hash stability + field-order guard.** `artifact_content_hash_blake3_hex` snapshot test against the fixture, PLUS a separate snapshot of `serde_json::to_vec(&body)` canonical bytes from a minimal artifact (catches `GraphArtifactBodyForHash` field reorders before they invalidate user pointers).

**1.3 Incremental merge integrity.** Existing `spur-graph/tests/extractor.rs` suite runs against Parquet-loaded prev-artifacts. `artifact_from_facts_incremental` unchanged.

**1.4 Resolver test matrix (PR3a).** For every combination of (`explicit_override` set / unset) × (`.spur/graph/CURRENT` exists / not) × (pointer file exists / not) × (legacy `.spur/graph-index.json` exists / not), assert `resolve_artifact_location` picks the correct precedence rule. Covers all reader call sites in `spur-cli`, `spur-tui`, `spur-mcp`.

**1.5 Partial-write rejection.** Construct a `<hash>.parquet.tmp.<pid>/` directory with all Parquet files present but no `manifest.json`. Assert `read_artifact_parquet` and the resolver refuse to load it. Construct one with `manifest.json: { "complete": false }`. Same assertion.

### Family 2 — Schema invariants (mandatory)

**2.1 Sort order.** Read `nodes.parquet`, assert sorted by `(file_path, stable_symbol_id)`. Read `edges.parquet`, assert `(src_id, dst_id)`. If present, read `edges_by_dst.parquet`, assert `(dst_id, src_id)`. Read `edges_unresolved.parquet`, assert `src_id`.

**2.2 Dictionary encoding present.** Read Parquet footer; assert `symbol_kind`, `relation`, `confidence`, `file_path`, `qualified_name`, `edge_kind` use DICTIONARY. `enclosing_scope` is one of {DICTIONARY, PLAIN} per pre-PR2 decision.

**2.3 Compression.** Footer-level assert: ZSTD on every column chunk.

**2.4 DuckDB round-trip (hard CI gate).** `duckdb` CLI is added to the CI image. Test spawns it, runs `SELECT COUNT(*) FROM read_parquet(...)` against each output file, compares to in-Rust row counts.

**2.5 Edge dual-write consistency (only when `edges_by_dst.parquet` is materialized).** Assert `edges_by_dst.parquet` and `edges.parquet` contain identical row multisets (modulo sort order). Catches dual-write drift on partial-write failure.

**2.6 Endpoint namespace consistency.** For every row in `edges.parquet`, assert `src_id` exists in `nodes.parquet.node_id ∪ files.parquet.node_id`, and same for `dst_id`. For every NodeId in `file_manifests.node_ids`, assert it exists in `nodes.parquet.node_id` (file manifests reference symbols, not files).

### Family 3 — Performance gates (hard CI gates at PR3b merge)

Each gate is asserted against the baselines captured pre-PR3a. Numbers are wall-clock medians of N=10 runs on the CI runner, peak RSS via `getrusage`. Correctness CI and perf CI run on separate jobs so perf-noise flakiness doesn't gate merges of unrelated work.

| Gate | Path | Threshold | Notes |
|---|---|---|---|
| 3.1 | `write_artifact_parquet` (incl. §6.9 atomic dance) | ≤ 2.0× baseline `write_artifact` (JSON) wall-clock | Accepted doubled write cost. |
| 3.2 | `read_artifact_parquet` | ≤ 0.5× baseline `load_artifact` (JSON) wall-clock | Headline reader win. |
| 3.3 | `read_artifact_parquet` peak RSS | ≤ baseline `load_artifact` peak RSS | No regression. |
| 3.4 | Full incremental build (load + merge + write), unchanged repo | ≤ 0.8× baseline incremental build wall-clock | End-to-end. |
| 3.5 | DuckDB cold first-query over `read_parquet(...)` | ≤ 1.5× POC median (commit 754b07a8) | Provenance: numbers derived from the POC's measured timings, not pulled from thin air. |
| 3.6 | DuckDB peak RSS during ingestion | ≤ 500 MB | The OOM that motivated the migration stays buried; threshold is the smallest power-of-two ceiling above POC measurements. |

Bench code lives in `crates/spur-graph/benches/parquet.rs`; the gates are a separate integration test reading the bench output and comparing to `baselines.json` + POC measurements.

### Out of scope

- Property-test fuzzing of arbitrary artifact shapes.
- Cross-tool compatibility matrix (Polars, Spark, pyarrow). DuckDB is the only consumer; Family 2.4 covers it.

## 13. Risks & rollback

**Risk: arrow-rs Parquet writer is not bit-for-bit deterministic across versions.**
Page boundaries, dictionary ordering can shift on a `parquet` crate upgrade. The content hash flow is unaffected (hashes canonical-JSON in-memory, not Parquet bytes). Family 1.2 guards the canonical form.

**Risk: Half-written `<hash>.parquet/` directory observed by a reader.**
Mitigation: §6.9 atomic publication protocol; `manifest.json.complete == true` is the sentinel. Family 1.5 tests the rejection path. Crash-recovery sweep of stale `*.tmp.*` directories in `store::cache::init`.

**Risk: A reader missed in PR3a runs against a path layout it does not understand.**
Mitigation: every reader migrates through `resolve_artifact_location`. Family 1.4 covers every current caller. PR3a is the high-risk step; PR3b only flips the writer after PR3a has been in main one cycle. Any remaining caller fails through the resolver with a clear error message.

**Risk: PR3b multi-step rollback (writer flip + cleanup) was the v2 design flaw.**
Mitigation in v3: PR3b is writer-flip-only (with `#[deprecated]` on `write_artifact` but not removal). PR3c is separable cleanup. Reverting PR3c alone restores `write_artifact` cleanly. Reverting PR3b restores JSON write while leaving the legacy JSON reader (from PR3a) functional.

**Risk: Existing `.spur/graph-index.pointer.json` files in user worktrees reference a `.json` artifact path that no longer exists after PR3b.**
Mitigation: legacy-JSON-read fallback from PR3a remains live. On next `graph build`, the cache code rebuilds against the same content hash and writes Parquet; the pointer is updated.

**Risk: A downstream tool reads `.spur/graph-index.json` directly.**
Mitigation: legacy JSON reader retained through PR3c and one further release. Deprecation warning at every load. Removal scheduled separately (FU-G).

**Risk: Multi-worktree git layouts make `.spur/graph/CURRENT` resolve to a `.git/spur-graph/artifacts/...` path the secondary worktree's GIT_DIR cannot reach.**
Mitigation: documented as a known limitation. Follow-up FU-H if SPUR formalizes multi-worktree workflows.

**Risk: `node_id` instability silently corrupts long-lived consumers.**
Resolved in v3: `node_id` IS the extractor's `NodeId(u64).0`, content-stable across rebuilds modulo extractor-schema changes (§11.5). Dense integer IDs needed by Onager are query-time only, never persisted, never brain-visible. PR4's view-layer enforcement makes this a type-system property, not a discipline rule.

**Risk: Endpoint domain mismatch (the largest v2-spec error).**
Resolved in v3: `nodes.parquet` and `files.parquet` share the NodeId namespace; the `endpoints` view (§6) unions them; edge `src_id`/`dst_id` resolve cleanly for both symbol and file endpoints (e.g. `Contains` edges).

**Rollback.**
- After PR3a: trivial revert. Reader-tolerance is purely additive.
- After PR3b: revert PR3b alone. PR3a's legacy-JSON read fallback is still in place; the workspace gracefully reads either format. JSON writer comes back via revert.
- After PR3c: trivial revert. Restores `write_artifact` (it's a delete-only PR).

## 14. Open follow-ups (tracked, not in this spec)

| ID | Item | Trigger |
|---|---|---|
| FU-A | Load DuckPGQ extension in the MCP server | ≥10 brain queries hand-rolling recursive CTEs that PGQ MATCH would express |
| FU-B | Load Onager extension in the MCP server | ≥10 brain queries attempting centrality/community via recursive SQL |
| FU-C | Apache GraphAr layering on top of Parquet | Kùzu's GraphAr loader stabilizes, or a Spark/GraphScope batch job is on the deck |
| FU-D | Predicate-pushdown partial load (§11.4) | Measured slowness on incremental builds where ≤5% of files change |
| FU-E | Arrow IPC dual-write for mmap zero-copy | An in-process Rust reader pins load latency below what Parquet decode allows |
| FU-F | Drop canonical-JSON hashing in favor of stable struct hash | A `SCHEMA_VERSION` bump that already invalidates pointers, making the hash basis swap free |
| FU-G | Sweep orphaned legacy `<hash>.json` artifacts | One release of field data after PR3c without regression |
| FU-H | Multi-worktree-safe `.spur/graph/CURRENT` resolution | SPUR formalizes multi-worktree workflows |
| FU-I | Promote `edges_by_dst.parquet` materialization | Pre-PR2 bench results favored lazy DuckDB dst-sort; revisit if inbound-lookup latency degrades at 10× scale |

## 15. References

- bd-1rqxk — tracking issue with all design amendments.
- `crates/spur-context/poc/duckdb-analyst/` (commit 754b07a8) — POC; measured DuckDB numbers feed §12 Family 3 thresholds.
- `docs/reviews/2026-05-21-parquet-exporter-review-claude-code.md` — v1 review.
- `docs/reviews/2026-05-21-parquet-exporter-review-codex.md` — v1 review.
- `docs/reviews/2026-05-22-parquet-exporter-v2-review-claude-code.md` — v2 review.
- `docs/reviews/2026-05-22-parquet-exporter-v2-review-codex.md` — v2 review.
- DuckDB documentation: `read_parquet`, predicate pushdown, row group statistics.
- DuckPGQ — Property Graph documentation, VLDB 2023 paper.
- Onager — graph analytics extension for DuckDB.
- Apache GraphAr — Format Specification, incubating project.
- arrow-rs and parquet crates — pure-Rust implementations under `arrow-rs` project.
