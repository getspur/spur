resource "aws_s3_bucket" "data" {
  bucket = var.bucket_name
}

resource "aws_s3_bucket_versioning" "data" {
  bucket = aws_s3_bucket.data.id

  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "data" {
  bucket = aws_s3_bucket.data.id

  rule {
    id     = "expire-fetch-artifacts"
    status = "Enabled"

    filter {
      prefix = "fetch/"
    }

    expiration {
      days = var.fetch_artifact_retention_days
    }

    noncurrent_version_expiration {
      noncurrent_days = var.fetch_artifact_retention_days
    }
  }
}

resource "aws_s3_bucket_ownership_controls" "data" {
  bucket = aws_s3_bucket.data.id

  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_dynamodb_table" "index_jobs" {
  name         = var.index_jobs_table_name
  billing_mode = "PAY_PER_REQUEST"
  hash_key     = "pk"

  attribute {
    name = "pk"
    type = "S"
  }

  # Sparse queue GSI keyed by (queue_shard, queue_sort_key). Only queued job
  # records carry these attributes (set at enqueue, removed at dispatch), so the
  # GSI only indexes the active backlog — keeping it sparse and cheap. The
  # drainer queries this GSI in FIFO order and deserializes full JOB#<job_id>
  # records directly from Query results. ALL keeps that access pattern to one
  # request per shard.
  # See docs/superpowers/specs/2026-07-10-context-service-index-queue-backpressure-design.md
  attribute {
    name = "queue_shard"
    type = "S"
  }

  attribute {
    name = "queue_sort_key"
    type = "S"
  }

  global_secondary_index {
    name            = var.index_queue_gsi_name
    hash_key        = "queue_shard"
    range_key       = "queue_sort_key"
    projection_type = "ALL"
  }

  point_in_time_recovery {
    enabled = true
  }

  server_side_encryption {
    enabled = true
  }

  ttl {
    attribute_name = "expires_at"
    enabled        = true
  }
}

resource "aws_dynamodb_table" "catalog_leases" {
  name         = var.catalog_leases_table_name
  billing_mode = "PAY_PER_REQUEST"
  hash_key     = "pk"

  attribute {
    name = "pk"
    type = "S"
  }

  point_in_time_recovery {
    enabled = true
  }

  server_side_encryption {
    enabled = true
  }

  ttl {
    attribute_name = "expires_at_unix_secs"
    enabled        = true
  }
}

resource "aws_db_subnet_group" "catalog" {
  name        = "${var.aurora_cluster_identifier}-subnets"
  description = "Private subnets for the SPUR context ingest catalog"
  subnet_ids  = local.aurora_subnet_ids
}

resource "aws_rds_cluster" "catalog" {
  cluster_identifier = var.aurora_cluster_identifier

  engine         = "aurora-postgresql"
  engine_mode    = "provisioned"
  engine_version = var.aurora_engine_version
  database_name  = var.aurora_database_name

  master_username             = var.aurora_master_username
  manage_master_user_password = true

  db_subnet_group_name   = aws_db_subnet_group.catalog.name
  vpc_security_group_ids = [aws_security_group.catalog_db.id]

  backup_retention_period = var.aurora_backup_retention_days
  copy_tags_to_snapshot   = true
  deletion_protection     = var.aurora_deletion_protection
  skip_final_snapshot     = true
  storage_encrypted       = true

  serverlessv2_scaling_configuration {
    min_capacity             = 0
    max_capacity             = var.aurora_max_acu
    seconds_until_auto_pause = var.aurora_seconds_until_auto_pause
  }
}

resource "aws_rds_cluster_instance" "catalog_writer" {
  identifier         = "${var.aurora_cluster_identifier}-writer-1"
  cluster_identifier = aws_rds_cluster.catalog.id

  engine         = aws_rds_cluster.catalog.engine
  engine_version = aws_rds_cluster.catalog.engine_version
  instance_class = "db.serverless"

  db_subnet_group_name = aws_db_subnet_group.catalog.name
  publicly_accessible  = false
}

resource "aws_cloudwatch_log_group" "lambda" {
  name              = "/aws/lambda/spur-context-service"
  retention_in_days = 14
}

resource "aws_s3_object" "lambda_zip" {
  bucket      = aws_s3_bucket.data.bucket
  key         = "lambda/spur-context-service-${filemd5(var.lambda_zip_path)}.zip"
  source      = var.lambda_zip_path
  source_hash = filemd5(var.lambda_zip_path)
}

resource "aws_lambda_function" "service" {
  function_name = "spur-context-service"
  description   = "DuckLake-served code context MCP service"

  # Deploy via S3 (direct UpdateFunctionCode is capped at ~70 MB; the bundled
  # libduckdb + ducklake/httpfs extensions push the zip past that). S3-backed
  # deployment supports up to 250 MB.
  s3_bucket        = aws_s3_bucket.data.bucket
  s3_key           = aws_s3_object.lambda_zip.key
  source_code_hash = filebase64sha256(var.lambda_zip_path)

  runtime       = "provided.al2023"
  architectures = ["arm64"]
  handler       = "bootstrap"

  role        = aws_iam_role.lambda.arn
  timeout     = var.lambda_timeout_sec
  memory_size = var.lambda_memory_mb

  environment {
    variables = {
      SPUR_CATALOG_S3_URI                       = var.catalog_s3_uri
      SPUR_INDEX_STATE_MACHINE_ARN              = aws_sfn_state_machine.index_build.arn
      SPUR_INDEX_JOBS_TABLE                     = aws_dynamodb_table.index_jobs.name
      SPUR_INDEX_QUEUE_GSI_NAME                 = var.index_queue_gsi_name
      SPUR_CATALOG_LEASES_TABLE                 = aws_dynamodb_table.catalog_leases.name
      SPUR_INDEX_RATE_LIMIT_PER_MINUTE          = tostring(var.index_rate_limit_per_minute)
      SPUR_INDEX_MAX_CONCURRENT_JOBS_PER_CALLER = tostring(var.index_max_concurrent_jobs_per_caller)
      SPUR_CONTEXT_MAX_TARBALL_BYTES            = tostring(var.context_max_tarball_bytes)
      SPUR_CONTEXT_MAX_GIT_BYTES                = tostring(var.context_max_git_bytes)
      SPUR_CONTEXT_MAX_BUILD_SECONDS            = tostring(var.context_max_build_seconds)
      SPUR_CONTEXT_ALLOWED_SOURCE_DOMAINS       = join(",", var.allowed_source_domains)
      SPUR_CONTEXT_ALLOW_ANONYMOUS_MUTATIONS    = var.allow_anonymous_mutations ? "1" : "0"
      # Bounded backlog queueing config. Defaults preserve current behavior
      # (reject over capacity) until an operator sets a non-zero queue cap.
      SPUR_INDEX_QUEUE_SHARD_COUNT             = tostring(var.index_queue_shard_count)
      SPUR_INDEX_MAX_RUNNING_JOBS_PER_OWNER    = tostring(coalesce(var.index_max_running_jobs_per_owner, var.index_max_concurrent_jobs_per_caller))
      SPUR_INDEX_MAX_QUEUED_JOBS_PER_OWNER     = tostring(var.index_max_queued_jobs_per_owner)
      SPUR_INDEX_MAX_RUNNING_JOBS_GLOBAL       = tostring(var.index_max_running_jobs_global)
      SPUR_INDEX_MAX_QUEUED_JOBS_GLOBAL        = tostring(var.index_max_queued_jobs_global)
      SPUR_INDEX_DRAINER_BATCH_LIMIT           = tostring(var.index_drainer_batch_limit)
      SPUR_INDEX_DRAINER_SCAN_LIMIT_PER_SHARD  = tostring(var.index_drainer_scan_limit_per_shard)
      SPUR_INDEX_DISPATCH_MAX_ATTEMPTS         = tostring(var.index_dispatch_max_attempts)
      SPUR_INDEX_DISPATCH_BACKOFF_BASE_SECONDS = tostring(var.index_dispatch_backoff_base_seconds)
    }
  }

  depends_on = [
    aws_iam_role_policy_attachment.lambda_basic,
    aws_cloudwatch_log_group.lambda,
  ]
}

# A successful admission kick is only a latency optimization. This scheduled
# invocation is the correctness path that eventually drains queued work after
# running capacity becomes available.
resource "aws_cloudwatch_event_rule" "index_queue_drainer" {
  name                = "spur-context-index-queue-drainer"
  description         = "Periodically dispatch bounded context-service index backlog"
  schedule_expression = "rate(${var.index_drainer_schedule_rate_minutes} ${var.index_drainer_schedule_rate_minutes == 1 ? "minute" : "minutes"})"
}

resource "aws_cloudwatch_event_target" "index_queue_drainer" {
  rule      = aws_cloudwatch_event_rule.index_queue_drainer.name
  target_id = "spur-context-service-drainer"
  arn       = aws_lambda_function.service.arn
  input = jsonencode({
    source      = "aws.events"
    detail-type = "Scheduled Event"
    detail = {
      operation = "drain_queued_jobs"
    }
  })
}

resource "aws_lambda_provisioned_concurrency_config" "warm" {
  count                             = var.concurrent_warm_instances > 0 ? 1 : 0
  function_name                     = aws_lambda_function.service.function_name
  provisioned_concurrent_executions = var.concurrent_warm_instances
  qualifier                         = "$LATEST"
}

resource "aws_apigatewayv2_api" "http" {
  name          = "spur-context-service"
  protocol_type = "HTTP"
}

resource "aws_apigatewayv2_integration" "lambda" {
  api_id                 = aws_apigatewayv2_api.http.id
  integration_type       = "AWS_PROXY"
  integration_method     = "POST"
  integration_uri        = aws_lambda_function.service.invoke_arn
  payload_format_version = "2.0"
}

resource "aws_apigatewayv2_route" "default" {
  api_id             = aws_apigatewayv2_api.http.id
  route_key          = "$default"
  target             = "integrations/${aws_apigatewayv2_integration.lambda.id}"
  authorization_type = var.api_authorization_type
}

resource "aws_apigatewayv2_stage" "default" {
  api_id      = aws_apigatewayv2_api.http.id
  name        = "$default"
  auto_deploy = true

  default_route_settings {
    throttling_burst_limit = var.api_throttle_burst_limit
    throttling_rate_limit  = var.api_throttle_rate_limit
  }
}

resource "aws_lambda_permission" "apigw" {
  statement_id  = "apigateway-invocation"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.service.function_name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_apigatewayv2_api.http.execution_arn}/*/*"
}

resource "aws_lambda_permission" "eventbridge_drainer" {
  statement_id  = "eventbridge-index-queue-drainer"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.service.function_name
  principal     = "events.amazonaws.com"
  source_arn    = aws_cloudwatch_event_rule.index_queue_drainer.arn
}
