# Claude ACP Adoption Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Adopt `@agentclientprotocol/claude-agent-acp` as Spur's Claude Code transport via the existing `NativeAcpConnection`, reaching usable feature parity (plan-mode, usage, commands, permission gating) in ~5 engineer-days without reimplementing the Claude Agent SDK in Rust.

**Architecture:** Configure Claude as an ACP agent (`transport = "acp"`, `command = "npx"`) so it flows through the production `NativeAcpConnection` path. Fix the child-stderr gap by writing per-session log files. Extend the `AgentConnection` trait narrowly with `set_session_mode` + `authenticate` to power a plan-mode toggle and clear auth-error messages. Add read-only TUI rendering for `UsageUpdate`, `CurrentModeUpdate`, and `AvailableCommandsUpdate` notifications the reference already emits. Defer fork/resume/model/auth-UI to follow-up specs.

**Tech Stack:** Rust 2021 workspace · `agent-client-protocol` crate v0.10 · `ratatui` TUI · `tokio` async runtime · `@agentclientprotocol/claude-agent-acp` npm binary run via `npx`.

---

## File Structure

**Files modified:**
- `Cargo.toml` — enable `unstable_session_usage` feature on `agent-client-protocol`
- `.spur/config.toml` — add `claude-code-acp` agent profile
- `crates/spur-acp/src/connection/native.rs` — pipe child stderr to a per-session log file; add `set_session_mode` + `authenticate` dispatch
- `crates/spur-acp/src/connection/mod.rs` — add `set_session_mode` + `authenticate` to `AgentConnection` trait with default stubs
- `crates/spur-acp/src/connection/stream_json_adapter.rs` — comment-only: clarify non-Claude usage
- `crates/spur-acp/src/lib.rs` — re-export `CurrentModeUpdate`, `AvailableCommandsUpdate`, `UsageUpdate`, `SetSessionModeRequest`, `SessionModeId`, `AuthenticateRequest`
- `crates/spur-tui/src/components/status_bar.rs` — render mode indicator + context%/cost if present
- `crates/spur-tui/src/views/session_detail.rs` — handle new `SessionUpdate` variants; bind `Esc-m` keystroke
- `crates/spur-tui/src/app.rs` — add session state fields (`current_mode`, `available_commands`, `context_used/size`) and forward new events
- `docs/superpowers/specs/2026-04-13-claude-acp-adoption-design.md` — already exists

**Files created:**
- `crates/spur-acp/examples/compat_spike.rs` — M0 standalone harness (NOT committed to production if results are negative)
- `docs/spur/claude-code-acp-setup.md` — operator doc

**Files left untouched:** `spur-mcp`, `spur-worktree`, `spur-cli`, `spur-cost`. Plumbing already correct (`orchestrator.rs` passes `mcp_servers` at `:219,835,920,942`).

---

## Task 0: Milestone M0 — Protocol-compat spike

**Files:**
- Create: `crates/spur-acp/examples/compat_spike.rs`
- Modify: `crates/spur-acp/Cargo.toml` (add example target)

Goal: confirm `NativeAcpConnection` can drive `claude-agent-acp` end-to-end, and measure npx cold-start. Kills F1+F2 before any production code changes. Time-box: half a day.

- [ ] **Step 1: Create the example file**

Create `crates/spur-acp/examples/compat_spike.rs`:

```rust
//! M0 protocol-compat spike.
//!
//! Drives `claude-agent-acp` via `NativeAcpConnection` and reports which ACP
//! methods round-trip. Not a production artifact — delete after M0 gate passes.
//!
//! Run: cargo run -p spur-acp --example compat_spike -- <path-to-cwd>

use std::path::PathBuf;
use std::time::Instant;

use agent_client_protocol::{
    AuthenticateRequest, AuthMethodId, ClientCapabilities, ContentBlock, InitializeRequest,
    PromptRequest, ProtocolVersion, SetSessionModeRequest, TextContent,
};
use spur_acp::connection::{AgentConnection, NativeAcpConnection};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("spur_acp=debug,compat_spike=info")
        .init();

    let cwd: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));

    println!("=== M0 compat spike: claude-agent-acp ===\n");

    let mut conn = NativeAcpConnection::new(
        "claude-code-acp-spike",
        "npx",
        vec![
            "--yes".to_string(),
            "@agentclientprotocol/claude-agent-acp@latest".to_string(),
        ],
        None,
    );

    let t0 = Instant::now();
    let init = conn
        .initialize(InitializeRequest::new(ProtocolVersion::LATEST))
        .await?;
    println!(
        "[ok] initialize in {:?}, protocol={:?}, caps={:?}",
        t0.elapsed(),
        init.protocol_version,
        init.agent_capabilities
    );

    let t1 = Instant::now();
    let session = conn.new_session(cwd.clone(), vec![]).await?;
    println!("[ok] new_session in {:?}: {}", t1.elapsed(), session.session_id);

    let prompt_req = PromptRequest::new(
        session.session_id.clone(),
        vec![ContentBlock::Text(TextContent::new(
            "Say only the word OK and nothing else.".to_string(),
        ))],
    );
    let mut stream = conn.prompt(prompt_req).await?;
    use futures::StreamExt;
    let mut chunks = 0usize;
    while let Some(_notif) = stream.next().await {
        chunks += 1;
        if chunks > 200 {
            break;
        }
    }
    println!("[ok] prompt streamed {chunks} notifications");

    let mode_req = SetSessionModeRequest::new(session.session_id.clone(), "plan");
    match conn.set_session_mode(mode_req).await {
        Ok(_) => println!("[ok] set_session_mode(plan)"),
        Err(e) => println!("[WARN] set_session_mode failed: {e}"),
    }

    match conn
        .authenticate(AuthenticateRequest::new(AuthMethodId("claude-ai-login".into())))
        .await
    {
        Ok(_) => println!("[ok] authenticate echoed"),
        Err(e) => println!("[info] authenticate returned: {e} (expected if not wired)"),
    }

    conn.shutdown().await?;
    println!("\n=== spike complete ===");
    Ok(())
}
```

- [ ] **Step 2: Wire the example into the crate**

Edit `crates/spur-acp/Cargo.toml`. After `[dependencies]` block, append:

```toml

[dev-dependencies]
tracing-subscriber = { workspace = true, features = ["env-filter"] }

[[example]]
name = "compat_spike"
path = "examples/compat_spike.rs"
```

Verify `tracing-subscriber` is declared in the workspace root `Cargo.toml`. If not, add it under `[workspace.dependencies]`: `tracing-subscriber = { version = "0.3", default-features = false, features = ["fmt", "env-filter"] }`.

- [ ] **Step 3: Build the example**

Run: `cargo build -p spur-acp --example compat_spike`
Expected: clean build, no warnings blocking progress. This proves the harness compiles against Spur's actual `NativeAcpConnection` surface — the only thing that matters at this point. If the build fails on a missing method (e.g. `set_session_mode`), that's expected now because Task 3 adds it. In that case, comment out the `set_session_mode` block in the spike, note the result, and proceed.

- [ ] **Step 4: Run the spike**

Run: `cargo run -p spur-acp --example compat_spike`
Expected output: all four `[ok]` lines. Record:
- `initialize` duration (informs npx cold-start concern).
- Whether `set_session_mode` succeeded (may fail in this first run if Task 3 isn't done yet — re-run after Task 3).
- Any unexpected stderr noise in the terminal (this is the BS2 problem in action; Task 2 fixes it).

If `initialize` or `new_session` hangs >30s or fails: STOP. The pinned `claude-agent-acp` version is incompatible with our ACP crate. Try pinning to a known-good version (`@agentclientprotocol/claude-agent-acp@0.3.2` or similar — check npm for the version that matches ACP schema v0.11).

- [ ] **Step 5: Commit the spike**

```bash
git add crates/spur-acp/examples/compat_spike.rs crates/spur-acp/Cargo.toml
git commit -m "feat(spur-acp): add M0 compat spike for claude-agent-acp"
```

---

## Task 1: Enable required ACP crate feature flags

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/spur-acp/Cargo.toml`

Goal: enable `unstable_session_usage` so `SessionUpdate::UsageUpdate` is visible to Spur. Other `unstable_*` features (fork, resume, etc.) are deferred per the spec.

- [ ] **Step 1: Edit workspace Cargo.toml**

Open `/Volumes/Projects/spur/Cargo.toml`. Locate the line:

```toml
agent-client-protocol = "0.10"
```

Replace with:

```toml
agent-client-protocol = { version = "0.10", features = ["unstable_session_usage"] }
```

- [ ] **Step 2: Verify workspace resolution**

Run: `cargo check -p spur-acp`
Expected: clean check. If a downstream crate uses an API that moved behind another feature gate, the compiler will tell you exactly which.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "chore(deps): enable unstable_session_usage on agent-client-protocol"
```

---

## Task 2: Capture subprocess stderr to a per-session log file

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs:486` (and surrounding spawn logic)

Goal: replace `Stdio::inherit()` with a pipe that writes the child's stderr to `.spur/logs/<agent>-<session_id>-acp.log`. Fixes BS2: TUI corruption from Node subprocess logs.

Note: `session_id` is assigned inside `new_session` (not at `initialize` time). The log path uses `<agent>-<pid>-acp.log` until a session exists, then rolls on first `new_session`. Implementation below uses `<agent>-<timestamp>-acp.log` to avoid touching session bookkeeping for the log file path.

- [ ] **Step 1: Write a failing unit test**

Append to `crates/spur-acp/src/connection/native.rs` (top of existing `#[cfg(test)] mod tests` block, or create one if missing):

```rust
#[cfg(test)]
mod stderr_capture_tests {
    use super::*;

    #[test]
    fn log_path_uses_spur_logs_directory() {
        let path = build_acp_log_path("claude-code-acp");
        assert!(
            path.to_string_lossy().contains(".spur/logs/"),
            "expected log under .spur/logs/, got {}",
            path.display()
        );
        assert!(
            path.to_string_lossy().ends_with("-acp.log"),
            "expected -acp.log suffix, got {}",
            path.display()
        );
        assert!(
            path.to_string_lossy().contains("claude-code-acp"),
            "expected agent name in path, got {}",
            path.display()
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-acp --lib stderr_capture_tests`
Expected: `error[E0425]: cannot find function build_acp_log_path`.

- [ ] **Step 3: Add the helper**

In `crates/spur-acp/src/connection/native.rs`, just before `impl NativeAcpConnection` (around line 100), add:

```rust
/// Compute the path where the ACP subprocess's stderr should be written.
/// Uses `.spur/logs/<agent>-<timestamp>-acp.log` relative to CWD.
fn build_acp_log_path(agent_name: &str) -> std::path::PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    std::path::PathBuf::from(".spur/logs").join(format!("{agent_name}-{ts}-acp.log"))
}
```

- [ ] **Step 4: Run test — expect pass**

Run: `cargo test -p spur-acp --lib stderr_capture_tests`
Expected: `test stderr_capture_tests::log_path_uses_spur_logs_directory ... ok`.

- [ ] **Step 5: Replace Stdio::inherit() in the spawn block**

In `crates/spur-acp/src/connection/native.rs`, locate the block around line 482-487:

```rust
        let child_result = tokio::process::Command::new(&command)
            .args(&extra_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn();
```

Replace with:

```rust
        let log_path = build_acp_log_path(&agent_name);
        if let Some(parent) = log_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    agent = %agent_name,
                    path = %parent.display(),
                    error = %e,
                    "NativeAcpConnection: failed to create log directory; falling back to inherit",
                );
            }
        }
        tracing::info!(
            agent = %agent_name,
            log_path = %log_path.display(),
            "NativeAcpConnection: capturing child stderr to log file"
        );
        let stderr_cfg = match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_path)
        {
            Ok(f) => std::process::Stdio::from(f),
            Err(e) => {
                tracing::warn!(
                    agent = %agent_name,
                    path = %log_path.display(),
                    error = %e,
                    "NativeAcpConnection: failed to open stderr log; falling back to inherit",
                );
                std::process::Stdio::inherit()
            }
        };

        let child_result = tokio::process::Command::new(&command)
            .args(&extra_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(stderr_cfg)
            .spawn();
```

- [ ] **Step 6: Build**

Run: `cargo build -p spur-acp`
Expected: clean build.

- [ ] **Step 7: Rerun the spike**

Run: `cargo run -p spur-acp --example compat_spike`
Expected: terminal no longer shows SDK noise lines. A new file exists at `.spur/logs/claude-code-acp-spike-<timestamp>-acp.log` with the subprocess's stderr content. Verify with `ls -la .spur/logs/` and `head .spur/logs/claude-code-acp-spike-*-acp.log`.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "fix(spur-acp): capture ACP subprocess stderr to per-session log file

Previously stderr was inherited, corrupting the TUI with SDK logs from
Node-based ACP agents (claude-agent-acp). Pipes to
.spur/logs/<agent>-<timestamp>-acp.log instead, with inherit fallback
if the log file can't be opened."
```

---

## Task 3: Extend AgentConnection trait with set_session_mode + authenticate

**Files:**
- Modify: `crates/spur-acp/src/connection/mod.rs`
- Modify: `crates/spur-acp/src/connection/native.rs`
- Modify: `crates/spur-acp/src/lib.rs`

Goal: add two methods to the trait (with default `"unsupported"` impls) and wire them through `NativeAcpConnection`'s LocalSet dispatcher. Other ACP methods stay untouched per spec.

- [ ] **Step 1: Write a failing test for the trait default**

Append to `crates/spur-acp/src/connection/mod.rs`:

```rust
#[cfg(test)]
mod agent_connection_defaults {
    use super::*;
    use agent_client_protocol::{AuthenticateRequest, AuthMethodId, SetSessionModeRequest};

    struct NullConn;

    #[async_trait]
    impl AgentConnection for NullConn {
        async fn initialize(&mut self, _r: InitializeRequest) -> anyhow::Result<InitializeResponse> {
            unimplemented!()
        }
        async fn new_session(
            &mut self,
            _cwd: PathBuf,
            _mcp: Vec<McpServer>,
        ) -> anyhow::Result<NewSessionResponse> {
            unimplemented!()
        }
        async fn prompt(
            &mut self,
            _r: PromptRequest,
        ) -> anyhow::Result<std::pin::Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
            unimplemented!()
        }
        async fn cancel(&mut self, _s: &str) -> anyhow::Result<()> { unimplemented!() }
        async fn shutdown(&mut self) -> anyhow::Result<()> { unimplemented!() }
        fn health(&self) -> crate::types::AgentHealth { unimplemented!() }
    }

    #[tokio::test]
    async fn set_session_mode_default_is_unsupported() {
        let mut c = NullConn;
        let req = SetSessionModeRequest::new(
            agent_client_protocol::SessionId::new("s".to_string()),
            "plan",
        );
        let err = c.set_session_mode(req).await.unwrap_err().to_string();
        assert!(err.contains("not supported"), "got: {err}");
    }

    #[tokio::test]
    async fn authenticate_default_is_unsupported() {
        let mut c = NullConn;
        let req = AuthenticateRequest::new(AuthMethodId("x".into()));
        let err = c.authenticate(req).await.unwrap_err().to_string();
        assert!(err.contains("not supported"), "got: {err}");
    }
}
```

- [ ] **Step 2: Verify tests fail**

Run: `cargo test -p spur-acp --lib agent_connection_defaults`
Expected: compile error — trait has no `set_session_mode` or `authenticate` methods yet.

- [ ] **Step 3: Add trait methods with default impls**

In `crates/spur-acp/src/connection/mod.rs`, update the `use agent_client_protocol::{...}` block at top (around line 30) to include the new types:

```rust
use agent_client_protocol::{
    AuthenticateRequest, AuthenticateResponse, InitializeRequest, InitializeResponse,
    ListSessionsRequest, ListSessionsResponse, LoadSessionRequest, McpServer, NewSessionResponse,
    PromptRequest, SessionNotification, SetSessionModeRequest, SetSessionModeResponse,
};
```

Inside the `pub trait AgentConnection` block (at the bottom, after `list_sessions`), append:

```rust
    /// Set the current mode of a session (e.g. `"plan"`, `"default"`).
    ///
    /// Not all transports support this; the default implementation returns an error.
    async fn set_session_mode(
        &mut self,
        request: SetSessionModeRequest,
    ) -> anyhow::Result<SetSessionModeResponse> {
        let _ = request;
        Err(anyhow::anyhow!("set_session_mode not supported by this transport"))
    }

    /// Authenticate with the agent using a previously-advertised auth method.
    ///
    /// Not all transports support this; the default implementation returns an error.
    async fn authenticate(
        &mut self,
        request: AuthenticateRequest,
    ) -> anyhow::Result<AuthenticateResponse> {
        let _ = request;
        Err(anyhow::anyhow!("authenticate not supported by this transport"))
    }
```

- [ ] **Step 4: Run trait-default tests — expect pass**

Run: `cargo test -p spur-acp --lib agent_connection_defaults`
Expected: 2 passed.

- [ ] **Step 5: Add AcpCommand variants for the two new methods**

In `crates/spur-acp/src/connection/native.rs`, locate the `enum AcpCommand` block (around line 59-89). After the `ListSessions` variant, append:

```rust
    SetSessionMode {
        request: SetSessionModeRequest,
        reply: oneshot::Sender<anyhow::Result<SetSessionModeResponse>>,
    },
    Authenticate {
        request: AuthenticateRequest,
        reply: oneshot::Sender<anyhow::Result<AuthenticateResponse>>,
    },
```

Update the `use agent_client_protocol::{...}` import block at the top of `native.rs` (around line 38-51) to include `AuthenticateRequest, AuthenticateResponse, SetSessionModeRequest, SetSessionModeResponse`.

- [ ] **Step 6: Add dispatcher arms on the LocalSet thread**

In `crates/spur-acp/src/connection/native.rs`, find the main command-dispatch `match` block inside the `local.block_on(&rt, async move { ... while let Some(cmd) = cmd_rx.recv().await { match cmd { ... } } })`. The existing arms handle `Initialize`, `NewSession`, `Prompt`, `Cancel`, `Shutdown`, `LoadSession`, `ListSessions`. After `ListSessions`, add:

```rust
                        AcpCommand::SetSessionMode { request, reply } => {
                            let result = connection
                                .set_session_mode(request)
                                .await
                                .map_err(|e| anyhow::anyhow!("set_session_mode failed: {e}"));
                            let _ = reply.send(result);
                        }
                        AcpCommand::Authenticate { request, reply } => {
                            let result = connection
                                .authenticate(request)
                                .await
                                .map_err(|e| anyhow::anyhow!("authenticate failed: {e}"));
                            let _ = reply.send(result);
                        }
```

If the exact syntax differs from the existing arms (e.g. the connection is accessed via a wrapper), mirror whatever pattern `ListSessions` uses — it's the closest analog.

- [ ] **Step 7: Override trait methods on NativeAcpConnection**

In `crates/spur-acp/src/connection/native.rs`, find the `impl AgentConnection for NativeAcpConnection` block. After the `list_sessions` override (following the pattern used there), append:

```rust
    async fn set_session_mode(
        &mut self,
        request: SetSessionModeRequest,
    ) -> anyhow::Result<SetSessionModeResponse> {
        let cmd_tx = self
            .cmd_tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("NativeAcpConnection '{}': not initialized", self.agent_name))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(AcpCommand::SetSessionMode { request, reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("NativeAcpConnection '{}': ACP thread gone", self.agent_name))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("NativeAcpConnection '{}': reply channel closed", self.agent_name))?
    }

    async fn authenticate(
        &mut self,
        request: AuthenticateRequest,
    ) -> anyhow::Result<AuthenticateResponse> {
        let cmd_tx = self
            .cmd_tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("NativeAcpConnection '{}': not initialized", self.agent_name))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(AcpCommand::Authenticate { request, reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("NativeAcpConnection '{}': ACP thread gone", self.agent_name))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("NativeAcpConnection '{}': reply channel closed", self.agent_name))?
    }
```

- [ ] **Step 8: Re-export the new types from spur-acp**

In `crates/spur-acp/src/lib.rs`, locate the re-export block (around line 19-28). Add to the existing `pub use agent_client_protocol::{...}` line:

```rust
pub use agent_client_protocol::{
    AuthenticateRequest, AuthenticateResponse, AuthMethodId, AvailableCommandsUpdate,
    ContentBlock, CurrentModeUpdate, ListSessionsRequest, LoadSessionRequest, Plan, PlanEntry,
    PlanEntryPriority, PlanEntryStatus, PermissionOption, RequestPermissionRequest,
    SelectedPermissionOutcome, SessionId, SessionInfo, SessionModeId, SessionNotification,
    SessionUpdate, SetSessionModeRequest, SetSessionModeResponse, TextContent, ToolCall,
    ToolCallStatus, ToolCallUpdate,
};
```

Adjust to include whatever is already there; the goal is the new names (`AuthenticateRequest`, `AvailableCommandsUpdate`, `CurrentModeUpdate`, `SessionModeId`, `SetSessionModeRequest`, `SetSessionModeResponse`, `AuthMethodId`) are exposed.

Also add the usage type behind cfg:

```rust
#[cfg(feature = "unstable_session_usage")]
pub use agent_client_protocol::UsageUpdate;
```

Only keep this cfg line if spur-acp's Cargo.toml reflects the feature; since Task 1 enabled it workspace-wide, unconditional `pub use agent_client_protocol::UsageUpdate;` is simpler and fine. Use the unconditional form.

- [ ] **Step 9: Build + run full test suite**

Run: `cargo build -p spur-acp && cargo test -p spur-acp`
Expected: clean build, all tests pass. The new `agent_connection_defaults` tests and the earlier `stderr_capture_tests` both pass.

- [ ] **Step 10: Commit**

```bash
git add crates/spur-acp/src/connection/mod.rs \
        crates/spur-acp/src/connection/native.rs \
        crates/spur-acp/src/lib.rs
git commit -m "feat(spur-acp): add set_session_mode and authenticate to AgentConnection

Narrow extension of the transport trait to power plan-mode toggle and
auth-required error surfacing for claude-agent-acp. Other advanced
ACP methods (fork/resume/close/model/config) intentionally deferred
to follow-up specs until a concrete TUI consumer needs them."
```

---

## Task 4: Render UsageUpdate, CurrentModeUpdate, AvailableCommandsUpdate in the TUI

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Modify: `crates/spur-tui/src/components/status_bar.rs`

Goal: read-only display. No interactive toggles yet (Task 5 adds the mode toggle). The catchall arm uses `_` with a debug-log so unknown variants don't break the UI.

- [ ] **Step 1: Add session state fields for the new data**

In `crates/spur-tui/src/app.rs`, locate the session-state struct (search for the struct that already holds per-session fields — likely something owning the transcript or per-`SessionId` state). Add these fields next to existing ones:

```rust
    pub current_mode: Option<String>,
    pub available_commands: Vec<String>,
    pub context_used: Option<u64>,
    pub context_size: Option<u64>,
```

If the struct is named e.g. `SessionState`, place these at the end. Default to `None` / empty `Vec` in any `Default` impl. Search the struct's initializer and add field assignments. The exact struct and initializer location depend on existing code; grep `pub struct.*Session` in `crates/spur-tui/src/app.rs` to find it.

- [ ] **Step 2: Write a failing test for variant handling**

Create `crates/spur-tui/tests/session_update_handling.rs` (new file):

```rust
//! Confirms unknown SessionUpdate variants don't crash the TUI.
//!
//! Smoke-level: we only assert that handlers map the three new variants to
//! expected state mutations. Full rendering is validated manually.

use spur_acp::{
    AvailableCommandsUpdate, ContentChunk, CurrentModeUpdate, SessionId, SessionNotification,
    SessionUpdate, TextContent, UsageUpdate,
};

fn nid() -> SessionId {
    SessionId::new("test".to_string())
}

#[test]
fn current_mode_update_sets_mode() {
    let update = SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new("plan"));
    let notif = SessionNotification::new(nid(), update);
    // apply_notification is the TUI helper we will add in Step 3.
    let mut s = spur_tui::test_support::new_session_state();
    spur_tui::test_support::apply_notification(&mut s, &notif);
    assert_eq!(s.current_mode.as_deref(), Some("plan"));
}

#[test]
fn available_commands_update_stores_names() {
    use agent_client_protocol::{AvailableCommand, AvailableCommandInput};
    let cmds = vec![
        AvailableCommand {
            name: "compact".to_string(),
            description: "compress context".to_string(),
            input: None,
            meta: None,
        },
    ];
    let update = SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(cmds));
    let notif = SessionNotification::new(nid(), update);
    let mut s = spur_tui::test_support::new_session_state();
    spur_tui::test_support::apply_notification(&mut s, &notif);
    assert_eq!(s.available_commands, vec!["compact".to_string()]);
}
```

Check the actual `AvailableCommand` shape in `agent_client_protocol_schema-0.11.4/src/client.rs` around the `AvailableCommandsUpdate` definition — adjust struct-literal fields if they differ. The test exists to lock in the mapping, not the exact shape.

- [ ] **Step 3: Add the test_support module and apply_notification helper**

In `crates/spur-tui/src/lib.rs`, add (after existing module declarations):

```rust
#[doc(hidden)]
pub mod test_support {
    use crate::app::SessionState; // adjust to actual struct name if different
    use spur_acp::{SessionNotification, SessionUpdate};

    pub fn new_session_state() -> SessionState {
        SessionState::default()
    }

    pub fn apply_notification(state: &mut SessionState, notif: &SessionNotification) {
        crate::app::apply_session_update(state, &notif.update);
    }
}
```

In `crates/spur-tui/src/app.rs`, add a free function `apply_session_update`:

```rust
pub(crate) fn apply_session_update(state: &mut SessionState, update: &spur_acp::SessionUpdate) {
    use spur_acp::SessionUpdate::*;
    match update {
        CurrentModeUpdate(u) => {
            state.current_mode = Some(u.current_mode_id.to_string());
        }
        AvailableCommandsUpdate(u) => {
            state.available_commands = u
                .available_commands
                .iter()
                .map(|c| c.name.clone())
                .collect();
        }
        UsageUpdate(u) => {
            state.context_used = Some(u.used);
            state.context_size = Some(u.size);
        }
        _ => {
            // Existing variants handled by the view's render path; unknowns
            // logged for triage.
            tracing::trace!("apply_session_update: unhandled variant");
        }
    }
}
```

Cross-check the actual field names on `UsageUpdate` (grep `pub struct UsageUpdate` in the schema crate). Adjust `u.used` / `u.size` to the real identifiers if different (likely `u.used` and `u.size` per the ACP schema convention).

Also double-check: `u.available_commands` vs whatever the field is named — open `agent_client_protocol_schema-0.11.4/src/client.rs` and search `AvailableCommandsUpdate`. Update the literal accordingly.

- [ ] **Step 4: Wire apply_session_update into the existing notification flow**

In `crates/spur-tui/src/views/session_detail.rs` around line 285 where the `match &notification.update { ... }` block lives, insert a call to `apply_session_update` **before** the existing match:

```rust
    crate::app::apply_session_update(state, &notification.update);
    match &notification.update {
        // ... existing arms for AgentMessageChunk, ToolCall, etc. unchanged
        _ => {
            tracing::trace!(
                "SessionDetail: unhandled SessionUpdate variant, display-only"
            );
        }
    }
```

The `state` reference at that point should be the session state containing the new fields. Adapt the exact argument to whatever lives in scope (the function likely takes `&mut self` with a `session_states: HashMap<SessionId, SessionState>`; fetch the entry by `notification.session_id` before applying).

- [ ] **Step 5: Render mode and usage in the status bar**

In `crates/spur-tui/src/components/status_bar.rs`, extend the `render` signature:

```rust
    pub fn render(
        frame: &mut Frame,
        area: Rect,
        view: &ViewId,
        total_cost: f64,
        elapsed: &str,
        current_mode: Option<&str>,
        context_used: Option<u64>,
        context_size: Option<u64>,
    ) {
```

Before the `frame.render_widget(...)` call, build a mode/usage fragment:

```rust
        let mode_text = current_mode
            .filter(|m| !m.is_empty())
            .map(|m| format!(" [{m}]"))
            .unwrap_or_default();

        let usage_text = match (context_used, context_size) {
            (Some(used), Some(size)) if size > 0 => {
                let pct = (used as f64 / size as f64) * 100.0;
                format!(" ctx {:.0}%", pct)
            }
            _ => String::new(),
        };
```

Append `Span::styled(mode_text, Style::default().fg(Color::Magenta))` and `Span::styled(usage_text, Style::default().fg(Color::LightBlue))` to the existing `Line::from(vec![...])` between `Span::raw("  ")` and the `"SPUR"` span.

Update every caller of `StatusBar::render` (grep `StatusBar::render` in `spur-tui/src`) to pass the new args. Most call sites can pass `None, None, None` until M4; the session_detail call site passes real values from `SessionState`.

- [ ] **Step 6: Run tests**

Run: `cargo test -p spur-tui`
Expected: new `session_update_handling` tests pass. Preexisting tests still pass.

- [ ] **Step 7: Manual visual check**

Run: `cargo run -p spur-cli -- tui` (or whatever command launches the TUI).
Expected: after launching a Claude session via a `claude-code-acp` profile (next task), the status bar shows mode + ctx% + cost. For this task alone the fields are `None` until Task 5 profile exists — just confirm the TUI still renders cleanly with the new `None` branches.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/src/app.rs \
        crates/spur-tui/src/lib.rs \
        crates/spur-tui/src/components/status_bar.rs \
        crates/spur-tui/src/views/session_detail.rs \
        crates/spur-tui/tests/session_update_handling.rs
git commit -m "feat(spur-tui): render CurrentModeUpdate, UsageUpdate, AvailableCommandsUpdate

Read-only display in the status bar (mode indicator, ctx%). Interactive
mode toggle lands in the next task. Unknown SessionUpdate variants log
at TRACE level so protocol evolution doesn't crash the UI."
```

---

## Task 5: Add claude-code-acp agent profile + smoke test

**Files:**
- Modify: `.spur/config.toml`

Goal: add a working agent profile that routes to `NativeAcpConnection`. Pin the upstream version. Document the pin.

- [ ] **Step 1: Edit the example config**

Append to `.spur/config.toml` (after the existing `[[agents.entries]]` blocks, before `[failover]`):

```toml
# Claude Code via the upstream ACP wrapper. Preferred transport: gives us
# plan mode, usage tracking, slash-command discovery, and full permission
# gating via the SDK. Requires Node.js on PATH.
#
# The pinned version is critical — @latest would silently pull breaking
# changes mid-session.
[[agents.entries]]
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.3.2"]
transport = "acp"
role = "both"
capabilities = []
cost_tier = "medium"
```

Adjust `0.3.2` to whichever version the M0 spike verified as working. If the spike used `@latest`, replace `latest` with the actual version reported by `npm view @agentclientprotocol/claude-agent-acp version`.

- [ ] **Step 2: Set the brain default to the new profile (optional, behind a comment)**

Near the top of `.spur/config.toml`, change:

```toml
[brain]
default = "claude-code"
fallback = ["kiro"]
```

to:

```toml
[brain]
# To try the new ACP-based Claude transport, flip `default` to
# "claude-code-acp". Keeping the stream-json one as default during rollout.
default = "claude-code"
fallback = ["claude-code-acp", "kiro"]
```

- [ ] **Step 3: Smoke test**

Run: `cargo run -p spur-cli -- tui`
In the TUI, start a brain session explicitly against the `claude-code-acp` profile (via the existing agent-selection UI or by temporarily setting `default = "claude-code-acp"`). Type `hello`. Expected:
1. A new file appears under `.spur/logs/claude-code-acp-*-acp.log` with SDK startup lines.
2. The TUI transcript receives the assistant's reply as `agent_message_chunk` notifications.
3. The status bar shows the mode indicator once claude-agent-acp emits `CurrentModeUpdate` (may take the first turn).
4. Triggering a file-write tool in the prompt (e.g., "create a file named /tmp/spur-test.txt with content ok") surfaces a permission dialog via Spur's existing permission UI.

Record any failures. Compat issues surface as either a blank transcript (protocol version mismatch — check the log file) or a hang on initialize (npx resolution failing — run `npx --yes @agentclientprotocol/claude-agent-acp@0.3.2 --help` separately to confirm the binary works).

- [ ] **Step 4: Commit**

```bash
git add .spur/config.toml
git commit -m "feat(config): add claude-code-acp agent profile

Routes Claude Code through NativeAcpConnection via the upstream
@agentclientprotocol/claude-agent-acp wrapper. Pinned to a specific
version — @latest would silently pull breaking changes. Left as a
fallback for now; switch [brain] default after the profile bakes."
```

---

## Task 6: Plan-mode toggle keystroke + auth-required error surfacing

**Files:**
- Modify: `crates/spur-tui/src/action.rs` (or wherever actions are defined)
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Modify: `crates/spur-core/src/orchestrator.rs`

Goal: bind `Esc-m` (or `Ctrl-P` — pick based on existing keymap conventions). Send a `SetSessionMode` request via the orchestrator. Surface `authRequired` errors as a dismissable banner.

- [ ] **Step 1: Write a failing test for the action-dispatch path**

Create `crates/spur-tui/tests/mode_toggle_action.rs`:

```rust
use spur_tui::action::Action;

#[test]
fn mode_toggle_action_exists() {
    // If this compiles, the variant exists. The integration of the action
    // into the view key handler is smoke-tested manually.
    let _ = Action::TogglePlanMode;
}
```

- [ ] **Step 2: Run it to fail**

Run: `cargo test -p spur-tui --test mode_toggle_action`
Expected: compile error — `TogglePlanMode` not found.

- [ ] **Step 3: Add the action variant**

In `crates/spur-tui/src/action.rs`, add a variant to the `Action` enum:

```rust
    TogglePlanMode,
```

- [ ] **Step 4: Verify the test compiles**

Run: `cargo test -p spur-tui --test mode_toggle_action`
Expected: 1 passed.

- [ ] **Step 5: Bind the keystroke in the session-detail view**

In `crates/spur-tui/src/views/session_detail.rs`, find the key-handling match block (grep `KeyEvent` or `KeyCode::`). Add:

```rust
            KeyCode::Char('m') if modifiers.contains(KeyModifiers::ALT) => {
                Some(Action::TogglePlanMode)
            }
```

Use `ALT` (matches `Esc-m` on most terminals) or follow the existing convention for similar toggles (inspect other bindings in the same file).

Also update the status-bar hints string in `crates/spur-tui/src/components/status_bar.rs` to include `[Alt-m]plan` under the `SessionDetail(_)` branch.

- [ ] **Step 6: Forward the action to the orchestrator**

In `crates/spur-tui/src/app.rs`, locate the dispatch match that handles `Action` variants (grep `Action::` uses). Add:

```rust
            Action::TogglePlanMode => {
                if let Some(session_id) = self.current_session_id() {
                    let next_mode = match self.session_state(&session_id)
                        .and_then(|s| s.current_mode.as_deref())
                    {
                        Some("plan") => "default",
                        _ => "plan",
                    };
                    let _ = self
                        .orchestrator_tx
                        .send(OrchestratorRequest::SetSessionMode {
                            session_id: session_id.clone(),
                            mode_id: next_mode.to_string(),
                        })
                        .await;
                }
            }
```

The exact names (`orchestrator_tx`, `OrchestratorRequest`, `current_session_id`, `session_state`) depend on existing code — inspect `app.rs` for the analogous prompt-send dispatch (it will already use a similar pattern).

- [ ] **Step 7: Add the orchestrator command + handler**

In `crates/spur-core/src/orchestrator.rs`, find the enum that carries requests from the TUI (e.g. `OrchestratorRequest` or similar). Add:

```rust
    SetSessionMode {
        session_id: spur_acp::SessionId,
        mode_id: String,
    },
```

In the handler dispatch match, add:

```rust
            OrchestratorRequest::SetSessionMode { session_id, mode_id } => {
                let req = agent_client_protocol::SetSessionModeRequest::new(
                    session_id.clone(),
                    mode_id.clone(),
                );
                match connection.set_session_mode(req).await {
                    Ok(_) => tracing::info!(session = %session_id, mode = %mode_id, "set_session_mode ok"),
                    Err(e) => tracing::warn!(session = %session_id, error = %e, "set_session_mode failed"),
                }
            }
```

Replace `connection` with whatever variable holds the `AgentConnection` for the session (often looked up by session id). The orchestrator's existing `prompt` dispatch shows the correct pattern.

- [ ] **Step 8: Surface authRequired errors**

Add an `Action::AuthRequired(String)` variant to `crates/spur-tui/src/action.rs` and a banner render in `session_detail.rs`. In the orchestrator's error path (where `prompt()` errors are currently swallowed or logged), detect the auth-required string and route it:

```rust
            Err(e) if e.to_string().contains("authRequired") || e.to_string().contains("authentication required") => {
                let _ = self.tui_tx.send(TuiEvent::AuthRequired(
                    "Claude Code requires authentication. Run `claude /login` in a terminal, then restart this session.".to_string()
                )).await;
            }
```

In `session_detail.rs`, add a dismissable red banner at the top of the view when `SessionState::auth_error` is `Some`. Clearing on any keystroke is fine.

- [ ] **Step 9: Build + test**

Run: `cargo build --workspace && cargo test -p spur-tui -p spur-acp -p spur-core`
Expected: all green.

- [ ] **Step 10: Manual test**

Run: `cargo run -p spur-cli -- tui` with `claude-code-acp` selected. Press `Alt-m`. Expected:
1. Mode indicator in status bar flips between `[plan]` and `[default]` on each press.
2. The subprocess log file at `.spur/logs/claude-code-acp-*.log` shows the mode change round-trip.

To test the auth banner: temporarily rename `~/.claude.json` → `~/.claude.json.bak` then try to start a session. Expected: red banner with the `/login` instruction. Restore `~/.claude.json` after.

- [ ] **Step 11: Commit**

```bash
git add crates/spur-tui/src/action.rs \
        crates/spur-tui/src/views/session_detail.rs \
        crates/spur-tui/src/app.rs \
        crates/spur-tui/src/components/status_bar.rs \
        crates/spur-tui/tests/mode_toggle_action.rs \
        crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-tui): plan-mode toggle and auth-required banner

Alt-m cycles the Claude session between default and plan modes via
set_session_mode. authRequired errors from the ACP subprocess are
surfaced as a dismissable banner instructing the user to run
\`claude /login\` externally; in-TUI auth flow deferred to a
follow-up spec."
```

---

## Task 7: Clarify StreamJsonAdapter's scope + deprecate Claude usage

**Files:**
- Modify: `crates/spur-acp/src/connection/stream_json_adapter.rs`
- Modify: `.spur/config.toml`

Goal: comment-only edit to the adapter; deprecation note on the old Claude profile.

- [ ] **Step 1: Update the adapter file header**

In `crates/spur-acp/src/connection/stream_json_adapter.rs`, replace lines 1-7 (the file-level doc comment) with:

```rust
//! `StreamJsonAdapter` — one-shot invocations of CLI tools that emit Claude-style
//! `stream-json` on stdout. One `claude -p --output-format stream-json …` process
//! per prompt; `--resume <sid>` links turns.
//!
//! **Scope:** non-Claude-Code agents whose CLI speaks this format. For Claude
//! Code itself, prefer the `claude-code-acp` profile in `.spur/config.toml`,
//! which routes through `NativeAcpConnection` and the upstream ACP wrapper —
//! richer features (plan mode, usage, commands, fork/resume) and a stable
//! protocol frame (ndjson), in contrast to this adapter's limited
//! 3-event / 4-content-block mapping in `protocol/claude_events.rs`.
//!
//! This adapter uses one-shot mode specifically because `--input-format
//! stream-json` exposes a Node stdout-buffering bug when piped; one-shot
//! flushes per line.
```

- [ ] **Step 2: Add a deprecation comment to the old Claude profile**

In `.spur/config.toml`, above the existing `[[agents.entries]]` block for `name = "claude-code"` (with `transport = "stream-json"`), insert:

```toml
# DEPRECATED: prefer the `claude-code-acp` profile below. This stream-json
# transport is feature-capped (no plan mode, no slash-command discovery,
# no UsageUpdate) and will be removed in a future release. Kept for
# environments without Node.js, which should be rare since Claude Code
# itself requires Node.
```

- [ ] **Step 3: Commit**

```bash
git add crates/spur-acp/src/connection/stream_json_adapter.rs .spur/config.toml
git commit -m "docs: scope StreamJsonAdapter to non-Claude agents

Comment-only changes: clarify that the canonical Claude Code transport
is now claude-code-acp via NativeAcpConnection. Mark the stream-json
Claude profile as deprecated; keep it available for environments
without Node.js."
```

---

## Task 8: Operator documentation

**Files:**
- Create: `docs/spur/claude-code-acp-setup.md`

- [ ] **Step 1: Write the doc**

Create `docs/spur/claude-code-acp-setup.md`:

```markdown
# Using Claude Code with Spur (via `claude-code-acp`)

This is the preferred transport for Claude Code. It runs the upstream
[`@agentclientprotocol/claude-agent-acp`](https://github.com/agentclientprotocol/claude-agent-acp)
binary as a subprocess that speaks ACP, and plugs into Spur's
`NativeAcpConnection`.

## Requirements

- Node.js 20+ on `PATH` (Claude Code itself needs this anyway).
- An authenticated Claude Code install: run `claude /login` once.

## Enabling

Add this profile to `.spur/config.toml`:

\`\`\`toml
[[agents.entries]]
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@<PINNED>"]
transport = "acp"
role = "both"
\`\`\`

Set as the default brain:

\`\`\`toml
[brain]
default = "claude-code-acp"
\`\`\`

## Version pinning

**Do not use `@latest`.** Pin a specific version. To discover current versions:

\`\`\`bash
npm view @agentclientprotocol/claude-agent-acp versions --json | tail -20
\`\`\`

Bump the pin in `.spur/config.toml` when you want to adopt upstream changes.
Run a smoke test after each bump (prompt → permission → plan-mode toggle).

## Logs

Each ACP subprocess writes its stderr to:

\`\`\`
.spur/logs/claude-code-acp-<timestamp>-acp.log
\`\`\`

Tail this when debugging. The Rust side's tracing output is separate and
goes to Spur's configured log sink (see `spur-tui` log-file-sink setup).

## Features enabled by this transport

- Plan-mode toggle (`Alt-m`).
- Live context-% and cost in the status bar.
- Slash-command list (displayed; execution deferred to a follow-up).
- Permission prompts gated through Spur's TUI.

## What's deferred

- In-TUI auth flow — run `claude /login` externally for now. A red banner
  in the TUI tells you when auth is required.
- Model picker.
- `fork_session` / `resume_session` UI.
- Slash-command execution wiring.
\`\`\`
```

- [ ] **Step 2: Commit**

```bash
git add docs/spur/claude-code-acp-setup.md
git commit -m "docs: add operator guide for claude-code-acp transport"
```

---

## Verification / End-to-end

After all tasks land, run the full smoke once more:

- [ ] **Step 1: Clean build**

Run: `cargo build --workspace`
Expected: clean.

- [ ] **Step 2: Full test suite**

Run: `cargo test --workspace`
Expected: all green.

- [ ] **Step 3: Spike rerun (optional, confidence check)**

Run: `cargo run -p spur-acp --example compat_spike`
Expected: all `[ok]` lines including `set_session_mode`.

- [ ] **Step 4: Manual TUI session**

Run: `cargo run -p spur-cli -- tui` with `brain.default = "claude-code-acp"`. Confirm:

- Transcript renders.
- Status bar shows mode indicator + ctx% once the first turn completes.
- `Alt-m` cycles plan mode.
- File-touching tools surface permission prompts.
- `.spur/logs/claude-code-acp-*.log` files exist and contain SDK logs.
- Removing Claude credentials triggers the red auth-required banner.

- [ ] **Step 5: Delete the spike example (optional)**

Once you're confident in the integration, the M0 example can be removed to avoid doc drift:

```bash
git rm crates/spur-acp/examples/compat_spike.rs
# also remove the [[example]] block + [dev-dependencies] addition from
# crates/spur-acp/Cargo.toml if no other example exists.
git commit -m "chore(spur-acp): remove M0 compat spike after integration shipped"
```

Or keep it under `examples/` as permanent integration smoke. Team preference.

---

## Spec coverage check

| Spec section | Implementing task |
|---|---|
| M0 protocol-compat spike | Task 0 |
| ACP feature flags (unstable_session_usage) | Task 1 |
| Stderr capture to per-session log file | Task 2 |
| Narrow AgentConnection trait extensions | Task 3 |
| Read-only TUI notifications | Task 4 |
| Agent profile + version pin + smoke + permission verify | Task 5 |
| Plan-mode toggle + auth-error surfacing | Task 6 |
| StreamJsonAdapter scope clarification + deprecation note | Task 7 |
| Operator docs | Task 8 |
| End-to-end verification | Verification section |

All milestones covered. Follow-up specs listed in the design doc
(`docs/superpowers/specs/2026-04-13-claude-acp-adoption-design.md`,
"Follow-up work") are intentionally out of scope here.
