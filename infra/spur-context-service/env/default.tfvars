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

# This stack is intentionally public/unauthenticated (demo/eval). The module
# default is AWS_IAM (SigV4); staging/prod keep that secure default. Do NOT move
# this to terraform.tfvars — that auto-loads into every env and would silently
# make staging/prod public too.
api_authorization_type = "NONE"
