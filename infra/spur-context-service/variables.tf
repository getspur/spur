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
  context_ducklake_data_path     = coalesce(var.context_ducklake_data_path, "s3://${var.bucket_name}/data/")
  worker_checkpoint_uri_template = "s3://${var.bucket_name}/jobs/{}/checkpoint.json"
}
