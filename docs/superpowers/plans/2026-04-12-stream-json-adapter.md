# StreamJsonAdapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `StreamJsonAdapter` that enables Claude Code as a brain agent via its bidirectional stream-json protocol.

**Architecture:** A new adapter implementing the `AgentConnection` trait. It spawns Claude Code with `--input-format stream-json --output-format stream-json`, reads structured NDJSON from stdout, maps each event to ACP SDK `SessionNotification` types, and writes JSON user messages to stdin for multi-turn. Uses a persistent background reader task with per-turn channel switching (same pattern as `NativeAcpConnection`).

**Tech Stack:** Rust, tokio (async), serde_json (parsing), agent-client-protocol SDK (ACP types)

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/spur-acp/src/protocol/mod.rs` | Module declaration for protocol submodule |
| `crates/spur-acp/src/protocol/claude_events.rs` | Serde types for Claude stream-json events + parser + mapper to ACP types |
| `crates/spur-acp/src/connection/stream_json_adapter.rs` | `AgentConnection` impl with background reader and channel-per-turn |
| `crates/spur-acp/src/connection/mod.rs` | Add module export (existing file) |
| `crates/spur-acp/src/lib.rs` | Add protocol module + re-export (existing file) |
| `crates/spur-acp/src/types.rs` | Add `StreamJson` variant to `TransportKind` (existing file) |
| `crates/spur-core/src/orchestrator.rs` | Add `StreamJson` dispatch arm (existing file) |

---

### Task 1: Add `StreamJson` to TransportKind enum

**Files:**
- Modify: `crates/spur-acp/src/types.rs:113-119`

- [ ] **Step 1: Add the variant**

In `crates/spur-acp/src/types.rs`, add `StreamJson` to the `TransportKind` enum:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportKind {
    Acp,
    Stdio,
    CliWrap,
    StreamJson,
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p spur-acp`
Expected: compiles. The orchestrator will have a non-exhaustive match warning — that's fine, we fix it in Task 6.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-acp/src/types.rs
git commit -m "feat(acp): add StreamJson variant to TransportKind"
```

---

### Task 2: Create protocol module with Claude event types

**Files:**
- Create: `crates/spur-acp/src/protocol/mod.rs`
- Create: `crates/spur-acp/src/protocol/claude_events.rs`
- Modify: `crates/spur-acp/src/lib.rs`

- [ ] **Step 1: Create the protocol module declaration**

Create `crates/spur-acp/src/protocol/mod.rs`:

```rust
pub mod claude_events;
```

- [ ] **Step 2: Create the Claude event types and parser**

Create `crates/spur-acp/src/protocol/claude_events.rs`:

```rust
//! Serde types for Claude Code's stream-json protocol.
//!
//! Claude Code emits newline-delimited JSON on stdout when invoked with
//! `--output-format stream-json`. This module defines the event types,
//! a parser, and a mapper to ACP `SessionNotification` types.

use agent_client_protocol::{
    ContentBlock, ContentChunk, SessionId, SessionNotification, SessionUpdate, TextContent,
    ToolCall as AcpToolCall, ToolCallId, ToolCallUpdate as AcpToolCallUpdate,
    ToolCallUpdateFields,
};
use serde::Deserialize;

// ─── Claude stream-json event types ────────────────────────────────────

/// Top-level event from Claude Code's stream-json stdout.
/// Each line is one JSON object matching one of these variants.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClaudeEvent {
    #[serde(rename = "system")]
    System(SystemEvent),
    #[serde(rename = "assistant")]
    Assistant(AssistantEvent),
    #[serde(rename = "result")]
    Result(ResultEvent),
}

/// Emitted once at startup. Contains session metadata.
#[derive(Debug, Deserialize)]
pub struct SystemEvent {
    pub subtype: String,
    pub session_id: String,
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    #[serde(rename = "permissionMode")]
    pub permission_mode: Option<String>,
}

/// An assistant response containing one or more content blocks.
#[derive(Debug, Deserialize)]
pub struct AssistantEvent {
    pub message: AssistantMessage,
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
pub struct AssistantMessage {
    pub content: Vec<AssistantContentBlock>,
}

/// Content block types within an assistant message.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum AssistantContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: serde_json::Value,
    },
}

/// End-of-turn event with cost and usage data.
#[derive(Debug, Deserialize)]
pub struct ResultEvent {
    pub subtype: String,
    pub is_error: Option<bool>,
    pub duration_ms: Option<u64>,
    pub result: Option<String>,
    pub total_cost_usd: Option<f64>,
    pub session_id: String,
}

// ─── Stdin message type ─────────────────────────────────────────────────

/// User message written to Claude's stdin in stream-json input mode.
#[derive(Debug, serde::Serialize)]
pub struct UserMessage<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'a str,
    pub content: &'a str,
}

// ─── Parsing ────────────────────────────────────────────────────────────

/// Parse a single stdout line into a ClaudeEvent.
/// Returns Err for unknown or malformed event types.
pub fn parse_event(line: &str) -> Result<ClaudeEvent, serde_json::Error> {
    serde_json::from_str(line)
}

// ─── Mapping to ACP types ───────────────────────────────────────────────

/// Map a ClaudeEvent to zero or more ACP SessionNotifications.
///
/// System and Result events are handled internally by the adapter
/// and produce no notifications.
pub fn map_to_notifications(
    event: &ClaudeEvent,
    session_id: &SessionId,
) -> Vec<SessionNotification> {
    match event {
        ClaudeEvent::Assistant(evt) => evt
            .message
            .content
            .iter()
            .filter_map(|block| {
                let update = match block {
                    AssistantContentBlock::Text { text } => {
                        let chunk =
                            ContentChunk::new(ContentBlock::Text(TextContent::new(text.clone())));
                        SessionUpdate::AgentMessageChunk(chunk)
                    }
                    AssistantContentBlock::Thinking { thinking } => {
                        let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(
                            thinking.clone(),
                        )));
                        SessionUpdate::AgentThoughtChunk(chunk)
                    }
                    AssistantContentBlock::ToolUse { id, name, input } => {
                        let mut tc = AcpToolCall::new(
                            ToolCallId::new(id.clone()),
                            name.clone(),
                        );
                        tc.raw_input = Some(input.clone());
                        SessionUpdate::ToolCall(tc)
                    }
                    AssistantContentBlock::ToolResult {
                        tool_use_id,
                        content,
                    } => {
                        let fields = ToolCallUpdateFields {
                            raw_output: Some(content.clone()),
                            ..ToolCallUpdateFields::new()
                        };
                        let tcu = AcpToolCallUpdate::new(
                            ToolCallId::new(tool_use_id.clone()),
                            fields,
                        );
                        SessionUpdate::ToolCallUpdate(tcu)
                    }
                };
                Some(SessionNotification::new(session_id.clone(), update))
            })
            .collect(),
        ClaudeEvent::System(_) | ClaudeEvent::Result(_) => vec![],
    }
}
```

- [ ] **Step 3: Add protocol module to lib.rs**

In `crates/spur-acp/src/lib.rs`, add the module declaration. Change:

```rust
pub mod config;
pub mod connection;
pub mod domain;
pub mod registry;
pub mod types;
```

to:

```rust
pub mod config;
pub mod connection;
pub mod domain;
pub mod protocol;
pub mod registry;
pub mod types;
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p spur-acp`
Expected: compiles clean. The `map_to_notifications` function may show dead-code warnings since nothing calls it yet — that's fine.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/protocol/mod.rs crates/spur-acp/src/protocol/claude_events.rs crates/spur-acp/src/lib.rs
git commit -m "feat(acp): add Claude stream-json event parser and ACP type mapper"
```

---

### Task 3: Create StreamJsonAdapter

**Files:**
- Create: `crates/spur-acp/src/connection/stream_json_adapter.rs`
- Modify: `crates/spur-acp/src/connection/mod.rs`
- Modify: `crates/spur-acp/src/lib.rs`

- [ ] **Step 1: Create the adapter file**

Create `crates/spur-acp/src/connection/stream_json_adapter.rs`:

```rust
//! `StreamJsonAdapter` — connects to Claude Code via bidirectional stream-json.
//!
//! # Lifecycle mapping
//!
//! | `AgentConnection` method | Behaviour |
//! |---|---|
//! | `initialize()` | Spawn claude with stream-json flags. Read first stdout line (system/init). Spawn persistent background reader task. |
//! | `new_session()` | Return session_id from init event. The process IS the session. |
//! | `prompt()` | Create per-turn channel. Send sender to background reader. Write JSON user message to stdin. Return receiver as Stream. |
//! | `cancel()` | Kill child process. |
//! | `shutdown()` | Close stdin, wait 3s, SIGKILL. Drop sender channel. |
//! | `health()` | Check child alive, return cached AgentHealth. |

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::unfold;
use futures::Stream;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::mpsc;
use uuid::Uuid;

use agent_client_protocol::{
    ContentBlock, ContentChunk, InitializeRequest, InitializeResponse, McpServer,
    NewSessionResponse, PromptRequest, ProtocolVersion, SessionId, SessionNotification,
    SessionUpdate, TextContent,
};

use crate::connection::AgentConnection;
use crate::protocol::claude_events::{
    parse_event, map_to_notifications, ClaudeEvent, UserMessage,
};
use crate::types::AgentHealth;

/// Connects to Claude Code via its bidirectional stream-json protocol.
///
/// Spawns a persistent `claude` process with `--input-format stream-json`
/// and `--output-format stream-json`. A background task reads stdout
/// continuously, mapping events to ACP `SessionNotification` types.
///
/// Multi-turn is handled by creating a fresh `mpsc` channel per `prompt()`
/// call. The background reader sends to the current channel until a
/// `result` event arrives, then drops the sender (ending the stream)
/// and waits for the next channel from the next `prompt()` call.
pub struct StreamJsonAdapter {
    agent_name: String,
    command: String,
    extra_args: Vec<String>,
    child: Option<Child>,
    stdin: Option<BufWriter<ChildStdin>>,
    /// Send new per-turn senders to the background reader.
    sender_tx: Option<mpsc::Sender<mpsc::Sender<SessionNotification>>>,
    /// Session ID extracted from the system/init event.
    session_id: Option<SessionId>,
    /// Model name from the system/init event.
    model: Option<String>,
    /// Cumulative cost across all turns.
    total_cost: Arc<Mutex<f64>>,
    /// Cached health status.
    health_status: AgentHealth,
}

impl StreamJsonAdapter {
    pub fn new(
        agent_name: impl Into<String>,
        command: impl Into<String>,
        extra_args: Vec<String>,
    ) -> Self {
        Self {
            agent_name: agent_name.into(),
            command: command.into(),
            extra_args,
            child: None,
            stdin: None,
            sender_tx: None,
            session_id: None,
            model: None,
            total_cost: Arc::new(Mutex::new(0.0)),
            health_status: AgentHealth::Unknown,
        }
    }

    /// The cumulative cost across all turns so far.
    pub fn total_cost(&self) -> f64 {
        *self.total_cost.lock().unwrap()
    }
}

#[async_trait]
impl AgentConnection for StreamJsonAdapter {
    // ─── initialize ─────────────────────────────────────────────────────

    async fn initialize(
        &mut self,
        _request: InitializeRequest,
    ) -> anyhow::Result<InitializeResponse> {
        tracing::debug!(
            agent = %self.agent_name,
            command = %self.command,
            "StreamJsonAdapter: spawning Claude Code process"
        );

        let mut child = tokio::process::Command::new(&self.command)
            .args(&self.extra_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                self.health_status =
                    AgentHealth::Error(format!("Failed to spawn process: {e}"));
                anyhow::anyhow!(
                    "StreamJsonAdapter '{}': failed to spawn '{}': {e}",
                    self.agent_name,
                    self.command
                )
            })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            anyhow::anyhow!("StreamJsonAdapter '{}': failed to capture stdin", self.agent_name)
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            anyhow::anyhow!("StreamJsonAdapter '{}': failed to capture stdout", self.agent_name)
        })?;

        self.stdin = Some(BufWriter::new(stdin));
        self.child = Some(child);

        // Read the first line — expect system/init event.
        let mut reader = BufReader::new(stdout);
        let mut first_line = String::new();
        reader.read_line(&mut first_line).await.map_err(|e| {
            anyhow::anyhow!("StreamJsonAdapter '{}': failed to read init event: {e}", self.agent_name)
        })?;

        let init_event = parse_event(first_line.trim()).map_err(|e| {
            anyhow::anyhow!(
                "StreamJsonAdapter '{}': failed to parse init event: {e}\nLine: {}",
                self.agent_name,
                first_line.trim()
            )
        })?;

        let (session_id_str, model) = match init_event {
            ClaudeEvent::System(ref sys) if sys.subtype == "init" => {
                (sys.session_id.clone(), sys.model.clone())
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "StreamJsonAdapter '{}': expected system/init as first event, got: {}",
                    self.agent_name,
                    first_line.trim()
                ));
            }
        };

        let session_id = SessionId::new(session_id_str);
        self.session_id = Some(session_id.clone());
        self.model = model;

        tracing::debug!(
            agent = %self.agent_name,
            session = %session_id,
            model = ?self.model,
            "StreamJsonAdapter: initialized, spawning background reader"
        );

        // Spawn the persistent background reader task.
        let (sender_tx, sender_rx) = mpsc::channel::<mpsc::Sender<SessionNotification>>(4);
        self.sender_tx = Some(sender_tx);

        let agent_name = self.agent_name.clone();
        let cost = Arc::clone(&self.total_cost);
        let bg_session_id = session_id.clone();

        tokio::spawn(async move {
            reader_loop(reader, sender_rx, bg_session_id, cost, agent_name).await;
        });

        self.health_status = AgentHealth::Ready;
        Ok(InitializeResponse::new(ProtocolVersion::LATEST))
    }

    // ─── new_session ────────────────────────────────────────────────────

    async fn new_session(
        &mut self,
        _cwd: PathBuf,
        _mcp_servers: Vec<McpServer>,
    ) -> anyhow::Result<NewSessionResponse> {
        // The process IS the session. Return the ID from the init event.
        let session_id = self.session_id.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "StreamJsonAdapter '{}': new_session called before initialize",
                self.agent_name
            )
        })?;

        tracing::debug!(
            agent = %self.agent_name,
            session = %session_id,
            "StreamJsonAdapter: returning existing session (process is the session)"
        );

        Ok(NewSessionResponse::new(session_id))
    }

    // ─── prompt ─────────────────────────────────────────────────────────

    async fn prompt(
        &mut self,
        request: PromptRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        // Extract text from content blocks.
        let prompt_text: String = request
            .prompt
            .iter()
            .filter_map(|block| {
                if let ContentBlock::Text(tc) = block {
                    Some(tc.text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        let session_id = request.session_id.clone();

        tracing::debug!(
            agent = %self.agent_name,
            session = %session_id,
            prompt_len = prompt_text.len(),
            "StreamJsonAdapter: sending prompt"
        );

        // Create a per-turn channel.
        let (notif_tx, notif_rx) = mpsc::channel::<SessionNotification>(64);

        // Send the sender to the background reader.
        let sender_tx = self.sender_tx.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "StreamJsonAdapter '{}': not initialized (no sender channel)",
                self.agent_name
            )
        })?;
        sender_tx.send(notif_tx).await.map_err(|_| {
            anyhow::anyhow!(
                "StreamJsonAdapter '{}': background reader died",
                self.agent_name
            )
        })?;

        // Write user message to stdin.
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            anyhow::anyhow!(
                "StreamJsonAdapter '{}': stdin not available",
                self.agent_name
            )
        })?;

        let user_msg = UserMessage {
            msg_type: "user",
            content: &prompt_text,
        };
        let json = serde_json::to_string(&user_msg)?;
        stdin.write_all(json.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;

        // Return the receiver as a Stream.
        let stream = unfold(notif_rx, |mut rx| async move {
            rx.recv().await.map(|notif| (notif, rx))
        });

        Ok(Box::pin(stream))
    }

    // ─── cancel ─────────────────────────────────────────────────────────

    async fn cancel(&mut self, session_id: &str) -> anyhow::Result<()> {
        if let Some(ref mut child) = self.child {
            tracing::debug!(
                agent = %self.agent_name,
                session = %session_id,
                "StreamJsonAdapter: killing subprocess on cancel"
            );
            child.kill().await.map_err(|e| {
                anyhow::anyhow!(
                    "StreamJsonAdapter '{}': failed to kill subprocess: {e}",
                    self.agent_name
                )
            })?;
        }
        self.child = None;
        Ok(())
    }

    // ─── shutdown ───────────────────────────────────────────────────────

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        tracing::debug!(agent = %self.agent_name, "StreamJsonAdapter: shutting down");

        // Drop the sender channel so the background reader exits.
        self.sender_tx.take();

        // Close stdin to signal EOF.
        self.stdin.take();

        if let Some(ref mut child) = self.child {
            match tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await {
                Ok(Ok(status)) => {
                    tracing::debug!(
                        agent = %self.agent_name,
                        status = %status,
                        "StreamJsonAdapter: subprocess exited cleanly"
                    );
                }
                Ok(Err(e)) => {
                    tracing::error!(
                        agent = %self.agent_name,
                        "StreamJsonAdapter: error waiting for subprocess: {e}"
                    );
                }
                Err(_) => {
                    tracing::debug!(
                        agent = %self.agent_name,
                        "StreamJsonAdapter: subprocess did not exit within 3s, sending SIGKILL"
                    );
                    if let Err(e) = child.kill().await {
                        tracing::error!(
                            agent = %self.agent_name,
                            "StreamJsonAdapter: failed to SIGKILL: {e}"
                        );
                    }
                }
            }
        }

        self.child.take();
        self.health_status = AgentHealth::Unknown;

        tracing::debug!(agent = %self.agent_name, "StreamJsonAdapter: shutdown complete");
        Ok(())
    }

    // ─── health ─────────────────────────────────────────────────────────

    fn health(&self) -> AgentHealth {
        if let Some(ref child) = self.child {
            match child.id() {
                Some(_) => self.health_status.clone(),
                None => AgentHealth::Error("StreamJsonAdapter subprocess has exited".into()),
            }
        } else {
            self.health_status.clone()
        }
    }
}

// ─── Background reader task ──────────────────────────────────────────────

/// Persistent background task that reads Claude's stdout NDJSON.
///
/// Waits for a sender from each `prompt()` call, reads events until
/// a `result` event arrives, then drops the sender (ending the stream)
/// and waits for the next sender.
async fn reader_loop(
    stdout: BufReader<ChildStdout>,
    mut sender_rx: mpsc::Receiver<mpsc::Sender<SessionNotification>>,
    session_id: SessionId,
    cost: Arc<Mutex<f64>>,
    agent_name: String,
) {
    let mut lines = stdout.lines();
    let mut current_tx: Option<mpsc::Sender<SessionNotification>> = None;

    loop {
        // If no current sender, wait for one from prompt().
        if current_tx.is_none() {
            match sender_rx.recv().await {
                Some(tx) => current_tx = Some(tx),
                None => {
                    tracing::debug!(agent = %agent_name, "reader_loop: sender channel closed, exiting");
                    break;
                }
            }
        }

        // Read next line from stdout.
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => {
                tracing::debug!(agent = %agent_name, "reader_loop: stdout EOF");
                break;
            }
            Err(e) => {
                tracing::error!(agent = %agent_name, "reader_loop: stdout read error: {e}");
                break;
            }
        };

        // Parse event.
        let event = match parse_event(&line) {
            Ok(e) => e,
            Err(_) => {
                tracing::debug!(
                    agent = %agent_name,
                    line_len = line.len(),
                    "reader_loop: unparseable event, skipping"
                );
                continue;
            }
        };

        // Handle result event: extract cost, drop sender (ends stream).
        if let ClaudeEvent::Result(ref r) = event {
            if let Some(c) = r.total_cost_usd {
                if let Ok(mut total) = cost.lock() {
                    *total += c;
                }
            }
            tracing::debug!(
                agent = %agent_name,
                cost = ?r.total_cost_usd,
                "reader_loop: turn complete"
            );
            current_tx = None; // drop sender → stream ends
            continue;
        }

        // Map to notifications and send.
        if let Some(ref tx) = current_tx {
            for notif in map_to_notifications(&event, &session_id) {
                if tx.send(notif).await.is_err() {
                    // Receiver dropped — consumer cancelled.
                    tracing::debug!(agent = %agent_name, "reader_loop: receiver dropped");
                    current_tx = None;
                    break;
                }
            }
        }
    }
}
```

- [ ] **Step 2: Add module export to connection/mod.rs**

In `crates/spur-acp/src/connection/mod.rs`, add after the existing module declarations:

```rust
pub mod stream_json_adapter;
pub use stream_json_adapter::StreamJsonAdapter;
```

- [ ] **Step 3: Add StreamJsonAdapter to lib.rs re-exports**

In `crates/spur-acp/src/lib.rs`, update the connection re-export line:

```rust
pub use connection::{AgentConnection, CliWrapAdapter, NativeAcpConnection, StdioAdapter, StreamJsonAdapter};
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p spur-acp`
Expected: compiles. Dead code warnings for `StreamJsonAdapter` are acceptable (not yet used by orchestrator).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/connection/stream_json_adapter.rs crates/spur-acp/src/connection/mod.rs crates/spur-acp/src/lib.rs
git commit -m "feat(acp): add StreamJsonAdapter for Claude Code stream-json protocol"
```

---

### Task 4: Wire StreamJsonAdapter into the orchestrator

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:696-713`
- Modify: `crates/spur-core/src/orchestrator.rs:936-947`

- [ ] **Step 1: Add import**

At the top of `crates/spur-core/src/orchestrator.rs`, the existing import (line 13) reads:

```rust
use spur_acp::connection::{AgentConnection, CliWrapAdapter, NativeAcpConnection, StdioAdapter};
```

Change to:

```rust
use spur_acp::connection::{AgentConnection, CliWrapAdapter, NativeAcpConnection, StdioAdapter, StreamJsonAdapter};
```

- [ ] **Step 2: Add dispatch arm in build_connection (first occurrence ~line 696)**

In the `build_connection` method, add the `StreamJson` arm to the match. The existing match (lines 696-713) ends with `CliWrap`. Add after it:

```rust
            TransportKind::StreamJson => Box::new(StreamJsonAdapter::new(
                config.name.clone(),
                config.command.clone(),
                config.args.clone(),
            )),
```

- [ ] **Step 3: Add dispatch arm in the second build_connection occurrence (~line 936)**

There is a second match on `TransportKind` around line 936 (for worker agent connections). Add the same arm:

```rust
            TransportKind::StreamJson => Box::new(StreamJsonAdapter::new(
                config.name.clone(),
                config.command.clone(),
                config.args.clone(),
            )),
```

- [ ] **Step 4: Verify the full workspace compiles**

Run: `cargo check --workspace`
Expected: compiles clean with no errors or warnings about non-exhaustive match.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(core): wire StreamJsonAdapter into orchestrator dispatch"
```

---

### Task 5: Add Claude Code to default agent discovery

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:518-530` (the `init_agents` method)

- [ ] **Step 1: Find the default agents list**

In `init_agents()`, there's a list of default agents to scan (around line 520):

```rust
("kiro", "kiro-cli", vec!["acp"], TransportKind::Acp),
```

Add Claude Code with stream-json transport. Add this entry to the defaults list:

```rust
(
    "claude-code",
    "claude",
    vec![
        "-p".to_string(),
        "--input-format".to_string(),
        "stream-json".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--bare".to_string(),
        "--include-partial-messages".to_string(),
        "--permission-mode".to_string(),
        "acceptEdits".to_string(),
    ],
    TransportKind::StreamJson,
),
```

Note: Check the exact type of the args field in the defaults tuple. If it's `vec!["acp"]` (Vec<&str>), convert to match. If it's `Vec<String>`, use the `.to_string()` form above.

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p spur-core`
Expected: compiles clean.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(core): add Claude Code as default agent with stream-json transport"
```

---

### Task 6: Manual smoke test

**Files:** None (testing only)

- [ ] **Step 1: Build the full project**

Run: `cargo build --workspace`
Expected: builds clean.

- [ ] **Step 2: Verify Claude Code is detected**

Run: `cargo run -p spur-cli -- agents` (or equivalent command to list agents)
Expected: `claude-code` appears in the agent list with transport `stream-json`.

- [ ] **Step 3: Test basic interaction (if Claude Code is authenticated)**

Run: `cargo run -p spur-cli -- run "what is 2+2"`
Expected:
- TUI opens, Claude Code spawns as brain
- ReAct trace shows thinking + response
- Cost is displayed after completion
- No hang or crash

- [ ] **Step 4: Test multi-turn (if applicable)**

In the TUI session detail view, type a follow-up message. Verify:
- The message is sent (appears in trace as user message)
- Claude responds (new assistant message appears)
- The session continues without process restart

---

## Summary

| Task | What | Key Files | LOC |
|---|---|---|---|
| 1 | Add `StreamJson` to TransportKind | types.rs | ~1 |
| 2 | Claude event types + parser + mapper | protocol/claude_events.rs | ~120 |
| 3 | StreamJsonAdapter (AgentConnection impl) | connection/stream_json_adapter.rs | ~280 |
| 4 | Wire into orchestrator dispatch | orchestrator.rs | ~10 |
| 5 | Add Claude Code to default agents | orchestrator.rs | ~15 |
| 6 | Smoke test | — | 0 |
