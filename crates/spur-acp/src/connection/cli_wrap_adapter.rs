//! `CliWrapAdapter` — lifts a one-shot CLI tool into the `AgentConnection` trait.
//!
//! # Lifecycle mapping
//!
//! | `AgentConnection` method | Behaviour |
//! |---|---|
//! | `initialize()` | Run `which <command>`; return synthetic `InitializeResponse` with minimal capabilities |
//! | `new_session()` | Store `cwd`; return synthetic `NewSessionResponse` with a fresh UUID-based `SessionId` |
//! | `prompt()` | Extract text from content blocks, spawn the command with cwd set, stream stdout line-by-line as `SessionNotification` items containing `SessionUpdate::AgentMessageChunk` |
//! | `cancel()` | Kill the child process if one is running |
//! | `shutdown()` | No-op (kill any lingering child for safety) |
//! | `health()` | Return stored `AgentHealth` |

use async_trait::async_trait;
use futures::stream::unfold;
use futures::Stream;
use std::path::PathBuf;
use std::pin::Pin;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use uuid::Uuid;

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, InitializeRequest, InitializeResponse, McpServer,
    NewSessionResponse, PromptRequest, SessionId, SessionNotification, SessionUpdate, TextContent,
};
use agent_client_protocol::schema::ProtocolVersion;

use crate::connection::AgentConnection;
use crate::types::AgentHealth;

#[cfg(any(test, feature = "test-support"))]
pub async fn spawn_cli_wrap_for_test(
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

/// Wraps a one-shot CLI tool as an `AgentConnection`.
///
/// On every `prompt()` call the adapter spawns a fresh subprocess, writes
/// the concatenated text prompt to stdin, captures stdout line-by-line, and
/// streams each line as an `AgentMessageChunk` `SessionNotification`.
pub struct CliWrapAdapter {
    /// Human-readable agent name (for log messages).
    agent_name: String,
    /// Binary to invoke (resolved via `which` on `initialize`).
    command: String,
    /// Extra arguments prepended to every invocation.
    extra_args: Vec<String>,
    /// Working directory stored from the last `new_session` call.
    cwd: Option<PathBuf>,
    /// Session id assigned by the last `new_session` call.
    session_id: Option<SessionId>,
    /// Running child process (held so `cancel()` can kill it).
    child: Option<Child>,
    /// Cached health status.
    health_status: AgentHealth,
}

impl CliWrapAdapter {
    /// Create a new adapter.
    ///
    /// `command` is the binary name or path. `extra_args` are prepended to
    /// every subprocess invocation (before the prompt text).
    pub fn new(
        agent_name: impl Into<String>,
        command: impl Into<String>,
        extra_args: Vec<String>,
    ) -> Self {
        Self {
            agent_name: agent_name.into(),
            command: command.into(),
            extra_args,
            cwd: None,
            session_id: None,
            child: None,
            health_status: AgentHealth::Unknown,
        }
    }
}

#[async_trait]
impl AgentConnection for CliWrapAdapter {
    // ─── initialize ────────────────────────────────────────────────────

    async fn initialize(
        &mut self,
        _request: InitializeRequest,
    ) -> anyhow::Result<InitializeResponse> {
        // Verify the binary exists on PATH.
        let which_result = tokio::process::Command::new("which")
            .arg(&self.command)
            .output()
            .await?;

        if !which_result.status.success() {
            self.health_status =
                AgentHealth::Error(format!("Command '{}' not found on PATH", self.command));
            return Err(anyhow::anyhow!(
                "CliWrapAdapter '{}': command '{}' not found on PATH",
                self.agent_name,
                self.command
            ));
        }

        self.health_status = AgentHealth::Ready;
        tracing::debug!(
            agent = %self.agent_name,
            command = %self.command,
            "CliWrapAdapter initialized (command found on PATH)"
        );

        // Return minimal capabilities: no MCP, no sessions.
        Ok(InitializeResponse::new(ProtocolVersion::LATEST))
    }

    // ─── new_session ────────────────────────────────────────────────────

    async fn new_session(
        &mut self,
        cwd: PathBuf,
        _mcp_servers: Vec<McpServer>,
    ) -> anyhow::Result<NewSessionResponse> {
        let session_id = SessionId::new(Uuid::new_v4().to_string());
        self.cwd = Some(cwd);
        self.session_id = Some(session_id.clone());

        tracing::debug!(
            agent = %self.agent_name,
            session = %session_id,
            "CliWrapAdapter: created synthetic session"
        );

        Ok(NewSessionResponse::new(session_id))
    }

    // ─── prompt ────────────────────────────────────────────────────────

    async fn prompt(
        &mut self,
        request: PromptRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        // Extract text from all Text content blocks.
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
        let cwd = self.cwd.clone().unwrap_or_else(|| PathBuf::from("."));

        tracing::debug!(
            agent = %self.agent_name,
            session = %session_id,
            prompt_len = prompt_text.len(),
            cwd = %cwd.display(),
            "CliWrapAdapter: spawning one-shot CLI process"
        );

        // Build arg list: extra_args + prompt text as trailing args.
        let mut cmd_args = self.extra_args.clone();
        if !prompt_text.is_empty() {
            cmd_args.push(prompt_text);
        }

        let mut cmd = tokio::process::Command::new(&self.command);
        cmd.args(&cmd_args)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn().map_err(|e| {
            self.health_status = AgentHealth::Error(format!("Failed to spawn process: {e}"));
            anyhow::anyhow!(
                "CliWrapAdapter '{}': failed to spawn '{}': {e}",
                self.agent_name,
                self.command
            )
        })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            anyhow::anyhow!(
                "CliWrapAdapter '{}': failed to capture stdout",
                self.agent_name
            )
        })?;

        // Channel to bridge the background reader task into the stream.
        let (tx, rx) = tokio::sync::mpsc::channel::<SessionNotification>(64);

        let agent_name = self.agent_name.clone();

        // Store the child so `cancel()` can kill it.
        self.child = Some(child);

        // Background task: read stdout and emit SessionNotifications.
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(line)));
                        let notif = SessionNotification::new(
                            session_id.clone(),
                            SessionUpdate::AgentMessageChunk(chunk),
                        );
                        if tx.send(notif).await.is_err() {
                            // Receiver dropped — consumer is no longer interested.
                            break;
                        }
                    }
                    Ok(None) => {
                        // EOF — subprocess finished normally; stream ends naturally.
                        tracing::debug!(
                            agent = %agent_name,
                            "CliWrapAdapter: subprocess stdout EOF"
                        );
                        break;
                    }
                    Err(e) => {
                        tracing::error!(
                            agent = %agent_name,
                            "CliWrapAdapter: error reading subprocess stdout: {e}"
                        );
                        break;
                    }
                }
            }
        });

        let stream = unfold(rx, |mut rx| async move {
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
                "CliWrapAdapter: killing subprocess on cancel"
            );
            child.kill().await.map_err(|e| {
                anyhow::anyhow!(
                    "CliWrapAdapter '{}': failed to kill subprocess: {e}",
                    self.agent_name
                )
            })?;
        }
        self.child = None;
        Ok(())
    }

    // ─── shutdown ───────────────────────────────────────────────────────

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        // CliWrap processes are one-shot; kill any lingering child just in case.
        if let Some(ref mut child) = self.child {
            let _ = child.kill().await;
        }
        self.child = None;
        Ok(())
    }

    // ─── health ─────────────────────────────────────────────────────────

    fn health(&self) -> AgentHealth {
        self.health_status.clone()
    }
}
