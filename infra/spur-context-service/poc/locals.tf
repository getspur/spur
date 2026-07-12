locals {
  name_prefix = "spur-context-auth-poc-${var.poc_suffix}"

  api_key_poc_enabled = var.poc_enabled && var.api_key_auth_enabled

  common_tags = {
    Service     = "spur-context-auth-poc"
    Environment = "poc-sandbox"
    ManagedBy   = "terraform"
    Owner       = var.poc_owner
    CostCenter  = var.cost_center
    PocId       = var.poc_suffix
    Issue       = "bd-2hv5u"
  }

  resource_server_identifier = "urn:spur:context-service"
  external_scope_descriptions = {
    "external.read"   = "Read external context data"
    "external.index"  = "Submit validation-only external index requests"
    "external.status" = "Read status owned by the authenticated caller"
  }
  custom_scope_descriptions = merge(
    local.external_scope_descriptions,
    local.api_key_poc_enabled ? {
      "keys.manage" = "Create, list, and revoke personal API keys"
    } : {},
  )
  custom_scopes = [
    for suffix in sort(keys(local.custom_scope_descriptions)) :
    "${local.resource_server_identifier}/${suffix}"
  ]
  external_scopes = [
    for suffix in sort(keys(local.external_scope_descriptions)) :
    "${local.resource_server_identifier}/${suffix}"
  ]
  api_key_management_scope = "${local.resource_server_identifier}/keys.manage"
  human_scopes             = concat(["openid", "email"], local.custom_scopes)
  m2m_clients = {
    read-status = {
      scopes = ["external.read", "external.status"]
    }
    index-only = {
      scopes = ["external.index"]
    }
  }

  cognito_issuer = var.poc_enabled ? "https://cognito-idp.${var.aws_region}.amazonaws.com/${aws_cognito_user_pool.poc[0].id}" : ""

  # Zero is deliberate: external_index can reach application validation but
  # cannot enqueue or dispatch a worker or Step Functions execution.
  index_max_running_jobs_global = 0
  index_max_queued_jobs_global  = 0

  api_key_supported_active_keys          = 500000
  api_key_default_ttl_days               = 90
  api_key_cleanup_schedule_minutes       = 5
  api_key_cleanup_max_buckets            = 4
  api_key_cleanup_max_pages              = 8
  api_key_cleanup_max_records            = 100
  api_key_cleanup_page_limit             = 100
  api_key_steady_state_expiries_per_hour = ceil(local.api_key_supported_active_keys / (local.api_key_default_ttl_days * 24))
  api_key_cleanup_invocations_per_hour   = floor(60 / local.api_key_cleanup_schedule_minutes)
  api_key_cleanup_capacity_per_hour      = local.api_key_cleanup_invocations_per_hour * min(local.api_key_cleanup_max_records, local.api_key_cleanup_page_limit)
}
