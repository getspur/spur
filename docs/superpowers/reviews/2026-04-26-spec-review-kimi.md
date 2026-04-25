# Review: spur-mcp perf-test relabel + loopback-bind sandbox skip design

**Reviewer:** Kimi Code CLI  
**Date:** 2026-04-26  
**Commit reviewed:** d9ba89c

---

## 1. Loopback-touching test inventory is incorrect (major)

`grep -rn '\.start()' crates/spur-mcp/tests/` at d9ba89c shows **only 2 of the 6 listed files** actually call `McpCallbackServer::start()`:

- **Actually calls `.start()`:** `rmcp_streamable_http.rs`, `server_start_pidfile.rs`
- **Does NOT call `.start()`:** `e2e_closure_v0e.rs`, `pidfile_single_brain.rs`, `parallel_response_shape.rs`, `block_timeout_continuation.rs`

Conversely, two files that **do** call `.start()` and already contain ad-hoc sandbox-skip logic are **omitted** from the spec:

- `persisted_authority_flip.rs` (lines 688, 755)
- `reconciler_tick.rs` (lines 1304, 1421)

Both already catch `"Failed to bind TCP listener"` and `eprintln!` + `return`. The spec's expected table and grep-based methodology will produce the wrong target set if applied literally.

## 2. `server_start_pidfile.rs` has a test that does not need skipping

`beads_backed_start_requires_repo_root_before_listener_boot` calls `.start()` but expects a pre-bind error: `repo_root` is unset. Reading `server.rs:1950-1952`, the `repo_root` check executes **before** `TcpListener::bind` (line 1984). In a sandbox this test still receives the expected `repo_root not set` error and passes. Only `dropping_server_handle_releases_pidfile_for_next_start` needs the skip macro.

## 3. `#[tokio::test]` semantics survive `mod perf_regressions` wrap (verified)

Rust attribute macros expand at the item level; nesting inside `mod perf_regressions { ... }` does not change `#[tokio::test]` behavior. The existing file `parallel_response_shape.rs` already uses `#[tokio::test(flavor = "current_thread", start_paused = true)]` successfully without any module wrap, confirming the macro handles these arguments at the function level.

*Minor correction:* the workspace root `Cargo.toml` does not declare `tokio` with `test-util`; that feature is added locally in `crates/spur-mcp/Cargo.toml`.

## 4. `FILLER_COUNT = 10_050` remains meaningful; no global state risk

The wrap changes only the test's module path. `FILLER_COUNT` stays a file-scope `const`; each test creates its own `TempDir` and independent beads repo. There is no cross-test ordering or mutable global state that the module wrap would alter.

## 5. "Skip before any side effect" is reachable for `rmcp_streamable_http.rs`

The spec requires the macro before all side effects. In `rmcp_streamable_http.rs`, `McpCallbackServer::start()` is at line 36. Before it are `BrainSessionId::new`, `McpCallbackServer::new`, and `server.set_workers` — these are in-memory constructors/mutations with no filesystem, network, or pidfile effects. The skip macro can be inserted at line 17 (the first line of the function body) and satisfies the rule.

---

## Recommendation

Before implementation, re-run the authoritative `grep -rn '\.start()' crates/spur-mcp/tests/` and update the target file list. Exclude files that do not call `.start()`; consider whether `persisted_authority_flip.rs` and `reconciler_tick.rs` should migrate their ad-hoc skips to the new macro, or at least be acknowledged in the design.
