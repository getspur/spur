# REST Gateway Write-Gate + Placeholder-Guard Fixes Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** Graph-validated code-review findings (see review run 2026-06-04; artifact hash `465fa71a…`, both findings confirmed accurate against the fresh graph).
**Design epic:** n/a (two-fix follow-up to `2026-06-03-rest-gateway-write-actions-review-fixes`)

**Goal:** Close two validated defects in the REST table gateway write/action path: (1) `act()` has no `allow_writes` re-check, leaving a gap between the `pub` `IoBridge::call_act` surface and the catalog-gated registration; (2) `ensure_no_unfilled_placeholders` uses `&&` so half-bracket templates (`/orders/{id`) ship verbatim.

**Architecture:** Two independent, single-file, TDD fixes in two separate cargo workspaces. No shared files, no ordering dependency — fully parallel.

**Tech Stack:** Rust, tokio, wiremock (existing test deps).

---

### Task 1: `act()` allow_writes guard

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs` (`async fn act`, line 429; tests module from line 497)
- Test: same file, `#[cfg(test)] mod tests`

**Depends on:** none

**Acceptance Criteria:**
- [ ] A new failing-then-passing test proves `act()` returns `Err` when `allow_writes = false`, even called directly (bypassing `catalog()`).
- [ ] The guard is the first statement in `act()`, before the `ActionRequest` destructure / any auth resolution / any network call.
- [ ] All pre-existing `rest-table-gateway` lib tests still pass (the dry-run and action tests already set `allow_writes = true`).
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway` is green.

**Suggested Worker:** codex (single-file, mechanical)

**Scope Boundary:**
- IN scope: `manifest_adapter.rs` `act()` body + one new test in the same file's tests module.
- OUT of scope: `lib.rs` (ext crate), `bridge.rs`, `catalog()`, `http.rs`, the `Adapter` trait default. Do NOT add an `allow_writes` field to `IoBridge`. Do NOT change `catalog()`.
- If you discover you need to touch any OUT-OF-SCOPE file, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Write the failing test** (append to `mod tests` in `manifest_adapter.rs`):

```rust
    #[tokio::test]
    async fn act_rejects_when_writes_disabled() {
        // Models the direct-bypass path: a caller holding the adapter calls act()
        // without going through catalog()/vtab registration. The gate must still hold.
        let manifest = Manifest::from_toml(
            r#"
[source]
name = "svc"
base_url = "https://example.invalid"
allow_writes = false

[[action]]
name = "create"
method = "POST"
path = "/orders"

[action.args]
"#,
        )
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
        let err = adapter.act(req).await.expect_err("writes disabled must error");
        assert!(
            err.to_string().contains("writes"),
            "error should mention writes are disabled, got: {err}"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway act_rejects_when_writes_disabled -- --nocapture`
Expected: FAIL — `act()` currently proceeds past the missing guard (it will try to resolve auth / reach the network for `https://example.invalid` rather than returning the writes-disabled error).

- [ ] **Step 3: Add the guard** — insert as the FIRST statement inside `async fn act(&self, req: ActionRequest) -> Result<Vec<RecordBatch>> {` (manifest_adapter.rs:429), before `let ActionRequest { … } = req;`:

```rust
        if !self.manifest.source.allow_writes {
            return Err(GatewayError::Adapter(
                "writes are not enabled for this connection (set allow_writes = true)".to_string(),
            ));
        }
```

- [ ] **Step 4: Run the new test + full lib suite**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway -- --nocapture`
Expected: PASS — the new test passes and all pre-existing tests stay green (existing action/dry-run tests use `allow_writes = true`).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/manifest_adapter.rs
git commit -m "fix(rest-table-gateway): T1 gate act() on allow_writes"
```

---

### Task 2: half-bracket placeholder guard

**Task ID:** `task-2`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway-ext/src/lib.rs` (`fn ensure_no_unfilled_placeholders`, line 630; test at line 796)
- Test: same file, `#[cfg(test)] mod tests`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `ensure_no_unfilled_placeholders` errors on a lone `{` OR a lone `}` (not only when both are present).
- [ ] The existing test gains two half-bracket cases that fail before the fix and pass after.
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test --manifest-path crates/spur-notebook/rest-table-gateway-ext/Cargo.toml --lib` is green. (The ext is a SEPARATE cargo workspace — use `--manifest-path`, never `-p`.)

**Suggested Worker:** codex (one-line logic change + test)

**Scope Boundary:**
- IN scope: `ensure_no_unfilled_placeholders` (the `&&` → `||` change) and the `ensure_no_unfilled_placeholders_errs_on_leftover` test in the same file.
- OUT of scope: `substitute_path_arg`, `compose_action_request`, the lib crate, the e2e test file. Do NOT change the FORBIDDEN char set.
- If you need to touch any OUT-OF-SCOPE file, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Extend the failing test** — replace the body of `ensure_no_unfilled_placeholders_errs_on_leftover` (lib.rs:796) with:

```rust
    #[test]
    fn ensure_no_unfilled_placeholders_errs_on_leftover() {
        assert!(ensure_no_unfilled_placeholders("/orders/{id}").is_err());
        // Half-bracket templates must also be rejected (authoring footgun):
        assert!(ensure_no_unfilled_placeholders("/orders/{id").is_err());
        assert!(ensure_no_unfilled_placeholders("/orders/id}").is_err());
        assert!(ensure_no_unfilled_placeholders("/orders/ok-123").is_ok());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test --manifest-path crates/spur-notebook/rest-table-gateway-ext/Cargo.toml --lib ensure_no_unfilled_placeholders_errs_on_leftover -- --nocapture`
Expected: FAIL on the `/orders/{id` and `/orders/id}` asserts — current `&&` lets a lone brace through.

- [ ] **Step 3: Fix the condition** — in `fn ensure_no_unfilled_placeholders` (lib.rs:630), change `&&` to `||`:

```rust
fn ensure_no_unfilled_placeholders(path: &str) -> Result<(), Box<dyn Error>> {
    if path.contains('{') || path.contains('}') {
        return Err(format!("action path has unfilled placeholder(s): {path}").into());
    }
    Ok(())
}
```

- [ ] **Step 4: Run the test + lib suite**

Run: `SPUR_REMOTE=1 scripts/spur-cargo test --manifest-path crates/spur-notebook/rest-table-gateway-ext/Cargo.toml --lib -- --nocapture`
Expected: PASS — all four asserts hold and the rest of the ext unit tests stay green.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway-ext/src/lib.rs
git commit -m "fix(rest-gateway-ext): T2 reject half-bracket action paths"
```
