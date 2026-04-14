# ACP Notification Bus Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the per-turn `SessionNotification` channel + grace window + `dead_tx` swap in `NativeAcpConnection` with a connection-scoped `tokio::sync::broadcast`, so notifications delivered after a `LoadSession`/`Prompt` response (e.g. `claude-code-acp`'s `available_commands_update`) are never dropped.

**Architecture:** Additive change — the `AgentConnection` trait grows one default-`None` method, `subscribe_session_notifications`. `NativeAcpConnection` overrides it; the other three adapters (stdio, cli_wrap, stream_json) keep their existing stream-based delivery unchanged. Inside `native.rs`, `SpurAcpClientDynamic::session_notification` publishes into the broadcast and never touches per-turn channels. The orchestrator subscribes once per connection and emits `SpurEventBody::AgentNotification` for every published item. `prompt()` / `load_session()` still return a `Stream<Item = SessionNotification>` for trait compat, but for `NativeAcpConnection` that stream is empty — the orchestrator's existing `while let Some(..)` drains exit immediately (harmless).

**Tech Stack:** Rust, `tokio::sync::broadcast`, `async_trait`, `agent_client_protocol` SDK, existing `SpurEvent` bus.

**Spec:** `docs/superpowers/specs/2026-04-14-acp-notification-bus-design.md`.

---

## File Structure

- **Create**
  - `crates/spur-acp/tests/session_notification_bus.rs` — integration test fixture that reproduces the dropped-notification race.
- **Modify**
  - `crates/spur-acp/src/connection/mod.rs` — add `subscribe_session_notifications` default trait method; re-export `tokio::sync::broadcast::Receiver<SessionNotification>`.
  - `crates/spur-acp/src/connection/native.rs` — add `session_notif_tx: broadcast::Sender`; rewire `SpurAcpClientDynamic::session_notification`; delete dead_tx / grace-window machinery; return empty stream from `prompt()`/`load_session()`.
  - `crates/spur-core/src/orchestrator.rs` — subscribe to `session_notifications` once per brain/worker connection; emit `AgentNotification` from the pump task.

Existing non-native adapters and the TUI are unchanged — the TUI already consumes `SpurEventBody::AgentNotification` filtered by `session_id` (`crates/spur-tui/src/views/session_detail.rs:872`) and doesn't care whether those events came from a stream drain or a broadcast pump.

---

## Preflight

- [ ] **Step 0: Establish baseline.**

Run:
```bash
cd /Volumes/Projects/spur
cargo test -p spur-acp -p spur-core -p spur-tui 2>&1 | tail -20
```
Expected: existing tests pass. Record the pass count — new tasks must not regress it.

Grep the most recent log for the baseline error rate:
```bash
perl -ne 's/\e\[[0-9;]*m//g; print if /send_result.*err/' .spur/logs/spur.log.$(date +%Y-%m-%d) 2>/dev/null | wc -l
```
Expected: a non-zero count (today's run showed 304). After Task 8 this must be 0 on a fresh run.

---

### Task 1: Failing regression test

**Files:**
- Create: `crates/spur-acp/tests/session_notification_bus.rs`

- [ ] **Step 1: Add test helper deps (if missing).**

Verify `crates/spur-acp/Cargo.toml` already has `tokio` with `rt-multi-thread`, `macros`, `sync`, `time` features and `anyhow` in `[dev-dependencies]`. If any are missing, add them. No changes otherwise.

- [ ] **Step 2: Write the failing test.**

```rust
// crates/spur-acp/tests/session_notification_bus.rs
//
// Regression test for the claude-code-acp dropped-notification race.
// A mock ACP agent publishes an `available_commands_update` *after* it has
// already replied to `session/load`. Under the per-turn channel design this
// notification was delivered to `dead_tx` and dropped. Under the broadcast
// design it must reach a subscriber that was registered at connection setup.

use agent_client_protocol::{
    AvailableCommand, AvailableCommandsUpdate, ContentBlock, SessionId,
    SessionNotification, SessionUpdate, TextContent,
};
use spur_acp::NativeAcpConnection;
use spur_acp::connection::AgentConnection;
use std::time::Duration;

/// Spawns a minimal mock ACP agent as a child process that:
///   1. handshakes `initialize`,
///   2. answers `session/load` with an empty response,
///   3. 50ms *after* the load response, pushes
///      `session/update{available_commands_update}` with one command named
///      `test-cmd`,
///   4. exits on EOF.
///
/// Lives under `tests/fixtures/mock_acp_delayed_cmds.rs`; compiled as a
/// bin-target in the test harness. If the fixture does not exist yet,
/// create it as part of this step.
fn spawn_mock_agent() -> (std::process::Child, String) {
    // Fixture binary path resolved at runtime via CARGO_BIN_EXE_mock_acp_delayed_cmds.
    let bin = env!("CARGO_BIN_EXE_mock_acp_delayed_cmds");
    let child = std::process::Command::new(bin)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn mock agent");
    (child, bin.to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_available_commands_update_reaches_subscriber() {
    // 1. Start connection against the mock agent binary.
    let mut conn = NativeAcpConnection::builder()
        .agent_name("mock-delayed")
        .command(env!("CARGO_BIN_EXE_mock_acp_delayed_cmds"))
        .build()
        .expect("build connection");

    conn.initialize(agent_client_protocol::InitializeRequest::new(
        agent_client_protocol::ProtocolVersion::LATEST,
    ))
    .await
    .expect("initialize");

    // 2. Subscribe BEFORE issuing load_session — broadcast must be live
    //    for the lifetime of the connection.
    let mut rx = conn
        .subscribe_session_notifications()
        .expect("native connection exposes a subscriber");

    // 3. Issue load_session. The mock answers immediately, then fires the
    //    available_commands_update 50ms later.
    let load_req = agent_client_protocol::LoadSessionRequest::new(
        Vec::new(),
        std::path::PathBuf::from("."),
        SessionId::new("mock-session".into()),
    );
    let _stream = conn.load_session(load_req).await.expect("load_session");

    // 4. Wait for the delayed notification. Fail fast on timeout.
    let notif = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("notification arrived within 2s")
        .expect("broadcast not closed");

    match notif.update {
        SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate {
            available_commands,
            ..
        }) => {
            assert_eq!(available_commands.len(), 1);
            assert_eq!(available_commands[0].name, "test-cmd");
        }
        other => panic!("expected AvailableCommandsUpdate, got {other:?}"),
    }
}
```

Then create the mock-agent bin fixture at `crates/spur-acp/tests/fixtures/mock_acp_delayed_cmds/main.rs` and register it in `crates/spur-acp/Cargo.toml`:

```toml
# crates/spur-acp/Cargo.toml — add at the end
[[test]]
name = "session_notification_bus"
path = "tests/session_notification_bus.rs"

[[bin]]
name = "mock_acp_delayed_cmds"
path = "tests/fixtures/mock_acp_delayed_cmds/main.rs"
required-features = []
```

Fixture implementation (read-write JSON-RPC line framing, 50ms delayed push):

```rust
// crates/spur-acp/tests/fixtures/mock_acp_delayed_cmds/main.rs
use std::io::{BufRead, BufReader, Write};
use serde_json::{json, Value};

fn send(line: &Value) {
    let out = serde_json::to_string(line).unwrap();
    println!("{out}");
    std::io::stdout().flush().ok();
}

fn main() {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut buf = String::new();
    loop {
        buf.clear();
        if reader.read_line(&mut buf).unwrap_or(0) == 0 {
            return;
        }
        let msg: Value = match serde_json::from_str(&buf) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        match method {
            "initialize" => send(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "protocolVersion": 1 }
            })),
            "session/load" => {
                // 1. Reply success.
                send(&json!({ "jsonrpc": "2.0", "id": id, "result": {} }));
                // 2. After 50ms, push the available_commands_update.
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    send(&json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": "mock-session",
                            "update": {
                                "sessionUpdate": "available_commands_update",
                                "availableCommands": [
                                    { "name": "test-cmd", "description": "t" }
                                ]
                            }
                        }
                    }));
                });
            }
            _ => {}
        }
    }
}
```

- [ ] **Step 3: Run test to verify it fails.**

```bash
cargo test -p spur-acp --test session_notification_bus 2>&1 | tail -30
```
Expected: **compile error** on `conn.subscribe_session_notifications()` (method does not exist yet). This compile failure IS the red-state for this test; we intentionally drive the trait change from it.

- [ ] **Step 4: Commit.**

```bash
git add crates/spur-acp/Cargo.toml crates/spur-acp/tests/session_notification_bus.rs \
        crates/spur-acp/tests/fixtures/mock_acp_delayed_cmds/main.rs
git commit -m "test(spur-acp): failing regression for delayed session_notification"
```

---

### Task 2: Add `subscribe_session_notifications` trait method (default `None`)

**Files:**
- Modify: `crates/spur-acp/src/connection/mod.rs`

- [ ] **Step 1: Add re-export and default method.**

Insert near the top of `mod.rs`, after the existing re-exports:

```rust
pub use tokio::sync::broadcast;
```

Insert inside `trait AgentConnection` (after `take_ext_notification_rx`, before the closing `}`):

```rust
    /// Subscribe to the connection-scoped broadcast of `SessionNotification`s.
    ///
    /// Implementations that publish notifications through a long-lived
    /// broadcast channel (only `NativeAcpConnection` at time of writing)
    /// return `Some(receiver)` here. The orchestrator spawns a pump task
    /// that converts every published notification into a
    /// `SpurEventBody::AgentNotification` tagged with the brain/worker's
    /// `spur_session_id`.
    ///
    /// Transports that stay on the per-call `Stream` API return `None`
    /// (default) — the orchestrator falls back to draining the stream
    /// handed back by `prompt()` / `load_session()`.
    fn subscribe_session_notifications(
        &self,
    ) -> Option<broadcast::Receiver<SessionNotification>> {
        None
    }
```

- [ ] **Step 2: Ensure workspace compiles.**

```bash
cargo check -p spur-acp 2>&1 | tail -10
```
Expected: clean compile (no warnings, no errors).

- [ ] **Step 3: Commit.**

```bash
git add crates/spur-acp/src/connection/mod.rs
git commit -m "feat(spur-acp): add AgentConnection::subscribe_session_notifications default"
```

---

### Task 3: Thread a broadcast sender through `NativeAcpConnection`

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs`

- [ ] **Step 1: Add the broadcast sender field.**

In the `NativeAcpConnection` struct (near the `ext_notification_tx` field at ~line 135), add:

```rust
    /// Connection-scoped broadcast of session notifications. Cloned into
    /// `SpurAcpClientDynamic`; subscribers obtained via
    /// `subscribe_session_notifications` live for the connection's whole
    /// lifetime — no per-turn channel swap, no grace window, no dead_tx.
    session_notif_tx: tokio::sync::broadcast::Sender<agent_client_protocol::SessionNotification>,
```

- [ ] **Step 2: Initialize it in the constructor.**

Find the `NativeAcpConnection::new` / builder implementation (search for `ext_notification_tx: ext_tx,` around line 179). Alongside it, add:

```rust
        let (session_notif_tx, _) = tokio::sync::broadcast::channel(1024);
```

and store it in the struct literal:

```rust
            session_notif_tx,
```

- [ ] **Step 3: Pass the sender to the ACP thread.**

Locate `acp_thread_main(...)` at ~line 252. Extend its signature with one more parameter:

```rust
fn acp_thread_main(
    agent_name: String,
    command: String,
    extra_args: Vec<String>,
    cmd_rx: mpsc::UnboundedReceiver<AcpCommand>,
    permission_tx: Option<mpsc::UnboundedSender<PermissionRequest>>,
    ext_notification_tx: mpsc::UnboundedSender<ExtNotificationPayload>,
    session_notif_tx: tokio::sync::broadcast::Sender<agent_client_protocol::SessionNotification>,
    child_pgid: Arc<Mutex<Option<i32>>>,
) {
```

At the spawn site (native.rs:249-261), clone `self.session_notif_tx.clone()` and pass it:

```rust
        let session_notif_tx_for_thread = self.session_notif_tx.clone();
        // ...
        .spawn(move || {
            acp_thread_main(
                thread_agent_name,
                command,
                extra_args,
                cmd_rx,
                permission_tx,
                ext_tx,
                session_notif_tx_for_thread,
                child_pgid,
            );
        })
```

- [ ] **Step 4: Check it compiles.**

```bash
cargo check -p spur-acp 2>&1 | tail -10
```
Expected: clean compile. (The sender is plumbed but not yet used — that's Task 4.)

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "refactor(spur-acp): plumb broadcast sender into NativeAcpConnection thread"
```

---

### Task 4: Rewire `SpurAcpClientDynamic::session_notification` onto the broadcast

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs`

- [ ] **Step 1: Swap the struct field on `SpurAcpClientDynamic`.**

Locate the struct (search `struct SpurAcpClientDynamic`). Replace:

```rust
    notification_tx: std::rc::Rc<std::cell::RefCell<mpsc::UnboundedSender<SessionNotification>>>,
```

with:

```rust
    session_notif_tx: tokio::sync::broadcast::Sender<SessionNotification>,
```

Also remove the `last_notification_at: std::rc::Rc<std::cell::RefCell<std::time::Instant>>,` field — it's no longer needed.

- [ ] **Step 2: Update the constructor in `acp_thread_main`.**

Replace the block at native.rs:788-814 (the `_initial_rx` creation plus `SpurAcpClientDynamic { ... }` literal) with:

```rust
        // Build the SpurAcpClient that publishes session notifications into
        // the connection-scoped broadcast. `session_notif_tx` is owned by
        // the outer `NativeAcpConnection` and lives for the connection's
        // whole lifetime — callbacks can never land on a dead channel.
        let spur_client = SpurAcpClientDynamic {
            session_notif_tx: session_notif_tx.clone(),
            cwd: std::rc::Rc::new(std::cell::RefCell::new(PathBuf::from("."))),
            permission_tx,
            terminals: std::rc::Rc::new(std::cell::RefCell::new(HashMap::new())),
            ext_notification_tx: ext_notification_tx.clone(),
        };
```

- [ ] **Step 3: Rewrite the `session_notification` impl.**

Replace the body of `async fn session_notification` (native.rs:1184-1214):

```rust
    async fn session_notification(
        &self,
        args: SessionNotification,
    ) -> agent_client_protocol::Result<()> {
        let variant = session_update_variant_name(&args.update);
        let text_len = match &args.update {
            agent_client_protocol::SessionUpdate::AgentMessageChunk(c)
            | agent_client_protocol::SessionUpdate::AgentThoughtChunk(c)
            | agent_client_protocol::SessionUpdate::UserMessageChunk(c) => {
                content_chunk_text_len(c)
            }
            _ => 0,
        };
        let session = args.session_id.to_string();
        // `broadcast::Sender::send` returns `Err(SendError)` only when every
        // receiver has been dropped. In our topology the orchestrator's pump
        // task subscribes at connection setup and stays alive for the whole
        // connection — so `Err` here indicates the connection is tearing
        // down and we can safely ignore it.
        let send_result = self.session_notif_tx.send(args);
        let send_result_str = if send_result.is_ok() { "ok" } else { "err" };
        tracing::debug!(
            streaming_probe = true,
            site = "A_session_notification",
            variant = variant,
            text_len = text_len,
            session = %session,
            send_result = send_result_str,
            "ACP session_notification (broadcast)"
        );
        Ok(())
    }
```

- [ ] **Step 4: Check compile.**

```bash
cargo check -p spur-acp 2>&1 | tail -20
```
Expected: compile errors point at the remaining `notification_tx.borrow_mut()` / `last_notification_at.borrow_mut()` references in the `Prompt` / `LoadSession` / `CancelAll` arms. Those are removed in Task 5 — this intermediate step is expected to still fail compile. If you want a green intermediate, stash this commit and fold into Task 5.

- [ ] **Step 5: Commit (may fail CI — this is intentional; the next task completes the migration).**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "refactor(spur-acp): session_notification publishes to broadcast (WIP)"
```

---

### Task 5: Strip the dead_tx / grace / per-turn mpsc machinery; return empty streams

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs`

- [ ] **Step 1: Rewrite the `AcpCommand::Prompt` arm.**

Replace the block at native.rs:871-960 (the entire `AcpCommand::Prompt { request, reply } => { ... }` arm, including the grace loop and `dead_tx` swap) with:

```rust
                AcpCommand::Prompt { request, reply } => {
                    // Notifications flow out-of-band via the
                    // `session_notif_tx` broadcast. The `Stream` returned to
                    // the caller is empty but remains a live
                    // `UnboundedReceiver` so the trait contract still
                    // compiles; it closes when we drop `tx_empty` below.
                    let (tx_empty, rx_empty) =
                        mpsc::unbounded_channel::<SessionNotification>();
                    let _ = reply.send(Ok(rx_empty));

                    let agent_name_prompt = agent_name.clone();
                    let session_id_for_probe = request.session_id.clone();
                    let prompt_result = connection.prompt(request).await;
                    match &prompt_result {
                        Ok(_) => tracing::debug!(
                            agent = %agent_name_prompt,
                            session = %session_id_for_probe,
                            "NativeAcpConnection: prompt completed"
                        ),
                        Err(e) => tracing::warn!(
                            agent = %agent_name_prompt,
                            session = %session_id_for_probe,
                            "NativeAcpConnection: prompt failed: {e}"
                        ),
                    }
                    // Drop `tx_empty` so the caller's stream terminates,
                    // signalling turn completion.
                    drop(tx_empty);
                }
```

- [ ] **Step 2: Rewrite the `AcpCommand::LoadSession` arm.**

Replace native.rs:1006-1074 with:

```rust
                AcpCommand::LoadSession { request, reply } => {
                    *cwd_ref.borrow_mut() = request.cwd.clone();

                    let (tx_empty, rx_empty) =
                        mpsc::unbounded_channel::<SessionNotification>();
                    let agent_name_load = agent_name.clone();
                    let session_id_for_probe = request.session_id.clone();
                    let load_result = connection.load_session(request).await;
                    match load_result {
                        Ok(_) => {
                            tracing::debug!(
                                agent = %agent_name_load,
                                session = %session_id_for_probe,
                                "NativeAcpConnection: load_session completed"
                            );
                            let _ = reply.send(Ok(rx_empty));
                        }
                        Err(e) => {
                            tracing::warn!(
                                agent = %agent_name_load,
                                session = %session_id_for_probe,
                                "NativeAcpConnection: load_session failed: {e}"
                            );
                            let _ = reply.send(Err(anyhow::anyhow!(
                                "NativeAcpConnection '{}': load_session failed: {e}",
                                agent_name_load
                            )));
                        }
                    }
                    drop(tx_empty);
                }
```

- [ ] **Step 3: Delete the `notification_tx` RefCell and `last_notification_at` plumbing at the top of `acp_thread_main`.**

Remove the lines at native.rs:786-803 (everything from the `// Create the notification channel for bridging session updates.` comment through the last `last_notification_at_for_thread = last_notification_at.clone();` line). The `acp_thread_main` no longer owns these — the broadcast sender handed in as a parameter is the only plumbing.

- [ ] **Step 4: Override `subscribe_session_notifications` on `NativeAcpConnection`.**

In the `impl AgentConnection for NativeAcpConnection` block (search for `async fn initialize` around line 231 or the `#[async_trait]` impl), add:

```rust
    fn subscribe_session_notifications(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<SessionNotification>> {
        Some(self.session_notif_tx.subscribe())
    }
```

- [ ] **Step 5: Check compile.**

```bash
cargo check -p spur-acp 2>&1 | tail -20
```
Expected: clean compile.

- [ ] **Step 6: Run the regression test.**

```bash
cargo test -p spur-acp --test session_notification_bus 2>&1 | tail -15
```
Expected: `test delayed_available_commands_update_reaches_subscriber ... ok`.

- [ ] **Step 7: Run the full `spur-acp` suite.**

```bash
cargo test -p spur-acp 2>&1 | tail -20
```
Expected: all existing tests pass (count ≥ Task 0 baseline).

- [ ] **Step 8: Commit.**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "fix(spur-acp): kill per-turn channel swap; notifications via broadcast"
```

---

### Task 6: Orchestrator subscribes once per brain connection

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Spawn the pump task at brain-session creation.**

Locate `create_brain_session` (search `fn create_brain_session` — around line 1080). Immediately after the connection is constructed and `initialize`d but BEFORE `new_session`/`load_session` is called, add:

```rust
        // Fan out session notifications from the connection's broadcast to
        // the SpurEvent bus, tagged by the brain's spur_session_id. If the
        // transport does not publish via broadcast (stdio / cli_wrap /
        // stream_json), this short-circuits and the per-call `Stream` path
        // below continues to handle notifications.
        if let Some(mut notif_rx) = connection.subscribe_session_notifications() {
            let funnel = self.funnel.clone();
            let spur_id_for_pump = session_id.clone();
            tokio::spawn(async move {
                loop {
                    match notif_rx.recv().await {
                        Ok(notif) => {
                            funnel.emit(SpurEventBody::AgentNotification {
                                session: spur_id_for_pump.clone(),
                                notification: Box::new(notif),
                            });
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                skipped = n,
                                session = %spur_id_for_pump,
                                "session notification pump lagged"
                            );
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            break;
                        }
                    }
                }
            });
        }
```

Repeat the same block inside `load_brain_session` (search `fn load_brain_session` — around line 1164) before the `load_session_with_bypass` call.

- [ ] **Step 2: Leave the existing stream-drain loops alone.**

The `while let Some(notification) = stream.next().await` loops at orchestrator.rs:345-363 (prompt path) and orchestrator.rs:515-526 (load history path) stay. For `NativeAcpConnection` the stream is now empty, so the loops exit immediately — harmless. For the three other adapters (stdio, cli_wrap, stream_json) those loops are still the sole notification path.

- [ ] **Step 3: Check compile.**

```bash
cargo check -p spur-core 2>&1 | tail -10
```
Expected: clean compile.

- [ ] **Step 4: Run the orchestrator test suite.**

```bash
cargo test -p spur-core 2>&1 | tail -20
```
Expected: all tests pass.

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): fan out broadcast session_notifications into SpurEvent bus"
```

---

### Task 7: End-to-end verification in a live process

**Files:** none (verification-only).

- [ ] **Step 1: Build the binary.**

```bash
cargo build --release -p spur-cli 2>&1 | tail -5
```
Expected: build succeeds.

- [ ] **Step 2: Run spur against `claude-code-acp`.**

In a separate terminal:
```bash
./target/release/spur run
```
Select `claude-code-acp` as the brain. Once the session is live, type `/` in the input to open the command popup.

Expected: the popup lists all ~144 commands advertised by `claude-code-acp` (`update-config`, `debug`, `simplify`, `claude-api`, `superpowers:brainstorm`, …). Before this change it was empty.

- [ ] **Step 3: Confirm no dropped notifications in today's log.**

```bash
perl -ne 's/\e\[[0-9;]*m//g; print if /send_result.*err/' .spur/logs/spur.log.$(date +%Y-%m-%d) | wc -l
```
Expected: `0`. (Baseline pre-fix was 304.)

- [ ] **Step 4: Spot-check other agents are unaffected.**

Run spur against `kiro` and `codex` each once. Verify command popup still works (kiro via its vendor-ext channel; codex via its static `/compact` + any ACP-advertised commands).

- [ ] **Step 5: Commit verification notes (optional).**

If verification uncovered anything worth recording, add a short note to `docs/superpowers/specs/2026-04-14-acp-notification-bus-design.md` under a new `## Verification Results` section and commit. Otherwise skip.

---

## Self-Review Checklist

- **Spec coverage:** ✅ Broadcast channel (Task 3), `session_notification` publisher (Task 4), subscribe trait method (Task 2/5), orchestrator pump (Task 6), dead_tx/grace removal (Task 5), regression test (Task 1), verification (Task 7).
- **Placeholder scan:** no "TBD"/"as appropriate"/"similar to"; every code block is complete.
- **Type consistency:** `session_notif_tx: broadcast::Sender<SessionNotification>` used identically in `NativeAcpConnection`, `SpurAcpClientDynamic`, `acp_thread_main` signature, and the `subscribe_session_notifications` override.
- **Out of scope check:** stdio/cli_wrap/stream_json adapters — untouched (trait default `None` preserves their behaviour). TUI — untouched (already consumes `AgentNotification`). `SpurEventBody` — no new variants.
