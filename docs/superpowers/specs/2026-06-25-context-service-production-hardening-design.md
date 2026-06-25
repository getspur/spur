# Context Service Production Hardening Design

**Date:** 2026-06-25
**Status:** approved direction, written for user review
**Scope:** production hardening for `crates/spur-context-service` and `infra/spur-context-service` blockers found in the on-demand indexing path.
**Builds on:**
- `docs/superpowers/specs/2026-06-22-code-context-service-design.md`
- `docs/superpowers/specs/2026-06-24-context-service-on-demand-indexing-design.md`

---

## Goal

Make the on-demand indexing path production-safe for the four confirmed blockers:

1. job state must be durable and visible across Lambda and ECS worker processes
2. `external_index_status` must have a real recovery path for stale queued/running jobs
3. the deployed worker image must contain every runtime binary the worker executes
4. concurrent index jobs must not lose DuckLake catalog updates

The design keeps the existing query surface and DuckLake data model. It separates the mutable control plane from the DuckLake data plane.

---

## Non-Goals

- no redesign of `external_code_search`, `external_code_read`, `external_code_callers`, `external_code_callees`, or `external_knowledge_context`
- no public API auth/WAF/throttling work in this spec
- no replacement of Step Functions with a different executor
- no move away from DuckLake/S3 for indexed code data
- no broad refactor of `translate.rs` beyond the lease/upload boundary needed for correctness

---

## Current Problems

### 1. Job rows are process-local

`CatalogResolver::from_connection` calls `ensure_index_jobs_table`, but that function switches to `memory` before creating the table. `jobs::insert` and `jobs::update_status` then explicitly use `memory.index_jobs`.

That means Lambda and the worker each see a different in-memory table. A worker can complete successfully while the API-visible job row remains stale, or disappears on a Lambda cold start.

### 2. Status reconciliation cannot repair production jobs

`route_index` currently inserts jobs with `execution_arn: None`, then returns the Step Functions ARN only in the response. `update_stale_job` cannot reconcile without an execution ARN, and production Lambda calls `route_index_status` without an `ExecutionStatusChecker`.

The tests cover a reconciliation helper, but the deployed path does not wire it.

### 3. Worker deploy paths disagree about image contents

`worker::build_graph` executes `spur graph build`. The documented `deploy.sh` worker image copies only `spur-context-worker`. A separate remote build script includes `spur`, but it is not the canonical deploy path.

The production worker image must be defined once and smoke-tested before Terraform updates the task definition.

### 4. Catalog upload is last-writer-wins

For S3 catalog DSNs, the worker downloads `catalog.ducklake`, mutates it locally, and uploads the whole file back to the same S3 key. If two workers do that concurrently, the later upload can erase the earlier catalog metadata.

The expensive fetch/build phase can run in parallel, but catalog mutation and upload must be serialized.

---

## First-Principles Model

There are two kinds of state:

- **Data plane:** indexed code data, DuckLake catalog metadata, and Parquet files. This stays in DuckLake/S3.
- **Control plane:** job identity, dedupe, status, stage, execution ARN, attempts, and catalog-write leases. This requires small atomic conditional updates across Lambda and ECS. It moves to DynamoDB.

The data plane is optimized for query and storage. The control plane is optimized for coordination. Mixing them is the root cause of blockers 1, 2, and 4.

---

## Approaches Considered

### Option A: Minimal patch in the existing DuckDB/DuckLake path

This would move `index_jobs` out of `memory`, persist `execution_arn`, and patch deploy scripts.

It is too weak for production because DuckLake/S3 catalog writes still need external concurrency control, and job rows remain coupled to the catalog backend's write constraints.

### Option B: DynamoDB control plane with DuckLake data plane

This is the chosen design.

DynamoDB owns job state, idempotency, status recovery metadata, and catalog leases. DuckLake remains the indexed-code data plane. Step Functions remains the executor.

This fixes the blockers without rewriting the query path.

### Option C: Read-only service with offline/admin indexing only

This avoids most coordination problems by removing agent-triggered writes. It is simpler operationally, but it drops the v2 on-demand indexing goal.

This is a fallback if on-demand indexing is intentionally deferred, not the target design.

---

## Chosen Design

### 1. Add a DynamoDB control plane

Terraform creates two DynamoDB tables:

- `spur-context-index-jobs`
- `spur-context-catalog-leases`

Both tables use on-demand billing, point-in-time recovery, server-side encryption, and TTL for cleanup fields.

`spur-context-index-jobs` is a single-table model with two item types:

| Item type | PK | Purpose |
|---|---|---|
| job | `JOB#<job_id>` | full status record returned by `external_index_status` |
| dedupe | `DEDUP#<source>#<package>#<revision>#<source_url_hash>` | active-request idempotency pointer to the owning `job_id` |

Job item fields:

- `job_id`
- `status`: `queued`, `running`, `complete`, `failed`
- `source`, `package`, `revision`, `source_url`, `source_url_hash`, `source_kind`
- `execution_arn`
- `attempt`
- `stage`
- `caller_id`
- `snapshot_id`
- `row_counts`
- `error_code`, `error_detail`
- `created_at`, `updated_at`, `started_at`, `completed_at`
- `expires_at`

The dedupe item is created in the same `TransactWriteItems` call as the job item with `attribute_not_exists(pk)`. If the transaction conflicts, Lambda reads the dedupe item and returns the existing active job.

On terminal failure, the worker or reconciler deletes the dedupe item conditionally if it still points to the same job, allowing a later retry. On terminal success, the worker deletes the dedupe item conditionally after the catalog commit is visible; future requests are satisfied by the warm `package_catalog` lookup.

### 2. Make DynamoDB the normal status source

`external_index_status({job_id})` reads `JOB#<job_id>` from DynamoDB.

For active jobs, the response remains backward-compatible and may add fields:

- `attempt`
- `stage`
- `updated_at`
- `execution_arn`
- `error.detail`

Existing clients that only read `status`, `job_id`, `snapshot_id`, `revision`, or `row_counts` continue to work.

### 3. Use Step Functions as a recovery oracle

Lambda stores `execution_arn` in the DynamoDB job item immediately after `StartExecution`.

If `external_index_status` sees `queued` or `running` and `updated_at` is older than 60 seconds, it calls `DescribeExecution(execution_arn)`.

Repair rules:

- `RUNNING`: keep DynamoDB status, optionally refresh `updated_at`
- `SUCCEEDED`: parse Step Functions output and mark the job `complete`
- `FAILED`, `TIMED_OUT`, `ABORTED`: mark the job `failed` with normalized error details and release the dedupe item

If `DescribeExecution` fails transiently, return the DynamoDB status rather than failing the API request.

### 4. Serialize only the catalog mutation/upload phase

The worker still runs fetch and graph build in parallel across jobs.

Before it downloads and mutates the DuckLake catalog, it acquires a DynamoDB lease keyed by catalog URI:

`CATALOG#<sha256(catalog_s3_uri)>`

Lease fields:

- `lease_key`
- `catalog_uri`
- `owner_job_id`
- `lease_token`
- `expires_at`
- `created_at`
- `updated_at`
- `fencing_counter`

Acquire condition:

- lease does not exist, or
- `expires_at < now`, or
- the same owner/token is renewing

Worker behavior:

1. update job `stage = "waiting_catalog_lease"`
2. acquire lease with a 10-minute expiry
3. update job `stage = "translating"`
4. download catalog, recording S3 ETag/version when available
5. translate artifact into the local catalog
6. renew/assert lease before upload
7. upload catalog with a required S3 conditional write
8. update job `complete`
9. release lease conditionally by `lease_token`

The worker renews the lease every 60 seconds while translating and asserts the lease immediately before upload. If the worker loses the lease before upload, it fails the job with a retryable catalog-conflict error. Step Functions retry policy can run the job again after backoff.

### 5. Add S3 catalog write defense-in-depth

Terraform enables S3 bucket versioning for the catalog/data bucket.

For S3 catalog DSNs, `CatalogDownload::fetch` records the current object ETag and version ID when S3 returns them. `CatalogDownload::upload` must use a conditional write (`If-Match` for existing objects, or `If-None-Match: *` for first create). If the Rust SDK path cannot express the required condition, implementation must use a small signed S3 request helper rather than silently falling back to unconditional `PutObject`.

The DynamoDB lease remains the primary concurrency control. S3 conditional writes protect against accidental writes outside the lease path.

### 6. Canonicalize the worker image build

`deploy.sh` becomes the only documented worker image path.

The image must include:

- `spur-context-worker`
- `spur`
- `git`
- `curl`
- `tar`
- `unzip`
- `ca-certificates`
- DuckDB CLI/extensions only if the current worker path still invokes them

`build-and-push-remote.sh` is either removed or rewritten as a thin wrapper around the same image assembly logic.

Before Terraform applies the new task definition, deploy performs a smoke test against the built image:

- `spur --version`
- `spur-context-worker` env validation path with intentionally missing env, expecting a controlled error

### 7. Remove hard-coded service bucket paths from worker writes

The worker derives data and checkpoint paths from configuration passed by Terraform/Step Functions.

Rules:

- no hard-coded `s3://spur-context/data/` inside worker write paths
- no hard-coded checkpoint bucket default in production task definitions
- Step Functions passes `SPUR_CATALOG_S3_URI`, `SPUR_CONTEXT_DUCKLAKE_DATA_PATH`, and checkpoint URI/prefix explicitly

---

## Component Changes

### `crates/spur-context-service/src/jobs.rs`

Replace DuckDB-backed job CRUD with a DynamoDB-backed store.

Expose a trait so unit tests can use an in-memory fake:

- `create_or_get_active_job`
- `record_execution_started`
- `mark_running`
- `update_stage`
- `mark_complete`
- `mark_failed`
- `lookup_job`
- `release_dedupe`

The existing `JobStatus` enum remains, with response serialization kept stable.

### `crates/spur-context-service/src/mcp.rs`

`route_index` changes to:

1. validate args and source URL
2. warm-check `package_catalog`
3. create or get an active DynamoDB job
4. if newly created, start Step Functions
5. persist `execution_arn`
6. return existing response fields plus additive debug fields

`route_index_status` changes to:

1. read the DynamoDB job
2. optionally reconcile stale active jobs through Step Functions
3. return a backward-compatible JSON response

### `crates/spur-context-service/src/lambda.rs`

Initialize DynamoDB and Step Functions clients once per warm Lambda environment.

The Lambda path wires production status reconciliation instead of only calling the helper used by tests.

### `crates/spur-context-service/src/worker.rs`

Move job progress updates from DuckDB `memory.index_jobs` to DynamoDB.

Wrap only the catalog mutation/upload phase with the DynamoDB lease.

Keep existing source validation, archive traversal checks, fetch logic, graph build subprocess, and translate flow.

### `crates/spur-context-service/src/catalog.rs`

Stop creating `memory.index_jobs` during `CatalogResolver::from_connection`.

Catalog connection setup should only attach DuckLake and ensure/query catalog schema as needed.

### `infra/spur-context-service`

Add:

- DynamoDB job table
- DynamoDB catalog lease table
- Lambda IAM for DynamoDB job reads/writes and lease reads when status repair needs it
- ECS task IAM for DynamoDB job writes and lease writes
- S3 bucket versioning
- explicit worker environment variables for catalog/data/checkpoint paths
- canonical worker image build/deploy script path

---

## Data Flow

### `external_index`

1. Lambda validates request and rejects unsafe URLs before creating a job.
2. Lambda queries `package_catalog`; if present, returns `{status: "complete"}`.
3. Lambda computes `dedupe_key`.
4. Lambda transactionally creates `JOB#...` and `DEDUP#...`, or returns the active existing job.
5. Lambda starts Step Functions for newly-created jobs.
6. Lambda records `execution_arn`.
7. Lambda returns `{status: "queued", job_id, execution_arn, revision}`.

### Worker

1. read job env from Step Functions
2. mark `running`, stage `fetch_source`
3. fetch source
4. mark stage `build_graph`
5. run `spur graph build`
6. mark stage `waiting_catalog_lease`
7. acquire catalog lease
8. mark stage `translate`
9. download/translate/upload catalog under lease
10. mark `complete`, release dedupe, release lease, send task success

On failure, the worker marks `failed` where possible, releases dedupe if owned, releases the lease if owned, and sends task failure.

### `external_index_status`

1. read job from DynamoDB
2. if active and stale, call `DescribeExecution`
3. repair terminal state if Step Functions is terminal
4. return current status response

---

## Error Handling

- DynamoDB conditional conflict on dedupe returns the existing active job.
- Failure to start Step Functions marks the job `failed` and releases dedupe.
- Failure to record `execution_arn` after successful `StartExecution` marks the job `failed` if possible and returns an internal error; this state should alarm because an orphan execution may exist.
- Lease acquisition timeout marks the job failed with `catalog_lease_timeout`.
- Lease loss before upload marks the job failed with `catalog_lease_lost`.
- S3 conditional upload failure marks the job failed with `catalog_write_conflict`; Step Functions retry may recover.
- `external_index_status` returns DynamoDB state if Step Functions repair fails transiently.

---

## Rollout Plan

1. Add DynamoDB tables and IAM without removing current code.
2. Introduce DynamoDB store and lease abstractions behind traits.
3. Switch Lambda `external_index` and status reads to DynamoDB.
4. Switch worker progress updates to DynamoDB.
5. Add catalog lease around translate/upload.
6. Canonicalize worker image build and add smoke test.
7. Remove `memory.index_jobs` runtime creation and DuckDB job CRUD.
8. Update docs and Terraform outputs.

No data migration is required for in-memory job rows. Existing indexed packages stay in DuckLake.

---

## Testing

### Unit tests

- dedupe transaction creates one job for concurrent identical requests
- failed jobs release dedupe for retry
- complete jobs preserve response shape
- stale active job reconciles from Step Functions `SUCCEEDED`
- stale active job reconciles from Step Functions `FAILED`
- lease acquire/renew/release honors token ownership and expiry
- lease loss blocks upload

### Integration-style tests with fakes

- `route_index` starts Step Functions only for newly-created jobs
- `route_index_status` returns DynamoDB state when `DescribeExecution` is unavailable
- worker marks stages in order and releases lease on success
- worker releases lease and dedupe on failure
- image smoke command fails if `spur` is absent

### AWS ignored tests

Add an ignored end-to-end test or script that runs against a staging stack:

1. call `external_index`
2. wait for Step Functions execution
3. poll `external_index_status`
4. verify `complete`
5. query the indexed revision through `external_code_search`

### Verification commands

- `scripts/spur-cargo --workdir crates/spur-context-service test --features lambda,worker`
- `terraform -chdir=infra/spur-context-service validate`
- worker image smoke test from `deploy.sh`

---

## Success Criteria

- Lambda cold starts no longer erase visible job status.
- ECS worker completion is visible through `external_index_status`.
- A stale active job can be repaired from Step Functions state.
- Documented deploy path produces an image that contains `spur`.
- Two concurrent jobs cannot upload the same catalog object without lease coordination.
- Public MCP response shapes remain backward-compatible except for additive fields.
