resource "aws_s3_bucket" "data" {
  bucket = var.bucket_name
}

resource "aws_s3_bucket_versioning" "data" {
  bucket = aws_s3_bucket.data.id

  versioning_configuration {
    status = "Enabled"
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
      SPUR_CATALOG_S3_URI          = var.catalog_s3_uri
      SPUR_INDEX_STATE_MACHINE_ARN = aws_sfn_state_machine.index_build.arn
      SPUR_INDEX_JOBS_TABLE        = aws_dynamodb_table.index_jobs.name
      SPUR_CATALOG_LEASES_TABLE    = aws_dynamodb_table.catalog_leases.name
    }
  }

  depends_on = [
    aws_iam_role_policy_attachment.lambda_basic,
    aws_cloudwatch_log_group.lambda,
  ]
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
  target        = aws_lambda_function.service.arn
}

resource "aws_lambda_permission" "apigw" {
  statement_id  = "apigateway-invocation"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.service.function_name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_apigatewayv2_api.http.execution_arn}/*/*"
}
