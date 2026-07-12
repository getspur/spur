# Context Service Google Cognito Auth Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Google as an optional Cognito human identity provider and verify the complete Google-to-Cognito-to-SPUR login path.

**Architecture:** Terraform creates the Google social IdP from sensitive apply-time credentials and conditionally adds it to the existing public human app client. Cognito remains the sole token issuer, so API Gateway, Lambda authorization, CLI PKCE, personal API keys, and M2M behavior do not change.

**Tech Stack:** Terraform AWS provider, Amazon Cognito user pools, Google OAuth 2.0 web client, Terraform test framework, AWS CLI, SPUR CLI.

---

### Task 1: Lock Down the Terraform Contract

**Files:**
- Modify: `infra/spur-context-service/tests/cognito_static.tftest.hcl`

- [ ] **Step 1: Add a disabled-state assertion**

Add a plan test that leaves `google_oauth_enabled=false` and asserts:

```hcl
length(aws_cognito_identity_provider.google) == 0
aws_cognito_user_pool_client.human[0].supported_identity_providers == toset(["COGNITO"])
```

- [ ] **Step 2: Add an enabled-state assertion**

Use dummy non-secret test credentials and assert one Google provider, exact
`openid email profile` scopes, Cognito's returned endpoint defaults, exact
standard-attribute mappings including `username = sub`, and human providers
`COGNITO` plus `Google`.

- [ ] **Step 3: Add invalid-credential tests**

Assert enabled configuration rejects a blank secret and a client ID that does
not end in `.apps.googleusercontent.com`.

- [ ] **Step 4: Run RED verification**

Run:

```bash
terraform -chdir=infra/spur-context-service test -filter=tests/cognito_static.tftest.hcl
```

Expected: failure because the Google variables/resource do not exist.

- [ ] **Step 5: Commit the failing contract tests**

```bash
git add infra/spur-context-service/tests/cognito_static.tftest.hcl
git commit -m "test(context-infra): G1 cover Google Cognito provider"
```

### Task 2: Implement the Optional Google Provider

**Files:**
- Modify: `infra/spur-context-service/variables.tf`
- Modify: `infra/spur-context-service/main.tf`
- Modify: `infra/spur-context-service/env/default.tfvars`

- [ ] **Step 1: Add validated sensitive variables**

Define `google_oauth_enabled`, `google_oauth_client_id`, and
`google_oauth_client_secret`. Keep the feature false by default. Require
Cognito auth, a Google client-ID suffix, and a nonblank secret when enabled.

- [ ] **Step 2: Create the Google Cognito IdP**

Add `aws_cognito_identity_provider.google` with provider type/name `Google`,
`authorize_scopes = "openid email profile"`, and mappings for `email`,
`email_verified`, `name`, `picture`, and `username`. Codify the Google endpoint
defaults returned by Cognito so plans remain stable while client-secret changes
remain managed.

- [ ] **Step 3: Update only the human app client**

Conditionally append the provider resource name to
`supported_identity_providers`. Keep M2M resources unchanged and establish an
explicit dependency on the resource server and Google provider.

- [ ] **Step 4: Enable Google in the default environment without secrets**

Set only `google_oauth_enabled = true` in `env/default.tfvars`. Supply the ID
and secret exclusively through protected `TF_VAR_*` environment variables at
plan/apply time.

- [ ] **Step 5: Run GREEN verification**

```bash
terraform -chdir=infra/spur-context-service fmt -check
terraform -chdir=infra/spur-context-service validate
terraform -chdir=infra/spur-context-service test -filter=tests/cognito_static.tftest.hcl
```

Expected: all commands exit 0.

- [ ] **Step 6: Commit the implementation**

```bash
git add infra/spur-context-service/variables.tf \
  infra/spur-context-service/main.tf \
  infra/spur-context-service/env/default.tfvars
git commit -m "feat(context-infra): G1 add Google Cognito provider"
```

### Task 3: Document Operations and Deploy

**Files:**
- Modify: `infra/spur-context-service/README.md`

- [ ] **Step 1: Add the Google setup and rotation runbook**

Document the exact Google origin/redirect URI, protected credential-file path,
`TF_VAR_*` extraction, saved-plan review, login command, verification, rollback,
and secret rotation sequence. Never include real credential values.

- [ ] **Step 2: Commit the runbook**

```bash
git add infra/spur-context-service/README.md
git commit -m "docs(context-infra): G1 document Google login operations"
```

- [ ] **Step 3: Load credentials without displaying them**

Read the `web.client_id` and `web.client_secret` fields from the owner-only JSON
into `TF_VAR_google_oauth_client_id` and
`TF_VAR_google_oauth_client_secret`. Validate presence and permissions without
printing either value.

- [ ] **Step 4: Create and review a saved production plan**

Use the existing artifact inputs plus explicit
`custom_domains_enabled=true` and `disable_execute_api_endpoint=false`.
Expected mutations: one Cognito Google IdP and one in-place human app-client
update only.

- [ ] **Step 5: Apply the reviewed plan**

Apply the saved plan and wait for Cognito propagation.

### Task 4: Run Live End-to-End Verification

**Files:** None.

- [ ] **Step 1: Verify AWS resource state**

Use `aws cognito-idp list-identity-providers` and
`describe-user-pool-client`. Expect `Google` and human providers
`["COGNITO", "Google"]`.

- [ ] **Step 2: Verify direct Google redirect**

Call the custom-domain authorize endpoint with code flow, PKCE, and
`identity_provider=Google`. Expect a `302` location on `accounts.google.com`.

- [ ] **Step 3: Complete interactive CLI login**

```bash
spur context auth login --profile google --url https://context.getspur.dev
```

Complete Google sign-in in the browser and require a successful local callback.

- [ ] **Step 4: Verify personal API-key flow**

Create a bounded test key with read/index/status scopes, call one MCP read tool,
revoke the key, verify the old key receives `403`, log out, and remove the test
federated user if it is disposable.

- [ ] **Step 5: Verify zero drift and cleanup**

Run the exact Terraform plan again and require `No changes`. Remove saved local
plans and temporary secret-bearing environment variables. Preserve the original
owner-only Google credential JSON for controlled rotation unless the operator
chooses to move it into an approved secret store.
