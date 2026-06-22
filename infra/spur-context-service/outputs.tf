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
