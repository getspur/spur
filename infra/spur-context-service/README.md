# spur-context-service Terraform

Infrastructure for the DuckLake-served code context MCP service on AWS Lambda.

## Architecture

```
Client → API Gateway (HTTP) → Lambda (ARM64, provided.al2023)
                                ↓
                            DuckDB + DuckLake
                                ↓
                            S3 (.ducklake catalog + Parquet data via httpfs)
```

No VPC. No PostgreSQL. No download. DuckDB reads both catalog metadata and
Parquet data directly from S3 via httpfs range requests.

## Resources

| Resource | Purpose |
|---|---|
| `aws_s3_bucket.data` | DuckLake catalog (.ducklake) + Parquet data files |
| `aws_lambda_function.service` | ARM64 Lambda, 1024MB, 30s timeout |
| `aws_apigatewayv2_api.http` | HTTP API front door |
| `aws_iam_role.lambda` | Execution role with S3 read + CloudWatch Logs |
| `aws_cloudwatch_log_group.lambda` | 14-day retention |

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
| `lambda_memory_mb` | `1024` | Lambda memory |
| `lambda_timeout_sec` | `30` | Lambda timeout |
| `concurrent_warm_instances` | `0` | Provisioned concurrency |

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
