# Context-service auth-client POC

This is a standalone Rust proof of concept for the context service's Cognito
clients. It is intentionally outside the production Lambda crate: `oauth2`,
`openidconnect`, and outbound HTTP dependencies must not enter the Lambda
dependency graph.

The package has no AWS SDK and its tests use local mock HTTP servers only. Do
not deploy it, create Cognito resources, or point it at production while using
this POC.

## Verify

Run every Rust command through the repository wrapper. The POC uses the
wrapper's `--dir` alias (equivalent to `--workdir`):

```sh
scripts/spur-cargo --dir infra/spur-context-service/poc/auth-client fmt --check
scripts/spur-cargo --dir infra/spur-context-service/poc/auth-client test
scripts/spur-cargo --dir infra/spur-context-service/poc/auth-client check
scripts/spur-cargo --dir infra/spur-context-service/poc/auth-client run -- --help
```

No command above makes an AWS or Cognito call. The `run` command is help-only
unless an operator explicitly invokes `m2m-token` with a configured token
endpoint.

## Safe configuration contract

`m2m-token` accepts no credential-bearing arguments. It reads only these
environment variable names:

- `SPUR_AUTH_CLIENT_ID`
- `SPUR_AUTH_CLIENT_SECRET`
- `SPUR_AUTH_TOKEN_ENDPOINT`
- `SPUR_AUTH_SCOPES`

Obtain values through the approved secret channel or process environment. Do
not put values in shell history, arguments, `.env` files, source, fixtures,
logs, screenshots, or issue comments. The command confirms acquisition without
printing the bearer token.

The public human PKCE/OIDC library configuration uses:

- `SPUR_AUTH_ISSUER`
- `SPUR_AUTH_AUTHORIZATION_ENDPOINT`
- `SPUR_AUTH_TOKEN_ENDPOINT`
- `SPUR_AUTH_HUMAN_CLIENT_ID`
- `SPUR_AUTH_REDIRECT_URI`

Production configuration requires fixed HTTPS issuer, authorization, and token
endpoints. HTTP is accepted only for loopback mock servers created by tests.

## Security behavior

- M2M requests use OAuth 2.0 `client_credentials`, Basic authentication, and
  an exact normalized scope set.
- Tokens are cached in memory by `(client_id, normalized_scope_set)`, use
  single-flight acquisition, refresh around 80% of lifetime with bounded
  jitter, and never survive their expiry.
- The reusable Rustls `reqwest` client disables redirects, disables proxies, and
  bounds connect and request timeouts.
- Each human authorization attempt creates fresh S256 PKCE, state, and nonce.
  The exchange validates state once and OIDC validates the returned ID token's
  signature, issuer, audience, nonce, and supplied access-token hash.
- Errors are local bounded reason codes. Secret, authorization-code, verifier,
  bearer-token, and raw response values are never formatted into errors.
