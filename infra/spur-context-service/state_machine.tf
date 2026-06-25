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
      cluster_arn                    = aws_ecs_cluster.indexing.arn
      worker_taskdef_arn             = aws_ecs_task_definition.worker.arn
      index_jobs_table_name          = aws_dynamodb_table.index_jobs.name
      catalog_leases_table_name      = aws_dynamodb_table.catalog_leases.name
      catalog_s3_uri                 = var.catalog_s3_uri
      context_ducklake_data_path     = local.context_ducklake_data_path
      worker_checkpoint_uri_template = local.worker_checkpoint_uri_template
      subnets_json                   = jsonencode(var.worker_subnets)
      security_groups_json           = jsonencode([aws_security_group.worker.id])
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
