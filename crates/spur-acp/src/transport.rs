use crate::types::{
    AgentCapabilities, AgentHealth, AgentStatus, McpEndpoint, PromptBlock, SessionEvent, SessionId,
};
use async_trait::async_trait;
use futures::stream::unfold;
use futures::Stream;
use serde_json::{json, Value};
use std::pin::Pin;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout};

/// Core trait for communicating with an AI coding agent.
///
/// Implementations handle the specifics of different protocols:
/// - `AcpTransport`: Full ACP (JSON-RPC 2.0 over stdio)
/// - `StdioTransport`: Raw stdin/stdout (Phase 2)
/// - `CliWrapTransport`: One-shot CLI invocation per task
#[async_trait]
pub trait AgentTransport: Send + Sync {
    /// Initialize the agent process and negotiate capabilities.
    /// Optionally pass an MCP server endpoint for the agent to connect to.
    async fn initialize(
        &mut self,
        mcp_endpoint: Option<McpEndpoint>,
    ) -> anyhow::Result<AgentCapabilities>;

    /// Create a new conversation session.
    async fn create_session(&mut self) -> anyhow::Result<SessionId>;

    /// Send a prompt to an active session and receive a stream of events.
    async fn prompt(
        &mut self,
        session: SessionId,
        prompt: Vec<PromptBlock>,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionEvent> + Send>>>;

    /// Cancel an in-progress prompt.
    async fn cancel(&mut self, session: SessionId) -> anyhow::Result<()>;

    /// Gracefully shut down the agent process.
    async fn shutdown(&mut self) -> anyhow::Result<()>;

    /// Current health status of the agent.
    fn health(&self) -> AgentHealth;
}

// ─── AcpTransport ──────────────────────────────────────────────────────

/// Full ACP transport: JSON-RPC 2.0 over stdio with streaming, sessions, and MCP passthrough.
pub struct AcpTransport {
    agent_name: String,
    command: String,
    args: Vec<String>,
    child: Option<Child>,
    stdin: Option<BufWriter<ChildStdin>>,
    stdout: Option<BufReader<ChildStdout>>,
    next_id: u64,
    health_status: AgentHealth,
}

impl AcpTransport {
    pub fn new(agent_name: String, command: String, args: Vec<String>) -> Self {
        Self {
            agent_name,
            command,
            args,
            child: None,
            stdin: None,
            stdout: None,
            next_id: 1,
            health_status: AgentHealth::Unknown,
        }
    }

    /// Spawn the child process with stdin/stdout piped.
    async fn spawn_process(&mut self) -> anyhow::Result<()> {
        tracing::debug!(
            agent = %self.agent_name,
            command = %self.command,
            "Spawning agent process"
        );

        let mut child = tokio::process::Command::new(&self.command)
            .args(&self.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                self.health_status =
                    AgentHealth::Error(format!("Failed to spawn process: {e}"));
                anyhow::anyhow!("Failed to spawn agent '{}' ({}): {e}", self.agent_name, self.command)
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stdin for agent '{}'", self.agent_name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stdout for agent '{}'", self.agent_name))?;

        self.stdin = Some(BufWriter::new(stdin));
        self.stdout = Some(BufReader::new(stdout));
        self.child = Some(child);

        tracing::debug!(agent = %self.agent_name, "Agent process spawned");
        Ok(())
    }

    /// Send a JSON-RPC request and read the corresponding response.
    async fn send_request(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let request_str = serde_json::to_string(&request)?;
        tracing::debug!(
            agent = %self.agent_name,
            method = %method,
            id = id,
            "Sending JSON-RPC request"
        );

        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Agent '{}' stdin not available", self.agent_name))?;

        stdin.write_all(request_str.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;

        // Read lines from stdout until we get a response matching our request ID.
        // Notifications (lines without an "id") are skipped here.
        let stdout = self
            .stdout
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Agent '{}' stdout not available", self.agent_name))?;

        loop {
            let mut line = String::new();
            let bytes_read = stdout.read_line(&mut line).await?;
            if bytes_read == 0 {
                self.health_status =
                    AgentHealth::Error("Agent process closed stdout unexpectedly".into());
                return Err(anyhow::anyhow!(
                    "Agent '{}' closed stdout while waiting for response to '{method}'",
                    self.agent_name
                ));
            }

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let response: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!(
                        agent = %self.agent_name,
                        line = %line,
                        "Skipping non-JSON line from agent: {e}"
                    );
                    continue;
                }
            };

            // Skip notifications (no "id" field)
            if response.get("id").is_none() {
                tracing::debug!(
                    agent = %self.agent_name,
                    method = response.get("method").and_then(|m| m.as_str()).unwrap_or("unknown"),
                    "Received notification while waiting for response, skipping"
                );
                continue;
            }

            // Check if this response matches our request ID
            if response.get("id").and_then(|v| v.as_u64()) == Some(id) {
                if let Some(error) = response.get("error") {
                    let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
                    let message = error
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("Unknown error");
                    return Err(anyhow::anyhow!(
                        "JSON-RPC error from '{}' for '{method}': [{code}] {message}",
                        self.agent_name
                    ));
                }
                return Ok(response.get("result").cloned().unwrap_or(Value::Null));
            }

            // Response with a different ID — log and continue waiting
            tracing::debug!(
                agent = %self.agent_name,
                expected_id = id,
                got_id = ?response.get("id"),
                "Received response with unexpected ID, skipping"
            );
        }
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn send_notification(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });

        let notification_str = serde_json::to_string(&notification)?;
        tracing::debug!(
            agent = %self.agent_name,
            method = %method,
            "Sending JSON-RPC notification"
        );

        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Agent '{}' stdin not available", self.agent_name))?;

        stdin.write_all(notification_str.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;

        Ok(())
    }
}

#[async_trait]
impl AgentTransport for AcpTransport {
    async fn initialize(
        &mut self,
        mcp_endpoint: Option<McpEndpoint>,
    ) -> anyhow::Result<AgentCapabilities> {
        self.spawn_process().await?;

        let mut params = json!({
            "protocolVersion": "2024-11-05",
            "clientInfo": {
                "name": "spur",
                "version": env!("CARGO_PKG_VERSION"),
            },
        });

        if let Some(endpoint) = mcp_endpoint {
            params["mcpServers"] = json!([{
                "name": endpoint.server_name,
                "socketPath": endpoint.socket_path,
            }]);
        }

        let result = self.send_request("initialize", params).await?;

        let capabilities = AgentCapabilities {
            name: result
                .get("serverInfo")
                .and_then(|s| s.get("name"))
                .and_then(|n| n.as_str())
                .map(String::from),
            version: result
                .get("serverInfo")
                .and_then(|s| s.get("version"))
                .and_then(|v| v.as_str())
                .map(String::from),
            supports_mcp: result
                .get("capabilities")
                .and_then(|c| c.get("mcp"))
                .and_then(|m| m.as_bool())
                .unwrap_or(false),
            supports_sessions: result
                .get("capabilities")
                .and_then(|c| c.get("sessions"))
                .and_then(|s| s.as_bool())
                .unwrap_or(false),
            supports_streaming: result
                .get("capabilities")
                .and_then(|c| c.get("streaming"))
                .and_then(|s| s.as_bool())
                .unwrap_or(false),
            raw: result.clone(),
        };

        // Send the initialized notification as per JSON-RPC protocol
        self.send_notification("notifications/initialized", json!({}))
            .await?;

        self.health_status = AgentHealth::Ready;
        tracing::debug!(
            agent = %self.agent_name,
            name = ?capabilities.name,
            version = ?capabilities.version,
            "Agent initialized"
        );

        Ok(capabilities)
    }

    async fn create_session(&mut self) -> anyhow::Result<SessionId> {
        let result = self.send_request("session/create", json!({})).await?;

        let session_id = result
            .get("sessionId")
            .and_then(|s| s.as_str())
            .map(|s| SessionId(s.to_string()))
            .unwrap_or_else(SessionId::new);

        tracing::debug!(
            agent = %self.agent_name,
            session = %session_id,
            "Session created"
        );

        Ok(session_id)
    }

    async fn prompt(
        &mut self,
        session: SessionId,
        prompt: Vec<PromptBlock>,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionEvent> + Send>>> {
        let id = self.next_id;
        self.next_id += 1;

        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": {
                "sessionId": session.0,
                "prompt": prompt,
            },
        });

        let request_str = serde_json::to_string(&request)?;
        tracing::debug!(
            agent = %self.agent_name,
            session = %session,
            id = id,
            "Sending prompt request"
        );

        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Agent '{}' stdin not available", self.agent_name))?;

        stdin.write_all(request_str.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;

        // Take stdout from self so we can move it into the spawned task.
        let stdout = self
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Agent '{}' stdout not available", self.agent_name))?;

        let (tx, rx) = tokio::sync::mpsc::channel::<SessionEvent>(64);
        let agent_name = self.agent_name.clone();
        let session_clone = session.clone();

        tokio::spawn(async move {
            let mut stdout = stdout;
            loop {
                let mut line = String::new();
                match stdout.read_line(&mut line).await {
                    Ok(0) => {
                        // EOF — process closed stdout
                        let _ = tx
                            .send(SessionEvent::Complete {
                                session_id: session_clone.clone(),
                            })
                            .await;
                        break;
                    }
                    Ok(_) => {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }

                        let msg: Value = match serde_json::from_str(line) {
                            Ok(v) => v,
                            Err(e) => {
                                tracing::debug!(
                                    agent = %agent_name,
                                    line = %line,
                                    "Skipping non-JSON line from agent: {e}"
                                );
                                continue;
                            }
                        };

                        // If this is the final response (has "id" matching ours), emit Complete
                        if msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
                            let _ = tx
                                .send(SessionEvent::Complete {
                                    session_id: session_clone.clone(),
                                })
                                .await;
                            break;
                        }

                        // Parse notifications (session/update events)
                        if let Some(event) = parse_session_event(&msg) {
                            if tx.send(event).await.is_err() {
                                // Receiver dropped
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            agent = %agent_name,
                            "Error reading from agent stdout: {e}"
                        );
                        let _ = tx
                            .send(SessionEvent::Error {
                                code: -1,
                                message: format!("IO error: {e}"),
                            })
                            .await;
                        break;
                    }
                }
            }
        });

        let stream = unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        });
        Ok(Box::pin(stream))
    }

    async fn cancel(&mut self, session: SessionId) -> anyhow::Result<()> {
        // During streaming, stdout is owned by the reader task, so we cannot use
        // send_request (which reads the response). Instead, send the cancel as a
        // notification — the streaming task will see the completion or error.
        if self.stdout.is_none() {
            self.send_notification(
                "session/cancel",
                json!({ "sessionId": session.0 }),
            )
            .await?;
        } else {
            self.send_request("session/cancel", json!({ "sessionId": session.0 }))
                .await?;
        }
        tracing::debug!(
            agent = %self.agent_name,
            session = %session,
            "Session cancelled"
        );
        Ok(())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        tracing::debug!(agent = %self.agent_name, "Shutting down agent");

        // Send shutdown. If stdout is taken (streaming in progress), send as
        // notification since we cannot read a response. Either way, tolerate errors
        // because the process may already be exiting.
        if self.stdout.is_some() {
            let shutdown_result = self.send_request("shutdown", json!({})).await;
            if let Err(e) = &shutdown_result {
                tracing::debug!(
                    agent = %self.agent_name,
                    "Shutdown request failed (may be expected): {e}"
                );
            }
        } else {
            let shutdown_result = self
                .send_notification("shutdown", json!({}))
                .await;
            if let Err(e) = &shutdown_result {
                tracing::debug!(
                    agent = %self.agent_name,
                    "Shutdown notification failed (may be expected): {e}"
                );
            }
        }

        // Drop stdin to signal EOF to the child
        self.stdin.take();

        if let Some(ref mut child) = self.child {
            // Wait up to 5 seconds for the process to exit
            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                child.wait(),
            )
            .await
            {
                Ok(Ok(status)) => {
                    tracing::debug!(
                        agent = %self.agent_name,
                        status = %status,
                        "Agent process exited"
                    );
                }
                Ok(Err(e)) => {
                    tracing::error!(
                        agent = %self.agent_name,
                        "Error waiting for agent process: {e}"
                    );
                }
                Err(_) => {
                    // Timeout — send SIGTERM (kill on all platforms via tokio)
                    tracing::debug!(
                        agent = %self.agent_name,
                        "Agent did not exit within 5 seconds, sending kill signal"
                    );
                    if let Err(e) = child.kill().await {
                        tracing::error!(
                            agent = %self.agent_name,
                            "Failed to kill agent process: {e}"
                        );
                    }
                }
            }
        }

        self.child.take();
        self.stdout.take();
        self.health_status = AgentHealth::Unknown;

        tracing::debug!(agent = %self.agent_name, "Agent shutdown complete");
        Ok(())
    }

    fn health(&self) -> AgentHealth {
        // Check if the child process is still alive
        if let Some(ref child) = self.child {
            match child.id() {
                Some(_) => {
                    // Process has a PID, use our stored status
                    self.health_status.clone()
                }
                None => {
                    // Process has exited
                    AgentHealth::Error("Agent process has exited".into())
                }
            }
        } else {
            self.health_status.clone()
        }
    }
}

/// Parse a JSON-RPC notification into a SessionEvent.
fn parse_session_event(msg: &Value) -> Option<SessionEvent> {
    let method = msg.get("method")?.as_str()?;
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "session/update" => {
            let event_type = params.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match event_type {
                "text" | "textDelta" => {
                    let text = params
                        .get("text")
                        .or_else(|| params.get("delta"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(SessionEvent::TextDelta(text))
                }
                "toolCallStart" | "tool_call_start" => {
                    let id = params
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = params
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let input = params.get("input").cloned().unwrap_or(Value::Null);
                    Some(SessionEvent::ToolCallStart { id, name, input })
                }
                "toolCallResult" | "tool_call_result" => {
                    let id = params
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let output = params.get("output").cloned().unwrap_or(Value::Null);
                    Some(SessionEvent::ToolCallResult { id, output })
                }
                "status" => {
                    let status_str = params
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("idle");
                    let status = match status_str {
                        "thinking" => AgentStatus::Thinking,
                        "working" => AgentStatus::Working,
                        "done" => AgentStatus::Done,
                        "error" => AgentStatus::Error,
                        _ => AgentStatus::Idle,
                    };
                    Some(SessionEvent::StatusUpdate(status))
                }
                "rateLimitHit" | "rate_limit" => {
                    let retry_after = params
                        .get("retryAfter")
                        .or_else(|| params.get("retry_after"))
                        .and_then(|v| v.as_u64())
                        .map(std::time::Duration::from_secs);
                    Some(SessionEvent::RateLimitHit { retry_after })
                }
                "error" => {
                    let code = params
                        .get("code")
                        .and_then(|c| c.as_i64())
                        .unwrap_or(-1) as i32;
                    let message = params
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("Unknown error")
                        .to_string();
                    Some(SessionEvent::Error { code, message })
                }
                _ => {
                    tracing::debug!(event_type = %event_type, "Unknown session/update event type");
                    None
                }
            }
        }
        _ => {
            tracing::debug!(method = %method, "Unhandled notification method");
            None
        }
    }
}

// ─── StdioTransport ────────────────────────────────────────────────────

/// Raw stdin/stdout transport for agents that support persistent interactive
/// conversation but do NOT speak the ACP JSON-RPC protocol.
///
/// Prompts are delimited with marker lines; responses are collected line-by-line
/// with a 2-second idle timeout used as the end-of-response heuristic.
pub struct StdioTransport {
    agent_name: String,
    command: String,
    args: Vec<String>,
    child: Option<Child>,
    stdin: Option<BufWriter<ChildStdin>>,
    stdout_reader: Option<BufReader<ChildStdout>>,
    health_status: AgentHealth,
}

impl StdioTransport {
    pub fn new(agent_name: String, command: String, args: Vec<String>) -> Self {
        Self {
            agent_name,
            command,
            args,
            child: None,
            stdin: None,
            stdout_reader: None,
            health_status: AgentHealth::Unknown,
        }
    }

    /// Spawn the child process with stdin/stdout piped.
    async fn spawn_process(&mut self) -> anyhow::Result<()> {
        tracing::debug!(
            agent = %self.agent_name,
            command = %self.command,
            "Spawning stdio agent process"
        );

        let mut child = tokio::process::Command::new(&self.command)
            .args(&self.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                self.health_status =
                    AgentHealth::Error(format!("Failed to spawn process: {e}"));
                anyhow::anyhow!(
                    "Failed to spawn stdio agent '{}' ({}): {e}",
                    self.agent_name,
                    self.command
                )
            })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            anyhow::anyhow!(
                "Failed to capture stdin for stdio agent '{}'",
                self.agent_name
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            anyhow::anyhow!(
                "Failed to capture stdout for stdio agent '{}'",
                self.agent_name
            )
        })?;

        self.stdin = Some(BufWriter::new(stdin));
        self.stdout_reader = Some(BufReader::new(stdout));
        self.child = Some(child);

        tracing::debug!(agent = %self.agent_name, "Stdio agent process spawned");
        Ok(())
    }
}

#[async_trait]
impl AgentTransport for StdioTransport {
    async fn initialize(
        &mut self,
        _mcp_endpoint: Option<McpEndpoint>,
    ) -> anyhow::Result<AgentCapabilities> {
        // MCP endpoint is intentionally ignored — StdioTransport does not support MCP.
        if _mcp_endpoint.is_some() {
            tracing::debug!(
                agent = %self.agent_name,
                "MCP endpoint provided but StdioTransport does not support MCP passthrough; ignoring"
            );
        }

        self.spawn_process().await?;

        // No JSON-RPC handshake — the process started successfully, so we are ready.
        self.health_status = AgentHealth::Ready;
        tracing::debug!(
            agent = %self.agent_name,
            "Stdio agent initialized (no protocol handshake)"
        );

        Ok(AgentCapabilities {
            name: Some(self.agent_name.clone()),
            version: None,
            supports_mcp: false,
            supports_sessions: false,
            supports_streaming: true,
            raw: Value::Null,
        })
    }

    async fn create_session(&mut self) -> anyhow::Result<SessionId> {
        // StdioTransport has no concept of sessions — the process IS the session.
        // Return a synthetic ID so the caller has something to reference.
        Ok(SessionId::new())
    }

    async fn prompt(
        &mut self,
        session: SessionId,
        prompt: Vec<PromptBlock>,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionEvent> + Send>>> {
        // Concatenate all text blocks into a single prompt string.
        let prompt_text: String = prompt
            .iter()
            .filter_map(|block| match block {
                PromptBlock::Text { text } => Some(text.as_str()),
            })
            .collect::<Vec<_>>()
            .join("\n");

        tracing::debug!(
            agent = %self.agent_name,
            session = %session,
            prompt_len = prompt_text.len(),
            "Sending prompt to stdio agent"
        );

        // Write delimited prompt to stdin.
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            anyhow::anyhow!("Stdio agent '{}' stdin not available", self.agent_name)
        })?;

        stdin.write_all(b"\n--- SPUR PROMPT ---\n").await?;
        stdin.write_all(prompt_text.as_bytes()).await?;
        stdin.write_all(b"\n--- END PROMPT ---\n").await?;
        stdin.flush().await?;

        // Take ownership of stdout reader so we can move it into the spawned task.
        let stdout = self.stdout_reader.take().ok_or_else(|| {
            anyhow::anyhow!(
                "Stdio agent '{}' stdout not available",
                self.agent_name
            )
        })?;

        let (tx, rx) = tokio::sync::mpsc::channel::<SessionEvent>(64);
        let agent_name = self.agent_name.clone();
        let session_clone = session.clone();

        tokio::spawn(async move {
            let mut stdout = stdout;
            let idle_timeout = std::time::Duration::from_secs(2);

            loop {
                let mut line = String::new();
                match tokio::time::timeout(idle_timeout, stdout.read_line(&mut line)).await {
                    Ok(Ok(0)) => {
                        // EOF — process closed stdout.
                        let _ = tx
                            .send(SessionEvent::Complete {
                                session_id: session_clone,
                            })
                            .await;
                        break;
                    }
                    Ok(Ok(_)) => {
                        // Got a line — emit as TextDelta (preserving the content, trimming
                        // the trailing newline only).
                        let text = line.trim_end_matches('\n').to_string();
                        if tx.send(SessionEvent::TextDelta(text)).await.is_err() {
                            // Receiver dropped.
                            break;
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::error!(
                            agent = %agent_name,
                            "Error reading from stdio agent stdout: {e}"
                        );
                        let _ = tx
                            .send(SessionEvent::Error {
                                code: -1,
                                message: format!("IO error: {e}"),
                            })
                            .await;
                        break;
                    }
                    Err(_) => {
                        // Idle timeout — no output for 2 seconds, treat as end of response.
                        tracing::debug!(
                            agent = %agent_name,
                            "Stdio agent idle for 2 seconds, treating as response complete"
                        );
                        let _ = tx
                            .send(SessionEvent::Complete {
                                session_id: session_clone,
                            })
                            .await;
                        break;
                    }
                }
            }
        });

        let stream = unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        });
        Ok(Box::pin(stream))
    }

    async fn cancel(&mut self, session: SessionId) -> anyhow::Result<()> {
        // Send SIGTERM to the child process.
        if let Some(ref child) = self.child {
            if let Some(pid) = child.id() {
                tracing::debug!(
                    agent = %self.agent_name,
                    session = %session,
                    pid = pid,
                    "Sending SIGTERM to stdio agent"
                );
                // Use the kill command to send SIGTERM without requiring the libc crate.
                let _ = tokio::process::Command::new("kill")
                    .args(["-s", "TERM", &pid.to_string()])
                    .output()
                    .await;
            }
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        tracing::debug!(agent = %self.agent_name, "Shutting down stdio agent");

        // Close stdin to signal EOF to the child process.
        self.stdin.take();

        if let Some(ref mut child) = self.child {
            // Wait up to 3 seconds for the process to exit gracefully.
            match tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await {
                Ok(Ok(status)) => {
                    tracing::debug!(
                        agent = %self.agent_name,
                        status = %status,
                        "Stdio agent process exited"
                    );
                }
                Ok(Err(e)) => {
                    tracing::error!(
                        agent = %self.agent_name,
                        "Error waiting for stdio agent process: {e}"
                    );
                }
                Err(_) => {
                    // Timeout — send SIGKILL.
                    tracing::debug!(
                        agent = %self.agent_name,
                        "Stdio agent did not exit within 3 seconds, sending SIGKILL"
                    );
                    if let Err(e) = child.kill().await {
                        tracing::error!(
                            agent = %self.agent_name,
                            "Failed to kill stdio agent process: {e}"
                        );
                    }
                }
            }
        }

        self.child.take();
        self.stdout_reader.take();
        self.health_status = AgentHealth::Unknown;

        tracing::debug!(agent = %self.agent_name, "Stdio agent shutdown complete");
        Ok(())
    }

    fn health(&self) -> AgentHealth {
        if let Some(ref child) = self.child {
            match child.id() {
                Some(_) => self.health_status.clone(),
                None => AgentHealth::Error("Stdio agent process has exited".into()),
            }
        } else {
            self.health_status.clone()
        }
    }
}

// ─── CliWrapTransport ──────────────────────────────────────────────────

/// Fallback transport: invokes agent CLI as a one-shot subprocess per task.
/// No sessions, no streaming — just command in, output out.
pub struct CliWrapTransport {
    agent_name: String,
    command: String,
    args: Vec<String>,
    child: Option<Child>,
    health_status: AgentHealth,
}

impl CliWrapTransport {
    pub fn new(agent_name: String, command: String, args: Vec<String>) -> Self {
        Self {
            agent_name,
            command,
            args,
            child: None,
            health_status: AgentHealth::Unknown,
        }
    }
}

#[async_trait]
impl AgentTransport for CliWrapTransport {
    async fn initialize(
        &mut self,
        _mcp_endpoint: Option<McpEndpoint>,
    ) -> anyhow::Result<AgentCapabilities> {
        // Check that the command binary exists on PATH
        let which_result = tokio::process::Command::new("which")
            .arg(&self.command)
            .output()
            .await?;

        if !which_result.status.success() {
            self.health_status =
                AgentHealth::Error(format!("Command '{}' not found on PATH", self.command));
            return Err(anyhow::anyhow!(
                "Agent '{}': command '{}' not found on PATH",
                self.agent_name,
                self.command
            ));
        }

        self.health_status = AgentHealth::Ready;
        tracing::debug!(
            agent = %self.agent_name,
            command = %self.command,
            "CliWrap agent initialized (command found on PATH)"
        );

        Ok(AgentCapabilities {
            name: Some(self.agent_name.clone()),
            version: None,
            supports_mcp: false,
            supports_sessions: false,
            supports_streaming: false,
            raw: Value::Null,
        })
    }

    async fn create_session(&mut self) -> anyhow::Result<SessionId> {
        // CliWrap doesn't support persistent sessions — return a synthetic ID
        Ok(SessionId::new())
    }

    async fn prompt(
        &mut self,
        session: SessionId,
        prompt: Vec<PromptBlock>,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionEvent> + Send>>> {
        // Concatenate all text blocks into a single prompt string
        let prompt_text: String = prompt
            .iter()
            .filter_map(|block| match block {
                PromptBlock::Text { text } => Some(text.as_str()),
            })
            .collect::<Vec<_>>()
            .join("\n");

        tracing::debug!(
            agent = %self.agent_name,
            session = %session,
            prompt_len = prompt_text.len(),
            "Spawning one-shot CLI process"
        );

        let mut child = tokio::process::Command::new(&self.command)
            .args(&self.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                self.health_status =
                    AgentHealth::Error(format!("Failed to spawn process: {e}"));
                anyhow::anyhow!(
                    "Failed to spawn CLI agent '{}' ({}): {e}",
                    self.agent_name,
                    self.command
                )
            })?;

        // Write prompt to stdin and close it
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt_text.as_bytes()).await?;
            stdin.shutdown().await?;
            // stdin is dropped here, closing the pipe
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stdout for CLI agent '{}'", self.agent_name))?;

        let (tx, rx) = tokio::sync::mpsc::channel::<SessionEvent>(64);
        let agent_name = self.agent_name.clone();
        let session_clone = session.clone();

        // Store the child so it can be cancelled
        self.child = Some(child);

        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if tx.send(SessionEvent::TextDelta(line)).await.is_err() {
                            // Receiver dropped
                            break;
                        }
                    }
                    Ok(None) => {
                        // EOF — process finished
                        let _ = tx
                            .send(SessionEvent::Complete {
                                session_id: session_clone,
                            })
                            .await;
                        break;
                    }
                    Err(e) => {
                        tracing::error!(
                            agent = %agent_name,
                            "Error reading from CLI agent stdout: {e}"
                        );
                        let _ = tx
                            .send(SessionEvent::Error {
                                code: -1,
                                message: format!("IO error: {e}"),
                            })
                            .await;
                        break;
                    }
                }
            }
        });

        let stream = unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        });
        Ok(Box::pin(stream))
    }

    async fn cancel(&mut self, session: SessionId) -> anyhow::Result<()> {
        if let Some(ref mut child) = self.child {
            tracing::debug!(
                agent = %self.agent_name,
                session = %session,
                "Killing CLI subprocess"
            );
            child.kill().await.map_err(|e| {
                anyhow::anyhow!(
                    "Failed to kill CLI agent '{}' subprocess: {e}",
                    self.agent_name
                )
            })?;
            self.child.take();
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        // CliWrap processes are one-shot, nothing to shut down.
        // Kill any lingering child just in case.
        if let Some(ref mut child) = self.child {
            let _ = child.kill().await;
            self.child.take();
        }
        Ok(())
    }

    fn health(&self) -> AgentHealth {
        self.health_status.clone()
    }
}
