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
2. Build `tokio::process::Command` with `args.command`, `args.args`, cwd, env vars
3. Set `stdout(Stdio::piped())`, `stderr(Stdio::piped())`
4. Spawn the process
5. Get PID from `child.id()`
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
3. Loop: `exit_rx.changed().await`, check if value is `Some`
4. Return `WaitForTerminalExitResponse::new(exit_status)`

### kill_terminal

1. Look up terminal, get `pid`
2. Kill via `std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status()`. Safe, no `unsafe` block, works on macOS and Linux.
3. Return `KillTerminalResponse::new()`

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
    // Read both streams into shared buffer using tokio::select!
    // When both EOF → child.wait() → send exit status
}
```

The reader task runs on the `LocalSet` via `spawn_local`. It merges stdout and stderr into a single buffer (ACP spec only has one `output` field). Output truncation uses the same char-boundary-safe pattern as the dashboard's text_batch capping.

## Cleanup

When the ACP thread exits (shutdown command or crash), all terminal processes must be killed. In the shutdown handler of `acp_thread_main`, iterate `terminals` and kill each PID. `tokio::process::Child` does NOT kill on drop — explicit cleanup is required.

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
