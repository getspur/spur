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
  --query "Functions[?FunctionName=='${prefix}'].FunctionName" --output json)
tables=$(aws_cmd dynamodb list-tables \
  --query "TableNames[?@=='${prefix}-jobs']" --output json)
logs=$(aws_cmd logs describe-log-groups --log-group-name-prefix "/aws/" \
  --query "logGroups[?contains(logGroupName, '${prefix}')].logGroupName" --output json)
roles=$(aws_cmd iam list-roles \
  --query "Roles[?RoleName=='${prefix}-lambda'].RoleName" --output json)
policies=$(aws_cmd iam list-policies --scope Local \
  --query "Policies[?PolicyName=='${prefix}-invoke'].PolicyName" --output json)

resource_servers='[]'
clients='[]'
versions='[]'
aliases='[]'
for pool_id in $(printf '%s' "$pool_ids" | jq -r '.[]'); do
  current_servers=$(aws_cmd cognito-idp list-resource-servers --user-pool-id "$pool_id" \
    --query 'ResourceServers[].Identifier' --output json)
  resource_servers=$(jq -cn --argjson left "$resource_servers" --argjson right "$current_servers" '$left + $right')
  current_clients=$(aws_cmd cognito-idp list-user-pool-clients --user-pool-id "$pool_id" \
    --query 'UserPoolClients[].ClientId' --output json)
  clients=$(jq -cn --argjson left "$clients" --argjson right "$current_clients" '$left + $right')
done
for function_name in $(printf '%s' "$functions" | jq -r '.[]'); do
  current_versions=$(aws_cmd lambda list-versions-by-function --function-name "$function_name" \
    --query 'Versions[?Version!=`$LATEST`].Version' --output json)
  versions=$(jq -cn --argjson left "$versions" --argjson right "$current_versions" '$left + $right')
  current_aliases=$(aws_cmd lambda list-aliases --function-name "$function_name" \
    --query 'Aliases[].Name' --output json)
  aliases=$(jq -cn --argjson left "$aliases" --argjson right "$current_aliases" '$left + $right')
done

jq -n \
  --arg poc_id "$poc_id" \
  --argjson tagged "$tagged" \
  --argjson pools "$pool_ids" \
  --argjson domains "$domains" \
  --argjson servers "$resource_servers" \
  --argjson clients "$clients" \
  --argjson apis "$apis" \
  --argjson functions "$functions" \
  --argjson versions "$versions" \
  --argjson aliases "$aliases" \
  --argjson tables "$tables" \
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
    lambda_functions: $functions,
    lambda_versions: $versions,
    lambda_aliases: $aliases,
    dynamodb_tables: $tables,
    cloudwatch_log_groups: $logs,
    iam_roles: $roles,
    iam_policies: $policies
  }'
