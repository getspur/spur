# Placeholder-only defaults. Deployments should pass env/<environment>.tfvars
# explicitly through deploy.sh or terraform -var-file; do not commit real
# account, network, image, or secret values here.

aws_region = "ap-southeast-5"

lambda_memory_mb          = 2048
lambda_timeout_sec        = 30
concurrent_warm_instances = 0

api_throttle_rate_limit              = 20
api_throttle_burst_limit             = 40
index_rate_limit_per_minute          = 10
index_max_concurrent_jobs_per_caller = 2
context_max_tarball_bytes            = 524288000
context_max_git_bytes                = 2147483648
context_max_build_seconds            = 1800
allowed_source_domains               = []

# Networking is discovered by default: leaving vpc_id/worker_subnets/
# worker_route_table_ids unset makes network.tf resolve the account default VPC,
# its subnets, and its route tables via data sources. staging/prod override
# these in env/<env>.tfvars to pin a dedicated VPC.

worker_ecr_image    = "123456789012.dkr.ecr.ap-southeast-5.amazonaws.com/spur-context-worker:latest"
worker_lambda_image = "123456789012.dkr.ecr.ap-southeast-5.amazonaws.com/spur-context-worker-lambda:latest"
