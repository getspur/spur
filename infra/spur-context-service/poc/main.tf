resource "aws_cognito_user_pool" "poc" {
  count = var.poc_enabled ? 1 : 0

  name                = local.name_prefix
  user_pool_tier      = "LITE"
  deletion_protection = "INACTIVE"
  username_attributes = ["email"]
  mfa_configuration   = "OPTIONAL"

  software_token_mfa_configuration {
    enabled = true
  }

  lifecycle {
    precondition {
      condition = (
        var.poc_suffix != "replace-me" &&
        var.poc_owner != "replace-with-owner" &&
        var.cost_center != "replace-with-cost-center" &&
        var.creation_confirmation == "I_UNDERSTAND_THIS_CREATES_DISPOSABLE_POC_RESOURCES"
      )
      error_message = "Enabled POC plans require unique names/tags and the explicit disposable-resource confirmation."
    }
  }
}

resource "aws_cognito_user_pool_domain" "poc" {
  count = var.poc_enabled ? 1 : 0

  domain       = local.name_prefix
  user_pool_id = aws_cognito_user_pool.poc[0].id
}

resource "aws_cognito_resource_server" "poc" {
  count = var.poc_enabled ? 1 : 0

  identifier   = local.resource_server_identifier
  name         = local.name_prefix
  user_pool_id = aws_cognito_user_pool.poc[0].id

  dynamic "scope" {
    for_each = local.custom_scope_descriptions

    content {
      scope_name        = scope.key
      scope_description = scope.value
    }
  }
}

resource "aws_cognito_user_pool_client" "human" {
  count = var.poc_enabled ? 1 : 0

  name                                 = "${local.name_prefix}-human"
  user_pool_id                         = aws_cognito_user_pool.poc[0].id
  depends_on                           = [aws_cognito_resource_server.poc]
  generate_secret                      = false
  allowed_oauth_flows_user_pool_client = true
  allowed_oauth_flows                  = ["code"]
  allowed_oauth_scopes                 = local.human_scopes
  callback_urls                        = sort(tolist(var.human_callback_urls))
  logout_urls                          = sort(tolist(var.human_logout_urls))
  supported_identity_providers         = ["COGNITO"]
  enable_token_revocation              = true
  prevent_user_existence_errors        = "ENABLED"

  access_token_validity  = 60
  id_token_validity      = 60
  refresh_token_validity = 1

  token_validity_units {
    access_token  = "minutes"
    id_token      = "minutes"
    refresh_token = "days"
  }
}

resource "aws_cognito_user_pool_client" "m2m" {
  for_each = var.poc_enabled ? local.m2m_clients : {}

  name                                 = "${local.name_prefix}-${each.key}"
  user_pool_id                         = aws_cognito_user_pool.poc[0].id
  depends_on                           = [aws_cognito_resource_server.poc]
  generate_secret                      = true
  allowed_oauth_flows_user_pool_client = true
  allowed_oauth_flows                  = ["client_credentials"]
  allowed_oauth_scopes = [
    for suffix in each.value.scopes :
    "${local.resource_server_identifier}/${suffix}"
  ]
  access_token_validity = 1

  token_validity_units {
    access_token = "hours"
  }
}

resource "aws_dynamodb_table" "index_jobs" {
  count = var.poc_enabled ? 1 : 0

  name                        = "${local.name_prefix}-jobs"
  billing_mode                = "PAY_PER_REQUEST"
  hash_key                    = "pk"
  deletion_protection_enabled = false

  attribute {
    name = "pk"
    type = "S"
  }

  ttl {
    attribute_name = "expires_at"
    enabled        = true
  }

  point_in_time_recovery {
    enabled = false
  }
}

resource "aws_cloudwatch_log_group" "lambda" {
  count = var.poc_enabled ? 1 : 0

  name              = "/aws/lambda/${local.name_prefix}"
  retention_in_days = 1
}

resource "aws_lambda_function" "validation" {
  count = var.poc_enabled ? 1 : 0

  function_name = local.name_prefix
  description   = "Disposable validation-only Cognito authentication POC"
  filename      = var.lambda_zip_path
  role          = aws_iam_role.lambda[0].arn
  runtime       = "provided.al2023"
  architectures = ["arm64"]
  handler       = "bootstrap"
  memory_size   = 512
  timeout       = 15
  publish       = true

  environment {
    variables = {
      SPUR_COGNITO_AUTH_ENABLED                 = "1"
      SPUR_COGNITO_ISSUER                       = local.cognito_issuer
      SPUR_COGNITO_HUMAN_CLIENT_ID              = aws_cognito_user_pool_client.human[0].id
      SPUR_COGNITO_M2M_CLIENT_IDS               = join(",", [for client in values(aws_cognito_user_pool_client.m2m) : client.id])
      SPUR_COGNITO_RESOURCE_SERVER_ID           = local.resource_server_identifier
      SPUR_COGNITO_DENY_CLIENT_IDS              = join(",", var.emergency_deny_client_ids)
      SPUR_COGNITO_OAUTH_PATH                   = "/mcp/oauth"
      SPUR_COGNITO_AUTHORIZATION_ENDPOINT       = "https://${aws_cognito_user_pool_domain.poc[0].domain}.auth.${var.aws_region}.amazoncognito.com/oauth2/authorize"
      SPUR_COGNITO_TOKEN_ENDPOINT               = "https://${aws_cognito_user_pool_domain.poc[0].domain}.auth.${var.aws_region}.amazoncognito.com/oauth2/token"
      SPUR_CONTEXT_SERVICE_BASE_URL             = aws_apigatewayv2_api.poc[0].api_endpoint
      SPUR_API_KEY_AUTH_ENABLED                 = local.api_key_poc_enabled ? "1" : "0"
      SPUR_API_KEY_ENVIRONMENT                  = "test"
      SPUR_API_KEY_DEFAULT_TTL_DAYS             = tostring(local.api_key_default_ttl_days)
      SPUR_API_KEY_MAX_TTL_DAYS                 = "365"
      SPUR_CONTEXT_API_KEYS_TABLE               = local.api_key_poc_enabled ? aws_dynamodb_table.api_keys[0].name : ""
      SPUR_CATALOG_DSN                          = "ducklake:sqlite:/tmp/${local.name_prefix}.ducklake"
      SPUR_CONTEXT_ALLOW_ANONYMOUS_MUTATIONS    = var.allow_anonymous_mutations ? "1" : "0"
      SPUR_CONTEXT_ALLOWED_SOURCE_DOMAINS       = "poc-no-source.invalid"
      SPUR_INDEX_JOBS_TABLE                     = aws_dynamodb_table.index_jobs[0].name
      SPUR_INDEX_STATE_MACHINE_ARN              = ""
      SPUR_INDEX_MAX_RUNNING_JOBS_PER_OWNER     = "0"
      SPUR_INDEX_MAX_QUEUED_JOBS_PER_OWNER      = "0"
      SPUR_INDEX_MAX_RUNNING_JOBS_GLOBAL        = tostring(local.index_max_running_jobs_global)
      SPUR_INDEX_MAX_QUEUED_JOBS_GLOBAL         = tostring(local.index_max_queued_jobs_global)
      SPUR_INDEX_MAX_CONCURRENT_JOBS_PER_CALLER = "0"
      SPUR_INDEX_RATE_LIMIT_PER_MINUTE          = "1"
      SPUR_INDEX_QUEUE_SHARD_COUNT              = "1"
      SPUR_INDEX_DRAINER_BATCH_LIMIT            = "1"
      SPUR_INDEX_DRAINER_SCAN_LIMIT_PER_SHARD   = "1"
      SPUR_INDEX_DRAINER_SCHEDULE_RATE_MINUTES  = "60"
      SPUR_INDEX_DISPATCH_MAX_ATTEMPTS          = "1"
      SPUR_INDEX_DISPATCH_BACKOFF_BASE_SECONDS  = "1"
      SPUR_CONTEXT_MAX_TARBALL_BYTES            = "1"
      SPUR_CONTEXT_MAX_GIT_BYTES                = "1"
      SPUR_CONTEXT_MAX_BUILD_SECONDS            = "1"
    }
  }

  depends_on = [
    aws_cloudwatch_log_group.lambda,
    aws_iam_role_policy.lambda,
    aws_iam_role_policy.api_key_management,
  ]
}

resource "aws_lambda_alias" "poc" {
  count = var.poc_enabled ? 1 : 0

  name             = "poc"
  description      = "Disposable POC version only"
  function_name    = aws_lambda_function.validation[0].function_name
  function_version = aws_lambda_function.validation[0].version
}

resource "aws_apigatewayv2_api" "poc" {
  count = var.poc_enabled ? 1 : 0

  name          = local.name_prefix
  protocol_type = "HTTP"
}

resource "aws_apigatewayv2_integration" "lambda" {
  count = var.poc_enabled ? 1 : 0

  api_id                 = aws_apigatewayv2_api.poc[0].id
  integration_type       = "AWS_PROXY"
  integration_method     = "POST"
  integration_uri        = aws_lambda_alias.poc[0].invoke_arn
  payload_format_version = "2.0"
  timeout_milliseconds   = 5000
}

resource "aws_apigatewayv2_authorizer" "cognito" {
  count = var.poc_enabled ? 1 : 0

  api_id           = aws_apigatewayv2_api.poc[0].id
  authorizer_type  = "JWT"
  identity_sources = ["$request.header.Authorization"]
  name             = "${local.name_prefix}-cognito"

  jwt_configuration {
    audience = concat(
      [aws_cognito_user_pool_client.human[0].id],
      [for client in values(aws_cognito_user_pool_client.m2m) : client.id],
    )
    issuer = local.cognito_issuer
  }
}

resource "aws_apigatewayv2_route" "oauth" {
  count = var.poc_enabled ? 1 : 0

  api_id               = aws_apigatewayv2_api.poc[0].id
  route_key            = "POST /mcp/oauth"
  target               = "integrations/${aws_apigatewayv2_integration.lambda[0].id}"
  authorization_type   = "JWT"
  authorizer_id        = aws_apigatewayv2_authorizer.cognito[0].id
  authorization_scopes = local.external_scopes
}

resource "aws_apigatewayv2_route" "legacy" {
  count = var.poc_enabled ? 1 : 0

  api_id             = aws_apigatewayv2_api.poc[0].id
  route_key          = "$default"
  target             = "integrations/${aws_apigatewayv2_integration.lambda[0].id}"
  authorization_type = var.legacy_authorization_type
}

resource "aws_cloudwatch_log_group" "api" {
  count = var.poc_enabled ? 1 : 0

  name              = "/aws/apigateway/${local.name_prefix}"
  retention_in_days = 1
}

resource "aws_apigatewayv2_stage" "poc" {
  count = var.poc_enabled ? 1 : 0

  api_id      = aws_apigatewayv2_api.poc[0].id
  name        = "$default"
  auto_deploy = true

  default_route_settings {
    throttling_burst_limit = 2
    throttling_rate_limit  = 1
  }

  access_log_settings {
    destination_arn = aws_cloudwatch_log_group.api[0].arn
    format = jsonencode({
      request_id         = "$context.requestId"
      route_key          = "$context.routeKey"
      status             = "$context.status"
      integration_status = "$context.integrationStatus"
      response_latency   = "$context.responseLatency"
      error_message      = "$context.error.message"
    })
  }
}

resource "aws_lambda_permission" "api" {
  count = var.poc_enabled ? 1 : 0

  statement_id  = "AllowDedicatedPocApi"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.validation[0].function_name
  qualifier     = aws_lambda_alias.poc[0].name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_apigatewayv2_api.poc[0].execution_arn}/*/*"
}
