# This plan-only test uses Terraform's mock provider. It must never apply
# resources or contact AWS; it proves native sign-up can complete verification.

mock_provider "aws" {}

mock_provider "aws" {
  alias = "us_east_1"
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

run "native_signup_auto_verifies_email" {
  command = plan

  variables {
    cognito_auth_enabled        = true
    cognito_user_pool_name      = "spur-context-signup-test"
    cognito_domain_prefix       = "spur-context-signup-test"
    cognito_human_callback_urls = ["http://127.0.0.1:8765/callback"]
    cognito_human_logout_urls   = ["http://127.0.0.1:8765/logout"]
  }

  assert {
    condition     = toset(aws_cognito_user_pool.context_service[0].auto_verified_attributes) == toset(["email"])
    error_message = "native email/password sign-up must deliver an email verification code"
  }
}
