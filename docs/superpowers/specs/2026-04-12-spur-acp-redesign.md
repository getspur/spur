# SPUR ACP Redesign: ACP-First Architecture

**Date:** 2026-04-12
**Status:** Approved
**Scope:** spur-acp, spur-mcp, spur-core, spur-tui, spur-pm, spur-cost

## Problem Statement

The `spur-acp` crate hand-rolls a subset of the Agent Client Protocol (ACP) against JSON-RPC 2.0 over stdio (~950 lines of custom protocol code). An official Rust SDK (`agent-client-protocol` v0.10.4) now exists, maintained by Zed + JetBrains, implementing the full spec. The current implementation has three critical issues:

1. **Custom protocol divergence** -- spur's ACP implementation uses non-standard event names, lacks client-side method dispatch (fs/read, terminal/create), and places `mcpServers` in `initialize` instead of `session/new`.
2. **Parallel delegation bug** -- `wait_for_response()` holds a Mutex on a shared response channel and drops non-matching responses. Parallel delegations silently lose results.
3. **Workers are deaf and blind** -- worker agents are spawned with `initialize(None)`, receiving no MCP endpoint. Spur cannot observe workers through ACP client callbacks because it doesn't implement the ACP `Client` trait.

## Design Principles

1. **ACP is the first-class interface.** Non-ACP agents (Stdio, CliWrap) adapt UP to ACP, not the other way around.
2. **MCP is for LLM-visible tools only.** ACP handles all agent lifecycle and observation. MCP is used only where ACP cannot reach: making custom tools (delegation, PM) visible to the brain's LLM.
3. **Workers use ACP only.** Spur observes workers through ACP session streams and client-side callbacks. No MCP for workers.

## Architecture

### Two-Layer Protocol Stack

```
BRAIN AGENT:
  ACP ---- lifecycle, streaming, fs/terminal callbacks (Client trait)
  MCP ---- delegation tools only (delegate_to_worker, list_workers, PM tools)

WORKER AGENTS:
  ACP ---- lifecycle, streaming, fs/terminal callbacks (Client trait)
  (no MCP)
```

**Why MCP for brain only:** Delegation is an LLM-level decision. The brain's LLM must see `delegate_to_worker` as a callable tool. ACP has no mechanism for clients to expose custom LLM-visible tools. MCP is the standard mechanism for dynamic tool discovery (`tools/list` -> `tools/call`). Works with any ACP agent without agent-side modifications.

**Why no MCP for workers:** ACP provides complete bidirectional communication. The worker calls back to spur through ACP client-side methods (`fs/read_text_file`, `fs/write_text_file`, `terminal/create`, `session/request_permission`). Spur observes workers through the ACP `session/update` stream. No additional channel needed.

### ACP Client-Side Callbacks (spur implements)

| Agent -> Spur (via ACP) | Spur behavior |
|---|---|
| `session/update` stream | Forward to event bus -> TUI observes in real-time |
| `fs/read_text_file` | Read from agent's worktree, log the access |
| `fs/write_text_file` | Write to agent's worktree, log the change |
| `terminal/create` | Spawn command in worktree context, log |
| `terminal/output` / `wait_for_exit` | Return command results |
| `terminal/kill` / `terminal/release` | Terminate command |
| `session/request_permission` | Auto-approve for workers, policy check for brain |

### Connection Architecture

```
Orchestrator (speaks ACP natively via AgentConnection trait)
    |
    |-- NativeAcpConnection (wraps official ClientSideConnection)
    |       |-- Kiro, Claude Code, Codex, Gemini...
    |
    |-- StdioAdapter (translates raw I/O -> ACP message types)
    |       |-- Any stdin/stdout agent
    |
    |-- CliWrapAdapter (translates one-shot CLI -> ACP lifecycle)
            |-- Any CLI tool
```

## Crate Structure

### spur-acp module layout

```
spur-acp/src/
  lib.rs              # Re-exports for backward compatibility
  connection/
    mod.rs            # AgentConnection trait (speaks official ACP types)
    native.rs         # NativeAcpConnection -- wraps SDK ClientSideConnection
    stdio_adapter.rs  # StdioAdapter -- raw I/O -> ACP messages
    cli_wrap_adapter.rs # CliWrapAdapter -- one-shot CLI -> ACP lifecycle
  domain/
    mod.rs
    events.rs         # SpurEvent enum
    delegation.rs     # DelegationStatus, DelegationResult
  config.rs           # SpurConfig, AgentConfig (unchanged)
  registry.rs         # AgentRegistry (unchanged)
```

### PM types move to spur-pm

`Issue`, `PrParams`, `IssueFilter`, `IssueSummary`, `IssueUpdate`, `PmEvent`, `PmSource` (~70 lines) move to `spur-pm/src/types.rs`. No new crates created.

### Dependency changes

- `spur-acp` gains: `agent-client-protocol = "0.10"`
- `spur-acp` loses: ~500 lines of custom JSON-RPC code
- `spur-core` imports PM types from `spur-pm` instead of `spur-acp`

## AgentConnection Trait

```rust
use agent_client_protocol::{
    InitializeParams, InitializeResult,
    SessionNewResult, SessionPromptParams,
    SessionUpdate, McpServerConfig,
};

#[async_trait]
pub trait AgentConnection: Send + Sync {
    async fn initialize(
        &mut self,
        params: InitializeParams,
    ) -> Result<InitializeResult>;

    async fn new_session(
        &mut self,
        mcp_servers: Option<Vec<McpServerConfig>>,
    ) -> Result<SessionNewResult>;

    async fn prompt(
        &mut self,
        params: SessionPromptParams,
    ) -> Result<Pin<Box<dyn Stream<Item = SessionUpdate> + Send>>>;

    async fn cancel(&mut self, session_id: &str) -> Result<()>;

    async fn shutdown(&mut self) -> Result<()>;
}
```

### NativeAcpConnection

Wraps the official SDK's `ClientSideConnection`. Spur implements the ACP `Client` trait to handle agent callbacks (fs/read, terminal/create, etc.). The ~500 lines of custom JSON-RPC code (send_request, send_notification, parse_session_event, spawn_process) are entirely replaced by the SDK.

### StdioAdapter

Translates raw stdin/stdout agents into ACP message types:
- `initialize()`: spawn process, return synthetic `InitializeResult` with `supports_sessions: false`, `supports_mcp: false`
- `new_session()`: synthetic session ID (the process IS the session)
- `prompt()`: write delimited text to stdin, translate stdout lines to `SessionUpdate::AgentMessageChunk`, idle timeout -> `TurnEnd`

### CliWrapAdapter

Translates one-shot CLI tools into single-session ACP lifecycle:
- `initialize()`: verify binary exists, return synthetic `InitializeResult` with minimal capabilities
- `prompt()`: spawn subprocess per prompt, stream stdout as `AgentMessageChunk`, emit `TurnEnd` on process exit

## Channel Architecture Fix

### Problem

`wait_for_response()` in `spur-mcp/src/server.rs` holds a `Mutex` on the shared response channel and drops non-matching responses. Parallel delegations silently lose results.

### Solution: oneshot-per-request pattern

```
McpCallbackServer                         Orchestrator
    |                                         |
    |  For each delegation:                   |
    |  1. Create oneshot::channel()           |
    |  2. Bundle Sender into request          |
    |                                         |
    |-- mpsc::Sender<DelegationRequest> ----->|-- mpsc::Receiver<DelegationRequest>
    |      (contains oneshot::Sender)          |
    |                                         |  Spawn task per request:
    |                                         |  1. Acquire semaphore permit
    |                                         |  2. Execute delegation
    |                                         |  3. Send result on oneshot
    |                                         |
    |-- oneshot::Receiver.await <-------------|  (direct, no matching)
```

### Changes

- `DelegationRequest` gains `respond_to: oneshot::Sender<DelegationResult>`
- `DelegationResponse` struct: deleted
- `DelegationChannel.response_tx`: deleted
- `McpCallbackServer.delegation_rx: Mutex<mpsc::Receiver>`: deleted
- `wait_for_response()` method: deleted
- `handle_delegate_parallel`: creates N oneshots, awaits all -- no cross-talk

### Delegation timeout

Each delegation wrapped with `tokio::time::timeout(Duration::from_secs(config.worker_timeout_secs), ...)` to prevent hung workers from leaking semaphore permits.

## Type System Changes

### Replaced by official SDK types

| Spur type | SDK replacement |
|---|---|
| `SessionId(pub String)` | SDK session ID type |
| `AgentCapabilities` | SDK `InitializeResult` |
| `PromptBlock::Text` | SDK content array (text, images, resources) |
| `SessionEvent` (6 variants) | SDK `SessionUpdate` types |
| `AgentStatus` (5 variants) | SDK status types |
| `McpEndpoint` | SDK `McpServerConfig` |
| `parse_session_event()` | SDK handles parsing |

### Kept (spur-specific, in domain/)

- `SpurEvent` -- orchestrator event bus (references SDK session/update types)
- `DelegationResult`, `DelegationStatus` -- delegation outcomes
- `AgentHealth` -- registry health tracking
- `CostTier`, `AgentRole`, `TransportKind` -- config enums

### Moved to spur-pm

- `Issue`, `PrParams`, `IssueFilter`, `IssueSummary`, `IssueUpdate`, `PmEvent`, `PmSource`

## Migration Strategy

### Phase 1: Foundation (no behavior change)

1. Add `agent-client-protocol` dependency to workspace
2. Move PM types to `spur-pm/src/types.rs`, update imports
3. Reorganize `spur-acp` into `connection/` and `domain/` modules
4. Verify: `cargo check` passes at each step

### Phase 2: ACP-First Transport (core change)

1. Define `AgentConnection` trait using SDK types (old `AgentTransport` coexists temporarily)
2. Implement `NativeAcpConnection` wrapping SDK `ClientSideConnection`
3. Implement ACP `Client` trait for spur (fs/read, fs/write, terminal/*, request_permission)
4. Implement `StdioAdapter` producing SDK ACP types
5. Implement `CliWrapAdapter` producing SDK ACP types
6. Switch orchestrator from `AgentTransport` to `AgentConnection`
7. Delete old transport code (`AgentTransport`, `AcpTransport`, `StdioTransport`, `CliWrapTransport`, `parse_session_event`)

### Phase 3: Channel Architecture Fix

1. Add `respond_to: oneshot::Sender<DelegationResult>` to `DelegationRequest`
2. Update `McpCallbackServer` -- delete Mutex, delete wait_for_response
3. Update orchestrator delegation handler -- extract oneshot sender, send result directly
4. Add `tokio::time::timeout` around delegation execution

### Phase 4: Worker Observation via ACP

1. ACP `Client` trait callbacks scoped to worker's worktree path
2. ACP session stream consumed by background task, forwarded to event bus
3. Workers initialized with ACP only: `new_session(None)` -- no MCP
4. Brain retains MCP: `new_session(Some(mcp_servers))`

### Phase 5: Cleanup

1. Update `spur-tui` event processing for SDK `SessionUpdate` types
2. Update `spur-cost` session tracking for SDK session ID type
3. Final verification: `cargo check`, `cargo test`, `cargo clippy`, manual end-to-end test

## Code Impact

| Change | Added | Deleted | Net |
|---|---|---|---|
| NativeAcpConnection + Client impl | ~150 | 0 | +150 |
| StdioAdapter | ~200 | ~290 | -90 |
| CliWrapAdapter | ~120 | ~150 | -30 |
| Old ACP transport + parse code | 0 | ~510 | -510 |
| Old type definitions | 0 | ~200 | -200 |
| Domain module + PM type move | ~30 | ~70 | -40 |
| Oneshot channel fix | ~30 | ~60 | -30 |
| Stream consumer + worker ACP | ~60 | ~30 | +30 |
| **Total** | **~590** | **~1,310** | **~-720** |

## Key Decisions Log

1. **ACP as first citizen** -- non-ACP transports adapt UP to ACP, not the other way around. ACP is the lingua franca, like LSP for language servers.
2. **Official SDK adoption** -- depend on `agent-client-protocol` for both types and runtime. Accept coupling to release cadence for full spec compliance.
3. **MCP for brain only** -- MCP provides LLM-visible delegation tools. Workers use ACP only for observation.
4. **Option D crate boundaries** -- keep one `spur-acp` crate with modules, move PM types to `spur-pm`. No new crates. Module boundaries are future crate split seam lines.
5. **Oneshot channel pattern** -- industry-standard fix for the parallel delegation response-loss bug.
6. **mcpServers in session/new** -- spec compliance fix (was incorrectly in initialize).
