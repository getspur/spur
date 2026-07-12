# Context Service Google Sign-In Through Cognito

## Goal

Enable public human users to authenticate with Google through the existing
Cognito user pool and custom OAuth domain without changing the context-service
token-validation or API-key authorization contracts.

## Current State

- Cognito issuer: the existing regional user-pool issuer.
- OAuth authority: `https://auth.context.getspur.dev`.
- Human app client: authorization code with PKCE, no client secret.
- Human callback: `http://127.0.0.1:8765/callback` for `spur context auth login`.
- The human app client currently supports only the native `COGNITO` provider.
- Google web client project: `getspur-context-auth`.
- Google redirect URI: `https://auth.context.getspur.dev/oauth2/idpresponse`.
- Google JavaScript origin: `https://auth.context.getspur.dev`.

## Design

Terraform owns an optional `aws_cognito_identity_provider` named `Google`.
The provider is enabled only when both Cognito auth and the new Google feature
flag are enabled. It requests only `openid email profile` and maps the standard
Google claims `email`, `email_verified`, `name`, and `picture` into Cognito
standard attributes. Cognito also returns the required `username = sub`
mapping. Terraform codifies that mapping and Cognito's returned Google endpoint
defaults to prevent perpetual drift without ignoring client-secret rotation.

The existing human app client continues supporting native Cognito users and
adds `Google` when enabled. Its authorization-code, PKCE, callback, token
lifetime, resource-server scopes, issuer, and API Gateway audience remain
unchanged. M2M clients remain Cognito-only.

## Secret Handling

The Google client ID and secret are sensitive Terraform variables. They are
read from the owner-only downloaded Google JSON by the deployment shell and
passed through `TF_VAR_google_oauth_client_id` and
`TF_VAR_google_oauth_client_secret`. They must never be committed, written to
tfvars, included in outputs, or printed in command logs.

The Cognito provider API requires the secret value, so Terraform state will
contain it. The existing encrypted remote state remains the system of record;
access to that state must stay least-privilege. Secret rotation creates a new
Google credential, applies it to Cognito, verifies login, and only then removes
the previous credential.

## Terraform Contract

New variables:

- `google_oauth_enabled`: boolean, false by default.
- `google_oauth_client_id`: sensitive string, nonblank and ending in
  `.apps.googleusercontent.com` when enabled.
- `google_oauth_client_secret`: sensitive nonblank string when enabled.

When disabled, Terraform creates no Google IdP and leaves
`supported_identity_providers = ["COGNITO"]`. When enabled, it creates exactly
one Google IdP and configures the human app client with both `COGNITO` and
`Google`.

## Login Flow

1. `spur context auth login --profile google --url https://context.getspur.dev`
   starts authorization code plus PKCE.
2. Cognito managed login offers Google alongside native Cognito sign-in.
3. Google returns its authorization code to
   `https://auth.context.getspur.dev/oauth2/idpresponse`.
4. Cognito creates or updates the federated user and returns a Cognito code to
   `http://127.0.0.1:8765/callback`.
5. The CLI exchanges that code with Cognito and stores the management session.
6. API-key creation and MCP authentication continue using the existing Cognito
   access-token and personal-key contracts.

## Failure Behavior

- Missing or malformed Google credentials fail Terraform validation before any
  AWS mutation.
- Google remains disabled by default in every environment.
- Removing the feature flag removes Google from the human app client before
  removing the provider, while native Cognito login remains available.
- A Google outage does not affect existing API keys, M2M clients, or native
  Cognito login.

## Verification

Static tests prove disabled and enabled resource/provider contracts and reject
invalid credentials. Deployment uses a saved Terraform plan with custom
domains explicitly enabled and execute-api compatibility preserved.

Live verification requires:

- Cognito `list-identity-providers` returns one `Google` provider.
- The human app client reports `COGNITO` and `Google`.
- An authorize request with `identity_provider=Google` redirects to
  `accounts.google.com` without exposing the client secret.
- `spur context auth login` completes through the local PKCE callback.
- A Google-authenticated user can create, use, and revoke a personal API key.
- A final Terraform plan reports no changes.
