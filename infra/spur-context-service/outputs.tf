output "api_url" {
  description = "HTTP API endpoint for the context service"
  value       = aws_apigatewayv2_api.http.api_endpoint
}

output "api_invoke_policy_arn" {
  description = "IAM policy ARN for SigV4 callers allowed to invoke the context-service API"
  value       = aws_iam_policy.context_service_invoke.arn
}

output "lambda_function_name" {
  description = "Lambda function name"
  value       = aws_lambda_function.service.function_name
}

output "lambda_function_arn" {
  description = "Lambda function ARN"
  value       = aws_lambda_function.service.arn
}

output "s3_bucket_name" {
  description = "S3 bucket for DuckLake catalog and data"
  value       = aws_s3_bucket.data.bucket
}

output "index_jobs_table_name" {
  description = "DynamoDB table for index job records and dedupe pointers"
  value       = aws_dynamodb_table.index_jobs.name
}

output "index_jobs_table_arn" {
  description = "DynamoDB table ARN for index job records and dedupe pointers"
  value       = aws_dynamodb_table.index_jobs.arn
}

output "catalog_leases_table_name" {
  description = "DynamoDB table for catalog write leases"
  value       = aws_dynamodb_table.catalog_leases.name
}

output "catalog_leases_table_arn" {
  description = "DynamoDB table ARN for catalog write leases"
  value       = aws_dynamodb_table.catalog_leases.arn
}

output "context_ducklake_data_path" {
  description = "DuckLake data path passed to worker tasks"
  value       = local.context_ducklake_data_path
}

output "aurora_catalog_endpoint" {
  description = "Aurora Postgres endpoint for the live ingest catalog"
  value       = aws_rds_cluster.catalog.endpoint
}

output "aurora_catalog_secret_arn" {
  description = "Secrets Manager ARN for the RDS-managed Aurora master credentials"
  value       = local.aurora_master_secret_arn
}

output "worker_checkpoint_uri_template" {
  description = "Checkpoint URI template passed to Step Functions for per-job worker checkpoint objects"
  value       = local.worker_checkpoint_uri_template
}

output "cloudwatch_log_group" {
  description = "CloudWatch log group for Lambda"
  value       = aws_cloudwatch_log_group.lambda.name
}

output "state_machine_arn" {
  description = "Step Functions state machine ARN for on-demand indexing"
  value       = aws_sfn_state_machine.index_build.arn
}

output "ecs_cluster_name" {
  description = "ECS cluster for indexing workers"
  value       = aws_ecs_cluster.indexing.name
}

output "worker_task_definition_arn" {
  description = "ECS task definition ARN for the indexing worker"
  value       = aws_ecs_task_definition.worker.arn
}

output "worker_image_uri" {
  description = "ECR image URI used by the ECS indexing worker"
  value       = var.worker_ecr_image
}

output "worker_lambda_image_uri" {
  description = "ECR image URI used by the Lambda indexing worker fast path"
  value       = var.worker_lambda_image
}

output "source_fetcher_lambda_image_uri" {
  description = "ECR image URI used by the non-VPC source fetcher Lambda"
  value       = var.source_fetcher_lambda_image
}

output "worker_lambda_function_name" {
  description = "Lambda function name for the indexing worker fast path"
  value       = aws_lambda_function.worker.function_name
}

output "worker_lambda_alias_arn" {
  description = "Lambda alias ARN invoked by the index build state machine"
  value       = aws_lambda_alias.worker_live.arn
}

output "gateway_vpc_endpoint_ids" {
  description = "Gateway VPC endpoint IDs for S3 and DynamoDB, keyed by service"
  value       = { for name, endpoint in aws_vpc_endpoint.gateway : name => endpoint.id }
}

output "interface_vpc_endpoint_ids" {
  description = "Interface VPC endpoint IDs for worker AWS API access, keyed by service"
  value       = { for name, endpoint in aws_vpc_endpoint.interface : name => endpoint.id }
}

output "vpc_endpoint_security_group_id" {
  description = "Security group ID attached to interface VPC endpoints, or null when endpoint creation is disabled"
  value       = try(aws_security_group.vpc_endpoints[0].id, null)
}

output "worker_log_group" {
  description = "CloudWatch log group for worker tasks"
  value       = aws_cloudwatch_log_group.worker.name
}

output "aws_region" {
  description = "AWS region for deployed resources"
  value       = var.aws_region
}
