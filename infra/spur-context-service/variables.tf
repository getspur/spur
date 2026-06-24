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
