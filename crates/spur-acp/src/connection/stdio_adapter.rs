//! `StdioAdapter` — lifts a persistent stdin/stdout subprocess into the `AgentConnection` trait.
//!
//! # Lifecycle mapping
//!
//! | `AgentConnection` method | Behaviour |
//! |---|---|
//! | `initialize()` | Spawn the subprocess with piped stdin/stdout and null stderr; return synthetic `InitializeResponse` with minimal capabilities |
//! | `new_session()` | Return synthetic `NewSessionResponse` with a UUID-based `SessionId`; the process IS the session |
//! | `prompt()` | Write delimited prompt to stdin, then spawn a tokio task that reads stdout line-by-line with a 2-second idle timeout — each line becomes an `AgentMessageChunk` `SessionNotification`; idle timeout triggers stream completion |
//! | `cancel()` | Send SIGTERM to the child process |
//! | `shutdown()` | Close stdin, wait up to 3 seconds, SIGKILL if needed |
//! | `health()` | Check whether the child process is still alive; return cached `AgentHealth` |

use std::path::PathBuf;
use std::pin::Pin;

use async_trait::async_trait;
use futures::stream::unfold;
use futures::Stream;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout};
use uuid::Uuid;

use agent_client_protocol::schema::{
    ContentBlock, ContentChunk, InitializeRequest, InitializeResponse, McpServer,
    NewSessionResponse, PromptRequest, ProtocolVersion, SessionId, SessionNotification,
    SessionUpdate, TextContent,
};

use crate::connection::AgentConnection;
use crate::types::AgentHealth;

#[cfg(any(test, feature = "test-support"))]
pub async fn spawn_stdio_for_test(
    command: &str,
    args: &[&str],
) -> std::io::Result<tokio::process::Child> {
    tokio::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}

/// Manages a persistent subprocess and translates raw stdin/stdout into ACP message types.
///
/// Unlike `CliWrapAdapter`, which spawns a fresh process for every `prompt()` call,
/// `StdioAdapter` keeps a single process alive for the lifetime of the connection.
/// Prompts are delimited with marker lines so the subprocess can distinguish
/// consecutive requests on the same I/O channel.
pub struct StdioAdapter {
    /// Human-readable agent name (for log messages).
    agent_name: String,
    /// Binary to invoke.
    command: String,
    /// Extra arguments passed to the binary on startup.
    extra_args: Vec<String>,
    /// Running child process (held so `cancel()` and `shutdown()` can send signals).
    child: Option<Child>,
    /// Buffered stdin writer — held open until `shutdown()`.
    stdin: Option<BufWriter<ChildStdin>>,
    /// Buffered stdout reader — taken into background tasks during `prompt()`.
    stdout_reader: Option<BufReader<ChildStdout>>,
    /// Session id assigned by the last `new_session()` call.
    session_id: Option<SessionId>,
    /// Cached health status.
    health_status: AgentHealth,
}

impl StdioAdapter {
    /// Create a new adapter.
    ///
    /// `command` is the binary name or path. `extra_args` are passed to the process
    /// at spawn time (before any prompt text).
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
            stdout_reader: None,
            session_id: None,
            health_status: AgentHealth::Unknown,
        }
    }

    /// Spawn the persistent child process with piped stdin/stdout and null stderr.
    fn spawn_process(&mut self) -> anyhow::Result<()> {
        tracing::debug!(
            agent = %self.agent_name,
            command = %self.command,
            "StdioAdapter: spawning persistent subprocess"
        );

        let mut cmd = tokio::process::Command::new(&self.command);
        cmd.args(&self.extra_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn().map_err(|e| {
            self.health_status = AgentHealth::Error(format!("Failed to spawn process: {e}"));
            anyhow::anyhow!(
                "StdioAdapter '{}': failed to spawn '{}': {e}",
                self.agent_name,
                self.command
            )
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            anyhow::anyhow!(
                "StdioAdapter '{}': failed to capture stdin",
                self.agent_name
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            anyhow::anyhow!(
                "StdioAdapter '{}': failed to capture stdout",
                self.agent_name
            )
        })?;

        self.stdin = Some(BufWriter::new(stdin));
        self.stdout_reader = Some(BufReader::new(stdout));
        self.child = Some(child);

        tracing::debug!(agent = %self.agent_name, "StdioAdapter: subprocess spawned successfully");
        Ok(())
    }
}

#[async_trait]
impl AgentConnection for StdioAdapter {
    // ─── initialize ─────────────────────────────────────────────────────

    async fn initialize(
        &mut self,
        _request: InitializeRequest,
    ) -> anyhow::Result<InitializeResponse> {
        self.spawn_process()?;

        // No ACP handshake — process started means we are ready.
        self.health_status = AgentHealth::Ready;
        tracing::debug!(
            agent = %self.agent_name,
            command = %self.command,
            "StdioAdapter initialized (no protocol handshake)"
        );

        // Return minimal capabilities: no MCP, no sessions.
        Ok(InitializeResponse::new(ProtocolVersion::LATEST))
    }

    // ─── new_session ─────────────────────────────────────────────────────

    async fn new_session(
        &mut self,
        _cwd: PathBuf,
        _mcp_servers: Vec<McpServer>,
    ) -> anyhow::Result<NewSessionResponse> {
        // StdioAdapter has no concept of sessions — the process IS the session.
        // Return a synthetic ID so callers have something to reference.
        let session_id = SessionId::new(Uuid::new_v4().to_string());
        self.session_id = Some(session_id.clone());

        tracing::debug!(
            agent = %self.agent_name,
            session = %session_id,
            "StdioAdapter: created synthetic session (process is the session)"
        );

        Ok(NewSessionResponse::new(session_id))
    }

    // ─── prompt ──────────────────────────────────────────────────────────

    async fn prompt(
        &mut self,
        request: PromptRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        // Concatenate all text blocks into a single prompt string.
        let prompt_text: String = request
            .prompt
            .iter()
            .filter_map(|block| {
                if let ContentBlock::Text(TextContent { text, .. }) = block {
                    Some(text.as_str())
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
            "StdioAdapter: writing delimited prompt to stdin"
        );

        // Write delimited prompt to stdin.
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            anyhow::anyhow!(
                "StdioAdapter '{}': stdin not available (not initialized?)",
                self.agent_name
            )
        })?;

        stdin.write_all(b"\n--- SPUR PROMPT ---\n").await?;
        stdin.write_all(prompt_text.as_bytes()).await?;
        stdin.write_all(b"\n--- END PROMPT ---\n").await?;
        stdin.flush().await?;

        // Take ownership of stdout reader so the background task can own it.
        let stdout = self.stdout_reader.take().ok_or_else(|| {
            anyhow::anyhow!(
                "StdioAdapter '{}': stdout reader not available",
                self.agent_name
            )
        })?;

        // Channel to bridge the background reader task into the returned stream.
        let (tx, rx) = tokio::sync::mpsc::channel::<SessionNotification>(64);
        let agent_name = self.agent_name.clone();

        // Background task: read stdout line-by-line with a 2-second idle timeout.
        // The idle timeout serves as the end-of-response heuristic for agents
        // that do not signal completion explicitly.
        tokio::spawn(async move {
            let mut stdout = stdout;
            let idle_timeout = std::time::Duration::from_secs(2);

            loop {
                let mut line = String::new();

                match tokio::time::timeout(idle_timeout, stdout.read_line(&mut line)).await {
                    // EOF — subprocess closed stdout.
                    Ok(Ok(0)) => {
                        tracing::debug!(
                            agent = %agent_name,
                            "StdioAdapter: subprocess stdout EOF, ending stream"
                        );
                        break;
                    }

                    // Got a line — emit as AgentMessageChunk.
                    Ok(Ok(_)) => {
                        // Trim the trailing newline but preserve all other whitespace.
                        let text = line.trim_end_matches('\n').to_string();
                        let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text)));
                        let notif = SessionNotification::new(
                            session_id.clone(),
                            SessionUpdate::AgentMessageChunk(chunk),
                        );
                        if tx.send(notif).await.is_err() {
                            // Receiver dropped — consumer is no longer interested.
                            break;
                        }
                    }

                    // I/O error.
                    Ok(Err(e)) => {
                        tracing::error!(
                            agent = %agent_name,
                            "StdioAdapter: error reading subprocess stdout: {e}"
                        );
                        break;
                    }

                    // Idle timeout — no output for 2 seconds; treat as end of response.
                    Err(_) => {
                        tracing::debug!(
                            agent = %agent_name,
                            "StdioAdapter: idle for 2 seconds, treating as response complete"
                        );
                        break;
                    }
                }
            }

            // Return the stdout reader so it could be put back if needed.
            // In this design we just drop it; the channel closing signals completion.
        });

        let stream = unfold(rx, |mut rx| async move {
            rx.recv().await.map(|notif| (notif, rx))
        });

        Ok(Box::pin(stream))
    }

    // ─── cancel ──────────────────────────────────────────────────────────

    async fn cancel(&mut self, session_id: &str) -> anyhow::Result<()> {
        if let Some(child) = self.child.as_ref() {
            if let Some(pid) = child.id() {
                tracing::debug!(
                    agent = %self.agent_name,
                    session = %session_id,
                    pid = pid,
                    "StdioAdapter: sending SIGTERM to subprocess"
                );
                // Use the `kill` command to send SIGTERM without requiring the libc crate.
                let _ = tokio::process::Command::new("kill")
                    .args(["-s", "TERM", &pid.to_string()])
                    .output()
                    .await;
            }
        }
        Ok(())
    }

    // ─── shutdown ────────────────────────────────────────────────────────

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        tracing::debug!(agent = %self.agent_name, "StdioAdapter: shutting down");

        // Drop stdin to signal EOF to the subprocess.
        self.stdin.take();

        if let Some(ref mut child) = self.child {
            // Wait up to 3 seconds for the process to exit gracefully.
            match tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await {
                Ok(Ok(status)) => {
                    tracing::debug!(
                        agent = %self.agent_name,
                        status = %status,
                        "StdioAdapter: subprocess exited cleanly"
                    );
                }
                Ok(Err(e)) => {
                    tracing::error!(
                        agent = %self.agent_name,
                        "StdioAdapter: error waiting for subprocess: {e}"
                    );
                }
                Err(_) => {
                    // Timeout — send SIGKILL.
                    tracing::debug!(
                        agent = %self.agent_name,
                        "StdioAdapter: subprocess did not exit within 3 seconds, sending SIGKILL"
                    );
                    if let Err(e) = child.kill().await {
                        tracing::error!(
                            agent = %self.agent_name,
                            "StdioAdapter: failed to SIGKILL subprocess: {e}"
                        );
                    }
                }
            }
        }

        self.child.take();
        self.stdout_reader.take();
        self.health_status = AgentHealth::Unknown;

        tracing::debug!(agent = %self.agent_name, "StdioAdapter: shutdown complete");
        Ok(())
    }

    // ─── health ──────────────────────────────────────────────────────────

    fn health(&self) -> AgentHealth {
        // If we have a child, check whether it's still running by inspecting its PID.
        // `child.id()` returns None once the process has been waited on / exited.
        if let Some(child) = self.child.as_ref() {
            match child.id() {
                Some(_) => self.health_status.clone(),
                None => AgentHealth::Error("StdioAdapter subprocess has exited".into()),
            }
        } else {
            self.health_status.clone()
        }
    }
}
