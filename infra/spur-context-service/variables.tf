variable "aws_region" {
  description = "AWS region for all resources"
  type        = string
  default     = "ap-southeast-5"
}

variable "bucket_name" {
  description = "S3 bucket for DuckLake catalog and Parquet data"
  type        = string
  default     = "spur-context"
}

variable "lambda_zip_path" {
  description = "Local path to the Lambda deployment zip"
  type        = string
  default     = "../../target/lambda/spur-context-service.zip"
}

variable "catalog_s3_uri" {
  description = "S3 URI of the DuckLake catalog file"
  type        = string
  default     = "s3://spur-context/catalog/catalog.ducklake"
}

variable "context_ducklake_data_path" {
  description = "DuckLake data path used by worker translate jobs. Defaults to s3://<bucket_name>/data/."
  type        = string
  default     = null
}

variable "aurora_cluster_identifier" {
  description = "Aurora Serverless v2 cluster identifier for the live DuckLake ingest catalog"
  type        = string
  default     = "spur-context-catalog"
}

variable "aurora_database_name" {
  description = "Postgres database name for the live DuckLake ingest catalog"
  type        = string
  default     = "spur_context"
}

variable "aurora_master_username" {
  description = "Aurora master username. The password is generated and stored by RDS in Secrets Manager."
  type        = string
  default     = "spur_context"
}

variable "aurora_engine_version" {
  description = "Aurora PostgreSQL engine version. Null lets RDS choose the regional default."
  type        = string
  default     = null
}

variable "aurora_subnets" {
  description = "Private subnet IDs for Aurora. Defaults to worker_subnets when null."
  type        = list(string)
  default     = null
}

variable "aurora_max_acu" {
  description = "Maximum Aurora Serverless v2 capacity in ACUs"
  type        = number
  default     = 4
}

variable "aurora_seconds_until_auto_pause" {
  description = "Seconds of inactivity before Aurora Serverless v2 auto-pauses at 0 ACU"
  type        = number
  default     = 300
}

variable "aurora_backup_retention_days" {
  description = "Aurora backup retention period in days"
  type        = number
  default     = 7
}

variable "aurora_deletion_protection" {
  description = "Enable deletion protection on the Aurora catalog cluster"
  type        = bool
  default     = true
}

variable "index_jobs_table_name" {
  description = "DynamoDB table name for context-service index job records and dedupe pointers"
  type        = string
  default     = "spur-context-index-jobs"
}

variable "catalog_leases_table_name" {
  description = "DynamoDB table name for context-service catalog write leases"
  type        = string
  default     = "spur-context-catalog-leases"
}

variable "lambda_memory_mb" {
  description = "Lambda memory allocation"
  type        = number
  default     = 1024
}

variable "lambda_timeout_sec" {
  description = "Lambda timeout in seconds"
  type        = number
  default     = 30
}

variable "concurrent_warm_instances" {
  description = "Provisioned concurrency (0 = disabled, eliminates cold start when > 0)"
  type        = number
  default     = 0
}

# ─── Public API Abuse Controls ────────────────────────────────────────────────

variable "api_throttle_rate_limit" {
  description = "API Gateway account-level route throttle rate in requests per second"
  type        = number
  default     = 20
}

variable "api_throttle_burst_limit" {
  description = "API Gateway account-level route throttle burst"
  type        = number
  default     = 40
}

variable "index_rate_limit_per_minute" {
  description = "Per authenticated caller external_index fixed-window rate limit"
  type        = number
  default     = 10
}

variable "index_max_concurrent_jobs_per_caller" {
  description = "Maximum queued/running external_index jobs per authenticated caller"
  type        = number
  default     = 2
}

variable "context_max_tarball_bytes" {
  description = "Maximum downloaded tarball bytes for external_index"
  type        = number
  default     = 524288000
}

variable "context_max_git_bytes" {
  description = "Maximum fetched git source tree bytes for external_index"
  type        = number
  default     = 2147483648
}

variable "context_max_build_seconds" {
  description = "Maximum spur graph build runtime for an indexing worker"
  type        = number
  default     = 1800
}

variable "allowed_source_domains" {
  description = "Optional source_url domain allow-list for external_index; empty allows public non-private domains"
  type        = list(string)
  default     = []
}

# ─── On-Demand Indexing ──────────────────────────────────────────────────────

variable "vpc_id" {
  description = "VPC ID for the ECS worker tasks (needs S3/RDS/SFN egress)"
  type        = string
}

variable "worker_subnets" {
  description = "Subnets for ECS worker tasks (need NAT gateway or VPC endpoints for S3/SFN)"
  type        = list(string)
}

variable "worker_ecr_image" {
  description = "ECR image URI for the spur-context-worker container (e.g. <acct>.dkr.ecr.<region>.amazonaws.com/spur-context-worker:latest)"
  type        = string
}

variable "worker_lambda_image" {
  description = "ECR image URI for the Lambda-compatible spur-context-worker image"
  type        = string
}

variable "worker_lambda_memory_mb" {
  description = "Lambda worker memory allocation. This account/region currently accepts up to 3008 MB; raise after a Lambda memory quota increase."
  type        = number
  default     = 3008
}

variable "worker_lambda_timeout_sec" {
  description = "Lambda worker timeout in seconds. Lambda max is 900 seconds."
  type        = number
  default     = 900
}

variable "worker_lambda_ephemeral_storage_mb" {
  description = "Lambda worker /tmp storage in MB. Lambda max is 10240 MB."
  type        = number
  default     = 10240
}

variable "worker_lambda_provisioned_concurrency" {
  description = "Provisioned concurrency for the Lambda worker live alias (0 = disabled)"
  type        = number
  default     = 0
}

locals {
  context_ducklake_data_path      = coalesce(var.context_ducklake_data_path, "s3://${var.bucket_name}/data/")
  worker_checkpoint_uri_template  = "s3://${var.bucket_name}/jobs/{}/checkpoint.json"
  aurora_subnet_ids               = var.aurora_subnets != null ? var.aurora_subnets : var.worker_subnets
  aurora_catalog_dsn              = "postgres:host=${aws_rds_cluster.catalog.endpoint} port=${aws_rds_cluster.catalog.port} dbname=${var.aurora_database_name} user=${var.aurora_master_username} sslmode=require"
  aurora_master_secret_arn        = aws_rds_cluster.catalog.master_user_secret[0].secret_arn
  aurora_master_password_valuearn = "${local.aurora_master_secret_arn}:password::"
}
