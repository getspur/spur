# Track A — skip_permissions follow-up cleanup — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the three independent follow-ups (F1, F3, F4) from the 2026-04-14 `skip_permissions` ship. Each is a separate commit; no coordination between them required.

**Architecture:** F1 extends the existing `skip_perm` helper module with a `load_session_with_bypass` mirror of `new_session_with_bypass`, applied at the one `load_session` call site. F3 extracts the duplicated `match transport { ... }` arms shared between `create_connection` (line 1494) and `run_one_worker_attempt` (line 2410). F4 rewrites a stale file-level doc comment.

**Tech Stack:** Rust, tokio, async_trait, serde. Tests via `cargo test -p spur-core` and `cargo test -p spur-acp`.

---

## Task 1 — F1: `load_session_with_bypass` helper + brain-resume wiring

**Why:** Today the brain-resume path at `orchestrator.rs:1273-1295` calls `connection.load_session(...)` and on success returns the stream without calling `set_session_mode`. For agents using L1b bypass (`claude-code-acp` with `session_mode = "bypassPermissions"`), resumed sessions run in the default mode and rely on L2 auto-approve as the only bypass. This is functionally correct but logs and traces diverge from fresh sessions. F1 restores symmetry.

**Files:**
- Modify: `crates/spur-core/src/skip_perm.rs` (add `load_session_with_bypass`)
- Modify: `crates/spur-core/src/orchestrator.rs:1273-1283` (call the new helper)
- Modify: `crates/spur-core/tests/skip_perm_helper.rs` (add tests)

- [ ] **Step 1: Write the failing tests**

Append to `crates/spur-core/tests/skip_perm_helper.rs`, before the closing `}` of the outer module:

```rust
#[tokio::test]
async fn load_session_skips_set_session_mode_when_flag_off() {
    let mut conn = TrackingConn::default();
    let cfg = make_cfg(false, Some("bypassPermissions"));
    load_session_with_bypass(
        &mut conn,
        &cfg,
        "acp-sess-1".to_string(),
        PathBuf::from("/cwd"),
        vec![],
    )
    .await
    .expect("load_session_with_bypass ok");
    assert_eq!(
        conn.calls,
        vec![("load_session".into(), "acp-sess-1".into())],
        "set_session_mode must not be called when skip_permissions = false",
    );
}

#[tokio::test]
async fn load_session_calls_set_session_mode_when_bypass_and_mode_present() {
    let mut conn = TrackingConn::default();
    let cfg = make_cfg(true, Some("bypassPermissions"));
    load_session_with_bypass(
        &mut conn,
        &cfg,
        "acp-sess-2".to_string(),
        PathBuf::from("/cwd"),
        vec![],
    )
    .await
    .expect("load_session_with_bypass ok");
    assert_eq!(
        conn.calls,
        vec![
            ("load_session".into(), "acp-sess-2".into()),
            ("set_session_mode".into(), "bypassPermissions".into()),
        ],
    );
}

#[tokio::test]
async fn load_session_skips_set_session_mode_when_mode_absent() {
    let mut conn = TrackingConn::default();
    let cfg = make_cfg(true, None);
    load_session_with_bypass(
        &mut conn,
        &cfg,
        "acp-sess-3".to_string(),
        PathBuf::from("/cwd"),
        vec![],
    )
    .await
    .expect("load_session_with_bypass ok");
    assert_eq!(
        conn.calls,
        vec![("load_session".into(), "acp-sess-3".into())],
        "set_session_mode must not be called when no mode configured",
    );
}

#[tokio::test]
async fn load_session_set_session_mode_error_is_non_fatal() {
    let mut conn = TrackingConn {
        fail_set_session_mode: true,
        ..Default::default()
    };
    let cfg = make_cfg(true, Some("bypassPermissions"));
    let res = load_session_with_bypass(
        &mut conn,
        &cfg,
        "acp-sess-4".to_string(),
        PathBuf::from("/cwd"),
        vec![],
    )
    .await;
    assert!(res.is_ok(), "load_session must succeed even if set_session_mode errors");
}
```

Add an import at the top of the file:

```rust
use spur_core::skip_perm::{load_session_with_bypass, new_session_with_bypass};
```

(Replacing the existing `new_session_with_bypass` import.)

Also extend `TrackingConn` to implement `load_session`. Find the existing `impl AgentConnection for TrackingConn` block in that test file and add:

```rust
async fn load_session(
    &mut self,
    request: agent_client_protocol::LoadSessionRequest,
) -> anyhow::Result<std::pin::Pin<Box<dyn futures::Stream<Item = agent_client_protocol::SessionNotification> + Send>>> {
    self.calls
        .push(("load_session".into(), request.session_id.0.to_string()));
    Ok(Box::pin(futures::stream::empty()))
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-core --test skip_perm_helper`
Expected: four new tests FAIL with `cannot find function 'load_session_with_bypass'` and (if the TrackingConn change compiles first) missing behavior.

- [ ] **Step 3: Implement `load_session_with_bypass`**

Append to `crates/spur-core/src/skip_perm.rs`:

```rust
use std::pin::Pin;

use agent_client_protocol::{LoadSessionRequest, SessionNotification};
use futures::Stream;

/// Call `conn.load_session(request)`. If `cfg.skip_permissions` is true
/// and `cfg.skip_permissions_session_mode` is set, then additionally
/// invoke `conn.set_session_mode(...)` with that mode on the loaded
/// session id.
///
/// Errors from `load_session` propagate. Errors from `set_session_mode`
/// are logged at `warn!` and swallowed — L2 auto-approve is the
/// fallback, so a non-honoring agent still bypasses permissions.
///
/// Mirror of `new_session_with_bypass` for the resume path. See
/// `docs/superpowers/specs/2026-04-14-spur-acp-skip-permissions-design.md`
/// for the full mechanism.
pub async fn load_session_with_bypass(
    conn: &mut dyn AgentConnection,
    cfg: &AgentConfig,
    acp_session_id: String,
    cwd: PathBuf,
    mcp_servers: Vec<McpServer>,
) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
    let session_id_for_mode = agent_client_protocol::SessionId(acp_session_id.clone().into());
    let request = LoadSessionRequest::new(session_id_for_mode.clone(), cwd).mcp_servers(mcp_servers);
    let stream = conn.load_session(request).await?;

    if cfg.skip_permissions {
        if let Some(mode) = cfg.skip_permissions_session_mode.as_deref() {
            let req = agent_client_protocol::SetSessionModeRequest::new(
                session_id_for_mode,
                mode.to_string(),
            );
            if let Err(e) = conn.set_session_mode(req).await {
                tracing::warn!(
                    agent = %cfg.name,
                    session_id = %acp_session_id,
                    mode_id = %mode,
                    error = %e,
                    "skip_permissions (load_session): set_session_mode failed; \
                     relying on L2 auto-approve"
                );
            } else {
                tracing::debug!(
                    agent = %cfg.name,
                    session_id = %acp_session_id,
                    mode_id = %mode,
                    "skip_permissions (load_session): set_session_mode applied"
                );
            }
        }
    }

    Ok(stream)
}
```

- [ ] **Step 4: Wire the helper into the brain-resume call site**

In `crates/spur-core/src/orchestrator.rs`, replace the direct `connection.load_session(...)` call at line 1273-1283 with a call to the new helper. Current code:

```rust
let (final_acp_session_id, history_stream, resumed) = match connection
    .load_session(
        LoadSessionRequest::new(acp_session_id.clone(), self.repo_root.clone())
            .mcp_servers(mcp_servers.clone()),
    )
    .await
{
    Ok(stream) => {
        debug!(brain = %brain_name, "load_session succeeded");
        (acp_session_id, Some(stream), true)
    }
```

Change to:

```rust
let (final_acp_session_id, history_stream, resumed) = match crate::skip_perm::load_session_with_bypass(
    &mut *connection,
    &brain_cfg,
    acp_session_id.0.to_string(),
    self.repo_root.clone(),
    mcp_servers.clone(),
)
.await
{
    Ok(stream) => {
        debug!(brain = %brain_name, "load_session succeeded");
        (acp_session_id, Some(stream), true)
    }
```

Note: `acp_session_id` at this point is a `spur_acp::SessionId` (newtype over `Arc<str>`). `.0.to_string()` gives the owned `String` the helper expects. If that's wrong for the current type shape, match the inner type — the function signature in the helper uses `String` to keep the helper agent-agnostic.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p spur-core --test skip_perm_helper`
Expected: all eight tests (four existing + four new) PASS.

- [ ] **Step 6: Full spur-core build**

Run: `cargo build -p spur-core`
Expected: clean build (the orchestrator call site change compiles).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-core/src/skip_perm.rs crates/spur-core/src/orchestrator.rs crates/spur-core/tests/skip_perm_helper.rs
git commit -m "fix(spur-core): re-apply session mode on resumed sessions (F1)

Adds load_session_with_bypass — mirror of new_session_with_bypass for
the brain-resume path. Previously, resumed sessions ran in the default
mode and relied on L2 auto-approve as the only bypass; now L1b is
symmetric across fresh and resumed sessions.

Follow-up F1 from the skip_permissions ship (f47b579).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 2 — F3: Extract shared transport factory

**Why:** `orchestrator.rs` creates an `AgentConnection` from a `transport` field in two places with identical match arms (create_connection at line 1494, run_one_worker_attempt at line 2410). Both will drift when transports change. Extract a private factory.

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (factor out the match)

- [ ] **Step 1: Read current call sites**

Both sites use the same `match config.transport { ... }` tree with four arms (Acp / Stdio / CliWrap / StreamJson). `create_connection` passes `perm_tx` through to the ACP arm and `None` elsewhere. `run_one_worker_attempt` passes `None` to the ACP arm. Signatures differ only by this one parameter.

- [ ] **Step 2: Add the shared factory**

In `crates/spur-core/src/orchestrator.rs`, near the top of the `impl Orchestrator` block (or right above `create_connection`), add a private free function:

```rust
fn build_connection_from_transport(
    config: &spur_acp::config::AgentConfig,
    spawn_args: Vec<String>,
    permission_tx: Option<tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
) -> Box<dyn AgentConnection> {
    match config.transport {
        TransportKind::Acp => Box::new(NativeAcpConnection::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
            permission_tx,
        )),
        TransportKind::Stdio => Box::new(StdioAdapter::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
        )),
        TransportKind::CliWrap => Box::new(CliWrapAdapter::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
        )),
        TransportKind::StreamJson => Box::new(StreamJsonAdapter::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
        )),
    }
}
```

A free function (not a method on `Orchestrator`) is preferred because `run_one_worker_attempt` is already free — both call sites can reach it trivially.

- [ ] **Step 3: Replace `create_connection`'s body**

Replace the `match config.transport { ... }` inside `create_connection` (line 1508-1530) with:

```rust
build_connection_from_transport(config, args, perm_tx)
```

Keep the surrounding computation for `args` and `perm_tx` (L1a and L2 selection). The method signature, the `effective_args()` call, and the `if config.skip_permissions { None } else { permission_tx }` line all stay.

- [ ] **Step 4: Replace the inline match in `run_one_worker_attempt`**

Replace the match at line 2410-2432 with:

```rust
let mut connection: Box<dyn AgentConnection> =
    build_connection_from_transport(agent_config, spawn_args, None);
```

The `spawn_args = agent_config.effective_args()` line above stays.

- [ ] **Step 5: Build and run existing tests**

Run: `cargo test -p spur-core`
Expected: all tests pass (behavior unchanged). Specifically the existing orchestrator integration tests (`cargo test -p spur-core --test ...`) exercise both call sites.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "refactor(spur-core): extract build_connection_from_transport factory (F3)

Deduplicates the match transport { Acp/Stdio/CliWrap/StreamJson }
arms between create_connection (line 1494) and run_one_worker_attempt
(line 2410). Single source of truth for transport dispatch, so adding
a new transport requires one change instead of two.

Follow-up F3 from the skip_permissions ship (f47b579).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 3 — F4: Update stale header on `skip_perm_spike.rs`

**Why:** The example's header comment says "Not a production artifact — delete after the design doc captures the observed matrix." The design doc has shipped and the probe is now a permanent diagnostic. The comment is misleading.

**Files:**
- Modify: `crates/spur-acp/examples/skip_perm_spike.rs:1-26`

- [ ] **Step 1: Rewrite the header comment**

Replace lines 1-25 of `crates/spur-acp/examples/skip_perm_spike.rs` with:

```rust
//! Skip-permissions diagnostic probe.
//!
//! A permanent diagnostic that exercises the per-agent skip_permissions
//! levers against a live agent and reports how many ACP `request_permission`
//! calls round-tripped. Used to:
//!
//!   - Verify a new agent's bypass claims before adding it to the
//!     supported-matrix (do `--trust-all-tools` / `bypassPermissions`
//!     actually suppress permission calls?).
//!   - Catch regressions when upgrading a pinned agent version.
//!
//! Covers two known-good agents today:
//!
//!   C1 (kiro-cli) — `kiro-cli acp --trust-all-tools` should suppress
//!       all ACP `request_permission` calls.
//!   C2 (claude-code-acp) — ACP `set_session_mode("bypassPermissions")`
//!       post-`new_session` should suppress all `request_permission`
//!       calls in practice.
//!
//! Design reference:
//!   docs/superpowers/specs/2026-04-14-spur-acp-skip-permissions-design.md
//!
//! Run:
//!   cargo run -p spur-acp --example skip_perm_spike -- <agent> <mode> [cwd]
//!
//! Where:
//!   agent ∈ { claude-code-acp, kiro }
//!   mode  ∈ { off, args, session, both }
//!   cwd   defaults to the current working directory
//!
//! Each run prints one summary line:
//!   agent=<…> mode=<…> permission_calls=<N> notifs=<N> took=<…>ms outcome=<ok|err>
```

- [ ] **Step 2: Build to confirm it still compiles**

Run: `cargo build -p spur-acp --example skip_perm_spike`
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-acp/examples/skip_perm_spike.rs
git commit -m "docs(spur-acp): clarify skip_perm_spike is a permanent diagnostic (F4)

The header previously said 'delete after the design doc captures the
observed matrix' but the design doc has shipped and the probe has
become the recurring smoke test for new agents and version bumps.

Follow-up F4 from the skip_permissions ship (f47b579).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 4 — Verification

**Files:** none modified.

- [ ] **Step 1: Full workspace test run**

Run: `cargo test --workspace`
Expected: all tests pass. Specifically, the 8 `skip_perm_helper` tests (4 original + 4 from F1) and any integration tests touching `orchestrator::create_connection` or `run_one_worker_attempt` must pass.

- [ ] **Step 2: Scoped clippy**

Run: `cargo clippy -p spur-core -p spur-acp --all-targets -- -D warnings`
Expected: no new warnings attributable to this work. Pre-existing warnings in unrelated files are not in scope.

- [ ] **Step 3: Manual sanity on skip_perm_spike (optional)**

If `kiro-cli` or `claude-code-acp` is installed:

```bash
cargo run -p spur-acp --example skip_perm_spike -- claude-code-acp off
cargo run -p spur-acp --example skip_perm_spike -- claude-code-acp both
```

Expected output matches the design doc's matrix: `off` shows permission_calls ≥ 1, `both` shows permission_calls = 0.

---

## Self-Review Checklist

- **Scope:** three independent commits (F1, F3, F4) + verification. Each can be reverted without affecting the others. ✓
- **Placeholder scan:** no TBD/TODO; every code block shows the actual change. ✓
- **Type consistency:** `load_session_with_bypass` signature uses `String` for the session id (agent-agnostic helper). Call site in orchestrator adapts via `.0.to_string()`. `build_connection_from_transport` is a free function, consistent with `run_one_worker_attempt`'s shape. ✓
- **Test coverage:** F1 gets 4 new tests mirroring the `new_session_with_bypass` test style. F3 is a refactor covered by existing integration tests. F4 is docs-only. ✓
- **Non-goals hit:** no config schema changes (that's Spec 1 territory), no new hooks, no behavior change beyond F1's log/trace symmetry. ✓
