# Personal API keys are independently disabled by default inside the already
# disposable POC. This root accepts only synthetic `spur_test_...` fixtures.

resource "aws_dynamodb_table" "api_keys" {
  count = local.api_key_poc_enabled ? 1 : 0

  name         = "${local.name_prefix}-api-keys"
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
    name            = "owner-gsi"
    hash_key        = "owner_gsi_pk"
    range_key       = "owner_gsi_sk"
    projection_type = "ALL"
  }

  global_secondary_index {
    name            = "expiry-gsi"
    hash_key        = "expiry_gsi_pk"
    range_key       = "expiry_gsi_sk"
    projection_type = "ALL"
  }

  point_in_time_recovery {
    enabled = false
  }

  server_side_encryption {
    enabled = true
  }

  ttl {
    attribute_name = "ttl"
    enabled        = true
  }
}

resource "aws_cloudwatch_log_group" "api_key_authorizer" {
  count = local.api_key_poc_enabled ? 1 : 0

  name              = "/aws/lambda/${local.name_prefix}-api-key-authorizer"
  retention_in_days = 1
}

resource "aws_lambda_function" "api_key_authorizer" {
  count = local.api_key_poc_enabled ? 1 : 0

  function_name = "${local.name_prefix}-api-key-authorizer"
  description   = "Disposable request authorizer accepting synthetic POC keys only"
  filename      = var.api_key_authorizer_zip_path
  role          = aws_iam_role.api_key_authorizer[0].arn
  runtime       = "provided.al2023"
  architectures = ["arm64"]
  handler       = "bootstrap"
  memory_size   = 128
  timeout       = 5
  publish       = true

  environment {
    variables = {
      SPUR_API_KEY_AUTH_ENABLED   = "1"
      SPUR_API_KEY_ENVIRONMENT    = "test"
      SPUR_CONTEXT_API_KEYS_TABLE = aws_dynamodb_table.api_keys[0].name
    }
  }

  depends_on = [
    aws_cloudwatch_log_group.api_key_authorizer,
    aws_iam_role_policy.api_key_authorizer,
  ]
}

resource "aws_lambda_alias" "api_key_authorizer" {
  count = local.api_key_poc_enabled ? 1 : 0

  name             = "poc"
  function_name    = aws_lambda_function.api_key_authorizer[0].function_name
  function_version = aws_lambda_function.api_key_authorizer[0].version
}

resource "aws_apigatewayv2_authorizer" "api_key" {
  count = local.api_key_poc_enabled ? 1 : 0

  api_id                            = aws_apigatewayv2_api.poc[0].id
  authorizer_type                   = "REQUEST"
  authorizer_uri                    = "arn:aws:apigateway:${var.aws_region}:lambda:path/2015-03-31/functions/${aws_lambda_alias.api_key_authorizer[0].invoke_arn}/invocations"
  authorizer_payload_format_version = "2.0"
  authorizer_result_ttl_in_seconds  = 30
  enable_simple_responses           = true
  identity_sources = [
    "$context.routeKey",
    "$request.header.X-SPUR-API-Key",
  ]
  name = "${local.name_prefix}-api-key"
}

resource "aws_apigatewayv2_integration" "api_key" {
  count = local.api_key_poc_enabled ? 1 : 0

  api_id                 = aws_apigatewayv2_api.poc[0].id
  integration_type       = "AWS_PROXY"
  integration_method     = "POST"
  integration_uri        = aws_lambda_alias.poc[0].invoke_arn
  payload_format_version = "2.0"
  timeout_milliseconds   = 5000
  request_parameters = {
    "remove:header.X-SPUR-API-Key" = "''"
  }
}

resource "aws_apigatewayv2_route" "api_key_discovery" {
  count = local.api_key_poc_enabled ? 1 : 0

  api_id             = aws_apigatewayv2_api.poc[0].id
  route_key          = "GET /.well-known/spur-context-service"
  target             = "integrations/${aws_apigatewayv2_integration.lambda[0].id}"
  authorization_type = "NONE"
}

resource "aws_apigatewayv2_route" "api_key_mcp" {
  count = local.api_key_poc_enabled ? 1 : 0

  api_id             = aws_apigatewayv2_api.poc[0].id
  route_key          = "POST /mcp/api-key"
  target             = "integrations/${aws_apigatewayv2_integration.api_key[0].id}"
  authorization_type = "CUSTOM"
  authorizer_id      = aws_apigatewayv2_authorizer.api_key[0].id
}

resource "aws_apigatewayv2_route" "api_key_management" {
  for_each = local.api_key_poc_enabled ? toset([
    "POST /auth/api-keys",
    "GET /auth/api-keys",
    "DELETE /auth/api-keys/{key_id}",
  ]) : toset([])

  api_id               = aws_apigatewayv2_api.poc[0].id
  route_key            = each.value
  target               = "integrations/${aws_apigatewayv2_integration.lambda[0].id}"
  authorization_type   = "JWT"
  authorizer_id        = aws_apigatewayv2_authorizer.cognito[0].id
  authorization_scopes = [local.api_key_management_scope]
}

resource "aws_lambda_permission" "api_key_authorizer" {
  count = local.api_key_poc_enabled ? 1 : 0

  statement_id  = "AllowDedicatedPocApiKeyAuthorizer"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.api_key_authorizer[0].function_name
  qualifier     = aws_lambda_alias.api_key_authorizer[0].name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_apigatewayv2_api.poc[0].execution_arn}/authorizers/${aws_apigatewayv2_authorizer.api_key[0].id}"
}

resource "aws_cloudwatch_log_group" "api_key_cleanup" {
  count = local.api_key_poc_enabled ? 1 : 0

  name              = "/aws/lambda/${local.name_prefix}-api-key-cleanup"
  retention_in_days = 1
}

resource "aws_lambda_function" "api_key_cleanup" {
  count = local.api_key_poc_enabled ? 1 : 0

  function_name = "${local.name_prefix}-api-key-cleanup"
  description   = "Disposable bounded cleanup for synthetic POC keys"
  filename      = var.api_key_cleanup_zip_path
  role          = aws_iam_role.api_key_cleanup[0].arn
  runtime       = "provided.al2023"
  architectures = ["arm64"]
  handler       = "bootstrap"
  memory_size   = 128
  timeout       = 60

  environment {
    variables = {
      SPUR_API_KEY_AUTH_ENABLED              = "1"
      SPUR_CONTEXT_API_KEYS_TABLE            = aws_dynamodb_table.api_keys[0].name
      SPUR_API_KEY_OWNER_GSI_NAME            = "owner-gsi"
      SPUR_API_KEY_EXPIRY_GSI_NAME           = "expiry-gsi"
      SPUR_API_KEY_CLEANUP_MAX_CATCHUP_HOURS = "168"
      SPUR_API_KEY_CLEANUP_MAX_BUCKETS       = tostring(local.api_key_cleanup_max_buckets)
      SPUR_API_KEY_CLEANUP_MAX_PAGES         = tostring(local.api_key_cleanup_max_pages)
      SPUR_API_KEY_CLEANUP_MAX_RECORDS       = tostring(local.api_key_cleanup_max_records)
      SPUR_API_KEY_CLEANUP_PAGE_LIMIT        = tostring(local.api_key_cleanup_page_limit)
    }
  }

  lifecycle {
    precondition {
      condition     = local.api_key_cleanup_capacity_per_hour > local.api_key_steady_state_expiries_per_hour
      error_message = "POC cleanup capacity must exceed the supported steady-state expiry rate."
    }
  }

  depends_on = [
    aws_cloudwatch_log_group.api_key_cleanup,
    aws_iam_role_policy.api_key_cleanup,
  ]
}

resource "aws_cloudwatch_event_rule" "api_key_cleanup" {
  count = local.api_key_poc_enabled ? 1 : 0

  name                = "${local.name_prefix}-api-key-cleanup"
  schedule_expression = "rate(${local.api_key_cleanup_schedule_minutes} minutes)"
}

resource "aws_cloudwatch_event_target" "api_key_cleanup" {
  count = local.api_key_poc_enabled ? 1 : 0

  rule      = aws_cloudwatch_event_rule.api_key_cleanup[0].name
  target_id = "api-key-cleanup"
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
  count = local.api_key_poc_enabled ? 1 : 0

  statement_id  = "AllowDedicatedPocApiKeyCleanup"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.api_key_cleanup[0].function_name
  principal     = "events.amazonaws.com"
  source_arn    = aws_cloudwatch_event_rule.api_key_cleanup[0].arn
}
