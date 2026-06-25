data "aws_caller_identity" "current" {}

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
        "s3:ListBucket"
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
