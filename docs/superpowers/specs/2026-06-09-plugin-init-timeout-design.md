# Separate plugin `INIT_TIMEOUT` from steady-state `REQUEST_TIMEOUT`

- **Date:** 2026-06-09
- **Status:** Approved
- **Owner:** brain
- **Worker:** codex
- **Design epic:** `bd-35pn`
- **Source:** core/foundation ↔ app integration architecture review (graph-verified, 2026-06-09)

## Problem (graph-verified)

App plugins are launched via `uv run --with-requirements <reqs> python <entry>`
(`crates/spur-notebook/src/mcp/plugin_loader.rs:339-367`). `uv` resolves and **installs the
app's dependencies before the child process answers the MCP `initialize` handshake**.

The `initialize` request is bounded by the same per-RPC timeout as every steady-state call:
`REQUEST_TIMEOUT = Duration::from_secs(30)` (plugin_loader.rs:33), applied in `PluginIo::request`
via `tokio::time::timeout(REQUEST_TIMEOUT, read)` → `PluginError::Timeout(method)`
(plugin_loader.rs:442-444). `initialize()` (468-482) and `list_tools()` (485) both route through
`request`, and `PluginHandle::launch` (516-555) awaits `io.initialize()` then `io.list_tools()`.

**Consequence:** a cold install of non-trivial dependencies (numpy/pandas/torch/…) that takes
longer than 30s makes an app's **first launch spuriously fail** with
`PluginError::Timeout("initialize")`, surfaced upstream as `app_plugin_spawn_failed`. Subsequent
launches (warm uv cache) succeed, which makes the failure intermittent and confusing.

There is **no plugin-spawn test harness** today — every test in `plugin_loader.rs` is a pure
`PluginConfig`/registry unit test (none spawns a child). The fix must add a minimal test seam.

## Approach

Give the **initialize handshake** a longer, operator-tunable timeout, leaving steady-state calls
on the existing 30s bound:

- Add `const DEFAULT_INIT_TIMEOUT: Duration = Duration::from_secs(180)`.
- Add `fn init_timeout() -> Duration` that reads `SPUR_PLUGIN_INIT_TIMEOUT_MS` (parse as millis)
  and falls back to `DEFAULT_INIT_TIMEOUT`. The env override exists for ops tuning **and** to let
  tests force a tiny timeout.
- Refactor `PluginIo::request(method, params)` into
  `request_with_timeout(method, params, timeout: Duration)` containing the current body (the
  `tokio::time::timeout(timeout, read)` call), with `request` delegating using `REQUEST_TIMEOUT`.
- `initialize()` calls `request_with_timeout("initialize", …, init_timeout())`.

Only `initialize` uses the long timeout: the cold install completes before the `initialize`
response arrives, so the subsequent `list_tools()` (and all steady-state `call_tool`/`ping`) stay
on `REQUEST_TIMEOUT`.

### Rejected alternatives
- **Raise `REQUEST_TIMEOUT` globally to 180s** — would make a genuinely-hung tool call block the
  UI for 3 minutes. Steady-state calls must stay snappy.
- **Pre-install deps at import** — larger feature (the deferred packaging epic); does not fix the
  launch-path bound and adds its own UX surface.

## Task (single, backend-only)

### T1 — `INIT_TIMEOUT` for the initialize handshake
- **Files:** Modify `crates/spur-notebook/src/mcp/plugin_loader.rs` (const + helper + `request`
  refactor + `initialize` + tests).
- **TDD:**
  1. **Failing test** `request_with_timeout_times_out_on_silent_plugin` in the existing
     `#[cfg(test)] mod tests`: spawn a silent child (`Command::new("sh").arg("-c").arg("sleep 30")`
     with piped stdin/stdout, `kill_on_drop(true)`), construct a `PluginIo` from its pipes
     (same-module access to private fields), call
     `request_with_timeout("initialize", json!({}), Duration::from_millis(50))`, assert
     `matches!(res, Err(PluginError::Timeout(_)))`. Fails to compile (method doesn't exist yet).
  2. Implement the const, `init_timeout()`, the `request`/`request_with_timeout` split, and the
     `initialize` change.
  3. **Second test** `init_timeout_honors_env_override_and_default`: with the env var unset →
     `DEFAULT_INIT_TIMEOUT`; set `SPUR_PLUGIN_INIT_TIMEOUT_MS=250` → `Duration::from_millis(250)`.
     (Guard env mutation so it doesn't leak; the test is single-threaded over that var.)
  4. `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook plugin` → PASS;
     `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-notebook -- -D warnings` → clean.
- **Acceptance:** initialize uses `init_timeout()` (default 180s, env-overridable); steady-state
  `request`/`list_tools`/`call_tool`/`ping` unchanged at `REQUEST_TIMEOUT`; both tests pass.
- **Scope boundary:** IN `plugin_loader.rs`. OUT: `open_path`/`spawn_app_plugin` lifecycle
  (deferred H2), ports, frontend. Emit `scope_drift` if other files are needed.
- **Suggested worker:** codex.

## Constraints
- Build/test only via `SPUR_REMOTE=1 scripts/spur-cargo` (never bare cargo).
- Single file. Steady-state timeout unchanged.

## Acceptance (epic)
An app whose cold dependency install takes up to `INIT_TIMEOUT` (default 180s) launches
successfully; steady-state tool calls remain bounded at 30s. Covered by the two unit tests above.

## Deferred follow-ups (separate epics — need design)
- **Async + observable bootstrap (H2 + open-design Panel E):** non-blocking spawn, in-window
  bootstrap-status HUD, spawn failure surfaced in-window, prior app preserved on failure.
- **Port-write serialization (H3):** advisory-lock the read-modify-write across the four port
  writers (`dag/ports.rs` + `ports_bootstrap.{py,js,rs}`).
- **App-tool namespacing / collision-as-error (M1).**
