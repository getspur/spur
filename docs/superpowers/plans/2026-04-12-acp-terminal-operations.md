# ACP Terminal Operations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the 5 ACP terminal Client methods in `SpurAcpClientDynamic` so agents (kiro-cli) can execute shell commands through spur.

**Architecture:** A `TerminalState` struct tracks each spawned process. A `spawn_local` reader task owns the `Child` handle, reads stdout+stderr into a shared `Rc<RefCell<String>>` buffer, and signals exit via a `watch::channel`. `kill_terminal` uses the stored PID. All state lives on the `!Send` ACP thread — single file change.

**Tech Stack:** Rust, tokio (process, sync::watch, io::AsyncReadExt, task::spawn_local), agent-client-protocol SDK 0.10.4

---

### Task 1: Add TerminalState struct and terminals map to SpurAcpClientDynamic

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs`

- [ ] **Step 1: Add imports**

Add to the existing `use agent_client_protocol::{...}` block:

```rust
use agent_client_protocol::{
    // ... existing imports ...
    CreateTerminalRequest, CreateTerminalResponse,
    KillTerminalRequest, KillTerminalResponse,
    ReleaseTerminalRequest, ReleaseTerminalResponse,
    TerminalOutputRequest, TerminalOutputResponse,
    WaitForTerminalExitRequest, WaitForTerminalExitResponse,
    TerminalExitStatus, TerminalId,
};
```

Add at the top of the file:

```rust
use std::cell::Cell;
use std::collections::HashMap;
use tokio::io::AsyncReadExt;
```

- [ ] **Step 2: Add TerminalState struct**

Add before `SpurAcpClientDynamic`:

```rust
// ─── Terminal State ─────────────────────────────────────────────────────────

/// State for a single terminal process spawned by the agent.
struct TerminalState {
    /// Shared output buffer (stdout + stderr merged), written by the reader task.
    output: std::rc::Rc<std::cell::RefCell<String>>,
    /// Sticky flag — set when output exceeds byte_limit.
    truncated: std::rc::Rc<Cell<bool>>,
    /// None while running, Some(status) when exited.
    exit_rx: tokio::sync::watch::Receiver<Option<TerminalExitStatus>>,
    /// Process ID for kill operations.
    pid: u32,
}
```

- [ ] **Step 3: Add terminals field to SpurAcpClientDynamic**

```rust
struct SpurAcpClientDynamic {
    notification_tx: std::rc::Rc<std::cell::RefCell<mpsc::UnboundedSender<SessionNotification>>>,
    cwd: std::rc::Rc<std::cell::RefCell<PathBuf>>,
    permission_tx: Option<mpsc::UnboundedSender<crate::types::PermissionRequest>>,
    terminals: std::rc::Rc<std::cell::RefCell<HashMap<String, TerminalState>>>,
}
```

- [ ] **Step 4: Initialize terminals in acp_thread_main**

Where `SpurAcpClientDynamic` is constructed (around line 449):

```rust
let spur_client = SpurAcpClientDynamic {
    notification_tx: notification_tx_for_client,
    cwd: std::rc::Rc::new(std::cell::RefCell::new(PathBuf::from("."))),
    permission_tx,
    terminals: std::rc::Rc::new(std::cell::RefCell::new(HashMap::new())),
};
let cwd_ref = spur_client.cwd.clone();
let terminals_ref = spur_client.terminals.clone();  // for shutdown cleanup
```

- [ ] **Step 5: Add terminal cleanup to shutdown handler**

In the `AcpCommand::Shutdown` arm (around line 552):

```rust
AcpCommand::Shutdown { reply } => {
    tracing::debug!(agent = %agent_name, "NativeAcpConnection: ACP thread received shutdown");
    // Kill all spawned terminals.
    for (id, terminal) in terminals_ref.borrow().iter() {
        if terminal.exit_rx.borrow().is_none() {
            tracing::debug!(terminal = %id, "Killing terminal on shutdown");
            let _ = std::process::Command::new("kill")
                .arg("-9")
                .arg(terminal.pid.to_string())
                .status();
        }
    }
    // Kill the agent child process.
    let _ = child.kill().await;
    let _ = reply.send(Ok(()));
    break;
}
```

- [ ] **Step 6: Verify build**

Run: `cargo check --package spur-acp`
Expected: PASS (new struct and field are used but Client methods not yet implemented — defaults still in place)

- [ ] **Step 7: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "feat(spur-acp): add TerminalState struct and terminals map for terminal operations"
```

---

### Task 2: Add the terminal_reader helper and append_output helper

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs`

- [ ] **Step 1: Add append_output helper**

Add after the `auto_deny` function at the bottom of the file:

```rust
// ─── Terminal helpers ────────────────────────────────────────────────────────

/// Append bytes to the shared output buffer, applying byte-limit truncation.
fn append_terminal_output(
    output: &std::rc::Rc<std::cell::RefCell<String>>,
    truncated: &std::rc::Rc<Cell<bool>>,
    byte_limit: Option<u64>,
    data: &[u8],
) {
    // Best-effort UTF-8 conversion (lossy for binary output)
    let text = String::from_utf8_lossy(data);
    let mut buf = output.borrow_mut();
    buf.push_str(&text);

    if let Some(limit) = byte_limit {
        let limit = limit as usize;
        if buf.len() > limit {
            let mut start = buf.len() - limit;
            while !buf.is_char_boundary(start) {
                start += 1;
            }
            *buf = buf[start..].to_string();
            truncated.set(true);
        }
    }
}
```

- [ ] **Step 2: Add terminal_reader async function**

Add after `append_terminal_output`:

```rust
/// Background task that reads stdout+stderr into a shared buffer and signals exit.
/// Owns the Child handle — kill is done via PID from TerminalState.
async fn terminal_reader(
    mut stdout: tokio::process::ChildStdout,
    mut stderr: tokio::process::ChildStderr,
    mut child: tokio::process::Child,
    output: std::rc::Rc<std::cell::RefCell<String>>,
    truncated: std::rc::Rc<Cell<bool>>,
    byte_limit: Option<u64>,
    exit_tx: tokio::sync::watch::Sender<Option<TerminalExitStatus>>,
) {
    let mut stdout_buf = [0u8; 4096];
    let mut stderr_buf = [0u8; 4096];
    let mut stdout_done = false;
    let mut stderr_done = false;

    loop {
        if stdout_done && stderr_done {
            break;
        }
        tokio::select! {
            result = AsyncReadExt::read(&mut stdout, &mut stdout_buf), if !stdout_done => {
                match result {
                    Ok(0) | Err(_) => stdout_done = true,
                    Ok(n) => append_terminal_output(&output, &truncated, byte_limit, &stdout_buf[..n]),
                }
            }
            result = AsyncReadExt::read(&mut stderr, &mut stderr_buf), if !stderr_done => {
                match result {
                    Ok(0) | Err(_) => stderr_done = true,
                    Ok(n) => append_terminal_output(&output, &truncated, byte_limit, &stderr_buf[..n]),
                }
            }
        }
    }

    let exit_status = match child.wait().await {
        Ok(status) => {
            let mut es = TerminalExitStatus::new();
            if let Some(code) = status.code() {
                es = es.exit_code(code as u32);
            }
            es
        }
        Err(_) => TerminalExitStatus::new(),
    };
    let _ = exit_tx.send(Some(exit_status));
}
```

- [ ] **Step 3: Verify build**

Run: `cargo check --package spur-acp`
Expected: PASS (helpers exist but are not yet called — will show unused warnings, that's fine)

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "feat(spur-acp): add terminal_reader task and append_terminal_output helper"
```

---

### Task 3: Implement create_terminal

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs`

- [ ] **Step 1: Add create_terminal to impl Client for SpurAcpClientDynamic**

Add inside the `impl Client for SpurAcpClientDynamic` block, after the `write_text_file` method:

```rust
    async fn create_terminal(
        &self,
        args: CreateTerminalRequest,
    ) -> agent_client_protocol::Result<CreateTerminalResponse> {
        let cwd = args.cwd.clone().unwrap_or_else(|| self.cwd.borrow().clone());
        let byte_limit = args.output_byte_limit.or(Some(10 * 1024 * 1024)); // 10MB default

        let mut cmd = tokio::process::Command::new(&args.command);
        cmd.args(&args.args)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        for env_var in &args.env {
            cmd.env(&env_var.name, &env_var.value);
        }

        let mut child = cmd.spawn().map_err(|e| {
            agent_client_protocol::Error::internal_error()
                .data(format!("Failed to spawn '{}': {e}", args.command))
        })?;

        let pid = child.id().ok_or_else(|| {
            agent_client_protocol::Error::internal_error()
                .data("Failed to get process ID")
        })?;

        let child_stdout = child.stdout.take().ok_or_else(|| {
            agent_client_protocol::Error::internal_error()
                .data("Failed to capture stdout")
        })?;
        let child_stderr = child.stderr.take().ok_or_else(|| {
            agent_client_protocol::Error::internal_error()
                .data("Failed to capture stderr")
        })?;

        let output = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let truncated = std::rc::Rc::new(Cell::new(false));
        let (exit_tx, exit_rx) = tokio::sync::watch::channel(None);

        // Spawn reader task — owns child, reads stdout/stderr, signals exit.
        tokio::task::spawn_local(terminal_reader(
            child_stdout,
            child_stderr,
            child,
            output.clone(),
            truncated.clone(),
            byte_limit,
            exit_tx,
        ));

        let terminal_id = TerminalId::new(uuid::Uuid::new_v4().to_string());
        let id_string = terminal_id.to_string();

        tracing::debug!(
            terminal = %id_string,
            command = %args.command,
            pid = pid,
            "Terminal created"
        );

        self.terminals.borrow_mut().insert(
            id_string,
            TerminalState { output, truncated, exit_rx, pid },
        );

        Ok(CreateTerminalResponse::new(terminal_id))
    }
```

- [ ] **Step 2: Verify build**

Run: `cargo check --package spur-acp`
Expected: PASS (may have warnings about unused imports for other terminal types — fixed in Task 4)

- [ ] **Step 3: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "feat(spur-acp): implement create_terminal with reader task and output capture"
```

---

### Task 4: Implement terminal_output, wait_for_terminal_exit, kill_terminal, release_terminal

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs`

- [ ] **Step 1: Add terminal_output**

Add inside `impl Client for SpurAcpClientDynamic`, after `create_terminal`:

```rust
    async fn terminal_output(
        &self,
        args: TerminalOutputRequest,
    ) -> agent_client_protocol::Result<TerminalOutputResponse> {
        let key = args.terminal_id.to_string();
        let map = self.terminals.borrow();
        let terminal = map.get(&key).ok_or_else(|| {
            agent_client_protocol::Error::invalid_params()
                .data(format!("Terminal '{}' not found", key))
        })?;

        let output = terminal.output.borrow().clone();
        let truncated = terminal.truncated.get();
        let exit_status = terminal.exit_rx.borrow().clone();

        Ok(TerminalOutputResponse::new(output)
            .truncated(truncated)
            .exit_status(exit_status))
    }
```

- [ ] **Step 2: Add wait_for_terminal_exit**

```rust
    async fn wait_for_terminal_exit(
        &self,
        args: WaitForTerminalExitRequest,
    ) -> agent_client_protocol::Result<WaitForTerminalExitResponse> {
        let key = args.terminal_id.to_string();

        // Clone the watch receiver, then drop the map borrow.
        let mut exit_rx = {
            let map = self.terminals.borrow();
            let terminal = map.get(&key).ok_or_else(|| {
                agent_client_protocol::Error::invalid_params()
                    .data(format!("Terminal '{}' not found", key))
            })?;
            terminal.exit_rx.clone()
        };

        // If already exited, return immediately.
        if let Some(status) = exit_rx.borrow().clone() {
            return Ok(WaitForTerminalExitResponse::new(status));
        }

        // Wait for exit.
        loop {
            match exit_rx.changed().await {
                Ok(()) => {
                    if let Some(status) = exit_rx.borrow().clone() {
                        return Ok(WaitForTerminalExitResponse::new(status));
                    }
                }
                Err(_) => {
                    // Sender dropped — reader task exited. Return whatever we have.
                    let status = exit_rx.borrow().clone()
                        .unwrap_or_else(TerminalExitStatus::new);
                    return Ok(WaitForTerminalExitResponse::new(status));
                }
            }
        }
    }
```

- [ ] **Step 3: Add kill_terminal**

```rust
    async fn kill_terminal(
        &self,
        args: KillTerminalRequest,
    ) -> agent_client_protocol::Result<KillTerminalResponse> {
        let key = args.terminal_id.to_string();

        let (pid, is_running) = {
            let map = self.terminals.borrow();
            let terminal = map.get(&key).ok_or_else(|| {
                agent_client_protocol::Error::invalid_params()
                    .data(format!("Terminal '{}' not found", key))
            })?;
            (terminal.pid, terminal.exit_rx.borrow().is_none())
        };

        if is_running {
            tracing::debug!(terminal = %key, pid = pid, "Killing terminal");
            let _ = std::process::Command::new("kill")
                .arg("-9")
                .arg(pid.to_string())
                .status();
        }

        Ok(KillTerminalResponse::new())
    }
```

- [ ] **Step 4: Add release_terminal**

```rust
    async fn release_terminal(
        &self,
        args: ReleaseTerminalRequest,
    ) -> agent_client_protocol::Result<ReleaseTerminalResponse> {
        let key = args.terminal_id.to_string();

        // Kill if still running, then remove from map.
        let pid_to_kill = {
            let map = self.terminals.borrow();
            if let Some(terminal) = map.get(&key) {
                if terminal.exit_rx.borrow().is_none() {
                    Some(terminal.pid)
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some(pid) = pid_to_kill {
            tracing::debug!(terminal = %key, pid = pid, "Killing terminal on release");
            let _ = std::process::Command::new("kill")
                .arg("-9")
                .arg(pid.to_string())
                .status();
        }

        self.terminals.borrow_mut().remove(&key);
        Ok(ReleaseTerminalResponse::new())
    }
```

- [ ] **Step 5: Verify full build**

Run: `cargo check`
Expected: PASS — all terminal methods implemented, full workspace compiles.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "feat(spur-acp): implement terminal_output, wait_for_terminal_exit, kill_terminal, release_terminal"
```

---

### Task 5: Final verification

**Files:** None (verification only)

- [ ] **Step 1: Full workspace build**

Run: `cargo check`
Expected: PASS

- [ ] **Step 2: Verify all 5 Client methods are implemented**

Run: `grep -n "async fn create_terminal\|async fn terminal_output\|async fn wait_for_terminal_exit\|async fn kill_terminal\|async fn release_terminal" crates/spur-acp/src/connection/native.rs`
Expected: 5 matches, all inside `impl Client for SpurAcpClientDynamic`

- [ ] **Step 3: Verify cleanup in shutdown handler**

Run: `grep -A5 "Kill all spawned terminals" crates/spur-acp/src/connection/native.rs`
Expected: Shows the terminal cleanup loop before child.kill()

- [ ] **Step 4: Commit any cleanup**

```bash
git add -A
git commit -m "chore: clean up terminal operations implementation"
```
