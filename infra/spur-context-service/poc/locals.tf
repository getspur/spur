locals {
  name_prefix = "spur-context-auth-poc-${var.poc_suffix}"

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
  custom_scope_descriptions = {
    "external.read"   = "Read external context data"
    "external.index"  = "Submit validation-only external index requests"
    "external.status" = "Read status owned by the authenticated caller"
  }
  custom_scopes = [
    for suffix in sort(keys(local.custom_scope_descriptions)) :
    "${local.resource_server_identifier}/${suffix}"
  ]
  human_scopes = concat(["openid", "email"], local.custom_scopes)
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
}
