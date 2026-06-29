# Staging variables for spur-context-service.
# Replace placeholder network and account values before deploying.

aws_region = "ap-southeast-5"

bucket_name                 = "spur-context-staging"
catalog_s3_uri              = "s3://spur-context-staging/gold/catalog-snapshot/current.json"
context_ducklake_data_path  = "s3://spur-context-staging/data/"
index_jobs_table_name       = "spur-context-index-jobs-staging"
catalog_leases_table_name   = "spur-context-catalog-leases-staging"
aurora_cluster_identifier   = "spur-context-catalog-staging"
aurora_deletion_protection  = false
concurrent_warm_instances   = 0
api_throttle_rate_limit     = 20
api_throttle_burst_limit    = 40
index_rate_limit_per_minute = 10
context_max_tarball_bytes   = 524288000
context_max_git_bytes       = 2147483648
context_max_build_seconds   = 1800
allowed_source_domains      = []

index_max_concurrent_jobs_per_caller = 2

# VPC with NAT gateway or VPC endpoints for S3, Step Functions, ECR, and RDS.
vpc_id = "vpc-staging-placeholder"

# Private subnets with outbound egress.
worker_subnets = [
  "subnet-staging-a",
  "subnet-staging-b",
]

worker_ecr_image    = "123456789012.dkr.ecr.ap-southeast-5.amazonaws.com/spur-context-worker:staging"
worker_lambda_image = "123456789012.dkr.ecr.ap-southeast-5.amazonaws.com/spur-context-worker-lambda:staging"
