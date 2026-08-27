# Variables for the original single-environment deployment (the "default" env
# that predates the staging/prod split). Deploy with:
#   ./deploy.sh --env default   (or -var-file=env/default.tfvars)
#
# Network (vpc_id / worker_subnets / worker_route_table_ids) is intentionally
# omitted: network.tf discovers the account default VPC via data sources.
# Package/image values (lambda_zip_path, worker_ecr_image, worker_lambda_image,
# source_fetcher_lambda_image) are injected by deploy.sh at build time.
# Names (bucket_name, table names, aurora identifier) use their secure module
# defaults, which already match this deployment (spur-context*).

aws_region = "ap-southeast-5"

# In-process concurrent invocations for the serving Lambda (run_concurrent).
# Independent of provisioned concurrency / warm instances.
lambda_max_concurrency = 4

# Live default stack has DNS-validated custom domains activated for
# context.getspur.dev + auth.context.getspur.dev. Keep this true so routine
# terraform applies do not destroy API mappings / ACM / Route53 records.
# execute-api remains enabled until clients fully migrate.
custom_domains_enabled       = true
disable_execute_api_endpoint = false

# Keep interface VPC endpoints in a single AZ/subnet for the default demo/eval
# stack to reduce PrivateLink endpoint-hour cost. Worker Lambda/ECS placement
# still uses the discovered default-VPC subnets.
interface_vpc_endpoint_subnet_ids = ["subnet-0e57004af78597f73"]

# Low-cost Lambda-worker mode: keep only the interface endpoints used by the
# serving/index orchestration path. ECR/logs/STS endpoints are intentionally not
# created here; use NAT or add those keys back before relying on ECS fallback.
interface_vpc_endpoint_service_keys = ["states", "secretsmanager"]

# The existing DuckLake catalog for this stack stores its data at
# s3://spur-context/data/ (NOT the module default s3://<bucket>/gold/data/).
# Must match the catalog's recorded DATA_PATH or attach fails with a mismatch.
context_ducklake_data_path = "s3://spur-context/data/"

# Serving reads the frozen-snapshot pointer here. The worker derives the pointer
# location from context_ducklake_data_path: for a bare .../data/ path it lands at
# .../data/gold/catalog-snapshot/current.json (snapshot_base_uri only strips a
# /gold/data suffix). The module default (.../gold/catalog-snapshot/...) assumes
# the /gold/data data path, so serving must be pointed at the /data/-derived key
# or it 404s and returns empty for every query.
catalog_s3_uri = "s3://spur-context/data/gold/catalog-snapshot/current.json"

# This stack is intentionally public/unauthenticated (demo/eval). The module
# default is AWS_IAM (SigV4); staging/prod keep that secure default. Do NOT move
# this to terraform.tfvars — that auto-loads into every env and would silently
# make staging/prod public too.
api_authorization_type = "NONE"

# Keep the demo/eval deployment on its public Code and Knowledge compatibility
# routes while adding exact Cognito and personal-key routes for evaluation.
cognito_auth_enabled   = true
cognito_user_pool_name = "spur-context-default-cognito"
cognito_domain_prefix  = "spur-context-default-auth-065285885105"
cognito_human_callback_urls = [
  "http://127.0.0.1:8765/callback",
]
cognito_human_logout_urls = [
  "http://127.0.0.1:8765/logout",
]

# Google credentials are supplied only through protected TF_VAR environment
# values loaded from the owner-only Google Auth Platform client JSON.
google_oauth_enabled = true

# Personal API keys are additive; the existing public demo route remains
# available for compatibility while the exact authenticated routes are tested.
api_key_auth_enabled = true

# This stack's API is public (NONE), and index/index_status are meant to be
# callable without an authenticated caller. Enable the shared anonymous identity
# for mutating tools (external_index/external_index_status). The anonymous caller
# still shares the per-caller rate limit / active-job cap.
allow_anonymous_mutations = true

# Enable the bounded external-index backlog for the public demo/eval stack.
# A single queue shard makes the small global cap exact rather than approximate.
index_max_queued_jobs_per_owner     = 10
index_max_queued_jobs_global        = 10
index_max_running_jobs_per_owner    = 2
index_max_running_jobs_global       = 2
index_queue_shard_count             = 1
index_drainer_batch_limit           = 2
index_drainer_scan_limit_per_shard  = 10
index_drainer_schedule_rate_minutes = 1

# Preserve current reality (live cluster has protection off). The module default
# is true; pinning false here keeps a scoped worker-Lambda deploy from silently
# enabling deletion protection. Flip to true deliberately when desired.
aurora_deletion_protection = false

# The spur cloud-build VM runs in this same default VPC, so it resolves the
# private ECR/secrets endpoints via VPC-wide private DNS and must be admitted
# on the endpoint SG (sg-0e8c8762149e621d8 = spur-builder-ssm). Without this,
# docker push during deploy times out against api.ecr.ap-southeast-5.
vpc_endpoint_extra_client_sg_ids = ["sg-0e8c8762149e621d8"]
