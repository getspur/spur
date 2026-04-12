# ACP Terminal Operations (Sub-project 3 of 4)

## Problem

`SpurAcpClientDynamic` uses the default `Client` trait implementations for terminal operations, which return errors. When kiro-cli (or any ACP agent) calls `create_terminal` to execute shell commands, it fails. This prevents agents from running build commands, tests, linters, or any CLI tool through spur.

## Solution

Implement the 5 terminal `Client` methods in `SpurAcpClientDynamic`: `create_terminal`, `terminal_output`, `wait_for_terminal_exit`, `kill_terminal`, `release_terminal`. All state is local to the `!Send` ACP thread — no cross-thread communication, no orchestrator changes, no TUI changes.

## Architecture

### TerminalState

```rust
struct TerminalState {
    output: Rc<RefCell<String>>,
    truncated: Rc<Cell<bool>>,
    exit_rx: tokio::sync::watch::Receiver<Option<TerminalExitStatus>>,
    pid: u32,
}
```

- **output** — shared with the reader task, continuously filled from stdout+stderr
- **truncated** — sticky flag, set when output exceeds `output_byte_limit`
- **exit_rx** — watch channel receiver, `None` while running, `Some(status)` when exited
- **pid** — process ID for kill, stored as `u32` (Copy, no borrow needed)

The `Child` process handle is **owned by the reader task** (moved into `spawn_local`), not stored in `TerminalState`. This avoids `Rc<RefCell<Child>>` borrow-across-await issues.

### SpurAcpClientDynamic changes

```rust
struct SpurAcpClientDynamic {
    notification_tx: Rc<RefCell<mpsc::UnboundedSender<SessionNotification>>>,
    cwd: Rc<RefCell<PathBuf>>,
    permission_tx: Option<mpsc::UnboundedSender<PermissionRequest>>,
    terminals: Rc<RefCell<HashMap<String, TerminalState>>>,  // NEW
}
```

### Data flow

```
create_terminal(command, args, cwd, env)
    ├── Spawn tokio::process::Command with piped stdout+stderr
    ├── child.id() → store PID in TerminalState
    ├── child.stdout.take() + child.stderr.take()
    ├── watch::channel(None) for exit notification
    └── spawn_local reader task (OWNS child):
            ├── tokio::select! reads stdout + stderr into shared buffer
            ├── Applies output_byte_limit truncation (trim from front, char-boundary safe)
            ├── When both streams EOF → child.wait()
            └── watch_tx.send(Some(TerminalExitStatus { exit_code, signal }))

terminal_output(terminal_id)
    → Borrow output from map → clone String → check exit_rx → return

wait_for_terminal_exit(terminal_id)
    → Clone exit_rx from map → drop borrow → await until Some → return status

kill_terminal(terminal_id)
    → Get PID from map → send SIGKILL

release_terminal(terminal_id)
    → Kill if still running → remove from map
```

## Method implementations

### create_terminal

1. Resolve `cwd`: use `args.cwd` if provided, else `self.cwd` (session working directory)
2. Resolve `output_byte_limit`: use `args.output_byte_limit.or(Some(10 * 1024 * 1024))` — default 10MB limit prevents OOM when agent doesn't set one
3. Build `tokio::process::Command` with `args.command`, `args.args`, cwd, env vars (iterate `args.env`, call `.env(name, value)` for each)
4. Set `stdout(Stdio::piped())`, `stderr(Stdio::piped())`
5. Spawn the process
6. Get PID from `child.id()` — return error if None (defensive; process may exit between spawn and id())
6. Take `stdout` and `stderr` handles from child
7. Create `watch::channel::<Option<TerminalExitStatus>>(None)`
8. Create shared `output: Rc<RefCell<String>>` and `truncated: Rc<Cell<bool>>`
9. `spawn_local` the reader task (takes ownership of child, stdout, stderr, output clone, exit_tx)
10. Generate `TerminalId` (UUID)
11. Insert `TerminalState { output, truncated, exit_rx, pid }` into `self.terminals`
12. Return `CreateTerminalResponse::new(terminal_id)`

### terminal_output

1. Look up terminal by ID in map
2. Return `TerminalOutputResponse` with:
   - `output`: `terminal.output.borrow().clone()`
   - `truncated`: `terminal.truncated.get()`
   - `exit_status`: `terminal.exit_rx.borrow().clone()`

### wait_for_terminal_exit

1. Look up terminal, clone `exit_rx`
2. Drop the map borrow
3. Loop: `exit_rx.changed().await`
   - `Ok(())` → check if value is `Some` → return exit status
   - `Err(_)` → sender dropped (reader task exited). Check final value; if None, return default `TerminalExitStatus::new()`
4. Return `WaitForTerminalExitResponse::new(exit_status)`

### kill_terminal

1. Look up terminal, extract `pid` and check `exit_rx.borrow().is_none()` (still running?) in ONE borrow scope
2. Drop the map borrow
3. If still running, kill via `std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status()`. Check-then-kill prevents PID reuse issues.
4. Return `KillTerminalResponse::new()`

### release_terminal

1. Look up terminal, check if still running (exit_rx)
2. If running, kill the PID
3. Remove from map (drops all shared state, reader task will exit on next write attempt)
4. Return `ReleaseTerminalResponse::new()`

## Reader task

```rust
async fn terminal_reader(
    mut stdout: ChildStdout,
    mut stderr: ChildStderr,
    mut child: Child,
    output: Rc<RefCell<String>>,
    truncated: Rc<Cell<bool>>,
    byte_limit: Option<u64>,
    exit_tx: watch::Sender<Option<TerminalExitStatus>>,
) {
    let mut stdout_buf = [0u8; 4096];
    let mut stderr_buf = [0u8; 4096];
    let mut stdout_done = false;
    let mut stderr_done = false;

    loop {
        if stdout_done && stderr_done { break; }  // MUST check before select!
        tokio::select! {
            result = AsyncReadExt::read(&mut stdout, &mut stdout_buf), if !stdout_done => {
                match result {
                    Ok(0) | Err(_) => stdout_done = true,
                    Ok(n) => append_output(&output, &truncated, byte_limit, &stdout_buf[..n]),
                }
            }
            result = AsyncReadExt::read(&mut stderr, &mut stderr_buf), if !stderr_done => {
                match result {
                    Ok(0) | Err(_) => stderr_done = true,
                    Ok(n) => append_output(&output, &truncated, byte_limit, &stderr_buf[..n]),
                }
            }
        }
    }

    let exit_status = match child.wait().await {
        Ok(status) => {
            let mut es = TerminalExitStatus::new();
            if let Some(code) = status.code() { es = es.exit_code(code as u32); }
            es
        }
        Err(_) => TerminalExitStatus::new(),
    };
    let _ = exit_tx.send(Some(exit_status));
}
```

The reader task runs on the `LocalSet` via `spawn_local`. It merges stdout and stderr into a single buffer (ACP spec only has one `output` field). The `if stdout_done && stderr_done` guard MUST be checked before `select!` — an empty `select!` (no enabled branches) panics. Output truncation uses the same char-boundary-safe pattern as the dashboard's text_batch capping.

## Cleanup

When the ACP thread exits (shutdown command or crash), all terminal processes must be killed. `tokio::process::Child` does NOT kill on drop — explicit cleanup is required.

The `terminals` map is `Rc<RefCell<HashMap>>`. Before moving `SpurAcpClientDynamic` into `ClientSideConnection::new()`, clone the `Rc` (same pattern as existing `cwd_ref`):

```rust
let terminals_ref = spur_client.terminals.clone();  // Rc clone — cheap
```

In the shutdown handler of `acp_thread_main`, use this retained reference:
```rust
AcpCommand::Shutdown { reply } => {
    for (_, terminal) in terminals_ref.borrow().iter() {
        let _ = std::process::Command::new("kill")
            .arg("-9").arg(terminal.pid.to_string()).status();
    }
    let _ = child.kill().await;
    let _ = reply.send(Ok(()));
    break;
}
```

## Files changed

| File | Change |
|------|--------|
| `crates/spur-acp/src/connection/native.rs` | Add `TerminalState` struct. Add `terminals` field to `SpurAcpClientDynamic`. Implement `create_terminal`, `terminal_output`, `wait_for_terminal_exit`, `kill_terminal`, `release_terminal`. Add `terminal_reader` async fn. Add terminal cleanup to shutdown path. |

## What does NOT change

- `AgentConnection` trait (terminal ops are Client-side only)
- Orchestrator (doesn't interact with terminal ops)
- TUI (tool call notifications from the agent provide UI representation)
- main.rs (no new channels needed)
- Permission flow (agents gate terminal execution via request_permission)
- Session management (Sub-project 4)
