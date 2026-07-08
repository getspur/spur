# Spur Context Service — `external_catalog` Discovery & Navigation Tool

- Status: approved design, not yet implemented
- Companion specs:
  - `docs/superpowers/specs/2026-06-24-context-service-on-demand-indexing-design.md`
  - `docs/superpowers/specs/2026-06-28-spur-context-medallion-design.md`
  - `crates/spur-context-service/docs/ARCHITECTURE.md`

## Problem

The context service exposes seven MCP tools (`crates/spur-context-service/src/mcp.rs`,
`tool_definitions()`): five serving tools that all **require** a `package`
argument, and two caller-scoped ingest tools. There is no way to discover what
is already indexed. A caller must know `package@revision` a priori or probe by
issuing a query and interpreting a not-found error. `external_index_status` is
scoped to a single caller-owned `job_id`, so it cannot serve as a listing.

Two capabilities are missing:

1. **Discovery** — "which packages/revisions are indexed and queryable?"
2. **Navigation** — "browse an indexed package like a catalog: package →
   revision → file tree → symbols," without already knowing symbol names.

## Industry grounding (why this shape)

The design composes five patterns that registries, code-intel platforms, and
lakehouse catalogs converged on:

| Pattern | Exemplars | Where it lands here |
|---|---|---|
| Two-level simple index (packages → versions) | PyPI PEP 503/691, OCI `_catalog` + `tags/list`, Go proxy `@v/list` | catalog levels 0–1 |
| Floating-ref resolution as first-class | Go proxy `@latest` | `refs` rows surfaced at level 1 |
| Search + browse + detail triad | crates.io, npm search, Amundsen/DataHub | existing search tools + this tool + `package_catalog` row as detail record |
| One-namespace-level-per-call descent, cursor pagination | Iceberg REST Catalog, MCP `resources/list` | progressive descent contract (mirrors `notebook_catalog`) |
| Catalog version token for cache coherence | OCI ETags, Go index feed | `generation` per row + top-level max generation |

## Architectural facts the design builds on (verified 2026-07-08)

- **The data already exists in gold.** `sql/catalog_tables.sql:248` defines
  `package_catalog` (source, package, revision, revision_kind, semver_*,
  snapshot_id, indexed_at, index_status, embeddings_status, row_counts JSON,
  generation, lineage hashes). `refs` (line 276) maps `ref_name → revision`.
  All serving tables are partitioned by `(source, package)`.
- **One row per coordinate, complete-only.** `write_catalog_metadata`
  (`src/translate.rs:1224`) runs DELETE + INSERT per
  `(source, package, revision)` in one transaction and hardcodes
  `index_status='complete'`; `update_refs` runs in the same transaction.
  Therefore the frozen snapshot only ever contains completed publishes — no
  generation dedupe and no status filtering is needed at read time. In-flight
  and failed jobs live in DynamoDB (control plane) and are out of scope here.
- **The query layer is already wired for these tables.** `QueryTables::load`
  (`src/query.rs:27`) resolves `refs` and `package_catalog` through
  `readable_table` (gold-vs-main indirection, `src/catalog.rs:506`), and
  `latest_revision` (`src/query.rs:843`) already queries `refs` and tolerates
  a missing `refs` table in older snapshots.
- **Serving dispatch is generic.** The Lambda `handler`
  (`src/lambda.rs:141`) special-cases only `external_index` /
  `external_index_status`; every other tool name flows through
  `mcp::handle_tool_sync` against the frozen snapshot (invariant B1: never
  Postgres). A new read-only tool needs **zero Lambda/infra changes**.
- **The client proxy is generic too.** `ContextServiceClient::call`
  (`crates/spur-core/src/mcp/context_service.rs:66`) POSTs
  `{"tool": name, "args": args}` — a new tool only needs a definition added to
  the client-side `tool_definitions()` (line 118).

## Tool design: `external_catalog`

One tool, progressive descent, one layer per call (the `notebook_catalog`
contract). Which layer is returned is determined by which coordinates are
present.

### Args

```rust
#[derive(Deserialize)]
struct CatalogArgs {
    source: Option<String>,          // default DEFAULT_SOURCE, like CodeSearchArgs
    package: Option<String>,
    revision: Option<String>,        // exact version/SHA
    #[serde(rename = "ref")]
    ref_name: Option<String>,        // alternative to revision (validate_revision_choice)
    path: Option<String>,            // directory prefix within package@revision
    name_filter: Option<String>,     // substring filter at levels 0 and 3
    limit: Option<usize>,            // default 50, max 200
    cursor: Option<String>,          // opaque keyset cursor
}
```

### Descent levels

| Level | Args present | Returns | Backing query |
|---|---|---|---|
| 0 packages | none (or `source`, `name_filter`) | rows of `{source, package, latest_revision, revision_count, indexed_at}` | `package_catalog` GROUP BY (source, package) LEFT JOIN `refs` ON ref_name='latest' |
| 1 revisions | `package` | full detail rows: `{revision, revision_kind, semver, indexed_at, embeddings_status, row_counts, generation, snapshot_id}` + `refs: [{ref_name, revision, updated_at}]` | `package_catalog` WHERE source+package; `refs` WHERE source+package |
| 2 file tree | `package` + `revision`/`ref` (+ optional `path`) | one directory layer: `{entries: [{name, kind: dir|file, file_count, symbol_count?}]}` | `file_manifests` (path + node_ids), prefix-grouped one segment past `path` |
| 3 symbols | `package` + `revision`/`ref` + `path` = exact file | symbol rows `{entity_name, qualified_name, symbol_kind, line_range, selector}` | `nodes` WHERE file_path = path (+ `name_filter`, `symbol_kind` future) |

Every level-2/3 row carries a ready-to-use `selector`
(`pkg:<package>@<revision>::<qualified>` via the existing `build_uri` /
selector grammar in `src/query.rs`) and `next` hints into
`external_code_read` / `external_code_search`, mirroring the evidence-pack
convention.

Level 2/3 revision handling reuses `resolve_revision` (`src/mcp.rs:790`) →
`CatalogResolver::resolve` (`src/catalog.rs:224`), so `ref: latest` works
exactly like the other serving tools; levels 0–1 do not resolve a revision.

### Response envelope

```json
{
  "level": "packages | revisions | tree | symbols",
  "rows": [...],
  "total_matches": 123,
  "truncated": false,
  "next_cursor": null,
  "catalog_generation": 42
}
```

- `catalog_generation` = `SELECT max(generation) FROM {package_catalog}` — a
  cheap catalog-wide version token; a client seeing the same value can reuse
  its cached listing (registry-ETag pattern).
- Pagination is keyset on the level's natural sort key
  (`(source, package)` / `revision` / entry name), encoded as an opaque
  cursor string. `truncated` + `next_cursor` follow the existing
  `CodeSearchResult { truncated }` convention.

## Wiring points (all four, no others)

1. **`src/query.rs`** — new fns `list_packages`, `list_revisions_and_refs`,
   `list_tree_entries`, `list_file_symbols`, each taking `&Connection` and
   using `QueryTables::load` (which already exposes `refs` and
   `package_catalog`). Follow the `latest_revision` missing-table guard for
   `refs`. Result structs serialize with `json_value` like `CodeSearchResult`.
2. **`src/mcp.rs`** — `CatalogArgs` + `external_catalog_def()` (schema shape
   mirrors `external_code_search_def`, `src/mcp.rs:1255`, incl. `DEFAULT_SOURCE`
   default and `additionalProperties: false`); `handle_catalog` dispatching on
   present args; register in `tool_definitions()` (line 150) and the
   `handle_tool_sync` match (line 176); `handle_catalog_without_catalog`
   returns the empty level-0 envelope (mirrors
   `handle_code_search_without_catalog`, line 237).
3. **`src/lambda.rs`** — no change; generic fallthrough routes it.
4. **`crates/spur-core/src/mcp/context_service.rs`** — add
   `external_catalog_def()` to the client `tool_definitions()` (line 118);
   `call` is already generic.

## Non-goals / follow-ups

- **Global job listing** (Sourcegraph-uploads-style "queued/processing/errored"
  view) is a control-plane feature over DynamoDB, intentionally separate from
  the serving-plane catalog. Revisit only if operators need it.
- **`since`-style change feed** for catalog mirroring — not needed for
  interactive agent browsing; `catalog_generation` covers cache coherence.
- **Cross-package symbol search** (making `package` optional on
  `external_code_search`) — orthogonal; the catalog reduces the need for it.
- Doc/skill updates on ship: `spurpower-code-explore` external-mode table
  ("this package is not indexed" row gains a "list what IS indexed" row),
  CLAUDE.md external tools blurb, and `docs/ARCHITECTURE.md` tool inventory.

## Testing

TDD per repo convention (`test(...)` commit first):

- `tests/mcp_test.rs`: level selection by arg shape; empty-catalog envelope on
  the without-catalog path; validation errors (revision+ref both set reuses
  `validate_revision_choice`); pagination/truncation; `ref: latest` descent.
- `tests/query_test.rs`: each `list_*` fn against a seeded local DuckLake
  (fixture pattern already used by `catalog_test.rs` / `seed_published_snapshot`),
  including the missing-`refs`-table guard and multi-package ordering.
- Staging smoke follow-up: after `external_index` completes, `external_catalog`
  level 0 must show the fixture package (extend `smoke-staging-e2e.py` E1).
