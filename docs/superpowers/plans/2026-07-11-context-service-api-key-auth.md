# Context Service CLI-Managed API Key Authentication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add feature-flagged, CLI-managed personal API keys for routine context-service MCP calls while preserving Cognito OAuth/M2M, IAM, demo, queue ownership, and EventBridge behavior.

**Architecture:** Cognito human OAuth protects exact key-management routes. A dedicated HTTP API request Lambda authorizer validates high-entropy personal keys from a dedicated DynamoDB table and passes a typed owner/scope context to the serving Lambda. `spur context mcp` uses a locally selected API key and never performs OAuth refresh during normal MCP operation.

**Tech Stack:** Rust 2021, Tokio, Reqwest/Rustls, `oauth2`, `openidconnect`, OS credential stores, AWS Lambda, API Gateway HTTP API request authorizers, DynamoDB transactions, EventBridge, Terraform mock tests, `scripts/spur-cargo --dir`.

---

## Task graph and ownership

| Task | Scope | Depends on | Commit intent |
|---|---|---|---|
| A | API-key domain and DynamoDB store primitives | none | `feat(spur-context-service): A add API key store` |
| B | Authorizer, discovery and management handlers | A | `feat(spur-context-service): B add API key auth routes` |
| C | Feature-flagged Terraform resources | B | `feat(context-infra): C provision API key auth` |
| D | Production OAuth/credential client crate | B | `feat(spur-context-auth): D promote OAuth client` |
| E | SPUR CLI and MCP proxy integration | B, D | `feat(spur-cli): E manage context API keys` |
| F | POC, runbooks and final verification | C, E | `test(spur-context-service): F harden API key POC` |

Tasks A, B and D run in sequence because each may update Rust dependency
metadata. Task C may run beside D after B because it owns only production
Terraform. Task E owns CLI/config/proxy files. Task F is the only task allowed
to update cross-component runbooks and the consolidated offline smoke runner.

## Task A: API-key domain and DynamoDB store primitives

**Files:**
- Create: `crates/spur-context-service/src/api_keys.rs`
- Create: `crates/spur-context-service/tests/api_keys_test.rs`
- Modify: `crates/spur-context-service/src/lib.rs`
- Modify: `crates/spur-context-service/Cargo.toml`
- Modify: `Cargo.lock`

- [ ] **Step 1: Write failing grammar, generation and persistence tests**

Add tests for canonical `spur_live_<26 base32>_<52 base32>` keys, rejection of
unknown environment/length/alphabet/segments, 128-bit public IDs, 256-bit
secrets, one-time reveal, and persisted records that contain only a 32-byte
digest.

The public interface established by the tests is:

```rust
pub struct GeneratedApiKey {
    pub public_id: String,
    pub plaintext: secrecy::SecretString,
    pub record: ApiKeyRecord,
}

pub fn generate_api_key(
    environment: KeyEnvironment,
    owner_id: &str,
    name: &str,
    scopes: ApiKeyScopes,
    now_epoch_seconds: u64,
    expires_at: u64,
) -> Result<GeneratedApiKey, ApiKeyError>;

pub fn parse_api_key(value: &str) -> Result<ParsedApiKey<'_>, ApiKeyError>;
pub fn verify_secret(parsed: &ParsedApiKey<'_>, stored_digest: &[u8]) -> bool;
```

- [ ] **Step 2: Run the focused tests and verify RED**

```bash
scripts/spur-cargo --dir crates/spur-context-service test --test api_keys_test \
  key_grammar_and_generation
```

Expected: compilation/test failure because `api_keys` does not exist.

- [ ] **Step 3: Implement pure key and scope types**

Implement fixed-length lowercase RFC 4648 Base32 without padding, OS CSPRNG
generation, SHA-256 over decoded secret bytes, constant-time digest comparison,
bounded names, explicit `live|test` environments, and normalized scopes limited
to `external.read|external.index|external.status`. Reject `keys.manage`.

Do not use password hashing or persist plaintext. Add only narrowly justified
dependencies (`rand`, `subtle`, and `secrecy` for redacted secret ownership).

- [ ] **Step 4: Write failing store transaction tests**

Define a store trait and fake store tests for:

```rust
#[async_trait]
pub trait ApiKeyStore: Send + Sync {
    async fn create_key(&self, request: CreateKeyRecord) -> Result<(), ApiKeyStoreError>;
    async fn get_key_consistent(&self, public_id: &str) -> Result<Option<ApiKeyRecord>, ApiKeyStoreError>;
    async fn list_owner_keys(&self, owner_id: &str, cursor: Option<&str>, limit: usize) -> Result<ApiKeyPage, ApiKeyStoreError>;
    async fn revoke_key(&self, owner_id: &str, public_id: &str, now: u64) -> Result<RevokeResult, ApiKeyStoreError>;
    async fn sweep_expired(&self, request: SweepRequest) -> Result<SweepPage, ApiKeyStoreError>;
}
```

Cover 11 concurrent creates yielding at most 10 active keys, duplicate public
IDs, conditional/idempotent revoke, cross-owner `not_found`, expired-key denial,
expiry-bucket cleanup, cursor lease/catch-up, and no double decrement.

- [ ] **Step 5: Implement fake and DynamoDB stores**

Use primary items `KEY#<id>`, owner counter `OWNER#<owner>`, cleanup cursor
`SYSTEM#expiry-sweeper`, owner GSI, and sparse expiry-hour GSI exactly as the
design specifies. Authentication uses strongly consistent `GetItem`.

Create/revoke use `TransactWriteItems` conditions. TTL performs delayed GC only.
Map DynamoDB cancellation reasons to bounded typed errors; never format key
material or raw request expressions.

- [ ] **Step 6: Run focused and regression tests**

```bash
scripts/spur-cargo --dir crates/spur-context-service test --test api_keys_test
scripts/spur-cargo --dir crates/spur-context-service test --test jobs_test
scripts/spur-cargo --dir crates/spur-context-service fmt -- --check
```

Expected: all pass.

- [ ] **Step 7: Commit Task A once**

```bash
git add crates/spur-context-service/Cargo.toml \
  crates/spur-context-service/src/api_keys.rs \
  crates/spur-context-service/src/lib.rs \
  crates/spur-context-service/tests/api_keys_test.rs \
  Cargo.lock
git commit -m "feat(spur-context-service): A add API key store"
```

## Task B: Authorizer, discovery and management handlers

**Files:**
- Create: `crates/spur-context-service/src/api_key_authorizer.rs`
- Create: `crates/spur-context-service/src/bin/api_key_authorizer.rs`
- Create: `crates/spur-context-service/tests/fixtures/api-key-auth-contract.json`
- Modify: `crates/spur-context-service/src/auth.rs`
- Modify: `crates/spur-context-service/src/lambda.rs`
- Modify: `crates/spur-context-service/src/lib.rs`
- Modify: `crates/spur-context-service/Cargo.toml`
- Modify: `crates/spur-context-service/tests/mcp_test.rs`
- Modify: `Cargo.lock`

- [ ] **Step 1: Add failing reserved-route and discovery tests**

Test exact classification for:

```text
GET    /.well-known/spur-context-service
POST   /mcp/api-key
POST   /auth/api-keys
GET    /auth/api-keys
DELETE /auth/api-keys/{key_id}
```

Prove that disabled/missing configuration rejects every reserved path before
legacy parsing, including `$default=NONE`. Discovery exposes only schema
version, issuer, public human client ID, endpoints, supported scopes, feature
status, and exact URLs.

- [ ] **Step 2: Run the Lambda fixture tests and verify RED**

```bash
scripts/spur-cargo --dir crates/spur-context-service test --features lambda \
  --lib api_key_fixture
```

Expected: missing route/context types or assertions fail.

- [ ] **Step 3: Implement fail-closed route and management auth types**

Extend route classification without changing OAuth/IAM/EventBridge precedence.
Add typed authorizer context version 1:

```rust
pub struct ApiKeyAuthContext {
    pub auth_context_version: u8,
    pub auth_kind: ApiKeyAuthKind,
    pub owner_id: String,
    pub key_id: String,
    pub scopes: ApiKeyScopes,
}
```

Management requires `keys.manage`, the configured human client ID, and human
principal kind. API-key/M2M/IAM/anonymous contexts cannot manage keys. Feature
enabled requires Cognito enabled. Wrong contexts do not downgrade.

- [ ] **Step 4: Add failing authorizer tests**

Cover missing/malformed keys, unknown ID, wrong digest, revoked, expired, store
failure, valid scope context, strongly consistent reads, simple-response shape,
route-key identity source assumptions, and identical bounded 401 bodies for all
credential failures. Ensure debug/error output contains no key or digest.

- [ ] **Step 5: Implement the lean authorizer**

The authorizer binary must not link DuckDB/catalog modules. It parses the fixed
key, performs one consistent lookup, verifies status/expiry/digest, and returns
only typed context. Store/config failures fail closed. Keep all log dimensions
bounded.

- [ ] **Step 6: Add failing management lifecycle tests**

Use a fake store and deterministic clock/RNG seam to test create/list/revoke,
one-time reveal, 90-day default/365-day maximum, ten-key cap, owner isolation,
scope subset, human-only management, and idempotent revoke.

- [ ] **Step 7: Implement discovery and management handlers**

Handle exact routes before generic `ToolRequest`. The creation response contains
the plaintext once; list/revoke never do. API-key MCP calls use trusted
`owner_id` for existing tool, queue, dedupe, rate and status paths and recheck
the exact body-selected scope.

- [ ] **Step 8: Run backend verification**

```bash
scripts/spur-cargo --dir crates/spur-context-service test --features lambda --lib
scripts/spur-cargo --dir crates/spur-context-service test --features lambda --test mcp_test
scripts/spur-cargo --dir crates/spur-context-service test --test api_keys_test
SPUR_REMOTE=1 scripts/spur-cargo --dir crates/spur-context-service clippy \
  --features lambda --all-targets -- -D warnings
```

Expected: feature tests pass; unrelated pre-existing lint failures must be
reported rather than hidden or edited outside scope.

- [ ] **Step 9: Commit Task B once**

```bash
git add Cargo.lock crates/spur-context-service
git commit -m "feat(spur-context-service): B add API key auth routes"
```

## Task C: Feature-flagged Terraform resources

**Files:**
- Create: `infra/spur-context-service/api_keys.tf`
- Create: `infra/spur-context-service/tests/api_key_static.tftest.hcl`
- Modify: `infra/spur-context-service/main.tf`
- Modify: `infra/spur-context-service/iam.tf`
- Modify: `infra/spur-context-service/variables.tf`
- Modify: `infra/spur-context-service/outputs.tf`
- Modify: `infra/spur-context-service/env/default.tfvars`
- Modify: `infra/spur-context-service/terraform.tfvars.example`

- [ ] **Step 1: Add failing disabled/enabled Terraform tests**

Disabled default must create no API-key table, GSI, authorizer, route,
management route, cleanup schedule, permission, log group, alarm, or output
value. Enabling API keys with Cognito disabled must fail a precondition.

Enabled mock plans must assert exact routes/auth types/scopes, 30-second cache,
header plus route-key identity sources, table/GSI/PITR/TTL contract, header-free
access logs, scoped permissions, cleanup cursor access, and unchanged
`$default`, OAuth and EventBridge resources.

- [ ] **Step 2: Run Terraform tests and verify RED**

```bash
terraform -chdir=infra/spur-context-service init -backend=false -input=false
terraform -chdir=infra/spur-context-service test \
  -test-directory=tests -filter=tests/api_key_static.tftest.hcl
```

Expected: references to absent variables/resources fail.

- [ ] **Step 3: Implement variables and resources**

Add the exact validated defaults from the design. Create the separate table,
owner/expiry GSIs, authorizer function/alias/role/logs, CUSTOM route, public
discovery route, JWT management routes, cleanup schedule, permissions, alarms,
and non-secret outputs. Attempt API-key-header removal in a route-specific
integration; do not make serving trust depend on it.

Use independent artifact input for the lean authorizer binary. Do not reuse a
zip that links the serving DuckDB binary.

- [ ] **Step 4: Prove IAM separation and compatibility**

Authorizer: `GetItem` on key table only. Management: required key/counter
transactions and owner query. Cleanup: expiry/owner query plus idempotent revoke
transactions. Preserve all existing IAM/Cognito/demo/drainer policies.

- [ ] **Step 5: Run complete non-applying Terraform checks**

```bash
terraform -chdir=infra/spur-context-service fmt -check -recursive
terraform -chdir=infra/spur-context-service validate
terraform -chdir=infra/spur-context-service test -test-directory=tests
```

Expected: all mock/plan tests pass and no AWS credentials are needed.

- [ ] **Step 6: Commit Task C once**

```bash
git add infra/spur-context-service
git commit -m "feat(context-infra): C provision API key auth"
```

## Task D: Production OAuth and credential client crate

**Files:**
- Create: `crates/spur-context-auth/Cargo.toml`
- Create: `crates/spur-context-auth/src/lib.rs`
- Create: `crates/spur-context-auth/src/oauth.rs`
- Create: `crates/spur-context-auth/src/management.rs`
- Create: `crates/spur-context-auth/src/credentials.rs`
- Create: `crates/spur-context-auth/tests/oauth.rs`
- Create: `crates/spur-context-auth/tests/management.rs`
- Create: `crates/spur-context-auth/tests/credentials.rs`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`

- [ ] **Step 1: Add the crate and failing promoted POC tests**

Move behavior, not files, from the isolated auth-client POC. Tests cover fresh
S256 PKCE/state/nonce, exact callback, OIDC issuer/audience/signature/nonce/hash,
redirect rejection, bounded timeouts, no proxy inheritance, management refresh,
redacted errors, and no secrets in debug output.

- [ ] **Step 2: Add failing credential-store contract tests**

Define:

```rust
#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn load(&self, profile: &CredentialProfile) -> Result<Option<StoredCredential>, CredentialError>;
    async fn store(&self, profile: &CredentialProfile, value: &StoredCredential) -> Result<(), CredentialError>;
    async fn delete(&self, profile: &CredentialProfile) -> Result<(), CredentialError>;
}
```

Use an in-memory fake. Test environment/keyring/restricted-file precedence,
Unix `0600` enforcement, no normal-config secrets, stdin import grammar, and
management-vs-API-key separation.

- [ ] **Step 3: Implement the production crate**

Use mature `oauth2`/`openidconnect`/Rustls clients, explicit endpoint/origin
validation, OS keyring adapter, restricted-file fallback, typed discovery,
management requests, and `secrecy`-backed secret-redacting types. Do not depend
on infra paths or the context-service Lambda crate.

- [ ] **Step 4: Run crate verification**

```bash
scripts/spur-cargo test -p spur-context-auth
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-context-auth --all-targets -- -D warnings
scripts/spur-cargo fmt --all -- --check
```

Expected: all pass.

- [ ] **Step 5: Commit Task D once**

```bash
git add Cargo.toml Cargo.lock crates/spur-context-auth
git commit -m "feat(spur-context-auth): D promote OAuth client"
```

## Task E: SPUR CLI and MCP proxy integration

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs`
- Modify: `crates/spur-core/src/mcp/context_service.rs`
- Modify: `crates/spur-core/Cargo.toml`
- Modify: `crates/spur-cli/src/main.rs`
- Modify: `crates/spur-cli/src/commands/mcp.rs`
- Modify: `crates/spur-cli/Cargo.toml`
- Modify: `Cargo.lock`
- Test: `crates/spur-core/src/mcp/context_service.rs`
- Test: `crates/spur-cli/tests/context_auth_cli.rs`

- [ ] **Step 1: Add failing explicit-auth-mode proxy tests**

Replace optional bearer-only behavior with:

```rust
pub enum ContextServiceAuth {
    None,
    OAuthBearer(SecretString),
    ApiKey(SecretString),
}
```

Tests prove route/header pairs, mutual exclusion, request body preservation,
redaction, and that API-key calls never invoke OAuth refresh.

- [ ] **Step 2: Implement `ContextServiceClient` auth modes**

API key selects `/mcp/api-key` and `X-SPUR-API-Key`. OAuth selects `/mcp/oauth`
and Bearer. Legacy keeps configured URL. Preserve timeouts/tool definitions and
bounded remote error handling.

- [ ] **Step 3: Add failing config and CLI command tests**

Add commands:

```text
spur context auth login|logout
spur context key create|list|use|revoke|add --stdin
spur context mcp [--profile NAME]
```

Test config contains URL/auth mode/profile/public ID hint only; environment,
keyring, file precedence; no API key argument; TTY-gated `--show-secret`; stdin
import; OAuth-only management; local-only `key use`; and logout preserving API
keys.

- [ ] **Step 4: Implement CLI orchestration**

Use `spur-context-auth` for discovery/login/management/credential stores. Keep
existing `--token` temporarily with deprecation text. Add no secret-bearing
clap debug or error paths. Browser/loopback failures return actionable bounded
messages.

- [ ] **Step 5: Run CLI/proxy regression tests**

```bash
scripts/spur-cargo test -p spur-core context_service
scripts/spur-cargo test -p spur-cli --test context_auth_cli
scripts/spur-cargo test -p spur-cli context_service_cli_config
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-cli -p spur-core --all-targets -- -D warnings
```

Expected: all scoped tests pass; existing context MCP tests remain green.

- [ ] **Step 6: Commit Task E once**

```bash
git add Cargo.lock crates/spur-acp crates/spur-core crates/spur-cli
git commit -m "feat(spur-cli): E manage context API keys"
```

## Task F: POC hardening, runbooks and final verification

**Files:**
- Modify: `infra/spur-context-service/poc/**`
- Modify: `infra/spur-context-service/README.md`
- Modify: `crates/spur-context-service/docs/ARCHITECTURE.md`
- Modify: `docs/superpowers/specs/2026-07-11-context-service-api-key-auth-design.md` only for verified implementation notes, not design changes
- Test: `infra/spur-context-service/poc/tests/test_harness_static.py`

- [ ] **Step 1: Extend the isolated POC without applying AWS**

Add disabled/default and mock-enabled API-key fixtures, synthetic keys only,
authorizer/management event fixtures, header-removal evidence row, cache and
revocation evidence rows, and isolated teardown inventory categories. No real
IDs, credentials, state, apply, destroy, or production references.

- [ ] **Step 2: Add cross-component regressions**

Prove exact committed fixture bodies parse through their routes; OAuth and API
keys share owner; multiple keys share quota; reserved disabled paths fail
closed; EventBridge bypasses HTTP auth; M2M/IAM/demo remain unchanged; CLI sends
the correct route/header; and no secret-shaped values appear in output.

- [ ] **Step 3: Update operator documentation**

Document enablement, discovery, CLI login/create/use/revoke, headless stdin/env,
30-second revocation SLO, emergency route kill switch, expiry sweeper lag,
revoke-by-owner offboarding, metrics, cost evidence, rollback, and teardown.

- [ ] **Step 4: Run the complete offline gate**

```bash
infra/spur-context-service/poc/scripts/offline-smoke.sh
scripts/spur-cargo test -p spur-context-auth
scripts/spur-cargo test -p spur-core context_service
scripts/spur-cargo test -p spur-cli --test context_auth_cli
scripts/spur-cargo --dir crates/spur-context-service test --features lambda
terraform -chdir=infra/spur-context-service fmt -check -recursive
terraform -chdir=infra/spur-context-service validate
terraform -chdir=infra/spur-context-service test -test-directory=tests
scripts/spur-cargo fmt --all -- --check
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-context-auth -p spur-core -p spur-cli \
  --all-targets -- -D warnings
```

Expected: all relevant tests pass. If the build VM is unavailable and local
fallback is killed by bundled DuckDB resource pressure, report that evidence
separately; do not claim the uncompleted command passed.

- [ ] **Step 5: Run final secret/ID and diff checks**

```bash
python3 infra/spur-context-service/poc/scripts/scan-secrets.py \
  infra/spur-context-service/poc/fixtures/*.json
git diff --check
git status --short
```

Expected: no secret-shaped fixture/output and only intended task files changed.

- [ ] **Step 6: Commit Task F once**

```bash
git add crates/spur-context-service/docs \
  docs/superpowers/specs/2026-07-11-context-service-api-key-auth-design.md \
  infra/spur-context-service
git commit -m "test(spur-context-service): F harden API key POC"
```

## Final review gate

Before plan merge, reviewers must verify:

- no task deployed or applied AWS resources;
- no raw API key, OAuth token, hash, authorization header, code or verifier is
  present in tracked files, logs or task summaries;
- feature-disabled routing cannot reach `$default` behavior;
- API-key authorizer context cannot downgrade to another auth scheme;
- `keys.manage` is human-only and absent from M2M/API-key scopes;
- create/revoke counters are transactionally exact under concurrency;
- expiry cleanup has a scalable sparse index and resumable cursor;
- all personal keys and OAuth resolve to one `cognito:user:<sub>` owner;
- CLI normal MCP operation performs no OAuth refresh; and
- the worker produced one scoped commit per task with a clean worktree.
