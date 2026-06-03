# Nango Adapter — Query-Param Auth & Pagination Page-Size Fixes

> **For SPUR orchestrator:** This plan is designed for `submit_plan`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source:** code-explore of `crates/spur-notebook/rest-table-gateway/src/adapter/nango.rs` (graph-traced, 2026-06-03)
**Design epic:** none (bugfix pair, no spec)

**Goal:** Close two latent correctness gaps in the Nango → manifest translator so the runtime capabilities that already exist (query-param auth, offset/cursor pagination) are actually reachable from generated provider manifests.

**Architecture:** `provider_to_manifest_stub` (nango.rs) translates a parsed Nango `ProviderEntry` into a gateway `Manifest`. Two translation branches under-populate the manifest: (a) `auth_cfg` never emits `AuthCfg::ApiKeyQuery` even though `resolve_auth`/`apply_auth` fully support it; (b) `pagination_cfg` hardcodes `page_size = 0`, and `fetch_offset_rows` treats `page_size == 0` as "fetch one page only", silently disabling pagination for every Nango-imported source. Both fixes are localized to `nango.rs` (struct + one function each) plus unit tests.

**Tech Stack:** Rust, serde / serde_yaml, indexmap.

**Scope note — dropped Gap 4:** `${connectionConfig.*}` base-URL substitution is **already implemented** in `crates/spur-notebook/rest-table-gateway/src/adapter/templating.rs` (`resolve_template` + `ConnectionContext::from_env`, env prefix `SPUR_CONN_`), wired into `ManifestAdapter::scan`, and covered by `tests::base_url_templated` + `templating.rs` unit tests. No task is needed.

**Sequencing:** Both tasks edit `nango.rs` and its `#[cfg(test)] mod tests`. To avoid same-file merge collisions, Task 2 depends on Task 1.

---

### Task 1: Emit `AuthCfg::ApiKeyQuery` for query-param API keys (Gap 2)

**Task ID:** `task-queryparam-auth`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/nango.rs` (struct `Proxy` ~lines 14-19; fn `auth_cfg` ~lines 101-122; add helper near `api_key_header` ~line 124; add test in `mod tests` ~line 279+)

**Depends on:** none

**Acceptance Criteria:**
- [ ] A provider whose `proxy.query` contains a value with `${apiKey}` generates `AuthCfg::ApiKeyQuery { param, env }`.
- [ ] Header-based API keys still generate `AuthCfg::Header` (existing `api_key_maps_to_header` test still passes).
- [ ] `cargo test -p spur-rest-table-gateway` passes.
- [ ] No compilation errors / warnings.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `nango.rs` only (`Proxy`, `auth_cfg`, new `api_key_query` helper, new test).
- OUT of scope: `manifest.rs` (`AuthCfg::ApiKeyQuery` already exists), `manifest_adapter.rs`, `http.rs`, the providers snapshot YAML.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Write the failing test** — add to `mod tests` in `nango.rs`:

```rust
#[test]
fn api_key_in_query_maps_to_api_key_query() {
    const Q: &str = r#"
acme:
  display_name: Acme
  categories:
    - search
  auth_mode: API_KEY
  proxy:
    base_url: "https://api.acme.test"
    query:
      api_key: "${apiKey}"
"#;
    let providers = parse_providers(Q).expect("providers yaml should parse");
    let manifest = provider_to_manifest_stub("acme", &providers["acme"]);

    match manifest.source.auth {
        crate::adapter::manifest::AuthCfg::ApiKeyQuery { param, env } => {
            assert_eq!(param, "api_key");
            assert_eq!(env, "ACME_API_KEY");
        }
        other => panic!("expected api_key_query auth, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-rest-table-gateway api_key_in_query_maps_to_api_key_query -- --nocapture`
Expected: FAIL (currently emits `Bearer`, not `ApiKeyQuery`).

- [ ] **Step 3: Add a `query` field to `Proxy`**

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct Proxy {
    pub base_url: Option<String>,
    pub headers: Option<IndexMap<String, String>>,
    pub query: Option<IndexMap<String, String>>,
    pub paginate: Option<Paginate>,
}
```

- [ ] **Step 4: Add the `api_key_query` helper** (mirror of `api_key_header`, place directly after it):

```rust
fn api_key_query(p: &ProviderEntry) -> Option<(&str, &str)> {
    p.proxy
        .as_ref()?
        .query
        .as_ref()?
        .iter()
        .find(|(_, value)| value.contains("${apiKey}"))
        .map(|(name, value)| (name.as_str(), value.as_str()))
}
```

- [ ] **Step 5: Extend the `API_KEY` arm of `auth_cfg`** to check query after header:

```rust
        Some("API_KEY") => {
            let env = format!("{upper}_API_KEY");
            if let Some((header, _)) = api_key_header(p) {
                AuthCfg::Header {
                    name: header.to_string(),
                    env,
                }
            } else if let Some((param, _)) = api_key_query(p) {
                AuthCfg::ApiKeyQuery {
                    param: param.to_string(),
                    env,
                }
            } else {
                AuthCfg::Bearer { env }
            }
        }
```

- [ ] **Step 6: Run the full crate tests**

Run: `cargo test -p spur-rest-table-gateway -- --nocapture`
Expected: PASS (new test green; `api_key_maps_to_header`, `oauth_maps_to_bearer_byo`, `toml_roundtrips` still green). Note: `auth_to_toml` already serializes `ApiKeyQuery`, so no TOML change is needed.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/nango.rs
git commit -m "fix(rest-table-gateway): emit ApiKeyQuery auth for query-param Nango keys"
```

---

### Task 2: Map Nango paginate `limit` to a non-zero `page_size` (Gap 5)

**Task ID:** `task-pagination-pagesize`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/nango.rs` (struct `Paginate` ~lines 21-29; fn `pagination_cfg` ~lines 134-171; add tests in `mod tests`)

**Depends on:** `task-queryparam-auth`

**Acceptance Criteria:**
- [ ] When `paginate.limit` is present, the generated `PaginationCfg.page_size` equals that value.
- [ ] When `paginate.limit` is absent, `page_size` defaults to `100` (non-zero, so `fetch_offset_rows`/cursor paging actually engages instead of returning page 1).
- [ ] Existing `oauth_maps_to_bearer_byo` cursor assertions still pass.
- [ ] `cargo test -p spur-rest-table-gateway` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `nango.rs` only (`Paginate`, `pagination_cfg`, new tests).
- OUT of scope: `manifest.rs` (`PaginationCfg.page_size` already exists), `http.rs` paging loops, the providers snapshot YAML.
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Rationale (do not skip):** `fetch_offset_rows` in `http.rs` returns only the first page when `p.page_size == 0` (`if p.page_size == 0 { return Ok(get_page(...).rows) }`). Because `pagination_cfg` hardcodes `page_size: 0` in every branch, no Nango-imported source ever paginates.

**Implementation:**

- [ ] **Step 1: Write the failing tests** — add to `mod tests` in `nango.rs`:

```rust
#[test]
fn offset_pagination_uses_limit_as_page_size() {
    const Y: &str = r#"
acme:
  display_name: Acme
  categories:
    - data
  auth_mode: API_KEY
  proxy:
    base_url: "https://api.acme.test"
    headers:
      authorization: "Bearer ${apiKey}"
    paginate:
      type: offset
      limit_name_in_request: limit
      cursor_name_in_request: offset
      limit: 250
"#;
    let providers = parse_providers(Y).expect("providers yaml should parse");
    let manifest = provider_to_manifest_stub("acme", &providers["acme"]);
    let pagination = manifest.source.pagination.expect("pagination");

    assert_eq!(pagination.style, "offset");
    assert_eq!(pagination.page_size, 250);
    assert_eq!(pagination.limit_param.as_deref(), Some("limit"));
    assert_eq!(pagination.offset_param.as_deref(), Some("offset"));
}

#[test]
fn pagination_defaults_page_size_when_limit_absent() {
    // SAMPLE's salesforce has cursor pagination but no `limit`.
    let providers = parse_providers(SAMPLE).expect("providers yaml should parse");
    let manifest = provider_to_manifest_stub("salesforce", &providers["salesforce"]);
    let pagination = manifest.source.pagination.expect("pagination");

    assert_eq!(pagination.style, "cursor");
    assert_eq!(pagination.page_size, 100);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-rest-table-gateway pagination -- --nocapture`
Expected: FAIL (both assert non-zero `page_size`; current code yields `0`).

- [ ] **Step 3: Add a `limit` field to `Paginate`**

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct Paginate {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub cursor_path_in_response: Option<String>,
    pub cursor_name_in_request: Option<String>,
    pub response_path: Option<String>,
    pub limit_name_in_request: Option<String>,
    pub limit: Option<u32>,
}
```

- [ ] **Step 4: Compute a non-zero page size in `pagination_cfg`** — add a const at module scope (near the other free functions) and use it in the `cursor` and `offset` arms:

```rust
const DEFAULT_PAGE_SIZE: u32 = 100;
```

In `pagination_cfg`, replace `page_size: 0` with `page_size: p.limit.unwrap_or(DEFAULT_PAGE_SIZE)` in the `"cursor"` and `"offset"` match arms. Leave the `"link"` arm's `page_size` as `0` (the link pager does not use it). Concretely the `cursor` arm becomes:

```rust
        "cursor" => Some(PaginationCfg {
            style: "cursor".to_string(),
            limit_param: None,
            offset_param: None,
            page_size: p.limit.unwrap_or(DEFAULT_PAGE_SIZE),
            cursor_path: p.cursor_path_in_response.clone(),
            cursor_param: p.cursor_name_in_request.clone(),
            link_rel: None,
            has_next_path: None,
        }),
```

and the `offset` arm becomes:

```rust
        "offset" => Some(PaginationCfg {
            style: "offset".to_string(),
            limit_param: p.limit_name_in_request.clone(),
            offset_param: p
                .cursor_name_in_request
                .clone()
                .or_else(|| Some("offset".to_string())),
            page_size: p.limit.unwrap_or(DEFAULT_PAGE_SIZE),
            cursor_path: None,
            cursor_param: None,
            link_rel: None,
            has_next_path: None,
        }),
```

- [ ] **Step 5: Run the full crate tests**

Run: `cargo test -p spur-rest-table-gateway -- --nocapture`
Expected: PASS (new pagination tests green; `oauth_maps_to_bearer_byo` and `toml_roundtrips` still green).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/nango.rs
git commit -m "fix(rest-table-gateway): map Nango paginate limit to non-zero page_size"
```
