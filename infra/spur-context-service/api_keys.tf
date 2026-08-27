# Personal API keys are an additive, disabled-by-default ingress. Every
# resource in this file is gated so existing IAM, demo, OAuth, M2M, and queue
# deployments are unchanged until an operator explicitly enables the feature.

locals {
  api_key_supported_user_count           = 50000
  api_key_steady_state_expiries_per_hour = ceil(local.api_key_supported_user_count * var.api_key_max_active_per_user / (var.api_key_default_ttl_days * 24))
  api_key_cleanup_invocations_per_hour   = floor(60 / var.api_key_cleanup_schedule_minutes)
  api_key_cleanup_records_per_invocation = min(var.api_key_cleanup_max_records, var.api_key_cleanup_page_limit)
  api_key_cleanup_capacity_per_hour      = local.api_key_cleanup_invocations_per_hour * local.api_key_cleanup_records_per_invocation
}

resource "aws_dynamodb_table" "api_keys" {
  count = var.api_key_auth_enabled ? 1 : 0

  name         = var.api_key_table_name
  billing_mode = "PAY_PER_REQUEST"
  hash_key     = "pk"

  attribute {
    name = "pk"
    type = "S"
  }

  attribute {
    name = "owner_gsi_pk"
    type = "S"
  }

  attribute {
    name = "owner_gsi_sk"
    type = "S"
  }

  attribute {
    name = "expiry_gsi_pk"
    type = "S"
  }

  attribute {
    name = "expiry_gsi_sk"
    type = "S"
  }

  global_secondary_index {
    name            = var.api_key_owner_gsi_name
    hash_key        = "owner_gsi_pk"
    range_key       = "owner_gsi_sk"
    projection_type = "ALL"
  }

  # Only active key records carry expiry_gsi_* attributes, making this a
  # sparse index over work still eligible for the bounded expiry sweeper.
  global_secondary_index {
    name            = var.api_key_expiry_gsi_name
    hash_key        = "expiry_gsi_pk"
    range_key       = "expiry_gsi_sk"
    projection_type = "ALL"
  }

  point_in_time_recovery {
    enabled = true
  }

  server_side_encryption {
    enabled = true
  }

  # TTL performs delayed garbage collection only. The authorizer enforces
  # expires_at synchronously and cleanup reclaims the active-key counter.
  ttl {
    attribute_name = "ttl"
    enabled        = true
  }
}

resource "aws_cloudwatch_log_group" "api_key_authorizer" {
  count = var.api_key_auth_enabled ? 1 : 0

  name              = "/aws/lambda/spur-context-api-key-authorizer"
  retention_in_days = var.api_key_authorizer_log_retention_days
}

resource "aws_lambda_function" "api_key_authorizer" {
  count = var.api_key_auth_enabled ? 1 : 0

  function_name = "spur-context-api-key-authorizer"
  description   = "Lean request authorizer for SPUR personal API keys"
  filename      = var.api_key_authorizer_zip_path

  source_code_hash = filebase64sha256(var.api_key_authorizer_zip_path)
  runtime          = "provided.al2023"
  architectures    = ["arm64"]
  handler          = "bootstrap"
  publish          = true

  role        = aws_iam_role.api_key_authorizer[0].arn
  timeout     = var.api_key_authorizer_timeout_sec
  memory_size = var.api_key_authorizer_memory_mb

  environment {
    variables = {
      SPUR_API_KEY_AUTH_ENABLED   = "1"
      SPUR_API_KEY_ENVIRONMENT    = var.api_key_environment
      SPUR_CONTEXT_API_KEYS_TABLE = aws_dynamodb_table.api_keys[0].name
    }
  }

  depends_on = [
    aws_cloudwatch_log_group.api_key_authorizer,
    aws_iam_role_policy.api_key_authorizer,
  ]
}

resource "aws_lambda_alias" "api_key_authorizer" {
  count = var.api_key_auth_enabled ? 1 : 0

  name             = "live"
  description      = "Stable API Gateway target for the lean API-key authorizer"
  function_name    = aws_lambda_function.api_key_authorizer[0].function_name
  function_version = aws_lambda_function.api_key_authorizer[0].version
}

resource "aws_apigatewayv2_authorizer" "api_key" {
  count = var.api_key_auth_enabled ? 1 : 0

  api_id                            = aws_apigatewayv2_api.http.id
  authorizer_type                   = "REQUEST"
  authorizer_uri                    = aws_lambda_alias.api_key_authorizer[0].invoke_arn
  authorizer_payload_format_version = "2.0"
  authorizer_result_ttl_in_seconds  = var.api_key_authorizer_cache_seconds
  enable_simple_responses           = true
  # The AWS provider exposes this set in route-key-first canonical order.
  # The Rust authorizer accepts either provider/event ordering while requiring
  # exactly these two values.
  identity_sources = [
    "$context.routeKey",
    "$request.header.X-SPUR-API-Key",
  ]
  name = "spur-context-api-key"
}

# A route-specific integration provides defense-in-depth by removing the raw
# key before invoking the Code Lambda. Code trusts only the typed
# authorizer context and remains correct if a live POC finds removal unsupported.
resource "aws_apigatewayv2_integration" "api_key" {
  count = var.api_key_auth_enabled ? 1 : 0

  api_id                 = aws_apigatewayv2_api.http.id
  integration_type       = "AWS_PROXY"
  integration_method     = "POST"
  integration_uri        = aws_lambda_function.code.invoke_arn
  payload_format_version = "2.0"
  request_parameters = {
    "remove:header.X-SPUR-API-Key" = "''"
  }
}

resource "aws_apigatewayv2_route" "api_key_discovery" {
  count = var.api_key_auth_enabled ? 1 : 0

  api_id             = aws_apigatewayv2_api.http.id
  route_key          = "GET /.well-known/spur-context-service"
  target             = "integrations/${aws_apigatewayv2_integration.code.id}"
  authorization_type = "NONE"
}

resource "aws_apigatewayv2_route" "api_key_mcp" {
  count = var.api_key_auth_enabled ? 1 : 0

  api_id             = aws_apigatewayv2_api.http.id
  route_key          = "POST /mcp/api-key"
  target             = "integrations/${aws_apigatewayv2_integration.api_key[0].id}"
  authorization_type = "CUSTOM"
  authorizer_id      = aws_apigatewayv2_authorizer.api_key[0].id
}

resource "aws_apigatewayv2_route" "api_key_management" {
  for_each = var.api_key_auth_enabled ? toset([
    "POST /auth/api-keys",
    "GET /auth/api-keys",
    "DELETE /auth/api-keys/{key_id}",
  ]) : toset([])

  api_id               = aws_apigatewayv2_api.http.id
  route_key            = each.value
  target               = "integrations/${aws_apigatewayv2_integration.code.id}"
  authorization_type   = "JWT"
  authorizer_id        = aws_apigatewayv2_authorizer.cognito[0].id
  authorization_scopes = [local.api_key_management_scope]
}

resource "aws_lambda_permission" "api_key_authorizer" {
  count = var.api_key_auth_enabled ? 1 : 0

  statement_id  = "apigateway-api-key-authorizer"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.api_key_authorizer[0].function_name
  qualifier     = aws_lambda_alias.api_key_authorizer[0].name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_apigatewayv2_api.http.execution_arn}/authorizers/${aws_apigatewayv2_authorizer.api_key[0].id}"
}

resource "aws_cloudwatch_log_group" "api_key_cleanup" {
  count = var.api_key_auth_enabled ? 1 : 0

  name              = "/aws/lambda/spur-context-api-key-cleanup"
  retention_in_days = var.api_key_cleanup_log_retention_days
}

resource "aws_lambda_function" "api_key_cleanup" {
  count = var.api_key_auth_enabled ? 1 : 0

  function_name = "spur-context-api-key-cleanup"
  description   = "Bounded expiry-hour cleanup for SPUR personal API keys"
  filename      = var.api_key_cleanup_zip_path

  source_code_hash = filebase64sha256(var.api_key_cleanup_zip_path)
  runtime          = "provided.al2023"
  architectures    = ["arm64"]
  handler          = "bootstrap"

  role        = aws_iam_role.api_key_cleanup[0].arn
  timeout     = var.api_key_cleanup_timeout_sec
  memory_size = var.api_key_cleanup_memory_mb

  environment {
    variables = {
      SPUR_API_KEY_AUTH_ENABLED              = "1"
      SPUR_CONTEXT_API_KEYS_TABLE            = aws_dynamodb_table.api_keys[0].name
      SPUR_API_KEY_OWNER_GSI_NAME            = var.api_key_owner_gsi_name
      SPUR_API_KEY_EXPIRY_GSI_NAME           = var.api_key_expiry_gsi_name
      SPUR_API_KEY_CLEANUP_MAX_CATCHUP_HOURS = tostring(var.api_key_cleanup_max_catchup_hours)
      SPUR_API_KEY_CLEANUP_MAX_BUCKETS       = tostring(var.api_key_cleanup_max_buckets)
      SPUR_API_KEY_CLEANUP_MAX_PAGES         = tostring(var.api_key_cleanup_max_pages)
      SPUR_API_KEY_CLEANUP_MAX_RECORDS       = tostring(var.api_key_cleanup_max_records)
      SPUR_API_KEY_CLEANUP_PAGE_LIMIT        = tostring(var.api_key_cleanup_page_limit)
    }
  }

  lifecycle {
    precondition {
      condition     = local.api_key_cleanup_capacity_per_hour > local.api_key_steady_state_expiries_per_hour
      error_message = "API-key cleanup capacity must exceed the supported 50k-user steady-state expiry rate."
    }
  }

  depends_on = [
    aws_cloudwatch_log_group.api_key_cleanup,
    aws_iam_role_policy.api_key_cleanup,
  ]
}

resource "aws_cloudwatch_event_rule" "api_key_cleanup" {
  count = var.api_key_auth_enabled ? 1 : 0

  name                = "spur-context-api-key-cleanup"
  description         = "Short-cadence bounded cleanup of expired personal API keys"
  schedule_expression = "rate(${var.api_key_cleanup_schedule_minutes} minutes)"
}

resource "aws_cloudwatch_event_target" "api_key_cleanup" {
  count = var.api_key_auth_enabled ? 1 : 0

  rule      = aws_cloudwatch_event_rule.api_key_cleanup[0].name
  target_id = "spur-context-api-key-cleanup"
  arn       = aws_lambda_function.api_key_cleanup[0].arn
  input = jsonencode({
    source      = "aws.events"
    detail-type = "Scheduled Event"
    detail = {
      operation = "sweep_expired_api_keys"
    }
  })
}

resource "aws_lambda_permission" "api_key_cleanup" {
  count = var.api_key_auth_enabled ? 1 : 0

  statement_id  = "eventbridge-api-key-cleanup"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.api_key_cleanup[0].function_name
  principal     = "events.amazonaws.com"
  source_arn    = aws_cloudwatch_event_rule.api_key_cleanup[0].arn
}

resource "aws_cloudwatch_log_metric_filter" "api_key_route_5xx" {
  count = var.api_key_auth_enabled ? 1 : 0

  name           = "spur-context-api-key-route-5xx"
  log_group_name = aws_cloudwatch_log_group.oauth_api_access[0].name
  pattern        = "{ $.route_key = \"POST /mcp/api-key\" && $.status = 5* }"

  metric_transformation {
    name      = "ApiKeyRoute5xx"
    namespace = "SPUR/ContextServiceAuth"
    value     = "1"
  }
}

resource "aws_cloudwatch_metric_alarm" "api_key_route_5xx" {
  count = var.api_key_auth_enabled ? 1 : 0

  alarm_name          = "spur-context-api-key-route-5xx"
  alarm_description   = "POST /mcp/api-key returned one or more 5xx responses in five minutes."
  comparison_operator = "GreaterThanOrEqualToThreshold"
  evaluation_periods  = 1
  metric_name         = aws_cloudwatch_log_metric_filter.api_key_route_5xx[0].metric_transformation[0].name
  namespace           = "SPUR/ContextServiceAuth"
  period              = 300
  statistic           = "Sum"
  threshold           = 1
  treat_missing_data  = "notBreaching"
  alarm_actions       = tolist(var.api_key_alarm_action_arns)
}

resource "aws_cloudwatch_metric_alarm" "api_key_authorizer_errors" {
  count = var.api_key_auth_enabled ? 1 : 0

  alarm_name          = "spur-context-api-key-authorizer-errors"
  alarm_description   = "The personal API-key authorizer returned a Lambda error."
  comparison_operator = "GreaterThanOrEqualToThreshold"
  evaluation_periods  = 1
  metric_name         = "Errors"
  namespace           = "AWS/Lambda"
  period              = 300
  statistic           = "Sum"
  threshold           = 1
  treat_missing_data  = "notBreaching"
  alarm_actions       = tolist(var.api_key_alarm_action_arns)

  dimensions = {
    FunctionName = aws_lambda_function.api_key_authorizer[0].function_name
  }
}

resource "aws_cloudwatch_metric_alarm" "api_key_cleanup_errors" {
  count = var.api_key_auth_enabled ? 1 : 0

  alarm_name          = "spur-context-api-key-cleanup-errors"
  alarm_description   = "The personal API-key expiry cleanup Lambda returned an error."
  comparison_operator = "GreaterThanOrEqualToThreshold"
  evaluation_periods  = 1
  metric_name         = "Errors"
  namespace           = "AWS/Lambda"
  period              = var.api_key_cleanup_schedule_minutes * 60
  statistic           = "Sum"
  threshold           = 1
  treat_missing_data  = "notBreaching"
  alarm_actions       = tolist(var.api_key_alarm_action_arns)

  dimensions = {
    FunctionName = aws_lambda_function.api_key_cleanup[0].function_name
  }
}

resource "aws_cloudwatch_metric_alarm" "api_key_cleanup_cursor_lag" {
  count = var.api_key_auth_enabled ? 1 : 0

  alarm_name          = "spur-context-api-key-cleanup-cursor-lag"
  alarm_description   = "The persisted personal API-key expiry cleanup cursor is behind its hourly SLO."
  comparison_operator = "GreaterThanOrEqualToThreshold"
  evaluation_periods  = 1
  metric_name         = "ApiKeyCleanupCursorLagHours"
  namespace           = "SPUR/ContextServiceAuth"
  period              = var.api_key_cleanup_schedule_minutes * 60
  statistic           = "Maximum"
  threshold           = var.api_key_cleanup_cursor_lag_alarm_hours
  treat_missing_data  = "breaching"
  alarm_actions       = tolist(var.api_key_alarm_action_arns)
}
