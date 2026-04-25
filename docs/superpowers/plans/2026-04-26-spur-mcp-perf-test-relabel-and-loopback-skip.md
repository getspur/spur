# spur-mcp: Perf-Test Relabel + Loopback-Bind Sandbox Skip — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Relabel `crates/spur-mcp/tests/mutation_pagination.rs` as a perf-regression fixture (no timing assertions) and add a shared `tests/common/mod.rs` loopback-bind probe so that integration tests calling `McpCallbackServer::start()` skip gracefully under sandboxes that deny loopback `bind(2)` with `EPERM`.

**Architecture:** Add one new file (`tests/common/mod.rs`) exposing `loopback_bindable()` (cached probe via `tokio::sync::OnceCell`) and a two-arm `skip_if_no_loopback!` macro. Each integration test that calls `.start()` adds `mod common;` plus one macro invocation at function entry. Four existing ad-hoc post-bind skips at `persisted_authority_flip.rs` (×2) and `reconciler_tick.rs` (×2) are migrated to the new pre-bind macro. The pagination test gets wrapped in `mod perf_regressions { ... }` with explicit named imports and the `t_v0d_5_` plan-ticket prefix dropped. No production-code changes anywhere.

**Tech Stack:** Rust 2021 edition, `tokio` 1.x (workspace dep with `full` features), `tokio::sync::OnceCell`, `#[macro_export]`, integration test target conventions (Cargo book §integration-tests).

**Spec:** `docs/superpowers/specs/2026-04-26-spur-mcp-perf-test-relabel-and-loopback-skip-design.md` (commit `d1c7ca5`).

**Authoritative wiring grep (must rerun before Task 2):** `rg '\.start\(\)' crates/spur-mcp/tests/`

---

## File Map

**Create:**
- `crates/spur-mcp/tests/common/mod.rs` — shared sandbox-skip helper + macro

**Modify:**
- `crates/spur-mcp/tests/rmcp_streamable_http.rs` — add `mod common;` + binary-arm macro
- `crates/spur-mcp/tests/server_start_pidfile.rs` — add `mod common;` + unary macro to one of two tests
- `crates/spur-mcp/tests/persisted_authority_flip.rs` — add `mod common;` + migrate 2 ad-hoc skips
- `crates/spur-mcp/tests/reconciler_tick.rs` — add `mod common;` + migrate 2 ad-hoc skips
- `crates/spur-mcp/tests/mutation_pagination.rs` — relabel as perf-regression fixture, wrap test in `mod perf_regressions`, drop `t_v0d_5_` prefix

**No `Cargo.toml` changes.** `tokio` is already a workspace dev-dep with `full` features; `OnceCell` is in `tokio::sync` and is reachable. No `[[test]]` entries needed; `tests/common/mod.rs` is auto-recognized by Cargo as a shared submodule (not its own integration binary) per Cargo book §cargo-targets.

---

## Task 1: Land the shared sandbox-skip helper

**Files:**
- Create: `crates/spur-mcp/tests/common/mod.rs`

This task is the foundation; no consumers wired yet. Per the spec, Cargo will not compile this file until at least one integration test declares `mod common;`, so there will be no "module never used" warning at this stage.

- [ ] **Step 1: Verify the directory does not exist yet**

```bash
ls crates/spur-mcp/tests/common 2>&1
```

Expected: `ls: crates/spur-mcp/tests/common: No such file or directory` (or similar — directory must not pre-exist).

- [ ] **Step 2: Create the helper module**

Write the file `crates/spur-mcp/tests/common/mod.rs` with exactly this content:

```rust
//! Shared helpers for spur-mcp integration tests.
//!
//! Currently only hosts the loopback-bind probe used by tests that exercise
//! `McpCallbackServer::start()`. Some sandboxes (seccomp profiles, restricted
//! container runtimes) deny loopback `bind(2)` with EPERM. Tests that touch
//! the listener skip gracefully in that environment rather than hard-failing.

use tokio::net::TcpListener;
use tokio::sync::OnceCell;

static LOOPBACK_BINDABLE: OnceCell<bool> = OnceCell::const_new();

/// Probe `127.0.0.1:0` at most once per test binary. The result is cached
/// after up to 3 bind attempts so that a transient port-exhaustion blip on a
/// healthy host does not permanently latch the binary into skip mode. EPERM
/// in a sandbox is immediate, so the retry budget costs only a few
/// microseconds in the failure path.
pub async fn loopback_bindable() -> bool {
    *LOOPBACK_BINDABLE
        .get_or_init(|| async {
            for _ in 0..3 {
                if TcpListener::bind("127.0.0.1:0").await.is_ok() {
                    return true;
                }
            }
            false
        })
        .await
}

/// Skip the current test (printing a sandbox note) when loopback bind is denied.
///
/// Use the unary form for `async fn name()` and the binary form for tests with
/// non-`()` return shapes (e.g. `Result<(), Box<dyn Error>>`), passing the
/// expression to early-return: `skip_if_no_loopback!("name", Ok(()));`.
#[macro_export]
macro_rules! skip_if_no_loopback {
    ($name:expr) => {
        if !$crate::common::loopback_bindable().await {
            eprintln!(
                "skipping {}: loopback TCP bind denied (sandbox/seccomp)",
                $name
            );
            return;
        }
    };
    ($name:expr, $ret:expr) => {
        if !$crate::common::loopback_bindable().await {
            eprintln!(
                "skipping {}: loopback TCP bind denied (sandbox/seccomp)",
                $name
            );
            return $ret;
        }
    };
}
```

- [ ] **Step 3: Verify the file compiles in isolation by running the existing test suite**

Cargo will not compile `tests/common/mod.rs` until at least one consumer declares `mod common;`. So this step verifies the *crate as a whole* still compiles cleanly with the new file present and unreferenced.

```bash
cargo build -p spur-mcp --tests 2>&1 | tail -20
```

Expected: `Finished` (or similar success line). No errors. No warnings about `tests/common/mod.rs` (it's not compiled).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/tests/common/mod.rs
git commit -m "test(spur-mcp): add shared loopback-bind sandbox-skip helper

Adds tests/common/mod.rs exposing loopback_bindable() (probe cached via
tokio::sync::OnceCell with a 3-attempt retry budget) and a two-arm
skip_if_no_loopback! macro. No consumers wired yet; Cargo treats the
file as a shared submodule so it is not compiled until a test declares
mod common;.

Refs: docs/superpowers/specs/2026-04-26-spur-mcp-perf-test-relabel-and-loopback-skip-design.md"
```

---

## Task 2: Wire `rmcp_streamable_http.rs` (pilot — binary macro arm)

**Files:**
- Modify: `crates/spur-mcp/tests/rmcp_streamable_http.rs`

This is the only test in the project that returns `Result<(), Box<dyn std::error::Error>>` from a loopback-touching `#[tokio::test]`. It exercises the **binary arm** of the macro, validating that arm before the unary arm is fanned out to other files.

- [ ] **Step 1: Verify the test currently exists with the expected signature**

```bash
sed -n '14,17p' crates/spur-mcp/tests/rmcp_streamable_http.rs
```

Expected output:
```
#[tokio::test]
async fn rmcp_client_can_initialize_list_tools_and_call_tool(
) -> Result<(), Box<dyn std::error::Error>> {
    let brain_sid = BrainSessionId::new(SessionId::new());
```

If the function name has changed, update Step 3 below to match before applying.

- [ ] **Step 2: Verify there is currently no `mod common;` declaration**

```bash
grep -n "^mod common" crates/spur-mcp/tests/rmcp_streamable_http.rs || echo "no mod common (expected)"
```

Expected: `no mod common (expected)`.

- [ ] **Step 3: Add `mod common;` and the macro invocation**

Apply two edits to `crates/spur-mcp/tests/rmcp_streamable_http.rs`:

**Edit A** — insert `mod common;` after the existing `use` block. Find:

```rust
use std::sync::Arc;

use rmcp::{model::CallToolRequestParams, transport::StreamableHttpClientTransport, ServiceExt};
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::{server::DetachedContinuationCtx, McpCallbackServer, WorkerInfo};
```

Replace with:

```rust
use std::sync::Arc;

use rmcp::{model::CallToolRequestParams, transport::StreamableHttpClientTransport, ServiceExt};
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::{server::DetachedContinuationCtx, McpCallbackServer, WorkerInfo};

mod common;
```

**Edit B** — insert the macro call as the first line inside the `async fn` body. Find:

```rust
async fn rmcp_client_can_initialize_list_tools_and_call_tool(
) -> Result<(), Box<dyn std::error::Error>> {
    let brain_sid = BrainSessionId::new(SessionId::new());
```

Replace with:

```rust
async fn rmcp_client_can_initialize_list_tools_and_call_tool(
) -> Result<(), Box<dyn std::error::Error>> {
    skip_if_no_loopback!(
        "rmcp_client_can_initialize_list_tools_and_call_tool",
        Ok(())
    );
    let brain_sid = BrainSessionId::new(SessionId::new());
```

- [ ] **Step 4: Verify compilation**

```bash
cargo build -p spur-mcp --test rmcp_streamable_http 2>&1 | tail -10
```

Expected: `Finished` with no errors. If the build fails with `cannot find macro skip_if_no_loopback`, double-check that `mod common;` was added at file scope (not inside a function or another module).

- [ ] **Step 5: Run the test on a host where loopback bind succeeds**

```bash
cargo test -p spur-mcp --test rmcp_streamable_http -- --nocapture 2>&1 | tail -20
```

Expected: `test result: ok. 1 passed; 0 failed`. Crucially, the output must NOT contain `skipping rmcp_client_can_initialize_list_tools_and_call_tool: loopback TCP bind denied` (the cached `true` path is taken).

- [ ] **Step 6: Verify the skip path renders correctly via static analysis**

Without an actual sandbox we can't trigger the skip path, but we can confirm the macro expanded as expected:

```bash
cargo expand -p spur-mcp --test rmcp_streamable_http 2>&1 | grep -A 4 "loopback_bindable" | head -10
```

Expected: shows `if !crate::common::loopback_bindable().await { eprintln!(...); return Ok(()); }` near the top of the test body. (If `cargo expand` is not installed, skip this step — it's confirmatory, not gating.)

- [ ] **Step 7: Commit**

```bash
git add crates/spur-mcp/tests/rmcp_streamable_http.rs
git commit -m "test(spur-mcp): wire rmcp_streamable_http to skip_if_no_loopback (binary arm)

Pilots the new pre-bind probe in the only loopback-touching test that
returns Result<(), Box<dyn Error>>. Validates the binary macro arm
(takes an explicit return expression) before the unary arm is fanned
out to remaining tests.

Refs: docs/superpowers/specs/2026-04-26-spur-mcp-perf-test-relabel-and-loopback-skip-design.md"
```

---

## Task 3: Wire `server_start_pidfile.rs` — only the listener-using test

**Files:**
- Modify: `crates/spur-mcp/tests/server_start_pidfile.rs`

This file has TWO `#[tokio::test]` functions that call `.start()`, but only ONE needs the macro:

- ✅ `dropping_server_handle_releases_pidfile_for_next_start` (line 90) — needs the macro.
- ❌ `beads_backed_start_requires_repo_root_before_listener_boot` (line 57) — **must NOT receive the macro**. It deliberately tests the pre-bind `repo_root` invariant at `crates/spur-mcp/src/server.rs:1898–1914`, which fires *before* `TcpListener::bind` at line 1933. The test passes in a sandbox without modification because it never reaches the bind.

- [ ] **Step 1: Verify the two test signatures**

```bash
grep -nE "^async fn|#\[tokio::test\]" crates/spur-mcp/tests/server_start_pidfile.rs | head -10
```

Expected:
```
56:#[tokio::test]
57:async fn beads_backed_start_requires_repo_root_before_listener_boot() {
89:#[tokio::test]
90:async fn dropping_server_handle_releases_pidfile_for_next_start() {
```

If line numbers or names differ, halt and re-resolve from grep before continuing.

- [ ] **Step 2: Add `mod common;` after the file's `use` block**

Find the last `use` statement near the top of the file (search for the line ending with `use std::time::Duration;` or similar — verify with):

```bash
grep -nE "^(use|mod) " crates/spur-mcp/tests/server_start_pidfile.rs | head -20
```

Insert `mod common;` immediately after the last top-level `use` declaration and before the first `fn` declaration.

- [ ] **Step 3: Add the macro call inside `dropping_server_handle_releases_pidfile_for_next_start`**

Find the existing function body. The current first lines are:

```rust
async fn dropping_server_handle_releases_pidfile_for_next_start() {
    if !br_available() {
        eprintln!(
            "skipping dropping_server_handle_releases_pidfile_for_next_start: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
```

Replace with:

```rust
async fn dropping_server_handle_releases_pidfile_for_next_start() {
    if !br_available() {
        eprintln!(
            "skipping dropping_server_handle_releases_pidfile_for_next_start: `br` not on PATH"
        );
        return;
    }
    skip_if_no_loopback!("dropping_server_handle_releases_pidfile_for_next_start");

    let dir = TempDir::new().expect("tempdir");
```

The macro is placed AFTER the `br_available()` skip (cheap, non-side-effecting check that should still run) and BEFORE the first side effect (`TempDir::new()`).

- [ ] **Step 4: Confirm the OTHER test was NOT modified**

```bash
sed -n '56,65p' crates/spur-mcp/tests/server_start_pidfile.rs
```

Expected: `beads_backed_start_requires_repo_root_before_listener_boot` retains its original body — no `skip_if_no_loopback!` line. If a macro call was added here by mistake, remove it and re-verify.

- [ ] **Step 5: Compile and run**

```bash
cargo test -p spur-mcp --test server_start_pidfile -- --nocapture 2>&1 | tail -20
```

Expected: `test result: ok. 2 passed`. Both tests still pass on a normal host. The output must NOT contain `skipping dropping_server_handle_releases_pidfile_for_next_start: loopback TCP bind denied` (cached `true` path).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-mcp/tests/server_start_pidfile.rs
git commit -m "test(spur-mcp): wire dropping_server_handle skip to loopback macro

Adds skip_if_no_loopback! to dropping_server_handle_releases_pidfile_for_next_start
only. The sibling test beads_backed_start_requires_repo_root_before_listener_boot
deliberately exercises the pre-bind repo_root invariant and intentionally does
not get the macro — its assertion fires before TcpListener::bind so it works
in any sandbox.

Refs: docs/superpowers/specs/2026-04-26-spur-mcp-perf-test-relabel-and-loopback-skip-design.md"
```

---

## Task 4: Migrate `persisted_authority_flip.rs` (2 ad-hoc skips → pre-bind macro)

**Files:**
- Modify: `crates/spur-mcp/tests/persisted_authority_flip.rs`

Two tests call `.start()` and currently have post-bind ad-hoc skip patterns. Each migrates to a single pre-bind macro call.

- Test `t_v0c_10_startup_reclaims_mid_plan_and_continues_dispatch` (line 655) — `.start()` at line 688, ad-hoc skip at lines 691–700.
- Test `t_v0c_11_startup_reclaim_clears_stale_dispatch_before_redispatch` (line 714) — `.start()` at line 755, ad-hoc skip at lines 758–767.

- [ ] **Step 1: Confirm current state**

```bash
grep -nE "Failed to bind TCP listener|^async fn t_v0c_1[01]" crates/spur-mcp/tests/persisted_authority_flip.rs
```

Expected: lists both function declarations and both `Failed to bind TCP listener` matches. If line numbers have shifted, the patterns below may need offsetting.

- [ ] **Step 2: Add `mod common;` after the file's top-level `use` block**

Find the existing top-of-file `use` block ending around the imports for `tokio::sync::Notify` and similar. Verify with:

```bash
grep -nE "^(use|mod) " crates/spur-mcp/tests/persisted_authority_flip.rs | head -20
```

Insert `mod common;` immediately after the final top-level `use` declaration and before the first `fn`.

- [ ] **Step 3: Migrate `t_v0c_10_startup_reclaims_mid_plan_and_continues_dispatch`**

Find the entry of the test, currently:

```rust
async fn t_v0c_10_startup_reclaims_mid_plan_and_continues_dispatch() {
    if !br_available() {
        eprintln!(
            "skipping t_v0c_10_startup_reclaims_mid_plan_and_continues_dispatch: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
```

Replace with:

```rust
async fn t_v0c_10_startup_reclaims_mid_plan_and_continues_dispatch() {
    if !br_available() {
        eprintln!(
            "skipping t_v0c_10_startup_reclaims_mid_plan_and_continues_dispatch: `br` not on PATH"
        );
        return;
    }
    skip_if_no_loopback!("t_v0c_10_startup_reclaims_mid_plan_and_continues_dispatch");

    let dir = TempDir::new().expect("tempdir");
```

Then find the post-bind ad-hoc skip, currently:

```rust
    let server = Arc::new(server);
    let started = Arc::clone(&server).start().await;
    let (_url, handle) = match started {
        Ok(started) => started,
        Err(error) => {
            let message = format!("{error:#}");
            if message.contains("Failed to bind TCP listener") {
                eprintln!(
                    "skipping t_v0c_10_startup_reclaims_mid_plan_and_continues_dispatch: {message}"
                );
                return;
            }
            panic!("start server: {message}");
        }
    };
```

Replace with:

```rust
    let server = Arc::new(server);
    let (_url, handle) = Arc::clone(&server)
        .start()
        .await
        .expect("start server (loopback bind already probed at fn entry)");
```

- [ ] **Step 4: Migrate `t_v0c_11_startup_reclaim_clears_stale_dispatch_before_redispatch`**

Same pattern as Step 3, applied to the second test. At the fn entry, after the existing `br_available()` check, insert:

```rust
    skip_if_no_loopback!("t_v0c_11_startup_reclaim_clears_stale_dispatch_before_redispatch");
```

Then find the post-bind ad-hoc skip near `.start()` (around line 755), currently:

```rust
    let server = Arc::new(server);
    let started = Arc::clone(&server).start().await;
    let (_url, handle) = match started {
        Ok(started) => started,
        Err(error) => {
            let message = format!("{error:#}");
            if message.contains("Failed to bind TCP listener") {
                eprintln!(
                    "skipping t_v0c_11_startup_reclaim_clears_stale_dispatch_before_redispatch: {message}"
                );
                return;
            }
            panic!("start server: {message}");
        }
    };
```

Replace with:

```rust
    let server = Arc::new(server);
    let (_url, handle) = Arc::clone(&server)
        .start()
        .await
        .expect("start server (loopback bind already probed at fn entry)");
```

- [ ] **Step 5: Verify zero remaining ad-hoc patterns in this file**

```bash
grep -n "Failed to bind TCP listener" crates/spur-mcp/tests/persisted_authority_flip.rs || echo "clean"
```

Expected: `clean`.

- [ ] **Step 6: Compile and run**

```bash
cargo test -p spur-mcp --test persisted_authority_flip -- --nocapture 2>&1 | tail -25
```

Expected: all tests pass on a normal host. No `skipping ...: loopback TCP bind denied` lines (cached `true`). Both `t_v0c_10` and `t_v0c_11` either pass or skip due to `br_available() == false` — neither should panic.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-mcp/tests/persisted_authority_flip.rs
git commit -m "test(spur-mcp): migrate persisted_authority_flip ad-hoc skips to pre-bind macro

Replaces post-bind 'Failed to bind TCP listener' match patterns at
t_v0c_10 and t_v0c_11 with skip_if_no_loopback! at fn entry. Pre-bind
probe avoids the wasted PmService/TempDir/subgraph setup cost in the
sandbox path; fn body now uses .expect() since the macro guarantees
.start() is reachable when execution reaches it.

Refs: docs/superpowers/specs/2026-04-26-spur-mcp-perf-test-relabel-and-loopback-skip-design.md"
```

---

## Task 5: Migrate `reconciler_tick.rs` (2 ad-hoc skips → pre-bind macro)

**Files:**
- Modify: `crates/spur-mcp/tests/reconciler_tick.rs`

Mirror image of Task 4 with two different test names:

- Test `submit_plan_default_notify_path_dispatches_ready_task` (line 1270) — `.start()` at line 1304, ad-hoc skip at line 1309.
- Test `execute_epic_default_notify_path_dispatches_ready_task` (line 1343) — `.start()` at line 1421, ad-hoc skip at line 1426.

- [ ] **Step 1: Confirm current state**

```bash
grep -nE "Failed to bind TCP listener|^async fn (submit_plan_default|execute_epic_default)" crates/spur-mcp/tests/reconciler_tick.rs
```

Expected: lists both function declarations (around lines 1270 and 1343) and both `Failed to bind TCP listener` matches.

- [ ] **Step 2: Add `mod common;` after the file's top-level `use` block**

```bash
grep -nE "^(use|mod) " crates/spur-mcp/tests/reconciler_tick.rs | head -20
```

Insert `mod common;` immediately after the final top-level `use` declaration.

- [ ] **Step 3: Migrate `submit_plan_default_notify_path_dispatches_ready_task`**

Find the entry of the test, currently:

```rust
async fn submit_plan_default_notify_path_dispatches_ready_task() {
    if !br_available() {
        eprintln!(
            "skipping submit_plan_default_notify_path_dispatches_ready_task: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
```

Replace with:

```rust
async fn submit_plan_default_notify_path_dispatches_ready_task() {
    if !br_available() {
        eprintln!(
            "skipping submit_plan_default_notify_path_dispatches_ready_task: `br` not on PATH"
        );
        return;
    }
    skip_if_no_loopback!("submit_plan_default_notify_path_dispatches_ready_task");

    let dir = TempDir::new().expect("tempdir");
```

Then find the post-bind ad-hoc skip near `.start()` (around line 1304):

```rust
    let server = Arc::new(server);
    let started = Arc::clone(&server).start().await;
    let (_url, _handle) = match started {
        Ok(started) => started,
        Err(error) => {
            let message = format!("{error:#}");
            if message.contains("Failed to bind TCP listener") {
                eprintln!(
                    "skipping submit_plan_default_notify_path_dispatches_ready_task: {message}"
                );
                return;
            }
            panic!("start server: {message}");
        }
    };
```

Replace with:

```rust
    let server = Arc::new(server);
    let (_url, _handle) = Arc::clone(&server)
        .start()
        .await
        .expect("start server (loopback bind already probed at fn entry)");
```

- [ ] **Step 4: Migrate `execute_epic_default_notify_path_dispatches_ready_task`**

Same pattern. At fn entry, after `br_available()` check, insert:

```rust
    skip_if_no_loopback!("execute_epic_default_notify_path_dispatches_ready_task");
```

Find the post-bind ad-hoc skip near `.start()` (around line 1421):

```rust
    let server = Arc::new(server);
    let started = Arc::clone(&server).start().await;
    let (_url, _handle) = match started {
        Ok(started) => started,
        Err(error) => {
            let message = format!("{error:#}");
            if message.contains("Failed to bind TCP listener") {
                eprintln!(
                    "skipping execute_epic_default_notify_path_dispatches_ready_task: {message}"
                );
                return;
            }
            panic!("start server: {message}");
        }
    };
```

Replace with:

```rust
    let server = Arc::new(server);
    let (_url, _handle) = Arc::clone(&server)
        .start()
        .await
        .expect("start server (loopback bind already probed at fn entry)");
```

- [ ] **Step 5: Verify zero remaining ad-hoc patterns in this file**

```bash
grep -n "Failed to bind TCP listener" crates/spur-mcp/tests/reconciler_tick.rs || echo "clean"
```

Expected: `clean`.

- [ ] **Step 6: Compile and run**

```bash
cargo test -p spur-mcp --test reconciler_tick -- --nocapture 2>&1 | tail -30
```

Expected: all tests pass on a normal host. No `skipping ...: loopback TCP bind denied` lines.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-mcp/tests/reconciler_tick.rs
git commit -m "test(spur-mcp): migrate reconciler_tick ad-hoc skips to pre-bind macro

Replaces post-bind 'Failed to bind TCP listener' match patterns at
submit_plan_default_notify_path_dispatches_ready_task and
execute_epic_default_notify_path_dispatches_ready_task with
skip_if_no_loopback! at fn entry.

Refs: docs/superpowers/specs/2026-04-26-spur-mcp-perf-test-relabel-and-loopback-skip-design.md"
```

---

## Task 6: Relabel `mutation_pagination.rs` as a perf-regression fixture

**Files:**
- Modify: `crates/spur-mcp/tests/mutation_pagination.rs`

Independent of Tasks 1–5. Replaces the top-of-file comment with a perf-regression doc-block, drops the `t_v0d_5_` plan-ticket prefix from the function name, and wraps the test in `mod perf_regressions { ... }` with explicit named imports.

- [ ] **Step 1: Confirm current top-of-file comment and function name**

```bash
sed -n '1,3p;138,142p' crates/spur-mcp/tests/mutation_pagination.rs
```

Expected:
```
//! T-v0d-5: mutation scans paginate past the former 10k issue truncation point.

use std::path::Path;
#[tokio::test]
async fn t_v0d_5_mutation_scans_paginate_past_10k_issues() {
    if !br_available() {
        eprintln!("skipping t_v0d_5_mutation_scans_paginate_past_10k_issues: `br` not on PATH");
```

- [ ] **Step 2: Replace the top-of-file comment with a perf-regression doc-block**

Find the current first line:

```rust
//! T-v0d-5: mutation scans paginate past the former 10k issue truncation point.
```

Replace with:

```rust
//! Performance-regression fixture for `PmService::list_issues`.
//!
//! `list_issues` once silently truncated at 10,000 rows; any future change that
//! reintroduces a similar cap (default limit, pagination off-by-one, query plan
//! change) must fail this test. The fixture seeds 10,050 issues, places a
//! `boundary_downstream` past the former 10k cap, runs `apply_mutation(SplitTask)`,
//! and asserts the boundary downstream gets rewired off the parent.
//!
//! This is a *correctness regression guard*, not a throughput benchmark — no
//! wall-clock or memory budgets are asserted. Timing-floor assertions were
//! considered and rejected: perf budgets in unit-style integration tests
//! tend to flake more than they catch.
```

- [ ] **Step 3: Wrap the test in `mod perf_regressions` and drop the `t_v0d_5_` prefix**

Find the current `#[tokio::test]` and the entire test function body (from line 138, opening `#[tokio::test]`, through the closing `}` of the test function around line 259). The whole block:

```rust
#[tokio::test]
async fn t_v0d_5_mutation_scans_paginate_past_10k_issues() {
    if !br_available() {
        eprintln!("skipping t_v0d_5_mutation_scans_paginate_past_10k_issues: `br` not on PATH");
        return;
    }
    if !sqlite_available() {
        eprintln!(
            "skipping t_v0d_5_mutation_scans_paginate_past_10k_issues: `sqlite3` not on PATH"
        );
        return;
    }
    // ... (rest of body unchanged)
}
```

Replace with the wrapped form. Note three changes:

1. The whole `#[tokio::test] async fn ... { ... }` is now indented one level inside `mod perf_regressions { ... }`.
2. `t_v0d_5_mutation_scans_paginate_past_10k_issues` becomes `mutation_scans_paginate_past_10k_issues` everywhere — including in the `eprintln!` skip messages.
3. A named-import block at the top of the inner module (no glob) plus a separate `use tempfile::TempDir;` (the file-scope `use tempfile::TempDir;` is not pulled into the inner module by `super::TempDir` because `TempDir` is not declared at file scope — it comes from a `use` statement; re-import directly inside the wrap).

The complete replacement:

```rust
mod perf_regressions {
    use super::{
        FILLER_COUNT, br_available, br_id, mutation_batch, run_br,
        seed_filler_issues, set_issue_timestamp, sqlite_available, task_draft,
    };
    use spur_mcp::plan::mutation::{DepRewirePolicy, PlanMutationOp};
    use spur_mcp::plan::mutation_executor::apply_mutation;
    use spur_pm::{IssueFilter, PmService};
    use std::sync::Arc;
    use tempfile::TempDir;
    use uuid::Uuid;

    #[tokio::test]
    async fn mutation_scans_paginate_past_10k_issues() {
        if !br_available() {
            eprintln!("skipping mutation_scans_paginate_past_10k_issues: `br` not on PATH");
            return;
        }
        if !sqlite_available() {
            eprintln!("skipping mutation_scans_paginate_past_10k_issues: `sqlite3` not on PATH");
            return;
        }

        let dir = TempDir::new().expect("tempdir");
        run_br(dir.path(), &["init"]).expect("br init failed");

        let parent = br_id(
            &run_br(dir.path(), &["create", "Parent", "--silent", "-t", "task"])
                .expect("create parent"),
        );
        let boundary_downstream = br_id(
            &run_br(
                dir.path(),
                &["create", "Boundary Downstream", "--silent", "-t", "task"],
            )
            .expect("create boundary downstream"),
        );

        seed_filler_issues(dir.path(), FILLER_COUNT).expect("seed filler issues");

        let head_downstream = br_id(
            &run_br(
                dir.path(),
                &["create", "Head Downstream", "--silent", "-t", "task"],
            )
            .expect("create head downstream"),
        );
        set_issue_timestamp(dir.path(), &head_downstream, "2091-01-01 00:00:00")
            .expect("promote head downstream to newest");

        run_br(dir.path(), &["dep", "add", &boundary_downstream, &parent])
            .expect("seed boundary dep");
        run_br(dir.path(), &["dep", "add", &head_downstream, &parent]).expect("seed head dep");

        let pm = Arc::new(
            PmService::try_new(None, true, false, dir.path(), None)
                .await
                .expect("PmService::try_new failed")
                .expect("expected beads-backed PmService"),
        );

        let first_ten_thousand = pm
            .list_issues(IssueFilter {
                limit: Some(10_000),
                ..Default::default()
            })
            .await
            .expect("list first 10k issues");
        assert!(
            first_ten_thousand
                .iter()
                .any(|issue| issue.id == head_downstream),
            "newest downstream should be inside the first 10k page"
        );
        assert!(
            !first_ten_thousand
                .iter()
                .any(|issue| issue.id == boundary_downstream),
            "boundary downstream must sit beyond the former 10k truncation point"
        );

        let widened_scan = pm
            .list_issues(IssueFilter {
                limit: Some(10_200),
                ..Default::default()
            })
            .await
            .expect("list widened issue window");
        assert!(
            widened_scan
                .iter()
                .any(|issue| issue.id == boundary_downstream),
            "widened scan must include the boundary downstream so the fixture proves the 10k split"
        );

        let batch = mutation_batch(
            Uuid::new_v4(),
            Some(Uuid::new_v4()),
            parent.clone(),
            vec![PlanMutationOp::SplitTask {
                parent: parent.clone(),
                children: vec![
                    task_draft("Child A", "First split child"),
                    task_draft("Child B", "Second split child"),
                ],
                dep_rewire: DepRewirePolicy::Barrier,
            }],
        );

        let child_ids = apply_mutation(pm.clone(), &batch)
            .await
            .expect("apply_mutation should succeed");
        assert_eq!(child_ids.len(), 2, "expected two split children");

        for downstream_id in [&head_downstream, &boundary_downstream] {
            let downstream = pm
                .get_issue(downstream_id)
                .await
                .expect("load downstream after mutation");
            assert!(
                !downstream.blocked_by.iter().any(|dep| dep == &parent),
                "downstream {downstream_id} must no longer depend on parent; blocked_by={:?}",
                downstream.blocked_by
            );
            for child_id in &child_ids {
                assert!(
                    downstream.blocked_by.iter().any(|dep| dep == child_id),
                    "downstream {downstream_id} must depend on child {child_id}; blocked_by={:?}",
                    downstream.blocked_by
                );
            }
        }
    }
}
```

Note that `MutationBatch` and `TaskDraft` are no longer imported because the inner module uses them only via the parent's `mutation_batch()` and `task_draft()` helpers, which return them transparently — the inner test body never names those types directly. Verify this by inspecting the body above: types referenced are `DepRewirePolicy`, `PlanMutationOp`, `IssueFilter`, `PmService`, `Arc`, `TempDir`, `Uuid` only.

- [ ] **Step 4: Verify file-scope helpers and constants are unchanged**

```bash
sed -n '1,140p' crates/spur-mcp/tests/mutation_pagination.rs | grep -E "^const|^fn|^use "
```

Expected: shows `FILLER_COUNT`, `br_available`, `sqlite_available`, `run_br`, `run_sql`, `br_id`, `task_draft`, `mutation_batch`, `seed_filler_issues`, `set_issue_timestamp`, plus the file-scope `use` block. None of these should have moved or changed visibility.

- [ ] **Step 5: Compile**

```bash
cargo build -p spur-mcp --test mutation_pagination 2>&1 | tail -10
```

Expected: `Finished` with no errors. If this fails with `cannot find ... in this scope`, audit the named-import block in Step 3 against every type/fn the inner test body actually references.

- [ ] **Step 6: Verify the new fully-qualified test path**

```bash
cargo test -p spur-mcp --test mutation_pagination mutation_scans_paginate_past_10k_issues -- --list 2>&1 | grep mutation_scans
```

Expected: exactly one match, fully qualified as `perf_regressions::mutation_scans_paginate_past_10k_issues: test`.

- [ ] **Step 7: Run the test**

```bash
cargo test -p spur-mcp --test mutation_pagination -- --nocapture 2>&1 | tail -20
```

Expected on a host with `br` and `sqlite3`: `test result: ok. 1 passed`. On a host without them: `skipping mutation_scans_paginate_past_10k_issues: \`br\` not on PATH` (or `sqlite3`) and `test result: ok. 1 passed; 0 failed; 0 ignored` (the test itself returns early — it counts as passed).

- [ ] **Step 8: Verify the dropped `t_v0d_5_` prefix is gone**

```bash
grep -n "t_v0d_5" crates/spur-mcp/tests/mutation_pagination.rs || echo "clean"
```

Expected: `clean`.

- [ ] **Step 9: Commit**

```bash
git add crates/spur-mcp/tests/mutation_pagination.rs
git commit -m "test(spur-mcp): relabel mutation_pagination as perf-regression fixture

Replaces the T-v0d-5 plan-ticket comment with a perf-regression doc-block
that names the original 10k truncation issue this test guards against.
Wraps the test in mod perf_regressions {} with explicit named imports +
re-imported tempfile::TempDir; surfaces classification in cargo test
output as mutation_pagination::perf_regressions::mutation_scans_paginate_past_10k_issues.
Drops the t_v0d_5_ prefix from the function name (plan IDs in test names rot).
No timing or throughput assertions added — correctness regression guard only.

Refs: docs/superpowers/specs/2026-04-26-spur-mcp-perf-test-relabel-and-loopback-skip-design.md"
```

---

## Task 7: Final verification sweep

**Files:** none modified — gating-only task.

This task runs all spec verification criteria (Section 3.4 of the spec) against the working tree. If any fail, return to the relevant earlier task and fix before proceeding.

- [ ] **Step 1: Verification criterion 6 — no new `#[ignore]`**

```bash
grep -rn "#\[ignore" crates/spur-mcp/tests/ || echo "no ignored tests"
```

Expected: `no ignored tests`. (None existed before; none should exist now.)

- [ ] **Step 2: Verification criterion 7 — both macro arms exercised**

```bash
grep -rn "skip_if_no_loopback!" crates/spur-mcp/tests/
```

Expected output must include:
- `rmcp_streamable_http.rs:` line(s) — the binary-arm call site (`Ok(())`).
- `server_start_pidfile.rs:`, `persisted_authority_flip.rs:`, `reconciler_tick.rs:` lines — five unary-arm call sites total.

If only one arm is used, drop the unused arm from the macro definition in `tests/common/mod.rs` before merging. Update the doc-comment on `skip_if_no_loopback!` accordingly. Make a separate cleanup commit.

- [ ] **Step 3: Verification criterion 8 — ad-hoc post-bind skips fully removed**

```bash
grep -rn "Failed to bind TCP listener" crates/spur-mcp/tests/ || echo "clean"
```

Expected: `clean`. Zero matches.

- [ ] **Step 4: Verification criterion 9 — excluded test still passes**

```bash
cargo test -p spur-mcp --test server_start_pidfile beads_backed_start_requires_repo_root_before_listener_boot -- --nocapture 2>&1 | tail -10
```

Expected: `test result: ok. 1 passed`. Output must NOT contain `skipping beads_backed_start_requires_repo_root_before_listener_boot: loopback TCP bind denied` (this test is exempt and runs to completion, asserting the pre-bind `repo_root` invariant).

- [ ] **Step 5: Verification criterion 1 — pagination test runs and passes**

```bash
cargo test -p spur-mcp --test mutation_pagination -- --nocapture 2>&1 | tail -10
```

Expected on hosts with `br` + `sqlite3`: `test result: ok. 1 passed`. The output line for the test should read `mutation_pagination::perf_regressions::mutation_scans_paginate_past_10k_issues ... ok`.

Record the wall-clock time printed by the runner in the eventual PR description (informational only — no assertion).

- [ ] **Step 6: Verification criterion 5 — discoverability of the renamed test**

```bash
cargo test -p spur-mcp mutation_scans_paginate_past_10k_issues -- --list 2>&1 | grep mutation_scans
```

Expected: exactly one line, fully qualified as `perf_regressions::mutation_scans_paginate_past_10k_issues: test`.

- [ ] **Step 6b: Verification criterion 2 — pagination test skip path (no br/sqlite3 on PATH)**

```bash
PATH=/usr/bin cargo test -p spur-mcp --test mutation_pagination -- --nocapture 2>&1 | tail -10
```

Expected: prints `skipping mutation_scans_paginate_past_10k_issues: \`br\` not on PATH` (or `\`sqlite3\` not on PATH` if `br` exists in `/usr/bin` but `sqlite3` doesn't on this host) and `test result: ok. 1 passed`. The binary must exit 0.

- [ ] **Step 7: Full crate test sweep**

```bash
cargo test -p spur-mcp -- --nocapture 2>&1 | tail -40
```

Expected on a host where loopback bind succeeds: ALL tests pass; no `skipping ...: loopback TCP bind denied` lines anywhere; no panics; no compile warnings.

- [ ] **Step 8: Sandbox path validation (skip if no sandbox available)**

If a restricted sandbox where `127.0.0.1:0` returns `EPERM` is reachable, run the same command inside it:

```bash
cargo test -p spur-mcp -- --nocapture 2>&1 | tail -40
```

Expected: every loopback-touching test prints `skipping <name>: loopback TCP bind denied (sandbox/seccomp)` and the binary exits 0. The excluded test `beads_backed_start_requires_repo_root_before_listener_boot` runs to completion and passes. The pagination test either runs (if `br`+`sqlite3` exist in the sandbox) or skips on `br_available()`.

If the sandbox is not reachable from the implementation environment, document this gap in the PR description and include the `cargo expand`-rendered macro body from Task 2 Step 6 as evidence the skip path resolves correctly.

- [ ] **Step 9: No commit needed**

This task is gating only. If everything passed, proceed to the PR. If anything failed, return to the relevant earlier task, fix, and rerun this entire verification sweep.

---

## Decision log (matches the spec's section 6)

- **Perf semantics:** option **A** — correctness regression guard, no timing/throughput assertions.
- **Perf label location:** option **A2** — wrap in `mod perf_regressions { }` inside the existing file, drop the `t_v0d_5_` plan-ticket prefix, no Cargo `[[test]]` rename.
- **TCP-bind sandbox skip strategy:** option **B3** — shared `tests/common/mod.rs` helper, not inline-per-test or env-var-gated.
- **Async cache primitive:** `tokio::sync::OnceCell` over `std::sync::OnceLock` — needed because the probe is async.
- **Macro shape:** two arms (unary + binary expr) — needed because `rmcp_streamable_http.rs` returns `Result<...>` and other tests don't.
- **Ad-hoc skip migration:** in scope. Replaces 4 existing post-bind `match` patterns with the new pre-bind macro for consistency and to skip wasted setup costs in the sandbox path.
- **Excluded test:** `beads_backed_start_requires_repo_root_before_listener_boot` deliberately exercises a pre-bind invariant and never receives the macro.
