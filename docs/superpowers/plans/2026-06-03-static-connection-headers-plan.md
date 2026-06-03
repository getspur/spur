# Static Connection Headers (`[source.headers]`) Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-03-static-connection-headers-google-ads-design.ipynb` (committed `2695cd77`)
**Design epic:** `bd-1irz` (approved)

**Goal:** Let a REST connection attach constant, templated headers (e.g. Google Ads `developer-token`) to every request **alongside** the existing OAuth2/auth header, by adding `[source.headers]` to the manifest and applying it on both the read and action request paths.

**Architecture:** Mirror the shipped auth pattern — *resolve in the adapter, apply in http*. `SourceCfg` gains a `headers` map (values support `${connectionConfig.*}` templating). A free `resolve_headers` helper (sibling to `resolve_auth`) templates the values; `ManifestAdapter::scan`/`act` thread the resolved `Vec<(String,String)>` into `HttpFetch`/`HttpAction`; `get_page`/`send_request` apply them right after `apply_auth`. **No enum, trait, or public-API change** — so the `AuthCfg` E0004 exhaustiveness trap cannot fire.

**Tech Stack:** Rust, serde/`toml`, `indexmap`, `reqwest`. Dev: `wiremock`. **No new dependencies.**

---

## File Structure Mapping

| File | Responsibility | Task |
|------|----------------|------|
| `crates/spur-notebook/rest-table-gateway/src/adapter/manifest.rs` | `SourceCfg.headers` field + parse tests | T1 |
| `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs` | `resolve_headers` helper + unit tests | T1 |
| `crates/spur-notebook/rest-table-gateway/src/adapter/http.rs` | `headers` field on `HttpFetch`/`HttpAction`; apply loop in `get_page`/`send_request` | T2 |
| `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs` | wire `scan`/`act`; wiremock tests | T2 |
| `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs` | Google Ads GAQL E2E (templated header + Bearer → rows) | T3 |

**Collision control:** All three tasks touch `manifest_adapter.rs`, so the DAG is a chain `T1 → T2 → T3`.

## Dependency DAG
```
T1 (manifest field + resolve_headers) → T2 (thread + apply + wiremock) → T3 (Google Ads E2E)
```
- T1 `depends_on: []`
- T2 `depends_on: [task-1-headers-config-and-resolve]`
- T3 `depends_on: [task-2-thread-and-apply]`

## Scope boundary — explicit
- **OUT (separate follow-ups):** GraphQL read path (`post_page`/`GraphqlFetch` is the 3rd `apply_auth` call site — apply static headers there for consistency in a later task); read-vs-write gating cleanup; GAQL `nextPageToken` pagination; per-table headers.

---

### Task 1: `[source.headers]` config + `resolve_headers` helper

**Task ID:** `task-1-headers-config-and-resolve`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest.rs` (`SourceCfg` struct ~lines 23-35; add parse tests in `#[cfg(test)] mod tests`)
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs` (add free fn `resolve_headers` near the other free helpers; add unit tests in `#[cfg(test)] mod tests`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `[source.headers]` parses into `SourceCfg.headers`; a manifest without it parses with an empty map (default).
- [ ] `resolve_headers` returns each header with its value run through `resolve_template`; a literal value (no `${…}`) is returned unchanged.
- [ ] A header named `authorization` (case-insensitive) yields `GatewayError::Manifest`.
- [ ] `cargo test -p spur-rest-table-gateway` passes; no warnings.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `manifest.rs` (`SourceCfg` + tests), `manifest_adapter.rs` (`resolve_headers` + tests).
- OUT of scope: `http.rs`, `HttpFetch`/`HttpAction`, `scan`/`act` wiring (that is T2). Do NOT yet call `resolve_headers` from `scan`/`act`.
- If you must touch out-of-scope files, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Write the failing tests** — add to `#[cfg(test)] mod tests` in `manifest.rs`:

```rust
#[test]
fn source_headers_parse() {
    let toml = r#"
[source]
name = "svc"
base_url = "https://x.test"
[source.headers]
developer-token = "${connectionConfig.developer_token}"
x-api-version = "v17"
"#;
    let m = Manifest::from_toml(toml).expect("manifest should parse");
    assert_eq!(m.source.headers.len(), 2);
    assert_eq!(
        m.source.headers.get("x-api-version").map(String::as_str),
        Some("v17")
    );
    assert_eq!(
        m.source.headers.get("developer-token").map(String::as_str),
        Some("${connectionConfig.developer_token}")
    );
}

#[test]
fn source_headers_default_empty() {
    let toml = r#"
[source]
name = "svc"
base_url = "https://x.test"
"#;
    let m = Manifest::from_toml(toml).expect("manifest should parse");
    assert!(m.source.headers.is_empty());
}
```

And add to `#[cfg(test)] mod tests` in `manifest_adapter.rs`:

```rust
#[test]
fn resolve_headers_returns_literals_and_rejects_authorization() {
    use indexmap::IndexMap;
    let ctx = ConnectionContext::from_env(&[]);

    let mut headers = IndexMap::new();
    headers.insert("x-api-version".to_string(), "v17".to_string());
    let out = resolve_headers(&headers, &ctx).expect("resolve");
    assert_eq!(out, vec![("x-api-version".to_string(), "v17".to_string())]);

    let mut bad = IndexMap::new();
    bad.insert("Authorization".to_string(), "Bearer x".to_string());
    assert!(matches!(
        resolve_headers(&bad, &ctx),
        Err(GatewayError::Manifest(_))
    ));
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p spur-rest-table-gateway source_headers resolve_headers -- --nocapture`
Expected: FAIL — `no field headers on SourceCfg` / `cannot find function resolve_headers` (compile errors).

- [ ] **Step 3: Add the `headers` field to `SourceCfg`** (`manifest.rs`, currently lines 23-35):

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct SourceCfg {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub transport: Transport,
    #[serde(default)]
    pub auth: AuthCfg,
    pub pagination: Option<PaginationCfg>,
    #[serde(default)]
    pub connection_config: Vec<String>,
    #[serde(default)]
    pub allow_writes: bool,
    #[serde(default)]
    pub headers: IndexMap<String, String>,
}
```

(`IndexMap` is already imported in `manifest.rs`.)

- [ ] **Step 4: Add the `resolve_headers` free function** in `manifest_adapter.rs`, placed near the top-level helpers (it is sync — no network). `IndexMap`, `ConnectionContext`, `resolve_template`, `GatewayError`, and `Result` are already imported in this file:

```rust
/// Template each static header value via `resolve_template`, returning name/value
/// pairs ready to attach to a request. `authorization` is reserved (it would shadow
/// the resolved auth header, which `reqwest` appends rather than replaces).
fn resolve_headers(
    headers: &IndexMap<String, String>,
    ctx: &ConnectionContext,
) -> Result<Vec<(String, String)>> {
    let mut out = Vec::with_capacity(headers.len());
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("authorization") {
            return Err(GatewayError::Manifest(
                "static header 'authorization' is reserved; use [source.auth]".to_string(),
            ));
        }
        out.push((name.clone(), resolve_template(value, ctx)?));
    }
    Ok(out)
}
```

> If `cargo` reports `resolve_headers` as unused at this stage (T2 adds the call sites), add `#[allow(dead_code)]` directly above it with a comment `// wired into scan/act in T2`. Remove the attribute in T2.

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p spur-rest-table-gateway source_headers resolve_headers -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Full suite + commit**

Run: `cargo test -p spur-rest-table-gateway`
Expected: PASS, no warnings.

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/manifest.rs crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs
git commit -m "feat(rest-table-gateway): add [source.headers] config + resolve_headers helper"
```

---

### Task 2: Thread headers into the request structs + apply after auth

**Task ID:** `task-2-thread-and-apply`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/http.rs` (`HttpFetch` ~lines 9-17; `HttpAction` ~lines 27-34; `get_page` ~lines 152-188; `send_request` ~lines 38-71; update any test literals)
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs` (`scan` ~lines 338-385; `act` ~lines 387-447; wiremock tests)

**Depends on:** `task-1-headers-config-and-resolve`

**Acceptance Criteria:**
- [ ] `HttpFetch` and `HttpAction` each carry `headers: Vec<(String,String)>`.
- [ ] `get_page` (read) and `send_request` (action) attach those headers **after** `apply_auth`.
- [ ] `scan` and `act` populate them via `resolve_headers(&self.manifest.source.headers, &connection_ctx)`.
- [ ] A read table with a static header sends it on the GET (wiremock asserts present).
- [ ] An action with `ResolvedAuth::Bearer` **and** a static header sends **both** headers on the POST (wiremock asserts both) and still returns rows.
- [ ] `cargo test -p spur-rest-table-gateway` passes; no warnings (remove any `#[allow(dead_code)]` left on `resolve_headers`).

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `http.rs` (the two structs + `get_page`/`send_request` + updating existing `HttpFetch`/`HttpAction` literals to include the new field), `manifest_adapter.rs` (`scan`/`act` wiring + new wiremock tests).
- OUT of scope: GraphQL (`post_page`/`GraphqlFetch`) — that is a deferred follow-up; the `AuthCfg` enum; `nango.rs`. Do NOT change auth behavior.
- If you must touch out-of-scope files, emit `scope_drift`.

**Scope Drift Checkpoint:** adding the `headers` field will break every `HttpFetch`/`HttpAction` literal that doesn't set it (compile errors `missing field headers`). Fix each by adding `headers: Vec::new()` (or `vec![]` in tests). If that surfaces more than ~6 literals, emit `scope_drift` (unexpected fan-out).

**Implementation:**

- [ ] **Step 1: Write the failing wiremock tests** — add to `#[cfg(test)] mod tests` in `manifest_adapter.rs`:

```rust
#[tokio::test]
async fn read_table_sends_static_header() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/items"))
        .and(wiremock::matchers::header("x-api-version", "v17"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_json(serde_json::json!([{ "id": "a" }])),
        )
        .mount(&server)
        .await;

    let toml = format!(
        r#"
[source]
name = "svc"
base_url = "{base}"
[source.headers]
x-api-version = "v17"
[[table]]
name = "items"
path = "/items"
[table.columns]
id = {{ json = "$.id", type = "Utf8" }}
"#,
        base = server.uri()
    );
    let adapter = ManifestAdapter::new(Manifest::from_toml(&toml).unwrap());
    let batches = adapter
        .scan(ScanRequest { table: "items".to_string(), predicates: vec![] })
        .await
        .unwrap();
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 1);
}

#[tokio::test]
async fn action_sends_bearer_and_static_header_together() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/orders"))
        .and(wiremock::matchers::header("authorization", "Bearer tok-1"))
        .and(wiremock::matchers::header("developer-token", "dev-123"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "order": { "id": "o1" } })),
        )
        .mount(&server)
        .await;

    let toml = format!(
        r#"
[source]
name = "svc"
base_url = "{base}"
allow_writes = true
[source.headers]
developer-token = "dev-123"
[[action]]
name = "create"
method = "POST"
path = "/orders"
response_path = "$.order"
[action.args]
price = {{ in = "body", type = "Float64", required = true }}
[action.columns]
id = {{ json = "$.id", type = "Utf8" }}
"#,
        base = server.uri()
    );
    let adapter = ManifestAdapter::new(Manifest::from_toml(&toml).unwrap());
    let req = ActionRequest {
        name: "create".to_string(),
        method: "POST".to_string(),
        path: "/orders".to_string(),
        query: vec![],
        body: Some(serde_json::json!({ "price": 0.5 })),
        auth: ResolvedAuth::Bearer("tok-1".to_string()),
        idempotency_key: None,
        dry_run: false,
    };
    let batches = adapter.act(req).await.unwrap();
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 1);
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p spur-rest-table-gateway static_header -- --nocapture`
Expected: FAIL — wiremock returns no match (header absent), so `scan`/`act` error or the mock is unmatched.

- [ ] **Step 3: Add the `headers` field to both request structs** (`http.rs`):

`HttpFetch` (lines 9-17) gains a final field:
```rust
pub struct HttpFetch<'a> {
    pub client: &'a Client,
    pub base_url: &'a str,
    pub path: &'a str,
    pub query: Vec<(String, String)>,
    pub pagination: Option<&'a PaginationCfg>,
    pub auth: &'a ResolvedAuth,
    pub response_path: Option<String>,
    pub headers: Vec<(String, String)>,
}
```
`HttpAction` (lines 27-34) gains a final field:
```rust
pub struct HttpAction<'a> {
    pub client: &'a Client,
    pub method: reqwest::Method,
    pub url: String,
    pub query: Vec<(String, String)>,
    pub body: Option<serde_json::Value>,
    pub auth: &'a ResolvedAuth,
    pub idempotency_key: Option<(String, String)>,
    pub headers: Vec<(String, String)>,
}
```

- [ ] **Step 4: Apply headers after `apply_auth`** in both builders (`http.rs`).

In `get_page`, replace `let req = apply_auth(req, f.auth);` with:
```rust
    let mut req = apply_auth(req, f.auth);
    for (name, value) in &f.headers {
        req = req.header(name.as_str(), value.as_str());
    }
```
In `send_request`, replace `let req = apply_auth(req, a.auth);` with:
```rust
    let mut req = apply_auth(req, a.auth);
    for (name, value) in &a.headers {
        req = req.header(name.as_str(), value.as_str());
    }
```

- [ ] **Step 5: Fix every other `HttpFetch`/`HttpAction` literal** so the crate compiles. The production sites are wired in Step 6; for all remaining literals (existing `http.rs` tests, etc.) add `headers: Vec::new(),` as the last field. Run `cargo build -p spur-rest-table-gateway 2>&1 | grep "missing field"` to enumerate them.

- [ ] **Step 6: Wire `scan` and `act`** (`manifest_adapter.rs`).

In `scan`, the `HttpFetch { … }` literal (where `connection_ctx` is already in scope) gains:
```rust
                    headers: resolve_headers(&self.manifest.source.headers, &connection_ctx)?,
```
In `act`, the `HttpAction { … }` literal (where `connection_ctx` is already in scope) gains:
```rust
            headers: resolve_headers(&self.manifest.source.headers, &connection_ctx)?,
```
Remove any `#[allow(dead_code)]` left on `resolve_headers` from T1.

- [ ] **Step 7: Run tests + full suite**

Run: `cargo test -p spur-rest-table-gateway -- --nocapture`
Expected: PASS — both new tests green; all prior tests (incl. `action_post_renders_typed_columns`) still green; no warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/http.rs crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs
git commit -m "feat(rest-table-gateway): apply static [source.headers] on read and action requests"
```

---

### Task 3: Google Ads GAQL end-to-end (templated developer-token + Bearer → rows)

**Task ID:** `task-3-google-ads-e2e`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs` (one `#[tokio::test]` in `#[cfg(test)] mod tests`)

**Depends on:** `task-2-thread-and-apply`

**Acceptance Criteria:**
- [ ] A Google-Ads-shaped manifest with `[source.headers] developer-token = "${connectionConfig.developer_token}"` resolves the token from `SPUR_CONN_DEVELOPER_TOKEN` and sends it together with the OAuth Bearer on a POST GAQL `:search`, with a `{query}` body, returning extracted rows.
- [ ] `cargo test -p spur-rest-table-gateway` passes; no warnings.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: one new test in `manifest_adapter.rs`. Sets/removes the `SPUR_CONN_DEVELOPER_TOKEN` env var within the test.
- OUT of scope: production code (T1/T2 already deliver it), the providers snapshot, `nango.rs`.
- If you must touch production code to make this pass, the feature is incomplete — emit `scope_drift` (it should already work end-to-end).

**Implementation:**

- [ ] **Step 1: Write the E2E test** — add to `#[cfg(test)] mod tests` in `manifest_adapter.rs`. It mirrors the templating style of the existing `base_url_templated` test (env-driven `${connectionConfig.*}`):

```rust
#[tokio::test]
async fn google_ads_gaql_sends_developer_token_and_bearer() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/customers/123/googleAds:search"))
        .and(wiremock::matchers::header("authorization", "Bearer ya29-test"))
        .and(wiremock::matchers::header("developer-token", "dev-tok-xyz"))
        .and(wiremock::matchers::body_string_contains("SELECT campaign.id"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "results": [
                { "campaign": { "id": "9482117003" }, "metrics": { "impressions": 128940 } },
                { "campaign": { "id": "9482117884" }, "metrics": { "impressions": 86552 } }
            ]
        })))
        .mount(&server)
        .await;

    std::env::set_var("SPUR_CONN_DEVELOPER_TOKEN", "dev-tok-xyz");

    let toml = format!(
        r#"
[source]
name = "google_ads"
base_url = "{base}"
allow_writes = true
connection_config = ["developer_token"]
[source.headers]
developer-token = "${{connectionConfig.developer_token}}"
[[action]]
name = "google_ads_search"
method = "POST"
path = "/customers/{{customer_id}}/googleAds:search"
response_path = "$.results"
[action.args]
customer_id = {{ in = "path", type = "Utf8", required = true }}
query       = {{ in = "body", type = "Utf8", required = true }}
[action.columns]
campaign_id = {{ json = "$.campaign.id", type = "Utf8" }}
impressions = {{ json = "$.metrics.impressions", type = "Int64" }}
"#,
        base = server.uri()
    );
    let adapter = ManifestAdapter::new(Manifest::from_toml(&toml).unwrap());

    let req = ActionRequest {
        name: "google_ads_search".to_string(),
        method: "POST".to_string(),
        path: "/customers/123/googleAds:search".to_string(),
        query: vec![],
        body: Some(serde_json::json!({ "query": "SELECT campaign.id, metrics.impressions FROM campaign" })),
        auth: ResolvedAuth::Bearer("ya29-test".to_string()),
        idempotency_key: None,
        dry_run: false,
    };
    let batches = adapter.act(req).await.unwrap();

    std::env::remove_var("SPUR_CONN_DEVELOPER_TOKEN");
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 2);
}
```

> Note: this test mutates a process-global env var. If the crate's test suite shows flakiness from parallel env access, gate it behind the same serialization the existing `${connectionConfig.*}` tests use (a shared `Mutex`/`ENV_LOCK`), matching `base_url_templated`.

- [ ] **Step 2: Run to verify it passes** (T1+T2 already deliver the behavior)

Run: `cargo test -p spur-rest-table-gateway google_ads_gaql -- --nocapture`
Expected: PASS — wiremock matched both headers + the GAQL body; 2 rows extracted.

- [ ] **Step 3: Full suite + commit**

Run: `cargo test -p spur-rest-table-gateway`
Expected: PASS, no warnings.

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs
git commit -m "test(rest-table-gateway): Google Ads GAQL e2e — developer-token + Bearer together"
```

---

## Self-Review

**1. Spec coverage:** §1–§4 integration (resolve→thread→apply) → T1+T2; `[source.headers]` parse → T1; reject `authorization` → T1; apply on read + action → T2; Bearer+developer-token together → T2 + T3; Google Ads worked example → T3. §10 OUT items (GraphQL, gating, pagination, per-table) are explicitly deferred. No spec requirement unmapped.

**2. Placeholder scan:** every code step is literal; no TBD/"handle errors"/"similar to". The `#[allow(dead_code)]` note in T1 is conditional and removed in T2.

**3. Type consistency:** `headers: Vec<(String,String)>` is identical on `HttpFetch`, `HttpAction`, and `resolve_headers`'s return. `resolve_headers(&IndexMap<String,String>, &ConnectionContext) -> Result<Vec<(String,String)>>` is used verbatim in `scan`/`act` (T2). `ScanRequest{table,predicates}` and `ActionRequest{…}` field shapes match the real structs read during planning.

**4. DAG validation:** `T1 → T2 → T3`, acyclic. The chain (not a wider fan-out) is required because all three edit `manifest_adapter.rs`; serializing avoids merge collisions.

**5. beads compatibility:** each task has a unique id, explicit `depends_on`, brain-verifiable acceptance criteria (specific `cargo test` invocations + wiremock header assertions), and a scope boundary with a `scope_drift` trigger.

## Verification gate (brain, at merge)
After all three approved:
```bash
cargo test -p spur-rest-table-gateway
cargo check -p spur-notebook
```
Both green before `merge_plan`. (No new `AuthCfg` variant → the E0004 trap is structurally avoided; confirm regardless.)
