# Spur Code Context Service — Design

**Date:** 2026-06-22
**Status:** Approved (brainstormed; pending implementation plan)
**Scope:** New crate `crates/spur-context-service`, AWS infrastructure (Lambda, Spot Fleet, RDS, S3), DuckLake catalog

## Problem

SPUR's `spur-graph` and `spur-analyst` serve workspace-internal code context — the project's own code graph, extracted via tree-sitter, queried via DuckDB MCP tools. Coding agents using SPUR can ask "where is this function defined?" or "what calls this?" within the current workspace.

When the agent needs to understand an **external package** (a crate from crates.io, an npm package, a git dependency), it falls back to web search, documentation sites, or guessing. There is no structured, version-precise, graph-aware code context for external dependencies.

Services like Context7 solve this for documentation but not for code structure. We need the equivalent for code: "given package X at version Y, show me the call graph, symbol definitions, and semantic search over the actual source."

## Goal

Build a **code context as a service** that indexes external packages using the existing `spur-graph` extraction pipeline and serves structured queries via an AWS Rust Lambda. The service provides MCP tools that coding agents call to get precise, version-scoped code context for any indexed package.

**Invariant:** Every `(source, package, revision)` that exists in the `package_catalog` has a complete DuckLake snapshot — structural tables (nodes, edges), text tables (section_bodies), and embedding tables (symbol_embeddings) — queryable in under 200ms from a warm Lambda.

## Architecture Overview

```
┌──────────────────────────────────────────────────────────────────────┐
│  BUILD LAYER (Spot Instances)                                         │
│                                                                       │
│  Trigger (SQS) ──→ Fetch source ──→ spur-cli graph build             │
│  (crates.io poll,    (tarball/git)   (tree-sitter + Jina embeddings)  │
│   git branch poll)                          │                         │
│                                       ┌─────▼──────┐                   │
│                                       │ Translate  │                   │
│                                       │ Lance→Parq │                   │
│                                       │ + DuckLake │                   │
│                                       │ snapshot   │                   │
│                                       └─────┬──────┘                   │
│                                             │                          │
│                    S3 (Parquet data) ◄──────┤                          │
│                    PostgreSQL (catalog) ◄────┘                          │
└──────────────────────────────────────────────────────────────────────┘
                                                                        │
┌──────────────────────────────────────────────────────────────────────┐
│  SERVE LAYER (AWS Lambda)                                            │
│                                                                       │
│  API Gateway ──→ Rust Lambda                                          │
│                   │                                                   │
│                   ├─ Catalog resolve: (package, ref) → revision       │
│                   │  PostgreSQL via RDS Proxy (~2-5ms)                │
│                   │                                                   │
│                   ├─ DuckDB query (embedded)                          │
│                   │  ATTACH DuckLake → SELECT from Parquet on S3      │
│                   │  Predicate pushdown on revision column (~20-100ms)│
│                   │                                                   │
│                   └─ JSON response (~1-5ms)                           │
│                                                                       │
│  Total: ~25-110ms per query (warm)                                    │
└──────────────────────────────────────────────────────────────────────┘
                                                                        │
┌──────────────────────────────────────────────────────────────────────┐
│  CONSUMER (Brain Agent)                                               │
│                                                                       │
│  MCP Client ──┬─→ spur-graph MCP (workspace, local process)          │
│               └─→ spur-context-service MCP (external, Lambda)         │
│                                                                       │
│  Agent routes: workspace symbols → local; pkg: selectors → Lambda     │
└──────────────────────────────────────────────────────────────────────┘
```

## Data Architecture

### Catalog: DuckLake

[DuckLake](https://github.com/duckdb/ducklake) is DuckDB Labs' open lakehouse format — a SQL catalog (PostgreSQL, SQLite, or MotherDuck) managing Parquet data files on object storage. It provides snapshot-based versioning (time travel), schema evolution, change data feed, and ACID catalog mutations.

**Why DuckLake over Iceberg/Delta:** The Lambda runs DuckDB as its embedded query engine. DuckLake is DuckDB-native — zero integration friction. The catalog DB is standard PostgreSQL (via RDS), the data is standard Parquet on S3. No additional services, no proprietary formats.

**Catalog DB:** PostgreSQL on RDS (db.r6g.large), accessed via RDS Proxy for Lambda connection pooling.

**Data storage:** Parquet files on S3 (`s3://spur-context/data/`), managed by DuckLake.

### Revision Model

Every indexed unit is identified by a **revision** — the package's version identity, which is either a semver string (registry packages) or a git commit SHA (git packages):

| Source Type | Revision Format | Example |
|---|---|---|
| Registry (crates.io, npm) | Semver string | `1.0.152` |
| Git dependency | Commit SHA | `a1b2c3d4e5f6...` |
| Git tag | Resolved to SHA | `tokio-1.35.0` → `def456...` |

Every row in every DuckLake table carries revision columns:

```
revision          STRING    — "1.0.152" (semver) | "a1b2c3d4..." (git SHA)
revision_kind     STRING    — "semver" | "git_sha"
semver_major      INT32     — null for git_sha
semver_minor      INT32     — null for git_sha
semver_patch      INT32     — null for git_sha
```

### DuckLake Table Schema

All tables live under a single DuckLake catalog (`spur_context`), partitioned by `(source, package)` so that all revisions of a given package cluster in the same S3 prefix — minimizing Lambda S3 read latency for per-package queries.

**Structural tables (translated from spur-graph parquet):**

```
nodes (
  stable_symbol_id   VARCHAR,
  package            VARCHAR,
  source             VARCHAR,          — "registry:crates-io" | "git:github.com/..."
  revision           VARCHAR,
  revision_kind      VARCHAR,
  semver_major       INTEGER,
  semver_minor       INTEGER,
  semver_patch       INTEGER,
  file_path          VARCHAR,
  byte_range_start   INTEGER,
  byte_range_end     INTEGER,
  line_start         INTEGER,
  line_end           INTEGER,
  entity_name        VARCHAR,
  qualified_name     VARCHAR,
  symbol_kind        VARCHAR,
  anchor_hash        VARCHAR,
  enclosing_scope    VARCHAR
)

edges (
  source_stable_id   VARCHAR,
  target_stable_id   VARCHAR,          — null for unresolved cross-package edges
  target_package     VARCHAR,          — populated for cross-package unresolved edges
  target_label       VARCHAR,          — populated for cross-package unresolved edges
  package            VARCHAR,
  source             VARCHAR,
  revision           VARCHAR,
  relation           VARCHAR,          — calls, contains, imports, etc.
  edge_kind          VARCHAR,          — calls, calls_dyn, references_hof, references_other
  confidence         VARCHAR,
  confidence_score   DOUBLE,
  bind_method        VARCHAR,
  receiver_text      VARCHAR,
  scope_text         VARCHAR
)

edges_unresolved (
  source_stable_id   VARCHAR,
  target_label       VARCHAR,
  target_package     VARCHAR,
  package            VARCHAR,
  source             VARCHAR,
  revision           VARCHAR,
  relation           VARCHAR,
  edge_kind          VARCHAR,
  confidence         VARCHAR,
  confidence_score   DOUBLE,
  bind_method        VARCHAR,
  receiver_text      VARCHAR,
  scope_text         VARCHAR
)

files (
  stable_file_id     VARCHAR,
  file_path          VARCHAR,
  package            VARCHAR,
  source             VARCHAR,
  revision           VARCHAR
)

file_manifests (
  stable_file_id     VARCHAR,
  path               VARCHAR,
  content_oid        VARCHAR,
  node_ids           VARCHAR[],
  package            VARCHAR,
  source             VARCHAR,
  revision           VARCHAR
)
```

**Text + embedding tables (translated from Lance sidecars):**

```
section_bodies (
  section_id         VARCHAR,
  package            VARCHAR,
  source             VARCHAR,
  revision           VARCHAR,
  file_path          VARCHAR,
  title              VARCHAR,
  body_text          VARCHAR,
  body_hash          VARCHAR,
  token_count        INTEGER
)

symbol_embeddings (
  stable_symbol_id   VARCHAR,
  package            VARCHAR,
  source             VARCHAR,
  revision           VARCHAR,
  file_path          VARCHAR,
  entity_name        VARCHAR,
  qualified_name     VARCHAR,
  symbol_kind        VARCHAR,
  embedding          FLOAT[768],
  embedding_model    VARCHAR,          — "JinaEmbeddingsV2BaseCode" (default)
  embedding_input_hash VARCHAR,
  embed_text_version VARCHAR            — "v2-jina-code"
)
```

**Temporal tables (git-sourced packages only):**

```
commits (
  sha                VARCHAR,
  parents            VARCHAR[],
  author_time        BIGINT,
  author_name        VARCHAR,
  author_email       VARCHAR,
  summary            VARCHAR,
  package            VARCHAR,
  source             VARCHAR
)

symbol_snapshots (
  stable_symbol_id   VARCHAR,
  commit             VARCHAR,
  package            VARCHAR,
  source             VARCHAR,
  file_path          VARCHAR,
  entity_name        VARCHAR,
  symbol_kind        VARCHAR,
  enclosing_scope    VARCHAR,
  byte_range         INTEGER[2],
  line_range         INTEGER[2],
  anchor_hash        VARCHAR
)

temporal_edges (
  source_endpoint    VARCHAR,          — serialized EdgeEndpoint
  target_endpoint    VARCHAR,
  relation           VARCHAR,
  change_kind        VARCHAR,          — Added, Deleted, RenamedFrom
  parent             VARCHAR,
  package            VARCHAR,
  source             VARCHAR
)
```

**Catalog metadata tables (managed by the build pipeline):**

```
package_catalog (
  source             VARCHAR,
  package            VARCHAR,
  revision           VARCHAR,
  revision_kind      VARCHAR,
  semver_major       INTEGER,
  semver_minor       INTEGER,
  semver_patch       INTEGER,
  snapshot_id        BIGINT,           — DuckLake snapshot containing this revision
  indexed_at         TIMESTAMP,
  index_status       VARCHAR,          — 'complete' | 'partial' | 'failed'
  embeddings_status  VARCHAR,          — 'complete' | 'pending' | 'skipped'
  row_counts         JSON,             — {nodes: N, edges: N, ...}
  PRIMARY KEY (source, package, revision)
)

refs (
  source             VARCHAR,
  package            VARCHAR,
  ref_name           VARCHAR,          — "latest", "main", tag names
  revision           VARCHAR,
  updated_at         TIMESTAMP,
  PRIMARY KEY (source, package, ref_name)
)
```

### Lance Elimination in the Service Layer

The current spur-graph produces Lance sidecars (`sections.lancedb` for BM25, `code_symbols.lance` for vector ANN). DuckLake catalogs Parquet files only, and DuckDB VSS HNSW persistence is explicitly experimental (not production-safe per DuckDB docs).

**Resolution (Path C):** Lance stays in spur-graph for workspace use (sub-millisecond ANN is valuable for the local brain agent). The service build pipeline translates Lance data to Parquet columns at build time. The Lambda never loads Lance — everything is DuckDB + Parquet.

| Capability | Workspace (spur-graph) | Service (spur-context-service) |
|---|---|---|
| BM25 search | Lance FTS index | DuckDB SQL macros over Parquet (proven in spur-analyst) |
| Vector search | Lance IVF+HNSQ (~1-5ms) | DuckDB `array_cosine_distance` brute-force (~10-50ms per package) |
| Data format | Parquet + Lance | Parquet only |
| Catalog | Filesystem pointer (`.spur/graph/CURRENT`) | DuckLake (PostgreSQL + S3 Parquet) |

### Cross-Package Edges

Edges to OTHER packages (e.g., serde calling `core::fmt::Formatter`) are stored as unresolved edges with `target_package` + `target_label` columns. The Lambda returns these as labeled-but-unresolved edges. If an agent wants to follow into the target package, it issues a follow-up query against that package's data.

This avoids O(packages²) cross-resolution at build time — each package is self-contained, inter-package links are lazy.

### Deduplication via Compression

A symbol unchanged across 194 versions of serde appears in 194 rows (one per revision). Parquet's columnar compression collapses the repeated `entity_name`, `qualified_name`, `source`, `byte_range` columns to near-zero marginal bytes. The `revision` column is dictionary-encoded to a few bits per row. Net storage for 194 versions of an unchanged symbol is ~2-3x one version, not 194x.

Parquet row-groups are sorted by `revision` so that a single revision's symbols cluster into 1-2 row groups. Parquet statistics (min/max per row group) enable S3 range requests to skip irrelevant row groups entirely.

## Build Pipeline

### Trigger Layer

| Source | Trigger | Latency | Mechanism |
|---|---|---|---|
| crates.io | New version published | ~5-10 min | Poll crates.io-index git repo every 5 min. Detect new entries, enqueue SQS job per `(crate, version)`. |
| crates.io | Version yanked | ~5 min | Same poll detects yank. Mark revision `yanked` in `package_catalog`. Data retained (time travel). |
| Git (tracked) | New commit on branch | ~15 min | Poll tracked branch tips every 15 min. If SHA changed, enqueue job. |
| Git (tagged) | New tag matching tracked pattern | ~15 min | Same poll detects matching tags. |
| Manual | Operator CLI | Immediate | `spur-context-service index --source ... --package ... --revision ...` |

SQS queue configuration:
- Visibility timeout: 15 min (max job duration)
- Dead-letter queue: 3 failed receives
- Spot Fleet consumes via long-polling

### Extract Stage (Spot Instance)

```
1. Fetch source
   registry: download tarball, extract to temp dir
   git: git clone --filter=blob:none, checkout at SHA/tag/branch

2. spur-cli graph build
   Produces standard spur-graph artifact:
     nodes.parquet, edges.parquet, edges_by_dst.parquet,
     edges_unresolved.parquet, files.parquet, file_manifests.parquet,
     tombstones.parquet, manifest.json
     sections.lancedb/ (doc section bodies + FTS index)
     code_symbols.lance/ (symbol embeddings + IVF+HNSQ index)

   Note: edges_by_dst.parquet (reverse edge index) is consumed during
   translation but NOT persisted as a separate DuckLake table. DuckDB
   handles reverse queries via predicate pushdown on the unified `edges`
   table (WHERE target_stable_id = ?). Parquet row-group statistics on
   target_stable_id enable efficient S3 range reads.
   
   git packages additionally:
     commits.parquet, symbol_snapshots.parquet, temporal_edges.parquet

3. Embedding generation (within graph build)
   Model: JinaEmbeddingsV2BaseCode (768-dim, code-specialized)
   Fallback: BGEBaseEnV15 (if Jina model unavailable)
   Embeddings land in code_symbols.lance sidecar
```

### Translate Stage (Same Spot Instance)

Reads the spur-graph artifact and writes to DuckLake tables:

```sql
-- Attach DuckLake catalog
ATTACH 'ducklake:postgresql://...' AS spur_context (DATA_PATH 's3://spur-context/data/');

-- Insert nodes with revision columns
INSERT INTO spur_context.nodes
SELECT
  stable_symbol_id,
  'serde' AS package,
  'registry:crates-io' AS source,
  '1.0.193' AS revision,
  'semver' AS revision_kind,
  1 AS semver_major, 0 AS semver_minor, 193 AS semver_patch,
  file_path, byte_range_start, byte_range_end,
  line_start, line_end, entity_name, qualified_name,
  symbol_kind, anchor_hash, enclosing_scope
FROM read_parquet('artifact/nodes.parquet');

-- Translate Lance sidecar → Parquet table
INSERT INTO spur_context.symbol_embeddings
SELECT
  stable_symbol_id,
  'serde' AS package, 'registry:crates-io' AS source, '1.0.193' AS revision,
  file_path, entity_name, qualified_name, symbol_kind,
  embedding::FLOAT[768] AS embedding,
  'JinaEmbeddingsV2BaseCode' AS embedding_model,
  embedding_input_hash,
  'v2-jina-code' AS embed_text_version
FROM lance_scan('artifact/code_symbols.lance');

-- Translate sections
INSERT INTO spur_context.section_bodies
SELECT
  section_id, 'serde' AS package, 'registry:crates-io' AS source,
  '1.0.193' AS revision, file_path, title, body_text, body_hash, token_count
FROM lance_scan('artifact/sections.lancedb');
```

### Commit Stage

```sql
-- DuckLake snapshot commit (atomic)
-- DuckLake auto-creates a snapshot on transaction commit

-- Update package_catalog
INSERT INTO spur_context.package_catalog VALUES (
  'registry:crates-io', 'serde', '1.0.193', 'semver',
  1, 0, 193,
  last_snapshot_id(),         -- DuckLake function for latest snapshot
  CURRENT_TIMESTAMP,
  'complete',                  -- index_status
  'complete',                  -- embeddings_status
  '{"nodes": 5234, "edges": 18234, "section_bodies": 89, "symbol_embeddings": 5234}'
);

-- Update refs (if this is the latest semver)
INSERT OR REPLACE INTO spur_context.refs VALUES (
  'registry:crates-io', 'serde', 'latest', '1.0.193', CURRENT_TIMESTAMP
);
```

### Spot Fleet Configuration

```
Instance type: r6a.xlarge (4 vCPU, 32GB RAM)
Spot allocation strategy: capacity-optimized
Max price: 60% of on-demand

Auto-scaling (SQS queue depth trigger):
  - Queue depth > 10 → scale up
  - Queue depth == 0 for 5 min → scale to zero
  - Min: 0, Max: 20

Spot interruption handling:
  - 2-minute warning → checkpoint job back to SQS → graceful shutdown
  - Job retried by next instance (idempotent — DuckLake detects duplicate revision inserts)
```

### Build Cost Estimates

| Metric | Estimate |
|---|---|
| Average crate extraction | 10-40s (tree-sitter + Jina embeddings) |
| Large crate (tokio, serde) | 2-5 min |
| Initial full crates.io index (~150k crates × ~10 versions) | ~1.5M jobs, 2-3 days with 20 spot instances |
| Steady-state (new publishes) | ~200-500 new versions/day, 1-3 hours/day, 1-2 instances |
| Spot compute cost | ~$50-100/month steady-state |
| S3 storage (all crates.io, version-columned) | ~500GB-1TB, ~$10-25/month |
| RDS PostgreSQL (catalog) | db.r6g.large, ~$50-100/month |

## Serving Layer

### Lambda Architecture

```
API Gateway (HTTP)
  POST /query
  Body: { "tool": "external_code_search", "args": { ... } }
    │
    ▼
Rust Lambda (cargo-lambda, provisioned concurrency)
    │
    ├─ 1. Catalog resolution
    │     Query PostgreSQL (RDS Proxy) for (package, ref) → revision
    │     ~2-5ms
    │
    ├─ 2. DuckDB query (embedded, in-process)
    │     ATTACH 'ducklake:postgresql://...' AS catalog
    │     SELECT ... FROM nodes
    │     WHERE package = $1 AND revision = $2 AND ...
    │     DuckDB reads Parquet from S3 via httpfs with predicate pushdown
    │     ~20-100ms
    │
    └─ 3. JSON response serialization
          ~1-5ms

Total: ~25-110ms per query (warm Lambda)
Cold start: ~1-2s (DuckDB + DuckLake + httpfs initialization)
```

**Lambda configuration:**
- Runtime: `provided.al2023` (cargo-lambda custom runtime)
- Memory: 2048MB (DuckDB + parquet decompression + query state)
- Timeout: 30s
- Provisioned concurrency: 5 warm instances during peak hours (eliminates cold start for production traffic)
- Ephemeral storage: 512MB (parquet row-group spill if needed)

**Cold start mitigation:**
- DuckDB + DuckLake + httpfs compiled statically into the Lambda binary (no `INSTALL` at runtime)
- Provisioned concurrency for warm pools
- Parquet metadata cached across invocations within a warm Lambda (same package queried repeatedly in an agent session)
- RDS Proxy pools DB connections across Lambda invocations

**Why Lambda over Fargate/ECS:** Pay-per-query (no idle cost), automatic scaling, zero operational overhead. Cold start (~1-2s) is acceptable for a code-context service (not a latency-critical path). If cold start proves problematic, the DuckDB query layer can migrate to Fargate — the MCP tool surface stays identical.

### Query Patterns

**Structural queries** (code_search, code_read, code_callers, code_callees):
- DuckDB reads Parquet from S3 with predicate pushdown on `(package, revision)` columns
- Parquet row-group statistics skip irrelevant row groups — only the requested revision's data is read
- Typical: 1-3 S3 range requests, 20-80ms

**Semantic search** (external_knowledge_context):
- BM25: SQL macros computing BM25 scores over `section_bodies` and `nodes` parquet (same pattern as spur-analyst's `init.sql`)
- Vector: brute-force `array_cosine_distance` over `symbol_embeddings.embedding` column, filtered by `(package, revision)`
- At per-package scale (5k-50k vectors after filter), brute-force cosine is 10-50ms — acceptable

**Cross-package edge following:**
- `code_callees` returns resolved edges (within package) AND unresolved edges (to other packages, labeled with `target_package`)
- Agent issues follow-up `external_code_search` against the target package if it wants to follow the edge

## MCP Tool Surface

### Tool Definitions

The service exposes five MCP tools, deployed as a Lambda-backed MCP server:

```
external_code_search(
  query: string,           — symbol name, pattern, or qualified name
  package: string,         — e.g., "serde", "tokio"
  source?: string,         — default "registry:crates-io"; "git:github.com/..." for git deps
  revision?: string,       — exact version or SHA; omit for "latest"
  ref?: string,            — branch/tag name; alternative to revision
  symbol_kind?: string,    — filter: function, struct, trait, etc.
  limit?: int              — default 20, max 200
) → { candidates: [...], total_matches: int, truncated: bool }

external_code_read(
  selector: string,        — pkg:serde@1.0.152::serde::de::Deserialize
  context_lines?: int      — surrounding context lines, default 0
) → { source: string, line_range: [int, int], file_path: string, ... }

external_code_callers(
  selector: string,
  include_unresolved?: bool — include cross-package labeled edges, default false
) → { callers: [...], counts_by_kind: {...}, unresolved_sample: [...] }

external_code_callees(
  selector: string,
  include_unresolved?: bool
) → { callees: [...], counts_by_kind: {...}, unresolved_sample: [...] }

external_knowledge_context(
  query: string,           — natural language: "how to deserialize JSON with serde"
  package: string,
  source?: string,
  revision?: string,
  ref?: string,
  scope?: string,          — "code" | "docs" | "all" (default "all")
  limit?: int              — default 8
) → { primary_evidence: [...], supporting_docs: [...], confidence: string, ... }
```

### Selector Scheme

External symbols use the `pkg:` prefix namespace:

```
pkg:serde@1.0.152::serde::de::Deserialize    — exact version
pkg:serde::Deserialize                         — latest version (refs table resolves)
pkg:tokio@abc123d::tokio::runtime::Runtime    — exact git SHA
pkg:tokio@main::tokio::runtime::Runtime        — branch tip (refs table resolves)
```

Resolution ladder:
1. `pkg:<package>@<revision>::<fqn>` → catalog lookup → DuckDB query
2. `pkg:<package>::<fqn>` → refs table resolves "latest" → catalog lookup → DuckDB query
3. `pkg:<package>@<revision>:<path>:<line>` → line locator within package
4. Bare names (no `pkg:` prefix) → workspace graph (unchanged behavior)

### MCP Integration

The brain agent connects to two MCP servers:

| MCP Server | Transport | Scope |
|---|---|---|
| `spur-graph` (existing) | stdio (local process) | Workspace graph |
| `spur-context-service` (new) | HTTP (API Gateway → Lambda) | External packages |

The agent routes queries based on context: workspace questions use local tools; external-package questions use the Lambda tools. The `pkg:` selector prefix is the explicit signal.

## Embedding Strategy

**Default model:** `JinaEmbeddingsV2BaseCode` (768-dimensional, code-specialized).

Jina's model is trained specifically for code understanding — it outperforms general-purpose text embedding models (like BGE-base) on code search and code similarity tasks. The 768-dim output matches the existing `EMBEDDING_VECTOR_DIMENSIONS` constant in spur-graph, so no schema changes.

The `embedding_model` column in `symbol_embeddings` records which model produced each row. This enables:
- **Model upgrades:** New versions indexed with a new model. Old versions retain old embeddings. The query layer can filter by model or query across both.
- **Multi-model:** Could store multiple embedding columns in the future (v2).

Build-time model selection: the spot instance reads `SPUR_EMBEDDING_MODEL` env var (default: `JinaEmbeddingsV2BaseCode`). Fallback to `BGEBaseEnV15` if the Jina model fails to load.

## Error Handling

### Build Pipeline

| Failure | Behavior |
|---|---|
| Spot interruption | 2-min warning → checkpoint to SQS → graceful shutdown. Job retried. |
| Tree-sitter parse failure | Log diagnostic, insert partial data, mark `index_status = 'partial'`. Queryable with caveat. |
| Embedding model load failure | Skip embeddings, mark `embeddings_status = 'pending'`. Structural queries still work. Follow-up job fills embeddings. |
| DuckLake commit failure | Retry 3x with backoff. On exhaustion: dead-letter queue, manual intervention. No catalog corruption. |
| Source fetch failure | Retry 3x. On failure: mark `index_status = 'failed'`, operator-visible. |

### Serving Layer

| Failure | Behavior |
|---|---|
| Package not indexed | Return `{ "status": "not_found", "package": "...", "available_revisions": [...] }` with 200 OK. |
| Catalog DB timeout | Return 503 with retry-after. RDS Proxy handles connection recovery. |
| S3 read timeout | DuckDB retries internally. On failure: return partial results with caveat. |
| Lambda cold start | First request ~1-2s. Provisioned concurrency minimizes for production traffic. |
| Revision not found but package exists | Return nearest available revisions + suggestion to check `package_catalog`. |

## New Crate Structure

```
crates/spur-context-service/
  Cargo.toml              — deps: duckdb, spur-analyst, spur-graph (build only),
                             serde_json, lambda_runtime, tokio
  src/
    lib.rs                — public query API (used by both Lambda and tests)
    catalog.rs            — DuckLake catalog resolution (package → revision → snapshot)
    query.rs              — DuckDB query builders for each MCP tool
    translate.rs          — Lance→Parquet translation (used by build pipeline)
    mcp.rs                — MCP tool definitions + JSON schemas
    lambda.rs             — cargo-lambda HTTP entry point
  tests/
    catalog_test.rs       — catalog resolution tests
    query_test.rs         — end-to-end query tests (embedded DuckDB + test data)
    translate_test.rs     — Lance→Parquet translation tests
```

**Dependency note:** `spur-context-service` depends on `spur-analyst` (for DuckDB query patterns and BM25 macro reuse) but NOT on `spur-graph` at query time. The `translate.rs` module uses `spur-graph` types for reading the artifact during the build pipeline, but this is a build-time dependency, not a serve-time one.

## Out of Scope (v1)

- **On-demand indexing:** Agent requests a package that isn't pre-indexed. v2 feature — requires synchronous build trigger + wait.
- **Cross-package graph queries:** Automatic edge following across package boundaries (e.g., "trace this call chain from my workspace through serde into core"). v2 — requires multi-artifact query planning.
- **Non-Rust ecosystems (npm, PyPI, Go):** v1 is crates.io + git Rust repos. The extraction pipeline (tree-sitter) supports other languages, but the ingestion triggers and source-fetch paths are Rust-ecosystem-specific in v1.
- **MotherDuck integration:** Using MotherDuck as the managed DuckLake catalog. v1 uses self-managed PostgreSQL RDS.
- **Multi-model embeddings:** Storing both Jina and BGE embeddings side-by-side. v2 — the schema supports it, the query layer doesn't need it yet.
- **Tiered extraction (document-mode for long-tail packages):** Lighter extraction for obscure packages. v2 — full graph for all packages in v1.
- **Cross-package edge resolution at build time:** Resolving edges from serde into core at build time (storing resolved cross-package edges). v2 — v1 stores unresolved labeled edges only.

## Testing Strategy

### Unit Tests (in `crates/spur-context-service/tests/`)

1. **catalog_resolution** — `(package, ref)` → revision lookups against a test DuckLake catalog
2. **query_nodes** — `external_code_search` against embedded DuckDB with fixture parquet
3. **query_callers_callees** — edge queries with resolved + unresolved cross-package edges
4. **translate_lance_to_parquet** — read a spur-graph artifact, translate to DuckLake tables, verify row counts and column values
5. **knowledge_context** — BM25 + vector search over fixture data, verify ranking and evidence assembly

### Integration Tests

6. **lambda_end_to_end** — deploy Lambda to LocalStack, call via HTTP, verify response shape
7. **build_pipeline** — trigger SQS job on spot instance (mocked), verify DuckLake snapshot + catalog entries
8. **catalog_concurrent_writes** — two spot instances index different packages simultaneously, verify no catalog corruption

### Smoke Tests (manual, documented)

9. Index serde 1.0.193 via CLI → query `external_code_search({query: "Deserialize", package: "serde"})` → verify results
10. Index tokio main branch → query `external_knowledge_context({query: "how to spawn a tokio task", package: "tokio", ref: "main"})` → verify ranked evidence
11. Cold-start timing: invoke Lambda after idle → measure first-request latency → verify < 2s

## Files Touched

**New:**
- `crates/spur-context-service/` — entire crate (Cargo.toml, src/, tests/)
- `docs/superpowers/specs/2026-06-22-code-context-service-design.md` — this file
- `infra/spur-context-service/` — Terraform/CloudFormation (Lambda, API Gateway, SQS, Spot Fleet, RDS, S3)

**Modified:**
- None. spur-graph and spur-analyst stay unchanged. The service consumes their output artifacts at build time.

**Unchanged:**
- `crates/spur-graph/` — workspace graph extraction, unchanged
- `crates/spur-analyst/` — workspace DuckDB query layer, unchanged
- `crates/spur-mcp/` — workspace MCP tools, unchanged
