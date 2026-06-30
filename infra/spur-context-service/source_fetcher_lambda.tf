# Non-VPC source fetcher. It performs internet source acquisition, stages the
# normalized archive under s3://<bucket>/fetch/, and returns a presigned HTTPS
# tarball URL for the worker backends.

resource "aws_cloudwatch_log_group" "source_fetcher_lambda" {
  name              = "/aws/lambda/spur-context-source-fetcher"
  retention_in_days = 14
}

resource "aws_lambda_function" "source_fetcher" {
  function_name = "spur-context-source-fetcher"
  description   = "Stateless source fetcher for context indexing"

  package_type  = "Image"
  image_uri     = var.source_fetcher_lambda_image
  architectures = ["arm64"]
  role          = aws_iam_role.source_fetcher_lambda.arn
  timeout       = var.source_fetcher_lambda_timeout_sec
  memory_size   = var.source_fetcher_lambda_memory_mb
  publish       = true

  ephemeral_storage {
    size = var.source_fetcher_lambda_ephemeral_storage_mb
  }

  environment {
    variables = {
      SPUR_CONTEXT_FETCH_BUCKET           = aws_s3_bucket.data.bucket
      SPUR_CONTEXT_FETCH_PREFIX           = "fetch"
      SPUR_CONTEXT_MAX_TARBALL_BYTES      = tostring(var.context_max_tarball_bytes)
      SPUR_CONTEXT_MAX_GIT_BYTES          = tostring(var.context_max_git_bytes)
      SPUR_CONTEXT_ALLOWED_SOURCE_DOMAINS = join(",", var.allowed_source_domains)
      SPUR_CONTEXT_FETCH_PRESIGN_SECONDS  = tostring(var.source_fetch_presign_seconds)
    }
  }

  depends_on = [
    aws_cloudwatch_log_group.source_fetcher_lambda,
    aws_iam_role_policy.source_fetcher_lambda,
  ]
}

resource "aws_lambda_alias" "source_fetcher_live" {
  name             = "live"
  description      = "Live source fetcher alias"
  function_name    = aws_lambda_function.source_fetcher.function_name
  function_version = aws_lambda_function.source_fetcher.version
}
