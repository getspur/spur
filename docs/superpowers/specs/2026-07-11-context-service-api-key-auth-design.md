# SPUR Context Service - CLI-Managed API Key Authentication Design

**Issue:** `bd-1u0iq`

**Builds on:** `2026-07-11-context-service-hybrid-cognito-auth-design.md`

**Status:** Approved architecture; awaiting written-spec review

**Date:** 2026-07-11

## Executive decision

SPUR will add personal API keys as a fourth, additive context-service
authentication mode. API keys supplement rather than replace Cognito M2M,
Cognito human OAuth, IAM, and the explicit anonymous demo path.

OAuth becomes a management-plane bootstrap for the SPUR CLI:

1. `spur context auth login` authenticates a human with Cognito authorization
   code plus PKCE.
2. `spur context key create` uses that human session and the `keys.manage`
   scope to create a personal API key.
3. `spur context mcp` loads the selected key from a credential store and calls
   the exact `POST /mcp/api-key` route.
4. Routine MCP operation does not start OAuth, refresh an OAuth token, or need a
   Cognito client secret.

The API-key route uses an API Gateway HTTP API request Lambda authorizer backed
by a dedicated DynamoDB table. The authorizer returns a trusted owner, public
key ID, and scopes. The serving Lambda ignores the raw key and independently
requires the exact scope for the body-selected `external_*` tool.

Every personal key owned by a Cognito user resolves to the same existing owner
identity, `cognito:user:<sub>`. Creating additional keys therefore cannot create
additional queue, rate-limit, dedupe, or status-visibility buckets.

AWS API Gateway native API keys and usage plans are not used. They are a REST
API metering facility, are not supported by the existing HTTP API product, and
AWS advises against using them as authentication or authorization.

## Problem

The hybrid Cognito design gives standards-based access to humans and customer
M2M integrations, but routine command-line use has two avoidable costs:

- a CLI must retain and refresh OAuth credentials during long-running MCP use;
- lightweight scripts must implement token acquisition and caching; and
- Cognito M2M clients are organization credentials, not convenient personal
  credentials for a developer workstation or local agent.

The current `spur context mcp` path already accepts an optional bearer token and
proxies body-routed `external_*` calls through `ContextServiceClient`. It does
not provide a login command, key lifecycle, secure credential profile, distinct
API-key header, or API-key route.

A safe API-key mode must not turn each key into a new tenant, expose raw keys in
configuration or logs, let API-key callers mint more keys, or let a disabled
feature fall through to the legacy `$default` route.

## Goals

- Let a human use Cognito once to create and manage personal API keys.
- Let `spur context mcp` authenticate routine calls without OAuth refresh.
- Reuse the exact existing `external.read`, `external.index`, and
  `external.status` scope semantics.
- Keep all of a user's OAuth and API-key traffic in one backlog-owner bucket.
- Store no recoverable API-key secret on the server.
- Bound revocation delay and document the emergency kill switch.
- Preserve all existing OAuth, M2M, IAM, demo, queue, and EventBridge behavior.
- Keep the feature disabled by default and prove it with offline/mock tests.
- Support local desktop and headless CLI credential workflows without secrets
  in arguments or normal configuration.

## Non-goals

- Replacing Cognito M2M or migrating existing organization clients.
- Replacing Cognito human login, IAM, or the explicit anonymous demo mode.
- Migrating from API Gateway HTTP API to REST API.
- Using API Gateway usage-plan API keys as an authentication mechanism.
- Allowing API keys or M2M clients to create, list, or revoke personal keys.
- Adding arbitrary custom roles, organization delegation, child keys, or
  service-account impersonation in v1.
- Returning or recovering a raw key after its one-time creation response.
- Writing `last_used_at` to DynamoDB on every request.
- Building a graphical account or key-management frontend.
- Deploying to AWS as part of the implementation or POC task.

## Terms and invariants

- **Management session:** a human Cognito OAuth session used only by
  `auth` and `key` CLI commands.
- **Personal API key:** a high-entropy credential owned by one Cognito human.
- **Public key ID:** a non-secret identifier used for lookup and bounded audit
  correlation.
- **Key secret:** the 256-bit random value revealed once to the client.
- **API-key owner:** always `cognito:user:<sub>` in v1.
- **Key scope:** one of `external.read`, `external.index`, or
  `external.status`. `keys.manage` is never a legal API-key scope.
- **Active key:** a key record whose status is `active` and whose expiry is in
  the future.
- **Revocation SLO:** a revoked key is rejected on authorizer cache miss
  immediately and on all requests within 30 seconds.

The following invariants are mandatory:

1. API-key identity is accepted only on exact API-key routes.
2. Management identity is accepted only from validated Cognito JWT context on
   exact management routes.
3. Malformed auth context never falls back to IAM, principal ID, source IP, or
   `anonymous-internal`.
4. Raw keys, secret hashes, OAuth tokens, authorization codes, PKCE values, and
   request headers are never logged.
5. A user cannot gain more quota by creating more keys.
6. Disabled or absent routes fail closed before tool routing.

## Existing implementation map

| Area | Current symbol/resource | Required evolution |
|---|---|---|
| CLI commands | `spur-cli::ContextCommands` | Add `auth` and `key` subcommands; keep `mcp`. |
| CLI resolution | `resolve_context_service_cli_config_with_env` | Resolve URL and credential profile without storing secrets in normal config. |
| MCP proxy | `ContextServiceClient` | Replace optional bearer-only state with explicit auth modes and route/header selection. |
| OAuth ingress | `POST /mcp/oauth`, `auth.rs`, `lambda.rs` | Preserve behavior; add exact management-route classification. |
| Legacy ingress | `$default` route | Preserve IAM/demo behavior; reserve new paths so disabled features cannot fall through. |
| Queue identity | `BacklogOwner::caller`, `JobRecord.caller_id` | Use the same `cognito:user:<sub>` for OAuth and every personal key. |
| Job status | `route_index_status_for_caller` | Preserve non-enumerating `not_found` across owners. |
| HTTP integration | `aws_apigatewayv2_integration.lambda` | Add exact management routes and a separate API-key integration/authorizer path. |
| POC OAuth client | `infra/spur-context-service/poc/auth-client` | Promote audited OAuth/PKCE logic into a production workspace crate used by `spur-cli`. |

The graph index was stale relative to the active worktree during discovery. The
symbols above were re-grounded with exact graph reads and the approved Cognito
integration branch before this design was written.

## Alternatives and decision record

### Chosen: HTTP API request Lambda authorizer

A dedicated authorizer rejects invalid credentials before the serving Lambda,
keeps key lookup and comparison out of normal tool handling, and passes typed
identity/scopes to the integration. It fits the existing API Gateway HTTP API
without replacing native Cognito JWT or IAM routes.

The authorizer is a separate Lambda binary and function. This creates a narrow
IAM policy and independently testable security boundary.

### Rejected for production: validation only in the serving Lambda

This is cheaper by one Lambda invocation and can provide immediate revocation,
but it moves the first authentication boundary into a large function that also
parses tools, prepares catalogs, performs admission, and handles scheduled
events. It remains useful as a comparison in the isolated POC, not as the
production choice.

### Rejected: REST API migration and native API keys

REST API native keys are intended for usage plans, quotas, and identifying API
consumers. AWS explicitly advises using IAM, Lambda authorizers, or Cognito for
access control. Migrating would also replace the lower-cost HTTP API and create
unrelated route/integration work.

### Rejected: API-key owner per key

An identity such as `api-key:<key_id>` would give one user a distinct quota and
status bucket per key. It would make the ten-key limit a tenfold quota bypass.
The public key ID remains audit metadata only.

### Rejected: password hashing or a server-side pepper in v1

The key secret is 256 bits from an operating-system CSPRNG, not a human
password. SHA-256 is sufficient against offline brute force at that entropy.
A pepper would add Secrets Manager availability and a multi-version rotation
contract without materially improving resistance to guessing. The design
depends on strong generation and rejects shorter or malformed keys.

## Route architecture

| Route | Edge authorization | Lambda policy |
|---|---|---|
| `GET /.well-known/spur-context-service` | `NONE`; public metadata only | Return bounded versioned discovery; no user or secret data. |
| `POST /mcp/oauth` | Existing Cognito JWT | Existing exact external-tool scope checks. |
| `POST /mcp/api-key` | New request Lambda authorizer | Accept only typed API-key authorizer context; exact tool scope required. |
| `POST /auth/api-keys` | Cognito JWT + `keys.manage` | Human client only; create a personal key. |
| `GET /auth/api-keys` | Cognito JWT + `keys.manage` | Human client only; list metadata for own keys. |
| `DELETE /auth/api-keys/{key_id}` | Cognito JWT + `keys.manage` | Human client only; idempotently revoke own key. |
| `$default` | Existing `AWS_IAM` or explicit demo `NONE` | Existing behavior unchanged, except reserved paths fail closed. |
| EventBridge schedule | Lambda resource policy | Existing drainer discriminator; never enters HTTP auth. |

`api_key_auth_enabled=true` has the Terraform precondition
`cognito_auth_enabled=true`. The human app client alone receives
`keys.manage`. M2M app clients do not receive it.

### Reserved-path fail-closed rule

The serving Lambda classifies these paths before generic body parsing:

- `/.well-known/spur-context-service`
- `/mcp/api-key`
- `/auth/api-keys`
- `/auth/api-keys/*`

The public discovery path is available only when Cognito authentication is
configured. It returns a versioned document containing issuer, public human
client ID, authorization/token endpoints, supported scopes, API-key feature
status, and exact management/MCP route URLs. It contains no client secret,
credential, user data, account ID, or internal resource ARN.

When the corresponding feature is disabled, absent, or misconfigured, reserved
paths return a bounded unavailable/not-found response. They never continue
through the legacy route. This check remains active even when Terraform omits an
exact route, because an unmatched path otherwise selects `$default`.

When API-key auth is enabled, `/mcp/api-key` requires a complete API-key
authorizer context. Management paths require a complete Cognito human context.
Any context from the wrong scheme returns 401/403 without fallback.

## API-key request flow

```text
spur context mcp
  -> credential resolver loads selected personal key
  -> POST /mcp/api-key
       X-SPUR-API-Key: spur_live_<id>_<secret>
       body: { "tool": "external_*", "args": {...} }
  -> API Gateway request authorizer
       parse fixed key grammar
       strongly consistent GetItem KEY#<id>
       reject disabled/revoked/expired/malformed
       constant-time compare SHA-256(secret)
       return owner_id, key_id, scopes, auth_kind=api_key
  -> API Gateway invokes serving Lambda
  -> serving Lambda validates typed context and exact path
  -> serving Lambda maps body tool to exact required scope
  -> existing MCP handler, queue, dedupe, rate and status logic
```

API Gateway response caching uses the raw API-key header and route key as
identity sources. Both allow and deny responses may be cached for 30 seconds.
The route key prevents a decision from being reused across routes. A cache miss
uses a strongly consistent primary-key read.

Verified Task C provider behavior exposes the two configured identity sources
in route-key-first order, even when configuration lists the header first. The
authorizer therefore treats the exact two values as an order-independent pair:
one value must equal the single raw `X-SPUR-API-Key` header and the other must
equal the exact `POST /mcp/api-key` route key. Missing, duplicate, extra, or
mismatched values fail before lookup. Terraform tests assert the observed
route-key-first provider order without converting the result to a set.

HTTP API payload v2 simple authorizer responses can return a cacheable
`isAuthorized: false` decision but cannot attach the custom bounded 401 body
described in the domain failure table. The production boundary therefore uses
the secure cacheable deny contract; API Gateway controls the client-visible
denial status/body. Task C must configure simple responses and the two-part
cache identity, and Task F's POC must record the observed gateway status/body
rather than claiming that the Lambda emits a custom 401 payload.

The serving Lambda never reads the raw API-key header. A separate API-key
integration should remove the header before invoking it when HTTP API parameter
mapping supports that operation. Header removal is defense-in-depth only; the
trust model remains correct if the POC proves that the header is still present.

## Management flow

### Login

```text
spur context auth login
  -> GET /.well-known/spur-context-service from configured service URL
  -> validate discovery schema, origin and HTTPS policy
  -> create fresh state, nonce and S256 PKCE verifier
  -> open system browser
  -> accept exact loopback callback
  -> exchange code with verifier
  -> validate state, issuer, audience, signature, nonce and access-token hash
  -> store management credentials in OS credential store
```

The CLI never accepts an authorization code, refresh token, access token, PKCE
verifier, or client secret as an argument. The existing isolated POC auth client
is promoted into a workspace crate rather than imported from `infra/`.

### Create

```text
spur context key create --name workstation \
  --scope external.read \
  --scope external.index \
  --scope external.status
```

The command obtains a fresh management access token when needed and posts the
requested name, scopes, and expiry. The backend verifies `keys.manage`, human
client ID, human principal kind, scope subset, active-key cap, and expiry bounds.

The response contains the full key once. By default, the CLI writes it directly
to the selected credential profile and prints only public ID, fingerprint,
scopes, and expiry. `--show-secret` prints the one-time secret only when stdout
is an interactive terminal or the caller explicitly selects a secure output
file. It refuses terminal-history-style arguments.

### List, select and revoke

```text
spur context key list
spur context key use <key-id>
spur context key revoke <key-id>
spur context auth logout
```

`list` returns metadata only. `use` changes the local active credential profile;
it is not a server mutation. `revoke` is idempotent. `logout` removes OAuth
management credentials but does not revoke or delete personal API keys.

### Headless and second-machine provisioning

```text
printf '%s' "$SPUR_CONTEXT_SERVICE_API_KEY" | spur context key add --stdin
```

`key add --stdin` reads one key without echo, validates its grammar, and stores
it in the selected local credential profile. It does not send a management
request. API keys are never accepted as positional or flag values.

## CLI credential contract

Credential lookup order for MCP operation is:

1. `SPUR_CONTEXT_SERVICE_API_KEY` for explicitly managed CI/headless processes;
2. the OS credential store for the selected profile; and
3. a separately configured credentials file with owner-only `0600` permissions.

The `0600` file is an explicit fallback for systems without a usable keyring.
It is not `config.toml`, must not be committed, and is rejected when permissions
are broader on Unix. Windows uses the platform credential store by default and
an equivalent restricted-file check when file fallback is selected.

Normal `[context_service]` configuration stores only:

- service/discovery URL;
- credential profile name;
- explicit auth mode; and
- optional non-secret public key ID hint.

The production client uses an explicit enum:

```text
ContextServiceAuth::None
ContextServiceAuth::OAuthBearer
ContextServiceAuth::ApiKey
```

Modes are mutually exclusive. API-key mode selects `/mcp/api-key` and sends
`X-SPUR-API-Key`; OAuth mode selects `/mcp/oauth` and sends Bearer; legacy mode
keeps the configured legacy URL. Existing `--token` remains temporarily for
compatibility but is deprecated for routine CLI use and must not be written to
new configuration.

## Key grammar and cryptography

The fixed grammar is:

```text
spur_<environment>_<public-id>_<secret>
```

V1 environments are `live` and `test`. The production service accepts `live`;
isolated POC stacks accept only `test` unless explicitly configured otherwise.

- `public-id`: 26 lowercase Base32 characters encoding 128 random bits.
- `secret`: 52 lowercase Base32 characters encoding 256 random bits.
- alphabet: `abcdefghijklmnopqrstuvwxyz234567`.
- separators: underscore, which never appears in encoded components.

Parsing requires the exact prefix, environment, segment count, segment lengths,
and alphabet. Unknown environments and non-canonical encodings fail before any
DynamoDB read. Generation uses the operating system CSPRNG.

The server stores `SHA-256(secret_bytes)` and compares a newly computed digest
in constant time. It does not hash the prefix or public ID as secret material.
Logs may contain the public ID only where an audit event requires it; metrics
prefer bounded reason dimensions rather than per-key dimensions.

## DynamoDB model

API keys use a dedicated on-demand table with encryption, point-in-time
recovery, and TTL. It is not added to the index-jobs table.

### Key item

```text
pk                 = KEY#<public-id>
entity             = api_key
owner_id           = cognito:user:<sub>
name               = bounded display name
secret_hash        = 32-byte binary SHA-256 digest
scopes             = string set
status             = active | revoked
created_at          = epoch seconds
expires_at          = epoch seconds
revoked_at          = optional epoch seconds
ttl                 = delayed-GC epoch seconds
owner_gsi_pk        = OWNER#<owner-id>
owner_gsi_sk        = KEY#<created-at>#<public-id>
expiry_gsi_pk       = EXPIRY#<UTC-hour-bucket>
expiry_gsi_sk       = <expires-at>#<public-id>
```

The owner GSI is for listing and operator discovery only. The sparse expiry GSI
is for bounded cleanup queries by UTC hour. Authentication uses a strongly
consistent read of the primary key because DynamoDB GSIs do not support strongly
consistent reads.

### Owner counter item

```text
pk                 = OWNER#<owner-id>
entity             = api_key_owner
active_key_count   = integer 0..10
updated_at         = epoch seconds
```

### Cleanup cursor item

```text
pk                 = SYSTEM#expiry-sweeper
entity             = cleanup_cursor
completed_hour     = UTC hour bucket
updated_at         = epoch seconds
lease_owner        = optional invocation ID
lease_expires_at   = optional epoch seconds
```

Cursor and lease updates are conditional so overlapping scheduled invocations
cannot independently advance past unfinished buckets.

### Atomic create

One transaction:

1. conditionally increments `active_key_count` when it is below 10; and
2. puts the key item with `attribute_not_exists(pk)`.

Random public-ID collisions retry generation. A failed transaction reveals no
key and changes no counter.

### Atomic revoke

One transaction:

1. conditionally changes the key from `active` to `revoked`; and
2. removes the expiry-GSI attributes; and
3. decrements the owner counter only when the transition occurs.

Repeated revoke returns the current revoked state without another decrement.
Cross-owner management returns `not_found` rather than revealing the key.

### Expiry and cleanup

The authorizer rejects `expires_at <= now` immediately. Expired records remain
counted until explicit revoke or cleanup; DynamoDB TTL is not an enforcement or
capacity mechanism.

An hourly EventBridge cleanup invokes a bounded sweeper that queries due sparse
expiry-GSI hour buckets and applies the same idempotent revoke transaction.
Late or retried sweeps process every uncompleted bucket since the persisted
cursor, with an operator-configured maximum catch-up horizon and an alarm when
the cursor lags. Capacity is therefore reclaimed within one hour under normal
operation. A user can revoke an expired key manually for immediate capacity.
The sweeper has bounded pages, retries, metrics, and no access to raw secrets.
Task C implements this as the independent
`spur-context-api-key-cleanup` Rust Lambda. It accepts only the exact
`sweep_expired_api_keys` EventBridge discriminator, derives the lease owner from
the Lambda request ID, applies the configured catch-up/page bounds to the
persisted store sweep, and emits secret-free CloudWatch EMF cursor-lag metrics.
Store, lease, clock, event, and configuration failures remain Lambda errors.

## Authorization semantics

The scope matrix is unchanged:

| Tools | Required key scope |
|---|---|
| `external_catalog`, `external_code_search`, `external_code_read`, `external_code_callers`, `external_code_callees`, `external_knowledge_context` | `external.read` |
| `external_index` | `external.index` |
| `external_index_status` | `external.status` |

The API Gateway authorizer may broadly allow a valid key with at least one
external scope. The serving Lambda remains the exact body-selected enforcement
point. `keys.manage` is rejected during API-key creation even if supplied in a
request and is never returned in API-key authorizer context.

The trusted integration context is versioned and typed:

```text
auth_context_version = 1
auth_kind            = api_key
owner_id             = cognito:user:<sub>
key_id               = <public-id>
scopes               = normalized space-separated scopes
```

Missing, duplicate, oversized, malformed, unknown-version, or wrong-route
context fails closed. The serving Lambda never derives owner identity from the
raw header or public key ID.

## Revocation, kill switch and offboarding

### Normal revocation

The key item changes immediately. Cached authorizer responses can continue to
allow or deny the exact key/route combination for at most 30 seconds. This
30-second interval is the documented revocation SLO.

### Emergency kill switch

The first emergency action is to disable or detach the exact API-key route and
authorizer while preserving OAuth, IAM, management, and scheduled-drainer
routes. A configuration flag also causes Lambda reserved-path checks to reject
API-key context. Operators do not wait for individual key updates during a
route-wide incident.

### User offboarding

V1 includes an audited IAM operator workflow to revoke all keys for one owner.
It queries the owner GSI, applies idempotent revoke transactions in bounded
batches, records progress without secrets, and can resume after partial failure.

Cognito account deletion must run revoke-by-owner first. The runbook verifies
zero active keys before deleting or disabling the account. The operator path is
not exposed to personal API keys or M2M credentials.

## Failure semantics

Externally visible API-key authentication failures are deliberately
non-enumerating:

| Condition | Response |
|---|---|
| Missing/malformed key | bounded 401 |
| Unknown public ID | same bounded 401 |
| Wrong secret | same bounded 401 |
| Expired/revoked key | same bounded 401 |
| Missing exact tool scope | bounded 403 `missing_scope` |
| Wrong route/auth context | bounded 401/403 without fallback |
| API-key feature disabled | bounded unavailable/not-found response |
| DynamoDB unavailable | fail closed with bounded 5xx; never authorize |

Management validation uses bounded reason enums but does not expose hashes,
raw claims, internal DynamoDB conditions, or cross-owner existence.

## Observability and cost controls

Metrics use bounded dimensions:

- authorizer decision: `allow`, `missing`, `malformed`, `unknown`, `mismatch`,
  `expired`, `revoked`, `store_error`;
- management outcome: `created`, `listed`, `revoked`, `cap_rejected`,
  `scope_rejected`, `store_error`;
- authorizer latency and DynamoDB latency;
- cache-hit approximation from authorizer invocation/request ratios;
- cleanup scanned, revoked, skipped, retried, and failed counts; and
- API-key-route 401/403/429/5xx rates.

Access logs omit authorization and API-key headers, request bodies, JWT claims,
subjects, hashes, and raw owner IDs. Audits may record actor kind, hashed owner,
public key ID, action, timestamps, scopes, and bounded result.

No synchronous `last_used_at` write occurs in v1. This avoids one DynamoDB write
per request. A later sampled activity feature requires a separate design.

The additional request cost consists of authorizer Lambda invocations on cache
miss, strongly consistent DynamoDB reads on cache miss, low-volume key lifecycle
writes, cleanup invocations, logs, and metrics. The actual authorizer invocation
count depends on traffic distribution across the 30-second cache key. Cost
evidence must calculate each AWS price dimension independently; it must not
reuse the rejected review estimate that mis-scaled Lambda invocation arithmetic.

## Terraform design

New variables are feature-flagged and validated:

- `api_key_auth_enabled` (default `false`);
- `api_key_authorizer_cache_seconds` (default and maximum for v1: `30`);
- `api_key_default_ttl_days` (default `90`);
- `api_key_max_ttl_days` (default/max `365`);
- `api_key_max_active_per_user` (fixed/default `10` in v1);
- authorizer and cleanup memory/timeout/log retention; and
- optional budget/alarm notification configuration.

Enabled mode creates only:

- the dedicated API-key table and owner GSI;
- the sparse expiry GSI and persisted cleanup cursor;
- dedicated authorizer function, alias, role, policy and log group;
- the public versioned discovery route when Cognito is enabled;
- exact API-key route, custom authorizer and invocation permission;
- exact management routes using the existing JWT authorizer;
- cleanup schedule, permission and scoped execution path;
- route-specific logs, metrics, alarms, and optional budget signals; and
- non-secret discovery outputs such as API-key route URL and feature status.

Authorizer IAM allows `GetItem` on the API-key table only. Management IAM allows
the exact key/counter transaction and owner-GSI query operations. Cleanup IAM
allows owner-GSI queries and idempotent revoke transactions. No role can read
raw keys because none are stored.

The separate API-key integration attempts to remove `X-SPUR-API-Key` before the
serving Lambda. Terraform mock tests assert the configured mapping. A separately
approved live POC must verify the observed Lambda event. The serving trust model
does not depend on removal.

The repository command `scripts/package-context-api-key-lambdas.sh` builds both
lean binaries as stripped, static `aarch64-unknown-linux-musl` bootstraps and
creates deterministic ZIPs at the Terraform defaults:
`target/lambda/spur-context-api-key-authorizer.zip` and
`target/lambda/spur-context-api-key-cleanup.zip`. The command uses the dedicated
crate lockfile, normalizes bootstrap timestamps, and packages exactly one
`bootstrap` entry per archive.

## Rust component boundaries

### `spur-context-auth-client`

A new workspace crate is promoted from the isolated POC. It owns:

- OAuth/OIDC discovery and PKCE login;
- loopback callback state machine;
- management-token refresh and redaction;
- context-service management HTTP client;
- credential-store trait and platform adapters; and
- typed key metadata and one-time creation response.

It does not depend on the serving Lambda crate.

### `spur-context-service::api_keys`

This module owns key grammar, CSPRNG generation, digesting, constant-time
comparison, typed records, scope normalization, transactions, list/revoke
semantics, and store traits. Pure logic and fake-store tests do not require AWS.

### Authorizer binary

A lean Lambda binary owns API Gateway authorizer event/response types, bounded
input validation, strongly consistent lookup, status/expiry checks, digest
comparison, and typed context output. It does not link catalog/DuckDB logic.

### Cleanup binary

A second lean Lambda binary validates the exact scheduled event and bounded
configuration, then invokes the shared store's fenced expiry sweep. The store
queries the sparse expiry GSI, transactionally transitions only still-active
expired keys, decrements owner accounting exactly once, advances only fully
drained hour buckets, and safely tolerates already-terminal/raced records. The
binary links neither serving/catalog code nor DuckDB.

### Serving Lambda

The existing Lambda owns reserved-path classification, management handlers,
versioned public discovery, typed authorizer-context validation, and exact
body-tool scope enforcement. It continues to own EventBridge discrimination and
existing tool routing.

### `ContextServiceClient`

The MCP proxy owns explicit auth mode, route selection, header selection,
timeouts, bounded remote errors, and secret-safe debug output. It never performs
OAuth refresh during API-key MCP operation.

## POC and test plan

### Pure Rust tests

- fixed key grammar accepts canonical values and rejects every malformed shape;
- generation produces correct entropy/length and no deterministic collisions;
- only the full key is returned once;
- persisted records contain digest but no raw key;
- digest comparison is constant-time at the chosen helper boundary;
- exact scope normalization rejects `keys.manage` and unknown scopes;
- create transaction enforces ten keys under concurrency;
- revoke decrements exactly once under concurrent retries;
- expiry rejects immediately and cleanup is idempotent;
- cross-owner list/revoke returns non-enumerating results; and
- store failures fail closed.

### Lambda contract tests

- discovery returns only the approved versioned public fields and rejects
  mismatched origin/scheme configuration;
- exact API-key and management routes classify before generic/legacy parsing;
- feature-disabled reserved routes cannot reach `$default` behavior;
- API-key route accepts only authorizer context version 1/kind `api_key`;
- missing/malformed context never falls back to JWT, IAM, principal ID, source
  IP, or anonymous;
- every external tool has exactly one required scope;
- OAuth and every personal key derive the same `cognito:user:<sub>` owner;
- multiple keys cannot create additional backlog-owner counters;
- cross-owner status remains `not_found`;
- scheduled EventBridge events bypass HTTP auth unchanged; and
- raw headers/keys do not appear in errors or logs.

### CLI tests

- login uses fresh PKCE/state/nonce and exact callback validation;
- management credentials use the credential-store abstraction;
- create stores the one-time key without printing it by default;
- `--show-secret` is TTY/secure-output constrained;
- `key add --stdin` reads without argument exposure;
- credential precedence is environment, keyring, then explicit restricted file;
- broad file permissions are rejected;
- `key use` changes only local non-secret profile metadata;
- API-key MCP mode selects `/mcp/api-key` and `X-SPUR-API-Key`;
- API-key MCP mode never invokes OAuth refresh;
- auth modes are mutually exclusive; and
- debug/error formatting redacts all credential values.

### Terraform tests

- disabled defaults create no API-key resources or routes;
- enabling API keys while Cognito is disabled fails validation;
- Cognito-enabled discovery is an exact public route with bounded output;
- management routes use JWT plus `keys.manage`;
- API-key route uses CUSTOM authorization and the provider-observed
  route-key-first identity-source order;
- cache TTL is 30 seconds and includes route key;
- table encryption, PITR, TTL, keys and owner GSI match the contract;
- expiry GSI and cleanup cursor support bounded catch-up without table scans;
- authorizer, management and cleanup IAM are least privilege;
- API-key header is absent from access-log format;
- header-removal mapping is configured as defense-in-depth;
- `$default`, OAuth, IAM/demo and drainer resources are unchanged; and
- outputs expose no secrets or hashes.

### Isolated live POC evidence

No implementation task may apply AWS resources. A separately approved sandbox
POC must prove:

1. the HTTP API authorizer event and simple-response context shape;
2. cache behavior for allow and deny responses with header plus route key;
3. revocation completes within 30 seconds;
4. API Gateway header removal behavior on the Lambda event;
5. disabled/unmatched paths cannot fall through to demo anonymous access;
6. strongly consistent reads observe create/revoke changes;
7. OAuth, API-key, IAM and EventBridge paths remain isolated;
8. no raw key appears in API Gateway, authorizer, Lambda or CLI logs; and
9. teardown inventory proves every POC resource and secret-bearing state is
   removed under the existing isolated POC process.

## Rollout

1. Land pure key/store logic and CLI credential abstractions behind disabled
   code paths.
2. Land disabled Terraform resources and mock tests.
3. Run the isolated live POC under separate approval.
4. Enable in a non-production environment for internal users only.
5. Verify shared OAuth/API-key ownership, revocation SLO, logs, queue health and
   cost metrics.
6. Enable production key creation for a small cohort.
7. Expand while retaining Cognito M2M as the recommended organization path.

Rollback disables the exact API-key route first. It does not disable Cognito,
delete the key table, rewrite jobs, or modify `$default`. Management may remain
available for revocation and evidence collection. Destructive teardown waits
through the cache TTL, revokes keys, disables deletion protection where
applicable, and verifies OAuth/IAM/drainer resources remain.

## Acceptance criteria

- A user can log in once, create a scoped personal key, and run
  `spur context mcp` without routine OAuth activity.
- The server stores no raw key and the CLI stores no secret in normal config.
- API-key and OAuth calls from one human share exactly one owner identity.
- Ten concurrent creates cannot produce more than ten active records.
- Concurrent/repeated revoke decrements capacity exactly once.
- Expired/revoked/unknown/wrong keys are non-enumerating and fail closed.
- Revocation is globally effective within 30 seconds.
- Disabled/misconfigured routes cannot fall through to legacy/demo behavior.
- Management is Cognito-human-only and `keys.manage` cannot be delegated to a
  key or M2M client.
- Existing OAuth, M2M, IAM, anonymous demo, queue and EventBridge tests pass.
- Offline verification and mock Terraform tests pass through repository
  wrappers; no AWS apply or production state change occurs.

## References

- [AWS: Control access to HTTP APIs with Lambda authorizers](https://docs.aws.amazon.com/apigateway/latest/developerguide/http-api-lambda-authorizer.html)
- [AWS: Choose between REST APIs and HTTP APIs](https://docs.aws.amazon.com/apigateway/latest/developerguide/http-api-vs-rest.html)
- [AWS: Usage plans and API keys for REST APIs](https://docs.aws.amazon.com/apigateway/latest/developerguide/api-gateway-api-usage-plans.html)
- [AWS: DynamoDB transactions](https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/transactions.html)
- [AWS: DynamoDB read consistency](https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/HowItWorks.ReadConsistency.html)
- [AWS: DynamoDB TTL](https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/TTL.html)
- [RFC 7636: Proof Key for Code Exchange](https://www.rfc-editor.org/rfc/rfc7636)
- [RFC 9700: OAuth 2.0 Security Best Current Practice](https://www.rfc-editor.org/rfc/rfc9700)
