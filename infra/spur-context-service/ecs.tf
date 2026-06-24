# ECS cluster for the on-demand indexing worker.
# Capacity providers: FARGATE_SPOT (primary, cheaper) with FARGATE (on-demand)
# base=1 fallback. The SF state machine's CapacityProviderStrategy controls
# the actual mix per task; the cluster just needs both providers associated.

resource "aws_ecs_cluster" "indexing" {
  name = "spur-context-indexing"

  setting {
    name  = "containerInsights"
    value = "enabled"
  }
}

resource "aws_ecs_cluster_capacity_providers" "indexing" {
  cluster_name = aws_ecs_cluster.indexing.name

  capacity_providers = ["FARGATE", "FARGATE_SPOT"]

  default_capacity_strategy {
    capacity_provider = "FARGATE_SPOT"
    weight            = 4

    base = 0
  }

  default_capacity_strategy {
    capacity_provider = "FARGATE"
    weight            = 1
  }}

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
