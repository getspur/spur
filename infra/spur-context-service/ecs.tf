# ECS cluster for the on-demand indexing worker.
# FARGATE and FARGATE_SPOT are AWS-managed capacity providers available on all
# clusters by default. The SF state machine's ASL specifies the
# CapacityProviderStrategy per RunTask call, so no cluster-level default needed.

resource "aws_ecs_cluster" "indexing" {
  name = "spur-context-indexing"

  setting {
    name  = "containerInsights"
    value = "enabled"
  }
}

# CloudWatch log group for worker task output.
resource "aws_cloudwatch_log_group" "worker" {
  name              = "/ecs/spur-context-worker"
  retention_in_days = 14
}

# Worker task definition. The container image is built from the same repo
# (spur-context-service --features worker) and pushed to ECR by deploy.sh.
# Sized at Fargate's upper end (4 vCPU / 30 GB) to handle tokio-sized crates.
resource "aws_ecs_task_definition" "worker" {
  family                   = "spur-context-worker"
  requires_compatibilities = ["FARGATE"]
  network_mode            = "awsvpc"
  cpu                      = "4096"
  memory                   = "30720"

  execution_role_arn = aws_iam_role.ecs_task_execution.arn
  task_role_arn      = aws_iam_role.ecs_task.arn

  container_definitions = jsonencode([
    {
      name      = "worker"
      image     = var.worker_ecr_image
      essential = true

      logConfiguration = {
        logDriver = "awslogs"
        options = {
          "awslogs-group"         = aws_cloudwatch_log_group.worker.name
          "awslogs-region"        = var.aws_region
          "awslogs-stream-prefix" = "indexing"
        }
      }

      stopTimeout = 120

      environment = [
        {
          name  = "AWS_REGION"
          value = var.aws_region
        }
      ]
    }
  ])
}
