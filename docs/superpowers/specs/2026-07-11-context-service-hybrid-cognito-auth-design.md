# SPUR Context Service — Hybrid Cognito Authentication Design

**Issue:** `bd-5v71s`

**Prior research:** `bd-2xges`

**Status:** Approved architecture; implementation-ready design

**Date:** 2026-07-11

## Executive decision

The context service will use three authentication paths, each for the workload
it serves best:

1. Human clients use an Amazon Cognito **Lite** user pool, OAuth 2.0
   authorization code, and PKCE with `S256`.
2. External server integrations use OAuth 2.0 `client_credentials`, with one
   confidential Cognito app client per customer organization.
3. Internal AWS workloads continue to use API Gateway `AWS_IAM` and SigV4.

An exact `POST /mcp/oauth` HTTP API route will use API Gateway's native JWT
authorizer and the existing Lambda integration. The existing `$default` route
will remain `NONE` in the demo environment and `AWS_IAM` by default elsewhere.
This two-route ingress is required during migration because an API Gateway route
has one authorization type. A full route match takes precedence over
`$default`, so the new route can be introduced without changing legacy request
URLs ([HTTP API route selection][aws-http-routes]).

All eight `external_*` tools remain body-routed behind the single OAuth POST
endpoint. The JWT authorizer performs signature, issuer, audience/client, time,
and broad-scope validation. Lambda then requires the exact scope for the tool
named in the body. Lambda also derives a collision-safe identity:

- `cognito:user:<sub>` for humans,
- `cognito:client:<client_id>` for M2M organizations, and
- `iam:<account_id>:<principal_unique_id>` for IAM callers.

The M2M cost-minimum profile uses a 24-hour access token only when the customer's
risk policy accepts a bearer-token replay window of up to 24 hours. The module's
safer balanced default is 6 hours. Clients cache and reuse tokens until 80% of
their lifetime plus jitter; they do not request a token per API call.

## Problem

The deployed service is an API Gateway HTTP API with one `$default` route and a
Lambda proxy integration. Its default infrastructure posture is `AWS_IAM`, while
the demo tfvars intentionally select `NONE` and allow mutations under the
shared `anonymous-internal` caller.
Lambda parses API Gateway JWT and IAM authorizer context, but currently:

- only `external_index` and `external_index_status` require a caller;
- JWT caller identity prefers `sub`, then `principal_id`;
- all other `external_*` tools are unauthenticated at the application layer;
- there is no exact per-tool OAuth scope check because the tool is selected from
  the JSON body after API Gateway has selected its route; and
- a client-credentials caller is not classified by `client_id`, so M2M tenants
  cannot have stable, isolated queue ownership.

This is sufficient for the demo and IAM-only internal callers, but it is not a
customer-facing OAuth contract. A malformed or over-privileged token must not
fall through to an IAM, principal-ID, source-IP, or anonymous identity.

## Goals

- Add standards-based human and M2M access without charging internal AWS traffic
  for Cognito M2M token responses.
- Enforce authentication for every `external_*` tool on the OAuth route and
  least-privilege authorization for the body-selected tool.
- Give humans, customer organizations, IAM workloads, and the legacy anonymous
  demo distinct identity namespaces.
- Preserve current per-owner queue caps, dedupe behavior, rate limiting, and
  non-enumerating status ownership.
- Introduce the change without breaking the current `NONE` and `AWS_IAM` routes.
- Make cost, replay exposure, rollback, and operator evidence measurable.

## Non-goals

- Replacing IAM for trusted AWS workloads.
- Turning Cognito into a direct API-key issuer. Cognito's native M2M contract is
  OAuth client credentials and access tokens, not long-lived API keys.
- Adding frontend UI code, identity-pool AWS credentials, a custom policy engine,
  or Amazon Verified Permissions.
- Encoding arbitrary tenant business roles in JWT claims in v1.
- Changing DynamoDB job, dedupe, queue, or rate-limit schemas.
- Removing the public demo route in this migration.
- Solving OAuth-route scale beyond the native authorizer's fixed 50-audience
  quota in v1.

## Assumptions and terminology

- **Access token** means a Cognito user-pool JWT with `token_use=access`; ID
  tokens are never accepted as API credentials.
- **Human client** means one public app client with no client secret.
- **Organization client** means one confidential app client and stable
  `client_id` for one customer organization. Rotating either of its two secrets
  does not change the `client_id`.
- **Caller identity** is the namespaced string stored in `JobRecord.caller_id`
  and used by current rate-limit and status-ownership checks.
- **Backlog owner** is the existing `BacklogOwner { kind, id }`. V1 continues to
  use `BacklogOwnerKind::Caller` and the namespaced caller identity.
- **Legacy route** is the existing `$default` route. **OAuth route** is the new
  exact `POST /mcp/oauth` route.
- The 50,000-MAU estimate assumes direct user-pool or social-provider users, a
  new-account Lite free tier of 10,000 MAUs, no Plus/ASF features, no enterprise
  SAML/OIDC MAUs, and no SMS or email delivery charges.

## Current implementation map

| Area | Current symbol/resource | Fact the implementation must preserve or change |
|---|---|---|
| Lambda ingress | `lambda.rs::handler`, `handle_api_gateway_request` | Scheduled drainer and HTTP events share one Lambda; only HTTP requests enter auth. |
| Request shape | `ApiGatewayRequest`, `ApiGatewayAuthorizer`, `JwtAuthorizer`, `IamAuthorizer` | JWT claims are a string map; IAM has `user_arn`, `caller_id`, `user_id`, and `account_id`; API Gateway v2 `rawQueryString` is retained only for the login redirect facade. |
| Current auth | `authenticated_caller_id`, `jwt_caller_id`, `iam_caller_id`, `auth_error_response` | Only index/status authenticate; JWT currently prefers `sub`; auth errors are HTTP 401. |
| Body routing | `parse_tool_request`, `routed_tool_name`, `handle_api_gateway_request` | API Gateway cannot select a scope from the request body. |
| External tools | `external_catalog`, `external_code_search`, `external_code_read`, `external_code_callers`, `external_code_callees`, `external_knowledge_context`, `external_index`, `external_index_status` in `mcp.rs` | The scope matrix must cover all eight names exactly. |
| Admission | `mcp.rs::route_index_inner` | DNS/abuse checks, per-caller rate limiting, canonical dedupe, and bounded enqueue already precede dispatch. |
| Status ownership | `mcp.rs::route_index_status_inner` | A caller mismatch returns `{"status":"not_found"}` and must remain non-enumerating. |
| Queue owner | `jobs.rs::BacklogOwner`, `BacklogOwner::caller`, `DynamoDbJobStore::enqueue_job` | Queue, dedupe, and hard per-owner/global caps already use caller-derived ownership. |
| API Gateway | `aws_apigatewayv2_api.http`, `.integration.lambda`, `.route.default`, `.stage.default` | One HTTP API, payload v2.0, `$default` auth from `api_authorization_type`, and route throttles already exist. |
| Lambda config | `aws_lambda_function.service` | The demo flag and queue/rate values are environment variables. No OAuth client secret belongs here. |
| Environments | `variables.tf`, `env/default.tfvars` | Module default is `AWS_IAM`; demo explicitly uses `NONE` plus mutations owned by `anonymous-internal`. |

The graph index reported stale file OIDs during discovery, so the symbol map was
confirmed against exact graph reads and current working-tree Terraform/Markdown
bytes before this design was written.

## Decision record and alternatives

### Chosen: native JWT route plus Lambda exact-scope enforcement

This is the lowest operational-cost design that preserves native signature and
time validation at the edge. API Gateway validates the bearer token before
Lambda invocation. Lambda sees validated claims, the route path, and the body,
so it can apply the missing tool-specific decision.

### Rejected: Cognito for internal AWS callers

It would make AWS-native services manage client secrets and would add a billed
successful token response for traffic already authenticated by IAM. SigV4 also
provides short-lived role credentials and IAM policy scoping without another
bearer credential.

### Rejected for v1: Lambda authorizer or direct API keys

A Lambda authorizer could read custom metadata or avoid the 50-audience limit,
but it adds code, invocation latency, cache semantics, and another security-
critical verifier. Direct API keys would require a new issuance, hashing,
rotation, revocation, and tenant registry; Cognito does not natively validate
such keys. Revisit a Lambda authorizer only if audience scale or immediate
revocation becomes more important than the native-authorizer simplicity.

**Supersession note (2026-07-11):** The later approved companion design,
`2026-07-11-context-service-api-key-auth-design.md`, adds personal API keys for
SPUR CLI use as a separate, feature-flagged authentication mode. It does not
replace the Cognito human/M2M or IAM decisions in this document. The companion
spec owns API-key issuance, Lambda-authorizer, storage, revocation, and CLI
contracts; this historical rejection explains why they were excluded from the
initial Cognito implementation.

### Chosen Rust libraries and dependency boundary

The production `spur-context-service` Lambda does not add an OAuth, OIDC, JWT,
JWKS, or HTTP-client dependency for Cognito authentication. API Gateway remains
the cryptographic verifier, and `src/auth.rs` performs only semantic checks over
the validated authorizer context. This avoids duplicate key caches, network I/O
on the request path, and disagreement between two signature verifiers.

The isolated POC/client utility uses mature standards-based crates instead:

- [`oauth2` 5.x][rust-oauth2] for M2M `client_credentials`, Basic client
  authentication, typed scopes and token responses, secret redaction, and PKCE
  primitives;
- [`openidconnect` 4.x][rust-openidconnect] for the human authorization-code
  flow, provider metadata, ID-token issuer/audience/signature verification,
  nonce verification, and access-token hash verification when the ID token
  supplies that claim; and
- `reqwest` with Rustls, redirect following disabled, bounded connect/request
  timeouts, and no proxy inherited from untrusted request data.

These dependencies live in a standalone POC package under
`infra/spur-context-service/poc/auth-client/`, with its own `Cargo.toml`. They
must not become normal dependencies of `spur-context-service`. Build and test
that package through `scripts/spur-cargo --dir infra/spur-context-service/poc/auth-client`,
never bare `cargo`.

For M2M, configure `AuthType::BasicAuth`, request only the exact custom scopes,
and cache the returned token by `(client_id, normalized_scope_set)`. The HTTP
client must reject redirects: the `oauth2` crate explicitly warns that following
token-endpoint redirects can expose requests to SSRF and credential leakage
([`oauth2` security guidance][rust-oauth2-security]). The human client always
uses `PkceCodeChallenge::new_random_sha256`, verifies one-time `state`, retains
the verifier only until code exchange, and verifies the returned ID token with
the original nonce. Although `ClientSecret` debug formatting is redacted, some
token-response parse errors retain raw response bytes. The POC maps library
errors immediately into a bounded local reason enum and never logs or returns a
raw `RequestTokenError`.

### Rejected: `cognito-jwt-verify` in the Lambda

An external-index inspection of `cognito-jwt-verify` 0.2.0 found that it does
not match this design's Cognito access-token contract:

- its base claims require user-shaped `sub`, `auth_time`, and `jti` values;
- its `jsonwebtoken::Validation` requires `sub`, enables `validate_aud`, and
  configures app client IDs as JWT audiences;
- Cognito access-token `aud` is present only when resource binding is requested,
  while this design deliberately classifies clients with `client_id`;
- an empty configured client-ID list skips client validation;
- the default permits both ID and access tokens instead of failing closed to
  access tokens; and
- its access-token integration test proves only that a dummy-signature token
  fails, not that a real user or M2M Cognito token succeeds.

The crate's RS256 header check, JWK caching, and exact scope splitting are useful
reference patterns, but adopting or forking it would duplicate API Gateway and
would require correcting the claim and audience model first. It is therefore a
research reference only, not an implementation dependency
([indexed crate source][rust-cognito-jwt-source]).

## Architecture and trust boundaries

```text
Human browser/CLI             Customer server             Internal AWS role
       | PKCE code                  | client secret              | SigV4
       v                            v                            v
+----------------------+   +----------------------+   +----------------------+
| Cognito user pool    |   | Cognito token       |   | API Gateway          |
| authorize/token/JWKS |   | endpoint            |   | $default AWS_IAM     |
+----------+-----------+   +----------+-----------+   +----------+-----------+
           | signed access token      | signed access token                 |
           +---------------+----------+                                     |
                           v                                                |
                 +--------------------------+                               |
                 | API Gateway HTTP API     |<------------------------------+
                 | POST /mcp/oauth: JWT     |
                 | $default: NONE/AWS_IAM   |
                 +------------+-------------+
                              | validated requestContext + untrusted body
                              v
                 +--------------------------+
                 | context-service Lambda   |
                 | semantic claims + exact  |
                 | tool-scope authorization |
                 +------------+-------------+
                              | namespaced caller/owner
                              v
                 +--------------------------+
                 | DynamoDB queue/jobs      |
                 | rate, caps, dedupe, ACL  |
                 +--------------------------+
```

Trust boundaries:

1. The browser and customer server are untrusted. Tokens, request bodies, URLs,
   and tool names are attacker controlled.
2. Cognito is the OAuth authorization server and signing-key authority.
3. API Gateway is the cryptographic JWT verifier. It obtains keys from the
   issuer JWKS, caches public keys for up to two hours, and validates `iss`,
   `aud` or `client_id`, time claims, and broad route scopes
   ([JWT authorizer behavior][aws-http-jwt]).
4. Lambda trusts API Gateway's authorizer context only for an invocation whose
   event and exact route match the configured integration. Its resource policy
   continues to permit API Gateway and EventBridge only. Lambda does not parse
   an unverified `Authorization` header and does not implement a second JWKS
   client.
5. The JSON body is still untrusted after edge authentication. Lambda must map
   the parsed tool name to one exact required scope before calling MCP logic.
6. DynamoDB is the authoritative job and ownership store. A valid token does not
   bypass caller ownership, rate, dedupe, or queue limits.

## Protocol flows

### Optional login redirect facade

The public custom-domain deployment exposes one credential-free convenience
route:

```text
Browser -> GET https://context.getspur.dev/auth/login?<raw authorization query>
API Gateway -> exact NONE route -> existing Lambda integration
Lambda -> validate facade flag + configured Cognito endpoints + bounded raw query
Lambda -> 302 Location: https://auth.context.getspur.dev/oauth2/authorize?<same bytes>
```

This route is a redirect facade, not an OAuth proxy. It exists only when both
`custom_domains_enabled` and `cognito_auth_enabled` are true. Every method other
than `GET`, child path, disabled configuration, malformed endpoint, malformed
percent escape, raw or percent-encoded control byte, fragment delimiter, raw
space/non-URI byte, or query longer than 8,192 bytes fails closed with the
bounded `route_unavailable` response. The successful response has an empty body
and `Cache-Control: no-store`, `Pragma: no-cache`, `Referrer-Policy:
no-referrer`, and `X-Content-Type-Options: nosniff` headers.

The Lambda never reads or forwards request bodies, cookies, authorization
headers, bearer credentials, authorization codes, or token requests on this
route. It appends the validated API Gateway v2 `rawQueryString` byte-for-byte
after `?`; it does not decode, normalize, reorder, or re-encode parameters. The
destination origin and `/oauth2/authorize` path come only from the validated
`SPUR_COGNITO_AUTHORIZATION_ENDPOINT`. Query text that resembles an authority
or contains a `redirect_uri` remains query data and cannot replace the
destination. Cognito remains responsible for enforcing its exact registered
callback allowlist.

OAuth discovery continues to advertise
`https://auth.context.getspur.dev/oauth2/authorize`, and authorization-code or
client-credentials token exchange continues directly at
`https://auth.context.getspur.dev/oauth2/token`. Neither operation is advertised
through `context.getspur.dev/auth/login`.

### Human authorization code with PKCE

```text
Human client -> local state: create random state, nonce, code_verifier
Human client -> Cognito /oauth2/authorize:
  response_type=code, client_id=<human>, exact redirect_uri,
  code_challenge=BASE64URL(SHA256(verifier)), code_challenge_method=S256,
  state=<one-time>, nonce=<one-time>, requested custom scopes
Cognito -> human: authenticate/consent, redirect with code + state
Human client -> local state: constant-time state check; reject mismatch/reuse
Human client -> Cognito /oauth2/token: code + redirect_uri + code_verifier
Cognito -> human client: access, ID, and refresh tokens
Human client -> local verifier: validate ID-token issuer/audience/nonce for login
Human client -> API Gateway POST /mcp/oauth: Authorization: Bearer <access token>
API Gateway -> Lambda: validated claims and original body
Lambda -> Lambda: token_use/access + client/sub + exact tool/scope checks
Lambda -> service: caller_id = cognito:user:<sub>
```

The human app client is public (`generate_secret=false`) and allows only the
code grant. The SPUR human client implementation MUST generate a unique PKCE
verifier for every authorization request, send its `S256` challenge, and send
the verifier only at token exchange. Cognito verifies the verifier against the
challenge when PKCE is supplied ([Cognito PKCE][aws-pkce]). The client also uses
a one-time, session-bound `state` and OIDC `nonce`. Current OAuth security
guidance requires public clients to use PKCE and recommends `S256`; it also
requires CSRF protection and exact redirect matching ([RFC 9700][rfc9700],
[RFC 7636][rfc7636]). Authorization-code-only configuration disables the
implicit grant, but Cognito app-client configuration has no control that proves
every authorize request included a PKCE challenge. PKCE use is therefore a
mandatory SPUR client contract, not an infrastructure-enforced app-client
property ([authorize endpoint][aws-authz], [token endpoint][aws-token]).

### Customer M2M client credentials

```text
Customer secret store -> integration: client_id + active client_secret
Integration -> local token cache: lookup by client_id + normalized scope set
[cache miss or refresh threshold]
Integration -> Cognito /oauth2/token:
  Authorization: Basic base64(client_id:client_secret)
  grant_type=client_credentials
  scope=<least required custom scopes>
Cognito -> integration: access token only, expires_in
Integration -> cache: encrypt at rest; refresh near 80% lifetime with jitter
Integration -> API Gateway POST /mcp/oauth: Bearer <access token>
API Gateway -> Lambda: validated claims and body
Lambda -> service: caller_id = cognito:client:<client_id>
```

Client credentials are for confidential clients and return an access token, not
an ID or refresh token ([RFC 6749 §4.4][rfc6749], [Cognito app clients][aws-app-clients]).
Each app client enables only `client_credentials` and a least-privilege subset
of custom scopes. Cognito requires a domain, client secret, resource-server
custom scopes, and the token endpoint for this grant
([Cognito M2M and scopes][aws-resource-server]).

### Internal IAM invocation

```text
AWS workload -> AWS credential provider: short-lived role credentials
AWS workload -> API Gateway $default: SigV4-signed POST
API Gateway -> IAM: authenticate and authorize execute-api:Invoke
API Gateway -> Lambda: IAM requestContext and body
Lambda -> service: caller_id = iam:<account_id>:<principal_unique_id>
Lambda -> DynamoDB: existing ownership, rate, dedupe, and queue operations
```

IAM callers never request Cognito tokens. Their IAM policy should be narrowed
from the current all-method resource to the intended stage/method/path when the
explicit legacy path is finalized. API Gateway accepts SigV4/SigV4a for routes
configured as `AWS_IAM` ([API Gateway IAM invocation][aws-iam-invoke]).

## API Gateway route contract

| Route | Authorization | Allowed workload | Lambda policy |
|---|---|---|---|
| `POST /mcp/oauth` | `JWT`; Cognito issuer; configured app-client audiences; route scopes are all three custom scopes | Human PKCE and external M2M | Only `external_*`; exact tool scope required |
| `$default` in demo | `NONE` | Existing demo clients | Preserve current reads; index/status use literal legacy `anonymous-internal` only when `allow_anonymous_mutations=true` |
| `$default` elsewhere | `AWS_IAM` | Internal AWS workloads | Preserve current tool surface; authenticated IAM identity required for index/status |
| EventBridge scheduled event | Lambda resource policy, not API Gateway | Queue drainer | Existing scheduled-event discriminator; never enters HTTP auth |

The JWT route's `authorization_scopes` contains all three custom scopes. API
Gateway treats this list as **any-of**, which is only the broad edge gate. It
cannot inspect `request.body.tool`; Lambda treats the matrix below as **one
exact required scope**.

Missing, malformed, bad-signature, expired, wrong-issuer, or wrong-audience
tokens stop at API Gateway with 401. A token with none of the route scopes stops
with 403. Lambda returns 401 for malformed semantic claims and 403 for a valid
identity missing the exact tool scope. `external_index_status` continues to
return `not_found` for a different valid owner, preventing job enumeration.

## Cognito resources and app-client configuration

### User pool

- Explicitly select feature plan `LITE`; do not rely on the current new-pool
  default, which is Essentials.
- Enable deletion protection outside the disposable POC.
- Use direct sign-in and approved social providers. Enterprise SAML/OIDC users
  are a distinct cost category and require a revised estimate.
- Prefer software-token (TOTP) MFA when MFA is required. SMS/email transport
  charges are not part of the Cognito MAU estimate.
- Enable token revocation for the human client. Lite does not supply refresh
  token rotation, so the human client must protect refresh tokens and revoke
  sessions explicitly.
- Configure a Cognito domain for `/oauth2/authorize`, `/oauth2/token`, and JWKS.
- Register exact callback and logout URLs. Wildcards are forbidden.

### Resource server and scopes

Use one resource-server identifier supplied as a variable; examples in tests and
docs use `urn:spur:context-service`, never an account-specific value. Define:

- `urn:spur:context-service/external.read`
- `urn:spur:context-service/external.index`
- `urn:spur:context-service/external.status`

Cognito writes custom scopes to the access token's space-delimited `scope`
claim. The implementation compares complete, case-sensitive tokens; it never
uses substring or prefix matching.

### Human public app client

- no client secret;
- authorization-code grant only (no implicit grant);
- callback/logout URL allowlists from Terraform variables;
- `openid` and, only when needed, `profile`/`email`, plus the custom scopes;
- access and ID token lifetime 60 minutes;
- refresh token lifetime 30 days, subject to product session policy; and
- prevent user-existence errors where compatible with the login experience.

### M2M organization app clients

- one stable client per organization, `for_each` over an opaque organization
  key that is safe for Terraform addresses and tags;
- generated secret, client-credentials grant only, no callback URL, no OIDC
  scopes, and no user authentication flows;
- allowed custom scopes are a validated least-privilege subset;
- 6-hour access-token lifetime by default;
- optional 24-hour cost-minimum lifetime only after a recorded risk acceptance;
  and
- at most two active secrets, used for zero-downtime rotation. Cognito now
  supports adding a second secret and deleting the old one
  ([app-client secret rotation][aws-client-secret],
  [AddUserPoolClientSecret][aws-add-secret]).

### Native-authorizer audience limit

Cognito access tokens normally carry the app client in `client_id`; a human
resource-binding request can add `aud`. API Gateway checks `aud` when present
and otherwise checks `client_id` against the configured audiences
([JWT authorizer behavior][aws-http-jwt]). V1 therefore:

- does not request Cognito resource binding;
- configures authorizer audiences as the human client ID plus every enabled M2M
  client ID;
- requires Lambda to check `client_id` even if an `aud` claim appears; and
- fails Terraform validation when `1 + enabled_m2m_organizations > 50`.

Fifty audiences per authorizer is a fixed, non-increasable HTTP API quota
([HTTP API quotas][aws-http-quotas]). Therefore one OAuth endpoint supports one
human client plus at most 49 organization clients. Before onboarding the 50th
organization, choose audience-sharded exact routes (each still body-routed),
multiple APIs, or a reviewed Lambda-authorizer design. Do not silently drop an
organization from the audience list.

## Exact tool-to-scope authorization matrix

| Body `tool` | Required scope | Read/mutation | Ownership effect |
|---|---|---|---|
| `external_catalog` | `urn:spur:context-service/external.read` | Read | None |
| `external_code_search` | `urn:spur:context-service/external.read` | Read | None |
| `external_code_read` | `urn:spur:context-service/external.read` | Read | None |
| `external_code_callers` | `urn:spur:context-service/external.read` | Read | None |
| `external_code_callees` | `urn:spur:context-service/external.read` | Read | None |
| `external_knowledge_context` | `urn:spur:context-service/external.read` | Read | None |
| `external_index` | `urn:spur:context-service/external.index` | Mutation | Rate, dedupe, queue, and job caller use the namespaced identity |
| `external_index_status` | `urn:spur:context-service/external.status` | Read control plane | Record caller must exactly match the namespaced identity |

An M2M client intended to submit and poll jobs normally receives `external.index`
and `external.status`; read-only integrations receive only `external.read`.
Granting `external.index` does not imply status or catalog read. Human clients
request only the scopes their product surface uses.

To prevent matrix drift, a test obtains all MCP tool definitions, filters names
starting with `external_`, and asserts that the set equals the policy-map keys.
Adding a ninth external tool without a scope then fails CI closed.

## Claim validation and caller identity contract

Lambda receives claims only after API Gateway JWT validation, but applies this
semantic contract before using any claim:

1. Determine the request path and authorizer shape. The exact OAuth route must
   have JWT context; the secure legacy route must have IAM context. A JWT-shaped
   request never falls back to IAM, `principal_id`, source IP, or anonymous.
2. Require exact configured `iss`, `token_use == "access"`, nonblank `client_id`,
   numeric unexpired `exp`, and a syntactically valid space-delimited `scope`.
   API Gateway already enforces signature and time; rechecking semantics makes
   the application contract explicit and testable.
3. Require `client_id` in the configured allowlist and absent from the emergency
   denylist. If `aud` is present, require it to match the configured API audience
   policy as well; never let `aud` suppress the client allowlist.
4. If `client_id` equals the human public client, require a nonblank `sub` and
   derive `cognito:user:<sub>`. Cognito advises treating `sub` as opaque rather
   than requiring an RFC UUID shape ([access-token claims][aws-access-token]).
5. Otherwise require `client_id` in the M2M client set and derive
   `cognito:client:<client_id>`, regardless of an unexpected `sub` claim.
6. Parse scopes into a set and require the matrix entry for the parsed tool.

Claim values used in keys must be 1–256 UTF-8 bytes after trimming and must not
contain NUL or ASCII control characters. Values are not lowercased. Cognito
`client_id` and `sub` are opaque, case-sensitive identifiers. The namespace and
delimiter are added by trusted code, so a user `sub` equal to a client ID cannot
collide with an M2M caller.

For IAM, use `account_id` plus the stable unique-principal prefix from `user_id`
(the portion before an STS session-name colon):
`iam:<account_id>:<principal_unique_id>`. This avoids making an assumed-role
session name part of queue ownership. If either value is absent or malformed,
use a canonical IAM `user_arn` only for an IAM user; otherwise reject. Do not
fall back to source IP. The existing `principal_id` and `identity.user_arn`
fallbacks remain available only on the legacy path during migration.

Legacy `NONE` mutations retain the literal caller `anonymous-internal` so
existing demo jobs remain pollable. Changing this identifier, including to the
read-path fallback `anonymous`, would orphan existing demo jobs from status
ownership. That compatibility exception is never produced on the OAuth or
IAM-strict paths.

## Queue, dedupe, rate-limit, and status integration

No DynamoDB schema migration is required. The current flow already passes a
caller string through all relevant controls:

- `check_index_rate_limit(caller_id, ...)` becomes per human, per M2M
  organization client, or per IAM principal.
- `backlog_owner_from_caller(caller_id)` continues to create
  `BacklogOwnerKind::Caller`; its ID is now namespaced.
- `DynamoDbJobStore::enqueue_job` keeps active canonical dedupe global to the
  package coordinate, while owner queue counters and job access retain the
  authenticated caller. A duplicate created by another caller must not expose
  that caller's job details; tests must confirm the existing response contract.
- Hard per-owner queued/running caps naturally isolate organizations because
  every organization has a distinct app client.
- Global queued/running caps, queue shards, and drainer fairness are unchanged.
- `route_index_status_inner` continues exact string comparison and returns
  `not_found` for a different caller.

Secret rotation does not change an organization client ID and therefore does
not orphan active jobs. If an app client must be deleted and recreated, operators
must either wait for its active jobs to become terminal or use an IAM operator
path to inspect them. V1 does not add a caller-alias table.

## Terraform design

### Resources

Add, guarded by `cognito_auth_enabled`:

- `aws_cognito_user_pool.context_service` with explicit Lite tier and deletion
  protection outside POC;
- `aws_cognito_user_pool_domain.context_service`;
- `aws_cognito_resource_server.context_service` with the three scopes;
- `aws_cognito_user_pool_client.human`;
- `aws_cognito_user_pool_client.m2m` using `for_each`;
- `aws_apigatewayv2_authorizer.cognito` with issuer, identity source
  `$request.header.Authorization`, and the complete audience list;
- `aws_apigatewayv2_route.oauth` with `POST /mcp/oauth`, the existing Lambda
  integration, JWT authorization, and all three broad route scopes;
- `aws_apigatewayv2_route.login_redirect` with exact `GET /auth/login`, the
  existing Lambda integration, and explicit `NONE` authorization only when
  Cognito and custom domains are enabled;
- access-log configuration and focused CloudWatch alarms; and
- optional `aws_budgets_budget.cognito` when a budget and subscribers are set.

Keep `aws_apigatewayv2_route.default` unchanged. Do not set its authorizer ID
when its authorization type is `NONE` or `AWS_IAM`. The current provider
constraint is `hashicorp/aws ~> 5.0`; implementation must prove in the isolated
POC that the selected provider version supports the explicit Cognito feature
plan. If it does not, pin the minimum compatible provider version in a separate
reviewed implementation change rather than omitting `LITE`.

The production ingress is `https://context.getspur.dev`; the hosted OAuth origin
is `https://auth.context.getspur.dev`. Terraform always bootstraps a delegated
Route 53 zone for `context.getspur.dev`, while certificates, validation records,
API Gateway mapping/aliases, and the Cognito custom domain remain guarded by the
false-by-default `custom_domains_enabled` activation switch. The parent zone
stays at Namecheap. A separate `disable_execute_api_endpoint` switch may become
true only after activation, E2E, and released-client migration. It never controls
the Cognito prefix domain, whose optional removal is a later reviewed change.

The regional Cognito issuer remains
`https://cognito-idp.<region>.amazonaws.com/<pool-id>` after custom-domain
activation. OIDC discovery and JWKS use that issuer; only the advertised
authorization and token endpoints move to
`https://auth.context.getspur.dev/oauth2/authorize` and `/oauth2/token`.

### Variables

| Variable | Type/default | Validation/meaning |
|---|---|---|
| `cognito_auth_enabled` | `bool`, `false` | Creates no Cognito/JWT-route resources by default. |
| `cognito_user_pool_name` | `string` | Nonblank, environment-qualified name. |
| `cognito_domain_prefix` | `string` | Cognito-compatible, unique, no account data in examples. |
| `cognito_resource_server_identifier` | `string`, `urn:spur:context-service` | Nonblank and stable after launch. |
| `cognito_human_callback_urls` | `set(string)` | Nonempty when enabled; HTTPS except loopback POC; exact URLs only. |
| `cognito_human_logout_urls` | `set(string)` | Same URL policy. |
| `cognito_human_access_token_minutes` | `number`, `60` | 5–1,440; production recommendation 60. |
| `cognito_human_refresh_token_days` | `number`, `30` | Must match product session policy. |
| `cognito_m2m_organizations` | map object | Opaque org key, display label, enabled flag, allowed scope set, `access_token_hours`; enabled count plus human must be ≤50. |
| `cognito_m2m_default_access_token_hours` | `number`, `6` | 1–24; org override of 24 requires risk-acceptance annotation. |
| `cognito_emergency_deny_client_ids` | `set(string)`, empty | Immediately fail closed in Lambda after config deployment; client IDs are not secrets but treat the list as restricted metadata. |
| `cognito_monthly_budget_usd` | nullable number | No budget resource when null. |
| `cognito_budget_subscriber_emails` | `set(string)` | Mark sensitive because it contains personal contact data. |

Do not put app-client secrets, tokens, or real identifiers in tfvars. Organization
display names and scope assignments are configuration, not bearer credentials.

### Lambda environment

Pass only non-secret validation data:

- `SPUR_COGNITO_AUTH_ENABLED`
- `SPUR_COGNITO_ISSUER`
- `SPUR_COGNITO_HUMAN_CLIENT_ID`
- `SPUR_COGNITO_M2M_CLIENT_IDS`
- `SPUR_COGNITO_RESOURCE_SERVER_ID`
- `SPUR_COGNITO_DENY_CLIENT_IDS`
- `SPUR_COGNITO_OAUTH_PATH`
- `SPUR_CONTEXT_LOGIN_REDIRECT_ENABLED`

The audience cap bounds these values below Lambda's environment-size concern.
Lambda never needs a Cognito client secret.

### Outputs and secret/state handling

Add nonsensitive outputs for the effective `api_url`, `cognito_issuer`,
`cognito_domain_url`, `cognito_authorization_endpoint`,
`cognito_token_endpoint`, `cognito_human_client_id`,
`cognito_m2m_client_ids`, `cognito_resource_server_identifier`,
`oauth_api_url`, and API-key route URLs. Before activation the effective outputs
retain the execute-api and Cognito prefix compatibility endpoints; after
activation they use the two stable custom domains. Add secret ARNs only if
provisioning writes generated customer secrets to Secrets Manager. Never output
secret values.

Terraform providers can retain generated app-client secrets in state even when
an output is marked `sensitive`; that flag masks CLI display but does not encrypt
state. The remote state bucket therefore requires encryption, versioning,
restricted IAM, access logging, and a reviewed retention policy. CI must not
publish plan/state artifacts. Customer secret delivery is one-time through an
approved secret manager or equivalent secure channel, with access audited and
revoked after acknowledgment. Plaintext email, chat, issue comments, and shell
history are prohibited.

Secret rotation is an operational transaction:

1. Add the second secret for the existing app client.
2. Deliver it through the approved secret channel.
3. Customer deploys it and proves a new token can invoke an allowed scope.
4. Observe that the old secret is no longer used for a full agreed overlap.
5. Delete the old secret, preserving the app client ID.
6. Record client ID hash, secret descriptor ID, actor, and timestamps—never the
   secret value.

If the Terraform AWS provider does not yet model the two-secret APIs, keep the
app client Terraform-owned and perform steps 1/5 with the Cognito API under a
restricted rotation role; document that subresource as intentional operational
state rather than forcing app-client replacement.

## Rust impact map

| File/symbol | Planned responsibility |
|---|---|
| Create `crates/spur-context-service/src/auth.rs` | Typed `AuthScheme`, `PrincipalKind`, `CallerIdentity`, `AuthDecision`, claim parser, IAM parser, scope set, exact external-tool policy, reason enums, and unit tests. No JWT signature library or network fetch. |
| `src/lib.rs` | Register the new private auth module. |
| `src/lambda.rs::ApiGatewayRequest*` | Retain route/raw-path fields and the API Gateway v2 raw query string; never reconstruct the login query from `queryStringParameters`. Extend IAM authorizer parsing only if the payload fixture proves another stable field is required. |
| `src/lambda.rs::handle_api_gateway_request` | Handle the exact login redirect and discovery before body parsing; otherwise parse body, classify route/auth, call `authorize_external_tool`, reject non-external tools on OAuth route, and pass the namespaced caller to index/status. Keep scheduled drainer handling before HTTP auth. |
| `src/lambda.rs::authenticated_caller_id`, `jwt_caller_id`, `iam_caller_id` | Replace OAuth/IAM-strict use with typed auth; retain narrowly named legacy fallback for `$default` compatibility. |
| `src/lambda.rs::auth_error_response` | Distinguish 401 authentication from 403 authorization without echoing claim/token data. |
| `src/mcp.rs` tool definitions | Expose or test the authoritative external-tool name set so policy-map exhaustiveness cannot drift. Routing and input schemas remain unchanged. |
| `src/mcp.rs::route_index_inner`, `route_index_status_inner` | No behavioral rewrite; add focused tests proving namespaced owner/rate and exact status ownership. |
| `src/jobs.rs::BacklogOwner`, `JobRecord`, `enqueue_job` | No schema change. Add tests for namespaced caller strings in owner keys and records. |
| Create `infra/spur-context-service/poc/auth-client/` | Standalone Rust package for human PKCE and M2M token acquisition. Use `oauth2` 5.x, `openidconnect` 4.x, and redirect-disabled Rustls `reqwest`; keep these out of the Lambda dependency graph. |
| `infra/spur-context-service/main.tf` | Cognito resources, authorizer, exact OAuth route, environment values, alarms, optional budget. |
| `infra/spur-context-service/variables.tf` | Feature flag, client, URL, TTL, org/scope, denylist, and budget contracts with validations. |
| `infra/spur-context-service/outputs.tf` | Public discovery values and IDs only; no secrets. |
| `infra/spur-context-service/env/*.tfvars` | Leave demo NONE behavior explicit; enable Cognito first in staging with placeholders supplied outside version control; production remains IAM until rollout gate. |

## Security and threat analysis

| Threat | Control | Residual risk / operator action |
|---|---|---|
| Stolen M2M secret | Secret manager, least-privilege scope, two-secret rotation, per-org client, audit access, delete compromised secret | Attacker can mint tokens until secret deletion; emergency denylist the client ID and rotate. |
| Stolen bearer access token | TLS, protected cache, log redaction, short TTL profile, per-owner rate/queue caps | Cognito tokens are bearer tokens, not sender constrained. A 24-hour token has up to a 24-hour replay window. Use 6 hours for higher-risk clients. |
| Non-immediate revocation | App-client deletion/secret rotation stops new issuance; Lambda denylist blocks a client ID after config rollout | Offline signature/expiry verification still accepts a previously issued revoked JWT until expiry. AWS explicitly notes signature/expiry-only libraries still accept revoked tokens ([token revocation][aws-revocation]). |
| Authorization-code interception/injection | SPUR human client always sends a unique PKCE `S256` challenge, exact redirect URI, state, nonce, and single-use verifier; Cognito verifies PKCE when supplied | Cognito app-client configuration cannot enforce challenge presence on every authorize request. Client compliance is mandatory and must be tested; compromised client storage/browser remains in scope. |
| CSRF/login swap | One-time session-bound `state`, PKCE, nonce validation | Client implementation must never continue after mismatch. |
| Token-endpoint redirect or SSRF | Standalone Rust client uses a fixed configured Cognito domain, HTTPS, Rustls, redirect policy `none`, and bounded timeouts | Operators must not derive token endpoints, proxies, or redirect policy from request input. DNS and host policy remain part of POC review. |
| Login-facade open redirect, CRLF, or query confusion | Exact public GET route; fixed validated Cognito authorization endpoint; byte-preserving URI-query allowlist; control/fragment/malformed-percent rejection; 8,192-byte cap; empty no-store response | Cognito still validates caller-supplied OAuth parameters and exact callback registration; the facade never handles tokens or codes. |
| ID token used as API token | Broad route scopes plus Lambda `token_use=access` | API Gateway alone cannot universally distinguish JWT types; Lambda check is mandatory. |
| Wrong tenant/client | API Gateway audience list, Lambda client allowlist, explicit human-client classification, namespaced ID | V1 audience quota limits one route to 49 M2M orgs. |
| Scope confusion | Exact full-token scope comparison and exhaustive tool matrix | Any new external tool fails CI and must receive a reviewed scope. |
| Auth downgrade | JWT context has precedence and fails closed; no fallback on malformed claims; OAuth route rejects IAM/anonymous | Legacy NONE remains intentionally public until separately retired. |
| Queue abuse | Existing URL/DNS checks, per-owner request rate, dedupe, hard per-owner/global queue and running caps, API throttles | One compromised organization can exhaust only its owner cap plus contend for global capacity. |
| Cross-owner status probing | Exact namespaced caller match, mismatch returns `not_found` | IAM operator access must be separately audited. |
| Secret/token leakage in telemetry | Never log Authorization, token, secret, authorization code, verifier, or full claims; structured reason codes only | Debug tooling and API Gateway access-log formats require review before deploy. |
| JWKS/key rotation | API Gateway-managed JWKS retrieval; Cognito retains overlapping keys; allow at least the documented two-hour API Gateway cache grace | A badly timed forced key removal can reject new tokens until cache refresh. Alarm on 401 spikes. |
| Cost/issuance abuse | Cache tokens, token-endpoint quotas, per-client usage dashboard, budget and anomaly alerts | M2M has no free token-response tier; investigate cache misses and client fan-out. |

Cognito access tokens can be configured from five minutes to one day
([access-token lifetime][aws-access-token], [Cognito quotas][aws-cognito-quotas]).
The maximum is a cost knob, not a revocation control.

The production backend must not add `cognito-jwt-verify`, `jsonwebtoken`,
`oauth2`, or `openidconnect` merely to interpret API Gateway authorizer claims.
Claim parsing and exact scope authorization use existing structured event/Serde
types. The standalone POC package is the only approved location for OAuth/OIDC
client dependencies in v1.

## Cost model and controls

### Human users

At the currently published Lite direct/social rate, 50,000 MAUs in an account
with the 10,000-MAU free tier produce 40,000 billed MAUs. The first paid tier is
USD 0.0055/MAU, so the illustrative baseline is:

```text
(50,000 - 10,000) × USD 0.0055 ≈ USD 220/month
```

This is not a quote. Eligibility, region, enterprise federation, messages,
advanced security, higher quotas, multi-Region replication, and downstream AWS
services can change the bill. Older eligible accounts can have a different Lite
free tier. Recalculate with the AWS Pricing Calculator at deployment
([Cognito pricing][aws-pricing]).

### M2M organizations

Cognito bills successful M2M token responses; it does not add a separate charge
for each registered app client. There is no M2M free tier
([Cognito pricing][aws-pricing]). For organization `o`:

```text
refresh_interval_hours(o)
  = token_ttl_hours(o) × refresh_fraction(o)

monthly_token_responses(o)
  ≈ active_cache_principals(o)
    × ceil(active_hours(o) / refresh_interval_hours(o))

monthly_M2M_cost
  = sum(monthly_token_responses(o)) × current_region_token_response_rate
```

With the recommended `refresh_fraction = 0.80`, one shared cache principal
continuously active for a 720-hour, 30-day month requests approximately 38
tokens at a 24-hour TTL, 150 at 6 hours, or 900 at 1 hour. These estimates are
subject to the initial request, downtime, randomized jitter, and interval
rounding. Horizontal replicas that do not share a cache multiply token
responses. The client must single-flight concurrent cache misses and refresh at
approximately 80% lifetime with randomized jitter.

Profiles:

| Profile | TTL | Approx. responses/month per continuously active cache | Tradeoff |
|---|---:|---:|---|
| Cost-minimum | 24h | 38 | Lowest issuance cost; largest theft/revocation blast radius |
| Balanced (default) | 6h | 150 | About four times issuance volume; caps normal replay exposure near six hours |
| High-security | 1h | 900 | Faster expiry; highest issuance volume and token-endpoint dependency |

### Governance

- Tag resources with environment, service, owner, managed-by, and cost-center.
- Dashboard human MAUs, successful M2M token responses per app client, active
  client count, and forecast versus budget.
- Forecast M2M responses with the configured refresh fraction and active shared
  cache-principal count; compare actual responses to that early-refresh model.
- Alert at 50/80/100% forecast or actual budget thresholds and on anomalous M2M
  token-response growth.
- Review TTL risk acceptances quarterly and downgrade inactive clients/scopes.
- Alert before 49 enabled M2M clients and before Cognito's published guidance to
  contact the account team above 2,500 app clients.
- Keep IAM internal traffic off the Cognito token endpoint.

## Observability and audit contract

### Structured application fields

Every OAuth/IAM decision emits one structured event with:

- API Gateway request ID and route (not the bearer header),
- tool name,
- `auth_scheme` (`cognito_user`, `cognito_client`, `iam`, `legacy_anonymous`),
- `principal_hash` (SHA-256 of the complete namespaced identity, truncated only
  for display), never raw `sub` or full client ID,
- required scope and decision (`allow`, `deny`),
- bounded reason enum (`missing_context`, `wrong_issuer`, `wrong_token_use`,
  `unknown_client`, `denylisted_client`, `malformed_subject`,
  `missing_scope`, `wrong_route`),
- job ID and queue outcome when applicable, and
- latency and cold-start marker.

Do not log tokens, secrets, authorization codes, PKCE verifier/challenge, refresh
tokens, the Authorization header, full claims, source URLs containing credentials,
or Terraform secret/state values.

### Metrics and alarms

- `AuthDecisionCount{scheme,tool,decision,reason}`
- `AuthLatencyMs{scheme}`
- `UnknownClientCount` and `DenylistedClientAttemptCount`
- API Gateway 401/403/429 and Lambda 5xx by route
- existing `rejection_count{reason}`, queue depth, dispatch latency, owner/global
  saturation, requeue, and stuck-dispatch metrics segmented by principal kind
- Cognito successful/failed token operations where the service exposes them,
  plus billing-derived M2M token-response counts

Alarm on denial/unknown-client spikes, any denylisted-client attempt, OAuth-route
5xx, sustained 429s, JWT 401 spikes during signing-key changes, queue saturation,
and budget forecasts. One dashboard combines edge auth, Lambda authorization,
queue health, Cognito usage, and estimated spend.

## Backend-only isolated POC

The POC is a separate Terraform root/state and must not reference a production
API, Lambda alias, DynamoDB table, state machine, user pool, domain, secret, or
IAM invoke policy. Prefer a sandbox account. If policy requires the same account,
use a unique random suffix, dedicated tags, a dedicated state backend key, a
dedicated Lambda/version and job table, and a zero/low queue cap. Never import
production resources into POC state.

### Procedure

1. Record baseline production Terraform plan and resource inventory; it must be
   unchanged by every POC command.
2. Build the backend from the candidate commit through `scripts/spur-cargo`; do
   not deploy an uncommitted local binary.
3. Build and test the standalone Rust POC client through
   `scripts/spur-cargo --dir infra/spur-context-service/poc/auth-client`.
   Configure its reusable
   Rustls `reqwest` client with redirects disabled and bounded connect/request
   timeouts. Read client secrets from the approved secret channel or environment,
   never from command-line arguments, source files, or fixture snapshots.
4. Create an isolated Lite pool, domain, resource server, public PKCE client,
   two M2M clients with different scope subsets, JWT authorizer, exact OAuth
   route, POC Lambda, POC log group, and POC DynamoDB job table.
5. Set the POC index queue/running limits so no production Step Functions or
   workers can start. Use an invalid-but-safe source URL to prove a scoped
   `external_index` request reached application validation without outbound
   fetch or job dispatch.
6. Capture sanitized request IDs, HTTP statuses, response reason enums, decoded
   non-secret claim names, Terraform resource addresses, and CloudWatch metrics.
7. Run the objective matrix below.
8. Re-run the production plan and prove zero changes.
9. Destroy the isolated POC from its own state; verify the user pool, domain,
   clients, API, Lambda, table, log group, and secrets are gone. Retain only
   sanitized evidence under the issue's approved artifact policy.

### Objective POC evidence

The POC proves the approved SPUR client sends PKCE and that Cognito rejects a
missing or mismatched verifier for a code issued with a challenge. It must not
claim that Cognito rejects an authorization request that omits a challenge
unless the live POC captures that behavior; authorization-code-only app-client
configuration is not itself proof of PKCE enforcement.

| Case | Expected evidence |
|---|---|
| Human code + PKCE S256 | Sanitized client trace shows a unique `S256` challenge; matching verifier returns access/ID/refresh; API accepts access token only |
| Human OIDC validation | `openidconnect` verifies issuer, audience, signature, nonce, and any supplied access-token hash before the client accepts the login |
| Missing/wrong verifier for a code issued with a challenge, or reused `state` | Client/token exchange fails; API is never called |
| M2M Basic authentication | Mock/live evidence shows the outbound request carries the secret only in the token-endpoint Basic header, requests exact scopes, and emits no secret/token in application logs |
| Token endpoint redirect | Redirect response is not followed; no Authorization header or body is replayed to the redirect target |
| M2M allowed read | Token cache is keyed by client plus normalized scopes; read tool succeeds with `cognito:client` principal kind in hashed audit |
| M2M token without user claims | Valid access token with `client_id`, `token_use=access`, and scope but no human `sub` is accepted as `cognito:client:<client_id>` |
| M2M missing exact scope | Edge broad scope may pass; Lambda returns 403 `missing_scope` |
| ID token on API | Rejected, never reaches tool execution |
| Expired/wrong issuer/wrong client | API Gateway 401 |
| Different M2M owner polls job | `not_found`, with no record details |
| 24-hour profile | `exp - iat` equals configured lifetime within service rounding; risk acceptance linked |
| Secret overlap | Both active secrets mint tokens during overlap; old secret fails after deletion |
| Previously issued token after secret deletion | Remains edge-valid until expiry unless client denylisted, demonstrating blast radius |
| IAM legacy request | SigV4 path still works without Cognito token issuance |
| Demo compatibility fixture | NONE plus anonymous flag yields literal `anonymous-internal` and preserves existing status ownership in the local/integration fixture only |
| Teardown | Empty tagged-resource inventory and production plan unchanged |

## Test strategy

### Rust unit tests

- all eight external tool names are present once in the scope map;
- any unknown `external_*` name fails closed;
- scope parsing is whitespace-delimited, exact, case-sensitive, and duplicate-safe;
- human client + opaque `sub` yields `cognito:user:<sub>`;
- M2M client without `sub` or resource-binding `aud` yields
  `cognito:client:<client_id>`; an unexpected `sub` does not change that owner;
- human missing/blank/control-character `sub` fails;
- missing/unknown/denylisted `client_id`, wrong issuer, wrong `token_use`, expired
  or malformed `exp`, malformed scope, and unexpected audience fail;
- malformed JWT context cannot fall back to IAM/principal/anonymous;
- stable IAM `user_id` prefix ignores STS session name;
- missing stable IAM fields fail rather than use source IP;
- OAuth route rejects non-external tools;
- 401 and 403 bodies expose bounded reasons but no claims/token; and
- legacy fallback and `anonymous-internal` fixtures remain byte-compatible where
  required.

The standalone POC client has mock-server tests proving:

- `client_credentials` uses Basic authentication and exact scopes;
- client-secret debug formatting is redacted, and a local error mapper prevents
  raw token-response bodies or bearer tokens from reaching logs/responses;
- redirects are rejected rather than followed with credentials;
- cache keys include client ID and normalized scope set, concurrent misses are
  single-flight, and refresh timing applies jitter without exceeding expiry;
- PKCE uses a fresh S256 challenge/verifier and one-time state/nonce per attempt;
  and
- wrong state, nonce, verifier, issuer, audience, or access-token hash fails
  closed.

### Service and queue tests

- each matrix row accepts its scope and rejects the other two;
- `external.index` alone cannot poll status;
- namespaced callers drive rate-limit keys and `BacklogOwner::caller` keys;
- two users, two M2M clients, and one IAM role have distinct owner counters;
- same organization secret rotation retains the same owner;
- cross-caller status remains `not_found`;
- queue-full, global-queue-full, dedupe, force, warm-hit, and retry behavior are
  unchanged; and
- scheduled drainer events do not pass through request authorization.

### Terraform/static tests

- `scripts/spur-cargo fmt --all -- --check` for Rust changes and `terraform fmt
  -check -recursive` for infrastructure changes;
- `terraform init -backend=false` and `terraform validate`;
- plans for Cognito disabled, demo NONE, staging JWT enabled, and production IAM
  plus JWT route;
- assertions that disabled mode creates no Cognito/JWT-route resources;
- assertions that the login facade is absent until custom-domain activation,
  then uses exact `GET /auth/login` with explicit `NONE` authorization while
  discovery and token endpoints become exact
  `https://auth.context.getspur.dev/oauth2/authorize` and `/oauth2/token` output
  values while the regional issuer remains unchanged;
- validation failure for zero callback URLs, invalid TTL/scope, secret-bearing
  tfvars, and more than 50 audiences; and
- plan/state-output scan proving no client secret is output or logged.

### Integration/e2e matrix

Cover edge 401/403 behavior, every tool/scope pair, human and M2M identity,
client denylist, IAM compatibility, NONE compatibility, queue isolation, secret
overlap, and sanitized logging. Tests that compile Rust must use
`scripts/spur-cargo`, never bare `cargo`.

## Implementation phases and deployment runbook

### Phase 0 — Contract and test fixtures

Implement typed auth and exhaustive matrix tests behind
`cognito_auth_enabled=false`. Add representative API Gateway v2 Cognito, IAM,
malformed, and EventBridge fixtures. Add explicit M2M fixtures without `sub` or
`aud`. Create the standalone POC client package and its mock HTTP tests, without
adding OAuth/OIDC dependencies to `spur-context-service`. No Terraform resource
exists yet.

### Phase 1 — Isolated POC

Run the backend-only POC above. Gate progression on the complete evidence matrix,
provider compatibility, no secret leakage, teardown, and unchanged production
plan.

### Phase 2 — Staging dark launch

Create staging Cognito resources and `POST /mcp/oauth`, while retaining staging
`$default` as `AWS_IAM`. Allow only test clients. Verify dashboards, costs,
scopes, queue isolation, and rollback. The JWT route enforces from its first
request; there is no permissive production shadow mode.

### Phase 3 — Human and M2M pilot

Enable the human client and a small set of organization clients. Begin with the
6-hour M2M profile unless a 24-hour risk acceptance is recorded. Narrow each
organization scope bundle. Run secret-rotation and denylist drills.

### Phase 4 — Production rollout

1. Confirm callbacks, organization count (<50 total audiences), scopes, TTL
   approvals, budget, alarm destinations, state protection, support owner, and
   that the built-in client default is `https://context.getspur.dev` without
   removing explicit URL override compatibility.
2. Apply only the Route 53 zone bootstrap. Copy its four authoritative NS values
   into `context` NS records at Namecheap, leaving the root zone and unrelated
   records untouched. Wait until public DNS returns the identical set.
3. Activate the regional API certificate, us-east-1 Cognito certificate, API
   Gateway custom domain/mapping, Cognito custom domain, and Route 53 aliases.
   Keep execute-api and the Cognito prefix domain enabled.
4. Fetch OIDC discovery from the regional `cognito_issuer` and require its
   advertised authorization and token endpoints to equal the stable auth-domain
   outputs. Fetch service discovery from `https://context.getspur.dev` and
   compare every advertised route with Terraform output.
5. Complete OAuth authorization-code plus PKCE and an allowed OAuth MCP call;
   create/use/revoke a personal API key and exercise the API-key MCP route; then
   run index/status/read MCP E2E. Confirm wrong-scope, missing, malformed, and
   revoked credentials fail closed and logs contain no credentials.
6. Smoke IAM SigV4 and confirm no Cognito token response for that call. Watch
   401/403/429, Lambda errors, queue saturation, token issuance, and budget for
   the agreed pre-release soak period.
7. Release clients with the stable custom-domain default only after all E2E
   evidence passes. Confirm a released client works without a URL override and
   that an explicit legacy or staging URL still wins.
8. Optionally disable execute-api in a separate saved plan after migration and
   monitoring. Keep the Cognito prefix through a further soak window; remove it
   only in a later reviewed change after proving no discovery, login, token,
   logout, management, MCP, or rollback dependency remains.

Any failure before step 7 blocks client release. Any failure after release
restores execute-api first if it was retired; custom-domain rollback is broader
because it returns effective outputs to both legacy endpoints. The regional
Cognito issuer is never replaced by the custom OAuth domain.

The demo remains explicitly `NONE` with mutations owned by the shared
`anonymous-internal` caller until a separate decision retires it. Production
`$default` remains `AWS_IAM` for internal AWS workloads; Cognito is additive,
not a replacement.

## Rollback and kill switches

Ordered fastest to broadest:

1. Add a compromised M2M `client_id` to the Lambda emergency denylist and deploy
   configuration; delete/disable its secret to stop new tokens.
2. Disable the affected human user/session through Cognito and, if immediate API
   denial is required, temporarily disable the OAuth route because offline JWT
   validation is not an immediate per-user revocation mechanism.
3. Set `cognito_auth_enabled=false` for Lambda semantic auth and remove/disable
   the exact OAuth route in a reviewed emergency apply. Existing signed tokens
   then have no reachable OAuth route.
4. Continue serving internal callers on unchanged `AWS_IAM`; demo behavior is
   unaffected.
5. Preserve the user pool and logs during the incident/rollback window. Do not
   destroy them until token max lifetime, investigation, and retention gates pass.

Rollback never changes queue records. Jobs submitted before rollback remain
owned by their namespaced caller and can be inspected by the same restored client
or an audited IAM operator. Re-enabling the route requires smoke tests and alarm
clearance.

## Open decisions and deployment gates

These do not block this specification, but each blocks its named rollout step:

1. **Human redirect/logout URLs and client type** — required before POC/staging
   app-client creation.
2. **Human MFA and social IdPs** — required before human pilot; TOTP is preferred
   when MFA is selected, and federation changes the cost model if enterprise
   SAML/OIDC is used.
3. **Per-organization scope and TTL profile** — required before each onboarding;
   24 hours needs explicit risk acceptance.
4. **Secret-delivery system and rotation role** — required before any customer
   M2M credential is created.
5. **Expected organization count** — if 49 will be exceeded during v1 lifetime,
   choose route sharding, multiple APIs, or Lambda authorizer before production
   instead of shipping the single-route cap.
6. **Emergency response SLO** — decide whether config-deploy denylisting is fast
   enough. If not, a low-latency external denylist or shorter TTL is a separate
   cost/security design.
7. **Legacy NONE retirement** — explicitly outside this rollout; public demo
   exposure remains visible in risk and cost dashboards.

## Acceptance-criteria traceability

| Acceptance area | Design sections |
|---|---|
| Problem, goals, non-goals, assumptions, decision | Opening through decision record |
| Three flows, trust boundaries, sequences | Architecture and protocol flows |
| Cognito resources, claims, key rotation | Cognito configuration; claim contract; threats |
| Exact external-tool matrix | Tool-to-scope matrix |
| Stable identity and malformed claims | Claim validation contract |
| Queue, dedupe, rate, ownership | Queue integration |
| Shared POST and defense in depth | Route contract; trust boundaries |
| Terraform, variables, outputs, state, POC | Terraform design; isolated POC |
| Compatibility, migration, rollback, kill switches | Route table; phases; rollback |
| Human/M2M cost and controls | Cost model and controls |
| Threats and abuse | Security and threat analysis |
| Observability and budgets | Observability; cost governance |
| POC evidence and teardown | Backend-only isolated POC |
| File/symbol impact and tests | Rust impact map; test strategy |
| Rust library selection and dependency isolation | Decision record; Rust impact map; POC |
| Deployment and open decisions | Runbook; open decisions |

## Authoritative sources

- [Amazon Cognito pricing][aws-pricing] — Lite MAUs, free tiers, M2M successful
  token-response billing, and app-client guidance.
- [Cognito scopes, M2M, and resource servers][aws-resource-server] — custom
  scopes, client-credentials requirements, and cost controls.
- [Cognito application-specific app-client settings][aws-app-clients] — public
  versus confidential clients, grant separation, and secret rotation.
- [Cognito PKCE][aws-pkce], [authorize endpoint][aws-authz], and
  [token endpoint][aws-token] — PKCE support and verification, code exchange,
  and client-credentials contracts.
- [Cognito access-token claims][aws-access-token] and
  [token revocation][aws-revocation] — `sub`, `client_id`, `token_use`, `scope`,
  TTL, signature, and offline revocation limitation.
- [API Gateway HTTP API JWT authorizers][aws-http-jwt] and
  [HTTP API quotas][aws-http-quotas] — validation behavior, key cache, scope
  behavior, and fixed audience limit.
- [RFC 6749][rfc6749], [RFC 7636][rfc7636], [RFC 9700][rfc9700], and
  [OpenID Connect Core 1.0][oidc-core] — OAuth grants, PKCE, current security BCP,
  state, and nonce.
- [`oauth2` 5.x][rust-oauth2] and its [security guidance][rust-oauth2-security]
  — typed client-credentials/PKCE requests, secret handling, state, and the
  requirement to disable HTTP redirects.
- [`openidconnect` 4.x][rust-openidconnect] — provider metadata, human OIDC flow,
  ID-token signature/issuer/audience/nonce validation, and supplied access-token
  hashes.
- [`cognito-jwt-verify` 0.2.0 source][rust-cognito-jwt-source] — evaluated as a
  Cognito-specific Rust reference and rejected for the production Lambda claim
  and audience contract.

[aws-access-token]: https://docs.aws.amazon.com/cognito/latest/developerguide/amazon-cognito-user-pools-using-the-access-token.html
[aws-add-secret]: https://docs.aws.amazon.com/cognito-user-identity-pools/latest/APIReference/API_AddUserPoolClientSecret.html
[aws-app-clients]: https://docs.aws.amazon.com/cognito/latest/developerguide/user-pool-settings-client-apps.html
[aws-authz]: https://docs.aws.amazon.com/cognito/latest/developerguide/authorization-endpoint.html
[aws-client-secret]: https://docs.aws.amazon.com/cognito/latest/developerguide/user-pool-settings-client-apps.html#cognito-user-pools-app-client-types
[aws-cognito-quotas]: https://docs.aws.amazon.com/cognito/latest/developerguide/quotas.html
[aws-http-jwt]: https://docs.aws.amazon.com/apigateway/latest/developerguide/http-api-jwt-authorizer.html
[aws-http-quotas]: https://docs.aws.amazon.com/apigateway/latest/developerguide/http-api-quotas.html
[aws-http-routes]: https://docs.aws.amazon.com/apigateway/latest/developerguide/http-api-develop-routes.html
[aws-iam-invoke]: https://docs.aws.amazon.com/apigateway/latest/developerguide/permissions.html
[aws-pricing]: https://aws.amazon.com/cognito/pricing/
[aws-pkce]: https://docs.aws.amazon.com/cognito/latest/developerguide/using-pkce-in-authorization-code.html
[aws-resource-server]: https://docs.aws.amazon.com/cognito/latest/developerguide/cognito-user-pools-define-resource-servers.html
[aws-revocation]: https://docs.aws.amazon.com/cognito/latest/developerguide/token-revocation.html
[aws-token]: https://docs.aws.amazon.com/cognito/latest/developerguide/token-endpoint.html
[oidc-core]: https://openid.net/specs/openid-connect-core-1_0-18.html
[rfc6749]: https://www.rfc-editor.org/rfc/rfc6749.html
[rfc7636]: https://www.rfc-editor.org/rfc/rfc7636.html
[rfc9700]: https://www.rfc-editor.org/rfc/rfc9700.html
[rust-cognito-jwt-source]: https://docs.rs/crate/cognito-jwt-verify/0.2.0/source/
[rust-oauth2]: https://docs.rs/oauth2/5.0.0/oauth2/
[rust-oauth2-security]: https://docs.rs/oauth2/5.0.0/oauth2/#security-warning
[rust-openidconnect]: https://docs.rs/openidconnect/4.0.1/openidconnect/
