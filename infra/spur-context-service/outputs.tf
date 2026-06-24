output "api_url" {
  description = "HTTP API endpoint for the context service"
  value       = aws_apigatewayv2_api.http.api_endpoint
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

output "worker_log_group" {
  description = "CloudWatch log group for worker tasks"
  value       = aws_cloudwatch_log_group.worker.name
}

output "aws_region" {
  description = "AWS region for deployed resources"
  value       = var.aws_region
}
