# OAuth2 Refresh-Token Grant (Approach B) — Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan`. Each task becomes a beads issue.

**Source spec:** `docs/superpowers/specs/2026-06-03-oauth2-refresh-grant-design.ipynb`
**Design epic:** `bd-1mmd`

**Goal:** Add a real OAuth2 refresh-token grant to `rest-table-gateway`: the user supplies `client_id`/`client_secret`/`refresh_token` via env vars; the gateway POSTs `grant_type=refresh_token` to the provider `token_url`, caches the minted access token with expiry, auto-refreshes, and applies it as `Bearer` — `apply_auth`/`HttpFetch` unchanged.

**Locked decisions:** (a) env-var credentials, (b) in-memory process-global token cache, (c) 60s refresh skew, (d) cache key = `(token_url, refresh_token)`.

**Architecture:** New `adapter/oauth.rs` token service (cache + refresh POST). New `AuthCfg::Oauth2Refresh` manifest variant. `ManifestAdapter::resolve_auth` becomes `async` and calls the token service inside the already-async `scan`. The Nango translator emits the new scheme for `OAUTH2` providers that carry a `token_url`.

**Scope note — deferred:** Wizard `required_env_vars` surfacing for `oauth2_refresh` connections (connection layer in `crates/spur-notebook/src/mcp/mod.rs` + `connection_store.rs`) is **out of scope** here — it needs an `AuthCfg`→env-vars derivation that doesn't exist yet. Until then, users set the 3 env vars manually and queries mint tokens correctly. Tracked as a follow-up under `bd-1mmd`.

**Sequencing:** Rust enum-exhaustiveness forces the variant + its `resolve_auth`/`auth_to_toml` arms to land together, and the real `resolve_auth` arm needs the token service. So the DAG is a chain: **T1 → T2 → T3**.

---

### Task 1: `oauth.rs` token service + `GatewayError::Auth`

**Task ID:** `task-oauth-service`
**Depends on:** none
**Suggested Worker:** codex

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/error.rs`
- Create: `crates/spur-notebook/rest-table-gateway/src/adapter/oauth.rs`
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/mod.rs` (add `mod oauth;`)

**Scope Boundary:**
- IN: the three files above only.
- OUT: `manifest.rs`, `nango.rs`, `manifest_adapter.rs`, the snapshot. If you must touch them, emit `scope_drift`.

**Acceptance Criteria:**
- [ ] `GatewayError::Auth(String)` exists.
- [ ] `oauth::access_token` exchanges on cache miss, returns the cached token on a fresh hit (no second HTTP call), and surfaces `GatewayError::Auth` on non-2xx.
- [ ] `cargo test -p spur-rest-table-gateway oauth` passes.

**Implementation:**

- [ ] **Step 1: Add the error variant** to `error.rs` `enum GatewayError`:

```rust
    #[error("auth error: {0}")]
    Auth(String),
```

- [ ] **Step 2: Register the module** — add to `adapter/mod.rs` alongside the other `mod` declarations:

```rust
pub(crate) mod oauth;
```

- [ ] **Step 3: Write `adapter/oauth.rs`:**

```rust
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::error::{GatewayError, Result};

/// Refresh proactively this long before the token's stated expiry.
const REFRESH_SKEW: Duration = Duration::from_secs(60);
/// Fallback lifetime when the provider omits `expires_in`.
const DEFAULT_TTL: Duration = Duration::from_secs(3600);

#[derive(Clone)]
struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

fn cache() -> &'static Mutex<HashMap<String, CachedToken>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedToken>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct RefreshGrant<'a> {
    pub token_url: &'a str,
    pub client_id: &'a str,
    pub client_secret: &'a str,
    pub refresh_token: &'a str,
    pub scope: Option<&'a str>,
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

fn cache_key(grant: &RefreshGrant<'_>) -> String {
    format!("{}|{}", grant.token_url, grant.refresh_token)
}

/// Return a valid access token for `grant`, minting one via the refresh-token
/// grant when the cache is empty or the cached token is within REFRESH_SKEW of expiry.
pub async fn access_token(client: &reqwest::Client, grant: &RefreshGrant<'_>) -> Result<String> {
    let key = cache_key(grant);

    if let Some(tok) = cache().lock().unwrap().get(&key) {
        if tok
            .expires_at
            .checked_duration_since(Instant::now())
            .map_or(false, |left| left > REFRESH_SKEW)
        {
            return Ok(tok.access_token.clone());
        }
    }

    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", grant.refresh_token),
        ("client_id", grant.client_id),
        ("client_secret", grant.client_secret),
    ];
    if let Some(scope) = grant.scope {
        form.push(("scope", scope));
    }

    let resp = client
        .post(grant.token_url)
        .form(&form)
        .send()
        .await
        .map_err(|e| GatewayError::Auth(format!("token refresh request failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(GatewayError::Auth(format!(
            "token refresh returned status {}",
            resp.status()
        )));
    }
    let body: TokenResponse = resp
        .json()
        .await
        .map_err(|e| GatewayError::Auth(format!("token refresh response parse failed: {e}")))?;

    let ttl = body.expires_in.map(Duration::from_secs).unwrap_or(DEFAULT_TTL);
    cache().lock().unwrap().insert(
        key,
        CachedToken {
            access_token: body.access_token.clone(),
            expires_at: Instant::now() + ttl,
        },
    );
    Ok(body.access_token)
}

/// Drop a cached token (e.g. after a 401 from the resource API).
pub fn invalidate(grant: &RefreshGrant<'_>) {
    cache().lock().unwrap().remove(&cache_key(grant));
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn miss_exchanges_then_hit_is_cached() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok-abc",
                "expires_in": 3600
            })))
            .expect(1) // exactly one HTTP exchange across both calls
            .mount(&server)
            .await;

        let url = format!("{}/token", server.uri());
        let grant = RefreshGrant {
            token_url: &url,
            client_id: "cid",
            client_secret: "csec",
            refresh_token: "miss_exchanges_then_hit_rt", // unique key per test
            scope: None,
        };
        let client = reqwest::Client::new();

        let t1 = access_token(&client, &grant).await.expect("first mint");
        let t2 = access_token(&client, &grant).await.expect("cache hit");
        assert_eq!(t1, "tok-abc");
        assert_eq!(t2, "tok-abc");
        // server.expect(1) is asserted on drop
    }

    #[tokio::test]
    async fn non_2xx_is_auth_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let url = format!("{}/token", server.uri());
        let grant = RefreshGrant {
            token_url: &url,
            client_id: "cid",
            client_secret: "csec",
            refresh_token: "non_2xx_rt",
            scope: None,
        };
        let err = access_token(&reqwest::Client::new(), &grant)
            .await
            .expect_err("should be auth error");
        assert!(matches!(err, GatewayError::Auth(_)));
    }
}
```

- [ ] **Step 4:** `cargo test -p spur-rest-table-gateway oauth -- --nocapture` → PASS. Commit `error.rs`, `adapter/oauth.rs`, `adapter/mod.rs`.

---

### Task 2: manifest variant + async `resolve_auth` wiring

**Task ID:** `task-oauth-runtime`
**Depends on:** `task-oauth-service`
**Suggested Worker:** codex

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest.rs` (enum `AuthCfg`)
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/nango.rs` (`auth_to_toml`)
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs` (`resolve_auth` + `scan`)

**Scope Boundary:**
- IN: the three files above.
- OUT: `oauth.rs` (done in T1), `nango.rs::auth_cfg` (that's T3), the snapshot. Emit `scope_drift` otherwise.

**Acceptance Criteria:**
- [ ] A TOML manifest with `auth = { scheme = "oauth2_refresh", … }` parses to `AuthCfg::Oauth2Refresh` and round-trips through `auth_to_toml`.
- [ ] `scan` against an `oauth2_refresh` manifest mints a token (mock token endpoint) and sends `Authorization: Bearer <minted>` to the resource API.
- [ ] A missing credential env var yields `GatewayError::Auth`, never a silent unauthenticated request.
- [ ] `cargo test -p spur-rest-table-gateway` passes (all prior tests stay green).

**Implementation:**

- [ ] **Step 1: Add the variant** to `manifest.rs` `enum AuthCfg` (serde tag `scheme`, `rename_all = "snake_case"` → matches `"oauth2_refresh"`):

```rust
    Oauth2Refresh {
        token_url: String,
        client_id_env: String,
        client_secret_env: String,
        refresh_token_env: String,
        #[serde(default)]
        scope: Option<String>,
    },
```

- [ ] **Step 2: Add the serialization arm** to `nango.rs` `fn auth_to_toml` (match on `AuthCfg`):

```rust
        AuthCfg::Oauth2Refresh {
            token_url,
            client_id_env,
            client_secret_env,
            refresh_token_env,
            scope,
        } => {
            let mut out = format!(
                "{{ scheme = \"oauth2_refresh\", token_url = {}, client_id_env = {}, client_secret_env = {}, refresh_token_env = {}",
                toml_string(token_url),
                toml_string(client_id_env),
                toml_string(client_secret_env),
                toml_string(refresh_token_env)
            );
            if let Some(scope) = scope {
                out.push_str(&format!(", scope = {}", toml_string(scope)));
            }
            out.push_str(" }");
            out
        }
```

- [ ] **Step 3: Make `resolve_auth` async + fallible** in `manifest_adapter.rs`. Change the signature to `async fn resolve_auth(&self) -> Result<ResolvedAuth>`, wrap the existing four arms in `Ok(...)`, and add the new arm:

```rust
    async fn resolve_auth(&self) -> Result<ResolvedAuth> {
        Ok(match &self.manifest.source.auth {
            AuthCfg::None => ResolvedAuth::None,
            AuthCfg::Bearer { env } => std::env::var(env)
                .map(ResolvedAuth::Bearer)
                .unwrap_or(ResolvedAuth::None),
            AuthCfg::Header { name, env } => std::env::var(env)
                .map(|value| ResolvedAuth::Header { name: name.clone(), value })
                .unwrap_or(ResolvedAuth::None),
            AuthCfg::Basic { user_env, pass_env } => {
                match (std::env::var(user_env), std::env::var(pass_env)) {
                    (Ok(user), Ok(pass)) => ResolvedAuth::Basic { user, pass },
                    _ => ResolvedAuth::None,
                }
            }
            AuthCfg::ApiKeyQuery { param, env } => std::env::var(env)
                .map(|value| ResolvedAuth::QueryParam { param: param.clone(), value })
                .unwrap_or(ResolvedAuth::None),
            AuthCfg::Oauth2Refresh {
                token_url,
                client_id_env,
                client_secret_env,
                refresh_token_env,
                scope,
            } => {
                let ctx = ConnectionContext::from_env(&self.manifest.source.connection_config);
                let token_url = resolve_template(token_url, &ctx)?;
                let read = |name: &str| {
                    std::env::var(name)
                        .map_err(|_| GatewayError::Auth(format!("missing credential env var {name}")))
                };
                let client_id = read(client_id_env)?;
                let client_secret = read(client_secret_env)?;
                let refresh_token = read(refresh_token_env)?;
                let grant = crate::adapter::oauth::RefreshGrant {
                    token_url: &token_url,
                    client_id: &client_id,
                    client_secret: &client_secret,
                    refresh_token: &refresh_token,
                    scope: scope.as_deref(),
                };
                let token = crate::adapter::oauth::access_token(&self.client, &grant).await?;
                ResolvedAuth::Bearer(token)
            }
        })
    }
```

- [ ] **Step 4: Update the `scan` call site** (manifest_adapter.rs:200): replace `let auth = self.resolve_auth();` with:

```rust
        let auth = self.resolve_auth().await?;
```

- [ ] **Step 5: Add tests** to `manifest_adapter.rs` `mod tests` (mirror `base_url_templated`, which already uses `wiremock`). One unit round-trip + one scan integration:

```rust
    #[test]
    fn oauth2_refresh_toml_roundtrips() {
        let manifest = Manifest::from_toml(
            r#"
[source]
name = "notion"
base_url = "https://api.notion.com/v1"
auth = { scheme = "oauth2_refresh", token_url = "https://api.notion.com/v1/oauth/token", client_id_env = "NOTION_CLIENT_ID", client_secret_env = "NOTION_CLIENT_SECRET", refresh_token_env = "NOTION_REFRESH_TOKEN" }

[[table]]
name = "pages"
path = "/pages"

[table.columns]
id = { json = "$.id", type = "Utf8" }
"#,
        )
        .expect("manifest should parse");
        match manifest.source.auth {
            crate::adapter::manifest::AuthCfg::Oauth2Refresh { token_url, refresh_token_env, .. } => {
                assert_eq!(token_url, "https://api.notion.com/v1/oauth/token");
                assert_eq!(refresh_token_env, "NOTION_REFRESH_TOKEN");
            }
            other => panic!("expected oauth2_refresh, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn oauth2_refresh_scan_sends_minted_bearer() {
        let token_srv = MockServer::start().await;
        let api_srv = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "minted-xyz", "expires_in": 3600
            })))
            .mount(&token_srv)
            .await;
        Mock::given(method("GET"))
            .and(path("/things"))
            .and(header("authorization", "Bearer minted-xyz"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([{ "id": "t1" }])))
            .mount(&api_srv)
            .await;

        std::env::set_var("OAUTHSCAN_CLIENT_ID", "cid");
        std::env::set_var("OAUTHSCAN_CLIENT_SECRET", "csec");
        std::env::set_var("OAUTHSCAN_REFRESH_TOKEN", "oauth2_refresh_scan_rt");

        let toml = format!(
            r#"
[source]
name = "oauthscan"
base_url = "{api}"
auth = {{ scheme = "oauth2_refresh", token_url = "{token}/oauth/token", client_id_env = "OAUTHSCAN_CLIENT_ID", client_secret_env = "OAUTHSCAN_CLIENT_SECRET", refresh_token_env = "OAUTHSCAN_REFRESH_TOKEN" }}

[[table]]
name = "things"
path = "/things"

[table.columns]
id = {{ json = "$.id", type = "Utf8" }}
"#,
            api = api_srv.uri(),
            token = token_srv.uri()
        );
        let adapter = ManifestAdapter::new(Manifest::from_toml(&toml).expect("parse"));
        let batches = adapter
            .scan(ScanRequest {
                table: "things".to_string(),
                predicates: vec![],
                projection: None,
                tvf_args: vec![],
                auth: ResolvedAuth::None,
            })
            .await
            .expect("scan should succeed");
        assert_eq!(batches[0].num_rows(), 1);
    }
```

> Ensure the test `use` block imports `header` from `wiremock::matchers` (it already imports `method`, `path`, `query_param`).

- [ ] **Step 6:** `cargo test -p spur-rest-table-gateway -- --nocapture` → PASS. Commit the three files.

---

### Task 3: Nango emits `oauth2_refresh` + snapshot `token_url`

**Task ID:** `task-oauth-nango`
**Depends on:** `task-oauth-runtime`
**Suggested Worker:** codex

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/nango.rs` (`ProviderEntry`, `auth_cfg`, tests)
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/nango_providers_snapshot.yaml` (add `token_url` to OAuth providers)

**Scope Boundary:**
- IN: the two files above. OUT: `manifest.rs`, `manifest_adapter.rs`, `oauth.rs`. Emit `scope_drift` otherwise.

**Acceptance Criteria:**
- [ ] `provider_to_manifest_stub` emits `AuthCfg::Oauth2Refresh` for an OAUTH2 provider that carries `token_url`, with env names `<UPPER>_CLIENT_ID` / `_CLIENT_SECRET` / `_REFRESH_TOKEN`.
- [ ] A provider with no `token_url` still maps to `AuthCfg::Bearer` (no regression — `oauth_maps_to_bearer_byo` stays green if its SAMPLE salesforce gains no token_url; do NOT add token_url to the in-test SAMPLE).
- [ ] `cargo test -p spur-rest-table-gateway` passes.

**Implementation:**

- [ ] **Step 1: Add `token_url` to `ProviderEntry`:**

```rust
pub struct ProviderEntry {
    pub display_name: Option<String>,
    pub categories: Option<Vec<String>>,
    pub auth_mode: Option<String>,
    pub token_url: Option<String>,
    pub proxy: Option<Proxy>,
}
```

- [ ] **Step 2: Branch the fallback arm of `auth_cfg`** (currently `_ => AuthCfg::Bearer { … }`):

```rust
        _ => {
            if let Some(token_url) = p.token_url.clone() {
                AuthCfg::Oauth2Refresh {
                    token_url,
                    client_id_env: format!("{upper}_CLIENT_ID"),
                    client_secret_env: format!("{upper}_CLIENT_SECRET"),
                    refresh_token_env: format!("{upper}_REFRESH_TOKEN"),
                    scope: None,
                }
            } else {
                AuthCfg::Bearer {
                    env: format!("{}_TOKEN", env_prefix(name)),
                }
            }
        }
```

- [ ] **Step 3: Write the failing test** in `nango.rs` `mod tests`:

```rust
    #[test]
    fn oauth2_with_token_url_maps_to_refresh_grant() {
        let providers = parse_providers(
            r#"
notion:
  display_name: Notion
  auth_mode: OAUTH2
  token_url: "https://api.notion.com/v1/oauth/token"
  proxy:
    base_url: "https://api.notion.com/v1"
"#,
        )
        .expect("providers yaml should parse");
        let manifest = provider_to_manifest_stub("notion", &providers["notion"]);
        match manifest.source.auth {
            crate::adapter::manifest::AuthCfg::Oauth2Refresh {
                token_url, client_id_env, client_secret_env, refresh_token_env, ..
            } => {
                assert_eq!(token_url, "https://api.notion.com/v1/oauth/token");
                assert_eq!(client_id_env, "NOTION_CLIENT_ID");
                assert_eq!(client_secret_env, "NOTION_CLIENT_SECRET");
                assert_eq!(refresh_token_env, "NOTION_REFRESH_TOKEN");
            }
            other => panic!("expected oauth2_refresh, got {other:?}"),
        }
    }

    #[test]
    fn oauth2_without_token_url_stays_bearer() {
        let providers = parse_providers(
            r#"
legacy:
  display_name: Legacy
  auth_mode: OAUTH2
  proxy:
    base_url: "https://api.legacy.test"
"#,
        )
        .expect("providers yaml should parse");
        let manifest = provider_to_manifest_stub("legacy", &providers["legacy"]);
        assert!(matches!(
            manifest.source.auth,
            crate::adapter::manifest::AuthCfg::Bearer { .. }
        ));
    }
```

- [ ] **Step 4: Enrich the snapshot** — add a `token_url:` line under each OAUTH2 provider in `nango_providers_snapshot.yaml` (Plaid stays TWO_STEP, no token_url). Use the documented token endpoints, e.g.:

```yaml
notion:
  auth_mode: OAUTH2
  token_url: "https://api.notion.com/v1/oauth/token"
slack:
  auth_mode: OAUTH2
  token_url: "https://slack.com/api/oauth.v2.access"
hubspot:
  auth_mode: OAUTH2
  token_url: "https://api.hubapi.com/oauth/v1/token"
salesforce:
  auth_mode: OAUTH2
  token_url: "${connectionConfig.salesforce_instance_url}/services/oauth2/token"
intercom:
  auth_mode: OAUTH2
  token_url: "https://api.intercom.io/auth/eagle/token"
asana:
  auth_mode: OAUTH2
  token_url: "https://app.asana.com/-/oauth_token"
google-calendar:
  auth_mode: OAUTH2
  token_url: "https://oauth2.googleapis.com/token"
gmail:
  auth_mode: OAUTH2
  token_url: "https://oauth2.googleapis.com/token"
jira:
  auth_mode: OAUTH2
  token_url: "https://auth.atlassian.com/oauth/token"
trello:
  auth_mode: OAUTH2
  token_url: "https://trello.com/1/OAuthGetAccessToken"
```

(Keep each provider's existing `display_name`/`categories`/`proxy` lines; only add `token_url`.)

- [ ] **Step 5:** `cargo test -p spur-rest-table-gateway -- --nocapture` → PASS. Commit `nango.rs` + the snapshot.

---

## Self-review
- **Spec coverage:** T1 = token service (§3 mint/cache/refresh + §7 failure handling); T2 = manifest contract (§4) + async seam (§2 request plane); T3 = Nango translator + snapshot (§2 config plane, §1 data gap). Wizard `required_env_vars` (§5) explicitly deferred.
- **DAG:** linear T1→T2→T3 (forced by enum exhaustiveness + token-service dependency). No cycles.
- **Type consistency:** `RefreshGrant`, `oauth::access_token`, `GatewayError::Auth`, `AuthCfg::Oauth2Refresh` field names match across tasks.
- **No placeholders:** every step has concrete code and a runnable `cargo test`.
