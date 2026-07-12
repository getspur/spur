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
      SPUR_INDEX_DRAINER_SCHEDULE_RATE_MINUTES = tostring(var.index_drainer_schedule_rate_minutes)
      SPUR_INDEX_DISPATCH_MAX_ATTEMPTS         = tostring(var.index_dispatch_max_attempts)
      SPUR_INDEX_DISPATCH_BACKOFF_BASE_SECONDS = tostring(var.index_dispatch_backoff_base_seconds)
      # Cognito values are validation/discovery metadata only. Never pass an
      # app-client secret, bearer token, authorization code, or PKCE value to
      # this Lambda; API Gateway performs cryptographic JWT verification.
      SPUR_COGNITO_AUTH_ENABLED       = var.cognito_auth_enabled ? "1" : "0"
      SPUR_COGNITO_ISSUER             = local.cognito_issuer
      SPUR_COGNITO_HUMAN_CLIENT_ID    = var.cognito_auth_enabled ? aws_cognito_user_pool_client.human[0].id : ""
      SPUR_COGNITO_M2M_CLIENT_IDS     = join(",", [for client in values(aws_cognito_user_pool_client.m2m) : client.id])
      SPUR_COGNITO_RESOURCE_SERVER_ID = var.cognito_resource_server_identifier
      SPUR_COGNITO_DENY_CLIENT_IDS    = join(",", var.cognito_emergency_deny_client_ids)
      SPUR_COGNITO_OAUTH_PATH         = "/mcp/oauth"
      # API-key configuration contains identifiers and bounds only. Raw keys,
      # digests, headers, and OAuth credentials never enter Lambda environment
      # state or the access-log format.
      SPUR_API_KEY_AUTH_ENABLED           = var.api_key_auth_enabled ? "1" : "0"
      SPUR_API_KEY_ENVIRONMENT            = var.api_key_environment
      SPUR_API_KEY_DEFAULT_TTL_DAYS       = tostring(var.api_key_default_ttl_days)
      SPUR_API_KEY_MAX_TTL_DAYS           = tostring(var.api_key_max_ttl_days)
      SPUR_CONTEXT_API_KEYS_TABLE         = var.api_key_auth_enabled ? aws_dynamodb_table.api_keys[0].name : ""
      SPUR_COGNITO_AUTHORIZATION_ENDPOINT = var.cognito_auth_enabled ? "${local.cognito_domain_url}/oauth2/authorize" : ""
      SPUR_COGNITO_TOKEN_ENDPOINT         = var.cognito_auth_enabled ? "${local.cognito_domain_url}/oauth2/token" : ""
      SPUR_CONTEXT_SERVICE_BASE_URL       = var.cognito_auth_enabled ? local.context_service_base_url : ""
      SPUR_CONTEXT_LOGIN_REDIRECT_ENABLED = var.custom_domains_enabled && var.cognito_auth_enabled ? "1" : "0"
    }
  }

  depends_on = [
    aws_iam_role_policy_attachment.lambda_basic,
    aws_cloudwatch_log_group.lambda,
    aws_iam_role_policy.api_key_management,
    # Do not advertise the custom API/OAuth endpoints until their API mapping
    # and DNS aliases exist. This also keeps the login facade unavailable during
    # a partial certificate/domain activation apply.
    aws_apigatewayv2_api_mapping.context_service,
    aws_route53_record.api_custom_domain_ipv4,
    aws_route53_record.api_custom_domain_ipv6,
    aws_route53_record.cognito_custom_domain,
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
  name                         = "spur-context-service"
  protocol_type                = "HTTP"
  disable_execute_api_endpoint = var.disable_execute_api_endpoint

  lifecycle {
    precondition {
      condition     = !var.disable_execute_api_endpoint || var.custom_domains_enabled
      error_message = "disable_execute_api_endpoint requires custom_domains_enabled so the service remains reachable."
    }

    precondition {
      condition     = !var.custom_domains_enabled || var.cognito_auth_enabled
      error_message = "custom_domains_enabled requires cognito_auth_enabled so activation creates both the API and Cognito custom domains."
    }
  }
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

  # The format deliberately contains route, status, latency, and bounded API
  # error metadata only. It never includes Authorization, JWT claims, client
  # secrets, OAuth codes, or request/response bodies.
  dynamic "access_log_settings" {
    for_each = var.cognito_auth_enabled ? [1] : []

    content {
      destination_arn = aws_cloudwatch_log_group.oauth_api_access[0].arn
      format = jsonencode({
        request_id         = "$context.requestId"
        request_time       = "$context.requestTime"
        route_key          = "$context.routeKey"
        status             = "$context.status"
        integration_status = "$context.integrationStatus"
        response_latency   = "$context.responseLatency"
        error_message      = "$context.error.message"
      })
    }
  }
}

# Phase 1: bootstrap only. This public sub-zone must be delegated from
# Namecheap before activation so ACM can validate without blocking the first
# apply. Terraform intentionally does not manage the getspur.dev parent zone.
resource "aws_route53_zone" "context_service" {
  name    = local.context_service_domain_name
  comment = "Delegated public zone for the SPUR context service"

  tags = {
    Service   = "spur-context-service"
    ManagedBy = "terraform"
  }
}

# Phase 2: activation. The API certificate is regional and therefore uses the
# default ap-southeast-5 provider. Cognito's CloudFront-backed certificate uses
# the explicit us-east-1 provider.
resource "aws_acm_certificate" "api_custom_domain" {
  count = var.custom_domains_enabled ? 1 : 0

  domain_name       = local.context_service_domain_name
  validation_method = "DNS"

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_route53_record" "api_certificate_validation" {
  count = var.custom_domains_enabled ? 1 : 0

  zone_id = aws_route53_zone.context_service.zone_id
  name    = one(aws_acm_certificate.api_custom_domain[0].domain_validation_options).resource_record_name
  type    = one(aws_acm_certificate.api_custom_domain[0].domain_validation_options).resource_record_type
  ttl     = 60
  records = [one(aws_acm_certificate.api_custom_domain[0].domain_validation_options).resource_record_value]
}

resource "aws_acm_certificate_validation" "api_custom_domain" {
  count = var.custom_domains_enabled ? 1 : 0

  certificate_arn         = aws_acm_certificate.api_custom_domain[0].arn
  validation_record_fqdns = aws_route53_record.api_certificate_validation[*].fqdn
}

resource "aws_acm_certificate" "cognito_custom_domain" {
  provider = aws.us_east_1
  count    = var.custom_domains_enabled ? 1 : 0

  domain_name       = local.cognito_custom_domain_name
  validation_method = "DNS"

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_route53_record" "cognito_certificate_validation" {
  count = var.custom_domains_enabled ? 1 : 0

  zone_id = aws_route53_zone.context_service.zone_id
  name    = one(aws_acm_certificate.cognito_custom_domain[0].domain_validation_options).resource_record_name
  type    = one(aws_acm_certificate.cognito_custom_domain[0].domain_validation_options).resource_record_type
  ttl     = 60
  records = [one(aws_acm_certificate.cognito_custom_domain[0].domain_validation_options).resource_record_value]
}

resource "aws_acm_certificate_validation" "cognito_custom_domain" {
  provider = aws.us_east_1
  count    = var.custom_domains_enabled ? 1 : 0

  certificate_arn         = aws_acm_certificate.cognito_custom_domain[0].arn
  validation_record_fqdns = aws_route53_record.cognito_certificate_validation[*].fqdn
}

resource "aws_apigatewayv2_domain_name" "context_service" {
  count = var.custom_domains_enabled ? 1 : 0

  domain_name = local.context_service_domain_name

  domain_name_configuration {
    certificate_arn = aws_acm_certificate_validation.api_custom_domain[0].certificate_arn
    endpoint_type   = "REGIONAL"
    ip_address_type = "dualstack"
    security_policy = "TLS_1_2"
  }
}

resource "aws_apigatewayv2_api_mapping" "context_service" {
  count = var.custom_domains_enabled ? 1 : 0

  api_id      = aws_apigatewayv2_api.http.id
  domain_name = aws_apigatewayv2_domain_name.context_service[0].id
  stage       = aws_apigatewayv2_stage.default.name
}

resource "aws_route53_record" "api_custom_domain_ipv4" {
  count = var.custom_domains_enabled ? 1 : 0

  zone_id = aws_route53_zone.context_service.zone_id
  name    = local.context_service_domain_name
  type    = "A"

  alias {
    name                   = aws_apigatewayv2_domain_name.context_service[0].domain_name_configuration[0].target_domain_name
    zone_id                = aws_apigatewayv2_domain_name.context_service[0].domain_name_configuration[0].hosted_zone_id
    evaluate_target_health = false
  }
}

resource "aws_route53_record" "api_custom_domain_ipv6" {
  count = var.custom_domains_enabled ? 1 : 0

  zone_id = aws_route53_zone.context_service.zone_id
  name    = local.context_service_domain_name
  type    = "AAAA"

  alias {
    name                   = aws_apigatewayv2_domain_name.context_service[0].domain_name_configuration[0].target_domain_name
    zone_id                = aws_apigatewayv2_domain_name.context_service[0].domain_name_configuration[0].hosted_zone_id
    evaluate_target_health = false
  }
}

# Cognito resources are deliberately all guarded by cognito_auth_enabled so a
# disabled default configuration has no user pool, domain, resource server, app
# client, JWT authorizer, OAuth route, or Cognito-specific observability state.
resource "aws_cognito_user_pool" "context_service" {
  count = var.cognito_auth_enabled ? 1 : 0

  name                = var.cognito_user_pool_name
  user_pool_tier      = "LITE"
  deletion_protection = var.cognito_user_pool_deletion_protection ? "ACTIVE" : "INACTIVE"

  username_attributes = ["email"]
  mfa_configuration   = "OPTIONAL"

  software_token_mfa_configuration {
    enabled = true
  }

  tags = {
    Service   = "spur-context-service"
    ManagedBy = "terraform"
  }
}

resource "aws_cognito_user_pool_domain" "context_service" {
  count = var.cognito_auth_enabled ? 1 : 0

  domain       = var.cognito_domain_prefix
  user_pool_id = aws_cognito_user_pool.context_service[0].id
}

resource "aws_cognito_user_pool_domain" "custom" {
  count = var.custom_domains_enabled && var.cognito_auth_enabled ? 1 : 0

  domain          = local.cognito_custom_domain_name
  certificate_arn = aws_acm_certificate_validation.cognito_custom_domain[0].certificate_arn
  user_pool_id    = aws_cognito_user_pool.context_service[0].id

  # Cognito rejects a custom domain unless its immediate parent has a public A
  # record. This dependency makes the API apex alias authoritative before the
  # Cognito CreateUserPoolDomain call is attempted.
  depends_on = [
    aws_route53_record.api_custom_domain_ipv4,
    aws_route53_record.api_custom_domain_ipv6,
  ]
}

resource "aws_route53_record" "cognito_custom_domain" {
  count = var.custom_domains_enabled && var.cognito_auth_enabled ? 1 : 0

  zone_id = aws_route53_zone.context_service.zone_id
  name    = local.cognito_custom_domain_name
  type    = "A"

  alias {
    name                   = aws_cognito_user_pool_domain.custom[0].cloudfront_distribution
    zone_id                = aws_cognito_user_pool_domain.custom[0].cloudfront_distribution_zone_id
    evaluate_target_health = false
  }
}

resource "aws_cognito_resource_server" "context_service" {
  count = var.cognito_auth_enabled ? 1 : 0

  identifier   = var.cognito_resource_server_identifier
  name         = "spur-context-service"
  user_pool_id = aws_cognito_user_pool.context_service[0].id

  dynamic "scope" {
    for_each = local.cognito_scope_descriptions

    content {
      scope_name        = scope.key
      scope_description = scope.value
    }
  }
}

resource "aws_cognito_identity_provider" "google" {
  count = var.cognito_auth_enabled && var.google_oauth_enabled ? 1 : 0

  user_pool_id  = aws_cognito_user_pool.context_service[0].id
  provider_name = "Google"
  provider_type = "Google"

  provider_details = {
    authorize_scopes = "openid email profile"
    client_id        = trimspace(var.google_oauth_client_id)
    client_secret    = var.google_oauth_client_secret
  }

  attribute_mapping = {
    email          = "email"
    email_verified = "email_verified"
    name           = "name"
    picture        = "picture"
  }
}

resource "aws_cognito_user_pool_client" "human" {
  count = var.cognito_auth_enabled ? 1 : 0

  name         = "spur-context-human"
  user_pool_id = aws_cognito_user_pool.context_service[0].id
  depends_on = [
    aws_cognito_resource_server.context_service,
    aws_cognito_identity_provider.google,
  ]
  generate_secret                      = false
  allowed_oauth_flows_user_pool_client = true
  allowed_oauth_flows                  = ["code"]
  allowed_oauth_scopes                 = local.cognito_human_allowed_oauth_scopes
  callback_urls                        = tolist(var.cognito_human_callback_urls)
  logout_urls                          = tolist(var.cognito_human_logout_urls)
  supported_identity_providers = concat(
    ["COGNITO"],
    var.google_oauth_enabled ? [aws_cognito_identity_provider.google[0].provider_name] : [],
  )
  enable_token_revocation       = true
  prevent_user_existence_errors = "ENABLED"

  access_token_validity  = var.cognito_human_access_token_minutes
  id_token_validity      = var.cognito_human_access_token_minutes
  refresh_token_validity = var.cognito_human_refresh_token_days

  token_validity_units {
    access_token  = "minutes"
    id_token      = "minutes"
    refresh_token = "days"
  }
}

resource "aws_cognito_user_pool_client" "m2m" {
  for_each = local.cognito_enabled_m2m_organizations

  name                                 = "spur-context-${each.key}"
  user_pool_id                         = aws_cognito_user_pool.context_service[0].id
  depends_on                           = [aws_cognito_resource_server.context_service]
  generate_secret                      = true
  allowed_oauth_flows_user_pool_client = true
  allowed_oauth_flows                  = ["client_credentials"]
  allowed_oauth_scopes = [
    for suffix in each.value.allowed_scopes :
    "${var.cognito_resource_server_identifier}/${suffix}"
  ]
  access_token_validity = local.cognito_m2m_access_token_hours[each.key]

  token_validity_units {
    access_token = "hours"
  }
}

resource "aws_apigatewayv2_authorizer" "cognito" {
  count = var.cognito_auth_enabled ? 1 : 0

  api_id           = aws_apigatewayv2_api.http.id
  authorizer_type  = "JWT"
  identity_sources = ["$request.header.Authorization"]
  name             = "spur-context-cognito"

  jwt_configuration {
    audience = concat(
      [aws_cognito_user_pool_client.human[0].id],
      [for client in values(aws_cognito_user_pool_client.m2m) : client.id],
    )
    issuer = local.cognito_issuer
  }
}

resource "aws_apigatewayv2_route" "oauth" {
  count = var.cognito_auth_enabled ? 1 : 0

  api_id               = aws_apigatewayv2_api.http.id
  route_key            = "POST /mcp/oauth"
  target               = "integrations/${aws_apigatewayv2_integration.lambda.id}"
  authorization_type   = "JWT"
  authorizer_id        = aws_apigatewayv2_authorizer.cognito[0].id
  authorization_scopes = local.cognito_custom_scopes
}

# Browsers may start human authorization from the memorable API hostname, but
# only this credential-free GET is public. Discovery and token exchange keep
# using the validated Cognito custom-domain endpoints directly.
resource "aws_apigatewayv2_route" "login_redirect" {
  count = var.custom_domains_enabled && var.cognito_auth_enabled ? 1 : 0

  api_id             = aws_apigatewayv2_api.http.id
  route_key          = "GET /auth/login"
  target             = "integrations/${aws_apigatewayv2_integration.lambda.id}"
  authorization_type = "NONE"
}

resource "aws_cloudwatch_log_group" "oauth_api_access" {
  count = var.cognito_auth_enabled ? 1 : 0

  name              = "/aws/apigateway/spur-context-service"
  retention_in_days = 14
}

resource "aws_cloudwatch_log_metric_filter" "oauth_route_5xx" {
  count = var.cognito_auth_enabled ? 1 : 0

  name           = "spur-context-oauth-route-5xx"
  log_group_name = aws_cloudwatch_log_group.oauth_api_access[0].name
  pattern        = "{ $.route_key = \"POST /mcp/oauth\" && $.status = 5* }"

  metric_transformation {
    name      = "OAuthRoute5xx"
    namespace = "SPUR/ContextServiceAuth"
    value     = "1"
  }
}

resource "aws_cloudwatch_metric_alarm" "oauth_route_5xx" {
  count = var.cognito_auth_enabled ? 1 : 0

  alarm_name          = "spur-context-oauth-route-5xx"
  alarm_description   = "POST /mcp/oauth returned one or more 5xx responses in five minutes."
  comparison_operator = "GreaterThanOrEqualToThreshold"
  evaluation_periods  = 1
  metric_name         = aws_cloudwatch_log_metric_filter.oauth_route_5xx[0].metric_transformation[0].name
  namespace           = "SPUR/ContextServiceAuth"
  period              = 300
  statistic           = "Sum"
  threshold           = 1
  treat_missing_data  = "notBreaching"
}

resource "aws_budgets_budget" "cognito" {
  count = var.cognito_auth_enabled && var.cognito_monthly_budget_usd != null ? 1 : 0

  name         = "spur-context-cognito-monthly"
  budget_type  = "COST"
  limit_amount = tostring(var.cognito_monthly_budget_usd)
  limit_unit   = "USD"
  time_unit    = "MONTHLY"

  cost_filter {
    name   = "Service"
    values = ["Amazon Cognito"]
  }

  dynamic "notification" {
    for_each = toset([50, 80, 100])

    content {
      comparison_operator        = "GREATER_THAN"
      threshold                  = notification.value
      threshold_type             = "PERCENTAGE"
      notification_type          = "FORECASTED"
      subscriber_email_addresses = tolist(var.cognito_budget_subscriber_emails)
    }
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
