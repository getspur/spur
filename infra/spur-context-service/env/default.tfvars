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

# This stack's API is public (NONE), and index/index_status are meant to be
# callable without an authenticated caller. Enable the shared anonymous identity
# for mutating tools (external_index/external_index_status). The anonymous caller
# still shares the per-caller rate limit / active-job cap.
allow_anonymous_mutations = true

# Preserve current reality (live cluster has protection off). The module default
# is true; pinning false here keeps a scoped worker-Lambda deploy from silently
# enabling deletion protection. Flip to true deliberately when desired.
aurora_deletion_protection = false

# The spur cloud-build VM runs in this same default VPC, so it resolves the
# private ECR/secrets endpoints via VPC-wide private DNS and must be admitted
# on the endpoint SG (sg-0e8c8762149e621d8 = spur-builder-ssm). Without this,
# docker push during deploy times out against api.ecr.ap-southeast-5.
vpc_endpoint_extra_client_sg_ids = ["sg-0e8c8762149e621d8"]
