variable "poc_enabled" {
  description = "Explicit feature gate. False plans an empty POC root."
  type        = bool
  default     = false
}

variable "api_key_auth_enabled" {
  description = "Independent personal API-key POC gate. Requires the disposable Cognito POC and accepts synthetic test keys only."
  type        = bool
  default     = false

  validation {
    condition     = !var.api_key_auth_enabled || var.poc_enabled
    error_message = "api_key_auth_enabled requires poc_enabled so only disposable Cognito humans can manage POC keys."
  }
}

variable "creation_confirmation" {
  description = "Apply guard. Set only after the isolated account, state, names, tags, caps, and plan have been reviewed."
  type        = string
  default     = "offline-only"
  sensitive   = true
}

variable "aws_region" {
  description = "Sandbox AWS region selected by the operator."
  type        = string
  default     = "us-east-1"

  validation {
    condition     = can(regex("^[a-z]{2}(-[a-z]+)+-[0-9]+$", var.aws_region))
    error_message = "aws_region must be an AWS region name."
  }
}

variable "poc_suffix" {
  description = "Unique lowercase suffix used in every POC resource name and the PocId tag."
  type        = string
  default     = "replace-me"

  validation {
    condition     = can(regex("^[a-z0-9][a-z0-9-]{2,20}$", var.poc_suffix))
    error_message = "poc_suffix must be 3-21 lowercase letters, digits, or hyphens."
  }
}

variable "poc_owner" {
  description = "Non-secret team or operator label for disposal ownership."
  type        = string
  default     = "replace-with-owner"

  validation {
    condition     = length(trimspace(var.poc_owner)) >= 3
    error_message = "poc_owner must identify the teardown owner."
  }
}

variable "cost_center" {
  description = "Non-secret sandbox cost-allocation label."
  type        = string
  default     = "replace-with-cost-center"

  validation {
    condition     = length(trimspace(var.cost_center)) >= 3
    error_message = "cost_center must be a nonblank sandbox cost label."
  }
}

variable "lambda_zip_path" {
  description = "Candidate context-service Lambda zip built from a committed revision."
  type        = string
  default     = "./artifacts/spur-context-service-poc.zip"

  validation {
    condition     = endswith(var.lambda_zip_path, ".zip")
    error_message = "lambda_zip_path must name the committed candidate Lambda zip."
  }
}

variable "api_key_authorizer_zip_path" {
  description = "Candidate API-key authorizer zip built from a committed revision."
  type        = string
  default     = "./artifacts/spur-context-api-key-authorizer-poc.zip"

  validation {
    condition     = endswith(var.api_key_authorizer_zip_path, ".zip")
    error_message = "api_key_authorizer_zip_path must name a zip artifact."
  }
}

variable "api_key_cleanup_zip_path" {
  description = "Candidate API-key cleanup zip built from a committed revision."
  type        = string
  default     = "./artifacts/spur-context-api-key-cleanup-poc.zip"

  validation {
    condition     = endswith(var.api_key_cleanup_zip_path, ".zip")
    error_message = "api_key_cleanup_zip_path must name a zip artifact."
  }
}

variable "human_callback_urls" {
  description = "Exact loopback callbacks for the disposable public PKCE client."
  type        = set(string)
  default     = ["http://127.0.0.1:8765/callback"]

  validation {
    condition = (
      contains(var.human_callback_urls, "http://127.0.0.1:8765/callback") &&
      alltrue([
        for value in var.human_callback_urls : can(regex("^http://(127\\.0\\.0\\.1|localhost)(:[0-9]+)?/", value))
      ])
    )
    error_message = "POC human callbacks must include the exact CLI callback http://127.0.0.1:8765/callback and contain only explicit loopback HTTP URLs."
  }
}

variable "human_logout_urls" {
  description = "Exact loopback logout URLs for the disposable public PKCE client."
  type        = set(string)
  default     = ["http://127.0.0.1:8765/logout"]

  validation {
    condition = length(var.human_logout_urls) > 0 && alltrue([
      for value in var.human_logout_urls : can(regex("^http://(127\\.0\\.0\\.1|localhost)(:[0-9]+)?/", value))
    ])
    error_message = "POC human logout URLs must be explicit loopback HTTP URLs."
  }
}

variable "legacy_authorization_type" {
  description = "POC-only compatibility route authorization. AWS_IAM is the safe live default; NONE is for mock fixtures only."
  type        = string
  default     = "AWS_IAM"

  validation {
    condition     = contains(["AWS_IAM", "NONE"], var.legacy_authorization_type)
    error_message = "legacy_authorization_type must be AWS_IAM or NONE."
  }
}

variable "allow_anonymous_mutations" {
  description = "POC-only anonymous-internal compatibility switch. Keep false for a live POC."
  type        = bool
  default     = false

  validation {
    condition     = !var.allow_anonymous_mutations || var.legacy_authorization_type == "NONE"
    error_message = "allow_anonymous_mutations requires the POC-only NONE compatibility route."
  }
}

variable "emergency_deny_client_ids" {
  description = "Restricted non-secret POC client IDs rejected by Lambda semantic validation."
  type        = set(string)
  default     = []
  sensitive   = true

  validation {
    condition     = alltrue([for value in var.emergency_deny_client_ids : length(trimspace(value)) > 0])
    error_message = "emergency_deny_client_ids cannot contain blank entries."
  }
}
