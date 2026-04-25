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

`McpCallbackServer::start()` at `crates/spur-mcp/src/server.rs:1933` does `TcpListener::bind("127.0.0.1:0").await`. Verified via `rg '\.start\(\)' crates/spur-mcp/tests/` at `d9ba89c`, **four** test files reach this path:

- `tests/rmcp_streamable_http.rs` (1 call site)
- `tests/server_start_pidfile.rs` (3 call sites across 2 tests)
- `tests/persisted_authority_flip.rs` (2 call sites)
- `tests/reconciler_tick.rs` (2 call sites)

In `persisted_authority_flip.rs` and `reconciler_tick.rs`, four of the call sites already have **ad-hoc post-bind skip logic** that catches `Err(error)` where `format!("{error:#}").contains("Failed to bind TCP listener")` and `eprintln!` + `return`. These work in restricted sandboxes today but pay the full pre-bind setup cost (TempDir, PmService construction, set_workers, etc.) before bailing. This spec migrates them to the new pre-bind macro for consistency and to skip the wasted setup.

In `server_start_pidfile.rs`, only **one of the two tests** needs the skip macro:
- `dropping_server_handle_releases_pidfile_for_next_start` — needs the macro.
- `beads_backed_start_requires_repo_root_before_listener_boot` — does **NOT** need the macro and must not receive it. This test deliberately exercises the pre-bind `repo_root` invariant at `server.rs:1898-1914`, which fires *before* `TcpListener::bind` at line 1933. In a sandbox the test still receives the expected `"repo_root not set"` error and passes.

Other tests in the crate construct `McpCallbackServer::new(...)` (in-memory) but do not call `.start()`, so they do not bind a TCP listener and are out of scope.

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
       use tempfile::TempDir;
       use uuid::Uuid;

       #[tokio::test]
       async fn mutation_scans_paginate_past_10k_issues() {
           // body unchanged from current test (lines 138–259), with the
           // function-name reference in eprintln updated to match the new name.
       }
   }
   ```

   The named-import list omits `tempfile::TempDir` from `super::{...}` (it's not declared at parent scope as a re-exportable item; it comes from `use tempfile::TempDir;` at file scope). Re-import directly with `use tempfile::TempDir;` inside the sub-module — clearest reading and avoids `super::TempDir` ambiguity.

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

The implementation plan **MUST regenerate the wiring list** via `rg '\.start\(\)' crates/spur-mcp/tests/` and audit each match. Do not copy the table below; it is descriptive at commit `d9ba89c` and may drift in future commits.

Verified wiring at `d9ba89c`:

| File | Test fn | Action | Macro arm | Notes |
|------|---------|--------|-----------|-------|
| `rmcp_streamable_http.rs` | `rmcp_client_can_initialize_list_tools_and_call_tool` (line 15) | new wiring | binary, `Ok(())` | Returns `Result<(), Box<dyn Error>>`. |
| `server_start_pidfile.rs` | `dropping_server_handle_releases_pidfile_for_next_start` (line 90) | new wiring | unary | Three `.start()` sites in the test (lines 115, 135 inside loop). One macro at fn entry suffices. |
| `server_start_pidfile.rs` | `beads_backed_start_requires_repo_root_before_listener_boot` (line 57) | **EXCLUDED** | — | See subsection below. |
| `persisted_authority_flip.rs` | test containing `.start()` at line 688 | migrate | unary | Replace ad-hoc post-bind skip at lines 691–700. |
| `persisted_authority_flip.rs` | test containing `.start()` at line 755 | migrate | unary | Replace ad-hoc post-bind skip at lines 758–767. |
| `reconciler_tick.rs` | test containing `.start()` at line 1304 | migrate | unary | Replace ad-hoc post-bind skip at line 1309. |
| `reconciler_tick.rs` | test containing `.start()` at line 1421 | migrate | unary | Replace ad-hoc post-bind skip at line 1426. |

Function names for the `persisted_authority_flip.rs` and `reconciler_tick.rs` rows are resolved during the planning grep pass by walking back from each `.start()` line to the enclosing `async fn`.

#### Excluded test: `beads_backed_start_requires_repo_root_before_listener_boot`

This test in `server_start_pidfile.rs` deliberately exercises a **pre-bind invariant**: it constructs a beads-backed server *without* setting `repo_root` and asserts that `start()` returns `Err("repo_root not set on McpCallbackServer")`. The check at `crates/spur-mcp/src/server.rs:1898–1914` runs **before** `TcpListener::bind` at line 1933, so in any environment (sandboxed or not) the test reaches the expected error and passes. Adding the skip macro to this test would be wrong — it would skip a test that has nothing to do with the listener.

Implementation rule: when auditing each `.start()` match, distinguish between *use-the-listener* tests (need the macro) and *exercise-pre-bind-invariants* tests (must not get the macro). The deciding question is "does this test expect `start()` to succeed?" — if no, audit whether the failure path is pre- or post-bind in `server.rs::start()`.

#### Migrating ad-hoc post-bind skips

Existing pattern at `persisted_authority_flip.rs:688-700` (representative):
```rust
let started = Arc::clone(&server).start().await;
let (_url, handle) = match started {
    Ok(started) => started,
    Err(error) => {
        let message = format!("{error:#}");
        if message.contains("Failed to bind TCP listener") {
            eprintln!("skipping <test_name>: {message}");
            return;
        }
        panic!("start server: {message}");
    }
};
```

After migration:
```rust
skip_if_no_loopback!("<test_name>");
// ... (existing setup unchanged) ...
let (_url, handle) = Arc::clone(&server).start().await.expect("start server");
```

The migration:
1. Inserts the macro at function entry, before any side effect (TempDir, PmService, set_repo_root, set_reconciler_enabled).
2. Replaces the post-bind match-on-error with a plain `.expect()` since the macro guarantees we don't reach `start()` in a sandbox.
3. Saves the test the cost of building the full setup before bailing.

Tests in the same binary that do **not** call `.start()` are not modified — the skip is per-test, not per-binary.

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
   `grep -rn 'skip_if_no_loopback!' crates/spur-mcp/tests/` shows at least one unary-arm call site (the `server_start_pidfile`, `persisted_authority_flip`, `reconciler_tick` tests) and at least one binary-arm call site (`rmcp_streamable_http`). If only one arm is used in practice, drop the unused arm before merging.

8. **Ad-hoc post-bind skip removal.**
   `grep -rn 'Failed to bind TCP listener' crates/spur-mcp/tests/` returns zero matches after implementation. The four existing post-bind match-on-error patterns at `persisted_authority_flip.rs:691-700`, `persisted_authority_flip.rs:758-767`, `reconciler_tick.rs:1309`, `reconciler_tick.rs:1426` are fully replaced by the pre-bind macro.

9. **Excluded test still passes.**
   `cargo test -p spur-mcp --test server_start_pidfile beads_backed_start_requires_repo_root_before_listener_boot` runs and passes both on a normal host and in the sandbox (without the macro, by virtue of the pre-bind `repo_root` check firing at `server.rs:1898–1914`).

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

- **Side-effect ordering:** the macro must precede *all* side effects in each test (temp dirs, PmService construction, pidfile acquisition). The implementation plan calls this out per-test; review during planning confirms placement. Verified at design time for `rmcp_streamable_http.rs` (line 17 is the first body line, all subsequent constructors are in-memory until `.start()` at line 36).

- **Pre-bind probe vs. post-bind skip semantic difference:** the four migrated ad-hoc skips matched on the post-bind error string `"Failed to bind TCP listener"` — they would have skipped on *any* bind failure, including (theoretically) `EADDRINUSE` from a port collision. The pre-bind macro probes `127.0.0.1:0` upfront with a 3-attempt retry budget. On a host where `127.0.0.1:0` succeeds (kernel allocates an unused ephemeral port — `EADDRINUSE` here is essentially impossible) the macro returns `true` and the test runs; if a real `start()` later fails for any reason, the test panics rather than skips. This is acceptable: `EADDRINUSE` on `0` is not a realistic failure mode, and the failure modes the existing skip was guarding against (sandbox `EPERM`) are exactly what the new probe catches.

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
- **Triple-review amendments (gemini, kimi, codex on `d9ba89c`):**
  - Added `tempfile::TempDir` import inside the `mod perf_regressions` block (gemini caught: file-scope `use tempfile::TempDir;` does not propagate into the wrap module via `super::TempDir` in the named-import list).
  - Replaced the loopback-test wiring table with the verified 4-file list: `rmcp_streamable_http`, `server_start_pidfile`, `persisted_authority_flip`, `reconciler_tick` (codex + kimi independently verified via `rg '\.start\(\)'`). Removed three files that don't call `.start()` (`e2e_closure_v0e`, `pidfile_single_brain`, `parallel_response_shape`, `block_timeout_continuation`) and added two that do (`persisted_authority_flip`, `reconciler_tick`).
  - Added explicit exclusion for `beads_backed_start_requires_repo_root_before_listener_boot` (kimi caught: deliberately exercises pre-bind `repo_root` invariant at `server.rs:1898–1914`, must not receive the macro).
  - Expanded scope to migrate four existing ad-hoc post-bind skip patterns (`persisted_authority_flip.rs:691-700, 758-767`; `reconciler_tick.rs:1309, 1426`) to the pre-bind macro for consistency. Adds verification step 8 (`grep` for the old error string returns zero matches post-implementation).
  - Mandated that the implementation plan regenerate the wiring list via `rg '\.start\(\)'` rather than copy from the spec table (codex's recommendation).
