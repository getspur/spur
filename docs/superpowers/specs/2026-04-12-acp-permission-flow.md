# ACP Interactive Permission Flow (Sub-project 2 of 4)

## Problem

`SpurAcpClientDynamic::request_permission()` auto-approves every tool call by selecting the first option. This is a security risk — agents can read, write, and execute without user consent. The TUI already has placeholder rendering for permissions (`TraceKind::Permission` with [y]/[n]/[a] hints and countdown), but no actual channel connects the ACP thread to the TUI.

## Solution

Add a dedicated permission channel from the ACP thread to the TUI. When an agent requests permission, the request flows to the TUI, the user responds, and the response flows back via a oneshot channel.

## Architecture

```
main.rs creates (perm_tx, perm_rx)
    │                                    │
    ▼                                    ▼
orch.run_interactive(perm_tx)            run_tui(perm_rx)
    │
    ▼ passed as parameter (not stored on Orchestrator)
spawn_brain_session(perm_tx)
    │
    ▼ clone
NativeAcpConnection::new(perm_tx)
    │
    ▼ passed to thread
acp_thread_main(perm_tx)
    │
    ▼
SpurAcpClientDynamic.request_permission():
    1. Create oneshot::channel()
    2. Send PermissionRequest { args (raw SDK), reply_tx } via perm_tx
    3. Await reply_rx with 60s timeout
    4. Map response → SDK RequestPermissionResponse
```

### Why a dedicated channel (not SpurEvent broadcast)

The permission response requires a `oneshot::Sender` which is not `Clone`. The broadcast channel requires `Clone` on all messages. Therefore permission requests cannot flow through the existing `SpurEvent` broadcast — they need their own channel.

### Why created in main.rs

A single channel serves all agents (brain + workers). The `perm_tx` is cloned per connection. The TUI has one `perm_rx` to poll. This matches the existing pattern for the event broadcast and user input channels.

## Types

```rust
// spur-acp/src/types.rs

/// Permission request sent from the ACP thread to the TUI via a dedicated channel.
/// Carries the raw SDK type for full protocol access (consistent with Sub-project 1
/// pass-through philosophy). If RequestPermissionRequest is not Send (unlikely —
/// all fields are String/Value/Vec), fall back to extracted fields.
pub struct PermissionRequest {
    pub args: agent_client_protocol::RequestPermissionRequest,
    pub reply_tx: tokio::sync::oneshot::Sender<PermissionResponse>,
}

pub struct PermissionResponse {
    pub option_id: String,
}
```

Re-export `RequestPermissionRequest` and `PermissionOption` from `spur-acp/src/lib.rs` so the TUI can access `args.tool_call`, `args.options`, etc. without adding `agent-client-protocol` to its Cargo.toml.

## ACP Thread Implementation

In `SpurAcpClientDynamic::request_permission()`:

```rust
async fn request_permission(&self, args: RequestPermissionRequest) -> Result<RequestPermissionResponse> {
    let Some(perm_tx) = &self.permission_tx else {
        // No TUI connected — auto-approve (non-interactive mode)
        return auto_approve(&args);
    };

    let (reply_tx, reply_rx) = oneshot::channel();
    let request = PermissionRequest {
        args: args.clone(),  // pass raw SDK type through
        reply_tx,
    };

    if perm_tx.send(request).is_err() {
        return auto_approve(&args);  // TUI disconnected
    }

    match tokio::time::timeout(Duration::from_secs(60), reply_rx).await {
        Ok(Ok(response)) => {
            let option_id = PermissionOptionId::new(response.option_id);
            Ok(RequestPermissionResponse::new(
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id))
            ))
        }
        _ => {
            // Timeout or channel dropped — select most restrictive option
            auto_deny(&args)
        }
    }
}

fn auto_deny(args: &RequestPermissionRequest) -> Result<RequestPermissionResponse> {
    // Select the last option (conventionally the most restrictive / deny).
    // If no options exist, use "deny" as fallback.
    let option_id = args.options.last()
        .map(|o| o.option_id.clone())
        .unwrap_or_else(|| PermissionOptionId::new("deny"));
    Ok(RequestPermissionResponse::new(
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id))
    ))
}

fn auto_approve(args: &RequestPermissionRequest) -> Result<RequestPermissionResponse> {
    let option_id = args.options.first()
        .map(|o| o.option_id.clone())
        .unwrap_or_else(|| PermissionOptionId::new("allow"));
    Ok(RequestPermissionResponse::new(
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id))
    ))
}
```

The `permission_tx` field is `Option<mpsc::UnboundedSender<PermissionRequest>>`:
- `Some(tx)` — interactive mode (Watch command), send request to TUI
- `None` — non-interactive mode (Run command), auto-approve as before

## TUI Integration

### Event loop

`run_tui()` gains an optional `perm_rx` parameter. The event loop adds a fourth `tokio::select!` branch:

```rust
Some(perm) = perm_rx.recv() => {
    app.handle_permission_request(perm);
}
```

### App state

```rust
struct App {
    // ... existing fields ...
    pending_permission: Option<(PermissionRequest, Instant)>,  // request + deadline
}
```

### handle_permission_request

1. If a permission is already pending, drop the old `reply_tx` (auto-deny the previous request)
2. Store the new `PermissionRequest` in `pending_permission`
3. Push `TraceKind::Permission { description: tool_title, pending: true, countdown: 30 }` to the active session's `ReactTrace`

### Key handling

The existing placeholder code in `session_detail.rs` already handles [y]/[n]/[a] when `has_pending_permission()` is true. Wire these to return Actions. The key-to-option mapping uses the SDK's `args.options` dynamically:

- `[y]` → `Action::PermissionResponse { option_id }` using the **first** option (conventionally allow)
- `[n]` → `Action::PermissionDenied` (drops `reply_tx`, ACP thread falls back to last/deny option)
- `[a]` → `Action::PermissionResponse { option_id }` using the option with "always" in its name/id (if present; if not, same as [y])

The TraceKind::Permission hint text is generated from actual option names:
```
⚠ PERMISSION: Edit file foo.rs
   [y] Allow  [n] Deny  [a] Always Allow  (auto-deny in 28s)
```

### process_action (App)

When `Action::PermissionResponse { option_id }` is received:
1. Take `pending_permission` from `self`
2. Send `PermissionResponse { option_id }` via the stored `reply_tx`
3. Update the `TraceKind::Permission` entry to `pending: false`

When `Action::PermissionDenied`:
1. Drop `pending_permission` (dropping `reply_tx` signals denial to ACP thread)
2. Update the trace entry to `pending: false`

## Timeout

**TUI-side (authoritative):** App tracks the deadline via `Instant::now() + Duration::from_secs(30)` stored alongside the `PermissionRequest`. In `App::tick()`:
```rust
if let Some((_, deadline)) = &self.pending_permission {
    if Instant::now() >= *deadline {
        self.pending_permission.take();  // drops reply_tx → auto-deny
        // Update trace entry to pending: false
    }
}
```
The `ReactTrace::tick()` countdown is **cosmetic only** — it shows seconds remaining to the user but does not control the actual timeout. The App's `Instant`-based timer is authoritative. This decouples rendering from logic.

**ACP-side (safety net):** `tokio::time::timeout(60s)` on `reply_rx.await`. If TUI crashes or channel breaks, the ACP thread doesn't block forever. Falls back to the most restrictive option.

## Backward compatibility

- `permission_tx` is `Option` on both `NativeAcpConnection` and `SpurAcpClientDynamic`
- When `None`: auto-approve as before (non-interactive commands like `spur run`)
- When `Some`: interactive permission flow (only `spur watch` sets this)
- `StdioAdapter` and `CliWrapAdapter` are unaffected — they don't use the `Client` trait

## Files changed

| File | Change |
|------|--------|
| `spur-acp/src/types.rs` | Add `PermissionRequest`, `PermissionResponse` |
| `spur-acp/src/lib.rs` | Re-export `RequestPermissionRequest`, `PermissionOption`, `PermissionOptionId` from SDK |
| `spur-acp/src/connection/native.rs` | Add `permission_tx` field to `NativeAcpConnection` and `SpurAcpClientDynamic`. Replace auto-approve in `request_permission()` with channel send + await. Thread `permission_tx` through `acp_thread_main()`. |
| `spur-core/src/orchestrator.rs` | Add `permission_tx` parameter to `run_interactive()` and `spawn_brain_session()`. Pass to `NativeAcpConnection::new()`. No new field on Orchestrator — parameter flows through. |
| `spur-tui/src/app.rs` | Add `pending_permission: Option<(PermissionRequest, Instant)>` field. Add `perm_rx` to event loop. Handle permission request arrival. Process `PermissionResponse`/`PermissionDenied` actions. Instant-based timeout in tick(). |
| `spur-tui/src/views/session_detail.rs` | Wire [y]/[n]/[a] placeholder handlers to return `Action::PermissionResponse`/`PermissionDenied`. |
| `spur-tui/src/action.rs` | Add `PermissionResponse { option_id: String }` and `PermissionDenied` variants. |
| `spur-cli/src/main.rs` | Create `(perm_tx, perm_rx)` in Watch command. Pass `perm_tx` to orchestrator, `perm_rx` to `run_tui()`. |

## What does NOT change

- `AgentConnection` trait (permission is NativeAcpConnection-specific)
- `StdioAdapter` / `CliWrapAdapter` (no permission support)
- `ReactTrace` rendering or tick countdown (already implemented)
- `SpurEvent` enum (permissions use dedicated channel)
- Terminal operations (Sub-project 3)
- Session management (Sub-project 4)
