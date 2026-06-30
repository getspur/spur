# Step Functions state machine for on-demand indexing.
# Triggered by the serve Lambda's external_index MCP tool via StartExecution.

resource "aws_sfn_state_machine" "index_build" {
  name     = "spur-context-index-build"
  role_arn = aws_iam_role.sfn.arn

  # The ASL template uses ${...} placeholders that Terraform replaces.
  # The worker uses runTask.sync — SF waits for the task to complete.
  definition = templatefile(
    "${path.module}/index_build_asl.json",
    {
      cluster_arn                       = aws_ecs_cluster.indexing.arn
      worker_taskdef_arn                = aws_ecs_task_definition.worker.arn
      worker_lambda_arn                 = aws_lambda_alias.worker_live.arn
      source_fetch_lambda_arn           = aws_lambda_alias.source_fetcher_live.arn
      worker_lambda_timeout_sec         = var.worker_lambda_timeout_sec
      source_fetcher_lambda_timeout_sec = var.source_fetcher_lambda_timeout_sec
      worker_ecs_timeout_sec            = var.context_max_build_seconds + 900
      index_jobs_table_name             = aws_dynamodb_table.index_jobs.name
      catalog_leases_table_name         = aws_dynamodb_table.catalog_leases.name
      catalog_dsn                       = local.aurora_catalog_dsn
      context_ducklake_data_path        = local.context_ducklake_data_path
      worker_checkpoint_uri_template    = local.worker_checkpoint_uri_template
      subnets_json                      = jsonencode(var.worker_subnets)
      security_groups_json              = jsonencode([aws_security_group.worker.id])
    }
  )
}

# Security group for the worker task — egress to S3 (via VPC endpoint or NAT),
# RDS (DuckLake catalog), and SFN (Step Functions API).
resource "aws_security_group" "worker" {
  name        = "spur-context-worker"
  description = "Egress for indexing worker tasks"
  vpc_id      = var.vpc_id

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = {
    Name = "spur-context-worker"
  }
}

# Security group for interface VPC endpoints. Workers connect to AWS APIs over
# private DNS on 443; gateway endpoints use route tables instead of SGs.
resource "aws_security_group" "vpc_endpoints" {
  count = var.create_vpc_endpoints ? 1 : 0

  name        = "spur-context-vpc-endpoints"
  description = "Private AWS service endpoints for indexing workers"
  vpc_id      = var.vpc_id

  ingress {
    description     = "HTTPS from indexing workers"
    from_port       = 443
    to_port         = 443
    protocol        = "tcp"
    security_groups = [aws_security_group.worker.id]
  }

  tags = {
    Name = "spur-context-vpc-endpoints"
  }
}

# Security group for Aurora — Postgres is reachable only from worker tasks.
resource "aws_security_group" "catalog_db" {
  name        = "spur-context-catalog-db"
  description = "Aurora Postgres ingest catalog access"
  vpc_id      = var.vpc_id

  ingress {
    description     = "Postgres from indexing workers"
    from_port       = 5432
    to_port         = 5432
    protocol        = "tcp"
    security_groups = [aws_security_group.worker.id]
  }

  tags = {
    Name = "spur-context-catalog-db"
  }
}
