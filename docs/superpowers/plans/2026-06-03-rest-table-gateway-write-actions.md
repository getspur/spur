# REST Table Gateway — Write/Action Endpoints Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-03-rest-table-gateway-write-actions-spec.ipynb`
**Design epic:** `bd-d1ub` (closed on approval)

**Goal:** Add manifest-declared write/action endpoints (`POST`/`PUT`/`PATCH`/`DELETE`) to the REST table gateway, callable from SQL as typed DuckDB table functions that submit an API request and return the response as rows.

**Architecture:** A new `[[action]]` manifest section declares each endpoint with per-arg `in={path|body|query}` mapping. Actions thread through a dedicated `Adapter::act(ActionRequest)` method (default "unsupported" impl, so existing adapters are untouched) and a new `IoBridge` `Job::Act` variant. `http.rs` gains a single-shot, no-retry `send_request`. Responses render via the existing `json_to_batch` (typed columns) or a generic `(http_status, body)` fallback. Writes are gated behind `allow_writes`, support `dry_run`, and may carry an `Idempotency-Key`.

**Tech Stack:** Rust, `reqwest`, `serde`/`toml`, `arrow`, the `duckdb` crate's `VTab` API, `wiremock` (tests), `async-trait`, `tokio`.

---

## File Structure Map

| File | Responsibility | Task |
|------|----------------|------|
| `crates/spur-notebook/rest-table-gateway/src/adapter/manifest.rs` | `[[action]]` config model: `ActionCfg`, `ArgCfg`, `ArgLocation`, `SourceCfg.allow_writes` | T1 |
| `crates/spur-notebook/rest-table-gateway/src/adapter/http.rs` | `send_request` — method+body, single-shot, no retry, idempotency header | T2 |
| `crates/spur-notebook/rest-table-gateway/src/adapter/mod.rs` | `ActionRequest`, `ArgSpec`, `TableKind::Action`, `Adapter::act` default | T3 |
| `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs` | `ManifestAdapter::act`, action entries in `catalog()` (gated on `allow_writes`), `dry_run` short-circuit | T3 |
| `crates/spur-notebook/rest-table-gateway/src/vtab/bridge.rs` | `Job::{Scan,Act}` enum, `call_act()` | T4 |
| `crates/spur-notebook/rest-table-gateway-ext/src/lib.rs` | `TableKind::Action` registration, `ApiActionVTab`, arg routing, `SPUR_REST_ALLOW_WRITES` env | T5 |
| `crates/spur-notebook/rest-table-gateway-ext/tests/load_extension_e2e.rs` | E2E: all four verbs through the loaded extension + gate + dry-run | T6 |

---

## Dependency DAG

```
T1 (manifest)  ─┐
                ├─> T3 (adapter act + catalog) ─> T4 (bridge) ─> T5 (extension) ─> T6 (E2E)
T2 (http send) ─┘
```

T1 and T2 are independent roots and dispatch in parallel.

---

### Task 1: Manifest `[[action]]` config model

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest.rs`

**Depends on:** none

**Suggested Worker:** codex

**Acceptance Criteria:**
- [ ] `Manifest` parses an `[[action]]` block with `[action.args]` and optional `[action.columns]`.
- [ ] `SourceCfg` gains `allow_writes: bool` defaulting to `false`.
- [ ] `ArgLocation` deserializes from `in = "path" | "body" | "query"`.
- [ ] All new + existing manifest tests pass; no compilation errors.

**Scope Boundary:**
- IN scope: `manifest.rs` only (new structs, `#[serde]` attributes, parse tests).
- OUT of scope: `manifest_adapter.rs`, `http.rs`, `mod.rs`. Do not wire these types into the adapter yet (that is T3).
- If you need to touch any file other than `manifest.rs`, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Write the failing test** (append to the `tests` module at the bottom of `manifest.rs`)

```rust
#[test]
fn parses_action_manifest() {
    let manifest = Manifest::from_toml(
        r#"
[source]
name = "polymarket"
base_url = "https://clob.polymarket.com"
allow_writes = true

[[action]]
name = "place_order"
method = "POST"
path = "/orders/{token_id}"
response_path = "$.order"
idempotency_header = "Idempotency-Key"
dry_run_arg = "dry_run"

[action.args]
token_id = { in = "path",  type = "Utf8",    required = true }
price    = { in = "body",  type = "Float64", required = true, json = "price" }
verbose  = { in = "query", type = "Boolean", required = false, param = "verbose" }

[action.columns]
order_id = { json = "$.id", type = "Utf8" }
"#,
    )
    .expect("action manifest should parse");

    assert!(manifest.source.allow_writes);
    assert_eq!(manifest.actions.len(), 1);
    let action = &manifest.actions[0];
    assert_eq!(action.name, "place_order");
    assert_eq!(action.method, "POST");
    assert_eq!(action.path, "/orders/{token_id}");
    assert_eq!(action.idempotency_header.as_deref(), Some("Idempotency-Key"));
    assert_eq!(action.dry_run_arg.as_deref(), Some("dry_run"));
    assert_eq!(action.args["token_id"].in_, ArgLocation::Path);
    assert_eq!(action.args["price"].in_, ArgLocation::Body);
    assert_eq!(action.args["price"].json.as_deref(), Some("price"));
    assert_eq!(action.args["verbose"].in_, ArgLocation::Query);
    assert!(!action.args["verbose"].required);
    assert!(action.columns.as_ref().unwrap().contains_key("order_id"));
}

#[test]
fn allow_writes_defaults_false() {
    let manifest = Manifest::from_toml(
        r#"
[source]
name = "polymarket"
base_url = "https://clob.polymarket.com"

[[table]]
name = "markets"
path = "/markets"

[table.columns]
id = { json = "$.id", type = "Utf8" }
"#,
    )
    .expect("manifest should parse");
    assert!(!manifest.source.allow_writes);
    assert!(manifest.actions.is_empty());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p spur-rest-table-gateway parses_action_manifest allow_writes_defaults_false -- --nocapture`
Expected: FAIL — `no field actions on Manifest`, `no field allow_writes on SourceCfg`, `ArgLocation not found`.

- [ ] **Step 3: Write the implementation**

Add `allow_writes` to `SourceCfg` (alongside the existing `#[serde(default)]` fields):

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
    pub allow_writes: bool, // NEW — safety gate
}
```

Add `actions` to `Manifest`:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub source: SourceCfg,
    #[serde(default, rename = "table")]
    pub tables: Vec<TableCfg>,
    #[serde(default, rename = "action")]
    pub actions: Vec<ActionCfg>, // NEW
}
```

Add the new config types (place near `TableCfg`):

```rust
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArgLocation {
    Path,
    Body,
    Query,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArgCfg {
    #[serde(rename = "in")]
    pub in_: ArgLocation,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub required: bool,
    /// Body key (defaults to the arg name when omitted). Only meaningful for `in = "body"`.
    #[serde(default)]
    pub json: Option<String>,
    /// Query parameter name (defaults to the arg name). Only meaningful for `in = "query"`.
    #[serde(default)]
    pub param: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActionCfg {
    pub name: String,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub response_path: Option<String>,
    #[serde(default)]
    pub idempotency_header: Option<String>,
    #[serde(default)]
    pub dry_run_arg: Option<String>,
    pub args: IndexMap<String, ArgCfg>,
    #[serde(default)]
    pub columns: Option<IndexMap<String, ColumnCfg>>,
}
```

(`IndexMap` and `ColumnCfg` are already imported/defined in this file.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p spur-rest-table-gateway -- --nocapture`
Expected: PASS — both new tests plus all pre-existing manifest tests.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/manifest.rs
git commit -m "feat(rest-gateway): manifest [[action]] config model + allow_writes"
```

---

### Task 2: `http.rs` method-aware single-shot `send_request`

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/http.rs`

**Depends on:** none

**Suggested Worker:** codex

**Acceptance Criteria:**
- [ ] `send_request` issues exactly one request for the given `reqwest::Method` with optional JSON body + query + auth + optional idempotency header.
- [ ] 204 / empty body returns `(status, Value::Null)`.
- [ ] Non-2xx returns `GatewayError::Http` containing the status code.
- [ ] No retry, no pagination in this code path.
- [ ] New `wiremock` tests pass; existing GET tests still pass.

**Scope Boundary:**
- IN scope: `http.rs` only — add `HttpAction` + `send_request` + tests. Reuse the existing `apply_auth`.
- OUT of scope: callers of `send_request` (that's T3), `mod.rs`, `manifest.rs`.
- If you need to touch any file other than `http.rs`, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Write the failing tests** (append to the `tests` module at the bottom of `http.rs`)

```rust
#[tokio::test]
async fn post_sends_body_and_returns_parsed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/orders/abc"))
        .and(header("idempotency-key", "key-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "o1" })))
        .mount(&server)
        .await;

    let client = Client::new();
    let auth = ResolvedAuth::None;
    let action = HttpAction {
        client: &client,
        method: reqwest::Method::POST,
        url: format!("{}/orders/abc", server.uri()),
        query: vec![("verbose".to_string(), "true".to_string())],
        body: Some(json!({ "price": 0.5 })),
        auth: &auth,
        idempotency_key: Some(("Idempotency-Key".to_string(), "key-1".to_string())),
    };

    let (status, body) = send_request(&action).await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(body["id"], "o1");
}

#[tokio::test]
async fn delete_204_returns_null_body() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/orders/abc"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let client = Client::new();
    let auth = ResolvedAuth::None;
    let action = HttpAction {
        client: &client,
        method: reqwest::Method::DELETE,
        url: format!("{}/orders/abc", server.uri()),
        query: vec![],
        body: None,
        auth: &auth,
        idempotency_key: None,
    };

    let (status, body) = send_request(&action).await.unwrap();
    assert_eq!(status, 204);
    assert!(body.is_null());
}

#[tokio::test]
async fn non_2xx_is_error_with_status() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/orders/abc"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({ "error": "bad" })))
        .mount(&server)
        .await;

    let client = Client::new();
    let auth = ResolvedAuth::None;
    let action = HttpAction {
        client: &client,
        method: reqwest::Method::PATCH,
        url: format!("{}/orders/abc", server.uri()),
        query: vec![],
        body: Some(json!({ "price": 1.0 })),
        auth: &auth,
        idempotency_key: None,
    };

    let err = send_request(&action).await.unwrap_err();
    assert!(format!("{err}").contains("422"));
}
```

Add the missing import to the `tests` module's `use wiremock::matchers::{...}` line: ensure `header` is included (it already is in the existing GET tests; if compiling the module in isolation, add it).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p spur-rest-table-gateway --lib http:: -- --nocapture`
Expected: FAIL — `HttpAction not found`, `send_request not found`.

- [ ] **Step 3: Write the implementation** (add to `http.rs`, after the `HttpFetch` definitions)

```rust
pub struct HttpAction<'a> {
    pub client: &'a Client,
    pub method: reqwest::Method,
    pub url: String,
    pub query: Vec<(String, String)>,
    pub body: Option<serde_json::Value>,
    pub auth: &'a ResolvedAuth,
    /// (header name, value) — attached verbatim when present.
    pub idempotency_key: Option<(String, String)>,
}

/// Issues exactly one request. No retry, no pagination.
/// Returns (status, parsed body). 204 / empty body -> Value::Null.
pub async fn send_request(a: &HttpAction<'_>) -> Result<(u16, serde_json::Value)> {
    let mut req = a.client.request(a.method.clone(), &a.url).query(&a.query);
    if let Some(body) = &a.body {
        req = req.json(body);
    }
    if let Some((name, value)) = &a.idempotency_key {
        req = req.header(name.as_str(), value.as_str());
    }
    let req = apply_auth(req, a.auth);

    let resp = req
        .send()
        .await
        .map_err(|e| GatewayError::Http(e.to_string()))?;
    let status = resp.status();

    if !status.is_success() {
        let snippet = resp.text().await.unwrap_or_default();
        let snippet: String = snippet.chars().take(500).collect();
        return Err(GatewayError::Http(format!(
            "status {status}: {snippet}"
        )));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| GatewayError::Http(e.to_string()))?;
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).map_err(|e| GatewayError::Http(e.to_string()))?
    };

    Ok((status.as_u16(), body))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p spur-rest-table-gateway -- --nocapture`
Expected: PASS — the three new tests plus all existing GET/pagination tests.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/http.rs
git commit -m "feat(rest-gateway): single-shot send_request for write verbs"
```

---

### Task 3: `Adapter::act` + `ManifestAdapter` action support

**Task ID:** `task-3`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/mod.rs`
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs`

**Depends on:** task-1, task-2

**Suggested Worker:** codex

**Acceptance Criteria:**
- [ ] `Adapter` trait has `async fn act(&self, ActionRequest) -> Result<Vec<RecordBatch>>` with a default impl returning a `GatewayError::Adapter("...does not support actions")`.
- [ ] `TableKind::Action { method, arg_specs }` exists; `ArgSpec` + `ArgLocation` are usable from the extension crate.
- [ ] `ManifestAdapter::catalog()` emits one `TableDef { kind: Action }` per `[[action]]` **only when `source.allow_writes` is true**.
- [ ] `ManifestAdapter::act()` composes the request from the `ActionRequest`, calls `send_request`, and renders rows: typed via `json_to_batch`+`response_path` when `columns` declared, else a generic `(http_status BIGINT, body VARCHAR)` row. `dry_run` returns the composed request as a row without sending.
- [ ] Existing adapter/manifest_adapter tests still pass.

**Scope Boundary:**
- IN scope: `mod.rs` (trait + types), `manifest_adapter.rs` (impl + catalog).
- OUT of scope: `bridge.rs` (T4), `ext/src/lib.rs` (T5), `http.rs`, `manifest.rs`.
- The default `act` impl MUST keep `PolymarketAdapter`, `GraphqlAdapter`, and test adapters compiling unchanged — do not edit those files.
- If you need to touch any file outside the two listed, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Write the failing test** (append to the `tests` module in `manifest_adapter.rs`; it already constructs `ManifestAdapter` from TOML in existing tests — follow that pattern)

```rust
#[tokio::test]
async fn action_post_renders_typed_columns() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/orders/tok1"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "order": { "id": "o9" } })),
        )
        .mount(&server)
        .await;

    let toml = format!(
        r#"
[source]
name = "pm"
base_url = "{base}"
allow_writes = true

[[action]]
name = "place_order"
method = "POST"
path = "/orders/{{token_id}}"
response_path = "$.order"

[action.args]
token_id = {{ in = "path", type = "Utf8", required = true }}
price    = {{ in = "body", type = "Float64", required = true }}

[action.columns]
id = {{ json = "$.id", type = "Utf8" }}
"#,
        base = server.uri()
    );
    let manifest = Manifest::from_toml(&toml).unwrap();
    let adapter = ManifestAdapter::new(manifest);

    // catalog exposes the action because allow_writes = true
    assert!(adapter
        .catalog()
        .iter()
        .any(|t| t.name == "place_order" && matches!(t.kind, TableKind::Action { .. })));

    let req = ActionRequest {
        name: "place_order".to_string(),
        method: "POST".to_string(),
        path: "/orders/tok1".to_string(),
        query: vec![],
        body: Some(serde_json::json!({ "price": 0.5 })),
        auth: ResolvedAuth::None,
        idempotency_key: None,
        dry_run: false,
    };
    let batches = adapter.act(req).await.unwrap();
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 1);
}

#[tokio::test]
async fn action_hidden_when_writes_disabled() {
    let toml = r#"
[source]
name = "pm"
base_url = "https://example.com"

[[action]]
name = "place_order"
method = "POST"
path = "/orders"

[action.args]
price = { in = "body", type = "Float64", required = true }
"#;
    let adapter = ManifestAdapter::new(Manifest::from_toml(toml).unwrap());
    assert!(!adapter.catalog().iter().any(|t| t.name == "place_order"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p spur-rest-table-gateway action_ -- --nocapture`
Expected: FAIL — `ActionRequest not found`, `TableKind::Action not found`, `act not found`.

- [ ] **Step 3: Write the implementation**

In `mod.rs`, add types and extend the trait + `TableKind`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgLocation {
    Path,
    Body,
    Query,
}

#[derive(Debug, Clone)]
pub struct ArgSpec {
    pub name: String,
    pub location: ArgLocation,
    pub ty: DataType,        // arrow_schema::DataType, already imported in this crate
    pub required: bool,
    pub json_key: String,    // body key (defaults to name)
    pub query_param: String, // query param (defaults to name)
}

#[derive(Debug, Clone)]
pub struct ActionRequest {
    pub name: String,
    pub method: String,
    pub path: String, // placeholders already filled
    pub query: Vec<(String, String)>,
    pub body: Option<serde_json::Value>,
    pub auth: ResolvedAuth,
    pub idempotency_key: Option<String>,
    pub dry_run: bool,
}

pub enum TableKind {
    Table,
    TableFunction { arg_names: Vec<String> },
    // NEW — carries everything the extension needs at bind time (it only sees TableKind)
    Action {
        method: String,
        path: String,                        // template with {placeholders}
        arg_specs: Vec<ArgSpec>,
        dry_run_arg: Option<String>,
        idempotency_header: Option<String>,
    },
}
```

Extend the `Adapter` trait with a defaulted method:

```rust
#[async_trait]
pub trait Adapter: Send + Sync {
    fn name(&self) -> &str;
    fn catalog(&self) -> Vec<TableDef>;
    async fn scan(&self, req: ScanRequest) -> Result<Vec<RecordBatch>>;
    async fn act(&self, _req: ActionRequest) -> Result<Vec<RecordBatch>> {
        Err(crate::error::GatewayError::Adapter(
            "this adapter does not support actions".to_string(),
        ))
    }
}
```

In `manifest_adapter.rs`:

1. Map `ActionCfg.args` → `Vec<ArgSpec>` (reuse the existing `manifest.rs` string-type → `DataType` mapping helper that `scan`/catalog already use for `ColumnCfg.ty`; if it is private, call the same path the table catalog uses). Body key defaults to the arg name, query param defaults to the arg name.

2. In `catalog()`, after building table defs, append action defs **guarded by `allow_writes`**:

```rust
if self.manifest.source.allow_writes {
    for action in &self.manifest.actions {
        let arg_specs = action
            .args
            .iter()
            .map(|(name, cfg)| ArgSpec {
                name: name.clone(),
                location: cfg.in_,
                ty: parse_arrow_type(&cfg.ty), // same helper used for columns
                required: cfg.required,
                json_key: cfg.json.clone().unwrap_or_else(|| name.clone()),
                query_param: cfg.param.clone().unwrap_or_else(|| name.clone()),
            })
            .collect();
        defs.push(TableDef {
            name: action.name.clone(),
            schema: action_response_schema(action), // declared columns OR (http_status, body)
            kind: TableKind::Action {
                method: action.method.clone(),
                path: action.path.clone(),
                arg_specs,
                dry_run_arg: action.dry_run_arg.clone(),
                idempotency_header: action.idempotency_header.clone(),
            },
        });
    }
}
```

`action_response_schema(action)`: if `action.columns` is `Some`, build the Arrow schema from it exactly as table columns are built today; otherwise return a fixed schema `[("http_status", Int64, false), ("body", Utf8, true)]`.

3. Implement `act()`:

```rust
async fn act(&self, req: ActionRequest) -> Result<Vec<RecordBatch>> {
    let action = self
        .manifest
        .actions
        .iter()
        .find(|a| a.name == req.name)
        .ok_or_else(|| GatewayError::Adapter(format!("unknown action {}", req.name)))?;

    let url = format!("{}{}", self.manifest.source.base_url.trim_end_matches('/'), req.path);

    if req.dry_run {
        // Compose-and-return without sending.
        return render_generic_row(
            0,
            serde_json::json!({
                "dry_run": true,
                "method": req.method,
                "url": url,
                "query": req.query,
                "body": req.body,
            }),
        );
    }

    let idempotency_key = match (&action.idempotency_header, &req.idempotency_key) {
        (Some(header), Some(value)) => Some((header.clone(), value.clone())),
        _ => None,
    };

    let http_action = HttpAction {
        client: &self.client, // ManifestAdapter already holds a reqwest::Client
        method: reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|e| GatewayError::Http(e.to_string()))?,
        url,
        query: req.query,
        body: req.body,
        auth: &req.auth,
        idempotency_key,
    };

    let (status, body) = send_request(&http_action).await?;

    match &action.columns {
        Some(_columns) => {
            // Typed rendering — reuse the exact path scan() uses.
            let rows = rows_from_body(&body, action.response_path.as_deref())?;
            json_rows_to_batches(&rows, &action_response_schema(action), action)
            // (use whatever helper scan() already calls to turn Vec<Value> + schema into batches)
        }
        None => render_generic_row(status, body),
    }
}
```

`render_generic_row(status, body)` builds a single-row `RecordBatch` with `http_status: Int64` and `body: Utf8` (body serialized to a JSON string; `Null` body -> SQL NULL). Reuse the crate's existing `RecordBatch` construction style.

> Note: reuse `rows_from_body` (already `pub(crate)` in `http.rs`) and the same JSON→batch helper `scan()` uses (`json_to_batch.rs`). Do not duplicate batch-building logic — call the existing function.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p spur-rest-table-gateway -- --nocapture`
Expected: PASS — new action tests plus all existing tests.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/mod.rs \
        crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs
git commit -m "feat(rest-gateway): Adapter::act + ManifestAdapter action support"
```

**Scope Drift Checkpoint:**
- If the JSON→batch helper used by `scan()` is not reusable for actions without refactor → emit `scope_drift` (do not invent a parallel renderer).
- If `ManifestAdapter` has no `reqwest::Client` field to reuse → emit `risk` and describe before adding one.

---

### Task 4: `IoBridge` `Job::Act` variant

**Task ID:** `task-4`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/vtab/bridge.rs`

**Depends on:** task-3

**Suggested Worker:** codex

**Acceptance Criteria:**
- [ ] `Job` becomes an enum with `Scan` and `Act` variants carrying the respective request types; both reply via `mpsc::Sender<Result<Vec<RecordBatch>>>`.
- [ ] The IO loop matches on the job and calls `scan`/`act`.
- [ ] `IoBridge::call` (scan) is preserved; `IoBridge::call_act` mirrors it for actions.
- [ ] Existing `bridge_works_inside_outer_tokio_runtime` test still passes; a new action test passes.

**Scope Boundary:**
- IN scope: `bridge.rs` only.
- OUT of scope: `ext/src/lib.rs` (T5), adapter files.
- If you need to touch any other file, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Write the failing test** (append to the `tests` module in `bridge.rs`; reuse the existing `TestAdapter` pattern, overriding `act`)

```rust
#[test]
fn bridge_dispatches_act() {
    struct ActAdapter;
    #[async_trait]
    impl Adapter for ActAdapter {
        fn name(&self) -> &str { "act" }
        fn catalog(&self) -> Vec<TableDef> { vec![] }
        async fn scan(&self, _req: ScanRequest) -> Result<Vec<RecordBatch>> { Ok(vec![]) }
        async fn act(&self, _req: ActionRequest) -> Result<Vec<RecordBatch>> {
            let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Int64, false)]));
            let col = Arc::new(Int64Array::from(vec![7]));
            Ok(vec![RecordBatch::try_new(schema, vec![col]).unwrap()])
        }
    }

    let bridge = IoBridge::new();
    let adapter: Arc<dyn Adapter> = Arc::new(ActAdapter);
    let req = ActionRequest {
        name: "x".into(), method: "POST".into(), path: "/x".into(),
        query: vec![], body: None, auth: Default::default(),
        idempotency_key: None, dry_run: false,
    };

    let outer = tokio::runtime::Runtime::new().unwrap();
    let result = outer.block_on(async {
        tokio::task::spawn_blocking(move || bridge.call_act(adapter, req)).await.unwrap()
    });
    assert_eq!(result.unwrap()[0].num_rows(), 1);
}
```

Add `ActionRequest` to the test module's `use crate::adapter::{...}` import.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p spur-rest-table-gateway bridge_dispatches_act -- --nocapture`
Expected: FAIL — `call_act not found`.

- [ ] **Step 3: Write the implementation**

```rust
use crate::adapter::{Adapter, ActionRequest, ScanRequest};

enum Job {
    Scan(Arc<dyn Adapter>, ScanRequest, mpsc::Sender<Result<Vec<RecordBatch>>>),
    Act(Arc<dyn Adapter>, ActionRequest, mpsc::Sender<Result<Vec<RecordBatch>>>),
}
```

Update the IO loop:

```rust
while let Ok(job) = rx.recv() {
    match job {
        Job::Scan(adapter, req, reply) => {
            let res = rt.block_on(adapter.scan(req));
            let _ = reply.send(res);
        }
        Job::Act(adapter, req, reply) => {
            let res = rt.block_on(adapter.act(req));
            let _ = reply.send(res);
        }
    }
}
```

Keep `call` (sending `Job::Scan`) and add:

```rust
pub fn call_act(&self, adapter: Arc<dyn Adapter>, req: ActionRequest) -> Result<Vec<RecordBatch>> {
    let (rtx, rrx) = mpsc::channel();
    self.tx
        .send(Job::Act(adapter, req, rtx))
        .map_err(|e| GatewayError::Adapter(format!("io bridge send: {e}")))?;
    rrx.recv()
        .map_err(|e| GatewayError::Adapter(format!("io bridge recv: {e}")))?
}
```

Update `call` to construct `Job::Scan(...)` instead of the old tuple.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p spur-rest-table-gateway bridge -- --nocapture`
Expected: PASS — both `bridge_works_inside_outer_tokio_runtime` and `bridge_dispatches_act`.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/vtab/bridge.rs
git commit -m "feat(rest-gateway): IoBridge Job::Act variant + call_act"
```

---

### Task 5: Extension registration — `ApiActionVTab`

**Task ID:** `task-5`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway-ext/src/lib.rs`

**Depends on:** task-4

**Suggested Worker:** codex

**Acceptance Criteria:**
- [ ] `register_adapter` handles `TableKind::Action` by registering a new `ApiActionVTab` named `<adapter>_<action>`.
- [ ] `ApiActionVTab::bind` routes each named arg into path/body/query by its `ArgSpec.location`, filling `{placeholders}` in the path, building the JSON body, and the query vec; missing required args -> bind error.
- [ ] `named_parameters()` is derived from `arg_specs` (not the hard-coded `token_id`/`depth` list).
- [ ] `init` calls `bridge.call_act(...)`; `func` reuses `write_batch_rows`.
- [ ] `dry_run` arg (named by `ActionCfg.dry_run_arg`) and `idempotency` arg are honored; `SPUR_REST_ALLOW_WRITES` env can force-enable the gate even if a manifest omitted `allow_writes` (OR semantics with the manifest flag).
- [ ] Crate compiles; `cargo build -p spur-rest` (the ext crate) succeeds.

**Scope Boundary:**
- IN scope: `rest-table-gateway-ext/src/lib.rs` only.
- OUT of scope: the gateway library crate (all of T1–T4), tests file (T6).
- The `allow_writes` *manifest* gate already filters `catalog()` in T3; here only add the **env override** (`SPUR_REST_ALLOW_WRITES`) and the registration/bind/func plumbing.
- If you need to touch any file other than `lib.rs`, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Add the `TableKind::Action` registration arm** in `register_adapter` (mirror the existing `TableFunction` arm):

```rust
TableKind::Action { method, path, arg_specs, dry_run_arg, idempotency_header } => {
    let extra = ApiActionExtra {
        bridge: Arc::clone(&bridge),
        adapter: Arc::clone(&adapter),
        action: table.name,
        method,
        action_path: path,
        schema: table.schema,
        arg_specs,
        dry_run_arg,
        idempotency_header,
    };
    con.register_table_function_with_extra_info::<ApiActionVTab, _>(&fn_name, &extra)?;
    registered += 1;
}
```

- [ ] **Step 2: Define `ApiActionExtra`, bind/init data, and `ApiActionVTab`** (mirror `ApiFunctionExtra` / `ApiFunctionVTab`):

```rust
#[derive(Clone)]
struct ApiActionExtra {
    bridge: Arc<IoBridge>,
    adapter: Arc<dyn Adapter>,
    action: String,
    method: String,
    action_path: String, // path template with {placeholders}
    schema: SchemaRef,
    arg_specs: Vec<ArgSpec>,
    dry_run_arg: Option<String>,
    idempotency_header: Option<String>,
}

struct ApiActionBindData {
    bridge: Arc<IoBridge>,
    adapter: Arc<dyn Adapter>,
    schema: SchemaRef,
    request: ActionRequest, // fully composed at bind time
}

struct ApiActionInitData {
    rows: Vec<RecordBatch>,
    cursor: Mutex<ApiCursor>,
}

struct ApiActionVTab;

impl VTab for ApiActionVTab {
    type InitData = ApiActionInitData;
    type BindData = ApiActionBindData;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        let extra = unsafe { &*bind.get_extra_info::<ApiActionExtra>() };
        for field in extra.schema.fields() {
            let lt = LogicalTypeHandle::from(arrow_to_duckdb_type(field.data_type())?);
            bind.add_result_column(field.name(), lt);
        }
        let request = compose_action_request(bind, extra)?;
        Ok(ApiActionBindData {
            bridge: Arc::clone(&extra.bridge),
            adapter: Arc::clone(&extra.adapter),
            schema: Arc::clone(&extra.schema),
            request,
        })
    }

    fn init(init: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        let bind_data = unsafe { &*init.get_bind_data::<ApiActionBindData>() };
        let rows = bind_data
            .bridge
            .call_act(Arc::clone(&bind_data.adapter), bind_data.request.clone())?;
        Ok(ApiActionInitData { rows, cursor: Mutex::new(ApiCursor::default()) })
    }

    fn func(func: &TableFunctionInfo<Self>, output: &mut DataChunkHandle) -> Result<(), Box<dyn Error>> {
        // identical chunked drain to ApiFunctionVTab::func, using get_init_data()/get_bind_data()
        // and write_batch_rows(...). Copy that body verbatim, swapping the data types.
    }

    fn named_parameters() -> Option<Vec<(String, LogicalTypeHandle)>> {
        // Cannot read arg_specs here (no extra). Return None and rely on
        // get_named_parameter(name) lookups in compose_action_request, OR if the
        // duckdb crate requires declared params, declare them in bind via extra.
        None
    }
}
```

- [ ] **Step 3: Implement `compose_action_request`** — the arg-routing core:

```rust
fn compose_action_request(
    bind: &BindInfo,
    extra: &ApiActionExtra,
) -> Result<ActionRequest, Box<dyn Error>> {
    let mut path = extra.action_path.clone(); // path template carried on extra (add field)
    let mut body = serde_json::Map::new();
    let mut query: Vec<(String, String)> = Vec::new();
    let mut dry_run = false;
    let mut idempotency_key: Option<String> = None;

    for spec in &extra.arg_specs {
        let raw = bind.get_named_parameter(&spec.name);
        let Some(value) = raw else {
            if spec.required {
                return Err(format!("action {} requires {}", extra.action, spec.name).into());
            }
            continue;
        };
        match spec.location {
            ArgLocation::Path => {
                path = path.replace(&format!("{{{}}}", spec.name), &duckdb_value_to_string(&value));
            }
            ArgLocation::Body => {
                body.insert(spec.json_key.clone(), duckdb_value_to_json(&value, &spec.ty));
            }
            ArgLocation::Query => {
                query.push((spec.query_param.clone(), duckdb_value_to_string(&value)));
            }
        }
    }

    // dry_run / idempotency named params (declared on extra via ActionCfg)
    if let Some(arg) = &extra.dry_run_arg {
        if let Some(v) = bind.get_named_parameter(arg) { dry_run = v.to_boolean(); }
    }
    if let Some(_header) = &extra.idempotency_header {
        if let Some(v) = bind.get_named_parameter("idempotency_key") {
            idempotency_key = Some(v.to_string());
        }
    }

    Ok(ActionRequest {
        name: extra.action.clone(),
        method: extra.method.clone(),
        path,
        query,
        body: if body.is_empty() { None } else { Some(serde_json::Value::Object(body)) },
        auth: ResolvedAuth::None, // resolved upstream; reuse existing scan auth wiring if present
        idempotency_key,
        dry_run,
    })
}
```

Add small helpers `duckdb_value_to_string` and `duckdb_value_to_json(value, &DataType)` near `bind_named_args` (mirroring how `bind_named_args` already calls `.to_string()` / `.to_int64()`). Carry `action_path`, `dry_run_arg`, `idempotency_header` on `ApiActionExtra` (extend the struct + the registration arm in Step 1 to populate them from the `TableDef`/manifest — if the path/template is not already on `TableKind::Action`, add it there in T3 scope; if discovered late, emit `scope_drift`).

- [ ] **Step 4: Env-override gate** — in `extension_entrypoint` (or `register_adapter`), compute `allow_writes = manifest.source.allow_writes || env::var("SPUR_REST_ALLOW_WRITES").is_ok()`. Since the manifest gate already filters `catalog()` in T3, the env override means: when set, re-derive the catalog with writes enabled. Simplest implementation: pass an `allow_writes_env: bool` into `ManifestAdapter` construction OR have `register_adapter` skip `Action` defs unless (`source.allow_writes` || env). Keep it to `lib.rs`: filter `TableKind::Action` defs at registration unless the env var is set OR the manifest already emitted them.

- [ ] **Step 5: Build**

Run: `cargo build -p spur-rest`
Expected: compiles cleanly.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway-ext/src/lib.rs
git commit -m "feat(rest-gateway-ext): ApiActionVTab registration + arg routing + write gate"
```

**Scope Drift Checkpoint:**
- `TableKind::Action` already carries `path` / `dry_run_arg` / `idempotency_header` / `arg_specs` (defined in T3), so bind has all metadata it needs — no T3 edits required. If you find a field genuinely missing, emit `scope_drift` rather than editing T3 files.
- If the `duckdb` crate's `VTab` requires `named_parameters()` to statically declare every param (so `get_named_parameter` works) → emit `risk` and describe. The fix is to thread `arg_specs` (already on `ApiActionExtra`) into a declared param list; if `named_parameters()` cannot access the extra, declare params in `bind` instead.

---

### Task 6: End-to-end test — four verbs through the loaded extension

**Task ID:** `task-6`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway-ext/tests/load_extension_e2e.rs`

**Depends on:** task-5

**Suggested Worker:** codex

**Acceptance Criteria:**
- [ ] Builds the extension, loads it with `allow_unsigned_extensions=true`, registers a write manifest (`allow_writes = true`) pointing at a `wiremock` server.
- [ ] Exercises `POST`, `PUT`, `PATCH`, `DELETE` actions via SQL `SELECT * FROM <adapter>_<action>(...)` and asserts the returned rows.
- [ ] Asserts a manifest WITHOUT `allow_writes` does **not** register the action (calling it errors / function unknown).
- [ ] Asserts `dry_run := true` returns the composed-request row and the `wiremock` server received **no** matching request.
- [ ] Test passes under the crate's E2E target.

**Scope Boundary:**
- IN scope: `load_extension_e2e.rs` only — add new `#[test]`/`#[tokio::test]` cases following the existing E2E harness in that file (it already builds + loads the extension and sets `SPUR_POLYMARKET_*` / `SPUR_REST_MANIFEST` env).
- OUT of scope: any `src/` file. If the E2E reveals a product bug, emit `risk` with the failing assertion rather than editing source.
- If you need to touch any file other than the test file, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Add the E2E test** (follow the existing harness in this file for how it builds the `.duckdb_extension`, opens a `Connection` with `allow_unsigned_extensions`, writes a manifest to a temp path, and sets `SPUR_REST_MANIFEST`)

```rust
#[tokio::test]
async fn write_actions_all_verbs_e2e() {
    let server = wiremock::MockServer::start().await;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    for (verb, p) in [("POST", "/orders"), ("PUT", "/orders/1"),
                      ("PATCH", "/orders/1"), ("DELETE", "/orders/1")] {
        Mock::given(method(verb)).and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server).await;
    }

    let manifest = format!(r#"
[source]
name = "svc"
base_url = "{base}"
allow_writes = true

[[action]]
name = "create"
method = "POST"
path = "/orders"
[action.args]
price = {{ in = "body", type = "Float64", required = true }}

[[action]]
name = "replace"
method = "PUT"
path = "/orders/{{id}}"
[action.args]
id = {{ in = "path", type = "Utf8", required = true }}
price = {{ in = "body", type = "Float64", required = true }}

[[action]]
name = "modify"
method = "PATCH"
path = "/orders/{{id}}"
[action.args]
id = {{ in = "path", type = "Utf8", required = true }}
price = {{ in = "body", type = "Float64", required = true }}

[[action]]
name = "remove"
method = "DELETE"
path = "/orders/{{id}}"
[action.args]
id = {{ in = "path", type = "Utf8", required = true }}
"#, base = server.uri());

    // ... write `manifest` to a temp file, set SPUR_REST_MANIFEST, LOAD the extension
    // (reuse the existing harness helper in this file) ...

    // POST
    // SELECT http_status FROM svc_create(price := 0.5) -> expect a row with status 200
    // PUT / PATCH / DELETE similarly with id := '1'
    // Assert each returns one row and status 200.
}

#[tokio::test]
async fn action_not_registered_without_allow_writes() {
    // Same as above but manifest omits `allow_writes`.
    // Expect: querying svc_create(...) errors with "unknown" / function not found.
}

#[tokio::test]
async fn dry_run_sends_nothing() {
    // allow_writes = true, action declares dry_run_arg = "dry_run".
    // SELECT * FROM svc_create(price := 0.5, dry_run := true)
    // Assert: returns the composed-request row AND server.received_requests() has no /orders POST.
}
```

- [ ] **Step 2: Run the E2E**

Run:
```bash
CARGO_TARGET_DIR=/private/tmp/spur-rest-table-gateway-ext-test-target \
  cargo test -p spur-rest --test load_extension_e2e write_actions -- --nocapture
```
Expected: PASS (after `scripts/build.sh` has produced the extension, per the crate README).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway-ext/tests/load_extension_e2e.rs
git commit -m "test(rest-gateway-ext): e2e write actions for all four verbs + gate + dry-run"
```

---

## Self-Review

**Spec coverage:**
- Manifest `[[action]]` schema → T1. ✅
- Per-arg `in` mapping → T1 (model) + T5 (routing). ✅
- `http.rs` send path → T2. ✅
- `Adapter::act` + bridge → T3 + T4. ✅
- Extension registration → T5. ✅
- Hybrid return shape → T3 (`act` rendering) + asserted in T6. ✅
- Safety: `allow_writes` gate → T3 (catalog filter) + T5 (env override); no-retry/pagination-bypass → T2 (structural); idempotency → T2+T5; dry_run → T3+T5. ✅
- Testing strategy → unit tests in T1–T4, E2E in T6. ✅
- Out of scope (INSERT INTO DML) → not planned, correct. ✅

**Placeholder scan:** Code steps carry concrete code. Two intentional "reuse the existing helper" pointers (json→batch in T3, E2E harness in T6) are guarded with `scope_drift`/`risk` checkpoints rather than left vague.

**Type consistency:** `ActionRequest`, `ArgSpec`, `ArgLocation`, `TableKind::Action`, `HttpAction`/`send_request` signatures are consistent across T1→T6. `ArgLocation` is defined once in `manifest.rs` (T1) and re-exported/mirrored for `ArgSpec` in `mod.rs` (T3) — T5 imports both.

**DAG validation:** `T1,T2 → T3 → T4 → T5 → T6`. No cycles. T1/T2 parallel. Chain depth 5 is inherent (each layer compiles on the one below).

**beads compatibility:** every task has a unique ID, explicit `depends_on`, verifiable acceptance criteria, and a scope boundary.

**Resolved cross-task seam:** `TableKind::Action` (T3) carries `method`, `path`, `arg_specs`, `dry_run_arg`, and `idempotency_header` — everything T5's `bind` needs, since the extension only sees `TableKind`. The remaining integration risk is the `duckdb` `VTab` `named_parameters()` declaration mechanics (flagged as a `risk` checkpoint in T5), not a missing field.
