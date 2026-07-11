# Terraform test uses a mock provider and command=plan only. It never contacts
# AWS and cannot apply resources.
mock_provider "aws" {}

run "disabled_default_plans_no_poc_resources" {
  command = plan

  assert {
    condition = (
      length(aws_cognito_user_pool.poc) == 0 &&
      length(aws_apigatewayv2_api.poc) == 0 &&
      length(aws_lambda_function.validation) == 0 &&
      length(aws_dynamodb_table.index_jobs) == 0
    )
    error_message = "the default safety gate must plan no POC resources"
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
