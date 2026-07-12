#!/bin/sh
# Verify a sanitized inventory captured by inventory.sh is structurally complete
# and empty. This script performs no AWS operations.
set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: $0 EXPECTED_POC_ID INVENTORY_JSON" >&2
  exit 2
fi

expected_poc_id=$1
inventory_json=$2

is_valid_poc_suffix() {
  value=$1
  length=${#value}
  [ "$length" -ge 3 ] && [ "$length" -le 21 ] || return 1
  case "$value" in
    [a-z0-9]*) ;;
    *) return 1 ;;
  esac
  case "$value" in
    *[!a-z0-9-]*) return 1 ;;
  esac
}

is_valid_poc_suffix "$expected_poc_id" || {
  echo "invalid POC suffix: expected 3-21 lowercase letters, digits, or hyphens" >&2
  exit 2
}

command -v jq >/dev/null 2>&1 || {
  echo "jq is required" >&2
  exit 2
}

required='[
  "tagged_resources",
  "cognito_user_pools",
  "cognito_domains",
  "cognito_resource_servers",
  "cognito_app_clients",
  "api_gateway_apis",
  "api_key_authorizers",
  "lambda_functions",
  "api_key_cleanup_functions",
  "lambda_versions",
  "lambda_aliases",
  "lambda_resource_policies",
  "dynamodb_tables",
  "api_key_tables",
  "api_key_cleanup_rules",
  "api_key_cleanup_targets",
  "cloudwatch_log_groups",
  "iam_roles",
  "iam_policies"
]'

jq -e --arg expected_poc_id "$expected_poc_id" --argjson required "$required" '
  if (.poc_id | type) != "string" then
      error("inventory poc_id must be a string")
    elif .poc_id != $expected_poc_id then
      error("inventory poc_id does not match reviewed expected POC ID")
    else
      .
    end
  | ($required - keys) as $missing
  | if ($missing | length) > 0 then
      error("inventory missing categories: \($missing | join(", "))")
    else
      .
    end
  | [
      to_entries[]
      | select(.key as $key | $required | index($key))
      | select((.value | type) != "array")
      | .key
    ] as $invalid_types
  | if ($invalid_types | length) > 0 then
      error("inventory categories must be arrays: \($invalid_types | join(", "))")
    else
      .
    end
  | [to_entries[] | select(.key as $key | $required | index($key)) | select(.value | length > 0)] as $remaining
  | if ($remaining | length) > 0 then
      error("POC resources remain: \($remaining | map(.key) | join(", "))")
    else
      true
    end
' "$inventory_json" >/dev/null

echo "teardown verified: every inventory category is empty for reviewed POC ID $expected_poc_id"
