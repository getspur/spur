# Context Service Production Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move `spur-context-service` on-demand indexing to a DynamoDB-backed production control plane, add catalog-write serialization, and make the deployed worker image contain the binaries it executes.

**Architecture:** DynamoDB owns mutable control-plane state: job records, idempotency pointers, progress, execution ARNs, and catalog leases. DuckLake/S3 remains the data plane for indexed code. Step Functions remains the executor and becomes a recovery oracle for stale active jobs.

**Tech Stack:** Rust 2021, `aws-sdk-dynamodb`, `aws-sdk-sfn`, `aws-sdk-s3`, DuckDB/DuckLake, Terraform, Step Functions, ECS/Fargate, `scripts/spur-cargo`.

---

## Ground Rules

- Preserve unstaged working-tree changes in `crates/spur-context-service/src/translate.rs` and `crates/spur-context-service/src/worker.rs`; they fix DuckLake flush/checkpoint and switch the worker translate path back to the Rust API.
- Do not use bare `cargo`; use `scripts/spur-cargo`.
- Keep public MCP responses backward-compatible. Add fields only.
- Commit each task independently with the repository commit format.
- Run focused tests for each task and the final full crate test before marking done.

## File Map

- `crates/spur-context-service/Cargo.toml`: add `aws-sdk-dynamodb` dependency if not already present.
- `crates/spur-context-service/src/jobs.rs`: replace DuckDB `memory.index_jobs` CRUD with DynamoDB-backed job store and fakes for tests.
- `crates/spur-context-service/src/mcp.rs`: wire `external_index` and `external_index_status` to the job store and Step Functions reconciliation.
- `crates/spur-context-service/src/lambda.rs`: initialize and pass DynamoDB/Step Functions clients on the production path.
- `crates/spur-context-service/src/worker.rs`: move progress updates to DynamoDB, add catalog lease acquisition/renew/release, and conditional catalog upload.
- `crates/spur-context-service/src/catalog.rs`: stop creating `memory.index_jobs`.
- `crates/spur-context-service/tests/*.rs`: update job, MCP, worker, catalog tests for DynamoDB fakes and lease behavior.
- `infra/spur-context-service/*.tf`: add DynamoDB tables, IAM, S3 versioning, worker env vars.
- `infra/spur-context-service/deploy.sh`: make the worker image include `spur` and add smoke tests.
- `infra/spur-context-service/build-and-push-remote.sh`: remove or make it a thin wrapper around the canonical image build.
- `tests/scripts/test_spur_context_service_deploy.py`: pin deploy script behavior.

---

### Task 1: DynamoDB Job Store Foundation

**Files:**
- Modify: `crates/spur-context-service/Cargo.toml`
- Modify: `crates/spur-context-service/src/jobs.rs`
- Modify: `crates/spur-context-service/tests/jobs_test.rs`

- [ ] **Step 1: Write failing job-store tests**

Add tests that exercise the new control-plane contract without AWS. Keep the fake in `tests/jobs_test.rs` or in a `#[cfg(test)]` module in `jobs.rs`.

Required test names:

```rust
#[test]
fn create_or_get_active_job_dedupes_identical_requests() { /* fake store */ }

#[test]
fn failed_job_releases_dedupe_for_retry() { /* fake store */ }

#[test]
fn record_execution_started_persists_execution_arn() { /* fake store */ }

#[test]
fn complete_job_preserves_status_response_fields() { /* fake store */ }
```

Run:

```bash
scripts/spur-cargo --workdir crates/spur-context-service test --test jobs_test --features lambda,worker
```

Expected: FAIL because the DynamoDB store types and methods do not exist yet.

- [ ] **Step 2: Add core job types and trait**

Replace the DuckDB-first API in `jobs.rs` with a store abstraction. Keep `JobStatus` names stable.

Target shape:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Running,
    Complete,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobKey {
    pub source: String,
    pub package: String,
    pub revision: String,
    pub source_url_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateJobRequest {
    pub source: String,
    pub package: String,
    pub revision: String,
    pub source_url: String,
    pub source_url_hash: String,
    pub source_kind: String,
    pub caller_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobRecord {
    pub job_id: String,
    pub status: JobStatus,
    pub source: String,
    pub package: String,
    pub revision: String,
    pub source_url: String,
    pub source_url_hash: String,
    pub source_kind: String,
    pub execution_arn: Option<String>,
    pub attempt: u32,
    pub stage: Option<String>,
    pub snapshot_id: Option<i64>,
    pub row_counts: Option<serde_json::Value>,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CreateJobOutcome {
    Created(JobRecord),
    Existing(JobRecord),
}

#[async_trait::async_trait]
pub trait JobStore: Send + Sync {
    async fn create_or_get_active_job(
        &self,
        request: CreateJobRequest,
    ) -> Result<CreateJobOutcome>;
    async fn record_execution_started(&self, job_id: &str, execution_arn: &str) -> Result<JobRecord>;
    async fn update_stage(&self, job_id: &str, status: JobStatus, stage: &str) -> Result<JobRecord>;
    async fn mark_complete(
        &self,
        job_id: &str,
        snapshot_id: i64,
        row_counts: serde_json::Value,
    ) -> Result<JobRecord>;
    async fn mark_failed(&self, job_id: &str, code: &str, detail: &str) -> Result<JobRecord>;
    async fn lookup_job(&self, job_id: &str) -> Result<Option<JobRecord>>;
    async fn release_dedupe_if_owner(&self, record: &JobRecord) -> Result<()>;
}
```

If `async_trait` is not already available, add `async-trait = "0.1"` to the standalone crate dependencies.

- [ ] **Step 3: Implement DynamoDB-backed store**

Add `DynamoDbJobStore` in `jobs.rs`.

Implementation rules:

- Table name comes from `SPUR_INDEX_JOBS_TABLE`, defaulting to `spur-context-index-jobs`.
- Job item PK is `JOB#<job_id>`.
- Dedupe item PK is `DEDUP#<source>#<package>#<revision>#<source_url_hash>`.
- `create_or_get_active_job` uses `TransactWriteItems` with `attribute_not_exists(pk)` on both items.
- On transaction conflict, read the dedupe item, then read the pointed job.
- `record_execution_started` updates the job with `execution_arn` and `updated_at`.
- Terminal failure deletes the dedupe item if it still points to the same `job_id`.
- Terminal success deletes the dedupe item after `mark_complete`.

- [ ] **Step 4: Run job-store tests**

Run:

```bash
scripts/spur-cargo --workdir crates/spur-context-service test --test jobs_test --features lambda,worker
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-context-service/Cargo.toml crates/spur-context-service/src/jobs.rs crates/spur-context-service/tests/jobs_test.rs
git commit -m "feat(spur-context-service): PCS1 add DynamoDB job store"
```

---

### Task 2: Lambda and MCP Status Integration

**Depends on:** Task 1

**Files:**
- Modify: `crates/spur-context-service/src/mcp.rs`
- Modify: `crates/spur-context-service/src/lambda.rs`
- Modify: `crates/spur-context-service/tests/mcp_test.rs`

- [ ] **Step 1: Write failing MCP tests**

Add or update tests covering the new production behavior with fake `JobStore` and fake Step Functions checker/starter.

Required test names:

```rust
#[tokio::test]
async fn external_index_creates_job_starts_execution_and_records_arn() { /* fake store */ }

#[tokio::test]
async fn external_index_returns_existing_deduped_job_without_starting_execution() { /* fake store */ }

#[tokio::test]
async fn external_index_status_repairs_stale_succeeded_execution() { /* fake checker */ }

#[tokio::test]
async fn external_index_status_returns_dynamodb_state_when_describe_execution_fails() { /* fake checker */ }
```

Run:

```bash
scripts/spur-cargo --workdir crates/spur-context-service test --test mcp_test --features lambda,worker
```

Expected: FAIL because route functions still use DuckDB job rows.

- [ ] **Step 2: Change route signatures to accept job store**

Update route functions so tests and Lambda can pass a store:

```rust
pub async fn route_index(
    args: &Value,
    db: &Connection,
    catalog: &CatalogResolver,
    jobs: &dyn JobStore,
    sfn_client: &impl IndexExecutionStarter,
    caller_id: &str,
) -> Result<Value, McpHandlerError>
```

For status:

```rust
pub async fn route_index_status(
    args: &Value,
    jobs: &dyn JobStore,
    checker: Option<&dyn ExecutionStatusChecker>,
) -> Result<Value, McpHandlerError>
```

Keep a small synchronous wrapper only if existing non-Lambda tests need it; do not keep production status on DuckDB rows.

- [ ] **Step 3: Wire `route_index` to DynamoDB job lifecycle**

Flow:

1. parse and validate args
2. run existing abuse validation and DNS check
3. run existing rate limiter
4. warm-check `package_catalog`
5. call `jobs.create_or_get_active_job`
6. if outcome is `Existing`, return `active_job_response`
7. if outcome is `Created`, call `StartExecution`
8. call `jobs.record_execution_started`
9. return queued response with `job_id`, `status`, `execution_arn`, and `revision`

- [ ] **Step 4: Wire `route_index_status` to DynamoDB and Step Functions repair**

Implement 60-second stale repair using the stored `updated_at` timestamp.

Repair rules:

- `RUNNING`: return the DynamoDB record
- `SUCCEEDED`: call `jobs.mark_complete`
- `FAILED`, `TIMED_OUT`, `ABORTED`: call `jobs.mark_failed`
- transient checker failure: return DynamoDB record

- [ ] **Step 5: Update Lambda client initialization**

In `lambda.rs`, initialize DynamoDB and Step Functions clients once per warm environment and pass them into `mcp::route_index` and `mcp::route_index_status`.

Production status must pass a real `SfnExecutionStatusChecker`; it must not call the no-checker path.

- [ ] **Step 6: Run MCP tests**

Run:

```bash
scripts/spur-cargo --workdir crates/spur-context-service test --test mcp_test --features lambda,worker
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-context-service/src/mcp.rs crates/spur-context-service/src/lambda.rs crates/spur-context-service/tests/mcp_test.rs
git commit -m "feat(spur-context-service): PCS1 use DynamoDB for index status"
```

---

### Task 3: Worker Progress, Catalog Lease, and Conditional Upload

**Depends on:** Task 1

**Files:**
- Modify: `crates/spur-context-service/src/worker.rs`
- Modify: `crates/spur-context-service/tests/worker_test.rs`

- [ ] **Step 1: Write failing worker lease tests**

Add fake lease/store tests.

Required test names:

```rust
#[tokio::test]
async fn worker_updates_job_stages_around_fetch_build_and_translate() { /* fake store */ }

#[tokio::test]
async fn catalog_lease_blocks_upload_when_token_is_lost() { /* fake lease */ }

#[tokio::test]
async fn catalog_download_upload_uses_conditional_s3_write_metadata() { /* fake S3 */ }
```

Run:

```bash
scripts/spur-cargo --workdir crates/spur-context-service test --test worker_test --features worker
```

Expected: FAIL because lease abstractions do not exist.

- [ ] **Step 2: Add catalog lease trait and DynamoDB implementation**

Target shape:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogLease {
    pub catalog_uri: String,
    pub owner_job_id: String,
    pub lease_token: String,
    pub expires_at_unix_secs: i64,
    pub fencing_counter: i64,
}

#[async_trait::async_trait]
pub trait CatalogLeaseStore: Send + Sync {
    async fn acquire(&self, catalog_uri: &str, owner_job_id: &str) -> Result<CatalogLease>;
    async fn renew(&self, lease: &CatalogLease) -> Result<CatalogLease>;
    async fn assert_owned(&self, lease: &CatalogLease) -> Result<()>;
    async fn release(&self, lease: &CatalogLease) -> Result<()>;
}
```

Rules:

- Table name comes from `SPUR_CATALOG_LEASES_TABLE`, default `spur-context-catalog-leases`.
- Key is `CATALOG#<sha256(catalog_uri)>`.
- Acquire allows absent lease, expired lease, or same-token renewal.
- Lease duration is 10 minutes.
- Worker renews every 60 seconds while translate runs.

- [ ] **Step 3: Move worker progress updates to `JobStore`**

Replace `update_job_status` and `update_job_status_with_connection` usage with `JobStore` calls.

Keep worker failure behavior:

- best-effort mark failed
- send task failure
- do not panic if DynamoDB update fails; log and continue to Step Functions failure reporting

- [ ] **Step 4: Wrap catalog mutation/upload with lease**

In `run_job_with_stage`:

1. fetch/build before acquiring lease
2. update stage `waiting_catalog_lease`
3. acquire lease
4. update stage `translate`
5. download catalog
6. translate with current Rust API path
7. assert/renew lease before upload
8. conditionally upload catalog
9. release lease after success or failure if owned

Preserve the current unstaged Rust API translate change. Do not reintroduce DuckDB CLI unless tests prove the Rust API path cannot flush S3 data.

- [ ] **Step 5: Require conditional S3 catalog upload**

Extend `CatalogDownload` to store ETag/version metadata from `GetObject`.

`upload` must fail rather than doing unconditional `PutObject` if it cannot perform a conditional write. If the AWS SDK does not expose the condition needed, implement a focused signed S3 request helper in `worker.rs` or a small worker-local module.

- [ ] **Step 6: Run worker tests**

Run:

```bash
scripts/spur-cargo --workdir crates/spur-context-service test --test worker_test --features worker
```

Expected: PASS, with AWS/git ignored tests still ignored.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-context-service/src/worker.rs crates/spur-context-service/tests/worker_test.rs
git commit -m "feat(spur-context-service): PCS1 serialize catalog writes"
```

---

### Task 4: Terraform and Canonical Worker Image

**Depends on:** Task 1

**Files:**
- Modify: `infra/spur-context-service/main.tf`
- Modify: `infra/spur-context-service/iam.tf`
- Modify: `infra/spur-context-service/ecs.tf`
- Modify: `infra/spur-context-service/state_machine.tf`
- Modify: `infra/spur-context-service/variables.tf`
- Modify: `infra/spur-context-service/outputs.tf`
- Modify: `infra/spur-context-service/deploy.sh`
- Modify: `infra/spur-context-service/build-and-push-remote.sh`
- Modify: `tests/scripts/test_spur_context_service_deploy.py`

- [ ] **Step 1: Write failing deploy-script tests**

Extend `tests/scripts/test_spur_context_service_deploy.py` with checks that the canonical deploy image includes both binaries.

Required assertions:

```python
assert "COPY spur-context-worker /usr/local/bin/spur-context-worker" in deploy_sh
assert "COPY spur /usr/local/bin/spur" in deploy_sh
assert "spur --version" in deploy_sh
assert "spur-context-worker" in deploy_sh
```

Run:

```bash
python -m pytest tests/scripts/test_spur_context_service_deploy.py
```

Expected: FAIL because `deploy.sh` currently copies only `spur-context-worker`.

- [ ] **Step 2: Add DynamoDB and S3 versioning Terraform**

Add:

- `aws_dynamodb_table.index_jobs`
- `aws_dynamodb_table.catalog_leases`
- `aws_s3_bucket_versioning.data`

Use on-demand billing, point-in-time recovery, server-side encryption, and TTL attributes.

- [ ] **Step 3: Add IAM permissions**

Lambda role:

- `dynamodb:GetItem`
- `dynamodb:PutItem`
- `dynamodb:UpdateItem`
- `dynamodb:DeleteItem`
- `dynamodb:TransactWriteItems`

ECS task role:

- same DynamoDB permissions for jobs and leases
- S3 permissions needed for conditional catalog read/write

- [ ] **Step 4: Pass explicit worker env vars**

Step Functions container overrides must pass:

- `SPUR_INDEX_JOBS_TABLE`
- `SPUR_CATALOG_LEASES_TABLE`
- `SPUR_CATALOG_S3_URI`
- `SPUR_CONTEXT_DUCKLAKE_DATA_PATH`
- `SPUR_CONTEXT_WORKER_CHECKPOINT_URI`

Remove production reliance on hard-coded `s3://spur-context/...` worker defaults.

- [ ] **Step 5: Canonicalize worker image build**

Update `deploy.sh` so the Docker context contains both:

- remote `spur-context-worker`
- remote `spur`

The inline Dockerfile must copy both binaries.

Add smoke commands after image build and before push/apply:

```bash
docker run --rm "$full_tag" /usr/local/bin/spur --version
docker run --rm "$full_tag" /usr/local/bin/spur-context-worker || true
```

If the remote Docker helper cannot run local `docker run`, add equivalent `scripts/cloud-build/docker-build.sh` support or a remote smoke command.

- [ ] **Step 6: Update or retire `build-and-push-remote.sh`**

Make `build-and-push-remote.sh` call the same image assembly path used by `deploy.sh`, or delete it if it is no longer referenced.

Do not leave two scripts that disagree about image contents.

- [ ] **Step 7: Run infra checks**

Run:

```bash
python -m pytest tests/scripts/test_spur_context_service_deploy.py
terraform -chdir=infra/spur-context-service validate
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add infra/spur-context-service tests/scripts/test_spur_context_service_deploy.py
git commit -m "feat(spur-context-service): PCS1 add production control-plane infra"
```

---

### Task 5: Remove DuckDB Job Ledger and Run Final Verification

**Depends on:** Task 2, Task 3, Task 4

**Files:**
- Modify: `crates/spur-context-service/src/catalog.rs`
- Modify: `crates/spur-context-service/sql/index_jobs.sql`
- Modify: `crates/spur-context-service/sql/init_catalog.sql`
- Modify: `crates/spur-context-service/tests/catalog_test.rs`
- Modify: `crates/spur-context-service/tests/jobs_test.rs`
- Modify: `infra/spur-context-service/README.md`
- Modify: `docs/superpowers/specs/2026-06-24-context-service-on-demand-indexing-design.md`

- [ ] **Step 1: Write failing cleanup regression tests**

Add tests that prove catalog initialization no longer creates or depends on `memory.index_jobs`.

Required test name:

```rust
#[test]
fn catalog_resolver_does_not_create_memory_index_jobs() { /* open catalog, inspect memory tables */ }
```

Run:

```bash
scripts/spur-cargo --workdir crates/spur-context-service test --test catalog_test --features lambda,worker
```

Expected: FAIL until catalog cleanup is implemented.

- [ ] **Step 2: Remove runtime `memory.index_jobs` creation**

In `catalog.rs`:

- remove `ensure_index_jobs_table`
- remove `INDEX_JOBS_SQL` include if unused
- remove fallback DDL helpers that only exist for `index_jobs`
- keep DuckLake attach/query behavior intact

- [ ] **Step 3: Remove obsolete SQL job DDL**

Delete or deprecate `sql/index_jobs.sql`. Remove `index_jobs` creation from `sql/init_catalog.sql`.

If local tests still need a historical SQL fixture, move it into the specific test file rather than runtime catalog setup.

- [ ] **Step 4: Update docs**

Update docs to state:

- job/status/dedupe/lease state lives in DynamoDB
- DuckLake/S3 only stores indexed package data
- worker image includes `spur` and `spur-context-worker`
- status recovery uses Step Functions `DescribeExecution`

- [ ] **Step 5: Run final verification**

Run:

```bash
scripts/spur-cargo --workdir crates/spur-context-service test --features lambda,worker
terraform -chdir=infra/spur-context-service validate
python -m pytest tests/scripts/test_spur_context_service_deploy.py
```

Expected: PASS. Ignored AWS/git tests may remain ignored unless staging credentials are configured.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-context-service infra/spur-context-service docs/superpowers/specs/2026-06-24-context-service-on-demand-indexing-design.md
git commit -m "refactor(spur-context-service): PCS1 remove DuckDB job ledger"
```

---

## Submitted Plan DAG

- `PCS1.a-dynamodb-job-store`: no dependencies
- `PCS1.b-lambda-mcp-status`: depends on `PCS1.a-dynamodb-job-store`
- `PCS1.c-worker-catalog-lease`: depends on `PCS1.a-dynamodb-job-store`
- `PCS1.d-infra-worker-image`: depends on `PCS1.a-dynamodb-job-store`
- `PCS1.e-cleanup-verification`: depends on `PCS1.b-lambda-mcp-status`, `PCS1.c-worker-catalog-lease`, and `PCS1.d-infra-worker-image`

## Plan Self-Review

- Spec coverage: durable jobs are covered by Task 1 and Task 2; status recovery by Task 2; worker image by Task 4; catalog write serialization by Task 3 and Task 4; cleanup by Task 5.
- Placeholder scan: no placeholder sections are intentionally left for workers; each task has required tests, implementation targets, commands, and commits.
- Type consistency: `JobStore`, `JobRecord`, `CreateJobOutcome`, and `CatalogLeaseStore` names are used consistently across tasks.
