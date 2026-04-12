# ACP Session Management (Sub-project 4 of 4)

## Problem

Spur creates a new session on every `spur watch` invocation. When spur restarts, all conversation context is lost. The ACP protocol supports session persistence via `load_session` and `list_sessions`, but spur's `AgentConnection` trait doesn't expose these methods.

## Solution

Add `load_session` and `list_sessions` to the `AgentConnection` trait with default error implementations. Implement them in `NativeAcpConnection`. Add a `--resume <session-id>` flag to `spur watch` that loads a prior session and streams its history into the TUI.

## Scope (v1)

**Implement:**
- `load_session` and `list_sessions` on `AgentConnection` trait
- `NativeAcpConnection` forwarding via `AcpCommand` (same pattern as prompt)
- `--resume <id>` flag on `spur watch`
- Orchestrator handles immediate spawn + history drain for resume

**Defer (v2):**
- `set_session_mode` / `set_session_config_option` — low value, same pattern, add later
- CLI `spur sessions list` command — nice-to-have, user can get IDs from agent storage
- TUI session picker — significant UI work, foundation first

## Design

### AgentConnection trait additions

```rust
#[async_trait]
pub trait AgentConnection: Send + Sync {
    // ... existing required methods ...

    /// Load a previously saved session. Returns a stream of history notifications.
    async fn load_session(
        &mut self,
        request: LoadSessionRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        anyhow::bail!("load_session not supported by this transport")
    }

    /// List available sessions from the agent.
    async fn list_sessions(
        &mut self,
        request: ListSessionsRequest,
    ) -> anyhow::Result<ListSessionsResponse> {
        anyhow::bail!("list_sessions not supported by this transport")
    }
}
```

`load_session` returns a `Stream<Item = SessionNotification>` — the same pattern as `prompt()`. The agent streams conversation history as notifications during `load_session`. The orchestrator drains this stream, emitting `SpurEvent::AgentNotification` for each, and the TUI renders them automatically.

### NativeAcpConnection implementation

Two new `AcpCommand` variants following the existing pattern:

```rust
enum AcpCommand {
    // ... existing variants ...
    LoadSession {
        request: LoadSessionRequest,
        reply: oneshot::Sender<anyhow::Result<mpsc::UnboundedReceiver<SessionNotification>>>,
    },
    ListSessions {
        request: ListSessionsRequest,
        reply: oneshot::Sender<anyhow::Result<ListSessionsResponse>>,
    },
}
```

`LoadSession` returns a notification receiver (same as `Prompt`) because the agent streams history during loading. The ACP thread handler swaps the notification channel destination before calling `connection.load_session()`, identical to the prompt handler pattern.

`ListSessions` is a simple request-response — no streaming.

### Orchestrator: --resume flow

`run_interactive` gains a `resume_session_id: Option<String>` parameter.

**Without --resume (current behavior):**
1. Wait for first user message
2. Lazy-spawn brain: `initialize` → `new_session` → `prompt`

**With --resume:**
1. Spawn brain IMMEDIATELY (not lazy): `initialize` → `load_session(session_id)`
2. Drain history notification stream → emit `SpurEvent::AgentNotification` for each
3. TUI renders conversation history in `ReactTrace`
4. Emit `SpurEvent::TurnComplete` to signal ready state
5. Wait for user message → `prompt()` as usual

The immediate spawn is necessary because history must be displayed before the user types their first message.

### CLI: --resume flag

```rust
Commands::Watch {
    brain: Option<String>,
    #[arg(long)]
    resume: Option<String>,  // NEW
}
```

Passed to `run_interactive` as `resume_session_id`.

### How the user gets a session ID

For v1, the user obtains session IDs from the agent's own storage. For kiro-cli, sessions are stored in `~/.kiro/sessions/cli/` as JSON files. The session ID is in the filename or metadata. A `spur sessions list` command can be added in v2.

## Files changed

| File | Change |
|------|--------|
| `spur-acp/src/connection/mod.rs` | 2 default methods on `AgentConnection`: `load_session` (returns Stream), `list_sessions` |
| `spur-acp/src/connection/native.rs` | 2 `AcpCommand` variants (`LoadSession`, `ListSessions`). 2 `AgentConnection` method impls forwarding via commands. 2 ACP thread handler arms (LoadSession swaps notification channel like Prompt; ListSessions is simple request-response). |
| `spur-acp/src/lib.rs` | Re-export `LoadSessionRequest`, `LoadSessionResponse`, `ListSessionsRequest`, `ListSessionsResponse`, `SessionInfo` |
| `spur-core/src/orchestrator.rs` | `run_interactive` takes `resume_session_id: Option<String>`. When `Some`: immediate spawn, call `load_session`, drain history stream as `SpurEvent::AgentNotification`, emit `TurnComplete`. `spawn_brain_session` gains a `resume_session_id` parameter to choose `load_session` vs `new_session`. |
| `spur-cli/src/main.rs` | `--resume <id>` flag on `Watch` command. Pass to `run_interactive`. |

## What does NOT change

- `StdioAdapter` / `CliWrapAdapter` (inherit default error implementations)
- TUI components (history notifications render automatically via existing `AgentNotification` path)
- `SpurAcpClientDynamic` (these are outgoing calls, not callbacks)
- Permission flow (Sub-project 2)
- Terminal operations (Sub-project 3)
