# SPUR ACP-First Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace spur's hand-rolled ACP implementation with the official `agent-client-protocol` SDK, fix the parallel delegation bug, and enable worker observation through ACP client callbacks.

**Architecture:** ACP is the first-class interface. The official SDK's `ClientSideConnection` replaces ~750 lines of custom JSON-RPC code. Non-ACP agents (Stdio, CliWrap) adapt UP to ACP via adapter types. MCP is used only for the brain agent's LLM-visible delegation tools. The delegation channel switches from shared mpsc with ID-matching to oneshot-per-request.

**Tech Stack:** Rust, `agent-client-protocol` ^0.10, `tokio`, `futures`, `async-trait`

**Spec:** `docs/superpowers/specs/2026-04-12-spur-acp-redesign.md`

---

## SDK Constraints (read before implementing)

The official `agent-client-protocol` crate has design choices that affect implementation:

1. **`Client` trait is `#[async_trait(?Send)]`** -- client-side callback futures are NOT Send. `ClientSideConnection` uses `LocalBoxFuture` internally. You may need `tokio::task::spawn_local` with a `LocalSet`, or run connections on a dedicated thread with its own runtime.

2. **Streaming is callback-based** -- `Agent::prompt()` returns `Result<PromptResponse>` (resolves on turn completion), NOT a stream. Session updates arrive via `Client::session_notification()` callback during the prompt. The `AgentConnection` trait bridges this to a stream using an internal `mpsc` channel.

3. **`SessionId` is `Arc<str>`** -- not `String`. Spur's current `SessionId(pub String)` needs migration. The SDK type has `From<String>` and `From<&str>`.

4. **Type names differ from spec draft** -- The spec document used approximate names. Actual SDK names: `InitializeRequest`/`InitializeResponse` (not `InitializeParams`/`InitializeResult`), `NewSessionRequest`/`NewSessionResponse`, `PromptRequest`/`PromptResponse`, `SessionNotification` containing `SessionUpdate` enum.

---

## File Structure

### New files
- `crates/spur-acp/src/connection/mod.rs` -- `AgentConnection` trait definition
- `crates/spur-acp/src/connection/native.rs` -- `NativeAcpConnection` wrapping SDK
- `crates/spur-acp/src/connection/stdio_adapter.rs` -- `StdioAdapter`
- `crates/spur-acp/src/connection/cli_wrap_adapter.rs` -- `CliWrapAdapter`
- `crates/spur-acp/src/domain/mod.rs` -- re-exports
- `crates/spur-acp/src/domain/events.rs` -- `SpurEvent` enum
- `crates/spur-acp/src/domain/delegation.rs` -- `DelegationStatus`, `DelegationResult`
- `crates/spur-pm/src/types.rs` -- PM types moved from spur-acp

### Modified files
- `Cargo.toml` -- add `agent-client-protocol` to workspace deps
- `crates/spur-acp/Cargo.toml` -- add `agent-client-protocol` dep
- `crates/spur-acp/src/lib.rs` -- reorganize re-exports
- `crates/spur-acp/src/types.rs` -- remove migrated types
- `crates/spur-acp/src/config.rs` -- unchanged content, may need import updates
- `crates/spur-pm/src/lib.rs` -- add `pub mod types;`, re-exports
- `crates/spur-pm/src/adapter.rs` -- import from `crate::types` instead of `spur_acp`
- `crates/spur-pm/src/github.rs` -- import from `crate::types` instead of `spur_acp`
- `crates/spur-pm/Cargo.toml` -- remove `spur-acp` dependency (PM types now local)
- `crates/spur-mcp/src/tools.rs` -- oneshot channel, import changes
- `crates/spur-mcp/src/server.rs` -- delete Mutex/wait_for_response, oneshot pattern
- `crates/spur-core/src/orchestrator.rs` -- switch to `AgentConnection`, import PM from spur-pm
- `crates/spur-tui/src/app.rs` -- update for new event types
- `crates/spur-cli/src/main.rs` -- import changes
- `crates/spur-cost/src/tracker.rs` -- session ID type change
- `crates/spur-cost/src/estimator.rs` -- import change
- `crates/spur-worktree/src/manager.rs` -- session ID type change

### Deleted files
- `crates/spur-acp/src/transport.rs` -- entirely replaced by `connection/` module

---

## Task 1: Add SDK Dependency and Verify Build

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/spur-acp/Cargo.toml`

- [ ] **Step 1: Add `agent-client-protocol` to workspace dependencies**

In `Cargo.toml` (workspace root), add under `[workspace.dependencies]`:

```toml
# ACP SDK
agent-client-protocol = "0.10"
```

In `crates/spur-acp/Cargo.toml`, add under `[dependencies]`:

```toml
agent-client-protocol = { workspace = true }
```

- [ ] **Step 2: Verify the dependency resolves and builds**

Run: `cargo check -p spur-acp 2>&1 | tail -5`
Expected: `Finished` with no errors. The crate should download and compile.

- [ ] **Step 3: Verify SDK types are accessible**

Add a temporary test at the bottom of `crates/spur-acp/src/lib.rs`:

```rust
#[cfg(test)]
mod sdk_smoke_test {
    #[test]
    fn sdk_types_accessible() {
        // Verify key SDK types exist and are importable
        let _: agent_client_protocol::SessionId = agent_client_protocol::SessionId::new("test");
        let _version = agent_client_protocol::ProtocolVersion::LATEST;
    }
}
```

Run: `cargo test -p spur-acp sdk_smoke_test`
Expected: PASS

- [ ] **Step 4: Remove the smoke test and commit**

Remove the `sdk_smoke_test` module from `lib.rs`.

```bash
git add Cargo.toml Cargo.lock crates/spur-acp/Cargo.toml
git commit -m "feat: add agent-client-protocol SDK dependency"
```

---

## Task 2: Move PM Types to spur-pm

**Files:**
- Create: `crates/spur-pm/src/types.rs`
- Modify: `crates/spur-pm/src/lib.rs`
- Modify: `crates/spur-pm/src/adapter.rs`
- Modify: `crates/spur-pm/src/github.rs`
- Modify: `crates/spur-pm/Cargo.toml`
- Modify: `crates/spur-acp/src/types.rs`
- Modify: `crates/spur-acp/src/lib.rs`
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Create `crates/spur-pm/src/types.rs` with PM types**

Copy the following types from `crates/spur-acp/src/types.rs` (lines 270-339) into a new file `crates/spur-pm/src/types.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Source of a project management issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PmSource {
    GitHub,
    Linear,
    Plane,
}

/// An issue from a PM tool, normalized to a common format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub source: PmSource,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub status: String,
    pub linked_prs: Vec<String>,
    pub url: String,
}

/// Parameters for creating a pull request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrParams {
    pub title: String,
    pub body: String,
    pub head_branch: String,
    pub base_branch: Option<String>,
    pub repo: Option<String>,
}

/// Filter for listing issues.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueFilter {
    pub labels: Vec<String>,
    pub status: Option<String>,
    pub assignee: Option<String>,
    pub since: Option<DateTime<Utc>>,
}

/// Summary of an issue (for list views).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSummary {
    pub id: String,
    pub source: PmSource,
    pub title: String,
    pub labels: Vec<String>,
    pub status: String,
    pub url: String,
}

/// Update to apply to an issue.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueUpdate {
    pub status: Option<String>,
    pub comment: Option<String>,
    pub add_labels: Vec<String>,
    pub remove_labels: Vec<String>,
}

/// Event from polling a PM tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PmEvent {
    IssueCreated(IssueSummary),
    IssueUpdated(IssueSummary),
}
```

- [ ] **Step 2: Update `crates/spur-pm/src/lib.rs` to export the types module**

```rust
pub mod adapter;
pub mod github;
pub mod types;

pub use adapter::PmAdapter;
pub use github::GitHubAdapter;
pub use types::*;
```

- [ ] **Step 3: Update `crates/spur-pm/src/adapter.rs` to import from local types**

Change line 2 from:
```rust
use spur_acp::{Issue, IssueFilter, IssueSummary, IssueUpdate, PmEvent, PrParams};
```
to:
```rust
use crate::types::{Issue, IssueFilter, IssueSummary, IssueUpdate, PmEvent, PrParams};
```

- [ ] **Step 4: Update `crates/spur-pm/src/github.rs` to import from local types**

Change line 4 from:
```rust
use spur_acp::{Issue, IssueFilter, IssueSummary, IssueUpdate, PmEvent, PmSource, PrParams};
```
to:
```rust
use crate::types::{Issue, IssueFilter, IssueSummary, IssueUpdate, PmEvent, PmSource, PrParams};
```

- [ ] **Step 5: Remove `spur-acp` dependency from `crates/spur-pm/Cargo.toml`**

Remove this line from `[dependencies]`:
```toml
spur-acp = { workspace = true }
```

Add `serde` and `chrono` if not already present (they are -- verify).

- [ ] **Step 6: Remove PM types from `crates/spur-acp/src/types.rs`**

Delete lines 270-339 (everything from `// ─── PM Types ──────` to the end of the file, covering: `PmSource`, `Issue`, `PrParams`, `IssueFilter`, `IssueSummary`, `IssueUpdate`, `PmEvent`).

Also remove `use chrono::{DateTime, Utc};` from line 1 if no remaining types use it.

- [ ] **Step 7: Update `crates/spur-core/src/orchestrator.rs` to import PM types from spur-pm**

The orchestrator uses `Issue` in `fetch_issue_context` and `build_brain_prompt`. Add at the top:

```rust
use spur_pm::Issue;
```

The existing `use spur_acp::types::*;` will no longer bring in PM types (which is correct).

- [ ] **Step 8: Add `spur-pm` to `crates/spur-core/Cargo.toml` if not already present**

Check `crates/spur-core/Cargo.toml` -- it should already have `spur-pm = { workspace = true }`. Verify.

- [ ] **Step 9: Verify build**

Run: `cargo check 2>&1 | tail -5`
Expected: `Finished` with no errors.

- [ ] **Step 10: Commit**

```bash
git add crates/spur-pm/src/types.rs crates/spur-pm/src/lib.rs \
  crates/spur-pm/src/adapter.rs crates/spur-pm/src/github.rs \
  crates/spur-pm/Cargo.toml crates/spur-acp/src/types.rs \
  crates/spur-core/src/orchestrator.rs
git commit -m "refactor: move PM types from spur-acp to spur-pm"
```

---

## Task 3: Reorganize spur-acp into Modules

**Files:**
- Create: `crates/spur-acp/src/domain/mod.rs`
- Create: `crates/spur-acp/src/domain/events.rs`
- Create: `crates/spur-acp/src/domain/delegation.rs`
- Create: `crates/spur-acp/src/connection/mod.rs` (empty trait placeholder)
- Modify: `crates/spur-acp/src/types.rs`
- Modify: `crates/spur-acp/src/lib.rs`

- [ ] **Step 1: Create `crates/spur-acp/src/domain/events.rs`**

Move `SpurEvent` from `types.rs` into this file:

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

use crate::types::{DelegationStatus, SessionId};
use crate::SessionEvent;

/// Events emitted by the orchestrator for TUI/cost-tracker consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SpurEvent {
    // Lifecycle
    BrainSpawned {
        agent: String,
        session: SessionId,
    },
    WorkerSpawned {
        agent: String,
        session: SessionId,
        worktree: PathBuf,
    },
    SessionCompleted {
        session: SessionId,
        success: bool,
    },

    // Streaming
    AgentOutput {
        session: SessionId,
        event: SessionEvent,
    },

    // Orchestration
    DelegationRequested {
        from: SessionId,
        to_agent: String,
        task: String,
    },
    DelegationCompleted {
        worker_session: SessionId,
        status: DelegationStatus,
    },
    ConflictDetected {
        files: Vec<PathBuf>,
    },

    // Rate limits
    RateLimitDetected {
        agent: String,
        retry_after: Option<Duration>,
    },
    BrainFailover {
        from: String,
        to: String,
    },

    // Cost
    CostUpdate {
        session: SessionId,
        agent: String,
        estimated_cost_usd: f64,
    },

    // PM
    IssueReceived {
        source: String,
        id: String,
    },
    PrCreated {
        url: String,
    },
    IssueUpdated {
        source: String,
        id: String,
        status: String,
    },
}
```

- [ ] **Step 2: Create `crates/spur-acp/src/domain/delegation.rs`**

Move `DelegationStatus` and `DelegationResult` from `types.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Result status of a delegation to a worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DelegationStatus {
    Success,
    Failed { error: String },
    Conflict { files: Vec<PathBuf> },
    Timeout,
}

/// Result returned from a completed delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationResult {
    pub status: DelegationStatus,
    /// Git diff of worker's changes.
    pub diff: Option<String>,
    /// Summary of what the worker did.
    pub summary: Option<String>,
    /// Estimated cost in USD.
    pub estimated_cost_usd: f64,
}
```

- [ ] **Step 3: Create `crates/spur-acp/src/domain/mod.rs`**

```rust
pub mod delegation;
pub mod events;

pub use delegation::{DelegationResult, DelegationStatus};
pub use events::SpurEvent;
```

- [ ] **Step 4: Create `crates/spur-acp/src/connection/mod.rs` (placeholder)**

```rust
// AgentConnection trait and implementations will be added in Phase 2.
// For now this module exists to establish the directory structure.
```

- [ ] **Step 5: Remove moved types from `crates/spur-acp/src/types.rs`**

Remove `SpurEvent`, `DelegationStatus`, and `DelegationResult` from `types.rs`. Keep everything else (`SessionId`, `AgentHealth`, `AgentStatus`, `SessionEvent`, `AgentCapabilities`, `McpEndpoint`, `PromptBlock`, `CostTier`, `AgentRole`, `TransportKind`).

- [ ] **Step 6: Update `crates/spur-acp/src/lib.rs`**

```rust
pub mod config;
pub mod connection;
pub mod domain;
pub mod registry;
pub mod transport;
pub mod types;

pub use config::AgentConfig;
pub use registry::AgentRegistry;
pub use transport::{AgentTransport, AcpTransport, CliWrapTransport, StdioTransport};

// Re-export domain types
pub use domain::{DelegationResult, DelegationStatus, SpurEvent};

// Re-export all legacy types for backward compatibility
pub use types::*;
```

- [ ] **Step 7: Verify build**

Run: `cargo check 2>&1 | tail -5`
Expected: `Finished` with no errors. All existing imports should still work via re-exports.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-acp/src/domain/ crates/spur-acp/src/connection/ \
  crates/spur-acp/src/types.rs crates/spur-acp/src/lib.rs
git commit -m "refactor: reorganize spur-acp into domain/ and connection/ modules"
```

---

## Task 4: Define AgentConnection Trait

**Files:**
- Modify: `crates/spur-acp/src/connection/mod.rs`
- Modify: `crates/spur-acp/src/lib.rs`

- [ ] **Step 1: Write the `AgentConnection` trait**

Replace the placeholder content of `crates/spur-acp/src/connection/mod.rs`:

```rust
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

use agent_client_protocol::{
    InitializeRequest, InitializeResponse,
    NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse,
    SessionNotification, McpServer,
};

use crate::types::AgentHealth;

/// Unified interface for communicating with AI coding agents.
///
/// All implementations speak ACP types natively. Native ACP agents use
/// the official SDK; non-ACP agents (Stdio, CliWrap) adapt their I/O
/// into ACP message types at the adapter boundary.
#[async_trait]
pub trait AgentConnection: Send + Sync {
    /// ACP initialize handshake. Returns agent capabilities.
    async fn initialize(
        &mut self,
        request: InitializeRequest,
    ) -> anyhow::Result<InitializeResponse>;

    /// Create a new conversation session.
    /// Pass MCP server configs here (per ACP spec, NOT in initialize).
    async fn new_session(
        &mut self,
        cwd: std::path::PathBuf,
        mcp_servers: Vec<McpServer>,
    ) -> anyhow::Result<NewSessionResponse>;

    /// Send a prompt and receive a stream of session update notifications.
    ///
    /// The stream yields `SessionNotification` events (text chunks, tool calls,
    /// status updates). The stream completes when the agent finishes its turn.
    ///
    /// For NativeAcpConnection: bridged from the SDK's callback-based model
    /// via an internal mpsc channel.
    /// For adapters: synthesized from raw I/O.
    async fn prompt(
        &mut self,
        request: PromptRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>>;

    /// Cancel in-progress work on a session.
    async fn cancel(&mut self, session_id: &str) -> anyhow::Result<()>;

    /// Gracefully shut down the agent process.
    async fn shutdown(&mut self) -> anyhow::Result<()>;

    /// Current health status of the agent.
    fn health(&self) -> AgentHealth;
}
```

- [ ] **Step 2: Re-export from lib.rs**

Add to `crates/spur-acp/src/lib.rs`:

```rust
pub use connection::AgentConnection;
```

- [ ] **Step 3: Verify build**

Run: `cargo check -p spur-acp 2>&1 | tail -5`
Expected: `Finished` with no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/src/connection/mod.rs crates/spur-acp/src/lib.rs
git commit -m "feat: define AgentConnection trait using official ACP SDK types"
```

---

## Task 5: Implement CliWrapAdapter

Starting with the simplest adapter to validate the trait design.

**Files:**
- Create: `crates/spur-acp/src/connection/cli_wrap_adapter.rs`
- Modify: `crates/spur-acp/src/connection/mod.rs`

- [ ] **Step 1: Implement CliWrapAdapter**

Create `crates/spur-acp/src/connection/cli_wrap_adapter.rs`. This adapter spawns a one-shot subprocess per prompt, streams stdout as `AgentMessageChunk` events, and emits a synthetic turn-end on process exit.

The implementation should:
- `initialize()`: verify command exists on PATH, return synthetic `InitializeResponse` with minimal capabilities
- `new_session()`: return synthetic session with generated ID, ignore MCP servers
- `prompt()`: spawn subprocess with task as args, capture stdout line-by-line, emit each line as `SessionNotification` containing `SessionUpdate::AgentMessageChunk`, emit completion on process exit
- `cancel()`: kill the subprocess
- `shutdown()`: no-op (process already exited per prompt)
- `health()`: return stored health status

Reference the existing `CliWrapTransport` in `crates/spur-acp/src/transport.rs` (starting around line 950) for the subprocess spawning pattern, but produce SDK types instead of spur's custom `SessionEvent`.

- [ ] **Step 2: Register in connection/mod.rs**

Add to `crates/spur-acp/src/connection/mod.rs`:

```rust
pub mod cli_wrap_adapter;
pub use cli_wrap_adapter::CliWrapAdapter;
```

- [ ] **Step 3: Verify build**

Run: `cargo check -p spur-acp 2>&1 | tail -5`
Expected: `Finished`

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/src/connection/cli_wrap_adapter.rs \
  crates/spur-acp/src/connection/mod.rs
git commit -m "feat: implement CliWrapAdapter (one-shot CLI -> ACP lifecycle)"
```

---

## Task 6: Implement StdioAdapter

**Files:**
- Create: `crates/spur-acp/src/connection/stdio_adapter.rs`
- Modify: `crates/spur-acp/src/connection/mod.rs`

- [ ] **Step 1: Implement StdioAdapter**

Create `crates/spur-acp/src/connection/stdio_adapter.rs`. This adapter manages a persistent subprocess, translates raw stdin/stdout into ACP message types.

The implementation should:
- `initialize()`: spawn process with piped stdin/stdout, return synthetic `InitializeResponse` with `supports_sessions: false`, `supports_mcp: false`
- `new_session()`: return synthetic session ID (the process IS the session), ignore MCP
- `prompt()`: write delimited prompt to stdin (`--- SPUR PROMPT ---` / `--- END PROMPT ---`), read stdout lines, translate each to `SessionNotification` with `SessionUpdate::AgentMessageChunk`, use 2-second idle timeout as end-of-response heuristic (emit turn-end notification)
- `cancel()`: send SIGTERM to child
- `shutdown()`: close stdin, wait with 3-second timeout, SIGKILL if needed
- `health()`: check if child process is alive

Reference existing `StdioTransport` in `crates/spur-acp/src/transport.rs` (lines 649-948) for the pattern, but produce SDK types.

- [ ] **Step 2: Register in connection/mod.rs**

Add to `crates/spur-acp/src/connection/mod.rs`:

```rust
pub mod stdio_adapter;
pub use stdio_adapter::StdioAdapter;
```

- [ ] **Step 3: Verify build**

Run: `cargo check -p spur-acp 2>&1 | tail -5`
Expected: `Finished`

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/src/connection/stdio_adapter.rs \
  crates/spur-acp/src/connection/mod.rs
git commit -m "feat: implement StdioAdapter (raw stdin/stdout -> ACP messages)"
```

---

## Task 7: Implement NativeAcpConnection

**Files:**
- Create: `crates/spur-acp/src/connection/native.rs`
- Modify: `crates/spur-acp/src/connection/mod.rs`

This is the most complex task. The SDK's `ClientSideConnection` uses a callback-based streaming model that must be bridged to our stream-based `AgentConnection` trait.

- [ ] **Step 1: Implement the ACP Client trait for spur**

The SDK's `Client` trait defines callbacks the agent invokes on spur. Create a `SpurAcpClient` struct that implements `agent_client_protocol::Client`:

```rust
// Inside native.rs

use agent_client_protocol::{
    Client, ReadTextFileRequest, ReadTextFileResponse,
    WriteTextFileRequest, WriteTextFileResponse,
    CreateTerminalRequest, CreateTerminalResponse,
    TerminalOutputRequest, TerminalOutputResponse,
    WaitForTerminalExitRequest, WaitForTerminalExitResponse,
    KillTerminalRequest, KillTerminalResponse,
    ReleaseTerminalRequest, ReleaseTerminalResponse,
    RequestPermissionRequest, RequestPermissionResponse,
    SessionNotification,
};
```

The `SpurAcpClient` should:
- Hold an `mpsc::UnboundedSender<SessionNotification>` for bridging session updates to the stream
- Hold a `PathBuf` for the working directory (worktree path) to scope filesystem ops
- Implement `session_notification()`: forward the notification to the mpsc sender
- Implement `read_text_file()`: read from `cwd.join(request.path)`, return content
- Implement `write_text_file()`: write to `cwd.join(request.path)`
- Implement `request_permission()`: auto-approve (return `Allow` outcome)
- Terminal methods: spawn commands via `tokio::process::Command` in the cwd context, track terminal processes by ID

- [ ] **Step 2: Implement NativeAcpConnection struct**

```rust
pub struct NativeAcpConnection {
    agent_name: String,
    command: String,
    args: Vec<String>,
    // SDK connection (set after initialize)
    connection: Option<ClientSideConnection>,
    // Channel for receiving session notifications (stream bridge)
    notification_rx: Option<mpsc::UnboundedReceiver<SessionNotification>>,
    health_status: AgentHealth,
}
```

- [ ] **Step 3: Implement AgentConnection for NativeAcpConnection**

- `initialize()`: spawn agent subprocess, create `SpurAcpClient` with mpsc channel, construct `ClientSideConnection::new(client, stdin, stdout, spawn)`, spawn the I/O future, call `connection.initialize(request)`, store connection
- `new_session()`: call `connection.new_session(NewSessionRequest { cwd, mcp_servers, .. })`
- `prompt()`: take the `notification_rx`, spawn a task that calls `connection.prompt(request)` (blocks until turn end), return the receiver wrapped as a Stream via `futures::stream::unfold` or `tokio_stream::wrappers::UnboundedReceiverStream`
- `cancel()`: call `connection.cancel(...)`
- `shutdown()`: drop the connection, wait for child process to exit
- `health()`: return stored health status

Note on `!Send` constraint: `ClientSideConnection` is `Send + Sync` but its internal futures use `LocalBoxFuture`. You may need to run the I/O future on a `tokio::task::LocalSet` or wrap the connection in a dedicated thread. Test this during implementation -- if `tokio::spawn` works, use it; if not, use `spawn_local`.

- [ ] **Step 4: Register in connection/mod.rs**

Add to `crates/spur-acp/src/connection/mod.rs`:

```rust
pub mod native;
pub use native::NativeAcpConnection;
```

- [ ] **Step 5: Verify build**

Run: `cargo check -p spur-acp 2>&1 | tail -5`
Expected: `Finished`

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs \
  crates/spur-acp/src/connection/mod.rs
git commit -m "feat: implement NativeAcpConnection wrapping official ACP SDK"
```

---

## Task 8: Oneshot Channel Fix (spur-mcp)

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs`
- Modify: `crates/spur-mcp/src/server.rs`
- Modify: `crates/spur-mcp/src/lib.rs`

- [ ] **Step 1: Update DelegationRequest with oneshot sender**

In `crates/spur-mcp/src/tools.rs`, add the oneshot sender and remove `DelegationResponse`:

```rust
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spur_acp::DelegationResult;
use tokio::sync::oneshot;

/// A delegation request sent from the MCP server to the orchestrator.
#[derive(Debug)]
pub struct DelegationRequest {
    pub id: String,
    pub agent: String,
    pub task: String,
    pub context_files: Vec<String>,
    /// Channel to send the result back to the MCP server.
    pub respond_to: oneshot::Sender<DelegationResult>,
}

// DELETE: DelegationResponse struct entirely

/// Channel the orchestrator holds to receive delegation requests.
pub struct DelegationChannel {
    pub request_rx: tokio::sync::mpsc::Receiver<DelegationRequest>,
    // DELETE: response_tx field
}
```

Note: `DelegationRequest` can no longer derive `Clone`, `Serialize`, `Deserialize` because `oneshot::Sender` doesn't implement them. Remove those derives.

- [ ] **Step 2: Update `crates/spur-mcp/src/lib.rs` exports**

Remove `DelegationResponse` from the pub use:

```rust
pub use tools::{
    DelegationChannel, DelegationRequest, ToolDefinition, tools_list,
};
```

- [ ] **Step 3: Update McpCallbackServer in `crates/spur-mcp/src/server.rs`**

Remove from the struct:
- `delegation_rx: Mutex<mpsc::Receiver<DelegationResponse>>` field

Remove the method:
- `wait_for_response()` entirely

Update `McpCallbackServer::new()`:
- Remove the response channel creation (`let (resp_tx, resp_rx) = mpsc::channel(32);`)
- Remove `delegation_rx` from the struct initialization
- Remove `response_tx` from the `DelegationChannel`

Update `handle_delegate_to_worker()`:
- Create `let (tx, rx) = oneshot::channel();`
- Include `respond_to: tx` in the `DelegationRequest`
- Replace `self.wait_for_response(&request_id).await` with `rx.await.map_err(|_| anyhow::anyhow!("Delegation cancelled"))`

Update `handle_delegate_parallel()`:
- Create N oneshot channels, one per task
- Collect all receivers in a `Vec`
- Await each receiver instead of calling `wait_for_response`

Update remaining PM tool handlers (`handle_get_issue`, `handle_update_issue`, `handle_create_pr`, `handle_get_session_cost`):
- Same oneshot pattern: create channel, bundle sender in request, await receiver

Update `handle_report_progress()`:
- This is fire-and-forget. Create a oneshot but don't await it (or use a different request type). Simplest: still create oneshot but drop the receiver immediately.

- [ ] **Step 4: Verify build**

Run: `cargo check -p spur-mcp 2>&1 | tail -5`
Expected: `Finished`

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/tools.rs crates/spur-mcp/src/server.rs \
  crates/spur-mcp/src/lib.rs
git commit -m "fix: replace shared response channel with oneshot-per-request pattern

Fixes parallel delegation bug where non-matching responses were
silently dropped, causing delegate_parallel to lose results."
```

---

## Task 9: Update Orchestrator Delegation Handler

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Update `handle_delegations` for oneshot pattern**

In `handle_delegations()` (around line 519), update the spawned task to extract and use the oneshot sender:

```rust
tokio::spawn(async move {
    let _permit = match semaphore.acquire().await {
        Ok(permit) => permit,
        Err(_) => {
            error!("Semaphore closed — aborting delegation");
            return;
        }
    };

    // Add timeout around delegation execution
    let result = match tokio::time::timeout(
        std::time::Duration::from_secs(300),
        Self::execute_delegation(
            request.agent.clone(),
            request.task.clone(),
            request.context_files.clone(),
            repo_root,
            agent_configs,
            event_tx,
        ),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => DelegationResult {
            status: DelegationStatus::Timeout,
            diff: None,
            summary: None,
            estimated_cost_usd: 0.0,
        },
    };

    // Send result directly to the caller via oneshot
    let _ = request.respond_to.send(result);
});
```

Note: `request` can no longer be cloned (oneshot::Sender is not Clone). Extract the fields you need (`agent`, `task`, `context_files`) before moving `request.respond_to` into the closure.

- [ ] **Step 2: Update `execute_delegation` signature**

The function no longer needs `request.id` (that was only used for response matching). Update the signature to take individual fields instead of the full `DelegationRequest`:

```rust
async fn execute_delegation(
    agent: String,
    task: String,
    context_files: Vec<String>,
    repo_root: PathBuf,
    agent_configs: Vec<spur_acp::config::AgentConfig>,
    event_tx: broadcast::Sender<SpurEvent>,
) -> DelegationResult {
```

Update the call site in `handle_delegations` to pass individual fields.

- [ ] **Step 3: Remove `DelegationResponse` imports**

Remove any `use spur_mcp::DelegationResponse` imports from `orchestrator.rs`. The `DelegationResponse` struct no longer exists.

Update `DelegationChannel` usage -- it no longer has `response_tx`, so remove `channel.response_tx.clone()`.

- [ ] **Step 4: Verify build**

Run: `cargo check 2>&1 | tail -5`
Expected: `Finished`

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat: update delegation handler for oneshot channels with timeout"
```

---

## Task 10: Switch Orchestrator to AgentConnection

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`
- Modify: `crates/spur-core/Cargo.toml`

- [ ] **Step 1: Add `agent-client-protocol` to spur-core dependencies**

In `crates/spur-core/Cargo.toml`:

```toml
agent-client-protocol = { workspace = true }
```

- [ ] **Step 2: Replace `create_transport` with `create_connection`**

Replace the `create_transport` method (around line 430) with:

```rust
fn create_connection(
    &self,
    config: &spur_acp::config::AgentConfig,
) -> Box<dyn AgentConnection> {
    match config.transport {
        TransportKind::Acp => Box::new(NativeAcpConnection::new(
            config.name.clone(),
            config.command.clone(),
            config.args.clone(),
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

- [ ] **Step 3: Update `run_adhoc` to use AgentConnection**

Update imports at the top of the file:

```rust
use spur_acp::connection::{AgentConnection, NativeAcpConnection, StdioAdapter, CliWrapAdapter};
use agent_client_protocol::{
    InitializeRequest, NewSessionRequest, PromptRequest,
    ContentBlock, TextContent, McpServer, McpServerStdio,
    ProtocolVersion, ClientCapabilities, Implementation,
};
```

Update `run_adhoc()`:
- Replace `self.create_transport(&brain_config)` with `self.create_connection(&brain_config)`
- Replace `transport.initialize(Some(mcp_endpoint))` with building an `InitializeRequest` and calling `connection.initialize(request)`
- Replace `transport.create_session()` with building a `NewSessionRequest` with `mcp_servers` and calling `connection.new_session(cwd, mcp_servers)`
- Replace `transport.prompt(session, prompt)` with building a `PromptRequest` using `ContentBlock::Text(TextContent { text, .. })` and calling `connection.prompt(request)`
- The stream processing loop should work similarly but now iterates over `SessionNotification` instead of `SessionEvent`

- [ ] **Step 4: Update `exec_direct` similarly**

Same pattern as `run_adhoc` but simpler (no MCP servers, no delegation channel).

- [ ] **Step 5: Update `execute_delegation` similarly**

Replace transport usage with connection. Workers get `new_session(worktree_path, vec![])` -- empty MCP servers (no MCP for workers).

- [ ] **Step 6: Update `check_agents`**

Replace transport usage with connection for health checks.

- [ ] **Step 7: Verify build**

Run: `cargo check 2>&1 | tail -5`
Expected: `Finished`

- [ ] **Step 8: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs crates/spur-core/Cargo.toml
git commit -m "feat: switch orchestrator from AgentTransport to AgentConnection"
```

---

## Task 11: Update TUI for New Event Types

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/Cargo.toml`

- [ ] **Step 1: Add SDK dependency to spur-tui**

In `crates/spur-tui/Cargo.toml`:

```toml
agent-client-protocol = { workspace = true }
```

- [ ] **Step 2: Update `process_event` in app.rs**

The `SpurEvent::AgentOutput` event now carries `SessionNotification` (SDK type) instead of `SessionEvent` (spur type). Update the match arm:

```rust
SpurEvent::AgentOutput { session, event: notification } => {
    let prefix = self.prefix_for_session(&session.0);
    match &notification.update {
        SessionUpdate::AgentMessageChunk(chunk) => {
            let text = &chunk.text;
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                self.push_log(&prefix, trimmed.to_string());
            }
        }
        SessionUpdate::ToolCall(tc) => {
            self.push_log(&prefix, format!("Tool call: {}", tc.name));
        }
        // ... handle other variants
        _ => {}
    }
}
```

Update imports accordingly. The session ID type changes from `SessionId(String)` to the SDK's `SessionId(Arc<str>)`.

- [ ] **Step 3: Update session ID usage**

The `session_agent` HashMap key and `prefix_for_session` parameter need to work with the SDK's `SessionId`. Since SDK `SessionId` derefs to `str`, use `.as_ref()` or `.to_string()` where needed.

- [ ] **Step 4: Verify build**

Run: `cargo check -p spur-tui 2>&1 | tail -5`
Expected: `Finished`

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/Cargo.toml
git commit -m "feat: update TUI for ACP SDK event types"
```

---

## Task 12: Update spur-cost and spur-worktree for New Session ID

**Files:**
- Modify: `crates/spur-cost/src/tracker.rs`
- Modify: `crates/spur-cost/src/estimator.rs`
- Modify: `crates/spur-worktree/src/manager.rs`
- Modify: `crates/spur-cli/src/main.rs`

- [ ] **Step 1: Update spur-cost session ID usage**

In `crates/spur-cost/src/tracker.rs`, the SDK `SessionId` is `Arc<str>`. Update usage to call `.as_ref()` or similar where a `&str` is needed for SQLite queries.

In `crates/spur-cost/src/estimator.rs`, `CostTier` stays in spur-acp -- no change needed unless imports shifted.

- [ ] **Step 2: Update spur-worktree session ID usage**

In `crates/spur-worktree/src/manager.rs` line 2: update the `SessionId` import path if needed. The SDK type has `Display` impl so string formatting should work.

- [ ] **Step 3: Update spur-cli imports**

In `crates/spur-cli/src/main.rs`:
- Line 6: `SessionId` may come from `agent_client_protocol` instead of `spur_acp`
- Lines 372-407: `AgentHealth` usage stays the same (still in `spur_acp`)

- [ ] **Step 4: Verify full workspace build**

Run: `cargo check 2>&1 | tail -5`
Expected: `Finished` -- entire workspace compiles

- [ ] **Step 5: Commit**

```bash
git add crates/spur-cost/ crates/spur-worktree/ crates/spur-cli/
git commit -m "refactor: update cost/worktree/cli for ACP SDK session ID type"
```

---

## Task 13: Delete Old Transport Code

**Files:**
- Delete: `crates/spur-acp/src/transport.rs`
- Modify: `crates/spur-acp/src/lib.rs`
- Modify: `crates/spur-acp/src/types.rs`

- [ ] **Step 1: Remove transport module from lib.rs**

Remove from `crates/spur-acp/src/lib.rs`:
```rust
pub mod transport;
pub use transport::{AgentTransport, AcpTransport, CliWrapTransport, StdioTransport};
```

- [ ] **Step 2: Delete the transport file**

```bash
rm crates/spur-acp/src/transport.rs
```

- [ ] **Step 3: Remove replaced types from types.rs**

Remove from `crates/spur-acp/src/types.rs` the types now provided by the SDK:
- `SessionId` struct and impls (replaced by SDK `SessionId`)
- `AgentStatus` enum (replaced by SDK status types)
- `SessionEvent` enum (replaced by SDK `SessionNotification`/`SessionUpdate`)
- `AgentCapabilities` struct (replaced by SDK `InitializeResponse`)
- `McpEndpoint` struct (replaced by SDK `McpServer`)
- `PromptBlock` enum (replaced by SDK `ContentBlock`)

Keep: `AgentHealth`, `CostTier`, `AgentRole`, `TransportKind`

- [ ] **Step 4: Update lib.rs re-exports**

Update `crates/spur-acp/src/lib.rs` to re-export SDK types for convenience:

```rust
pub mod config;
pub mod connection;
pub mod domain;
pub mod registry;
pub mod types;

pub use config::AgentConfig;
pub use connection::{AgentConnection, NativeAcpConnection, StdioAdapter, CliWrapAdapter};
pub use domain::{DelegationResult, DelegationStatus, SpurEvent};
pub use registry::AgentRegistry;
pub use types::*;

// Re-export commonly used SDK types
pub use agent_client_protocol::SessionId;
```

- [ ] **Step 5: Fix any remaining import errors**

Run: `cargo check 2>&1`

Fix any remaining references to deleted types across the workspace. Common fixes:
- `spur_acp::SessionEvent` -> `agent_client_protocol::SessionNotification` or `SessionUpdate`
- `spur_acp::PromptBlock` -> `agent_client_protocol::ContentBlock`
- `spur_acp::AgentCapabilities` -> `agent_client_protocol::InitializeResponse`

- [ ] **Step 6: Verify clean build**

Run: `cargo check 2>&1 | tail -5`
Expected: `Finished`

Run: `cargo clippy 2>&1 | tail -10`
Expected: No errors (warnings OK for now)

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat: delete old transport code, complete ACP SDK migration

Removes ~950 lines of hand-rolled ACP implementation (AgentTransport
trait, AcpTransport, StdioTransport, CliWrapTransport, parse_session_event).
All agent communication now goes through AgentConnection backed by the
official agent-client-protocol SDK."
```

---

## Task 14: Final Verification

**Files:** None (verification only)

- [ ] **Step 1: Full workspace build**

Run: `cargo build 2>&1 | tail -5`
Expected: `Finished`

- [ ] **Step 2: Run all tests**

Run: `cargo test 2>&1 | tail -10`
Expected: All tests pass

- [ ] **Step 3: Run clippy**

Run: `cargo clippy -- -D warnings 2>&1 | tail -10`
Expected: No errors

- [ ] **Step 4: Verify binary works**

Run: `cargo run -p spur-cli -- --help`
Expected: CLI help output shows all commands

Run: `cargo run -p spur-cli -- agents`
Expected: Lists agents (or "No agents registered")

- [ ] **Step 5: Verify line count reduction**

Run: `git diff --stat HEAD~14..HEAD` (adjust commit count)
Expected: Net deletion of ~700+ lines

- [ ] **Step 6: Final commit (if any fixups needed)**

```bash
git add -A
git commit -m "chore: final cleanup after ACP-first redesign"
```

---

## Summary

| Task | Phase | Description | Risk |
|------|-------|-------------|------|
| 1 | Foundation | Add SDK dependency | Low |
| 2 | Foundation | Move PM types to spur-pm | Low |
| 3 | Foundation | Reorganize spur-acp modules | Low |
| 4 | ACP-First | Define AgentConnection trait | Medium |
| 5 | ACP-First | Implement CliWrapAdapter | Low |
| 6 | ACP-First | Implement StdioAdapter | Medium |
| 7 | ACP-First | Implement NativeAcpConnection | **High** (SDK integration, !Send) |
| 8 | Channel Fix | Oneshot channel in spur-mcp | Medium |
| 9 | Channel Fix | Update orchestrator delegation | Medium |
| 10 | Switch | Orchestrator uses AgentConnection | **High** (largest change) |
| 11 | Cleanup | Update TUI for SDK types | Medium |
| 12 | Cleanup | Update cost/worktree/cli | Low |
| 13 | Cleanup | Delete old transport code | Medium |
| 14 | Verification | Full build + test | Low |
