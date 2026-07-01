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

# Preserve current reality (live cluster has protection off). The module default
# is true; pinning false here keeps a scoped worker-Lambda deploy from silently
# enabling deletion protection. Flip to true deliberately when desired.
aurora_deletion_protection = false

# The spur cloud-build VM runs in this same default VPC, so it resolves the
# private ECR/secrets endpoints via VPC-wide private DNS and must be admitted
# on the endpoint SG (sg-0e8c8762149e621d8 = spur-builder-ssm). Without this,
# docker push during deploy times out against api.ecr.ap-southeast-5.
vpc_endpoint_extra_client_sg_ids = ["sg-0e8c8762149e621d8"]
