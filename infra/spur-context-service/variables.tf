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
  description = "S3 URI of the frozen serving DuckLake snapshot pointer or snapshot file"
  type        = string
  default     = "s3://spur-context/gold/catalog-snapshot/current.json"
}

variable "allow_anonymous_mutations" {
  description = "Allow mutating tools (external_index/external_index_status) without an authenticated caller, falling back to a shared anonymous identity. Intended for internal-team / trusted-network stacks where the API route is public (NONE). Secure-by-default off; the shared anonymous identity still shares the per-caller rate limit / active-job cap."
  type        = bool
  default     = false
}

variable "api_authorization_type" {
  description = "Authorization for the HTTP API $default route. AWS_IAM (SigV4, secure default) or NONE (public, unauthenticated — use only for demo/eval stacks). Valid: NONE, AWS_IAM, JWT, CUSTOM."
  type        = string
  default     = "AWS_IAM"

  validation {
    condition     = contains(["NONE", "AWS_IAM", "JWT", "CUSTOM"], var.api_authorization_type)
    error_message = "api_authorization_type must be one of NONE, AWS_IAM, JWT, CUSTOM."
  }
}

variable "context_ducklake_data_path" {
  description = "DuckLake data path used by worker translate jobs. Must end in /gold/data so the frozen snapshot pointer lands at s3://<bucket>/gold/catalog-snapshot/current.json. Defaults to s3://<bucket_name>/gold/data/."
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

# ─── Bounded Backlog Queueing ────────────────────────────────────────────────
# Config surface for the DynamoDB backlog/backpressure design. Defaults preserve
# the current live behavior (reject over capacity) until a downstream task
# switches admission to the bounded-queue path. See
# docs/superpowers/specs/2026-07-10-context-service-index-queue-backpressure-design.md

variable "index_queue_gsi_name" {
  description = "Name of the sparse DynamoDB GSI keyed by (queue_shard, queue_sort_key) used by the drainer to scan queued jobs in FIFO order."
  type        = string
  default     = "queue-gsi"
}

variable "index_queue_shard_count" {
  description = "Number of shards for the sparse queue GSI partition key. The drainer rotates shard order/cursor so every queued job is scanned within a bounded number of runs. Default 16 per the design spec."
  type        = number
  default     = 16

  validation {
    condition     = var.index_queue_shard_count > 0 && var.index_queue_shard_count <= 1024
    error_message = "index_queue_shard_count must be between 1 and 1024."
  }
}

variable "index_max_running_jobs_per_owner" {
  description = "Maximum concurrent running/dispatching index jobs per backlog owner (replaces the legacy per-caller active-job cap once the queue path is enabled). Default mirrors index_max_concurrent_jobs_per_caller to preserve current behavior."
  type        = number
  default     = 2
}

variable "index_max_queued_jobs_per_owner" {
  description = "Maximum accepted queued backlog per backlog owner. 0 (default) preserves the current reject-over-capacity contract until queueing is enabled."
  type        = number
  default     = 0
}

variable "index_max_running_jobs_global" {
  description = "Global concurrent running/dispatching job cap enforced via a RUNNING# token pool. 0 (default) disables the hard global running cap; small deployments rely on per-owner caps plus API Gateway throttles."
  type        = number
  default     = 0
}

variable "index_max_queued_jobs_global" {
  description = "Global accepted queued backlog, enforced via sharded GLOBAL#QUEUE# counters (conservative/approximate under contention). 0 (default) disables the global queued cap."
  type        = number
  default     = 0
}

variable "index_dispatch_max_attempts" {
  description = "Maximum transient-dispatch retry attempts before a queued job is marked failed with error_code=dispatch_exhausted."
  type        = number
  default     = 3

  validation {
    condition     = var.index_dispatch_max_attempts > 0
    error_message = "index_dispatch_max_attempts must be greater than zero."
  }
}

variable "index_dispatch_backoff_base_seconds" {
  description = "Base seconds for exponential backoff when re-queuing a job after a transient dispatch failure. The actual backoff is base * 2^(attempt-1), capped at a sane maximum."
  type        = number
  default     = 5

  validation {
    condition     = var.index_dispatch_backoff_base_seconds > 0
    error_message = "index_dispatch_backoff_base_seconds must be greater than zero."
  }
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
  description = "VPC ID for the ECS worker tasks (needs S3/RDS/SFN egress). Empty (default) discovers the account default VPC via data.aws_vpc.selected."
  type        = string
  default     = ""
}

variable "worker_subnets" {
  description = "Private subnets for Lambda and ECS worker tasks. Empty (default) discovers all subnets of the selected VPC. By default this module creates NAT-free VPC endpoints in these subnets."
  type        = list(string)
  default     = []
}

variable "interface_vpc_endpoint_subnet_ids" {
  description = "Subnet IDs for interface VPC endpoint ENIs. Empty (default) reuses worker_subnets/all discovered worker subnets; set one subnet for low-cost dev stacks."
  type        = list(string)
  default     = []

  validation {
    condition     = alltrue([for subnet_id in var.interface_vpc_endpoint_subnet_ids : length(trimspace(subnet_id)) > 0])
    error_message = "interface_vpc_endpoint_subnet_ids entries must be non-empty subnet IDs."
  }
}

variable "interface_vpc_endpoint_service_keys" {
  description = "Interface VPC endpoint service keys to create. Use [\"states\", \"secretsmanager\"] for Lambda-worker-only low-cost stacks; add ecr_api/ecr_dkr/logs/sts when private ECS fallback tasks need NAT-free ECR pull, CloudWatch Logs, or STS access."
  type        = set(string)
  default     = ["states", "secretsmanager", "ecr_api", "ecr_dkr", "logs", "sts"]

  validation {
    condition = alltrue([
      for service_key in var.interface_vpc_endpoint_service_keys :
      contains(["states", "secretsmanager", "ecr_api", "ecr_dkr", "logs", "sts"], service_key)
    ])
    error_message = "interface_vpc_endpoint_service_keys entries must be one of states, secretsmanager, ecr_api, ecr_dkr, logs, sts."
  }
}

variable "worker_route_table_ids" {
  description = "Route table IDs associated with worker_subnets for S3 and DynamoDB gateway endpoints. Required when create_vpc_endpoints is true."
  type        = list(string)
  default     = []

  validation {
    condition     = alltrue([for route_table_id in var.worker_route_table_ids : length(trimspace(route_table_id)) > 0])
    error_message = "worker_route_table_ids entries must be non-empty route table IDs."
  }
}

variable "vpc_endpoint_extra_client_sg_ids" {
  description = "Extra security group IDs (beyond the worker SG) allowed inbound 443 on the interface VPC endpoints. Needed for other clients sharing this VPC that rely on the endpoints' VPC-wide private DNS, e.g. the spur cloud-build VM in the default VPC."
  type        = list(string)
  default     = []
}

variable "create_vpc_endpoints" {
  description = "Create NAT-free VPC endpoints for worker access to S3, DynamoDB, Step Functions, Secrets Manager, ECR, CloudWatch Logs, and STS. Disable only when worker_subnets already have equivalent NAT or endpoints."
  type        = bool
  default     = true
}

variable "vpc_endpoint_region" {
  description = "Optional region override for AWS VPC endpoint service names. Defaults to aws_region."
  type        = string
  default     = null

  validation {
    condition     = var.vpc_endpoint_region == null ? true : length(trimspace(var.vpc_endpoint_region)) > 0
    error_message = "vpc_endpoint_region must be null or a non-empty region name."
  }
}

variable "worker_ecr_image" {
  description = "ECR image URI for the spur-context-worker container (e.g. <acct>.dkr.ecr.<region>.amazonaws.com/spur-context-worker:latest)"
  type        = string
}

variable "worker_lambda_image" {
  description = "ECR image URI for the Lambda-compatible spur-context-worker image"
  type        = string
}

variable "source_fetcher_lambda_image" {
  description = "ECR image URI for the non-VPC source fetcher Lambda image"
  type        = string
}

variable "manage_ecr_lifecycle_policies" {
  description = "Manage ECR lifecycle policies for context-service worker image repositories."
  type        = bool
  default     = true
}

variable "ecr_lifecycle_repository_names" {
  description = "Existing ECR repository names that should receive the context-service cleanup lifecycle policy."
  type        = set(string)
  default = [
    "spur-context-worker",
    "spur-context-worker-lambda",
    "spur-context-source-fetcher",
  ]

  validation {
    condition     = alltrue([for repository_name in var.ecr_lifecycle_repository_names : length(trimspace(repository_name)) > 0])
    error_message = "ecr_lifecycle_repository_names entries must be non-empty ECR repository names."
  }
}

variable "ecr_lifecycle_keep_tagged_images" {
  description = "Number of tagged images to retain in each context-service ECR repository."
  type        = number
  default     = 10

  validation {
    condition     = var.ecr_lifecycle_keep_tagged_images > 0
    error_message = "ecr_lifecycle_keep_tagged_images must be greater than zero."
  }
}

variable "ecr_lifecycle_untagged_image_days" {
  description = "Expire untagged ECR images older than this many days."
  type        = number
  default     = 7

  validation {
    condition     = var.ecr_lifecycle_untagged_image_days > 0
    error_message = "ecr_lifecycle_untagged_image_days must be greater than zero."
  }
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

variable "source_fetcher_lambda_timeout_sec" {
  description = "Source fetcher Lambda timeout in seconds. Lambda max is 900 seconds."
  type        = number
  default     = 900
}

variable "source_fetcher_lambda_memory_mb" {
  description = "Source fetcher Lambda memory allocation"
  type        = number
  default     = 1024
}

variable "source_fetcher_lambda_ephemeral_storage_mb" {
  description = "Source fetcher Lambda /tmp storage in MB. Lambda max is 10240 MB."
  type        = number
  default     = 10240
}

variable "source_fetch_presign_seconds" {
  description = "Validity period in seconds for presigned fetch artifact URLs returned to workers"
  type        = number
  default     = 21600
}

variable "fetch_artifact_retention_days" {
  description = "Number of days to retain staged fetch artifacts under s3://<bucket>/fetch/"
  type        = number
  default     = 7
}

locals {
  # Must end in `/gold/data` so catalog.rs `snapshot_base_uri` strips that
  # suffix to derive the bucket root and writes the frozen snapshot pointer at
  # `s3://<bucket>/gold/catalog-snapshot/current.json` — matching the serving
  # `catalog_s3_uri` default. A bare `.../data/` path offsets the entire gold
  # layer one level deep (`.../data/gold/...`) and serving never finds it.
  context_ducklake_data_path      = coalesce(var.context_ducklake_data_path, "s3://${var.bucket_name}/gold/data/")
  worker_checkpoint_uri_template  = "s3://${var.bucket_name}/jobs/{}/checkpoint.json"
  aurora_subnet_ids               = var.aurora_subnets != null ? var.aurora_subnets : local.net_subnet_ids
  aurora_catalog_dsn              = "postgres:host=${aws_rds_cluster.catalog.endpoint} port=${aws_rds_cluster.catalog.port} dbname=${var.aurora_database_name} user=${var.aurora_master_username} sslmode=require"
  aurora_master_secret_arn        = aws_rds_cluster.catalog.master_user_secret[0].secret_arn
  aurora_master_password_valuearn = "${local.aurora_master_secret_arn}:password::"
}
