# OAuth2 Authorization-Code Browser Wiring (Approach C — runnable) — Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-03-oauth2-authcode-browser-flow-design.ipynb` (committed `65a64e22`)
**Predecessor plan:** `docs/superpowers/plans/2026-06-03-oauth2-authcode-foundation-plan.md` (T1–T3 primitives shipped: `oauth::exchange_code`, `bind_loopback`/`await_callback`, `TokenResponse.refresh_token`, `ProviderEntry` auth-code field parsing)
**Design epic:** none — design captured in the spec notebook.

**Goal:** Make the browser authorization-code flow actually runnable end-to-end so a user acquires a connection's `refresh_token` with one click (browser consent → loopback → exchange → persisted), instead of pasting a token.

**Architecture:** Keep the GUI-agnostic gateway crate pure — it gains only PKCE/state generation and an authorization-URL builder (Task 1). The interactive orchestration (open browser, capture redirect, exchange, persist, register) lives in the notebook/Tauri layer (Task 4) and is exposed via a daemon command + MCP tool with a wizard branch (Task 5). The acquired `refresh_token` is written through a new credential sink (Task 4 dependency, Task 2) — a 0600 JSON file behind a trait, **separate from `~/.spur/connections.json`** so the existing "no secrets in the catalog" invariant (the ext reads `connections.json`) is preserved. Provider data for Google/Facebook Ads is filled in (Task 3).

**Tech Stack:** Rust, `reqwest` (re-exports `Url` for URL building — no `url` dep needed), `tokio` (`net` for loopback), `tauri-plugin-opener` 2.2.3 (already a dep — opens the system browser from the daemon via the `AppHandle`). Task 1 adds `sha2`, `base64`, `rand` to the gateway crate for PKCE (match workspace versions if already present elsewhere). Dev: `wiremock` (already a dev-dep).

**§6 open question — RESOLVED (was blocking):** `connection_store` persists `credential_env_vars` (names only) + `manifest_toml`; **secret values are intentionally never persisted** (`crates/spur-notebook/src/mcp/mod.rs:1922` comment). The sink is therefore genuinely new surface. v1 = dedicated 0600 file `~/.spur/credentials.json` behind a `CredentialSink` trait (Option C / keychain deferred to v2). This keeps secrets out of the ext-readable `connections.json`.

---

## File Structure Mapping

| File | Responsibility | Touched by |
|------|----------------|------------|
| `crates/spur-notebook/rest-table-gateway/Cargo.toml` | Add `sha2`/`base64`/`rand` deps | Task 1 |
| `crates/spur-notebook/rest-table-gateway/src/adapter/oauth.rs` | PKCE + state + `build_authorize_url` (pure) | Task 1 |
| `crates/spur-notebook/src/connection_secrets.rs` (new) | `CredentialSink` trait + 0600 file impl + env loader | Task 2 |
| `crates/spur-notebook/src/lib.rs` | `pub mod connection_secrets;` | Task 2 |
| `crates/spur-notebook/jute-notebook/src-tauri/src/nango_providers_snapshot.yaml` | `authorization_url`/`authorization_params`/`scope_separator` for google-ads + facebook-ads | Task 3 |
| `crates/spur-notebook/rest-table-gateway/connections/tier-a/google_ads.connection.toml` | add `scope` to oauth2_refresh auth | Task 3 |
| `crates/spur-notebook/rest-table-gateway/connections/tier-a/facebook_ads.connection.toml` | add `scope` to oauth2_refresh auth | Task 3 |
| `crates/spur-notebook/src/mcp/mod.rs` | `oauth_connect` orchestrator + `DaemonControlCommand::OauthConnect` dispatch | Task 4 |
| `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` | `DaemonControlCommand::OauthConnect { name }` variant | Task 4 |
| `crates/spur-notebook/src/mcp/tools/api_connection.rs` | `notebook_oauth_connect` MCP tool + "connect_with_browser" wizard branch | Task 5 |

**Collision control:** Task 1 and Task 4 both edit `oauth.rs`/`mod.rs` regions but in different files for the pure vs. orchestration split; Task 4 depends on Task 1 so the `oauth.rs` API exists first. Task 5 depends on Task 4 (needs the daemon command).

---

## Dependency DAG

```
Task 1 (oauth primitives) ─┐
Task 2 (credential sink) ──┼─► Task 4 (orchestrator + daemon cmd) ─► Task 5 (MCP tool + wizard branch)
Task 3 (provider data) ────┘
```

Roots (parallel): **Task 1, Task 2, Task 3**. Then **Task 4** (depends 1,2,3). Then **Task 5** (depends 4).

---

### Task 1: OAuth PKCE + state + authorize-URL builder

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/Cargo.toml` (deps)
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/oauth.rs` (add pure helpers + tests)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `generate_pkce()` returns a verifier and its S256 challenge such that `challenge == base64url_nopad(sha256(verifier))`.
- [ ] `generate_state()` returns a non-empty URL-safe nonce; two calls differ.
- [ ] `build_authorize_url` produces a URL containing `response_type=code`, `code_challenge_method=S256`, the scope, state, challenge, redirect_uri, and any extra params.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway oauth` green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `oauth.rs` new pure functions + `Cargo.toml` deps. No browser, no I/O, no wizard.
- OUT of scope: `bind_loopback`/`await_callback`/`exchange_code` (already shipped — do not modify), any notebook-crate file.
- If you need to touch files outside this list → emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Add deps** to `crates/spur-notebook/rest-table-gateway/Cargo.toml` under `[dependencies]` (match versions already used elsewhere in the workspace if present):

```toml
sha2 = "0.10"
base64 = "0.22"
rand = "0.9"
```

- [ ] **Step 2: Write failing tests** appended to the `#[cfg(test)] mod tests` block in `oauth.rs`:

```rust
#[test]
fn pkce_challenge_is_s256_of_verifier() {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let pkce = generate_pkce();
    assert!(!pkce.verifier.is_empty());
    let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(pkce.verifier.as_bytes()));
    assert_eq!(pkce.challenge, expected);
    assert!(!pkce.challenge.contains('='));
}

#[test]
fn state_nonce_is_unique_and_urlsafe() {
    let a = generate_state();
    let b = generate_state();
    assert_ne!(a, b);
    assert!(!a.is_empty());
    assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
}

#[test]
fn authorize_url_contains_required_params() {
    let url = build_authorize_url(&AuthorizeUrlParams {
        authorization_url: "https://accounts.google.com/o/oauth2/v2/auth",
        client_id: "cid.apps.googleusercontent.com",
        redirect_uri: "http://127.0.0.1:51847/callback",
        scope: "https://www.googleapis.com/auth/adwords",
        state: "st8",
        code_challenge: "chal",
        extra: &[("access_type", "offline"), ("prompt", "consent")],
    })
    .expect("url");
    assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
    assert!(url.contains("response_type=code"));
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("code_challenge=chal"));
    assert!(url.contains("state=st8"));
    assert!(url.contains("access_type=offline"));
    assert!(url.contains("prompt=consent"));
    assert!(url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fadwords"));
}
```

- [ ] **Step 3: Run** `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway oauth` — expect FAIL (items not defined).

- [ ] **Step 4: Implement** in `oauth.rs` (above the `#[cfg(test)]` block):

```rust
use base64::Engine as _;
use rand::RngCore as _;
use sha2::{Digest, Sha256};

/// PKCE verifier + its S256 challenge.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// Generate a PKCE pair: a high-entropy verifier and its base64url(S256) challenge.
pub fn generate_pkce() -> Pkce {
    let mut bytes = [0u8; 64];
    rand::rng().fill_bytes(&mut bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let challenge =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Pkce { verifier, challenge }
}

/// Generate a single-use CSRF `state` nonce (URL-safe, no padding).
pub fn generate_state() -> String {
    let mut bytes = [0u8; 24];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Inputs for the consent URL. `extra` carries provider `authorization_params`
/// (e.g. `access_type=offline`, `prompt=consent`).
pub struct AuthorizeUrlParams<'a> {
    pub authorization_url: &'a str,
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub scope: &'a str,
    pub state: &'a str,
    pub code_challenge: &'a str,
    pub extra: &'a [(&'a str, &'a str)],
}

/// Build the provider consent URL with PKCE + state. Uses `reqwest::Url`
/// (re-exported) so query values are correctly percent-encoded.
pub fn build_authorize_url(p: &AuthorizeUrlParams<'_>) -> Result<String> {
    let mut params: Vec<(&str, &str)> = vec![
        ("client_id", p.client_id),
        ("redirect_uri", p.redirect_uri),
        ("response_type", "code"),
        ("scope", p.scope),
        ("state", p.state),
        ("code_challenge", p.code_challenge),
        ("code_challenge_method", "S256"),
    ];
    params.extend_from_slice(p.extra);
    let url = reqwest::Url::parse_with_params(p.authorization_url, &params)
        .map_err(|e| GatewayError::Auth(format!("authorize url build failed: {e}")))?;
    Ok(url.to_string())
}
```

- [ ] **Step 5: Run** the test command again — expect PASS. Then `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-rest-table-gateway -- -D warnings`.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/Cargo.toml crates/spur-notebook/rest-table-gateway/src/adapter/oauth.rs
git commit -m "feat(rest-table-gateway): task-1 PKCE + state + authorize-url builder"
```

---

### Task 2: Credential sink (trait + 0600 file + env loader)

**Task ID:** `task-2`

**Files:**
- Create: `crates/spur-notebook/src/connection_secrets.rs`
- Modify: `crates/spur-notebook/src/lib.rs` (add `pub mod connection_secrets;`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `FileCredentialSink::store(key, value)` then `load_all()` round-trips the pair.
- [ ] The backing file is created with `0o600` permissions (unix).
- [ ] `load_secrets_into_env()` sets every stored pair into the process env and returns the count.
- [ ] Secrets are written to `~/.spur/credentials.json`, **not** `connections.json`.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook connection_secrets` green.

**Suggested Worker:** claude-code-acp (security-sensitive; trait boundary)

**Scope Boundary:**
- IN scope: new `connection_secrets.rs` + the one `lib.rs` module line.
- OUT of scope: `connection_store.rs` (do not co-mingle secrets), `mcp/mod.rs`, the ext.
- If you need to touch out-of-scope files → emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Write failing tests** in the new file's `#[cfg(test)] mod tests`:

```rust
#[tokio::test]
async fn store_then_load_round_trips() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("credentials.json");
    let sink = FileCredentialSink::at(path.clone());
    sink.store("GOOGLE_ADS_REFRESH_TOKEN", "1//abc").await.expect("store");
    let all = sink.load_all().await.expect("load");
    assert_eq!(all.get("GOOGLE_ADS_REFRESH_TOKEN").map(String::as_str), Some("1//abc"));
}

#[cfg(unix)]
#[tokio::test]
async fn secrets_file_is_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("credentials.json");
    let sink = FileCredentialSink::at(path.clone());
    sink.store("K", "v").await.expect("store");
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test]
async fn load_into_env_sets_vars() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("credentials.json");
    let sink = FileCredentialSink::at(path.clone());
    sink.store("SPUR_TEST_SECRET_X", "yes").await.expect("store");
    let n = sink.load_into_env().await.expect("load env");
    assert!(n >= 1);
    assert_eq!(std::env::var("SPUR_TEST_SECRET_X").ok().as_deref(), Some("yes"));
}
```

- [ ] **Step 2: Run** `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook connection_secrets` — expect FAIL.

- [ ] **Step 3: Implement** `connection_secrets.rs`:

```rust
//! Credential sink for OAuth-acquired secrets (Approach C).
//!
//! Secrets are stored in a dedicated 0600 file (`~/.spur/credentials.json`),
//! deliberately separate from `connections.json` (which the DuckDB extension
//! reads and must stay secret-free). The `CredentialSink` trait keeps a future
//! OS-keychain backend swappable without touching callers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[async_trait]
pub trait CredentialSink: Send + Sync {
    async fn store(&self, key: &str, value: &str) -> Result<()>;
    async fn load_all(&self) -> Result<BTreeMap<String, String>>;
    /// Set every stored secret into the process environment; returns the count.
    async fn load_into_env(&self) -> Result<usize> {
        let all = self.load_all().await?;
        let n = all.len();
        for (k, v) in all {
            std::env::set_var(k, v);
        }
        Ok(n)
    }
}

#[derive(Default, Serialize, Deserialize)]
struct SecretsFile {
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

pub struct FileCredentialSink {
    path: PathBuf,
}

impl FileCredentialSink {
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// Default sink at `~/.spur/credentials.json`.
    pub fn default_path() -> Result<Self> {
        let home = dirs::home_dir().context("could not resolve home directory")?;
        Ok(Self::at(home.join(".spur").join("credentials.json")))
    }

    async fn read(&self) -> Result<SecretsFile> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SecretsFile::default()),
            Err(e) => Err(e).with_context(|| format!("failed to read {}", self.path.display())),
        }
    }

    async fn write(&self, file: &SecretsFile) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let bytes = serde_json::to_vec_pretty(file)?;
        tokio::fs::write(&self.path, &bytes).await
            .with_context(|| format!("failed to write {}", self.path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600)).await.ok();
        }
        Ok(())
    }
}

#[async_trait]
impl CredentialSink for FileCredentialSink {
    async fn store(&self, key: &str, value: &str) -> Result<()> {
        let mut file = self.read().await?;
        file.secrets.insert(key.to_string(), value.to_string());
        self.write(&file).await
    }

    async fn load_all(&self) -> Result<BTreeMap<String, String>> {
        Ok(self.read().await?.secrets)
    }
}

/// Convenience: load `~/.spur/credentials.json` into the process env at startup
/// and before kernel spawn so the DuckDB extension inherits OAuth secrets.
pub async fn load_secrets_into_env() -> Result<usize> {
    FileCredentialSink::default_path()?.load_into_env().await
}
```

> If `async-trait`, `dirs`, or `tempfile` (dev) are not already deps of `spur-notebook`, add them to `crates/spur-notebook/Cargo.toml` (`async-trait` and `dirs` are widely used in the workspace; `tempfile` under `[dev-dependencies]`). Mirror `connection_store.rs`'s home-dir resolution if it uses a different crate than `dirs`.

- [ ] **Step 4: Wire module** — add to `crates/spur-notebook/src/lib.rs`:

```rust
pub mod connection_secrets;
```

- [ ] **Step 5: Run** the test command — expect PASS. Then clippy.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/src/connection_secrets.rs crates/spur-notebook/src/lib.rs crates/spur-notebook/Cargo.toml
git commit -m "feat(spur-notebook): task-2 credential sink (0600 file behind trait)"
```

---

### Task 3: Provider auth-code data for Google/Facebook Ads

**Task ID:** `task-3`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/nango_providers_snapshot.yaml`
- Modify: `crates/spur-notebook/rest-table-gateway/connections/tier-a/google_ads.connection.toml`
- Modify: `crates/spur-notebook/rest-table-gateway/connections/tier-a/facebook_ads.connection.toml`
- Test: `crates/spur-notebook/rest-table-gateway/src/adapter/nango.rs` (add a parse test)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `parse_providers(NANGO snapshot)` returns `google-ads` with `authorization_url == "https://accounts.google.com/o/oauth2/v2/auth"` and non-empty `authorization_params`.
- [ ] `google_ads.connection.toml` parses via `Manifest::from_toml` with `AuthCfg::Oauth2Refresh { scope: Some("https://www.googleapis.com/auth/adwords"), .. }`.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway nango` green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the two preset TOMLs, the snapshot YAML, one nango parse test.
- OUT of scope: `oauth.rs`, `mcp/mod.rs`, any orchestration.

**Implementation:**

- [ ] **Step 1: Write failing test** in `nango.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn google_ads_snapshot_has_authorization_url() {
    const SNAPSHOT: &str =
        include_str!("../../../jute-notebook/src-tauri/src/nango_providers_snapshot.yaml");
    let providers = parse_providers(SNAPSHOT).expect("parse snapshot");
    let g = providers.get("google-ads").expect("google-ads present");
    assert_eq!(
        g.authorization_url.as_deref(),
        Some("https://accounts.google.com/o/oauth2/v2/auth")
    );
    assert!(g.authorization_params.as_ref().is_some_and(|p| p.contains_key("access_type")));
}
```

- [ ] **Step 2: Run** `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway nango` — expect FAIL.

- [ ] **Step 3: Edit the snapshot** — add to the `google-ads:` entry (after `token_url:`):

```yaml
  authorization_url: "https://accounts.google.com/o/oauth2/v2/auth"
  authorization_params:
    access_type: offline
    prompt: consent
  scope_separator: " "
```

  and to `facebook-ads:`:

```yaml
  authorization_url: "https://www.facebook.com/v21.0/dialog/oauth"
  scope_separator: ","
```

- [ ] **Step 4: Add `scope`** to `google_ads.connection.toml`'s auth line:

```toml
auth = { scheme = "oauth2_refresh", token_url = "https://oauth2.googleapis.com/token", client_id_env = "GOOGLE_ADS_CLIENT_ID", client_secret_env = "GOOGLE_ADS_CLIENT_SECRET", refresh_token_env = "GOOGLE_ADS_REFRESH_TOKEN", scope = "https://www.googleapis.com/auth/adwords" }
```

  and to `facebook_ads.connection.toml`'s auth line add `scope = "ads_read"`.

- [ ] **Step 5: Run** the test command — expect PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src-tauri/src/nango_providers_snapshot.yaml crates/spur-notebook/rest-table-gateway/connections/tier-a/google_ads.connection.toml crates/spur-notebook/rest-table-gateway/connections/tier-a/facebook_ads.connection.toml crates/spur-notebook/rest-table-gateway/src/adapter/nango.rs
git commit -m "feat(rest-table-gateway): task-3 authorize-url + scope for ads providers"
```

---

### Task 4: `oauth_connect` orchestrator + daemon command

**Task ID:** `task-4`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs` (add `OauthConnect { name }` variant)
- Modify: `crates/spur-notebook/src/mcp/mod.rs` (dispatch arm + `oauth_connect` method)

**Depends on:** `task-1`, `task-2`, `task-3`

**Acceptance Criteria:**
- [ ] New `DaemonControlCommand::OauthConnect { name }` deserializes from `{ "OauthConnect": { "name": "google_ads" } }`.
- [ ] `oauth_connect` loads the saved connection's manifest, builds the consent URL from `task-1` helpers + the provider's `authorization_url`, binds the loopback, opens the browser via `tauri-plugin-opener`, awaits the redirect, verifies `state`, exchanges the code, stores `refresh_token` via the `task-2` sink, sets it in the process env, and re-registers the datasource.
- [ ] State-mismatch and non-2xx exchange return `BridgeError::Handler` (never panic).
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook oauth_connect` green (test covers the non-browser orchestration seam — see Step 1).

**Suggested Worker:** claude-code-acp (multi-file, Tauri + async orchestration)

**Scope Boundary:**
- IN scope: the `commands.rs` enum variant + `mod.rs` dispatch arm + the `oauth_connect` method and a small testable helper.
- OUT of scope: `oauth.rs` (consume only), `api_connection.rs` (Task 5), the React frontend.
- **Scope-drift checkpoint:** if opening the browser requires changes outside `tauri-plugin-opener`'s `OpenerExt`, or if the manifest doesn't expose `authorization_url` → emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Write a failing test** for the testable seam (factor the post-redirect half — exchange + persist — into `complete_oauth_connect` so it's unit-testable without a browser). In `mcp/mod.rs` tests (or a sibling test module):

```rust
#[tokio::test]
async fn complete_oauth_connect_persists_refresh_token() {
    // wiremock token endpoint returns a refresh_token for grant_type=authorization_code
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::body_string_contains("grant_type=authorization_code"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "at", "refresh_token": "rt-123", "expires_in": 3600
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let sink = crate::connection_secrets::FileCredentialSink::at(dir.path().join("credentials.json"));

    let token = complete_oauth_connect(
        &reqwest::Client::new(),
        &format!("{}/token", server.uri()),
        "cid", "secret", "code-xyz", "verifier",
        "http://127.0.0.1:0/callback",
        "GOOGLE_ADS_REFRESH_TOKEN",
        &sink,
    )
    .await
    .expect("complete");

    assert_eq!(token, "rt-123");
    assert_eq!(
        sink.load_all().await.unwrap().get("GOOGLE_ADS_REFRESH_TOKEN").map(String::as_str),
        Some("rt-123")
    );
}
```

- [ ] **Step 2: Run** `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook oauth_connect` — expect FAIL.

- [ ] **Step 3: Add the command variant** in `commands.rs` (next to `AddApiDatasourceFromManifest`):

```rust
    OauthConnect {
        name: String,
    },
```

- [ ] **Step 4: Implement** the testable seam + orchestrator in `mcp/mod.rs` (gated `#[cfg(feature = "datasource-introspect")]` like its siblings):

```rust
use spur_rest_table_gateway::adapter::oauth;
use crate::connection_secrets::CredentialSink;

/// Post-redirect half: exchange the code and persist the refresh token. Pure
/// enough to unit-test without a browser.
async fn complete_oauth_connect(
    client: &reqwest::Client,
    token_url: &str,
    client_id: &str,
    client_secret: &str,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
    refresh_token_env: &str,
    sink: &dyn CredentialSink,
) -> Result<String, BridgeError> {
    let tokens = oauth::exchange_code(
        client,
        &oauth::AuthCodeGrant {
            token_url,
            client_id,
            client_secret,
            code,
            code_verifier,
            redirect_uri,
        },
    )
    .await
    .map_err(|e| BridgeError::Handler {
        code: "oauth_exchange_failed".to_string(),
        message: e.to_string(),
    })?;
    sink.store(refresh_token_env, &tokens.refresh_token)
        .await
        .map_err(|e| BridgeError::Handler {
            code: "oauth_persist_failed".to_string(),
            message: e.to_string(),
        })?;
    std::env::set_var(refresh_token_env, &tokens.refresh_token);
    Ok(tokens.refresh_token)
}
```

  Then the full orchestrator method on `impl NotebookDaemonControl` (browser half). It: loads the saved connection template (via `connection_store::list`) by `name`, parses its manifest to read `AuthCfg::Oauth2Refresh { token_url, client_id_env, client_secret_env, refresh_token_env, scope }`, resolves the provider's `authorization_url`/`authorization_params`/`scope_separator` from `NANGO_PROVIDERS_SNAPSHOT` (keyed by the template's `provider`), reads `client_id`/`client_secret` from env (the `*_env` names), then:

```rust
async fn oauth_connect(&self, name: String) -> Result<DaemonControlSuccess, BridgeError> {
    // 1. load template + manifest, extract Oauth2Refresh fields + provider key (omitted for brevity;
    //    mirror persist_and_register_manifest_api_datasource's manifest parsing)
    // 2. resolve authorization_url + extra params from NANGO_PROVIDERS_SNAPSHOT via nango::parse_providers
    // 3. PKCE + state + loopback:
    let pkce = oauth::generate_pkce();
    let state = oauth::generate_state();
    let (redirect_uri, listener) = oauth::bind_loopback().await.map_err(handler_err("oauth_loopback_failed"))?;
    let extra: Vec<(&str, &str)> = /* from authorization_params */ vec![];
    let auth_url = oauth::build_authorize_url(&oauth::AuthorizeUrlParams {
        authorization_url: &authorization_url,
        client_id: &client_id,
        redirect_uri: &redirect_uri,
        scope: scope.as_deref().unwrap_or(""),
        state: &state,
        code_challenge: &pkce.challenge,
        extra: &extra,
    }).map_err(handler_err("oauth_url_failed"))?;

    // 4. open the system browser via tauri-plugin-opener
    if let Some(app) = self.app.as_ref() {
        use tauri_plugin_opener::OpenerExt;
        let _ = app.opener().open_url(auth_url.clone(), None::<&str>);
    }

    // 5. await the single redirect and verify state
    let cb = oauth::await_callback(listener).await.map_err(handler_err("oauth_callback_failed"))?;
    if cb.state != state {
        return Err(BridgeError::Handler {
            code: "oauth_state_mismatch".to_string(),
            message: "redirect state did not match the issued nonce".to_string(),
        });
    }

    // 6. exchange + persist + set env
    let client = reqwest::Client::new();
    complete_oauth_connect(&client, &token_url, &client_id, &client_secret,
        &cb.code, &pkce.verifier, &redirect_uri, &refresh_token_env, &*self.credential_sink()).await?;

    // 7. re-register the datasource now that the refresh token resolves
    self.persist_and_register_manifest_api_datasource(name, provider, manifest_toml, Vec::new()).await
}
```

  where `handler_err(code)` is a small closure `move |e| BridgeError::Handler { code: code.into(), message: e.to_string() }`, and `credential_sink()` returns `FileCredentialSink::default_path()`. Add the dispatch arm:

```rust
DaemonControlCommand::OauthConnect { name } => self.oauth_connect(name).await,
```

- [ ] **Step 5: Run** the test command — expect PASS. Then `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-notebook -- -D warnings`.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs crates/spur-notebook/src/mcp/mod.rs
git commit -m "feat(spur-notebook): task-4 oauth_connect browser orchestrator + daemon cmd"
```

---

### Task 5: `notebook_oauth_connect` MCP tool + wizard branch

**Task ID:** `task-5`

**Files:**
- Modify: `crates/spur-notebook/src/mcp/tools/api_connection.rs` (new tool + "connect_with_browser" branch)
- Modify: `crates/spur-notebook/src/mcp/mod.rs` (register the tool method name in the dispatch table)

**Depends on:** `task-4`

**Acceptance Criteria:**
- [ ] A new MCP tool `notebook_oauth_connect { name }` dispatches `DaemonControlCommand::OauthConnect`.
- [ ] When `call_add_api_connection` finds an `Oauth2Refresh` manifest whose **only** missing env var is `refresh_token_env` (client id/secret present), it returns `status: "awaiting_oauth"` with `action: "connect_with_browser"` and the tool name to call — instead of the generic `awaiting_credentials`.
- [ ] Existing `add_api_connection_*` tests still pass.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook api_connection` green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `api_connection.rs` tool + branch, one dispatch registration line in `mod.rs`.
- OUT of scope: `oauth_connect` orchestrator (Task 4), `oauth.rs`, frontend.

**Implementation:**

- [ ] **Step 1: Write failing test** in `api_connection.rs` tests:

```rust
#[test]
fn oauth_only_missing_refresh_token_returns_connect_with_browser() {
    // manifest: oauth2_refresh with client id/secret present in env, refresh token absent
    let _cid = EnvVarGuard::set("GOOGLE_ADS_CLIENT_ID".into(), "x");
    let _sec = EnvVarGuard::set("GOOGLE_ADS_CLIENT_SECRET".into(), "y");
    std::env::remove_var("GOOGLE_ADS_REFRESH_TOKEN");
    let prepared = prepare_manifest(&AddApiConnectionParams {
        name: "google_ads".into(),
        provider: Some("google-ads".into()),
        spec_text: None,
        manifest_toml: None,
        connection_only: None,
    })
    .expect("prepare");
    let action = oauth_action_for(&prepared); // new helper
    assert_eq!(action.as_deref(), Some("connect_with_browser"));
}
```

- [ ] **Step 2: Run** `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook api_connection` — expect FAIL.

- [ ] **Step 3: Implement** — add the tool constant + handler mirroring `call_add_api_connection`'s shape:

```rust
pub const OAUTH_CONNECT_METHOD: &str = "notebook.oauth_connect";

#[derive(serde::Deserialize)]
struct OauthConnectParams {
    name: String,
}

pub async fn call_oauth_connect(
    deps: &ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: OauthConnectParams = serde_json::from_value(arguments)
        .map_err(|e| McpError::invalid_params(
            format!("{OAUTH_CONNECT_METHOD} requires {{ name }}"),
            Some(json!({ "error": e.to_string() })),
        ))?;
    let daemon = deps.daemon.as_ref().ok_or_else(daemon_unavailable)?;
    let response = daemon
        .handle(daemon_request(jute::commands::DaemonControlCommand::OauthConnect { name: params.name }))
        .await;
    let response = check_response(response)?;
    let _ = daemon_result(OAUTH_CONNECT_METHOD, response)?;
    Ok(CallToolResult::structured(json!({ "status": "ready" })))
}

/// Returns Some("connect_with_browser") when the manifest is Oauth2Refresh and
/// the only missing env var is the refresh token (client id/secret present).
fn oauth_action_for(prepared: &PreparedManifest) -> Option<String> {
    // parse prepared.manifest_toml; if AuthCfg::Oauth2Refresh and
    // missing_env_vars == [refresh_token_env] → Some("connect_with_browser")
    // else None  (full impl reads prepared.required_env_vars + the manifest auth)
    None // replace with real check during implementation
}
```

  Then in `call_add_api_connection`, before the generic `awaiting_credentials` return, branch:

```rust
if let Some(action) = oauth_action_for(&prepared) {
    return Ok(CallToolResult::structured(json!({
        "status": "awaiting_oauth",
        "name": params.name,
        "action": action,
        "tool": OAUTH_CONNECT_METHOD,
        "message": "Click Connect with browser to authorize this connection."
    })));
}
```

  Register the method in `mod.rs`'s dispatch match:

```rust
"notebook.oauth_connect" => tools::api_connection::call_oauth_connect(&self.deps, arguments).await,
```

  (Replace the `oauth_action_for` placeholder body with the real manifest/env check — it is the crux of this task, not a stub at completion.)

- [ ] **Step 4: Run** the test command — expect PASS. Then clippy.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/src/mcp/tools/api_connection.rs crates/spur-notebook/src/mcp/mod.rs
git commit -m "feat(spur-notebook): task-5 oauth_connect MCP tool + wizard branch"
```

---

## Post-merge integration (brain, after all tasks Approved)

1. `merge_plan` → cherry-pick task commits to `main`.
2. **Rebuild + reinstall the macOS app** so the running daemon contains the new code: `cargo run -p xtask -- install --remote` (or the local app build path) — the browser flow only works in a daemon built from this code.
3. Call `load_secrets_into_env()` at daemon startup and before kernel spawn (verify wired; if not, add a one-line call — tracked as a follow-up if out of plan scope).
4. Live smoke test: provide `GOOGLE_ADS_CLIENT_ID`/`SECRET` + developer token + login id; run `notebook_add_api_connection(google-ads)` → `awaiting_oauth` → `notebook_oauth_connect(google_ads)` → click Allow → connection `ready` → restart kernel → GAQL query.

---

## Deferred (not in this plan)

- Full React wizard states (Ready / Consent spinner / Connected) per spec §5 — this plan exposes the flow via MCP tool + structured `awaiting_oauth` action; the frontend can consume it next.
- OS keychain sink (spec §6 Option C).
- `Refreshing → Reauth → NeedsAuth` self-healing edge (spec §4) — the orchestrator exists; wiring query-time 4xx back to it is a follow-up.
- Facebook Ads end-to-end validation (data added, not live-tested).
