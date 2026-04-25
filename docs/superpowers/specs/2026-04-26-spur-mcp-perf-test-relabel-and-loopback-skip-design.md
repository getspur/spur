# spur-mcp: perf-test relabel + loopback-bind sandbox skip — Design

**Status:** Draft for implementation
**Date:** 2026-04-26
**Crate:** `crates/spur-mcp`
**Scope:** Two coordinated changes inside `crates/spur-mcp/tests/`. No production-code changes. No other crates touched.

---

## 1. Problem statement

Two independent issues are addressed together because they share a target directory and one rollout PR keeps the diff readable.

### 1.1 Perf-regression test classification

`crates/spur-mcp/tests/mutation_pagination.rs` contains a single `#[tokio::test]`, currently named `t_v0d_5_mutation_scans_paginate_past_10k_issues`. It exists because `PmService::list_issues` once silently truncated at 10,000 rows. The fixture seeds 10,050 issues into the beads SQLite database via a recursive CTE INSERT, places a `boundary_downstream` past the former 10k cap, runs `apply_mutation(SplitTask)`, and asserts both `head_downstream` (newest) and `boundary_downstream` (beyond the 10k boundary) get rewired off the parent.

The test is a **performance-regression guard**: any future change that re-introduces a similar cap (default limit, pagination off-by-one, query plan change) must fail here. But the file's classification is invisible — the name carries a stale plan-ticket prefix (`t_v0d_5_`), there is no doc-block declaring the perf semantics, and the test sits next to ordinary correctness fixtures (`mutation_acyclicity.rs`, `mutation_split.rs`, `mutation_write_ahead.rs`).

The goal is to make the perf framing visible from `cargo test` output and from a one-pass read of the file, without timing assertions, without dataset sweeps, and without renaming the file (preserves the `mutation_*` family, preserves grep history).

### 1.2 Loopback-bind tests fail in restricted sandboxes

`McpCallbackServer::start()` at `crates/spur-mcp/src/server.rs:1933` does `TcpListener::bind("127.0.0.1:0").await`. Six integration tests reach this path and currently hard-fail with `EPERM` ("Operation not permitted") when run inside a sandbox whose seccomp profile denies loopback `bind(2)`:

- `tests/rmcp_streamable_http.rs`
- `tests/e2e_closure_v0e.rs`
- `tests/pidfile_single_brain.rs`
- `tests/server_start_pidfile.rs`
- `tests/parallel_response_shape.rs`
- `tests/block_timeout_continuation.rs`

Other tests in the same crate may also reach `start()`. The implementation pass (planning skill) will produce the authoritative list via `grep -rn '\.start()' crates/spur-mcp/tests/`.

The goal is for these tests to **skip gracefully** in restricted sandboxes (eprintln + early return) the same way `mutation_pagination.rs` already skips when `br` or `sqlite3` are absent, while running normally on hosts where loopback bind succeeds. This is a test-boundary fix only — `McpCallbackServer::start()` itself is unchanged.

## 2. Non-goals

- No timing/throughput assertions on the pagination fixture. Considered (option B) and rejected — perf budgets in unit-style integration tests tend to flake more than they catch, and there is no recorded regression a budget would have caught.
- No dataset sweep (parameterized `FILLER_COUNT`). Out of scope; if needed later, a separate fixture can characterize scaling.
- No file rename, no Cargo `[[test]]` entry, no move into a `tests/perf/` subdirectory. (Cargo does not auto-discover subdirs as integration binaries; using one would force a `[[test]]` declaration for no semantic gain.)
- No transport refactor to remove the loopback dependency. An in-memory rmcp transport for tests would be a larger, separate project.
- No changes to other crates. `spur-tui`, `spur-acp`, `spur-bot` may have similar sandbox-bind exposures; if the same pattern is needed elsewhere, that is a follow-up.
- No `#[ignore]` attributes. Default `cargo test` must remain green in both sandboxed and unsandboxed environments.

## 3. Design

### 3.1 Section 1 — Perf-regression fixture relabel

**File:** `crates/spur-mcp/tests/mutation_pagination.rs` (no rename).

**Changes:**

1. Replace the single-line top-of-file comment with a doc-block that classifies the file as a performance-regression fixture, names the original 10k-truncation regression it guards against, and explicitly states that timing assertions were considered and rejected (correctness-only).

2. Drop the `t_v0d_5_` plan-ticket prefix from the test function name. The plan associated with that ticket is closed; plan IDs in test names rot. Function rename: `t_v0d_5_mutation_scans_paginate_past_10k_issues` → `mutation_scans_paginate_past_10k_issues`.

3. Wrap the test in a sub-module `mod perf_regressions { ... }` so the perf classification surfaces in `cargo test` output as `mutation_pagination::perf_regressions::mutation_scans_paginate_past_10k_issues`.

4. Inside the sub-module, use **explicit named imports** rather than a glob (`use super::*` would silently miss the file-scope private helpers — Rust's glob import only re-exports `pub` items; named imports work for parent-private items because child modules can already see their parents):

   ```rust
   mod perf_regressions {
       use super::{
           FILLER_COUNT, br_available, br_id, mutation_batch, run_br,
           seed_filler_issues, set_issue_timestamp, sqlite_available, task_draft,
       };
       use spur_mcp::plan::mutation::{DepRewirePolicy, MutationBatch, PlanMutationOp, TaskDraft};
       use spur_mcp::plan::mutation_executor::apply_mutation;
       use spur_pm::{IssueFilter, PmService};
       use std::sync::Arc;
       use uuid::Uuid;

       #[tokio::test]
       async fn mutation_scans_paginate_past_10k_issues() {
           // body unchanged from current test (lines 138–259), with the
           // function-name reference in eprintln updated to match the new name.
       }
   }
   ```

5. Helpers (`br_available`, `sqlite_available`, `run_br`, `run_sql`, `br_id`, `task_draft`, `mutation_batch`, `seed_filler_issues`, `set_issue_timestamp`) and `FILLER_COUNT` stay private at file scope. No `pub` mutation needed.

6. Skip-on-missing-prerequisite pattern (`br_available()` / `sqlite_available()` guards) preserved verbatim.

7. `FILLER_COUNT = 10_050` preserved as the regression-boundary witness.

### 3.2 Section 2 — Shared sandbox-skip helper

**New file:** `crates/spur-mcp/tests/common/mod.rs`

Cargo only auto-discovers top-level `.rs` files in `tests/` as integration-test binaries; subdirectories like `tests/common/` are not auto-compiled and do not become their own binaries. The `mod.rs` form (vs. a top-level `tests/common.rs` which *would* become a binary) is the conventional way to share code across integration tests.

**Contents:**

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

**Why `tokio::sync::OnceCell` over `std::sync::OnceLock`:** the probe is async (`TcpListener::bind` is async), and `OnceLock::get_or_init` takes a synchronous closure. The naive workarounds (manual `get` + `set` with the lost-race tolerance, or `OnceLock` storing only the success case) either pay one bind syscall per consumer in the failure path or risk latching `false` on a transient blip. `tokio::sync::OnceCell::get_or_init` is async-aware, runs the probe at most once across all concurrent callers, and is already available via the workspace `tokio` dep (`features = ["full"]` for tests).

**Why two macro arms:** the unary form emits `return;` which is a type error inside `async fn ... -> Result<(), _>`. Three of the loopback-touching tests are unit-returning; `rmcp_streamable_http.rs` returns `Result<(), Box<dyn std::error::Error>>`. The binary arm lets each call site state its early-return expression explicitly. A future test with a third return shape would call the binary arm with an appropriate value (e.g. `Default::default()`); the macro will fail to compile rather than silently miscompile if neither arm fits.

**Cargo.toml:** no change. `tests/common/mod.rs` does not require a `[[test]]` entry.

### 3.3 Section 2b — Per-test wiring

Each loopback-touching integration test gets two additions:

1. `mod common;` declaration at the top of the file (immediately after the existing `use` block). Because each integration-test `.rs` is its own crate root, `mod common;` resolves to `tests/common/mod.rs`.

2. A `skip_if_no_loopback!(...)` invocation at the start of every `#[tokio::test]` function that calls `McpCallbackServer::start()`, **before** any side effect (no temp dir, no PmService, no pidfile probe — the skip must be cheap and side-effect-free).

   - Tests with `async fn name()` return `()` use the unary form: `skip_if_no_loopback!("name");`
   - `rmcp_streamable_http.rs::rmcp_client_can_initialize_list_tools_and_call_tool` returns `Result<(), Box<dyn std::error::Error>>`; it uses the binary form: `skip_if_no_loopback!("rmcp_client_can_initialize_list_tools_and_call_tool", Ok(()));`

The implementation pass produces the exact list of tests via `grep -rn '\.start()' crates/spur-mcp/tests/` and audits each match. The expected list at design time:

| Test file | Test name | Macro arm |
|-----------|-----------|-----------|
| `rmcp_streamable_http.rs` | `rmcp_client_can_initialize_list_tools_and_call_tool` | binary, `Ok(())` |
| `e2e_closure_v0e.rs` | (per-test, see implementation pass) | unary |
| `pidfile_single_brain.rs` | (per-test) | unary |
| `server_start_pidfile.rs` | (per-test) | unary |
| `parallel_response_shape.rs` | (per-test) | unary |
| `block_timeout_continuation.rs` | (per-test) | unary |

Tests in the same binary that do **not** call `start()` are not modified — the skip is per-test, not per-binary, so unrelated tests in the same file still run (and on a sandbox host, the cached `false` from the probe still costs only one bind attempt for the binary as a whole).

### 3.4 Section 3 — Verification

All seven checks must pass before claiming the work complete:

1. **Pagination test, host with `br` + `sqlite3`:**
   `cargo test -p spur-mcp --test mutation_pagination -- --nocapture`
   Must print `mutation_pagination::perf_regressions::mutation_scans_paginate_past_10k_issues ... ok`. Wall-clock recorded in the implementation PR description for posterity (no assertion).

2. **Pagination test, environment without `br`/`sqlite3`:**
   `PATH=/usr/bin cargo test -p spur-mcp --test mutation_pagination -- --nocapture`
   Must print the existing `skipping …` line and exit 0.

3. **Loopback probe, restricted sandbox:**
   `cargo test -p spur-mcp -- --nocapture` from inside the failing sandbox. Every previously-EPERM test must now print `skipping <name>: loopback TCP bind denied (sandbox/seccomp)` and the binary must exit 0.

4. **Loopback probe, normal host:**
   Same command on a host where loopback bind succeeds. All loopback tests must run and pass — no `skipping …` line for them. The probe is shared across the binary, so this confirms the cached-`true` path.

5. **Function-rename / module-wrap discoverability:**
   `cargo test -p spur-mcp mutation_scans_paginate_past_10k_issues -- --list` must list exactly one match, under `mutation_pagination::perf_regressions::`.

6. **No new `#[ignore]` attributes.**
   `grep -rn '#\[ignore' crates/spur-mcp/tests/` returns no new lines vs `main`.

7. **Both macro arms exercised.**
   `grep -rn 'skip_if_no_loopback!' crates/spur-mcp/tests/` shows at least one unary-arm call site and at least one binary-arm call site. If only one arm is used in practice, drop the unused arm before merging.

### 3.5 Rollout sequence

The implementation plan will follow this order; each step is independently committable.

1. Land `crates/spur-mcp/tests/common/mod.rs` with the helper and macro. Confirm it compiles (will produce a "module never used" warning until step 2 lands; acceptable for a single intermediate commit).
2. Wire `tests/rmcp_streamable_http.rs` only (`mod common;` + binary-arm macro call). Validate verification steps 3 and 4 on this single file before fanning out.
3. Wire the remaining five loopback tests in one mechanical pass. Re-run verification steps 3 and 4.
4. Apply Section 1 changes to `tests/mutation_pagination.rs`: doc-block rewrite, sub-module wrap, named imports, function rename. Validate verification steps 1, 2, 5.
5. Final sweep: full `cargo test -p spur-mcp` on a normal host **and** in the sandbox. Both must be green. Verification step 6 + 7 confirmed.

## 4. Risks

- **`tokio::sync::OnceCell` semantics:** the helper relies on `get_or_init` running the async closure at most once across all concurrent callers in a binary. This is documented behavior. If the closure panics, the cell remains uninitialized and subsequent callers re-run it; the probe doesn't panic in normal paths, but if `TcpListener::bind` ever did, the retry budget would not protect us — a subsequent caller would run the probe again. Acceptable: the probe is simple enough that panic exposure is essentially zero.

- **Macro hygiene path resolution:** `#[macro_export]` puts `skip_if_no_loopback!` at the test crate root. Each consumer needs **both** `mod common;` and the macro to be in scope. Because `#[macro_export]` exports to the crate root and integration tests' crate root is the file itself, the macro is callable unqualified after `mod common;` is declared. Step 2 of the rollout validates this on a single file before fanning out.

- **Plan-ticket prefix removal:** `t_v0d_5_` may appear in archived plan documents. A grep pass during planning will identify any references; archived plan docs are not load-bearing and references can be left as historical breadcrumbs. CI workflows in `.github/workflows/` do not invoke the test by symbol name (verified during design review).

- **Future test with a third return shape:** if a new loopback-touching test returns, e.g., `Result<NonUnit, Error>`, neither macro arm fits. The binary arm will accept any expression of the right type, so this isn't strictly a third arm — the test author writes `skip_if_no_loopback!("name", Ok(my_default_value));`. If multiple tests grow exotic shapes, consider adding a third `expr` arm or just inlining the skip logic at the call site.

- **Side-effect ordering:** the macro must precede *all* side effects in each test (temp dirs, PmService construction, pidfile acquisition). The implementation plan calls this out per-test; review during planning confirms placement.

## 5. Out of scope

- Other crates (`spur-tui`, `spur-acp`, `spur-bot`) with similar sandbox exposures — separate follow-up.
- Replacing the loopback transport with an in-memory rmcp transport — separate, larger project.
- Restructuring `mutation_pagination.rs` to characterize scaling (parameterized `FILLER_COUNT` over multiple sizes) — option C from brainstorming, not chosen.
- Adding wall-clock or memory budgets to the perf fixture — option B from brainstorming, not chosen.

## 6. Decision log (from brainstorming)

- **Perf semantics:** option **A** chosen — keep correctness assertions only, classify as perf-regression fixture, no timing/throughput assertions.
- **Perf label location:** option **A2** chosen — keep file at `tests/mutation_pagination.rs`, label inside the file via doc-block + sub-module wrap, drop plan-ticket prefix.
- **TCP-bind sandbox skip strategy:** option **B3** chosen — shared `tests/common/mod.rs` helper, not inline-per-test or env-var-gated.
- **Async cache primitive:** `tokio::sync::OnceCell` over `std::sync::OnceLock` (caught in review iteration; needed because the probe is async).
- **Macro shape:** two arms (unary + binary expr) over `Default::default()` trick (caught in review iteration; needed because `rmcp_streamable_http.rs` returns `Result<...>` and one of the other tests doesn't).
