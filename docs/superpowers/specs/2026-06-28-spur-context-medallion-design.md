# spur-context-service — Medallion (bronze/silver/gold) Data Architecture

- **Status:** Draft (design approved in brainstorming; pending spec review)
- **Date:** 2026-06-28
- **Scope:** `crates/spur-context-service`
- **Author:** brainstormed with dual adversarial review (codex = correctness, opencode = cost) + first-principles/MCTS evaluation

## 1. Problem & context

`spur-context-service` indexes external packages into a DuckLake lakehouse and serves
code-context over MCP (Lambda) and bulk ingest over Fargate.

Today the pipeline is **mostly ephemeral**: a Fargate worker fetches source (git/tarball)
to `/tmp`, runs `spur graph build` to a parquet+Lance artifact in `/tmp`, then `translate`
loads 12 coordinate-stamped DuckLake tables partitioned by `(source, package)`. Only the
final tables persist. The DuckLake catalog is a single mutable `.ducklake` file on S3,
mutated via download→modify-local→upload-with-If-Match, serialized by a DynamoDB lease.

Two stages that have real reuse value — the raw source and the graph artifact — are thrown
away after every run. This makes reprocessing (after a graph-builder or embedding-model
upgrade) require a full re-fetch + rebuild, and makes raw source unretrievable once upstream
changes or disappears.

## 2. Goals (irreducible requirements)

- **R1 Retrieval** — raw source bytes re-fetchable by `(source, package, version)` even if
  upstream vanishes.
- **R2 Reprocessability** — rebuild a downstream layer after a tool-version bump without
  redoing upstream work, cheaply.
- **R3 Serving correctness** — a reader never observes a half-published or file-missing
  index for a resolved `(source, package, revision)`.
- **R4 Low cost** — zero idle *server* cost is the top priority; persistent storage is the
  only acceptable always-on cost.
- **R5 Staleness visibility** — "which packages lag which tool version" is a SQL query.

### Non-goals (deferred — YAGNI)

- Cross-package analytics / a single global queryable catalog for ad-hoc analysis. (If ever
  needed, build a separate rollup reader; do not burden serving.)
- Automatic staleness reconciler/scheduler. (Versions are stamped now so it can be added
  later without schema change.)

## 3. Load-bearing invariant

**Serving is always per-`(source, package, revision)`.** `query.rs` resolves a single
coordinate before any scan; there are no cross-package serving queries. Consequence: serving
only ever needs one package's catalog at a time, which is what makes a per-package serving
snapshot (below) viable.

## 4. Architecture (decision: Aurora ingest-only + S3 serving snapshot — "B1")

The DuckLake catalog metadata lives in **Aurora Serverless v2 (Postgres), scale-to-zero,
written by ingest only.** After each gold publish, ingest exports a **per-package read-only
catalog snapshot** to S3. **Serving reads gold data files + that snapshot from S3 and never
connects to Aurora** — so Aurora can sleep and serving never eats a cold start.

```
                                        ┌───────────────┐
                    MCP / client ─────► │  API Gateway  │
                                        └───────┬───────┘
                                   ┌────────────▼─────────────┐   NON-VPC Lambda
                                   │   Serving Lambda          │   (reads S3 only,
                                   │   external_code_*          │    no VPC ⇒ no NAT)
                                   │   external_knowledge_*     │
                                   └────────────┬─────────────┘
                                                │ read-only (per-package)
                          ┌─────────────────────▼──────────────────────┐
                          │                    S3                         │
                          │  gold/catalog-snapshot/<pkg>.ducklake ◄───────┼ serving resolves here
                          │  gold/data/…              (parquet)            │
                          │  silver/<src>/<pkg>/<ver>/<builder>/ + manifest│
                          │  bronze/<src>/<pkg>/<ver>/source.<ext>         │
                          └──▲──────────▲───────────────▲────────────────┘
                             │          │               │  S3 GATEWAY VPC ENDPOINT (free)
            write bronze ────┘   r/w silver             │ read silver        ▲ export snapshot
        ╔════════════════════════════════════════════════════════════════════╪═══════════╗
        ║ VPC                                                                  │           ║
        ║  ┌─────────────┐  ┌─────────────┐   ┌──────────────────────┐        │           ║
        ║  │ FETCH       │  │ BUILD       │   │ TRANSLATE            │─────────┘           ║
        ║  │ Fargate     │  │ Fargate     │   │ Fargate              │   ┌────────────────┐║
        ║  │ PUBLIC sub  │  │ no internet │   │ reaches Aurora in-VPC│──►│ Aurora v2       │║
        ║  │ +public IP  │  │ no Aurora   │   │ + S3 gateway EP      │   │ Serverless      │║
        ║  │ git/tarball │  │ graph build │   │ writes catalog       │   │ scale-to-zero   │║
        ║  └──────┬──────┘  └─────────────┘   └──────────────────────┘   │ DuckLake catalog│║
        ║         │ internet egress (NO NAT: public IP)                  │ INGEST-ONLY     │║
        ╚═════════╪══════════════════════════════════════════════════    │ private subnet  │║
                  ▼                                                       └────────────────┘║
        [ github / crates.io / arbitrary source ]   ════════════════════════════════════════
   Orchestration: Step Functions + DynamoDB (jobs) — gateway endpoints / public-IP, no NAT
```

## 5. Networking & cost

NAT gateway is **not** required anywhere:

| Component | Public internet? | Reached via | NAT |
|---|---|---|---|
| Serving Lambda | no (reads S3) | non-VPC public AWS net / S3 gateway EP | none |
| FETCH stage | yes (git/tarball) | public subnet + public IP | none |
| BUILD stage | no | S3 gateway endpoint | none |
| TRANSLATE stage | no | S3 gateway EP + Aurora in-VPC | none |
| Aurora v2 | no (private) | in-VPC from TRANSLATE only | none |

Idle cost ≈ **Aurora paused ($0) + S3 storage**. Aurora wakes only during ingest bursts.
Caveat: Fargate stages call Step Functions `SendTaskSuccess/Failure`; give those tasks a
public IP (cheapest) or add an SFN interface endpoint (~$7/mo). Still no NAT.

## 6. Data model

### S3 layout

```
s3://spur-context/
  bronze/<source>/<package>/<version>/source.{tar.gz|zip|gitbundle}
  silver/<source>/<package>/<version>/<builder_version>/{nodes.parquet,…,code_symbols.lance,sections.lancedb}
  silver/<source>/<package>/<version>/<builder_version>/manifest.json
  gold/data/…                                  # DuckLake-managed parquet, PARTITIONED BY (source,package)
  gold/catalog-snapshot/<source>__<package>.ducklake   # per-package read-only serving snapshot
```

Bronze and silver are written to **immutable, content-addressed prefixes**. Silver registers
only after writing a **manifest** (file list + sizes + ETags + schema hash); consumers read
manifest-listed files, never a glob prefix (avoids partial-upload / Lance multi-file races).

### DuckLake schemas (in Aurora-backed catalog)

- **`bronze.raw_sources`** — `source, package, version, revision_kind, semver_*, source_kind,
  source_url, s3_uri, content_sha256, bytes, fetched_at, fetch_status`. PARTITIONED BY (source, package).
- **`silver.graph_artifacts`** — `source, package, version, …, builder_version,
  graph_content_hash, artifact_s3_prefix, manifest_uri, node/edge/file/embedding counts,
  built_at, build_status`. PARTITIONED BY (source, package).
- **`gold.*`** — the existing 12 tables (schema-qualified), plus a **generation** column and
  **lineage columns** on `gold.package_catalog`: `generation`, `bronze_content_sha256`,
  `silver_graph_content_hash`, `builder_version`, `translate_schema_version`
  (`embed_text_version` already exists).

### Lineage as enforced identity tuples

Idempotency and reproducibility are keyed by explicit content identities, not just coordinates:

- bronze: `(source, package, version, content_sha256)`
- silver: `(bronze_content_sha256, builder_version, graph_content_hash)`
- gold:   `(silver_graph_content_hash, translate_schema_version, embed_text_version)`

Hash drift on an existing `(source, package, version)` is rejected unless it is published as a
new logical version. This gives R2 (rebuild keyed by exact inputs), R5 (staleness query), and
idempotent re-ingest in one mechanism.

## 7. Ingest pipeline (3 stages)

1. **FETCH (bronze)** — abuse-validate URL, fetch git/tarball, upload archive to the
   content-hash bronze prefix, register `bronze.raw_sources`. Dedup: if `(source, package,
   version)` already exists with a matching `content_sha256`, skip the fetch. Lifecycle rule
   on bronze (Intelligent-Tiering @30d, keep latest-N).
2. **BUILD (silver)** — `spur graph build` from bronze, upload artifact files to the immutable
   silver prefix, write+validate `manifest.json`, register `silver.graph_artifacts` with
   `builder_version` + `graph_content_hash`. No internet, no Aurora.
3. **TRANSLATE (gold)** — read manifest-listed silver files, write a new immutable **generation**
   of the 12 gold tables, validate (all tables + sidecars + counts + lineage), then **publish by
   flipping `gold.package_catalog` to that generation** (generation-flip fencing). Finally,
   **export the per-package catalog snapshot** to `gold/catalog-snapshot/`.

Publish is atomic at the pointer flip: readers either see the previous complete generation or
the new one, never a partial write (R3).

## 8. Serving path

Serving Lambda (non-VPC) resolves `(source, package, ref/revision)` against the per-package
`gold/catalog-snapshot/<…>.ducklake` (attached read-only), then scans `gold/data` parquet —
all from S3, no Aurora, no NAT. `refs`/`package_catalog` for `latest` resolution live inside
the per-package snapshot (latest is per-package anyway).

## 9. The snapshot-export risk and its fence (critical)

**Chosen path B1 carries one real correctness risk** (flagged by the codex review as the
single riskiest assumption): an exported `.ducklake` snapshot can reference parquet files that
DuckLake compaction/cleanup later deletes → serving reads a missing file.

### POC results (2026-06-28, on real AWS — apse5, Aurora Serverless v2 16.13 min-0-ACU, duckdb 1.5.3)

Validated end-to-end via the `spur-builder` EC2 over SSM; data path `s3://wiilearn-spur-sccache-apse5/ducklake-poc/`:

- **Ingest works:** `ATTACH 'ducklake:postgres:…' (DATA_PATH 's3://…')` wrote parquet to S3, read back via the Aurora catalog. ✅
- **`COPY FROM DATABASE` does NOT work** for the snapshot (codex's index warning confirmed): fails with `Cannot bind index 'ducklake_snapshot', unknown index type ''` — the 29 `ducklake_*` metadata tables carry indexes DuckDB can't replicate into a file catalog, even with the `ducklake` extension loaded. ❌
- **Working snapshot mechanism = data-only metadata copy + index replay:** create a fresh `duckdb:` file and `CREATE TABLE snap."<t>" AS SELECT * FROM pg.public."<t>"` for each of the 29 `ducklake_*` tables, then **recreate the catalog's 5 UNIQUE primary-key indexes** (pulled from `pg_indexes`: `ducklake_data_file(data_file_id)`, `ducklake_delete_file(delete_file_id)`, `ducklake_schema(schema_id)`, `ducklake_snapshot(snapshot_id)`, `ducklake_snapshot_changes(snapshot_id)`). Produces a ~3.3 MB structurally-faithful `snapshot.duckdb`. Verified: re-indexed snapshot serves correctly. ✅
  - I.e. the "can't copy indexes" limitation of `COPY FROM DATABASE` is sidestepped by copying data first then replaying the 5 index DDLs.
- **Serving with zero Postgres works:** a fresh DuckDB process (postgres extension never loaded) downloaded `snapshot.duckdb` from S3, `ATTACH 'ducklake:snapshot.duckdb' (DATA_PATH 's3://…', READ_ONLY)`, and served the correct rows straight from S3. ✅
- **Durability is a *safe* degradation, not a crash:** after DELETE+INSERT + `ducklake_merge_adjacent_files` + `ducklake_expire_snapshots(older_than=>now())` + `ducklake_cleanup_old_files(older_than=>now())` on the live Aurora catalog, the **already-published stale snapshot still served its point-in-time rows** (1000) rather than erroring — DuckLake retains data files while any live snapshot references them (merge-on-read delete files). Re-exporting refreshed the snapshot to current state (min_id 501, max_id 1500, 500 new rows). A hard "missing file" only occurs under aggressive rewrite+expire+cleanup with a window shorter than the snapshot's republish lag.

- **End-to-end serving on Lambda works:** a container Lambda (arm64, `public.ecr.aws/lambda/python:3.12`, `duckdb` + baked `ducklake`/`httpfs`/`aws` extensions, role `spur-context-lambda-poc`) — **no Postgres, S3-only** — downloaded the frozen `snapshot.duckdb` from S3, `ATTACH 'ducklake:…' (READ_ONLY)`, and returned correct results (`1000` rows; detailed query `[1000, 501, 1500, 500]` matching the post-mutation catalog). ✅
  - **Cold start ~15 s** (duckdb import + extension load + catalog download; init phase hit Lambda's 10 s cap and spilled into the handler), **warm invokes ~instant**, peak memory 261 MB. → serving needs provisioned concurrency or a warm-keep, or a lighter package, if cold-start latency matters.
  - Gotcha: Lambda rejects buildx's default OCI manifest-list+attestation image — build with `--provenance=false --sbom=false --platform linux/arm64`.

**Conclusion:** B1 is **fully validated end-to-end on real AWS** (ingest → Aurora catalog + S3 data → frozen `.ducklake` snapshot on S3 → no-Postgres Lambda read). The snapshot mechanism is **data-only metadata copy + 5-index replay** (NOT `COPY FROM DATABASE`). The compaction risk is bounded and fence-able via the cleanup `older_than` window. Open serving concern: cold-start latency.

Required mitigations (validated/refined by the POC above):

- **Generation-flip publish** (above) so a snapshot only ever points at a fully-validated,
  complete generation.
- **Retention/pinning:** DuckLake destructive cleanup must not delete data files referenced by
  a currently-published serving snapshot. Either disable cleanup, set a retention window longer
  than the snapshot republish cadence, or pin the snapshot's referenced files.
- **Re-export after compaction:** whenever gold for a package is compacted/rewritten, re-export
  that package's snapshot and only then allow cleanup of the superseded files.
- **Post-export verification:** before making a snapshot live, verify every data file it
  references exists in S3.
- Export mechanism is **data-only metadata copy** of the 29 `ducklake_*` tables into a fresh
  `duckdb:` file (NOT `COPY FROM DATABASE`, which fails on metadata indexes — see POC results).
- Initialize gold with the final serving S3 data path so the exported snapshot attaches
  without `OVERRIDE_DATA_PATH`.

## 10. Versioning, staleness, reprocessing

- Version columns stamped at ingest (bronze `content_sha256`; silver `builder_version` +
  `graph_content_hash`; gold `translate_schema_version` + `embed_text_version`).
- Staleness = SQL over the registries (e.g. `silver.builder_version < $current`).
- On-demand reprocess = a new `JobEnv` entrypoint `--from-layer {silver|bronze}` reusing the
  existing SFN/Fargate/DynamoDB machinery (rebuild gold from silver without re-fetch; rebuild
  silver from bronze). No new infra.

## 11. Migration (forward-only)

No dual-run, no backfill. Add the new nullable columns (additive, safe). Persist bronze+silver
for new ingests. Leave existing rows untouched; NULL lineage = "pre-medallion / stale", lazily
reprocessed on demand. Delete the duplicate `init_catalog.sql` (drift risk).

## 12. Decisions log

| # | Decision | Choice |
|---|---|---|
| Q1 | Driver | persistent + reprocessable + retrievable; bronze stores raw S3 + URI registry |
| Q2 | Catalog store | **Aurora Serverless v2 (Postgres), scale-to-zero, ingest-only** |
| Q3 | Serving read path | **B1: S3 catalog snapshot; serving never touches Aurora** |
| Q4 | Gold | **materialized** DuckLake tables (kept; not virtual/skip) |
| Q5 | Versioning | stamp now (staleness = query), on-demand reprocess, auto-reconciler deferred |

Hardening adopted from review/MCTS: identity-tuple lineage, generation-flip publish, manifest
silver, forward-only migration, NAT-free networking.

**Note on the contested axis:** first-principles/MCTS and the industry "Frozen DuckLake"
pattern both favor a per-package self-contained DuckDB-file catalog (no Postgres) on
cost+correctness. The user chose Aurora B1 deliberately, with the explicit rationale that
**Aurora's value is supporting multiple concurrent ingest writers against one ACID catalog**
(the dual-engine pattern: concurrent Postgres writers in, frozen read-only DuckDB-file out).
The NAT cost objection was retracted (Aurora is NAT-free and near-zero idle). The POC
validated the full mechanism on real AWS, including the snapshot export (data-only copy + 5
index replay) and safe-degradation durability — so the §9 fence is proven, not just proposed.

## 13. Action items (phased; value front-loaded, contested change last)

- **Phase 0 — Data contracts (foundational).** Identity tuples, S3 prefix scheme, manifest
  schema; add nullable lineage+version+generation columns to `package_catalog`; delete duplicate
  `init_catalog.sql`. Exit: DDL + manifest spec reviewed; columns additive.
- **Phase 1 — Persist bronze (R1).** Upload to content-hash prefix; `bronze.raw_sources`;
  dedup by `content_sha256`; lifecycle rule. Exit: re-request skips fetch; retrieval test passes.
  Lifecycle follow-up: configure the bronze S3 bucket/prefix to move `bronze/*` objects to
  Intelligent-Tiering after 30 days and retain only the latest-N noncurrent objects per
  coordinate/version policy once the infra phase owns bucket rules.
- **Phase 2 — Persist silver + manifest (R2 substrate).** Immutable prefix + validated manifest;
  `silver.graph_artifacts`; translate reads manifest files. Exit: silver registered with
  `graph_content_hash` + `builder_version`.
- **Phase 3 — Aurora catalog + generation-flip + snapshot export (R3) — DECISION-GATED SPIKE.**
  Stand up Aurora v2 (scale-to-zero, private subnet, NAT-free topology); move catalog to Aurora;
  generation-flip publish; export per-package snapshot; prove the §9 fence (compaction/cleanup
  retention + post-export verification). Exit: spike proves snapshot attaches + survives a
  compaction cycle; no-half-publish / no-missing-file tests pass.
- **Phase 4 — Reprocess entrypoint (R2 realized).** `--from-layer {silver|bronze}`; staleness
  SQL report. Exit: rebuild gold-from-silver without refetch; rebuild silver-from-bronze.

Phases 0–2 + 4 deliver R1 + R2 + R5 at zero new infra. Phase 3 is the Aurora + serving-snapshot
upgrade and is the only phase carrying the §9 risk.

## 14. Testing

- Per-layer unit tests: bronze upload+register+dedup; silver register+manifest validation; gold
  schema-qualified translate + generation flip.
- Full-lineage integration test: fetch→bronze→silver→gold asserting registries + identity tuples.
- Publish-atomicity test: a reader never sees a partial generation; pointer flip is all-or-nothing.
- Snapshot-fence test: simulate compaction/cleanup; assert serving snapshot never references a
  deleted file (the §9 guarantee).
- Reprocess tests: gold-from-silver without refetch; silver-from-bronze.
- Serving-decoupling test: serving resolves + reads with Aurora unreachable (proves B1).

## 15. Open items / spike

- ~~Exact DuckLake mechanism for snapshot export from an Aurora-backed catalog~~ — **RESOLVED
  by POC:** data-only copy of the 29 `ducklake_*` metadata tables into a `duckdb:` file.
- DuckLake retention/cleanup configuration that guarantees the §9 fence (POC showed safe
  degradation; still tune `older_than` ≥ snapshot republish lag and re-export after compaction).
- Snapshot is whole-catalog (all packages) per `COPY`/data-copy; confirm size/time scaling to
  ~10k packages (POC: 29 tables → 3.3 MB for one tiny package).
- Confirm current AWS pricing (Aurora v2 ACU rate, public IPv4, interface endpoints) before
  finalizing cost claims.
