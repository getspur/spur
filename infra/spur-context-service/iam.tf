data "aws_caller_identity" "current" {}

resource "aws_iam_policy" "context_service_invoke" {
  name        = "SpurContextServiceInvoke"
  description = "Allows SigV4 callers to invoke the SPUR context-service HTTP API"

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect = "Allow"
      Action = [
        "execute-api:Invoke"
      ]
      Resource = "${aws_apigatewayv2_api.http.execution_arn}/*/*"
    }]
  })
}

resource "aws_iam_role" "lambda" {
  name = "spur-context-lambda"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "lambda.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy_attachment" "lambda_basic" {
  role       = aws_iam_role.lambda.name
  policy_arn = "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole"
}

resource "aws_iam_role_policy_attachment" "lambda_vpc_access" {
  role       = aws_iam_role.lambda.name
  policy_arn = "arn:aws:iam::aws:policy/service-role/AWSLambdaVPCAccessExecutionRole"
}

resource "aws_iam_role" "code_lambda" {
  name = "spur-context-code-lambda"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "lambda.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role" "knowledge_lambda" {
  name = "spur-context-knowledge-lambda"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "lambda.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

# X-Ray writes are account-scoped, while log writes remain restricted to each
# function's dedicated log group. Neither serving function is VPC-attached.
resource "aws_iam_role_policy" "code_lambda_runtime" {
  name = "CodeLambdaRuntimeAccess"
  role = aws_iam_role.code_lambda.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid    = "DedicatedCodeLogs"
        Effect = "Allow"
        Action = [
          "logs:CreateLogStream",
          "logs:PutLogEvents"
        ]
        Resource = "${aws_cloudwatch_log_group.code_lambda.arn}:*"
      },
      {
        Sid    = "ActiveXRayTracing"
        Effect = "Allow"
        Action = [
          "xray:PutTraceSegments",
          "xray:PutTelemetryRecords"
        ]
        Resource = "*"
      }
    ]
  })
}

resource "aws_iam_role_policy" "knowledge_lambda_runtime" {
  name = "KnowledgeLambdaRuntimeAccess"
  role = aws_iam_role.knowledge_lambda.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid    = "DedicatedKnowledgeLogs"
        Effect = "Allow"
        Action = [
          "logs:CreateLogStream",
          "logs:PutLogEvents"
        ]
        Resource = "${aws_cloudwatch_log_group.knowledge_lambda.arn}:*"
      },
      {
        Sid    = "ActiveXRayTracing"
        Effect = "Allow"
        Action = [
          "xray:PutTraceSegments",
          "xray:PutTelemetryRecords"
        ]
        Resource = "*"
      }
    ]
  })
}

# Code can read only the shared pointer, immutable serving registries, and
# immutable Silver graph/source artifacts. It cannot list or mutate the bucket.
resource "aws_iam_role_policy" "code_s3_read" {
  name = "CodeServingArtifactRead"
  role = aws_iam_role.code_lambda.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect = "Allow"
      Action = [
        "s3:GetObject",
        "s3:GetObjectVersion"
      ]
      Resource = [
        "${aws_s3_bucket.data.arn}/${local.catalog_s3_key}",
        "${aws_s3_bucket.data.arn}/${local.catalog_snapshot_s3_prefix}/generations/*/serving-registry.json",
        "${aws_s3_bucket.data.arn}/silver/*"
      ]
    }]
  })
}

# Knowledge reads the frozen pointer/snapshot and immutable query data. It has
# no S3 mutation permission.
resource "aws_iam_role_policy" "knowledge_s3_read" {
  name = "KnowledgeFrozenCatalogRead"
  role = aws_iam_role.knowledge_lambda.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "s3:GetObject",
          "s3:GetObjectVersion"
        ]
        Resource = "${aws_s3_bucket.data.arn}/*"
      },
      {
        Effect = "Allow"
        Action = [
          "s3:ListBucket"
        ]
        Resource = aws_s3_bucket.data.arn
      }
    ]
  })
}

resource "aws_iam_role_policy" "code_lambda_dynamodb" {
  name = "CodeDynamoDbControlPlaneAccess"
  role = aws_iam_role.code_lambda.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "dynamodb:GetItem",
          "dynamodb:PutItem",
          "dynamodb:UpdateItem",
          "dynamodb:DeleteItem",
          "dynamodb:TransactWriteItems",
          "dynamodb:Scan"
        ]
        Resource = [
          aws_dynamodb_table.index_jobs.arn,
          aws_dynamodb_table.catalog_leases.arn
        ]
      },
      {
        Effect = "Allow"
        Action = [
          "dynamodb:Query"
        ]
        Resource = "${aws_dynamodb_table.index_jobs.arn}/index/${var.index_queue_gsi_name}"
      }
    ]
  })
}

resource "aws_iam_role_policy" "code_lambda_sfn" {
  name = "CodeStepFunctionsControl"
  role = aws_iam_role.code_lambda.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "states:StartExecution"
        ]
        Resource = aws_sfn_state_machine.index_build.arn
      },
      {
        Effect = "Allow"
        Action = [
          "states:DescribeExecution",
          "states:StopExecution"
        ]
        Resource = "*"
      }
    ]
  })
}

resource "aws_iam_role" "source_fetcher_lambda" {
  name = "spur-context-source-fetcher-lambda"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "lambda.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy" "source_fetcher_lambda" {
  name = "SourceFetcherAccess"
  role = aws_iam_role.source_fetcher_lambda.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "logs:CreateLogStream",
          "logs:PutLogEvents"
        ]
        Resource = "${aws_cloudwatch_log_group.source_fetcher_lambda.arn}:*"
      },
      {
        Effect = "Allow"
        Action = [
          "s3:PutObject",
          "s3:GetObject",
          "s3:AbortMultipartUpload"
        ]
        Resource = "${aws_s3_bucket.data.arn}/fetch/*"
      },
      {
        Effect = "Allow"
        Action = [
          "s3:ListBucket"
        ]
        Resource = aws_s3_bucket.data.arn
        Condition = {
          StringLike = {
            "s3:prefix" = [
              "fetch",
              "fetch/*"
            ]
          }
        }
      }
    ]
  })
}

resource "aws_iam_role_policy" "lambda_catalog_secret" {
  name = "CatalogSecretAccess"
  role = aws_iam_role.lambda.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect = "Allow"
      Action = [
        "secretsmanager:GetSecretValue"
      ]
      Resource = local.aurora_master_secret_arn
    }]
  })
}

resource "aws_iam_role_policy" "s3_access" {
  name = "S3CatalogAccess"
  role = aws_iam_role.lambda.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect = "Allow"
      Action = [
        "s3:GetObject",
        "s3:GetObjectVersion",
        "s3:PutObject",
        "s3:ListBucket",
        "s3:DeleteObject"
      ]
      Resource = [
        aws_s3_bucket.data.arn,
        "${aws_s3_bucket.data.arn}/*"
      ]
    }]
  })
}

resource "aws_iam_role_policy" "lambda_dynamodb" {
  name = "DynamoDbControlPlaneAccess"
  role = aws_iam_role.lambda.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "dynamodb:GetItem",
          "dynamodb:PutItem",
          "dynamodb:UpdateItem",
          "dynamodb:DeleteItem",
          "dynamodb:TransactWriteItems",
          "dynamodb:Scan"
        ]
        Resource = [
          aws_dynamodb_table.index_jobs.arn,
          aws_dynamodb_table.catalog_leases.arn
        ]
      },
      {
        Effect = "Allow"
        Action = [
          "dynamodb:Query"
        ]
        Resource = "${aws_dynamodb_table.index_jobs.arn}/index/${var.index_queue_gsi_name}"
      }
    ]
  })
}

# Personal API-key permissions intentionally remain outside the legacy serving
# policy above. The authorizer can only read one primary-key record, management
# can transact key/counter records and query the owner index, and cleanup has a
# separate execution role for cursor/expiry work.

resource "aws_iam_role" "api_key_authorizer" {
  count = var.api_key_auth_enabled ? 1 : 0

  name = "spur-context-api-key-authorizer"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "lambda.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy" "api_key_authorizer" {
  count = var.api_key_auth_enabled ? 1 : 0

  name = "ApiKeyAuthorizerAccess"
  role = aws_iam_role.api_key_authorizer[0].id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid      = "ConsistentPrimaryKeyLookup"
        Effect   = "Allow"
        Action   = local.api_key_authorizer_dynamodb_actions
        Resource = aws_dynamodb_table.api_keys[0].arn
      },
      {
        Sid    = "DedicatedAuthorizerLogs"
        Effect = "Allow"
        Action = [
          "logs:CreateLogStream",
          "logs:PutLogEvents"
        ]
        Resource = "${aws_cloudwatch_log_group.api_key_authorizer[0].arn}:*"
      }
    ]
  })
}

resource "aws_iam_role_policy" "api_key_management" {
  count = var.api_key_auth_enabled ? 1 : 0

  name = "ApiKeyManagementAccess"
  role = aws_iam_role.code_lambda.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid      = "KeyAndOwnerCounterTransactions"
        Effect   = "Allow"
        Action   = local.api_key_management_dynamodb_actions
        Resource = aws_dynamodb_table.api_keys[0].arn
      },
      {
        Sid      = "OwnKeyListing"
        Effect   = "Allow"
        Action   = local.api_key_management_query_actions
        Resource = "${aws_dynamodb_table.api_keys[0].arn}/index/${var.api_key_owner_gsi_name}"
      }
    ]
  })
}

resource "aws_iam_role" "api_key_cleanup" {
  count = var.api_key_auth_enabled ? 1 : 0

  name = "spur-context-api-key-cleanup"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "lambda.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy" "api_key_cleanup" {
  count = var.api_key_auth_enabled ? 1 : 0

  name = "ApiKeyCleanupAccess"
  role = aws_iam_role.api_key_cleanup[0].id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid      = "PersistCursorAndRevokeExpiredKeys"
        Effect   = "Allow"
        Action   = local.api_key_cleanup_dynamodb_actions
        Resource = aws_dynamodb_table.api_keys[0].arn
      },
      {
        Sid    = "BoundedExpiryAndOwnerQueries"
        Effect = "Allow"
        Action = local.api_key_cleanup_query_actions
        Resource = [
          "${aws_dynamodb_table.api_keys[0].arn}/index/${var.api_key_expiry_gsi_name}",
          "${aws_dynamodb_table.api_keys[0].arn}/index/${var.api_key_owner_gsi_name}"
        ]
      },
      {
        Sid    = "DedicatedCleanupLogs"
        Effect = "Allow"
        Action = [
          "logs:CreateLogStream",
          "logs:PutLogEvents"
        ]
        Resource = "${aws_cloudwatch_log_group.api_key_cleanup[0].arn}:*"
      }
    ]
  })
}

# Lambda → Step Functions: StartExecution for on-demand indexing.
resource "aws_iam_role_policy" "lambda_sfn" {
  name = "StepFunctionsStartExecution"
  role = aws_iam_role.lambda.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "states:StartExecution"
        ]
        Resource = aws_sfn_state_machine.index_build.arn
      },
      {
        Effect = "Allow"
        Action = [
          "states:DescribeExecution",
          "states:StopExecution"
        ]
        Resource = "*"
      }
    ]
  })
}

# ─── Step Functions IAM ──────────────────────────────────────────────────────

# SF execution role: run ECS tasks + manage EventBridge rules for task monitoring.
resource "aws_iam_role" "sfn" {
  name = "spur-context-sfn"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "states.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy" "sfn_ecs" {
  name = "EcsRunTask"
  role = aws_iam_role.sfn.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = ["ecs:RunTask"]
        Resource = [
          "${aws_ecs_task_definition.worker.arn_without_revision}:*"
        ]
        Condition = {
          StringEquals = {
            "ecs:cluster" = aws_ecs_cluster.indexing.arn
          }
        }
      },
      {
        Effect = "Allow"
        Action = [
          "ecs:StopTask",
          "ecs:DescribeTasks"
        ]
        Resource = "*"
      },
      {
        Effect = "Allow"
        Action = [
          "events:PutTargets",
          "events:PutRule",
          "events:DescribeRule"
        ]
        Resource = [
          "arn:aws:events:${var.aws_region}:${data.aws_caller_identity.current.account_id}:rule/StepFunctionsGetEventsForECSTaskRule"
        ]
      },
      {
        Effect = "Allow"
        Action = ["iam:PassRole"]
        Resource = [
          aws_iam_role.ecs_task_execution.arn,
          aws_iam_role.ecs_task.arn
        ]
        Condition = {
          StringEquals = {
            "iam:PassedToService" = "ecs-tasks.amazonaws.com"
          }
        }
      }
    ]
  })
}

resource "aws_iam_role_policy" "sfn_lambda" {
  name = "LambdaInvokeWorker"
  role = aws_iam_role.sfn.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect = "Allow"
      Action = ["lambda:InvokeFunction"]
      Resource = [
        aws_lambda_function.worker.arn,
        aws_lambda_alias.worker_live.arn
      ]
    }]
  })
}

resource "aws_iam_role_policy" "sfn_source_fetcher_lambda" {
  name = "LambdaInvokeSourceFetcher"
  role = aws_iam_role.sfn.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect = "Allow"
      Action = ["lambda:InvokeFunction"]
      Resource = [
        aws_lambda_function.source_fetcher.arn,
        aws_lambda_alias.source_fetcher_live.arn
      ]
    }]
  })
}

# ─── ECS Task Execution Role ─────────────────────────────────────────────────
# Allows the ECS agent to pull the image from ECR and write logs.

resource "aws_iam_role" "ecs_task_execution" {
  name = "spur-context-worker-execution"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "ecs-tasks.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy_attachment" "ecs_task_execution" {
  role       = aws_iam_role.ecs_task_execution.name
  policy_arn = "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy"
}

resource "aws_iam_role_policy" "ecs_task_execution_catalog_secret" {
  name = "CatalogSecretAccess"
  role = aws_iam_role.ecs_task_execution.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect = "Allow"
      Action = [
        "secretsmanager:GetSecretValue"
      ]
      Resource = local.aurora_master_secret_arn
    }]
  })
}

# ─── ECS Task Role ───────────────────────────────────────────────────────────
# The worker container's runtime permissions: S3 write (checkpoints + DuckLake
# data), SFN SendTaskSuccess/Failure, and pass-through IAM for AWS SDK calls.

resource "aws_iam_role" "ecs_task" {
  name = "spur-context-worker-task"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "ecs-tasks.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy" "ecs_task_s3" {
  name = "S3CheckpointAndDataAccess"
  role = aws_iam_role.ecs_task.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "s3:GetObject",
          "s3:GetObjectVersion",
          "s3:PutObject",
          "s3:ListBucket",
          "s3:DeleteObject"
        ]
        Resource = [
          aws_s3_bucket.data.arn,
          "${aws_s3_bucket.data.arn}/*"
        ]
      }
    ]
  })
}

resource "aws_iam_role_policy" "ecs_task_dynamodb" {
  name = "DynamoDbControlPlaneAccess"
  role = aws_iam_role.ecs_task.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect = "Allow"
      Action = [
        "dynamodb:GetItem",
        "dynamodb:PutItem",
        "dynamodb:UpdateItem",
        "dynamodb:DeleteItem",
        "dynamodb:TransactWriteItems"
      ]
      Resource = [
        aws_dynamodb_table.index_jobs.arn,
        aws_dynamodb_table.catalog_leases.arn
      ]
    }]
  })
}

resource "aws_iam_role_policy" "ecs_task_sfn" {
  name = "StepFunctionsTaskToken"
  role = aws_iam_role.ecs_task.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect = "Allow"
      Action = [
        "states:SendTaskSuccess",
        "states:SendTaskFailure",
        "states:SendTaskHeartbeat"
      ]
      Resource = "*"
    }]
  })
}
