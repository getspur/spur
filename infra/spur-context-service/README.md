# spur-context-service Terraform

Infrastructure for the DuckLake-served code context MCP service on AWS Lambda,
plus the on-demand indexing worker control plane.

## Architecture

```
Client → API Gateway (HTTP, AWS_IAM) → Lambda (ARM64, provided.al2023)
                                ├── DuckDB + DuckLake reads
                                │       ↓
                                │   S3 (.ducklake catalog + Parquet data via httpfs)
                                ├── DynamoDB job/status/dedupe/quota records
                                ├── scheduled queue drainer → Step Functions
                                └── Step Functions DescribeExecution repair

external_index → bounded DynamoDB queue → drainer → Step Functions → source fetcher Lambda
                                                   │          │
                                                   │          └── public HTTPS/git source
                                                   │              → S3 fetch artifact
                                                   │              → presigned HTTPS URL
                                                   ↓
                                            Lambda worker
                                                   │
                                                   └── ECS worker fallback
                                                       for Lambda platform
                                                       failures/timeouts
                                                   ↓
                                    DuckLake/S3 package data writes
                                    under DynamoDB catalog lease
```

DuckLake/S3 is the data plane for indexed package rows, refs, catalog metadata,
and Parquet files. DynamoDB is the production control plane for job status,
idempotency, execution ARNs, bounded queued/running quotas, running-release
tokens, and catalog write leases. EventBridge invokes the queue drainer on a
fixed schedule; the post-admission kick reduces latency but is not required for
eventual dispatch.

The source fetcher Lambda is deliberately outside the worker VPC. Public GitHub
and non-S3 HTTPS tarball inputs take the fetch-split path: Step Functions invokes
the fetcher, the fetcher validates and downloads the public source, normalizes it
to `source.tar.gz`, stages it under `s3://<bucket>/fetch/<job_id>/source.tar.gz`,
and returns a presigned HTTPS S3 URL. The worker backends then consume that URL
as a normal tarball. Raw `s3://` URLs are not part of the handoff contract.

The indexing worker Lambda and ECS fallback run inside `worker_subnets`. The
default network model is NAT-free for AWS service access: Terraform creates S3
and DynamoDB gateway endpoints on `worker_route_table_ids`, plus private-DNS
interface endpoints for Step Functions, Secrets Manager, ECR API, ECR Docker
registry, CloudWatch Logs, and STS. Interface endpoint ENIs use
`interface_vpc_endpoint_subnet_ids` when set, otherwise they fall back to the
worker subnets. `interface_vpc_endpoint_service_keys` can reduce the interface
service set; low-cost Lambda-worker stacks use only `states` and
`secretsmanager`, while private ECS fallback stacks without NAT should keep
`ecr_api`, `ecr_dkr`, `logs`, and `sts` too. The interface endpoint security
group accepts HTTPS only from the worker security group. Operators who already
provide NAT or equivalent shared endpoints can set
`create_vpc_endpoints=false`.

VPC endpoints do not provide arbitrary public internet egress. Presigned S3
HTTPS tarballs already staged in S3 skip the fetcher (`prefetch_source=false`)
and go directly to the worker through the S3 endpoint; public GitHub and generic
internet tarballs use `FetchSource` first.

The deployed ECS fallback image includes both `/usr/local/bin/spur-context-worker`
and `/usr/local/bin/spur`; `deploy.sh` smoke-tests both before Terraform applies
the task definition. The Lambda fast-path image includes
`/usr/local/bin/spur-context-worker-lambda` plus `/usr/local/bin/spur` and is
invoked first by Step Functions. `external_index_status` repairs stale
queued/running jobs through Step Functions `DescribeExecution` when an execution
ARN is present.

## Resources

| Resource | Purpose |
|---|---|
| `aws_s3_bucket.data` | DuckLake catalog (.ducklake) + Parquet data files |
| `aws_dynamodb_table.index_jobs` | Job records, sparse queue GSI, active dedupe pointers, owner/global quota counters, running-release tokens, execution ARNs |
| `aws_dynamodb_table.catalog_leases` | Serialized DuckLake catalog write leases |
| `aws_lambda_function.service` | ARM64 Lambda, 1024MB, 30s timeout |
| `aws_sfn_state_machine.index_build` | Lambda-first on-demand indexing orchestration |
| `aws_lambda_function.worker` | Fast-start indexing worker image |
| `aws_lambda_function.source_fetcher` | Non-VPC public source fetcher image |
| `aws_ecs_cluster.indexing` / task definition | Fargate fallback worker runtime |
| `aws_apigatewayv2_api.http` | HTTP API front door |
| `aws_cloudwatch_event_rule.index_queue_drainer` | Scheduled correctness path for dispatching queued jobs |
| `aws_iam_policy.context_service_invoke` | Same-account SigV4 invoke policy for allowed callers |
| `aws_iam_role.lambda` | Execution role with S3 read, DynamoDB, SFN + CloudWatch Logs |
| `aws_iam_role.worker_task` | ECS fallback worker role with S3, DynamoDB, SFN callback permissions |
| `aws_cloudwatch_log_group.lambda` | 14-day retention |
| `aws_cloudwatch_log_group.worker_lambda` | Lambda worker logs |
| `aws_cloudwatch_log_group.source_fetcher_lambda` | Source fetcher Lambda logs |
| `aws_cloudwatch_log_group.worker` | Worker task logs |
| `aws_vpc_endpoint.gateway` | S3 and DynamoDB gateway endpoints for worker route tables |
| `aws_vpc_endpoint.interface` | Private-DNS endpoints for worker AWS API access |
| `aws_security_group.vpc_endpoints` | HTTPS ingress from worker tasks to interface endpoints |

## Authentication And Abuse Controls

The public API route uses API Gateway `AWS_IAM`; callers must sign requests with
SigV4 and have `execute-api:Invoke` permission. The Lambda still rejects
mutating tools (`external_index`, `external_index_status`) unless API Gateway
passes an authenticated JWT/IAM/principal identity, so an accidentally
unauthenticated route does not silently fall back to source IP identity.

`external_index` applies three layers of controls:

- API Gateway route throttling via `api_throttle_rate_limit` and
  `api_throttle_burst_limit`.
- Per authenticated caller DynamoDB-backed fixed-window rate limiting plus
  transactional per-owner/global queued and running caps. Running capacity is
  released exactly once when a job reaches a terminal state.
- Source and build caps. URL size hints are checked before job creation; workers
  revalidate URLs, cap tarball downloads/source trees, and kill `spur graph
  build` after `context_max_build_seconds`.

## Optional Cognito OAuth ingress

`cognito_auth_enabled` is `false` by default. In that mode Terraform creates no
Cognito user pool, domain, resource server, app clients, JWT authorizer,
`POST /mcp/oauth` route, or Cognito-specific access-log/metric resources. The
existing `$default` route remains exactly as configured: `NONE` for
`env/default.tfvars` and `AWS_IAM` by default elsewhere. The EventBridge queue
drainer continues to invoke the Lambda directly.

When enabled, the module adds a Cognito **LITE** user pool, hosted domain, the
`external.read`, `external.index`, and `external.status` resource-server
scopes, a public authorization-code human client, and one confidential
client-credentials client for each enabled organization. API Gateway attaches a
native JWT authorizer only to `POST /mcp/oauth`; `$default` never receives the
authorizer. The authorizer's three route scopes are an any-of edge gate. Lambda
uses the supplied non-secret issuer, client IDs, resource-server ID, denylist,
and fixed `/mcp/oauth` path to enforce the exact body-selected tool scope.

Use non-production placeholders in committed configuration. A real enabled
environment needs exact callback and logout URLs, an environment-qualified pool
name, and a unique Cognito domain prefix:

```hcl
cognito_auth_enabled        = true
cognito_user_pool_name      = "spur-context-example-cognito"
cognito_domain_prefix       = "spur-context-example-auth"
cognito_human_callback_urls = [
  "http://127.0.0.1:8765/callback", # required by `spur context auth login`
  "https://app.example.test/oauth/callback",
]
cognito_human_logout_urls   = ["https://app.example.test/logout"]

cognito_m2m_organizations = {
  example_org = {
    display_name       = "Example organization"
    enabled            = true
    allowed_scopes     = ["external.index", "external.status"]
    access_token_hours = 6
    risk_acceptance    = null
  }
}
```

Callback and logout URLs must be exact HTTPS URLs. HTTP is accepted only for
`localhost`, `127.0.0.1`, or `[::1]` loopback POC URLs; wildcards are rejected.
When personal API-key authentication is enabled, the callback set must include
exactly `http://127.0.0.1:8765/callback` for the CLI. If port 8765 is occupied,
the CLI fails before launching Cognito and reports that the registered port must
be made available.
M2M scopes are limited to the three listed scope suffixes, and one human client
plus all enabled organizations may not exceed API Gateway's 50 JWT audiences.
The balanced M2M default is six hours. An enabled 24-hour organization must
provide `risk_acceptance.accepted_by`, RFC3339 `accepted_at`, and `ticket`
metadata to make its bearer-token replay risk reviewable.

M2M app clients use `generate_secret = true`, so Terraform state can contain a
generated `client_secret` even though no output exposes it. Treat remote state
as secret-bearing: encrypt it, restrict IAM, retain access logs, and never
publish plan/state artifacts. Do not put app-client secrets, tokens, real
client IDs, authorization codes, PKCE values, or real subscriber emails in
tfvars. The only Cognito Lambda environment values are non-secret validation
metadata; client secrets are never passed to the Lambda. API Gateway access
logs contain only route/status/latency/bounded-error fields and intentionally
omit headers, claims, request bodies, and credentials.

Set `cognito_monthly_budget_usd` plus one or more
`cognito_budget_subscriber_emails` to create the optional Cognito forecast
budget. The subscriber variable is marked sensitive because it contains contact
data. Enabled Cognito also creates a plan-only-safe OAuth-route 5xx metric and
alarm from the redacted API access log.

Run the local mock-provider plan tests after Terraform changes; every run uses
`command = plan` and never creates AWS resources:

```bash
terraform -chdir=infra/spur-context-service test \
  -test-directory=tests \
  -filter=tests/cognito_static.tftest.hcl
```

## Cognito operator runbook

The steps below describe an approved deployment lifecycle. They are not
authorization to apply this module. Repository verification uses mock-provider
plans and `init -backend=false`; it never needs AWS credentials.

### Enablement and discovery

1. Keep `cognito_auth_enabled=false` while configuring exact callback/logout
   URLs, the environment-qualified pool/domain names, organization scopes and
   TTLs, budget recipients, and the emergency denylist. Confirm the existing
   `$default` route remains `AWS_IAM` (or the explicitly reviewed demo `NONE`),
   and retain the EventBridge drainer configuration.
2. Run `terraform fmt -check -recursive`, `terraform init -backend=false`,
   `terraform validate`, and the mock-provider tests. Review a real remote-state
   plan only in the separately approved environment; enabling Cognito must add
   the exact `POST /mcp/oauth` route without replacing `$default`.
3. After an approved apply, distribute only the nonsensitive discovery outputs:
   `cognito_issuer`, `cognito_domain_url`, `cognito_human_client_id`,
   `cognito_m2m_client_ids`, `cognito_resource_server_identifier`, and
   `oauth_api_url`. Use `cognito_issuer` for OIDC discovery and JWKS
   (`/.well-known/openid-configuration` and `/.well-known/jwks.json`). Use
   `cognito_domain_url` for hosted `/oauth2/authorize`, `/oauth2/token`, and
   logout endpoints. No output contains an M2M secret.
4. Smoke one least-privilege human/M2M call on `/mcp/oauth`, then smoke the
   unchanged IAM route. Verify that OAuth identities are namespaced, IAM issued
   no Cognito token, cross-owner status is `not_found`, and queue/drainer health
   did not regress before onboarding another organization.

### Credential delivery and rotation

Generated M2M secrets can be present in Terraform state. A restricted
provisioning step must copy a new secret directly from Cognito/provider state
into the approved secret manager without printing it, placing it in a plan
artifact, shell history, email, chat, or issue comment. Deliver the organization
its stable client ID, exact allowed scopes, TTL, and an audited one-time secret
reference; revoke delivery access after acknowledgment. The human client is
public and receives no secret.

Rotate without changing ownership:

1. Add a second secret to the existing app client and write it to the approved
   secret manager.
2. Have the customer deploy the new reference and prove it can mint a token and
   invoke only an allowed scope.
3. Observe a full agreed overlap with no old-secret issuance, then delete the
   old secret. Preserve the app client ID so queued jobs and status ownership
   remain `cognito:client:<client_id>`.
4. Record actor, timestamps, client-ID hash, and secret descriptor ID, never the
   secret value. If compromise is suspected, denylist the client ID as part of
   the same incident response.

### TTL and audience risk gates

The balanced default is a six-hour M2M token. A 24-hour token enlarges the
bearer replay and non-immediate-revocation window to as much as 24 hours and
requires the committed risk-acceptance metadata. Secret deletion stops new
issuance but does not invalidate an already issued access token; shorten the TTL
or deploy the emergency denylist when that residual window is unacceptable.

One human client plus enabled organization clients must fit API Gateway's fixed
50-audience limit, so this route supports at most 49 enabled M2M organizations.
Alert before that boundary and select a reviewed sharding/authorizer design
before onboarding another organization; never truncate the audience list.

### Monitoring

Monitor the redacted OAuth access log and alarms for route-specific
401/403/429 rates, JWT 401 spikes during key rotation, OAuth/Lambda 5xx, and
sustained throttling. Correlate those with Cognito token operations and budget
forecasts, per-owner/global queue saturation, dedupe outcomes, dispatch latency,
scheduled-drainer invocations, retries, and stuck jobs. The module provides the
OAuth-route 5xx alarm and optional 50/80/100% forecast budget notifications;
the operating dashboard must retain the existing queue and Lambda signals.
Never add Authorization headers, claims, client IDs, subjects, request bodies,
secrets, or tokens to access-log formats or alert payloads.

### Rollback

For one compromised organization, add its client ID to the Lambda denylist and
delete/rotate its secret to stop new issuance. For broader rollback, prepare a
reviewed configuration deployment that deny-lists the human client ID and every
enabled M2M client ID; this fails all OAuth callers closed while the unchanged
IAM `$default` route remains the internal path. If the route itself must be
removed, use a reviewed incident change that removes only `POST /mcp/oauth` and
keeps the user pool/logs retained. Do not use `cognito_auth_enabled=false` as a
fast rollback: in the current module that is a teardown request for all guarded
Cognito resources and deletion protection will block it. The demo route, when
intentionally enabled, retains the literal `anonymous-internal` owner. Do not
rewrite queue records: existing namespaced jobs remain available after the same
client is restored or through an audited IAM operator path. Preserve logs and
the user pool through the maximum token TTL and incident-retention window.

### Teardown

Stop onboarding and token issuance, wait through the maximum issued-token TTL,
and wait for active namespaced jobs to become terminal (or record an audited IAM
operator disposition). Capture a resource inventory and a destroy plan before
changing state. Because production defaults to deletion protection, first apply
a separately reviewed update with
`cognito_user_pool_deletion_protection=false` while Cognito remains enabled;
then set `cognito_auth_enabled=false` and review that the next plan removes only
the Cognito clients/pool/domain, JWT authorizer/route, OAuth log/alarm, and
optional budget. It must not remove `$default`, the service Lambda, job table,
EventBridge schedule, or drainer permissions.

After an approved destroy, verify the Cognito/JWT outputs are null/empty, the
targeted resources are absent, `$default` still has its expected authorization,
the scheduled drainer remains configured, and no secret-bearing plan/state/log
artifact was retained outside the state policy.

## Personal API-key operator runbook

Personal keys are additive to Cognito OAuth/M2M, IAM, the explicit demo path,
and scheduled EventBridge events. The procedures below describe an approved
deployment lifecycle; they are not authorization to apply or destroy resources.
Repository verification is offline and uses Terraform mock plans only.

### API-key enablement and discovery

Keep `api_key_auth_enabled=false` until Cognito human OAuth is enabled and its
runbook checks pass. Configure the API-key table, authorizer/cleanup artifacts,
30-second authorizer cache, 90-day default TTL, cleanup bounds, alarms, and
budget evidence, then run formatting, validation, and mock tests. An approved
plan with `api_key_auth_enabled=true` must add exactly:

- `GET /.well-known/spur-context-service` as public bounded discovery;
- `POST /mcp/api-key` with the request authorizer;
- the three JWT + `keys.manage` management routes; and
- the dedicated table, authorizer, cleanup schedule, IAM, logs, and alarms.

It must leave `POST /mcp/oauth`, `$default`, M2M clients, IAM/demo settings, and
the queue-drainer EventBridge input unchanged. After a separately approved
apply, publish only discovery outputs and verify that the document's issuer,
human client ID, endpoints, scopes, feature status, and exact URLs match the
reviewed environment. Discovery contains no account ID, client secret, token,
key, digest, ARN, or user data.

### CLI-managed personal keys

Use human OAuth only for management and a personal key for routine MCP traffic:

```bash
spur context auth login --profile workstation --url https://context.example.test
spur context key create --name workstation --scope external.read --profile workstation
spur context key list --profile workstation
spur context key use PUBLIC_KEY_ID
spur context mcp --profile PUBLIC_KEY_ID
spur context key revoke PUBLIC_KEY_ID --profile workstation
spur context auth logout --profile workstation
```

Creation stores the one-time key without printing it. `--show-secret` is allowed
only on an interactive terminal and should be used only with an approved secure
capture path. `auth logout` removes management credentials but preserves local
API-key profiles. OAuth and every personal key for the user resolve to the same
`cognito:user:<sub>` owner, so extra keys do not create extra rate, queue,
dedupe, or status-visibility buckets.

### Headless credential delivery

Never pass a personal key as an argument. Import an approved one-time value from
stdin with `spur context key add --stdin --profile automation`, or provide it to
one process through `SPUR_CONTEXT_SERVICE_API_KEY`. The environment value has
precedence over the OS keyring and explicit restricted credential file. Use
`SPUR_CONTEXT_CREDENTIALS_FILE` only for the reviewed 0600/owner-only fallback.
Normal `.spur/config.toml` stores only URL, auth mode, profile, and optional
public-ID hint. Do not persist environment dumps, command tracing, stdin capture,
raw headers, or debug output.

### API-key revocation and emergency route kill switch

A revoke changes the key record immediately; a cache miss rejects it at once,
and cached allow/deny decisions expire within the documented 30-second revocation SLO.
Verify rejection after that window without logging the key.
For route-wide compromise, first detach or disable only `POST /mcp/api-key` and
set the serving feature flag to reject API-key context. Preserve OAuth,
management (for revocation), IAM/demo, and both scheduled EventBridge paths.
Do not wait for per-key revocation before applying this emergency route kill
switch.

### Cleanup capacity and cursor lag

The supported model is 50,000 users × ten keys with a 90-day default TTL:
500,000 / 2,160 hours rounds up to **232 keys/hour**. A five-minute schedule and
100-record invocation cap provide **1,200 records/hour**. Each invocation is
also bounded to four forward buckets, eight pages, and 100 records; a large
hour resumes through durable `has_more` state. The 168-hour horizon selects an
oldest starting bucket and is not an invocation work multiplier.

Alarm on cleanup Lambda errors and missing/breaching cursor-lag metrics at the
five-minute cadence. Investigate lag before it threatens the one-hour normal
operation SLO. Manual owner revoke releases capacity immediately; DynamoDB TTL
is delayed garbage collection, never revocation or capacity accounting.

### Owner offboarding

Run the audited IAM-only revoke-by-owner workflow before disabling or deleting a
Cognito account. Query the owner GSI, process bounded idempotent batches, retain
a resumable cursor, and record only actor, hashed owner, public key IDs, bounded
results, and timestamps. Verify zero active keys and wait through the
30-second cache bound before completing offboarding. Personal keys and M2M
credentials cannot invoke this operator workflow.

### API-key metrics and cost evidence

Monitor bounded authorizer decisions and latency, management outcomes, API-key
route 401/403/429/5xx, cleanup scanned/revoked/retried/failed counts, cursor lag,
Lambda errors, queue saturation, and Cognito/budget forecasts. Access logs must
omit both credential headers, request bodies, JWT claims, subjects, owners,
digests, and raw keys.

Calculate cost evidence per AWS price dimension: authorizer invocations,
strongly consistent reads on cache misses, lifecycle writes, cleanup
invocations, DynamoDB storage/PITR, logs, metrics, alarms, and optional budgets.
Use measured request distribution and cache-hit approximation; do not multiply
one blended estimate across unrelated dimensions.

### API-key rollback and teardown

Rollback disables the exact API-key route first, preserving management long
enough to revoke keys. Wait through the cache TTL, verify OAuth/IAM/demo and
drainer traffic, then disable the feature in a reviewed plan. Do not rewrite
queued jobs or delete the shared owner records.

For destructive teardown, revoke all owners, confirm cleanup cursor completion,
retain audit/log evidence per policy, inventory the API-key table, authorizer and
alias, cleanup function/rule/target, route/integration, permissions, roles,
policies, logs, alarms, and secret-bearing Terraform state, then review the
destroy plan. After separate approval, verify those categories are absent while
`POST /mcp/oauth`, `$default`, Cognito M2M, IAM/demo, the service Lambda, job
table, and index drainer remain unchanged. Delete retained state only under the
environment's state-retention policy.

## Bounded Backlog Operations

The module keeps queue admission opt-in: `index_max_queued_jobs_per_owner=0`
leaves durable queue admission disabled, so a cold `external_index` request is
rejected with `queue_full`. Set a finite per-owner queue cap to enable the
backlog. Global caps use `0` to mean disabled; operators enabling queueing in a
multi-owner deployment should set finite global queued and running caps as
well. The legacy `index_max_concurrent_jobs_per_caller` value remains the
per-owner running fallback when `index_max_running_jobs_per_owner` is null.

| Variable | Default | Runtime effect |
|---|---:|---|
| `index_max_queued_jobs_per_owner` | `0` | Hard queued cap per backlog owner; zero disables cold-job queue admission |
| `index_max_queued_jobs_global` | `0` | Service-wide queued cap; zero disables this optional cap |
| `index_max_running_jobs_per_owner` | `null` | Running/dispatching cap per owner; null inherits the legacy per-caller cap (`2`) |
| `index_max_running_jobs_global` | `0` | Hard global running-token cap (`1..32`); zero disables this optional cap |
| `index_queue_shard_count` | `16` | Sparse queue-GSI partitions scanned in rotated order |
| `index_drainer_batch_limit` | `8` | Maximum jobs dispatched by one drainer invocation |
| `index_drainer_scan_limit_per_shard` | `32` | Maximum queue candidates queried per shard and invocation |
| `index_drainer_schedule_rate_minutes` | `1` | EventBridge correctness cadence; whole minutes, minimum `1` |
| `index_dispatch_max_attempts` | `3` | Dispatch attempts before terminal `dispatch_exhausted` |
| `index_dispatch_backoff_base_seconds` | `5` | Base for transient-dispatch exponential backoff |

The Lambda receives these as `SPUR_INDEX_*` environment variables. Queue scans
use `Query` against `${index_jobs_table_arn}/index/<index_queue_gsi_name>`;
admission, dispatch, and terminal release use DynamoDB transactions on the base
table. Queue job records carry string `queue_shard` and `queue_sort_key`
attributes for the sparse GSI; its `ALL` projection returns the full job record
that the drainer deserializes. Owner/global counters and `RUNNING#<job_id>` /
`GLOBAL#RUNNING_TOKEN#<n>` items use only the table's string `pk`, so they do
not require or violate additional table key attributes.

For a concrete small-cap burst, configure:

```hcl
index_rate_limit_per_minute         = 10
index_queue_shard_count             = 1
index_max_queued_jobs_per_owner     = 10
index_max_queued_jobs_global        = 10
index_max_running_jobs_per_owner    = 2
index_max_running_jobs_global       = 2
index_drainer_batch_limit           = 2
index_drainer_scan_limit_per_shard  = 10
index_drainer_schedule_rate_minutes = 1
```

With 10 unique cold `external_index` requests from one owner, all 10 can be
admitted (or an active duplicate is deduplicated). Transactional running caps
allow at most two jobs to reach `dispatching`/`running`; the rest remain queued.
Admission kicks may start those first jobs immediately. As terminal workers
release capacity, the EventBridge invocation starts replacements in batches of
at most two, so a failed or missed kick cannot strand the remaining backlog.
Requests beyond either finite queued cap receive `queue_full` or
`global_queue_full` instead of creating unbounded work.

## Tenant Isolation Notes

This deployment intentionally indexes public external packages into one shared
catalog and S3 bucket. Tenants do not receive direct S3, Aurora, DynamoDB, or
Step Functions access; their isolation boundary is the signed API caller, the
per-caller quota records, and the package coordinate stored on each job and
catalog row.

The shared catalog/bucket model is acceptable only for public package indexing.
Do not use it for private tenant source or tenant-confidential artifacts without
adding tenant-scoped catalog filtering, per-tenant S3 prefixes and IAM policies,
and API response authorization checks. Current workers serialize catalog writes
with DynamoDB leases, but they do not provide private data-plane isolation inside
the shared DuckLake catalog.

## Deploy

Terraform uses a partial S3 backend declared in `versions.tf`:
`backend "s3" {}`. The backend is configured per environment at init time,
and variable values are loaded from `env/<environment>.tfvars`. The committed
`terraform.tfvars` file is placeholder-only and is not the deployment source.

Before the first deployment, an operator must bootstrap the remote-state
resources once outside this module:

- S3 bucket for Terraform state.
- DynamoDB table for Terraform state locking.

Do not create those bootstrap resources by applying this Terraform module.
Record their names in `backends/staging.s3.tfbackend` and
`backends/prod.s3.tfbackend`, replacing the placeholder bucket and lock table
names.

```bash
# Full build + package + deploy to staging.
# Uses backends/staging.s3.tfbackend and env/staging.tfvars.
./deploy.sh

# Deploy to prod.
./deploy.sh --env prod

# Or provide explicit files.
./deploy.sh \
  --backend-config backends/prod.s3.tfbackend \
  --var-file env/prod.tfvars

# Or use an existing zip
./deploy.sh --local-zip /path/to/lambda.zip

# Build the serving Lambda zip without touching Terraform-managed resources
./deploy.sh --skip-worker --package-only

# Build arm64 worker/fetcher image tarballs locally through docker buildx, without ECR
SPUR_CONTEXT_SERVICE_BUILD_MODE=self-contained \
SPUR_CONTEXT_SERVICE_PUSH_IMAGES=0 \
./build-and-push-remote.sh --no-push
```

For manual Terraform operations, always pass the matching backend config and
variable file:

```bash
terraform init -backend-config=backends/staging.s3.tfbackend
terraform plan \
  -var-file=env/staging.tfvars \
  -var="lambda_zip_path=../../target/lambda/spur-context-service.zip" \
  -var="worker_ecr_image=<ecs-worker-image-uri>" \
  -var="worker_lambda_image=<worker-lambda-image-uri>" \
  -var="source_fetcher_lambda_image=<source-fetcher-lambda-image-uri>"
terraform apply \
  -var-file=env/staging.tfvars \
  -var="lambda_zip_path=../../target/lambda/spur-context-service.zip" \
  -var="worker_ecr_image=<ecs-worker-image-uri>" \
  -var="worker_lambda_image=<worker-lambda-image-uri>" \
  -var="source_fetcher_lambda_image=<source-fetcher-lambda-image-uri>"
```

## CI/CD

The context-service GitHub Actions workflow lives at
`.github/workflows/context-service.yml`.

Pull requests and pushes that touch this service run:

- `scripts/spur-cargo --workdir crates/spur-context-service test --all-features`
- deploy-script guardrails in `tests/scripts/test_spur_context_service_deploy.py`
- `infra/spur-context-service/test-graviton2-baseline.sh`

Real AWS artifact builds are gated through `workflow_dispatch` and the
`context-service-staging` environment. Set `build_aws_artifacts=true` to build
the Graviton2-safe worker/fetcher image tarballs and serving Lambda zip. CI defaults to
`SPUR_CONTEXT_SERVICE_BUILD_MODE=self-contained` and
`SPUR_CONTEXT_SERVICE_PUSH_IMAGES=0`, so it uses docker buildx with
`--platform linux/arm64 --provenance=false` and does not depend on the remote
builder VM, create ECR repositories, push images, or run Terraform.

Operator-run pushes remain available by leaving the default
`SPUR_CONTEXT_SERVICE_BUILD_MODE=remote` in `build-and-push-remote.sh`.
Set `SPUR_CLOUD` when the remote builder cloud should differ from the
`scripts/spur-cargo` default:

```bash
SPUR_CONTEXT_SERVICE_BUILD_MODE=remote ./build-and-push-remote.sh
./deploy.sh --skip-worker --package-only
```

Required repository/environment configuration:

| Name | Kind | Purpose |
|---|---|---|
| `CONTEXT_SERVICE_AWS_ROLE_ARN` | secret | AWS role assumed by GitHub OIDC for ECR, Lambda, S3, and smoke access |
| `CONTEXT_SERVICE_AWS_REGION` | variable | AWS region, defaults to `ap-southeast-5` |
| `CONTEXT_SERVICE_STAGING_LAMBDA` | variable | Staging serving Lambda name for the smoke test |
| `CONTEXT_SERVICE_STAGING_SOURCE_BUCKET` | variable | Bucket where the smoke uploads its tiny source tarball |
| `CONTEXT_SERVICE_STAGING_DATA_BUCKET` | variable | Bucket containing bronze, silver, and gold medallion objects |

## Staging E2E Smoke

Run the staging smoke locally after authenticating to AWS:

```bash
export AWS_REGION=ap-southeast-5
export SPUR_CONTEXT_SMOKE_LAMBDA=spur-context-service
export SPUR_CONTEXT_SMOKE_SOURCE_BUCKET=spur-context-staging
export SPUR_CONTEXT_SMOKE_DATA_BUCKET=spur-context-staging

# No-ingest preflight: validates required env, AWS STS auth, and Lambda
# serving catalog config without uploading a fixture or calling external_index.
infra/spur-context-service/smoke-staging-e2e.sh --preflight

# Full E2E smoke: uploads the fixture and invokes real ingest.
infra/spur-context-service/smoke-staging-e2e.sh

# FetchSource E2E smoke: indexes a public GitHub repo through the non-VPC
# source fetcher, asserts Step Functions visited FetchSource, then checks
# medallion objects and serving queries.
infra/spur-context-service/smoke-staging-e2e.sh --github-source
```

The default full smoke publishes a tiny Rust package tarball to S3, presigns it,
and calls `external_index` with that presigned HTTPS S3 URL. That smoke must
stay on the `prefetch_source=false` path so it proves the existing presigned-S3
tarball handoff remains green.

The GitHub smoke calls `external_index` with a public GitHub `git+https` URL
(`SPUR_CONTEXT_SMOKE_GITHUB_URL`, default
`git+https://github.com/BurntSushi/memchr.git`) and `source_kind=git`. It waits
for completion, calls `aws stepfunctions get-execution-history`, asserts the
execution visited `FetchSource`, and then checks medallion objects plus serving
queries. The caller running this smoke needs `states:GetExecutionHistory` for
the returned execution ARN in addition to the Lambda/S3/STS permissions used by
the default smoke.

Both full smoke modes wait for the real worker to complete, check the bronze
source object at
`bronze/{source}/{package}/{revision}/source.tar.gz`, the silver artifact
manifest at `silver/{source}/{package}/{revision}/{builder_version}/manifest.json`,
the gold frozen snapshot pointer at `gold/catalog-snapshot/current.json`,
non-zero `symbol_embeddings`, and then serves `external_code_search`,
`external_code_read`, and `external_knowledge_context` from the staging Lambda.
The `source` path component is the worker's verbatim source coordinate, such as
`registry:crates-io` or `github`.

Serving is intentionally zero-Postgres: the script fails if the serving Lambda
is configured with a Postgres `SPUR_CATALOG_DSN`, or if `SPUR_CATALOG_S3_URI`
does not point at the frozen snapshot pointer
`s3://<bucket>/gold/catalog-snapshot/current.json`. Set
`SPUR_CONTEXT_SMOKE_ALLOW_NON_POINTER_SNAPSHOT=1` only for a temporary staging
debug run against a direct S3 `.ducklake` snapshot.

## Graviton2-safe CPU baseline

Deployable arm64 artifacts for this service must target Graviton2-class hosts:
the serving Lambda bootstrap, the Lambda worker image binary, the Fargate worker
image binary, and the `spur` binary copied into worker images. Build them through
`deploy.sh`, which sources `graviton2-baseline.sh` and exports guarded
`RUSTFLAGS`, `CFLAGS`, and `CXXFLAGS` for those artifact builds. The
self-contained docker buildx path reuses the same helper constants and exports
them into the arm64 build container before compiling.

The allowed Rust `target-cpu` values for deployable artifacts are
`neoverse-n1` or `generic`; C/C++ flags must use `-mcpu=neoverse-n1` or a
generic `-march=armv8-a...` baseline. Do not use `neoverse-v2` for Lambda or
Fargate artifacts: Lambda arm64 and Fargate arm64 run on Graviton2-compatible
hardware, while the remote build VM may support newer instructions. Normal
local/dev `scripts/spur-cargo` builds may continue to use the VM default; this
rule only applies to artifacts deployed by `infra/spur-context-service`.

Run the guard regression check after changing these scripts:

```bash
bash infra/spur-context-service/test-graviton2-baseline.sh
```

## Provisioned Concurrency

To eliminate cold start entirely, set `concurrent_warm_instances`:

```bash
terraform apply -var concurrent_warm_instances=1
```

## Variables

| Name | Default | Description |
|---|---|---|
| `aws_region` | `ap-southeast-5` | AWS region |
| `bucket_name` | `spur-context` | S3 bucket for data |
| `catalog_s3_uri` | `s3://spur-context/gold/catalog-snapshot/current.json` | Frozen serving DuckLake snapshot pointer or snapshot file |
| `context_ducklake_data_path` | `s3://<bucket_name>/data/` | DuckLake data path passed to worker jobs |
| `index_jobs_table_name` | `spur-context-index-jobs` | DynamoDB table for job records and dedupe |
| `catalog_leases_table_name` | `spur-context-catalog-leases` | DynamoDB table for catalog leases |
| `lambda_memory_mb` | `1024` | Lambda memory |
| `lambda_timeout_sec` | `30` | Lambda timeout |
| `concurrent_warm_instances` | `0` | Provisioned concurrency |
| `api_throttle_rate_limit` | `20` | API Gateway route throttle rate per second |
| `api_throttle_burst_limit` | `40` | API Gateway route throttle burst |
| `index_rate_limit_per_minute` | `10` | Per-caller `external_index` requests per minute |
| `index_max_concurrent_jobs_per_caller` | `2` | Per-caller queued/running index job cap |
| `context_max_tarball_bytes` | `524288000` | Tarball download/source cap |
| `context_max_git_bytes` | `2147483648` | Git source tree cap |
| `context_max_build_seconds` | `1800` | Worker `spur graph build` timeout |
| `allowed_source_domains` | `[]` | Optional `source_url` domain allow-list |
| `vpc_id` | n/a | VPC for ECS worker tasks |
| `worker_subnets` | n/a | Private subnets for Lambda and ECS worker tasks |
| `interface_vpc_endpoint_subnet_ids` | `[]` | Optional subnet IDs for interface endpoint ENIs; empty reuses worker subnets |
| `interface_vpc_endpoint_service_keys` | all interface services | Interface endpoint service keys to create; use `states` + `secretsmanager` for low-cost Lambda-worker stacks |
| `worker_route_table_ids` | `[]` | Route tables associated with `worker_subnets`; required when `create_vpc_endpoints=true` |
| `create_vpc_endpoints` | `true` | Create NAT-free worker endpoints for S3, DynamoDB, and selected interface services |
| `vpc_endpoint_region` | `null` | Optional endpoint service-name region override; defaults to `aws_region` |
| `worker_ecr_image` | n/a | ECS fallback worker image URI built by `deploy.sh` |
| `worker_lambda_image` | n/a | Lambda fast-path worker image URI built by `deploy.sh` |
| `manage_ecr_lifecycle_policies` | `true` | Manage cleanup policies for context-service ECR repositories |
| `ecr_lifecycle_repository_names` | context worker repos | ECR repositories that receive the cleanup policy |
| `ecr_lifecycle_keep_tagged_images` | `10` | Tagged images to retain per context-service ECR repository |
| `ecr_lifecycle_untagged_image_days` | `7` | Age in days before untagged ECR images expire |
| `worker_lambda_memory_mb` | `3008` | Lambda worker memory for this account/region cap |
| `worker_lambda_timeout_sec` | `900` | Lambda worker timeout |
| `worker_lambda_ephemeral_storage_mb` | `10240` | Lambda worker `/tmp` storage |
| `worker_lambda_provisioned_concurrency` | `0` | Warm Lambda worker instances |

## Cold Start Performance

| Configuration | INIT | First request | Warm |
|---|---|---|---|
| Default (no concurrency) | 432ms | ~1950ms | ~100ms |
| With provisioned concurrency | — | — | ~100ms (always warm) |

## Remote State

Remote state is required for team deployments. This module intentionally keeps
only a partial S3 backend in source control. Initialize each environment with
its own backend config so staging and prod use separate state keys and share
the bootstrap lock table configured by `dynamodb_table`.
