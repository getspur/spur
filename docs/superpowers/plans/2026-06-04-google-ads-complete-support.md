# Complete Google Ads Support Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** Graph-validated gap audit (2026-06-04). Four gaps confirmed via `code_*` + spur-analyst blast-radius (artifact `465fa71a…`). See chat audit: no GAds preset, no `login-customer-id`, static bearer instead of `oauth2_refresh`, and action path is single-shot (no GAQL `nextPageToken` pagination).

**Design epic:** n/a (follow-up to the write-actions + static-headers features already on main).

**Goal:** Ship real Google Ads support: a usable connection preset wired to `oauth2_refresh` with both required headers (`developer-token` + `login-customer-id`), and cursor pagination on the action path so GAQL `search` returns all pages.

**Architecture:** Two tasks. Task 1 is config-only (Gaps 1-file/2/3 collapse into authoring one preset + tests) — all targets are zero-churn leaves, no engine code. Task 2 is the one engine change (Gap 4): action-path cursor pagination, mirroring the existing read-path `fetch_cursor_rows` loop over `send_request`. The graph proved the action path and read path share **no** HTTP primitive (`send_request` is called only by `act`; the read path uses `get_page`), so Task 2 cannot regress reads. Task 2 depends on Task 1 because both edit `manifest_adapter.rs` (serialize to avoid clobber).

**Out of this plan (deferred):** Nango provider-catalog surfacing for the UI picker (`call_list_api_providers` → `ListNangoProviders`). The graph flagged `call_list_api_providers` as the only actively-churning symbol in the set (self_churn_90d=2) — touching it now risks merge collisions. Revisit once it settles.

**Tech Stack:** Rust, tokio, wiremock, serde/toml (existing deps).

---

### Task 1: Google Ads connection preset (oauth2_refresh + both headers)

**Task ID:** `task-1`

**Files:**
- Create: `crates/spur-notebook/rest-table-gateway/connections/tier-a/google_ads.connection.toml`
- Test: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs` (`#[cfg(test)] mod tests`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] A shipped `google_ads.connection.toml` preset exists with `auth = oauth2_refresh`, `[source.headers]` carrying both `developer-token` and `login-customer-id`, and a `google_ads_search` POST action.
- [ ] A structural test parses the shipped preset and asserts those fields (so the artifact can't silently drift).
- [ ] A runtime wiremock test proves: `oauth2_refresh` fetches a bearer from the token endpoint, and the GAQL POST carries `authorization: Bearer …` + `developer-token` + `login-customer-id` together.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway` is green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the new preset `.toml` + two new tests in `manifest_adapter.rs`.
- OUT of scope: `manifest.rs`, `http.rs`, `oauth.rs`, the ext crate, `mcp/mod.rs`, Nango. Do NOT add new config fields (oauth2_refresh + headers already exist). Do NOT add pagination here (that is Task 2).
- If you must touch any OUT-OF-SCOPE file, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Create the preset** at `crates/spur-notebook/rest-table-gateway/connections/tier-a/google_ads.connection.toml`:

```toml
# Google Ads (GAQL search). Requires a Google Ads developer token, an OAuth2
# client (client_id/secret) and a refresh token, plus the customer id you query.
# login-customer-id is required for manager (MCC) accounts.
[source]
name = "google_ads"
base_url = "https://googleads.googleapis.com/v17"
allow_writes = true
connection_config = ["DEVELOPER_TOKEN", "LOGIN_CUSTOMER_ID"]
auth = { scheme = "oauth2_refresh", token_url = "https://oauth2.googleapis.com/token", client_id_env = "GOOGLE_ADS_CLIENT_ID", client_secret_env = "GOOGLE_ADS_CLIENT_SECRET", refresh_token_env = "GOOGLE_ADS_REFRESH_TOKEN" }

[source.headers]
developer-token = "${connectionConfig.DEVELOPER_TOKEN}"
login-customer-id = "${connectionConfig.LOGIN_CUSTOMER_ID}"

[[action]]
name = "google_ads_search"
method = "POST"
path = "/customers/{customer_id}/googleAds:search"
response_path = "$.results"

[action.args]
customer_id = { in = "path", type = "Utf8", required = true }
query = { in = "body", type = "Utf8", required = true }
```

- [ ] **Step 2: Structural test** — append to `mod tests` in `manifest_adapter.rs` (validates the shipped artifact deserializes and has the required shape):

```rust
    #[test]
    fn google_ads_preset_has_refresh_auth_and_required_headers() {
        let toml = include_str!(
            "../../connections/tier-a/google_ads.connection.toml"
        );
        let manifest = Manifest::from_toml(toml).expect("preset parses");
        // Both Google Ads headers are present.
        assert_eq!(
            manifest.source.headers.get("developer-token").map(String::as_str),
            Some("${connectionConfig.DEVELOPER_TOKEN}")
        );
        assert_eq!(
            manifest.source.headers.get("login-customer-id").map(String::as_str),
            Some("${connectionConfig.LOGIN_CUSTOMER_ID}")
        );
        // Auth is oauth2_refresh (not a static bearer).
        assert!(matches!(
            manifest.source.auth,
            crate::adapter::manifest::AuthCfg::Oauth2Refresh { .. }
        ));
        // The search action exists and is a POST.
        let action = manifest
            .actions
            .iter()
            .find(|a| a.name == "google_ads_search")
            .expect("search action present");
        assert_eq!(action.method, "POST");
        assert!(manifest.source.allow_writes);
    }
```

- [ ] **Step 3: Runtime test** — append to `mod tests`. Mirrors the preset shape but points `base_url`/`token_url` at a wiremock server, proving the full oauth2_refresh + dual-header request. Use unique env var names and remove them at the end:

```rust
    #[tokio::test]
    async fn google_ads_action_uses_refresh_bearer_and_both_headers() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // 1) token endpoint exchanges the refresh token for an access token.
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "ya29-test-token",
                "expires_in": 3600
            })))
            .mount(&server)
            .await;
        // 2) GAQL search requires Bearer + developer-token + login-customer-id.
        Mock::given(method("POST"))
            .and(path("/customers/123/googleAds:search"))
            .and(header("authorization", "Bearer ya29-test-token"))
            .and(header("developer-token", "dev-tok-1"))
            .and(header("login-customer-id", "987654321"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [{ "resourceName": "customers/123/campaigns/1" }]
            })))
            .mount(&server)
            .await;

        std::env::set_var("SPUR_CONN_DEVELOPER_TOKEN", "dev-tok-1");
        std::env::set_var("SPUR_CONN_LOGIN_CUSTOMER_ID", "987654321");
        std::env::set_var("GADS_TEST_CLIENT_ID", "cid");
        std::env::set_var("GADS_TEST_CLIENT_SECRET", "secret");
        std::env::set_var("GADS_TEST_REFRESH_TOKEN", "rt-1");

        let manifest = Manifest::from_toml(&format!(
            r#"
[source]
name = "google_ads"
base_url = "{base}"
allow_writes = true
connection_config = ["DEVELOPER_TOKEN", "LOGIN_CUSTOMER_ID"]
auth = {{ scheme = "oauth2_refresh", token_url = "{base}/token", client_id_env = "GADS_TEST_CLIENT_ID", client_secret_env = "GADS_TEST_CLIENT_SECRET", refresh_token_env = "GADS_TEST_REFRESH_TOKEN" }}

[source.headers]
developer-token = "${{connectionConfig.DEVELOPER_TOKEN}}"
login-customer-id = "${{connectionConfig.LOGIN_CUSTOMER_ID}}"

[[action]]
name = "google_ads_search"
method = "POST"
path = "/customers/{{customer_id}}/googleAds:search"
response_path = "$.results"

[action.args]
customer_id = {{ in = "path", type = "Utf8", required = true }}
query = {{ in = "body", type = "Utf8", required = true }}
"#,
            base = server.uri()
        ))
        .expect("manifest parses");

        let adapter = ManifestAdapter::new(manifest);
        let req = ActionRequest {
            name: "google_ads_search".to_string(),
            method: "POST".to_string(),
            path: "/customers/123/googleAds:search".to_string(),
            query: vec![],
            body: Some(serde_json::json!({
                "query": "SELECT campaign.id FROM campaign"
            })),
            idempotency_key: None,
            dry_run: false,
        };
        let batches = adapter.act(req).await.expect("gaql search succeeds");

        std::env::remove_var("SPUR_CONN_DEVELOPER_TOKEN");
        std::env::remove_var("SPUR_CONN_LOGIN_CUSTOMER_ID");
        std::env::remove_var("GADS_TEST_CLIENT_ID");
        std::env::remove_var("GADS_TEST_CLIENT_SECRET");
        std::env::remove_var("GADS_TEST_REFRESH_TOKEN");
        assert_eq!(batches.len(), 1);
    }
```

- [ ] **Step 4: Run tests**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway -- --nocapture`
Expected: both new tests pass; full crate suite green. (If `act()` returns a generic single row for a column-less action, the test asserts `batches.len() == 1`, which holds for the single-result payload.)

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/connections/tier-a/google_ads.connection.toml crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs
git commit -m "feat(rest-table-gateway): ship Google Ads preset (oauth2_refresh + headers)"
```

---

### Task 2: action-path cursor pagination (GAQL nextPageToken)

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest.rs` (`ActionCfg` — add optional pagination)
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs` (`act()` — loop when paginated; + test)
- Modify: `crates/spur-notebook/rest-table-gateway/connections/tier-a/google_ads.connection.toml` (declare the action's pagination)

**Depends on:** task-1

**Acceptance Criteria:**
- [ ] An action can declare cursor pagination; when set, `act()` loops, injecting the cursor into the request body and accumulating rows across pages until the response cursor is empty/absent.
- [ ] A wiremock test serves two pages (page 1 → `nextPageToken`, page 2 → none) and asserts ALL rows from both pages are returned and the second request carried the page token.
- [ ] Single-shot behavior is unchanged when no pagination is declared (existing action tests stay green).
- [ ] The GAds preset's `google_ads_search` action declares `pagination` (cursor_path `$.nextPageToken`, body param `pageToken`).
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway` is green.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `ActionCfg` pagination field (manifest.rs), the `act()` pagination loop + test (manifest_adapter.rs), and the preset's action pagination block.
- OUT of scope: `http.rs` (reuse the existing `pub` `send_request` and `pub(crate)` `cursor_value` — do NOT modify them), the read path (`scan`/`get_page`/`fetch_cursor_rows` — leave untouched), the ext crate, `mcp/mod.rs`. Do NOT change `send_request`'s signature.
- If you must touch any OUT-OF-SCOPE file, emit `scope_drift` immediately.

**Reference (existing read-path cursor loop to mirror):** `fetch_cursor_rows` at `http.rs:238` — loops `get_page`, extracts the next cursor via `cursor_value(&page.body, cursor_path)`, breaks when empty/unchanged. Task 2 mirrors this over `send_request` for the action path, injecting the cursor into the request **body** (GAQL `pageToken`) rather than the query string.

**Implementation:**

- [ ] **Step 1: Add the config struct + field** in `manifest.rs`. Add near `ActionCfg`:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct ActionPaginationCfg {
    /// JSON path to the next-page cursor in the response body, e.g. "$.nextPageToken".
    pub cursor_path: String,
    /// Request body field the cursor is written into on the next request, e.g. "pageToken".
    pub cursor_param: String,
}
```
and add to `ActionCfg` (after `columns`):
```rust
    #[serde(default)]
    pub pagination: Option<ActionPaginationCfg>,
```

- [ ] **Step 2: Write the failing test** — append to `mod tests` in `manifest_adapter.rs`. Two pages via wiremock `expect`/`respond` on the `pageToken` body field:

```rust
    #[tokio::test]
    async fn action_paginates_with_cursor_until_exhausted() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // Page 2: request carries the page token; response has no further cursor.
        Mock::given(method("POST"))
            .and(path("/customers/1/googleAds:search"))
            .and(body_partial_json(serde_json::json!({ "pageToken": "tok-2" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [{ "id": "b" }]
            })))
            .mount(&server)
            .await;
        // Page 1: no page token in the body; response returns nextPageToken.
        Mock::given(method("POST"))
            .and(path("/customers/1/googleAds:search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [{ "id": "a" }],
                "nextPageToken": "tok-2"
            })))
            .mount(&server)
            .await;

        let manifest = Manifest::from_toml(&format!(
            r#"
[source]
name = "g"
base_url = "{base}"
allow_writes = true

[[action]]
name = "search"
method = "POST"
path = "/customers/1/googleAds:search"
response_path = "$.results"
pagination = {{ cursor_path = "$.nextPageToken", cursor_param = "pageToken" }}

[action.args]
query = {{ in = "body", type = "Utf8", required = true }}

[action.columns]
id = {{ json = "$.id", type = "Utf8" }}
"#,
            base = server.uri()
        ))
        .expect("manifest parses");

        let adapter = ManifestAdapter::new(manifest);
        let req = ActionRequest {
            name: "search".to_string(),
            method: "POST".to_string(),
            path: "/customers/1/googleAds:search".to_string(),
            query: vec![],
            body: Some(serde_json::json!({ "query": "SELECT x" })),
            idempotency_key: None,
            dry_run: false,
        };
        let batches = adapter.act(req).await.expect("paginated action succeeds");
        let total: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 2, "rows from both pages accumulate");
    }
```

Run it and confirm it FAILS (today `act()` is single-shot: only page 1's row returns → total == 1).

- [ ] **Step 3: Implement the loop in `act()`** (`manifest_adapter.rs:429`). After resolving `auth`/`idempotency_key`/headers and building the per-request inputs, branch on `action.pagination`. Reuse the existing `pub` `send_request` and `pub(crate)` `cursor_value` (already imported paths: `crate::adapter::http::{send_request, cursor_value}`). Sketch — accumulate rows across pages, injecting the cursor into a clone of the body each iteration:

```rust
        // (existing) build `auth`, `idempotency_key`, headers, base `url`, `query`, `body`.
        let pages = match &action.pagination {
            None => {
                let http_action = HttpAction { /* existing single-shot fields */ };
                vec![send_request(&http_action).await?]
            }
            Some(pg) => {
                let mut out = Vec::new();
                let mut next: Option<String> = None;
                loop {
                    let mut page_body = body.clone().unwrap_or_else(|| serde_json::json!({}));
                    if let Some(token) = &next {
                        page_body
                            .as_object_mut()
                            .ok_or_else(|| GatewayError::Adapter(
                                "paginated action body must be a JSON object".to_string(),
                            ))?
                            .insert(pg.cursor_param.clone(), serde_json::json!(token));
                    }
                    let http_action = HttpAction {
                        client: &self.client,
                        method: reqwest::Method::from_bytes(method.as_bytes())
                            .map_err(|e| GatewayError::Http(e.to_string()))?,
                        url: url.clone(),
                        query: query.clone(),
                        body: Some(page_body),
                        auth: &auth,
                        idempotency_key: idempotency_key.clone(),
                        headers: resolve_headers(&self.manifest.source.headers, &connection_ctx)?,
                    };
                    let (status, resp_body) = send_request(&http_action).await?;
                    let cursor = cursor_value(&resp_body, &pg.cursor_path);
                    out.push((status, resp_body));
                    match cursor {
                        Some(c) if !c.is_empty() && next.as_deref() != Some(c.as_str()) => {
                            next = Some(c);
                        }
                        _ => break,
                    }
                }
                out
            }
        };
```

Then fold `pages` into the existing row-rendering: for the `columns.is_some()` path, run `action_rows` over each page body and concatenate before `rows_to_batch`; for the column-less path, keep current generic-row behavior on the first/last page. Keep the existing single-page code path byte-for-byte when `pagination` is `None`. Adapt names to the actual locals in `act()` — do not blindly paste.

- [ ] **Step 4: Wire the preset** — add to the `[[action]]` block in `google_ads.connection.toml`:

```toml
pagination = { cursor_path = "$.nextPageToken", cursor_param = "pageToken" }
```
Update the Task-1 structural test only if needed (it does not assert pagination, so it should still pass).

- [ ] **Step 5: Run tests**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway -- --nocapture`
Expected: the new pagination test passes (total == 2), and all prior action tests (single-shot, no `pagination`) stay green.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/manifest.rs crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs crates/spur-notebook/rest-table-gateway/connections/tier-a/google_ads.connection.toml
git commit -m "feat(rest-table-gateway): action-path cursor pagination (GAQL nextPageToken)"
```

---

## Self-Review

- **Spec coverage:** Gap 1 (preset file) → Task 1; Gap 2 (login-customer-id) → Task 1 headers; Gap 3 (oauth2_refresh) → Task 1 auth + runtime test; Gap 4 (action pagination) → Task 2. Gap 1's UI provider-surfacing is explicitly deferred (Nango churn).
- **DAG:** task-1 (root) → task-2 (depends_on task-1). Valid, acyclic. Serialized intentionally — both edit `manifest_adapter.rs`, so parallel dispatch would clobber.
- **Type consistency:** `ActionPaginationCfg` defined in Task 2 Step 1; referenced by `ActionCfg.pagination` and the preset. `cursor_value`/`send_request` reused (no signature change). `AuthCfg::Oauth2Refresh` already exists.
- **No placeholders:** preset, both Task-1 tests, the Task-2 config + failing test are concrete; the `act()` loop is a labelled sketch the worker adapts to the real locals (genuine engine integration, not a copy-paste).
