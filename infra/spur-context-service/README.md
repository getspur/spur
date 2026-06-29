# spur-context-service Terraform

Infrastructure for the DuckLake-served code context MCP service on AWS Lambda,
plus the on-demand indexing worker control plane.

## Architecture

```
Client → API Gateway (HTTP, AWS_IAM) → Lambda (ARM64, provided.al2023)
                                ├── DuckDB + DuckLake reads
                                │       ↓
                                │   S3 (.ducklake catalog + Parquet data via httpfs)
                                ├── DynamoDB job/status/dedupe/quota records
                                └── Step Functions DescribeExecution repair

external_index → DynamoDB job + dedupe → Step Functions → Lambda worker
                                                   │
                                                   └── ECS worker fallback
                                                       for Lambda platform
                                                       failures/timeouts
                                                   ↓
                                    DuckLake/S3 package data writes
                                    under DynamoDB catalog lease
```

DuckLake/S3 is the data plane for indexed package rows, refs, catalog metadata,
and Parquet files. DynamoDB is the production control plane for job status,
idempotency, execution ARNs, per-caller active-job quotas, and catalog write
leases.

The indexing worker Lambda and ECS fallback run inside `worker_subnets`. The
default network model is NAT-free for AWS service access: Terraform creates S3
and DynamoDB gateway endpoints on `worker_route_table_ids`, plus private-DNS
interface endpoints in `worker_subnets` for Step Functions, Secrets Manager,
ECR API, ECR Docker registry, CloudWatch Logs, and STS. The interface endpoint
security group accepts HTTPS only from the worker security group. Operators who
already provide NAT or equivalent shared endpoints can set
`create_vpc_endpoints=false`.

VPC endpoints do not provide arbitrary public internet egress. Source URLs that
must be fetched from the public internet still need a separate egress path, or
should be staged in S3 so the worker reaches them through the S3 endpoint.

The deployed ECS fallback image includes both `/usr/local/bin/spur-context-worker`
and `/usr/local/bin/spur`; `deploy.sh` smoke-tests both before Terraform applies
the task definition. The Lambda fast-path image includes
`/usr/local/bin/spur-context-worker-lambda` plus `/usr/local/bin/spur` and is
invoked first by Step Functions. `external_index_status` repairs stale
queued/running jobs through Step Functions `DescribeExecution` when an execution
ARN is present.

## Resources

| Resource | Purpose |
|---|---|
| `aws_s3_bucket.data` | DuckLake catalog (.ducklake) + Parquet data files |
| `aws_dynamodb_table.index_jobs` | Job records, active dedupe pointers, caller quota records, execution ARNs |
| `aws_dynamodb_table.catalog_leases` | Serialized DuckLake catalog write leases |
| `aws_lambda_function.service` | ARM64 Lambda, 1024MB, 30s timeout |
| `aws_sfn_state_machine.index_build` | Lambda-first on-demand indexing orchestration |
| `aws_lambda_function.worker` | Fast-start indexing worker image |
| `aws_ecs_cluster.indexing` / task definition | Fargate fallback worker runtime |
| `aws_apigatewayv2_api.http` | HTTP API front door |
| `aws_iam_policy.context_service_invoke` | Same-account SigV4 invoke policy for allowed callers |
| `aws_iam_role.lambda` | Execution role with S3 read, DynamoDB, SFN + CloudWatch Logs |
| `aws_iam_role.worker_task` | ECS fallback worker role with S3, DynamoDB, SFN callback permissions |
| `aws_cloudwatch_log_group.lambda` | 14-day retention |
| `aws_cloudwatch_log_group.worker_lambda` | Lambda worker logs |
| `aws_cloudwatch_log_group.worker` | Worker task logs |
| `aws_vpc_endpoint.gateway` | S3 and DynamoDB gateway endpoints for worker route tables |
| `aws_vpc_endpoint.interface` | Private-DNS endpoints for worker AWS API access |
| `aws_security_group.vpc_endpoints` | HTTPS ingress from worker tasks to interface endpoints |

## Authentication And Abuse Controls

The public API route uses API Gateway `AWS_IAM`; callers must sign requests with
SigV4 and have `execute-api:Invoke` permission. The Lambda still rejects
mutating tools (`external_index`, `external_index_status`) unless API Gateway
passes an authenticated JWT/IAM/principal identity, so an accidentally
unauthenticated route does not silently fall back to source IP identity.

`external_index` applies three layers of controls:

- API Gateway route throttling via `api_throttle_rate_limit` and
  `api_throttle_burst_limit`.
- Per authenticated caller DynamoDB-backed fixed-window rate limiting and an
  active-job cap. The active-job cap is atomic with job creation and is released
  when a job reaches a terminal state.
- Source and build caps. URL size hints are checked before job creation; workers
  revalidate URLs, cap tarball downloads/source trees, and kill `spur graph
  build` after `context_max_build_seconds`.

## Tenant Isolation Notes

This deployment intentionally indexes public external packages into one shared
catalog and S3 bucket. Tenants do not receive direct S3, Aurora, DynamoDB, or
Step Functions access; their isolation boundary is the signed API caller, the
per-caller quota records, and the package coordinate stored on each job and
catalog row.

The shared catalog/bucket model is acceptable only for public package indexing.
Do not use it for private tenant source or tenant-confidential artifacts without
adding tenant-scoped catalog filtering, per-tenant S3 prefixes and IAM policies,
and API response authorization checks. Current workers serialize catalog writes
with DynamoDB leases, but they do not provide private data-plane isolation inside
the shared DuckLake catalog.

## Deploy

```bash
# Full build + package + deploy
./deploy.sh

# Or use an existing zip
./deploy.sh --local-zip /path/to/lambda.zip

# Build the serving Lambda zip without touching Terraform-managed resources
./deploy.sh --skip-worker --package-only

# Build arm64 worker image tarballs locally through docker buildx, without ECR
SPUR_CONTEXT_SERVICE_BUILD_MODE=self-contained \
SPUR_CONTEXT_SERVICE_PUSH_IMAGES=0 \
./build-and-push-remote.sh --no-push
```

## CI/CD

The context-service GitHub Actions workflow lives at
`.github/workflows/context-service.yml`.

Pull requests and pushes that touch this service run:

- `scripts/spur-cargo --workdir crates/spur-context-service test --all-features`
- deploy-script guardrails in `tests/scripts/test_spur_context_service_deploy.py`
- `infra/spur-context-service/test-graviton2-baseline.sh`

Real AWS artifact builds are gated through `workflow_dispatch` and the
`context-service-staging` environment. Set `build_aws_artifacts=true` to build
the Graviton2-safe worker image tarballs and serving Lambda zip. CI defaults to
`SPUR_CONTEXT_SERVICE_BUILD_MODE=self-contained` and
`SPUR_CONTEXT_SERVICE_PUSH_IMAGES=0`, so it uses docker buildx with
`--platform linux/arm64 --provenance=false` and does not depend on the remote
builder VM, create ECR repositories, push images, or run Terraform.

Operator-run pushes remain available by leaving the default
`SPUR_CONTEXT_SERVICE_BUILD_MODE=remote` in `build-and-push-remote.sh`.
Set `SPUR_CLOUD` when the remote builder cloud should differ from the
`scripts/spur-cargo` default:

```bash
SPUR_CONTEXT_SERVICE_BUILD_MODE=remote ./build-and-push-remote.sh
./deploy.sh --skip-worker --package-only
```

Required repository/environment configuration:

| Name | Kind | Purpose |
|---|---|---|
| `CONTEXT_SERVICE_AWS_ROLE_ARN` | secret | AWS role assumed by GitHub OIDC for ECR, Lambda, S3, and smoke access |
| `CONTEXT_SERVICE_AWS_REGION` | variable | AWS region, defaults to `ap-southeast-5` |
| `CONTEXT_SERVICE_STAGING_LAMBDA` | variable | Staging serving Lambda name for the smoke test |
| `CONTEXT_SERVICE_STAGING_SOURCE_BUCKET` | variable | Bucket where the smoke uploads its tiny source tarball |
| `CONTEXT_SERVICE_STAGING_DATA_BUCKET` | variable | Bucket containing bronze, silver, and gold medallion objects |

## Staging E2E Smoke

Run the staging smoke locally after authenticating to AWS:

```bash
export AWS_REGION=ap-southeast-5
export SPUR_CONTEXT_SMOKE_LAMBDA=spur-context-service
export SPUR_CONTEXT_SMOKE_SOURCE_BUCKET=spur-context-staging
export SPUR_CONTEXT_SMOKE_DATA_BUCKET=spur-context-staging

# No-ingest preflight: validates required env, AWS STS auth, and Lambda
# serving catalog config without uploading a fixture or calling external_index.
infra/spur-context-service/smoke-staging-e2e.sh --preflight

# Full E2E smoke: uploads the fixture and invokes real ingest.
infra/spur-context-service/smoke-staging-e2e.sh
```

The full smoke publishes a tiny Rust package tarball, calls `external_index`,
waits for the real worker to complete, checks the bronze source object at
`bronze/{source}/{package}/{revision}/source.tar.gz`, the silver artifact
manifest at `silver/{source}/{package}/{revision}/{builder_version}/manifest.json`,
the gold frozen snapshot pointer at `gold/catalog-snapshot/current.json`,
non-zero `symbol_embeddings`, and then serves `external_code_search`,
`external_code_read`, and `external_knowledge_context` from the staging Lambda.
The `source` path component is the worker's verbatim source coordinate, such as
`registry:crates-io`.

Serving is intentionally zero-Postgres: the script fails if the serving Lambda
is configured with a Postgres `SPUR_CATALOG_DSN`, or if `SPUR_CATALOG_S3_URI`
does not point at the frozen snapshot pointer
`s3://<bucket>/gold/catalog-snapshot/current.json`. Set
`SPUR_CONTEXT_SMOKE_ALLOW_NON_POINTER_SNAPSHOT=1` only for a temporary staging
debug run against a direct S3 `.ducklake` snapshot.

## Graviton2-safe CPU baseline

Deployable arm64 artifacts for this service must target Graviton2-class hosts:
the serving Lambda bootstrap, the Lambda worker image binary, the Fargate worker
image binary, and the `spur` binary copied into worker images. Build them through
`deploy.sh`, which sources `graviton2-baseline.sh` and exports guarded
`RUSTFLAGS`, `CFLAGS`, and `CXXFLAGS` for those artifact builds. The
self-contained docker buildx path reuses the same helper constants and exports
them into the arm64 build container before compiling.

The allowed Rust `target-cpu` values for deployable artifacts are
`neoverse-n1` or `generic`; C/C++ flags must use `-mcpu=neoverse-n1` or a
generic `-march=armv8-a...` baseline. Do not use `neoverse-v2` for Lambda or
Fargate artifacts: Lambda arm64 and Fargate arm64 run on Graviton2-compatible
hardware, while the remote build VM may support newer instructions. Normal
local/dev `scripts/spur-cargo` builds may continue to use the VM default; this
rule only applies to artifacts deployed by `infra/spur-context-service`.

Run the guard regression check after changing these scripts:

```bash
bash infra/spur-context-service/test-graviton2-baseline.sh
```

## Provisioned Concurrency

To eliminate cold start entirely, set `concurrent_warm_instances`:

```bash
terraform apply -var concurrent_warm_instances=1
```

## Variables

| Name | Default | Description |
|---|---|---|
| `aws_region` | `ap-southeast-5` | AWS region |
| `bucket_name` | `spur-context` | S3 bucket for data |
| `catalog_s3_uri` | `s3://spur-context/gold/catalog-snapshot/current.json` | Frozen serving DuckLake snapshot pointer or snapshot file |
| `context_ducklake_data_path` | `s3://<bucket_name>/data/` | DuckLake data path passed to worker jobs |
| `index_jobs_table_name` | `spur-context-index-jobs` | DynamoDB table for job records and dedupe |
| `catalog_leases_table_name` | `spur-context-catalog-leases` | DynamoDB table for catalog leases |
| `lambda_memory_mb` | `1024` | Lambda memory |
| `lambda_timeout_sec` | `30` | Lambda timeout |
| `concurrent_warm_instances` | `0` | Provisioned concurrency |
| `api_throttle_rate_limit` | `20` | API Gateway route throttle rate per second |
| `api_throttle_burst_limit` | `40` | API Gateway route throttle burst |
| `index_rate_limit_per_minute` | `10` | Per-caller `external_index` requests per minute |
| `index_max_concurrent_jobs_per_caller` | `2` | Per-caller queued/running index job cap |
| `context_max_tarball_bytes` | `524288000` | Tarball download/source cap |
| `context_max_git_bytes` | `2147483648` | Git source tree cap |
| `context_max_build_seconds` | `1800` | Worker `spur graph build` timeout |
| `allowed_source_domains` | `[]` | Optional `source_url` domain allow-list |
| `vpc_id` | n/a | VPC for ECS worker tasks |
| `worker_subnets` | n/a | Private subnets for Lambda and ECS worker tasks |
| `worker_route_table_ids` | `[]` | Route tables associated with `worker_subnets`; required when `create_vpc_endpoints=true` |
| `create_vpc_endpoints` | `true` | Create NAT-free worker endpoints for S3, DynamoDB, Step Functions, Secrets Manager, ECR, CloudWatch Logs, and STS |
| `vpc_endpoint_region` | `null` | Optional endpoint service-name region override; defaults to `aws_region` |
| `worker_ecr_image` | n/a | ECS fallback worker image URI built by `deploy.sh` |
| `worker_lambda_image` | n/a | Lambda fast-path worker image URI built by `deploy.sh` |
| `worker_lambda_memory_mb` | `3008` | Lambda worker memory for this account/region cap |
| `worker_lambda_timeout_sec` | `900` | Lambda worker timeout |
| `worker_lambda_ephemeral_storage_mb` | `10240` | Lambda worker `/tmp` storage |
| `worker_lambda_provisioned_concurrency` | `0` | Warm Lambda worker instances |

## Cold Start Performance

| Configuration | INIT | First request | Warm |
|---|---|---|---|
| Default (no concurrency) | 432ms | ~1950ms | ~100ms |
| With provisioned concurrency | — | — | ~100ms (always warm) |

## Remote State (optional)

For production, migrate to S3 backend:

```bash
# Create state bucket
aws s3 mb s3://spur-tf-state --region ap-southeast-5

# Add to versions.tf:
# backend "s3" {
#   bucket = "spur-tf-state"
#   key    = "spur-context-service/terraform.tfstate"
#   region = "ap-southeast-5"
# }
```
