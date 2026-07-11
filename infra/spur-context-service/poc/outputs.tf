output "poc_id" {
  description = "Unique tag value used to inventory and verify teardown."
  value       = var.poc_enabled ? var.poc_suffix : null
}

output "cognito_issuer" {
  description = "POC Cognito issuer; null when the safety gate is disabled."
  value       = var.poc_enabled ? local.cognito_issuer : null
}

output "cognito_domain_url" {
  description = "POC hosted-domain base URL; null when disabled."
  value       = var.poc_enabled ? "https://${aws_cognito_user_pool_domain.poc[0].domain}.auth.${var.aws_region}.amazoncognito.com" : null
}

output "human_client_id" {
  description = "Public PKCE client ID. This is an identifier, not a credential."
  value       = var.poc_enabled ? aws_cognito_user_pool_client.human[0].id : null
}

output "m2m_client_ids" {
  description = "Confidential app-client IDs only. Generated secret values are intentionally never output."
  value       = { for key, client in aws_cognito_user_pool_client.m2m : key => client.id }
}

output "oauth_api_url" {
  description = "Exact JWT-protected POC route."
  value       = var.poc_enabled ? "${aws_apigatewayv2_api.poc[0].api_endpoint}/mcp/oauth" : null
}

output "lambda_alias_arn" {
  description = "Dedicated disposable Lambda alias used by the POC API."
  value       = var.poc_enabled ? aws_lambda_alias.poc[0].arn : null
}

output "index_jobs_table_name" {
  description = "Dedicated validation-only POC job table."
  value       = var.poc_enabled ? aws_dynamodb_table.index_jobs[0].name : null
}

output "iam_invoke_policy_arn" {
  description = "Dedicated POC API invoke policy for the IAM compatibility smoke."
  value       = var.poc_enabled ? aws_iam_policy.invoke[0].arn : null
}

output "api_key_auth_enabled" {
  description = "Whether the isolated POC includes personal API-key ingress."
  value       = local.api_key_poc_enabled
}

output "api_key_mcp_url" {
  description = "Exact synthetic-test-key MCP route; null when independently disabled."
  value       = local.api_key_poc_enabled ? "${aws_apigatewayv2_api.poc[0].api_endpoint}/mcp/api-key" : null
}

output "api_key_management_url" {
  description = "Cognito-human-only personal-key management base URL; null when independently disabled."
  value       = local.api_key_poc_enabled ? "${aws_apigatewayv2_api.poc[0].api_endpoint}/auth/api-keys" : null
}

output "api_key_table_name" {
  description = "Dedicated disposable API-key table name; null when independently disabled."
  value       = local.api_key_poc_enabled ? aws_dynamodb_table.api_keys[0].name : null
}

output "api_key_authorizer_function_name" {
  description = "Dedicated disposable request-authorizer function name; null when disabled."
  value       = local.api_key_poc_enabled ? aws_lambda_function.api_key_authorizer[0].function_name : null
}

output "api_key_cleanup_function_name" {
  description = "Dedicated disposable expiry-cleanup function name; null when disabled."
  value       = local.api_key_poc_enabled ? aws_lambda_function.api_key_cleanup[0].function_name : null
}
