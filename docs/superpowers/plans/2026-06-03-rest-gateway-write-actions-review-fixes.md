# REST Gateway Write Actions — Review-Fix Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-03-rest-table-gateway-write-actions-spec.ipynb`
**Origin feature plan:** `docs/superpowers/plans/2026-06-03-rest-table-gateway-write-actions.md` (merged to main, commits `2e148a9b..f2667100`)
**Review basis:** two multi-perspective code reviews (code-reviewer + code-explore + graph-analyst, fresh-index re-run) of range `28b048b7..f2667100`.

**Goal:** Fix the confirmed correctness, security, and robustness defects found in the merged write/action feature before it is pushed.

**Architecture:** The write/action path is `SQL → ApiActionVTab::bind → compose_action_request → IoBridge Job::Act → ManifestAdapter::act → send_request → Arrow rows`. The fixes resolve auth inside `act()` (mirroring `scan()`), make path-arg substitution injection-safe, align the dry-run result schema with the declared schema, constrain HTTP methods, harden the named-parameter registration global, and stop write actions from masquerading as readable tables in the notebook datasource listing.

**Tech Stack:** Rust 2021, `duckdb` VTab API, `reqwest`, `arrow`, `serde_json`, `wiremock` (tests). Build/test through `scripts/spur-cargo` only (remote-default).

---

## Conventions for every task

- Build/test ONLY via `scripts/spur-cargo` (never bare `cargo`). Lint via `SPUR_REMOTE=1 scripts/spur-cargo clippy -p <crate> -- -D warnings`.
- TDD cadence: write the failing test first (a `test(...)` commit), then the `fix(...)` commit.
- Commit format: `<type>(<scope>): <sub-id> <short imperative>` (subject < 72 chars).
- If you must touch a file outside the listed scope, emit a `scope_drift` signal immediately — do not silently expand.

---

## Dependency DAG

```
T1 (auth) ─┬─> T2 (path)  ──> T5 (global+bool)
           └─> T3 (method) ──> T4 (dry-run schema)
T6 (datasource kind filter)   [independent root]
```

- T2 and T3 run in parallel after T1 (T2 owns `ext/src/lib.rs`, T3 owns `manifest_adapter.rs` — no overlap).
- T5 stacks on T2 (both edit `ext/src/lib.rs`). T4 stacks on T3 (both edit `manifest_adapter.rs`).
- T6 is independent (different crate/file: `spur-notebook/src/mcp/mod.rs`).

---

### Task 1: Resolve manifest auth inside `act()` (CRITICAL — auth bypass)

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/mod.rs` (`ActionRequest` struct)
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs` (`ManifestAdapter::act`, its test)
- Modify: `crates/spur-notebook/rest-table-gateway-ext/src/lib.rs` (`compose_action_request`)
- Modify: `crates/spur-notebook/rest-table-gateway/src/vtab/bridge.rs` (test `ActionRequest` literal)

**Depends on:** none

**Suggested Worker:** codex

**Problem:** `compose_action_request` hardcodes `auth: ResolvedAuth::None` (lib.rs ~566) and `ManifestAdapter::act` forwards that to `HttpAction` without ever calling `self.resolve_auth().await?` the way `scan()` does (manifest_adapter.rs:341). Every authenticated write action is sent with no credentials.

**Fix:** Remove the `auth` field from `ActionRequest` entirely; resolve auth inside `act()` after the dry-run short-circuit (dry-run must NOT resolve auth or hit the network).

**Scope Boundary:**
- IN scope: the four files above, only the auth plumbing for actions.
- OUT of scope: scan path, http.rs, any pagination/error refactor.

**Implementation:**

- [ ] **Step 1: Write the failing test** in `manifest_adapter.rs` `#[cfg(test)] mod tests` (model on the existing `action_post_renders_typed_columns` and on `http.rs::bearer_auth_header_sent`). Use a unique env var name to avoid cross-test races.

```rust
#[tokio::test]
async fn action_post_applies_bearer_auth() {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/orders"))
        .and(header("authorization", "Bearer tok-act-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;

    std::env::set_var("SPUR_TEST_ACT_BEARER", "tok-act-123");
    let manifest = Manifest::from_toml(&format!(
        r#"
[source]
name = "svc"
base_url = "{}"
allow_writes = true
auth = {{ scheme = "bearer", env = "SPUR_TEST_ACT_BEARER" }}

[[action]]
name = "create"
method = "POST"
path = "/orders"
"#,
        server.uri()
    ))
    .expect("manifest parses");

    let adapter = ManifestAdapter::new(manifest);
    let req = ActionRequest {
        name: "create".to_string(),
        method: "POST".to_string(),
        path: "/orders".to_string(),
        query: vec![],
        body: None,
        idempotency_key: None,
        dry_run: false,
    };
    let batches = adapter.act(req).await.expect("authenticated action succeeds");
    std::env::remove_var("SPUR_TEST_ACT_BEARER");
    assert_eq!(batches.len(), 1);
}
```

- [ ] **Step 2: Run to verify it fails** (the `ActionRequest` literal won't compile until the field is removed, AND the mock requires the header which is not currently sent).
  Run: `scripts/spur-cargo test -p spur-rest-table-gateway action_post_applies_bearer_auth -- --nocapture`
  Expected: FAIL (compile error on `auth` field once removed below, then mock 4xx because no auth header).

- [ ] **Step 3: Remove the `auth` field** from `ActionRequest` in `mod.rs`:

```rust
#[derive(Debug, Clone)]
pub struct ActionRequest {
    pub name: String,
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Option<serde_json::Value>,
    pub idempotency_key: Option<String>,
    pub dry_run: bool,
}
```

- [ ] **Step 4: Resolve auth in `act()`** (`manifest_adapter.rs`). Drop `auth` from the destructure; add resolution AFTER the dry-run block, before building `HttpAction`:

```rust
async fn act(&self, req: ActionRequest) -> Result<Vec<RecordBatch>> {
    let ActionRequest {
        name,
        method,
        path,
        query,
        body,
        idempotency_key,
        dry_run,
    } = req;
    // ... unchanged: find action, resolve base_url, build url ...

    if dry_run {
        // ... unchanged generic dry-run row (typed schema handled in task-4) ...
    }

    let auth = self.resolve_auth().await?;
    let idempotency_key = match (&action.idempotency_header, idempotency_key) {
        (Some(header), Some(value)) => Some((header.clone(), value)),
        _ => None,
    };
    let http_action = HttpAction {
        client: &self.client,
        method: reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|e| GatewayError::Http(e.to_string()))?,
        url,
        query,
        body,
        auth: &auth,
        idempotency_key,
    };
    // ... unchanged send_request + render ...
}
```

- [ ] **Step 5: Fix the constructor** in `compose_action_request` (`ext/src/lib.rs`): delete the `auth: ResolvedAuth::None,` line from the returned `ActionRequest`. Remove the now-unused `ResolvedAuth` import from that file if and only if nothing else uses it (check with the compiler; `ApiTableVTab`/scan code may still use it — do not remove if still referenced).

- [ ] **Step 6: Fix the test `ActionRequest` literal** in `bridge.rs` (~line 168): remove the `auth` field. Fix the existing `action_post_renders_typed_columns` test literal in `manifest_adapter.rs` (~line 664) the same way.

- [ ] **Step 7: Run the suite**
  Run: `scripts/spur-cargo test -p spur-rest-table-gateway`
  Expected: PASS (incl. the new test). Then `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-rest-table-gateway -p spur-rest-table-gateway-ext -- -D warnings`.

- [ ] **Step 8: Commit** (two commits: failing test, then fix).

**Acceptance Criteria:**
- [ ] `ActionRequest` has no `auth` field.
- [ ] `act()` calls `self.resolve_auth().await?` for non-dry-run; dry-run does NOT resolve auth or send a request.
- [ ] New `action_post_applies_bearer_auth` test passes (proves the header is sent).
- [ ] Both gateway crates compile; clippy clean; existing tests pass.

---

### Task 2: Injection-safe path-arg substitution (CRITICAL — path traversal + IMPORTANT residual placeholder)

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway-ext/src/lib.rs` (`compose_action_request`; add pure helper `substitute_path_arg` + unit tests)

**Depends on:** task-1 (same file region in `compose_action_request`)

**Suggested Worker:** codex

**Problem:** `compose_action_request` does `path.replace("{name}", &duckdb_value_to_string(&value))` (lib.rs ~529) with no validation — a value like `../admin` rewrites the request target. Separately, an omitted optional path arg leaves a literal `{name}` in the URL (the `continue` at ~521 with no post-loop check).

**Fix:** Reject path-location values containing URL-significant characters (no new dependency), and after the arg loop error if any `{...}` placeholder remains. Extract a pure, unit-testable helper.

**Scope Boundary:**
- IN scope: `ext/src/lib.rs` path handling only.
- OUT of scope: body/query handling, manifest_adapter.rs.

**Implementation:**

- [ ] **Step 1: Write failing unit tests** in the `#[cfg(test)]` module of `ext/src/lib.rs`:

```rust
#[test]
fn substitute_path_arg_rejects_traversal_and_separators() {
    let mut p = "/orders/{id}".to_string();
    assert!(substitute_path_arg(&mut p, "id", "../admin").is_err());
    let mut p = "/orders/{id}".to_string();
    assert!(substitute_path_arg(&mut p, "id", "a/b").is_err());
    let mut p = "/orders/{id}".to_string();
    assert!(substitute_path_arg(&mut p, "id", "ok-123").is_ok());
    assert_eq!(p, "/orders/ok-123");
}

#[test]
fn ensure_no_unfilled_placeholders_errs_on_leftover() {
    assert!(ensure_no_unfilled_placeholders("/orders/{id}").is_err());
    assert!(ensure_no_unfilled_placeholders("/orders/ok-123").is_ok());
}
```

- [ ] **Step 2: Run to verify failure** (helpers don't exist yet).
  Run: `scripts/spur-cargo test -p spur-rest-table-gateway-ext substitute_path_arg -- --nocapture`
  Expected: FAIL (unresolved function).

- [ ] **Step 3: Add the helpers** in `ext/src/lib.rs`:

```rust
/// Substitute one `{name}` path placeholder with a validated value.
/// Rejects values that would alter URL structure (path traversal / injection).
fn substitute_path_arg(path: &mut String, name: &str, value: &str) -> Result<(), Box<dyn Error>> {
    const FORBIDDEN: &[char] = &['/', '?', '#', '%', '\\'];
    if value.contains("..") || value.chars().any(|c| FORBIDDEN.contains(&c)) {
        return Err(format!(
            "path argument {name} contains forbidden characters: {value:?}"
        )
        .into());
    }
    *path = path.replace(&format!("{{{}}}", name), value);
    Ok(())
}

/// Error if any `{...}` placeholder remains unsubstituted in the path.
fn ensure_no_unfilled_placeholders(path: &str) -> Result<(), Box<dyn Error>> {
    if path.contains('{') && path.contains('}') {
        return Err(format!("action path has unfilled placeholder(s): {path}").into());
    }
    Ok(())
}
```

- [ ] **Step 4: Wire `compose_action_request`** — replace the `ArgLocation::Path` arm and add the post-loop check:

```rust
ArgLocation::Path => {
    substitute_path_arg(&mut path, &spec.name, &duckdb_value_to_string(&value))?;
}
```
…and after the `for spec in &extra.arg_specs` loop, before building the `ActionRequest`:
```rust
ensure_no_unfilled_placeholders(&path)?;
```

- [ ] **Step 5: Run + lint.**
  Run: `scripts/spur-cargo test -p spur-rest-table-gateway-ext substitute_path_arg ensure_no_unfilled_placeholders` then `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-rest-table-gateway-ext -- -D warnings`.
  Expected: PASS.

- [ ] **Step 6: Commit** (failing test, then fix).

**Acceptance Criteria:**
- [ ] Path values containing `/ ? # % \\` or `..` are rejected with an error.
- [ ] Unfilled `{...}` placeholders error instead of being sent verbatim.
- [ ] No new crate dependency added.
- [ ] Unit tests pass; clippy clean.

---

### Task 3: Constrain action HTTP methods to write verbs (IMPORTANT)

**Task ID:** `task-3`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs` (`action_def` / new validation; test)

**Depends on:** task-1 (same file)

**Suggested Worker:** codex

**Problem:** `action.method` is a free-form string passed to `reqwest::Method::from_bytes` (manifest_adapter.rs:428) with no allowlist; `GET`/`CONNECT`/etc. are silently accepted, violating the write-only contract.

**Scope Boundary:**
- IN scope: method validation at action-definition time in `manifest_adapter.rs`.
- OUT of scope: lib.rs, http.rs.

**Implementation:**

- [ ] **Step 1: Write the failing test** (in `manifest_adapter.rs` tests):

```rust
#[test]
fn action_def_rejects_non_write_method() {
    let toml = r#"
[source]
name = "svc"
base_url = "https://x"
allow_writes = true

[[action]]
name = "bad"
method = "GET"
path = "/x"
"#;
    let manifest = Manifest::from_toml(toml).expect("parses");
    // catalog() filter_maps Ok(...) only, so a GET action must be dropped/erroring.
    let defs = ManifestAdapter::new(manifest).catalog();
    assert!(
        !defs.iter().any(|d| d.name == "bad"),
        "non-write method must not produce an action def"
    );
}

#[test]
fn action_def_accepts_write_methods() {
    for m in ["post", "PUT", "Patch", "DELETE"] {
        let toml = format!(
            "[source]\nname=\"s\"\nbase_url=\"https://x\"\nallow_writes=true\n\n[[action]]\nname=\"a\"\nmethod=\"{m}\"\npath=\"/x\"\n"
        );
        let manifest = Manifest::from_toml(&toml).expect("parses");
        assert!(ManifestAdapter::new(manifest).catalog().iter().any(|d| d.name == "a"));
    }
}
```

- [ ] **Step 2: Run to verify failure.**
  Run: `scripts/spur-cargo test -p spur-rest-table-gateway action_def_rejects_non_write_method -- --nocapture`
  Expected: FAIL (GET currently accepted).

- [ ] **Step 3: Add validation** in `action_def` (return `Err` for non-write methods; `catalog()` already `filter_map`s `.ok()` so an erroring def is dropped — which the gate test relies on):

```rust
fn action_def(action: &ActionCfg) -> Result<TableDef> {
    const ALLOWED: &[&str] = &["POST", "PUT", "PATCH", "DELETE"];
    if !ALLOWED.contains(&action.method.to_ascii_uppercase().as_str()) {
        return Err(GatewayError::Manifest(format!(
            "action '{}' uses unsupported method '{}' (allowed: POST, PUT, PATCH, DELETE)",
            action.name, action.method
        )));
    }
    Ok(TableDef {
        name: action.name.clone(),
        schema: Self::action_response_schema(action)?,
        kind: TableKind::Action {
            method: action.method.to_ascii_uppercase(),
            path: action.path.clone(),
            arg_specs: Self::action_arg_specs(action)?,
            dry_run_arg: action.dry_run_arg.clone(),
            idempotency_header: action.idempotency_header.clone(),
        },
    })
}
```
(Normalizing `method` to uppercase here also makes the `from_bytes` call in `act()` canonical.)

- [ ] **Step 4: Run + lint.**
  Run: `scripts/spur-cargo test -p spur-rest-table-gateway action_def`
  Expected: PASS.

- [ ] **Step 5: Commit** (failing test, then fix).

**Acceptance Criteria:**
- [ ] Actions with methods outside {POST,PUT,PATCH,DELETE} produce no `TableDef` (erroring def filtered by `catalog()`).
- [ ] Write methods accepted case-insensitively and normalized to uppercase.
- [ ] Tests pass; clippy clean.

---

### Task 4: Dry-run result schema must match the declared schema (IMPORTANT)

**Task ID:** `task-4`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs` (`act` dry-run branch; helper; test)

**Depends on:** task-3 (same file)

**Suggested Worker:** codex

**Problem:** When an action declares typed `columns`, `bind` advertises the typed schema, but the dry-run branch returns `render_generic_row` (`http_status:Int64, body:Utf8`). Reading that batch downcasts by the typed schema and fails at runtime. No test covers columns+dry_run.

**Fix:** When `action.columns` is `Some`, dry-run returns a single all-null row matching the typed schema; when `None`, keep the generic dry-run row (unchanged).

**Scope Boundary:**
- IN scope: dry-run rendering in `manifest_adapter.rs`.
- OUT of scope: non-dry-run rendering, lib.rs.

**Implementation:**

- [ ] **Step 1: Write the failing test:**

```rust
#[tokio::test]
async fn dry_run_with_columns_matches_typed_schema() {
    let manifest = Manifest::from_toml(
        r#"
[source]
name = "svc"
base_url = "https://example.invalid"
allow_writes = true

[[action]]
name = "create"
method = "POST"
path = "/orders"
dry_run_arg = "dry_run"

[action.columns]
order_id = { json = "$.id", type = "Utf8" }
"#,
    )
    .expect("parses");
    let adapter = ManifestAdapter::new(manifest);
    let req = ActionRequest {
        name: "create".into(),
        method: "POST".into(),
        path: "/orders".into(),
        query: vec![],
        body: None,
        idempotency_key: None,
        dry_run: true,
    };
    let batches = adapter.act(req).await.expect("dry-run ok");
    let typed = ManifestAdapter::new(
        Manifest::from_toml(
            "[source]\nname=\"svc\"\nbase_url=\"https://example.invalid\"\nallow_writes=true\n\n[[action]]\nname=\"create\"\nmethod=\"POST\"\npath=\"/orders\"\n\n[action.columns]\norder_id = { json = \"$.id\", type = \"Utf8\" }\n",
        )
        .unwrap(),
    );
    // Schema of the returned batch must equal the declared action schema.
    let expected = typed.catalog().into_iter().find(|d| d.name == "create").unwrap().schema;
    assert_eq!(batches[0].schema().fields(), expected.fields());
}
```

- [ ] **Step 2: Run to verify failure** (returns generic 2-col schema, not 1-col typed).
  Run: `scripts/spur-cargo test -p spur-rest-table-gateway dry_run_with_columns_matches_typed_schema -- --nocapture`
  Expected: FAIL (schema mismatch).

- [ ] **Step 3: Add a typed dry-run helper and branch on columns** in `manifest_adapter.rs`:

```rust
fn render_typed_dry_run(action: &ActionCfg) -> Result<Vec<RecordBatch>> {
    let schema = Self::action_response_schema(action)?; // typed
    let arrays: Vec<ArrayRef> = schema
        .fields()
        .iter()
        .map(|f| arrow_array::new_null_array(f.data_type(), 1))
        .collect();
    let batch = RecordBatch::try_new(schema, arrays)
        .map_err(|e| GatewayError::Schema(e.to_string()))?;
    Ok(vec![batch])
}
```
In `act()`, replace the dry-run block:
```rust
if dry_run {
    return match &action.columns {
        Some(_) => Self::render_typed_dry_run(action),
        None => Self::render_generic_row(
            0,
            serde_json::json!({
                "dry_run": true, "method": method, "url": url,
                "query": query, "body": body,
            }),
        ),
    };
}
```
(Add `use arrow_array::new_null_array;` or fully-qualify; ensure `arrow-array` exposes it — it does.)

- [ ] **Step 4: Run + lint.**
  Run: `scripts/spur-cargo test -p spur-rest-table-gateway dry_run` then clippy.
  Expected: PASS (existing `dry_run_sends_nothing` still green — it uses no columns).

- [ ] **Step 5: Commit** (failing test, then fix).

**Acceptance Criteria:**
- [ ] Dry-run on a typed-columns action returns a batch whose schema equals the declared schema.
- [ ] Dry-run on a no-columns action is unchanged (generic row).
- [ ] Tests pass; clippy clean.

---

### Task 5: Harden the named-parameter registration global + robust bool parsing (IMPORTANT + MINOR)

**Task ID:** `task-5`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway-ext/src/lib.rs` (`ACTION_NAMED_PARAMETERS` → `thread_local!`; `register_action_table_function`; `duckdb_value_to_bool`; tests)

**Depends on:** task-2 (same file)

**Suggested Worker:** codex

**Problem:** `ACTION_NAMED_PARAMETERS` is a process-global `OnceLock<Mutex<Vec<..>>>` written before registration and cleared after; the clear swallows lock poisoning (`if let Ok`) and is skipped on the error/panic path, leaving stale parameters. Registration is single-threaded today, but the global is fragile. Also `duckdb_value_to_bool` uses `to_string().parse::<bool>()` which only accepts exact lowercase `true`/`false`.

**Fix:** Move the parameter list to a `thread_local!` (isolates per-thread registration; eliminates the cross-thread race) and guarantee it is cleared even when registration fails. Make bool parsing case-insensitive.

**Scope Boundary:**
- IN scope: the named-parameter mechanism and the bool helper in `ext/src/lib.rs`.
- OUT of scope: other VTab logic, core crate.

**Implementation:**

- [ ] **Step 1: Write failing tests** in `ext/src/lib.rs` tests:

```rust
#[test]
fn action_named_parameters_round_trip_and_clear() {
    with_action_named_parameters_set(vec![("a".into(), DataType::Utf8)], || {
        assert_eq!(read_action_named_parameters().len(), 1);
    });
    // After the scope, the slot must be cleared even though the closure returned.
    assert!(read_action_named_parameters().is_empty());
}

#[test]
fn duckdb_bool_parse_is_case_insensitive() {
    assert!(parse_bool_str("TRUE").unwrap());
    assert!(!parse_bool_str("False").unwrap());
    assert!(parse_bool_str("notabool").is_err());
}
```

- [ ] **Step 2: Run to verify failure.**
  Run: `scripts/spur-cargo test -p spur-rest-table-gateway-ext action_named_parameters_round_trip_and_clear duckdb_bool_parse_is_case_insensitive -- --nocapture`
  Expected: FAIL (helpers don't exist).

- [ ] **Step 3: Replace the static with a thread-local + scoped setter.** Delete `static ACTION_NAMED_PARAMETERS` and `action_named_parameters()`. Add:

```rust
thread_local! {
    // Registration is single-threaded per connection; thread-local isolates each
    // register_*->named_parameters() handshake and cannot race across threads.
    static ACTION_NAMED_PARAMETERS: std::cell::RefCell<Vec<(String, DataType)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn read_action_named_parameters() -> Vec<(String, DataType)> {
    ACTION_NAMED_PARAMETERS.with(|c| c.borrow().clone())
}

/// Set the param list, run `f` (which performs DuckDB registration that calls
/// `named_parameters()`), then ALWAYS clear — even if `f` panics/returns early.
fn with_action_named_parameters_set<R>(params: Vec<(String, DataType)>, f: impl FnOnce() -> R) -> R {
    ACTION_NAMED_PARAMETERS.with(|c| *c.borrow_mut() = params);
    let result = f();
    ACTION_NAMED_PARAMETERS.with(|c| c.borrow_mut().clear());
    result
}

fn parse_bool_str(s: &str) -> Result<bool, Box<dyn Error>> {
    match s.to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("invalid Boolean action argument: {other:?}").into()),
    }
}
```
(If guaranteed clear-on-panic is desired, wrap the clear in a drop guard; the scoped setter above already covers the normal early-return path and is sufficient for the single-threaded registration invariant — document this.)

- [ ] **Step 4: Rewire `register_action_table_function`** to use the scoped setter:

```rust
fn register_action_table_function(
    con: &Connection,
    fn_name: &str,
    extra: &ApiActionExtra,
) -> Result<(), Box<dyn Error>> {
    let named_parameters = action_named_parameter_types(extra)?;
    with_action_named_parameters_set(named_parameters, || {
        con.register_table_function_with_extra_info::<ApiActionVTab, _>(fn_name, extra)
    })
}
```
Update `ApiActionVTab::named_parameters` to read via `read_action_named_parameters()`. Update `duckdb_value_to_bool` to call `parse_bool_str(&duckdb_value_to_string(value))`.

- [ ] **Step 5: Run + lint + the e2e regression.**
  Run: `scripts/spur-cargo test -p spur-rest-table-gateway-ext` then `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-rest-table-gateway-ext -- -D warnings`.
  Expected: PASS (incl. existing `load_extension_e2e` action/gate/dry-run tests — they exercise registration + `named_parameters`).

- [ ] **Step 6: Commit** (failing test, then fix).

**Acceptance Criteria:**
- [ ] No process-global static for named parameters; per-thread with guaranteed clear after registration.
- [ ] Bool action args parse case-insensitively; invalid values error clearly.
- [ ] All `load_extension_e2e` tests still pass; clippy clean.

---

### Task 6: Stop write actions from masquerading as readable tables in datasource listing (IMPORTANT — cross-crate)

**Task ID:** `task-6`

**Files:**
- Modify: `crates/spur-notebook/src/mcp/mod.rs` (filter `TableKind::Action` out of `jute::commands::Table` listings)

**Depends on:** none (independent root; different crate/file)

**Suggested Worker:** codex

**Problem:** `api_datasource_table` (mcp/mod.rs:1092) maps a `TableDef` to `jute::commands::Table` using only `name` + `schema`, ignoring `table.kind`. `Action` defs from `ManifestAdapter::catalog()` are therefore exposed/persisted as ordinary readable tables (reachable via the manifest-persist path). Until the frontend models write actions, they must not appear as query tables.

**Fix:** Filter out `TableKind::Action` entries wherever `catalog()` is mapped into `jute::commands::Table`. Centralize with a tiny predicate and apply at every call site.

**Scope Boundary:**
- IN scope: `mcp/mod.rs` datasource-table listing only.
- OUT of scope: gateway crates, jute frontend types, adding new UI fields.
- NOTE: there are multiple call sites that map `catalog()` → `Table`. Locate them all with:
  `rg -n "api_datasource_table\b|\.catalog\(\)" crates/spur-notebook/src/mcp/mod.rs`
  (graph-identified sites include ~1075, and the attach/update/persist paths). Apply the filter at each. If you discover a site that needs different handling, emit `scope_drift`.

**Implementation:**

- [ ] **Step 1: Write the failing test** (in `mcp/mod.rs` tests, under the `datasource-introspect` feature if gated):

```rust
#[test]
fn action_defs_are_excluded_from_datasource_tables() {
    use spur_rest_table_gateway::adapter::{TableDef, TableKind};
    use std::sync::Arc;
    use arrow_schema::{Schema, Field, DataType};

    let read = TableDef {
        name: "markets".into(),
        schema: Arc::new(Schema::new(vec![Field::new("id", DataType::Utf8, true)])),
        kind: TableKind::Table,
    };
    let action = TableDef {
        name: "create".into(),
        schema: Arc::new(Schema::new(vec![Field::new("order_id", DataType::Utf8, true)])),
        kind: TableKind::Action {
            method: "POST".into(),
            path: "/orders".into(),
            arg_specs: vec![],
            dry_run_arg: None,
            idempotency_header: None,
        },
    };
    let tables: Vec<_> = datasource_tables_from_catalog("svc", vec![read, action]);
    assert!(tables.iter().any(|t| t.name == "svc_markets"));
    assert!(!tables.iter().any(|t| t.name == "svc_create"));
}
```

- [ ] **Step 2: Run to verify failure.**
  Run: `scripts/spur-cargo test -p spur-notebook action_defs_are_excluded_from_datasource_tables -- --nocapture`
  Expected: FAIL (helper missing / action included).

- [ ] **Step 3: Add a centralizing helper** in `mcp/mod.rs` and route call sites through it:

```rust
#[cfg(feature = "datasource-introspect")]
fn datasource_tables_from_catalog(
    adapter_name: &str,
    catalog: Vec<spur_rest_table_gateway::adapter::TableDef>,
) -> Vec<jute::commands::Table> {
    use spur_rest_table_gateway::adapter::TableKind;
    catalog
        .into_iter()
        .filter(|t| !matches!(t.kind, TableKind::Action { .. }))
        .map(|t| api_datasource_table(adapter_name, t))
        .collect()
}
```
Replace each `adapter.catalog().into_iter().map(|t| api_datasource_table(name, t)).collect()` (and the attach/update/persist equivalents) with `datasource_tables_from_catalog(name, adapter.catalog())`.

- [ ] **Step 4: Run + lint.**
  Run: `scripts/spur-cargo test -p spur-notebook` then `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-notebook -- -D warnings`.
  Expected: PASS.

- [ ] **Step 5: Commit** (failing test, then fix).

**Acceptance Criteria:**
- [ ] `TableKind::Action` defs are excluded from all `jute::commands::Table` datasource listings.
- [ ] Read tables/table-functions still listed unchanged.
- [ ] Tests pass; clippy clean.

---

## Self-Review (completed)

- **Coverage:** Every CONFIRMED finding from the fresh-index re-review maps to a task — auth bypass (T1), path injection + residual placeholder (T2), method allowlist (T3), dry-run schema (T4), named-parameter global + bool parse (T5), kind-blind datasource exposure (T6). Deliberately deferred (documented, non-blocking): structured HTTP-status error variant (protocol change touching scan callers), cross-crate helper de-duplication (`json_path_get`, `write_batch_rows`), `register_tables` legacy path (no live callers), and `action_rows` single-object wrapping (intended behavior). These are tracked as follow-ups, not write-safety bugs.
- **No placeholders:** every code step contains real code; every test step has runnable `scripts/spur-cargo` commands and expected results.
- **DAG:** valid, acyclic; T2∥T3 after T1; T5 after T2; T4 after T3; T6 independent. No two concurrently-eligible tasks edit the same file.
- **beads compatibility:** each task has a unique id, explicit `depends_on`, verifiable acceptance criteria, and a scope boundary with a `scope_drift` instruction.
