# These runs are plan-only and use Terraform's mock provider. They must never
# apply resources or contact AWS; they prove the feature gate and contracts.

mock_provider "aws" {}

mock_provider "aws" {
  alias = "us_east_1"
}

override_resource {
  target = aws_route53_zone.context_service
  values = {
    name_servers = [
      "ns-101.awsdns-01.com",
      "ns-202.awsdns-02.net",
      "ns-303.awsdns-03.org",
      "ns-404.awsdns-04.co.uk",
    ]
  }
  override_during = plan
}

override_resource {
  target = aws_apigatewayv2_api.http
  values = {
    api_endpoint  = "https://example.execute-api.ap-southeast-5.amazonaws.com"
    execution_arn = "arn:aws:execute-api:ap-southeast-5:111122223333:example"
  }
  override_during = plan
}

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

run "custom_domain_bootstrap_only_creates_delegated_zone" {
  command = plan

  assert {
    condition     = aws_route53_zone.context_service.name == "context.getspur.dev"
    error_message = "bootstrap must create the delegated context.getspur.dev public hosted zone"
  }

  assert {
    condition     = output.route53_delegation_name_servers == aws_route53_zone.context_service.name_servers
    error_message = "bootstrap must output the hosted-zone nameservers for Namecheap delegation"
  }

  assert {
    condition = (
      length(aws_acm_certificate.api_custom_domain) == 0 &&
      length(aws_acm_certificate.cognito_custom_domain) == 0 &&
      length(aws_apigatewayv2_domain_name.context_service) == 0 &&
      length(aws_cognito_user_pool_domain.custom) == 0
    )
    error_message = "bootstrap must not request certificates or create custom domains"
  }

  assert {
    condition     = aws_apigatewayv2_api.http.disable_execute_api_endpoint == false && output.api_url == aws_apigatewayv2_api.http.api_endpoint
    error_message = "bootstrap defaults must preserve the execute-api endpoint"
  }
}

run "custom_domains_disabled_preserves_cognito_prefix_domain" {
  command = plan

  variables {
    cognito_auth_enabled        = true
    cognito_user_pool_name      = "spur-context-test-cognito"
    cognito_domain_prefix       = "spur-context-test-auth"
    cognito_human_callback_urls = ["https://test.example.test/callback"]
    cognito_human_logout_urls   = ["https://test.example.test/logout"]
  }

  assert {
    condition     = aws_cognito_user_pool_domain.context_service[0].domain == "spur-context-test-auth" && length(aws_cognito_user_pool_domain.custom) == 0
    error_message = "custom domains disabled must preserve the existing Cognito prefix domain"
  }

  assert {
    condition     = output.cognito_domain_url == "https://spur-context-test-auth.auth.ap-southeast-5.amazoncognito.com"
    error_message = "Cognito OAuth discovery must continue using the prefix domain before activation"
  }

  assert {
    condition     = output.oauth_api_url == "${aws_apigatewayv2_api.http.api_endpoint}/mcp/oauth"
    error_message = "OAuth API discovery must continue using execute-api before activation"
  }
}

run "custom_domain_activation_builds_regional_api_and_cognito_domains" {
  command = plan

  variables {
    custom_domains_enabled      = true
    cognito_auth_enabled        = true
    cognito_user_pool_name      = "spur-context-test-cognito"
    cognito_domain_prefix       = "spur-context-test-auth"
    cognito_human_callback_urls = ["https://test.example.test/callback"]
    cognito_human_logout_urls   = ["https://test.example.test/logout"]
  }

  assert {
    condition = (
      aws_acm_certificate.api_custom_domain[0].domain_name == "context.getspur.dev" &&
      aws_acm_certificate.cognito_custom_domain[0].domain_name == "auth.context.getspur.dev" &&
      length(aws_acm_certificate_validation.api_custom_domain) == 1 &&
      length(aws_acm_certificate_validation.cognito_custom_domain) == 1
    )
    error_message = "activation must create and DNS-validate both regional certificates"
  }

  assert {
    condition = (
      aws_apigatewayv2_domain_name.context_service[0].domain_name == "context.getspur.dev" &&
      aws_apigatewayv2_domain_name.context_service[0].domain_name_configuration[0].endpoint_type == "REGIONAL" &&
      aws_apigatewayv2_domain_name.context_service[0].domain_name_configuration[0].ip_address_type == "dualstack" &&
      aws_apigatewayv2_api_mapping.context_service[0].stage == aws_apigatewayv2_stage.default.name &&
      aws_route53_record.api_custom_domain_ipv4[0].type == "A" &&
      aws_route53_record.api_custom_domain_ipv6[0].type == "AAAA"
    )
    error_message = "activation must map the default API stage to dual-stack Route 53 aliases"
  }

  assert {
    condition = (
      aws_cognito_user_pool_domain.custom[0].domain == "auth.context.getspur.dev" &&
      aws_route53_record.cognito_custom_domain[0].type == "A"
    )
    error_message = "activation must create the Cognito custom domain and CloudFront alias target"
  }

  assert {
    condition = (
      output.api_url == "https://context.getspur.dev" &&
      output.oauth_api_url == "https://context.getspur.dev/mcp/oauth" &&
      output.cognito_domain_url == "https://auth.context.getspur.dev" &&
      aws_lambda_function.service.environment[0].variables["SPUR_CONTEXT_SERVICE_BASE_URL"] == "https://context.getspur.dev" &&
      aws_lambda_function.service.environment[0].variables["SPUR_COGNITO_AUTHORIZATION_ENDPOINT"] == "https://auth.context.getspur.dev/oauth2/authorize"
    )
    error_message = "activation must switch effective API and Cognito OAuth discovery to custom domains"
  }

  assert {
    condition     = aws_apigatewayv2_api.http.disable_execute_api_endpoint == false
    error_message = "custom-domain activation alone must not disable execute-api"
  }
}

run "execute_api_can_only_be_disabled_after_custom_domain_activation" {
  command = plan

  variables {
    disable_execute_api_endpoint = true
  }

  expect_failures = [aws_apigatewayv2_api.http]
}

run "custom_domain_activation_requires_cognito_auth" {
  command = plan

  variables {
    custom_domains_enabled = true
    cognito_auth_enabled   = false
  }

  expect_failures = [aws_apigatewayv2_api.http]
}

run "execute_api_disable_is_a_separate_post_migration_control" {
  command = plan

  variables {
    custom_domains_enabled       = true
    disable_execute_api_endpoint = true
    cognito_auth_enabled         = true
    cognito_user_pool_name       = "spur-context-test-cognito"
    cognito_domain_prefix        = "spur-context-test-auth"
    cognito_human_callback_urls  = ["https://test.example.test/callback"]
    cognito_human_logout_urls    = ["https://test.example.test/logout"]
  }

  assert {
    condition     = aws_apigatewayv2_api.http.disable_execute_api_endpoint == true && output.api_url == "https://context.getspur.dev"
    error_message = "post-migration toggle must disable execute-api without changing the custom service URL"
  }
}

run "runbook_uses_regional_issuer_for_oidc_discovery" {
  command = plan

  assert {
    condition = (
      strcontains(file("${path.module}/README.md"), "cognito_issuer=\"$(terraform output -raw cognito_issuer)\"") &&
      strcontains(file("${path.module}/README.md"), "\"$${cognito_issuer}/.well-known/openid-configuration\"") &&
      !strcontains(file("${path.module}/README.md"), "https://auth.context.getspur.dev/.well-known/openid-configuration")
    )
    error_message = "the runbook must fetch OIDC discovery from the regional Cognito issuer, not the custom OAuth domain"
  }

  assert {
    condition = (
      strcontains(file("${path.module}/README.md"), ".authorization_endpoint == \"https://auth.context.getspur.dev/oauth2/authorize\"") &&
      strcontains(file("${path.module}/README.md"), ".token_endpoint == \"https://auth.context.getspur.dev/oauth2/token\"")
    )
    error_message = "the runbook must verify Cognito advertises custom-domain authorization and token endpoints"
  }
}
