# These runs are plan-only and use Terraform's mock provider. They must never
# apply resources or contact AWS; they prove the feature gate and contracts.

mock_provider "aws" {}

variables {
  lambda_zip_path                     = "index_build_asl.json"
  worker_ecr_image                    = "111122223333.dkr.ecr.us-east-1.amazonaws.com/spur-context-worker:test"
  worker_lambda_image                 = "111122223333.dkr.ecr.us-east-1.amazonaws.com/spur-context-worker-lambda:test"
  source_fetcher_lambda_image         = "111122223333.dkr.ecr.us-east-1.amazonaws.com/spur-context-source-fetcher:test"
  vpc_id                              = "vpc-0123456789abcdef0"
  worker_subnets                      = ["subnet-0123456789abcdef0"]
  worker_route_table_ids              = ["rtb-0123456789abcdef0"]
  create_vpc_endpoints                = false
  interface_vpc_endpoint_service_keys = []
}

run "disabled_default_keeps_legacy_iam_without_cognito" {
  command = plan

  assert {
    condition     = length(aws_cognito_user_pool.context_service) == 0 && length(aws_cognito_user_pool_client.human) == 0 && length(aws_cognito_user_pool_client.m2m) == 0
    error_message = "disabled mode must not create Cognito user-pool or app-client resources"
  }

  assert {
    condition     = length(aws_apigatewayv2_authorizer.cognito) == 0 && length(aws_apigatewayv2_route.oauth) == 0
    error_message = "disabled mode must not create a JWT authorizer or OAuth route"
  }

  assert {
    condition     = aws_apigatewayv2_route.default.authorization_type == "AWS_IAM"
    error_message = "the existing default route must keep its AWS_IAM default"
  }
}

run "disabled_demo_keeps_legacy_none_route" {
  command = plan

  variables {
    api_authorization_type    = "NONE"
    allow_anonymous_mutations = true
  }

  assert {
    condition     = aws_apigatewayv2_route.default.authorization_type == "NONE" && length(aws_apigatewayv2_route.oauth) == 0
    error_message = "the demo NONE route must remain unchanged while Cognito is disabled"
  }
}

run "enabled_staging_creates_cognito_jwt_route_and_nonsecret_lambda_config" {
  command = plan

  variables {
    cognito_auth_enabled        = true
    cognito_user_pool_name      = "spur-context-staging-cognito"
    cognito_domain_prefix       = "spur-context-staging-auth"
    cognito_human_callback_urls = ["https://staging.example.test/oauth/callback"]
    cognito_human_logout_urls   = ["https://staging.example.test/logout"]
    cognito_m2m_organizations = {
      acme = {
        display_name       = "Acme test organization"
        enabled            = true
        allowed_scopes     = ["external.index", "external.status"]
        access_token_hours = 6
        risk_acceptance    = null
      }
    }
  }

  assert {
    condition     = aws_cognito_user_pool.context_service[0].user_pool_tier == "LITE" && aws_cognito_user_pool_client.human[0].generate_secret == false && aws_cognito_user_pool_client.m2m["acme"].generate_secret == true
    error_message = "enabled mode must create the Lite pool with public human and confidential M2M clients"
  }

  assert {
    condition     = aws_apigatewayv2_route.default.authorization_type == "AWS_IAM" && aws_apigatewayv2_route.oauth[0].route_key == "POST /mcp/oauth" && aws_apigatewayv2_route.oauth[0].authorization_type == "JWT"
    error_message = "the JWT route must be exact and additive to the IAM default route"
  }

  assert {
    condition     = toset(aws_apigatewayv2_route.oauth[0].authorization_scopes) == toset(["urn:spur:context-service/external.read", "urn:spur:context-service/external.index", "urn:spur:context-service/external.status"])
    error_message = "the JWT edge gate must contain all three broad custom scopes"
  }

  assert {
    condition     = aws_lambda_function.service.environment[0].variables["SPUR_COGNITO_AUTH_ENABLED"] == "1" && aws_lambda_function.service.environment[0].variables["SPUR_COGNITO_OAUTH_PATH"] == "/mcp/oauth" && !contains(keys(aws_lambda_function.service.environment[0].variables), "SPUR_COGNITO_CLIENT_SECRET")
    error_message = "Lambda may receive only non-secret Cognito validation configuration"
  }

}

run "enabled_production_keeps_iam_default_alongside_jwt_route" {
  command = plan

  variables {
    cognito_auth_enabled        = true
    cognito_user_pool_name      = "spur-context-production-cognito"
    cognito_domain_prefix       = "spur-context-production-auth"
    cognito_human_callback_urls = ["https://app.example.test/oauth/callback"]
    cognito_human_logout_urls   = ["https://app.example.test/logout"]
  }

  assert {
    condition     = aws_apigatewayv2_route.default.authorization_type == "AWS_IAM" && aws_apigatewayv2_authorizer.cognito[0].authorizer_type == "JWT"
    error_message = "production must retain $default AWS_IAM while adding a Cognito JWT authorizer"
  }
}

run "enabled_requires_human_callback_urls" {
  command = plan

  variables {
    cognito_auth_enabled      = true
    cognito_user_pool_name    = "spur-context-test-cognito"
    cognito_domain_prefix     = "spur-context-test-auth"
    cognito_human_logout_urls = ["https://test.example.test/logout"]
  }

  expect_failures = [var.cognito_human_callback_urls]
}

run "invalid_m2m_scope_is_rejected" {
  command = plan

  variables {
    cognito_auth_enabled        = true
    cognito_user_pool_name      = "spur-context-test-cognito"
    cognito_domain_prefix       = "spur-context-test-auth"
    cognito_human_callback_urls = ["http://127.0.0.1:3000/callback"]
    cognito_human_logout_urls   = ["http://127.0.0.1:3000/logout"]
    cognito_m2m_organizations = {
      invalid = {
        display_name       = "Invalid scope test"
        enabled            = true
        allowed_scopes     = ["external.admin"]
        access_token_hours = 6
        risk_acceptance    = null
      }
    }
  }

  expect_failures = [var.cognito_m2m_organizations]
}

run "m2m_24_hour_ttl_requires_recorded_risk_metadata" {
  command = plan

  variables {
    cognito_auth_enabled        = true
    cognito_user_pool_name      = "spur-context-test-cognito"
    cognito_domain_prefix       = "spur-context-test-auth"
    cognito_human_callback_urls = ["https://test.example.test/callback"]
    cognito_human_logout_urls   = ["https://test.example.test/logout"]
    cognito_m2m_organizations = {
      risky = {
        display_name       = "Risk metadata test"
        enabled            = true
        allowed_scopes     = ["external.read"]
        access_token_hours = 24
        risk_acceptance    = null
      }
    }
  }

  expect_failures = [var.cognito_m2m_organizations]
}

run "authorizer_audience_limit_is_rejected" {
  command = plan

  variables {
    cognito_auth_enabled        = true
    cognito_user_pool_name      = "spur-context-test-cognito"
    cognito_domain_prefix       = "spur-context-test-auth"
    cognito_human_callback_urls = ["https://test.example.test/callback"]
    cognito_human_logout_urls   = ["https://test.example.test/logout"]
    cognito_m2m_organizations = {
      for number in range(50) : "org-${number}" => {
        display_name       = "Audience limit test ${number}"
        enabled            = true
        allowed_scopes     = ["external.read"]
        access_token_hours = 6
        risk_acceptance    = null
      }
    }
  }

  expect_failures = [var.cognito_m2m_organizations]
}
