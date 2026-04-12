# ACP Interactive Permission Flow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace auto-approve in `SpurAcpClientDynamic::request_permission()` with an interactive flow that sends permission requests to the TUI and awaits user response via a oneshot channel.

**Architecture:** A dedicated `mpsc` channel carries `PermissionRequest` (containing the raw SDK `RequestPermissionRequest` + a `oneshot::Sender` for the response) from the ACP thread to the TUI. The channel is created in `main.rs`, `perm_tx` flows as a parameter through `run_interactive()` → `spawn_brain_session()` → `NativeAcpConnection` → `acp_thread_main()` → `SpurAcpClientDynamic`. The TUI polls `perm_rx`, displays the request, and sends back the user's choice.

**Tech Stack:** Rust, tokio channels (mpsc + oneshot), agent-client-protocol SDK 0.10.4, ratatui

---

### Task 1: Add permission types to spur-acp

**Files:**
- Modify: `crates/spur-acp/src/types.rs`
- Modify: `crates/spur-acp/src/lib.rs`

- [ ] **Step 1: Add PermissionRequest and PermissionResponse types**

Add at the end of `crates/spur-acp/src/types.rs`:

```rust
// ─── Permission Flow ──────────────────────────────────────────────────

/// A permission request sent from the ACP thread to the TUI.
/// Carries the raw SDK request for full protocol access, plus a oneshot
/// channel for the TUI to send back the user's choice.
pub struct PermissionRequest {
    pub args: agent_client_protocol::RequestPermissionRequest,
    pub reply_tx: tokio::sync::oneshot::Sender<PermissionResponse>,
}

/// The user's permission decision — the selected option ID.
#[derive(Debug, Clone)]
pub struct PermissionResponse {
    pub option_id: String,
}
```

- [ ] **Step 2: Add SDK re-exports for permission types**

In `crates/spur-acp/src/lib.rs`, add to the existing SDK re-export block:

```rust
pub use agent_client_protocol::{
    RequestPermissionRequest, PermissionOption, PermissionOptionId,
    PermissionOptionKind, RequestPermissionOutcome, SelectedPermissionOutcome,
};
```

- [ ] **Step 3: Verify build**

Run: `cargo check --package spur-acp`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/src/types.rs crates/spur-acp/src/lib.rs
git commit -m "feat(spur-acp): add PermissionRequest/Response types and SDK re-exports"
```

---

### Task 2: Add permission_tx to NativeAcpConnection and SpurAcpClientDynamic

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs`

- [ ] **Step 1: Add permission_tx field to NativeAcpConnection**

In `crates/spur-acp/src/connection/native.rs`, add to the struct (around line 82):

```rust
pub struct NativeAcpConnection {
    agent_name: String,
    command: String,
    extra_args: Vec<String>,
    cmd_tx: Option<mpsc::UnboundedSender<AcpCommand>>,
    thread_handle: Option<std::thread::JoinHandle<()>>,
    health_status: AgentHealth,
    permission_tx: Option<mpsc::UnboundedSender<crate::types::PermissionRequest>>,  // NEW
}
```

- [ ] **Step 2: Update NativeAcpConnection::new() to accept permission_tx**

```rust
pub fn new(
    agent_name: impl Into<String>,
    command: impl Into<String>,
    extra_args: Vec<String>,
    permission_tx: Option<mpsc::UnboundedSender<crate::types::PermissionRequest>>,
) -> Self {
    Self {
        agent_name: agent_name.into(),
        command: command.into(),
        extra_args,
        cmd_tx: None,
        thread_handle: None,
        health_status: AgentHealth::Unknown,
        permission_tx,
    }
}
```

- [ ] **Step 3: Thread permission_tx into acp_thread_main()**

In the `initialize()` method (around line 141), where `acp_thread_main` is spawned, pass `permission_tx`:

```rust
let permission_tx = self.permission_tx.clone();
let handle = std::thread::Builder::new()
    .name(format!("acp-{}", agent_name))
    .spawn(move || {
        acp_thread_main(thread_agent_name, command, extra_args, cmd_rx, permission_tx);
    })
```

Update `acp_thread_main` signature:

```rust
fn acp_thread_main(
    agent_name: String,
    command: String,
    extra_args: Vec<String>,
    mut cmd_rx: mpsc::UnboundedReceiver<AcpCommand>,
    permission_tx: Option<mpsc::UnboundedSender<crate::types::PermissionRequest>>,
) {
```

- [ ] **Step 4: Pass permission_tx to SpurAcpClientDynamic**

Inside `acp_thread_main`, where `SpurAcpClientDynamic` is constructed (around line 443):

```rust
let spur_client = SpurAcpClientDynamic {
    notification_tx: notification_tx_for_client,
    cwd: std::rc::Rc::new(std::cell::RefCell::new(PathBuf::from("."))),
    permission_tx,  // NEW — mpsc::UnboundedSender is Send, safe in !Send struct
};
```

Add the field to the struct (around line 567):

```rust
struct SpurAcpClientDynamic {
    notification_tx: std::rc::Rc<std::cell::RefCell<mpsc::UnboundedSender<SessionNotification>>>,
    cwd: std::rc::Rc<std::cell::RefCell<PathBuf>>,
    permission_tx: Option<mpsc::UnboundedSender<crate::types::PermissionRequest>>,  // NEW
}
```

- [ ] **Step 5: Verify build** (will fail in spur-core due to changed constructor — expected)

Run: `cargo check --package spur-acp`
Expected: PASS for spur-acp itself

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "feat(spur-acp): add permission_tx to NativeAcpConnection and SpurAcpClientDynamic"
```

---

### Task 3: Implement interactive request_permission()

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs`

- [ ] **Step 1: Replace auto-approve with channel send + await**

Replace the `request_permission` implementation in `impl Client for SpurAcpClientDynamic` (around line 574):

```rust
async fn request_permission(
    &self,
    args: RequestPermissionRequest,
) -> agent_client_protocol::Result<RequestPermissionResponse> {
    let Some(ref perm_tx) = self.permission_tx else {
        // No TUI connected — auto-approve (non-interactive mode)
        return auto_approve(&args);
    };

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let request = crate::types::PermissionRequest {
        args: args.clone(),
        reply_tx,
    };

    if perm_tx.send(request).is_err() {
        // TUI disconnected — fall back to auto-approve
        tracing::warn!("NativeAcpConnection: permission channel closed, auto-approving");
        return auto_approve(&args);
    }

    tracing::debug!(
        session = %args.session_id,
        "NativeAcpConnection: awaiting interactive permission response"
    );

    match tokio::time::timeout(std::time::Duration::from_secs(60), reply_rx).await {
        Ok(Ok(response)) => {
            let option_id = agent_client_protocol::PermissionOptionId::new(response.option_id);
            tracing::debug!(option = %option_id, "NativeAcpConnection: permission granted");
            Ok(RequestPermissionResponse::new(
                RequestPermissionOutcome::Selected(
                    SelectedPermissionOutcome::new(option_id),
                ),
            ))
        }
        Ok(Err(_)) => {
            // oneshot sender dropped (TUI timeout or dismiss)
            tracing::debug!("NativeAcpConnection: permission denied (channel dropped)");
            auto_deny(&args)
        }
        Err(_) => {
            // 60s safety timeout
            tracing::warn!("NativeAcpConnection: permission timed out (60s safety)");
            auto_deny(&args)
        }
    }
}
```

- [ ] **Step 2: Add auto_approve and auto_deny helper functions**

Add after the `impl Client for SpurAcpClientDynamic` block:

```rust
fn auto_approve(
    args: &RequestPermissionRequest,
) -> agent_client_protocol::Result<RequestPermissionResponse> {
    let option_id = args
        .options
        .first()
        .map(|o| o.option_id.clone())
        .unwrap_or_else(|| agent_client_protocol::PermissionOptionId::new("allow"));
    Ok(RequestPermissionResponse::new(
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
    ))
}

fn auto_deny(
    args: &RequestPermissionRequest,
) -> agent_client_protocol::Result<RequestPermissionResponse> {
    let option_id = args
        .options
        .last()
        .map(|o| o.option_id.clone())
        .unwrap_or_else(|| agent_client_protocol::PermissionOptionId::new("deny"));
    Ok(RequestPermissionResponse::new(
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
    ))
}
```

- [ ] **Step 3: Verify spur-acp build**

Run: `cargo check --package spur-acp`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "feat(spur-acp): implement interactive request_permission with oneshot channel"
```

---

### Task 4: Thread permission_tx through orchestrator

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Add permission_tx parameter to run_interactive()**

Change the signature at line 312:

```rust
pub async fn run_interactive(
    mut self,
    mut user_input_rx: mpsc::Receiver<InteractiveInput>,
    brain_override: Option<String>,
    permission_tx: Option<mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
) -> Result<()> {
```

- [ ] **Step 2: Pass permission_tx to spawn_brain_session()**

Where `spawn_brain_session` is called inside `run_interactive` (around line 335):

```rust
match self
    .spawn_brain_session(brain_override.as_deref(), permission_tx.clone())
    .await
```

Update `spawn_brain_session` signature:

```rust
pub async fn spawn_brain_session(
    &mut self,
    brain_override: Option<&str>,
    permission_tx: Option<mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
) -> Result<BrainSession> {
```

- [ ] **Step 3: Pass permission_tx to create_connection()**

In `spawn_brain_session`, where `create_connection` is called (line 649):

```rust
let mut connection = self.create_connection(&brain_config, permission_tx.clone());
```

Update `create_connection`:

```rust
fn create_connection(
    &self,
    config: &spur_acp::config::AgentConfig,
    permission_tx: Option<mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
) -> Box<dyn AgentConnection> {
    match config.transport {
        TransportKind::Acp => Box::new(NativeAcpConnection::new(
            config.name.clone(),
            config.command.clone(),
            config.args.clone(),
            permission_tx,
        )),
        TransportKind::Stdio => Box::new(StdioAdapter::new(
            config.name.clone(),
            config.command.clone(),
            config.args.clone(),
        )),
        TransportKind::CliWrap => Box::new(CliWrapAdapter::new(
            config.name.clone(),
            config.command.clone(),
            config.args.clone(),
        )),
    }
}
```

- [ ] **Step 4: Fix other call sites of spawn_brain_session and create_connection**

`spawn_brain_session` is also called from `run_adhoc()` — pass `None`:

```rust
self.spawn_brain_session(brain.as_deref(), None).await?;
```

`create_connection` is also called from worker delegation code — pass `None`:

Search for all `create_connection(` calls and add the `None` parameter for non-interactive paths.

- [ ] **Step 5: Verify build**

Run: `cargo check --package spur-core`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): thread permission_tx through run_interactive and spawn_brain_session"
```

---

### Task 5: Add PermissionResponse/PermissionDenied to TUI Action enum

**Files:**
- Modify: `crates/spur-tui/src/action.rs`

- [ ] **Step 1: Add new action variants**

```rust
pub enum Action {
    Quit,
    NavigateTo(ViewId),
    NavigateBack,
    SendMessage {
        session: SessionId,
        text: String,
        interrupt: bool,
    },
    PermissionResponse { option_id: String },  // NEW
    PermissionDenied,                           // NEW
    ToggleVerbose,
    ScrollUp,
    ScrollDown,
    ScrollToTop,
    ScrollToBottom,
    CycleFocus,
    ShowHelp,
    HideHelp,
    Tick,
}
```

- [ ] **Step 2: Commit**

```bash
git add crates/spur-tui/src/action.rs
git commit -m "feat(spur-tui): add PermissionResponse and PermissionDenied action variants"
```

---

### Task 6: Wire TUI App to receive and process permissions

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/src/lib.rs` (if run_tui is re-exported)

- [ ] **Step 1: Add pending_permission field to App**

```rust
use std::time::Instant;
use spur_acp::types::PermissionRequest;

pub struct App {
    // ... existing fields ...
    pending_permission: Option<(PermissionRequest, Instant)>,
}
```

Initialize in `App::new()`:
```rust
pending_permission: None,
```

- [ ] **Step 2: Update run_tui() signature to accept perm_rx**

```rust
pub async fn run_tui(
    event_rx: broadcast::Receiver<SpurEvent>,
    user_input_tx: Option<mpsc::Sender<UserInput>>,
    mut perm_rx: Option<mpsc::UnboundedReceiver<PermissionRequest>>,
) -> anyhow::Result<()> {
```

- [ ] **Step 3: Add perm_rx poll to the event loop**

Inside the `tokio::select!` block, add a new branch. The `perm_rx` is `Option` so we need to handle it conditionally:

```rust
tokio::select! {
    Some(Ok(crossterm_event)) = event_stream.next() => {
        app.handle_crossterm_event(crossterm_event);
    }
    result = event_rx.recv() => {
        // ... existing ...
    }
    _ = tick_interval.tick() => {
        app.tick();
    }
    Some(perm) = async {
        match perm_rx.as_mut() {
            Some(rx) => rx.recv().await,
            None => std::future::pending().await,
        }
    } => {
        app.handle_permission_request(perm);
    }
}
```

- [ ] **Step 4: Implement handle_permission_request()**

Add to `impl App`:

```rust
fn handle_permission_request(&mut self, request: PermissionRequest) {
    // Auto-deny any existing pending permission
    self.pending_permission.take();

    // Extract description from the SDK args
    let description = request.args.tool_call.fields.title
        .clone()
        .unwrap_or_else(|| "Tool call".to_string());

    // Compute countdown from option names for display
    let countdown = 30u8;

    // Push permission trace entry to the active session detail
    if let Some(ref mut detail) = self.session_detail {
        detail.push_permission(&description, countdown);
    }

    // Store with deadline
    let deadline = Instant::now() + std::time::Duration::from_secs(countdown as u64);
    self.pending_permission = Some((request, deadline));
    self.dirty = true;
}
```

- [ ] **Step 5: Add timeout check to tick()**

In `App::tick()`, add before the existing match:

```rust
pub fn tick(&mut self) {
    // Check permission timeout
    if let Some((_, deadline)) = &self.pending_permission {
        if Instant::now() >= *deadline {
            // Drop reply_tx → auto-deny on ACP side
            self.pending_permission.take();
            if let Some(ref mut detail) = self.session_detail {
                detail.clear_pending_permission();
            }
            self.dirty = true;
        }
    }

    // ... existing tick logic ...
}
```

- [ ] **Step 6: Handle PermissionResponse and PermissionDenied in process_action()**

Add to the `match action` in `process_action()`:

```rust
Action::PermissionResponse { option_id } => {
    if let Some((perm, _)) = self.pending_permission.take() {
        let _ = perm.reply_tx.send(spur_acp::types::PermissionResponse { option_id });
    }
    if let Some(ref mut detail) = self.session_detail {
        detail.clear_pending_permission();
    }
}

Action::PermissionDenied => {
    // Drop reply_tx (signals denial to ACP thread)
    self.pending_permission.take();
    if let Some(ref mut detail) = self.session_detail {
        detail.clear_pending_permission();
    }
}
```

- [ ] **Step 7: Verify build** (session_detail methods don't exist yet — expected to fail)

Run: `cargo check --package spur-tui 2>&1 | head -5`
Expected: Errors about `push_permission` and `clear_pending_permission` — fixed in Task 7.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): add permission request handling to App with Instant-based timeout"
```

---

### Task 7: Wire session_detail [y]/[n]/[a] handlers and add permission helpers

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Add push_permission() and clear_pending_permission() methods**

Add to `impl SessionDetailView`:

```rust
pub fn push_permission(&mut self, description: &str, countdown: u8) {
    self.react_trace.push(TraceEntry {
        kind: TraceKind::Permission {
            description: description.to_string(),
            pending: true,
            countdown,
        },
        text: String::new(),
        timestamp: Self::now_stamp(),
    });
}

pub fn clear_pending_permission(&mut self) {
    // Walk entries in reverse to find the most recent pending permission
    // and mark it as not pending. ReactTrace doesn't expose entries mutably,
    // so we push a new "resolved" entry instead.
    // (The existing TraceKind::Permission rendering handles pending: false correctly.)
}
```

Note: `ReactTrace` doesn't expose `entries` mutably. The simplest approach is to track pending state in the view or push a new entry. Check `react_trace.rs` for any mutable entry access patterns. If none exist, push a follow-up Think entry: `"Permission resolved"`.

- [ ] **Step 2: Wire [y]/[n]/[a] key handlers**

Replace the placeholder permission key handlers (around line 96) in `handle_key()`:

```rust
if self.react_trace.has_pending_permission() {
    match key.code {
        KeyCode::Char('y') => {
            return Some(Action::PermissionResponse {
                option_id: "allow".to_string(),  // Will be refined by App using actual options
            });
        }
        KeyCode::Char('n') => {
            return Some(Action::PermissionDenied);
        }
        KeyCode::Char('a') => {
            return Some(Action::PermissionResponse {
                option_id: "always_allow".to_string(),
            });
        }
        _ => {
            // Fall through to normal key handling
        }
    }
}
```

Wait — the session_detail doesn't know the actual option_ids. The App does (it holds the `PermissionRequest`). The session_detail should return a generic action, and the App maps it to the right option_id.

Better approach: return semantic actions and let App resolve to option_ids:

```rust
KeyCode::Char('y') => {
    // App will map this to the first option
    return Some(Action::PermissionResponse {
        option_id: String::new(),  // sentinel: App fills in first option
    });
}
KeyCode::Char('n') => {
    return Some(Action::PermissionDenied);
}
KeyCode::Char('a') => {
    // App will map this to always-allow option
    return Some(Action::PermissionResponse {
        option_id: "always".to_string(),  // sentinel: App finds always option
    });
}
```

Actually, cleaner: use distinct Action variants or a hint field. But YAGNI — let the App check: if `option_id` is empty, use first option. If `option_id` is "always", find the always option. Otherwise use it literally.

Update process_action in App to resolve:

```rust
Action::PermissionResponse { option_id } => {
    if let Some((perm, _)) = self.pending_permission.take() {
        let resolved_id = if option_id.is_empty() {
            // [y] — first option
            perm.args.options.first()
                .map(|o| o.option_id.to_string())
                .unwrap_or_else(|| "allow".to_string())
        } else if option_id == "always" {
            // [a] — find option with "always" in name
            perm.args.options.iter()
                .find(|o| o.name.to_lowercase().contains("always"))
                .or(perm.args.options.first())
                .map(|o| o.option_id.to_string())
                .unwrap_or_else(|| "allow".to_string())
        } else {
            option_id
        };
        let _ = perm.reply_tx.send(spur_acp::types::PermissionResponse {
            option_id: resolved_id,
        });
    }
    if let Some(ref mut detail) = self.session_detail {
        detail.clear_pending_permission();
    }
}
```

- [ ] **Step 3: Verify build**

Run: `cargo check --package spur-tui`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): wire [y]/[n]/[a] permission handlers with option resolution"
```

---

### Task 8: Wire permission channel in main.rs Watch command

**Files:**
- Modify: `crates/spur-cli/src/main.rs`

- [ ] **Step 1: Create permission channel and pass to orchestrator + TUI**

Update the `Commands::Watch` arm (around line 355):

```rust
Commands::Watch { brain } => {
    let orch = load_orchestrator(repo_root)?;
    let event_rx = orch.subscribe();

    // Create permission channel
    let (perm_tx, perm_rx) =
        tokio::sync::mpsc::unbounded_channel::<spur_acp::types::PermissionRequest>();

    // Create user input channel
    let (user_tx, user_rx) =
        tokio::sync::mpsc::channel::<spur_core::InteractiveInput>(32);

    // Spawn interactive orchestrator (moves ownership)
    let orch_handle = tokio::spawn(async move {
        if let Err(e) = orch
            .run_interactive(user_rx, brain, Some(perm_tx))
            .await
        {
            tracing::error!(error = %e, "Interactive session error");
        }
    });

    // Create wrapper sender for TUI input
    let (tui_tx, mut tui_rx) =
        tokio::sync::mpsc::channel::<spur_tui::UserInput>(32);
    tokio::spawn(async move {
        while let Some(input) = tui_rx.recv().await {
            let _ = user_tx
                .send(spur_core::InteractiveInput {
                    text: input.text,
                    interrupt: input.interrupt,
                })
                .await;
        }
    });

    // Run TUI with permission channel
    spur_tui::run_tui(event_rx, Some(tui_tx), Some(perm_rx)).await?;

    orch_handle.abort();
    Ok(())
}
```

- [ ] **Step 2: Update run_tui call in lib.rs if needed**

Check if `run_tui` is re-exported from `spur_tui/src/lib.rs`. If so, update the re-export to match the new signature.

- [ ] **Step 3: Verify full workspace build**

Run: `cargo check`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/spur-cli/src/main.rs
git commit -m "feat(spur-cli): create permission channel in Watch command, wire to orchestrator and TUI"
```

---

### Task 9: Final verification

**Files:** None (verification only)

- [ ] **Step 1: Full workspace build**

Run: `cargo check`
Expected: PASS

- [ ] **Step 2: Grep for remaining auto-approve references**

Run: `grep -rn "Auto-approve\|auto-approving permission" crates/ --include="*.rs"`
Expected: Only the `auto_approve()` helper function (which is the fallback for non-interactive mode). The old inline auto-approve in `request_permission` should be gone.

- [ ] **Step 3: Verify permission channel is Optional everywhere**

Run: `grep -rn "permission_tx" crates/ --include="*.rs"`
Expected: All usages are `Option<...>`. No unwrap without a check.

- [ ] **Step 4: Commit any cleanup**

```bash
git add -A
git commit -m "chore: clean up permission flow implementation"
```
