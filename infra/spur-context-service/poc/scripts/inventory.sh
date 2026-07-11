#!/bin/sh
# Capture a read-only, sanitized inventory of resources belonging to one POC ID.
# Requires explicitly configured sandbox credentials when used by an operator.
set -eu

if [ "$#" -ne 3 ]; then
  echo "usage: $0 AWS_PROFILE AWS_REGION POC_SUFFIX" >&2
  exit 2
fi

profile=$1
region=$2
poc_id=$3

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

is_valid_poc_suffix "$poc_id" || {
  echo "invalid POC suffix: expected 3-21 lowercase letters, digits, or hyphens" >&2
  exit 2
}

prefix="spur-context-auth-poc-${poc_id}"

for command in aws jq; do
  command -v "$command" >/dev/null 2>&1 || {
    echo "$command is required" >&2
    exit 2
  }
done

aws_cmd() {
  aws --no-cli-pager --profile "$profile" --region "$region" "$@"
}

tagged=$(aws_cmd resourcegroupstaggingapi get-resources \
  --tag-filters "Key=PocId,Values=${poc_id}" \
  --query 'ResourceTagMappingList[].ResourceARN' --output json)
pool_ids=$(aws_cmd cognito-idp list-user-pools --max-results 60 \
  --query "UserPools[?Name=='${prefix}'].Id" --output json)
domain=$(aws_cmd cognito-idp describe-user-pool-domain --domain "$prefix" \
  --query 'DomainDescription.Domain' --output json 2>/dev/null || printf 'null')
domains=$(jq -cn --argjson value "$domain" 'if $value == null then [] else [$value] end')
apis=$(aws_cmd apigatewayv2 get-apis \
  --query "Items[?Name=='${prefix}'].ApiId" --output json)
functions=$(aws_cmd lambda list-functions \
  --query "Functions[?starts_with(FunctionName, '${prefix}')].FunctionName" --output json)
api_key_cleanup_functions=$(aws_cmd lambda list-functions \
  --query "Functions[?FunctionName=='${prefix}-api-key-cleanup'].FunctionName" --output json)
tables=$(aws_cmd dynamodb list-tables \
  --query "TableNames[?@=='${prefix}-jobs' || @=='${prefix}-api-keys']" --output json)
api_key_tables=$(aws_cmd dynamodb list-tables \
  --query "TableNames[?@=='${prefix}-api-keys']" --output json)
logs=$(aws_cmd logs describe-log-groups --log-group-name-prefix "/aws/" \
  --query "logGroups[?contains(logGroupName, '${prefix}')].logGroupName" --output json)
roles=$(aws_cmd iam list-roles \
  --query "Roles[?starts_with(RoleName, '${prefix}-')].RoleName" --output json)
policies=$(aws_cmd iam list-policies --scope Local \
  --query "Policies[?PolicyName=='${prefix}-invoke'].PolicyName" --output json)
cleanup_rules=$(aws_cmd events list-rules --name-prefix "${prefix}-api-key-cleanup" \
  --query 'Rules[].Name' --output json)

resource_servers='[]'
clients='[]'
versions='[]'
aliases='[]'
api_key_authorizers='[]'
cleanup_targets='[]'
lambda_resource_policies='[]'
for pool_id in $(printf '%s' "$pool_ids" | jq -r '.[]'); do
  current_servers=$(aws_cmd cognito-idp list-resource-servers --user-pool-id "$pool_id" \
    --query 'ResourceServers[].Identifier' --output json)
  resource_servers=$(jq -cn --argjson left "$resource_servers" --argjson right "$current_servers" '$left + $right')
  current_clients=$(aws_cmd cognito-idp list-user-pool-clients --user-pool-id "$pool_id" \
    --query 'UserPoolClients[].ClientId' --output json)
  clients=$(jq -cn --argjson left "$clients" --argjson right "$current_clients" '$left + $right')
done
for api_id in $(printf '%s' "$apis" | jq -r '.[]'); do
  current_authorizers=$(aws_cmd apigatewayv2 get-authorizers --api-id "$api_id" \
    --query "Items[?contains(Name, 'api-key')].Name" --output json)
  api_key_authorizers=$(jq -cn --argjson left "$api_key_authorizers" --argjson right "$current_authorizers" '$left + $right')
done
for function_name in $(printf '%s' "$functions" | jq -r '.[]'); do
  current_versions=$(aws_cmd lambda list-versions-by-function --function-name "$function_name" \
    --query 'Versions[?Version!=`$LATEST`].Version' --output json)
  versions=$(jq -cn --argjson left "$versions" --argjson right "$current_versions" '$left + $right')
  current_aliases=$(aws_cmd lambda list-aliases --function-name "$function_name" \
    --query 'Aliases[].Name' --output json)
  aliases=$(jq -cn --argjson left "$aliases" --argjson right "$current_aliases" '$left + $right')
  if aws_cmd lambda get-policy --function-name "$function_name" --output json >/dev/null 2>&1; then
    lambda_resource_policies=$(jq -cn --argjson values "$lambda_resource_policies" --arg value "$function_name" '$values + [$value]')
  fi
done
for rule_name in $(printf '%s' "$cleanup_rules" | jq -r '.[]'); do
  current_targets=$(aws_cmd events list-targets-by-rule --rule "$rule_name" \
    --query 'Targets[].Id' --output json)
  cleanup_targets=$(jq -cn --argjson left "$cleanup_targets" --argjson right "$current_targets" '$left + $right')
done

jq -n \
  --arg poc_id "$poc_id" \
  --argjson tagged "$tagged" \
  --argjson pools "$pool_ids" \
  --argjson domains "$domains" \
  --argjson servers "$resource_servers" \
  --argjson clients "$clients" \
  --argjson apis "$apis" \
  --argjson api_key_authorizers "$api_key_authorizers" \
  --argjson functions "$functions" \
  --argjson api_key_cleanup_functions "$api_key_cleanup_functions" \
  --argjson versions "$versions" \
  --argjson aliases "$aliases" \
  --argjson lambda_resource_policies "$lambda_resource_policies" \
  --argjson tables "$tables" \
  --argjson api_key_tables "$api_key_tables" \
  --argjson cleanup_rules "$cleanup_rules" \
  --argjson cleanup_targets "$cleanup_targets" \
  --argjson logs "$logs" \
  --argjson roles "$roles" \
  --argjson policies "$policies" \
  '{
    poc_id: $poc_id,
    tagged_resources: $tagged,
    cognito_user_pools: $pools,
    cognito_domains: $domains,
    cognito_resource_servers: $servers,
    cognito_app_clients: $clients,
    api_gateway_apis: $apis,
    api_key_authorizers: $api_key_authorizers,
    lambda_functions: $functions,
    api_key_cleanup_functions: $api_key_cleanup_functions,
    lambda_versions: $versions,
    lambda_aliases: $aliases,
    lambda_resource_policies: $lambda_resource_policies,
    dynamodb_tables: $tables,
    api_key_tables: $api_key_tables,
    api_key_cleanup_rules: $cleanup_rules,
    api_key_cleanup_targets: $cleanup_targets,
    cloudwatch_log_groups: $logs,
    iam_roles: $roles,
    iam_policies: $policies
  }'
