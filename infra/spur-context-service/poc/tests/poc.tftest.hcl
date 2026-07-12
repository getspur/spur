# Terraform test uses a mock provider and command=plan only. It never contacts
# AWS and cannot apply resources.
mock_provider "aws" {}

override_resource {
  target = aws_dynamodb_table.api_keys
  values = {
    arn = "arn:aws:dynamodb:us-east-1:111122223333:table/spur-context-auth-poc-unit-test-key-api-keys"
  }
  override_during = plan
}

run "disabled_default_plans_no_poc_resources" {
  command = plan

  assert {
    condition = (
      length(aws_cognito_user_pool.poc) == 0 &&
      length(aws_apigatewayv2_api.poc) == 0 &&
      length(aws_lambda_function.validation) == 0 &&
      length(aws_dynamodb_table.index_jobs) == 0 &&
      length(aws_dynamodb_table.api_keys) == 0 &&
      length(aws_lambda_function.api_key_authorizer) == 0 &&
      length(aws_lambda_function.api_key_cleanup) == 0 &&
      length(aws_apigatewayv2_route.api_key_mcp) == 0 &&
      length(aws_apigatewayv2_route.api_key_management) == 0 &&
      length(aws_cloudwatch_event_rule.api_key_cleanup) == 0
    )
    error_message = "the default safety gate must plan no POC resources"
  }
}

run "api_key_feature_is_independently_disabled" {
  command = plan

  variables {
    poc_enabled           = true
    creation_confirmation = "I_UNDERSTAND_THIS_CREATES_DISPOSABLE_POC_RESOURCES"
    poc_suffix            = "unit-test-no-key"
    poc_owner             = "fixture-owner"
    cost_center           = "fixture-cost"
  }

  assert {
    condition = (
      length(aws_dynamodb_table.api_keys) == 0 &&
      length(aws_lambda_function.api_key_authorizer) == 0 &&
      length(aws_lambda_function.api_key_cleanup) == 0 &&
      length(aws_apigatewayv2_route.api_key_mcp) == 0 &&
      length(aws_apigatewayv2_route.api_key_management) == 0 &&
      aws_apigatewayv2_route.oauth[0].route_key == "POST /mcp/oauth" &&
      aws_apigatewayv2_route.legacy[0].authorization_type == "AWS_IAM"
    )
    error_message = "API-key resources must be independently disabled without changing OAuth or IAM"
  }
}

run "api_key_feature_requires_the_registered_cli_callback" {
  command = plan

  variables {
    poc_enabled           = true
    api_key_auth_enabled  = true
    human_callback_urls   = ["http://127.0.0.1:9999/callback"]
    creation_confirmation = "I_UNDERSTAND_THIS_CREATES_DISPOSABLE_POC_RESOURCES"
    poc_suffix            = "unit-test-wrong-port"
    poc_owner             = "fixture-owner"
    cost_center           = "fixture-cost"
  }

  expect_failures = [var.human_callback_urls]
}

run "mock_enabled_api_key_plan_is_exact_bounded_and_isolated" {
  command = plan

  variables {
    poc_enabled                 = true
    api_key_auth_enabled        = true
    creation_confirmation       = "I_UNDERSTAND_THIS_CREATES_DISPOSABLE_POC_RESOURCES"
    poc_suffix                  = "unit-test-key"
    poc_owner                   = "fixture-owner"
    cost_center                 = "fixture-cost"
    api_key_authorizer_zip_path = "./artifacts/synthetic-authorizer.zip"
    api_key_cleanup_zip_path    = "./artifacts/synthetic-cleanup.zip"
  }

  assert {
    condition = (
      toset(aws_cognito_user_pool_client.human[0].callback_urls) == toset(["http://127.0.0.1:8765/callback"]) &&
      aws_apigatewayv2_route.api_key_discovery[0].route_key == "GET /.well-known/spur-context-service" &&
      aws_apigatewayv2_route.api_key_mcp[0].route_key == "POST /mcp/api-key" &&
      aws_apigatewayv2_route.api_key_mcp[0].authorization_type == "CUSTOM" &&
      toset(keys(aws_apigatewayv2_route.api_key_management)) == toset([
        "POST /auth/api-keys",
        "GET /auth/api-keys",
        "DELETE /auth/api-keys/{key_id}",
      ])
    )
    error_message = "mock-enabled mode must add only the exact discovery, API-key MCP, and management routes"
  }

  assert {
    condition = toset(jsondecode(aws_iam_role_policy.api_key_management[0].policy).Statement[0].Action) == toset([
      "dynamodb:GetItem",
      "dynamodb:PutItem",
      "dynamodb:UpdateItem",
      "dynamodb:TransactWriteItems",
    ])
    error_message = "POC API-key management transactions require their underlying PutItem and UpdateItem actions"
  }

  assert {
    condition = (
      aws_apigatewayv2_authorizer.api_key[0].authorizer_result_ttl_in_seconds == 30 &&
      toset(aws_apigatewayv2_authorizer.api_key[0].identity_sources) == toset([
        "$context.routeKey",
        "$request.header.X-SPUR-API-Key",
      ]) &&
      aws_apigatewayv2_integration.api_key[0].request_parameters["remove:header.X-SPUR-API-Key"] == "''" &&
      aws_lambda_function.api_key_authorizer[0].environment[0].variables["SPUR_API_KEY_ENVIRONMENT"] == "test"
    )
    error_message = "the POC authorizer must use synthetic test keys, route-aware 30-second caching, and header removal"
  }

  assert {
    condition = (
      aws_cloudwatch_event_rule.api_key_cleanup[0].schedule_expression == "rate(5 minutes)" &&
      jsondecode(aws_cloudwatch_event_target.api_key_cleanup[0].input).detail.operation == "sweep_expired_api_keys" &&
      aws_lambda_function.api_key_cleanup[0].environment[0].variables["SPUR_API_KEY_CLEANUP_MAX_BUCKETS"] == "4" &&
      aws_lambda_function.api_key_cleanup[0].environment[0].variables["SPUR_API_KEY_CLEANUP_MAX_PAGES"] == "8" &&
      aws_lambda_function.api_key_cleanup[0].environment[0].variables["SPUR_API_KEY_CLEANUP_MAX_RECORDS"] == "100"
    )
    error_message = "cleanup must retain the bounded five-minute 1,200-record/hour contract"
  }

  assert {
    condition = (
      aws_apigatewayv2_route.oauth[0].route_key == "POST /mcp/oauth" &&
      aws_apigatewayv2_route.legacy[0].authorization_type == "AWS_IAM" &&
      aws_lambda_function.validation[0].environment[0].variables["SPUR_INDEX_MAX_RUNNING_JOBS_GLOBAL"] == "0" &&
      aws_lambda_function.validation[0].environment[0].variables["SPUR_INDEX_MAX_QUEUED_JOBS_GLOBAL"] == "0"
    )
    error_message = "API-key POC mode must preserve OAuth/IAM and zero-dispatch isolation"
  }
}

run "enabled_fixture_is_isolated_and_dispatch_disabled" {
  command = plan

  variables {
    poc_enabled           = true
    creation_confirmation = "I_UNDERSTAND_THIS_CREATES_DISPOSABLE_POC_RESOURCES"
    poc_suffix            = "unit-test-a1"
    poc_owner             = "fixture-owner"
    cost_center           = "fixture-cost"
  }

  assert {
    condition     = aws_cognito_user_pool.poc[0].name == "spur-context-auth-poc-unit-test-a1" && aws_cognito_user_pool.poc[0].deletion_protection == "INACTIVE"
    error_message = "enabled mode must use the unique disposable POC name"
  }

  assert {
    condition     = length(aws_cognito_user_pool_client.m2m) == 2 && aws_cognito_user_pool_client.human[0].generate_secret == false
    error_message = "the POC needs one public human client and two isolated M2M scope bundles"
  }

  assert {
    condition     = aws_apigatewayv2_route.oauth[0].route_key == "POST /mcp/oauth" && aws_apigatewayv2_route.legacy[0].authorization_type == "AWS_IAM"
    error_message = "the exact OAuth route must be additive to the IAM compatibility route"
  }

  assert {
    condition = (
      aws_lambda_function.validation[0].environment[0].variables["SPUR_INDEX_STATE_MACHINE_ARN"] == "" &&
      aws_lambda_function.validation[0].environment[0].variables["SPUR_INDEX_MAX_RUNNING_JOBS_GLOBAL"] == "0" &&
      aws_lambda_function.validation[0].environment[0].variables["SPUR_INDEX_MAX_QUEUED_JOBS_GLOBAL"] == "0"
    )
    error_message = "the POC Lambda must have no state machine and zero dispatch caps"
  }

  assert {
    condition = (
      lookup(aws_lambda_function.validation[0].environment[0].variables, "SPUR_CATALOG_DSN", "") == "ducklake:sqlite:/tmp/spur-context-auth-poc-unit-test-a1.ducklake" &&
      aws_lambda_function.validation[0].environment[0].variables["SPUR_CONTEXT_ALLOWED_SOURCE_DOMAINS"] == "poc-no-source.invalid" &&
      jsondecode(file("${path.module}/fixtures/external-index-validation-only.json")).args.source_url == "https://validation-only.invalid/spur-context-poc.tar.gz"
    )
    error_message = "the Lambda must bootstrap a local catalog before denying the committed hostname before DNS"
  }

  assert {
    condition     = aws_dynamodb_table.index_jobs[0].name == "spur-context-auth-poc-unit-test-a1-jobs"
    error_message = "the validation table must be dedicated to this POC suffix"
  }
}

run "anonymous_internal_is_mock_fixture_only" {
  command = plan

  variables {
    poc_enabled               = true
    creation_confirmation     = "I_UNDERSTAND_THIS_CREATES_DISPOSABLE_POC_RESOURCES"
    poc_suffix                = "unit-test-none"
    poc_owner                 = "fixture-owner"
    cost_center               = "fixture-cost"
    legacy_authorization_type = "NONE"
    allow_anonymous_mutations = true
  }

  assert {
    condition = (
      aws_apigatewayv2_route.legacy[0].authorization_type == "NONE" &&
      aws_lambda_function.validation[0].environment[0].variables["SPUR_CONTEXT_ALLOW_ANONYMOUS_MUTATIONS"] == "1"
    )
    error_message = "the explicit mock fixture must preserve anonymous-internal compatibility"
  }
}
