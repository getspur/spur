# The legacy state predates the split Code/Knowledge serving rollout. These
# moves preserve the exact live API Gateway route identities while changing
# their route keys and destinations in place.
moved {
  from = aws_apigatewayv2_route.default
  to   = aws_apigatewayv2_route.compatibility_knowledge
}

moved {
  from = aws_apigatewayv2_route.oauth
  to   = aws_apigatewayv2_route.oauth_knowledge
}

# Retain the legacy serving resources for one non-destructive cutover. They are
# deliberately frozen and are not referenced by any new direct integration,
# route, event target, or output.
resource "aws_cloudwatch_log_group" "lambda" {
  name              = "/aws/lambda/spur-context-service"
  retention_in_days = 14

  lifecycle {
    prevent_destroy = true
    ignore_changes  = all
  }
}

resource "aws_cloudwatch_log_group" "sfn" {
  name              = "/aws/vendedlogs/states/spur-context-index-build"
  retention_in_days = 14

  lifecycle {
    prevent_destroy = true
    ignore_changes  = all
  }
}

resource "aws_lambda_function" "service" {
  function_name = "spur-context-service"
  description   = "Legacy context serving compatibility retention"

  s3_bucket        = aws_s3_bucket.data.bucket
  s3_key           = aws_s3_object.lambda_zip.key
  source_code_hash = filebase64sha256(var.lambda_zip_path)

  runtime       = "provided.al2023"
  architectures = ["arm64"]
  handler       = "bootstrap"
  role          = aws_iam_role.lambda.arn
  timeout       = var.lambda_timeout_sec
  memory_size   = var.lambda_memory_mb

  lifecycle {
    prevent_destroy = true
    ignore_changes  = all
  }
}

# The legacy worker IAM objects exist in the canonical state and live account,
# but no longer participate in the current worker execution path. Minimal valid
# declarations keep each address owned; ignore_changes freezes the reviewed
# live policy documents until a later cleanup task explicitly removes them.
locals {
  legacy_compatibility_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect   = "Deny"
      Action   = "iam:CreateUser"
      Resource = "*"
    }]
  })
}

resource "aws_iam_role" "worker_lambda" {
  name_prefix        = "spur-context-worker-lambda-compat-"
  assume_role_policy = aws_iam_role.lambda.assume_role_policy

  lifecycle {
    prevent_destroy = true
    ignore_changes  = all
  }
}

resource "aws_iam_role_policy" "lambda_xray" {
  name_prefix = "legacy-lambda-xray-compat-"
  role        = aws_iam_role.lambda.id
  policy      = local.legacy_compatibility_policy

  lifecycle {
    prevent_destroy = true
    ignore_changes  = all
  }
}

resource "aws_iam_role_policy" "sfn_observability" {
  name_prefix = "legacy-sfn-observability-compat-"
  role        = aws_iam_role.sfn.id
  policy      = local.legacy_compatibility_policy

  lifecycle {
    prevent_destroy = true
    ignore_changes  = all
  }
}

resource "aws_iam_role_policy" "worker_lambda_dynamodb" {
  name_prefix = "legacy-worker-dynamodb-compat-"
  role        = aws_iam_role.worker_lambda.id
  policy      = local.legacy_compatibility_policy

  lifecycle {
    prevent_destroy = true
    ignore_changes  = all
  }
}

resource "aws_iam_role_policy" "worker_lambda_observability" {
  name_prefix = "legacy-worker-observability-compat-"
  role        = aws_iam_role.worker_lambda.id
  policy      = local.legacy_compatibility_policy

  lifecycle {
    prevent_destroy = true
    ignore_changes  = all
  }
}

resource "aws_iam_role_policy" "worker_lambda_s3" {
  name_prefix = "legacy-worker-s3-compat-"
  role        = aws_iam_role.worker_lambda.id
  policy      = local.legacy_compatibility_policy

  lifecycle {
    prevent_destroy = true
    ignore_changes  = all
  }
}

resource "aws_iam_role_policy" "worker_lambda_vpc" {
  name_prefix = "legacy-worker-vpc-compat-"
  role        = aws_iam_role.worker_lambda.id
  policy      = local.legacy_compatibility_policy

  lifecycle {
    prevent_destroy = true
    ignore_changes  = all
  }
}

resource "aws_iam_role_policy" "worker_lambda_vpc_function_deny" {
  name_prefix = "legacy-worker-vpc-deny-compat-"
  role        = aws_iam_role.worker_lambda.id
  policy      = local.legacy_compatibility_policy

  lifecycle {
    prevent_destroy = true
    ignore_changes  = all
  }
}

# The old invocation grants stay attached to the frozen legacy function. The
# split Code and Knowledge grants use distinct addresses in main.tf.
resource "aws_lambda_permission" "apigw" {
  statement_id  = "apigateway-invocation"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.service.function_name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_apigatewayv2_api.http.execution_arn}/*/*"

  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_lambda_permission" "eventbridge_drainer" {
  statement_id  = "eventbridge-index-queue-drainer"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.service.function_name
  principal     = "events.amazonaws.com"
  source_arn    = aws_cloudwatch_event_rule.index_queue_drainer.arn

  lifecycle {
    prevent_destroy = true
  }
}
