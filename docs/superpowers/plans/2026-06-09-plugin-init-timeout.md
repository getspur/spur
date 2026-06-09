# Plugin `INIT_TIMEOUT` Implementation Plan

> **For SPUR orchestrator:** designed for `submit_plan`. Single task under epic `bd-35pn`.

**Source spec:** `docs/superpowers/specs/2026-06-09-plugin-init-timeout-design.md`
**Design epic:** `bd-35pn` (open)

**Goal:** Give the plugin `initialize` handshake a longer, env-tunable timeout (`INIT_TIMEOUT`,
default 180s) so cold `uv` dependency installs don't fail an app's first launch; steady-state calls
keep `REQUEST_TIMEOUT` (30s).

**Architecture:** One file. Split `PluginIo::request` into a timeout-parameterized core; route
`initialize` through `init_timeout()`.

---

## Task 1: `INIT_TIMEOUT` for the initialize handshake

**Task ID:** `task-1`

**Files:**
- Modify: `crates/spur-notebook/src/mcp/plugin_loader.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `initialize()` uses `init_timeout()` (default 180s, env `SPUR_PLUGIN_INIT_TIMEOUT_MS`).
- [ ] `request` (steady-state), `list_tools`, `call_tool`, `ping` still use `REQUEST_TIMEOUT`.
- [ ] Two passing tests (timeout-fires, env-parse).
- [ ] `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook plugin` and clippy `-D warnings` pass.

**Scope Boundary:**
- IN: `plugin_loader.rs`.
- OUT: `open_path`/`spawn_app_plugin` lifecycle, ports, frontend. Emit `scope_drift` if needed.

**Implementation:**

- [ ] **Step 1: failing test** in the existing `#[cfg(test)] mod tests`. Imports needed:
  `tokio::process::Command`, `std::process::Stdio`, `tokio::io::BufReader`, `std::time::Duration`,
  `serde_json::json`. `PluginIo`'s fields are private but same-module, so construct it directly:

```rust
#[tokio::test]
async fn request_with_timeout_times_out_on_silent_plugin() {
    // A child that never speaks MCP — initialize will never get a response.
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg("sleep 30")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn fake plugin");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = tokio::io::BufReader::new(child.stdout.take().expect("stdout"));
    let mut io = PluginIo { stdin, stdout, next_id: 1 };

    let res = io
        .request_with_timeout("initialize", serde_json::json!({}), std::time::Duration::from_millis(50))
        .await;
    assert!(matches!(res, Err(PluginError::Timeout(method)) if method == "initialize"));
}
```

- [ ] **Step 2: run, verify fail** —
  `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook request_with_timeout_times_out` → FAIL
  (method not defined).

- [ ] **Step 3: implement.** Near `REQUEST_TIMEOUT` (plugin_loader.rs:33) add:

```rust
const DEFAULT_INIT_TIMEOUT: Duration = Duration::from_secs(180);

/// Timeout for the plugin `initialize` handshake. Longer than `REQUEST_TIMEOUT` because
/// `uv run --with-requirements` resolves+installs dependencies before the child answers.
/// Overridable via `SPUR_PLUGIN_INIT_TIMEOUT_MS` (milliseconds) for ops tuning and tests.
fn init_timeout() -> Duration {
    std::env::var("SPUR_PLUGIN_INIT_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_INIT_TIMEOUT)
}
```

  Refactor `PluginIo::request` (plugin_loader.rs:393-446): rename it to
  `request_with_timeout(&mut self, method: &str, params: Value, timeout: Duration)` — body
  unchanged **except** the final match uses the `timeout` parameter instead of the const — and add
  a thin delegating `request`:

```rust
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, PluginError> {
        self.request_with_timeout(method, params, REQUEST_TIMEOUT).await
    }

    async fn request_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, PluginError> {
        // ... existing request body (id, write_message, the `read` async block) unchanged ...
        match tokio::time::timeout(timeout, read).await {
            Ok(result) => result,
            Err(_) => Err(PluginError::Timeout(method.to_string())),
        }
    }
```

  Change `initialize()` (plugin_loader.rs:468-482) to call the timeout variant for the handshake:

```rust
    async fn initialize(&mut self) -> Result<(), PluginError> {
        self.request_with_timeout(
            "initialize",
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "spur-notebook", "version": env!("CARGO_PKG_VERSION") },
            }),
            init_timeout(),
        )
        .await?;
        self.notify("notifications/initialized", json!({})).await
    }
```

  Leave `list_tools` (485), `call_tool`, and `ping` (501) calling `request` (→ `REQUEST_TIMEOUT`).

- [ ] **Step 4: second test** (env parsing). Mutating a process-global env var in a test is
  inherently racy across threads; keep it self-contained and restore afterward:

```rust
#[test]
fn init_timeout_honors_env_override_and_default() {
    // Default when unset.
    std::env::remove_var("SPUR_PLUGIN_INIT_TIMEOUT_MS");
    assert_eq!(init_timeout(), DEFAULT_INIT_TIMEOUT);
    // Honors the override.
    std::env::set_var("SPUR_PLUGIN_INIT_TIMEOUT_MS", "250");
    assert_eq!(init_timeout(), Duration::from_millis(250));
    std::env::remove_var("SPUR_PLUGIN_INIT_TIMEOUT_MS");
}
```

  If `cargo test`'s parallelism makes the env test flaky, gate it with `#[serial_test::serial]`
  **only if** that crate is already a dev-dependency; otherwise leave it and rely on the
  remove→assert→set→assert→remove ordering within the single test.

- [ ] **Step 5: run, verify pass** —
  `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook plugin` → PASS;
  `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-notebook -- -D warnings` → clean.

- [ ] **Step 6: commit** — `feat(spur-notebook): bd-35pn longer INIT_TIMEOUT for plugin handshake`.

**Suggested Worker:** codex.

---

## Self-Review
- **Spec coverage:** const + `init_timeout()` + `request`/`request_with_timeout` split + `initialize`
  change + two tests — all spec bullets covered. ✓
- **Placeholders:** none — full code for helper, refactor, `initialize`, and both tests. ✓
- **Type consistency:** `request_with_timeout(&mut self, &str, Value, Duration) -> Result<Value,
  PluginError>`; `request` delegates; `PluginError::Timeout(String)` already exists
  (plugin_loader.rs:50). ✓
- **DAG:** single task, no deps. ✓
- **beads:** one task under `bd-35pn`, scope boundary + verifiable acceptance. ✓
