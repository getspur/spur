# These runs are plan-only and use Terraform's mock provider. They must never
# apply resources or contact AWS; they prove the API-key feature gate and its
# infrastructure security boundaries.

mock_provider "aws" {}

# Keep policy documents plan-known so least-privilege Resource assertions do
# not require apply. These are synthetic values used only by the mock provider.
override_resource {
  target = aws_dynamodb_table.api_keys
  values = {
    arn = "arn:aws:dynamodb:us-east-1:111122223333:table/spur-context-api-keys"
  }
  override_during = plan
}

override_resource {
  target = aws_cloudwatch_log_group.api_key_authorizer
  values = {
    arn = "arn:aws:logs:us-east-1:111122223333:log-group:/aws/lambda/spur-context-api-key-authorizer"
  }
  override_during = plan
}

override_resource {
  target = aws_cloudwatch_log_group.api_key_cleanup
  values = {
    arn = "arn:aws:logs:us-east-1:111122223333:log-group:/aws/lambda/spur-context-api-key-cleanup"
  }
  override_during = plan
}

variables {
  lambda_zip_path                     = "index_build_asl.json"
  api_key_authorizer_zip_path         = "variables.tf"
  api_key_cleanup_zip_path            = "outputs.tf"
  worker_ecr_image                    = "111122223333.dkr.ecr.us-east-1.amazonaws.com/spur-context-worker:test"
  worker_lambda_image                 = "111122223333.dkr.ecr.us-east-1.amazonaws.com/spur-context-worker-lambda:test"
  source_fetcher_lambda_image         = "111122223333.dkr.ecr.us-east-1.amazonaws.com/spur-context-source-fetcher:test"
  vpc_id                              = "vpc-0123456789abcdef0"
  worker_subnets                      = ["subnet-0123456789abcdef0"]
  worker_route_table_ids              = ["rtb-0123456789abcdef0"]
  create_vpc_endpoints                = false
  interface_vpc_endpoint_service_keys = []
}

run "disabled_default_has_no_api_key_infrastructure_or_endpoints" {
  command = plan

  assert {
    condition = (
      length(aws_dynamodb_table.api_keys) == 0 &&
      length(aws_apigatewayv2_authorizer.api_key) == 0 &&
      length(aws_apigatewayv2_integration.api_key) == 0 &&
      length(aws_apigatewayv2_route.api_key_mcp) == 0 &&
      length(aws_apigatewayv2_route.api_key_discovery) == 0 &&
      length(aws_apigatewayv2_route.api_key_management) == 0
    )
    error_message = "disabled mode must not create an API-key table, authorizer, integration, or exact routes"
  }

  assert {
    condition = (
      length(aws_lambda_function.api_key_authorizer) == 0 &&
      length(aws_lambda_alias.api_key_authorizer) == 0 &&
      length(aws_lambda_function.api_key_cleanup) == 0 &&
      length(aws_cloudwatch_event_rule.api_key_cleanup) == 0 &&
      length(aws_cloudwatch_event_target.api_key_cleanup) == 0 &&
      length(aws_lambda_permission.api_key_authorizer) == 0 &&
      length(aws_lambda_permission.api_key_cleanup) == 0
    )
    error_message = "disabled mode must not create API-key functions, aliases, cleanup schedules, targets, or permissions"
  }

  assert {
    condition = (
      length(aws_iam_role.api_key_authorizer) == 0 &&
      length(aws_iam_role.api_key_cleanup) == 0 &&
      length(aws_iam_role_policy.api_key_authorizer) == 0 &&
      length(aws_iam_role_policy.api_key_management) == 0 &&
      length(aws_iam_role_policy.api_key_cleanup) == 0 &&
      length(aws_cloudwatch_log_group.api_key_authorizer) == 0 &&
      length(aws_cloudwatch_log_group.api_key_cleanup) == 0 &&
      length(aws_cloudwatch_log_metric_filter.api_key_route_5xx) == 0 &&
      length(aws_cloudwatch_metric_alarm.api_key_route_5xx) == 0 &&
      length(aws_cloudwatch_metric_alarm.api_key_authorizer_errors) == 0 &&
      length(aws_cloudwatch_metric_alarm.api_key_cleanup_errors) == 0 &&
      length(aws_cloudwatch_metric_alarm.api_key_cleanup_cursor_lag) == 0
    )
    error_message = "disabled mode must not create API-key IAM, logs, metric filters, or alarms"
  }

  assert {
    condition = (
      output.api_key_auth_enabled == false &&
      output.api_key_table_name == null &&
      output.api_key_mcp_url == null &&
      output.api_key_management_url == null &&
      output.api_key_authorizer_function_name == null
    )
    error_message = "disabled mode must expose only a false feature status and null API-key discovery outputs"
  }

  assert {
    condition     = aws_apigatewayv2_route.default.authorization_type == "AWS_IAM" && length(aws_apigatewayv2_route.oauth) == 0
    error_message = "disabled API-key mode must preserve the legacy IAM default and absent OAuth route"
  }
}

run "api_keys_require_cognito" {
  command = plan

  variables {
    api_key_auth_enabled = true
  }

  expect_failures = [var.api_key_auth_enabled]
}

run "api_keys_require_the_registered_cli_callback" {
  command = plan

  variables {
    api_key_auth_enabled        = true
    cognito_auth_enabled        = true
    cognito_user_pool_name      = "spur-context-callback-test"
    cognito_domain_prefix       = "spur-context-callback-test"
    cognito_human_callback_urls = ["https://context.example.test/oauth/callback"]
    cognito_human_logout_urls   = ["https://context.example.test/logout"]
  }

  expect_failures = [var.api_key_auth_enabled]
}

run "cleanup_page_limit_cannot_exceed_backend_bound" {
  command = plan

  variables {
    api_key_cleanup_page_limit = 101
  }

  expect_failures = [var.api_key_cleanup_page_limit]
}

run "cleanup_invocation_and_schedule_bounds_are_enforced" {
  command = plan

  variables {
    api_key_cleanup_schedule_minutes = 16
    api_key_cleanup_max_buckets      = 9
    api_key_cleanup_max_records      = 101
  }

  expect_failures = [
    var.api_key_cleanup_schedule_minutes,
    var.api_key_cleanup_max_buckets,
    var.api_key_cleanup_max_records,
  ]
}

run "cleanup_page_budget_has_a_finite_upper_bound" {
  command = plan

  variables {
    api_key_cleanup_max_pages = 17
  }

  expect_failures = [var.api_key_cleanup_max_pages]
}

run "cleanup_page_budget_preserves_late_index_overlap" {
  command = plan

  variables {
    api_key_cleanup_max_buckets = 4
    api_key_cleanup_max_pages   = 5
  }

  expect_failures = [var.api_key_cleanup_max_pages]
}

run "enabled_api_keys_create_exact_isolated_contract" {
  command = plan

  variables {
    api_key_auth_enabled        = true
    cognito_auth_enabled        = true
    cognito_user_pool_name      = "spur-context-api-key-test"
    cognito_domain_prefix       = "spur-context-api-key-test"
    cognito_human_callback_urls = ["http://127.0.0.1:8765/callback"]
    cognito_human_logout_urls   = ["https://context.example.test/logout"]
    cognito_m2m_organizations = {
      compatibility = {
        display_name       = "M2M compatibility fixture"
        enabled            = true
        allowed_scopes     = ["external.read"]
        access_token_hours = 6
        risk_acceptance    = null
      }
    }
  }

  assert {
    condition = (
      contains(aws_cognito_user_pool_client.human[0].allowed_oauth_scopes, "urn:spur:context-service/keys.manage") &&
      !contains(aws_cognito_user_pool_client.m2m["compatibility"].allowed_oauth_scopes, "urn:spur:context-service/keys.manage") &&
      toset(aws_apigatewayv2_route.oauth[0].authorization_scopes) == toset([
        "urn:spur:context-service/external.read",
        "urn:spur:context-service/external.index",
        "urn:spur:context-service/external.status",
      ])
    )
    error_message = "keys.manage must belong only to the human client and must not broaden the existing OAuth route or M2M clients"
  }

  assert {
    condition = (
      aws_apigatewayv2_route.default.authorization_type == "AWS_IAM" &&
      aws_apigatewayv2_route.oauth[0].route_key == "POST /mcp/oauth" &&
      aws_apigatewayv2_route.oauth[0].authorization_type == "JWT" &&
      aws_apigatewayv2_route.api_key_discovery[0].route_key == "GET /.well-known/spur-context-service" &&
      aws_apigatewayv2_route.api_key_discovery[0].authorization_type == "NONE" &&
      aws_apigatewayv2_route.api_key_mcp[0].route_key == "POST /mcp/api-key" &&
      aws_apigatewayv2_route.api_key_mcp[0].authorization_type == "CUSTOM"
    )
    error_message = "API-key discovery and MCP routes must be exact and additive to unchanged default/OAuth routes"
  }

  assert {
    condition = (
      toset(keys(aws_apigatewayv2_route.api_key_management)) == toset([
        "POST /auth/api-keys",
        "GET /auth/api-keys",
        "DELETE /auth/api-keys/{key_id}",
      ]) &&
      alltrue([
        for route in values(aws_apigatewayv2_route.api_key_management) :
        route.authorization_type == "JWT" &&
        toset(route.authorization_scopes) == toset(["urn:spur:context-service/keys.manage"])
      ])
    )
    error_message = "all three exact management routes must require the Cognito keys.manage scope"
  }

  assert {
    condition = (
      aws_apigatewayv2_authorizer.api_key[0].authorizer_type == "REQUEST" &&
      aws_apigatewayv2_authorizer.api_key[0].authorizer_payload_format_version == "2.0" &&
      aws_apigatewayv2_authorizer.api_key[0].enable_simple_responses == true &&
      aws_apigatewayv2_authorizer.api_key[0].authorizer_result_ttl_in_seconds == 30 &&
      # hashicorp/aws exposes identity_sources as a canonicalized set. Assert
      # the provider-observed list instead of masking its order with toset.
      length(tolist(aws_apigatewayv2_authorizer.api_key[0].identity_sources)) == 2 &&
      tolist(aws_apigatewayv2_authorizer.api_key[0].identity_sources)[0] == "$context.routeKey" &&
      tolist(aws_apigatewayv2_authorizer.api_key[0].identity_sources)[1] == "$request.header.X-SPUR-API-Key"
    )
    error_message = "the request authorizer must use simple responses and the provider-observed route-first 30-second cache identity"
  }

  assert {
    condition = (
      aws_dynamodb_table.api_keys[0].billing_mode == "PAY_PER_REQUEST" &&
      aws_dynamodb_table.api_keys[0].hash_key == "pk" &&
      aws_dynamodb_table.api_keys[0].point_in_time_recovery[0].enabled == true &&
      aws_dynamodb_table.api_keys[0].server_side_encryption[0].enabled == true &&
      aws_dynamodb_table.api_keys[0].ttl[0].attribute_name == "ttl" &&
      aws_dynamodb_table.api_keys[0].ttl[0].enabled == true &&
      toset(aws_dynamodb_table.api_keys[0].global_secondary_index[*].name) == toset(["owner-gsi", "expiry-gsi"]) &&
      one([
        for index in aws_dynamodb_table.api_keys[0].global_secondary_index : index
        if index.name == "owner-gsi"
      ]).hash_key == "owner_gsi_pk" &&
      one([
        for index in aws_dynamodb_table.api_keys[0].global_secondary_index : index
        if index.name == "owner-gsi"
      ]).range_key == "owner_gsi_sk" &&
      one([
        for index in aws_dynamodb_table.api_keys[0].global_secondary_index : index
        if index.name == "expiry-gsi"
      ]).hash_key == "expiry_gsi_pk" &&
      one([
        for index in aws_dynamodb_table.api_keys[0].global_secondary_index : index
        if index.name == "expiry-gsi"
      ]).range_key == "expiry_gsi_sk"
    )
    error_message = "the separate on-demand table must enable encryption, PITR, TTL, and the owner/expiry GSIs"
  }

  assert {
    condition = (
      aws_apigatewayv2_integration.api_key[0].request_parameters["remove:header.X-SPUR-API-Key"] == "''" &&
      !strcontains(aws_apigatewayv2_stage.default.access_log_settings[0].format, "X-SPUR-API-Key") &&
      !strcontains(aws_apigatewayv2_stage.default.access_log_settings[0].format, "Authorization")
    )
    error_message = "the API-key integration must remove the credential header and access logs must omit credential headers"
  }

  assert {
    condition = (
      local.api_key_authorizer_dynamodb_actions == ["dynamodb:GetItem"] &&
      toset(local.api_key_management_dynamodb_actions) == toset([
        "dynamodb:GetItem",
        "dynamodb:PutItem",
        "dynamodb:UpdateItem",
        "dynamodb:TransactWriteItems",
      ]) &&
      local.api_key_management_query_actions == ["dynamodb:Query"] &&
      toset(local.api_key_cleanup_dynamodb_actions) == toset([
        "dynamodb:GetItem",
        "dynamodb:UpdateItem",
        "dynamodb:TransactWriteItems",
      ]) &&
      local.api_key_cleanup_query_actions == ["dynamodb:Query"] &&
      length(aws_iam_role_policy.api_key_authorizer) == 1 &&
      length(aws_iam_role_policy.api_key_management) == 1 &&
      length(aws_iam_role_policy.api_key_cleanup) == 1 &&
      jsondecode(aws_iam_role_policy.api_key_authorizer[0].policy).Statement[0].Resource == "arn:aws:dynamodb:us-east-1:111122223333:table/spur-context-api-keys" &&
      jsondecode(aws_iam_role_policy.api_key_management[0].policy).Statement[0].Resource == "arn:aws:dynamodb:us-east-1:111122223333:table/spur-context-api-keys" &&
      jsondecode(aws_iam_role_policy.api_key_management[0].policy).Statement[1].Resource == "arn:aws:dynamodb:us-east-1:111122223333:table/spur-context-api-keys/index/owner-gsi" &&
      jsondecode(aws_iam_role_policy.api_key_cleanup[0].policy).Statement[0].Resource == "arn:aws:dynamodb:us-east-1:111122223333:table/spur-context-api-keys" &&
      toset(jsondecode(aws_iam_role_policy.api_key_cleanup[0].policy).Statement[1].Resource) == toset([
        "arn:aws:dynamodb:us-east-1:111122223333:table/spur-context-api-keys/index/expiry-gsi",
        "arn:aws:dynamodb:us-east-1:111122223333:table/spur-context-api-keys/index/owner-gsi",
      ])
    )
    error_message = "authorizer, management, and cleanup IAM must retain their separate least-privilege action sets"
  }

  assert {
    condition = (
      aws_cloudwatch_event_rule.api_key_cleanup[0].schedule_expression == "rate(5 minutes)" &&
      aws_cloudwatch_event_target.api_key_cleanup[0].input != null &&
      aws_lambda_function.api_key_cleanup[0].environment[0].variables["SPUR_API_KEY_CLEANUP_MAX_CATCHUP_HOURS"] == "168" &&
      aws_lambda_function.api_key_cleanup[0].environment[0].variables["SPUR_API_KEY_CLEANUP_MAX_BUCKETS"] == "4" &&
      aws_lambda_function.api_key_cleanup[0].environment[0].variables["SPUR_API_KEY_CLEANUP_MAX_PAGES"] == "8" &&
      aws_lambda_function.api_key_cleanup[0].environment[0].variables["SPUR_API_KEY_CLEANUP_MAX_RECORDS"] == "100" &&
      aws_lambda_function.api_key_cleanup[0].environment[0].variables["SPUR_API_KEY_CLEANUP_PAGE_LIMIT"] == "100" &&
      var.api_key_cleanup_max_catchup_hours > var.api_key_cleanup_max_buckets &&
      floor(60 / var.api_key_cleanup_schedule_minutes) * var.api_key_cleanup_max_records > ceil(50000 * var.api_key_max_active_per_user / (var.api_key_default_ttl_days * 24)) &&
      length(aws_cloudwatch_log_group.api_key_authorizer) == 1 &&
      length(aws_cloudwatch_log_group.api_key_cleanup) == 1 &&
      length(aws_cloudwatch_metric_alarm.api_key_route_5xx) == 1 &&
      length(aws_cloudwatch_metric_alarm.api_key_authorizer_errors) == 1 &&
      length(aws_cloudwatch_metric_alarm.api_key_cleanup_errors) == 1 &&
      length(aws_cloudwatch_metric_alarm.api_key_cleanup_cursor_lag) == 1 &&
      aws_cloudwatch_metric_alarm.api_key_cleanup_errors[0].period == 300 &&
      aws_cloudwatch_metric_alarm.api_key_cleanup_cursor_lag[0].period == 300 &&
      aws_cloudwatch_metric_alarm.api_key_cleanup_cursor_lag[0].treat_missing_data == "breaching"
    )
    error_message = "enabled mode must provide bounded five-minute cleanup capacity above steady-state expiry plus dedicated lag/error alarms"
  }

  assert {
    condition = (
      aws_lambda_function.api_key_authorizer[0].filename == "variables.tf" &&
      aws_lambda_function.api_key_authorizer[0].function_name == "spur-context-api-key-authorizer" &&
      aws_iam_role.api_key_authorizer[0].name == "spur-context-api-key-authorizer" &&
      aws_lambda_function.api_key_cleanup[0].function_name == "spur-context-api-key-cleanup" &&
      aws_iam_role.api_key_cleanup[0].name == "spur-context-api-key-cleanup" &&
      aws_lambda_function.service.environment[0].variables["SPUR_API_KEY_AUTH_ENABLED"] == "1" &&
      aws_lambda_function.service.environment[0].variables["SPUR_CONTEXT_API_KEYS_TABLE"] == aws_dynamodb_table.api_keys[0].name
    )
    error_message = "the lean authorizer must use its own artifact/role while serving receives only non-secret API-key configuration"
  }

  assert {
    condition = (
      output.api_key_auth_enabled == true &&
      output.api_key_table_name == aws_dynamodb_table.api_keys[0].name &&
      output.api_key_authorizer_function_name == "spur-context-api-key-authorizer"
    )
    error_message = "enabled outputs must be useful discovery metadata without credentials or hashes"
  }

  assert {
    condition = (
      aws_cloudwatch_event_target.index_queue_drainer.input == jsonencode({
        source      = "aws.events"
        detail-type = "Scheduled Event"
        detail = {
          operation = "drain_queued_jobs"
        }
      }) &&
      aws_iam_role_policy.lambda_dynamodb.name == "DynamoDbControlPlaneAccess"
    )
    error_message = "API-key provisioning must not change EventBridge drainer input or fold key access into the legacy Lambda DynamoDB policy"
  }
}
