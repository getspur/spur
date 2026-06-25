# spur-context-service Terraform

Infrastructure for the DuckLake-served code context MCP service on AWS Lambda,
plus the on-demand indexing worker control plane.

## Architecture

```
Client → API Gateway (HTTP) → Lambda (ARM64, provided.al2023)
                                ├── DuckDB + DuckLake reads
                                │       ↓
                                │   S3 (.ducklake catalog + Parquet data via httpfs)
                                ├── DynamoDB job/status/dedupe records
                                └── Step Functions DescribeExecution repair

external_index → DynamoDB job + dedupe → Step Functions → ECS worker
                                                     ↓
                                      DuckLake/S3 package data writes
                                      under DynamoDB catalog lease
```

DuckLake/S3 is the data plane for indexed package rows, refs, catalog metadata,
and Parquet files. DynamoDB is the production control plane for job status,
idempotency, execution ARNs, and catalog write leases.

The deployed worker image includes both `/usr/local/bin/spur-context-worker`
and `/usr/local/bin/spur`; `deploy.sh` smoke-tests both before Terraform applies
the task definition. `external_index_status` repairs stale queued/running jobs
through Step Functions `DescribeExecution` when an execution ARN is present.

## Resources

| Resource | Purpose |
|---|---|
| `aws_s3_bucket.data` | DuckLake catalog (.ducklake) + Parquet data files |
| `aws_dynamodb_table.index_jobs` | Job records, active dedupe pointers, execution ARNs |
| `aws_dynamodb_table.catalog_leases` | Serialized DuckLake catalog write leases |
| `aws_lambda_function.service` | ARM64 Lambda, 1024MB, 30s timeout |
| `aws_sfn_state_machine.index_build` | On-demand indexing orchestration |
| `aws_ecs_cluster.indexing` / task definition | Fargate worker runtime |
| `aws_apigatewayv2_api.http` | HTTP API front door |
| `aws_iam_role.lambda` | Execution role with S3 read, DynamoDB, SFN + CloudWatch Logs |
| `aws_iam_role.worker_task` | Worker role with S3, DynamoDB, SFN callback permissions |
| `aws_cloudwatch_log_group.lambda` | 14-day retention |
| `aws_cloudwatch_log_group.worker` | Worker task logs |

## Deploy

```bash
# Full build + package + deploy
./deploy.sh

# Or use an existing zip
./deploy.sh --local-zip /path/to/lambda.zip
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
| `catalog_s3_uri` | `s3://spur-context/catalog/catalog.ducklake` | DuckLake catalog path |
| `context_ducklake_data_path` | `s3://<bucket_name>/data/` | DuckLake data path passed to worker jobs |
| `index_jobs_table_name` | `spur-context-index-jobs` | DynamoDB table for job records and dedupe |
| `catalog_leases_table_name` | `spur-context-catalog-leases` | DynamoDB table for catalog leases |
| `lambda_memory_mb` | `1024` | Lambda memory |
| `lambda_timeout_sec` | `30` | Lambda timeout |
| `concurrent_warm_instances` | `0` | Provisioned concurrency |
| `vpc_id` | n/a | VPC for ECS worker tasks |
| `worker_subnets` | `[]` | Subnets for ECS worker tasks |
| `worker_ecr_image` | n/a | Worker image URI built by `deploy.sh` |

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
