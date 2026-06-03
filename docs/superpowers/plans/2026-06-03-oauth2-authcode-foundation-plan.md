# OAuth2 Authorization-Code Foundation (Approach C) — Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-03-oauth2-authcode-browser-flow-design.ipynb` (committed `65a64e22`)
**Design epic:** none — design captured in the spec notebook. The §6 credential-sink decision is **deliberately deferred** (it needs a threat-review answer to "does the connection store persist credentials or only manifests?"). This plan ships only the decision-free, headlessly-verifiable foundation.

**Goal:** Build the three dependency-free, fully unit/wiremock-testable primitives that Approach C (browser authorization-code flow) needs — Nango auth-code field parsing, the `authorization_code` token exchange, and the loopback redirect listener — without wiring the interactive wizard or the token sink.

**Architecture:** All three primitives slot into existing files. Nango parsing gains optional fields on `ProviderEntry` (additive serde, consumed later by the deferred wizard). `oauth.rs` gains a one-shot `exchange_code` (sibling to the shipped `access_token` refresh grant) and a `tokio::net::TcpListener`-based loopback capture. Nothing is wired into the query-time path or the wizard yet, so each task is independently reviewable and reversible. PKCE *generation* and the wizard branch are explicitly out of scope (see §"Deferred").

**Tech Stack:** Rust, `reqwest` (json + urlencoded form — already deps), `tokio` (full, so `net` is available), `serde`/`serde_yaml`, `indexmap`. Dev: `wiremock` (already a dev-dep). **No new dependencies are introduced.**

---

## File Structure Mapping

| File | Responsibility | Touched by |
|------|----------------|------------|
| `crates/spur-notebook/rest-table-gateway/src/adapter/nango.rs` | Parse Nango provider auth-code fields into `ProviderEntry` | Task 1 |
| `crates/spur-notebook/rest-table-gateway/src/adapter/oauth.rs` | `authorization_code` exchange + `refresh_token` capture | Task 2 |
| `crates/spur-notebook/rest-table-gateway/src/adapter/oauth.rs` | Loopback redirect listener (one-shot) | Task 3 |

**Collision control:** Tasks 2 and 3 both edit `oauth.rs`, so **Task 3 depends on Task 2** to serialize the edits. Task 1 edits a different file (`nango.rs`) and runs in parallel with Task 2.

## Dependency DAG

```
Task 1 (nango parse) ─┐         (independent)
Task 2 (exchange_code) ─→ Task 3 (loopback)
```

- Task 1: `depends_on: []`
- Task 2: `depends_on: []`
- Task 3: `depends_on: [task-2-exchange-code]`

## Deferred (NOT in this plan — do not implement)

- **PKCE generation** (`code_verifier` + S256 `code_challenge` + `state`): needs base64url + a CSPRNG, neither in the workspace. That dependency choice is bundled with the §6 review. `exchange_code` takes a caller-supplied `code_verifier: &str` so the exchange itself is testable now.
- **Credential sink** (§6 of the spec): the one genuine design decision. Blocked on the threat-review question above.
- **Wizard "Connect with browser" branch** (`api_connection.rs`): orchestration that wires Tasks 1–3 + PKCE + sink together. Cannot be e2e-tested headlessly (needs a real browser), so it is a follow-up epic.

---

### Task 1: Parse Nango authorization-code provider fields

**Task ID:** `task-1-nango-authcode-fields`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/nango.rs` (struct `ProviderEntry` lines 6-13; add test in `#[cfg(test)] mod tests`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] A provider YAML with `authorization_url`, `authorization_params`, and `scope_separator` parses into the corresponding `Option` fields on `ProviderEntry`.
- [ ] Providers WITHOUT those fields still parse (existing `parse_providers` tests and the `SAMPLE` constant remain green) — the fields are `Option`/`#[serde(default)]`.
- [ ] `cargo test -p spur-rest-table-gateway` passes.
- [ ] No compilation errors or warnings.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `nango.rs` only — the `ProviderEntry` struct and one new test.
- OUT of scope: `provider_to_manifest_stub`, `auth_cfg`, `auth_to_toml`, `manifest.rs`, `oauth.rs`, the providers snapshot YAML. The new fields are parsed-only; threading them into a manifest/wizard is a deferred task.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Write the failing test** — add to `#[cfg(test)] mod tests` in `nango.rs`:

```rust
#[test]
fn parses_authorization_code_fields() {
    const Y: &str = r#"
acme:
  display_name: Acme
  auth_mode: OAUTH2
  authorization_url: "https://acme.test/oauth/authorize"
  token_url: "https://acme.test/oauth/token"
  authorization_params:
    response_type: code
    prompt: consent
  scope_separator: " "
  proxy:
    base_url: "https://api.acme.test"
"#;
    let providers = parse_providers(Y).expect("providers yaml should parse");
    let p = &providers["acme"];
    assert_eq!(
        p.authorization_url.as_deref(),
        Some("https://acme.test/oauth/authorize")
    );
    assert_eq!(p.scope_separator.as_deref(), Some(" "));
    let params = p
        .authorization_params
        .as_ref()
        .expect("authorization_params should be present");
    assert_eq!(params.get("response_type").map(String::as_str), Some("code"));
    assert_eq!(params.get("prompt").map(String::as_str), Some("consent"));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p spur-rest-table-gateway parses_authorization_code_fields -- --nocapture`
Expected: FAIL — `no field authorization_url on type &ProviderEntry` (compile error).

- [ ] **Step 3: Add the three optional fields to `ProviderEntry`**

Replace the struct (currently lines 6-13) with:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderEntry {
    pub display_name: Option<String>,
    pub categories: Option<Vec<String>>,
    pub auth_mode: Option<String>,
    pub token_url: Option<String>,
    // Authorization-code (Approach C) provider fields. Parsed now; consumed by the
    // deferred "Connect with browser" wizard task. Optional so non-OAuth providers
    // (and the SAMPLE / OAUTH2_CC / TWO_STEP entries) keep parsing unchanged.
    pub authorization_url: Option<String>,
    #[serde(default)]
    pub authorization_params: Option<IndexMap<String, String>>,
    pub scope_separator: Option<String>,
    pub proxy: Option<Proxy>,
}
```

(`IndexMap` is already imported at the top of `nango.rs` via `use indexmap::IndexMap;`.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p spur-rest-table-gateway parses_authorization_code_fields -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Run the full crate test suite (no regressions)**

Run: `cargo test -p spur-rest-table-gateway -- --nocapture`
Expected: PASS — all existing tests (`parse_providers`-based, `SAMPLE`-based) still green. No new warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/nango.rs
git commit -m "feat(rest-table-gateway): parse Nango authorization-code provider fields"
```

---

### Task 2: `oauth::exchange_code` — the authorization_code grant

**Task ID:** `task-2-exchange-code`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/oauth.rs` (struct `TokenResponse` lines 31-36; add `AuthCodeGrant`, `TokenSet`, `exchange_code`; add tests in `#[cfg(test)] mod tests`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `exchange_code` POSTs `grant_type=authorization_code` with `code`, `code_verifier`, `client_id`, `client_secret`, `redirect_uri` and returns `{ access_token, refresh_token }` on success.
- [ ] A 2xx response that omits `refresh_token` yields `GatewayError::Auth` (never a silent partial success — acquiring the refresh token is the whole point).
- [ ] A non-2xx response yields `GatewayError::Auth`.
- [ ] The shipped `access_token` refresh-grant path and its two tests are unchanged and still green.
- [ ] `cargo test -p spur-rest-table-gateway` passes with no warnings.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `oauth.rs` only — `TokenResponse` (add one field), new `AuthCodeGrant`/`TokenSet`/`exchange_code`, new tests.
- OUT of scope: `access_token`, `RefreshGrant`, the cache, `manifest_adapter.rs`, `http.rs`, the wizard. Do NOT cache the exchange result (it is one-shot setup-time).
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Write the failing tests** — add to `#[cfg(test)] mod tests` in `oauth.rs` (the module already imports `wiremock::matchers::{body_string_contains, method, path}` and `wiremock::{Mock, MockServer, ResponseTemplate}`):

```rust
#[tokio::test]
async fn exchange_code_returns_access_and_refresh() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code_verifier=verifier-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "at-1",
            "refresh_token": "rt-1",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    let url = format!("{}/token", server.uri());
    let grant = AuthCodeGrant {
        token_url: &url,
        client_id: "cid",
        client_secret: "csec",
        code: "auth-code-xyz",
        code_verifier: "verifier-123",
        redirect_uri: "http://127.0.0.1:0/callback",
    };
    let set = exchange_code(&reqwest::Client::new(), &grant)
        .await
        .expect("exchange should succeed");
    assert_eq!(set.access_token, "at-1");
    assert_eq!(set.refresh_token, "rt-1");
}

#[tokio::test]
async fn exchange_code_missing_refresh_token_is_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "at-only"
        })))
        .mount(&server)
        .await;

    let url = format!("{}/token", server.uri());
    let grant = AuthCodeGrant {
        token_url: &url,
        client_id: "cid",
        client_secret: "csec",
        code: "c",
        code_verifier: "v",
        redirect_uri: "http://127.0.0.1:0/callback",
    };
    let err = exchange_code(&reqwest::Client::new(), &grant)
        .await
        .expect_err("missing refresh_token should error");
    assert!(matches!(err, GatewayError::Auth(_)));
}

#[tokio::test]
async fn exchange_code_non_2xx_is_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;

    let url = format!("{}/token", server.uri());
    let grant = AuthCodeGrant {
        token_url: &url,
        client_id: "cid",
        client_secret: "csec",
        code: "c",
        code_verifier: "v",
        redirect_uri: "http://127.0.0.1:0/callback",
    };
    let err = exchange_code(&reqwest::Client::new(), &grant)
        .await
        .expect_err("non-2xx should error");
    assert!(matches!(err, GatewayError::Auth(_)));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p spur-rest-table-gateway exchange_code -- --nocapture`
Expected: FAIL — `cannot find type AuthCodeGrant` / `cannot find function exchange_code` (compile errors).

- [ ] **Step 3: Add a `refresh_token` field to `TokenResponse`**

Replace the struct (currently lines 31-36):

```rust
#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}
```

(The existing `access_token` refresh path reads only `access_token`/`expires_in`; the new `#[serde(default)]` field is inert for it.)

- [ ] **Step 4: Add `AuthCodeGrant`, `TokenSet`, and `exchange_code`**

Place directly after the `access_token` function (before the `invalidate` fn). The `#[allow(dead_code)]` mirrors the existing `invalidate` precedent — these are wired by the deferred wizard task:

```rust
/// One-time authorization-code exchange — Approach C's *acquire* step.
/// `code_verifier` is supplied by the caller (PKCE generation is a separate,
/// deferred concern); this function only performs the token POST.
#[allow(dead_code)]
pub struct AuthCodeGrant<'a> {
    pub token_url: &'a str,
    pub client_id: &'a str,
    pub client_secret: &'a str,
    pub code: &'a str,
    pub code_verifier: &'a str,
    pub redirect_uri: &'a str,
}

/// Tokens returned by a successful authorization-code exchange.
#[allow(dead_code)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
}

/// Exchange an authorization `code` for tokens via `grant_type=authorization_code`.
/// Unlike `access_token`, this is a one-shot setup-time call: it is NOT cached, and it
/// REQUIRES a `refresh_token` in the response (acquiring that token is the point of C).
#[allow(dead_code)]
pub async fn exchange_code(
    client: &reqwest::Client,
    grant: &AuthCodeGrant<'_>,
) -> Result<TokenSet> {
    let form = vec![
        ("grant_type", "authorization_code"),
        ("code", grant.code),
        ("code_verifier", grant.code_verifier),
        ("client_id", grant.client_id),
        ("client_secret", grant.client_secret),
        ("redirect_uri", grant.redirect_uri),
    ];

    let resp = client
        .post(grant.token_url)
        .form(&form)
        .send()
        .await
        .map_err(|e| GatewayError::Auth(format!("code exchange request failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(GatewayError::Auth(format!(
            "code exchange returned status {}",
            resp.status()
        )));
    }
    let body: TokenResponse = resp
        .json()
        .await
        .map_err(|e| GatewayError::Auth(format!("code exchange response parse failed: {e}")))?;
    let refresh_token = body.refresh_token.ok_or_else(|| {
        GatewayError::Auth(
            "authorization_code exchange did not return a refresh_token".to_string(),
        )
    })?;
    Ok(TokenSet {
        access_token: body.access_token,
        refresh_token,
    })
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p spur-rest-table-gateway exchange_code -- --nocapture`
Expected: PASS (all three new tests green).

- [ ] **Step 6: Run the full crate test suite (no regressions)**

Run: `cargo test -p spur-rest-table-gateway -- --nocapture`
Expected: PASS — `miss_exchanges_then_hit_is_cached` and `non_2xx_is_auth_error` (the refresh-grant tests) still green. No warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/oauth.rs
git commit -m "feat(rest-table-gateway): add oauth authorization_code exchange (Approach C acquire)"
```

---

### Task 3: `oauth` loopback redirect listener

**Task ID:** `task-3-loopback-listener`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/oauth.rs` (add `use` imports, `Callback`, `bind_loopback`, `await_callback`, `percent_decode`; add tests)

**Depends on:** `task-2-exchange-code` (same file — serializes the `oauth.rs` edit to avoid a merge collision)

**Acceptance Criteria:**
- [ ] `bind_loopback` binds an ephemeral `127.0.0.1:0` port and returns a `http://127.0.0.1:<port>/callback` redirect URI plus the bound listener.
- [ ] `await_callback` serves exactly one request, extracts `code` and `state` from the query string (percent-decoded), writes a "close this tab" 200, and returns them.
- [ ] A request missing `code` or `state` yields `GatewayError::Auth`.
- [ ] `percent_decode` correctly handles `%XX` escapes and `+`.
- [ ] `cargo test -p spur-rest-table-gateway` passes with no warnings; no new dependencies added to `Cargo.toml`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `oauth.rs` only — new listener fns, the `Callback` struct, the `percent_decode` helper, and their tests.
- OUT of scope: `Cargo.toml` (tokio `full` already provides `net`; add nothing), `exchange_code`/`access_token`, the wizard. Do NOT wire the listener to `exchange_code` yet — that orchestration is the deferred wizard task.
- If you discover you need to touch OUT-OF-SCOPE files (especially `Cargo.toml`), emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Write the failing tests** — add to `#[cfg(test)] mod tests` in `oauth.rs`:

```rust
#[tokio::test]
async fn loopback_captures_code_and_state() {
    let (redirect_uri, listener) = bind_loopback().await.expect("bind loopback");
    assert!(redirect_uri.starts_with("http://127.0.0.1:"));
    assert!(redirect_uri.ends_with("/callback"));

    let handle = tokio::spawn(async move { await_callback(listener).await });

    let client = reqwest::Client::new();
    let _ = client
        .get(format!("{redirect_uri}?code=abc%2F123&state=xyz789"))
        .send()
        .await
        .expect("redirect request should reach the listener");

    let cb = handle.await.expect("join").expect("callback parsed");
    assert_eq!(cb.code, "abc/123");
    assert_eq!(cb.state, "xyz789");
}

#[test]
fn percent_decode_handles_escapes_and_plus() {
    assert_eq!(percent_decode("abc%2F123"), "abc/123");
    assert_eq!(percent_decode("a+b"), "a b");
    assert_eq!(percent_decode("plain"), "plain");
    assert_eq!(percent_decode("%41%42"), "AB");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p spur-rest-table-gateway loopback -- --nocapture`
Expected: FAIL — `cannot find function bind_loopback` / `percent_decode` (compile errors).

- [ ] **Step 3: Add the tokio io/net imports**

At the top of `oauth.rs`, below the existing `use` block (after line 5), add:

```rust
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
```

- [ ] **Step 4: Add the listener, the `Callback` struct, and `percent_decode`**

Place after `exchange_code` (from Task 2). All `#[allow(dead_code)]` until the deferred wizard wires them:

```rust
/// The authorization `code` + `state` captured from the OAuth redirect.
#[allow(dead_code)]
pub struct Callback {
    pub code: String,
    pub state: String,
}

/// Bind an ephemeral loopback listener for the OAuth redirect. Returns the
/// `redirect_uri` to hand the provider plus the bound listener to await on.
#[allow(dead_code)]
pub async fn bind_loopback() -> Result<(String, TcpListener)> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| GatewayError::Auth(format!("loopback bind failed: {e}")))?;
    let addr: SocketAddr = listener
        .local_addr()
        .map_err(|e| GatewayError::Auth(format!("loopback local_addr failed: {e}")))?;
    let redirect_uri = format!("http://127.0.0.1:{}/callback", addr.port());
    Ok((redirect_uri, listener))
}

/// Await exactly one redirect request, parse `code`/`state` from the query string,
/// reply with a minimal close-tab page, and return the captured values. The listener
/// is consumed (dropped) after one request.
#[allow(dead_code)]
pub async fn await_callback(listener: TcpListener) -> Result<Callback> {
    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|e| GatewayError::Auth(format!("loopback accept failed: {e}")))?;

    let mut buf = vec![0u8; 4096];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| GatewayError::Auth(format!("loopback read failed: {e}")))?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Request line: "GET /callback?code=...&state=... HTTP/1.1"
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| GatewayError::Auth("loopback: malformed request line".to_string()))?;
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");

    let mut code = None;
    let mut state = None;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            match k {
                "code" => code = Some(percent_decode(v)),
                "state" => state = Some(percent_decode(v)),
                _ => {}
            }
        }
    }

    let body = "<html><body>You can close this tab and return to SPUR.</body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;

    match (code, state) {
        (Some(code), Some(state)) => Ok(Callback { code, state }),
        _ => Err(GatewayError::Auth(
            "loopback callback missing code or state".to_string(),
        )),
    }
}

/// Minimal `application/x-www-form-urlencoded` percent-decoding for query values.
#[allow(dead_code)]
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p spur-rest-table-gateway loopback percent_decode -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Run the full crate test suite (no regressions, no warnings)**

Run: `cargo test -p spur-rest-table-gateway -- --nocapture`
Expected: PASS — every prior test still green; no `dead_code`/unused-import warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/oauth.rs
git commit -m "feat(rest-table-gateway): add oauth loopback redirect listener (Approach C)"
```

---

## Self-Review

**1. Spec coverage (against the notebook §3 module DAG + §10 build sequence):**
- Spec T1 (parse fields + `TokenResponse.refresh_token`) → split across Task 1 (parse) and Task 2 (`refresh_token`). ✓
- Spec T3 (`exchange_code`) → Task 2. ✓
- Spec T2 loopback half → Task 3. ✓
- Spec T2 PKCE half, T4 sink, T5 wizard → **explicitly deferred** with documented reasons (dep decision / threat review / non-headless). ✓ No silent gaps.

**2. Placeholder scan:** No `TBD`/`handle edge cases`/"similar to Task N" — every code step is literal and complete. ✓

**3. Type consistency:** `AuthCodeGrant`/`TokenSet`/`exchange_code` defined in Task 2 are only referenced by Task 2's tests. `Callback`/`bind_loopback`/`await_callback`/`percent_decode` defined and used within Task 3. `TokenResponse.refresh_token` (Task 2) is read only inside `exchange_code` (Task 2). No forward references across tasks. ✓

**4. DAG validation:** `{1, 2}` roots, `3 → 2`. Acyclic. Task 1 ∥ Task 2 maximizes parallelism; Task 3's dependency is the minimal serialization needed for the shared `oauth.rs` file. ✓

**5. beads compatibility:** Each task has a unique ID, an explicit `depends_on`, brain-verifiable acceptance criteria (specific `cargo test` invocations), and a scope boundary naming the OUT-of-scope files + a `scope_drift` trigger. ✓

## Verification gate (brain, at merge)
Per the Approach-B lesson (per-crate tests passed but a cross-crate `E0004` only surfaced at the parent crate), after all three tasks are approved run:

```bash
cargo test -p spur-rest-table-gateway
cargo check -p spur-notebook
```

Both must be green before `merge_plan`. (This plan adds no new `AuthCfg` variant, so the specific E0004 trap should not recur — but confirm regardless.)
