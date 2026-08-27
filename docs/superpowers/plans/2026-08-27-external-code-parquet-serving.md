# External Code Parquet Serving Implementation Plan

> **For SPUR orchestrator:** This plan is designed for a beads-backed execution epic. Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels. Workers must preserve RED and GREEN as separate commits and may not write production code before observing the intended failing test.

**Source spec:** `docs/superpowers/specs/2026-08-27-external-code-parquet-serving-design.ipynb`
**Formal @spec cells:** `ARCHITECTURE`, `EXTERNAL-TOOL-ROUTING-V1`, `PUBLICATION-ELIGIBILITY-V1`
**Design epic:** `bd-1iru` (closed)
**Architecture solve:** `sol_e486929ddb034f79` (`sat`, lexicographic optimum complete)

**Goal:** Serve catalog and `external_code_*` requests from immutable Silver Parquet artifacts without DuckDB, while keeping `external_knowledge_context` on DuckDB/DuckLake, preserving one invocation per request and one generation-atomic publication boundary.

**Architecture:** Deploy exactly two serving Lambdas behind the existing API hostname. The caller routes by tool directly to Code or Knowledge. Code opens `spur-graph` Parquet plus an immutable source-text sidecar through a validated `/tmp` cache. Knowledge retains the frozen DuckLake snapshot reader. A single live pointer advances only after the snapshot, registry, graph manifest, and source sidecar for one generation are complete and hash-valid. There is no router Lambda. Existing source-fetcher, worker, authorizer, and cleanup Lambdas are not counted as serving Lambdas.

**Tech Stack:** Rust 2021, `spur-graph::store::parquet::ParquetClient`, Arrow/Parquet, AWS SDK for Rust, S3, AWS Lambda, API Gateway HTTP API, Terraform, Python smoke tests, and Z3.

**TDD rule for every task:** Before RED, call `solve_rule_spec`. If a live implemented family owns the rule, use `solve_rules`; if no family matches but the task introduces a bound, use the generic preflight/check/solve flow. Reload `sol_e486929ddb034f79` whenever the architecture choice is relevant. After GREEN, re-run every solve used by that task against landed values. `unknown` and `timeout` are never completion evidence.

---

### Task 1: Define the immutable serving-registry contract

**Task ID:** `serving-registry-contract`

**Files:**
- Create: `crates/spur-context-service/src/serving_registry.rs`
- Modify: `crates/spur-context-service/src/lib.rs`
- Create: `crates/spur-context-service/tests/serving_registry_test.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] Registry serialization is deterministic and versioned.
- [ ] Every package entry carries the registry generation plus strong references for graph manifest and source sidecar.
- [ ] Empty hashes, non-S3 artifact URIs, duplicate package identities, mixed generations, and incomplete entries are rejected.
- [ ] Resolution is exact for `(source, package, revision)` and never silently falls back across generations.
- [ ] The persisted architecture solve is recorded in the completion audit.

**Suggested Worker:** codex

**Scope Boundary:** Registry types and pure validation only. Do not add S3 I/O, DuckDB queries, cache policy, or Lambda routing.

**TDD Implementation:**

- [ ] **SOLVE PRE:** Navigate `data_integrity` for uniqueness, foreign-key/generation consistency, and conditional-required rules. Use `solve_rules` for every applicable implemented rule; otherwise record that the remaining validation is path-only. Reload `sol_e486929ddb034f79` and assert `catalog_backend=immutable_registry`.
- [ ] **RED:** Add a real mixed-generation fixture and the wished-for API:

```rust
#[test]
fn registry_rejects_package_from_another_generation() {
    let mut registry = complete_registry(7);
    registry.packages[0].generation = 6;
    assert_eq!(registry.validate().unwrap_err().code(), "generation_mismatch");
}
```

- [ ] **Verify RED:** `scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --test serving_registry_test registry_rejects_package_from_another_generation`
- [ ] **Commit RED:** `git commit -m "test(spur-context-service): serving-registry reject mixed generation"`
- [ ] **GREEN:** Implement deterministic structs centered on this contract:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRef {
    pub uri: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServingPackage {
    pub source: String,
    pub package: String,
    pub revision: String,
    pub generation: i64,
    pub graph_prefix_uri: String,
    pub graph_manifest: ArtifactRef,
    pub source_sidecar: ArtifactRef,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServingRegistry {
    pub schema_version: u32,
    pub generation: i64,
    pub packages: Vec<ServingPackage>,
}
```

- [ ] **Verify GREEN:** Run the targeted test, then `scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --test serving_registry_test`.
- [ ] **SOLVE POST:** Re-run applicable integrity rules against the landed fixture set and record the result IDs.
- [ ] **Commit GREEN:** `git commit -m "feat(spur-context-service): serving-registry validate immutable entries"`

### Task 2: Publish a source-text Parquet sidecar with Silver

**Task ID:** `source-sidecar`

**Files:**
- Modify: `crates/spur-context-service/Cargo.toml`
- Create: `crates/spur-context-service/src/source_sidecar.rs`
- Modify: `crates/spur-context-service/src/lib.rs`
- Modify: `crates/spur-context-service/src/worker.rs`
- Modify: `crates/spur-context-service/tests/worker_test.rs`

**Depends on:** `serving-registry-contract`

**Acceptance Criteria:**
- [ ] Every successfully persisted Silver graph contains `source_files.parquet` with `file_path`, `content_oid`, and `source_text` columns.
- [ ] Rows are derived from the graph file manifest and the prepared source root; non-UTF-8 or missing referenced sources fail publication.
- [ ] `SilverManifestFile` carries SHA-256 in addition to size and ETag for every file.
- [ ] The sidecar is uploaded before the Silver manifest and is included in the manifest hash.
- [ ] The writer uses Arrow/Parquet directly; it does not invoke DuckDB.

**Suggested Worker:** codex

**Scope Boundary:** Silver artifact construction and manifest integrity only. Do not change Gold schema or frozen generation publication.

**TDD Implementation:**

- [ ] **SOLVE PRE:** Navigate the integrity catalog for required columns and unique `(file_path, content_oid)` rows. No new numeric buffer or row-group bound may be selected without a solver result.
- [ ] **RED:** Extend the worker fixture to persist two real source files and read the produced Parquet back with Arrow:

```rust
let sidecar = artifact_dir.join("source_files.parquet");
let rows = read_source_sidecar(&sidecar).unwrap();
assert_eq!(rows[0].file_path, "src/lib.rs");
assert_eq!(rows[0].content_oid, expected_oid);
assert_eq!(rows[0].source_text, "pub fn answer() -> u32 { 42 }\n");
assert!(manifest.files.iter().any(|f| f.path == "source_files.parquet" && f.sha256.len() == 64));
```

- [ ] **Verify RED:** `scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --test worker_test silver_persistence_includes_source_sidecar`
- [ ] **Commit RED:** `git commit -m "test(spur-context-service): source-sidecar require source parquet"`
- [ ] **GREEN:** Add Arrow/Parquet dependencies compatible with `spur-graph`, build rows from the graph manifest, write to a temp file, fsync/close, hash, then rename into the artifact directory before `persist_silver_graph_artifact_with_manifest` enumerates files.
- [ ] **Verify GREEN:** Run the targeted test and `scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --test worker_test`.
- [ ] **SOLVE POST:** Re-run any selected integrity rules against the landed schema and row fixture.
- [ ] **Commit GREEN:** `git commit -m "feat(spur-context-service): source-sidecar publish source parquet"`

### Task 3: Carry Silver artifact identity into translation

**Task ID:** `translation-lineage`

**Files:**
- Modify: `crates/spur-context-service/src/translate.rs`
- Modify: `crates/spur-context-service/src/worker.rs`
- Modify: `crates/spur-context-service/tests/worker_test.rs`

**Depends on:** `source-sidecar`

**Acceptance Criteria:**
- [ ] `TranslateLineage` carries graph prefix, graph-manifest URI/hash/bytes, and source-sidecar URI/hash/bytes.
- [ ] Prepared jobs derive every field from the persisted `SilverGraphArtifact` and manifest, never from path convention alone.
- [ ] Empty or mismatched artifact identity stops translation before a Gold write.
- [ ] Existing lineage and worker tests remain green.

**Suggested Worker:** codex

**Scope Boundary:** In-memory lineage handoff only. Do not alter Gold SQL schema or S3 publication ordering.

**TDD Implementation:**

- [ ] **SOLVE PRE:** Navigate `data_integrity.conditional_required`; execute it if the live catalog can express “complete Silver requires all artifact refs.”
- [ ] **RED:** Extend `worker_test` so `prepare_job` must expose the exact persisted URIs and SHA-256 values in `prepared.lineage`.
- [ ] **Verify RED:** `scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --test worker_test prepared_job_carries_serving_artifact_lineage`
- [ ] **Commit RED:** `git commit -m "test(spur-context-service): translation-lineage require artifact refs"`
- [ ] **GREEN:** Extend `TranslateLineage`, populate it from `SilverGraphArtifact`, locate the source sidecar in the validated manifest, and strengthen `validate_options` without introducing fallback values.
- [ ] **Verify GREEN:** Run the targeted test, then the full `worker_test` and `translate_test` targets.
- [ ] **SOLVE POST:** Re-run any conditional-required verification on the landed struct fixture.
- [ ] **Commit GREEN:** `git commit -m "feat(spur-context-service): translation-lineage carry serving refs"`

### Task 4: Store serving lineage in the Gold package catalog

**Task ID:** `gold-serving-lineage`

**Files:**
- Modify: `crates/spur-context-service/sql/catalog_tables.sql`
- Modify: `crates/spur-context-service/src/translate.rs`
- Modify: `crates/spur-context-service/tests/translate_test.rs`
- Modify: `crates/spur-context-service/tests/catalog_test.rs`

**Depends on:** `translation-lineage`

**Acceptance Criteria:**
- [ ] `gold.package_catalog` stores all graph-manifest and source-sidecar refs with additive `ADD COLUMN IF NOT EXISTS` migration statements.
- [ ] Metadata insertion is in the same transaction as the existing package-catalog row and rejects incomplete lineage.
- [ ] Schema contract tests and translation round trips assert exact values.
- [ ] Knowledge queries remain backward compatible with pre-migration rows, but such rows are ineligible for serving-registry publication.

**Suggested Worker:** codex

**Scope Boundary:** Gold schema and metadata persistence only. Do not publish the live S3 pointer.

**TDD Implementation:**

- [ ] **SOLVE PRE:** Use the data-integrity catalog to verify required-field and per-package uniqueness constraints for a complete row.
- [ ] **RED:** Add schema-column assertions and a translation round trip for `graph_manifest_uri`, `graph_manifest_sha256`, `graph_manifest_bytes`, `source_sidecar_uri`, `source_sidecar_sha256`, and `source_sidecar_bytes`.
- [ ] **Verify RED:** `scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --test translate_test package_catalog_records_serving_artifacts`
- [ ] **Commit RED:** `git commit -m "test(spur-context-service): gold-serving-lineage require artifact identity"`
- [ ] **GREEN:** Add the columns and bind the already validated `TranslateLineage` values in `write_catalog_metadata`.
- [ ] **Verify GREEN:** Run `translate_test` and `catalog_test` in full.
- [ ] **SOLVE POST:** Verify the landed complete and incomplete row fixtures through the same family rules.
- [ ] **Commit GREEN:** `git commit -m "feat(spur-context-service): gold-serving-lineage persist artifact identity"`

### Task 5: Publish one generation-atomic snapshot and registry pointer

**Task ID:** `generation-publication`

**Files:**
- Modify: `crates/spur-context-service/src/catalog.rs`
- Modify: `crates/spur-context-service/src/serving_registry.rs`
- Modify: `crates/spur-context-service/tests/catalog_test.rs`

**Depends on:** `gold-serving-lineage`

**Acceptance Criteria:**
- [ ] The immutable serving registry is built only from `index_status='complete'` rows for the exact generation.
- [ ] Publication order is snapshot object → snapshot manifest → serving registry → shared live pointer.
- [ ] The live pointer contains strong refs for both the DuckLake snapshot and serving registry and advances once.
- [ ] Conditional S3 publication prevents two writers from overwriting a newer or conflicting same-generation pointer.
- [ ] Missing sidecars, hash mismatches, incomplete packages, rollback generations, and same-generation conflicts leave the old pointer unchanged.

**Suggested Worker:** codex

**Scope Boundary:** Frozen publication and S3 compare-and-swap only. Do not implement Lambda downloads or query handling.

**TDD Implementation:**

- [ ] **SOLVE PRE:** Navigate workflow and data-integrity rules for the publication trace and generation consistency. Use the formal `PUBLICATION-ELIGIBILITY-V1` notebook cell as the authoritative predicate; do not weaken it.
- [ ] **RED:** Add a real fake-S3 publication trace showing a missing sidecar never writes `current.json`:

```rust
let error = publish_generation(&store, generation_with_missing_sidecar()).unwrap_err();
assert_eq!(error.code(), "incomplete_serving_generation");
assert_eq!(store.read("catalog/current.json").unwrap(), previous_pointer_bytes);
```

- [ ] **Verify RED:** `scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --test catalog_test incomplete_registry_does_not_advance_pointer`
- [ ] **Commit RED:** `git commit -m "test(spur-context-service): generation-publication reject partial registry"`
- [ ] **GREEN:** Extend the immutable manifest/pointer with `serving_registry_uri`, `serving_registry_sha256`, and `serving_registry_bytes`; upload the deterministic registry before a conditional pointer write using the observed ETag/version.
- [ ] **Verify GREEN:** Run `catalog_test` in full and the existing frozen-snapshot tests.
- [ ] **SOLVE POST:** Re-run the selected trace/integrity rules against the landed order. Record the proof that there is no trace where `pointer_advanced=true` and any required artifact is incomplete.
- [ ] **Commit GREEN:** `git commit -m "feat(spur-context-service): generation-publication advance one live pointer"`

### Task 6: Extract shared Lambda HTTP and authentication primitives

**Task ID:** `lambda-http-extraction`

**Files:**
- Create: `crates/spur-context-service/src/lambda_http.rs`
- Modify: `crates/spur-context-service/src/lambda.rs`
- Modify: `crates/spur-context-service/src/lib.rs`

**Depends on:** `source-sidecar`

**Acceptance Criteria:**
- [ ] API Gateway request/response types, route parsing, JSON response construction, caller identity, and auth-route guards are reusable without DuckDB.
- [ ] Existing OAuth, API-key, management, abuse, and request-envelope behavior is byte-for-byte compatible.
- [ ] This task is a behavior-preserving extraction; no tool changes are mixed in.

**Suggested Worker:** codex

**Scope Boundary:** Mechanical extraction from `lambda.rs`. Do not add a second handler or change tool eligibility.

**TDD Implementation:**

- [ ] **SOLVE PRE:** Call `solve_rule_spec`; this task should remain TDD-only unless route-state extraction introduces a bounded choice.
- [ ] **RED:** Add characterization tests for OAuth, API-key, management, and direct-tool paths through a new pure `classify_route` API while the function does not yet exist.
- [ ] **Verify RED:** `scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --features lambda --lib lambda_http_contract`
- [ ] **Commit RED:** `git commit -m "test(spur-context-service): lambda-http characterize route parsing"`
- [ ] **GREEN:** Move the smallest shared types/functions, keep public visibility crate-local, and leave `lambda::handler` behavior unchanged.
- [ ] **Verify GREEN:** `scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --features lambda --lib` plus `mcp_test`, `abuse_test`, and `api_keys_test`.
- [ ] **Commit GREEN:** `git commit -m "refactor(spur-context-service): lambda-http extract ingress primitives"`

### Task 7: Materialize and validate generation-scoped S3 artifacts

**Task ID:** `artifact-cache`

**Files:**
- Create: `crates/spur-context-service/src/artifact_cache.rs`
- Modify: `crates/spur-context-service/src/lib.rs`
- Create: `crates/spur-context-service/tests/artifact_cache_test.rs`

**Depends on:** `lambda-http-extraction`, `serving-registry-contract`

**Acceptance Criteria:**
- [ ] Cache keys contain generation, package identity, and SHA-256.
- [ ] Downloads use temp-file → hash/size verify → atomic rename.
- [ ] Concurrent requests for the same artifact coalesce to one fetch.
- [ ] A generation change removes the prior generation directory before serving the new one.
- [ ] Capacity is checked from manifest bytes against the configured `/tmp` capacity, including the in-flight temp file; there is no guessed hidden reserve.
- [ ] Missing/corrupt/oversized artifacts return sanitized retryable errors and never fall back to stale bytes.

**Suggested Worker:** codex

**Scope Boundary:** Fetch/cache primitive with injectable real filesystem test fetcher. Do not open Parquet or handle MCP tools.

**TDD Implementation:**

- [ ] **SOLVE PRE:** Use `resource.aggregate_capacity` or `resource.request_within_limit` if implemented for `resident_bytes + incoming_bytes <= tmp_capacity_bytes`; otherwise use generic constraints after spec/check. The model must feed the test fixture sizes.
- [ ] **RED:** Write concurrent real-file tests for one fetch and a corruption test that leaves no final file:

```rust
let paths = futures::future::join_all((0..2).map(|_| cache.materialize(&artifact))).await;
assert!(paths.iter().all(Result::is_ok));
assert_eq!(fetcher.fetch_count(), 1);
assert!(!cache.final_path(&corrupt_artifact).exists());
```

- [ ] **Verify RED:** `scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --test artifact_cache_test coalesces_same_artifact_download`
- [ ] **Commit RED:** `git commit -m "test(spur-context-service): artifact-cache require atomic coalesced fetch"`
- [ ] **GREEN:** Implement an async fetch trait, AWS S3 implementation, generation directory ownership, checksum verification, and coalescing with shared per-key initialization.
- [ ] **Verify GREEN:** Run `artifact_cache_test` in full.
- [ ] **SOLVE POST:** Re-run capacity verification with landed arithmetic and test witness values.
- [ ] **Commit GREEN:** `git commit -m "feat(spur-context-service): artifact-cache validate generation artifacts"`

### Task 8: Serve catalog and search from Parquet

**Task ID:** `parquet-catalog-search`

**Files:**
- Modify: `crates/spur-context-service/Cargo.toml`
- Create: `crates/spur-context-service/src/code_backend.rs`
- Modify: `crates/spur-context-service/src/mcp.rs`
- Modify: `crates/spur-context-service/tests/mcp_test.rs`

**Depends on:** `generation-publication`, `artifact-cache`

**Acceptance Criteria:**
- [ ] `spur-graph` is a path dependency with `default-features = false` so embedding models are not pulled into Code Lambda.
- [ ] `external_catalog` and `external_code_search` use the immutable registry plus `ParquetClient`, not DuckDB.
- [ ] `external_index` warm-hit lookup and `external_index_status` use the same registry generation.
- [ ] Response JSON matches existing MCP fixtures for populated, empty-search, and selector-error cases.
- [ ] Missing or corrupt generation artifacts fail closed with sanitized retryable errors; they do not return misleading empty catalog/search results.

**Suggested Worker:** codex

**Scope Boundary:** Catalog, index warm lookup/status, and code search only. Do not implement source reads or caller/callee edges.

**TDD Implementation:**

- [ ] **SOLVE PRE:** Reload the architecture solve and verify the route/tool eligibility cell; no new bound may be introduced without a fresh solve.
- [ ] **RED:** Reuse one real `spur-graph` Parquet fixture and assert DuckDB-free backend parity for catalog and search.
- [ ] **Verify RED:** `scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --test mcp_test parquet_backend_matches_catalog_and_search_contracts`
- [ ] **Commit RED:** `git commit -m "test(spur-context-service): parquet-search require mcp parity"`
- [ ] **GREEN:** Implement a `CodeBackend` that resolves a package in one `ServingRegistry`, materializes its graph directory, opens `ParquetClient`, and maps existing request/response DTOs without SQL.
- [ ] **Verify GREEN:** Run the targeted test and the complete `mcp_test` target.
- [ ] **SOLVE POST:** Re-run tool eligibility against the landed dispatch table.
- [ ] **Commit GREEN:** `git commit -m "feat(spur-context-service): parquet-search serve catalog and symbols"`

### Task 9: Serve reads and graph edges from Parquet plus source sidecar

**Task ID:** `parquet-read-edges`

**Files:**
- Modify: `crates/spur-context-service/src/code_backend.rs`
- Modify: `crates/spur-context-service/src/source_sidecar.rs`
- Modify: `crates/spur-context-service/src/mcp.rs`
- Modify: `crates/spur-context-service/tests/mcp_test.rs`

**Depends on:** `parquet-catalog-search`

**Acceptance Criteria:**
- [ ] `external_code_read` resolves the symbol/file through `ParquetClient` and retrieves exact text from the sidecar by `(file_path, content_oid)`.
- [ ] `external_code_callers` and `external_code_callees` use `GraphQueryClient` edge APIs and preserve direct/dynamic/reference edge kinds.
- [ ] Ambiguity, missing source text, content-OID mismatch, and corrupt sidecar are explicit sanitized errors.
- [ ] Existing response-shape fixtures pass for all four code tools.

**Suggested Worker:** codex

**Scope Boundary:** The remaining code tools only. Do not modify Lambda entrypoints, caller URL routing, or Terraform.

**TDD Implementation:**

- [ ] **SOLVE PRE:** Call the catalog; this is path behavior unless a new result or traversal cap is proposed. Existing request caps must be preserved, not re-picked.
- [ ] **RED:** Add one real artifact fixture with a known caller edge and source text; assert exact `external_code_read`, callers, and callees JSON.
- [ ] **Verify RED:** `scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --test mcp_test parquet_backend_matches_read_and_edge_contracts`
- [ ] **Commit RED:** `git commit -m "test(spur-context-service): parquet-read require source and edge parity"`
- [ ] **GREEN:** Use `resolve_selector`, symbol/file APIs, caller/callee APIs, and sidecar OID validation. Keep response mapping at the current MCP boundary.
- [ ] **Verify GREEN:** Run the targeted test and full `mcp_test`.
- [ ] **Commit GREEN:** `git commit -m "feat(spur-context-service): parquet-read serve source and edges"`

### Task 10: Build separate Code and Knowledge Lambda binaries

**Task ID:** `serving-lambda-binaries`

**Files:**
- Modify: `crates/spur-context-service/Cargo.toml`
- Modify: `crates/spur-context-service/src/lib.rs`
- Modify: `crates/spur-context-service/src/lambda.rs`
- Create: `crates/spur-context-service/src/bin/code_lambda.rs`
- Create: `crates/spur-context-service/src/bin/knowledge_lambda.rs`
- Modify: `crates/spur-context-service/src/main.rs`

**Depends on:** `parquet-read-edges`, `lambda-http-extraction`

**Acceptance Criteria:**
- [ ] `code-lambda` feature/binary compiles without `duckdb`, DuckLake extensions, or embedding dependencies.
- [ ] `knowledge-lambda` feature/binary retains the current frozen DuckLake snapshot behavior.
- [ ] Code accepts catalog, index, index-status, search, read, callers, and callees; it rejects knowledge.
- [ ] Knowledge accepts only `external_knowledge_context`; it rejects code/catalog/index tools.
- [ ] Missing/corrupt Code artifacts are sanitized retryable failures, not empty success.
- [ ] Exactly one handler executes for each direct API request.

**Suggested Worker:** codex

**Scope Boundary:** Rust feature graph and two serving handlers. Do not change caller URLs, Terraform, or deploy packaging.

**TDD Implementation:**

- [ ] **SOLVE PRE:** Reload `sol_e486929ddb034f79`; run workflow verification for the finite tool-routing table if available. Required witness: two serving Lambdas, one invocation/request, Code DuckDB flag zero.
- [ ] **RED:** Add handler tests for the complete allow/deny matrix and a feature-graph assertion that `code-lambda` does not enable `duckdb`.
- [ ] **Verify RED:** `scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --no-default-features --features code-lambda --lib code_lambda_tool_eligibility`
- [ ] **Commit RED:** `git commit -m "test(spur-context-service): serving-lambdas require split eligibility"`
- [ ] **GREEN:** Split features into shared serving, Code, Knowledge, and worker closures; make both binaries call shared ingress with an explicit backend kind. Keep the legacy `spur-context-service` binary as a temporary Knowledge alias only until deploy packaging switches.
- [ ] **Verify GREEN:**
  - `scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --no-default-features --features code-lambda --bin spur-context-code-lambda`
  - `scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --no-default-features --features knowledge-lambda --bin spur-context-knowledge-lambda`
  - `scripts/spur-cargo tree --manifest-path crates/spur-context-service/Cargo.toml --no-default-features --features code-lambda --edges normal --prefix none` and fail the task if output contains a `duckdb` package.
- [ ] **SOLVE POST:** Re-run the route workflow with the landed dispatch table and feature flags.
- [ ] **Commit GREEN:** `git commit -m "feat(spur-context-service): serving-lambdas split code and knowledge"`

### Task 11: Route the client directly by external tool

**Task ID:** `caller-direct-routing`

**Files:**
- Modify: `crates/spur-core/src/mcp/context_service.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] One configured origin produces explicit Code and Knowledge endpoint URLs for OAuth, API-key, and unauthenticated modes.
- [ ] Catalog/index/status/code tools select `/code`; knowledge selects `/knowledge`.
- [ ] Every call performs exactly one HTTP POST; there is no probe, retry-to-other-backend, or router hop.
- [ ] Existing token secrecy and authorization headers are unchanged.

**Suggested Worker:** codex

**Scope Boundary:** `ContextServiceClient` URL construction and its inline tests only. Do not edit the server, infra, or generic MCP registry.

**TDD Implementation:**

- [ ] **SOLVE PRE:** Reload the architecture solve and verify `caller_can_route_by_tool=true`, `caller_route_changes=1`, and `lambda_invocations_per_request=1`.
- [ ] **RED:** Extend the real HTTP test server to capture URLs and counts:

```rust
assert_eq!(client.endpoint_for("external_code_read").path(), "/mcp/oauth/code");
assert_eq!(client.endpoint_for("external_knowledge_context").path(), "/mcp/oauth/knowledge");
assert_eq!(server.request_count(), 2);
```

- [ ] **Verify RED:** `scripts/spur-cargo test -p spur-core context_service_routes_external_tools_directly`
- [ ] **Commit RED:** `git commit -m "test(spur-core): context-routing require direct tool paths"`
- [ ] **GREEN:** Store Code and Knowledge endpoints or derive them once in construction; use a closed tool classifier in `call_value` and keep one request path.
- [ ] **Verify GREEN:** `scripts/spur-cargo test -p spur-core context_service`
- [ ] **SOLVE POST:** Re-run the exact landed classifier through the route workflow proof.
- [ ] **Commit GREEN:** `git commit -m "feat(spur-core): context-routing dispatch code and knowledge directly"`

### Task 12: Split serving compute without doubling warm cost

**Task ID:** `terraform-serving-compute`

**Files:**
- Modify: `infra/spur-context-service/main.tf`
- Modify: `infra/spur-context-service/iam.tf`
- Modify: `infra/spur-context-service/variables.tf`
- Modify: `infra/spur-context-service/outputs.tf`
- Modify: `tests/scripts/test_spur_context_service_deploy.py`

**Depends on:** `serving-lambda-binaries`

**Acceptance Criteria:**
- [ ] Terraform defines exactly two serving functions: `spur-context-code` and `spur-context-knowledge`.
- [ ] Other worker/source-fetcher/authorizer/cleanup functions remain unchanged and are not counted as serving functions.
- [ ] Code receives only registry/artifact S3 read permissions and has no catalog secret or DuckDB extension configuration.
- [ ] Knowledge retains frozen snapshot/extension configuration.
- [ ] Existing `concurrent_warm_instances` provisioned-concurrency budget is attached only to Knowledge; Code provisioned concurrency is zero, so the split does not double fixed warm cost.
- [ ] No new serving reserved-concurrency setting is introduced. Tests document that reserved concurrency is a capacity limit, while provisioned concurrency is the billable warm pool.
- [ ] Code `/tmp` capacity is explicit and the cache receives the exact byte capacity through environment configuration.

**Suggested Worker:** codex

**Scope Boundary:** Lambda resources, IAM, logs, aliases, variables, and outputs. Do not edit API routes or packaging.

**TDD Implementation:**

- [ ] **SOLVE PRE:** Use resource rules to verify two serving assignments and `code_warm + knowledge_warm <= existing_warm_budget`, with `code_warm=0`. Use the AWS minimum ephemeral storage value already imposed by the platform; do not invent a second cache reserve.
- [ ] **RED:** Add Python assertions for two serving resources, isolated roles/env, one Knowledge-only provisioned concurrency resource, and exact cache capacity wiring.
- [ ] **Verify RED:** `uv run --with pytest pytest tests/scripts/test_spur_context_service_deploy.py -q -k serving_compute`
- [ ] **Commit RED:** `git commit -m "test(context-infra): serving-compute require two bounded lambdas"`
- [ ] **GREEN:** Replace the one service Lambda resource with Code and Knowledge resources and preserve the total existing warm-instance count on Knowledge only.
- [ ] **Verify GREEN:** Run the complete deployment test file and `terraform -chdir=infra/spur-context-service fmt -check -recursive`.
- [ ] **SOLVE POST:** Verify the landed Terraform defaults and tfvars stay inside the existing warm budget.
- [ ] **Commit GREEN:** `git commit -m "feat(context-infra): serving-compute split code and knowledge"`

### Task 13: Wire direct authenticated API routes

**Task ID:** `terraform-direct-routes`

**Files:**
- Modify: `infra/spur-context-service/main.tf`
- Modify: `infra/spur-context-service/api_keys.tf`
- Modify: `tests/scripts/test_spur_context_service_deploy.py`

**Depends on:** `terraform-serving-compute`, `caller-direct-routing`

**Acceptance Criteria:**
- [ ] OAuth routes are `POST /mcp/oauth/code` and `POST /mcp/oauth/knowledge`.
- [ ] API-key routes are `POST /mcp/api-key/code` and `POST /mcp/api-key/knowledge`.
- [ ] If unauthenticated compatibility is enabled, its routes are `POST /mcp/code` and `POST /mcp/knowledge`.
- [ ] Management/login/key lifecycle routes continue to target Code.
- [ ] Each tool path has exactly one API Gateway integration target; no Lambda invokes the other Lambda.
- [ ] Route authorization modes remain identical to their existing parent routes.

**Suggested Worker:** codex

**Scope Boundary:** API Gateway integrations/routes and static tests only. Do not change Lambda compute, client classification, or deployment packaging.

**TDD Implementation:**

- [ ] **SOLVE PRE:** Verify the full route table with `workflow.transition_allowed`/bounded trace if applicable and the formal `EXTERNAL-TOOL-ROUTING-V1` cell.
- [ ] **RED:** Add static tests that map every external tool class to exactly one integration and reject a router integration or cross-Lambda invoke permission.
- [ ] **Verify RED:** `uv run --with pytest pytest tests/scripts/test_spur_context_service_deploy.py -q -k direct_routes`
- [ ] **Commit RED:** `git commit -m "test(context-infra): direct-routes require one serving target"`
- [ ] **GREEN:** Add explicit integrations and route keys, retaining existing authorizers and control routes.
- [ ] **Verify GREEN:** Run the complete deployment test file and Terraform format check.
- [ ] **SOLVE POST:** Re-run the landed route table; record one terminal serving target for every tool.
- [ ] **Commit GREEN:** `git commit -m "feat(context-infra): direct-routes map code and knowledge"`

### Task 14: Package distinct Code and Knowledge ZIPs

**Task ID:** `serving-packaging`

**Files:**
- Modify: `infra/spur-context-service/deploy.sh`
- Modify: `tests/scripts/test_spur_context_service_deploy.py`
- Modify: `tests/scripts/test_spur_context_service_ecr.py` only if shared deploy helpers require it

**Depends on:** `serving-lambda-binaries`, `terraform-direct-routes`

**Acceptance Criteria:**
- [ ] Deploy builds `spur-context-code-lambda.zip` and `spur-context-knowledge-lambda.zip` from their exact binaries/features.
- [ ] Code ZIP contains no DuckDB/DuckLake/httpfs/aws extension files.
- [ ] Knowledge ZIP retains pinned, checksum-verified extensions.
- [ ] Terraform receives distinct immutable ZIP paths and hashes.
- [ ] Existing worker image publication and ECR hardening are unchanged.

**Suggested Worker:** codex

**Scope Boundary:** Build/package/deploy-script plumbing and source-level tests. Do not deploy or modify API routes.

**TDD Implementation:**

- [ ] **SOLVE PRE:** Call the catalog; this task is TDD-only unless packaging introduces a new numeric size cap.
- [ ] **RED:** Add tests that inspect both produced ZIP manifests and explicitly reject DuckDB extension names in Code.
- [ ] **Verify RED:** `uv run --with pytest pytest tests/scripts/test_spur_context_service_deploy.py -q -k serving_zip`
- [ ] **Commit RED:** `git commit -m "test(context-infra): serving-packaging require duckdb-free code zip"`
- [ ] **GREEN:** Parameterize binary build/package helpers, run the extension copy only for Knowledge, and pass both ZIP paths to Terraform.
- [ ] **Verify GREEN:** Run both Python deployment suites and `bash -n infra/spur-context-service/deploy.sh`.
- [ ] **Commit GREEN:** `git commit -m "feat(context-infra): serving-packaging build two lambda zips"`

### Task 15: Deploy to staging and double-check every external tool with AWS CLI

**Task ID:** `staging-external-smoke`

**Files:**
- Modify: `infra/spur-context-service/smoke-staging-e2e.py`
- Modify: `infra/spur-context-service/smoke-staging-e2e.sh`
- Create: `tests/scripts/test_spur_context_service_external_routes.py`

**Depends on:** `serving-packaging`, `terraform-direct-routes`

**Acceptance Criteria:**
- [ ] Local contract tests cover catalog, index, index-status, search, read, callers, callees, and knowledge with real response-shape chaining (search result feeds read/edge selectors).
- [ ] The staging deployment is the only external mutation; production is never targeted.
- [ ] AWS CLI verifies two serving function configurations, route integrations, role separation, Code’s absent provisioned-concurrency config, and Knowledge’s warm count within the pre-split budget.
- [ ] Staging smoke invokes all external tools, confirms Code versus Knowledge log destinations, and observes one serving invocation per request.
- [ ] A known package is used; `external_index` is idempotent and the script waits for a complete status before query tools.
- [ ] Missing credentials/config produce a beads `blocked` signal with the exact failed preflight, never a fabricated pass.

**Suggested Worker:** codex

**Scope Boundary:** Staging harness, staging deployment, and read-only AWS CLI cross-checks after deployment. Never deploy or mutate production. Do not change Rust/Terraform behavior in this task; regressions return to the owning predecessor task.

**TDD Implementation:**

- [ ] **SOLVE PRE:** Re-run the architecture and route proofs as a final preflight; do not deploy on `unknown`, `timeout`, or a changed optimum.
- [ ] **RED:** Add offline fixture tests for route/config parsing and response chaining before editing the smoke harness.
- [ ] **Verify RED:** `uv run --with pytest pytest tests/scripts/test_spur_context_service_external_routes.py -q`
- [ ] **Commit RED:** `git commit -m "test(context-infra): staging-smoke cover every external tool"`
- [ ] **GREEN:** Extend the harness to:
  1. preflight `aws sts get-caller-identity` and the staging region/account;
  2. deploy with `infra/spur-context-service/deploy.sh staging`;
  3. query `aws lambda get-function-configuration` for Code and Knowledge;
  4. query `aws apigatewayv2 get-routes` and `get-integrations`;
  5. invoke `external_index`, poll `external_index_status`, then exercise catalog/search/read/callers/callees/knowledge;
  6. query bounded CloudWatch log windows for the returned correlation IDs and assert one serving function handled each request.
- [ ] **Verify GREEN:** Run the offline test, `bash -n` on the harness, then the staging smoke. Capture exact AWS CLI commands, request IDs, and summarized results in the beads completion audit.
- [ ] **SOLVE POST:** Re-run the landed routing facts after the staging evidence and record the solve IDs.
- [ ] **Commit GREEN:** `git commit -m "test(context-infra): staging-smoke verify split serving path"`

## Dependency DAG

```text
serving-registry-contract
    └── source-sidecar
        ├── translation-lineage
        │   └── gold-serving-lineage
        │       └── generation-publication ────────────────┐
        └── lambda-http-extraction                         │
            └── artifact-cache ────────────────────────────┤
                                                          v
                                                parquet-catalog-search
                                                          │
                                                parquet-read-edges
                                                          │
                                                serving-lambda-binaries
                                                          │
                                                terraform-serving-compute
                                                          │
caller-direct-routing ─────────────────────────> terraform-direct-routes
                                                          │
                                                serving-packaging
                                                          │
                                                staging-external-smoke
```

`generation-publication` and `artifact-cache` converge before the Parquet backend. `caller-direct-routing` is intentionally independent until Terraform route wiring. Tasks that touch `Cargo.toml`, `lib.rs`, `worker.rs`, `lambda.rs`, or deployment tests are sequenced to avoid sibling overlay conflicts.

## Final verification gate

The plan is complete only when all of the following are freshly observed:

```bash
scripts/spur-cargo fmt --manifest-path crates/spur-context-service/Cargo.toml -- --check
scripts/spur-cargo test --manifest-path crates/spur-context-service/Cargo.toml --all-features
scripts/spur-cargo test -p spur-core context_service
uv run --with pytest pytest tests/scripts/test_spur_context_service_deploy.py tests/scripts/test_spur_context_service_ecr.py tests/scripts/test_spur_context_service_external_routes.py -q
terraform -chdir=infra/spur-context-service fmt -check -recursive
bash -n infra/spur-context-service/deploy.sh
bash -n infra/spur-context-service/smoke-staging-e2e.sh
```

The review must also confirm the Code feature graph and ZIP are DuckDB-free, every worker recorded RED before GREEN, all relevant post-solves remain conclusive, the staging AWS CLI evidence names only staging resources, and unrelated pre-existing worktree changes were not included.
