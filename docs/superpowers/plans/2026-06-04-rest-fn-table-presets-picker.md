# REST Function-as-Table — Presets, Typed Action Columns & Picker Reachability Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-04-rest-function-as-table-design.ipynb`
**Design epic:** `bd-824z` (closed)

**Goal:** Close the *real* gap that keeps Google Ads / Facebook Ads (and curated presets) from working as queryable REST functions for an analyst — typed action columns, picker→preset reachability, and the snapshot entries — given that the action/table-function **engine and DuckDB registration already ship in the loadable ext** (`register_adapter` → `ApiActionVTab`).

**Architecture:** The production analyst surface is the loadable DuckDB extension `spur_rest` (`crates/spur-notebook/rest-table-gateway-ext`), loaded by the notebook's Python kernel (`mcp/mod.rs:835`). Its `extension_entrypoint` → `register_saved_connections` → `register_adapter` already registers all three `TableKind` variants (`Table`, `TableFunction`, `Action`) from `adapter.catalog()`. `catalog()` includes actions only when `allow_writes=true`; `act()` returns typed rows only when the action declares `columns`. The gateway's in-process `register_tables` is test-only and out of scope. This plan therefore touches **manifest/catalog construction only** (preset TOMLs, the picker's `build_api_import_manifest`, the Nango snapshot) plus one ext e2e proving the chain.

**Tech Stack:** Rust, serde/TOML, wiremock, DuckDB (loadable extension). Build/test via `scripts/spur-cargo`. **The ext is a SEPARATE cargo workspace — test it with `--manifest-path crates/spur-notebook/rest-table-gateway-ext/Cargo.toml`, never `-p`.**

---

### Task 1: Typed columns for the Google Ads search action

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/connections/tier-a/google_ads.connection.toml`
- Test: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs` (`#[cfg(test)] mod tests`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `google_ads.connection.toml`'s `[[action]] google_ads_search` declares an `[action.columns]` block so `act()` returns typed Arrow rows instead of a single generic JSON row.
- [ ] A new test parses the preset, runs `act()` against a wiremock returning a GAQL `$.results` payload, and asserts the returned `RecordBatch` has the typed columns (not the generic `status/body` shape).
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway` is green.

**Suggested Worker:** codex (config + one focused test)

**Scope Boundary:**
- IN scope: the `google_ads.connection.toml` `[action.columns]` block and one new test in `manifest_adapter.rs`.
- OUT of scope: `act()` engine code, the ext crate, `build_api_import_manifest`, the snapshot yaml, any other preset.
- If you discover you need to touch any OUT-OF-SCOPE file, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Add the `[action.columns]` block** to `google_ads.connection.toml`, immediately after the existing `[action.args]` block. Column `json` paths are evaluated against each element of `response_path` (`$.results`), so they are relative to a single GAQL result row:

```toml
[action.columns]
campaign_id = { json = "$.campaign.id", type = "Utf8" }
campaign_name = { json = "$.campaign.name", type = "Utf8" }
impressions = { json = "$.metrics.impressions", type = "Int64" }
clicks = { json = "$.metrics.clicks", type = "Int64" }
cost_micros = { json = "$.metrics.costMicros", type = "Int64" }
```

- [ ] **Step 2: Write the failing test** (append to `mod tests` in `manifest_adapter.rs`). Model it on the existing `google_ads_action_uses_refresh_bearer_and_both_headers` test (same file) for wiremock + env setup, but assert typed columns:

```rust
    #[tokio::test]
    async fn google_ads_action_returns_typed_columns() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [
                    {"campaign": {"id": "11", "name": "A"},
                     "metrics": {"impressions": "100", "clicks": "5", "costMicros": "2000000"}},
                    {"campaign": {"id": "12", "name": "B"},
                     "metrics": {"impressions": "50", "clicks": "1", "costMicros": "500000"}}
                ]
            })))
            .mount(&server)
            .await;

        // Reuse the shipped preset, overriding only base_url to the mock.
        let toml = include_str!("../../connections/tier-a/google_ads.connection.toml")
            .replace("https://googleads.googleapis.com/v17", &server.uri());
        // Minimal creds so resolve_auth/headers succeed (mirror the existing GAds test).
        std::env::set_var("SPUR_CONN_DEVELOPER_TOKEN", "dev");
        std::env::set_var("SPUR_CONN_LOGIN_CUSTOMER_ID", "123");
        std::env::set_var("GOOGLE_ADS_CLIENT_ID", "cid");
        std::env::set_var("GOOGLE_ADS_CLIENT_SECRET", "secret");
        std::env::set_var("GOOGLE_ADS_REFRESH_TOKEN", "rt");

        let manifest = Manifest::from_toml(&toml).expect("preset parses");
        let adapter = ManifestAdapter::new(manifest);
        let req = ActionRequest {
            name: "google_ads_search".to_string(),
            method: "POST".to_string(),
            path: "/customers/123/googleAds:search".to_string(),
            query: vec![],
            body: Some(serde_json::json!({"query": "SELECT campaign.id FROM campaign"})),
            idempotency_key: None,
            dry_run: false,
        };
        let batches = adapter.act(req).await.expect("act returns typed rows");
        let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(rows, 2, "two GAQL results map to two typed rows");
        let schema = batches[0].schema();
        assert!(schema.column_with_name("campaign_id").is_some());
        assert!(schema.column_with_name("clicks").is_some());

        for k in ["SPUR_CONN_DEVELOPER_TOKEN","SPUR_CONN_LOGIN_CUSTOMER_ID",
                  "GOOGLE_ADS_CLIENT_ID","GOOGLE_ADS_CLIENT_SECRET","GOOGLE_ADS_REFRESH_TOKEN"] {
            std::env::remove_var(k);
        }
    }
```

- [ ] **Step 3: Run the test to verify it fails first, then passes after Step 1.** If the auth/refresh mock needs a token endpoint, mirror exactly what `google_ads_action_uses_refresh_bearer_and_both_headers` does in this same file (do not invent a new auth path).

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway google_ads_action_returns_typed_columns -- --nocapture`
Expected: PASS — two typed rows; `campaign_id`/`clicks` columns present.

- [ ] **Step 4: Full lib suite**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway`
Expected: PASS — all pre-existing tests stay green.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/connections/tier-a/google_ads.connection.toml \
        crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs
git commit -m "feat(rest-table-gateway): task-1 typed columns for google ads search action"
```

---

### Task 2: Picker prefers curated presets over the read-only stub

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/src/mcp/mod.rs` (`build_api_import_manifest`, lines ~956-1019)
- Test: `crates/spur-notebook/src/mcp/mod.rs` (`#[cfg(test)] mod tests`)

**Depends on:** none

**Acceptance Criteria:**
- [ ] When a bundled curated preset `connections/tier-a/<provider>.connection.toml` exists for the picked provider, `build_api_import_manifest` builds the manifest from that preset (full auth/headers/actions/`allow_writes`) instead of calling `provider_to_manifest_stub`.
- [ ] Provider-name normalization maps the snapshot's hyphenated key to the underscore preset filename (`google-ads` → `google_ads`).
- [ ] When no curated preset exists, behavior is unchanged (Nango stub fallback).
- [ ] A test proves picking `google-ads` yields a manifest with `allow_writes = true` and the `google_ads_search` action (i.e. NOT the empty stub).
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook --features datasource-introspect` is green.

**Suggested Worker:** codex (single-function change + test)

**Scope Boundary:**
- IN scope: `build_api_import_manifest` and one test in `mod.rs`. A bundled preset lookup (compile-time `include_str!`-backed table keyed by normalized provider name, or a `match` over the known tier-a providers — pick the pattern already used for embedded assets in this crate; do NOT read from the filesystem at runtime).
- OUT of scope: the ext crate, `provider_to_manifest_stub` itself, the snapshot yaml, preset TOML contents.
- If you need to touch any OUT-OF-SCOPE file, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Add a curated-preset lookup.** Near `build_api_import_manifest` (`#[cfg(feature = "datasource-introspect")]`), add a helper that returns the embedded preset TOML for a normalized provider name. Embed with `include_str!` so it ships in the binary (the preset files live in the sibling `rest-table-gateway` crate; reference them by relative path from `mod.rs`):

```rust
#[cfg(feature = "datasource-introspect")]
fn curated_preset_toml(source: &str) -> Option<&'static str> {
    // `source` is already trimmed + lowercased by normalize_api_datasource_source.
    // Snapshot keys are hyphenated; preset filenames use underscores.
    let key = source.replace('-', "_");
    match key.as_str() {
        "google_ads" => Some(include_str!(
            "../../rest-table-gateway/connections/tier-a/google_ads.connection.toml"
        )),
        _ => None,
    }
}
```

(Verify the relative `include_str!` path compiles from `crates/spur-notebook/src/mcp/mod.rs`; adjust the `../` depth if the build errors. Add a `facebook_ads` arm only if task-3 has landed the file — otherwise leave it to task-3 to extend this match.)

- [ ] **Step 2: Prefer the preset in `build_api_import_manifest`.** In the `if let Some(provider) = provider { ... }` branch, before constructing the Nango stub, try the curated preset:

```rust
    let mut toml = if let Some(provider) = provider {
        if let Some(preset) = curated_preset_toml(&source) {
            preset.to_string()
        } else {
            let providers =
                spur_rest_table_gateway::adapter::nango::parse_providers(NANGO_PROVIDERS_SNAPSHOT)
                    .map_err(|error| BridgeError::Handler {
                        code: "nango_provider_snapshot_failed".to_string(),
                        message: format!("failed to parse bundled Nango providers snapshot: {error}"),
                    })?;
            let provider = providers.get(&source).ok_or_else(|| BridgeError::Handler {
                code: "unknown_nango_provider".to_string(),
                message: format!("unknown Nango provider: {}", provider.trim()),
            })?;
            let manifest_stub =
                spur_rest_table_gateway::adapter::nango::provider_to_manifest_stub(&source, provider);
            spur_rest_table_gateway::adapter::nango::manifest_to_toml(&manifest_stub)
        }
    } else {
        // ... unchanged no-provider stub branch ...
    };
```

- [ ] **Step 3: Write the test** (append to `mod tests` in `mod.rs`, gated `#[cfg(feature = "datasource-introspect")]`):

```rust
    #[test]
    fn build_api_import_manifest_prefers_curated_google_ads_preset() {
        let (source, manifest) =
            build_api_import_manifest("gads", Some("google-ads".to_string()), None)
                .expect("manifest builds from preset");
        assert_eq!(source, "google_ads");
        assert!(manifest.source.allow_writes, "preset enables writes");
        assert!(
            manifest.actions.iter().any(|a| a.name == "google_ads_search"),
            "curated preset carries the search action, not the empty stub"
        );
    }
```

- [ ] **Step 4: Run**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook --features datasource-introspect build_api_import_manifest_prefers_curated -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/src/mcp/mod.rs
git commit -m "feat(spur-notebook): task-2 picker prefers curated tier-a presets over stub"
```

---

### Task 3: Snapshot entries + Facebook Ads preset

**Task ID:** `task-3`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/nango_providers_snapshot.yaml`
- Create: `crates/spur-notebook/rest-table-gateway/connections/tier-a/facebook_ads.connection.toml`
- Test: `crates/spur-notebook/rest-table-gateway/src/adapter/nango.rs` or an existing preset-parse test module (parse-validates the new preset)

**Depends on:** none

**Acceptance Criteria:**
- [ ] `nango_providers_snapshot.yaml` contains `google-ads` and `facebook-ads` entries (OAuth2 → tier B) so both appear in the picker.
- [ ] `facebook_ads.connection.toml` exists with `allow_writes = true`, the required auth/headers, and a row-returning insights action declaring `[action.columns]`.
- [ ] A test parses `facebook_ads.connection.toml` via `Manifest::from_toml` and asserts the action + at least one column exist.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway` is green.

**Suggested Worker:** codex (config + parse test)

**Scope Boundary:**
- IN scope: the snapshot yaml entries, the new `facebook_ads.connection.toml`, and one parse test.
- OUT of scope: `build_api_import_manifest` (task-2 owns it — do NOT edit `curated_preset_toml`'s match here unless task-2 is already merged and you are explicitly extending it; if so, add only the `facebook_ads` arm), `google_ads.connection.toml`, the ext crate.
- If you need to touch any OUT-OF-SCOPE file, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Add snapshot entries.** Mirror the existing `google-calendar` entry shape in `nango_providers_snapshot.yaml` (same fields: provider key, `auth_mode: OAUTH2`, `proxy.base_url`, etc.). Add `google-ads` and `facebook-ads` keys. Keep YAML 2-space indentation consistent with the file.

- [ ] **Step 2: Create `facebook_ads.connection.toml`** modeled on `google_ads.connection.toml`:

```toml
# Facebook/Meta Marketing API insights. Requires a Meta app (client_id/secret),
# a long-lived OAuth2 token, and the ad account id you query (act_<id>).
[source]
name = "facebook_ads"
base_url = "https://graph.facebook.com/v21.0"
allow_writes = true
connection_config = ["AD_ACCOUNT_ID"]
auth = { scheme = "oauth2_refresh", token_url = "https://graph.facebook.com/v21.0/oauth/access_token", client_id_env = "FACEBOOK_ADS_CLIENT_ID", client_secret_env = "FACEBOOK_ADS_CLIENT_SECRET", refresh_token_env = "FACEBOOK_ADS_REFRESH_TOKEN" }

[[action]]
name = "facebook_ads_insights"
method = "POST"
path = "/{ad_account_id}/insights"
response_path = "$.data"
pagination = { cursor_path = "$.paging.cursors.after", cursor_param = "after" }

[action.args]
ad_account_id = { in = "path", type = "Utf8", required = true }
fields = { in = "query", type = "Utf8", required = true }
date_preset = { in = "query", type = "Utf8", required = false }

[action.columns]
campaign_name = { json = "$.campaign_name", type = "Utf8" }
impressions = { json = "$.impressions", type = "Int64" }
clicks = { json = "$.clicks", type = "Int64" }
spend = { json = "$.spend", type = "Utf8" }
```

(If `oauth2_refresh` field names differ from what `AuthCfg` accepts, copy the exact `auth = { ... }` keys from `google_ads.connection.toml`, which is known to parse.)

- [ ] **Step 3: Write the parse test** (append to the preset-parse tests; mirror the existing `google_ads_preset_has_refresh_auth_and_required_headers` style):

```rust
    #[test]
    fn facebook_ads_preset_parses_with_typed_insights_action() {
        let toml = include_str!("../../connections/tier-a/facebook_ads.connection.toml");
        let manifest = Manifest::from_toml(toml).expect("facebook ads preset parses");
        assert!(manifest.source.allow_writes);
        let action = manifest
            .actions
            .iter()
            .find(|a| a.name == "facebook_ads_insights")
            .expect("insights action present");
        let columns = action.columns.as_ref().expect("typed columns present");
        assert!(columns.contains_key("impressions"));
    }
```

- [ ] **Step 4: Run**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway facebook_ads_preset_parses -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/jute-notebook/src-tauri/src/nango_providers_snapshot.yaml \
        crates/spur-notebook/rest-table-gateway/connections/tier-a/facebook_ads.connection.toml \
        crates/spur-notebook/rest-table-gateway/src/adapter/nango.rs
git commit -m "feat(rest-table-gateway): task-3 add google-ads/facebook-ads snapshot + fb preset"
```

---

### Task 4: End-to-end — action registers and returns typed rows through the ext

**Task ID:** `task-4`

**Files:**
- Create/Modify: `crates/spur-notebook/rest-table-gateway-ext/tests/load_extension_e2e.rs` (add one test; this file already loads the ext and queries — mirror `load_extension_queries_polymarket_markets_from_mock_rest_api`)

**Depends on:** task-1

**Acceptance Criteria:**
- [ ] A new e2e loads the built `spur_rest` extension with `SPUR_REST_MANIFEST` pointing at a temp manifest (allow_writes=true, a POST action with `[action.columns]`, `base_url` = a wiremock) and asserts `SELECT * FROM <source>_<action>(<arg> := '...')` returns the typed rows.
- [ ] The test reuses the existing harness's extension-build/load helper in this file (do not re-implement extension loading).
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test --manifest-path crates/spur-notebook/rest-table-gateway-ext/Cargo.toml --test load_extension_e2e` is green.

**Suggested Worker:** codex (one e2e mirroring an existing one)

**Scope Boundary:**
- IN scope: one new test function in `load_extension_e2e.rs` + any small temp-manifest helper local to that test.
- OUT of scope: `src/lib.rs` of the ext (registration already works — do NOT modify it), the gateway crate, the picker.
- If the test reveals the ext does NOT register the action (registration bug rather than a manifest gap), STOP and emit `risk` with the evidence — do not patch `lib.rs` under this task.

**Implementation:**

- [ ] **Step 1: Add the e2e** modeled on `load_extension_queries_polymarket_markets_from_mock_rest_api` (same file). Use a wiremock POST returning a `$.results`-style body, write a temp manifest, set `SPUR_REST_MANIFEST`, load the extension, and query the named-parameter action function:

```rust
#[test]
fn load_extension_queries_action_as_typed_table_function() {
    // 1. wiremock POST -> {"results":[{"v":{"id":"1"}},{"v":{"id":"2"}}]}
    // 2. temp manifest:
    //    [source] name="demo" base_url=<mock> allow_writes=true
    //    [[action]] name="search" method="POST" path="/q" response_path="$.results"
    //      [action.args] q = { in="body", type="Utf8", required=true }
    //      [action.columns] id = { json="$.v.id", type="Utf8" }
    // 3. SPUR_REST_MANIFEST=<temp>; LOAD '<ext>'
    // 4. SELECT * FROM demo_search(q := 'x') ORDER BY id  -> rows ["1","2"]
    // (Fill in using the mock/build/load helpers already in this file.)
}
```

- [ ] **Step 2: Run**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test --manifest-path crates/spur-notebook/rest-table-gateway-ext/Cargo.toml --test load_extension_e2e load_extension_queries_action_as_typed_table_function -- --nocapture`
Expected: PASS — the action function returns two typed rows. This proves: `LOAD` → `register_saved_connections`/`SPUR_REST_MANIFEST` → `register_adapter` → `ApiActionVTab` → typed rows.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway-ext/tests/load_extension_e2e.rs
git commit -m "test(rest-gateway-ext): task-4 action registers as typed table function e2e"
```

---

## DAG

```
task-1 ──► task-4
task-2  (independent)
task-3  (independent)
```

`task-1`, `task-2`, `task-3` are fully independent (no shared files) and dispatch in parallel. `task-4` depends on `task-1` (capstone e2e validating the typed-column pattern end-to-end through the ext).

## Out of scope (already shipped — do NOT re-implement)

- The DuckDB registration of actions/table-functions (`register_adapter`, `register_action_table_function`, `ApiActionVTab`, `compose_action_request`, `named_parameters()`) — confirmed working in the ext.
- The gateway in-process `vtab::register_tables` — test-only; not the analyst surface.
- `act()` engine (typed-column extraction via `action_column_extracts`, action pagination) — already present; task-1 only adds the preset's `columns` declaration.
