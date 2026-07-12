resource "aws_iam_role" "lambda" {
  count = var.poc_enabled ? 1 : 0

  name = "${local.name_prefix}-lambda"
  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Action    = "sts:AssumeRole"
      Principal = { Service = "lambda.amazonaws.com" }
    }]
  })
}

resource "aws_iam_role_policy" "lambda" {
  count = var.poc_enabled ? 1 : 0

  name = "${local.name_prefix}-validation-only"
  role = aws_iam_role.lambda[0].id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid      = "WriteDedicatedLogs"
        Effect   = "Allow"
        Action   = ["logs:CreateLogStream", "logs:PutLogEvents"]
        Resource = ["${aws_cloudwatch_log_group.lambda[0].arn}:*"]
      },
      {
        Sid      = "UseDedicatedValidationTable"
        Effect   = "Allow"
        Action   = ["dynamodb:GetItem", "dynamodb:PutItem", "dynamodb:Query", "dynamodb:UpdateItem"]
        Resource = [aws_dynamodb_table.index_jobs[0].arn]
      },
    ]
  })
}

resource "aws_iam_policy" "invoke" {
  count = var.poc_enabled ? 1 : 0

  name        = "${local.name_prefix}-invoke"
  description = "Invoke only the disposable Cognito POC API."
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid      = "InvokeDedicatedPocApiOnly"
      Effect   = "Allow"
      Action   = "execute-api:Invoke"
      Resource = "${aws_apigatewayv2_api.poc[0].execution_arn}/*/*"
    }]
  })
}

resource "aws_iam_role_policy" "api_key_management" {
  count = local.api_key_poc_enabled ? 1 : 0

  name = "ApiKeyManagementAccess"
  role = aws_iam_role.lambda[0].id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid    = "KeyAndOwnerCounterTransactions"
        Effect = "Allow"
        Action = [
          "dynamodb:GetItem",
          "dynamodb:PutItem",
          "dynamodb:UpdateItem",
          "dynamodb:TransactWriteItems",
        ]
        Resource = aws_dynamodb_table.api_keys[0].arn
      },
      {
        Sid      = "OwnKeyListing"
        Effect   = "Allow"
        Action   = ["dynamodb:Query"]
        Resource = "${aws_dynamodb_table.api_keys[0].arn}/index/owner-gsi"
      },
    ]
  })
}

resource "aws_iam_role" "api_key_authorizer" {
  count = local.api_key_poc_enabled ? 1 : 0

  name = "${local.name_prefix}-api-key-authorizer"
  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Action    = "sts:AssumeRole"
      Principal = { Service = "lambda.amazonaws.com" }
    }]
  })
}

resource "aws_iam_role_policy" "api_key_authorizer" {
  count = local.api_key_poc_enabled ? 1 : 0

  name = "ApiKeyAuthorizerAccess"
  role = aws_iam_role.api_key_authorizer[0].id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid      = "ConsistentPrimaryKeyLookup"
        Effect   = "Allow"
        Action   = ["dynamodb:GetItem"]
        Resource = aws_dynamodb_table.api_keys[0].arn
      },
      {
        Sid      = "DedicatedAuthorizerLogs"
        Effect   = "Allow"
        Action   = ["logs:CreateLogStream", "logs:PutLogEvents"]
        Resource = "${aws_cloudwatch_log_group.api_key_authorizer[0].arn}:*"
      },
    ]
  })
}

resource "aws_iam_role" "api_key_cleanup" {
  count = local.api_key_poc_enabled ? 1 : 0

  name = "${local.name_prefix}-api-key-cleanup"
  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Action    = "sts:AssumeRole"
      Principal = { Service = "lambda.amazonaws.com" }
    }]
  })
}

resource "aws_iam_role_policy" "api_key_cleanup" {
  count = local.api_key_poc_enabled ? 1 : 0

  name = "ApiKeyCleanupAccess"
  role = aws_iam_role.api_key_cleanup[0].id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid      = "PersistCursorAndRevokeExpiredKeys"
        Effect   = "Allow"
        Action   = ["dynamodb:GetItem", "dynamodb:UpdateItem", "dynamodb:TransactWriteItems"]
        Resource = aws_dynamodb_table.api_keys[0].arn
      },
      {
        Sid    = "BoundedExpiryAndOwnerQueries"
        Effect = "Allow"
        Action = ["dynamodb:Query"]
        Resource = [
          "${aws_dynamodb_table.api_keys[0].arn}/index/expiry-gsi",
          "${aws_dynamodb_table.api_keys[0].arn}/index/owner-gsi",
        ]
      },
      {
        Sid      = "DedicatedCleanupLogs"
        Effect   = "Allow"
        Action   = ["logs:CreateLogStream", "logs:PutLogEvents"]
        Resource = "${aws_cloudwatch_log_group.api_key_cleanup[0].arn}:*"
      },
    ]
  })
}
