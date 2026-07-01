# Lambda worker for low-latency indexing attempts.
#
# The ECS worker remains available as the fallback for Lambda timeouts or
# Lambda service-side failures. Lambda is capped at 15 minutes, 10 GB memory,
# and 10 GB /tmp, so it should be treated as the fast path rather than the only
# worker backend. The general Lambda memory limit is higher, but this
# account/region currently rejects worker memory above 3008 MB.

resource "aws_cloudwatch_log_group" "worker_lambda" {
  name              = "/aws/lambda/spur-context-worker"
  retention_in_days = 14
}

resource "aws_lambda_function" "worker" {
  function_name = "spur-context-worker"
  description   = "Fast-start context indexing worker"

  package_type  = "Image"
  image_uri     = var.worker_lambda_image
  architectures = ["arm64"]
  role          = aws_iam_role.lambda.arn
  timeout       = var.worker_lambda_timeout_sec
  memory_size   = var.worker_lambda_memory_mb
  publish       = true

  ephemeral_storage {
    size = var.worker_lambda_ephemeral_storage_mb
  }

  vpc_config {
    subnet_ids         = local.net_subnet_ids
    security_group_ids = [aws_security_group.worker.id]
  }

  environment {
    variables = {
      SPUR_INDEX_JOBS_TABLE            = aws_dynamodb_table.index_jobs.name
      SPUR_CATALOG_LEASES_TABLE        = aws_dynamodb_table.catalog_leases.name
      SPUR_CATALOG_DSN                 = local.aurora_catalog_dsn
      SPUR_CATALOG_PASSWORD_SECRET_ARN = local.aurora_master_secret_arn
      SPUR_CONTEXT_DUCKLAKE_DATA_PATH  = local.context_ducklake_data_path
      SPUR_CONTEXT_WORKER_LAMBDA_MODE  = "1"
      SPUR_CONTEXT_MAX_TARBALL_BYTES   = tostring(var.context_max_tarball_bytes)
      SPUR_CONTEXT_MAX_GIT_BYTES       = tostring(var.context_max_git_bytes)
      SPUR_CONTEXT_MAX_BUILD_SECONDS   = tostring(var.context_max_build_seconds)
    }
  }

  depends_on = [
    aws_iam_role_policy_attachment.lambda_basic,
    aws_iam_role_policy_attachment.lambda_vpc_access,
    aws_iam_role_policy.lambda_catalog_secret,
    aws_cloudwatch_log_group.worker_lambda,
  ]
}

resource "aws_lambda_alias" "worker_live" {
  name             = "live"
  description      = "Live fast-start worker alias"
  function_name    = aws_lambda_function.worker.function_name
  function_version = aws_lambda_function.worker.version
}

resource "aws_lambda_provisioned_concurrency_config" "worker_warm" {
  count                             = var.worker_lambda_provisioned_concurrency > 0 ? 1 : 0
  function_name                     = aws_lambda_function.worker.function_name
  provisioned_concurrent_executions = var.worker_lambda_provisioned_concurrency
  qualifier                         = aws_lambda_alias.worker_live.name
}
